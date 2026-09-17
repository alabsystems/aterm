// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Stacked combining marks. A cell's second mark on the same side of its base
//! is LIFTED clear of the first (above the baseline) or DROPPED under it
//! (below), as far as the cell has room — the stack never leaves the cell's
//! band. Before `MarkStack` every mark of a cell painted at the row's one
//! anchor, so `e` + U+0301 + U+0301 was pixel-equal to `e` + U+0301 up to
//! double-coverage darkening and every mark past the first was invisible.
//!
//! Every assertion here is about ROWS of ink, not darkness: a stack that grew
//! occupies rows the single mark did not. Two rigs: the default line height and
//! a doubled one, where every lift is whole and the marks form separate bands.
//! The row numbers below are this machine's FACE as well as its rasterizer —
//! `Renderer::from_system(18.0, ..)` resolves SF Mono
//! (`/System/Library/Fonts/SFNSMono.ttf`), whose cell is 11x21 with baseline 17,
//! which is what leaves two rows of room over an acute and none under a dot
//! below; CoreText supplies the rasters.
//!
//! The placement operand is the mark's INK box, never the box its raster path
//! reports — see `the_stack_is_placed_by_ink_not_by_the_reported_raster_box`,
//! which is the end-to-end witness for the seam
//! `aterm_render::glyph_ink_box` closes.

use aterm_core::terminal::Terminal;
use aterm_render::{Frame, Renderer, Theme, glyph_ink_box};

const ROWS: usize = 2;
const COLS: usize = 4;

/// The renderer at `line_height` (1.0 is the default cell box).
fn renderer(line_height: f32) -> Option<Renderer> {
    Renderer::from_system(18.0, Theme::default()).map(|mut r| {
        // Deterministic pixels: block on the lazy fallback parses so a parse
        // landing between two renders cannot recolour a frame.
        r.debug_block_on_lazy_fallbacks();
        r.set_line_height(line_height);
        r
    })
}

/// `text` at row 1, column 0, cursor hidden. Row 0 stays empty: it is the band
/// a stack that was not bounded would climb into.
fn frame(rend: &mut Renderer, text: &str) -> Frame {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(format!("\x1b[?25l\x1b[2;1H{text}").as_bytes());
    rend.render_input(&term.cell_frame(ROWS, COLS))
}

/// Frame rows on which `a` and `b` differ anywhere — with `b` the bare base,
/// the rows the marks' ink occupies.
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
        let Some(mut rend) = renderer(line_height) else {
            eprintln!("SKIP: no system monospace font");
            return;
        };
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
        // The stack grew UPWARD: the pair's ink starts on a higher row than
        // the single mark's and covers more rows. Overprinting (the old
        // placement) only darkens the same rows, which fails both.
        assert!(
            rows_two[0] < rows_one[0],
            "line height {line_height}: two acutes must start above one: {rows_two:?} vs {rows_one:?}"
        );
        assert!(
            rows_two.len() > rows_one.len(),
            "line height {line_height}: two acutes must occupy more rows than one: {rows_two:?} vs {rows_one:?}"
        );
        // BOUNDED: nothing of the stack reaches the row above.
        let (top, _) = band(&rend);
        assert!(
            rows_two.iter().all(|&y| y >= top),
            "line height {line_height}: the stack climbed above the cell band (top {top}): {rows_two:?}"
        );
    }
}

#[test]
fn a_whole_lift_leaves_an_ink_free_row_and_the_first_mark_untouched() {
    let Some(mut rend) = renderer(2.0) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
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
    // The first acute is placed exactly as when it was alone (its lift is
    // zero), so its rows are byte-identical between the two frames.
    for &y in &rows_one {
        assert_eq!(
            one.pixels[y * one.width..(y + 1) * one.width],
            two.pixels[y * two.width..(y + 1) * two.width],
            "row {y}: the first acute moved when a second was added"
        );
    }
}

#[test]
fn acute_over_circumflex_are_two_separate_bands() {
    let Some(mut rend) = renderer(2.0) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let bare = frame(&mut rend, "e");
    let rows = differing_rows(&frame(&mut rend, "e\u{0302}\u{0301}"), &bare);
    eprintln!("circumflex + acute rows {rows:?}");
    assert!(rows.len() >= 3, "non-vacuity: two marks draw ink: {rows:?}");
    // Two marks, two bands: the lifted acute sits one clear row above the
    // circumflex, so the ink rows are not one contiguous run.
    assert!(
        rows.windows(2).any(|w| w[1] > w[0] + 1),
        "acute and circumflex must be separated by an ink-free row: {rows:?}"
    );
}

#[test]
fn second_below_mark_is_dropped_as_far_as_the_descent_allows() {
    for line_height in [1.0, 2.0] {
        let Some(mut rend) = renderer(line_height) else {
            eprintln!("SKIP: no system monospace font");
            return;
        };
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
        // A below stack never moves its first mark, never loses ink, and never
        // leaves the band — under a descent with no room it overprints as it
        // always did rather than being cut off.
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

/// WHY the stack is placed by [`glyph_ink_box`] and not by the `(height, ymin)`
/// a raster path reports: CoreText reports a box that overhangs the ink on the
/// rows, and for a mark sitting just under the baseline the overhang crosses it
/// — so a box-fed classifier calls a dot below a baseline-STRADDLING overlay,
/// which joins no stack, and every dot of a stack stays at the one anchor.
///
/// Measured on the CoreText raster of SF Mono at 18px: U+0323 COMBINING DOT
/// BELOW reports `gh 6, ymin -5` — top edge `+1`, a row ABOVE the baseline it
/// has no ink on — while its ink is `gh 3, ymin -4`, top edge `-1`. This
/// asserts the ink is a below-mark (which is what the placement must read) and
/// RECORDS the reported box, so a path whose box needs no trimming — the
/// subpixel path pads X only, and the portable one crops its pad off — is
/// visible in the log rather than silently making the sibling tests vacuous.
#[test]
fn the_stack_is_placed_by_ink_not_by_the_reported_raster_box() {
    let Some(mut rend) = renderer(2.0) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let key = rend.glyph_key('\u{0323}');
    let img = rend.glyph_image(key);
    let (gh, ymin) = (img.height() as i32, img.ymin());
    let (ink_h, ink_ymin) = glyph_ink_box(img).expect("U+0323 rasterizes to ink");
    eprintln!(
        "U+0323: reported box gh {gh} ymin {ymin} (top edge {}), ink gh {ink_h} ymin {ink_ymin} \
         (top edge {})",
        ymin + gh,
        ink_ymin + ink_h as i32,
    );
    assert!(
        ink_ymin + ink_h as i32 <= 0,
        "U+0323's INK lies wholly below the baseline — it is a below-mark on every raster path"
    );
    assert!(
        ink_h as i32 <= gh && ink_ymin >= ymin,
        "the ink box is inside the reported box"
    );
    if ymin + gh > 0 {
        // This raster path pads. The box rule would classify this below-mark as
        // a straddling overlay, so `second_below_mark_is_dropped_as_far_as_the_
        // descent_allows` above is the live regression for the operand.
        assert!(
            ink_ymin + ink_h as i32 <= 0,
            "the pad, not the glyph, is what crosses the baseline"
        );
    } else {
        eprintln!(
            "NOTE: this raster path reports a vertically unpadded box for U+0323 — the operand \
             seam is not observable here, only on the CoreText path, whose pad crosses the \
             baseline"
        );
    }
}
