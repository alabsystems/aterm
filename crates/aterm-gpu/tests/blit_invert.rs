// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Present-path blit: the fullscreen-triangle blit (`vs_blit` + `fs_blit`, NEAREST) that copies
// the offscreen `Rgba8Unorm` frame into the swapchain. With `BlitUniform.flag != 0` (visual-bell
// flash) it inverts RGB, the GPU counterpart of the CPU `px ^ 0x00ffffff`; the unflagged 1:1 copy
// is pinned by `blit_bands.rs`. The swapchain is not readable headless, so the same pipeline,
// shader, sampler and uniform run against a readable target (`blit_to_offscreen_for_test`);
// `present()`, WSI and scanout are not exercised. No GPU or no font: COULD NOT RUN.

use std::sync::Arc;

use aterm_core::render::{FreeSampler, FreeSprite, FreeZ};
use aterm_core::terminal::Terminal;
use aterm_gpu::GpuRenderer;
use aterm_render::{Frame, RenderInput, SceneAtlas, Theme};

use crate::common::{self, bb, gg, rr};

const ROWS: usize = 6;
const COLS: usize = 24;

/// The blit seams read the wgpu offscreen, so this suite drives the wgpu oracle arm.
fn fresh_gpu() -> GpuRenderer {
    common::wgpu_oracle(18.0, Theme::default())
}

/// A prompt, SGR red/green/blue text and glyphs, so the blit runs over real pixels.
fn representative_input() -> RenderInput {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"$ blit check >_\r\n");
    term.process(b"\x1b[31mRED\x1b[0m \x1b[32mGREEN\x1b[0m \x1b[34mBLUE\x1b[0m\r\n");
    term.process(b"\x1b[1mbold\x1b[0m plain 0123456789");
    term.cell_frame(ROWS, COLS)
}

/// A ramp that visits every 8-bit value in every channel: `i` for red and odd strides (coprime
/// with 256) for green and blue.
fn full_domain_ramp(w: usize, h: usize) -> Vec<u32> {
    (0..w * h)
        .map(|i| {
            let r = (i % 256) as u32;
            let g = ((i * 5) % 256) as u32; // gcd(5, 256) == 1
            let b = ((i * 3 + 128) % 256) as u32; // gcd(3, 256) == 1
            (r << 16) | (g << 8) | b
        })
        .collect()
}

/// Put exact `0x00RRGGBB` bytes into the renderer's offscreen and return the readback,
/// asserting the injection was exact: one opaque, untinted NEAREST 1:1 free sprite covering the
/// frame, drawn `OverText` over a blank cursor-hidden grid. The caller disables bloom and
/// shimmer.
fn inject_offscreen(
    gpu: &mut GpuRenderer,
    win: &mut aterm_gpu::WindowGpu,
    pixels: &[u32],
    w: usize,
    h: usize,
) -> Frame {
    let mut rgba = Vec::with_capacity(w * h * 4);
    for &p in pixels {
        rgba.extend_from_slice(&[rr(p) as u8, gg(p) as u8, bb(p) as u8, 255]);
    }
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"\x1b[?25l"); // no cursor pixels over the injected art
    let mut input = term.cell_frame(ROWS, COLS);
    input.free_atlas = Some(Arc::new(SceneAtlas {
        width: w as u32,
        height: h as u32,
        rgba,
        version: 1,
    }));
    input.free_sprites.push(FreeSprite {
        x: 0,
        y: 0,
        w: w as u16,
        h: h as u16,
        ax: 0,
        ay: 0,
        aw: w as u16, // bake == dest: the NEAREST 1:1 contract
        ah: h as u16,
        tint: 0x00FF_FFFF, // no tint
        alpha: 255,        // fully opaque: src-over is a replacement
        flip_x: false,
        z: FreeZ::OverText,
        sampler: FreeSampler::Nearest,
    });
    let got = gpu.render_input(win, &input, None);
    assert_eq!(
        (got.width, got.height),
        (w, h),
        "injection frame size changed under us"
    );
    for (i, (&want, &have)) in pixels.iter().zip(got.pixels.iter()).enumerate() {
        assert_eq!(
            have & 0x00ff_ffff,
            want,
            "PREMISE: the 1:1 opaque free sprite must inject bytes verbatim — \
             pixel {i} wanted {want:#08x}, offscreen has {have:#08x}"
        );
    }
    got
}

/// Literal double invert over the whole 8-bit domain: the second leg blits the first leg's
/// output bytes (re-injected), so `invert(invert(x)) == x` is performed, not composed, and
/// `255 - x` is checked at all 256 values in all three channels.
#[test]
fn blit_double_invert_round_trips_the_real_output() {
    let mut gpu = fresh_gpu();
    // Bloom and shimmer both write extra light into the offscreen.
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    let mut win = aterm_gpu::WindowGpu::new();

    // Size the ramp to the renderer's own frame (one throwaway render).
    let probe = gpu.render_input(&mut win, &representative_input(), None);
    let (w, h) = (probe.width, probe.height);

    // Leg 1: ramp -> offscreen, blit invert.
    let ramp = full_domain_ramp(w, h);
    let src = inject_offscreen(&mut gpu, &mut win, &ramp, w, h);
    let inv1 = gpu.blit_to_offscreen_for_test(&mut win, true);

    // Non-vacuity: every 8-bit value is present in every channel of the source readback.
    let mut seen = [[false; 256]; 3];
    for &p in &src.pixels {
        seen[0][rr(p) as usize] = true;
        seen[1][gg(p) as usize] = true;
        seen[2][bb(p) as usize] = true;
    }
    for (c, name) in ["red", "green", "blue"].iter().enumerate() {
        let missing = (0..256).filter(|&v| !seen[c][v]).count();
        assert_eq!(
            missing, 0,
            "the injected ramp must cover all 256 {name} values ({missing} missing)"
        );
    }

    for (i, (&s, &o)) in src.pixels.iter().zip(inv1.pixels.iter()).enumerate() {
        for (sc, oc) in [(rr(s), rr(o)), (gg(s), gg(o)), (bb(s), bb(o))] {
            assert_eq!(
                oc,
                255 - sc,
                "domain-complete invert: pixel {i} channel {sc} must invert to {} (got {oc})",
                255 - sc
            );
        }
    }

    // Leg 2: blit the first blit's output.
    let inv1_px: Vec<u32> = inv1.pixels.iter().map(|p| p & 0x00ff_ffff).collect();
    let _ = inject_offscreen(&mut gpu, &mut win, &inv1_px, w, h);
    let inv2 = gpu.blit_to_offscreen_for_test(&mut win, true);

    // Name the first pixel that failed to come home rather than dumping both frames.
    if let Some((i, (&want, &have))) = src
        .pixels
        .iter()
        .zip(inv2.pixels.iter())
        .enumerate()
        .find(|&(_, (&a, &b))| a != b)
    {
        let (x, y) = (i % w, i / w);
        panic!(
            "LITERAL DOUBLE INVERT BROKEN: blitting the invert's own output with \
             invert on must return the original frame byte-for-byte — first \
             mismatch at pixel {i} (x={x}, y={y}): wanted {want:#08x}, got {have:#08x}"
        );
    }
    // Non-vacuity: a blit that did nothing would pass the identity above.
    assert_ne!(
        inv1.pixels, src.pixels,
        "NON-VACUITY: the first invert must actually change the frame"
    );
    eprintln!(
        "literal double invert: {} pixels round-tripped byte-for-byte over the full 8-bit domain",
        src.pixels.len()
    );
}
