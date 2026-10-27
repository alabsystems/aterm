// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Width resizes in the differential VT oracle: where the cursor's next write goes after one,
//! and the guard that keeps out the resizes aterm's main-screen rewrap decides.

use super::caps::Caps;
use super::engines::{Astream, Aterm, Snap};
use super::generator::{self, Reach};

/// One step of a resize case.
#[derive(Clone, Copy, Debug)]
enum Step {
    Out(&'static [u8]),
    /// A resize to this many columns, the rows kept.
    Width(u16),
}

use Step::{Out, Width};

struct Case {
    name: &'static str,
    cols: u16,
    rows: u16,
    steps: &'static [Step],
    /// aterm's rows at the end, trailing blanks trimmed.
    rows_after: &'static [&'static str],
}

const CASES: &[Case] = &[
    Case {
        name: "a wrap pending at the edge takes the next column on a widening",
        cols: 5,
        rows: 2,
        steps: &[Out(b"abcde"), Width(10), Out(b"X")],
        rows_after: &["abcdeX", ""],
    },
    Case {
        name: "and on the alternate screen",
        cols: 5,
        rows: 2,
        steps: &[Out(b"\x1b[?1049habcde"), Width(10), Out(b"X")],
        rows_after: &["abcdeX", ""],
    },
    Case {
        name: "a wrap pending at the edge stays pending on a narrowing",
        cols: 10,
        rows: 2,
        steps: &[Out(b"\x1b[?1049habcdefghij"), Width(5), Out(b"X")],
        rows_after: &["abcde", "X"],
    },
    Case {
        name: "a cursor whose cell the narrowing cuts off is clamped onto the last column",
        cols: 10,
        rows: 2,
        steps: &[Out(b"\x1b[?1049habc\x1b[8G"), Width(5), Out(b"X")],
        rows_after: &["abc X", ""],
    },
    Case {
        name: "and with a line below, which the glyph leaves alone",
        cols: 10,
        rows: 2,
        steps: &[Out(b"\x1b[?1049habc\r\ndef\x1b[1;8H"), Width(5), Out(b"X")],
        rows_after: &["abc X", "def"],
    },
    Case {
        name: "and on the bottom row, which the glyph does not scroll",
        cols: 10,
        rows: 2,
        steps: &[Out(b"\x1b[?1049hr0\r\nabc\x1b[2;8H"), Width(5), Out(b"X")],
        rows_after: &["r0", "abc X"],
    },
    Case {
        name: "a cursor in a gap on the main screen keeps its column where the row has room",
        cols: 10,
        rows: 2,
        steps: &[Out(b"abc\x1b[8G"), Width(8), Out(b"X")],
        rows_after: &["abc    X", ""],
    },
    Case {
        name: "a cursor in a gap past the text keeps its column on a widening",
        cols: 10,
        rows: 2,
        steps: &[Out(b"abc\x1b[8G"), Width(20), Out(b"X")],
        rows_after: &["abc    X", ""],
    },
    Case {
        name: "a cursor past wrapped text on the alternate screen stays put",
        cols: 6,
        rows: 3,
        steps: &[Out(b"\x1b[?1049habcdefghij"), Width(10), Out(b"X")],
        rows_after: &["abcdef", "ghijX", ""],
    },
    Case {
        name: "a wrap pending at a right margin stays pending across a widening",
        cols: 10,
        rows: 2,
        steps: &[Out(b"\x1b[?69h\x1b[1;3sabc"), Width(12), Out(b"X")],
        rows_after: &["abc", "X"],
    },
    Case {
        name: "a same-size resize keeps the wrap",
        cols: 5,
        rows: 2,
        steps: &[Out(b"abcde"), Width(5), Out(b"X")],
        rows_after: &["abcde", "X"],
    },
];

/// Run `c` through both engines, comparing after each step; a resize the guard would skip is
/// a case written wrong.
fn run(c: &Case) -> Result<Snap, String> {
    let mut a = Astream::new(c.cols, c.rows);
    let mut t = Aterm::new(c.cols, c.rows);
    for (i, step) in c.steps.iter().enumerate() {
        match *step {
            Out(bytes) => {
                a.feed(bytes);
                t.feed(bytes);
            }
            Width(cols) => {
                if t.reflow_moves_cells(cols, c.rows) {
                    return Err(format!("step {i}: the guard keeps this resize out"));
                }
                a.resize(cols, c.rows);
                t.resize(cols, c.rows);
            }
        }
        let (sa, st) = (a.snap(), t.snap());
        if let Some(what) = sa.first_difference(&st) {
            return Err(format!(
                "step {i} ({step:?}): {what}\n  astream-term (cursor {:?}):\n{}  aterm (cursor {:?}):\n{}",
                sa.cursor,
                sa.text_block(),
                st.cursor,
                st.text_block()
            ));
        }
    }
    Ok(t.snap())
}

#[test]
fn the_resize_cases_agree() {
    let mut failures = Vec::new();
    for c in CASES {
        match run(c) {
            Ok(end) => {
                let rows: Vec<String> = (0..end.rows).map(|r| end.row_text(r)).collect();
                if rows != c.rows_after {
                    failures.push(format!("{}: aterm ends {rows:?}", c.name));
                }
            }
            Err(why) => failures.push(format!("{}: {why}", c.name)),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// The guard is not vacuous: it keeps out the resizes whose main-screen rewrap moves cells (and
/// there the engines do disagree), and only those.
#[test]
fn the_reflow_guard_keeps_out_only_what_the_fold_cannot_follow() {
    let engines = |cols: u16, rows: u16, bytes: &[u8]| {
        let mut a = Astream::new(cols, rows);
        let mut t = Aterm::new(cols, rows);
        a.feed(bytes);
        t.feed(bytes);
        (a, t)
    };
    for (what, cols, bytes, to, then) in [
        (
            "a wrapped line widened",
            6,
            b"abcdefghij".as_slice(),
            10,
            b"".as_slice(),
        ),
        ("a long line narrowed", 10, b"abcdefgh", 4, b""),
        (
            "a wrapped line under the alternate screen, shown again",
            6,
            b"abcdefghij\x1b[?47h",
            10,
            b"\x1b[?47l",
        ),
        (
            "a cursor past the narrower row, whose cell aterm carries onto a row of its own",
            10,
            b"abc\x1b[8G",
            5,
            b"X",
        ),
    ] {
        let (mut a, mut t) = engines(cols, 3, bytes);
        assert!(
            t.reflow_moves_cells(to, 3),
            "{what}: the guard keeps it out"
        );
        a.resize(to, 3);
        t.resize(to, 3);
        a.feed(then);
        t.feed(then);
        assert!(
            a.snap().first_difference(&t.snap()).is_some(),
            "{what}: the engines disagree there"
        );
    }
    for (what, cols, bytes, to) in [
        ("an edge wrap widened", 5, b"abcde".as_slice(), 10),
        ("short rows narrowed", 10, b"abc\r\nde", 4),
        (
            "wrapped text on the alternate screen",
            6,
            b"\x1b[?1049habcdefghij",
            10,
        ),
    ] {
        let (_, t) = engines(cols, 3, bytes);
        assert!(!t.reflow_moves_cells(to, 3), "{what}: the guard lets it in");
    }
}

/// The widening is not vacuous: over a fixed run of seeds, the generated programs change the
/// width in both directions from a wrap pending on the last column, from a cursor after its row's
/// text, from a cursor whose own cell the narrowing cuts off, and from a row that continues the
/// one above, and the guard keeps some out. Only a resize that changes the width counts toward
/// the rule: one that keeps it leaves the cursor and its wrap alone. A width change on a continued
/// row is always the alternate screen's (aterm rewraps the main screen's, which the guard keeps
/// out).
#[test]
fn the_generated_resizes_reach_the_rule() {
    let caps = Caps::probe();
    let mut reach = Reach::default();
    for seed in 0..5_000 {
        let program = generator::program(seed, caps);
        assert!(
            generator::run_reaching(&program, &mut reach).is_none(),
            "seed {seed:#x} diverges (random_subset_programs_agree reports it)"
        );
    }
    // About a third of what these seeds reached when the floors were set (2,815 applied, 1,157 of
    // them keeping the width; width changes: 1,176 wider, 482 narrower, 86 from an edge wrap, 259
    // after the text, 22 from a cut-off cell, 95 on a continued row; 3,379 kept out).
    let floors = [
        ("applied", reach.applied, 1_000),
        ("widened", reach.widened, 400),
        ("narrowed", reach.narrowed, 150),
        ("width changes from an edge wrap", reach.edge_wrap, 28),
        ("width changes from after the text", reach.past_text, 85),
        (
            "narrowings that cut off the cursor's cell",
            reach.cut_off,
            7,
        ),
        (
            "width changes on a continued row",
            reach.on_continuation,
            30,
        ),
        ("kept out", reach.kept_out, 1),
    ];
    for (what, got, floor) in floors {
        assert!(got >= floor, "{what}: {got} < {floor} ({reach:?})");
    }
}
