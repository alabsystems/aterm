// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Who dismissed a box (session log phase 5, rulings T1 and T2), host side:
//! the admission an input mark takes under the timeline lock before a byte,
//! the acceptance its completion makes after the write, and the person's
//! count the log reconciles against before it reaches the tracker.

use aterm_slog::{ActWord, Actor, Kind, Op, Rec};

use super::tests::{hand_clock, stamp};
use crate::control_input_ack::{InputMark, testing};
use crate::human_input::Gesture;
use crate::session_log::InputVerb;
use crate::session_store::SessionHandle;

const CLAUDE: Option<aterm_phase::Program> = Some(aterm_phase::Program::Claude);

/// `h`'s session with `claude` in front under the root shell, its agent run
/// open and busy (its turn open).
fn claude_busy(h: &SessionHandle) {
    let mut g = h.ctx.timeline.lock().unwrap();
    g.note_root_group(100);
    assert!(g.note_foreground_group(300));
    assert!(g.set_program(300, Some("claude".into())));
    let pgid = g.agent().program_pgid;
    let _ = g.sweep_slog(&crate::session_log::BlockRead::default(), false, 100, pgid);
    g.publish_agent("busy", None, None, CLAUDE, stamp(1));
}

/// The status sweep publishes `word` (`detail`) read from screen `seq`.
fn publish(h: &SessionHandle, word: &'static str, detail: Option<&str>, seq: u64) {
    h.ctx.timeline.lock().unwrap().publish_agent(
        word,
        detail.map(str::to_string),
        None,
        CLAUDE,
        stamp(seq),
    );
}

/// The approval closes `h`'s log holds, oldest first.
fn approval_closes(h: &SessionHandle) -> Vec<Rec> {
    h.ctx
        .timeline
        .lock()
        .unwrap()
        .slog()
        .log()
        .window()
        .filter(|r| r.op == Op::Close && r.k == Kind::Approval)
        .cloned()
        .collect()
}

/// The harness's `key act=approve`, admitted now.
fn harness_approve(h: &SessionHandle) -> InputMark {
    InputMark::at_epoch(
        &h.ctx,
        None,
        h.local_id,
        Actor::Harness,
        InputVerb::Key,
        None,
        false,
    )
    .acting(Some(ActWord::Approve))
}

/// T1's bound: two of the sweep's agent reads at least, so the read that
/// saw the box go is inside it.
#[test]
fn step_act_ms_spans_two_agent_reads() {
    let read = crate::session_status::AGENT_MIN_INTERVAL.as_millis();
    assert!(
        u128::from(aterm_slog::STEP_ACT_MS) >= 2 * read,
        "STEP_ACT_MS {} < 2 × the agent read interval {read} ms",
        aterm_slog::STEP_ACT_MS
    );
}

/// T1 (`ActBindsNextBox`, through the real `InputMark::finish`): an approve
/// admitted under box 1 whose bytes were written after one screen read
/// replaced box 1 with box 2 binds neither: both close `passed` by nobody.
/// POSITIVE CONTROL: admitted and written under the box it dismisses, it
/// closes it `approved` by the harness.
#[cfg(unix)]
#[test]
fn an_act_accepted_after_its_box_was_replaced_binds_neither_box() {
    let (h, _reader) = testing::owned_handle(0);
    let _hand = hand_clock(&h.ctx.timeline);
    let store = crate::session_store::new_store();
    store.write().unwrap().register(h.clone());
    claude_busy(&h);
    publish(&h, "prompt", Some("edit"), 2);
    let mark = harness_approve(&h);
    publish(&h, "prompt", Some("bash:not-read-only"), 3);
    assert!(testing::paste(&h, &mark, "y"), "the bytes were written");
    publish(&h, "busy", None, 4);
    let tale: Vec<_> = approval_closes(&h)
        .iter()
        .map(|r| (r.out(), r.by))
        .collect();
    assert_eq!(
        tale,
        [
            (Some("passed"), Actor::Nobody),
            (Some("passed"), Actor::Nobody)
        ],
        "the late approve bound a box"
    );
    publish(&h, "prompt", Some("edit"), 5);
    let mark = harness_approve(&h);
    assert!(testing::paste(&h, &mark, "y"));
    publish(&h, "busy", None, 6);
    let last = approval_closes(&h).pop().expect("box 3 closed");
    assert_eq!((last.out(), last.by), (Some("approved"), Actor::Harness));
}

/// T2: a person's key that found the timeline held (`note_person`'s
/// `try_lock` failed) still attributes: the sweep's observe of the box the
/// key dismissed delivers it first, so the box the harness had approved
/// closes `by=human out=passed`. One thread, no timing. NEGATIVE CONTROL: a
/// log never bound to the person's count loses the key, and the harness's
/// approve stands.
#[cfg(unix)]
#[test]
fn a_person_key_that_found_the_timeline_held_still_attributes() {
    let close = |bound: bool| {
        let (h, _reader) = testing::owned_handle(0);
        let _hand = hand_clock(&h.ctx.timeline);
        let store = crate::session_store::new_store();
        if bound {
            store.write().unwrap().register(h.clone());
        } else {
            let mut g = h.ctx.timeline.lock().unwrap();
            let now = g.slog().now();
            g.slog_mut().open_session(None, now);
        }
        claude_busy(&h);
        // A burst already running (its stamp ahead of every clock): the key
        // below opens none, so it takes `note_person`'s `try_lock`.
        let _ = h.ctx.human_input.note(u64::MAX / 2, Gesture::Other);
        publish(&h, "prompt", Some("edit"), 2);
        let mark = harness_approve(&h);
        assert!(testing::paste(&h, &mark, "y"));
        {
            let _held = h.ctx.timeline.lock().unwrap();
            assert!(!crate::app_input::note_person(&h.ctx, Gesture::Other));
        }
        publish(&h, "busy", None, 3);
        let c = approval_closes(&h).pop().expect("the box closed");
        (c.out(), c.by)
    };
    assert_eq!(
        close(true),
        (Some("passed"), Actor::Human),
        "the person's key was lost to the hold"
    );
    assert_eq!(close(false), (Some("approved"), Actor::Harness));
}

/// One control input from `actor` with `act`, admitted under an approval
/// box (or, `question`, a question) that is still open when its bytes are
/// written (`open`), or that a screen read closed in between, through the
/// real `InputMark::finish`. The burst from `actor` is already running, so
/// a mark after it is an acted input's own. Returns the step's close
/// (`out=`, `by=`) and the input marks the input wrote, as wire lines
/// without their `seq` and `t=`, `<step>` standing for the step's node.
fn acted_input(
    actor: Actor,
    act: Option<ActWord>,
    question: bool,
    open: bool,
) -> ((Option<&'static str>, Actor), Vec<String>) {
    let (h, _reader) = testing::owned_handle(0);
    let _hand = hand_clock(&h.ctx.timeline);
    let store = crate::session_store::new_store();
    store.write().unwrap().register(h.clone());
    let key = |act| {
        InputMark::at_epoch(&h.ctx, None, h.local_id, actor, InputVerb::Key, None, false)
            .acting(act)
    };
    // The burst's first input, before the agent runs: it binds nothing.
    assert!(testing::paste(&h, &key(None), "x"));
    let before = testing::input_count(&h);
    claude_busy(&h);
    if question {
        publish(&h, "prompt", Some("question"), 2);
    } else {
        publish(&h, "prompt", Some("edit"), 2);
    }
    let mark = key(act);
    if !open {
        publish(&h, "busy", None, 3);
    }
    assert!(testing::paste(&h, &mark, "1"), "the bytes were written");
    publish(&h, "busy", None, 4);
    let g = h.ctx.timeline.lock().unwrap();
    let log = g.slog().log();
    let step = if question {
        Kind::Question
    } else {
        Kind::Approval
    };
    let close = log
        .window()
        .find(|r| r.op == Op::Close && r.k == step)
        .unwrap_or_else(|| {
            let lines: Vec<String> = log
                .window()
                .filter_map(|r| aterm_slog::encode(r, aterm_slog::Tier::Host).ok())
                .collect();
            panic!(
                "{actor:?} {act:?} question={question} open={open}: no {step:?} close\n{}",
                lines.join("\n")
            )
        });
    let node = close.node.to_string();
    let marks = log
        .window()
        .filter(|r| r.k == Kind::Input)
        .skip(before)
        .map(|r| {
            let line = aterm_slog::encode(r, aterm_slog::Tier::Host).expect("fits");
            line.split_whitespace()
                .skip(1)
                .filter(|t| !t.starts_with("t="))
                .map(|t| t.replace(&node, "<step>"))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    ((close.out(), close.by), marks)
}

/// T1's matrix, through the real `InputMark::finish`: every actor × act ×
/// step × open-or-closed. A step still open at the write closes `by=` the
/// writer, its outcome the act's only from a proven author (the harness)
/// and only for the step the act answers; one closed before the write
/// closes `by=- out=passed`. Every acted input is marked whatever the
/// burst, with its act, and a late one with `re=` the step it was admitted
/// under; a plain one inside its burst is not marked.
#[cfg(unix)]
#[test]
fn every_actor_act_and_step_closes_by_the_input_bound_to_it() {
    let acts = [
        None,
        Some(ActWord::Approve),
        Some(ActWord::Deny),
        Some(ActWord::Answer),
    ];
    let mut failures = Vec::new();
    for actor in [Actor::Ctl, Actor::Harness] {
        for act in acts {
            for question in [false, true] {
                for open in [true, false] {
                    let out = match (actor, act, question, open) {
                        (Actor::Harness, Some(ActWord::Approve), false, true) => "approved",
                        (Actor::Harness, Some(ActWord::Deny), false, true) => "declined",
                        (Actor::Harness, Some(ActWord::Answer), true, true) => "answered",
                        _ => "passed",
                    };
                    let by = if open { actor } else { Actor::Nobody };
                    let marks: Vec<String> = act
                        .map(|a| {
                            let re = if open { "" } else { " re=<step>" };
                            format!(
                                "mark - input verb=key act={}{re} by={} src=ctl conf=strong",
                                a.word(),
                                actor.word()
                            )
                        })
                        .into_iter()
                        .collect();
                    let want = ((Some(out), by), marks);
                    let got = acted_input(actor, act, question, open);
                    if got != want {
                        failures.push(format!(
                            "{actor:?} act={act:?} question={question} open={open}: \
                             got {got:?}, want {want:?}"
                        ));
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// T39 through T2's reconcile: the person's first key after delivered mail,
/// which found the timeline held (`note_person`'s `try_lock` failed), is
/// still a `human` mark after the mail's, written by the next entry that
/// reaches the log (here the sweep's agent read). One thread, no timing.
/// NEGATIVE CONTROL: a log never bound to the person's count loses the key,
/// and no human mark follows the mail.
#[cfg(unix)]
#[test]
fn a_persons_first_key_after_mail_is_marked_though_the_timeline_was_held() {
    let after_mail = |bound: bool| {
        let (h, _reader) = testing::owned_handle(0);
        let _hand = hand_clock(&h.ctx.timeline);
        let store = crate::session_store::new_store();
        store.write().unwrap().register(h.clone());
        if !bound {
            // Bound to a count no key moves: the log never learns of it.
            h.ctx
                .timeline
                .lock()
                .unwrap()
                .slog_mut()
                .bind_person(std::sync::Arc::default());
        }
        // A burst already running (its stamp ahead of every clock): the key
        // below opens none, so it takes `note_person`'s `try_lock`.
        let _ = h.ctx.human_input.note(u64::MAX / 2, Gesture::Other);
        let line = format!(
            "{} off=9 from=h-a kind=task trust=agent text=x",
            h.ctx.self_id.as_str()
        );
        assert!(crate::fabric::cmd_deliver(&store, &line).starts_with("OK"));
        {
            let _held = h.ctx.timeline.lock().unwrap();
            assert!(!crate::app_input::note_person(&h.ctx, Gesture::Other));
        }
        claude_busy(&h);
        let g = h.ctx.timeline.lock().unwrap();
        g.slog()
            .log()
            .window()
            .filter(|r| r.op == Op::Mark)
            .skip_while(|r| r.k != Kind::Mail)
            .filter(|r| r.k == Kind::Human)
            .count()
    };
    assert_eq!(after_mail(true), 1, "the held key after the mail was lost");
    assert_eq!(after_mail(false), 0);
}

/// T2's second case (deferred to 5D-5; rulings T10, T39): delivered mail,
/// then the person's key that found the timeline held, then a later `ctl
/// turn`: the turn is not the mail's. The held key is the `human` mark
/// between the two, written by the `ctl turn`'s own reconcile before the
/// exchange reaches the tracker, and the fold forgets the waiting mail at
/// it. One thread, no timing.
/// NEGATIVE CONTROL: a log never bound to the person's count loses the key,
/// and the same turn links the mail.
#[cfg(unix)]
#[test]
fn a_held_persons_key_after_mail_links_no_later_ctl_turn() {
    let link_after = |bound: bool| {
        let (h, _reader) = testing::owned_handle(0);
        let hand = hand_clock(&h.ctx.timeline);
        let store = crate::session_store::new_store();
        store.write().unwrap().register(h.clone());
        if !bound {
            // Bound to a count no key moves: the log never learns of it.
            h.ctx
                .timeline
                .lock()
                .unwrap()
                .slog_mut()
                .bind_person(std::sync::Arc::default());
        }
        // Claude at rest, its run open and no turn.
        {
            let mut g = h.ctx.timeline.lock().unwrap();
            g.note_root_group(100);
            assert!(g.note_foreground_group(300));
            assert!(g.set_program(300, Some("claude".into())));
            let pgid = g.agent().program_pgid;
            let _ = g.sweep_slog(&crate::session_log::BlockRead::default(), false, 100, pgid);
            g.publish_agent("idle", None, None, CLAUDE, stamp(1));
        }
        // A burst already running: the key below takes `note_person`'s
        // `try_lock`.
        let _ = h.ctx.human_input.note(u64::MAX / 2, Gesture::Other);
        hand.set(100);
        let line = format!(
            "{} off=9 from=h-a kind=task trust=agent text=x",
            h.ctx.self_id.as_str()
        );
        assert!(crate::fabric::cmd_deliver(&store, &line).starts_with("OK"));
        {
            let _held = h.ctx.timeline.lock().unwrap();
            assert!(!crate::app_input::note_person(&h.ctx, Gesture::Other));
        }
        hand.set(200);
        let mut g = h.ctx.timeline.lock().unwrap();
        let now = g.slog().now();
        let _ = g.slog_mut().ctl_turn(Actor::Ctl, 1, None, now);
        hand.set(300);
        g.publish_agent("busy", None, None, CLAUDE, stamp(2));
        let log = g.slog().log();
        let turn = log
            .window()
            .rfind(|r| r.op == Op::Open && r.k == Kind::Turn)
            .cloned()
            .expect("the ctl turn");
        assert_eq!(turn.by, Actor::Ctl, "the exchange's turn");
        log.fold().mail_link(turn.node).map(|l| l.off)
    };
    assert_eq!(
        link_after(true),
        None,
        "the turn after the person's key took the mail"
    );
    assert_eq!(link_after(false), Some(9), "the unbound log lost the key");
}

/// The approval box's close after a harness's `approve` (accepted through
/// the real `InputMark::finish`) and then a person's gesture `ev` through the
/// seam's class ([`crate::app_input::note_person_event`], no mouse tracking,
/// the primary screen).
#[cfg(unix)]
fn close_after_a_gesture(ev: &crate::input::InputEvent) -> (Option<&'static str>, Actor) {
    let (h, _reader) = testing::owned_handle(0);
    let _hand = hand_clock(&h.ctx.timeline);
    let store = crate::session_store::new_store();
    store.write().unwrap().register(h.clone());
    claude_busy(&h);
    publish(&h, "prompt", Some("edit"), 2);
    let mark = harness_approve(&h);
    assert!(testing::paste(&h, &mark, "y"), "the bytes were written");
    let _ = crate::app_input::note_person_event(&h.ctx, ev);
    publish(&h, "busy", None, 3);
    let c = approval_closes(&h).pop().expect("the box closed");
    (c.out(), c.by)
}

/// A person's gestures, each as the seam receives it: a wheel, a view
/// scroll, a selecting drag and a click (none reaches a program that
/// tracks no mouse), and the key `y`.
fn gestures() -> ([crate::input::InputEvent; 4], crate::input::InputEvent) {
    use crate::input::{InputEvent, PixelOffset, ScrollIntent};
    let wheel = InputEvent::Wheel {
        dir: aterm_types::mouse::WheelDir::Up,
        lines: 3,
        detent: true,
        row: 0,
        col: 0,
        mods: 0,
        px_off: PixelOffset::CELL_ORIGIN,
    };
    let drag = InputEvent::MouseMove {
        buttons: 0,
        row: 1,
        col: 4,
        mods: 0,
        side: aterm_core::selection::SelectionSide::Left,
        px_off: PixelOffset::CELL_ORIGIN,
    };
    let click = InputEvent::MouseButton {
        button: aterm_types::mouse::MouseButton::Left,
        pressed: true,
        row: 0,
        col: 0,
        mods: 0,
        click_count: 1,
        side: aterm_core::selection::SelectionSide::Left,
        block: false,
        suppress_copy_on_select: false,
        px_off: PixelOffset::CELL_ORIGIN,
    };
    let key = InputEvent::Key {
        key: aterm_types::keyboard::Key::Character('y'),
        mods: aterm_types::keyboard::Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: aterm_types::keyboard::KeyEventType::Press,
    };
    (
        [wheel, InputEvent::ScrollView(ScrollIntent::Up), drag, click],
        key,
    )
}

/// T1, A GESTURE THAT REACHES NO PROGRAM DISMISSES NO BOX: after the
/// harness's approve, a person's wheel over the viewport, view scroll,
/// selecting drag or click leaves the box the harness's (`approved`); only
/// a gesture that wrote the program bytes is the person's dismissal.
/// POSITIVE CONTROL: the person's key `y` closes it `passed` by the person.
#[cfg(unix)]
#[test]
fn a_gesture_that_reaches_no_program_dismisses_no_box() {
    let (others, key) = gestures();
    for ev in &others {
        assert_eq!(
            close_after_a_gesture(ev),
            (Some("approved"), Actor::Harness),
            "{ev:?} dismissed the box"
        );
    }
    assert_eq!(close_after_a_gesture(&key), (Some("passed"), Actor::Human));
}

/// T39, A GESTURE THAT REACHES NO PROGRAM ANSWERS NO MAIL: delivered mail,
/// then (inside a running burst) a person's wheel, view scroll, drag and
/// click write no `human` mark after it; the key after them writes one.
/// POSITIVE CONTROL: the key.
#[test]
fn a_gesture_that_reaches_no_program_answers_no_mail() {
    let h = crate::session_store::test_handle(32);
    let _hand = hand_clock(&h.ctx.timeline);
    let store = crate::session_store::new_store();
    store.write().unwrap().register(h.clone());
    // A burst already running (its stamp ahead of every clock).
    let _ = h.ctx.human_input.note(u64::MAX / 2, Gesture::Other);
    let line = format!(
        "{} off=9 from=h-a kind=task trust=agent text=x",
        h.ctx.self_id.as_str()
    );
    assert!(crate::fabric::cmd_deliver(&store, &line).starts_with("OK"));
    let after_mail = || {
        h.ctx
            .timeline
            .lock()
            .unwrap()
            .slog()
            .log()
            .window()
            .filter(|r| r.op == Op::Mark)
            .skip_while(|r| r.k != Kind::Mail)
            .filter(|r| r.k == Kind::Human)
            .count()
    };
    let (others, key) = gestures();
    for ev in &others {
        let _ = crate::app_input::note_person_event(&h.ctx, ev);
        assert_eq!(after_mail(), 0, "{ev:?} answered the mail");
    }
    let _ = crate::app_input::note_person_event(&h.ctx, &key);
    assert_eq!(after_mail(), 1, "the key after the mail");
}

/// T2, ONE COUNT DELIVERED ONCE: delivered mail; a person's key that opens
/// a burst is counted (`note_reaching`, `note_person`'s first half); a
/// reconciling entry (the status sweep's agent publication) wins the
/// timeline and delivers it; then the key's own delivery
/// (`deliver_noted`, its second half) runs. One thread, in that order: one
/// `human` mark after the mail, not one per delivery.
#[test]
fn a_persons_key_a_reconcile_delivered_first_is_marked_once() {
    let h = crate::session_store::test_handle(33);
    let _hand = hand_clock(&h.ctx.timeline);
    let store = crate::session_store::new_store();
    store.write().unwrap().register(h.clone());
    claude_busy(&h);
    let line = format!(
        "{} off=9 from=h-a kind=task trust=agent text=x",
        h.ctx.self_id.as_str()
    );
    assert!(crate::fabric::cmd_deliver(&store, &line).starts_with("OK"));
    let noted = h
        .ctx
        .human_input
        .note_reaching(crate::metrics::now_us(), Gesture::Other, true);
    assert!(noted.burst, "the first gesture opens a burst");
    publish(&h, "idle", None, 2);
    let _ = crate::app_input::deliver_noted(&h.ctx, noted, true);
    let g = h.ctx.timeline.lock().unwrap();
    let human: Vec<String> = g
        .slog()
        .log()
        .window()
        .filter(|r| r.op == Op::Mark)
        .skip_while(|r| r.k != Kind::Mail)
        .filter(|r| r.k == Kind::Human)
        .filter_map(|r| aterm_slog::encode(r, aterm_slog::Tier::Host).ok())
        .collect();
    assert_eq!(human.len(), 1, "one key, one human mark: {human:?}");
}

/// T2, A RECONCILE INSIDE A PERSON'S NOTE: delivered mail; a person's key
/// that opens a burst is counted, and before its class is published (the
/// one gap in `note_reaching_split`) a reconciling entry (the status
/// sweep's agent publication) wins the timeline; then the class lands and
/// the key's own delivery (`deliver_noted`) runs. One thread, in that
/// order: one `human` mark after the mail, the key's. NEGATIVE CONTROL (by
/// scratch revert): the class published before the count and the counts
/// read apart (the order until 2026-10-18) mark the key twice.
#[test]
fn a_reconcile_inside_a_persons_note_marks_the_key_once() {
    let h = crate::session_store::test_handle(34);
    let _hand = hand_clock(&h.ctx.timeline);
    let store = crate::session_store::new_store();
    store.write().unwrap().register(h.clone());
    claude_busy(&h);
    let line = format!(
        "{} off=9 from=h-a kind=task trust=agent text=x",
        h.ctx.self_id.as_str()
    );
    assert!(crate::fabric::cmd_deliver(&store, &line).starts_with("OK"));
    let noted = h.ctx.human_input.note_reaching_split(
        crate::metrics::now_us(),
        Gesture::Other,
        true,
        || publish(&h, "idle", None, 2),
    );
    assert!(noted.burst, "the first gesture opens a burst");
    let _ = crate::app_input::deliver_noted(&h.ctx, noted, true);
    let g = h.ctx.timeline.lock().unwrap();
    let human: Vec<String> = g
        .slog()
        .log()
        .window()
        .filter(|r| r.op == Op::Mark)
        .skip_while(|r| r.k != Kind::Mail)
        .filter(|r| r.k == Kind::Human)
        .filter_map(|r| aterm_slog::encode(r, aterm_slog::Tier::Host).ok())
        .collect();
    assert_eq!(human.len(), 1, "one key, one human mark: {human:?}");
    assert!(!human[0].contains("reached="), "the key reached: {human:?}");
}

/// T39, A BURST A GESTURE THAT REACHES NO PROGRAM OPENS ANSWERS NO MAIL:
/// Claude at rest, mail delivered, and the person's first gesture (a burst
/// opens: the usual moment mail is waiting) is a wheel the program is not
/// sent: over the viewport, under X10 tracking (press-only), or horizontal
/// on the alternate screen under DEC 1007 with no tracking (alternate
/// scroll is vertical only; the host turns DEC 1007 on for every session).
/// Its burst keeps its `human` row and mark, the mark saying `reached=0`,
/// and the `ctl turn` after it links the mail. POSITIVE CONTROL: a burst
/// the key `y` or a wheel under normal tracking opens answers it, its mark
/// plain, and the turn links none. NEGATIVE CONTROLS (by scratch revert): a
/// burst mark that does not say `reached=0` (as 5D-4 wrote it) forgets the
/// mail, and the turn links none; a wheel that reaches whenever its route
/// is not the viewport (the arm until 2026-10-18) answers it under X10 and
/// for the horizontal wheel.
#[cfg(unix)]
#[test]
fn a_burst_a_gesture_that_reaches_no_program_opens_answers_no_mail() {
    let after = |set: &[u8], ev: &crate::input::InputEvent| {
        let (h, _reader) = testing::owned_handle(0);
        h.term.lock().unwrap().process(set);
        let hand = hand_clock(&h.ctx.timeline);
        let store = crate::session_store::new_store();
        store.write().unwrap().register(h.clone());
        {
            let mut g = h.ctx.timeline.lock().unwrap();
            g.note_root_group(100);
            assert!(g.note_foreground_group(300));
            assert!(g.set_program(300, Some("claude".into())));
            let pgid = g.agent().program_pgid;
            let _ = g.sweep_slog(&crate::session_log::BlockRead::default(), false, 100, pgid);
            g.publish_agent("idle", None, None, CLAUDE, stamp(1));
        }
        hand.set(100);
        let line = format!(
            "{} off=9 from=h-a kind=task trust=agent text=x",
            h.ctx.self_id.as_str()
        );
        assert!(crate::fabric::cmd_deliver(&store, &line).starts_with("OK"));
        let _ = crate::app_input::note_person_event(&h.ctx, ev);
        hand.set(200);
        let mut g = h.ctx.timeline.lock().unwrap();
        let now = g.slog().now();
        let _ = g.slog_mut().ctl_turn(Actor::Ctl, 1, None, now);
        hand.set(300);
        g.publish_agent("busy", None, None, CLAUDE, stamp(2));
        let rows = g.since(None).filter(|e| e.kind == "human").count();
        let log = g.slog().log();
        let marks: Vec<String> = log
            .window()
            .filter(|r| r.op == Op::Mark)
            .skip_while(|r| r.k != Kind::Mail)
            .filter(|r| r.k == Kind::Human)
            .map(|r| {
                r.f.get_u(aterm_slog::Key::Reached)
                    .map_or_else(|| "human".to_string(), |n| format!("human reached={n}"))
            })
            .collect();
        let turn = log
            .window()
            .rfind(|r| r.op == Op::Open && r.k == Kind::Turn)
            .cloned()
            .expect("the ctl turn");
        assert_eq!(turn.by, Actor::Ctl, "the exchange's turn");
        (rows, marks, log.fold().mail_link(turn.node).map(|l| l.off))
    };
    let (others, key) = gestures();
    let wheel = &others[0];
    let mirror = aterm_core::terminal::ModeMirror::default();
    assert!(!crate::human_input::reaches_program(wheel, &mirror));
    let crate::input::InputEvent::Wheel {
        lines,
        detent,
        row,
        col,
        mods,
        px_off,
        ..
    } = *wheel
    else {
        unreachable!("gestures() leads with a wheel")
    };
    let left = crate::input::InputEvent::Wheel {
        dir: aterm_types::mouse::WheelDir::Left,
        lines,
        detent,
        row,
        col,
        mods,
        px_off,
    };
    for (set, ev) in [
        (&b""[..], wheel),
        (b"\x1b[?9h", wheel),
        (b"\x1b[?1049h\x1b[?1007h", &left),
    ] {
        assert_eq!(
            after(set, ev),
            (1, vec!["human reached=0".to_string()], Some(9)),
            "the wheel's burst under {:?} answered the mail",
            String::from_utf8_lossy(set)
        );
    }
    for (set, ev) in [(&b""[..], &key), (b"\x1b[?1000h", wheel)] {
        assert_eq!(
            after(set, ev),
            (1, vec!["human".to_string()], None),
            "{ev:?} under {:?} is the person's answer",
            String::from_utf8_lossy(set)
        );
    }
}
