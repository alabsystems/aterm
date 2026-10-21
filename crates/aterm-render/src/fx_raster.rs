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
//! column table per art width and size, [`SpriteCols`]) and paints, resolved
//! once and filled on each row, byte-equal to the per-pixel oracle the tests
//! keep.
//!
//! **One slicer.** [`slice_layers`] is the engine's only statement of which
//! slices a frame draws: this rasterizer stamps them, the GPU renderer turns
//! each into one instance, and [`fx_bloom_quads`] into the bloom source. So
//! the band gate, the clips and the occluder subtraction cannot drift between
//! the backends.

use std::sync::Arc;

use aterm_core::render::{
    FX_FRAME_ART_TEXELS, FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRamps, FxRect,
    FxSlot, FxSpace, FxSprite, FxTexels, GlowQuad, PxRect,
};
use aterm_hash::FxHashSet;

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

/// The frame the engine slices against ([`crate::Renderer::fx_frame`] gives
/// a renderer's own).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FxFrame {
    /// Framebuffer width in pixels.
    pub w: usize,
    /// Framebuffer height in pixels.
    pub h: usize,
    /// The grid's left edge (the Grid-space x origin).
    pub pad: usize,
    /// The grid's top edge (the Grid-space y origin and the band base).
    pub grid_top: usize,
    /// Cell height: the band pitch.
    pub cell_h: usize,
    /// Grid rows: the band count.
    pub rows: usize,
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
pub struct FxSlice {
    /// Left edge (inclusive).
    pub x0: usize,
    /// Right edge (exclusive).
    pub x1: usize,
    /// Top edge (inclusive).
    pub y0: usize,
    /// Bottom edge (exclusive).
    pub y1: usize,
}

/// The internal canvas spelling retained for spill replay.
pub(crate) type Slice = FxSlice;

/// Where the stamps land: the frame's pixels ([`FramePx`]) or a spill
/// strip's replay planes (`crate::spill`). Every span is in window pixels,
/// inside [`Self::bounds`], and carries pixel law F's paint for those pixels;
/// a target applies it with the frame's kernels and may note what else it
/// needs (the spill's coverage), so the frame and its replays cannot drift.
pub(crate) trait FxCanvas {
    /// The window pixels this target holds; nothing outside is handed to it.
    fn bounds(&self) -> Slice;
    /// The Exact flat paint `(c, a)` over row `y`, columns `[x0, x1)`
    /// ([`fill_flat`]).
    fn flat(&mut self, y: usize, x0: usize, x1: usize, c: u32, a: u8);
    /// The Exact ramp over row `y`, columns `[x0, x1)`: column `x0 + k` is
    /// ramp column `i0 + k` of a `qw`-wide ramp from `c0` to `c1`
    /// ([`fill_glow_span`]).
    #[allow(clippy::too_many_arguments)]
    fn ramp(&mut self, y: usize, x0: usize, x1: usize, i0: u32, qw: u32, c0: FxInk, c1: FxInk);
    /// The Linear paint `(c, a)` over row `y`, columns `[x0, x1)` (the
    /// FreeSprite [`blend`]; `a > 0`).
    fn linear(&mut self, y: usize, x0: usize, x1: usize, c: u32, a: u8);
}

/// The frame as a stamp target: `px` row-major, `w` wide.
pub(crate) struct FramePx<'a> {
    px: &'a mut [u32],
    w: usize,
}

impl<'a> FramePx<'a> {
    /// The frame `px`, `w` pixels wide (its height is `px.len() / w`).
    pub(crate) fn new(px: &'a mut [u32], w: usize) -> Self {
        Self { px, w }
    }

    #[inline]
    fn span(&mut self, y: usize, x0: usize, x1: usize) -> &mut [u32] {
        &mut self.px[y * self.w + x0..y * self.w + x1]
    }
}

impl FxCanvas for FramePx<'_> {
    fn bounds(&self) -> Slice {
        Slice {
            x0: 0,
            x1: self.w,
            y0: 0,
            y1: self.px.len().checked_div(self.w).unwrap_or(0),
        }
    }

    #[inline]
    fn flat(&mut self, y: usize, x0: usize, x1: usize, c: u32, a: u8) {
        fill_flat(self.span(y, x0, x1), c, a);
    }

    #[inline]
    fn ramp(&mut self, y: usize, x0: usize, x1: usize, i0: u32, qw: u32, c0: FxInk, c1: FxInk) {
        fill_glow_span(self.span(y, x0, x1), i0, qw, c0, c1);
    }

    #[inline]
    fn linear(&mut self, y: usize, x0: usize, x1: usize, c: u32, a: u8) {
        for p in self.span(y, x0, x1) {
            *p = blend(*p, c, a);
        }
    }
}

/// The slicer's resident scratch: the occluder-subtraction stack, the pieces
/// it leaves, and the frame's art budget. Capacity is kept across frames.
#[derive(Debug, Default)]
pub struct FxSlicer {
    stack: Vec<(Area, usize)>,
    pieces: Vec<Area>,
    budget: FxBudget,
}

/// Whether the engine draws `layer` at all: its blend accepts its content,
/// and it is not a Linear layer at Glow (§4.5 has no Linear Glow group).
fn layer_draws(layer: &FxLayer) -> bool {
    layer.content.accepts(layer.blend)
        && !(layer.blend == FxBlend::Linear && layer.slot == FxSlot::Glow)
}

/// Every sprite the frame's layers can draw, in draw order (layers in list
/// order, items in content order): a sprite of its layer's blend, over a
/// non-empty art, at a non-zero size, in a layer the engine draws. Its clip
/// and the frame do not matter: the art budget is ruled on what the layers
/// hold, so a sprite's drawing never depends on damage or scroll.
pub fn for_each_drawn_sprite<'a>(layers: &'a [FxLayer], mut f: impl FnMut(&'a FxSprite)) {
    for layer in layers.iter().filter(|l| layer_draws(l)) {
        for item in layer.content.items() {
            if let FxItem::Sprite(sp) = item
                && item.blend() == layer.blend
                && !sp.art.is_empty()
                && sp.dw != 0
                && sp.dh != 0
            {
                f(sp);
            }
        }
    }
}

/// The frame art budget's two widths: the one-byte arts (Index8) and the
/// four-byte ones (Rgba8Premul and Rgba8Straight), one GPU atlas each.
#[inline]
fn budget_width(t: FxTexels) -> usize {
    match t {
        FxTexels::Index8 => 0,
        FxTexels::Rgba8Premul | FxTexels::Rgba8Straight => 1,
    }
}

/// The frame art budget's ruling ([`FX_FRAME_ART_TEXELS`]): which arts the
/// frame's sprites may not draw. One statement for every backend: the CPU
/// stamps, the GPU instances, its atlas admission and the bloom source all
/// read it through [`slice_layers`] or [`Self::rule`], so a sprite the budget
/// refuses draws nothing anywhere. Capacity is kept across frames.
#[derive(Debug, Default)]
pub struct FxBudget {
    seen: FxHashSet<u128>,
    refused: FxHashSet<u128>,
    refused_items: u64,
    /// The layers the ruling is for: each one's content, slot and blend, all
    /// that a ruling reads (the walk filters on them, and a content is
    /// immutable). The contents are held, so an address cannot be a freed
    /// content's, reused.
    ruled: Vec<(Arc<FxContent>, FxSlot, FxBlend)>,
}

impl FxBudget {
    /// Rule on `layers`: walk the drawn sprites ([`for_each_drawn_sprite`])
    /// and refuse each distinct art that would take its width's total of
    /// distinct texels past [`FX_FRAME_ART_TEXELS`]; a refused art's sprites
    /// draw nothing. A frame whose sprites fit counting every repeat (every
    /// real frame) is ruled by one sum, touching no set. Layers of the contents,
    /// slots and blends last ruled on keep that ruling unwalked: the slicer
    /// rules once per pass and a frame takes up to six passes, and a card at
    /// rest keeps its contents for many frames.
    pub fn rule(&mut self, layers: &[FxLayer]) {
        if self.ruled.len() == layers.len()
            && self
                .ruled
                .iter()
                .zip(layers)
                .all(|((content, slot, blend), l)| {
                    Arc::ptr_eq(content, &l.content) && *slot == l.slot && *blend == l.blend
                })
        {
            return;
        }
        self.ruled.clear();
        self.ruled.extend(
            layers
                .iter()
                .map(|l| (Arc::clone(&l.content), l.slot, l.blend)),
        );
        #[cfg(any(test, debug_assertions))]
        crate::test_counters::count_budget_ruling();
        self.refused.clear();
        self.refused_items = 0;
        let mut sum = [0u64; 2];
        for_each_drawn_sprite(layers, |sp| {
            sum[budget_width(sp.art.texels())] += area(&sp.art);
        });
        if sum.iter().all(|&s| s <= FX_FRAME_ART_TEXELS) {
            return;
        }
        self.seen.clear();
        let mut used = [0u64; 2];
        let Self {
            seen,
            refused,
            refused_items,
            ..
        } = self;
        for_each_drawn_sprite(layers, |sp| {
            let id = sp.art.id();
            if seen.insert(id) {
                let u = &mut used[budget_width(sp.art.texels())];
                if *u + area(&sp.art) <= FX_FRAME_ART_TEXELS {
                    *u += area(&sp.art);
                } else {
                    refused.insert(id);
                }
            }
            if refused.contains(&id) {
                *refused_items += 1;
            }
        });
    }

    /// Whether the last ruling refused `art`.
    #[must_use]
    pub fn refuses(&self, art: &FxArt) -> bool {
        !self.refused.is_empty() && self.refused.contains(&art.id())
    }

    /// The sprites the last ruling refused (each draws nothing).
    #[must_use]
    pub fn refused_items(&self) -> u64 {
        self.refused_items
    }
}

/// An art's texel count.
#[inline]
fn area(art: &FxArt) -> u64 {
    u64::from(art.w()) * u64::from(art.h())
}

impl FxSlicer {
    /// The frame art budget the last [`slice_layers`] ruled by.
    #[must_use]
    pub fn budget(&self) -> &FxBudget {
        &self.budget
    }
}

/// The rasterizer's resident scratch: the slicer's, and the sprite column
/// table ([`SpriteCols`]). Capacity is kept across frames.
#[derive(Debug, Default)]
pub(crate) struct FxScratch {
    slicer: FxSlicer,
    cols: SpriteCols,
}

/// A sprite's column table ([`sprite_cols`]), built when a slice of the
/// sprite is first drawn and kept while the sprites drawn after it have the
/// same art width and destination width. The table is a function of those
/// two alone, so a run of sprites of one size (a rim laid as its cover's
/// rectangles, each one item over the same art) builds it once, and a sprite
/// whose every band is kept builds none. Capacity is kept across frames.
#[derive(Debug, Default)]
pub struct SpriteCols {
    /// The `(art width, dw)` that `cols` was built for.
    key: Option<(u16, u16)>,
    cols: Vec<u32>,
}

impl SpriteCols {
    /// `sp`'s column table: the kept one when its `(art width, dw)` matches,
    /// else built afresh.
    pub(crate) fn of(&mut self, sp: &FxSprite) -> &[u32] {
        let key = (sp.art.w(), sp.dw);
        if self.key != Some(key) {
            #[cfg(any(test, debug_assertions))]
            crate::test_counters::count_sprite_cols_build();
            sprite_cols(sp, &mut self.cols);
            self.key = Some(key);
        }
        &self.cols
    }
}

/// What [`slice_layers`] hands its slices to.
pub trait FxSliceSink<'a> {
    /// The next drawable item of `layer`, addressed from the window-pixel
    /// origin `origin` (pixel law F's unclamped `O`). `false` skips its
    /// slices (a backend that cannot draw it fails closed).
    fn item(&mut self, layer: &'a FxLayer, item: &'a FxItem, origin: (i64, i64)) -> bool;
    /// One slice of the last accepted item, in band `band`.
    fn slice(&mut self, s: FxSlice, band: usize);
}

/// Slice every layer `select` takes against `f`: per item, its draw rect (its
/// box ∩ its clip ∩ the layer clip ∩ the frame, minus every occluder) cut at
/// the cell-row bands, handing `sink` each slice whose band `band_open`
/// admits. Layers go in list order, items in content order, so a sink that
/// draws what it is handed composites in the engine's order.
///
/// Fail closed: a layer whose content its blend does not accept (or a Linear
/// layer at Glow), an item of the other blend, and a sprite whose art the
/// frame's art budget refuses ([`FxBudget`], ruled here on all of `layers`)
/// hand over nothing.
pub fn slice_layers<'a>(
    f: &FxFrame,
    layers: &'a [FxLayer],
    occluders: &[PxRect],
    select: impl Fn(&FxLayer) -> bool,
    band_open: impl Fn(usize) -> bool,
    slicer: &mut FxSlicer,
    sink: &mut impl FxSliceSink<'a>,
) {
    if f.rows == 0 || f.w == 0 || f.h == 0 {
        return;
    }
    slicer.budget.rule(layers);
    for layer in layers {
        if !select(layer) || !layer_draws(layer) {
            continue;
        }
        if !reaches_an_open_band(f, layer, &band_open) {
            continue;
        }
        let Some((shift, clip)) = layer_place(f, layer) else {
            continue;
        };
        for item in layer.content.items() {
            // Per item, fail closed: an item the layer's blend does not draw
            // draws nothing, and nor does a sprite whose art the frame's art
            // budget refuses.
            if item.blend() != layer.blend {
                continue;
            }
            if let FxItem::Sprite(sp) = item
                && slicer.budget.refuses(&sp.art)
            {
                continue;
            }
            let (origin, area) = item_place(item, shift);
            let draw = area.intersect(clip);
            if draw.is_empty() {
                continue;
            }
            subtract_occluders(draw, occluders, slicer);
            if slicer.pieces.is_empty() || !sink.item(layer, item, origin) {
                continue;
            }
            for &piece in &slicer.pieces {
                for_each_slice(f, piece, &band_open, |s, b| sink.slice(s, b));
            }
        }
    }
}

/// The CPU sink: stamps each slice into a frame buffer.
struct Stamper<'p, 'a, C: FxCanvas> {
    canvas: &'p mut C,
    cols: &'p mut SpriteCols,
    cur: Option<(&'a FxItem, (i64, i64), &'a [FxInk])>,
}

impl<'a, C: FxCanvas> FxSliceSink<'a> for Stamper<'_, 'a, C> {
    fn item(&mut self, layer: &'a FxLayer, item: &'a FxItem, origin: (i64, i64)) -> bool {
        self.cur = Some((item, origin, layer.content.palette()));
        true
    }

    fn slice(&mut self, s: FxSlice, _band: usize) {
        let bounds = self.canvas.bounds();
        let s = FxSlice {
            x0: s.x0.max(bounds.x0),
            x1: s.x1.min(bounds.x1),
            y0: s.y0.max(bounds.y0),
            y1: s.y1.min(bounds.y1),
        };
        if s.x0 < s.x1
            && s.y0 < s.y1
            && let Some((item, origin, palette)) = self.cur
        {
            let cols = match item {
                FxItem::Sprite(sp) => self.cols.of(sp),
                FxItem::Rect(_) | FxItem::Ramps(_) => &[],
            };
            // Clipping changes only the destination, never pixel law F's origin.
            stamp(self.canvas, s, item, origin, palette, cols);
        }
    }
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
    if px.len() < f.w * f.h {
        return;
    }
    let mut canvas = FramePx::new(&mut px[..f.w * f.h], f.w);
    draw_layers_on(
        &mut canvas,
        f,
        layers,
        occluders,
        slot,
        blend,
        dirty,
        scratch,
    );
}

/// [`draw_layers`] onto any [`FxCanvas`]: the shared slicer still owns
/// geometry, bands and occluders; the CPU sink intersects its slices with
/// the target while retaining each item's original window-pixel origin.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_layers_on<C: FxCanvas>(
    canvas: &mut C,
    f: &FxFrame,
    layers: &[FxLayer],
    occluders: &[PxRect],
    slot: FxSlot,
    blend: FxBlend,
    dirty: Option<&[bool]>,
    scratch: &mut FxScratch,
) {
    let FxScratch { slicer, cols } = scratch;
    let mut sink = Stamper {
        canvas,
        cols,
        cur: None,
    };
    slice_layers(
        f,
        layers,
        occluders,
        |l| l.slot == slot && l.blend == blend,
        |b| dirty.is_none_or(|d| d.get(b).copied().unwrap_or(false)),
        slicer,
        &mut sink,
    );
}

/// Where `layer` lands in the frame: the window-pixel shift of its content
/// origin (offset plus the Grid origin) and its clip in window pixels within
/// the frame, or `None` when that clip is empty.
fn layer_place(f: &FxFrame, layer: &FxLayer) -> Option<((i64, i64), Area)> {
    let (gx, gy) = space_origin(f, layer);
    let clip = Area::of(layer.clip, gx, gy).intersect(frame_area(f));
    (!clip.is_empty()).then_some((
        (
            i64::from(layer.offset.0) + gx,
            i64::from(layer.offset.1) + gy,
        ),
        clip,
    ))
}

/// The window pixel of `layer`'s space origin: `(0, 0)`, or the Grid origin
/// `(pad, grid_top)`.
fn space_origin(f: &FxFrame, layer: &FxLayer) -> (i64, i64) {
    match layer.space {
        FxSpace::Window => (0, 0),
        FxSpace::Grid => (f.pad as i64, f.grid_top as i64),
    }
}

/// The frame's pixels as an [`Area`].
fn frame_area(f: &FxFrame) -> Area {
    Area {
        x0: 0,
        y0: 0,
        x1: f.w as i64,
        y1: f.h as i64,
    }
}

/// Whether any band `band_open` admits meets `layer`'s extent in the frame
/// ([`FxLayer::extent`] at its space origin): every pixel the layer draws lies
/// there (`fx_placement_is_the_spec`), so a layer for which this is `false`
/// hands over no slice, and the slicer passes it over without reading its
/// items (a card whose rows the frame keeps costs nothing).
///
/// Out of line, with the band gate behind `dyn`: one call per layer. Inlined
/// as a generic, it grew the GPU's `slice_layers` instance until the
/// optimizer put the GPU sink's `item` and `layer_place` out of line, a call
/// per item, and the Metal encode of a scrolling frame took 0.6 % more
/// instructions than with no test at all (the effects bench); out of line
/// it is within 0.3 %.
#[inline(never)]
fn reaches_an_open_band(f: &FxFrame, layer: &FxLayer, band_open: &dyn Fn(usize) -> bool) -> bool {
    let (gx, gy) = space_origin(f, layer);
    let reach = Area::of(layer.extent(), gx, gy).intersect(frame_area(f));
    !reach.is_empty()
        && (f.band_of(reach.y0 as usize)..=f.band_of(reach.y1 as usize - 1)).any(band_open)
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
fn subtract_occluders(draw: Area, occluders: &[PxRect], scratch: &mut FxSlicer) {
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
/// band is open, with its band, to `stamp`.
fn for_each_slice(
    f: &FxFrame,
    piece: Area,
    band_open: &impl Fn(usize) -> bool,
    mut stamp: impl FnMut(FxSlice, usize),
) {
    let (x0, x1) = (piece.x0 as usize, piece.x1 as usize);
    let (y0, y1) = (piece.y0 as usize, piece.y1 as usize);
    let mut b = f.band_of(y0);
    loop {
        let (by0, by1) = f.band(b);
        let (sy0, sy1) = (y0.max(by0), y1.min(by1));
        if sy0 < sy1 && band_open(b) {
            stamp(
                FxSlice {
                    x0,
                    x1,
                    y0: sy0,
                    y1: sy1,
                },
                b,
            );
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
pub(crate) fn stamp<C: FxCanvas>(
    canvas: &mut C,
    s: Slice,
    item: &FxItem,
    o: (i64, i64),
    palette: &[FxInk],
    cols: &[u32],
) {
    match item {
        FxItem::Rect(r) => stamp_rect(canvas, s, o, r, palette),
        FxItem::Ramps(r) => stamp_ramps(canvas, s, o, r, palette),
        FxItem::Sprite(sp) => stamp_sprite(canvas, s, o, sp, palette, cols),
    }
}

/// A sprite's column table: `cols[d] = ⌈d·dw / w⌉` for `d` in `0..=w`, the
/// first destination column whose displayed index `l·w / dw` is at least
/// `d`. Texel run `[d0, d1)` covers destination columns
/// `[cols[d0], cols[d1])`. Built at most once per item ([`SpriteCols`]; not
/// per run and row: two divisions per run per row were a fifth of the stamp),
/// into `out`'s kept capacity; empty for an empty art.
pub(crate) fn sprite_cols(sp: &FxSprite, out: &mut Vec<u32>) {
    out.clear();
    let (aw, dw) = (u64::from(sp.art.w()), u64::from(sp.dw));
    if aw == 0 {
        return;
    }
    // `⌈d·dw / w⌉ <= dw <= u16::MAX`: the cast is lossless.
    out.extend((0..=aw).map(|d| (d * dw).div_ceil(aw) as u32));
}

fn stamp_rect<C: FxCanvas>(canvas: &mut C, s: Slice, o: (i64, i64), r: &FxRect, palette: &[FxInk]) {
    let j = u64::from(r.ink);
    let (Some(c0), Some(c1)) = (ink_at(palette, j), ink_at(palette, j + 1)) else {
        return;
    };
    // The ramp index of the slice's first column, from the item origin.
    let i0 = (s.x0 as i64 - o.0) as u32;
    for y in s.y0..s.y1 {
        canvas.ramp(y, s.x0, s.x1, i0, u32::from(r.w), c0, c1);
    }
}

fn stamp_ramps<C: FxCanvas>(
    canvas: &mut C,
    s: Slice,
    o: (i64, i64),
    r: &FxRamps,
    palette: &[FxInk],
) {
    let i0 = (s.x0 as i64 - o.0) as u32;
    for y in s.y0..s.y1 {
        let j = u64::from(r.ink) + 2 * (y as i64 - o.1) as u64;
        let (Some(c0), Some(c1)) = (ink_at(palette, j), ink_at(palette, j + 1)) else {
            continue;
        };
        canvas.ramp(y, s.x0, s.x1, i0, u32::from(r.w), c0, c1);
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

/// The paint of one sprite span: an Exact `(c, a)` (`over_premul`), or a
/// Linear straight paint `(c, a)` (`blend`, never `a == 0`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpanPaint {
    Exact(u32, u8),
    Straight(u32, u8),
}

/// Walk one sprite slice as flat spans: every `(x0, x1, y0, y1, paint)` the
/// slice paints, from the art's run lists. Pixels outside every span are
/// written nothing (index 0, a palette miss, a clear texel, a zero straight
/// opacity). The one statement of the run walk: [`stamp_sprite`] fills the
/// spans, [`fx_bloom_quads`] turns them into light.
fn sprite_spans(
    s: FxSlice,
    o: (i64, i64),
    sp: &FxSprite,
    palette: &[FxInk],
    cols: &[u32],
    mut span: impl FnMut(usize, usize, usize, usize, SpanPaint),
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
            let paint = match kind {
                FxTexels::Index8 => {
                    let j = u64::from(sp.ink) + u64::from(run.value) - 1;
                    match ink_at(palette, j) {
                        Some(ink) => SpanPaint::Exact(ink.color, ink.alpha),
                        None => continue,
                    }
                }
                FxTexels::Rgba8Premul => {
                    SpanPaint::Exact(run.value & 0x00FF_FFFF, (run.value >> 24) as u8)
                }
                FxTexels::Rgba8Straight => match straight_paint(run.value, tint) {
                    (_, 0) => continue,
                    (c, a) => SpanPaint::Straight(c, a),
                },
            };
            span(x0, x1, y, y_end, paint);
        }
        y = y_end;
    }
}

fn stamp_sprite<C: FxCanvas>(
    canvas: &mut C,
    s: FxSlice,
    o: (i64, i64),
    sp: &FxSprite,
    palette: &[FxInk],
    cols: &[u32],
) {
    sprite_spans(s, o, sp, palette, cols, |x0, x1, y0, y1, paint| {
        for y in y0..y1 {
            match paint {
                SpanPaint::Exact(c, a) => canvas.flat(y, x0, x1, c, a),
                SpanPaint::Straight(c, a) => canvas.linear(y, x0, x1, c, a),
            }
        }
    });
}

/// The bloom source of every `bloom` layer, as [`GlowQuad`]s in window pixels
/// appended to `out` (design §4.6: bloom layers are also fed, ungated, to the
/// GPU's `bloom_glow` source; bloom is GPU-only, outside CPU parity).
///
/// Every band is open (the bloom is recomposed whole each frame), the clips
/// and occluders apply, and each quad is tagged with its band. An Exact
/// layer's quads paint, through the [`GlowQuad`] law, exactly what pixel law F
/// stamps: a rect slice that spans its item, or a flat one, is one quad over
/// the slice; a horizontally cut gradient is one column per quad, its
/// lerped ink; a Ramps row is its own rect; a sprite span is one flat quad. A
/// Linear layer's straight paint enters as source-over light premultiplied
/// by its opacity.
pub fn fx_bloom_quads(
    f: &FxFrame,
    layers: &[FxLayer],
    occluders: &[PxRect],
    slicer: &mut FxSlicer,
    cols: &mut SpriteCols,
    out: &mut Vec<GlowQuad>,
) {
    struct Bloom<'s, 'a> {
        out: &'s mut Vec<GlowQuad>,
        cols: &'s mut SpriteCols,
        cur: Option<(&'a FxItem, (i64, i64), &'a [FxInk])>,
    }
    impl Bloom<'_, '_> {
        /// A Rect-law ramp over `[x0, x1) × [y0, y1)`, its item `qw` wide
        /// from `ox`, between `c0` and `c1`.
        #[allow(clippy::too_many_arguments)]
        fn ramp(
            &mut self,
            band: usize,
            (x0, x1, y0, y1): (usize, usize, usize, usize),
            ox: i64,
            qw: u16,
            c0: FxInk,
            c1: FxInk,
        ) {
            let quad = |x: usize, w: usize, c0: FxInk, c1: FxInk| GlowQuad {
                row: band as u16,
                x: x as u16,
                y: y0 as u16,
                w: w as u16,
                h: (y1 - y0) as u16,
                color: c0.color,
                alpha: c0.alpha,
                color2: c1.color,
                alpha2: c1.alpha,
            };
            let whole = x0 as i64 == ox && x1 as i64 == ox + i64::from(qw);
            if c0 == c1 || whole {
                self.out.push(quad(x0, x1 - x0, c0, c1));
                return;
            }
            let (a0, a1) = (u32::from(c0.alpha), u32::from(c1.alpha));
            for x in x0..x1 {
                let i = (x as i64 - ox) as u32;
                let ink = FxInk {
                    color: glow_lerp_rgb(c0.color, c1.color, i, u32::from(qw)),
                    alpha: glow_lerp(a0, a1, i, u32::from(qw)) as u8,
                };
                self.out.push(quad(x, 1, ink, ink));
            }
        }
    }
    impl<'a> FxSliceSink<'a> for Bloom<'_, 'a> {
        fn item(&mut self, layer: &'a FxLayer, item: &'a FxItem, origin: (i64, i64)) -> bool {
            self.cur = Some((item, origin, layer.content.palette()));
            true
        }

        fn slice(&mut self, s: FxSlice, band: usize) {
            let Some((item, o, palette)) = self.cur else {
                return;
            };
            match item {
                FxItem::Rect(r) => {
                    let j = u64::from(r.ink);
                    if let (Some(c0), Some(c1)) = (ink_at(palette, j), ink_at(palette, j + 1)) {
                        self.ramp(band, (s.x0, s.x1, s.y0, s.y1), o.0, r.w, c0, c1);
                    }
                }
                FxItem::Ramps(r) => {
                    for y in s.y0..s.y1 {
                        let j = u64::from(r.ink) + 2 * (y as i64 - o.1) as u64;
                        if let (Some(c0), Some(c1)) = (ink_at(palette, j), ink_at(palette, j + 1)) {
                            self.ramp(band, (s.x0, s.x1, y, y + 1), o.0, r.w, c0, c1);
                        }
                    }
                }
                FxItem::Sprite(sp) => {
                    let out = &mut *self.out;
                    let cols = self.cols.of(sp);
                    sprite_spans(s, o, sp, palette, cols, |x0, x1, y0, y1, paint| {
                        let (color, alpha) = match paint {
                            SpanPaint::Exact(c, a) => (c, a),
                            // Straight `(c, a)` as premultiplied source-over light.
                            SpanPaint::Straight(c, a) => {
                                let mul8 = |v: u32| (v * u32::from(a) + 127) / 255;
                                let ch = |sh: u32| mul8((c >> sh) & 0xff) << sh;
                                (ch(16) | ch(8) | ch(0), a)
                            }
                        };
                        out.push(GlowQuad {
                            row: band as u16,
                            x: x0 as u16,
                            y: y0 as u16,
                            w: (x1 - x0) as u16,
                            h: (y1 - y0) as u16,
                            color,
                            alpha,
                            color2: color,
                            alpha2: alpha,
                        });
                    });
                }
            }
        }
    }
    let mut sink = Bloom {
        out,
        cols,
        cur: None,
    };
    slice_layers(
        f,
        layers,
        occluders,
        |l| l.bloom,
        |_| true,
        slicer,
        &mut sink,
    );
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
    s: FxSlice,
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

#[cfg(test)]
mod canvas_tests {
    use std::sync::Arc;

    use aterm_core::render::FxContent;

    use super::*;

    struct WindowPx<'a> {
        frame: FramePx<'a>,
        bounds: FxSlice,
        calls: [usize; 3],
    }

    impl WindowPx<'_> {
        fn contains(&self, y: usize, x0: usize, x1: usize) {
            assert!(self.bounds.y0 <= y && y < self.bounds.y1);
            assert!(self.bounds.x0 <= x0 && x0 < x1 && x1 <= self.bounds.x1);
        }
    }

    impl FxCanvas for WindowPx<'_> {
        fn bounds(&self) -> FxSlice {
            self.bounds
        }
        fn flat(&mut self, y: usize, x0: usize, x1: usize, c: u32, a: u8) {
            self.contains(y, x0, x1);
            self.calls[0] += 1;
            self.frame.flat(y, x0, x1, c, a);
        }
        fn ramp(&mut self, y: usize, x0: usize, x1: usize, i0: u32, qw: u32, c0: FxInk, c1: FxInk) {
            self.contains(y, x0, x1);
            self.calls[1] += 1;
            self.frame.ramp(y, x0, x1, i0, qw, c0, c1);
        }
        fn linear(&mut self, y: usize, x0: usize, x1: usize, c: u32, a: u8) {
            self.contains(y, x0, x1);
            self.calls[2] += 1;
            self.frame.linear(y, x0, x1, c, a);
        }
    }

    #[test]
    fn a_partial_canvas_gets_the_frames_pixels_and_leaves_its_surroundings() {
        let f = FxFrame {
            w: 12,
            h: 10,
            pad: 1,
            grid_top: 1,
            cell_h: 3,
            rows: 3,
        };
        let bounds = FxSlice {
            x0: 3,
            x1: 10,
            y0: 2,
            y1: 9,
        };
        let sprite = |texels: FxTexels, data: Vec<u8>| {
            FxItem::Sprite(FxSprite {
                x: -1,
                y: -1,
                dw: 12,
                dh: 10,
                flip_x: true,
                ink: 0,
                clip: PxRect::new(1, 0, 11, 10),
                art: Arc::new(FxArt::new(2, 2, texels, data).expect("four texels")),
            })
        };
        let layers = [
            FxLayer::new(
                1,
                Arc::new(FxContent::new(
                    vec![
                        FxItem::Rect(FxRect {
                            x: -2,
                            y: 0,
                            w: 15,
                            h: 10,
                            ink: 0,
                        }),
                        FxItem::Ramps(FxRamps {
                            x: 0,
                            y: 4,
                            w: 12,
                            h: 2,
                            ink: 0,
                        }),
                        sprite(
                            FxTexels::Rgba8Premul,
                            vec![3, 5, 7, 0, 32, 64, 96, 128, 0, 0, 0, 0, 80, 40, 20, 255],
                        ),
                    ],
                    vec![
                        FxInk::over(0x030507, 64),
                        FxInk::over(0x204060, 128),
                        FxInk::over(0x334455, 255),
                        FxInk::over(0x090301, 64),
                    ],
                )),
                FxSlot::Over,
                FxSpace::Window,
                FxBlend::Exact,
            ),
            FxLayer::new(
                2,
                Arc::new(FxContent::new(
                    vec![sprite(
                        FxTexels::Rgba8Straight,
                        vec![
                            255, 0, 0, 180, 0, 255, 0, 90, 0, 0, 255, 0, 100, 50, 200, 255,
                        ],
                    )],
                    vec![FxInk::over(0xffffff, 255)],
                )),
                FxSlot::Over,
                FxSpace::Window,
                FxBlend::Linear,
            ),
        ];
        let occluders = [PxRect::new(5, 3, 7, 6)];
        let gate = [false, true, false];
        let ground = 0x88112233;
        for dirty in [None, Some(gate.as_slice())] {
            let mut full = vec![ground; f.w * f.h];
            let mut partial = full.clone();
            let mut scratch = FxScratch::default();
            let mut window = WindowPx {
                frame: FramePx::new(&mut partial, f.w),
                bounds,
                calls: [0; 3],
            };
            for blend in [FxBlend::Exact, FxBlend::Linear] {
                draw_layers(
                    &mut full,
                    &f,
                    &layers,
                    &occluders,
                    FxSlot::Over,
                    blend,
                    dirty,
                    &mut scratch,
                );
                draw_layers_on(
                    &mut window,
                    &f,
                    &layers,
                    &occluders,
                    FxSlot::Over,
                    blend,
                    dirty,
                    &mut scratch,
                );
            }
            assert!(
                window.calls.into_iter().all(|n| n > 0),
                "all three paints reached"
            );
            let mut painted = false;
            for y in 0..f.h {
                for x in 0..f.w {
                    let inside = bounds.x0 <= x && x < bounds.x1 && bounds.y0 <= y && y < bounds.y1;
                    let want = if inside { full[y * f.w + x] } else { ground };
                    assert_eq!(partial[y * f.w + x], want, "pixel ({x}, {y})");
                    painted |= inside && want != ground;
                }
            }
            assert!(painted, "the restricted target actually painted");
            assert_eq!(partial[4 * f.w + 5], ground, "the occluder stays clear");
            if dirty.is_some() {
                assert_eq!(partial[2 * f.w + 3], ground, "closed band stays clear");
            }
        }
    }
}
