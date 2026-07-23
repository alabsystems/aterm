// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// GPU frame-cost benchmark: ms/frame for a full GPU render+readback of a busy
// grid, at a typical terminal size (24x80) and a large one (50x200). This is the
// per-frame GPU rendering cost (atlas build + instances + two passes + readback).
//   cargo bench -p aterm-gpu --bench gpu_frame
//
// In addition to the WARM cases (atlas pre-built outside the measured loop), this
// bench has BUILD-path cases that put the one-time build work *inside* the timed
// routine:
//   * gpu_frame_cold_atlas_24x80 — a fresh WindowGpu per iteration (created in
//     un-timed setup), so the FIRST render rasterises every glyph and runs
//     Atlas::blit (the per-row memcpy) in the measured section.
//   * gpu_frame_inline_image_cold_24x80 — a grid with a small inline image (OSC
//     1337 File=) rendered into a fresh WindowGpu, so build_image_plane runs the
//     image-plane copy (the dw==tw single-memcpy fast path) in the timed section.
//
// Skips cleanly (prints a note, no benchmarks) when there is no GPU/font.

use aterm_core::terminal::Terminal;
use aterm_gpu::GpuRenderer;
use aterm_render::Theme;
use criterion::{BatchSize, Criterion, black_box};

/// A busy grid: every cell filled with cycling text + a few colour runs, so the
/// atlas, instance buffers, and both passes are all exercised.
fn busy_term(rows: usize, cols: usize) -> Terminal {
    let mut term = Terminal::new(rows as u16, cols as u16);
    let alphabet = b"abcdefghijklmnopqrstuvwxyz0123456789 ";
    let mut line = Vec::with_capacity(cols + 16);
    for r in 0..rows {
        line.clear();
        // a colour run at the start of each row, then plain cycling glyphs
        line.extend_from_slice(b"\x1b[3");
        line.push(b'1' + (r % 6) as u8);
        line.push(b'm');
        for c in 0..cols {
            line.push(alphabet[(r + c) % alphabet.len()]);
        }
        line.extend_from_slice(b"\x1b[0m");
        if r + 1 < rows {
            line.extend_from_slice(b"\r\n");
        }
        term.process(&line);
    }
    term
}

/// Solid-colour `w`×`h` opaque RGBA PNG (mirrors tests/inline_image_parity.rs).
fn solid_png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..(w * h) {
        rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().expect("png header");
        writer.write_image_data(&rgba).expect("png data");
    }
    out
}

/// An iTerm2 OSC 1337 `File=` escape carrying a base64 PNG payload.
fn osc_1337_file(args: &str, payload: &[u8]) -> Vec<u8> {
    let b64 = aterm_codec::base64::encode(payload).expect("encode");
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b]1337;File=");
    out.extend_from_slice(args.as_bytes());
    out.push(b':');
    out.extend_from_slice(b64.as_bytes());
    out.extend_from_slice(b"\x1b\\");
    out
}

fn main() {
    let theme = Theme::default();
    let mut gpu = match GpuRenderer::new(16.0, theme) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP bench: no GPU/font available: {e}");
            return;
        }
    };
    let (name, backend) = gpu.adapter();
    eprintln!("GPU: {name} (backend {backend})");

    let mut c = Criterion::default().configure_from_args();
    for (rows, cols) in [(24usize, 80usize), (50usize, 200usize)] {
        let mut term = busy_term(rows, cols);
        // A-3: the engine builds the snapshot; the renderer consumes the value.
        // The bench measures the GPU encode + readback, so build it once outside.
        let input = term.cell_frame(rows, cols);
        // Per-window GPU state (atlas, pipelines, prev-input buffer). Warm it up
        // once so the atlas/pipeline build is not folded into the measured frame.
        let mut win = aterm_gpu::WindowGpu::new();
        gpu.render_no_readback(&mut win, &input);
        c.bench_function(&format!("gpu_frame_{rows}x{cols}"), |b| {
            b.iter(|| {
                let f = gpu.render_input(&mut win, &input, None);
                black_box(f);
            });
        });
    }

    // COLD-ATLAS build path: a FRESH WindowGpu per iteration means the atlas is
    // empty, so the first render rasterises every distinct glyph and copies each
    // into the atlas via Atlas::blit (the per-row memcpy). The fresh window is
    // built in un-timed setup; only the cold first render is measured. This is the
    // only case that exercises optimisation (1) — the warm cases above never run
    // Atlas::blit in their timed loop.
    {
        let (rows, cols) = (24usize, 80usize);
        let mut term = busy_term(rows, cols);
        let input = term.cell_frame(rows, cols);
        c.bench_function(&format!("gpu_frame_cold_atlas_{rows}x{cols}"), |b| {
            b.iter_batched(
                aterm_gpu::WindowGpu::new,
                |mut win| {
                    let f = gpu.render_input(&mut win, &input, None);
                    black_box(f);
                },
                BatchSize::SmallInput,
            );
        });
    }

    // INLINE-IMAGE build path: a small opaque image placed over the first two
    // cells of row 0 (OSC 1337 File=, same fixture shape as the inline_image
    // parity tests). Rendering into a FRESH WindowGpu runs build_image_plane with
    // an empty image cache, so the decoded footprint is copied into the per-frame
    // image texture. A single image footprint width == the packed-texture row
    // width, so the copy takes the `dw == tw` single-memcpy fast path —
    // optimisation (2). The fresh window is built in un-timed setup; only the cold
    // render (atlas build + image-plane build + passes) is measured.
    {
        let (rows, cols) = (24usize, 80usize);
        let (cw, ch) = gpu.cell_size();
        let mut term = busy_term(rows, cols);
        // Make the engine's cell-pixel size match the GPU renderer's metrics, so
        // the 2x1-cell image footprint is exactly (2*cw)x(1*ch) px (the natural,
        // unscaled case — keeps the image-plane copy on the dw==tw fast path).
        term.set_cell_pixel_size(cw as u16, ch as u16);
        // Cover cols 0-1 of row 0 with a solid image (it overwrites the busy text
        // already there at the home position).
        let png = solid_png(2 * cw as u32, ch as u32, [255, 200, 0]);
        term.process(b"\x1b[H"); // cursor home (row 0, col 0)
        term.process(&osc_1337_file("inline=1;width=2;height=1", &png));
        let input = term.cell_frame(rows, cols);
        // Only emit the image case if the snapshot actually carries an image;
        // otherwise (decoder/feature unavailable) skip it cleanly so the bench
        // still reports the cold-atlas case.
        let has_image = input.images.iter().any(|row| !row.is_empty());
        if has_image {
            c.bench_function(&format!("gpu_frame_inline_image_cold_{rows}x{cols}"), |b| {
                b.iter_batched(
                    aterm_gpu::WindowGpu::new,
                    |mut win| {
                        let f = gpu.render_input(&mut win, &input, None);
                        black_box(f);
                    },
                    BatchSize::SmallInput,
                );
            });
        } else {
            eprintln!(
                "SKIP gpu_frame_inline_image_cold: snapshot carries no inline image \
                 (image decoding unavailable in this build)"
            );
        }
    }

    c.final_summary();
}
