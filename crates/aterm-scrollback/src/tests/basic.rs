// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Core scrollback CRUD: create, push, get, iterate, clear, promotion, eviction.

use super::*;

#[test]
fn scrollback_promotion() {
    // Small limits to trigger promotion
    let mut sb = Scrollback::with_block_size(10, 100, 10_000_000, 5);

    // Push 15 lines - should promote 5 to warm
    for i in 0..15 {
        sb.push_str(&format!("Line {i}"));
    }

    assert_eq!(sb.line_count(), 15);
    assert_eq!(sb.hot_line_count(), 10);
    assert_eq!(sb.warm_line_count(), 5);

    // Verify we can still read all lines
    for i in 0..15 {
        let line = sb.get_line(i).expect("no error").expect("line present");
        assert_eq!(line.to_string(), format!("Line {i}"));
    }
}

#[test]
fn scrollback_clear() {
    let mut sb = Scrollback::new(100, 1000, 10_000_000);
    for i in 0..50 {
        sb.push_str(&format!("Line {i}"));
    }
    assert_eq!(sb.line_count(), 50);

    sb.clear();
    assert_eq!(sb.line_count(), 0);
    assert_eq!(sb.hot_line_count(), 0);
    assert_eq!(sb.warm_line_count(), 0);
    assert_eq!(sb.cold_line_count(), 0);
}

/// Content survives many hot→warm→cold cycles: lines of varied length read back
/// exactly from whichever tier they end in, forward, reverse, random-access and
/// through the iterator.
#[test]
fn tier_transition_content_fidelity_multi_cycle() {
    // hot=3, warm=6, block_size=3: two warm blocks before eviction to cold.
    let mut sb = Scrollback::with_block_size(3, 6, 10_000_000, 3);

    let total_lines = 60;
    let mut expected: Vec<String> = Vec::with_capacity(total_lines);

    for i in 0..total_lines {
        // Vary content length: short, medium, long, and with special chars
        let content = match i % 4 {
            0 => format!("L{i}"),
            1 => format!("Line {i}: {}", "x".repeat(50)),
            2 => format!("Line {i}: mixed \t\n special \x1b[31m chars"),
            _ => format!("Line {i}: unicode — «café» — 日本語テスト"),
        };
        expected.push(content.clone());
        sb.push_str(&content);
    }

    assert_eq!(sb.line_count(), total_lines);
    assert!(sb.cold_line_count() > 0, "expected data in cold tier");
    assert!(sb.warm_line_count() > 0, "expected data in warm tier");
    assert!(sb.hot_line_count() > 0, "expected data in hot tier");

    // Forward read: verify every line from oldest to newest
    for (i, expected_content) in expected.iter().enumerate() {
        let line = sb
            .get_line(i)
            .unwrap_or_else(|e| panic!("get_line({i}) error: {e}"))
            .unwrap_or_else(|| panic!("get_line({i}) returned None"));
        assert_eq!(
            line.to_string(),
            *expected_content,
            "content mismatch at forward index {i}"
        );
    }

    // Reverse read: verify every line from newest to oldest
    // (exercises different decompression cache access patterns)
    for rev_idx in 0..total_lines {
        let expected_idx = total_lines - 1 - rev_idx;
        let line = sb
            .get_line_rev(rev_idx)
            .unwrap_or_else(|e| panic!("get_line_rev({rev_idx}) error: {e}"))
            .unwrap_or_else(|| panic!("get_line_rev({rev_idx}) returned None"));
        assert_eq!(
            line.to_string(),
            expected[expected_idx],
            "content mismatch at reverse index {rev_idx} (forward {expected_idx})"
        );
    }

    // Random-access pattern: read every 7th line to exercise cache eviction
    for i in (0..total_lines).step_by(7) {
        let line = sb
            .get_line(i)
            .unwrap_or_else(|e| panic!("random get_line({i}) error: {e}"))
            .unwrap_or_else(|| panic!("random get_line({i}) returned None"));
        assert_eq!(
            line.to_string(),
            expected[i],
            "content mismatch at random-access index {i}"
        );
    }

    // Iterator: verify full iteration matches
    let iterated: Vec<String> = sb.iter().map(|l| l.to_string()).collect();
    assert_eq!(iterated.len(), total_lines, "iterator length mismatch");
    for (i, (got, want)) in iterated.iter().zip(expected.iter()).enumerate() {
        assert_eq!(got, want, "iterator content mismatch at index {i}");
    }
}
