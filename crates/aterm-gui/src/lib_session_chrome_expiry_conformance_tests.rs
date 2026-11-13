// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{App, WindowId};
use crate::session_chrome::{
    CACHE_MAX_AGE_MS, EXPIRY_WINDOW_SCAN_BUDGET, TabMenuEntry, menu_shown_at, next_cache_expiry_ms,
};

fn clock_moved(app: &mut App) -> u64 {
    let expiry = crate::turn_ledger::now_ms() + CACHE_MAX_AGE_MS;
    app.session_chrome
        .get_mut(&0)
        .expect("warmed cache")
        .expires_ms = Some(expiry);
    expiry
}

#[test]
fn quiet_expiry_recomposes_live_tooltip_at_the_exact_deadline() {
    let mut app = App::headless_for_test();
    app.refresh_window_tabs(WindowId(0));
    let expiry = clock_moved(&mut app);
    let cached = app.session_chrome.get_mut(&0).expect("warmed cache");
    cached.ext.tooltip = Some("stale-age-sentinel".to_string());
    app.windows
        .get_mut(&WindowId(0))
        .expect("window")
        .tab_set
        .tab_at_mut(0)
        .expect("tab")
        .presentation
        .tooltip = Some("stale-age-sentinel".to_string());

    assert_eq!(
        app.advance_session_chrome_expiry(expiry - 1),
        crate::session_chrome::ExpiryProgress::default()
    );
    let progress = app.advance_session_chrome_expiry(expiry);
    assert_eq!(progress.admitted_session, Some(0));
    assert_eq!(progress.window_scans, 1);
    assert_eq!(progress.window_refreshes, 1);
    assert_eq!(progress.completed_session, Some(0));
    let refreshed = app.session_chrome.get(&0).expect("recomposed cache");
    assert_ne!(
        refreshed.ext.tooltip.as_deref(),
        Some("stale-age-sentinel"),
        "the due entry was removed before the window recomposed it"
    );
    assert_eq!(
        app.windows[&WindowId(0)].tab_set.tabs()[0]
            .presentation
            .tooltip,
        refreshed.ext.tooltip,
        "the live tab receives the fresh composition in the expiry turn"
    );
    assert_eq!(
        next_cache_expiry_ms(&app.session_chrome),
        None,
        "recomposed with nothing the clock moves (no upgrade): no further wake"
    );
}

#[test]
fn a_quiet_desktops_tabs_arm_no_wake_and_their_menus_age_as_shown() {
    use std::time::{Duration, Instant};
    let mut app = App::headless_for_test();
    app.windows
        .get_mut(&WindowId(0))
        .expect("window")
        .window_on_screen_for_test = true;
    app.refresh_window_tabs(WindowId(0));
    let (now, now_ms) = (Instant::now(), crate::turn_ledger::now_ms());
    assert_eq!(app.session_chrome[&0].expires_ms, None, "nothing to move");
    assert_eq!(app.session_chrome_wake(now, now_ms), None, "no wake");
    let menu = app.session_chrome[&0].ext.menu.clone();
    let started = menu
        .iter()
        .find_map(|e| match e {
            TabMenuEntry::Event { words, at_ms } if words == "started" => Some(*at_ms),
            _ => None,
        })
        .expect("a fresh tab's `started` row, with its stamp");
    let shown = |at: u64| {
        menu_shown_at(&menu, at)
            .into_iter()
            .find_map(|e| match e {
                TabMenuEntry::Header(h) if h.starts_with("started ") => Some(h),
                _ => None,
            })
            .expect("the row, aged")
    };
    assert_eq!(shown(started + 2_000), "started \u{b7} just now");
    assert_eq!(shown(started + 5 * 60_000), "started \u{b7} 5m ago");
    let card = app.tab_context_menu_model(WindowId(0), 0);
    assert!(
        card.iter()
            .any(|e| matches!(e, TabMenuEntry::Header(h) if h.starts_with("started \u{b7} "))),
        "{card:?}"
    );
    assert!(!card.iter().any(|e| matches!(e, TabMenuEntry::Event { .. })));
    assert_eq!(app.session_chrome[&0].ext.menu, menu, "no recomposition");
    let expiry = clock_moved(&mut app);
    assert_eq!(
        app.session_chrome_wake(now, expiry - 5_000),
        Some(now + Duration::from_millis(5_000))
    );
}

#[test]
fn real_window_scan_conforms_to_model_and_bulk_mutant_is_rejected() {
    let model = aterm_models_misc::session_chrome_expiry_model();
    let mut state = model.init_state();
    assert!(model.fire("Seed", &mut state));
    assert!(model.fire("Expire", &mut state));
    assert!(model.fire("Begin", &mut state));

    let mut app = App::headless_for_test();
    app.refresh_window_tabs(WindowId(0));
    let expiry = clock_moved(&mut app);
    let real = app.advance_session_chrome_expiry(expiry);
    assert!(model.fire("Scan", &mut state));
    assert_eq!(
        i64::try_from(real.window_scans).unwrap(),
        state["work"],
        "model work is the real window scan, not merely a selected session"
    );
    assert!(real.window_scans <= EXPIRY_WINDOW_SCAN_BUDGET);
    assert!(real.window_refreshes <= real.window_scans);
    assert!(model.check_invariant("WorkPerTurnBounded", &state));

    let mut mutant_state = state;
    mutant_state.insert("work", 3);
    assert!(
        !model.check_invariant("WorkPerTurnBounded", &mutant_state),
        "an unbounded shipping mutant must fail the exact model invariant"
    );
}
