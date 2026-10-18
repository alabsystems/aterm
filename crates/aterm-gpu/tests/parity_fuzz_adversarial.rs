// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! GPU render panic-freedom and CPU/GPU parity on adversarial engine states.
//!
//! Drives the engine with random bytes, crafted CSI/OSC/SGR and
//! emoji/CJK/combining/procedural content to exercise the GPU-specific code
//! (atlas packing, colour atlas, wgpu encode, cursor compositing), renders on
//! the GPU, and asserts no panic, a valid frame, and CPU==GPU within the
//! <=8-LSB parity bound. Deterministic.

use aterm_core::terminal::Terminal;
use aterm_render::Theme;

use crate::common::{
    backends, count_exceeding_frame as count_exceeding,
    max_channel_delta_frame as max_channel_delta,
};

/// Per-pixel ceiling for the rare glyph AA-fringe pixel (software vs hardware
/// sRGB encode; see `parity_fuzz.rs`). Interiors stay within 8.
const EDGE_CEIL: i32 = 40;
/// Max fringe pixels per frame.
const MAX_FRINGE_PX: usize = 8;

#[inline]
fn next(s: &mut u64) -> u32 {
    *s = s
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (*s >> 33) as u32
}

/// Append one chunk of feature-rich / hostile content.
fn emit_adversarial(s: &mut u64, buf: &mut Vec<u8>) {
    const SAMPLES: &[&[u8]] = &[
        b"\xf0\x9f\x9a\x80",                           // 🚀
        "\u{2764}\u{fe0f}".as_bytes(),                 // ❤️ (VS16)
        "\u{1f468}\u{200d}\u{1f469}".as_bytes(),       // ZWJ pair
        "\u{1f44d}\u{1f3fd}".as_bytes(),               // 👍🏽 skin tone
        "\u{1f1fa}\u{1f1f8}".as_bytes(),               // 🇺🇸 flag
        "e\u{0301}".as_bytes(),                        // decomposed é
        b"\xe6\x97\xa5",                               // CJK 日
        "\u{2500}\u{2502}\u{250c}\u{2510}".as_bytes(), // box
        "\u{2588}\u{2580}\u{2584}\u{2591}".as_bytes(), // blocks
        "\u{1fb00}".as_bytes(),                        // sextant
        "\u{1cd4e}".as_bytes(),                        // octant
        "\u{e0b0}\u{e0b2}".as_bytes(),                 // Powerline
        b"\xf0",                                       // truncated UTF-8
        b"\xff",                                       // invalid byte
        b"abc ",
    ];
    match next(s) % 6 {
        0 | 1 => buf.extend_from_slice(SAMPLES[(next(s) as usize) % SAMPLES.len()]),
        2 => {
            // CSI with random params + final.
            buf.extend_from_slice(b"\x1b[");
            for _ in 0..(next(s) % 4) {
                buf.extend_from_slice(next(s).to_string().as_bytes());
                buf.push(b';');
            }
            buf.push((0x40 + (next(s) % 0x3f)) as u8);
        }
        3 => {
            // SGR (colours / styles) — exercise styled glyph + decoration paths.
            buf.extend_from_slice(b"\x1b[");
            buf.extend_from_slice((next(s) % 110).to_string().as_bytes());
            buf.push(b'm');
        }
        4 => buf.push((next(s) & 0xff) as u8), // raw byte
        _ => buf.extend_from_slice(b"\r\n"),
    }
}

#[test]
fn gpu_render_adversarial_states_never_panics_and_matches_cpu() {
    let theme = Theme::default();
    let px = 16.0;
    let (mut cpu, mut gpu) = backends(px, theme);
    // Compare the shared base render: GPU-only bloom and heat shimmer are
    // outside the parity check.
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();
    let mut win = aterm_gpu::WindowGpu::new();

    let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut worst = 0i32;
    let mut worst_exc = 0usize;
    let iters = 150;
    for it in 0..iters {
        // Vary the grid every frame — stresses GPU texture allocation across many
        // sizes, including degenerate-small ones (down to 1x1).
        let rows = 1 + (next(&mut s) % 28) as usize;
        let cols = 1 + (next(&mut s) % 78) as usize;
        let mut term = Terminal::new(rows as u16, cols as u16);
        let mut buf = Vec::with_capacity(128);
        let chunks = 4 + next(&mut s) % 20;
        for _ in 0..chunks {
            emit_adversarial(&mut s, &mut buf);
        }
        term.process(&buf);
        // Vary the render path: DEC double-width and double-height lines (the
        // clip/scale path), plus cursor hide/show.
        let dec = next(&mut s) % 5;
        match dec {
            0 => term.process(b"\x1b#6"), // DECDWL
            1 => term.process(b"\x1b#3"), // DECDHL top half
            2 => term.process(b"\x1b#4"), // DECDHL bottom half
            _ => {}
        }
        if next(&mut s).is_multiple_of(3) {
            term.process(b"\x1b[?25l");
        }

        let input = term.cell_frame(rows, cols);
        let cpu_frame = cpu.render_input(&input);
        let gpu_frame = gpu.render_input(&mut win, &input, None); // must not panic
        assert_eq!(
            (cpu_frame.width, cpu_frame.height),
            (gpu_frame.width, gpu_frame.height),
            "iter {it}: dimensions diverge"
        );
        assert!(!gpu_frame.pixels.is_empty(), "iter {it}: empty GPU frame");

        // DECDHL (dec 1/2) is excluded from parity: with the top half anchored
        // above the visible area the clip boundary does not align to the 2x
        // source grid, and GPU UV + NEAREST sampling picks a different source
        // row than the CPU's integer clip (`parity_fuzz.rs` covers the aligned
        // case). It still renders for panic-freedom; DECDWL (dec 0) is checked.
        let is_decdhl = dec == 1 || dec == 2;
        if !is_decdhl {
            let d = max_channel_delta(&cpu_frame, &gpu_frame);
            worst = worst.max(d);
            // Bounded by magnitude and count (the AA-fringe bound; see `parity_fuzz.rs`).
            let exc = count_exceeding(&cpu_frame, &gpu_frame, 8);
            worst_exc = worst_exc.max(exc);
            assert!(
                d <= EDGE_CEIL && exc <= MAX_FRINGE_PX,
                "iter {it}: diverge by {d} (ceil {EDGE_CEIL}) over {exc} px (max {MAX_FRINGE_PX}) \
                 ({rows}x{cols}, dec={dec}) — linear-light AA-fringe bound exceeded (interiors <=8)"
            );
        }
    }
    eprintln!(
        "gpu adversarial parity fuzz: {iters} frames; worst delta {worst}, worst fringe-px {worst_exc}"
    );
}
