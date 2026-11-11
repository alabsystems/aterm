// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Spill-band rasterizer.

use aterm_core::render::{FxBlend, FxInk, FxLayer, FxSlot, FxSpace, GlowEnd, GlowRamp, PxRect};

use crate::fire_field;
use crate::fx_raster::{self, FxCanvas, FxFrame, FxScratch, FxSlice, Knock};
use crate::{
    FireMode, FirePatch, GlowQuad, GlowRamps, HaloMode, RainHalo, RenderInput, Renderer, add_sat,
    halo_row_ny, halo_weight, over_rgb, premul_rgb,
};

/// The band geometry a spill buffer was laid out for. `pad_top` is explicit because
/// moving only the asymmetric grid origin keeps the frame size but changes every
/// horizontal strip split.
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
    #[inline]
    const fn grid_top(&self) -> usize {
        self.pad_top + self.head
    }

    /// The trailing band absorbs the top-pad reduction so the fixed frame
    /// extent still contains exactly `2 * pad` vertical padding pixels.
    #[inline]
    const fn trailing_h(&self) -> usize {
        self.pad.saturating_mul(2).saturating_sub(self.pad_top)
    }

    #[inline]
    const fn grid_bottom(&self) -> usize {
        self.frame_h.saturating_sub(self.trailing_h())
    }

    /// The four band strips in packed buffer order (top including head, bottom, left,
    /// right), each row-major, concatenated.
    #[inline]
    const fn strips(&self) -> [StripRect; 4] {
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
    /// nothing of it lies in the frame.
    #[inline]
    const fn frame_rect(&self, r: PxRect) -> Option<Rect> {
        #[inline]
        const fn clamp(v: i32, hi: usize) -> usize {
            if v <= 0 {
                0
            } else {
                let u = v as usize;
                if u < hi { u } else { hi }
            }
        }
        let (x0, x1) = (clamp(r.x0, self.frame_w), clamp(r.x1, self.frame_w));
        let (y0, y1) = (clamp(r.y0, self.frame_h), clamp(r.y1, self.frame_h));
        if x0 < x1 && y0 < y1 {
            Some((x0, y0, x1 - x0, y1 - y0))
        } else {
            None
        }
    }

    /// The window pixels `layer` may write ([`FxLayer::extent`] at its space's
    /// origin), clipped to the frame.
    #[inline]
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
    #[inline]
    const fn band_px(&self) -> usize {
        let s = self.strips();
        s[0].w * s[0].h + s[1].w * s[1].h + s[2].w * s[2].h + s[3].w * s[3].h
    }

    /// The rows of `ramp` whose lights reach the band, as `(top, rows)`: a
    /// light reaches it where its rect does ([`Self::rect_touches_band`]), a
    /// run whole where any of its rows does, and the span runs from the
    /// first such lit row to the last; `None` where none does.
    fn band_rows(&self, ramp: &GlowRamp, ends: &[GlowEnd]) -> Option<(u16, u16)> {
        let (x, y, w) = (
            usize::from(ramp.x),
            usize::from(ramp.y),
            usize::from(ramp.w),
        );
        let xe = (x + w).min(self.frame_w);
        if x >= xe || y >= self.frame_h {
            return None;
        }
        // Rows past the frame's foot reach nothing.
        let row_cap = usize::from(ramp.h).min(self.frame_h - y);
        let mut end = row_cap;
        let mut top = 0;
        if x >= self.pad && xe <= self.frame_w - self.pad {
            // Inside the grid's columns: only the rows above its top and those
            // at and below its foot (the rows between, inside the grid box,
            // spill nothing and widen no dirty rect past theirs).
            let above = self.grid_top().saturating_sub(y).min(end);
            let foot = self.grid_bottom().saturating_sub(y);
            match (above > 0, foot < end) {
                (false, false) => return None,
                (false, true) => top = foot,
                (true, false) => end = above,
                (true, true) => {}
            }
        }
        let (mut top, mut end) = (top as u16, end as u16);
        if ramp.joined > 0 {
            // The run widens the span to its rows, but not past the foot.
            let (j0, j1) = (ramp.join, (ramp.join + ramp.joined + 1).min(row_cap as u16));
            if j0 < end && top < j1 {
                (top, end) = (top.min(j0), end.max(j1));
            }
        }
        let dark = |r: u16| {
            let (a, b) = ramp.row_ends(ends, r);
            a | b == 0
        };
        while top < end && dark(top) {
            top += 1;
        }
        while end > top && dark(end - 1) {
            end -= 1;
        }
        (top < end).then_some((top, end - top))
    }

    /// Whether a frame-space rect reaches the band: non-empty after the frame clip and
    /// not inside the grid box.
    #[inline]
    const fn rect_touches_band(&self, (x, y, w, h): Rect) -> bool {
        let xe_raw = x + w;
        let ye_raw = y + h;
        let xe = if xe_raw < self.frame_w {
            xe_raw
        } else {
            self.frame_w
        };
        let ye = if ye_raw < self.frame_h {
            ye_raw
        } else {
            self.frame_h
        };
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
/// round-half).
#[inline]
const fn cover_union(a: u8, s: u8) -> u8 {
    ((s as u32) + ((a as u32) * (255 - (s as u32)) + 127) / 255) as u8
}

/// The max RGB channel of a premultiplied light colour — the pseudo-alpha an
/// additive emission contributes to the spill coverage.
#[inline]
const fn max_channel(premul: u32) -> u8 {
    let r = (premul >> 16) & 0xff;
    let g = (premul >> 8) & 0xff;
    let b = premul & 0xff;
    let rg = if r > g { r } else { g };
    (if rg > b { rg } else { b }) as u8
}

/// Solve one band pixel to a straight `(r, g, b, a)` with `over_rgb(base, rgb, a) == c`
/// per channel: `c` is the pixel after the band's op replay over `base`, `a0` the
/// accumulated coverage.
#[inline]
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
            (255 * (t[k] - b[k]) - 127).div_ceil(255 - b[k])
        } else if t[k] < b[k] {
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

/// The engine half of the cross-pane effects compositor.
#[derive(Default)]
pub struct SpillBand {
    geom: SpillGeom,
    /// The band's base bg (frame default-bg RGB) the solve is exact over.
    base_bg: u32,
    /// Whether `HaloMode::Over` veils are excluded from the spill (inverted so the
    /// derived default includes them and the byte-parity law holds out of the box).
    veils_excluded: bool,
    rev: u32,
    /// The exported straight-alpha RGBA strips (packed as `SpillGeom::strips`). The
    /// allocation moves only on a geometry change: the host's pointer-stability contract.
    rgba: Vec<u8>,
    /// This update's dirty rects, packed `x,y,w,h` frame-absolute device px.
    rects: Vec<i32>,
    color: Vec<u32>,
    cover: Vec<u8>,
    prev: BandStreams,
    cur: BandStreams,
    /// The fx rasterizer's resident scratch (occluder pieces, sprite column
    /// tables), so a replay allocates nothing once warm.
    fx_scratch: FxScratch,
}

/// One frame's band-clipped emissions, per window-absolute stream: a glow
/// stream's quads with its ramps' rows that reach the band.
#[derive(Default, PartialEq)]
struct BandStreams {
    glow_under: (Vec<GlowQuad>, GlowRamps),
    fire: Vec<FirePatch>,
    glow: (Vec<GlowQuad>, GlowRamps),
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
    #[inline]
    const fn is_empty(&self) -> bool {
        self.glow_under.0.is_empty()
            && self.glow_under.1.is_empty()
            && self.fire.is_empty()
            && self.glow.0.is_empty()
            && self.glow.1.is_empty()
            && self.halo.is_empty()
            && self.fx.is_empty()
    }
}

type Rect = (usize, usize, usize, usize);

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

/// [`clip_stream`] for a glow stream: its quads that reach the band and,
/// of each ramp, the rows whose lights do ([`SpillGeom::band_rows`]), in
/// draw order.
fn clip_glow(
    (dst, dst_ramps): &mut (Vec<GlowQuad>, GlowRamps),
    (src, ramps): (&[GlowQuad], &GlowRamps),
    rows: usize,
    geom: SpillGeom,
) {
    dst.clear();
    dst_ramps.clear();
    use crate::glow_ramps::Lights;
    crate::glow_ramps::each_laid(src, ramps, |laid| match laid {
        Lights::Quads(run) => dst.extend(
            run.iter()
                .filter(|q| q.row() < rows && geom.rect_touches_band(q.rect()))
                .copied(),
        ),
        Lights::Ramp(ramp) => {
            if usize::from(ramp.row) < rows
                && let Some(kept) = geom.band_rows(ramp, &ramps.ends)
            {
                dst_ramps.copy_rows(ramp, &ramps.ends, dst.len(), kept);
            }
        }
    });
}

/// A band glow stream's lights' rects, its ramps' each the rows it keeps.
fn glow_rects((quads, ramps): &(Vec<GlowQuad>, GlowRamps)) -> impl Iterator<Item = Rect> + '_ {
    quads
        .iter()
        .map(Emission::rect)
        .chain(ramps.items.iter().map(|r| {
            (
                usize::from(r.x),
                usize::from(r.y),
                usize::from(r.w),
                usize::from(r.h),
            )
        }))
}

impl SpillBand {
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Band-content revision.
    #[inline]
    pub const fn rev(&self) -> u32 {
        self.rev
    }

    /// The last update's dirty rects, packed `x,y,w,h` (4 `i32`s per rect).
    #[inline]
    pub const fn rects(&self) -> &[i32] {
        self.rects.as_slice()
    }

    /// The straight-alpha RGBA strip buffer.
    #[inline]
    pub const fn rgba(&self) -> &[u8] {
        self.rgba.as_slice()
    }

    /// Include `HaloMode::Over` veils in the spill (default `true`).
    #[inline]
    pub const fn set_include_veils(&mut self, on: bool) {
        self.veils_excluded = !on;
    }

    /// Refresh the spill for the frame `input` the renderer just composed.
    pub fn update(&mut self, renderer: &Renderer, input: &RenderInput) {
        let geom = SpillGeom {
            pad: renderer.pad(),
            pad_top: renderer.pad_top(),
            head: renderer.head(),
            frame_w: 0,
            frame_h: 0,
        };
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

        let (rows, cur) = (input.rows, &mut self.cur);
        clip_glow(
            &mut cur.glow_under,
            (&input.glow_under, &input.glow_under_ramps),
            rows,
            geom,
        );
        clip_stream(&mut cur.fire, &input.fire_patch, rows, geom, |_| true);
        clip_glow(
            &mut cur.glow,
            (&input.cursor_glow_add, &input.cursor_glow_add_ramps),
            rows,
            geom,
        );
        let include_veils = !self.veils_excluded;
        clip_stream(&mut cur.halo, &input.glow_halo, rows, geom, |q| {
            include_veils || q.mode == HaloMode::Add
        });
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
            self.rects.clear();
            return;
        }
        if geom_changed {
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
            self.rects.clear();
        } else {
            self.emit_dirty_rects(geom_changed || bg_changed);
            self.rasterize(renderer.cell_size().1, rows);
            self.rev = self.rev.wrapping_add(1);
        }
        std::mem::swap(&mut self.prev, &mut self.cur);
    }

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
        for r in glow_rects(&self.prev.glow_under).chain(glow_rects(&self.cur.glow_under)) {
            grow(r);
        }
        for q in self.prev.fire.iter().chain(&self.cur.fire) {
            grow(q.rect());
        }
        for r in glow_rects(&self.prev.glow).chain(glow_rects(&self.cur.glow)) {
            grow(r);
        }
        for q in self.prev.halo.iter().chain(&self.cur.halo) {
            grow(q.rect());
        }
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
            let mut fx =
                |color: &mut [u32], cover: &mut [u8], (slot, blend, knock): (_, _, Knock)| {
                    if cur.fx.iter().any(|l| {
                        l.slot == slot
                            && l.blend == blend
                            && knock.takes(l)
                            && geom
                                .layer_rect(l)
                                .is_some_and(|r| clip_to_strip(s, r).is_some())
                    }) {
                        fx_raster::draw_picked_layers_on(
                            &mut StripCanvas { s, color, cover },
                            &frame,
                            &cur.fx,
                            &cur.fx_occluders,
                            (slot, blend, knock),
                            None,
                            fx_scratch,
                        );
                    }
                };
            fx(color, cover, (FxSlot::Under, FxBlend::Linear, Knock::Not));
            replay_glow(color, cover, s, &cur.glow_under);
            fx(color, cover, (FxSlot::Under, FxBlend::Exact, Knock::Any));
            replay_fire(color, cover, s, pad, &cur.fire);
            fx(color, cover, (FxSlot::Under, FxBlend::Linear, Knock::Only));
            replay_glow(color, cover, s, &cur.glow);
            fx(color, cover, (FxSlot::Glow, FxBlend::Exact, Knock::Any));
            replay_halo(color, cover, s, &cur.halo);
            fx(color, cover, (FxSlot::Over, FxBlend::Exact, Knock::Any));
            fx(color, cover, (FxSlot::Over, FxBlend::Linear, Knock::Any));
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
        if c0 == c1 {
            self.flat(y, x0, x1, c0.color, c0.alpha);
            return;
        }
        let k = self.span(y, x0, x1);
        fx_raster::fill_glow_span(&mut self.color[k.clone()], i0, qw, c0, c1);
        if c0.alpha == c1.alpha {
            let a = c0.alpha;
            for (j, v) in self.cover[k].iter_mut().enumerate() {
                let i = i0 + j as u32;
                let c = crate::glow_lerp_rgb(c0.color, c1.color, i, qw);
                *v = cover_union(*v, a.max(max_channel(c)));
            }
            return;
        }
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

/// Replay a band glow stream in draw order: its quads, and each ramp's
/// lit rows as the quads they stand for, read off its ends in place.
fn replay_glow(
    color: &mut [u32],
    cover: &mut [u8],
    s: StripRect,
    (quads, ramps): &(Vec<GlowQuad>, GlowRamps),
) {
    use crate::glow_ramps::Lights;
    crate::glow_ramps::each_laid(quads, ramps, |laid| match laid {
        Lights::Quads(run) => replay_flat_add(color, cover, s, run.iter().copied()),
        Lights::Ramp(ramp) => replay_flat_add(color, cover, s, ramp.quads(&ramps.ends)),
    });
}

fn replay_flat_add(
    color: &mut [u32],
    cover: &mut [u8],
    s: StripRect,
    quads: impl Iterator<Item = GlowQuad>,
) {
    for q in quads {
        let Some((x0, y0, xe, ye)) = clip_to_strip(s, q.rect()) else {
            continue;
        };
        if q.is_flat() {
            let a = q.alpha;
            let c = q.color;
            let ap = a.max(max_channel(c));
            for y in y0..ye {
                let row = (y - s.y) * s.w;
                let span = row + (x0 - s.x)..row + (xe - s.x);
                fx_raster::fill_flat(&mut color[span.clone()], c, a);
                for v in &mut cover[span] {
                    *v = cover_union(*v, ap);
                }
            }
            continue;
        }
        let (a0, a1) = (u32::from(q.alpha), u32::from(q.alpha2));
        let qw = u32::from(q.w);
        for x in x0..xe {
            let i = (x - q.x as usize) as u32;
            let a = if a0 == a1 {
                q.alpha
            } else {
                crate::glow_lerp(a0, a1, i, qw) as u8
            };
            let c = crate::glow_lerp_rgb(q.color, q.color2, i, qw);
            let ap = a.max(max_channel(c));
            for y in y0..ye {
                let k = (y - s.y) * s.w + (x - s.x);
                color[k] = crate::over_premul(color[k], c, a);
                cover[k] = cover_union(cover[k], ap);
            }
        }
    }
}

fn replay_halo(color: &mut [u32], cover: &mut [u8], s: StripRect, quads: &[RainHalo]) {
    for mode in [HaloMode::Add, HaloMode::Over] {
        for q in quads.iter().filter(|q| q.mode == mode) {
            let Some((x0, y0, xe, ye)) = clip_to_strip(s, q.rect()) else {
                continue;
            };
            let (cx, cy) = (q.cx as i32, q.cy as i32);
            let rx2 = (q.rx as i32) * (q.rx as i32);
            let ry2 = (q.ry as i32) * (q.ry as i32);
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
#[path = "spill_tests.rs"]
mod tests;
