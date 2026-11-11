// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn default_colors_are_the_xterm_256_table() {
    for (index, rgb) in [
        (0, Rgb::new(0, 0, 0)),
        (7, Rgb::new(229, 229, 229)),
        (8, Rgb::new(138, 143, 153)),
        (15, Rgb::new(255, 255, 255)),
        (16, Rgb::new(0, 0, 0)),
        (231, Rgb::new(255, 255, 255)),
        (196, Rgb::new(255, 0, 0)),
        (232, Rgb::new(8, 8, 8)),
        (255, Rgb::new(238, 238, 238)),
    ] {
        assert_eq!(ColorPalette::default_color(index), rgb, "index {index}");
    }
}

#[test]
fn windows_blues_read_as_ink_and_as_a_background() {
    let page = Rgb::new(0x11, 0x13, 0x18);
    let ink = Rgb::new(0xD0, 0xD0, 0xD0);
    let (black, white) = (Rgb::new(0, 0, 0), Rgb::new(255, 255, 255));
    let serves = |blue: Rgb, bright: Rgb| {
        blue.contrast(page) >= 3.5
            && ink.contrast(blue) >= 3.0
            && white.contrast(blue) >= 4.5
            && bright.contrast(black) > blue.contrast(black)
            && bright.contrast(page) >= 7.0
    };
    let historical = (Rgb::new(59, 142, 234), Rgb::new(92, 92, 255));
    assert!(!serves(historical.0, historical.1));
    let shipped = (
        ColorPalette::default_color(4),
        ColorPalette::default_color(12),
    );
    assert_eq!(serves(shipped.0, shipped.1), cfg!(windows), "{shipped:?}");
    assert_eq!(shipped == historical, !cfg!(windows));
}

#[test]
fn windows_normal_tones_are_one_family() {
    let page = Rgb::new(0x11, 0x13, 0x18);
    let saturation = |c: Rgb| {
        let max = c.r.max(c.g).max(c.b);
        let min = c.r.min(c.g).min(c.b);
        f32::from(max - min) / f32::from(max.max(1))
    };
    let family = |slots: [Rgb; 5]| {
        slots
            .iter()
            .all(|&c| saturation(c) <= 0.7 && c.contrast(page) >= 5.0)
    };
    let [red, magenta] = [1, 5].map(ColorPalette::default_color);
    let xterm = [
        red,
        Rgb::new(0, 205, 0),
        Rgb::new(205, 205, 0),
        magenta,
        Rgb::new(0, 205, 205),
    ];
    assert!(!family(xterm), "control: xterm's primaries");
    let shipped = [1, 2, 3, 5, 6].map(ColorPalette::default_color);
    assert_eq!(family(shipped), cfg!(windows), "{shipped:?}");
    assert_eq!(shipped == xterm, !cfg!(windows));
}

#[test]
fn windows_brights_are_their_normals_lighter() {
    use crate::scheme_signature::{chroma_hue, oklab, oklab_distance};
    let page = Rgb::new(0x11, 0x13, 0x18);
    let text = [
        ColorPalette::default_color(7),
        ColorPalette::default_color(15),
        crate::ColorScheme::default().foreground,
    ];
    let pair_serves = |slot: u8, normal: Rgb, bright: Rgb| {
        let ((c_normal, h_normal), (c_bright, h_bright)) = (chroma_hue(normal), chroma_hue(bright));
        let turn = (h_bright - h_normal + 180.0).rem_euclid(360.0) - 180.0;
        let hue_held = if slot == 3 {
            (0.0..=20.0).contains(&turn)
        } else {
            turn.abs() <= 10.0
        };
        let step = oklab(bright).0 - oklab(normal).0;
        let one_dark = matches!(slot, 2 | 3 | 5 | 6);
        let stepped = !one_dark
            || ((0.10..=0.14).contains(&step)
                && bright.contrast(normal) >= 1.45
                && c_bright >= 0.85 * c_normal);
        let apart = if one_dark { 0.11 } else { 0.05 };
        hue_held
            && stepped
            && oklab_distance(normal, bright) >= apart
            && text.iter().all(|&t| oklab_distance(t, bright) >= 0.09)
            && bright.contrast(page) >= 4.5
            && bright.contrast(page) > normal.contrast(page)
    };
    let serves = |brights: [Rgb; 6]| {
        (1..=6u8)
            .zip(brights)
            .all(|(slot, bright)| pair_serves(slot, ColorPalette::default_color(slot), bright))
    };
    let shipped = [9, 10, 11, 12, 13, 14].map(ColorPalette::default_color);
    let dracula = [
        shipped[0],
        Rgb::new(0x50, 0xFA, 0x7B),
        Rgb::new(0xF1, 0xFA, 0x8C),
        shipped[3],
        Rgb::new(0xFF, 0x79, 0xC6),
        Rgb::new(0x8B, 0xE9, 0xFD),
    ];
    if cfg!(windows) {
        let one_dark_pro = [
            Rgb::new(0xAE, 0xDE, 0x8B),
            Rgb::new(0xFF, 0xDA, 0x97),
            Rgb::new(0xE4, 0x8C, 0xFE),
            Rgb::new(0x65, 0xD1, 0xDF),
        ];
        for (slot, (bright, close)) in [2u8, 3, 5, 6].into_iter().zip(
            [dracula[1], dracula[2], dracula[4], dracula[5]]
                .into_iter()
                .zip(one_dark_pro),
        ) {
            let normal = ColorPalette::default_color(slot);
            assert!(
                !pair_serves(slot, normal, bright),
                "control: Dracula's slot {}",
                slot + 8
            );
            assert!(
                !pair_serves(slot, normal, close),
                "control: One Dark Pro's slot {}",
                slot + 8
            );
        }
        let cream = Rgb::new(0xFF, 0xEA, 0xC6);
        assert!(
            !pair_serves(3, ColorPalette::default_color(3), cream),
            "control: amber's own hue at +0.12, a cream"
        );
    }
    assert_eq!(serves(shipped), cfg!(windows), "{shipped:?}");
    assert_eq!(shipped == dracula, !cfg!(windows));
}

#[test]
fn parse_color_spec_reads_every_xparsecolor_form() {
    for (spec, rgb) in [
        ("rgb:ff/00/80", Rgb::new(255, 0, 128)),
        ("rgb:f/0/8", Rgb::new(255, 0, 136)),
        ("#f08", Rgb::new(255, 0, 136)),
        ("#ff0080", Rgb::new(255, 0, 128)),
        ("#fff000888", Rgb::new(255, 0, 136)),
        ("#ffff00008080", Rgb::new(255, 0, 128)),
        ("rgbi:1.0/0.5/0.0", Rgb::new(255, 128, 0)),
        ("rgbi:0.0/0.0/0.0", Rgb::new(0, 0, 0)),
        ("rgbi:1.0/1.0/1.0", Rgb::new(255, 255, 255)),
        ("rgbi:1/0/0", Rgb::new(255, 0, 0)),
        ("rgbi:2.0/1.5/1.0", Rgb::new(255, 255, 255)),
        ("rgbi:-0.5/-1.0/0.0", Rgb::new(0, 0, 0)),
        ("rgbi:0.333/0.667/0.5", Rgb::new(85, 170, 128)),
    ] {
        assert_eq!(ColorPalette::parse_color_spec(spec), Some(rgb), "{spec}");
    }
}

#[test]
fn parse_color_spec_refuses_malformed_specs() {
    for spec in [
        "",
        "#",
        "#ff",
        "#ffff",
        "rgb:",
        "rgb:ff/00",
        "rgb:gg/00/00",
        "notacolor",
        "rgbi:",
        "rgbi:1.0/0.5",
        "rgbi:abc/0.0/0.0",
        "rgbi:1.0/0.5/0.0/0.5",
        "#你好",
        "#你",
        "#+f+0+8",
        "#ffX00Y88Z",
        "#ffXX00YY80ZZ",
        "rgb:+f/00/80",
    ] {
        assert_eq!(ColorPalette::parse_color_spec(spec), None, "{spec:?}");
    }
}

#[test]
fn parse_named_color_reads_x11_and_css_names() {
    for (name, rgb) in [
        ("black", Rgb::new(0, 0, 0)),
        ("silver", Rgb::new(192, 192, 192)),
        ("gray", Rgb::new(128, 128, 128)),
        ("white", Rgb::new(255, 255, 255)),
        ("maroon", Rgb::new(128, 0, 0)),
        ("red", Rgb::new(255, 0, 0)),
        ("purple", Rgb::new(128, 0, 128)),
        ("fuchsia", Rgb::new(255, 0, 255)),
        ("green", Rgb::new(0, 128, 0)),
        ("lime", Rgb::new(0, 255, 0)),
        ("olive", Rgb::new(128, 128, 0)),
        ("yellow", Rgb::new(255, 255, 0)),
        ("navy", Rgb::new(0, 0, 128)),
        ("blue", Rgb::new(0, 0, 255)),
        ("teal", Rgb::new(0, 128, 128)),
        ("aqua", Rgb::new(0, 255, 255)),
        ("coral", Rgb::new(255, 127, 80)),
        ("rebeccapurple", Rgb::new(102, 51, 153)),
    ] {
        assert_eq!(ColorPalette::parse_color_spec(name), Some(rgb), "{name}");
    }
}

#[test]
fn format_color_spec_is_the_16_bit_spelling_and_round_trips() {
    assert_eq!(
        ColorPalette::format_color_spec(Rgb::new(0, 0, 0)),
        "rgb:0000/0000/0000"
    );
    assert_eq!(
        ColorPalette::format_color_spec(Rgb::new(255, 255, 255)),
        "rgb:ffff/ffff/ffff"
    );
    for r in [0u8, 1, 127, 128, 255] {
        for g in [0u8, 1, 127, 128, 255] {
            for b in [0u8, 1, 127, 128, 255] {
                let original = Rgb::new(r, g, b);
                let spec = ColorPalette::format_color_spec(original);
                let parsed = ColorPalette::parse_color_spec(&spec).unwrap();
                assert_eq!(
                    parsed, original,
                    "roundtrip failed for ({r}, {g}, {b}): {spec}"
                );
            }
        }
    }
}

#[test]
fn palette_cache_agrees_with_overrides_after_mutations() {
    fn assert_coherent(p: &ColorPalette, what: &str, overrides: usize) {
        assert_eq!(p.overrides_count(), overrides, "{what}: override count");
        for i in 0..=255u8 {
            let expected = p
                .overrides()
                .iter()
                .find(|(idx, _)| *idx == i)
                .map_or_else(|| ColorPalette::default_color(i), |(_, c)| *c);
            assert_eq!(p.get(i), expected, "{what}: index {i}");
        }
    }

    let mut p = ColorPalette::new();
    assert_coherent(&p, "fresh", 0);

    for i in [0u8, 7, 15, 42, 200, 255] {
        p.set(i, Rgb::new(i ^ 0x5A, i.wrapping_add(9), 3));
    }
    assert_coherent(&p, "after sets", 6);

    p.set(7, Rgb::new(1, 1, 1));
    assert_eq!(p.get(7), Rgb::new(1, 1, 1));
    assert_coherent(&p, "after in-place update", 6);

    p.reset_color(15);
    assert_coherent(&p, "after reset_color of a middle entry", 5);

    p.set(42, ColorPalette::default_color(42));
    assert_coherent(&p, "after set-to-default", 4);

    p.reset_color(101);
    assert_coherent(&p, "after reset_color of a non-override", 4);

    p.reset();
    assert_coherent(&p, "after reset", 0);
    assert_eq!(ColorPalette::parse_color_spec("rgbi:+0.5/0.5/0.5"), None);
}
