// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! FX LAYERS: the engine's sprite and bitmap primitive (`docs/DESIGN-card-engine-2026-10-19.md` §4).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

/// A half-open pixel rectangle `[x0, x1) × [y0, y1)` in signed pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PxRect {
    /// Left edge (inclusive).
    pub x0: i32,
    /// Top edge (inclusive).
    pub y0: i32,
    /// Right edge (exclusive).
    pub x1: i32,
    /// Bottom edge (exclusive).
    pub y1: i32,
}

impl PxRect {
    /// The unbounded rectangle.
    pub const EVERYWHERE: Self = Self {
        x0: i32::MIN,
        y0: i32::MIN,
        x1: i32::MAX,
        y1: i32::MAX,
    };

    /// The canonical empty rectangle.
    pub const EMPTY: Self = Self {
        x0: 0,
        y0: 0,
        x1: 0,
        y1: 0,
    };

    /// `[x0, x1) × [y0, y1)`.
    #[must_use]
    #[inline]
    pub const fn new(x0: i32, y0: i32, x1: i32, y1: i32) -> Self {
        Self { x0, y0, x1, y1 }
    }

    /// The `w × h` rectangle with its top-left corner at `(x, y)`.
    #[must_use]
    #[inline]
    pub const fn at(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self {
            x0: x,
            y0: y,
            x1: sat_i32(x as i64 + w as i64),
            y1: sat_i32(y as i64 + h as i64),
        }
    }

    /// Whether the rectangle covers no pixel.
    #[must_use]
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }

    /// The common part of two rectangles (possibly empty).
    #[must_use]
    #[inline]
    pub const fn intersect(self, other: Self) -> Self {
        Self {
            x0: if self.x0 > other.x0 {
                self.x0
            } else {
                other.x0
            },
            y0: if self.y0 > other.y0 {
                self.y0
            } else {
                other.y0
            },
            x1: if self.x1 < other.x1 {
                self.x1
            } else {
                other.x1
            },
            y1: if self.y1 < other.y1 {
                self.y1
            } else {
                other.y1
            },
        }
    }

    /// Whether the two rectangles share a pixel.
    #[must_use]
    #[inline]
    pub const fn overlaps(self, other: Self) -> bool {
        !self.intersect(other).is_empty()
    }

    /// The smallest rectangle holding both.
    #[must_use]
    #[inline]
    pub const fn hull(self, other: Self) -> Self {
        match (self.is_empty(), other.is_empty()) {
            (true, true) => Self::EMPTY,
            (true, false) => other,
            (false, true) => self,
            (false, false) => Self {
                x0: if self.x0 < other.x0 {
                    self.x0
                } else {
                    other.x0
                },
                y0: if self.y0 < other.y0 {
                    self.y0
                } else {
                    other.y0
                },
                x1: if self.x1 > other.x1 {
                    self.x1
                } else {
                    other.x1
                },
                y1: if self.y1 > other.y1 {
                    self.y1
                } else {
                    other.y1
                },
            },
        }
    }

    /// The rectangle moved by `(dx, dy)`, saturating.
    #[must_use]
    #[inline]
    pub const fn translate(self, dx: i32, dy: i32) -> Self {
        Self {
            x0: shift_edge(self.x0, dx),
            y0: shift_edge(self.y0, dy),
            x1: shift_edge(self.x1, dx),
            y1: shift_edge(self.y1, dy),
        }
    }
}

/// One edge moved by `d`: an unbounded edge (`i32::MIN`/`i32::MAX`) stays put,
/// a bounded one saturates short of them.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the cast arm is reached only with v strictly inside i32's range"
)]
#[inline]
const fn shift_edge(edge: i32, d: i32) -> i32 {
    if edge == i32::MIN || edge == i32::MAX {
        edge
    } else {
        let v = edge as i64 + d as i64;
        if v <= i32::MIN as i64 {
            i32::MIN + 1
        } else if v >= i32::MAX as i64 {
            i32::MAX - 1
        } else {
            v as i32
        }
    }
}

/// `v` clamped into `i32`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the cast arm is reached only with v inside i32's range"
)]
#[inline]
const fn sat_i32(v: i64) -> i32 {
    if v > i32::MAX as i64 {
        i32::MAX
    } else if v < i32::MIN as i64 {
        i32::MIN
    } else {
        v as i32
    }
}

/// One palette entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FxInk {
    /// Premultiplied colour `0x00RRGGBB` (the tint under [`FxBlend::Linear`]).
    pub color: u32,
    /// Opacity `0..=255`.
    pub alpha: u8,
}

impl FxInk {
    /// Additive light of `color` (premultiplied).
    #[must_use]
    #[inline]
    pub const fn add(color: u32) -> Self {
        Self { color, alpha: 0 }
    }

    /// Source-over paint.
    #[must_use]
    #[inline]
    pub const fn over(color: u32, alpha: u8) -> Self {
        Self { color, alpha }
    }
}

/// How an [`FxArt`]'s bytes are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FxTexels {
    /// One byte per texel.
    Index8,
    /// Four bytes per texel, `R, G, B, A`, premultiplied.
    Rgba8Premul,
    /// Four bytes per texel, `R, G, B, A`, straight alpha, drawn under [`FxBlend::Linear`] with the sprite's palette entry as tint and opacity.
    Rgba8Straight,
}

impl FxTexels {
    /// Bytes per texel.
    #[must_use]
    #[inline]
    pub const fn bytes_per_texel(self) -> usize {
        match self {
            Self::Index8 => 1,
            Self::Rgba8Premul | Self::Rgba8Straight => 4,
        }
    }

    /// The blend an art of this kind is drawn under.
    #[must_use]
    #[inline]
    pub const fn blend(self) -> FxBlend {
        match self {
            Self::Index8 | Self::Rgba8Premul => FxBlend::Exact,
            Self::Rgba8Straight => FxBlend::Linear,
        }
    }

    /// Whether a packed texel value ([`FxArt::texel`]) writes nothing under pixel law F.
    #[must_use]
    #[inline]
    pub const fn is_clear(self, value: u32) -> bool {
        match self {
            Self::Index8 | Self::Rgba8Premul => value == 0,
            Self::Rgba8Straight => value >> 24 == 0,
        }
    }
}

/// One horizontal run of identical, drawing texels in one art row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FxRun {
    /// First texel column of the run.
    pub x: u16,
    /// Run length in texels (`>= 1`).
    pub len: u16,
    /// The packed texel value ([`FxArt::texel`]) every texel of the run holds.
    pub value: u32,
}

/// An art's per-row run lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FxRowRuns {
    /// `starts[y]..starts[y + 1]` indexes row `y`'s runs (`h + 1` entries).
    starts: Box<[u32]>,
    runs: Box<[FxRun]>,
}

impl FxRowRuns {
    /// The runs of art row `y`, left to right (empty past the last row).
    #[must_use]
    #[inline]
    pub fn row(&self, y: usize) -> &[FxRun] {
        let Some(next) = y.checked_add(1) else {
            return &[];
        };
        match (self.starts.get(y), self.starts.get(next)) {
            (Some(&a), Some(&b)) => &self.runs[a as usize..b as usize],
            _ => &[],
        }
    }

    fn build(art: &FxArt) -> Self {
        let (w, h) = (usize::from(art.w), usize::from(art.h));
        let mut starts = Vec::with_capacity(h + 1);
        let mut runs = Vec::new();
        for y in 0..h {
            starts.push(u32::try_from(runs.len()).expect("at most u16::MAX² texel runs"));
            let mut x = 0;
            while x < w {
                let value = art.texel(x, y);
                let mut end = x + 1;
                while end < w && art.texel(end, y) == value {
                    end += 1;
                }
                if !art.texels.is_clear(value) {
                    runs.push(FxRun {
                        x: u16::try_from(x).expect("column below the u16 art width"),
                        len: u16::try_from(end - x).expect("run at most the u16 art width"),
                        value,
                    });
                }
                x = end;
            }
        }
        starts.push(u32::try_from(runs.len()).expect("at most u16::MAX² texel runs"));
        Self {
            starts: starts.into_boxed_slice(),
            runs: runs.into_boxed_slice(),
        }
    }
}

/// The longest side, in texels, an [`FxArt`] may have.
pub const FX_ART_MAX_SIDE: u16 = 2048;

/// The art one frame may draw per texel width (the one-byte Index8 arts, and the four-byte RGBA ones).
pub const FX_FRAME_ART_TEXELS: u64 = 1 << 22;

/// An image the engine draws.
pub struct FxArt {
    id: u128,
    w: u16,
    h: u16,
    texels: FxTexels,
    data: Box<[u8]>,
    /// The largest Index8 texel (0 for an RGBA art): the palette reach
    /// `FxContent`'s seal checks.
    max_index: u8,
    /// CPU run lists, built once on first use.
    runs: OnceLock<FxRowRuns>,
}

impl FxArt {
    /// An art of `w × h` texels of kind `texels`, row-major, top row first.
    #[must_use]
    pub fn new(w: u16, h: u16, texels: FxTexels, data: impl Into<Box<[u8]>>) -> Option<Self> {
        if w > FX_ART_MAX_SIDE || h > FX_ART_MAX_SIDE {
            return None;
        }
        let data = data.into();
        let need = usize::from(w)
            .checked_mul(usize::from(h))?
            .checked_mul(texels.bytes_per_texel())?;
        if data.len() != need {
            return None;
        }
        let max_index = match texels {
            FxTexels::Index8 => data.iter().copied().max().unwrap_or(0),
            FxTexels::Rgba8Premul | FxTexels::Rgba8Straight => 0,
        };
        let kind = match texels {
            FxTexels::Index8 => 1u8,
            FxTexels::Rgba8Premul => 2,
            FxTexels::Rgba8Straight => 3,
        };
        let mut head = [0u8; 5];
        head[0] = kind;
        head[1..3].copy_from_slice(&w.to_le_bytes());
        head[3..5].copy_from_slice(&h.to_le_bytes());
        let id = aterm_hash::fnv1a_128_extend(aterm_hash::fnv1a_128(&head), &data);
        Some(Self {
            id,
            w,
            h,
            texels,
            data,
            max_index,
            runs: OnceLock::new(),
        })
    }

    /// The content address.
    #[must_use]
    #[inline]
    pub const fn id(&self) -> u128 {
        self.id
    }

    /// Width in texels.
    #[must_use]
    #[inline]
    pub const fn w(&self) -> u16 {
        self.w
    }

    /// Height in texels.
    #[must_use]
    #[inline]
    pub const fn h(&self) -> u16 {
        self.h
    }

    /// The texel kind.
    #[must_use]
    #[inline]
    pub const fn texels(&self) -> FxTexels {
        self.texels
    }

    /// The raw bytes, row-major.
    #[must_use]
    #[inline]
    pub const fn data(&self) -> &[u8] {
        &self.data
    }

    /// Whether the art has no texel.
    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// The largest Index8 texel value (0 for an RGBA art, or a blank one).
    #[must_use]
    #[inline]
    pub const fn max_index(&self) -> u8 {
        self.max_index
    }

    /// The packed value of texel `(x, y)`.
    ///
    /// # Panics
    /// When `(x, y)` lies outside the art.
    #[must_use]
    #[inline]
    pub fn texel(&self, x: usize, y: usize) -> u32 {
        assert!(
            x < usize::from(self.w) && y < usize::from(self.h),
            "texel ({x}, {y}) outside a {}x{} art",
            self.w,
            self.h
        );
        let i = y * usize::from(self.w) + x;
        match self.texels {
            FxTexels::Index8 => u32::from(self.data[i]),
            FxTexels::Rgba8Premul | FxTexels::Rgba8Straight => {
                let t = &self.data[i * 4..i * 4 + 4];
                u32::from(t[3]) << 24
                    | u32::from(t[0]) << 16
                    | u32::from(t[1]) << 8
                    | u32::from(t[2])
            }
        }
    }

    /// The per-row run lists the CPU rasterizer stamps from, built on first call and kept.
    #[inline]
    pub fn runs(&self) -> &FxRowRuns {
        self.runs.get_or_init(|| FxRowRuns::build(self))
    }
}

impl std::fmt::Debug for FxArt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FxArt")
            .field("id", &format_args!("{:032x}", self.id))
            .field("w", &self.w)
            .field("h", &self.h)
            .field("texels", &self.texels)
            .finish_non_exhaustive()
    }
}

/// An image item.
#[derive(Debug, Clone)]
pub struct FxSprite {
    /// Left edge of the destination, in the content's space.
    pub x: i32,
    /// Top edge of the destination, in the content's space.
    pub y: i32,
    /// Destination width in pixels.
    pub dw: u16,
    /// Destination height in pixels.
    pub dh: u16,
    /// Mirror the art horizontally.
    pub flip_x: bool,
    /// The image.
    pub art: Arc<FxArt>,
    /// The palette base.
    pub ink: u32,
    /// The sprite's own clip, in the content's space (moves with the layer).
    pub clip: PxRect,
}

/// A rectangle item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FxRect {
    /// Left edge, in the content's space.
    pub x: i32,
    /// Top edge, in the content's space.
    pub y: i32,
    /// Width in pixels.
    pub w: u16,
    /// Height in pixels.
    pub h: u16,
    /// Palette base.
    pub ink: u32,
}

/// A stack of `h` one-row ramps sharing `x` and `w`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FxRamps {
    /// Left edge, in the content's space.
    pub x: i32,
    /// Top edge (row 0), in the content's space.
    pub y: i32,
    /// Width in pixels.
    pub w: u16,
    /// Number of rows.
    pub h: u16,
    /// Palette base.
    pub ink: u32,
}

/// One item of an [`FxContent`].
#[derive(Debug, Clone)]
pub enum FxItem {
    /// A (gradient) rectangle.
    Rect(FxRect),
    /// A stack of one-row ramps.
    Ramps(FxRamps),
    /// An image.
    Sprite(FxSprite),
}

impl FxItem {
    /// The pixels the item may write, in the content's space.
    #[must_use]
    #[inline]
    pub const fn bounds(&self) -> PxRect {
        match self {
            Self::Rect(r) => PxRect::at(r.x, r.y, r.w as u32, r.h as u32),
            Self::Ramps(r) => PxRect::at(r.x, r.y, r.w as u32, r.h as u32),
            Self::Sprite(s) => PxRect::at(s.x, s.y, s.dw as u32, s.dh as u32).intersect(s.clip),
        }
    }

    /// The blend this item is drawn under.
    #[must_use]
    #[inline]
    pub fn blend(&self) -> FxBlend {
        match self {
            Self::Rect(_) | Self::Ramps(_) => FxBlend::Exact,
            Self::Sprite(s) => s.art.texels.blend(),
        }
    }
}

/// What a sealed [`FxContent`] holds, by blend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FxClass {
    /// No item.
    Empty,
    /// Only items one blend draws.
    Only(FxBlend),
    /// Items of both blends: refused at seal in a debug build, drawn by
    /// neither in release.
    Mixed,
}

/// A layer's retained payload.
#[derive(Debug)]
pub struct FxContent {
    items: Vec<FxItem>,
    palette: Vec<FxInk>,
    extent: PxRect,
    class: FxClass,
}

/// [`fx_layer_builds`]'s counter.
static FX_LAYER_BUILDS: AtomicU64 = AtomicU64::new(0);
/// [`fx_payload_copies`]'s counter.
static FX_PAYLOAD_COPIES: AtomicU64 = AtomicU64::new(0);

/// How many [`FxContent`] payloads this process has sealed.
#[must_use]
#[inline]
pub fn fx_layer_builds() -> u64 {
    FX_LAYER_BUILDS.load(Ordering::Relaxed)
}

/// How many deep copies of an [`FxContent`] payload this process has made (`metrics`' `fx_payload_copies`).
#[must_use]
#[inline]
pub fn fx_payload_copies() -> u64 {
    FX_PAYLOAD_COPIES.load(Ordering::Relaxed)
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// This thread's share of the two counters, `(builds, copies)`: a test
    /// runs beside others that seal and copy contents of their own.
    static FX_COUNTS_HERE: std::cell::Cell<(u64, u64)> = const { std::cell::Cell::new((0, 0)) };
}

/// Book `(builds, copies)` on this thread's counters.
#[inline]
fn count_here(builds: u64, copies: u64) {
    #[cfg(any(test, debug_assertions))]
    FX_COUNTS_HERE.with(|c| {
        let (b, k) = c.get();
        c.set((b + builds, k + copies));
    });
    #[cfg(not(any(test, debug_assertions)))]
    let _ = (builds, copies);
}

/// [`fx_layer_builds`] and [`fx_payload_copies`] booked on THIS thread.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
#[inline]
pub fn fx_counts_here() -> (u64, u64) {
    FX_COUNTS_HERE.with(std::cell::Cell::get)
}

impl Clone for FxContent {
    #[inline]
    fn clone(&self) -> Self {
        FX_PAYLOAD_COPIES.fetch_add(1, Ordering::Relaxed);
        count_here(0, 1);
        Self {
            items: self.items.clone(),
            palette: self.palette.clone(),
            extent: self.extent,
            class: self.class,
        }
    }
}

impl Default for FxContent {
    #[inline]
    fn default() -> Self {
        Self {
            items: Vec::new(),
            palette: Vec::new(),
            extent: PxRect::EMPTY,
            class: FxClass::Empty,
        }
    }
}

impl FxContent {
    /// Sealed content of `items` over `palette`.
    ///
    /// # Panics (debug)
    /// On a mixture of blends, a sprite over a zero-size art, or a palette index past the end (see [`Self::edit`]).
    #[must_use]
    pub fn new(items: Vec<FxItem>, palette: Vec<FxInk>) -> Self {
        let mut content = Self {
            items,
            palette,
            ..Self::default()
        };
        content.seal(true);
        content
    }

    /// Change the items and the palette in place, then re-seal.
    ///
    /// # Panics (debug)
    /// When content mixes [`FxBlend::Exact`] and [`FxBlend::Linear`], references out-of-bounds palette entries, or mixes additive and source-over stops.
    pub fn edit(&mut self, f: impl FnOnce(&mut Vec<FxItem>, &mut Vec<FxInk>)) {
        struct Reseal<'a> {
            content: &'a mut FxContent,
            sealed: bool,
        }
        impl Drop for Reseal<'_> {
            fn drop(&mut self) {
                if !self.sealed {
                    self.content.seal(false);
                }
            }
        }
        let mut guard = Reseal {
            content: self,
            sealed: false,
        };
        f(&mut guard.content.items, &mut guard.content.palette);
        guard.content.seal(true);
        guard.sealed = true;
    }

    /// Sealed content WITHOUT the debug refusals, for the tests of what the renderers draw for a malformed item (nothing, per pixel law F).
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    #[must_use]
    pub fn new_unchecked(items: Vec<FxItem>, palette: Vec<FxInk>) -> Self {
        let mut content = Self {
            items,
            palette,
            ..Self::default()
        };
        content.seal(false);
        content
    }

    /// The items, in draw order.
    #[must_use]
    #[inline]
    pub fn items(&self) -> &[FxItem] {
        &self.items
    }

    /// The palette every item indexes.
    #[must_use]
    #[inline]
    pub fn palette(&self) -> &[FxInk] {
        &self.palette
    }

    /// The hull of every item's bounds, in the content's space ([`PxRect::EMPTY`] when nothing can draw).
    #[must_use]
    #[inline]
    pub const fn extent(&self) -> PxRect {
        self.extent
    }

    /// Whether a layer of blend `blend` draws this content.
    #[must_use]
    #[inline]
    pub const fn accepts(&self, blend: FxBlend) -> bool {
        match self.class {
            FxClass::Empty => true,
            FxClass::Only(b) => matches!(
                (b, blend),
                (FxBlend::Exact, FxBlend::Exact) | (FxBlend::Linear, FxBlend::Linear)
            ),
            FxClass::Mixed => false,
        }
    }

    fn seal(&mut self, refuse: bool) {
        FX_LAYER_BUILDS.fetch_add(1, Ordering::Relaxed);
        count_here(1, 0);
        let mut extent = PxRect::EMPTY;
        let mut class = FxClass::Empty;
        let pal = self.palette.len() as u64;
        for (i, item) in self.items.iter().enumerate() {
            extent = extent.hull(item.bounds());
            class = match (class, item.blend()) {
                (FxClass::Empty, b) => FxClass::Only(b),
                (FxClass::Only(a), b) if a == b => class,
                _ => FxClass::Mixed,
            };
            if refuse {
                Self::refuse_malformed(i, item, &self.palette, pal);
            }
        }
        if refuse {
            assert!(
                !cfg!(debug_assertions) || class != FxClass::Mixed,
                "fx content mixes Exact and Linear items: one layer has one blend"
            );
        }
        self.extent = extent;
        self.class = class;
    }

    /// The debug refusals of [`Self::edit`] for one item.
    fn refuse_malformed(i: usize, item: &FxItem, palette: &[FxInk], pal: u64) {
        if !cfg!(debug_assertions) {
            return;
        }
        let one_mode = |k: u64| {
            let at = |k: u64| usize::try_from(k).ok().and_then(|k| palette.get(k));
            Some((at(k)?.alpha == 0) == (at(k + 1)?.alpha == 0))
        };
        match item {
            FxItem::Rect(r) => {
                let j = u64::from(r.ink);
                assert!(
                    j + 1 < pal,
                    "fx item {i}: rect ink {j}+1 past a {pal}-entry palette"
                );
                assert!(
                    one_mode(j) == Some(true),
                    "fx item {i}: a rect is additive at both ends or source-over at both ends"
                );
            }
            FxItem::Ramps(r) => {
                let j = u64::from(r.ink);
                let need = 2 * u64::from(r.h);
                assert!(
                    r.h == 0 || j + need <= pal,
                    "fx item {i}: ramps ink {j} + {need} entries past a {pal}-entry palette"
                );
                for row in 0..u64::from(r.h) {
                    assert!(
                        one_mode(j + 2 * row) == Some(true),
                        "fx item {i}: ramps row {row} is additive at one end only"
                    );
                }
            }
            FxItem::Sprite(s) => {
                assert!(
                    !s.art.is_empty(),
                    "fx item {i}: a sprite of a zero-size art"
                );
                let j = u64::from(s.ink);
                match s.art.texels {
                    FxTexels::Index8 => {
                        let k = u64::from(s.art.max_index);
                        assert!(
                            k == 0 || j + k - 1 < pal,
                            "fx item {i}: index {k} at ink {j} past a {pal}-entry palette"
                        );
                    }
                    FxTexels::Rgba8Straight => assert!(
                        j < pal,
                        "fx item {i}: tint entry {j} past a {pal}-entry palette"
                    ),
                    FxTexels::Rgba8Premul => {}
                }
            }
        }
    }
}

/// Where a layer sits in the frame's painter's order (`docs/DESIGN-card-engine-2026-10-19.md` §4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FxSlot {
    /// Under the glyph ink.
    Under,
    /// Over the glyphs, right after the cursor aurora (`cursor_glow_add`) and before the radial halos.
    Glow,
    /// Over the glyphs, right after the nova light (so over pane chrome such as the scrollbar) and before the rain halos.
    Over,
}

/// Which coordinate space a layer's `offset` and `clip` are in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FxSpace {
    /// Window pixels (the cursor-effect streams' space).
    Window,
    /// Grid-interior pixels.
    Grid,
}

/// How a layer composites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FxBlend {
    /// The Unorm byte law.
    Exact,
    /// FreeSprite's linear-light law over an sRGB view (CPU `blend`), within its declared parity tolerance.
    Linear,
}

/// One retained fx layer of a frame.
#[derive(Debug, Clone)]
pub struct FxLayer {
    /// The host's stable name for the layer (damage matches layers by it).
    pub id: u64,
    /// The sealed payload.
    pub content: Arc<FxContent>,
    /// The z-slot.
    pub slot: FxSlot,
    /// The space `offset` and `clip` are in.
    pub space: FxSpace,
    /// The compositing law.
    pub blend: FxBlend,
    /// Also feed the GPU bloom source (GPU-only; the CPU ignores it).
    pub bloom: bool,
    /// Where the content's origin lands, in `space`.
    pub offset: (i32, i32),
    /// The layer's clip in `space` (a pane box).
    pub clip: PxRect,
}

impl FxLayer {
    /// A layer at offset `(0, 0)`, unclipped, without bloom.
    ///
    /// # Panics (debug)
    /// When the content cannot be drawn under `blend`, or a [`FxBlend::Linear`] layer asks for [`FxSlot::Glow`].
    #[must_use]
    #[inline]
    pub fn new(
        id: u64,
        content: Arc<FxContent>,
        slot: FxSlot,
        space: FxSpace,
        blend: FxBlend,
    ) -> Self {
        debug_assert!(
            content.accepts(blend),
            "fx layer {id}: its content is not drawn under {blend:?}"
        );
        debug_assert!(
            !(blend == FxBlend::Linear && slot == FxSlot::Glow),
            "fx layer {id}: a Linear layer draws at Under or Over"
        );
        Self {
            id,
            content,
            slot,
            space,
            blend,
            bloom: false,
            offset: (0, 0),
            clip: PxRect::EVERYWHERE,
        }
    }

    /// The pixels the layer may write, in its space.
    #[must_use]
    #[inline]
    pub fn extent(&self) -> PxRect {
        self.content
            .extent()
            .translate(self.offset.0, self.offset.1)
            .intersect(self.clip)
    }
}

impl PartialEq for FxLayer {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && Arc::ptr_eq(&self.content, &other.content)
            && self.slot == other.slot
            && self.space == other.space
            && self.blend == other.blend
            && self.bloom == other.bloom
            && self.offset == other.offset
            && self.clip == other.clip
    }
}

impl Eq for FxLayer {}

#[cfg(test)]
mod tests {
    use super::*;

    fn index_art(w: u16, h: u16, data: &[u8]) -> Arc<FxArt> {
        Arc::new(FxArt::new(w, h, FxTexels::Index8, data.to_vec()).expect("sized"))
    }

    #[test]
    fn an_art_is_addressed_by_its_kind_size_and_bytes() {
        let a = FxArt::new(2, 2, FxTexels::Index8, vec![0, 1, 2, 3]).unwrap();
        let same = FxArt::new(2, 2, FxTexels::Index8, vec![0, 1, 2, 3]).unwrap();
        assert_eq!(a.id(), same.id());
        let byte = FxArt::new(2, 2, FxTexels::Index8, vec![0, 1, 2, 4]).unwrap();
        let shape = FxArt::new(4, 1, FxTexels::Index8, vec![0, 1, 2, 3]).unwrap();
        let kind = FxArt::new(1, 1, FxTexels::Rgba8Premul, vec![0, 1, 2, 3]).unwrap();
        let kind2 = FxArt::new(1, 1, FxTexels::Rgba8Straight, vec![0, 1, 2, 3]).unwrap();
        for other in [&byte, &shape, &kind, &kind2] {
            assert_ne!(a.id(), other.id(), "{other:?}");
        }
        assert_ne!(kind.id(), kind2.id());
        assert_eq!(a.max_index(), 3);
        assert_eq!(kind.max_index(), 0);
        assert!(FxArt::new(2, 2, FxTexels::Index8, vec![0; 3]).is_none());
        assert!(FxArt::new(1, 1, FxTexels::Rgba8Premul, vec![0; 3]).is_none());
        assert!(FxArt::new(0, 5, FxTexels::Index8, Vec::new()).is_some_and(|a| a.is_empty()));
        let max = FX_ART_MAX_SIDE;
        assert!(FxArt::new(max, max, FxTexels::Rgba8Premul, Vec::new()).is_none());
        assert!(FxArt::new(max, max, FxTexels::Rgba8Straight, Vec::new()).is_none());
        assert!(
            FxArt::new(max, 0, FxTexels::Rgba8Straight, Vec::new())
                .is_some_and(|art| art.is_empty())
        );
        let side = usize::from(max);
        assert!(FxArt::new(max, 1, FxTexels::Index8, vec![1; side]).is_some());
        assert!(FxArt::new(max + 1, 1, FxTexels::Index8, vec![1; side + 1]).is_none());
        assert!(FxArt::new(1, max + 1, FxTexels::Index8, vec![1; side + 1]).is_none());
        assert!(FxArt::new(max + 1, 0, FxTexels::Index8, Vec::new()).is_none());
        assert!(FxArt::new(u16::MAX, u16::MAX, FxTexels::Rgba8Premul, Vec::new()).is_none());
    }

    /// The run lists, expanded, are exactly the drawing texels: every
    /// non-clear texel lies in one run of its own value, no clear texel lies
    /// in any, runs are maximal and ordered.
    #[test]
    fn run_lists_expand_to_exactly_the_drawing_texels() {
        let empty = FxArt::new(0, 0, FxTexels::Index8, Vec::new()).unwrap();
        assert!(empty.runs().row(0).is_empty());
        assert!(empty.runs().row(usize::MAX).is_empty());
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for case in 0..300 {
            let (w, h) = ((next() % 17) as u16, (next() % 9) as u16);
            let texels = [
                FxTexels::Index8,
                FxTexels::Rgba8Premul,
                FxTexels::Rgba8Straight,
            ][case % 3];
            let n = usize::from(w) * usize::from(h) * texels.bytes_per_texel();
            let data: Vec<u8> = (0..n)
                .map(|_| [0u8, 0, 1, 2, 200][(next() % 5) as usize])
                .collect();
            let art = FxArt::new(w, h, texels, data).unwrap();
            let runs = art.runs();
            for y in 0..usize::from(h) {
                let mut covered = vec![None; usize::from(w)];
                let mut last_end = 0usize;
                for (k, run) in runs.row(y).iter().enumerate() {
                    let (x0, x1) = (usize::from(run.x), usize::from(run.x + run.len));
                    assert!(run.len >= 1 && x0 >= last_end && x1 <= usize::from(w));
                    assert!(!texels.is_clear(run.value), "case {case}: a clear run");
                    if x0 > 0 {
                        assert_ne!(art.texel(x0 - 1, y), run.value, "case {case} run {k}");
                    }
                    if x1 < usize::from(w) {
                        assert_ne!(art.texel(x1, y), run.value, "case {case} run {k}");
                    }
                    for c in &mut covered[x0..x1] {
                        *c = Some(run.value);
                    }
                    last_end = x1;
                }
                for (x, c) in covered.iter().enumerate() {
                    let t = art.texel(x, y);
                    if texels.is_clear(t) {
                        assert_eq!(*c, None, "case {case}: clear texel ({x}, {y}) in a run");
                    } else {
                        assert_eq!(*c, Some(t), "case {case}: texel ({x}, {y})");
                    }
                }
            }
            assert!(runs.row(usize::from(h)).is_empty());
        }
    }

    #[test]
    fn seal_takes_the_hull_of_every_item_and_a_sprite_within_its_clip() {
        let art = index_art(2, 2, &[1, 1, 1, 1]);
        let content = FxContent::new(
            vec![
                FxItem::Rect(FxRect {
                    x: -5,
                    y: 3,
                    w: 4,
                    h: 2,
                    ink: 0,
                }),
                FxItem::Sprite(FxSprite {
                    x: 10,
                    y: -7,
                    dw: 20,
                    dh: 20,
                    flip_x: false,
                    art: art.clone(),
                    ink: 0,
                    clip: PxRect::new(12, -100, 15, 100),
                }),
                FxItem::Rect(FxRect {
                    x: 1000,
                    y: 1000,
                    w: 0,
                    h: 9,
                    ink: 0,
                }),
                FxItem::Ramps(FxRamps {
                    x: 0,
                    y: 30,
                    w: 1,
                    h: 1,
                    ink: 0,
                }),
            ],
            vec![FxInk::add(0x10_2030); 2],
        );
        assert_eq!(content.extent(), PxRect::new(-5, -7, 15, 31));
        assert!(content.accepts(FxBlend::Exact));
        assert!(!content.accepts(FxBlend::Linear));
        assert_eq!(FxContent::default().extent(), PxRect::EMPTY);
        assert!(FxContent::default().accepts(FxBlend::Linear));
    }

    #[test]
    fn edit_reseals_and_keeps_capacity() {
        let mut content = FxContent::new(Vec::with_capacity(16), vec![FxInk::add(1); 2]);
        content.edit(|items, _| {
            items.push(FxItem::Rect(FxRect {
                x: 1,
                y: 2,
                w: 3,
                h: 4,
                ink: 0,
            }));
        });
        assert_eq!(content.extent(), PxRect::new(1, 2, 4, 6));
        let cap = content.items().as_ptr();
        content.edit(|items, _| {
            items.clear();
            items.push(FxItem::Rect(FxRect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
                ink: 0,
            }));
        });
        assert_eq!(content.extent(), PxRect::new(0, 0, 1, 1));
        assert_eq!(content.items().as_ptr(), cap, "the refill reuses the Vec");

        let straight =
            Arc::new(FxArt::new(1, 1, FxTexels::Rgba8Straight, vec![1, 2, 3, 255]).unwrap());
        let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            content.edit(|items, _| {
                items.clear();
                items.push(FxItem::Sprite(FxSprite {
                    x: -4,
                    y: 9,
                    dw: 6,
                    dh: 2,
                    flip_x: false,
                    art: straight,
                    ink: 0,
                    clip: PxRect::EVERYWHERE,
                }));
                panic!("captured edit interruption");
            });
        }));
        assert!(interrupted.is_err());
        assert_eq!(content.items().len(), 1, "completed edits remain");
        assert_eq!(content.extent(), PxRect::new(-4, 9, 2, 11));
        assert!(
            content.accepts(FxBlend::Linear),
            "the new items set the blend"
        );
        assert!(!content.accepts(FxBlend::Exact));
        assert_eq!(content.items().as_ptr(), cap, "resealing keeps the Vec");

        content.edit(|items, _| items.clear());
        assert!(content.items().is_empty());
        assert_eq!(content.extent(), PxRect::EMPTY);
        assert!(content.accepts(FxBlend::Exact) && content.accepts(FxBlend::Linear));
    }

    #[test]
    #[should_panic(expected = "mixes Exact and Linear")]
    fn seal_refuses_a_mixture_of_blends() {
        let straight = Arc::new(FxArt::new(1, 1, FxTexels::Rgba8Straight, vec![1; 4]).unwrap());
        let _ = FxContent::new(
            vec![
                FxItem::Rect(FxRect {
                    x: 0,
                    y: 0,
                    w: 1,
                    h: 1,
                    ink: 0,
                }),
                FxItem::Sprite(FxSprite {
                    x: 0,
                    y: 0,
                    dw: 1,
                    dh: 1,
                    flip_x: false,
                    art: straight,
                    ink: 0,
                    clip: PxRect::EVERYWHERE,
                }),
            ],
            vec![FxInk::add(1); 2],
        );
    }

    #[test]
    #[should_panic(expected = "zero-size art")]
    fn seal_refuses_a_zero_size_art() {
        let blank = Arc::new(FxArt::new(0, 3, FxTexels::Index8, Vec::new()).unwrap());
        let _ = FxContent::new(
            vec![FxItem::Sprite(FxSprite {
                x: 0,
                y: 0,
                dw: 4,
                dh: 4,
                flip_x: false,
                art: blank,
                ink: 0,
                clip: PxRect::EVERYWHERE,
            })],
            Vec::new(),
        );
    }

    #[test]
    #[should_panic(expected = "past a 3-entry palette")]
    fn seal_refuses_an_index_past_the_palette() {
        let _ = FxContent::new(
            vec![FxItem::Sprite(FxSprite {
                x: 0,
                y: 0,
                dw: 2,
                dh: 1,
                flip_x: false,
                art: index_art(2, 1, &[1, 3]),
                ink: 1,
                clip: PxRect::EVERYWHERE,
            })],
            vec![FxInk::add(1); 3],
        );
    }

    #[test]
    #[should_panic(expected = "past a 3-entry palette")]
    fn seal_refuses_a_ramps_stack_past_the_palette() {
        let _ = FxContent::new(
            vec![FxItem::Ramps(FxRamps {
                x: 0,
                y: 0,
                w: 2,
                h: 2,
                ink: 0,
            })],
            vec![FxInk::add(1); 3],
        );
    }

    #[test]
    #[should_panic(expected = "additive at both ends")]
    fn seal_refuses_a_rect_additive_at_one_end() {
        let _ = FxContent::new(
            vec![FxItem::Rect(FxRect {
                x: 0,
                y: 0,
                w: 2,
                h: 2,
                ink: 0,
            })],
            vec![FxInk::add(1), FxInk::over(1, 9)],
        );
    }

    #[test]
    #[should_panic(expected = "ramps row 1 is additive at one end only")]
    fn seal_refuses_a_ramps_row_additive_at_one_end() {
        let _ = FxContent::new(
            vec![FxItem::Ramps(FxRamps {
                x: 0,
                y: 0,
                w: 2,
                h: 2,
                ink: 0,
            })],
            vec![
                FxInk::add(1),
                FxInk::add(2),
                FxInk::add(3),
                FxInk::over(1, 9),
            ],
        );
    }

    #[test]
    #[should_panic(expected = "tint entry 2 past a 2-entry palette")]
    fn seal_refuses_a_straight_tint_past_the_palette() {
        let straight = Arc::new(FxArt::new(1, 1, FxTexels::Rgba8Straight, vec![9; 4]).unwrap());
        let _ = FxContent::new(
            vec![FxItem::Sprite(FxSprite {
                x: 0,
                y: 0,
                dw: 1,
                dh: 1,
                flip_x: false,
                art: straight,
                ink: 2,
                clip: PxRect::EVERYWHERE,
            })],
            vec![FxInk::over(0x00FF_FFFF, 255); 2],
        );
    }

    #[test]
    #[should_panic(expected = "fx layer 4: its content is not drawn under Linear")]
    fn a_layer_refuses_content_its_blend_does_not_draw() {
        let rects = Arc::new(FxContent::new(
            vec![FxItem::Rect(FxRect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
                ink: 0,
            })],
            vec![FxInk::add(1); 2],
        ));
        let _ = FxLayer::new(4, rects, FxSlot::Over, FxSpace::Window, FxBlend::Linear);
    }

    #[test]
    #[should_panic(expected = "fx layer 5: a Linear layer draws at Under or Over")]
    fn a_layer_refuses_linear_at_glow() {
        let _ = FxLayer::new(
            5,
            Arc::new(FxContent::default()),
            FxSlot::Glow,
            FxSpace::Window,
            FxBlend::Linear,
        );
    }

    /// The unchecked seam seals the hull and the class but refuses nothing,
    /// so the renderers' fail-closed law can be tested.
    #[test]
    fn the_unchecked_seam_seals_without_refusing() {
        let content = FxContent::new_unchecked(
            vec![FxItem::Rect(FxRect {
                x: 0,
                y: 0,
                w: 2,
                h: 2,
                ink: 7,
            })],
            Vec::new(),
        );
        assert_eq!(content.extent(), PxRect::new(0, 0, 2, 2));
    }

    #[test]
    fn layer_equality_is_identity() {
        let content = Arc::new(FxContent::default());
        let base = FxLayer::new(
            7,
            content.clone(),
            FxSlot::Under,
            FxSpace::Window,
            FxBlend::Exact,
        );
        assert_eq!(base, base.clone());
        let twin = FxLayer {
            content: Arc::new(FxContent::default()),
            ..base.clone()
        };
        assert_ne!(base, twin, "equal bytes in a new Arc are a change");
        let variants: [fn(&mut FxLayer); 7] = [
            |l| l.id += 1,
            |l| l.slot = FxSlot::Over,
            |l| l.space = FxSpace::Grid,
            |l| l.blend = FxBlend::Linear,
            |l| l.bloom = true,
            |l| l.offset.1 += 1,
            |l| l.clip = PxRect::new(0, 0, 1, 1),
        ];
        for (i, change) in variants.iter().enumerate() {
            let mut other = base.clone();
            change(&mut other);
            assert_ne!(base, other, "field change {i} must compare unequal");
        }
    }

    #[test]
    fn rect_algebra() {
        let a = PxRect::new(0, 0, 10, 10);
        let b = PxRect::new(5, -5, 20, 5);
        assert_eq!(a.intersect(b), PxRect::new(5, 0, 10, 5));
        assert!(a.overlaps(b));
        assert!(!a.overlaps(PxRect::new(10, 0, 11, 1)), "half-open");
        assert_eq!(a.hull(b), PxRect::new(0, -5, 20, 10));
        assert_eq!(a.hull(PxRect::EMPTY), a);
        assert_eq!(PxRect::EMPTY.hull(PxRect::new(3, 3, 3, 9)), PxRect::EMPTY);
        assert_eq!(a.translate(-3, 4), PxRect::new(-3, 4, 7, 14));
        assert_eq!(PxRect::EVERYWHERE.translate(5, -5), PxRect::EVERYWHERE);
        let half = PxRect::new(i32::MIN, 0, 10, i32::MAX).translate(1, 1);
        assert_eq!(half, PxRect::new(i32::MIN, 1, 11, i32::MAX));
        assert_eq!(PxRect::at(i32::MAX - 1, 0, 5, 5).x1, i32::MAX);
        let layer = FxLayer {
            offset: (100, 50),
            clip: PxRect::new(0, 0, 103, 1000),
            ..FxLayer::new(
                1,
                Arc::new(FxContent::new(
                    vec![FxItem::Rect(FxRect {
                        x: 1,
                        y: 2,
                        w: 5,
                        h: 5,
                        ink: 0,
                    })],
                    vec![FxInk::add(1); 2],
                )),
                FxSlot::Over,
                FxSpace::Grid,
                FxBlend::Exact,
            )
        };
        assert_eq!(layer.extent(), PxRect::new(101, 52, 103, 57));
    }
}
