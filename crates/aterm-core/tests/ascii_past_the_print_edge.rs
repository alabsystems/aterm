// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Printable ASCII where a double-width row's print edge sits left of the DECSLRM left margin (the
//! left margin past half the screen, a DECDWL row the wrap lands on). The grid's ASCII run writers
//! wrapped there without finding room on any row, for ever; the text prints where letters of
//! another script, which reach the grid one glyph at a time, print.

use aterm_core::terminal::Terminal;
use std::sync::mpsc;
use std::time::Duration;

/// Margins in columns 3-5 of a five-column screen, a DECDWL bottom row (print edge at column 2),
/// and the cursor at the right margin of the row above it with its wrap pending.
const SETUP: &str = "\x1b[?69h\x1b[3;5s\x1b[4;1H\x1b#6\x1b[3;5Hx";

/// The rows, cursor and pending wrap `text` leaves after `pen` and [`SETUP`], with its Cyrillic
/// letters read as the ASCII they stand in for, on a thread a one-minute hang detector watches.
fn printed(pen: &'static str, text: &'static str) -> String {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut t = Terminal::new(4, 5);
        t.process(SETUP.as_bytes());
        t.process(pen.as_bytes());
        t.process(text.as_bytes());
        let c = t.cursor();
        let rows: Vec<String> = (0..4)
            .map(|row| {
                t.row_text(row)
                    .unwrap_or_default()
                    .replace('\u{430}', "a")
                    .replace('\u{431}', "b")
                    .replace('\u{432}', "c")
            })
            .collect();
        let _ = tx.send(format!(
            "{rows:?} cursor=({},{}) wrap={}",
            c.row,
            c.col,
            t.grid().pending_wrap()
        ));
    });
    rx.recv_timeout(Duration::from_secs(60))
        .unwrap_or_else(|_| panic!("printing {text:?} under {pen:?} did not finish"))
}

#[test]
fn ascii_past_a_double_width_print_edge_prints_as_single_glyphs_do() {
    for pen in ["", "\x1b[1m", "\x1b[38;2;1;2;3m", "\x1b]8;;http://x/\x1b\\"] {
        assert_eq!(
            printed(pen, "abc"),
            printed(pen, "\u{430}\u{431}\u{432}"),
            "a run under {pen:?}"
        );
        assert_eq!(
            printed(pen, "a\x1b[5b"),
            printed(pen, "\u{430}\x1b[5b"),
            "REP under {pen:?}"
        );
    }
}
