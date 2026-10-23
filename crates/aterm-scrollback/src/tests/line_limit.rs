// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Line limit enforcement and binary search validation.

use super::*;

#[test]
fn scrollback_line_limit_default_is_safe_cap() {
    let sb = Scrollback::with_defaults();
    assert_eq!(sb.line_limit(), Some(DEFAULT_LINE_LIMIT));
}

#[test]
fn scrollback_line_limit_zero_disables() {
    let mut sb = Scrollback::new(100, 1000, 10_000_000);
    sb.set_line_limit(Some(5));

    for i in 0..10 {
        sb.push_str(&format!("Line {i}"));
    }
    assert_eq!(sb.line_count(), 5);

    sb.set_line_limit(Some(0));
    assert_eq!(sb.line_count(), 0);
}

#[test]
fn scrollback_default_caps_runaway_push_from_oldest() {
    let mut sb = Scrollback::with_defaults();
    assert_eq!(sb.line_limit(), Some(DEFAULT_LINE_LIMIT));

    let cap = 500usize;
    sb.set_line_limit(Some(cap));

    let push_total: usize = cap * 10;
    for i in 0..push_total {
        sb.push_str(&format!("Line {i}"));
    }

    assert_eq!(sb.line_count(), cap, "line_count must be clamped to limit");

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
    let hot_limit = 10;
    let warm_limit = 50;
    let block_size = 5;
    let mut sb = Scrollback::with_block_size(hot_limit, warm_limit, 10_000_000, block_size);

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

    sb.set_line_limit(Some(hot_before));

    assert!(sb.cold_line_count() <= cold_before);
    assert!(sb.warm_line_count() <= warm_before);
    assert_eq!(sb.cold_line_count(), 0, "cold tier must drain before warm");
    assert_eq!(sb.warm_line_count(), 0, "warm tier must drain before hot");
    assert_eq!(sb.line_count(), hot_before);
}

#[test]
fn scrollback_line_limit_bounds_massive_push() {
    let cap = 200usize;
    let mut sb = Scrollback::new(20, 200, 10_000_000);
    sb.set_line_limit(Some(cap));

    for i in 0..50_000 {
        sb.push_str(&format!("x{i}"));
    }

    assert_eq!(sb.line_count(), cap);
    assert!(
        sb.budgeted_memory_used() < 4 * 1024 * 1024,
        "budgeted_memory_used={} exceeded safety headroom",
        sb.budgeted_memory_used(),
    );
}

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

    let empty: Vec<usize> = vec![];
    let mut empty_steps = 0usize;
    let result = binary_search_counted(&empty, 5, &mut empty_steps);
    assert_eq!(result, Err(0));
}
