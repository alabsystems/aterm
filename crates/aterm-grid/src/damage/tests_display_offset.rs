// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for display-offset damage computation (#6072).

use super::Damage;
use super::display_offset::{DisplayOffsetDamage, compute_display_offset_damage};

#[test]
fn test_no_damage_when_offset_unchanged() {
    let result = compute_display_offset_damage(5, 5, 24);
    assert_eq!(result, DisplayOffsetDamage::None);
}

#[test]
fn test_no_damage_zero_offsets() {
    let result = compute_display_offset_damage(0, 0, 24);
    assert_eq!(result, DisplayOffsetDamage::None);
}

#[test]
fn test_full_damage_when_delta_exceeds_rows() {
    let result = compute_display_offset_damage(0, 30, 24);
    assert_eq!(result, DisplayOffsetDamage::Full);
}

#[test]
fn test_full_damage_when_delta_equals_rows() {
    let result = compute_display_offset_damage(0, 24, 24);
    assert_eq!(result, DisplayOffsetDamage::Full);
}

#[test]
fn test_top_rows_on_scroll_up() {
    // Scrolling up: new_offset > old_offset → top rows are new from scrollback.
    let result = compute_display_offset_damage(0, 5, 24);
    assert_eq!(result, DisplayOffsetDamage::TopRows(5));
}

#[test]
fn test_bottom_rows_on_scroll_down() {
    // Scrolling down: new_offset < old_offset → bottom rows are new live content.
    let result = compute_display_offset_damage(10, 3, 24);
    assert_eq!(
        result,
        DisplayOffsetDamage::BottomRows { start: 17, end: 24 }
    );
}

#[test]
fn test_bottom_rows_on_offset_reset() {
    // Reset to 0: old_offset > 0, new_offset = 0 → bottom rows.
    let result = compute_display_offset_damage(5, 0, 24);
    assert_eq!(
        result,
        DisplayOffsetDamage::BottomRows { start: 19, end: 24 }
    );
}

#[test]
fn test_edge_case_delta_equals_rows_minus_one() {
    // Scroll by visible_rows - 1: still partial, not full.
    let result = compute_display_offset_damage(0, 23, 24);
    assert_eq!(result, DisplayOffsetDamage::TopRows(23));
}

#[test]
fn test_zero_visible_rows() {
    // Edge case: zero visible rows → any non-zero delta is full.
    let result = compute_display_offset_damage(0, 1, 0);
    assert_eq!(result, DisplayOffsetDamage::Full);
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

#[test]
fn test_full_damage_on_large_down_scroll() {
    let result = compute_display_offset_damage(100, 0, 24);
    assert_eq!(result, DisplayOffsetDamage::Full);
}
