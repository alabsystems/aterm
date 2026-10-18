// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// `render_row_bg` emits no background fill whose colour the band's base already
// carries (the full path's `frame_bg | bg_t<<24` clear, the damaged path's band reset).
// The elision stores nothing a fill would change, so it is byte-identical as long as
// the predicate is right; these fixtures pin where a naive predicate breaks:
//
//   1. an SGR background equal to the frame default: `RenderCell.bg` is a bare
//      `[u8; 3]`, so it must render the same pixel as a default cell;
//   2. a live wallpaper: the base is backdrop texels, so nothing may be elided against
//      `frame_bg`;
//
// (background opacity below 1, the base carrying a transmittance byte, is
// `background_and_cursor_opacity_gpu_match_cpu` in `aterm-gpu`), plus a partial-damage
// repaint asserted byte-identical to a fresh full repaint.

use aterm_core::render::SceneAtlas;
use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme, WindowCpu, rgb_to_u32};

const ROWS: usize = 6;
const COLS: usize = 20;

fn renderer() -> Renderer {
    Renderer::from_system(18.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature")
}

/// A terminal whose live default background is the renderer theme's, as in the shipping
/// app (`applied_terminal_config*` pins the engine default bg to `theme.bg`): the state
/// in which the base clear and a default cell share a colour.
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
fn frame(
    r: &mut Renderer,
    wc: &mut WindowCpu,
    input: &aterm_core::render::RenderInput,
) -> Vec<u32> {
    let view = r.render_input_cached(wc, input);
    view.pixels().to_vec()
}

#[test]
fn sgr_background_equal_to_the_frame_default_renders_the_same_pixel() {
    let mut r = renderer();
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
    // Both cells resolved to concrete colours; the coincident one is the frame default.
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
    // Under a wallpaper the band's base is backdrop texels, not `frame_bg`, yet a
    // selection band takes `theme.selection`, which a theme may set equal to its
    // background. Without the `!wallpaper` conjunct the predicate would elide that band
    // and the picture would show through the selection. (An SGR cell whose bg equals the
    // pane default is not this case: `resolve` already returns `None` for it.)
    let theme_bg = Theme::default().bg;
    let theme = Theme {
        selection: theme_bg,
        ..Theme::default()
    };
    let mut r = Renderer::from_system(18.0, theme)
        .expect("no font: build with aterm-render's default embedded-font feature");
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
fn a_partial_damage_repaint_is_byte_identical_to_a_full_repaint() {
    let mut warm = renderer();
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

    // A one-row mutation: the damaged path resets only that row's band, then runs the
    // elided background pass over it.
    for step in 0..6u8 {
        term.process(format!("\x1b[1;1Hstep {step}").as_bytes());
        let input = term.cell_frame(ROWS, COLS);
        let damaged = frame(&mut warm, &mut wc, &input);
        let mut fresh = renderer();
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
