// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// PHOSPHOR digital-rain (`rain_quads` + `rain_atlas` + `rain_add`) CPU/GPU
// parity — the cat_parity method applied to a GENUINE `MatrixRain` emission
// (aterm-effects, deterministic seed), not hand-built quads. Rain is the same
// NEAREST 1:1 sprite regime as the cat (bake == dest size, shared RGBA texels,
// CPU integer src-over vs GPU ALPHA_BLENDING on the sRGB view), so it inherits
// the pinned effect-only bar — target <= 1, hard <= 2 — but it additionally
// exercises the one path cat parity leaves trivial: TINT. The rain ramp makes
// `SpriteQuad::tint` load-bearing (CPU quantizes the multiply to 8 bits via
// `(c*f+127)/255`; the GPU multiplies in f32 in `fs_sprite_over`), and the
// tests here assert the emission carries non-trivial tints so the pin can
// never go vacuous (design §9).
//
// Covered:
//   * base frames WITHOUT rain are byte-exact CPU==GPU (delta 0), so the
//     measured rain delta is effect-only;
//   * a genuine rain frame's sprite delta is <= 2 hard (<= 1 target), with
//     non-trivial, multi-level ramp tints asserted;
//   * `rain_add` bright-head halos over the background are BYTE-EXACT
//     (premultiplied One/One == CPU `add_sat`), the glow_parity contract;
//   * the combined frame (sprites + halos) stays within the sprite bar;
//   * empty rain channels are byte-identical on the GPU — atlas with no
//     quads, and a populated input after `clear_overlays` (the
//     rain_disabled_bytes_identical pin, GPU side).
//
// Gated: no GPU or no font -> the tests no-op (return), like the other
// parity gates. Byte-exact additive gates additionally skip on downlevel
// (sRGB-offscreen) adapters via `additive_is_byte_exact`, the glow idiom.

mod rain_common;

use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme, WindowCpu};
use rain_common::RainScene;

fn rr(p: u32) -> i32 {
    ((p >> 16) & 0xff) as i32
}
fn gg(p: u32) -> i32 {
    ((p >> 8) & 0xff) as i32
}
fn bb(p: u32) -> i32 {
    (p & 0xff) as i32
}

fn max_channel_delta(a: &[u32], b: &[u32]) -> i32 {
    let mut m = 0;
    for (&pa, &pb) in a.iter().zip(b.iter()) {
        m = m.max((rr(pa) - rr(pb)).abs());
        m = m.max((gg(pa) - gg(pb)).abs());
        m = m.max((bb(pa) - bb(pb)).abs());
    }
    m
}

fn backends(px: f32, theme: Theme) -> Option<(Renderer, aterm_gpu::GpuRenderer)> {
    let gpu = match aterm_gpu::GpuRenderer::new(px, theme) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return None;
        }
    };
    let Some(cpu) = Renderer::from_system(px, theme) else {
        eprintln!("SKIP: no system monospace font");
        return None;
    };
    Some((cpu, gpu))
}

/// THE rain parity pin (design §9/§10). The base frame is procedural
/// full-block glyphs + background — byte-exact CPU==GPU (delta 0) — so every
/// measured delta below is the rain's alone. A real engine is driven to a
/// rich frame (tinted trail levels + bright-head halos), then each channel is
/// pinned in isolation and combined:
///   (a) base without rain: delta == 0;
///   (b) sprites only: delta <= 2 hard (target <= 1), tints non-trivial and
///       multi-level so the CPU `mul8` vs GPU f32 tint path is exercised;
///   (c) halos only: BYTE-EXACT over the background (One/One == add_sat);
///   (d) sprites + halos combined: still within the sprite bar.
#[test]
fn rain_effect_only_parity_pinned_over_bg_and_text() {
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (10usize, 40usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Hidden cursor + a procedural block row: byte-exact base, and the row's
    // cells are occupied so the engine's Tier-A mask keeps rain out of them.
    term.process("\x1b[?25l████████████".as_bytes());

    // (a) Base (no rain): the effect-only premise — CPU==GPU exactly.
    let base_input = term.cell_frame(rows, cols);
    let cpu_base = cpu.render_input(&base_input);
    let gpu_base = gpu.render_input(&mut win, &base_input, None);
    assert_eq!(
        max_channel_delta(&cpu_base.pixels, &gpu_base.pixels),
        0,
        "procedural-block base must be byte-exact so the rain delta is effect-only"
    );

    // Drive a REAL engine to a genuine emission (deterministic seed).
    let mut scene = RainScene::new(rows, cols, cpu.cell_size(), &base_input);
    scene.drive_until_raining();

    // The anti-vacuity pins: the ramp must make tint load-bearing. All-white
    // tints would silently reduce this test to cat parity (design §9).
    assert!(
        scene.quads.iter().any(|q| q.tint != 0x00FF_FFFF),
        "rain emission carries only trivial (white) tints — the tint path is unexercised"
    );
    let mut tints: Vec<u32> = scene.quads.iter().map(|q| q.tint).collect();
    tints.sort_unstable();
    tints.dedup();
    assert!(
        tints.len() >= 2,
        "rain emission carries a single tint level; the ramp should light several \
         trail levels (got {tints:?})"
    );

    // (b) Sprites only: the NEAREST-1:1 + tint bar.
    let mut quads_in = term.cell_frame(rows, cols);
    quads_in.rain_quads = scene.quads.clone();
    quads_in.rain_atlas = scene.atlas();
    let cpu_q = cpu.render_input(&quads_in);
    let gpu_q = gpu.render_input(&mut win, &quads_in, None);
    assert_ne!(
        cpu_q.pixels, cpu_base.pixels,
        "the rain sprites must actually paint (non-vacuous)"
    );
    let delta_q = max_channel_delta(&cpu_q.pixels, &gpu_q.pixels);
    eprintln!(
        "rain sprites effect-only GPU vs CPU max per-channel delta = {delta_q} (target <= 1, \
         {} quads, {} tint levels)",
        scene.quads.len(),
        tints.len(),
    );
    assert!(
        delta_q <= 2,
        "rain sprite parity broke its pinned bar: max per-channel delta {delta_q} > 2 \
         (target <= 1) — the tinted NEAREST-1:1 path diverged"
    );

    // (c) Halos only: premultiplied additive light over the (byte-exact) base
    // must be BYTE-EXACT — GPU One/One == CPU add_sat. Downlevel adapters
    // fold the add into linear (accepted divergence), so gate like glow.
    let mut add_in = term.cell_frame(rows, cols);
    add_in.rain_add = scene.add.clone();
    let cpu_a = cpu.render_input(&add_in);
    let gpu_a = gpu.render_input(&mut win, &add_in, None);
    assert_ne!(
        cpu_a.pixels, cpu_base.pixels,
        "the bright-head halos must actually paint (non-vacuous)"
    );
    let delta_a = max_channel_delta(&cpu_a.pixels, &gpu_a.pixels);
    eprintln!(
        "rain_add halo GPU vs CPU max per-channel delta = {delta_a} ({} halo quads)",
        scene.add.len()
    );
    if gpu.additive_is_byte_exact() {
        assert_eq!(
            delta_a, 0,
            "rain_add premultiplied additive must be BYTE-EXACT CPU==GPU (got {delta_a})"
        );
    } else {
        eprintln!("SKIP byte-exact rain_add gate: downlevel sRGB offscreen (linear add)");
    }

    // (d) The full frame (sprites + halos), the shape the gui host actually
    // sends: still within the sprite bar (the additive layer adds no error
    // where the add is byte-exact).
    if gpu.additive_is_byte_exact() {
        let mut full_in = term.cell_frame(rows, cols);
        scene.apply(&mut full_in);
        let cpu_f = cpu.render_input(&full_in);
        let gpu_f = gpu.render_input(&mut win, &full_in, None);
        let delta_f = max_channel_delta(&cpu_f.pixels, &gpu_f.pixels);
        eprintln!("rain full-frame GPU vs CPU max per-channel delta = {delta_f}");
        assert!(
            delta_f <= 2,
            "combined rain frame diverged past the sprite bar: max delta {delta_f} > 2"
        );
    }
}

/// SCISSORED-PATH pin for the GPU rain ROW FILTER (round-3 deferral closed):
/// with a steady rain field and a ONE-ROW text change, the SCISSORED present
/// path (`present_input_readback` → `encode_present_frame` → `RepaintScope::
/// Dirty`) builds + uploads ONLY the `row_active` rain instances. The filter
/// is pixel-exact because the scissor-band fill in `compute_dirty_rows` marks
/// every band row a rain quad overlaps — an admitted quad is always on a
/// fully rebuilt row, a dropped quad is always scissor-clipped.
///
/// Non-vacuity is constructed, not hoped for: the text change lands on a row
/// that CARRIES rain quads (a wrong DROP there paints a hole the byte-compare
/// catches), rows past the band carry quads too (so the drop branch actually
/// executes), and `scissor_taken()` must tick (the frame really took the
/// Dirty scope — a Full fallback would pass trivially, per codex round-4).
/// The check needs no cross-backend tolerance: the scissored frame must be
/// BYTE-IDENTICAL to a fresh full render by an independent renderer. CPU-vs-
/// GPU stays within the pinned sprite bar on top.
#[test]
fn scissored_path_rain_row_filter_matches_full_render() {
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win_cpu = WindowCpu::new();
    let mut win_gpu = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (10usize, 40usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("\x1b[?25l████████████".as_bytes());
    let base_a = term.cell_frame(rows, cols);
    let mut scene = RainScene::new(rows, cols, cpu.cell_size(), &base_a);
    scene.drive_until_raining();
    let mut qrows: Vec<u16> = scene.quads.iter().map(|q| q.row).collect();
    qrows.sort_unstable();
    qrows.dedup();
    assert!(
        scene.quads.len() >= 10 && qrows.len() >= 3,
        "need a multi-row field to make the filter non-vacuous ({} quads on {} rows)",
        scene.quads.len(),
        qrows.len()
    );
    // The damaged row: a QUAD-BEARING row with quad rows strictly above AND
    // below it — the band ends here, so the rows below hold the quads the
    // filter must DROP while this row's quads must be KEPT and repainted.
    let hit_row = qrows[1];
    assert!(
        qrows.first() < Some(&hit_row) && qrows.last() > Some(&hit_row),
        "picked band row must have rain strictly above and below (rows {qrows:?})"
    );

    // Frame A: text A + rain — primes the CPU cache and the GPU present-path
    // offscreen/`present_prev` (this first present is the Full repaint).
    let mut in_a = base_a.clone();
    scene.apply(&mut in_a);
    let _ = cpu.render_input_cached(&mut win_cpu, &in_a);
    let _ = gpu.present_input_readback(&mut win_gpu, &in_a);

    // Frame B: ONE row of text changes; the rain is UNCHANGED (settled) —
    // exactly the sparse-damage shape the row filter optimizes.
    term.process(format!("\x1b[{};1H▒▒▒▒▒▒", hit_row + 1).as_bytes());
    let mut in_b = term.cell_frame(rows, cols);
    scene.apply(&mut in_b);

    let cpu_b = cpu
        .render_input_cached(&mut win_cpu, &in_b)
        .pixels()
        .to_vec();
    let scissors_before = gpu.scissor_taken();
    let gpu_b = gpu.present_input_readback(&mut win_gpu, &in_b).pixels;
    assert!(
        gpu.scissor_taken() > scissors_before,
        "the one-row change must take the GPU scissored path (Full would \
         bypass the row filter and pass vacuously)"
    );

    // Ground truth: a fresh full render of frame B by an INDEPENDENT renderer
    // (the nova_parity idiom — no shared caches, no shared prev state).
    let mut gpu2 = aterm_gpu::GpuRenderer::new(18.0, theme).expect("GPU was available above");
    let mut win_fresh = aterm_gpu::WindowGpu::new();
    let gpu_fresh = gpu2.render_input(&mut win_fresh, &in_b, None);
    assert_eq!(
        gpu_b, gpu_fresh.pixels,
        "scissored GPU frame (row-filtered rain instances) must be \
         byte-identical to a fresh full render of the same input"
    );

    let delta = max_channel_delta(&cpu_b, &gpu_b);
    eprintln!("scissored-path rain CPU vs GPU max per-channel delta = {delta}");
    assert!(
        delta <= 2,
        "scissored-path rain parity broke the pinned sprite bar: {delta} > 2"
    );
}

/// Empty rain channels are byte-identical on the GPU — the
/// rain_disabled_bytes_identical pin (design §10, GPU side): a disabled or
/// drained feature must leave the frame untouched. Covers an atlas with no
/// quads (uploads but draws nothing) and a populated input restored to empty
/// by `clear_overlays` (the introspection-capture contract).
#[test]
fn rain_disabled_bytes_identical_on_gpu() {
    let theme = Theme::default();
    let Some((cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (6usize, 20usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l$ matrix off");

    // Pre-feature frame: a cell_frame's rain channels default empty/None.
    let base_input = term.cell_frame(rows, cols);
    assert!(base_input.rain_quads.is_empty());
    assert!(base_input.rain_atlas.is_none());
    assert!(base_input.rain_add.is_empty());
    let base = gpu.render_input(&mut win, &base_input, None).pixels;

    // A genuine baked atlas + emission from the real engine.
    let mut scene = RainScene::new(rows, cols, cpu.cell_size(), &base_input);
    scene.drive_until_raining();

    // Atlas but no quads: uploads, draws nothing, bytes identical.
    let mut atlas_only = term.cell_frame(rows, cols);
    atlas_only.rain_atlas = scene.atlas();
    assert!(
        atlas_only.rain_atlas.is_some(),
        "the engine must have baked an atlas"
    );
    let atlas_only_px = gpu.render_input(&mut win, &atlas_only, None).pixels;
    assert_eq!(
        base, atlas_only_px,
        "a rain atlas with no quads must be byte-identical on the GPU"
    );

    // Populated (must paint), then clear_overlays (must restore the bare frame).
    let mut cleared = term.cell_frame(rows, cols);
    scene.apply(&mut cleared);
    assert!(!cleared.rain_quads.is_empty() && !cleared.rain_add.is_empty());
    let painted = gpu.render_input(&mut win, &cleared, None).pixels;
    assert_ne!(base, painted, "a live rain frame must paint on the GPU");
    cleared.clear_overlays();
    let stripped = gpu.render_input(&mut win, &cleared, None).pixels;
    assert_eq!(
        base, stripped,
        "clear_overlays must restore the bare GPU frame (quads + halos cleared, atlas nulled)"
    );
}
