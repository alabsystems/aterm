// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SPILL-BAND rasterizer — the chrome-band slice of the window-space effects
//! layer, flattened into a host-blittable straight-alpha RGBA buffer so an
//! EMBEDDER can composite a pane's effect overflow (glow rings, flames) OVER
//! its neighbours (the cross-pane effects compositor's engine half).
//!
//! The composed frame already paints the chrome band
//! (`[head][pad_top][grid][2*pad-pad_top]` minus the grid box) with the
//! window-absolute emission streams, but those
//! pixels are OPAQUE theme-bg + light — a host cannot blit them over another
//! pane without painting solid bars. [`SpillBand`] re-rasterizes ONLY the four
//! window-absolute emission streams (`glow_under`, `fire_patch`,
//! `cursor_glow_add`, `glow_halo` — the streams that can leave the grid;
//! everything else is grid-anchored/grid-interior) into four packed strips
//! — **top (incl. head), bottom, left, right** — and solves each covered
//! pixel back to a straight `(rgb, alpha)` pair such that
//!
//! > `over_rgb(band_bg, rgb, alpha) == the composed frame's band byte`
//!
//! holds EXACTLY (the seam-continuity law: a host compositing the buffer
//! source-over onto the pane's own background reproduces the in-frame ring
//! byte-for-byte, so the `.pane` clip line can never show a seam). Over any
//! OTHER background the same pair is the documented source-over approximation
//! of the engine's saturating-add light.
//!
//! The per-pixel kernels are the renderer's own — [`add_sat`]/[`premul_rgb`]
//! for the flat additive quads, [`halo_row_ny`]/[`halo_weight`] +
//! [`over_rgb`] for the radial halos/veils, and the [`fire_field`] functions
//! for the fire patches — replayed in the frame's exact band z-order
//! (glow_under → fire Add → fire Over → cursor_glow_add → halo Add → halo
//! Over), so spill and frame cannot drift.
//!
//! IDENTITY LAW: with `0/0` chrome there is no band — the buffer is empty,
//! the revision never advances, and `update` is a length-check no-op. Frames
//! whose band-relevant inputs are unchanged (typing with a settled,
//! grid-interior glow; idle re-renders) keep the revision and report zero
//! dirty rects, so a host can skip its blit entirely on the engine's word.

use crate::fire_field;
use crate::{
    FireMode, FirePatch, GlowQuad, HaloMode, RainHalo, RenderInput, Renderer, add_sat, halo_row_ny,
    halo_weight, over_rgb, premul_rgb,
};

/// The band geometry a spill buffer was laid out for: chrome + padded-frame
/// dims. `pad_top` is explicit because changing only the asymmetric grid origin
/// preserves the frame dimensions while changing every horizontal strip split.
/// Any change re-derives the strips (and is the ONE event that may move
/// [`SpillBand::rgba`]'s allocation — the host's pointer-stability contract).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct SpillGeom {
    pad: usize,
    pad_top: usize,
    head: usize,
    frame_w: usize,
    frame_h: usize,
}

/// One strip's rect in FRAME-ABSOLUTE device px.
#[derive(Clone, Copy, Default)]
struct StripRect {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

impl SpillGeom {
    /// Grid content's Y origin: `pad_top + head` (the renderer's `grid_top`).
    fn grid_top(&self) -> usize {
        self.pad_top + self.head
    }

    /// The trailing band absorbs the top-pad reduction so the fixed frame
    /// extent still contains exactly `2 * pad` vertical padding pixels.
    fn trailing_h(&self) -> usize {
        self.pad.saturating_mul(2).saturating_sub(self.pad_top)
    }

    fn grid_bottom(&self) -> usize {
        self.frame_h.saturating_sub(self.trailing_h())
    }

    /// The four band strips in the PACKED BUFFER ORDER — top (incl. head),
    /// bottom, left, right — each row-major, concatenated. Zero-area strips
    /// (e.g. `pad == 0` with a head band) stay in the layout with 0 bytes.
    fn strips(&self) -> [StripRect; 4] {
        let grid_bottom = self.grid_bottom();
        let grid_h = grid_bottom.saturating_sub(self.grid_top());
        [
            StripRect {
                x: 0,
                y: 0,
                w: self.frame_w,
                h: self.grid_top(),
            },
            StripRect {
                x: 0,
                y: grid_bottom,
                w: self.frame_w,
                h: self.trailing_h(),
            },
            StripRect {
                x: 0,
                y: self.grid_top(),
                w: self.pad,
                h: grid_h,
            },
            StripRect {
                x: self.frame_w - self.pad,
                y: self.grid_top(),
                w: self.pad,
                h: grid_h,
            },
        ]
    }

    /// Total band pixels across the four strips.
    fn band_px(&self) -> usize {
        self.strips().iter().map(|s| s.w * s.h).sum()
    }

    /// Whether a frame-space rect reaches the band: non-empty after the frame
    /// clip and NOT fully contained in the grid box. Mirrors the renderer's
    /// clipping so a grid-interior emission is invisible to the spill
    /// fingerprint (typing-only frames must not advance the revision).
    fn rect_touches_band(&self, x: usize, y: usize, w: usize, h: usize) -> bool {
        let xe = (x + w).min(self.frame_w);
        let ye = (y + h).min(self.frame_h);
        if x >= xe || y >= ye {
            return false;
        }
        !(x >= self.pad
            && y >= self.grid_top()
            && xe <= self.frame_w - self.pad
            && ye <= self.grid_bottom())
    }
}

/// Union of two coverages as source-over alpha accumulation
/// (`a' = s + a·(255−s)/255`, round-half) — the alpha bookkeeping that runs
/// alongside the colour replay. Additive light carries its max channel as a
/// pseudo-coverage (the brightest light occludes most), veils their real one.
#[inline]
fn cover_union(a: u8, s: u8) -> u8 {
    (u32::from(s) + (u32::from(a) * (255 - u32::from(s)) + 127) / 255) as u8
}

/// The max RGB channel of a premultiplied light colour — the pseudo-alpha an
/// additive emission contributes to the spill coverage.
#[inline]
fn max_channel(premul: u32) -> u8 {
    ((premul >> 16) & 0xff)
        .max((premul >> 8) & 0xff)
        .max(premul & 0xff) as u8
}

/// Solve one band pixel back to a straight `(r, g, b, a)` such that
/// `over_rgb(base, rgb, a) == c` per channel — the EXACT-over-own-bg law the
/// byte-parity property pins. `c` is the pixel after the band's op replay
/// over `base`; `a0` is the accumulated coverage.
///
/// Derivation (per channel, `out(s) = (s·a + b·(255−a) + 127) / 255`):
/// `out` is monotone in `s` with steps ≤ 1, so every target in
/// `[out(0), out(255)]` is hit by some `s`; both bracket bounds widen
/// monotonically in `a`, so lifting `a` to the per-channel minima below makes
/// every channel solvable with ONE shared alpha (`a = 255` always brackets —
/// the saturated-light fallback). The natural `a0` is kept when feasible so
/// the spill stays translucent light, not paint.
fn solve_spill_px(base: u32, c: u32, a0: u8) -> [u8; 4] {
    if a0 == 0 {
        debug_assert_eq!(c, base, "untouched spill pixel must still hold the band bg");
        return [0, 0, 0, 0];
    }
    let b = [(base >> 16) & 0xff, (base >> 8) & 0xff, base & 0xff];
    let t = [(c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff];
    let mut a = u32::from(a0);
    for k in 0..3 {
        let need = if t[k] > b[k] {
            // Brightened past the bg: need out(255) ≥ C ⇔ a ≥ ⌈(255(C−b)−127)/(255−b)⌉.
            (255 * (t[k] - b[k]) - 127).div_ceil(255 - b[k])
        } else if t[k] < b[k] {
            // Darkened below the bg (veil/ink): need out(0) ≤ C ⇔ a ≥ 255 − (255C+127)/b.
            255 - (255 * t[k] + 127) / b[k]
        } else {
            0
        };
        a = a.max(need);
    }
    let a = a.min(255);
    let mut out = [0u8; 4];
    out[3] = a as u8;
    for k in 0..3 {
        // Smallest s with out(s) == C; minimality + the ≤1 output step make the
        // floor-division round-trip exact (debug-asserted).
        let s = (255 * t[k])
            .saturating_sub(b[k] * (255 - a) + 127)
            .div_ceil(a)
            .min(255);
        debug_assert_eq!(
            (s * a + b[k] * (255 - a) + 127) / 255,
            t[k],
            "spill solve must reproduce the band byte exactly"
        );
        out[k] = s as u8;
    }
    out
}

/// The engine half of the cross-pane effects compositor: a persistent,
/// straight-alpha RGBA rasterization of the effect emissions falling in the
/// chrome band, plus the change signal (revision + packed dirty rects) a host
/// needs to blit it with zero per-frame polling cost. See the module docs for
/// the layout and the seam-continuity law. Owned by the wasm terminals and
/// fed once per rendered frame via [`SpillBand::update`].
#[derive(Default)]
pub struct SpillBand {
    geom: SpillGeom,
    /// The band's base bg (frame default-bg RGB) the solve is exact over.
    base_bg: u32,
    /// Whether `HaloMode::Over` VEILS are EXCLUDED from the spill (inverted so
    /// the derived default — `false` = veils included — keeps the byte-parity
    /// law universal out of the box). Excluding scopes the spill to pure
    /// additive light + fire ink — the light-theme-smoke policy escape.
    veils_excluded: bool,
    rev: u32,
    /// The exported straight-alpha RGBA strips (see `SpillGeom::strips` for
    /// the packing). Allocation moves ONLY on a geometry change — the host's
    /// pointer-stability contract.
    rgba: Vec<u8>,
    /// This update's dirty rects, packed `x,y,w,h` frame-absolute device px.
    rects: Vec<i32>,
    // Replay planes (band colour over base_bg + accumulated coverage),
    // strip-packed like `rgba`; retained so a change-frame allocates nothing.
    color: Vec<u32>,
    cover: Vec<u8>,
    // The previous frame's band-clipped emissions — the content fingerprint
    // that keeps the revision still on typing-only/settled frames.
    prev_glow_under: Vec<GlowQuad>,
    prev_fire: Vec<FirePatch>,
    prev_glow: Vec<GlowQuad>,
    prev_halo: Vec<RainHalo>,
    // Scratch for the current frame's clip (swapped into prev_* on a raster).
    cur_glow_under: Vec<GlowQuad>,
    cur_fire: Vec<FirePatch>,
    cur_glow: Vec<GlowQuad>,
    cur_halo: Vec<RainHalo>,
}

impl SpillBand {
    pub fn new() -> Self {
        Self::default()
    }

    /// Monotone band-content revision: advances ONLY when the exported bytes
    /// changed (new/moved/vanished band emissions, a band bg change under
    /// live content, a veil-policy flip with veils present, a resize with
    /// content). The host's dirty signal — unchanged rev ⇒ skip the blit.
    pub fn rev(&self) -> u32 {
        self.rev
    }

    /// The LAST update's dirty rects, packed `x,y,w,h` (frame-absolute device
    /// px), 4 `i32`s per rect. Empty on a no-change frame. Content changes
    /// report the prev∪cur emission bbox clipped per strip; geometry/bg/policy
    /// changes report the full band.
    pub fn rects(&self) -> &[i32] {
        &self.rects
    }

    /// The straight-alpha RGBA strip buffer (see the module docs for the
    /// packing). Empty at 0/0 chrome — the identity law.
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// Include `HaloMode::Over` veils in the spill (default `true`). Applies
    /// from the next [`Self::update`]; flipping it with veils in the band
    /// re-rasters and advances the revision (the clipped-stream fingerprint
    /// sees the veils appear/vanish), with no veils present it is free.
    pub fn set_include_veils(&mut self, on: bool) {
        self.veils_excluded = !on;
    }

    /// Rasterize/refresh the spill for the frame `input` the renderer just
    /// composed. Cheap when nothing band-relevant changed: clip + compare the
    /// four emission streams, then return with the revision (and buffer
    /// bytes/pointer) untouched.
    pub fn update(&mut self, renderer: &Renderer, input: &RenderInput) {
        let geom = SpillGeom {
            pad: renderer.pad(),
            pad_top: renderer.pad_top(),
            head: renderer.head(),
            frame_w: 0,
            frame_h: 0,
        };
        // Identity law: 0/0 chrome has no band. Skip even the frame-size
        // derivation; a chrome→0 transition drops the buffers (content
        // vanished ⇒ one revision tick so the host clears its overlay).
        if geom.pad == 0 && geom.head == 0 {
            self.rects.clear();
            if self.geom != geom {
                if self.has_prev_content() {
                    self.rev = self.rev.wrapping_add(1);
                }
                self.clear_content();
                self.rgba = Vec::new();
                self.color = Vec::new();
                self.cover = Vec::new();
                self.geom = geom;
            }
            return;
        }
        let (frame_w, frame_h) = renderer.frame_size(input.rows, input.cols);
        let geom = SpillGeom {
            frame_w,
            frame_h,
            ..geom
        };
        let base_bg = renderer.frame_bg(input) & 0x00FF_FFFF;

        // Band-clip the four window-absolute emission streams (mirroring the
        // renderer's `row >= rows` skip so the fingerprint matches what the
        // frame actually drew). Grid-interior emissions vanish here — the
        // typing-only stability guarantee.
        self.cur_glow_under.clear();
        self.cur_glow_under.extend(
            input
                .glow_under
                .iter()
                .filter(|q| {
                    (q.row as usize) < input.rows
                        && geom.rect_touches_band(
                            q.x as usize,
                            q.y as usize,
                            q.w as usize,
                            q.h as usize,
                        )
                })
                .copied(),
        );
        self.cur_fire.clear();
        self.cur_fire.extend(
            input
                .fire_patch
                .iter()
                .filter(|q| {
                    (q.row as usize) < input.rows
                        && geom.rect_touches_band(
                            q.x as usize,
                            q.y as usize,
                            q.w as usize,
                            q.h as usize,
                        )
                })
                .copied(),
        );
        self.cur_glow.clear();
        self.cur_glow.extend(
            input
                .cursor_glow_add
                .iter()
                .filter(|q| {
                    (q.row as usize) < input.rows
                        && geom.rect_touches_band(
                            q.x as usize,
                            q.y as usize,
                            q.w as usize,
                            q.h as usize,
                        )
                })
                .copied(),
        );
        // Excluded veils are dropped AT THE CLIP, so the veil policy is part
        // of the stream fingerprint: a flip with veils present re-rasters, a
        // flip without them is a no-op.
        let include_veils = !self.veils_excluded;
        self.cur_halo.clear();
        self.cur_halo.extend(
            input
                .glow_halo
                .iter()
                .filter(|q| {
                    (q.row as usize) < input.rows
                        && (include_veils || q.mode == HaloMode::Add)
                        && geom.rect_touches_band(
                            q.x as usize,
                            q.y as usize,
                            q.w as usize,
                            q.h as usize,
                        )
                })
                .copied(),
        );

        let geom_changed = geom != self.geom;
        let bg_changed = base_bg != self.base_bg;
        let streams_changed = self.cur_glow_under != self.prev_glow_under
            || self.cur_fire != self.prev_fire
            || self.cur_glow != self.prev_glow
            || self.cur_halo != self.prev_halo;
        if !(geom_changed || bg_changed || streams_changed) {
            // Typing-only / settled / idle frame: rev, rects and bytes all still.
            self.rects.clear();
            return;
        }
        if geom_changed {
            // The ONE event that may move the exported allocation. Zeroed =
            // fully transparent until (re)rastered below.
            let px = geom.band_px();
            self.rgba.clear();
            self.rgba.resize(px * 4, 0);
            self.color.clear();
            self.color.resize(px, 0);
            self.cover.clear();
            self.cover.resize(px, 0);
        }
        self.geom = geom;
        self.base_bg = base_bg;

        if !self.has_prev_content() && !self.has_cur_content() {
            // A bg/geometry change under an all-transparent band: no visible
            // bytes changed, so the revision (the host's blit signal) holds.
            self.rects.clear();
            self.adopt_cur();
            return;
        }

        self.emit_dirty_rects(geom_changed || bg_changed);
        self.rasterize();
        self.adopt_cur();
        self.rev = self.rev.wrapping_add(1);
    }

    fn has_prev_content(&self) -> bool {
        !(self.prev_glow_under.is_empty()
            && self.prev_fire.is_empty()
            && self.prev_glow.is_empty()
            && self.prev_halo.is_empty())
    }

    fn has_cur_content(&self) -> bool {
        !(self.cur_glow_under.is_empty()
            && self.cur_fire.is_empty()
            && self.cur_glow.is_empty()
            && self.cur_halo.is_empty())
    }

    fn clear_content(&mut self) {
        self.prev_glow_under.clear();
        self.prev_fire.clear();
        self.prev_glow.clear();
        self.prev_halo.clear();
    }

    /// Swap the just-clipped streams into the fingerprint (allocation-reusing).
    fn adopt_cur(&mut self) {
        std::mem::swap(&mut self.prev_glow_under, &mut self.cur_glow_under);
        std::mem::swap(&mut self.prev_fire, &mut self.cur_fire);
        std::mem::swap(&mut self.prev_glow, &mut self.cur_glow);
        std::mem::swap(&mut self.prev_halo, &mut self.cur_halo);
    }

    /// Pack this update's dirty rects: the full band on a geometry/bg/policy
    /// event, else the prev∪cur emission bbox ∩ each strip (prev so vanished
    /// light is repainted away, cur so new light lands).
    fn emit_dirty_rects(&mut self, full_band: bool) {
        self.rects.clear();
        let geom = self.geom;
        let mut push = |x0: usize, y0: usize, xe: usize, ye: usize| {
            self.rects.extend_from_slice(&[
                x0 as i32,
                y0 as i32,
                (xe - x0) as i32,
                (ye - y0) as i32,
            ]);
        };
        if full_band {
            for s in geom.strips() {
                if s.w * s.h > 0 {
                    push(s.x, s.y, s.x + s.w, s.y + s.h);
                }
            }
            return;
        }
        // Frame-clipped bbox over every prev∪cur band emission rect.
        let mut bb: Option<(usize, usize, usize, usize)> = None;
        let mut grow = |x: usize, y: usize, w: usize, h: usize| {
            let xe = (x + w).min(geom.frame_w);
            let ye = (y + h).min(geom.frame_h);
            if x >= xe || y >= ye {
                return;
            }
            bb = Some(match bb {
                None => (x, y, xe, ye),
                Some((bx, by, bxe, bye)) => (bx.min(x), by.min(y), bxe.max(xe), bye.max(ye)),
            });
        };
        for q in self.prev_glow_under.iter().chain(&self.cur_glow_under) {
            grow(q.x as usize, q.y as usize, q.w as usize, q.h as usize);
        }
        for q in self.prev_fire.iter().chain(&self.cur_fire) {
            grow(q.x as usize, q.y as usize, q.w as usize, q.h as usize);
        }
        for q in self.prev_glow.iter().chain(&self.cur_glow) {
            grow(q.x as usize, q.y as usize, q.w as usize, q.h as usize);
        }
        for q in self.prev_halo.iter().chain(&self.cur_halo) {
            grow(q.x as usize, q.y as usize, q.w as usize, q.h as usize);
        }
        let Some((bx, by, bxe, bye)) = bb else {
            return;
        };
        for s in geom.strips() {
            let x0 = bx.max(s.x);
            let y0 = by.max(s.y);
            let xe = bxe.min(s.x + s.w);
            let ye = bye.min(s.y + s.h);
            if x0 < xe && y0 < ye {
                push(x0, y0, xe, ye);
            }
        }
    }

    /// Replay the band's op sequence over `base_bg` per strip — the frame's
    /// exact z-order (glow_under → fire Add → fire Over → cursor_glow_add →
    /// halo Add → halo Over) — then solve every pixel to the exported
    /// straight-alpha pair. Writes IN PLACE (no allocation, stable pointer).
    fn rasterize(&mut self) {
        let base = self.base_bg;
        self.color.fill(base);
        self.cover.fill(0);
        let pad = self.geom.pad;
        let mut off = 0usize;
        for s in self.geom.strips() {
            let n = s.w * s.h;
            if n == 0 {
                continue;
            }
            let color = &mut self.color[off..off + n];
            let cover = &mut self.cover[off..off + n];
            replay_flat_add(color, cover, s, &self.cur_glow_under);
            replay_fire(color, cover, s, pad, &self.cur_fire);
            replay_flat_add(color, cover, s, &self.cur_glow);
            replay_halo(color, cover, s, &self.cur_halo);
            off += n;
        }
        for ((c, a), out) in self
            .color
            .iter()
            .zip(&self.cover)
            .zip(self.rgba.as_chunks_mut::<4>().0)
        {
            out.copy_from_slice(&solve_spill_px(base, *c, *a));
        }
    }
}

/// Strip-clip a frame-space emission rect; `None` when it misses the strip.
#[inline]
fn clip_to_strip(
    s: StripRect,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
) -> Option<(usize, usize, usize, usize)> {
    let x0 = x.max(s.x);
    let y0 = y.max(s.y);
    let xe = (x + w).min(s.x + s.w);
    let ye = (y + h).min(s.y + s.h);
    (x0 < xe && y0 < ye).then_some((x0, y0, xe, ye))
}

/// The flat additive quads (`glow_under`, `cursor_glow_add`): the
/// `draw_flat_add` contract — window-absolute premultiplied [`add_sat`] light
/// — restricted to one strip. Pure per-pixel functions of absolute coords, so
/// the strip clip is byte-identical to the frame's own rasterization.
fn replay_flat_add(color: &mut [u32], cover: &mut [u8], s: StripRect, quads: &[GlowQuad]) {
    for q in quads {
        let Some((x0, y0, xe, ye)) =
            clip_to_strip(s, q.x as usize, q.y as usize, q.w as usize, q.h as usize)
        else {
            continue;
        };
        let ap = max_channel(q.color);
        for y in y0..ye {
            let row = (y - s.y) * s.w;
            for x in x0..xe {
                let i = row + (x - s.x);
                color[i] = add_sat(color[i], q.color);
                cover[i] = cover_union(cover[i], ap);
            }
        }
    }
}

/// The radial halos (`glow_halo`): the `draw_radial_add` contract — Add quads
/// then Over veils, per-pixel weight from the SHARED [`halo_row_ny`]/
/// [`halo_weight`] kernels — restricted to one strip. (The frame loop's
/// ellipse-span clamp only skips zero-weight pixels, so omitting it here is
/// byte-identical.)
fn replay_halo(color: &mut [u32], cover: &mut [u8], s: StripRect, quads: &[RainHalo]) {
    for mode in [HaloMode::Add, HaloMode::Over] {
        for q in quads.iter().filter(|q| q.mode == mode) {
            let Some((x0, y0, xe, ye)) =
                clip_to_strip(s, q.x as usize, q.y as usize, q.w as usize, q.h as usize)
            else {
                continue;
            };
            let (cx, cy) = (q.cx as i32, q.cy as i32);
            let rx2 = (q.rx as i32) * (q.rx as i32);
            let ry2 = (q.ry as i32) * (q.ry as i32);
            // Mirror the frame loop's [`HaloMode::Over`] alpha CEILING so the
            // exported band stays byte-continuous with the in-frame veil (`0`
            // high byte == uncapped, i.e. every historical veil is unchanged).
            let over_cap = crate::halo_over_cap(q.color);
            for y in y0..ye {
                let ny = halo_row_ny(y as i32 - cy, ry2);
                if ny >= 256 {
                    continue;
                }
                let row = (y - s.y) * s.w;
                for x in x0..xe {
                    let wt = halo_weight(x as i32 - cx, ny, rx2);
                    if wt == 0 {
                        continue;
                    }
                    let wt = wt.min(255) as u8;
                    let i = row + (x - s.x);
                    match mode {
                        HaloMode::Add => {
                            let pm = premul_rgb(q.color, wt);
                            color[i] = add_sat(color[i], pm);
                            cover[i] = cover_union(cover[i], max_channel(pm));
                        }
                        HaloMode::Over => {
                            let wt = wt.min(over_cap);
                            color[i] = over_rgb(color[i], q.color, wt);
                            cover[i] = cover_union(cover[i], wt);
                        }
                    }
                }
            }
        }
    }
}

/// The fire field (`fire_patch`): the `draw_fire_patch` contract — Add
/// patches then Over patches, per-pixel [`fire_field`] evaluation at ABSOLUTE
/// window coords (the field is a pure function of them, so a strip-clipped
/// sweep is byte-identical) — restricted to one strip. `pad` feeds
/// `top_fade_y` exactly as the frame does (the byte-identity + GPU-parity
/// anchor).
fn replay_fire(color: &mut [u32], cover: &mut [u8], s: StripRect, pad: usize, quads: &[FirePatch]) {
    for mode in [FireMode::Add, FireMode::Over] {
        for q in quads.iter().filter(|q| q.mode == mode) {
            let Some((x0, y0, xe, ye)) =
                clip_to_strip(s, q.x as usize, q.y as usize, q.w as usize, q.h as usize)
            else {
                continue;
            };
            let fp = fire_field::FireFieldParams {
                base_y: q.base_y as i32,
                peak_h: q.peak_h as i32,
                phase: q.phase,
                temp: q.temp as i32,
                strength: q.strength as i32,
                lean: q.lean as i32,
                cov_cap: q.cov_cap as i32,
                cell_h: q.cell_h as i32,
                top_fade_y: pad as i32,
            };
            let pc = fire_field::fire_precomp(&fp);
            for y in y0..ye {
                let tf = fire_field::fire_top_fade(y as i32, &fp);
                let mut sampler = fire_field::FireRow::new(y as i32, x0 as i32, &fp, &pc);
                let row = (y - s.y) * s.w;
                for x in x0..xe {
                    let c = sampler.core(x as i32);
                    let i = row + (x - s.x);
                    match mode {
                        FireMode::Add => {
                            let pm = fire_field::fire_shade_add(&c, &fp, tf);
                            if pm != 0 {
                                color[i] = add_sat(color[i], pm);
                                cover[i] = cover_union(cover[i], max_channel(pm));
                            }
                        }
                        FireMode::Over => {
                            let (rgb, a) = fire_field::fire_shade_over(&c, &fp, tf);
                            if a != 0 {
                                color[i] = over_rgb(color[i], rgb, a);
                                cover[i] = cover_union(cover[i], a);
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::{Theme, WindowCpu};
    use aterm_core::terminal::Terminal;

    /// The exact-over-own-bg solver law, in isolation: for every (bg byte,
    /// target byte) pair — brightened light, darkening veils, saturation,
    /// identity — and several natural coverages, the solved straight pair
    /// must round-trip through the engine's own `over_rgb` to the target
    /// EXACTLY. This is the algebra the byte-parity property stands on.
    #[test]
    fn solver_reproduces_band_bytes_exactly_across_the_lattice() {
        for b in (0..=255u32).step_by(3) {
            for t in (0..=255u32).step_by(3) {
                for a0 in [1u8, 37, 128, 255] {
                    let base = (b << 16) | (b << 8) | b;
                    let c = (t << 16) | (t << 8) | t;
                    let out = solve_spill_px(base, c, a0);
                    let composed = over_rgb(
                        base,
                        (u32::from(out[0]) << 16) | (u32::from(out[1]) << 8) | u32::from(out[2]),
                        out[3],
                    );
                    assert_eq!(
                        composed, c,
                        "solve must be over_rgb-exact for b={b} t={t} a0={a0}"
                    );
                }
            }
        }
        // Mixed-channel pairs: one shared alpha must satisfy asymmetric
        // channels (a lifts to the most demanding channel; the others re-solve).
        for (base, c) in [
            (0x0010_2030u32, 0x00FF_E0B0u32),
            (0x00F0_F0F0, 0x0010_2030),
            (0x0080_8080, 0x0080_FF00),
            (0x0011_1318, 0x0011_1318),
        ] {
            for a0 in [1u8, 90, 255] {
                let out = solve_spill_px(base, c, a0);
                let composed = over_rgb(
                    base,
                    (u32::from(out[0]) << 16) | (u32::from(out[1]) << 8) | u32::from(out[2]),
                    out[3],
                );
                assert_eq!(composed, c, "mixed-channel solve for {base:06x}->{c:06x}");
            }
        }
    }

    /// Walk every band pixel of the composed frame and assert the parity law:
    /// `over_rgb(base, spill_rgb, spill_a) == frame_band_rgb` (transparent
    /// spill pixels must sit on an untouched-bg frame byte). Returns the
    /// per-strip count of lit (alpha > 0) spill pixels for non-vacuity checks.
    fn assert_band_parity(
        r: &mut Renderer,
        input: &RenderInput,
        spill: &SpillBand,
        base: u32,
    ) -> [usize; 4] {
        let mut win = WindowCpu::new();
        let view = r.render_input_cached(&mut win, input);
        let (w, h) = (view.width(), view.height());
        let pixels = view.pixels();
        let geom = SpillGeom {
            pad: r.pad(),
            pad_top: r.pad_top(),
            head: r.head(),
            frame_w: w,
            frame_h: h,
        };
        let buf = spill.rgba();
        assert_eq!(buf.len(), geom.band_px() * 4, "buffer sized to the band");
        let mut lit = [0usize; 4];
        let mut off = 0usize;
        for (si, s) in geom.strips().iter().enumerate() {
            for yy in 0..s.h {
                for xx in 0..s.w {
                    let px = &buf[(off + yy * s.w + xx) * 4..][..4];
                    let (x, y) = (s.x + xx, s.y + yy);
                    let frame = pixels[y * w + x] & 0x00FF_FFFF;
                    let composed = if px[3] == 0 {
                        base
                    } else {
                        lit[si] += 1;
                        over_rgb(
                            base,
                            (u32::from(px[0]) << 16) | (u32::from(px[1]) << 8) | u32::from(px[2]),
                            px[3],
                        )
                    };
                    assert_eq!(
                        composed, frame,
                        "spill ∘ bg must equal the frame band at ({x},{y}) in strip {si}"
                    );
                }
            }
            off += s.w * s.h;
        }
        lit
    }

    /// THE byte-parity property: compose(spill OVER theme bg) equals the
    /// composed padded frame's band region byte-for-byte, with all four
    /// window-absolute emission streams active (flat adds, fire Add + Over
    /// ink, radial Add halos, Over veils) straddling all four strips AND the
    /// grid edge — on a dark theme and a light one (saturation clamps + the
    /// darkening-veil solve are theme-dependent paths).
    #[test]
    fn spill_band_composes_byte_identical_over_the_frame_band() {
        let dark = Theme::default();
        let light = Theme {
            fg: 0x001A_1A1A,
            bg: 0x00F5_F5F0,
            cursor: 0x0033_3333,
            selection: 0x00B0_C4DE,
        };
        for theme in [dark, light] {
            let Some(mut r) = Renderer::from_system(16.0, theme) else {
                return; // no usable system font in this environment
            };
            r.set_pad(10);
            r.set_pad_top(3);
            r.set_head(24);
            let mut term = Terminal::new(6, 20);
            let mut input = term.cell_frame(6, 20);
            let (fw, fh) = r.frame_size(6, 20);
            // Flat additive quads: head band + one straddling grid_top AND the
            // left grid edge (its grid-interior part must stay frame-only).
            input.glow_under = vec![
                GlowQuad {
                    row: 0,
                    x: 2,
                    y: 4,
                    w: 40,
                    h: 14,
                    color: 0x0020_1008,
                    // ADDITIVE light (see `GlowQuad::alpha`).
                    alpha: 0,
                },
                GlowQuad {
                    row: 0,
                    x: 0,
                    y: 20,
                    w: 18,
                    h: 20,
                    color: 0x0008_1020,
                    // ADDITIVE light (see `GlowQuad::alpha`).
                    alpha: 0,
                },
            ];
            // Fire field patches in the head band, Add then Over ink.
            input.fire_patch = vec![
                FirePatch {
                    row: 0,
                    x: 0,
                    y: 4,
                    w: 80,
                    h: 30,
                    base_y: 44,
                    peak_h: 48,
                    phase: 977_331,
                    temp: 210,
                    strength: 230,
                    lean: 6,
                    cov_cap: 210,
                    cell_h: 18,
                    mode: FireMode::Add,
                },
                FirePatch {
                    row: 0,
                    x: 30,
                    y: 0,
                    w: 60,
                    h: 30,
                    base_y: 40,
                    peak_h: 40,
                    phase: 512_101,
                    temp: 180,
                    strength: 200,
                    lean: -4,
                    cov_cap: 190,
                    cell_h: 18,
                    mode: FireMode::Over,
                },
            ];
            // Aurora quads: right strip + bottom strip.
            input.cursor_glow_add = vec![
                GlowQuad {
                    row: 0,
                    x: (fw - 8) as u16,
                    y: 40,
                    w: 8,
                    h: 12,
                    color: 0x0018_3040,
                    // ADDITIVE light (see `GlowQuad::alpha`).
                    alpha: 0,
                },
                GlowQuad {
                    row: 5,
                    x: 20,
                    y: (fh - 8) as u16,
                    w: 30,
                    h: 8,
                    color: 0x0030_1010,
                    // ADDITIVE light (see `GlowQuad::alpha`).
                    alpha: 0,
                },
            ];
            // Radial halos: an Add ember on the left strip + an Over veil on
            // the bottom strip (the light-theme smoke path).
            input.glow_halo = vec![
                RainHalo {
                    row: 1,
                    x: 0,
                    y: 40,
                    w: 12,
                    h: 16,
                    color: 0x0060_80FF,
                    cx: 5,
                    cy: 48,
                    rx: 9,
                    ry: 10,
                    mode: HaloMode::Add,
                },
                RainHalo {
                    row: 5,
                    x: 24,
                    y: (fh - 10) as u16,
                    w: 40,
                    h: 10,
                    color: 0x0040_3830,
                    cx: 44,
                    cy: (fh - 4) as u16,
                    rx: 22,
                    ry: 9,
                    mode: HaloMode::Over,
                },
            ];
            let mut spill = SpillBand::new();
            spill.update(&r, &input);
            assert_eq!(spill.rev(), 1, "band content must tick the revision once");
            let strips = spill.geom.strips();
            assert_eq!(spill.geom.grid_top(), 3 + 24);
            assert_eq!(strips[1].y, fh - (2 * 10 - 3));
            assert_eq!(strips[1].h, 2 * 10 - 3);
            assert_eq!(strips[2].h, 6 * r.cell_size().1);
            let lit = assert_band_parity(&mut r, &input, &spill, theme.bg & 0x00FF_FFFF);
            for (i, n) in lit.iter().enumerate() {
                assert!(
                    *n > 0,
                    "strip {i} must carry lit spill pixels (non-vacuous)"
                );
            }
        }
    }

    /// A top-pad-only origin move preserves the framebuffer size and total
    /// spill allocation length, so `pad_top` itself must participate in the
    /// geometry key. The move must force a full-band re-raster and remain
    /// byte-identical to the newly positioned composed frame.
    #[test]
    fn asymmetric_top_pad_invalidates_spill_geometry_at_constant_frame_size() {
        let theme = Theme::default();
        let Some(mut r) = Renderer::from_system(16.0, theme) else {
            return;
        };
        const PAD: usize = 10;
        const HEAD: usize = 17;
        const TOP_A: usize = 3;
        const TOP_B: usize = 8;
        let (rows, cols) = (6usize, 20usize);
        r.set_pad(PAD);
        r.set_pad_top(TOP_A);
        r.set_head(HEAD);
        let frame_size = r.frame_size(rows, cols);
        let mut term = Terminal::new(rows as u16, cols as u16);
        let mut input = term.cell_frame(rows, cols);
        input.cursor_glow_add = vec![GlowQuad {
            row: 0,
            x: 4,
            y: 4,
            w: 24,
            h: 10,
            color: 0x0020_3040,
            // ADDITIVE light (see `GlowQuad::alpha`).
            alpha: 0,
        }];
        let mut spill = SpillBand::new();
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 1);
        assert_eq!(spill.geom.pad_top, TOP_A);
        let len = spill.rgba().len();
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 1, "unchanged geometry/content stays cached");
        assert!(spill.rects().is_empty());

        r.set_pad_top(TOP_B);
        assert_eq!(r.frame_size(rows, cols), frame_size);
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 2, "grid-origin move must re-raster live spill");
        assert_eq!(spill.geom.pad_top, TOP_B);
        assert_eq!(
            spill.rgba().len(),
            len,
            "vertical padding extent is conserved"
        );
        let expected_rects: Vec<i32> = spill
            .geom
            .strips()
            .iter()
            .flat_map(|s| [s.x as i32, s.y as i32, s.w as i32, s.h as i32])
            .collect();
        assert_eq!(
            spill.rects(),
            expected_rects,
            "origin move dirties the full band"
        );
        assert_band_parity(&mut r, &input, &spill, theme.bg & 0x00FF_FFFF);
    }

    /// The revision/dirty-rect discipline: idle frames, grid-interior-only
    /// emissions (the typing-only law) and settled band content all keep the
    /// revision AND report zero rects; band content appearing, moving and
    /// vanishing each tick it exactly once with in-band rects.
    #[test]
    fn rev_advances_only_on_band_content_change() {
        let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
            return;
        };
        r.set_pad(8);
        r.set_head(20);
        let mut term = Terminal::new(6, 20);
        let mut input = term.cell_frame(6, 20);
        let (fw, fh) = r.frame_size(6, 20);
        let mut spill = SpillBand::new();

        // Idle: buffer allocated for the geometry, fully transparent, rev 0.
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 0, "no content → no revision");
        assert!(spill.rects().is_empty());
        assert!(
            !spill.rgba().is_empty(),
            "nonzero chrome keeps the band buffer allocated"
        );
        assert!(
            spill.rgba().iter().all(|&b| b == 0),
            "idle band transparent"
        );

        // A grid-interior glow (mid-frame) is invisible to the band.
        let (ix, iy) = ((fw / 2) as u16, (fh / 2) as u16);
        input.cursor_glow_add = vec![GlowQuad {
            row: 2,
            x: ix,
            y: iy,
            w: 8,
            h: 8,
            color: 0x0010_2030,
            // ADDITIVE light (see `GlowQuad::alpha`).
            alpha: 0,
        }];
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 0, "grid-interior emissions must not tick");

        // ... even when it MOVES (typing under a glow deep in the grid).
        input.cursor_glow_add[0].x = ix + 6;
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 0, "typing-only frame must not tick");
        assert!(spill.rects().is_empty());

        // Band content appears: exactly one tick, rects confined to the band.
        input.cursor_glow_add.push(GlowQuad {
            row: 0,
            x: 0,
            y: 0,
            w: 20,
            h: 12,
            color: 0x0020_1008,
            // ADDITIVE light (see `GlowQuad::alpha`).
            alpha: 0,
        });
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 1);
        let rects = spill.rects().to_vec();
        assert!(!rects.is_empty());
        for rect in rects.as_chunks::<4>().0 {
            let (x, y, w, h) = (rect[0], rect[1], rect[2], rect[3]);
            assert!(w > 0 && h > 0);
            assert!(x >= 0 && y >= 0 && x + w <= fw as i32 && y + h <= fh as i32);
        }

        // Settled band content: unchanged emissions → rev and rects still.
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 1, "settled content must not re-tick");
        assert!(spill.rects().is_empty());

        // Interior churn while the band part is settled: still still.
        input.cursor_glow_add[0].x = ix;
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 1);
        assert!(spill.rects().is_empty());

        // Band content vanishes: one tick, the vacated spot is reported dirty,
        // the buffer returns to fully transparent.
        input.cursor_glow_add.truncate(1);
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 2);
        assert!(
            !spill.rects().is_empty(),
            "vanished light must report rects"
        );
        assert!(spill.rgba().iter().all(|&b| b == 0));
    }

    /// Pointer stability: content re-rasters write IN PLACE (the host may
    /// hold its view across frames of one geometry); only a geometry change
    /// re-derives the allocation and its length.
    #[test]
    fn spill_ptr_stays_stable_across_content_re_rasters() {
        let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
            return;
        };
        r.set_pad(8);
        r.set_head(20);
        let mut term = Terminal::new(6, 20);
        let mut input = term.cell_frame(6, 20);
        input.cursor_glow_add = vec![GlowQuad {
            row: 0,
            x: 0,
            y: 0,
            w: 16,
            h: 10,
            color: 0x0020_2020,
            // ADDITIVE light (see `GlowQuad::alpha`).
            alpha: 0,
        }];
        let mut spill = SpillBand::new();
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 1);
        let ptr = spill.rgba().as_ptr();
        let len = spill.rgba().len();

        // Animate the band content: revision ticks, allocation holds.
        input.cursor_glow_add[0].x = 6;
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 2);
        assert_eq!(
            spill.rgba().as_ptr(),
            ptr,
            "content re-raster must not move the export"
        );
        assert_eq!(spill.rgba().len(), len);

        // Geometry change: the one event allowed to re-derive the allocation.
        r.set_pad(12);
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 3, "resize with content re-rasters");
        let geom = SpillGeom {
            pad: 12,
            pad_top: 12,
            head: 20,
            frame_w: r.frame_size(6, 20).0,
            frame_h: r.frame_size(6, 20).1,
        };
        assert_eq!(spill.rgba().len(), geom.band_px() * 4);
    }

    /// The identity law: 0/0 chrome has no band — zero bytes, a revision that
    /// never moves, no rects — and chrome→0 clears with exactly one tick.
    #[test]
    fn zero_chrome_is_identity_and_transitions_clear() {
        let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
            return;
        };
        let mut term = Terminal::new(6, 20);
        let mut input = term.cell_frame(6, 20);
        input.cursor_glow_add = vec![GlowQuad {
            row: 0,
            x: 0,
            y: 0,
            w: 10,
            h: 10,
            color: 0x0020_2020,
            // ADDITIVE light (see `GlowQuad::alpha`).
            alpha: 0,
        }];
        let mut spill = SpillBand::new();
        spill.update(&r, &input);
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 0, "0/0 chrome: the revision never advances");
        assert_eq!(spill.rgba().len(), 0, "0/0 chrome: empty buffer");
        assert!(spill.rects().is_empty());

        // Chrome arrives: the edge emission now lands in the band.
        r.set_pad(8);
        r.set_head(20);
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 1);
        assert!(!spill.rgba().is_empty());

        // Chrome leaves: content vanished — ONE clearing tick, then still.
        r.set_pad(0);
        r.set_head(0);
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 2, "chrome→0 with content clears with one tick");
        assert_eq!(spill.rgba().len(), 0);
        assert!(spill.rects().is_empty());
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 2, "steady 0/0 chrome stays still");
    }

    /// `set_include_veils(false)` drops `HaloMode::Over` veils from the band
    /// (Add light stays), the flip itself re-rasters exactly when veils are
    /// present, and is free when they are not — the policy is part of the
    /// clipped-stream fingerprint.
    #[test]
    fn include_veils_policy_gates_over_halos() {
        let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
            return;
        };
        r.set_pad(8);
        r.set_head(20);
        let mut term = Terminal::new(6, 20);
        let mut input = term.cell_frame(6, 20);
        let (fw, _) = r.frame_size(6, 20);
        // Disjoint Add ember + Over veil, both in the top strip.
        input.glow_halo = vec![
            RainHalo {
                row: 0,
                x: 4,
                y: 4,
                w: 10,
                h: 10,
                color: 0x0040_80C0,
                cx: 8,
                cy: 8,
                rx: 7,
                ry: 7,
                mode: HaloMode::Add,
            },
            RainHalo {
                row: 0,
                x: 40,
                y: 4,
                w: 12,
                h: 12,
                color: 0x0020_2020,
                cx: 46,
                cy: 10,
                rx: 8,
                ry: 8,
                mode: HaloMode::Over,
            },
        ];
        let mut spill = SpillBand::new();
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 1);
        // Top strip is packed first at full frame width: alpha at (x, y) is
        // buf[(y·fw + x)·4 + 3].
        let alpha_at = |spill: &SpillBand, x: usize, y: usize| spill.rgba()[(y * fw + x) * 4 + 3];
        assert!(alpha_at(&spill, 8, 8) > 0, "Add ember centre lit");
        assert!(alpha_at(&spill, 46, 10) > 0, "veil centre lit by default");

        spill.set_include_veils(false);
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 2, "policy flip with veils present re-rasters");
        assert!(
            alpha_at(&spill, 8, 8) > 0,
            "Add light survives the veil gate"
        );
        assert_eq!(alpha_at(&spill, 46, 10), 0, "veil excluded from the spill");

        spill.set_include_veils(true);
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 3);
        assert!(alpha_at(&spill, 46, 10) > 0, "veil restored");

        // No veils in the input: the flip is invisible to the fingerprint.
        input.glow_halo.truncate(1);
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 4, "removing the veil is a content change");
        spill.set_include_veils(false);
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 4, "policy flip without veils present is free");
    }
}
