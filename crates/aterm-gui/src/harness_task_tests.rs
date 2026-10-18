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
