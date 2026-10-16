// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A band row's words clear the seam that closes it (the band's foot room; design
//! rulings 248, 254 and 256).
//!
//! The seam is a 1 px rail on the band row's last pixel row, and the row asks its words
//! to keep clear of it (`ChromeRaster::clear_rail`), so one pixel row of ground separates
//! the lowest descender from the seam. These tests compose the band through the real
//! `compose_band` and render it through the CPU renderer at the shipped sizes, and pin
//! the cases that keep the face's underline as the seam: cells too short to lift, a
//! strain rail, an outlined capsule, and chips that keep their own fill.

use aterm_core::render::HostRowPixels;
use aterm_core::terminal::{RenderCell, Terminal, UnderlineStyle};
use aterm_messages::ink::{BandInks, BarBase, ThemeInks};
use aterm_render::band::{
    BandFrame, RAIL_SEAM_MIN_CELL_H, band_bleed, cell, compose_band, native_host_room,
    seam_can_rail,
};
use aterm_render::{Renderer, Theme};

const COLS: usize = 44;
const PAD: usize = 8;

/// A size whose band rails its seam on every platform's default face
/// (Windows' Cascadia Mono PL: a 30 px cell, room `(1, 2)`).
const RAILED_PX: f32 = 24.0;

fn inks() -> BandInks {
    BandInks::derive(
        ThemeInks {
            bg: [0x11, 0x13, 0x18],
            fg: [0xd4, 0xd4, 0xd4],
            cursor: [0xf1, 0xfa, 0x8c],
        },
        None,
        BarBase::Blend,
    )
}

fn pack(c: [u8; 3]) -> u32 {
    (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2])
}

/// `p`, `g`, `y`, `j`, `q` and a comma: every descender the band's words can
/// carry.
const TEXT: &str = "Couldn't upgrade Claude in tab 1, gypqj";

fn band_row(c: &BandInks, text: &str) -> Vec<RenderCell> {
    (0..COLS)
        .map(|i| {
            let ch = text.chars().nth(i).unwrap_or(' ');
            cell(ch, c.value, c.bar_bg, false, false)
        })
        .collect()
}

/// One composed frame: `rows` band rows (the last one the stack's seam) over
/// two terminal rows, with the mutation `tweak` applied to the frame first.
struct Composed {
    r: Renderer,
    dst: aterm_core::render::RenderInput,
    at: BandFrame,
}

fn compose(
    px: f32,
    rows: usize,
    tweak: impl FnOnce(&mut Vec<Option<aterm_messages::ink::RowRaster>>),
) -> Composed {
    let mut r = Renderer::from_system(px, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");
    let (cw, ch) = r.cell_size();
    r.set_pad(PAD);
    let c = inks();
    let mut term = Terminal::new(rows as u16 + 2, COLS as u16);
    term.process(b"terminal text under the band\r\nsecond terminal row");
    let mut dst = term.cell_frame(rows + 2, COLS);
    dst.cursor_visible = false;
    let painted: Vec<Vec<RenderCell>> = (0..rows).map(|_| band_row(&c, TEXT)).collect();
    let mut rasters = vec![None; rows];
    tweak(&mut rasters);
    let at = BandFrame {
        cols: COLS,
        cell_w: cw,
        cell_h: ch,
        pad: PAD,
        lo: 0,
        frame_w: COLS * cw + 2 * PAD,
        grid_top: r.grid_top(),
        // The room the native host of THIS platform composes with: measured on
        // Windows, the cell-height rule alone elsewhere (`native_host_room`).
        room: native_host_room(|| r.band_room()),
    };
    let composed = compose_band(
        &mut dst,
        None,
        &painted,
        &rasters,
        rows,
        &c,
        false,
        at,
        &mut Vec::new(),
        HostRowPixels::Placed,
    );
    assert_eq!(composed, rows);
    Composed { r, dst, at }
}

/// Row `row`'s pixel rows at `px`, read off the rendered frame:
/// `(seam_row, cell_h, ink_rows)` relative to the row's top, where
/// `ink_rows[y]` counts the pixels of row `y` that are neither the band's
/// ground nor a full-width rule.
fn measure(px: f32, rows: usize, row: usize) -> (usize, usize, Vec<usize>) {
    let Composed { mut r, dst, at } = compose(px, rows, |_| {});
    let ch = at.cell_h;
    let f = r.render_input(&dst);
    let top = r.grid_top() + row * ch;
    let c = inks();
    let ground = pack(c.bar_bg);
    let (x0, x1) = (PAD, PAD + COLS * at.cell_w);
    let px_at = |x: usize, y: usize| f.pixels[y * f.width + x] & 0x00ff_ffff;
    let ink: Vec<usize> = (0..ch)
        .map(|y| (x0..x1).filter(|&x| px_at(x, top + y) != ground).count())
        .collect();
    // The seam (or divider): the row of the widest run of one non-ground tone
    // covering most of the width.
    let full = |y: usize| {
        let tone = px_at(x0, top + y);
        tone != ground
            && (x0..x1).filter(|&x| px_at(x, top + y) == tone).count() * 10 >= (x1 - x0) * 6
    };
    let seam_row = (0..ch)
        .rev()
        .find(|&y| full(y))
        .unwrap_or_else(|| panic!("{px}px row {row}: no seam row; ink per row {ink:?}"));
    (seam_row, ch, ink)
}

fn assert_words_clear_the_seam(rows: usize) {
    let mut ran = 0;
    for px in [16.0f32, 18.0, 24.0, 28.0, 32.0] {
        let mut face = Renderer::from_system(px, Theme::default())
            .expect("no font: build with aterm-render's default embedded-font feature");
        // A cell or face that cannot pay for the rail keeps the underline seam,
        // decided as this platform's host decides it.
        let (_, cell_h) = face.cell_size();
        if !seam_can_rail(cell_h, native_host_room(|| face.band_room())) {
            continue;
        }
        for row in 0..rows {
            let (seam_row, ch, ink) = measure(px, rows, row);
            ran += 1;
            // Non-vacuity: the words are there, and reach well down the row.
            assert!(
                ink.iter().filter(|&&n| n > 0).count() >= ch / 2,
                "{px}px row {row}: ink rows {ink:?}"
            );
            // (1) The seam is the row's last pixel row.
            assert_eq!(
                seam_row,
                ch - 1,
                "{px}px row {row}: the seam is at row {seam_row} of {ch}; ink per row {ink:?}"
            );
            // (2) One clear row above it: no glyph ink there.
            assert_eq!(
                ink[seam_row - 1],
                0,
                "{px}px row {row}: the row above the seam carries ink; ink per row {ink:?}"
            );
            // (3) The words are still whole above it.
            assert!(
                ink.iter().take(seam_row - 1).any(|&n| n > 0),
                "{px}px row {row}: no words above the seam"
            );
        }
    }
    assert!(ran > 0);
}

/// A band row's lowest ink keeps one clear pixel row above the rail on its last pixel
/// row.
#[test]
fn a_band_rows_words_keep_one_clear_pixel_row_above_its_closing_seam() {
    assert_words_clear_the_seam(1);
}

/// A stack keeps one style: the divider between two rows is a rail on the
/// upper row's last pixel row too, with the same clear row above it.
#[test]
fn a_stacked_rows_divider_is_a_rail_with_the_same_clear_row() {
    assert_words_clear_the_seam(2);
}

/// Under [`RAIL_SEAM_MIN_CELL_H`], or where the face's measured room cannot pay for the
/// lift ([`seam_can_rail`]: Windows' Cascadia Mono PL at 14 px and 18 px, room
/// `(0, 1)`), the seam stays the underline: no rail is laid and the words do not lift.
#[test]
fn a_cell_too_short_to_lift_keeps_the_underline_seam() {
    let mut ran = 0;
    // On Windows also 18 px: a tall cell whose face has no room for the rail.
    let sizes: &[f32] = if cfg!(windows) {
        &[8.0, 10.0, 12.0, 14.0, 18.0]
    } else {
        &[8.0, 10.0, 12.0, 14.0]
    };
    for &px in sizes {
        let Composed { dst, at, .. } = compose(px, 1, |_| {});
        if seam_can_rail(at.cell_h, at.room) {
            continue;
        }
        ran += 1;
        assert!(
            dst.chrome_rasters.is_empty(),
            "{px}px (cell {} against {RAIL_SEAM_MIN_CELL_H}, room {:?}): no rail \
             where the words cannot lift clear",
            at.cell_h,
            at.room
        );
        assert!(
            dst.cells[0]
                .iter()
                .all(|k| k.underline == UnderlineStyle::Single),
            "{px}px: the seam is the underline"
        );
    }
    assert!(ran > 0, "a size under the threshold was exercised");
}

/// The rail spans the frame edge to edge, so the renderer leaves out the gutters' own
/// seam segment and the seam is one line from window edge to window edge.
#[test]
fn the_seam_rail_covers_the_gutters_and_the_bleed_adds_no_second_line() {
    let Composed { mut r, dst, at } = compose(RAILED_PX, 1, |_| {});
    let c = inks();
    let m = dst.chrome_raster(0).expect("the seam's raster");
    assert_eq!((m.rail_h, m.clear_rail), (1, true));
    assert_eq!(m.rail.len(), at.frame_w);
    assert!(aterm_render::chrome_rail_covers_gutters(Some(m)));
    assert!(!aterm_render::chrome_rail_covers_gutters(None));
    r.set_chrome_bleed(Some(band_bleed(
        &c,
        0,
        1,
        [None; aterm_render::CHROME_ROW_EDGES],
    )));
    let f = r.render_input(&dst);
    let top = r.grid_top();
    let ch = at.cell_h;
    let px_at = |x: usize, y: usize| f.pixels[y * f.width + x] & 0x00ff_ffff;
    let seam = pack(c.label);
    let ground = pack(c.bar_bg);
    for x in [
        0,
        PAD - 1,
        PAD + COLS * at.cell_w,
        PAD + COLS * at.cell_w + PAD - 1,
    ] {
        assert_eq!(px_at(x, top + ch - 1), seam, "gutter x={x}: the rail");
        assert_eq!(
            px_at(x, top + ch - 2),
            ground,
            "gutter x={x}: no second line above it"
        );
        assert_eq!(
            px_at(x, top + ch - 3),
            ground,
            "gutter x={x}: none under the old underline row"
        );
    }
}

/// A stack with a strain rail (ruling 243), an outlined capsule (ruling 254) or chips
/// that keep their own fill keeps the underline seam: those rows own their foot, and a
/// rail would leave holes under a chip's `own` columns.
#[test]
fn a_stack_with_a_strain_rail_or_owned_chips_keeps_the_underline_seam() {
    use aterm_messages::ink::RowRaster;
    let c = inks();
    let seam = pack(c.label);
    type Make = fn(usize) -> RowRaster;
    let kinds: [(&str, Make); 3] = [
        ("own", |_| RowRaster {
            own: vec![(2, 6)],
            ..RowRaster::default()
        }),
        ("strain rail", |w| RowRaster {
            rail: vec![Some([0xff, 0x88, 0x00]); w],
            clear_rail: true,
            ..RowRaster::default()
        }),
        ("ring", |_| RowRaster {
            rings: vec![(2, 9, [0xbd, 0x93, 0xf9], [0, 0, 0])],
            ..RowRaster::default()
        }),
    ];
    for (kind, make) in kinds {
        let Composed { dst, .. } = compose(RAILED_PX, 1, |rasters| {
            // A rail as wide as any frame the test can make; `on_frame`
            // repeats a row's last column past its end.
            rasters[0] = Some(make(4096));
        });
        let m = dst.chrome_raster(0).expect("the row's raster");
        assert!(
            m.rail.iter().all(|&px| px != seam),
            "{kind}: no seam rail was laid over the row's own foot"
        );
        if kind == "strain rail" {
            // Where the gauge is lit it is the row's lower edge: the seam gives
            // way to it cell by cell (`give_seam_to_rails`).
            assert!(m.rail_h > 1, "{kind}: the gauge keeps its own height");
        } else {
            assert!(
                dst.cells[0][20].underline == UnderlineStyle::Single,
                "{kind}: the underline still closes the stack"
            );
        }
    }
}
