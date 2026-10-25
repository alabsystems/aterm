// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::WindowId;
use crate::native_app::{ActionInvocation, AppEvent};
use crate::native_ui::ActionId;

/// SESSIONS' `Every session` row on the App (design §11.5.5).
#[path = "harness_every_tests.rs"]
mod every;

/// The page counts landings (design §12.4).
#[path = "harness_landings_tests.rs"]
mod landings;

/// The page's plain counts (design §14.6).
#[path = "harness_counts_tests.rs"]
mod counts;

#[cfg(target_os = "macos")]
#[test]
fn lan_discovery_uses_only_the_resolved_service_endpoint() {
    assert_eq!(
        bonjour_ssh_target(
            "Office Server._ssh._tcp.local. can be reached at actual-peer.local.:2222 (interface 8)"
        ),
        Some(("actual-peer.local".to_string(), 2222))
    );
    for unresolved in [
        "",
        "Office Server._ssh._tcp.local.",
        "Office Server can be reached at actual-peer.local.:invalid (interface 8)",
        "Office Server can be reached at :22 (interface 8)",
        "Office Server can be reached at /foreign/path:22 (interface 8)",
    ] {
        assert_eq!(bonjour_ssh_target(unresolved), None, "{unresolved}");
    }
}

#[test]
fn durations_and_shares_read_as_the_page_says_them() {
    assert_eq!(duration_words(0), "0m");
    assert_eq!(duration_words(59_999), "<1m");
    assert_eq!(duration_words(60_000), "1m");
    assert_eq!(duration_words(3_600_000), "1h");
    assert_eq!(duration_words(7_500_000), "2h 5m");
    assert_eq!(share_words(3, 100), "3%");
    assert_eq!(share_words(1, 1_000), "<1%");
    assert_eq!(share_words(999, 1_000), ">99%");
    assert_eq!(share_words(1_000, 1_000), "100%");
    assert_eq!(share_words(0, 0), "0%");
}

#[test]
fn a_reading_says_its_clock_time() {
    let at = 1_000_000_000;
    assert_eq!(read_at_words(at, Some(0)), "at 13:46");
    assert_eq!(read_at_words(at, Some(-5 * 3_600)), "at 08:46");
    assert_eq!(read_at_words(at, Some(11 * 3_600)), "at 00:46");
    assert_eq!(read_at_words(at, None), "at 13:46 UTC");
}

#[test]
fn today_starts_at_the_local_midnight() {
    const DAY: i64 = 86_400_000;
    const HOUR: i64 = 3_600_000;
    let fixed = |offset_s: Option<i64>| move |_: i64| offset_s;
    let now = 2 * DAY + HOUR;
    assert_eq!(local_midnight_ms(now, fixed(None)), 2 * DAY);
    assert_eq!(local_midnight_ms(now, fixed(Some(0))), 2 * DAY);
    assert_eq!(local_midnight_ms(now, fixed(Some(-7_200))), DAY + 2 * HOUR);
    assert_eq!(
        local_midnight_ms(now, fixed(Some(7_200))),
        2 * DAY - 2 * HOUR
    );

    let spring = 10 * DAY + 7 * HOUR;
    let fall = 20 * DAY + 6 * HOUR;
    let zone = move |unix_s: i64| {
        let ms = unix_s * 1000;
        Some(if (spring..fall).contains(&ms) {
            -4 * 3_600
        } else {
            -5 * 3_600
        })
    };
    let morning = 10 * DAY + 14 * HOUR;
    assert_eq!(local_midnight_ms(morning, zone), 10 * DAY + 5 * HOUR);
    assert_eq!(
        local_midnight_ms(morning, fixed(Some(-4 * 3_600))),
        10 * DAY + 4 * HOUR,
        "now's offset alone: an hour early"
    );
    let morning = 20 * DAY + 15 * HOUR;
    assert_eq!(local_midnight_ms(morning, zone), 20 * DAY + 4 * HOUR);
    assert_eq!(
        local_midnight_ms(morning, fixed(Some(-5 * 3_600))),
        20 * DAY + 5 * HOUR,
        "now's offset alone: an hour late"
    );
    let morning = 15 * DAY + 14 * HOUR;
    assert_eq!(local_midnight_ms(morning, zone), 15 * DAY + 4 * HOUR);
}

#[test]
fn a_midnight_that_comes_twice_starts_the_day_at_the_first() {
    const DAY: i64 = 86_400_000;
    const HOUR: i64 = 3_600_000;
    let spring = 10 * DAY + 5 * HOUR;
    let fall = 20 * DAY + 5 * HOUR;
    let zone = move |unix_s: i64| {
        let ms = unix_s * 1000;
        Some(if (spring..fall).contains(&ms) {
            -4 * 3_600
        } else {
            -5 * 3_600
        })
    };
    let first = 20 * DAY + 4 * HOUR;
    for now in [
        20 * DAY + 17 * HOUR,
        20 * DAY + 4 * HOUR + 1_800_000,
        20 * DAY + 5 * HOUR + 1_800_000,
    ] {
        assert_eq!(local_midnight_ms(now, zone), first, "{now}");
    }
    assert_eq!(
        local_midnight_ms(20 * DAY + 17 * HOUR, |_: i64| Some(-5 * 3_600)),
        first + HOUR,
        "now's offset alone: the second 00:00, an hour late"
    );
    for now in [10 * DAY + 16 * HOUR, spring + 1_800_000] {
        assert_eq!(local_midnight_ms(now, zone), spring, "{now}");
    }
    assert_eq!(
        local_midnight_ms(21 * DAY + 17 * HOUR, zone),
        21 * DAY + 5 * HOUR
    );
    assert_eq!(
        local_midnight_ms(11 * DAY + 16 * HOUR, zone),
        11 * DAY + 4 * HOUR
    );
}

#[test]
fn the_collector_reads_a_state_tree_and_creates_nothing() {
    use aterm_agent::supervise::approvals::{Outcome, Row};
    let root = aterm_tempfile::TempDir::new().unwrap();
    let state = root.path().join("state");
    let harness = root.path().join("harness");
    let now = 10 * 86_400_000_i64 + 12 * 3_600_000;
    let utc = |_: i64| Some(0);
    let empty = collect_harness_stats(Some(&state), Some(&harness), now, utc).unwrap();
    assert_eq!(
        empty.offset_s,
        Some(0),
        "now's offset, for the reading's time"
    );
    assert!(empty.today.is_empty() && empty.week.is_empty());
    assert_eq!(empty.collected_unix_ms, now);
    assert!(!state.exists() && !harness.exists(), "nothing was made");
    assert_eq!(
        std::fs::read_dir(root.path()).unwrap().count(),
        0,
        "the root holds nothing"
    );

    let drive = state.join("drive");
    std::fs::create_dir_all(&drive).unwrap();
    let approved = |ts: i64| {
        Row {
            rule_id: "read-only@v1",
            outcome: Outcome::Approved,
            command: "ls",
            reason: "",
            box_seq: 1,
        }
        .to_json(ts, Some("s-1"))
    };
    let lines = [
        approved(now - 10 * 86_400_000),
        approved(now - 3 * 86_400_000),
        approved(now - 3_600_000),
    ];
    std::fs::write(drive.join("s-1.jsonl"), lines.join("\n") + "\n").unwrap();
    let read = collect_harness_stats(Some(&state), Some(&harness), now, utc).unwrap();
    assert_eq!((read.today.approved, read.week.approved), (1, 2));
    assert!(!harness.exists(), "the harness state was not made");
    assert_eq!(
        collect_harness_stats(None, Some(&harness), now, |_: i64| None),
        Err("no state directory resolves here".to_string())
    );
}

#[test]
fn the_effective_table_is_every_key_against_the_default() {
    let mut cfg = SupervisorConfig::default();
    cfg.set("approve", "safe").unwrap();
    cfg.set("continue_text", "carry on").unwrap();
    cfg.set("codex.enabled", "false").unwrap();
    let rows = config_rows(&cfg);
    let keys: Vec<&str> = rows.iter().map(|row| row.key.as_str()).collect();
    let agents: Vec<String> = aterm_phase::Program::ALL
        .into_iter()
        .filter(|agent| agent.harness_table())
        .map(|agent| format!("{}.enabled", agent.name()))
        .collect();
    let mut want = vec!["enabled"];
    want.extend(agents.iter().map(String::as_str));
    want.extend(&aterm_agent::supervise::config::KEYS[1..]);
    assert_eq!(keys, want);
    let yours: Vec<&str> = rows
        .iter()
        .filter(|row| !row.is_default)
        .map(|row| row.key.as_str())
        .collect();
    assert_eq!(yours, ["codex.enabled", "approve", "continue_text"]);
    let value = |key: &str| &rows.iter().find(|row| row.key == key).unwrap().value;
    assert_eq!(value("approve"), "safe");
    assert_eq!(value("codex.enabled"), "false");
    assert_eq!(value("claude.enabled"), "true");
    let retired = crate::app_config::HarnessPolicy::read("[harness]\nrm_breaker = false\n");
    let rows = config_rows(&retired.policy);
    let last = rows.last().unwrap();
    assert_eq!(
        (last.key.as_str(), last.value.as_str(), last.is_default),
        ("rm_breaker", "false", false)
    );
}

fn idle_host() -> crate::harness_host::HostHandle {
    crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    )
}

fn run_claude(app: &App, session: u64) {
    let ctx = app.pool.get(session).unwrap().ctx.clone();
    let mut tl = ctx.timeline.lock().unwrap();
    tl.note_foreground_group(7);
    tl.set_program(7, Some("claude".into()));
}

#[test]
fn a_press_on_a_sessions_land_writes_its_word() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    run_claude(&app, 0);
    let press = |app: &mut App, action: &str| {
        let (_, view) = app.active_native_view(wid).expect("the Settings view");
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new(action),
                value: None,
            }),
        )
        .expect("dispatched");
    };

    assert!(app.open_harness_settings(wid, crate::harness_screen::HarnessFocus::Top));
    let state = app.harness_state();
    assert_eq!(state.sessions.len(), 1);
    assert_eq!(state.sessions[0].program, "Claude Code");
    assert_eq!(state.sessions[0].view, AutopilotView::Unhosted);
    press(&mut app, "harness/session/0/autopilot/land");
    assert_eq!(app.session_autopilot(0).0, None, "no host: nothing written");

    let host = idle_host();
    app.harness = Some(host.clone());
    app.publish_native_harness_state();
    let revision = app.harness_screen.revision;
    press(&mut app, "harness/session/0/autopilot/land");
    assert_eq!(
        app.session_autopilot(0),
        (Some(Autopilot::Land), AutopilotView::Land, false)
    );
    assert!(
        app.harness_screen.revision > revision,
        "the page was republished with the word"
    );
    let state = app.harness_state();
    assert_eq!(state.sessions[0].word, Some(Autopilot::Land));
    assert_eq!(state.sessions[0].view, AutopilotView::Land);
    host.shutdown_and_join();
}

#[test]
fn the_page_lists_agy_and_muse_code_sessions_and_no_shells() {
    for (program, listed) in [
        ("agy", Some("Antigravity CLI")),
        ("muse", Some("Muse Code")),
        ("zsh", None),
    ] {
        let mut app = App::headless_for_test();
        {
            let ctx = app.pool.get(0).unwrap().ctx.clone();
            let mut tl = ctx.timeline.lock().unwrap();
            tl.note_foreground_group(7);
            tl.set_program(7, Some(program.into()));
        }
        assert!(app.open_harness_settings(WindowId(0), crate::harness_screen::HarnessFocus::Top));
        let state = app.harness_state();
        assert_eq!(
            state.sessions.first().map(|s| s.program),
            listed,
            "{program}"
        );
    }
}

#[test]
fn the_page_lists_an_agent_session_read_under_node() {
    for (program, reader, listed) in [
        ("node", Some(aterm_phase::Program::Muse), Some("Muse Code")),
        ("node", None, None),
        ("zsh", Some(aterm_phase::Program::Muse), None),
        ("gemini", None, None),
        ("gemini", Some(aterm_phase::Program::Generic), None),
    ] {
        let mut app = App::headless_for_test();
        {
            let ctx = app.pool.get(0).unwrap().ctx.clone();
            let mut tl = ctx.timeline.lock().unwrap();
            tl.note_foreground_group(7);
            tl.set_program(7, Some(program.into()));
            tl.publish_agent(
                "idle",
                None,
                None,
                reader,
                crate::session_timeline::AgentStamp {
                    generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                    fp: 1,
                },
            );
        }
        assert!(app.open_harness_settings(WindowId(0), crate::harness_screen::HarnessFocus::Top));
        let state = app.harness_state();
        assert_eq!(
            state.sessions.first().map(|s| s.program),
            listed,
            "{program} {reader:?}"
        );
    }
}

#[test]
fn a_reading_for_the_running_ticket_is_kept() {
    let mut app = App::headless_for_test();
    let report = |approved| {
        let mut today = HarnessStats::new(0);
        today.approved = approved;
        today.rows = 1;
        Box::new(HarnessStatsReport::new(
            today.clone(),
            today,
            0,
            None,
            &|_| None,
        ))
    };
    app.finish_harness_stats(app.harness_screen.sequence, Ok(report(1)));
    assert!(app.harness_screen.stats.report.is_none());
    app.harness_screen.sequence = 2;
    app.harness_screen.stats.collecting = true;
    app.finish_harness_stats(1, Ok(report(1)));
    assert!(app.harness_screen.stats.collecting && app.harness_screen.stats.report.is_none());
    app.finish_harness_stats(2, Ok(report(5)));
    assert!(!app.harness_screen.stats.collecting);
    assert_eq!(
        app.harness_screen
            .stats
            .report
            .as_ref()
            .map(|r| r.today.approved),
        Some(5)
    );
    app.harness_screen.sequence = 3;
    app.harness_screen.stats.collecting = true;
    app.finish_harness_stats(3, Err("no state directory resolves here".to_string()));
    assert_eq!(
        app.harness_screen.stats.unavailable.as_deref(),
        Some("no state directory resolves here")
    );
    assert!(
        app.harness_screen.stats.report.is_some(),
        "the last reading stands"
    );
    app.start_harness_stats_refresh();
    assert!(!app.harness_screen.stats.collecting);
}

fn publish(ctx: &crate::SessionCtx, word: &'static str) -> bool {
    ctx.timeline.lock().unwrap().publish_agent(
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

fn claim(ctx: &crate::SessionCtx) {
    crate::session_timeline::claim_supervisor(
        ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .unwrap();
}

fn stretch(app: &App, session: u64) -> Option<(AbPhase, AbArm)> {
    app.harness_screen
        .ab
        .stretch(session)
        .map(|(phase, arm, _)| (phase, arm))
}

#[test]
fn the_app_feeds_the_ab_clock_at_each_change() {
    let mut app = App::headless_for_test();
    run_claude(&app, 0);
    let host = idle_host();
    app.harness = Some(host.clone());
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    claim(&ctx);
    assert_eq!(app.harness_ab_arm(0), AbArm::Cruise);
    assert!(publish(&ctx, "busy"));
    app.note_harness_phase(0);
    assert_eq!(stretch(&app, 0), Some((AbPhase::Busy, AbArm::Cruise)));
    assert!(app.set_session_autopilot(
        0,
        Some(Autopilot::Land),
        crate::autopilot::Surface::Settings
    ));
    assert_eq!(stretch(&app, 0), Some((AbPhase::Busy, AbArm::Land)));
    assert!(app.set_session_autopilot(
        0,
        Some(Autopilot::Off),
        crate::autopilot::Surface::Settings
    ));
    assert_eq!(stretch(&app, 0), Some((AbPhase::Busy, AbArm::Unsupervised)));
    assert!(publish(&ctx, "idle"));
    app.note_harness_phase(0);
    assert_eq!(
        stretch(&app, 0),
        Some((AbPhase::AtPrompt, AbArm::Unsupervised))
    );
    app.retire_harness_ab(0);
    assert_eq!(app.harness_screen.ab.open_stretches(), 0);
    host.shutdown_and_join();
}

#[test]
fn the_face_and_the_pause_told_move_the_books_cause() {
    use aterm_agent::harness::acts::TaskFace;
    let mut app = App::headless_for_test();
    run_claude(&app, 0);
    let host = idle_host();
    app.harness = Some(host.clone());
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    let sid = ctx.self_id.as_str().to_string();
    claim(&ctx);
    assert!(publish(&ctx, "idle"));
    app.note_harness_phase(0);
    let open = |app: &App| app.harness_screen.ab.stretch(0);
    assert_eq!(open(&app), Some((AbPhase::AtPrompt, AbArm::Cruise, None)));
    let plan = crate::PausePlan {
        at_unix: 1_792_262_513,
        short: 2,
    };
    let cause = |app: &App| open(app).and_then(|(_, _, cause)| cause);
    app.apply_harness_fact(&sid, crate::HarnessFact::Pause(Some(plan)));
    assert_eq!(cause(&app), Some(AbCause::Pausing));
    app.apply_harness_fact(&sid, crate::HarnessFact::Task(Some(TaskFace::Landed)));
    assert_eq!(cause(&app), Some(AbCause::Pausing), "Cruise never lands");
    app.apply_harness_fact(&sid, crate::HarnessFact::Pause(None));
    assert_eq!(cause(&app), None);
    assert!(app.set_session_autopilot(0, Some(Autopilot::Land), crate::autopilot::Surface::Chip));
    app.note_harness_arm(0);
    app.apply_harness_fact(&sid, crate::HarnessFact::Task(Some(TaskFace::Open)));
    app.apply_harness_fact(&sid, crate::HarnessFact::Task(Some(TaskFace::Landed)));
    assert_eq!(
        open(&app),
        Some((AbPhase::AtPrompt, AbArm::Land, Some(AbCause::Landed)))
    );
    app.apply_harness_fact(&sid, crate::HarnessFact::Pause(Some(plan)));
    assert_eq!(cause(&app), Some(AbCause::Landed));
    app.apply_harness_fact(&sid, crate::HarnessFact::Task(Some(TaskFace::Open)));
    assert_eq!(cause(&app), Some(AbCause::Pausing));
    app.apply_harness_fact(&sid, crate::HarnessFact::Pause(None));
    assert_eq!(cause(&app), None);
    assert!(app.set_session_autopilot(
        0,
        Some(Autopilot::Off),
        crate::autopilot::Surface::Settings
    ));
    crate::session_timeline::release_supervisor(
        &ctx,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    );
    app.note_harness_arm(0);
    app.apply_harness_fact(&sid, crate::HarnessFact::Pause(Some(plan)));
    assert_eq!(open(&app), Some((AbPhase::AtPrompt, AbArm::Off, None)));
    host.shutdown_and_join();
}

#[test]
fn a_moved_wall_reads_the_books_calendar_again_once_it_is_fed() {
    use crate::autopilot_timers::set_fake_clocks;
    let mut app = App::headless_for_test();
    run_claude(&app, 0);
    let wall = 1_792_177_200_000_i64;
    set_fake_clocks(Some((std::time::Instant::now(), wall)));
    app.on_wall_moved();
    assert!(
        !app.harness_screen.ab.has_calendar(),
        "nothing fed, nothing read"
    );
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    assert!(publish(&ctx, "busy"));
    app.note_harness_phase(0);
    assert!(app.harness_screen.ab.has_calendar(), "read where first fed");
    let today = |app: &App| {
        app.harness_screen
            .ab
            .ring_at(crate::turn_ledger::now_ms())
            .map(|(_, today)| today)
    };
    let first = today(&app).expect("a calendar");
    set_fake_clocks(Some((std::time::Instant::now(), wall + 3 * 86_400_000)));
    assert_eq!(today(&app), Some(first), "no wake, no move");
    app.on_wall_moved();
    assert_eq!(today(&app), Some(first + 3));
    set_fake_clocks(None);
}

#[test]
fn the_exit_seam_writes_the_live_book() {
    use crate::harness_ab::{AbCalendar, bucket};
    use crate::harness_ab_store::{dir_of, own_name, read_others};
    let state = aterm_tempfile::tempdir().expect("scratch state");
    crate::harness_host::with_harness_state(state.path(), || {
        let mut app = App::headless_for_test();
        let own = dir_of(state.path()).join(own_name());
        app.write_harness_ab_at_exit();
        assert!(!own.exists(), "nothing fed, nothing written");
        let book = &mut app.harness_screen.ab;
        book.set_calendar(AbCalendar::read(
            0,
            unix_ms_now(),
            crate::autopilot_timers::local_offset_at,
        ));
        book.observe(1, Some(AbPhase::Busy), None, AbArm::Off, 0);
        book.observe(1, Some(AbPhase::AtBox), None, AbArm::Off, 1_000);
        book.observe(1, Some(AbPhase::Busy), None, AbArm::Off, 3_000);
        book.retire(1, 4_000);
        app.write_harness_ab_at_exit();
        assert!(own.exists(), "the exit seam wrote it");
        let today = app
            .harness_screen
            .ab
            .ring_at(crate::turn_ledger::now_ms())
            .map(|(_, today)| today)
            .expect("a calendar");
        let read = read_others(state.path(), "0-0.json", today);
        let off = |day: &crate::harness_ab::AbDay| day.arms[AbArm::Off.index()];
        let (at_box, busy): (u64, u64) = read
            .iter()
            .map(|day| (off(day).at_box_ms, off(day).busy_ms))
            .fold((0, 0), |(a, b), (x, y)| (a + x, b + y));
        assert_eq!((at_box, busy), (2_000, 2_000));
        let boxes: u32 = read.iter().map(|day| day.holds[2][0][bucket(2_000)]).sum();
        assert_eq!(boxes, 1);
    });
}

fn off_day(day: i64, first_unix_ms: i64, busy_ms: u64) -> crate::harness_ab::AbDay {
    let mut kept = crate::harness_ab::AbDay::new(day, first_unix_ms);
    kept.arms[AbArm::Off.index()].busy_ms = busy_ms;
    kept
}

fn ring_of(day: crate::harness_ab::AbDay) -> crate::harness_ab::AbRing {
    use crate::harness_ab::DAYS;
    let mut ring: crate::harness_ab::AbRing = [None; DAYS];
    ring[usize::try_from(day.day.rem_euclid(DAYS as i64)).unwrap()] = Some(day);
    ring
}

#[test]
fn a_reading_writes_this_books_file_and_reads_the_others() {
    use crate::harness_ab_store::{dir_of, read_others, write_own};
    const DAY: i64 = 20_002;
    let noon = DAY * 86_400_000 + 43_200_000;
    let utc: fn(i64) -> Option<i64> = |_| Some(0);
    let state = aterm_tempfile::tempdir().expect("scratch state");
    let hs = aterm_tempfile::tempdir().expect("scratch harness state");
    let file = |name: &str| dir_of(hs.path()).join(name);
    write_own(
        hs.path(),
        "1-1.json",
        &ring_of(off_day(DAY, 10, 5_000)),
        DAY,
        0,
    )
    .unwrap();
    let writes = std::sync::Arc::new(crate::harness_ab_store::AbWrites::default());
    let mine = writes
        .snapshot((ring_of(off_day(DAY, 20, 7_000)), DAY))
        .unwrap();
    let report = read_harness_stats(
        Some(state.path()),
        Some(hs.path()),
        Some(&mine),
        "2-2.json",
        noon,
        utc,
        &FleetSlot::default(),
    )
    .expect("a reading");
    assert_eq!(report.ab_others, [off_day(DAY, 10, 5_000)], "never its own");
    assert!(file("2-2.json").exists(), "its own written");
    let unread = writes
        .snapshot((ring_of(off_day(DAY, 20, 7_000)), DAY))
        .unwrap();
    let failed = read_harness_stats(
        None,
        Some(hs.path()),
        Some(&unread),
        "3-3.json",
        noon,
        utc,
        &FleetSlot::default(),
    );
    assert!(failed.is_err());
    assert!(file("3-3.json").exists());
    let unfed = read_harness_stats(
        Some(state.path()),
        Some(hs.path()),
        None,
        "4-4.json",
        noon,
        utc,
        &FleetSlot::default(),
    )
    .expect("a reading");
    assert!(!file("4-4.json").exists());
    assert_eq!(unfed.ab_others, read_others(hs.path(), "4-4.json", DAY));
    assert_eq!(unfed.ab_others[0].arms[AbArm::Off.index()].busy_ms, 19_000);
    let none = read_harness_stats(
        Some(state.path()),
        None,
        Some(&mine),
        "5-5.json",
        noon,
        utc,
        &FleetSlot::default(),
    );
    assert!(none.is_err());
    assert!(!file("5-5.json").exists());
}

#[test]
fn the_page_adds_the_other_processes_days_to_its_week() {
    use crate::harness_ab::AbCalendar;
    let mut app = App::headless_for_test();
    let wall = unix_ms_now();
    let book = &mut app.harness_screen.ab;
    book.set_calendar(AbCalendar::read(
        0,
        wall,
        crate::autopilot_timers::local_offset_at,
    ));
    book.observe(1, Some(AbPhase::Busy), None, AbArm::Off, 0);
    book.retire(1, 2_000);
    let today = book
        .ring_at(crate::turn_ledger::now_ms())
        .map(|(_, today)| today)
        .expect("a calendar");
    let alone = app.harness_ab();
    assert_eq!(alone.arm(AbArm::Off).busy_ms, 2_000, "no reading yet");
    let earlier = wall - 3 * 86_400_000;
    let mut today_stats = HarnessStats::new(0);
    today_stats.rows = 0;
    let mut report =
        HarnessStatsReport::new(today_stats.clone(), today_stats, wall, Some(0), &|_| {
            Some(0)
        });
    report.ab_others = vec![
        off_day(today - 7, earlier - 86_400_000, 100_000),
        off_day(today - 3, earlier, 5_000),
        off_day(today, wall, 1_000),
    ];
    app.harness_screen.stats.report = Some(std::sync::Arc::new(report));
    let week = app.harness_ab();
    assert_eq!(week.arm(AbArm::Off).busy_ms, 8_000, "the week's three");
    assert_eq!(week.first, Some((today - 3, earlier)));
}

#[test]
fn the_ab_arm_reads_whose_claim_is_live() {
    let mut app = App::headless_for_test();
    run_claude(&app, 0);
    let host = idle_host();
    app.harness = Some(host.clone());
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    let now_us = crate::metrics::now_us();
    claim(&ctx);
    assert_eq!(app.harness_ab_arm(0), AbArm::Cruise);
    assert!(app.set_session_autopilot(
        0,
        Some(Autopilot::Off),
        crate::autopilot::Surface::Settings
    ));
    assert_eq!(app.harness_ab_arm(0), AbArm::Unsupervised, "ours, live");
    crate::session_timeline::release_supervisor(&ctx, None, now_us, aterm_slog::Actor::Ctl);
    crate::session_timeline::claim_supervisor(&ctx, "another-host", None, None, now_us).unwrap();
    assert_eq!(
        app.harness_ab_arm(0),
        AbArm::Unsupervised,
        "another's, live"
    );
    crate::session_timeline::release_supervisor(&ctx, None, now_us, aterm_slog::Actor::Ctl);
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        Some(1),
        0,
    )
    .unwrap();
    assert_eq!(app.harness_ab_arm(0), AbArm::Off, "ours, lapsed");
    host.shutdown_and_join();
}

fn busy_screen() -> Vec<String> {
    let rule = "\u{2500}".repeat(120);
    let mut rows = vec![String::new(); 24];
    rows[0] = "\u{23fa} Working on it.".into();
    rows[19] = "\u{2736} Deliberating\u{2026} (3s \u{00b7} thinking)".into();
    rows[20] = rule.clone();
    rows[21] = "\u{276f} ".into();
    rows[22] = rule;
    rows[23] = "  \u{23f5}\u{23f5} bypass permissions on \u{00b7} esc to interrupt".into();
    rows
}

fn paint(app: &App, session: u64, rows: &[String]) {
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

#[test]
fn the_sweep_the_meta_wake_and_the_policy_feed_the_ab_clock() {
    use std::time::{Duration, Instant};
    let mut app = App::headless_for_test();
    let session = app.next_session_id;
    app.push_stub_tab(WindowId(0), crate::stub_session(session));
    let host = idle_host();
    app.harness = Some(host.clone());
    let ctx = app.pool.get(session).unwrap().ctx.clone();
    claim(&ctx);

    assert_eq!(stretch(&app, session), None, "nothing read yet");
    let mut t = Instant::now();
    for _ in 0..3 {
        paint(&app, session, &busy_screen());
        let _ = app.observe_session_statuses(t);
        t += Duration::from_millis(300);
    }
    assert_eq!(ctx.timeline.lock().unwrap().agent().word, "busy");
    assert_eq!(
        stretch(&app, session),
        Some((AbPhase::Busy, AbArm::Cruise)),
        "the sweep's publish opened the stretch, on Cruise under the claim"
    );

    crate::session_timeline::release_supervisor(
        &ctx,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    );
    assert_eq!(stretch(&app, session), Some((AbPhase::Busy, AbArm::Cruise)));
    app.on_meta_changed(session);
    assert_eq!(
        stretch(&app, session),
        Some((AbPhase::Busy, AbArm::Unsupervised)),
        "Cruise with no claim is unsupervised"
    );

    let mut stamps = crate::harness_policy_cell::HarnessStampSource::new();
    let off = stamps.stamp(crate::app_config::HarnessPolicy::read(
        "[harness]\nautopilot = \"off\"\n",
    ));
    assert_eq!(
        stretch(&app, session),
        Some((AbPhase::Busy, AbArm::Unsupervised))
    );
    app.adopt_harness_policy(off);
    assert_eq!(
        stretch(&app, session),
        Some((AbPhase::Busy, AbArm::Off)),
        "the default off and no claim: plain Claude Code"
    );
    host.shutdown_and_join();
}

#[test]
fn a_lapsed_claim_moves_the_ab_arm_as_a_released_one_does() {
    use aterm_messages::presence::drive::Desk as _;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(session));
    run_claude(&app, session);
    let host = idle_host();
    app.harness = Some(host.clone());
    let ctx = app.pool.get(session).unwrap().ctx.clone();
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Off),
        crate::autopilot::Surface::Settings
    ));
    let holder = "aterm-supervise:42";
    let now = crate::metrics::now_us();
    crate::session_timeline::claim_supervisor(&ctx, holder, None, Some(now + 600_000_000), now)
        .unwrap();
    app.on_meta_changed(session);
    assert!(publish(&ctx, "idle"));
    app.note_harness_phase(session);
    assert_eq!(
        stretch(&app, session),
        Some((AbPhase::AtPrompt, AbArm::Unsupervised)),
        "Off behind another's claim"
    );
    assert!(app.open_harness_settings(wid, crate::harness_screen::HarnessFocus::Top));
    app.publish_native_harness_state();

    let now = crate::metrics::now_us();
    crate::session_timeline::claim_supervisor(&ctx, holder, None, Some(now - 1), now - 2).unwrap();
    assert_eq!(
        stretch(&app, session),
        Some((AbPhase::AtPrompt, AbArm::Unsupervised)),
        "NEGATIVE CONTROL: not lapsed yet"
    );
    let revision = app.harness_screen.revision;
    app.lapse_supervisor(session);
    assert!(
        ctx.meta.lock().unwrap().supervisor.is_none(),
        "the timer lapsed it"
    );
    assert_eq!(
        stretch(&app, session),
        Some((AbPhase::AtPrompt, AbArm::Off)),
        "the Off baseline's wait from the lapse"
    );
    assert!(
        app.harness_screen.revision > revision,
        "the lapse republished the page"
    );
    host.shutdown_and_join();
}

#[test]
fn a_closed_tab_leaves_the_harness_page_in_the_publish_its_close_makes() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(session));
    run_claude(&app, session);
    assert!(app.open_harness_settings(wid, crate::harness_screen::HarnessFocus::Top));
    app.publish_native_harness_state();
    let published = |app: &App| -> Vec<u64> {
        let instance = app
            .native_runtime
            .instance_by_kind(crate::native_app::AppKind::Settings)
            .expect("the Settings controller");
        let Some(crate::native_app::NativeApp::Settings(settings)) =
            app.native_runtime.app(instance)
        else {
            unreachable!("a Settings instance")
        };
        settings
            .harness_snapshot()
            .sessions
            .iter()
            .map(|s| s.local_id)
            .collect()
    };
    assert_eq!(published(&app), [session], "listed while open");
    let revision = app.harness_screen.revision;
    assert!(!app.close_tab_at(wid, 1), "not the window's last tab");
    assert!(app.pool.get(session).is_none(), "the session is gone");
    assert!(
        app.harness_screen.revision > revision,
        "the close republished the page"
    );
    assert_eq!(published(&app), Vec::<u64>::new(), "no ghost row");
}

#[test]
fn the_wire_arms_a_timer_and_the_page_reads_it_in_the_same_publish() {
    use crate::autopilot_timers::{local_offset_at, set_fake_clocks};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(session));
    run_claude(&app, session);
    let host = idle_host();
    app.harness = Some(host.clone());
    let fri = 1_792_177_200_i64;
    let off = local_offset_at(fri).expect("the zone reads");
    let day = (fri + off).div_euclid(86_400);
    let noon = (day * 86_400 + 12 * 3_600 - off) * 1000;
    set_fake_clocks(Some((std::time::Instant::now(), noon)));
    assert!(app.open_harness_settings(wid, crate::harness_screen::HarnessFocus::Top));
    app.publish_native_harness_state();
    let row = |app: &App| {
        let instance = app
            .native_runtime
            .instance_by_kind(crate::native_app::AppKind::Settings)
            .expect("the Settings controller");
        let Some(crate::native_app::NativeApp::Settings(settings)) =
            app.native_runtime.app(instance)
        else {
            unreachable!("a Settings instance")
        };
        let projection = settings.harness_snapshot().projection();
        let row = projection
            .sessions
            .iter()
            .find(|row| row.local_id == session)
            .expect("the session's row")
            .clone();
        row.state_line()
    };
    assert_eq!(row(&app), None, "CONTROL: no timer, nothing to say");
    let wire = |app: &mut App, rest: &str| {
        let s = app.pool.get(session).unwrap();
        let (term, ctx) = (s.term.clone(), s.ctx.clone());
        let (reply, changed) = crate::control::cmd_meta(&term, &app.store, session, &ctx, rest);
        assert!(reply.starts_with("OK"), "{reply}");
        assert!(changed, "{rest}");
    };

    wire(&mut app, "set autopilot cruise until=+2m");
    let revision = app.harness_screen.revision;
    app.on_meta_changed(session);
    assert_eq!(
        app.harness_screen.revision,
        revision + 1,
        "the arm published the page once"
    );
    assert_eq!(
        row(&app).as_deref(),
        Some("lands at 12:02"),
        "the row reads the timer in the publish the write made"
    );

    wire(&mut app, "set autopilot cruise");
    app.on_meta_changed(session);
    assert_eq!(row(&app), None, "the timer dropped");
    wire(&mut app, "set autopilot cruise until=+2m");
    app.note_harness_arm(session);
    app.harness_screen_moved();
    app.index_autopilot_timer(session, None);
    assert_eq!(
        row(&app),
        None,
        "indexed after the publish, the row lags the chip"
    );
    assert!(
        app.autopilot_timers.glass(session).is_some(),
        "CONTROL: the chip has it"
    );
    set_fake_clocks(None);
    host.shutdown_and_join();
}

#[test]
fn a_stale_autopilot_request_is_refused_by_the_app() {
    use crate::native_app::HarnessRequest;
    let mut app = App::headless_for_test();
    let land = HarnessRequest::Autopilot {
        session: 0,
        word: Autopilot::Land,
    };
    assert_eq!(app.harness_posture(), HarnessPosture::Unhosted);
    app.execute_native_harness(land);
    assert_eq!(app.session_autopilot(0).0, None, "no host: nothing written");

    let host = idle_host();
    app.harness = Some(host.clone());
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
    );
    assert_eq!(app.harness_posture(), HarnessPosture::MasterOff);
    app.execute_native_harness(land);
    assert_eq!(
        app.session_autopilot(0).0,
        None,
        "the master switch off: nothing written"
    );

    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read(""),
    );
    app.execute_native_harness(land);
    assert_eq!(app.session_autopilot(0).0, Some(Autopilot::Land));
    host.shutdown_and_join();
}

#[test]
fn an_autopilot_request_for_a_switched_off_agent_is_refused_by_the_app() {
    use crate::native_app::HarnessRequest;
    let land = HarnessRequest::Autopilot {
        session: 0,
        word: Autopilot::Land,
    };
    let written = |policy: &str| {
        let mut app = App::headless_for_test();
        run_claude(&app, 0);
        let host = idle_host();
        app.harness = Some(host.clone());
        app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
            crate::app_config::HarnessPolicy::read(policy),
        );
        assert!(
            matches!(app.harness_posture(), HarnessPosture::Restricted(..)),
            "{policy}"
        );
        app.execute_native_harness(land);
        let word = app.session_autopilot(0).0;
        let row = app.harness_state().sessions[0].default;
        host.shutdown_and_join();
        (word, row)
    };
    assert_eq!(written("[harness.claude]\nenabled = false\n"), (None, None));
    assert_eq!(
        written("[harness.codex]\nenabled = false\n"),
        (Some(Autopilot::Land), Some(Autopilot::Cruise))
    );
}

#[test]
fn an_agents_own_switch_off_is_named_on_its_chip_and_its_row() {
    use aterm_phase::Program;
    let read = |policy: &str| {
        let mut app = App::headless_for_test();
        run_claude(&app, 0);
        let host = idle_host();
        app.harness = Some(host.clone());
        app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
            crate::app_config::HarnessPolicy::read(policy),
        );
        let view = app.session_autopilot(0).1;
        let row = app.harness_state().projection().sessions[0].clone();
        host.shutdown_and_join();
        (view, row.detail, row.mode, row.default)
    };
    let (view, detail, mode, default) = read("[harness.claude]\nenabled = false\n");
    assert_eq!(view, AutopilotView::AgentOff(Program::Claude));
    assert!(
        detail.ends_with("Supervise Claude Code sessions is off"),
        "{detail}"
    );
    assert_eq!((mode, default), (None, None), "its buttons are inert");
    let (view, detail, ..) = read("[harness.codex]\nenabled = false\n");
    assert_eq!(view, AutopilotView::Cruise, "NEGATIVE CONTROL: {detail}");
    let (view, detail, ..) = read("[harness]\nenabled = false\n");
    assert_eq!(view, AutopilotView::MasterOff);
    assert!(detail.ends_with("harness off in Settings"), "{detail}");
}

#[test]
fn the_defaults_button_hands_a_session_back_to_the_default() {
    use crate::native_app::HarnessRequest;
    let mut app = App::headless_for_test();
    let host = idle_host();
    app.harness = Some(host.clone());
    assert_eq!(app.harness_posture(), HarnessPosture::On(Autopilot::Cruise));
    let press = |app: &mut App, word| {
        app.execute_native_harness(HarnessRequest::Autopilot { session: 0, word });
    };
    let events = |app: &App| {
        app.pool
            .get(0)
            .unwrap()
            .ctx
            .timeline
            .lock()
            .unwrap()
            .since(None)
            .filter(|e| e.kind == "meta-change")
            .count()
    };
    press(&mut app, Autopilot::Cruise);
    assert_eq!(app.session_autopilot(0).0, None, "already the default");
    assert_eq!(events(&app), 0, "nothing written");
    press(&mut app, Autopilot::Land);
    assert_eq!(app.session_autopilot(0).0, Some(Autopilot::Land));
    press(&mut app, Autopilot::Cruise);
    assert_eq!(
        app.session_autopilot(0),
        (None, AutopilotView::Cruise, false),
        "back to the default, its own word cleared"
    );
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nautopilot = \"off\"\n"),
    );
    assert_eq!(app.session_autopilot(0).1, AutopilotView::Off);
    press(&mut app, Autopilot::Cruise);
    assert_eq!(app.session_autopilot(0).0, Some(Autopilot::Cruise));
    host.shutdown_and_join();
}

#[test]
fn an_off_press_pins_off_under_a_default_of_off() {
    use crate::native_app::HarnessRequest;
    let mut app = App::headless_for_test();
    let host = idle_host();
    app.harness = Some(host.clone());
    let policy = |text: &str| {
        crate::harness_policy_cell::HarnessPolicyCell::new(crate::app_config::HarnessPolicy::read(
            text,
        ))
    };
    let press = |app: &mut App, word| {
        app.execute_native_harness(HarnessRequest::Autopilot { session: 0, word });
    };
    app.harness_policy = policy("[harness]\nautopilot = \"off\"\n");
    assert_eq!(app.harness_posture(), HarnessPosture::On(Autopilot::Off));
    press(&mut app, Autopilot::Off);
    assert_eq!(
        app.session_autopilot(0).0,
        Some(Autopilot::Off),
        "no word: Off pinned, not left to the default"
    );
    assert!(app.set_session_autopilot(
        0,
        Some(Autopilot::Land),
        crate::autopilot::Surface::Settings
    ));
    press(&mut app, Autopilot::Off);
    assert_eq!(
        app.session_autopilot(0),
        (Some(Autopilot::Off), AutopilotView::Off, false),
        "its word Land: Off pinned, not cleared"
    );
    app.harness_policy = policy("");
    assert_eq!(app.harness_posture(), HarnessPosture::On(Autopilot::Cruise));
    assert_eq!(app.session_autopilot(0).1, AutopilotView::Off);
    press(&mut app, Autopilot::Cruise);
    assert_eq!(
        app.session_autopilot(0),
        (None, AutopilotView::Cruise, false)
    );
    for default in Autopilot::ALL {
        assert!(!hands_back(Autopilot::Off, default), "{default:?}");
        for pressed in [Autopilot::Cruise, Autopilot::Land] {
            assert_eq!(
                hands_back(pressed, default),
                pressed == default,
                "{pressed:?} under {default:?}"
            );
        }
    }
    host.shutdown_and_join();
}

#[test]
fn a_failed_reread_beside_a_reading_is_said() {
    let mut today = HarnessStats::new(0);
    today.approved = 2;
    today.rows = 2;
    let mut state = HarnessState::unobserved();
    state.observed = true;
    state.stats.report = Some(std::sync::Arc::new(HarnessStatsReport::new(
        today.clone(),
        today,
        1_000_000_000,
        Some(0),
        &|_| Some(0),
    )));
    let failed = |state: &HarnessState| {
        state
            .projection()
            .did_rows
            .into_iter()
            .find(|(label, _)| label == "Last check")
    };
    assert_eq!(failed(&state), None);
    state.stats.unavailable = Some("the ledgers' reader failed".to_string());
    let rows = state.projection().did_rows;
    assert_eq!(
        rows.iter()
            .find(|(label, _)| label == "Approved")
            .map(|(_, words)| words.as_str()),
        Some("2 today \u{b7} 2 in 7 days"),
        "the older reading stands"
    );
    assert_eq!(
        failed(&state),
        Some((
            "Last check".to_string(),
            "failed: the ledgers' reader failed".to_string()
        ))
    );
}

#[test]
fn what_the_harness_did_draws_every_family_the_fold_keeps() {
    let mut today = HarnessStats::new(0);
    today.approved = 40;
    today.unproven = 40;
    today.land_prompts = 2;
    today.declined = 1;
    today.limit_wait_ms = 125_000;
    today.rows = 43;
    let mut week = HarnessStats::new(0);
    week.approved = 212;
    week.unproven = 45;
    week.land_prompts = 9;
    week.declined = 4;
    week.limit_wait_ms = 7_500_000;
    week.rows = 230;
    let mut state = HarnessState::unobserved();
    state.observed = true;
    let report = |today: &HarnessStats, week: &HarnessStats| {
        Some(std::sync::Arc::new(HarnessStatsReport::new(
            today.clone(),
            week.clone(),
            1_000_000_000,
            Some(0),
            &|_| Some(0),
        )))
    };
    state.stats.report = report(&today, &week);
    let rows = state.projection().did_rows;
    let words = |label: &str| {
        rows.iter()
            .find(|(l, _)| l == label)
            .map(|(_, w)| w.clone())
            .unwrap_or_else(|| panic!("no {label} row: {rows:?}"))
    };
    assert_eq!(words(APPROVED.label), "40 today \u{b7} 212 in 7 days");
    assert_eq!(words(UNPROVEN_OF_APPROVED), "40 today \u{b7} 45 in 7 days");
    assert_eq!(words(LAND_PROMPTS.label), "2 today \u{b7} 9 in 7 days");
    assert_eq!(words(DECLINED.label), "1 today \u{b7} 4 in 7 days");
    assert_eq!(words("Waited at limits"), "2m today \u{b7} 2h 5m in 7 days");
    let said = |words: &str| {
        rows.iter()
            .map(|(l, w)| format!("{l} {w}").to_lowercase().matches(words).count())
            .sum::<usize>()
    };
    assert_eq!(said(UNPROVEN_OF_APPROVED), 1, "said once: {rows:?}");
    assert_eq!(said("unproven"), 0, "never the ledger's word: {rows:?}");
    let labels: Vec<&str> = rows.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(
        labels[..6],
        [
            APPROVED.label,
            UNPROVEN_OF_APPROVED,
            DECLINED.label,
            LAND_PROMPTS.label,
            NONE_IN_7_DAYS,
            "Waited at limits",
        ]
    );
    today.unproven = 0;
    week.unproven = 0;
    state.stats.report = report(&today, &week);
    let rows = state.projection().did_rows;
    assert!(
        rows.iter()
            .all(|(l, w)| l != UNPROVEN_OF_APPROVED && !w.contains("no safe rule")),
        "a week with none says none: {rows:?}"
    );
    week.unproven = 3;
    state.stats.report = report(&today, &week);
    let rows = state.projection().did_rows;
    assert_eq!(
        rows.iter()
            .find(|(l, _)| l == UNPROVEN_OF_APPROVED)
            .map(|(_, w)| w.as_str()),
        Some("0 today \u{b7} 3 in 7 days"),
    );
    today.needs_told = 1;
    week.needs_told = 2;
    week.escalated = 1;
    state.stats.report = report(&today, &week);
    let rows = state.projection().did_rows;
    let at = |label: &str| rows.iter().position(|(l, _)| l == label);
    assert_eq!(
        at(NEEDS_TOLD.label),
        at(NEEDED_YOU.label).map(|i| i + 1),
        "{rows:?}"
    );
    assert_eq!(
        rows[at(NEEDS_TOLD.label).unwrap()].1,
        "1 today \u{b7} 2 in 7 days"
    );
}

#[test]
fn what_the_harness_did_counts_the_occupancy_facts() {
    let mut today = HarnessStats::new(0);
    today.rows = 3;
    today.facts_would.insert("hold".to_string(), 2);
    let mut week = HarnessStats::new(0);
    week.rows = 9;
    week.facts_would.insert("hold".to_string(), 5);
    week.facts_told.insert("hold".to_string(), 1);
    let mut state = HarnessState::unobserved();
    state.observed = true;
    let report = |today: &HarnessStats, week: &HarnessStats, idle: Option<u64>| {
        let mut report =
            HarnessStatsReport::new(today.clone(), week.clone(), 1_000_000_000, Some(0), &|_| {
                Some(0)
            });
        report.idle_ready_min = idle;
        Some(std::sync::Arc::new(report))
    };
    let numbers = |words: &str| -> Vec<u64> {
        words
            .split_whitespace()
            .filter_map(|w| w.parse().ok())
            .collect()
    };
    state.stats.report = report(&today, &week, Some(12));
    let rows = state.projection().did_rows;
    let words = |label: &str| {
        rows.iter()
            .find(|(l, _)| l == label)
            .map(|(_, w)| w.clone())
            .unwrap_or_else(|| panic!("no {label} row: {rows:?}"))
    };
    assert_eq!(
        numbers(&words("Facts told (hold)"))[..2],
        [0, 1],
        "{rows:?}"
    );
    assert_eq!(
        numbers(&words("Facts it would tell (hold)"))[..2],
        [2, 5],
        "{rows:?}"
    );
    assert_eq!(numbers(&words("Idle while ready")), [12], "{rows:?}");

    week.facts_would.clear();
    week.facts_told.clear();
    today.facts_would.clear();
    state.stats.report = report(&today, &week, None);
    let rows = state.projection().did_rows;
    assert!(
        rows.iter()
            .all(|(l, _)| !l.starts_with("Facts") && l != "Idle while ready"),
        "{rows:?}"
    );
}

#[test]
fn one_name_for_each_count_from_the_chip_to_the_page() {
    use aterm_agent::harness::acts::Act;
    use aterm_agent::harness::stats::{DecisionKind, Tally};
    let press = decision(
        "s-7",
        0,
        DecisionKind::Unproven,
        ("git push", "", Some("Approve permission boxes: All")),
    );
    assert!(
        decision_sentence(&press, "fix the build")
            .ends_with(&format!("; {NO_SAFE_RULE_PROVED} it")),
        "DECIDED FOR YOU says it in the page's words"
    );
    for words in FAMILY_WORDS.iter().chain([&NEEDED_YOU, &NEEDS_TOLD]) {
        let mut first = words.many.chars();
        let label: String = first
            .next()
            .map(|c| c.to_uppercase().chain(first).collect())
            .unwrap_or_default();
        assert_eq!(words.label, label, "{words:?}");
    }
    for (words, act) in [
        (APPROVED, Act::Approved),
        (KEPT_GOING, Act::Continued),
        (ANSWERED, Act::Answered),
        (RESUMED, Act::Resumed),
        (DECLINED, Act::Declined),
    ] {
        assert_eq!((words.one, words.many), (act.word(), act.word()));
    }
    let mut today = HarnessStats::per_session(0);
    today.continued = 3;
    today.escalated = 1;
    today.human_wait_ms = 30_000;
    today.by_session.as_mut().expect("per session").insert(
        "s-1".to_string(),
        Tally {
            continued: 3,
            escalated: 1,
            human_wait_ms: 30_000,
            ..Tally::default()
        },
    );
    today.rows = 4;
    let mut week = HarnessStats::new(0);
    week.continued = 3;
    week.escalated = 1;
    week.rows = 4;
    let mut state = HarnessState::unobserved();
    state.observed = true;
    state.stats.report = Some(std::sync::Arc::new(HarnessStatsReport::new(
        today,
        week,
        1_000_000_000,
        Some(0),
        &|_| Some(0),
    )));
    let projection = state.projection();
    let (lead, _) = projection.today.expect("today's lines");
    assert!(lead.starts_with("3 kept going today"), "{lead:?}");
    let did = |label: &str| projection.did_rows.iter().any(|(l, _)| l == label);
    assert!(
        did(KEPT_GOING.label) && did(NEEDED_YOU.label),
        "{:?}",
        projection.did_rows
    );
    assert!(
        !did("Continued") && !did("Escalated"),
        "{:?}",
        projection.did_rows
    );
    let tally = Tally {
        continued: 3,
        escalated: 1,
        ..Tally::default()
    };
    let day = day_line(&tally, Some(0)).expect("a day");
    assert!(
        day.contains(&format!("{} once", NEEDED_YOU.one)) && day.contains("3 kept going"),
        "{day:?}"
    );
}

#[test]
fn a_session_with_a_blank_title_is_named_by_its_program() {
    assert_eq!(session_title(Some("deploy"), "Claude Code"), "deploy");
    assert_eq!(session_title(Some("  "), "Claude Code"), "Claude Code");
    assert_eq!(session_title(None, "Codex"), "Codex");
}

#[test]
fn a_title_that_is_the_agents_name_names_its_program() {
    for title in ["Claude Code", "\u{2733} Claude Code", "claude code "] {
        assert!(names_program(title, "Claude Code"), "{title:?}");
    }
    assert!(names_program("\u{280b} Codex", "Codex"));
    for title in [
        "Fix login bug",
        "claude-docs",
        "\u{2733} Claude Code review",
    ] {
        assert!(!names_program(title, "Claude Code"), "{title:?}");
    }
}

#[test]
fn a_sessions_row_names_its_agent_once() {
    let detail = |title: &str| {
        let mut state = HarnessState::unobserved();
        state.observed = true;
        state.posture = HarnessPosture::On(Autopilot::Cruise);
        state.sessions = vec![HarnessSession {
            local_id: 0,
            sid: "s-0".to_string(),
            title: title.to_string(),
            place: "in tab 1".to_string(),
            program: "Claude Code",
            word: None,
            view: AutopilotView::Cruise,
            glass: crate::claude_lights::ChipGlass::default(),
            armed_at_ms: None,
            set_by: None,
            default: Some(Autopilot::Cruise),
            supervised: true,
            need: None,
            hold: None,
        }];
        state.projection().sessions[0].detail.clone()
    };
    for title in ["Claude Code", "\u{2733} Claude Code"] {
        assert_eq!(
            detail(title),
            "Tab 1 \u{b7} supervised \u{b7} autopilot cruise",
            "{title:?}"
        );
    }
    assert_eq!(
        detail("Fix login bug"),
        "Tab 1 \u{b7} Claude Code \u{b7} supervised \u{b7} autopilot cruise"
    );
}

#[test]
fn a_sessions_row_is_named_as_its_tab_is() {
    let app = App::headless_for_test();
    run_claude(&app, 0);
    let title = |app: &App| app.harness_state().sessions[0].title.clone();
    assert_eq!(title(&app), "Claude Code", "untitled: its agent's name");
    {
        let session = app.pool.get(0).unwrap();
        let mut term = session.term.lock().unwrap();
        term.process(b"\x1b]2;Fix login bug\x07");
    }
    assert_eq!(title(&app), "Fix login bug");
    {
        let session = app.pool.get(0).unwrap();
        session
            .ctx
            .meta
            .lock()
            .unwrap()
            .set("title", Some("deploy".to_string()));
    }
    assert_eq!(title(&app), "deploy");
}

#[test]
fn a_sessions_row_drops_its_agents_spinner_frame() {
    let app = App::headless_for_test();
    run_claude(&app, 0);
    let title = |app: &App| app.harness_state().sessions[0].title.clone();
    let osc = |app: &App, text: &str| {
        let session = app.pool.get(0).unwrap();
        let mut term = session.term.lock().unwrap();
        term.process(format!("\x1b]2;{text}\x07").as_bytes());
    };
    for (osc_title, row) in [
        (
            "\u{2826} Add tested calculator functions | proj-codex",
            "Add tested calculator functions | proj-codex",
        ),
        (
            "\u{280b} Add tested calculator functions | proj-codex",
            "Add tested calculator functions | proj-codex",
        ),
        (
            "\u{25D1} Calc.py small tested module",
            "Calc.py small tested module",
        ),
        (
            "\u{2733} Calc.py small tested module",
            "\u{2733} Calc.py small tested module",
        ),
    ] {
        osc(&app, osc_title);
        assert_eq!(title(&app), row, "{osc_title:?}");
    }
    app.pool
        .get(0)
        .unwrap()
        .ctx
        .meta
        .lock()
        .unwrap()
        .set("title", Some("\u{2826} deploy".to_string()));
    assert_eq!(title(&app), "\u{2826} deploy", "the person's name, whole");
}

#[test]
fn the_sessions_state_line_is_formatted_from_the_glass_for_every_state() {
    use crate::claude_lights::{ChipGlass, Clock, LandAtGlass, Stamp, Weekday};
    use aterm_agent::harness::acts::{Act, TaskFace};
    const TODAY: i64 = 20_007;
    let dated = |view: AutopilotView, glass: ChipGlass, today: i64| HarnessSessionRow {
        local_id: 7,
        title: "fix the build".to_string(),
        name: "fix the build".to_string(),
        day_line: None,
        detail: String::new(),
        mode: view.mode(),
        default: view.mode(),
        view,
        glass,
        armed_at_ms: None,
        today,
        setting: "follows the default".to_string(),
        need: None,
        hold: None,
    };
    let row = |view: AutopilotView, glass: ChipGlass| dated(view, glass, TODAY);
    let clock = |h: u16, m: u16| Clock::of_minute(h * 60 + m);
    let stamp = |h: u16, m: u16| Stamp {
        clock: clock(h, m),
        day: TODAY,
    };
    let timer = |day| ChipGlass {
        land_at: Some(LandAtGlass {
            clock: clock(18, 0),
            other_day: day,
        }),
        ..ChipGlass::default()
    };
    let told = |face| ChipGlass {
        task: Some(face),
        task_at: Some(stamp(17, 42)),
        ..ChipGlass::default()
    };
    let last = Some((Act::Approved, stamp(14, 5)));
    let cases = [
        (
            AutopilotView::Cruise,
            timer(None),
            "lands at 18:00".to_string(),
        ),
        (
            AutopilotView::Cruise,
            timer(Some(Weekday::Sat)),
            "lands Sat 18:00".to_string(),
        ),
        (
            AutopilotView::Land,
            told(TaskFace::Landed),
            "landed 17:42".to_string(),
        ),
        (
            AutopilotView::Cruise,
            ChipGlass {
                land_at: timer(Some(Weekday::Sat)).land_at,
                ..told(TaskFace::Landed)
            },
            "lands Sat 18:00".to_string(),
        ),
        (
            AutopilotView::Cruise,
            ChipGlass {
                last,
                ..told(TaskFace::Landed)
            },
            "last: \u{2713} approved 14:05".to_string(),
        ),
        (
            AutopilotView::Cruise,
            ChipGlass {
                land_at: timer(None).land_at,
                ..told(TaskFace::Theirs)
            },
            "lands at 18:00".to_string(),
        ),
        (
            AutopilotView::Land,
            told(TaskFace::Theirs),
            "quiet: a person's own exchange".to_string(),
        ),
        (
            AutopilotView::Cruise,
            ChipGlass {
                last,
                ..ChipGlass::default()
            },
            "last: \u{2713} approved 14:05".to_string(),
        ),
        (
            AutopilotView::Land,
            ChipGlass {
                last,
                ..told(TaskFace::Open)
            },
            "last: \u{2713} approved 14:05".to_string(),
        ),
        (
            AutopilotView::Off,
            ChipGlass {
                last,
                ..told(TaskFace::Landed)
            },
            "last: \u{2713} approved 14:05".to_string(),
        ),
    ];
    for (view, glass, line) in cases {
        assert_eq!(
            row(view, glass).state_line(),
            Some(line),
            "{view:?} {glass:?}"
        );
    }
    for (view, glass, line) in [
        (
            AutopilotView::Land,
            told(TaskFace::Landed),
            "landed Fri 17:42",
        ),
        (
            AutopilotView::Cruise,
            ChipGlass {
                last,
                ..ChipGlass::default()
            },
            "last: \u{2713} approved Fri 14:05",
        ),
    ] {
        assert_eq!(
            dated(view, glass, TODAY + 1).state_line().as_deref(),
            Some(line),
            "{view:?} {glass:?}"
        );
        assert!(
            !row(view, glass).state_line().unwrap().contains("Fri"),
            "{view:?}: its own day names none"
        );
    }
    for (view, glass, line) in [
        (
            AutopilotView::Land,
            told(TaskFace::Landed),
            "landed 11 Oct 17:42",
        ),
        (
            AutopilotView::Cruise,
            ChipGlass {
                last,
                ..ChipGlass::default()
            },
            "last: \u{2713} approved 11 Oct 14:05",
        ),
    ] {
        assert_eq!(
            dated(view, glass, TODAY + 7).state_line().as_deref(),
            Some(line),
            "{view:?} {glass:?}"
        );
    }
    for view in AutopilotView::ALL {
        assert_eq!(
            row(view, ChipGlass::default()).state_line(),
            None,
            "{view:?}"
        );
    }
    assert_eq!(row(AutopilotView::Land, timer(None)).state_line(), None);
    let unplaced = ChipGlass {
        task: Some(TaskFace::Landed),
        ..ChipGlass::default()
    };
    assert_eq!(
        row(AutopilotView::Land, unplaced).state_line().as_deref(),
        Some("landed")
    );
}

#[test]
fn the_state_line_says_the_pause_where_no_face_or_timer_has_the_line() {
    use crate::claude_lights::{ChipGlass, Clock, LandAtGlass, Stamp};
    use crate::harness_glass::PauseGlass;
    use aterm_agent::harness::acts::{Act, TaskFace};
    const TODAY: i64 = 20_007;
    let row = |view: AutopilotView, glass: ChipGlass| HarnessSessionRow {
        local_id: 7,
        title: "fix the build".to_string(),
        name: "fix the build".to_string(),
        day_line: None,
        detail: String::new(),
        mode: view.mode(),
        default: view.mode(),
        view,
        glass,
        armed_at_ms: None,
        today: TODAY,
        setting: "follows the default".to_string(),
        need: None,
        hold: None,
    };
    let stamp = |h: u16, m: u16| Stamp {
        clock: Clock::of_minute(h * 60 + m),
        day: TODAY,
    };
    let pause = Some(PauseGlass {
        until: stamp(18, 41),
        short: 3,
    });
    let last = Some((Act::Approved, stamp(18, 35)));
    let pausing = ChipGlass {
        pause,
        last,
        ..ChipGlass::default()
    };
    for view in [AutopilotView::Cruise, AutopilotView::Land] {
        assert_eq!(
            row(view, pausing).state_line().as_deref(),
            Some("pausing until 18:41 (3 short turns in a row)"),
            "{view:?}"
        );
    }
    let timed = ChipGlass {
        land_at: Some(LandAtGlass {
            clock: Clock::of_minute(19 * 60),
            other_day: None,
        }),
        ..pausing
    };
    assert_eq!(
        row(AutopilotView::Cruise, timed).state_line().as_deref(),
        Some("lands at 19:00"),
        "the timer's line wins"
    );
    let landed = ChipGlass {
        task: Some(TaskFace::Landed),
        task_at: Some(stamp(17, 42)),
        ..pausing
    };
    assert_eq!(
        row(AutopilotView::Land, landed).state_line().as_deref(),
        Some("landed 17:42"),
        "the landing wins"
    );
    assert_eq!(
        row(
            AutopilotView::Cruise,
            ChipGlass {
                pause: None,
                ..pausing
            }
        )
        .state_line()
        .as_deref(),
        Some("last: \u{2713} approved 18:35")
    );
}

struct Page {
    app: App,
    host: crate::harness_host::HostHandle,
    t0: std::time::Instant,
}

impl Page {
    fn open() -> Self {
        let mut app = App::headless_for_test();
        run_claude(&app, 0);
        let host = idle_host();
        app.harness = Some(host.clone());
        assert!(app.open_harness_settings(WindowId(0), crate::harness_screen::HarnessFocus::Top));
        let t0 = std::time::Instant::now();
        app.harness_screen.began_reading(t0);
        Self { app, host, t0 }
    }

    fn at(&self, ms: u64) -> std::time::Instant {
        self.t0 + std::time::Duration::from_millis(ms)
    }

    fn act(&mut self, act: aterm_agent::harness::acts::Act, ms: u64) {
        let at = crate::harness_glass::Arrival {
            now: self.at(ms),
            unix: 1_792_159_500 + i64::try_from(ms / 1000).expect("small"),
            offset_s: Some(0),
        };
        crate::harness_glass::on_act_at(&mut self.app, 0, act, at);
    }

    fn land(&mut self) {
        let report = Box::new(HarnessStatsReport::new(
            HarnessStats::new(0),
            HarnessStats::new(0),
            0,
            None,
            &|_| None,
        ));
        let sequence = self.app.harness_screen.sequence;
        self.app.finish_harness_stats(sequence, Ok(report));
    }

    fn newer(&self) -> bool {
        self.app
            .harness_state()
            .projection()
            .did_rows
            .iter()
            .any(|(label, _)| label == "Newer")
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        self.host.shutdown_and_join();
    }
}

#[test]
fn an_act_marks_the_page_stale_and_owes_one_reread_per_ten_seconds() {
    use crate::harness_glass::{DWELL, REREAD};
    use aterm_agent::harness::acts::Act;
    {
        let mut app = App::headless_for_test();
        run_claude(&app, 0);
        let host = idle_host();
        app.harness = Some(host.clone());
        let t0 = std::time::Instant::now();
        crate::harness_glass::on_act_at(
            &mut app,
            0,
            Act::Approved,
            crate::harness_glass::Arrival {
                now: t0,
                unix: 1_792_159_500,
                offset_s: Some(0),
            },
        );
        assert!(!app.harness_screen.stats.stale);
        assert_eq!(app.harness_screen.reread_at(), None);
        app.expire_harness_acts_at(t0 + DWELL);
        assert_eq!(app.harness_acts_deadline(), None);
        host.shutdown_and_join();
    }

    let mut page = Page::open();
    let revision = page.app.harness_screen.revision;
    page.act(Act::Continued, 20_000);
    assert!(page.app.harness_screen.stats.stale);
    assert!(page.app.harness_screen.revision > revision, "republished");
    assert_eq!(
        page.app.harness_state().sessions[0]
            .glass
            .last
            .map(|(act, _)| act),
        Some(Act::Continued),
        "the row's glass carries the act"
    );
    let owed = page.at(20_000) + DWELL;
    assert_eq!(page.app.harness_screen.reread_at(), Some(owed));
    assert_eq!(page.app.harness_acts_deadline(), Some(owed));
    page.land();
    assert!(page.app.harness_screen.stats.stale, "older than the act");
    assert!(page.newer(), "the page says so");
    assert_eq!(page.app.harness_screen.reread_at(), Some(owed));
    page.app.expire_harness_acts_at(owed);
    assert_eq!(page.app.harness_screen.reread_at(), None, "taken");
    page.app.harness_screen.began_reading(owed);
    page.act(Act::Approved, 24_000);
    page.act(Act::Answered, 25_000);
    assert_eq!(page.app.harness_screen.reread_at(), Some(owed + REREAD));
    assert_eq!(page.app.harness_acts_deadline(), Some(owed + REREAD));
    page.land();
    assert!(page.app.harness_screen.stats.stale);
    assert_eq!(page.app.harness_screen.reread_at(), Some(owed + REREAD));
    page.app.expire_harness_acts_at(owed + REREAD);
    page.app.harness_screen.began_reading(owed + REREAD);
    page.land();
    assert!(!page.app.harness_screen.stats.stale, "covered: fresh");
    assert!(!page.newer());
    assert_eq!(page.app.harness_acts_deadline(), None);
}

#[test]
fn every_act_reaches_the_page_and_its_reread_waits_a_dwell_past_the_newest() {
    use crate::harness_glass::{DWELL, REREAD};
    use aterm_agent::harness::acts::Act;
    let mut page = Page::open();
    page.act(Act::Approved, 20_000);
    assert_eq!(
        page.app.harness_screen.reread_at(),
        Some(page.at(20_000) + DWELL)
    );
    page.act(Act::Approved, 21_000);
    let glass = &page.app.harness_glass[&0];
    assert_eq!(glass.trail_gen(), 1, "one entry on the chip's trail");
    assert_eq!(
        page.app.harness_screen.reread_at(),
        Some(page.at(21_000) + DWELL),
        "the repeat reached the page"
    );
    page.app.harness_screen.began_reading(page.at(23_000));
    page.land();
    assert!(page.app.harness_screen.stats.stale);
    assert_eq!(page.app.harness_screen.reread_at(), Some(page.at(33_000)));
    page.app.expire_harness_acts_at(page.at(33_000));
    assert_eq!(page.app.harness_screen.reread_at(), None, "taken");
    page.app.harness_screen.began_reading(page.at(33_000));
    page.land();
    assert!(!page.app.harness_screen.stats.stale, "both read");

    for ms in [50_000, 51_800, 53_600] {
        page.act(Act::Answered, ms);
    }
    assert_eq!(
        page.app.harness_screen.reread_at(),
        Some(page.at(53_600) + DWELL),
        "a dwell past the dialog's last key"
    );
    page.app.expire_harness_acts_at(page.at(56_600));
    page.app.harness_screen.began_reading(page.at(56_600));
    page.land();
    assert!(
        !page.app.harness_screen.stats.stale,
        "the dialog read whole"
    );

    for i in 0..10 {
        page.act(Act::Continued, 100_000 + 2_000 * i);
    }
    assert_eq!(
        page.app.harness_screen.reread_at(),
        Some(page.at(100_000) + REREAD)
    );
}

#[test]
fn a_reread_due_while_a_reading_runs_is_owed_again() {
    use crate::harness_glass::{DWELL, REREAD};
    use aterm_agent::harness::acts::Act;
    let mut page = Page::open();
    page.land();
    page.act(Act::Approved, 7_000);
    assert_eq!(page.app.harness_screen.reread_at(), Some(page.at(10_000)));
    let refresh = page.at(9_800);
    page.app.harness_screen.began_reading(refresh);
    page.app.expire_harness_acts_at(page.at(10_000));
    assert_eq!(page.app.harness_screen.reread_at(), None, "taken");
    page.land();
    assert!(page.app.harness_screen.stats.stale);
    assert!(page.newer());
    assert_eq!(
        page.app.harness_screen.reread_at(),
        Some(refresh + REREAD),
        "owed again: the page reads again, as it says"
    );
    assert_eq!(page.app.harness_acts_deadline(), Some(refresh + REREAD));
    page.app.expire_harness_acts_at(refresh + REREAD);
    page.app.harness_screen.began_reading(refresh + REREAD);
    page.land();
    assert!(!page.newer());
    assert_eq!(page.app.harness_acts_deadline(), None);
    page.act(Act::Approved, 60_000);
    page.app
        .harness_screen
        .began_reading(page.at(60_000) + DWELL);
    page.land();
    assert!(!page.app.harness_screen.stats.stale);
}

#[test]
fn a_closed_page_keeps_no_reread_armed() {
    use crate::harness_glass::DWELL;
    use aterm_agent::harness::acts::Act;
    let mut page = Page::open();
    page.act(Act::Approved, 1_000);
    page.app.expire_harness_acts_at(page.at(1_000) + DWELL);
    assert_eq!(
        page.app.harness_acts_deadline(),
        Some(page.at(10_000)),
        "CONTROL"
    );
    assert!(!page.app.close_tab_at(WindowId(0), 1), "the Settings tab");
    assert!(!page.app.harness_page_shown());
    assert_eq!(page.app.harness_acts_deadline(), None, "nothing armed");
    page.app.expire_harness_acts();
    assert_eq!(page.app.harness_screen.reread_at(), None);
    assert!(!page.app.harness_screen.stats.stale);
}

#[test]
fn a_page_held_open_across_the_minute_names_the_day_from_then_on() {
    use crate::autopilot_timers::{TimerChoice, glass_of, local_offset_at, set_fake_clocks};
    use crate::claude_lights::Weekday;
    let mut app = App::headless_for_test();
    let fri = 1_792_177_200_i64;
    let off = local_offset_at(fri).expect("the zone reads");
    let day = (fri + off).div_euclid(86_400);
    let local = |hh: i64, mm: i64| (day * 86_400 + hh * 3_600 + mm * 60 - off) * 1000;
    let opened = local(18, 30);
    assert_eq!(
        local_offset_at(local(23, 59) / 1000),
        Some(off),
        "no clock change between"
    );
    let t0 = std::time::Instant::now();
    let mono = |wall: i64| {
        t0 + std::time::Duration::from_millis(u64::try_from(wall - opened).expect("ahead"))
    };
    let clocks = |wall: i64| set_fake_clocks(Some((mono(wall), wall)));
    clocks(opened);
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nland_at = \"18:36\"\n"),
    );
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
        settings.harness_snapshot().land_at_offer.expect("offered")
    };
    assert!(app.open_harness_settings(WindowId(0), crate::harness_screen::HarnessFocus::Top));
    assert!(app.harness_page_shown());
    assert_eq!(published(&app).other_day, None, "18:36 is ahead today");
    assert_eq!(app.autopilot_offer_deadline(), Some(mono(local(18, 36))));

    clocks(local(18, 36));
    let revision = app.harness_screen.revision;
    app.expire_autopilot_offer();
    assert!(app.harness_screen.revision > revision, "published again");
    let offer = published(&app);
    assert_eq!(offer.other_day, Some(Weekday::of_days(day + 1)));
    let armed = app
        .resolve_timer_choice(TimerChoice::At)
        .expect("a press arms");
    assert_eq!(
        glass_of(armed, local(18, 36), &local_offset_at),
        Some(offer),
        "the button says what a press arms"
    );

    assert!(!app.close_tab_at(WindowId(0), 1), "the Settings tab");
    assert!(!app.harness_page_shown());
    assert_eq!(
        app.autopilot_offer_deadline(),
        None,
        "no wake for a closed page"
    );
    app.expire_autopilot_offer();
    assert_eq!(app.autopilot_timers.offer_due(), None, "the note dropped");
    set_fake_clocks(None);
}

#[test]
fn a_page_held_open_past_midnight_reads_the_new_day() {
    use crate::autopilot_timers::{local_offset_at, set_fake_clocks};
    use crate::harness_screen::HarnessFocus;
    let fri = 1_792_177_200_i64;
    let off = local_offset_at(fri).expect("the zone reads");
    let day = (fri + off).div_euclid(86_400);
    let local = |day: i64, hh: i64, mm: i64| (day * 86_400 + hh * 3_600 + mm * 60 - off) * 1000;
    let opened = local(day, 23, 50);
    assert_eq!(
        local_offset_at(local(day + 2, 0, 0) / 1000),
        Some(off),
        "no clock change between"
    );
    let t0 = std::time::Instant::now();
    let mono = |wall: i64| {
        t0 + std::time::Duration::from_millis(u64::try_from(wall - opened).expect("ahead"))
    };
    let clocks = |wall: i64| set_fake_clocks(Some((mono(wall), wall)));
    let until = |app: &App| {
        let instance = app
            .native_runtime
            .instance_by_kind(crate::native_app::AppKind::Settings)
            .expect("the Settings controller");
        let Some(crate::native_app::NativeApp::Settings(settings)) =
            app.native_runtime.app(instance)
        else {
            unreachable!("a Settings instance")
        };
        settings.harness_snapshot().ab.until
    };
    let stamp = |wall: i64| crate::harness_glass::Stamp::at(wall.div_euclid(1000));
    let read_friday = |app: &mut App| {
        app.harness_screen.stats.collecting = false;
        app.harness_screen.stats.report = Some(std::sync::Arc::new(HarnessStatsReport::new(
            HarnessStats::per_session(local(day, 0, 0)),
            HarnessStats::new(local(day - 6, 0, 0)),
            opened,
            Some(off),
            &|_| Some(off),
        )));
    };
    for land_at in [None, Some("23:55")] {
        let mut app = App::headless_for_test();
        clocks(opened);
        if let Some(at) = land_at {
            app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
                crate::app_config::HarnessPolicy::read(&format!("[harness]\nland_at = \"{at}\"\n")),
            );
        }
        assert!(app.open_harness_settings(WindowId(0), HarnessFocus::Top));
        assert!(app.harness_page_shown());
        read_friday(&mut app);
        app.publish_native_harness_state();
        assert_eq!(until(&app), stamp(opened), "{land_at:?}: 23:50");
        let asked = app.harness_screen.readings_asked;
        if land_at.is_some() {
            assert_eq!(
                app.autopilot_offer_deadline(),
                Some(mono(local(day, 23, 55))),
                "{land_at:?}: the offer turns first"
            );
            clocks(local(day, 23, 55));
            let revision = app.harness_screen.revision;
            app.expire_autopilot_offer();
            assert!(app.harness_screen.revision > revision, "published again");
            assert_eq!(
                app.harness_screen.readings_asked, asked,
                "CONTROL: a turn over today's reading reads nothing"
            );
            assert_eq!(until(&app), stamp(local(day, 23, 55)), "the turn's");
        }
        assert_eq!(
            app.autopilot_offer_deadline(),
            Some(mono(local(day + 1, 0, 0))),
            "{land_at:?}: one wake, at midnight"
        );
        clocks(local(day + 1, 0, 0) - 1);
        app.expire_autopilot_offer();
        assert_eq!(
            app.harness_screen.readings_asked, asked,
            "{land_at:?}: short of it"
        );
        clocks(local(day + 1, 0, 0));
        app.expire_autopilot_offer();
        assert_eq!(
            app.harness_screen.readings_asked,
            asked + 1,
            "{land_at:?}: past midnight the page reads the new day"
        );
        let midnight = stamp(local(day + 1, 0, 0)).expect("the zone reads");
        assert_eq!(until(&app), Some(midnight), "{land_at:?}: the new day's");
        assert_eq!(
            (midnight.day, midnight.words(Some(day + 1))),
            (day + 1, "00:00".to_string())
        );
        if land_at.is_none() {
            assert_eq!(
                app.autopilot_offer_deadline(),
                Some(mono(local(day + 2, 0, 0))),
                "then the next midnight"
            );
        }
        assert!(!app.close_tab_at(WindowId(0), 1), "the Settings tab");
        assert_eq!(app.autopilot_offer_deadline(), None, "{land_at:?}");
        set_fake_clocks(None);
    }
}

#[test]
fn a_scroll_or_an_arrow_lets_the_landed_row_go() {
    use crate::app_introspect::AuxTarget;
    use crate::harness_screen::HarnessFocus;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    run_claude(&app, 0);
    let host = idle_host();
    app.harness = Some(host.clone());
    let selected = |app: &App| {
        app.read_aux_controls(AuxTarget::Prefs).iter().any(|l| {
            l.starts_with("ui key=\"harness/session/0/show\" ") && l.contains("selected:true")
        })
    };
    let landed = |app: &App| {
        app.native_settings_route_view_target(crate::native_settings::SettingsRoute::Harness)
            .map(|(_, _, _, state)| state.harness_focus)
    };
    for event in [
        AppEvent::ScrollLines(1),
        AppEvent::ScrollLines(-1),
        AppEvent::ScrollPage(1),
        AppEvent::StepFocus(1),
        AppEvent::StepFocus(-1),
    ] {
        let name = format!("{event:?}");
        assert!(app.open_harness_settings(wid, HarnessFocus::Session(0)));
        assert!(selected(&app), "{name}: landed");
        app.publish_native_harness_state();
        assert!(selected(&app), "CONTROL {name}: a publish keeps it");
        assert_eq!(
            landed(&app),
            Some(HarnessFocus::Session(0)),
            "CONTROL {name}"
        );
        let (_, view) = app.active_native_view(wid).expect("the Settings view");
        app.dispatch_native_view_event(wid, view, event)
            .expect("dispatched");
        assert_eq!(landed(&app), Some(HarnessFocus::Top), "{name} lets it go");
        assert!(!selected(&app), "{name}: drawn so");
    }
    host.shutdown_and_join();
}

#[test]
fn a_page_held_open_across_midnight_names_the_faces_day_from_then_on() {
    use crate::autopilot_timers::{local_offset_at, set_fake_clocks};
    use aterm_agent::harness::acts::TaskFace;
    let mut app = App::headless_for_test();
    run_claude(&app, 0);
    let host = idle_host();
    app.harness = Some(host.clone());
    assert!(app.set_session_autopilot(
        0,
        Some(aterm_agent::supervise::Autopilot::Land),
        crate::autopilot::Surface::Chip
    ));
    let fri = 1_792_177_200_i64;
    let off = local_offset_at(fri).expect("the zone reads");
    let day = (fri + off).div_euclid(86_400);
    let local = |day: i64, hh: i64, mm: i64| (day * 86_400 + hh * 3_600 + mm * 60 - off) * 1000;
    let opened = local(day, 23, 58);
    assert_eq!(
        local_offset_at(local(day + 1, 23, 59) / 1000),
        Some(off),
        "no clock change between"
    );
    let t0 = std::time::Instant::now();
    let mono = |wall: i64| {
        t0 + std::time::Duration::from_millis(u64::try_from(wall - opened).expect("ahead"))
    };
    let clocks = |wall: i64| set_fake_clocks(Some((mono(wall), wall)));
    clocks(opened);
    assert!(crate::harness_glass::on_task(
        &mut app,
        0,
        Some(TaskFace::Landed),
        local(day, 23, 50)
    ));
    let line = |app: &App| {
        let instance = app
            .native_runtime
            .instance_by_kind(crate::native_app::AppKind::Settings)
            .expect("the Settings controller");
        let Some(crate::native_app::NativeApp::Settings(settings)) =
            app.native_runtime.app(instance)
        else {
            unreachable!("a Settings instance")
        };
        settings.harness_snapshot().projection().sessions[0].state_line()
    };
    assert!(app.open_harness_settings(WindowId(0), crate::harness_screen::HarnessFocus::Top));
    assert!(app.harness_page_shown());
    assert_eq!(line(&app).as_deref(), Some("landed 23:50"), "its own day");
    assert_eq!(
        app.autopilot_offer_deadline(),
        Some(mono(local(day + 1, 0, 0))),
        "one wake, at midnight"
    );

    clocks(local(day + 1, 0, 0) - 1);
    app.expire_autopilot_offer();
    assert_eq!(line(&app).as_deref(), Some("landed 23:50"), "short of it");

    clocks(local(day + 1, 0, 0));
    let revision = app.harness_screen.revision;
    app.expire_autopilot_offer();
    assert!(app.harness_screen.revision > revision, "published again");
    let fri = crate::claude_lights::Weekday::of_days(day).short();
    assert_eq!(
        line(&app),
        Some(format!("landed {fri} 23:50")),
        "yesterday's, by its weekday"
    );
    assert_eq!(
        app.autopilot_offer_deadline(),
        Some(mono(local(day + 2, 0, 0))),
        "then the next midnight"
    );

    assert!(!app.close_tab_at(WindowId(0), 1), "the Settings tab");
    assert!(!app.harness_page_shown());
    assert_eq!(
        app.autopilot_offer_deadline(),
        None,
        "no wake for a closed page"
    );
    app.expire_autopilot_offer();
    assert_eq!(app.autopilot_timers.offer_due(), None, "the note dropped");
    set_fake_clocks(None);
    host.shutdown_and_join();
}

#[test]
fn the_pages_land_at_offer_is_the_instant_a_press_would_arm() {
    use crate::autopilot_timers::{TimerChoice, glass_of, local_offset_at, set_fake_clocks};
    use crate::claude_lights::Weekday;
    let mut app = App::headless_for_test();
    let fri = 1_792_177_200_i64;
    let off = local_offset_at(fri).expect("the zone reads");
    let day = (fri + off).div_euclid(86_400);
    let now = day * 86_400 + 18 * 3_600 + 49 * 60 - off;
    assert_eq!(local_offset_at(now), Some(off), "no clock change between");
    set_fake_clocks(Some((std::time::Instant::now(), now * 1000)));
    for (land_at, other_day) in [("18:36", Some(Weekday::of_days(day + 1))), ("18:55", None)] {
        app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
            crate::app_config::HarnessPolicy::read(&format!(
                "[harness]\nland_at = \"{land_at}\"\n"
            )),
        );
        let offer = app.harness_state().land_at_offer.expect("offered");
        assert_eq!(
            (offer.clock.as_str(), offer.other_day),
            (land_at, other_day),
            "{land_at}"
        );
        let armed = app
            .resolve_timer_choice(TimerChoice::At)
            .expect("a press arms");
        assert_eq!(
            glass_of(armed, now * 1000, &local_offset_at),
            Some(offer),
            "{land_at}: the press arms what the button says"
        );
    }
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\n"),
    );
    assert_eq!(app.harness_state().land_at_offer, None);
    set_fake_clocks(None);
}

fn day_of(edit: impl FnOnce(&mut HarnessStats)) -> HarnessStats {
    let mut day = HarnessStats::per_session(0);
    edit(&mut day);
    day
}

fn state_with_today(today: Option<HarnessStats>, collecting: bool) -> HarnessState {
    let mut state = HarnessState::unobserved();
    state.observed = true;
    state.stats.collecting = collecting;
    state.stats.report = today.map(|today| {
        std::sync::Arc::new(HarnessStatsReport::new(
            today,
            HarnessStats::new(0),
            0,
            Some(0),
            &|_| Some(0),
        ))
    });
    state
}

#[test]
fn statuss_today_says_its_nonzero_families_in_their_order() {
    assert_eq!(HarnessState::unobserved().today_lines(), None);
    assert_eq!(state_with_today(None, false).today_lines(), None);
    assert_eq!(
        state_with_today(None, true).today_lines(),
        Some(("reading\u{2026}".to_string(), String::new()))
    );
    assert_eq!(
        state_with_today(Some(day_of(|_| {})), false).today_lines(),
        Some(("nothing yet today".to_string(), String::new()))
    );
    let full = day_of(|d| {
        d.approved = 37;
        d.continued = 12;
        d.answered = 4;
        d.resumed = 2;
        d.land_prompts = 1;
        d.declined = 0;
        d.upgraded = 3;
        d.human_wait_ms = 240_000;
    });
    assert_eq!(
        state_with_today(Some(full), false).today_lines(),
        Some((
            "37 approved today \u{b7} 12 kept going \u{b7} 4 answered".to_string(),
            "2 resumed \u{b7} 1 land prompt \u{b7} 3 upgraded \u{b7} waited for you 4m".to_string()
        ))
    );
    let short = day_of(|d| {
        d.approved = 1;
        d.answered = 2;
    });
    assert_eq!(
        state_with_today(Some(short), false).today_lines(),
        Some((
            "1 approved today \u{b7} 2 answered".to_string(),
            String::new()
        ))
    );
    let wait = day_of(|d| d.human_wait_ms = 60_000);
    assert_eq!(
        state_with_today(Some(wait), false).today_lines(),
        Some(("waited for you 1m today".to_string(), String::new()))
    );
}

#[test]
fn a_rows_day_line_says_its_need_then_three_families_then_partial() {
    use aterm_agent::harness::stats::Tally;
    let at_18_29 = (18 * 3_600 + 29 * 60) * 1000;
    let need = Tally {
        escalated: 1,
        human_wait_ms: 71_000,
        last_escalated_ms: Some(at_18_29),
        approved: 31,
        continued: 1,
        ..Tally::default()
    };
    assert_eq!(
        day_line(&need, Some(0)).as_deref(),
        Some("today: waited for you once, 1m at 18:29 \u{b7} 31 approved \u{b7} 1 kept going")
    );
    assert_eq!(
        day_line(&need, None).as_deref(),
        Some("today: waited for you once, 1m at 18:29 UTC \u{b7} 31 approved \u{b7} 1 kept going")
    );
    let many = Tally {
        escalated: 2,
        approved: 3,
        continued: 2,
        answered: 1,
        resumed: 4,
        land_prompts: 1,
        cut: true,
        ..Tally::default()
    };
    assert_eq!(
        day_line(&many, Some(0)).as_deref(),
        Some(
            "today: waited for you twice \u{b7} 3 approved \u{b7} 2 kept going \u{b7} 1 answered \u{b7} partial"
        )
    );
    let thrice = Tally {
        escalated: 3,
        ..Tally::default()
    };
    assert_eq!(
        day_line(&thrice, Some(0)).as_deref(),
        Some("today: waited for you 3 times")
    );
    assert_eq!(day_line(&Tally::default(), Some(0)), None);
    let unproven = Tally {
        approved: 2,
        unproven: 2,
        ..Tally::default()
    };
    assert_eq!(
        day_line(&unproven, Some(0)).as_deref(),
        Some("today: 2 approved")
    );
    let told = Tally {
        needs_told: 2,
        approved: 1,
        ..Tally::default()
    };
    assert_eq!(
        day_line(&told, Some(0)).as_deref(),
        Some("today: 2 needs, carried on \u{b7} 1 approved")
    );
    let both = Tally {
        escalated: 1,
        needs_told: 1,
        ..Tally::default()
    };
    assert_eq!(
        day_line(&both, Some(0)).as_deref(),
        Some("today: waited for you once \u{b7} 1 need, carried on")
    );
}

#[test]
fn each_row_takes_its_own_sessions_day_and_a_name_its_own() {
    use aterm_agent::harness::stats::Tally;
    let session = |local_id: u64, title: &str, place: &str| HarnessSession {
        local_id,
        sid: format!("s-{local_id}"),
        title: title.to_string(),
        place: place.to_string(),
        program: "Claude Code",
        word: None,
        view: AutopilotView::Cruise,
        glass: crate::claude_lights::ChipGlass::default(),
        armed_at_ms: None,
        set_by: None,
        default: Some(Autopilot::Cruise),
        supervised: true,
        need: None,
        hold: None,
    };
    let mut today = HarnessStats::per_session(0);
    today.approved = 9;
    let shares = today.by_session.as_mut().expect("per session");
    shares.insert(
        "s-1".to_string(),
        Tally {
            approved: 4,
            ..Tally::default()
        },
    );
    shares.insert(
        "s-gone".to_string(),
        Tally {
            approved: 5,
            ..Tally::default()
        },
    );
    let mut state = state_with_today(Some(today), false);
    state.posture = HarnessPosture::On(Autopilot::Cruise);
    state.sessions = vec![
        session(1, "Claude Code", "in tab 1, left pane"),
        session(2, "Claude Code", "in tab 1, right pane"),
        session(3, "fix the build", "in tab 2"),
    ];
    let rows = state.projection().sessions;
    let days: Vec<Option<&str>> = rows.iter().map(|r| r.day_line.as_deref()).collect();
    assert_eq!(days, [Some("today: 4 approved"), None, None]);
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Claude Code, tab 1, left pane",
            "Claude Code, tab 1, right pane",
            "fix the build"
        ]
    );
    assert!(
        rows[0].detail.starts_with("Tab 1, left pane"),
        "{}",
        rows[0].detail
    );
    assert!(
        rows[1].detail.starts_with("Tab 1, right pane"),
        "{}",
        rows[1].detail
    );
}

#[test]
fn a_rows_escalation_clock_uses_the_events_offset_not_the_readings() {
    use aterm_agent::harness::stats::Tally;
    const DAY: i64 = 86_400_000;
    const HOUR: i64 = 3_600_000;
    let read = |now: i64, before: i64, after: i64, zone: &dyn Fn(i64) -> Option<i64>| {
        let since = local_midnight_ms(now, zone);
        assert!(before >= since && after >= since && before <= now && after <= now);
        let mut today = HarnessStats::per_session(since);
        let shares = today.by_session.as_mut().expect("per session");
        for (sid, at) in [("before", before), ("after", after)] {
            shares.insert(
                sid.to_string(),
                Tally {
                    escalated: 1,
                    last_escalated_ms: Some(at),
                    ..Tally::default()
                },
            );
        }
        let report =
            HarnessStatsReport::new(today, HarnessStats::new(0), now, zone(now / 1000), zone);
        let checked = read_at_words(report.collected_unix_ms, report.offset_s);
        let session = |local_id, sid: &str| HarnessSession {
            local_id,
            sid: sid.to_string(),
            title: sid.to_string(),
            place: "in tab 1".to_string(),
            program: "Claude Code",
            word: None,
            view: AutopilotView::Cruise,
            glass: crate::claude_lights::ChipGlass::default(),
            armed_at_ms: None,
            set_by: None,
            default: Some(Autopilot::Cruise),
            supervised: false,
            need: None,
            hold: None,
        };
        let mut state = HarnessState::unobserved();
        state.observed = true;
        state.sessions = vec![session(1, "before"), session(2, "after")];
        state.stats.report = Some(std::sync::Arc::new(report));
        let words: Vec<String> = state
            .session_rows()
            .into_iter()
            .map(|row| row.day_line.expect("an escalation"))
            .collect();
        (checked, words)
    };
    let before = 10 * DAY + 6 * HOUR + HOUR / 2;
    let after = before + HOUR;
    let now = 10 * DAY + 16 * HOUR;
    for (offset, expected) in [
        (Some(0), ["06:30", "07:30"]),
        (Some(-4 * 3_600), ["02:30", "03:30"]),
        (None, ["06:30 UTC", "07:30 UTC"]),
    ] {
        let (checked, got) = read(now, before, after, &|_| offset);
        let expected: Vec<String> = expected
            .into_iter()
            .map(|at| format!("today: waited for you once at {at}"))
            .collect();
        assert_eq!(got, expected, "fixed or unknown offset {offset:?}");
        assert_eq!(checked, read_at_words(now, offset));
    }
    let spring = 10 * DAY + 7 * HOUR;
    let fall = 20 * DAY + 6 * HOUR;
    let zone = |unix_s: i64| {
        Some(if (spring..fall).contains(&(unix_s * 1000)) {
            -4 * 3_600
        } else {
            -5 * 3_600
        })
    };
    let mut observations = Vec::new();
    for (now, before, after, want) in [
        (
            10 * DAY + 16 * HOUR,
            spring - HOUR / 2,
            spring + HOUR / 2,
            ["01:30", "03:30"],
        ),
        (
            20 * DAY + 17 * HOUR,
            fall - HOUR / 2,
            fall + HOUR / 2,
            ["01:30", "01:30"],
        ),
    ] {
        assert_ne!(
            zone(before / 1000),
            zone(now / 1000),
            "earlier event's offset differs"
        );
        let (checked, got) = read(now, before, after, &zone);
        assert_eq!(checked, "at 12:00", "the reading keeps its own offset");
        let expected: Vec<String> = want
            .into_iter()
            .map(|at| format!("today: waited for you once at {at}"))
            .collect();
        observations.push((got, expected));
    }
    assert!(
        observations.iter().all(|(got, expected)| got == expected),
        "HARNESS_DAY_LINE_USED_COLLECTION_OFFSET: {observations:?}"
    );
}

#[test]
fn the_day_has_ended_only_past_the_midnight_after_its_reading() {
    const DAY: i64 = 86_400_000;
    let utc = |_: i64| Some(0);
    let slot = |since_ms: i64| HarnessStatsSlot {
        report: Some(std::sync::Arc::new(HarnessStatsReport::new(
            HarnessStats::per_session(since_ms),
            HarnessStats::new(0),
            since_ms,
            Some(0),
            &|_| Some(0),
        ))),
        ..HarnessStatsSlot::default()
    };
    let midnight = 20_000 * DAY;
    assert!(
        !slot(midnight).day_ended(midnight + DAY - 1, utc),
        "the same day"
    );
    assert!(
        slot(midnight).day_ended(midnight + DAY, utc),
        "the next day"
    );
    assert!(!HarnessStatsSlot::default().day_ended(midnight + 2 * DAY, utc));
    let east = |_: i64| Some(7_200);
    assert!(slot(midnight - 7_200_000).day_ended(midnight + DAY - 3_600_000, east));
    assert!(
        !slot(midnight - 7_200_000).day_ended(midnight + DAY - 3_600_000 - 3 * 3_600_000, east)
    );
}

#[test]
fn two_agents_in_one_tab_are_told_apart_by_their_panes() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let right = app.split_active_stub_tab(wid);
    run_claude(&app, 0);
    let sid = |app: &App, s: u64| app.pool.get(s).unwrap().ctx.self_id.as_str().to_string();
    let (left_sid, right_sid) = (sid(&app, 0), sid(&app, right));
    assert_eq!(
        app.session_place(&left_sid),
        app.upgrade_place(&left_sid),
        "one agent beside a shell: no pane"
    );
    assert!(!app.session_place(&left_sid).contains("pane"));
    run_claude(&app, right);
    let tab = app.upgrade_place(&left_sid);
    assert_eq!(app.session_place(&left_sid), format!("{tab}, left pane"));
    assert_eq!(app.session_place(&right_sid), format!("{tab}, right pane"));
    assert_eq!(
        app.upgrade_place(&right_sid),
        tab,
        "titles keep the tab alone"
    );
    app.harness = Some(idle_host());
    let rows = app.harness_state().projection().sessions;
    assert_eq!(rows.len(), 2);
    assert!(rows[0].name.ends_with("left pane"), "{}", rows[0].name);
    assert!(rows[1].name.ends_with("right pane"), "{}", rows[1].name);
    assert_ne!(rows[0].name, rows[1].name);
}

#[test]
fn show_raises_the_sessions_tab_and_focuses_its_pane() {
    use crate::native_app::HarnessRequest;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let right = app.split_active_stub_tab(wid);
    run_claude(&app, 0);
    run_claude(&app, right);
    let other = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(other));
    app.switch_tab_in(wid, 1);
    assert_eq!(
        app.focused_session_id(wid),
        Some(other),
        "another tab in front"
    );
    let tab_pane = |app: &App| app.focused_session_id(wid);
    app.switch_tab_in(wid, 0);
    assert_eq!(
        tab_pane(&app),
        Some(right),
        "the selection kept the right pane"
    );
    app.switch_tab_in(wid, 1);
    assert_eq!(tab_pane(&app), Some(other));
    app.execute_native_harness(HarnessRequest::Show { session: 0 });
    assert_eq!(tab_pane(&app), Some(0), "the left pane, focused");
    app.execute_native_harness(HarnessRequest::Show { session: right });
    assert_eq!(tab_pane(&app), Some(right), "the right pane, focused");
    app.execute_native_harness(HarnessRequest::Show { session: 777 });
    assert_eq!(tab_pane(&app), Some(right), "a gone session moves nothing");
}

#[test]
fn a_landing_stands_until_a_render_has_drawn_it() {
    use crate::app_introspect::AuxTarget;
    use crate::harness_screen::HarnessFocus;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    run_claude(&app, 0);
    let mut sessions = vec![0];
    for _ in 0..9 {
        let s = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(s));
        run_claude(&app, s);
        sessions.push(s);
    }
    let host = idle_host();
    app.harness = Some(host.clone());
    let far = *sessions.last().expect("sessions");
    let show = |s: u64| format!("ui key=\"harness/session/{s}/show\" ");
    let drawn = |app: &App, s: u64| {
        app.read_aux_controls(AuxTarget::Prefs)
            .into_iter()
            .find(|l| l.starts_with(&show(s)))
    };
    let selected =
        |line: &Option<String>| line.as_ref().is_some_and(|l| l.contains("selected:true"));
    let switch = format!(
        "ui key=\"settings/control/{}\" ",
        crate::prefs::EDIT_HARNESS_ENABLED
    );
    let supervision = |app: &App| {
        app.read_aux_controls(AuxTarget::Prefs)
            .iter()
            .any(|l| l.starts_with(&switch))
    };
    assert!(app.open_harness_settings(wid, HarnessFocus::Top));
    app.publish_native_harness_state();
    assert_eq!(drawn(&app, far), None, "the far row is below the fold");
    let scroll_to_the_bottom = |app: &mut App| {
        let (_, view) = app.active_native_view(wid).expect("the Settings view");
        for _ in 0..40 {
            app.dispatch_native_view_event(wid, view, AppEvent::ScrollPage(1))
                .expect("dispatched");
        }
        assert!(!supervision(app), "control: SUPERVISION is scrolled away");
        assert_eq!(
            drawn(app, 0),
            None,
            "control: the first row is scrolled away"
        );
        assert_eq!(
            drawn(app, far),
            None,
            "control: the far row is scrolled away"
        );
    };
    type Reply = (&'static str, fn(&mut App));
    let replies: [Reply; 3] = [
        ("HarnessChanged", |app| app.publish_native_harness_state()),
        ("PackagesChanged", |app| {
            app.dispatch_to_settings_views(&AppEvent::PackagesChanged { revision: 1 });
        }),
        ("UpdateChanged", |app| {
            app.dispatch_to_settings_views(&AppEvent::UpdateChanged { revision: 1 });
        }),
    ];
    for (reply, arrive) in replies {
        assert!(app.open_harness_settings(wid, HarnessFocus::Top));
        assert_eq!(drawn(&app, far), None, "{reply}: control");
        assert!(app.open_harness_settings(wid, HarnessFocus::Session(far)));
        arrive(&mut app);
        let landed = drawn(&app, far);
        assert!(selected(&landed), "{reply}, at the top: {landed:?}");
        scroll_to_the_bottom(&mut app);
        assert!(app.open_harness_settings(wid, HarnessFocus::Session(0)));
        arrive(&mut app);
        arrive(&mut app);
        let landed = drawn(&app, 0);
        assert!(selected(&landed), "{reply}, scrolled: {landed:?}");
        scroll_to_the_bottom(&mut app);
        assert!(app.open_harness_settings(wid, HarnessFocus::Supervision));
        arrive(&mut app);
        assert!(supervision(&app), "{reply}: SUPERVISION in view");
    }
    assert!(app.open_harness_settings(wid, HarnessFocus::Top));
    let late = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(late));
    assert!(app.open_harness_settings(wid, HarnessFocus::Session(late)));
    assert_eq!(drawn(&app, late), None, "not listed yet");
    app.dispatch_to_settings_views(&AppEvent::PackagesChanged { revision: 2 });
    assert_eq!(drawn(&app, late), None, "still not listed");
    run_claude(&app, late);
    app.publish_native_harness_state();
    let landed = drawn(&app, late);
    assert!(selected(&landed), "listed, and landed on: {landed:?}");
    let status = |app: &App| {
        app.read_aux_controls(AuxTarget::Prefs)
            .iter()
            .any(|l| l.starts_with("ui key=\"harness/status"))
    };
    let (_, view) = app.active_native_view(wid).expect("the Settings view");
    for _ in 0..40 {
        app.dispatch_native_view_event(wid, view, AppEvent::ScrollPage(-1))
            .expect("dispatched");
    }
    assert!(status(&app), "control: at the top");
    let later = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(later));
    assert!(app.open_harness_settings(wid, HarnessFocus::Session(later)));
    assert_eq!(drawn(&app, later), None, "not listed yet");
    assert!(app.open_harness_settings(wid, HarnessFocus::Top));
    app.publish_native_harness_state();
    assert!(status(&app), "View ▸ Coding Agent Harness…: the top");
    run_claude(&app, later);
    app.publish_native_harness_state();
    assert_eq!(
        drawn(&app, later),
        None,
        "listed after the page was opened at its top: not landed on"
    );
    assert!(status(&app), "the page stayed at its top");
    scroll_to_the_bottom(&mut app);
    assert!(!status(&app), "control: STATUS is scrolled away");
    assert!(app.open_harness_settings(wid, HarnessFocus::Top));
    assert!(
        status(&app),
        "View ▸ Coding Agent Harness… from the bottom: the top"
    );
    host.shutdown_and_join();
}

#[test]
fn a_sessions_landing_puts_its_row_in_view_drawn_selected() {
    use crate::app_introspect::AuxTarget;
    use crate::harness_screen::HarnessFocus;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    run_claude(&app, 0);
    let mut sessions = vec![0];
    for _ in 0..9 {
        let s = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(s));
        run_claude(&app, s);
        sessions.push(s);
    }
    let host = idle_host();
    app.harness = Some(host.clone());
    let far = *sessions.last().expect("sessions");
    let show = |s: u64| format!("ui key=\"harness/session/{s}/show\" ");
    let line = |app: &App, s: u64| {
        app.read_aux_controls(AuxTarget::Prefs)
            .into_iter()
            .find(|l| l.starts_with(&show(s)))
    };
    let selected =
        |line: &Option<String>| line.as_ref().is_some_and(|l| l.contains("selected:true"));
    assert!(app.open_harness_settings(wid, HarnessFocus::Top));
    app.publish_native_harness_state();
    let landed: Vec<u64> = sessions
        .iter()
        .copied()
        .filter(|&s| selected(&line(&app, s)))
        .collect();
    assert!(landed.is_empty(), "nothing landed: {landed:?}");
    assert_eq!(
        line(&app, far),
        None,
        "the far row is past the first screen"
    );
    let closed = app.close_settings_tabs();
    assert!(closed, "the Settings tab closed");
    assert!(app.open_harness_settings(wid, HarnessFocus::Session(far)));
    app.publish_native_harness_state();
    let landed = line(&app, far);
    assert!(selected(&landed), "landed and selected: {landed:?}");
    assert!(app.open_harness_settings(wid, HarnessFocus::Session(0)));
    let first = line(&app, 0);
    assert!(selected(&first), "{first:?}");
    assert!(!selected(&line(&app, far)), "one row selected");
    let (_, view) = app.active_native_view(wid).expect("the Settings view");
    app.dispatch_native_view_event(
        wid,
        view,
        AppEvent::Action(ActionInvocation {
            id: ActionId::new("harness/refresh"),
            value: None,
        }),
    )
    .expect("dispatched");
    assert!(!selected(&line(&app, 0)), "the press let it go");
    assert!(app.open_harness_settings(wid, HarnessFocus::Supervision));
    let switch = format!(
        "ui key=\"settings/control/{}\" ",
        crate::prefs::EDIT_HARNESS_ENABLED
    );
    assert!(
        app.read_aux_controls(AuxTarget::Prefs)
            .iter()
            .any(|l| l.starts_with(&switch)),
        "SUPERVISION in view"
    );
    host.shutdown_and_join();
}

fn decision(
    sid: &str,
    t_ms: i64,
    kind: aterm_agent::harness::stats::DecisionKind,
    (asked, did, why): (&str, &str, Option<&str>),
) -> aterm_agent::harness::stats::Decision {
    aterm_agent::harness::stats::Decision {
        t_ms,
        sid: sid.to_string(),
        kind,
        asked: Box::from(asked),
        said: false,
        did: Box::from(did),
        why: why.map(Box::from),
        times: 1,
    }
}

fn state_deciding(
    decisions: Vec<aterm_agent::harness::stats::Decision>,
    total: u64,
) -> HarnessState {
    let mut today = HarnessStats::per_session(0);
    today.declined = total;
    let decided = today.decided.as_mut().expect("per session keeps decisions");
    decided.rows = decisions;
    decided.total = total;
    let mut state = state_with_today(Some(today), false);
    state.sessions = vec![HarnessSession {
        local_id: 7,
        sid: "s-7".to_string(),
        title: "fix the build".to_string(),
        place: "in tab 1".to_string(),
        program: "Claude Code",
        word: None,
        view: AutopilotView::Cruise,
        glass: crate::claude_lights::ChipGlass::default(),
        armed_at_ms: None,
        set_by: None,
        default: Some(Autopilot::Cruise),
        supervised: true,
        need: None,
        hold: None,
    }];
    state.decided_open = vec![OpenSession {
        sid: "s-9".to_string(),
        local_id: 9,
        title: "zsh".to_string(),
    }];
    state
}

#[test]
fn decided_rows_name_a_live_and_a_closed_session() {
    use aterm_agent::harness::stats::DecisionKind;
    let at = |h: i64, m: i64| (h * 3_600 + m * 60) * 1_000;
    let mut press = decision(
        "s-9",
        at(18, 28),
        DecisionKind::Unproven,
        (
            "python3 - <<PY \u{2026}",
            "",
            Some("Approve permission boxes: All"),
        ),
    );
    press.times = 3;
    let mut said = decision(
        "s-7",
        at(18, 30),
        DecisionKind::Answered,
        (
            "Done for now.",
            "the answer text \u{201c}Decide.\u{201d}",
            None,
        ),
    );
    said.said = true;
    let decisions = vec![
        decision(
            "s-7",
            at(18, 25),
            DecisionKind::Chose,
            (
                "Should divide() return true division (float) or floor division?",
                "True division (float)",
                Some("no option was marked Recommended, so it took the first"),
            ),
        ),
        press,
        said,
        decision(
            "s-gone",
            at(18, 34),
            DecisionKind::Answered,
            (
                "What should I add next?",
                "the land answer: land all of the work on the remote main soonest, start nothing \
                 new",
                None,
            ),
        ),
        decision(
            "s-7",
            at(18, 40),
            DecisionKind::Declined,
            ("rm -rf build", "", Some("Nothing outside the worktree.")),
        ),
    ];
    let id = |k: usize| decisions[k].id();
    let ids = [id(4), id(3), id(2), id(1), id(0)];
    let state = state_deciding(decisions, 5);
    let rows: Vec<(String, String, String, Option<u64>)> = state
        .decided_rows()
        .into_iter()
        .map(|row| (row.id, row.clock, row.sentence, row.show))
        .collect();
    let [i5, i4, i3, i2, i1] = ids;
    assert_eq!(
        rows,
        [
            (
                i5,
                "18:40".to_string(),
                "fix the build: declined rm -rf build: Nothing outside the worktree.".to_string(),
                Some(7)
            ),
            (
                i4,
                "18:34".to_string(),
                "A closed session asked \u{201c}What should I add next?\u{201d} \u{2192} the land \
                 answer: land all of the work on the remote main soonest, start nothing new"
                    .to_string(),
                None
            ),
            (
                i3,
                "18:30".to_string(),
                "fix the build said \u{201c}Done for now.\u{201d} \u{2192} the answer text \
                 \u{201c}Decide.\u{201d}"
                    .to_string(),
                Some(7)
            ),
            (
                i2,
                "18:28".to_string(),
                "zsh: pressed Yes on python3 - <<PY \u{2026} \u{d7}3 under Approve permission \
                 boxes: All; no safe rule proved it"
                    .to_string(),
                Some(9)
            ),
            (
                i1,
                "18:25".to_string(),
                "fix the build asked \u{201c}Should divide() return true division (float) or \
                 floor division?\u{201d} \u{2192} the harness chose True division (float): no \
                 option was marked Recommended, so it took the first"
                    .to_string(),
                Some(7)
            ),
        ]
    );
    assert!(
        rows.iter()
            .all(|(_, _, sentence, _)| !sentence.to_lowercase().contains("unsafe"))
    );
    assert_eq!(state.projection().decided_total, 5);
    let mut closed = state.clone();
    closed.sessions.clear();
    let first = closed.decided_rows().remove(0);
    assert_eq!(
        (first.sentence.as_str(), first.show),
        (
            "A closed session: declined rm -rf build: Nothing outside the worktree.",
            None
        )
    );
}

#[test]
fn statuss_second_line_counts_the_choices_made_for_you() {
    use aterm_agent::harness::stats::DecisionKind;
    let one = decision("s-7", 0, DecisionKind::Declined, ("x", "", None));
    let state = state_deciding(vec![one.clone(), one.clone(), one.clone()], 70);
    let (_, rest) = state.today_lines().expect("lines");
    assert!(rest.ends_with("70 choices made for you"), "{rest}");
    let state = state_deciding(vec![one], 1);
    assert_eq!(
        state.today_lines().map(|(_, rest)| rest).as_deref(),
        Some("1 choice made for you")
    );
    let state = state_deciding(Vec::new(), 0);
    let (_, rest) = state.today_lines().expect("lines");
    assert!(!rest.contains("made for you"), "CONTROL: {rest:?}");
}

#[test]
fn an_unproven_option_and_a_kept_reset_in_words() {
    use aterm_agent::harness::stats::DecisionKind;
    let all = Some("Approve permission boxes: All");
    let mut wait = decision(
        "s-7",
        0,
        DecisionKind::Unproven,
        ("", "Stop and wait for limit to reset", all),
    );
    wait.times = 2;
    let said = |d: &aterm_agent::harness::stats::Decision| decision_sentence(d, "fix the build");
    assert_eq!(
        said(&decision(
            "s-7",
            0,
            DecisionKind::Unproven,
            ("Enable the new model?", "No, keep default", all)
        )),
        "fix the build: chose \u{201c}No, keep default\u{201d} on Enable the new model? under \
         Approve permission boxes: All; no safe rule proved it"
    );
    assert_eq!(
        said(&wait),
        "fix the build: chose \u{201c}Stop and wait for limit to reset\u{201d} on a box \u{d7}2 \
         under Approve permission boxes: All; no safe rule proved it"
    );
    assert_eq!(
        said(&decision(
            "s-7",
            0,
            DecisionKind::KeptReset,
            (
                "",
                "",
                Some("the check for a reset took longer than it may")
            )
        )),
        "fix the build: kept its limit reset unspent: the check for a reset took longer than it may"
    );
    assert_eq!(
        said(&decision("s-7", 0, DecisionKind::KeptReset, ("", "", None))),
        "fix the build: kept its limit reset unspent"
    );
    for yes in ["", "Yes"] {
        assert!(
            said(&decision(
                "s-7",
                0,
                DecisionKind::Unproven,
                ("touch a", yes, all)
            ))
            .starts_with("fix the build: pressed Yes on touch a under"),
            "{yes:?}"
        );
    }
}

#[test]
fn a_decided_row_keeps_its_key_when_an_older_row_sorts_in() {
    use aterm_agent::harness::stats::DecisionKind;
    let words = ("rm -rf build", "", Some("no"));
    let a = decision("s-7", 1_000, DecisionKind::Declined, words);
    let b = decision("s-7", 3_000, DecisionKind::Declined, words);
    let late = decision(
        "s-7",
        2_000,
        DecisionKind::Chose,
        ("Late?", "Yes", Some("the option marked Recommended")),
    );
    let before = state_deciding(vec![a.clone(), b.clone()], 2).decided_rows();
    let after = state_deciding(vec![a.clone(), late, b.clone()], 3).decided_rows();
    assert_eq!(before[0].id, b.id());
    assert_eq!(after[0].id, b.id(), "the newest keeps its key");
    assert_eq!(after[2].id, a.id());
    let twice = state_deciding(vec![a.clone(), a.clone()], 2).decided_rows();
    assert_eq!(
        twice.iter().map(|row| row.id.clone()).collect::<Vec<_>>(),
        [format!("{}-2", a.id()), a.id()]
    );
    let place = |rows: &[crate::harness_screen::DecidedRow], id: &str| {
        rows.len() - rows.iter().position(|row| row.id == id).expect("listed")
    };
    assert_eq!((place(&before, &b.id()), place(&after, &b.id())), (2, 3));
}

#[test]
fn each_decisions_clock_uses_its_events_offset_not_the_readings() {
    use aterm_agent::harness::stats::DecisionKind;
    const HOUR: i64 = 3_600_000;
    const DAY: i64 = 24 * HOUR;
    let read = |now: i64, before: i64, after: i64, zone: &dyn Fn(i64) -> Option<i64>| {
        let mut today = HarnessStats::per_session(0);
        let decided = today.decided.as_mut().expect("per-session decisions");
        decided.total = 2;
        decided.rows = vec![
            decision(
                "before",
                before,
                DecisionKind::Declined,
                ("before?", "", None),
            ),
            decision("after", after, DecisionKind::Declined, ("after?", "", None)),
        ];
        let report = HarnessStatsReport::new(
            today,
            HarnessStats::new(0),
            now,
            zone(now.div_euclid(1000)),
            zone,
        );
        let checked = read_at_words(report.collected_unix_ms, report.offset_s);
        let mut state = HarnessState::unobserved();
        state.stats.report = Some(std::sync::Arc::new(report));
        let clocks: Vec<String> = state.decided_rows().into_iter().map(|r| r.clock).collect();
        (checked, clocks)
    };
    let before = 10 * DAY + 6 * HOUR + HOUR / 2;
    let after = before + HOUR;
    let now = 10 * DAY + 16 * HOUR;
    for (offset, want) in [
        (Some(0), ["07:30", "06:30"]),
        (Some(-4 * 3_600), ["03:30", "02:30"]),
        (None, ["07:30 UTC", "06:30 UTC"]),
    ] {
        let (checked, clocks) = read(now, before, after, &|_| offset);
        assert_eq!(clocks, want, "CONTROL: fixed or unknown zone");
        assert_eq!(checked, read_at_words(now, offset));
    }
    let spring = 10 * DAY + 7 * HOUR;
    let fall = 20 * DAY + 6 * HOUR;
    let zone = |unix: i64| {
        Some(if (spring..fall).contains(&(unix * 1000)) {
            -4 * 3_600
        } else {
            -5 * 3_600
        })
    };
    for (now, boundary, want) in [
        (10 * DAY + 16 * HOUR, spring, ["03:30", "01:30"]),
        (20 * DAY + 17 * HOUR, fall, ["01:30", "01:30"]),
    ] {
        let before = boundary - HOUR / 2;
        let after = boundary + HOUR / 2;
        assert_ne!(zone(before / 1000), zone(after / 1000));
        let (checked, clocks) = read(now, before, after, &zone);
        assert_eq!(
            clocks, want,
            "DECISION_EVENT_CLOCK_NEEDS_EVENT_OFFSET: newest first"
        );
        assert_eq!(checked, read_at_words(now, zone(now / 1000)));
    }
    let (_, clocks) = read(now, before, after, &|at| {
        (at != before / 1000).then_some(-4 * 3_600)
    });
    assert_eq!(clocks, ["03:30", "06:30 UTC"]);
}

/// THE STATS READING DIALS A SIBLING AS ITS HANDSHAKE ASKS: the token beside
/// the sibling's socket on an `AUTH <hex>` line, then `ls`.
#[cfg(unix)]
#[test]
fn the_stats_reading_dials_a_sibling_on_its_auth_line_and_never_blind() {
    use std::io::{Read as _, Write as _};
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    let scratch = crate::test_scratch::short_dir("the siblings' sockets");
    let dir = scratch.path().to_path_buf();
    let (with, without) = (4_000_001_u32, 4_000_002_u32);
    let sock = |pid: u32| dir.join(format!("aterm-{pid}.sock"));
    let token_file = dir.join(format!("aterm-{with}.token"));
    let token = "ab".repeat(32);
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&token_file)
        .and_then(|mut f| f.write_all(token.as_bytes()))
        .expect("the sibling's token");
    let done = std::sync::Arc::new(AtomicBool::new(false));
    let listen = |pid: u32| {
        let listener = UnixListener::bind(sock(pid)).expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let done = std::sync::Arc::clone(&done);
        std::thread::spawn(move || {
            let mut sent: Vec<Vec<u8>> = Vec::new();
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).expect("blocking");
                        let mut bytes = Vec::new();
                        let _ = stream.read_to_end(&mut bytes);
                        sent.push(bytes);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if done.load(Ordering::Acquire) {
                            return sent;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    Err(e) => panic!("accept: {e}"),
                }
            }
        })
    };
    let (answered, blind) = (listen(with), listen(without));

    let _ = sibling_aterm_tabs_in(vec![dir.clone()]);
    done.store(true, Ordering::Release);
    let answered = answered.join().expect("the sibling's listener");
    let blind = blind.join().expect("the tokenless sibling's listener");

    assert!(!answered.is_empty(), "the reading dials the sibling");
    for bytes in &answered {
        assert_eq!(
            String::from_utf8_lossy(bytes),
            format!("AUTH {token}\nsessions\n"),
            "the handshake's own line, then the roster verb"
        );
    }
    assert!(!blind.is_empty(), "the tokenless sibling is connected to");
    assert!(
        blind.iter().all(Vec::is_empty),
        "and sent nothing blind: {blind:?}"
    );
}

#[test]
fn the_harness_reading_asks_nothing_of_the_fleet() {
    let sweeps = || FLEET_SWEEPS.with(std::cell::Cell::get);
    let utc = |_: i64| Some(0);
    let state = aterm_tempfile::tempdir().expect("scratch state");
    let hs = aterm_tempfile::tempdir().expect("scratch harness state");
    let read = |slot: &FleetSlot| {
        read_harness_stats(
            Some(state.path()),
            Some(hs.path()),
            None,
            "1-1.json",
            0,
            utc,
            slot,
        )
        .expect("a reading")
    };
    let before = sweeps();
    read(&FleetSlot::default());
    assert_eq!(
        sweeps(),
        before,
        "the Harness page's reading sweeps nothing"
    );
    let fleet = FleetAsk {
        fleet: true,
        local_vitals: true,
    };
    read(&FleetSlot::new(fleet));
    assert_eq!(
        sweeps(),
        before + 1,
        "CONTROL: Tab Control's sweeps the fleet"
    );
    let raised = FleetSlot::default();
    assert!(raised.raise(fleet));
    read(&raised);
    assert_eq!(sweeps(), before + 2, "a raised reading sweeps the fleet");
    let _ = read_harness_stats(None, None, None, "1-1.json", 0, utc, &FleetSlot::new(fleet));
    assert_eq!(sweeps(), before + 2, "a failed reading sweeps nothing");
    assert!(
        !raised.raise(fleet),
        "taken: a raise after it does not count"
    );
    let taken = FleetSlot::default();
    assert_eq!(taken.take(), FleetAsk::default());
    assert!(!taken.raise(fleet));

    let mut app = App::headless_for_test();
    assert!(app.open_harness_settings(WindowId(0), HarnessFocus::Top));
    assert!(app.harness_page_shown() && !app.tabs_page_shown());
    app.start_harness_stats_refresh();
    assert_eq!(app.harness_screen.last_fleet_ask, Some(FleetAsk::default()));
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Tabs));
    assert!(app.tabs_page_shown());
    app.start_harness_stats_refresh();
    assert_eq!(app.harness_screen.last_fleet_ask, Some(fleet));
    app.vitals.snapshot = Some(std::sync::Arc::new(aterm_vitals::model::Snapshot::default()));
    app.start_harness_stats_refresh();
    assert_eq!(
        app.harness_screen.last_fleet_ask,
        Some(FleetAsk {
            fleet: true,
            local_vitals: false,
        }),
        "VitalsHost's snapshot is this Mac's vitals"
    );
}

#[test]
fn a_fleetless_reading_keeps_the_last_sweep_and_owes_tab_control_one() {
    let wid = WindowId(0);
    let report = |sibling: &str| {
        let mut report = HarnessStatsReport::new(
            HarnessStats::per_session(0),
            HarnessStats::new(0),
            0,
            Some(0),
            &|_| Some(0),
        );
        if !sibling.is_empty() {
            report.sibling_tabs = vec![TabSpreadsheetRow {
                key: sibling.to_string(),
                ..TabSpreadsheetRow::default()
            }];
        }
        Box::new(report)
    };
    let land = |app: &mut App, landed: Box<HarnessStatsReport>| {
        app.harness_screen.stats.collecting = true;
        let sequence = app.harness_screen.sequence;
        app.finish_harness_stats(sequence, Ok(landed));
    };
    let siblings = |app: &App| {
        app.harness_screen.stats.report.as_deref().map(|r| {
            r.sibling_tabs
                .iter()
                .map(|t| t.key.clone())
                .collect::<Vec<_>>()
        })
    };
    let mut app = App::headless_for_test();
    assert!(app.open_harness_settings(wid, HarnessFocus::Top));
    app.harness_screen.reading = FleetAsk {
        fleet: true,
        local_vitals: false,
    };
    land(&mut app, report("sibling/1"));
    assert_eq!(siblings(&app), Some(vec!["sibling/1".to_string()]));
    app.start_harness_stats_refresh();
    assert!(!app.harness_screen.reading.fleet);
    let asked = app.harness_screen.readings_asked;
    land(&mut app, report(""));
    assert_eq!(
        siblings(&app),
        Some(vec!["sibling/1".to_string()]),
        "the last sweep kept"
    );
    assert_eq!(
        app.harness_screen.readings_asked, asked,
        "CONTROL: Tab Control not shown, nothing more asked"
    );
    let swept = FleetAsk {
        fleet: true,
        local_vitals: true,
    };
    app.start_harness_stats_refresh();
    app.harness_screen.stats.collecting = true;
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Tabs));
    assert!(
        app.harness_screen.reading.fleet,
        "the running reading raised"
    );
    assert_eq!(
        app.harness_screen.fleet_slot.take(),
        swept,
        "the reading's thread sweeps the fleet once done with the ledgers"
    );
    let asked = app.harness_screen.readings_asked;
    let sequence = app.harness_screen.sequence;
    app.finish_harness_stats(sequence, Ok(report("sibling/2")));
    assert_eq!(
        app.harness_screen.readings_asked, asked,
        "no second reading of the ledgers"
    );
    assert_eq!(
        siblings(&app),
        Some(vec!["sibling/2".to_string()]),
        "the raised reading's fleet replaces the last"
    );
    assert!(app.open_harness_settings(wid, HarnessFocus::Top));
    assert!(!app.tabs_page_shown());
    app.start_harness_stats_refresh();
    assert!(!app.harness_screen.reading.fleet);
    app.harness_screen.stats.collecting = true;
    assert_eq!(app.harness_screen.fleet_slot.take(), FleetAsk::default());
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Tabs));
    assert!(!app.harness_screen.reading.fleet, "taken: not raised");
    let asked = app.harness_screen.readings_asked;
    let sequence = app.harness_screen.sequence;
    app.finish_harness_stats(sequence, Ok(report("")));
    assert_eq!(
        siblings(&app),
        Some(vec!["sibling/2".to_string()]),
        "the last sweep kept"
    );
    assert_eq!(app.harness_screen.readings_asked, asked + 1, "owed at once");
    assert_eq!(app.harness_screen.last_fleet_ask, Some(swept));
    land(&mut app, report("sibling/3"));
    assert_eq!(siblings(&app), Some(vec!["sibling/3".to_string()]));
}

fn listed_with(local_id: u64, verdict: &crate::needs::Verdict) -> HarnessSession {
    HarnessSession {
        local_id,
        sid: format!("s-{local_id}"),
        title: format!("tab {local_id}"),
        place: format!("in tab {local_id}"),
        program: "Claude Code",
        word: None,
        view: AutopilotView::Cruise,
        glass: crate::claude_lights::ChipGlass::default(),
        armed_at_ms: None,
        set_by: None,
        default: Some(Autopilot::Cruise),
        supervised: true,
        need: verdict.harness_ask().cloned(),
        hold: None,
    }
}

fn asked(owner: &str, text: &str) -> crate::needs::Verdict {
    crate::needs::of(&crate::needs::NeedFacts {
        attention: vec![crate::needs::AttentionFact {
            owner: owner.to_string(),
            text: text.to_string(),
            kind: Some(aterm_types::needs::AttentionKind::Ask),
        }],
        ..crate::needs::NeedFacts::default()
    })
}

#[test]
fn the_status_card_counts_sessions_that_need_you() {
    let needs = crate::harness_host::NEEDS_ATTENTION_OWNER;
    let approval = crate::needs::of(&crate::needs::NeedFacts {
        agent: Some(crate::needs::AgentFact {
            word: "prompt",
            detail: Some("bash:not-read-only".to_string()),
            rev: 3,
            subject: Some("rm -rf build".to_string()),
        }),
        ..crate::needs::NeedFacts::default()
    });
    assert_eq!(
        approval.need.as_ref().map(|n| n.word),
        Some(aterm_types::needs::NeedWord::Approval)
    );
    let verdicts = [
        asked(
            needs,
            "claude needs you: sign 0.48.0 (+1 more) \u{2014} carrying on",
        ),
        approval,
        asked(
            "lab",
            "claude needs you: look at the lab \u{2014} carrying on",
        ),
        crate::needs::Verdict::default(),
    ];
    let mut state = HarnessState::unobserved();
    state.observed = true;
    state.posture = HarnessPosture::On(Autopilot::Cruise);
    state.sessions = verdicts
        .iter()
        .zip(1..)
        .map(|(verdict, id)| listed_with(id, verdict))
        .collect();
    let sessions = |state: &HarnessState| {
        state
            .status_rows()
            .into_iter()
            .find(|(key, _)| key == "Sessions")
            .map(|(_, value)| value)
    };
    assert_eq!(
        sessions(&state).as_deref(),
        Some("4 of 4 supervised \u{b7} 1 need you")
    );
    state.sessions.truncate(1);
    state.sessions[0].need = None;
    assert_eq!(sessions(&state).as_deref(), Some("1 of 1 supervised"));
}

#[test]
fn a_need_heads_the_rows_state_line() {
    use crate::claude_lights::{ChipGlass, Clock, Stamp};
    use crate::harness_glass::PauseGlass;
    const TODAY: i64 = 20_007;
    let ask = asked(
        crate::harness_host::NEEDS_ATTENTION_OWNER,
        "claude needs you: sign 0.48.0 (+1 more) \u{2014} carrying on",
    );
    let row = |glass: ChipGlass| HarnessSessionRow {
        local_id: 7,
        title: "fix the build".to_string(),
        name: "fix the build".to_string(),
        day_line: None,
        detail: String::new(),
        mode: Some(Autopilot::Cruise),
        default: Some(Autopilot::Cruise),
        view: AutopilotView::Cruise,
        glass,
        armed_at_ms: None,
        today: TODAY,
        setting: "follows the default".to_string(),
        need: ask.harness_ask().cloned(),
        hold: None,
    };
    let pausing = ChipGlass {
        pause: Some(PauseGlass {
            until: Stamp {
                clock: Clock::of_minute(18 * 60 + 41),
                day: TODAY,
            },
            short: 3,
        }),
        ..ChipGlass::default()
    };
    assert_eq!(
        row(pausing).state_line().as_deref(),
        Some(
            "needs you: sign 0.48.0 (+1 more); carrying on \u{b7} pausing until 18:41 (3 short \
             turns in a row)"
        )
    );
    assert_eq!(
        row(ChipGlass::default()).state_line().as_deref(),
        Some("needs you: sign 0.48.0 (+1 more); carrying on")
    );
}

#[test]
fn the_page_reads_a_published_harness_need() {
    let mut app = App::headless_for_test();
    run_claude(&app, 0);
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    ctx.meta
        .lock()
        .unwrap()
        .set_attention_kind(
            crate::harness_host::NEEDS_ATTENTION_OWNER,
            Some("claude needs you: sign 0.48.0 (+2 more) \u{2014} carrying on".into()),
            Some(aterm_types::needs::AttentionKind::Ask),
        )
        .expect("a slot");
    assert_eq!(
        app.harness_state().sessions[0].need,
        None,
        "CONTROL: unpublished"
    );
    app.publish_need(0);
    let state = app.harness_state();
    let need = state.sessions[0].need.as_ref().expect("the published need");
    assert_eq!(
        (need.first.as_str(), need.more, need.program),
        ("sign 0.48.0", 2, aterm_phase::Program::Claude)
    );
    assert!(
        state
            .status_rows()
            .iter()
            .any(|(key, value)| key == "Sessions" && value.ends_with("\u{b7} 1 need you")),
        "{:?}",
        state.status_rows()
    );
}

#[test]
fn the_open_page_follows_a_harness_need() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    run_claude(&app, 0);
    assert!(app.open_harness_settings(wid, crate::harness_screen::HarnessFocus::Top));
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    let set = |text: Option<&str>| {
        ctx.meta
            .lock()
            .unwrap()
            .set_attention_kind(
                crate::harness_host::NEEDS_ATTENTION_OWNER,
                text.map(str::to_string),
                text.map(|_| aterm_types::needs::AttentionKind::Ask),
            )
            .expect("a slot");
    };
    let revision = app.harness_screen.revision;
    set(Some(
        "claude needs you: sign 0.48.0 (+1 more) \u{2014} carrying on",
    ));
    app.refresh_presence_session(0);
    assert!(
        app.harness_screen.revision > revision,
        "the open page was republished with the need"
    );
    assert_eq!(
        app.harness_state().sessions[0]
            .need
            .as_ref()
            .map(|need| (need.first.as_str(), need.more)),
        Some(("sign 0.48.0", 1))
    );
    let revision = app.harness_screen.revision;
    set(None);
    app.refresh_presence_session(0);
    assert!(
        app.harness_screen.revision > revision,
        "the open page was republished without it"
    );
    assert_eq!(app.harness_state().sessions[0].need, None);
}
