// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// FRM-1's correctness gate: `render_row_bg` no longer emits a background fill
// whose colour is the one the band's BASE already carries (the full path's
// `pixels.resize(w·h, frame_bg | bg_t<<24)`, the damaged path's `fill_band_bg`
// of exactly that value). The elision is byte-identical BY CONSTRUCTION — the
// skipped `fill` would store the u32 already in every pixel it covers — but
// "by construction" is only as good as the predicate, so this file pins the
// four fixtures where a NAIVE version of that predicate breaks:
//
//   1. a plain frame                       — the elision fires everywhere;
//   2. an SGR background that COINCIDES     — a cell explicitly coloured to the
//      with the frame default                 frame default. `RenderCell.bg` is
//                                             a bare `[u8; 3]`, so "explicitly
//                                             this colour" and "default" are the
//                                             same value and must render the same
//                                             pixel either way;
//   3. a LIVE WALLPAPER                     — the band's base is BACKDROP TEXELS,
//                                             not one scalar, so nothing may be
//                                             elided against `frame_bg`. A guard
//                                             that dropped `!wallpaper` would punch
//                                             the backdrop through a SELECTION band
//                                             whose colour coincides with it;
//   4. background OPACITY below 1           — the base carries a transmittance
//                                             byte in bits 24..32, so the
//                                             comparison must be against
//                                             `frame_bg | (bg_t << 24)` and not
//                                             against `frame_bg` alone;
//
// plus a PARTIAL-DAMAGE repaint of the same content, asserted byte-identical to
// a from-scratch full repaint through a renderer that has never rendered before.

use aterm_core::render::SceneAtlas;
use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme, WindowCpu, rgb_to_u32};

const ROWS: usize = 6;
const COLS: usize = 20;

fn renderer() -> Option<Renderer> {
    Renderer::from_system(18.0, Theme::default())
}

/// A terminal whose LIVE default background is the renderer theme's — the state
/// the shipping app is always in (`applied_terminal_config_*` pins the engine
/// default bg to `theme.bg` on every session), and the state in which "the base
/// clear and a default cell are the same colour" is true at all.
fn themed_term() -> Terminal {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    let bg = Theme::default().bg;
    term.process(
        format!(
            "\x1b]11;#{:02x}{:02x}{:02x}\x07",
            (bg >> 16) & 0xff,
            (bg >> 8) & 0xff,
            bg & 0xff
        )
        .as_bytes(),
    );
    term
}

/// The pixel at the centre of cell `(row, col)`'s background.
fn cell_px(r: &Renderer, pixels: &[u32], w: usize, row: usize, col: usize) -> u32 {
    let (cw, ch) = r.cell_size();
    // Two pixels in from the cell's left edge and one row above its baseline
    // band's bottom: inside the fill, outside any glyph ink for a BLANK cell.
    let x = r.pad() + col * cw + 1;
    let y = r.grid_top() + row * ch + 1;
    pixels[y * w + x]
}

/// Render `input` through the shipping damage-tracked entry and hand back the
/// framebuffer.
fn frame(r: &mut Renderer, wc: &mut WindowCpu, input: &aterm_core::render::RenderInput) -> Vec<u32> {
    let view = r.render_input_cached(wc, input);
    view.pixels().to_vec()
}

#[test]
fn plain_frame_default_cells_carry_the_clear_colour() {
    let Some(mut r) = renderer() else {
        eprintln!("SKIP: no system font");
        return;
    };
    let mut term = themed_term();
    term.process(b"hello");
    let input = term.cell_frame(ROWS, COLS);
    let (w, _h) = r.frame_size(ROWS, COLS);
    let mut wc = WindowCpu::new();
    let px = frame(&mut r, &mut wc, &input);

    let theme_bg = Theme::default().bg;
    // A blank cell far from the text, and the interior of the padding band:
    // both are the clear colour, and the elision is what leaves them that way.
    assert_eq!(
        cell_px(&r, &px, w, 3, 10),
        theme_bg,
        "a blank default-bg cell must show the frame clear colour"
    );
    // A blank cell on the TEXT row, past the end of the text: still the clear
    // colour, so the elision covers the materialized prefix as well as the tail.
    assert_eq!(
        cell_px(&r, &px, w, 0, COLS - 2),
        theme_bg,
        "a blank cell on the text row must show the frame clear colour"
    );
    // Non-vacuity: the frame really rasterized ink somewhere, so the assertions
    // above are not describing an empty buffer.
    assert!(
        px.iter().any(|&p| p != theme_bg),
        "the frame is entirely the clear colour — no glyph was rasterized, so \
         this fixture proves nothing"
    );
}

#[test]
fn sgr_background_equal_to_the_frame_default_renders_the_same_pixel() {
    let Some(mut r) = renderer() else {
        eprintln!("SKIP: no system font");
        return;
    };
    let theme_bg = Theme::default().bg;
    let mut term = themed_term();
    // Row 1 col 0: an EXPLICIT truecolor background that coincides exactly with
    // the frame default. Row 1 col 2: an explicit background that does not.
    term.process(
        format!(
            "\x1b[2;1H\x1b[48;2;{};{};{}m \x1b[0m \x1b[48;2;200;30;40m \x1b[0m",
            (theme_bg >> 16) & 0xff,
            (theme_bg >> 8) & 0xff,
            theme_bg & 0xff
        )
        .as_bytes(),
    );
    let input = term.cell_frame(ROWS, COLS);
    // The engine resolved both cells to concrete colours; the coincident one IS
    // the frame default, which is the whole point of the fixture.
    assert_eq!(
        rgb_to_u32(input.cells[1][0].bg),
        theme_bg,
        "fixture: the SGR cell must resolve to exactly the frame default"
    );
    assert_ne!(
        rgb_to_u32(input.cells[1][2].bg),
        theme_bg,
        "fixture: the control cell must NOT resolve to the frame default"
    );

    let (w, _h) = r.frame_size(ROWS, COLS);
    let mut wc = WindowCpu::new();
    let px = frame(&mut r, &mut wc, &input);
    assert_eq!(
        cell_px(&r, &px, w, 1, 0),
        theme_bg,
        "a cell whose SGR background coincides with the frame default must \
         render exactly that colour"
    );
    assert_eq!(
        cell_px(&r, &px, w, 1, 2),
        rgb_to_u32(input.cells[1][2].bg),
        "a cell whose SGR background differs must still be painted"
    );
}

#[test]
fn under_a_wallpaper_nothing_is_elided_against_the_theme_background() {
    // The hazard the `!wallpaper` conjunct exists for. Under a wallpaper the
    // band's base is BACKDROP TEXELS, so `frame_bg` is not what stands in those
    // pixels — and a run CAN still resolve to exactly `frame_bg` there: a
    // SELECTION band takes `theme.selection`, which a theme is free to set equal
    // to its background. A predicate that compared the run against `frame_bg`
    // without excluding the wallpaper regime would elide that band and let the
    // picture show through the selection.
    //
    // (An SGR cell whose bg merely COINCIDES with the frame default is NOT this
    // case: `resolve` already returns `None` for any cell whose bg equals its
    // pane default under a wallpaper — `RenderCell.bg` is a bare `[u8; 3]`, so
    // "explicitly this colour" and "default" are the same value — and that rule
    // predates and is untouched by the elision.)
    let theme_bg = Theme::default().bg;
    let theme = Theme {
        selection: theme_bg,
        ..Theme::default()
    };
    let Some(mut r) = Renderer::from_system(18.0, theme) else {
        eprintln!("SKIP: no system font");
        return;
    };
    let mut term = themed_term();
    term.process(b"selected text here");
    {
        let sel = term.text_selection_mut();
        sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 7, SelectionSide::Right);
        sel.complete_selection();
    }
    let (w, h) = r.frame_size(ROWS, COLS);
    // A flat backdrop, distinct from the theme bg in every texel.
    let backdrop = [0x40u8, 0x20, 0x60];
    let mut rgba = vec![0u8; w * h * 4];
    for px in rgba.as_chunks_mut::<4>().0 {
        px[0] = backdrop[0];
        px[1] = backdrop[1];
        px[2] = backdrop[2];
        px[3] = 0xff;
    }
    let wallpaper = std::sync::Arc::new(SceneAtlas {
        width: w as u32,
        height: h as u32,
        rgba,
        version: 1,
    });
    let mut input = term.cell_frame(ROWS, COLS);
    input.wallpaper = Some(wallpaper);
    assert_ne!(
        rgb_to_u32(backdrop),
        theme_bg,
        "fixture: the backdrop must differ from the theme bg or the test is vacuous"
    );
    assert!(
        input.selection_contains_cell(0, 1, false, false),
        "fixture: the selection must actually reach the sampled cell"
    );

    let mut wc = WindowCpu::new();
    let px = frame(&mut r, &mut wc, &input);
    assert_eq!(
        cell_px(&r, &px, w, 3, 10),
        rgb_to_u32(backdrop),
        "a blank default-bg cell must show the BACKDROP under a wallpaper"
    );
    assert_eq!(
        cell_px(&r, &px, w, 0, 1),
        theme_bg,
        "a SELECTION band whose colour coincides with the frame default must \
         still be painted under a wallpaper — eliding it would show the \
         backdrop through the selection"
    );
}

#[test]
fn background_opacity_keeps_the_transmittance_byte_on_default_cells() {
    let Some(mut r) = renderer() else {
        eprintln!("SKIP: no system font");
        return;
    };
    // Below 1.0, so `bg_transmittance()` is non-zero and the band's base is
    // `frame_bg | (bg_t << 24)`, not `frame_bg`.
    r.set_background_opacity(0.75);
    let theme_bg = Theme::default().bg;
    let mut term = themed_term();
    term.process(b"x");
    let input = term.cell_frame(ROWS, COLS);
    let (w, _h) = r.frame_size(ROWS, COLS);
    let mut wc = WindowCpu::new();
    let px = frame(&mut r, &mut wc, &input);

    let blank = cell_px(&r, &px, w, 3, 10);
    assert_eq!(
        blank & 0x00ff_ffff,
        theme_bg,
        "a default-bg cell keeps the frame default's RGB under opacity"
    );
    assert_ne!(
        blank >> 24,
        0,
        "a default-bg cell must carry the transmittance byte — if this is 0 the \
         base and the resolved run disagree in bits 24..32 and the elision \
         predicate is comparing the wrong value"
    );
}

#[test]
fn a_partial_damage_repaint_is_byte_identical_to_a_full_repaint() {
    let Some(mut warm) = renderer() else {
        eprintln!("SKIP: no system font");
        return;
    };
    let theme_bg = Theme::default().bg;
    let mut term = themed_term();
    term.process(
        format!(
            "first line\r\n\x1b[48;2;{};{};{}m coincident \x1b[0m\r\n\x1b[41mred\x1b[0m",
            (theme_bg >> 16) & 0xff,
            (theme_bg >> 8) & 0xff,
            theme_bg & 0xff
        )
        .as_bytes(),
    );
    let mut wc = WindowCpu::new();
    let _ = frame(&mut warm, &mut wc, &term.cell_frame(ROWS, COLS));

    // A one-row mutation: the damaged path re-establishes ONLY that row's band
    // from `fill_band_bg` and then runs the elided background pass over it.
    for step in 0..6u8 {
        term.process(format!("\x1b[1;1Hstep {step}").as_bytes());
        let input = term.cell_frame(ROWS, COLS);
        let damaged = frame(&mut warm, &mut wc, &input);
        let mut fresh = renderer().expect("system font");
        let full = fresh.render_input(&input);
        assert_eq!(
            damaged.len(),
            full.pixels.len(),
            "step {step}: frame sizes disagree"
        );
        assert!(
            damaged == full.pixels,
            "step {step}: the damaged repaint diverged from a full repaint"
        );
    }
}
