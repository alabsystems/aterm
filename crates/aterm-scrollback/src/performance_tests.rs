// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Performance proof tests for scrollback tier operations.
//!
//! Extracted from tests.rs to keep file sizes under 1000 lines.
//! Tests prove scaling behavior of push_line, truncate, and remove_newest.

use super::*;

/// Measure `push_line` throughput at different fill levels.
///
/// Production accounting is O(1) per push (cached running totals). In debug/test
/// builds, `assert_bytes_used_invariant` recomputes from all tiers — O(H+B+P) —
/// on every state change. This test captures the debug-build overhead as a
/// regression guard: if the ratio blows up, either the invariant check regressed
/// or incremental accounting drifted and triggered expensive recomputation.
#[test]
fn push_line_overhead_scales_with_tier_sizes() {
    fn measure_push_batch(pre_fill: usize, batch: usize) -> std::time::Duration {
        let mut sb = Scrollback::with_block_size(50, 200, 500_000_000, 50);
        for i in 0..pre_fill {
            sb.push_str(&format!("Fill-{i}"));
        }
        let start = std::time::Instant::now();
        for i in 0..batch {
            sb.push_str(&format!("Measure-{i}"));
        }
        start.elapsed()
    }

    let batch = 500;
    let small_time = measure_push_batch(500, batch);
    let large_time = measure_push_batch(10_000, batch);

    let ratio = large_time.as_nanos() as f64 / small_time.as_nanos().max(1) as f64;
    eprintln!(
        "push_line overhead: 500 pre-fill = {small_time:?}, 10K pre-fill = {large_time:?}, ratio = {ratio:.1}x"
    );
    // Debug builds run O(H+B+P) invariant checks per push, so the large case
    // has more work per push. Release builds are O(1) per push (cached totals).
    // Assert the debug overhead isn't catastrophically quadratic (< 40x).
    assert!(
        ratio < 40.0,
        "push_line overhead ratio {ratio:.1}x exceeds 40x — \
         possible regression in total_memory_used() hot path"
    );
}

/// Prove that tier-aware truncate correctly handles cross-tier line removal.
///
/// The tier-aware truncate removes oldest lines from cold → warm → hot
/// without decompressing the entire scrollback. Cold tier uses front_offset
/// for O(1) line removal. This test verifies correctness at scale.
#[test]
fn truncate_slow_path_scales_with_kept_lines() {
    let mut sb = Scrollback::with_block_size(10, 50, 500_000_000, 10);

    let total = 500;
    for i in 0..total {
        sb.push_str(&format!("T-{i}"));
    }
    assert!(sb.cold_line_count() > 0, "need cold tier data");
    assert!(sb.warm_line_count() > 0, "need warm tier data");

    let keep = 300; // More than hot → triggers cross-tier path
    assert!(keep > sb.hot_line_count(), "must trigger cross-tier path");

    sb.truncate(keep).expect("truncate should succeed");
    assert_eq!(sb.line_count(), keep);

    // Verify correctness: kept lines are the newest `keep`.
    let expected_first = total - keep;
    let line = sb.get_line(0).expect("ok").expect("present");
    assert_eq!(line.to_string(), format!("T-{expected_first}"));
    let line = sb.get_line(keep - 1).expect("ok").expect("present");
    assert_eq!(line.to_string(), format!("T-{}", total - 1));
}

/// Prove that push_line with line_limit does NOT degrade with scrollback size.
///
/// Before #5893: every push to a full scrollback triggered O(total_lines)
/// decompress-all truncate. After: cold tier front_offset gives O(1) truncate.
///
/// This test measures push throughput at two fill levels with a line_limit set.
/// A ratio near 1.0 proves truncate is O(1). A high ratio would indicate
/// the old O(N) slow path is still being hit.
#[test]
fn push_with_line_limit_constant_time_truncate() {
    fn measure_push_with_limit(limit: usize, batch: usize) -> std::time::Duration {
        let mut sb = Scrollback::with_block_size(50, 200, 500_000_000, 50);
        sb.set_line_limit(Some(limit));
        // Fill to the limit.
        for i in 0..limit {
            sb.push_str(&format!("Fill-{i}"));
        }
        assert_eq!(sb.line_count(), limit);
        // Measure: every push triggers truncate.
        let start = std::time::Instant::now();
        for i in 0..batch {
            sb.push_str(&format!("Measure-{i}"));
        }
        start.elapsed()
    }

    let batch = 500;
    let small_time = measure_push_with_limit(500, batch);
    let large_time = measure_push_with_limit(10_000, batch);

    let ratio = large_time.as_nanos() as f64 / small_time.as_nanos().max(1) as f64;
    eprintln!(
        "push_line+truncate: limit=500 = {small_time:?}, limit=10K = {large_time:?}, ratio = {ratio:.1}x"
    );
    // With tier-aware truncate (cold front_offset), the ratio should be near 1.0.
    // Before #5893, this was O(total_lines) per push → ratio ~20x.
    // Allow up to 5x for test noise and cold tier page-drop amortization.
    assert!(
        ratio < 5.0,
        "push_line+truncate ratio {ratio:.1}x exceeds 5x — \
         tier-aware truncate may not be working (expected O(1), got O(N))"
    );
}

/// Prove that remove_newest works correctly with tier-aware back-removal.
///
/// Tier-aware remove_newest removes from hot → warm → cold back-to-front,
/// decompressing at most one boundary block/page at a time.
#[test]
fn remove_newest_slow_path_scales_with_kept_lines() {
    let mut sb = Scrollback::with_block_size(10, 50, 500_000_000, 10);

    let total = 500;
    for i in 0..total {
        sb.push_str(&format!("R-{i}"));
    }
    assert!(sb.cold_line_count() > 0, "need cold tier data");

    let remove = sb.hot_line_count() + 50; // Triggers cross-tier path
    let keep = total - remove;

    sb.remove_newest(remove).expect("should succeed");
    assert_eq!(sb.line_count(), keep);

    // Verify correctness: kept lines are the oldest.
    let line = sb.get_line(0).expect("ok").expect("present");
    assert_eq!(line.to_string(), "R-0");
    let line = sb.get_line(keep - 1).expect("ok").expect("present");
    assert_eq!(line.to_string(), format!("R-{}", keep - 1));
}

/// remove_newest: hot-only removal (fast path, no decompression).
#[test]
fn remove_newest_hot_only() {
    let mut sb = Scrollback::with_block_size(10, 50, 500_000_000, 10);

    for i in 0..5 {
        sb.push_str(&format!("H-{i}"));
    }
    assert_eq!(sb.hot_line_count(), 5);
    assert_eq!(sb.warm_line_count(), 0);
    assert_eq!(sb.cold_line_count(), 0);

    sb.remove_newest(2).expect("should succeed");
    assert_eq!(sb.line_count(), 3);

    for i in 0..3 {
        let line = sb.get_line(i).expect("ok").expect("present");
        assert_eq!(line.to_string(), format!("H-{i}"));
    }
}

/// remove_newest: removal spans hot + warm tiers.
#[test]
fn remove_newest_spans_warm() {
    let mut sb = Scrollback::with_block_size(5, 50, 500_000_000, 5);

    // Push enough lines to have data in warm + hot.
    for i in 0..20 {
        sb.push_str(&format!("W-{i}"));
    }
    let hot = sb.hot_line_count();
    let warm = sb.warm_line_count();
    assert!(warm > 0, "need warm tier data");

    // Remove all of hot + some of warm.
    let remove = hot + 3;
    sb.remove_newest(remove).expect("should succeed");
    let expected = 20 - remove;
    assert_eq!(sb.line_count(), expected);

    // Verify kept lines are the oldest.
    for i in 0..expected {
        let line = sb.get_line(i).expect("ok").expect("present");
        assert_eq!(line.to_string(), format!("W-{i}"));
    }
}

/// remove_newest: removal reaches into cold tier.
#[test]
fn remove_newest_spans_cold() {
    let mut sb = Scrollback::with_block_size(5, 10, 500_000_000, 5);

    for i in 0..50 {
        sb.push_str(&format!("C-{i}"));
    }
    let hot = sb.hot_line_count();
    let warm = sb.warm_line_count();
    let cold = sb.cold_line_count();
    assert!(
        cold > 0,
        "need cold tier data: hot={hot} warm={warm} cold={cold}"
    );

    // Remove all hot + all warm + some cold.
    let remove = hot + warm + 3;
    sb.remove_newest(remove).expect("should succeed");
    let expected = 50 - remove;
    assert_eq!(sb.line_count(), expected);

    // Verify kept lines are the oldest.
    for i in 0..expected {
        let line = sb.get_line(i).expect("ok").expect("present");
        assert_eq!(line.to_string(), format!("C-{i}"));
    }
}

/// remove_newest: partial warm block re-compression preserves data.
#[test]
fn remove_newest_partial_warm_block() {
    let mut sb = Scrollback::with_block_size(5, 50, 500_000_000, 5);

    for i in 0..15 {
        sb.push_str(&format!("P-{i}"));
    }
    let hot = sb.hot_line_count();
    assert!(sb.warm_line_count() > 0, "need warm data");

    // Remove hot + 2 lines (leaving a partial warm block).
    let remove = hot + 2;
    sb.remove_newest(remove).expect("should succeed");
    let expected = 15 - remove;
    assert_eq!(sb.line_count(), expected);

    for i in 0..expected {
        let line = sb.get_line(i).expect("ok").expect("present");
        assert_eq!(line.to_string(), format!("P-{i}"));
    }
}

/// remove_newest: removing all lines clears the scrollback.
#[test]
fn remove_newest_all_lines() {
    let mut sb = Scrollback::with_block_size(5, 10, 500_000_000, 5);

    for i in 0..30 {
        sb.push_str(&format!("A-{i}"));
    }
    assert!(sb.cold_line_count() > 0);

    sb.remove_newest(30).expect("should succeed");
    assert_eq!(sb.line_count(), 0);
    assert_eq!(sb.hot_line_count(), 0);
    assert_eq!(sb.warm_line_count(), 0);
    assert_eq!(sb.cold_line_count(), 0);
}

/// #5918: Prove that `remove_newest(1)` on a large scrollback does NOT spike
/// memory to O(total_lines).
///
/// Before the tier-aware fix, `remove_newest` loaded ALL kept lines into a Vec
/// when removal spanned tiers (OOM on large scrollbacks). The tier-aware
/// implementation decompresses at most one boundary block (~block_size lines).
///
/// This test verifies the memory-bounded property by checking that
/// `memory_used()` after `remove_newest(1)` stays close to the pre-removal
/// level — NOT doubling from full-materialization.
#[test]
fn remove_newest_memory_bounded_cross_tier() {
    // Small hot tier (5 lines) forces remove_newest(1) to cross into warm
    // when hot is empty. block_size=50 means at most ~50 lines decompressed.
    let mut sb = Scrollback::with_block_size(5, 200, 500_000_000, 50);

    // Fill with enough data to populate all three tiers.
    let total = 10_000;
    for i in 0..total {
        sb.push_str(&format!("Memory-test-{i}"));
    }
    assert!(sb.cold_line_count() > 0, "need cold tier data");
    assert!(sb.warm_line_count() > 0, "need warm tier data");

    // Drain the hot tier to force remove_newest into the cross-tier path.
    let hot = sb.hot_line_count();
    if hot > 0 {
        sb.remove_newest(hot).expect("drain hot");
    }
    assert_eq!(sb.hot_line_count(), 0, "hot should be empty");

    let mem_before = sb.total_memory_used();
    let lines_before = sb.line_count();

    // Remove 1 line — must cross into warm tier (hot is empty).
    sb.remove_newest(1)
        .expect("remove_newest(1) should succeed");
    assert_eq!(sb.line_count(), lines_before - 1);

    let mem_after = sb.total_memory_used();

    // Memory should stay in the same ballpark. The old O(N) approach would
    // roughly double memory (full materialization + rebuild). Allow up to
    // 50% increase for warm block decompression/recompression overhead.
    let ratio = mem_after as f64 / mem_before.max(1) as f64;
    eprintln!("remove_newest(1) memory: before={mem_before}, after={mem_after}, ratio={ratio:.2}x");
    assert!(
        ratio < 1.5,
        "remove_newest(1) memory ratio {ratio:.2}x exceeds 1.5x — \
         likely materializing all lines (expected O(block_size), got O(total))"
    );
}

/// #5918: Prove that `remove_newest` timing is sublinear in total scrollback size.
///
/// The tier-aware implementation is O(blocks) not O(total_lines) — the warm
/// tier's truncate_back_lines rebuilds cumulative indices from all blocks after
/// the boundary block modification. With block_size=50: 50K lines → ~1000 blocks,
/// 1K lines → ~20 blocks, giving a theoretical ratio of ~50x for O(N) but only
/// ~15x for O(blocks). The old approach was O(total_lines) giving ~50x.
#[test]
fn remove_newest_time_sublinear_in_total_size() {
    fn measure_remove(total: usize) -> std::time::Duration {
        let mut sb = Scrollback::with_block_size(5, 200, 500_000_000, 50);
        for i in 0..total {
            sb.push_str(&format!("Scale-{i}"));
        }
        // Drain hot to force cross-tier path.
        let hot = sb.hot_line_count();
        if hot > 0 {
            sb.remove_newest(hot).expect("drain hot");
        }
        let start = std::time::Instant::now();
        sb.remove_newest(1).expect("should succeed");
        start.elapsed()
    }

    let small_time = measure_remove(1_000);
    let large_time = measure_remove(50_000);

    let ratio = large_time.as_nanos() as f64 / small_time.as_nanos().max(1) as f64;
    eprintln!(
        "remove_newest(1) timing: 1K lines = {small_time:?}, 50K lines = {large_time:?}, ratio = {ratio:.1}x"
    );
    // The old O(total_lines) approach gave ~50x ratio (proportional to 50K/1K).
    // The tier-aware approach is O(blocks) — still proportional to total data
    // but divided by block_size. Allow up to 20x for the block-level overhead
    // plus test noise.
    assert!(
        ratio < 20.0,
        "remove_newest(1) time ratio {ratio:.1}x exceeds 20x — \
         possible O(total_lines) regression (expected O(blocks))"
    );
}

/// Prove that memory pressure eviction of many cold pages completes
/// without hanging or OOM.
///
/// When `set_memory_budget()` forces bulk cold eviction, the while-loop in
/// `handle_memory_pressure()` calls `pop_front()` per page.
#[test]
fn memory_pressure_cold_eviction_scaling() {
    // Build a scrollback with many cold pages.
    // Small hot/warm to force rapid promotion to cold.
    // block_size=10 with hot_limit=10 means every 10 lines promote a block.
    let mut sb = Scrollback::with_block_size(10, 50, 100_000_000, 10);

    // Push enough lines to create ~200 cold pages.
    for i in 0..2500 {
        sb.push_str(&format!("Pressure-test-line-{i}"));
    }
    assert!(
        sb.cold_line_count() > 1000,
        "expected many cold lines, got {}",
        sb.cold_line_count()
    );

    let cold_before = sb.cold_line_count();
    let start = std::time::Instant::now();

    // Slash the budget to force bulk cold eviction.
    sb.set_memory_budget(1)
        .expect("memory budget reduction should succeed");

    let elapsed = start.elapsed();
    let cold_after = sb.cold_line_count();

    eprintln!("memory_pressure eviction: cold {cold_before} -> {cold_after}, elapsed {elapsed:?}");

    // Cold tier should be fully or mostly evicted.
    assert_eq!(cold_after, 0, "cold tier should be empty after budget=1");
}

/// Guard the end-to-end warm-to-cold budget path in a high-block configuration.
///
/// `WarmTier::pop_front()` calls `rebuild_cumulative()` on each eviction,
/// making K sequential evictions O(K*B_remaining) = O(B^2) total. The exact
/// rebuild work for this setup's warm tier is covered by
/// `warm_tier_pop_front_rebuild_steps_match_quadratic_bound`; this test keeps a
/// practical wall-clock guard on the full `set_memory_budget()` path.
#[test]
fn warm_eviction_cost_bounded_with_many_blocks() {
    // Create a scrollback with many small warm blocks to maximize B.
    // block_size=5, hot_limit=5, warm_limit=2000 → up to 400 warm blocks.
    fn measure_warm_eviction(warm_limit: usize) -> (usize, std::time::Duration) {
        let mut sb = Scrollback::with_block_size(5, warm_limit, 500_000_000, 5);

        // Fill until warm tier is well-populated.
        let fill = warm_limit + 200; // Extra to ensure cold promotion.
        for i in 0..fill {
            sb.push_str(&format!("WE-{i}"));
        }
        let warm_blocks = sb.warm.block_count();
        assert!(warm_blocks > 0, "need warm blocks for eviction test");
        assert!(
            sb.cold_line_count() > 0,
            "fill pattern should create cold data for the end-to-end budget path"
        );

        let start = std::time::Instant::now();
        // Slash budget to force all warm blocks out of the warm tier.
        sb.set_memory_budget(1)
            .expect("memory budget reduction should succeed");
        let elapsed = start.elapsed();

        assert_eq!(
            sb.warm.block_count(),
            0,
            "all warm blocks should be evicted"
        );
        (warm_blocks, elapsed)
    }

    let (small_blocks, small_time) = measure_warm_eviction(200);
    let (large_blocks, large_time) = measure_warm_eviction(2000);

    let ratio = large_time.as_nanos() as f64 / small_time.as_nanos().max(1) as f64;
    let block_ratio = large_blocks as f64 / small_blocks.max(1) as f64;
    eprintln!(
        "warm eviction: {small_blocks} blocks = {small_time:?}, {large_blocks} blocks = {large_time:?}, \
         time ratio = {ratio:.1}x, block ratio = {block_ratio:.1}x"
    );
    // With rebuild_cumulative O(B) per pop, K pops cost O(K*B) = O(B^2).
    // The larger case therefore does substantially more rebuild work as the
    // warm-block count rises, even before the rest of the budget path runs.
    // The end-to-end path also pays cold-tier recompression and accounting
    // costs, so keep this bound generous and treat it only as a wall-clock
    // regression guard against something worse than the expected shape.
    // Allow up to 200x for debug-build overhead and test noise.
    // If this exceeds 200x, something worse than O(B^2) has been introduced.
    assert!(
        ratio < 200.0,
        "warm eviction ratio {ratio:.1}x exceeds 200x — \
         possible super-quadratic regression (expected O(B^2) bounded)"
    );
}
