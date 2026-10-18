// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The overline's own colour channel (`RenderCell::overline_color`) on the CPU
// renderer. A cell's three line decorations colour independently: the overline
// takes the channel, the underline keeps SGR 58's colour, and the strike keeps
// the glyph's ink, so a chrome seam (`aterm_gui::link_target`) holds one tone
// across cells with different foregrounds.
//
// No escape sequence assigns an overline colour (53/55 set the line, 58/59 are
// the underline's), so these tests set it on the `RenderInput` as aterm's row
// builders do.

use aterm_core::terminal::{RenderCell, Terminal};
use aterm_render::{Frame, Renderer, Theme, rgb_to_u32};

fn renderer() -> Renderer {
    Renderer::from_system(18.0, Theme::default())
        .map(|mut r| {
            // Deterministic pixels: a lazy fallback parse landing between two
            // renders would recolour a "must not change" frame.
            r.debug_block_on_lazy_fallbacks();
            r
        })
        .expect("no font: build with aterm-render's default embedded-font feature")
}

/// The pixels of one cell's top row, where `overline_rect` anchors its band.
fn top_row_pixels(f: &Frame, cw: usize, row: usize, ch: usize, col: usize) -> Vec<u32> {
    let y = row * ch;
    (col * cw..(col * cw + cw).min(f.width))
        .map(|x| f.pixels[y * f.width + x])
        .collect()
}

/// How deep `ink` runs down from a cell's top edge: an overline is drawn at the
/// face's resolved underline thickness, so it is measured, not assumed.
fn band_depth(f: &Frame, cw: usize, ch: usize, col: usize, ink: u32) -> usize {
    let x0 = col * cw;
    let x1 = (x0 + cw).min(f.width);
    (0..ch.min(f.height))
        .take_while(|&y| {
            f.pixels[y * f.width + x0..y * f.width + x1]
                .iter()
                .all(|px| *px == ink)
        })
        .count()
}

/// Set every cell's overline colour, the way a chrome row builder does.
fn paint_seam(cells: &mut [Vec<RenderCell>], color: [u8; 3]) {
    for row in cells.iter_mut() {
        for cell in row.iter_mut() {
            cell.overline_color = Some(color);
        }
    }
}

/// An overline wears its own channel instead of the cell's foreground, and no
/// other pixel on the row changes.
#[test]
fn an_overline_takes_its_channel_and_leaves_the_rest_of_the_cell_alone() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (1usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l\x1b[53mOVER");

    let plain = rend
        .render_input(&term.cell_frame(rows, cols))
        .pixels
        .clone();

    let seam: [u8; 3] = [0xFF, 0x00, 0x99];
    let mut input = term.cell_frame(rows, cols);
    paint_seam(&mut input.cells, seam);
    let painted = rend.render_input(&input);

    assert_ne!(
        plain, painted.pixels,
        "an overline colour that changes nothing is not a channel"
    );
    let band = top_row_pixels(&painted, cw, 0, ch, 0);
    assert!(
        band.iter().all(|px| *px == rgb_to_u32(seam)),
        "the overline band must be the channel's exact bytes: {band:?}"
    );
    // Everything below the band is untouched.
    let t = band_depth(&painted, cw, ch, 0, rgb_to_u32(seam));
    assert!(
        (1..ch).contains(&t),
        "the seam must be a band inside the cell, not {t} of {ch} rows"
    );
    assert_eq!(
        &painted.pixels[painted.width * t..],
        &plain[painted.width * t..],
        "the channel must not reach any row but the overline's own"
    );
}

/// A cell with all three decorations paints three independently coloured
/// bands; the strike, which nothing gives a colour, keeps the glyph's ink.
#[test]
fn the_three_decoration_bands_take_three_independent_inks() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (1usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Overline + strike + underline, with SGR 58 colouring the underline only.
    term.process(b"\x1b[?25l\x1b[53m\x1b[9m\x1b[4m\x1b[58;2;0;255;0m    ");

    let over: [u8; 3] = [0xFF, 0x00, 0x99];
    let under: [u8; 3] = [0x00, 0xFF, 0x00];
    let mut input = term.cell_frame(rows, cols);
    paint_seam(&mut input.cells, over);
    let f = rend.render_input(&input);

    let band = top_row_pixels(&f, cw, 0, ch, 0);
    assert!(
        band.iter().all(|px| *px == rgb_to_u32(over)),
        "the overline takes its own channel: {band:?}"
    );
    // Below the top band: SGR 58's underline colour, and no overline colour.
    let t = band_depth(&f, cw, ch, 0, rgb_to_u32(over));
    let rest: Vec<u32> = (t..ch)
        .flat_map(|y| (0..cw).map(move |x| (y, x)))
        .map(|(y, x)| f.pixels[y * f.width + x])
        .collect();
    assert!(
        rest.contains(&rgb_to_u32(under)),
        "the SGR 58 underline must keep its own colour under an overline channel"
    );
    assert!(
        !rest.contains(&rgb_to_u32(over)),
        "the overline channel must not leak into the underline or the strike"
    );
    // The strike's ink is the cell's fg, which the blank cells carry nowhere
    // else, so its presence proves the strike took the fg.
    let fg = rgb_to_u32(input.cells[0][0].fg);
    assert!(
        rest.contains(&fg),
        "the strike must still paint in the glyph's own ink"
    );
}

/// One rule tone across a row whose cells carry two inks.
#[test]
fn a_seam_holds_one_tone_across_a_row_of_two_different_inks() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (1usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Two runs of overlined text in two very different foregrounds.
    term.process(b"\x1b[?25l\x1b[53m\x1b[38;2;80;80;80mdim\x1b[38;2;255;255;255mLIT");

    let mut input = term.cell_frame(rows, cols);
    // Control: without the channel the rule follows each cell's ink.
    let follows = rend.render_input(&input);
    let dim_band = top_row_pixels(&follows, cw, 0, ch, 0);
    let lit_band = top_row_pixels(&follows, cw, 0, ch, 3);
    assert_ne!(
        dim_band, lit_band,
        "without the channel a rule takes as many tones as the row has inks"
    );

    let seam: [u8; 3] = [0x40, 0x44, 0x4A];
    paint_seam(&mut input.cells, seam);
    let held = rend.render_input(&input);
    for col in 0..6 {
        let band = top_row_pixels(&held, cw, 0, ch, col);
        assert!(
            band.iter().all(|px| *px == rgb_to_u32(seam)),
            "column {col} breaks the seam's one tone: {band:?}"
        );
    }
}
