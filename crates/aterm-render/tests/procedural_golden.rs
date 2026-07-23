// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Byte-exact golden PNGs for the procedural glyph source.
//!
//! Golden PNGs are reserved for FULLY-CONTROLLED pixels (the repo's testing
//! convention) — and procedural glyphs are exactly that: pure functions of
//! `(char, cell_w, cell_h)`, no fonts, no platform variance. A strip of
//! representative glyphs from all three blocks is composed white-on-black via
//! `Frame::to_png` and must match the committed golden byte for byte, at an
//! odd and an even cell size.
//!
//! Regenerate intentionally with `ATERM_BLESS_GOLDEN=1 cargo test -p
//! aterm-render --test procedural_golden` and review the image diff.

use aterm_render::{Frame, procedural};

/// One representative per drawing family: solid light/heavy lines, dashes,
/// corners/tees/crosses, mixed weights, doubles, arcs, diagonals, half lines,
/// eighth blocks, half blocks, shades, quadrants and braille.
const GLYPHS: &[char] = &[
    '─', '━', '│', '┃', '┄', '┇', '┈', '╌', '┌', '┐', '└', '┘', '├', '┤', '┬', '┴', '┼', '┏', '╋',
    '┾', '╄', '═', '║', '╔', '╗', '╚', '╝', '╠', '╣', '╦', '╩', '╬', '╪', '╫', '╒', '╜', '╭', '╮',
    '╯', '╰', '╱', '╲', '╳', '╴', '╹', '╼', '╿', '▀', '▄', '▌', '▐', '█', '▁', '▃', '▆', '▎', '▊',
    '▔', '▕', '░', '▒', '▓', '▖', '▚', '▛', '▟', '\u{2801}', '\u{2813}', '\u{28C0}', '\u{28FF}',
];

/// Compose the glyph strip: one row of `GLYPHS.len()` cells, coverage painted
/// as grey-on-black luminance — bit-controlled pixels only. Orthogonal
/// families must still be hard 0/255 (pure white or pure black); the AA
/// families (arcs ╭╮╯╰, diagonals ╱╲╳) carry interior grey levels, which the
/// goldens pin byte-exactly all the same.
fn strip(cw: usize, ch: usize) -> Frame {
    let (w, h) = (cw * GLYPHS.len(), ch);
    let mut pixels = vec![0u32; w * h];
    for (i, &g) in GLYPHS.iter().enumerate() {
        let cov = procedural::coverage(g, cw, ch).expect("strip glyphs are procedural");
        for y in 0..ch {
            for x in 0..cw {
                let c = cov[y * cw + x];
                assert!(
                    procedural::antialiased(g) || c == 0 || c == 255,
                    "{g:?}: orthogonal procedural coverage must be hard 0/255"
                );
                let c = u32::from(c);
                pixels[y * w + i * cw + x] = (c << 16) | (c << 8) | c;
            }
        }
    }
    Frame {
        width: w,
        height: h,
        pixels,
    }
}

fn check(name: &str, cw: usize, ch: usize) {
    let png = strip(cw, ch).to_png();
    let path = format!("{}/tests/golden/{name}", env!("CARGO_MANIFEST_DIR"));
    if std::env::var_os("ATERM_BLESS_GOLDEN").is_some() {
        std::fs::write(&path, &png).expect("bless golden");
        return;
    }
    let want = std::fs::read(&path).unwrap_or_else(|e| {
        panic!("missing golden {path}: {e}; generate with ATERM_BLESS_GOLDEN=1")
    });
    assert_eq!(
        png, want,
        "procedural strip at cell {cw}x{ch} drifted from {name}; if the change is \
         intentional, regenerate with ATERM_BLESS_GOLDEN=1 and review the diff"
    );
}

/// Odd cell dims (off-centre strokes take the documented right/bottom bias).
#[test]
fn golden_strip_9x19() {
    check("procedural_9x19.png", 9, 19);
}

/// Even cell dims (strokes centre exactly).
#[test]
fn golden_strip_10x20() {
    check("procedural_10x20.png", 10, 20);
}
