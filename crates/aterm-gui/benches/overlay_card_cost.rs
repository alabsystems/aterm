// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An open overlay card's redraw (its tray's paint and its raster), for the Overview and the
//! palette at 1×, at 2×, and at 2× with the width cap read as device pixels: µs per redraw at
//! p50/p90/p99 and the raster's size. A measurement for a person, never a gate: nothing asserts a
//! time.
//!
//! `targo --unverified bench -p aterm-gui --features bench-support --bench overlay_card_cost`

fn main() {
    const FRAMES: usize = 300;
    for (what, p50, p90, p99) in aterm_gui::overlay_card_cost(FRAMES) {
        println!("PROBE {what:>42}: µs p50 {p50:.1} p90 {p90:.1} p99 {p99:.1}");
    }
}
