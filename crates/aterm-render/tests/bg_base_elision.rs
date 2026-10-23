// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// `render_row_bg` elision fixtures for SGR default-coincident backgrounds,
// wallpaper backdrops, and partial-damage repaints.

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

/// Terminal with its live default background set to `Theme::default().bg`.
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

/// Samples a background pixel inside cell `(row, col)` outside glyph ink.
fn cell_px(r: &Renderer, pixels: &[u32], w: usize, row: usize, col: usize) -> u32 {
    let (cw, ch) = r.cell_size();
    let x = r.pad() + col * cw + 1;
    let y = r.grid_top() + row * ch + 1;
    pixels[y * w + x]
}

/// Renders `input` through the damage-tracked entry and returns the framebuffer.
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
    // Under a wallpaper the base is backdrop texels, so a selection matching `theme.bg` must not be elided.
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
