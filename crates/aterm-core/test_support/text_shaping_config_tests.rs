// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn test_ambiguous_width_default() {
    let config = TextShapingConfig::default();
    assert_eq!(config.ambiguous_char_width(), 1);
}

#[test]
fn test_ambiguous_width_double() {
    let config = TextShapingConfig {
        ambiguous_width: AmbiguousWidth::Double,
        ..Default::default()
    };
    assert_eq!(config.ambiguous_char_width(), 2);
}

#[test]
fn test_ligature_mode_enabled() {
    let config = TextShapingConfig {
        ligature_mode: LigatureMode::Enabled,
        ..Default::default()
    };
    assert!(!config.should_disable_ligatures(Some((0, 1)), 0, 0, 2));
    assert!(!config.should_disable_ligatures(None, 0, 0, 2));
}

#[test]
fn test_ligature_mode_disabled() {
    let config = TextShapingConfig {
        ligature_mode: LigatureMode::Disabled,
        ..Default::default()
    };
    assert!(config.should_disable_ligatures(Some((0, 1)), 0, 0, 2));
    assert!(config.should_disable_ligatures(None, 0, 0, 2));
}

#[test]
fn test_ligature_mode_cursor_disabled() {
    let config = TextShapingConfig {
        ligature_mode: LigatureMode::CursorDisabled,
        ..Default::default()
    };

    // Cursor at row 0 col 1, glyph spans cols 0-2 → overlaps
    assert!(config.should_disable_ligatures(Some((0, 1)), 0, 0, 2));

    // Cursor at row 0 col 5, glyph spans cols 0-2 → no overlap
    assert!(!config.should_disable_ligatures(Some((0, 5)), 0, 0, 2));

    // Cursor at row 1, glyph on row 0 → different row
    assert!(!config.should_disable_ligatures(Some((1, 1)), 0, 0, 2));

    // No cursor visible → ligatures enabled
    assert!(!config.should_disable_ligatures(None, 0, 0, 2));

    // Cursor at boundary (col 0, start of glyph)
    assert!(config.should_disable_ligatures(Some((0, 0)), 0, 0, 2));

    // Cursor at boundary (col 2, end exclusive)
    assert!(!config.should_disable_ligatures(Some((0, 2)), 0, 0, 2));
}

// NOTE: the string→`FontFeature` parser and `FontFeature::new` are exercised in
// `aterm_types::text_shaping` tests (the canonical home of the parser); this file
// covers the aterm-core-facing config types only.

#[test]
fn test_ligature_mode_discriminant() {
    assert_eq!(LigatureMode::Enabled as u8, 0);
    assert_eq!(LigatureMode::CursorDisabled as u8, 1);
    assert_eq!(LigatureMode::Disabled as u8, 2);
}

#[test]
fn test_ambiguous_width_discriminant() {
    assert_eq!(AmbiguousWidth::Single as u8, 0);
    assert_eq!(AmbiguousWidth::Double as u8, 1);
}
