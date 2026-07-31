// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// EMBERFORGE GLYPH CONTRAST-HALO on the GPU — both backends draw the dark warm
// dilation ring around a fire-engulfed glyph (a `fire_halo` cell in a
// `fire_patch` frame — the COLOUR-FREE strength stream; the ink itself is
// never recoloured, the no-recolor law) so the letterform stays legible inside
// the flame. Parity is RELEASED for the halo (the GPU takes the richer
// deco-over path, the CPU a true dilation), so this is NOT a byte-exact pin;
// it verifies:
//   * BOTH backends actually render the halo — a real dark ring darkens the
//     flame around the engulfed glyph on CPU AND GPU (the "both backends render
//     fire" sanity check, halo edition) — and its alpha SCALES with the cell's
//     engulfment strength on both;
//   * the halo is GATED on fire: fire_halo WITHOUT fire_patch draws no halo,
//     so the CPU/GPU frame stays byte-exact — as does `char_fg` alone (the
//     retired recolour stream keeps working as pure fg substitution and never
//     keys the ring);
//   * the CPU and GPU halos stay visually close (a bounded per-channel delta) —
//     the shared offsets + shared deco-over/blend contract + the shared
//     `fire_halo_alpha` byte keep them aligned.
//
// Gated: no GPU or no font -> no-op (like every other parity gate).

use aterm_core::render::{CharFg, FireHaloCell};
use aterm_core::terminal::Terminal;
use aterm_render::{FireMode, FirePatch, Theme};

mod common;
use common::{backends, bb, gg, max_channel_delta, rr};

fn luma(p: u32) -> i32 {
    rr(p) + gg(p) + bb(p)
}

/// One bright single-row fire patch filling `row`'s band across a column span.
fn bright_fire(row: u16, ch: usize, x: usize, w: usize, cov_cap: u8) -> FirePatch {
    FirePatch {
        row,
        x: x as u16,
        y: (row as usize * ch) as u16,
        w: w as u16,
        h: ch as u16,
        base_y: ((row as usize + 1) * ch) as u16,
        peak_h: (3 * ch) as u16,
        phase: 4096,
        temp: 240,
        strength: 255,
        lean: 0,
        cov_cap,
        cell_h: ch as u16,
        mode: FireMode::Add,
    }
}

fn halo(row: u16, col: u16, strength: u8) -> FireHaloCell {
    FireHaloCell { row, col, strength }
}

/// The retired recolour stream's hot-gold ink — kept to pin that `char_fg`
/// alone stays a pure (byte-exact) fg substitution that never keys the ring.
const HEAT_GLOW_FG: u32 = 0x00FF_C87A;

/// Count MARGIN ring pixels (just outside the engulfed block cell at `col`,
/// row 1) that the halo frame pulled well below the fire-only frame — the
/// halo's darkening footprint on the flame.
fn halo_ring_px(
    halo: &[u32],
    fire_only: &[u32],
    width: usize,
    cols: usize,
    cw: usize,
    ch: usize,
    col: usize,
) -> usize {
    let pad = (width - cols * cw) / 2;
    let bx0 = pad + col * cw;
    let (by0, by1) = (pad + ch + ch / 4, pad + ch + (3 * ch) / 4);
    let mut ring = 0usize;
    for y in by0..by1 {
        for x in ((bx0 - 3)..bx0).chain((bx0 + cw)..(bx0 + cw + 3)) {
            let i = y * width + x;
            if luma(halo[i]) + 90 < luma(fire_only[i]) {
                ring += 1;
            }
        }
    }
    ring
}

/// Summed luma deficit the halo carves out of the same ring margins — the
/// strength-scaling metric (linear in the stamp alpha, so ordering is exact).
fn halo_ring_deficit(
    halo: &[u32],
    fire_only: &[u32],
    width: usize,
    cols: usize,
    cw: usize,
    ch: usize,
    col: usize,
) -> u64 {
    let pad = (width - cols * cw) / 2;
    let bx0 = pad + col * cw;
    let (by0, by1) = (pad + ch + ch / 4, pad + ch + (3 * ch) / 4);
    let mut sum = 0u64;
    for y in by0..by1 {
        for x in ((bx0 - 3)..bx0).chain((bx0 + cw)..(bx0 + cw + 3)) {
            let i = y * width + x;
            sum += u64::from((luma(fire_only[i]) - luma(halo[i])).max(0) as u32);
        }
    }
    sum
}

/// BOTH backends render the fire contrast-halo at strength-scaled alpha, and
/// it is gated on a live fire field (fire_halo alone AND char_fg alone stay
/// byte-exact).
#[test]
fn both_backends_render_the_fire_contrast_halo() {
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (3usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Hidden cursor; a full-block glyph at row 1, col 4, spaces around it.
    term.process(b"\x1b[?25l\x1b[2;5H\xe2\x96\x88");
    let (cw, ch) = cpu.cell_size();
    let width = cpu.render_input(&term.cell_frame(rows, cols)).width;

    let fire = bright_fire(1, ch, cw, 8 * cw, 255);
    let engulfed = halo(1, 4, 255);

    // (a) fire_halo ALONE (no fire): gated OFF — must be byte-exact CPU==GPU
    // (the colour-free stream adds no pixels without a live fire field).
    let mut halo_only = term.cell_frame(rows, cols);
    halo_only.fire_halo = vec![engulfed];
    let cpu_ho = cpu.render_input(&halo_only).pixels.clone();
    let gpu_ho = gpu.render_input(&mut win, &halo_only, None).pixels;
    assert_eq!(
        max_channel_delta(&cpu_ho, &gpu_ho),
        0,
        "fire_halo WITHOUT fire draws no halo and must stay byte-exact CPU==GPU"
    );

    // (a2) char_fg ALONE (no fire): the retired recolour mechanism stays a
    // pure fg substitution — byte-exact CPU==GPU, and it never keys the ring.
    let mut char_only = term.cell_frame(rows, cols);
    char_only.char_fg = vec![CharFg {
        row: 1,
        col: 4,
        fg: HEAT_GLOW_FG,
    }];
    let cpu_co = cpu.render_input(&char_only).pixels.clone();
    let gpu_co = gpu.render_input(&mut win, &char_only, None).pixels;
    assert_eq!(
        max_channel_delta(&cpu_co, &gpu_co),
        0,
        "char_fg WITHOUT fire draws no halo and must stay byte-exact CPU==GPU"
    );

    // (b) fire-only reference (no fire_halo → no halo).
    let mut fire_only = term.cell_frame(rows, cols);
    fire_only.fire_patch = vec![fire];
    let cpu_fo = cpu.render_input(&fire_only).pixels.clone();
    let gpu_fo = gpu.render_input(&mut win, &fire_only, None).pixels;

    // (c) fire + fire_halo → the halo rings the engulfed glyph on BOTH backends.
    let mut haloed = term.cell_frame(rows, cols);
    haloed.fire_patch = vec![fire];
    haloed.fire_halo = vec![engulfed];
    let cpu_h = cpu.render_input(&haloed).pixels.clone();
    let gpu_h = gpu.render_input(&mut win, &haloed, None).pixels;

    let cpu_ring = halo_ring_px(&cpu_h, &cpu_fo, width, cols, cw, ch, 4);
    let gpu_ring = halo_ring_px(&gpu_h, &gpu_fo, width, cols, cw, ch, 4);
    let delta = max_channel_delta(&cpu_h, &gpu_h);
    eprintln!(
        "fire contrast-halo: CPU ring px={cpu_ring}, GPU ring px={gpu_ring}, CPU-vs-GPU max delta={delta}"
    );

    assert!(
        cpu_ring >= 12,
        "the CPU must ring the engulfed glyph with a dark halo ({cpu_ring} px)"
    );
    assert!(
        gpu_ring >= 12,
        "the GPU must ring the engulfed glyph with a dark halo ({gpu_ring} px)"
    );
    assert_ne!(
        cpu_h, cpu_fo,
        "the halo frame must differ from the fire-only frame (non-vacuous)"
    );
    // Parity RELEASED: the CPU dilation and the GPU deco-over dilation share the
    // offsets + the blend contract + the `fire_halo_alpha` byte, so they stay
    // visually close. A loose bound (not byte-exact) catches gross divergence
    // without pinning the richer path.
    assert!(
        delta <= 24,
        "CPU and GPU halos must stay visually close (max per-channel delta {delta})"
    );

    // (d) STRENGTH SCALING on both backends: a weak lick rims strictly less
    // than the full wall — and the weak frames stay just as close CPU-vs-GPU
    // (the shared alpha byte).
    let mut licked = term.cell_frame(rows, cols);
    licked.fire_patch = vec![fire];
    licked.fire_halo = vec![halo(1, 4, 24)];
    let cpu_l = cpu.render_input(&licked).pixels.clone();
    let gpu_l = gpu.render_input(&mut win, &licked, None).pixels;
    let cpu_wall = halo_ring_deficit(&cpu_h, &cpu_fo, width, cols, cw, ch, 4);
    let cpu_lick = halo_ring_deficit(&cpu_l, &cpu_fo, width, cols, cw, ch, 4);
    let gpu_wall = halo_ring_deficit(&gpu_h, &gpu_fo, width, cols, cw, ch, 4);
    let gpu_lick = halo_ring_deficit(&gpu_l, &gpu_fo, width, cols, cw, ch, 4);
    let lick_delta = max_channel_delta(&cpu_l, &gpu_l);
    eprintln!(
        "halo strength scaling: CPU wall/lick deficit={cpu_wall}/{cpu_lick}, \
         GPU wall/lick deficit={gpu_wall}/{gpu_lick}, lick CPU-vs-GPU max delta={lick_delta}"
    );
    assert!(
        cpu_lick > 0 && cpu_wall > cpu_lick,
        "CPU: the wall must rim strictly harder than a lick ({cpu_wall} vs {cpu_lick})"
    );
    assert!(
        gpu_lick > 0 && gpu_wall > gpu_lick,
        "GPU: the wall must rim strictly harder than a lick ({gpu_wall} vs {gpu_lick})"
    );
    assert!(
        lick_delta <= 24,
        "CPU and GPU weak-strength halos must stay visually close (max delta {lick_delta})"
    );
}
