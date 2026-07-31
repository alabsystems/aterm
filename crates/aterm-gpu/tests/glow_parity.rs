// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Parity gate for the LUMEN cursor aurora (additive light): the GPU One/One
// pipeline over the linear Rgba8Unorm offscreen must match the CPU integer
// `add_sat`. Premultiplied additive is the strongest primitive — over an opaque
// background it is BYTE-EXACT (delta 0); over anti-aliased text it inherits the
// base glyph tolerance (<=8) but never widens it.
//
// Gated: no GPU / no system font -> the test no-ops (returns).

use aterm_core::terminal::Terminal;
use aterm_render::{
    BeamClip, BeamVertex, GlowQuad, HaloMode, RainHalo, Theme, WindowCpu, comet_beam, premul_rgb,
};

mod common;
use common::{backends_fontdue as backends, gg, max_channel_delta};

/// (a) BLANK-TARGET additive validation: premultiplied light over pure background
/// must be BYTE-EXACT on CPU and GPU, and equal min(255, bg+premul) per channel.
/// This empirically locks the One/One Rgba8Unorm round-trip before anything relies
/// on it.
#[test]
fn glow_additive_is_byte_exact_over_background() {
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (4usize, 16usize);
    let term = Terminal::new(rows as u16, cols as u16); // all background, no glyphs
    let (cw, ch) = cpu.cell_size();
    let mut input = {
        let mut t = term;
        t.cell_frame(rows, cols)
    };
    input.cursor_visible = false; // isolate the glow: no cursor pixels in the mix

    // A spread of premultiplied light quads at several coverages, each a full cell.
    let base = 0x0050_FA7B; // Dracula green
    for (i, a) in [40u8, 90, 160, 220, 255].iter().enumerate() {
        let col = i + 1;
        input.cursor_glow_add.push(GlowQuad {
            row: 1,
            x: (col * cw) as u16,
            y: ch as u16,
            w: cw as u16,
            h: ch as u16,
            color: premul_rgb(base, *a),
        });
    }

    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    assert_eq!(
        (cpu_frame.width, cpu_frame.height),
        (gpu_frame.width, gpu_frame.height)
    );
    let delta = max_channel_delta(&cpu_frame.pixels, &gpu_frame.pixels);
    eprintln!("LUMEN additive-over-bg max per-channel delta = {delta}");
    // Byte-exact additive holds only on native (plain-Unorm offscreen). On a downlevel
    // (GLES/WebGL2) adapter the single sRGB offscreen makes the One/One add land in
    // linear, an accepted approximation — skip the byte-exact gate there so it never
    // fires on the downlevel path. See GpuRenderer::additive_is_byte_exact.
    if gpu.additive_is_byte_exact() {
        assert_eq!(
            delta, 0,
            "premultiplied additive over a flat bg must be BYTE-EXACT CPU==GPU (got {delta})"
        );
    } else {
        eprintln!("SKIP byte-exact additive gate: downlevel sRGB offscreen (linear add)");
    }

    // And the value is exactly min(255, bg + premul): sample the brightest quad's
    // center on the CPU frame and check the math holds.
    let bg = theme.bg;
    let premul = premul_rgb(base, 255);
    let want_g = (((bg >> 8) & 0xff) + ((premul >> 8) & 0xff)).min(255) as i32;
    let cx = 5 * cw + cw / 2; // 5th quad (a=255), center
    let cy = ch + ch / 2;
    let got = cpu_frame.pixels[cy * cpu_frame.width + cx];
    assert_eq!(
        gg(got),
        want_g,
        "additive green channel must be min(255, bg+premul)"
    );
}

/// (b) FULL-FRAME over real text: a hand-built aurora (a comet across one row + a
/// crown straddling 3 rows) composited over glyphs must match within the glyph
/// tolerance (<=8) — additive preserves, never widens, the base AA divergence.
#[test]
fn glow_over_text_matches_within_tolerance() {
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (5usize, 24usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"$ cargo build --release\r\n$ ./target/release/aterm");
    let (cw, ch) = cpu.cell_size();
    let mut input = term.cell_frame(rows, cols);

    let base = 0x007A_A2F7; // a blue accent
    // Comet across row 1 (over the second command line), head-bright → tail-faint.
    for c in 0..12usize {
        let a = (40 + c * 16).min(230) as u8;
        input.cursor_glow_add.push(GlowQuad {
            row: 1,
            x: (c * cw) as u16,
            y: ch as u16,
            w: cw as u16,
            h: ch as u16,
            color: premul_rgb(base, a),
        });
    }
    // A crown straddling rows 0,1,2 at column 12 (each as its own single-row quad).
    for r in 0..3usize {
        input.cursor_glow_add.push(GlowQuad {
            row: r as u16,
            x: (12 * cw) as u16,
            y: (r * ch) as u16,
            w: (cw * 2) as u16,
            h: ch as u16,
            color: premul_rgb(base, 80),
        });
    }

    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    let delta = max_channel_delta(&cpu_frame.pixels, &gpu_frame.pixels);
    eprintln!("LUMEN over-text max per-channel delta = {delta}");
    // Additive parity holds within the glyph tolerance only on the native byte-exact
    // path; downlevel folds the add into linear (accepted), so gate it. See
    // GpuRenderer::additive_is_byte_exact.
    if gpu.additive_is_byte_exact() {
        assert!(delta <= 8, "glow over text diverges: max delta {delta} > 8");
    } else {
        eprintln!("SKIP glow-over-text parity gate: downlevel sRGB offscreen (linear add)");
    }
}

/// (c) The EMPTY-aurora code path is a TRUE no-op: a render whose
/// `cursor_glow_add` is empty must be BYTE-IDENTICAL to one where a glow quad was
/// pushed and then cleared back to empty — i.e. an emptied glow list leaves no
/// residue in the renderer or the frame. Asserted on a SINGLE backend each (CPU,
/// then mirrored on the GPU), so it isolates the glow no-op rather than measuring
/// CPU/GPU parity (which the other two tests already lock).
#[test]
fn empty_glow_is_byte_identical_to_no_glow() {
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    let (rows, cols) = (4usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"hello aterm");
    let (cw, ch) = cpu.cell_size();
    let mut input = term.cell_frame(rows, cols);
    assert!(input.cursor_glow_add.is_empty());

    // A full-cell quad we push then immediately clear, to exercise the
    // emptied-glow path (not merely a never-touched empty list).
    let quad = GlowQuad {
        row: 1,
        x: cw as u16,
        y: ch as u16,
        w: cw as u16,
        h: ch as u16,
        color: premul_rgb(0x0050_FA7B, 255),
    };

    // CPU: baseline (empty) vs pushed-then-cleared (empty again) must be byte-equal.
    let cpu_base = cpu.render_input(&input);
    input.cursor_glow_add.push(quad);
    input.cursor_glow_add.clear();
    let cpu_after = cpu.render_input(&input);
    assert_eq!(
        max_channel_delta(&cpu_base.pixels, &cpu_after.pixels),
        0,
        "empty-glow path is not a no-op on the CPU"
    );

    // GPU: same empty-vs-emptied invariant on a single backend.
    let mut win = aterm_gpu::WindowGpu::new();
    let gpu_base = gpu.render_input(&mut win, &input, None);
    input.cursor_glow_add.push(quad);
    input.cursor_glow_add.clear();
    let gpu_after = gpu.render_input(&mut win, &input, None);
    assert_eq!(
        max_channel_delta(&gpu_base.pixels, &gpu_after.pixels),
        0,
        "empty-glow path is not a no-op on the GPU"
    );
}

/// (d) DAMAGED/CACHED-PATH glow parity: tests (a)–(c) all drive `render_input`,
/// which FULL-repaints through a throwaway `WindowCpu`/`WindowGpu`. This drives
/// the per-frame PRESENTATION hot path — `render_input_cached` on a SINGLE
/// persistent renderer+window per backend — so the LUMEN additive glow must
/// survive the CPU damage-tracked path and the GPU dirty-gate too. Frame A places
/// a glow quad at P1 (priming both caches); frame B MOVES it to P2 (a real change
/// that misses the GPU gate and re-renders). Over a glyph-free background the
/// premultiplied-additive light is BYTE-EXACT, so CPU frame B must equal GPU
/// frame B with max per-channel delta 0.
#[test]
fn damaged_path_glow_parity_cpu_matches_gpu() {
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    let mut win_cpu = WindowCpu::new();
    let mut win_gpu = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (4usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16); // all background, no glyphs
    let (cw, ch) = cpu.cell_size();
    let mut base_input = term.cell_frame(rows, cols);
    base_input.cursor_visible = false; // isolate the glow: no cursor pixels in the mix

    let color = premul_rgb(0x0050_FA7B, 200); // Dracula green
    let quad_at = |col: usize, row: u16| GlowQuad {
        row,
        x: (col * cw) as u16,
        y: (row as usize * ch) as u16,
        w: cw as u16,
        h: ch as u16,
        color,
    };

    // Frame A: glow at P1 (col 2, row 1) — primes both caches.
    let mut in_a = base_input.clone();
    in_a.cursor_glow_add.push(quad_at(2, 1));
    let cpu_a = cpu
        .render_input_cached(&mut win_cpu, &in_a)
        .pixels()
        .to_vec();
    let _ = gpu.render_input_cached(&mut win_gpu, &in_a);

    // Frame B: MOVE the glow to P2 (col 10, row 2) — a genuine content change.
    let mut in_b = base_input.clone();
    in_b.cursor_glow_add.push(quad_at(10, 2));
    let misses_before = gpu.gate_misses();
    let cpu_b = cpu
        .render_input_cached(&mut win_cpu, &in_b)
        .pixels()
        .to_vec();
    let gpu_view = gpu.render_input_cached(&mut win_gpu, &in_b);
    let gpu_b = gpu_view.pixels().to_vec();
    let (gw, gh) = (gpu_view.width(), gpu_view.height());
    drop(gpu_view);

    // The moved glow is a real change → the GPU gate MISSED (a fresh re-render,
    // not a stale gate-hit returning frame A's pixels) ...
    assert!(
        gpu.gate_misses() > misses_before,
        "moved glow must MISS the GPU dirty gate (real re-render)"
    );
    // ... and frame B genuinely differs from frame A (the glow actually moved).
    assert!(cpu_a != cpu_b, "glow did not move between frames A and B");

    assert_eq!((gw, gh), (cols * cw, rows * ch), "unexpected frame size");

    // Over a glyph-free background, premultiplied additive light is BYTE-EXACT,
    // so the damaged/cached path must match the GPU to the LSB.
    let delta = max_channel_delta(&cpu_b, &gpu_b);
    eprintln!("damaged-path glow CPU vs GPU max per-channel delta = {delta}");
    // Byte-exact additive holds only on the native plain-Unorm offscreen; skip the gate
    // on a downlevel sRGB offscreen (linear add). See GpuRenderer::additive_is_byte_exact.
    if gpu.additive_is_byte_exact() {
        assert_eq!(
            delta, 0,
            "glow over bg via the cached path must be BYTE-EXACT CPU==GPU (got {delta})"
        );
    } else {
        eprintln!("SKIP damaged-path byte-exact additive gate: downlevel sRGB offscreen");
    }
}

/// (e) GPU-only BLOOM smoke + energy check. With bloom ENABLED (the default), the
/// comet glow gains a radiant halo, so the rendered frame carries strictly MORE
/// light than the SAME frame with bloom disabled (the byte-parity base). This
/// exercises the whole bloom pipeline — extract → gaussian blur → additive
/// composite — on the REAL device, proving it runs without a validation error and
/// adds energy. It deliberately does NOT assert exact pixels: the bloom is a
/// GPU-only embellishment layered outside the parity proof.
#[test]
fn bloom_adds_light_over_the_base() {
    let theme = Theme::default();
    let px = 18.0;
    let Some((cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (6usize, 24usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    let (cw, ch) = cpu.cell_size();
    let mut input = term.cell_frame(rows, cols);
    input.cursor_visible = false; // isolate the glow

    // A bright comet across the middle row.
    let base = 0x0050_FA7B;
    for c in 4..18usize {
        input.cursor_glow_add.push(GlowQuad {
            row: 3,
            x: (c * cw) as u16,
            y: (3 * ch) as u16,
            w: cw as u16,
            h: ch as u16,
            color: premul_rgb(base, 220),
        });
    }

    // Bloom ON (default): the bloom target is built with the offscreen on this first
    // render, then the halo is composited.
    gpu.set_bloom(true);
    let on = gpu.render_input(&mut win, &input, None);
    // Bloom OFF: same input + same window/offscreen (no resize) → base render only.
    gpu.set_bloom(false);
    let off = gpu.render_input(&mut win, &input, None);

    assert_eq!((on.width, on.height), (off.width, off.height));
    let green_sum = |px: &[u32]| -> u64 { px.iter().map(|p| ((p >> 8) & 0xff) as u64).sum() };
    let (son, soff) = (green_sum(&on.pixels), green_sum(&off.pixels));
    eprintln!(
        "bloom green-sum: on={son} off={soff} (delta {})",
        son - soff
    );
    assert!(
        son > soff,
        "GPU bloom must ADD light over the base: on={son} should exceed off={soff}"
    );

    // RIGOR: the bloom must SPREAD (it is a blur, not merely a brighter line). A
    // pixel a few px ABOVE the comet's top edge — where the crisp base has no glow —
    // must gain light with bloom on. (Grid origin is (0,0): pad is 0 in this test
    // renderer, as the other glow tests rely on.)
    let probe = (3 * ch).saturating_sub(3) * on.width + (10 * cw + cw / 2);
    let g_on = ((on.pixels[probe] >> 8) & 0xff) as i32;
    let g_off = ((off.pixels[probe] >> 8) & 0xff) as i32;
    eprintln!("bloom halo spill above comet: on={g_on} off={g_off}");
    assert!(
        g_on > g_off,
        "bloom must SPREAD a halo beyond the comet: above-comet green on={g_on} should exceed off={g_off}"
    );
}

/// (f) HEAD-BAND PARITY — the beam/halo twin of fire_patch_parity's
/// `fire_patch_head_band_parity_cpu_matches_gpu`: with a chrome head band
/// (`set_head`) AND interior padding (`set_pad`) on BOTH backends, the three
/// WINDOW-ABSOLUTE cursor-effect streams every non-fire style emits through —
/// the aurora (`cursor_glow_add`, here a REAL `comet_beam` polyline rising out
/// of the grid's top row into the band, tagged row 0), a `glow_halo` centred
/// IN the band, and a `glow_under` quad in the band — plus one GRID-space
/// `nova_add` quad must render BYTE-EXACT CPU==GPU, and the band pixels
/// (`y < pad + head`) must actually carry light on both backends. This pins
/// the raw (no pad/grid_top offset) emission of the window-absolute streams,
/// the `pad + head` offset the grid streams add, and — via the cached-path leg
/// — the head-TALL (grid_top, not pad-tall) row-0 top strip on the damaged
/// path; the head=0 tests above keep pinning the identity layout.
#[test]
fn glow_head_band_parity_cpu_matches_gpu() {
    const P: usize = 6; // interior pad (px per edge)
    const H: usize = 20; // chrome head band (px above the padded grid)
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    cpu.set_pad(P);
    cpu.set_head(H);
    gpu.set_pad(P);
    gpu.set_head(H);
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (6usize, 24usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("\x1b[?25l".as_bytes());
    let (cw, ch) = cpu.cell_size();
    let grid_top = P + H;

    // (a) Base (no effects): the head-band framing itself is byte-exact, and
    // the frame carries the `head` extension (`rows·ch + 2·pad + head` tall).
    let base_input = term.cell_frame(rows, cols);
    let cpu_base = cpu.render_input(&base_input);
    let gpu_base = gpu.render_input(&mut win, &base_input, None);
    assert_eq!(
        (cpu_base.width, cpu_base.height),
        (cols * cw + 2 * P, rows * ch + 2 * P + H),
        "unexpected head-band frame size"
    );
    assert_eq!(
        max_channel_delta(&cpu_base.pixels, &gpu_base.pixels),
        0,
        "head-band base must be byte-exact so the glow delta is effect-only"
    );

    // (b) A real beam through the PUBLIC rasterizer, clipped by the effects
    // box the host derives (grid box + the head band above it, bands anchored
    // at `grid_top`): a polyline rising from the grid's top row INTO the band,
    // whose above-grid quads carry row tag 0 (the damage-hint contract).
    let clip = BeamClip {
        x0: P as i32,
        y0: 0,
        x1: (P + cols * cw) as i32,
        y1: (grid_top + rows * ch) as i32,
        cell_h: ch as i32,
        origin_y: grid_top as i32,
    };
    let hue = 0x0048_C9FF;
    let verts = [
        BeamVertex {
            x: (P + 2 * cw) as f32,
            y: (grid_top + ch / 2) as f32,
            color: hue,
            cov: 230,
        },
        BeamVertex {
            x: (P + 9 * cw) as f32,
            y: (P + H / 2) as f32,
            color: 0x00FF_66CC,
            cov: 255,
        },
    ];
    let mut quads = Vec::new();
    comet_beam(&mut quads, clip, &verts, 3.0, 1, 0.0);
    assert!(
        quads
            .iter()
            .any(|q| q.row == 0 && (q.y as usize) < grid_top),
        "premise: the beam must emit an above-grid band tagged row 0"
    );

    let mut input = term.cell_frame(rows, cols);
    input.cursor_glow_add = quads;
    // The radial twin, centred IN the band (window-absolute, row tag 0) ...
    input.glow_halo.push(RainHalo {
        row: 0,
        x: (P + 12 * cw) as u16,
        y: 2,
        w: (4 * cw) as u16,
        h: (grid_top - 4) as u16,
        color: premul_rgb(hue, 200),
        cx: (P + 14 * cw) as u16,
        cy: (grid_top / 2) as u16,
        rx: (2 * cw) as u16,
        ry: grid_top as u16,
        mode: HaloMode::Add,
    });
    // ... the under-glyph stream, same window-absolute convention ...
    input.glow_under.push(GlowQuad {
        row: 0,
        x: (P + 17 * cw) as u16,
        y: 4,
        w: (2 * cw) as u16,
        h: 9,
        color: premul_rgb(0x00FF_8844, 160),
    });
    // ... and one GRID-space nova quad: pins the `pad`/`grid_top` offset the
    // grid streams add on both backends (`grid_top16 = pad + head`).
    input.nova_add.push(GlowQuad {
        row: 1,
        x: (3 * cw) as u16,
        y: (ch + 3) as u16,
        w: cw as u16,
        h: 4,
        color: premul_rgb(0x0050_FA7B, 180),
    });

    let cpu_f = cpu.render_input(&input);
    let gpu_f = gpu.render_input(&mut win, &input, None);
    assert_ne!(
        cpu_f.pixels, cpu_base.pixels,
        "the head-band streams must actually paint (non-vacuous)"
    );
    // The band really carries light on BOTH backends: some pixel strictly
    // above the grid (`y < grid_top`) differs from that backend's base.
    let band_lit =
        |painted: &[u32], base: &[u32], w: usize| (0..grid_top * w).any(|i| painted[i] != base[i]);
    assert!(
        band_lit(&cpu_f.pixels, &cpu_base.pixels, cpu_f.width),
        "CPU: the glow must draw inside the head band (y < pad + head)"
    );
    assert!(
        band_lit(&gpu_f.pixels, &gpu_base.pixels, gpu_f.width),
        "GPU: the glow must draw inside the head band (y < pad + head)"
    );
    let delta = max_channel_delta(&cpu_f.pixels, &gpu_f.pixels);
    eprintln!("glow head-band GPU vs CPU max per-channel delta = {delta}");
    if gpu.additive_is_byte_exact() {
        assert_eq!(
            delta, 0,
            "head-band glow must be BYTE-EXACT CPU==GPU (got {delta})"
        );
    } else {
        eprintln!("SKIP byte-exact head-band glow gate: downlevel sRGB offscreen");
    }

    // (c) The DAMAGED/CACHED path: prime both caches with the base, then
    // render the glow frame through `render_input_cached` — the row-0 damage
    // hints must open the head-TALL top strip (grid_top, not pad) on both
    // backends, landing byte-identical to the fresh full renders above (a
    // backend-internal law, asserted on ANY adapter).
    let mut win_cpu = WindowCpu::new();
    let mut win_gpu = aterm_gpu::WindowGpu::new();
    let _ = cpu.render_input_cached(&mut win_cpu, &base_input);
    let _ = gpu.render_input_cached(&mut win_gpu, &base_input);
    let cpu_c = cpu
        .render_input_cached(&mut win_cpu, &input)
        .pixels()
        .to_vec();
    let gpu_c = gpu
        .render_input_cached(&mut win_gpu, &input)
        .pixels()
        .to_vec();
    assert_eq!(
        cpu_c, cpu_f.pixels,
        "CPU cached head-band glow frame must equal the fresh full render"
    );
    assert_eq!(
        gpu_c, gpu_f.pixels,
        "GPU cached head-band glow frame must equal the fresh full render"
    );
}
