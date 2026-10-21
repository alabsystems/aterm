// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Spill-band rasterizer: the chrome-band slice of the window-space effects layer as a
//! host-blittable straight-alpha RGBA buffer, so an embedder can composite a pane's
//! effect overflow (glow rings, flames) over its neighbours.
//!
//! The composed frame already paints the chrome band
//! (`[head][pad_top][grid][2*pad-pad_top]` minus the grid box), but opaquely: theme bg
//! plus light, which a host cannot blit over another pane. [`SpillBand`] re-rasterizes
//! only the four streams that can leave the grid (`glow_under`, `fire_patch`,
//! `cursor_glow_add`, `glow_halo`) into four packed strips (top including head, bottom,
//! left, right) and solves each covered pixel to a straight `(rgb, alpha)` pair with
//!
//! > `over_rgb(band_bg, rgb, alpha) == the composed frame's band byte`
//!
//! exactly, so a host compositing it source-over onto the pane's own background
//! reproduces the in-frame ring and the `.pane` clip line never shows a seam. Over any
//! other background the pair approximates the engine's saturating-add light.
//!
//! The per-pixel kernels are the renderer's own ([`add_sat`]/[`premul_rgb`],
//! [`halo_row_ny`]/[`halo_weight`] + [`over_rgb`], and [`fire_field`]), replayed in the
//! frame's band z-order (glow_under → fire Add → fire Over → cursor_glow_add → halo Add
//! → halo Over), so spill and frame cannot drift.
//!
//! The fx layers (`docs/DESIGN-card-engine-2026-10-19.md` §4.4) replay too: every
//! layer whose extent reaches the band, in either space, is drawn by the frame's own
//! fx rasterizer onto each strip ([`fx_raster::draw_layers_on`]: the same slicing,
//! occluder subtraction and pixel law F, cut to the strip), at its slot's place in
//! that order: Linear Under → glow_under → Exact Under → fire → cursor_glow_add →
//! Glow → halo → Exact Over → Linear Over. A card's border in the padding is then
//! part of the spill, and the occluders cut it there exactly as in the frame.
//!
//! With 0/0 chrome there is no band: the buffer is empty and the revision never
//! advances. Frames whose band inputs are unchanged (typing under a grid-interior glow,
//! idle re-renders) keep the revision and report no dirty rects, so a host can skip its
//! blit.

use aterm_core::render::{FxBlend, FxInk, FxLayer, FxSlot, FxSpace, PxRect};

use crate::fire_field;
use crate::fx_raster::{self, FxCanvas, FxFrame, FxScratch, FxSlice};
use crate::{
    FireMode, FirePatch, GlowQuad, HaloMode, RainHalo, RenderInput, Renderer, add_sat, halo_row_ny,
    halo_weight, over_rgb, premul_rgb,
};

/// The band geometry a spill buffer was laid out for. `pad_top` is explicit because
/// moving only the asymmetric grid origin keeps the frame size but changes every
/// horizontal strip split. Any change re-derives the strips and is the one event that
/// may move [`SpillBand::rgba`]'s allocation.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct SpillGeom {
    pad: usize,
    pad_top: usize,
    head: usize,
    frame_w: usize,
    frame_h: usize,
}

/// One strip's rect in frame-absolute device px.
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

    /// The four band strips in packed buffer order (top including head, bottom, left,
    /// right), each row-major, concatenated. Zero-area strips (e.g. `pad == 0` with a
    /// head band) stay in the layout with 0 bytes.
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

    /// A window-pixel rect clipped to the frame, as a [`Rect`]; `None` when
    /// nothing of it lies in the frame. Unbounded edges clip like any other.
    fn frame_rect(&self, r: PxRect) -> Option<Rect> {
        let clamp = |v: i32, hi: usize| usize::try_from(v.max(0)).map_or(hi, |v| v.min(hi));
        let (x0, x1) = (clamp(r.x0, self.frame_w), clamp(r.x1, self.frame_w));
        let (y0, y1) = (clamp(r.y0, self.frame_h), clamp(r.y1, self.frame_h));
        (x0 < x1 && y0 < y1).then_some((x0, y0, x1 - x0, y1 - y0))
    }

    /// The window pixels `layer` may write ([`FxLayer::extent`] at its space's
    /// origin), clipped to the frame.
    fn layer_rect(&self, layer: &FxLayer) -> Option<Rect> {
        let e = layer.extent();
        if e.is_empty() {
            return None;
        }
        let e = match layer.space {
            FxSpace::Window => e,
            FxSpace::Grid => e.translate(
                i32::try_from(self.pad).unwrap_or(i32::MAX),
                i32::try_from(self.grid_top()).unwrap_or(i32::MAX),
            ),
        };
        self.frame_rect(e)
    }

    /// Total band pixels across the four strips.
    fn band_px(&self) -> usize {
        self.strips().iter().map(|s| s.w * s.h).sum()
    }

    /// Whether a frame-space rect reaches the band: non-empty after the frame clip and
    /// not inside the grid box. Mirrors the renderer's clipping, so grid-interior
    /// emissions never advance the revision.
    fn rect_touches_band(&self, (x, y, w, h): Rect) -> bool {
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

/// Union of two coverages as source-over alpha accumulation (`a' = s + a·(255−s)/255`,
/// round-half). Additive light contributes its max channel as a pseudo-coverage, veils
/// their real one.
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

/// Solve one band pixel to a straight `(r, g, b, a)` with `over_rgb(base, rgb, a) == c`
/// per channel: `c` is the pixel after the band's op replay over `base`, `a0` the
/// accumulated coverage.
///
/// Per channel `out(s) = (s·a + b·(255−a) + 127) / 255` is monotone in `s` with steps
/// ≤ 1, so every target in `[out(0), out(255)]` is reachable; both bounds widen with
/// `a`, so lifting `a` to the per-channel minima below makes every channel solvable with
/// one shared alpha (`a = 255` always works). The natural `a0` is kept when feasible so
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

/// The engine half of the cross-pane effects compositor: a persistent straight-alpha
/// RGBA rasterization of the effect emissions in the chrome band, plus the change
/// signal (revision and packed dirty rects) a host needs to blit it without polling.
/// Owned by the wasm terminals and fed once per rendered frame via [`SpillBand::update`].
#[derive(Default)]
pub struct SpillBand {
    geom: SpillGeom,
    /// The band's base bg (frame default-bg RGB) the solve is exact over.
    base_bg: u32,
    /// Whether `HaloMode::Over` veils are excluded from the spill (inverted so the
    /// derived default includes them and the byte-parity law holds out of the box).
    /// Excluding keeps only additive light and fire ink, for light-theme smoke.
    veils_excluded: bool,
    rev: u32,
    /// The exported straight-alpha RGBA strips (packed as `SpillGeom::strips`). The
    /// allocation moves only on a geometry change: the host's pointer-stability contract.
    rgba: Vec<u8>,
    /// This update's dirty rects, packed `x,y,w,h` frame-absolute device px.
    rects: Vec<i32>,
    // Replay planes (band colour over base_bg + accumulated coverage),
    // strip-packed like `rgba`; retained so a change-frame allocates nothing.
    color: Vec<u32>,
    cover: Vec<u8>,
    // The previous frame's band-clipped emissions — the content fingerprint
    // that keeps the revision still on typing-only/settled frames.
    prev: BandStreams,
    // Scratch for the current frame's clip (swapped into `prev` on a raster).
    cur: BandStreams,
    /// The fx rasterizer's resident scratch (occluder pieces, sprite column
    /// tables), so a replay allocates nothing once warm.
    fx_scratch: FxScratch,
}

/// One frame's band-clipped emissions, per window-absolute stream.
#[derive(Default, PartialEq)]
struct BandStreams {
    glow_under: Vec<GlowQuad>,
    fire: Vec<FirePatch>,
    glow: Vec<GlowQuad>,
    halo: Vec<RainHalo>,
    /// The fx layers whose extent reaches the band, in frame order. Compared
    /// by identity (the content `Arc`), and held, so a settled card costs a
    /// pointer compare and a freed content can never alias a new one.
    fx: Vec<FxLayer>,
    /// The occluders reaching the band, kept only while `fx` is not empty
    /// (an occluder only cuts layers).
    fx_occluders: Vec<PxRect>,
}

impl BandStreams {
    fn is_empty(&self) -> bool {
        self.glow_under.is_empty()
            && self.fire.is_empty()
            && self.glow.is_empty()
            && self.halo.is_empty()
            && self.fx.is_empty()
    }
}

/// A frame-space rect as `(x, y, w, h)` device px.
type Rect = (usize, usize, usize, usize);

/// The grid row and frame-space rect every window-absolute emission carries.
trait Emission: Copy {
    fn row(&self) -> usize;
    fn rect(&self) -> Rect;
}

macro_rules! impl_emission {
    ($($t:ty),*) => {$(
        impl Emission for $t {
            fn row(&self) -> usize {
                self.row as usize
            }
            fn rect(&self) -> Rect {
                (self.x as usize, self.y as usize, self.w as usize, self.h as usize)
            }
        }
    )*};
}
impl_emission!(GlowQuad, FirePatch, RainHalo);

/// Band-clip one emission stream into `dst` (allocation-reusing), mirroring the
/// renderer's `row >= rows` skip so the fingerprint matches what the frame
/// actually drew.
fn clip_stream<T: Emission>(
    dst: &mut Vec<T>,
    src: &[T],
    rows: usize,
    geom: SpillGeom,
    keep: impl Fn(&T) -> bool,
) {
    dst.clear();
    dst.extend(
        src.iter()
            .filter(|q| q.row() < rows && keep(q) && geom.rect_touches_band(q.rect()))
            .copied(),
    );
}

impl SpillBand {
    pub fn new() -> Self {
        Self::default()
    }

    /// Band-content revision: advances only when the exported bytes changed (band
    /// emissions appeared, moved or vanished; the bg, the veil policy or the geometry
    /// changed under live content). Unchanged ⇒ the host may skip its blit.
    pub fn rev(&self) -> u32 {
        self.rev
    }

    /// The last update's dirty rects, packed `x,y,w,h` (frame-absolute device px), 4
    /// `i32`s per rect. Empty on a no-change frame. Content changes report the prev∪cur
    /// emission bbox clipped per strip; geometry/bg/policy changes report the full band.
    pub fn rects(&self) -> &[i32] {
        &self.rects
    }

    /// The straight-alpha RGBA strip buffer (packing in the module docs); empty at 0/0
    /// chrome.
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// Include `HaloMode::Over` veils in the spill (default `true`), from the next
    /// [`Self::update`]. A flip re-rasters and ticks the revision only when veils are
    /// in the band.
    pub fn set_include_veils(&mut self, on: bool) {
        self.veils_excluded = !on;
    }

    /// Refresh the spill for the frame `input` the renderer just composed. When nothing
    /// band-relevant changed this is a clip and compare of the four streams, leaving the
    /// revision, bytes and pointer untouched.
    pub fn update(&mut self, renderer: &Renderer, input: &RenderInput) {
        let geom = SpillGeom {
            pad: renderer.pad(),
            pad_top: renderer.pad_top(),
            head: renderer.head(),
            frame_w: 0,
            frame_h: 0,
        };
        // 0/0 chrome has no band. A chrome→0 transition drops the buffers, with one
        // revision tick if content vanished so the host clears its overlay.
        if geom.pad == 0 && geom.head == 0 {
            self.rects.clear();
            if self.geom != geom {
                if !self.prev.is_empty() {
                    self.rev = self.rev.wrapping_add(1);
                }
                self.prev = BandStreams::default();
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

        // Band-clip the four window-absolute streams; grid-interior emissions vanish
        // here, which keeps typing-only frames still.
        let (rows, cur) = (input.rows, &mut self.cur);
        clip_stream(&mut cur.glow_under, &input.glow_under, rows, geom, |_| true);
        clip_stream(&mut cur.fire, &input.fire_patch, rows, geom, |_| true);
        clip_stream(&mut cur.glow, &input.cursor_glow_add, rows, geom, |_| true);
        // Excluded veils are dropped at the clip, so the veil policy is part of the
        // fingerprint: a flip with veils present re-rasters, without them it is free.
        let include_veils = !self.veils_excluded;
        clip_stream(&mut cur.halo, &input.glow_halo, rows, geom, |q| {
            include_veils || q.mode == HaloMode::Add
        });
        // The fx layers reaching the band (Arc clones: no payload is copied),
        // and the occluders that can cut them there.
        cur.fx.clear();
        cur.fx.extend(
            input
                .fx_layers
                .iter()
                .filter(|l| {
                    geom.layer_rect(l)
                        .is_some_and(|r| geom.rect_touches_band(r))
                })
                .cloned(),
        );
        cur.fx_occluders.clear();
        if !cur.fx.is_empty() {
            cur.fx_occluders.extend(
                input
                    .fx_occluders
                    .iter()
                    .filter(|o| {
                        geom.frame_rect(**o)
                            .is_some_and(|r| geom.rect_touches_band(r))
                    })
                    .copied(),
            );
        }

        let geom_changed = geom != self.geom;
        let bg_changed = base_bg != self.base_bg;
        let streams_changed = self.cur != self.prev;
        if !(geom_changed || bg_changed || streams_changed) {
            // Typing-only / settled / idle frame: rev, rects and bytes all still.
            self.rects.clear();
            return;
        }
        if geom_changed {
            // The one event that may move the exported allocation; zeroed means
            // transparent until rastered below.
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

        if self.prev.is_empty() && self.cur.is_empty() {
            // A bg/geometry change under an all-transparent band: no visible
            // bytes changed, so the revision (the host's blit signal) holds.
            self.rects.clear();
        } else {
            self.emit_dirty_rects(geom_changed || bg_changed);
            self.rasterize(renderer.cell_size().1, rows);
            self.rev = self.rev.wrapping_add(1);
        }
        // Swap the just-clipped streams into the fingerprint (allocation-reusing).
        std::mem::swap(&mut self.prev, &mut self.cur);
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
        let mut grow = |(x, y, w, h): Rect| {
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
        for q in self.prev.glow_under.iter().chain(&self.cur.glow_under) {
            grow(q.rect());
        }
        for q in self.prev.fire.iter().chain(&self.cur.fire) {
            grow(q.rect());
        }
        for q in self.prev.glow.iter().chain(&self.cur.glow) {
            grow(q.rect());
        }
        for q in self.prev.halo.iter().chain(&self.cur.halo) {
            grow(q.rect());
        }
        // A layer's pixels lie in its extent, and an occluder change moves
        // pixels only inside some layer's, so the layers' rects bound both.
        for l in self.prev.fx.iter().chain(&self.cur.fx) {
            if let Some(r) = geom.layer_rect(l) {
                grow(r);
            }
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

    /// Replay the band's ops over `base_bg` per strip in the frame's z-order, then solve
    /// every pixel to the exported straight-alpha pair, in place (stable pointer).
    /// `cell_h` and `rows` place the fx layers' band slicing as the frame's.
    fn rasterize(&mut self, cell_h: usize, rows: usize) {
        let base = self.base_bg;
        self.color.fill(base);
        self.cover.fill(0);
        let geom = self.geom;
        let pad = geom.pad;
        let frame = FxFrame {
            w: geom.frame_w,
            h: geom.frame_h,
            pad,
            grid_top: geom.grid_top(),
            cell_h,
            rows,
        };
        let Self {
            color,
            cover,
            cur,
            fx_scratch,
            ..
        } = self;
        let mut off = 0usize;
        for s in geom.strips() {
            let n = s.w * s.h;
            if n == 0 {
                continue;
            }
            let color = &mut color[off..off + n];
            let cover = &mut cover[off..off + n];
            let mut fx = |color: &mut [u32], cover: &mut [u8], slot, blend| {
                if !cur.fx.is_empty() {
                    fx_raster::draw_layers_on(
                        &mut StripCanvas { s, color, cover },
                        &frame,
                        &cur.fx,
                        &cur.fx_occluders,
                        slot,
                        blend,
                        None,
                        fx_scratch,
                    );
                }
            };
            fx(color, cover, FxSlot::Under, FxBlend::Linear);
            replay_flat_add(color, cover, s, &cur.glow_under);
            fx(color, cover, FxSlot::Under, FxBlend::Exact);
            replay_fire(color, cover, s, pad, &cur.fire);
            replay_flat_add(color, cover, s, &cur.glow);
            fx(color, cover, FxSlot::Glow, FxBlend::Exact);
            replay_halo(color, cover, s, &cur.halo);
            fx(color, cover, FxSlot::Over, FxBlend::Exact);
            fx(color, cover, FxSlot::Over, FxBlend::Linear);
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

/// One strip's replay planes as an fx stamp target: the frame's kernels on
/// the colour plane, and each paint's coverage folded into the cover plane as
/// the glow quads fold theirs (an Exact paint covers by `max(a, max channel)`,
/// so additive light counts by its brightness; a Linear one by its opacity).
struct StripCanvas<'a> {
    s: StripRect,
    color: &'a mut [u32],
    cover: &'a mut [u8],
}

impl StripCanvas<'_> {
    /// The plane index range of window row `y`, columns `[x0, x1)`.
    #[inline]
    fn span(&self, y: usize, x0: usize, x1: usize) -> std::ops::Range<usize> {
        let row = (y - self.s.y) * self.s.w;
        row + (x0 - self.s.x)..row + (x1 - self.s.x)
    }
}

impl FxCanvas for StripCanvas<'_> {
    fn bounds(&self) -> FxSlice {
        FxSlice {
            x0: self.s.x,
            x1: self.s.x + self.s.w,
            y0: self.s.y,
            y1: self.s.y + self.s.h,
        }
    }

    fn flat(&mut self, y: usize, x0: usize, x1: usize, c: u32, a: u8) {
        let k = self.span(y, x0, x1);
        fx_raster::fill_flat(&mut self.color[k.clone()], c, a);
        let ap = a.max(max_channel(c));
        for v in &mut self.cover[k] {
            *v = cover_union(*v, ap);
        }
    }

    fn ramp(&mut self, y: usize, x0: usize, x1: usize, i0: u32, qw: u32, c0: FxInk, c1: FxInk) {
        let k = self.span(y, x0, x1);
        fx_raster::fill_glow_span(&mut self.color[k.clone()], i0, qw, c0, c1);
        let (a0, a1) = (u32::from(c0.alpha), u32::from(c1.alpha));
        for (j, v) in self.cover[k].iter_mut().enumerate() {
            let i = i0 + j as u32;
            let a = crate::glow_lerp(a0, a1, i, qw) as u8;
            let c = crate::glow_lerp_rgb(c0.color, c1.color, i, qw);
            *v = cover_union(*v, a.max(max_channel(c)));
        }
    }

    fn linear(&mut self, y: usize, x0: usize, x1: usize, c: u32, a: u8) {
        let k = self.span(y, x0, x1);
        for (p, v) in self.color[k.clone()].iter_mut().zip(&mut self.cover[k]) {
            *p = crate::blend(*p, c, a);
            *v = cover_union(*v, a);
        }
    }
}

/// Strip-clip a frame-space emission rect; `None` when it misses the strip.
#[inline]
fn clip_to_strip(s: StripRect, (x, y, w, h): Rect) -> Option<(usize, usize, usize, usize)> {
    let x0 = x.max(s.x);
    let y0 = y.max(s.y);
    let xe = (x + w).min(s.x + s.w);
    let ye = (y + h).min(s.y + s.h);
    (x0 < xe && y0 < ye).then_some((x0, y0, xe, ye))
}

/// The flat glow quads (`glow_under`, `cursor_glow_add`), restricted to one strip: the
/// `draw_flat_add` contract, window-absolute premultiplied light, [`add_sat`] at
/// `GlowQuad::alpha == 0` and premultiplied source-over otherwise (the rainbow bed).
/// Pure per-pixel functions of absolute coords, so the strip clip is byte-identical.
///
/// Every quad takes the frame's gradient law ([`GlowQuad::color2`]): per column, colour
/// and opacity are [`crate::glow_lerp`]'d between the quad's edges and composited
/// source-over; a flat quad is the lerp's identity and opacity 0 is `add_sat` byte for
/// byte, so one loop replays both branches. The column index is read against the
/// quad's unclipped left edge, as the frame reads it.
fn replay_flat_add(color: &mut [u32], cover: &mut [u8], s: StripRect, quads: &[GlowQuad]) {
    for q in quads {
        let Some((x0, y0, xe, ye)) = clip_to_strip(s, q.rect()) else {
            continue;
        };
        let (a0, a1) = (u32::from(q.alpha), u32::from(q.alpha2));
        let qw = u32::from(q.w);
        for x in x0..xe {
            // `x − q.x < q.w ≤ u16::MAX`, so the cast is exact.
            let i = (x - q.x as usize) as u32;
            let a = crate::glow_lerp(a0, a1, i, qw) as u8;
            let c = crate::glow_lerp_rgb(q.color, q.color2, i, qw);
            // The Over case occludes by its own opacity as well as its light —
            // the solver's `a0` must reflect the paint's real coverage.
            let ap = a.max(max_channel(c));
            for y in y0..ye {
                let k = (y - s.y) * s.w + (x - s.x);
                color[k] = crate::over_premul(color[k], c, a);
                cover[k] = cover_union(cover[k], ap);
            }
        }
    }
}

/// The radial halos (`glow_halo`), restricted to one strip: the `draw_radial_add`
/// contract, Add quads then Over veils, weighted by the shared [`halo_row_ny`] /
/// [`halo_weight`] kernels. (The frame loop's ellipse-span clamp only skips zero-weight
/// pixels, so omitting it is byte-identical.)
fn replay_halo(color: &mut [u32], cover: &mut [u8], s: StripRect, quads: &[RainHalo]) {
    for mode in [HaloMode::Add, HaloMode::Over] {
        for q in quads.iter().filter(|q| q.mode == mode) {
            let Some((x0, y0, xe, ye)) = clip_to_strip(s, q.rect()) else {
                continue;
            };
            let (cx, cy) = (q.cx as i32, q.cy as i32);
            let rx2 = (q.rx as i32) * (q.rx as i32);
            let ry2 = (q.ry as i32) * (q.ry as i32);
            // Mirror the frame loop's [`HaloMode::Over`] alpha ceiling so the band
            // stays byte-continuous with the in-frame veil (`0` high byte: uncapped).
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

/// The fire field (`fire_patch`), restricted to one strip: the `draw_fire_patch`
/// contract, Add patches then Over patches, each pixel a pure [`fire_field`] function of
/// absolute window coords, so a strip-clipped sweep is byte-identical. `pad` feeds
/// `top_fade_y` exactly as the frame does.
fn replay_fire(color: &mut [u32], cover: &mut [u8], s: StripRect, pad: usize, quads: &[FirePatch]) {
    for mode in [FireMode::Add, FireMode::Over] {
        for q in quads.iter().filter(|q| q.mode == mode) {
            let Some((x0, y0, xe, ye)) = clip_to_strip(s, q.rect()) else {
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
    use std::sync::Arc;

    use super::*;
    use crate::{Theme, WindowCpu};
    use aterm_core::render::{FxArt, FxContent, FxItem, FxRamps, FxRect, FxSprite, FxTexels};
    use aterm_core::terminal::Terminal;

    /// The solver alone: over a (bg byte, target byte) lattice and several natural
    /// coverages, the solved straight pair round-trips through `over_rgb` to the target
    /// exactly.
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
        let (lit, seams) = band_parity(r, input, spill, base, true);
        assert_eq!(seams, 0);
        lit
    }

    /// The parity walk of [`assert_band_parity`]: the per-strip lit counts and
    /// the number of band pixels where spill ∘ bg and the frame differ (the
    /// seams), panicking at the first one when `strict`.
    fn band_parity(
        r: &mut Renderer,
        input: &RenderInput,
        spill: &SpillBand,
        base: u32,
        strict: bool,
    ) -> ([usize; 4], usize) {
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
        let mut seams = 0usize;
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
                    if strict {
                        assert_eq!(
                            composed, frame,
                            "spill ∘ bg must equal the frame band at ({x},{y}) in strip {si}"
                        );
                    }
                    seams += usize::from(composed != frame);
                }
            }
            off += s.w * s.h;
        }
        (lit, seams)
    }

    /// An fx layer of one item.
    fn fx_layer(
        id: u64,
        item: FxItem,
        palette: Vec<FxInk>,
        slot: FxSlot,
        space: FxSpace,
        blend: FxBlend,
    ) -> FxLayer {
        FxLayer::new(
            id,
            Arc::new(FxContent::new(vec![item], palette)),
            slot,
            space,
            blend,
        )
    }

    fn fx_rect(x: i32, y: i32, w: usize, h: usize) -> FxItem {
        FxItem::Rect(FxRect {
            x,
            y,
            w: u16::try_from(w).expect("small"),
            h: u16::try_from(h).expect("small"),
            ink: 0,
        })
    }

    fn fx_sprite(x: i32, y: i32, dw: u16, dh: u16, art: Arc<FxArt>) -> FxItem {
        FxItem::Sprite(FxSprite {
            x,
            y,
            dw,
            dh,
            flip_x: false,
            art,
            ink: 0,
            clip: PxRect::EVERYWHERE,
        })
    }

    /// A straight-alpha art with clear, opaque and translucent texels.
    fn straight_art(w: u16, h: u16) -> Arc<FxArt> {
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let a = [0u8, 255, 140, 60][usize::from((x + y) % 4)];
                data.extend_from_slice(&[(x * 41 + 20) as u8, (y * 23 + 90) as u8, 180, a]);
            }
        }
        Arc::new(FxArt::new(w, h, FxTexels::Rgba8Straight, data).expect("sized"))
    }

    /// Layers of every slot and blend, in both spaces, reaching all four
    /// strips of a `fw × fh` frame (pad 10, grid_top 27): a card border in
    /// the padding (Window, Under, Exact), a Grid-space Over rect and ramps
    /// that reach up and left out of the grid, a Glow Index8 sprite on the
    /// right strip, Linear Straight sprites Under (right strip) and Over
    /// (bottom strip, from the grid), and a Premul sprite in the head band.
    fn band_layers(fw: usize, fh: usize, ch: usize) -> Vec<FxLayer> {
        let (fwi, fhi) = (fw as i32, fh as i32);
        let border = FxLayer::new(
            1,
            Arc::new(FxContent::new(
                vec![
                    fx_rect(2, 6, fw - 4, 3),
                    fx_rect(2, 6, 3, fh - 12),
                    fx_rect(fwi - 5, 6, 3, fh - 12),
                    fx_rect(2, fhi - 6, fw - 4, 3),
                ],
                vec![
                    FxInk::over(0x0030_2010, 0x60),
                    FxInk::over(0x0010_2030, 0x30),
                ],
            )),
            FxSlot::Under,
            FxSpace::Window,
            FxBlend::Exact,
        );
        let grid_over = FxLayer::new(
            2,
            Arc::new(FxContent::new(
                vec![
                    fx_rect(-8, -10, 60, 14),
                    FxItem::Ramps(FxRamps {
                        x: -6,
                        y: 2 * ch as i32,
                        w: 9,
                        h: 3,
                        ink: 0,
                    }),
                ],
                vec![
                    FxInk::add(0x0028_1810),
                    FxInk::add(0x0008_1828),
                    FxInk::over(0x0040_2000, 0x80),
                    FxInk::over(0x0000_2040, 0x20),
                    FxInk::add(0x0030_3030),
                    FxInk::add(0x0003_0303),
                ],
            )),
            FxSlot::Over,
            FxSpace::Grid,
            FxBlend::Exact,
        );
        let glow = fx_layer(
            3,
            fx_sprite(
                fwi - 9,
                38,
                8,
                20,
                Arc::new(
                    FxArt::new(3, 2, FxTexels::Index8, vec![1, 0, 2, 2, 1, 0]).expect("sized"),
                ),
            ),
            vec![FxInk::add(0x0040_3020), FxInk::over(0x0020_3040, 0xa0)],
            FxSlot::Glow,
            FxSpace::Window,
            FxBlend::Exact,
        );
        let linear_under = fx_layer(
            4,
            fx_sprite(fwi - 12, 30, 12, 30, straight_art(4, 5)),
            vec![FxInk::over(0x00ff_e0c0, 200)],
            FxSlot::Under,
            FxSpace::Window,
            FxBlend::Linear,
        );
        let linear_over = fx_layer(
            5,
            fx_sprite(30, 6 * ch as i32 - 4, 20, 14, straight_art(5, 3)),
            vec![FxInk::over(0x00c0_e0ff, 230)],
            FxSlot::Over,
            FxSpace::Grid,
            FxBlend::Linear,
        );
        let mut premul = Vec::new();
        for k in 0..12u8 {
            let a = [0u8, 255, 120, 0][usize::from(k % 4)];
            let c = if a == 0 { 0x18 } else { a / 2 };
            premul.extend_from_slice(&[c, c / 2, c / 3, a]);
        }
        let head = fx_layer(
            6,
            fx_sprite(
                10,
                2,
                40,
                16,
                Arc::new(FxArt::new(4, 3, FxTexels::Rgba8Premul, premul).expect("sized")),
            ),
            vec![],
            FxSlot::Under,
            FxSpace::Window,
            FxBlend::Exact,
        );
        vec![border, grid_over, glow, linear_under, linear_over, head]
    }

    /// THE FX LAYERS SPILL (design §4.4): with layers of every slot and blend,
    /// in both spaces, reaching all four strips (a card border in the padding
    /// among them) and two occluders cutting them there, the spill over the
    /// theme bg equals the composed frame's band byte for byte, alone and
    /// interleaved with the glow_under, fire, aurora and halo streams on the
    /// same pixels, on a dark and a light theme.
    ///
    /// CONTROLS (in the test): the same frame's spill without its fx layers,
    /// and its spill without its occluders, each leave a seam.
    #[test]
    fn fx_layers_spill_byte_identical_over_the_frame_band() {
        let dark = Theme::default();
        let light = Theme {
            fg: 0x001A_1A1A,
            bg: 0x00F5_F5F0,
            cursor: 0x0033_3333,
            selection: 0x00B0_C4DE,
        };
        for theme in [dark, light] {
            let mut r = Renderer::from_system(16.0, theme).expect(crate::NO_FONT);
            r.set_pad(10);
            r.set_pad_top(3);
            r.set_head(24);
            let base = theme.bg & 0x00FF_FFFF;
            let ch = r.cell_size().1;
            let mut term = Terminal::new(6, 20);
            let (fw, fh) = r.frame_size(6, 20);
            let mut input = term.cell_frame(6, 20);
            input.fx_layers = band_layers(fw, fh, ch);
            input.fx_occluders = vec![
                PxRect::new(0, 8, 50, 14),
                PxRect::new(fw as i32 - 6, 44, fw as i32, 52),
            ];
            let spill_of = |r: &Renderer, i: &RenderInput| {
                let mut s = SpillBand::new();
                s.update(r, i);
                s
            };
            // Alone: every strip is lit by the layers.
            let spill = spill_of(&r, &input);
            let lit = assert_band_parity(&mut r, &input, &spill, base);
            for (i, n) in lit.iter().enumerate() {
                assert!(*n > 0, "strip {i} carries fx light (non-vacuous)");
            }
            // Interleaved with the streams on the same pixels.
            // Each stream overlaps a layer it must not commute with: the
            // order of every fx slot against its neighbours shows in a byte.
            input.glow_under = vec![
                GlowQuad::gradient(0, 2, 4, 60, 14, 0x0030_0810, 0x30, 0x0008_1030, 0x50),
                GlowQuad::gradient(
                    1,
                    (fw - 12) as u16,
                    32,
                    12,
                    20,
                    0x0020_3010,
                    0x40,
                    0x0010_2030,
                    0x60,
                ),
            ];
            input.fire_patch = vec![FirePatch {
                row: 0,
                x: 0,
                y: 2,
                w: 70,
                h: 26,
                base_y: 40,
                peak_h: 44,
                phase: 977_331,
                temp: 210,
                strength: 230,
                lean: 6,
                cov_cap: 210,
                cell_h: 18,
                mode: FireMode::Add,
            }];
            input.cursor_glow_add = vec![GlowQuad {
                row: 0,
                x: (fw - 10) as u16,
                y: 36,
                w: 10,
                h: 20,
                color: 0x0018_3040,
                alpha: 0x40,
                color2: 0x0018_3040,
                alpha2: 0x40,
            }];
            let veil = |row, x: usize, y: usize, w: usize, h: usize| RainHalo {
                row,
                x: x as u16,
                y: y as u16,
                w: w as u16,
                h: h as u16,
                color: 0x0040_3830,
                cx: (x + w / 2) as u16,
                cy: (y + h / 2) as u16,
                rx: (w / 2 + 2) as u16,
                ry: (h / 2 + 2) as u16,
                mode: HaloMode::Over,
            };
            input.glow_halo = vec![
                veil(5, 20, fh - 12, 50, 12),
                veil(1, fw - 12, 34, 12, 26),
                veil(0, 0, 10, 40, 20),
            ];
            let spill = spill_of(&r, &input);
            assert_band_parity(&mut r, &input, &spill, base);

            // CONTROL: the spill of the frame without its layers has a seam.
            let mut bare = input.clone();
            bare.fx_layers.clear();
            let partial = spill_of(&r, &bare);
            let (_, seams) = band_parity(&mut r, &input, &partial, base, false);
            assert!(seams > 0, "CONTROL: a spill without the fx shows a seam");
            // CONTROL: so does its spill without the occluders.
            let mut uncut = input.clone();
            uncut.fx_occluders.clear();
            let partial = spill_of(&r, &uncut);
            let (_, seams) = band_parity(&mut r, &input, &partial, base, false);
            assert!(
                seams > 0,
                "CONTROL: a spill that ignores the occluders shows a seam"
            );
        }
    }

    /// The fx layers move the revision as the other streams do: a layer wholly
    /// inside the grid and settled layers (the same `Arc`s) keep it still with
    /// no rects; a moved band layer ticks it once with rects covering its old
    /// and new place in the band; an occluder change over a band layer ticks
    /// it; the layer vanishing ticks it and leaves the band transparent.
    #[test]
    fn fx_layers_tick_the_revision_only_when_the_band_changes() {
        let mut r = Renderer::from_system(16.0, Theme::default()).expect(crate::NO_FONT);
        r.set_pad(8);
        r.set_head(20);
        let mut term = Terminal::new(6, 20);
        let mut input = term.cell_frame(6, 20);
        let (fw, fh) = r.frame_size(6, 20);
        let (cw, ch) = r.cell_size();
        let mut spill = SpillBand::new();
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 0);

        // Inside the grid: invisible to the band, even as it changes.
        let inner = fx_layer(
            1,
            fx_rect(2 * cw as i32, ch as i32, 4 * cw, ch),
            vec![FxInk::add(0x0020_2020); 2],
            FxSlot::Under,
            FxSpace::Grid,
            FxBlend::Exact,
        );
        input.fx_layers = vec![inner.clone()];
        spill.update(&r, &input);
        let mut moved_inner = inner.clone();
        moved_inner.offset = (cw as i32, ch as i32);
        input.fx_layers = vec![moved_inner];
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 0, "grid-interior layers never tick");
        assert!(spill.rects().is_empty());

        // A border in the head band: one tick.
        let border = fx_layer(
            2,
            fx_rect(4, 4, 40, 6),
            vec![FxInk::add(0x0030_2010); 2],
            FxSlot::Under,
            FxSpace::Window,
            FxBlend::Exact,
        );
        input.fx_layers = vec![inner.clone(), border.clone()];
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 1);
        assert!(!spill.rgba().iter().all(|&b| b == 0), "the border is lit");

        // Settled: the same Arcs keep the revision and report nothing.
        input.fx_layers = vec![inner.clone(), border.clone()];
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 1, "settled layers never tick");
        assert!(spill.rects().is_empty());

        // Moved 30 px right: one tick, rects covering old and new place.
        let mut moved = border.clone();
        moved.offset.0 = 30;
        input.fx_layers = vec![inner.clone(), moved.clone()];
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 2);
        let rects = spill.rects().to_vec();
        let covers = |x: i32, y: i32| {
            rects
                .as_chunks::<4>()
                .0
                .iter()
                .any(|q| (q[0]..q[0] + q[2]).contains(&x) && (q[1]..q[1] + q[3]).contains(&y))
        };
        assert!(
            covers(4, 4) && covers(73, 9),
            "prev ∪ cur extents: {rects:?}"
        );
        for q in rects.as_chunks::<4>().0 {
            assert!(q[0] >= 0 && q[1] >= 0 && q[0] + q[2] <= fw as i32 && q[1] + q[3] <= fh as i32);
        }

        // An occluder over the band layer: one tick.
        input.fx_occluders = vec![PxRect::new(0, 0, 50, 8)];
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 3, "an occluder cutting band light ticks");
        // An occluder change inside the grid only: still.
        input.fx_occluders.push(PxRect::new(
            3 * cw as i32 + 8,
            30 + ch as i32,
            5 * cw as i32 + 8,
            30 + 2 * ch as i32,
        ));
        spill.update(&r, &input);
        assert_eq!(
            spill.rev(),
            3,
            "an occluder off the band is not band content"
        );

        // Vanished: one tick, transparent again.
        input.fx_layers = vec![inner];
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 4);
        assert!(!spill.rects().is_empty());
        assert!(spill.rgba().iter().all(|&b| b == 0));
    }

    /// The byte-parity property: spill over the theme bg equals the composed frame's
    /// band byte for byte, with every window-absolute stream (flat adds, fire Add and
    /// Over, radial Add halos, Over veils) straddling all four strips and the grid edge,
    /// on a dark and a light theme.
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
            let mut r = Renderer::from_system(16.0, theme).expect(crate::NO_FONT);
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
                    alpha: 0,
                    color2: 0x0020_1008,
                    alpha2: 0,
                },
                GlowQuad {
                    row: 0,
                    x: 0,
                    y: 20,
                    w: 18,
                    h: 20,
                    color: 0x0008_1020,
                    alpha: 0,
                    color2: 0x0008_1020,
                    alpha2: 0,
                },
                GlowQuad {
                    row: 0,
                    x: 6,
                    y: 22,
                    w: 24,
                    h: 12,
                    color: 0x0009_1216,
                    // Source-over paint (the rainbow bed's blend) straddling
                    // grid_top: the replay must take draw_flat_add's Over branch.
                    alpha: 0x16,
                    color2: 0x0009_1216,
                    alpha2: 0x16,
                },
                // Gradient quads, ramped per column from the unclipped left edge:
                // an additive ramp in the head band, and a source-over ramp that
                // starts left of the pad and straddles grid_top, so the strip clip
                // cuts both its left end and its bottom.
                GlowQuad::gradient(0, 20, 6, 100, 8, 0x0060_0000, 0, 0x0000_0060, 0),
                GlowQuad::gradient(0, 4, 18, 60, 14, 0x0030_0810, 0x30, 0x0008_1030, 0x50),
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
                    alpha: 0,
                    color2: 0x0018_3040,
                    alpha2: 0,
                },
                GlowQuad {
                    row: 5,
                    x: 20,
                    y: (fh - 8) as u16,
                    w: 30,
                    h: 8,
                    color: 0x0030_1010,
                    alpha: 0,
                    color2: 0x0030_1010,
                    alpha2: 0,
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
        let mut r = Renderer::from_system(16.0, theme).expect(crate::NO_FONT);
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
            alpha: 0,
            color2: 0x0020_3040,
            alpha2: 0,
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

    /// The revision/dirty-rect discipline: idle frames, grid-interior-only emissions and
    /// settled band content keep the revision and report no rects; band content
    /// appearing, moving and vanishing each tick it exactly once with in-band rects.
    #[test]
    fn rev_advances_only_on_band_content_change() {
        let mut r = Renderer::from_system(16.0, Theme::default()).expect(crate::NO_FONT);
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
            alpha: 0,
            color2: 0x0010_2030,
            alpha2: 0,
        }];
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 0, "grid-interior emissions must not tick");

        // ... even when it moves (typing under a glow deep in the grid).
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
            alpha: 0,
            color2: 0x0020_1008,
            alpha2: 0,
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

    /// Pointer stability: content re-rasters write in place (the host may hold its view
    /// across frames of one geometry); only a geometry change re-derives the allocation.
    #[test]
    fn spill_ptr_stays_stable_across_content_re_rasters() {
        let mut r = Renderer::from_system(16.0, Theme::default()).expect(crate::NO_FONT);
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
            alpha: 0,
            color2: 0x0020_2020,
            alpha2: 0,
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

    /// 0/0 chrome has no band (no bytes, no revision ticks, no rects), and chrome→0
    /// clears with exactly one tick.
    #[test]
    fn zero_chrome_is_identity_and_transitions_clear() {
        let mut r = Renderer::from_system(16.0, Theme::default()).expect(crate::NO_FONT);
        let mut term = Terminal::new(6, 20);
        let mut input = term.cell_frame(6, 20);
        input.cursor_glow_add = vec![GlowQuad {
            row: 0,
            x: 0,
            y: 0,
            w: 10,
            h: 10,
            color: 0x0020_2020,
            // `alpha: 0` is additive light (see `GlowQuad::alpha`).
            alpha: 0,
            color2: 0x0020_2020,
            alpha2: 0,
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

        // Chrome leaves: one clearing tick, then still.
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
        let mut r = Renderer::from_system(16.0, Theme::default()).expect(crate::NO_FONT);
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
