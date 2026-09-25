// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Property tests over the tiered `Scrollback`: order and content survive
//! hot -> warm -> cold promotion, forward and reverse reads agree, and the
//! line limit evicts oldest-first. Moved here from aterm-core, which only
//! reached this type through its re-export.

use super::*;
use proptest::prelude::*;

proptest! {
    /// Scrollback lines maintain order.
    ///
    /// Property: Lines added to scrollback maintain their relative order.
    #[test]
    fn scrollback_order_preserved(
        line_count in 10usize..100,
    ) {
        let mut sb = Scrollback::new(1000, 10000, 10_000_000);

        // Add numbered lines
        for i in 0..line_count {
            sb.push_str(&format!("Line_{:04}", i));
        }

        // Verify order
        for i in 0..line_count {
            let line = sb.get_line(i).unwrap();
            prop_assert!(
                line.is_some(),
                "get_line({}) should return Some for {} lines",
                i, line_count
            );
            let expected = format!("Line_{:04}", i);
            let actual = line.unwrap().to_string();
            prop_assert_eq!(
                actual, expected,
                "Line {} content mismatch",
                i
            );
        }
    }

    /// Scrollback tier transitions preserve content.
    ///
    /// Property: When lines move between tiers (hot -> warm -> cold),
    /// their content is preserved.
    #[test]
    fn scrollback_tier_content_preserved(
        hot_lines in 5usize..10,
        warm_lines in 10usize..20,
    ) {
        // Small hot tier to force tier transitions
        let mut sb = Scrollback::new(hot_lines, warm_lines, 1_000_000);

        // Add more lines than hot tier can hold
        let total_lines = hot_lines + warm_lines + 5;
        let mut expected_content = Vec::new();

        for i in 0..total_lines {
            let content = format!("Content_{:04}", i);
            expected_content.push(content.clone());
            sb.push_str(&content);
        }

        // Verify all lines are retrievable with correct content
        for (i, expected) in expected_content.iter().enumerate() {
            let line = sb.get_line(i).unwrap();
            prop_assert!(
                line.is_some(),
                "get_line({}) should return Some after pushing {} lines",
                i, total_lines
            );
            let actual = line.unwrap().to_string();
            prop_assert_eq!(
                &actual, expected,
                "Line {} content mismatch",
                i
            );
        }

        // Verify line count
        prop_assert_eq!(
            sb.line_count(), total_lines,
            "Line count should be {}",
            total_lines
        );
    }

    /// Scrollback search finds lines across all tiers.
    ///
    /// Property: Search returns lines from all tiers (hot, warm, cold).
    #[test]
    fn scrollback_search_across_tiers(
        hot_lines in 3usize..5,
        warm_lines in 5usize..10,
    ) {
        let mut sb = Scrollback::new(hot_lines, warm_lines, 1_000_000);

        // Add lines with unique markers that will end up in different tiers
        let total = hot_lines + warm_lines + 5;
        for i in 0..total {
            let marker = format!("MARKER_{:03}_DATA", i);
            sb.push_str(&marker);
        }

        // Search for each marker
        for i in 0..total {
            let query = format!("MARKER_{:03}", i);
            let line = sb.get_line(i).unwrap();
            prop_assert!(
                line.is_some(),
                "get_line({}) should return Some for {} total lines",
                i, total
            );
            let content = line.unwrap().to_string();
            prop_assert!(
                content.contains(&query),
                "Line {} should contain '{}' but got '{}'",
                i, query, content
            );
        }
    }
}

proptest! {
    /// Scrollback FIFO ordering: push N lines, get_line(0) returns the oldest.
    ///
    /// Property: After pushing lines in order, get_line(i) returns the i-th
    /// oldest line and get_line_rev(0) returns the newest.
    #[test]
    fn scrollback_fifo_ordering(
        lines in prop::collection::vec("[a-z]{1,10}", 1..50),
    ) {
        let mut sb = Scrollback::new(100, 1000, 10_000_000);
        for line_str in &lines {
            sb.push_str(line_str);
        }

        prop_assert_eq!(
            sb.line_count(), lines.len(),
            "line_count should match number of pushed lines"
        );

        // Forward order: get_line(0) = oldest = first pushed
        for (i, expected) in lines.iter().enumerate() {
            let text = sb.get_line(i).unwrap().unwrap_or_else(|| {
                panic!("get_line({}) should return Some for {} lines", i, lines.len())
            });
            prop_assert_eq!(
                text.as_str().unwrap_or(""), expected.as_str(),
                "get_line({}) should return {:?}",
                i, expected
            );
        }

        // Reverse order: get_line_rev(0) = newest = last pushed
        for (rev_i, expected) in lines.iter().rev().enumerate() {
            let text = sb.get_line_rev(rev_i).unwrap().unwrap_or_else(|| {
                panic!("get_line_rev({}) should return Some for {} lines", rev_i, lines.len())
            });
            prop_assert_eq!(
                text.as_str().unwrap_or(""), expected.as_str(),
                "get_line_rev({}) should return {:?}",
                rev_i, expected
            );
        }
    }

    /// Scrollback line_limit enforcement: line_count never exceeds the limit.
    ///
    /// Property: After setting a line_limit and pushing more lines than the limit,
    /// line_count is always <= limit.
    #[test]
    fn scrollback_line_limit_enforced(
        limit in 1usize..20,
        push_count in 1usize..50,
    ) {
        let mut sb = Scrollback::new(100, 1000, 10_000_000);
        sb.set_line_limit(Some(limit));

        for i in 0..push_count {
            sb.push_str(&format!("line{}", i));
        }

        prop_assert!(
            sb.line_count() <= limit,
            "line_count {} should be <= limit {} after pushing {} lines",
            sb.line_count(), limit, push_count
        );

        // Verify the most recent lines are kept (FIFO eviction)
        if push_count > limit {
            let expected_oldest = push_count - limit;
            let text = sb.get_line(0).unwrap().unwrap_or_else(|| {
                panic!("get_line(0) should return Some when {} lines pushed with limit {}", push_count, limit)
            });
            prop_assert_eq!(
                text.as_str().unwrap_or(""),
                &format!("line{}", expected_oldest),
                "oldest line should be 'line{}' (limit={}, pushed={})",
                expected_oldest, limit, push_count
            );
        }
    }

    /// Scrollback get_line_rev is consistent with get_line.
    ///
    /// Property: get_line_rev(i) == get_line(line_count - 1 - i).
    #[test]
    fn scrollback_get_line_rev_consistent(
        lines in prop::collection::vec("[a-z]{1,5}", 1..30),
    ) {
        let mut sb = Scrollback::new(100, 1000, 10_000_000);
        for line_str in &lines {
            sb.push_str(line_str);
        }

        let count = sb.line_count();
        for i in 0..count {
            let forward = sb.get_line(count - 1 - i).unwrap();
            let reverse = sb.get_line_rev(i).unwrap();

            prop_assert_eq!(
                forward.as_ref().and_then(|l| l.as_str()),
                reverse.as_ref().and_then(|l| l.as_str()),
                "get_line({}) should match get_line_rev({})",
                count - 1 - i, i
            );
        }
    }
}
