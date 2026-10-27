// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A resize carries reflowed cursor cells and real pending wraps through both resize paths and
//! storage shapes. The alternate screen clips ordinary cut-off cursor cells without inventing
//! a wrap that overwrites another row or scrolls from the bottom.

use aterm_core::scrollback::Scrollback;
use aterm_core::terminal::Terminal;

fn screen(term: &Terminal) -> Vec<String> {
    let grid = term.grid();
    (0..grid.rows())
        .map(|r| grid.row_text(r).expect("visible row"))
        .collect()
}

#[derive(Clone, Copy, Debug)]
enum Path {
    Sync,
    Offloaded,
}

fn resize(term: &mut Terminal, path: Path, rows: u16, cols: u16) {
    match path {
        Path::Sync => term.resize(rows, cols),
        Path::Offloaded => {
            let mut job = term.resize_offloading_scrollback(rows, cols);
            while let Some(pending) = job {
                job = term.finish_resize_offload(pending.reflow());
            }
        }
    }
}

const SHAPES: [(bool, Path); 3] = [
    (false, Path::Sync),
    (true, Path::Sync),
    (true, Path::Offloaded),
];

fn terminal(tiered: bool, rows: u16, cols: u16) -> Terminal {
    if tiered {
        Terminal::with_scrollback(rows, cols, 1000, Scrollback::with_defaults())
    } else {
        Terminal::new(rows, cols)
    }
}

/// Print `before` at `cols`, resize to `to` columns, print `after`, and read the screen.
fn run(tiered: bool, path: Path, cols: u16, before: &[u8], to: u16, after: &[u8]) -> Vec<String> {
    let mut term = terminal(tiered, 4, cols);
    term.process(before);
    resize(&mut term, path, 4, to);
    term.process(after);
    screen(&term)
}

/// A case: its name, the width, the bytes before, the width resized to, the bytes after, and the
/// screen then.
type Case = (
    &'static str,
    u16,
    &'static [u8],
    u16,
    &'static [u8],
    &'static [&'static str],
);

#[test]
fn the_glyph_after_a_resize_never_lands_on_one_the_program_printed() {
    let cases: &[Case] = &[
        (
            "a wrap pending at the edge, widened: the next column",
            5,
            b"abcde",
            10,
            b"X",
            &["abcdeX", "", "", ""],
        ),
        (
            "a cursor after a wrapped line's last glyph, widened onto a full row",
            6,
            b"abcdefghij",
            10,
            b"X",
            &["abcdefghij", "X", "", ""],
        ),
        (
            "a cursor after the last glyph, narrowed onto a full row",
            10,
            b"abcde",
            5,
            b"X",
            &["abcde", "X", "", ""],
        ),
        (
            "a prompt that fills the narrower row: the typed text goes on the next",
            20,
            b"user@host:~/src $ ",
            9,
            b"ls",
            &["user@host", ":~/src $ ", "ls", ""],
        ),
        (
            "the alternate screen (no reflow), widened: the next column",
            5,
            b"\x1b[?1049habcde",
            10,
            b"X",
            &["abcdeX", "", "", ""],
        ),
        (
            "a cursor in a gap past the narrower row, with a line below: a row of its own",
            10,
            b"abc\r\ndef\x1b[1;8H",
            5,
            b"X",
            &["abc", "  X", "def", ""],
        ),
        (
            "and on an empty row",
            10,
            b"\r\ndef\x1b[1;8H",
            5,
            b"X",
            &["", "  X", "def", ""],
        ),
        (
            "a cursor just after a line that fills the narrower row, with a line below",
            10,
            b"abcde\r\nfgh\x1b[1;6H",
            5,
            b"X",
            &["abcde", "X", "fgh", ""],
        ),
        (
            "a cursor just after a wrapped line that fills the wider row, with a line below",
            6,
            b"abcdefghij\r\nXYZ\x1b[2;5H",
            10,
            b"!",
            &["abcdefghij", "!", "XYZ", ""],
        ),
        (
            "the alternate screen, a cursor past the narrower row: clamped onto its last column, as xterm does",
            10,
            b"\x1b[?1049habc\x1b[8G",
            5,
            b"X",
            &["abc X", "", "", ""],
        ),
        (
            "and with a line below, which the glyph leaves alone",
            10,
            b"\x1b[?1049habc\r\ndef\x1b[1;8H",
            5,
            b"X",
            &["abc X", "def", "", ""],
        ),
        (
            "and on the bottom row, which the glyph does not scroll",
            10,
            b"\x1b[?1049hr0\r\nr1\r\nr2\r\nabc\x1b[4;8H",
            5,
            b"X",
            &["r0", "r1", "r2", "abc X"],
        ),
        (
            "the alternate screen, a cursor past wrapped text stays where it was",
            6,
            b"\x1b[?1049habcdefghij",
            10,
            b"X",
            &["abcdef", "ghijX", "", ""],
        ),
    ];
    let mut wrong = Vec::new();
    for &(name, cols, before, to, after, want) in cases {
        for (tiered, path) in SHAPES {
            let got = run(tiered, path, cols, before, to, after);
            if got != want {
                wrong.push(format!("{name} (tiered {tiered}, {path:?}): {got:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn a_prompt_typed_after_a_narrowing_reads_whole_when_widened_back() {
    for (tiered, path) in SHAPES {
        let mut term = terminal(tiered, 4, 20);
        term.process(b"user@host:~/src $ ");
        resize(&mut term, path, 4, 9);
        term.process(b"ls");
        resize(&mut term, path, 4, 20);
        assert_eq!(
            screen(&term),
            ["user@host:~/src $ ls", "", "", ""],
            "tiered {tiered}, {path:?}"
        );
        let cursor = term.cursor();
        assert_eq!(
            (cursor.row, cursor.col, term.grid().pending_wrap()),
            (1, 0, false),
            "tiered {tiered}, {path:?}: the cursor after the s, which fills the row, starts the next one"
        );
        term.process(b" -la");
        assert_eq!(
            lines(&term),
            ["user@host:~/src $ ls", "~ -la", "", ""],
            "tiered {tiered}, {path:?}: and what is typed next continues the line"
        );
    }
}

/// Each screen row's text, `~` in front of a row that continues the one above.
fn lines(term: &Terminal) -> Vec<String> {
    let grid = term.grid();
    (0..grid.rows())
        .map(|r| {
            let mark = if grid.row(r).is_some_and(|row| row.is_wrapped()) {
                "~"
            } else {
                ""
            };
            format!("{mark}{}", grid.row_text(r).expect("visible row"))
        })
        .collect()
}

/// A narrowing case: its name, the width, the bytes before, the narrower width, the bytes after.
type Narrowing = (&'static str, u16, &'static [u8], u16, &'static [u8]);

#[test]
fn the_glyph_after_a_narrowing_never_joins_the_line_below_to_the_cursors() {
    // Once widened back, the screen is what the same bytes show with no resize at all.
    let cases: &[Narrowing] = &[
        (
            "a cursor in a gap past the narrower row",
            10,
            b"abc\r\ndef\x1b[1;8H",
            5,
            b"X",
        ),
        (
            "a cursor in a gap on an empty row",
            10,
            b"\r\ndef\x1b[1;8H",
            5,
            b"X",
        ),
        (
            "a cursor just after a line that fills the narrower row",
            10,
            b"abcde\r\nfgh\x1b[1;6H",
            5,
            b"X",
        ),
        (
            "a completion list below a prompt that fills the narrower row",
            12,
            b"$ ls foo\r\nfoo1  foo2\r\nfoo3\x1b[2A\x1b[1;9H",
            8,
            b"b",
        ),
    ];
    let mut wrong = Vec::new();
    for &(name, cols, before, to, after) in cases {
        for (tiered, path) in SHAPES {
            let mut term = terminal(tiered, 6, cols);
            term.process(before);
            resize(&mut term, path, 6, to);
            term.process(after);
            resize(&mut term, path, 6, cols);
            let mut unresized = terminal(tiered, 6, cols);
            unresized.process(before);
            unresized.process(after);
            let (got, want) = (lines(&term), lines(&unresized));
            if got != want {
                wrong.push(format!(
                    "{name} (tiered {tiered}, {path:?}): {got:?}, want {want:?}"
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn the_glyph_after_a_restore_never_lands_on_one_its_line_printed() {
    // A DECSC slot (and the one `?1049h` saves) moves with the text it was saved on. Each case:
    // its name, the width, the bytes before, the width resized to, the bytes after, and the
    // screen then.
    let cases: &[Case] = &[
        (
            "a slot just after a line that fills the narrower row, the cursor on it",
            10,
            b"abcde\x1b7",
            5,
            b"\x1b8X",
            &["abcde", "~X", "", ""],
        ),
        (
            "and with the cursor gone on to a line below: the slot opens a row of its own",
            10,
            b"abcde\x1b7\r\nfgh",
            5,
            b"\x1b8X",
            &["abcde", "~X", "fgh", ""],
        ),
        (
            "a slot just after a wrapped line widened onto a full row",
            6,
            b"abcdefghij\x1b7\r\nXYZ",
            10,
            b"\x1b8!",
            &["abcdefghij", "~!", "XYZ", ""],
        ),
        (
            "the slot ?1049h saves, after a line that fills the narrower row",
            10,
            b"abcde\x1b[?1049hfull screen",
            5,
            b"\x1b[?1049lX",
            &["abcde", "~X", "", ""],
        ),
        (
            "a slot in a gap past a row with room: clamped onto its blank last column",
            10,
            b"abc\x1b[8G\x1b7\r\ndef",
            5,
            b"\x1b8X",
            &["abc X", "def", "", ""],
        ),
    ];
    let mut wrong = Vec::new();
    for &(name, cols, before, to, after, want) in cases {
        for (tiered, path) in SHAPES {
            let mut term = terminal(tiered, 4, cols);
            term.process(before);
            resize(&mut term, path, 4, to);
            term.process(after);
            let got = lines(&term);
            if got != want {
                wrong.push(format!("{name} (tiered {tiered}, {path:?}): {got:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
