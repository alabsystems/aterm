// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The act barrier (`crate::act_barrier`) across the real gather and parse pipeline, on a real
//! PTY: an act is read against the parser only once it holds what the program wrote before it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::{DeferredReaderGate, attach_reader_inner, park_reader};
use crate::act_barrier::{PATIENCE, Passage};
use crate::{App, Session, Wake, WindowId};

/// OSC 7501 reports as codex-alab writes them.
const IDLE: &[u8] = b"\x1b]7501;state=idle:app=codex\x1b\\";
const WORKING: &[u8] = b"\x1b]7501;state=working:app=codex\x1b\\";

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let seen = aterm_spec::wait::within(PATIENCE, Duration::ZERO, || done().then_some(()));
    assert!(seen.is_some(), "timed out waiting for {what}");
}

fn program_writes(slave: i32, bytes: &[u8]) {
    // SAFETY: a write of a live buffer to the test-owned slave.
    let wrote = unsafe { libc::write(slave, bytes.as_ptr().cast(), bytes.len()) };
    assert_eq!(
        usize::try_from(wrote).ok(),
        Some(bytes.len()),
        "the program wrote"
    );
}

fn turns(session: &Session) -> u64 {
    crate::term_lock(&session.term).program_state().turns
}

/// A session on a real PTY with its reader attached behind `gate`.
fn reading(app: &App, id: u64, gate: Option<DeferredReaderGate>) -> (Session, i32) {
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    let mut session = crate::stub_session(id);
    session.master = master;
    let mut factory = app.session_factory.clone();
    factory.byte_arm = false;
    attach_reader_inner(
        &mut session,
        WindowId(0),
        Arc::new(|_: Wake| true),
        &factory,
        gate,
    )
    .expect("attach the real reader");
    (session, slave)
}

fn close(mut session: Session, slave: i32) {
    let _ = park_reader(&mut session, Instant::now() + PATIENCE);
    let master = std::mem::replace(&mut session.master, -1);
    aterm_pty::close_fd(slave);
    aterm_pty::close_fd(master);
}

#[test]
fn an_act_barrier_stands_until_the_parser_holds_what_the_program_wrote_before_it() {
    let app = App::headless_for_test();
    let gate = DeferredReaderGate::closed();
    let (session, slave) = reading(&app, 95, Some(gate.clone()));
    program_writes(slave, WORKING);
    program_writes(slave, b"\n");
    assert!(aterm_pty::output_queue_len(session.master).is_some_and(|n| n > 0));
    let answer = session
        .ctx
        .reset_lane
        .ask()
        .expect("a live reader takes a barrier");

    session.last_output_ns.store(0, Ordering::Release);
    let stalled = session.term.lock().expect("terminal lock");
    gate.release();
    wait_for("the parse stage to take up what queued", || {
        session.last_output_ns.load(Ordering::Acquire) != 0
    });
    assert_eq!(aterm_pty::output_queue_len(session.master), Some(0));
    assert!(
        answer.try_recv().is_err(),
        "read but not yet parsed: the barrier stands"
    );
    assert_eq!(
        stalled.program_state().turns,
        0,
        "CONTROL: what an act read without the barrier would count"
    );
    drop(stalled);

    assert!(
        answer.recv_timeout(PATIENCE).is_ok(),
        "the barrier passes once the parser holds the bytes"
    );
    assert_eq!(turns(&session), 1, "the program's turn was parsed first");
    close(session, slave);
}

#[test]
fn a_barrier_with_nothing_queued_passes_and_the_reader_reads_on() {
    let app = App::headless_for_test();
    let (session, slave) = reading(&app, 96, None);
    program_writes(slave, IDLE);
    wait_for("the report to be parsed", || {
        crate::term_lock(&session.term).program_state().seq > 0
    });
    for _ in 0..2 {
        assert_eq!(session.ctx.reset_lane.pass(PATIENCE), Passage::Passed);
    }
    program_writes(slave, WORKING);
    wait_for("a write after the barriers to be parsed", || {
        turns(&session) == 1
    });
    close(session, slave);
}

#[test]
fn a_barrier_asked_of_a_reader_that_stops_disconnects() {
    let app = App::headless_for_test();
    let gate = DeferredReaderGate::closed();
    let (mut session, slave) = reading(&app, 97, Some(gate));
    let answer = session
        .ctx
        .reset_lane
        .ask()
        .expect("a live reader takes a barrier");
    session.reader_stop.store(true, Ordering::Release);
    aterm_pty::wake(session.wake_wr);
    assert_eq!(
        answer.recv_timeout(PATIENCE),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected),
        "a stopped reader answers no barrier, and hangs none"
    );
    assert_eq!(
        session.ctx.reset_lane.pass(PATIENCE),
        Passage::Unbarriered,
        "with no reader there is nothing to wait for"
    );
    session.reader_join = None;
    let master = std::mem::replace(&mut session.master, -1);
    aterm_pty::close_fd(slave);
    aterm_pty::close_fd(master);
}

#[test]
fn a_late_barrier_lets_the_sessions_acts_through_until_the_parser_answers_one() {
    let app = App::headless_for_test();
    let gate = DeferredReaderGate::closed();
    let (session, slave) = reading(&app, 99, Some(gate.clone()));
    let lane = &session.ctx.reset_lane;
    assert_eq!(
        lane.pass(Duration::ZERO),
        Passage::Late,
        "a reader behind its gate answers nothing at once"
    );
    assert!(lane.behind_for_test());
    assert_eq!(
        lane.pass(PATIENCE),
        Passage::Behind,
        "the next act asks no barrier, and waits for none"
    );
    gate.release();
    wait_for("the parser to answer the late barrier", || {
        !lane.behind_for_test()
    });
    assert_eq!(
        lane.pass(PATIENCE),
        Passage::Passed,
        "caught up, the next act waits for its barrier again"
    );
    close(session, slave);
}

/// codex-alab's landing turn: its `working` written to the PTY before its `landed@v1` and not yet
/// read when the act arrives. The landing is marked with that turn counted.
#[test]
fn a_landing_is_marked_with_the_turn_its_program_wrote_before_it() {
    let app = App::headless_for_test();
    let gate = DeferredReaderGate::closed();
    let (session, slave) = reading(&app, 98, Some(gate.clone()));
    {
        let mut tl = session.ctx.timeline.lock().unwrap();
        tl.note_foreground_group(4242);
        tl.set_program(4242, Some("codex".into()));
    }
    session
        .ctx
        .meta
        .lock()
        .unwrap()
        .set("autopilot", Some("land".into()));
    crate::session_timeline::claim_supervisor_as(
        &session.ctx,
        &format!("codex-alab@{}", std::process::id()),
        Some(1),
        None,
        Some(crate::self_claim::stand_in::running()),
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .expect("a proof takes a free session");
    program_writes(slave, WORKING);

    let acted = Arc::new(AtomicBool::new(false));
    let releaser = {
        let (lane, gate, acted) = (
            Arc::clone(&session.ctx.reset_lane),
            gate.clone(),
            Arc::clone(&acted),
        );
        std::thread::spawn(move || {
            wait_for(
                "the act to ask its barrier, or to be read without one",
                || lane.asked_for_test() > 0 || acted.load(Ordering::Acquire),
            );
            gate.release();
        })
    };
    let (reply, _wakes) = crate::control::meta_wakes_for_test(
        &app.store,
        &session.term,
        98,
        &session.ctx,
        "set harness_act landed@v1",
        Some(1),
    );
    acted.store(true, Ordering::Release);
    releaser.join().expect("the releaser");
    assert_eq!(reply, "OK\n");
    let mark = session.ctx.timeline.lock().unwrap().landing_mark();
    assert_eq!(
        mark.map(|m| m.program_turns),
        Some(1),
        "the landing's own turn, written before it, is counted in its mark"
    );
    close(session, slave);
}
