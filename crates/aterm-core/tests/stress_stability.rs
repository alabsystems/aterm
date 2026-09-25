// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Stress stability tests for the terminal engine.
//!
//! These tests feed pathological parser input, extreme grid operations and
//! wide characters through a Terminal and assert that it remains in a valid
//! state afterward.
//! They are gated behind the `long-tests` feature to avoid slowing CI.
//!
//! Run with: cargo test -p aterm-core --test stress_stability --features long-tests
//!
//! ## What This Validates
//!
//! - No panics under extreme input
//! - Cursor stays within grid bounds
//! - Grid dimensions remain correct
//! - Terminal processes arbitrary byte sequences without corruption

#![cfg(feature = "long-tests")]

use aterm_core::terminal::Terminal;

/// Pathological input that maximizes parser branch coverage.
#[test]
fn stress_pathological_parser() {
    let mut term = Terminal::new(24, 80);

    // Incomplete escape sequences (parser must handle partial input gracefully)
    let incomplete_sequences: &[&[u8]] = &[
        b"\x1b",    // Bare ESC
        b"\x1b[",   // Incomplete CSI
        b"\x1b[1",  // CSI with partial param
        b"\x1b[1;", // CSI with trailing semicolon
        b"\x1b]",   // Incomplete OSC
        b"\x1b]0",  // OSC with partial command
        b"\x1b]0;", // OSC with partial payload
        b"\x1bP",   // Incomplete DCS
        b"\x1b(",   // Incomplete charset designate
        b"\x1b[?",  // Incomplete private mode
        b"\x1b[>",  // Incomplete secondary DA query
    ];

    for seq in incomplete_sequences {
        // Process incomplete sequence, then complete it with a valid follow-up
        term.process(seq);
        // Follow up with normal content to reset parser state
        term.process(b"Hello\n");
    }

    // Zero-param CSI sequences
    let zero_param_csi = b"\x1b[m\x1b[H\x1b[J\x1b[K\x1b[A\x1b[B\x1b[C\x1b[D";
    for _ in 0..10_000 {
        term.process(zero_param_csi);
    }

    // Maximum-value params (should be clamped, not overflow)
    term.process(b"\x1b[99999;99999H"); // Huge cursor position
    term.process(b"\x1b[99999A"); // Huge cursor up
    term.process(b"\x1b[99999B"); // Huge cursor down
    term.process(b"\x1b[99999L"); // Huge insert lines
    term.process(b"\x1b[99999M"); // Huge delete lines
    term.process(b"\x1b[99999S"); // Huge scroll up
    term.process(b"\x1b[99999T"); // Huge scroll down

    // Verify terminal is still sane
    let cursor = term.cursor();
    assert!(
        cursor.row < term.rows(),
        "cursor row out of bounds after pathological input"
    );
    assert!(
        cursor.col <= term.cols(),
        "cursor col out of bounds after pathological input"
    );
}

/// Grid operations under extreme conditions.
#[test]
fn stress_grid_operations() {
    let mut term = Terminal::new(100, 200);

    // Rapid IL/DL at every row position
    for row in 1..=100 {
        let seq = format!("\x1b[{row};1H\x1b[5L\x1b[{row};1H\x1b[5M");
        term.process(seq.as_bytes());
    }

    // Scroll regions of every possible height
    for top in 1..50 {
        let bottom = top + 10;
        if bottom <= 100 {
            let seq = format!("\x1b[{top};{bottom}r\x1b[{top};1HRegion content\n");
            term.process(seq.as_bytes());
        }
    }
    term.process(b"\x1b[r"); // Reset scroll region

    // Erase operations
    for _ in 0..1000 {
        term.process(b"\x1b[2J"); // Erase all
        term.process(b"Refill\n");
        term.process(b"\x1b[1J"); // Erase above
        term.process(b"\x1b[0J"); // Erase below
        term.process(b"\x1b[2K"); // Erase line
    }

    // Validate
    assert_eq!(term.rows(), 100);
    assert_eq!(term.cols(), 200);
    let cursor = term.cursor();
    assert!(cursor.row < 100);
    assert!(cursor.col <= 200);
}

/// Wide character handling under pressure.
#[test]
fn stress_wide_characters() {
    let mut term = Terminal::new(24, 80);

    // Fill entire screen with CJK
    let cjk_line = "\u{4E2D}\u{6587}\u{5B57}\u{7B26}\u{53F7}\u{6D4B}\u{8BD5}\u{7EC8}\u{7AEF}\u{6A21}\u{62DF}\u{5668}\u{538B}\u{529B}\u{6D4B}\u{9A8C}\u{6570}\u{636E}\u{751F}\u{6210}\u{4E2D}\u{6587}\u{5B57}\u{7B26}\u{53F7}\u{6D4B}\u{8BD5}\u{7EC8}\u{7AEF}";
    for _ in 0..1000 {
        term.process(format!("{cjk_line}\n").as_bytes());
    }

    // Emoji sequences
    let emoji_line = "\u{1F600}\u{1F601}\u{1F602}\u{1F603}\u{1F604}\u{1F605}\u{1F606}\u{1F607}";
    for _ in 0..1000 {
        term.process(format!("{emoji_line}\n").as_bytes());
    }

    // Wide chars at column boundaries (forces wrap handling)
    for _ in 0..1000 {
        // Write ASCII to column 79, then a wide char that must wrap
        term.process(b"\x1b[1;79H");
        term.process("\u{4E2D}".as_bytes());
        term.process(b"\n");
    }

    // Validate grid
    let cursor = term.cursor();
    assert!(cursor.row < term.rows());
    assert!(cursor.col <= term.cols());

    // Check that cells are accessible
    for r in 0..term.rows() {
        for c in 0..term.cols() {
            if let Some(cell) = term.grid().cell(r, c) {
                let _ = cell.char();
            }
        }
    }
}
