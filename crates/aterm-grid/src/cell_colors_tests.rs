// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

/// One side's colour mode, for the exhaustive walks below.
#[derive(Clone, Copy, Debug)]
enum Side {
    Default,
    Indexed(u8),
    Rgb,
}

fn every_side() -> impl Iterator<Item = Side> {
    [Side::Default, Side::Rgb]
        .into_iter()
        .chain((0..=255u8).map(Side::Indexed))
}

fn assert_side(label: &str, (default, indexed, rgb, index): (bool, bool, bool, u8), want: Side) {
    match want {
        Side::Default => assert!(default && !indexed && !rgb, "{label}: want default"),
        Side::Indexed(i) => {
            assert!(!default && indexed && !rgb, "{label}: want indexed");
            assert_eq!(index, i, "{label}: index");
        }
        Side::Rgb => assert!(!default && !indexed && rgb, "{label}: want rgb"),
    }
}

fn fg(c: PackedColors) -> (bool, bool, bool, u8) {
    (
        c.fg_is_default(),
        c.fg_is_indexed(),
        c.fg_is_rgb(),
        c.fg_index(),
    )
}

fn bg(c: PackedColors) -> (bool, bool, bool, u8) {
    (
        c.bg_is_default(),
        c.bg_is_indexed(),
        c.bg_is_rgb(),
        c.bg_index(),
    )
}

fn set_fg(c: PackedColors, side: Side) -> PackedColors {
    match side {
        Side::Default => c.set_fg_default(),
        Side::Indexed(i) => c.set_fg_indexed(i),
        Side::Rgb => c.with_rgb_fg(),
    }
}

fn set_bg(c: PackedColors, side: Side) -> PackedColors {
    match side {
        Side::Default => c.set_bg_default(),
        Side::Indexed(i) => c.set_bg_indexed(i),
        Side::Rgb => c.with_rgb_bg(),
    }
}

#[test]
fn packed_colors_every_fg_bg_and_extras_state() {
    assert_eq!(PackedColors::DEFAULT.0, 0);
    assert_eq!(PackedColors::new(), PackedColors::DEFAULT);
    assert_eq!(PackedColors::default(), PackedColors::DEFAULT);
    assert!(PackedColors::DEFAULT.is_default());
    assert!(!PackedColors::DEFAULT.has_extras());

    for i in 0..=255u8 {
        let only_fg = PackedColors::with_indexed_fg(i);
        assert_side("with_indexed_fg fg", fg(only_fg), Side::Indexed(i));
        assert_side("with_indexed_fg bg", bg(only_fg), Side::Default);
        let only_bg = PackedColors::with_indexed_bg(i);
        assert_side("with_indexed_bg fg", fg(only_bg), Side::Default);
        assert_side("with_indexed_bg bg", bg(only_bg), Side::Indexed(i));
        for j in 0..=255u8 {
            let both = PackedColors::with_indexed(i, j);
            assert_side("with_indexed fg", fg(both), Side::Indexed(i));
            assert_side("with_indexed bg", bg(both), Side::Indexed(j));
            assert!(!both.is_default() && !both.has_extras());
        }
    }

    let starts = [
        PackedColors::DEFAULT,
        PackedColors::with_indexed(100, 200),
        PackedColors::DEFAULT.with_rgb_fg().with_rgb_bg(),
        PackedColors::with_indexed(7, 9).with_extras_flag(),
    ];
    for start in starts {
        for want_fg in every_side() {
            for want_bg in every_side() {
                let fg_first = set_bg(set_fg(start, want_fg), want_bg);
                let bg_first = set_fg(set_bg(start, want_bg), want_fg);
                for c in [fg_first, bg_first] {
                    for extras in [false, true] {
                        let c = if extras {
                            c.with_extras_flag()
                        } else {
                            c.without_extras_flag()
                        };
                        assert_eq!(c.has_extras(), extras, "{start:?} -> {c:?}");
                        assert_side("fg", fg(c), want_fg);
                        assert_side("bg", bg(c), want_bg);
                        assert_eq!(
                            c.is_default(),
                            matches!((want_fg, want_bg), (Side::Default, Side::Default)),
                            "{c:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn packed_color_every_mode() {
    for d in [PackedColor::DEFAULT_FG, PackedColor::DEFAULT_BG] {
        assert!(d.is_default(), "{d:?}");
        assert!(!d.is_indexed(), "{d:?}");
        assert!(!d.is_rgb(), "{d:?}");
    }
    for i in 0..=255u8 {
        let c = PackedColor::indexed(i);
        assert!(c.is_indexed() && !c.is_default() && !c.is_rgb(), "{c:?}");
        assert_eq!(c.index(), i);
    }
    let components = [0u8, 1, 10, 20, 30, 64, 127, 128, 254, 255];
    for r in components {
        for g in components {
            for b in components {
                let c = PackedColor::rgb(r, g, b);
                assert!(c.is_rgb() && !c.is_default() && !c.is_indexed(), "{c:?}");
                assert_eq!(c.rgb_components(), (r, g, b));
            }
        }
    }
}
