// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Differential oracle for the SGR rendition path.

use aterm_core::terminal::{RenderCell, Terminal};

const ROWS: u16 = 6;
const COLS: u16 = 20;

fn cell_at(t: &Terminal, row: usize) -> RenderCell {
    t.render_row(row)
        .first()
        .copied()
        .unwrap_or_else(|| t.implicit_blank_render_cell())
}

fn cell_after(seq: &str) -> RenderCell {
    let mut t = Terminal::new(ROWS, COLS);
    t.process(b"\x1b[H");
    t.process(seq.as_bytes());
    t.process(b"X");
    cell_at(&t, 0)
}

fn erased_cell_after(seq: &str) -> RenderCell {
    let mut t = Terminal::new(ROWS, COLS);
    t.process(b"\x1b[H");
    t.process(seq.as_bytes());
    t.process(b"\x1b[2J");
    cell_at(&t, 1)
}

fn scrolled_in_cell_after(seq: &str) -> RenderCell {
    let mut t = Terminal::new(ROWS, COLS);
    t.process(b"\x1b[H");
    t.process(seq.as_bytes());
    t.process(format!("\x1b[{ROWS};1H").as_bytes());
    t.process(b"\n");
    cell_at(&t, usize::from(ROWS - 1))
}

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
    ("ansi fg+bg", "\x1b[31m\x1b[42m", "\x1b[31;42m"),
    ("bright fg", "\x1b[91m", "\x1b[38;5;9m"),
    ("bright bg", "\x1b[101m", "\x1b[48;5;9m"),
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
    let mut t = Terminal::new(ROWS, COLS);
    let mut sampled = Vec::new();
    for n in 0u32..5_000 {
        let r = u8::try_from(n >> 10).expect("n < 5_120 so n >> 10 < 5");
        let g = u8::try_from((n >> 2) & 0xFF).expect("masked to a byte");
        let b = u8::try_from(n & 0xFF).expect("masked to a byte");
        t.process(b"\x1b[H");
        t.process(format!("\x1b[38;2;{r};{g};{b}mZ").as_bytes());
        if n % 977 == 0 {
            sampled.push((n, [r, g, b]));
        }
    }
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
