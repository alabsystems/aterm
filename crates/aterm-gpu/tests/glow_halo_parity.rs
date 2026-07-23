// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// GLOW-HALO (`RenderInput.glow_halo`) CPU/GPU byte parity — the EMBERFORGE
// radial cursor-effect light stream, plumbed end-to-end in P3. The stream is
// `RainHalo`-shaped (integer elliptical falloff) and premultiplied One/One
// additive, so over a byte-exact base it inherits the STRONGEST bar in the
// harness: BYTE-EXACT CPU==GPU (delta 0), the `rain_add`/glow contract.
//
// Covered:
//   * base frames WITHOUT halos are byte-exact CPU==GPU (delta 0), so the
//     measured halo delta is effect-only;
//   * a SYNTHETIC halo field over block text — varied rx≠ry, varied colours,
//     off-centre cx/cy spanning several row bands (per-row quads sharing one
//     centre), and quads clipped at the grid edges — is BYTE-EXACT;
//   * the damaged/cached path: a halo MOVING between frames must miss the GPU
//     dirty gate and re-render byte-exactly (the prev∪cur row discipline);
//   * an emptied stream (`clear_overlays`) restores the bare GPU frame;
//   * `HaloMode::Over` (EMBERFORGE P7, the light-theme veil — CPU `over_rgb`
//     == GPU `fs_rain_glow_over` through the deco source-over blend state on
//     the Unorm view): a mixed Add+Over field — veils overlapping embers,
//     edge-clipped, multi-row — is BYTE-EXACT over BOTH dark text-on-dark
//     and light text-on-white frames (where the veil visibly DARKENS the
//     white ground: the smoke-on-light-theme law), and a MOVED veil on the
//     damaged/cached path misses the dirty gate and stays byte-exact.
//
// Gated: no GPU or no font -> the tests no-op (return), like the other parity
// gates. Byte-exact additive gates additionally skip on downlevel
// (sRGB-offscreen) adapters via `additive_is_byte_exact`, the glow idiom.

use aterm_core::terminal::Terminal;
use aterm_render::{HaloMode, RainHalo, Renderer, Theme, WindowCpu};

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

/// Emit one halo as its legal per-row-band quads (the producer contract): the
/// covered rect `centre ± (rx, ry)` is clamped to the grid interior, then
/// split at cell-row boundaries — each slice tagged with its own `row` while
/// SHARING the one falloff centre. Returns nothing for a fully-clipped halo.
#[allow(clippy::too_many_arguments)]
fn emit_halo(
    out: &mut Vec<RainHalo>,
    cx: usize,
    cy: usize,
    rx: u16,
    ry: u16,
    color: u32,
    ch: usize,
    grid_w: usize,
    grid_h: usize,
) {
    assert!(cx < grid_w && cy < grid_h, "centre must be grid-interior");
    let x0 = cx.saturating_sub(rx as usize);
    let x1 = (cx + rx as usize + 1).min(grid_w);
    let y0 = cy.saturating_sub(ry as usize);
    let y1 = (cy + ry as usize + 1).min(grid_h);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let mut y = y0;
    while y < y1 {
        let row = y / ch;
        let band_end = ((row + 1) * ch).min(y1);
        out.push(RainHalo {
            row: row as u16,
            x: x0 as u16,
            y: y as u16,
            w: (x1 - x0) as u16,
            h: (band_end - y) as u16,
            color,
            cx: cx as u16,
            cy: cy as u16,
            rx: rx.max(1),
            ry: ry.max(1),
            mode: HaloMode::Add,
        });
        y = band_end;
    }
}

/// THE glow_halo parity pin. The base frame is procedural full-block glyphs +
/// background — byte-exact CPU==GPU (delta 0) — so the halo delta below is the
/// stream's alone. A synthetic field of halos with varied `rx != ry`, varied
/// colours, off-centre `cx`/`cy` spanning several row bands, and quads clipped
/// at the grid edges is then composited OVER that text on both backends: the
/// frames must be BYTE-IDENTICAL (the integer-falloff parity contract).
#[test]
fn glow_halo_synthetic_field_is_byte_exact_over_text() {
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (10usize, 40usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Hidden cursor + procedural block rows: a byte-exact base with TEXT under
    // the halos (blocks render identically on both backends — delta 0).
    term.process("\x1b[?25l".as_bytes());
    for r in 1..8usize {
        term.process(format!("\x1b[{};1H{}", r + 1, "█".repeat(30)).as_bytes());
    }
    let (cw, ch) = cpu.cell_size();
    let (grid_w, grid_h) = (cols * cw, rows * ch);

    // (a) Base (no halos): the effect-only premise — CPU==GPU exactly.
    let base_input = term.cell_frame(rows, cols);
    let cpu_base = cpu.render_input(&base_input);
    let gpu_base = gpu.render_input(&mut win, &base_input, None);
    assert_eq!(
        max_channel_delta(&cpu_base.pixels, &gpu_base.pixels),
        0,
        "procedural-block base must be byte-exact so the halo delta is effect-only"
    );

    // (b) The synthetic field — every RainHalo degree of freedom exercised.
    let mut halos = Vec::new();
    // A tall ember straddling three row bands (rx != ry, off-centre in its band).
    emit_halo(
        &mut halos,
        6 * cw + cw / 2,
        2 * ch + ch / 3,
        (2 * cw) as u16,
        (3 * ch / 2) as u16,
        0x00FF_6018,
        ch,
        grid_w,
        grid_h,
    );
    // A squat wide ember (ry < rx) mid-grid, over the blocks.
    emit_halo(
        &mut halos,
        12 * cw,
        4 * ch + ch / 2,
        (3 * cw) as u16,
        (ch / 2).max(1) as u16,
        0x0030_C0FF,
        ch,
        grid_w,
        grid_h,
    );
    // Clipped at the LEFT edge: centre 2px in, the quad clamps at x=0.
    emit_halo(
        &mut halos,
        2,
        5 * ch + ch / 2,
        (2 * cw) as u16,
        ch as u16,
        0x0080_FF80,
        ch,
        grid_w,
        grid_h,
    );
    // Clipped at the BOTTOM-RIGHT corner: quads clamp at both far edges.
    emit_halo(
        &mut halos,
        grid_w - 3,
        grid_h - 2,
        (2 * cw) as u16,
        ch as u16,
        0x00FF_FFFF,
        ch,
        grid_w,
        grid_h,
    );
    // Clipped at the TOP edge: centre 2px down, the crown clamps at y=0.
    emit_halo(
        &mut halos,
        20 * cw,
        2,
        (3 * cw) as u16,
        ch as u16,
        0x00C8_40E0,
        ch,
        grid_w,
        grid_h,
    );
    // Anti-vacuity: the field must be multi-quad, multi-row, and carry a
    // genuine row-spanner (per-row quads sharing one centre).
    let mut qrows: Vec<u16> = halos.iter().map(|q| q.row).collect();
    qrows.sort_unstable();
    qrows.dedup();
    assert!(
        halos.len() > qrows.len() && qrows.len() >= 4,
        "need a spanning multi-row field ({} quads on {} rows)",
        halos.len(),
        qrows.len()
    );

    let mut input = term.cell_frame(rows, cols);
    input.glow_halo = halos;
    let cpu_f = cpu.render_input(&input);
    let gpu_f = gpu.render_input(&mut win, &input, None);
    assert_ne!(
        cpu_f.pixels, cpu_base.pixels,
        "the synthetic halos must actually paint (non-vacuous)"
    );
    let delta = max_channel_delta(&cpu_f.pixels, &gpu_f.pixels);
    eprintln!(
        "glow_halo synthetic-field GPU vs CPU max per-channel delta = {delta} ({} quads)",
        input.glow_halo.len()
    );
    // Byte-exact additive holds only on native (plain-Unorm offscreen); the
    // downlevel sRGB offscreen folds the add into linear — the glow idiom.
    if gpu.additive_is_byte_exact() {
        assert_eq!(
            delta, 0,
            "glow_halo radial additive must be BYTE-EXACT CPU==GPU (got {delta})"
        );
    } else {
        eprintln!("SKIP byte-exact glow_halo gate: downlevel sRGB offscreen (linear add)");
    }
}

/// DAMAGED/CACHED-PATH parity: the per-frame presentation hot path
/// (`render_input_cached` on persistent renderer+window per backend). Frame A
/// places a halo at P1 (priming both caches); frame B MOVES it to P2 — a real
/// change that must MISS the GPU dirty gate, repaint the prev∪cur halo rows
/// (the `glow_halo_changed` discipline), and land byte-exact over the
/// glyph-free background on both backends.
#[test]
fn damaged_path_glow_halo_parity_cpu_matches_gpu() {
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win_cpu = WindowCpu::new();
    let mut win_gpu = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (6usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16); // all background
    let (cw, ch) = cpu.cell_size();
    let mut base_input = term.cell_frame(rows, cols);
    base_input.cursor_visible = false; // isolate the halo: no cursor pixels

    let halo_at = |col: usize, row: u16| RainHalo {
        row,
        x: (col * cw) as u16,
        y: (row as usize * ch) as u16,
        w: (2 * cw) as u16,
        h: ch as u16,
        color: 0x0060_FF30,
        cx: (col * cw + cw) as u16,
        cy: (row as usize * ch + ch / 2) as u16,
        rx: cw as u16,
        ry: (ch / 2).max(1) as u16,
        mode: HaloMode::Add,
    };

    // Frame A: halo at P1 (col 2, row 1) — primes both caches.
    let mut in_a = base_input.clone();
    in_a.glow_halo.push(halo_at(2, 1));
    let _ = cpu.render_input_cached(&mut win_cpu, &in_a);
    let _ = gpu.render_input_cached(&mut win_gpu, &in_a);

    // Frame B: MOVE the halo to P2 (col 9, row 4) — a genuine content change.
    let mut in_b = base_input.clone();
    in_b.glow_halo.push(halo_at(9, 4));
    let misses_before = gpu.gate_misses();
    let cpu_b = cpu
        .render_input_cached(&mut win_cpu, &in_b)
        .pixels()
        .to_vec();
    let gpu_b = gpu
        .render_input_cached(&mut win_gpu, &in_b)
        .pixels()
        .to_vec();
    assert!(
        gpu.gate_misses() > misses_before,
        "a moved glow_halo must MISS the GPU dirty gate (real re-render)"
    );

    // Ground truth: the damaged frame must equal a FRESH full render (no ghost
    // at the vacated P1 rows, no missing light at P2) ...
    let cpu_fresh = cpu.render_input(&in_b).pixels.clone();
    assert_eq!(
        cpu_b, cpu_fresh,
        "CPU cached-damaged glow_halo frame must equal a fresh full render"
    );
    // ... and byte-exact across backends over the glyph-free background.
    let delta = max_channel_delta(&cpu_b, &gpu_b);
    eprintln!("damaged-path glow_halo CPU vs GPU max per-channel delta = {delta}");
    if gpu.additive_is_byte_exact() {
        assert_eq!(
            delta, 0,
            "glow_halo over bg via the cached path must be BYTE-EXACT CPU==GPU (got {delta})"
        );
    } else {
        eprintln!("SKIP damaged-path byte-exact glow_halo gate: downlevel sRGB offscreen");
    }
}

/// A calm LIGHT theme (white ground, near-black ink) — the frame additive
/// light is invisible on, i.e. the frame `HaloMode::Over` exists for.
fn light_theme() -> Theme {
    Theme {
        fg: 0x0020_2020,
        bg: 0x00FF_FFFF,
        ..Theme::default()
    }
}

/// THE `HaloMode::Over` parity pin, over BOTH grounds: a mixed Add+Over halo
/// field (a smoke veil dead-centred on an ember — the overlap exercises the
/// Add-then-Over split order — plus multi-row and edge-clipped veils) over
/// block text must be BYTE-EXACT CPU==GPU on a dark text-on-dark frame AND on
/// a light text-on-white frame. On the white ground the veil must visibly
/// DARKEN the frame (the smoke-on-light-theme law — additive smoke cannot),
/// so the byte-exact gate is provably non-vacuous on the path that matters.
#[test]
fn over_veils_byte_exact_over_dark_and_light_frames() {
    for (label, theme) in [("dark", Theme::default()), ("light", light_theme())] {
        let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
            return;
        };
        let mut win = aterm_gpu::WindowGpu::new();
        let (rows, cols) = (10usize, 40usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process("\x1b[?25l".as_bytes());
        for r in 1..8usize {
            term.process(format!("\x1b[{};1H{}", r + 1, "█".repeat(30)).as_bytes());
        }
        let (cw, ch) = cpu.cell_size();
        let (grid_w, grid_h) = (cols * cw, rows * ch);

        // (a) Base (no halos): the effect-only premise — CPU==GPU exactly.
        let base_input = term.cell_frame(rows, cols);
        let cpu_base = cpu.render_input(&base_input);
        let gpu_base = gpu.render_input(&mut win, &base_input, None);
        assert_eq!(
            max_channel_delta(&cpu_base.pixels, &gpu_base.pixels),
            0,
            "({label}) block base must be byte-exact so the veil delta is effect-only"
        );

        // (b) The field: an Add ember with a grey smoke veil dead on top of it
        // (same centre/radii — the Add-then-Over order is visible in every
        // overlapped byte), a tall veil straddling three row bands, and a veil
        // clipped at the bottom-right corner.
        let mut halos = Vec::new();
        let (ecx, ecy) = (6 * cw + cw / 2, 3 * ch + ch / 2);
        emit_halo(
            &mut halos,
            ecx,
            ecy,
            (2 * cw) as u16,
            ch as u16,
            0x00FF_6018,
            ch,
            grid_w,
            grid_h,
        );
        let over_from = halos.len();
        emit_halo(
            &mut halos,
            ecx,
            ecy,
            (2 * cw) as u16,
            ch as u16,
            0x0030_3038,
            ch,
            grid_w,
            grid_h,
        );
        emit_halo(
            &mut halos,
            14 * cw,
            4 * ch + ch / 3,
            (2 * cw) as u16,
            (3 * ch / 2) as u16,
            0x0018_1810,
            ch,
            grid_w,
            grid_h,
        );
        emit_halo(
            &mut halos,
            grid_w - 3,
            grid_h - 2,
            (2 * cw) as u16,
            ch as u16,
            0x0040_2020,
            ch,
            grid_w,
            grid_h,
        );
        for q in &mut halos[over_from..] {
            q.mode = HaloMode::Over;
        }
        assert!(
            halos[over_from..].len() >= 5,
            "({label}) need a multi-quad, row-spanning Over field"
        );

        let mut input = term.cell_frame(rows, cols);
        input.glow_halo = halos;
        let cpu_f = cpu.render_input(&input);
        let gpu_f = gpu.render_input(&mut win, &input, None);
        assert_ne!(
            cpu_f.pixels, cpu_base.pixels,
            "({label}) the veils must actually paint (non-vacuous)"
        );
        if label == "light" {
            // The law the mode exists for: smoke DARKENS the white ground.
            let pad = (cpu_f.width - grid_w) / 2;
            let smoke_centre = (pad + 14 * cw) + (pad + 4 * ch + ch / 3) * cpu_f.width;
            assert!(
                luma(cpu_f.pixels[smoke_centre]) < luma(cpu_base.pixels[smoke_centre]),
                "an Over veil must darken the white ground at its centre"
            );
        }
        let delta = max_channel_delta(&cpu_f.pixels, &gpu_f.pixels);
        eprintln!(
            "glow_halo Over ({label}) GPU vs CPU max per-channel delta = {delta} ({} quads)",
            input.glow_halo.len()
        );
        if gpu.additive_is_byte_exact() {
            assert_eq!(
                delta, 0,
                "({label}) Over veils must be BYTE-EXACT CPU==GPU (got {delta})"
            );
        } else {
            eprintln!("SKIP byte-exact Over gate ({label}): downlevel sRGB offscreen");
        }
    }
}

/// Summed-RGB luminance proxy (monotone per channel, so ordering is exact).
fn luma(p: u32) -> i32 {
    rr(p) + gg(p) + bb(p)
}

/// DAMAGED/CACHED-PATH parity for `HaloMode::Over` on the light theme: frame A
/// places a smoke veil at P1, frame B MOVES it to P2 — the move must MISS the
/// GPU dirty gate, repaint the prev∪cur rows, and land byte-exact (the Add
/// test's discipline, veil edition — this pins the Over pipeline inside the
/// scissored repaint pass).
#[test]
fn damaged_path_over_veil_parity_cpu_matches_gpu() {
    let Some((mut cpu, mut gpu)) = backends(18.0, light_theme()) else {
        return;
    };
    let mut win_cpu = WindowCpu::new();
    let mut win_gpu = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (6usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16); // all background
    let (cw, ch) = cpu.cell_size();
    let mut base_input = term.cell_frame(rows, cols);
    base_input.cursor_visible = false; // isolate the veil: no cursor pixels

    let veil_at = |col: usize, row: u16| RainHalo {
        row,
        x: (col * cw) as u16,
        y: (row as usize * ch) as u16,
        w: (2 * cw) as u16,
        h: ch as u16,
        color: 0x0030_3038,
        cx: (col * cw + cw) as u16,
        cy: (row as usize * ch + ch / 2) as u16,
        rx: cw as u16,
        ry: (ch / 2).max(1) as u16,
        mode: HaloMode::Over,
    };

    // Frame A: veil at P1 (col 2, row 1) — primes both caches.
    let mut in_a = base_input.clone();
    in_a.glow_halo.push(veil_at(2, 1));
    let _ = cpu.render_input_cached(&mut win_cpu, &in_a);
    let _ = gpu.render_input_cached(&mut win_gpu, &in_a);

    // Frame B: MOVE the veil to P2 (col 9, row 4) — a genuine content change.
    let mut in_b = base_input.clone();
    in_b.glow_halo.push(veil_at(9, 4));
    let misses_before = gpu.gate_misses();
    let cpu_b = cpu
        .render_input_cached(&mut win_cpu, &in_b)
        .pixels()
        .to_vec();
    let gpu_b = gpu
        .render_input_cached(&mut win_gpu, &in_b)
        .pixels()
        .to_vec();
    assert!(
        gpu.gate_misses() > misses_before,
        "a moved Over veil must MISS the GPU dirty gate (real re-render)"
    );

    // Ground truth: no ghost smoke at the vacated P1 rows, no missing veil at
    // P2 (cached == fresh) ...
    let cpu_fresh = cpu.render_input(&in_b).pixels.clone();
    assert_eq!(
        cpu_b, cpu_fresh,
        "CPU cached-damaged Over frame must equal a fresh full render"
    );
    assert_ne!(
        cpu_b,
        cpu.render_input(&base_input).pixels,
        "the moved veil must actually darken the white ground (non-vacuous)"
    );
    // ... and byte-exact across backends over the glyph-free background.
    let delta = max_channel_delta(&cpu_b, &gpu_b);
    eprintln!("damaged-path Over veil CPU vs GPU max per-channel delta = {delta}");
    if gpu.additive_is_byte_exact() {
        assert_eq!(
            delta, 0,
            "Over veil via the cached path must be BYTE-EXACT CPU==GPU (got {delta})"
        );
    } else {
        eprintln!("SKIP damaged-path byte-exact Over gate: downlevel sRGB offscreen");
    }
}

/// An emptied stream is byte-identical on the GPU: a populated `glow_halo`
/// must paint, and `clear_overlays` must restore the bare frame — the
/// introspection-capture (`image plain`) contract, GPU side.
#[test]
fn glow_halo_disabled_bytes_identical_on_gpu() {
    let theme = Theme::default();
    let Some((cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (6usize, 20usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l$ embers off");
    let (cw, ch) = cpu.cell_size();

    let base_input = term.cell_frame(rows, cols);
    assert!(base_input.glow_halo.is_empty());
    let base = gpu.render_input(&mut win, &base_input, None).pixels;

    let mut cleared = term.cell_frame(rows, cols);
    cleared.glow_halo.push(RainHalo {
        row: 2,
        x: (3 * cw) as u16,
        y: (2 * ch) as u16,
        w: (2 * cw) as u16,
        h: ch as u16,
        color: 0x0080_FF80,
        cx: (4 * cw) as u16,
        cy: (2 * ch + ch / 2) as u16,
        rx: cw as u16,
        ry: (ch / 2).max(1) as u16,
        mode: HaloMode::Add,
    });
    let painted = gpu.render_input(&mut win, &cleared, None).pixels;
    assert_ne!(
        base, painted,
        "a live glow_halo frame must paint on the GPU"
    );
    cleared.clear_overlays();
    assert!(cleared.glow_halo.is_empty(), "clear_overlays must strip it");
    let stripped = gpu.render_input(&mut win, &cleared, None).pixels;
    assert_eq!(
        base, stripped,
        "clear_overlays must restore the bare GPU frame (glow_halo IS bling)"
    );
}
