// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PHOSPHOR `RainBaker` — box-filters the 24×48 1-bit glyph master down to
//! live-cell-metric tiles in ONE [`SceneAtlas`] (matrix-rain design §2): an
//! 8×8 tile grid of `(cell_w, cell_h)` white-RGB straight-alpha tiles, baked
//! progressively (≤ [`MAX_RAIN_BAKES_PER_TICK`] tiles per engine tick),
//! version-bumped on every rebake, wholesale-restarted on a cell-metric
//! change. Coverage is integer fixed-point ONLY (denominator `24·48` — exact
//! area weighting, no floats), so the atlas bytes are deterministic.

use std::sync::Arc;

use aterm_render::SceneAtlas;

use super::rom::{MASTER_H, MASTER_W, ROM_GLYPHS, RomMaster};

/// Tiles per atlas row/column (8×8 grid of the 64 glyphs).
pub const RAIN_TILE_GRID: usize = 8;
/// Progressive bake budget: at most this many tiles per engine tick, so a
/// cell-metric change never spikes one frame with a 64-tile bake.
pub const MAX_RAIN_BAKES_PER_TICK: usize = 8;

/// Progressive master → cell-metric tile baker. Resident; the only transient
/// allocation is the published [`SceneAtlas`] snapshot after a bake tick
/// (steady state — atlas complete — is allocation-free).
#[derive(Default)]
pub struct RainBaker {
    cell_w: u16,
    cell_h: u16,
    /// Straight-alpha RGBA8 texels, `(8·cell_w) × (8·cell_h)`.
    rgba: Vec<u8>,
    /// Monotonic; bumped on EVERY rebake batch and every restart, so a
    /// partially-baked atlas re-uploads progressively and the repaint key
    /// sees each batch. PER-INSTANCE on purpose: the engine fingerprint folds
    /// this version, and cross-engine determinism (two engines fed identical
    /// inputs emit identical fingerprints) is a pinned contract — so the
    /// sequence must be a pure function of this baker's history, never a
    /// process-global counter. The GPU texture cache therefore must NOT key
    /// on `(version, w, h)` alone (a REBUILT engine deterministically replays
    /// the same sequence and would alias its predecessor's stale texels): it
    /// keys on the published snapshot's Arc IDENTITY, which is unique per
    /// publish by construction (split-pane audit).
    version: u64,
    /// Next tile index to bake; `>= ROM_GLYPHS` ⇒ complete.
    next_tile: usize,
    published: Option<Arc<SceneAtlas>>,
    dirty: bool,
    /// Reused y-invariant per-dest-column source geometry: the ascending
    /// `(mx, wx)` box-filter segments for every dest column, flat. Depends
    /// only on `x` and `cell_w`, so it is built once per bake and shared
    /// across all 64 tiles (rebuilt only when `cell_w` changes).
    col_seg: Vec<(u32, u32)>,
    /// Per-dest-column offsets into `col_seg` (length `cell_w + 1`).
    col_off: Vec<u32>,
}

impl RainBaker {
    /// Per-tick prologue: a cell-metric change wholesale-restarts the bake
    /// (tiles of the old metric must never be sampled at the new one).
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
    pub fn complete(&self) -> bool {
        self.next_tile >= ROM_GLYPHS || self.cell_w == 0 || self.cell_h == 0
    }

    /// Bake up to [`MAX_RAIN_BAKES_PER_TICK`] pending tiles. Returns whether
    /// any tile was baked (each batch bumps the version — a rebake must
    /// repaint AND re-upload).
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

    /// Whether a SELECTIVE re-bake is sound right now: a live cell metric and a
    /// complete atlas, i.e. every tile currently holds exactly the texels a full
    /// bake of the current master would produce for it. Before the first
    /// geometry `complete()` is vacuously true (`cell_w == 0`) but there is no
    /// buffer to patch, so that case is excluded here.
    #[must_use]
    pub fn can_rebake(&self) -> bool {
        self.cell_w != 0 && self.cell_h != 0 && self.next_tile >= ROM_GLYPHS
    }

    /// Re-bake ONLY the tiles named by `mask` (bit `i` ⇒ tile `i`) into the
    /// finished atlas. The tiles left alone keep the texels they already hold,
    /// which are the texels a full bake would produce for them as long as their
    /// master glyphs did not change — the caller owns that half of the
    /// invariant by re-authoring precisely the slots it lists here. The
    /// published atlas bytes are therefore identical to a `restart()` + full
    /// bake of the same master.
    ///
    /// VERSION ACCOUNTING IS DELIBERATE. The version is folded into the engine's
    /// frame fingerprint, which is a pinned byte-identity contract, so this path
    /// advances it by exactly what the wholesale path advanced it by — one
    /// restart plus one bump per progressive batch — even though it physically
    /// bakes fewer tiles. Monotonic, still changes whenever the bytes change,
    /// and the fingerprint sequence is unmoved.
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
    pub fn version(&self) -> u64 {
        self.version
    }

    /// The published atlas snapshot, rebuilt lazily after a bake batch.
    /// `None` until the first bake — rain-free frames carry no atlas.
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
    pub fn tile_origin(&self, glyph: u32) -> (u16, u16) {
        let g = glyph as usize % ROM_GLYPHS;
        (
            ((g % RAIN_TILE_GRID) * usize::from(self.cell_w)) as u16,
            ((g / RAIN_TILE_GRID) * usize::from(self.cell_h)) as u16,
        )
    }

    /// Box-filter one glyph's 24×48 master into its `(cell_w, cell_h)` tile:
    /// exact integer area weighting. Destination pixel `x` covers master
    /// x-range `[x·24/cw, (x+1)·24/cw)`; scaling both axes by `(cw, ch)`
    /// keeps every overlap an integer, and the per-pixel denominator is the
    /// constant `24·48` (the full source extent of one destination pixel).
    fn bake_tile(&mut self, rom: &RomMaster, tile: usize) {
        let (cw, ch) = (usize::from(self.cell_w), usize::from(self.cell_h));
        if cw == 0 || ch == 0 {
            return;
        }
        let atlas_w = cw * RAIN_TILE_GRID;
        let (ox, oy) = ((tile % RAIN_TILE_GRID) * cw, (tile / RAIN_TILE_GRID) * ch);
        const DEN: u32 = (MASTER_W * MASTER_H) as u32;

        // y-invariant per-dest-column source geometry: `(c/cw, (d-1)/cw)` and
        // the per-source-column horizontal weight `wx` depend only on `x` and
        // `cw`, never `y`, so build the ascending `(mx, wx)` segments once per
        // bake (shared across all 64 tiles; `cw` fixes both the length and the
        // contents). `mx1 = ((x+1)·MW - 1)/cw <= MW-1` always, so the old
        // `.min(MASTER_W-1)` bound was a no-op and is dropped. Iterating the
        // segments ascending keeps the `num` accumulation order byte-identical.
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

        // Per-dest-row source scratch `(wy, row_bits)`, indexed by `my`.
        // `my` and `wy`/`row` depend only on `(y, my)`, not `x`, so compute
        // them once per dest row instead of once per dest pixel. Only the
        // written range `my0..=my_end` is read below, so no per-row zeroing.
        let mut src: [(u32, u32); MASTER_H] = [(0, 0); MASTER_H];
        for y in 0..ch {
            // Master rows overlapping dest row `y`, with overlaps in 1/ch units.
            let (a, b) = (y * MASTER_H, (y + 1) * MASTER_H);
            let (my0, my1) = (a / ch, (b - 1) / ch);
            let my_end = my1.min(MASTER_H - 1);
            for (i, slot) in src[my0..=my_end].iter_mut().enumerate() {
                let my = my0 + i;
                let wy = (b.min((my + 1) * ch) - a.max(my * ch)) as u32;
                *slot = (wy, rom.row(tile, my));
            }
            let src_row = &src[my0..=my_end];
            for x in 0..cw {
                let seg = &self.col_seg[self.col_off[x] as usize..self.col_off[x + 1] as usize];
                let mut num: u32 = 0;
                for &(wy, row) in src_row {
                    for &(mx, wx) in seg {
                        if (row >> mx) & 1 == 1 {
                            num += wx * wy;
                        }
                    }
                }
                // num <= DEN always: a dest pixel's scaled extent is exactly
                // MASTER_W×MASTER_H, so Σwx·Σwy = 24·48 = DEN at full coverage
                // and only set bits contribute ⇒ (num·255 + DEN/2)/DEN <= 255
                // exactly. The old `.min(255)` was provably dead; dropped.
                let alpha = ((num * 255 + DEN / 2) / DEN) as u8;
                let idx = ((oy + y) * atlas_w + ox + x) * 4;
                self.rgba[idx] = 255;
                self.rgba[idx + 1] = 255;
                self.rgba[idx + 2] = 255;
                self.rgba[idx + 3] = alpha;
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

    /// Baking is deterministic byte-for-byte (integer coverage only).
    #[test]
    fn bake_bytes_are_deterministic() {
        let mut a = baked(9, 18);
        let mut b = baked(9, 18);
        assert_eq!(a.atlas().unwrap().rgba, b.atlas().unwrap().rgba);
    }

    /// Progressive discipline: ≤ 8 tiles per bake tick (8 ticks to finish),
    /// with a version bump on every batch and on the metric-change restart.
    #[test]
    fn progressive_bake_and_version_bumps() {
        let rom = rasterize_master();
        let mut b = RainBaker::default();
        b.begin_frame(10, 20);
        let v0 = b.version();
        let mut batches = 0;
        let mut last = v0;
        while !b.complete() {
            let before = b.next_tile;
            assert!(b.bake_tiles(&rom));
            assert!(b.next_tile - before <= MAX_RAIN_BAKES_PER_TICK);
            assert_ne!(b.version(), last, "every batch bumps the version");
            last = b.version();
            batches += 1;
        }
        assert_eq!(batches, ROM_GLYPHS / MAX_RAIN_BAKES_PER_TICK);
        assert!(!b.bake_tiles(&rom), "complete baker bakes nothing");
        // Metric change restarts the bake and bumps the version again.
        b.begin_frame(12, 24);
        assert_ne!(b.version(), last);
        assert!(!b.complete());
        assert!(
            b.atlas().is_none(),
            "no atlas until the first new-bake batch"
        );
    }

    /// SPLIT-PANE AUDIT: versions are deterministic PER INSTANCE (the engine
    /// fingerprint contract), so a rebuilt baker REPLAYS its predecessor's
    /// version sequence — which is exactly why the GPU texture cache must key
    /// on the published snapshot's Arc IDENTITY, never `(version, w, h)`
    /// alone. Pin both halves: the sequences collide, and every publish is a
    /// fresh allocation (per-publish-unique identity).
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
