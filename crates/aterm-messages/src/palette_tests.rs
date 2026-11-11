// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const DARK_WORDS: [[u8; 3]; 2] = [[0x28, 0x2a, 0x36], [0xf8, 0xf8, 0xf2]];

#[test]
fn a_coloured_cursor_keeps_its_hue_and_a_grey_one_borrows() {
    let (blue, cyan) = ([0x26, 0x8b, 0xd2], [0x2a, 0xa1, 0x98]);
    for cursor in [[0x50, 0xfa, 0x7b], [0x7a, 0xa2, 0xf7], [0xdc, 0x8a, 0x78]] {
        assert_eq!(
            MeterHue::pick(cursor, blue, cyan, &DARK_WORDS),
            MeterHue::Cursor
        );
    }
    for cursor in [[0x65, 0x7b, 0x83], [0xf8, 0xf8, 0xf2], [0x80, 0x80, 0x80]] {
        assert_ne!(
            MeterHue::pick(cursor, blue, cyan, &DARK_WORDS),
            MeterHue::Cursor
        );
    }
}

#[test]
fn blue_unless_only_cyan_carries_the_words() {
    let grey = [0x83, 0x94, 0x96];
    let words = [[0x00, 0x2b, 0x36], [0x93, 0xa1, 0xa1]];
    assert_eq!(
        MeterHue::pick(grey, [0x26, 0x8b, 0xd2], [0x2a, 0xa1, 0x98], &words),
        MeterHue::Cyan
    );
    assert_eq!(
        MeterHue::pick(grey, [0x61, 0xaf, 0xef], [0x56, 0xb6, 0xc2], &words),
        MeterHue::Blue
    );
    let light = [[0xfd, 0xf6, 0xe3], [0x58, 0x6e, 0x75]];
    assert_eq!(
        MeterHue::pick(
            [0x65, 0x7b, 0x83],
            [0x26, 0x8b, 0xd2],
            [0x2a, 0xa1, 0x98],
            &light
        ),
        MeterHue::Blue
    );
    assert_eq!(
        MeterHue::pick(
            [0xec, 0xef, 0xf4],
            [0x81, 0xa1, 0xc1],
            [0x88, 0xc0, 0xd0],
            &[[0x2e, 0x34, 0x40], [0xd8, 0xde, 0xe9]]
        ),
        MeterHue::Blue
    );
    assert_eq!(
        MeterHue::pick(grey, [0x70, 0x70, 0x70], [0x2a, 0xa1, 0x98], &words),
        MeterHue::Cyan
    );
    assert_eq!(
        MeterHue::pick(grey, [0x70, 0x70, 0x70], [0x71, 0x71, 0x71], &words),
        MeterHue::Cursor
    );
}

#[test]
fn a_dark_warm_hue_reads_brown_and_borrows_blue() {
    assert!(reads_brown([0x8b, 0x4a, 0x2b]), "a saddle brown");
    assert!(reads_brown([0x9a, 0x55, 0x45]), "a terracotta label");
    assert!(!reads_brown([0xdc, 0x8a, 0x78]), "the rosewater itself");
    assert!(!reads_brown([0x1e, 0x66, 0xf5]), "Latte's blue");
    assert!(!reads_brown([0x40, 0x40, 0x40]), "a grey");
    assert!(!reads_brown([0x1a, 0x7f, 0x37]), "a dark green");
    let (l, c, h) = oklch([0xff, 0x00, 0x00]);
    assert!((l - 0.628).abs() < 0.01 && (c - 0.258).abs() < 0.01 && (h - 29.2).abs() < 0.5);
    let blue = [0x1e, 0x66, 0xf5];
    assert!(outline_borrows_blue([0x9a, 0x55, 0x45], blue, blue));
    assert!(!outline_borrows_blue([0x1e, 0x66, 0xf5], blue, blue));
    assert!(!outline_borrows_blue(
        [0x9a, 0x55, 0x45],
        [0x70, 0x70, 0x70],
        [0x70, 0x70, 0x70]
    ));
}

#[test]
fn a_warm_pastel_floored_brown_keeps_its_fill_and_takes_a_darker_edge() {
    let rosewater = [0xdc, 0x8a, 0x78];
    let brick = [0xb0, 0x6e, 0x60];
    assert!(keeps_pastel(rosewater, brick), "Latte's brick bar");
    assert!(!keeps_pastel(rosewater, rosewater), "not floored");
    assert!(
        !keeps_pastel([0x1e, 0x66, 0xf5], [0x1a, 0x57, 0xd0]),
        "a blue floored stays blue"
    );
    assert!(
        !keeps_pastel([0x8b, 0x4a, 0x2b], [0x6f, 0x3b, 0x22]),
        "a brown was never a pastel"
    );
    let track = [0xdf, 0xd8, 0xdc];
    let edge = pastel_edge(rosewater, track);
    assert!(contrast(edge, track) >= EDGE_CONTRAST, "{edge:?}");
    assert!(
        contrast(edge, track) < EDGE_CONTRAST + 0.1,
        "the least move"
    );
    assert!(contrast(edge, rosewater) >= 1.4, "darker than the fill");
    assert!(
        (oklch(edge).2 - oklch(rosewater).2).abs() < 3.0,
        "the same hue"
    );
    let deep = [0x90, 0x50, 0x40];
    assert_eq!(pastel_edge(deep, track), deep);
    let dark = [0x20, 0x20, 0x28];
    let light = pastel_edge([0x60, 0x40, 0x40], dark);
    assert!(luminance(light) > luminance([0x60, 0x40, 0x40]));
    assert!(contrast(light, dark) >= EDGE_CONTRAST);
}

#[test]
fn chroma_reads_oklch() {
    assert!(chroma([0x80, 0x80, 0x80]) < 1e-3);
    assert!(chroma([0, 0, 255]) > 0.3);
    assert!(near_grey([0x65, 0x7b, 0x83]), "Solarized Light's cursor");
    assert!(
        !near_grey([0xdc, 0x8a, 0x78]),
        "Catppuccin Latte's rosewater"
    );
}
