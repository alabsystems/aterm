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
    Some((cpu, gpu))
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
    Some((cpu, gpu))
}
