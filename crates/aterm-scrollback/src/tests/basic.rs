// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Core scrollback CRUD: create, push, get, iterate, clear, promotion, eviction.

use super::*;

#[test]
fn scrollback_new() {
    let sb = Scrollback::new(100, 1000, 10_000_000);
    assert_eq!(sb.line_count(), 0);
    assert_eq!(sb.hot_line_count(), 0);
    assert_eq!(sb.warm_line_count(), 0);
    assert_eq!(sb.cold_line_count(), 0);
}

#[test]
fn scrollback_push_line() {
    let mut sb = Scrollback::new(100, 1000, 10_000_000);
    sb.push_str("Hello");
    sb.push_str("World");

    assert_eq!(sb.line_count(), 2);
    assert_eq!(sb.hot_line_count(), 2);
}

#[test]
fn scrollback_get_line() {
    let mut sb = Scrollback::new(100, 1000, 10_000_000);
    sb.push_str("Line 0");
    sb.push_str("Line 1");
    sb.push_str("Line 2");

    assert_eq!(
        sb.get_line(0)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Line 0"
    );
    assert_eq!(
        sb.get_line(1)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Line 1"
    );
    assert_eq!(
        sb.get_line(2)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Line 2"
    );
    assert!(sb.get_line(3).expect("no error").is_none());
}

#[test]
fn scrollback_get_line_rev() {
    let mut sb = Scrollback::new(100, 1000, 10_000_000);
    sb.push_str("Line 0");
    sb.push_str("Line 1");
    sb.push_str("Line 2");

    assert_eq!(
        sb.get_line_rev(0)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Line 2"
    );
    assert_eq!(
        sb.get_line_rev(1)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Line 1"
    );
    assert_eq!(
        sb.get_line_rev(2)
            .expect("no error")
            .expect("line present")
            .to_string(),
        "Line 0"
    );
}

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
fn scrollback_eviction() {
    // Small limits to trigger eviction
    let mut sb = Scrollback::with_block_size(5, 10, 10_000_000, 5);

    // Push 25 lines - should evict to cold
    for i in 0..25 {
        sb.push_str(&format!("Line {i}"));
    }

    assert_eq!(sb.line_count(), 25);
    assert!(sb.cold_line_count() > 0);

    // Verify we can still read all lines
    for i in 0..25 {
        let line = sb.get_line(i).expect("no error").expect("line present");
        assert_eq!(line.to_string(), format!("Line {i}"));
    }
}

#[test]
fn scrollback_iterator() {
    let mut sb = Scrollback::new(100, 1000, 10_000_000);
    for i in 0..10 {
        sb.push_str(&format!("Line {i}"));
    }

    let lines: Vec<_> = sb.iter().collect();
    assert_eq!(lines.len(), 10);
    assert_eq!(lines[0].to_string(), "Line 0");
    assert_eq!(lines[9].to_string(), "Line 9");
}

#[test]
fn scrollback_rev_iterator() {
    let mut sb = Scrollback::new(100, 1000, 10_000_000);
    for i in 0..10 {
        sb.push_str(&format!("Line {i}"));
    }

    let lines: Vec<_> = sb.iter_rev().collect();
    assert_eq!(lines.len(), 10);
    assert_eq!(lines[0].to_string(), "Line 9");
    assert_eq!(lines[9].to_string(), "Line 0");
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

/// Verify content fidelity through multiple full tier transition cycles.
///
/// Pushes enough lines to trigger multiple hot→warm→cold eviction cycles,
/// with varied content lengths to exercise different compression ratios.
/// Verifies every line reads back correctly from whichever tier it ended up in.
///
/// This catches data corruption in the LZ4 (warm) and Zstd (cold) codecs,
/// off-by-one errors in tier index arithmetic, and decompression cache
/// coherence issues.
///
/// Part of #5550 (memory safety verification).
#[test]
fn tier_transition_content_fidelity_multi_cycle() {
    // Very small limits: hot=3, warm=6, block_size=3.
    // With block_size=3, each warm block holds 3 lines.
    // warm_limit=6 means 2 blocks before eviction to cold.
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
