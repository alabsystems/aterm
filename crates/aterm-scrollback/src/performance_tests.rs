// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Performance proof tests for scrollback tier operations.
//!
//! Extracted from tests.rs to keep file sizes under 1000 lines.
//! Tests prove scaling behavior of push_line, truncate, and remove_newest.

use super::*;

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
