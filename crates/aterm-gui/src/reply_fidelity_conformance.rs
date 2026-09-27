// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 bind for `ReplyFidelity` (`aterm_spec::derive::reply_fidelity_model`):
//! once a forwarded verb has reached the child, no later failure may be
//! reported to the client as `ERR forward` — and a failure before it reached
//! the child must be.
//!
//! Drives the shipping `deliver_then_relay` (the whole of `connect_and_relay`
//! but its relay stage) against a real child socket, and projects:
//!
//! * `delivered` — the child ACTUALLY READ the forwarded first line (observed on
//!   the child's side of the socket, not inferred from the forwarder);
//! * `reported_err` — the call returned `Err`, which is exactly when
//!   `try_proxy_forward` writes `ERR forward` to the client.
//!
//! Three real runs: the real relay (delivered, no error), a relay stage that
//! fails AFTER delivery (the `try_clone`-under-fd-exhaustion shape, injected at
//! the seam because it cannot be staged in a shared test process), and a dial
//! with no listener (reported, never delivered).
//!
//! NEGATIVE CONTROL: from the real delivered state, reporting the relay failure
//! is rejected by the committed model and admitted by `Buggy = 1`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc;

use aterm_spec::derive::reply_fidelity_model;
use aterm_spec::{interp, verify};

use super::*;

type State = BTreeMap<&'static str, i64>;

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

/// A throwaway child listener that reports the first line it READS, then
/// answers and hangs up.
struct Child {
    sock: String,
    read: mpsc::Receiver<String>,
    thread: std::thread::JoinHandle<()>,
    _dir: aterm_tempfile::TempDir,
}

fn child() -> Child {
    let dir = aterm_tempfile::TempDir::new_in("/tmp").unwrap();
    let path = dir.path().join("child.sock");
    let listener = aterm_uds::CtlListener::bind(&path).expect("bind child");
    let (tx, read) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().expect("accept");
        let mut first = String::new();
        let _ = BufReader::new(conn.try_clone().unwrap()).read_line(&mut first);
        let _ = tx.send(first);
        let _ = conn.write_all(b"OK\n");
        let _ = conn.shutdown(std::net::Shutdown::Both);
    });
    Child {
        sock: path.to_string_lossy().into_owned(),
        read,
        thread,
        _dir: dir,
    }
}

const LINE: &str = "TOKEN abcd @. screen\n";

/// Did the child read exactly the forwarded line? Waits for the child's read,
/// bounded, so an undelivered line projects as `false` rather than hanging.
fn child_read(c: &Child) -> bool {
    c.read
        .recv_timeout(std::time::Duration::from_secs(5))
        .is_ok_and(|line| line == LINE)
}

#[test]
fn real_forward_outcomes_conform_to_reply_fidelity_model() {
    let init = reply_fidelity_model().init_state();

    // 1. The real relay: delivered, and nothing reported.
    let c = child();
    let (client_app, client_relay) = CtlStream::pair().expect("pair");
    let sock = c.sock.clone();
    let forward =
        std::thread::spawn(move || connect_and_relay(&sock, LINE, &client_relay, &[]).is_err());
    let mut reply = String::new();
    BufReader::new(client_app.try_clone().unwrap())
        .read_line(&mut reply)
        .unwrap();
    assert_eq!(reply, "OK\n", "the child's answer crossed the real relay");
    let _ = client_app.shutdown(std::net::Shutdown::Both);
    drop(client_app);
    let reported = forward.join().unwrap();
    let delivered = child_read(&c);
    let after = state(delivered, reported);
    let (ok, why) = conforms(&init, &after, "Deliver");
    assert!(
        ok,
        "real relay: {init:?} -> {after:?} is not Deliver\n{why}"
    );
    c.thread.join().unwrap();

    // 2. A relay stage that fails AFTER delivery. The failing stage observes
    //    the delivery from the child's side first, so the run is split into its
    //    two real steps.
    let c = child();
    let (_client_app, client_relay) = CtlStream::pair().expect("pair");
    let mut at_relay = None;
    let result = deliver_then_relay(&c.sock, LINE, &client_relay, &[], |_, _| {
        at_relay = Some(state(child_read(&c), false));
        Err(std::io::Error::from_raw_os_error(libc::EMFILE))
    });
    let delivered = at_relay.expect("the relay stage ran");
    let (ok, why) = conforms(&init, &delivered, "Deliver");
    assert!(
        ok,
        "delivery: {init:?} -> {delivered:?} is not Deliver\n{why}"
    );
    let after = state(delivered["delivered"] == 1, result.is_err());
    let (ok, why) = conforms(&delivered, &after, "RelayFail");
    assert!(
        ok,
        "a relay failure after delivery was reported: {delivered:?} -> {after:?}\n{why}"
    );
    c.thread.join().unwrap();

    // 3. No listener: the dial fails before anything is delivered, and that
    //    failure IS reported.
    let dir = aterm_tempfile::TempDir::new_in("/tmp").unwrap();
    let absent = dir.path().join("absent.sock");
    let (_client_app, client_relay) = CtlStream::pair().expect("pair");
    let result = connect_and_relay(&absent.to_string_lossy(), LINE, &client_relay, &[]);
    let after = state(false, result.is_err());
    let (ok, why) = conforms(&init, &after, "DialFail");
    assert!(
        ok,
        "dial failure: {init:?} -> {after:?} is not DialFail\n{why}"
    );
}

/// The defect from a REAL delivered state: the relay failure reported as ERR.
#[test]
fn an_error_reported_after_delivery_is_the_buggy_step() {
    let model = reply_fidelity_model();
    let c = child();
    let (_client_app, client_relay) = CtlStream::pair().expect("pair");
    let mut delivered = None;
    let _ = deliver_then_relay(&c.sock, LINE, &client_relay, &[], |_, _| {
        delivered = Some(state(child_read(&c), false));
        Ok(())
    });
    let delivered = delivered.expect("the relay stage ran");
    assert_eq!(delivered["delivered"], 1, "the child really read the line");
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
    c.thread.join().unwrap();
}
