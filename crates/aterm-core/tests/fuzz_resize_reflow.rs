// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Resize / reflow robustness fuzz.
//!
//! Reflow — re-splitting wrapped lines when the terminal width changes — is one
//! of the most bug-prone paths in any terminal: a wide (2-cell) glyph straddling
//! the old wrap point, a combining mark on the last column, the cursor sitting in
//! the pending-wrap state, all become edge cases at the new width. The engine
//! panic-freedom fuzz never resizes; the render fuzz resizes but checks pixels,
//! not engine invariants. This drives varied content (long wrapping ASCII, wide
//! CJK, emoji clusters, combining marks, tabs, SGR) and then RESIZES through many
//! random dimensions — including degenerate 1xN / Nx1 — asserting after each
//! resize that the engine state stays sound: dimensions match, the cursor is in
//! bounds, and every cell is accessible (corruption would index-panic).
//! Deterministic LCG -> any failure reproduces exactly.

use aterm_core::terminal::Terminal;

#[inline]
fn next(s: &mut u64) -> u32 {
    *s = s
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (*s >> 33) as u32
}

fn emit_content(s: &mut u64, term: &mut Terminal) {
    const SAMPLES: &[&[u8]] = &[
        b"the quick brown fox jumps over the lazy dog 0123456789 ", // long -> wraps
        "\u{65e5}\u{672c}\u{8a9e}\u{306e}\u{30c6}\u{30ad}\u{30b9}\u{30c8}".as_bytes(), // wide CJK
        "\u{1f680}\u{1f600}\u{2764}\u{fe0f}\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}".as_bytes(), // emoji + ZWJ
        "e\u{0301}n\u{0303}o\u{0308}a\u{030a}".as_bytes(), // combining marks
        b"\tTABBED\t",
        b"\x1b[31mred\x1b[1mbold\x1b[0m ",
        b"\r\n",
        b"\x1b[H", // home
        b"X",      // single cell at the current position (exercises pending-wrap)
    ];
    let n = 1 + next(s) % 10;
    for _ in 0..n {
        term.process(SAMPLES[(next(s) as usize) % SAMPLES.len()]);
    }
}

fn check_invariants(term: &Terminal, it: u32, r: u16, c: u16) {
    assert_eq!(
        term.rows(),
        r,
        "iter {it}: rows {} != resize target {r}",
        term.rows()
    );
    assert_eq!(
        term.cols(),
        c,
        "iter {it}: cols {} != resize target {c}",
        term.cols()
    );
    let cur = term.cursor();
    assert!(
        cur.row < r,
        "iter {it}: cursor row {} out of bounds (rows {r})",
        cur.row
    );
    assert!(
        cur.col <= c,
        "iter {it}: cursor col {} out of bounds (cols {c})",
        cur.col
    );
}

#[test]
fn resize_reflow_keeps_engine_invariants() {
    let mut s: u64 = 0xD1CE_F00D_1234_5678;
    let mut term = Terminal::new(24, 80);
    for it in 0..30_000u32 {
        emit_content(&mut s, &mut term);
        // Resize ~1/2 the time through a wide range of dimensions, incl. very
        // narrow widths that force aggressive re-wrapping and degenerate sizes.
        if next(&mut s) & 1 == 0 {
            let r = 1 + (next(&mut s) % 50) as u16;
            let c = 1 + (next(&mut s) % 200) as u16;
            term.resize(r, c);
            check_invariants(&term, it, r, c);
        }
    }
    // Every cell of the final grid must be accessible without panic.
    let (rows, cols) = (term.rows(), term.cols());
    for row in 0..rows {
        for col in 0..cols {
            if let Some(cell) = term.grid().cell(row, col) {
                let _ = cell.char();
            }
        }
    }
}
