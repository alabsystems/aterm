// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT CAME DOWN, IN DONE FOR YOU (`docs/DESIGN-harness-autopilot-2026-10-15.md` §11.4; the App's
//! half of §11.4.5): a background session's end after 45 s of its agent's busy time is a Done for
//! you row read off the real App (`App::status_session_row`, `Glance::build`), the menu-bar title
//! is `❯•` and `chrome`'s `statusitem` line counts it (`done=1`); an act and a trail move leave the
//! row and the glance's fingerprint as they were; the menu bar's Go marks the end seen and the row
//! goes, and so does a worker's Read Result, whose one row the end shares; the glance owes a
//! rebuild where the end marks, where a look quiets it and at a rename.

use std::sync::Arc;

use aterm_agent::harness::acts::{Act, TaskFace};

use super::*;
use crate::status_item::{DoneAt, Glance};

const SEC: u64 = 1_000;

/// Shut the test's host down on success and assertion unwind alike.
struct TestHost(crate::harness_host::HostHandle);

impl Drop for TestHost {
    fn drop(&mut self) {
        self.0.shutdown_and_join();
    }
}

/// An App whose session 0 runs Claude Code under Land, claimed by this
/// host, in window 0's first tab with a second tab in front, and window 0
/// looked at (the frontmost, on screen). Its sid, and the host to shut
/// down. (`harness_end_tests`' desk, the background case.)
fn desk() -> (App, String, TestHost) {
    let mut app = App::headless_for_test();
    let host = TestHost(crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    ));
    app.harness = Some(host.0.clone());
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    {
        let mut tl = ctx.timeline.lock().expect("timeline");
        tl.note_foreground_group(7);
        tl.set_program(7, Some("claude".into()));
    }
    ctx.meta
        .lock()
        .expect("meta")
        .set("autopilot", Some("land".into()));
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .expect("claimed");
    let id = app.next_session_id;
    app.push_stub_tab(WindowId(0), crate::stub_session(id));
    app.frontmost_window = Some(WindowId(0));
    app.windows
        .get_mut(&WindowId(0))
        .expect("window 0")
        .band_on_screen_for_test = true;
    (app, ctx.self_id.as_str().to_string(), host)
}

/// The agent word the status sweep publishes for session 0.
fn publish(app: &App, word: &'static str) -> bool {
    app.pool
        .get(0)
        .expect("session 0")
        .ctx
        .timeline
        .lock()
        .expect("timeline")
        .publish_agent(
            word,
            None,
            None,
            Some(aterm_phase::Program::Claude),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                fp: 1,
            },
        )
}

/// Session 0's task opens, its agent works `ms` on the ledger clock (moved
/// on, not waited), and its loop tells landed (Land's end; Cruise never
/// ends): an end where `ms` is past the floor.
fn end_after(app: &mut App, sid: &str, ms: u64) {
    tell(app, sid, Some(TaskFace::Open));
    assert!(publish(app, "busy"), "busy published");
    app.note_harness_phase(0);
    crate::turn_ledger::advance_for_test(ms);
    assert!(publish(app, "idle"), "idle published");
    app.note_harness_phase(0);
    tell(app, sid, Some(TaskFace::Landed));
}

/// Session `sid`'s loop tells `face` through the host's wake.
fn tell(app: &mut App, sid: &str, face: Option<TaskFace>) {
    app.apply_harness_fact(sid, crate::HarnessFact::Task(face));
}

/// Session 0's SESSIONS row's state line on Settings ▸ Harness: the words
/// the row must never carry (`last: …` once the trail has an act).
fn state_line(app: &App) -> Option<String> {
    app.harness_state()
        .projection()
        .sessions
        .iter()
        .find(|row| row.local_id == 0)
        .and_then(crate::harness_screen::HarnessSessionRow::state_line)
}

/// A BACKGROUND END IS A DONE FOR YOU ROW, AND `chrome` COUNTS IT (§11.4.1;
/// §11.4.5 "Lab", read off the real App): the tab's label, `landed`, a Go to
/// the session; the title `❯•`; `statusitem title=❯• … done=1`; the glance
/// owes its rebuild. NEGATIVE CONTROL: the same end in the tab looked at is
/// seen as it lands — no row, `done=0`, title `❯` — and 8 s of work is no
/// end at all.
#[test]
fn a_background_end_is_a_done_for_you_row_and_chrome_counts_it() {
    let (mut app, sid, host) = desk();
    app.take_status_rebuild();
    end_after(&mut app, &sid, 45 * SEC);
    assert!(app.end_unseen(0), "PRECONDITION: it stands unseen");
    assert!(
        app.take_status_rebuild(),
        "the end that marks dirties the glance"
    );
    let g = app.status_glance();
    assert_eq!(g.done.len(), 1, "{:?}", g.done);
    let row = &g.done[0];
    assert_eq!(row.label, app.tab_label_of_session(0));
    assert!(row.landed, "Land's end is a landing");
    assert_eq!(row.result_bytes, 0);
    let DoneAt::Go(at) = row.at else {
        panic!("an end's row goes to the session: {row:?}")
    };
    assert_eq!(g.go[usize::from(at)], 0);
    let face_ms = app
        .pool
        .get(0)
        .expect("session 0")
        .ctx
        .timeline
        .lock()
        .expect("timeline")
        .task_face();
    assert_eq!(
        face_ms.map(|(_, at)| u64::try_from(at).unwrap_or(0)),
        Some(row.since_unix_ms),
        "the instant the face moved to landed"
    );
    let line = app.status_item_chrome_line();
    assert!(
        line.starts_with("statusitem title=\u{276f}\u{2022} needs=0 stuck=0 ")
            && line.contains(" done=1 "),
        "{line}"
    );
    tell(&mut app, &sid, None);
    assert_eq!(app.status_glance().done, g.done, "kept through unknown");
    drop(host);

    let (mut front, sid, host) = desk();
    front.switch_tab_in(WindowId(0), 0);
    end_after(&mut front, &sid, 45 * SEC);
    assert!(!front.end_unseen(0), "seen as it lands");
    let g = front.status_glance();
    assert!(g.done.is_empty(), "{:?}", g.done);
    assert_eq!(g.button_title(), "\u{276f}");
    assert!(front.status_item_chrome_line().contains(" done=0 "));
    drop(host);

    let (mut quick, sid, host) = desk();
    end_after(&mut quick, &sid, 8 * SEC);
    assert!(quick.status_glance().done.is_empty(), "8 s: no end, no row");
    drop(host);
}

/// NOTHING ON THE ROW GOES STALE (§11.4.3, item 3; §11.4.5 "Nothing stale";
/// the review's R1): a glance is built with the end's row standing, then
/// acts arrive and the trail moves — the row and the fingerprint are as
/// they were, so nothing is handed over and nothing is redrawn. NEGATIVE
/// CONTROL, in-test: the row's state line on Settings ▸ Harness moved with
/// the act (`last: …`), so a row carrying those words changes its
/// fingerprint — the defect R1 names.
#[test]
fn an_act_and_a_trail_move_leave_the_end_row_as_it_was() {
    let (mut app, sid, host) = desk();
    end_after(&mut app, &sid, 45 * SEC);
    tell(&mut app, &sid, None);
    let before = app.status_glance();
    let line_before = state_line(&app);
    assert_eq!(before.done.len(), 1, "PRECONDITION: the row stands");
    app.apply_harness_fact(&sid, crate::HarnessFact::Act(Act::Approved));
    app.apply_harness_fact(&sid, crate::HarnessFact::Act(Act::Continued));
    app.apply_harness_fact(
        &sid,
        crate::HarnessFact::Pause(Some(crate::PausePlan {
            at_unix: 4_000_000_000,
            short: 3,
        })),
    );
    let after = app.status_glance();
    assert_eq!(after.done, before.done, "the row");
    assert_eq!(
        after.fingerprint(),
        before.fingerprint(),
        "nothing handed over"
    );
    let line_after = state_line(&app);
    assert_ne!(line_after, line_before, "PRECONDITION: the trail moved");
    let carrying = |g: &Glance, words: Option<&String>| {
        let mut g = g.clone();
        g.done[0].label = format!("{} \u{b7} {}", g.done[0].label, words.map_or("", |w| w));
        g.fingerprint()
    };
    assert_ne!(
        carrying(&after, line_after.as_ref()),
        carrying(&before, line_before.as_ref()),
        "a row with the state line's words goes stale"
    );
    drop(host);
}

/// GO MARKS THE END SEEN AND THE ROW GOES (§11.4.3, item 4; §11.4.5 "Go"):
/// the menu's click on the row — made from another app, with aterm's window
/// behind it (a status item click never activates aterm), so the tab switch
/// it makes is no look — resolved against the glance it was drawn from,
/// raises the session, marks its end seen, and the next glance has no row,
/// `done=0` and title `❯`; it owed that rebuild. NEGATIVE CONTROL (as run):
/// main's `note_session_done_seen` alone, which never marks the end, leaves
/// the row and fails this test. (Where the window is looked at, the switch
/// is the look by the one rule, whatever Go marks.)
#[test]
fn go_marks_the_end_seen_and_the_row_goes() {
    let (mut app, sid, host) = desk();
    end_after(&mut app, &sid, 45 * SEC);
    app.windows
        .get_mut(&WindowId(0))
        .expect("window 0")
        .occluded = true;
    let drawn = app.status_glance();
    let drawn = app.status_drawn_next(drawn);
    let DoneAt::Go(index) = drawn.done[0].at else {
        panic!("a Go row: {:?}", drawn.done)
    };
    app.take_status_rebuild();
    let pick = crate::status_item::Pick {
        generation: drawn.generation,
        index,
    };
    #[cfg(target_os = "macos")]
    app.dispatch_status_action(crate::status_item::StatusAction::Go(pick));
    #[cfg(not(target_os = "macos"))]
    {
        let session = app
            .status_pick(pick)
            .and_then(|g| g.go.get(usize::from(pick.index)).copied());
        app.go_to_session(session.expect("the row's session"));
    }
    assert!(!app.end_unseen(0), "Go is a look");
    assert!(app.take_status_rebuild(), "the look dirties the glance");
    let g = app.status_glance();
    assert!(g.done.is_empty(), "the row goes: {:?}", g.done);
    assert_eq!(g.button_title(), "\u{276f}");
    assert!(app.status_item_chrome_line().contains(" done=0 "));
    end_after(&mut app, &sid, 40 * SEC);
    assert_eq!(app.status_glance().done.len(), 1, "a new end is news");
    drop(host);
}

/// READING A WORKER'S RESULT IS A LOOK AT ITS END (§11.4.3, item 2; §11.2.4
/// "one `•` and one Done for you row"): a `ctl` worker under Land whose
/// task is done with a result and whose landing stands unseen is ONE row, the
/// task's Read Result. The menu's click on it — made from another app, with
/// aterm's window behind it — opens the result, and the session has nothing
/// left to look at: the next glance has no row (no Go row comes back for the
/// end), `done=0`, title `❯`, the tab's `•` down, and the glance owed that
/// rebuild. NEGATIVE CONTROL (as run, `controls/run.sh` `read-without-the-end`):
/// main's `read_status_result`, which marks the task's done alone, leaves
/// the end's Go row (`done=1`, `❯•`) and fails this test.
#[test]
fn reading_a_workers_result_is_a_look_at_its_end() {
    let (mut app, sid, host) = desk();
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-status-read-")
        .tempdir()
        .expect("exclusive result dir");
    let path = dir.path().join("result.md");
    std::fs::write(&path, "# fleet summary\n").expect("result");
    for state in [
        crate::task_record::TaskState::Working,
        crate::task_record::TaskState::Done,
    ] {
        let mut mark = crate::task_record::TaskMark::new(state);
        if state == crate::task_record::TaskState::Done {
            mark.result_bytes = 1434;
            mark.result_path = Some(path.clone());
        }
        crate::task_record::mark_session_task(&ctx, 0, &mark).expect("marked");
        app.on_task_changed(0);
    }
    let unended = app
        .status_glance()
        .reads
        .into_iter()
        .next()
        .expect("result");
    assert_eq!(unended.end_serial, None, "no end was drawn yet");
    end_after(&mut app, &sid, 45 * SEC);
    assert!(app.end_unseen(0), "PRECONDITION: the end stands unseen");
    app.windows
        .get_mut(&WindowId(0))
        .expect("window 0")
        .occluded = true;
    let drawn = app.status_glance();
    let drawn = app.status_drawn_next(drawn);
    assert_eq!(drawn.done.len(), 1, "one row: {:?}", drawn.done);
    let DoneAt::Read(index) = drawn.done[0].at else {
        panic!("the task's Read Result row: {:?}", drawn.done)
    };
    let entered = drawn.reads[usize::from(index)].entered;
    let first_end = drawn.reads[usize::from(index)]
        .end_serial
        .expect("the result shared this end");
    app.note_status_painted(Some(drawn.clone()));
    app.take_status_rebuild();
    let pick = crate::status_item::Pick {
        generation: drawn.generation,
        index,
    };
    #[cfg(target_os = "macos")]
    app.dispatch_status_action(crate::status_item::StatusAction::ReadResult(pick));
    #[cfg(not(target_os = "macos"))]
    {
        let result = app
            .status_pick(pick)
            .and_then(|g| g.reads.get(usize::from(pick.index)).cloned());
        app.read_status_result(&result.expect("the row's result"));
    }
    assert_eq!(
        app.done_seen.get(&0),
        Some(&entered),
        "PRECONDITION: the result opened and its done is seen"
    );
    assert!(app.take_status_rebuild(), "the read dirties the glance");
    let g = app.status_glance();
    assert!(
        g.done.is_empty(),
        "no row comes back for the end: {:?}",
        g.done
    );
    assert_eq!(g.button_title(), "\u{276f}");
    assert!(app.status_item_chrome_line().contains(" done=0 "));
    assert!(!app.end_unseen(0), "the tab's `•` goes with the row");

    end_after(&mut app, &sid, 40 * SEC);
    let later = app.harness_glass[&0].end().expect("a later end").serial;
    assert_ne!(later, first_end, "PRECONDITION: a new end");
    assert!(app.end_unseen(0));
    app.read_status_result(&unended);
    assert!(
        app.end_unseen(0),
        "a result drawn before any end sees no later end"
    );
    let current = app.status_glance();
    let _ = app.status_drawn_next(current.clone());
    let _ = app.status_drawn_next(current);
    let stale = app
        .status_pick(pick)
        .and_then(|g| g.reads.get(usize::from(pick.index)).cloned())
        .expect("the painted menu still owns its old result");
    assert_eq!(stale.entered, entered, "the captured task done");
    assert_eq!(stale.end_serial, Some(first_end), "the captured end");
    app.read_status_result(&stale);
    assert!(
        app.end_unseen(0),
        "reading the old result leaves the later end unseen"
    );
    assert_eq!(app.harness_glass[&0].end().expect("kept").serial, later);
    let g = app.status_glance();
    assert_eq!(g.done.len(), 1, "the later end remains: {:?}", g.done);
    assert!(matches!(g.done[0].at, DoneAt::Go(_)));
    drop(host);
}

/// A RENAME RENAMES THE ROW (§11.4.3, item 3): the glance is built again
/// when the tab's title moves, and the row says the new name.
#[test]
fn a_rename_renames_the_end_row() {
    let (mut app, sid, host) = desk();
    end_after(&mut app, &sid, 45 * SEC);
    app.take_status_rebuild();
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    ctx.meta
        .lock()
        .expect("meta")
        .set("title", Some("proj-api refactor".into()));
    app.on_meta_changed(0);
    assert!(app.take_status_rebuild(), "the rename dirties the glance");
    let g = app.status_glance();
    assert_eq!(g.done.len(), 1);
    assert_eq!(g.done[0].label, "proj-api refactor");
    drop(host);
}
