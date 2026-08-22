// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! DIFFERENTIAL ORACLE for the SGR rendition path.
//!
//! `handle_sgr` / `csi_dispatch_sgr_fast` route the SAME rendition through a
//! dozen different shapes: single-param fast paths, the 3-param 256-colour
//! shape, the 4-param attribute+256-colour shape, the 5- and 10-param truecolour
//! shapes, the colon (ISO 8613-3) sub-parameter path, and the generic loop.
//! Historically each shape called its own `update_style_id_*` specialization,
//! and those specializations differed in WHICH cached fields they refreshed —
//! which is exactly how a rendition can be "set" and still paint the previous
//! colour (the `\x1b[39;49m` staleness the old fast path documented in
//! handler_sgr.rs, and the `update_fg_cache_indexed` cache-hit arm that never
//! refreshed `cached_has_style_extras`).
//!
//! This suite pins the invariant those specializations must satisfy and that no
//! per-shape test can: **two byte sequences that denote the same rendition must
//! render identically, and must arm the same background for erase/scroll (BCE,
//! #7522)**. It is written to hold on BOTH sides of the style-interner deletion —
//! it asserts observable rendition behaviour, never a `StyleId` — so it doubles
//! as the parity gate for that change.

use aterm_core::terminal::{RenderCell, Terminal};

const ROWS: u16 = 6;
const COLS: u16 = 20;

/// Column 0 of `row`, as the engine itself resolves it.
///
/// `render_row` returns one `RenderCell` per STORED column, and a grid row is
/// sparse: a row an erase left at its default background stores no cells at all,
/// so `first()` is `None`. That absence is not a missing observable — it IS the
/// observable. `implicit_blank_render_cell` is the engine's own answer for an
/// unmaterialized column (the same value hosts pad snapshot rows with), resolved
/// through the same palette / default-colour / DECSCNM path as a stored cell. So
/// an armed BCE background shows up as a stored cell carrying it, an unarmed one
/// as the implicit blank, and the two are directly comparable.
///
/// The original helpers called `.expect("row N has cells")` here, which made
/// every BCE assertion below panic before it could compare anything — on BOTH
/// sides of the interner deletion. The BCE half of this oracle had never once
/// run until this was fixed.
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
/// (1, 0) — a cell the erase FILLED, never one the writer touched. This is the
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
/// The scrolled-in blank is filled from the same template (#7522).
fn scrolled_in_cell_after(seq: &str) -> RenderCell {
    let mut t = Terminal::new(ROWS, COLS);
    t.process(b"\x1b[H");
    t.process(seq.as_bytes());
    // Park on the last row, then LF to scroll.
    t.process(format!("\x1b[{ROWS};1H").as_bytes());
    t.process(b"\n");
    cell_at(&t, usize::from(ROWS - 1))
}

/// Every entry denotes ONE rendition written two ways. Left is the shape with a
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
    // The rendition cache used to be refreshed by TWO different code paths
    // depending on whether the interner's L1/L2 probe hit: the hit arm called
    // `update_fg_cache_indexed`, which left `cached_has_style_extras` at its
    // previous value. Priming the cache with the indexed rendition, spending one
    // rendition on truecolour, and returning to the indexed one is exactly the
    // sequence that took the hit arm with a stale RGB flag in tow.
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
    // Renditions are stored INLINE in the cell (plus the extras RGB ring for
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
    // Re-drive each sampled colour LAST so it owns the cell, and assert it
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
