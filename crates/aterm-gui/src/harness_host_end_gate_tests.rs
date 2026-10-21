// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! GATE (a) OF THE ONE END (`docs/DESIGN-harness-autopilot-2026-10-15.md`
//! §11.2.5; §11.2.3 item 3, the review's R2), under FOREVER CRUISE / FOREVER
//! LAND, where the one end is Land's landing: a relaunch over a LANDED task —
//! the host's REAL worker running aterm-agent's REAL hosted loop over the
//! App's own session terminal ([`super::watched`]), its supervision restarted
//! so the loop restates the landing from its journal's seed — arms NO end
//! whatever the reply on screen is, though that reply works 40 s: the
//! landing's own defense (no move); a reply that moved on (the seed dropped:
//! open); a clean defense someone else's turn wrote (no land act asked for
//! it, so it lands nothing: the seed dropped, open). The faces are applied to
//! a real headless App as the host's wake applies them, the restatement
//! first, then the reply's 40 s of busy time fed to the A/B book as the
//! status sweep feeds it, then the point's faces — the order a relaunch tells
//! them in. Nothing arms: no landed mark on the session's tab behind the
//! front one, no end on its glass and no landing counted — nor where a
//! landing follows the point's open at once (a withdrawn landing: the
//! reply's work all before the open, clipped there).

use super::*;
use crate::presence::ChipLevel;

/// A clean defense written in someone else's turn: other words than the
/// landing's, which no land act asked for.
const OTHER_DEFENSE: &str = "Rebased and pushed what you asked.\nNothing left: all of it is on \
                             the remote main branch.";
/// A reply that moved on: no defense, the work going on.
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

/// The faces a restart over `reply` tells, from the unknown on.
fn want(reply: Reply) -> Vec<Option<TaskFace>> {
    match reply {
        Reply::Same => vec![None, Some(TaskFace::Landed)],
        Reply::MovedOn | Reply::OtherDefense => {
            vec![None, Some(TaskFace::Landed), Some(TaskFace::Open)]
        }
    }
}

/// `told` split after the restated landing: what the loop's start told, and
/// what its first point told.
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

/// THE RESTARTED LOOP'S FIRST POINT, JOURNALED: a line after the first
/// loop's `EXIT` that only a judged point writes — the landing's note again
/// (`DONE …`), the wait a first point's back-off leaves (`WAITING …`), or the
/// act it typed or tried to (`CONTINUED …`, `TYPED …`, `SKIPPED …` where the
/// test's terminal took no submit). A must-happen wait for the record itself
/// (a hang detector), as [`Watched::note_past`] is: the faces that point
/// tells precede its line in the loop's own thread, so they are in the
/// channel once it is written.
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

/// The agent of the App's session 0 works `ms` (busy, the ledger clock moved
/// on, then at its prompt), fed to the book through the sweep's own path.
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

/// One relaunch over a landing, the reply `reply` on screen after 40 s of
/// work, told in `order` against the restarted loop's first point
/// ([`Order`]). The App's session 0's tab mark, whether an end stands unseen,
/// and the landings counted after.
fn relaunch(reply: Reply, order: Order) -> (ChipLevel, bool, u32) {
    let mut app = App::headless_for_test();
    let (w, sid, _) = watched(&mut app, "end-gate");
    app.harness = Some(w.host.clone());
    // A tab in front: the session's own is behind it, where a mark shows.
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
    // The relaunch: the loop's supervision stops; the reply is on screen
    // when the next loop's first point reads it.
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
            // The reply's own landing, at once: no work after the open.
            apply(
                &mut app,
                &sid,
                &[crate::HarnessFact::Task(Some(TaskFace::Landed))],
            );
        }
    } else {
        // The work told after the point's open: the task's own.
        let opened = point
            .iter()
            .position(|f| *f == crate::HarnessFact::Task(Some(TaskFace::Open)))
            .map_or(point.len(), |at| at + 1);
        let (to_open, rest) = point.split_at(opened);
        apply(&mut app, &sid, to_open);
        work(&mut app, 40_000);
        apply(&mut app, &sid, rest);
        // The work goes on to a landing of its own.
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

/// GATE (a): A RELAUNCH OVER A LANDING ARMS NO END, WHATEVER THE REPLY ON
/// SCREEN — each of its three outcomes after 40 s of the reply's work: no
/// landed mark (`"done"` not among the tab's chrome states), no end
/// standing, no landing counted. A WITHDRAWN LANDING arms none either: the
/// point opened the task (the reply moved on, or another turn's defense)
/// and a landing follows at once, the reply's 40 s all before the open —
/// one landing counted, as it is one, but no mark and no end, its work
/// clipped at the open. NEGATIVE CONTROLS: the clip removed
/// (`AbBook::task_opened` not zeroing the work at the open) arms an end for
/// the withdrawn landing — as run, this test fails; in-test, the same 40 s
/// told after the point's open (work the task did after it opened), then a
/// landing of its own, arms one — the mark, the end and the count.
#[test]
fn a_relaunch_over_a_landing_arms_no_end_whatever_the_reply() {
    for reply in [Reply::Same, Reply::MovedOn, Reply::OtherDefense] {
        assert_eq!(
            relaunch(reply, Order::Relaunch),
            (ChipLevel::Off, false, 0),
            "{reply:?}: nothing arms"
        );
    }
    // THE CLIP: a landing withdrawn and landed again with no work since.
    for reply in [Reply::MovedOn, Reply::OtherDefense] {
        assert_eq!(
            relaunch(reply, Order::Withdrawn),
            (ChipLevel::Off, false, 1),
            "WITHDRAWN {reply:?}: a landing, but its work ran before it opened"
        );
    }
    // CONTROL: work after the task opened, then its landing, is an end.
    for reply in [Reply::MovedOn, Reply::OtherDefense] {
        assert_eq!(
            relaunch(reply, Order::TaskOwn),
            (ChipLevel::Done, true, 1),
            "CONTROL {reply:?}: real work arms"
        );
    }
}
