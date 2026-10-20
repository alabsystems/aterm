// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LANDING'S ENDS in the window (`docs/DESIGN-harness-autopilot-2026-10-15.md`
//! §9.4): the five faces on the chip under Land and none under Cruise or Off,
//! the records of a landing landed or spent and of nothing else, and a
//! person's own exchange typed through the App's input after a landing,
//! read back as `status task=theirs`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_agent::harness::acts::TaskFace;
use aterm_agent::supervise::Autopilot;
use aterm_agent::supervise::SupervisorConfig;
use aterm_agent::supervise::policy::turn_end::{
    Composer, TurnEndReading, TurnEndState, decide_turn_end,
};

use super::*;
use crate::claude_card::LineEnd;
use crate::claude_lights::ChipFace;

/// A host that supervises nothing: what the App's posture needs to read
/// `On`, shut down by the caller.
fn idle_host() -> crate::harness_host::HostHandle {
    crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    )
}

/// A headless App whose session 0 runs Claude Code under a host, its own
/// word `word`: its sid.
fn under(app: &mut App, word: &str) -> String {
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    {
        let mut tl = ctx.timeline.lock().unwrap();
        tl.note_foreground_group(7);
        tl.set_program(7, Some("claude".into()));
    }
    ctx.meta.lock().unwrap().set("autopilot", Some(word.into()));
    ctx.self_id.as_str().to_string()
}

/// The session's records of a landing's end, oldest first: title and lines.
fn landing_records(app: &App, sid: &str) -> Vec<(String, Vec<String>)> {
    let key = format!("harness.landing.{sid}");
    app.messages
        .log()
        .records()
        .filter(|r| r.key.as_deref() == Some(key.as_str()))
        .map(|r| (r.title.clone(), r.detail.clone()))
        .collect()
}

/// UNDER LAND, A LANDING THAT LANDED OR WAS SPENT IS A RECORD, AND NOTHING
/// ELSE IS (§9.4.1, §9.4.3): `Autopilot landed in tab 1` and `Autopilot quiet
/// in tab 1`, each led by its tab and saying what the harness does now; the
/// same face told again is no move and no second record; open, done, theirs
/// and unknown are none. NEGATIVE CONTROLS: the same moves under Cruise and
/// Off record nothing — the chip draws no face there either.
#[test]
fn a_landing_landed_or_spent_under_land_is_a_record_and_no_other_move_is() {
    let mut app = App::headless_for_test();
    let host = idle_host();
    app.harness = Some(host.clone());
    let sid = under(&mut app, "land");
    let tell = |app: &mut App, face: Option<TaskFace>| {
        app.apply_harness_fact(&sid, crate::HarnessFact::Task(face));
    };
    tell(&mut app, Some(TaskFace::Open));
    assert!(landing_records(&app, &sid).is_empty(), "open is no end");
    tell(&mut app, Some(TaskFace::Landed));
    tell(&mut app, Some(TaskFace::Landed));
    assert_eq!(
        landing_records(&app, &sid),
        [(
            "Autopilot landed in tab 1".to_string(),
            vec![
                "Tab 1: the agent finished after the land instruction; the harness types \
                 nothing more until someone else's turn"
                    .to_string()
            ]
        )],
        "once: told again is no move"
    );
    for face in [
        Some(TaskFace::Open),
        Some(TaskFace::Theirs),
        Some(TaskFace::Done),
        None,
        Some(TaskFace::Open),
    ] {
        tell(&mut app, face);
    }
    assert_eq!(
        landing_records(&app, &sid).len(),
        1,
        "none of those is recorded"
    );
    tell(&mut app, Some(TaskFace::Spent));
    let records = landing_records(&app, &sid);
    assert_eq!(records.len(), 2);
    assert_eq!(records[1].0, "Autopilot quiet in tab 1");
    assert_eq!(
        records[1].1,
        [
            "Tab 1: no done reply after the land instruction and 2 nudges; the harness is \
          quiet, but for a land answer to a question"
        ]
    );
    for (title, _) in &records {
        assert_eq!(
            aterm_messages::text::glass_title_fault(title),
            None,
            "{title}"
        );
    }
    // NEGATIVE CONTROLS: Cruise and Off record no move.
    for word in ["cruise", "off"] {
        let mut app = App::headless_for_test();
        app.harness = Some(host.clone());
        let sid = under(&mut app, word);
        for face in [
            TaskFace::Open,
            TaskFace::Landed,
            TaskFace::Open,
            TaskFace::Spent,
        ] {
            app.apply_harness_fact(&sid, crate::HarnessFact::Task(Some(face)));
        }
        assert!(landing_records(&app, &sid).is_empty(), "{word}");
    }
    host.shutdown_and_join();
}

/// Session `sid`'s loop tells `face` through the host's wake.
fn tell(app: &mut App, sid: &str, face: Option<TaskFace>) {
    app.apply_harness_fact(sid, crate::HarnessFact::Task(face));
}

/// A RESTARTED LANDING IS RESTATED, NOT RECORDED AGAIN (the review of slice
/// C): the host tells "unknown" as a loop ends, and the next loop of a landed
/// session starts LANDED from its seed — a restart, a policy change, a
/// panicked body, an Off and back, each the same moves. The window writes no
/// second `Autopilot landed` record, and the landed time stays the landing's
/// own (the timeline's, and the clock the chip and Settings ▸ Harness read,
/// both on the wall clock), not the restart's. A seamless update's successor knew no face: its
/// window's first face, landed, is no record either. NEGATIVE CONTROLS: the
/// seed dropped (open), a landing after it is new — recorded, at its own
/// time; and unknown then open then landed (a loop with no seed that landed
/// again) is a new landing too.
#[test]
fn a_restarted_landing_is_restated_with_its_first_time_and_recorded_once() {
    let mut app = App::headless_for_test();
    let host = idle_host();
    app.harness = Some(host.clone());
    let sid = under(&mut app, "land");
    tell(&mut app, &sid, Some(TaskFace::Open));
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert_eq!(landing_records(&app, &sid).len(), 1);
    // The move is stamped on the WALL CLOCK (unix ms), as the page's
    // `landed 17:42` reads it — not the process's age.
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    let now = aterm_agent::supervise::journal::unix_ms();
    let (face, at) = ctx.timeline.lock().unwrap().task_face().unwrap();
    assert_eq!(face, TaskFace::Landed);
    assert!(
        (now - at).abs() < 60_000,
        "stamped on the wall clock: {at} against {now}"
    );
    assert_eq!(
        app.chip_glass(0).task_at,
        crate::harness_glass::Stamp::at(at.div_euclid(1000))
    );
    // The landing three hours back, so a restart's clock would show.
    let old = now - 3 * 3_600_000;
    {
        let mut tl = ctx.timeline.lock().unwrap();
        tl.set_task_face(Some(TaskFace::Open), old);
        tl.set_task_face(Some(TaskFace::Landed), old);
    }
    let face_at = || ctx.timeline.lock().unwrap().task_face();
    for restart in 0..3 {
        tell(&mut app, &sid, None);
        assert_eq!(face_at(), None, "{restart}: unknown");
        tell(&mut app, &sid, Some(TaskFace::Landed));
        assert_eq!(
            landing_records(&app, &sid).len(),
            1,
            "{restart}: restated, not recorded again"
        );
        assert_eq!(face_at(), Some((TaskFace::Landed, old)), "{restart}");
        assert_eq!(
            app.chip_glass(0).task_at,
            crate::harness_glass::Stamp::at(old.div_euclid(1000)),
            "{restart}: the chip's and the page's clock"
        );
    }
    // A seamless update's successor: a new window, no face known.
    let mut fresh = App::headless_for_test();
    fresh.harness = Some(host.clone());
    let fresh_sid = under(&mut fresh, "land");
    tell(&mut fresh, &fresh_sid, Some(TaskFace::Landed));
    assert_eq!(fresh.chip_glass(0).task, Some(TaskFace::Landed));
    assert!(landing_records(&fresh, &fresh_sid).is_empty());
    // NEGATIVE CONTROLS: the seed dropped, then a new landing.
    tell(&mut app, &sid, Some(TaskFace::Open));
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert_eq!(landing_records(&app, &sid).len(), 2, "a new landing");
    assert_ne!(face_at().map(|(_, at)| at), Some(old), "its own time");
    // …and a loop with no seed that landed again.
    tell(&mut app, &sid, None);
    tell(&mut app, &sid, Some(TaskFace::Open));
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert_eq!(landing_records(&app, &sid).len(), 3, "a new landing");
    host.shutdown_and_join();
}

/// THE WINDOW'S HALF OF A RESTARTED DONE (the review of 2026-10-17,
/// owner-eye-4: the done clock was stamped again when the loop restarted): a
/// contract of the window's alone, the faces told by hand. The host tells
/// "unknown" as a loop ends, and the next loop of a done session starts DONE
/// from its seed (`TurnEndState::seed_done`): the window gives the held face
/// back with the time it first moved there — the timeline's, and the clock
/// the chip's `done 17:42`, its title, Settings ▸ Harness and a fired timer's
/// record (`FireOver::Done`) read — not the restart's. Under Cruise, where
/// done is drawn, and under Land. NEGATIVE CONTROL: the seed withdrawn
/// (unknown, then open, then done — the loop's tell where the first point
/// read another reply's done, and a loop with no seed) is a new done,
/// stamped at its own time. That the real loop, restarted by the real host,
/// tells the window those faces — the fix itself, which this test cannot
/// see taken out — is `harness_host`'s
/// `a_real_supervision_restart_keeps_a_dones_first_time`.
#[test]
fn a_restarted_done_keeps_its_first_time() {
    let host = idle_host();
    for word in ["cruise", "land"] {
        let mut app = App::headless_for_test();
        app.harness = Some(host.clone());
        let sid = under(&mut app, word);
        tell(&mut app, &sid, Some(TaskFace::Open));
        tell(&mut app, &sid, Some(TaskFace::Done));
        let ctx = app.pool.get(0).unwrap().ctx.clone();
        let now = aterm_agent::supervise::journal::unix_ms();
        // The done three hours back, so a restart's clock would show.
        let old = now - 3 * 3_600_000;
        {
            let mut tl = ctx.timeline.lock().unwrap();
            tl.set_task_face(Some(TaskFace::Open), old);
            tl.set_task_face(Some(TaskFace::Done), old);
        }
        let face_at = || ctx.timeline.lock().unwrap().task_face();
        let held = crate::harness_glass::Stamp::at(old.div_euclid(1000));
        for restart in 0..3 {
            tell(&mut app, &sid, None);
            assert_eq!(face_at(), None, "{word} {restart}: unknown");
            assert_eq!(app.chip_glass(0).task, None, "{word} {restart}");
            tell(&mut app, &sid, Some(TaskFace::Done));
            assert_eq!(
                face_at(),
                Some((TaskFace::Done, old)),
                "{word} {restart}: the held time"
            );
            let glass = app.chip_glass(0);
            assert_eq!(glass.task, Some(TaskFace::Done), "{word} {restart}");
            assert_eq!(
                glass.task_at, held,
                "{word} {restart}: the chip's, the title's and the page's clock"
            );
        }
        assert!(
            landing_records(&app, &sid).is_empty(),
            "{word}: a done is no landing"
        );
        // NEGATIVE CONTROL: the seed withdrawn — open, then a new done.
        tell(&mut app, &sid, None);
        tell(&mut app, &sid, Some(TaskFace::Open));
        tell(&mut app, &sid, Some(TaskFace::Done));
        let (face, at) = face_at().unwrap();
        assert_eq!(face, TaskFace::Done);
        assert!(
            (now - at).abs() < 60_000,
            "{word}: a new done, at its own time: {at} against {now}"
        );
        assert_ne!(app.chip_glass(0).task_at, held, "{word}");
    }
    host.shutdown_and_join();
}

/// A FACE RESTATED FROM THE JOURNAL TAKES ITS NOTE'S TIME WHERE THE WINDOW
/// HELD NONE (§10.2.7; the review of the done's seed: a seamless update's new
/// window, which no face of the session crossed, stamped the restated done
/// and landing with the update's clock): the loop's start tells a seeded end
/// with the wall time the journal noted it at (`HarnessFact::TaskNoted`), and
/// a window holding no face shows that time — the timeline's, and the chip's,
/// title's and page's clock — under Cruise and under Land, a landing so
/// restated recording nothing. A window that held the face over "unknown"
/// keeps its own first time, and a note's time past now is taken as now.
/// NEGATIVE CONTROL: the same face told plain to a fresh window
/// (`HarnessFact::Task`) is stamped at the window's now.
#[test]
fn a_restated_face_takes_its_notes_time_in_a_window_that_held_none() {
    let host = idle_host();
    let now = aterm_agent::supervise::journal::unix_ms();
    let old = now - 3 * 3_600_000;
    let stamp = |at: i64| crate::harness_glass::Stamp::at(at.div_euclid(1000));
    let face_at = |app: &App| {
        app.pool
            .get(0)
            .unwrap()
            .ctx
            .timeline
            .lock()
            .unwrap()
            .task_face()
    };
    for word in ["cruise", "land"] {
        for face in [TaskFace::Done, TaskFace::Landed] {
            let mut fresh = App::headless_for_test();
            fresh.harness = Some(host.clone());
            let sid = under(&mut fresh, word);
            fresh.apply_harness_fact(&sid, crate::HarnessFact::TaskNoted(face, old));
            assert_eq!(face_at(&fresh), Some((face, old)), "{word} {face:?}");
            let glass = fresh.chip_glass(0);
            assert_eq!(glass.task, Some(face), "{word} {face:?}");
            assert_eq!(glass.task_at, stamp(old), "{word} {face:?}: the clock");
            assert!(
                landing_records(&fresh, &sid).is_empty(),
                "{word} {face:?}: restated, no record"
            );
            // NEGATIVE CONTROL: told plain, a fresh window stamps its now.
            let mut plain = App::headless_for_test();
            plain.harness = Some(host.clone());
            let plain_sid = under(&mut plain, word);
            plain.apply_harness_fact(&plain_sid, crate::HarnessFact::Task(Some(face)));
            let (_, at) = face_at(&plain).unwrap();
            assert!(
                at >= now,
                "{word} {face:?}: plain is now: {at} against {now}"
            );
        }
    }
    // A window that held the face over "unknown" keeps its own first time.
    let mut app = App::headless_for_test();
    app.harness = Some(host.clone());
    let sid = under(&mut app, "cruise");
    let mine = old - 60_000;
    {
        let ctx = app.pool.get(0).unwrap().ctx.clone();
        let mut tl = ctx.timeline.lock().unwrap();
        tl.set_task_face(Some(TaskFace::Open), mine);
        tl.set_task_face(Some(TaskFace::Done), mine);
    }
    app.apply_harness_fact(&sid, crate::HarnessFact::Task(None));
    app.apply_harness_fact(&sid, crate::HarnessFact::TaskNoted(TaskFace::Done, old));
    assert_eq!(face_at(&app), Some((TaskFace::Done, mine)), "the held time");
    // A note's time past now is now, never a clock ahead.
    let mut ahead = App::headless_for_test();
    ahead.harness = Some(host.clone());
    let ahead_sid = under(&mut ahead, "cruise");
    let later = aterm_agent::supervise::journal::unix_ms() + 3 * 3_600_000;
    ahead.apply_harness_fact(
        &ahead_sid,
        crate::HarnessFact::TaskNoted(TaskFace::Done, later),
    );
    let (_, at) = face_at(&ahead).unwrap();
    assert!(at < later && at >= now, "clamped to now: {at}");
    host.shutdown_and_join();
}

/// A SPENT LANDING IS RECORDED ONCE AN EPISODE (the review of slice C): the
/// engine tells a landing spent as a move from open where its last land
/// act's turn ended with no done; after a person's own exchange (`theirs`)
/// or a done task, someone else's turn reads spent again — the same spent
/// landing, no second `Autopilot quiet` record. A landed task is never
/// spent after it (the engine's side), and a move from landed straight to
/// spent would be no record either. NEGATIVE CONTROL: a new loop — unknown,
/// its start open, its own episode typed again — spent once more is a new
/// record.
#[test]
fn a_spent_landing_is_recorded_once_an_episode() {
    let mut app = App::headless_for_test();
    let host = idle_host();
    app.harness = Some(host.clone());
    let sid = under(&mut app, "land");
    let quiet = |app: &App| {
        landing_records(app, &sid)
            .iter()
            .filter(|(t, _)| t.starts_with("Autopilot quiet"))
            .count()
    };
    tell(&mut app, &sid, Some(TaskFace::Open));
    tell(&mut app, &sid, Some(TaskFace::Spent));
    assert_eq!(quiet(&app), 1);
    for back_from in [TaskFace::Theirs, TaskFace::Done, TaskFace::Theirs] {
        tell(&mut app, &sid, Some(back_from));
        tell(&mut app, &sid, Some(TaskFace::Spent));
        assert_eq!(quiet(&app), 1, "spent again after {back_from:?}");
    }
    tell(&mut app, &sid, Some(TaskFace::Landed));
    tell(&mut app, &sid, Some(TaskFace::Spent));
    assert_eq!(quiet(&app), 1, "landed is never spent after it");
    // NEGATIVE CONTROL: a new loop's own episode.
    tell(&mut app, &sid, None);
    tell(&mut app, &sid, Some(TaskFace::Open));
    tell(&mut app, &sid, Some(TaskFace::Spent));
    assert_eq!(quiet(&app), 2, "a new episode");
    host.shutdown_and_join();
}

/// The A/B book's ends this week, counted to now.
fn ends(app: &App) -> crate::harness_ab::AbEnds {
    app.harness_screen
        .ab
        .week_at(crate::turn_ledger::now_ms(), &[])
        .ends
}

/// A LANDING IS COUNTED ONCE, HOWEVER OFTEN IT IS TOLD (§11.6.5 "Counted
/// once"): under Land, open then landed is one landing in the A/B book and
/// one `harness.landing.<sid>` record; unknown then landed again (a loop's
/// restart restating it) leaves both at one; a spent landing is one quiet
/// end, told again after a person's exchange none more. The count is the
/// record's rule (`is_an_end`), so the two agree. Under Cruise nothing is
/// counted here: Cruise's done is the one end's, slice 2a's. NEGATIVE
/// CONTROL: the count fed without `is_an_end` makes the restated landing a
/// second (the mutation run in the landing commit).
#[test]
fn a_landing_is_counted_once_however_often_it_is_told() {
    let mut app = App::headless_for_test();
    let host = idle_host();
    app.harness = Some(host.clone());
    let sid = under(&mut app, "land");
    tell(&mut app, &sid, Some(TaskFace::Open));
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert_eq!(ends(&app).landed, 1);
    assert_eq!(landing_records(&app, &sid).len(), 1);
    for face in [None, Some(TaskFace::Landed), None, Some(TaskFace::Landed)] {
        tell(&mut app, &sid, face);
    }
    assert_eq!(ends(&app).landed, 1, "restated, not counted again");
    assert_eq!(landing_records(&app, &sid).len(), 1, "nor recorded again");
    tell(&mut app, &sid, Some(TaskFace::Open));
    tell(&mut app, &sid, Some(TaskFace::Spent));
    tell(&mut app, &sid, Some(TaskFace::Theirs));
    tell(&mut app, &sid, Some(TaskFace::Spent));
    let counted = ends(&app);
    assert_eq!((counted.landed, counted.quiet, counted.done), (1, 1, 0));
    assert_eq!(landing_records(&app, &sid).len(), 2, "landed, quiet");
    // Under Cruise: a landing's ends are no count here.
    let mut cruise = App::headless_for_test();
    cruise.harness = Some(host.clone());
    let sid = under(&mut cruise, "cruise");
    for face in [
        TaskFace::Open,
        TaskFace::Landed,
        TaskFace::Open,
        TaskFace::Done,
    ] {
        tell(&mut cruise, &sid, Some(face));
    }
    assert_eq!(ends(&cruise), crate::harness_ab::AbEnds::default());
    host.shutdown_and_join();
}

/// This host's supervisor claim on `ctx`, taken now (`claimed`), or given
/// back; the App told, as the claim's wire wake tells it.
fn claim(app: &mut App, ctx: &crate::SessionCtx, claimed: bool) {
    let now_us = crate::metrics::now_us();
    if claimed {
        crate::session_timeline::claim_supervisor(
            ctx,
            &crate::harness_host::holder(),
            None,
            None,
            now_us,
        )
        .unwrap();
    } else {
        crate::session_timeline::release_supervisor(ctx, None, now_us, aterm_slog::Actor::Ctl);
    }
    app.on_meta_changed(0);
}

/// THE TIME FROM LAND TO LANDED STARTS WHERE THE MODE MOVED, through the
/// App's own paths (§11.6.4 "Before this process", §11.6.7): a session
/// whose word is Land when the book first reads it — restored mid-landing,
/// the new process's supervisor not yet claiming — is unsupervised, then
/// Land once claimed, and its landing counts with no span; a session pressed
/// from Cruise to Land starts at the press, and its claim lapsing and taken
/// again keeps that start, so its landing has one span. NEGATIVE CONTROL:
/// the mode read as the arm (`lands` = the arm is Land in
/// `App::note_harness_arm`, the first build's rule) starts the restored
/// landing at the claim (a sample) and the pressed one again at the retake.
#[test]
fn the_time_from_land_to_landed_starts_where_the_mode_moved() {
    use crate::harness_ab::AbArm;
    let host = idle_host();
    // Restored mid-landing: a new process's book, the word already Land.
    let mut app = App::headless_for_test();
    app.harness = Some(host.clone());
    let sid = under(&mut app, "land");
    app.harness_screen.ab = crate::harness_ab::AbBook::default();
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    app.on_meta_changed(0);
    assert_eq!(app.harness_ab_arm(0), AbArm::Unsupervised, "no claim yet");
    claim(&mut app, &ctx, true);
    assert_eq!(app.harness_ab_arm(0), AbArm::Land);
    assert_eq!(
        app.harness_screen.ab.land_from(0),
        None,
        "the claim starts nothing"
    );
    tell(&mut app, &sid, Some(TaskFace::Open));
    tell(&mut app, &sid, Some(TaskFace::Landed));
    let counted = ends(&app);
    assert_eq!(
        (counted.landed, counted.landing.iter().sum::<u32>()),
        (1, 0),
        "counted, with no span"
    );
    // Pressed from Cruise; the claim lapses and is taken again.
    let mut app = App::headless_for_test();
    app.harness = Some(host.clone());
    let sid = under(&mut app, "cruise");
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    claim(&mut app, &ctx, true);
    assert_eq!(app.harness_ab_arm(0), AbArm::Cruise);
    assert!(app.set_session_autopilot(0, Some(Autopilot::Land)));
    let from = app.harness_screen.ab.land_from(0);
    assert!(from.is_some(), "the press starts it");
    // The pure book case `the_start_follows_the_mode_never_the_claim` uses
    // separated captured instants to distinguish a start keyed to the claim.
    // This App case checks the caller wiring without waiting on the clock.
    claim(&mut app, &ctx, false);
    assert_eq!(app.harness_ab_arm(0), AbArm::Unsupervised);
    claim(&mut app, &ctx, true);
    assert_eq!(app.harness_ab_arm(0), AbArm::Land);
    assert_eq!(
        app.harness_screen.ab.land_from(0),
        from,
        "kept through the blip"
    );
    tell(&mut app, &sid, Some(TaskFace::Open));
    tell(&mut app, &sid, Some(TaskFace::Landed));
    let counted = ends(&app);
    assert_eq!(
        (counted.landed, counted.landing.iter().sum::<u32>()),
        (1, 1)
    );
    host.shutdown_and_join();
}

/// A WORD CARRIED FROM BEFORE THIS PROCESS IS NO MOVE (§11.6.4 "Before
/// this process", §11.6.7): window 0's bootstrap shell runs Claude Code with
/// no word, so the book first reads it under the table's default (Cruise);
/// its restored leaf's `land` is then grafted on (`graft_restored_user_meta`,
/// the path a relaunch and a seamless update's owed graft take), and the new
/// supervisor claims it: its landing counts with no span. A grafted word
/// that moves nothing leaves the book as it was. NEGATIVE CONTROL: the graft
/// not telling the book (the review fix's first build, lab `fr1`) reads the
/// carried word as a move into Land and starts the span at the claim.
#[test]
fn a_word_carried_from_before_this_process_starts_no_span() {
    use crate::harness_ab::AbArm;
    let host = idle_host();
    let mut app = App::headless_for_test();
    app.harness = Some(host.clone());
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    {
        let mut tl = ctx.timeline.lock().unwrap();
        tl.note_foreground_group(7);
        tl.set_program(7, Some("claude".into()));
    }
    let sid = ctx.self_id.as_str().to_string();
    app.on_meta_changed(0);
    assert_eq!(app.harness_ab_reading(0), (AbArm::Unsupervised, false));
    let leaf = |word: &str| crate::restore::TerminalLeafRestore {
        sid: None,
        lineage: None,
        cwd: None,
        title: String::new(),
        profile: None,
        local_id: Some(0),
        user_title: None,
        description: None,
        icon: None,
        role: None,
        attention: None,
        questions: None,
        autopilot: Some(word.to_string()),
        land_at: None,
        identity: None,
        agent: None,
        supervised: None,
        lost: None,
        held: false,
        holds: Vec::new(),
        theme: None,
    };
    assert!(app.graft_restored_user_meta(0, &leaf("land")));
    claim(&mut app, &ctx, true);
    assert_eq!(app.harness_ab_reading(0), (AbArm::Land, true));
    assert_eq!(
        app.harness_screen.ab.land_from(0),
        None,
        "the carried word starts nothing"
    );
    tell(&mut app, &sid, Some(TaskFace::Open));
    tell(&mut app, &sid, Some(TaskFace::Landed));
    let counted = ends(&app);
    assert_eq!(
        (counted.landed, counted.landing.iter().sum::<u32>()),
        (1, 0),
        "counted, with no span"
    );
    // A press into Land, then the same word grafted again: nothing moved,
    // so the press's start stands.
    let mut app = App::headless_for_test();
    app.harness = Some(host.clone());
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    under(&mut app, "cruise");
    claim(&mut app, &ctx, true);
    assert!(app.set_session_autopilot(0, Some(Autopilot::Land)));
    let from = app.harness_screen.ab.land_from(0);
    assert!(from.is_some());
    app.graft_restored_user_meta(0, &leaf("land"));
    assert_eq!(app.harness_screen.ab.land_from(0), from);
    host.shutdown_and_join();
}

/// A QUIET LANDING THAT LANDS AFTER ALL IS ONE END (§11.6.7; `TaskFace::of`:
/// a done reply to a question's land answer ends the quiet): open → spent →
/// landed under Land reads `Land landed 1`, and both records stay (the quiet
/// and the landing each happened). A quiet end followed by a new task that
/// lands (open between) is two ends. NEGATIVE CONTROL: the landing counted
/// alone (`AbBook::end` for spent → landed, the first build) reads `Land
/// landed 1, quiet 1`.
#[test]
fn a_quiet_landing_that_lands_after_all_is_one_end() {
    let host = idle_host();
    let words = |faces: &[TaskFace]| {
        let mut app = App::headless_for_test();
        app.harness = Some(host.clone());
        let sid = under(&mut app, "land");
        for face in faces {
            tell(&mut app, &sid, Some(*face));
        }
        (
            crate::harness_ab::ends_words(&ends(&app)),
            landing_records(&app, &sid)
                .into_iter()
                .map(|(title, _)| title)
                .collect::<Vec<_>>(),
        )
    };
    use TaskFace::{Landed, Open, Spent};
    assert_eq!(
        words(&[Open, Spent, Landed]),
        (
            Some("Land landed 1".to_string()),
            vec![
                "Autopilot quiet in tab 1".to_string(),
                "Autopilot landed in tab 1".to_string()
            ]
        )
    );
    assert_eq!(
        words(&[Open, Spent, Open, Landed]).0.as_deref(),
        Some("Land landed 1, quiet 1"),
        "a new task's landing is its own end"
    );
    host.shutdown_and_join();
}

/// THE LANDING'S OWN PUBLISH READS IT (§11.6.7): with Settings ▸ Harness
/// shown, the page the landing republishes already counts it — no later
/// publish, no Refresh. NEGATIVE CONTROL: the count placed after the
/// republish in `App::harness_task_moved` leaves the page at 0.
#[test]
fn the_page_a_landing_publishes_counts_it() {
    let mut app = App::headless_for_test();
    let host = idle_host();
    app.harness = Some(host.clone());
    let sid = under(&mut app, "land");
    assert!(
        app.open_harness_settings(crate::WindowId(0), crate::harness_screen::HarnessFocus::Top)
    );
    assert!(app.harness_page_shown());
    let published = |app: &App| {
        let instance = app
            .native_runtime
            .instance_by_kind(crate::native_app::AppKind::Settings)
            .expect("the Settings controller");
        let Some(crate::native_app::NativeApp::Settings(settings)) =
            app.native_runtime.app(instance)
        else {
            unreachable!("a Settings instance")
        };
        settings.harness_snapshot().ab.ends.landed
    };
    tell(&mut app, &sid, Some(TaskFace::Open));
    assert_eq!(published(&app), 0);
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert_eq!(published(&app), 1, "the landing's own publish");
    host.shutdown_and_join();
}

/// THE MOVES THAT ARE ENDS ([`is_an_end`]): landed from any face the window
/// knew, spent from open alone; landed from unknown (a loop's start, the
/// seed) and spent from anything else are none, and so is every other face.
#[test]
fn a_landings_end_is_judged_by_the_face_it_moved_from() {
    let faces = [
        None,
        Some(TaskFace::Open),
        Some(TaskFace::Landed),
        Some(TaskFace::Done),
        Some(TaskFace::Spent),
        Some(TaskFace::Theirs),
    ];
    for was in faces {
        for face in faces.into_iter().flatten() {
            let want = match face {
                TaskFace::Landed => was.is_some(),
                TaskFace::Spent => was == Some(TaskFace::Open),
                _ => false,
            };
            assert_eq!(is_an_end(was, face), want, "{was:?} → {face:?}");
        }
    }
}

/// A record's title names its tab within the glass title rule, and a session
/// no tab shows is named by nothing; every face but landed and spent is no
/// record. THE PANE (§10.3.3, item 9) is in the LINE alone: a session in a tab
/// that holds two agents leads its line with its pane, whole within the
/// detail line's cap, its title the tab's. NEGATIVE CONTROL: the line's place
/// never reaches the title.
#[test]
fn a_records_title_names_its_tab_within_the_title_rule() {
    for (place, title) in [
        ("in tab 3", "Autopilot landed in tab 3"),
        ("in window 2, tab 3", "Autopilot landed in window 2, tab 3"),
        ("in its tab", "Autopilot landed"),
    ] {
        let m = task_record("s-1", place, place, TaskFace::Landed).expect("a record");
        assert_eq!(m.title, title, "{place}");
        assert_eq!(aterm_messages::text::glass_title_fault(&m.title), None);
    }
    for face in [TaskFace::Open, TaskFace::Done, TaskFace::Theirs] {
        assert!(
            task_record("s-1", "in tab 3", "in tab 3", face).is_none(),
            "{face:?}"
        );
    }
    for (line_place, lead) in [
        ("in tab 3, left pane", "Tab 3, left pane: "),
        ("in window 2, tab 3, pane 3", "Window 2, tab 3, pane 3: "),
    ] {
        for face in [TaskFace::Landed, TaskFace::Spent] {
            let m = task_record("s-1", "in tab 3", line_place, face).expect("a record");
            assert!(!m.title.contains("pane"), "{}", m.title);
            let line = &m.detail[0];
            assert!(line.starts_with(lead), "{line}");
            assert!(
                line.chars().count() <= aterm_messages::DETAIL_LINE_CAP
                    && !line.ends_with('\u{2026}'),
                "the line is whole: {line}"
            );
        }
    }
}

/// THE FIVE FACES UNDER LAND, THE TWO ENDS UNDER CRUISE, NONE UNDER OFF
/// (§9.4.1, §10.2.1): each face the loop tells reaches the glass the frame
/// reads, and the chip wears `⤓ land`, `✓ landed`, `✓ done`, `⤓ quiet` and
/// `⤓ quiet` for open, landed, done, spent and theirs under Land; under
/// Cruise `∞ ✓ landed` and `∞ ✓ done` for the two ends and `∞` for the rest;
/// the card's line ends `✓ landed` or `✓ done` under both, the mode's own
/// words for the rest. Unknown draws as open. NEGATIVE CONTROL: Off keeps
/// v1's face and line whatever is told.
#[test]
fn the_faces_show_under_land_the_ends_under_cruise_and_none_under_off() {
    let host = idle_host();
    for (word, view) in [
        ("land", AutopilotView::Land),
        ("cruise", AutopilotView::Cruise),
        ("off", AutopilotView::Off),
    ] {
        let mut app = App::headless_for_test();
        app.harness = Some(host.clone());
        let sid = under(&mut app, word);
        assert_eq!(app.session_autopilot(0).1, view, "{word}");
        for (face, chip, end) in [
            (Some(TaskFace::Open), ChipFace::Land, LineEnd::Mode),
            (Some(TaskFace::Landed), ChipFace::Landed, LineEnd::Landed),
            (Some(TaskFace::Done), ChipFace::Done, LineEnd::Done),
            (Some(TaskFace::Spent), ChipFace::Spent, LineEnd::Mode),
            (Some(TaskFace::Theirs), ChipFace::Theirs, LineEnd::Mode),
            (None, ChipFace::Land, LineEnd::Mode),
        ] {
            app.apply_harness_fact(&sid, crate::HarnessFact::Task(face));
            let glass = app.chip_glass(0);
            assert_eq!(glass.task, face, "{word}: the glass holds what was told");
            let want = match (view, face) {
                (AutopilotView::Land, _) => (chip, end),
                (AutopilotView::Cruise, Some(TaskFace::Landed)) => {
                    (ChipFace::CruiseLanded, LineEnd::Landed)
                }
                (AutopilotView::Cruise, Some(TaskFace::Done)) => {
                    (ChipFace::CruiseDone, LineEnd::Done)
                }
                (AutopilotView::Cruise, _) => (ChipFace::Cruise, LineEnd::Mode),
                _ => (ChipFace::Off, LineEnd::Mode),
            };
            assert_eq!(
                (ChipFace::of(view, &glass), LineEnd::of(view, glass.task)),
                want,
                "{word}: {face:?}"
            );
        }
    }
    host.shutdown_and_join();
}

/// What a point of the loop reads: the worker's last words `said` after a
/// turn of `worked`, under Land, a person's last keystroke `person` ago.
fn reading(said: &str, worked: Option<Duration>, person: Option<Duration>) -> TurnEndReading {
    TurnEndReading {
        phase: aterm_agent::supervise::phase::Phase::Idle,
        authoritative: true,
        survey: false,
        wall: None,
        wall_message: String::new(),
        reset_at: None,
        jitter: Duration::ZERO,
        resumes_by_itself: false,
        composer: Composer::Empty,
        said_tail: Some(said.to_string()),
        worked,
        rules: None,
        pending_input: false,
        interrupted: false,
        restartable: true,
        resume: None,
        upgrading: false,
        taskless: false,
        person,
        reach: Default::default(),
        login_back: false,
        auto_continue: None,
        since_limit: None,
        served_in_transcript: false,
        reset_clock: None,
        program: aterm_phase::Program::Claude,
        account: None,
        autopilot: Autopilot::Land,
        login_label: None,
        login_superseded: false,
        login_notice: None,
        finished_record: None,
        finished_at_check: false,
        check_record: None,
        left_record: false,
        left_work: None,
        notice_elsewhere: false,
        wall_remembered: false,
        background: false,
        answered: None,
        worked_doubt: Duration::ZERO,
        record_turn: None,
        credits: None,
        repo: None,
        login_screen: None,
        login_interrupted: false,
        login_method: None,
        login_shared: None,
    }
}

/// A PERSON'S OWN EXCHANGE AFTER A LANDING READS `task=theirs` (§9.6.3's
/// acceptance): the landing told `landed` through the host's wake, then a
/// person's key typed through the App's own input (`Source::Human` — ctl
/// input is never a person's, so the lab cannot type one) stamps the
/// session's person, which `status` prints as `human_ms=`; the loop reads
/// that stamp into the point of the person's quick turn, the policy leaves
/// the turn to them, and the face it tells reaches `status task=theirs`.
/// NEGATIVE CONTROL: the same turn with no person's key reads `task=open` —
/// the task reopened, not theirs.
#[test]
fn a_persons_own_exchange_typed_into_the_app_after_a_landing_reads_task_theirs() {
    use crate::input::InputEvent;
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers};
    let field = |app: &App, key: &str| {
        let record = app.session_status_record(0).expect("live session");
        record
            .split_whitespace()
            .find_map(|t| t.strip_prefix(key))
            .unwrap_or_else(|| panic!("{key} in {record}"))
            .to_string()
    };
    let run = |type_a_key: bool| {
        let mut app = App::headless_for_test();
        let host = idle_host();
        app.harness = Some(host.clone());
        let sid = under(&mut app, "land");
        let ctx = app.pool.get(0).unwrap().ctx.clone();
        crate::session_timeline::claim_supervisor(
            &ctx,
            crate::autopilot::own_holder(),
            None,
            None,
            crate::metrics::now_us(),
        )
        .unwrap();
        // The loop, ten minutes back: the land instruction, and its done
        // reply.
        let back = Instant::now() - Duration::from_secs(600);
        let mut st = TurnEndState::default();
        st.face_start();
        let first = reading("Stage 1 landed.", Some(Duration::from_secs(180)), None);
        st.observe(&first, back);
        let act = decide_turn_end(&st, &first, &SupervisorConfig::default(), back);
        st.acted(&act, &first, back);
        let done = back + Duration::from_secs(120);
        st.observe(
            &reading(
                "Committed it all.\nATERM-TASK-DONE",
                Some(Duration::from_secs(120)),
                None,
            ),
            done,
        );
        let told = st.face_moved();
        assert_eq!(told, Some(TaskFace::Landed));
        app.apply_harness_fact(&sid, crate::HarnessFact::Task(told));
        assert_eq!(field(&app, "task="), "landed");
        assert_eq!(field(&app, "human_ms="), "-", "no person yet");
        // A person types into the window.
        if type_a_key {
            let key = InputEvent::Key {
                key: Key::Character('y'),
                mods: Modifiers::empty(),
                base_layout: None,
                layout_text: None,
                event_type: KeyEventType::Press,
            };
            let _ = app.input(crate::WindowId(0), key, crate::input::Source::Human);
        }
        // The loop's point at the end of their quick turn reads the stamp
        // as `status` prints it.
        let person = field(&app, "human_ms=")
            .parse::<u64>()
            .ok()
            .map(Duration::from_millis);
        assert_eq!(person.is_some(), type_a_key, "{person:?}");
        st.observe(
            &reading("You're welcome!", Some(Duration::from_secs(5)), person),
            Instant::now(),
        );
        if let Some(face) = st.face_moved() {
            app.apply_harness_fact(&sid, crate::HarnessFact::Task(Some(face)));
        }
        let task = field(&app, "task=");
        host.shutdown_and_join();
        task
    };
    assert_eq!(run(true), "theirs");
    // NEGATIVE CONTROL: nobody's key.
    assert_eq!(run(false), "open");
}
