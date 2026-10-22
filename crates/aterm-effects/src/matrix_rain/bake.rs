// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Phosphor `RainBaker`: box-filters the 24x48 1-bit glyph master down to
//! live-cell-metric tiles in one [`SceneAtlas`] (`docs/matrix-rain-design.md`
//! §2). The atlas is an 8x8 grid of `(cell_w, cell_h)` white-RGB
//! straight-alpha tiles, baked progressively (at most
//! [`MAX_RAIN_BAKES_PER_TICK`] per engine tick), version-bumped on every
//! rebake and restarted on a cell-metric change. Coverage is integer
//! fixed-point (denominator `24*48`, exact area weighting), so the atlas bytes
//! are deterministic.

use std::sync::Arc;

use aterm_render::SceneAtlas;

use super::rom::{MASTER_H, MASTER_W, ROM_GLYPHS, RomMaster};

/// Tiles per atlas row/column (8×8 grid of the 64 glyphs).
pub const RAIN_TILE_GRID: usize = 8;
/// Progressive bake budget: at most this many tiles per engine tick, so a
/// cell-metric change never spikes one frame with a 64-tile bake.
pub const MAX_RAIN_BAKES_PER_TICK: usize = 8;

/// Progressive master -> cell-metric tile baker. Resident; the only transient
/// allocation is the published [`SceneAtlas`] snapshot after a bake tick, so
/// a complete atlas is allocation-free.
#[derive(Default)]
pub struct RainBaker {
    cell_w: u16,
    cell_h: u16,
    /// Straight-alpha RGBA8 texels, `(8·cell_w) × (8·cell_h)`.
    rgba: Vec<u8>,
    /// Monotonic; bumped on every rebake batch and every restart, so a
    /// partially-baked atlas re-uploads progressively and the repaint key sees
    /// each batch. Per-instance, because the engine fingerprint folds it and
    /// two engines fed identical inputs must emit identical fingerprints. A
    /// rebuilt engine therefore replays the same sequence, so the GPU texture
    /// cache keys on the published snapshot's `Arc` identity, not on
    /// `(version, w, h)`.
    version: u64,
    /// Next tile index to bake; `>= ROM_GLYPHS` ⇒ complete.
    next_tile: usize,
    published: Option<Arc<SceneAtlas>>,
    dirty: bool,
    /// Per-dest-column source geometry: the ascending `(mx, wx)` box-filter
    /// segments for every dest column, flattened. Depends only on `x` and
    /// `cell_w`, so it is shared by all 64 tiles and rebuilt only when
    /// `cell_w` changes.
    col_seg: Vec<(u32, u32)>,
    /// Per-dest-column offsets into `col_seg` (length `cell_w + 1`).
    col_off: Vec<u32>,
}

impl RainBaker {
    /// Per-tick prologue: a cell-metric change wholesale-restarts the bake
    /// (tiles of the old metric must never be sampled at the new one).
    #[inline]
    pub fn begin_frame(&mut self, cell_w: u16, cell_h: u16) {
        if (cell_w, cell_h) == (self.cell_w, self.cell_h) {
            return;
        }
        self.cell_w = cell_w;
        self.cell_h = cell_h;
        self.restart();
    }

    /// Restart the bake from tile 0 (metric change / config rebuild). Keeps
    /// the buffer allocation when the size is unchanged.
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

    /// Bake up to [`MAX_RAIN_BAKES_PER_TICK`] pending tiles. Returns whether
    /// any tile was baked (each batch bumps the version, so a rebake repaints
    /// and re-uploads).
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

    /// Whether a selective re-bake is sound: a live cell metric and a complete
    /// atlas, so every tile holds exactly what a full bake of the current
    /// master would produce. Before the first geometry `complete()` is
    /// vacuously true (`cell_w == 0`) but there is no buffer to patch, so that
    /// case is excluded.
    #[must_use]
    #[inline]
    pub const fn can_rebake(&self) -> bool {
        self.cell_w != 0 && self.cell_h != 0 && self.next_tile >= ROM_GLYPHS
    }

    /// Re-bake only the tiles named by `mask` (bit `i` = tile `i`) into the
    /// finished atlas. Untouched tiles keep their texels, which match a full
    /// bake as long as their master glyphs did not change; the caller
    /// guarantees that by listing exactly the slots it re-authored. The
    /// published bytes equal a `restart()` plus full bake of the same master.
    ///
    /// The version advances exactly as the wholesale path would (one restart
    /// plus one bump per progressive batch), even though fewer tiles are
    /// baked, so the frame fingerprint sequence is identical.
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

    /// Monotonic atlas version (folded into the frame fingerprint).
    #[must_use]
    #[inline]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// The published atlas snapshot, rebuilt lazily after a bake batch.
    /// `None` until the first bake — rain-free frames carry no atlas.
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

    /// Atlas texel origin of `glyph`'s tile (its rect is `cell_w × cell_h`).
    #[must_use]
    #[inline]
    pub const fn tile_origin(&self, glyph: u32) -> (u16, u16) {
        let g = glyph as usize % ROM_GLYPHS;
        (
            ((g % RAIN_TILE_GRID) * self.cell_w as usize) as u16,
            ((g / RAIN_TILE_GRID) * self.cell_h as usize) as u16,
        )
    }

    /// Box-filter one glyph's 24x48 master into its `(cell_w, cell_h)` tile
    /// with exact integer area weighting. Dest pixel `x` covers master x-range
    /// `[x*24/cw, (x+1)*24/cw)`; scaling both axes by `(cw, ch)` keeps every
    /// overlap an integer, and the per-pixel denominator is the constant
    /// `24*48`.
    fn bake_tile(&mut self, rom: &RomMaster, tile: usize) {
        let (cw, ch) = (usize::from(self.cell_w), usize::from(self.cell_h));
        if cw == 0 || ch == 0 {
            return;
        }
        let atlas_w = cw * RAIN_TILE_GRID;
        let (ox, oy) = ((tile % RAIN_TILE_GRID) * cw, (tile / RAIN_TILE_GRID) * ch);
        const DEN: u32 = (MASTER_W * MASTER_H) as u32;

        // The per-dest-column source range and horizontal weights `wx` depend
        // only on `x` and `cw`, so the ascending `(mx, wx)` segments are built
        // once per `cw` and shared across all 64 tiles. `mx1 = ((x+1)*MW -
        // 1)/cw <= MW-1` always, so no clamp is needed. Iterating ascending
        // keeps the `num` accumulation order fixed.
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

        // Per-dest-row source scratch `(wy, row_bits)`, indexed by `my`; it
        // depends only on `(y, my)`, so it is computed once per dest row. Only
        // `my0..=my_end` is read, so no per-row zeroing.
        let mut src: [(u32, u32); MASTER_H] = [(0, 0); MASTER_H];
        for y in 0..ch {
            // Master rows overlapping dest row `y`, with overlaps in 1/ch units.
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
                // num <= DEN: a dest pixel's scaled extent is exactly
                // MASTER_W*MASTER_H = DEN at full coverage, so the rounded
                // alpha is at most 255 without a clamp.
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

    /// Max tile alpha for glyph `g` in a finished baker.
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

    /// All 64 tiles carry ink at both a small and a retina cell metric.
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

    /// Versions are deterministic per instance, so a rebuilt baker replays
    /// its predecessor's sequence; every publish is still a fresh allocation,
    /// so the GPU cache's `Arc`-identity key never aliases.
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

    /// A same-metric `begin_frame` is a no-op (no restart churn per tick).
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
