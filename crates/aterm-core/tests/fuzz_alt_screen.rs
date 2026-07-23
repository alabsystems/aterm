// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Alt-screen (mode 1049) round-trip CORRECTNESS fuzz.
//!
//! `ESC[?1049h` saves the cursor and switches to a cleared alternate buffer;
//! `ESC[?1049l` switches back and restores the cursor — the mechanism vim, tmux,
//! less, htop all use. A bug here (alt buffer aliasing the main, a botched
//! restore) silently CORRUPTS the user's screen the moment they quit the editor
//! — a classic, high-impact terminal defect. The engine panic-freedom fuzz only
//! hits the exact mode numbers (1049) by low-probability chance, so this path is
//! effectively unfuzzed, and the property that matters is CORRECTNESS, not just
//! no-panic: after a round-trip, the main screen + cursor must be byte-identical
//! to before. This paints a distinctive main screen, enters the alt buffer,
//! scribbles arbitrary content there, exits, and asserts the main screen and
//! cursor were restored exactly. Deterministic.

use aterm_core::terminal::Terminal;

#[inline]
fn next(s: &mut u64) -> u32 {
    *s = s
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (*s >> 33) as u32
}

fn read_screen(term: &Terminal) -> Vec<String> {
    let (rows, cols) = (term.rows(), term.cols());
    let mut out = Vec::with_capacity(rows as usize);
    for r in 0..rows {
        let mut line = String::with_capacity(cols as usize);
        for c in 0..cols {
            if let Some(cell) = term.grid().cell(r, c) {
                line.push(cell.char());
            }
        }
        out.push(line);
    }
    out
}

/// Scribble arbitrary content (this runs INSIDE the alt buffer — it must not be
/// able to reach back and mutate the saved main screen).
fn scribble(s: &mut u64, term: &mut Terminal) {
    const SAMPLES: &[&[u8]] = &[
        b"alt scratch content ",
        "\u{65e5}\u{672c}\u{8a9e}".as_bytes(),  // wide CJK
        "\u{1f680}\u{2764}\u{fe0f}".as_bytes(), // emoji
        "x\u{0301}".as_bytes(),                 // combining
        b"\r\n",
        b"\x1b[2J\x1b[H",   // clear alt
        b"\x1b[5;10H",      // cursor move within alt
        b"\x1b[31m\x1b[1m", // SGR
        b"\x1b[10L",        // insert lines
        b"\t",
    ];
    let n = 1 + next(s) % 12;
    for _ in 0..n {
        term.process(SAMPLES[(next(s) as usize) % SAMPLES.len()]);
    }
}

#[test]
fn alt_screen_1049_roundtrip_preserves_main_and_cursor() {
    let mut s: u64 = 0xA17E_5C2E_BEEF_0042;
    let mut term = Terminal::new(24, 80);
    for it in 0..2000u32 {
        // Ensure we are on the MAIN screen, then paint a distinctive picture.
        term.process(b"\x1b[?1049l\x1b[2J\x1b[H");
        let lines = 1 + next(&mut s) % 12;
        for line in 0..lines {
            term.process(format!("main{it}-{line}:").as_bytes());
            if next(&mut s).is_multiple_of(3) {
                term.process("\u{4e2d}\u{6587}".as_bytes()); // wide chars
            }
            term.process(b"more text\r\n");
        }
        // Move the cursor somewhere non-trivial, then snapshot main + cursor.
        term.process(b"\x1b[7;13H");
        let main_before = read_screen(&term);
        let cur_before = term.cursor();

        // Enter the alt buffer, scribble, leave.
        term.process(b"\x1b[?1049h");
        scribble(&mut s, &mut term);
        term.process(b"\x1b[?1049l");

        // The main screen and cursor must be byte-identical to the snapshot.
        assert_eq!(
            read_screen(&term),
            main_before,
            "iter {it}: ESC[?1049 round-trip corrupted the main screen"
        );
        let cur_after = term.cursor();
        assert_eq!(
            (cur_after.row, cur_after.col),
            (cur_before.row, cur_before.col),
            "iter {it}: ESC[?1049 round-trip did not restore the cursor"
        );
    }
}
