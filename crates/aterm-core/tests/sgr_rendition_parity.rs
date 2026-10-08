// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Differential oracle for the SGR rendition path.
//!
//! `handle_sgr` / `csi_dispatch_sgr_fast` route the same rendition through a
//! dozen shapes: single-param fast paths, the 3-param 256-colour shape, the
//! 4-param attribute+256-colour shape, the 5- and 10-param truecolour shapes,
//! the colon (ISO 8613-3) sub-parameter path, and the generic loop.
//!
//! Invariant: two byte sequences that denote the same rendition render
//! identically and arm the same background for erase/scroll (BCE). It asserts
//! observable rendition behaviour only, never internal style ids.

use aterm_core::terminal::{RenderCell, Terminal};

const ROWS: u16 = 6;
const COLS: u16 = 20;

/// Column 0 of `row`, as the engine itself resolves it.
///
/// A grid row is sparse: a row an erase left at its default background stores
/// no cells, so `render_row(..).first()` is `None`, and that absence is the
/// observable. `implicit_blank_render_cell` is the engine's answer for an
/// unmaterialized column (what hosts pad snapshot rows with), resolved through
/// the same palette / default-colour / DECSCNM path as a stored cell, so an
/// armed BCE background (a stored cell) and an unarmed one (the implicit blank)
/// compare directly.
fn cell_at(t: &Terminal, row: usize) -> RenderCell {
    t.render_row(row)
        .first()
        .copied()
        .unwrap_or_else(|| t.implicit_blank_render_cell())
}

/// Feed `seq` to a fresh terminal, print `X`, and return the rendered cell.
fn cell_after(seq: &str) -> RenderCell {
    let mut t = Terminal::new(ROWS, COLS);
    t.process(b"\x1b[H");
    t.process(seq.as_bytes());
    t.process(b"X");
    cell_at(&t, 0)
}

/// Feed `seq`, then erase the whole screen, and return the rendered cell at
/// (1, 0) — a cell the erase filled, never one the writer touched. This is the
/// BCE cursor template's observable: `set_cursor_template` is what makes an
/// erase (and a scroll, and an autowrap) inherit the current background.
fn erased_cell_after(seq: &str) -> RenderCell {
    let mut t = Terminal::new(ROWS, COLS);
    t.process(b"\x1b[H");
    t.process(seq.as_bytes());
    t.process(b"\x1b[2J");
    cell_at(&t, 1)
}

/// Feed `seq`, then scroll one line in at the bottom via LF on the last row.
/// The scrolled-in blank is filled from the same template.
fn scrolled_in_cell_after(seq: &str) -> RenderCell {
    let mut t = Terminal::new(ROWS, COLS);
    t.process(b"\x1b[H");
    t.process(seq.as_bytes());
    // Park on the last row, then LF to scroll.
    t.process(format!("\x1b[{ROWS};1H").as_bytes());
    t.process(b"\n");
    cell_at(&t, usize::from(ROWS - 1))
}

/// Every entry denotes one rendition written two ways. Left is the shape with a
/// dedicated fast path; right reaches the same state by another route (a second
/// CSI, the colon form, or a shape that falls through to the generic loop).
const EQUIVALENT_ENCODINGS: &[(&str, &str, &str)] = &[
    // 3-param 256-colour fg vs the ISO 8613-3 colon form (generic loop).
    ("indexed fg", "\x1b[38;5;202m", "\x1b[38:5:202m"),
    // 3-param 256-colour bg vs colon form.
    ("indexed bg", "\x1b[48;5;19m", "\x1b[48:5:19m"),
    // 4-param attribute + 256-colour fg vs two single-param CSIs.
    (
        "bold + indexed fg",
        "\x1b[1;38;5;202m",
        "\x1b[1m\x1b[38;5;202m",
    ),
    // 4-param attribute + 256-colour bg vs two single-param CSIs.
    (
        "underline + indexed bg",
        "\x1b[4;48;5;19m",
        "\x1b[4m\x1b[48;5;19m",
    ),
    // 5-param truecolour fg vs the colon form with an empty colour space.
    (
        "truecolour fg",
        "\x1b[38;2;10;20;30m",
        "\x1b[38:2::10:20:30m",
    ),
    // 5-param truecolour bg vs colon form.
    (
        "truecolour bg",
        "\x1b[48;2;40;50;60m",
        "\x1b[48:2::40:50:60m",
    ),
    // 10-param combined fg+bg truecolour vs two separate CSIs.
    (
        "truecolour fg+bg",
        "\x1b[38;2;10;20;30;48;2;40;50;60m",
        "\x1b[38;2;10;20;30m\x1b[48;2;40;50;60m",
    ),
    // Single-param ANSI colours vs the same codes inside a multi-param CSI
    // (which takes the generic loop).
    ("ansi fg+bg", "\x1b[31m\x1b[42m", "\x1b[31;42m"),
    // Bright ANSI via the 90/100 range vs the 256-colour indices they alias.
    ("bright fg", "\x1b[91m", "\x1b[38;5;9m"),
    ("bright bg", "\x1b[101m", "\x1b[48;5;9m"),
    // Return-to-default reached by SGR 0 vs by the individual reset codes: the
    // second route never calls `reset_sgr`, so it is the one that goes stale if
    // a shape forgets to refresh the writer caches.
    (
        "return to default",
        "\x1b[31;42;1m\x1b[0m",
        "\x1b[31;42;1m\x1b[39;49;22m",
    ),
];

#[test]
fn equivalent_sgr_encodings_render_identically() {
    for (name, a, b) in EQUIVALENT_ENCODINGS {
        assert_eq!(
            cell_after(a),
            cell_after(b),
            "{name}: two encodings of the same rendition rendered differently \
             ({a:?} vs {b:?}) — a per-shape SGR fast path is refreshing a \
             different set of cached fields than the generic path"
        );
    }
}

#[test]
fn equivalent_sgr_encodings_arm_the_same_bce_background() {
    for (name, a, b) in EQUIVALENT_ENCODINGS {
        assert_eq!(
            erased_cell_after(a),
            erased_cell_after(b),
            "{name}: {a:?} and {b:?} left DIFFERENT BCE cursor templates — an \
             erase after one paints a different background than after the other \
             (#7522)"
        );
        assert_eq!(
            scrolled_in_cell_after(a),
            scrolled_in_cell_after(b),
            "{name}: {a:?} and {b:?} filled a scrolled-in line differently — the \
             BCE cursor template is out of sync with the rendition (#7522)"
        );
    }
}

#[test]
fn erase_after_bg_sgr_inherits_that_background() {
    // Anchors the pair test above to an absolute: the erased cell must actually
    // carry the SGR background, not merely agree with its twin.
    let painted = erased_cell_after("\x1b[48;5;19m");
    let plain = erased_cell_after("");
    assert_ne!(
        painted.bg, plain.bg,
        "erase after `\\x1b[48;5;19m` painted the DEFAULT background — the BCE \
         cursor template was never armed"
    );
    let restored = erased_cell_after("\x1b[48;5;19m\x1b[49m");
    assert_eq!(
        restored.bg, plain.bg,
        "erase after returning the background to default still painted the old \
         background — the BCE cursor template was never re-armed"
    );
}

#[test]
fn indexed_fg_after_truecolour_fg_is_not_contaminated() {
    // Prime the rendition cache with an indexed rendition, spend one on
    // truecolour, and return to the indexed one: the cached style-extras flag
    // must follow.
    let mut t = Terminal::new(ROWS, COLS);
    t.process(b"\x1b[H\x1b[38;5;1mA");
    t.process(b"\r\n\x1b[38;2;9;9;9mB");
    t.process(b"\r\n\x1b[38;5;1mC");

    let first = cell_at(&t, 0);
    let rgb = cell_at(&t, 1);
    let again = cell_at(&t, 2);

    assert_eq!((first.ch, rgb.ch, again.ch), ('A', 'B', 'C'));
    assert_eq!(
        first.fg, again.fg,
        "the same indexed foreground rendered differently before and after a \
         truecolour rendition — the colour cache carried RGB state across"
    );
    assert_ne!(
        rgb.fg, again.fg,
        "the truecolour foreground survived into the following indexed rendition"
    );
}

#[test]
fn many_distinct_truecolours_all_render_exactly() {
    // Renditions are stored inline in the cell (plus the extras RGB ring for
    // 24-bit overflow); no per-terminal style table is consulted, so the number
    // of distinct colours a session has already used cannot change what a cell
    // paints. 5 000 distinct triples is far past any cache-sized working set.
    let mut t = Terminal::new(ROWS, COLS);
    let mut sampled = Vec::new();
    for n in 0u32..5_000 {
        // Byte-wise construction (no `as` casts): every triple in 0..5_000 is
        // distinct — `r` selects the 1 024-block and `(g, b)` recover n mod 1 024.
        let r = u8::try_from(n >> 10).expect("n < 5_120 so n >> 10 < 5");
        let g = u8::try_from((n >> 2) & 0xFF).expect("masked to a byte");
        let b = u8::try_from(n & 0xFF).expect("masked to a byte");
        t.process(b"\x1b[H");
        t.process(format!("\x1b[38;2;{r};{g};{b}mZ").as_bytes());
        if n % 977 == 0 {
            sampled.push((n, [r, g, b]));
        }
    }
    // Re-drive each sampled colour last so it owns the cell, and assert it
    // paints exactly — a late-session rendition must not be degraded.
    for (n, rgb) in sampled {
        t.process(b"\x1b[H");
        t.process(format!("\x1b[38;2;{};{};{}mZ", rgb[0], rgb[1], rgb[2]).as_bytes());
        let cell = cell_at(&t, 0);
        assert_eq!(
            cell.fg, rgb,
            "distinct truecolour #{n} did not render its own value"
        );
    }
}
