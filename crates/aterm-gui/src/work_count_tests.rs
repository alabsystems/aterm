// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Count tests for the many-tab / many-session paths and the subscribe idle wake.

use super::*;
use crate::work_counts;
use std::time::{Duration, Instant};

/// The PTY reader's batch cadence under a flood — the rate the `Wake::Output` gates run at.
const BURST_DT: Duration = Duration::from_micros(333);

/// Window 0 split to `panes` panes, then `tabs - 1` more tabs appended, each split as it becomes active.
fn workspace(tabs: usize, panes: usize) -> App {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    for _ in 1..panes {
        app.split_active_stub_tab(wid);
    }
    for _ in 1..tabs {
        let sid = app.next_session_id;
        app.push_stub_tab(wid, stub_session(sid));
        for _ in 1..panes {
            app.split_active_stub_tab(wid);
        }
    }
    assert_eq!(app.windows[&wid].tab_set.len(), tabs, "fixture tab count");
    assert_eq!(
        app.pool.iter().count(),
        tabs * panes,
        "fixture session count"
    );
    app
}

fn set_grid(app: &mut App, rows: u16, cols: u16) {
    let ws = app.windows.get_mut(&WindowId(0)).expect("fixture window");
    ws.rows = rows;
    ws.cols = cols;
}

/// The live-drag tick (`resize_panes_scoped(wid, true)`) lays out the active tab only.
#[test]
fn a_live_drag_tick_lays_out_only_the_active_tab() {
    const TABS: usize = 8;
    let mut app = workspace(TABS, 4);
    let wid = WindowId(0);
    app.resize_panes_scoped(wid, false);
    let _ = work_counts::take_plans_built();

    set_grid(&mut app, 30, 100);
    app.resize_panes_scoped(wid, false);
    assert_eq!(
        work_counts::take_plans_built(),
        TABS,
        "a settle plans every tab"
    );

    set_grid(&mut app, 32, 120);
    for _ in 0..16 {
        app.resize_panes_scoped(wid, true);
    }
    assert!(
        app.windows[&wid].panes_stale,
        "background tabs are deferred"
    );
    assert_eq!(
        work_counts::take_plans_built(),
        16,
        "a live-drag tick must lay out only the active tab — a background tab's \
         discarded plan is the work MPT-2 removed"
    );
}

/// A successful present's latency walk touches the visible panes' stamps only.
#[test]
fn the_present_walk_touches_only_the_visible_panes_stamps() {
    const PANES: usize = 4;
    let mut app = workspace(30, PANES);
    let wid = WindowId(0);
    let arm = |app: &App| {
        for s in app.pool.iter() {
            s.last_output_ns
                .store(1, std::sync::atomic::Ordering::Relaxed);
        }
    };
    arm(&app);
    let _ = work_counts::take_latency_stamps_touched();
    let _ = app.present_latency_ns(wid);
    let first = work_counts::take_latency_stamps_touched();
    assert!(
        (PANES..=2 * PANES).contains(&first),
        "the first walk clears the newly revealed panes and books them: {first}"
    );
    let _ = work_counts::take_plans_built();
    for _ in 0..8 {
        arm(&app);
        let _ = app.present_latency_ns(wid);
    }
    assert_eq!(
        work_counts::take_latency_stamps_touched(),
        8 * PANES,
        "a steady present touches exactly the visible panes' stamps"
    );
    assert_eq!(
        work_counts::take_plans_built(),
        8,
        "a present lays out the active tab only"
    );
}

/// Inside the observation interval the status sweep probes no session.
#[test]
fn a_rate_limited_status_sweep_probes_no_session() {
    const SESSIONS: usize = 30;
    let mut app = workspace(1, SESSIONS);
    assert!(
        app.config.tab_status_or_default(),
        "tab_status must be ON, or the sweep is one early return and proves nothing"
    );
    let t0 = Instant::now();
    let _ = work_counts::take_status_probes();
    let _ = app.observe_session_statuses(t0);
    assert_eq!(
        work_counts::take_status_probes(),
        SESSIONS,
        "CONTROL: the first sweep probes every session"
    );
    let mut now = t0;
    for _ in 0..100 {
        now += BURST_DT;
        let _ = app.observe_session_statuses(now);
    }
    assert_eq!(
        work_counts::take_status_probes(),
        0,
        "100 flood wakes inside the interval must probe nothing"
    );
    let _ = app.observe_session_statuses(t0 + Duration::from_secs(2));
    assert_eq!(
        work_counts::take_status_probes(),
        SESSIONS,
        "CONTROL: past the interval the sweep probes again"
    );
}

/// In the steady state the title-drift gate is decided by the labeling window's epoch cache and walks no tab.
#[test]
fn a_steady_title_gate_walks_no_tab() {
    const TABS: usize = 30;
    let mut app = workspace(TABS, 1);
    let wid = WindowId(0);
    let active = app.focused_session_id(wid).expect("an active session");
    let mut now = Instant::now();
    app.observe_title_drift(active, now);
    for _ in 0..16 {
        now += BURST_DT;
        app.observe_title_drift(active, now);
    }
    let _ = work_counts::take_title_tab_touches();
    for _ in 0..100 {
        now += BURST_DT;
        app.observe_title_drift(active, now);
    }
    assert_eq!(
        work_counts::take_title_tab_touches(),
        0,
        "a steady flood must not walk the tab list"
    );

    app.windows
        .get_mut(&wid)
        .expect("fixture window")
        .tab_title_epochs
        .remove(&active);
    now += Duration::from_millis(500);
    app.observe_title_drift(active, now);
    assert_eq!(
        work_counts::take_title_tab_touches(),
        TABS,
        "the uncached window walks to the active (last) tab"
    );
}

/// Exercise the accepted OSC path with every cache warm, rather than measuring only
/// the epoch gate. The counters cover drift walks, identity reads, title reads, retain membership
/// calls and chrome-cache writes; they do not claim CPU time or total copied bytes.
#[test]
fn complete_title_refresh_has_bounded_cache_maintenance() {
    for tabs in [1, 30, 200] {
        let mut app = workspace(tabs, 1);
        let wid = WindowId(0);
        let sessions: Vec<_> = app.pool.iter().map(|s| s.id).collect();
        let t0 = Instant::now();
        for &sid in &sessions {
            term_lock(&app.pool.get(sid).unwrap().term)
                .process(format!("\x1b]2;label-{sid}\x07").as_bytes());
            app.observe_title_drift(sid, t0);
        }
        let active = app.focused_session_id(wid).unwrap();
        let ids: Vec<_> = app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .map(|t| t.id)
            .collect();
        let ws = &app.windows[&wid];
        assert_eq!(ws.tab_title_cache.len(), tabs);
        assert_eq!(ws.tab_title_epochs.len(), tabs);
        assert_eq!(ws.tab_chrome_titles.len(), tabs);
        assert_eq!(ws.tab_chrome_titles_by_tab.len(), tabs);
        let _ = work_counts::take_title_refresh();
        term_lock(&app.pool.get(active).unwrap().term).process(b"\x1b]2;changed-label\x07");
        let later = t0 + Duration::from_secs(1);
        app.observe_title_drift(active, later);
        let work = work_counts::take_title_refresh();
        eprintln!("complete-title tabs={tabs} changed={work:?}");
        assert!(work.drift_tabs > 0 && work.drift_tabs <= 3 * tabs);
        assert!(work.drift_cache_probes > 0 && work.drift_cache_probes <= 4 * tabs);
        assert!(work.eligibility_reads > 0 && work.eligibility_reads <= tabs);
        assert!(work.identities > 0 && work.identities <= tabs);
        assert!(work.reads > 0 && work.reads <= tabs);
        assert!(work.cache_probes > 0 && work.cache_probes <= 4 * tabs);
        assert!(work.cache_writes > 0 && work.cache_writes <= 2 * tabs);
        let ws = &app.windows[&wid];
        assert_eq!(ws.tab_title_cache[&active].title, "changed-label");
        for (id, tab) in ids.iter().zip(ws.tab_set.tabs()) {
            assert_eq!(*id, tab.id, "refresh preserves ordering and identity");
            let sid = app
                .view_store
                .get(tab.focus)
                .copied()
                .unwrap()
                .terminal_session()
                .unwrap();
            let expected = if sid == active {
                "changed-label".to_owned()
            } else {
                format!("label-{sid}")
            };
            assert_eq!(ws.tab_chrome_titles[&sid], expected);
            assert_eq!(ws.tab_chrome_titles_by_tab[id].0, Some(sid));
            assert_eq!(ws.tab_chrome_titles_by_tab[id].1, expected);
            assert_eq!(
                ws.tab_title_epochs[&sid],
                term_lock(&app.pool.get(sid).unwrap().term).title_epoch()
            );
        }
        assert_eq!(
            work.chrome_bytes,
            2 * ws
                .tab_chrome_titles
                .values()
                .map(String::len)
                .sum::<usize>()
        );
        for tick in 1..=100 {
            term_lock(&app.pool.get(active).unwrap().term).process(b"\x1b]2;changed-label\x07");
            app.observe_title_drift(active, later + Duration::from_millis(tick));
        }
        let unchanged = work_counts::take_title_refresh();
        eprintln!("complete-title tabs={tabs} unchanged-flood={unchanged:?}");
        assert_eq!(unchanged, work_counts::TitleRefreshWork::default());
        assert!(!app.title_drift.pending.contains(&active));
    }
}

#[test]
fn complete_title_refresh_preserves_shared_and_detached_identity() {
    let mut app = workspace(3, 1);
    let original = WindowId(0);
    let sid = app.focused_session_id(original).unwrap();
    term_lock(&app.pool.get(sid).unwrap().term).process(b"\x1b]2;before-detach\x07");
    app.observe_title_drift(sid, Instant::now());
    let tab = app.windows[&original].tab_set.active_id().unwrap();
    let detached = app.detach_active_tab_logical().unwrap();
    let shared = app.open_active_session_in_new_window_logical().unwrap();
    for wid in [original, detached, shared] {
        app.refresh_window_tabs(wid);
    }
    let unrelated = app.windows[&original].tab_chrome_titles_by_tab.clone();
    let _ = work_counts::take_title_refresh();
    term_lock(&app.pool.get(sid).unwrap().term).process(b"\x1b]2;after-detach\x07");
    app.observe_title_drift(sid, Instant::now() + Duration::from_secs(1));
    let work = work_counts::take_title_refresh();
    eprintln!("complete-title shared-detached={work:?}");
    assert_eq!(work.reads, 2, "only the two labeling windows refresh");
    assert_eq!(app.windows[&original].tab_chrome_titles_by_tab, unrelated);
    assert!(!app.windows[&original].tab_title_cache.contains_key(&sid));
    assert!(
        !app.windows[&original]
            .tab_chrome_titles_by_tab
            .contains_key(&tab)
    );
    for wid in [detached, shared] {
        assert_eq!(app.windows[&wid].tab_chrome_titles[&sid], "after-detach");
    }
    app.pool
        .get(sid)
        .unwrap()
        .ctx
        .meta
        .lock()
        .unwrap()
        .set("title", Some("pinned-label".into()));
    app.refresh_meta_dependent_chrome(sid);
    for wid in [detached, shared] {
        assert_eq!(app.windows[&wid].tab_chrome_titles[&sid], "pinned-label");
    }
    app.close_window_logical(shared);
    app.detach_rollback_logical(Some(original), detached);
    app.refresh_window_tabs(original);
    assert_eq!(
        app.windows[&original].tab_chrome_titles_by_tab[&tab].0,
        Some(sid)
    );
    assert_eq!(
        app.windows[&original].tab_chrome_titles_by_tab[&tab].1,
        "pinned-label"
    );
    assert!(app.structural_invariants_ok());
}

#[test]
fn complete_title_refresh_keeps_parked_and_native_fallbacks() {
    let mut app = workspace(2, 1);
    let wid = WindowId(0);
    let hidden = app.focused_session_id(wid).unwrap();
    term_lock(&app.pool.get(hidden).unwrap().term).process(b"\x1b]2;hidden-terminal\x07");
    app.refresh_window_tabs(wid);
    let instance = app
        .native_runtime
        .insert_instance(crate::native_app::NativeApp::Allocation(Box::default()))
        .unwrap();
    let native = app
        .split_active_with_native(
            wid,
            crate::tab_model::SplitAxis::Horizontal,
            instance,
            crate::native_app::AppViewState::Allocation(Box::default()),
        )
        .unwrap();
    let parked = crate::app_workers_tests::land_worker(&mut app);
    term_lock(&app.pool.get(parked).unwrap().term).process(b"\x1b]2;parked-label\x07");
    app.refresh_window_tabs(wid);
    assert!(!app.windows[&wid].tab_title_cache.contains_key(&hidden));
    assert!(!app.windows[&wid].tab_chrome_titles.contains_key(&hidden));
    let native_title = app.windows[&wid]
        .tab_set
        .tabs()
        .iter()
        .find(|t| t.focus == native)
        .map(|t| app.windows[&wid].tab_chrome_titles_by_tab[&t.id].clone())
        .unwrap();
    assert_eq!(native_title.0, None);
    app.observe_title_drift(parked, Instant::now() + Duration::from_secs(1));
    let ws = &app.windows[&wid];
    assert_eq!(ws.tab_chrome_titles[&parked], "parked-label");
    assert_eq!(ws.tooltips_pushed.len(), ws.tab_set.shown_len());
    assert!(ws.tab_set.tabs().len() > ws.tab_set.shown_len());
    assert!(
        ws.tab_chrome_titles_by_tab
            .values()
            .any(|v| v == &native_title)
    );
    assert!(app.structural_invariants_ok());
}

/// An idle `events` digest wake with saturated ledgers touches no retained record.
#[test]
fn an_idle_events_wake_touches_no_retained_record() {
    use crate::subscribe::digest_fixture::DigestFixture;
    const TARGETS: usize = 4;
    let floor = aterm_slog::row_floor(crate::session_log::ring_limits()).records;
    let mut f = DigestFixture::build(TARGETS, 2 * 1000, 2 * crate::turn_ledger::LEDGER_CAP, {
        aterm_slog::MAX_RECORDS + floor
    });
    let (blocks, turns, rows, lost) = f.retained();
    assert!(
        blocks >= 1000 && turns == crate::turn_ledger::LEDGER_CAP && rows >= floor && lost > 0,
        "the fixture must saturate every ledger and the view: {blocks}/{turns}/{rows}/{lost}"
    );
    let _ = work_counts::take_retained_records_touched();
    for woke in [false, true, false, false] {
        assert_eq!(f.wake(woke), 0, "an idle wake emits nothing");
    }
    assert_eq!(
        work_counts::take_retained_records_touched(),
        0,
        "an idle wake must not touch the retained ledgers"
    );

    f.land_turn();
    assert!(f.wake(true) > 0, "a wake after a real append emits");
    let touched = work_counts::take_retained_records_touched();
    let per_target_bound = 2 + crate::turn_ledger::LEDGER_CAP.ilog2() as usize + 1;
    assert!(
        (TARGETS..=TARGETS * per_target_bound).contains(&touched),
        "a wake with one new turn per target touched {touched} records (bound \
         {per_target_bound} per target)"
    );

    f.land_row();
    assert!(f.wake(true) > 0, "a wake after a new row emits");
    let touched = work_counts::take_retained_records_touched();
    assert!(
        (TARGETS..=TARGETS * per_target_bound).contains(&touched),
        "a wake with one new row per target touched {touched} records"
    );
}

/// A headless window drawn through the real redraw path on a scripted CPU surface.
#[cfg(debug_assertions)]
fn presented_window() -> (App, WindowId, u64) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    app.windows.get_mut(&wid).expect("fixture window").present = Some(crate::PresentTarget::Cpu {
        surface: crate::present::CpuSurface::Scripted(Default::default()),
    });
    let ws = &app.windows[&wid];
    assert!(
        matches!(&ws.present, Some(crate::PresentTarget::Cpu { surface }) if surface.scripted().is_some()),
        "a scripted surface"
    );
    (app, wid, session)
}

/// Successful presents on `wid`'s scripted surface so far.
#[cfg(debug_assertions)]
fn presents(app: &App, wid: WindowId) -> usize {
    match &app.windows[&wid].present {
        Some(crate::PresentTarget::Cpu { surface }) => {
            surface.scripted().map_or(0, |s| s.commits())
        }
        _ => 0,
    }
}

/// Cell geometries the renderer derived over `frames` presented frames of `wid`.
#[cfg(debug_assertions)]
fn derivations_over(
    app: &mut App,
    wid: WindowId,
    frames: usize,
    mut before: impl FnMut(&mut App),
) -> usize {
    let _ = work_counts::take_cell_geometry_derivations();
    for _ in 0..frames {
        before(app);
        app.redraw_window(wid);
    }
    work_counts::take_cell_geometry_derivations()
}

/// The cell box is derived once per change, never per frame.
#[cfg(debug_assertions)]
#[test]
fn a_frame_derives_the_cell_box_only_when_an_input_changed() {
    const FRAMES: usize = 20;
    let (mut app, wid, session) = presented_window();
    let committed = presents(&app, wid);
    let _ = derivations_over(&mut app, wid, 2, |_| {});
    assert!(
        presents(&app, wid) > committed,
        "the frames reach the glass"
    );
    let rest = derivations_over(&mut app, wid, FRAMES, |_| {});
    assert_eq!(rest, 0, "frames at rest derive no cell box");
    let term = app.pool.get(session).expect("session").term.clone();
    let keys = derivations_over(&mut app, wid, FRAMES, |_| {
        crate::term_lock(&term).process(b"k");
    });
    assert_eq!(keys, 0, "keystroke frames derive no cell box");

    let px = app.windows[&wid].metrics.font_px;
    let before = app.win_cell_size(wid);
    app.windows
        .get_mut(&wid)
        .expect("fixture window")
        .metrics
        .font_px = px * 2.0;
    let _ = work_counts::take_cell_geometry_derivations();
    app.redraw_window(wid);
    assert_eq!(
        work_counts::take_cell_geometry_derivations(),
        1,
        "CONTROL: a new scale derives once"
    );
    let doubled = app.win_cell_size(wid);
    assert!(
        doubled.0.abs_diff(2 * before.0) <= 1 && doubled.1.abs_diff(2 * before.1) <= 1,
        "the new scale's cell box ({before:?} -> {doubled:?})"
    );
    assert_eq!(
        derivations_over(&mut app, wid, FRAMES, |_| {}),
        0,
        "kept at 2x"
    );
    app.windows
        .get_mut(&wid)
        .expect("fixture window")
        .metrics
        .font_px = px;
    assert_eq!(
        derivations_over(&mut app, wid, FRAMES, |_| {}),
        0,
        "kept at 1x"
    );
    assert_eq!(app.win_cell_size(wid), before, "the first display's box");

    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-cell-box-reload")
        .tempdir()
        .expect("scratch dir");
    let dir = scratch.path();
    let geometry = app.backend.cell_geometry(px);
    let _ = work_counts::take_cell_geometry_derivations();
    apply_config_text(&mut app, dir, "adjust_baseline = 3\n");
    app.redraw_window(wid);
    assert_eq!(
        work_counts::take_cell_geometry_derivations(),
        1,
        "CONTROL: a reloaded baseline derives once"
    );
    assert_eq!(
        app.backend.cell_geometry(px),
        (geometry.0, geometry.1, geometry.2 + 3),
        "the same box, its baseline 3 px down"
    );
    assert_eq!(
        derivations_over(&mut app, wid, FRAMES, |_| {}),
        0,
        "kept after the baseline's reload"
    );

    // The reload re-grids every window inside the size it holds, a headless one too, so the
    // one derivation may fall in the reload or in the frame after it.
    let _ = work_counts::take_cell_geometry_derivations();
    apply_config_text(&mut app, dir, "line_height = 1.5\n");
    app.redraw_window(wid);
    assert_eq!(
        work_counts::take_cell_geometry_derivations(),
        1,
        "CONTROL: a reloaded line height derives once"
    );
    let taller = app.win_cell_size(wid);
    assert_eq!(taller.0, before.0, "the same advance");
    assert!(
        taller.1 > before.1,
        "a taller cell ({before:?} -> {taller:?})"
    );
    assert_eq!(app.cell_size(), taller, "the renderer's own box at this px");
    assert_eq!(
        derivations_over(&mut app, wid, FRAMES, |_| {}),
        0,
        "kept after the reload"
    );

    let _ = work_counts::take_cell_geometry_derivations();
    app.set_font_size(app.font_size + 4.0);
    app.redraw_window(wid);
    assert_eq!(
        work_counts::take_cell_geometry_derivations(),
        2,
        "CONTROL: a zoom derives once on each renderer"
    );
    let zoomed = app.win_cell_size(wid);
    assert!(
        (app.windows[&wid].metrics.font_px - (px + 4.0)).abs() < 0.5,
        "the window took the zoom"
    );
    assert_eq!(app.cell_size(), zoomed, "the zoomed renderer's own box");
    assert!(
        zoomed.1 > taller.1,
        "a bigger cell ({taller:?} -> {zoomed:?})"
    );
    assert_eq!(
        derivations_over(&mut app, wid, FRAMES, |_| {}),
        0,
        "kept after the zoom"
    );
}

/// Apply `text` as `aterm.toml` through the real admission path.
#[cfg(debug_assertions)]
fn apply_config_text(app: &mut App, dir: &std::path::Path, text: &str) {
    let path = dir.join("aterm.toml");
    crate::test_scratch::write_config(&path, text);
    let observation =
        crate::native_config_service::VersionedConfigService::observe_path(&path, false)
            .expect("observe");
    let config: Config = aterm_toml::from_str(text).expect("valid toml");
    let assets = config.resolve_asset_catalog();
    let path_feed_fps = config.path_feed_fingerprints();
    let sparkle = config.prepare_sparkle_runtime();
    app.apply_prepared_config_generation(crate::native_font_catalog::PreparedConfigGeneration {
        observation,
        config,
        values: std::collections::BTreeMap::new(),
        disk_faults: std::sync::Arc::default(),
        assets,
        path_feed_fps,
        sparkle,
        fonts: None,
        warnings: Vec::new(),
        font_refusals: Vec::new(),
        reason: crate::app_colorscheme::ThemeChange::ConfigReload,
    });
}
