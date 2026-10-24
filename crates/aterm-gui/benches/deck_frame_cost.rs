// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The top deck's frame, stage by stage (layout and paint, the prims' fingerprint, the raster
//! and the fold) and whole, on the merged deck's busy and calm scenes at 2238, 1100 and 700 px
//! (2×) and 1119 px (1×): µs per call at p50/p90/p99 and the prim count. A measurement for a
//! person, never a gate: nothing asserts a time.
//!
//! `targo --unverified bench --profile opt-test -p aterm-gui --features bench-support --bench deck_frame_cost`
//! (its scenes are debug-gated, so a build without debug assertions only says so).

#[cfg(debug_assertions)]
fn main() {
    const FRAMES: usize = 400;
    for (what, prims, p50, p90, p99) in aterm_gui::deck_frame_cost(FRAMES) {
        println!("PROBE {what:>32}: µs p50 {p50:.1} p90 {p90:.1} p99 {p99:.1} ({prims} prims)");
    }
}

#[cfg(not(debug_assertions))]
fn main() {
    eprintln!(
        "deck_frame_cost draws the debug-gated deck fixtures: run it with --profile opt-test"
    );
    std::process::exit(2);
}
