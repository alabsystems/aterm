// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests that every window surface for a landing says `landed`, seen or not.

use std::sync::Arc;

use aterm_agent::harness::acts::TaskFace;

use crate::status_item::{EndRow, Glance, SessionRow, TaskRow, compose_status_menu, menu_lines};
use crate::task_record::TaskState;
use crate::{App, WindowId};

const NOW: i64 = 1_800_000_000;

fn ms(secs: i64) -> u64 {
    u64::try_from(secs).expect("a positive time") * 1000
}

fn worker(
    id: u64,
    label: &str,
    from: &str,
    state: TaskState,
    why: Option<&'static str>,
    result: u64,
) -> SessionRow {
    SessionRow {
        id,
        sid: format!("s-{id:020}"),
        title: label.to_string(),
        agent: "idle",
        task: Some(TaskRow {
            state,
            why,
            from: from.to_string(),
            entered: 7,
            since_unix_ms: ms(NOW - 4 * 60),
            result_bytes: result,
            result_path: (result > 0).then(|| std::path::PathBuf::from("/tmp/r.md")),
            seen: false,
        }),
        ..SessionRow::default()
    }
}

fn menu(rows: &[SessionRow]) -> Vec<String> {
    let glance = Glance::build(rows, &[], None, ms(NOW), None);
    menu_lines(&compose_status_menu(&glance, NOW), &[])
}

fn line_with(lines: &[String], words: &str) -> String {
    let found: Vec<&String> = lines.iter().filter(|l| l.contains(words)).collect();
    assert_eq!(found.len(), 1, "one line with {words:?}: {lines:#?}");
    found[0]
        .rsplit_once(" at=")
        .map_or_else(|| found[0].clone(), |(line, _)| line.to_string())
}

#[test]
fn a_landed_worker_reads_landed_and_an_older_done_reads_done() {
    let manager = SessionRow {
        id: 1,
        sid: "s-manager".to_string(),
        title: "lab-sweep".to_string(),
        agent: "busy",
        ..SessionRow::default()
    };
    let rows = [
        manager,
        worker(2, "bench", "s-manager", TaskState::Working, None, 0),
        worker(
            3,
            "lint",
            "s-manager",
            TaskState::Done,
            Some(crate::task_record::WHY_LANDED),
            2150,
        ),
        worker(4, "fmt", "s-manager", TaskState::Done, Some("marker"), 0),
    ];
    let lines = menu(&rows);
    assert!(
        line_with(&lines, "\u{2713} lint")
            .contains("\u{2713} lint \u{2014} landed 4m \u{b7} 2.1 KB"),
        "{lines:#?}"
    );
    assert!(
        line_with(&lines, "\u{2713} fmt").contains("\u{2713} fmt \u{2014} done 4m"),
        "{lines:#?}"
    );
    assert!(
        line_with(&lines, "bench \u{2014}").contains("\u{25cf} bench \u{2014} working 4m"),
        "{lines:#?}"
    );
    assert!(
        line_with(&lines, "lab-sweep \u{2014}")
            .contains("lab-sweep \u{2014} 3 workers: 1 working, 1 landed, 1 done"),
        "{lines:#?}"
    );
}

#[test]
fn this_mac_counts_landed_and_exited_apart() {
    let plain = SessionRow {
        id: 1,
        sid: "s-plain".to_string(),
        title: "docs pass".to_string(),
        agent: "idle",
        landed: true,
        ..SessionRow::default()
    };
    let rows = [
        plain,
        worker(
            2,
            "lint",
            crate::task_record::FROM_CTL,
            TaskState::Done,
            Some(crate::task_record::WHY_LANDED),
            0,
        ),
        worker(
            3,
            "bench",
            crate::task_record::FROM_CTL,
            TaskState::Exited,
            Some("exit"),
            0,
        ),
    ];
    let lines = menu(&rows);
    assert_eq!(
        line_with(&lines, "This Mac \u{2014}"),
        "statusmenu \"This Mac \u{2014} 2 landed \u{b7} 1 exited\" enabled=no"
    );
    let mut older = rows.to_vec();
    older.push(worker(
        4,
        "fmt",
        crate::task_record::FROM_CTL,
        TaskState::Done,
        Some("done-check"),
        0,
    ));
    assert_eq!(
        line_with(&menu(&older), "This Mac \u{2014}"),
        "statusmenu \"This Mac \u{2014} 2 landed \u{b7} 1 done \u{b7} 1 exited\" enabled=no"
    );
}

#[test]
fn a_seen_landing_stays_landed_in_done_for_you() {
    let landed = |id, label| {
        worker(
            id,
            label,
            crate::task_record::FROM_CTL,
            TaskState::Done,
            Some(crate::task_record::WHY_LANDED),
            0,
        )
    };
    let mut seen = landed(1, "seen");
    seen.end = Some(EndRow {
        serial: 1,
        since_unix_ms: ms(NOW - 4 * 60),
        seen: true,
    });
    let quick = landed(2, "quick");
    let older = worker(
        3,
        "older",
        crate::task_record::FROM_CTL,
        TaskState::Done,
        Some("marker"),
        0,
    );
    let lines = menu(&[seen, quick, older]);
    assert!(
        line_with(&lines, "\u{2713} seen").contains("\u{2713} seen \u{2014} landed 4m"),
        "{lines:#?}"
    );
    assert!(
        line_with(&lines, "\u{2713} quick").contains("\u{2713} quick \u{2014} landed 4m"),
        "{lines:#?}"
    );
    assert!(
        line_with(&lines, "\u{2713} older").contains("\u{2713} older \u{2014} done 4m"),
        "{lines:#?}"
    );
}

fn land_worker(app: &mut App, local: u64, title: &str) {
    let ctx = app.pool.get(local).expect("pooled").ctx.clone();
    ctx.meta
        .lock()
        .expect("meta")
        .set("title", Some(title.to_string()));
    let mut mark = crate::task_record::TaskMark::new(TaskState::Done);
    mark.why = Some(crate::task_record::WHY_LANDED);
    crate::task_record::mark_session_task(&ctx, local, &mark).expect("marked");
    app.on_task_changed(local);
}

fn two_landed_workers(first: &str, second: &str) -> (App, u64) {
    let mut app = App::headless_for_test();
    let id = app.next_session_id;
    app.push_stub_tab(WindowId(0), crate::stub_session(id));
    land_worker(&mut app, 0, first);
    land_worker(&mut app, id, second);
    (app, id)
}

fn done_rows(app: &App) -> Vec<String> {
    let glance = app.status_glance();
    assert_eq!(glance.done.len(), 2, "both landings: {:?}", glance.done);
    let mut labels: Vec<String> = glance.done.iter().map(|d| d.label.clone()).collect();
    labels.sort();
    labels
}

#[test]
fn two_rows_with_one_label_say_where_they_are() {
    let (app, _) = two_landed_workers("Claude Code", "Claude Code");
    assert_eq!(
        done_rows(&app),
        ["Claude Code in tab 1", "Claude Code in tab 2"]
    );
}

#[test]
fn a_unique_row_names_no_place() {
    let (app, _) = two_landed_workers("Claude Code", "lint");
    assert_eq!(done_rows(&app), ["Claude Code", "lint"]);
}

#[test]
fn two_panes_of_one_tab_with_one_label_say_which_pane() {
    let mut app = App::headless_for_test();
    let right = app.split_active_stub_tab(WindowId(0));
    land_worker(&mut app, 0, "Claude Code");
    land_worker(&mut app, right, "Claude Code");
    let rows = done_rows(&app);
    assert_ne!(rows[0], rows[1], "told apart: {rows:?}");
    for row in &rows {
        assert!(row.starts_with("Claude Code in tab 1, "), "{rows:?}");
    }
}

pub(super) struct TestHost(crate::harness_host::HostHandle);

impl Drop for TestHost {
    fn drop(&mut self) {
        self.0.shutdown_and_join();
    }
}

pub(super) fn desk(word: &str) -> (App, String, TestHost) {
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
        .set("autopilot", Some(word.into()));
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
    (app, ctx.self_id.as_str().to_string(), host)
}

pub(super) fn tell(app: &mut App, sid: &str, face: Option<TaskFace>) {
    app.apply_harness_fact(sid, crate::HarnessFact::Task(face));
}

fn driven(app: &App) -> Option<String> {
    app.presence_hover(0, None).and_then(|hover| {
        hover
            .split(crate::native_ui::FACT_SEPARATOR)
            .flat_map(|part| part.lines())
            .find(|part| part.starts_with("driven by aterm"))
            .map(str::to_string)
    })
}

fn pushed(_app: &App) -> String {
    let tips = crate::work_counts::last_tooltips_pushed(WindowId(0)).expect("a stamp");
    tips.first().cloned().flatten().unwrap_or_default()
}

#[test]
fn the_hover_names_the_chips_face() {
    let (mut app, sid, _host) = desk("land");
    assert_eq!(driven(&app).as_deref(), Some("driven by aterm (land)"));
    tell(&mut app, &sid, Some(TaskFace::Open));
    assert_eq!(driven(&app).as_deref(), Some("driven by aterm (land)"));
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert_eq!(
        driven(&app).as_deref(),
        Some("driven by aterm (land, landed)"),
        "no clock: the hover is read long after its push"
    );
    tell(&mut app, &sid, Some(TaskFace::Theirs));
    assert_eq!(
        driven(&app).as_deref(),
        Some("driven by aterm (land, quiet)")
    );

    let (mut cruise, _, _host) = desk("cruise");
    assert_eq!(driven(&cruise).as_deref(), Some("driven by aterm (cruise)"));
    meta(&mut cruise, "set autopilot cruise until=+2h");
    let timed = driven(&cruise).expect("driven");
    assert!(
        timed.starts_with("driven by aterm (cruise, lands ") && timed.ends_with(')'),
        "{timed}"
    );
}

fn meta(app: &mut App, rest: &str) {
    let session = app.pool.get(0).expect("session 0");
    let (term, ctx) = (session.term.clone(), session.ctx.clone());
    let (reply, changed) = crate::control::cmd_meta(&term, &app.store, 0, &ctx, rest);
    assert!(reply.starts_with("OK"), "{reply}");
    if changed {
        app.on_meta_changed(0);
    }
}

#[test]
fn the_hover_follows_the_face() {
    let (mut app, sid, _host) = desk("land");
    tell(&mut app, &sid, Some(TaskFace::Open));
    let _ = app.refresh_window_tabs(WindowId(0));
    assert!(
        pushed(&app).contains("driven by aterm (land)"),
        "{}",
        pushed(&app)
    );
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert!(!app.end_unseen(0), "PRECONDITION: under the floor, no end");
    assert!(
        pushed(&app).contains("driven by aterm (land, landed)"),
        "{}",
        pushed(&app)
    );
    tell(&mut app, &sid, Some(TaskFace::Theirs));
    assert!(
        pushed(&app).contains("driven by aterm (land, quiet)"),
        "{}",
        pushed(&app)
    );
}

#[test]
fn the_hover_follows_the_timer() {
    let (mut app, _, _host) = desk("cruise");
    let _ = app.refresh_window_tabs(WindowId(0));
    assert!(
        pushed(&app).contains("driven by aterm (cruise)"),
        "{}",
        pushed(&app)
    );
    meta(&mut app, "set autopilot cruise until=+2h");
    assert!(
        pushed(&app).contains("driven by aterm (cruise, lands "),
        "{}",
        pushed(&app)
    );
    meta(&mut app, "set autopilot cruise");
    assert!(
        pushed(&app).contains("driven by aterm (cruise)"),
        "{}",
        pushed(&app)
    );
}

#[test]
fn this_mac_counts_a_landed_tab_that_is_no_worker() {
    let (mut app, sid, _host) = desk("land");
    tell(&mut app, &sid, Some(TaskFace::Open));
    let _ = app.take_status_rebuild();
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert!(app.take_status_rebuild(), "the glance owes a rebuild");
    assert_eq!(app.status_glance().here.landed, 1);
    tell(&mut app, &sid, Some(TaskFace::Theirs));
    assert!(app.take_status_rebuild(), "the glance owes a rebuild");
    assert_eq!(app.status_glance().here.landed, 0);
}

#[test]
fn a_landing_under_cruise_is_not_counted_landed() {
    let (mut app, sid, _host) = desk("land");
    tell(&mut app, &sid, Some(TaskFace::Open));
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert_eq!(app.status_glance().here.landed, 1);
    assert_eq!(
        driven(&app).as_deref(),
        Some("driven by aterm (land, landed)")
    );
    meta(&mut app, "set autopilot cruise");
    assert_eq!(
        app.chip_glass(0).task,
        Some(TaskFace::Landed),
        "PRECONDITION: the glass keeps the landed face"
    );
    assert_eq!(app.status_glance().here.landed, 0, "the chip wears cruise");
    assert_eq!(driven(&app).as_deref(), Some("driven by aterm (cruise)"));
    meta(&mut app, "set autopilot land");
    assert_eq!(app.status_glance().here.landed, 1, "back under Land");
}

/// §14.6: This Mac counts a Land tab that holds as `landing`, the page's word for it, and the
/// same tab under Cruise, at rest, as `resting`.
#[test]
fn this_mac_counts_a_held_land_tab_landing_and_a_cruise_one_resting() {
    use aterm_agent::harness::acts::{HoldLeg, LandHeld};
    let (mut app, sid, _host) = desk("land");
    idle(&app);
    tell(&mut app, &sid, Some(TaskFace::Open));
    let here = |app: &App| {
        let g = app.status_glance();
        (g.here.landing, g.here.resting)
    };
    assert_eq!(here(&app), (0, 1), "no hold before Land's instruction");
    let held = LandHeld {
        since_ms: Some(1_792_159_320_000),
        nudges: 2,
        read: true,
        legs: HoldLeg::Repo.bit(),
        unpushed: 1,
        ..LandHeld::default()
    };
    if crate::harness_host::store_land_held(&app.store, &sid, Some(held)) {
        app.apply_harness_fact(&sid, crate::HarnessFact::LandHeld);
    }
    assert!(app.shown_hold(0).is_some(), "PRECONDITION: the hold stands");
    assert_eq!(here(&app), (1, 0), "a held Land is landing");
    meta(&mut app, "set autopilot cruise");
    assert_eq!(here(&app), (0, 1), "a Cruise tab at rest is resting");
    meta(&mut app, "set autopilot land");
    assert_eq!(here(&app), (1, 0), "back under Land, the hold kept");
}

/// §14.6: the Overview's row of a landed task says `landed` once, in its task words, and its
/// driver says `land`; the tab's hover, which has no task words, keeps `land, landed`.
#[test]
fn the_overview_says_landed_once() {
    let (mut app, sid, _host) = desk("land");
    idle(&app);
    tell(&mut app, &sid, Some(TaskFace::Open));
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert!(app.open_overview().is_ok());
    let row = |app: &App| {
        let lines = app.windows[&WindowId(0)]
            .overview()
            .expect("the Overview is open")
            .line_texts();
        lines
            .iter()
            .find(|l| l.contains("driven by aterm"))
            .cloned()
            .unwrap_or_else(|| panic!("the landed tab's row: {lines:#?}"))
    };
    let before = row(&app);
    assert!(
        before.ends_with("driven by aterm (land, landed)"),
        "CONTROL: no task words, so the driver says it: {before}"
    );
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    let mut mark = crate::task_record::TaskMark::new(TaskState::Done);
    mark.why = Some(crate::task_record::WHY_LANDED);
    mark.result_bytes = 128;
    crate::task_record::mark_session_task(&ctx, 0, &mark).expect("marked");
    app.overview_refresh_all();
    let after = row(&app);
    assert!(after.contains(" \u{b7} task landed "), "{after}");
    assert!(
        after.ends_with(" \u{b7} 128 B \u{b7} driven by aterm (land)"),
        "{after}"
    );
    assert_eq!(after.matches("landed").count(), 1, "said once: {after}");
    assert_eq!(
        driven(&app).as_deref(),
        Some("driven by aterm (land, landed)"),
        "the hover keeps both"
    );
}

pub(super) fn idle(app: &App) {
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    ctx.timeline.lock().expect("timeline").publish_agent(
        "idle",
        None,
        None,
        Some(aterm_phase::Program::Claude),
        crate::session_timeline::AgentStamp {
            generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
            fp: 1,
        },
    );
}

#[test]
fn a_timer_on_another_day_names_its_day() {
    use crate::claude_lights::{ChipFace, ChipGlass, Clock, LandAtGlass, Weekday};
    let glass = |other_day| ChipGlass {
        land_at: Some(LandAtGlass {
            clock: Clock::of_minute(18 * 60),
            other_day,
        }),
        ..ChipGlass::default()
    };
    let words = |other_day| {
        crate::app_presence::driven_words(
            ChipFace::CruiseUntil,
            &glass(other_day),
            None,
            false,
            None,
        )
    };
    assert_eq!(words(None).as_deref(), Some("cruise, lands at 18:00"));
    assert_eq!(
        words(Some(Weekday::Sat)).as_deref(),
        Some("cruise, lands Sat 18:00")
    );
}

const WALL_MS: i64 = 1_792_177_200_000;

#[test]
fn the_hover_follows_a_commits_rebuild() {
    crate::autopilot_timers::set_fake_clocks(Some((std::time::Instant::now(), WALL_MS)));
    let store = |app: &App| {
        let ctx = app.pool.get(0).expect("session 0").ctx.clone();
        assert!(
            crate::session_timeline::apply_autopilot(
                &ctx,
                Some("cruise".into()),
                Some(WALL_MS + 3_600_000),
                aterm_slog::Actor::Ctl,
                None,
            )
            .moved
        );
    };
    let (mut app, _, _host) = desk("cruise");
    app.handoff_successor = true;
    app.incoming_handoff_pending = true;
    store(&app);
    let _ = app.refresh_window_tabs(WindowId(0));
    assert!(
        pushed(&app).contains("driven by aterm (cruise)"),
        "{}",
        pushed(&app)
    );
    app.incoming_handoff_pending = false;
    app.settle_autopilot_commit();
    assert!(
        pushed(&app).contains("driven by aterm (cruise, lands "),
        "{}",
        pushed(&app)
    );
    let (mut app, _, _host) = desk("cruise");
    app.handoff_successor = true;
    app.incoming_handoff_pending = true;
    store(&app);
    app.index_autopilot_timer(0, Some(crate::autopilot_timers::ClosedOver::Aterm));
    app.incoming_handoff_pending = false;
    let _ = crate::work_counts::take_tooltip_pushes();
    app.settle_autopilot_commit();
    assert_eq!(
        crate::work_counts::take_tooltip_pushes(),
        1,
        "window 0's strip, stamped once"
    );
    crate::autopilot_timers::set_fake_clocks(None);
}

#[test]
fn the_hover_follows_a_fire_that_found_the_timer_moved() {
    let (mut app, _, _host) = desk("cruise");
    let mono = std::time::Instant::now();
    let land_at = WALL_MS + 120_000;
    crate::autopilot_timers::set_fake_clocks(Some((mono, WALL_MS)));
    assert!(app.set_session_autopilot_timed(
        0,
        Some(aterm_agent::supervise::Autopilot::Cruise),
        Some(land_at),
        crate::autopilot::Surface::Menu
    ));
    let before = pushed(&app);
    assert!(
        before.contains("driven by aterm (cruise, lands "),
        "{before}"
    );
    let later = land_at + 600_000;
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    assert!(
        crate::session_timeline::apply_autopilot(
            &ctx,
            Some("cruise".into()),
            Some(later),
            aterm_slog::Actor::Ctl,
            None,
        )
        .moved
    );
    crate::autopilot_timers::set_fake_clocks(Some((
        mono + std::time::Duration::from_secs(150),
        land_at,
    )));
    app.expire_autopilot_timers();
    assert_eq!(
        app.autopilot_timers.entry(0).map(|e| e.land_at_ms),
        Some(later),
        "PRECONDITION: indexed again at the new instant"
    );
    let now = driven(&app).expect("driven");
    assert_ne!(
        before.lines().find(|l| l.starts_with("driven by aterm")),
        Some(now.as_str()),
        "PRECONDITION: ten minutes later reads another time"
    );
    assert!(pushed(&app).contains(&now), "{}", pushed(&app));
    crate::autopilot_timers::set_fake_clocks(None);
}

#[test]
fn a_timed_write_stamps_each_strip_once() {
    let (mut app, _, _host) = desk("cruise");
    let _ = crate::work_counts::take_tooltip_pushes();
    meta(&mut app, "set autopilot land");
    let plain = crate::work_counts::take_tooltip_pushes();
    assert!(plain >= 1, "PRECONDITION: a meta move stamps the strip");
    meta(&mut app, "set autopilot cruise until=+2h");
    assert_eq!(crate::work_counts::take_tooltip_pushes(), plain, "the wire");
    assert!(
        pushed(&app).contains("driven by aterm (cruise, lands "),
        "{}",
        pushed(&app)
    );
    assert!(app.set_session_autopilot(
        0,
        Some(aterm_agent::supervise::Autopilot::Cruise),
        crate::autopilot::Surface::Menu
    ));
    let _ = crate::work_counts::take_tooltip_pushes();
    assert!(app.set_session_autopilot(
        0,
        Some(aterm_agent::supervise::Autopilot::Land),
        crate::autopilot::Surface::Menu
    ));
    let plain = crate::work_counts::take_tooltip_pushes();
    assert!(plain >= 1, "PRECONDITION: a window write stamps the strip");
    assert!(app.set_session_autopilot_timed(
        0,
        Some(aterm_agent::supervise::Autopilot::Cruise),
        Some(crate::autopilot_timers::wall_ms() + 3_600_000),
        crate::autopilot::Surface::Menu
    ));
    assert_eq!(crate::work_counts::take_tooltip_pushes(), plain, "the menu");
    assert!(
        pushed(&app).contains("driven by aterm (cruise, lands "),
        "{}",
        pushed(&app)
    );
}

/// The same economy with the strip on and a Settings tab ahead of the agent's tab: the restamp
/// counts tabs as the frame does, not as the all-terminal projection does.
#[test]
fn the_hover_asks_no_redraw_past_a_settings_tab_ahead_of_its_own() {
    let (mut app, sid, _host) = desk("land");
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let settings = app.windows[&wid]
        .tab_set
        .active_index()
        .expect("Settings is active");
    app.move_tab(wid, settings, 0);
    app.switch_tab_in(wid, 1);
    assert_eq!(app.focused_session_id(wid), Some(0));
    assert_eq!(
        (
            app.windows[&wid].tab_set.active_index(),
            app.windows[&wid].tabs.active
        ),
        (Some(1), 0),
        "PRECONDITION: the canonical and all-terminal indices differ"
    );
    app.windows.get_mut(&wid).expect("window").present = Some(crate::PresentTarget::Cpu {
        surface: crate::present::CpuSurface::Scripted(Default::default()),
    });
    tell(&mut app, &sid, Some(TaskFace::Open));
    app.redraw_window(wid);
    // Where the strip is aterm's own pixels (Linux, Windows) the chrome fonts install after the
    // first present, so it is not kept waiting, and ask the window holding the Settings page for
    // the frame that draws with them.
    app.redraw_window(wid);
    assert!(
        app.windows[&wid]
            .last_present
            .as_ref()
            .is_some_and(|key| key.tab_strip != 0),
        "PRECONDITION: a frame with the strip was presented"
    );
    let _ = crate::work_counts::take_hover_redraws();
    tell(&mut app, &sid, Some(TaskFace::Landed));
    let agent_tip = crate::work_counts::last_tooltips_pushed(wid)
        .and_then(|tips| tips.get(1).cloned().flatten())
        .unwrap_or_default();
    assert!(
        agent_tip.contains("driven by aterm (land, landed)"),
        "PRECONDITION: the agent tab's hover was stamped: {agent_tip}"
    );
    assert_eq!(crate::work_counts::take_hover_redraws(), 0);
}

#[test]
fn the_hover_asks_no_redraw_where_the_strip_did_not_move() {
    let (mut app, sid, _host) = desk("land");
    app.windows.get_mut(&WindowId(0)).expect("window").present = Some(crate::PresentTarget::Cpu {
        surface: crate::present::CpuSurface::Scripted(Default::default()),
    });
    tell(&mut app, &sid, Some(TaskFace::Open));
    app.redraw_window(WindowId(0));
    assert!(
        app.windows
            .get(&WindowId(0))
            .is_some_and(|ws| ws.last_present.is_some()),
        "PRECONDITION: a frame was presented"
    );
    let _ = crate::work_counts::take_hover_redraws();
    tell(&mut app, &sid, Some(TaskFace::Landed));
    assert!(
        pushed(&app).contains("driven by aterm (land, landed)"),
        "PRECONDITION: the hover was stamped: {}",
        pushed(&app)
    );
    assert_eq!(crate::work_counts::take_hover_redraws(), 0);
}
