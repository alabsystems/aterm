// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host-side session-log input attribution and reconciliation tests.

use aterm_slog::{ActWord, Actor, Kind, Op, Rec};

use super::tests::{hand_clock, stamp};
use crate::control_input_ack::{InputMark, testing};
use crate::human_input::Gesture;
use crate::session_log::InputVerb;
use crate::session_store::SessionHandle;

const CLAUDE: Option<aterm_phase::Program> = Some(aterm_phase::Program::Claude);

fn claude_busy(h: &SessionHandle) {
    let mut g = h.ctx.timeline.lock().unwrap();
    g.note_root_group(100);
    assert!(g.note_foreground_group(300));
    assert!(g.set_program(300, Some("claude".into())));
    let pgid = g.agent().program_pgid;
    let _ = g.sweep_slog(&crate::session_log::BlockRead::default(), false, 100, pgid);
    g.publish_agent("busy", None, None, CLAUDE, stamp(1));
}

fn publish(h: &SessionHandle, word: &'static str, detail: Option<&str>, seq: u64) {
    h.ctx.timeline.lock().unwrap().publish_agent(
        word,
        detail.map(str::to_string),
        None,
        CLAUDE,
        stamp(seq),
    );
}

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

fn harness_approve(h: &SessionHandle) -> InputMark {
    InputMark::at_epoch(
        &h.ctx,
        h.local_id,
        Actor::Harness,
        InputVerb::Key,
        None,
        false,
    )
    .acting(Some(ActWord::Approve))
}

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
/// closes `by=human out=passed`.
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
            g.slog_mut().open_session(None, None, now);
        }
        claude_busy(&h);
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
        InputMark::at_epoch(&h.ctx, h.local_id, actor, InputVerb::Key, None, false).acting(act)
    };
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
/// step × open-or-closed.
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
/// reaches the log (here the sweep's agent read).
#[cfg(unix)]
#[test]
fn a_persons_first_key_after_mail_is_marked_though_the_timeline_was_held() {
    let after_mail = |bound: bool| {
        let (h, _reader) = testing::owned_handle(0);
        let _hand = hand_clock(&h.ctx.timeline);
        let store = crate::session_store::new_store();
        store.write().unwrap().register(h.clone());
        if !bound {
            h.ctx
                .timeline
                .lock()
                .unwrap()
                .slog_mut()
                .bind_person(std::sync::Arc::default());
        }
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

/// T2's second case (deferred to 5D-5): delivered mail,
/// then the person's key that found the timeline held, then a later `ctl
/// turn`: the turn is not the mail's. The held key is the `human` mark
/// between the two, written by the `ctl turn`'s own reconcile before the
/// exchange reaches the tracker, and the fold forgets the waiting mail at
/// it.
#[cfg(unix)]
#[test]
fn a_held_persons_key_after_mail_links_no_later_ctl_turn() {
    let link_after = |bound: bool| {
        let (h, _reader) = testing::owned_handle(0);
        let hand = hand_clock(&h.ctx.timeline);
        let store = crate::session_store::new_store();
        store.write().unwrap().register(h.clone());
        if !bound {
            h.ctx
                .timeline
                .lock()
                .unwrap()
                .slog_mut()
                .bind_person(std::sync::Arc::default());
        }
        {
            let mut g = h.ctx.timeline.lock().unwrap();
            g.note_root_group(100);
            assert!(g.note_foreground_group(300));
            assert!(g.set_program(300, Some("claude".into())));
            let pgid = g.agent().program_pgid;
            let _ = g.sweep_slog(&crate::session_log::BlockRead::default(), false, 100, pgid);
            g.publish_agent("idle", None, None, CLAUDE, stamp(1));
        }
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

/// THE MAIL A TURN PROBABLY ANSWERED, IN THE VERBS' BRIEFS:
/// delivered `kind=task` mail, then a `ctl turn` that
/// names no cause.
#[cfg(unix)]
#[test]
fn the_brief_verbs_name_the_task_mail_a_ctl_turn_probably_answered() {
    let (h, _reader) = testing::owned_handle(0);
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
    hand.set(200);
    let turn = {
        let mut g = h.ctx.timeline.lock().unwrap();
        let now = g.slog().now();
        let _ = g.slog_mut().ctl_turn(Actor::Ctl, 1, None, now);
        hand.set(300);
        g.publish_agent("busy", None, None, CLAUDE, stamp(2));
        let log = g.slog().log();
        for tier in ["", "tier=content"] {
            let q = crate::session_log::LogQuery::parse(tier).expect("a log query");
            let text = crate::session_log::render_log(log, &q);
            assert!(
                !text.contains("mail=@"),
                "`log {tier}` names a link:\n{text}"
            );
        }
        log.window()
            .rfind(|r| r.op == Op::Open && r.k == Kind::Turn)
            .map(|r| r.node)
            .expect("the ctl turn")
    };
    for verb in ["", "all"] {
        let text = crate::control::cmd_brief(&h.ctx, &store, verb);
        let line = text
            .lines()
            .find(|l| l.starts_with(&format!("{turn} ")))
            .unwrap_or_else(|| panic!("`brief {verb}` has no line for {turn}:\n{text}"));
        assert!(
            line.contains(" by=ctl ledger=1 mail=@9~ "),
            "`brief {verb}`: {line}"
        );
    }
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

#[test]
fn a_gesture_that_reaches_no_program_answers_no_mail() {
    let h = crate::session_store::test_handle(32);
    let _hand = hand_clock(&h.ctx.timeline);
    let store = crate::session_store::new_store();
    store.write().unwrap().register(h.clone());
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

#[test]
fn a_persons_first_input_after_an_update_answers_mail_delivered_before_it() {
    let marks_after_handoff = |mailed: bool| {
        let h = crate::session_store::test_handle(35);
        let _hand = hand_clock(&h.ctx.timeline);
        let store = crate::session_store::new_store();
        store.write().unwrap().register(h.clone());
        if mailed {
            let line = format!(
                "{} off=9 from=h-a kind=task trust=agent text=x",
                h.ctx.self_id.as_str()
            );
            assert!(crate::fabric::cmd_deliver(&store, &line).starts_with("OK"));
        }
        let (next, _next_store, _hand2) =
            super::tests::hand_over(&h, &store.read().unwrap(), 1_000, true);
        let latch = next
            .ctx
            .timeline
            .lock()
            .unwrap()
            .slog()
            .log()
            .fold()
            .in_since_human();
        let _ = next.ctx.human_input.note(u64::MAX / 2, Gesture::Other);
        let human = || {
            next.ctx
                .timeline
                .lock()
                .unwrap()
                .slog()
                .log()
                .window()
                .filter(|r| r.op == Op::Mark)
                .skip_while(|r| r.k != Kind::Handoff)
                .filter(|r| r.k == Kind::Human)
                .count()
        };
        let _ = crate::app_input::note_person_reaching(&next.ctx, Gesture::Other, true);
        let first = human();
        let _ = crate::app_input::note_person_reaching(&next.ctx, Gesture::Other, true);
        ((first, human()), latch)
    };
    let (marks, latch) = marks_after_handoff(true);
    assert_eq!(
        marks,
        (1, 1),
        "the first input after the update answers the mail delivered before it, once"
    );
    assert!(latch, "the latch the carry handed the successor");
    assert_eq!(
        marks_after_handoff(false),
        ((0, 0), false),
        "no mail, no mark"
    );
}

/// THE VIEW'S `human` ROWS AND THE BURST MARKS (every `human` mark but the
/// mail latch's own, `why=mail`), as counts.
fn human_rows_and_burst_marks(g: &crate::session_timeline::SessionTimeline) -> (usize, usize) {
    let rows = g.since(None).filter(|e| e.kind == "human").count();
    let marks = g
        .slog()
        .log()
        .window()
        .filter(|r| r.op == Op::Mark && r.k == Kind::Human)
        .filter(|r| r.f.get_word(aterm_slog::Key::Why) != Some("mail"))
        .count();
    (rows, marks)
}

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
    assert!(!human[0].contains("why="), "the burst's mark: {human:?}");
    assert_eq!(
        human_rows_and_burst_marks(&g),
        (1, 1),
        "the burst's row is its mark"
    );
}

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
    assert_eq!(
        human_rows_and_burst_marks(&g),
        (1, 1),
        "the burst's row is its mark (6F-8)"
    );
}

#[test]
fn a_persons_key_a_mail_delivery_reconciled_first_is_marked_before_the_mail() {
    let h = crate::session_store::test_handle(36);
    let _hand = hand_clock(&h.ctx.timeline);
    let store = crate::session_store::new_store();
    store.write().unwrap().register(h.clone());
    claude_busy(&h);
    let deliver = |off: u64| {
        let line = format!(
            "{} off={off} from=h-a kind=task trust=agent text=x",
            h.ctx.self_id.as_str()
        );
        assert!(crate::fabric::cmd_deliver(&store, &line).starts_with("OK"));
    };
    deliver(9);
    let noted = h
        .ctx
        .human_input
        .note_reaching(crate::metrics::now_us(), Gesture::Other, true);
    assert!(noted.burst, "the first gesture opens a burst");
    deliver(10);
    let _ = crate::app_input::deliver_noted(&h.ctx, noted, true);
    let g = h.ctx.timeline.lock().unwrap();
    let marks: Vec<String> = g
        .slog()
        .log()
        .window()
        .filter(|r| r.op == Op::Mark)
        .skip_while(|r| r.k != Kind::Mail)
        .filter(|r| matches!(r.k, Kind::Human | Kind::Mail))
        .map(|r| match r.f.get_word(aterm_slog::Key::Why) {
            Some(why) => format!("{} why={why}", r.k.word()),
            None => r.k.word().to_string(),
        })
        .collect();
    assert_eq!(
        marks,
        ["mail", "human", "mail"],
        "the key is the burst's mark, before the second mail"
    );
    assert_eq!(
        human_rows_and_burst_marks(&g),
        (1, 1),
        "the burst's row is its mark"
    );
}

/// T39, A BURST A GESTURE THAT REACHES NO PROGRAM OPENS ANSWERS NO MAIL:
/// Claude at rest, mail delivered, and the person's first gesture (a burst
/// opens: the usual moment mail is waiting) is a wheel the program is not
/// sent: over the viewport, under X10 tracking (press-only), or horizontal
/// on the alternate screen under DEC 1007 with no tracking (alternate
/// scroll is vertical only; the host turns DEC 1007 on for every session).
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
    assert!(!crate::human_input::reaches_program(
        wheel,
        &mirror,
        crate::input::WheelPlatform::Host
    ));
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

fn class_of(ev: &crate::input::InputEvent) -> &'static str {
    use crate::input::InputEvent as E;
    match ev {
        E::Key { .. } => "key",
        E::Text(_) => "text",
        E::KeySequence(_) => "key-sequence",
        E::MouseButton { .. } => "mouse-button",
        E::MouseMove { .. } => "mouse-move",
        E::Wheel { .. } => "wheel",
        E::ScrollView(_) => "scroll-view",
        E::Paste(..) => "paste",
        E::Resize { .. } => "resize",
        E::ResizeWindowPx { .. } => "resize-window-px",
        E::Focus(_) => "focus",
    }
}

fn every_event() -> Vec<crate::input::InputEvent> {
    use crate::input::{InputEvent, PasteFraming, PixelOffset, ScrollIntent};
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
    use aterm_types::mouse::{ALT_MASK, MouseButton, SHIFT_MASK, WheelDir};
    let side = aterm_core::selection::SelectionSide::Left;
    let mut out = Vec::new();
    for event_type in [
        KeyEventType::Press,
        KeyEventType::Repeat,
        KeyEventType::Release,
    ] {
        for key in [
            Key::Character('y'),
            Key::Named(NamedKey::ArrowUp),
            Key::Named(NamedKey::Enter),
            Key::Named(NamedKey::Backspace),
            Key::Named(NamedKey::Numpad5),
            Key::Named(NamedKey::NumpadEnter),
            Key::Named(NamedKey::ShiftLeft),
            Key::Named(NamedKey::CapsLock),
        ] {
            for mods in [
                Modifiers::empty(),
                Modifiers::CTRL,
                Modifiers::ALT,
                Modifiers::META,
            ] {
                out.push(InputEvent::Key {
                    key: key.clone(),
                    mods,
                    base_layout: None,
                    layout_text: None,
                    event_type,
                });
            }
        }
    }
    for text in ["é", "ok", "\t", ""] {
        out.push(InputEvent::Text(text.to_string()));
    }
    out.push(InputEvent::KeySequence(b"\x1b[A".to_vec()));
    out.push(InputEvent::KeySequence(Vec::new()));
    for button in [MouseButton::Left, MouseButton::Middle, MouseButton::Right] {
        for pressed in [true, false] {
            for mods in [0, SHIFT_MASK] {
                out.push(InputEvent::MouseButton {
                    button,
                    pressed,
                    row: 1,
                    col: 4,
                    mods,
                    click_count: 1,
                    side,
                    block: false,
                    suppress_copy_on_select: false,
                    px_off: PixelOffset::CELL_ORIGIN,
                });
            }
        }
    }
    for buttons in [0, 1, 2, 3] {
        out.push(InputEvent::MouseMove {
            buttons,
            row: 2,
            col: 7,
            mods: 0,
            side,
            px_off: PixelOffset::CELL_ORIGIN,
        });
    }
    for dir in [
        WheelDir::Up,
        WheelDir::Down,
        WheelDir::Left,
        WheelDir::Right,
    ] {
        for mods in [0, SHIFT_MASK, ALT_MASK] {
            for detent in [true, false] {
                for lines in [1, 3] {
                    out.push(InputEvent::Wheel {
                        dir,
                        lines,
                        detent,
                        row: 0,
                        col: 0,
                        mods,
                        px_off: PixelOffset::CELL_ORIGIN,
                    });
                }
            }
        }
    }
    out.push(InputEvent::ScrollView(ScrollIntent::Up));
    for (text, framing) in [
        ("x", PasteFraming::Gesture { bracketed: false }),
        ("x", PasteFraming::Gesture { bracketed: true }),
        ("x", PasteFraming::AtDrain),
        ("a\nb", PasteFraming::Gesture { bracketed: false }),
        (CONTROLS_ONLY, PasteFraming::Gesture { bracketed: false }),
        (CONTROLS_ONLY, PasteFraming::Gesture { bracketed: true }),
        (CONTROLS_ONLY, PasteFraming::AtDrain),
    ] {
        out.push(InputEvent::Paste(text.to_string(), framing));
    }
    out.push(InputEvent::Resize {
        rows: 30,
        cols: 100,
        echo_to_window: false,
    });
    out.push(InputEvent::ResizeWindowPx {
        width: 800,
        height: 600,
    });
    out.push(InputEvent::Focus(true));
    out.push(InputEvent::Focus(false));
    out
}

const CONTROLS_ONLY: &str = "\x1b\u{9b}\x03";

const MIRROR_STATES: [(&str, &[u8]); 29] = [
    ("none", b""),
    ("x10", b"\x1b[?9h"),
    ("normal", b"\x1b[?1000h"),
    ("button-event", b"\x1b[?1002h\x1b[?1006h"),
    ("any-event", b"\x1b[?1003h"),
    ("alt", b"\x1b[?1049h"),
    ("alt+1007", b"\x1b[?1049h\x1b[?1007h"),
    ("alt+1007+decckm", b"\x1b[?1049h\x1b[?1007h\x1b[?1h"),
    ("alt+1007+x10", b"\x1b[?1049h\x1b[?1007h\x1b[?9h"),
    ("alt+1007+normal", b"\x1b[?1049h\x1b[?1007h\x1b[?1000h"),
    ("alt+1007+kitty-all", b"\x1b[?1049h\x1b[?1007h\x1b[>31u"),
    ("focus", b"\x1b[?1004h"),
    ("bracketed", b"\x1b[?2004h"),
    ("decckm", b"\x1b[?1h"),
    ("deckpam", b"\x1b="),
    ("vt52", b"\x1b[?2l"),
    ("modify-other-keys-1", b"\x1b[>4;1m"),
    ("modify-other-keys-2", b"\x1b[>4;2m"),
    ("format-other-keys", b"\x1b[>4;2m\x1b[>4;1f"),
    ("decbkm", b"\x1b[?67h"),
    ("alt-sends-no-escape", b"\x1b[?1039l"),
    ("meta-sends-escape", b"\x1b[?1036h"),
    ("num-lock-reset", b"\x1b[?1035l"),
    ("kitty-disambiguate", b"\x1b[>1u"),
    ("kitty-event-types", b"\x1b[>2u"),
    ("kitty-alternate-keys", b"\x1b[>4u"),
    ("kitty-all-keys", b"\x1b[>8u"),
    ("kitty-associated-text", b"\x1b[>16u"),
    ("kitty-all", b"\x1b[>31u"),
];

#[test]
fn what_reaches_the_program_is_what_the_seam_sends_on_every_platform() {
    use crate::input::{InputEvent, WheelPlatform};
    let events = every_event();
    let mut classes: Vec<&str> = events.iter().map(class_of).collect();
    classes.sort_unstable();
    classes.dedup();
    assert_eq!(
        classes.len(),
        11,
        "every InputEvent class is driven: {classes:?}"
    );
    let sink = aterm_session::sink::SinkWriter::new(-1);
    let mut wrong = Vec::new();
    let mut wrote_of = std::collections::BTreeMap::new();
    let (mut asked, mut outside) = (0_usize, 0_usize);
    for platform in WheelPlatform::ARMS {
        for (state, set) in MIRROR_STATES {
            for (i, ev) in events.iter().enumerate() {
                let term = std::sync::Arc::new(std::sync::Mutex::new(
                    aterm_core::terminal::Terminal::new(24, 80),
                ));
                term.lock().unwrap().process(set);
                let mirror = crate::mode_mirror_of(&term);
                let (_, bytes) = crate::input::seam_encode_on(platform, &term, &mirror, &sink, ev);
                let wrote = !bytes.is_empty();
                wrote_of.insert((format!("{platform:?}"), state, i), wrote);
                if !crate::human_input::is_person_gesture(ev) {
                    outside += 1;
                    continue;
                }
                asked += 1;
                let reaches = crate::human_input::reaches_program(ev, &mirror, platform);
                if reaches != wrote {
                    wrong.push(format!(
                        "{platform:?} {state}: {ev:?} reaches={reaches} wrote={bytes:?}"
                    ));
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {asked} gestures disagree; the first: {:#?}",
        wrong.len(),
        &wrong[..wrong.len().min(8)]
    );
    assert!(asked > 0 && outside > 0, "asked {asked}, outside {outside}");
    let at = |want: &InputEvent| {
        events
            .iter()
            .position(|e| e == want)
            .expect("the witness is in the table")
    };
    let wheel = |dir, detent| InputEvent::Wheel {
        dir,
        lines: 1,
        detent,
        row: 0,
        col: 0,
        mods: 0,
        px_off: crate::input::PixelOffset::CELL_ORIGIN,
    };
    let up = at(&wheel(aterm_types::mouse::WheelDir::Up, true));
    let left = at(&wheel(aterm_types::mouse::WheelDir::Left, true));
    let wrote = |platform: WheelPlatform, state: &'static str, i: usize| {
        wrote_of[&(format!("{platform:?}"), state, i)]
    };
    let off = WheelPlatform::Windows(crate::input::WheelNotch::Lines(0));
    let paste = |framing| at(&InputEvent::Paste(CONTROLS_ONLY.to_string(), framing));
    let (unbracketed, bracketed, live) = (
        paste(crate::input::PasteFraming::Gesture { bracketed: false }),
        paste(crate::input::PasteFraming::Gesture { bracketed: true }),
        paste(crate::input::PasteFraming::AtDrain),
    );
    for (platform, state, i, want) in [
        (WheelPlatform::Mac, "none", unbracketed, false),
        (WheelPlatform::Mac, "bracketed", unbracketed, false),
        (WheelPlatform::Mac, "none", bracketed, true),
        (WheelPlatform::Mac, "none", live, false),
        (WheelPlatform::Mac, "bracketed", live, true),
        (WheelPlatform::Mac, "x10", up, false),
        (WheelPlatform::Mac, "normal", up, true),
        (WheelPlatform::Mac, "alt+1007", left, false),
        (WheelPlatform::Mac, "alt+1007", up, true),
        (WheelPlatform::Vte, "alt+1007", up, true),
        (off, "alt+1007", up, false),
        (off, "alt+1007+normal", up, true),
    ] {
        assert_eq!(
            wrote(platform, state, i),
            want,
            "witness: {platform:?} {state} {:?}",
            events[i]
        );
    }
}

#[test]
fn a_drag_in_its_reports_cell_writes_nothing_and_its_report_had_reached() {
    use crate::input::{InputEvent, PixelOffset, WheelPlatform};
    let side = aterm_core::selection::SelectionSide::Left;
    let press = InputEvent::MouseButton {
        button: aterm_types::mouse::MouseButton::Left,
        pressed: true,
        row: 1,
        col: 4,
        mods: 0,
        click_count: 1,
        side,
        block: false,
        suppress_copy_on_select: false,
        px_off: PixelOffset::CELL_ORIGIN,
    };
    let drag = |col| InputEvent::MouseMove {
        buttons: 0,
        row: 1,
        col,
        mods: 0,
        side,
        px_off: PixelOffset::CELL_ORIGIN,
    };
    let sink = aterm_session::sink::SinkWriter::new(-1);
    for (state, set, reports) in [
        ("button-event", &b"\x1b[?1002h\x1b[?1006h"[..], true),
        ("any-event", b"\x1b[?1003h", true),
        ("normal", b"\x1b[?1000h", false),
    ] {
        let term = std::sync::Arc::new(std::sync::Mutex::new(aterm_core::terminal::Terminal::new(
            24, 80,
        )));
        term.lock().unwrap().process(set);
        let mirror = crate::mode_mirror_of(&term);
        let mut seen = Vec::new();
        for ev in [&press, &drag(4), &drag(5), &drag(5)] {
            let (_, bytes) =
                crate::input::seam_encode_on(WheelPlatform::Mac, &term, &mirror, &sink, ev);
            let reaches = crate::human_input::reaches_program(ev, &mirror, WheelPlatform::Mac);
            seen.push((!bytes.is_empty(), reaches));
        }
        let want = if reports {
            vec![(true, true), (false, true), (true, true), (false, true)]
        } else {
            vec![(true, true), (false, false), (false, false), (false, false)]
        };
        assert_eq!(seen, want, "{state}: (wrote, reaches) for press, drags");
    }
}

/// THE SWEEP'S READ of the Claude Code screen `fixture` (fix E8), as `session_status` makes it:
/// the screen moved (a feed the reader counts), the read sees that count, presence's verdict on
/// the screen is published, and the frame it shows is noted at the count the read saw.
fn see(h: &SessionHandle, fixture: &str, seq: u64) {
    let rows = crate::session_log::keys::tests::fixture_rows(fixture);
    let v = crate::presence::agent_verdict(
        Some("claude"),
        false,
        None,
        &rows,
        crate::presence::Cursor::Unknown,
        "",
        std::time::Instant::now(),
    );
    assert!(
        h.ctx
            .screen_moves
            .fed(crate::control::ScreenGen { epoch: 0, seq })
    );
    let moves = h.ctx.screen_moves.count();
    let mut g = h.ctx.timeline.lock().unwrap();
    g.publish_agent(v.word(), v.detail(), None, CLAUDE, stamp(seq));
    g.slog_mut().note_frame(v.turn().keys, moves);
}

/// A person's plain press of `key`.
fn press(key: aterm_types::keyboard::Key) -> crate::input::InputEvent {
    crate::input::InputEvent::Key {
        key,
        mods: aterm_types::keyboard::Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: aterm_types::keyboard::KeyEventType::Press,
    }
}

const BOX_YES: &str = "claude-2.1.282-box-bash-ask-rule";
const BOX_GARBLED: &str = "claude-2.1.280-box-bash-touch";
const BOX_NO: &str = "claude-2.1.282-box-tool-default-no";
const ASK: &str = "claude-2.1.281-ask-one";
const REVIEW: &str = "claude-2.1.282-question-review";
const BUSY: &str = "claude-2.1.283-footer-busy";
/// `❯ 1. Yes, and keep allowing …`, `2. No, and block reads …` (a refusal the reader reads as
/// kept), `3. No, and ask again next time`, `4. Yes, but ask again next time`.
const READ_OUTSIDE: &str = "claude-2.1.287-read-outside-setting-measured";

/// Who pressed a key in [`a_key_closes_the_box_or_question_as_its_frame_shows_it_choosing`].
#[derive(Clone, Copy, Debug)]
enum Hand {
    /// The window's input path ([`crate::app_input::note_person_event`]).
    Person,
    /// A control client's `key`, through the real `InputMark::finish`.
    Ctl,
}

/// The close of the box or question `fixture` shows after `hand` pressed `key` on it and the
/// screen moved on: `(k, out, by, conf)`.
#[cfg(unix)]
fn close_after_key(
    fixture: &str,
    hand: Hand,
    key: crate::session_log::KeyClass,
) -> (Kind, Option<&'static str>, Actor, aterm_slog::Conf) {
    use aterm_types::keyboard::{Key, NamedKey};
    let (h, _reader) = testing::owned_handle(0);
    let _hand = hand_clock(&h.ctx.timeline);
    let store = crate::session_store::new_store();
    store.write().unwrap().register(h.clone());
    claude_busy(&h);
    see(&h, fixture, 2);
    let ev = match key {
        crate::session_log::KeyClass::Enter => press(Key::Named(NamedKey::Enter)),
        crate::session_log::KeyClass::Esc => press(Key::Named(NamedKey::Escape)),
        crate::session_log::KeyClass::Digit(d) => press(Key::Character(char::from(b'0' + d))),
        crate::session_log::KeyClass::Other => press(Key::Character('y')),
    };
    match hand {
        Hand::Person => {
            let _ = crate::app_input::note_person_event(&h.ctx, &ev);
        }
        Hand::Ctl => {
            let mark =
                InputMark::at_epoch(&h.ctx, h.local_id, Actor::Ctl, InputVerb::Key, None, false)
                    .keyed(crate::session_log::KeyClass::of_event(&ev));
            assert!(testing::input(&h, &mark, &ev), "the key was written");
        }
    }
    see(&h, BUSY, 3);
    let g = h.ctx.timeline.lock().unwrap();
    let c = g
        .slog()
        .log()
        .window()
        .rfind(|r| r.op == Op::Close && matches!(r.k, Kind::Approval | Kind::Question))
        .cloned()
        .unwrap_or_else(|| panic!("{fixture} {hand:?} {key:?}: no step closed"));
    (c.k, c.out(), c.by, c.conf)
}

/// E8 (refining T1), THROUGH THE REAL SEAMS: a key's close reads the key's class and the option
/// the screen showed it choosing, presence's frame of the real screen, whoever pressed it. Enter
/// on a focused `Yes` approves, Enter on a focused `No` declines (Enter is not "yes"), a digit
/// chooses the option it numbers, Esc the box's cancel, and a question's chosen option answers
/// it, each at conf=heuristic; a key no option places (`y`) closes `passed` as before. `by=` is
/// attribution's, unchanged.
#[cfg(unix)]
#[test]
fn a_key_closes_the_box_or_question_as_its_frame_shows_it_choosing() {
    use crate::session_log::KeyClass::{Digit, Enter, Esc, Other};
    use aterm_slog::Conf::{Heuristic, Strong};
    let rows = [
        (
            BOX_YES,
            Hand::Person,
            Enter,
            Kind::Approval,
            "approved",
            Heuristic,
        ),
        (
            BOX_YES,
            Hand::Person,
            Digit(2),
            Kind::Approval,
            "declined",
            Heuristic,
        ),
        (
            BOX_GARBLED,
            Hand::Person,
            Enter,
            Kind::Approval,
            "approved",
            Heuristic,
        ),
        (
            BOX_GARBLED,
            Hand::Person,
            Digit(3),
            Kind::Approval,
            "passed",
            Strong,
        ),
        (
            BOX_YES,
            Hand::Person,
            Esc,
            Kind::Approval,
            "declined",
            Heuristic,
        ),
        (
            BOX_YES,
            Hand::Person,
            Other,
            Kind::Approval,
            "passed",
            Strong,
        ),
        (
            BOX_NO,
            Hand::Person,
            Enter,
            Kind::Approval,
            "declined",
            Heuristic,
        ),
        (
            ASK,
            Hand::Person,
            Enter,
            Kind::Question,
            "answered",
            Heuristic,
        ),
        (
            BOX_YES,
            Hand::Ctl,
            Enter,
            Kind::Approval,
            "approved",
            Heuristic,
        ),
        (
            BOX_GARBLED,
            Hand::Ctl,
            Digit(2),
            Kind::Approval,
            "approved",
            Heuristic,
        ),
        (
            BOX_YES,
            Hand::Ctl,
            Digit(2),
            Kind::Approval,
            "declined",
            Heuristic,
        ),
        (
            BOX_YES,
            Hand::Ctl,
            Esc,
            Kind::Approval,
            "declined",
            Heuristic,
        ),
        (BOX_YES, Hand::Ctl, Other, Kind::Approval, "passed", Strong),
        (
            BOX_NO,
            Hand::Ctl,
            Enter,
            Kind::Approval,
            "declined",
            Heuristic,
        ),
        (
            ASK,
            Hand::Ctl,
            Digit(1),
            Kind::Question,
            "answered",
            Heuristic,
        ),
        (
            REVIEW,
            Hand::Ctl,
            Digit(2),
            Kind::Question,
            "declined",
            Heuristic,
        ),
        (
            READ_OUTSIDE,
            Hand::Person,
            Digit(2),
            Kind::Approval,
            "passed",
            Strong,
        ),
        (
            READ_OUTSIDE,
            Hand::Ctl,
            Digit(2),
            Kind::Approval,
            "passed",
            Strong,
        ),
        (
            READ_OUTSIDE,
            Hand::Ctl,
            Digit(4),
            Kind::Approval,
            "approved",
            Heuristic,
        ),
        (
            READ_OUTSIDE,
            Hand::Person,
            Digit(3),
            Kind::Approval,
            "declined",
            Heuristic,
        ),
    ];
    let mut failures = Vec::new();
    for (fixture, hand, key, k, out, conf) in rows {
        let by = match hand {
            Hand::Person => Actor::Human,
            Hand::Ctl => Actor::Ctl,
        };
        let got = close_after_key(fixture, hand, key);
        let want = (k, Some(out), by, conf);
        if got != want {
            failures.push(format!(
                "{fixture} {hand:?} {key:?}: got {got:?}, want {want:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// E8, THE FRAME A KEY LANDS ON IS THE LAST READ BEFORE IT, AND ONLY WHILE IT SHOWS THE PROGRAM'S
/// STATE AFTER EVERY EARLIER INPUT: a key that moved the focus voids the frame it landed on, so
/// an Enter before the next read chooses nothing (`passed`); a read the screen did not move
/// after it (taken before the program redrew) changes nothing; the redraw's read counts. A
/// control input admitted on a frame chooses nothing when a person's key reached the program
/// before its acceptance, and a person's key delivered behind one the timeline's hold kept
/// chooses nothing (the held key landed on that frame first).
#[cfg(unix)]
#[test]
fn a_key_chooses_only_on_the_frame_the_program_showed_before_it() {
    use aterm_types::keyboard::{Key, NamedKey};
    let close = |f: &dyn Fn(&SessionHandle)| {
        let (h, _reader) = testing::owned_handle(0);
        let _hand = hand_clock(&h.ctx.timeline);
        let store = crate::session_store::new_store();
        store.write().unwrap().register(h.clone());
        claude_busy(&h);
        see(&h, BOX_YES, 2);
        f(&h);
        see(&h, BUSY, 9);
        let c = approval_closes(&h).pop().expect("the box closed");
        (c.out(), c.by, c.conf)
    };
    let enter = press(Key::Named(NamedKey::Enter));
    let down = press(Key::Named(NamedKey::ArrowDown));
    let person = |h: &SessionHandle, ev: &crate::input::InputEvent| {
        let _ = crate::app_input::note_person_event(&h.ctx, ev);
    };
    use aterm_slog::Conf::{Heuristic, Strong};
    assert_eq!(
        close(&|h| person(h, &enter)),
        (Some("approved"), Actor::Human, Heuristic),
        "the control: Enter on the read before it"
    );
    assert_eq!(
        close(&|h| {
            person(h, &down);
            person(h, &enter);
        }),
        (Some("passed"), Actor::Human, Strong),
        "Enter after a key no read followed"
    );
    assert_eq!(
        close(&|h| {
            person(h, &down);
            let moves = h.ctx.screen_moves.count();
            let keys = crate::session_log::keys::tests::frame(BOX_YES);
            h.ctx
                .timeline
                .lock()
                .unwrap()
                .slog_mut()
                .note_frame(keys, moves);
            person(h, &enter);
        }),
        (Some("passed"), Actor::Human, Strong),
        "a read the key did not move"
    );
    // The program's redraw of the same box with its focus moved to a `No`: the screen moved and
    // the read saw it, the verdict unchanged, the frame the default-No box's.
    let redraw = |h: &SessionHandle| {
        assert!(
            h.ctx
                .screen_moves
                .fed(crate::control::ScreenGen { epoch: 0, seq: 3 })
        );
        let moves = h.ctx.screen_moves.count();
        let keys = crate::session_log::keys::tests::frame(BOX_NO);
        h.ctx
            .timeline
            .lock()
            .unwrap()
            .slog_mut()
            .note_frame(keys, moves);
    };
    assert_eq!(
        close(&|h| {
            person(h, &down);
            redraw(h);
            person(h, &enter);
        }),
        (Some("declined"), Actor::Human, Heuristic),
        "the redraw's read: the focus on `No`"
    );
    assert_eq!(
        close(&|h| {
            let mark =
                InputMark::at_epoch(&h.ctx, h.local_id, Actor::Ctl, InputVerb::Key, None, false)
                    .keyed(crate::session_log::KeyClass::Enter);
            person(h, &down);
            assert!(testing::input(h, &mark, &enter));
        }),
        (Some("passed"), Actor::Ctl, Strong),
        "a person's key between a control input's admission and its acceptance"
    );
    let two = press(Key::Character('2'));
    assert_eq!(
        close(&|h| {
            // A gesture that reaches no program opens the burst, so the Enter after it takes the
            // timeline's `try_lock`, which the hold refuses.
            let _ = crate::app_input::note_person_reaching(&h.ctx, Gesture::Other, false);
            {
                let _held = h.ctx.timeline.lock().unwrap();
                assert!(!crate::app_input::note_person_event(&h.ctx, &enter));
            }
            person(h, &two);
        }),
        (Some("passed"), Actor::Human, Strong),
        "a key behind one the hold kept: the Enter reached the program first"
    );
}

/// The program drew the screen numbered `seq` and the PTY reader counted the move, but no agent
/// read saw it yet.
fn drawn(h: &SessionHandle, seq: u64) {
    assert!(
        h.ctx
            .screen_moves
            .fed(crate::control::ScreenGen { epoch: 0, seq })
    );
}

/// E8, the lander's review: A FRAME STANDS ONLY WHILE THE SCREEN HAS NOT MOVED PAST IT. The box
/// the last read showed moved (the program closed or redrew it by itself) before the next read:
/// a key then chooses nothing, a person's or a control input's, until a read sees the move; a
/// read of the same zone at the moved count keeps the frame. The witness counts the screen the
/// store bound at registration (`SessionLog::bind_screen`): a key that reached the program after
/// an unread move voids the frame though a later read, taken before the program's redraw, shows
/// the screen moved past the last read.
#[cfg(unix)]
#[test]
fn a_frame_stands_only_while_the_screen_has_not_moved_past_it() {
    use aterm_slog::Conf::{Heuristic, Strong};
    use aterm_types::keyboard::{Key, NamedKey};
    let close = |f: &dyn Fn(&SessionHandle)| {
        let (h, _reader) = testing::owned_handle(0);
        let _hand = hand_clock(&h.ctx.timeline);
        let store = crate::session_store::new_store();
        store.write().unwrap().register(h.clone());
        claude_busy(&h);
        see(&h, BOX_YES, 2);
        f(&h);
        see(&h, BUSY, 9);
        let c = approval_closes(&h).pop().expect("the box closed");
        (c.out(), c.by, c.conf)
    };
    let enter = press(Key::Named(NamedKey::Enter));
    let down = press(Key::Named(NamedKey::ArrowDown));
    let person = |h: &SessionHandle, ev: &crate::input::InputEvent| {
        let _ = crate::app_input::note_person_event(&h.ctx, ev);
    };
    let ctl_enter = |h: &SessionHandle| {
        let mark = InputMark::at_epoch(&h.ctx, h.local_id, Actor::Ctl, InputVerb::Key, None, false)
            .keyed(crate::session_log::KeyClass::Enter);
        assert!(testing::input(h, &mark, &enter), "the key was written");
    };
    assert_eq!(
        close(&|h| person(h, &enter)),
        (Some("approved"), Actor::Human, Heuristic),
        "the control: Enter on the read before it"
    );
    assert_eq!(
        close(&|h| {
            drawn(h, 3);
            person(h, &enter);
        }),
        (Some("passed"), Actor::Human, Strong),
        "a person's Enter after an unread move"
    );
    assert_eq!(
        close(&|h| {
            drawn(h, 3);
            ctl_enter(h);
        }),
        (Some("passed"), Actor::Ctl, Strong),
        "a control Enter admitted after an unread move"
    );
    assert_eq!(
        close(&|h| {
            drawn(h, 3);
            let moves = h.ctx.screen_moves.count();
            h.ctx
                .timeline
                .lock()
                .unwrap()
                .slog_mut()
                .note_same_frame(moves);
            person(h, &enter);
        }),
        (Some("approved"), Actor::Human, Heuristic),
        "the same zone read again at the moved count"
    );
    assert_eq!(
        close(&|h| {
            drawn(h, 3);
            person(h, &down);
            let moves = h.ctx.screen_moves.count();
            let keys = crate::session_log::keys::tests::frame(BOX_YES);
            h.ctx
                .timeline
                .lock()
                .unwrap()
                .slog_mut()
                .note_frame(keys, moves);
            person(h, &enter);
        }),
        (Some("passed"), Actor::Human, Strong),
        "a read after an unread move and a key, before the program's redraw"
    );
}

/// Paint the fixture `name`'s screen at the foot of `app`'s session `sid` (50 rows by 140), as a
/// program draws it, and count the move as the PTY reader does under the term lock.
fn draw(app: &crate::App, sid: u64, name: &str) {
    draw_titled(app, sid, name, "");
}

/// [`draw`], the program setting its window title to `title` first (OSC 2; `""` leaves it).
fn draw_titled(app: &crate::App, sid: u64, name: &str, title: &str) {
    let rows = crate::session_log::keys::tests::fixture_rows(name);
    let pooled = app.pool.get(sid).expect("pooled");
    let mut t = crate::term_lock(&pooled.term);
    if t.cols() != 140 {
        t.resize(50, 140);
    }
    let top = 50 - rows.len();
    let mut bytes = String::from("\x1b[H\x1b[2J");
    if !title.is_empty() {
        bytes.push_str(&format!("\x1b]2;{title}\x07"));
    }
    for (i, row) in rows.iter().enumerate() {
        bytes.push_str(&format!("\x1b[{};1H{row}", top + i + 1));
    }
    t.process(bytes.as_bytes());
    let _ = pooled.ctx.screen_moves.fed(crate::control::screen_gen(&t));
}

/// E8, the lander's review: THE SWEEP'S OWN READ IS THE FRAME A PERSON'S KEY LANDS ON. Through
/// the real sweep (`App::observe_session_statuses`: its agent read, the screen's count it takes
/// under the term lock, the frame it notes) and the real person seam (`App::note_person_input`),
/// a key closes the box Claude Code's screen showed by the option it landed on; the same box
/// drawn again and read keeps the frame, and one drawn again and not read since leaves the key
/// choosing nothing.
#[cfg(unix)]
#[test]
fn the_sweeps_own_read_is_the_frame_a_persons_key_lands_on() {
    use aterm_slog::Conf::{Heuristic, Strong};
    use aterm_types::keyboard::{Key, NamedKey};
    use std::time::{Duration, Instant};
    let run = |again: Option<bool>, key: &crate::input::InputEvent| {
        let mut app = crate::App::headless_for_test();
        let sid = app.next_session_id;
        app.push_stub_tab(crate::WindowId(0), crate::stub_session(sid));
        let t0 = Instant::now();
        let at = |n: u32| t0 + Duration::from_millis(300) * n;
        draw(&app, sid, BUSY);
        let _ = app.observe_session_statuses(at(0));
        draw(&app, sid, BOX_YES);
        let _ = app.observe_session_statuses(at(1));
        if let Some(read) = again {
            draw(&app, sid, BOX_YES);
            if read {
                let _ = app.observe_session_statuses(at(2));
            }
        }
        app.note_person_input(sid, key, crate::input::Source::Human);
        draw(&app, sid, BUSY);
        let _ = app.observe_session_statuses(at(3));
        let h = app.pool.get(sid).expect("pooled");
        let g = h.ctx.timeline.lock().unwrap();
        let log = g.slog().log();
        let c = log
            .window()
            .rfind(|r| r.op == Op::Close && r.k == Kind::Approval)
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "the box never closed: {:?}",
                    log.window().map(|r| format!("{r:?}")).collect::<Vec<_>>()
                )
            });
        (c.out(), c.by, c.conf)
    };
    let enter = press(Key::Named(NamedKey::Enter));
    let two = press(Key::Character('2'));
    assert_eq!(
        run(None, &enter),
        (Some("approved"), Actor::Human, Heuristic),
        "Enter on the focused `1. Yes` the sweep read"
    );
    assert_eq!(
        run(None, &two),
        (Some("declined"), Actor::Human, Heuristic),
        "`2` on `2. No`"
    );
    assert_eq!(
        run(Some(true), &enter),
        (Some("approved"), Actor::Human, Heuristic),
        "the box drawn again and read: its zone unchanged, its frame kept"
    );
    assert_eq!(
        run(Some(false), &enter),
        (Some("passed"), Actor::Human, Strong),
        "the box drawn again and not read since"
    );
}

/// The last approval step's close in `app`'s session `sid`: its `out=`, `by=` and `conf=`.
fn last_approval_close(
    app: &crate::App,
    sid: u64,
) -> (Option<&'static str>, Actor, aterm_slog::Conf) {
    let h = app.pool.get(sid).expect("pooled");
    let g = h.ctx.timeline.lock().unwrap();
    let log = g.slog().log();
    let c = log
        .window()
        .rfind(|r| r.op == Op::Close && r.k == Kind::Approval)
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "no box closed: {:?}",
                log.window().map(|r| format!("{r:?}")).collect::<Vec<_>>()
            )
        });
    (c.out(), c.by, c.conf)
}

/// E8, merged with the rest settle (agent-verdict-gate): A READ THE REST SETTLE WITHHOLDS KEEPS
/// NO FRAME. The sweep withholds a reading that is no busy within `AGENT_MIN_INTERVAL` of the
/// title's run bit falling, and publishes nothing for it: the screen it read is one no
/// classification stands for, so the frame the last classified read showed does not stand at
/// that read's moves, as the agent stamp does not. Claude Code drew its `1. Yes` box under a
/// running title, then, with the title at rest, a box whose focused option is `No`; a person's
/// Enter inside the settle chooses nothing (it closes `passed`), where a frame carried onto the
/// withheld read would close the box approved. The control: a read past the settle classifies
/// the `No` box, and the same Enter declines.
#[cfg(unix)]
#[test]
fn a_read_the_rest_settle_withholds_keeps_no_frame() {
    use aterm_slog::Conf::{Heuristic, Strong};
    use aterm_types::keyboard::{Key, NamedKey};
    use std::time::{Duration, Instant};
    const RUNS: &str = "\u{25d0} Claude Code";
    const RESTS: &str = "\u{2733} Claude Code";
    let run = |past_the_settle: bool| {
        let mut app = crate::App::headless_for_test();
        let sid = app.next_session_id;
        app.push_stub_tab(crate::WindowId(0), crate::stub_session(sid));
        let t0 = Instant::now();
        let at = |n: u32| t0 + Duration::from_millis(300) * n;
        draw_titled(&app, sid, BUSY, RUNS);
        let _ = app.observe_session_statuses(at(0));
        draw_titled(&app, sid, BOX_YES, RUNS);
        let _ = app.observe_session_statuses(at(1));
        assert_eq!(
            crate::session_log::keys::tests::frame(BOX_YES)
                .and_then(|k| k.chose(crate::session_log::KeyClass::Enter)),
            Some(aterm_slog::Chose::Approve),
            "the first box's Enter approves"
        );
        let before = app.session_status_record(sid).expect("live");
        draw_titled(&app, sid, BOX_NO, RESTS);
        let _ = app.observe_session_statuses(at(2));
        let withheld = app.session_status_record(sid).expect("live");
        let agent_gen = |r: &str| {
            r.split(' ')
                .find_map(|kv| kv.strip_prefix("agent_gen="))
                .map(str::to_string)
        };
        assert_eq!(
            agent_gen(&withheld),
            agent_gen(&before),
            "the settle withheld the read of the second box: {withheld}"
        );
        if past_the_settle {
            let _ = app.observe_session_statuses(at(3));
        }
        let enter = press(Key::Named(NamedKey::Enter));
        app.note_person_input(sid, &enter, crate::input::Source::Human);
        draw_titled(&app, sid, BUSY, RUNS);
        let _ = app.observe_session_statuses(at(4));
        last_approval_close(&app, sid)
    };
    assert_eq!(
        run(false),
        (Some("passed"), Actor::Human, Strong),
        "Enter on a box only a withheld read saw"
    );
    assert_eq!(
        run(true),
        (Some("declined"), Actor::Human, Heuristic),
        "CONTROL: the same Enter once a read past the settle classified the box"
    );
}
