// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use std::hint::black_box;
use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;

fn generate_ascii(size: usize) -> Vec<u8> {
    let pattern = b"Hello, World! This is a test of the terminal parser. ABCDEFGHIJKLMNOPQRSTUVWXYZ 0123456789 ";
    pattern.iter().cycle().take(size).copied().collect()
}

fn generate_mixed_terminal(size: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(size);
    let text = b"Line of text here with some content";
    let colors = [
        b"\x1b[31m".as_slice(), // Red
        b"\x1b[32m",            // Green
        b"\x1b[33m",            // Yellow
        b"\x1b[0m",             // Reset
        b"\x1b[1m",             // Bold
        b"\x1b[4m",             // Underline
    ];

    let mut i = 0;
    while data.len() < size {
        if i % 5 == 0 && !data.is_empty() {
            data.extend_from_slice(colors[i % colors.len()]);
        }
        data.extend_from_slice(text);
        data.push(b'\n');
        i += 1;
    }
    data.truncate(size);
    data
}

fn generate_heavy_escapes(size: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(size);
    let sequences = [
        b"\x1b[38;5;196m".as_slice(), // 256-color foreground
        b"\x1b[48;5;21m",             // 256-color background
        b"\x1b[38;2;255;128;64m",     // RGB foreground
        b"\x1b[1;4;5m",               // Bold, underline, blink
        b"\x1b[0m",                   // Reset
        b"\x1b[H",                    // Home
        b"\x1b[2J",                   // Clear screen
        b"\x1b[10;20H",               // Move cursor
        b"\x1b[?25h",                 // Show cursor
        b"\x1b[?25l",                 // Hide cursor
    ];

    let mut i = 0;
    while data.len() < size {
        data.extend_from_slice(sequences[i % sequences.len()]);
        data.extend_from_slice(b"X");
        i += 1;
    }
    data.truncate(size);
    data
}

fn measure_terminal_processing_mb_s(name: &str, input: &[u8], target_duration: Duration) -> u64 {
    let rows = 24;
    let cols = 80;

    let mut iterations = 1usize;
    loop {
        let start = Instant::now();
        for _ in 0..iterations {
            let mut term = Terminal::new(rows, cols);
            term.process(black_box(input));
            black_box(term.cursor().row);
        }
        let elapsed = start.elapsed();
        if elapsed >= target_duration {
            let total_bytes = (iterations as u64) * (input.len() as u64);
            let mb_s = (total_bytes as f64) / elapsed.as_secs_f64() / 1_000_000.0;
            eprintln!(
                "perf_gate_quick: {name}: iterations={iterations} bytes={} elapsed_s={:.3} mb_s={:.1}",
                input.len(),
                elapsed.as_secs_f64(),
                mb_s
            );
            return mb_s.round().max(0.0) as u64;
        }
        iterations = iterations.saturating_mul(2);
    }
}

fn main() {
    let size = 64 * 1024;
    let target_duration = Duration::from_millis(200);

    let ascii = generate_ascii(size);
    let mixed = generate_mixed_terminal(size);
    let escapes = generate_heavy_escapes(size);

    let ascii_mb_s = measure_terminal_processing_mb_s("ascii", &ascii, target_duration);
    let mixed_mb_s = measure_terminal_processing_mb_s("mixed", &mixed, target_duration);
    let escapes_mb_s = measure_terminal_processing_mb_s("escapes", &escapes, target_duration);

    println!(
        "PERF_GATE_QUICK ascii_mb_s={ascii_mb_s} mixed_mb_s={mixed_mb_s} escapes_mb_s={escapes_mb_s}"
    );
}
