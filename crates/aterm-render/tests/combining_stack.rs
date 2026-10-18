// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Stacked combining marks: a cell's second mark on the same side of its base is lifted
//! clear of the first (above the baseline) or dropped under it (below), as far as the
//! cell has room, and never leaves the cell's band.
//!
//! The assertions are about rows of ink, not darkness: a grown stack occupies rows the
//! single mark did not. Two rigs: the default line height and a doubled one, where every
//! lift is whole. On macOS `Renderer::from_system(18.0, ..)` resolves SF Mono (an 11x21
//! cell, baseline 17: two rows of room over an acute, none under a dot below) through
//! CoreText. Placement reads the mark's ink box (`aterm_render::glyph_ink_box`), never the
//! raster path's reported box: CoreText pads a dot below a row past the baseline, so a
//! box-fed classifier would call it a straddling overlay and never stack it
//! (`second_below_mark_is_dropped_as_far_as_the_descent_allows`).

use aterm_core::terminal::Terminal;
use aterm_render::{Frame, Renderer, Theme};

const ROWS: usize = 2;
const COLS: usize = 4;

/// The renderer at `line_height` (1.0 is the default cell box).
fn renderer(line_height: f32) -> Renderer {
    Renderer::from_system(18.0, Theme::default())
        .map(|mut r| {
            // Block on lazy fallback parses so one cannot land between two renders.
            r.debug_block_on_lazy_fallbacks();
            r.set_line_height(line_height);
            r
        })
        .expect("no font: build with aterm-render's default embedded-font feature")
}

/// `text` at row 1, column 0, cursor hidden. Row 0 stays empty: an unbounded stack
/// would climb into it.
fn frame(rend: &mut Renderer, text: &str) -> Frame {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(format!("\x1b[?25l\x1b[2;1H{text}").as_bytes());
    rend.render_input(&term.cell_frame(ROWS, COLS))
}

/// Frame rows on which `a` and `b` differ; with `b` the bare base, the marks' rows.
fn differing_rows(a: &Frame, b: &Frame) -> Vec<usize> {
    assert_eq!((a.width, a.height), (b.width, b.height), "frame dims");
    (0..a.height)
        .filter(|&y| {
            a.pixels[y * a.width..(y + 1) * a.width] != b.pixels[y * b.width..(y + 1) * b.width]
        })
        .collect()
}

/// Row 1's band: `[top, bottom)` in frame rows.
fn band(rend: &Renderer) -> (usize, usize) {
    let (_, ch) = rend.cell_size();
    let top = rend.grid_top() + ch;
    (top, top + ch)
}

#[test]
fn second_above_mark_is_lifted_and_stays_in_the_band() {
    for line_height in [1.0, 2.0] {
        let mut rend = renderer(line_height);
        let bare = frame(&mut rend, "e");
        let one = frame(&mut rend, "e\u{0301}");
        let two = frame(&mut rend, "e\u{0301}\u{0301}");

        let rows_one = differing_rows(&one, &bare);
        let rows_two = differing_rows(&two, &bare);
        let (cw, ch) = rend.cell_size();
        eprintln!(
            "line height {line_height}: cell {cw}x{ch} baseline {}: one acute rows {rows_one:?}, \
             two acutes rows {rows_two:?}",
            rend.baseline(),
        );
        assert!(
            !rows_one.is_empty(),
            "non-vacuity: a single acute draws ink"
        );
        // The stack grew upward: the pair's ink starts higher and covers more rows,
        // where overprinting would only darken the same rows.
        assert!(
            rows_two[0] < rows_one[0],
            "line height {line_height}: two acutes must start above one: {rows_two:?} vs {rows_one:?}"
        );
        assert!(
            rows_two.len() > rows_one.len(),
            "line height {line_height}: two acutes must occupy more rows than one: {rows_two:?} vs {rows_one:?}"
        );
        // Bounded: nothing of the stack reaches the row above.
        let (top, _) = band(&rend);
        assert!(
            rows_two.iter().all(|&y| y >= top),
            "line height {line_height}: the stack climbed above the cell band (top {top}): {rows_two:?}"
        );
    }
}

#[test]
fn a_whole_lift_leaves_an_ink_free_row_and_the_first_mark_untouched() {
    let mut rend = renderer(2.0);
    let bare = frame(&mut rend, "e");
    let one = frame(&mut rend, "e\u{0301}");
    let two = frame(&mut rend, "e\u{0301}\u{0301}");
    let rows_one = differing_rows(&one, &bare);
    let rows_two = differing_rows(&two, &bare);
    assert!(!rows_one.is_empty(), "non-vacuity");
    // One clear row between the two acutes.
    assert!(
        rows_two.windows(2).any(|w| w[1] > w[0] + 1),
        "the acutes must be separated by an ink-free row: {rows_two:?}"
    );
    // The first acute's lift is zero, so its rows are byte-identical in both frames.
    for &y in &rows_one {
        assert_eq!(
            one.pixels[y * one.width..(y + 1) * one.width],
            two.pixels[y * two.width..(y + 1) * two.width],
            "row {y}: the first acute moved when a second was added"
        );
    }
}

#[test]
fn second_below_mark_is_dropped_as_far_as_the_descent_allows() {
    for line_height in [1.0, 2.0] {
        let mut rend = renderer(line_height);
        let bare = frame(&mut rend, "e");
        let rows_one = differing_rows(&frame(&mut rend, "e\u{0323}"), &bare);
        let rows_two = differing_rows(&frame(&mut rend, "e\u{0323}\u{0323}"), &bare);
        eprintln!(
            "line height {line_height}: one dot below rows {rows_one:?}, two dots below rows {rows_two:?}"
        );
        assert!(
            !rows_one.is_empty(),
            "non-vacuity: a single dot below draws ink"
        );
        // A below stack never moves its first mark, loses ink, or leaves the band;
        // with no room in the descent it overprints rather than being cut off.
        let (_, bottom) = band(&rend);
        assert!(
            rows_two[0] == rows_one[0] && rows_two.last() >= rows_one.last(),
            "line height {line_height}: the first dot moved or the pair lost rows: {rows_two:?} vs {rows_one:?}"
        );
        assert!(
            rows_two.iter().all(|&y| y < bottom),
            "line height {line_height}: the stack dropped below the cell band (bottom {bottom}): {rows_two:?}"
        );
        if line_height > 1.0 {
            assert!(
                rows_two.last() > rows_one.last() && rows_two.len() > rows_one.len(),
                "line height {line_height}: two dots below must end below one: {rows_two:?} vs {rows_one:?}"
            );
        }
    }
}
