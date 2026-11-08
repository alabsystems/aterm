// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Raster slicing measurement; no fonts, display, or timing gate.

use std::hint::black_box;
use std::time::Instant;

use aterm_render::ligature_shaping::extract_cell_slice;

fn main() {
    const ITERS: usize = 100_000;
    for cell_w in [8usize, 16, 32] {
        let (width, height) = (cell_w * 3, cell_w * 2);
        let raster: Vec<u8> = (0..width * height).map(|i| (i % 251 + 1) as u8).collect();
        let mut samples = Vec::new();
        for _ in 0..7 {
            let start = Instant::now();
            let mut checksum = 0usize;
            for i in 0..ITERS {
                let tile = extract_cell_slice(
                    black_box(&raster),
                    width,
                    height,
                    -2,
                    cell_w,
                    black_box((i % 4) as i64),
                );
                checksum += usize::from(black_box(&tile)[0]);
            }
            black_box(checksum);
            samples.push(start.elapsed().as_nanos() as f64 / ITERS as f64);
        }
        println!("cell={cell_w} height={height} ns/slice samples={samples:?}");
        samples.sort_by(f64::total_cmp);
        println!(
            "median={:.3} ns/slice debug_assertions={}",
            samples[3],
            cfg!(debug_assertions)
        );
    }
}
