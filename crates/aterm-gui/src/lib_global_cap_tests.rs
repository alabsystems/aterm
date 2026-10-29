// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    App, GLOBAL_SCROLLBACK_BUDGET_BYTES, ScrollbackMaintenanceEvent,
    admits_bulk_scrollback_maintenance, global_scrollback_budget_for, global_shed_order,
};

#[test]
fn global_scrollback_budget_is_capped_and_unknown_ram_takes_the_ceiling() {
    assert!(global_scrollback_budget_for(Some(8192)) < GLOBAL_SCROLLBACK_BUDGET_BYTES);
    assert_eq!(
        global_scrollback_budget_for(Some(65536)),
        GLOBAL_SCROLLBACK_BUDGET_BYTES
    );
    assert_eq!(
        global_scrollback_budget_for(None),
        GLOBAL_SCROLLBACK_BUDGET_BYTES
    );
}

#[test]
fn global_shed_order_picks_largest_until_under_cap() {
    let mib = 1usize << 20;
    let usages = [(1u64, 150 * mib), (2, 100 * mib), (3, 50 * mib)];
    assert_eq!(global_shed_order(&usages, 250 * mib), vec![1]);
    assert!(global_shed_order(&usages, 400 * mib).is_empty());
    assert!(global_shed_order(&[], 10).is_empty());
    assert_eq!(global_shed_order(&usages, 120 * mib), vec![1, 2]);
    let huge = [(1u64, usize::MAX), (2, usize::MAX - 1)];
    assert_eq!(global_shed_order(&huge, usize::MAX), vec![1]);
    assert_eq!(global_shed_order(&huge, 0), vec![1, 2]);
}

#[test]
fn shipping_lane_admission_matches_the_derived_model() {
    let model = aterm_models_misc::scrollback_maintenance_lane_model();

    let mut output = model.init_state();
    assert!(model.fire("ObserveOutput", &mut output));
    let output_model_admits = !model.successors("BeginBulkTrim", &output).is_empty();
    assert_eq!(
        admits_bulk_scrollback_maintenance(ScrollbackMaintenanceEvent::OrdinaryOutput),
        output_model_admits,
        "ordinary output must not enter the bulk-eviction lane"
    );
    assert_eq!(output["blocking_lock"], 0);
    assert_eq!(output["unbounded_work"], 0);

    let mut app = App::headless_for_test();
    let terminal = app
        .pool
        .iter()
        .next()
        .expect("headless terminal")
        .term
        .clone();
    let before = terminal
        .lock()
        .expect("checkpoint before ordinary-output gate")
        .checkpoint();
    app.route_bulk_scrollback_maintenance(ScrollbackMaintenanceEvent::OrdinaryOutput, false);
    let after = terminal
        .lock()
        .expect("checkpoint after ordinary-output gate")
        .checkpoint();
    assert_eq!(
        after, before,
        "ordinary output must be mutation-free in the shipping App"
    );

    let mut pressure = model.init_state();
    assert!(model.fire("ObserveMemoryPressure", &mut pressure));
    let pressure_model_admits = !model.successors("BeginBulkTrim", &pressure).is_empty();
    assert_eq!(
        admits_bulk_scrollback_maintenance(ScrollbackMaintenanceEvent::OsMemoryPressure),
        pressure_model_admits,
        "only the explicit OS-pressure lane may perform bulk eviction"
    );
    assert!(model.fire("BeginBulkTrim", &mut pressure));
    assert_eq!(pressure["blocking_lock"], 1);
    assert_eq!(pressure["unbounded_work"], 1);
}

const ALT_TEST_BUDGET: usize = 8 * 1024 * 1024;

fn app_with_alt_screen_history() -> (App, usize) {
    let app = App::headless_for_test();
    let session = app.pool.iter().next().expect("headless session");
    let mut term = super::term_lock(&session.term);
    *term = aterm_core::terminal::Terminal::with_scrollback(
        24,
        120,
        8,
        aterm_core::scrollback::Scrollback::new(8, 16, ALT_TEST_BUDGET),
    );
    let mut state: u64 = 0x2026_0926;
    let mut chunk = String::with_capacity(64 * 1024);
    for i in 0..30_000usize {
        chunk.push_str(&format!("{i:06} "));
        for _ in 0..12 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            chunk.push_str(&format!("{:08x}", state >> 32));
        }
        chunk.push_str("\r\n");
        if chunk.len() > 60 * 1024 {
            term.process(chunk.as_bytes());
            chunk.clear();
        }
    }
    term.process(chunk.as_bytes());
    term.process(b"\x1b[?1049h");
    assert!(
        term.modes().alternate_screen,
        "?1049h entered the alt screen"
    );
    assert!(
        term.scrollback().is_none(),
        "the ACTIVE grid is the store-less alt buffer — the read the lanes used to gate on"
    );
    let before = term.history_scrollback_bytes().expect("history store");
    assert!(
        before > 1 << 20,
        "history must exceed the 1 MiB shed floor for the shed to bite (got {before})"
    );
    assert!(
        before < ALT_TEST_BUDGET / 2,
        "history must sit under half the budget for WARN to keep it whole (got {before})"
    );
    drop(term);
    (app, before)
}

fn history_bytes(app: &App) -> usize {
    let session = app.pool.iter().next().expect("headless session");
    super::term_lock(&session.term)
        .history_scrollback_bytes()
        .expect("history store still attached")
}

#[test]
fn pressure_shed_trims_the_history_of_an_alt_screen_tab() {
    let (mut app, before) = app_with_alt_screen_history();

    let warn = app.shed_memory_under_pressure(false);
    assert_eq!(
        warn.reached, 1,
        "the alt-screen tab's store is reached: {warn:?}"
    );
    assert_eq!(
        (warn.trimmed, warn.freed_bytes),
        (0, 0),
        "history under half the budget: WARN frees nothing and says so ({warn:?})"
    );
    assert_eq!(history_bytes(&app), before);

    let critical = app.shed_memory_under_pressure(true);
    assert_eq!(
        (critical.reached, critical.trimmed),
        (1, 1),
        "the alt-screen tab is trimmed under CRITICAL: {critical:?}"
    );
    let after = history_bytes(&app);
    assert!(
        after < before && after <= 1 << 20,
        "the MAIN grid's history dropped to the 1 MiB floor ({before} -> {after})"
    );
    assert_eq!(
        critical.freed_bytes,
        before - after,
        "the count is bytes the store measured, not sessions with a store"
    );
}

#[test]
fn global_cap_counts_and_sheds_an_alt_screen_tab() {
    let (mut app, before) = app_with_alt_screen_history();

    let reclaim = app.enforce_scrollback_cap_at(before / 2);
    assert_eq!(
        (reclaim.reached, reclaim.trimmed),
        (1, 1),
        "the alt-screen tab counts toward the aggregate and is shed: {reclaim:?}"
    );
    let after = history_bytes(&app);
    assert!(
        after < before,
        "main-grid history dropped ({before} -> {after})"
    );
    assert_eq!(reclaim.freed_bytes, before - after);

    let idle = app.enforce_scrollback_cap_at(usize::MAX);
    assert_eq!(idle, super::ScrollbackReclaim::default());
    assert_eq!(history_bytes(&app), after);
}
