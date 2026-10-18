// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// A snapshot is final on the GPU path: `GpuRenderer::render_input` plans glyphs
// through its inner CPU renderer, so a fresh renderer's first snapshot of a glyph
// the primary face lacks could draw the provisional `.notdef` a background
// fallback parse leaves. The capture path (`render_input_target`) settles the
// parses the encode started and re-encodes while the font epoch moved during
// the pass. Keying on the epoch matters: a chain can land between two rows and
// be consumed by the later row's probe, leaving nothing to settle while the
// earlier row is still blank.
//
// The renderer under test is deliberately not font-settled; a settled CPU
// renderer is the oracle for which glyphs route off the primary face.
//
// No GPU or no font: COULD NOT RUN.

use aterm_core::terminal::Terminal;
use aterm_render::{FaceId, Theme};

use crate::common::{self, bb, gg, rr};

fn dist(a: u32, c: u32) -> i32 {
    (rr(a) - rr(c)).abs() + (gg(a) - gg(c)).abs() + (bb(a) - bb(c)).abs()
}

#[test]
fn a_fresh_gpu_renderers_first_snapshot_paints_every_fallback_glyph() {
    let theme = Theme::default();
    let px = 20.0;
    let mut oracle = common::cpu(px, theme);
    oracle.debug_block_on_lazy_fallbacks();
    // The message band's fallback-face glyphs; a glyph the primary covers never
    // touches the chain, and a glyph nothing covers is honest tofu either way.
    let probed: Vec<char> = ['\u{21BB}', '\u{26A0}', '\u{21E3}']
        .into_iter()
        .filter(|&ch| oracle.glyph_key_text(ch).source != FaceId::Primary)
        .collect();
    assert!(
        !probed.is_empty(),
        "the fixture primary leaves a probed glyph to the chain"
    );
    let mut gpu = common::gpu(px, theme);
    assert!(
        !gpu.fallback_parse_pending(),
        "precondition: a fresh renderer has probed nothing — the chain is lazy"
    );
    let (rows, cols) = (2usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    let mut bytes = String::from("\x1b[?25l");
    for row in 0..rows {
        bytes.push_str(&format!("\x1b[{};1H", row + 1));
        for (i, ch) in probed.iter().enumerate() {
            bytes.push_str(&format!("\x1b[{}G{ch}", 2 + 2 * i));
        }
    }
    term.process(bytes.as_bytes());
    let mut input = term.cell_frame(rows, cols);
    for row in 0..rows {
        for (i, &ch) in probed.iter().enumerate() {
            let cell = &mut input.cells[row][1 + 2 * i];
            assert_eq!(cell.ch, ch, "row {row}: the glyph sits at its column");
            cell.text_presentation = true; // the band's presentation
        }
    }
    let mut win = aterm_gpu::WindowGpu::new();
    let frame = gpu.render_input(&mut win, &input, None);
    let (cw, ch_px) = oracle.cell_size();
    assert_eq!(
        (frame.width, frame.height),
        (cols * cw, rows * ch_px),
        "an unpadded snapshot of the grid"
    );
    // Each cell's ground is its own `bg` (a written cell carries the terminal's
    // default background, an untouched one the theme's), within GPU rounding;
    // the written blank at col 0 is the control.
    let ink = |col: usize, row: usize| -> usize {
        // A trailing untouched cell is elided from its row (a default-bg
        // span) and painted in the theme's bg.
        let ground = input.cells[row].get(col).map_or(theme.bg, |c| {
            (u32::from(c.bg[0]) << 16) | (u32::from(c.bg[1]) << 8) | u32::from(c.bg[2])
        });
        (row * ch_px..(row + 1) * ch_px)
            .flat_map(|y| (col * cw..(col + 1) * cw).map(move |x| (x, y)))
            .filter(|&(x, y)| dist(frame.pixels[y * frame.width + x], ground) > 24)
            .count()
    };
    assert_eq!(ink(0, 0), 0, "control: a written blank cell has no ink");
    assert_eq!(ink(7, 0), 0, "control: an untouched blank cell has no ink");
    for (i, &ch) in probed.iter().enumerate() {
        for row in 0..rows {
            assert!(
                ink(1 + 2 * i, row) > 0,
                "GPU: {ch:?} painted a blank cell on row {row} of a fresh renderer's first snapshot"
            );
        }
    }
    assert!(
        !gpu.fallback_parse_pending(),
        "the snapshot settled the parses it started"
    );
}
