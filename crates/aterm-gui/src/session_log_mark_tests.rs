// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The session log's consumer half, host side (phase 3A): the `status`
//! fields.

use aterm_slog::{Actor, Conf, Fields, Key, Kind, Mark, Src};

use super::tests::{WALL0, opened, stamp, with_claude};

/// `log_seq= log_ep= run= turn= step= need= need_since_ms=`: a fresh session
/// names nothing open; an agent's turn with a Bash box names the run, the
/// turn, the step and the observed need with its age.
#[test]
fn the_status_fields_name_the_open_path_and_the_observed_need() {
    let (tl, hand) = opened();
    assert_eq!(
        tl.lock().unwrap().slog().status_fields(WALL0),
        "log_seq=1 log_ep=1 run=- turn=- step=- need=- need_since_ms=-"
    );
    with_claude(&tl);
    let mut g = tl.lock().unwrap();
    hand.set(1_000);
    g.publish_agent(
        "busy",
        None,
        None,
        Some(aterm_phase::Program::Claude),
        stamp(5),
    );
    hand.set(2_000);
    g.publish_agent(
        "prompt",
        Some("bash:not-read-only".into()),
        None,
        Some(aterm_phase::Program::Claude),
        stamp(6),
    );
    let head = g.slog().log().last_seq();
    assert_eq!(
        g.slog().status_fields(WALL0 + 2_500),
        format!(
            "log_seq={head} log_ep=1 run=r1 turn=t1 step=p1 need=approval:bash need_since_ms=500"
        )
    );
}

/// NEGATIVE: a need only a claim raised is not `status`'s (it names no
/// author); `brief` says it as `claim need`.
#[test]
fn a_claimed_need_is_never_the_status_need() {
    let (tl, _hand) = opened();
    let mut g = tl.lock().unwrap();
    let now = g.slog().now();
    let log = g.slog_mut().log_mut_for_journal();
    log.mark(
        Mark::new(Kind::Need, Src::Mark, Conf::Claim)
            .by(Actor::Ctl)
            .fields(Fields::new().word(Key::Word, "help").expect("help")),
        now,
    )
    .expect("a claim");
    assert!(log.state().top_need().is_some(), "the fold holds it");
    assert!(
        g.slog()
            .status_fields(WALL0)
            .ends_with(" need=- need_since_ms=-"),
        "a claim's need is brief's, not status's"
    );
}

/// While an update's carry fences the readers, `log_seq=` is the fence: a
/// `since=` taken from `status` is one every read accepts.
#[test]
fn a_fenced_log_seq_is_the_fence() {
    let (tl, _hand) = opened();
    let mut g = tl.lock().unwrap();
    let now = g.slog().now();
    let log = g.slog_mut().log_mut_for_journal();
    log.fence_reads();
    let fence = log.read_fence().expect("fenced");
    for _ in 0..3 {
        log.mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), now)
            .expect("a mark");
    }
    assert!(log.last_seq() > fence);
    assert!(
        g.slog()
            .status_fields(WALL0)
            .starts_with(&format!("log_seq={fence} log_ep=1 ")),
        "{}",
        g.slog().status_fields(WALL0)
    );
}

// ---- control input -------------------------------------------------------

use super::InputVerb;

fn input_marks(tl: &std::sync::Mutex<crate::session_timeline::SessionTimeline>) -> Vec<String> {
    super::tests::lines_of(tl)
        .into_iter()
        .filter(|l| l.contains(" mark - input "))
        .collect()
}

/// `mark - input verb= by=` once per author's burst: two sends within 30 s
/// write one, a third 30 s on writes another, and another author's input
/// marks its own burst. The bytes are never recorded (the call has none).
#[test]
fn ctl_input_marks_one_burst_per_actor() {
    let (tl, hand) = opened();
    let edge = Actor::Sid(aterm_slog::ShortId::new("s-0123456789abcdef0123").expect("sid"));
    let send = |by: Actor| {
        let mut g = tl.lock().unwrap();
        let at = g.slog().now();
        g.slog_mut().ctl_input(by, InputVerb::Send, None, at);
    };
    send(Actor::Harness);
    hand.advance(10_000);
    send(Actor::Harness);
    assert_eq!(input_marks(&tl).len(), 1, "one burst");
    send(edge);
    assert_eq!(input_marks(&tl).len(), 2, "another author's own burst");
    hand.advance(30_000);
    send(Actor::Harness);
    let marks = input_marks(&tl);
    assert_eq!(marks.len(), 3, "{marks:?}");
    assert!(
        marks[0].contains(" verb=send by=harness src=ctl conf=strong "),
        "{}",
        marks[0]
    );
    assert!(
        marks[1].contains(" by=s-0123456789abcdef0123 "),
        "{}",
        marks[1]
    );
}

/// R11: the burst table holds eight authors; every author past them shares
/// one slot, so sixty-four authors rotating inside one window write at most
/// nine marks, however fast they rotate.
#[test]
fn input_marks_from_many_authors_are_bounded() {
    let (tl, hand) = opened();
    for round in 0..4 {
        for i in 0..64u32 {
            let by = Actor::Sid(aterm_slog::ShortId::new(&format!("s-{i:020x}")).expect("sid"));
            let mut g = tl.lock().unwrap();
            let at = g.slog().now();
            g.slog_mut().ctl_input(by, InputVerb::Key, None, at);
            drop(g);
            hand.advance(10);
        }
        let _ = round;
    }
    assert_eq!(
        input_marks(&tl).len(),
        9,
        "eight named authors and the shared slot"
    );
}

/// R11 for the input marks: the trusted actors keep burst clocks of their
/// own. Nine edges typing every 29 s hold the eight slots and the shared
/// one, and still the harness's input, then `ctl`'s and the bridge's, each
/// write their `mark - input` (the harness's again a burst later).
/// NEGATIVE CONTROL: a tenth edge, in the same flood, writes none.
#[test]
fn a_flood_of_edges_never_silences_a_trusted_actors_input() {
    let (tl, hand) = opened();
    let edge = |i: u32| Actor::Sid(aterm_slog::ShortId::new(&format!("s-{i:020x}")).expect("sid"));
    let send = |by: Actor| {
        let mut g = tl.lock().unwrap();
        let at = g.slog().now();
        g.slog_mut().ctl_input(by, InputVerb::Send, None, at);
    };
    let by_of = |by: &str| {
        input_marks(&tl)
            .iter()
            .filter(|l| l.contains(&format!(" by={by} ")))
            .count()
    };
    for round in 0..3 {
        for i in 0..9 {
            send(edge(i));
        }
        match round {
            0 => send(Actor::Harness),
            1 => {
                send(Actor::Ctl);
                send(Actor::Bridge);
                send(edge(9));
            }
            _ => send(Actor::Harness),
        }
        hand.advance(29_000);
    }
    assert_eq!(by_of("harness"), 2, "{:?}", input_marks(&tl));
    assert_eq!(by_of("ctl"), 1, "{:?}", input_marks(&tl));
    assert_eq!(by_of("bridge"), 1, "{:?}", input_marks(&tl));
    assert_eq!(
        by_of("s-00000000000000000009"),
        0,
        "NEGATIVE CONTROL: the shared slot was held: {:?}",
        input_marks(&tl)
    );
}

/// A turn is the last author's within five seconds: the harness's `send`
/// then `busy` 4 s on opens `by=harness`; NEGATIVE at 6 s the turn is
/// nobody's; a person's keys after the harness's input take the turn.
#[test]
fn a_turn_is_the_last_actors_within_five_seconds() {
    let turn_by = |gap: u64, person_after: bool| -> String {
        let (tl, hand) = opened();
        with_claude(&tl);
        {
            let mut g = tl.lock().unwrap();
            let at = g.slog().now();
            g.slog_mut()
                .ctl_input(Actor::Harness, InputVerb::Send, None, at);
            if person_after {
                hand.advance(1_000);
                let at = g.slog().now();
                g.slog_mut().human_input(true, at);
            }
            hand.advance(gap);
            g.publish_agent(
                "busy",
                None,
                None,
                Some(aterm_phase::Program::Claude),
                stamp(5),
            );
        }
        super::tests::lines_of(&tl)
            .into_iter()
            .find(|l| l.contains(" open t1 turn "))
            .expect("a turn opened")
    };
    assert!(turn_by(4_000, false).contains(" by=harness "));
    assert!(
        !turn_by(6_000, false).contains(" by="),
        "past the window: nobody's"
    );
    assert!(turn_by(3_000, true).contains(" by=human "));
}

/// A caused send names its turn: `cause=` and `cause_k=` on the turn's open.
#[test]
fn a_caused_send_names_its_turn() {
    let (tl, hand) = opened();
    with_claude(&tl);
    {
        let mut g = tl.lock().unwrap();
        let at = g.slog().now();
        g.slog_mut().ctl_input(
            Actor::Ctl,
            InputVerb::Send,
            aterm_slog::ShortId::new("@88231"),
            at,
        );
        hand.advance(1_000);
        g.publish_agent(
            "busy",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            stamp(5),
        );
    }
    let lines = super::tests::lines_of(&tl);
    let open = lines
        .iter()
        .find(|l| l.contains(" open t1 turn "))
        .expect("a turn");
    assert!(
        open.contains(" by=ctl ") && open.contains("cause=@88231") && open.contains("cause_k=mail"),
        "{open}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains(" mark - input verb=send cause=@88231 cause_k=mail by=ctl ")),
        "{lines:?}"
    );
}

// ---- the wake (design §2.2; ruling R9) -------------------------------------

/// A registration on `session`'s subscriber slot, with any notify already
/// pending drained.
fn watching(
    subscribers: &crate::subscribe::Subscribers,
    session: u64,
) -> crate::subscribe::Subscription {
    let sub = crate::subscribe::SubscriberSet::register(subscribers, &[session]);
    let _ = sub.wait(std::time::Duration::ZERO);
    sub
}

/// The site woke `session`'s watchers (a one-minute hang detector: a site that
/// forgot its wake leaves the watcher parked) and took the wake it owed.
fn woke_and_took(
    sub: &crate::subscribe::Subscription,
    timeline: &std::sync::Mutex<crate::session_timeline::SessionTimeline>,
    site: &str,
) {
    assert!(
        sub.wait(aterm_spec::wait::MUST_HAPPEN),
        "{site}: the watchers were never woken"
    );
    assert!(
        !timeline.lock().unwrap().slog_take_wake(),
        "{site}: the site left its wake owed"
    );
}

/// The status sweep's pass (a failing command's run, closed at its D) wakes
/// the session's watchers.
#[test]
fn the_sweep_wakes_the_logs_watchers() {
    use std::time::{Duration, Instant};
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let front = app.front_terminal(wid).expect("a front terminal");
    let (session, term) = (front.session, std::sync::Arc::clone(&front.term));
    let timeline = std::sync::Arc::clone(&app.pool.get(session).expect("pooled").ctx.timeline);
    let t0 = Instant::now();
    app.observe_session_statuses(t0);
    let sub = watching(&app.subscribers, session);
    let _ = timeline.lock().unwrap().slog_take_wake();
    crate::term_lock(&term).process(
        b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\false\r\n\x1b]133;C\x1b\\\
          \x1b]133;D;1\x1b\\\x1b]133;A\x1b\\\x1b]133;B\x1b\\",
    );
    app.observe_session_statuses(t0 + Duration::from_secs(1));
    woke_and_took(&sub, &timeline, "the sweep");
}

/// The settle tick closes a turn with no output and nothing else to wake a
/// reader: it wakes them.
#[test]
fn the_settle_tick_wakes_the_logs_watchers() {
    let mut app = crate::App::headless_for_test();
    let timeline = app.pool.get(0).expect("session").ctx.timeline.clone();
    let now = crate::turn_ledger::now_ms();
    let hand = super::tests::hand_clock_from(&timeline, now);
    {
        let mut g = timeline.lock().unwrap();
        g.note_root_group(100);
        g.note_foreground_group(300);
        g.set_program(300, Some("claude".into()));
        g.publish_agent(
            "busy",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            stamp(1),
        );
        g.publish_agent(
            "idle",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            stamp(2),
        );
        let _ = g.slog_take_wake();
    }
    let sub = watching(&app.subscribers, 0);
    hand.set(aterm_slog::SETTLE_MS);
    app.session_status.slog_wakes.note(0, Some(0));
    app.tick_session_logs();
    woke_and_took(&sub, &timeline, "the settle tick");
}

/// A person's burst (`mark - human`) wakes the watchers.
#[test]
fn a_persons_burst_wakes_the_logs_watchers() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let session = app.front_terminal(wid).expect("front").session;
    let timeline = app.pool.get(session).expect("pooled").ctx.timeline.clone();
    let _ = timeline.lock().unwrap().slog_take_wake();
    let sub = watching(&app.subscribers, session);
    let _ = app.input(
        wid,
        crate::input::InputEvent::Text("x".into()),
        crate::input::Source::Human,
    );
    assert!(
        super::tests::lines_of(&timeline)
            .iter()
            .any(|l| l.contains(" mark - human by=human ")),
        "PRECONDITION: the burst's mark"
    );
    woke_and_took(&sub, &timeline, "a person's burst");
}

/// A failure the person comes to look at (`note_session_seen`) and one an
/// update carried as seen (`note_carried_exit`, ruling R9) wake the watchers.
#[test]
fn a_failure_seen_wakes_the_logs_watchers() {
    use std::time::{Duration, Instant};
    for carried in [false, true] {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        for ws in app.windows.values_mut() {
            ws.os_window_for_test = true;
            ws.focused = false;
        }
        let front = app.front_terminal(wid).expect("a front terminal");
        let (session, term) = (front.session, std::sync::Arc::clone(&front.term));
        let timeline = app.pool.get(session).expect("pooled").ctx.timeline.clone();
        crate::term_lock(&term).process(
            b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\make\r\n\x1b]133;C\x1b\\\
              \x1b]133;D;2\x1b\\\x1b]133;A\x1b\\\x1b]133;B\x1b\\",
        );
        let t0 = Instant::now();
        app.observe_session_statuses(t0);
        app.observe_session_statuses(t0 + Duration::from_secs(1));
        let _ = timeline.lock().unwrap().slog_take_wake();
        let sub = watching(&app.subscribers, session);
        if carried {
            let block = crate::session_status::completed_block(&crate::term_lock(&term));
            {
                let pooled = &mut app.pool.sessions.get_mut(&session).expect("pane").session;
                let _ = pooled.child_exit.set(aterm_pty::ChildExit::Code(2));
                pooled
                    .child_reaped
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            app.note_carried_exit(session, true, block);
            woke_and_took(&sub, &timeline, "a carried exit seen");
        } else {
            for ws in app.windows.values_mut() {
                ws.focused = true;
            }
            app.note_session_seen(wid, session);
            woke_and_took(&sub, &timeline, "note_session_seen");
        }
    }
}

/// An update that stands down lifts its readers' fence: what was written in
/// the overlap is theirs to read, and they are woken (ruling R9).
#[cfg(unix)]
#[test]
fn a_lifted_fence_wakes_the_logs_watchers() {
    let mut app = crate::App::headless_for_test();
    let timeline = app.pool.get(0).expect("session").ctx.timeline.clone();
    {
        let mut g = timeline.lock().unwrap();
        let now = g.slog().now();
        let log = g.slog_mut().log_mut_for_journal();
        log.fence_reads();
        log.mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), now)
            .expect("an overlap record");
        let _ = g.slog_take_wake();
    }
    let sub = watching(&app.subscribers, 0);
    app.lift_session_log_fences();
    woke_and_took(&sub, &timeline, "a lifted fence");
}

/// Control input owes its wake exactly when it wrote (`mark - input` once a
/// burst): the hook wakes then, and a second input in the burst owes none.
#[test]
fn control_input_owes_a_wake_only_when_it_wrote() {
    let (tl, _hand) = opened();
    let _ = tl.lock().unwrap().slog_take_wake();
    let input = |tl: &std::sync::Mutex<crate::session_timeline::SessionTimeline>| {
        let mut g = tl.lock().unwrap();
        let at = g.slog().now();
        g.slog_mut()
            .ctl_input(Actor::Ctl, InputVerb::Send, None, at);
        g.slog_take_wake()
    };
    assert!(input(&tl), "the burst's mark");
    assert!(
        !input(&tl),
        "inside the burst: nothing written, nothing owed"
    );
}

// ---- the look (phase 3A review) --------------------------------------------

/// THE ONE LOOK (`SessionLog::await_look`): an `await log` at an ended
/// session reads the log past its cursor and whether the close is due in one
/// borrow of the log, so in one hold of the timeline's lock, and the close
/// clears the flag in the hold that writes it (`SessionLog::close_session`).
/// Before the close the look is due with nothing yet; after it the look
/// answers the close, never `(Wait, false)`, which is the `ERR exited` a
/// waiter would give over the close it waited for. NEGATIVE CONTROL: an exit
/// handling that ended with no retire (`take_close_due`) looks `(Wait,
/// false)`, and the waiter answers `ERR exited`.
#[test]
fn the_await_look_reads_a_close_with_the_flag_it_cleared() {
    use super::{LogAwait, LogAwaitStep};
    let what = LogAwait::parse(&["close=session"]).expect("parses").what;
    let (tl, _hand) = opened();
    let mut g = tl.lock().unwrap();
    let cursor = aterm_slog::Cursor::new(g.slog().log().ep(), g.slog().log().reader_last_seq());
    g.slog_mut().note_close_due();
    assert!(
        matches!(
            g.slog().await_look(what, cursor),
            (LogAwaitStep::Wait(_), true)
        ),
        "due, and nothing written yet"
    );
    let now = g.slog().now();
    g.slog_mut().close_session(
        aterm_slog::SessionOut::ShellExit,
        Actor::Nobody,
        Some(0),
        now,
    );
    match g.slog().await_look(what, cursor) {
        (LogAwaitStep::Answer(reply), false) => assert!(
            reply.contains(" close ") && reply.contains(" out=shell-exit"),
            "{reply}"
        ),
        (LogAwaitStep::Answer(reply), true) => panic!("the close clears what was due: {reply}"),
        (LogAwaitStep::Wait(_), due) => panic!("a look past the close missed it (due={due})"),
    }
    drop(g);

    let (tl, _hand) = opened();
    let mut g = tl.lock().unwrap();
    let cursor = aterm_slog::Cursor::new(g.slog().log().ep(), g.slog().log().reader_last_seq());
    g.slog_mut().note_close_due();
    assert!(g.slog_mut().take_close_due(), "it was due");
    assert!(
        matches!(
            g.slog().await_look(what, cursor),
            (LogAwaitStep::Wait(_), false)
        ),
        "no close is coming"
    );
}
