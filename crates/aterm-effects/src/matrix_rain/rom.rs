// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Phosphor glyph ROM: 64 glyphs rasterized onto a 24×48 1-bit master grid, with optional terminal-output material overrides.

use std::sync::OnceLock;

/// Master glyph cell width in bits (one `u32` row per scanline).
pub const MASTER_W: usize = 24;
/// Master glyph cell height in scanlines.
pub const MASTER_H: usize = 48;
/// ROM size — must match [`super::field::GLYPH_COUNT`].
pub const ROM_GLYPHS: usize = 64;

const THICK: i32 = 4;
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

const RECIPES: [&[Stroke]; ROM_GLYPHS] = [
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
    &[
        L(7, 8, 17, 8),
        L(17, 8, 17, 40),
        L(17, 40, 7, 40),
        L(7, 40, 7, 8),
    ], // 0
    &[L(8, 14, 12, 8), L(12, 8, 12, 40), L(7, 40, 17, 40)],   // 1
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
    &[L(14, 8, 6, 26), L(6, 26, 19, 26), L(15, 16, 15, 42)],  // 4
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
    &[L(5, 8, 19, 8), L(19, 8, 10, 42)],                      // 7
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
    &[L(5, 8, 19, 8), L(19, 8, 5, 40), L(5, 40, 19, 40)],     // Z
    &[
        L(12, 10, 12, 34),
        L(4, 22, 20, 22),
        L(6, 12, 18, 32),
        L(18, 12, 6, 32),
    ], // *
    &[D(12, 14), D(12, 32)],                                  // :
    &[L(5, 24, 19, 24)],                                      // -
    &[L(5, 18, 19, 18), L(5, 30, 19, 30)],                    // =
    &[L(12, 10, 12, 38), L(4, 24, 20, 24)],                   // +
    &[L(18, 10, 6, 24), L(6, 24, 18, 38)],                    // <
    &[L(6, 10, 18, 24), L(18, 24, 6, 38)],                    // >
    &[L(12, 6, 12, 42)],                                      // |
    &[D(12, 24)],                                             // katakana middle dot
    &[L(9, 8, 8, 16), L(15, 8, 14, 16)],                      // "
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

/// Rasterized 1-bit master: 64 glyphs × 48 scanlines, one `u32` of 24 valid bits per scanline.
#[derive(Clone)]
pub struct RomMaster {
    rows: Vec<u32>,
}

impl RomMaster {
    /// Scanline `y` of `glyph` (low 24 bits valid).
    #[must_use]
    #[inline]
    pub fn row(&self, glyph: usize, y: usize) -> u32 {
        self.rows[glyph * MASTER_H + y]
    }
}

/// Rasterizes the full 64-glyph ROM master using integer math.
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

/// Returns the decorative ROM master, rasterized at most once per process.
#[inline]
pub(crate) fn decorative_master() -> &'static RomMaster {
    static DECORATIVE: OnceLock<RomMaster> = OnceLock::new();
    DECORATIVE.get_or_init(rasterize_master)
}

/// Returns the embedded 8×8 bitmap for a supported terminal-output character.
#[must_use]
#[inline]
pub fn material_bitmap(c: char) -> Option<[u8; 8]> {
    super::bitmap_font::bitmap(c)
}

/// Authors a hybrid 64-slot master with `chars` in the leading slots and decorative ROM in the tail.
#[must_use]
pub fn rasterize_material_master(chars: &[char]) -> RomMaster {
    let mut master = decorative_master().clone();
    for (glyph_index, &ch) in chars.iter().take(ROM_GLYPHS).enumerate() {
        let _ = author_material_glyph(&mut master, glyph_index, ch);
    }
    master
}

fn author_material_glyph(master: &mut RomMaster, slot: usize, ch: char) -> bool {
    let Some(bitmap) = material_bitmap(ch) else {
        return false;
    };
    let glyph = &mut master.rows[slot * MASTER_H..(slot + 1) * MASTER_H];
    glyph.fill(0);
    for (sy, bits) in bitmap.into_iter().enumerate() {
        if bits == 0 {
            continue;
        }
        let mut row_mask = 0u32;
        let mut b = bits;
        while b != 0 {
            let sx = b.trailing_zeros() as usize;
            b &= b - 1;
            row_mask |= 0b11 << (4 + sx * 2);
        }
        let y0 = 4 + sy * 5;
        glyph[y0..y0 + 5].fill(row_mask);
    }
    true
}

/// Re-authors `slot` of `master` in place for `ch`, or restores its decorative glyph when `None` or unsupported.
pub fn reauthor_material_glyph(master: &mut RomMaster, slot: usize, ch: Option<char>) {
    if slot >= ROM_GLYPHS {
        return;
    }
    if let Some(ch) = ch
        && author_material_glyph(master, slot, ch)
    {
        return;
    }
    let range = slot * MASTER_H..(slot + 1) * MASTER_H;
    master.rows[range.clone()].copy_from_slice(&decorative_master().rows[range]);
}

/// Set pixel `(x, y)` if in bounds.
#[inline]
fn plot(glyph: &mut [u32], x: i32, y: i32) {
    if (0..MASTER_W as i32).contains(&x) && (0..MASTER_H as i32).contains(&y) {
        glyph[y as usize] |= 1 << x;
    }
}

/// Stamp a `THICK×THICK` square centered-ish on `(x, y)` (offset −1 keeps the
/// stroke visually centered on the authored line).
#[inline]
fn stamp_thick(glyph: &mut [u32], x: i32, y: i32) {
    for dy in -1..THICK - 1 {
        for dx in -1..THICK - 1 {
            plot(glyph, x + dx, y + dy);
        }
    }
}

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
