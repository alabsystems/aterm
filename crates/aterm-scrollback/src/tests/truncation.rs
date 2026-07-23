// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Truncate and remove_newest operations including cross-tier slow paths.

use super::*;

#[test]
fn scrollback_truncate() {
    let mut sb = Scrollback::new(100, 1000, 10_000_000);
    for i in 0..50 {
        sb.push_str(&format!("Line {i}"));
    }

    sb.truncate(10).expect("truncate should succeed");
    assert_eq!(sb.line_count(), 10);

    // Should keep the last 10 lines
    assert_eq!(
        sb.get_line(0)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Line 40"
    );
    assert_eq!(
        sb.get_line(9)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Line 49"
    );
}

/// Test truncate when lines span multiple tiers.
///
/// Regression test for #1693: truncate() was clearing cold/warm tiers
/// unconditionally, losing data when n exceeded hot tier size.
#[test]
fn scrollback_truncate_cross_tier() {
    // Small limits to force tier transitions
    let mut sb = Scrollback::with_block_size(5, 10, 10_000_000, 5);

    // Push 25 lines - spreads across cold (5), warm (10), hot (10)
    for i in 0..25 {
        sb.push_str(&format!("Line {i}"));
    }

    // Verify lines are in multiple tiers
    assert!(sb.cold_line_count() > 0, "expected cold tier data");
    assert!(sb.warm_line_count() > 0, "expected warm tier data");
    let hot_count = sb.hot_line_count();
    assert!(hot_count > 0 && hot_count < 25, "expected partial hot tier");

    // Truncate to keep more lines than hot tier has
    // Bug: old code would clear cold/warm and only keep hot lines
    let keep = 15;
    assert!(keep > hot_count, "test requires n > hot_line_count");

    sb.truncate(keep).expect("truncate should succeed");
    assert_eq!(sb.line_count(), keep, "line count mismatch after truncate");

    // Verify we kept the LAST 15 lines (10-24)
    assert_eq!(
        sb.get_line(0)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Line 10"
    );
    assert_eq!(
        sb.get_line(14)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Line 24"
    );
}

/// Test truncate with n=0 clears all tiers.
#[test]
fn scrollback_truncate_zero() {
    let mut sb = Scrollback::with_block_size(5, 10, 10_000_000, 5);
    for i in 0..25 {
        sb.push_str(&format!("Line {i}"));
    }

    sb.truncate(0).expect("truncate should succeed");
    assert_eq!(sb.line_count(), 0);
    assert_eq!(sb.hot_line_count(), 0);
    assert_eq!(sb.warm_line_count(), 0);
    assert_eq!(sb.cold_line_count(), 0);
}

/// Bug: `remove_newest` slow path dumped all surviving lines into hot tier,
/// bypassing tier promotion and violating `hot_limit`. After the fix,
/// the slow path re-promotes excess hot lines to warm/cold.
#[test]
fn remove_newest_slow_path_restores_tier_structure() {
    let mut sb = Scrollback::with_block_size(5, 10, 10_000_000, 5);

    // Push 30 lines — spreads across cold, warm, hot.
    for i in 0..30 {
        sb.push_str(&format!("Line {i:02}"));
    }
    assert_eq!(sb.line_count(), 30);
    let hot_before = sb.hot_line_count();
    assert!(
        hot_before <= sb.hot_limit(),
        "hot tier should respect limit before operation"
    );

    // Remove more than hot tier contains → triggers slow path.
    let remove = hot_before + 1;
    sb.remove_newest(remove)
        .expect("remove_newest should succeed");

    let expected = 30 - remove;
    assert_eq!(sb.line_count(), expected, "total line count after remove");

    // The bug: all surviving lines were dumped into hot tier, exceeding hot_limit.
    assert!(
        sb.hot_line_count() < sb.hot_limit(),
        "hot tier ({}) must stay under hot_limit ({}) after slow-path rebuild",
        sb.hot_line_count(),
        sb.hot_limit()
    );

    // Verify all remaining lines are accessible and have correct content.
    for i in 0..expected {
        let line = sb
            .get_line(i)
            .expect("no decompression error")
            .unwrap_or_else(|| panic!("line {i} should exist"));
        assert_eq!(line.to_string(), format!("Line {i:02}"));
    }
}

/// Tier-aware truncate preserves tier structure: only removes lines from the
/// oldest tier(s) without rebuilding the entire scrollback. Hot tier lines
/// remain untouched when removals happen in cold/warm.
#[test]
fn truncate_slow_path_restores_tier_structure() {
    let mut sb = Scrollback::with_block_size(5, 10, 10_000_000, 5);

    for i in 0..30 {
        sb.push_str(&format!("Line {i:02}"));
    }
    assert_eq!(sb.line_count(), 30);

    // Keep more lines than hot tier has → triggers cross-tier truncate.
    let keep = 20;
    assert!(
        keep > sb.hot_line_count(),
        "test requires keep > hot_line_count to trigger cross-tier path"
    );

    sb.truncate(keep).expect("truncate should succeed");
    assert_eq!(sb.line_count(), keep, "total line count after truncate");

    // Hot tier stays at or below the promotion threshold (tier-aware truncate
    // only removes from cold/warm when those tiers contain the oldest lines).
    assert!(
        sb.hot_line_count() <= sb.hot_limit(),
        "hot tier ({}) must stay at or below hot_limit ({}) after truncate",
        sb.hot_line_count(),
        sb.hot_limit()
    );

    // Verify the kept lines are the newest 20 (lines 10-29).
    for i in 0..keep {
        let line = sb
            .get_line(i)
            .expect("no decompression error")
            .unwrap_or_else(|| panic!("line {i} should exist"));
        let expected_idx = 30 - keep + i;
        assert_eq!(line.to_string(), format!("Line {expected_idx:02}"));
    }
}
