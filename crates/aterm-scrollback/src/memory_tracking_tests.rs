// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

/// Both incrementally kept counters — the diagnostic total and the budgeted
/// bytes (#5881) — equal a from-scratch recount after every kind of mutation.
#[test]
fn scrollback_incremental_counters_match_recomputed() {
    let mut sb = Scrollback::with_block_size(10, 30, 20_000, 10);
    let check = |sb: &Scrollback, after: &str| {
        assert_eq!(
            sb.total_memory_used(),
            sb.recompute_total_memory_used(),
            "total drift after {after}"
        );
        assert_eq!(
            sb.budgeted_bytes,
            sb.recompute_budgeted_bytes(),
            "budgeted drift after {after}"
        );
    };

    for i in 0..200 {
        sb.push_str(&format!("Tracked-line-{i:03}-{}", "x".repeat(24)));
        check(&sb, &format!("push {i}"));
    }
    sb.remove_newest(40).expect("remove_newest should succeed");
    check(&sb, "remove_newest");
    sb.truncate(60).expect("truncate should succeed");
    check(&sb, "truncate");
    sb.set_memory_budget((sb.total_memory_used() / 2).max(1))
        .expect("memory budget update should succeed");
    check(&sb, "budget eviction");
}

// =========================================================================
// Budget/diagnostic split tests
// =========================================================================

/// Proves that read-only warm-cache fills do not perturb the budget counter.
#[test]
fn scrollback_budget_ignores_warm_cache_population() {
    // Small tiers to force warm promotion quickly.
    let mut sb = Scrollback::with_block_size(10, 100, 1_000_000, 10);

    // Push enough lines to create warm-tier blocks.
    for i in 0..30 {
        sb.push_str(&format!("Warm-budget-{i:03}-{}", "x".repeat(40)));
    }
    assert!(sb.warm_line_count() > 0, "fixture must have warm data");

    let budget_before = sb.budgeted_bytes;
    // memory_used() reads live from tiers (warm uses Cell for cache tracking).
    let hw_mem_before = sb.memory_used();

    // Read a warm-tier line — this populates the block cache.
    let _line = sb
        .get_line(0)
        .expect("no error")
        .expect("line 0 should exist");

    let budget_after = sb.budgeted_bytes;
    let hw_mem_after = sb.memory_used();

    assert_eq!(
        budget_before, budget_after,
        "budgeted_bytes must not change on warm cache fill"
    );
    assert!(
        hw_mem_after > hw_mem_before,
        "memory_used() should grow from warm cache (diagnostic), \
         got before={hw_mem_before} after={hw_mem_after}"
    );
}
