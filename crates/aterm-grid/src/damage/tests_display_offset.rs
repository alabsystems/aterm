// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for display-offset damage computation (#6072).

use super::Damage;
use super::display_offset::{DisplayOffsetDamage, compute_display_offset_damage};

#[test]
fn compute_display_offset_damage_rows() {
    // (label, old offset, new offset, visible rows, expected damage).
    // Each row was its own test.
    let rows = [
        ("offset unchanged", 5, 5, 24, DisplayOffsetDamage::None),
        ("both offsets zero", 0, 0, 24, DisplayOffsetDamage::None),
        ("delta exceeds rows", 0, 30, 24, DisplayOffsetDamage::Full),
        ("delta equals rows", 0, 24, 24, DisplayOffsetDamage::Full),
        // Scrolling up: the top rows are new from scrollback.
        ("scroll up by 5", 0, 5, 24, DisplayOffsetDamage::TopRows(5)),
        // Scrolling down: the bottom rows are new live content.
        (
            "scroll down 10 -> 3",
            10,
            3,
            24,
            DisplayOffsetDamage::BottomRows { start: 17, end: 24 },
        ),
        (
            "reset 5 -> 0",
            5,
            0,
            24,
            DisplayOffsetDamage::BottomRows { start: 19, end: 24 },
        ),
        // visible_rows - 1 is still partial, not full.
        (
            "delta = rows - 1",
            0,
            23,
            24,
            DisplayOffsetDamage::TopRows(23),
        ),
        // Zero visible rows: any non-zero delta is full.
        ("zero visible rows", 0, 1, 0, DisplayOffsetDamage::Full),
        ("large scroll down", 100, 0, 24, DisplayOffsetDamage::Full),
    ];
    for (label, old, new, visible, want) in rows {
        assert_eq!(
            compute_display_offset_damage(old, new, visible),
            want,
            "{label}"
        );
    }
}

#[test]
fn test_apply_none_does_nothing() {
    let mut damage = Damage::new(24);
    damage.apply_display_offset_damage(DisplayOffsetDamage::None);
    assert!(!damage.has_damage());
}

#[test]
fn test_apply_full_marks_full() {
    let mut damage = Damage::new(24);
    damage.apply_display_offset_damage(DisplayOffsetDamage::Full);
    assert!(damage.is_full());
}

#[test]
fn test_apply_top_rows_marks_correct_rows() {
    let mut damage = Damage::new(24);
    damage.apply_display_offset_damage(DisplayOffsetDamage::TopRows(3));
    assert!(damage.is_row_damaged(0));
    assert!(damage.is_row_damaged(1));
    assert!(damage.is_row_damaged(2));
    assert!(!damage.is_row_damaged(3));
}

#[test]
fn test_apply_bottom_rows_marks_correct_rows() {
    let mut damage = Damage::new(24);
    damage.apply_display_offset_damage(DisplayOffsetDamage::BottomRows { start: 20, end: 24 });
    assert!(!damage.is_row_damaged(19));
    assert!(damage.is_row_damaged(20));
    assert!(damage.is_row_damaged(23));
}
