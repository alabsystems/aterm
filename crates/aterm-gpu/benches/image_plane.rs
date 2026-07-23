// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Image-plane pack microbench: isolates `GpuRenderer::pack_image_plane` (the
// per-frame inline-image texture build) from GPU upload, so the `dw == tw`
// single-memcpy fast path can be measured DIRECTLY against a naive row-by-row
// pack. The full-frame gpu_frame bench cannot surface this win — one small image
// is a few KB against a ~2 ms cold frame; here the copy IS the whole measurement,
// pure CPU and deterministic (no GPU/font needed).
//   cargo bench -p aterm-gpu --bench image_plane
//
// Compare each `*_fastpath` against its `*_perrow` sibling: the delta is the win
// the `dw == tw` fast path buys. `pack_image_plane` is byte-identical to `per_row`
// (locked by renderer.rs::tests::pack_image_plane_fast_path_matches_per_row).

use aterm_gpu::GpuRenderer;
use criterion::{Criterion, black_box};

/// Reference: the pre-optimization path — always copy row by row.
fn per_row(items: &[(u32, u32, u32, &[u8])], tw: u32, th: u32) -> Vec<u8> {
    let mut data = vec![0u8; (tw * th * 4) as usize];
    for &(y0, dw, dh, rgba) in items {
        for y in 0..dh {
            let src = (y * dw * 4) as usize;
            let dst = ((y0 + y) * tw) as usize * 4;
            data[dst..dst + (dw * 4) as usize].copy_from_slice(&rgba[src..src + (dw * 4) as usize]);
        }
    }
    data
}

fn raster(w: u32, h: u32) -> Vec<u8> {
    (0..(w * h * 4)).map(|i| i as u8).collect()
}

fn main() {
    let mut c = Criterion::default().configure_from_args();

    // (A) One large full-width image (dw == tw): a ~full-screen inline image,
    // 720x432 px (≈80x24 cells at 9x18). Fast path = one 1.24 MiB memcpy; the
    // reference = 432 row copies of 2880 B.
    {
        let (w, h) = (720u32, 432u32);
        let rgba = raster(w, h);
        let items: Vec<(u32, u32, u32, &[u8])> = vec![(0, w, h, rgba.as_slice())];
        c.bench_function("image_plane_large_fastpath", |b| {
            b.iter(|| black_box(GpuRenderer::pack_image_plane(black_box(&items), w, h)));
        });
        c.bench_function("image_plane_large_perrow", |b| {
            b.iter(|| black_box(per_row(black_box(&items), w, h)));
        });
    }

    // (B) Many stacked SAME-WIDTH footprints (all dw == tw -> all fast path): 256
    // cell-sized 9x18 images. Isolates the per-footprint overhead the fast path
    // saves (one memcpy per image vs 18 short row copies).
    {
        let (cw, ch, n) = (9u32, 18u32, 256u32);
        let cell = raster(cw, ch);
        let items: Vec<(u32, u32, u32, &[u8])> =
            (0..n).map(|i| (i * ch, cw, ch, cell.as_slice())).collect();
        let th = n * ch;
        c.bench_function("image_plane_many_fastpath", |b| {
            b.iter(|| black_box(GpuRenderer::pack_image_plane(black_box(&items), cw, th)));
        });
        c.bench_function("image_plane_many_perrow", |b| {
            b.iter(|| black_box(per_row(black_box(&items), cw, th)));
        });
    }

    c.final_summary();
}
