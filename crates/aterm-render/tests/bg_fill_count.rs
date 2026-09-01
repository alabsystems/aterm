// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! FRM-1 AS A COUNT: a repainted band is filled ONCE, not twice.
//!
//! # The hole this closes
//!
//! `tests/bg_base_elision.rs` is FRM-1's correctness gate and it is a good one —
//! it pins the four fixtures where a naive elision predicate breaks, byte for
//! byte. What it cannot do is notice the elision GOING AWAY. Painting a band's
//! base colour and then painting the identical colour over it again is
//! byte-identical output; every pixel assertion in that file stays green while
//! the frame pays for a whole extra framebuffer of stores. The measured win
//! (keystroke_echo 43.24 -> 41.33 us, scrolled_back_wheel 102.38 -> 96.65 us,
//! 7/7 rounds) would be handed back in silence.
//!
//! Only the frame's own bench could see it, and the bench is not in the merge
//! contract — `xtask gate perf` is a TIMING gate that needs release builds of
//! several harnesses and has not run green in a month. A COUNT has none of those
//! problems: it is exact, machine-independent, cannot flake under load, and
//! rides `cargo test` — hence `tools/verify.sh --fast`, the merge contract — at
//! zero marginal cost. (This named "and the pre-push hook" until 2026-08-31.
//! It does not ride one: `.githooks/pre-push` was demoted to ADVISORY on
//! 2026-08-24 and its whole body is one printf and `exit 0`.)
//!
//! WHAT A COUNT CANNOT CATCH: a constant-factor slowdown with the counts
//! unchanged. If `fill_rect` itself became 3x slower this file stays green. It
//! guards the STRUCTURE of the win — how many fills a frame emits — not its
//! cost.
//!
//! # The identity being pinned
//!
//! With no wallpaper live, every background run whose colour is the band's own
//! base is skipped, so
//!
//! ```text
//!     emitted + at_base == total
//! ```
//!
//! and, for the fixture to prove anything, `at_base > 0` — the redundant state
//! must actually be reached. A giveback (the `.filter(|_| run_bg != band_base)`
//! conjuncts deleted) reads `emitted == total` with `at_base` unchanged, and
//! fails here.
//!
//! The other side is pinned too: under a live WALLPAPER the band's base is
//! backdrop texels rather than one scalar, nothing may be elided, and the same
//! identity degenerates to `emitted == total` with `at_base == 0`. A predicate
//! that dropped the `!wallpaper` conjunct — punching the picture through a
//! selection band — moves this count before it moves a pixel anyone reports.
//!
//! # THE ELISION IS THREE COMPILE SITES, AND EACH NEEDS ITS OWN FIXTURE
//!
//! `render_row_bg` skips a base-coloured background in three separate places,
//! and "delete the elision and watch this file go red" is only honest if it is
//! checked ONE SITE AT A TIME. It was not, when this file landed:
//!
//! | site | where | fixture that reaches it |
//! |------|-------|-------------------------|
//! | mid-run flush of the uniform coalescing arm | `run_bg.filter(..)` on the colour change | [`a_base_run_that_ends_mid_row_is_elided_at_the_mid_run_flush`] |
//! | final flush at end of row | `run_bg.filter(..)` after the column loop | [`a_plain_frame_never_fills_a_band_with_the_colour_it_already_carries`] |
//! | per-column MIXED (split-pane) arm | `band_base != Some(color)` | [`a_composed_split_row_elides_the_pane_sitting_on_the_frame_default`] |
//!
//! With only the first two fixtures present, deleting the MID-RUN or the MIXED
//! site alone left this file GREEN — a plain themed row is one run of the band
//! base spanning the whole row, so it reaches nothing but the final flush. Those
//! two blind spots were the materialized PREFIX and the split-pane path, i.e.
//! most of the area the measured win came from.
//!
//! MAINTENANCE RULE: a new elision site is a new fixture, and the claim that a
//! fixture covers it is made by MUTATING THAT SITE ALONE and seeing this file
//! fail — never by mutating all of them together, which is how the gap above got
//! in. Each fixture's REACH assertions exist so that a fixture which stops
//! entering its site fails loudly instead of passing vacuously.

use aterm_core::grid::LineSize;
use aterm_core::render::{LineSizeSpan, SceneAtlas};
use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;
use aterm_render::{DefaultBgSpan, Renderer, Theme, WindowCpu, rgb_to_u32, row_is_uniform};

const ROWS: usize = 6;
const COLS: usize = 20;

/// A terminal whose LIVE default background is the renderer theme's — the state
/// the shipping app is always in, and the only state in which "the base clear
/// and a default cell are the same colour" is true at all. A `Terminal::new`
/// fixture publishes `default_bg = COLOR_UNSET` and renders VT-spec BLACK over a
/// theme clear, which is the black-backed-text arrangement the product does not
/// ship — and which reaches none of this.
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
    let Some(mut r) = Renderer::from_system(18.0, Theme::default()) else {
        eprintln!("SKIP: no system font");
        return;
    };
    let mut term = themed_term();
    term.process(b"hello");
    let input = term.cell_frame(ROWS, COLS);
    let mut wc = WindowCpu::new();
    let _ = r.render_input_cached(&mut wc, &input);

    let (total, at_base) = r.last_bg_runs();
    let emitted = r.last_bg_fills();

    // REACH, first: without redundant runs in the frame there is nothing to
    // elide and the identity below is satisfied by doing nothing.
    assert!(total > 0, "the frame resolved no background run at all");
    assert!(
        at_base > 0,
        "no run in this frame carried the band's base colour ({at_base}/{total}) \
         — the fixture never reaches the state FRM-1 removes, so the count \
         identity would hold vacuously"
    );

    // THE CLAIM: every base-coloured run was SKIPPED.
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

#[test]
fn under_a_live_wallpaper_no_background_run_is_elided() {
    // The `!wallpaper` conjunct, from the cost side. Under a wallpaper the base
    // is backdrop texels, so `band_base` is `None`, nothing compares equal to
    // it, and every resolved run must be paid for.
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
    let backdrop = [0x40u8, 0x20, 0x60];
    assert_ne!(
        rgb_to_u32(backdrop),
        theme_bg,
        "fixture: the backdrop must differ from the theme bg"
    );
    let mut rgba = vec![0u8; w * h * 4];
    for px in rgba.as_chunks_mut::<4>().0 {
        px[0] = backdrop[0];
        px[1] = backdrop[1];
        px[2] = backdrop[2];
        px[3] = 0xff;
    }
    let mut input = term.cell_frame(ROWS, COLS);
    input.wallpaper = Some(std::sync::Arc::new(SceneAtlas {
        width: w as u32,
        height: h as u32,
        rgba,
        version: 1,
    }));
    assert!(
        input.selection_contains_cell(0, 1, false, false),
        "fixture: the selection must actually reach the band being counted"
    );

    let mut wc = WindowCpu::new();
    let _ = r.render_input_cached(&mut wc, &input);
    let (total, at_base) = r.last_bg_runs();
    assert!(total > 0, "the wallpaper frame resolved no background run");
    assert_eq!(
        at_base, 0,
        "under a wallpaper there is no scalar band base, so no run can be AT it"
    );
    assert_eq!(
        r.last_bg_fills(),
        total,
        "a wallpapered frame elided a background run — the selection band whose \
         colour coincides with the theme background would show the picture \
         through it"
    );
}

/// The band base as a cell's `[r, g, b]` — the composed-row fixture below hands
/// panes their live default background by colour, and `RenderCell::bg` is bytes.
fn rgb_bytes(c: u32) -> [u8; 3] {
    [
        ((c >> 16) & 0xff) as u8,
        ((c >> 8) & 0xff) as u8,
        (c & 0xff) as u8,
    ]
}

/// SITE COVERAGE — the MID-RUN flush of the uniform run-coalescing path.
///
/// # Why the plain fixture above does not reach it
///
/// A plain single-pane row of text is ONE coalesced run of the band's base
/// colour spanning the whole row, so it only ever leaves through the FINAL
/// flush at end of row. The MID-RUN flush — the one that fires when a run's
/// colour CHANGES, and the one that carries the materialized PREFIX, which is
/// where the area is — is never entered by that shape. Deleting its
/// `.filter(|_| run_bg != band_base)` ALONE therefore left this file GREEN —
/// measured: with only the two original fixtures the counts did not move by so
/// much as one fill, because neither frame ever entered that flush. A refactor
/// could hand back that half of the win in total silence.
///
/// # The shape that reaches it
///
/// An SGR-background span sits BETWEEN two default-background spans on one row.
/// The leading base-coloured run now ends on a COLOUR CHANGE rather than at
/// EOL, so it can only leave through the mid-run flush.
#[test]
fn a_base_run_that_ends_mid_row_is_elided_at_the_mid_run_flush() {
    let Some(mut r) = Renderer::from_system(18.0, Theme::default()) else {
        eprintln!("SKIP: no system font");
        return;
    };
    let mut term = themed_term();
    // Cursor hidden: this fixture is background-run STRUCTURE and nothing else.
    term.process(b"\x1b[?25lab\x1b[48;2;200;40;40mXY\x1b[0mcd");
    let input = term.cell_frame(ROWS, COLS);

    // FIXTURE SHAPE, asserted rather than assumed — the mid-run argument below
    // is only airtight while every counted run comes from ONE row.
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

    // REACH — and specifically reach of the MID-RUN flush. One row resolved TWO
    // runs carrying the band's base colour, and at most ONE run per row leaves
    // through the final flush, so at least one of those two necessarily went out
    // through the mid-run flush. That is the site this fixture exists to cover,
    // and this inequality is what proves it is still being entered.
    assert!(
        at_base >= 2,
        "row 0 resolved {at_base} base-coloured run(s) of {total} — this fixture \
         needs TWO (base | SGR | base), because only then must one of them have \
         left through the MID-RUN flush rather than the end-of-row one. With \
         fewer, the row coalesced differently and this file is back to covering \
         the final flush alone"
    );
    // The other half of the shape: the SGR span really is a distinct colour, so
    // the row is genuinely split rather than one run the walk merged away.
    assert!(
        emitted > 0,
        "no fill was emitted at all — the SGR background span did not survive \
         into the frame, so nothing forced the base run to end mid-row"
    );

    // THE CLAIM, unchanged: every base-coloured run was SKIPPED — including the
    // one that flushed mid-row.
    assert_eq!(
        emitted + at_base,
        total,
        "of {total} background run(s), {at_base} carried the band's base colour \
         and {emitted} fill(s) were emitted. The MID-RUN flush is repainting a \
         band with the colour it already carries."
    );
}

/// SITE COVERAGE — the per-column MIXED (non-uniform) path.
///
/// # Why the fixtures above do not reach it
///
/// `row_is_uniform` is true for every single-pane frame, so both fixtures above
/// take the coalescing arm and the mixed arm's own elision (`band_base !=
/// Some(color)`) is never executed. Deleting it ALONE left this file GREEN —
/// measured: with only the two original fixtures the counts did not move at all,
/// because no frame in the file carried a mixed row. That arm is the SPLIT-PANE
/// path — the composed frame whose rows are the widest the product paints — so
/// the gate was under-covering exactly where the area is.
///
/// # The shape that reaches it
///
/// A composed split row: the left pane sits on a DEC double-width line, which is
/// what puts a `LineSizeSpan` on the row and makes it MIXED, and the right pane
/// carries its OWN live default background (`DefaultBgSpan` — two panes with
/// different OSC 11 state). Both sides of the decision are then live on one row:
/// the left pane resolves to the band's own base and must be skipped, the right
/// pane resolves to a different colour and must be paid for.
#[test]
fn a_composed_split_row_elides_the_pane_sitting_on_the_frame_default() {
    let Some(mut r) = Renderer::from_system(18.0, Theme::default()) else {
        eprintln!("SKIP: no system font");
        return;
    };
    let theme_bg = Theme::default().bg;
    let mut term = themed_term();
    term.process(b"\x1b[?25l");
    let mut input = term.cell_frame(ROWS, COLS);
    // The `themed_term` trap, checked: a frame still publishing COLOR_UNSET
    // reaches none of this and the counts below would be measuring nothing.
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

    // The composed row. `line_size_spans` non-empty is exactly what
    // `row_is_uniform` reads, and this is the compositor's own frame shape: one
    // span for the DEC pane, the other pane left unclaimed, with
    // `line_sizes[row]` carrying the row-level summary.
    input.line_size_spans.resize_with(ROWS, Vec::new);
    input.line_size_spans[0] = vec![LineSizeSpan::new(0, SPLIT, LineSize::DoubleWidth)];
    input.line_sizes[0] = LineSize::DoubleWidth;
    input.default_bg_spans.resize_with(ROWS, Vec::new);
    input.default_bg_spans[0] = vec![DefaultBgSpan::new(SPLIT, COLS, pane_bg)];

    // Materialize the whole row so it is the PREFIX loop that walks it (the
    // sparse tail is a separate, uncounted path): the left half at the frame
    // default — the band's own base, nothing to write — and the right half at
    // its pane's default, which must be filled.
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

    // REACH, three-sided here. The mixed arm counts one run PER COLUMN (it
    // cannot merge — each column clamps to its own pane's run box), so a row
    // that resolved `COLS` runs is proof the per-column arm, and not the
    // coalescing one, did the walking.
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

    // THE CLAIM, unchanged: every base-coloured column was SKIPPED.
    assert_eq!(
        emitted + at_base,
        total,
        "of {total} per-column background run(s) on the composed row, {at_base} \
         carried the band's base colour and {emitted} fill(s) were emitted. The \
         SPLIT-PANE path is filling a band with the colour it already carries."
    );
}
