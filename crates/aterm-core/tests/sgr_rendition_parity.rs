// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Differential oracle for the SGR rendition path.
//!
//! `handle_sgr` / `csi_dispatch_sgr_fast` route one rendition through many shapes: the
//! single-param fast paths, the 3-, 4-, 5- and 10-param 256-colour and truecolour shapes,
//! the colon (ISO 8613-3) form, and the generic loop. Two sequences that denote the same
//! rendition render identically and arm the same BCE background. Only observable rendition
//! is asserted, never style ids.

use aterm_core::terminal::{RenderCell, Terminal};

const ROWS: u16 = 6;
const COLS: u16 = 20;

/// Column 0 of `row` as the engine resolves it. A row an erase left at the default
/// background stores no cells; `implicit_blank_render_cell` resolves that absence through
/// the same palette path as a stored cell, so armed and unarmed BCE backgrounds compare.
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

/// Feed `seq`, erase the screen, and return the cell at (1, 0), which only the erase
/// filled: the BCE cursor template (`set_cursor_template`) as observed.
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
    t.process(format!("\x1b[{ROWS};1H").as_bytes());
    t.process(b"\n");
    cell_at(&t, usize::from(ROWS - 1))
}

/// Every entry denotes one rendition written two ways. Left is the shape with a
/// dedicated fast path; right reaches the same state by another route (a second
/// CSI, the colon form, or a shape that falls through to the generic loop).
const EQUIVALENT_ENCODINGS: &[(&str, &str, &str)] = &[
    ("indexed fg", "\x1b[38;5;202m", "\x1b[38:5:202m"),
    ("indexed bg", "\x1b[48;5;19m", "\x1b[48:5:19m"),
    (
        "bold + indexed fg",
        "\x1b[1;38;5;202m",
        "\x1b[1m\x1b[38;5;202m",
    ),
    (
        "underline + indexed bg",
        "\x1b[4;48;5;19m",
        "\x1b[4m\x1b[48;5;19m",
    ),
    // The colon form with an empty colour-space id.
    (
        "truecolour fg",
        "\x1b[38;2;10;20;30m",
        "\x1b[38:2::10:20:30m",
    ),
    (
        "truecolour bg",
        "\x1b[48;2;40;50;60m",
        "\x1b[48:2::40:50:60m",
    ),
    (
        "truecolour fg+bg",
        "\x1b[38;2;10;20;30;48;2;40;50;60m",
        "\x1b[38;2;10;20;30m\x1b[48;2;40;50;60m",
    ),
    // A multi-param CSI takes the generic loop.
    ("ansi fg+bg", "\x1b[31m\x1b[42m", "\x1b[31;42m"),
    // Bright ANSI via the 90/100 range vs the 256-colour indices they alias.
    ("bright fg", "\x1b[91m", "\x1b[38;5;9m"),
    ("bright bg", "\x1b[101m", "\x1b[48;5;9m"),
    // SGR 0 vs the individual reset codes: the second never calls `reset_sgr`, so it goes
    // stale if a shape forgets to refresh the writer caches.
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
    // The absolute behind the pair test: the erased cell carries the SGR background.
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
    // Indexed, truecolour, indexed again: the cached style-extras flag must follow.
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
    // Renditions live in the cell (24-bit overflow in the extras RGB ring), not in a style
    // table, so how many colours a session has used cannot change what a cell paints. 5 000
    // triples is far past any cache-sized working set.
    let mut t = Terminal::new(ROWS, COLS);
    let mut sampled = Vec::new();
    for n in 0u32..5_000 {
        // A distinct triple for every n in 0..5_000.
        let r = u8::try_from(n >> 10).expect("n < 5_120 so n >> 10 < 5");
        let g = u8::try_from((n >> 2) & 0xFF).expect("masked to a byte");
        let b = u8::try_from(n & 0xFF).expect("masked to a byte");
        t.process(b"\x1b[H");
        t.process(format!("\x1b[38;2;{r};{g};{b}mZ").as_bytes());
        if n % 977 == 0 {
            sampled.push((n, [r, g, b]));
        }
    }
    // Re-drive each sampled colour so it owns the cell: a late-session rendition is exact.
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

#[test]
fn a_truecolour_rewrite_carrying_extras_paints_its_own_colour() {
    // A truecolour run that also carries a link, an underline style or an SGR 58
    // colour stores its RGB beside those extras, while a plain truecolour run (or
    // a truecolour erase) stores its bytes elsewhere. Rewriting such cells must
    // paint the new colour, not the bytes the earlier writer left.
    for extras in [
        "\x1b]8;;https://e.x/\x1b\\",
        "\x1b[4:3m",
        "\x1b[4;58:2::255:0:0m",
    ] {
        let mut t = Terminal::new(ROWS, COLS);
        t.process(b"\x1b[H\x1b[38;2;10;20;30mhello world\r");
        t.process(format!("\x1b[38;2;200;0;0m{extras}hello").as_bytes());
        assert_eq!(
            cell_at(&t, 0).fg,
            [200, 0, 0],
            "{extras:?}: the rewritten cell painted the overwritten text's colour"
        );

        let mut t = Terminal::new(ROWS, COLS);
        t.process(b"\x1b[H\x1b[48;2;1;2;3m\x1b[K");
        t.process(format!("\x1b[48;2;50;60;70m{extras}link").as_bytes());
        assert_eq!(
            cell_at(&t, 0).bg,
            [50, 60, 70],
            "{extras:?}: the cell painted the erased line's background"
        );
    }
}

#[test]
fn a_vs16_spacer_paints_its_leads_truecolour() {
    // VS16 widens the previous cell; its new right half takes the lead's
    // rendition, not the colour of the text it replaced.
    let mut t = Terminal::new(ROWS, COLS);
    t.process(b"\x1b[H\x1b[48;2;1;2;3mxxxxxxxx\r");
    t.process("\x1b[48;2;200;0;0m\u{2764}\u{FE0F}".as_bytes());
    let row = t.render_row(0);
    assert_eq!(
        (row[0].bg, row[1].bg),
        ([200, 0, 0], [200, 0, 0]),
        "the widened emoji's right half kept the replaced cell's background"
    );
}

#[test]
fn a_serialized_screen_replays_every_rendition() {
    // `serialize_ansi` is replayable ANSI: a fresh terminal fed it paints each
    // cell as the source does, for every rendition the SGR decoder stores.
    let mut a = Terminal::new(ROWS, COLS);
    a.process(
        b"\x1b[H\x1b[4:3mC\x1b[0m\x1b[21mD\x1b[0m\x1b[4:4mo\x1b[0m\x1b[4:5ma\x1b[0m\
          \x1b[4m\x1b[58:2::255:0:0mU\x1b[0m\x1b[4;58:5:9mI\x1b[0m\x1b[4:3;58:2::0:9:9mW\
          \x1b[0m\x1b[73mS\x1b[0m\x1b[74mb\x1b[0m\x1b[53mO\x1b[0m",
    );
    let mut b = Terminal::new(ROWS, COLS);
    b.process(a.serialize_ansi(usize::from(ROWS), Some(0)).as_bytes());
    assert_eq!(a.render_row(0), b.render_row(0));
    let flags = |t: &Terminal| -> Vec<_> {
        (0..COLS)
            .map(|c| t.grid().cell(0, c).map(|cell| cell.flags()))
            .collect()
    };
    assert_eq!(flags(&a), flags(&b), "a stored rendition did not replay");
}

#[test]
fn a_wide_half_an_edit_orphans_paints_the_erase_background() {
    // ICH, DCH, DECDC and ECH blank the half of a wide pair they bisect or push
    // off the edge with the erase background, as EL does: its colour, not no
    // colour and not the bytes of the glyph that was there. (input, the orphan
    // columns of row 0)
    let bce = "\x1b[48;2;200;0;0m";
    let cases: [(String, &[usize]); 6] = [
        (format!("ab\u{4E2D}cd{bce}\x1b[1;1H\x1b[3P"), &[0]),
        (
            format!("\x1b[48;2;1;2;3mxxxx\r\x1b[0m\u{4E2D}{bce}\x1b[1;2H\x1b[P"),
            &[0],
        ),
        (format!("\u{4E2D}ab{bce}\x1b[1;2H\x1b[@"), &[0, 2]),
        (format!("abcdefgh\u{4E2D}{bce}\x1b[1;1H\x1b[@"), &[9]),
        (format!("ab\u{4E2D}cd{bce}\x1b[1;3H\x1b['~"), &[2]),
        (format!("\u{4E2D}ab{bce}\x1b[1;2H\x1b[X"), &[0]),
    ];
    for (input, orphans) in cases {
        let mut t = Terminal::new(3, 10);
        t.process(input.as_bytes());
        let row = t.render_row(0);
        for &c in orphans {
            assert_eq!(row[c].bg, [200, 0, 0], "{input:?}: column {c}");
        }
    }
}
