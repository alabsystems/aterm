// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Scaling tests for push_line, truncate and remove_newest across tiers.

use super::*;

/// `push_line` under a line limit walks a constant number of records regardless of scrollback size.
#[test]
fn push_with_line_limit_constant_time_truncate() {
    fn records_walked_pushing_with_limit(limit: usize, batch: usize) -> usize {
        let mut sb = Scrollback::with_block_size(50, 200, 500_000_000, 50);
        sb.set_line_limit(Some(limit));
        for i in 0..limit {
            sb.push_str(&format!("Fill-{i}"));
        }
        assert_eq!(sb.line_count(), limit);
        let _ = crate::line::take_records_walked();
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
    for (limit, walked) in [(500, small), (10_000, large)] {
        assert_eq!(
            walked, batch,
            "{batch} pushes at limit {limit} walked {walked} records — the only decode \
             a push may pay is its line's one warm->cold framing check; more means \
             truncate is decoding the scrollback (#5893)"
        );
    }
}

/// `remove_newest(1)` across tiers decompresses at most one boundary block rather than materializing the full scrollback.
#[test]
fn remove_newest_memory_bounded_cross_tier() {
    let mut sb = Scrollback::with_block_size(5, 200, 500_000_000, 50);

    let total = 10_000;
    for i in 0..total {
        sb.push_str(&format!("Memory-test-{i}"));
    }
    assert!(sb.cold_line_count() > 0, "need cold tier data");
    assert!(sb.warm_line_count() > 0, "need warm tier data");

    let hot = sb.hot_line_count();
    if hot > 0 {
        sb.remove_newest(hot).expect("drain hot");
    }
    assert_eq!(sb.hot_line_count(), 0, "hot should be empty");

    let mem_before = sb.total_memory_used();
    let lines_before = sb.line_count();

    sb.remove_newest(1)
        .expect("remove_newest(1) should succeed");
    assert_eq!(sb.line_count(), lines_before - 1);

    let mem_after = sb.total_memory_used();

    let ratio = mem_after as f64 / mem_before.max(1) as f64;
    eprintln!("remove_newest(1) memory: before={mem_before}, after={mem_after}, ratio={ratio:.2}x");
    assert!(
        ratio < 1.5,
        "remove_newest(1) memory ratio {ratio:.2}x exceeds 1.5x — \
         likely materializing all lines (expected O(block_size), got O(total))"
    );
}

/// `remove_newest(1)` decode work stays bounded by a single boundary block regardless of scrollback size.
#[test]
fn remove_newest_work_sublinear_in_total_size() {
    fn records_walked_removing_one(total: usize) -> usize {
        let mut sb = Scrollback::with_block_size(50, 200, 500_000_000, 50);
        for i in 0..total {
            sb.push_str(&format!("Scale-{i}"));
        }
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
