// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The knock-out on both backends: a body drawn under the text (`FreeZ::UnderText`) is cut to
// the page under each glyph that lies on it and two pixels round the ink
// (`aterm_render::KNOCK_DILATE_OFFSETS`), the CPU in its composite and the GPU on its
// glyph-knock stream, before any trail draws. Judged on the GPU frame per ink pixel against
// the nearest ring of pixels the glyphs leave at the page, as the CPU's
// `companion_knock_out` judges its own; the two frames stay close. Negative control: the
// knock-out off leaves the letters under 4.5:1 against a tan coat.
//
// No GPU or no font: COULD NOT RUN.

use std::sync::Arc;

use aterm_core::render::{FreeSampler, FreeSprite, FreeZ, RenderInput, SceneAtlas};
use aterm_core::terminal::Terminal;
use aterm_render::Theme;

use crate::common::{backends, max_channel_delta};

const ROWS: usize = 4;
const COLS: usize = 24;

fn lin(c: u32) -> f64 {
    let s = f64::from(c) / 255.0;
    if s <= 0.040_45 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

fn rel_luma(p: u32) -> f64 {
    0.2126 * lin((p >> 16) & 0xff) + 0.7152 * lin((p >> 8) & 0xff) + 0.0722 * lin(p & 0xff)
}

fn contrast(a: u32, b: u32) -> f64 {
    let (la, lb) = (rel_luma(a), rel_luma(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// `(ink px judged, ink px under 4.5:1)`: each pixel of `clean` reading 4.5:1 or more on the
/// page, inside `bx`, against the median of its nearest ring (Chebyshev 1, else 2) of pixels
/// `clean` leaves at the page colour.
fn judge(clean: &[u32], frame: &[u32], width: usize, paper: u32, bx: [usize; 4]) -> (usize, usize) {
    let height = clean.len() / width;
    let (mut ink, mut under) = (0, 0);
    for y in bx[1]..bx[3].min(height) {
        for x in bx[0]..bx[2].min(width) {
            let i = y * width + x;
            if contrast(clean[i] & 0x00ff_ffff, paper) < 4.5 {
                continue;
            }
            let ring = |d: i64| -> Vec<u32> {
                let mut g = Vec::new();
                for dy in -d..=d {
                    for dx in -d..=d {
                        let (xx, yy) = (x as i64 + dx, y as i64 + dy);
                        if dx.abs().max(dy.abs()) != d
                            || xx < 0
                            || yy < 0
                            || xx >= width as i64
                            || yy >= height as i64
                        {
                            continue;
                        }
                        let j = yy as usize * width + xx as usize;
                        if clean[j] & 0x00ff_ffff == paper {
                            g.push(frame[j] & 0x00ff_ffff);
                        }
                    }
                }
                g
            };
            let mut ground = ring(1);
            if ground.is_empty() {
                ground = ring(2);
            }
            if ground.is_empty() {
                continue;
            }
            ground.sort_by(|a, b| rel_luma(*a).total_cmp(&rel_luma(*b)));
            ink += 1;
            if contrast(frame[i] & 0x00ff_ffff, ground[ground.len() / 2]) < 4.5 {
                under += 1;
            }
        }
    }
    (ink, under)
}

/// Letters on a tan body read against the page on the GPU as on the CPU; with the knock-out
/// off the GPU leaves them under the bar.
#[test]
fn both_backends_knock_the_body_out_under_its_letters() {
    let (mut cpu, mut gpu) = backends(18.0, Theme::default());
    let mut win = aterm_gpu::WindowGpu::new();
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"\x1b]10;#d0d0d0\x07\x1b]11;#111318\x07\x1b[?25l\r\nthe kitty flies");
    let (cw, ch) = cpu.cell_size();
    let clean = cpu
        .render_input(&term.cell_frame(ROWS, COLS))
        .pixels
        .clone();
    let width = cpu.render_input(&term.cell_frame(ROWS, COLS)).width;
    let paper = 0x0011_1318u32;
    assert_eq!(
        clean[0] & 0x00ff_ffff,
        paper,
        "the page colour must reach the frame"
    );

    let (bx0, by0, bw, bh) = (4 * cw, ch / 2, 5 * cw, 2 * ch);
    let mut input: RenderInput = term.cell_frame(ROWS, COLS);
    input.free_atlas = Some(Arc::new(SceneAtlas {
        width: bw as u32,
        height: bh as u32,
        rgba: (0..bw * bh).flat_map(|_| [0xAA, 0x95, 0x68, 255]).collect(),
        version: 1,
    }));
    input.free_sprites = vec![FreeSprite {
        x: bx0 as i32,
        y: by0 as i32,
        w: bw as u16,
        h: bh as u16,
        ax: 0,
        ay: 0,
        aw: bw as u16,
        ah: bh as u16,
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
        z: FreeZ::UnderText,
        sampler: FreeSampler::Nearest,
    }];
    let bx = [bx0, by0, bx0 + bw, by0 + bh];

    let cpu_on = cpu.render_input(&input).pixels.clone();
    let gpu_on = gpu.render_input(&mut win, &input, None).pixels;
    let (cpu_ink, cpu_under) = judge(&clean, &cpu_on, width, paper, bx);
    let (gpu_ink, gpu_under) = judge(&clean, &gpu_on, width, paper, bx);
    let delta = max_channel_delta(&cpu_on, &gpu_on);
    eprintln!(
        "knock-out: CPU {cpu_under}/{cpu_ink} ink px under 4.5, GPU {gpu_under}/{gpu_ink}, \
         CPU-vs-GPU max delta {delta}"
    );
    assert!(
        gpu_ink > 40,
        "the body must lie on the letters ({gpu_ink} ink px)"
    );
    assert_eq!(cpu_under, 0, "CPU: letters on the body under 4.5:1");
    assert_eq!(gpu_under, 0, "GPU: letters on the body under 4.5:1");
    // A loose bound: the GPU blends in linear light, the CPU in its own linear table.
    assert!(
        delta <= 24,
        "CPU and GPU knock-outs must stay visually close (max per-channel delta {delta})"
    );

    gpu.set_knock_out(false);
    let mut win_off = aterm_gpu::WindowGpu::new();
    let gpu_off = gpu.render_input(&mut win_off, &input, None).pixels;
    gpu.set_knock_out(true);
    let (off_ink, off_under) = judge(&clean, &gpu_off, width, paper, bx);
    assert!(
        off_under * 2 > off_ink,
        "negative control: with the knock-out off only {off_under} of {off_ink} GPU ink px \
         read under 4.5:1"
    );
}

/// On glass the knock-out lays the page's colour, never its opacity, on both backends: at
/// background opacity 0.8 a body box over the letters whose texels are all transparent leaves
/// the frame byte-equal with and without the knock-out (an opaque stamp drew a solid
/// glyph-shaped outline round each letter on the glass), and with a tan coat no pixel is made
/// more opaque than the body left it, the letters still reading against the page's colour.
#[test]
fn on_a_translucent_window_both_backends_keep_the_glass() {
    let (mut cpu, mut gpu) = backends(18.0, Theme::default());
    cpu.set_background_opacity(0.8);
    gpu.set_background_opacity(0.8);
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"\x1b]10;#d0d0d0\x07\x1b]11;#111318\x07\x1b[?25l\r\nthe kitty flies");
    let (cw, ch) = cpu.cell_size();
    let (bx0, by0, bw, bh) = (4 * cw, ch / 2, 5 * cw, 2 * ch);
    let sprite = FreeSprite {
        x: bx0 as i32,
        y: by0 as i32,
        w: bw as u16,
        h: bh as u16,
        ax: 0,
        ay: 0,
        aw: bw as u16,
        ah: bh as u16,
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
        z: FreeZ::UnderText,
        sampler: FreeSampler::Nearest,
    };
    let mut with = |texel: [u8; 4]| -> RenderInput {
        let mut input: RenderInput = term.cell_frame(ROWS, COLS);
        input.free_atlas = Some(Arc::new(SceneAtlas {
            width: bw as u32,
            height: bh as u32,
            rgba: (0..bw * bh).flat_map(|_| texel).collect(),
            version: 1,
        }));
        input.free_sprites = vec![sprite];
        input
    };
    let air = with([0, 0, 0, 0]);
    let fur = with([0xAA, 0x95, 0x68, 255]);
    for name in ["CPU", "GPU"] {
        let mut frame = |input: &RenderInput, knock: bool| -> Vec<u32> {
            if name == "CPU" {
                cpu.set_knock_out(knock);
                cpu.render_input(input).pixels.clone()
            } else {
                gpu.set_knock_out(knock);
                let mut win = aterm_gpu::WindowGpu::new();
                gpu.render_input(&mut win, input, None).pixels
            }
        };
        let (on, off) = (frame(&air, true), frame(&air, false));
        assert!(
            off[0] >> 24 > 0,
            "{name}: fixture: the page is glass ({:08x})",
            off[0]
        );
        let changed = on.iter().zip(&off).filter(|(a, b)| a != b).count();
        assert_eq!(
            changed, 0,
            "{name}: an all-transparent body box: the knock-out changed {changed} px of the glass"
        );
        let (on, off) = (frame(&fur, true), frame(&fur, false));
        let opaquer = on
            .iter()
            .zip(&off)
            .filter(|(a, b)| (**a >> 24) + 1 < (**b >> 24))
            .count();
        assert_eq!(
            opaquer, 0,
            "{name}: the knock-out made {opaquer} px more opaque than the body left them"
        );
        let recoloured = on
            .iter()
            .zip(&off)
            .filter(|(a, b)| (**a & 0x00FF_FFFF) != (**b & 0x00FF_FFFF))
            .count();
        assert!(
            recoloured > 40,
            "{name}: the knock-out cut only {recoloured} px of the coat to the page"
        );
    }
    cpu.set_knock_out(true);
    gpu.set_knock_out(true);
}
