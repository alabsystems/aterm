// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! First-party [`Font`](crate::font::Font) face backed by `aterm-font` and
//! [`crate::variation::varied_glyph_raster_with_face`] ([`crate::variation`], [`crate::raster`]).
//!
//! Exposes [`Font::from_bytes`](crate::font::Font::from_bytes), [`Font::from_shared_slice`](crate::font::Font::from_shared_slice), [`Font::from_shared_vec`](crate::font::Font::from_shared_vec),
//! [`Font::rasterize`](crate::font::Font::rasterize), [`Font::rasterize_indexed`](crate::font::Font::rasterize_indexed), [`Font::lookup_glyph_index`](crate::font::Font::lookup_glyph_index),
//! [`Font::unicode_glyph_index`](crate::font::Font::unicode_glyph_index), [`Font::horizontal_line_metrics`](crate::font::Font::horizontal_line_metrics), [`Font::horizontal_kern`](crate::font::Font::horizontal_kern),
//! and [`Font::metrics`](crate::font::Font::metrics) alongside [`Metrics`](crate::font::Metrics), [`LineMetrics`](crate::font::LineMetrics), and [`FontSettings`](crate::font::FontSettings).

use aterm_hash::{FxHashMap, FxHashSet};
use std::num::NonZeroU16;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Settings for [`Font::from_bytes`].
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct FontSettings {
    /// Which face to take out of a TrueType/OpenType collection (`.ttc`).
    pub collection_index: u32,
}

/// Layout information for one glyph at one size.
#[derive(Copy, Clone, PartialEq, Debug, Default)]
pub struct Metrics {
    /// Whole-pixel offset of the bitmap's left edge from the pen.
    pub xmin: i32,
    /// Whole-pixel offset of the bitmap's bottom edge from the baseline, negative below it.
    pub ymin: i32,
    /// Bitmap width, in whole pixels.
    pub width: usize,
    /// Bitmap height, in whole pixels.
    pub height: usize,
    /// Horizontal advance, in fractional pixels.
    pub advance_width: f32,
}

/// Line positioning for a face at one size.
#[derive(Copy, Clone, PartialEq, Debug, Default)]
pub struct LineMetrics {
    /// Above the baseline, positive.
    pub ascent: f32,
    /// Below the baseline, negative.
    pub descent: f32,
    /// Designer's recommended gap between one line's descent and the next line's ascent.
    pub line_gap: f32,
    /// `ascent - descent + line_gap`, summed in design units before scaling.
    pub new_line_size: f32,
}

impl LineMetrics {
    /// Build from raw `hhea`/`OS/2` design units, computing `new_line_size` in
    /// `i32` first so a tall face cannot overflow `i16`.
    #[inline]
    const fn new(ascent: i16, descent: i16, line_gap: i16) -> Self {
        let (a, d, g) = (ascent as i32, descent as i32, line_gap as i32);
        Self {
            ascent: a as f32,
            descent: d as f32,
            line_gap: g as f32,
            new_line_size: (a - d + g) as f32,
        }
    }

    /// Scale design units to pixels.
    #[inline]
    const fn scale(&self, scale: f32) -> Self {
        Self {
            ascent: self.ascent * scale,
            descent: self.descent * scale,
            line_gap: self.line_gap * scale,
            new_line_size: self.new_line_size * scale,
        }
    }
}

/// The font file, however the caller holds it.
#[derive(Clone)]
pub enum FaceBytes {
    /// The styled / shared-face store's handle.
    Slice(Arc<[u8]>),
    /// The host-injection intern's handle, and the copy arm of discovery.
    Vec(Arc<Vec<u8>>),
    /// The mapping arm of discovery (`font_file::admit_font_file`).
    Mapped(Arc<crate::font_file::MappedFontFile>),
    /// A face compiled into the binary (`include_bytes!`, `crate::bundled`).
    Static(&'static [u8]),
}

/// A discovery index's non-owning key.
pub(crate) enum WeakFaceBytes {
    Slice(std::sync::Weak<[u8]>),
    Vec(std::sync::Weak<Vec<u8>>),
    Mapped(std::sync::Weak<crate::font_file::MappedFontFile>),
    Static(&'static [u8]),
}

impl WeakFaceBytes {
    #[inline]
    pub(crate) fn upgrade(&self) -> Option<FaceBytes> {
        match self {
            Self::Slice(bytes) => bytes.upgrade().map(FaceBytes::Slice),
            Self::Vec(bytes) => bytes.upgrade().map(FaceBytes::Vec),
            Self::Mapped(bytes) => bytes.upgrade().map(FaceBytes::Mapped),
            Self::Static(bytes) => Some(FaceBytes::Static(bytes)),
        }
    }

    #[inline]
    pub(crate) fn is_live(&self) -> bool {
        match self {
            Self::Slice(bytes) => bytes.strong_count() != 0,
            Self::Vec(bytes) => bytes.strong_count() != 0,
            Self::Mapped(bytes) => bytes.strong_count() != 0,
            Self::Static(_) => true,
        }
    }
}

impl FaceBytes {
    #[inline]
    pub(crate) fn downgrade(&self) -> WeakFaceBytes {
        match self {
            Self::Slice(bytes) => WeakFaceBytes::Slice(Arc::downgrade(bytes)),
            Self::Vec(bytes) => WeakFaceBytes::Vec(Arc::downgrade(bytes)),
            Self::Mapped(bytes) => WeakFaceBytes::Mapped(Arc::downgrade(bytes)),
            Self::Static(bytes) => WeakFaceBytes::Static(bytes),
        }
    }

    /// Whether both handles are the same allocation or mapping.
    #[inline]
    #[must_use]
    pub fn ptr_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (FaceBytes::Slice(a), FaceBytes::Slice(b)) => Arc::ptr_eq(a, b),
            (FaceBytes::Vec(a), FaceBytes::Vec(b)) => Arc::ptr_eq(a, b),
            (FaceBytes::Mapped(a), FaceBytes::Mapped(b)) => Arc::ptr_eq(a, b),
            (FaceBytes::Static(a), FaceBytes::Static(b)) => {
                a.as_ptr() == b.as_ptr() && a.len() == b.len()
            }
            _ => false,
        }
    }

    /// Whether both handles hold the same bytes.
    #[inline]
    #[must_use]
    pub fn same_bytes(&self, other: &Self) -> bool {
        if self.ptr_eq(other) {
            return true;
        }
        if let (FaceBytes::Mapped(a), FaceBytes::Mapped(b)) = (self, other)
            && a.same_file(b)
        {
            return true;
        }
        self.len() == other.len() && self[..] == other[..]
    }

    /// Whether the bytes are a file mapping (file-backed, evictable, shared).
    #[inline]
    #[must_use]
    pub const fn is_mapped(&self) -> bool {
        matches!(self, FaceBytes::Mapped(_))
    }

    /// Whether the bytes are anonymous heap this process owns, as opposed to file-backed pages.
    #[inline]
    #[must_use]
    pub const fn is_heap(&self) -> bool {
        matches!(self, FaceBytes::Slice(_) | FaceBytes::Vec(_))
    }
}

impl From<Arc<[u8]>> for FaceBytes {
    fn from(bytes: Arc<[u8]>) -> Self {
        FaceBytes::Slice(bytes)
    }
}

impl From<Arc<Vec<u8>>> for FaceBytes {
    fn from(bytes: Arc<Vec<u8>>) -> Self {
        FaceBytes::Vec(bytes)
    }
}

impl core::ops::Deref for FaceBytes {
    type Target = [u8];
    #[inline]
    fn deref(&self) -> &[u8] {
        match self {
            FaceBytes::Slice(b) => b,
            FaceBytes::Vec(b) => b,
            FaceBytes::Mapped(b) => b,
            FaceBytes::Static(b) => b,
        }
    }
}

impl core::fmt::Debug for FaceBytes {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let arm = match self {
            FaceBytes::Slice(_) => "Slice",
            FaceBytes::Vec(_) => "Vec",
            FaceBytes::Mapped(_) => "Mapped",
            FaceBytes::Static(_) => "Static",
        };
        f.debug_struct("FaceBytes")
            .field("arm", &arm)
            .field("len", &self.len())
            .finish()
    }
}

/// A parsed font face.
#[derive(Clone)]
pub struct Font {
    /// The font file, shared.
    data: FaceBytes,
    /// Which face of a collection [`Self::data`] holds.
    index: u32,
    /// `units_per_em`, as `f32` because every use divides by it.
    upem: f32,
    /// `maxp` glyph count.
    glyph_count: u16,
    /// Every cmap subtable enumerated, non-zero mappings only, later subtables overwriting
    /// earlier ones.
    char_to_glyph: FxHashMap<char, NonZeroU16>,
    /// Where [`Font::unicode_glyph_index`] answers differently from [`Self::char_to_glyph`].
    unicode_overrides: Box<[(char, u16)]>,
    /// Each glyph's advance and, for a `glyf` face, ink box, kept once asked for.
    memo: Arc<GlyphMemo>,
    /// Unscaled `hhea`/`OS/2` line metrics, `None` only if the face declares none.
    line_metrics: Option<LineMetrics>,
    /// The legacy `kern` table, copied out (kilobytes) so a pair lookup parses only it.
    kern: Option<Box<[u8]>>,
}

impl core::fmt::Debug for Font {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Font")
            .field("index", &self.index)
            .field("upem", &self.upem)
            .field("glyph_count", &self.glyph_count)
            .field("bytes", &self.data.len())
            .finish()
    }
}

impl Font {
    /// The face's source bytes, through which a weak parsed-face index recovers its key.
    #[inline]
    pub(crate) fn source_bytes(&self) -> &FaceBytes {
        &self.data
    }

    /// Parse a face out of `data`.
    pub fn from_bytes<D: core::ops::Deref<Target = [u8]>>(
        data: D,
        settings: FontSettings,
    ) -> Result<Font, &'static str> {
        Self::parse(FaceBytes::Slice(Arc::from(&*data)), settings)
    }

    /// [`Font::from_bytes`] adopting a caller's `Arc<[u8]>`.
    pub fn from_shared_slice(
        data: Arc<[u8]>,
        settings: FontSettings,
    ) -> Result<Font, &'static str> {
        Self::parse(FaceBytes::Slice(data), settings)
    }

    /// [`Font::from_bytes`] adopting a caller's `Arc<Vec<u8>>`.
    pub fn from_shared_vec(
        data: Arc<Vec<u8>>,
        settings: FontSettings,
    ) -> Result<Font, &'static str> {
        Self::parse(FaceBytes::Vec(data), settings)
    }

    /// [`Font::from_bytes`] adopting any [`FaceBytes`], including a file mapping.
    pub fn from_shared(data: FaceBytes, settings: FontSettings) -> Result<Font, &'static str> {
        Self::parse(data, settings)
    }

    /// The one parse all four constructors reach.
    fn parse(data: FaceBytes, settings: FontSettings) -> Result<Font, &'static str> {
        let index = settings.collection_index;
        let face = aterm_font::Face::parse(&data, index).map_err(describe)?;
        let upem = f32::from(face.units_per_em());
        if upem <= 0.0 {
            return Err("The head table declares a zero or malformed units_per_em.");
        }
        let glyph_count = face.number_of_glyphs();

        let mut char_to_glyph: FxHashMap<char, NonZeroU16> = FxHashMap::default();
        let mut contested: FxHashSet<char> = FxHashSet::default();
        if let Some(cmap) = face.tables().cmap {
            let has_unicode = cmap.subtables.into_iter().any(|s| s.is_unicode());
            for subtable in cmap.subtables {
                let legacy = has_unicode && !subtable.is_unicode();
                subtable.codepoints(|cp| {
                    let Some(ch) = char::from_u32(cp) else {
                        return;
                    };
                    if let Some(gid) = subtable.glyph_index(cp)
                        && let Some(nz) = NonZeroU16::new(gid.0)
                        && nz.get() < glyph_count
                    {
                        let replaced = char_to_glyph.insert(ch, nz);
                        if legacy || (has_unicode && replaced.is_some_and(|old| old != nz)) {
                            contested.insert(ch);
                        }
                    }
                });
            }
        }
        let unicode_overrides = unicode_overrides(&face, &char_to_glyph, contested, glyph_count);

        let memo = Arc::new(GlyphMemo::new(glyph_count, face.tables().glyf.is_some()));

        let line_metrics = Some(LineMetrics::new(
            face.ascender(),
            face.descender(),
            face.line_gap(),
        ));

        let kern = face
            .raw_face()
            .table(aterm_font::Tag::from_bytes(b"kern"))
            .map(Box::<[u8]>::from);

        Ok(Font {
            data,
            index,
            upem,
            glyph_count,
            char_to_glyph,
            unicode_overrides,
            memo,
            line_metrics,
            kern,
        })
    }

    /// The glyph id `ch` maps to, or `0` (`.notdef`).
    #[inline]
    #[must_use]
    pub fn lookup_glyph_index(&self, ch: char) -> u16 {
        self.char_to_glyph.get(&ch).map_or(0, |g| g.get())
    }

    /// The glyph id the face's Unicode cmap subtables map `ch` to.
    #[inline]
    #[must_use]
    pub fn unicode_glyph_index(&self, ch: char) -> u16 {
        self.unicode_overrides
            .binary_search_by_key(&ch, |&(point, _)| point)
            .ok()
            .and_then(|at| self.unicode_overrides.get(at))
            .map_or_else(|| self.lookup_glyph_index(ch), |&(_, gid)| gid)
    }

    /// Design-units → pixels at `px`.
    #[inline]
    #[must_use]
    pub fn scale_factor(&self, px: f32) -> f32 {
        px / self.upem
    }

    /// Line metrics scaled to `px`.
    #[inline]
    #[must_use]
    pub fn horizontal_line_metrics(&self, px: f32) -> Option<LineMetrics> {
        Some(self.line_metrics?.scale(self.scale_factor(px)))
    }

    /// Legacy `kern`-table adjustment for `left`→`right` at `px`.
    #[must_use]
    pub fn horizontal_kern(&self, left: char, right: char, px: f32) -> Option<f32> {
        self.horizontal_kern_indexed(
            self.unicode_glyph_index(left),
            self.unicode_glyph_index(right),
            px,
        )
    }

    /// [`Font::horizontal_kern`] by glyph id.
    #[must_use]
    pub fn horizontal_kern_indexed(&self, left: u16, right: u16, px: f32) -> Option<f32> {
        let table = aterm_font::kern::Table::parse(self.kern.as_deref()?)?;
        let (l, r) = (aterm_font::GlyphId(left), aterm_font::GlyphId(right));
        let value = table
            .subtables
            .into_iter()
            .filter(|s| s.horizontal && !s.variable)
            .find_map(|s| s.glyphs_kerning(l, r))?;
        Some(f32::from(value) * self.scale_factor(px))
    }

    /// Layout metrics for the Unicode glyph of `ch` at `px`.
    #[inline]
    #[must_use]
    pub fn metrics(&self, ch: char, px: f32) -> Metrics {
        self.metrics_indexed(self.unicode_glyph_index(ch), px)
    }

    /// [`Font::metrics`] by glyph id.
    #[must_use]
    pub fn metrics_indexed(&self, gid: u16, px: f32) -> Metrics {
        let advance_width = self.advance_px(gid, px);
        let scale = self.scale_factor(px);
        let Some(bbox) = self.design_bbox(gid) else {
            return Metrics {
                advance_width,
                ..Metrics::default()
            };
        };
        let (xmin, ymin, w, h) = ink_box_px(bbox, scale);
        Metrics {
            xmin,
            ymin,
            width: w,
            height: h,
            advance_width,
        }
    }

    /// Rasterize the Unicode glyph of `ch` at `px`, including character-addressed
    /// synthetic bold and italic runs.
    #[inline]
    #[must_use]
    pub fn rasterize(&self, ch: char, px: f32) -> (Metrics, Vec<u8>) {
        self.rasterize_indexed(self.unicode_glyph_index(ch), px)
    }

    /// [`Font::rasterize`] by glyph id.
    #[inline]
    #[must_use]
    pub fn rasterize_indexed(&self, gid: u16, px: f32) -> (Metrics, Vec<u8>) {
        self.rasterize_indexed_at(gid, px, 0.0)
    }

    /// [`Font::rasterize_indexed`] for a glyph whose pen sits `dx` px (a fraction, `0.0..1.0`).
    #[must_use]
    pub fn rasterize_indexed_at(&self, gid: u16, px: f32, dx: f32) -> (Metrics, Vec<u8>) {
        if px.is_nan() || px <= 0.0 || gid >= self.glyph_count {
            return (Metrics::default(), Vec::new());
        }
        let Ok(face) = aterm_font::Face::parse(&self.data, self.index) else {
            return (Metrics::default(), Vec::new());
        };
        match crate::variation::varied_glyph_raster_with_face_at(&face, gid, px, dx) {
            Some((width, height, xmin, ymin, advance_width, cov)) => (
                Metrics {
                    xmin,
                    ymin,
                    width,
                    height,
                    advance_width,
                },
                cov,
            ),
            None => (
                Metrics {
                    advance_width: self.advance_px(gid, px),
                    ..Metrics::default()
                },
                Vec::new(),
            ),
        }
    }

    /// Scaled `hmtx` advance for `gid` (`scale * units`), the number the grid is built from.
    #[inline]
    fn advance_px(&self, gid: u16, px: f32) -> f32 {
        let units = self.memo.advance(gid, || {
            let face = aterm_font::Face::parse(&self.data, self.index).ok()?;
            face.glyph_hor_advance(aterm_font::GlyphId(gid))
        });
        self.scale_factor(px) * f32::from(units)
    }

    /// Design-space ink box for `gid`: kept once outlined for a `glyf` face,
    /// otherwise a face parse per call (the CFF path).
    fn design_bbox(&self, gid: u16) -> Option<[i16; 4]> {
        if gid >= self.glyph_count {
            return None;
        }
        let outline = || {
            let face = aterm_font::Face::parse(&self.data, self.index).ok()?;
            face.glyph_bounding_box(aterm_font::GlyphId(gid))
                .map(|b| [b.x_min, b.y_min, b.x_max, b.y_max])
        };
        if self.memo.keeps_boxes() {
            let b = self.memo.ink_box(gid, outline);
            return (b != [0, 0, 0, 0]).then_some(b);
        }
        outline()
    }
}

/// What a [`Font`] answers per glyph, worked out the first time each glyph
/// is asked for and kept: its `hmtx` advance (with `HVAR` or `gvar` deltas)
/// and, for a `glyf` face, its ink box. Working out every glyph's at parse
/// cost an outline per glyph id (and a variation walk per glyph of a variable
/// face) before the first character drew, for glyphs that never draw.
struct GlyphMemo {
    /// Per glyph: the advance in the low 16 bits, [`Self::ADVANCE_KNOWN`]
    /// once it is there, [`Self::BOX_KNOWN`] once `boxes` holds the box.
    words: Box<[AtomicU32]>,
    /// Per glyph, the ink box's four `i16`s, for a `glyf` face; empty for
    /// any other, whose box is outlined per call.
    boxes: Box<[AtomicU64]>,
}

impl GlyphMemo {
    const ADVANCE_KNOWN: u32 = 1 << 16;
    const BOX_KNOWN: u32 = 1 << 17;

    fn new(glyph_count: u16, keeps_boxes: bool) -> Self {
        let count = usize::from(glyph_count);
        Self {
            words: (0..count).map(|_| AtomicU32::new(0)).collect(),
            boxes: (0..if keeps_boxes { count } else { 0 })
                .map(|_| AtomicU64::new(0))
                .collect(),
        }
    }

    fn keeps_boxes(&self) -> bool {
        !self.boxes.is_empty()
    }

    /// `gid`'s advance in design units, `0` when the face has none (or past
    /// the glyph count); `compute` runs the first time it is asked for.
    fn advance(&self, gid: u16, compute: impl FnOnce() -> Option<u16>) -> u16 {
        let Some(word) = self.words.get(usize::from(gid)) else {
            return 0;
        };
        let known = word.load(Ordering::Acquire);
        if known & Self::ADVANCE_KNOWN != 0 {
            return known as u16;
        }
        let advance = compute().unwrap_or(0);
        // A racing thread computes the same advance, so the bits agree.
        word.fetch_or(Self::ADVANCE_KNOWN | u32::from(advance), Ordering::Release);
        advance
    }

    /// `gid`'s ink box, `[0; 4]` when it has none; `compute` runs the first
    /// time it is asked for.
    fn ink_box(&self, gid: u16, compute: impl FnOnce() -> Option<[i16; 4]>) -> [i16; 4] {
        let (Some(word), Some(packed)) = (
            self.words.get(usize::from(gid)),
            self.boxes.get(usize::from(gid)),
        ) else {
            return [0; 4];
        };
        if word.load(Ordering::Acquire) & Self::BOX_KNOWN != 0 {
            let bits = packed.load(Ordering::Relaxed).to_le_bytes();
            let (pairs, _) = bits.as_chunks::<2>();
            return core::array::from_fn(|i| {
                pairs.get(i).map_or(0, |&pair| i16::from_le_bytes(pair))
            });
        }
        let ink = compute().unwrap_or([0; 4]);
        let mut bits = [0u8; 8];
        for (pair, v) in bits.as_chunks_mut::<2>().0.iter_mut().zip(ink) {
            *pair = v.to_le_bytes();
        }
        // Stored before the flag is released, so a reader that sees the flag sees it.
        packed.store(u64::from_le_bytes(bits), Ordering::Relaxed);
        word.fetch_or(Self::BOX_KNOWN, Ordering::Release);
        ink
    }
}

/// The whole-pixel bitmap box `(xmin, ymin, width, height)` for a design-space ink box at
/// `scale`: the same expression [`crate::variation::varied_glyph_raster_with_face`] sizes
/// its coverage grid with, so [`Font::metrics`] and [`Font::rasterize`] agree on the box.
#[inline]
fn ink_box_px(bbox: [i16; 4], scale: f32) -> (i32, i32, usize, usize) {
    let x_min = (f32::from(bbox[0]) * scale).floor();
    let y_min = (f32::from(bbox[1]) * scale).floor();
    let x_max = (f32::from(bbox[2]) * scale).ceil();
    let y_max = (f32::from(bbox[3]) * scale).ceil();
    let w = (x_max - x_min).max(0.0) as usize;
    let h = (y_max - y_min).max(0.0) as usize;
    (x_min as i32, y_min as i32, w, h)
}

/// [`Font::unicode_overrides`] for a face whose enumerated map is
/// `char_to_glyph`: of the `contested` code points, each whose Unicode answer
/// ([`Font::unicode_glyph_index`]) is not the enumerated one, with that answer.
fn unicode_overrides(
    face: &aterm_font::Face<'_>,
    char_to_glyph: &FxHashMap<char, NonZeroU16>,
    contested: FxHashSet<char>,
    glyph_count: u16,
) -> Box<[(char, u16)]> {
    let mut overrides = Vec::new();
    let Some(cmap) = face.tables().cmap else {
        return overrides.into();
    };
    if contested.is_empty() {
        return overrides.into();
    }
    let mut contested: Vec<char> = contested.into_iter().collect();
    contested.sort_unstable();
    let unicode: Vec<aterm_font::cmap::Subtable<'_>> = cmap
        .subtables
        .into_iter()
        .filter(|s| s.is_unicode())
        .collect();
    for ch in contested {
        let answer = unicode
            .iter()
            .find_map(|s| {
                s.glyph_index(u32::from(ch))
                    .map(|g| g.0)
                    .filter(|&g| g != 0 && g < glyph_count)
            })
            .unwrap_or(0);
        if answer != char_to_glyph.get(&ch).map_or(0, |g| g.get()) {
            overrides.push((ch, answer));
        }
    }
    overrides.into()
}

/// `bytes`, a single-face font file.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn with_cmap_record_last(bytes: &[u8], platform: u16, encoding: u16) -> Option<Vec<u8>> {
    let u16_at = |at: usize| -> Option<usize> {
        let field: [u8; 2] = bytes.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(usize::from(u16::from_be_bytes(field)))
    };
    let u32_at = |at: usize| -> Option<usize> {
        let field: [u8; 4] = bytes.get(at..at.checked_add(4)?)?.try_into().ok()?;
        usize::try_from(u32::from_be_bytes(field)).ok()
    };
    let cmap_record = (0..u16_at(4)?)
        .map(|i| 12 + 16 * i)
        .find(|&record| bytes.get(record..record + 4) == Some(b"cmap".as_slice()))?;
    let cmap = u32_at(cmap_record + 8)?;
    let count = u16_at(cmap.checked_add(2)?)?;
    let first = cmap + 4;
    let found = (0..count).find(|&i| {
        u16_at(first + 8 * i) == Some(usize::from(platform))
            && u16_at(first + 8 * i + 2) == Some(usize::from(encoding))
    })?;
    let mut moved = bytes.to_vec();
    moved
        .get_mut(first + 8 * found..first + 8 * count)?
        .rotate_left(8);
    Some(moved)
}

/// aterm-font's parse failure, in ttf-parser's wording, as the `&'static str` the call
/// sites format.
fn describe(error: aterm_font::FaceParsingError) -> &'static str {
    use aterm_font::FaceParsingError::*;
    match error {
        MalformedFont => "An attempt to read out of bounds detected.",
        UnknownMagic => {
            "Face data must start with 0x00010000, 0x74727565, 0x4F54544F or 0x74746366."
        }
        FaceIndexOutOfBounds => "The face index is larger than the number of faces in the font.",
        NoHeadTable => "The head table is missing or malformed.",
        NoHheaTable => "The hhea table is missing or malformed.",
        NoMaxpTable => "The maxp table is missing or malformed.",
    }
}

#[cfg(test)]
#[path = "font_tests.rs"]
mod tests;
