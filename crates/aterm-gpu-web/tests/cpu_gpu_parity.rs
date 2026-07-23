// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates
//
// Web CPU↔GPU parity: the two web renderers must agree on pixels.
//
// `aterm-wasm` rasterizes the grid on the CPU (`aterm_render::Renderer`) and
// `aterm-gpu-web` rasterizes it on the GPU (`aterm_gpu::GpuRenderer`, WebGL2 in
// the browser). Both feed the SAME engine grid and BOTH hand JS an RGBA8 buffer
// for `putImageData` — so a divergence between them is a visible rendering bug in
// one of the two web paths.
//
// The browser GPU path needs a real `<canvas>`/WebGL surface (wasm-only), so it
// can't run here. Instead this test reproduces aterm-gpu-web's EXACT native
// construction — `GpuContext::new()` + a `Renderer::from_bytes` face handed to
// `GpuRenderer::from_parts` (lib.rs `init()`), the same shape as the wasm path —
// and compares it to aterm-wasm's CPU renderer (`Renderer::from_bytes`) at the
// same px/theme. The comparison is on the EXPANDED RGBA8 each web crate emits
// (`render()` packs `0x00RRGGBB` -> `r,g,b,0xff`), not the internal packed frame,
// so it gates the actual bytes that reach the canvas.
//
// Both sides inject the SAME bundled font, exactly as the web crates inject a
// host-fetched font, so the test is deterministic and never skips for a missing
// system face. Gated: no GPU -> the test no-ops (like aterm-gpu's own parity
// tests).

use aterm_core::terminal::Terminal;
use aterm_gpu::{GpuContext, GpuRenderer, WindowGpu};
use aterm_render::{Frame, Renderer, Theme};

/// The bundled deterministic monospace face, injected into BOTH renderers the way
/// the web crates inject a font fetched in JS — so parity can't drift on a missing
/// or mismatched system font.
const FONT: &[u8] = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");

const PX: f32 = 18.0;

/// A web theme in the `0x00RRGGBB` shape aterm-wasm/aterm-gpu-web seed from JS.
fn web_theme() -> Theme {
    Theme {
        fg: 0x00E0_E0E0,
        bg: 0x001E_1E2E,
        cursor: 0x00FF_FFFF,
        selection: 0x0030_4060,
    }
}

fn rr(p: u32) -> i32 {
    ((p >> 16) & 0xff) as i32
}
fn gg(p: u32) -> i32 {
    ((p >> 8) & 0xff) as i32
}
fn bb(p: u32) -> i32 {
    (p & 0xff) as i32
}

/// Expand a packed-`0x00RRGGBB` frame to RGBA8 EXACTLY as the web crates' `render`
/// does (`r,g,b,0xff`) — this is the buffer handed to canvas `putImageData`.
fn to_rgba8(f: &Frame) -> Vec<u8> {
    let mut v = Vec::with_capacity(f.pixels.len() * 4);
    for &p in &f.pixels {
        v.push((p >> 16) as u8);
        v.push((p >> 8) as u8);
        v.push(p as u8);
        v.push(0xff);
    }
    v
}

fn max_byte_delta(a: &[u8], b: &[u8]) -> i32 {
    a.iter()
        .zip(b)
        .map(|(&x, &y)| (x as i32 - y as i32).abs())
        .max()
        .unwrap_or(0)
}

/// ASCII + colour + reverse-video grid (no ligature sequences, no font fallback —
/// both faces use default shaping over the bundled face, so any delta is pure
/// CPU/GPU rounding).
fn demo_term() -> (Terminal, usize, usize) {
    let (rows, cols) = (6usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(
        b"\x1b[31mRR\x1b[0m\r\n\
          \x1b[44m  \x1b[0m\r\n\
          \x1b[7mXX\x1b[0m\r\n\
          ab\r\n",
    );
    (term, rows, cols)
}

#[test]
fn web_cpu_gpu_rgba8_parity() {
    let theme = web_theme();

    // GPU side, built EXACTLY as aterm-gpu-web::init does: a GpuContext + a
    // from_bytes CPU face handed to from_parts. Gate: no GPU -> skip cleanly.
    let ctx = match GpuContext::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIP: no GPU: {e}");
            return;
        }
    };
    let gpu_face = Renderer::from_bytes(FONT, PX, theme).expect("bundled font loads (gpu face)");
    let mut gpu = match GpuRenderer::from_parts(ctx, gpu_face, None, theme) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: gpu renderer unavailable: {e}");
            return;
        }
    };

    // CPU side, built as aterm-wasm does: from_bytes at the same px/theme.
    let mut cpu = Renderer::from_bytes(FONT, PX, theme).expect("bundled font loads (cpu face)");

    let mut win = WindowGpu::new();
    let (mut term, rows, cols) = demo_term();
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    // `None`: no settings-card tray in this CPU/GPU parity test (P3 `render_input`
    // gained the `tray: Option<TrayQuad>` arg; the parity check renders bare frames).
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "web CPU/GPU frame dimensions differ"
    );

    let cpu_rgba = to_rgba8(&cpu_frame);
    let gpu_rgba = to_rgba8(&gpu_frame);
    assert_eq!(
        cpu_rgba.len(),
        gpu_rgba.len(),
        "web CPU/GPU RGBA8 buffer lengths differ"
    );
    assert_eq!(
        cpu_rgba.len(),
        cpu_frame.width * cpu_frame.height * 4,
        "RGBA8 buffer is not width*height*4"
    );

    // The whole frame matches within the established CPU/GPU antialiasing tolerance
    // (only round-vs-floor coverage rounding differs); alpha is 0xff on both.
    let delta = max_byte_delta(&cpu_rgba, &gpu_rgba);
    eprintln!("web CPU/GPU RGBA8 max byte delta = {delta}");
    assert!(
        delta <= 8,
        "web CPU/GPU RGBA8 diverge: max byte delta {delta} > 8"
    );

    // Non-empty sanity: the red 'R' glyph actually rendered on the GPU path, so we
    // didn't just compare two background frames.
    let red_seen = gpu_frame
        .pixels
        .iter()
        .any(|&p| rr(p) > 140 && gg(p) < 90 && bb(p) < 90);
    assert!(
        red_seen,
        "expected red glyph pixels on the GPU frame (non-empty render)"
    );
}
