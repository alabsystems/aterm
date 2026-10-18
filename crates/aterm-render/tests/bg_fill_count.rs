// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The background-fill elision as a count: a repainted band is filled once, not twice.
//!
//! `tests/bg_base_elision.rs` pins the elision byte for byte but cannot see it go away,
//! since repainting a band's base colour over itself is byte-identical. A count is exact,
//! machine-independent and runs with the ordinary tests; it guards how many fills a frame
//! emits, not what each costs.
//!
//! With no wallpaper, every background run whose colour is the band's own base is
//! skipped, so
//!
//! ```text
//!     emitted + at_base == total
//! ```
//!
//! with `at_base > 0` for the fixture to prove anything. Under a live wallpaper nothing may
//! be elided: a pixel property, pinned by `tests/bg_base_elision.rs`.
//!
//! `render_row_bg` elides at three sites, each reached by its own fixture:
//!
//! | site | where | fixture |
//! |------|-------|---------|
//! | mid-run flush of the uniform coalescing arm | `run_bg != band_base` on the colour change | [`a_base_run_that_ends_mid_row_is_elided_at_the_mid_run_flush`] |
//! | final flush at end of row | `run_bg != band_base` after the column loop | [`a_plain_frame_never_fills_a_band_with_the_colour_it_already_carries`] |
//! | per-column mixed (split-pane) arm | `band_base != Some(color)` | [`a_composed_split_row_elides_the_pane_sitting_on_the_frame_default`] |
//!
//! A new elision site needs a new fixture, and coverage is claimed by mutating that site
//! alone and seeing this file fail. Each fixture's reach assertions make a fixture that
//! stops entering its site fail instead of passing vacuously.

use aterm_core::grid::LineSize;
use aterm_core::render::LineSizeSpan;
use aterm_core::terminal::Terminal;
use aterm_render::{DefaultBgSpan, Renderer, Theme, WindowCpu, row_is_uniform};

const ROWS: usize = 6;
const COLS: usize = 20;

/// A terminal whose live default background is the renderer theme's, as in the shipping
/// app. A bare `Terminal::new` publishes `default_bg = COLOR_UNSET` and renders VT black
/// over the theme clear, which reaches none of this.
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

#[test]
fn a_plain_frame_never_fills_a_band_with_the_colour_it_already_carries() {
    let mut r = Renderer::from_system(18.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");
    let mut term = themed_term();
    term.process(b"hello");
    let input = term.cell_frame(ROWS, COLS);
    let mut wc = WindowCpu::new();
    let _ = r.render_input_cached(&mut wc, &input);

    let (total, at_base) = r.last_bg_runs();
    let emitted = r.last_bg_fills();

    // Reach: without base-coloured runs the identity below holds vacuously.
    assert!(total > 0, "the frame resolved no background run at all");
    assert!(
        at_base > 0,
        "no run in this frame carried the band's base colour ({at_base}/{total}) \
         — the fixture never reaches the state FRM-1 removes, so the count \
         identity would hold vacuously"
    );

    // The claim: every base-coloured run was skipped.
    assert_eq!(
        emitted + at_base,
        total,
        "of {total} background run(s), {at_base} carried the band's base colour \
         and {emitted} fill(s) were emitted. FRM-1 says those two partition the \
         runs — a band is filled ONCE. `emitted == total` means every repainted \
         band is being filled twice with byte-identical colour again, which no \
         pixel assertion can see."
    );
}

/// A packed colour as a cell's `[r, g, b]` (`RenderCell::bg` is bytes).
fn rgb_bytes(c: u32) -> [u8; 3] {
    [
        ((c >> 16) & 0xff) as u8,
        ((c >> 8) & 0xff) as u8,
        (c & 0xff) as u8,
    ]
}

/// Site coverage: the mid-run flush of the uniform coalescing arm. A plain row is one
/// base-coloured run leaving through the final flush; an SGR-background span between two
/// default-background spans makes the leading base run end on a colour change, so it can
/// only leave through the mid-run flush.
#[test]
fn a_base_run_that_ends_mid_row_is_elided_at_the_mid_run_flush() {
    let mut r = Renderer::from_system(18.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");
    let mut term = themed_term();
    // Cursor hidden: this fixture is background-run structure only.
    term.process(b"\x1b[?25lab\x1b[48;2;200;40;40mXY\x1b[0mcd");
    let input = term.cell_frame(ROWS, COLS);

    // The mid-run argument holds only while every counted run comes from one row.
    assert_eq!(
        input.cells[0].len(),
        6,
        "fixture: row 0 must materialize exactly the six written cells"
    );
    assert!(
        input.cells[1..].iter().all(Vec::is_empty),
        "fixture: only row 0 may be materialized, so every counted run is its own"
    );

    let mut wc = WindowCpu::new();
    let _ = r.render_input_cached(&mut wc, &input);
    let (total, at_base) = r.last_bg_runs();
    let emitted = r.last_bg_fills();

    // Reach of the mid-run flush: at most one run per row leaves through the final
    // flush, so of two base-coloured runs on one row at least one left mid-run.
    assert!(
        at_base >= 2,
        "row 0 resolved {at_base} base-coloured run(s) of {total} — this fixture \
         needs TWO (base | SGR | base), because only then must one of them have \
         left through the MID-RUN flush rather than the end-of-row one. With \
         fewer, the row coalesced differently and this file is back to covering \
         the final flush alone"
    );
    // The SGR span really is a distinct colour, so the row is split.
    assert!(
        emitted > 0,
        "no fill was emitted at all — the SGR background span did not survive \
         into the frame, so nothing forced the base run to end mid-row"
    );

    // The claim: every base-coloured run was skipped, including the mid-row one.
    assert_eq!(
        emitted + at_base,
        total,
        "of {total} background run(s), {at_base} carried the band's base colour \
         and {emitted} fill(s) were emitted. The MID-RUN flush is repainting a \
         band with the colour it already carries."
    );
}

/// Site coverage: the per-column mixed (split-pane) arm, which no single-pane frame
/// reaches (`row_is_uniform` is true for all of them). The left pane sits on a DEC
/// double-width line, which makes the row mixed; the right pane has its own default
/// background (`DefaultBgSpan`). The left pane resolves to the band's base and must be
/// skipped, the right to another colour and must be filled.
#[test]
fn a_composed_split_row_elides_the_pane_sitting_on_the_frame_default() {
    let mut r = Renderer::from_system(18.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");
    let theme_bg = Theme::default().bg;
    let mut term = themed_term();
    term.process(b"\x1b[?25l");
    let mut input = term.cell_frame(ROWS, COLS);
    // A frame still publishing COLOR_UNSET would reach none of this.
    assert_eq!(
        input.default_bg, theme_bg,
        "fixture: the frame's live default background must BE the theme's"
    );

    const SPLIT: usize = COLS / 2;
    let pane_bg: u32 = 0x0024_4668;
    assert_ne!(
        pane_bg, theme_bg,
        "fixture: the right pane's default must differ from the frame clear, or \
         nothing on this row can be counted as emitted"
    );

    // The compositor's frame shape: one line-size span for the DEC pane (what
    // `row_is_uniform` reads), the other pane unclaimed, `line_sizes[row]` summarizing.
    input.line_size_spans.resize_with(ROWS, Vec::new);
    input.line_size_spans[0] = vec![LineSizeSpan::new(0, SPLIT, LineSize::DoubleWidth)];
    input.line_sizes[0] = LineSize::DoubleWidth;
    input.default_bg_spans.resize_with(ROWS, Vec::new);
    input.default_bg_spans[0] = vec![DefaultBgSpan::new(SPLIT, COLS, pane_bg)];

    // Materialize the whole row so the prefix loop walks it (the sparse tail is a
    // separate, uncounted path): the left half at the frame default, the right half
    // at its pane's default.
    let blank = term.implicit_blank_render_cell();
    input.cells[0].resize(COLS, blank);
    for (c, cell) in input.cells[0].iter_mut().enumerate() {
        cell.ch = ' ';
        cell.bg = rgb_bytes(if c < SPLIT { theme_bg } else { pane_bg });
    }

    assert!(
        !row_is_uniform(&input, 0),
        "fixture: row 0 must be MIXED, or this walks the coalescing arm and \
         covers the same two sites the fixtures above already do"
    );
    assert!(
        input.cells[1..].iter().all(Vec::is_empty),
        "fixture: only row 0 may be materialized, so every counted run is its own"
    );

    let mut wc = WindowCpu::new();
    let _ = r.render_input_cached(&mut wc, &input);
    let (total, at_base) = r.last_bg_runs();
    let emitted = r.last_bg_fills();

    // Reach: the mixed arm counts one run per column (each column clamps to its own
    // pane's run box), so `COLS` runs proves the per-column arm did the walking.
    assert_eq!(
        total, COLS as u32,
        "the composed row resolved {total} run(s), not one per column — the \
         frame is no longer taking the MIXED arm, so this fixture has stopped \
         covering it"
    );
    assert!(
        at_base > 0,
        "no column of the composed row carried the band's base colour \
         ({at_base}/{total}) — the fixture never reaches the state FRM-1 \
         removes on this path, so the identity below would hold vacuously"
    );
    assert!(
        emitted > 0,
        "the composed row emitted no fill at all — the right pane's own default \
         background never reached the frame, so only one side of the mixed \
         decision is live"
    );

    // The claim: every base-coloured column was skipped.
    assert_eq!(
        emitted + at_base,
        total,
        "of {total} per-column background run(s) on the composed row, {at_base} \
         carried the band's base colour and {emitted} fill(s) were emitted. The \
         SPLIT-PANE path is filling a band with the colour it already carries."
    );
}
