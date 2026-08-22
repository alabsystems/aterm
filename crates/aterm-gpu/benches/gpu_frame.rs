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
//   * gpu_frame_inline_image_9distinct_photo_steady_40x100 — the same steady
//     9-distinct shape carrying PHOTOGRAPHIC (near-incompressible) thumbnails
//     instead of flat solid colour: the realistic-payload price of the same
//     admission-policy fix, since decode cost scales with payload entropy.
//   * gpu_frame_inline_image_9distinct_steady_24x80 — a STEADY frame carrying 9
//     DISTINCT inline images (a contact-sheet / lsix shape), rendered repeatedly
//     into a WARM window. This is the pricing workload for the GpuImageCache
//     admission policy: 9 distinct images exceeded the historical count-only
//     cap of 8, so the deterministic row-major probe order of
//     build_image_plane's layout loop missed on EVERY probe and re-decoded all
//     9 PNGs on EVERY present, forever. With the CPU-ported entry+byte-budget
//     cache the steady frames here run decode-free; the before/after delta on
//     this target prices the fix directly.
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

/// A page of PLAIN text: cycling glyphs on the DEFAULT background, with no SGR
/// colour at all, so every row is ONE horizontal background run. This is the
/// shape of the content a terminal actually spends its life showing — a man
/// page, a source file, a log tail — and the case a per-CELL background stream
/// prices at `cols` quads per row and a RUN-coalesced one at 1.
///
/// It is the deliberate best-case bound on `busy_term`, which lays a colour run
/// at the start of every row (2 runs/row): the pair brackets real content from
/// both sides, so a measured win cannot be an artefact of one fixture.
fn plain_term(rows: usize, cols: usize) -> Terminal {
    let mut term = Terminal::new(rows as u16, cols as u16);
    let alphabet = b"abcdefghijklmnopqrstuvwxyz0123456789 ";
    let mut line = Vec::with_capacity(cols + 16);
    for r in 0..rows {
        line.clear();
        for c in 0..cols {
            line.push(alphabet[(r + c) % alphabet.len()]);
        }
        if r + 1 < rows {
            line.extend_from_slice(b"\r\n");
        }
        term.process(&line);
    }
    term
}

/// A page whose rows carry REAL BACKGROUND runs — `runs` equal-width spans per
/// row, each with a different SGR *background* colour, over cycling glyphs.
///
/// This is the fixture that bounds the run-coalescing win from BELOW, and it
/// exists because the obvious candidate does not. `busy_term`'s per-row escape is
/// `\x1b[3Xm` — a FOREGROUND colour — so its background is the theme default from
/// column 0 to the last column, exactly like `plain_term`: measured, both emit the
/// same bg instance count, so the two together bracket nothing. A row that is one
/// span coalesces `cols` quads into 1; a row that is `runs` spans coalesces them
/// into `runs`, which is the shape of a status line, a diff with highlighted
/// hunks, or a `ls --color` listing on a coloured background.
fn bg_run_term(rows: usize, cols: usize, runs: usize) -> Terminal {
    let mut term = Terminal::new(rows as u16, cols as u16);
    let alphabet = b"abcdefghijklmnopqrstuvwxyz0123456789 ";
    let span = cols.div_ceil(runs.max(1));
    let mut line = Vec::with_capacity(cols * 8);
    for r in 0..rows {
        line.clear();
        for c in 0..cols {
            if c % span == 0 {
                // 41..46: a real BACKGROUND colour, cycled per span and offset per
                // row so no two adjacent rows share a span boundary colour.
                line.extend_from_slice(b"\x1b[4");
                line.push(b'1' + ((c / span + r) % 6) as u8);
                line.push(b'm');
            }
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

/// A photographic-ish PNG: per-pixel pseudo-random RGB over a slow gradient, so
/// the payload is near-INCOMPRESSIBLE and the decode does real inflate +
/// unfilter work — the shape a thumbnail of an actual photo has. `solid_png`'s
/// single flat colour is the degenerate opposite (it deflates to almost
/// nothing and decodes in microseconds), which is why a solid-colour contact
/// sheet understates any decode-avoidance fix.
fn photo_png(w: u32, h: u32, seed: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    let mut s = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    for y in 0..h {
        for x in 0..w {
            // xorshift32 — deterministic, and decorrelated enough per pixel
            // that the PNG filters cannot find a short encoding.
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let n = s.to_le_bytes();
            rgba.extend_from_slice(&[
                n[0].wrapping_add((x * 255 / w.max(1)) as u8),
                n[1].wrapping_add((y * 255 / h.max(1)) as u8),
                n[2],
                255,
            ]);
        }
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

    // BACKGROUND-RUN cases. The two fixtures above/below bracket the bg stream's
    // structure: `plain_term` is one run per row (the plain-text page), `busy_term`
    // is two (a colour run then the default). Both render a FULLY materialized
    // grid, so the bg emission walk visits every cell either way and the only thing
    // that differs is how many quads come out of it — which is exactly what
    // `last_bg_instances()` reports and what the run coalescing changes.
    for (rows, cols) in [(24usize, 80usize), (50usize, 200usize)] {
        let mut term = plain_term(rows, cols);
        let input = term.cell_frame(rows, cols);
        let mut win = aterm_gpu::WindowGpu::new();
        gpu.render_no_readback(&mut win, &input);
        // TWO-SIDED REACH GUARD, evaluated on whatever build is running so the
        // bench is a valid A/B on BOTH sides of the change:
        //   * lower — the fixture must actually reach the bg emission walk. One
        //     quad per repainted row is the floor any correct implementation
        //     emits for a fully materialized grid; fewer means the frame never
        //     rendered (a gate hit, an empty grid) and the number is a lie.
        //   * upper — the fixture must not be pathological. `rows*cols` is the
        //     per-cell shape's own ceiling; the slack covers the band/strip
        //     re-establish quads. More means something other than the cell walk
        //     is flooding the stream and the case is not pricing this code.
        // `last_instances()` deliberately, NOT the exact `last_bg_instances()` this
        // change adds: the guard must compile and run on BOTH sides of the patch so
        // the case is a real A/B baseline at HEAD. The sum is dominated by the bg +
        // glyph streams in equal measure on a fully materialized grid, so a bg drop
        // from `cols` to 1 per row shows up here as ~half the instances; the exact
        // isolation is available to tests via `last_bg_instances()`.
        let bg = gpu.last_instances();
        assert!(
            bg >= rows,
            "plain_{rows}x{cols}: {bg} instances is below one per row — the \
             fixture did not reach the emission walk"
        );
        assert!(
            bg <= 4 * rows * cols,
            "plain_{rows}x{cols}: {bg} instances exceeds the per-cell ceiling \
             — this fixture is not pricing the cell walk"
        );
        eprintln!(
            "gpu_frame_plain_{rows}x{cols}: {bg} instances/frame over {} cells \
             ({:.2} instances per cell)",
            rows * cols,
            bg as f64 / (rows * cols) as f64
        );
        c.bench_function(&format!("gpu_frame_plain_{rows}x{cols}"), |b| {
            b.iter(|| {
                let f = gpu.render_input(&mut win, &input, None);
                black_box(f);
            });
        });
    }

    // THE SAME TWO FIXTURES WITHOUT THE READBACK — the shape a real frame has.
    // `render_input` above copies the whole offscreen back to the CPU (~10 MB at
    // 50x200) and hands it to the caller; the SHIPPED path never does that, it
    // blits the offscreen to the swapchain. That readback is a fixed ~1.4 ms floor
    // that is identical for any instance count, so it swamps an emission-side
    // change by construction. `render_no_readback` runs the same FULL-scope
    // `encode_frame` (instance build + upload + both passes) and blocks on the GPU,
    // which is exactly the cost a background-run change moves.
    for (fixture, mk) in [
        ("plain", plain_term as fn(usize, usize) -> Terminal),
        ("busy", busy_term as fn(usize, usize) -> Terminal),
        // 8 real background spans per row: the LOWER bound on the coalescing win
        // (cols -> 8 quads per row instead of cols -> 1), so a measured win here
        // cannot be an artefact of an all-default-background fixture.
        (
            "bgruns8",
            (|r, c| bg_run_term(r, c, 8)) as fn(usize, usize) -> Terminal,
        ),
    ] {
        for (rows, cols) in [(24usize, 80usize), (50usize, 200usize)] {
            let mut term = mk(rows, cols);
            let input = term.cell_frame(rows, cols);
            let mut win = aterm_gpu::WindowGpu::new();
            gpu.render_no_readback(&mut win, &input);
            let n = gpu.last_instances();
            assert!(
                n >= rows && n <= 4 * rows * cols,
                "noread_{fixture}_{rows}x{cols}: {n} instances is outside the \
                 one-per-row floor / per-cell ceiling bracket"
            );
            eprintln!("gpu_frame_noread_{fixture}_{rows}x{cols}: {n} instances/frame");
            c.bench_function(&format!("gpu_frame_noread_{fixture}_{rows}x{cols}"), |b| {
                b.iter(|| {
                    gpu.render_no_readback(&mut win, &input);
                });
            });
        }
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

    // WHOLE-ROW SCROLLBACK PRESENT — the frame class `compute_dirty_rows` calls
    // `FullRepaint` (display_offset AND the absolute anchor both shift) even though
    // the grid merely slid by a known integer row delta. The CPU backend rescues it
    // with `scroll_blit_plan`; the GPU present path used to re-encode every row of
    // every scrolled frame: ~24k bg + ~24k glyph instances, the ligature planner and
    // the key prepass over every row, a full-target Clear and a full
    // present-offscreen re-copy, per notch.
    //
    // `present_encode_poll` is the right instrument: it runs the REAL present-path
    // encode (the same `encode_present_frame` gate, the same scissor decision, the
    // same persistent offscreen) and blocks until the GPU is done, WITHOUT a
    // readback — which is identical for any scope and would otherwise swamp the
    // difference between "3 rows" and "every row".
    {
        let (rows, cols) = (50usize, 200usize);
        let mut term = busy_term(rows, cols);
        // Cursor hidden: a history scroll moves the cursor's VIEWPORT row, and the
        // planner refuses any frame whose cursor moves. A real scrollback read is
        // exactly this shape (the shell is idle, the cursor parked off-screen).
        term.process(b"\x1b[?25l");
        // DENSE history — the retained rows must be full rows, or a scrolled frame's
        // full repaint is cheap for the boring reason that most cells are empty and
        // the A/B understates what the rescue saves.
        let mut filler = String::with_capacity(cols);
        for c in 0..cols - 1 {
            filler.push(char::from(b'a' + (c % 26) as u8));
        }
        for _ in 0..600 {
            term.process(filler.as_bytes());
            term.process(b"\r\n");
        }
        // Snapshots 3 rows apart. Building them OUTSIDE the timed loop keeps the
        // engine's `cell_frame` cost out of a measurement of the renderer.
        let mut frames = Vec::with_capacity(16);
        for _ in 0..16 {
            term.scroll_display(3);
            frames.push(term.cell_frame(rows, cols));
        }
        let mut win = aterm_gpu::WindowGpu::new();
        gpu.present_encode_poll(&mut win, &frames[0]);
        // TWO-SIDED REACH GUARD, on pre-existing counters so the case is a valid A/B
        // baseline at HEAD as well as with the rescue in place:
        //   * every present must take exactly one of the two arms, so the deltas must
        //     sum to the presents just driven — zero means the sweep never reached
        //     the gate and the timings below mean nothing;
        //   * and the sweep must be a real scroll, not a gate hit: a gate-hit frame
        //     still counts as scissored, so the sum (not the split) is the guard and
        //     the SPLIT is what the A/B reads (all-full before, all-scissored after).
        let (s0, f0) = (gpu.scissor_taken(), gpu.full_repaints());
        for f in frames.iter().skip(1) {
            gpu.present_encode_poll(&mut win, f);
        }
        let (scissored, fulls) = (gpu.scissor_taken() - s0, gpu.full_repaints() - f0);
        assert!(
            scissored + fulls == (frames.len() - 1) as u64,
            "present_scrollback: {scissored} scissored + {fulls} full is not the \
             {} presents this guard just drove",
            frames.len() - 1
        );
        eprintln!(
            "gpu_present_scrollback_{rows}x{cols}: {scissored} scissored / {fulls} \
             full repaints over {} scroll notches, {} instances on the last frame",
            frames.len() - 1,
            gpu.last_instances()
        );
        // PING-PONG through the snapshots so consecutive presents are ALWAYS exactly
        // one notch (3 rows) apart in one direction or the other — wrapping straight
        // from the last back to the first would be a 45-row jump, which the planner
        // refuses (|delta| >= rows retains nothing) and which is not what a scroll
        // gesture does.
        let (mut i, mut forward) = (0usize, true);
        c.bench_function(&format!("gpu_present_scrollback_{rows}x{cols}"), |b| {
            b.iter(|| {
                if forward {
                    i += 1;
                    forward = i + 1 < frames.len();
                } else {
                    i -= 1;
                    forward = i == 0;
                }
                gpu.present_encode_poll(&mut win, &frames[i]);
            });
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

    // STEADY-STATE 9-DISTINCT-IMAGE path (the GpuImageCache thrash pricing
    // workload, IMG-2). Nine DISTINCT solid PNGs (distinct colours -> distinct
    // payload Arcs) are placed as a vertical strip of 2-row thumbnails (the
    // engine LEFT-anchors every OSC 1337 image at column 0, iTerm2 semantics,
    // so a multi-column sheet would collapse onto itself — a stacked strip is
    // exactly what repeated `imgcat` produces anyway), the window is warmed
    // once (all decodes + plane build + atlas outside the timed loop), then
    // the SAME input is rendered repeatedly. Every present re-runs
    // build_image_plane's layout loop, whose cache probes precede the
    // plane-reuse fast path — so with an admission policy that cannot hold a
    // 9-image working set (the old count-only MAX=8) each timed iteration
    // pays 9 full PNG decodes; with the entry+byte-budget cache it pays 9
    // cheap Arc::ptr_eq probes and the plane-reuse exit. The workload prices
    // exactly that admission-policy delta and nothing else.
    {
        let (rows, cols) = (24usize, 80usize);
        let (cw, ch) = gpu.cell_size();
        let mut term = busy_term(rows, cols);
        // Engine cell-pixel size == GPU metrics, so every footprint decodes at
        // 1:1 (identity resample fast path): the timed delta isolates the PNG
        // DECODE the cache exists to avoid, with no resample noise on top.
        term.set_cell_pixel_size(cw as u16, ch as u16);
        // Nine 8x2-cell thumbnails stacked vertically — CUP rows 2,4,..,18
        // (1-based), so image `idx` covers 0-based rows 2*idx+1..=2*idx+2 at
        // cols 0-7 (the engine anchors OSC 1337 placements at column 0): nine
        // DISTINCT payloads (the colour varies), all fully on the 24-row grid,
        // none overlapping.
        for idx in 0..9u32 {
            let png = solid_png(
                8 * cw as u32,
                2 * ch as u32,
                [200, 20 + (idx * 25) as u8, (10 + idx * 20) as u8],
            );
            term.process(format!("\x1b[{};1H", 2 * idx + 2).as_bytes());
            term.process(&osc_1337_file("inline=1;width=8;height=2", &png));
        }
        let input = term.cell_frame(rows, cols);
        // REACH GUARDS (two-sided): the workload must actually carry MORE
        // distinct images than the historical count-only cap of 8, or it
        // cannot price the thrash at all — a decoder-less build (no image in
        // the snapshot) or a placement regression (fewer than 9 distinct
        // Arcs) skips loudly instead of publishing a vacuous number.
        let mut distinct: Vec<*const aterm_core::grid::extra::ImageData> = Vec::new();
        for row in &input.images {
            for (_c, iref) in row {
                let p = std::sync::Arc::as_ptr(&iref.image);
                if !distinct.contains(&p) {
                    distinct.push(p);
                }
            }
        }
        if distinct.len() > 8 {
            let mut win = aterm_gpu::WindowGpu::new();
            // Warm: atlas, all 9 decodes, plane pack+upload — none of that
            // belongs to the steady-state measurement.
            gpu.render_no_readback(&mut win, &input);
            c.bench_function(
                &format!("gpu_frame_inline_image_9distinct_steady_{rows}x{cols}"),
                |b| {
                    b.iter(|| {
                        let f = gpu.render_input(&mut win, &input, None);
                        black_box(f);
                    });
                },
            );
        } else {
            eprintln!(
                "SKIP gpu_frame_inline_image_9distinct_steady: snapshot carries {} \
                 distinct images (need 9 — image decoding unavailable in this build?)",
                distinct.len()
            );
        }
    }

    // THE SAME 9-DISTINCT STEADY SHAPE AT REALISTIC PAYLOAD SCALE. Identical in
    // every respect to the target above — stacked strip, warm window, steady
    // re-render of one input — except that the nine thumbnails carry
    // PHOTOGRAPHIC (near-incompressible) payloads at 24x4 cells instead of flat
    // solid colour at 8x2. That single change is the point: the admission
    // policy exists to avoid a DECODE, and decode cost scales with payload
    // entropy and area, so the solid-colour target measures the fix against a
    // near-zero decode and reports a few percent. A real contact sheet (lsix,
    // timg, `kitten icat` over a photo directory) looks like THIS one.
    {
        let (rows, cols) = (40usize, 100usize);
        let (cw, ch) = gpu.cell_size();
        let mut term = busy_term(rows, cols);
        term.set_cell_pixel_size(cw as u16, ch as u16);
        // Nine 24x4-cell photo thumbnails stacked at 0-based rows 4*idx..4*idx+3
        // (CUP is 1-based), all fully on the 40-row grid, none overlapping.
        for idx in 0..9u32 {
            let png = photo_png(24 * cw as u32, 4 * ch as u32, idx + 1);
            term.process(format!("\x1b[{};1H", 4 * idx + 1).as_bytes());
            term.process(&osc_1337_file("inline=1;width=24;height=4", &png));
        }
        let input = term.cell_frame(rows, cols);
        // Same two-sided reach guard as above: MORE distinct images than the
        // historical count-only cap of 8, or the target cannot price the thrash.
        let mut distinct: Vec<*const aterm_core::grid::extra::ImageData> = Vec::new();
        for row in &input.images {
            for (_c, iref) in row {
                let p = std::sync::Arc::as_ptr(&iref.image);
                if !distinct.contains(&p) {
                    distinct.push(p);
                }
            }
        }
        if distinct.len() > 8 {
            let mut win = aterm_gpu::WindowGpu::new();
            gpu.render_no_readback(&mut win, &input);
            c.bench_function(
                &format!("gpu_frame_inline_image_9distinct_photo_steady_{rows}x{cols}"),
                |b| {
                    b.iter(|| {
                        let f = gpu.render_input(&mut win, &input, None);
                        black_box(f);
                    });
                },
            );
        } else {
            eprintln!(
                "SKIP gpu_frame_inline_image_9distinct_photo_steady: snapshot carries {} \
                 distinct images (need 9 — image decoding unavailable in this build?)",
                distinct.len()
            );
        }
    }

    // TRAY-RESIDENT PRESENT PATH. The decorative build BADGE is `Some` for the
    // whole session once its Settings toggle is on, and a resident card used to
    // disable the scissored repaint for every present of that session — a ~200x40
    // px pill costing a full grid re-encode per keystroke echo. This case prices
    // exactly that, through the REAL gate: `present_virtual` runs the same
    // `present_to_view` compose-and-blit body as the swapchain arm (encode, bloom,
    // tray, shift, letterbox blit), so the `tray` A/B measures the present-path
    // routing and nothing else.
    //
    // Each iteration alternates between two snapshots that differ in ONE row, so a
    // working scissor has a one-row dirty set and a broken one rebuilds the grid.
    {
        let (rows, cols) = (50usize, 200usize);
        let mut term = busy_term(rows, cols);
        term.process(b"\x1b[?25l");
        let frame_a = term.cell_frame(rows, cols);
        term.process(b"\x1b[3;1Hthe one row that changes");
        let frame_b = term.cell_frame(rows, cols);
        let (fw, fh) = gpu.frame_size(rows, cols);
        let (fw, fh) = (fw as u32, fh as u32);
        // The shipped badge geometry: a small pill in the top-right corner.
        let (pw, ph) = (200u32.min(fw), 40u32.min(fh));
        let pill: Vec<u8> = [30u8, 30, 34, 220].repeat((pw * ph) as usize);
        let quad = aterm_gpu::TrayQuad {
            rgba: &pill,
            pw,
            ph,
            dx: fw.saturating_sub(pw + 8),
            dy: 8,
        };
        for (label, tray) in [("badge", Some(quad)), ("none", None)] {
            let mut win = aterm_gpu::WindowGpu::new();
            // Warm: first present is a Full repaint by construction (no prior
            // frame), second establishes the steady state being measured.
            assert!(gpu.present_virtual(&mut win, &frame_a, false, None, tray));
            assert!(gpu.present_virtual(&mut win, &frame_b, false, None, tray));
            // TWO-SIDED REACH GUARD on a measured pair of presents, evaluated on
            // whatever build is running so the case is a valid A/B on both sides:
            //   * lower — the pair must actually present (every present takes
            //     exactly one of the two arms, so the counters must move by 2).
            //     Zero means the workload never reached the gate and the timing
            //     below is meaningless.
            //   * upper — no present may take BOTH arms; a delta above 2 means
            //     something other than these presents is driving the counters.
            let (s0, f0) = (gpu.scissor_taken(), gpu.full_repaints());
            assert!(gpu.present_virtual(&mut win, &frame_a, false, None, tray));
            assert!(gpu.present_virtual(&mut win, &frame_b, false, None, tray));
            let (scissored, fulls) = (gpu.scissor_taken() - s0, gpu.full_repaints() - f0);
            assert!(
                scissored + fulls == 2,
                "present_tray_{label}: {scissored} scissored + {fulls} full is not \
                 the two presents this guard just drove"
            );
            eprintln!(
                "gpu_present_tray_{label}_{rows}x{cols}: {scissored} scissored / \
                 {fulls} full repaints per 2 presents, {} instances on the last frame",
                gpu.last_instances()
            );
            let mut flip = false;
            c.bench_function(&format!("gpu_present_tray_{label}_{rows}x{cols}"), |b| {
                b.iter(|| {
                    flip = !flip;
                    let input = if flip { &frame_b } else { &frame_a };
                    let ok = gpu.present_virtual(&mut win, input, false, None, tray);
                    black_box(ok);
                });
            });
        }
    }

    c.final_summary();
}
