// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 bind for `ReplyFidelity` (`aterm_models_effects::reply_fidelity_model`):
//! once a forwarded verb has reached the host, no later failure may be
//! reported to the client as `ERR forward`, and a failure before that must be.
//!
//! Drives the shipping `connect_hop` and `deliver_then_relay` (all of
//! `relay_hop` but its relay stage) against a real host socket, and projects:
//!
//! * `delivered` — the host actually read the forwarded line (observed on the
//!   host's side; the line after the connect probe);
//! * `reported_err` — either returned `Err`, which is exactly when
//!   `try_proxy_forward` leaves the client an error: `ERR no such session`
//!   from the local dispatch for [`ForwardError::NotAHop`], `ERR forward` for
//!   anything else.
//!
//! Five runs: the real relay (delivered, no error), a relay stage that fails
//! after delivery (injected at the seam; a worker thread the system refuses to
//! start cannot be staged in a shared test process), a dial with no listener, a
//! host that never accepts, and a host with no token beside its socket (the
//! last three reported, never delivered).
//!
//! Negative control: from the real delivered state, reporting the relay
//! failure is rejected by the committed model and admitted by `Buggy = 1`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc;

use aterm_models_effects::reply_fidelity_model;
use aterm_spec::{interp, verify};

use super::*;
use crate::test_socket::{hang_up, join_after_hang_up};

type State = BTreeMap<&'static str, i64>;

/// The connect-probe bound for runs whose host answers at once, and how long a run
/// waits for the host's read: hang detectors for what must happen, never latency
/// budgets, so a loaded machine cannot turn a delivered verb into `delivered = 0`.
/// The unanswered-probe run passes its own short bound.
const PATIENCE: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;

fn state(delivered: bool, reported_err: bool) -> State {
    [
        ("delivered", i64::from(delivered)),
        ("reported_err", i64::from(reported_err)),
    ]
    .into_iter()
    .collect()
}

fn conforms(prev: &State, next: &State, action: &str) -> (bool, String) {
    verify::validate_transition_tiered(
        &reply_fidelity_model(),
        &[],
        prev,
        next,
        Some(action),
        "proxy reply-fidelity conformance",
    )
}

/// A throwaway host listener with its token beside the socket: it answers the
/// connect probe, reports the next request line it reads (the forwarded line),
/// then answers and hangs up.
struct Host {
    sock: String,
    read: mpsc::Receiver<String>,
    thread: std::thread::JoinHandle<()>,
    _dir: aterm_tempfile::TempDir,
}

fn host() -> Host {
    let dir = aterm_tempfile::TempDir::new_in("/tmp").unwrap();
    let path = dir.path().join("host.sock");
    std::fs::write(dir.path().join("host.sock.token"), "abcd\n").expect("the host's token");
    let listener = aterm_uds::CtlListener::bind(&path).expect("bind host");
    let (tx, read) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().expect("accept");
        let mut reader = BufReader::new(conn.try_clone().unwrap());
        let mut probe = String::new();
        let _ = reader.read_line(&mut probe);
        assert_eq!(probe, PROBE, "the forward proves the host serves first");
        let _ = conn.write_all(b"OK version=host\n");
        let mut line = String::new();
        let _ = reader.read_line(&mut line);
        let _ = tx.send(line);
        let _ = conn.write_all(b"OK\n");
        let _ = conn.shutdown(std::net::Shutdown::Both);
    });
    Host {
        sock: path.to_string_lossy().into_owned(),
        read,
        thread,
        _dir: dir,
    }
}

/// What the forward sends once the host serves: the caller's line as it came.
const LINE: &str = "@s-1 screen";
/// What the host reads first: the handshake with the connect probe, presenting
/// the token beside its socket.
const PROBE: &str = "TOKEN abcd version\n";

/// Whether the host read exactly the forwarded line. Bounded by [`PATIENCE`],
/// so an undelivered line projects as `false` rather than hanging; any line
/// read (EOF included) is reported at once.
fn host_read(h: &Host) -> bool {
    h.read
        .recv_timeout(PATIENCE)
        .is_ok_and(|line| line.strip_suffix('\n') == Some(LINE))
}

/// The forward to `sock` up to its relay stage: `connect_hop`, then
/// `deliver_then_relay` with `relay`. Whether it reported an error, and the
/// connect's own when it was that.
fn forward(
    sock: &str,
    client: &CtlStream,
    accept_within: std::time::Duration,
    relay: impl FnOnce(&CtlStream, &CtlStream, ForwardHost) -> std::io::Result<()>,
) -> (bool, Option<ForwardError>) {
    match connect_hop(sock) {
        Ok(hop) => (
            deliver_then_relay(hop, LINE, client, &[], accept_within, relay).is_err(),
            None,
        ),
        Err(error) => (true, Some(error)),
    }
}

#[test]
fn real_forward_outcomes_conform_to_reply_fidelity_model() {
    let init = reply_fidelity_model().init_state();

    // 1. The real relay: delivered, and nothing reported.
    let h = host();
    let (client_app, client_relay) = CtlStream::pair().expect("pair");
    let sock = h.sock.clone();
    let forward_thread = std::thread::spawn(move || match connect_hop(&sock) {
        Ok(hop) => relay_hop(hop, LINE, &client_relay, &[]).is_err(),
        Err(_) => true,
    });
    // The answer must cross: the read is a hang detector, not a latency budget.
    client_app
        .set_read_timeout(Some(PATIENCE))
        .expect("bound the reply read");
    let mut reply = String::new();
    BufReader::new(client_app.try_clone().unwrap())
        .read_line(&mut reply)
        .unwrap();
    assert_eq!(reply, "OK\n", "the host's answer crossed the real relay");
    // Hang the client up for the whole socket, so the relay's client-to-host
    // pump reads EOF even while a sibling's fork holds a copy of the client
    // end; closing this test's descriptor is not that.
    hang_up(client_app);
    let reported = join_after_hang_up(forward_thread, "the forward to end on its client's hang-up");
    let delivered = host_read(&h);
    let after = state(delivered, reported);
    let (ok, why) = conforms(&init, &after, "Deliver");
    assert!(
        ok,
        "real relay: {init:?} -> {after:?} is not Deliver\n{why}"
    );
    join_after_hang_up(h.thread, "the stand-in host to finish serving");

    // 2. A relay stage that fails after delivery. The failing stage observes
    //    the delivery from the host's side first, so the run is split into its
    //    two real steps.
    let h = host();
    let (_client_app, client_relay) = CtlStream::pair().expect("pair");
    let mut at_relay = None;
    let (reported, _) = forward(&h.sock, &client_relay, PATIENCE, |_, _, _| {
        at_relay = Some(state(host_read(&h), false));
        Err(std::io::Error::from_raw_os_error(libc::EAGAIN))
    });
    let delivered = at_relay.expect("the relay stage ran");
    let (ok, why) = conforms(&init, &delivered, "Deliver");
    assert!(
        ok,
        "delivery: {init:?} -> {delivered:?} is not Deliver\n{why}"
    );
    let after = state(delivered["delivered"] == 1, reported);
    let (ok, why) = conforms(&delivered, &after, "RelayFail");
    assert!(
        ok,
        "a relay failure after delivery was reported: {delivered:?} -> {after:?}\n{why}"
    );
    join_after_hang_up(h.thread, "the stand-in host to finish serving");

    // 3. No listener: the connect finds no hop, nothing is delivered, and the
    //    local dispatch reports it.
    let dir = aterm_tempfile::TempDir::new_in("/tmp").unwrap();
    let absent = dir.path().join("absent.sock");
    let (_client_app, client_relay) = CtlStream::pair().expect("pair");
    let (reported, error) = forward(
        &absent.to_string_lossy(),
        &client_relay,
        PATIENCE,
        |_, _, _| Ok(()),
    );
    assert!(
        matches!(error, Some(ForwardError::NotAHop)),
        "no listener is no hop: {error:?}"
    );
    let after = state(false, reported);
    let (ok, why) = conforms(&init, &after, "DialFail");
    assert!(
        ok,
        "dial failure: {init:?} -> {after:?} is not DialFail\n{why}"
    );

    // 4. A host that never accepts: the connect probe goes unanswered,
    //    nothing is delivered, and the failure is reported.
    let dir = aterm_tempfile::TempDir::new_in("/tmp").unwrap();
    let wedged = dir.path().join("wedged.sock");
    std::fs::write(dir.path().join("wedged.sock.token"), "abcd\n").expect("token");
    let _listener = aterm_uds::CtlListener::bind(&wedged).expect("bind, never accept");
    let (_client_app, client_relay) = CtlStream::pair().expect("pair");
    let (reported, _) = forward(
        &wedged.to_string_lossy(),
        &client_relay,
        std::time::Duration::from_millis(300),
        |_, _, _| Ok(()),
    );
    let after = state(false, reported);
    let (ok, why) = conforms(&init, &after, "DialFail");
    assert!(
        ok,
        "an unaccepted forward: {init:?} -> {after:?} is not DialFail\n{why}"
    );

    // 5. A host with no token beside its socket once the connection exists:
    //    no hop, the forward sends nothing, and the failure is reported.
    let dir = aterm_tempfile::TempDir::new_in("/tmp").unwrap();
    let tokenless = dir.path().join("aterm-77031.sock");
    let listener = aterm_uds::CtlListener::bind(&tokenless).expect("bind the host");
    let heard = std::thread::spawn(move || {
        let (conn, _) = listener.accept().expect("accept");
        // In read slices, never one read parked on the end-of-file
        // ([`crate::test_socket::read_to_end_of_file`]).
        crate::test_socket::read_to_end_of_file(&conn, |read| {
            format!("the forward's end ({} bytes read)", read.len())
        })
    });
    let (_client_app, client_relay) = CtlStream::pair().expect("pair");
    let (reported, error) = forward(
        &tokenless.to_string_lossy(),
        &client_relay,
        PATIENCE,
        |_, _, _| Ok(()),
    );
    assert!(
        matches!(error, Some(ForwardError::NotAHop)),
        "no token is no hop: {error:?}"
    );
    // The host's `accept` returns either way: the forward's dropped
    // connection is first in its backlog, otherwise this closed connection
    // is, so the run is judged rather than hung.
    drop(CtlStream::connect(&tokenless));
    // Its read ends at the last close of the forward's end, which a
    // sibling's fork may hold until its exec: a delay, joined with a hang
    // detector.
    let heard = join_after_hang_up(heard, "the host to read the forward to its end");
    let delivered = String::from_utf8_lossy(&heard).lines().any(|l| l == LINE);
    let after = state(delivered, reported);
    let (ok, why) = conforms(&init, &after, "DialFail");
    assert!(
        ok,
        "a host with no token to present: {init:?} -> {after:?} is not DialFail \
         (it read {:?})\n{why}",
        String::from_utf8_lossy(&heard)
    );
}

/// The defect from a real delivered state: the relay failure reported as ERR.
#[test]
fn an_error_reported_after_delivery_is_the_buggy_step() {
    let model = reply_fidelity_model();
    let h = host();
    let (_client_app, client_relay) = CtlStream::pair().expect("pair");
    let mut delivered = None;
    let _ = forward(&h.sock, &client_relay, PATIENCE, |_, _, _| {
        delivered = Some(state(host_read(&h), false));
        Ok(())
    });
    let delivered = delivered.expect("the relay stage ran");
    assert_eq!(delivered["delivered"], 1, "the host really read the line");
    let falsely_failed = state(true, true);
    assert!(
        !conforms(&delivered, &falsely_failed, "RelayFail").0,
        "the committed model must reject ERR after delivery"
    );
    assert!(
        interp::with_buggy(&model, 1)
            .successors("RelayFail", &delivered)
            .contains(&falsely_failed),
        "Buggy = 1 admits exactly this report"
    );
    assert!(!model.check_invariant("NoErrorAfterDelivery", &falsely_failed));
    join_after_hang_up(h.thread, "the stand-in host to finish serving");
}
