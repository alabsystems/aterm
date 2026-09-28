// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! COUNT tests for the many-tab / many-session wins MPT-2..5 and the subscribe
//! idle wake (docs/PERF-REGRESSION-DEFENCE.md, W-4 and "If you are the next
//! person here", item 1).
//!
//! Until 2026-09-25 these wins were proved only inside the criterion benches
//! (`benches/workspace_scaling.rs`, `benches/subscribe_digest.rs`) and by
//! outcome-parity unit tests that carry no count, so nothing in the merge
//! contract's test stage failed on their giveback: a pass that went back to
//! doing the work reaches the same ANSWER, only slower. Each test here reads a
//! `crate::work_counts` counter placed at the site that does the removed WORK,
//! with the fixture shapes lifted from those benches (N tabs x M panes staged
//! through `push_stub_tab` + `split_active_stub_tab`, exactly as
//! `BenchApp::stage_workspace` does; the digest through the bench's own
//! `DigestFixture`). Each also shows its counter moving on a control, so a zero
//! is never a counter nobody increments.

use super::*;
use crate::work_counts;
use std::time::{Duration, Instant};

/// The PTY reader's batch cadence under a flood — the rate the `Wake::Output`
/// gates run at (`workspace_scaling.rs`' `BURST_DT`).
const BURST_DT: Duration = Duration::from_micros(333);

/// `BenchApp::stage_workspace(tabs, panes)`: window 0 split to `panes` panes,
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
    use crate::subscribe::bench_seam::DigestFixture;
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
