// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! Web CPU/GPU RGBA8 rendering parity test.

use aterm_core::terminal::Terminal;
use aterm_gpu::{GpuContext, GpuRenderer, WindowGpu};
use aterm_render::{Frame, Renderer, Theme};

const FONT: &[u8] = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");

const PX: f32 = 18.0;

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

    let ctx = match GpuContext::new() {
        Ok(c) => c,
        Err(e) => aterm_spec::host::could_not_run(format!("no GPU: {e}")),
    };
    let gpu_face = Renderer::from_bytes(FONT, PX, theme).expect("bundled font loads (gpu face)");
    let mut gpu = match GpuRenderer::from_parts(ctx, gpu_face, None, theme) {
        Ok(g) => g,
        Err(e) => aterm_spec::host::could_not_run(format!("gpu renderer unavailable: {e}")),
    };

    let mut cpu = Renderer::from_bytes(FONT, PX, theme).expect("bundled font loads (cpu face)");

    let mut win = WindowGpu::new();
    let (mut term, rows, cols) = demo_term();
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
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

    let delta = max_byte_delta(&cpu_rgba, &gpu_rgba);
    eprintln!("web CPU/GPU RGBA8 max byte delta = {delta}");
    assert!(
        delta <= 8,
        "web CPU/GPU RGBA8 diverge: max byte delta {delta} > 8"
    );

    let red_seen = gpu_frame
        .pixels
        .iter()
        .any(|&p| rr(p) > 140 && gg(p) < 90 && bb(p) < 90);
    assert!(
        red_seen,
        "expected red glyph pixels on the GPU frame (non-empty render)"
    );
}
