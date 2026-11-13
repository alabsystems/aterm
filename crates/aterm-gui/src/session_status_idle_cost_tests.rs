// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::{Duration, Instant};

const CHILDLESS_PID: i32 = 0x7FFF_FFF0;

fn stage_pty_like_sessions(app: &mut crate::App, count: u64) {
    for i in 0..count {
        let mut session = crate::stub_session(1000 + i);
        session.master = 0x4000_0000 + i32::try_from(i).expect("small fixture");
        session.pid = CHILDLESS_PID;
        app.pool.insert(session);
    }
}

fn idle_captures(pty_sessions: u64, secs: u64) -> u64 {
    let mut app = crate::App::headless_for_test();
    stage_pty_like_sessions(&mut app, pty_sessions);
    let t0 = Instant::now();
    let turns = secs * 4;
    for turn in 0..turns {
        let now = t0 + Duration::from_millis(250 * turn);
        let _ = app.observe_session_statuses(now);
    }
    app.job_probe.capture_count()
}

#[test]
fn an_idle_windows_job_probe_cost_does_not_scale_with_tab_count() {
    const SECS: u64 = 30;
    let ceiling_refreshes = SECS / crate::quit_safety::JOB_PROBE_MAX_AGE.as_secs() + 1;

    let one_tab = idle_captures(1, SECS);
    let eight_tabs = idle_captures(8, SECS);
    println!(
        "IDLE JOB PROBES over {SECS}s: 1 tab = {one_tab} captures, \
         8 tabs = {eight_tabs} captures (ceiling allows {ceiling_refreshes})"
    );

    assert_eq!(
        eight_tabs, one_tab,
        "an idle window's process-table captures must not scale with tab \
         count (8 tabs bought {eight_tabs}, 1 tab bought {one_tab})"
    );
    assert!(
        eight_tabs <= ceiling_refreshes,
        "{SECS}s of idle must buy at most one capture per \
         JOB_PROBE_MAX_AGE ({ceiling_refreshes}), bought {eight_tabs}"
    );
    assert!(
        eight_tabs >= 1,
        "the fixture must actually reach the probe — a staged session whose \
         master/pid never arm `foreground_job` would price an early return"
    );
}

#[test]
fn a_session_whose_grid_moves_buys_a_fresh_capture() {
    let mut app = crate::App::headless_for_test();
    stage_pty_like_sessions(&mut app, 3);
    let t0 = Instant::now();
    let _ = app.observe_session_statuses(t0);
    let settled = app.job_probe.capture_count();
    assert!(settled >= 1, "the first sweep captures");

    let t1 = t0 + Duration::from_millis(500);
    let _ = app.observe_session_statuses(t1);
    assert_eq!(
        app.job_probe.capture_count(),
        settled,
        "a settled sweep must buy nothing"
    );

    let term = app.pool.get(1000).expect("staged session").term.clone();
    term.lock()
        .unwrap_or_else(|p| p.into_inner())
        .process(b"$ cargo build\r\n");
    let t2 = t1 + Duration::from_millis(500);
    let _ = app.observe_session_statuses(t2);
    assert_eq!(
        app.job_probe.capture_count(),
        settled + 1,
        "moved evidence must force exactly one fresh capture"
    );
}
