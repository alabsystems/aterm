// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The first-party font face.
//!
//! A [`Font`] holds a face's cmap, horizontal advances, line metrics and legacy `kern`
//! pairs, read once with `aterm-font`. Rasterization is
//! [`crate::variation::varied_glyph_raster_with_face`]: aterm-font outlines into the
//! [`crate::raster`] signed-area coverage fill, flattened at the size drawn.
//!
//! Behaviour the grid and face routing depend on:
//!
//! The API is deliberately the SEVEN methods the workspace actually called on
//! `fontdue::Font` — [`Font::from_bytes`], [`Font::rasterize`],
//! [`Font::rasterize_indexed`], [`Font::lookup_glyph_index`],
//! [`Font::horizontal_line_metrics`], [`Font::horizontal_kern`],
//! [`Font::metrics`] — plus [`Metrics`], [`LineMetrics`] and [`FontSettings`].
//! Nothing was added "for completeness": an unused method is an unmeasured
//! method. One was added since, because a caller drew wrong without it:
//! [`Font::unicode_glyph_index`], the answer of the face's UNICODE subtables
//! (see `lookup_glyph_index` below).
//!
//! # Where it is deliberately IDENTICAL to fontdue, and where it is not
//!
//! IDENTICAL, because changing them would move pixels or move the terminal grid:
//!
//! * **Advance widths.** `scale * hmtx_advance`, with `scale = px / upem` — the
//!   same two multiplications, in the same order, off the same `u16`. `cell_w`
//!   is an advance, so this is the number the whole grid is built on. Measured
//!   bit-identical on 119 460 / 119 460 glyphs across four faces.
//! * **Bitmap metrics.** `xmin = floor(x_min·s)`, `ymin = floor(y_min·s)`,
//!   `width = ceil(x_max·s) − xmin`, `height = ceil(y_max·s) − ymin`. fontdue
//!   spells this as `ceil(bounds.width + fract(bounds.xmin))` with a matching
//!   `offset_y` correction; the two are the same integers (`ceil(a − ⌊b⌋)
//!   = ⌈a⌉ − ⌊b⌋` for integral `⌊b⌋`), and [`crate::variation`] already used
//!   this spelling.
//! * **`lookup_glyph_index`.** fontdue's map is built by ENUMERATING every cmap
//!   subtable and inserting the non-zero mappings, a later subtable overwriting
//!   an earlier one — Mac-Roman `(1,0)` subtables included. This reproduces that
//!   exactly, deliberately, because the styled/fallback ROUTING lattice is
//!   proven against that answer and `StyledFace::unicode_gid` is the machinery
//!   that corrects it downstream. Retiring a dependency is not a licence to move
//!   which face a cell draws from. (`crate::fontdue_cmap_covers`, the
//!   parse-free predicate for the deferred tier, predicts exactly this map — so
//!   the two stay consistent by construction.)
//!
//!   That answer is WRONG as a glyph choice wherever a legacy subtable is
//!   listed after the Unicode one. Monaco's cmap is `(0,3)`, then seven Mac
//!   Roman `(1,0)`, Mac Cyrillic `(1,7)` and Mac Central European `(1,29)`, so
//!   94 of the 96 printable Latin-1 characters U+00A0–U+00FF map to the glyph
//!   of the Mac byte with that value: `·` U+00B7 to `∑`, `×` U+00D7 to `◊`.
//!   [`Font::unicode_glyph_index`] is the answer of the Unicode subtables, the
//!   way CoreText, FreeType's charmap selection and HarfBuzz choose. The grid
//!   draws its primary face by that answer (`Renderer::primary_unicode_gid`),
//!   and so must any other text that turns a char into a glyph outside the
//!   routing lattice — the chrome drew `·` as `∑` through this map.
//! * **`px <= 0`** returns `(Metrics::default(), Vec::new())`, and a glyph with
//!   no outline (space) returns a 0×0 raster carrying only the advance.
//!
//! DIFFERENT, on purpose, with the reason:
//!
//! * **An out-of-range glyph id returns an empty raster instead of panicking.**
//!   fontdue indexes `self.glyphs[index as usize]` and panics on an id past the
//!   end; this crate takes glyph ids from shaping, from other faces' cmaps and
//!   from `sbix`/`COLR` records, so "the caller asked for a glyph this face does
//!   not have" is a routing outcome, not a bug worth a crash.
//! * **The ink box comes from the face's DECLARED `glyph_bounding_box`**, not
//!   from the observed extrema of a pre-flattened outline. fontdue flattens
//!   every glyph ONCE at parse time against `FontSettings::scale = 40`, which
//!   is both why its parse costs ~150 ms on a broad face and why its accuracy
//!   DEGRADES as the render size rises above that scale. Here the outline is
//!   flattened at the size it is drawn at.
//!
//!   A declared box can be LOOSER than the outline it declares, so this box is
//!   sometimes bigger than fontdue's — and it is bigger in the SAFE direction:
//!   it can cost atlas space, never a clipped glyph.
//!   Measured over 109 704
//!   (glyph, size) pairs of the embedded faces: 102 425 boxes are identical to
//!   fontdue's and every other one CONTAINS it — so no ink is lost and nothing
//!   moves. The worst slack is 9 px (a Nerd Font icon at 32 px whose declared
//!   box is generous), and the ring the bigger box adds carries 29/255 of
//!   coverage in TOTAL across all 109 704 rasters (22 texels, heaviest 4/255):
//!   the sliver between fontdue's once-at-parse chords and the real curve, which
//!   the finer first-party flattening reaches and fontdue's box stops short of.
//!
//!   The one place the slack is visible: `fallback_fit_scale` shrinks a
//!   proportional fallback raster to fit its cell by reading the ink box, so a
//!   looser box fits slightly smaller. Measured over 13 713 glyphs, the mean
//!   shrink against fontdue is 0.0037 and the worst is 0.19 — one glyph,
//!   DejaVu's U+10FA, whose declared box runs 30% past its outline. That test
//!   holds both numbers.
//! * **Coverage is the first-party fill, and it is now the more accurate of the
//!   two.** Measured against an exact analytic reference over 2 108 scored
//!   glyph rasters (of 2 320 swept; 212 excluded as genuinely self-overlapping)
//!   from both embedded faces at 8..32 px, this path's mean |error| is
//!   **0.075/255** (worst cell 4.4) against fontdue's **0.261/255** (worst cell
//!   21.3), which is why the two masks still differ: 0.37/255 mean over 30.0 M
//!   shared texels, 42/255 on the single worst texel. It was the other way
//!   round — 0.810/255, 3.2× WORSE than fontdue — until
//!   `raster::FLATTEN_SAGITTA_PX` replaced the flattening budget `raster.rs`
//!   had inherited from `ab_glyph_rasterizer` with one derived from the 8-bit
//!   mask. Either way the mask delta is a few LSBs on antialiased edges, and it
//!   is why the handful of tests that pinned BYTES to fontdue were re-baselined
//!   onto this face rather than kept.
//! * **No GSUB pre-load, no `name`, no vertical metrics, no `chars()` map
//!   handed out, no `file_hash`.** Nothing called them. Shaping substitutions
//!   come from `aterm_font::shape` (`crate::ligature_shaping`), which reads GSUB
//!   itself.
//!
//!   One consequence, measured and pinned: fontdue builds geometry only for the
//!   glyphs its cmap and GSUB reach and reports advance `0` for every other
//!   glyph id. This face reads `hmtx` for all of them, so it reports a real
//!   advance where fontdue reported zero — 48 glyphs of DejaVu. On every
//!   ADDRESSABLE id the advances are bit-identical: 109 904 of 109 904.
//!
//! # Cost
//!
//! Parsing is the cmap walk plus one `hmtx` read per glyph plus the `kern`
//! header — no outline is touched. MEASURED against fontdue on the same bytes at
//! opt-level 3, live heap and wall time:
//!
//! ```text
//!                                     adopting   copying    fontdue        ours     fontdue
//!   DejaVu Sans Mono (embedded)           68 kB     404 kB    9,118 kB    1.08 ms    6.14 ms
//!   Symbols Nerd Font Mono (embedded)    245 kB   2,694 kB   56,756 kB    5.69 ms   33.93 ms
//!   Apple Symbols.ttf                    114 kB     991 kB   12,023 kB    1.68 ms    7.30 ms
//!   Arial Unicode.ttf         (22 MB)  1,067 kB  23,800 kB  260,691 kB   26.92 ms  148.45 ms
//!   STHeiti Light.ttc         (53 MB)  1,086 kB  55,562 kB  323,015 kB   61.25 ms  201.61 ms
//! ```
//!
//! "adopting" is [`Font::from_shared_slice`] / [`Font::from_shared_vec`], which
//! every byte store in this crate uses; "copying" is [`Font::from_bytes`], whose
//! extra megabytes ARE the file. So a resident broad face costs about a
//! megabyte of derived tables where fontdue's cost 300, and that megabyte is
//! what the deferred-parse machinery (`LazyFontdue`) now defers.
//!
//! [`Font::unicode_glyph_index`]'s override table came after that table was
//! measured, and it is the one derived table that depends on the cmap's
//! legacy subtables. MEASURED: none for Apple Symbols or Arial Unicode, 5
//! entries for Symbols Nerd Font Mono and 37 for DejaVu Sans Mono (control
//! codes their Mac Roman subtables map and their Unicode ones do not), 129 for
//! Monaco (1 kB), and 12 280 for STHeiti Light (98 kB), whose Macintosh
//! Chinese subtable enumerates its two-byte codes as code points. Building it
//! is one Unicode lookup per code point a legacy subtable maps: about 25 µs
//! on Monaco and under 1 ms on STHeiti Light in an unoptimised build.
//!
//! The FILE is retained, unlike fontdue's — which kept converted geometry and
//! not one byte of the file — so the constructors come in three shapes and only
//! one of them copies: [`Font::from_shared_slice`] and [`Font::from_shared_vec`]
//! ADOPT a caller's `Arc`, and every byte store in this crate hands its own
//! handle over. That distinction is worth 54 MB on one face (see the table
//! above), which is why it exists rather than a single `&[u8]` constructor.
//!
//! The face is NOT retained between calls: `aterm_font::Face<'a>` borrows its
//! bytes, and a self-referential owner would need `unsafe` to express. Instead
//! every field a hot path reads — the cmap map, the advance table, the line
//! metrics, the `kern` bytes and (for `glyf` faces) the per-glyph ink box — is
//! computed at parse time, so the only calls that re-parse are the ones that
//! need an OUTLINE ([`Font::rasterize`], and [`Font::metrics`] on a CFF face).
//! Those are exactly the calls every consumer in the workspace already serves
//! from a glyph cache.

use aterm_hash::{FxHashMap, FxHashSet};
use std::num::NonZeroU16;
use std::sync::Arc;

/// Settings for [`Font::from_bytes`].
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct FontSettings {
    /// Which face to take out of a TrueType/OpenType collection (`.ttc`).
    /// `0` for a plain single-face file.
    pub collection_index: u32,
}

/// Layout information for one glyph at one size.
#[derive(Copy, Clone, PartialEq, Debug, Default)]
pub struct Metrics {
    /// Whole-pixel offset of the bitmap's left edge from the pen, negative when
    /// the glyph reaches left of the origin.
    pub xmin: i32,
    /// Whole-pixel offset of the bitmap's bottom edge from the baseline,
    /// negative below it.
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
    /// Designer's recommended gap between one line's descent and the next
    /// line's ascent.
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

/// The font file, however the caller holds it. The face reads outlines on demand, so it
/// keeps the file, and adopting the caller's handle avoids copying it.
///
/// The arms are the crate's handle types: `Arc<[u8]>` (styled and shared-face stores),
/// `Arc<Vec<u8>>` (host injection), and a read-only private mapping
/// ([`crate::font_file::MappedFontFile`]) for discovered files on the sealed system font
/// volume, so a 192 MB emoji collection is faulted in per glyph instead of copied. One
/// `Arc<dyn AsRef<[u8]>>` cannot unify them: `Arc<[u8]>` is already unsized. Every holder
/// of discovered bytes (`FallbackFace`, the colour-emoji slot, `DISCOVERED_FONT_BYTES`,
/// `PARSED_FONT_INTERN`) carries this, so a file is held once however it was admitted.
#[derive(Clone)]
pub enum FaceBytes {
    /// The styled / shared-face store's handle.
    Slice(Arc<[u8]>),
    /// The host-injection intern's handle, and the copy arm of discovery.
    Vec(Arc<Vec<u8>>),
    /// The mapping arm of discovery (`font_file::admit_font_file`).
    Mapped(Arc<crate::font_file::MappedFontFile>),
    /// A face compiled into the binary (`include_bytes!`, `crate::bundled`): the
    /// executable's read-only pages, faulted in per glyph, no anonymous heap.
    Static(&'static [u8]),
}

/// A discovery index's non-owning key; dead entries are pruned on the next lookup. A weak
/// slice keeps its `Arc` allocation until then, unlike the Vec and mapping arms.
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

    /// Whether both handles are the same allocation or mapping: the O(1) identity the
    /// pointer-keyed caches rely on.
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

    /// Whether both handles hold the same bytes, answered by identity where it can (the
    /// same handle, or two mappings of one inode) and by comparison only otherwise.
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

    /// Whether the bytes are a file mapping (file-backed, evictable, shared). Diagnostics.
    #[inline]
    #[must_use]
    pub const fn is_mapped(&self) -> bool {
        matches!(self, FaceBytes::Mapped(_))
    }

    /// Whether the bytes are anonymous heap this process owns, as opposed to file-backed
    /// pages (a mapping, or a face compiled into the executable). Diagnostics: the seal's
    /// residency split (`AdmittedFontSources::discovered_residency`).
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

/// A parsed font face. A clone copies the derived tables, never the file (an [`Arc`]).
#[derive(Clone)]
pub struct Font {
    /// The font file, shared. Kept because outlines are read on demand.
    data: FaceBytes,
    /// Which face of a collection [`Self::data`] holds.
    index: u32,
    /// `units_per_em`, as `f32` because every use divides by it.
    upem: f32,
    /// `maxp` glyph count — the bound `rasterize_indexed` refuses to exceed.
    glyph_count: u16,
    /// Every cmap subtable enumerated, non-zero mappings only, later subtables overwriting
    /// earlier ones.
    char_to_glyph: FxHashMap<char, NonZeroU16>,
    /// Where [`Font::unicode_glyph_index`] answers differently from
    /// [`Self::char_to_glyph`]: each code point a legacy subtable (Macintosh
    /// `(1,*)`, Windows Symbol `(3,0)`, …), or a later Unicode subtable that
    /// disagrees with an earlier one, won in the enumerated map, with the glyph
    /// the face's Unicode subtables map it to (`0`: none), sorted by code
    /// point. Empty for a face that has no legacy subtable and whose Unicode
    /// subtables agree, and for one with no Unicode subtable at all, which
    /// keeps the enumerated answer.
    ///
    /// A sorted slice, not a map, because the big ones are Apple's CJK faces,
    /// whose Macintosh two-byte subtable enumerates its codes as code points
    /// (STHeiti Light: 12 280 entries, 98 kB here against ~150 kB as a map),
    /// and a lookup that misses an empty slice costs nothing.
    unicode_overrides: Box<[(char, u16)]>,
    /// `hmtx` advance per glyph id, in design units. Indexed by gid; a gid past
    /// the end of the table repeats the last entry, which is what
    /// `glyph_hor_advance` already does, so this is a straight copy.
    advances: Box<[u16]>,
    /// Per-glyph ink box in design units `(x_min, y_min, x_max, y_max)`, for `glyf` faces,
    /// where it is a header read; this keeps [`Font::metrics`] allocation- and parse-free.
    /// CFF faces have no such header (`None`), so `metrics` re-parses.
    bboxes: Option<Box<[[i16; 4]]>>,
    /// Unscaled `hhea`/`OS/2` line metrics, `None` only if the face declares
    /// none.
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

    /// Parse a face out of `data`; the error names which table refused.
    ///
    /// This copies `data`. A caller holding the file behind an `Arc` hands it to
    /// [`Font::from_shared_slice`] or [`Font::from_shared_vec`] instead, as every store in
    /// this crate does (53 MB saved on `STHeiti Light.ttc`).
    pub fn from_bytes<D: core::ops::Deref<Target = [u8]>>(
        data: D,
        settings: FontSettings,
    ) -> Result<Font, &'static str> {
        Self::parse(FaceBytes::Slice(Arc::from(&*data)), settings)
    }

    /// [`Font::from_bytes`] adopting a caller's `Arc<[u8]>` (the styled and shared-face
    /// stores' handle): the face costs its derived tables, not a second copy of the file.
    pub fn from_shared_slice(
        data: Arc<[u8]>,
        settings: FontSettings,
    ) -> Result<Font, &'static str> {
        Self::parse(FaceBytes::Slice(data), settings)
    }

    /// [`Font::from_bytes`] adopting a caller's `Arc<Vec<u8>>` (the interned and discovered
    /// stores' handle).
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
        // `units_per_em()` widens a `u16`, so there is no NaN case.
        if upem <= 0.0 {
            return Err("The head table declares a zero or malformed units_per_em.");
        }
        let glyph_count = face.number_of_glyphs();

        // fontdue's map, built fontdue's way: enumerate every subtable (not just
        // the Unicode ones), keep only non-zero mappings, let a later subtable
        // win. `char::from_u32` replaces fontdue's `transmute`, which is the one
        // place this is strictly safer rather than merely equal: a cmap that
        // emits a surrogate code point yields `None` here instead of an invalid
        // `char`, and no `char` a caller can hold is a surrogate anyway.
        //
        // The same walk notes each code point whose winner may not be the
        // Unicode subtables' answer (`unicode_overrides`): one a legacy
        // subtable mapped, or one a subtable re-mapped to a different glyph. A
        // face with no Unicode subtable notes nothing; its legacy map is its
        // answer.
        let mut char_to_glyph: FxHashMap<char, NonZeroU16> = FxHashMap::default();
        // Keep one candidate per scalar while walking: repeated legacy subtables
        // must not multiply temporary storage before the final sort.
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

        // Read `hmtx` once so `metrics(..).advance_width`, the UI text measurer's
        // per-character call, never parses.
        let advances: Box<[u16]> = (0..glyph_count)
            .map(|gid| {
                face.glyph_hor_advance(aterm_font::GlyphId(gid))
                    .unwrap_or(0)
            })
            .collect();

        // Ink boxes, only where they are a header read (see the field docs).
        let bboxes: Option<Box<[[i16; 4]]>> = face.tables().glyf.map(|_| {
            (0..glyph_count)
                .map(|gid| {
                    face.glyph_bounding_box(aterm_font::GlyphId(gid))
                        .map_or([0, 0, 0, 0], |b| [b.x_min, b.y_min, b.x_max, b.y_max])
                })
                .collect()
        });

        // `Face::ascender`/`descender`/`line_gap` apply the `USE_TYPO_METRICS` rule, as
        // `geometry_metrics` does.
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
            advances,
            bboxes,
            line_metrics,
            kern,
        })
    }

    /// The glyph id `ch` maps to, or `0` (`.notdef`). This is the enumerated-cmap answer
    /// (module docs), not `aterm_font::Face::glyph_index`'s Unicode-only one.
    #[inline]
    #[must_use]
    pub fn lookup_glyph_index(&self, ch: char) -> u16 {
        self.char_to_glyph.get(&ch).map_or(0, |g| g.get())
    }

    /// The glyph id the face's UNICODE cmap subtables map `ch` to — platform
    /// 0, and Windows `(3,1)` and `(3,10)` — or `0` (`.notdef`) when they map
    /// it to nothing. The first Unicode subtable, in table order, that maps
    /// `ch` to a glyph the face has wins. A face with no Unicode subtable
    /// answers [`Font::lookup_glyph_index`]: its legacy subtables are all the
    /// map it has.
    ///
    /// This, not [`Font::lookup_glyph_index`], is the glyph a char draws as:
    /// see the module docs for the Macintosh subtables that answer otherwise.
    /// Draw by id with it ([`Font::rasterize_indexed`],
    /// [`Font::metrics_indexed`], [`Font::horizontal_kern_indexed`]).
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

    /// Line metrics scaled to `px`; `None` only when the face declares none.
    #[inline]
    #[must_use]
    pub fn horizontal_line_metrics(&self, px: f32) -> Option<LineMetrics> {
        Some(self.line_metrics?.scale(self.scale_factor(px)))
    }

    /// Legacy `kern`-table adjustment for `left`→`right` at `px`, or `None` without a
    /// `kern` table or pair. Nothing reads GPOS kerning: the run shaper
    /// ([`crate::ligature_shaping`]) positions by advance alone.
    #[must_use]
    pub fn horizontal_kern(&self, left: char, right: char, px: f32) -> Option<f32> {
        self.horizontal_kern_indexed(
            self.lookup_glyph_index(left),
            self.lookup_glyph_index(right),
            px,
        )
    }

    /// [`Font::horizontal_kern`] by glyph id.
    #[must_use]
    pub fn horizontal_kern_indexed(&self, left: u16, right: u16, px: f32) -> Option<f32> {
        let table = aterm_font::kern::Table::parse(self.kern.as_deref()?)?;
        let (l, r) = (aterm_font::GlyphId(left), aterm_font::GlyphId(right));
        // The first horizontal, non-variable subtable with an entry for this pair wins.
        let value = table
            .subtables
            .into_iter()
            .filter(|s| s.horizontal && !s.variable)
            .find_map(|s| s.glyphs_kerning(l, r))?;
        Some(f32::from(value) * self.scale_factor(px))
    }

    /// Layout metrics for `ch` at `px`.
    #[inline]
    #[must_use]
    pub fn metrics(&self, ch: char, px: f32) -> Metrics {
        self.metrics_indexed(self.lookup_glyph_index(ch), px)
    }

    /// [`Font::metrics`] by glyph id. An id past the end of the face is an
    /// empty box with a zero advance, never a panic.
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

    /// Rasterize `ch` at `px`: its [`Metrics`] and a `width * height` coverage
    /// mask, 0 = uncovered, 255 = fully covered, top row first.
    #[inline]
    #[must_use]
    pub fn rasterize(&self, ch: char, px: f32) -> (Metrics, Vec<u8>) {
        self.rasterize_indexed(self.lookup_glyph_index(ch), px)
    }

    /// [`Font::rasterize`] by glyph id. An out-of-range id or a NaN or non-positive `px`
    /// returns an empty mask with default metrics; a glyph with no outline returns an empty
    /// mask with its real advance.
    #[inline]
    #[must_use]
    pub fn rasterize_indexed(&self, gid: u16, px: f32) -> (Metrics, Vec<u8>) {
        self.rasterize_indexed_at(gid, px, 0.0)
    }

    /// [`Font::rasterize_indexed`] for a glyph whose pen sits `dx` px (a
    /// fraction, `0.0..1.0`) right of a whole pixel: the outline is filled
    /// there, and `xmin` is that moved ink box's left edge, so the mask
    /// drawn at the pen's whole pixel plus `xmin` keeps the run's true
    /// spacing. At `dx = 0` it is exactly [`Font::rasterize_indexed`].
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
            // The raster declines an empty or absurd (> 4096 px a side) ink box; the cell
            // keeps its advance.
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
        let units = self.advances.get(usize::from(gid)).copied().unwrap_or(0);
        self.scale_factor(px) * f32::from(units)
    }

    /// Design-space ink box for `gid`: the precomputed `glyf` entry when there
    /// is one, otherwise a face parse (the CFF path). `None` for a blank glyph.
    fn design_bbox(&self, gid: u16) -> Option<[i16; 4]> {
        if gid >= self.glyph_count {
            return None;
        }
        if let Some(table) = self.bboxes.as_ref() {
            let b = *table.get(usize::from(gid))?;
            // A blank glyph (space) has no entry, stored as the all-zero box.
            return (b != [0, 0, 0, 0]).then_some(b);
        }
        let face = aterm_font::Face::parse(&self.data, self.index).ok()?;
        face.glyph_bounding_box(aterm_font::GlyphId(gid))
            .map(|b| [b.x_min, b.y_min, b.x_max, b.y_max])
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
/// A code point no subtable contested has one answer in every Unicode
/// subtable that maps it, and that answer won the enumeration, so only the
/// contested ones are asked. Sorted, because `contested` is sorted first.
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

/// `bytes`, a single-face font file, with the first cmap encoding record for
/// `(platform, encoding)` moved to the END of the record list, the others kept
/// in order. Every subtable and every other table keep their bytes; only which
/// subtable the enumeration reaches last changes. `None` when the file has no
/// such record.
///
/// This is how a test gets Monaco's shape — a Macintosh subtable after the
/// Unicode one — on a face every host has: the embedded DejaVu Sans Mono lists
/// its Mac Roman `(1,0)` subtable BEFORE `(3,1)`, where the enumeration lets
/// the Unicode subtable win.
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
    // The table directory: 12 bytes of header, then 16 per table record, whose
    // tag leads and whose offset is the third field.
    let cmap_record = (0..u16_at(4)?)
        .map(|i| 12 + 16 * i)
        .find(|&record| bytes.get(record..record + 4) == Some(b"cmap".as_slice()))?;
    let cmap = u32_at(cmap_record + 8)?;
    // The cmap header: version, record count, then 8 bytes per encoding record
    // (platform, encoding, subtable offset).
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

/// aterm-font's parse failure, as the `&'static str` the call sites format
/// (ttf-parser's wording, which they always printed).
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
mod tests {
    use super::{Font, FontSettings, with_cmap_record_last};

    /// The characters the Stats Deck drew wrong on Monaco — `·` as `∑`, `×`
    /// as `◊` — and two more Latin-1 characters the same subtables remap.
    const LATIN_1: [char; 4] = ['\u{B7}', '\u{D7}', '\u{B0}', '\u{E9}'];

    /// What the face's Unicode subtables map `ch` to, read off aterm-font one
    /// subtable at a time: the first that maps it to a glyph the face has, `0`
    /// for none, and `None` for a face with no Unicode subtable.
    fn unicode_answer(face: &aterm_font::Face<'_>, ch: char) -> Option<u16> {
        let cmap = face.tables().cmap?;
        let mut unicode = cmap
            .subtables
            .into_iter()
            .filter(|s| s.is_unicode())
            .peekable();
        unicode.peek()?;
        Some(
            unicode
                .find_map(|s| {
                    s.glyph_index(u32::from(ch))
                        .map(|g| g.0)
                        .filter(|&g| g != 0 && g < face.number_of_glyphs())
                })
                .unwrap_or(0),
        )
    }

    /// Holds [`Font::unicode_glyph_index`] to [`unicode_answer`] (to the
    /// enumerated map for a face with no Unicode subtable) over every char any
    /// subtable of `face` maps, and answers the chars where it is not
    /// [`Font::lookup_glyph_index`]'s answer.
    fn moved_from_the_enumerated_map(font: &Font, face: &aterm_font::Face<'_>) -> Vec<char> {
        let mut chars = Vec::new();
        if let Some(cmap) = face.tables().cmap {
            for subtable in cmap.subtables {
                subtable.codepoints(|cp| chars.extend(char::from_u32(cp)));
            }
        }
        chars.sort_unstable();
        chars.dedup();
        let mut moved = Vec::new();
        for ch in chars {
            let want = unicode_answer(face, ch).unwrap_or_else(|| font.lookup_glyph_index(ch));
            assert_eq!(
                font.unicode_glyph_index(ch),
                want,
                "U+{:04X}: the Unicode subtables' glyph",
                u32::from(ch)
            );
            if want != font.lookup_glyph_index(ch) {
                moved.push(ch);
            }
        }
        moved
    }

    /// A MACINTOSH SUBTABLE LISTED AFTER THE UNICODE ONE DOES NOT MOVE A CHAR
    /// OFF ITS GLYPH. The embedded DejaVu Sans Mono with its Mac Roman `(1,0)`
    /// record moved last has Monaco's shape on every host: the enumerated map,
    /// kept as it was, now gives `·` the glyph of Mac Roman byte 0xB7 (`∑`)
    /// and `×` that of 0xD7 (`◊`), which is what the chrome drew. The Unicode
    /// answer is the Unicode subtable's glyph, the very glyph the shipped face
    /// (whose Unicode subtables come last and win its enumeration) answers.
    #[cfg(feature = "embedded-font")]
    #[test]
    fn a_mac_subtable_listed_last_does_not_remap_latin_1() {
        let shipped_bytes = crate::embedded_font();
        let moved_bytes = with_cmap_record_last(shipped_bytes, 1, 0)
            .expect("DejaVu Sans Mono carries a Mac Roman (1,0) subtable");
        let font = Font::from_bytes(moved_bytes.as_slice(), FontSettings::default())
            .expect("the reordered face parses");
        let face = aterm_font::Face::parse(&moved_bytes, 0).expect("the reordered face parses");
        let shipped = Font::from_bytes(shipped_bytes, FontSettings::default())
            .expect("the embedded face parses");
        let gid = |ch: char| face.glyph_index(ch).map_or(0, |g| g.0);

        // The fixture has the defect: the enumerated map reaches Mac Roman last.
        assert_eq!(font.lookup_glyph_index('\u{B7}'), gid('\u{2211}'), "· is ∑");
        assert_eq!(font.lookup_glyph_index('\u{D7}'), gid('\u{25CA}'), "× is ◊");
        for ch in LATIN_1 {
            let want = gid(ch);
            assert_ne!(want, 0, "DejaVu Sans Mono maps U+{:04X}", u32::from(ch));
            assert_ne!(
                font.lookup_glyph_index(ch),
                want,
                "the enumerated map remaps U+{:04X}",
                u32::from(ch)
            );
            assert_eq!(
                font.unicode_glyph_index(ch),
                want,
                "U+{:04X}",
                u32::from(ch)
            );
            assert_eq!(
                shipped.lookup_glyph_index(ch),
                want,
                "U+{:04X}",
                u32::from(ch)
            );
        }
        let moved = moved_from_the_enumerated_map(&font, &face);
        // 89 of the 96 on this face: Mac Roman shares `¢ £ © ± µ` with
        // Latin-1, and DejaVu maps its 0xDA and 0xF0 to no glyph.
        let latin_1 = moved
            .iter()
            .filter(|ch| ('\u{A0}'..='\u{FF}').contains(*ch));
        assert!(
            latin_1.count() >= 80,
            "the Mac Roman subtable remaps most of Latin-1: {moved:?}"
        );
        // And every printable char the reordered face answers by Unicode is the
        // glyph the shipped face draws today.
        for ch in moved.iter().filter(|ch| !ch.is_control()) {
            assert_eq!(
                font.unicode_glyph_index(*ch),
                shipped.lookup_glyph_index(*ch),
                "U+{:04X}",
                u32::from(*ch)
            );
        }
    }

    /// NOTHING A FACE DRAWS RIGHT TODAY MOVES. The embedded faces carry a Mac
    /// Roman subtable BEFORE their Unicode ones, so the enumeration already
    /// gives every printable char its Unicode glyph; the Unicode answer agrees
    /// with it on all of them and differs only on control codes the Mac
    /// subtable maps and the Unicode ones do not. A face whose cmap is Unicode
    /// only (the bundled Iosevka Term) agrees everywhere.
    #[cfg(feature = "embedded-font")]
    #[test]
    fn the_unicode_answer_keeps_every_printable_char_of_the_shipped_faces() {
        for (name, bytes) in [
            ("DejaVu Sans Mono", crate::embedded_font()),
            ("Symbols Nerd Font Mono", crate::embedded_symbols_font()),
            (
                "Iosevka Term",
                crate::bundled::face_for_id(crate::bundled::IOSEVKA_ID)
                    .expect("the bundled Iosevka Term"),
            ),
        ] {
            let font = Font::from_bytes(bytes, FontSettings::default()).expect(name);
            let face = aterm_font::Face::parse(bytes, 0).expect(name);
            let moved = moved_from_the_enumerated_map(&font, &face);
            assert!(
                moved.iter().all(|ch| ch.is_control()),
                "{name}: only control codes move: {moved:?}"
            );
        }
    }

    /// MONACO DRAWS LATIN-1 BY ITS UNICODE SUBTABLE. The face that showed the
    /// defect: its `(0,3)` subtable is followed by Mac Roman, Mac Cyrillic and
    /// Mac Central European ones, and the last wins the enumeration. Every
    /// printable Latin-1 char resolves to the glyph `(0,3)` maps it to — the
    /// glyph the terminal grid draws (`aterm_font::Face::glyph_index`, behind
    /// `Renderer::primary_unicode_gid`).
    #[cfg(target_os = "macos")]
    #[test]
    fn monaco_resolves_latin_1_through_its_unicode_subtable() {
        let path = "/System/Library/Fonts/Monaco.ttf";
        let Ok(bytes) = std::fs::read(path) else {
            aterm_spec::host::could_not_run(format!("no {path} on this Mac"));
        };
        let font = Font::from_bytes(bytes.as_slice(), FontSettings::default()).expect(path);
        let face = aterm_font::Face::parse(&bytes, 0).expect(path);
        let grid = |ch: char| face.glyph_index(ch).map_or(0, |g| g.0);
        for ch in LATIN_1 {
            assert_ne!(grid(ch), 0, "Monaco maps U+{:04X}", u32::from(ch));
        }
        for ch in '\u{A0}'..='\u{FF}' {
            assert_eq!(
                font.unicode_glyph_index(ch),
                grid(ch),
                "U+{:04X}",
                u32::from(ch)
            );
        }
        // Every other char Monaco's subtables map answers by Unicode too. How
        // many of them the enumerated map gets wrong is a fact of Apple's file,
        // not of aterm, so it is not asserted here: the fixture test above
        // proves the defect on every host.
        moved_from_the_enumerated_map(&font, &face);
    }
}
