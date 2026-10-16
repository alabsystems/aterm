// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! COUNT tests for the many-tab / many-session wins MPT-2..5 and the subscribe
//! idle wake (docs/PERF-REGRESSION-DEFENCE.md, W-4 and "If you are the next
//! person here", item 1).
//!
//! A pass that goes back to doing the work reaches the same ANSWER, only
//! slower, so outcome-parity tests cannot catch the giveback. Each test here
//! reads a `crate::work_counts` counter placed at the site that does the
//! removed WORK (N tabs x M panes staged through `push_stub_tab` +
//! `split_active_stub_tab`; the digest through `DigestFixture`). Each also
//! shows its counter moving on a control, so a zero is never a counter nobody
//! increments.

use super::*;
use crate::work_counts;
use std::time::{Duration, Instant};

/// The PTY reader's batch cadence under a flood — the rate the `Wake::Output`
/// gates run at.
const BURST_DT: Duration = Duration::from_micros(333);

/// Window 0 split to `panes` panes,
/// then `tabs - 1` more tabs appended, each split as it becomes active. Leaves
/// the LAST tab active.
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

/// MPT-2: the live-drag tick (`resize_panes_scoped(wid, true)`, run by
/// `redraw_window` on EVERY presented frame while `panes_stale` stands) lays
/// out the ACTIVE tab only. Before MPT-2 it built every tab's plan and threw
/// the background ones away: 8 plans per frame here, 30 at the finding's shape.
#[test]
fn a_live_drag_tick_lays_out_only_the_active_tab() {
    const TABS: usize = 8;
    let mut app = workspace(TABS, 4);
    let wid = WindowId(0);
    app.resize_panes_scoped(wid, false);
    let _ = work_counts::take_plans_built();

    // CONTROL: an AllTabs settle lays out every tab — the counter counts.
    set_grid(&mut app, 30, 100);
    app.resize_panes_scoped(wid, false);
    assert_eq!(
        work_counts::take_plans_built(),
        TABS,
        "a settle plans every tab"
    );

    // THE DRAG: a new grid, active-only ticks. One plan per tick.
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

/// MPT-3: a successful present's latency walk touches the VISIBLE panes' stamps
/// only — O(active tab), never O(tabs x panes). Before MPT-3 every present
/// walked every leaf of every tab to discard hidden stamps: ~120 read-modify-
/// writes at 30 x 4, against the 4 here.
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
    // The retired sweep walked every TAB's leaves, which needs every tab's
    // plan: one plan per present is the active tab's and nothing else.
    assert_eq!(
        work_counts::take_plans_built(),
        8,
        "a present lays out the active tab only"
    );
}

/// MPT-4: inside the observation interval the status sweep probes NO session
/// — the O(1) deadline gate answers before the pool fold. Before MPT-4 every
/// output wake folded the whole pool (30 probes per wake here, thousands of
/// wakes a second under a flood) to usually find nothing due.
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

/// MPT-5 (the half that landed): in the steady state the title-drift gate is
/// decided by the labeling window's epoch cache and walks NO tab. Before the
/// conjuncts were reordered it walked the window's whole tab list on every
/// output burst: 30 tab touches per wake here.
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

    // CONTROL: a window whose cache does not cover the session (it started
    // labeling it after the last flush) walks the list to find out — the part
    // the reverse index MPT-5 asks for would remove, and proof the counter runs.
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

/// The subscribe IDLE WAKE: an `events` digest whose ledgers are saturated
/// (1000 shell blocks, 512 turn records, 512 timeline events per target)
/// touches NO retained record on a wake where nothing happened. Before
/// `458f0fa28` every wake — the 4 Hz liveness ticks included — walked all
/// three ledgers linearly: ~2,000 records per target per wake.
#[test]
fn an_idle_events_wake_touches_no_retained_record() {
    use crate::subscribe::digest_fixture::DigestFixture;
    const TARGETS: usize = 4;
    let mut f = DigestFixture::build(TARGETS, 2 * 1000, 2 * crate::turn_ledger::LEDGER_CAP, {
        2 * crate::session_timeline::TIMELINE_CAP
    });
    let (blocks, turns, timeline) = f.retained();
    assert!(
        blocks >= 1000
            && turns == crate::turn_ledger::LEDGER_CAP
            && timeline == crate::session_timeline::TIMELINE_CAP,
        "the fixture must saturate every ledger: {blocks}/{turns}/{timeline}"
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

    // CONTROL: one real turn per target — the wake emits, and reaches the
    // ledger by a SEEK (O(log n + new)), never a walk.
    f.land_turn();
    assert!(f.wake(true) > 0, "a wake after a real append emits");
    let touched = work_counts::take_retained_records_touched();
    let per_target_bound = 2 + crate::turn_ledger::LEDGER_CAP.ilog2() as usize + 1;
    assert!(
        (TARGETS..=TARGETS * per_target_bound).contains(&touched),
        "a wake with one new turn per target touched {touched} records (bound \
         {per_target_bound} per target)"
    );
}

/// A headless window drawn through the real redraw path: a scripted CPU
/// surface takes the present (`crate::present::scripted`), so every frame
/// below is `App::redraw_window` end to end, glass included.
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

/// Cell geometries the renderer derived over `frames` presented frames of
/// `wid`, each after `before` runs (a key's echo, or nothing).
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

/// THE CELL BOX IS DERIVED ONCE PER CHANGE, NEVER PER FRAME: the window's
/// cell size (`App::win_cell_size`, asked by the frame's layout, the cursor,
/// the band, the cell-pixel sync and the composer's caret — 4 times a frame
/// at rest and 6 a keystroke on this window, measured 2026-10-16) is read
/// from what the renderer kept for the window's px, so a frame at rest and a
/// keystroke's frame derive nothing. Each derivation reads the face's
/// metrics, and the macOS default primary (SF Mono, instantiated) parses
/// the face for them: ~0.2 µs a call.
///
/// Every real input of it derives exactly once and is then kept: a window
/// whose scale changed (a move to another display, `on_scale_factor_changed`
/// re-resolving its `MetricsView`), a config reload of the baseline (pushed
/// onto the live renderer, which forgets), one that moves the cell box
/// (`line_height`, a rebuild), and a font zoom (a new renderer at the new
/// px). Each reads the new geometry (it is what the face derives now), never
/// a kept one.
///
/// NEGATIVE CONTROLS: each change's first frame derives (the counter counts
/// the frame's asks), and its answer is the new geometry. Without the kept
/// answers the rest frames read 4.2 a frame, and without the baseline's
/// forget the reloaded baseline reads the kept one (both mutation-checked).
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

    // A SCALE CHANGE: the window's metric record re-resolved at 2x (what
    // `on_scale_factor_changed` does for an attached window). Its first frame
    // derives the new px once; the frames after keep it.
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
    // Back on the first display: still kept, nothing derived.
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

    // A CONFIG RELOAD of the baseline escape hatch (`adjust_baseline`),
    // through the real admission path: a live push onto the renderer
    // (`apply_render_knob`), no rebuild, so the renderer itself forgets what
    // it kept. The reload and its first frame derive once between them.
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

    // A CONFIG RELOAD that moves the cell box (`line_height`), through the
    // real admission path: a rebuild, whose new renderer has kept nothing.
    apply_config_text(&mut app, dir, "line_height = 1.5\n");
    let _ = work_counts::take_cell_geometry_derivations();
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

    // A FONT ZOOM: the zoom's own transaction asks the outgoing renderer at
    // the new px (`refresh_all_window_metrics` syncing the cell pixel size)
    // before the rebuild replaces it, and the new renderer, which has kept
    // nothing, derives on its first frame: once per renderer, then kept.
    let _ = work_counts::take_cell_geometry_derivations();
    app.set_font_px(px + 4.0);
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

/// Apply `text` as `aterm.toml` through the real admission path (the
/// prepared generation a config watcher hands the App).
#[cfg(debug_assertions)]
fn apply_config_text(app: &mut App, dir: &std::path::Path, text: &str) {
    let path = dir.join("aterm.toml");
    std::fs::write(&path, text).expect("write config");
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
    });
}
