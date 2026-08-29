// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// HEAT-SHIMMER laws (the bloom parity class at PRESENT time): the air above
// burning cells refracts — a subtle rising per-pixel UV displacement of the
// finished frame, derived from the SAME `cursor_glow_add` stream the bloom
// feeds on. Like the bloom it is deliberately OUTSIDE the CPU/GPU byte
// differentials (they call `set_shimmer(false)`), and ALWAYS inside what
// introspection reads. This suite pins the honesty contract:
//
//   * NO-OP LAW — flag off, an empty glow stream, or a region-less stream
//     (fire on the top row: no air above it) presents BYTE-IDENTICAL pixels,
//     on both the offscreen-readback and the present paths;
//   * BOUNDED DISPLACEMENT — no pixel's value travels farther than the
//     amplitude (+ the bilinear footprint): every shimmered pixel stays
//     within the min/max envelope of its small source neighbourhood;
//   * SCOPED — outside the derived region rect the frame is byte-identical
//     (hard zero beyond the bound), and inside it the shimmer is really
//     there (non-vacuous);
//   * ALL THREE INTROSPECTION PATHS — the offscreen readback (`render_input`,
//     the control-socket `image` source), the copyable-SWAPCHAIN video tap,
//     and the VIRTUAL present (81cbf35c) all carry the shimmer, and the two
//     tap arms agree byte-for-byte with the phase pinned;
//   * SCISSOR HONESTY — a scissored application-present encoder source equals
//     a fresh full render at the same pinned phase, preserving app-boundary
//     present/introspection parity.
//
// The pass's ONE wall-clock term (the rising phase — the documented
// bloom-class exception, like the SDR crown envelope) is pinned via
// `set_shimmer_phase_for_test` wherever bytes are compared.
//
// Gated like the differential harness: no GPU / no system font -> no-op.
// The `#[ignore]`d `shimmer_visual_dump` writes the 3x-zoom phase-step PNGs +
// a difference heatmap for visual self-review (SHIMMER_PNG_DIR to place them).

use aterm_core::terminal::Terminal;
use aterm_gpu::video_tap::{CaptureOpts, DEFAULT_BUDGET};
use aterm_render::{FirePatch, GlowQuad, RenderInput, Theme, premul_rgb};

fn gpu_or_skip() -> Option<aterm_gpu::GpuRenderer> {
    match aterm_gpu::GpuRenderer::new(16.0, Theme::default()) {
        Ok(mut g) => {
            g.debug_block_on_lazy_fallbacks();
            Some(g)
        }
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            None
        }
    }
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
    })
    .collect()
}

/// The FIRE-STYLE marker the shimmer gate keys on (`shimmer_live`): only the
/// EMBERFORGE fire style fills `fire_patch`, so the heat haze never wobbles a
/// phaser/laser/water frame. A ZERO-AREA patch marks the style live without
/// rasterizing a single field pixel — the fixtures' bytes stay those of the
/// glow stream alone.
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

/// NO-OP LAW: flag off / empty stream / region-less stream (fire on the top
/// row) all present byte-identical pixels — offscreen readback AND present
/// path. The flag-off arm runs UNPINNED (wall clock live): two renders agree
/// only if no time-dependent pass ran at all.
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

/// BOUNDED DISPLACEMENT + SCOPED: over a high-contrast stripe pattern, every
/// shimmered pixel stays within the min/max envelope of its ±2 px horizontal /
/// ±3 px vertical source neighbourhood (amplitude ≤ 1.5 + bilinear footprint,
/// with ±1 rounding slack), the alpha lane is untouched, every pixel OUTSIDE
/// the derived region rect is byte-identical, and the region really shimmers.
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

/// ALL THREE INTROSPECTION PATHS carry the shimmer: the offscreen readback
/// (`render_input` — the control-socket `image` source), the copyable
/// SWAPCHAIN video tap, and the VIRTUAL present's tap. With the phase pinned
/// the two tap arms are byte-identical (the present-real theorem extended over
/// the shimmer), both differ from their shimmer-off twins ONLY inside the
/// region rect, and the offscreen readback shimmers identically.
#[test]
fn shimmer_reaches_all_three_introspection_paths() {
    let Some(mut gpu) = gpu_or_skip() else { return };
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

/// SCISSOR HONESTY (the bloom's scissor law extended): with the phase pinned,
/// a SCISSORED shimmer application-present encoder source (one keystroke, the
/// comet moves) is byte-identical to a fresh full render of the same input,
/// preserving app-boundary present/introspection parity on the incremental path.
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
        })
        .collect();
    assert!(
        gpu.shimmer_region_for_test(&dark).is_none(),
        "dark stream: no region"
    );
}

// ---------------------------------------------------------------------------
// Visual self-review harness (#[ignore]d, not a gate): renders a text frame
// with a synthetic hot region across 12 phase steps and writes 3x-zoom PNGs
// plus a union difference-heatmap PNG.
//
//   SHIMMER_PNG_DIR=/path cargo test -p aterm-gpu --test heat_shimmer \
//     -- --ignored shimmer_visual_dump --nocapture
// ---------------------------------------------------------------------------

fn write_png(path: &std::path::Path, w: usize, h: usize, rgb: &[u8]) {
    let file = std::fs::File::create(path).expect("create png");
    let wtr = std::io::BufWriter::new(file);
    let mut enc = aterm_png::Encoder::new(wtr, w as u32, h as u32);
    enc.set_color(aterm_png::ColorType::Rgb);
    enc.set_depth(aterm_png::BitDepth::Eight);
    enc.write_header()
        .expect("png header")
        .write_image_data(rgb)
        .expect("png data");
}

/// 3x nearest-neighbour zoom of a readback frame into RGB bytes.
fn zoom3_rgb(frame: &aterm_render::Frame) -> (usize, usize, Vec<u8>) {
    const Z: usize = 3;
    let (w, h) = (frame.width, frame.height);
    let mut out = vec![0u8; w * Z * h * Z * 3];
    for y in 0..h * Z {
        for x in 0..w * Z {
            let p = frame.pixels[(y / Z) * w + x / Z];
            let i = (y * w * Z + x) * 3;
            out[i] = ((p >> 16) & 0xff) as u8;
            out[i + 1] = ((p >> 8) & 0xff) as u8;
            out[i + 2] = (p & 0xff) as u8;
        }
    }
    (w * Z, h * Z, out)
}

#[test]
#[ignore = "visual self-review harness — writes PNGs, run explicitly"]
fn shimmer_visual_dump() {
    let Some(mut gpu) = gpu_or_skip() else { return };
    gpu.set_bloom(true);
    let (rows, cols) = (14usize, 40usize);
    let (cw, ch) = gpu.cell_size();
    // Text right down to the fire line (the realistic case: the burning
    // cursor cell sits under the lines already printed), plus lines BELOW it
    // that must stay crisp (the region is strictly above the hot band).
    let mut term = Terminal::new(rows as u16, cols as u16);
    for (r, line) in [
        "$ cargo build --release",
        "   Compiling aterm v0.29.0",
        "   the quick brown fox jumps over it",
        "   0123456789 ~ haze reads through me",
        "   ==== ---- ==== ---- ==== ---- ====",
        "   letters hold their shape, the air",
        "   just breathes ~ 0123456789 oOo___",
        "   =-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=",
        "", // row 8: the fire line
        "   below the fire: byte-crisp, always",
        "   ==== ---- ==== ---- ==== ---- ====",
    ]
    .iter()
    .enumerate()
    {
        term.process(format!("\x1b[{};1H{}", r + 1, line).as_bytes());
    }
    let mut input = term.cell_frame(rows, cols);
    input.cursor_visible = false;
    // The synthetic hot region: a fire line, bright core with cooler
    // shoulders, so the heat proxy varies across the columns.
    let row = 8usize;
    input.cursor_glow_add = (8..26)
        .map(|c| {
            let d = (c as i32 - 17).unsigned_abs() as u8;
            GlowQuad {
                row: row as u16,
                x: (c * cw) as u16,
                y: (row * ch) as u16,
                w: cw as u16,
                h: ch as u16,
                color: premul_rgb(0x00FF_6A00, 245u8.saturating_sub(d * 22)),
            }
        })
        .collect();
    input.fire_patch = fire_marker();

    let dir = std::env::var("SHIMMER_PNG_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("aterm-heat-shimmer"));
    std::fs::create_dir_all(&dir).expect("create output dir");

    let mut win = aterm_gpu::WindowGpu::new();
    gpu.set_shimmer(false);
    let base = gpu.render_input(&mut win, &input, None);
    let (zw, zh, rgb) = zoom3_rgb(&base);
    write_png(&dir.join("phase_base_off.png"), zw, zh, &rgb);

    // The region crop for the phase film strip: the derived rect plus a cell
    // of context on every side, so the motion reads side-by-side.
    let (rx0, ry0, rx1, ry1) = gpu.shimmer_region_for_test(&input).expect("region");
    let (sx0, sy0) = (rx0.saturating_sub(cw as u32) as usize, ry0 as usize);
    let (sx1, sy1) = (
        (rx1 as usize + cw).min(base.width),
        (ry1 as usize + 2 * ch).min(base.height),
    );
    let (strip_w, strip_h) = (sx1 - sx0, sy1 - sy0);
    let mut strip = vec![0u8; strip_w * 3 * 12 * strip_h * 3 * 3];

    gpu.set_shimmer(true);
    let mut union_diff = vec![0u16; base.width * base.height];
    for step in 0..12 {
        let phase = step as f32 * 0.15;
        gpu.set_shimmer_phase_for_test(Some(phase));
        let frame = gpu.render_input(&mut win, &input, None);
        let (zw, zh, rgb) = zoom3_rgb(&frame);
        write_png(&dir.join(format!("phase_{step:02}.png")), zw, zh, &rgb);
        // Blit this phase's 3x region crop into the film strip.
        for y in 0..strip_h * 3 {
            for x in 0..strip_w * 3 {
                let src = ((sy0 * 3 + y) * zw + sx0 * 3 + x) * 3;
                let dst = (y * strip_w * 3 * 12 + step * strip_w * 3 + x) * 3;
                strip[dst..dst + 3].copy_from_slice(&rgb[src..src + 3]);
            }
        }
        for (u, (&a, &b)) in union_diff
            .iter_mut()
            .zip(frame.pixels.iter().zip(base.pixels.iter()))
        {
            let d = lanes(a)
                .iter()
                .zip(lanes(b).iter())
                .map(|(x, y)| (x - y).unsigned_abs() as u16)
                .sum::<u16>();
            *u = (*u).max(d);
        }
    }
    write_png(
        &dir.join("phase_strip.png"),
        strip_w * 3 * 12,
        strip_h * 3,
        &strip,
    );
    // Difference heatmap (union over the 12 phases), 3x zoom: black = equal,
    // blue -> orange -> white with increasing displacement energy.
    const Z: usize = 3;
    let (w, h) = (base.width, base.height);
    let mut heat_rgb = vec![0u8; w * Z * h * Z * 3];
    for y in 0..h * Z {
        for x in 0..w * Z {
            let d = union_diff[(y / Z) * w + x / Z].min(255) as u32;
            let i = (y * w * Z + x) * 3;
            heat_rgb[i] = (d * 2).min(255) as u8;
            heat_rgb[i + 1] = d.saturating_sub(64).min(255) as u8;
            heat_rgb[i + 2] = if d == 0 {
                0
            } else {
                (96 + d / 2).min(255) as u8
            };
        }
    }
    write_png(&dir.join("diff_heatmap.png"), w * Z, h * Z, &heat_rgb);
    println!(
        "wrote {} phase PNGs + diff_heatmap.png to {}",
        12,
        dir.display()
    );
}
