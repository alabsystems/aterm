// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Benchmark measuring per-frame native compile cost for Settings pages.

fn main() {
    const FRAMES: usize = 400;
    for (what, p50, p90, p99) in aterm_gui::settings_frame_cost(FRAMES) {
        println!("PROBE {what:>24}: µs/frame p50 {p50:.1} p90 {p90:.1} p99 {p99:.1}");
    }
}
