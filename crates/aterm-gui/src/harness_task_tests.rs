// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LANDING in the window (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.4, Autopilot Land): the three faces on the chip under Land and none under Cruise or Off, the record of a landing and of nothing else, a proven self-claim's own landing and its reopening at anyone's later turn, and a person's own exchange typed through the App's input after a landing, read back as `status task=theirs`.

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

/// UNDER LAND, A LANDING IS A RECORD, AND NOTHING ELSE IS (§9.4.1, FOREVER
/// LAND): `Autopilot landed in tab 1`, led by its tab; the same face told
/// again is no move and no second record; open, theirs and unknown are none.
#[test]
fn a_landing_under_land_is_a_record_and_no_other_move_is() {
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
    let records = landing_records(&app, &sid);
    assert_eq!(
        records.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>(),
        ["Autopilot landed in tab 1"],
        "once: told again is no move"
    );
    assert!(records[0].1[0].starts_with("Tab 1: "), "{:?}", records[0].1);
    for face in [
        Some(TaskFace::Open),
        Some(TaskFace::Theirs),
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
    for (title, _) in &records {
        assert_eq!(
            aterm_messages::text::glass_title_fault(title),
            None,
            "{title}"
        );
    }
    for word in ["cruise", "off"] {
        let mut app = App::headless_for_test();
        app.harness = Some(host.clone());
        let sid = under(&mut app, word);
        for face in [
            TaskFace::Open,
            TaskFace::Landed,
            TaskFace::Open,
            TaskFace::Landed,
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
/// panicked body, an Off and back, each the same moves.
#[test]
fn a_restarted_landing_is_restated_with_its_first_time_and_recorded_once() {
    let mut app = App::headless_for_test();
    let host = idle_host();
    app.harness = Some(host.clone());
    let sid = under(&mut app, "land");
    tell(&mut app, &sid, Some(TaskFace::Open));
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert_eq!(landing_records(&app, &sid).len(), 1);
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
    let mut fresh = App::headless_for_test();
    fresh.harness = Some(host.clone());
    let fresh_sid = under(&mut fresh, "land");
    tell(&mut fresh, &fresh_sid, Some(TaskFace::Landed));
    assert_eq!(fresh.chip_glass(0).task, Some(TaskFace::Landed));
    assert!(landing_records(&fresh, &fresh_sid).is_empty());
    tell(&mut app, &sid, Some(TaskFace::Open));
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert_eq!(landing_records(&app, &sid).len(), 2, "a new landing");
    assert_ne!(face_at().map(|(_, at)| at), Some(old), "its own time");
    tell(&mut app, &sid, None);
    tell(&mut app, &sid, Some(TaskFace::Open));
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert_eq!(landing_records(&app, &sid).len(), 3, "a new landing");
    host.shutdown_and_join();
}

/// THE WINDOW'S HALF OF A RESTARTED LANDING (the review of 2026-10-17,
/// owner-eye-4: the end's clock was stamped again when the loop restarted):
/// a contract of the window's alone, the faces told by hand.
#[test]
fn a_restarted_landing_keeps_its_first_time() {
    let host = idle_host();
    let mut app = App::headless_for_test();
    app.harness = Some(host.clone());
    let sid = under(&mut app, "land");
    tell(&mut app, &sid, Some(TaskFace::Open));
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert_eq!(landing_records(&app, &sid).len(), 1);
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    let now = aterm_agent::supervise::journal::unix_ms();
    let old = now - 3 * 3_600_000;
    {
        let mut tl = ctx.timeline.lock().unwrap();
        tl.set_task_face(Some(TaskFace::Open), old);
        tl.set_task_face(Some(TaskFace::Landed), old);
    }
    let face_at = || ctx.timeline.lock().unwrap().task_face();
    let held = crate::harness_glass::Stamp::at(old.div_euclid(1000));
    for restart in 0..3 {
        tell(&mut app, &sid, None);
        assert_eq!(face_at(), None, "{restart}: unknown");
        assert_eq!(app.chip_glass(0).task, None, "{restart}");
        tell(&mut app, &sid, Some(TaskFace::Landed));
        assert_eq!(
            face_at(),
            Some((TaskFace::Landed, old)),
            "{restart}: the held time"
        );
        let glass = app.chip_glass(0);
        assert_eq!(glass.task, Some(TaskFace::Landed), "{restart}");
        assert_eq!(
            glass.task_at, held,
            "{restart}: the chip's, the title's and the page's clock"
        );
    }
    assert_eq!(
        landing_records(&app, &sid).len(),
        1,
        "a restated landing is no new record"
    );
    tell(&mut app, &sid, None);
    tell(&mut app, &sid, Some(TaskFace::Open));
    tell(&mut app, &sid, Some(TaskFace::Landed));
    let (face, at) = face_at().unwrap();
    assert_eq!(face, TaskFace::Landed);
    assert!(
        (now - at).abs() < 60_000,
        "a new landing, at its own time: {at} against {now}"
    );
    assert_ne!(app.chip_glass(0).task_at, held);
    assert_eq!(landing_records(&app, &sid).len(), 2, "a new landing");
    host.shutdown_and_join();
}

/// A LANDING RESTATED FROM THE JOURNAL TAKES ITS NOTE'S TIME WHERE THE
/// WINDOW HELD NONE (§10.2.7: a seamless update's new window, which no face
/// of the session crossed, stamped the restated landing with the update's
/// clock): the loop's start tells a seeded landing with the wall time the
/// journal noted it at (`HarnessFact::TaskNoted`), and a window holding no
/// face shows that time — the timeline's, and the chip's, title's and page's
/// clock — recording nothing.
#[test]
fn a_restated_landing_takes_its_notes_time_in_a_window_that_held_none() {
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
        let face = TaskFace::Landed;
        let mut fresh = App::headless_for_test();
        fresh.harness = Some(host.clone());
        let sid = under(&mut fresh, word);
        fresh.apply_harness_fact(&sid, crate::HarnessFact::TaskNoted(face, old));
        assert_eq!(face_at(&fresh), Some((face, old)), "{word}");
        let glass = fresh.chip_glass(0);
        assert_eq!(glass.task, Some(face), "{word}");
        assert_eq!(glass.task_at, stamp(old), "{word}: the clock");
        assert!(
            landing_records(&fresh, &sid).is_empty(),
            "{word}: restated, no record"
        );
        let mut plain = App::headless_for_test();
        plain.harness = Some(host.clone());
        let plain_sid = under(&mut plain, word);
        plain.apply_harness_fact(&plain_sid, crate::HarnessFact::Task(Some(face)));
        let (_, at) = face_at(&plain).unwrap();
        assert!(at >= now, "{word}: plain is now: {at} against {now}");
    }
    let mut app = App::headless_for_test();
    app.harness = Some(host.clone());
    let sid = under(&mut app, "land");
    let mine = old - 60_000;
    {
        let ctx = app.pool.get(0).unwrap().ctx.clone();
        let mut tl = ctx.timeline.lock().unwrap();
        tl.set_task_face(Some(TaskFace::Open), mine);
        tl.set_task_face(Some(TaskFace::Landed), mine);
    }
    app.apply_harness_fact(&sid, crate::HarnessFact::Task(None));
    app.apply_harness_fact(&sid, crate::HarnessFact::TaskNoted(TaskFace::Landed, old));
    assert_eq!(
        face_at(&app),
        Some((TaskFace::Landed, mine)),
        "the held time"
    );
    let mut ahead = App::headless_for_test();
    ahead.harness = Some(host.clone());
    let ahead_sid = under(&mut ahead, "land");
    let later = aterm_agent::supervise::journal::unix_ms() + 3 * 3_600_000;
    ahead.apply_harness_fact(
        &ahead_sid,
        crate::HarnessFact::TaskNoted(TaskFace::Landed, later),
    );
    let (_, at) = face_at(&ahead).unwrap();
    assert!(at < later && at >= now, "clamped to now: {at}");
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
/// restart restating it) leaves both at one; a person's exchange (`theirs`)
/// counts nothing.
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
    for face in [
        None,
        Some(TaskFace::Landed),
        None,
        Some(TaskFace::Landed),
        Some(TaskFace::Theirs),
    ] {
        tell(&mut app, &sid, face);
    }
    assert_eq!(ends(&app).landed, 1, "restated, not counted again");
    assert_eq!(landing_records(&app, &sid).len(), 1, "nor recorded again");
    tell(&mut app, &sid, Some(TaskFace::Open));
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert_eq!(ends(&app).landed, 2);
    assert_eq!(landing_records(&app, &sid).len(), 2);
    let mut cruise = App::headless_for_test();
    cruise.harness = Some(host.clone());
    let sid = under(&mut cruise, "cruise");
    for face in [TaskFace::Open, TaskFace::Landed, TaskFace::Open] {
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
/// again keeps that start, so its landing has one span.
#[test]
fn the_time_from_land_to_landed_starts_where_the_mode_moved() {
    use crate::harness_ab::AbArm;
    let host = idle_host();
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
    let mut app = App::headless_for_test();
    app.harness = Some(host.clone());
    let sid = under(&mut app, "cruise");
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    claim(&mut app, &ctx, true);
    assert_eq!(app.harness_ab_arm(0), AbArm::Cruise);
    assert!(app.set_session_autopilot(0, Some(Autopilot::Land), crate::autopilot::Surface::Chip));
    let from = app.harness_screen.ab.land_from(0);
    assert!(from.is_some(), "the press starts it");
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
/// supervisor claims it: its landing counts with no span.
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
        log_ep: None,
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
        autopilot_by: None,
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
    let mut app = App::headless_for_test();
    app.harness = Some(host.clone());
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    under(&mut app, "cruise");
    claim(&mut app, &ctx, true);
    assert!(app.set_session_autopilot(0, Some(Autopilot::Land), crate::autopilot::Surface::Chip));
    let from = app.harness_screen.ab.land_from(0);
    assert!(from.is_some());
    app.graft_restored_user_meta(0, &leaf("land"));
    assert_eq!(app.harness_screen.ab.land_from(0), from);
    host.shutdown_and_join();
}

/// A NEW TASK'S LANDING IS ITS OWN END, TIMED FROM ITS OPENING (§11.2.3,
/// item 2; §11.6.3, item 4): under Land first read as Land (no start), open
/// → landed → open → landed counts two landings and two records, and the
/// second is timed from the move that opened its task
/// (`AbBook::task_opened` under Land starts the span), so the Ends row reads
/// one sample.
#[test]
fn a_new_tasks_landing_is_its_own_end_timed_from_its_opening() {
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
    use TaskFace::{Landed, Open};
    let landed = "Autopilot landed in tab 1".to_string();
    assert_eq!(
        words(&[Open, Landed, Open, Landed]),
        (
            Some(format!(
                "Land landed 2{}Land to landed: not enough yet (1)",
                crate::native_ui::FACT_SEPARATOR
            )),
            vec![landed.clone(), landed.clone()]
        ),
        "a new task's landing is its own end, timed from its opening"
    );
    assert_eq!(
        words(&[Open, Landed]),
        (Some("Land landed 1".to_string()), vec![landed]),
        "CONTROL: the first landing has no span"
    );
    host.shutdown_and_join();
}

/// THE LANDING'S OWN PUBLISH READS IT (§11.6.7): with Settings ▸ Harness
/// shown, the page the landing republishes already counts it — no later
/// publish, no Refresh.
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
/// knew; landed from unknown (a loop's start, the seed) is none, and so is
/// every other face.
#[test]
fn a_landings_end_is_judged_by_the_face_it_moved_from() {
    let faces = [
        None,
        Some(TaskFace::Open),
        Some(TaskFace::Landed),
        Some(TaskFace::Theirs),
    ];
    for was in faces {
        for face in faces.into_iter().flatten() {
            let want = face == TaskFace::Landed && was.is_some();
            assert_eq!(is_an_end(was, face), want, "{was:?} → {face:?}");
        }
    }
}

/// A record's title names its tab within the glass title rule, and a session
/// no tab shows is named by nothing; every face but landed is no record.
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
    for face in [TaskFace::Open, TaskFace::Theirs] {
        assert!(
            task_record("s-1", "in tab 3", "in tab 3", face).is_none(),
            "{face:?}"
        );
    }
    for (line_place, lead) in [
        ("in tab 3, left pane", "Tab 3, left pane: "),
        ("in window 2, tab 3, pane 3", "Window 2, tab 3, pane 3: "),
    ] {
        let m = task_record("s-1", "in tab 3", line_place, TaskFace::Landed).expect("a record");
        assert!(!m.title.contains("pane"), "{}", m.title);
        let line = &m.detail[0];
        assert!(line.starts_with(lead), "{line}");
        assert!(
            line.chars().count() <= aterm_messages::DETAIL_LINE_CAP && !line.ends_with('\u{2026}'),
            "the line is whole: {line}"
        );
    }
}

/// THE THREE FACES UNDER LAND, NONE UNDER CRUISE OR OFF (§9.4.1, FOREVER
/// CRUISE): each face the loop tells reaches the glass the frame reads, and
/// the chip wears `⤓ land`, `✓ landed` and `⤓ quiet` for open, landed and
/// theirs under Land, the card's line ending `✓ landed` for the landing and
/// the mode's own words for the rest; Cruise takes nothing as the end and
/// wears `∞` whatever is told.
#[test]
fn the_faces_show_under_land_and_none_under_cruise_or_off() {
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
            (Some(TaskFace::Theirs), ChipFace::Theirs, LineEnd::Mode),
            (None, ChipFace::Land, LineEnd::Mode),
        ] {
            app.apply_harness_fact(&sid, crate::HarnessFact::Task(face));
            let glass = app.chip_glass(0);
            assert_eq!(glass.task, face, "{word}: the glass holds what was told");
            let want = match (view, face) {
                (AutopilotView::Land, _) => (chip, end),
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
        said_cut: false,
        opens_cut: false,
        turn_said: None,
        opened_by: None,
        message_drawn: true,
        submits: None,
        inputs_before: None,
        screen_mark: None,
        worked,
        rules: None,
        pending_input: false,
        interrupted: false,
        restartable: true,
        relaunch_off: false,
        resume: None,
        upgrading: false,
        taskless: false,
        person,
        reach: Default::default(),
        login_back: false,
        auto_continue: None,
        since_limit: None,
        served_in_transcript: false,
        chosen_in_transcript: None,
        conversation: aterm_agent::supervise::policy::turn_end::Conversation::Hostless,
        reset_clock: None,
        program: aterm_phase::Program::Claude,
        account: None,
        autopilot: Autopilot::Land,
        login_label: None,
        login_superseded: false,
        login_notice: None,
        finished_record: None,
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
        login_at_wall: None,
        login_method: None,
        login_shared: None,
        roster: None,
        seat: Default::default(),
    }
}

/// A PERSON'S OWN EXCHANGE AFTER A LANDING READS `task=theirs` (§9.6.3's
/// acceptance): the landing told `landed` through the host's wake, then a
/// person's key typed through the App's own input (`Source::Human` — ctl
/// input is never a person's, so the lab cannot type one) stamps the
/// session's person, which `status` prints as `human_ms=`; the loop reads
/// that stamp into the point of the person's quick turn, the policy leaves
/// the turn to them, and the face it tells reaches `status task=theirs`.
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
                "Nothing left: committed, merged and pushed it all to origin/main.",
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
    assert_eq!(run(false), "open");
}

/// THE FACE A PROVEN SELF-CLAIM'S ACT TELLS (Autopilot Land): its landing
/// `landed@v1` is `landed`, recorded by `meta set harness_act` alone; a point
/// it left to a person's own exchange (`left-to-person@v1`, aterm's own
/// loop's `LEFT` rule, recorded and never typed) is `theirs`; its re-exec
/// onto a new build (`upgrade@v1`, which carries a landing across) tells
/// none; every other act it records — a continuation, a land act, a resume
/// after a wall, an older build's `finished@v1` token act — reopens the work.
#[test]
fn a_self_claims_landing_act_is_landed_and_every_other_act_is_open() {
    use crate::harness_task::{SELF_CLAIM_LANDED, self_claim_act_face};
    assert_eq!(
        self_claim_act_face(SELF_CLAIM_LANDED),
        Some(TaskFace::Landed)
    );
    assert_eq!(
        self_claim_act_face("left-to-person@v1"),
        Some(TaskFace::Theirs)
    );
    assert_eq!(self_claim_act_face("upgrade@v1"), None);
    for act in [
        "continue@v1",
        "land@v1",
        "usage-resume@v1",
        "approve-all@v1",
        "finished@v1",
        "landed@v2",
        "landed",
        "upgrade@v2",
        "left-to-person@v2",
        "left-to-person",
    ] {
        assert_eq!(self_claim_act_face(act), Some(TaskFace::Open), "{act}");
    }
    let face = crate::control::harness_act_face;
    assert_eq!(face("set harness_act landed@v1"), Some(TaskFace::Landed));
    assert_eq!(
        face("set harness_act left-to-person@v1"),
        Some(TaskFace::Theirs)
    );
    assert_eq!(face("set  harness_act   continue@v1"), Some(TaskFace::Open));
    assert_eq!(face("set harness_act upgrade@v1"), None);
    assert_eq!(
        face("set harness_act left-to-person@v1 until_us=5000000"),
        Some(TaskFace::Theirs)
    );
    assert_eq!(
        face("set harness_act landed@v1 until_us=0"),
        Some(TaskFace::Landed)
    );
    for rest in [
        "prepare harness_act",
        "unset harness_act",
        "set harness_act",
        "set harness_act landed@v1 extra",
        "set harness_act landed@v1 until_us=bad",
        "set harness_act landed@v1 until_us=1 extra",
        "set harness_acts landed@v1",
        "set autopilot land",
        "",
    ] {
        assert_eq!(face(rest), None, "NEGATIVE CONTROL: {rest:?}");
    }
}

/// Paint Claude Code's screen as session `session`'s whole screen, 24 by
/// 120: its spinner over the prompt while `busy`, else the prompt alone.
fn paint_claude(app: &App, session: u64, busy: bool) {
    let rule = "\u{2500}".repeat(120);
    let mut rows = vec![String::new(); 24];
    rows[0] = "\u{23fa} Working on it.".into();
    if busy {
        rows[19] = "\u{2736} Deliberating\u{2026} (3s \u{00b7} thinking)".into();
    }
    rows[20] = rule.clone();
    rows[21] = "\u{276f} ".into();
    rows[22] = rule;
    rows[23] = if busy {
        "  \u{23f5}\u{23f5} bypass permissions on \u{00b7} esc to interrupt".into()
    } else {
        "  \u{23f5}\u{23f5} bypass permissions on (shift+tab to cycle)".into()
    };
    let pooled = app.pool.get(session).expect("pooled");
    let mut term = crate::term_lock(&pooled.term);
    if term.cols() != 120 {
        term.resize(24, 120);
    }
    let mut bytes = String::from("\x1b[H\x1b[2J");
    for (i, row) in rows.iter().enumerate() {
        bytes.push_str(&format!("\x1b[{};1H{row}", i + 1));
    }
    term.process(bytes.as_bytes());
}

/// THE STATUS SWEEP REOPENS A PROVEN SELF-CLAIM'S LANDING (Autopilot Land
/// §10.4): after the claim's `landed@v1`, the sweep that reads Claude Code
/// busy — someone's later turn — makes the face `open`, through the sweep
/// itself.
#[test]
fn the_status_sweep_reopens_a_proven_self_claims_landing_at_a_later_turn() {
    let mut app = App::headless_for_test();
    let host = idle_host();
    app.harness = Some(host.clone());
    let session = app.next_session_id;
    app.push_stub_tab(crate::WindowId(0), crate::stub_session(session));
    let ctx = app.pool.get(session).unwrap().ctx.clone();
    ctx.meta
        .lock()
        .unwrap()
        .set("autopilot", Some("land".into()));
    let sid = ctx.self_id.as_str().to_string();
    crate::session_timeline::claim_supervisor_as(
        &ctx,
        &format!("codex-alab@{}", std::process::id()),
        Some(1),
        None,
        Some(crate::self_claim::stand_in::running()),
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .expect("a proof takes a free session");
    assert!(app.beside_proven_self_claim(session));
    let face = |app: &App| {
        app.pool
            .get(session)
            .unwrap()
            .ctx
            .timeline
            .lock()
            .unwrap()
            .task_face()
            .map(|(f, _)| f)
    };
    let mut t = Instant::now();
    let mut sweep = |app: &mut App, busy: bool| {
        for _ in 0..3 {
            paint_claude(app, session, busy);
            let _ = app.observe_session_statuses(t);
            t += Duration::from_millis(300);
        }
    };
    sweep(&mut app, false);
    ctx.timeline
        .lock()
        .unwrap()
        .queue_self_task(TaskFace::Landed);
    app.apply_harness_fact(&sid, crate::HarnessFact::SelfTask(TaskFace::Landed));
    assert_eq!(face(&app), Some(TaskFace::Landed));
    sweep(&mut app, false);
    assert_eq!(face(&app), Some(TaskFace::Landed), "quiet: still landed");
    sweep(&mut app, true);
    assert_eq!(
        ctx.timeline.lock().unwrap().agent().word,
        "busy",
        "the sweep read the turn"
    );
    assert_eq!(
        face(&app),
        Some(TaskFace::Open),
        "the sweep's read of a later turn reopens it"
    );
    host.shutdown_and_join();
}

/// THE WIRE OF A PROVEN SELF-CLAIM'S ACT (Autopilot Land): `meta set
/// harness_act landed@v1` on the claim's own connection is recorded, the
/// `meta` arm posts the face it tells, and — its wakes applied as the app
/// loop applies them — `status task=` reads `landed`; the claim's next act
/// (`continue@v1`) reads `open` again.
#[test]
fn a_proven_self_claims_landed_act_on_the_wire_reads_task_landed_then_open() {
    let mut app = App::headless_for_test();
    let host = idle_host();
    app.harness = Some(host.clone());
    under(&mut app, "land");
    let (term, ctx) = {
        let shell = app.pool.get(0).unwrap();
        (Arc::clone(&shell.term), Arc::clone(&shell.ctx))
    };
    crate::session_timeline::claim_supervisor_as(
        &ctx,
        &format!("codex-alab@{}", std::process::id()),
        Some(1),
        None,
        Some(crate::self_claim::stand_in::running()),
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .expect("a proof takes a free session");
    assert!(app.beside_proven_self_claim(0));
    let store = Arc::clone(&app.store);
    let serve =
        |rest: &str, conn| crate::control::meta_wakes_for_test(&store, &term, 0, &ctx, rest, conn);
    let apply = |app: &mut App, wakes: Vec<crate::Wake>| {
        let mut faces = Vec::new();
        for wake in wakes {
            match wake {
                crate::Wake::MetaChanged { session } => app.on_meta_changed(session),
                crate::Wake::HarnessFact { sid, fact } => {
                    if let crate::HarnessFact::SelfTask(face) = &fact {
                        faces.push(*face);
                    }
                    app.apply_harness_fact(&sid, fact);
                }
                other => panic!("the meta arm posts no {other:?}"),
            }
        }
        faces
    };
    let task = |app: &App| {
        let record = app.session_status_record(0).expect("live session");
        record
            .split_whitespace()
            .find_map(|t| t.strip_prefix("task="))
            .unwrap_or("none")
            .to_string()
    };
    let (reply, wakes) = serve("set harness_act landed@v1", Some(2));
    assert_eq!(reply, "ERR denied\n");
    assert!(wakes.is_empty(), "a refused act posts nothing: {wakes:?}");
    assert_ne!(task(&app), "landed");
    let (reply, wakes) = serve("prepare harness_act", Some(1));
    assert!(reply.starts_with("OK until_us="), "{reply}");
    assert!(reply.ends_with(" lease_ms=5000\n"), "{reply}");
    assert!(wakes.is_empty(), "a prepare posts no face: {wakes:?}");
    assert_ne!(task(&app), "landed");
    let (reply, wakes) = serve("set harness_act landed@v1", Some(1));
    assert_eq!(reply, "OK\n");
    assert_eq!(apply(&mut app, wakes), [TaskFace::Landed]);
    assert_eq!(task(&app), "landed");
    let (reply, wakes) = serve("set title building", Some(1));
    assert_eq!(reply, "OK\n");
    assert_eq!(apply(&mut app, wakes), []);
    assert_eq!(task(&app), "landed", "a title is no act");
    let (reply, wakes) = serve("set harness_act continue@v1", Some(1));
    assert_eq!(reply, "OK\n");
    assert_eq!(apply(&mut app, wakes), [TaskFace::Open]);
    assert_eq!(task(&app), "open", "the claim's next act reopens it");
    host.shutdown_and_join();
}

/// A PROVEN SELF-CLAIM'S OWN LANDING SHOWS, AND ANYONE'S LATER TURN REOPENS
/// IT (Autopilot Land): beside a proven self-claim the hosted loop tells
/// no face (it only watches), the claim's `landed@v1` act makes the face
/// `landed` — recorded once as a landing under Land — and its holder's quiet
/// leaves it there; once someone else's turn begins (the agent reads busy)
/// the status sweep's reopen makes it `open`.
#[test]
fn a_proven_self_claims_landing_shows_and_anyones_later_turn_reopens_it() {
    let mut app = App::headless_for_test();
    let host = idle_host();
    app.harness = Some(host.clone());
    let sid = under(&mut app, "land");
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    let face = |app: &App| app.chip_glass(0).task;
    ctx.timeline
        .lock()
        .unwrap()
        .queue_self_task(TaskFace::Landed);
    app.apply_harness_fact(&sid, crate::HarnessFact::SelfTask(TaskFace::Landed));
    assert_eq!(face(&app), None, "no proven self-claim, no face");
    crate::session_timeline::claim_supervisor_as(
        &ctx,
        &format!("codex-alab@{}", std::process::id()),
        Some(1),
        None,
        Some(crate::self_claim::stand_in::running()),
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .expect("a proof takes a free session");
    assert!(app.beside_proven_self_claim(0));
    tell(&mut app, &sid, Some(TaskFace::Theirs));
    assert_eq!(face(&app), None, "the hosted loop tells no face beside it");
    ctx.timeline
        .lock()
        .unwrap()
        .queue_self_task(TaskFace::Landed);
    app.apply_harness_fact(&sid, crate::HarnessFact::SelfTask(TaskFace::Landed));
    assert_eq!(face(&app), Some(TaskFace::Landed));
    ctx.timeline
        .lock()
        .unwrap()
        .queue_self_task(TaskFace::Landed);
    app.apply_harness_fact(&sid, crate::HarnessFact::SelfTask(TaskFace::Landed));
    assert_eq!(
        landing_records(&app, &sid).len(),
        1,
        "a landing, recorded once"
    );
    app.reopen_self_claim_landing(0);
    assert_eq!(face(&app), Some(TaskFace::Landed), "quiet: still landed");
    let publish = |word: &'static str| {
        ctx.timeline.lock().unwrap().publish_agent(
            word,
            None,
            None,
            Some(aterm_phase::Program::Claude),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                fp: 1,
            },
        );
    };
    publish("busy");
    app.reopen_self_claim_landing(0);
    assert_eq!(face(&app), Some(TaskFace::Open), "anyone's turn reopens it");
    assert!(crate::session_timeline::release_supervisor(
        &ctx,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl
    ));
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert_eq!(face(&app), Some(TaskFace::Landed));
    app.reopen_self_claim_landing(0);
    assert_eq!(
        face(&app),
        Some(TaskFace::Landed),
        "the sweep reopens no hosted loop's landing"
    );
    host.shutdown_and_join();
}

/// A PROVEN SELF-CLAIM LANDS NOTHING UNDER CRUISE OR OFF (Autopilot Cruise:
/// nothing ends a Cruise task): its `landed@v1` act, told as
/// `HarnessFact::SelfTask(Landed)` while the session runs under Cruise or
/// Off, leaves the face as it was — no `task=landed`, no landing recorded —
/// where a codex-alab that misbehaved would have set it.
#[test]
fn a_proven_self_claims_landing_counts_only_under_land() {
    for word in ["cruise", "off", "land"] {
        let mut app = App::headless_for_test();
        let host = idle_host();
        app.harness = Some(host.clone());
        let sid = under(&mut app, word);
        let ctx = app.pool.get(0).unwrap().ctx.clone();
        crate::session_timeline::claim_supervisor_as(
            &ctx,
            &format!("codex-alab@{}", std::process::id()),
            Some(1),
            None,
            Some(crate::self_claim::stand_in::running()),
            crate::metrics::now_us(),
            aterm_slog::Actor::Ctl,
        )
        .expect("a proof takes a free session");
        assert!(app.beside_proven_self_claim(0));
        ctx.timeline.lock().unwrap().queue_self_task(TaskFace::Open);
        app.apply_harness_fact(&sid, crate::HarnessFact::SelfTask(TaskFace::Open));
        ctx.timeline
            .lock()
            .unwrap()
            .queue_self_task(TaskFace::Landed);
        app.apply_harness_fact(&sid, crate::HarnessFact::SelfTask(TaskFace::Landed));
        let face = app.chip_glass(0).task;
        let records = landing_records(&app, &sid).len();
        if word == "land" {
            assert_eq!(face, Some(TaskFace::Landed), "CONTROL: {word}");
            assert_eq!(records, 1, "CONTROL: {word}");
        } else {
            assert_eq!(face, Some(TaskFace::Open), "{word}: no landing");
            assert_eq!(records, 0, "{word}: nothing recorded");
        }
        host.shutdown_and_join();
    }
}
