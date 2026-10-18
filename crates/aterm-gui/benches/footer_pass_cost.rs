// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Claude Code footer's per-frame pass on one pane, timed: for each
//! kind of frame, µs per pass at p50/p90/p99 over 4000 passes. A measurement
//! for a person, never a gate: its allocations and its work per frame are
//! ASSERTED by `claude_footer_cost_tests.rs`.
//!
//! `targo --unverified bench -p aterm-gui --features bench-support --bench footer_pass_cost`

fn main() {
    const PASSES: usize = 4000;
    for (what, p50, p90, p99) in aterm_gui::footer_pass_cost(PASSES) {
        println!("PROBE {what:>12}: µs/pass p50 {p50:.2} p90 {p90:.2} p99 {p99:.2}");
    }
}
