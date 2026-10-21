// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Settings page's frame of a preview's clock, timed: µs per native compile
//! at p50/p90/p99 over 400 frames, at short windows whose pages shed their
//! chrome and at a shipped one. A measurement for a person, never a gate.
//!
//! `targo --unverified bench -p aterm-gui --features bench-support --bench settings_frame_cost`

fn main() {
    const FRAMES: usize = 400;
    for (what, p50, p90, p99) in aterm_gui::settings_frame_cost(FRAMES) {
        println!("PROBE {what:>24}: µs/frame p50 {p50:.1} p90 {p90:.1} p99 {p99:.1}");
    }
}
