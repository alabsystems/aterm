// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared support for the GPU parity suites.

#![allow(dead_code)] // each test binary uses its own subset of the kit

pub mod sprites;

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

/// Render until the frame stops changing (at most `SETTLE_LIMIT` extra renders) and return the last one.
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

/// Checked-in JetBrains Mono fixture path.
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
    aterm_gpu::GpuRenderer::from_parts(ctx, cpu_on(font, px, theme), Some(font.into()), theme)
        .expect("the GPU renderer assembles on an acquired device")
}

/// Move `g` onto the wgpu oracle arm.
#[cfg(wgpu_arm)]
pub fn disarm_for_oracle(g: &mut aterm_gpu::GpuRenderer) {
    #[cfg(target_os = "macos")]
    g.disarm_metal_for_oracle();
    #[cfg(target_os = "linux")]
    g.disarm_gl_for_oracle();
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let _ = g;
}

/// The arms a parity test runs on, each a fresh [`backends`] pair at
/// `px`/`theme`: the native arm (Metal on macOS, GL on Linux) and, in the
/// oracle build on macOS or Linux, the wgpu oracle.
pub fn arms(px: f32, theme: Theme) -> Vec<(&'static str, Renderer, aterm_gpu::GpuRenderer)> {
    let (cpu, gpu) = backends(px, theme);
    #[allow(unused_mut, reason = "the oracle build adds an arm")]
    let mut out = vec![("native", cpu, gpu)];
    #[cfg(all(wgpu_arm, any(target_os = "macos", target_os = "linux")))]
    {
        let (cpu, mut gpu) = backends(px, theme);
        disarm_for_oracle(&mut gpu);
        out.push(("wgpu-oracle", cpu, gpu));
    }
    out
}

/// That `gpu`'s last frame drew on the arm `arm` names (the oracle is
/// neither Metal nor GL).
#[track_caller]
pub fn assert_arm(arm: &str, gpu: &aterm_gpu::GpuRenderer) {
    #[cfg(target_os = "macos")]
    assert_eq!(
        gpu.metal_render_armed(),
        arm == "native",
        "{arm}: the frame's arm"
    );
    #[cfg(target_os = "linux")]
    assert_eq!(
        gpu.last_frame_on_gl(),
        arm == "native",
        "{arm}: the frame's arm"
    );
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let _ = (arm, gpu);
}

/// End the calling test as could-not-run unless this host ships a colour-emoji face.
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

/// The CPU and GPU renderers under test, both on [`FIXTURE_FONT`] and font-settled.
pub fn backends(px: f32, theme: Theme) -> (Renderer, aterm_gpu::GpuRenderer) {
    backends_with_font(std::path::Path::new(FIXTURE_FONT), px, theme)
}

/// A CPU renderer at `px`/`theme` on [`FIXTURE_FONT`].
pub fn cpu(px: f32, theme: Theme) -> Renderer {
    cpu_on(FIXTURE_FONT, px, theme)
}

/// A GPU renderer at `px`/`theme` on [`FIXTURE_FONT`].
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

/// [`backends`], with both renderers built on the font file at `font`.
pub fn backends_with_font(
    font: &std::path::Path,
    px: f32,
    theme: Theme,
) -> (Renderer, aterm_gpu::GpuRenderer) {
    let family = font.to_str().expect("a UTF-8 font path");
    let gpu = gpu_on(family, px, theme);
    font_settled(cpu_on(family, px, theme), gpu)
}

/// Block both renderers on their lazy fallback-chain parses.
pub fn font_settled(
    mut cpu: Renderer,
    mut gpu: aterm_gpu::GpuRenderer,
) -> (Renderer, aterm_gpu::GpuRenderer) {
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();
    (cpu, gpu)
}

/// A second, independent GPU renderer, font-settled like [`backends`].
pub fn independent_gpu(px: f32, theme: Theme) -> aterm_gpu::GpuRenderer {
    let mut gpu = gpu(px, theme);
    gpu.debug_block_on_lazy_fallbacks();
    gpu
}

/// [`backends`] for the additive parity suites using the fontdue rasterizer.
pub fn backends_fontdue(px: f32, theme: Theme) -> (Renderer, aterm_gpu::GpuRenderer) {
    static FORCE_FONTDUE: std::sync::Once = std::sync::Once::new();
    FORCE_FONTDUE.call_once(|| {
        aterm_log::env::set("ATERM_RASTERIZER", "fontdue");
    });
    let mut gpu = gpu(px, theme);
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    font_settled(cpu_on(FIXTURE_FONT, px, theme), gpu)
}

/// Hidden-cursor `rows`×`cols` terminal whose rows `block_rows` carry 30 full blocks.
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

/// Assert cached-path no-ghosting and dirty-gate behaviour across three frames.
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

    let hits_before = gpu.gate_hits();
    let _ = gpu.render_input_cached(&mut win_gpu, c);
    assert!(
        gpu.gate_hits() > hits_before,
        "a settled {settled} must take the dirty gate"
    );
}

/// Assert `delta == 0`, or at most `WAIVED_LSB` where [`waived_on`] applies.
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

/// Gate for byte-exact comparisons of one CPU-vs-GPU parity test.
#[derive(Debug, Default)]
pub struct ByteExact {
    deferred: std::cell::RefCell<Vec<String>>,
}

impl ByteExact {
    /// Whether `gpu` composites byte-exact with the CPU, deferring `what` when not.
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
        if !deferred.is_empty() && !std::thread::panicking() {
            aterm_spec::host::could_not_run(format!(
                "{}: this adapter renders to a downlevel (non-sRGB) offscreen, where the add \
                 lands in linear, so CPU==GPU cannot be byte-exact here; every other check passed",
                deferred.join(", ")
            ));
        }
    }
}

/// End the calling test COULD NOT RUN unless `gpu` composites byte-exact with the CPU.
pub fn byte_exact_or_could_not_run(gpu: &aterm_gpu::GpuRenderer, what: &str) {
    if !gpu.additive_is_byte_exact() {
        aterm_spec::host::could_not_run(format!(
            "{what}: this adapter renders to a downlevel (non-sRGB) offscreen, where the add \
             lands in linear, so CPU==GPU cannot be byte-exact here"
        ));
    }
}
