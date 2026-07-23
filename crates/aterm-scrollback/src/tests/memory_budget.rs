// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Memory tracking and cold tier budget enforcement (#5444).

use super::*;

#[test]
fn scrollback_memory_tracking() {
    let mut sb = Scrollback::new(100, 1000, 10_000_000);

    let initial_mem = sb.memory_used();
    sb.push_str("Hello World");

    assert!(sb.memory_used() > initial_mem);
}

#[test]
fn scrollback_cold_memory_used() {
    // Small limits to trigger eviction to cold tier
    let mut sb = Scrollback::with_block_size(5, 10, 10_000_000, 5);

    // Initially cold memory should be 0
    assert_eq!(sb.cold_memory_used(), 0);

    // Push 25 lines - should evict to cold
    for i in 0..25 {
        sb.push_str(&format!("Line {i}"));
    }

    // Verify cold tier has data
    assert!(sb.cold_line_count() > 0);
    // Cold tier should now have compressed data
    assert!(sb.cold_memory_used() > 0);
}

#[test]
fn scrollback_total_memory_used() {
    // Small limits to trigger eviction to cold tier
    let mut sb = Scrollback::with_block_size(5, 10, 10_000_000, 5);

    // memory_used includes base struct size, so initial is non-zero
    let initial_total = sb.total_memory_used();
    let initial_hot_warm = sb.memory_used();

    // Push lines only to hot tier
    for i in 0..4 {
        sb.push_str(&format!("Line {i}"));
    }
    // Hot only: total = memory_used (cold tier is empty)
    assert_eq!(sb.total_memory_used(), sb.memory_used());
    assert_eq!(sb.cold_memory_used(), 0);
    assert!(
        sb.memory_used() > initial_hot_warm,
        "memory should increase with lines"
    );

    // Push 25 lines - should have data in all tiers
    for i in 4..25 {
        sb.push_str(&format!("Line {i}"));
    }

    // Total should include cold tier
    let hot_warm = sb.memory_used();
    let cold = sb.cold_memory_used();
    assert_eq!(sb.total_memory_used(), hot_warm + cold);
    assert!(cold > 0, "cold tier should have compressed data");
    assert!(
        sb.total_memory_used() > initial_total,
        "total should increase"
    );
}

/// Cold tier budget enforcement (#5444): reducing the memory budget evicts cold pages.
#[test]
fn set_memory_budget_evicts_cold_pages() {
    // Small tiers: hot=5, warm=10, block_size=5, generous initial budget
    let mut sb = Scrollback::with_block_size(5, 10, 10_000_000, 5);

    // Push 50 lines → forces data through hot → warm → cold
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

    // Reduce budget to 1 byte → forces cold eviction
    sb.set_memory_budget(1)
        .expect("memory budget reduction should succeed");

    // Cold pages should have been evicted
    assert!(
        sb.cold_line_count() < cold_before,
        "cold tier should shrink after budget reduction: before={cold_before}, after={}",
        sb.cold_line_count()
    );
    assert!(
        sb.line_count() < total_before,
        "total line_count should decrease after cold eviction: before={total_before}, after={}",
        sb.line_count()
    );

    // Line count consistency: total = hot + warm + cold
    assert_eq!(
        sb.line_count(),
        sb.hot_line_count() + sb.warm_line_count() + sb.cold_line_count(),
        "line_count must equal sum of tier counts after eviction"
    );
}

/// Cold tier budget enforcement (#5444): push_line triggers cold eviction under pressure.
#[test]
fn push_line_triggers_cold_eviction_when_over_budget() {
    // Tiny budget: forces eviction as soon as cold tier has data
    let mut sb = Scrollback::with_block_size(5, 10, 1, 5);

    // Push enough to force cold tier population + eviction
    for i in 0..100 {
        sb.push_str(&format!("Pressure-line-{i}"));
    }

    // With budget=1, cold pages should have been evicted as they formed.
    assert!(
        sb.line_count() < 100,
        "some lines should have been evicted: line_count={}",
        sb.line_count()
    );

    // Line count consistency
    assert_eq!(
        sb.line_count(),
        sb.hot_line_count() + sb.warm_line_count() + sb.cold_line_count(),
        "line_count must equal sum of tier counts"
    );

    // Surviving lines should still be readable
    for i in 0..sb.line_count() {
        let line = sb.get_line(i).expect("no error").expect("line present");
        assert!(!line.to_string().is_empty(), "line {i} should have content");
    }
}

/// Cold tier budget enforcement (#5444): over_budget returns false after eviction.
#[test]
fn over_budget_false_after_cold_eviction() {
    let mut sb = Scrollback::with_block_size(5, 10, 10_000_000, 5);

    // Fill cold tier
    for i in 0..50 {
        sb.push_str(&format!("Over-budget-line-{i}"));
    }
    assert!(sb.cold_line_count() > 0, "need cold data");

    // Set budget to half the current memory to force partial eviction
    let half_memory = sb.total_memory_used() / 2;
    sb.set_memory_budget(half_memory.max(1))
        .expect("memory budget reduction should succeed");

    // After set_memory_budget, handle_memory_pressure should have run
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

/// Cold tier budget enforcement (#5444): newest lines survive eviction.
#[test]
fn cold_eviction_preserves_newest_lines() {
    let mut sb = Scrollback::with_block_size(5, 10, 10_000_000, 5);

    for i in 0..50 {
        sb.push_str(&format!("Preserve-line-{i}"));
    }

    // Reduce budget to force cold eviction of oldest data
    sb.set_memory_budget(1)
        .expect("memory budget reduction should succeed");

    let count = sb.line_count();
    assert!(count > 0, "should have at least hot tier lines");

    // The newest line should still be accessible (it's in the hot tier)
    let newest = sb
        .get_line_rev(0)
        .expect("no error")
        .expect("newest line present");
    assert_eq!(
        newest.to_string(),
        "Preserve-line-49",
        "newest line must survive cold eviction"
    );
}
