// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fx layers on the GPU (`docs/DESIGN-card-engine-2026-10-19.md` §4.6).

use super::*;
use aterm_core::render::{FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxSlot, FxTexels};
use aterm_render::{FxFrame, FxSlice, FxSliceSink, FxSlicer, GlowQuad, GlowRamps};

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

/// The atlas's first side, and its cap (also clamped to the device's 2-D texture limit,
/// [`GpuRenderer::fx_atlas_cap`]).
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
    /// Linear knock layers at Under (the companions' bodies under the text): `G_BASE_FG`'s
    /// first draw, over the under-glyph light, under the ink, before the glyph knock-out.
    KnockUnder,
}

/// How many [`FxStream`]s exist.
pub(crate) const FX_STREAM_COUNT: usize = FxStream::KnockUnder as usize + 1;

impl FxStream {
    /// The stream a layer draws into, or `None` for the one placement the engine refuses (a
    /// Linear layer at Glow: the CPU draws it nowhere).
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

    /// The stream `layer` draws into: [`Self::of`] its slot and blend, a knock layer's
    /// Linear Under in [`Self::KnockUnder`].
    #[inline]
    pub(crate) const fn of_layer(layer: &FxLayer) -> Option<Self> {
        match Self::of(layer.slot, layer.blend) {
            Some(Self::LinearUnder) if layer.knock => Some(Self::KnockUnder),
            s => s,
        }
    }
}

/// A palette entry as its frame-palette word.
#[inline]
pub(crate) const fn palette_word(ink: FxInk) -> u32 {
    let c = ink.color;
    ((c >> 16) & 0xff) | (((c >> 8) & 0xff) << 8) | ((c & 0xff) << 16) | ((ink.alpha as u32) << 24)
}

/// A glow end ([`aterm_render::GlowEnd`]) as its frame-palette word.
#[inline]
const fn end_word(end: u32) -> u32 {
    palette_word(FxInk::over(
        aterm_render::end_color(end),
        aterm_render::end_alpha(end),
    ))
}

/// Lay a window-space glow stream as fx instances, in its draw order: each
/// quad a Rect of its two ends, each ramp ([`GlowRamps`]) one Ramps item of
/// its rows' ends, so `fs_fx` draws every row by law F exactly as the quad
/// it stands for (a dark row's zero ends draw nothing). A ramp's run keeps
/// its ends once ([`aterm_render::GlowRamp::end_row`]), its first row and
/// length in `src.y`/`src.z` for `fx_ramp_row` to fold its rows onto them.
/// Rows tagged past `rows` or not `row_active` are skipped, as the glow
/// stream skips them.
pub(crate) fn glow_stream_into(
    (quads, ramps): (&[GlowQuad], &GlowRamps),
    rows: usize,
    row_active: impl Fn(usize) -> bool,
    palette: &mut Vec<u32>,
    out: &mut Vec<FxInstance>,
) {
    let live = |row: u16| usize::from(row) < rows && row_active(usize::from(row));
    let rect = |x: u16, y: u16, w: u16, h: u16, pal: u32, kind: u32| FxInstance {
        rect: [x, y, w, h],
        origin: [x as i16, y as i16],
        dst: [0, 0],
        src: [w, 0, 0, 0],
        pal,
        flags: kind,
    };
    use aterm_render::glow_ramps::Lights;
    aterm_render::glow_ramps::each_laid(quads, ramps, |laid| match laid {
        Lights::Quads(run) => {
            for q in run.iter().filter(|q| live(q.row)) {
                let pal = palette.len() as u32;
                palette.push(palette_word(FxInk::over(q.color, q.alpha)));
                palette.push(palette_word(FxInk::over(q.color2, q.alpha2)));
                out.push(rect(q.x, q.y, q.w, q.h, pal, FX_KIND_RECT));
            }
        }
        Lights::Ramp(r) => {
            if live(r.row) {
                let pal = palette.len() as u32;
                let ink = r.ink as usize;
                palette.extend(
                    ramps.ends[ink..ink + r.end_count()]
                        .iter()
                        .map(|&e| end_word(e)),
                );
                out.push(FxInstance {
                    src: [r.w, r.join, r.joined, 0],
                    ..rect(r.x, r.y, r.w, r.h, pal, FX_KIND_RAMPS)
                });
            }
        }
    });
}

/// Whether a glow stream's quads may draw as glow instances ahead of its
/// ramps: every light from its first ramp on adds (both ends at alpha 0), and
/// saturating adds of light commute, so drawing the quads first and the ramps
/// after them in their order stores the bytes the stream's own order does
/// (the quads before the first ramp come first in both).
pub(crate) fn ramps_commute(quads: &[GlowQuad], ramps: &GlowRamps) -> bool {
    let from = ramps
        .items
        .first()
        .map_or(quads.len(), |r| (r.at as usize).min(quads.len()));
    quads[from..].iter().all(|q| q.alpha == 0 && q.alpha2 == 0)
        && ramps.ends.iter().all(|&e| aterm_render::end_alpha(e) == 0)
}

/// What a frame's ramps cost the GPU side drawn in an fx stream rather than
/// as the glow instances their lit rows stand for, in the picoseconds of
/// frame time each adds (a saving is negative) over build, stage, encode and
/// the GPU, as one arm measured them: opening the fx stream on a frame that
/// draws none, each lit row in an fx stream so opened, and each lit row in
/// an fx stream the frame opens anyway for its own art. A ramp itself costs
/// the routes alike (measured −2.8 ± 6.9 ns), so the rule counts lit rows
/// only (`docs/measured/card-engine-b5-2026-10-24.md`, rounds 2 and 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FxRampCost {
    /// Opening the fx stream: its draws, its palette's upload and binding.
    pub(crate) open: i64,
    /// One lit row in an fx stream the ramps open, against its glow instance.
    pub(crate) light: i64,
    /// One lit row in an fx stream that also draws the frame's art, against
    /// its glow instance.
    pub(crate) art: i64,
}

impl FxRampCost {
    /// Whether ramps carrying `lit` lit rows cost the frame less in the fx
    /// stream than as their glow instances, on a frame whose fx stream draws
    /// art anyway (`art`) or none.
    #[inline]
    pub(crate) const fn pays(&self, art: bool, lit: u64) -> bool {
        // A frame's lit rows are budgeted in the thousands, far from overflow.
        let lit = if lit > i32::MAX as u64 {
            i32::MAX as i64
        } else {
            lit as i64
        };
        let cost = if art {
            lit * self.art
        } else {
            self.open + lit * self.light
        };
        cost < 0
    }
}

/// The arm a frame draws on, as the ramps' route costs were measured
/// ([`ramp_cost`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RampArm {
    /// The native Metal arm.
    #[cfg_attr(
        not(any(test, target_os = "macos")),
        expect(dead_code, reason = "only a macOS build arms Metal")
    )]
    Metal,
    /// The GL arm (Linux and WebGL2).
    #[cfg_attr(
        not(any(test, gl_arm)),
        expect(dead_code, reason = "only a build with the GL arm draws on it")
    )]
    Gl,
    /// The D3D11 arm (Windows).
    #[cfg_attr(
        not(any(test, d3d11_arm)),
        expect(dead_code, reason = "only a Windows build arms D3D11")
    )]
    D3d11,
    /// No GPU arm armed.
    Unarmed,
}

/// The Metal arm's measured costs ([`FxRampCost`]), on the Mac16,8 (M4 Pro)
/// the lane measured (`docs/measured/card-engine-b5-2026-10-24/`): opening
/// the fx stream 8.75 ± 1.5 µs, each lit row 5.7 ± 0.2 ns less in it than as
/// a glow instance (6 ns, as round 2's measured tree held it), so ramps open
/// it from 1,459 lit rows a frame; in an fx stream the frame draws art in
/// anyway, each lit row costs the GPU's median frame 0.78 ± 0.12 ns more than
/// its glow instance (pet 1.07, icons 0.30), so no count of them pays.
pub(crate) const METAL_RAMP_COST: FxRampCost = FxRampCost {
    open: 8_749_000,
    light: -6_000,
    art: 780,
};

/// The measured costs of the arm a frame draws on ([`FxRampCost`]), or
/// `None` where no cost was measured: there the frame draws every ramp as the
/// glow instances its lit rows stand for, the GPU work a stream with no ramp
/// draws, so no unmeasured arm can lose to it.
#[must_use]
#[inline]
pub(crate) const fn ramp_cost(arm: RampArm) -> Option<FxRampCost> {
    match arm {
        RampArm::Metal => Some(METAL_RAMP_COST),
        RampArm::Gl | RampArm::D3d11 | RampArm::Unarmed => None,
    }
}

/// Whether a frame's glow ramps draw in its fx stream, under the measured
/// costs of the arm it draws on (`cost`, [`ramp_cost`]): where their lit rows,
/// both streams' together, cost less there than as glow instances, in an fx
/// stream the frame draws its art in anyway (`fx_open`) or one they open.
/// Otherwise, and on an arm with no measured cost, each lit row draws as the
/// glow instance it stands for, and the frame opens no fx stream and uploads
/// no palette for them.
#[inline]
pub(crate) fn ramps_open_fx(
    fx_open: bool,
    streams: [&GlowRamps; 2],
    cost: Option<FxRampCost>,
) -> bool {
    let Some(cost) = cost else {
        return false;
    };
    let lit: u64 = streams.iter().map(|s| s.lit as u64).sum();
    lit > 0 && cost.pays(fx_open, lit)
}

/// Palette entries per row of the GL and D3D11 arms' palette texture (`cell.glsl` and
/// `cell.hlsl` fetch entry `j` at `(j % 1024, j / 1024)`).
#[cfg(any(gl_arm, d3d11_arm))]
pub(crate) const FX_PALETTE_ROW: u32 = 1024;

/// The GL and D3D11 arms' palette ring depth.
#[cfg(any(gl_arm, d3d11_arm))]
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

/// One shelf of a [`ShelfPack`]: its top, its height, and its free columns.
#[derive(Clone, Debug)]
struct Shelf {
    y: u32,
    h: u32,
    /// The free `[x0, x1)` column spans, sorted, disjoint and never adjacent
    /// (a release merges a span into its neighbours).
    free: Vec<(u32, u32)>,
}

/// A best-fit shelf packer over a `side × side` square. No gutter: the
/// fragment reads texels by integer address, never filtered, so a neighbour's
/// texel is never sampled. A released region's columns go back to its shelf,
/// and the trailing shelves left wholly free go back to the square.
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

    /// Place a `w × h` region: on the shortest shelf tall enough with room (in its leftmost
    /// free span wide enough), else on a new shelf, else `None`.
    fn place(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        let side = self.side;
        let mut best: Option<(usize, usize)> = None;
        for (i, s) in self.shelves.iter().enumerate() {
            if s.h < h || best.is_some_and(|(b, _)| self.shelves[b].h <= s.h) {
                continue;
            }
            if let Some(k) = s.free.iter().position(|&(x0, x1)| x1 - x0 >= w) {
                best = Some((i, k));
            }
        }
        if let Some((i, k)) = best {
            let s = &mut self.shelves[i];
            let (x0, x1) = s.free[k];
            if x0 + w == x1 {
                s.free.remove(k);
            } else {
                s.free[k].0 = x0 + w;
            }
            return Some((x0, s.y));
        }
        if w > side || self.bottom + h > side {
            return None;
        }
        let at = (0, self.bottom);
        self.shelves.push(Shelf {
            y: self.bottom,
            h,
            free: if w < side {
                vec![(w, side)]
            } else {
                Vec::new()
            },
        });
        self.bottom += h;
        Some(at)
    }

    /// Give back the `w` columns at `x` of the shelf at `y`, then every
    /// trailing shelf left wholly free.
    fn release(&mut self, x: u32, y: u32, w: u32) {
        let side = self.side;
        let Some(s) = self.shelves.iter_mut().find(|s| s.y == y) else {
            return;
        };
        let at = s.free.partition_point(|&(x0, _)| x0 < x);
        s.free.insert(at, (x, x + w));
        if s.free.get(at + 1).is_some_and(|&(x0, _)| x0 == x + w) {
            s.free[at].1 = s.free[at + 1].1;
            s.free.remove(at + 1);
        }
        if at > 0 && s.free[at - 1].1 == x {
            s.free[at - 1].1 = s.free[at].1;
            s.free.remove(at);
        }
        while self
            .shelves
            .last()
            .is_some_and(|s| s.free.as_slice() == [(0, side)])
        {
            if let Some(s) = self.shelves.pop() {
                self.bottom = s.y;
            }
        }
    }
}

/// One resident art: its texels, its region, the write that put it there,
/// and the last admission that drew or prefetched it.
#[derive(Debug)]
struct Resident {
    art: Arc<FxArt>,
    region: [u16; 4],
    seq: u64,
    used: u64,
}

/// One of the two fx atlases, CPU side: an LRU residency over one texture.
///
/// An art the frame draws or prefetches that is not resident takes a free region; failing
/// that, the least recently used run of arts this frame did not ask for gives its regions
/// back ([`FxAtlasSide::victims`]); failing that, a new generation is minted holding the
/// frame's arts. Each region write takes the next sequence number, and an arm's texture
/// syncs by uploading the resident arts written since its last sync ([`sync_fx_tex`]):
/// resident regions are disjoint, so upload order cannot matter, and an evicted region is
/// not read until a write puts a resident art there. On Metal, a write into a region a frame
/// in flight may read waits for those frames (`before_write`).
#[derive(Debug)]
pub(crate) struct FxAtlasSide {
    kind: FxAtlasKind,
    pack: ShelfPack,
    /// Bumped at every mint; `0` before the first (nothing resident, no
    /// texture: an arm binds a 1×1 placeholder).
    generation: u64,
    resident: FxHashMap<u128, Resident>,
    /// The last region write's sequence number (`0`: none yet).
    seq: u64,
    /// The admission clock, stamped on every art an admission asks for.
    clock: u64,
    /// Regions given back by eviction, over the side's life.
    evictions: u64,
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
            seq: 0,
            clock: 0,
            evictions: 0,
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

    /// The last region write's sequence number.
    #[inline]
    pub(crate) const fn seq(&self) -> u64 {
        self.seq
    }

    /// Regions given back by eviction so far.
    #[cfg(test)]
    #[inline]
    pub(crate) const fn evictions(&self) -> u64 {
        self.evictions
    }

    /// How many arts are resident.
    #[cfg(test)]
    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.resident.len()
    }

    /// The resident arts written after write `after`, in write order.
    pub(crate) fn written_after(&self, after: u64) -> Vec<(&Arc<FxArt>, [u16; 4])> {
        if self.seq <= after {
            return Vec::new();
        }
        let mut out: Vec<(u64, &Arc<FxArt>, [u16; 4])> = self
            .resident
            .values()
            .filter(|r| r.seq > after)
            .map(|r| (r.seq, &r.art, r.region))
            .collect();
        out.sort_unstable_by_key(|&(seq, ..)| seq);
        out.into_iter()
            .map(|(_, art, region)| (art, region))
            .collect()
    }

    /// The resident region of art `id`.
    #[inline]
    pub(crate) fn region(&self, id: u128) -> Option<[u16; 4]> {
        self.resident.get(&id).map(|r| r.region)
    }

    /// Make every art of `draw` (the arts this frame's sprites draw, unique,
    /// within the frame budget) and of `hint` (the prefetched ones, unique,
    /// none in `draw`) resident: each takes a free region or one eviction
    /// frees; when one cannot, mint a new generation holding `draw` and what
    /// of `hint` fits beside it.
    fn admit(&mut self, draw: &[Arc<FxArt>], hint: &[Arc<FxArt>], cap: u32) {
        self.clock += 1;
        let clock = self.clock;
        let mut all_resident = true;
        for art in draw.iter().chain(hint) {
            if let Some(r) = self.resident.get_mut(&art.id()) {
                r.used = clock;
            } else {
                all_resident = false;
            }
        }
        if all_resident {
            return;
        }
        let mut overflow = false;
        for art in draw {
            if self.resident.contains_key(&art.id()) || self.unfit_drawn.contains(&art.id()) {
                continue;
            }
            overflow |= !self.place_evicting(art);
        }
        for art in hint {
            if overflow {
                break;
            }
            let id = art.id();
            if self.resident.contains_key(&id) || self.unfit_hint.contains(&id) {
                continue;
            }
            overflow |= !self.place_evicting(art);
        }
        if overflow {
            self.mint(draw, hint, cap);
        }
    }

    /// Place `art`: in a free region, else in the room the cheapest
    /// [`Self::victims`] give back: whether it did.
    fn place_evicting(&mut self, art: &Arc<FxArt>) -> bool {
        let (w, h) = (u32::from(art.w()), u32::from(art.h()));
        if let Some((x, y)) = self.pack.place(w, h) {
            self.record(art, x, y);
            return true;
        }
        let Some(victims) = self.victims(w, h) else {
            return false;
        };
        for id in victims {
            if let Some(r) = self.resident.remove(&id) {
                let [x, y, w, _] = r.region;
                self.pack.release(u32::from(x), u32::from(y), u32::from(w));
                self.evictions += 1;
            }
        }
        match self.pack.place(w, h) {
            Some((x, y)) => {
                self.record(art, x, y);
                true
            }
            None => false,
        }
    }

    /// The arts whose eviction makes room for a `w × h` region, chosen among the runs that
    /// free it whole: neighbouring columns at least `w` wide on a shelf at least `h` tall, or
    /// the shelves from one to the bottom when giving them back leaves `h` rows under the
    /// last kept one. A run holds only arts no admission of this frame asked for. The pick is
    /// the run whose newest use is oldest (no art goes while an older one would do), then the
    /// fewest arts, the shortest shelf, the oldest writes. `None` when no run frees the room
    /// (the caller mints).
    fn victims(&self, w: u32, h: u32) -> Option<Vec<u128>> {
        /// A shelf's columns: a free span or a resident's region.
        struct Seg {
            x0: u32,
            x1: u32,
            held: Option<(u64, u64, u128)>,
        }
        let side = self.pack.side;
        if w > side || h > side {
            return None;
        }
        let clock = self.clock;
        let shelves = &self.pack.shelves;
        let mut by_shelf: Vec<Vec<Seg>> = shelves
            .iter()
            .map(|s| {
                s.free
                    .iter()
                    .map(|&(x0, x1)| Seg { x0, x1, held: None })
                    .collect()
            })
            .collect();
        for (&id, r) in &self.resident {
            let y = u32::from(r.region[1]);
            if let Some(k) = shelves.iter().position(|s| s.y == y) {
                let x0 = u32::from(r.region[0]);
                by_shelf[k].push(Seg {
                    x0,
                    x1: x0 + u32::from(r.region[2]),
                    held: Some((r.used, r.seq, id)),
                });
            }
        }
        // (newest use, arts, shelf height, newest write) of a run, and its arts.
        type RunKey = (u64, usize, u32, u64);
        let mut best: Option<(RunKey, Vec<u128>)> = None;
        let mut offer = |key: RunKey, run: &mut dyn Iterator<Item = u128>| {
            if best.as_ref().is_none_or(|(k, _)| key < *k) {
                best = Some((key, run.collect()));
            }
        };
        for (shelf, segs) in shelves.iter().zip(&mut by_shelf) {
            segs.sort_unstable_by_key(|s| s.x0);
            if shelf.h < h {
                continue;
            }
            for i in 0..segs.len() {
                let (mut used, mut seq, mut arts) = (0, 0, 0);
                for j in i..segs.len() {
                    let seg = &segs[j];
                    if j > i && segs[j - 1].x1 != seg.x0 {
                        break;
                    }
                    if let Some((u, q, _)) = seg.held {
                        if u >= clock {
                            break;
                        }
                        (used, seq, arts) = (used.max(u), seq.max(q), arts + 1);
                    }
                    if seg.x1 - segs[i].x0 >= w {
                        offer(
                            (used, arts, shelf.h, seq),
                            &mut segs[i..=j].iter().filter_map(|s| s.held.map(|(.., id)| id)),
                        );
                        break;
                    }
                }
            }
        }
        // The shelves from `k` down, given back whole.
        let (mut used, mut seq, mut arts) = (0, 0, 0);
        for k in (0..shelves.len()).rev() {
            let held = by_shelf[k].iter().filter_map(|s| s.held);
            if held.clone().any(|(u, ..)| u >= clock) {
                break;
            }
            for (u, q, _) in held {
                (used, seq, arts) = (used.max(u), seq.max(q), arts + 1);
            }
            if shelves[k].y + h <= side {
                offer(
                    (used, arts, h, seq),
                    &mut by_shelf[k..]
                        .iter()
                        .flatten()
                        .filter_map(|s| s.held.map(|(.., id)| id)),
                );
                break;
            }
        }
        best.map(|(_, run)| run)
    }

    fn record(&mut self, art: &Arc<FxArt>, x: u32, y: u32) {
        self.seq += 1;
        let region = [x as u16, y as u16, art.w(), art.h()];
        self.resident.insert(
            art.id(),
            Resident {
                art: Arc::clone(art),
                region,
                seq: self.seq,
                used: self.clock,
            },
        );
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
    /// The frame's one slicer, whose frame art budget is ruled once a frame
    /// by whichever reader of the frame's layers comes first (the knock-out's
    /// [`Self::knock_rects`], [`Self::admit`], [`Self::build`],
    /// [`Self::bloom_source`]); the others read that ruling.
    slicer: FxSlicer,
    /// The budget epoch ([`aterm_render::FxBudget::epoch`]) the last
    /// admission acted on: a different one is the sign that the layers
    /// changed, whoever ruled them.
    admitted: Option<u64>,
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
    /// The frames in flight the arm's sync waited for before a region
    /// rewrite, and the nanoseconds it parked (Metal's sync sets it).
    pub(crate) reader_wait: (u64, u64),
    /// The bloom layers' source quads, and their scratch.
    bloom_src: Vec<GlowQuad>,
    bloom_cols: aterm_render::SpriteCols,
}

impl FxGpu {
    /// Make every art this frame's layers draw, and every prefetched art, resident in the
    /// CPU atlases.
    pub(crate) fn admit(&mut self, layers: &[FxLayer], prefetch: &[Arc<FxArt>], cap: u32) {
        self.slicer.rule(layers);
        let epoch = self.slicer.budget().epoch();
        let changed = self.admitted != Some(epoch);
        if !changed && self.memo_cap == cap && self.prefetch_admitted(prefetch) {
            return;
        }
        self.admitted = Some(epoch);
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

    /// Build this frame's fx instances into `out` (one vector per [`FxStream`], cleared
    /// first) and its palette, slicing every layer under `band_open`.
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
            |l| FxStream::of_layer(l).is_some(),
            band_open,
            &mut self.slicer,
            &mut sink,
        );
        self.refused = sink.refused + self.slicer.budget().refused_items();
    }

    /// The bloom source: `cursor_glow_add` and then this frame's `bloom` layers' quads, or
    /// `None` when no layer blooms.
    pub(crate) fn bloom_source<'s>(
        &'s mut self,
        f: &FxFrame,
        input: &RenderInput,
    ) -> Option<&'s [GlowQuad]> {
        if !input.fx_layers.iter().any(|l| l.bloom) {
            return None;
        }
        self.bloom_src.clear();
        input
            .cursor_glow_add_ramps
            .expand_into(&input.cursor_glow_add, &mut self.bloom_src);
        aterm_render::fx_bloom_quads(
            f,
            &input.fx_layers,
            &input.fx_occluders,
            &mut self.slicer,
            &mut self.bloom_cols,
            &mut self.bloom_src,
        );
        Some(&self.bloom_src)
    }

    /// This frame's `bloom` layers' quads alone, the lights an item list
    /// carries after its stream's ([`aterm_render::bloom::items_into`]), or
    /// `None` when no layer blooms.
    pub(crate) fn bloom_layers<'s>(
        &'s mut self,
        f: &FxFrame,
        input: &RenderInput,
    ) -> Option<&'s [GlowQuad]> {
        if !input.fx_layers.iter().any(|l| l.bloom) {
            return None;
        }
        self.bloom_src.clear();
        aterm_render::fx_bloom_quads(
            f,
            &input.fx_layers,
            &input.fx_occluders,
            &mut self.slicer,
            &mut self.bloom_cols,
            &mut self.bloom_src,
        );
        Some(&self.bloom_src)
    }
}

/// Whether a frame feeds the bloom: it has light to blur, and its text can
/// take the blur ([`RenderInput::bloom_gate`]).
#[inline]
pub(crate) fn bloom_source_live(input: &RenderInput) -> bool {
    !input.bloom_gate.held()
        && (!input.cursor_glow_add.is_empty()
            || !input.cursor_glow_add_ramps.is_empty()
            || input.fx_layers.iter().any(|l| l.bloom))
}

/// Whether the present's crown (the HDR or SDR aurora boost over the bloom source) has
/// light to read.
#[inline]
pub(crate) fn crown_glow_present(input: &RenderInput) -> bool {
    input
        .cursor_glow_add
        .iter()
        .any(|q| (q.row as usize) < input.rows)
        || input
            .cursor_glow_add_ramps
            .items
            .iter()
            .any(|r| (r.row as usize) < input.rows)
        || input.fx_layers.iter().any(|l| l.bloom)
}

/// Whether every Exact Under layer of a frame draws only opaque pixels
/// ([`FxContent::draws_opaque`]): such light stores the same byte through
/// the sRGB view as the Unorm `over` law does (an alpha-255 `over` is a
/// store), so route (1) of the pass economy may draw it in the base pass.
/// A frame whose `glow_under` carries ramps draws that stream in the Under
/// stream ([`glow_stream_into`]), light that is not opaque.
#[inline]
pub(crate) fn under_draws_opaque(input: &RenderInput) -> bool {
    input.glow_under_ramps.is_empty()
        && input
            .fx_layers
            .iter()
            .filter(|l| FxStream::of(l.slot, l.blend) == Some(FxStream::Under))
            .all(|l| l.content.draws_opaque())
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
    /// The content's key in `bases`.
    content: usize,
    /// Resolved frame-palette offset (`Some(0)` when the fragment reads no palette).
    pal: Option<u32>,
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
        let Some(stream) = FxStream::of_layer(layer) else {
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
                content: Arc::as_ptr(&layer.content) as usize,
                pal: (!reads_palette).then_some(0),
                y_limit,
            },
            &layer.content,
        ));
        true
    }

    fn slice(&mut self, s: FxSlice, _band: usize) {
        let Some((cur, content)) = &mut self.cur else {
            return;
        };
        let y1 = (s.y1 as i64).min(cur.y_limit);
        if (s.y0 as i64) >= y1 {
            return;
        }
        let pal = match cur.pal {
            Some(pal) => pal,
            None => {
                let base = match self.bases.iter().rev().find(|(k, _)| *k == cur.content) {
                    Some(&(_, base)) => base,
                    None => {
                        let base = self.palette.len() as u32;
                        self.palette
                            .extend(content.palette().iter().map(|&ink| palette_word(ink)));
                        self.bases.push((cur.content, base));
                        base
                    }
                };
                let pal = base + cur.ink;
                cur.pal = Some(pal);
                pal
            }
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
    /// The last region write this texture holds ([`FxAtlasSide::seq`]).
    synced: u64,
}

/// Bring an arm's texture of `side` up to date: re-minted on a new
/// generation (or when `fresh` says the texture is gone), then every resident
/// art written since its last sync uploaded into its region; `before_write`
/// runs first when those writes land in a texture earlier frames may sample.
/// `Err` when the mint or an upload is refused (a removed D3D11 device), the
/// texture then unsynced, so the next sync writes those arts again.
pub(crate) fn sync_fx_tex(
    handle: crate::device_layer::DeviceHandle<'_>,
    slot: &mut Option<FxArmTex>,
    side: &FxAtlasSide,
    fresh: impl Fn(&FxArmTex) -> bool,
    before_write: impl FnOnce(),
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
            synced: 0,
        });
    }
    let t = slot.as_mut().expect("minted above");
    if t.synced >= side.seq() {
        return Ok(FxSynced {
            bytes: 0,
            minted: stale,
            appended: false,
        });
    }
    if !stale {
        before_write();
    }
    let bpp = side.kind.bytes_per_texel();
    let mut bytes = 0u64;
    for (art, r) in side.written_after(t.synced) {
        let (x, y, w, h) = (
            u32::from(r[0]),
            u32::from(r[1]),
            u32::from(r[2]),
            u32::from(r[3]),
        );
        handle.try_upload_texture_region(&t.tex, x, y, w, h, art.data(), w * bpp)?;
        bytes += art.data().len() as u64;
    }
    t.synced = side.seq();
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
    /// Whether arts were written into a texture earlier frames may sample.
    pub(crate) appended: bool,
}

/// Whether an arm must sync its texture of `side` this frame.
#[inline]
pub(crate) fn fx_tex_due(slot: &Option<FxArmTex>, side: &FxAtlasSide, drawing: bool) -> bool {
    drawing
        || (side.generation() > 0
            && slot
                .as_ref()
                .is_none_or(|t| t.generation != side.generation() || t.synced < side.seq()))
}

#[cfg(test)]
#[path = "fx_gpu_tests.rs"]
mod tests;
