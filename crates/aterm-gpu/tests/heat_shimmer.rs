// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Heat shimmer: a subtle rising per-pixel UV displacement of the finished
// frame above burning cells, derived from the `cursor_glow_add` stream the
// bloom uses. Like the bloom it is outside the CPU/GPU byte differentials
// (they call `set_shimmer(false)`) and inside everything introspection reads.
//
//   * No-op: flag off, an empty glow stream, or a region-less stream (fire on
//     the top row) presents identical pixels on the readback and present
//     paths.
//   * Bounded displacement: every shimmered pixel stays within the min/max
//     envelope of its small source neighbourhood.
//   * Scoped: outside the derived region rect the frame is byte-identical,
//     and inside it the shimmer is present.
//   * Introspection: the offscreen readback (`render_input`, the `image`
//     source), the swapchain video tap and the virtual present all carry it;
//     the two taps agree byte-for-byte with the phase pinned.
//   * Scissor: a scissored present source equals a fresh full render at the
//     same pinned phase.
//
// The pass's one wall-clock term (the rising phase) is pinned via
// `set_shimmer_phase_for_test` wherever bytes are compared.
//
// No GPU or no system font: no-op.

use aterm_core::terminal::Terminal;
use aterm_gpu::video_tap::{CaptureOpts, DEFAULT_BUDGET};
use aterm_render::{FirePatch, GlowQuad, RenderInput, Theme, premul_rgb};

use crate::common;

fn gpu_or_skip() -> Option<aterm_gpu::GpuRenderer> {
    let mut g = common::gpu_or_skip(16.0, Theme::default())?;
    g.debug_block_on_lazy_fallbacks();
    Some(g)
}

/// A warm ember comet: one row of premultiplied fire-orange glow quads over
/// `cols`, hot enough (max channel ~0.9) to drive the heat proxy to ~1.
fn fire_comet(row: usize, cols: std::ops::Range<usize>, cw: usize, ch: usize) -> Vec<GlowQuad> {
    cols.map(|c| GlowQuad {
        row: row as u16,
        x: (c * cw) as u16,
        y: (row * ch) as u16,
        w: cw as u16,
        h: ch as u16,
        color: premul_rgb(0x00FF_6A00, 230),
        // ADDITIVE light (see `GlowQuad::alpha`).
        alpha: 0,
        color2: premul_rgb(0x00FF_6A00, 230),
        alpha2: 0,
    })
    .collect()
}

/// The fire-style marker the shimmer gate keys on (`shimmer_live`): only the
/// fire style fills `fire_patch`, so phaser/laser/water frames never shimmer.
/// A zero-area patch marks the style live without rasterizing any field
/// pixel, so fixture bytes are the glow stream's alone.
fn fire_marker() -> Vec<FirePatch> {
    vec![FirePatch::default()]
}

/// Split a readback pixel (`0xTTRRGGBB`) into its four lanes.
fn lanes(p: u32) -> [i32; 4] {
    [
        ((p >> 24) & 0xff) as i32,
        ((p >> 16) & 0xff) as i32,
        ((p >> 8) & 0xff) as i32,
        (p & 0xff) as i32,
    ]
}

/// No-op: flag off / empty stream / region-less stream present identical
/// pixels on the readback and present paths. The flag-off arm runs with the
/// wall clock live, so agreement shows no time-dependent pass ran.
#[test]
fn shimmer_no_op_law_flag_off_and_empty_region() {
    let Some(mut gpu) = gpu_or_skip() else { return };
    gpu.set_bloom(false); // isolate the shimmer layer
    let (rows, cols) = (10usize, 30usize);
    let (cw, ch) = gpu.cell_size();
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"$ heat shimmer no-op law\r\nsome text under the haze");
    let plain = term.cell_frame(rows, cols);
    let mut fire = term.cell_frame(rows, cols);
    fire.cursor_glow_add = fire_comet(6, 4..14, cw, ch);
    fire.fire_patch = fire_marker();
    let mut top_fire = term.cell_frame(rows, cols);
    top_fire.cursor_glow_add = fire_comet(0, 4..14, cw, ch);
    top_fire.fire_patch = fire_marker();

    let mut win = aterm_gpu::WindowGpu::new();

    // Flag OFF with a live stream, wall clock UNPINNED: byte-stable across
    // renders — proof no time-dependent pass ran.
    gpu.set_shimmer(false);
    let off_a = gpu.render_input(&mut win, &fire, None).pixels;
    let off_b = gpu.render_input(&mut win, &fire, None).pixels;
    assert!(
        off_a == off_b,
        "flag off must run NO pass (wall-clock leak)"
    );

    // Empty stream, shimmer ON: identical to the shimmer-off render.
    let off_plain = gpu.render_input(&mut win, &plain, None).pixels;
    gpu.set_shimmer(true);
    gpu.set_shimmer_phase_for_test(Some(0.31));
    let on_plain = gpu.render_input(&mut win, &plain, None).pixels;
    assert!(
        on_plain == off_plain,
        "empty glow stream must be a byte-identical no-op"
    );

    // Region-less stream (fire on the TOP row — no air above it): the derived
    // region is None and the pass must not run.
    assert!(
        gpu.shimmer_region_for_test(&top_fire).is_none(),
        "top-row fire has no air above it: no region"
    );
    gpu.set_shimmer(false);
    let off_top = gpu.render_input(&mut win, &top_fire, None).pixels;
    gpu.set_shimmer(true);
    let on_top = gpu.render_input(&mut win, &top_fire, None).pixels;
    assert!(
        on_top == off_top,
        "a region-less stream must be a byte-identical no-op"
    );

    // The PRESENT path (the compose-present-offscreen route): same laws.
    let mut win_off = aterm_gpu::WindowGpu::new();
    gpu.set_shimmer(false);
    let p_off = gpu.present_input_readback(&mut win_off, &top_fire).pixels;
    let mut win_on = aterm_gpu::WindowGpu::new();
    gpu.set_shimmer(true);
    let p_on = gpu.present_input_readback(&mut win_on, &top_fire).pixels;
    assert!(
        p_on == p_off,
        "present path: a region-less stream must present byte-identical pixels"
    );

    // Non-vacuity for the whole suite: with a REAL region the shimmer alters
    // the readback (the visibility law proper is in the bounded test below).
    let on_fire = gpu.render_input(&mut win, &fire, None).pixels;
    assert!(
        on_fire != off_a,
        "a live hot region must actually shimmer (non-vacuous)"
    );
}

/// Bounded and scoped: over a high-contrast stripe pattern, every shimmered
/// pixel stays within the min/max of its ±2 px horizontal / ±3 px vertical
/// neighbourhood (amplitude ≤ 1.5 + bilinear footprint, ±1 rounding), alpha
/// is untouched, pixels outside the region rect are byte-identical, and the
/// region does shimmer.
#[test]
fn shimmer_bounded_displacement_and_scoped_to_region() {
    let Some(mut gpu) = gpu_or_skip() else { return };
    gpu.set_bloom(false); // isolate the shimmer layer
    let (rows, cols) = (12usize, 30usize);
    let (cw, ch) = gpu.cell_size();
    // Horizontal stripes: full-block rows alternating with background rows —
    // hard edges everywhere the haze can act on.
    let mut term = Terminal::new(rows as u16, cols as u16);
    for r in (0..rows).step_by(2) {
        term.process(format!("\x1b[{};1H{}", r + 1, "\u{2588}".repeat(cols)).as_bytes());
    }
    let mut input = term.cell_frame(rows, cols);
    input.cursor_visible = false;
    input.cursor_glow_add = fire_comet(8, 6..16, cw, ch);
    input.fire_patch = fire_marker();
    let (rx0, ry0, rx1, ry1) = gpu
        .shimmer_region_for_test(&input)
        .expect("a hot mid-grid comet derives a region");

    let mut win = aterm_gpu::WindowGpu::new();
    gpu.set_shimmer(false);
    let src = gpu.render_input(&mut win, &input, None);
    gpu.set_shimmer(true);
    gpu.set_shimmer_phase_for_test(Some(0.31));
    let out = gpu.render_input(&mut win, &input, None);
    assert_eq!((src.width, src.height), (out.width, out.height));
    let (w, h) = (src.width, src.height);

    let mut inside_diffs = 0usize;
    let mut max_diff = 0i32;
    for y in 0..h {
        for x in 0..w {
            let o = out.pixels[y * w + x];
            let s = src.pixels[y * w + x];
            let in_region =
                (x as u32) >= rx0 && (x as u32) < rx1 && (y as u32) >= ry0 && (y as u32) < ry1;
            if !in_region {
                assert!(
                    o == s,
                    "pixel ({x},{y}) outside the region rect ({rx0},{ry0})..({rx1},{ry1}) \
                     must be byte-identical (got {o:#010x} vs {s:#010x})"
                );
                continue;
            }
            let ol = lanes(o);
            let sl = lanes(s);
            // Alpha lane (transmittance byte): COLOR-only write mask.
            assert_eq!(ol[0], sl[0], "alpha lane perturbed at ({x},{y})");
            if o != s {
                inside_diffs += 1;
            }
            // Min/max envelope of the source neighbourhood.
            let mut lo = [255i32; 4];
            let mut hi = [0i32; 4];
            for yy in y.saturating_sub(3)..=(y + 3).min(h - 1) {
                for xx in x.saturating_sub(2)..=(x + 2).min(w - 1) {
                    let n = lanes(src.pixels[yy * w + xx]);
                    for c in 1..4 {
                        lo[c] = lo[c].min(n[c]);
                        hi[c] = hi[c].max(n[c]);
                    }
                }
            }
            for c in 1..4 {
                assert!(
                    ol[c] >= lo[c] - 1 && ol[c] <= hi[c] + 1,
                    "channel {c} at ({x},{y}) = {} escaped its bounded-displacement \
                     envelope [{}, {}] — the shimmer moved content farther than the \
                     amplitude allows",
                    ol[c],
                    lo[c],
                    hi[c]
                );
                max_diff = max_diff.max((ol[c] - sl[c]).abs());
            }
        }
    }
    assert!(
        inside_diffs > 0,
        "the region must actually shimmer (introspection visibility)"
    );
    assert!(
        max_diff >= 8,
        "the shimmer should be a visible refraction, not FP dust (max diff {max_diff})"
    );
}

fn opts() -> CaptureOpts {
    CaptureOpts {
        half_res: false,
        budget_bytes: DEFAULT_BUDGET,
        fps_cap: None,
        requested_ms: 0,
    }
}

/// Present `input` once through the copyable-SWAPCHAIN stand-in with the video
/// tap armed, returning the single harvested frame's bytes.
fn tap_swapchain(gpu: &mut aterm_gpu::GpuRenderer, input: &RenderInput, w: u32, h: u32) -> Vec<u8> {
    let mut win = aterm_gpu::WindowGpu::new();
    gpu.video_begin_standin_for_test(&mut win, w, h, opts())
        .expect("standin tap");
    gpu.present_swapchain_standin_for_test(&mut win, input, false, None, None, (w, h));
    gpu.video_after_present(&mut win, 1);
    let take = gpu.video_finish(&mut win).expect("swapchain take");
    assert_eq!(take.frames.len(), 1);
    take.frames[0].rgba.clone()
}

/// Present `input` once through the VIRTUAL present (81cbf35c) with the video
/// tap armed, returning the single harvested frame's bytes.
fn tap_virtual(gpu: &mut aterm_gpu::GpuRenderer, input: &RenderInput, w: u32, h: u32) -> Vec<u8> {
    let mut win = aterm_gpu::WindowGpu::new();
    gpu.virtual_begin(&mut win, w, h, opts())
        .expect("virtual tap");
    assert!(gpu.present_virtual(&mut win, input, false, None, None));
    gpu.video_after_present(&mut win, 1);
    let take = gpu.video_finish(&mut win).expect("virtual take");
    assert_eq!(take.frames.len(), 1);
    take.frames[0].rgba.clone()
}

/// All three introspection paths carry the shimmer: the offscreen readback,
/// the swapchain video tap, and the virtual present's tap. With the phase
/// pinned the two taps are byte-identical, each differs from its shimmer-off
/// counterpart only inside the region rect, and the readback shimmers
/// identically.
#[test]
fn shimmer_reaches_all_three_introspection_paths() {
    let Some(mut gpu) = gpu_or_skip() else { return };
    // These taps are the wgpu stand-in harness (the wgpu oracle arm). The
    // armed tap ring has its own differential
    // (`armed_present_tap_ring_captures_the_submit_b_destination_byte_for_byte`).
    #[cfg(target_os = "macos")]
    gpu.disarm_metal_for_oracle();
    gpu.set_bloom(true); // the full parity-class stack, as shipped
    gpu.set_sdr_glow_boost(0.0); // keep the crown's own wall-clock ease out
    gpu.set_shimmer_phase_for_test(Some(0.31));
    let (rows, cols) = (10usize, 30usize);
    let (cw, ch) = gpu.cell_size();
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"$ shimmer across every tap\r\ntext above the fire line");
    let mut input = term.cell_frame(rows, cols);
    input.cursor_glow_add = fire_comet(6, 4..14, cw, ch);
    input.fire_patch = fire_marker();
    let (rx0, ry0, rx1, ry1) = gpu
        .shimmer_region_for_test(&input)
        .expect("hot region derives");
    let (fw, fh) = gpu.frame_size(rows, cols);
    let (fw, fh) = (fw as u32, fh as u32);

    // Shimmer ON: the two tap arms must agree byte-for-byte.
    gpu.set_shimmer(true);
    let swap_on = tap_swapchain(&mut gpu, &input, fw, fh);
    let virt_on = tap_virtual(&mut gpu, &input, fw, fh);
    assert!(
        swap_on == virt_on,
        "swapchain and virtual presents must harvest byte-identical shimmer frames"
    );

    // Shimmer OFF twins: the difference is the shimmer, scoped to the region.
    gpu.set_shimmer(false);
    let swap_off = tap_swapchain(&mut gpu, &input, fw, fh);
    assert!(
        swap_on != swap_off,
        "the video tap must capture the shimmer (non-vacuous)"
    );
    let mut diffs_in = 0usize;
    for y in 0..fh as usize {
        for x in 0..fw as usize {
            let i = (y * fw as usize + x) * 4;
            let same = swap_on[i..i + 4] == swap_off[i..i + 4];
            let in_region =
                (x as u32) >= rx0 && (x as u32) < rx1 && (y as u32) >= ry0 && (y as u32) < ry1;
            if in_region {
                diffs_in += usize::from(!same);
            } else {
                assert!(
                    same,
                    "tapped pixel ({x},{y}) outside the region rect changed — the \
                     shimmer leaked past its bound"
                );
            }
        }
    }
    assert!(
        diffs_in > 0,
        "the tapped shimmer must land inside the region"
    );

    // The offscreen READBACK (the `image` introspection source) shimmers too.
    let mut win = aterm_gpu::WindowGpu::new();
    gpu.set_shimmer(true);
    let rb_on = gpu.render_input(&mut win, &input, None).pixels;
    gpu.set_shimmer(false);
    let rb_off = gpu.render_input(&mut win, &input, None).pixels;
    assert!(
        rb_on != rb_off,
        "the offscreen readback must carry the shimmer (image introspection)"
    );
}

/// Scissor: with the phase pinned, a scissored shimmer present source (one
/// keystroke, the comet moves) equals a fresh full render of the same input.
#[test]
fn shimmer_scissored_present_matches_fresh_render() {
    let Some(mut gpu) = gpu_or_skip() else { return };
    assert!(gpu.shimmer_enabled(), "shimmer must be ON by default");
    assert!(gpu.bloom_enabled(), "bloom must be ON by default");
    gpu.set_shimmer_phase_for_test(Some(0.27));
    gpu.set_cursor_blink_phase(true);
    gpu.set_cursor_style_override(None);
    let (rows, cols) = (10usize, 30usize);
    let (cw, ch) = gpu.cell_size();
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"$ prompt");
    term.process(b"\x1b[6;1Hmid row text under the haze");
    term.process(b"\x1b[1;9H");
    let mut win = aterm_gpu::WindowGpu::new();

    // Frame A: first paint (FULL) with the comet on row 6.
    let mut in_a = term.cell_frame(rows, cols);
    in_a.cursor_glow_add = fire_comet(6, 4..12, cw, ch);
    in_a.fire_patch = fire_marker();
    let _ = gpu.present_input_readback(&mut win, &in_a);

    // Frame B: one keystroke + the comet moves one column — the typing tick.
    term.process(b"x");
    let mut in_b = term.cell_frame(rows, cols);
    in_b.cursor_glow_add = fire_comet(6, 5..13, cw, ch);
    in_b.fire_patch = fire_marker();
    let scissor_before = gpu.scissor_taken();
    let got = gpu.present_input_readback(&mut win, &in_b).pixels;
    assert!(
        gpu.scissor_taken() > scissor_before,
        "typing with the shimmer alive must take the SCISSOR path"
    );

    let mut win_fresh = aterm_gpu::WindowGpu::new();
    let fresh = gpu.render_input(&mut win_fresh, &in_b, None).pixels;
    assert!(
        got == fresh,
        "scissored shimmer present is NOT byte-identical to a fresh full render"
    );
}

/// Region-derivation sanity: the rect sits strictly ABOVE the hot band,
/// spans the quads (plus the rolloff margin), and dark/empty streams derive
/// nothing.
#[test]
fn shimmer_region_derivation_sanity() {
    let Some(gpu) = gpu_or_skip() else { return };
    let (rows, cols) = (10usize, 30usize);
    let (cw, ch) = gpu.cell_size();
    let mut term = Terminal::new(rows as u16, cols as u16);
    let mut input = term.cell_frame(rows, cols);
    assert!(
        gpu.shimmer_region_for_test(&input).is_none(),
        "empty stream: no region"
    );
    input.cursor_glow_add = fire_comet(6, 4..14, cw, ch);
    let (x0, y0, x1, y1) = gpu.shimmer_region_for_test(&input).expect("region");
    let pad = gpu.pad() as u32;
    let hot_top = pad + (6 * ch) as u32;
    assert_eq!(y1, hot_top, "the pass rect ends AT the hot band's top edge");
    assert!(y0 < y1, "the rect has height (air above the fire)");
    assert!(
        x0 <= pad + (4 * cw) as u32 && x1 >= pad + (14 * cw) as u32,
        "the rect spans the burning columns"
    );
    // A dark stream (zero-brightness quads) derives nothing.
    let mut dark = term.cell_frame(rows, cols);
    dark.cursor_glow_add = (4..14)
        .map(|c| GlowQuad {
            row: 6,
            x: (c * cw) as u16,
            y: (6 * ch) as u16,
            w: cw as u16,
            h: ch as u16,
            color: 0,
            // ADDITIVE light (see `GlowQuad::alpha`).
            alpha: 0,
            color2: 0,
            alpha2: 0,
        })
        .collect();
    assert!(
        gpu.shimmer_region_for_test(&dark).is_none(),
        "dark stream: no region"
    );
}
