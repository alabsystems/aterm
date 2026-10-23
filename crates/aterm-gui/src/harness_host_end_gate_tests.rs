// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Relaunch over a landed task under Autopilot Cruise / Land ([`super::watched`]).

use super::*;
use crate::presence::ChipLevel;

const OTHER_DEFENSE: &str = "Rebased and pushed what you asked.\nNothing left: all of it is on \
                             the remote main branch.";
const MOVED_ON: &str = "Two tests still fail on the parser's edge case; fixing that next.";

/// When the reply's 40 s of work is told, against the faces of the restarted
/// loop's first point.
#[derive(Clone, Copy, Debug)]
enum Order {
    /// Before the point, as a relaunch tells it, and nothing after.
    Relaunch,
    /// Before the point, then a landing told at once after the point's open:
    /// a WITHDRAWN landing — the reply opened the task before its own
    /// defense landed it, so all of the reply's work ran before the open
    /// (the CLIP, `AbBook::task_opened` zeroing the work at the open).
    Withdrawn,
    /// After the point's open, then a landing of the task's own (the
    /// in-test control): work the task did after it opened.
    TaskOwn,
}

/// What the reply on screen at the relaunch was.
#[derive(Clone, Copy, Debug)]
enum Reply {
    /// The landing's own defense, as the seed's.
    Same,
    /// The work going on.
    MovedOn,
    /// Another turn's clean defense.
    OtherDefense,
}

fn want(reply: Reply) -> Vec<Option<TaskFace>> {
    match reply {
        Reply::Same => vec![None, Some(TaskFace::Landed)],
        Reply::MovedOn | Reply::OtherDefense => {
            vec![None, Some(TaskFace::Landed), Some(TaskFace::Open)]
        }
    }
}

fn at_restatement(told: &[crate::HarnessFact]) -> (&[crate::HarnessFact], &[crate::HarnessFact]) {
    let mut unknown = false;
    for (at, fact) in told.iter().enumerate() {
        match fact {
            crate::HarnessFact::Task(None) => unknown = true,
            crate::HarnessFact::Task(Some(TaskFace::Landed))
            | crate::HarnessFact::TaskNoted(TaskFace::Landed, _)
                if unknown =>
            {
                return told.split_at(at + 1);
            }
            _ => {}
        }
    }
    (told, &[])
}

fn point_journaled(w: &Watched, what: &str) {
    let mut lines = Vec::new();
    let found = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(5),
        || {
            lines = aterm_agent::supervise::journal::read_journal(&w.journal)
                .map(|(records, _)| records.into_iter().map(|r| r.line).collect::<Vec<_>>())
                .unwrap_or_default();
            lines
                .iter()
                .skip_while(|line| !line.starts_with("EXIT "))
                .any(|line| {
                    line.starts_with("DONE ")
                        || line.starts_with("WAITING ")
                        || line.starts_with("CONTINUED ")
                        || line.starts_with("TYPED ")
                        || line.starts_with("SKIPPED ")
                })
                .then_some(())
        },
    );
    found.unwrap_or_else(|| panic!("not within 60 s: {what}: the journal {lines:?}"));
}

fn work(app: &mut App, ms: u64) {
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    let publish = |word: &'static str| {
        ctx.timeline.lock().unwrap().publish_agent(
            word,
            None,
            None,
            Some(Program::Claude),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                fp: 1,
            },
        )
    };
    assert!(publish("busy"));
    app.note_harness_phase(0);
    crate::turn_ledger::advance_for_test(ms);
    assert!(publish("idle"));
    app.note_harness_phase(0);
}

fn relaunch(reply: Reply, order: Order) -> (ChipLevel, bool, u32) {
    let mut app = App::headless_for_test();
    let (w, sid, _) = watched(&mut app, "end-gate");
    app.harness = Some(w.host.clone());
    let id = app.next_session_id;
    app.push_stub_tab(crate::WindowId(0), crate::stub_session(id));
    let first = w.told_until("the first loop's landing", |t| {
        faces(t).contains(&Some(TaskFace::Landed))
    });
    apply(&mut app, &sid, &first);
    w.note_past(1, "the first loop's note of the landing");
    assert!(
        !app.end_unseen(0),
        "PRECONDITION: a restated landing is no end"
    );
    let mut claude_off = on();
    claude_off.set("claude.enabled", "false").unwrap();
    w.host.set_config(claude_off);
    let mut told = w.told_until("unknown as the worker stops", |t| faces(t).contains(&None));
    let term = app.pool.get(0).unwrap().term.clone();
    let cols = usize::from(term.lock().unwrap().cols());
    let said = match reply {
        Reply::Same => LANDING_REPLY,
        Reply::MovedOn => MOVED_ON,
        Reply::OtherDefense => OTHER_DEFENSE,
    };
    term.lock().unwrap().process(&ended_screen(said, cols));
    w.host.set_config(on());
    let wanted = want(reply);
    told.extend(w.told_until(&format!("{reply:?}"), |t| {
        faces(t).len() + 1 >= wanted.len()
    }));
    point_journaled(&w, &format!("{reply:?}"));
    told.extend(w.facts.try_iter());
    assert_eq!(faces(&told), wanted, "{reply:?}: {told:?}");
    let (start, point) = at_restatement(&told);
    apply(&mut app, &sid, start);
    if let Order::Relaunch | Order::Withdrawn = order {
        work(&mut app, 40_000);
        apply(&mut app, &sid, point);
        if let Order::Withdrawn = order {
            assert!(
                faces(point).contains(&Some(TaskFace::Open)),
                "PRECONDITION {reply:?}: the point opened the task"
            );
            apply(
                &mut app,
                &sid,
                &[crate::HarnessFact::Task(Some(TaskFace::Landed))],
            );
        }
    } else {
        let opened = point
            .iter()
            .position(|f| *f == crate::HarnessFact::Task(Some(TaskFace::Open)))
            .map_or(point.len(), |at| at + 1);
        let (to_open, rest) = point.split_at(opened);
        apply(&mut app, &sid, to_open);
        work(&mut app, 40_000);
        apply(&mut app, &sid, rest);
        apply(
            &mut app,
            &sid,
            &[crate::HarnessFact::Task(Some(TaskFace::Landed))],
        );
    }
    let ws = &app.windows[&crate::WindowId(0)];
    let mut metadata: Vec<crate::tab_bar::TabStripMetadata> = ws
        .tab_set
        .tabs()
        .iter()
        .map(|t| crate::tab_bar::TabStripMetadata::from_presentation(&t.presentation))
        .collect();
    app.stamp_presence_chips(crate::WindowId(0), &mut metadata);
    let landed = app
        .harness_screen
        .ab
        .week_at(crate::turn_ledger::now_ms(), &[])
        .ends
        .landed;
    (metadata[0].attention, app.end_unseen(0), landed)
}

#[test]
fn a_relaunch_over_a_landing_arms_no_end_whatever_the_reply() {
    for reply in [Reply::Same, Reply::MovedOn, Reply::OtherDefense] {
        assert_eq!(
            relaunch(reply, Order::Relaunch),
            (ChipLevel::Off, false, 0),
            "{reply:?}: nothing arms"
        );
    }
    for reply in [Reply::MovedOn, Reply::OtherDefense] {
        assert_eq!(
            relaunch(reply, Order::Withdrawn),
            (ChipLevel::Off, false, 1),
            "WITHDRAWN {reply:?}: a landing, but its work ran before it opened"
        );
    }
    for reply in [Reply::MovedOn, Reply::OtherDefense] {
        assert_eq!(
            relaunch(reply, Order::TaskOwn),
            (ChipLevel::Done, true, 1),
            "CONTROL {reply:?}: real work arms"
        );
    }
}
