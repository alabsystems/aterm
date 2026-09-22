// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The host may present a Space and the next letter's echoes in one frame.
//! A subsequent composer wrap must carry the already typed part of the word.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::rainbow_kitty::TypedClass;

fn wrapped_columns(coalesced: bool, glyph_width: u16, boundary_class: TypedClass) -> BTreeSet<u16> {
    let geom = Geom {
        cw: 8,
        ch: 16,
        rows: 30,
        cols: 80,
        origin_x: 0,
        origin_y: 0,
        win_w: 640,
        win_h: 480,
        head: 0,
    };
    let cfg = GlowConfig {
        enabled: true,
        style: GlowStyle::RainbowKitty,
        classic_mono: false,
        ribbon_tall: true,
        ribbon_flat: false,
        dark_theme: true,
        theme_fg: 0x00c8_d3f5,
        theme_bg: 0x001a_1b26,
        color: 0x0050_fa7b,
        accent: 0x007a_a2f7,
        duration: Duration::from_millis(400),
        length: 24,
        intensity: 1.0,
        audible: true,
        radius: 0.6,
        ring: true,
        beam: false,
        head_dx: 0.5,
        pack: None,
    };
    let mut glow = CursorGlow::default();
    let mut quads = Vec::new();
    let mut now = Instant::now();
    const ROW: u16 = 20;
    glow.tick(Some((ROW, 2)), now, &cfg, geom, &mut quads);
    // Fill to the last word boundary of an 80-column composer's 2-cell
    // inset, using the same public committed-key/observed-caret API as GUI.
    let prefix_end = 77 - glyph_width;
    for col in 3..=prefix_end {
        now += Duration::from_millis(70);
        glow.note_typed_glyph(now, 1, false, TypedClass::Glyph);
        glow.tick(Some((ROW, col)), now, &cfg, geom, &mut quads);
    }
    // The suffix is Space plus a one- or two-cell glyph, ending at caret78.
    // The only scheduling difference is whether the Space has its own tick.
    now += Duration::from_millis(70);
    glow.note_typed_glyph(now, 1, false, boundary_class);
    if !coalesced {
        glow.tick(Some((ROW, prefix_end + 1)), now, &cfg, geom, &mut quads);
    }
    now += Duration::from_millis(4);
    glow.note_typed_glyph(now, glyph_width, false, TypedClass::Glyph);
    now += Duration::from_millis(12);
    glow.tick(Some((ROW, 78)), now, &cfg, geom, &mut quads);

    // Typing y re-wraps the word onto the lower row, as a bottom-pinned
    // composer grows upward: scrolling carries the original band to ROW-1,
    // and the new caret is after the relocated glyph and y on ROW.
    now += Duration::from_millis(70);
    glow.note_typed_glyph(now, 1, false, TypedClass::Glyph);
    glow.note_scroll(1);
    let landing = 3 + glyph_width;
    glow.tick(Some((ROW, landing)), now, &cfg, geom, &mut quads);
    now += Duration::from_millis(40);
    glow.tick(Some((ROW, landing)), now, &cfg, geom, &mut quads);
    let columns = glow
        .v2_ribbon()
        .expect("rainbow engine active")
        .cells()
        .iter()
        .filter(|cell| cell.row == ROW && cell.typing && !cell.leaving())
        .map(|cell| cell.col)
        .collect();
    let admission: Vec<_> = glow
        .admission_log()
        .map(|record| record.line(now))
        .collect();
    let tail = &admission[admission.len().saturating_sub(4)..];
    println!(
        "coalesced={coalesced}, glyph_width={glyph_width}, lower-row columns={columns:?}, admission={tail:?}"
    );
    columns
}

#[test]
fn separated_space_and_glyph_wrap_keep_the_moved_word_lit() {
    let columns = wrapped_columns(false, 1, TypedClass::Space);
    assert!(columns.contains(&2), "the relocated x is dark: {columns:?}");
    assert!(columns.contains(&3), "the new y is dark: {columns:?}");
}

#[test]
fn coalesced_space_and_glyph_wrap_keep_the_moved_word_lit() {
    let columns = wrapped_columns(true, 1, TypedClass::Space);
    assert!(columns.contains(&2), "the relocated x is dark: {columns:?}");
    assert!(columns.contains(&3), "the new y is dark: {columns:?}");
}

#[test]
fn coalesced_space_and_wide_glyph_wrap_keep_both_moved_cells_lit() {
    let columns = wrapped_columns(true, 2, TypedClass::Space);
    assert_eq!(columns, BTreeSet::from([2, 3, 4]));
}

#[test]
fn the_space_class_is_required_to_relocate_the_word() {
    let spaced = wrapped_columns(true, 1, TypedClass::Space);
    let plain = wrapped_columns(true, 1, TypedClass::Glyph);
    assert!(spaced.contains(&2), "the moved word must be lit");
    assert!(
        !plain.contains(&2),
        "plain Glyph cannot invent a word boundary"
    );
    assert!(plain.contains(&3), "the new key itself still lights");
}
