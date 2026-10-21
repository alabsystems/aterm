// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unit tests that a blocking verb gives its lane back when its caller hangs up: the verbs'
//! hangup checks, and the lanes (`control_lanes.rs`) marking which caller a verb serves.
//! The end-to-end half is `crates/aterm/tests/control_lanes_live_headless.rs`.

use std::io::Write;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_uds::CtlStream;

use super::control_lanes::{LaneLimits, LaneReader, LaneService, Lanes, Outbox};
use super::control_session::{TurnIo, cmd_await, cmd_turn};
use super::{
    HANGUP_POLL, HUNG_UP_REPLY, Request, Scope, ServeDisposition, ServingFd, caller_hung_up,
    hangup_park,
};

/// Run `await <args>` on a fresh thread that is SERVING `server` (exactly what
/// a control lane does for the length of a connection), and return its reply
/// and its session's watchers. The await registers on them before its first
/// hangup check, so a watcher there says it is parked.
fn await_while_serving(
    server: CtlStream,
    args: &'static str,
) -> (
    std::thread::JoinHandle<String>,
    Arc<crate::session_watch::Watchers>,
) {
    let handle = crate::session_store::test_handle(0);
    let watchers = handle.ctx.watchers.clone();
    let waiter = std::thread::spawn(move || {
        let store = crate::session_store::new_store();
        let _serving = ServingFd::enter(&server);
        cmd_await(&handle.term, &store, 0, &handle.ctx, args)
    });
    (waiter, watchers)
}

/// Wait for `seen` (the lane's view of its caller's hangup) with a hang detector
/// ([`aterm_spec::wait::MUST_HAPPEN`]), never a latency budget.
///
/// A dropped caller reads as hung up (`POLLHUP` on the lane's write-side probe,
/// `aterm_uds::hangup`) only once its socket's last descriptor closes, and sibling tests'
/// forks hold copies until they exec: macOS does not honor `FD_CLOFORK`, and std marks a
/// socketpair close-on-exec only after creating it. A `shutdown` cannot stand in, since a
/// half-close must never read as a hangup, so these tests wait for the hangup.
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

/// A killed client (both halves closed, as SIGKILL does) gives back the lane of an `await`
/// parked on it at the hangup, not at the await's ten-minute timeout.
#[test]
fn a_hung_up_caller_releases_a_parked_await_within_a_poll() {
    let (client, server) = CtlStream::pair().expect("socket pair");
    let (waiter, watchers) =
        await_while_serving(server, "match never-on-this-screen-xyzzy timeout=600000");
    until_seen("the await to park on its session", || watchers.count() == 1);
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
    let (waiter, _) = await_while_serving(server, "match never-on-this-screen-xyzzy timeout=250");
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

/// A `turn` whose caller hangs up after its text is typed still presses its submit through
/// the main-thread gate (`call_main`): abandoning it would leave the text unsubmitted in the
/// composer, where the next turn's text is appended. The turn is committed once its text is
/// in the composer; only the waits before the first byte and after a verified submit give
/// the lane back.
#[test]
fn a_turn_whose_caller_hangs_up_after_typing_still_submits_through_the_main_thread_gate() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let (client, server) = CtlStream::pair().expect("socket pair");
    let client = Mutex::new(Some(client));
    let presses = AtomicUsize::new(0);
    let handle = crate::session_store::test_handle(0);
    let store = crate::session_store::new_store();
    let _serving = ServingFd::enter(&server);
    let paste = |_: &str| {
        if main_hop_refused_for_hangup() {
            return false;
        }
        // The caller dies once its text is in the composer; the turn goes on once its
        // lane has seen that ([`until_seen`]).
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
    // The commitment ends with the turn: the next hop on this lane is refused.
    assert!(
        main_hop_refused_for_hangup(),
        "a hop after the turn is refused for the hangup"
    );
}

/// `typed=1`: a caller that hangs up after the first key still gets the rest of its keys and
/// its Enter through the main-thread gate, or a partial prompt is left in the composer.
#[test]
fn a_typed_turn_whose_caller_hangs_up_mid_typing_still_types_and_submits() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let (client, server) = CtlStream::pair().expect("socket pair");
    let client = Mutex::new(Some(client));
    let keys = AtomicUsize::new(0);
    let presses = AtomicUsize::new(0);
    let handle = crate::session_store::test_handle(0);
    let store = crate::session_store::new_store();
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

/// Negative control for the two above: a caller that hung up before the first byte has
/// nothing half-applied, so the turn's first hop is refused and nothing is typed.
#[test]
fn a_turn_whose_caller_hung_up_before_typing_types_nothing() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let (client, server) = CtlStream::pair().expect("socket pair");
    let pastes = AtomicUsize::new(0);
    let handle = crate::session_store::test_handle(0);
    let store = crate::session_store::new_store();
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
    let last_turn = || {
        let ledger = handle
            .ctx
            .turns
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        ledger
            .records()
            .last()
            .map(|r| (r.text.to_string(), r.submitted, r.status))
    };
    // The target echoes the text, and the Enter runs it: the press moves the
    // screen, so the submit verifies (`submitted=1`) and the settle runs.
    let paste = |text: &str| {
        handle.term.lock().expect("term").process(text.as_bytes());
        true
    };

    // The caller dies with its Enter; the settle (5 s of idle asked) sees it once the lane
    // has ([`until_seen`]).
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

    // Negative control: with its caller still there the same turn settles.
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

/// A WRITE HANDED TO THE SESSION'S ORDERED WRITER GIVES ITS LANE BACK WHEN ITS
/// CALLER HANGS UP. [`super::await_written`] waits in the one park every
/// blocking verb waits in: a write the writer never runs, waited on for ten
/// minutes, answers `HungUp` once the serving connection's caller is gone, so
/// only a seen hangup answers within the test's hang detector. Off a serving
/// lane the writer's own answers are what they say: its receipt is the write's,
/// and a write dropped unrun wrote nothing.
///
/// Negative control: a caller that only half-closed is still there, so the same
/// wait, bounded just past one [`HANGUP_POLL`] slice, asks after it and runs to
/// its bound: `Queued`.
#[test]
fn a_write_waited_on_gives_its_lane_back_when_its_caller_hangs_up() {
    use super::{Waited, await_written};
    use crate::app_input::paste_order::Written;
    use crate::input::EgressReceipt;

    let (client, server) = CtlStream::pair().expect("socket pair");
    let (unrun, written) = Written::for_test();
    let waiter = std::thread::spawn(move || {
        let _serving = ServingFd::enter(&server);
        matches!(
            await_written(&written, Duration::from_secs(600)),
            Waited::HungUp
        )
    });
    drop(client);
    until_seen("the write's wait to see its caller hang up", || {
        waiter.is_finished()
    });
    assert!(waiter.join().expect("waiter"), "the hangup ends the wait");
    drop(unrun);

    let (client, server) = CtlStream::pair().expect("socket pair");
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("half-close");
    let (unrun, written) = Written::for_test();
    let waiter = std::thread::spawn(move || {
        let _serving = ServingFd::enter(&server);
        matches!(
            await_written(&written, HANGUP_POLL + Duration::from_millis(50)),
            Waited::Queued
        )
    });
    until_seen("the half-closed caller's wait to run to its bound", || {
        waiter.is_finished()
    });
    assert!(
        waiter.join().expect("waiter"),
        "a half-closed caller is not a hangup"
    );
    drop((unrun, client));

    let (writer, written) = Written::for_test();
    writer
        .send(EgressReceipt::deferred_full())
        .expect("the writer answers");
    assert!(matches!(
        await_written(&written, aterm_spec::wait::MUST_HAPPEN),
        Waited::Wrote(receipt) if receipt == EgressReceipt::deferred_full()
    ));
    let (writer, written) = Written::for_test();
    drop(writer);
    assert!(matches!(
        await_written(&written, aterm_spec::wait::MUST_HAPPEN),
        Waited::Unrun
    ));
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

/// A lane service whose one wait, `park`, parks as every blocking verb does
/// ([`hangup_park`] slices, [`caller_hung_up`] on each wake) and records each check's answer.
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
        request: Request,
        _scope: Scope,
        _stream: &CtlStream,
        _reader: &mut LaneReader<'_>,
        out: &mut Outbox,
    ) -> Option<ServeDisposition> {
        let line = request.line();
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

    fn is_wait(&self, request: &Request, _scope: Scope) -> bool {
        request.line() == "park"
    }

    fn subscribe(&self, _request: Request, _scope: Scope, _stream: CtlStream) {}

    fn release(&self, _id: u64) {}

    fn has_live_claim(&self, _id: u64) -> bool {
        false
    }
}

/// The real [`Lanes`] mark the connection a wait lane serves, so a wait parked there sees its
/// caller hang up and gives the lane back. Negative control: while the client lives, every
/// answer the parked wait's own check gave says it is there.
#[test]
fn a_wait_lane_is_given_back_when_its_caller_hangs_up() {
    let service = Arc::new(ParkingService::default());
    let lanes = Lanes::new(LaneLimits {
        rpc: 1,
        wait: 1,
        inline: 0,
        open: 8,
        subscriptions: 1,
    });
    assert_eq!(lanes.start().rpc, 1, "the request lane starts");
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

/// A main-thread request whose caller hung up is refused before anything is posted, so a
/// stalled main thread does not pile up requests whose clients gave up. Negative controls: a
/// connected caller, and a half-closed one that still reads its reply, are not refused.
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
    // The refusal sees the hangup once its lane does ([`until_seen`]).
    until_seen(
        "a closed caller's request to be refused before it is posted",
        || main_thread_refusal() == hung_up,
    );
}
