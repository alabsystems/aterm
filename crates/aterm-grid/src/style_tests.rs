// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests and test-only helpers for the grid style module.

use super::*;
use crate::PackedColors;

#[test]
fn color_constructors() {
    let c = Color::new(100, 150, 200);
    assert_eq!(c.r, 100);
    assert_eq!(c.g, 150);
    assert_eq!(c.b, 200);
    assert_eq!(c.a, 255);

    let c2 = Color::with_alpha(100, 150, 200, 128);
    assert_eq!(c2.a, 128);
}

#[test]
fn color_from_rgb_tuple() {
    let c = Color::from_rgb((10, 20, 30));
    assert_eq!(c.to_rgb(), (10, 20, 30));
}

#[test]
fn table_default_style() {
    let table = StyleTable::new();
    assert_eq!(table.len(), 1);
    // A table with only the default style is considered "empty" (no user styles)
    assert!(table.is_empty());

    let default = table.get(StyleId::DEFAULT).unwrap();
    assert!(default.is_default());
}

#[test]
fn extended_style_roundtrip() {
    let colors = PackedColors::with_indexed(100, 200);
    let flags = CellFlags::BOLD
        .union(CellFlags::UNDERLINE)
        .union(CellFlags::STRIKETHROUGH);

    let ext = ExtendedStyle::from_cell_style(colors, flags, None, None);

    let packed = ext.to_packed_colors();
    assert!(packed.fg_is_indexed());
    assert_eq!(packed.fg_index(), 100);
    assert!(packed.bg_is_indexed());
    assert_eq!(packed.bg_index(), 200);

    let cell_flags = ExtendedStyle::attrs_to_cell_flags(ext.style.attrs);
    assert!(cell_flags.contains(CellFlags::BOLD));
    assert!(cell_flags.contains(CellFlags::UNDERLINE));
    assert!(cell_flags.contains(CellFlags::STRIKETHROUGH));
}

#[test]
fn all_style_attrs_roundtrip() {
    // Non-underline attrs plus UNDERLINE only: dotted and dashed are compound
    // bit patterns, so each underline variant is tested separately below.
    let base_attrs = StyleAttrs::BOLD
        | StyleAttrs::DIM
        | StyleAttrs::ITALIC
        | StyleAttrs::BLINK
        | StyleAttrs::INVERSE
        | StyleAttrs::HIDDEN
        | StyleAttrs::STRIKETHROUGH
        | StyleAttrs::OVERLINE;

    let base_flags = CellFlags::BOLD
        .union(CellFlags::DIM)
        .union(CellFlags::ITALIC)
        .union(CellFlags::BLINK)
        .union(CellFlags::INVERSE)
        .union(CellFlags::HIDDEN)
        .union(CellFlags::STRIKETHROUGH)
        .union(CellFlags::OVERLINE);

    // CellFlags -> StyleAttrs
    let ext = ExtendedStyle::from_cell_style(PackedColors::DEFAULT, base_flags, None, None);
    assert_eq!(ext.style.attrs, base_attrs);

    // StyleAttrs -> CellFlags
    let recovered_flags = ExtendedStyle::attrs_to_cell_flags(base_attrs);
    assert_eq!(
        recovered_flags.bits() & base_flags.bits(),
        base_flags.bits()
    );

    // Each underline variant round-trips independently.
    for (cf, sa) in [
        (CellFlags::UNDERLINE, StyleAttrs::UNDERLINE),
        (CellFlags::DOUBLE_UNDERLINE, StyleAttrs::DOUBLE_UNDERLINE),
        (CellFlags::CURLY_UNDERLINE, StyleAttrs::CURLY_UNDERLINE),
        (CellFlags::DOTTED_UNDERLINE, StyleAttrs::DOTTED_UNDERLINE),
        (CellFlags::DASHED_UNDERLINE, StyleAttrs::DASHED_UNDERLINE),
    ] {
        let ext =
            ExtendedStyle::from_cell_style(PackedColors::DEFAULT, base_flags.union(cf), None, None);
        assert!(
            ext.style.attrs.contains(sa),
            "{sa:?} should survive CellFlags -> StyleAttrs"
        );
        let recovered = ExtendedStyle::attrs_to_cell_flags(base_attrs.union(sa));
        assert!(
            recovered.contains(cf),
            "{cf:?} should survive StyleAttrs -> CellFlags"
        );
    }
}

#[test]
fn superscript_and_subscript_roundtrip_between_cell_flags_and_style_attrs() {
    for (cell_flag, style_attr) in [
        (CellFlags::SUPERSCRIPT, StyleAttrs::SUPERSCRIPT),
        (CellFlags::SUBSCRIPT, StyleAttrs::SUBSCRIPT),
    ] {
        let ext = ExtendedStyle::from_cell_style(PackedColors::DEFAULT, cell_flag, None, None);
        assert!(
            ext.style.attrs.contains(style_attr),
            "{style_attr:?} should survive CellFlags -> StyleAttrs conversion"
        );

        let recovered_flags = ExtendedStyle::attrs_to_cell_flags(style_attr);
        assert!(
            recovered_flags.contains(cell_flag),
            "{cell_flag:?} should survive StyleAttrs -> CellFlags conversion"
        );
    }
}
