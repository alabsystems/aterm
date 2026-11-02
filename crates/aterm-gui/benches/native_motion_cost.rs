// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Benchmark measuring a native page's paint-only motion frame (MOT-1).

fn main() {
    const FRAMES: usize = 400;
    for (what, p50, p90, p99) in aterm_gui::native_motion_cost(FRAMES) {
        println!("PROBE {what:>44}: µs/frame p50 {p50:.1} p90 {p90:.1} p99 {p99:.1}");
    }
}
