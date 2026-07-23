// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Memory-efficiency measurement harness (perf-memory work).
//!
//! Run with: `cargo test -p aterm-scrollback mem_measure -- --ignored --nocapture`

use crate::{CellAttrs, HyperlinkSpan, Line, Scrollback};
use aterm_rle::{Rle, Run};

fn print_size<T>(name: &str) -> usize {
    let s = std::mem::size_of::<T>();
    println!("  size_of::<{name}>() = {s}");
    s
}

#[test]
#[ignore = "measurement harness; run explicitly with --ignored --nocapture"]
fn mem_measure_size_of() {
    println!("=== size_of (aterm-scrollback) ===");
    print_size::<Line>("Line");
    print_size::<CellAttrs>("CellAttrs");
    print_size::<Run<CellAttrs>>("Run<CellAttrs>");
    print_size::<HyperlinkSpan>("HyperlinkSpan");
    print_size::<Rle<CellAttrs>>("Rle<CellAttrs>");
    print_size::<Option<Rle<CellAttrs>>>("Option<Rle<CellAttrs>>");
}

/// (b) ~1000 lines of scrollback with mixed content.
#[test]
#[ignore = "measurement harness; run explicitly with --ignored --nocapture"]
fn mem_measure_scrollback_mixed() {
    let mut sb = Scrollback::new(100_000, 0, 256 * 1024 * 1024);

    let n = 1000usize;
    for i in 0..n {
        match i % 4 {
            // Plain short text, no attrs.
            0 => {
                sb.push_line(Line::from(
                    format!("plain line {i}: the quick brown fox").as_str(),
                ));
            }
            // Styled text (RLE attrs with a couple of runs).
            1 => {
                let text = format!("styled line {i}: colored prompt then text");
                let len = text.chars().count() as u32;
                let half = len / 2;
                let mut rle: Rle<CellAttrs> = Rle::new();
                rle.extend_with(CellAttrs::new(0x01_FF8800, 0xFF_000000, 0b1), half);
                rle.extend_with(CellAttrs::new(0x01_00FF88, 0xFF_000000, 0), len - half);
                sb.push_line(Line::with_attrs(&text, rle));
            }
            // Hyperlinked text.
            2 => {
                let text = format!("link line {i}: click https://example.com/{i} here");
                let len = text.chars().count() as u32;
                let mut rle: Rle<CellAttrs> = Rle::new();
                rle.extend_with(CellAttrs::new(0x01_4488FF, 0xFF_000000, 0), len);
                let url: std::sync::Arc<str> =
                    std::sync::Arc::from(format!("https://example.com/{i}").as_str());
                let spans = vec![HyperlinkSpan::new(13, 33, url)];
                sb.push_line(Line::with_hyperlinks(&text, rle, spans));
            }
            // Wider / fuller line (closer to terminal width).
            _ => {
                let text: String = "x".repeat(80);
                let mut rle: Rle<CellAttrs> = Rle::new();
                rle.extend_with(CellAttrs::new(0x01_AABBCC, 0x01_112233, 0), 80);
                sb.push_line(Line::with_attrs(&text, rle));
            }
        }
    }

    let total = sb.total_memory_used();
    println!("=== (b) {n} scrollback lines (mixed) ===");
    println!("  total_memory_used = {total} bytes");
    println!("  per-line = {:.2} bytes", total as f64 / n as f64);
    println!("  size_of::<Line>() = {}", std::mem::size_of::<Line>());
}
