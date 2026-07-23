// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Memory-efficiency measurement harness (perf-memory work).
//!
//! Run with: `cargo test -p aterm-grid mem_measure -- --ignored --nocapture`
//!
//! These tests are `#[ignore]`d so they don't run in the normal suite — they
//! exist to record per-cell / per-structure byte costs before and after the
//! representation/layout optimization.

use crate::extra::{CellExtra, KittyPlaceholderData};
use crate::{Cell, CellCoord, Grid, PackedColor, PackedColors};
use aterm_rle::Run;

fn print_size<T>(name: &str) -> usize {
    let s = std::mem::size_of::<T>();
    println!("  size_of::<{name}>() = {s}");
    s
}

#[test]
#[ignore = "measurement harness; run explicitly with --ignored --nocapture"]
fn mem_measure_size_of() {
    println!("=== size_of (aterm-grid) ===");
    print_size::<Cell>("Cell");
    print_size::<CellExtra>("CellExtra");
    print_size::<CellCoord>("CellCoord");
    print_size::<KittyPlaceholderData>("KittyPlaceholderData");
    print_size::<PackedColors>("PackedColors");
    print_size::<PackedColor>("PackedColor");
    print_size::<crate::row::Row>("Row");
    print_size::<Run<u32>>("Run<u32>");
    // FxHashMap<CellCoord, CellExtra> entry footprint (key+value).
    println!(
        "  hashmap entry (CellCoord + CellExtra) = {}",
        std::mem::size_of::<CellCoord>() + std::mem::size_of::<CellExtra>()
    );
}

/// (a) Full screen of true-color RGB cells: distinct fg/bg per cell.
///
/// Drives the grid through the same RGB ring-buffer path the renderer uses.
#[test]
#[ignore = "measurement harness; run explicitly with --ignored --nocapture"]
fn mem_measure_truecolor_screen() {
    let rows: u16 = 50;
    let cols: u16 = 200;
    let mut grid = Grid::new(rows, cols);

    // Write a distinct RGB fg/bg into every cell via the extras ring buffer,
    // exactly as the styled-write path would for true-color ASCII runs.
    let extras = grid.extras_mut();
    for r in 0..rows {
        for c in 0..cols {
            let v = (u32::from(r) * u32::from(cols) + u32::from(c)) as u8;
            extras.set_rgb_ring_range(
                r,
                c,
                c + 1,
                Some([v, v.wrapping_add(1), v.wrapping_add(2)]),
                Some([v.wrapping_add(3), v.wrapping_add(4), v.wrapping_add(5)]),
                rows,
                cols,
            );
        }
    }

    let total = grid.memory_used();
    let cells = usize::from(rows) * usize::from(cols);
    println!("=== (a) true-color screen {rows}x{cols} = {cells} cells ===");
    println!("  total grid.memory_used() = {total} bytes");
    println!("  per-cell = {:.2} bytes", total as f64 / cells as f64);
}

/// (c) A screen with combining marks / wide chars.
#[test]
#[ignore = "measurement harness; run explicitly with --ignored --nocapture"]
fn mem_measure_combining_wide_screen() {
    let rows: u16 = 50;
    let cols: u16 = 200;
    let mut grid = Grid::new(rows, cols);

    // Every other cell: a base char with a combining mark (HashMap CellExtra).
    // Remaining: a non-BMP wide char (complex ring).
    let extras = grid.extras_mut();
    for r in 0..rows {
        for c in 0..cols {
            if c % 2 == 0 {
                let extra = extras.get_or_create(CellCoord::new(r, c));
                extra.add_combining('\u{0301}');
            } else {
                extras.set_complex_char_ring(r, c, '\u{1F600}', rows, cols);
            }
        }
    }

    let total = grid.memory_used();
    let cells = usize::from(rows) * usize::from(cols);
    println!("=== (c) combining/wide screen {rows}x{cols} = {cells} cells ===");
    println!("  total grid.memory_used() = {total} bytes");
    println!("  per-cell = {:.2} bytes", total as f64 / cells as f64);
}

/// Startup resident bytes for a freshly-constructed grid (perf-grid-startup).
///
/// Captures the ring Vec capacity + page-store preheat at t=0, before any
/// writes/scrolls. Run before/after the construction reservation changes.
#[test]
#[ignore = "measurement harness; run explicitly with --ignored --nocapture"]
fn mem_measure_startup_footprint() {
    for (rows, cols) in [(24u16, 80u16), (50, 200)] {
        let grid = Grid::new(rows, cols);
        let row_struct = std::mem::size_of::<crate::row::Row>();
        let ring_cap_entries = grid.ring_rows_capacity_for_test();
        let ring_cap_bytes = ring_cap_entries * row_struct;
        let pages_bytes = grid.pages_total_memory_for_test();
        let total = grid.memory_used();
        println!("=== startup footprint {rows}x{cols} ===");
        println!("  size_of::<Row>()         = {row_struct} bytes");
        println!("  ring Vec capacity        = {ring_cap_entries} entries");
        println!(
            "  ring Vec capacity bytes  = {ring_cap_bytes} ({:.1} KB)",
            ring_cap_bytes as f64 / 1024.0
        );
        println!(
            "  page-store total_memory  = {pages_bytes} ({:.1} KB)",
            pages_bytes as f64 / 1024.0
        );
        println!(
            "  grid.memory_used()       = {total} ({:.1} KB)",
            total as f64 / 1024.0
        );
    }
}

/// Direct CellExtra memory cost for a single hyperlink-only cell.
#[test]
#[ignore = "measurement harness; run explicitly with --ignored --nocapture"]
fn mem_measure_cellextra_breakdown() {
    println!("=== CellExtra field breakdown ===");
    let base = std::mem::size_of::<CellExtra>();
    println!("  size_of::<CellExtra>() = {base}");

    let mut hl = CellExtra::default();
    hl.set_hyperlink(Some(std::sync::Arc::<str>::from("https://example.com")));
    println!("  hyperlink-only memory_used = {}", hl.memory_used());

    let mut rgb = CellExtra::default();
    rgb.set_fg_rgb(Some([1, 2, 3]));
    rgb.set_bg_rgb(Some([4, 5, 6]));
    println!("  rgb-only memory_used = {}", rgb.memory_used());

    let _ = PackedColor::DEFAULT_FG;
}
