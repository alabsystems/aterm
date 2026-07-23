// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PHOSPHOR glyph ROM — 64 glyphs on a 24×48 1-bit master grid
//! (matrix-rain design §2). The cold ROM contains ~40 katakana-like forms,
//! digits, symbols, and kanji-like blocks. When output material is enabled,
//! [`rasterize_material_master`] replaces its first slots with the LITERAL
//! characters sampled from the terminal, using small public-domain bitmap
//! faces. The resulting master still feeds the same fixed-size atlas on native
//! and embedded/wasm hosts.
//!
//! Recipes are const data (line segments + dots); an integer rasterizer
//! (Bresenham strokes at ~4 px thickness) produces the 1-bit master ONCE at
//! first bake. Deterministic, no floats. The bar is RECOGNIZABILITY, not
//! typographic beauty — the mirrored film texture comes from `flip_x` at
//! render time, not from authoring mirrored art.

use font8x8::{
    BASIC_FONTS, BLOCK_FONTS, BOX_FONTS, GREEK_FONTS, HIRAGANA_FONTS, LATIN_FONTS, MISC_FONTS,
    UnicodeFonts,
};

/// Master glyph cell width in bits (one `u32` row per scanline).
pub const MASTER_W: usize = 24;
/// Master glyph cell height in scanlines.
pub const MASTER_H: usize = 48;
/// ROM size — must match [`super::field::GLYPH_COUNT`].
pub const ROM_GLYPHS: usize = 64;

/// Stroke thickness: each Bresenham point stamps a `THICK×THICK` square
/// (offset −1, so a stroke reads ~4 px wide on the 24-px master).
const THICK: i32 = 4;
/// Dot radius² (a stamped disc, `dx² + dy² <= DOT_R2` → ~5 px across).
const DOT_R2: i32 = 6;

/// One authoring primitive on the 24×48 grid.
#[derive(Clone, Copy)]
enum Stroke {
    /// Line segment `(x0, y0) → (x1, y1)`, stamped at [`THICK`].
    L(i32, i32, i32, i32),
    /// Round dot centered at `(x, y)`.
    D(i32, i32),
}

use Stroke::{D, L};

/// The 64 recipes. Order is the field's glyph index (`glyph_at() % 64`):
/// 0..=39 katakana-like, 40..=49 digits, 50..=63 symbols/kanji-like.
const RECIPES: [&[Stroke]; ROM_GLYPHS] = [
    // -- katakana-like forms (simplified straight-stroke approximations) ----
    &[L(3, 10, 21, 10), L(15, 10, 13, 24), L(13, 24, 7, 40)], // a
    &[L(18, 8, 8, 24), L(12, 20, 12, 42)],                    // i
    &[
        D(12, 7),
        L(5, 14, 19, 14),
        L(19, 14, 19, 26),
        L(19, 26, 9, 40),
    ], // u
    &[L(5, 10, 19, 10), L(12, 10, 12, 38), L(4, 38, 20, 38)], // e
    &[
        L(4, 14, 20, 14),
        L(14, 8, 14, 34),
        L(14, 34, 10, 40),
        L(14, 20, 6, 34),
    ], // o
    &[
        L(4, 14, 19, 14),
        L(15, 8, 15, 30),
        L(15, 30, 10, 40),
        L(8, 14, 5, 34),
    ], // ka
    &[L(5, 14, 19, 12), L(4, 24, 20, 22), L(14, 6, 10, 42)],  // ki
    &[
        L(9, 8, 17, 8),
        L(17, 8, 20, 18),
        L(20, 18, 8, 40),
        L(9, 8, 6, 20),
    ], // ku
    &[
        L(9, 8, 5, 24),
        L(8, 14, 20, 14),
        L(15, 14, 15, 30),
        L(15, 30, 10, 40),
    ], // ke
    &[L(5, 12, 19, 12), L(19, 12, 19, 36), L(5, 36, 19, 36)], // ko
    &[
        L(4, 16, 20, 16),
        L(9, 9, 9, 26),
        L(16, 9, 16, 32),
        L(16, 32, 11, 40),
    ], // sa
    &[D(7, 11), D(6, 21), L(4, 38, 20, 12)],                  // shi
    &[L(5, 10, 19, 10), L(19, 10, 8, 38), L(11, 26, 20, 38)], // su
    &[
        L(9, 8, 9, 36),
        L(9, 36, 19, 36),
        L(4, 18, 19, 14),
        L(19, 14, 17, 24),
    ], // se
    &[L(6, 10, 8, 20), L(18, 8, 12, 40)],                     // so
    &[
        L(9, 8, 17, 8),
        L(17, 8, 20, 18),
        L(20, 18, 8, 40),
        L(9, 8, 6, 20),
        L(10, 24, 16, 28),
    ], // ta
    &[
        L(6, 10, 19, 8),
        L(4, 20, 20, 20),
        L(13, 10, 13, 34),
        L(13, 34, 9, 40),
    ], // chi
    &[D(7, 10), D(12, 9), L(20, 10, 8, 40)],                  // tsu
    &[
        L(6, 8, 18, 8),
        L(4, 16, 20, 16),
        L(12, 16, 12, 32),
        L(12, 32, 8, 40),
    ], // te
    &[L(8, 6, 8, 42), L(8, 18, 18, 28)],                      // to
    &[L(4, 16, 20, 16), L(13, 6, 13, 28), L(13, 28, 8, 40)],  // na
    &[L(6, 12, 18, 12), L(4, 34, 20, 34)],                    // ni
    &[L(5, 10, 19, 10), L(19, 10, 7, 38), L(8, 20, 17, 32)],  // nu
    &[
        D(12, 6),
        L(5, 14, 19, 14),
        L(19, 14, 6, 34),
        L(12, 22, 12, 42),
        L(12, 28, 18, 36),
    ], // ne
    &[L(17, 6, 7, 42)],                                       // no
    &[L(10, 10, 5, 38), L(14, 10, 19, 38)],                   // ha
    &[L(7, 8, 7, 36), L(7, 36, 19, 36), L(7, 22, 18, 14)],    // hi
    &[L(5, 10, 19, 10), L(19, 10, 17, 26), L(17, 26, 7, 40)], // fu
    &[L(4, 20, 10, 10), L(10, 10, 20, 36)],                   // he
    &[
        L(4, 14, 20, 14),
        L(12, 6, 12, 36),
        L(7, 22, 5, 32),
        L(17, 22, 19, 32),
    ], // ho
    &[L(5, 10, 19, 10), L(19, 10, 9, 30), D(14, 34)],         // ma
    &[L(6, 10, 18, 14), L(6, 22, 18, 26), L(6, 34, 18, 38)],  // mi
    &[L(13, 8, 5, 36), L(5, 36, 19, 36), D(15, 28)],          // mu
    &[L(17, 6, 7, 40), L(6, 18, 19, 34)],                     // me
    &[
        L(6, 10, 18, 10),
        L(4, 20, 20, 20),
        L(11, 10, 11, 32),
        L(11, 32, 17, 36),
    ], // mo
    &[L(5, 14, 18, 20), L(18, 20, 16, 28), L(12, 6, 12, 42)], // ya
    &[L(6, 12, 17, 12), L(17, 12, 17, 32), L(4, 32, 20, 32)], // yu
    &[
        L(5, 10, 19, 10),
        L(19, 10, 19, 38),
        L(5, 38, 19, 38),
        L(8, 24, 19, 24),
    ], // yo
    &[
        L(6, 8, 18, 8),
        L(4, 16, 20, 16),
        L(20, 16, 18, 28),
        L(18, 28, 8, 40),
    ], // ra
    &[L(8, 8, 8, 28), L(16, 8, 16, 32), L(16, 32, 10, 42)],   // ri
    // -- digits 0-9 ----------------------------------------------------------
    &[
        L(7, 8, 17, 8),
        L(17, 8, 17, 40),
        L(17, 40, 7, 40),
        L(7, 40, 7, 8),
    ], // 0
    &[L(8, 14, 12, 8), L(12, 8, 12, 40), L(7, 40, 17, 40)], // 1
    &[
        L(6, 10, 17, 10),
        L(17, 10, 17, 22),
        L(17, 22, 6, 40),
        L(6, 40, 18, 40),
    ], // 2
    &[
        L(6, 8, 17, 8),
        L(17, 8, 17, 40),
        L(17, 40, 6, 40),
        L(9, 23, 17, 23),
    ], // 3
    &[L(14, 8, 6, 26), L(6, 26, 19, 26), L(15, 16, 15, 42)], // 4
    &[
        L(18, 8, 7, 8),
        L(7, 8, 7, 22),
        L(7, 22, 17, 22),
        L(17, 22, 17, 40),
        L(17, 40, 6, 40),
    ], // 5
    &[
        L(17, 8, 9, 8),
        L(9, 8, 7, 20),
        L(7, 20, 7, 40),
        L(7, 40, 17, 40),
        L(17, 40, 17, 24),
        L(17, 24, 7, 24),
    ], // 6
    &[L(5, 8, 19, 8), L(19, 8, 10, 42)],                    // 7
    &[
        L(7, 8, 17, 8),
        L(17, 8, 17, 40),
        L(17, 40, 7, 40),
        L(7, 40, 7, 8),
        L(7, 24, 17, 24),
    ], // 8
    &[
        L(17, 24, 7, 24),
        L(7, 24, 7, 8),
        L(7, 8, 17, 8),
        L(17, 8, 17, 40),
        L(17, 40, 9, 40),
    ], // 9
    // -- symbols + kanji-like blocks -----------------------------------------
    &[L(5, 8, 19, 8), L(19, 8, 5, 40), L(5, 40, 19, 40)], // Z
    &[
        L(12, 10, 12, 34),
        L(4, 22, 20, 22),
        L(6, 12, 18, 32),
        L(18, 12, 6, 32),
    ], // *
    &[D(12, 14), D(12, 32)],                              // :
    &[L(5, 24, 19, 24)],                                  // -
    &[L(5, 18, 19, 18), L(5, 30, 19, 30)],                // =
    &[L(12, 10, 12, 38), L(4, 24, 20, 24)],               // +
    &[L(18, 10, 6, 24), L(6, 24, 18, 38)],                // <
    &[L(6, 10, 18, 24), L(18, 24, 6, 38)],                // >
    &[L(12, 6, 12, 42)],                                  // |
    &[D(12, 24)],                                         // katakana middle dot
    &[L(9, 8, 8, 16), L(15, 8, 14, 16)],                  // "
    &[
        L(6, 8, 18, 8),
        L(18, 8, 18, 40),
        L(18, 40, 6, 40),
        L(6, 40, 6, 8),
        L(6, 24, 18, 24),
    ], // sun-like block
    &[
        L(6, 10, 18, 10),
        L(18, 10, 18, 38),
        L(18, 38, 6, 38),
        L(6, 38, 6, 10),
    ], // mouth-like block
    &[L(6, 12, 18, 12), L(4, 24, 20, 24), L(12, 12, 12, 42)], // dry-like block
];

/// The rasterized 1-bit master: 64 glyphs × 48 scanlines, one `u32` of
/// 24 valid bits per scanline (bit `x` set ⇒ pixel `(x, y)` inked).
pub struct RomMaster {
    rows: Vec<u32>,
}

impl RomMaster {
    /// Scanline `y` of glyph `g` (low 24 bits valid).
    #[must_use]
    pub fn row(&self, glyph: usize, y: usize) -> u32 {
        self.rows[glyph * MASTER_H + y]
    }

    /// Whether master pixel `(x, y)` of `glyph` is inked.
    #[must_use]
    pub fn is_set(&self, glyph: usize, x: usize, y: usize) -> bool {
        x < MASTER_W && (self.row(glyph, y) >> x) & 1 == 1
    }
}

/// Rasterize the whole ROM. Called once at first bake (the master is ~12 KB);
/// pure integer math, so the bit pattern is identical on every host.
#[must_use]
pub fn rasterize_master() -> RomMaster {
    let mut rows = vec![0u32; ROM_GLYPHS * MASTER_H];
    for (g, recipe) in RECIPES.iter().enumerate() {
        let glyph = &mut rows[g * MASTER_H..(g + 1) * MASTER_H];
        for stroke in *recipe {
            match *stroke {
                L(x0, y0, x1, y1) => stamp_line(glyph, x0, y0, x1, y1),
                D(x, y) => stamp_dot(glyph, x, y),
            }
        }
    }
    RomMaster { rows }
}

/// The embedded bitmap for one literal material character. The supported
/// families cover Basic/Latin text, Greek, terminal box/block drawing, and
/// Hiragana. Unsupported code points are skipped by the material sampler; they
/// are never substituted with a different-looking character and therefore can
/// never make the "real output" contract dishonest.
#[must_use]
pub fn material_bitmap(c: char) -> Option<[u8; 8]> {
    BASIC_FONTS
        .get(c)
        .or_else(|| LATIN_FONTS.get(c))
        .or_else(|| GREEK_FONTS.get(c))
        .or_else(|| BOX_FONTS.get(c))
        .or_else(|| BLOCK_FONTS.get(c))
        .or_else(|| HIRAGANA_FONTS.get(c))
        .or_else(|| MISC_FONTS.get(c))
        .filter(|bitmap| bitmap.iter().any(|&row| row != 0))
}

/// Author a hybrid 64-slot master whose prefix is the exact `chars` supplied
/// by the output-material bank and whose unused tail remains the classic ROM.
/// Each 8×8 source pixel expands to 2×5 pixels, centered in the 24×48 master.
/// The four-pixel breathing room on every side prevents chunky letterforms from
/// touching adjacent terminal cells after the integer box filter. Conversion
/// remains integer-only and byte-identical on every backend.
#[must_use]
pub fn rasterize_material_master(chars: &[char]) -> RomMaster {
    let mut master = rasterize_master();
    for (glyph_index, &ch) in chars.iter().take(ROM_GLYPHS).enumerate() {
        let Some(bitmap) = material_bitmap(ch) else {
            continue;
        };
        let glyph = &mut master.rows[glyph_index * MASTER_H..(glyph_index + 1) * MASTER_H];
        glyph.fill(0);
        for (sy, bits) in bitmap.into_iter().enumerate() {
            for sx in 0..8usize {
                if bits & (1 << sx) == 0 {
                    continue;
                }
                for dy in 0..5usize {
                    for dx in 0..2usize {
                        plot(glyph, (4 + sx * 2 + dx) as i32, (4 + sy * 5 + dy) as i32);
                    }
                }
            }
        }
    }
    master
}

/// Set pixel `(x, y)` if in bounds.
fn plot(glyph: &mut [u32], x: i32, y: i32) {
    if (0..MASTER_W as i32).contains(&x) && (0..MASTER_H as i32).contains(&y) {
        glyph[y as usize] |= 1 << x;
    }
}

/// Stamp a `THICK×THICK` square centered-ish on `(x, y)` (offset −1 keeps the
/// stroke visually centered on the authored line).
fn stamp_thick(glyph: &mut [u32], x: i32, y: i32) {
    for dy in -1..THICK - 1 {
        for dx in -1..THICK - 1 {
            plot(glyph, x + dx, y + dy);
        }
    }
}

/// Integer Bresenham line, stamped at stroke thickness.
fn stamp_line(glyph: &mut [u32], x0: i32, y0: i32, x1: i32, y1: i32) {
    let (dx, dy) = ((x1 - x0).abs(), (y1 - y0).abs());
    let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
    let mut err = dx - dy;
    let (mut x, mut y) = (x0, y0);
    loop {
        stamp_thick(glyph, x, y);
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 > -dy {
            err -= dy;
            x += sx;
        }
        if e2 < dx {
            err += dx;
            y += sy;
        }
    }
}

/// Stamp a round dot (`dx² + dy² <= DOT_R2`).
fn stamp_dot(glyph: &mut [u32], x: i32, y: i32) {
    for dy in -2..=2 {
        for dx in -2..=2 {
            if dx * dx + dy * dy <= DOT_R2 {
                plot(glyph, x + dx, y + dy);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every one of the 64 glyphs rasterizes to a nonempty mask (an all-blank
    /// tile would read as a missing rain cell).
    #[test]
    fn all_64_glyphs_are_nonempty() {
        let m = rasterize_master();
        for g in 0..ROM_GLYPHS {
            let inked: u32 = (0..MASTER_H).map(|y| m.row(g, y).count_ones()).sum();
            assert!(inked >= 12, "glyph {g} too sparse ({inked} px)");
        }
    }

    /// Rasterization is deterministic and stays inside the 24-bit scanline.
    #[test]
    fn master_is_deterministic_and_in_bounds() {
        let a = rasterize_master();
        let b = rasterize_master();
        for g in 0..ROM_GLYPHS {
            for y in 0..MASTER_H {
                assert_eq!(a.row(g, y), b.row(g, y));
                assert_eq!(a.row(g, y) >> MASTER_W, 0, "ink past MASTER_W");
            }
        }
    }

    /// Glyphs are distinct as bit patterns (64 distinguishable forms — the
    /// recognizability floor).
    #[test]
    fn glyphs_are_pairwise_distinct() {
        let m = rasterize_master();
        for a in 0..ROM_GLYPHS {
            for b in a + 1..ROM_GLYPHS {
                let same = (0..MASTER_H).all(|y| m.row(a, y) == m.row(b, y));
                assert!(!same, "glyphs {a} and {b} rasterize identically");
            }
        }
    }

    /// Literal output material is literal: case and punctuation retain their
    /// own bitmap instead of hashing into a decorative kana substitute.
    #[test]
    fn material_master_preserves_real_characters() {
        let chars = ['C', 'c', '0', '{', '─'];
        let m = rasterize_material_master(&chars);
        for (g, ch) in chars.iter().enumerate() {
            let inked: u32 = (0..MASTER_H).map(|y| m.row(g, y).count_ones()).sum();
            assert!(inked > 0, "material character {ch:?} has ink");
        }
        for (a, b) in [(0usize, 1usize), (1, 2), (2, 3), (3, 4)] {
            assert!(
                (0..MASTER_H).any(|y| m.row(a, y) != m.row(b, y)),
                "distinct real characters {:?}/{:?} remain distinct",
                chars[a],
                chars[b]
            );
        }
        assert!(
            material_bitmap('🐈').is_none(),
            "unsupported art is skipped, never faked"
        );
        assert!(
            material_bitmap('\u{00a0}').is_none(),
            "an all-zero whitespace bitmap is not visible rain material"
        );
    }
}
