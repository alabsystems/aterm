// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0 AND (MIT OR Apache-2.0)
//
// The value types, `Face` and `fonts_in_collection` below are derived from
// ttf-parser 0.25.1 src/lib.rs (Copyright (c) 2018 Yevhenii Reizner and the
// ttf-parser authors, MIT OR Apache-2.0; see LICENSE-MIT and LICENSE-APACHE),
// cut down to the tables this crate reads and modified by the aterm project in
// 2026. The sinks (`sink.rs`) and the crate layout are aterm's.

// Registers the `trust` tool so `#[cfg_attr(trust_verify, trust::…)]`
// attributes name it. A stable build ignores this.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! aterm's font reader, the sfnt half of the text-fonts carve (TF.3 and TF.5
//! of docs/AUDIT-third-party-2026-10-15.md): every table aterm-render reads.
//!
//! - the sfnt table directory and TrueType/OpenType collections;
//! - `head`, `hhea`, `maxp`, `hmtx`/`vmtx`, `vhea`, `VORG`, `post`, `OS/2`
//!   and `name`;
//! - `cmap` lookups and enumeration, formats 0, 2, 4, 6, 10, 12, 13 and 14;
//! - outlines: `glyf`/`loca`, `gvar`, and CFF1 (CID-keyed included) and CFF2
//!   charstrings;
//! - variations: `fvar`, `avar`, the item variation store, `HVAR`, `VVAR`
//!   and `MVAR` (the line metrics at an instance);
//! - legacy `kern` (OpenType formats 0 and 2, AAT formats 0 to 3; an AAT
//!   state machine is recognised, not run);
//! - colour and bitmap glyphs: `COLR` v0 and v1 with `CPAL`, `sbix`, and
//!   `CBLC`/`CBDT` (with `EBLC`/`EBDT` and Apple's `bloc`/`bdat`, which share
//!   its layout);
//! - `GSUB`: the script, feature and lookup lists with feature variations,
//!   and every substitution subtable type; `GDEF`'s glyph classes and mark
//!   sets; `GPOS`'s lists (not its positioning subtables).
//!
//! On a well-formed font every answer is ttf-parser 0.25.1's, bit for bit,
//! as aterm's Linux build configures it: with `gvar-alloc`, so a glyph past
//! 32 `gvar` tuples draws (macOS, Windows and wasm build ttf-parser without
//! it, and such a glyph has no outline there). The table readers are carved
//! from it, and `tests/reader_oracle.rs` holds this crate to the TF.1 reader
//! corpus ttf-parser recorded, every query of it: outline point streams, COLR
//! paint graphs, bitmap bytes, name records.
//!
//! What it adds is the never-panic guarantee upstream states but does not
//! keep: arithmetic that could overflow on a malformed font is checked or
//! widened, every index the font steers is tested first (by `get`, or by the
//! length test before it), and an outline that cannot be read returns
//! `None`. So a malformed font can answer differently where upstream panics,
//! hangs or does unbounded work: a CFF2 `blend` on an empty stack is an error
//! (upstream panics, release builds too), a `GSUB` extension of an extension
//! is `None` (upstream never returns), `cmap` enumeration stops at U+10FFFF
//! and after one call per code point the format can name (overlapping groups
//! multiplied it upstream), an outline query stops after 65,536 steps
//! (glyph entries, TrueType points and CFF operators, counted across a
//! composite's or a charstring's whole recursion: nesting limits alone let a
//! shared-child graph take upstream exponential time; the heaviest glyph of
//! 809 macOS system and committed faces takes 5,598, measured 2026-10-16),
//! and a `COLR` paint walk stops after 2^16 paint tables (likewise; the COLR
//! v1 test fonts' glyphs visit at most 51). Where upstream's release build
//! wraps instead of panicking (OS/2 win descent, the face heights, a `COLR`
//! variation index past `u32::MAX`), this crate wraps too. A font is
//! untrusted input (a user's font directory, a font a web page hands the
//! wasm build), so `tests/never_panic.rs` mutates real faces and asks every
//! query of every glyph.
//!
//! Outlines go to an [`OutlineSink`] in font units. [`ScaledSink`] maps them
//! into a raster's pixel space, and [`PointSink`] receives the unscaled points
//! (on- and off-curve, contour by contour) a grid-fitter works on.
//!
//! [`shape`] is the text shaper over this reader (TF.6), carved from rustybuzz
//! 0.20.1: `GSUB` lookup types 1 to 8 with their flags and mark filtering,
//! the default and caller features, automatic fractions, OpenType script
//! selection and the Arabic joining forms. Where it answers differently from
//! rustybuzz, by design, is listed in `src/shape/pipeline.rs`: no
//! normalization, no mirroring (the bidi step's), positions by advance alone,
//! and no AAT `morx`.
//!
//! `autohint` (the `hint` feature, and every build with debug assertions) is
//! FreeType's automatic hinter as skrifa
//! 0.46.2 ports it (TF.4): a face's glyphs grid-fitted at a size, instance
//! and smooth target, answering skrifa's outlines bit for bit.

#![forbid(unsafe_code)]
// The derived readers (and their upstream unit tests) keep ttf-parser
// 0.25.1's shape, so each file still diffs readably against its upstream
// file; these lints flag only that shape. Code aterm writes meets them.
#![allow(clippy::identity_op)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::collapsible_else_if)]
#![allow(clippy::upper_case_acronyms)]
#![allow(clippy::get_first)]
#![allow(clippy::new_without_default)]
#![allow(clippy::len_without_is_empty)]
#![allow(clippy::field_reassign_with_default)]
#![allow(clippy::manual_is_multiple_of)]
#![allow(clippy::derivable_impls)]
#![allow(clippy::same_item_push)]
#![cfg_attr(test, allow(clippy::bool_assert_comparison))]

#[cfg(any(feature = "hint", debug_assertions))]
pub mod autohint;
mod delta_set;
mod ggg;
mod parser;
pub mod shape;
mod sink;
mod tables;
mod var_store;
mod work;

pub use parser::{Fixed, FromData, LazyArray16, LazyArray32, LazyArrayIter16, LazyArrayIter32};
use parser::{NumFrom, Offset, Offset32, Stream, TryNumFrom};
pub use sink::{OutlineSink, PointKind, PointSink, ScaledSink};
pub use tables::name::name_id;
pub use tables::os2::{Permissions, ScriptMetrics, Style, UnicodeRanges, Weight, Width};
pub use tables::{
    CFFError, PlatformId, avar, cbdt, cblc, cff1 as cff, cff2, cmap, colr, cpal, fvar, gdef, glyf,
    gsub, gvar, head, hhea, hmtx, hvar, kern, loca, maxp, mvar, name, os2, post, sbix, vhea, vorg,
    vvar,
};

pub use fvar::VariationAxis;
use head::IndexToLocationFormat;
#[cfg(all(any(test, debug_assertions), not(trust_verify)))]
pub use work::{Spent, take_spent};

pub mod opentype_layout {
    //! The [OpenType Layout](
    //! https://docs.microsoft.com/en-us/typography/opentype/spec/chapter2#overview)
    //! common tables: script, feature and lookup lists, coverage and class
    //! definitions, (chained) context lookups and feature variations.
    pub use crate::ggg::*;
}

/// A type-safe wrapper for glyph ID.
#[repr(transparent)]
#[derive(Clone, Copy, Ord, PartialOrd, Eq, PartialEq, Default, Debug, Hash)]
pub struct GlyphId(pub u16);

impl FromData for GlyphId {
    const SIZE: usize = 2;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        u16::parse(data).map(GlyphId)
    }
}

/// A TrueType font magic.
///
/// <https://docs.microsoft.com/en-us/typography/opentype/spec/otff#organization-of-an-opentype-font>
#[derive(Clone, Copy, PartialEq, Debug)]
enum Magic {
    TrueType,
    OpenType,
    FontCollection,
}

impl FromData for Magic {
    const SIZE: usize = 4;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        match u32::parse(data)? {
            0x00010000 | 0x74727565 => Some(Magic::TrueType),
            0x4F54544F => Some(Magic::OpenType),
            0x74746366 => Some(Magic::FontCollection),
            _ => None,
        }
    }
}

/// A variation coordinate in a normalized coordinate system.
///
/// Basically any number in a -1.0..1.0 range, where 0 is the default value,
/// stored as F2DOT14.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct NormalizedCoordinate(i16);

impl From<i16> for NormalizedCoordinate {
    /// Creates a new coordinate, clamped to the -16384..=16384 range.
    #[inline]
    fn from(n: i16) -> Self {
        NormalizedCoordinate(parser::i16_bound(-16384, n, 16384))
    }
}

impl From<f32> for NormalizedCoordinate {
    /// Creates a new coordinate, clamped to the -1.0..=1.0 range. NaN is 0.
    #[inline]
    fn from(n: f32) -> Self {
        NormalizedCoordinate((parser::f32_bound(-1.0, n, 1.0) * 16384.0) as i16)
    }
}

impl NormalizedCoordinate {
    /// Returns the coordinate value as F2DOT14.
    #[inline]
    #[must_use]
    pub const fn get(self) -> i16 {
        self.0
    }
}

/// A 4-byte tag.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tag(pub u32);

impl Tag {
    /// Creates a `Tag` from bytes.
    #[inline]
    #[must_use]
    pub const fn from_bytes(bytes: &[u8; 4]) -> Self {
        Tag(u32::from_be_bytes(*bytes))
    }

    /// Returns tag as 4-element byte array.
    #[inline]
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 4] {
        self.0.to_be_bytes()
    }

    /// Returns tag value as `u32` number.
    #[inline]
    #[must_use]
    pub const fn as_u32(&self) -> u32 {
        self.0
    }
}

impl core::fmt::Debug for Tag {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Tag({self})")
    }
}

impl core::fmt::Display for Tag {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for b in self.to_bytes() {
            write!(f, "{}", char::from(b))?;
        }
        Ok(())
    }
}

impl FromData for Tag {
    const SIZE: usize = 4;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        u32::parse(data).map(Tag)
    }
}

/// Line metrics (the `post` table's underline).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LineMetrics {
    /// Line position.
    pub position: i16,
    /// Line thickness.
    pub thickness: i16,
}

/// A rectangle.
///
/// Doesn't guarantee that `x_min` <= `x_max` and/or `y_min` <= `y_max`.
#[repr(C)]
#[allow(missing_docs)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x_min: i16,
    pub y_min: i16,
    pub x_max: i16,
    pub y_max: i16,
}

impl Rect {
    #[inline]
    const fn zero() -> Self {
        Self {
            x_min: 0,
            y_min: 0,
            x_max: 0,
            y_max: 0,
        }
    }
}

/// A rectangle described by the left-lower and upper-right points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectF {
    /// The horizontal minimum of the rect.
    pub x_min: f32,
    /// The vertical minimum of the rect.
    pub y_min: f32,
    /// The horizontal maximum of the rect.
    pub x_max: f32,
    /// The vertical maximum of the rect.
    pub y_max: f32,
}

impl RectF {
    #[inline]
    const fn new() -> Self {
        RectF {
            x_min: f32::MAX,
            y_min: f32::MAX,
            x_max: f32::MIN,
            y_max: f32::MIN,
        }
    }

    #[inline]
    const fn is_default(&self) -> bool {
        self.x_min == f32::MAX
            && self.y_min == f32::MAX
            && self.x_max == f32::MIN
            && self.y_max == f32::MIN
    }

    #[inline]
    fn extend_by(&mut self, x: f32, y: f32) {
        self.x_min = self.x_min.min(x);
        self.y_min = self.y_min.min(y);
        self.x_max = self.x_max.max(x);
        self.y_max = self.y_max.max(y);
    }

    #[inline]
    fn to_rect(self) -> Option<Rect> {
        Some(Rect {
            x_min: i16::try_num_from(self.x_min)?,
            y_min: i16::try_num_from(self.y_min)?,
            x_max: i16::try_num_from(self.x_max)?,
            y_max: i16::try_num_from(self.y_max)?,
        })
    }
}

/// An affine transform.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Transform {
    /// The 'a' component of the transform.
    pub a: f32,
    /// The 'b' component of the transform.
    pub b: f32,
    /// The 'c' component of the transform.
    pub c: f32,
    /// The 'd' component of the transform.
    pub d: f32,
    /// The 'e' component of the transform.
    pub e: f32,
    /// The 'f' component of the transform.
    pub f: f32,
}

impl Transform {
    /// Creates a new transform with the specified components.
    #[inline]
    #[must_use]
    pub const fn new(a: f32, b: f32, c: f32, d: f32, e: f32, f: f32) -> Self {
        Transform { a, b, c, d, e, f }
    }

    /// Creates a new translation transform.
    #[inline]
    #[must_use]
    pub const fn new_translate(tx: f32, ty: f32) -> Self {
        Transform::new(1.0, 0.0, 0.0, 1.0, tx, ty)
    }

    /// Creates a new rotation transform; `angle` is in half-turns.
    #[inline]
    #[must_use]
    pub fn new_rotate(angle: f32) -> Self {
        let cc = (angle * core::f32::consts::PI).cos();
        let ss = (angle * core::f32::consts::PI).sin();

        Transform::new(cc, ss, -ss, cc, 0.0, 0.0)
    }

    /// Creates a new skew transform; the angles are in half-turns.
    #[inline]
    #[must_use]
    pub fn new_skew(skew_x: f32, skew_y: f32) -> Self {
        let x = (skew_x * core::f32::consts::PI).tan();
        let y = (skew_y * core::f32::consts::PI).tan();

        Transform::new(1.0, y, -x, 1.0, 0.0, 0.0)
    }

    /// Creates a new scale transform.
    #[inline]
    #[must_use]
    pub const fn new_scale(sx: f32, sy: f32) -> Self {
        Transform::new(sx, 0.0, 0.0, sy, 0.0, 0.0)
    }

    /// Combines two transforms with each other.
    #[inline]
    #[must_use]
    pub const fn combine(ts1: Self, ts2: Self) -> Self {
        Transform {
            a: ts1.a * ts2.a + ts1.c * ts2.b,
            b: ts1.b * ts2.a + ts1.d * ts2.b,
            c: ts1.a * ts2.c + ts1.c * ts2.d,
            d: ts1.b * ts2.c + ts1.d * ts2.d,
            e: ts1.a * ts2.e + ts1.c * ts2.f + ts1.e,
            f: ts1.b * ts2.e + ts1.d * ts2.f + ts1.f,
        }
    }

    #[inline]
    fn apply_to(&self, x: &mut f32, y: &mut f32) {
        let tx = *x;
        let ty = *y;
        *x = self.a * tx + self.c * ty + self.e;
        *y = self.b * tx + self.d * ty + self.f;
    }

    /// Checks whether a transform is the identity transform.
    #[inline]
    #[must_use]
    pub const fn is_default(&self) -> bool {
        // A direct float comparison is fine in our case.
        self.a == 1.0
            && self.b == 0.0
            && self.c == 0.0
            && self.d == 1.0
            && self.e == 0.0
            && self.f == 0.0
    }
}

impl Default for Transform {
    #[inline]
    fn default() -> Self {
        Transform::new(1.0, 0.0, 0.0, 1.0, 0.0, 0.0)
    }
}

/// A float point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointF {
    /// The X-axis coordinate.
    pub x: f32,
    /// The Y-axis coordinate.
    pub y: f32,
}

/// A RGBA color in the sRGB color space.
#[allow(missing_docs)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RgbaColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

impl RgbaColor {
    /// Creates a new `RgbaColor`.
    #[inline]
    #[must_use]
    pub const fn new(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        Self {
            blue,
            green,
            red,
            alpha,
        }
    }

    /// Scales the alpha by `alpha` (0..=1); the cast saturates, and NaN
    /// gives 0.
    #[inline]
    pub(crate) fn apply_alpha(&mut self, alpha: f32) {
        self.alpha = (((f32::from(self.alpha) / 255.0) * alpha) * 255.0) as u8;
    }
}

/// A glyph raster image format.
#[allow(missing_docs)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RasterImageFormat {
    PNG,

    /// A monochrome bitmap.
    ///
    /// The most significant bit of the first byte corresponds to the top-left pixel, proceeding
    /// through succeeding bits moving left to right. The data for each row is padded to a byte
    /// boundary, so the next row begins with the most significant bit of a new byte. 1 corresponds
    /// to black, and 0 to white.
    BitmapMono,

    /// A packed monochrome bitmap.
    ///
    /// The most significant bit of the first byte corresponds to the top-left pixel, proceeding
    /// through succeeding bits moving left to right. Data is tightly packed with no padding. 1
    /// corresponds to black, and 0 to white.
    BitmapMonoPacked,

    /// A grayscale bitmap with 2 bits per pixel.
    ///
    /// The most significant bits of the first byte corresponds to the top-left pixel, proceeding
    /// through succeeding bits moving left to right. The data for each row is padded to a byte
    /// boundary, so the next row begins with the most significant bit of a new byte.
    BitmapGray2,

    /// A packed grayscale bitmap with 2 bits per pixel.
    ///
    /// The most significant bits of the first byte corresponds to the top-left pixel, proceeding
    /// through succeeding bits moving left to right. Data is tightly packed with no padding.
    BitmapGray2Packed,

    /// A grayscale bitmap with 4 bits per pixel.
    ///
    /// The most significant bits of the first byte corresponds to the top-left pixel, proceeding
    /// through succeeding bits moving left to right. The data for each row is padded to a byte
    /// boundary, so the next row begins with the most significant bit of a new byte.
    BitmapGray4,

    /// A packed grayscale bitmap with 4 bits per pixel.
    ///
    /// The most significant bits of the first byte corresponds to the top-left pixel, proceeding
    /// through succeeding bits moving left to right. Data is tightly packed with no padding.
    BitmapGray4Packed,

    /// A grayscale bitmap with 8 bits per pixel.
    ///
    /// The first byte corresponds to the top-left pixel, proceeding through succeeding bytes
    /// moving left to right.
    BitmapGray8,

    /// A color bitmap with 32 bits per pixel.
    ///
    /// The first group of four bytes corresponds to the top-left pixel, proceeding through
    /// succeeding pixels moving left to right. Each byte corresponds to a color channel and the
    /// channels within a pixel are in blue, green, red, alpha order. Color values are
    /// pre-multiplied by the alpha. For example, the color "full-green with half translucency"
    /// is encoded as `\x00\x80\x00\x80`, and not `\x00\xFF\x00\x80`.
    BitmapPremulBgra32,
}

/// A glyph's raster image.
///
/// Note, that glyph metrics are in pixels and not in font units.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RasterGlyphImage<'a> {
    /// Horizontal offset.
    pub x: i16,

    /// Vertical offset.
    pub y: i16,

    /// Image width.
    ///
    /// It doesn't guarantee that this value is the same as set in the `data`.
    pub width: u16,

    /// Image height.
    ///
    /// It doesn't guarantee that this value is the same as set in the `data`.
    pub height: u16,

    /// A pixels per em of the selected strike.
    pub pixels_per_em: u16,

    /// An image format.
    pub format: RasterImageFormat,

    /// A raw image data. It's up to the caller to decode it.
    pub data: &'a [u8],
}

/// Phantom points: the `gvar` deltas of a glyph's side bearings and advances.
///
/// Available only for variable fonts with the `gvar` table.
#[derive(Clone, Copy, Debug)]
pub struct PhantomPoints {
    /// Left side bearing point.
    pub left: PointF,
    /// Right side bearing point.
    pub right: PointF,
    /// Top side bearing point.
    pub top: PointF,
    /// Bottom side bearing point.
    pub bottom: PointF,
}

/// A raw table record.
#[derive(Clone, Copy, Debug)]
#[allow(missing_docs)]
pub struct TableRecord {
    pub tag: Tag,
    pub check_sum: u32,
    pub offset: u32,
    pub length: u32,
}

impl FromData for TableRecord {
    const SIZE: usize = 16;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        Some(TableRecord {
            tag: s.read::<Tag>()?,
            check_sum: s.read::<u32>()?,
            offset: s.read::<u32>()?,
            length: s.read::<u32>()?,
        })
    }
}

/// The most variation axes a [`Face`] keeps coordinates for; a face with
/// more is read at its default instance only.
const MAX_VAR_COORDS: usize = 64;

#[derive(Clone)]
struct VarCoords {
    data: [NormalizedCoordinate; MAX_VAR_COORDS],
    len: u8,
}

impl Default for VarCoords {
    #[inline]
    fn default() -> Self {
        Self {
            data: [NormalizedCoordinate::default(); MAX_VAR_COORDS],
            len: 0,
        }
    }
}

impl VarCoords {
    #[inline]
    fn as_slice(&self) -> &[NormalizedCoordinate] {
        self.data.get(..usize::from(self.len)).unwrap_or(&[])
    }

    #[inline]
    fn as_mut_slice(&mut self) -> &mut [NormalizedCoordinate] {
        self.data
            .get_mut(..usize::from(self.len))
            .unwrap_or(&mut [])
    }
}

/// A list of font face parsing errors.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FaceParsingError {
    /// An attempt to read out of bounds detected.
    ///
    /// Should occur only on malformed fonts.
    MalformedFont,
    /// Face data must start with `0x00010000`, `0x74727565`, `0x4F54544F` or `0x74746366`.
    UnknownMagic,
    /// The face index is larger than the number of faces in the font.
    FaceIndexOutOfBounds,
    /// The `head` table is missing or malformed.
    NoHeadTable,
    /// The `hhea` table is missing or malformed.
    NoHheaTable,
    /// The `maxp` table is missing or malformed.
    NoMaxpTable,
}

impl core::fmt::Display for FaceParsingError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FaceParsingError::MalformedFont => write!(f, "malformed font"),
            FaceParsingError::UnknownMagic => write!(f, "unknown magic"),
            FaceParsingError::FaceIndexOutOfBounds => write!(f, "face index is out of bounds"),
            FaceParsingError::NoHeadTable => write!(f, "the head table is missing or malformed"),
            FaceParsingError::NoHheaTable => write!(f, "the hhea table is missing or malformed"),
            FaceParsingError::NoMaxpTable => write!(f, "the maxp table is missing or malformed"),
        }
    }
}

impl std::error::Error for FaceParsingError {}

/// A raw font face: the table directory of one face of a font file.
#[derive(Clone, Copy)]
pub struct RawFace<'a> {
    /// The input font file data.
    pub data: &'a [u8],
    /// An array of table records.
    pub table_records: LazyArray16<'a, TableRecord>,
}

impl<'a> RawFace<'a> {
    /// Reads the table directory of face `index` (0 for a single-face file;
    /// [`fonts_in_collection`] gives a collection's count).
    pub fn parse(data: &'a [u8], index: u32) -> Result<Self, FaceParsingError> {
        // https://docs.microsoft.com/en-us/typography/opentype/spec/otff#organization-of-an-opentype-font

        let mut s = Stream::new(data);

        // Read **font** magic.
        let magic = s.read::<Magic>().ok_or(FaceParsingError::UnknownMagic)?;
        if magic == Magic::FontCollection {
            s.skip::<u32>(); // version
            let number_of_faces = s.read::<u32>().ok_or(FaceParsingError::MalformedFont)?;
            let offsets = s
                .read_array32::<Offset32>(number_of_faces)
                .ok_or(FaceParsingError::MalformedFont)?;

            let face_offset = offsets
                .get(index)
                .ok_or(FaceParsingError::FaceIndexOutOfBounds)?;
            // Face offset is from the start of the font data,
            // so we have to adjust it to the current parser offset.
            let face_offset = face_offset
                .to_usize()
                .checked_sub(s.offset())
                .ok_or(FaceParsingError::MalformedFont)?;
            s.advance_checked(face_offset)
                .ok_or(FaceParsingError::MalformedFont)?;

            // Read **face** magic.
            // Each face in a font collection also starts with a magic.
            let magic = s.read::<Magic>().ok_or(FaceParsingError::UnknownMagic)?;
            // And face in a font collection can't be another collection.
            if magic == Magic::FontCollection {
                return Err(FaceParsingError::UnknownMagic);
            }
        } else {
            // When reading from a regular font (not a collection) disallow index to be non-zero
            // Basically treat the font as a one-element collection
            if index != 0 {
                return Err(FaceParsingError::FaceIndexOutOfBounds);
            }
        }

        let num_tables = s.read::<u16>().ok_or(FaceParsingError::MalformedFont)?;
        s.advance(6); // searchRange (u16) + entrySelector (u16) + rangeShift (u16)
        let table_records = s
            .read_array16::<TableRecord>(num_tables)
            .ok_or(FaceParsingError::MalformedFont)?;

        Ok(RawFace {
            data,
            table_records,
        })
    }

    /// Returns the raw data of a selected table.
    #[inline]
    #[must_use]
    pub fn table(&self, tag: Tag) -> Option<&'a [u8]> {
        let (_, table) = self
            .table_records
            .binary_search_by(|record| record.tag.cmp(&tag))?;
        let offset = usize::num_from(table.offset);
        let length = usize::num_from(table.length);
        let end = offset.checked_add(length)?;
        self.data.get(offset..end)
    }
}

impl core::fmt::Debug for RawFace<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "RawFace {{ ... }}")
    }
}

/// The raw data of the tables this crate reads, by tag.
#[derive(Clone, Copy, Default)]
struct RawFaceTables<'a> {
    head: &'a [u8],
    hhea: &'a [u8],
    maxp: &'a [u8],

    bdat: Option<&'a [u8]>,
    bloc: Option<&'a [u8]>,
    cbdt: Option<&'a [u8]>,
    cblc: Option<&'a [u8]>,
    cff: Option<&'a [u8]>,
    cmap: Option<&'a [u8]>,
    colr: Option<&'a [u8]>,
    cpal: Option<&'a [u8]>,
    ebdt: Option<&'a [u8]>,
    eblc: Option<&'a [u8]>,
    gdef: Option<&'a [u8]>,
    glyf: Option<&'a [u8]>,
    gpos: Option<&'a [u8]>,
    gsub: Option<&'a [u8]>,
    hmtx: Option<&'a [u8]>,
    kern: Option<&'a [u8]>,
    loca: Option<&'a [u8]>,
    name: Option<&'a [u8]>,
    os2: Option<&'a [u8]>,
    post: Option<&'a [u8]>,
    sbix: Option<&'a [u8]>,
    vhea: Option<&'a [u8]>,
    vmtx: Option<&'a [u8]>,
    vorg: Option<&'a [u8]>,

    avar: Option<&'a [u8]>,
    cff2: Option<&'a [u8]>,
    fvar: Option<&'a [u8]>,
    gvar: Option<&'a [u8]>,
    hvar: Option<&'a [u8]>,
    mvar: Option<&'a [u8]>,
    vvar: Option<&'a [u8]>,
}

/// The parsed tables of a [`Face`]. A malformed optional table is `None`.
#[allow(missing_docs)]
#[derive(Clone)]
pub struct FaceTables<'a> {
    // Mandatory tables.
    pub head: head::Table,
    pub hhea: hhea::Table,
    pub maxp: maxp::Table,

    /// Apple's `bloc` + `bdat` bitmaps (the `EBLC`/`EBDT` layout).
    pub bdat: Option<cbdt::Table<'a>>,
    pub cbdt: Option<cbdt::Table<'a>>,
    pub cff: Option<cff::Table<'a>>,
    pub cmap: Option<cmap::Table<'a>>,
    /// `COLR`, with the `CPAL` it reads its colours from.
    pub colr: Option<colr::Table<'a>>,
    pub ebdt: Option<cbdt::Table<'a>>,
    pub gdef: Option<gdef::Table<'a>>,
    pub glyf: Option<glyf::Table<'a>>,
    /// The `GPOS` layout lists (scripts, features, lookup headers); its
    /// positioning subtables are not read.
    pub gpos: Option<opentype_layout::LayoutTable<'a>>,
    /// The `GSUB` layout table: its lookups' subtables read as
    /// [`gsub::SubstitutionSubtable`].
    pub gsub: Option<opentype_layout::LayoutTable<'a>>,
    pub hmtx: Option<hmtx::Table<'a>>,
    pub kern: Option<kern::Table<'a>>,
    pub name: Option<name::Table<'a>>,
    pub os2: Option<os2::Table<'a>>,
    pub post: Option<post::Table>,
    pub sbix: Option<sbix::Table<'a>>,
    pub vhea: Option<vhea::Table>,
    pub vmtx: Option<hmtx::Table<'a>>,
    pub vorg: Option<vorg::Table<'a>>,

    pub avar: Option<avar::Table<'a>>,
    pub cff2: Option<cff2::Table<'a>>,
    pub fvar: Option<fvar::Table<'a>>,
    pub gvar: Option<gvar::Table<'a>>,
    pub hvar: Option<hvar::Table<'a>>,
    pub mvar: Option<mvar::Table<'a>>,
    pub vvar: Option<vvar::Table<'a>>,
}

/// A font face: the tables of one face of a font file, and the variation
/// instance its varied answers are read at.
///
/// `Face` borrows the font data and allocates nothing; parse one when needed.
#[derive(Clone)]
pub struct Face<'a> {
    raw_face: RawFace<'a>,
    tables: FaceTables<'a>,
    coordinates: VarCoords,
}

impl<'a> Face<'a> {
    /// Parses face `index` of `data` (0 for a single-face file).
    ///
    /// Required tables: `head`, `hhea` and `maxp`. An optional table with
    /// invalid data is skipped.
    pub fn parse(data: &'a [u8], index: u32) -> Result<Self, FaceParsingError> {
        let raw_face = RawFace::parse(data, index)?;
        let raw_tables = Self::collect_tables(raw_face);

        let mut face = Face {
            raw_face,
            coordinates: VarCoords::default(),
            tables: Self::parse_tables(raw_tables)?,
        };

        if let Some(ref fvar) = face.tables.fvar {
            face.coordinates.len = fvar.axes.len().min(MAX_VAR_COORDS as u16) as u8;
        }

        Ok(face)
    }

    fn collect_tables(raw_face: RawFace<'a>) -> RawFaceTables<'a> {
        let mut tables = RawFaceTables::default();

        for record in raw_face.table_records {
            let start = usize::num_from(record.offset);
            let end = match start.checked_add(usize::num_from(record.length)) {
                Some(v) => v,
                None => continue,
            };

            let table_data = raw_face.data.get(start..end);
            match &record.tag.to_bytes() {
                b"CBDT" => tables.cbdt = table_data,
                b"CBLC" => tables.cblc = table_data,
                b"CFF " => tables.cff = table_data,
                b"CFF2" => tables.cff2 = table_data,
                b"COLR" => tables.colr = table_data,
                b"CPAL" => tables.cpal = table_data,
                b"EBDT" => tables.ebdt = table_data,
                b"EBLC" => tables.eblc = table_data,
                b"GDEF" => tables.gdef = table_data,
                b"GPOS" => tables.gpos = table_data,
                b"GSUB" => tables.gsub = table_data,
                b"HVAR" => tables.hvar = table_data,
                b"MVAR" => tables.mvar = table_data,
                b"OS/2" => tables.os2 = table_data,
                b"VORG" => tables.vorg = table_data,
                b"VVAR" => tables.vvar = table_data,
                b"avar" => tables.avar = table_data,
                b"bdat" => tables.bdat = table_data,
                b"bloc" => tables.bloc = table_data,
                b"cmap" => tables.cmap = table_data,
                b"fvar" => tables.fvar = table_data,
                b"glyf" => tables.glyf = table_data,
                b"gvar" => tables.gvar = table_data,
                b"head" => tables.head = table_data.unwrap_or_default(),
                b"hhea" => tables.hhea = table_data.unwrap_or_default(),
                b"hmtx" => tables.hmtx = table_data,
                b"kern" => tables.kern = table_data,
                b"loca" => tables.loca = table_data,
                b"maxp" => tables.maxp = table_data.unwrap_or_default(),
                b"name" => tables.name = table_data,
                b"post" => tables.post = table_data,
                b"sbix" => tables.sbix = table_data,
                b"vhea" => tables.vhea = table_data,
                b"vmtx" => tables.vmtx = table_data,
                _ => {}
            }
        }

        tables
    }

    fn parse_tables(raw_tables: RawFaceTables<'a>) -> Result<FaceTables<'a>, FaceParsingError> {
        let head = head::Table::parse(raw_tables.head).ok_or(FaceParsingError::NoHeadTable)?;
        let hhea = hhea::Table::parse(raw_tables.hhea).ok_or(FaceParsingError::NoHheaTable)?;
        let maxp = maxp::Table::parse(raw_tables.maxp).ok_or(FaceParsingError::NoMaxpTable)?;

        let hmtx = raw_tables.hmtx.and_then(|data| {
            hmtx::Table::parse(hhea.number_of_metrics, maxp.number_of_glyphs, data)
        });

        let vhea = raw_tables.vhea.and_then(vhea::Table::parse);
        let vmtx = if let Some(vhea) = vhea {
            raw_tables.vmtx.and_then(|data| {
                hmtx::Table::parse(vhea.number_of_metrics, maxp.number_of_glyphs, data)
            })
        } else {
            None
        };

        let loca = raw_tables.loca.and_then(|data| {
            loca::Table::parse(maxp.number_of_glyphs, head.index_to_location_format, data)
        });
        let glyf = if let Some(loca) = loca {
            raw_tables
                .glyf
                .and_then(|data| glyf::Table::parse(loca, data))
        } else {
            None
        };

        let bdat = if let Some(bloc) = raw_tables.bloc.and_then(cblc::Table::parse) {
            raw_tables
                .bdat
                .and_then(|data| cbdt::Table::parse(bloc, data))
        } else {
            None
        };

        let cbdt = if let Some(cblc) = raw_tables.cblc.and_then(cblc::Table::parse) {
            raw_tables
                .cbdt
                .and_then(|data| cbdt::Table::parse(cblc, data))
        } else {
            None
        };

        let ebdt = if let Some(eblc) = raw_tables.eblc.and_then(cblc::Table::parse) {
            raw_tables
                .ebdt
                .and_then(|data| cbdt::Table::parse(eblc, data))
        } else {
            None
        };

        let cpal = raw_tables.cpal.and_then(cpal::Table::parse);
        let colr = if let Some(cpal) = cpal {
            raw_tables
                .colr
                .and_then(|data| colr::Table::parse(cpal, data))
        } else {
            None
        };

        Ok(FaceTables {
            head,
            hhea,
            maxp,

            bdat,
            cbdt,
            cff: raw_tables.cff.and_then(cff::Table::parse),
            cmap: raw_tables.cmap.and_then(cmap::Table::parse),
            colr,
            ebdt,
            gdef: raw_tables.gdef.and_then(gdef::Table::parse),
            glyf,
            gpos: raw_tables
                .gpos
                .and_then(opentype_layout::LayoutTable::parse),
            gsub: raw_tables
                .gsub
                .and_then(opentype_layout::LayoutTable::parse),
            hmtx,
            kern: raw_tables.kern.and_then(kern::Table::parse),
            name: raw_tables.name.and_then(name::Table::parse),
            os2: raw_tables.os2.and_then(os2::Table::parse),
            post: raw_tables.post.and_then(post::Table::parse),
            sbix: raw_tables
                .sbix
                .and_then(|data| sbix::Table::parse(maxp.number_of_glyphs, data)),
            vhea,
            vmtx,
            vorg: raw_tables.vorg.and_then(vorg::Table::parse),

            avar: raw_tables.avar.and_then(avar::Table::parse),
            cff2: raw_tables.cff2.and_then(cff2::Table::parse),
            fvar: raw_tables.fvar.and_then(fvar::Table::parse),
            gvar: raw_tables.gvar.and_then(gvar::Table::parse),
            hvar: raw_tables.hvar.and_then(hvar::Table::parse),
            mvar: raw_tables.mvar.and_then(mvar::Table::parse),
            vvar: raw_tables.vvar.and_then(vvar::Table::parse),
        })
    }

    /// Returns the parsed tables.
    #[inline]
    #[must_use]
    pub const fn tables(&self) -> &FaceTables<'a> {
        &self.tables
    }

    /// Returns the table directory this face was parsed from.
    #[inline]
    #[must_use]
    pub const fn raw_face(&self) -> &RawFace<'a> {
        &self.raw_face
    }

    /// Checks that face is marked as *Monospaced* (`post.isFixedPitch`).
    ///
    /// Returns `false` when `post` table is not present.
    #[inline]
    #[must_use]
    pub fn is_monospaced(&self) -> bool {
        self.tables
            .post
            .map(|post| post.is_monospaced)
            .unwrap_or(false)
    }

    /// Checks that face is variable: it has a `fvar` table with an axis.
    #[inline]
    #[must_use]
    pub const fn is_variable(&self) -> bool {
        // `fvar::Table::parse` already checked that `axisCount` is non-zero.
        self.tables.fvar.is_some()
    }

    /// Returns face's italic angle.
    ///
    /// Returns `0.0` when `post` table is not present.
    #[inline]
    pub fn italic_angle(&self) -> f32 {
        self.tables
            .post
            .map(|table| table.italic_angle)
            .unwrap_or(0.0)
    }

    /// Returns the `name` table's records; empty when the face has none.
    #[inline]
    pub fn names(&self) -> name::Names<'a> {
        self.tables.name.unwrap_or_default().names
    }

    /// Checks that face is marked as *Regular* (upright: see [`Self::style`]).
    #[inline]
    pub fn is_regular(&self) -> bool {
        self.style() == Style::Normal
    }

    /// Checks that face is marked as *Italic*.
    #[inline]
    pub fn is_italic(&self) -> bool {
        // A face can have a Normal style and a non-zero italic angle, which also makes it italic.
        self.style() == Style::Italic || self.italic_angle() != 0.0
    }

    /// Checks that face is marked as *Bold*: `OS/2` `fsSelection`, or the
    /// `head` table's `macStyle` when the face has no `OS/2` table.
    #[inline]
    pub fn is_bold(&self) -> bool {
        self.tables
            .os2
            .map_or_else(|| self.tables.head.mac_bold(), |os2| os2.is_bold())
    }

    /// Checks that face is marked as *Oblique*.
    ///
    /// Returns `false` when OS/2 table is not present or when its version is < 4.
    #[inline]
    pub fn is_oblique(&self) -> bool {
        self.style() == Style::Oblique
    }

    /// Returns face style: `OS/2`'s, or Italic from the `head` table's
    /// `macStyle` when the face has no `OS/2` table.
    #[inline]
    pub fn style(&self) -> Style {
        match self.tables.os2 {
            Some(os2) => os2.style(),
            None if self.tables.head.mac_italic() => Style::Italic,
            None => Style::default(),
        }
    }

    /// Returns face's weight: `OS/2`'s, or Bold from the `head` table's
    /// `macStyle` when the face has no `OS/2` table (else `Weight::Normal`).
    #[inline]
    pub fn weight(&self) -> Weight {
        match self.tables.os2 {
            Some(os2) => os2.weight(),
            None if self.tables.head.mac_bold() => Weight::Bold,
            None => Weight::default(),
        }
    }

    /// Returns face's width.
    ///
    /// Returns `Width::Normal` when OS/2 table is not present or when value is invalid.
    #[inline]
    pub fn width(&self) -> Width {
        self.tables.os2.map(|os2| os2.width()).unwrap_or_default()
    }

    // Read https://github.com/freetype/freetype/blob/49270c17011491227ec7bd3fb73ede4f674aa065/src/sfnt/sfobjs.c#L1279
    // to learn more about the logic behind the following functions.

    /// Returns a horizontal face ascender: OS/2's typographic ascender when
    /// `USE_TYPO_METRICS` is set, else `hhea`'s (OS/2's when that is 0),
    /// with the `MVAR` delta at the current instance.
    #[inline]
    pub fn ascender(&self) -> i16 {
        if let Some(os_2) = self.tables.os2 {
            if os_2.use_typographic_metrics() {
                let value = os_2.typographic_ascender();
                return self.apply_metrics_variation(Tag::from_bytes(b"hasc"), value);
            }
        }

        let mut value = self.tables.hhea.ascender;
        if value == 0 {
            if let Some(os_2) = self.tables.os2 {
                value = os_2.typographic_ascender();
                if value == 0 {
                    value = os_2.windows_ascender();
                    value = self.apply_metrics_variation(Tag::from_bytes(b"hcla"), value);
                } else {
                    value = self.apply_metrics_variation(Tag::from_bytes(b"hasc"), value);
                }
            }
        }

        value
    }

    /// Returns a horizontal face descender, chosen as [`Face::ascender`] is.
    #[inline]
    pub fn descender(&self) -> i16 {
        if let Some(os_2) = self.tables.os2 {
            if os_2.use_typographic_metrics() {
                let value = os_2.typographic_descender();
                return self.apply_metrics_variation(Tag::from_bytes(b"hdsc"), value);
            }
        }

        let mut value = self.tables.hhea.descender;
        if value == 0 {
            if let Some(os_2) = self.tables.os2 {
                value = os_2.typographic_descender();
                if value == 0 {
                    value = os_2.windows_descender();
                    value = self.apply_metrics_variation(Tag::from_bytes(b"hcld"), value);
                } else {
                    value = self.apply_metrics_variation(Tag::from_bytes(b"hdsc"), value);
                }
            }
        }

        value
    }

    /// Returns face's height: ascender minus descender. Wrapping, as
    /// upstream's release build computes it, so a malformed pair cannot
    /// overflow.
    #[inline]
    pub fn height(&self) -> i16 {
        self.ascender().wrapping_sub(self.descender())
    }

    /// Returns a horizontal face line gap, chosen as [`Face::ascender`] is.
    #[inline]
    pub fn line_gap(&self) -> i16 {
        if let Some(os_2) = self.tables.os2 {
            if os_2.use_typographic_metrics() {
                let value = os_2.typographic_line_gap();
                return self.apply_metrics_variation(Tag::from_bytes(b"hlgp"), value);
            }
        }

        let mut value = self.tables.hhea.line_gap;
        // For line gap, we have to check that ascender or descender are 0, not line gap itself.
        if self.tables.hhea.ascender == 0 || self.tables.hhea.descender == 0 {
            if let Some(os_2) = self.tables.os2 {
                if os_2.typographic_ascender() != 0 || os_2.typographic_descender() != 0 {
                    value = os_2.typographic_line_gap();
                    value = self.apply_metrics_variation(Tag::from_bytes(b"hlgp"), value);
                } else {
                    value = 0;
                }
            }
        }

        value
    }

    /// Returns OS/2's typographic ascender at the current instance.
    ///
    /// Returns `None` when OS/2 table is not present.
    #[inline]
    pub fn typographic_ascender(&self) -> Option<i16> {
        self.tables.os2.map(|table| {
            let v = table.typographic_ascender();
            self.apply_metrics_variation(Tag::from_bytes(b"hasc"), v)
        })
    }

    /// Returns OS/2's typographic descender at the current instance.
    ///
    /// Returns `None` when OS/2 table is not present.
    #[inline]
    pub fn typographic_descender(&self) -> Option<i16> {
        self.tables.os2.map(|table| {
            let v = table.typographic_descender();
            self.apply_metrics_variation(Tag::from_bytes(b"hdsc"), v)
        })
    }

    /// Returns OS/2's typographic line gap at the current instance.
    ///
    /// Returns `None` when OS/2 table is not present.
    #[inline]
    pub fn typographic_line_gap(&self) -> Option<i16> {
        self.tables.os2.map(|table| {
            let v = table.typographic_line_gap();
            self.apply_metrics_variation(Tag::from_bytes(b"hlgp"), v)
        })
    }

    /// Returns a vertical face ascender (`vhea`) at the current instance.
    #[inline]
    pub fn vertical_ascender(&self) -> Option<i16> {
        self.tables
            .vhea
            .map(|vhea| vhea.ascender)
            .map(|v| self.apply_metrics_variation(Tag::from_bytes(b"vasc"), v))
    }

    /// Returns a vertical face descender (`vhea`) at the current instance.
    #[inline]
    pub fn vertical_descender(&self) -> Option<i16> {
        self.tables
            .vhea
            .map(|vhea| vhea.descender)
            .map(|v| self.apply_metrics_variation(Tag::from_bytes(b"vdsc"), v))
    }

    /// Returns a vertical face height, wrapping as [`Face::height`] does.
    #[inline]
    pub fn vertical_height(&self) -> Option<i16> {
        Some(
            self.vertical_ascender()?
                .wrapping_sub(self.vertical_descender()?),
        )
    }

    /// Returns a vertical face line gap (`vhea`) at the current instance.
    #[inline]
    pub fn vertical_line_gap(&self) -> Option<i16> {
        self.tables
            .vhea
            .map(|vhea| vhea.line_gap)
            .map(|v| self.apply_metrics_variation(Tag::from_bytes(b"vlgp"), v))
    }

    /// Returns face's x height at the current instance.
    ///
    /// Returns `None` when OS/2 table is not present or when its version is < 2.
    #[inline]
    pub fn x_height(&self) -> Option<i16> {
        self.tables
            .os2
            .and_then(|os_2| os_2.x_height())
            .map(|v| self.apply_metrics_variation(Tag::from_bytes(b"xhgt"), v))
    }

    /// Returns face's capital height at the current instance.
    ///
    /// Returns `None` when OS/2 table is not present or when its version is < 2.
    #[inline]
    pub fn capital_height(&self) -> Option<i16> {
        self.tables
            .os2
            .and_then(|os_2| os_2.capital_height())
            .map(|v| self.apply_metrics_variation(Tag::from_bytes(b"cpht"), v))
    }

    /// Returns face's underline metrics (`post`) at the current instance.
    ///
    /// Returns `None` when `post` table is not present.
    #[inline]
    pub fn underline_metrics(&self) -> Option<LineMetrics> {
        let mut metrics = self.tables.post?.underline_metrics;

        if self.is_variable() {
            self.apply_metrics_variation_to(Tag::from_bytes(b"undo"), &mut metrics.position);
            self.apply_metrics_variation_to(Tag::from_bytes(b"unds"), &mut metrics.thickness);
        }

        Some(metrics)
    }

    /// Returns face's strikeout metrics (OS/2) at the current instance.
    ///
    /// Returns `None` when OS/2 table is not present.
    #[inline]
    pub fn strikeout_metrics(&self) -> Option<LineMetrics> {
        let mut metrics = self.tables.os2?.strikeout_metrics();

        if self.is_variable() {
            self.apply_metrics_variation_to(Tag::from_bytes(b"stro"), &mut metrics.position);
            self.apply_metrics_variation_to(Tag::from_bytes(b"strs"), &mut metrics.thickness);
        }

        Some(metrics)
    }

    /// Returns face's subscript metrics (OS/2) at the current instance.
    ///
    /// Returns `None` when OS/2 table is not present.
    #[inline]
    pub fn subscript_metrics(&self) -> Option<ScriptMetrics> {
        let mut metrics = self.tables.os2?.subscript_metrics();

        if self.is_variable() {
            self.apply_metrics_variation_to(Tag::from_bytes(b"sbxs"), &mut metrics.x_size);
            self.apply_metrics_variation_to(Tag::from_bytes(b"sbys"), &mut metrics.y_size);
            self.apply_metrics_variation_to(Tag::from_bytes(b"sbxo"), &mut metrics.x_offset);
            self.apply_metrics_variation_to(Tag::from_bytes(b"sbyo"), &mut metrics.y_offset);
        }

        Some(metrics)
    }

    /// Returns face's superscript metrics (OS/2) at the current instance.
    ///
    /// Returns `None` when OS/2 table is not present.
    #[inline]
    pub fn superscript_metrics(&self) -> Option<ScriptMetrics> {
        let mut metrics = self.tables.os2?.superscript_metrics();

        if self.is_variable() {
            self.apply_metrics_variation_to(Tag::from_bytes(b"spxs"), &mut metrics.x_size);
            self.apply_metrics_variation_to(Tag::from_bytes(b"spys"), &mut metrics.y_size);
            self.apply_metrics_variation_to(Tag::from_bytes(b"spxo"), &mut metrics.x_offset);
            self.apply_metrics_variation_to(Tag::from_bytes(b"spyo"), &mut metrics.y_offset);
        }

        Some(metrics)
    }

    /// Returns face permissions.
    ///
    /// Returns `None` in case of a malformed value.
    #[inline]
    pub fn permissions(&self) -> Option<Permissions> {
        self.tables.os2?.permissions()
    }

    /// Checks if the face allows embedding a subset, further restricted by [`Self::permissions`].
    #[inline]
    pub fn is_subsetting_allowed(&self) -> bool {
        self.tables
            .os2
            .map(|t| t.is_subsetting_allowed())
            .unwrap_or(false)
    }

    /// Checks if the face allows outline data to be embedded.
    #[inline]
    pub fn is_outline_embedding_allowed(&self) -> bool {
        self.tables
            .os2
            .map(|t| t.is_outline_embedding_allowed())
            .unwrap_or(false)
    }

    /// Returns [Unicode Ranges](https://docs.microsoft.com/en-us/typography/opentype/spec/os2#ur).
    #[inline]
    pub fn unicode_ranges(&self) -> UnicodeRanges {
        self.tables
            .os2
            .map(|t| t.unicode_ranges())
            .unwrap_or_default()
    }

    /// Returns face's units per EM, in 16..=16384.
    #[inline]
    #[must_use]
    pub const fn units_per_em(&self) -> u16 {
        self.tables.head.units_per_em
    }

    /// Returns a total number of glyphs in the face. Never zero.
    #[inline]
    #[must_use]
    pub const fn number_of_glyphs(&self) -> u16 {
        self.tables.maxp.number_of_glyphs.get()
    }

    /// Resolves a glyph ID for a code point through the face's Unicode
    /// `cmap` subtables, in table order.
    ///
    /// Returns `None` instead of `0` when glyph is not found.
    #[inline]
    pub fn glyph_index(&self, code_point: char) -> Option<GlyphId> {
        for subtable in self.tables.cmap?.subtables {
            if !subtable.is_unicode() {
                continue;
            }

            if let Some(id) = subtable.glyph_index(u32::from(code_point)) {
                return Some(id);
            }
        }

        None
    }

    /// Resolves a variation of a glyph ID from two code points through the
    /// first format 14 subtable ([Unicode Variation Sequences](
    /// https://docs.microsoft.com/en-us/typography/opentype/spec/cmap#format-14-unicode-variation-sequences)).
    ///
    /// Returns `None` instead of `0` when glyph is not found.
    #[inline]
    pub fn glyph_variation_index(&self, code_point: char, variation: char) -> Option<GlyphId> {
        for subtable in self.tables.cmap?.subtables {
            if let cmap::Format::UnicodeVariationSequences(ref table) = subtable.format {
                return match table.glyph_index(u32::from(code_point), u32::from(variation))? {
                    cmap::GlyphVariationResult::Found(v) => Some(v),
                    cmap::GlyphVariationResult::UseDefault => self.glyph_index(code_point),
                };
            }
        }

        None
    }

    /// Returns glyph's horizontal advance at the current instance: `hmtx`
    /// plus the `HVAR` delta (else the `gvar` phantom point), rounded.
    #[inline]
    pub fn glyph_hor_advance(&self, glyph_id: GlyphId) -> Option<u16> {
        let mut advance = self.tables.hmtx?.advance(glyph_id)? as f32;

        if self.is_variable() {
            // Ignore variation offset when `hvar` is not set.
            if let Some(hvar) = self.tables.hvar {
                if let Some(offset) = hvar.advance_offset(glyph_id, self.coords()) {
                    // Upstream rounds as `+ 0.5` and a truncating cast.
                    advance += offset + 0.5;
                }
            } else if let Some(points) = self.glyph_phantom_points(glyph_id) {
                advance += points.right.x + 0.5
            }
        }

        u16::try_num_from(advance)
    }

    /// Returns glyph's vertical advance at the current instance: `vmtx`
    /// plus the `VVAR` delta (else the `gvar` phantom point), rounded.
    #[inline]
    pub fn glyph_ver_advance(&self, glyph_id: GlyphId) -> Option<u16> {
        let mut advance = self.tables.vmtx?.advance(glyph_id)? as f32;

        if self.is_variable() {
            // Ignore variation offset when `vvar` is not set.
            if let Some(vvar) = self.tables.vvar {
                if let Some(offset) = vvar.advance_offset(glyph_id, self.coords()) {
                    advance += offset + 0.5;
                }
            } else if let Some(points) = self.glyph_phantom_points(glyph_id) {
                advance += points.bottom.y + 0.5
            }
        }

        u16::try_num_from(advance)
    }

    /// Returns glyph's horizontal side bearing at the current instance.
    #[inline]
    pub fn glyph_hor_side_bearing(&self, glyph_id: GlyphId) -> Option<i16> {
        let mut bearing = self.tables.hmtx?.side_bearing(glyph_id)? as f32;

        if self.is_variable() {
            // Ignore variation offset when `hvar` is not set.
            if let Some(hvar) = self.tables.hvar {
                if let Some(offset) = hvar.left_side_bearing_offset(glyph_id, self.coords()) {
                    bearing += offset + 0.5;
                }
            }
        }

        i16::try_num_from(bearing)
    }

    /// Returns glyph's vertical side bearing at the current instance.
    #[inline]
    pub fn glyph_ver_side_bearing(&self, glyph_id: GlyphId) -> Option<i16> {
        let mut bearing = self.tables.vmtx?.side_bearing(glyph_id)? as f32;

        if self.is_variable() {
            // Ignore variation offset when `vvar` is not set.
            if let Some(vvar) = self.tables.vvar {
                if let Some(offset) = vvar.top_side_bearing_offset(glyph_id, self.coords()) {
                    bearing += offset + 0.5;
                }
            }
        }

        i16::try_num_from(bearing)
    }

    /// Returns glyph's vertical origin according to the
    /// [Vertical Origin Table](https://docs.microsoft.com/en-us/typography/opentype/spec/vorg),
    /// at the current instance.
    #[inline]
    pub fn glyph_y_origin(&self, glyph_id: GlyphId) -> Option<i16> {
        let mut origin = self.tables.vorg.map(|vorg| vorg.glyph_y_origin(glyph_id))? as f32;

        if self.is_variable() {
            // Ignore variation offset when `vvar` is not set.
            if let Some(vvar) = self.tables.vvar {
                if let Some(offset) = vvar.vertical_origin_offset(glyph_id, self.coords()) {
                    origin += offset + 0.5;
                }
            }
        }

        i16::try_num_from(origin)
    }

    /// Outlines a glyph at the current instance into `sink`, in font units,
    /// and returns its tight bounding box.
    ///
    /// `gvar` (with `glyf`), then `glyf`, then `CFF `, then `CFF2`: the first
    /// table the face has answers. `None` when the glyph has no outline or
    /// its data is malformed; a malformed glyph may have sent part of its
    /// outline to `sink` first, exactly as ttf-parser does.
    /// A query also refuses after 65,536 cumulative glyph-entry, TrueType
    /// point or CFF-instruction steps, shared across component/subroutine
    /// calls: a small acyclic font program cannot expand without a bound.
    #[inline]
    pub fn outline_glyph(&self, glyph_id: GlyphId, sink: &mut dyn OutlineSink) -> Option<Rect> {
        if let Some(ref gvar) = self.tables.gvar {
            return gvar.outline(self.tables.glyf?, self.coords(), glyph_id, sink);
        }

        if let Some(table) = self.tables.glyf {
            return table.outline(glyph_id, sink);
        }

        if let Some(ref cff) = self.tables.cff {
            return cff.outline(glyph_id, sink).ok();
        }

        if let Some(ref cff2) = self.tables.cff2 {
            return cff2.outline(self.coords(), glyph_id, sink).ok();
        }

        None
    }

    /// Outlines a glyph at the current instance into `sink` as points: the
    /// on- and off-curve points of each contour as the font stores them
    /// (`glyf`: TrueType's quadratic points, composite components
    /// transformed; CFF: each segment's control and end points), and returns
    /// the bounding box [`Face::outline_glyph`] would.
    #[inline]
    pub fn outline_glyph_points(
        &self,
        glyph_id: GlyphId,
        sink: &mut dyn PointSink,
    ) -> Option<Rect> {
        if self.tables.gvar.is_some() || self.tables.glyf.is_some() {
            let mut null = sink::NullSink;
            return if let Some(ref gvar) = self.tables.gvar {
                gvar.outline_tapped(self.tables.glyf?, self.coords(), glyph_id, &mut null, sink)
            } else {
                self.tables.glyf?.outline_tapped(glyph_id, &mut null, sink)
            };
        }
        let mut adapter = sink::SegmentPoints::new(sink);
        let bbox = self.outline_glyph(glyph_id, &mut adapter);
        adapter.finish();
        bbox
    }

    /// Returns a tight glyph bounding box at the current instance, computed
    /// from the outline (the `glyf` header boxes are ignored: they can be
    /// malformed, and a variation moves them).
    #[inline]
    pub fn glyph_bounding_box(&self, glyph_id: GlyphId) -> Option<Rect> {
        self.outline_glyph(glyph_id, &mut sink::NullSink)
    }

    /// Returns a bounding box that large enough to enclose any glyph from the face.
    #[inline]
    #[must_use]
    pub const fn global_bounding_box(&self) -> Rect {
        self.tables.head.global_bbox
    }

    /// Returns a glyph's raster image: the strike closest to
    /// `pixels_per_em` from below, else the next larger one (`u16::MAX`
    /// picks the largest).
    ///
    /// Tables are read in the order `sbix`, `bloc` + `bdat`, `EBLC` +
    /// `EBDT`, `CBLC` + `CBDT`; the first one the face has answers. The image
    /// is returned encoded, unvalidated.
    #[inline]
    pub fn glyph_raster_image(
        &self,
        glyph_id: GlyphId,
        pixels_per_em: u16,
    ) -> Option<RasterGlyphImage<'a>> {
        if let Some(table) = self.tables.sbix {
            if let Some(strike) = table.best_strike(pixels_per_em) {
                return strike.get(glyph_id);
            }
        }
        if let Some(bdat) = self.tables.bdat {
            return bdat.get(glyph_id, pixels_per_em);
        }

        if let Some(ebdt) = self.tables.ebdt {
            return ebdt.get(glyph_id, pixels_per_em);
        }

        if let Some(cbdt) = self.tables.cbdt {
            return cbdt.get(glyph_id, pixels_per_em);
        }

        None
    }

    /// Returns `true` if the glyph can be painted from the `COLR`+`CPAL`
    /// tables.
    #[inline]
    pub fn is_color_glyph(&self, glyph_id: GlyphId) -> bool {
        self.tables()
            .colr
            .map(|colr| colr.contains(glyph_id))
            .unwrap_or(false)
    }

    /// Returns the number of palettes stored in the `COLR`+`CPAL` tables.
    #[inline]
    pub fn color_palettes(&self) -> Option<core::num::NonZeroU16> {
        Some(self.tables().colr?.palettes.palettes())
    }

    /// Paints a color glyph from the `COLR` table at the current instance,
    /// with `palette` (0 is the default) and `foreground_color` for the
    /// foreground palette index.
    ///
    /// Returns `None` if the glyph has no `COLR` definition or if the glyph
    /// definition is malformed.
    #[inline]
    pub fn paint_color_glyph(
        &self,
        glyph_id: GlyphId,
        palette: u16,
        foreground_color: RgbaColor,
        painter: &mut dyn colr::Painter<'a>,
    ) -> Option<()> {
        self.tables
            .colr?
            .paint(glyph_id, palette, painter, self.coords(), foreground_color)
    }

    /// Returns the variation axes (`fvar`), empty for a static face.
    #[inline]
    pub fn variation_axes(&self) -> LazyArray16<'a, VariationAxis> {
        self.tables.fvar.map(|fvar| fvar.axes).unwrap_or_default()
    }

    /// Sets a variation axis coordinate, in user space: normalized by `fvar`,
    /// then mapped by `avar`.
    ///
    /// Returns `None` when face is not variable, has more than 64 axes, or
    /// doesn't have such axis.
    pub fn set_variation(&mut self, axis: Tag, value: f32) -> Option<()> {
        if !self.is_variable() {
            return None;
        }

        if usize::from(self.variation_axes().len()) >= MAX_VAR_COORDS {
            return None;
        }

        for (i, var_axis) in self.variation_axes().into_iter().enumerate() {
            if var_axis.tag == axis {
                if let Some(slot) = self.coordinates.as_mut_slice().get_mut(i) {
                    *slot = var_axis.normalized_value(value);
                }

                if let Some(avar) = self.tables.avar {
                    let _ = avar.map_coordinate(self.coordinates.as_mut_slice(), i);
                }
            }
        }

        Some(())
    }

    /// Sets the normalized coordinates directly, in axis order: the
    /// autohinter's CFF path, which normalizes in 16.16 as skrifa does
    /// (`autohint/location.rs`). Coordinates past the face's axes are
    /// ignored; axes past the coordinates sit at their default.
    #[cfg(any(feature = "hint", debug_assertions))]
    pub(crate) fn set_normalized_coordinates(
        &mut self,
        coords: impl IntoIterator<Item = NormalizedCoordinate>,
    ) {
        let slots = self.coordinates.as_mut_slice();
        slots.fill(NormalizedCoordinate::default());
        for (slot, coord) in slots.iter_mut().zip(coords) {
            *slot = coord;
        }
    }

    /// Returns the current normalized variation coordinates.
    #[inline]
    pub fn variation_coordinates(&self) -> &[NormalizedCoordinate] {
        self.coordinates.as_slice()
    }

    /// Checks that face has non-default variation coordinates.
    #[inline]
    pub fn has_non_default_variation_coordinates(&self) -> bool {
        self.coordinates.as_slice().iter().any(|c| c.0 != 0)
    }

    /// Parses glyph's phantom points.
    ///
    /// Available only for variable fonts with the `gvar` table.
    #[inline]
    pub fn glyph_phantom_points(&self, glyph_id: GlyphId) -> Option<PhantomPoints> {
        let glyf = self.tables.glyf?;
        let gvar = self.tables.gvar?;
        gvar.phantom_points(glyf, self.coords(), glyph_id)
    }

    #[inline]
    fn metrics_var_offset(&self, tag: Tag) -> f32 {
        self.tables
            .mvar
            .and_then(|table| table.metric_offset(tag, self.coords()))
            .unwrap_or(0.0)
    }

    #[inline]
    fn apply_metrics_variation(&self, tag: Tag, mut value: i16) -> i16 {
        self.apply_metrics_variation_to(tag, &mut value);
        value
    }

    /// Adds the `MVAR` delta for `tag` at the current instance, truncated
    /// toward zero as upstream does; a sum outside i16 leaves `value` as is.
    #[inline]
    fn apply_metrics_variation_to(&self, tag: Tag, value: &mut i16) {
        if self.is_variable() {
            let v = f32::from(*value) + self.metrics_var_offset(tag);
            if let Some(v) = i16::try_num_from(v) {
                *value = v;
            }
        }
    }

    #[inline]
    fn coords(&self) -> &[NormalizedCoordinate] {
        self.coordinates.as_slice()
    }
}

impl core::fmt::Debug for Face<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Face()")
    }
}

/// Returns the number of fonts stored in a TrueType font collection.
///
/// Returns `None` if a provided data is not a TrueType font collection.
#[inline]
pub fn fonts_in_collection(data: &[u8]) -> Option<u32> {
    let mut s = Stream::new(data);
    if s.read::<Magic>()? != Magic::FontCollection {
        return None;
    }

    s.skip::<u32>(); // version
    s.read::<u32>()
}
