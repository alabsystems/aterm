// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Progressive box-filtering [`RainBaker`] from the 24×48 1-bit [`RomMaster`] into 64 glyph arts.

use std::sync::Arc;

use aterm_core::render::{FxArt, FxTexels};

use super::rom::{MASTER_H, MASTER_W, ROM_GLYPHS, RomMaster};

/// Maximum tiles baked per engine tick.
pub const MAX_RAIN_BAKES_PER_TICK: usize = 8;

/// Progressive master-to-cell-metric tile baker: one interned `cell_w × cell_h`
/// white-coverage art a glyph (straight RGBA, `255, 255, 255, coverage`), so a
/// rain cell drawn under the Linear law paints exactly its tint at its
/// coverage times its alpha.
pub struct RainBaker {
    cell_w: u16,
    cell_h: u16,
    /// The baked tiles by glyph: `None` until baked at the current metrics.
    tiles: Vec<Option<Arc<FxArt>>>,
    /// Per-instance monotonic version bumped on every rebake batch and restart.
    version: u64,
    /// Next tile index to bake (`>= ROM_GLYPHS` when complete).
    next_tile: usize,
    /// Flattened `(mx, wx)` horizontal box-filter segments shared across all 64 tiles for `cell_w`.
    col_seg: Vec<(u32, u32)>,
    /// Per-dest-column offsets into `col_seg` (length `cell_w + 1`).
    col_off: Vec<u32>,
}

impl Default for RainBaker {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl RainBaker {
    /// Construct an unbaked tile baker before any frame metric is bound.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cell_w: 0,
            cell_h: 0,
            tiles: Vec::new(),
            version: 0,
            next_tile: 0,
            col_seg: Vec::new(),
            col_off: Vec::new(),
        }
    }
    /// Restart the bake whenever `(cell_w, cell_h)` changes.
    #[inline]
    pub fn begin_frame(&mut self, cell_w: u16, cell_h: u16) {
        if (cell_w, cell_h) == (self.cell_w, self.cell_h) {
            return;
        }
        self.cell_w = cell_w;
        self.cell_h = cell_h;
        self.restart();
    }

    /// Restart the bake from tile 0 at the current cell metrics.
    #[inline]
    pub fn restart(&mut self) {
        self.tiles.clear();
        self.tiles.resize(ROM_GLYPHS, None);
        self.next_tile = 0;
        self.version = self.version.wrapping_add(1);
    }

    /// Whether all 64 tiles are baked at the current metric.
    #[must_use]
    #[inline]
    pub const fn complete(&self) -> bool {
        self.next_tile >= ROM_GLYPHS || self.cell_w == 0 || self.cell_h == 0
    }

    /// Bake up to [`MAX_RAIN_BAKES_PER_TICK`] pending tiles, returning whether any tile was baked.
    #[inline]
    pub fn bake_tiles(&mut self, rom: &RomMaster) -> bool {
        if self.complete() {
            return false;
        }
        let end = (self.next_tile + MAX_RAIN_BAKES_PER_TICK).min(ROM_GLYPHS);
        let mut tile = self.next_tile;
        while tile < end {
            self.bake_tile(rom, tile);
            tile += 1;
        }
        self.next_tile = end;
        self.version = self.version.wrapping_add(1);
        true
    }

    /// Return whether the baker has non-zero cell metrics and all 64 tiles baked.
    #[must_use]
    #[inline]
    pub const fn can_rebake(&self) -> bool {
        self.cell_w != 0 && self.cell_h != 0 && self.next_tile >= ROM_GLYPHS
    }

    /// Re-bake the tiles selected by `mask` and advance `version` to match a full rebuild.
    #[inline]
    pub fn rebake_tiles(&mut self, rom: &RomMaster, mask: u64) -> bool {
        if !self.can_rebake() || mask == 0 {
            return false;
        }
        let mut rest = mask;
        while rest != 0 {
            let tile = rest.trailing_zeros() as usize;
            rest &= rest - 1;
            if tile < ROM_GLYPHS {
                self.bake_tile(rom, tile);
            }
        }
        let wholesale = 1 + ROM_GLYPHS.div_ceil(MAX_RAIN_BAKES_PER_TICK) as u64;
        self.version = self.version.wrapping_add(wholesale);
        true
    }

    /// Monotonic tile-set version folded into the frame fingerprint.
    #[must_use]
    #[inline]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// `glyph`'s baked tile (`glyph` taken modulo the 64 glyphs), `None` until baked.
    #[must_use]
    #[inline]
    pub const fn tile(&self, glyph: u8) -> Option<&Arc<FxArt>> {
        const _: () = assert!(ROM_GLYPHS.is_power_of_two());
        let tiles = self.tiles.as_slice();
        let idx = (glyph as usize) & (ROM_GLYPHS - 1);
        if idx < tiles.len() {
            tiles[idx].as_ref()
        } else {
            None
        }
    }

    /// Box-filter `tile`'s 24×48 master into a `(cell_w, cell_h)` art with integer area weighting.
    #[inline]
    fn bake_tile(&mut self, rom: &RomMaster, tile: usize) {
        let (cw, ch) = (usize::from(self.cell_w), usize::from(self.cell_h));
        if cw == 0 || ch == 0 || tile >= self.tiles.len() {
            return;
        }
        const DEN: u32 = (MASTER_W * MASTER_H) as u32;

        if self.col_off.len() != cw + 1 {
            self.col_seg.clear();
            self.col_off.clear();
            self.col_off.push(0);
            let mut x = 0;
            while x < cw {
                let (c, d) = (x * MASTER_W, (x + 1) * MASTER_W);
                let (mx0, mx1) = (c / cw, (d - 1) / cw);
                let mut mx = mx0;
                while mx <= mx1 {
                    let wx = (d.min((mx + 1) * cw) - c.max(mx * cw)) as u32;
                    self.col_seg.push((mx as u32, wx));
                    mx += 1;
                }
                self.col_off.push(self.col_seg.len() as u32);
                x += 1;
            }
        }

        let glyph_rows = &rom.rows()[tile * MASTER_H..(tile + 1) * MASTER_H];
        let mut rgba = [255u8, 255, 255, 0].repeat(cw * ch);
        let mut y = 0;
        while y < ch {
            let cur_y = y;
            y += 1;
            let (a, b) = (cur_y * MASTER_H, (cur_y + 1) * MASTER_H);
            let (my0, my1) = (a / ch, (b - 1) / ch);
            let my_end = my1.min(MASTER_H - 1);
            let mut col_wy = [0u32; MASTER_W];
            let mut any_lit = false;
            let mut my = my0;
            while my <= my_end {
                let mut row = glyph_rows[my];
                if row != 0 {
                    any_lit = true;
                    let wy = (b.min((my + 1) * ch) - a.max(my * ch)) as u32;
                    while row != 0 {
                        let mx = row.trailing_zeros() as usize;
                        row &= row - 1;
                        if mx < MASTER_W {
                            col_wy[mx] += wy;
                        }
                    }
                }
                my += 1;
            }
            if !any_lit {
                continue;
            }
            let row_off = cur_y * cw;
            let mut x = 0;
            while x < cw {
                let mut num: u32 = 0;
                let seg = &self.col_seg[self.col_off[x] as usize..self.col_off[x + 1] as usize];
                let mut si = 0;
                while si < seg.len() {
                    let (mx, wx) = seg[si];
                    num += wx * col_wy[mx as usize];
                    si += 1;
                }
                if num > 0 {
                    rgba[(row_off + x) * 4 + 3] = ((num * 255 + DEN / 2) / DEN) as u8;
                }
                x += 1;
            }
        }
        self.tiles[tile] =
            aterm_render::fx::bitmap(FxTexels::Rgba8Straight, self.cell_w, self.cell_h, rgba);
    }
}

#[cfg(test)]
mod tests {
    use super::super::rom::rasterize_master;
    use super::*;

    fn baked(cw: u16, ch: u16) -> RainBaker {
        let rom = rasterize_master();
        let mut b = RainBaker::default();
        b.begin_frame(cw, ch);
        while !b.complete() {
            assert!(b.bake_tiles(&rom));
        }
        b
    }

    fn tile_peak(b: &RainBaker, g: u8) -> u8 {
        let art = b.tile(g).expect("baked tile");
        assert_eq!((art.w(), art.h()), (b.cell_w, b.cell_h), "one cell a tile");
        art.data()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|t| t[3])
            .max()
            .unwrap_or(0)
    }

    /// Verify that all 64 tiles carry ink across small and retina cell metrics.
    #[test]
    fn all_tiles_nonempty_at_live_metrics() {
        for (cw, ch) in [(7u16, 14u16), (10, 20), (20, 40)] {
            let b = baked(cw, ch);
            for g in 0..ROM_GLYPHS as u8 {
                assert!(tile_peak(&b, g) >= 64, "tile {g} too faint at {cw}x{ch}");
                assert!(
                    b.tile(g)
                        .expect("baked")
                        .data()
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .all(|t| t[..3] == [255, 255, 255]),
                    "tile {g}: white coverage, so a tint paints exactly"
                );
            }
        }
    }

    /// Verify that rebuilt bakers replay the same version sequence and bake the same tiles.
    #[test]
    fn rebuilt_bakers_replay_versions_and_tiles() {
        let rom = rasterize_master();
        let mut first = Vec::new();
        let mut first_ids = Vec::new();
        for run in 0..2 {
            let mut b = RainBaker::default();
            b.begin_frame(10, 20);
            let mut seq = vec![b.version()];
            while !b.complete() {
                b.bake_tiles(&rom);
                seq.push(b.version());
            }
            let ids: Vec<u128> = (0..ROM_GLYPHS as u8)
                .map(|g| b.tile(g).expect("baked").id())
                .collect();
            if run == 0 {
                (first, first_ids) = (seq, ids);
            } else {
                assert_eq!(seq, first, "rebuilt baker replays the version sequence");
                assert_eq!(ids, first_ids, "…and bakes the same tiles");
            }
        }
    }

    /// Verify that `begin_frame` with unchanged metrics preserves bake progress.
    #[test]
    fn same_metric_begin_frame_keeps_progress() {
        let rom = rasterize_master();
        let mut b = RainBaker::default();
        b.begin_frame(10, 20);
        b.bake_tiles(&rom);
        let (tile, ver) = (b.next_tile, b.version());
        assert!(b.tile(0).is_some() && b.tile(MAX_RAIN_BAKES_PER_TICK as u8).is_none());
        b.begin_frame(10, 20);
        assert_eq!((b.next_tile, b.version()), (tile, ver));
    }
}
