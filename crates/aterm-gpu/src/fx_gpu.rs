// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fx layers on the GPU (`docs/DESIGN-card-engine-2026-10-19.md` §4.6).

use super::*;
use aterm_core::render::{FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxSlot, FxTexels};
use aterm_render::{FxFrame, FxSlice, FxSliceSink, FxSlicer, GlowQuad};

/// One band slice of one fx item, as the vertex stage reads it ([`pipeline_table::FX_LAYOUT`]).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FxInstance {
    /// The slice: x, y, w, h in window pixels.
    pub(crate) rect: [u16; 4],
    /// The item's window-pixel origin `O` modulo 2¹⁶: the fragment stage needs only the
    /// item-local pixel `(p − O) mod 2¹⁶` (`fx_local`).
    pub(crate) origin: [i16; 2],
    /// A sprite's destination size `(dw, dh)`.
    pub(crate) dst: [u16; 2],
    /// A sprite's atlas region `(x, y, w, h)`.
    pub(crate) src: [u16; 4],
    /// The absolute palette entry the item's `ink` names in this frame's palette.
    pub(crate) pal: u32,
    /// Kind, flip, texel kind and Index8 reach ([`FX_KIND_SPRITE`] and its siblings).
    pub(crate) flags: u32,
}
impl_pod_zeroable!(FxInstance {
    rect: [u16; 4],
    origin: [i16; 2],
    dst: [u16; 2],
    src: [u16; 4],
    pal: u32,
    flags: u32
});
const _: () = assert!(std::mem::size_of::<FxInstance>() as u64 == pipeline_table::FX_LAYOUT.stride);

/// `flags` bits 0-1.
pub(crate) const FX_KIND_RECT: u32 = 0;
/// `flags` bits 0-1.
pub(crate) const FX_KIND_RAMPS: u32 = 1;
/// `flags` bits 0-1.
pub(crate) const FX_KIND_SPRITE: u32 = 2;
/// `flags` bit 2.
pub(crate) const FX_FLIP: u32 = 1 << 2;
/// `flags` bits 3-4.
pub(crate) const FX_TEXELS_SHIFT: u32 = 3;
/// `flags` bits 8-15.
pub(crate) const FX_REACH_SHIFT: u32 = 8;

/// The atlas's first side, and its cap (also clamped to the device's 2-D texture limit, [`GpuRenderer::fx_atlas_cap`]).
pub(crate) const FX_ATLAS_MIN: u32 = 512;
pub(crate) const FX_ATLAS_CAP: u32 = 4096;

/// The fx streams a frame draws, by `(slot, blend)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FxStream {
    /// Linear layers at Under.
    LinearUnder = 0,
    /// Exact layers at Under.
    Under,
    /// Exact layers at Glow.
    Glow,
    /// Exact layers at Over.
    Over,
    /// Linear layers at Over.
    LinearOver,
}

/// How many [`FxStream`]s exist.
pub(crate) const FX_STREAM_COUNT: usize = FxStream::LinearOver as usize + 1;

impl FxStream {
    /// The stream a layer draws into, or `None` for the one placement the engine refuses (a Linear layer at Glow: the CPU draws it nowhere).
    #[inline]
    pub(crate) const fn of(slot: FxSlot, blend: FxBlend) -> Option<Self> {
        match (slot, blend) {
            (FxSlot::Under, FxBlend::Exact) => Some(Self::Under),
            (FxSlot::Glow, FxBlend::Exact) => Some(Self::Glow),
            (FxSlot::Over, FxBlend::Exact) => Some(Self::Over),
            (FxSlot::Under, FxBlend::Linear) => Some(Self::LinearUnder),
            (FxSlot::Over, FxBlend::Linear) => Some(Self::LinearOver),
            (FxSlot::Glow, FxBlend::Linear) => None,
        }
    }
}

/// A palette entry as its frame-palette word.
#[inline]
pub(crate) const fn palette_word(ink: FxInk) -> u32 {
    let c = ink.color;
    ((c >> 16) & 0xff) | (((c >> 8) & 0xff) << 8) | ((c & 0xff) << 16) | ((ink.alpha as u32) << 24)
}

/// Palette entries per row of the GL arm's palette texture (`cell.glsl` fetches entry `j` at `(j % 1024, j / 1024)`).
#[cfg(gl_arm)]
pub(crate) const FX_PALETTE_ROW: u32 = 1024;

/// The GL arm's palette ring depth.
#[cfg(gl_arm)]
pub(crate) const FX_PALETTE_RING: usize = 3;

/// Which of the two atlas textures an art lives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FxAtlasKind {
    /// `R8Unorm`: Index8 arts.
    R8 = 0,
    /// `Rgba8Unorm`.
    Rgba = 1,
}

impl FxAtlasKind {
    /// The atlas an art of `texels` lives in.
    #[inline]
    pub(crate) const fn of(texels: FxTexels) -> Self {
        match texels {
            FxTexels::Index8 => Self::R8,
            FxTexels::Rgba8Premul | FxTexels::Rgba8Straight => Self::Rgba,
        }
    }

    /// The texture's format.
    #[inline]
    pub(crate) const fn format(self) -> crate::device_layer::TexelFormat {
        match self {
            Self::R8 => crate::device_layer::TexelFormat::R8Unorm,
            Self::Rgba => crate::device_layer::TexelFormat::Rgba8Unorm,
        }
    }

    /// Bytes per texel, as the art stores them.
    #[inline]
    pub(crate) const fn bytes_per_texel(self) -> u32 {
        match self {
            Self::R8 => 1,
            Self::Rgba => 4,
        }
    }

    /// The debug label every arm mints the texture under.
    #[inline]
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::R8 => "aterm-gpu fx index8 atlas",
            Self::Rgba => "aterm-gpu fx rgba atlas",
        }
    }
}

/// One shelf of a [`ShelfPack`]: its top, its height, and the next free x.
#[derive(Clone, Copy, Debug)]
struct Shelf {
    y: u32,
    h: u32,
    x: u32,
}

/// A best-fit shelf packer over a `side × side` square. No gutter: the
/// fragment reads texels by integer address, never filtered, so a neighbour's
/// texel is never sampled.
#[derive(Debug, Default)]
struct ShelfPack {
    side: u32,
    shelves: Vec<Shelf>,
    bottom: u32,
}

impl ShelfPack {
    #[inline]
    const fn new(side: u32) -> Self {
        Self {
            side,
            shelves: Vec::new(),
            bottom: 0,
        }
    }

    /// Place a `w × h` region: on the lowest shelf tall enough with room, else
    /// on a new shelf, else `None`.
    fn place(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        let side = self.side;
        let mut best: Option<usize> = None;
        for (i, s) in self.shelves.iter().enumerate() {
            if s.h >= h && s.x + w <= side && best.is_none_or(|b| s.h < self.shelves[b].h) {
                best = Some(i);
            }
        }
        if let Some(i) = best {
            let s = &mut self.shelves[i];
            let at = (s.x, s.y);
            s.x += w;
            return Some(at);
        }
        if w > side || self.bottom + h > side {
            return None;
        }
        let at = (0, self.bottom);
        self.shelves.push(Shelf {
            y: self.bottom,
            h,
            x: w,
        });
        self.bottom += h;
        Some(at)
    }
}

/// One of the two fx atlases, CPU side.
#[derive(Debug)]
pub(crate) struct FxAtlasSide {
    kind: FxAtlasKind,
    pack: ShelfPack,
    /// Bumped at every mint; `0` before the first (nothing resident, no
    /// texture: an arm binds a 1×1 placeholder).
    generation: u64,
    resident: FxHashMap<u128, [u16; 4]>,
    /// Every art of this generation in placement order: an arm's texture of
    /// this generation holds a prefix of it ([`FxArmTex::uploaded`]).
    log: Vec<(Arc<FxArt>, [u16; 4])>,
    /// Prefetched arts this generation's mint could not place beside the
    /// arts the frame drew; skipped as hints until the next mint, so an
    /// overfull prefetch cannot re-mint every frame.
    unfit_hint: FxHashSet<u128>,
    /// Drawn arts this generation's mint could not place: only on a device
    /// whose 2-D texture limit holds the cap below 4096 (at 4096 every art
    /// set the frame budget admits fits, `aterm_core::render::FX_FRAME_ART_TEXELS`).
    unfit_drawn: FxHashSet<u128>,
}

impl FxAtlasSide {
    fn new(kind: FxAtlasKind) -> Self {
        Self {
            kind,
            pack: ShelfPack::default(),
            generation: 0,
            resident: FxHashMap::default(),
            log: Vec::new(),
            unfit_hint: FxHashSet::default(),
            unfit_drawn: FxHashSet::default(),
        }
    }

    /// The texture side, `0` before the first mint.
    #[inline]
    pub(crate) const fn side(&self) -> u32 {
        self.pack.side
    }

    /// The current generation.
    #[inline]
    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    /// This generation's arts and regions, in placement order.
    #[inline]
    pub(crate) fn log(&self) -> &[(Arc<FxArt>, [u16; 4])] {
        &self.log
    }

    /// The resident region of art `id`.
    #[inline]
    pub(crate) fn region(&self, id: u128) -> Option<[u16; 4]> {
        self.resident.get(&id).copied()
    }

    /// Make every art of `draw` (the arts this frame's sprites draw, unique,
    /// within the frame budget) and of `hint` (the prefetched ones, unique,
    /// none in `draw`) resident: append what fits into the current
    /// generation; when anything does not, mint a new generation holding
    /// `draw` and what of `hint` fits beside it.
    fn admit(&mut self, draw: &[Arc<FxArt>], hint: &[Arc<FxArt>], cap: u32) {
        let mut overflow = false;
        for art in draw {
            if self.resident.contains_key(&art.id()) || self.unfit_drawn.contains(&art.id()) {
                continue;
            }
            match self.pack.place(u32::from(art.w()), u32::from(art.h())) {
                Some((x, y)) => self.record(art, x, y),
                None => overflow = true,
            }
        }
        for art in hint {
            if overflow {
                break;
            }
            let id = art.id();
            if self.resident.contains_key(&id) || self.unfit_hint.contains(&id) {
                continue;
            }
            match self.pack.place(u32::from(art.w()), u32::from(art.h())) {
                Some((x, y)) => self.record(art, x, y),
                None => overflow = true,
            }
        }
        if overflow {
            self.mint(draw, hint, cap);
        }
    }

    fn record(&mut self, art: &Arc<FxArt>, x: u32, y: u32) {
        let region = [x as u16, y as u16, art.w(), art.h()];
        self.resident.insert(art.id(), region);
        self.log.push((Arc::clone(art), region));
    }

    /// Mint a new generation holding `draw`, then what of `hint` fits beside
    /// it, each tallest first: at the smallest side from the current one (512
    /// at the first) doubling to `cap` that holds them all, or at `cap` with
    /// what fits.
    fn mint(&mut self, draw: &[Arc<FxArt>], hint: &[Arc<FxArt>], cap: u32) {
        let tallest_first = |arts: &[Arc<FxArt>]| {
            let mut order: Vec<Arc<FxArt>> = arts.to_vec();
            order.sort_by(|a, b| {
                b.h()
                    .cmp(&a.h())
                    .then(b.w().cmp(&a.w()))
                    .then(a.id().cmp(&b.id()))
            });
            order
        };
        let (draw, hint) = (tallest_first(draw), tallest_first(hint));
        let mut side = self.pack.side.max(FX_ATLAS_MIN).min(cap);
        let (pack, placed) = loop {
            let mut pack = ShelfPack::new(side);
            let placed: Vec<Option<(u32, u32)>> = draw
                .iter()
                .chain(&hint)
                .map(|a| pack.place(u32::from(a.w()), u32::from(a.h())))
                .collect();
            if side >= cap || placed.iter().all(Option::is_some) {
                break (pack, placed);
            }
            side = (side * 2).min(cap);
        };
        self.pack = pack;
        self.generation += 1;
        self.resident.clear();
        self.log.clear();
        self.unfit_hint.clear();
        self.unfit_drawn.clear();
        for (i, (art, at)) in draw.iter().chain(&hint).zip(placed).enumerate() {
            match at {
                Some((x, y)) => self.record(art, x, y),
                None if i < draw.len() => {
                    self.unfit_drawn.insert(art.id());
                }
                None => {
                    self.unfit_hint.insert(art.id());
                }
            }
        }
    }
}

/// The two fx atlases, CPU side.
#[derive(Debug)]
pub(crate) struct FxAtlas {
    pub(crate) sides: [FxAtlasSide; 2],
}

impl Default for FxAtlas {
    fn default() -> Self {
        Self {
            sides: [
                FxAtlasSide::new(FxAtlasKind::R8),
                FxAtlasSide::new(FxAtlasKind::Rgba),
            ],
        }
    }
}

impl FxAtlas {
    /// The atlas `kind` lives in.
    #[inline]
    pub(crate) const fn side(&self, kind: FxAtlasKind) -> &FxAtlasSide {
        &self.sides[kind as usize]
    }

    /// An art's resident region.
    #[inline]
    fn region(&self, art: &FxArt) -> Option<[u16; 4]> {
        self.side(FxAtlasKind::of(art.texels())).region(art.id())
    }
}

/// The renderer's fx state.
#[derive(Debug, Default)]
pub(crate) struct FxGpu {
    pub(crate) atlas: FxAtlas,
    /// The instances' slicer, whose frame art budget is ruled once a frame:
    /// by [`Self::admit`], then read by [`Self::build`]'s slice of the same
    /// layers. Nothing else rules it, so a ruling afresh is the admission's
    /// sign that the layers changed.
    slicer: FxSlicer,
    /// This frame's palette words.
    pub(crate) palette: Vec<u32>,
    /// `(content pointer, base)` of the contents drawn this frame: each
    /// content's palette is appended once.
    bases: Vec<(usize, u32)>,
    memo_prefetch: Vec<Arc<FxArt>>,
    memo_cap: u32,
    /// Admission scratch, by atlas: the arts the frame's sprites draw (those
    /// the frame art budget admits, ruled on [`Self::slicer`]) and the
    /// prefetched ones.
    want: [Vec<Arc<FxArt>>; 2],
    hint: [Vec<Arc<FxArt>>; 2],
    seen: FxHashSet<u128>,
    /// Sprite items refused this frame, drawing nothing.
    pub(crate) refused: u64,
    /// Atlas bytes the arm uploaded for this frame (set by the arm's sync).
    pub(crate) atlas_bytes: u64,
    /// The bloom layers' source quads, and their scratch.
    bloom_src: Vec<GlowQuad>,
    bloom_cols: aterm_render::SpriteCols,
    bloom_slicer: FxSlicer,
}

impl FxGpu {
    /// Make every art this frame's layers draw, and every prefetched art, resident in the CPU atlases.
    pub(crate) fn admit(&mut self, layers: &[FxLayer], prefetch: &[Arc<FxArt>], cap: u32) {
        let changed = self.slicer.rule(layers);
        if !changed && self.memo_cap == cap && self.prefetch_admitted(prefetch) {
            return;
        }
        for w in self.want.iter_mut().chain(&mut self.hint) {
            w.clear();
        }
        self.seen.clear();
        let Self {
            want, seen, slicer, ..
        } = self;
        let budget = slicer.budget();
        aterm_render::for_each_drawn_sprite(layers, |sp| {
            if !budget.refuses(&sp.art) && seen.insert(sp.art.id()) {
                want[FxAtlasKind::of(sp.art.texels()) as usize].push(Arc::clone(&sp.art));
            }
        });
        for art in prefetch {
            if !art.is_empty() && self.seen.insert(art.id()) {
                self.hint[FxAtlasKind::of(art.texels()) as usize].push(Arc::clone(art));
            }
        }
        for ((side, want), hint) in self.atlas.sides.iter_mut().zip(&self.want).zip(&self.hint) {
            if !(want.is_empty() && hint.is_empty()) {
                side.admit(want, hint, cap);
            }
        }
        self.memo_prefetch.clear();
        self.memo_prefetch.extend(prefetch.iter().cloned());
        self.memo_cap = cap;
    }

    /// Whether `prefetch` is the list the last admission saw (identity).
    #[inline]
    pub(crate) fn prefetch_admitted(&self, prefetch: &[Arc<FxArt>]) -> bool {
        self.memo_prefetch.len() == prefetch.len()
            && self
                .memo_prefetch
                .iter()
                .zip(prefetch)
                .all(|(m, p)| Arc::ptr_eq(m, p))
    }

    /// Build this frame's fx instances into `out` (one vector per [`FxStream`], cleared first) and its palette, slicing every layer under `band_open`.
    pub(crate) fn build(
        &mut self,
        f: &FxFrame,
        layers: &[FxLayer],
        occluders: &[aterm_core::render::PxRect],
        band_open: impl Fn(usize) -> bool,
        out: &mut [Vec<FxInstance>; FX_STREAM_COUNT],
    ) {
        for v in out.iter_mut() {
            v.clear();
        }
        self.palette.clear();
        self.bases.clear();
        self.refused = 0;
        if layers.is_empty() {
            return;
        }
        let mut sink = FxBuild {
            out,
            atlas: &self.atlas,
            palette: &mut self.palette,
            bases: &mut self.bases,
            cur: None,
            refused: 0,
        };
        aterm_render::slice_layers(
            f,
            layers,
            occluders,
            |l| FxStream::of(l.slot, l.blend).is_some(),
            band_open,
            &mut self.slicer,
            &mut sink,
        );
        self.refused = sink.refused + self.slicer.budget().refused_items();
    }

    /// Append this frame's `bloom` layers to `cursor_glow_add`, or return `None` when no layer blooms.
    pub(crate) fn bloom_source<'s>(
        &'s mut self,
        f: &FxFrame,
        input: &RenderInput,
    ) -> Option<&'s [GlowQuad]> {
        if !input.fx_layers.iter().any(|l| l.bloom) {
            return None;
        }
        self.bloom_src.clear();
        self.bloom_src.extend_from_slice(&input.cursor_glow_add);
        aterm_render::fx_bloom_quads(
            f,
            &input.fx_layers,
            &input.fx_occluders,
            &mut self.bloom_slicer,
            &mut self.bloom_cols,
            &mut self.bloom_src,
        );
        Some(&self.bloom_src)
    }
}

/// Whether a frame feeds the bloom.
#[inline]
pub(crate) fn bloom_source_live(input: &RenderInput) -> bool {
    !input.cursor_glow_add.is_empty() || input.fx_layers.iter().any(|l| l.bloom)
}

/// Whether the present's crown (the HDR or SDR aurora boost over the bloom source) has light to read.
#[inline]
pub(crate) fn crown_glow_present(input: &RenderInput) -> bool {
    input
        .cursor_glow_add
        .iter()
        .any(|q| (q.row as usize) < input.rows)
        || input.fx_layers.iter().any(|l| l.bloom)
}

/// The item a [`FxBuild`] is slicing.
#[derive(Clone, Copy)]
struct FxCur {
    stream: FxStream,
    origin: [i16; 2],
    dst: [u16; 2],
    src: [u16; 4],
    /// The item's `ink`: its offset into its content's palette.
    ink: u32,
    flags: u32,
    /// Whether the fragment reads the palette (every kind but Premul).
    reads_palette: bool,
    /// The content's key in `bases`.
    content: usize,
    /// A Ramps item's last drawable row bound (exclusive, window px): rows
    /// past the palette write nothing, so slices are cut there.
    y_limit: i64,
}

/// The GPU sink of the engine's slicer: one [`FxInstance`] per slice.
struct FxBuild<'s, 'a> {
    out: &'s mut [Vec<FxInstance>; FX_STREAM_COUNT],
    atlas: &'s FxAtlas,
    palette: &'s mut Vec<u32>,
    bases: &'s mut Vec<(usize, u32)>,
    cur: Option<(FxCur, &'a FxContent)>,
    refused: u64,
}

impl<'a> FxSliceSink<'a> for FxBuild<'_, 'a> {
    fn item(&mut self, layer: &'a FxLayer, item: &'a FxItem, origin: (i64, i64)) -> bool {
        self.cur = None;
        let Some(stream) = FxStream::of(layer.slot, layer.blend) else {
            return false;
        };
        let len = layer.content.palette().len() as u64;
        let reach_of = |ink: u32| len.saturating_sub(u64::from(ink));
        let (flags, dst, src, ink, reads_palette, y_limit) = match item {
            FxItem::Rect(r) => {
                if reach_of(r.ink) < 2 {
                    return false;
                }
                (FX_KIND_RECT, [0, 0], [r.w, 0, 0, 0], r.ink, true, i64::MAX)
            }
            FxItem::Ramps(r) => {
                let rows = reach_of(r.ink) / 2;
                if rows == 0 {
                    return false;
                }
                (
                    FX_KIND_RAMPS,
                    [0, 0],
                    [r.w, 0, 0, 0],
                    r.ink,
                    true,
                    origin
                        .1
                        .saturating_add(i64::try_from(rows).unwrap_or(i64::MAX)),
                )
            }
            FxItem::Sprite(sp) => {
                let art = &*sp.art;
                if art.is_empty() || sp.dw == 0 || sp.dh == 0 {
                    return false;
                }
                let Some(region) = self.atlas.region(art) else {
                    self.refused += 1;
                    return false;
                };
                let flip = if sp.flip_x { FX_FLIP } else { 0 };
                let (texels, reads_palette) = match art.texels() {
                    FxTexels::Index8 => {
                        let reach = reach_of(sp.ink).min(255);
                        if reach == 0 {
                            return false;
                        }
                        ((reach as u32) << FX_REACH_SHIFT, true)
                    }
                    FxTexels::Rgba8Premul => (1 << FX_TEXELS_SHIFT, false),
                    FxTexels::Rgba8Straight => {
                        if reach_of(sp.ink) == 0 {
                            return false;
                        }
                        (2 << FX_TEXELS_SHIFT, true)
                    }
                };
                (
                    FX_KIND_SPRITE | flip | texels,
                    [sp.dw, sp.dh],
                    region,
                    if reads_palette { sp.ink } else { 0 },
                    reads_palette,
                    i64::MAX,
                )
            }
        };
        let wrap = |v: i64| v.rem_euclid(1 << 16) as u16 as i16;
        self.cur = Some((
            FxCur {
                stream,
                origin: [wrap(origin.0), wrap(origin.1)],
                dst,
                src,
                ink,
                flags,
                reads_palette,
                content: Arc::as_ptr(&layer.content) as usize,
                y_limit,
            },
            &layer.content,
        ));
        true
    }

    fn slice(&mut self, s: FxSlice, _band: usize) {
        let Some((cur, content)) = self.cur else {
            return;
        };
        let y1 = (s.y1 as i64).min(cur.y_limit);
        if (s.y0 as i64) >= y1 {
            return;
        }
        let pal = if cur.reads_palette {
            let base = match self.bases.iter().find(|(k, _)| *k == cur.content) {
                Some(&(_, base)) => base,
                None => {
                    let base = self.palette.len() as u32;
                    self.palette
                        .extend(content.palette().iter().map(|&ink| palette_word(ink)));
                    self.bases.push((cur.content, base));
                    base
                }
            };
            base + cur.ink
        } else {
            0
        };
        self.out[cur.stream as usize].push(FxInstance {
            rect: [
                s.x0 as u16,
                s.y0 as u16,
                (s.x1 - s.x0) as u16,
                (y1 as usize - s.y0) as u16,
            ],
            origin: cur.origin,
            dst: cur.dst,
            src: cur.src,
            pal,
            flags: cur.flags,
        });
    }
}

/// One arm's texture of one fx atlas.
pub(crate) struct FxArmTex {
    pub(crate) tex: crate::device_layer::LayerTexture,
    generation: u64,
    uploaded: usize,
}

/// Bring an arm's texture of `side` up to date.
pub(crate) fn sync_fx_tex(
    handle: crate::device_layer::DeviceHandle<'_>,
    slot: &mut Option<FxArmTex>,
    side: &FxAtlasSide,
    fresh: impl Fn(&FxArmTex) -> bool,
    before_append: impl FnOnce(),
) -> Result<FxSynced, String> {
    let generation = side.generation();
    let stale = slot
        .as_ref()
        .is_none_or(|t| t.generation != generation || !fresh(t));
    if stale {
        let s = side.side().max(1);
        let tex = handle.try_create_texture_2d(
            side.kind.label(),
            side.kind.format(),
            s,
            s,
            crate::device_layer::TexUsage::UPLOADED_SAMPLED,
            None,
        )?;
        *slot = Some(FxArmTex {
            tex,
            generation,
            uploaded: 0,
        });
    }
    let t = slot.as_mut().expect("minted above");
    let pending = &side.log()[t.uploaded.min(side.log().len())..];
    if pending.is_empty() {
        return Ok(FxSynced {
            bytes: 0,
            minted: stale,
            appended: false,
        });
    }
    if !stale {
        before_append();
    }
    let bpp = side.kind.bytes_per_texel();
    let mut bytes = 0u64;
    for (art, r) in pending {
        let (x, y, w, h) = (
            u32::from(r[0]),
            u32::from(r[1]),
            u32::from(r[2]),
            u32::from(r[3]),
        );
        handle.upload_texture_region(&t.tex, x, y, w, h, art.data(), w * bpp);
        bytes += art.data().len() as u64;
    }
    t.uploaded = side.log().len();
    Ok(FxSynced {
        bytes,
        minted: stale,
        appended: !stale,
    })
}

/// What [`sync_fx_tex`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FxSynced {
    /// Texel bytes uploaded.
    pub(crate) bytes: u64,
    /// Whether the texture was (re-)minted (a bind over it is stale).
    pub(crate) minted: bool,
    /// Whether arts were appended to a texture earlier frames may sample.
    pub(crate) appended: bool,
}

/// Whether an arm must sync its texture of `side` this frame.
#[inline]
pub(crate) fn fx_tex_due(slot: &Option<FxArmTex>, side: &FxAtlasSide, drawing: bool) -> bool {
    drawing
        || (side.generation() > 0
            && slot
                .as_ref()
                .is_none_or(|t| t.generation != side.generation() || t.uploaded < side.log().len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_core::render::{FxRect, FxSpace, FxSprite, PxRect};

    fn art(w: u16, h: u16, seed: u8) -> Arc<FxArt> {
        let data: Vec<u8> = (0..usize::from(w) * usize::from(h))
            .map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed) | 1)
            .collect();
        Arc::new(FxArt::new(w, h, FxTexels::Index8, data).expect("sized"))
    }

    fn sprite_layer(arts: &[Arc<FxArt>]) -> FxLayer {
        let items = arts
            .iter()
            .enumerate()
            .map(|(i, a)| {
                FxItem::Sprite(FxSprite {
                    x: i as i32 * 3,
                    y: 0,
                    dw: a.w(),
                    dh: a.h(),
                    flip_x: false,
                    art: Arc::clone(a),
                    ink: 0,
                    clip: PxRect::EVERYWHERE,
                })
            })
            .collect();
        let palette = (0..255).map(|i| FxInk::over(i, 255)).collect();
        FxLayer::new(
            1,
            Arc::new(FxContent::new(items, palette)),
            FxSlot::Over,
            FxSpace::Window,
            FxBlend::Exact,
        )
    }

    /// Regions are disjoint, inside the side, and stay put while the
    /// generation lasts; a frame whose arts overflow mints a generation
    /// holding exactly those arts.
    #[test]
    fn the_atlas_appends_in_place_and_compacts_on_overflow() {
        let mut fx = FxGpu::default();
        let small: Vec<_> = (0..40).map(|i| art(60, 50, i)).collect();
        fx.admit(&[sprite_layer(&small)], &[], FX_ATLAS_CAP);
        let side = fx.atlas.side(FxAtlasKind::R8);
        assert_eq!(
            (side.side(), side.generation(), side.generation()),
            (512, 1, 1)
        );
        let first: Vec<_> = small.iter().map(|a| side.region(a.id()).unwrap()).collect();
        for (i, a) in first.iter().enumerate() {
            assert!(u32::from(a[0] + a[2]) <= 512 && u32::from(a[1] + a[3]) <= 512);
            for b in &first[..i] {
                let apart = a[0] + a[2] <= b[0]
                    || b[0] + b[2] <= a[0]
                    || a[1] + a[3] <= b[1]
                    || b[1] + b[3] <= a[1];
                assert!(apart, "{a:?} overlaps {b:?}");
            }
        }
        let more: Vec<_> = small.iter().cloned().chain([art(30, 30, 99)]).collect();
        fx.admit(&[sprite_layer(&more)], &[], FX_ATLAS_CAP);
        let side = fx.atlas.side(FxAtlasKind::R8);
        assert_eq!(side.generation(), 1, "a fitting art appends");
        for (a, r) in small.iter().zip(&first) {
            assert_eq!(
                side.region(a.id()),
                Some(*r),
                "a resident region never moves"
            );
        }
        let big: Vec<_> = (0..12).map(|i| art(200, 150, 100 + i)).collect();
        fx.admit(&[sprite_layer(&big)], &[], FX_ATLAS_CAP);
        let side = fx.atlas.side(FxAtlasKind::R8);
        assert_eq!(side.generation(), 2);
        assert_eq!(side.side(), 1024);
        assert_eq!(
            side.log().len(),
            big.len(),
            "compaction keeps only the frame's arts"
        );
        assert!(small.iter().all(|a| side.region(a.id()).is_none()));
        let layers = [sprite_layer(&big)];
        fx.admit(&layers, &[], FX_ATLAS_CAP);
        let mints = fx.atlas.side(FxAtlasKind::R8).generation();
        fx.admit(&layers, &[], FX_ATLAS_CAP);
        assert_eq!(fx.atlas.side(FxAtlasKind::R8).generation(), mints);
    }

    /// On a device whose texture limit holds the cap below an art (here 512
    /// against a 600-wide art), the art is unfit: refused and counted, its
    /// items draw nothing, and the frame does not re-mint for it again.
    #[test]
    fn an_art_past_the_cap_fails_closed_without_thrashing() {
        let mut fx = FxGpu::default();
        let arts = vec![art(40, 40, 1), art(600, 10, 2)];
        let layers = [sprite_layer(&arts)];
        fx.admit(&layers, &[], 512);
        let side = fx.atlas.side(FxAtlasKind::R8);
        assert!(side.region(arts[0].id()).is_some());
        assert!(side.region(arts[1].id()).is_none());
        let mints = side.generation();
        fx.admit(&[sprite_layer(&arts)], &[], 512);
        assert_eq!(fx.atlas.side(FxAtlasKind::R8).generation(), mints);
        let f = FxFrame {
            w: 800,
            h: 64,
            pad: 0,
            grid_top: 0,
            cell_h: 16,
            rows: 4,
        };
        let mut out: [Vec<FxInstance>; FX_STREAM_COUNT] = Default::default();
        fx.build(&f, &layers, &[], |_| true, &mut out);
        assert_eq!(fx.refused, 1, "the unfit art's item is refused");
        assert!(out[FxStream::Over as usize].iter().all(|i| i.src[2] == 40));
    }

    /// ONE RULING A FRAME: the atlas admission and the instances' slice read
    /// one frame art budget, ruled once when the frame's layers change and
    /// not at all while they stand.
    #[test]
    fn a_frame_rules_its_art_budget_once() {
        let mut fx = FxGpu::default();
        let f = FxFrame {
            w: 800,
            h: 64,
            pad: 0,
            grid_top: 0,
            cell_h: 16,
            rows: 4,
        };
        let mut out: [Vec<FxInstance>; FX_STREAM_COUNT] = Default::default();
        let mut frame = |layers: &[FxLayer]| {
            let _ = aterm_render::test_counters::take_budget_rulings();
            fx.admit(layers, &[], FX_ATLAS_CAP);
            fx.build(&f, layers, &[], |_| true, &mut out);
            aterm_render::test_counters::take_budget_rulings()
        };
        let arts = vec![art(40, 40, 1), art(20, 10, 2)];
        let layers = [sprite_layer(&arts)];
        assert_eq!(frame(&layers), 1, "a new frame is ruled once");
        assert_eq!(frame(&layers), 0, "a standing frame is not ruled");
        assert_eq!(
            frame(&[sprite_layer(&arts)]),
            1,
            "a new content is ruled once"
        );
        assert_eq!(
            out[FxStream::Over as usize].len(),
            3 + 1,
            "CONTROL: both sprites drawn, the 40-tall one in its three bands"
        );
    }

    /// THE FRAME BUDGET FITS THE ATLAS: at the cap, every frame whose drawn
    /// arts keep the budget's law (each side at most half the cap, at most a
    /// quarter of the cap's area in all) is wholly resident after admission,
    /// in disjoint regions, whatever the generation held before and whatever
    /// the prefetch asks beside it.
    #[test]
    fn every_frame_within_the_art_budget_is_resident() {
        let mut rng = 0x2545_f491_4f6c_dd1du64;
        let mut next = move |n: u32| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng % u64::from(n.max(1))) as u32
        };
        let mut serial = 0u32;
        let mut fresh = |w: u32, h: u32| {
            serial += 1;
            let mut data = vec![1u8; (w * h) as usize];
            for (b, s) in data.iter_mut().zip(serial.to_le_bytes()) {
                *b = s;
            }
            Arc::new(FxArt::new(w as u16, h as u16, FxTexels::Index8, data).expect("sized"))
        };
        let disjoint_and_inside = |side: &FxAtlasSide, arts: &[Arc<FxArt>], frame: usize| {
            let regions: Vec<[u16; 4]> = arts
                .iter()
                .map(|a| {
                    side.region(a.id())
                        .unwrap_or_else(|| panic!("frame {frame}: a drawn art is not resident"))
                })
                .collect();
            for (i, a) in regions.iter().enumerate() {
                let (ax1, ay1) = (
                    u32::from(a[0]) + u32::from(a[2]),
                    u32::from(a[1]) + u32::from(a[3]),
                );
                assert!(
                    ax1 <= side.side() && ay1 <= side.side(),
                    "frame {frame}: {a:?}"
                );
                for b in &regions[..i] {
                    let apart = ax1 <= u32::from(b[0])
                        || u32::from(b[0]) + u32::from(b[2]) <= u32::from(a[0])
                        || ay1 <= u32::from(b[1])
                        || u32::from(b[1]) + u32::from(b[3]) <= u32::from(a[1]);
                    assert!(apart, "frame {frame}: {a:?} overlaps {b:?}");
                }
            }
        };
        let cap = 256u32;
        let (half, budget) = (cap / 2, u64::from(cap) * u64::from(cap) / 4);
        let mut side = FxAtlasSide::new(FxAtlasKind::R8);
        let mut pool: Vec<Arc<FxArt>> = Vec::new();
        let (mut mints, mut appends) = (0, 0);
        for frame in 0..400 {
            let shape = |next: &mut dyn FnMut(u32) -> u32| match next(4) {
                0 => (1 + next(4), half / 2 + next(half / 2 + 1)),
                1 => (half / 2 + next(half / 2 + 1), 1 + next(4)),
                2 => (1 + next(half), 1 + next(half)),
                _ => (1 + next(16), 1 + next(16)),
            };
            let mut pick = |total: u64, next: &mut dyn FnMut(u32) -> u32, pool: &mut Vec<_>| {
                let (mut arts, mut used, mut seen) = (Vec::new(), 0u64, FxHashSet::default());
                for _ in 0..1 + next(24) {
                    let art: Arc<FxArt> = if !pool.is_empty() && next(2) == 0 {
                        Arc::clone(&pool[next(pool.len() as u32) as usize])
                    } else {
                        let (w, h) = shape(next);
                        let art = fresh(w, h);
                        pool.push(Arc::clone(&art));
                        art
                    };
                    let a = u64::from(art.w()) * u64::from(art.h());
                    if used + a <= total && seen.insert(art.id()) {
                        used += a;
                        arts.push(art);
                    }
                }
                (arts, seen)
            };
            let (draw, drawn) = pick(budget >> next(3), &mut next, &mut pool);
            let hinted = if next(4) == 0 { 4 * budget } else { 0 };
            let (hint, _) = pick(hinted, &mut next, &mut pool);
            let hint: Vec<_> = hint
                .into_iter()
                .filter(|a| !drawn.contains(&a.id()))
                .collect();
            let (generation, logged) = (side.generation(), side.log().len());
            side.admit(&draw, &hint, cap);
            if side.generation() != generation {
                mints += 1;
            } else if side.log().len() > logged {
                appends += 1;
            }
            disjoint_and_inside(&side, &draw, frame);
        }
        assert!(
            mints > 40 && appends > 40,
            "the history compacts ({mints} mints) and appends ({appends})"
        );
        let max = u32::from(aterm_core::render::FX_ART_MAX_SIDE);
        let extremes = [
            vec![fresh(max, max)],
            vec![fresh(max, max / 2), fresh(max, max / 2)],
            (0..max).map(|_| fresh(1, max)).collect::<Vec<_>>(),
        ];
        let hint: Vec<_> = (0..3).map(|_| fresh(max, max)).collect();
        for (k, draw) in extremes.iter().enumerate() {
            let texels: u64 = draw
                .iter()
                .map(|a| u64::from(a.w()) * u64::from(a.h()))
                .sum();
            assert_eq!(texels, aterm_core::render::FX_FRAME_ART_TEXELS);
            let mut side = FxAtlasSide::new(FxAtlasKind::R8);
            side.admit(&hint[..1], &[], FX_ATLAS_CAP);
            side.admit(draw, &hint, FX_ATLAS_CAP);
            disjoint_and_inside(&side, draw, 1000 + k);
        }
    }

    /// Instances carry the origin mod 2¹⁶ and the absolute palette base; a
    /// content's palette is appended once however many items draw from it.
    #[test]
    fn instances_name_their_item_and_palettes_append_once() {
        let mut fx = FxGpu::default();
        let palette = vec![
            FxInk::add(0x10_20_30),
            FxInk::add(0x11_22_33),
            FxInk::over(0x40_50_60, 7),
            FxInk::over(1, 9),
        ];
        let content = Arc::new(FxContent::new(
            vec![
                FxItem::Rect(FxRect {
                    x: -65_000,
                    y: 6,
                    w: 65_535,
                    h: 5,
                    ink: 2,
                }),
                FxItem::Rect(FxRect {
                    x: 2,
                    y: 6,
                    w: 4,
                    h: 5,
                    ink: 0,
                }),
            ],
            palette,
        ));
        let layer = |id| {
            FxLayer::new(
                id,
                Arc::clone(&content),
                FxSlot::Under,
                FxSpace::Window,
                FxBlend::Exact,
            )
        };
        let layers = [layer(1), layer(2)];
        let f = FxFrame {
            w: 64,
            h: 32,
            pad: 0,
            grid_top: 0,
            cell_h: 8,
            rows: 4,
        };
        let mut out: [Vec<FxInstance>; FX_STREAM_COUNT] = Default::default();
        fx.admit(&layers, &[], FX_ATLAS_CAP);
        fx.build(&f, &layers, &[], |_| true, &mut out);
        assert_eq!(fx.palette.len(), 4, "one content, one palette");
        assert_eq!(fx.palette[2], 0x07_60_50_40, "RGBA bytes, little-endian");
        let under = &out[FxStream::Under as usize];
        assert_eq!(
            under.len(),
            2 * 2 * 2,
            "two layers of two rects, each cut at y = 8"
        );
        let wide: Vec<_> = under.iter().filter(|i| i.src[0] == 65_535).collect();
        assert_eq!(wide.len(), 4);
        assert!(wide.iter().all(|i| i.origin == [536, 6] && i.pal == 2));
        assert!(wide.iter().all(|i| i.rect[0] == 0 && i.rect[2] == 64));
        let narrow: Vec<_> = under.iter().filter(|i| i.src[0] == 4).collect();
        assert!(narrow.iter().all(|i| i.origin == [2, 6] && i.pal == 0));
        assert!(under.iter().all(|i| i.flags == FX_KIND_RECT));
        fx.build(&f, &layers, &[], |b| b == 1, &mut out);
        let under = &out[FxStream::Under as usize];
        assert_eq!(under.len(), 4);
        assert!(under.iter().all(|i| i.rect[1] == 8 && i.rect[3] == 3));

        let art = Arc::new(
            FxArt::new(1, 1, FxTexels::Rgba8Straight, vec![10, 20, 30, 255]).expect("sized"),
        );
        let linear = Arc::new(FxContent::new(
            vec![FxItem::Sprite(FxSprite {
                x: 0,
                y: 0,
                dw: 1,
                dh: 1,
                flip_x: false,
                art: Arc::clone(&art),
                ink: 0,
                clip: PxRect::EVERYWHERE,
            })],
            vec![FxInk::over(0x00ff_ffff, 255)],
        ));
        for (why, slot, blend) in [
            ("slot", FxSlot::Glow, FxBlend::Linear),
            ("blend", FxSlot::Under, FxBlend::Exact),
        ] {
            let mut layer = FxLayer::new(
                3,
                Arc::clone(&linear),
                FxSlot::Under,
                FxSpace::Window,
                FxBlend::Linear,
            );
            layer.slot = slot;
            layer.blend = blend;
            let mut recovery = FxGpu::default();
            recovery.admit(std::slice::from_ref(&layer), &[], FX_ATLAS_CAP);
            recovery.build(&f, std::slice::from_ref(&layer), &[], |_| true, &mut out);
            assert!(
                out.iter().all(Vec::is_empty),
                "{why}: invalid draws nothing"
            );
            assert!(recovery.atlas.region(&art).is_none(), "{why}: not admitted");

            layer.slot = FxSlot::Under;
            layer.blend = FxBlend::Linear;
            assert!(Arc::ptr_eq(&linear, &layer.content), "the same content Arc");
            recovery.admit(std::slice::from_ref(&layer), &[], FX_ATLAS_CAP);
            recovery.build(&f, std::slice::from_ref(&layer), &[], |_| true, &mut out);
            assert!(
                recovery.atlas.region(&art).is_some(),
                "{why}: valid art admitted"
            );
            assert_eq!(
                out[FxStream::LinearUnder as usize].len(),
                1,
                "{why}: restored"
            );
            assert!(
                out.iter().enumerate().all(|(i, instances)| {
                    i == FxStream::LinearUnder as usize || instances.is_empty()
                }),
                "{why}: only the valid stream draws"
            );
            assert_eq!(recovery.refused, 0, "{why}: no missing art");

            let generation = recovery.atlas.side(FxAtlasKind::Rgba).generation();
            recovery.admit(std::slice::from_ref(&layer), &[], FX_ATLAS_CAP);
            recovery.build(&f, std::slice::from_ref(&layer), &[], |_| true, &mut out);
            assert_eq!(
                recovery.atlas.side(FxAtlasKind::Rgba).generation(),
                generation,
                "{why}: unchanged valid admission keeps residency"
            );
            assert_eq!(
                out[FxStream::LinearUnder as usize].len(),
                1,
                "{why}: unchanged valid frame still draws"
            );
        }
    }
}
