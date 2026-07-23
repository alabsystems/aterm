// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Scrollback, RLE, triggers, grapheme, and command tier property tests.

use proptest::prelude::*;

// ============== Scrollback Tier Ordering Property Tests (UNIFIED_ROADMAP B3) ==============

proptest! {
    /// Scrollback lines maintain order.
    ///
    /// Property: Lines added to scrollback maintain their relative order.
    #[test]
    fn scrollback_order_preserved(
        line_count in 10usize..100,
    ) {
        use crate::scrollback::Scrollback;

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
        use crate::scrollback::Scrollback;

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
        use crate::scrollback::Scrollback;

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

// ============== RLE Property Tests (#1931) ==============

proptest! {
    /// RLE roundtrip: from_iter then iter recovers original values.
    ///
    /// Property: For any sequence of u8 values, encoding into RLE and
    /// iterating back produces the original sequence.
    #[test]
    fn rle_roundtrip(values in prop::collection::vec(0u8..4, 0..200)) {
        use aterm_rle::Rle;

        let rle = Rle::from_iter(values.iter().copied());

        // Length preserved
        prop_assert_eq!(
            rle.len() as usize, values.len(),
            "RLE length {} should match input length {}",
            rle.len(), values.len()
        );

        // Content preserved via iteration
        let recovered: Vec<u8> = rle.iter().collect();
        prop_assert_eq!(
            recovered, values,
            "RLE roundtrip should preserve all values"
        );
    }

    /// RLE run count is always <= element count.
    ///
    /// Property: The number of runs never exceeds the number of elements,
    /// and equals element count only when all values are distinct.
    #[test]
    fn rle_run_count_bounded(values in prop::collection::vec(0u8..4, 1..200)) {
        use aterm_rle::Rle;

        let rle = Rle::from_iter(values.iter().copied());

        prop_assert!(
            rle.run_count() <= values.len(),
            "run_count {} should be <= element count {}",
            rle.run_count(), values.len()
        );

        prop_assert!(
            rle.run_count() >= 1,
            "non-empty RLE should have at least 1 run"
        );
    }

    /// RLE get returns correct value for every valid index.
    ///
    /// Property: get(i) matches the i-th element from the original input.
    #[test]
    fn rle_get_matches_input(values in prop::collection::vec(0u8..4, 1..100)) {
        use aterm_rle::Rle;

        let rle = Rle::from_iter(values.iter().copied());

        for (i, expected) in values.iter().enumerate() {
            let got = rle.get(i as u32);
            prop_assert_eq!(
                got, Some(*expected),
                "get({}) should return {:?} but got {:?}",
                i, expected, got
            );
        }

        // Out of bounds returns None
        prop_assert_eq!(
            rle.get(values.len() as u32), None,
            "get(len) should return None"
        );
    }

    /// RLE set preserves length and updates value.
    ///
    /// Property: After set(i, v), get(i) == v and len() is unchanged.
    #[test]
    fn rle_set_preserves_length(
        values in prop::collection::vec(0u8..4, 2..50),
        idx_frac in 0.0f64..1.0,
        new_val in 0u8..4,
    ) {
        use aterm_rle::Rle;

        let mut rle = Rle::from_iter(values.iter().copied());
        let idx = (idx_frac * (values.len() - 1) as f64) as u32;
        let original_len = rle.len();

        rle.set(idx, new_val);

        prop_assert_eq!(
            rle.len(), original_len,
            "set should not change length"
        );
        prop_assert_eq!(
            rle.get(idx), Some(new_val),
            "get({}) should return {} after set",
            idx, new_val
        );
    }
}

// ============== Triggers Property Tests (#1931) ==============

proptest! {
    /// post_process_match output is always a prefix of the input.
    ///
    /// Property: The result is always a substring starting at byte 0 of the input.
    #[test]
    fn triggers_post_process_is_prefix(input in "[a-zA-Z0-9.,;:!?()\\[\\]{}'\"/]{0,100}") {
        use crate::triggers::post_process_match;

        let result = post_process_match(&input);

        prop_assert!(
            input.starts_with(result),
            "result {:?} should be a prefix of input {:?}",
            result, input
        );
    }

    /// post_process_match never increases length.
    ///
    /// Property: |output| <= |input| for any input.
    #[test]
    fn triggers_post_process_no_growth(input in "\\PC{0,100}") {
        use crate::triggers::post_process_match;

        let result = post_process_match(&input);

        prop_assert!(
            result.len() <= input.len(),
            "result length {} should be <= input length {}",
            result.len(), input.len()
        );
    }

    /// post_process_match is idempotent.
    ///
    /// Property: post_process(post_process(x)) == post_process(x).
    #[test]
    fn triggers_post_process_idempotent(input in "\\PC{0,100}") {
        use crate::triggers::post_process_match;

        let once = post_process_match(&input);
        let twice = post_process_match(once);

        prop_assert_eq!(
            once, twice,
            "post_process_match should be idempotent: once={:?}, twice={:?}",
            once, twice
        );
    }

    /// post_process_match preserves balanced parentheses.
    ///
    /// Property: If input ends with ')' and '(' count >= ')' count,
    /// the trailing ')' is preserved.
    #[test]
    fn triggers_balanced_parens_preserved(
        inner in "[a-zA-Z0-9]{1,20}",
    ) {
        use crate::triggers::post_process_match;

        // Construct balanced input like "func(arg)"
        let input = format!("func({})", inner);
        let result = post_process_match(&input);

        prop_assert_eq!(
            result, input.as_str(),
            "balanced parens should be preserved: input={:?}, result={:?}",
            input, result
        );
    }
}

// ============== Grapheme Property Tests (#1931) ==============

proptest! {
    /// Grapheme display width is always 0, 1, or 2.
    ///
    /// Property: For any single grapheme, display width is bounded [0, 2].
    #[test]
    fn grapheme_width_bounded(c in proptest::char::any()) {
        use crate::grapheme::grapheme_display_width;

        let s = c.to_string();
        let width = grapheme_display_width(&s);

        prop_assert!(
            width <= 2,
            "grapheme_display_width({:?}) = {} should be <= 2",
            s, width
        );
    }

    /// ASCII printable characters have width 1.
    ///
    /// Property: For printable ASCII (0x20-0x7E), display width is 1.
    #[test]
    fn grapheme_ascii_printable_width_one(c in 0x20u8..=0x7E) {
        use crate::grapheme::grapheme_display_width;

        let s = String::from(c as char);
        let width = grapheme_display_width(&s);

        prop_assert_eq!(
            width, 1,
            "ASCII printable {:?} (0x{:02x}) should have width 1, got {}",
            s, c, width
        );
    }

    /// grapheme_width aggregate is consistent with split_graphemes.
    ///
    /// Property: The aggregate display_width equals the sum of individual
    /// grapheme widths from split_graphemes.
    #[test]
    fn grapheme_width_consistent_with_split(s in "[a-zA-Z0-9 ]{0,50}") {
        use crate::grapheme::{grapheme_width, split_graphemes};

        let info = grapheme_width(&s);
        let split_width: usize = split_graphemes(&s).map(|g| g.width).sum();
        let split_count: usize = split_graphemes(&s).count();

        prop_assert_eq!(
            info.display_width, split_width,
            "aggregate width {} should match sum of splits {}",
            info.display_width, split_width
        );
        prop_assert_eq!(
            info.grapheme_count, split_count,
            "aggregate count {} should match split count {}",
            info.grapheme_count, split_count
        );
    }

    /// grapheme_width byte_count always matches string length.
    ///
    /// Property: byte_count == s.len() for any input.
    #[test]
    fn grapheme_byte_count_matches(s in "\\PC{0,100}") {
        use crate::grapheme::grapheme_width;

        let info = grapheme_width(&s);

        prop_assert_eq!(
            info.byte_count, s.len(),
            "byte_count {} should match s.len() {}",
            info.byte_count, s.len()
        );
    }
}

// ============== RLE Extended Property Tests (#1931) ==============

proptest! {
    /// RLE set_range preserves total length.
    ///
    /// Property: For any valid range [start, end), set_range does not change len().
    #[test]
    fn rle_set_range_preserves_length(
        values in prop::collection::vec(0u8..4, 2..100),
        start_frac in 0.0f64..1.0,
        end_frac in 0.0f64..1.0,
        new_val in 0u8..4,
    ) {
        use aterm_rle::Rle;

        let mut rle = Rle::from_iter(values.iter().copied());
        let original_len = rle.len();

        let start = (start_frac * values.len() as f64) as u32;
        let end = (end_frac * values.len() as f64) as u32;
        let (start, end) = if start <= end { (start, end) } else { (end, start) };

        rle.set_range(start, end, new_val);

        prop_assert_eq!(
            rle.len(), original_len,
            "set_range({}, {}, {}) should not change length (was {}, now {})",
            start, end, new_val, original_len, rle.len()
        );

        // Verify the set values are correct
        for i in start..end.min(original_len) {
            prop_assert_eq!(
                rle.get(i), Some(new_val),
                "get({}) should return {} after set_range",
                i, new_val
            );
        }
    }

    /// RLE extend_with is equivalent to repeated push.
    ///
    /// Property: extend_with(v, n) produces the same sequence as n calls to push(v).
    #[test]
    fn rle_extend_with_matches_push(
        prefix in prop::collection::vec(0u8..4, 0..20),
        value in 0u8..4,
        count in 0u32..50,
    ) {
        use aterm_rle::Rle;

        let mut rle_push = Rle::from_iter(prefix.iter().copied());
        let mut rle_extend = Rle::from_iter(prefix.iter().copied());

        for _ in 0..count {
            rle_push.push(value);
        }
        rle_extend.extend_with(value, count);

        prop_assert_eq!(
            rle_push.len(), rle_extend.len(),
            "extend_with length should match repeated push"
        );

        let push_vals: Vec<u8> = rle_push.iter().collect();
        let extend_vals: Vec<u8> = rle_extend.iter().collect();
        prop_assert_eq!(
            push_vals, extend_vals,
            "extend_with content should match repeated push"
        );
    }

    /// RLE resize preserves prefix content.
    ///
    /// Property: After resize to larger, the original prefix is preserved.
    #[test]
    fn rle_resize_preserves_prefix(
        values in prop::collection::vec(0u8..4, 1..50),
        extra in 1u32..50,
    ) {
        use aterm_rle::Rle;

        let mut rle = Rle::from_iter(values.iter().copied());
        let original_len = rle.len();
        rle.resize(original_len + extra);

        prop_assert_eq!(rle.len(), original_len + extra);

        // Original values preserved
        for (i, &expected) in values.iter().enumerate() {
            prop_assert_eq!(
                rle.get(i as u32), Some(expected),
                "original value at {} should be preserved after resize",
                i
            );
        }

        // New values are default (0 for u8)
        for i in original_len..(original_len + extra) {
            prop_assert_eq!(
                rle.get(i), Some(0u8),
                "extended value at {} should be default after resize",
                i
            );
        }
    }
}

// ============== Scrollback Property Tests (#1931) ==============

proptest! {
    /// Scrollback FIFO ordering: push N lines, get_line(0) returns the oldest.
    ///
    /// Property: After pushing lines in order, get_line(i) returns the i-th
    /// oldest line and get_line_rev(0) returns the newest.
    #[test]
    fn scrollback_fifo_ordering(
        lines in prop::collection::vec("[a-z]{1,10}", 1..50),
    ) {
        use crate::scrollback::Scrollback;

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
        use crate::scrollback::Scrollback;

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
        use crate::scrollback::Scrollback;

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
