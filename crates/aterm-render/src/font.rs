// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The first-party font face — the type that RETIRED `fontdue`.
//!
//! # What this is
//!
//! A [`Font`] is a parsed face: the cmap, the horizontal advances, the `hhea`
//! line metrics and the legacy `kern` pairs, read once out of the file with
//! `ttf-parser` (which this crate links anyway, for `rustybuzz`, `sbix`
//! colour-emoji extraction and the FONT-2 variation path). Rasterization is
//! [`crate::variation::varied_glyph_raster_with_face`] — the SAME
//! ttf-parser-outline → [`crate::raster`] signed-area coverage fill the
//! variation and `sbix` paths already used, which is why retiring fontdue added
//! no rasterizer: the rasterizer was already here and already proven, glyph for
//! glyph, against `ab_glyph_rasterizer` at f32 bit equality
//! (`tests/rasterizer_oracle.rs`).
//!
//! The API is deliberately the SEVEN methods the workspace actually called on
//! `fontdue::Font` — [`Font::from_bytes`], [`Font::rasterize`],
//! [`Font::rasterize_indexed`], [`Font::lookup_glyph_index`],
//! [`Font::horizontal_line_metrics`], [`Font::horizontal_kern`],
//! [`Font::metrics`] — plus [`Metrics`], [`LineMetrics`] and [`FontSettings`].
//! Nothing was added "for completeness": an unused method is an unmeasured
//! method.
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
//!   `tests/first_party_face_vs_fontdue.rs` proves that over 109 704
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
//!   come from `rustybuzz` (`crate::ligature_shaping`), which reads GSUB itself.
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
//! opt-level 3 (`tests/face_parse_cost.rs`), live heap and wall time:
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
//! The FILE is retained, unlike fontdue's — which kept converted geometry and
//! not one byte of the file — so the constructors come in three shapes and only
//! one of them copies: [`Font::from_shared_slice`] and [`Font::from_shared_vec`]
//! ADOPT a caller's `Arc`, and every byte store in this crate hands its own
//! handle over. That distinction is worth 54 MB on one face (see the table
//! above), which is why it exists rather than a single `&[u8]` constructor.
//!
//! The face is NOT retained between calls: `ttf_parser::Face<'a>` borrows its
//! bytes, and a self-referential owner would need `unsafe` to express. Instead
//! every field a hot path reads — the cmap map, the advance table, the line
//! metrics, the `kern` bytes and (for `glyf` faces) the per-glyph ink box — is
//! computed at parse time, so the only calls that re-parse are the ones that
//! need an OUTLINE ([`Font::rasterize`], and [`Font::metrics`] on a CFF face).
//! Those are exactly the calls every consumer in the workspace already serves
//! from a glyph cache.

use aterm_hash::FxHashMap;
use std::num::NonZeroU16;
use std::sync::Arc;

/// Settings for [`Font::from_bytes`].
///
/// One field, because one field was used. fontdue's `scale` (the parse-time
/// flattening budget) has no counterpart here — outlines are flattened at the
/// size they are drawn at — and `load_substitutions` (its GSUB pre-load) has no
/// counterpart either, because substitution is `rustybuzz`'s job in this crate.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct FontSettings {
    /// Which face to take out of a TrueType/OpenType COLLECTION (`.ttc`).
    /// `0` for a plain single-face file.
    pub collection_index: u32,
}

/// Layout information for one glyph at one size — fontdue's convention, kept
/// because the whole renderer is written in it.
#[derive(Copy, Clone, PartialEq, Debug, Default)]
pub struct Metrics {
    /// Whole-pixel offset of the bitmap's LEFT edge from the pen, negative when
    /// the glyph reaches left of the origin.
    pub xmin: i32,
    /// Whole-pixel offset of the bitmap's BOTTOM edge from the baseline,
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
    /// Below the baseline, NEGATIVE.
    pub descent: f32,
    /// Designer's recommended gap between one line's descent and the next
    /// line's ascent.
    pub line_gap: f32,
    /// `ascent - descent + line_gap`, precomputed in DESIGN units and then
    /// scaled — the same order fontdue used, so the f32 rounding is identical.
    pub new_line_size: f32,
}

impl LineMetrics {
    /// Build from raw `hhea`/`OS/2` design units, computing `new_line_size` in
    /// `i32` first so a tall face cannot overflow `i16`.
    fn new(ascent: i16, descent: i16, line_gap: i16) -> Self {
        let (a, d, g) = (i32::from(ascent), i32::from(descent), i32::from(line_gap));
        Self {
            ascent: a as f32,
            descent: d as f32,
            line_gap: g as f32,
            new_line_size: (a - d + g) as f32,
        }
    }

    /// Scale design units to pixels.
    fn scale(&self, scale: f32) -> Self {
        Self {
            ascent: self.ascent * scale,
            descent: self.descent * scale,
            line_gap: self.line_gap * scale,
            new_line_size: self.new_line_size * scale,
        }
    }
}

/// The font FILE, however the caller already holds it.
///
/// This exists so that adopting a caller's handle is possible in BOTH of the
/// workspace's two shapes. `fontdue` retained no bytes at all — it kept only the
/// geometry it had already converted — so a face and its file were never the
/// same allocation and the question never came up. This face reads outlines on
/// demand and therefore MUST hold the file, which makes "whose copy" a real
/// question: a 53 MB `.ttc` copied because the constructor only took a slice is
/// 53 MB of pure waste, permanently, in a process that already had it.
///
/// The two arms are the two handle types the crate's byte stores actually use —
/// `DISCOVERED_FONT_BYTES`/`PARSED_FONT_INTERN` hold `Arc<Vec<u8>>`, the styled
/// and shared-face stores hold `Arc<[u8]>`. An `Arc<dyn AsRef<[u8]>>` would
/// unify them on paper and cannot: `Arc<[u8]>` is already unsized and will not
/// coerce again.
#[derive(Clone)]
enum FaceBytes {
    /// The styled / shared-face store's handle.
    Slice(Arc<[u8]>),
    /// The interned / discovered store's handle.
    Vec(Arc<Vec<u8>>),
}

impl core::ops::Deref for FaceBytes {
    type Target = [u8];
    #[inline]
    fn deref(&self) -> &[u8] {
        match self {
            FaceBytes::Slice(b) => b,
            FaceBytes::Vec(b) => b,
        }
    }
}

/// A parsed font face.
///
/// Cheap to clone in the sense that matters: the font FILE is an [`Arc`], so a
/// clone copies the derived tables but never the megabytes.
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
    /// fontdue's `char_to_glyph`: EVERY cmap subtable enumerated, non-zero
    /// mappings only, later subtables overwriting earlier ones.
    char_to_glyph: FxHashMap<char, NonZeroU16>,
    /// `hmtx` advance per glyph id, in design units. Indexed by gid; a gid past
    /// the end of the table repeats the last entry, which is what
    /// `glyph_hor_advance` already does, so this is a straight copy.
    advances: Box<[u16]>,
    /// Per-glyph ink box in design units `(x_min, y_min, x_max, y_max)`, for
    /// `glyf` faces only — there the box is a four-`i16` read out of the glyph
    /// header, so precomputing it makes [`Font::metrics`] allocation-free and
    /// parse-free. A CFF face has no such header (the box is a by-product of
    /// running the charstring), so it stays `None` and `metrics` re-parses.
    bboxes: Option<Box<[[i16; 4]]>>,
    /// Unscaled `hhea`/`OS/2` line metrics, `None` only if the face declares
    /// none.
    line_metrics: Option<LineMetrics>,
    /// The raw legacy `kern` table, copied out of the file (kilobytes) so a
    /// pair lookup costs a header parse instead of a whole-face parse.
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
    /// Parse a face out of `data`.
    ///
    /// The error is a `&'static str` (fontdue's shape, so the call sites'
    /// `.map_err(|e| e.to_string())` is unchanged) naming which table refused.
    ///
    /// This COPIES `data`, because a slice is all it is given. A caller that
    /// already holds the file behind an `Arc` should hand the handle over
    /// instead — [`Font::from_shared_slice`] or [`Font::from_shared_vec`] — and
    /// every store in this crate does. MEASURED on this machine: parsing
    /// `STHeiti Light.ttc` costs 55,562 kB of live heap through here and
    /// 1,086 kB through the sharing constructors, because 53 MB of it is the
    /// copy.
    pub fn from_bytes<D: core::ops::Deref<Target = [u8]>>(
        data: D,
        settings: FontSettings,
    ) -> Result<Font, &'static str> {
        Self::parse(FaceBytes::Slice(Arc::from(&*data)), settings)
    }

    /// [`Font::from_bytes`] adopting a caller's `Arc<[u8]>` — the styled tier's
    /// and the shared-face store's handle type. The face costs its derived
    /// tables and NOT a second copy of the file.
    pub fn from_shared_slice(
        data: Arc<[u8]>,
        settings: FontSettings,
    ) -> Result<Font, &'static str> {
        Self::parse(FaceBytes::Slice(data), settings)
    }

    /// [`Font::from_bytes`] adopting a caller's `Arc<Vec<u8>>` — the interned
    /// and discovered stores' handle type. Same bargain as
    /// [`Font::from_shared_slice`].
    pub fn from_shared_vec(
        data: Arc<Vec<u8>>,
        settings: FontSettings,
    ) -> Result<Font, &'static str> {
        Self::parse(FaceBytes::Vec(data), settings)
    }

    /// The one parse all three constructors reach.
    fn parse(data: FaceBytes, settings: FontSettings) -> Result<Font, &'static str> {
        let index = settings.collection_index;
        let face = ttf_parser::Face::parse(&data, index).map_err(describe)?;
        let upem = f32::from(face.units_per_em());
        // `units_per_em()` widens a `u16`, so this value is always finite and a
        // plain `<=` is the whole test — there is no NaN case to lose.
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
        let mut char_to_glyph: FxHashMap<char, NonZeroU16> = FxHashMap::default();
        if let Some(cmap) = face.tables().cmap {
            for subtable in cmap.subtables {
                subtable.codepoints(|cp| {
                    let Some(ch) = char::from_u32(cp) else {
                        return;
                    };
                    if let Some(gid) = subtable.glyph_index(cp)
                        && let Some(nz) = NonZeroU16::new(gid.0)
                        && nz.get() < glyph_count
                    {
                        char_to_glyph.insert(ch, nz);
                    }
                });
            }
        }

        // `hmtx` is a flat array; reading it once here is what keeps
        // `metrics(..).advance_width` — the UI text measurer's per-character
        // call — off the parse path entirely.
        let advances: Box<[u16]> = (0..glyph_count)
            .map(|gid| {
                face.glyph_hor_advance(ttf_parser::GlyphId(gid))
                    .unwrap_or(0)
            })
            .collect();

        // Ink boxes, but only where they are a header read (see the field docs).
        let bboxes: Option<Box<[[i16; 4]]>> = face.tables().glyf.map(|_| {
            (0..glyph_count)
                .map(|gid| {
                    face.glyph_bounding_box(ttf_parser::GlyphId(gid))
                        .map_or([0, 0, 0, 0], |b| [b.x_min, b.y_min, b.x_max, b.y_max])
                })
                .collect()
        });

        // `Face::ascender`/`descender`/`line_gap` already apply the
        // `USE_TYPO_METRICS` rule, which is the law `geometry_metrics` states
        // elsewhere in this crate — so this is the same triple fontdue read.
        let line_metrics = Some(LineMetrics::new(
            face.ascender(),
            face.descender(),
            face.line_gap(),
        ));

        let kern = face
            .raw_face()
            .table(ttf_parser::Tag::from_bytes(b"kern"))
            .map(Box::<[u8]>::from);

        Ok(Font {
            data,
            index,
            upem,
            glyph_count,
            char_to_glyph,
            advances,
            bboxes,
            line_metrics,
            kern,
        })
    }

    /// The glyph id `ch` maps to, or `0` (`.notdef`) when the face has no
    /// mapping. See the module docs for why this is the enumerated-cmap answer
    /// and not `ttf_parser::Face::glyph_index`'s Unicode-only one.
    #[inline]
    #[must_use]
    pub fn lookup_glyph_index(&self, ch: char) -> u16 {
        self.char_to_glyph.get(&ch).map_or(0, |g| g.get())
    }

    /// Design units per em.
    #[inline]
    #[must_use]
    pub fn units_per_em(&self) -> f32 {
        self.upem
    }

    /// The family this face advertises in its `name` table (ID 1, or the
    /// typographic family 16 when the face declares one), or `None` when it
    /// names itself in an encoding this reader cannot decode.
    ///
    /// Read from the file on demand rather than cached at parse time: the only
    /// callers are diagnostics and the resolver-identity assertions, so a
    /// per-face `String` would be paid by every one of the thousands of faces a
    /// font scan constructs and read by almost none of them. `Self::data` and
    /// `Self::index` are retained for outline reads anyway, so re-parsing here
    /// costs a table walk and no I/O.
    #[must_use]
    pub fn name(&self) -> Option<String> {
        use ttf_parser::name_id::{FAMILY, TYPOGRAPHIC_FAMILY};
        let face = ttf_parser::Face::parse(&self.data, self.index).ok()?;
        let names = face.names();
        let mut family = None;
        for i in 0..names.len() {
            let Some(record) = names.get(i) else { continue };
            // TYPOGRAPHIC_FAMILY wins where both exist — it is the name that
            // groups the styled members of one family (the "Noto Sans" that
            // "Noto Sans SemiBold" belongs to), which is what a caller asking a
            // face what it IS means.
            match record.name_id {
                TYPOGRAPHIC_FAMILY => {
                    if let Some(name) = record.to_string() {
                        return Some(name);
                    }
                }
                FAMILY if family.is_none() => family = record.to_string(),
                _ => {}
            }
        }
        family
    }

    /// Design-units → pixels at `px`.
    #[inline]
    #[must_use]
    pub fn scale_factor(&self, px: f32) -> f32 {
        px / self.upem
    }

    /// Line metrics scaled to `px`; `None` only when the face declares none.
    #[must_use]
    pub fn horizontal_line_metrics(&self, px: f32) -> Option<LineMetrics> {
        Some(self.line_metrics?.scale(self.scale_factor(px)))
    }

    /// Legacy `kern`-table pair adjustment for `left`→`right` at `px`, or
    /// `None` when the face has no `kern` table or no value for the pair.
    ///
    /// GPOS kerning is NOT read here — it was not read before either. A face
    /// whose kerning lives only in GPOS shapes through
    /// [`crate::ligature_shaping`], which runs `rustybuzz` over the real
    /// feature set.
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
        let table = ttf_parser::kern::Table::parse(self.kern.as_deref()?)?;
        let (l, r) = (ttf_parser::GlyphId(left), ttf_parser::GlyphId(right));
        // First horizontal, non-variable subtable that has an opinion wins —
        // the same "take the first usable horizontal subtable" rule fontdue
        // applied, widened only in that a subtable with no entry for THIS pair
        // no longer ends the search.
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
    /// mask, 0 = uncovered, 255 = fully covered, TOP row first.
    #[inline]
    #[must_use]
    pub fn rasterize(&self, ch: char, px: f32) -> (Metrics, Vec<u8>) {
        self.rasterize_indexed(self.lookup_glyph_index(ch), px)
    }

    /// [`Font::rasterize`] by glyph id.
    ///
    /// An id past the end of the face, a non-finite or non-positive `px`, and a
    /// glyph with no outline all return an EMPTY mask — the first two with
    /// default metrics, the last with the glyph's real advance, because a space
    /// still moves the pen.
    #[must_use]
    pub fn rasterize_indexed(&self, gid: u16, px: f32) -> (Metrics, Vec<u8>) {
        // `px` is caller-supplied, so NaN is a real input and is spelled out
        // rather than hidden inside a negated comparison: an unordered size is
        // refused exactly as a non-positive one is.
        if px.is_nan() || px <= 0.0 || gid >= self.glyph_count {
            return (Metrics::default(), Vec::new());
        }
        let Ok(face) = ttf_parser::Face::parse(&self.data, self.index) else {
            return (Metrics::default(), Vec::new());
        };
        match crate::variation::varied_glyph_raster_with_face(&face, gid, px) {
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
            // `varied_glyph_raster_with_face` declines an ink box that is empty
            // or absurd (over 4096 px a side). The advance still has to be
            // right — a cell whose glyph refuses to draw must not also lose its
            // width.
            None => (
                Metrics {
                    advance_width: self.advance_px(gid, px),
                    ..Metrics::default()
                },
                Vec::new(),
            ),
        }
    }

    /// Scaled `hmtx` advance for `gid` — the ONE number the terminal grid is
    /// built from, so it is read straight out of the precomputed table with the
    /// same `scale * units` fontdue used.
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
        let face = ttf_parser::Face::parse(&self.data, self.index).ok()?;
        face.glyph_bounding_box(ttf_parser::GlyphId(gid))
            .map(|b| [b.x_min, b.y_min, b.x_max, b.y_max])
    }
}

/// The bitmap box, in whole pixels, for a design-space ink box at `scale`:
/// `(xmin, ymin, width, height)`.
///
/// This is fontdue's `metrics_raw` arithmetic in its shorter algebraic form —
/// see the module docs for why the two agree integer for integer — and it is
/// the SAME expression [`crate::variation::varied_glyph_raster_with_face`]
/// sizes its coverage grid with, which is what keeps [`Font::metrics`] and
/// [`Font::rasterize`] from ever disagreeing about a glyph's box.
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

/// ttf-parser's parse failure, as the `&'static str` the call sites format.
fn describe(error: ttf_parser::FaceParsingError) -> &'static str {
    use ttf_parser::FaceParsingError::*;
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
