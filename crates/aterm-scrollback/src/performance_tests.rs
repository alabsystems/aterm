// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Performance proof tests for scrollback tier operations.
//!
//! Extracted from tests.rs to keep file sizes under 1000 lines.
//! Tests prove scaling behavior of push_line, truncate, and remove_newest.

use super::*;

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
/// #5893: cold tier front_offset gives O(1) truncate, where a decompress-all
/// truncate costs O(total_lines) per push to a full scrollback.
///
/// COUNTED, NOT TIMED (wall-clock ratios failed on a loaded machine). The
/// work a decompress-all truncate does is DECODING: every block it reads walks
/// its serialized records. So the proof counts the records walked
/// ([`crate::line::take_records_walked`], counted at the one walk every block
/// read goes through) while `batch` pushes run against a full scrollback, at a
/// 500-line and a 10K-line limit. The only decoding either may do is the
/// warm->cold eviction's framing check, once per line aging out of the warm
/// tier — so both counts are the batch, whatever the limit.
#[test]
fn push_with_line_limit_constant_time_truncate() {
    fn records_walked_pushing_with_limit(limit: usize, batch: usize) -> usize {
        let mut sb = Scrollback::with_block_size(50, 200, 500_000_000, 50);
        sb.set_line_limit(Some(limit));
        // Fill to the limit.
        for i in 0..limit {
            sb.push_str(&format!("Fill-{i}"));
        }
        assert_eq!(sb.line_count(), limit);
        let _ = crate::line::take_records_walked();
        // Measure: every push triggers truncate.
        for i in 0..batch {
            sb.push_str(&format!("Measure-{i}"));
        }
        assert_eq!(sb.line_count(), limit);
        crate::line::take_records_walked()
    }

    let batch = 500;
    let small = records_walked_pushing_with_limit(500, batch);
    let large = records_walked_pushing_with_limit(10_000, batch);
    eprintln!("push_line+truncate records walked: limit=500 -> {small}, limit=10K -> {large}");
    // At steady state every pushed line pushes one line out of warm, and the
    // eviction's framing check walks each such line exactly once: the count IS
    // the batch, at either limit. The #5893 shape walked the whole scrollback on
    // EVERY push (>= 500 x limit).
    for (limit, walked) in [(500, small), (10_000, large)] {
        assert_eq!(
            walked, batch,
            "{batch} pushes at limit {limit} walked {walked} records — the only decode \
             a push may pay is its line's one warm->cold framing check; more means \
             truncate is decoding the scrollback (#5893)"
        );
    }
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
/// Loading ALL kept lines into a Vec when removal spans tiers OOMs on large
/// scrollbacks; the tier-aware path decompresses at most one boundary block
/// (~block_size lines).
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

/// #5918: Prove that `remove_newest` work does not grow with total scrollback size.
///
/// Count decoded records: removing one line reads its 50-line boundary block
/// once, with identical work at 1K and 50K lines. Materializing the whole
/// history or decoding the boundary twice violates this bound.
#[test]
fn remove_newest_work_sublinear_in_total_size() {
    fn records_walked_removing_one(total: usize) -> usize {
        let mut sb = Scrollback::with_block_size(50, 200, 500_000_000, 50);
        for i in 0..total {
            sb.push_str(&format!("Scale-{i}"));
        }
        // Drain hot to force cross-tier path.
        let hot = sb.hot_line_count();
        if hot > 0 {
            sb.remove_newest(hot).expect("drain hot");
        }
        let before = sb.line_count();
        let _ = crate::line::take_records_walked();
        sb.remove_newest(1).expect("should succeed");
        let walked = crate::line::take_records_walked();
        assert_eq!(sb.line_count(), before - 1);
        walked
    }

    let small = records_walked_removing_one(1_000);
    let large = records_walked_removing_one(50_000);
    eprintln!("remove_newest(1) records walked: 1K lines -> {small}, 50K lines -> {large}");
    assert!(
        large <= 50,
        "remove_newest(1) on 50K lines walked {large} records — it is decoding more \
         than the one 50-line boundary block (O(total_lines) regression, #5918)"
    );
    assert_eq!(
        small, large,
        "the decode work of remove_newest(1) must not depend on scrollback size"
    );
}
