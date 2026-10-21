// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Actual bundled-font pixels for the small footer's native combining overlays.
//! No system-font discovery, window, atlas mock or host UI is involved.

use super::*;
use aterm_core::terminal::Script;
use aterm_core::terminal::Terminal;

fn renderer() -> Renderer {
    Renderer::from_bytes(embedded_font(), 24.0, Theme::default()).expect("bundled font")
}

fn input(script: Script, marks: &[char], double: bool) -> RenderInput {
    let mut term = Terminal::new(3, 12);
    term.process(b"\x1b[?25l\x1b[2;4Hx");
    let mut input = term.cell_frame(3, 12);
    input.cells[1][3].script = script;
    for row in &mut input.cells {
        for cell in row {
            cell.fg = [255, 255, 255];
            cell.bg = [0, 0, 0];
        }
    }
    if !marks.is_empty() {
        input.combining[1] = vec![(3, marks.into())];
    }
    if double {
        input.line_sizes[1] = aterm_core::grid::LineSize::DoubleWidth;
    }
    input
}

fn mark_pixels(
    renderer: &mut Renderer,
    script: Script,
    marks: &[char],
    double: bool,
) -> Vec<(i32, i32)> {
    let bare = renderer.render_input(&input(script, &[], double));
    let marked = renderer.render_input(&input(script, marks, double));
    assert_eq!((bare.width, bare.height), (marked.width, marked.height));
    let width = marked.width;
    marked
        .pixels
        .iter()
        .zip(&bare.pixels)
        .enumerate()
        .filter_map(|(at, (marked, bare))| {
            (marked != bare).then_some(((at % width) as i32, (at / width) as i32))
        })
        .collect()
}

#[test]
fn ordinary_marks_keep_their_original_keys_and_pixels_after_small_script_rasters() {
    let mut r = renderer();
    let mut ordinary = input(Script::Normal, &['\u{0301}', '\u{0323}'], false);
    ordinary.cells[1][3].bold = true;
    ordinary.cells[1][3].italic = true;
    for mark in ['\u{0301}', '\u{0323}'] {
        let original = r.glyph_key(mark);
        assert_eq!(r.combining_mark_key(mark, &ordinary.cells[1][3]), original);
    }
    let before = r.render_input(&ordinary);
    assert!(!mark_pixels(&mut r, Script::Normal, &['\u{0301}'], false).is_empty());
    for nudge in [-8, 0, 8] {
        r.render_input(&input(
            Script::Sub { nudge },
            &['\u{0301}', '\u{0323}'],
            false,
        ));
    }
    let after = r.render_input(&ordinary);
    assert_eq!(
        before.pixels, after.pixels,
        "small-script masks cannot replace ordinary mark rasters"
    );
}

#[test]
fn noncomposable_and_stacked_small_script_marks_draw_scaled_ink_with_the_base() {
    let mut r = renderer();
    let mark = '\u{0301}';
    let cell = input(Script::Sub { nudge: 0 }, &[mark], false).cells[1][3];
    let normal_key = r.glyph_key(mark);
    let small_key = r.combining_mark_key(mark, &cell);
    assert_ne!(small_key, normal_key);
    assert_eq!(small_key.style.script(), StyleBits::SUB);
    let normal = mark_pixels(&mut r, Script::Normal, &[mark], false);
    let small = mark_pixels(&mut r, cell.script, &[mark], false);
    let stack = mark_pixels(&mut r, cell.script, &[mark, mark], false);
    assert!(
        !normal.is_empty() && !small.is_empty() && !stack.is_empty(),
        "all fixtures draw actual accent ink"
    );
    assert!(
        small.len() < normal.len(),
        "small text must not keep a full-size accent"
    );
    assert!(
        small.iter().map(|p| p.1).min() > normal.iter().map(|p| p.1).min(),
        "the accent follows the small base's lowered baseline"
    );
    assert_ne!(
        stack, small,
        "stacked marks remain visible rather than overprinting one raster"
    );
    let (_, ch) = r.cell_size();
    let top = (r.grid_top() + ch) as i32;
    assert!(
        stack.iter().all(|p| p.1 >= top && p.1 < top + ch as i32),
        "the stack stays inside its own row"
    );
}

#[test]
fn small_script_mark_ink_follows_horizontal_nudge_on_normal_and_double_width_rows() {
    let mut r = renderer();
    for double in [false, true] {
        let zero = mark_pixels(&mut r, Script::Sub { nudge: 0 }, &['\u{0301}'], double);
        assert!(!zero.is_empty(), "nudge fixture carries visible accent ink");
        let advance = r.cell_size().0 * if double { 2 } else { 1 };
        for nudge in [-8, 8] {
            let script = Script::Sub { nudge };
            let moved = mark_pixels(&mut r, script, &['\u{0301}'], double);
            let shift = mark_script_shift(script, advance);
            assert_ne!(shift, 0);
            let expected: Vec<_> = zero.iter().map(|&(x, y)| (x + shift, y)).collect();
            assert_eq!(
                moved, expected,
                "nudge {nudge}, double-width {double}: mark centring cannot cancel the base's sideways motion"
            );
        }
    }
}
