// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn frame(w: usize, h: usize, paint: impl Fn(usize, usize) -> [u8; 4]) -> Frame {
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            rgba.extend_from_slice(&paint(x, y));
        }
    }
    Frame { rgba, w, h }
}

#[test]
fn the_band_under_the_deck_is_held_to_the_band_without_it() {
    let band = |x: usize, y: usize| {
        if (1..3).contains(&x) && (1..3).contains(&y) {
            [200, 200, 200, 255]
        } else {
            [40, 40, 40, 255]
        }
    };
    let off = frame(5, 8, band);
    let deck = |x: usize, y: usize| {
        if y < 3 {
            [90, 0, 0, 255]
        } else {
            band(x, y - 3)
        }
    };
    let on = frame(5, 11, deck);
    let held = band_held(&on, 3, &off, 4).expect("the band is under the Deck");
    assert!(
        held.contains("5x4 band under the Deck (rows 3..7)"),
        "{held}"
    );
    let over = frame(5, 11, |x, y| {
        if (1..4).contains(&y) {
            [90, 0, 0, 255]
        } else {
            band(x, y.saturating_sub(3))
        }
    });
    let why = band_held(&over, 3, &off, 4).unwrap_err();
    assert!(why.starts_with("5 of 20 band pixels"), "{why}");
    assert!(band_held(&on, 2, &off, 4).is_err());
    assert!(band_held(&on, 8, &off, 4).is_err(), "past the frame");
}

#[test]
fn the_decks_height_is_its_points_at_the_scale_at_the_heads_top() {
    let dims = |scale: &str, head: u32, deck_y: u32, deck_h: u32| {
        Dims::read(&format!(
            "OK 24x80 scale={scale} cell_w=9 cell_h=20 pad=12 pad_top=2 head={head} \
             tab_rows=1 deck_y={deck_y} deck_h={deck_h} frame_w=744 surface_w=744 \
             surface_h=590"
        ))
        .expect("dims")
    };
    // The Deck's rows, the Disk Custody band and Network State: 62 + 30 + 30 pt.
    assert!(head(&dims("1.00", 136, 0, 122)).is_ok());
    assert!(head(&dims("1.25", 172, 0, 154)).is_ok());
    assert!(head(&dims("1.50", 204, 0, 183)).is_ok());
    assert!(head(&dims("2.00", 272, 0, 244)).is_ok());
    // CONTROL: the reservation before the Disk Custody band (62 + 30 pt) is refused.
    assert!(head(&dims("1.00", 106, 0, 92)).is_err());
    assert!(head(&dims("1.00", 76, 0, 62)).is_err());
    assert!(head(&dims("1.25", 172, 0, 153)).is_err());
    assert!(head(&dims("1.00", 136, 14, 122)).is_err());
    assert!(head(&dims("2.00", 272, 0, 122)).is_err());
    assert!(head(&dims("1.00", 14, 0, 0)).is_err());
    let d = dims("1.00", 136, 0, 122);
    assert_eq!(d.band_foot(), 158);
    assert_eq!(d.deck_rows(), 0..122);
    assert!(Dims::read("OK 24x80 scale=1.00").is_err());
}
