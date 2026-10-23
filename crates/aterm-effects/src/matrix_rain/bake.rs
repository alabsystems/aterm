// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Progressive box-filtering [`RainBaker`] from the 24×48 1-bit [`RomMaster`] into an 8×8 [`SceneAtlas`].

use std::sync::Arc;

use aterm_render::SceneAtlas;

use super::rom::{MASTER_H, MASTER_W, ROM_GLYPHS, RomMaster};

/// Tiles per atlas row and column (8×8 grid of the 64 glyphs).
pub const RAIN_TILE_GRID: usize = 8;
/// Maximum tiles baked per engine tick.
pub const MAX_RAIN_BAKES_PER_TICK: usize = 8;

/// Progressive master-to-cell-metric tile baker publishing [`SceneAtlas`] snapshots.
#[derive(Default)]
pub struct RainBaker {
    cell_w: u16,
    cell_h: u16,
    /// Straight-alpha RGBA8 texels (`(8 * cell_w) * (8 * cell_h)`).
    rgba: Vec<u8>,
    /// Per-instance monotonic version bumped on every rebake batch and restart.
    version: u64,
    /// Next tile index to bake (`>= ROM_GLYPHS` when complete).
    next_tile: usize,
    published: Option<Arc<SceneAtlas>>,
    dirty: bool,
    /// Flattened `(mx, wx)` horizontal box-filter segments shared across all 64 tiles for `cell_w`.
    col_seg: Vec<(u32, u32)>,
    /// Per-dest-column offsets into `col_seg` (length `cell_w + 1`).
    col_off: Vec<u32>,
}

impl RainBaker {
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
        let (cw, ch) = (usize::from(self.cell_w), usize::from(self.cell_h));
        self.rgba.clear();
        self.rgba
            .resize(cw * RAIN_TILE_GRID * ch * RAIN_TILE_GRID * 4, 0);
        self.next_tile = 0;
        self.version = self.version.wrapping_add(1);
        self.published = None;
        self.dirty = false;
    }

    /// Whether all 64 tiles are baked at the current metric.
    #[must_use]
    #[inline]
    pub const fn complete(&self) -> bool {
        self.next_tile >= ROM_GLYPHS || self.cell_w == 0 || self.cell_h == 0
    }

    /// Bake up to [`MAX_RAIN_BAKES_PER_TICK`] pending tiles, returning whether any tile was baked.
    pub fn bake_tiles(&mut self, rom: &RomMaster) -> bool {
        if self.complete() {
            return false;
        }
        let end = (self.next_tile + MAX_RAIN_BAKES_PER_TICK).min(ROM_GLYPHS);
        for tile in self.next_tile..end {
            self.bake_tile(rom, tile);
        }
        self.next_tile = end;
        self.version = self.version.wrapping_add(1);
        self.dirty = true;
        true
    }

    /// Return whether the baker has non-zero cell metrics and all 64 tiles baked.
    #[must_use]
    #[inline]
    pub const fn can_rebake(&self) -> bool {
        self.cell_w != 0 && self.cell_h != 0 && self.next_tile >= ROM_GLYPHS
    }

    /// Re-bake the tiles selected by `mask` into the completed atlas and advance `version` to match a full rebuild.
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
        self.dirty = true;
        true
    }

    /// Monotonic atlas version folded into the frame fingerprint.
    #[must_use]
    #[inline]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Return the published atlas snapshot, rebuilding it lazily after a dirty bake batch.
    #[inline]
    pub fn atlas(&mut self) -> Option<Arc<SceneAtlas>> {
        if self.dirty {
            self.published = Some(Arc::new(SceneAtlas {
                width: u32::from(self.cell_w) * RAIN_TILE_GRID as u32,
                height: u32::from(self.cell_h) * RAIN_TILE_GRID as u32,
                rgba: self.rgba.clone(),
                version: self.version,
            }));
            self.dirty = false;
        }
        self.published.clone()
    }

    /// Atlas texel origin of `glyph`'s `cell_w × cell_h` tile.
    #[must_use]
    #[inline]
    pub const fn tile_origin(&self, glyph: u32) -> (u16, u16) {
        let g = glyph as usize % ROM_GLYPHS;
        (
            ((g % RAIN_TILE_GRID) * self.cell_w as usize) as u16,
            ((g / RAIN_TILE_GRID) * self.cell_h as usize) as u16,
        )
    }

    /// Box-filter `tile`'s 24×48 master into its `(cell_w, cell_h)` atlas slot with integer area weighting.
    fn bake_tile(&mut self, rom: &RomMaster, tile: usize) {
        let (cw, ch) = (usize::from(self.cell_w), usize::from(self.cell_h));
        if cw == 0 || ch == 0 {
            return;
        }
        let atlas_w = cw * RAIN_TILE_GRID;
        let (ox, oy) = ((tile % RAIN_TILE_GRID) * cw, (tile / RAIN_TILE_GRID) * ch);
        const DEN: u32 = (MASTER_W * MASTER_H) as u32;

        if self.col_off.len() != cw + 1 {
            self.col_seg.clear();
            self.col_off.clear();
            self.col_off.push(0);
            for x in 0..cw {
                let (c, d) = (x * MASTER_W, (x + 1) * MASTER_W);
                let (mx0, mx1) = (c / cw, (d - 1) / cw);
                for mx in mx0..=mx1 {
                    let wx = (d.min((mx + 1) * cw) - c.max(mx * cw)) as u32;
                    self.col_seg.push((mx as u32, wx));
                }
                self.col_off.push(self.col_seg.len() as u32);
            }
        }

        let mut src: [(u32, u32); MASTER_H] = [(0, 0); MASTER_H];
        for y in 0..ch {
            let (a, b) = (y * MASTER_H, (y + 1) * MASTER_H);
            let (my0, my1) = (a / ch, (b - 1) / ch);
            let my_end = my1.min(MASTER_H - 1);
            let mut src_len = 0;
            for my in my0..=my_end {
                let row = rom.row(tile, my);
                if row != 0 {
                    let wy = (b.min((my + 1) * ch) - a.max(my * ch)) as u32;
                    src[src_len] = (wy, row);
                    src_len += 1;
                }
            }
            let src_row = &src[..src_len];
            let row_base = (oy + y) * atlas_w + ox;
            if src_row.is_empty() {
                for x in 0..cw {
                    let idx = (row_base + x) * 4;
                    self.rgba[idx..idx + 4].copy_from_slice(&[255, 255, 255, 0]);
                }
                continue;
            }
            for x in 0..cw {
                let mut num: u32 = 0;
                let seg = &self.col_seg[self.col_off[x] as usize..self.col_off[x + 1] as usize];
                for &(wy, row) in src_row {
                    for &(mx, wx) in seg {
                        if (row >> mx) & 1 == 1 {
                            num += wx * wy;
                        }
                    }
                }
                let alpha = ((num * 255 + DEN / 2) / DEN) as u8;
                let idx = (row_base + x) * 4;
                self.rgba[idx..idx + 4].copy_from_slice(&[255, 255, 255, alpha]);
            }
        }
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

    fn tile_peak(b: &mut RainBaker, g: u32) -> u8 {
        let atlas = b.atlas().expect("baked atlas");
        let (ax, ay) = b.tile_origin(g);
        let (cw, ch) = (usize::from(b.cell_w), usize::from(b.cell_h));
        let mut peak = 0u8;
        for y in 0..ch {
            for x in 0..cw {
                let idx = ((usize::from(ay) + y) * atlas.width as usize + usize::from(ax) + x) * 4;
                peak = peak.max(atlas.rgba[idx + 3]);
            }
        }
        peak
    }

    /// Verify that all 64 tiles carry ink across small and retina cell metrics.
    #[test]
    fn all_tiles_nonempty_at_live_metrics() {
        for (cw, ch) in [(7u16, 14u16), (10, 20), (20, 40)] {
            let mut b = baked(cw, ch);
            for g in 0..ROM_GLYPHS as u32 {
                assert!(
                    tile_peak(&mut b, g) >= 64,
                    "tile {g} too faint at {cw}x{ch}"
                );
            }
        }
    }

    /// Verify that rebuilt bakers replay the same version sequence while publishing distinct `Arc`s.
    #[test]
    fn rebuilt_bakers_replay_versions_but_publish_fresh_arcs() {
        let rom = rasterize_master();
        let mut first = Vec::new();
        let mut arcs: Vec<Arc<SceneAtlas>> = Vec::new();
        for run in 0..2 {
            let mut b = RainBaker::default();
            b.begin_frame(10, 20);
            let mut seq = vec![b.version()];
            while !b.complete() {
                b.bake_tiles(&rom);
                seq.push(b.version());
                let a = b.atlas().expect("baked batch publishes");
                assert!(
                    !arcs.iter().any(|prev| Arc::ptr_eq(prev, &a)),
                    "every publish is a distinct allocation"
                );
                arcs.push(a);
            }
            if run == 0 {
                first = seq;
            } else {
                assert_eq!(seq, first, "rebuilt baker replays the version sequence");
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
        b.begin_frame(10, 20);
        assert_eq!((b.next_tile, b.version()), (tile, ver));
    }
}
