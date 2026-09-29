// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A blocking verb gives its lane back when its caller hangs up, at the unit
//! level: the verbs' own hangup checks, and the lanes (`control_lanes.rs`)
//! telling a verb whose caller it serves. The end-to-end half — a real headless
//! instance saturated with waits whose callers then hang up — is
//! `crates/aterm/tests/control_lanes_live_headless.rs`.

use std::io::{BufReader, Write};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use aterm_uds::CtlStream;

use super::control_lanes::{LaneLimits, LaneService, Lanes};
use super::control_session::{TurnIo, cmd_await, cmd_turn};
use super::{
    HANGUP_POLL, HUNG_UP_REPLY, Scope, ServeDisposition, ServingFd, caller_hung_up, hangup_park,
};

/// Run `await <args>` on a fresh thread that is SERVING `server` (exactly what
/// a control lane does for the length of a connection), and return its reply.
fn await_while_serving(
    server: CtlStream,
    args: &'static str,
) -> std::thread::JoinHandle<(String, Instant)> {
    std::thread::spawn(move || {
        let handle = crate::session_store::test_handle(0);
        let store = crate::session_store::new_store();
        let registry = crate::subscribe::new_registry();
        let _serving = ServingFd::enter(&server);
        let reply = cmd_await(&handle.term, &store, 0, &handle.ctx, args, &registry);
        (reply, Instant::now())
    })
}

/// THE KILLED CLIENT. An `await` that can never latch parks on its lane; the
/// client dies (its socket closes both halves, exactly as a SIGKILL does). The
/// lane must be given back within a hangup poll — not at the 20 s timeout the
/// old code ran to while the client was already gone.
#[test]
fn a_hung_up_caller_releases_a_parked_await_within_a_poll() {
    let (client, server) = CtlStream::pair().expect("socket pair");
    let waiter = await_while_serving(server, "match never-on-this-screen-xyzzy timeout=20000");
    std::thread::sleep(Duration::from_millis(150));
    let hung_up = Instant::now();
    drop(client);
    let (reply, returned) = waiter.join().expect("await thread");
    let released = returned.saturating_duration_since(hung_up);
    assert_eq!(reply, HUNG_UP_REPLY, "the parked await must see the hangup");
    eprintln!("await lane released {released:?} after the caller hung up");
    assert!(
        released < HANGUP_POLL + Duration::from_millis(800),
        "a hung-up caller held its lane for {released:?}"
    );
}

/// THE NEGATIVE CONTROL: a caller that only HALF-closed (sent its request and
/// shut its write side) is still waiting for the answer, so the same await
/// runs to its own deadline and answers it.
#[test]
fn a_half_closed_caller_still_gets_its_answer() {
    let (client, server) = CtlStream::pair().expect("socket pair");
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("half-close");
    let started = Instant::now();
    let waiter = await_while_serving(server, "match never-on-this-screen-xyzzy timeout=600");
    let (reply, _) = waiter.join().expect("await thread");
    assert_eq!(
        reply, "OK timeout\n",
        "a half-closed caller is not a hangup"
    );
    assert!(started.elapsed() >= Duration::from_millis(600));
    drop(client);
}

/// A `turn` whose caller hangs up AFTER its text is typed still presses its
/// submit: the echo settle and the submit verification finish, because
/// abandoning them would leave the text sitting unsubmitted in the target's
/// composer, where the next turn's text is appended to it. (Only the waits
/// before the first byte and after a verified submit give the lane back.) The
/// hangup lands inside the paste, so the old code — every phase abortable —
/// returned `ERR exited` from the echo settle without pressing anything.
#[test]
fn a_turn_whose_caller_hangs_up_after_typing_still_submits() {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let (client, server) = CtlStream::pair().expect("socket pair");
    let client = Mutex::new(Some(client));
    let presses = AtomicUsize::new(0);
    let handle = crate::session_store::test_handle(0);
    let store = crate::session_store::new_store();
    let registry = crate::subscribe::new_registry();
    let _serving = ServingFd::enter(&server);
    let paste = |_: &str| {
        // The caller dies the moment its text is in the composer.
        drop(client.lock().expect("client slot").take());
        assert!(caller_hung_up(), "the hangup is visible to the lane");
        true
    };
    let press = |key: &str| {
        assert_eq!(key, "enter");
        presses.fetch_add(1, Ordering::SeqCst);
        true
    };
    let reply = cmd_turn(
        &handle.term,
        &store,
        0,
        "idle=1 timeout=5000 submit_window=300 presses=1 hello",
        &registry,
        &handle.ctx,
        &TurnIo {
            paste: &paste,
            press: &press,
            ..TurnIo::paste_only()
        },
    );
    assert_eq!(
        presses.load(Ordering::SeqCst),
        1,
        "the typed text must be submitted even though the caller hung up: {reply}"
    );
    assert!(
        !reply.starts_with("ERR"),
        "the turn ran to its verdict: {reply}"
    );
}

/// A `turn` whose caller hangs up AFTER its submit landed: the settle gives
/// the lane back at once (nothing is left half-typed), and the turn — typed
/// AND submitted — is still in the turn ledger, as `status=hangup`, because
/// `history`, the events digest and `aterm drive report` read it back. Its
/// verdict still says `submitted=1`, for an in-process reader such as the
/// durable operator. The old code returned `ERR exited` from the settle ahead
/// of the ledger push, so a submitted turn vanished from `history`
/// (2026-09-28 review). The negative control keeps its caller and settles:
/// `status=settled`.
#[test]
fn a_turn_whose_caller_hangs_up_after_its_submit_is_still_recorded() {
    let handle = crate::session_store::test_handle(0);
    let store = crate::session_store::new_store();
    let registry = crate::subscribe::new_registry();
    let last_turn = || {
        let ledger = handle
            .ctx
            .turns
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        ledger
            .records()
            .last()
            .map(|r| (r.text.clone(), r.submitted, r.status))
    };
    // The target echoes the text, and the Enter runs it: the press moves the
    // screen, so the submit verifies (`submitted=1`) and the settle runs.
    let paste = |text: &str| {
        handle.term.lock().expect("term").process(text.as_bytes());
        true
    };

    // The caller dies with its Enter: the settle (5 s of idle asked) is
    // where it finds out.
    let (client, server) = CtlStream::pair().expect("socket pair");
    let client = Mutex::new(Some(client));
    let hung_up = Mutex::new(None);
    let press = |key: &str| {
        assert_eq!(key, "enter");
        handle.term.lock().expect("term").process(b"\r\nran\r\n");
        drop(client.lock().expect("client slot").take());
        *hung_up.lock().expect("hangup time") = Some(Instant::now());
        true
    };
    let serving = ServingFd::enter(&server);
    let reply = cmd_turn(
        &handle.term,
        &store,
        0,
        "idle=5000 timeout=20000 submit_window=2000 presses=1 gone-caller",
        &registry,
        &handle.ctx,
        &TurnIo {
            paste: &paste,
            press: &press,
            ..TurnIo::paste_only()
        },
    );
    let returned = Instant::now();
    drop(serving);
    let hung_up = hung_up
        .lock()
        .expect("hangup time")
        .expect("the turn pressed its Enter");
    let released = returned.saturating_duration_since(hung_up);
    assert!(
        reply.contains("submitted=1 status=hangup"),
        "the settle must see the hangup: {reply}"
    );
    assert!(
        released < HANGUP_POLL + Duration::from_millis(800),
        "a hung-up turn held its lane for {released:?}"
    );
    assert_eq!(
        last_turn(),
        Some(("gone-caller".to_string(), true, "hangup")),
        "the submitted turn must be in the ledger"
    );

    // THE NEGATIVE CONTROL: the same turn with its caller still there runs
    // its settle and is recorded as settled.
    let (_client, server) = CtlStream::pair().expect("socket pair");
    let press = |_: &str| {
        handle.term.lock().expect("term").process(b"\r\nran\r\n");
        true
    };
    let _serving = ServingFd::enter(&server);
    let reply = cmd_turn(
        &handle.term,
        &store,
        0,
        "idle=50 timeout=20000 submit_window=2000 presses=1 kept-caller",
        &registry,
        &handle.ctx,
        &TurnIo {
            paste: &paste,
            press: &press,
            ..TurnIo::paste_only()
        },
    );
    assert!(
        reply.contains("submitted=1 status=settled"),
        "a caller that stays gets its verdict: {reply}"
    );
    assert_eq!(
        last_turn(),
        Some(("kept-caller".to_string(), true, "settled"))
    );
}

/// Off a serving lane (an in-process caller, a unit test) nothing is ever hung
/// up and every park is exactly as long as asked.
#[test]
fn an_unserved_thread_never_hangs_up_and_parks_as_asked() {
    assert!(!caller_hung_up());
    assert_eq!(hangup_park(Duration::from_secs(9)), Duration::from_secs(9));
    let (_client, server) = CtlStream::pair().expect("socket pair");
    let serving = ServingFd::enter(&server);
    assert_eq!(hangup_park(Duration::from_secs(9)), HANGUP_POLL);
    assert!(!caller_hung_up(), "a live caller is not hung up");
    drop(serving);
    assert_eq!(hangup_park(Duration::from_secs(9)), Duration::from_secs(9));
}

/// A lane service whose one wait, `park`, parks the way every blocking verb
/// does — [`hangup_park`] slices, [`caller_hung_up`] asked on each wake — and
/// reports when it gave its lane back.
struct ParkingService {
    released: Mutex<mpsc::Sender<Instant>>,
}

impl LaneService for ParkingService {
    fn authenticate(
        &self,
        _stream: &CtlStream,
        reader: &mut BufReader<&CtlStream>,
    ) -> Option<(Scope, Option<String>)> {
        (super::read_request_line(reader)? == "AUTH ok").then_some((Scope::Owner, None))
    }

    fn serve_line(
        &self,
        line: String,
        _scope: Scope,
        stream: &CtlStream,
        _reader: &mut BufReader<&CtlStream>,
    ) -> Option<ServeDisposition> {
        let reply = if line == "park" {
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                if caller_hung_up() {
                    let _ = self
                        .released
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .send(Instant::now());
                    break HUNG_UP_REPLY.to_string();
                }
                let now = Instant::now();
                if now >= deadline {
                    break "OK timeout\n".to_string();
                }
                std::thread::sleep(hangup_park(deadline - now));
            }
        } else {
            format!("OK {line}\n")
        };
        let mut writer = stream;
        writer
            .write_all(reply.as_bytes())
            .is_err()
            .then_some(ServeDisposition::Close)
    }

    fn is_wait(&self, line: &str, _scope: Scope) -> bool {
        line == "park"
    }

    fn subscribe(&self, _line: String, _scope: Scope, _stream: CtlStream) {}

    fn release(&self, _id: u64) {}
}

/// THE LANES TELL A VERB WHOSE CALLER IT SERVES. A wait handed to a wait lane
/// by the real [`Lanes`] parks there; its client dies. The lane must see the
/// hangup and be given back within a hangup poll — which it can only do if
/// the lane that carries the connection marked it as the one being served. A
/// client that is still there keeps its wait parked (the negative control).
#[test]
fn a_wait_lane_is_given_back_when_its_caller_hangs_up() {
    let (released_tx, released) = mpsc::channel();
    let lanes = Lanes::new(LaneLimits {
        rpc: 1,
        wait: 1,
        open: 8,
    });
    assert_eq!(lanes.start(), 1, "the request lane starts");
    lanes.publish(Arc::new(ParkingService {
        released: Mutex::new(released_tx),
    }));

    let (mut client, server) = CtlStream::pair().expect("socket pair");
    assert!(lanes.admit(server).is_ok(), "the connection is admitted");
    client.write_all(b"AUTH ok\npark\n").expect("send the wait");

    // Parked on its wait lane, with the caller alive: nothing is released.
    assert!(
        released.recv_timeout(Duration::from_millis(600)).is_err(),
        "a wait whose caller is still there was released"
    );
    let census = lanes.census();
    assert_eq!(
        (census.waits, census.wait_threads),
        (1, 1),
        "the wait is served on a wait lane: {census:?}"
    );

    let hung_up = Instant::now();
    drop(client);
    let at = released
        .recv_timeout(Duration::from_secs(10))
        .expect("the wait lane never saw its caller hang up");
    let took = at.saturating_duration_since(hung_up);
    eprintln!("wait lane released {took:?} after the caller hung up");
    assert!(
        took < HANGUP_POLL + Duration::from_millis(800),
        "a hung-up caller held its wait lane for {took:?}"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while lanes.census().waits != 0 || lanes.census().open != 0 {
        assert!(
            Instant::now() < deadline,
            "the lanes still hold the hung-up connection: {:?}",
            lanes.census()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
