// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

fn assert_line_text(sb: &Scrollback, idx: usize, expected: &str, context: &str) {
    let line = sb
        .get_line(idx)
        .unwrap_or_else(|e| panic!("{context}: get_line({idx}) failed: {e}"))
        .unwrap_or_else(|| panic!("{context}: line {idx} missing"));
    assert_eq!(line.to_string(), expected, "{context}: wrong text at {idx}");
}

/// Regression for the warm front_offset path: if a trimmed corrupt warm block is
/// later repaired, reads and warm->cold eviction must preserve only the
/// surviving logical suffix instead of resurrecting the consumed prefix.
#[test]
fn repaired_trimmed_warm_block_keeps_logical_suffix() {
    let mut sb = Scrollback::with_block_size(5, 10, 10_000_000, 5);

    for i in 0..15 {
        sb.push_str(&format!("Line {i}"));
    }

    sb.truncate(12).expect("truncate should succeed");
    let original = sb
        .warm
        .oldest_block_bytes()
        .expect("trimmed warm block should exist");
    sb.warm.corrupt_oldest_block();

    for i in 15..20 {
        sb.push_str(&format!("Line {i}"));
    }

    sb.warm.restore_oldest_block(original);
    let restored = sb
        .warm
        .blocks_mut()
        .front()
        .cloned()
        .expect("restored trimmed block should still be first");
    assert_eq!(
        restored.line_count(),
        2,
        "only the surviving suffix should remain"
    );
    let restored_first = restored
        .get_line(0)
        .expect("block should decompress")
        .expect("first surviving line should exist");
    let restored_second = restored
        .get_line(1)
        .expect("block should keep the second surviving line")
        .expect("second surviving line should exist");
    assert_eq!(restored_first.to_string(), "Line 3");
    assert_eq!(restored_second.to_string(), "Line 4");
    assert_line_text(&sb, 0, "Line 3", "warm read after repair");
    assert_line_text(&sb, 1, "Line 4", "warm read after repair");

    for i in 20..25 {
        sb.push_str(&format!("Line {i}"));
    }

    assert!(
        sb.cold_line_count() >= 2,
        "restored trimmed block should evict into cold once it decodes again"
    );
    assert_line_text(&sb, 0, "Line 3", "cold read after eviction");
    assert_line_text(&sb, 1, "Line 4", "cold read after eviction");
}
