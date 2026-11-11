// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::Terminal;

/// The rows after `next` lands on `t`, and on a fresh terminal fed `t`'s serialization.
fn after_replay(rows: u16, cols: u16, input: &str, next: &str) -> (Vec<String>, Vec<String>) {
    let mut a = Terminal::new(rows, cols);
    a.process(input.as_bytes());
    let mut b = Terminal::new(rows, cols);
    b.process(a.serialize_ansi(usize::from(rows), Some(0)).as_bytes());
    let text = |t: &mut Terminal| {
        t.process(next.as_bytes());
        (0..usize::from(rows))
            .map(|r| t.row_text(r).unwrap_or_default())
            .collect()
    };
    (text(&mut a), text(&mut b))
}

#[test]
fn a_replay_wraps_where_the_source_would() {
    for (rows, input) in [
        (3, "abcde"),
        (2, "12345\r\nabcde"),
        (3, "abc\u{4E2D}"),
        (3, "\x1b[31mabcd\x1b[44me"),
    ] {
        let (source, replay) = after_replay(rows, 5, input, "Z");
        assert_eq!(source, replay, "{input:?}");
    }
    for input in ["abcd", "\x1b[?7labcde"] {
        let (source, replay) = after_replay(3, 5, input, "Z");
        assert_eq!(source, replay, "{input:?}");
    }
}

/// What later output shows on `t` after `next`: each row with its colours, the cursor, which screen is up, and whether the cursor shows.
fn shows(t: &mut Terminal, next: &str) -> (Vec<String>, (u16, u16), bool, bool) {
    t.process(next.as_bytes());
    let rows = (0..t.rows())
        .map(|r| t.grid().row_ansi_text_screen(r).unwrap_or_default())
        .collect();
    let c = t.cursor();
    (
        rows,
        (c.row, c.col),
        t.modes().alternate_screen,
        t.modes().cursor_visible,
    )
}

/// A terminal fed `input` and one fed its restore paint show the same after `next`; `paint` makes the restore paint.
fn restored_shows_the_same(
    rows: u16,
    cols: u16,
    input: &str,
    next: &str,
    paint: impl Fn(&Terminal) -> String,
) -> bool {
    let mut a = Terminal::new(rows, cols);
    a.process(input.as_bytes());
    let mut b = Terminal::new(rows, cols);
    b.process(b"\x1b[?1049h\x1b[31mleft over from before\x1b[3;4r\x1b(0");
    b.process(paint(&a).as_bytes());
    shows(&mut a, next) == shows(&mut b, next)
}

/// The states later output depends on, each followed by output that shows whether it was restored.
const STATES: &[(&str, &str, &str)] = &[
    (
        "alt screen with a region and a pen, then its exit",
        "$ vim\r\nshell row\r\n\x1b[31m\x1b[?1049h\x1b[2;4r\x1b[44m\x1b[1;1Hvim top\x1b[4;1Hvim low",
        "\nIN REGION\x1b[?1049lback\r\nplain",
    ),
    (
        "tmux-style status line under a region",
        "\x1b[?1049h\x1b[?25l\x1b[1;5r\x1b[6;1H\x1b[42mstatus\x1b[m\x1b[5;1Hpane",
        "\r\nscrolled\r\nagain\x1b[6;8Hx",
    ),
    ("line drawing in G0", "\x1b(0", "qqx"),
    ("line drawing shifted in from G1", "\x1b)0\x0e", "lqk"),
    (
        "origin mode in a region",
        "\x1b[2;5r\x1b[?6h\x1b[2;3H",
        "X\x1b[1;1HY\x1b[9;1HZ",
    ),
    ("insert mode", "abcdef\x1b[1;2H\x1b[4h", "ZZ"),
    (
        "a saved cursor and pen",
        "\x1b[3;4H\x1b[32m\x1b7\x1b[1;1H\x1b[m",
        "\x1b8saved",
    ),
    ("a hidden cursor", "\x1b[?25l", "x"),
    ("wrap off", "\x1b[?7l", "0123456789abc"),
    (
        "custom tab stops",
        "\x1b[3g\x1b[1;5H\x1bH\x1b[1;1H",
        "\tX\tY",
    ),
    (
        "the alt screen entered without a save",
        "main\x1b[?47halt",
        "\x1b[?47lZ",
    ),
    (
        "a pen with an underline colour",
        "\x1b[4:3;58:2::1:2:3;1m",
        "u",
    ),
    ("a deferred wrap", "\x1b[1;1H0123456789", "W"),
];

#[test]
fn a_restore_paint_carries_every_state_later_output_depends_on() {
    for (what, input, next) in STATES {
        assert!(
            restored_shows_the_same(6, 10, input, next, Terminal::serialize_restore_ansi),
            "{what}"
        );
    }
    let cells_only =
        |t: &Terminal| format!("\x1bc{}", t.serialize_ansi(usize::from(t.rows()), Some(0)));
    for (what, input, next) in &STATES[..2] {
        assert!(
            !restored_shows_the_same(6, 10, input, next, cells_only),
            "negative control: cells alone miss {what}"
        );
    }
}

/// A resize keeps the cursor's next write (aterm-grid `resize-keeps-the-next-write`), and the
/// states in this matrix leave a deferred wrap only on a full-width row's last column.
#[test]
fn a_restore_paint_carries_where_a_resize_left_the_next_write() {
    let mut wrong = Vec::new();
    for (what, cols, input, to) in [
        ("a cursor in a gap, narrowed past", 10, "abc\x1b[1;8H", 5),
        (
            "a cursor on an empty row, narrowed past",
            10,
            "\x1b[1;8H",
            5,
        ),
        (
            "and on the alternate screen",
            10,
            "\x1b[?1049habc\x1b[1;8H",
            5,
        ),
        ("a cursor after a line the narrowing fills", 10, "abcde", 5),
        ("an edge wrap widened", 5, "abcde", 10),
        ("an edge wrap narrowed", 10, "abcdefghij", 5),
        (
            "an edge wrap narrowed on the alternate screen",
            10,
            "\x1b[?1049habcdefghij",
            5,
        ),
        (
            "a wrapped line widened onto a full row",
            6,
            "abcdefghij",
            10,
        ),
    ] {
        let mut live = Terminal::new(4, cols);
        live.process(input.as_bytes());
        live.resize(4, to);
        let mut restored = Terminal::new(4, to);
        restored.process(live.serialize_restore_ansi().as_bytes());
        let (got, want) = (shows(&mut restored, "XY"), shows(&mut live, "XY"));
        if got != want {
            wrong.push(format!("{what}: restored {got:?}, live {want:?}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
