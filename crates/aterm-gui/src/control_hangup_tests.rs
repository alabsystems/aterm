// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A blocking verb gives its lane back when its caller hangs up, at the unit
//! level: the verbs' own hangup checks, and the lanes (`control_lanes.rs`)
//! telling a verb whose caller it serves. The end-to-end half — a real headless
//! instance saturated with waits whose callers then hang up — is
//! `crates/aterm/tests/control_lanes_live_headless.rs`.

use std::io::Write;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_uds::CtlStream;

use super::control_lanes::{LaneLimits, LaneReader, LaneService, Lanes};
use super::control_session::{TurnIo, cmd_await, cmd_turn};
use super::{
    HANGUP_POLL, HUNG_UP_REPLY, Scope, ServeDisposition, ServingFd, caller_hung_up, hangup_park,
};

/// Run `await <args>` on a fresh thread that is SERVING `server` (exactly what
/// a control lane does for the length of a connection), and return its reply.
/// The await registers on `registry` before its first hangup check, so a
/// watched session there says it is parked.
fn await_while_serving(
    server: CtlStream,
    registry: crate::subscribe::Subscribers,
    args: &'static str,
) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let handle = crate::session_store::test_handle(0);
        let store = crate::session_store::new_store();
        let _serving = ServingFd::enter(&server);
        cmd_await(&handle.term, &store, 0, &handle.ctx, args, &registry)
    })
}

/// Wait for `seen` — the lane's view of its caller's hangup — with a HANG
/// DETECTOR ([`aterm_spec::wait::MUST_HAPPEN`]), never a latency budget.
///
/// A dropped caller is hung up only once its socket's LAST descriptor closes:
/// the lane's probe (`aterm_uds::hangup`) reads `POLLHUP` on its write side,
/// and nothing short of that last close raises it. In this lib-test process
/// sibling tests fork all the time, and each child holds a copy of every
/// descriptor until it execs (macOS 26.6 does not honor `FD_CLOFORK`), and
/// std on macOS marks a socketpair close-on-exec only after creating it, so a
/// fork in between hands over a copy that outlives the exec as well. The full
/// suite failed here under load, never alone. Hanging the caller up with a
/// `shutdown` — the remedy for the read-side probes, which see its EOF at once
/// — cannot stand in for that last close: while a copy lives the lane's
/// write-side probe then reads neither `POLLOUT` nor `POLLHUP` (measured on
/// macOS 26.6), and a half-close must never read as a hangup. So the hangup is
/// a fact these tests wait for.
#[track_caller]
fn until_seen(what: &str, mut seen: impl FnMut() -> bool) {
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, what, || seen().then_some(()));
}

/// Join an await thread with a hang detector: the await asks a ten-minute
/// timeout, so the test's own minute decides when a never-seen hangup fails.
#[track_caller]
fn join_await(waiter: std::thread::JoinHandle<String>, what: &str) -> String {
    until_seen(what, || waiter.is_finished());
    waiter.join().expect("await thread")
}

/// THE KILLED CLIENT. An `await` that can never latch parks on its lane; the
/// client dies (its socket closes both halves, exactly as a SIGKILL does). The
/// lane must be given back on the hangup — not at the await's own timeout,
/// which the old code ran to while the client was already gone. That timeout
/// is the longest an await takes (ten minutes), so only a seen hangup can
/// answer within the test's hang detector ([`join_await`]).
#[test]
fn a_hung_up_caller_releases_a_parked_await_within_a_poll() {
    let (client, server) = CtlStream::pair().expect("socket pair");
    let registry = crate::subscribe::new_registry();
    let waiter = await_while_serving(
        server,
        registry.clone(),
        "match never-on-this-screen-xyzzy timeout=600000",
    );
    until_seen("the await to park on its session", || {
        registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .watched_sessions()
            == 1
    });
    drop(client);
    let reply = join_await(waiter, "the parked await to see its caller hang up");
    assert_eq!(reply, HUNG_UP_REPLY, "the parked await must see the hangup");
}

/// THE NEGATIVE CONTROL: a caller that only HALF-closed (sent its request and
/// shut its write side) is still waiting for the answer, so the same await
/// runs to its own deadline and answers it. That deadline is just past one
/// [`HANGUP_POLL`] slice: the await asks after its caller before it parks and
/// again after each slice, so a half-close read as a hangup fails at once.
#[test]
fn a_half_closed_caller_still_gets_its_answer() {
    let (client, server) = CtlStream::pair().expect("socket pair");
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("half-close");
    let waiter = await_while_serving(
        server,
        crate::subscribe::new_registry(),
        "match never-on-this-screen-xyzzy timeout=250",
    );
    let reply = join_await(waiter, "the half-closed caller's await to time out");
    assert_eq!(
        reply, "OK timeout\n",
        "a half-closed caller is not a hangup"
    );
    drop(client);
}

/// Whether a main-thread hop made now would be refused because its caller
/// hung up — the first check [`super::control_media::call_main`] makes, the
/// gate a Front- or Local-routed turn's keys and Enter go through.
fn main_hop_refused_for_hangup() -> bool {
    use super::control_media::{CLIENT_HUNG_UP, MainHopError, main_thread_refusal};
    main_thread_refusal() == Some(MainHopError::Reason(CLIENT_HUNG_UP))
}

/// A `turn` whose caller hangs up AFTER its text is typed still presses its
/// submit: the echo settle and the submit verification finish, because
/// abandoning them would leave the text sitting unsubmitted in the target's
/// composer, where the next turn's text is appended to it. (Only the waits
/// before the first byte and after a verified submit give the lane back.) The
/// hangup lands inside the paste, so the old code — every phase abortable —
/// returned `ERR exited` from the echo settle without pressing anything.
///
/// Its Enter goes through the gate the shipped routes take: the tab on
/// screen (`@<sid>` of the visible tab, or a flagless turn) presses through
/// `call_main`, which refuses a hop whose caller hung up (2e14db320). The
/// turn is committed once its text is in the composer, so its Enter is not
/// refused: refusing it left the text sitting unsubmitted, the turn answered
/// `ERR usage` and was never recorded (round eight of the update audit, U51).
/// The negative control: before anything is typed, the same hangup still
/// refuses the hop.
#[test]
fn a_turn_whose_caller_hangs_up_after_typing_still_submits_through_the_main_thread_gate() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let (client, server) = CtlStream::pair().expect("socket pair");
    let client = Mutex::new(Some(client));
    let presses = AtomicUsize::new(0);
    let handle = crate::session_store::test_handle(0);
    let store = crate::session_store::new_store();
    let registry = crate::subscribe::new_registry();
    let _serving = ServingFd::enter(&server);
    let paste = |_: &str| {
        if main_hop_refused_for_hangup() {
            return false;
        }
        // The caller dies the moment its text is in the composer, and the
        // turn goes on once its lane has seen that ([`until_seen`]: the full
        // suite failed here, 1 in ~6900 tests, never alone).
        drop(client.lock().expect("client slot").take());
        until_seen("the hangup to become visible to the lane", caller_hung_up);
        true
    };
    let press = |key: &str| {
        assert_eq!(key, "enter");
        if main_hop_refused_for_hangup() {
            return false;
        }
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
    // The commitment ends with the turn: the next hop on this lane is
    // refused for the hangup, as any request of a gone caller is.
    assert!(
        main_hop_refused_for_hangup(),
        "a hop after the turn is refused for the hangup"
    );
}

/// `typed=1`: a caller that hangs up after the first key still gets the rest
/// of its keys and its Enter through the main-thread gate, or a partial
/// prompt is left in the composer (`ERR key delivery failed`, U51).
#[test]
fn a_typed_turn_whose_caller_hangs_up_mid_typing_still_types_and_submits() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let (client, server) = CtlStream::pair().expect("socket pair");
    let client = Mutex::new(Some(client));
    let keys = AtomicUsize::new(0);
    let presses = AtomicUsize::new(0);
    let handle = crate::session_store::test_handle(0);
    let store = crate::session_store::new_store();
    let registry = crate::subscribe::new_registry();
    let _serving = ServingFd::enter(&server);
    let key = |_: crate::input::InputEvent| {
        if main_hop_refused_for_hangup() {
            return false;
        }
        if keys.fetch_add(1, Ordering::SeqCst) == 0 {
            drop(client.lock().expect("client slot").take());
            until_seen("the hangup to become visible to the lane", caller_hung_up);
        }
        true
    };
    let press = |_: &str| {
        if main_hop_refused_for_hangup() {
            return false;
        }
        presses.fetch_add(1, Ordering::SeqCst);
        true
    };
    let reply = cmd_turn(
        &handle.term,
        &store,
        0,
        "idle=1 timeout=20000 submit_window=300 presses=1 typed=1 cadence=1 hey",
        &registry,
        &handle.ctx,
        &TurnIo {
            press: &press,
            key: &key,
            ..TurnIo::paste_only()
        },
    );
    assert_eq!(
        keys.load(Ordering::SeqCst),
        3,
        "every key of the text is typed: {reply}"
    );
    assert_eq!(presses.load(Ordering::SeqCst), 1, "and submitted: {reply}");
    assert!(
        !reply.starts_with("ERR"),
        "the turn ran to its verdict: {reply}"
    );
}

/// THE NEGATIVE CONTROL for the two above: a caller that hung up before the
/// first byte is typed has nothing half-applied, so the turn's first hop is
/// refused and nothing reaches the composer.
#[test]
fn a_turn_whose_caller_hung_up_before_typing_types_nothing() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let (client, server) = CtlStream::pair().expect("socket pair");
    let pastes = AtomicUsize::new(0);
    let handle = crate::session_store::test_handle(0);
    let store = crate::session_store::new_store();
    let registry = crate::subscribe::new_registry();
    let _serving = ServingFd::enter(&server);
    drop(client);
    until_seen("the hangup to become visible to the lane", caller_hung_up);
    let paste = |_: &str| {
        if main_hop_refused_for_hangup() {
            return false;
        }
        pastes.fetch_add(1, Ordering::SeqCst);
        true
    };
    let press = |_: &str| !main_hop_refused_for_hangup();
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
    assert_eq!(pastes.load(Ordering::SeqCst), 0, "nothing typed: {reply}");
    assert_eq!(reply, "ERR paste delivery failed\n");
}

/// A `turn` whose caller hangs up AFTER its submit landed: the settle gives
/// the lane back at once (nothing is left half-typed), and the turn — typed
/// AND submitted — is still in the turn ledger, as `status=hangup`, because
/// `history`, the events digest and the harness's report reader read it back. Its
/// verdict still says `submitted=1`, for an in-process reader. The old code
/// returned `ERR exited` from the settle ahead of the ledger push, so a
/// submitted turn vanished from `history` (2026-09-28 review). The negative
/// control keeps its caller and settles: `status=settled`.
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
    // where it finds out — once its lane has seen it ([`until_seen`]), or a
    // copy a sibling's fork holds runs the settle out as `status=settled`.
    let (client, server) = CtlStream::pair().expect("socket pair");
    let client = Mutex::new(Some(client));
    let press = |key: &str| {
        assert_eq!(key, "enter");
        handle.term.lock().expect("term").process(b"\r\nran\r\n");
        drop(client.lock().expect("client slot").take());
        until_seen("the hangup to become visible to the lane", caller_hung_up);
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
    drop(serving);
    assert!(
        reply.contains("submitted=1 status=hangup"),
        "the settle must see the hangup: {reply}"
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
/// records every answer that check gave: the facts the test reads, the last
/// `true` being the moment it gave its lane back.
#[derive(Default)]
struct ParkingService {
    checks: Mutex<Vec<bool>>,
}

impl ParkingService {
    /// Each answer the parked wait's [`caller_hung_up`] gave so far, in order.
    fn checks(&self) -> Vec<bool> {
        self.checks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl LaneService for ParkingService {
    fn authenticate(
        &self,
        _stream: &CtlStream,
        reader: &mut LaneReader<'_>,
    ) -> Option<(Scope, crate::control::Principal, Option<String>)> {
        (super::read_request_line(reader)? == "AUTH ok").then_some((
            Scope::Owner,
            crate::control::Principal::Instance,
            None,
        ))
    }

    fn serve_line(
        &self,
        line: String,
        _scope: Scope,
        _stream: &CtlStream,
        _reader: &mut LaneReader<'_>,
        out: &mut Vec<u8>,
    ) -> Option<ServeDisposition> {
        let reply = if line == "park" {
            // Past the test's hang detector: only a seen hangup releases it.
            let deadline = Instant::now() + Duration::from_secs(600);
            loop {
                let hung_up = caller_hung_up();
                self.checks
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(hung_up);
                if hung_up {
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
        out.extend_from_slice(reply.as_bytes());
        None
    }

    fn is_wait(&self, line: &str, _scope: Scope) -> bool {
        line == "park"
    }

    fn subscribe(&self, _line: String, _scope: Scope, _stream: CtlStream) {}

    fn release(&self, _id: u64) {}

    fn has_live_claim(&self, _id: u64) -> bool {
        false
    }
}

/// THE LANES TELL A VERB WHOSE CALLER IT SERVES. A wait handed to a wait lane
/// by the real [`Lanes`] parks there; its client dies. The lane must see the
/// hangup and be given back within a hangup poll — which it can only do if
/// the lane that carries the connection marked it as the one being served. A
/// client that is still there keeps its wait parked (the negative control):
/// the parked wait ASKED after it and was told it is there — a fact read off
/// the check itself, never a window in which nothing happened.
#[test]
fn a_wait_lane_is_given_back_when_its_caller_hangs_up() {
    let service = Arc::new(ParkingService::default());
    let lanes = Lanes::new(LaneLimits {
        rpc: 1,
        wait: 1,
        inline: 0,
        open: 8,
    });
    assert_eq!(lanes.start(), 1, "the request lane starts");
    lanes.publish(service.clone());

    let (mut client, server) = CtlStream::pair().expect("socket pair");
    assert!(lanes.admit(server).is_ok(), "the connection is admitted");
    client.write_all(b"AUTH ok\npark\n").expect("send the wait");

    // Parked, with the caller alive: the wait's hangup check has run, and
    // every answer it gave says the caller is there.
    until_seen("the parked wait to ask after its caller", || {
        !service.checks().is_empty()
    });
    assert!(
        !service.checks().contains(&true),
        "a wait whose caller is still there saw a hangup: {:?}",
        service.checks()
    );
    // It is inside the wait (the check ran), so where it runs is settled.
    let census = lanes.census();
    assert_eq!(
        (census.waits, census.wait_threads),
        (1, 1),
        "the wait is served on a wait lane: {census:?}"
    );

    drop(client);
    until_seen("the wait lane to see its caller hang up", || {
        service.checks().last() == Some(&true)
    });
    until_seen("the lanes to let the hung-up connection go", || {
        let census = lanes.census();
        census.waits == 0 && census.open == 0
    });
}

/// A MAIN-THREAD REQUEST WHOSE CALLER HUNG UP is refused before anything is
/// posted (2026-09-28: the whole app stopped running for 3h22m, and requests
/// whose clients had given up and asked again still waited on the main
/// thread). The refusal every main-thread hop consults answers that the client
/// hung up once the caller closed. Negative controls: a connected caller, and
/// a half-closed one that still reads its reply, are not turned away for it.
#[test]
fn a_main_thread_request_whose_caller_hung_up_is_refused_before_it_is_posted() {
    use super::control_media::{CLIENT_HUNG_UP, MainHopError, main_thread_refusal};

    let hung_up = Some(MainHopError::Reason(CLIENT_HUNG_UP));
    let (client, server) = CtlStream::pair().expect("socket pair");
    let _serving = ServingFd::enter(&server);
    assert_ne!(
        main_thread_refusal(),
        hung_up,
        "a connected caller is served"
    );
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("half-close");
    assert_ne!(
        main_thread_refusal(),
        hung_up,
        "a half-closed caller still reads its reply"
    );
    drop(client);
    // The refusal sees the hangup once its lane does ([`until_seen`]: the
    // full suite failed here under load, never alone).
    until_seen(
        "a closed caller's request to be refused before it is posted",
        || main_thread_refusal() == hung_up,
    );
}
