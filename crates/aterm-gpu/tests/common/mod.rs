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

/// Render until the frame stops changing (at most `SETTLE_LIMIT` extra renders) and return the
/// last one. It does not remove the fallback-face race (a chain that has not arrived yet is
/// already a fixed point, drawing `.notdef`); [`font_settled`], applied by `backends`, does.
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

/// The face every renderer here is built on: the checked-in JetBrains Mono
/// fixture, so a frame is the same on every host and no suite depends on the
/// machine's fonts. Passed as the family, the window's `--font` seam.
pub const FIXTURE_FONT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../aterm-render/tests/fixtures/jetbrains-mono.ttf"
);

/// A GPU renderer at `px`/`theme` on `font`. A host with no usable GPU could
/// not run the test (`aterm_spec::host::could_not_run`), which is no pass;
/// past the device, a failure is the code's.
fn gpu_on(font: &str, px: f32, theme: Theme) -> aterm_gpu::GpuRenderer {
    let ctx = aterm_gpu::GpuContext::new()
        .unwrap_or_else(|e| aterm_spec::host::could_not_run(format!("no GPU: {e}")));
    // On Linux the renderer draws, presents and reads back through the
    // first-party GL arm (docs/AUDIT-third-party-2026-10-15.md GW.4), as on
    // macOS through Metal: both are armed at construction.
    aterm_gpu::GpuRenderer::from_parts(ctx, cpu_on(font, px, theme), Some(font.into()), theme)
        .expect("the GPU renderer assembles on an acquired device")
}

/// Move `g` onto the wgpu oracle arm: on macOS and Linux a renderer draws on
/// its first-party arm (Metal, GL), so a suite that reads the wgpu offscreen,
/// drives wgpu-typed seams (blits, stand-in textures, taps, the allocator) or
/// checks the WGSL that ships on Windows and wasm asks for the oracle. A no-op
/// where wgpu is the renderer. Only a build carrying the oracle arm (the
/// `--features wgpu-oracle` lane on macOS and Linux) has it.
#[cfg(wgpu_arm)]
pub fn disarm_for_oracle(g: &mut aterm_gpu::GpuRenderer) {
    #[cfg(target_os = "macos")]
    g.disarm_metal_for_oracle();
    #[cfg(target_os = "linux")]
    g.disarm_gl_for_oracle();
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let _ = g;
}

/// End the calling test as could-not-run unless this host ships a
/// colour-emoji face: every macOS and Windows does, a Linux host when Noto
/// Color Emoji is installed. Read from the files, never from a frame, so a
/// renderer that lost its colour glyphs fails rather than blaming the host.
pub fn require_host_colour_emoji() {
    let noto = "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf";
    if !cfg!(any(target_os = "macos", windows)) && !std::path::Path::new(noto).is_file() {
        aterm_spec::host::could_not_run(format!("{noto} is not installed here"));
    }
}

/// The CPU renderer at `px`/`theme` on `font`.
fn cpu_on(font: &str, px: f32, theme: Theme) -> Renderer {
    Renderer::from_system_with_family(Some(font), px, theme)
        .unwrap_or_else(|| panic!("{font} does not load"))
}

/// The CPU and GPU renderers under test, both on [`FIXTURE_FONT`] and
/// font-settled ([`font_settled`]).
pub fn backends(px: f32, theme: Theme) -> (Renderer, aterm_gpu::GpuRenderer) {
    backends_with_font(std::path::Path::new(FIXTURE_FONT), px, theme)
}

/// A CPU renderer at `px`/`theme` on [`FIXTURE_FONT`], for a suite that builds
/// its own oracle beside a pair.
pub fn cpu(px: f32, theme: Theme) -> Renderer {
    cpu_on(FIXTURE_FONT, px, theme)
}

/// A GPU renderer at `px`/`theme` on [`FIXTURE_FONT`]. Not font-settled: a
/// suite that compares it against another renderer settles it itself (or uses
/// [`backends`]).
pub fn gpu(px: f32, theme: Theme) -> aterm_gpu::GpuRenderer {
    gpu_on(FIXTURE_FONT, px, theme)
}

/// [`gpu`] on the wgpu oracle arm ([`disarm_for_oracle`]).
#[cfg(wgpu_arm)]
pub fn wgpu_oracle(px: f32, theme: Theme) -> aterm_gpu::GpuRenderer {
    let mut g = gpu(px, theme);
    disarm_for_oracle(&mut g);
    g
}

/// [`backends`], with both renderers built on the font file at `font`, passed as the family
/// (the window's `--font` / `font_family` seam, `from_system_with_family`), so no environment
/// variable is involved.
pub fn backends_with_font(
    font: &std::path::Path,
    px: f32,
    theme: Theme,
) -> (Renderer, aterm_gpu::GpuRenderer) {
    let family = font.to_str().expect("a UTF-8 font path");
    let gpu = gpu_on(family, px, theme);
    font_settled(cpu_on(family, px, theme), gpu)
}

/// Block both renderers on their lazy fallback-chain parses, so later renders use a final chain
/// and the CJK face cannot land between a test's two arms (CPU draws 日本, GPU `.notdef`).
/// `Renderer::ensure_fallback` takes its path list when it spawns, so the parse happens once.
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
    let mut gpu = gpu(px, theme);
    gpu.debug_block_on_lazy_fallbacks();
    gpu
}

/// [`backends`] for the additive parity suites (glow, nova): forces the
/// deterministic fontdue rasterizer and turns off the GPU-only bloom and
/// shimmer, so the comparison covers the shared base path.
pub fn backends_fontdue(px: f32, theme: Theme) -> (Renderer, aterm_gpu::GpuRenderer) {
    // Byte-exact additive parity needs fontdue: CoreText's crisp edges turn the ≤8 CPU/GPU
    // sub-pixel difference into a full-channel divergence under a One/One glow, while fontdue's
    // soft edges absorb it. Set once, before any renderer in this binary is built.
    static FORCE_FONTDUE: std::sync::Once = std::sync::Once::new();
    FORCE_FONTDUE.call_once(|| {
        aterm_log::env::set("ATERM_RASTERIZER", "fontdue");
    });
    let mut gpu = gpu(px, theme);
    // Bloom (GPU-only, additive on top of the base; see the `bloom_*` tests) and heat shimmer
    // (wall-clock; `heat_shimmer.rs`) are off, so the comparison covers the shared base path.
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    // Font-settled for the same reason [`backends`] is.
    font_settled(cpu_on(FIXTURE_FONT, px, theme), gpu)
}

/// The byte-exact text ground the overlay-stream parity suites composite over: a hidden-cursor
/// `rows`×`cols` terminal whose rows `block_rows` each carry 30 procedural full blocks (█).
/// Blocks render identically on both backends, so a stream's delta over this base is its own.
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

/// The cached-path no-ghosting + dirty-gate pin the sprite suites make, on both backends: frame
/// `a` primes the caches; `b` moves the sprite (the GPU gate must miss, and each backend's
/// cached repaint must equal a fresh full render, so the vacated bands re-cleared); `c` repeats
/// `b` and must take the GPU dirty gate. `moved` and `settled` name the sprite in failures.
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

    // Settled: byte-equal sprites + same atlas version ⇒ the GPU gate hits.
    let hits_before = gpu.gate_hits();
    let _ = gpu.render_input_cached(&mut win_gpu, c);
    assert!(
        gpu.gate_hits() > hits_before,
        "a settled {settled} must take the dirty gate"
    );
}

// Byte-exact CPU==GPU parity for the source-over streams: where it is pinned, and where a
// one-LSB miss is waived.
//
// Several tests require delta 0 between CPU and GPU frames of the source-over streams
// (`fs_fire_over`, `fs_rain_glow_over`, the source-over bed, the ribbon train). That holds on
// Apple-silicon GPUs; the Intel and AMD GPUs of x86_64 Macs store some source-over pixels one
// LSB off (the additive streams stay exact), and so does Windows: an AMD Radeon 780M under
// DX12 missed by one in the fire and veil Over, the ribbon train and the gradient bed
// (2026-10-17, its additive streams exact). The fixed-function blend's precision is the
// API's to choose, and Windows has no Apple GPU.
//
// Mesa's software rasterizer on Linux, llvmpipe for GL and lavapipe for Vulkan (both name
// their device `llvmpipe`), misses by one in the fire and veil Over too: on the GL arm, and on
// lavapipe through the rows' `_cpu_vs_wgpu_oracle` twins, on aarch64 and x86_64 (the Linux
// lane, Mesa 25.2.8, 2026-10-19). Probed on llvmpipe's GLES, it blends a unorm8 target in
// 8-bit fixed point, rounding each product on its own as `(t + (t >> 8) + 0x80) >> 8` with
// `t = x·y`, then adding with saturation. Every one of the 16,777,216 (src, dst, alpha) cases
// of a `SRC_ALPHA, ONE_MINUS_SRC_ALPHA` blend matched that law, and 4,121,116 of them sit one
// below or above the single rounding the CPU's `over_rgb` takes, none further
// (`docs/measured/linux-gpu-rows-2026-10-19.md`). So [`assert_byte_exact`] reports a
// one-LSB miss on stderr and passes on x86_64 macOS, on Windows and on an `llvmpipe`
// adapter, and 2 or more still fails; everywhere else (Apple GPUs, and any Linux GPU, none
// of which has run here) it asserts delta 0. Callers still gate on
// `GpuRenderer::additive_is_byte_exact` first.

/// `assert_eq!(delta, 0, "{message}")`, except where [`waived_on`] names the
/// host or adapter: there a one-LSB miss prints its report line and fails
/// nothing, and a larger miss fails with the caller's message.
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
    // On Linux a renderer draws on the GL arm, armed at construction, even
    // where the context is the oracle's wgpu one; a test that disarmed it
    // ([`disarm_for_oracle`]) drew on wgpu, and the label names which arm drew.
    #[cfg(target_os = "linux")]
    let backend = if gpu.last_frame_on_gl() {
        "the GL arm"
    } else {
        backend
    };
    let Some(waived) = waived_on(adapter) else {
        assert_eq!(delta, 0, "{message}");
        return;
    };
    // The stderr handle rather than `eprintln!`, which libtest captures and
    // drops for a passing test: a waived pin must show in every run.
    let line = format!(
        "{test}: {what}: CPU vs GPU max per-channel delta {delta} on {adapter} ({backend}) — not \
         asserted byte-exact on {waived}: pinned on Apple GPUs, and this GPU's source-over \
         store need not round to the same last bit (tests/common/mod.rs records what was \
         measured where)\n"
    );
    let _ = std::io::stderr().write_all(line.as_bytes());
    assert!(
        delta <= WAIVED_LSB,
        "{message} — and {delta} is past the one-LSB rounding bound {waived} is allowed on \
         {adapter} ({backend})"
    );
}

/// The one LSB a blend unit may round off from the integer law: the most a
/// waived run allows.
const WAIVED_LSB: i32 = 1;

/// Where a one-LSB source-over miss is waived, as the report line names it:
/// x86_64 macOS and Windows by host, Mesa's software rasterizer by adapter.
fn waived_on(adapter: &str) -> Option<&'static str> {
    if cfg!(all(target_arch = "x86_64", target_os = "macos")) {
        Some("x86_64 macOS")
    } else if cfg!(windows) {
        Some("Windows")
    } else if cfg!(target_os = "linux") && adapter.starts_with("llvmpipe") {
        Some("Mesa's llvmpipe and lavapipe")
    } else {
        None
    }
}

/// The byte-exact comparisons of one CPU-vs-GPU parity test.
///
/// The byte-exact comparison holds only where the offscreen is sRGB
/// ([`aterm_gpu::GpuRenderer::additive_is_byte_exact`]): on a downlevel adapter
/// the additive and blended streams land in linear. Every other property a
/// test checks (gate misses, non-vacuity, drained frames, sizes) holds on any
/// adapter. So a test asks [`ByteExact::holds`] before each byte-exact
/// assertion and runs everything else; on a downlevel adapter the comparisons
/// it could not make are remembered, and when the test would otherwise pass it
/// ends COULD NOT RUN naming them (when the gate drops), never a pass with a
/// SKIP printed.
#[derive(Debug, Default)]
pub struct ByteExact {
    deferred: std::cell::RefCell<Vec<String>>,
}

impl ByteExact {
    /// Whether `gpu` composites byte-exact with the CPU, so the caller asserts
    /// the comparison `what`. When it does not, `what` is deferred to the
    /// COULD NOT RUN this gate ends the test with.
    pub fn holds(&self, gpu: &aterm_gpu::GpuRenderer, what: &str) -> bool {
        let exact = gpu.additive_is_byte_exact();
        if !exact {
            self.deferred.borrow_mut().push(what.to_owned());
        }
        exact
    }
}

impl Drop for ByteExact {
    fn drop(&mut self) {
        let deferred = self.deferred.get_mut();
        // A test already failing keeps its own failure.
        if !deferred.is_empty() && !std::thread::panicking() {
            aterm_spec::host::could_not_run(format!(
                "{}: this adapter renders to a downlevel (non-sRGB) offscreen, where the add \
                 lands in linear, so CPU==GPU cannot be byte-exact here; every other check passed",
                deferred.join(", ")
            ));
        }
    }
}

/// End the calling test COULD NOT RUN unless `gpu` composites byte-exact with
/// the CPU (an sRGB offscreen); `what` names the comparison it cannot make.
pub fn byte_exact_or_could_not_run(gpu: &aterm_gpu::GpuRenderer, what: &str) {
    if !gpu.additive_is_byte_exact() {
        aterm_spec::host::could_not_run(format!(
            "{what}: this adapter renders to a downlevel (non-sRGB) offscreen, where the add \
             lands in linear, so CPU==GPU cannot be byte-exact here"
        ));
    }
}
