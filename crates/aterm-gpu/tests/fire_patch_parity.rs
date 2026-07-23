// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// FIRE-PATCH (`RenderInput.fire_patch`) CPU/GPU byte parity — the EMBERFORGE
// per-pixel procedural fire field, plumbed end-to-end. Every covered device
// pixel evaluates the shared pure-integer field (`aterm_render::fire_field`
// on the CPU; the op-for-op `fs_fire_add`/`fs_fire_over` WGSL twins on the
// GPU), so the stream inherits the STRONGEST bar in the harness: BYTE-EXACT
// CPU==GPU (delta 0), the `glow_halo`/`rain_add` contract at full art scale.
//
// Covered:
//   * base frames WITHOUT fire are byte-exact CPU==GPU (delta 0), so the
//     measured fire delta is effect-only;
//   * a SYNTHETIC burn field over block text — multiple temps, strengths,
//     leans, cov_caps, multi-row patches, edge-clipped quads — is BYTE-EXACT,
//     across font sizes including 4K-class cells and multiple phases;
//   * `FireMode::Over` (the light-theme ink-fire): a mixed Add+Over field is
//     BYTE-EXACT over dark AND white frames, and on white the ink visibly
//     DARKENS the ground (the smoke-on-light-theme law, fire edition);
//   * the damaged/cached path: an ANIMATING burn (advancing phase) that also
//     moves rows must MISS the GPU dirty gate, repaint the prev∪cur patch
//     rows (the `fire_patch_changed` discipline), and land byte-exact with
//     no ghost at the vacated rows;
//   * the SEAM LAW: one wide patch vs the same burn split into many narrow
//     patches renders BYTE-IDENTICAL frames on each backend (the field is a
//     pure function of absolute coordinates — zero seams);
//   * the NO-OP LAW: `clear_overlays` restores the bare frame byte-exactly;
//   * DETERMINISM: the same input renders the same bytes twice.
//
// Gated: no GPU or no font -> the tests no-op (return), like the other parity
// gates. Byte-exact CPU==GPU gates additionally skip on downlevel
// (sRGB-offscreen) adapters via `additive_is_byte_exact`, the glow idiom.
// Backend-internal laws (seam, no-op, determinism) hold on ANY adapter and
// are asserted unconditionally.

use aterm_core::terminal::Terminal;
use aterm_render::{FireMode, FirePatch, Renderer, Theme, WindowCpu};

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

/// Summed-RGB luminance proxy (monotone per channel, so ordering is exact).
fn luma(p: u32) -> i32 {
    rr(p) + gg(p) + bb(p)
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

/// One burn's parameters (grid-interior px), turned into legal per-row-band
/// patches by [`emit_burn`].
#[derive(Clone, Copy)]
struct Burn {
    x: usize,
    w: usize,
    base_y: usize,
    peak_h: u16,
    phase: u32,
    temp: u8,
    strength: u8,
    lean: i8,
    cov_cap: u8,
    mode: FireMode,
}

/// Emit one burn as its legal per-row-band patches (the producer contract):
/// the covered rect spans the flame's full vertical reach (root down at
/// `base_y`, tongues overshooting to ~1.2·peak_h above it), clamped to the
/// grid interior, split at cell-row boundaries — each slice tagged with its
/// own `row` while SHARING the burn's field parameters, so the field is
/// continuous across the slices.
fn emit_burn(out: &mut Vec<FirePatch>, b: Burn, ch: usize, grid_w: usize, grid_h: usize) {
    let x0 = b.x.min(grid_w);
    let x1 = (b.x + b.w).min(grid_w);
    let reach = (b.peak_h as usize) * 6 / 5 + 2;
    let y0 = b.base_y.saturating_sub(reach);
    let y1 = (b.base_y + 1).min(grid_h);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let mut y = y0;
    while y < y1 {
        let row = y / ch;
        let band_end = ((row + 1) * ch).min(y1);
        out.push(FirePatch {
            row: row as u16,
            x: x0 as u16,
            y: y as u16,
            w: (x1 - x0) as u16,
            h: (band_end - y) as u16,
            base_y: b.base_y as u16,
            peak_h: b.peak_h,
            phase: b.phase,
            temp: b.temp,
            strength: b.strength,
            lean: b.lean,
            cov_cap: b.cov_cap,
            cell_h: ch as u16,
            mode: b.mode,
        });
        y = band_end;
    }
}

/// A synthetic multi-burn field exercising every FirePatch degree of freedom:
/// temps across the range, strengths, both lean signs, differing cov_caps,
/// multi-row reaches, and an edge-clipped burn.
fn synthetic_burns(phase: u32, ch: usize, grid_w: usize, grid_h: usize) -> Vec<FirePatch> {
    let mut out = Vec::new();
    let burns = [
        // A hot tall burn spanning several row bands.
        Burn {
            x: ch,
            w: 8 * ch,
            base_y: grid_h - ch / 2,
            peak_h: (3 * ch) as u16,
            phase,
            temp: 230,
            strength: 240,
            lean: -48,
            cov_cap: 200,
            mode: FireMode::Add,
        },
        // A cool ember burn, opposite lean, tight cov_cap.
        Burn {
            x: 10 * ch,
            w: 5 * ch,
            base_y: grid_h - ch,
            peak_h: (2 * ch) as u16,
            phase,
            temp: 51,
            strength: 160,
            lean: 90,
            cov_cap: 90,
            mode: FireMode::Add,
        },
        // A mid burn clipped at the right grid edge.
        Burn {
            x: grid_w.saturating_sub(3 * ch),
            w: 4 * ch,
            base_y: grid_h - 2,
            peak_h: (5 * ch / 2) as u16,
            phase: phase.wrapping_add(7777),
            temp: 128,
            strength: 255,
            lean: 0,
            cov_cap: 160,
            mode: FireMode::Add,
        },
        // A short weak burn high in the grid (top rows).
        Burn {
            x: 2 * ch,
            w: 3 * ch,
            base_y: 2 * ch - 1,
            peak_h: (3 * ch / 2) as u16,
            phase: phase.wrapping_add(191),
            temp: 180,
            strength: 90,
            lean: -120,
            cov_cap: 255,
            mode: FireMode::Add,
        },
    ];
    for b in burns {
        emit_burn(&mut out, b, ch, grid_w, grid_h);
    }
    out
}

/// THE fire-patch parity pin. The base frame is procedural full-block glyphs
/// over background — byte-exact CPU==GPU (delta 0) — so the fire delta below
/// is the stream's alone. The synthetic burn field is composited over that
/// text on both backends at MULTIPLE PHASES: the frames must be
/// BYTE-IDENTICAL (the shared-integer-field parity contract).
#[test]
fn fire_patch_synthetic_field_is_byte_exact_over_text() {
    let theme = Theme::default();
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
    let (_cw, ch) = cpu.cell_size();
    let (grid_w, grid_h) = (cols * cpu.cell_size().0, rows * ch);

    // (a) Base (no fire): the effect-only premise — CPU==GPU exactly.
    let base_input = term.cell_frame(rows, cols);
    let cpu_base = cpu.render_input(&base_input);
    let gpu_base = gpu.render_input(&mut win, &base_input, None);
    assert_eq!(
        max_channel_delta(&cpu_base.pixels, &gpu_base.pixels),
        0,
        "procedural-block base must be byte-exact so the fire delta is effect-only"
    );

    // (b) The synthetic burn field at several quantized phases (16 ms steps
    // and a large late-clock value exercising the wrapping offsets).
    for phase in [0u32, 91_750, 91_766, 1 << 20] {
        let mut input = term.cell_frame(rows, cols);
        input.fire_patch = synthetic_burns(phase, ch, grid_w, grid_h);
        // Anti-vacuity: multi-quad, multi-row field.
        let mut qrows: Vec<u16> = input.fire_patch.iter().map(|q| q.row).collect();
        qrows.sort_unstable();
        qrows.dedup();
        assert!(
            input.fire_patch.len() > qrows.len() && qrows.len() >= 4,
            "need a spanning multi-row field ({} patches on {} rows)",
            input.fire_patch.len(),
            qrows.len()
        );
        let cpu_f = cpu.render_input(&input);
        let gpu_f = gpu.render_input(&mut win, &input, None);
        assert_ne!(
            cpu_f.pixels, cpu_base.pixels,
            "the synthetic burns must actually paint (non-vacuous, phase {phase})"
        );
        let delta = max_channel_delta(&cpu_f.pixels, &gpu_f.pixels);
        eprintln!(
            "fire_patch synthetic field (phase {phase}) GPU vs CPU max per-channel delta = {delta} ({} patches)",
            input.fire_patch.len()
        );
        if gpu.additive_is_byte_exact() {
            assert_eq!(
                delta, 0,
                "fire field must be BYTE-EXACT CPU==GPU (got {delta} at phase {phase})"
            );
        } else {
            eprintln!("SKIP byte-exact fire gate: downlevel sRGB offscreen (linear add)");
        }
    }
}

/// 4K-CLASS CELLS: the same parity pin at a 44 px font (cell heights in the
/// ~50-100 px range — the field's DPI-scaled anatomy at full resolution),
/// over a glyph-free frame so the base is trivially byte-exact.
#[test]
fn fire_patch_byte_exact_at_4k_class_cells() {
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(44.0, theme) else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (6usize, 24usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("\x1b[?25l".as_bytes());
    let (cw, ch) = cpu.cell_size();
    let (grid_w, grid_h) = (cols * cw, rows * ch);
    assert!(
        ch >= 44,
        "4K-class premise: cell height {ch} should be >= 44"
    );

    let base_input = term.cell_frame(rows, cols);
    let cpu_base = cpu.render_input(&base_input);
    let gpu_base = gpu.render_input(&mut win, &base_input, None);
    assert_eq!(
        max_channel_delta(&cpu_base.pixels, &gpu_base.pixels),
        0,
        "bg-only base must be byte-exact so the fire delta is effect-only"
    );

    let mut input = term.cell_frame(rows, cols);
    input.fire_patch = synthetic_burns(123_456, ch, grid_w, grid_h);
    let cpu_f = cpu.render_input(&input);
    let gpu_f = gpu.render_input(&mut win, &input, None);
    assert_ne!(cpu_f.pixels, cpu_base.pixels, "the 4K burns must paint");
    let delta = max_channel_delta(&cpu_f.pixels, &gpu_f.pixels);
    eprintln!("fire_patch 4K-class GPU vs CPU max per-channel delta = {delta}");
    if gpu.additive_is_byte_exact() {
        assert_eq!(
            delta, 0,
            "4K-class fire field must be BYTE-EXACT CPU==GPU (got {delta})"
        );
    } else {
        eprintln!("SKIP byte-exact 4K fire gate: downlevel sRGB offscreen");
    }
}

/// A calm LIGHT theme (white ground, near-black ink) — the frame additive
/// fire light is invisible on, i.e. the frame `FireMode::Over` exists for.
fn light_theme() -> Theme {
    Theme {
        fg: 0x0020_2020,
        bg: 0x00FF_FFFF,
        ..Theme::default()
    }
}

/// THE `FireMode::Over` parity pin, over BOTH grounds: a mixed Add+Over burn
/// field (the Over ink dead over an Add ember — the overlap exercises the
/// Add-then-Over split order) over block text must be BYTE-EXACT CPU==GPU on
/// a dark frame AND on a light text-on-white frame. On the white ground the
/// ink-fire must visibly DARKEN the frame (additive fire cannot), so the
/// byte-exact gate is provably non-vacuous on the path that matters.
#[test]
fn over_ink_fire_byte_exact_over_dark_and_light_frames() {
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

        let base_input = term.cell_frame(rows, cols);
        let cpu_base = cpu.render_input(&base_input);
        let gpu_base = gpu.render_input(&mut win, &base_input, None);
        assert_eq!(
            max_channel_delta(&cpu_base.pixels, &gpu_base.pixels),
            0,
            "({label}) block base must be byte-exact so the fire delta is effect-only"
        );

        let mut patches = Vec::new();
        // An Add ember with an Over ink-burn dead on top of it (same span —
        // the Add-then-Over order is visible in every overlapped byte).
        let ember = Burn {
            x: 4 * ch,
            w: 6 * ch,
            base_y: grid_h - ch,
            peak_h: (2 * ch) as u16,
            phase: 55_555,
            temp: 200,
            strength: 220,
            lean: -40,
            cov_cap: 180,
            mode: FireMode::Add,
        };
        emit_burn(&mut patches, ember, ch, grid_w, grid_h);
        emit_burn(
            &mut patches,
            Burn {
                temp: 140,
                cov_cap: 200,
                mode: FireMode::Over,
                ..ember
            },
            ch,
            grid_w,
            grid_h,
        );
        // A pure ink burn spanning rows, clipped at the bottom-right corner.
        let ink = Burn {
            x: grid_w - 4 * ch,
            w: 5 * ch,
            base_y: grid_h - 1,
            peak_h: (3 * ch) as u16,
            phase: 88_888,
            temp: 128,
            strength: 250,
            lean: 64,
            cov_cap: 220,
            mode: FireMode::Over,
        };
        emit_burn(&mut patches, ink, ch, grid_w, grid_h);
        let n_over = patches.iter().filter(|p| p.mode == FireMode::Over).count();
        assert!(
            n_over >= 5,
            "({label}) need a multi-patch, row-spanning Over field ({n_over})"
        );

        let mut input = term.cell_frame(rows, cols);
        input.fire_patch = patches;
        let cpu_f = cpu.render_input(&input);
        let gpu_f = gpu.render_input(&mut win, &input, None);
        assert_ne!(
            cpu_f.pixels, cpu_base.pixels,
            "({label}) the burns must actually paint (non-vacuous)"
        );
        if label == "light" {
            // The law the mode exists for: ink-fire DARKENS the white ground.
            let pad = (cpu_f.width - grid_w) / 2;
            let probe_x = pad + grid_w - 2 * ch;
            let probe_y = pad + grid_h - ch / 2;
            let probe = probe_x + probe_y * cpu_f.width;
            assert!(
                luma(cpu_f.pixels[probe]) < luma(cpu_base.pixels[probe]),
                "an Over ink-burn must darken the white ground near its root"
            );
        }
        let delta = max_channel_delta(&cpu_f.pixels, &gpu_f.pixels);
        eprintln!(
            "fire Over ({label}) GPU vs CPU max per-channel delta = {delta} ({} patches)",
            input.fire_patch.len()
        );
        if gpu.additive_is_byte_exact() {
            assert_eq!(
                delta, 0,
                "({label}) ink-fire must be BYTE-EXACT CPU==GPU (got {delta})"
            );
        } else {
            eprintln!("SKIP byte-exact Over fire gate ({label}): downlevel sRGB offscreen");
        }
    }
}

/// DAMAGED/CACHED-PATH parity: the per-frame presentation hot path. Frame A
/// places a burn (priming both caches); frame B advances its PHASE by one
/// 16 ms tick AND moves it to different rows — the exact animating-producer
/// shape. The change must MISS the GPU dirty gate (`fire_patch_changed`),
/// repaint the prev∪cur patch rows, and land byte-exact with no ghost at the
/// vacated rows.
#[test]
fn damaged_path_fire_patch_parity_cpu_matches_gpu() {
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win_cpu = WindowCpu::new();
    let mut win_gpu = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (8usize, 20usize);
    let mut term = Terminal::new(rows as u16, cols as u16); // all background
    let (cw, ch) = cpu.cell_size();
    let (grid_w, grid_h) = (cols * cw, rows * ch);
    let mut base_input = term.cell_frame(rows, cols);
    base_input.cursor_visible = false; // isolate the fire: no cursor pixels

    let burn_at = |base_y: usize, phase: u32| {
        let mut v = Vec::new();
        emit_burn(
            &mut v,
            Burn {
                x: 2 * ch,
                w: 6 * ch,
                base_y,
                peak_h: (2 * ch) as u16,
                phase,
                temp: 210,
                strength: 230,
                lean: -60,
                cov_cap: 190,
                mode: FireMode::Add,
            },
            ch,
            grid_w,
            grid_h,
        );
        v
    };

    // Frame A: burn rooted near the bottom — primes both caches.
    let mut in_a = base_input.clone();
    in_a.fire_patch = burn_at(grid_h - 4, 70_000);
    let _ = cpu.render_input_cached(&mut win_cpu, &in_a);
    let _ = gpu.render_input_cached(&mut win_gpu, &in_a);

    // Frame B: the burn ANIMATES (phase +16 ≈ one 60 fps tick) and moves up
    // two rows — a genuine content change on new and vacated rows.
    let mut in_b = base_input.clone();
    in_b.fire_patch = burn_at(grid_h - 2 * ch - 4, 70_016);
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
        "an animating fire_patch must MISS the GPU dirty gate (real re-render)"
    );

    // Ground truth: the damaged frame must equal a FRESH full render (no
    // ghost flame at the vacated rows, no missing flame at the new rows) ...
    let cpu_fresh = cpu.render_input(&in_b).pixels.clone();
    assert_eq!(
        cpu_b, cpu_fresh,
        "CPU cached-damaged fire frame must equal a fresh full render"
    );
    // ... and byte-exact across backends over the glyph-free background.
    let delta = max_channel_delta(&cpu_b, &gpu_b);
    eprintln!("damaged-path fire_patch CPU vs GPU max per-channel delta = {delta}");
    if gpu.additive_is_byte_exact() {
        assert_eq!(
            delta, 0,
            "fire via the cached path must be BYTE-EXACT CPU==GPU (got {delta})"
        );
    } else {
        eprintln!("SKIP damaged-path byte-exact fire gate: downlevel sRGB offscreen");
    }
}

/// THE SEAM LAW: the field is a pure function of ABSOLUTE pixel coordinates
/// plus shared burn parameters, so ONE wide patch and the SAME burn split
/// into many narrow patches must render BYTE-IDENTICAL frames — zero seams.
/// Pinned on the CPU unconditionally and on the GPU against itself (a
/// backend-internal law needing no cross-backend gate), in both modes.
#[test]
fn seam_law_split_patches_byte_identical_to_one_wide() {
    let theme = Theme::default();
    let Some((mut cpu, gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut gpu = gpu;
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (8usize, 32usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("\x1b[?25l".as_bytes());
    let (cw, ch) = cpu.cell_size();
    let (grid_w, grid_h) = (cols * cw, rows * ch);

    for mode in [FireMode::Add, FireMode::Over] {
        let burn = Burn {
            x: ch,
            w: 20 * ch,
            base_y: grid_h - 3,
            peak_h: (3 * ch) as u16,
            phase: 424_242,
            temp: 220,
            strength: 245,
            lean: -70,
            cov_cap: 200,
            mode,
        };
        // (a) One wide patch per row band.
        let mut wide = term.cell_frame(rows, cols);
        emit_burn(&mut wide.fire_patch, burn, ch, grid_w, grid_h);
        // (b) The same burn split into 3 px-wide slivers, sharing every
        // field parameter (the producer's continuity contract).
        let mut split = term.cell_frame(rows, cols);
        let mut x = burn.x;
        while x < burn.x + burn.w {
            let w = 3.min(burn.x + burn.w - x);
            emit_burn(
                &mut split.fire_patch,
                Burn { x, w, ..burn },
                ch,
                grid_w,
                grid_h,
            );
            x += w;
        }
        assert!(
            split.fire_patch.len() > wide.fire_patch.len() * 10,
            "the split field must be genuinely fine-grained"
        );

        let cpu_wide = cpu.render_input(&wide).pixels.clone();
        let cpu_split = cpu.render_input(&split).pixels.clone();
        assert_ne!(
            cpu_wide,
            cpu.render_input(&term.cell_frame(rows, cols)).pixels,
            "({mode:?}) the wide burn must actually paint (non-vacuous)"
        );
        assert_eq!(
            cpu_wide, cpu_split,
            "({mode:?}) CPU: split patches must be byte-identical to one wide patch (zero seams)"
        );

        let gpu_wide = gpu.render_input(&mut win, &wide, None).pixels;
        let gpu_split = gpu.render_input(&mut win, &split, None).pixels;
        assert_eq!(
            gpu_wide, gpu_split,
            "({mode:?}) GPU: split patches must be byte-identical to one wide patch (zero seams)"
        );
    }
}

/// DETERMINISM: the field is pure and the pipelines are stateless — the same
/// input must render the identical bytes twice, on both backends.
#[test]
fn fire_patch_determinism_same_bytes_twice() {
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (8usize, 24usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l$ burn twice");
    let (cw, ch) = cpu.cell_size();
    let (grid_w, grid_h) = (cols * cw, rows * ch);

    let mut input = term.cell_frame(rows, cols);
    input.fire_patch = synthetic_burns(31_337, ch, grid_w, grid_h);
    let cpu_1 = cpu.render_input(&input).pixels.clone();
    let cpu_2 = cpu.render_input(&input).pixels.clone();
    assert_eq!(cpu_1, cpu_2, "CPU fire render must be deterministic");
    let gpu_1 = gpu.render_input(&mut win, &input, None).pixels;
    let gpu_2 = gpu.render_input(&mut win, &input, None).pixels;
    assert_eq!(gpu_1, gpu_2, "GPU fire render must be deterministic");
}

/// HEAD-BAND PARITY: with a chrome head band (`set_head`) AND interior padding
/// (`set_pad`) on BOTH backends, a WINDOW-ABSOLUTE burn rooted in the grid's
/// top row whose tongues overshoot ABOVE the grid — into the head band
/// (`y < pad + head`, row tag 0, the damage-hint contract) — must render
/// BYTE-EXACT CPU==GPU, and the band pixels must actually carry flame on both
/// backends (the fire really escapes the grid; row-0 tags open the top strip).
#[test]
fn fire_patch_head_band_parity_cpu_matches_gpu() {
    const P: usize = 6; // interior pad (px per edge)
    const H: usize = 20; // chrome head band (px above the padded grid)
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    cpu.set_pad(P);
    cpu.set_head(H);
    gpu.set_pad(P);
    gpu.set_head(H);
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (8usize, 24usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("\x1b[?25l".as_bytes());
    let (cw, ch) = cpu.cell_size();
    let grid_top = P + H;

    // (a) Base (no fire): the head-band framing itself is byte-exact, and the
    // frame carries the `head` extension (`rows·ch + 2·pad + head` tall).
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
        "head-band base must be byte-exact so the fire delta is effect-only"
    );

    // (b) One strong burn rooted in the grid's FIRST row, WINDOW-ABSOLUTE
    // (the producer contract): its tongues overshoot above `grid_top`. Bands
    // anchor at `grid_top`; the above-grid slice carries row tag 0.
    let base_y = grid_top + ch / 2;
    let peak_h = (2 * ch) as u16;
    let reach = (peak_h as usize) * 6 / 5 + 2;
    let (x0, x1) = (P + 2 * cw, P + 14 * cw);
    let y0 = base_y.saturating_sub(reach); // above grid_top: head-band pixels
    assert!(y0 < grid_top, "premise: the burn must reach the head band");
    let mut patches = Vec::new();
    let mut y = y0;
    while y < base_y + 1 {
        // Band split anchored at grid_top; the above-grid band tags row 0.
        let band = (y as i32 - grid_top as i32).div_euclid(ch as i32);
        let band_end = (grid_top as i32 + (band + 1) * ch as i32) as usize;
        let band_end = band_end.min(base_y + 1);
        patches.push(FirePatch {
            row: band.max(0) as u16,
            x: x0 as u16,
            y: y as u16,
            w: (x1 - x0) as u16,
            h: (band_end - y) as u16,
            base_y: base_y as u16,
            peak_h,
            phase: 70_000,
            temp: 240,
            strength: 250,
            lean: -40,
            cov_cap: 255,
            cell_h: ch as u16,
            mode: FireMode::Add,
        });
        y = band_end;
    }
    assert_eq!(patches[0].row, 0, "the above-grid slice must tag row 0");

    let mut input = term.cell_frame(rows, cols);
    input.fire_patch = patches;
    let cpu_f = cpu.render_input(&input);
    let gpu_f = gpu.render_input(&mut win, &input, None);
    assert_ne!(
        cpu_f.pixels, cpu_base.pixels,
        "the head-band burn must actually paint (non-vacuous)"
    );

    // The band really carries flame on BOTH backends: some pixel strictly
    // above the grid (`y < grid_top`) differs from that backend's base.
    let band_lit =
        |painted: &[u32], base: &[u32], w: usize| (0..grid_top * w).any(|i| painted[i] != base[i]);
    assert!(
        band_lit(&cpu_f.pixels, &cpu_base.pixels, cpu_f.width),
        "CPU: the fire must draw inside the head band (y < pad + head)"
    );
    assert!(
        band_lit(&gpu_f.pixels, &gpu_base.pixels, gpu_f.width),
        "GPU: the fire must draw inside the head band (y < pad + head)"
    );

    let delta = max_channel_delta(&cpu_f.pixels, &gpu_f.pixels);
    eprintln!("fire_patch head-band GPU vs CPU max per-channel delta = {delta}");
    if gpu.additive_is_byte_exact() {
        assert_eq!(
            delta, 0,
            "head-band fire must be BYTE-EXACT CPU==GPU (got {delta})"
        );
    } else {
        eprintln!("SKIP byte-exact head-band fire gate: downlevel sRGB offscreen");
    }
}

/// The NO-OP LAW, GPU side: a populated `fire_patch` must paint, and
/// `clear_overlays` must restore the bare frame byte-identically — the
/// introspection-capture (`image plain`) contract.
#[test]
fn fire_patch_disabled_bytes_identical_on_gpu() {
    let theme = Theme::default();
    let Some((cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (6usize, 20usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l$ embers off");
    let (cw, ch) = cpu.cell_size();
    let (grid_w, grid_h) = (cols * cw, rows * ch);

    let base_input = term.cell_frame(rows, cols);
    assert!(base_input.fire_patch.is_empty());
    let base = gpu.render_input(&mut win, &base_input, None).pixels;

    let mut cleared = term.cell_frame(rows, cols);
    cleared.fire_patch = synthetic_burns(99_999, ch, grid_w, grid_h);
    let painted = gpu.render_input(&mut win, &cleared, None).pixels;
    assert_ne!(
        base, painted,
        "a live fire_patch frame must paint on the GPU"
    );
    cleared.clear_overlays();
    assert!(
        cleared.fire_patch.is_empty(),
        "clear_overlays must strip it"
    );
    let stripped = gpu.render_input(&mut win, &cleared, None).pixels;
    assert_eq!(
        base, stripped,
        "clear_overlays must restore the bare GPU frame (fire_patch IS bling)"
    );
}
