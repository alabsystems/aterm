// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Parity for the cursor aurora (additive light): the GPU One/One pipeline on the Rgba8Unorm
// offscreen matches the CPU integer `add_sat`, byte-exact over an opaque background and within
// the base glyph tolerance (<=8) over anti-aliased text. Also the cached path, a chrome head
// band, and bloom (GPU-only, smoke-checked).
//
// A host without a GPU could not run it (`common::gpu`).

use aterm_core::terminal::Terminal;
use aterm_render::{
    BeamClip, BeamVertex, GlowQuad, HaloMode, RainHalo, Theme, WindowCpu, comet_beam, premul_rgb,
};

use crate::common::{backends_fontdue as backends, gg, max_channel_delta};

/// Premultiplied light over pure background is byte-exact on both backends and equals
/// `min(255, bg + premul)` per channel.
#[test]
fn glow_additive_is_byte_exact_over_background() {
    let theme = Theme::default();
    let px = 18.0;
    let (mut cpu, mut gpu) = backends(px, theme);
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
            // Additive light (see `GlowQuad::alpha`).
            alpha: 0,
            color2: premul_rgb(base, *a),
            alpha2: 0,
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
    // Byte-exact only on native; on a downlevel sRGB offscreen the add lands in linear (see
    // `GpuRenderer::additive_is_byte_exact`).
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

/// An aurora (a comet across a row and a crown over 3 rows) over glyphs matches within the
/// glyph tolerance (<=8).
#[test]
fn glow_over_text_matches_within_tolerance() {
    let theme = Theme::default();
    let px = 18.0;
    let (mut cpu, mut gpu) = backends(px, theme);
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
            alpha: 0,
            color2: premul_rgb(base, a),
            alpha2: 0,
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
            alpha: 0,
            color2: premul_rgb(base, 80),
            alpha2: 0,
        });
    }

    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    let delta = max_channel_delta(&cpu_frame.pixels, &gpu_frame.pixels);
    eprintln!("LUMEN over-text max per-channel delta = {delta}");
    if gpu.additive_is_byte_exact() {
        assert!(delta <= 8, "glow over text diverges: max delta {delta} > 8");
    } else {
        eprintln!("SKIP glow-over-text parity gate: downlevel sRGB offscreen (linear add)");
    }
}

/// Glow through the cached path (`render_input_cached` on one persistent renderer and window
/// per backend): frame A places a glow at P1; frame B moves it to P2 (missing the GPU gate)
/// and must be byte-exact CPU vs GPU over a glyph-free background; frame C drains the glow and
/// must return to each backend's own bare frame.
#[test]
fn damaged_path_glow_parity_cpu_matches_gpu() {
    let theme = Theme::default();
    let px = 18.0;
    let (mut cpu, mut gpu) = backends(px, theme);
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
        alpha: 0,
        color2: color,
        alpha2: 0,
    };

    // Frame 0: the bare frame through the cached path on a fresh window (a
    // full repaint), the oracle for the drained frame C.
    let cpu_bare = cpu
        .render_input_cached(&mut win_cpu, &base_input)
        .pixels()
        .to_vec();
    let gpu_bare = gpu
        .render_input_cached(&mut win_gpu, &base_input)
        .pixels()
        .to_vec();

    // Frame A: glow at P1 (col 2, row 1) — primes both caches.
    let mut in_a = base_input.clone();
    in_a.cursor_glow_add.push(quad_at(2, 1));
    let cpu_a = cpu
        .render_input_cached(&mut win_cpu, &in_a)
        .pixels()
        .to_vec();
    let _ = gpu.render_input_cached(&mut win_gpu, &in_a);

    // Frame B: move the glow to P2 (col 10, row 2), a real content change.
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

    // The moved glow is a real change, so the GPU gate missed (a fresh re-render, not a stale
    // hit returning frame A's pixels) ...
    assert!(
        gpu.gate_misses() > misses_before,
        "moved glow must MISS the GPU dirty gate (real re-render)"
    );
    // ... and frame B genuinely differs from frame A (the glow actually moved).
    assert!(cpu_a != cpu_b, "glow did not move between frames A and B");

    assert_eq!((gw, gh), (cols * cw, rows * ch), "unexpected frame size");

    // Additive light over a glyph-free background is byte-exact.
    let delta = max_channel_delta(&cpu_b, &gpu_b);
    eprintln!("damaged-path glow CPU vs GPU max per-channel delta = {delta}");
    if gpu.additive_is_byte_exact() {
        assert_eq!(
            delta, 0,
            "glow over bg via the cached path must be BYTE-EXACT CPU==GPU (got {delta})"
        );
    } else {
        eprintln!("SKIP damaged-path byte-exact additive gate: downlevel sRGB offscreen");
    }

    // Frame C: drain the aurora. On a damage-tracked path drained light survives unless the
    // dirty set covers the previous frame's rows (`compute_dirty_rows`); each backend's
    // drained frame must return to its own bare frame.
    let in_c = base_input.clone(); // byte-equal to frame 0: empty cursor_glow_add
    assert!(
        in_c.cursor_glow_add.is_empty(),
        "frame C must carry no glow"
    );

    // CPU: the row-preserving damaged path — the leg with teeth.
    let cpu_c = cpu
        .render_input_cached(&mut win_cpu, &in_c)
        .pixels()
        .to_vec();
    // Non-vacuity: frame B was lit relative to bare, or returning to bare would prove nothing.
    assert_ne!(
        max_channel_delta(&cpu_bare, &cpu_b),
        0,
        "NON-VACUITY: the lit frame B must differ from the bare frame on the CPU"
    );
    assert_eq!(
        max_channel_delta(&cpu_bare, &cpu_c),
        0,
        "CPU damaged path: an emptied glow must return the bare frame — residue \
         left on the rows the drained quad occupied"
    );

    // GPU leg 1: `render_input_cached` is gate-or-full-repaint, so it cannot
    // strand residue; this pins that the drained frame misses the gate. Leg 2
    // tests the residue law.
    let misses_before_c = gpu.gate_misses();
    let gpu_c = gpu
        .render_input_cached(&mut win_gpu, &in_c)
        .pixels()
        .to_vec();
    assert!(
        gpu.gate_misses() > misses_before_c,
        "draining the glow must MISS the GPU dirty gate (real re-render)"
    );
    assert_eq!(
        max_channel_delta(&gpu_bare, &gpu_c),
        0,
        "GPU gate path: an emptied glow must return the bare frame"
    );

    // GPU leg 2: the scissored present (`present_input_readback`), where rows
    // outside the dirty band are preserved: bare, A, B, drained; the drained
    // frame must equal bare.
    let mut win_sc = aterm_gpu::WindowGpu::new();
    let sc_bare = gpu.present_input_readback(&mut win_sc, &base_input).pixels;
    let _ = gpu.present_input_readback(&mut win_sc, &in_a);
    let sc_b = gpu.present_input_readback(&mut win_sc, &in_b).pixels;
    let scissors_before = gpu.scissor_taken();
    let sc_c = gpu.present_input_readback(&mut win_sc, &in_c).pixels;
    assert!(
        gpu.scissor_taken() > scissors_before,
        "the drained frame must take the GPU SCISSORED path (else this leg proves nothing)"
    );
    assert_ne!(
        max_channel_delta(&sc_bare, &sc_b),
        0,
        "NON-VACUITY: the lit frame B must differ from the bare frame on the GPU \
         scissored path"
    );
    assert_eq!(
        max_channel_delta(&sc_bare, &sc_c),
        0,
        "GPU scissored path: an emptied glow must return the bare frame — stale \
         light survived on a Load-preserved row"
    );
}

/// Bloom smoke and energy check: with bloom on the frame carries strictly more light than with
/// it off, so the extract, blur and composite run on the device. Not pixel-exact: bloom is
/// outside the parity proof.
#[test]
fn bloom_adds_light_over_the_base() {
    let theme = Theme::default();
    let px = 18.0;
    let (cpu, mut gpu) = backends(px, theme);
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
            alpha: 0,
            color2: premul_rgb(base, 220),
            alpha2: 0,
        });
    }

    // Bloom on (default): this first glow frame builds the bloom target on demand, then
    // composites the halo.
    gpu.set_bloom(true);
    let on = gpu.render_input(&mut win, &input, None);
    // Bloom off: same input, same window/offscreen (no resize), base render only.
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

    // The bloom spreads: a pixel a few px above the comet's top edge gains light.
    // Grid origin is (0, 0) (pad 0).
    let probe = (3 * ch).saturating_sub(3) * on.width + (10 * cw + cw / 2);
    let g_on = ((on.pixels[probe] >> 8) & 0xff) as i32;
    let g_off = ((off.pixels[probe] >> 8) & 0xff) as i32;
    eprintln!("bloom halo spill above comet: on={g_on} off={g_off}");
    assert!(
        g_on > g_off,
        "bloom must SPREAD a halo beyond the comet: above-comet green on={g_on} should exceed off={g_off}"
    );
}

/// Head band: with a chrome head band and interior padding on both backends, the
/// window-absolute streams (a real `comet_beam` rising into the band, a `glow_halo` and a
/// `glow_under` quad in the band) and a grid-space `nova_add` quad render byte-exact CPU==GPU,
/// and band pixels carry light. The cached-path leg checks the head-tall row-0 top strip.
#[test]
fn glow_head_band_parity_cpu_matches_gpu() {
    const P: usize = 6; // interior pad (px per edge)
    const H: usize = 20; // chrome head band (px above the padded grid)
    let theme = Theme::default();
    let px = 18.0;
    let (mut cpu, mut gpu) = backends(px, theme);
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

    // (b) A beam through the public rasterizer, clipped by the effects box
    // (grid box plus head band), rising into the band with its above-grid quads
    // tagged row 0.
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
    // The radial twin, centred in the band (window-absolute, row tag 0) ...
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
        alpha: 0,
        color2: premul_rgb(0x00FF_8844, 160),
        alpha2: 0,
    });
    // ... and one grid-space nova quad: pins the `pad`/`grid_top` offset the grid streams add
    // on both backends (`grid_top16 = pad + head`).
    input.nova_add.push(GlowQuad {
        row: 1,
        x: (3 * cw) as u16,
        y: (ch + 3) as u16,
        w: cw as u16,
        h: 4,
        color: premul_rgb(0x0050_FA7B, 180),
        alpha: 0,
        color2: premul_rgb(0x0050_FA7B, 180),
        alpha2: 0,
    });

    let cpu_f = cpu.render_input(&input);
    let gpu_f = gpu.render_input(&mut win, &input, None);
    assert_ne!(
        cpu_f.pixels, cpu_base.pixels,
        "the head-band streams must actually paint (non-vacuous)"
    );
    // The band carries light on both backends: some pixel above the grid (`y < grid_top`)
    // differs from that backend's base.
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

    // (c) Cached path: prime with the base, render the glow frame through
    // `render_input_cached`; the row-0 damage hints open the head-tall top strip
    // and the result equals the fresh full renders.
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

/// The bloom extract ramps a gradient quad across its own width. The extract
/// draws into the half-res target with full-res uniforms, so the ramp column
/// must be measured in the quad's own space. A red-to-blue gradient across 30
/// cells, bloom on minus off, sampled 3 px above: the halo is red-dominant over
/// the left end and blue-dominant over the right, like the control built from
/// 30 flat quads. Runs on the Metal renderer and (macOS) the wgpu oracle.
#[test]
fn bloom_halo_follows_a_gradient_quad_s_ramp() {
    let theme = Theme::default();
    let (rows, cols) = (8usize, 40usize);
    let (c0, c1) = (0x00E0_0000u32, 0x0000_00E0u32);
    for oracle in [false, true] {
        if oracle && !cfg!(target_os = "macos") {
            continue;
        }
        let (cpu, mut gpu) = backends(18.0, theme);
        #[cfg(target_os = "macos")]
        if oracle {
            gpu.disarm_metal_for_oracle();
        }
        let (cw, ch) = cpu.cell_size();
        let mut win = aterm_gpu::WindowGpu::new();
        let mut term = Terminal::new(rows as u16, cols as u16);
        let (x0, n) = (4usize, 30usize);
        let gradient = vec![GlowQuad::gradient(
            3,
            (x0 * cw) as u16,
            (3 * ch) as u16,
            (n * cw) as u16,
            ch as u16,
            c0,
            0,
            c1,
            0,
        )];
        let w = (n * cw) as u32;
        let cells: Vec<GlowQuad> = (0..n)
            .map(|k| {
                // The column-centre colour of cell k's middle pixel.
                let i = (k * cw + cw / 2) as u32;
                let c = aterm_render::glow_lerp_rgb(c0, c1, i, w);
                GlowQuad::flat(
                    3,
                    ((x0 + k) * cw) as u16,
                    (3 * ch) as u16,
                    cw as u16,
                    ch as u16,
                    c,
                    0,
                )
            })
            .collect();
        let mut gains = Vec::new();
        for quads in [gradient, cells] {
            let mut input = term.cell_frame(rows, cols);
            input.cursor_visible = false;
            input.cursor_glow_add = quads;
            gpu.set_bloom(true);
            let on = gpu.render_input(&mut win, &input, None);
            gpu.set_bloom(false);
            let off = gpu.render_input(&mut win, &input, None);
            let gain = |col: usize| {
                let p = (3 * ch - 3) * on.width + col * cw + cw / 2;
                let ch_of = |v: u32, sh: u32| ((v >> sh) & 0xff) as i32;
                (
                    ch_of(on.pixels[p], 16) - ch_of(off.pixels[p], 16),
                    ch_of(on.pixels[p], 0) - ch_of(off.pixels[p], 0),
                )
            };
            gains.push([gain(6), gain(18), gain(32)]);
        }
        eprintln!(
            "oracle={oracle}: (red, blue) halo gain at cols 6/18/32 — gradient {:?}, \
             flat cells {:?}",
            gains[0], gains[1]
        );
        for (name, g) in [("gradient", gains[0]), ("flat cells", gains[1])] {
            let ([r6, b6], [r32, b32]) = ([g[0].0, g[0].1], [g[2].0, g[2].1]);
            assert!(
                r6 > b6 && r6 > 0,
                "oracle={oracle} {name}: the halo over the red end is red: {g:?}"
            );
            assert!(
                b32 > r32 && b32 > 0,
                "oracle={oracle} {name}: the halo over the BLUE end is blue: {g:?}"
            );
        }
    }
}
