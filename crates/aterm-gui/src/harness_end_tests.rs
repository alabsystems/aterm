// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ONE END, THE TAB'S `•` AND THE LOOK THAT QUIETS IT
//! (`docs/DESIGN-harness-autopilot-2026-10-15.md` §11.2; the tests of
//! §11.2.5 but the carry), under Autopilot Cruise / Land,
//! where the one end is Land's landing and Cruise never ends:
//! `is_a_task_end`'s table; a background session's landing after 45 s of the
//! agent's busy time, fed through the A/B book as the status sweep feeds it
//! (`App::note_harness_phase`), wears the tab's mark and is counted; the same
//! landing in the tab someone looks at is seen as it lands; a landed face
//! told under Cruise marks nothing; the one looking rule quiets it, with one
//! strip re-stamp, and never while nobody can look; the menu bar's Go quiets
//! it; a loop's restart zeroes no work, and the landing seed withdrawn over
//! a carry-on's reply counts none of that reply's; an unseen end at rest
//! arms nothing.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_agent::harness::acts::TaskFace;

use super::*;
use crate::WindowId;
use crate::presence::ChipLevel;

const SEC: u64 = 1_000;

/// A host that supervises nothing: what the App's posture needs to read
/// `On`, shut down by the caller.
fn idle_host() -> crate::harness_host::HostHandle {
    crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    )
}

/// An App whose session 0 runs Claude Code under its own word `word`,
/// claimed by this host (the word's arm), in window 0's first tab — with a
/// second tab in front where `behind` — and window 0 looked at (the
/// frontmost, on screen). Its sid, and the host to shut down.
fn desk(word: &str, behind: bool) -> (App, String, crate::harness_host::HostHandle) {
    let mut app = App::headless_for_test();
    let host = idle_host();
    app.harness = Some(host.clone());
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    {
        let mut tl = ctx.timeline.lock().unwrap();
        tl.note_foreground_group(7);
        tl.set_program(7, Some("claude".into()));
    }
    ctx.meta.lock().unwrap().set("autopilot", Some(word.into()));
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .unwrap();
    if behind {
        let id = app.next_session_id;
        app.push_stub_tab(WindowId(0), crate::stub_session(id));
    }
    look(&mut app, true);
    (app, ctx.self_id.as_str().to_string(), host)
}

/// Window 0 looked at (`on`: frontmost and on screen) or not (on no
/// screen: headless, as the lab's window is).
fn look(app: &mut App, on: bool) {
    app.frontmost_window = Some(WindowId(0));
    app.windows
        .get_mut(&WindowId(0))
        .unwrap()
        .band_on_screen_for_test = on;
}

/// The agent word the status sweep publishes for session `session`.
fn publish(app: &App, session: u64, word: &'static str) -> bool {
    app.pool
        .get(session)
        .unwrap()
        .ctx
        .timeline
        .lock()
        .unwrap()
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

/// Session `session`'s agent goes busy, works `ms` on the ledger clock (moved
/// on, not waited), and sits at its prompt: both words fed to the A/B book
/// as the sweep feeds them.
fn work(app: &mut App, session: u64, ms: u64) {
    assert!(publish(app, session, "busy"), "busy published");
    app.note_harness_phase(session);
    crate::turn_ledger::advance_for_test(ms);
    assert!(publish(app, session, "idle"), "idle published");
    app.note_harness_phase(session);
}

/// Session `sid`'s loop tells `face` through the host's wake.
fn tell(app: &mut App, sid: &str, face: Option<TaskFace>) {
    app.apply_harness_fact(sid, crate::HarnessFact::Task(face));
}

/// Every tab's mark in window 0, as its strip is stamped.
fn marks(app: &App) -> Vec<ChipLevel> {
    let ws = &app.windows[&WindowId(0)];
    let mut metadata: Vec<crate::tab_bar::TabStripMetadata> = ws
        .tab_set
        .tabs()
        .iter()
        .map(|t| crate::tab_bar::TabStripMetadata::from_presentation(&t.presentation))
        .collect();
    app.stamp_presence_chips(WindowId(0), &mut metadata);
    metadata.iter().map(|m| m.attention).collect()
}

/// The ends the book counted this week.
fn ends(app: &App) -> crate::harness_ab::AbEnds {
    app.harness_screen
        .ab
        .week_at(crate::turn_ledger::now_ms(), &[])
        .ends
}

/// THE ONE END IS ONE TABLE (§11.2.3, item 1): landed, from a face the
/// window knew (open, theirs), after at least 30 s of work; every other move
/// is none — Cruise has no end to tell. NEGATIVE CONTROLS, each a row that
/// must be none or must be an end: unknown → landed (the update path, a
/// successor's first face); open → landed after 8 s of work; open → theirs;
/// landed → open; and the floor's own edge, 30 s an end and a millisecond
/// less none (its comparison flipped fails one of the two).
#[test]
fn an_end_is_a_landing_from_a_known_face_past_the_floor() {
    use TaskFace::{Landed, Open, Theirs};
    assert_eq!(END_WORK_MS, 30 * SEC, "the pet's own line");
    let rows: [(Option<TaskFace>, TaskFace, u64, Option<EndFace>); 11] = [
        (Some(Open), Landed, 45 * SEC, Some(EndFace::Landed)),
        (Some(Theirs), Landed, 45 * SEC, Some(EndFace::Landed)),
        (Some(Open), Landed, END_WORK_MS, Some(EndFace::Landed)),
        (Some(Open), Landed, END_WORK_MS - 1, None),
        (Some(Open), Landed, 8 * SEC, None),
        (None, Landed, 45 * SEC, None),
        (Some(Landed), Landed, 45 * SEC, None),
        (Some(Open), Theirs, 45 * SEC, None),
        (Some(Open), Open, 45 * SEC, None),
        (Some(Landed), Open, 45 * SEC, None),
        (Some(Theirs), Open, 45 * SEC, None),
    ];
    for (was, face, worked, want) in rows {
        assert_eq!(
            is_a_task_end(was, face, worked),
            want,
            "{was:?} → {face:?} after {worked} ms"
        );
    }
}

/// THE TAB'S `•` FOR A LANDING NOBODY SAW (§11.2.1; §11.2.5 "The `•`"): two
/// tabs, the background session under Land told open, 45 s of its agent's
/// busy time, then landed — its tab wears the mark (`"done"` among its
/// chrome states), and the page's Ends row counts one landing. The chip still
/// says landed. NEGATIVE CONTROLS: the same landing in the front tab of the
/// frontmost on-screen window is seen as it lands — no mark, even once
/// another tab comes in front — and is still counted; 8 s of work marks
/// nothing, though the face is landed and the landing counted (the record's
/// rule has no floor); and a landed face told under Cruise, which never
/// ends, marks nothing and counts nothing.
#[test]
fn a_background_landing_wears_the_mark_and_a_looked_at_one_none() {
    let (mut app, sid, host) = desk("land", true);
    tell(&mut app, &sid, Some(TaskFace::Open));
    work(&mut app, 0, 45 * SEC);
    assert_eq!(marks(&app)[0], ChipLevel::Off, "working is no mark");
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert_eq!(marks(&app), [ChipLevel::Done, ChipLevel::Off]);
    assert!(ChipLevel::Done.chrome_states().contains(&"done"));
    // `aterm ctl chrome` where no native strip draws (headless): the line the
    // strip would show, the mark among the background tab's states.
    let line = app.stripless_tab_chrome().expect("the tab line");
    assert!(
        line.contains(r#"selected=1 "#) && line.contains(r#"states=[["done"], ["selected"]]"#),
        "{line}"
    );
    assert_eq!(ends(&app).landed, 1, "the landing counted");
    assert_eq!(app.chip_glass(0).task, Some(TaskFace::Landed));
    let end = app.harness_glass[&0].end().expect("the end");
    assert_eq!((end.face, end.serial, end.seen), (EndFace::Landed, 1, 0));
    assert_eq!(end.at, app.chip_glass(0).task_at, "the face's own clock");
    // Unknown between loop runs keeps it: no flicker.
    tell(&mut app, &sid, None);
    assert_eq!(marks(&app)[0], ChipLevel::Done, "kept through unknown");
    host.shutdown_and_join();

    // NEGATIVE CONTROL: in the tab someone looks at.
    let (mut front, sid, host) = desk("land", false);
    tell(&mut front, &sid, Some(TaskFace::Open));
    work(&mut front, 0, 45 * SEC);
    crate::work_counts::take_end_restamps();
    tell(&mut front, &sid, Some(TaskFace::Landed));
    assert!(!front.end_unseen(0), "seen as it lands");
    assert_eq!(
        crate::work_counts::take_end_restamps(),
        0,
        "nothing was drawn"
    );
    assert_eq!(ends(&front).landed, 1, "counted all the same");
    let id = front.next_session_id;
    front.push_stub_tab(WindowId(0), crate::stub_session(id));
    assert_eq!(marks(&front), [ChipLevel::Off, ChipLevel::Off]);
    let line = front.stripless_tab_chrome().expect("the tab line");
    assert!(!line.contains(r#""done""#), "{line}");
    host.shutdown_and_join();

    // NEGATIVE CONTROL: under the floor.
    let (mut quick, sid, host) = desk("land", true);
    tell(&mut quick, &sid, Some(TaskFace::Open));
    work(&mut quick, 0, 8 * SEC);
    tell(&mut quick, &sid, Some(TaskFace::Landed));
    assert_eq!(quick.chip_glass(0).task, Some(TaskFace::Landed), "landed");
    assert_eq!(marks(&quick)[0], ChipLevel::Off, "8 s: no mark");
    assert_eq!(ends(&quick).landed, 1, "the landing still counted");
    host.shutdown_and_join();

    // NEGATIVE CONTROL: Cruise never ends.
    let (mut cruise, sid, host) = desk("cruise", true);
    tell(&mut cruise, &sid, Some(TaskFace::Open));
    work(&mut cruise, 0, 45 * SEC);
    tell(&mut cruise, &sid, Some(TaskFace::Landed));
    assert_eq!(marks(&cruise)[0], ChipLevel::Off, "Cruise marks nothing");
    assert!(!cruise.end_unseen(0));
    assert_eq!(ends(&cruise).landed, 0, "and counts nothing");
    host.shutdown_and_join();
}

/// A LANDING IS SEEN BY THE ONE LOOKING RULE (§11.2.1 "It goes when he
/// looks"; §11.2.5 "Seen"): a switch to its tab quiets it with exactly one
/// strip re-stamp; away and back does not mark it again; a second landing
/// does; while the frontmost window is occluded, a switch quiets nothing;
/// the menu bar's Go quiets it; someone else's turn (theirs) clears it and
/// its mark. NEGATIVE CONTROLS (as run on main, each failing this test):
/// `note_done_seen` with its two older sources alone (the end never seen:
/// the mark comes back when the tab goes behind); a looking rule that reads
/// focus alone (the occluded window quiets it).
#[test]
fn a_landing_is_seen_by_the_one_looking_rule_and_marks_again_when_new() {
    let (mut app, sid, host) = desk("land", true);
    tell(&mut app, &sid, Some(TaskFace::Open));
    work(&mut app, 0, 45 * SEC);
    crate::work_counts::take_end_restamps();
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert_eq!(crate::work_counts::take_end_restamps(), 1, "the end's");
    assert_eq!(marks(&app)[0], ChipLevel::Done);
    // The switch to it.
    app.switch_tab_in(WindowId(0), 0);
    assert_eq!(crate::work_counts::take_end_restamps(), 1, "the look's");
    assert!(!app.end_unseen(0));
    // Away, and back again.
    app.switch_tab_in(WindowId(0), 1);
    assert_eq!(
        marks(&app),
        [ChipLevel::Off, ChipLevel::Off],
        "seen stays seen"
    );
    app.switch_tab_in(WindowId(0), 0);
    app.switch_tab_in(WindowId(0), 1);
    assert_eq!(crate::work_counts::take_end_restamps(), 0, "nothing new");
    assert_eq!(marks(&app)[0], ChipLevel::Off);
    // A second landing: someone's turn opened a task, more work, landed.
    tell(&mut app, &sid, Some(TaskFace::Open));
    work(&mut app, 0, 40 * SEC);
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert_eq!(marks(&app)[0], ChipLevel::Done, "a new landing is news");
    assert_eq!(app.harness_glass[&0].end().map(|e| e.serial), Some(2));
    // Occluded: nobody can look, so the switch quiets nothing.
    app.windows.get_mut(&WindowId(0)).unwrap().occluded = true;
    crate::work_counts::take_end_restamps();
    app.switch_tab_in(WindowId(0), 0);
    app.switch_tab_in(WindowId(0), 1);
    assert!(app.end_unseen(0), "an occluded window sees nothing");
    assert_eq!(marks(&app)[0], ChipLevel::Done);
    assert_eq!(crate::work_counts::take_end_restamps(), 0);
    // The menu bar's Go.
    app.note_session_done_seen(0);
    assert!(!app.end_unseen(0), "Go is a look");
    assert_eq!(crate::work_counts::take_end_restamps(), 1);
    assert_eq!(marks(&app)[0], ChipLevel::Off);
    // A third landing, cleared by someone else's turn before anyone looked.
    tell(&mut app, &sid, Some(TaskFace::Open));
    work(&mut app, 0, 40 * SEC);
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert_eq!(marks(&app)[0], ChipLevel::Done);
    crate::work_counts::take_end_restamps();
    tell(&mut app, &sid, Some(TaskFace::Theirs));
    assert_eq!(
        app.harness_glass[&0].end(),
        None,
        "an end never outlives its work"
    );
    assert_eq!(marks(&app)[0], ChipLevel::Off);
    assert_eq!(
        crate::work_counts::take_end_restamps(),
        1,
        "its mark taken down"
    );
    host.shutdown_and_join();
}

/// A LOOP'S RESTART ZEROES NO WORK (§11.2.3, item 2, "What never zeroes
/// it"; the review's R12): 25 s of work, the loop restarted (unknown, then
/// open restated from unknown), 10 s more, then landed — 35 s since the task
/// opened, an end. NEGATIVE CONTROLS: zeroing at a move from unknown fails
/// this test (as run on main: 10 s, no end); in-test, the same 25 s then a
/// move from a face the window knew to open (a new task), 10 s and landed,
/// is no end.
#[test]
fn a_loops_restart_zeroes_no_work() {
    let (mut app, sid, host) = desk("land", true);
    tell(&mut app, &sid, Some(TaskFace::Open));
    work(&mut app, 0, 25 * SEC);
    tell(&mut app, &sid, None);
    tell(&mut app, &sid, Some(TaskFace::Open));
    work(&mut app, 0, 10 * SEC);
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert_eq!(marks(&app)[0], ChipLevel::Done, "25 s + 10 s");
    host.shutdown_and_join();

    // CONTROL: a move from a known face opens a new task.
    let (mut app, sid, host) = desk("land", true);
    tell(&mut app, &sid, Some(TaskFace::Open));
    work(&mut app, 0, 25 * SEC);
    tell(&mut app, &sid, Some(TaskFace::Theirs));
    tell(&mut app, &sid, Some(TaskFace::Open));
    work(&mut app, 0, 10 * SEC);
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert_eq!(marks(&app)[0], ChipLevel::Off, "10 s since the task opened");
    host.shutdown_and_join();
}

/// THE CLIP (§11.2.3, item 3; the review's R2): a relaunch over a landed
/// task restates the landing from unknown (the landing seed: no end), a
/// reply runs 40 s, and its point WITHDRAWS the seed — open, then a landing,
/// at one point. The reply's busy time ran before the open it caused, so it
/// counts none of it: no mark. Nor where the published word moves after the
/// loop's point (only the lag counts). NEGATIVE CONTROLS: the clip removed
/// (no zeroing at the withdrawal's open) arms an end — as run on main, this
/// test fails; in-test, a reply that moved on (the seed dropped: open)
/// BEFORE 40 s of real work, then landed, is an end.
#[test]
fn a_withdrawn_landing_seed_counts_none_of_the_replys_work() {
    // The reply's word moves before the loop's point.
    let (mut app, sid, host) = desk("land", true);
    tell(&mut app, &sid, Some(TaskFace::Landed));
    work(&mut app, 0, 40 * SEC);
    tell(&mut app, &sid, Some(TaskFace::Open));
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert_eq!(marks(&app)[0], ChipLevel::Off, "the clip: no end");
    assert!(!app.end_unseen(0));
    host.shutdown_and_join();

    // The published word moves after the loop's point.
    let (mut app, sid, host) = desk("land", true);
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert!(publish(&app, 0, "busy"));
    app.note_harness_phase(0);
    crate::turn_ledger::advance_for_test(40 * SEC);
    tell(&mut app, &sid, Some(TaskFace::Open));
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert!(publish(&app, 0, "idle"));
    app.note_harness_phase(0);
    assert_eq!(marks(&app)[0], ChipLevel::Off, "the lag alone");
    host.shutdown_and_join();

    // CONTROL: the seed dropped first, then real work, then landed.
    let (mut app, sid, host) = desk("land", true);
    tell(&mut app, &sid, Some(TaskFace::Landed));
    tell(&mut app, &sid, Some(TaskFace::Open));
    work(&mut app, 0, 40 * SEC);
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert_eq!(marks(&app)[0], ChipLevel::Done, "work after the opening");
    host.shutdown_and_join();
}

/// AN UNSEEN END AT REST ARMS NOTHING (§11.2.3, item 7; §11.2.5 "Idle"): with
/// a landing standing unseen behind the front tab, 20 s on, presence folds no
/// deadline, the harness's glass none and the card none — as before the end.
/// NEGATIVE CONTROL (as run on main): a variant that arms a redraw for an
/// unseen end (presence's deadline while one stands) fails this test.
#[test]
fn an_unseen_end_at_rest_arms_nothing() {
    let (mut app, sid, host) = desk("land", true);
    tell(&mut app, &sid, Some(TaskFace::Open));
    work(&mut app, 0, 45 * SEC);
    let later = Instant::now() + Duration::from_secs(20);
    let before = (
        app.presence_deadline(later),
        app.harness_acts_deadline(),
        app.claude_card_wake(),
    );
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert!(app.end_unseen(0), "PRECONDITION: it stands");
    let after = (
        app.presence_deadline(later),
        app.harness_acts_deadline(),
        app.claude_card_wake(),
    );
    assert_eq!(after, before, "as before the end");
    assert_eq!(after.0, None, "presence folds nothing");
    host.shutdown_and_join();
}

/// A LANDING'S NEXT TASK IS TIMED FROM ITS OWN OPENING (§11.2.3, item 2;
/// §11.6.3, item 4): open → landed after 45 s wears the mark and counts one
/// landing; a new task opened (landed → open) restarts the time from Land to
/// landed there, and the open takes the mark down. NEGATIVE CONTROL: under
/// Cruise an opening starts no span.
#[test]
fn a_landings_next_task_is_timed_from_its_opening() {
    let (mut app, sid, host) = desk("land", true);
    tell(&mut app, &sid, Some(TaskFace::Open));
    work(&mut app, 0, 45 * SEC);
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert_eq!(marks(&app)[0], ChipLevel::Done);
    assert_eq!(ends(&app).landed, 1);
    let opened = crate::turn_ledger::now_ms();
    tell(&mut app, &sid, Some(TaskFace::Open));
    assert!(
        app.harness_screen
            .ab
            .land_from(0)
            .is_some_and(|from| from >= opened),
        "the span starts at the task's opening"
    );
    assert_eq!(marks(&app)[0], ChipLevel::Off, "open clears the end");
    host.shutdown_and_join();

    // CONTROL: under Cruise.
    let (mut app, sid, host) = desk("cruise", true);
    tell(&mut app, &sid, Some(TaskFace::Open));
    work(&mut app, 0, 45 * SEC);
    tell(&mut app, &sid, Some(TaskFace::Theirs));
    tell(&mut app, &sid, Some(TaskFace::Open));
    assert_eq!(app.harness_screen.ab.land_from(0), None, "no span");
    host.shutdown_and_join();
}

/// OFF THEN LAND OVER WORK NO LOOP WATCHED ARMS NOTHING (§11.2.4
/// "Restatements arm nothing"; "Off, the agent's own switch, the master
/// switch"): a task open under Land with 5 s of work, the session set Off
/// (the loop stops, its claim given back), the person's own 40 s turn, then
/// Land again — the loop restates open from unknown, and a landing follows.
/// The work was the person's: the chip says landed and the landing is
/// counted (the record's rule has no floor), but no mark. NEGATIVE CONTROLS:
/// Off's busy time counted (the arm not read in the work sum's adds, as
/// built first on main) marks the tab, so this fails; in-test, the same 40 s
/// turn under Land, no Off, marks.
#[test]
fn off_then_land_over_work_no_loop_watched_arms_nothing() {
    let (mut app, sid, host) = desk("land", true);
    tell(&mut app, &sid, Some(TaskFace::Open));
    work(&mut app, 0, 5 * SEC);
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    ctx.meta
        .lock()
        .unwrap()
        .set("autopilot", Some("off".into()));
    assert!(crate::session_timeline::release_supervisor(
        &ctx,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl
    ));
    tell(&mut app, &sid, None);
    app.note_harness_arm(0);
    assert_eq!(
        app.harness_ab_arm(0),
        crate::harness_ab::AbArm::Off,
        "PRECONDITION: Off"
    );
    work(&mut app, 0, 40 * SEC);
    ctx.meta
        .lock()
        .unwrap()
        .set("autopilot", Some("land".into()));
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .unwrap();
    app.note_harness_arm(0);
    tell(&mut app, &sid, Some(TaskFace::Open));
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert_eq!(app.chip_glass(0).task, Some(TaskFace::Landed), "landed");
    assert_eq!(marks(&app)[0], ChipLevel::Off, "Off's work is the person's");
    assert_eq!(ends(&app).landed, 1, "the landing counted");
    host.shutdown_and_join();

    // CONTROL: the same turn under Land.
    let (mut app, sid, host) = desk("land", true);
    tell(&mut app, &sid, Some(TaskFace::Open));
    work(&mut app, 0, 5 * SEC);
    tell(&mut app, &sid, None);
    work(&mut app, 0, 40 * SEC);
    tell(&mut app, &sid, Some(TaskFace::Open));
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert_eq!(marks(&app)[0], ChipLevel::Done, "Land's work marks");
    host.shutdown_and_join();
}

/// A SETTINGS TAB CLOSED BEHIND LEAVES THE TAB IN FRONT IN FRONT, AND ITS
/// NEIGHBOUR'S END UNSEEN (the final check of 2026-10-20, W2): tabs [the tab
/// in front, a background landing, Settings] in a looked-at window, the
/// Settings tab closed by index as the strip's ✕, the tab menu and `ctl tab
/// close` close it. The front tab stays in front, and the landing keeps its
/// `•` — never brought to the front by the close and seen there. NEGATIVE
/// CONTROLS: a close of Settings while it IS in front still hands the front
/// to its neighbour (the removal's rule), which sees the landing; and on
/// head without the keep, the background close selected the landing's tab
/// and quieted its end.
#[test]
fn closing_a_settings_tab_behind_keeps_the_front_and_the_neighbours_end_unseen() {
    let set_up = || {
        let (mut app, sid, host) = desk("land", true);
        // [the tab in front, the landed session's tab]: the stub to index 0.
        app.move_tab(WindowId(0), 1, 0);
        tell(&mut app, &sid, Some(TaskFace::Open));
        work(&mut app, 0, 45 * SEC);
        tell(&mut app, &sid, Some(TaskFace::Landed));
        assert!(
            app.open_settings_tab(crate::native_settings::SettingsRoute::Home),
            "Settings opens"
        );
        assert_eq!(app.windows[&WindowId(0)].tab_set.len(), 3);
        assert!(app.end_unseen(0), "PRECONDITION: the landing stands unseen");
        (app, host)
    };
    let front_of = |app: &App| app.windows[&WindowId(0)].tab_set.active_index();

    let (mut app, host) = set_up();
    app.switch_tab_in(WindowId(0), 0);
    let front = app.windows[&WindowId(0)].tab_set.active_id();
    assert_eq!(front_of(&app), Some(0));
    assert!(app.end_unseen(0), "PRECONDITION: still unseen behind");
    assert!(
        !app.close_tab_at(WindowId(0), 2),
        "not the window's last tab"
    );
    assert_eq!(
        app.windows[&WindowId(0)].tab_set.len(),
        2,
        "Settings closed"
    );
    assert_eq!(
        (
            app.windows[&WindowId(0)].tab_set.active_id(),
            app.end_unseen(0)
        ),
        (front, true),
        "the tab in front stays in front; the neighbour's landing is not seen"
    );
    assert_eq!(
        marks(&app),
        [ChipLevel::Off, ChipLevel::Done],
        "its • stands"
    );
    host.shutdown_and_join();

    // NEGATIVE CONTROL: Settings in front, closed — its neighbour comes to
    // the front, and a looked-at window sees the landing there.
    let (mut app, host) = set_up();
    assert_eq!(front_of(&app), Some(2), "PRECONDITION: Settings in front");
    assert!(!app.close_tab_at(WindowId(0), 2));
    assert_eq!(front_of(&app), Some(1), "the neighbour's turn");
    assert!(!app.end_unseen(0), "looked at, seen");
    host.shutdown_and_join();
}
