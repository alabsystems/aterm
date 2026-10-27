// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The session log's consumer half, host side: the `status` fields.

use aterm_slog::{Actor, Conf, Fields, Key, Kind, Mark, Src};

use super::tests::{WALL0, opened, stamp, with_claude};
use crate::App;

#[test]
fn the_status_fields_name_the_open_path_and_the_observed_need() {
    let (tl, hand) = opened();
    assert_eq!(
        tl.lock().unwrap().slog().status_fields(WALL0),
        "log_seq=1 log_ep=1 log_base=1 run=- turn=- step=- need=- need_since_ms=-"
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
            "log_seq={head} log_ep=1 log_base=1 run=r1 turn=t1 step=p1 need=approval:bash need_since_ms=500"
        )
    );
}

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

use super::InputVerb;

fn input_marks(tl: &std::sync::Mutex<crate::session_timeline::SessionTimeline>) -> Vec<String> {
    super::tests::lines_of(tl)
        .into_iter()
        .filter(|l| l.contains(" mark - input "))
        .collect()
}

#[test]
fn ctl_input_marks_one_burst_per_actor() {
    let (tl, hand) = opened();
    let edge = Actor::Sid(aterm_slog::ShortId::new("s-0123456789abcdef0123").expect("sid"));
    let send = |by: Actor| {
        let mut g = tl.lock().unwrap();
        let at = g.slog().now();
        g.slog_mut()
            .ctl_input(by, InputVerb::Send, None, super::InputAct::default(), at);
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

#[test]
fn input_marks_from_many_authors_are_bounded() {
    let (tl, hand) = opened();
    for round in 0..4 {
        for i in 0..64u32 {
            let by = Actor::Sid(aterm_slog::ShortId::new(&format!("s-{i:020x}")).expect("sid"));
            let mut g = tl.lock().unwrap();
            let at = g.slog().now();
            g.slog_mut()
                .ctl_input(by, InputVerb::Key, None, super::InputAct::default(), at);
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

#[test]
fn a_flood_of_edges_never_silences_a_trusted_actors_input() {
    let (tl, hand) = opened();
    let edge = |i: u32| Actor::Sid(aterm_slog::ShortId::new(&format!("s-{i:020x}")).expect("sid"));
    let send = |by: Actor| {
        let mut g = tl.lock().unwrap();
        let at = g.slog().now();
        g.slog_mut()
            .ctl_input(by, InputVerb::Send, None, super::InputAct::default(), at);
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

#[test]
fn a_flood_of_acted_keys_keeps_the_sessions_records() {
    use crate::task_record::{TaskMark, TaskState};
    let h = crate::session_store::test_handle(71);
    let hand = super::tests::hand_clock(&h.ctx.timeline);
    let store = crate::session_store::new_store();
    store.write().unwrap().register(h.clone());
    let (reply, _) = super::cmd_mark(&h.ctx, Actor::Harness, true, "- need word=review");
    assert!(reply.starts_with("OK"), "{reply}");
    let _ = h.ctx.timeline.lock().unwrap().mark_task(
        &TaskMark::new(TaskState::Working),
        false,
        std::time::Instant::now(),
        1,
    );
    let deny = super::InputAct {
        act: Some(aterm_slog::ActWord::Deny),
        re: None,
    };
    let key = || {
        let mut g = h.ctx.timeline.lock().unwrap();
        let at = g.slog().now();
        g.slog_mut()
            .ctl_input(Actor::Ctl, InputVerb::Key, None, deny, at);
    };
    for _ in 0..3000 {
        key();
    }
    let acted = || -> Vec<String> {
        input_marks(&h.ctx.timeline)
            .into_iter()
            .filter(|l| l.contains(" act=deny "))
            .collect()
    };
    let burst = usize::try_from(aterm_slog::MARK_BURST).expect("small");
    assert_eq!(
        acted().len(),
        1 + burst,
        "the burst's mark and twenty tokens"
    );
    hand.advance(1_000);
    key();
    let marks = acted();
    assert_eq!(marks.len(), 2 + burst, "a token a second");
    let dropped = 3000 - 1 - burst;
    assert!(
        marks
            .last()
            .is_some_and(|l| l.contains(&format!(" dropped={dropped} "))),
        "the unmarked keys counted: {:?}",
        marks.last()
    );
    let lines = super::tests::lines_of(&h.ctx.timeline);
    for kind in [" mark - need ", " mark - task "] {
        assert!(
            lines.iter().any(|l| l.contains(kind)),
            "the flood evicted{kind}"
        );
    }
}

#[test]
fn an_edges_dropped_count_rides_only_its_own_mark() {
    let (tl, hand) = opened();
    let edge = |i: u32| Actor::Sid(aterm_slog::ShortId::new(&format!("s-{i:020x}")).expect("sid"));
    let deny = super::InputAct {
        act: Some(aterm_slog::ActWord::Deny),
        re: None,
    };
    let key = |by: Actor| {
        let mut g = tl.lock().unwrap();
        let at = g.slog().now();
        g.slog_mut().ctl_input(by, InputVerb::Key, None, deny, at);
    };
    let last_by = |by: &str| {
        input_marks(&tl)
            .into_iter()
            .rfind(|l| l.contains(&format!(" by={by} ")))
            .expect("a mark by the author")
    };
    for _ in 0..100 {
        key(edge(1));
    }
    let burst = usize::try_from(aterm_slog::MARK_BURST).expect("small");
    assert_eq!(input_marks(&tl).len(), 1 + burst, "A's burst and tokens");
    key(edge(2));
    let b = last_by("s-00000000000000000002");
    assert!(!b.contains(" dropped="), "B dropped nothing: {b}");
    hand.advance(1_000);
    key(edge(1));
    let a = last_by("s-00000000000000000001");
    let dropped = 100 - 1 - burst;
    assert!(a.contains(&format!(" dropped={dropped} ")), "A's own: {a}");
}

#[test]
fn overflowed_edges_each_say_their_own_dropped_count() {
    let (tl, hand) = opened();
    let edge = |i: u32| Actor::Sid(aterm_slog::ShortId::new(&format!("s-{i:020x}")).expect("sid"));
    let deny = super::InputAct {
        act: Some(aterm_slog::ActWord::Deny),
        re: None,
    };
    let key = |by: Actor| {
        let mut g = tl.lock().unwrap();
        let at = g.slog().now();
        g.slog_mut().ctl_input(by, InputVerb::Key, None, deny, at);
    };
    let by_word = |by: Actor| format!(" by={} ", by.word());
    let marks_by = |by: Actor| -> Vec<String> {
        let w = by_word(by);
        input_marks(&tl)
            .into_iter()
            .filter(|l| l.contains(&w))
            .collect()
    };
    let burst = usize::try_from(aterm_slog::MARK_BURST).expect("small");
    let slots = u32::try_from(aterm_slog::MARK_ACTORS).expect("small");
    for i in 0..slots {
        for _ in 0..=burst {
            key(edge(i));
        }
    }
    let (a, b) = (edge(100), edge(101));
    let (a_keys, b_keys) = (50, 12);
    for _ in 0..a_keys {
        key(a);
    }
    for _ in 0..b_keys {
        key(b);
    }
    let a_short = a_keys - marks_by(a).len();
    let b_short = b_keys - marks_by(b).len();
    assert!(
        a_short > 0 && b_short > 0,
        "both were short: {a_short} {b_short}"
    );
    assert!(
        marks_by(a).len() <= 1 + burst,
        "A had the overflow's tokens"
    );
    hand.advance(1_000);
    key(a);
    let a_mark = marks_by(a).pop().expect("A's mark");
    assert!(
        a_mark.contains(&format!(" dropped={a_short} by=")),
        "A says its own: {a_mark}"
    );
    hand.advance(1_000);
    key(b);
    let b_mark = marks_by(b).pop().expect("B's mark");
    assert!(
        b_mark.contains(&format!(" dropped={b_short} by=")),
        "B says its own: {b_mark}"
    );

    let past: Vec<Actor> = (0..=u32::try_from(aterm_slog::MARK_OWED_AUTHORS).expect("small"))
        .map(|i| edge(2000 + i))
        .collect();
    for &by in &past {
        key(by);
        key(by);
    }
    let first_short = 2 - marks_by(past[0]).len();
    let second_short = 2 - marks_by(past[1]).len();
    assert!(first_short > 0 && second_short > 0);
    hand.advance(1_000);
    key(past[0]);
    let first = marks_by(past[0]).pop().expect("the first's mark");
    assert!(
        first.contains(&format!(" dropped={first_short} pool=shared by=")),
        "the spilled count is the pool's: {first}"
    );
    hand.advance(1_000);
    key(past[1]);
    let second = marks_by(past[1]).pop().expect("the second's mark");
    assert!(
        second.contains(&format!(" dropped={second_short} by=")),
        "its own, plain: {second}"
    );
}

#[test]
fn a_turn_is_the_last_actors_within_five_seconds() {
    let turn_by = |gap: u64, person_after: bool| -> String {
        let (tl, hand) = opened();
        with_claude(&tl);
        {
            let mut g = tl.lock().unwrap();
            let at = g.slog().now();
            g.slog_mut().ctl_input(
                Actor::Harness,
                InputVerb::Send,
                None,
                super::InputAct::default(),
                at,
            );
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
            super::InputAct::default(),
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

fn watching(app: &App, session: u64) -> crate::session_watch::Subscription {
    let sub = crate::session_watch::Subscription::single(
        &app.pool.get(session).expect("a live session").ctx.watchers,
    );
    let _ = sub.wait(std::time::Duration::ZERO);
    sub
}

fn woke_and_took(
    sub: &crate::session_watch::Subscription,
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
    let sub = watching(&app, session);
    let _ = timeline.lock().unwrap().slog_take_wake();
    crate::term_lock(&term).process(
        b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\false\r\n\x1b]133;C\x1b\\\
          \x1b]133;D;1\x1b\\\x1b]133;A\x1b\\\x1b]133;B\x1b\\",
    );
    app.observe_session_statuses(t0 + Duration::from_secs(1));
    woke_and_took(&sub, &timeline, "the sweep");
}

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
    let sub = watching(&app, 0);
    hand.set(aterm_slog::SETTLE_MS);
    app.session_status.slog_wakes.note(0, Some(0));
    app.tick_session_logs();
    woke_and_took(&sub, &timeline, "the settle tick");
}

#[test]
fn a_persons_burst_wakes_the_logs_watchers() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let session = app.front_terminal(wid).expect("front").session;
    let timeline = app.pool.get(session).expect("pooled").ctx.timeline.clone();
    let _ = timeline.lock().unwrap().slog_take_wake();
    let sub = watching(&app, session);
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
        let sub = watching(&app, session);
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
/// the overlap is theirs to read, and they are woken.
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
    let sub = watching(&app, 0);
    app.lift_session_log_fences();
    woke_and_took(&sub, &timeline, "a lifted fence");
}

#[test]
fn control_input_owes_a_wake_only_when_it_wrote() {
    let (tl, _hand) = opened();
    let _ = tl.lock().unwrap().slog_take_wake();
    let input = |tl: &std::sync::Mutex<crate::session_timeline::SessionTimeline>| {
        let mut g = tl.lock().unwrap();
        let at = g.slog().now();
        g.slog_mut().ctl_input(
            Actor::Ctl,
            InputVerb::Send,
            None,
            super::InputAct::default(),
            at,
        );
        g.slog_take_wake()
    };
    assert!(input(&tl), "the burst's mark");
    assert!(
        !input(&tl),
        "inside the burst: nothing written, nothing owed"
    );
}

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
        "-",
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

#[test]
fn an_await_log_list_answers_on_any_member() {
    use super::{AWAIT_LOG_USAGE, LogAwait, LogAwaitStep};
    let list = LogAwait::parse(&["close=turn,kind=note"]).expect("a list");
    assert_eq!(list.what.members().len(), 2);
    let first_only = LogAwait::parse(&["close=turn"]).expect("one").what;
    assert_eq!(list.what.members()[0], first_only.members()[0]);
    let (tl, _hand) = opened();
    let mut g = tl.lock().unwrap();
    let cursor = aterm_slog::Cursor::new(g.slog().log().ep(), g.slog().log().reader_last_seq());
    let now = g.slog().now();
    g.slog_mut()
        .log_mut_for_journal()
        .mark(
            Mark::new(Kind::Note, Src::Mark, Conf::Claim)
                .at(aterm_slog::Node::SESSION)
                .fields(Fields::new().text(Key::Text, "second").expect("text")),
            now,
        )
        .expect("a note");
    match g.slog().await_check(list.what, cursor) {
        LogAwaitStep::Answer(reply) => assert!(reply.contains(" mark - note "), "{reply}"),
        LogAwaitStep::Wait(_) => panic!("the second member's record answers the list"),
    }
    assert!(
        matches!(
            g.slog().await_check(first_only, cursor),
            LogAwaitStep::Wait(_)
        ),
        "NEGATIVE CONTROL: the first member alone does not answer a note"
    );
    for bad in [
        &["need,need"][..],
        &["need,close=turn,open=run,kind=note,kind=bell"],
        &["need", "close=turn"],
        &["need,bogus"],
        &["need,"],
        &[",need"],
        &["close=turn,kind=nope"],
        &["since=1", "ep=1"],
        &["need", "since=1", "since=2", "ep=1"],
    ] {
        assert_eq!(LogAwait::parse(bad), Err(AWAIT_LOG_USAGE), "{bad:?}");
    }
    let four =
        LogAwait::parse(&["need,close=turn,open=run,kind=note", "since=3", "ep=1"]).expect("four");
    assert_eq!(four.what.members().len(), 4);
    assert!(four.what.has_need());
}
