// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Memory tracking and cold tier budget enforcement.

use super::*;

#[test]
fn set_memory_budget_evicts_cold_pages() {
    let mut sb = Scrollback::with_block_size(5, 10, 10_000_000, 5);

    for i in 0..50 {
        sb.push_str(&format!("Budget-test-line-{i}"));
    }

    let total_before = sb.line_count();
    let cold_before = sb.cold_line_count();
    assert!(cold_before > 0, "need cold tier data for this test");
    assert!(
        !sb.over_budget(),
        "should not be over budget with generous limit"
    );

    sb.set_memory_budget(1)
        .expect("memory budget reduction should succeed");

    assert!(
        sb.cold_line_count() < cold_before,
        "cold tier should shrink after budget reduction: before={cold_before}, after={}",
        sb.cold_line_count()
    );
    assert_eq!(
        sb.cold_line_count(),
        0,
        "a 1-byte budget leaves no cold page"
    );
    assert!(
        sb.line_count() < total_before,
        "total line_count should decrease after cold eviction: before={total_before}, after={}",
        sb.line_count()
    );

    assert_eq!(
        sb.line_count(),
        sb.hot_line_count() + sb.warm_line_count() + sb.cold_line_count(),
        "line_count must equal sum of tier counts after eviction"
    );
    let newest = sb.get_line_rev(0).expect("no error").expect("newest line");
    assert_eq!(newest.to_string(), "Budget-test-line-49");
}

#[test]
fn pressure_evictions_are_counted_and_limit_truncation_is_not() {
    let mut sb = Scrollback::with_block_size(5, 10, 10_000_000, 5);
    for i in 0..50 {
        sb.push_str(&format!("Pressure-count-line-{i}"));
    }
    assert_eq!(sb.pressure_evicted_lines(), 0, "no loss under budget");
    let before = sb.line_count();
    sb.set_memory_budget(1).expect("budget shrink enforces");
    let lost = before - sb.line_count();
    assert!(lost > 0, "the 1-byte budget really evicted");
    assert_eq!(
        sb.pressure_evicted_lines(),
        lost as u64,
        "every pressure-evicted line is counted, none double-counted"
    );

    let counted = sb.pressure_evicted_lines();
    sb.set_line_limit(Some(1));
    assert!(sb.line_count() <= 1);
    assert_eq!(
        sb.pressure_evicted_lines(),
        counted,
        "set_line_limit truncation must not inflate the loss counter"
    );
}

#[test]
fn push_line_triggers_cold_eviction_when_over_budget() {
    let mut sb = Scrollback::with_block_size(5, 10, 1, 5);

    for i in 0..100 {
        sb.push_str(&format!("Pressure-line-{i}"));
    }

    assert!(
        sb.line_count() < 100,
        "some lines should have been evicted: line_count={}",
        sb.line_count()
    );

    assert_eq!(
        sb.line_count(),
        sb.hot_line_count() + sb.warm_line_count() + sb.cold_line_count(),
        "line_count must equal sum of tier counts"
    );

    for i in 0..sb.line_count() {
        let line = sb.get_line(i).expect("no error").expect("line present");
        assert!(!line.to_string().is_empty(), "line {i} should have content");
    }
}

#[test]
fn over_budget_false_after_cold_eviction() {
    let mut sb = Scrollback::with_block_size(5, 10, 10_000_000, 5);

    for i in 0..50 {
        sb.push_str(&format!("Over-budget-line-{i}"));
    }
    assert!(sb.cold_line_count() > 0, "need cold data");

    let half_memory = sb.total_memory_used() / 2;
    sb.set_memory_budget(half_memory.max(1))
        .expect("memory budget reduction should succeed");

    let still_over = sb.over_budget();
    let evictable_empty = sb.cold_line_count() == 0 && sb.warm_line_count() == 0;
    assert!(
        !still_over || evictable_empty,
        "should be under budget or have exhausted evictable tiers: over={still_over}, \
         total_mem={}, budget={}, cold={}, warm={}",
        sb.total_memory_used(),
        sb.memory_budget(),
        sb.cold_line_count(),
        sb.warm_line_count()
    );
}
