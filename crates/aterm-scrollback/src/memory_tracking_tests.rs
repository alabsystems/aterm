// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn scrollback_incremental_bytes_used_matches_recomputed() {
    let mut sb = Scrollback::with_block_size(10, 30, 20_000, 10);

    for i in 0..200 {
        sb.push_str(&format!("Tracked-line-{i:03}-{}", "x".repeat(24)));
        assert_eq!(
            sb.total_memory_used(),
            sb.recompute_total_memory_used(),
            "counter drift after push {i}",
        );
    }

    sb.remove_newest(40).expect("remove_newest should succeed");
    assert_eq!(
        sb.total_memory_used(),
        sb.recompute_total_memory_used(),
        "counter drift after remove_newest",
    );

    sb.truncate(60).expect("truncate should succeed");
    assert_eq!(
        sb.total_memory_used(),
        sb.recompute_total_memory_used(),
        "counter drift after truncate",
    );

    sb.set_memory_budget((sb.total_memory_used() / 2).max(1))
        .expect("memory budget update should succeed");
    assert_eq!(
        sb.total_memory_used(),
        sb.recompute_total_memory_used(),
        "counter drift after budget eviction",
    );
}

// =========================================================================
// Budget/diagnostic split tests (#5881)
// =========================================================================

#[test]
fn scrollback_budgeted_bytes_tracks_through_operations() {
    let mut sb = Scrollback::with_block_size(10, 30, 20_000, 10);

    for i in 0..200 {
        sb.push_str(&format!("Budget-line-{i:03}-{}", "x".repeat(24)));
        assert_eq!(
            sb.budgeted_bytes,
            sb.recompute_budgeted_bytes(),
            "budgeted_bytes drift after push {i}",
        );
    }

    sb.remove_newest(40).expect("remove_newest should succeed");
    assert_eq!(
        sb.budgeted_bytes,
        sb.recompute_budgeted_bytes(),
        "budgeted_bytes drift after remove_newest",
    );

    sb.truncate(60).expect("truncate should succeed");
    assert_eq!(
        sb.budgeted_bytes,
        sb.recompute_budgeted_bytes(),
        "budgeted_bytes drift after truncate",
    );

    sb.set_memory_budget((sb.budgeted_bytes / 2).max(1))
        .expect("memory budget update should succeed");
    assert_eq!(
        sb.budgeted_bytes,
        sb.recompute_budgeted_bytes(),
        "budgeted_bytes drift after budget eviction",
    );
}

/// Proves that read-only warm-cache fills do not perturb the budget counter.
/// Design step 5, test 1 from #5881 design doc.
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

/// Disk-backed: warm-cache fill does not affect budget.
/// Design step 5, test 2 from #5881 design doc.
#[cfg(feature = "disk-tier")]
#[test]
fn disk_backed_budget_ignores_warm_cache_population() {
    use aterm_tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("warm-budget.dtrm");

    let config = DiskBackedScrollbackConfig::new(&path)
        .with_hot_limit(10)
        .with_warm_limit(100)
        .with_block_size(10);
    let mut sb = DiskBackedScrollback::with_config(config).unwrap();

    for i in 0..30 {
        sb.push_str(&format!("Warm-budget-{i:03}-{}", "x".repeat(40)))
            .unwrap();
    }
    assert!(sb.warm_line_count() > 0, "fixture must have warm data");

    let budget_before = sb.budgeted_bytes();

    // Read a warm-tier line — this populates the block cache.
    let warm_start = sb.cold_line_count();
    let _line = sb
        .get_line(warm_start)
        .expect("no error")
        .expect("warm line should exist");

    assert_eq!(
        budget_before,
        sb.budgeted_bytes(),
        "budgeted_bytes must not change on warm cache fill"
    );
}

/// Disk-backed: cold-cache fill does not affect budget.
/// Design step 5, test 3 from #5881 design doc.
#[cfg(feature = "disk-tier")]
#[test]
fn disk_backed_budget_ignores_cold_cache_population() {
    use aterm_tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("cold-budget.dtrm");

    let config = DiskBackedScrollbackConfig::new(&path)
        .with_hot_limit(5)
        .with_warm_limit(10)
        .with_block_size(5);
    let mut sb = DiskBackedScrollback::with_config(config).unwrap();

    // Push enough to get data into cold tier.
    for i in 0..25 {
        sb.push_str(&format!("Cold-budget-{i:03}-{}", "x".repeat(40)))
            .unwrap();
    }
    assert!(sb.cold_line_count() > 0, "fixture must have cold data");

    let budget_before = sb.budgeted_bytes();
    let cold_mem_before = sb.cold_memory_used();

    // Read a cold-tier line — this populates the LRU cache.
    let _line = sb
        .get_line(0)
        .expect("no error")
        .expect("cold line should exist");

    assert_eq!(
        budget_before,
        sb.budgeted_bytes(),
        "budgeted_bytes must not change on cold cache fill"
    );
    assert!(
        sb.cold_memory_used() > cold_mem_before,
        "cold_memory_used should grow from cold cache (diagnostic)"
    );
}
