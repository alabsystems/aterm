// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared support kit for the GPU parity suites — the channel extractors, the
//! delta comparators, and the CPU+GPU backend constructors that the parity,
//! fuzz and blit test binaries used to carry as private copies. Each consuming
//! test declares `mod common;` (the standard integration-test share;
//! `rain_common` is the same pattern for the rain fixture).

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

/// Render until the frame STOPS CHANGING (or `SETTLE_LIMIT` renders), and return
/// the last one.
///
/// A renderer's fallback chain is parsed on a BACKGROUND thread and installed by
/// whichever later render polls it: a cell that wanted a face the chain had not
/// delivered yet is drawn `.notdef` and repainted on the epoch bump. That is the
/// shipped design, and it makes any parity comparison that renders one backend
/// MORE TIMES than the other unsound — the face can land BETWEEN the two arms,
/// and then the frames differ for a reason that is not parity.
///
/// Measured 2026-09-13: `linear_mode_matches_cpu_and_keeps_procedural_exact`
/// renders the CPU twice (a corrected frame, then the linear one) and the GPU
/// once. Before the font seal mapped its faces the CJK chain was still parsing
/// when the test ended, so BOTH arms were missing the ideographs and matched; as
/// soon as mapping made that parse fast enough to land between the arms, the CPU
/// arm drew 日本 and the GPU arm did not — 398 pixels over tolerance, worst delta
/// 229, in a test that was reading a scheduling race as a pixel divergence.
///
/// Settling both arms to a fixed point costs nothing — the tolerance is
/// unchanged and a real divergence still fails — but it does NOT remove that
/// race, and the sentence that used to stand here saying it did was wrong. A
/// chain that has not arrived yet is ALREADY a fixed point: every render draws
/// the same `.notdef`, so the loop returns on its second render, happily, from
/// the pre-arrival state. `linear_mode_matches_cpu_and_keeps_procedural_exact`
/// kept failing 2 runs in 6 with these calls in place. Settling answers "has
/// this backend stopped changing"; only [`font_settled`] — which `backends` now
/// applies to every pair — answers "has the font arrived".
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

/// How many extra renders a backend gets to reach a fixed point. The chain is a
/// handful of faces and each render of the demo grid is sub-millisecond, so this
/// is generous by two orders of magnitude; a backend that never settles is a
/// real defect and the caller's assertion says so.
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

/// Construct the CPU and GPU renderers under test, or skip (None, with the
/// reason on stderr) when the host has no usable GPU or system font.
///
/// Both come back FONT-SETTLED: [`font_settled`] blocks on the background
/// fallback-chain parses before the pair is handed over, so no test can render
/// through a chain that is still arriving. See [`font_settled`] for why that
/// belongs here and not in each test.
pub fn backends(px: f32, theme: Theme) -> Option<(Renderer, aterm_gpu::GpuRenderer)> {
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
    Some(font_settled(cpu, gpu))
}

/// Block both renderers on their lazy fallback-chain parses, so every later
/// render routes glyphs through a FINAL chain.
///
/// The chain is parsed on a background thread and installed by whichever later
/// render polls it, which makes "has the CJK face landed yet?" a function of
/// machine load and of what else the test binary did first — an order- and
/// contention-dependence, not a property of the renderers. When the arrival
/// falls BETWEEN a parity test's two arms, the CPU arm draws 日本 and the GPU
/// arm draws `.notdef`: measured on 2026-09-15 as
/// `linear_mode_matches_cpu_and_keeps_procedural_exact` failing 2 runs in 6 at
/// clean main with a max per-channel delta of 229 against a tolerance of 8.
///
/// [`settled_cpu`] cannot substitute. It renders to a FIXED POINT, and a chain
/// that has not arrived yet is already a fixed point — every render draws the
/// same `.notdef` — so settling returns happily from the pre-arrival state and
/// the race is untouched. Settling answers "has this backend stopped changing";
/// only blocking answers "has the font arrived".
///
/// It is done HERE, in the one constructor every parity suite calls, rather than
/// per test: of the 35 tests in `gpu_matches_cpu.rs` that build a pair, 31
/// remembered to block and four did not — which is exactly the shape of defect a
/// shared constructor is for, and the count is worse across the other parity
/// binaries. `Renderer::ensure_fallback` TAKES its path list when it spawns, so
/// the parse happens at most once: after this the pair is settled for good and no
/// later render can re-open the window.
pub fn font_settled(
    mut cpu: Renderer,
    mut gpu: aterm_gpu::GpuRenderer,
) -> (Renderer, aterm_gpu::GpuRenderer) {
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();
    (cpu, gpu)
}

/// [`backends`] for the ADDITIVE parity suites (glow, nova): forces the
/// deterministic fontdue rasterizer and disables the GPU-only bloom + shimmer
/// layers so the differential covers exactly the shared, proven base path.
pub fn backends_fontdue(px: f32, theme: Theme) -> Option<(Renderer, aterm_gpu::GpuRenderer)> {
    // Byte-exact GPU==CPU compositing parity is defined against the DETERMINISTIC
    // fontdue rasterizer. The macOS-default CoreText rasterizer produces CRISP,
    // natively-hinted glyph edges; the inherent sub-pixel difference between the CPU
    // direct blit and the GPU atlas NEAREST-sample (≤8 on its own, as gpu_matches_cpu
    // shows) lands a crisp edge at e.g. coverage ~0.2 vs ~0.8, and the One/One
    // ADDITIVE glow over it then amplifies that into a full-channel divergence (one
    // side clips to white, the other stays glow-coloured). fontdue's soft AA edges
    // absorb the same sub-pixel offset, so the additive parity these tests check is
    // only meaningful on the deterministic rasterizer (which is exactly what fontdue
    // is "the path for tests" for). `call_once` blocks every caller until the var is
    // set, so it is in place before either renderer below is constructed (no
    // set_var/getenv race).
    static FORCE_FONTDUE: std::sync::Once = std::sync::Once::new();
    FORCE_FONTDUE.call_once(|| {
        // Set once, before any renderer in this test binary is built; every
        // additive-parity test wants the same deterministic value. Routed through
        // the workspace's one lock-scoped env helper.
        aterm_log::env::set("ATERM_RASTERIZER", "fontdue");
    });
    let mut gpu = match aterm_gpu::GpuRenderer::new(px, theme) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return None;
        }
    };
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

// ---------------------------------------------------------------------------
// Byte-exact CPU==GPU parity, as pinned on Apple silicon's GPUs, and what an
// x86_64 macOS run does with it.
//
// Six sites in five tests — `over_ink_fire_byte_exact_over_dark_and_light_frames`,
// `over_veils_byte_exact_over_dark_and_light_frames`,
// `damaged_path_over_veil_parity_cpu_matches_gpu`,
// `source_over_glow_under_is_byte_exact_and_leaves_the_additive_half_alone`
// and `ribbon_beam_v_train_is_byte_exact_cpu_vs_gpu` (its full-frame train and
// its damaged-path train) — demand a per-channel delta of 0 between the CPU
// frame and the GPU frame of the SOURCE-OVER streams. The shader's argument
// for that (`renderer.rs`, `fs_fire_over` and `fs_rain_glow_over`: "one
// rounding on store, no exact ties") was measured to hold on the GPUs that
// pinned it, Apple silicon's. On a 2017 15-inch MacBook Pro (macOS 13.7.8)
// BOTH of its GPUs store the same source-over one LSB off on some pixels,
// measured 2026-09-16 with the Trust toolchain on the tree this kit landed
// in: on the AMD Radeon Pro 560 (`ATERM_GPU_POWER=high`) every one of the
// six rows above misses with a max per-channel delta of exactly 1; on the
// Intel HD Graphics 630 (the default low-power pick) the rows that miss vary
// from run to run — the fire Over dark frame in one run, the fire light
// frame, both veil frames and the full-frame ribbon train in another — each
// by exactly 1, with fixed phases and no clock in any of these tests. The
// additive streams' ten exact sites in the same files (a raw 8-bit add
// through the One/One Unorm view) pass on both GPUs in every run. No cause
// beyond those GPUs' blend rounding is claimed: the same line prints for any
// one-LSB miss here, and it names the adapter so a run says which GPU it saw.
//
// So everywhere but x86_64 macOS [`assert_byte_exact`] IS the assertion those
// tests always made — `assert_eq!(delta, 0, message)`, same message, same
// failure — written as a pair, the `not(all(target_arch = "x86_64",
// target_os = "macos"))` arm first so that is readable at a glance. x86_64
// Linux and Windows keep the exact assertion: nothing was measured there. On
// x86_64 macOS a one-LSB miss prints one stderr line naming the adapter and
// fails nothing, and the delta must still sit inside the one-LSB rounding
// bound this crate states for a float store (`blit_invert.rs`'s "<= 1 LSB"
// floor, `cat_parity.rs`'s target): a delta of 2 or more is the failure it is
// everywhere. Downlevel (GLES/WebGL2) hosts are unaffected — every caller
// still gates on `GpuRenderer::additive_is_byte_exact` first.
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
