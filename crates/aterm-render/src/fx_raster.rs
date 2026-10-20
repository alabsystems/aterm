// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The CPU rasterizer of the fx layers (`docs/DESIGN-card-engine-2026-10-19.md`
//! §4.2-§4.3): pixel law F, the engine's band slicing and the occluder
//! subtraction.
//!
//! **Pixel law F.** For a window pixel `(px, py)` of an item whose origin is
//! `O = item.xy + layer.offset` (plus `(pad, grid_top)` in Grid space):
//!
//! * Rect: `i = px − O.x`, `c = glow_lerp_rgb(c0, c1, i, w)`,
//!   `a = glow_lerp(a0, a1, i, w)` over palette entries `ink`, `ink + 1`.
//! * Ramps: row `r = py − O.y` is the Rect rule over entries `ink + 2r`,
//!   `ink + 2r + 1`.
//! * Sprite: `l = (px, py) − O`, texel `t = (l.x·w / dw, l.y·h / dh)`,
//!   `t.x = w − 1 − t.x` under `flip_x`. Index8 texel `k = 0` writes nothing,
//!   else entry `j = ink + k − 1`, and `j` past the palette writes nothing.
//!   An Rgba8Premul texel is its own `(c, a)`. An Rgba8Straight texel takes
//!   entry `ink` as tint and opacity.
//! * Exact: `dst = over_premul(dst, c, a)` (`add_sat` at `a = 0`, a store at
//!   `a = 255`). Linear: FreeSprite's `blend`.
//!
//! Every term reads the absolute pixel and the UNCLAMPED item origin, never
//! the slice, so cutting an item at a band edge, a clip or an occluder changes
//! no byte (the free-sprite argument). That is what lets the engine own the
//! one-band law: an item's draw rect (its box ∩ its clip ∩ the layer clip ∩
//! the frame, minus every occluder) is cut at the cell-row bands, and a slice
//! is stamped only when its band is being repainted.
//!
//! Sprites are stamped from the art's per-row run lists ([`FxArt::runs`]):
//! the destination rows that read one art row share its runs' spans (from a
//! per-item column table) and paints, resolved once and filled on each row,
//! byte-equal to the per-pixel oracle the tests keep.

use aterm_core::render::{
    FxArt, FxBlend, FxInk, FxItem, FxLayer, FxRamps, FxRect, FxSlot, FxSpace, FxSprite, FxTexels,
    PxRect,
};

use crate::{add_sat, blend, glow_lerp, glow_lerp_rgb, over_premul};

/// Row `r`'s pixel band in a frame of height `h` whose grid starts at
/// `grid_top` with `cell_h`-tall rows: `[grid_top + r·cell_h, grid_top +
/// (r + 1)·cell_h)` clamped to `h`, with row 0 opened to the window top and
/// the last row run to the frame bottom. The ONE statement of the band rule:
/// [`crate::Renderer::row_pixel_band`] (Phase A's band reset, the free-sprite
/// stamp, a presenter's partial copy) and the fx slicer both read it, and it
/// is the GPU scissor's edge rule (`frame_load_and_scissor`).
pub(crate) fn band_px(
    r: usize,
    rows: usize,
    h: usize,
    grid_top: usize,
    cell_h: usize,
) -> (usize, usize) {
    let y0 = if r == 0 {
        0
    } else {
        (grid_top + r * cell_h).min(h)
    };
    let y1 = if r + 1 >= rows {
        h
    } else {
        (grid_top + (r + 1) * cell_h).min(h)
    };
    (y0, y1)
}

/// The frame the engine slices against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FxFrame {
    /// Framebuffer width in pixels.
    pub(crate) w: usize,
    /// Framebuffer height in pixels.
    pub(crate) h: usize,
    /// The grid's left edge (the Grid-space x origin).
    pub(crate) pad: usize,
    /// The grid's top edge (the Grid-space y origin and the band base).
    pub(crate) grid_top: usize,
    /// Cell height: the band pitch.
    pub(crate) cell_h: usize,
    /// Grid rows: the band count.
    pub(crate) rows: usize,
}

impl FxFrame {
    fn band(&self, b: usize) -> (usize, usize) {
        band_px(b, self.rows, self.h, self.grid_top, self.cell_h)
    }

    /// The band holding pixel row `y` (`y < h`, `rows > 0`).
    fn band_of(&self, y: usize) -> usize {
        if y < self.grid_top {
            0
        } else {
            ((y - self.grid_top) / self.cell_h.max(1)).min(self.rows - 1)
        }
    }
}

/// A half-open rectangle in `i64` window pixels: the slicer's working type,
/// wide enough that no item origin, offset or grid shift can overflow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Area {
    x0: i64,
    y0: i64,
    x1: i64,
    y1: i64,
}

impl Area {
    /// `r` moved by `(dx, dy)`; an unbounded [`PxRect`] edge stays unbounded.
    fn of(r: PxRect, dx: i64, dy: i64) -> Self {
        let edge = |e: i32, d: i64| match e {
            i32::MIN => i64::MIN,
            i32::MAX => i64::MAX,
            e => i64::from(e) + d,
        };
        Self {
            x0: edge(r.x0, dx),
            y0: edge(r.y0, dy),
            x1: edge(r.x1, dx),
            y1: edge(r.y1, dy),
        }
    }

    fn at(x: i64, y: i64, w: u16, h: u16) -> Self {
        Self {
            x0: x,
            y0: y,
            x1: x + i64::from(w),
            y1: y + i64::from(h),
        }
    }

    fn is_empty(self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }

    fn intersect(self, o: Self) -> Self {
        Self {
            x0: self.x0.max(o.x0),
            y0: self.y0.max(o.y0),
            x1: self.x1.min(o.x1),
            y1: self.y1.min(o.y1),
        }
    }
}

/// One stamp: a rectangle of window pixels inside the frame and inside one
/// band.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Slice {
    pub(crate) x0: usize,
    pub(crate) x1: usize,
    pub(crate) y0: usize,
    pub(crate) y1: usize,
}

/// The rasterizer's resident scratch: the occluder-subtraction stack, the
/// pieces it leaves, and the current sprite's column table
/// ([`sprite_cols`]). Capacity is kept across frames.
#[derive(Debug, Default)]
pub(crate) struct FxScratch {
    stack: Vec<(Area, usize)>,
    pieces: Vec<Area>,
    cols: Vec<u32>,
}

/// Draw every layer of `slot` and `blend` into `px` (row-major, `f.w` wide).
///
/// `dirty` is the row-repaint gate: `None` on a full repaint, else a slice is
/// stamped only when its band's flag is set, so light never re-accumulates on
/// a row the frame keeps. Layers draw in list order, items in content order.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_layers(
    px: &mut [u32],
    f: &FxFrame,
    layers: &[FxLayer],
    occluders: &[PxRect],
    slot: FxSlot,
    blend: FxBlend,
    dirty: Option<&[bool]>,
    scratch: &mut FxScratch,
) {
    if f.rows == 0 || f.w == 0 || f.h == 0 || px.len() < f.w * f.h {
        return;
    }
    for layer in layers {
        if layer.slot != slot || layer.blend != blend || !layer.content.accepts(blend) {
            continue;
        }
        let Some((shift, clip)) = layer_place(f, layer) else {
            continue;
        };
        let palette = layer.content.palette();
        for item in layer.content.items() {
            // Per item, fail closed: an item the layer's blend does not draw
            // draws nothing.
            if item.blend() != blend {
                continue;
            }
            let (origin, area) = item_place(item, shift);
            let draw = area.intersect(clip);
            if draw.is_empty() {
                continue;
            }
            subtract_occluders(draw, occluders, scratch);
            if let FxItem::Sprite(sp) = item {
                sprite_cols(sp, &mut scratch.cols);
            }
            let FxScratch { pieces, cols, .. } = &*scratch;
            for &piece in pieces {
                for_each_slice(f, piece, dirty, |s| {
                    stamp(px, f.w, s, item, origin, palette, cols)
                });
            }
        }
    }
}

/// Where `layer` lands in the frame: the window-pixel shift of its content
/// origin (offset plus the Grid origin) and its clip in window pixels within
/// the frame, or `None` when that clip is empty.
fn layer_place(f: &FxFrame, layer: &FxLayer) -> Option<((i64, i64), Area)> {
    let (gx, gy) = match layer.space {
        FxSpace::Window => (0, 0),
        FxSpace::Grid => (f.pad as i64, f.grid_top as i64),
    };
    let frame = Area {
        x0: 0,
        y0: 0,
        x1: f.w as i64,
        y1: f.h as i64,
    };
    let clip = Area::of(layer.clip, gx, gy).intersect(frame);
    (!clip.is_empty()).then_some((
        (
            i64::from(layer.offset.0) + gx,
            i64::from(layer.offset.1) + gy,
        ),
        clip,
    ))
}

/// An item's window-pixel origin `O` under the layer shift `(lx, ly)`, and its
/// box at `O` within its own clip (a sprite's), before the layer clip.
fn item_place(item: &FxItem, (lx, ly): (i64, i64)) -> ((i64, i64), Area) {
    match item {
        FxItem::Rect(r) => {
            let o = (i64::from(r.x) + lx, i64::from(r.y) + ly);
            (o, Area::at(o.0, o.1, r.w, r.h))
        }
        FxItem::Ramps(r) => {
            let o = (i64::from(r.x) + lx, i64::from(r.y) + ly);
            (o, Area::at(o.0, o.1, r.w, r.h))
        }
        FxItem::Sprite(s) => {
            let o = (i64::from(s.x) + lx, i64::from(s.y) + ly);
            (
                o,
                Area::at(o.0, o.1, s.dw, s.dh).intersect(Area::of(s.clip, lx, ly)),
            )
        }
    }
}

/// `draw` minus every occluder, as disjoint pieces in `scratch.pieces`.
///
/// Each occluder cuts every piece it overlaps into at most four (above,
/// below, left, right), so any number of occluders compose; an explicit stack
/// keeps the depth off the call stack.
fn subtract_occluders(draw: Area, occluders: &[PxRect], scratch: &mut FxScratch) {
    scratch.pieces.clear();
    if occluders.is_empty() {
        scratch.pieces.push(draw);
        return;
    }
    scratch.stack.clear();
    scratch.stack.push((draw, 0));
    while let Some((r, i)) = scratch.stack.pop() {
        let Some(&o) = occluders.get(i) else {
            scratch.pieces.push(r);
            continue;
        };
        let cut = r.intersect(Area::of(o, 0, 0));
        if cut.is_empty() {
            scratch.stack.push((r, i + 1));
            continue;
        }
        let parts = [
            Area { y1: cut.y0, ..r },
            Area { y0: cut.y1, ..r },
            Area {
                y0: cut.y0,
                x1: cut.x0,
                y1: cut.y1,
                ..r
            },
            Area {
                x0: cut.x1,
                y0: cut.y0,
                y1: cut.y1,
                ..r
            },
        ];
        for p in parts {
            if !p.is_empty() {
                scratch.stack.push((p, i + 1));
            }
        }
    }
}

/// Cut `piece` (inside the frame) at the band edges and hand each slice whose
/// band is being repainted to `stamp`.
fn for_each_slice(f: &FxFrame, piece: Area, dirty: Option<&[bool]>, mut stamp: impl FnMut(Slice)) {
    let (x0, x1) = (piece.x0 as usize, piece.x1 as usize);
    let (y0, y1) = (piece.y0 as usize, piece.y1 as usize);
    let mut b = f.band_of(y0);
    loop {
        let (by0, by1) = f.band(b);
        let (sy0, sy1) = (y0.max(by0), y1.min(by1));
        if sy0 < sy1 && dirty.is_none_or(|d| d.get(b).copied().unwrap_or(false)) {
            stamp(Slice {
                x0,
                x1,
                y0: sy0,
                y1: sy1,
            });
        }
        if by1 >= y1 || b + 1 >= f.rows {
            break;
        }
        b += 1;
    }
}

/// Palette entry `j`, or `None` past the end (pixel law F: writes nothing).
#[inline]
fn ink_at(palette: &[FxInk], j: u64) -> Option<FxInk> {
    usize::try_from(j)
        .ok()
        .and_then(|j| palette.get(j).copied())
}

/// Stamp one item over one slice, addressed from the item origin `o`. `cols`
/// is a sprite's column table ([`sprite_cols`]); a rect ignores it.
pub(crate) fn stamp(
    px: &mut [u32],
    w: usize,
    s: Slice,
    item: &FxItem,
    o: (i64, i64),
    palette: &[FxInk],
    cols: &[u32],
) {
    match item {
        FxItem::Rect(r) => stamp_rect(px, w, s, o, r, palette),
        FxItem::Ramps(r) => stamp_ramps(px, w, s, o, r, palette),
        FxItem::Sprite(sp) => stamp_sprite(px, w, s, o, sp, palette, cols),
    }
}

/// A sprite's column table: `cols[d] = ⌈d·dw / w⌉` for `d` in `0..=w`, the
/// first destination column whose displayed index `l·w / dw` is at least
/// `d`. Texel run `[d0, d1)` covers destination columns
/// `[cols[d0], cols[d1])`. Built once per item (not per run and row: two
/// divisions per run per row were a fifth of the stamp), into `out`'s kept
/// capacity; empty for an empty art.
pub(crate) fn sprite_cols(sp: &FxSprite, out: &mut Vec<u32>) {
    out.clear();
    let (aw, dw) = (u64::from(sp.art.w()), u64::from(sp.dw));
    if aw == 0 {
        return;
    }
    // `⌈d·dw / w⌉ <= dw <= u16::MAX`: the cast is lossless.
    out.extend((0..=aw).map(|d| (d * dw).div_ceil(aw) as u32));
}

fn stamp_rect(px: &mut [u32], w: usize, s: Slice, o: (i64, i64), r: &FxRect, palette: &[FxInk]) {
    let j = u64::from(r.ink);
    let (Some(c0), Some(c1)) = (ink_at(palette, j), ink_at(palette, j + 1)) else {
        return;
    };
    // The ramp index of the slice's first column, from the item origin.
    let i0 = (s.x0 as i64 - o.0) as u32;
    for y in s.y0..s.y1 {
        fill_glow_span(
            &mut px[y * w + s.x0..y * w + s.x1],
            i0,
            u32::from(r.w),
            c0,
            c1,
        );
    }
}

fn stamp_ramps(px: &mut [u32], w: usize, s: Slice, o: (i64, i64), r: &FxRamps, palette: &[FxInk]) {
    let i0 = (s.x0 as i64 - o.0) as u32;
    for y in s.y0..s.y1 {
        let j = u64::from(r.ink) + 2 * (y as i64 - o.1) as u64;
        let (Some(c0), Some(c1)) = (ink_at(palette, j), ink_at(palette, j + 1)) else {
            continue;
        };
        fill_glow_span(
            &mut px[y * w + s.x0..y * w + s.x1],
            i0,
            u32::from(r.w),
            c0,
            c1,
        );
    }
}

/// The linear-light paint of a straight texel `A<<24 | RGB` under `tint`:
/// FreeSprite's law, `(mul8(rgb, tint.rgb), mul8(A, tint.alpha))`.
#[inline]
fn straight_paint(texel: u32, tint: FxInk) -> (u32, u8) {
    let mul8 = |c: u32, f: u32| (c * f + 127) / 255;
    let ch = |sh: u32| mul8((texel >> sh) & 0xff, (tint.color >> sh) & 0xff);
    (
        ch(16) << 16 | ch(8) << 8 | ch(0),
        mul8(texel >> 24, u32::from(tint.alpha)) as u8,
    )
}

/// The Linear tint a sprite draws with, or `None` when it draws nothing
/// (an Exact art needs none: `Some(FxInk::default())`).
#[inline]
fn sprite_tint(art: &FxArt, sp: &FxSprite, palette: &[FxInk]) -> Option<FxInk> {
    match art.texels() {
        FxTexels::Rgba8Straight => ink_at(palette, u64::from(sp.ink)),
        FxTexels::Index8 | FxTexels::Rgba8Premul => Some(FxInk::default()),
    }
}

fn stamp_sprite(
    px: &mut [u32],
    w: usize,
    s: Slice,
    o: (i64, i64),
    sp: &FxSprite,
    palette: &[FxInk],
    cols: &[u32],
) {
    let art = &*sp.art;
    if art.is_empty() || sp.dw == 0 || sp.dh == 0 {
        return;
    }
    debug_assert_eq!(
        cols.len(),
        usize::from(art.w()) + 1,
        "the sprite's own table"
    );
    let Some(tint) = sprite_tint(art, sp, palette) else {
        return;
    };
    let art_w = usize::from(art.w());
    let (ah, dh) = (u64::from(art.h()), u64::from(sp.dh));
    let kind = art.texels();
    let runs = art.runs();
    let mut y = s.y0;
    while y < s.y1 {
        // Destination row `m` reads art row `ty = m·h / dh`, and so do the
        // rows up to `⌈(ty + 1)·dh / h⌉`: the whole group shares its run
        // spans and paints, resolved once (the quad stream's shape: a span
        // computed once and filled on every row it covers).
        let m = (y as i64 - o.1) as u64;
        let ty = m * ah / dh;
        let y_end = ((o.1 + ((ty + 1) * dh).div_ceil(ah) as i64) as usize).min(s.y1);
        debug_assert!(y_end > y, "row {m} lies in art row {ty}'s group");
        for run in runs.row(ty as usize) {
            // The run's texels `[t0, t1)` show where the displayed index
            // `l·w / dw` lies in `[d0, d1)` (mirrored under `flip_x`), i.e. at
            // destination columns `[cols[d0], cols[d1])`. Runs are disjoint,
            // so their spans are, and the order they stamp in changes no byte.
            let (t0, t1) = (
                usize::from(run.x),
                usize::from(run.x) + usize::from(run.len),
            );
            let (d0, d1) = if sp.flip_x {
                (art_w - t1, art_w - t0)
            } else {
                (t0, t1)
            };
            let x0 = (o.0 + i64::from(cols[d0])).max(s.x0 as i64);
            let x1 = (o.0 + i64::from(cols[d1])).min(s.x1 as i64);
            if x0 >= x1 {
                continue;
            }
            let (x0, x1) = (x0 as usize, x1 as usize);
            let rows = (y..y_end).map(|r| r * w);
            match kind {
                FxTexels::Index8 => {
                    let j = u64::from(sp.ink) + u64::from(run.value) - 1;
                    if let Some(ink) = ink_at(palette, j) {
                        for r in rows {
                            fill_flat(&mut px[r + x0..r + x1], ink.color, ink.alpha);
                        }
                    }
                }
                FxTexels::Rgba8Premul => {
                    let (c, a) = (run.value & 0x00FF_FFFF, (run.value >> 24) as u8);
                    for r in rows {
                        fill_flat(&mut px[r + x0..r + x1], c, a);
                    }
                }
                FxTexels::Rgba8Straight => {
                    let (c, a) = straight_paint(run.value, tint);
                    if a != 0 {
                        for r in rows {
                            for p in &mut px[r + x0..r + x1] {
                                *p = blend(*p, c, a);
                            }
                        }
                    }
                }
            }
        }
        y = y_end;
    }
}

/// One span of a [`GlowQuad`](aterm_core::render::GlowQuad)-law rect: column
/// `k` of `span` is ramp column `i0 + k` of a `qw`-wide ramp from `c0` to
/// `c1`. Shared with `Renderer::draw_flat_add`, so the glow streams and the fx
/// rects are one loop.
#[inline]
pub(crate) fn fill_glow_span(span: &mut [u32], i0: u32, qw: u32, c0: FxInk, c1: FxInk) {
    if c0 == c1 {
        fill_flat(span, c0.color, c0.alpha);
        return;
    }
    let (a0, a1) = (u32::from(c0.alpha), u32::from(c1.alpha));
    for (k, p) in span.iter_mut().enumerate() {
        let i = i0 + k as u32;
        let a = glow_lerp(a0, a1, i, qw) as u8;
        let c = glow_lerp_rgb(c0.color, c1.color, i, qw);
        *p = over_premul(*p, c, a);
    }
}

/// A flat span of the Exact law: `add_sat` at alpha 0, a store at 255,
/// `over_premul` between. The three arms are one equation
/// (`over_premul(d, c, 0) == add_sat(d, c)`, `over_premul(d, c, 255)` is `c`
/// over `d`'s transmittance byte), specialized out of the pixel loop.
#[inline]
pub(crate) fn fill_flat(span: &mut [u32], c: u32, a: u8) {
    match a {
        0 => {
            for p in span {
                *p = add_sat(*p, c);
            }
        }
        255 => {
            let c = c & 0x00FF_FFFF;
            for p in span {
                *p = (*p & 0xFF00_0000) | c;
            }
        }
        _ => {
            for p in span {
                *p = over_premul(*p, c, a);
            }
        }
    }
}

/// The per-pixel statement of pixel law F for a sprite slice: the oracle the
/// run-list stamp is held to. It reads every texel through the law itself and
/// never through [`FxTexels::is_clear`] (the run-list builder's filter), so a
/// filter that dropped drawing texels (an additive Premul one, `a = 0` with
/// colour) would diverge here instead of hiding in both.
#[cfg(test)]
pub(crate) fn stamp_sprite_per_pixel(
    px: &mut [u32],
    w: usize,
    s: Slice,
    o: (i64, i64),
    sp: &FxSprite,
    palette: &[FxInk],
    texel_bias: usize,
) {
    let art = &*sp.art;
    if art.is_empty() || sp.dw == 0 || sp.dh == 0 {
        return;
    }
    let Some(tint) = sprite_tint(art, sp, palette) else {
        return;
    };
    let (aw, ah) = (u64::from(art.w()), u64::from(art.h()));
    let (dw, dh) = (u64::from(sp.dw), u64::from(sp.dh));
    for y in s.y0..s.y1 {
        for x in s.x0..s.x1 {
            let (l, m) = ((x as i64 - o.0) as u64, (y as i64 - o.1) as u64);
            let mut tx = (l * aw / dw) as usize;
            if sp.flip_x {
                tx = art.w() as usize - 1 - tx;
            }
            // `texel_bias` is the tests' off-by-one control.
            let tx = (tx + texel_bias).min(art.w() as usize - 1);
            let v = art.texel(tx, (m * ah / dh) as usize);
            let p = &mut px[y * w + x];
            match art.texels() {
                // Index 0 writes nothing; `k` names entry `ink + k − 1`.
                FxTexels::Index8 => {
                    if v != 0
                        && let Some(ink) = ink_at(palette, u64::from(sp.ink) + u64::from(v) - 1)
                    {
                        *p = over_premul(*p, ink.color, ink.alpha);
                    }
                }
                // Every Premul texel is its own paint: an all-zero one is
                // `add_sat(dst, 0)`, a no-op by the law, not by a filter.
                FxTexels::Rgba8Premul => *p = over_premul(*p, v & 0x00FF_FFFF, (v >> 24) as u8),
                FxTexels::Rgba8Straight => {
                    let (c, a) = straight_paint(v, tint);
                    if a != 0 {
                        *p = blend(*p, c, a);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "fx_raster_tests.rs"]
mod tests;
