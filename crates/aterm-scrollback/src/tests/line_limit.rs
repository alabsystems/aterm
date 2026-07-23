// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Line limit enforcement (#1865, #7929) and binary search validation.

use super::*;

#[test]
fn scrollback_line_limit_default_is_safe_cap() {
    // #7929: default line limit must be set to prevent runaway-stdout DoS.
    // Callers that want unlimited history must explicitly opt in.
    let sb = Scrollback::with_defaults();
    assert_eq!(sb.line_limit(), Some(DEFAULT_LINE_LIMIT));
    assert_eq!(
        DEFAULT_LINE_LIMIT, 100_000,
        "DEFAULT_LINE_LIMIT changed; update docs and HN readiness gate",
    );
}

#[test]
fn scrollback_set_line_limit() {
    let mut sb = Scrollback::with_defaults();
    // Default now has a bounded limit (#7929). Clear first to exercise the
    // full Some→None→Some matrix.
    sb.set_line_limit(None);
    assert_eq!(sb.line_limit(), None);

    sb.set_line_limit(Some(100));
    assert_eq!(sb.line_limit(), Some(100));

    sb.set_line_limit(None);
    assert_eq!(sb.line_limit(), None);
}

#[test]
fn scrollback_line_limit_enforced_on_push() {
    let mut sb = Scrollback::new(100, 1000, 10_000_000);
    sb.set_line_limit(Some(10));

    // Push 15 lines
    for i in 0..15 {
        sb.push_str(&format!("Line {i}"));
    }

    // Only 10 lines should remain (oldest 5 discarded)
    assert_eq!(sb.line_count(), 10);

    // Should have newest lines (5-14)
    let line = sb
        .get_line(0)
        .expect("no error")
        .expect("line 0 should exist");
    assert!(
        line.to_string().contains("Line 5"),
        "oldest kept should be line 5, got: {line}",
    );
    let line = sb
        .get_line(9)
        .expect("no error")
        .expect("line 9 should exist");
    assert!(
        line.to_string().contains("Line 14"),
        "newest should be line 14, got: {line}",
    );
}

#[test]
fn scrollback_set_line_limit_truncates_immediately() {
    let mut sb = Scrollback::new(100, 1000, 10_000_000);

    // Push 20 lines
    for i in 0..20 {
        sb.push_str(&format!("Line {i}"));
    }
    assert_eq!(sb.line_count(), 20);

    // Set limit to 10 - should truncate immediately
    sb.set_line_limit(Some(10));
    assert_eq!(sb.line_count(), 10);

    // Should keep newest 10 lines (10-19)
    let line = sb
        .get_line(0)
        .expect("no error")
        .expect("line 0 should exist");
    assert!(
        line.to_string().contains("Line 10"),
        "oldest kept should be line 10"
    );
}

#[test]
fn scrollback_line_limit_zero_disables() {
    let mut sb = Scrollback::new(100, 1000, 10_000_000);
    sb.set_line_limit(Some(5));

    for i in 0..10 {
        sb.push_str(&format!("Line {i}"));
    }
    assert_eq!(sb.line_count(), 5);

    // Setting to Some(0) clears everything
    sb.set_line_limit(Some(0));
    assert_eq!(sb.line_count(), 0);
}

#[test]
fn scrollback_default_caps_runaway_push_from_oldest() {
    // #7929 regression: with the default line_limit in place, pushing
    // far more than the cap must not grow `line_count` past it, and must
    // evict the oldest lines (tier-ordered: cold → warm → hot).
    let mut sb = Scrollback::with_defaults();
    assert_eq!(sb.line_limit(), Some(DEFAULT_LINE_LIMIT));

    // 10x the default cap. Using a small limit + small tiers keeps this
    // fast enough for the unit suite while exercising cross-tier drops.
    let cap = 500usize;
    sb.set_line_limit(Some(cap));

    let push_total: usize = cap * 10;
    for i in 0..push_total {
        sb.push_str(&format!("Line {i}"));
    }

    // Line count is bounded.
    assert_eq!(sb.line_count(), cap, "line_count must be clamped to limit");

    // Oldest-first eviction preserved: the oldest retained line must be
    // `push_total - cap` (everything before that was dropped).
    let expected_oldest = format!("Line {}", push_total - cap);
    let oldest = sb
        .get_line(0)
        .expect("no error")
        .expect("line 0 should exist")
        .to_string();
    assert!(
        oldest.contains(&expected_oldest),
        "expected oldest retained line to contain `{expected_oldest}`, got `{oldest}`",
    );

    // Newest is preserved.
    let newest = sb
        .get_line_rev(0)
        .expect("no error")
        .expect("newest line should exist")
        .to_string();
    let expected_newest = format!("Line {}", push_total - 1);
    assert!(
        newest.contains(&expected_newest),
        "expected newest line to contain `{expected_newest}`, got `{newest}`",
    );
}

#[test]
fn scrollback_line_limit_evicts_from_oldest_tier_first() {
    // Populate enough to push lines through every tier (cold + warm + hot),
    // then set a lower limit and confirm eviction strictly removes from the
    // oldest tier first (cold before warm before hot).
    let hot_limit = 10;
    let warm_limit = 50;
    let block_size = 5;
    let mut sb = Scrollback::with_block_size(hot_limit, warm_limit, 10_000_000, block_size);

    // Fill enough to force lines into warm and cold.
    for i in 0..200 {
        sb.push_str(&format!("Line {i}"));
    }
    assert!(
        sb.cold_line_count() > 0,
        "expected cold tier to be non-empty"
    );
    assert!(
        sb.warm_line_count() > 0,
        "expected warm tier to be non-empty"
    );
    assert!(sb.hot_line_count() > 0, "expected hot tier to be non-empty");

    let cold_before = sb.cold_line_count();
    let warm_before = sb.warm_line_count();
    let hot_before = sb.hot_line_count();

    // Trim to just the hot tier's worth of lines.
    sb.set_line_limit(Some(hot_before));

    // All removed lines must come from cold first, then warm; hot survives.
    assert!(sb.cold_line_count() <= cold_before);
    assert!(sb.warm_line_count() <= warm_before);
    // If we can fit entirely in hot, cold + warm should be drained before hot
    // loses a single line.
    assert_eq!(sb.cold_line_count(), 0, "cold tier must drain before warm");
    assert_eq!(sb.warm_line_count(), 0, "warm tier must drain before hot");
    assert_eq!(sb.line_count(), hot_before);
}

#[test]
fn scrollback_line_limit_bounds_massive_push() {
    // Regression for #7929 HN F09-1: a runaway stdout must not blow out
    // scrollback. Push >> cap and assert the final footprint is bounded by
    // the cap plus one hot-tier block of amortization (push_line only
    // truncates *after* the line is appended).
    let cap = 200usize;
    let mut sb = Scrollback::new(20, 200, 10_000_000);
    sb.set_line_limit(Some(cap));

    for i in 0..50_000 {
        sb.push_str(&format!("x{i}"));
    }

    assert_eq!(sb.line_count(), cap);
    // Budgeted memory must be reasonable relative to the cap.
    // Each "xN" line is ~5 bytes + overhead; 200 lines well under 1 MiB.
    assert!(
        sb.budgeted_memory_used() < 4 * 1024 * 1024,
        "budgeted_memory_used={} exceeded safety headroom",
        sb.budgeted_memory_used(),
    );
}

/// Verify that `binary_search_counted` matches `slice::binary_search` for a
/// range of inputs, ensuring the hand-rolled implementation used for step
/// counting is behaviorally equivalent to the stdlib.
#[test]
fn binary_search_counted_matches_stdlib() {
    let cumulative: Vec<usize> = vec![3, 7, 12, 20, 25, 30, 44, 50];

    for target in 0..=55 {
        let expected = cumulative.binary_search(&target);
        let mut steps = 0usize;
        let actual = binary_search_counted(&cumulative, target, &mut steps);
        assert_eq!(
            actual, expected,
            "mismatch for target={target}: counted={actual:?}, stdlib={expected:?}"
        );
        assert!(
            steps > 0 || cumulative.is_empty(),
            "should perform at least one step"
        );
    }

    // Empty slice edge case.
    let empty: Vec<usize> = vec![];
    let mut empty_steps = 0usize;
    let result = binary_search_counted(&empty, 5, &mut empty_steps);
    assert_eq!(result, Err(0));
}
