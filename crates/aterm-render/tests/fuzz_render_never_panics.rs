// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Panic-freedom fuzz for the CPU RENDER path on adversarial engine states.
//!
//! The engine fuzz (`aterm-core`) pins that `process()` never panics; this pins
//! the next stage — turning a grid into pixels — never panics either. That path
//! grew a lot of new code (rustybuzz cluster shaping, Apple-Color-Emoji `sbix`
//! decode, combining-mark centering, DECDWL/DECDHL NEAREST scaling, synthetic
//! bold/italic, the SGR decoration rects, procedural box/block/braille/sextant/
//! Powerline) that today is exercised only by CURATED parity/golden tests. A
//! shaping or metrics panic on hostile output would crash the window.
//!
//! This drives the engine with input BIASED toward those features (emoji of
//! every form, decomposed combining sequences, wide CJK, procedural glyphs,
//! random SGR styling, DEC line sizing) plus raw/garbage bytes, then RENDERS
//! every resulting frame and asserts the rasterizer never panics and always
//! returns a non-degenerate framebuffer. Deterministic LCG → reproducible.

use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme};

#[inline]
fn next(state: &mut u64) -> u32 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (*state >> 33) as u32
}

/// SGR styling: a run of random parameters (incl. the truecolor / underline-
/// colour `38;2`/`48;2`/`58;2;r;g;b` and `4:n` underline-style forms) then `m`.
/// Exercises synthetic bold/italic, every underline style, strike/overline, and
/// SGR-58 underline colour.
fn emit_sgr(s: &mut u64, buf: &mut Vec<u8>) {
    buf.extend_from_slice(b"\x1b[");
    let np = 1 + next(s) % 5;
    for i in 0..np {
        if i > 0 {
            buf.push(b';');
        }
        match next(s) % 14 {
            0 => buf.push(b'1'),               // bold
            1 => buf.push(b'3'),               // italic
            2 => buf.push(b'4'),               // underline
            3 => buf.push(b'9'),               // strike
            4 => buf.extend_from_slice(b"53"), // overline
            5 => buf.push(b'7'),               // inverse
            6 => buf.push(b'2'),               // dim
            7 => buf.push(b'0'),               // reset
            8 => {
                // 4:n curly/dotted/dashed underline style
                buf.extend_from_slice(b"4:");
                buf.extend_from_slice((next(s) % 6).to_string().as_bytes());
            }
            9 => emit_color(s, buf, b"38"),  // fg truecolor
            10 => emit_color(s, buf, b"48"), // bg truecolor
            11 => emit_color(s, buf, b"58"), // underline colour
            _ => buf.extend_from_slice((next(s) % 80).to_string().as_bytes()),
        }
    }
    buf.push(b'm');
}

fn emit_color(s: &mut u64, buf: &mut Vec<u8>, sel: &[u8]) {
    buf.extend_from_slice(sel);
    buf.extend_from_slice(b";2;");
    for i in 0..3 {
        if i > 0 {
            buf.push(b';');
        }
        buf.extend_from_slice((next(s) % 256).to_string().as_bytes());
    }
}

#[test]
fn render_adversarial_engine_states_never_panics() {
    let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let mut s = 0x5EED_1337_C0DE_FACEu64;
    let mut term = Terminal::new(24, 80);

    // Byte samples that drive the NEW render paths.
    let samples: &[&[u8]] = &[
        "🚀".as_bytes(),
        "❤\u{fe0f}".as_bytes(),              // VS16 colour
        "❤".as_bytes(),                      // bare (mono)
        "👨\u{200d}👩\u{200d}👧".as_bytes(), // ZWJ family
        "👍\u{1f3fd}".as_bytes(),            // skin tone
        "1\u{fe0f}\u{20e3}".as_bytes(),      // keycap
        "🇺🇸".as_bytes(),                     // flag (RI pair)
        "🇫".as_bytes(),                      // lone RI
        "e\u{0301}".as_bytes(),              // decomposed é
        "n\u{0303}".as_bytes(),
        "o\u{0308}".as_bytes(),
        "a\u{0301}\u{0308}\u{0303}".as_bytes(), // stacked marks
        "日本語".as_bytes(),                    // wide CJK
        "─│┌┐└┘├┤┬┴┼".as_bytes(),               // box-drawing (procedural)
        "█▀▄▌▐░▒▓".as_bytes(),                  // blocks/shades
        "⠀⡀⣿⠿".as_bytes(),                      // braille
        "\u{1fb00}\u{1fb1e}\u{1fb3b}".as_bytes(), // sextants
        "\u{e0b0}\u{e0b1}\u{e0b2}\u{e0bc}".as_bytes(), // Powerline
        b"\x1b#3",
        b"\x1b#4", // DECDHL top/bottom
        b"\x1b#6",
        b"\x1b#5", // DECDWL / single
        b"\xf0",
        b"\xe6\x97",
        b"\x80",
        b"\xff", // truncated / invalid UTF-8
        b"plain text ",
        b"AaBbCc09",
    ];

    let mut buf: Vec<u8> = Vec::with_capacity(256);
    // 700 frames × up to 8 feature-chunks each — high diversity per render keeps
    // wall-clock reasonable in the debug suite while covering the new paths well.
    for _ in 0..700u32 {
        buf.clear();
        let chunks = 1 + next(&mut s) % 8;
        for _ in 0..chunks {
            match next(&mut s) % 7 {
                0 | 1 => buf.extend_from_slice(samples[(next(&mut s) as usize) % samples.len()]),
                2 => emit_sgr(&mut s, &mut buf),
                3 => {
                    // a CSI cursor/edit op with random params
                    buf.extend_from_slice(b"\x1b[");
                    let np = next(&mut s) % 4;
                    for _ in 0..np {
                        buf.extend_from_slice(next(&mut s).to_string().as_bytes());
                        buf.push(b';');
                    }
                    buf.push((0x40 + (next(&mut s) % 0x3f)) as u8);
                }
                4 => buf.push((next(&mut s) & 0xFF) as u8),
                5 => buf.extend_from_slice(b"\r\n"),
                _ => buf.push((next(&mut s) % 0x20) as u8), // a random control byte
            }
        }
        term.process(&buf);

        // Occasionally resize so the render path sees fresh dimensions (and the
        // reflow of pathological wide/cluster/mark content across the new grid).
        if next(&mut s).is_multiple_of(96) {
            // Bounded sizes: large grids dominate render cost, and the wrap /
            // reflow / scale edges we care about fire at modest dimensions too.
            let rr = (1 + next(&mut s) % 32) as u16;
            let cc = (1 + next(&mut s) % 100) as u16;
            term.resize(rr, cc);
        }

        // RENDER the resulting frame — this is what must never panic.
        let (rows, cols) = (term.rows() as usize, term.cols() as usize);
        let frame = r.render_input(&term.cell_frame(rows, cols));
        assert!(
            frame.width > 0 && frame.height > 0 && !frame.pixels.is_empty(),
            "render produced a degenerate frame ({}x{})",
            frame.width,
            frame.height
        );
    }
}
