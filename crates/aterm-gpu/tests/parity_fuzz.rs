// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// CPU==GPU rendering parity fuzz. The CPU renderer is the GPU renderer's
// correctness oracle; `gpu_matches_cpu.rs` pins specific features, and this
// sweeps random mixed content (ASCII, colour emoji with VS16/ZWJ/flags, every
// SGR style and decoration, wide CJK, combining marks, procedural glyphs,
// opaque and transparent iTerm2 inline images, DECDWL/DECDHL) and asserts the
// two paths stay within the glyph AA/blend tolerance on every frame. A
// deterministic PRNG keeps it reproducible; gated on a GPU.

use aterm_core::terminal::Terminal;
use aterm_render::Theme;

use crate::common::{
    backends, count_exceeding_frame as count_exceeding,
    max_channel_delta_frame as max_channel_delta,
};

/// A 2x2-cell RGBA PNG: left column opaque, right column 50% alpha, covering
/// both the straight-RGBA image blit and the alpha-over-bg composite the GPU
/// image pass shares with colour emoji.
fn image_osc(cw: u32, ch: u32) -> Vec<u8> {
    let (iw, ih) = (2 * cw, 2 * ch);
    let mut rgba = Vec::with_capacity((iw * ih * 4) as usize);
    for _y in 0..ih {
        for x in 0..iw {
            let a = if x >= cw { 128 } else { 255 };
            rgba.extend_from_slice(&[60, 170, 220, a]);
        }
    }
    let mut png = Vec::new();
    {
        let mut enc = aterm_png::Encoder::new(&mut png, iw, ih);
        enc.set_color(aterm_png::ColorType::Rgba);
        enc.set_depth(aterm_png::BitDepth::Eight);
        let mut writer = enc.write_header().expect("png header");
        writer.write_image_data(&rgba).expect("png data");
    }
    let b64 = aterm_codec::base64::encode(&png).expect("encode");
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b]1337;File=inline=1;width=2;height=2:");
    out.extend_from_slice(b64.as_bytes());
    out.extend_from_slice(b"\x1b\\");
    out
}

/// Per-pixel ceiling for the rare divergent AA-fringe pixel. Metal measures
/// 1 over this seed; the ceiling leaves headroom for other GPU drivers.
const EDGE_CEIL: i32 = 40;
/// Max fringe pixels per frame (Metal measures 0). A real regression (a wrong
/// glyph or region) produces thousands.
const MAX_FRINGE_PX: usize = 8;

/// Tokens the fuzz strings together. Each is raw bytes fed to the terminal.
const TOKENS: &[&[u8]] = &[
    b"abc",
    b"XY",
    b"  ",
    b"123",
    b".rs",
    b"/usr",
    // Descenders: with `baseline = round(ascent)` some faces let g/j/p/q/y
    // overshoot the row band by a pixel, which the shared `row_scale` band
    // clip keeps identical. No other token exercises it.
    b"gjpq",
    b"yes/go",
    b"\x1b[1m",
    b"\x1b[3m",
    b"\x1b[4m",
    b"\x1b[9m",
    b"\x1b[21m",
    b"\x1b[4:3m",
    b"\x1b[53m",
    b"\x1b[0m",
    b"\x1b[31m",
    b"\x1b[42m",
    b"\x1b[7m",
    b"\x1b[2m",
    b"\x1b[38;2;200;120;40m",
    b"\x1b[4;58:2::255:0:0m",
    "\u{1F680}".as_bytes(),                   // rocket
    "\u{2764}\u{FE0F}".as_bytes(),            // VS16 heart
    "\u{1F468}\u{200D}\u{1F4BB}".as_bytes(),  // ZWJ tech
    "\u{1F1FA}\u{1F1F8}".as_bytes(),          // US flag
    "\u{1F44D}\u{1F3FD}".as_bytes(),          // skin-tone thumb
    "\u{65E5}\u{672C}".as_bytes(),            // CJK
    "e\u{0301}".as_bytes(),                   // é decomposed
    "\u{250C}\u{2500}\u{2510}".as_bytes(),    // box
    "\u{2588}\u{2592}".as_bytes(),            // block + shade
    "\u{2847}".as_bytes(),                    // braille
    "\u{1FB13}".as_bytes(),                   // sextant
    "\u{1CD00}\u{1CD71}\u{1CDE5}".as_bytes(), // octants
    "\u{E0B0}\u{E0B6}".as_bytes(),            // powerline
    b"\r\n",
    b"\r\n",
    b"\x1b#6", // DECDWL (line start)
    b"\x1b#3",
    b"\x1b#4", // DECDHL top/bottom
];

#[test]
fn cpu_gpu_parity_fuzz() {
    let theme = Theme::default();
    let px = 17.0;
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

    // Precompute the inline-image OSC for the renderer's cell size so the image's
    // footprint maps cleanly onto whole cells (iTerm2 places from the left margin).
    let (cw, ch) = cpu.cell_size();
    let image_token = image_osc(cw as u32, ch as u32);

    let (rows, cols) = (8usize, 24usize);
    let mut state: u64 = 0x243F_6A88_85A3_08D3;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as u32
    };

    let mut worst = 0i32;
    let mut worst_exc = 0usize;
    let iters = 160;
    for it in 0..iters {
        let mut term = Terminal::new(rows as u16, cols as u16);
        // The image footprint needs the cell pixel size so it tiles whole cells.
        term.set_cell_pixel_size(cw as u16, ch as u16);
        // Half the frames hide the cursor; the rest exercise the block cursor too.
        if next() & 1 == 0 {
            term.process(b"\x1b[?25l");
        }
        let token_count = 12 + (next() % 40) as usize;
        for _ in 0..token_count {
            // ~1 in 8 tokens is an inline image over the text, so many frames
            // exercise the image pass without it dominating.
            if next() % 8 == 0 {
                term.process(&image_token);
                continue;
            }
            let tok = TOKENS[(next() as usize) % TOKENS.len()];
            term.process(tok);
        }
        let input = term.cell_frame(rows, cols);
        let cpu_frame = cpu.render_input(&input);
        let gpu_frame = gpu.render_input(&mut win, &input, None);
        assert_eq!(
            (cpu_frame.width, cpu_frame.height),
            (gpu_frame.width, gpu_frame.height),
            "iter {it}: dimensions diverge"
        );
        let d = max_channel_delta(&cpu_frame, &gpu_frame);
        worst = worst.max(d);
        // Compositing is in linear light (CPU `blend`, GPU sRGB target). Almost
        // every pixel is within 8; a few glyph AA-fringe pixels differ more
        // because software and hardware sRGB encodes round differently near
        // black. Bounding both the per-pixel ceiling and the fringe count
        // still catches real regressions.
        let exc = count_exceeding(&cpu_frame, &gpu_frame, 8);
        worst_exc = worst_exc.max(exc);
        assert!(
            d <= EDGE_CEIL && exc <= MAX_FRINGE_PX,
            "iter {it}: diverge by {d} (ceil {EDGE_CEIL}) over {exc} px (max {MAX_FRINGE_PX}) \
             of {} — linear-light AA-fringe bound exceeded (interiors must stay <=8)",
            cpu_frame.pixels.len()
        );
    }
    eprintln!(
        "parity fuzz: {iters} frames; worst delta {worst}, worst fringe-px/frame {worst_exc}"
    );
}
