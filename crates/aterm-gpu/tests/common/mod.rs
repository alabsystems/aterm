// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared support for the GPU parity suites: channel extractors, delta
//! comparators, and the CPU+GPU backend constructors.

#![allow(dead_code)] // each test binary uses its own subset of the kit

use aterm_render::{Frame, Renderer, Theme};

pub fn rr(p: u32) -> i32 {
    ((p >> 16) & 0xff) as i32
}
pub fn gg(p: u32) -> i32 {
    ((p >> 8) & 0xff) as i32
}
pub fn bb(p: u32) -> i32 {
    (p & 0xff) as i32
}

/// Render until the frame stops changing (or `SETTLE_LIMIT` renders) and return
/// the last one.
///
/// Fallback faces are parsed in the background and installed by a later render,
/// so a test rendering one backend more times than the other can see a face land
/// between the arms. Settling reaches a fixed point, but a chain that has not
/// arrived yet is already one (every render draws `.notdef`), so it does not
/// remove that race; [`font_settled`], applied by `backends`, does.
pub fn settled_cpu(cpu: &mut Renderer, input: &aterm_render::RenderInput) -> Frame {
    let mut last = cpu.render_input(input);
    for _ in 0..SETTLE_LIMIT {
        let next = cpu.render_input(input);
        if next.pixels == last.pixels {
            return next;
        }
        last = next;
    }
    last
}

/// [`settled_cpu`] for the GPU backend.
pub fn settled_gpu(
    gpu: &mut aterm_gpu::GpuRenderer,
    win: &mut aterm_gpu::WindowGpu,
    input: &aterm_render::RenderInput,
) -> Frame {
    let mut last = gpu.render_input(win, input, None);
    for _ in 0..SETTLE_LIMIT {
        let next = gpu.render_input(win, input, None);
        if next.pixels == last.pixels {
            return next;
        }
        last = next;
    }
    last
}

/// Extra renders allowed to reach a fixed point; generous, so a backend that
/// never settles is a real defect.
const SETTLE_LIMIT: usize = 64;

/// Largest per-channel absolute delta between two pixel buffers.
pub fn max_channel_delta(a: &[u32], b: &[u32]) -> i32 {
    let mut m = 0;
    for (&pa, &pb) in a.iter().zip(b.iter()) {
        m = m.max((rr(pa) - rr(pb)).abs());
        m = m.max((gg(pa) - gg(pb)).abs());
        m = m.max((bb(pa) - bb(pb)).abs());
    }
    m
}

/// [`max_channel_delta`] over whole [`Frame`]s.
pub fn max_channel_delta_frame(a: &Frame, b: &Frame) -> i32 {
    max_channel_delta(&a.pixels, &b.pixels)
}

/// Number of pixels whose worst channel delta exceeds `tol`.
pub fn count_exceeding_frame(a: &Frame, b: &Frame, tol: i32) -> usize {
    let mut n = 0;
    for (&pa, &pb) in a.pixels.iter().zip(b.pixels.iter()) {
        let mut d = 0;
        d = d.max((rr(pa) - rr(pb)).abs());
        d = d.max((gg(pa) - gg(pb)).abs());
        d = d.max((bb(pa) - bb(pb)).abs());
        if d > tol {
            n += 1;
        }
    }
    n
}

/// Construct the CPU and GPU renderers under test, or skip (`None`, reason on
/// stderr) without a usable GPU or system font. Both are font-settled
/// ([`font_settled`]).
pub fn backends(px: f32, theme: Theme) -> Option<(Renderer, aterm_gpu::GpuRenderer)> {
    let gpu = gpu_or_skip(px, theme)?;
    let Some(cpu) = Renderer::from_system(px, theme) else {
        eprintln!("SKIP: no system monospace font");
        return None;
    };
    Some(font_settled(cpu, gpu))
}

/// A GPU renderer at `px`/`theme`, or skip (None, with the reason on stderr)
/// when the host has no usable GPU or font. Not font-settled: a suite that
/// compares it against another renderer settles it itself (or uses
/// [`backends`]).
pub fn gpu_or_skip(px: f32, theme: Theme) -> Option<aterm_gpu::GpuRenderer> {
    aterm_gpu::GpuRenderer::new(px, theme)
        .map_err(|e| eprintln!("SKIP: no GPU renderer: {e}"))
        .ok()
}

/// [`gpu_or_skip`] on the wgpu oracle arm. On macOS a renderer presents
/// through Metal, so a suite that reads the wgpu offscreen or drives wgpu-typed
/// seams (blits, stand-in textures, taps, the allocator) asks for the oracle.
pub fn wgpu_oracle_or_skip(px: f32, theme: Theme) -> Option<aterm_gpu::GpuRenderer> {
    let g = gpu_or_skip(px, theme)?;
    #[cfg(target_os = "macos")]
    let g = {
        let mut g = g;
        g.disarm_metal_for_oracle();
        g
    };
    Some(g)
}

/// [`backends`], with BOTH renderers built on the font file at `font` — passed as
/// the family, exactly the window's `--font` / `font_family` seam
/// (`from_system_with_family`), so no environment variable is involved.
pub fn backends_with_font(
    font: &std::path::Path,
    px: f32,
    theme: Theme,
) -> Option<(Renderer, aterm_gpu::GpuRenderer)> {
    let family = font.to_str()?;
    let gpu = match aterm_gpu::GpuRenderer::new_with_family(Some(family), px, theme) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU renderer: {e}");
            return None;
        }
    };
    let Some(cpu) = Renderer::from_system_with_family(Some(family), px, theme) else {
        eprintln!("SKIP: {family} does not load");
        return None;
    };
    Some(font_settled(cpu, gpu))
}

/// Block both renderers on their lazy fallback-chain parses, so later renders
/// use a final chain.
///
/// Otherwise the CJK face can land between a test's two arms (CPU draws 日本,
/// GPU draws `.notdef`) depending on machine load. [`settled_cpu`] cannot
/// replace this (see there). Done in the shared constructor so no test can
/// forget it. `Renderer::ensure_fallback` takes its path list when it spawns,
/// so the parse happens once and the pair stays settled.
pub fn font_settled(
    mut cpu: Renderer,
    mut gpu: aterm_gpu::GpuRenderer,
) -> (Renderer, aterm_gpu::GpuRenderer) {
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();
    (cpu, gpu)
}

/// A second, independent GPU renderer for a test that already holds a pair,
/// font-settled like the pair.
pub fn independent_gpu(px: f32, theme: Theme) -> aterm_gpu::GpuRenderer {
    let mut gpu = aterm_gpu::GpuRenderer::new(px, theme).expect("GPU was available above");
    gpu.debug_block_on_lazy_fallbacks();
    gpu
}

/// [`backends`] for the additive parity suites (glow, nova): forces the
/// deterministic fontdue rasterizer and turns off the GPU-only bloom and
/// shimmer, so the comparison covers the shared base path.
pub fn backends_fontdue(px: f32, theme: Theme) -> Option<(Renderer, aterm_gpu::GpuRenderer)> {
    // Byte-exact additive parity needs fontdue: CoreText's crisp edges turn the
    // ≤8 CPU/GPU sub-pixel difference into a full-channel divergence under a
    // One/One glow, while fontdue's soft edges absorb it. `call_once` sets the
    // variable before either renderer is constructed.
    static FORCE_FONTDUE: std::sync::Once = std::sync::Once::new();
    FORCE_FONTDUE.call_once(|| {
        // Set once before any renderer in this binary is built, via the
        // workspace's lock-scoped env helper.
        aterm_log::env::set("ATERM_RASTERIZER", "fontdue");
    });
    let mut gpu = gpu_or_skip(px, theme)?;
    // These tests prove the byte-parity-critical BASE render (the crisp glow quads,
    // CPU == GPU). The GPU-only bloom is a deliberate additive layer ON TOP of that
    // base, verified separately (see the `bloom_*` tests). Disable it here so the
    // differential comparison covers exactly the shared, proven path. The heat
    // shimmer is the same parity class (and wall-clock at present) — off too;
    // it is verified separately in `heat_shimmer.rs` with a pinned phase.
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    let cpu = match Renderer::from_system(px, theme) {
        Some(c) => c,
        None => {
            eprintln!("SKIP: no system monospace font");
            return None;
        }
    };
    // Font-settled for the same reason [`backends`] is.
    Some(font_settled(cpu, gpu))
}

/// The byte-exact TEXT ground the overlay-stream parity suites composite over:
/// a hidden-cursor `rows`×`cols` terminal whose rows `block_rows` each carry 30
/// procedural full blocks (█). Blocks render identically on both backends
/// (delta 0), so a stream's measured delta over this base is the effect's alone.
pub fn block_text_term(
    rows: usize,
    cols: usize,
    block_rows: std::ops::Range<usize>,
) -> aterm_core::terminal::Terminal {
    let mut term = aterm_core::terminal::Terminal::new(rows as u16, cols as u16);
    term.process("\x1b[?25l".as_bytes());
    for r in block_rows {
        term.process(format!("\x1b[{};1H{}", r + 1, "█".repeat(30)).as_bytes());
    }
    term
}

/// The damaged/cached-path no-ghosting + dirty-gate pin the sprite suites make,
/// on BOTH backends: frame `a` primes the caches; frame `b` MOVES the sprite (a
/// real change — the GPU gate must MISS, and each backend's cached repaint must
/// equal a fresh full render byte-for-byte, so the vacated bands re-cleared);
/// frame `c` repeats `b` (settled) and must take the GPU dirty gate. `px` and
/// `theme` are the pair's; `moved` and `settled` name the sprite in the failure
/// messages.
pub fn assert_moved_then_settled(
    (cpu, gpu): (&mut Renderer, &mut aterm_gpu::GpuRenderer),
    (px, theme): (f32, Theme),
    [a, b, c]: [&aterm_render::RenderInput; 3],
    moved: &str,
    settled: &str,
) {
    let mut win_cpu = aterm_render::WindowCpu::new();
    let mut win_gpu = aterm_gpu::WindowGpu::new();
    let _ = cpu.render_input_cached(&mut win_cpu, a);
    let _ = gpu.render_input_cached(&mut win_gpu, a);

    let misses_before = gpu.gate_misses();
    let cpu_b_cached = cpu.render_input_cached(&mut win_cpu, b).pixels().to_vec();
    let gpu_b_cached = gpu.render_input_cached(&mut win_gpu, b).pixels().to_vec();
    assert!(
        gpu.gate_misses() > misses_before,
        "a moved {moved} must MISS the GPU dirty gate (real re-render)"
    );

    // Fresh ground truths (throwaway caches / a fresh GPU renderer).
    let cpu_b_fresh = cpu.render_input(b).pixels;
    assert_eq!(
        cpu_b_cached, cpu_b_fresh,
        "CPU damaged path must repaint the moved {moved} with no ghosting"
    );
    let mut gpu2 = independent_gpu(px, theme);
    let mut win2 = aterm_gpu::WindowGpu::new();
    let gpu_b_fresh = gpu2.render_input(&mut win2, b, None).pixels;
    assert_eq!(
        gpu_b_cached, gpu_b_fresh,
        "GPU dirty-row path must repaint the moved {moved} with no ghosting"
    );

    // Settled: byte-equal sprites + same atlas version ⇒ the GPU gate HITS.
    let hits_before = gpu.gate_hits();
    let _ = gpu.render_input_cached(&mut win_gpu, c);
    assert!(
        gpu.gate_hits() > hits_before,
        "a settled {settled} must take the dirty gate"
    );
}

// ---------------------------------------------------------------------------
// Byte-exact CPU==GPU parity for the source-over streams, and x86_64 macOS.
//
// Several tests require a delta of 0 between CPU and GPU frames of the
// source-over streams (`fs_fire_over`, `fs_rain_glow_over`, the source-over
// bed, the ribbon train). That holds on Apple-silicon GPUs. The Intel and AMD
// GPUs of x86_64 Macs store some source-over pixels one LSB off (the additive
// streams stay exact).
//
// So [`assert_byte_exact`] is `assert_eq!(delta, 0, message)` everywhere except
// x86_64 macOS, where a one-LSB miss prints a stderr line naming the adapter
// and passes, and a delta of 2 or more still fails. x86_64 Linux and Windows
// keep the exact check. Callers still gate on
// `GpuRenderer::additive_is_byte_exact` first.
// ---------------------------------------------------------------------------

/// `assert_eq!(delta, 0, "{message}")` — the assertion the caller made before
/// this helper existed, unchanged, on every target but x86_64 macOS.
#[cfg(not(all(target_arch = "x86_64", target_os = "macos")))]
#[track_caller]
pub fn assert_byte_exact(
    _test: &str,
    _what: &str,
    _gpu: &aterm_gpu::GpuRenderer,
    delta: i32,
    message: std::fmt::Arguments<'_>,
) {
    assert_eq!(delta, 0, "{message}");
}

/// x86_64 macOS: a one-LSB miss prints its report line and fails nothing; a
/// larger miss fails with the caller's message, as it does everywhere.
#[cfg(all(target_arch = "x86_64", target_os = "macos"))]
#[track_caller]
pub fn assert_byte_exact(
    test: &str,
    what: &str,
    gpu: &aterm_gpu::GpuRenderer,
    delta: i32,
    message: std::fmt::Arguments<'_>,
) {
    use std::io::Write as _;
    if delta == 0 {
        return;
    }
    let (adapter, backend) = gpu.adapter();
    // The stderr handle rather than `eprintln!`, which libtest captures and
    // drops for a passing test: a waived pin must show in every run.
    let line = format!(
        "{test}: {what}: CPU vs GPU max per-channel delta {delta} on {adapter} ({backend}) — not \
         asserted byte-exact on x86_64 macOS: pinned on Apple GPUs, and this GPU's source-over \
         store need not round to the same last bit (tests/common/mod.rs records what was \
         measured where)\n"
    );
    let _ = std::io::stderr().write_all(line.as_bytes());
    assert!(
        delta <= X86_64_MACOS_LSB,
        "{message} — and {delta} is past the one-LSB rounding bound x86_64 macOS is allowed on \
         {adapter} ({backend})"
    );
}

/// The one LSB a float store may round off from the integer law — the floor
/// `blit_invert.rs` states — and the most an x86_64 macOS run waives.
#[cfg(all(target_arch = "x86_64", target_os = "macos"))]
const X86_64_MACOS_LSB: i32 = 1;
