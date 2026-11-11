// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn default_style_is_empty_and_cached_as_default() {
    let style = CurrentStyle::default();
    assert!(style.fg.is_default(), "fg is DEFAULT_FG");
    assert!(style.bg.is_default(), "bg is DEFAULT_BG");
    assert!(style.flags.is_empty(), "no flags");
    assert!(!style.protected, "not protected");
    assert!(style.is_default(), "cached is_default");
    assert!(!style.has_style_extras(), "no style extras");
}

#[test]
fn one_attribute_or_colour_leaves_the_default() {
    let d_fg = PackedColor::DEFAULT_FG;
    let d_bg = PackedColor::DEFAULT_BG;
    let empty = CellFlags::empty();
    let rows = [
        ("bold", CellFlags::BOLD, d_fg, d_bg, false),
        ("italic", CellFlags::ITALIC, d_fg, d_bg, false),
        ("underline", CellFlags::UNDERLINE, d_fg, d_bg, false),
        ("dim", CellFlags::DIM, d_fg, d_bg, false),
        ("blink", CellFlags::BLINK, d_fg, d_bg, false),
        ("inverse", CellFlags::INVERSE, d_fg, d_bg, false),
        ("hidden", CellFlags::HIDDEN, d_fg, d_bg, false),
        ("strikethrough", CellFlags::STRIKETHROUGH, d_fg, d_bg, false),
        (
            "curly underline (extended bits)",
            CellFlags::CURLY_UNDERLINE,
            d_fg,
            d_bg,
            true,
        ),
        (
            "overline (extended bits)",
            CellFlags::OVERLINE,
            d_fg,
            d_bg,
            true,
        ),
        ("indexed fg 1", empty, PackedColor::indexed(1), d_bg, false),
        ("indexed bg 4", empty, d_fg, PackedColor::indexed(4), false),
        ("rgb fg", empty, PackedColor::rgb(255, 128, 0), d_bg, true),
        ("rgb bg", empty, d_fg, PackedColor::rgb(0, 64, 128), true),
    ];
    for (label, flag, fg, bg, extras) in rows {
        let mut style = CurrentStyle {
            fg,
            bg,
            ..CurrentStyle::default()
        };
        style.flags.insert(flag);
        style.update_cached_colors();
        assert!(style.flags.contains(flag), "{label}: flag set");
        assert_eq!(style.fg, fg, "{label}: fg");
        assert_eq!(style.bg, bg, "{label}: bg");
        assert!(!style.is_default(), "{label}: no longer default");
        assert_eq!(style.has_style_extras(), extras, "{label}: style extras");
    }
}

#[test]
fn test_clear_bold_flag() {
    let mut style = CurrentStyle::default();
    style.flags.insert(CellFlags::BOLD);
    style.flags.remove(CellFlags::BOLD);
    style.update_cached_colors();
    assert!(!style.flags.contains(CellFlags::BOLD));
    assert!(style.is_default(), "clearing bold should restore default");
}

#[test]
fn test_reset_clears_all() {
    let mut style = CurrentStyle {
        fg: PackedColor::rgb(255, 0, 0),
        bg: PackedColor::indexed(5),
        ..CurrentStyle::default()
    };
    style.flags.insert(CellFlags::BOLD);
    style.flags.insert(CellFlags::ITALIC);
    style.protected = true;
    style.update_cached_colors();

    style.reset();

    assert!(style.fg.is_default());
    assert!(style.bg.is_default());
    assert!(style.flags.is_empty());
    assert!(!style.protected);
    assert!(style.is_default());
    assert!(!style.has_style_extras());
}

#[test]
fn test_reset_sgr_preserves_protected() {
    let mut style = CurrentStyle {
        fg: PackedColor::rgb(255, 0, 0),
        ..CurrentStyle::default()
    };
    style.flags.insert(CellFlags::BOLD);
    style.protected = true;
    style.update_cached_colors();

    style.reset_sgr();

    assert!(style.fg.is_default(), "sgr reset should clear fg color");
    assert!(style.flags.is_empty(), "sgr reset should clear flags");
    assert!(style.protected, "sgr reset must NOT clear protected");
    assert!(
        !style.is_default(),
        "style with protected=true is not default"
    );
}

#[test]
fn test_reset_sgr_without_protected_is_default() {
    let mut style = CurrentStyle {
        fg: PackedColor::indexed(3),
        ..CurrentStyle::default()
    };
    style.flags.insert(CellFlags::UNDERLINE);
    style.update_cached_colors();

    style.reset_sgr();

    assert!(
        style.is_default(),
        "sgr reset without protected should be default"
    );
}

#[test]
fn test_new_with_all_fields() {
    let style = CurrentStyle::new(
        PackedColor::rgb(255, 0, 0),
        PackedColor::indexed(4),
        CellFlags::BOLD.union(CellFlags::UNDERLINE),
        true,
    );
    assert!(style.fg.is_rgb());
    assert!(style.bg.is_indexed());
    assert!(style.flags.contains(CellFlags::BOLD));
    assert!(style.flags.contains(CellFlags::UNDERLINE));
    assert!(style.protected);
    assert!(style.has_style_extras()); // RGB fg
    assert!(!style.is_default());
}
