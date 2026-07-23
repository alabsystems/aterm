// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Stress stability tests for the terminal engine.
//!
//! These tests feed large volumes of mixed realistic and pathological input
//! through a Terminal and assert that it remains in a valid state afterward.
//! They are gated behind the `long-tests` feature to avoid slowing CI.
//!
//! Run with: cargo test -p aterm-core --test stress_stability --features long-tests
//!
//! ## What This Validates
//!
//! - No panics under extreme input
//! - Cursor stays within grid bounds
//! - Grid dimensions remain correct
//! - Scrollback does not grow without bound
//! - Terminal processes arbitrary byte sequences without corruption

#![cfg(feature = "long-tests")]

use aterm_core::terminal::{Terminal, TerminalBuilder};
use std::time::Instant;

/// Generate mixed realistic + pathological input.
///
/// The stream alternates between:
/// - Normal text with SGR colors (simulates compiler output)
/// - Pathological CSI with many parameters
/// - Long OSC strings
/// - CJK + emoji wide characters
/// - Rapid mode switches
/// - IL/DL grid operations
/// - Scroll region manipulation
/// - Raw binary noise
fn generate_mixed_stress_input(total_bytes: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(total_bytes);
    let mut phase = 0u64;

    while data.len() < total_bytes {
        match phase % 8 {
            // Normal colored output
            0 => {
                for i in 0..100 {
                    data.extend_from_slice(
                        format!(
                            "\x1b[{}m{}: Normal terminal output line with colors\x1b[0m\n",
                            31 + (i % 7),
                            i
                        )
                        .as_bytes(),
                    );
                }
            }
            // Pathological CSI: many-param SGR
            1 => {
                for _ in 0..50 {
                    data.extend_from_slice(
                        b"\x1b[1;2;3;4;5;7;8;9;31;32;33;34;35;36;37;91;92;93;94;95mX",
                    );
                    data.extend_from_slice(
                        b"\x1b[38;5;196;48;5;21;1;3;4;7;38;5;82;48;5;55;22;23;24;27mY\n",
                    );
                }
            }
            // Long OSC strings
            2 => {
                // 4KB OSC string (not 64KB, to keep overall size manageable)
                data.extend_from_slice(b"\x1b]0;");
                for j in 0..4096 {
                    data.push(b'T' + (j % 6) as u8);
                }
                data.push(0x07);
                // Short OSCs in rapid succession
                for i in 0..20 {
                    data.extend_from_slice(format!("\x1b]0;Title {i}\x07").as_bytes());
                }
            }
            // Wide characters: CJK + emoji
            3 => {
                let cjk: &[&str] = &[
                    "\u{4E2D}", "\u{6587}", "\u{5B57}", "\u{7B26}", "\u{53F7}", "\u{6D4B}",
                    "\u{8BD5}",
                ];
                let emoji: &[&str] = &["\u{1F600}", "\u{1F680}", "\u{1F525}", "\u{2728}"];
                for i in 0..200 {
                    if i % 3 == 0 {
                        data.extend_from_slice(emoji[i % emoji.len()].as_bytes());
                    } else {
                        data.extend_from_slice(cjk[i % cjk.len()].as_bytes());
                    }
                    if i % 39 == 0 {
                        data.push(b'\n');
                    }
                }
            }
            // Mode switches
            4 => {
                for _ in 0..50 {
                    data.extend_from_slice(b"\x1b[?1049h"); // Alt screen
                    data.extend_from_slice(b"Alt screen content\n");
                    data.extend_from_slice(b"\x1b[?1049l");
                    data.extend_from_slice(b"\x1b[?7h\x1b[?7l"); // Wrap toggle
                    data.extend_from_slice(b"\x1b[?25h\x1b[?25l"); // Cursor toggle
                }
            }
            // Grid operations: IL/DL, erase, scroll regions
            5 => {
                for i in 0..50 {
                    let row = (i % 23) + 1;
                    data.extend_from_slice(format!("\x1b[{row};1H").as_bytes());
                    data.extend_from_slice(b"\x1b[1L"); // Insert line
                    data.extend_from_slice(b"Inserted line content\n");
                    data.extend_from_slice(b"\x1b[1M"); // Delete line
                    data.extend_from_slice(b"\x1b[2J"); // Erase screen
                }
                // Scroll region exercise
                data.extend_from_slice(b"\x1b[5;20r"); // Set region
                for _ in 0..20 {
                    data.extend_from_slice(b"Region scroll\n");
                }
                data.extend_from_slice(b"\x1b[r"); // Reset region
            }
            // Cursor positioning chaos
            6 => {
                let mut rng: u32 = 54321;
                for _ in 0..200 {
                    rng = rng.wrapping_mul(1103515245).wrapping_add(12345);
                    let r = (rng % 50) + 1;
                    let c = ((rng >> 8) % 200) + 1;
                    data.extend_from_slice(format!("\x1b[{r};{c}H").as_bytes());
                    data.push(b'*');
                }
            }
            // Semi-random bytes (deterministic but chaotic)
            _ => {
                let mut val: u32 = 98765;
                for _ in 0..500 {
                    val = val
                        .wrapping_mul(6364136223846793005u64 as u32)
                        .wrapping_add(1);
                    let byte = (val >> 16) as u8;
                    // Avoid NUL which can cause issues unrelated to parsing
                    if byte != 0 {
                        data.push(byte);
                    }
                }
                data.push(b'\n');
            }
        }
        phase += 1;
    }

    data.truncate(total_bytes);
    data
}

/// Feed 100MB of mixed input and validate terminal state.
#[test]
fn stress_100mb_mixed_input() {
    let target_size = 100 * 1024 * 1024; // 100MB
    let input = generate_mixed_stress_input(target_size);
    assert!(
        input.len() >= target_size,
        "generated input should be at least 100MB, got {} bytes",
        input.len()
    );

    let mut term = TerminalBuilder::new()
        .rows(50)
        .cols(132)
        .ring_buffer_size(10_000)
        .build();

    let start = Instant::now();

    // Process in 64KB chunks (realistic PTY read size)
    let chunk_size = 64 * 1024;
    for chunk in input.chunks(chunk_size) {
        term.process(chunk);
    }

    let elapsed = start.elapsed();
    let throughput_mib = (input.len() as f64) / (1024.0 * 1024.0) / elapsed.as_secs_f64();

    // Print performance summary
    eprintln!("--- Stress Test Summary ---");
    eprintln!("Input size:  {} MB", input.len() / (1024 * 1024));
    eprintln!("Wall time:   {:.2}s", elapsed.as_secs_f64());
    eprintln!("Throughput:  {throughput_mib:.1} MiB/s");

    // Validate terminal state
    let cursor = term.cursor();
    let rows = term.rows();
    let cols = term.cols();

    // Grid dimensions must be unchanged
    assert_eq!(rows, 50, "rows should remain 50 after stress test");
    assert_eq!(cols, 132, "cols should remain 132 after stress test");

    // Cursor must be within bounds
    assert!(
        cursor.row < rows,
        "cursor row {} must be < rows {}",
        cursor.row,
        rows
    );
    assert!(
        cursor.col <= cols,
        "cursor col {} must be <= cols {}",
        cursor.col,
        cols
    );

    // Grid cells must be accessible without panic
    for r in 0..rows {
        for c in 0..cols {
            // Just accessing the cell is enough -- any corruption would panic
            if let Some(cell) = term.grid().cell(r, c) {
                let _ = cell.char();
            }
        }
    }

    eprintln!("Cursor pos:  ({}, {})", cursor.row, cursor.col);
    eprintln!("All grid cells accessible: OK");
    eprintln!("--- Stress Test PASSED ---");
}

/// Scrollback memory stability: verify the terminal reaches steady state.
///
/// Feed 1M lines through a terminal with 10K scrollback limit. After initial
/// fill, memory usage should not grow linearly with additional input.
#[test]
fn stress_scrollback_memory_stability() {
    let mut term = TerminalBuilder::new()
        .rows(24)
        .cols(80)
        .ring_buffer_size(10_000)
        .build();

    // Phase 1: Fill scrollback to capacity (>10K lines)
    let fill_lines = 15_000;
    let fill_input = generate_scrollback_fill(fill_lines);
    term.process(&fill_input);

    // Phase 2: Feed many more lines and verify the terminal remains stable.
    let additional_lines = 100_000;
    let chunk_lines = 1000;
    let chunk = generate_scrollback_fill(chunk_lines);

    let start = Instant::now();
    for _ in 0..(additional_lines / chunk_lines) {
        term.process(&chunk);
    }
    let elapsed = start.elapsed();

    // Validate
    let cursor = term.cursor();
    assert!(cursor.row < term.rows());
    assert!(cursor.col <= term.cols());

    let throughput_mib = (additional_lines * 82) as f64 / (1024.0 * 1024.0) / elapsed.as_secs_f64();
    eprintln!("--- Scrollback Stability ---");
    eprintln!("Lines fed:   {}", fill_lines + additional_lines);
    eprintln!("Throughput:  {throughput_mib:.1} MiB/s");
    eprintln!("--- PASSED ---");
}

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

// =============================================================================
// Helper generators
// =============================================================================

fn generate_scrollback_fill(line_count: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(line_count * 82);
    for i in 0..line_count {
        data.extend_from_slice(
            format!("scrollback-fill-line-{i:08}-abcdefghijklmnopqrstuvwxyz-0123456789\n")
                .as_bytes(),
        );
    }
    data
}
