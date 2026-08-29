// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// THE OVERLINE'S OWN COLOUR CHANNEL (`RenderCell::overline_color`), on the CPU
// renderer.
//
// A cell's three line decorations are three separately-coloured bands, and the
// property that makes the channel worth having is INDEPENDENCE: the overline
// takes its channel, the underline keeps SGR 58's, and the strike — which has
// no colour anywhere, in any terminal — keeps the glyph's own ink. A seam
// painted for chrome must therefore hold ONE tone across a row whose cells
// carry different foregrounds, which is the whole reason the channel exists
// (`aterm_gui::link_target`).
//
// The channel is deliberately unreachable from the byte stream: no ECMA-48 code
// and no vendor extension assigns an overline colour (53/55 set the line, 58/59
// are the UNDERLINE's), so these tests set it on the `RenderInput` the way
// aterm's own row builders do.

use aterm_core::terminal::{RenderCell, Terminal};
use aterm_render::{Frame, Renderer, Theme, rgb_to_u32};

fn renderer() -> Option<Renderer> {
    Renderer::from_system(18.0, Theme::default()).map(|mut r| {
        // Deterministic pixels: block on the lazy fallback parses so a parse
        // landing between two renders can't recolour a "must not change" frame.
        r.debug_block_on_lazy_fallbacks();
        r
    })
}

/// The pixels of one cell's TOP row — where `overline_rect` anchors its band.
fn top_row_pixels(f: &Frame, cw: usize, row: usize, ch: usize, col: usize) -> Vec<u32> {
    let y = row * ch;
    (col * cw..(col * cw + cw).min(f.width))
        .map(|x| f.pixels[y * f.width + x])
        .collect()
}

/// How deep `ink` runs down from a cell's top edge. Measured off the frame
/// rather than assumed to be one pixel: an overline is drawn at the font's
/// resolved underline thickness, which differs per face, per size and per
/// platform, and these tests only ever run on one face at a time.
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

/// An overline wears its own channel instead of its cell's foreground — the
/// band the channel exists to colour, and no other pixel on the row.
#[test]
fn an_overline_takes_its_channel_and_leaves_the_rest_of_the_cell_alone() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
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
    // Everything BELOW the band is untouched: the channel colours one rule, not
    // the text under it.
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

/// A cell with all three decorations paints three INDEPENDENTLY coloured bands.
/// The strike is the control: nothing anywhere gives a strikethrough a colour,
/// so it must keep the glyph's ink while the two channels around it move.
#[test]
fn the_three_decoration_bands_take_three_independent_inks() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
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
    // The underline keeps SGR 58's colour: it is present somewhere below the
    // top band and the overline's colour is nowhere in the rest of the cell.
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
    // The strike has no channel: its ink is the cell's foreground, which the
    // blank cells carry nowhere else, so its presence proves it took the fg.
    let fg = rgb_to_u32(input.cells[0][0].fg);
    assert!(
        rest.contains(&fg),
        "the strike must still paint in the glyph's own ink"
    );
}

/// The seam property the chrome bands depend on: ONE rule tone across a row
/// whose cells deliberately carry two inks. Without the channel the bright run
/// drags a brighter dash of the rule along with it.
#[test]
fn a_seam_holds_one_tone_across_a_row_of_two_different_inks() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (1usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Two runs of overlined text in two very different foregrounds.
    term.process(b"\x1b[?25l\x1b[53m\x1b[38;2;80;80;80mdim\x1b[38;2;255;255;255mLIT");

    let mut input = term.cell_frame(rows, cols);
    // The un-channelled row is the control: its rule follows each cell's ink.
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

/// The empty channel is the pre-channel path, byte for byte: `None` everywhere
/// must render exactly what a frame that never heard of the channel renders.
#[test]
fn an_unset_overline_colour_renders_the_cells_own_foreground() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (1usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l\x1b[53m\x1b[38;2;255;0;0mRED");
    let input = term.cell_frame(rows, cols);
    assert!(
        input.cells[0].iter().all(|c| c.overline_color.is_none()),
        "no escape sequence may reach the chrome-only channel"
    );
    let f = rend.render_input(&input);
    let band = top_row_pixels(&f, cw, 0, ch, 0);
    assert!(
        band.iter().all(|px| *px == rgb_to_u32([255, 0, 0])),
        "an unset channel leaves the overline in its cell's fg: {band:?}"
    );
}
