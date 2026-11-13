// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]

//! Headless, deterministic CPU terminal rasterizer turning [`RenderInput`] into an RGBA framebuffer.

use aterm_hash::{FxHashMap, FxHashSet, KeyedHashMap};
use emoji_cluster::ClusterShape;

use aterm_core::grid::LineSize;
pub use aterm_core::render::DefaultBgSpan;
pub use aterm_core::render::{BandIcon, ChromeIcon, ChromeRaster, ChromeRing, InkSplit};
use aterm_core::render::{EffectStreamDamage, LineSizeSpan};
use aterm_core::render::{FxBlend, FxInk, FxSlot};
use aterm_core::terminal::{CursorStyle, RenderCell, UnderlineStyle};
use fx_raster::Knock;
use std::sync::Arc;

#[cfg(trust_verify)]
macro_rules! trust_contract_fn {
    (
        $(#[$meta:meta])*
        $vis:vis fn $name:ident($($arg:ident : $arg_ty:ty),* $(,)?) -> $ret:ident
        clauses [$($clauses:tt)+]
        { $($body:tt)* }
    ) => {
        $(#[$meta])*
        $vis fn $name($($arg: $arg_ty),*) -> $ret
            $($clauses)+
        { $($body)* }
    };
}

#[cfg(not(trust_verify))]
macro_rules! trust_contract_fn {
    (
        $(#[$meta:meta])*
        $vis:vis fn $name:ident($($arg:ident : $arg_ty:ty),* $(,)?) -> $ret:ident
        clauses [$($clauses:tt)+]
        { $($body:tt)* }
    ) => {
        $(#[$meta])*
        $vis fn $name($($arg: $arg_ty),*) -> $ret { $($body)* }
    };
}

#[cfg(trust_verify)]
macro_rules! verify_island {
    ($($tt:tt)*) => {
        $($tt)*
    };
}

#[cfg(not(trust_verify))]
macro_rules! verify_island {
    ($($tt:tt)*) => {};
}

pub mod apron;
pub mod band;
pub mod bloom;
pub mod bundled;
pub mod capture_pixels;
pub mod chrome_metrics;
mod colr;
pub mod deco;
mod emoji_cluster;
pub mod fire_field;
pub mod font;
pub mod font_catalog;
pub mod font_chain;
pub mod font_file;
pub mod fx;
mod fx_damage;
mod fx_raster;
pub mod glow_ramps;
pub mod glyph_ink;
pub mod hdr;
#[cfg(any(
    all(unix, not(target_os = "macos")),
    windows,
    all(test, feature = "embedded-font", target_os = "macos")
))]
#[cfg_attr(all(test, target_os = "macos"), allow(dead_code))]
mod hinted;
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub mod legacy_sprite_oracle;
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub mod letter_judge;
pub mod ligature_shaping;
#[cfg(all(test, windows, feature = "embedded-font"))]
mod no_ligatures;
pub mod procedural;
/// The scanline signed-area coverage rasterizer every outline fill in this crate uses.
pub mod raster;
pub mod rounded_bar;
pub mod scroll_translate;
pub mod spill;
pub mod sprites;
#[cfg(any(
    all(unix, not(target_os = "macos")),
    all(test, feature = "embedded-font", target_os = "macos")
))]
#[cfg_attr(all(test, target_os = "macos"), allow(dead_code))]
mod subpixel;
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub mod test_counters;
pub mod variation;
pub mod vibrancy;
#[cfg(windows)]
mod win_locale;

pub use apron::ApronScratch;
pub use fx_damage::RowBands;
/// The fx engine's one slicer and its bloom source (`fx_raster`), shared with the GPU renderer so the band gate, the clips and the occluders are one statement on every backend.
pub use fx_raster::{
    FxBudget, FxFrame, FxSlice, FxSliceSink, FxSlicer, KnockRect, SpriteCols,
    for_each_drawn_sprite, fx_bloom_quads, knock_rects, slice_layers,
};
pub use glow_ramps::{BeamRows, RampCost, RampSink};
pub use glyph_ink::{GlyphCoverage, GlyphInk, GlyphRaster};
pub use spill::SpillBand;

pub use deco::{
    DECO_ATLAS_MAX_DIM, DECO_ATLAS_SPRITES, DecoMetrics, DecoTables, UNDERCURL_SPRITE,
    resolve_deco_metrics, undercurl_band, undercurl_coverage, undercurl_supported,
    undercurl_tile_col, underline_band,
};

pub use aterm_types::text_shaping::{
    AmbiguousWidth, FontFeature, FontFeatureSet, LigatureMode, TextShapingConfig,
    parse_font_features,
};
pub use ligature_shaping::{ColumnGlyph, ShapedRun};

type InternedFace = (crate::font::FaceBytes, std::sync::Arc<crate::font::Font>);

type FallbackParseRx = std::sync::mpsc::Receiver<Vec<FallbackFace>>;

thread_local! {
    static FONT_BYTES_INTERN: std::cell::RefCell<Vec<std::sync::Weak<Vec<u8>>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

static PARSED_FONT_INTERN: std::sync::Mutex<Vec<std::sync::Weak<crate::font::Font>>> =
    std::sync::Mutex::new(Vec::new());

/// Return a shared `Arc` for `bytes`, reusing an identical interned blob.
pub fn intern_font_bytes(bytes: Vec<u8>) -> std::sync::Arc<Vec<u8>> {
    FONT_BYTES_INTERN.with(|cell| {
        let mut store = cell.borrow_mut();
        store.retain(|entry| entry.strong_count() != 0);
        if let Some(existing) = store
            .iter()
            .filter_map(std::sync::Weak::upgrade)
            .find(|a| a.len() == bytes.len() && a.as_slice() == bytes.as_slice())
        {
            return existing;
        }
        let arc = std::sync::Arc::new(bytes);
        store.push(std::sync::Arc::downgrade(&arc));
        arc
    })
}

/// Borrowed-slice twin of [`intern_font_bytes`].
pub fn intern_font_bytes_slice(bytes: &[u8]) -> std::sync::Arc<Vec<u8>> {
    FONT_BYTES_INTERN.with(|cell| {
        let mut store = cell.borrow_mut();
        store.retain(|entry| entry.strong_count() != 0);
        if let Some(existing) = store
            .iter()
            .filter_map(std::sync::Weak::upgrade)
            .find(|a| a.len() == bytes.len() && a.as_slice() == bytes)
        {
            return existing;
        }
        let arc = std::sync::Arc::new(bytes.to_vec());
        store.push(std::sync::Arc::downgrade(&arc));
        arc
    })
}

/// [`Renderer::take_missing_font_classes`] bit.
pub const MISSING_FONT_CLASS_TEXT: u8 = 1 << 0;

/// [`Renderer::take_missing_font_classes`] bit.
pub const MISSING_FONT_CLASS_EMOJI: u8 = 1 << 1;

static DISCOVERED_FONT_BYTES: std::sync::Mutex<Vec<crate::font::WeakFaceBytes>> =
    std::sync::Mutex::new(Vec::new());

fn intern_discovered_font_bytes(bytes: &crate::font::FaceBytes) -> crate::font::FaceBytes {
    let mut store = DISCOVERED_FONT_BYTES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    store.retain(crate::font::WeakFaceBytes::is_live);
    if let Some(existing) = store
        .iter()
        .filter_map(crate::font::WeakFaceBytes::upgrade)
        .find(|a| a.same_bytes(bytes))
    {
        return existing;
    }
    store.push(bytes.downgrade());
    bytes.clone()
}

#[cfg(all(test, feature = "embedded-font"))]
fn discovered_font_bytes_contains(bytes: &[u8]) -> bool {
    DISCOVERED_FONT_BYTES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter_map(crate::font::WeakFaceBytes::upgrade)
        .any(|held| held[..] == *bytes)
}

#[cfg(test)]
const NO_FONT: &str = "no font: build with aterm-render's default embedded-font feature";

fn intern_parsed_font(bytes: &[u8]) -> Result<InternedFace, String> {
    intern_parsed_font_keyed(bytes, || {
        crate::font::FaceBytes::Vec(std::sync::Arc::new(bytes.to_vec()))
    })
}

fn intern_parsed_font_owned(
    bytes: &crate::font::FaceBytes,
) -> Result<std::sync::Arc<crate::font::Font>, String> {
    intern_parsed_font_keyed(bytes, || bytes.clone()).map(|(_, font)| font)
}

fn intern_parsed_font_keyed(
    bytes: &[u8],
    key: impl FnOnce() -> crate::font::FaceBytes,
) -> Result<InternedFace, String> {
    if let Some(face) = interned_parsed_font_lookup(bytes) {
        return Ok(face);
    }
    let src = key();
    let parsed = crate::font::Font::from_shared(src.clone(), crate::font::FontSettings::default())
        .map_err(|e| e.to_string())?;
    let font = std::sync::Arc::new(parsed);
    let mut store = PARSED_FONT_INTERN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(existing) = interned_parsed_font_lookup_in(&mut store, bytes) {
        return Ok(existing);
    }
    store.push(std::sync::Arc::downgrade(&font));
    Ok((src, font))
}

fn same_slice(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && (std::ptr::eq(a.as_ptr(), b.as_ptr()) || a == b)
}

fn interned_parsed_font_lookup(bytes: &[u8]) -> Option<InternedFace> {
    let mut store = PARSED_FONT_INTERN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    interned_parsed_font_lookup_in(&mut store, bytes)
}

fn interned_parsed_font_lookup_in(
    store: &mut Vec<std::sync::Weak<crate::font::Font>>,
    bytes: &[u8],
) -> Option<InternedFace> {
    store.retain(|entry| entry.strong_count() != 0);
    store
        .iter()
        .filter_map(std::sync::Weak::upgrade)
        .find(|font| same_slice(font.source_bytes(), bytes))
        .map(|font| (font.source_bytes().clone(), font))
}

struct SharedFace {
    bytes: std::sync::Arc<[u8]>,
    index: u32,
    font: std::sync::Weak<crate::font::Font>,
}

static SHARED_PARSED_FACES: std::sync::Mutex<Vec<SharedFace>> = std::sync::Mutex::new(Vec::new());

/// The parsed face for `bytes` at `index`, reusing a live parse of the identical bytes when one exists.
pub fn shared_parsed_face(
    bytes: &[u8],
    index: u32,
) -> Result<(std::sync::Arc<[u8]>, std::sync::Arc<crate::font::Font>), String> {
    if let Some(hit) = shared_parsed_face_lookup(bytes, index) {
        return Ok(hit);
    }
    let owned: std::sync::Arc<[u8]> = std::sync::Arc::from(bytes);
    let font = parse_face_at(&owned, index)?;
    Ok(shared_parsed_face_publish(
        bytes,
        || owned.clone(),
        index,
        font,
    ))
}

fn shared_parsed_face_owned(
    bytes: &std::sync::Arc<[u8]>,
    index: u32,
) -> Result<std::sync::Arc<crate::font::Font>, String> {
    if let Some((_, font)) = shared_parsed_face_lookup(bytes, index) {
        return Ok(font);
    }
    let font = parse_face_at(bytes, index)?;
    Ok(shared_parsed_face_publish(bytes, || bytes.clone(), index, font).1)
}

fn parse_face_at(
    bytes: &std::sync::Arc<[u8]>,
    index: u32,
) -> Result<std::sync::Arc<crate::font::Font>, String> {
    crate::font::Font::from_shared_slice(
        bytes.clone(),
        crate::font::FontSettings {
            collection_index: index,
        },
    )
    .map(std::sync::Arc::new)
    .map_err(|e| e.to_string())
}

fn shared_face_bytes(bytes: &[u8], index: u32) -> std::sync::Arc<[u8]> {
    let mut store = SHARED_PARSED_FACES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    shared_faces_prune(&mut store);
    if let Some(entry) = store
        .iter()
        .find(|entry| shared_face_is(entry, bytes, index))
    {
        return entry.bytes.clone();
    }
    let src: std::sync::Arc<[u8]> = std::sync::Arc::from(bytes);
    store.push(SharedFace {
        bytes: src.clone(),
        index,
        font: std::sync::Weak::new(),
    });
    src
}

fn shared_face_is(entry: &SharedFace, bytes: &[u8], index: u32) -> bool {
    entry.index == index && entry.bytes.len() == bytes.len() && &*entry.bytes == bytes
}

fn shared_faces_prune(store: &mut Vec<SharedFace>) {
    store.retain(|entry| {
        entry.font.strong_count() > 0 || std::sync::Arc::strong_count(&entry.bytes) > 1
    });
}

fn shared_parsed_face_lookup(
    bytes: &[u8],
    index: u32,
) -> Option<(std::sync::Arc<[u8]>, std::sync::Arc<crate::font::Font>)> {
    let mut store = SHARED_PARSED_FACES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    shared_faces_prune(&mut store);
    store.iter().find_map(|entry| {
        shared_face_is(entry, bytes, index)
            .then(|| entry.font.upgrade().map(|font| (entry.bytes.clone(), font)))
            .flatten()
    })
}

fn shared_parsed_face_publish(
    bytes: &[u8],
    key: impl FnOnce() -> std::sync::Arc<[u8]>,
    index: u32,
    font: std::sync::Arc<crate::font::Font>,
) -> (std::sync::Arc<[u8]>, std::sync::Arc<crate::font::Font>) {
    let mut store = SHARED_PARSED_FACES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(entry) = store
        .iter_mut()
        .find(|entry| shared_face_is(entry, bytes, index))
    {
        if let Some(live) = entry.font.upgrade() {
            return (entry.bytes.clone(), live);
        }
        entry.font = std::sync::Arc::downgrade(&font);
        return (entry.bytes.clone(), font);
    }
    let src = key();
    shared_faces_prune(&mut store);
    store.push(SharedFace {
        bytes: src.clone(),
        index,
        font: std::sync::Arc::downgrade(&font),
    });
    (src, font)
}

/// Colours as 0x00RR_GGBB.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Theme {
    pub fg: u32,
    pub bg: u32,
    pub cursor: u32,
    pub selection: u32,
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            fg: 0x00D0_D0D0,
            bg: 0x0011_1318,
            cursor: 0x0050_FA7B,
            selection: 0x0033_415E,
        }
    }
}

impl From<aterm_types::scheme::ThemeParts> for Theme {
    fn from(parts: aterm_types::scheme::ThemeParts) -> Self {
        let aterm_types::scheme::ThemeParts {
            fg,
            bg,
            cursor,
            selection,
        } = parts;
        Theme {
            fg,
            bg,
            cursor,
            selection,
        }
    }
}

impl From<&aterm_types::scheme::ColorScheme> for Theme {
    fn from(scheme: &aterm_types::scheme::ColorScheme) -> Self {
        Theme::from(scheme.to_theme_parts())
    }
}

#[cfg(test)]
mod cell_geometry_memo_tests;
#[cfg(test)]
mod color_chain_tests;
#[cfg(all(test, feature = "embedded-font"))]
mod combining_script_tests;
#[cfg(test)]
mod contrast_floor_tests;
#[cfg(all(test, target_os = "macos"))]
mod coretext_bridge_tests;
#[cfg(test)]
mod dark_nudge_grid_tests;
#[cfg(test)]
mod face_identity_tests;
#[cfg(test)]
mod named_instance_tests;
#[cfg(test)]
mod raster_tests;
#[cfg(test)]
mod ribbon_wedge_tests;
#[cfg(test)]
mod styled_instance_tests;
#[cfg(test)]
#[path = "lib_theme_from_scheme_tests.rs"]
mod theme_from_scheme_tests;

pub mod render_api;
pub use render_api::{
    CharFg, DecoBlend, DecoGlyph, FireHaloCell, FireMode, FirePatch, Frame, GlowBlend, GlowEnd,
    GlowQuad, GlowRamp, GlowRamps, HaloMode, InkCell, Laid, RainHalo, RampMark, Rasterizer,
    RenderInput, RenderView, SceneAtlas, SelectionClip, TrailCell, WordDecoration, end_alpha,
    end_color, glow_end,
};

/// Which glyph source a [`GlyphKey`] rasterizes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FaceId {
    /// The primary monospace face (also serves `.notdef` for misses).
    Primary,
    /// A real bold weight of the primary family injected by the host ([`Renderer::set_bold_font`]), for SGR-bold cells the primary covers.
    BoldPrimary,
    /// The broad-coverage Unicode fallback face (CJK, symbols, math).
    Fallback,
    /// A monochrome symbol fallback (e.g. STIX Two Math), after [`FaceId::Fallback`] and before [`FaceId::ColorEmoji`].
    SymbolFallback,
    /// No font at all: box-drawing / block / braille coverage synthesized from the cell geometry by [`procedural`].
    Procedural,
    /// Apple Color Emoji (`sbix` colour bitmaps).
    ColorEmoji,
    /// A colour glyph's `sbix` alpha silhouette drawn as foreground-tinted coverage (a [`GlyphClass::Mono`] image).
    ColorEmojiMono,
    /// A face of the active display layer (`display_font = "pixel+engraved"`, [`Renderer::set_display_faces`]).
    DisplayMix,
    /// A system font found at query time to cover a code point none of the configured faces carry.
    RuntimeFallback,
}

#[inline]
fn is_synthetic_symbol_key(key: GlyphKey) -> bool {
    key.source == FaceId::Procedural
        && key.glyph_class == GlyphClass::Mono
        && key.chr().is_some_and(procedural::covers_symbol)
}

#[inline]
const fn harmonized_fallback_source(source: FaceId) -> bool {
    matches!(
        source,
        FaceId::Fallback | FaceId::SymbolFallback | FaceId::RuntimeFallback
    )
}

#[inline]
const fn cell_fitted_source(source: FaceId) -> bool {
    harmonized_fallback_source(source) || matches!(source, FaceId::DisplayMix)
}

/// The pixel class of a glyph image.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GlyphClass {
    /// 8-bit alpha coverage, tinted by the cell foreground at blit time.
    Mono,
    /// 32-bit RGBA colour emoji by code point.
    Rgba,
    /// 32-bit RGBA colour emoji by colour-font glyph id.
    RgbaGid,
    /// 32-bit RGBA composite of a cluster the colour face shaped to several glyphs (an unligated ZWJ sequence or flag) fitted into two cells.
    RgbaCluster,
    /// 8-bit alpha coverage addressed by primary-face glyph ID (`ch_or_id` is an opaque primary-font glyph id, not a code point).
    MonoGid,
}

/// Style variant bits for a glyph, part of its cache identity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StyleBits(pub u8);

impl StyleBits {
    /// No style variation.
    pub const REGULAR: StyleBits = StyleBits(0);
    pub const BOLD: StyleBits = StyleBits(1 << 0);
    pub const ITALIC: StyleBits = StyleBits(1 << 1);
    /// A subscript cell (`Script::Sub`).
    pub const SUB: StyleBits = StyleBits(1 << 2);

    /// These bits without the script bit.
    #[inline]
    #[must_use]
    pub const fn without_script(self) -> StyleBits {
        StyleBits(self.0 & !Self::SUB.0)
    }

    /// Only the script bit.
    #[inline]
    #[must_use]
    pub const fn script(self) -> StyleBits {
        StyleBits(self.0 & Self::SUB.0)
    }

    /// Whether every bit of `other` is set in `self`.
    #[inline]
    pub const fn contains(self, other: StyleBits) -> bool {
        self.0 & other.0 == other.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ScalarPresentation {
    Default,
    Text,
}

/// Cache/atlas identity of one rasterized glyph image.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GlyphKey {
    pub source: FaceId,
    pub glyph_class: GlyphClass,
    /// The Unicode scalar for character glyphs.
    pub ch_or_id: u32,
    pub style: StyleBits,
    /// Logical cell span the raster is fitted into.
    pub cell_span: u8,
    /// Pixel size in 26.6 fixed point ([`GlyphKey::quantize_px`]), so the key stays `Eq + Hash` and one cache can host multiple sizes.
    pub px_q: u32,
}

impl GlyphKey {
    /// Quantize a pixel size into the key's 26.6 fixed-point form.
    #[inline]
    pub fn quantize_px(px: f32) -> u32 {
        (px * 64.0).round() as u32
    }

    /// Key for a coverage (Mono) glyph of character `ch` from `source`.
    #[inline]
    pub fn mono_char(source: FaceId, ch: char, style: StyleBits, px_q: u32) -> GlyphKey {
        GlyphKey {
            source,
            glyph_class: GlyphClass::Mono,
            ch_or_id: ch as u32,
            style,
            cell_span: if cell_fitted_source(source) {
                aterm_grapheme::char_width(ch).min(2) as u8
            } else {
                0
            },
            px_q,
        }
    }

    /// Key for a colour (Rgba) glyph of `ch` from the colour-emoji face.
    #[inline]
    pub const fn rgba_char(source: FaceId, ch: char, px_q: u32) -> GlyphKey {
        GlyphKey {
            source,
            glyph_class: GlyphClass::Rgba,
            ch_or_id: ch as u32,
            style: StyleBits::REGULAR,
            cell_span: 0,
            px_q,
        }
    }

    /// Key for a colour (RgbaGid) glyph by colour-font glyph id.
    #[inline]
    pub const fn rgba_gid(source: FaceId, gid: u16, px_q: u32) -> GlyphKey {
        GlyphKey {
            source,
            glyph_class: GlyphClass::RgbaGid,
            ch_or_id: gid as u32,
            style: StyleBits::REGULAR,
            cell_span: 0,
            px_q,
        }
    }

    /// [`Self::rgba_char`] of [`FaceId::ColorEmoji`] drawn by colour-chain face `face` ([`COLOUR_FACE_SHIFT`]).
    #[inline]
    pub const fn rgba_char_in(face: u8, ch: char, px_q: u32) -> GlyphKey {
        let key = Self::rgba_char(FaceId::ColorEmoji, ch, px_q);
        GlyphKey {
            ch_or_id: key.ch_or_id | ((face as u32) << COLOUR_FACE_SHIFT),
            ..key
        }
    }

    /// [`Self::rgba_gid`] of [`FaceId::ColorEmoji`] for a glyph id of colour-chain face `face` ([`COLOUR_FACE_SHIFT`]).
    #[inline]
    pub const fn rgba_gid_in(face: u8, gid: u16, px_q: u32) -> GlyphKey {
        let key = Self::rgba_gid(FaceId::ColorEmoji, gid, px_q);
        GlyphKey {
            ch_or_id: key.ch_or_id | ((face as u32) << COLOUR_FACE_SHIFT),
            ..key
        }
    }

    /// The colour-chain face an [`GlyphClass::Rgba`] or [`GlyphClass::RgbaGid`] key draws from ([`COLOUR_FACE_SHIFT`]).
    #[inline]
    #[must_use]
    pub const fn colour_face(&self) -> u8 {
        if matches!(self.glyph_class, GlyphClass::Rgba | GlyphClass::RgbaGid) {
            (self.ch_or_id >> COLOUR_FACE_SHIFT) as u8
        } else {
            0
        }
    }

    /// Key for the colour composite of cluster run `run` ([`GlyphClass::RgbaCluster`]).
    #[inline]
    pub const fn rgba_cluster(run: u32, px_q: u32) -> GlyphKey {
        GlyphKey {
            source: FaceId::ColorEmoji,
            glyph_class: GlyphClass::RgbaCluster,
            ch_or_id: run,
            style: StyleBits::REGULAR,
            cell_span: 0,
            px_q,
        }
    }

    /// Key for a coverage (MonoGid) glyph addressed by primary-face glyph id.
    #[inline]
    pub const fn mono_gid(gid: u16, style: StyleBits, px_q: u32) -> GlyphKey {
        GlyphKey {
            source: FaceId::Primary,
            glyph_class: GlyphClass::MonoGid,
            ch_or_id: gid as u32,
            style,
            cell_span: 0,
            px_q,
        }
    }

    /// Key for cell `slice` of a collapsed (Cascadia n:1) ligature of primary glyph `gid`.
    #[inline]
    pub const fn mono_gid_slice(gid: u16, slice: u16, style: StyleBits, px_q: u32) -> GlyphKey {
        GlyphKey {
            source: FaceId::Primary,
            glyph_class: GlyphClass::MonoGid,
            ch_or_id: (gid as u32) | ((slice as u32 + 1) << LIG_SLICE_SHIFT),
            style,
            cell_span: 0,
            px_q,
        }
    }

    /// The character this key rasterizes, when `ch_or_id` is a code point.
    #[inline]
    pub const fn chr(&self) -> Option<char> {
        match self.glyph_class {
            GlyphClass::RgbaGid | GlyphClass::RgbaCluster | GlyphClass::MonoGid => None,
            GlyphClass::Rgba => char::from_u32(self.ch_or_id & ((1 << COLOUR_FACE_SHIFT) - 1)),
            GlyphClass::Mono => char::from_u32(self.ch_or_id),
        }
    }
}

/// Where a colour key ([`GlyphClass::Rgba`], [`GlyphClass::RgbaGid`]) carries the index of the colour-chain face that draws it ([`Renderer::color_pick`]) in [`GlyphKey::ch_or_id`].
pub const COLOUR_FACE_SHIFT: u32 = 24;

#[inline]
fn qualify_text_presentation_key(mut key: GlyphKey) -> GlyphKey {
    if matches!(key.glyph_class, GlyphClass::Mono | GlyphClass::MonoGid) {
        key.cell_span = 1;
    }
    key
}

/// Bit in [`GlyphKey::ch_or_id`] where a collapsed-ligature slice index (`slice + 1`) sits above the 16-bit glyph id ([`GlyphKey::mono_gid_slice`]).
pub const LIG_SLICE_SHIFT: u32 = 16;

/// `GlyphKey::ch_or_id` bit for a shade cell whose left pixel column is odd (`(pad + col * cell_w) & 1`).
pub const SHADE_PHASE_X_BIT: u32 = 1 << 24;
/// The row twin of [`SHADE_PHASE_X_BIT`].
pub const SHADE_PHASE_Y_BIT: u32 = 1 << 25;

/// `GlyphKey::ch_or_id` bit of a drawn band icon ([`band_icon_key`]).
pub const CHROME_ICON_BIT: u32 = 1 << 26;

/// `GlyphKey::ch_or_id` bit of a flag drawn as its two letters ([`regional_pair_key`]).
pub const REGIONAL_PAIR_BIT: u32 = 1 << 27;

/// The key of a flag the colour face has no glyph for, drawn as its two regional-indicator letters in the primary face, one a cell (`🇯🇵` as `JP`), as Windows Terminal and Edge draw the flags Segoe UI Emoji lacks.
#[must_use]
pub fn regional_pair_key(cluster: &str, style: StyleBits, px_q: u32) -> Option<GlyphKey> {
    let mut letters = cluster.chars().map(|c| match c {
        '\u{1F1E6}'..='\u{1F1FF}' => Some(c as u32 - 0x1F1E6),
        _ => None,
    });
    let (Some(Some(a)), Some(Some(b)), None) = (letters.next(), letters.next(), letters.next())
    else {
        return None;
    };
    Some(GlyphKey {
        source: FaceId::Procedural,
        glyph_class: GlyphClass::Mono,
        ch_or_id: REGIONAL_PAIR_BIT | (a << 5) | b,
        style: StyleBits(style.0 & (StyleBits::BOLD.0 | StyleBits::ITALIC.0)),
        cell_span: 0,
        px_q,
    })
}

/// `GlyphKey::ch_or_id` bit of a band icon that spills into the blank cell on each side of its own.
pub const CHROME_ICON_SPILL_BIT: u32 = 1 << 8;

/// Where a sub-script key ([`StyleBits::SUB`]) carries its cell's sideways nudge ([`Script::Sub`](aterm_core::terminal::Script::Sub)) in [`GlyphKey::ch_or_id`].
pub const SUB_NUDGE_SHIFT: u32 = 24;

/// An emoji-presented cell's colour key under its `script`: a [`Script::Sub`](aterm_core::terminal::Script::Sub) one carries [`StyleBits::SUB`], which draws the emoji small in the rows [`Script::small_emoji_rows`](aterm_core::terminal::Script::small_emoji_rows) names (its nudge ignored); a key the colour face does not draw is left as it is.
#[inline]
#[must_use]
pub const fn small_emoji_keyed(key: GlyphKey, script: aterm_core::terminal::Script) -> GlyphKey {
    match script {
        aterm_core::terminal::Script::Sub { .. }
            if matches!(
                key.glyph_class,
                GlyphClass::Rgba | GlyphClass::RgbaGid | GlyphClass::RgbaCluster
            ) =>
        {
            GlyphKey {
                style: StyleBits(key.style.0 | StyleBits::SUB.0),
                ..key
            }
        }
        _ => key,
    }
}

/// Fold a [`Script::Sub`](aterm_core::terminal::Script::Sub) cell's sideways nudge into its glyph key, so each nudge is its own raster.
#[inline]
#[must_use]
pub const fn sub_nudged(key: GlyphKey, script: aterm_core::terminal::Script) -> GlyphKey {
    match script {
        aterm_core::terminal::Script::Sub { nudge }
            if nudge != 0 && key.style.script().0 != StyleBits::REGULAR.0 =>
        {
            GlyphKey {
                ch_or_id: key.ch_or_id | ((nudge.cast_unsigned() as u32) << SUB_NUDGE_SHIFT),
                ..key
            }
        }
        _ => key,
    }
}

/// Whether band icon `icon` in cell `c` of a frame row (`cells`) spills into its neighbours.
#[inline]
#[must_use]
pub fn chrome_icon_spills(icon: BandIcon, cells: &[RenderCell], c: usize) -> bool {
    let blank = |n: Option<&RenderCell>| n.is_some_and(|n| n.ch == ' ' && !n.wide);
    !icon.is_glass_mark() && c > 0 && blank(cells.get(c - 1)) && blank(cells.get(c + 1))
}

/// The glyph key of `icon` at size `px_q` in a cell of weight `style` (only [`StyleBits::BOLD`] is read), spilling into its blank neighbours when `spill` and `icon` is a badge: a glass mark's key is its cell's own whatever `spill` says ([`chrome_icon_spills`]).
#[inline]
#[must_use]
pub const fn band_icon_key(icon: BandIcon, style: StyleBits, spill: bool, px_q: u32) -> GlyphKey {
    let spill = spill && !icon.is_glass_mark();
    GlyphKey {
        source: FaceId::Procedural,
        glyph_class: GlyphClass::Mono,
        ch_or_id: CHROME_ICON_BIT | if spill { CHROME_ICON_SPILL_BIT } else { 0 } | icon.index(),
        style: if style.contains(StyleBits::BOLD) {
            StyleBits::BOLD
        } else {
            StyleBits::REGULAR
        },
        cell_span: 0,
        px_q,
    }
}

/// Fold a cell's absolute pixel-position parity into a procedural shade key (U+2591–2593).
#[inline]
#[must_use]
pub const fn shade_phase_key(key: GlyphKey, cell_px_x: usize, cell_px_y: usize) -> GlyphKey {
    if !matches!(key.source, FaceId::Procedural)
        || !matches!(key.glyph_class, GlyphClass::Mono)
        || key.ch_or_id < 0x2591
        || key.ch_or_id > 0x2593
    {
        return key;
    }
    let mut ch_or_id = key.ch_or_id;
    if cell_px_x & 1 == 1 {
        ch_or_id |= SHADE_PHASE_X_BIT;
    }
    if cell_px_y & 1 == 1 {
        ch_or_id |= SHADE_PHASE_Y_BIT;
    }
    GlyphKey { ch_or_id, ..key }
}

/// One rasterized glyph, the value a [`GlyphKey`] resolves to.
#[derive(Clone, Debug)]
pub enum GlyphImage {
    /// 8-bit alpha coverage, row-major, `width * height` bytes.
    Mono {
        width: usize,
        height: usize,
        xmin: i32,
        ymin: i32,
        advance: f32,
        bytes: Vec<u8>,
    },
    /// 32-bit RGBA colour, row-major, `width * height * 4` bytes (colour emoji).
    Rgba {
        width: usize,
        height: usize,
        xmin: i32,
        ymin: i32,
        advance: f32,
        bytes: Vec<u8>,
    },
}

impl GlyphImage {
    /// Stored bitmap bytes, used for the cache's image-payload budget.
    #[inline]
    #[must_use]
    pub fn byte_len(&self) -> usize {
        match self {
            GlyphImage::Mono { bytes, .. } | GlyphImage::Rgba { bytes, .. } => bytes.len(),
        }
    }

    #[inline]
    pub fn width(&self) -> usize {
        match self {
            GlyphImage::Mono { width, .. } | GlyphImage::Rgba { width, .. } => *width,
        }
    }

    #[inline]
    pub fn height(&self) -> usize {
        match self {
            GlyphImage::Mono { height, .. } | GlyphImage::Rgba { height, .. } => *height,
        }
    }

    #[inline]
    pub fn xmin(&self) -> i32 {
        match self {
            GlyphImage::Mono { xmin, .. } | GlyphImage::Rgba { xmin, .. } => *xmin,
        }
    }

    #[inline]
    pub fn ymin(&self) -> i32 {
        match self {
            GlyphImage::Mono { ymin, .. } | GlyphImage::Rgba { ymin, .. } => *ymin,
        }
    }

    #[inline]
    pub fn advance(&self) -> f32 {
        match self {
            GlyphImage::Mono { advance, .. } | GlyphImage::Rgba { advance, .. } => *advance,
        }
    }

    #[inline]
    #[must_use]
    /// The raw bitmap bytes (1 byte/texel for `Mono`, 4 for `Rgba`).
    pub fn bytes(&self) -> &[u8] {
        match self {
            GlyphImage::Mono { bytes, .. } | GlyphImage::Rgba { bytes, .. } => bytes,
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
#[derive(Clone, Debug)]
struct SubpxImage {
    width: usize,
    height: usize,
    xmin: i32,
    ymin: i32,
    bytes: Vec<u8>,
}

/// One glyph in a bounded semantic text run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SemanticGlyph {
    pub column: usize,
    pub key: GlyphKey,
}

type ShapedRunGlyphs = Option<std::sync::Arc<ShapedRun>>;

enum ShapedHit<'a> {
    Cur(&'a ShapedRunGlyphs),
    Prev(&'a ShapedRunGlyphs),
}

#[derive(Default)]
struct ShapedRunCache {
    cur: FxHashMap<StyleBits, KeyedHashMap<Box<str>, ShapedRunGlyphs>>,
    prev: FxHashMap<StyleBits, KeyedHashMap<Box<str>, ShapedRunGlyphs>>,
}

impl ShapedRunCache {
    #[inline]
    fn get(&self, style: StyleBits, run: &str) -> Option<ShapedHit<'_>> {
        if let Some(v) = self.cur.get(&style).and_then(|m| m.get(run)) {
            return Some(ShapedHit::Cur(v));
        }
        if let Some(v) = self.prev.get(&style).and_then(|m| m.get(run)) {
            return Some(ShapedHit::Prev(v));
        }
        None
    }

    #[inline]
    fn insert(&mut self, style: StyleBits, run: Box<str>, v: ShapedRunGlyphs) {
        self.cur.entry(style).or_default().entry(run).or_insert(v);
    }

    #[inline]
    fn cur_len(&self) -> usize {
        self.cur.values().map(|m| m.len()).sum()
    }

    #[inline]
    fn rotate(&mut self) {
        std::mem::swap(&mut self.cur, &mut self.prev);
        self.cur.clear();
    }

    #[inline]
    fn clear(&mut self) {
        self.cur.clear();
        self.prev.clear();
    }
}

#[derive(Default)]
struct TwoGenTextCache<V> {
    cur: KeyedHashMap<Box<str>, V>,
    prev: KeyedHashMap<Box<str>, V>,
}

impl<V> TwoGenTextCache<V> {
    #[inline]
    fn get(&self, key: &str) -> Option<(&V, bool)> {
        if let Some(v) = self.cur.get(key) {
            return Some((v, false));
        }
        if let Some(v) = self.prev.get(key) {
            return Some((v, true));
        }
        None
    }

    #[inline]
    fn insert(&mut self, key: Box<str>, v: V) {
        self.cur.entry(key).or_insert(v);
    }

    #[inline]
    fn cur_len(&self) -> usize {
        self.cur.len()
    }

    #[inline]
    fn rotate(&mut self) {
        std::mem::swap(&mut self.cur, &mut self.prev);
        self.cur.clear();
    }

    #[inline]
    fn clear(&mut self) {
        self.cur.clear();
        self.prev.clear();
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.cur.is_empty() && self.prev.is_empty()
    }
}

type VarCoords = Option<std::sync::Arc<[(u32, f32)]>>;

struct ResolvedInstances {
    drawn: VarCoords,
    geometry: VarCoords,
    bold: VarCoords,
}

/// The instance a styled or injected face of the primary family draws at, resolved
/// for its role ([`FaceRole`]) by the policy the primary's own instance follows
/// ([`Renderer::compute_variations_for_face`]), so SF Mono's italic, whose `fvar`
/// default is Light, draws the weight of the Regular upright beside it. Both `None`
/// for a static face and where the instance is the face's `fvar` default.
#[derive(Clone, Debug, Default, PartialEq)]
struct FaceInstance {
    /// What the face's glyphs are drawn and shaped at.
    drawn: VarCoords,
    /// Its own bold instance, for a bold-italic cell the italic face serves: a real
    /// weight where synthetic bold would dilate `drawn`.
    bold: VarCoords,
}

/// The role a primary-family face plays, which picks its instance
/// ([`FaceInstance`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FaceRole {
    /// The italic face: the regular weight, as the upright primary draws it.
    Regular,
    /// A bold, bold-italic or injected bold face: a bold weight
    /// ([`variation::bold_role_coords`]).
    Bold,
}

impl FaceRole {
    /// The role of styled slot `slot` (0 bold, 1 italic, 2 bold-italic).
    const fn of_slot(slot: usize) -> Self {
        if slot == 1 { Self::Regular } else { Self::Bold }
    }
}

#[derive(Clone)]
enum AdmittedFontBlob {
    Slice(std::sync::Arc<[u8]>),
    Face(crate::font::FaceBytes),
}

impl AdmittedFontBlob {
    #[inline]
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Slice(bytes) => bytes,
            Self::Face(bytes) => bytes,
        }
    }

    #[inline]
    fn is_file_backed(&self) -> bool {
        matches!(self, Self::Face(bytes) if !bytes.is_heap())
    }
}

impl PartialEq for AdmittedFontBlob {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Face(a), Self::Face(b)) => a.same_bytes(b),
            (Self::Slice(a), Self::Slice(b)) if std::sync::Arc::ptr_eq(a, b) => true,
            _ => self.bytes() == other.bytes(),
        }
    }
}

impl std::fmt::Debug for AdmittedFontBlob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdmittedFontBlob")
            .field("bytes", &self.bytes().len())
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AdmittedFaceSource {
    bytes: AdmittedFontBlob,
    index: u32,
}

/// Collision-free snapshot of every face in one prepared font generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmittedFontSources {
    sealed: bool,
    primary: Option<AdmittedFaceSource>,
    injected_bold: Option<AdmittedFaceSource>,
    styled: [Option<AdmittedFaceSource>; 3],
    fallback: Vec<AdmittedFaceSource>,
    symbol: Vec<AdmittedFaceSource>,
    emoji: Vec<AdmittedFaceSource>,
}

impl AdmittedFontSources {
    /// How this generation's discovered faces (fallback chain, symbol slot, colour chain) are resident.
    #[must_use]
    pub fn discovered_residency(&self) -> (u64, u64) {
        let mut mapped = 0u64;
        let mut copied = 0u64;
        for source in self
            .fallback
            .iter()
            .chain(self.symbol.iter())
            .chain(self.emoji.iter())
        {
            let len = source.bytes.bytes().len() as u64;
            if source.bytes.is_file_backed() {
                mapped += len;
            } else {
                copied += len;
            }
        }
        (mapped, copied)
    }
}

/// What one style draws with, in a sealed font generation ([`DrawnFaces`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyleDraw {
    /// The real face that draws the style (`SF Mono Italic`, or `SF Mono Bold` for a variable face's bold instance).
    pub face: Option<String>,
    /// The style bits drawn synthetically over that face (dilation for bold, shear for italic).
    pub synthetic: StyleBits,
}

/// The faces a sealed font generation draws, named as people know them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrawnFaces {
    /// The regular face's family (`SF Mono`).
    pub regular: Option<String>,
    /// Whether the regular face has programming ligatures (`liga`/`calt`, the features the run shaper turns on).
    pub regular_has_ligatures: bool,
    /// Bold, italic and bold italic, in the renderer's slot order.
    pub styles: [StyleDraw; 3],
    /// The first symbol-fallback face's family (`Noto Sans Symbols 2`).
    pub symbol: Option<String>,
    /// The colour emoji face's family (`Apple Color Emoji`).
    pub emoji: Option<String>,
}

fn face_display_name(bytes: &[u8], index: u32) -> Option<String> {
    use aterm_font::name_id::{FAMILY, FULL_NAME, SUBFAMILY};
    let face = aterm_font::Face::parse(bytes, index).ok()?;
    let name = font_catalog::face_name(&face, FULL_NAME).or_else(|| {
        let family = font_catalog::face_name(&face, FAMILY)?;
        Some(match font_catalog::face_name(&face, SUBFAMILY) {
            Some(sub) if !sub.eq_ignore_ascii_case("regular") => format!("{family} {sub}"),
            _ => family,
        })
    })?;
    Some(public_face_name(&name).into_owned())
}

/// What face `index` of `bytes` is called drawn at the named instance `named`: its
/// family and the instance's subfamily (`SF Mono Light Italic`); where no record names
/// that instance, the name its table gives the `fvar` default at that default, else its
/// family and the style it draws (a role's own instance, `SF Mono Bold Italic` named in
/// the Italic row, is not the `fvar` default `SF Mono Light Italic` the table names).
fn named_instance_display_name(bytes: &[u8], index: u32, named: &NamedInstance) -> Option<String> {
    let face = aterm_font::Face::parse(bytes, index).ok()?;
    if let Some(record) = variation::instance_records(&face)
        .into_iter()
        .find(|record| *record.coords == *named.coords)
    {
        let family = face_family_name(bytes, index)?;
        let subfamily = font_catalog::face_name(&face, record.subfamily_name_id)?;
        return Some(if subfamily.eq_ignore_ascii_case("regular") {
            family
        } else {
            format!("{family} {subfamily}")
        });
    }
    let default: Vec<i32> = face
        .variation_axes()
        .into_iter()
        .map(|axis| (axis.def_value * 65536.0).round() as i32)
        .collect();
    if *default == *named.coords {
        return face_display_name(bytes, index);
    }
    let family = face_family_name(bytes, index)?;
    let bold = variation::probe_face(&face)
        .and_then(|probe| {
            let wght = probe
                .axes
                .iter()
                .position(|axis| axis.tag == variation::WGHT_TAG)?;
            named.design_coords().get(wght).copied()
        })
        .map_or_else(
            || font_catalog::listed_style(&face).0,
            |weight| weight >= 600.0,
        );
    Some(match (bold, face.is_italic() || face.is_oblique()) {
        (false, false) => family,
        (true, false) => format!("{family} Bold"),
        (false, true) => format!("{family} Italic"),
        (true, true) => format!("{family} Bold Italic"),
    })
}

/// What face `index` of `bytes` drawn at `coords` is called where they are one of its
/// named instances other than its regular one (`SF Mono Light Italic`); `None` at its
/// regular instance and between named instances, where its role's words name it.
fn other_named_instance_name(bytes: &[u8], index: u32, coords: &[(u32, f32)]) -> Option<String> {
    let face = aterm_font::Face::parse(bytes, index).ok()?;
    let probe = variation::probe_face(&face)?;
    let value = |tag: u32, axis: &variation::VfAxis| {
        coords
            .iter()
            .find(|(held, _)| *held == tag)
            .map_or(axis.def, |&(_, value)| value)
    };
    let at = |record: &[f32]| {
        probe
            .axes
            .iter()
            .zip(record)
            .all(|(axis, &want)| (value(axis.tag, axis) - want).abs() < 1e-3)
    };
    if probe.regular_coords.as_deref().is_some_and(at) {
        return None;
    }
    let record = variation::instance_records(&face)
        .into_iter()
        .find(|record| record.coords.len() == probe.axes.len() && at(&record.design_coords()))?;
    named_instance_display_name(bytes, index, &NamedInstance::from_fixed(record.coords))
}

/// What face `index` of `bytes` drawn in a bold role at `coords` is called: the named instance
/// they are (`SF Mono Bold`, `SF Mono Heavy` a weight class above a heavy regular); else its
/// family and `Bold` (`Bold Italic` on a slanted face) where they draw its designed Bold's
/// weight (on axes the user set) or, for a face that designs no Bold, the standard Bold weight
/// ([`variation::BOLD_WGHT`]); else its family and the weight they draw (`SF Mono at weight
/// 700` over a regular asked at 600, past SF Mono's Bold at 683): never a designed face they do
/// not draw. `None` for coords without a weight.
fn bold_instance_name(bytes: &[u8], index: u32, coords: &[(u32, f32)]) -> Option<String> {
    if let Some(name) = other_named_instance_name(bytes, index, coords) {
        return Some(name);
    }
    let face = aterm_font::Face::parse(bytes, index).ok()?;
    let probe = variation::probe_face(&face)?;
    let weight = coords
        .iter()
        .find(|&&(tag, _)| tag == variation::WGHT_TAG)
        .map(|&(_, value)| value)?;
    let designed = probe
        .axes
        .iter()
        .position(|axis| axis.tag == variation::WGHT_TAG)
        .and_then(|w| {
            probe
                .bold_coords
                .as_deref()
                .and_then(|bold| bold.get(w).copied())
        });
    let bold = (weight - designed.unwrap_or(variation::BOLD_WGHT)).abs() < 0.5;
    let family = face_family_name(bytes, index)?;
    let italic = if face.is_italic() || face.is_oblique() {
        " Italic"
    } else {
        ""
    };
    Some(if bold {
        format!("{family} Bold{italic}")
    } else {
        format!("{family}{italic} at weight {}", weight.round())
    })
}

fn face_family_name(bytes: &[u8], index: u32) -> Option<String> {
    use aterm_font::name_id::{FAMILY, TYPOGRAPHIC_FAMILY};
    let face = aterm_font::Face::parse(bytes, index).ok()?;
    let family = font_catalog::face_name(&face, TYPOGRAPHIC_FAMILY)
        .or_else(|| font_catalog::face_name(&face, FAMILY))?;
    Some(public_face_name(&family).into_owned())
}

/// The top rows of the grid are host chrome, not terminal content, and their surface owns the window's padding as well as its own cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChromeBleed {
    /// How many grid rows from the top are chrome (`0` is inert).
    pub rows: usize,
    /// Chrome rows `[0, first)` the bleed leaves on the frame's padding.
    pub first: usize,
    /// The chrome surface tone, `0x00RRGGBB`, painted opaque like the chrome cells, whose non-default background never takes the background-opacity transmittance.
    pub color: u32,
    /// The hairline closing the last chrome row against the content below, `0x00RRGGBB`, or `None`.
    pub seam: Option<u32>,
    /// Whether row 0's cell backgrounds, not just the flat [`Self::color`], continue up through the `[0, grid_top)` strip column by column.
    pub top_extends_cells: bool,
    /// Chrome rows whose side gutters wear their own tones instead of [`Self::color`].
    pub row_edges: [Option<ChromeRowEdges>; CHROME_ROW_EDGES],
}

/// How many chrome rows can carry their own gutter tones ([`ChromeBleed::row_edges`]).
pub const CHROME_ROW_EDGES: usize = 3;

/// One chrome row whose side gutters continue its own edge cells' tones ([`ChromeBleed::row_edges`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChromeRowEdges {
    /// The chrome row, counted from the top like [`ChromeBleed::rows`].
    pub row: usize,
    /// The left gutter's tone, `0x00RRGGBB`.
    pub left: u32,
    /// The right gutter's tone, `0x00RRGGBB`.
    pub right: u32,
}

impl ChromeBleed {
    /// The `(left, right)` gutter tones of chrome row `r`.
    #[inline]
    #[must_use]
    pub fn gutter_tones(&self, r: usize) -> (u32, u32) {
        self.row_edges
            .iter()
            .flatten()
            .find(|e| e.row == r)
            .map_or((self.color, self.color), |e| (e.left, e.right))
    }
}

type CellGeometry = (usize, usize, i32);

#[derive(Clone, Copy, Debug, Default)]
struct CellGeometryMemo {
    slots: [Option<(u32, CellGeometry)>; 4],
    next: usize,
}

impl CellGeometryMemo {
    #[inline]
    fn get(&self, key: u32) -> Option<CellGeometry> {
        self.slots
            .iter()
            .flatten()
            .find(|(kept, _)| *kept == key)
            .map(|&(_, geometry)| geometry)
    }

    #[inline]
    fn with(mut self, key: u32, geometry: CellGeometry) -> Self {
        self.slots[self.next] = Some((key, geometry));
        self.next = (self.next + 1) % self.slots.len();
        self
    }
}

/// Monospace CPU rasterizer.
pub struct Renderer {
    font: std::sync::Arc<crate::font::Font>,
    shaping_primary_bytes: Option<std::sync::Arc<[u8]>>,
    primary_index: u32,
    /// The named instance the primary's font name chose ([`NamedInstance`]).
    primary_named: Option<NamedInstance>,
    var_coords: VarCoords,
    geom_coords: VarCoords,
    vf_bold_coords: VarCoords,
    cfg_font_variations: Vec<(u32, f32)>,
    shaping_variations: Vec<(aterm_font::Tag, f32)>,
    font_weight_dark_nudge: f32,
    bold_font: Option<crate::font::Font>,
    bold_font_bytes: Option<std::sync::Arc<[u8]>>,
    bold_font_index: u32,
    /// The named instance the injected bold face's font name chose ([`NamedInstance`]).
    bold_font_named: Option<NamedInstance>,
    /// The instance the injected bold face draws at ([`FaceRole::Bold`];
    /// [`Self::refresh_family_instances`] keeps it current).
    bold_font_instance: FaceInstance,
    bold_gid_cache: FxHashMap<char, Option<u16>>,
    bold_yields_private_use: bool,
    bold_lowered_asterisk: Option<bundled::LoweredAsterisk>,
    has_ligature_features: bool,
    has_user_features: bool,
    shaping: aterm_types::text_shaping::TextShapingConfig,
    resolved_features: Vec<aterm_types::text_shaping::FontFeature>,
    engine_features: Vec<aterm_font::shape::Feature>,
    shaped_runs: ShapedRunCache,
    fallback_chain: Vec<FallbackFace>,
    display_layer: Vec<DisplayLayerFace>,
    fallback_pick: FxHashMap<char, usize>,
    fallback_paths: Vec<String>,
    fallback_user_prefix: usize,
    cfg_fallback_fonts: Vec<String>,
    cfg_symbol_font: Option<String>,
    cfg_emoji_font: Option<String>,
    symbol_chain: Vec<FallbackFace>,
    symbol_fallback_paths: Vec<String>,
    symbol_user_prefix: usize,
    fallback_parse_rx: Option<FallbackParseRx>,
    symbol_parse_rx: Option<FallbackParseRx>,
    missing_font_classes: u8,
    runtime_discovery: bool,
    styled_faces: [Option<StyledFace>; 3],
    synthetic_styles: bool,
    #[cfg(any(all(unix, not(target_os = "macos")), windows))]
    hint_mode: hinted::HintMode,
    #[cfg(any(all(unix, not(target_os = "macos")), windows))]
    hint_bank: hinted::HintBank,
    #[cfg(all(unix, not(target_os = "macos")))]
    subpix_mode: subpixel::SubpixelMode,
    #[cfg(all(unix, not(target_os = "macos")))]
    subpx_bank: hinted::HintBank,
    #[cfg(all(unix, not(target_os = "macos")))]
    subpx_glyphs: FxHashMap<GlyphKey, Option<SubpxImage>>,
    #[cfg(all(unix, not(target_os = "macos")))]
    subpx_frame: bool,
    #[cfg(target_os = "macos")]
    rasterizer: RasterKind,
    #[cfg(target_os = "macos")]
    ct_cache: FxHashMap<(usize, u32, u32, u8), Option<macos_coretext::CtFont>>,
    #[cfg(target_os = "macos")]
    ct_descriptors: FxHashMap<usize, Option<macos_coretext::CtFaceDescriptors>>,
    primary_path: Option<String>,
    styled_loaded: bool,
    styled_retire_pending: [Option<StyledFaceId>; 3],
    admitted_sources_sealed: bool,
    drawn_faces: Option<DrawnFaces>,
    shapeable_scratch: Vec<bool>,
    break_mask_scratch: Vec<bool>,
    run_boundary_scratch: Vec<bool>,
    glyph_plan_scratch: Vec<ColumnGlyph>,
    glyph_plan_row: Option<usize>,
    /// The row below's column plan while a row's band stamps that row's halo lift, apart from
    /// `glyph_plan_scratch` so the plan cached for the row drawn last is kept.
    halo_plan_scratch: Vec<ColumnGlyph>,
    fg_clip: (i32, i32),
    fg_ink_top: i32,
    overshoot_trust: OvershootTrust,
    overshoot_surprise: bool,
    bg_runs_total: std::cell::Cell<u32>,
    bg_runs_at_base: std::cell::Cell<u32>,
    bg_fills_emitted: std::cell::Cell<u32>,
    shape_run_scratch: String,
    shape_chars_scratch: Vec<char>,
    shaping_buffer: aterm_font::shape::UnicodeBuffer,
    deco_scratch: Vec<[usize; 4]>,
    deco_skip_rect_scratch: Vec<[usize; 4]>,
    ink_scratch: Vec<bool>,
    keep_spans_scratch: Vec<(usize, usize)>,
    curl_scratch: Vec<(usize, usize, usize, usize, usize, usize, u32)>,
    ligature_break_scratch: Vec<usize>,
    /// The frame's knock-layer bodies' boxes ([`knock_rects`]), resident.
    knock_cut: KnockCut,
    /// [`Renderer::set_knock_out`]: a test's negative control.
    #[cfg(any(test, debug_assertions))]
    knock_off: bool,
    color_chain: Vec<ColorFace>,
    color_font_paths: Vec<String>,
    runtime_fallback: RuntimeFallback,
    px: f32,
    px_q: u32,
    cell_w: usize,
    cell_h: usize,
    line_height_scale: f32,
    typo_lm: Option<TypoLineMetrics>,
    letter_descent: Option<f32>,
    primary_yields_private_use: bool,
    primary_fits_two_cell_glyphs: bool,
    primary_yields_wide_characters: bool,
    primary_yields_small_dingbats: bool,
    primary_lowered_asterisk: Option<bundled::LoweredAsterisk>,
    baseline_adjust: i32,
    cell_geometry_memo: std::cell::Cell<CellGeometryMemo>,
    pad: usize,
    head: usize,
    pad_top: Option<usize>,
    chrome_bleed: Option<ChromeBleed>,
    chrome_room_memo: Option<(usize, i32, (usize, usize))>,
    chrome_room: (usize, usize),
    /// [`Self::glyph_ink`]'s last measure, keyed by the face, the rasters, the size and the blend.
    glyph_ink_memo: Option<glyph_ink::GlyphInkMemo>,
    /// The current input's actual glyph evidence, with unchanged rows shared across frames.
    glyph_frame_ink_memo: Option<glyph_ink::FrameInkMemo>,
    /// Identity retained by observations, so equal local epoch counters in two renderers
    /// cannot authorize reuse of one another's coverage.
    glyph_ink_identity: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Bumped each time the glyph rasters are dropped ([`Self::clear_glyph_images`]).
    glyph_generation: u64,
    baseline: i32,
    theme: Theme,
    stem_lut: [u8; 256],
    stem_gamma: f32,
    text_blending: TextBlending,
    font_thicken: bool,
    selection_fg: Option<u32>,
    selection_inactive: bool,
    selection_inactive_bg: Option<u32>,
    min_contrast: f32,
    bg_opacity: f32,
    cursor_opacity: f32,
    glyphs: FxHashMap<GlyphKey, CachedGlyph>,
    glyph_bytes: usize,
    glyph_frame: u64,
    glyph_kept: (usize, usize),
    keys: FxHashMap<(char, ScalarPresentation), GlyphKey>,
    primary_gid_cache: FxHashMap<char, Option<u16>>,
    emoji_keys: FxHashMap<char, GlyphKey>,
    cluster_shapes: emoji_cluster::ClusterShapes,
    styled_keys: FxHashMap<(char, StyleBits, ScalarPresentation), GlyphKey>,
    font_epoch: u64,
    land_parses_on_poll: bool,
    cursor_blink_phase: bool,
    cursor_style_override: Option<CursorStyle>,
    deco_masks: DecoMaskCache,
    deco_tables: Option<deco::DecoTables>,
    underline_adjust_pos: i32,
    underline_adjust_thick: i32,
    underline_skip_descenders: bool,
    undercurl_masks: UndercurlMaskCache,
    fx_scratch: fx_raster::FxScratch,
    /// [`Self::set_legacy_sprites`]: the legacy sprite path, a differential test's oracle.
    #[cfg(any(test, debug_assertions))]
    legacy_sprites: Option<legacy_sprite_oracle::LegacySprites>,
    alpha_memo: Option<Box<AlphaMemoBank>>,
}

type UndercurlMaskCache =
    std::cell::RefCell<Vec<((usize, usize, usize, usize), std::sync::Arc<Vec<u8>>)>>;

type DecoMaskCache = std::cell::RefCell<Vec<((DecoGlyph, usize, usize), std::sync::Arc<Vec<u8>>)>>;

/// How the last [`Renderer::render_input_cached`] frame changed the cached framebuffer, recorded on [`WindowCpu`] so a CPU presenter can bound its copy and reported damage to the changed pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum DamageOutcome {
    /// Every pixel may have changed.
    #[default]
    Full,
    /// Nothing changed (dirty-gate hit).
    GateHit,
    /// Only the rows flagged in [`WindowCpu::dirty_rows`] were repainted.
    Rows,
    /// A rigid history scroll.
    Scroll { delta_rows: i32 },
}

/// Per-window CPU damage-cache state, the analog of the GPU's `WindowGpu`.
#[derive(Default)]
pub struct WindowCpu {
    /// Damage-tracking cache for [`render_input_cached`](Renderer::render_input_cached).
    pub(crate) cache: Option<RenderCache>,
    /// Dirty-row scratch for [`render_input_cached`](Renderer::render_input_cached), handed to [`compute_dirty_rows`] each frame and reset in place.
    pub(crate) dirty_scratch: Vec<bool>,
    /// Shift-plan scratch: [`scroll_shift_plan`] writes its candidate dirty set here while [`dirty_scratch`](Self::dirty_scratch) holds the row diff's, so `render_core` can keep the smaller.
    pub(crate) shift_scratch: Vec<bool>,
    /// Decoded inline-image cache (iTerm2 OSC 1337 `File=`), keyed by the payload `Arc`'s pointer and the footprint size, each entry resampled to `cols*cell_w × rows*cell_h` so a covered cell paints a 1:1 tile.
    pub(crate) image_cache: ImageCache,
    /// Present-translate scratch.
    pub(crate) present_scratch: Vec<u32>,
    /// Incoming-row apron scratch.
    pub(crate) apron: ApronScratch,
    /// Whether the last [`render_input_cached`](Renderer::render_input_cached) handed back the translated [`present_scratch`](Self::present_scratch) rather than the cache.
    pub(crate) last_translated: bool,
    /// Presenter scratch (the renderer never reads it).
    pub(crate) presented_translated: bool,
    /// How the last [`render_input_cached`](Renderer::render_input_cached) frame changed the cached pixels ([`DamageOutcome`]).
    pub(crate) last_damage: DamageOutcome,
    /// Presenter scratch (the renderer never reads it).
    pub(crate) presented_chrome: bool,
    /// Presenter scratch (the renderer never reads it).
    pub(crate) presented_edge_rows: std::ops::Range<usize>,
    /// Presenter scratch (the renderer never reads it).
    pub(crate) fallback_pending_prev: bool,
    /// Lower-apron scratch, resident across frames.
    pub(crate) apron_band: Vec<u32>,
    /// Wallpaper conversion cache.
    pub(crate) wallpaper_px: Option<WallpaperPx>,
}

/// One converted wallpaper generation (see [`WindowCpu::wallpaper_px`]).
#[derive(Default)]
pub(crate) struct WallpaperPx {
    key: usize,
    px: Vec<u32>,
}

impl WindowCpu {
    pub fn new() -> Self {
        Self::default()
    }

    fn into_presented_frame(self) -> Frame {
        let cache = self.cache.expect("cache populated by render");
        Frame {
            width: cache.width,
            height: cache.height,
            pixels: if self.last_translated {
                self.present_scratch
            } else {
                cache.pixels
            },
        }
    }

    /// Drop the damage cache so the next `render_input_cached` repaints fully.
    pub fn invalidate(&mut self) {
        self.cache = None;
        self.last_damage = DamageOutcome::Full;
        self.apron.invalidate();
    }

    /// How the last [`render_input_cached`](Renderer::render_input_cached) frame changed the cached pixels.
    pub fn last_damage(&self) -> DamageOutcome {
        self.last_damage
    }

    /// The untranslated damage cache (empty before the first frame).
    pub fn frame_pixels(&self) -> &[u32] {
        self.cache.as_ref().map_or(&[], |c| &c.pixels[..])
    }

    /// The pixels the last [`render_input_cached`](Renderer::render_input_cached) handed back, as a shared borrow.
    pub fn presented_pixels(&self) -> &[u32] {
        if self.last_translated {
            &self.present_scratch
        } else {
            self.frame_pixels()
        }
    }

    /// Whether the most recent frame was translated (see the field doc).
    pub fn last_translated(&self) -> bool {
        self.last_translated
    }

    /// Presenter scratch: whether the last accepted present was translated.
    pub fn presented_translated(&self) -> bool {
        self.presented_translated
    }

    /// Presenter scratch: record whether the accepted present was translated.
    pub fn set_presented_translated(&mut self, translated: bool) {
        self.presented_translated = translated;
    }

    /// Per-row repaint flags of the last frame.
    pub fn dirty_rows(&self) -> &[bool] {
        &self.dirty_scratch
    }

    /// Presenter scratch: whether the previous present composited frontend chrome.
    pub fn presented_chrome(&self) -> bool {
        self.presented_chrome
    }

    /// Presenter scratch: record whether the frame just presented carried frontend chrome.
    pub fn set_presented_chrome(&mut self, chrome: bool) {
        self.presented_chrome = chrome;
    }

    /// Presenter scratch: the chrome extent the remainder bands were last placed with.
    pub fn presented_edge_rows(&self) -> std::ops::Range<usize> {
        self.presented_edge_rows.clone()
    }

    /// Presenter scratch: record the chrome extent the present just placed the remainder bands with.
    pub fn set_presented_edge_rows(&mut self, rows: std::ops::Range<usize>) {
        self.presented_edge_rows = rows;
    }

    /// Presenter scratch: store this redraw's fallback-parse-pending signal and return the previous one.
    pub fn swap_fallback_pending(&mut self, pending: bool) -> bool {
        std::mem::replace(&mut self.fallback_pending_prev, pending)
    }
}

/// Projection of the asymmetric layout and CPU damage cache onto `AsymmetricPadLayout`'s state variables, so the Tier-1 test reads the production cache stamp directly.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AsymmetricPadLayoutProjection {
    pub pad: usize,
    pub head: usize,
    pub pad_top: usize,
    pub pad_bottom: usize,
    pub grid_top: usize,
    pub cached_grid_top: Option<usize>,
    pub cache_hit: bool,
    pub full_repaint: bool,
}

/// Project the real renderer/cache state used by the `#[refines]` anchors.
#[doc(hidden)]
#[must_use]
pub fn project_asymmetric_pad_layout(
    renderer: &Renderer,
    window: &WindowCpu,
    rows: usize,
) -> AsymmetricPadLayoutProjection {
    let (_, frame_h) = renderer.frame_size(rows, 1);
    let grid_top = renderer.grid_top();
    let grid_bottom = grid_top.saturating_add(rows.saturating_mul(renderer.cell_size().1));
    AsymmetricPadLayoutProjection {
        pad: renderer.pad(),
        head: renderer.head(),
        pad_top: renderer.pad_top(),
        pad_bottom: frame_h.saturating_sub(grid_bottom),
        grid_top,
        cached_grid_top: window.cache.as_ref().map(|cache| cache.grid_top),
        cache_hit: window.last_damage == DamageOutcome::GateHit,
        full_repaint: window.last_damage == DamageOutcome::Full,
    }
}

type ImageCacheKey = (usize, usize, usize);

/// The decoded inline images of one window, for both renderers' image passes.
pub struct ImageCache {
    entries: FxHashMap<ImageCacheKey, ImageCacheEntry>,
    order: std::collections::BTreeMap<u64, ImageCacheKey>,
    tick: u64,
    frame_start: u64,
    bytes: usize,
    raster_cap: usize,
    seen: FxHashSet<ImageCacheKey>,
    demands: Vec<usize>,
}

struct ImageCacheEntry {
    _pin: std::sync::Weak<aterm_core::grid::extra::ImageData>,
    raster: PlacementRaster,
    tick: u64,
}

impl Default for ImageCache {
    fn default() -> Self {
        Self {
            entries: FxHashMap::default(),
            order: std::collections::BTreeMap::new(),
            tick: 0,
            frame_start: 0,
            bytes: 0,
            raster_cap: MAX_PLACEMENT_RASTER_PX,
            seen: FxHashSet::default(),
            demands: Vec::new(),
        }
    }
}

impl ImageCache {
    const MAX_ENTRIES: usize = 64;
    const MAX_BYTES: usize = 64 * 1024 * 1024;

    /// Start a frame that shows `placements`.
    pub fn begin_frame<'a>(
        &mut self,
        placements: impl IntoIterator<
            Item = (
                &'a std::sync::Arc<aterm_core::grid::extra::ImageData>,
                usize,
                usize,
            ),
        >,
    ) {
        self.tick += 1;
        self.frame_start = self.tick;
        self.seen.clear();
        self.demands.clear();
        let mut last = None;
        for (image, fp_w, fp_h) in placements {
            if fp_w == 0 || fp_h == 0 {
                continue;
            }
            let key = Self::key(image, fp_w, fp_h);
            if last == Some(key) {
                continue;
            }
            last = Some(key);
            if self.seen.insert(key) {
                let (w, h) = placement_raster_size(fp_w, fp_h);
                self.demands.push(w * h);
            }
        }
        self.raster_cap = frame_raster_cap(&mut self.demands);
    }

    fn key(
        image: &std::sync::Arc<aterm_core::grid::extra::ImageData>,
        fp_w: usize,
        fp_h: usize,
    ) -> ImageCacheKey {
        (std::sync::Arc::as_ptr(image) as usize, fp_w, fp_h)
    }

    fn current(&self, raster: &PlacementRaster, fp_w: usize, fp_h: usize) -> bool {
        raster.rgba.is_empty()
            || (raster.w, raster.h) == placement_raster_size_within(fp_w, fp_h, self.raster_cap)
    }

    fn touch(&mut self, key: ImageCacheKey) {
        let Some(entry) = self.entries.get_mut(&key) else {
            return;
        };
        if entry.tick == self.tick {
            return;
        }
        self.order.remove(&entry.tick);
        self.tick += 1;
        entry.tick = self.tick;
        self.order.insert(self.tick, key);
    }

    fn get(
        &mut self,
        image: &std::sync::Arc<aterm_core::grid::extra::ImageData>,
        fp_w: usize,
        fp_h: usize,
    ) -> Option<&PlacementRaster> {
        let key = Self::key(image, fp_w, fp_h);
        let entry = self.entries.get(&key)?;
        if !self.current(&entry.raster, fp_w, fp_h) {
            return None;
        }
        self.touch(key);
        self.entries.get(&key).map(|e| &e.raster)
    }

    /// The raster cached for `image` at this footprint, at this frame's size, without marking it used.
    pub fn peek(
        &self,
        image: &std::sync::Arc<aterm_core::grid::extra::ImageData>,
        fp_w: usize,
        fp_h: usize,
    ) -> Option<&PlacementRaster> {
        self.entries
            .get(&Self::key(image, fp_w, fp_h))
            .map(|e| &e.raster)
            .filter(|raster| self.current(raster, fp_w, fp_h))
    }

    fn get_or_insert_with(
        &mut self,
        image: &std::sync::Arc<aterm_core::grid::extra::ImageData>,
        fp_w: usize,
        fp_h: usize,
        decode: impl FnOnce() -> PlacementRaster,
    ) -> &PlacementRaster {
        if self.get(image, fp_w, fp_h).is_none() {
            self.put(image.clone(), fp_w, fp_h, decode());
        }
        &self.entries[&Self::key(image, fp_w, fp_h)].raster
    }

    /// The raster for `image` at this footprint, decoded on a miss at this frame's size ([`decode_placement_within`]).
    pub fn get_or_decode(
        &mut self,
        image: &std::sync::Arc<aterm_core::grid::extra::ImageData>,
        fp_w: usize,
        fp_h: usize,
    ) -> &PlacementRaster {
        let cap = self.raster_cap;
        self.get_or_insert_with(image, fp_w, fp_h, || {
            decode_placement_within(image, fp_w, fp_h, cap).unwrap_or(PlacementRaster {
                w: 0,
                h: 0,
                rgba: Vec::new(),
            })
        })
    }

    fn put(
        &mut self,
        image: std::sync::Arc<aterm_core::grid::extra::ImageData>,
        fp_w: usize,
        fp_h: usize,
        raster: PlacementRaster,
    ) {
        let key = Self::key(&image, fp_w, fp_h);
        if let Some(old) = self.entries.remove(&key) {
            self.order.remove(&old.tick);
            self.bytes -= old.raster.rgba.len();
        }
        self.tick += 1;
        self.bytes += raster.rgba.len();
        self.order.insert(self.tick, key);
        self.entries.insert(
            key,
            ImageCacheEntry {
                _pin: std::sync::Arc::downgrade(&image),
                raster,
                tick: self.tick,
            },
        );
        while self.entries.len() > Self::MAX_ENTRIES || self.bytes > Self::MAX_BYTES {
            let Some((&tick, &lru)) = self.order.first_key_value() else {
                break;
            };
            if tick >= self.frame_start || tick == self.tick {
                break;
            }
            self.order.remove(&tick);
            if let Some(evicted) = self.entries.remove(&lru) {
                self.bytes -= evicted.raster.rgba.len();
            }
        }
    }
}

/// Cached state for the damage-tracking fast path in [`Renderer::render_input`].
pub(crate) struct RenderCache {
    pixels: Vec<u32>,
    width: usize,
    height: usize,
    grid_top: usize,
    input: RenderInput,
    cursor_blink_phase: bool,
    cursor_style_override: Option<CursorStyle>,
    font_epoch: u64,
}

const FONT_CANDIDATES: &[&str] = &[
    "/System/Library/Fonts/SFNSMono.ttf",
    "/System/Library/Fonts/Menlo.ttc",
    "/System/Library/Fonts/Monaco.ttf",
    "/System/Library/Fonts/Supplemental/Andale Mono.ttf",
    "/System/Library/Fonts/Supplemental/Courier New.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
    "/usr/share/fonts/opentype/noto/NotoSansMono-Regular.ttf",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\CascadiaMono.ttf",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\consola.ttf",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\lucon.ttf",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\cour.ttf",
];

fn primary_candidate_paths() -> Vec<String> {
    bundled::default_primary_candidates()
        .iter()
        .chain(FONT_CANDIDATES)
        .filter(|s| builtin_candidate_is_native(s))
        .map(|s| s.to_string())
        .collect()
}

/// Ordered fixed-path candidates used when no configured primary family is active.
#[must_use]
pub fn primary_font_candidate_paths() -> Vec<String> {
    primary_candidate_paths()
}

/// The face an unconfigured primary loads.
#[must_use]
pub fn default_primary_identity() -> Option<FaceRef> {
    primary_candidate_paths().into_iter().find_map(|candidate| {
        let bytes = read_resolved_font(&candidate).ok()?;
        let index = if bundled::is_bundled_id(&candidate) {
            0
        } else {
            best_face(&bytes, None, FaceTarget::REGULAR).map_or(0, |(_, index)| index)
        };
        face_admissible(&bytes, index).then(|| FaceRef::new(candidate, index))
    })
}

/// Whether `family` is a generic monospace family name rather than a face.
#[must_use]
pub fn is_generic_monospace_family(family: &str) -> bool {
    matches!(
        normalize_family(family.trim()).as_str(),
        "monospace" | "mono"
    )
}

/// The bundled last-resort monospace face.
#[cfg(feature = "embedded-font")]
pub fn embedded_font() -> &'static [u8] {
    include_bytes!("../assets/DejaVuSansMono.ttf")
}

/// One bundled display face.
pub struct DisplayFace {
    /// The stable selector after the `display:` scheme (config `display_font` value).
    pub id: &'static str,
    /// What the letterform is, for the Settings label.
    pub label: &'static str,
    /// The actual font's name + licence, for honest attribution in UI.
    pub face: &'static str,
    /// Render with a synthetic weight boost (the `apply_synthetic_style` dilation, on regular cells), for a face whose strokes rasterize to a hairline at body px (`bubble`'s Chewy at 16px).
    pub embolden: bool,
    bytes: &'static [u8],
}

/// The virtual-family scheme for bundled display faces.
pub const DISPLAY_FACE_SCHEME: &str = "display:";

/// The bundled display faces, in display order.
pub const DISPLAY_FACES: &[DisplayFace] = &[
    DisplayFace {
        id: "chunky",
        label: "Chunky",
        face: "Luckiest Guy (Brian J. Bonislawsky / Astigmatic, Apache-2.0)",
        embolden: false,
        bytes: include_bytes!("../assets/game/LuckiestGuy-Regular.ttf"),
    },
    DisplayFace {
        id: "pixel",
        label: "Pixel",
        face: "Monocraft (Idrees Hassan, OFL-1.1)",
        embolden: false,
        bytes: include_bytes!("../assets/game/Monocraft.ttf"),
    },
    DisplayFace {
        id: "engraved",
        label: "Engraved",
        face: "Cinzel (Natanael Gama / NDISCOVER, OFL-1.1)",
        embolden: false,
        bytes: include_bytes!("../assets/game/Cinzel-var.ttf"),
    },
    DisplayFace {
        id: "bubble",
        label: "Bubble",
        face: "Chewy (Font Diner / Sideshow, Apache-2.0)",
        embolden: true,
        bytes: include_bytes!("../assets/game/Chewy-Regular.ttf"),
    },
];

/// Canonicalize a display-face id.
#[must_use]
pub fn display_face_canonical_id(id: &str) -> Option<&'static str> {
    let id = id.trim();
    DISPLAY_FACES
        .iter()
        .find(|face| face.id.eq_ignore_ascii_case(id))
        .map(|face| face.id)
}

/// The embedded face bytes for a `display_font` id (`"pixel"`, …), or `None` for an unknown one.
#[must_use]
pub fn display_face_bytes(id: &str) -> Option<&'static [u8]> {
    let id = display_face_canonical_id(id)?;
    DISPLAY_FACES
        .iter()
        .find(|face| face.id == id)
        .map(|face| face.bytes)
}

/// The maximum number of faces one display-face mix may combine.
pub const DISPLAY_FACE_MIX_MAX: usize = 3;

/// The display faces a `display:` family string names, as canonical ids in the order written.
#[must_use]
pub fn display_face_ids_for_family(family: &str) -> Option<Vec<&'static str>> {
    let ids = family.trim().strip_prefix(DISPLAY_FACE_SCHEME)?;
    let mut seen: Vec<&'static str> = Vec::new();
    for id in ids.split('+') {
        let id = display_face_canonical_id(id)?;
        if seen.contains(&id) || seen.len() == DISPLAY_FACE_MIX_MAX {
            return None;
        }
        seen.push(id);
    }
    (!seen.is_empty()).then_some(seen)
}

/// The display-layer ids for a set of requested ids (a `display_font` value's parts, any spelling).
#[must_use]
pub fn display_layer_ids<'a>(requested: impl IntoIterator<Item = &'a str>) -> Vec<&'static str> {
    let mut ids: Vec<&'static str> = Vec::new();
    for id in requested {
        if let Some(id) = display_face_canonical_id(id)
            && !ids.contains(&id)
            && ids.len() < DISPLAY_FACE_MIX_MAX
        {
            ids.push(id);
        }
    }
    ids.sort_by_key(|id| DISPLAY_FACES.iter().position(|face| face.id == *id));
    ids
}

/// The deterministic per-character pick.
#[must_use]
pub fn display_mix_face_index(ch: char, total: usize) -> usize {
    if total <= 1 {
        return 0;
    }
    ((ch as u32).wrapping_mul(2_654_435_761) >> 16) as usize % total
}

/// How a bundled display face is fitted into the terminal grid.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DisplayFaceFit {
    /// The face is proportional.
    pub proportional: bool,
    /// Apply the synthetic weight boost.
    pub embolden: bool,
    /// The highest printable-ASCII ink above the baseline of a glyph that stands on the line, in em (a mark floating above it is seated on its own).
    pub ink_top_em: f32,
    /// The lowest printable-ASCII ink of a glyph with a body above the line, in em, negative below the baseline (`_` is seated on its own).
    pub ink_bottom_em: f32,
    /// The median advance of the ASCII letters, in em.
    pub letter_advance_em: f32,
}

const DISPLAY_FACE_MONO_SPREAD: f32 = 1.02;

const DISPLAY_FACE_PROBE_PX: f32 = 128.0;

const DISPLAY_FACE_MIN_SCALE: f32 = 0.25;

/// The [`DisplayFaceFit`] for a bundled display face, identified by its bytes.
#[must_use]
pub fn display_face_fit(bytes: &[u8]) -> Option<DisplayFaceFit> {
    let entry = DISPLAY_FACES.iter().find(|font| {
        font.bytes.len() == bytes.len()
            && (std::ptr::eq(font.bytes.as_ptr(), bytes.as_ptr()) || font.bytes == bytes)
    })?;
    let identity = DisplayFaceFit {
        embolden: entry.embolden,
        ..DisplayFaceFit::default()
    };
    let font = crate::font::Font::from_bytes(bytes, crate::font::FontSettings::default()).ok()?;
    let mut advances: Vec<f32> = Vec::new();
    let mut letters: Vec<f32> = Vec::new();
    let (mut ink_top, mut ink_bottom) = (0.0_f32, 0.0_f32);
    for ch in '!'..='~' {
        let metrics = font.metrics(ch, DISPLAY_FACE_PROBE_PX);
        if metrics.advance_width > 0.0 {
            advances.push(metrics.advance_width);
            if ch.is_ascii_alphabetic() {
                letters.push(metrics.advance_width);
            }
        }
        if metrics.height > 0 {
            let bottom = metrics.ymin as f32;
            let top = bottom + metrics.height as f32;
            if bottom <= 0.0 {
                ink_top = ink_top.max(top);
            }
            if top > 0.0 {
                ink_bottom = ink_bottom.min(bottom);
            }
        }
    }
    let median = |values: &mut Vec<f32>| {
        values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        values.get(values.len() / 2).copied().unwrap_or(0.0)
    };
    let letter_advance = median(&mut letters);
    let identity = DisplayFaceFit {
        ink_top_em: ink_top / DISPLAY_FACE_PROBE_PX,
        ink_bottom_em: ink_bottom / DISPLAY_FACE_PROBE_PX,
        letter_advance_em: letter_advance / DISPLAY_FACE_PROBE_PX,
        ..identity
    };
    if advances.is_empty() {
        return Some(identity);
    }
    let max = advances.iter().copied().fold(0.0_f32, f32::max);
    let median = median(&mut advances);
    Some(DisplayFaceFit {
        proportional: median > 0.0 && max / median >= DISPLAY_FACE_MONO_SPREAD,
        ..identity
    })
}

/// The uniform scale a display face is drawn at in a `cell_w` x `cell_h` cell at `px`.
#[doc(hidden)]
#[must_use]
pub fn display_face_scale(fit: &DisplayFaceFit, px: f32, cell_w: usize, cell_h: usize) -> f32 {
    if !px.is_finite() || px <= 0.0 {
        return 1.0;
    }
    let room = |space: f32, need_em: f32| {
        if need_em > 0.0 && need_em.is_finite() {
            space.max(0.0) / (need_em * px)
        } else {
            1.0
        }
    };
    let band = fit.ink_top_em - fit.ink_bottom_em;
    let mut scale = room(cell_w as f32, fit.letter_advance_em)
        .min(room(cell_h as f32, band))
        .min(1.0);
    let (lo, hi) = display_face_baselines(fit, scale, px, cell_h);
    if lo > hi {
        scale = scale.min(room(cell_h as f32 - 1.0, band));
    }
    if scale.is_finite() {
        scale.clamp(DISPLAY_FACE_MIN_SCALE, 1.0)
    } else {
        1.0
    }
}

fn display_face_baselines(fit: &DisplayFaceFit, scale: f32, px: f32, cell_h: usize) -> (i32, i32) {
    let rows = |em: f32| {
        let rows = (em.max(0.0) * scale * px).ceil();
        if rows.is_finite() {
            rows.min(i32::MAX as f32) as i32
        } else {
            0
        }
    };
    let cell_h = i32::try_from(cell_h).unwrap_or(i32::MAX);
    (
        rows(fit.ink_top_em),
        cell_h.saturating_sub(rows(-fit.ink_bottom_em)),
    )
}

/// The baseline the display layer stands on in a `cell_w` x `cell_h` cell whose primary baseline is `baseline` px below its top, at `px`.
#[doc(hidden)]
#[must_use]
pub fn display_layer_baseline(
    fits: &[DisplayFaceFit],
    face: Option<&DisplayFaceFit>,
    px: f32,
    cell_w: usize,
    cell_h: usize,
    baseline: i32,
) -> i32 {
    let bounds = |fit: &DisplayFaceFit| {
        let scale = display_face_scale(fit, px, cell_w, cell_h);
        display_face_baselines(fit, scale, px, cell_h)
    };
    let (lo, hi) = fits
        .iter()
        .map(bounds)
        .fold((i32::MIN, i32::MAX), |(lo, hi), (l, h)| {
            (lo.max(l), hi.min(h))
        });
    let (lo, hi) = if lo <= hi {
        (lo, hi)
    } else {
        face.map_or((lo.min(hi), lo.min(hi)), bounds)
    };
    baseline.clamp(lo.min(hi), hi.max(lo.min(hi)))
}

#[derive(Clone)]
struct DisplayLayerFace {
    id: &'static str,
    face: FallbackFace,
    fit: DisplayFaceFit,
}

/// The bundled Symbols Nerd Font.
#[cfg(feature = "embedded-font")]
pub fn embedded_symbols_font() -> &'static [u8] {
    include_bytes!("../assets/SymbolsNerdFontMono-Regular.ttf")
}

const FALLBACK_CANDIDATES: &[&str] = &[
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "/System/Library/Fonts/Apple Symbols.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansHebrew-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansArabic-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansSyriac-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansThaana-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansNKo-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansAdlam-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansTifinagh-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansEthiopic-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansVai-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansCoptic-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansArmenian-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansGeorgian-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansDevanagari-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansBengali-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansGurmukhi-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansGujarati-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansOriya-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansTamil-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansTelugu-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansKannada-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansMalayalam-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansSinhala-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansThai-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansLao-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansKhmer-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansMyanmar-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansTibetan-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSerifTibetan-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansCherokee-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansCanadianAboriginal-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansOsage-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansDeseret-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansRunic-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansCuneiform-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansEgyptianHieroglyphs-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoMusic-Regular.ttf",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\msyh.ttc",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\YuGothR.ttc",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\malgun.ttf",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\Nirmala.ttc",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\LeelawUI.ttf",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\sylfaen.ttf",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\ebrima.ttf",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\gadugi.ttf",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\mvboli.ttf",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\arial.ttf",
];

const NATIVE_SCRIPT_FALLBACK_CANDIDATES: &[&str] = &[
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/usr/share/fonts/truetype/noto/NotoSansHebrew-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansArabic-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansSyriac-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansThaana-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansNKo-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansAdlam-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansTifinagh-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansEthiopic-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansVai-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansCoptic-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansArmenian-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansGeorgian-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansDevanagari-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansBengali-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansGurmukhi-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansGujarati-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansOriya-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansTamil-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansTelugu-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansKannada-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansMalayalam-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansSinhala-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansThai-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansLao-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansKhmer-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansMyanmar-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansTibetan-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSerifTibetan-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansCherokee-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansCanadianAboriginal-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansOsage-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansDeseret-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansRunic-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansCuneiform-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansEgyptianHieroglyphs-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoMusic-Regular.ttf",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\YuGothR.ttc",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\YuGothL.ttc",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\YuGothM.ttc",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\YuGothB.ttc",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\msyh.ttc",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\malgun.ttf",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\Nirmala.ttc",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\LeelawUI.ttf",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\sylfaen.ttf",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\ebrima.ttf",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\gadugi.ttf",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\mvboli.ttf",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\msyhl.ttc",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\msyhbd.ttc",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\malgunsl.ttf",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\malgunbd.ttf",
];

#[cfg(windows)]
const WEIGHT_SIBLING_FAMILIES: &[&[(&str, u16)]] = &[
    &[
        ("C:\\Windows\\Fonts\\YuGothL.ttc", 300),
        ("C:\\Windows\\Fonts\\YuGothR.ttc", 400),
        ("C:\\Windows\\Fonts\\YuGothM.ttc", 500),
        ("C:\\Windows\\Fonts\\YuGothB.ttc", 700),
    ],
    &[
        ("C:\\Windows\\Fonts\\msyhl.ttc", 290),
        ("C:\\Windows\\Fonts\\msyh.ttc", 400),
        ("C:\\Windows\\Fonts\\msyhbd.ttc", 700),
    ],
    &[
        ("C:\\Windows\\Fonts\\malgunsl.ttf", 300),
        ("C:\\Windows\\Fonts\\malgun.ttf", 400),
        ("C:\\Windows\\Fonts\\malgunbd.ttf", 700),
    ],
];
#[cfg(not(windows))]
const WEIGHT_SIBLING_FAMILIES: &[&[(&str, u16)]] = &[];

fn weight_matched_fallback_paths(
    mut paths: Vec<String>,
    user_prefix: usize,
    primary_weight: u16,
    exists: impl Fn(&str) -> bool,
) -> Vec<String> {
    for path in paths.iter_mut().skip(user_prefix) {
        let Some(family) = WEIGHT_SIBLING_FAMILIES
            .iter()
            .find(|family| family.iter().any(|&(p, _)| p == path.as_str()))
        else {
            continue;
        };
        let rank = |w: u16| fallback_weight_rank(w, false, primary_weight, false);
        if let Some(&(best, _)) = family
            .iter()
            .filter(|&&(p, _)| p == path.as_str() || exists(p))
            .min_by_key(|&&(p, w)| (rank(w), p != path.as_str()))
        {
            *path = best.to_string();
        }
    }
    paths
}

const SYMBOL_FALLBACK_CANDIDATES: &[&str] = &[
    "/System/Library/Fonts/Supplemental/STIXTwoMath.otf",
    "/System/Library/Fonts/STIXTwoMath.otf",
    "/System/Library/Fonts/Apple Symbols.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\seguisym.ttf",
];

const SYMBOL_TIER_ADDITIVE_CANDIDATES: &[&str] =
    &["/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf"];

const SYMBOL_TIER_TEXT_ONLY_BACKSTOPS: &[&str] =
    &["/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"];

fn symbol_face_is_text_only_backstop(face: &FallbackFace) -> bool {
    [face.path.as_deref(), face.relocated_from.as_deref()]
        .into_iter()
        .flatten()
        .any(|p| SYMBOL_TIER_TEXT_ONLY_BACKSTOPS.contains(&p) || bundled::is_bundled_id(p))
}

const FOREIGN_PRIVATE_USE_FACES: &[&str] = &[
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\seguisym.ttf",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\arial.ttf",
    #[cfg(target_os = "linux")]
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
];

fn chain_face_is_foreign_private_use(face: &FallbackFace) -> bool {
    [face.path.as_deref(), face.relocated_from.as_deref()]
        .into_iter()
        .flatten()
        .any(|p| FOREIGN_PRIVATE_USE_FACES.contains(&p))
}

fn embedded_symbols_map(ch: char) -> bool {
    #[cfg(feature = "embedded-font")]
    {
        static MAPPED: std::sync::OnceLock<Vec<char>> = std::sync::OnceLock::new();
        MAPPED
            .get_or_init(|| {
                let mut mapped = Vec::new();
                if let Ok(face) = aterm_font::Face::parse(embedded_symbols_font(), 0)
                    && let Some(cmap) = face.tables().cmap
                {
                    for sub in cmap.subtables.into_iter().filter(|t| t.is_unicode()) {
                        sub.codepoints(|cp| {
                            if let Some(c) = char::from_u32(cp)
                                && face.glyph_index(c).is_some_and(|g| g.0 != 0)
                            {
                                mapped.push(c);
                            }
                        });
                    }
                }
                mapped.sort_unstable();
                mapped.dedup();
                mapped
            })
            .binary_search(&ch)
            .is_ok()
    }
    #[cfg(not(feature = "embedded-font"))]
    {
        let _ = ch;
        false
    }
}

fn embedded_symbols_own_text_point(ch: char) -> bool {
    !font_chain::is_private_use(ch) && embedded_symbols_map(ch)
}

const COLOR_EMOJI_CANDIDATES: &[&str] = &[
    "/System/Library/Fonts/Apple Color Emoji.ttc",
    "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
    #[cfg(windows)]
    "C:\\Windows\\Fonts\\seguiemj.ttf",
];

/// The built-in colour-emoji rows that name a file of this system.
fn builtin_color_emoji_candidates() -> impl Iterator<Item = &'static str> {
    COLOR_EMOJI_CANDIDATES
        .iter()
        .copied()
        .filter(|p| builtin_candidate_is_native(p))
}

fn fallback_discovery_paths() -> Vec<String> {
    let paths: Vec<String> = bundled::broad_fallback_ids()
        .iter()
        .chain(FALLBACK_CANDIDATES)
        .filter(|s| builtin_candidate_is_native(s))
        .map(|s| (*s).to_string())
        .collect();
    #[cfg(windows)]
    let paths = han_reading_order(paths, win_locale::user_reads_japanese_first());
    paths
}

/// The built-in Simplified Chinese row, which the table lists ahead of the Japanese one.
#[cfg(windows)]
const WINDOWS_CHINESE_ROW: &str = "C:\\Windows\\Fonts\\msyh.ttc";
/// The built-in Japanese row.
#[cfg(windows)]
const WINDOWS_JAPANESE_ROW: &str = "C:\\Windows\\Fonts\\YuGothR.ttc";

/// `paths` with the Han rows in the user's reading order. Japanese and Chinese share their ideographs' code points and the first face that maps one draws it. The table's order (Microsoft YaHei, then Yu Gothic) draws a whole line of either script in one face, since YaHei maps the kana and the ideographs everyday text in either script uses; Yu Gothic maps the Japanese repertoire alone, so ahead of YaHei it would split a Chinese line between two faces. `japanese_first` (the user's languages name Japanese ahead of Chinese, `win_locale`) moves the Japanese row to just before the Chinese one, which gives kana and kanji their Japanese forms.
#[cfg(windows)]
fn han_reading_order(mut paths: Vec<String>, japanese_first: bool) -> Vec<String> {
    if japanese_first
        && let Some(chinese) = paths.iter().position(|p| p == WINDOWS_CHINESE_ROW)
        && let Some(japanese) = paths.iter().position(|p| p == WINDOWS_JAPANESE_ROW)
        && chinese < japanese
    {
        paths[chinese..=japanese].rotate_right(1);
    }
    paths
}

fn fallback_candidate_paths(config: &[String]) -> (Vec<String>, usize) {
    (
        fallback_chain_order(config, &fallback_discovery_paths()),
        config.len(),
    )
}

fn symbol_discovery_paths() -> Vec<String> {
    bundled::symbol_fallback_ids()
        .iter()
        .chain(
            SYMBOL_FALLBACK_CANDIDATES
                .iter()
                .filter(|p| builtin_candidate_is_native(p) && !bundled::supersedes_system_face(p)),
        )
        .map(|s| (*s).to_string())
        .collect()
}

fn symbol_fallback_candidate_paths(config: &[String]) -> (Vec<String>, usize) {
    (
        fallback_chain_order(config, &symbol_discovery_paths()),
        config.len(),
    )
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RasterKind {
    Fontdue,
    CoreText,
}

#[cfg(target_os = "macos")]
fn select_rasterizer() -> RasterKind {
    if !portable_raster_forced() {
        return RasterKind::CoreText;
    }
    static ANNOUNCED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ANNOUNCED.get_or_init(|| {
        eprintln!(
            "aterm-render: ATERM_RASTERIZER set: portable rasterizer, not \
             CoreText (no hinting, subpixel off, no variable instancing); \
             unset it for shipped text."
        );
    });
    RasterKind::Fontdue
}

#[cfg(any(unix, windows))]
fn portable_raster_forced() -> bool {
    aterm_types::dev_seam!("ATERM_RASTERIZER").is_some_and(|v| v == "portable" || v == "fontdue")
}

type StyledFaceId = u64;

fn next_styled_face_id() -> StyledFaceId {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[derive(Clone)]
struct StyledFace {
    font: LazyFontdue,
    bytes: std::sync::Arc<[u8]>,
    index: u32,
    gid_cache: FxHashMap<char, Option<u16>>,
    id: StyledFaceId,
    yields_private_use: bool,
    lowered_asterisk: Option<bundled::LoweredAsterisk>,
    /// The named instance this face's font name chose ([`NamedInstance`]).
    named: Option<NamedInstance>,
    /// The instance this face draws at for its slot's role
    /// ([`Renderer::refresh_family_instances`] keeps it current).
    instance: FaceInstance,
}

impl StyledFace {
    fn parsed(&self) -> Option<&std::sync::Arc<crate::font::Font>> {
        self.font.get_at(&self.bytes, self.index)
    }

    fn covers(&self, ch: char) -> bool {
        fontdue_cmap_covers(&self.bytes, self.index, ch)
    }

    fn unicode_gid(&mut self, ch: char) -> Option<u16> {
        if let Some(&g) = self.gid_cache.get(&ch) {
            return g;
        }
        let g = aterm_font::Face::parse(&self.bytes, self.index)
            .ok()
            .and_then(|f| f.glyph_index(ch))
            .map(|gid| gid.0)
            .filter(|&gid| gid != 0);
        self.gid_cache.insert(ch, g);
        g
    }
}

fn styled_sibling_paths(primary: &str) -> [Vec<String>; 3] {
    let path = std::path::Path::new(primary);
    let Some(dir) = path.parent() else {
        return Default::default();
    };
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return Default::default();
    };
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("ttf");
    let base = stem
        .strip_suffix("-Regular")
        .or_else(|| stem.strip_suffix("Regular"))
        .or_else(|| stem.strip_suffix("-Roman"))
        .or_else(|| stem.strip_suffix("-Book"))
        .unwrap_or(stem);
    let mk = |suffixes: &[&str]| -> Vec<String> {
        let mut out = Vec::new();
        for suf in suffixes {
            for e in [ext, "ttf", "otf", "ttc"] {
                let cand = dir.join(format!("{base}{suf}.{e}"));
                let cand = cand.to_string_lossy().into_owned();
                if !out.contains(&cand) {
                    out.push(cand);
                }
            }
        }
        out
    };
    [
        mk(&["-Bold", " Bold", "Bold", "-bold"]),
        mk(&[
            "-Italic", "-Oblique", " Italic", " Oblique", "Italic", "Oblique",
        ]),
        mk(&[
            "-BoldItalic",
            "-BoldOblique",
            " Bold Italic",
            "BoldItalic",
            "BoldOblique",
        ]),
    ]
}

fn color_emoji_discovery_paths() -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for dir in user_font_dirs() {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut hits: Vec<String> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                    matches!(e.to_ascii_lowercase().as_str(), "ttf" | "otf" | "ttc")
                }) && p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.to_ascii_lowercase().contains("emoji"))
            })
            .filter_map(|p| p.to_str().map(String::from))
            .collect();
        hits.sort();
        paths.extend(hits);
    }
    paths.extend(builtin_color_emoji_candidates().map(str::to_string));
    paths
}

fn color_emoji_candidate_paths(config: &[String]) -> Vec<String> {
    fallback_chain_order(config, &color_emoji_discovery_paths())
}

#[derive(Clone)]
struct ColorFace {
    bytes: crate::font::FaceBytes,
    injected: bool,
}

const MAX_COLOR_FACES: usize = 1 << (u32::BITS - COLOUR_FACE_SHIFT);

fn color_face_draws(bytes: &[u8], ch: char) -> bool {
    aterm_font::Face::parse(bytes, 0).ok().is_some_and(|face| {
        face.glyph_index(ch)
            .is_some_and(|gid| colour_drawable(&face, gid))
    })
}

#[derive(Clone, Copy, Default)]
struct FaceNorm {
    ideo_adv_em: Option<f32>,
    x_height_em: Option<f32>,
}

fn face_norm_facts(bytes: &[u8], index: u32) -> FaceNorm {
    let Ok(face) = aterm_font::Face::parse(bytes, index) else {
        return FaceNorm::default();
    };
    let upem = f32::from(face.units_per_em());
    if upem <= 0.0 {
        return FaceNorm::default();
    }
    let ideo_adv_em = face
        .glyph_index('\u{4E00}')
        .and_then(|g| face.glyph_hor_advance(g))
        .filter(|&a| a > 0)
        .map(|a| f32::from(a) / upem);
    FaceNorm {
        ideo_adv_em,
        x_height_em: x_height_em_of(&face, upem),
    }
}

fn x_height_em_of(face: &aterm_font::Face<'_>, upem: f32) -> Option<f32> {
    if let Some(xh) = face.x_height().filter(|&v| v > 0) {
        return Some(f32::from(xh) / upem);
    }
    let gid = face.glyph_index('x')?;
    let bb = face.glyph_bounding_box(gid)?;
    (bb.y_max > 0).then(|| f32::from(bb.y_max) / upem)
}

#[derive(Clone)]
struct LazyFontdue(std::sync::Arc<std::sync::OnceLock<Option<std::sync::Arc<crate::font::Font>>>>);

impl LazyFontdue {
    fn deferred() -> Self {
        LazyFontdue(std::sync::Arc::new(std::sync::OnceLock::new()))
    }

    fn ready(font: std::sync::Arc<crate::font::Font>) -> Self {
        let cell = std::sync::OnceLock::new();
        let _ = cell.set(Some(font));
        LazyFontdue(std::sync::Arc::new(cell))
    }

    fn get(&self, bytes: &crate::font::FaceBytes) -> Option<&std::sync::Arc<crate::font::Font>> {
        if self.0.get().is_none() {
            let parsed = intern_parsed_font_owned(bytes).ok();
            let _ = self.0.set(parsed);
        }
        self.0.get().and_then(Option::as_ref)
    }

    fn get_at(
        &self,
        bytes: &std::sync::Arc<[u8]>,
        index: u32,
    ) -> Option<&std::sync::Arc<crate::font::Font>> {
        if self.0.get().is_none() {
            let parsed = shared_parsed_face_owned(bytes, index).ok();
            let _ = self.0.set(parsed);
        }
        self.0.get().and_then(Option::as_ref)
    }

    fn known_bad(&self) -> bool {
        matches!(self.0.get(), Some(None))
    }

    fn parsed(&self) -> Option<&std::sync::Arc<crate::font::Font>> {
        self.0.get().and_then(Option::as_ref)
    }

    #[cfg(test)]
    fn is_materialised(&self) -> bool {
        self.0.get().is_some()
    }

    #[cfg(all(test, any(target_os = "linux", feature = "embedded-font")))]
    fn failed_parse() -> Self {
        let cell = std::sync::OnceLock::new();
        let _ = cell.set(None);
        LazyFontdue(std::sync::Arc::new(cell))
    }
}

#[derive(Clone)]
struct FallbackFace {
    font: LazyFontdue,
    bytes: crate::font::FaceBytes,
    index: u32,
    path: Option<String>,
    relocated_from: Option<String>,
    scope: Option<std::sync::Arc<ScriptScope>>,
    chosen: bool,
    norm: FaceNorm,
}

impl FallbackFace {
    fn from_bytes(bytes: &[u8], path: Option<String>) -> Result<FallbackFace, String> {
        let (bytes, font) = intern_parsed_font(bytes)?;
        let norm = face_norm_facts(&bytes, 0);
        Ok(FallbackFace {
            font: LazyFontdue::ready(font),
            bytes,
            index: 0,
            path,
            relocated_from: None,
            scope: None,
            chosen: true,
            norm,
        })
    }

    fn from_path_bytes(
        bytes: &crate::font::FaceBytes,
        path: Option<String>,
    ) -> Result<FallbackFace, String> {
        if !face_admissible(bytes, 0) {
            return Err("face is not admissible as a renderable font".to_string());
        }
        let bytes = intern_discovered_font_bytes(bytes);
        let norm = face_norm_facts(&bytes, 0);
        Ok(FallbackFace {
            font: LazyFontdue::deferred(),
            bytes,
            index: 0,
            path,
            relocated_from: None,
            scope: None,
            chosen: false,
            norm,
        })
    }
}

/// The chain cells [`Renderer::ready_fallback_faces`] reads, owned.
#[derive(Clone)]
pub struct FallbackCoverage {
    faces: Vec<(LazyFontdue, crate::font::FaceBytes, u32)>,
}

impl FallbackCoverage {
    /// For each char of `text`, parse the first face in chain order whose cmap maps it.
    pub fn materialise_for(&self, text: &str) -> usize {
        let mut chars: Vec<char> = text
            .chars()
            .filter(|ch| !ch.is_whitespace() && !ch.is_control())
            .collect();
        chars.sort_unstable();
        chars.dedup();
        let mut parsed = 0;
        for ch in chars {
            for (cell, bytes, index) in &self.faces {
                if let Some(font) = cell.parsed() {
                    if font.lookup_glyph_index(ch) != 0 {
                        break;
                    }
                    continue;
                }
                if cell.known_bad() || !fontdue_cmap_covers(bytes, *index, ch) {
                    continue;
                }
                if cell.get(bytes).is_some() {
                    parsed += 1;
                    break;
                }
            }
        }
        parsed
    }
}

fn face_admissible(bytes: &[u8], index: u32) -> bool {
    aterm_font::Face::parse(bytes, index).is_ok_and(|face| face.units_per_em() > 0)
}

fn fontdue_cmap_covers(bytes: &[u8], index: u32, ch: char) -> bool {
    let Ok(face) = aterm_font::Face::parse(bytes, index) else {
        return false;
    };
    let Some(cmap) = face.tables().cmap else {
        return false;
    };
    for record_index in 0..cmap.subtables.len() {
        let Some(subtable) = cmap.subtables.get(record_index) else {
            break;
        };
        if subtable
            .glyph_index(ch as u32)
            .is_none_or(|glyph| glyph.0 == 0)
        {
            continue;
        }
        if matches!(
            subtable.format,
            aterm_font::cmap::Format::HighByteMappingThroughTable(_)
        ) {
            let Some(cmap_bytes) = face.raw_face().table(aterm_font::Tag::from_bytes(b"cmap"))
            else {
                continue;
            };
            if !fontdue_format2_enumerates(cmap_bytes, record_index, ch) {
                continue;
            }
        }
        return true;
    }
    false
}

fn fontdue_format2_enumerates(cmap: &[u8], record_index: u16, ch: char) -> bool {
    fn be_u16_at(bytes: &[u8], offset: usize) -> Option<u16> {
        let end = offset.checked_add(2)?;
        Some(u16::from_be_bytes(bytes.get(offset..end)?.try_into().ok()?))
    }
    fn be_u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
        let end = offset.checked_add(4)?;
        Some(u32::from_be_bytes(bytes.get(offset..end)?.try_into().ok()?))
    }
    fn checked(cmap: &[u8], record_index: u16, ch: char) -> Option<bool> {
        const CMAP_HEADER_LEN: usize = 4;
        const ENCODING_RECORD_LEN: usize = 8;
        const ENCODING_RECORD_OFFSET_FIELD: usize = 4;
        const FORMAT2_KEYS_OFFSET: usize = 6;

        let record = CMAP_HEADER_LEN + usize::from(record_index) * ENCODING_RECORD_LEN;
        let subtable_offset =
            usize::try_from(be_u32_at(cmap, record + ENCODING_RECORD_OFFSET_FIELD)?).ok()?;
        let subtable = cmap.get(subtable_offset..)?;
        if be_u16_at(subtable, 0)? != 2 {
            return Some(false);
        }

        let codepoint = u16::try_from(ch as u32).ok()?;
        let single_byte = codepoint <= 0xFF;
        let key_index = if single_byte {
            usize::from(codepoint)
        } else {
            usize::from(codepoint >> 8)
        };
        let key = be_u16_at(subtable, FORMAT2_KEYS_OFFSET + key_index * 2)?;
        Some((key == 0) == single_byte)
    }

    checked(cmap, record_index, ch).unwrap_or(false)
}

#[derive(Clone, Default)]
struct RuntimeFallback {
    faces: Vec<RuntimeFace>,
    failed_paths: FxHashSet<String>,
    decisions: FxHashMap<char, Option<usize>>,
    embedded_decisions: FxHashMap<char, Option<usize>>,
}

#[derive(Clone)]
struct RuntimeFace {
    font: Option<std::sync::Arc<crate::font::Font>>,
    bytes: crate::font::FaceBytes,
    index: u32,
    path: String,
    norm: FaceNorm,
}

#[derive(Clone, Copy)]
struct FallbackProbeCtx {
    use_ct: bool,
    primary_weight: u16,
    primary_italic: bool,
}

impl RuntimeFallback {
    const MAX_DECISIONS: usize = 4096;

    const MAX_FACES: usize = 32;

    fn resolve(&mut self, ch: char, ctx: FallbackProbeCtx) -> Option<usize> {
        if let Some(&decision) = self.decisions.get(&ch) {
            return decision;
        }
        let decision = self.discover(ch, ctx);
        if self.decisions.len() >= Self::MAX_DECISIONS {
            self.decisions.retain(|_, v| v.is_some());
            if self.decisions.len() >= Self::MAX_DECISIONS {
                self.decisions.clear();
            }
        }
        self.decisions.insert(ch, decision);
        decision
    }

    fn discover(&mut self, ch: char, ctx: FallbackProbeCtx) -> Option<usize> {
        for path in runtime_fallback_candidate_paths(ch, ctx) {
            if let Some(i) = self.faces.iter().position(|f| f.path == path) {
                if runtime_face_can_render(&self.faces[i], ch, ctx.use_ct) {
                    return Some(i);
                }
                continue;
            }
            if self.failed_paths.contains(&path) {
                continue;
            }
            let Ok(bytes) = font_file::read_font_file(std::path::Path::new(&path)) else {
                self.failed_paths.insert(path);
                continue;
            };
            let Some(index) =
                best_collection_face(&bytes, ch, ctx.primary_weight, ctx.primary_italic)
            else {
                self.failed_paths.insert(path);
                continue;
            };
            let norm = face_norm_facts(&bytes, index);
            let bytes = crate::font::FaceBytes::Vec(intern_font_bytes(bytes));
            let font = crate::font::Font::from_shared(
                bytes.clone(),
                crate::font::FontSettings {
                    collection_index: index,
                },
            )
            .ok()
            .map(std::sync::Arc::new);
            let face = RuntimeFace {
                font,
                bytes,
                index,
                path,
                norm,
            };
            let accepted = runtime_face_can_render(&face, ch, ctx.use_ct);
            if accepted || self.faces.len() < Self::MAX_FACES {
                self.faces.push(face);
                if accepted {
                    return Some(self.faces.len() - 1);
                }
            }
        }
        self.try_embedded_symbols(ch, ctx)
    }

    #[cfg(feature = "embedded-font")]
    const EMBEDDED_SYMBOLS_PATH: &'static str = "<embedded-symbols-nerd-font>";

    fn try_embedded_symbols(&mut self, ch: char, ctx: FallbackProbeCtx) -> Option<usize> {
        #[cfg(not(feature = "embedded-font"))]
        let _ = (ch, ctx);
        #[cfg(feature = "embedded-font")]
        {
            let key = Self::EMBEDDED_SYMBOLS_PATH;
            if !self.faces.iter().any(|f| f.path == key) {
                let bytes = crate::font::FaceBytes::Static(embedded_symbols_font());
                let norm = face_norm_facts(&bytes, 0);
                let font = crate::font::Font::from_shared(
                    bytes.clone(),
                    crate::font::FontSettings::default(),
                )
                .ok()
                .map(std::sync::Arc::new);
                self.faces.push(RuntimeFace {
                    font,
                    bytes,
                    index: 0,
                    path: key.to_string(),
                    norm,
                });
            }
            if let Some(i) = self.faces.iter().position(|f| f.path == key)
                && runtime_face_can_render(&self.faces[i], ch, ctx.use_ct)
            {
                return Some(i);
            }
        }
        None
    }

    fn resolve_embedded_only(&mut self, ch: char, ctx: FallbackProbeCtx) -> Option<usize> {
        if let Some(&decision) = self.embedded_decisions.get(&ch) {
            return decision;
        }
        let decision = self.try_embedded_symbols(ch, ctx);
        if self.embedded_decisions.len() >= Self::MAX_DECISIONS {
            self.embedded_decisions.retain(|_, v| v.is_some());
            if self.embedded_decisions.len() >= Self::MAX_DECISIONS {
                self.embedded_decisions.clear();
            }
        }
        self.embedded_decisions.insert(ch, decision);
        decision
    }

    #[allow(clippy::type_complexity)]
    fn parts_for(
        &self,
        ch: char,
    ) -> Option<(
        Option<std::sync::Arc<crate::font::Font>>,
        crate::font::FaceBytes,
        u32,
        FaceNorm,
    )> {
        let &Some(i) = self
            .decisions
            .get(&ch)
            .or_else(|| self.embedded_decisions.get(&ch))?
        else {
            return None;
        };
        let f = self.faces.get(i)?;
        Some((f.font.clone(), f.bytes.clone(), f.index, f.norm))
    }
}

struct RendererProbe<'a, 'g> {
    r: &'a mut Renderer,
    ch: char,
    wants_emoji: bool,
    primary_gid: &'g mut Option<u16>,
}

impl RendererProbe<'_, '_> {
    fn yields_to_colour(&mut self) -> bool {
        self.wants_emoji && self.r.color_font_has(self.ch)
    }
}

impl font_chain::ChainProbe for RendererProbe<'_, '_> {
    fn covers(&mut self, tier: font_chain::Tier) -> bool {
        match tier {
            font_chain::Tier::Procedural => {
                procedural::covers(self.ch) || self.r.spinner_star_preempts(self.ch)
            }
            font_chain::Tier::Primary => {
                if self
                    .r
                    .primary_path
                    .as_deref()
                    .is_some_and(bundled::is_bundled_id)
                    && self.yields_to_colour()
                {
                    *self.primary_gid = None;
                    return false;
                }
                *self.primary_gid = self.r.primary_unicode_gid(self.ch);
                self.primary_gid.is_some()
            }
            font_chain::Tier::Fallback => !self.yields_to_colour() && self.r.fallback_has(self.ch),
            font_chain::Tier::Symbol => {
                if self.yields_to_colour() && !self.r.symbol_chain_may_be_chosen() {
                    false
                } else {
                    let built_in_yields = self.yields_to_colour();
                    self.r.symbol_fallback_has(self.ch, built_in_yields)
                }
            }
            font_chain::Tier::Color => self.r.color_font_has(self.ch),
            font_chain::Tier::RuntimeDecisions => {
                let ctx = self.r.runtime_probe_ctx();
                self.r.runtime_fallback.resolve(self.ch, ctx).is_some()
            }
            font_chain::Tier::RuntimeEmbedded => {
                let ctx = self.r.runtime_probe_ctx();
                self.r
                    .runtime_fallback
                    .resolve_embedded_only(self.ch, ctx)
                    .is_some()
            }
            font_chain::Tier::Synthetic => procedural::covers_symbol(self.ch),
        }
    }

    fn parse_pending(&mut self, tier: font_chain::Tier) -> bool {
        match tier {
            font_chain::Tier::Fallback => self.r.fallback_parse_rx.is_some(),
            font_chain::Tier::Symbol => self.r.symbol_parse_rx.is_some(),
            _ => false,
        }
    }
}

fn runtime_face_can_render(face: &RuntimeFace, ch: char, use_ct: bool) -> bool {
    #[cfg(target_os = "macos")]
    if use_ct {
        return ct_face_can_render_shared(face.bytes.clone(), face.index, ch);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = use_ct;
    face.font.as_deref().is_some_and(|f| face_can_render(f, ch))
}

fn best_collection_face(bytes: &[u8], ch: char, pw: u16, pi: bool) -> Option<u32> {
    let n = aterm_font::fonts_in_collection(bytes).unwrap_or(1);
    let mut best: Option<(u32, u32)> = None;
    for i in 0..n {
        let Ok(face) = aterm_font::Face::parse(bytes, i) else {
            continue;
        };
        if face.glyph_index(ch).is_none_or(|g| g.0 == 0) {
            continue;
        }
        let rank = fallback_weight_rank(face.weight().to_number(), face.is_italic(), pw, pi);
        if best.is_none_or(|(br, _)| rank < br) {
            best = Some((rank, i));
        }
    }
    best.map(|(_, i)| i)
}

const RUNTIME_FALLBACK_PROBE_PX: f32 = 16.0;

fn face_can_render(face: &crate::font::Font, ch: char) -> bool {
    if face.lookup_glyph_index(ch) == 0 {
        return false;
    }
    let (m, bytes) = face.rasterize(ch, RUNTIME_FALLBACK_PROBE_PX);
    m.width > 0 && m.height > 0 && bytes.iter().any(|&c| c > 0)
}

#[cfg(target_os = "macos")]
fn ct_face_can_render_shared(handle: crate::font::FaceBytes, index: u32, ch: char) -> bool {
    let Ok(face) = aterm_font::Face::parse(&handle, index) else {
        return false;
    };
    let Some(gid) = face.glyph_index(ch).filter(|g| g.0 != 0) else {
        return false;
    };
    if face.glyph_raster_image(gid, u16::MAX).is_some() || face.is_color_glyph(gid) {
        return false;
    }
    let Some(ct) = macos_coretext::CtFont::new(handle, index, RUNTIME_FALLBACK_PROBE_PX, &[])
    else {
        return false;
    };
    matches!(
        ct.rasterize(gid.0, false),
        Some(rg) if rg.width > 0 && rg.height > 0 && rg.bytes.iter().any(|&c| c > 0)
    )
}

/// CoreText drawability probe.
#[cfg(target_os = "macos")]
#[doc(hidden)]
#[must_use]
pub fn ct_face_can_render(bytes: &[u8], index: u32, ch: char) -> bool {
    ct_face_can_render_shared(
        crate::font::FaceBytes::Slice(std::sync::Arc::from(bytes)),
        index,
        ch,
    )
}

fn runtime_fallback_candidate_paths(ch: char, ctx: FallbackProbeCtx) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    #[cfg(target_os = "macos")]
    {
        if let Some(p) = macos_coretext::font_path_for_char(ch)
            && !path_is_last_resort(&p)
        {
            paths.push(p);
        }
    }
    runtime_fallback_scan_candidates(ch, ctx, &mut paths);
    paths
}

fn path_is_last_resort(path: &str) -> bool {
    std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("LastResort"))
}

struct FontCoverage {
    path: String,
    ranges: Vec<(u32, u32)>,
    weight: u16,
    italic: bool,
}

impl FontCoverage {
    fn covers(&self, cp: u32) -> bool {
        self.ranges
            .binary_search_by(|&(lo, hi)| {
                if cp < lo {
                    std::cmp::Ordering::Greater
                } else if cp > hi {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .is_ok()
    }
}

fn font_cmap_ranges(bytes: &[u8], cps: &mut Vec<u32>) -> Option<Vec<(u32, u32)>> {
    let face = aterm_font::Face::parse(bytes, 0).ok()?;
    let cmap = face.tables().cmap?;
    cps.clear();
    for sub in cmap.subtables {
        if sub.is_unicode() {
            sub.codepoints(|cp| cps.push(cp));
        }
    }
    if cps.is_empty() {
        return None;
    }
    cps.sort_unstable();
    cps.dedup();
    let mut ranges: Vec<(u32, u32)> = Vec::with_capacity(64);
    for &cp in cps.iter() {
        match ranges.last_mut() {
            Some(last) if cp == last.1 + 1 => last.1 = cp,
            _ => ranges.push((cp, cp)),
        }
    }
    Some(ranges)
}

/// Whether [`font_coverage_index`] has been built in this process.
#[must_use]
pub fn font_coverage_index_built() -> bool {
    FONT_COVERAGE_INDEX.get().is_some()
}

/// How many times [`runtime_fallback_scan_candidates`] consulted (and so forced) the coverage index in this process.
#[must_use]
pub fn font_coverage_scan_queries() -> u64 {
    FONT_COVERAGE_SCAN_QUERIES.load(std::sync::atomic::Ordering::Relaxed)
}

static FONT_COVERAGE_INDEX: std::sync::OnceLock<Vec<FontCoverage>> = std::sync::OnceLock::new();

static FONT_COVERAGE_SCAN_QUERIES: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

const LINUX_BUILTIN_FONT_ROOT: &str = "/usr/share/fonts/";

fn relocate_builtin_font_path(declared: &str, files: &[std::path::PathBuf]) -> Option<String> {
    if !declared.starts_with(LINUX_BUILTIN_FONT_ROOT) {
        return None;
    }
    let name = std::path::Path::new(declared).file_name()?;
    files
        .iter()
        .find(|f| f.file_name() == Some(name) && f.as_os_str() != declared)
        .and_then(|f| f.to_str())
        .map(str::to_string)
}

fn relocate_missing_builtin_fonts(paths: &[String], user_prefix: usize) -> Vec<Option<String>> {
    let mut out = vec![None; paths.len()];
    #[cfg(windows)]
    {
        let fonts_dir = windows_fonts_dir();
        for (slot, path) in out.iter_mut().zip(paths).skip(user_prefix) {
            *slot = relocate_windows_builtin_font_path(path, &fonts_dir);
        }
    }
    if !cfg!(all(unix, not(target_os = "macos"))) {
        return out;
    }
    let missing: Vec<usize> = paths
        .iter()
        .enumerate()
        .skip(user_prefix)
        .filter(|(_, p)| {
            p.starts_with(LINUX_BUILTIN_FONT_ROOT) && !std::path::Path::new(p.as_str()).is_file()
        })
        .map(|(i, _)| i)
        .collect();
    if missing.is_empty() {
        return out;
    }
    let files = font_catalog::system_font_files();
    for i in missing {
        out[i] = relocate_builtin_font_path(&paths[i], &files);
    }
    out
}

fn admit_chain_face(declared: &str, relocated: Option<&str>) -> Option<FallbackFace> {
    if let Some(bytes) = bundled::face_for_id(declared) {
        return FallbackFace::from_path_bytes(
            &crate::font::FaceBytes::Static(bytes),
            Some(declared.to_string()),
        )
        .ok();
    }
    let load = relocated.unwrap_or(declared);
    let bytes = font_file::admit_font_file(std::path::Path::new(load)).ok()?;
    let mut face = FallbackFace::from_path_bytes(&bytes, Some(load.to_string())).ok()?;
    if relocated.is_some() {
        face.relocated_from = Some(declared.to_string());
    }
    Some(face)
}

fn build_fallback_chain(paths: &[String], user_prefix: usize) -> Vec<FallbackFace> {
    let additive = |i: usize, p: &str| {
        i < user_prefix
            || NATIVE_SCRIPT_FALLBACK_CANDIDATES.contains(&p)
            || bundled::is_bundled_id(p)
    };
    let speculative = paths
        .iter()
        .enumerate()
        .position(|(i, p)| !additive(i, p.as_str()))
        .map_or(paths.len(), |first_broad| first_broad + 1);

    let relocated = relocate_missing_builtin_fonts(paths, user_prefix);

    let parsed: Vec<Option<FallbackFace>> = std::thread::scope(|scope| {
        let workers: Vec<_> = paths[..speculative]
            .iter()
            .zip(&relocated)
            .map(|(p, moved)| scope.spawn(move || admit_chain_face(p, moved.as_deref())))
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap_or(None))
            .collect()
    });

    let mut chain = Vec::new();
    for (i, (p, face)) in paths.iter().zip(parsed).enumerate() {
        let Some(face) = face else {
            continue;
        };
        let keep_scanning = additive(i, p.as_str());
        chain.push(face);
        if !keep_scanning {
            return chain;
        }
    }
    for (i, p) in paths.iter().enumerate().skip(speculative) {
        let Some(face) = admit_chain_face(p, relocated[i].as_deref()) else {
            continue;
        };
        let keep_scanning = additive(i, p.as_str());
        chain.push(face);
        if !keep_scanning {
            break;
        }
    }
    chain
}

fn build_symbol_chain(paths: &[String], user_prefix: usize) -> Vec<FallbackFace> {
    let relocated = relocate_missing_builtin_fonts(paths, user_prefix);
    let mut chain = Vec::new();
    for (i, p) in paths.iter().enumerate() {
        let Some(mut face) = admit_chain_face(p, relocated[i].as_deref()) else {
            continue;
        };
        face.chosen = i < user_prefix;
        let keep_scanning = i >= user_prefix
            && (SYMBOL_TIER_ADDITIVE_CANDIDATES.contains(&p.as_str()) || bundled::is_bundled_id(p));
        chain.push(face);
        if !keep_scanning {
            break;
        }
    }
    chain
}

const SCRIPT_SUPPLEMENT_PROBES: &[char] = &[
    '\u{05D0}',
    '\u{0628}',
    '\u{0710}',
    '\u{0780}',
    '\u{07CA}',
    '\u{1E900}',
    '\u{2D30}',
    '\u{1200}',
    '\u{A500}',
    '\u{2C80}',
    '\u{0531}',
    '\u{10D0}',
    '\u{0915}',
    '\u{0995}',
    '\u{0A15}',
    '\u{0A95}',
    '\u{0B15}',
    '\u{0B95}',
    '\u{0C15}',
    '\u{0C95}',
    '\u{0D15}',
    '\u{0D9A}',
    '\u{0E01}',
    '\u{0E81}',
    '\u{1780}',
    '\u{1000}',
    '\u{0F40}',
    '\u{1820}',
    '\u{A000}',
    '\u{13A0}',
    '\u{1401}',
    '\u{104B0}',
    '\u{10400}',
    '\u{16A0}',
    '\u{12000}',
    '\u{13000}',
    '\u{4E2D}',
    '\u{3042}',
    '\u{30A2}',
    '\u{D55C}',
];

fn cmap_code_points(cmap: &aterm_font::cmap::Table<'_>) -> Vec<u32> {
    let mut points: Vec<u32> = Vec::new();
    for sub in cmap.subtables.into_iter().filter(|t| t.is_unicode()) {
        sub.codepoints(|cp| points.push(cp));
    }
    points.sort_unstable();
    points.dedup();
    points
}

fn points_by_script_run<'p>(
    points: &'p [u32],
    scripts: &[aterm_grapheme::Script],
    mut each: impl FnMut(aterm_grapheme::Script, &'p [u32]),
) {
    let mut runs = aterm_grapheme::script_runs().peekable();
    while let Some((first, script)) = runs.next() {
        let end = runs.peek().map_or(0x11_0000, |&(next, _)| next);
        if !scripts.contains(&script) {
            continue;
        }
        let from = points.partition_point(|&cp| cp < first);
        let to = points.partition_point(|&cp| cp < end);
        if from < to {
            each(script, &points[from..to]);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ScriptScope {
    scripts: Vec<aterm_grapheme::Script>,
    blocks: Vec<u32>,
}

impl ScriptScope {
    const BLOCK_SHIFT: u32 = 7;

    fn of_face(bytes: &[u8], index: u32, probes: &[char]) -> Self {
        let mut scripts: Vec<aterm_grapheme::Script> =
            probes.iter().map(|&c| aterm_grapheme::script(c)).collect();
        scripts.sort_unstable();
        scripts.dedup();
        let mut blocks = Vec::new();
        if let Ok(face) = aterm_font::Face::parse(bytes, index)
            && let Some(cmap) = face.tables().cmap
        {
            let points = cmap_code_points(&cmap);
            points_by_script_run(&points, &scripts, |_, run| {
                blocks.extend(run.iter().map(|&cp| cp >> Self::BLOCK_SHIFT));
            });
        }
        blocks.sort_unstable();
        blocks.dedup();
        Self { scripts, blocks }
    }

    fn admits(&self, ch: char) -> bool {
        let script = aterm_grapheme::script(ch);
        if self.scripts.contains(&script) {
            return true;
        }
        (script == aterm_grapheme::Script::COMMON || script == aterm_grapheme::Script::INHERITED)
            && !aterm_grapheme::is_emoji_presentation(ch)
            && self
                .blocks
                .binary_search(&(u32::from(ch) >> Self::BLOCK_SHIFT))
                .is_ok()
    }
}

#[derive(Clone, Debug)]
struct ScriptFaceCandidate {
    path: String,
    covers: u64,
    script_points: Vec<u16>,
    monospace: bool,
    weight: u16,
    italic: bool,
}

const SCRIPT_SUPPLEMENT_MIN_POINTS: u16 = 16;

const SCRIPT_PROBE_MAX_TABLE_BYTES: usize = 8 * 1024 * 1024;

fn script_face_candidate(path: &std::path::Path, missing: u64) -> Option<ScriptFaceCandidate> {
    let found = font_file::read_face_tables(
        path,
        0,
        &[*b"cmap", *b"OS/2", *b"post"],
        SCRIPT_PROBE_MAX_TABLE_BYTES,
    )
    .ok()?;
    let mut tables = found.tables.into_iter();
    let cmap_bytes = tables.next().flatten()?;
    let os2 = tables.next().flatten();
    let post = tables.next().flatten();
    let cmap = aterm_font::cmap::Table::parse(&cmap_bytes)?;
    let mut covers = 0u64;
    for (i, &probe) in SCRIPT_SUPPLEMENT_PROBES.iter().enumerate() {
        if cmap
            .subtables
            .into_iter()
            .filter(|t| t.is_unicode())
            .any(|t| t.glyph_index(u32::from(probe)).is_some_and(|g| g.0 != 0))
        {
            covers |= 1 << i;
        }
    }
    if covers & missing == 0 {
        return None;
    }
    #[cfg(any(test, debug_assertions))]
    crate::test_counters::count_script_census();
    let probe_scripts: Vec<(usize, aterm_grapheme::Script)> = SCRIPT_SUPPLEMENT_PROBES
        .iter()
        .enumerate()
        .filter(|&(i, _)| covers & (1 << i) != 0)
        .map(|(i, &c)| (i, aterm_grapheme::script(c)))
        .collect();
    let scripts: Vec<aterm_grapheme::Script> = probe_scripts.iter().map(|&(_, s)| s).collect();
    let mut script_points = vec![0u16; SCRIPT_SUPPLEMENT_PROBES.len()];
    let points = cmap_code_points(&cmap);
    points_by_script_run(&points, &scripts, |script, run| {
        let n = u16::try_from(run.len()).unwrap_or(u16::MAX);
        for &(i, probe_script) in &probe_scripts {
            if probe_script == script {
                script_points[i] = script_points[i].saturating_add(n);
            }
        }
    });
    for (i, &n) in script_points.iter().enumerate() {
        if n < SCRIPT_SUPPLEMENT_MIN_POINTS {
            covers &= !(1 << i);
        }
    }
    if covers & missing == 0 {
        return None;
    }
    let (weight, italic) = os2
        .as_deref()
        .and_then(aterm_font::os2::Table::parse)
        .map_or((400, false), |t| {
            (
                t.weight().to_number(),
                t.style() != aterm_font::Style::Normal,
            )
        });
    let monospace = post
        .as_deref()
        .and_then(aterm_font::post::Table::parse)
        .is_some_and(|t| t.is_monospaced);
    Some(ScriptFaceCandidate {
        path: path.to_str()?.to_string(),
        covers,
        script_points,
        monospace,
        weight,
        italic,
    })
}

fn script_face_candidates(missing: u64) -> std::sync::Arc<Vec<ScriptFaceCandidate>> {
    type Read = (
        Vec<std::path::PathBuf>,
        u64,
        std::sync::Arc<Vec<ScriptFaceCandidate>>,
    );
    static CANDIDATES: std::sync::Mutex<Option<Read>> = std::sync::Mutex::new(None);
    let files = font_catalog::system_font_files();
    let mut held = CANDIDATES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut wanted = missing;
    if let Some((seen, read_for, found)) = held.as_ref()
        && *seen == files
    {
        if missing & !*read_for == 0 {
            return std::sync::Arc::clone(found);
        }
        wanted |= *read_for;
    }
    let found: std::sync::Arc<Vec<ScriptFaceCandidate>> = std::sync::Arc::new(
        files
            .iter()
            .filter(|p| !p.to_str().is_some_and(path_is_last_resort))
            .filter_map(|p| script_face_candidate(p, wanted))
            .collect(),
    );
    *held = Some((files, wanted, std::sync::Arc::clone(&found)));
    found
}

fn pick_script_faces(candidates: &[ScriptFaceCandidate], missing: u64) -> Vec<(usize, u64)> {
    let mut picks: Vec<(usize, u64)> = Vec::new();
    for bit in (0..SCRIPT_SUPPLEMENT_PROBES.len()).filter(|&i| missing & (1 << i) != 0) {
        let best = candidates
            .iter()
            .enumerate()
            .filter(|(_, c)| c.covers & (1 << bit) != 0)
            .min_by_key(|&(i, c)| {
                (
                    c.covers.count_ones(),
                    std::cmp::Reverse(c.script_points.get(bit).copied().unwrap_or(0)),
                    !c.monospace,
                    c.weight.abs_diff(400),
                    c.italic,
                    i,
                )
            })
            .map(|(i, _)| i);
        let Some(best) = best else {
            continue;
        };
        match picks.iter_mut().find(|(i, _)| *i == best) {
            Some((_, scripts)) => *scripts |= 1 << bit,
            None => picks.push((best, 1 << bit)),
        }
    }
    picks
}

const SCRIPT_SUPPLEMENT_NAMED: &[(&str, char)] = &[
    (
        "/usr/share/fonts/truetype/noto/NotoSansMongolian-Regular.ttf",
        '\u{1820}',
    ),
    (
        "/usr/share/fonts/truetype/noto/NotoSansYi-Regular.ttf",
        '\u{A000}',
    ),
];

fn chain_face_draws(face: &FallbackFace, ch: char) -> bool {
    aterm_font::Face::parse(&face.bytes, face.index)
        .ok()
        .and_then(|f| f.glyph_index(ch))
        .is_some_and(|g| g.0 != 0)
}

fn admit_script_face(
    declared: &str,
    relocated: Option<&str>,
    scripts: u64,
) -> Option<FallbackFace> {
    let mut face = admit_chain_face(declared, relocated)?;
    let probes: Vec<char> = SCRIPT_SUPPLEMENT_PROBES
        .iter()
        .enumerate()
        .filter(|&(bit, _)| scripts & (1 << bit) != 0)
        .map(|(_, &probe)| probe)
        .collect();
    face.scope = Some(std::sync::Arc::new(ScriptScope::of_face(
        &face.bytes,
        face.index,
        &probes,
    )));
    Some(face)
}

fn script_supplement(chain: &[FallbackFace]) -> Vec<FallbackFace> {
    if !cfg!(all(unix, not(target_os = "macos"))) {
        return Vec::new();
    }
    let missing = SCRIPT_SUPPLEMENT_PROBES
        .iter()
        .enumerate()
        .filter(|&(_, &probe)| !chain.iter().any(|face| chain_face_draws(face, probe)))
        .fold(0u64, |set, (i, _)| set | (1 << i));
    supplement_missing_scripts(missing, SCRIPT_SUPPLEMENT_NAMED, script_face_candidates)
}

fn supplement_missing_scripts(
    mut missing: u64,
    named: &[(&str, char)],
    candidates: impl FnOnce(u64) -> std::sync::Arc<Vec<ScriptFaceCandidate>>,
) -> Vec<FallbackFace> {
    let mut supplement = Vec::new();
    let wanted: Vec<(String, char, u64)> = named
        .iter()
        .filter_map(|&(path, probe)| {
            let bit = 1u64 << SCRIPT_SUPPLEMENT_PROBES.iter().position(|&p| p == probe)?;
            (missing & bit != 0).then(|| (path.to_string(), probe, bit))
        })
        .collect();
    let paths: Vec<String> = wanted.iter().map(|(path, _, _)| path.clone()).collect();
    let relocated = relocate_missing_builtin_fonts(&paths, 0);
    for ((declared, probe, bit), moved) in wanted.into_iter().zip(relocated) {
        let Some(face) = admit_script_face(&declared, moved.as_deref(), bit) else {
            continue;
        };
        if chain_face_draws(&face, probe) {
            missing &= !bit;
            supplement.push(face);
        }
    }
    if missing == 0 {
        return supplement;
    }
    let candidates = candidates(missing);
    supplement.extend(
        pick_script_faces(&candidates, missing)
            .into_iter()
            .filter_map(|(i, scripts)| admit_script_face(&candidates[i].path, None, scripts)),
    );
    supplement
}

fn build_host_fallback_chain(paths: &[String], user_prefix: usize) -> Vec<FallbackFace> {
    let mut chain = build_fallback_chain(paths, user_prefix);
    let discovers = paths
        .iter()
        .skip(user_prefix)
        .any(|p| bundled::is_bundled_id(p) || FALLBACK_CANDIDATES.contains(&p.as_str()));
    if discovers {
        let supplement = script_supplement(&chain);
        chain.extend(supplement);
    }
    chain
}

fn font_coverage_index() -> &'static [FontCoverage] {
    FONT_COVERAGE_INDEX.get_or_init(|| {
        let mut idx: Vec<FontCoverage> = Vec::new();
        let mut bytes: Vec<u8> = Vec::new();
        let mut cps: Vec<u32> = Vec::new();
        for path in font_files() {
            let Some(p) = path.to_str().map(str::to_string) else {
                continue;
            };
            if path_is_last_resort(&p) {
                continue;
            }
            if font_file::read_font_file_into(&path, &mut bytes).is_err() {
                continue;
            }
            let (weight, italic) = aterm_font::Face::parse(&bytes, 0)
                .map(|f| (f.weight().to_number(), f.is_italic()))
                .unwrap_or((400, false));
            if let Some(ranges) = font_cmap_ranges(&bytes, &mut cps) {
                idx.push(FontCoverage {
                    path: p,
                    ranges,
                    weight,
                    italic,
                });
            }
        }
        idx
    })
}

fn runtime_fallback_scan_candidates(ch: char, ctx: FallbackProbeCtx, out: &mut Vec<String>) {
    FONT_COVERAGE_SCAN_QUERIES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let cp = ch as u32;
    let mut hits: Vec<(u32, &FontCoverage)> = font_coverage_index()
        .iter()
        .filter(|fc| fc.covers(cp) && !out.contains(&fc.path))
        .map(|fc| {
            (
                fallback_weight_rank(fc.weight, fc.italic, ctx.primary_weight, ctx.primary_italic),
                fc,
            )
        })
        .collect();
    hits.sort_by_key(|&(rank, _)| rank);
    out.extend(hits.into_iter().map(|(_, fc)| fc.path.clone()));
}

/// The sub-pixel placement policy for one axis of a CoreText glyph raster, a pure function of the padded ink-box origin so it can be verified without font i/o (`crates/aterm-render/tests/ct_fractional_bearing.rs`).
#[inline]
#[must_use]
pub fn ct_pen_and_bearing(b: f64) -> (f64, i32) {
    let fl = b.floor();
    (-fl, fl as i32)
}

/// The bitmap extent (one axis) for a CoreText glyph raster whose padded ink spans `ink + 2*pad` px from a fractional phase in `[0, 1)`.
#[inline]
#[must_use]
pub fn ct_padded_extent(ink: f64, pad: f64) -> usize {
    (ink + 2.0 * pad).ceil() as usize + 1
}

/// CJK normalization clamp bounds.
pub const CJK_SCALE_MIN: f32 = 0.85;
/// See [`CJK_SCALE_MIN`].
pub const CJK_SCALE_MAX: f32 = 1.15;
/// X-height normalization clamp bounds for non-CJK fallback faces.
pub const XHEIGHT_SCALE_MIN: f32 = 0.75;
/// See [`XHEIGHT_SCALE_MIN`].
pub const XHEIGHT_SCALE_MAX: f32 = 1.25;

/// The rasterization scale for a CJK fallback face.
#[inline]
#[must_use]
pub fn fallback_cjk_scale(cell2_px: f32, ideo_adv_px: f32) -> f32 {
    scale_clamped(cell2_px, ideo_adv_px, CJK_SCALE_MIN, CJK_SCALE_MAX)
}

/// The rasterization scale for a non-CJK fallback face.
#[inline]
#[must_use]
pub fn fallback_xheight_scale(primary_xh_px: f32, face_xh_px: f32) -> f32 {
    scale_clamped(
        primary_xh_px,
        face_xh_px,
        XHEIGHT_SCALE_MIN,
        XHEIGHT_SCALE_MAX,
    )
}

#[inline]
fn scale_clamped(target: f32, actual: f32, lo: f32, hi: f32) -> f32 {
    let usable = target.is_finite() && actual.is_finite() && target > 0.0 && actual > 0.0;
    if !usable {
        return 1.0;
    }
    let r = target / actual;
    if !r.is_finite() {
        return 1.0;
    }
    r.clamp(lo, hi)
}

/// The horizontal centring offset for a wide (2-cell) fallback glyph, as the colour-emoji raster does it.
#[inline]
#[must_use]
pub const fn wide_center_offset(box_w: i32, adv_px: i32) -> i32 {
    (box_w - adv_px).div_euclid(2)
}

/// Default terminal-cell span for a scalar when no materialized cell is available.
#[doc(hidden)]
#[inline]
#[must_use]
pub fn fallback_cell_count(
    ch: char,
    shaping: &aterm_types::text_shaping::TextShapingConfig,
) -> usize {
    let cjk = matches!(
        shaping.ambiguous_width,
        aterm_types::text_shaping::AmbiguousWidth::Double
    );
    if cjk {
        aterm_grapheme::char_width_cjk(ch)
    } else {
        aterm_grapheme::char_width(ch)
    }
    .min(2)
}

/// Logical span stored in a render-ready row for its lead cell.
#[doc(hidden)]
#[inline]
#[must_use]
pub fn materialized_cell_span(row: &[RenderCell], col: usize) -> u8 {
    if row.get(col + 1).is_some_and(|next| next.wide) {
        2
    } else {
        1
    }
}

/// Uniform scale that fits a proportional fallback raster into its terminal cell box without changing its aspect ratio.
#[doc(hidden)]
#[inline]
#[must_use]
pub fn fallback_fit_scale(
    box_w: usize,
    box_h: usize,
    width: usize,
    height: usize,
    xmin: i32,
    advance: f32,
) -> f32 {
    fit_scale_about_advance_centre(box_w, box_h, width, height, xmin, advance, true)
}

/// How a fitted fallback raster is sized and anchored, by the code point's Unicode class ([`fallback_fit_class`]).
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackFit {
    /// Letters and digits: advance fit, baseline anchor.
    Text,
    /// Everything else: ink fit, ink-centre anchor.
    Symbol,
}

/// The [`FallbackFit`] class of `ch`.
#[doc(hidden)]
#[inline]
#[must_use]
pub fn fallback_fit_class(ch: char) -> FallbackFit {
    if ch.is_alphanumeric() {
        FallbackFit::Text
    } else {
        FallbackFit::Symbol
    }
}

/// [`fallback_fit_scale`] for a [`FallbackFit::Symbol`].
#[doc(hidden)]
#[inline]
#[must_use]
pub fn fallback_ink_fit_scale(
    box_w: usize,
    box_h: usize,
    width: usize,
    height: usize,
    xmin: i32,
    advance: f32,
) -> f32 {
    fit_scale_about_advance_centre(box_w, box_h, width, height, xmin, advance, false)
}

fn fit_scale_about_advance_centre(
    box_w: usize,
    box_h: usize,
    width: usize,
    height: usize,
    xmin: i32,
    advance: f32,
    fit_advance: bool,
) -> f32 {
    if box_w == 0 || box_h == 0 || width == 0 || height == 0 {
        return 1.0;
    }
    let advance = if advance.is_finite() && advance > 0.0 {
        advance
    } else {
        width as f32
    };
    let centre = advance * 0.5;
    let ink_left = xmin as f32 - centre;
    let ink_right = xmin as f32 + width as f32 - centre;
    let ink_extent = ink_left.abs().max(ink_right.abs());
    let extent = if fit_advance {
        ink_extent.max(centre.abs())
    } else {
        ink_extent
    };
    let centred_w = 2.0 * extent;
    if !centred_w.is_finite() || centred_w <= 0.0 {
        return 1.0;
    }
    let sx = box_w as f32 / centred_w;
    let sy = box_h as f32 / height as f32;
    let scale = sx.min(sy).min(1.0);
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

#[derive(Clone, Copy, Debug)]
struct DeferredStyle {
    bold_px: usize,
    italic: bool,
}

impl DeferredStyle {
    const NONE: DeferredStyle = DeferredStyle {
        bold_px: 0,
        italic: false,
    };
}

const SYNTHETIC_ITALIC_SHEAR: f32 = 0.2;

/// The baseline-relative `ymin` of a fallback raster shrunk from `h` to `dst_h` rows.
#[doc(hidden)]
#[inline]
#[must_use]
pub fn shrink_ymin_about_ink_centre(
    ymin: i32,
    h: usize,
    dst_h: usize,
    baseline: i32,
    cell_h: usize,
) -> i32 {
    let h = i64::try_from(h).unwrap_or(i64::MAX / 4);
    let dst_h = i64::try_from(dst_h).unwrap_or(i64::MAX / 4);
    let twice = 2 * i64::from(ymin) + h - dst_h;
    let mut y = twice.div_euclid(2) + twice.rem_euclid(2);
    let cell_h = i64::try_from(cell_h).unwrap_or(i64::MAX / 4);
    if dst_h <= cell_h {
        let hi = i64::from(baseline) - dst_h;
        let lo = i64::from(baseline) - cell_h;
        y = y.clamp(lo, hi);
    }
    y.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// The [`FallbackFit::Text`] anchor.
#[doc(hidden)]
#[inline]
#[must_use]
pub fn shrink_ymin_about_baseline(ymin: i32, scale: f32) -> i32 {
    let y = (ymin as f32 * scale).round();
    if y.is_finite() {
        y.clamp(i32::MIN as f32, i32::MAX as f32) as i32
    } else {
        ymin
    }
}

/// Clamp a fallback glyph's bitmap rows to its cell row band.
#[inline]
#[must_use]
pub fn clamp_to_row_band(top: i32, height: usize, band_h: usize) -> (usize, usize) {
    clamp_to_band(top, height, band_h)
}

/// Clamp a fallback glyph's bitmap columns to its cell column band.
#[inline]
#[must_use]
pub fn clamp_to_col_band(left: i32, width: usize, band_w: usize) -> (usize, usize) {
    clamp_to_band(left, width, band_w)
}

fn ink_row_extent(height: usize, row_has_ink: impl Fn(usize) -> bool) -> Option<(usize, usize)> {
    let first = (0..height).find(|&y| row_has_ink(y))?;
    let last = (first..height).rev().find(|&y| row_has_ink(y))?;
    Some((first, last + 1))
}

fn seated_ymin(
    ink: (usize, usize),
    height: usize,
    ymin: i32,
    baseline: i32,
    cell_h: usize,
) -> Option<i32> {
    let floor = i32::try_from(cell_h).ok()?;
    let (first, last) = ink;
    if first >= last || last > height {
        return None;
    }
    let gy0 = baseline - i32::try_from(height).ok()? - ymin;
    let ink_top = gy0 + i32::try_from(first).ok()?;
    let ink_bottom = gy0 + i32::try_from(last).ok()?;
    let readable = baseline.min(floor);
    (ink_bottom > floor && ink_top >= readable && last - first <= cell_h)
        .then_some(ymin + (ink_bottom - floor))
}

fn glyph_ink_rows(img: &GlyphImage) -> Option<(usize, usize)> {
    let (w, h) = (img.width(), img.height());
    match img {
        GlyphImage::Mono { bytes, .. } => ink_row_extent(h, |y| {
            bytes
                .get(y * w..(y + 1) * w)
                .is_some_and(|r| r.iter().any(|&a| a > 0))
        }),
        GlyphImage::Rgba { bytes, .. } => ink_row_extent(h, |y| {
            bytes
                .get(y * w * 4..(y + 1) * w * 4)
                .is_some_and(|r| r.as_chunks::<4>().0.iter().any(|px| px[3] > 0))
        }),
    }
}

/// A glyph's ink box in the frame of its reported `(height, ymin)`.
#[must_use]
pub fn glyph_ink_box(img: &GlyphImage) -> Option<(usize, i32)> {
    let (first, last) = glyph_ink_rows(img)?;
    let top = img.ymin() + i32::try_from(img.height()).ok()?;
    Some((last - first, top - i32::try_from(last).ok()?))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OvershootTrust {
    /// Printable ASCII and the procedural cells stay in their cell.
    AsciiInCell,
    /// ASCII is judged by its raster too (a face or a line height whose ASCII rises above the cell).
    Rasters,
    /// A raster the per-cell key does not name (a ligature, a band icon) rose where the per-cell raster did not.
    Nothing,
}

struct CachedGlyph {
    img: GlyphImage,
    ink: Option<(usize, i32)>,
    used: std::cell::Cell<u64>,
    /// The knock-out's ring of a mono raster ([`knock_ring`]), built the first time the glyph
    /// lies on a companion's body and dropped with the entry.
    ring: std::cell::OnceCell<Box<[u8]>>,
    /// Coverage, and where the raster lays its ink row by row in its cell, are measured once
    /// for this raster and evicted with it.
    coverage: std::cell::OnceCell<Option<(std::sync::Arc<GlyphCoverage>, glyph_ink::InkRows)>>,
}

impl CachedGlyph {
    fn new(img: GlyphImage) -> Self {
        let ink = glyph_ink_box(&img);
        Self {
            img,
            ink,
            used: std::cell::Cell::new(0),
            ring: std::cell::OnceCell::new(),
            coverage: std::cell::OnceCell::new(),
        }
    }
}

/// The knock-out mask's mix ([`Renderer::blit_glyph_ring`]): coverage `t` takes the share of
/// the pixel cut toward the whole as the old page stamp's [`blend`] took a pixel toward the
/// page (the shares left uncovered multiply), and every channel keeps that share as a linear
/// byte, the alpha factor the cut reads ([`fx_raster::cut_sprite_art`]). [`blend`] toward
/// white kept it sRGB-encoded, which cut a letter's soft rim deeper than the stamp did.
#[inline]
fn knock_cover(m: u32, _white: u32, t: u8) -> u32 {
    let m = m & 0xff;
    (m + ((255 - m) * u32::from(t) + 127) / 255) * 0x0001_0101
}

/// The coverage a glyph pixel of coverage `cov` stamps at the offset `(dx, dy)` of
/// [`KNOCK_DILATE_OFFSETS`]: its own, and the whole pixel beside a pixel of [`KNOCK_INK_LEVEL`] or
/// more (the offsets a pixel away or less), so the ground next to a pixel that reads as ink is
/// what lies under the body and none of its coat. With no level, its own everywhere.
#[inline]
const fn knock_stamp(cov: u8, dx: i32, dy: i32) -> u8 {
    match KNOCK_INK_LEVEL {
        Some(level) if cov >= level && dx.abs() <= 1 && dy.abs() <= 1 => 255,
        _ => cov,
    }
}

/// `bytes` as [`knock_stamp`] stamps them a pixel away or less, where that is not `bytes`.
fn knock_near_stamp(bytes: &[u8]) -> Option<Vec<u8>> {
    let level = KNOCK_INK_LEVEL?;
    bytes
        .iter()
        .any(|&cov| cov >= level && cov != 255)
        .then(|| bytes.iter().map(|&cov| knock_stamp(cov, 0, 0)).collect())
}

/// The knock-out's ring of a `width × height` mono coverage (`bytes`): its coverage stamped at
/// each of [`KNOCK_DILATE_OFFSETS`] ([`knock_stamp`]), composed as each stamp's [`knock_cover`]
/// composes (the shares left uncovered multiply), over a raster [`KNOCK_REACH`] pixels larger on
/// every side. One blit of it cuts a body as the 21 stamps one after another do, a 21st of the
/// work.
fn knock_ring(width: usize, height: usize, bytes: &[u8]) -> Box<[u8]> {
    let reach = KNOCK_REACH as usize;
    let (rw, rh) = (width + 2 * reach, height + 2 * reach);
    let mut left = vec![255u32; rw * rh];
    for &(dx, dy) in &KNOCK_DILATE_OFFSETS {
        let (ox, oy) = ((reach as i32 + dx) as usize, (reach as i32 + dy) as usize);
        for (j, row) in bytes.chunks_exact(width).take(height).enumerate() {
            let out = &mut left[(j + oy) * rw + ox..(j + oy) * rw + ox + width];
            for (l, &cov) in out.iter_mut().zip(row) {
                let cov = knock_stamp(cov, dx, dy);
                if cov != 0 {
                    *l = (*l * (255 - u32::from(cov)) + 127) / 255;
                }
            }
        }
    }
    left.into_iter().map(|l| (255 - l) as u8).collect()
}

#[cfg(test)]
#[path = "lib_knock_ring_tests.rs"]
mod knock_ring_tests;

fn seat_ink_on_the_cell_floor(img: &mut GlyphImage, baseline: i32, cell_h: usize) {
    let Some(ink) = glyph_ink_rows(img) else {
        return;
    };
    let (GlyphImage::Mono { height, ymin, .. } | GlyphImage::Rgba { height, ymin, .. }) = img;
    if let Some(seated) = seated_ymin(ink, *height, *ymin, baseline, cell_h) {
        *ymin = seated;
    }
}

#[inline]
fn clamp_to_band(pos: i32, len: usize, band: usize) -> (usize, usize) {
    let (pos, len, band) = (pos as i64, len as i64, band as i64);
    let skip = (-pos).clamp(0, len);
    let new_pos = pos + skip;
    if new_pos >= band {
        return (skip as usize, 0);
    }
    let new_len = (len - skip).min(band - new_pos).max(0);
    (skip as usize, new_len as usize)
}

fn resample_mask_area(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Option<Vec<u8>> {
    let src_len = sw.checked_mul(sh)?;
    let dst_len = dw.checked_mul(dh)?;
    if src.len() != src_len || sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return None;
    }
    if (sw, sh) == (dw, dh) {
        return Some(src.to_vec());
    }
    let mut out = vec![0u8; dst_len];
    let denom = (sw as u128) * (sh as u128);
    for dy in 0..dh {
        let sy0 = dy.saturating_mul(sh) / dh;
        let sy1 = (dy
            .saturating_add(1)
            .saturating_mul(sh)
            .saturating_add(dh - 1)
            / dh)
            .min(sh);
        for dx in 0..dw {
            let sx0 = dx.saturating_mul(sw) / dw;
            let sx1 = (dx
                .saturating_add(1)
                .saturating_mul(sw)
                .saturating_add(dw - 1)
                / dw)
                .min(sw);
            let mut sum = 0u128;
            for sy in sy0..sy1 {
                let wy = area_overlap(dy, sy, sh, dh) as u128;
                for sx in sx0..sx1 {
                    let wx = area_overlap(dx, sx, sw, dw) as u128;
                    sum += u128::from(src[sy * sw + sx]) * wx * wy;
                }
            }
            out[dy * dw + dx] = ((sum + denom / 2) / denom).min(255) as u8;
        }
    }
    Some(out)
}

fn resample_mask_placed(
    src: &[u8],
    sw: usize,
    sh: usize,
    scale: f32,
    x: f32,
    y: f32,
) -> Option<(Vec<u8>, usize, usize, i32, i32)> {
    if sw == 0 || sh == 0 || src.len() != sw.checked_mul(sh)? || scale.is_nan() || scale <= 0.0 {
        return None;
    }
    let (left, top) = (x.floor(), y.floor());
    let (fx, fy) = (x - left, y - top);
    let dw = (fx + sw as f32 * scale).ceil().max(1.0) as usize;
    let dh = (fy + sh as f32 * scale).ceil().max(1.0) as usize;
    let spans = |n: usize, phase: f32, dn: usize| -> Vec<Vec<(usize, f32)>> {
        (0..n)
            .map(|i| {
                let (a, b) = (phase + i as f32 * scale, phase + (i + 1) as f32 * scale);
                (a.floor() as usize..(b.ceil() as usize).min(dn))
                    .filter_map(|d| {
                        let o = b.min(d as f32 + 1.0) - a.max(d as f32);
                        (o > 0.0).then_some((d, o))
                    })
                    .collect()
            })
            .collect()
    };
    let (wx, wy) = (spans(sw, fx, dw), spans(sh, fy, dh));
    let mut sum = vec![0.0f32; dw * dh];
    for (sy, rows) in wy.iter().enumerate() {
        for (sx, cols) in wx.iter().enumerate() {
            let c = f32::from(src[sy * sw + sx]);
            if c == 0.0 {
                continue;
            }
            for &(dy, oy) in rows {
                for &(dx, ox) in cols {
                    sum[dy * dw + dx] += c * ox * oy;
                }
            }
        }
    }
    let out = sum.iter().map(|v| v.round().min(255.0) as u8).collect();
    Some((out, dw, dh, left as i32, top as i32))
}

fn crop_to_ink(
    w: usize,
    h: usize,
    xmin: i32,
    ymin: i32,
    bytes: Vec<u8>,
) -> (usize, usize, i32, i32, Vec<u8>) {
    if w.checked_mul(h) != Some(bytes.len()) {
        return (w, h, xmin, ymin, bytes);
    }
    let rows = ink_row_extent(h, |y| bytes[y * w..(y + 1) * w].iter().any(|&a| a > 0));
    let cols = ink_row_extent(w, |x| (0..h).any(|y| bytes[y * w + x] > 0));
    let (Some((y0, y1)), Some((x0, x1))) = (rows, cols) else {
        return (w, h, xmin, ymin, bytes);
    };
    if (x0, y0, x1, y1) == (0, 0, w, h) {
        return (w, h, xmin, ymin, bytes);
    }
    let (nw, nh) = (x1 - x0, y1 - y0);
    let mut out = Vec::with_capacity(nw * nh);
    for row in bytes.chunks_exact(w).skip(y0).take(nh) {
        out.extend_from_slice(&row[x0..x1]);
    }
    let lift = |n: usize| i32::try_from(n).unwrap_or(i32::MAX);
    (
        nw,
        nh,
        xmin.saturating_add(lift(x0)),
        ymin.saturating_add(lift(h - y1)),
        out,
    )
}

fn crop_mask_columns(
    src: &[u8],
    src_w: usize,
    height: usize,
    skip: usize,
    width: usize,
) -> Option<Vec<u8>> {
    if src.len() != src_w.checked_mul(height)? || skip.checked_add(width)? > src_w {
        return None;
    }
    let mut out = Vec::with_capacity(width.checked_mul(height)?);
    for row in src.chunks_exact(src_w) {
        out.extend_from_slice(&row[skip..skip + width]);
    }
    Some(out)
}

/// The runtime-fallback candidate rank for a face of OS/2 `weight` / `italic` against the primary's, smaller is better.
#[inline]
#[must_use]
pub const fn fallback_weight_rank(
    weight: u16,
    italic: bool,
    primary_weight: u16,
    primary_italic: bool,
) -> u32 {
    let dw = (weight as i32 - primary_weight as i32).unsigned_abs();
    dw + if italic != primary_italic { 1000 } else { 0 }
}

#[cfg(target_os = "macos")]
mod macos_coretext {
    use std::ffi::c_void;

    use crate::font::FaceBytes;

    type CFTypeRef = *const c_void;
    type CFStringRef = *const c_void;
    type CFURLRef = *const c_void;
    type CTFontRef = *const c_void;
    type CFAllocatorRef = *const c_void;

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const K_CF_URL_POSIX_PATH_STYLE: isize = 0;

    // SAFETY (whole block): CoreFoundation / CoreText entry points with the signatures in Apple's
    // headers. `font_path_for_char` upholds each contract: every `Create`/`Copy` result is released
    // exactly once, pointers passed in are live objects it created, and the UTF-16 unit count
    // handed to `CTFontCreateForString` matches the buffer.
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithBytes(
            alloc: CFAllocatorRef,
            bytes: *const u8,
            num_bytes: isize,
            encoding: u32,
            is_external: bool,
        ) -> CFStringRef;
        fn CFStringGetLength(s: CFStringRef) -> isize;
        fn CFStringGetCString(
            s: CFStringRef,
            buffer: *mut u8,
            buffer_size: isize,
            encoding: u32,
        ) -> bool;
        fn CFRelease(cf: CFTypeRef);
    }

    #[link(name = "CoreText", kind = "framework")]
    unsafe extern "C" {
        fn CTFontCopyAttribute(font: CTFontRef, attribute: CFStringRef) -> CFTypeRef;
        static kCTFontURLAttribute: CFStringRef;
    }

    /// Proof that AppKit's font bridge is registered in this process.
    ///
    /// Core Text gives each font and font descriptor the Objective-C class registered for its
    /// type when the object is made, and caches what it made. AppKit's load registers
    /// `NSCTFont`/`NSCTFontDescriptor`; an object made before that stays `__NSCFType` for its
    /// whole life, and a later `NSFont` (any window control's text) receives the cached
    /// object and raises `initWithTypefaceInfo:key:renderingMode:`, aborting the process.
    /// So every constructor in [`create`] takes this proof, and [`Bridged::registered`],
    /// its only constructor, loads AppKit first. That load registers classes only: it creates
    /// no application and opens no WindowServer connection.
    #[derive(Clone, Copy)]
    struct Bridged(());

    impl Bridged {
        fn registered() -> Self {
            aterm_macos_ui::appkit::load();
            Self(())
        }
    }

    /// The Core Text calls that make font and descriptor objects, each behind [`Bridged`].
    mod create {
        use super::{
            Bridged, CFArrayRef, CFDataRef, CFRange, CFStringRef, CTFontDescriptorRef, CTFontRef,
        };
        use std::ffi::c_void;

        #[link(name = "CoreText", kind = "framework")]
        unsafe extern "C" {
            fn CTFontCreateWithName(
                name: CFStringRef,
                size: f64,
                matrix: *const c_void,
            ) -> CTFontRef;
            fn CTFontCreateForString(
                current: CTFontRef,
                string: CFStringRef,
                range: CFRange,
            ) -> CTFontRef;
            fn CTFontManagerCreateFontDescriptorsFromData(data: CFDataRef) -> CFArrayRef;
            fn CTFontCreateWithFontDescriptor(
                desc: CTFontDescriptorRef,
                size: f64,
                matrix: *const c_void,
            ) -> CTFontRef;
            fn CTFontDescriptorCreateCopyWithVariation(
                original: CTFontDescriptorRef,
                axis_identifier: *const c_void,
                value: f64,
            ) -> CTFontDescriptorRef;
        }

        /// `CTFontCreateWithName(name, size, NULL)`.
        ///
        /// # Safety
        /// `name` is a live `CFString`. The caller releases a non-null result once.
        pub(super) unsafe fn font_with_name(_: Bridged, name: CFStringRef, size: f64) -> CTFontRef {
            // SAFETY: the caller's contract above; a null matrix is the identity.
            unsafe { CTFontCreateWithName(name, size, std::ptr::null()) }
        }

        /// `CTFontCreateForString(current, string, range)`.
        ///
        /// # Safety
        /// `current` is null or a live `CTFont`, `string` a live `CFString`, and `range` lies
        /// within `string`'s UTF-16 units. The caller releases a non-null result once.
        pub(super) unsafe fn font_for_string(
            _: Bridged,
            current: CTFontRef,
            string: CFStringRef,
            range: CFRange,
        ) -> CTFontRef {
            // SAFETY: the caller's contract above.
            unsafe { CTFontCreateForString(current, string, range) }
        }

        /// `CTFontManagerCreateFontDescriptorsFromData(data)`.
        ///
        /// # Safety
        /// `data` is a live `CFData`. The caller releases a non-null result once.
        pub(super) unsafe fn descriptors_from_data(_: Bridged, data: CFDataRef) -> CFArrayRef {
            // SAFETY: the caller's contract above.
            unsafe { CTFontManagerCreateFontDescriptorsFromData(data) }
        }

        /// `CTFontCreateWithFontDescriptor(desc, size, NULL)`.
        ///
        /// # Safety
        /// `desc` is a live `CTFontDescriptor`. The caller releases a non-null result once.
        pub(super) unsafe fn font_with_descriptor(
            _: Bridged,
            desc: CTFontDescriptorRef,
            size: f64,
        ) -> CTFontRef {
            // SAFETY: the caller's contract above; a null matrix is the identity.
            unsafe { CTFontCreateWithFontDescriptor(desc, size, std::ptr::null()) }
        }

        /// `CTFontDescriptorCreateCopyWithVariation(original, axis, value)`.
        ///
        /// # Safety
        /// `original` is a live `CTFontDescriptor` and `axis` a live `CFNumber`. The caller
        /// releases a non-null result once.
        pub(super) unsafe fn descriptor_with_variation(
            _: Bridged,
            original: CTFontDescriptorRef,
            axis: *const c_void,
            value: f64,
        ) -> CTFontDescriptorRef {
            // SAFETY: the caller's contract above.
            unsafe { CTFontDescriptorCreateCopyWithVariation(original, axis, value) }
        }
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFURLCopyFileSystemPath(url: CFURLRef, path_style: isize) -> CFStringRef;
    }

    #[repr(C)]
    struct CFRange {
        location: isize,
        length: isize,
    }

    /// SAFETY: see the extern block's block-level note; `s.as_ptr()`/`s.len()` describe a valid
    /// UTF-8 buffer for the duration of the call.
    unsafe fn cfstring(s: &str) -> CFStringRef {
        unsafe {
            CFStringCreateWithBytes(
                std::ptr::null(),
                s.as_ptr(),
                s.len() as isize,
                K_CF_STRING_ENCODING_UTF8,
                false,
            )
        }
    }

    /// SAFETY: `s` must be a live `CFStringRef`.
    unsafe fn cfstring_to_string(s: CFStringRef) -> Option<String> {
        if s.is_null() {
            return None;
        }
        unsafe {
            let len = CFStringGetLength(s);
            let cap = (len as usize).saturating_mul(4).saturating_add(1).max(8);
            let mut buf = vec![0u8; cap];
            if !CFStringGetCString(s, buf.as_mut_ptr(), cap as isize, K_CF_STRING_ENCODING_UTF8) {
                return None;
            }
            let nul = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            buf.truncate(nul);
            String::from_utf8(buf).ok()
        }
    }

    /// Resolve the file path of the font CoreText would use to render `ch`.
    pub fn font_path_for_char(ch: char) -> Option<String> {
        let mut u16buf = [0u16; 2];
        let utf16 = ch.encode_utf16(&mut u16buf);
        let utf16_len = utf16.len() as isize;
        let s = ch.to_string();
        let bridged = Bridged::registered();
        // SAFETY: every Create/Copy below is released exactly once on all paths via
        // the `release` helpers; we never deref the opaque pointers. See the extern
        // block's block-level SAFETY note.
        unsafe {
            let cf = cfstring(&s);
            if cf.is_null() {
                return None;
            }
            let base_name = cfstring("Helvetica");
            let base = if base_name.is_null() {
                std::ptr::null()
            } else {
                create::font_with_name(bridged, base_name, 12.0)
            };
            if !base_name.is_null() {
                CFRelease(base_name);
            }
            let range = CFRange {
                location: 0,
                length: utf16_len,
            };
            let substituted = create::font_for_string(bridged, base, cf, range);
            if !base.is_null() {
                CFRelease(base);
            }
            CFRelease(cf);
            if substituted.is_null() {
                return None;
            }
            let url = CTFontCopyAttribute(substituted, kCTFontURLAttribute) as CFURLRef;
            CFRelease(substituted);
            if url.is_null() {
                return None;
            }
            let path_cf = CFURLCopyFileSystemPath(url, K_CF_URL_POSIX_PATH_STYLE);
            CFRelease(url as CFTypeRef);
            let path = cfstring_to_string(path_cf);
            if !path_cf.is_null() {
                CFRelease(path_cf);
            }
            path
        }
    }

    type CFDataRef = *const c_void;
    type CFArrayRef = *const c_void;
    type CTFontDescriptorRef = *const c_void;
    type CGColorSpaceRef = *const c_void;
    type CGContextRef = *const c_void;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPoint {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGSize {
        width: f64,
        height: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }

    #[repr(C)]
    struct CFAllocatorContext {
        version: isize,
        info: *mut c_void,
        retain: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
        release: Option<unsafe extern "C" fn(*const c_void)>,
        copy_description: Option<unsafe extern "C" fn(*const c_void) -> CFStringRef>,
        allocate: Option<unsafe extern "C" fn(isize, usize, *mut c_void) -> *mut c_void>,
        reallocate:
            Option<unsafe extern "C" fn(*mut c_void, isize, usize, *mut c_void) -> *mut c_void>,
        deallocate: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
        preferred_size: Option<unsafe extern "C" fn(isize, usize, *mut c_void) -> isize>,
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFDataCreateWithBytesNoCopy(
            alloc: CFAllocatorRef,
            bytes: *const u8,
            length: isize,
            deallocator: CFAllocatorRef,
        ) -> CFDataRef;
        fn CFAllocatorCreate(
            alloc: CFAllocatorRef,
            context: *mut CFAllocatorContext,
        ) -> CFAllocatorRef;
        #[cfg(test)]
        fn CFDataGetBytePtr(data: CFDataRef) -> *const u8;
        fn CFArrayGetCount(arr: CFArrayRef) -> isize;
        fn CFArrayGetValueAtIndex(arr: CFArrayRef, idx: isize) -> *const c_void;
        fn CFNumberCreate(
            alloc: CFAllocatorRef,
            the_type: isize,
            value_ptr: *const c_void,
        ) -> *const c_void;
    }

    const CF_NUMBER_SINT32_TYPE: isize = 3;

    #[link(name = "CoreText", kind = "framework")]
    unsafe extern "C" {
        fn CTFontGetBoundingRectsForGlyphs(
            font: CTFontRef,
            orientation: u32,
            glyphs: *const u16,
            rects: *mut CGRect,
            count: isize,
        ) -> CGRect;
        fn CTFontGetAdvancesForGlyphs(
            font: CTFontRef,
            orientation: u32,
            glyphs: *const u16,
            advances: *mut CGSize,
            count: isize,
        ) -> f64;
        fn CTFontDrawGlyphs(
            font: CTFontRef,
            glyphs: *const u16,
            positions: *const CGPoint,
            count: usize,
            context: CGContextRef,
        );
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGColorSpaceCreateDeviceGray() -> CGColorSpaceRef;
        fn CGColorSpaceRelease(cs: CGColorSpaceRef);
        fn CGBitmapContextCreate(
            data: *mut c_void,
            width: usize,
            height: usize,
            bits_per_component: usize,
            bytes_per_row: usize,
            space: CGColorSpaceRef,
            bitmap_info: u32,
        ) -> CGContextRef;
        fn CGContextRelease(ctx: CGContextRef);
        fn CGContextSetShouldAntialias(ctx: CGContextRef, v: bool);
        fn CGContextSetShouldSmoothFonts(ctx: CGContextRef, v: bool);
        fn CGContextSetAllowsFontSmoothing(ctx: CGContextRef, v: bool);
        fn CGContextSetGrayFillColor(ctx: CGContextRef, gray: f64, alpha: f64);
    }

    /// A rasterized glyph: 8-bit coverage and placement in the portable raster's convention (`xmin`/`ymin` baseline-relative with y up; `bytes` row-major, top row first), so it drops straight into [`super::GlyphImage::Mono`].
    pub struct RasterGlyph {
        pub width: usize,
        pub height: usize,
        pub xmin: i32,
        pub ymin: i32,
        pub advance: f32,
        pub bytes: Vec<u8>,
    }

    unsafe extern "C" fn release_face_bytes(_bytes: *mut c_void, info: *mut c_void) {
        if !info.is_null() {
            // SAFETY: `info` is the `Box<FaceBytes>` `CtFaceDescriptors::new` leaked for exactly this call, and CF invokes a deallocator once.
            drop(unsafe { Box::from_raw(info.cast::<FaceBytes>()) });
        }
    }

    unsafe extern "C" fn never_allocate(
        _size: isize,
        _hint: usize,
        _info: *mut c_void,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// CoreText's parse of one font file.
    pub struct CtFaceDescriptors {
        data: CFDataRef,
        descs: CFArrayRef,
        _keepalive: FaceBytes,
        /// Carried to [`CtFont::from_descriptors`], which makes fonts from this parse.
        bridged: Bridged,
    }

    // SAFETY: `data`/`descs` are immutable Core Foundation objects (atomic retain/release, thread-safe reads) and `FaceBytes` is `Send + Sync`, so the parse may be moved or shared across threads like `CtFont`.

    unsafe impl Send for CtFaceDescriptors {}

    unsafe impl Sync for CtFaceDescriptors {}

    impl Drop for CtFaceDescriptors {
        fn drop(&mut self) {
            // SAFETY: both are live CF objects this value created, each released once. Releasing `data` does not free the bytes: CF's boxed handle goes only with the data object's last reference (ours or CoreText's), via the deallocator.
            unsafe {
                CFRelease(self.descs);
                CFRelease(self.data);
            }
        }
    }

    impl CtFaceDescriptors {
        /// Parse `bytes` once.
        pub fn new(bytes: FaceBytes) -> Option<Self> {
            let bridged = Bridged::registered();
            let cf_handle: *mut FaceBytes = Box::into_raw(Box::new(bytes.clone()));
            let mut context = CFAllocatorContext {
                version: 0,
                info: cf_handle.cast(),
                retain: None,
                release: None,
                copy_description: None,
                allocate: Some(never_allocate),
                reallocate: None,
                deallocate: Some(release_face_bytes),
                preferred_size: None,
            };
            // SAFETY: `context` outlives the `CFAllocatorCreate` call, which
            // copies it. The CFData retains `deallocator` for its lifetime, so
            // our reference is released as soon as the data exists; if the data
            // cannot be created the boxed handle is reclaimed here instead. The
            // byte pointer stays valid for the data's whole life because the
            // boxed handle (and `_keepalive`) pin the allocation or mapping.
            unsafe {
                let deallocator = CFAllocatorCreate(std::ptr::null(), &raw mut context);
                if deallocator.is_null() {
                    drop(Box::from_raw(cf_handle));
                    return None;
                }
                let data = CFDataCreateWithBytesNoCopy(
                    std::ptr::null(),
                    bytes.as_ptr(),
                    bytes.len() as isize,
                    deallocator,
                );
                if data.is_null() {
                    CFRelease(deallocator);
                    drop(Box::from_raw(cf_handle));
                    return None;
                }
                CFRelease(deallocator);
                let descs = create::descriptors_from_data(bridged, data);
                if descs.is_null() {
                    CFRelease(data);
                    return None;
                }
                Some(Self {
                    data,
                    descs,
                    _keepalive: bytes,
                    bridged,
                })
            }
        }

        /// Test only: the address the `CFData` presents, equal to the handle's own bytes when nothing was copied.
        #[cfg(test)]
        pub fn data_ptr(&self) -> *const u8 {
            // SAFETY: `data` is a live CFData; the accessor only reads it.
            unsafe { CFDataGetBytePtr(self.data) }
        }
    }

    impl CtFont {
        /// Test only: the `CTFont` this value owns, borrowed for as long as `self` lives.
        #[cfg(test)]
        pub fn as_ptr(&self) -> *const c_void {
            self.font
        }
    }

    /// A CoreText font handle for one face of some font bytes at a fixed pixel size.
    pub struct CtFont {
        font: CTFontRef,
        _keepalive: FaceBytes,
    }

    // SAFETY: `font` (CTFontRef) is an immutable Core Foundation object; CFRetain/CFRelease
    // are atomic and reads thread-safe, and `FaceBytes` is `Send + Sync`, so a `CtFont` may
    // be moved or shared across threads (`GpuRenderer::new_with_family` builds the CPU
    // `Renderer` on a worker thread).

    unsafe impl Send for CtFont {}

    unsafe impl Sync for CtFont {}

    impl Drop for CtFont {
        fn drop(&mut self) {
            // SAFETY: `font` is a live CT object we created; released exactly
            // once. See the block-level SAFETY note above.
            unsafe {
                if !self.font.is_null() {
                    CFRelease(self.font);
                }
            }
        }
    }

    impl CtFont {
        /// Build a CTFont for the `index`-th face of `bytes` at `px` device pixels, instantiated at `variations`.
        pub fn new(
            bytes: FaceBytes,
            index: u32,
            px: f32,
            variations: &[(u32, f32)],
        ) -> Option<CtFont> {
            let descs = CtFaceDescriptors::new(bytes)?;
            Self::from_descriptors(&descs, index, px, variations)
        }

        /// [`Self::new`] off an already-parsed file.
        pub fn from_descriptors(
            descs: &CtFaceDescriptors,
            index: u32,
            px: f32,
            variations: &[(u32, f32)],
        ) -> Option<CtFont> {
            // SAFETY: descriptors (base + each derived variation copy) are
            // released after use; the created font is retained in `CtFont` and
            // released once in `Drop`. Opaque pointers are never dereferenced.
            unsafe {
                let descs_ref = descs.descs;
                let count = CFArrayGetCount(descs_ref);
                let font = if (index as isize) < count {
                    let mut desc = CFArrayGetValueAtIndex(descs_ref, index as isize);
                    let mut owned: CTFontDescriptorRef = std::ptr::null();
                    for &(tag, value) in variations {
                        let tag_i32 = tag as i32;
                        let axis = CFNumberCreate(
                            std::ptr::null(),
                            CF_NUMBER_SINT32_TYPE,
                            (&raw const tag_i32).cast(),
                        );
                        if axis.is_null() {
                            continue;
                        }
                        let derived = create::descriptor_with_variation(
                            descs.bridged,
                            desc,
                            axis,
                            f64::from(value),
                        );
                        CFRelease(axis);
                        if derived.is_null() {
                            continue;
                        }
                        if !owned.is_null() {
                            CFRelease(owned);
                        }
                        owned = derived;
                        desc = derived;
                    }
                    let font = create::font_with_descriptor(descs.bridged, desc, px as f64);
                    if !owned.is_null() {
                        CFRelease(owned);
                    }
                    font
                } else {
                    std::ptr::null()
                };
                if font.is_null() {
                    return None;
                }
                Some(CtFont {
                    font,
                    _keepalive: descs._keepalive.clone(),
                })
            }
        }

        /// Rasterize glyph `gid` to an 8-bit coverage mask via a grayscale CoreGraphics context (antialiased, font smoothing off: pure coverage, so the downstream linear-light blend does the gamma).
        pub fn rasterize(&self, gid: u16, thicken: bool) -> Option<RasterGlyph> {
            // SAFETY: single-glyph CoreText/CoreGraphics calls with correctly sized
            // buffers; the bitmap context writes into `buf` (w*h bytes, 1 byte/px);
            // context + colorspace are released once. See block-level SAFETY note.
            unsafe {
                let glyphs = [gid];
                let mut rect = CGRect {
                    origin: CGPoint { x: 0.0, y: 0.0 },
                    size: CGSize {
                        width: 0.0,
                        height: 0.0,
                    },
                };
                CTFontGetBoundingRectsForGlyphs(self.font, 0, glyphs.as_ptr(), &mut rect, 1);
                let mut adv = CGSize {
                    width: 0.0,
                    height: 0.0,
                };
                CTFontGetAdvancesForGlyphs(self.font, 0, glyphs.as_ptr(), &mut adv, 1);
                let advance = adv.width as f32;

                let pad = if thicken { 2.0_f64 } else { 1.0_f64 };
                if !(rect.size.width > 0.0 && rect.size.height > 0.0) {
                    return Some(RasterGlyph {
                        width: 0,
                        height: 0,
                        xmin: 0,
                        ymin: 0,
                        advance,
                        bytes: Vec::new(),
                    });
                }
                let bx = rect.origin.x - pad;
                let by = rect.origin.y - pad;
                let (pen_x, xmin) = super::ct_pen_and_bearing(bx);
                let (pen_y, ymin) = super::ct_pen_and_bearing(by);
                let w = super::ct_padded_extent(rect.size.width, pad);
                let h = super::ct_padded_extent(rect.size.height, pad);

                let mut buf = vec![0u8; w * h];
                let cs = CGColorSpaceCreateDeviceGray();
                if cs.is_null() {
                    return None;
                }
                let ctx = CGBitmapContextCreate(buf.as_mut_ptr() as *mut c_void, w, h, 8, w, cs, 0);
                CGColorSpaceRelease(cs);
                if ctx.is_null() {
                    return None;
                }
                CGContextSetShouldAntialias(ctx, true);
                CGContextSetShouldSmoothFonts(ctx, thicken);
                CGContextSetAllowsFontSmoothing(ctx, thicken);
                CGContextSetGrayFillColor(ctx, 1.0, 1.0);
                let pos = [CGPoint { x: pen_x, y: pen_y }];
                CTFontDrawGlyphs(self.font, glyphs.as_ptr(), pos.as_ptr(), 1, ctx);
                CGContextRelease(ctx);

                Some(RasterGlyph {
                    width: w,
                    height: h,
                    xmin,
                    ymin,
                    advance,
                    bytes: buf,
                })
            }
        }

        /// Test only: the raw CoreText ink rect for `gid` as `(x, y, w, h)` (baseline relative, y up), so the conformance test can bind [`Self::rasterize`]'s placement to [`super::ct_pen_and_bearing`] / [`super::ct_padded_extent`] on real fonts.
        #[cfg(test)]
        pub fn ink_rect(&self, gid: u16) -> (f64, f64, f64, f64) {
            // SAFETY: a single-glyph query with a correctly sized out-buffer;
            // see the extern block's block-level SAFETY note.
            unsafe {
                let glyphs = [gid];
                let mut rect = CGRect {
                    origin: CGPoint { x: 0.0, y: 0.0 },
                    size: CGSize {
                        width: 0.0,
                        height: 0.0,
                    },
                };
                CTFontGetBoundingRectsForGlyphs(self.font, 0, glyphs.as_ptr(), &mut rect, 1);
                (
                    rect.origin.x,
                    rect.origin.y,
                    rect.size.width,
                    rect.size.height,
                )
            }
        }
    }

    type CFAttributedStringRef = *const c_void;
    type CFDictionaryRef = *const c_void;
    type CTLineRef = *const c_void;
    type CTRunRef = *const c_void;

    #[repr(C)]
    struct CFDictionaryKeyCallBacks {
        version: isize,
        retain: *const c_void,
        release: *const c_void,
        copy_description: *const c_void,
        equal: *const c_void,
        hash: *const c_void,
    }

    #[repr(C)]
    struct CFDictionaryValueCallBacks {
        version: isize,
        retain: *const c_void,
        release: *const c_void,
        copy_description: *const c_void,
        equal: *const c_void,
    }

    // SAFETY (whole block): CoreFoundation / CoreText entry points with the signatures in Apple's
    // headers. `CtFont::shape` upholds each contract: every `Create` result is released exactly
    // once, the `Get` results are borrowed from objects it keeps alive across their use, and every
    // out-buffer holds the run's glyph count.
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFDictionaryCreate(
            alloc: CFAllocatorRef,
            keys: *const *const c_void,
            values: *const *const c_void,
            num_values: isize,
            key_callbacks: *const CFDictionaryKeyCallBacks,
            value_callbacks: *const CFDictionaryValueCallBacks,
        ) -> CFDictionaryRef;
        fn CFDictionaryGetValue(dict: CFDictionaryRef, key: *const c_void) -> *const c_void;
        fn CFAttributedStringCreate(
            alloc: CFAllocatorRef,
            string: CFStringRef,
            attributes: CFDictionaryRef,
        ) -> CFAttributedStringRef;
        fn CFEqual(a: CFTypeRef, b: CFTypeRef) -> bool;
        static kCFTypeDictionaryKeyCallBacks: CFDictionaryKeyCallBacks;
        static kCFTypeDictionaryValueCallBacks: CFDictionaryValueCallBacks;
    }

    #[link(name = "CoreText", kind = "framework")]
    unsafe extern "C" {
        fn CTLineCreateWithAttributedString(string: CFAttributedStringRef) -> CTLineRef;
        fn CTLineGetGlyphRuns(line: CTLineRef) -> CFArrayRef;
        fn CTRunGetGlyphCount(run: CTRunRef) -> isize;
        fn CTRunGetAttributes(run: CTRunRef) -> CFDictionaryRef;
        fn CTRunGetGlyphs(run: CTRunRef, range: CFRange, buffer: *mut u16);
        fn CTRunGetPositions(run: CTRunRef, range: CFRange, buffer: *mut CGPoint);
        fn CTRunGetAdvances(run: CTRunRef, range: CFRange, buffer: *mut CGSize);
        fn CTRunGetStringIndices(run: CTRunRef, range: CFRange, buffer: *mut isize);
        static kCTFontAttributeName: CFStringRef;
    }

    /// One glyph CoreText shaped.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct CtShapedGlyph {
        pub gid: u16,
        pub cluster: u32,
        pub x_advance: i32,
        pub x_offset: i32,
        pub y_offset: i32,
    }

    impl CtFont {
        /// Shape `text` with CoreText on this font, glyphs in visual order.
        pub fn shape(&self, text: &str) -> Option<Vec<CtShapedGlyph>> {
            let mut utf8_of_utf16: Vec<u32> = Vec::with_capacity(text.len());
            for (offset, ch) in text.char_indices() {
                for _ in 0..ch.len_utf16() {
                    utf8_of_utf16.push(offset as u32);
                }
            }
            let mut out = Vec::new();
            // SAFETY: see the extern blocks' notes. `string`, `attrs`, `attributed`
            // and `line` are created here and released once each below, on every
            // path; `runs` and each run's attributes are borrowed from `line`,
            // which outlives their use; each out-buffer is sized to the run's
            // glyph count, which the range asks for in full.
            unsafe {
                let string = cfstring(text);
                if string.is_null() {
                    return None;
                }
                let keys = [kCTFontAttributeName];
                let values = [self.font];
                let attrs = CFDictionaryCreate(
                    std::ptr::null(),
                    keys.as_ptr(),
                    values.as_ptr(),
                    1,
                    &raw const kCFTypeDictionaryKeyCallBacks,
                    &raw const kCFTypeDictionaryValueCallBacks,
                );
                if attrs.is_null() {
                    CFRelease(string);
                    return None;
                }
                let attributed = CFAttributedStringCreate(std::ptr::null(), string, attrs);
                CFRelease(attrs);
                CFRelease(string);
                if attributed.is_null() {
                    return None;
                }
                let line = CTLineCreateWithAttributedString(attributed);
                CFRelease(attributed);
                if line.is_null() {
                    return None;
                }
                let runs = CTLineGetGlyphRuns(line);
                let mut ok = !runs.is_null();
                let mut pen = 0.0f64;
                let run_count = if ok { CFArrayGetCount(runs) } else { 0 };
                for r in 0..run_count {
                    let run = CFArrayGetValueAtIndex(runs, r);
                    let run_font =
                        CFDictionaryGetValue(CTRunGetAttributes(run), kCTFontAttributeName);
                    if run_font.is_null() || !CFEqual(run_font, self.font) {
                        ok = false;
                        break;
                    }
                    let count = CTRunGetGlyphCount(run);
                    let Ok(n) = usize::try_from(count) else {
                        ok = false;
                        break;
                    };
                    let all = || CFRange {
                        location: 0,
                        length: count,
                    };
                    let mut glyphs = vec![0u16; n];
                    let mut positions = vec![CGPoint { x: 0.0, y: 0.0 }; n];
                    let mut advances = vec![
                        CGSize {
                            width: 0.0,
                            height: 0.0,
                        };
                        n
                    ];
                    let mut indices = vec![0isize; n];
                    CTRunGetGlyphs(run, all(), glyphs.as_mut_ptr());
                    CTRunGetPositions(run, all(), positions.as_mut_ptr());
                    CTRunGetAdvances(run, all(), advances.as_mut_ptr());
                    CTRunGetStringIndices(run, all(), indices.as_mut_ptr());
                    for i in 0..n {
                        let cluster = usize::try_from(indices[i])
                            .ok()
                            .and_then(|u| utf8_of_utf16.get(u))
                            .copied()
                            .unwrap_or(u32::MAX);
                        out.push(CtShapedGlyph {
                            gid: glyphs[i],
                            cluster,
                            x_advance: advances[i].width.round() as i32,
                            x_offset: (positions[i].x - pen).round() as i32,
                            y_offset: positions[i].y.round() as i32,
                        });
                        pen += advances[i].width;
                    }
                }
                CFRelease(line);
                ok.then_some(out)
            }
        }
    }

    static EMOJI_SHAPING_FONT: std::sync::Mutex<Option<((usize, usize), CtFont)>> =
        std::sync::Mutex::new(None);

    /// [`CtFont::shape`] on face 0 of the colour face `bytes`.
    pub fn shape_emoji(bytes: &FaceBytes, text: &str) -> Option<Vec<CtShapedGlyph>> {
        let key = (bytes.as_ptr() as usize, bytes.len());
        let mut slot = EMOJI_SHAPING_FONT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.as_ref().map(|(k, _)| *k) != Some(key) {
            let units_per_em = aterm_font::Face::parse(bytes, 0).ok()?.units_per_em();
            let font = CtFont::new(bytes.clone(), 0, f32::from(units_per_em), &[])?;
            *slot = Some((key, font));
        }
        slot.as_ref()?.1.shape(text)
    }
}

/// The face-selection policy for a code point's ordinary (non-VS16) text dispatch, a pure function of coverage facts so it can be verified without font i/o (`crates/aterm-render/tests/presentation_gate.rs`).
#[inline]
#[must_use]
pub const fn select_face(
    procedural: bool,
    primary_has: bool,
    fallback_has: bool,
    symbol_has: bool,
    color_has: bool,
    wants_emoji: bool,
) -> FaceId {
    if procedural {
        FaceId::Procedural
    } else if primary_has {
        FaceId::Primary
    } else if fallback_has {
        FaceId::Fallback
    } else if symbol_has {
        FaceId::SymbolFallback
    } else if color_has && wants_emoji {
        FaceId::ColorEmoji
    } else if color_has {
        FaceId::ColorEmojiMono
    } else {
        FaceId::Primary
    }
}

/// Which real face a styled (bold / italic / bold-italic) primary-family glyph or ligature run draws from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FacePick {
    /// No real styled face serves the style.
    Primary,
    /// The host-injected real bold face ([`Renderer::set_bold_font`] / config `font_family_bold`).
    InjectedBold { synthetic: StyleBits },
    /// A real styled sibling / collection / injected face (`Renderer::styled_faces[slot]`; 0 = bold, 1 = italic, 2 = bold-italic).
    Styled { slot: usize, synthetic: StyleBits },
}

/// The styled-face resolution policy, a pure function of the requested style and face availability
/// so `crates/aterm-render/tests/styled_faces.rs` can enumerate all 2^6 inputs.
#[inline]
#[must_use]
pub fn resolve_styled_face(style: StyleBits, injected_bold: bool, covers: [bool; 3]) -> FacePick {
    resolve_styled_face_lazy(style, injected_bold, |slot| covers[slot])
}

#[inline]
fn resolve_styled_face_lazy(
    style: StyleBits,
    injected_bold: bool,
    mut covers: impl FnMut(usize) -> bool,
) -> FacePick {
    let bold = style.contains(StyleBits::BOLD);
    let italic = style.contains(StyleBits::ITALIC);
    match (bold, italic) {
        (false, false) => FacePick::Primary,
        (true, false) => {
            if injected_bold {
                FacePick::InjectedBold {
                    synthetic: StyleBits::REGULAR,
                }
            } else if covers(0) {
                FacePick::Styled {
                    slot: 0,
                    synthetic: StyleBits::REGULAR,
                }
            } else {
                FacePick::Primary
            }
        }
        (false, true) => {
            if covers(1) {
                FacePick::Styled {
                    slot: 1,
                    synthetic: StyleBits::REGULAR,
                }
            } else {
                FacePick::Primary
            }
        }
        (true, true) => {
            if covers(2) {
                FacePick::Styled {
                    slot: 2,
                    synthetic: StyleBits::REGULAR,
                }
            } else if injected_bold {
                FacePick::InjectedBold {
                    synthetic: StyleBits::ITALIC,
                }
            } else if covers(0) {
                FacePick::Styled {
                    slot: 0,
                    synthetic: StyleBits::ITALIC,
                }
            } else if covers(1) {
                FacePick::Styled {
                    slot: 1,
                    synthetic: StyleBits::BOLD,
                }
            } else {
                FacePick::Primary
            }
        }
    }
}

/// The fallback-chain precedence law.
#[must_use]
pub fn fallback_chain_order(config: &[String], discovery: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(config.len() + discovery.len());
    out.extend(config.iter().cloned());
    out.extend(discovery.iter().cloned());
    out
}

/// One axis of the grid-fit, remainder-absorbing padding split computed by [`pad_split`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PadSplit {
    /// Grid cells along this axis (always `>= 1`).
    pub cells: usize,
    /// Leading (left/top) padding band, px.
    pub pad_lo: usize,
    /// Trailing (right/bottom) padding band, px.
    pub pad_hi: usize,
}

/// The window-fit policy for one axis.
#[inline]
#[must_use]
pub fn pad_split(win_px: usize, pad: usize, cell_px: usize) -> PadSplit {
    let cell = cell_px.max(1);
    let usable = win_px.saturating_sub(2 * pad);
    let cells = (usable / cell).max(1);
    let rem = win_px.saturating_sub(2 * pad + cells * cell);
    PadSplit {
        cells,
        pad_lo: pad + rem / 2,
        pad_hi: pad + (rem - rem / 2),
    }
}

/// Raw framebuffer extent.
#[inline]
#[must_use]
pub const fn frame_size_px(
    rows: usize,
    cols: usize,
    cell_w: usize,
    cell_h: usize,
    pad: usize,
    head: usize,
) -> (usize, usize) {
    let border = pad.saturating_mul(2);
    (
        cols.saturating_mul(cell_w).saturating_add(border),
        rows.saturating_mul(cell_h)
            .saturating_add(border)
            .saturating_add(head),
    )
}

/// Maximum storage for one packed or RGBA8 framebuffer.
pub const MAX_FRAME_BYTES: usize = 256 * 1024 * 1024;

/// Check the shared frame geometry and storage limit before allocating.
pub fn checked_frame_size_px(
    rows: usize,
    cols: usize,
    cell_w: usize,
    cell_h: usize,
    pad: usize,
    head: usize,
) -> Result<(usize, usize), String> {
    let (w, h) = frame_size_px(rows, cols, cell_w, cell_h, pad, head);
    let bytes = w.checked_mul(h).and_then(|n| n.checked_mul(4));
    if w > MAX_FRAME_BYTES / 4
        || h > MAX_FRAME_BYTES / 4
        || bytes.is_none_or(|n| n > MAX_FRAME_BYTES)
    {
        return Err(format!(
            "framebuffer {w}x{h} exceeds the {MAX_FRAME_BYTES}-byte limit"
        ));
    }
    Ok((w, h))
}

/// Where the (grid + `2·pad`) frame's leading edge sits inside a raw-window surface of `dst_px`, for one axis.
#[inline]
#[must_use]
pub const fn band_offset(dst_px: usize, src_px: usize) -> i64 {
    (dst_px as i64 - src_px as i64).div_euclid(2)
}

/// The vertical placement policy.
#[inline]
#[must_use]
pub const fn band_offset_y(dst_px: usize, src_px: usize) -> i64 {
    if cfg!(target_os = "linux") {
        0
    } else {
        band_offset(dst_px, src_px)
    }
}

/// One axis of a frame's placement inside a surface ([`frame_placement`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AxisPlacement {
    /// Where the frame's leading edge sits in the surface.
    pub origin: i64,
    /// Surface pixels before the frame.
    pub band_before: usize,
    /// Frame pixels on the surface, starting at surface pixel `band_before` and frame pixel `crop_before`.
    pub visible: usize,
    /// Surface pixels after the frame.
    pub band_after: usize,
    /// Frame pixels cut before the visible run.
    pub crop_before: usize,
    /// Frame pixels cut after the visible run.
    pub crop_after: usize,
}

impl AxisPlacement {
    #[inline]
    fn at(origin: i64, surface: usize, frame: usize) -> Self {
        let surface_px = i64::try_from(surface).unwrap_or(i64::MAX);
        let frame_px = i64::try_from(frame).unwrap_or(i64::MAX);
        let end = origin.saturating_add(frame_px);
        let start = origin.clamp(0, surface_px);
        let stop = end.clamp(start, surface_px);
        let px = |value: i64| usize::try_from(value).unwrap_or(0);
        Self {
            origin,
            band_before: px(start),
            visible: px(stop - start),
            band_after: px(surface_px - stop),
            crop_before: px(start - origin),
            crop_after: px(end - stop),
        }
    }

    /// The frame pixel shown at surface pixel `surface_px`.
    #[inline]
    #[must_use]
    pub fn source(&self, surface_px: usize) -> Option<usize> {
        surface_px
            .checked_sub(self.band_before)
            .filter(|&run| run < self.visible)
            .map(|run| run + self.crop_before)
    }
}

/// Where a `src_w`×`src_h` frame lands in a `dst_w`×`dst_h` surface.
#[inline]
#[must_use]
pub fn frame_placement(dst_w: usize, dst_h: usize, src_w: usize, src_h: usize) -> FramePlacement {
    FramePlacement {
        x: AxisPlacement::at(band_offset(dst_w, src_w), dst_w, src_w),
        y: AxisPlacement::at(band_offset_y(dst_h, src_h), dst_h, src_h),
    }
}

/// A frame's placement in a surface, per axis ([`frame_placement`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FramePlacement {
    pub x: AxisPlacement,
    pub y: AxisPlacement,
}

/// A rectangle of whole pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl FramePlacement {
    /// Where the frame's visible pixels are in the `surface_w`×`surface_h` surface it was placed in.
    #[inline]
    #[must_use]
    pub fn visible_rect(&self, surface_w: usize, surface_h: usize) -> Option<PixelRect> {
        let spans = |axis: &AxisPlacement| {
            axis.band_before
                .checked_add(axis.visible)?
                .checked_add(axis.band_after)
        };
        (spans(&self.x)? == surface_w && spans(&self.y)? == surface_h).then_some(PixelRect {
            x: self.x.band_before,
            y: self.y.band_before,
            width: self.x.visible,
            height: self.y.visible,
        })
    }
}

/// Place the rendered frame (`src`, `src_w`×`src_h`, packed `0x00RRGGBB`) into a raw-window-sized destination (`dst_w`×`dst_h`) at its [`frame_placement`], painting the surrounding remainder bands `band_rgb`.
#[allow(
    clippy::too_many_arguments,
    reason = "a destination surface + a source frame + the present flags is irreducibly 9 scalars; a struct would only relocate the list"
)]
pub fn place_frame_bands(
    dst: &mut [u32],
    dst_w: usize,
    dst_h: usize,
    src: &[u32],
    src_w: usize,
    src_h: usize,
    invert: bool,
    band_rgb: u32,
    edge_rows: std::ops::Range<usize>,
) {
    let band = band_rgb & 0x00ff_ffff;
    let xor = if invert { 0x00ff_ffff } else { 0 };
    let place = frame_placement(dst_w, dst_h, src_w, src_h);
    let x0 = place.x.band_before;
    let x1 = x0 + place.x.visible;
    let sx0 = place.x.crop_before;
    for y in 0..dst_h {
        let Some(row) = dst.get_mut(y * dst_w..y * dst_w + dst_w) else {
            break;
        };
        let Some(sy) = place.y.source(y) else {
            row.fill(band);
            continue;
        };
        let (lo, hi) = if edge_rows.contains(&sy) && src_w > 0 {
            let base = sy * src_w;
            (src[base] & 0x00ff_ffff, src[base + src_w - 1] & 0x00ff_ffff)
        } else {
            (band, band)
        };
        row[..x0].fill(lo);
        row[x1..].fill(hi);
        let srow = &src[sy * src_w + sx0..sy * src_w + sx0 + (x1 - x0)];
        if invert {
            for (d, &s) in row[x0..x1].iter_mut().zip(srow) {
                *d = s ^ xor;
            }
        } else {
            row[x0..x1].copy_from_slice(srow);
        }
    }
}

/// Continue the chrome rows' edge pixels through the remainder bands again, from the frame as it now stands on the surface.
pub fn continue_chrome_edges(
    dst: &mut [u32],
    dst_w: usize,
    dst_h: usize,
    src_w: usize,
    src_h: usize,
    edge_rows: std::ops::Range<usize>,
) {
    let place = frame_placement(dst_w, dst_h, src_w, src_h);
    let x0 = place.x.band_before;
    let x1 = x0 + place.x.visible;
    if x1 <= x0 || (x0 == 0 && x1 == dst_w) {
        return;
    }
    for y in 0..dst_h {
        if !place.y.source(y).is_some_and(|sy| edge_rows.contains(&sy)) {
            continue;
        }
        let Some(row) = dst.get_mut(y * dst_w..y * dst_w + dst_w) else {
            break;
        };
        let (lo, hi) = (row[x0] & 0x00ff_ffff, row[x1 - 1] & 0x00ff_ffff);
        row[..x0].fill(lo);
        row[x1..].fill(hi);
    }
}

/// The drop-target overlay's inset border over a `frame_w`×`frame_h` frame, in px: the short side
/// over 200 within `2..=6`, scaled by `scale_q4` sixteenths (0 keeps it; at least a pixel once
/// scaled), and at most half the frame on each axis. The CPU presenter and each GPU arm's present
/// draw their overlay at this width, and the GPU frame oracle's twins hold each arm to it.
#[inline]
#[must_use]
pub fn drop_overlay_border_px(frame_w: usize, frame_h: usize, scale_q4: u8) -> usize {
    let base = (frame_w.min(frame_h) / 200).clamp(2, 6);
    let scaled = if scale_q4 == 0 {
        base
    } else {
        (base * usize::from(scale_q4) / 16).max(1)
    };
    scaled.min(frame_w / 2).min(frame_h / 2).max(1)
}

const FONT_DIRS: &[&str] = &[
    #[cfg(target_os = "macos")]
    "Library/Fonts",
    ".fonts",
    ".local/share/fonts",
    #[cfg(windows)]
    "AppData/Local/Microsoft/Windows/Fonts",
    #[cfg(target_os = "macos")]
    "/Library/Fonts",
    #[cfg(target_os = "macos")]
    "/System/Library/Fonts",
    #[cfg(not(windows))]
    "/usr/share/fonts",
    #[cfg(not(windows))]
    "/usr/local/share/fonts",
    #[cfg(not(windows))]
    "/run/host/usr/share/fonts",
];

/// The directory the built-in Windows rows are spelled under; the machine's own is [`windows_fonts_dir`].
#[cfg(windows)]
const WINDOWS_BUILTIN_FONT_ROOT: &str = "C:\\Windows\\Fonts";

/// Windows' system font directory, `%WINDIR%\Fonts` (`%SystemRoot%` where that is unset), which is `C:\Windows\Fonts` only on a machine installed to `C:\Windows`.
#[cfg(windows)]
fn windows_fonts_dir() -> std::path::PathBuf {
    windows_fonts_dir_from(std::env::var_os("WINDIR"), std::env::var_os("SystemRoot"))
}

/// [`windows_fonts_dir`] over the two variables' values: the first that is an absolute path, else the spelled root.
#[cfg(windows)]
fn windows_fonts_dir_from(
    windir: Option<std::ffi::OsString>,
    system_root: Option<std::ffi::OsString>,
) -> std::path::PathBuf {
    [windir, system_root]
        .into_iter()
        .flatten()
        .map(std::path::PathBuf::from)
        .find(|root| root.is_absolute())
        .map_or_else(
            || std::path::PathBuf::from(WINDOWS_BUILTIN_FONT_ROOT),
            |root| root.join("Fonts"),
        )
}

/// Where this machine keeps the built-in Windows row `declared` when its font directory is not the one the rows spell: the same file name under `fonts_dir`. `None` for any other path, and where the two directories agree.
#[cfg(windows)]
fn relocate_windows_builtin_font_path(
    declared: &str,
    fonts_dir: &std::path::Path,
) -> Option<String> {
    let name = declared
        .strip_prefix(WINDOWS_BUILTIN_FONT_ROOT)?
        .strip_prefix('\\')?;
    let spelled = fonts_dir
        .to_str()
        .is_some_and(|dir| dir.eq_ignore_ascii_case(WINDOWS_BUILTIN_FONT_ROOT));
    if spelled || name.contains(['\\', '/']) {
        return None;
    }
    fonts_dir.join(name).to_str().map(str::to_string)
}

/// Whether the built-in candidate `path` can name a file of this system. A rooted POSIX path names none on Windows, where it would be read off the current drive (`C:\usr\share\fonts\…`, a directory any user can create).
#[inline]
fn builtin_candidate_is_native(path: &str) -> bool {
    !cfg!(windows) || !path.starts_with('/')
}

const FONT_EXTS: &[&str] = &["ttf", "otf", "ttc", "otc"];

/// One face of a font file.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FaceRef {
    /// The font file, or a compiled-in identity ([`read_resolved_font`]).
    pub path: String,
    /// The face's collection index in that file.
    pub index: u32,
    /// The named instance of a variable face the font's name chose (`SF Mono Light
    /// Italic`); `None` where the face's role picks its instance.
    pub instance: Option<NamedInstance>,
}

impl FaceRef {
    #[must_use]
    pub fn new(path: impl Into<String>, index: u32) -> Self {
        Self {
            path: path.into(),
            index,
            instance: None,
        }
    }

    /// This face drawn at `instance`.
    #[must_use]
    pub fn with_instance(mut self, instance: Option<NamedInstance>) -> Self {
        self.instance = instance;
        self
    }
}

/// A NAMED INSTANCE OF A VARIABLE FACE THAT A FONT NAME CHOSE: `font_family_italic =
/// "SF Mono Light Italic"` names SF Mono's italic file at its Light Italic instance, not
/// the file. The face draws exactly this instance, as a static face draws itself: the
/// role's weight (`font_weight`, `font_variation`, the dark-theme nudge, a bold role's
/// class above regular) moves only a face its family or role chose. A name for the
/// instance the role draws anyway (`SF Mono Regular`, `SF Mono Italic`) chooses no
/// instance ([`named_instance_of`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NamedInstance {
    /// The `fvar` record's design-space coords, one per axis in `fvar` order, in the
    /// record's own 16.16 fixed.
    coords: Box<[i32]>,
}

impl NamedInstance {
    /// The instance at `coords` (16.16 fixed, `fvar` axis order).
    #[must_use]
    pub fn from_fixed(coords: impl Into<Box<[i32]>>) -> Self {
        Self {
            coords: coords.into(),
        }
    }

    /// The instance's design-space coords, `fvar` axis order.
    #[must_use]
    pub fn design_coords(&self) -> Vec<f32> {
        self.coords
            .iter()
            .map(|&raw| raw as f32 / 65536.0)
            .collect()
    }

    /// The instance as `(tag, value)` pairs over `axes`, or `None` where they are not
    /// this instance's axes (another face's bytes).
    fn paired(&self, axes: &[variation::VfAxis]) -> Option<Vec<f32>> {
        (axes.len() == self.coords.len()).then(|| {
            axes.iter()
                .zip(self.design_coords())
                .map(|(axis, value)| variation::clamp_axis(axis, value))
                .collect()
        })
    }
}

/// The instance `face` draws when a name names the instance `record_coords` of it: that
/// instance, or `None` where it is the instance the face's role draws without one (its
/// regular instance, [`variation::resolve_default_coords`]), so a name for it keeps
/// following the role's weight.
pub(crate) fn named_instance_of(
    face: &aterm_font::Face<'_>,
    record_coords: &[i32],
) -> Option<NamedInstance> {
    let probe = variation::probe_face(face)?;
    if probe.axes.len() != record_coords.len() {
        return None;
    }
    let automatic = variation::resolve_default_coords(&probe.axes, probe.regular_coords.as_deref());
    let chosen = NamedInstance::from_fixed(record_coords.to_vec());
    let same = chosen
        .design_coords()
        .iter()
        .zip(&automatic)
        .all(|(a, b)| (a - b).abs() < 1e-3);
    (!same).then_some(chosen)
}

/// The style a resolution looks for inside a family.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FaceTarget {
    /// The `OS/2` weight class wanted (400 regular, 700 bold).
    pub weight: u16,
    /// Whether an italic or oblique face is wanted.
    pub slanted: bool,
}

impl FaceTarget {
    pub const REGULAR: Self = Self {
        weight: 400,
        slanted: false,
    };
    pub const BOLD: Self = Self {
        weight: 700,
        slanted: false,
    };
    pub const ITALIC: Self = Self {
        weight: 400,
        slanted: true,
    };
    pub const BOLD_ITALIC: Self = Self {
        weight: 700,
        slanted: true,
    };

    /// The target of styled slot `slot`.
    #[inline]
    #[must_use]
    pub const fn for_slot(slot: usize) -> Self {
        match slot {
            0 => Self::BOLD,
            1 => Self::ITALIC,
            2 => Self::BOLD_ITALIC,
            _ => Self::REGULAR,
        }
    }

    /// How far `face` is from this target.
    #[inline]
    #[must_use]
    pub fn distance(self, face: &aterm_font::Face<'_>) -> u32 {
        let slanted = face.is_italic() || face.is_oblique();
        u32::from(face.weight().to_number().abs_diff(self.weight))
            + if slanted == self.slanted { 0 } else { 10_000 }
    }

    /// Whether `face` draws this style.
    #[inline]
    #[must_use]
    pub fn suits(self, face: &aterm_font::Face<'_>) -> bool {
        let slanted = face.is_italic() || face.is_oblique();
        let bold = face.is_bold() || face.weight().to_number() >= 600;
        slanted == self.slanted && bold == (self.weight >= 600)
    }

    /// Whether a face that draws `style` ([`DrawnStyle`]) draws this style.
    #[inline]
    #[must_use]
    pub const fn suits_style(self, style: DrawnStyle) -> bool {
        style.slanted == self.slanted && style.bold == (self.weight >= 600)
    }

    /// The style face `index` of `bytes` draws in this style's role: a variable face at
    /// the named instance its font name chose ([`FaceRef::instance`]), else at the
    /// instance the role draws (its regular, or a bold role's bold), so the weight
    /// drawn, not the face's `fvar` default's, says whether it is bold; `None` where the
    /// bytes do not parse.
    #[must_use]
    pub fn drawn_style_in(
        self,
        bytes: &[u8],
        index: u32,
        named: Option<&NamedInstance>,
    ) -> Option<DrawnStyle> {
        let face = aterm_font::Face::parse(bytes, index).ok()?;
        let weight = variation::probe_face(&face).and_then(|probe| {
            let axes = &probe.axes;
            let wght = axes
                .iter()
                .position(|axis| axis.tag == variation::WGHT_TAG)?;
            let coords = match named {
                Some(named) => named.paired(axes)?,
                None => {
                    let regular =
                        variation::resolve_default_coords(axes, probe.regular_coords.as_deref());
                    if self.weight >= 600 {
                        variation::bold_role_coords(&probe, &regular, &[])
                    } else {
                        regular
                    }
                }
            };
            coords.get(wght).copied()
        });
        Some(DrawnStyle {
            bold: weight.map_or_else(
                || face.is_bold() || face.weight().to_number() >= 600,
                |weight| weight >= 600.0,
            ),
            slanted: face.is_italic() || face.is_oblique(),
        })
    }

    /// [`Self::suits`] for face `index` of `bytes` as it draws in this style's role
    /// ([`Self::drawn_style_in`]); a face that does not parse is not judged.
    #[must_use]
    pub fn suits_face_in(self, bytes: &[u8], index: u32, named: Option<&NamedInstance>) -> bool {
        self.drawn_style_in(bytes, index, named)
            .is_none_or(|style| self.suits_style(style))
    }
}

/// The style a face draws in a role ([`FaceTarget::drawn_style_in`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrawnStyle {
    /// It draws at a bold weight (600 or more).
    pub bold: bool,
    /// It is italic or oblique.
    pub slanted: bool,
}

/// Resolve a font family name (e.g. `"JetBrains Mono"`) to the face that draws `target` by scanning the standard font directories.
#[must_use]
pub fn resolve_font_family(family: &str, target: FaceTarget) -> Option<FaceRef> {
    let trimmed = family.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.contains(['/', '\\']) {
        let path = std::path::Path::new(trimmed);
        return font_file::validate_font_file(path)
            .is_ok()
            .then(|| FaceRef::new(trimmed, face_in_file(path, target)));
    }
    let want = normalize_family(trimmed);
    if want.is_empty() {
        return None;
    }
    let mut prefix_hit: Option<std::path::PathBuf> = None;
    for path in font_files() {
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let norm = normalize_family(stem);
        if norm == want {
            let index = face_in_file(&path, target);
            return path.to_str().map(|p| FaceRef::new(p, index));
        }
        if prefix_hit.is_none() && norm.starts_with(&want) {
            prefix_hit = Some(path);
        }
    }
    if let Some(face) = resolve_by_name_table(&want, target) {
        return Some(face);
    }
    let path = prefix_hit?;
    let index = face_in_file(&path, target);
    path.to_str().map(|p| FaceRef::new(p, index))
}

/// Resolve a family request that may name a bundled face ([`bundled`]) to a face of an installed file or a bundled virtual identity (face 0).
#[must_use]
pub fn resolve_family_or_bundled(family: &str, target: FaceTarget) -> Option<FaceRef> {
    let trimmed = family.trim();
    if is_generic_monospace_family(trimmed) {
        return default_primary_identity();
    }
    let bundled = bundled::face_for_family(trimmed);
    if let Some((id, _)) = bundled
        && bundled::is_identity(trimmed)
    {
        return Some(FaceRef::new(id, 0));
    }
    resolve_font_family(trimmed, target).or_else(|| bundled.map(|(id, _)| FaceRef::new(id, 0)))
}

/// The bytes behind a resolved font identity.
pub fn read_resolved_font(resolved: &str) -> std::io::Result<std::borrow::Cow<'static, [u8]>> {
    if let Some(bytes) = bundled::face_for_id(resolved) {
        return Ok(std::borrow::Cow::Borrowed(bytes));
    }
    font_file::read_font_file(std::path::Path::new(resolved)).map(std::borrow::Cow::Owned)
}

/// Why a `display:` family is no face.
#[must_use]
pub fn display_family_refusal(request: &str) -> Option<String> {
    display_face_ids_for_family(request).map(|_| {
        format!(
            "{:?} is a display face, not a font family; display faces are set with display_font",
            request.trim()
        )
    })
}

/// Resolve one config-authored family-or-path to the face that draws `target` and validate its file under the explicit-font admission policy.
pub fn resolve_config_font(family: &str, target: FaceTarget) -> Result<FaceRef, String> {
    let trimmed = family.trim();
    if trimmed.is_empty() {
        return Err("empty font name or path".to_string());
    }
    if let Some(refusal) = display_family_refusal(trimmed) {
        return Err(refusal);
    }
    if trimmed.contains(['/', '\\']) {
        let path = std::path::Path::new(trimmed);
        font_file::validate_font_file(path)
            .map_err(|error| format!("font path {trimmed:?} is not admissible ({error})"))?;
        return Ok(FaceRef::new(trimmed, face_in_file(path, target)));
    }
    let face = resolve_family_or_bundled(trimmed, target)
        .ok_or_else(|| format!("{trimmed:?} does not resolve to a font file"))?;
    if bundled::is_bundled_id(&face.path) {
        return Ok(face);
    }
    font_file::validate_font_file(std::path::Path::new(&face.path))
        .map_err(|error| format!("resolved font {:?} is not admissible ({error})", face.path))?;
    Ok(face)
}

const NAME_SCAN_MAX_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
enum NameMatch {
    /// The request names this face, at this named instance where it names one of a
    /// variable face ([`NamedInstance`]).
    Face(Option<NamedInstance>),
    /// The request names a role this variable face serves (`SF Mono Bold Italic`),
    /// bold or not ([`font_catalog::variable_roles`]).
    Role { bold: bool },
    /// The request names the face's family (`Menlo`).
    Family,
}

/// What a system face that hides its name is called.
#[must_use]
pub fn public_face_name(name: &str) -> std::borrow::Cow<'_, str> {
    match name.strip_prefix(".SF NS ") {
        Some(rest) => std::borrow::Cow::Owned(format!("SF {rest}")),
        None => std::borrow::Cow::Borrowed(name),
    }
}

/// Whether `name`, after [`public_face_name`], is still a hidden system name (a leading dot).
#[must_use]
pub fn is_hidden_face_name(name: &str) -> bool {
    public_face_name(name).starts_with('.')
}

fn face_name_match(face: &aterm_font::Face<'_>, want: &str) -> Option<NameMatch> {
    use aterm_font::name_id::{
        FAMILY, FULL_NAME, POST_SCRIPT_NAME, SUBFAMILY, TYPOGRAPHIC_FAMILY, TYPOGRAPHIC_SUBFAMILY,
    };
    let mut records: Vec<(u16, String)> = Vec::new();
    for name in face.names() {
        if matches!(
            name.name_id,
            FAMILY
                | SUBFAMILY
                | FULL_NAME
                | POST_SCRIPT_NAME
                | TYPOGRAPHIC_FAMILY
                | TYPOGRAPHIC_SUBFAMILY
        ) && let Some(text) = name_record_text(&name)
        {
            for spelling in [text.trim(), &public_face_name(text.trim())] {
                let normalized = normalize_family(spelling);
                if !records.contains(&(name.name_id, normalized.clone())) {
                    records.push((name.name_id, normalized));
                }
            }
        }
    }
    let named = |id: u16| {
        records
            .iter()
            .filter(move |(i, _)| *i == id)
            .map(|(_, t)| t)
    };
    let family_then = |rest: &str| {
        named(FAMILY)
            .chain(named(TYPOGRAPHIC_FAMILY))
            .any(|family| want.strip_prefix(family.as_str()) == Some(rest))
    };
    // A variable face answers to its family and each role it serves (`SF Mono
    // Italic`, `SF Mono Bold`), the names a picker lists it by
    // ([`font_catalog::variable_roles`]): the role picks its instance.
    if let Some((_, slot)) = font_catalog::variable_roles(face)
        .into_iter()
        .find(|(words, _)| family_then(&normalize_family(words)))
    {
        return Some(NameMatch::Role {
            bold: matches!(slot, Some(0 | 2)),
        });
    }
    // A variable face answers to each named instance's name, its family and the
    // instance's subfamily (`SF Mono Light Italic`) or the instance's PostScript name,
    // and then draws that instance.
    for record in variation::instance_records(face) {
        let subfamily = name_spellings(face, record.subfamily_name_id);
        let postscript = record
            .postscript_name_id
            .map(|id| name_spellings(face, id))
            .unwrap_or_default();
        if subfamily
            .iter()
            .any(|sub| !sub.is_empty() && family_then(sub))
            || postscript.iter().any(|ps| ps == want)
        {
            return Some(NameMatch::Face(named_instance_of(face, &record.coords)));
        }
    }
    let face_named = named(FULL_NAME)
        .chain(named(POST_SCRIPT_NAME))
        .any(|t| t == want)
        || [
            (FAMILY, SUBFAMILY),
            (TYPOGRAPHIC_FAMILY, TYPOGRAPHIC_SUBFAMILY),
        ]
        .into_iter()
        .any(|(family, subfamily)| {
            named(family).any(|f| {
                want.strip_prefix(f.as_str())
                    .is_some_and(|rest| !rest.is_empty() && named(subfamily).any(|s| s == rest))
            })
        });
    if face_named {
        // The name table names a variable face's `fvar` default instance.
        let default = face
            .variation_axes()
            .into_iter()
            .map(|axis| (axis.def_value * 65536.0).round() as i32)
            .collect::<Vec<i32>>();
        return Some(NameMatch::Face(
            (!default.is_empty())
                .then(|| named_instance_of(face, &default))
                .flatten(),
        ));
    }
    named(FAMILY)
        .chain(named(TYPOGRAPHIC_FAMILY))
        .any(|t| t == want)
        .then_some(NameMatch::Family)
}

/// Every spelling of `name` record `name_id`, normalized ([`normalize_family`]), with a
/// hidden system name's public one ([`public_face_name`]).
fn name_spellings(face: &aterm_font::Face<'_>, name_id: u16) -> Vec<String> {
    let mut spellings = Vec::new();
    for name in face.names() {
        if name.name_id != name_id {
            continue;
        }
        let Some(text) = name_record_text(&name) else {
            continue;
        };
        for spelling in [text.trim(), &public_face_name(text.trim())] {
            let normalized = normalize_family(spelling);
            if !spellings.contains(&normalized) {
                spellings.push(normalized);
            }
        }
    }
    spellings
}

/// The face of `bytes` that best answers a request, as `(distance, index)`.
pub(crate) fn best_face(
    bytes: &[u8],
    want: Option<&str>,
    target: FaceTarget,
) -> Option<(u32, u32)> {
    best_named_face(bytes, want, target).map(|(distance, index, _)| (distance, index))
}

/// The instance a role name (`SF Mono Bold Italic`, [`NameMatch::Role`]) chooses of `face`
/// asked for `target`'s style: none where the role is that style, so the face follows
/// the role's weight there; else the role's own instance, so a role named in another
/// style's row draws what it names and that row says it is another style
/// ([`FaceTarget::suits_face_in`]), as `Menlo Bold Italic` in the Italic row does.
fn role_named_elsewhere(
    face: &aterm_font::Face<'_>,
    bold: bool,
    target: FaceTarget,
) -> Option<NamedInstance> {
    let slanted = face.is_italic() || face.is_oblique();
    if bold == (target.weight >= 600) && slanted == target.slanted {
        return None;
    }
    let probe = variation::probe_face(face)?;
    let regular = variation::resolve_default_coords(&probe.axes, probe.regular_coords.as_deref());
    let coords = if bold {
        variation::bold_role_coords(&probe, &regular, &[])
    } else {
        regular
    };
    Some(NamedInstance::from_fixed(
        coords
            .iter()
            .map(|&value| (value * 65536.0).round() as i32)
            .collect::<Vec<i32>>(),
    ))
}

/// [`best_face`] with the named instance the request names of it ([`NamedInstance`]).
pub(crate) fn best_named_face(
    bytes: &[u8],
    want: Option<&str>,
    target: FaceTarget,
) -> Option<(u32, u32, Option<NamedInstance>)> {
    let faces = aterm_font::fonts_in_collection(bytes).unwrap_or(1).max(1);
    let mut best: Option<(u32, u32)> = None;
    for index in 0..faces {
        let Ok(face) = aterm_font::Face::parse(bytes, index) else {
            continue;
        };
        let distance = match want.map(|want| face_name_match(&face, want)) {
            Some(None) => continue,
            Some(Some(NameMatch::Face(instance))) => return Some((0, index, instance)),
            Some(Some(NameMatch::Role { bold })) => {
                return Some((0, index, role_named_elsewhere(&face, bold, target)));
            }
            Some(Some(NameMatch::Family)) | None => target.distance(&face),
        };
        if best.is_none_or(|(held, _)| distance < held) {
            best = Some((distance, index));
        }
    }
    best.map(|(distance, index)| (distance, index, None))
}

fn face_in_file(path: &std::path::Path, target: FaceTarget) -> u32 {
    if !font_file::is_ttc_font_file(path) {
        return 0;
    }
    font_file::read_font_file(path)
        .ok()
        .and_then(|bytes| best_face(&bytes, None, target))
        .map_or(0, |(_, index)| index)
}

/// A `name` record's text.
pub(crate) fn name_record_text(name: &aterm_font::name::Name<'_>) -> Option<String> {
    name.to_string().or_else(|| {
        (name.platform_id == aterm_font::PlatformId::Macintosh
            && name.encoding_id == 0
            && name.name.is_ascii())
        .then(|| String::from_utf8_lossy(name.name).into_owned())
    })
}

#[must_use]
fn resolve_by_name_table(want: &str, target: FaceTarget) -> Option<FaceRef> {
    resolve_by_name_table_in(font_files(), want, target)
}

fn resolve_by_name_table_in(
    files: impl IntoIterator<Item = std::path::PathBuf>,
    want: &str,
    target: FaceTarget,
) -> Option<FaceRef> {
    /// The best match yet: its rank, file, face and the instance it names.
    type Held = ((u32, bool), std::path::PathBuf, u32, Option<NamedInstance>);
    let mut best: Option<Held> = None;
    for path in files {
        if best
            .as_ref()
            .is_some_and(|(_, best, _, _)| best.parent() != path.parent())
        {
            continue;
        }
        let Ok(bytes) = font_file::read_bounded_font_file(&path, NAME_SCAN_MAX_BYTES as usize)
        else {
            continue;
        };
        if let Some((distance, index, instance)) = best_named_face(&bytes, Some(want), target) {
            let rank = name_match_rank(distance, &path);
            if best.as_ref().is_none_or(|(held, _, _, _)| rank < *held) {
                best = Some((rank, path, index, instance));
            }
            if rank == NAME_MATCH_SETTLED {
                break;
            }
        }
    }
    best.and_then(|(_, path, index, instance)| {
        path.to_str()
            .map(|p| FaceRef::new(p, index).with_instance(instance))
    })
}

/// How a file whose `name` table matched a request ranks against the other matches in its directory, best (lowest) first.
pub(crate) fn name_match_rank(distance: u32, path: &std::path::Path) -> (u32, bool) {
    (distance, is_sibling_alias(path))
}

fn is_sibling_alias(path: &std::path::Path) -> bool {
    use std::path::Component;
    std::fs::read_link(path).is_ok_and(|target| {
        let mut parts = target.components().filter(|c| *c != Component::CurDir);
        matches!(parts.next(), Some(Component::Normal(_))) && parts.next().is_none()
    })
}

/// The [`name_match_rank`] no other match can beat.
pub(crate) const NAME_MATCH_SETTLED: (u32, bool) = (0, false);

/// The font directories to scan, with the per-user entries joined to `$HOME`.
#[must_use]
pub fn font_search_dirs() -> Vec<std::path::PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from);
    let (user_xdg, system_xdg) = xdg_font_dirs();
    font_search_dirs_in(home.as_deref(), user_xdg, system_xdg)
}

fn xdg_font_dirs() -> (Option<std::path::PathBuf>, Vec<std::path::PathBuf>) {
    #[cfg(target_os = "linux")]
    {
        let user = aterm_types::dirs::data_dir().map(|dir| dir.join("fonts"));
        let data_dirs = std::env::var_os("XDG_DATA_DIRS")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        let system = std::env::split_paths(&data_dirs)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join("fonts"))
            .collect();
        (user, system)
    }
    #[cfg(not(target_os = "linux"))]
    {
        (None, Vec::new())
    }
}

fn font_search_dirs_in(
    home: Option<&std::path::Path>,
    user_xdg: Option<std::path::PathBuf>,
    system_xdg: Vec<std::path::PathBuf>,
) -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    let mut push = |dir: std::path::PathBuf| {
        if !dirs
            .iter()
            .any(|held| dir.starts_with(held) || held.starts_with(&dir))
        {
            dirs.push(dir);
        }
    };
    let mut user_xdg = user_xdg;
    for entry in FONT_DIRS {
        let path = std::path::Path::new(entry);
        if path.is_absolute() {
            if let Some(dir) = user_xdg.take() {
                push(dir);
            }
            push(path.to_path_buf());
        } else if let Some(home) = home {
            push(home.join(entry));
        }
    }
    if let Some(dir) = user_xdg {
        push(dir);
    }
    #[cfg(windows)]
    push(windows_fonts_dir());
    for dir in system_xdg {
        push(dir);
    }
    dirs
}

fn user_font_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .map(|home| {
            [".local/share/fonts", ".fonts"]
                .iter()
                .map(|dir| home.join(dir))
                .collect()
        })
        .unwrap_or_default();
    if let (Some(xdg), _) = xdg_font_dirs()
        && !dirs.contains(&xdg)
    {
        dirs.push(xdg);
    }
    dirs
}

const FONT_SCAN_MAX_DEPTH: usize = 8;

fn font_files() -> Vec<std::path::PathBuf> {
    font_catalog::system_font_files()
}

pub(crate) fn normalize_family(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != '-' && *c != '_')
        .flat_map(char::to_lowercase)
        .collect()
}

/// Basic metrics for a resolved font face.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaceInfo {
    /// Absolute path to the resolved font file.
    pub path: String,
    /// The face's collection index in that file ([`FaceRef::index`]).
    pub index: u32,
    /// Advance width of `'M'` in px at [`Self::PROBE_PX`] (a cell width for a monospace face; just one glyph's advance for a proportional one).
    pub cell_width: usize,
    /// Cell height in px at [`Self::PROBE_PX`], matching [`Renderer::cell_size`].
    pub cell_height: usize,
    /// Baseline offset in px at [`Self::PROBE_PX`], matching [`Renderer::baseline`].
    pub baseline: i32,
    /// Number of glyphs in the face (from the font's glyph table).
    pub glyph_count: usize,
}

impl FaceInfo {
    /// The fixed probe size the reported metrics are measured at.
    pub const PROBE_PX: f32 = 16.0;
}

/// The available font files as family stems (`"Menlo"`, `"SFNSMono"`) from [`FONT_DIRS`], de-duplicated and sorted.
#[must_use]
pub fn list_fonts() -> Vec<String> {
    let mut families: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for path in font_files() {
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            families.insert(stem.to_string());
        }
    }
    let installed: Vec<String> = families.iter().map(|stem| normalize_family(stem)).collect();
    for family in bundled::selectable_families() {
        let want = normalize_family(family);
        if !installed.iter().any(|stem| stem.starts_with(&want)) {
            families.insert((*family).to_string());
        }
    }
    families.into_iter().collect()
}

#[inline]
fn cell_w_from_advance(advance: f32) -> usize {
    (advance.round() as usize).max(1)
}

#[inline]
fn checked_cell_w_from_advance(advance: f32) -> Option<usize> {
    if !advance.is_finite() {
        return None;
    }
    let width = cell_w_from_advance(advance);
    (width <= i32::MAX as usize).then_some(width)
}

/// Cell height + baseline from a face's vertical metrics, applying a line-height `scale` that grows/shrinks the cell box without changing the glyph rasterization px.
#[inline]
#[must_use]
pub fn cell_h_baseline(ascent: f32, descent: f32, line_gap: f32, scale: f32) -> (usize, i32) {
    let content = ascent - descent;
    let natural = (content + line_gap).max(1.0);
    let cell_h = (natural * scale).round().max(1.0);
    let leading = cell_h - content;
    let baseline = (ascent + leading / 2.0).round() as i32;
    (cell_h as usize, baseline)
}

#[inline]
fn checked_cell_h_baseline(
    ascent: f32,
    descent: f32,
    gap: f32,
    scale: f32,
    baseline_adjust: i32,
) -> Option<(usize, i32)> {
    if ![ascent, descent, gap, scale]
        .into_iter()
        .all(f32::is_finite)
        || scale <= 0.0
    {
        return None;
    }
    let (cell_h, baseline) = cell_h_baseline(ascent, descent, gap, scale);
    if cell_h > i32::MAX as usize || matches!(baseline, i32::MIN | i32::MAX) {
        return None;
    }
    Some((cell_h, baseline.checked_add(baseline_adjust)?))
}

#[inline]
fn font_px_unchanged(current: f32, requested: f32) -> bool {
    (requested - current).abs() < 0.01
}

#[inline]
fn requested_line_height(current: f32, requested: f32) -> Option<f32> {
    if !requested.is_finite() {
        return None;
    }
    let scale = requested.max(0.5);
    if (scale - current).abs() < 0.001 {
        None
    } else {
        Some(scale)
    }
}

#[inline]
fn checked_cell_geometry(
    metrics: &variation::VariedMetricsPx,
    line_height: f32,
    baseline_adjust: i32,
) -> Option<CellGeometry> {
    let cell_w = checked_cell_w_from_advance(metrics.m_advance)?;
    let (cell_h, baseline) = checked_cell_h_baseline(
        metrics.ascent,
        metrics.descent,
        metrics.line_gap,
        line_height,
        baseline_adjust,
    )?;
    Some((cell_w, cell_h, baseline))
}

const LETTER_HINT_SLACK_PX: f32 = 1.0;

#[inline]
fn letter_floored_descent(descent: f32, letter: f32, px: f32) -> f32 {
    let deepest = letter * px;
    if deepest < descent {
        deepest - LETTER_HINT_SLACK_PX
    } else {
        descent
    }
}

#[derive(Clone, Copy, Debug)]
struct TypoLineMetrics {
    ascent: f32,
    descent: f32,
    line_gap: f32,
}

fn typo_line_metrics(bytes: &[u8], index: u32) -> Option<TypoLineMetrics> {
    let face = aterm_font::Face::parse(bytes, index).ok()?;
    let os2 = face.tables().os2?;
    if !os2.use_typographic_metrics() {
        return None;
    }
    let upem = f32::from(face.units_per_em());
    if upem <= 0.0 {
        return None;
    }
    Some(TypoLineMetrics {
        ascent: f32::from(os2.typographic_ascender()) / upem,
        descent: f32::from(os2.typographic_descender()) / upem,
        line_gap: f32::from(os2.typographic_line_gap()) / upem,
    })
}

fn deco_tables(bytes: &[u8], index: u32) -> Option<deco::DecoTables> {
    let face = aterm_font::Face::parse(bytes, index).ok()?;
    let upem = f32::from(face.units_per_em());
    if upem <= 0.0 {
        return None;
    }
    let frac = |m: aterm_font::LineMetrics| {
        (m.thickness > 0).then_some((f32::from(m.position) / upem, f32::from(m.thickness) / upem))
    };
    Some(deco::DecoTables {
        underline: face.underline_metrics().and_then(frac),
        strikeout: face.strikeout_metrics().and_then(frac),
    })
}

/// Resolve a font `family` to its [`FaceInfo`] (path, cell metrics at [`FaceInfo::PROBE_PX`], glyph count), or `None` if it does not resolve or parse.
#[must_use]
pub fn face_info(family: &str) -> Option<FaceInfo> {
    let FaceRef {
        path,
        index,
        instance,
    } = resolve_family_or_bundled(family, FaceTarget::REGULAR)?;
    let bytes = read_resolved_font(&path).ok()?;
    let renderer = Renderer::from_named_face(
        &bytes,
        index,
        instance,
        FaceInfo::PROBE_PX,
        Theme::default(),
    )
    .ok()?;
    let (cell_width, cell_height) = renderer.cell_size();
    let glyph_count = aterm_font::Face::parse(&bytes, index)
        .map(|f| f.number_of_glyphs() as usize)
        .unwrap_or(0);
    Some(FaceInfo {
        path,
        index,
        cell_width,
        cell_height,
        baseline: renderer.baseline(),
        glyph_count,
    })
}

#[cfg(test)]
#[path = "lib_font_enum_tests.rs"]
mod font_enum_tests;

impl Renderer {
    /// Build from explicit single-face font bytes (face 0) at a finite, positive pixel size.
    pub fn from_bytes(bytes: &[u8], px: f32, theme: Theme) -> Result<Self, String> {
        Self::from_face(bytes, 0, px, theme)
    }

    /// Build from face `index` of explicit font bytes (a collection's face, [`FaceRef::index`]) at a finite, positive pixel size.
    pub fn from_face(bytes: &[u8], index: u32, px: f32, theme: Theme) -> Result<Self, String> {
        Self::from_named_face(bytes, index, None, px, theme)
    }

    /// [`Self::from_face`] drawing the primary at `named` ([`FaceRef::instance`]).
    fn from_named_face(
        bytes: &[u8],
        index: u32,
        named: Option<NamedInstance>,
        px: f32,
        theme: Theme,
    ) -> Result<Self, String> {
        if !px.is_finite() || px <= 0.0 {
            return Err("font pixel size must be finite and positive".into());
        }
        let (primary_bytes, font) = shared_parsed_face(bytes, index)?;
        let lm = font
            .horizontal_line_metrics(px)
            .ok_or("font has no horizontal line metrics")?;
        let adv = font.metrics('M', px).advance_width;
        let cell_w =
            checked_cell_w_from_advance(adv).ok_or("font cell width is not representable")?;
        let typo_lm = typo_line_metrics(bytes, index);
        let (asc, desc, gap) = typo_lm.map_or((lm.ascent, lm.descent, lm.line_gap), |t| {
            (t.ascent * px, t.descent * px, t.line_gap * px)
        });
        let letter_descent = bundled::letter_descent_floor(bytes);
        let desc = letter_descent.map_or(desc, |letter| letter_floored_descent(desc, letter, px));
        let (cell_h, baseline) = checked_cell_h_baseline(asc, desc, gap, 1.0, 0)
            .ok_or("font cell height or baseline is not representable")?;
        let mut r = Renderer {
            font,
            shaping_primary_bytes: Some(primary_bytes),
            primary_index: index,
            primary_named: named,
            var_coords: None,
            geom_coords: None,
            vf_bold_coords: None,
            cfg_font_variations: Vec::new(),
            shaping_variations: Vec::new(),
            font_weight_dark_nudge: 0.0,
            bold_font: None,
            bold_font_bytes: None,
            bold_font_index: 0,
            bold_font_named: None,
            bold_font_instance: FaceInstance::default(),
            bold_gid_cache: FxHashMap::default(),
            bold_yields_private_use: false,
            bold_lowered_asterisk: None,
            has_ligature_features: ligature_shaping::font_has_ligature_features(bytes, index),
            has_user_features: false,
            shaping: aterm_types::text_shaping::TextShapingConfig::default(),
            resolved_features: ligature_shaping::build_feature_list(&[], true),
            engine_features: ligature_shaping::engine_features(
                &ligature_shaping::build_feature_list(&[], true),
            ),
            shaped_runs: ShapedRunCache::default(),
            fallback_chain: Vec::new(),
            display_layer: Vec::new(),
            fallback_pick: FxHashMap::default(),
            fallback_paths: Vec::new(),
            fallback_user_prefix: 0,
            cfg_fallback_fonts: Vec::new(),
            cfg_symbol_font: None,
            cfg_emoji_font: None,
            symbol_chain: Vec::new(),
            symbol_fallback_paths: Vec::new(),
            symbol_user_prefix: 0,
            fallback_parse_rx: None,
            symbol_parse_rx: None,
            missing_font_classes: 0,
            runtime_discovery: true,
            styled_faces: [None, None, None],
            synthetic_styles: true,
            #[cfg(any(all(unix, not(target_os = "macos")), windows))]
            hint_mode: hinted::HintMode::initial(),
            #[cfg(any(all(unix, not(target_os = "macos")), windows))]
            hint_bank: hinted::HintBank::default(),
            #[cfg(all(unix, not(target_os = "macos")))]
            subpix_mode: subpixel::SubpixelMode::initial(),
            #[cfg(all(unix, not(target_os = "macos")))]
            subpx_bank: hinted::HintBank::default(),
            #[cfg(all(unix, not(target_os = "macos")))]
            subpx_glyphs: FxHashMap::default(),
            #[cfg(all(unix, not(target_os = "macos")))]
            subpx_frame: false,
            #[cfg(target_os = "macos")]
            rasterizer: select_rasterizer(),
            #[cfg(target_os = "macos")]
            ct_cache: FxHashMap::default(),
            #[cfg(target_os = "macos")]
            ct_descriptors: FxHashMap::default(),
            primary_path: None,
            styled_loaded: false,
            styled_retire_pending: [None; 3],
            admitted_sources_sealed: false,
            drawn_faces: None,
            shapeable_scratch: Vec::new(),
            break_mask_scratch: Vec::new(),
            run_boundary_scratch: Vec::new(),
            glyph_plan_scratch: Vec::new(),
            glyph_plan_row: None,
            halo_plan_scratch: Vec::new(),
            fg_clip: (i32::MIN, i32::MAX),
            fg_ink_top: i32::MAX,
            overshoot_trust: OvershootTrust::AsciiInCell,
            overshoot_surprise: false,
            bg_runs_total: std::cell::Cell::new(0),
            bg_runs_at_base: std::cell::Cell::new(0),
            bg_fills_emitted: std::cell::Cell::new(0),
            shape_run_scratch: String::new(),
            shape_chars_scratch: Vec::new(),
            shaping_buffer: aterm_font::shape::UnicodeBuffer::new(),
            deco_scratch: Vec::new(),
            deco_skip_rect_scratch: Vec::new(),
            ink_scratch: Vec::new(),
            keep_spans_scratch: Vec::new(),
            curl_scratch: Vec::new(),
            ligature_break_scratch: Vec::new(),
            // Resident from the start: a first body under the text knocks
            // its letters out without allocating (`free_sprite_alloc`).
            knock_cut: KnockCut::default(),
            #[cfg(any(test, debug_assertions))]
            knock_off: false,
            color_chain: Vec::new(),
            color_font_paths: Vec::new(),
            runtime_fallback: RuntimeFallback::default(),
            px,
            px_q: GlyphKey::quantize_px(px),
            cell_w,
            cell_h,
            line_height_scale: 1.0,
            typo_lm,
            letter_descent,
            primary_yields_private_use: bundled::yields_private_use(bytes),
            primary_fits_two_cell_glyphs: bundled::fits_two_cell_glyphs(bytes),
            primary_yields_wide_characters: bundled::yields_wide_characters(bytes),
            primary_yields_small_dingbats: bundled::yields_small_dingbats(bytes),
            primary_lowered_asterisk: bundled::lowered_asterisk(bytes),
            baseline_adjust: 0,
            cell_geometry_memo: std::cell::Cell::default(),
            pad: 0,
            head: 0,
            pad_top: None,
            chrome_bleed: None,
            chrome_room_memo: None,
            chrome_room: (0, 0),
            glyph_ink_memo: None,
            glyph_frame_ink_memo: None,
            glyph_ink_identity: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            glyph_generation: 0,
            baseline,
            theme,
            stem_lut: build_stem_lut(DEFAULT_STEM_GAMMA),
            stem_gamma: DEFAULT_STEM_GAMMA,
            text_blending: TextBlending::default(),
            font_thicken: false,
            selection_fg: None,
            selection_inactive: false,
            selection_inactive_bg: None,
            min_contrast: 0.0,
            bg_opacity: 1.0,
            cursor_opacity: 1.0,
            glyphs: FxHashMap::default(),
            glyph_bytes: 0,
            glyph_frame: 0,
            glyph_kept: (0, 0),
            primary_gid_cache: FxHashMap::default(),
            keys: FxHashMap::default(),
            emoji_keys: FxHashMap::default(),
            cluster_shapes: emoji_cluster::ClusterShapes::default(),
            styled_keys: FxHashMap::default(),
            font_epoch: 0,
            land_parses_on_poll: false,
            cursor_blink_phase: true,
            cursor_style_override: None,
            deco_masks: std::cell::RefCell::new(Vec::new()),
            deco_tables: deco_tables(bytes, index),
            underline_adjust_pos: 0,
            underline_adjust_thick: 0,
            underline_skip_descenders: true,
            undercurl_masks: std::cell::RefCell::new(Vec::new()),
            fx_scratch: fx_raster::FxScratch::default(),
            #[cfg(any(test, debug_assertions))]
            legacy_sprites: None,
            alpha_memo: None,
        };
        r.refresh_variations()
            .ok_or("resolved font instance has no usable cell geometry")?;
        Ok(r)
    }

    /// Install the display layer (`display_font`).
    pub fn set_display_faces(&mut self, ids: &[&str]) -> Result<(), String> {
        let mut layer = Vec::new();
        for id in display_layer_ids(ids.iter().copied()) {
            let bytes = display_face_bytes(id).ok_or_else(|| format!("no display face {id:?}"))?;
            let fit = display_face_fit(bytes).ok_or_else(|| format!("display face {id:?}"))?;
            layer.push(DisplayLayerFace {
                id,
                face: FallbackFace::from_bytes(bytes, None)?,
                fit,
            });
        }
        if layer
            .iter()
            .map(|face| face.id)
            .eq(self.display_layer.iter().map(|face| face.id))
        {
            return Ok(());
        }
        self.display_layer = layer;
        self.keys.clear();
        self.styled_keys.clear();
        self.emoji_keys.clear();
        self.shaped_runs.clear();
        self.clear_glyph_images();
        Ok(())
    }

    /// The display layer's faces, as registry ids in the order they are picked from ([`Self::set_display_faces`]).
    #[must_use]
    pub fn display_faces(&self) -> Vec<&'static str> {
        self.display_layer.iter().map(|face| face.id).collect()
    }

    /// Reset the broad-coverage fallback chain to a single face from explicit bytes (eagerly).
    pub fn set_fallback_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        let face = FallbackFace::from_bytes(bytes, None)?;
        self.fallback_chain.clear();
        self.fallback_chain.push(face);
        self.fallback_pick.clear();
        self.keys.clear();
        self.clear_glyph_images();
        self.clear_fallback_candidates();
        Ok(())
    }

    /// Append a broad fallback face to the chain (eagerly).
    pub fn add_fallback_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        let face = FallbackFace::from_bytes(bytes, None)?;
        self.fallback_chain.push(face);
        self.fallback_pick.clear();
        self.keys.clear();
        self.clear_fallback_candidates();
        Ok(())
    }

    /// Install a monochrome symbol fallback face from explicit bytes (eagerly), consulted only after the primary and broad fallback miss.
    pub fn set_symbol_fallback_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.symbol_chain.clear();
        self.symbol_chain
            .push(FallbackFace::from_bytes(bytes, None)?);
        self.symbol_fallback_paths.clear();
        self.symbol_user_prefix = 0;
        self.keys.clear();
        self.clear_glyph_images();
        Ok(())
    }

    /// Install a colour-emoji face from explicit bytes (eagerly) at the head of the colour chain ([`Self::color_pick`]), replacing an earlier injected face.
    pub fn set_color_font_bytes(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        aterm_font::Face::parse(&bytes, 0).map_err(|e| e.to_string())?;
        self.install_color_font(crate::font::FaceBytes::Vec(intern_font_bytes(bytes)));
        Ok(())
    }

    /// Arc twin of [`Self::set_color_font_bytes`] for already-interned blobs (the wasm font registry).
    pub fn set_color_font_arc(&mut self, bytes: std::sync::Arc<Vec<u8>>) -> Result<(), String> {
        aterm_font::Face::parse(&bytes, 0).map_err(|e| e.to_string())?;
        self.install_color_font(crate::font::FaceBytes::Vec(bytes));
        Ok(())
    }

    fn install_color_font(&mut self, bytes: crate::font::FaceBytes) {
        self.color_chain
            .retain(|face| !face.injected && !face.bytes.same_bytes(&bytes));
        self.color_chain.insert(
            0,
            ColorFace {
                bytes,
                injected: true,
            },
        );
        self.color_chain.truncate(MAX_COLOR_FACES);
        self.forget_color_routing();
    }

    fn forget_color_routing(&mut self) {
        self.clear_glyph_images();
        self.emoji_keys.clear();
        self.cluster_shapes.clear();
        self.keys.clear();
        self.styled_keys.clear();
    }

    /// Install a real bold weight of the primary family from explicit bytes, so SGR-bold cells draw a true heavier weight ([`FaceId::BoldPrimary`]) ahead of the `-Bold` sibling or synthetic [`embolden`] (the wasm renderer cannot read the host filesystem, so the host pushes the bytes).
    pub fn set_bold_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.set_bold_face(bytes, 0)
    }

    /// [`Self::set_bold_font`] for face `index` of `bytes`.
    pub fn set_bold_face(&mut self, bytes: &[u8], index: u32) -> Result<(), String> {
        self.set_named_bold_face(bytes, index, None)
    }

    /// [`Self::set_bold_face`] drawn at `named` ([`FaceRef::instance`]).
    pub fn set_named_bold_face(
        &mut self,
        bytes: &[u8],
        index: u32,
        named: Option<NamedInstance>,
    ) -> Result<(), String> {
        let owned: std::sync::Arc<[u8]> = std::sync::Arc::from(bytes);
        let font = crate::font::Font::from_shared_slice(
            owned.clone(),
            crate::font::FontSettings {
                collection_index: index,
            },
        )
        .map_err(|e| e.to_string())?;
        self.bold_font = Some(font);
        self.bold_yields_private_use = bundled::yields_private_use(&owned);
        self.bold_lowered_asterisk = bundled::lowered_asterisk(&owned);
        self.bold_font_bytes = Some(owned);
        self.bold_font_index = index;
        self.bold_font_named = named;
        self.refresh_family_instances();
        self.refresh_drawn_faces();
        self.bold_gid_cache.clear();
        self.styled_keys.clear();
        self.clear_glyph_images();
        self.shaped_runs.clear();
        self.clear_face_address_caches();
        Ok(())
    }

    /// Install a real styled face of the primary family from explicit bytes.
    pub fn set_styled_font_bytes(&mut self, slot: usize, bytes: &[u8]) -> Result<(), String> {
        self.set_styled_face(slot, bytes, 0)
    }

    /// [`Self::set_styled_font_bytes`] for face `index` of `bytes`.
    pub fn set_styled_face(&mut self, slot: usize, bytes: &[u8], index: u32) -> Result<(), String> {
        self.set_named_styled_face(slot, bytes, index, None)
    }

    /// [`Self::set_styled_face`] drawn at `named` ([`FaceRef::instance`]).
    pub fn set_named_styled_face(
        &mut self,
        slot: usize,
        bytes: &[u8],
        index: u32,
        named: Option<NamedInstance>,
    ) -> Result<(), String> {
        if slot >= 3 {
            return Err(format!(
                "styled slot {slot} out of range (0 = bold, 1 = italic, 2 = bold-italic)"
            ));
        }
        let owned: std::sync::Arc<[u8]> = std::sync::Arc::from(bytes);
        let font = crate::font::Font::from_shared_slice(
            owned.clone(),
            crate::font::FontSettings {
                collection_index: index,
            },
        )
        .map_err(|e| e.to_string())?;
        self.styled_faces[slot] = Some(StyledFace {
            font: LazyFontdue::ready(std::sync::Arc::new(font)),
            yields_private_use: bundled::yields_private_use(&owned),
            lowered_asterisk: bundled::lowered_asterisk(&owned),
            bytes: owned,
            index,
            gid_cache: FxHashMap::default(),
            id: next_styled_face_id(),
            named,
            instance: FaceInstance::default(),
        });
        self.styled_retire_pending[slot] = None;
        self.refresh_family_instances();
        self.refresh_drawn_faces();
        self.styled_keys.clear();
        self.clear_glyph_images();
        self.shaped_runs.clear();
        self.clear_face_address_caches();
        Ok(())
    }

    /// Whether synthetic bold/italic (dilation / shear) is enabled.
    #[inline]
    #[must_use]
    pub const fn synthetic_styles(&self) -> bool {
        self.synthetic_styles
    }

    /// Enable or disable synthetic styled rendering (config `font_synthetic_style`).
    pub fn set_synthetic_styles(&mut self, on: bool) -> bool {
        if self.synthetic_styles == on {
            return false;
        }
        self.synthetic_styles = on;
        self.clear_glyph_images();
        self.refresh_drawn_faces();
        true
    }

    fn clear_fallback_candidates(&mut self) {
        self.fallback_paths.clear();
        self.fallback_user_prefix = 0;
    }

    /// Install the config fallback-font chain (`fallback_fonts`).
    pub fn set_config_fallback_fonts(&mut self, paths: &[String]) -> bool {
        if self.cfg_fallback_fonts == paths {
            return false;
        }
        self.cfg_fallback_fonts = paths.to_vec();
        (self.fallback_paths, self.fallback_user_prefix) = fallback_candidate_paths(paths);
        self.admitted_sources_sealed = false;
        self.drawn_faces = None;
        self.fallback_chain.clear();
        self.fallback_pick.clear();
        self.keys.clear();
        self.clear_glyph_images();
        true
    }

    /// Install the config symbol-fallback font (`symbol_font`), which outranks the built-ins ([`fallback_chain_order`]).
    pub fn set_config_symbol_font(&mut self, path: Option<&str>) -> bool {
        if self.cfg_symbol_font.as_deref() == path {
            return false;
        }
        self.cfg_symbol_font = path.map(str::to_string);
        let config: Vec<String> = path.map(str::to_string).into_iter().collect();
        self.symbol_chain.clear();
        (self.symbol_fallback_paths, self.symbol_user_prefix) =
            symbol_fallback_candidate_paths(&config);
        self.admitted_sources_sealed = false;
        self.drawn_faces = None;
        self.keys.clear();
        self.clear_glyph_images();
        true
    }

    /// Install the config colour-emoji font (`emoji_font`), which outranks discovery ([`fallback_chain_order`]).
    pub fn set_config_emoji_font(&mut self, path: Option<&str>) -> bool {
        if self.cfg_emoji_font.as_deref() == path {
            return false;
        }
        self.cfg_emoji_font = path.map(str::to_string);
        let config: Vec<String> = path.map(str::to_string).into_iter().collect();
        self.color_chain.clear();
        self.color_font_paths = color_emoji_candidate_paths(&config);
        self.admitted_sources_sealed = false;
        self.drawn_faces = None;
        self.forget_color_routing();
        true
    }

    /// Test/debug: the pending fallback candidate paths (config, then built-ins), before the lazy first-miss load consumes them.
    #[doc(hidden)]
    #[must_use]
    pub fn debug_fallback_candidate_paths(&self) -> &[String] {
        &self.fallback_paths
    }

    /// Test/debug: the file path of the loaded fallback chain face that covers `ch`, `None` when no chain face covers it or the covering face was byte-injected.
    #[doc(hidden)]
    #[must_use]
    pub fn debug_fallback_pick_path(&mut self, ch: char) -> Option<String> {
        self.debug_block_on_lazy_fallbacks();
        if !self.fallback_has(ch) {
            return None;
        }
        self.fallback_pick
            .get(&ch)
            .and_then(|&i| self.fallback_chain.get(i))
            .and_then(|f| f.path.clone())
    }

    /// Test/debug: the pending symbol-fallback candidate paths.
    #[doc(hidden)]
    #[must_use]
    pub fn debug_symbol_candidate_paths(&self) -> &[String] {
        &self.symbol_fallback_paths
    }

    /// Test/debug: the pending colour-emoji candidate paths.
    #[doc(hidden)]
    #[must_use]
    pub fn debug_emoji_candidate_paths(&self) -> &[String] {
        &self.color_font_paths
    }

    fn bold_unicode_gid(&mut self, ch: char) -> Option<u16> {
        self.bold_font.as_ref()?;
        if self.private_use_yields_to_embedded(self.bold_yields_private_use, ch) {
            return None;
        }
        if let Some(&g) = self.bold_gid_cache.get(&ch) {
            return g;
        }
        let g = self
            .bold_font_bytes
            .as_deref()
            .and_then(|b| aterm_font::Face::parse(b, self.bold_font_index).ok())
            .and_then(|f| f.glyph_index(ch))
            .map(|gid| gid.0)
            .filter(|&gid| gid != 0);
        self.bold_gid_cache.insert(ch, g);
        g
    }

    /// Swap the primary face from explicit bytes (the host's `terminalFontFamily`; the wasm renderer cannot read the host filesystem), re-deriving cell metrics at the current px and line height and dropping every glyph, key and shaping cache as `set_px` does.
    pub fn set_primary_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.set_primary_face(bytes, 0)
    }

    /// [`Self::set_primary_font`] for face `index` of `bytes` (a collection's face, [`FaceRef::index`]).
    pub fn set_primary_face(&mut self, bytes: &[u8], index: u32) -> Result<(), String> {
        self.set_named_primary_face(bytes, index, None)
    }

    /// [`Self::set_primary_face`] drawn at `named` ([`FaceRef::instance`]).
    fn set_named_primary_face(
        &mut self,
        bytes: &[u8],
        index: u32,
        named: Option<NamedInstance>,
    ) -> Result<(), String> {
        let (primary_bytes, font) = shared_parsed_face(bytes, index)?;
        let lm = font
            .horizontal_line_metrics(self.px)
            .ok_or("font has no horizontal line metrics at the current px")?;
        let typo_lm = typo_line_metrics(bytes, index);
        let letter_descent = bundled::letter_descent_floor(bytes);
        let (asc, desc, gap) = typo_lm.map_or((lm.ascent, lm.descent, lm.line_gap), |t| {
            (
                t.ascent * self.px,
                t.descent * self.px,
                t.line_gap * self.px,
            )
        });
        let resolved = self.compute_variations_for_face(
            Some((bytes, index, named.as_ref())),
            &self.cfg_font_variations,
            self.font_weight_dark_nudge,
        );
        let (advance, asc, desc, gap) = resolved
            .geometry
            .as_deref()
            .and_then(|coords| variation::varied_metrics_px(bytes, index, coords, self.px))
            .map_or(
                (font.metrics('M', self.px).advance_width, asc, desc, gap),
                |m| (m.m_advance, m.ascent, m.descent, m.line_gap),
            );
        let cell_w =
            checked_cell_w_from_advance(advance).ok_or("font cell width is not representable")?;
        let desc =
            letter_descent.map_or(desc, |letter| letter_floored_descent(desc, letter, self.px));
        let (cell_h, baseline) =
            checked_cell_h_baseline(asc, desc, gap, self.line_height_scale, self.baseline_adjust)
                .ok_or("font has no usable cell geometry at the current px")?;
        self.font = font;
        self.shaping_primary_bytes = Some(primary_bytes);
        self.primary_index = index;
        self.primary_named = named;
        self.var_coords = None;
        self.geom_coords = None;
        self.vf_bold_coords = None;
        self.shaping_variations.clear();
        self.has_ligature_features = ligature_shaping::font_has_ligature_features(bytes, index);
        self.typo_lm = typo_lm;
        self.letter_descent = letter_descent;
        self.forget_cell_geometry();
        self.primary_yields_private_use = bundled::yields_private_use(bytes);
        self.primary_fits_two_cell_glyphs = bundled::fits_two_cell_glyphs(bytes);
        self.primary_yields_wide_characters = bundled::yields_wide_characters(bytes);
        self.primary_yields_small_dingbats = bundled::yields_small_dingbats(bytes);
        self.primary_lowered_asterisk = bundled::lowered_asterisk(bytes);
        self.deco_tables = deco_tables(bytes, index);
        self.undercurl_masks.borrow_mut().clear();
        self.cell_w = cell_w;
        self.cell_h = cell_h;
        self.baseline = baseline;
        self.styled_faces = [None, None, None];
        self.styled_retire_pending = [None; 3];
        self.styled_loaded = false;
        self.primary_path = None;
        self.admitted_sources_sealed = false;
        self.drawn_faces = None;
        self.clear_glyph_images();
        self.keys.clear();
        self.emoji_keys.clear();
        self.styled_keys.clear();
        self.cluster_shapes.clear();
        self.shaped_runs.clear();
        self.fallback_pick.clear();
        self.primary_gid_cache.clear();
        self.deco_masks.borrow_mut().clear();
        self.clear_face_address_caches();
        self.install_variations(resolved, (cell_w, cell_h, baseline));
        Ok(())
    }

    /// Install a primary face whose bytes a host worker resolved from `path`.
    pub fn set_primary_font_from_resolved_path(
        &mut self,
        bytes: &[u8],
        face: FaceRef,
    ) -> Result<(), String> {
        let FaceRef {
            path,
            index,
            instance,
        } = face;
        self.set_named_primary_face(bytes, index, instance)?;
        self.primary_path = Some(path);
        self.admitted_sources_sealed = false;
        self.drawn_faces = None;
        Ok(())
    }

    /// Build from the first available system monospace font ([`FONT_CANDIDATES`]).
    pub fn from_system(px: f32, theme: Theme) -> Option<Self> {
        Self::from_system_with_family(None, px, theme)
    }

    /// Build from one already-resolved font file's admitted bytes.
    pub fn from_resolved_font_file(
        face: FaceRef,
        bytes: &[u8],
        px: f32,
        theme: Theme,
    ) -> Result<Self, String> {
        let mut renderer = Self::from_named_face(bytes, face.index, face.instance, px, theme)?;
        renderer.primary_path = Some(face.path);
        (renderer.fallback_paths, renderer.fallback_user_prefix) = fallback_candidate_paths(&[]);
        (renderer.symbol_fallback_paths, renderer.symbol_user_prefix) =
            symbol_fallback_candidate_paths(&[]);
        renderer.color_font_paths = color_emoji_candidate_paths(&[]);
        Ok(renderer)
    }

    /// Like [`Renderer::from_system`], but tries a configured font family name first.
    pub fn from_system_with_family(family: Option<&str>, px: f32, theme: Theme) -> Option<Self> {
        if let Some(ids) = family.and_then(display_face_ids_for_family) {
            let mut renderer = Self::from_system_with_face(None, px, theme)?;
            let _ = renderer.set_display_faces(&ids);
            return Some(renderer);
        }
        let face = family.and_then(|family| resolve_family_or_bundled(family, FaceTarget::REGULAR));
        Self::from_system_with_face(face.as_ref(), px, theme)
    }

    /// Like [`Renderer::from_system`], but tries an already-resolved face first (what the host's worker admitted for `font_family`: a file and its collection index, [`FaceRef`]), then the built-in [`FONT_CANDIDATES`].
    pub fn from_system_with_face(face: Option<&FaceRef>, px: f32, theme: Theme) -> Option<Self> {
        let candidates = face
            .map(|face| (face.path.clone(), Some((face.index, face.instance.clone()))))
            .into_iter()
            .chain(primary_candidate_paths().into_iter().map(|p| (p, None)));
        for (p, chosen) in candidates {
            if let Some((id, bytes)) = bundled::face_for_family(&p)
                && let Ok(r) = Self::from_resolved_font_file(FaceRef::new(id, 0), bytes, px, theme)
            {
                return Some(r);
            }
            if let Ok(bytes) = font_file::read_font_file(std::path::Path::new(&p)) {
                let (index, instance) = chosen.unwrap_or_else(|| {
                    (
                        best_face(&bytes, None, FaceTarget::REGULAR).map_or(0, |(_, index)| index),
                        None,
                    )
                });
                if let Ok(r) = Self::from_resolved_font_file(
                    FaceRef::new(p, index).with_instance(instance),
                    &bytes,
                    px,
                    theme,
                ) {
                    return Some(r);
                }
            }
        }
        #[cfg(feature = "embedded-font")]
        if let Ok(mut r) = Self::from_bytes(embedded_font(), px, theme) {
            (r.fallback_paths, r.fallback_user_prefix) = fallback_candidate_paths(&[]);
            (r.symbol_fallback_paths, r.symbol_user_prefix) = symbol_fallback_candidate_paths(&[]);
            r.color_font_paths = color_emoji_candidate_paths(&[]);
            return Some(r);
        }
        None
    }

    fn ensure_fallback(&mut self) {
        self.poll_fallback_parses();
        if !self.fallback_chain.is_empty()
            || self.fallback_parse_rx.is_some()
            || self.fallback_paths.is_empty()
        {
            return;
        }
        let user_prefix = std::mem::take(&mut self.fallback_user_prefix);
        let paths = weight_matched_fallback_paths(
            std::mem::take(&mut self.fallback_paths),
            user_prefix,
            self.runtime_probe_ctx().primary_weight,
            |p| {
                #[cfg(windows)]
                if let Some(moved) = relocate_windows_builtin_font_path(p, &windows_fonts_dir()) {
                    return std::path::Path::new(&moved).is_file();
                }
                std::path::Path::new(p).is_file()
            },
        );
        #[cfg(not(target_arch = "wasm32"))]
        {
            let (tx, rx) = std::sync::mpsc::channel();
            let thread_paths = paths.clone();
            if std::thread::Builder::new()
                .name("aterm-fallback-parse".into())
                .spawn(move || {
                    let _ = tx.send(build_host_fallback_chain(&thread_paths, user_prefix));
                })
                .is_ok()
            {
                self.fallback_parse_rx = Some(rx);
                return;
            }
        }
        self.fallback_chain = build_host_fallback_chain(&paths, user_prefix);
    }

    fn install_fallback_chain(&mut self, chain: Vec<FallbackFace>) {
        if self.fallback_chain.is_empty() && !chain.is_empty() {
            self.fallback_chain = chain;
        }
        self.font_epoch += 1;
    }

    fn install_symbol_result(&mut self, chain: Vec<FallbackFace>) {
        if self.symbol_chain.is_empty() {
            self.symbol_chain = chain;
        }
        self.font_epoch += 1;
    }

    fn poll_fallback_parses(&mut self) {
        let recv = |rx: &std::sync::mpsc::Receiver<Vec<FallbackFace>>, wait: bool| {
            if wait {
                rx.recv()
                    .map_err(|_| std::sync::mpsc::TryRecvError::Disconnected)
            } else {
                rx.try_recv()
            }
        };
        let wait = self.land_parses_on_poll;
        if let Some(rx) = self.fallback_parse_rx.take() {
            match recv(&rx, wait) {
                Ok(chain) => self.install_fallback_chain(chain),
                Err(std::sync::mpsc::TryRecvError::Empty) => self.fallback_parse_rx = Some(rx),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.font_epoch += 1,
            }
        }
        if let Some(rx) = self.symbol_parse_rx.take() {
            match recv(&rx, wait) {
                Ok(chain) => self.install_symbol_result(chain),
                Err(std::sync::mpsc::TryRecvError::Empty) => self.symbol_parse_rx = Some(rx),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.font_epoch += 1,
            }
        }
    }

    /// Test/debug: make every lazy-parse poll wait for the parse ([`Self::poll_fallback_parses`]), so a parse kicked by one cell's probe lands at the next cell's probe.
    #[doc(hidden)]
    #[cfg(test)]
    pub fn debug_land_lazy_parses_on_poll(&mut self, on: bool) {
        self.land_parses_on_poll = on;
    }

    /// The glyph-routing epoch.
    #[inline]
    #[must_use]
    pub const fn font_epoch(&self) -> u64 {
        self.font_epoch
    }

    /// Whether a background fallback-face parse is still in flight, installing any completed one first.
    pub fn fallback_parse_pending(&mut self) -> bool {
        self.poll_fallback_parses();
        self.fallback_parse_rx.is_some() || self.symbol_parse_rx.is_some()
    }

    /// Drain the missing-font class bits accumulated since the last call ([`MISSING_FONT_CLASS_TEXT`] | [`MISSING_FONT_CLASS_EMOJI`]).
    pub fn take_missing_font_classes(&mut self) -> u8 {
        std::mem::take(&mut self.missing_font_classes)
    }

    /// Enable or disable the system font resolver on the give-up path (default on).
    pub fn set_runtime_font_discovery(&mut self, enabled: bool) -> bool {
        let widened = enabled && (!self.runtime_discovery || self.admitted_sources_sealed);
        self.runtime_discovery = enabled;
        if enabled {
            self.admitted_sources_sealed = false;
            self.drawn_faces = None;
        }
        if widened {
            self.keys.clear();
            self.styled_keys.clear();
            self.emoji_keys.clear();
            self.clear_glyph_images();
            self.font_epoch += 1;
        }
        widened
    }

    fn block_on_lazy_fallbacks(&mut self) {
        self.ensure_fallback();
        let _ = self.settle_in_flight_fallback_parses();
        self.ensure_symbol_fallback();
        let _ = self.settle_in_flight_fallback_parses();
    }

    /// Wait for any broad- or symbol-chain parse in flight and install what it produced (a chain, or the give-up), returning whether there was one.
    pub fn settle_in_flight_fallback_parses(&mut self) -> bool {
        let mut settled = false;
        if let Some(rx) = self.fallback_parse_rx.take() {
            match rx.recv() {
                Ok(chain) => self.install_fallback_chain(chain),
                Err(_) => self.font_epoch += 1,
            }
            settled = true;
        }
        if let Some(rx) = self.symbol_parse_rx.take() {
            match rx.recv() {
                Ok(chain) => self.install_symbol_result(chain),
                Err(_) => self.font_epoch += 1,
            }
            settled = true;
        }
        settled
    }

    /// Resolve and pre-rasterize a bounded semantic text specimen through the terminal's real font cascade.
    pub fn prepare_semantic_text(&mut self, text: &str) {
        use aterm_grapheme::GraphemeClusters;

        const MAX_CLUSTERS: usize = 256;
        const MAX_SCALARS: usize = 512;
        let mut scalar_count = 0_usize;
        let clusters: Vec<&str> = text
            .graphemes()
            .take(MAX_CLUSTERS)
            .take_while(|cluster| {
                let count = cluster.chars().take(MAX_SCALARS + 1).count();
                if count > MAX_SCALARS.saturating_sub(scalar_count) {
                    return false;
                }
                scalar_count += count;
                true
            })
            .collect();

        let needs_mono_fallback = clusters
            .iter()
            .flat_map(|cluster| cluster.chars())
            .any(|ch| {
                !ch.is_whitespace()
                    && !ch.is_control()
                    && !aterm_grapheme::is_emoji_char(ch)
                    && !procedural::covers(ch)
                    && self.primary_unicode_gid(ch).is_none()
            });
        if needs_mono_fallback {
            self.block_on_lazy_fallbacks();
        }
        if clusters
            .iter()
            .flat_map(|cluster| cluster.chars())
            .any(aterm_grapheme::is_emoji_char)
        {
            self.ensure_color_font();
        }

        for cluster in clusters {
            let emoji_cluster =
                cluster.chars().count() > 1 && cluster.chars().any(aterm_grapheme::is_emoji_char);
            if emoji_cluster && let Some(key) = self.glyph_key_cluster(cluster) {
                let _ = self.glyph_image(key);
                continue;
            }
            for ch in cluster.chars() {
                if matches!(ch, '\u{200D}' | '\u{FE0E}' | '\u{FE0F}') {
                    continue;
                }
                let key = self.glyph_key(ch);
                let _ = self.glyph_image(key);
            }
        }
        self.evict_glyph_caches_if_large();
    }

    /// Settle and warm every terminal typography style used by a semantic specimen.
    pub fn prepare_semantic_typography(&mut self, text: &str) {
        self.ensure_styled_faces();
        self.prepare_semantic_text(text);

        let ascii = text
            .chars()
            .filter(char::is_ascii)
            .take(256)
            .collect::<String>();
        for style in [
            StyleBits::BOLD,
            StyleBits::ITALIC,
            StyleBits(StyleBits::BOLD.0 | StyleBits::ITALIC.0),
        ] {
            for ch in ascii.chars() {
                if let Some(key) = self.covered_semantic_glyph_key(ch, style) {
                    let _ = self.glyph_image(key);
                }
            }
            for glyph in self.semantic_ascii_glyph_plan(&ascii, style) {
                let _ = self.glyph_image(glyph.key);
            }
        }
        self.evict_glyph_caches_if_large();
    }

    /// Resolve one semantic-surface character only when the cascade owns a real glyph.
    pub fn covered_semantic_glyph_key(&mut self, ch: char, style: StyleBits) -> Option<GlyphKey> {
        let key = self.glyph_key_styled(ch, style);
        let primary_notdef =
            key.source == FaceId::Primary && self.primary_unicode_gid(ch).is_none();
        (!primary_notdef).then_some(key)
    }

    /// Plan a short one-row terminal run through the same shaping seam as a terminal frame.
    #[must_use]
    pub fn semantic_ascii_glyph_plan(
        &mut self,
        text: &str,
        style: StyleBits,
    ) -> Vec<SemanticGlyph> {
        const MAX_CELLS: usize = 256;
        let chars: Vec<char> = text.chars().take(MAX_CELLS).collect();
        if chars.is_empty() || chars.iter().any(|ch| !ch.is_ascii()) {
            return Vec::new();
        }
        self.apply_pending_styled_retires();
        let bold = style.contains(StyleBits::BOLD);
        let italic = style.contains(StyleBits::ITALIC);
        let cells: Vec<RenderCell> = chars
            .iter()
            .copied()
            .map(|ch| RenderCell {
                ch,
                fg: [255; 3],
                bg: [0; 3],
                wide: false,
                emoji_presentation: false,
                text_presentation: false,
                bold,
                italic,
                underline: UnderlineStyle::None,
                strikethrough: false,
                overline: false,
                underline_color: None,
                overline_color: None,
                script: aterm_core::terminal::Script::Normal,
            })
            .collect();
        let mut input = RenderInput::empty();
        input.rows = 1;
        input.cols = cells.len();
        input.cells.push(cells);

        let mut plan = Vec::new();
        self.row_glyph_plan(&input, 0, &[], &mut plan);
        let mut out = Vec::with_capacity(chars.len());
        for (column, ch) in chars.into_iter().enumerate() {
            let key = match plan.get(column).copied().unwrap_or(ColumnGlyph::PerCell) {
                ColumnGlyph::Ligated(gid) => Some(self.ligature_key(gid, style)),
                ColumnGlyph::LigatedSlice { gid, k, .. } => {
                    Some(self.ligature_slice_key(gid, k, style))
                }
                ColumnGlyph::PerCell => self.covered_semantic_glyph_key(ch, style),
            };
            if let Some(key) = key {
                out.push(SemanticGlyph { column, key });
            }
        }
        self.evict_glyph_caches_if_large();
        out
    }

    /// Test/debug: force the lazy broad and symbol fallback faces to load now, blocking on the background parses, so glyph routing for those slots is final.
    #[doc(hidden)]
    pub fn debug_block_on_lazy_fallbacks(&mut self) {
        self.block_on_lazy_fallbacks();
    }

    fn fallback_has(&mut self, ch: char) -> bool {
        self.ensure_fallback();
        if self.fallback_pick.contains_key(&ch) {
            return true;
        }
        let incidental = aterm_grapheme::is_emoji_presentation(ch)
            || font_chain::is_private_use(ch)
            || bundled::is_yijing_line_symbol(ch);
        let yield_private_use = self.foreign_private_use_yields(ch);
        for (i, face) in self.fallback_chain.iter().enumerate() {
            if incidental && face.path.as_deref().is_some_and(bundled::is_bundled_id) {
                continue;
            }
            if face.scope.as_ref().is_some_and(|scope| !scope.admits(ch)) {
                continue;
            }
            if yield_private_use && chain_face_is_foreign_private_use(face) {
                continue;
            }
            let covered = aterm_font::Face::parse(&face.bytes, face.index)
                .ok()
                .and_then(|f| f.glyph_index(ch))
                .is_some_and(|g| g.0 != 0);
            if covered {
                self.fallback_pick.insert(ch, i);
                return true;
            }
        }
        false
    }

    fn ensure_symbol_fallback(&mut self) {
        self.poll_fallback_parses();
        if !self.symbol_chain.is_empty()
            || self.symbol_parse_rx.is_some()
            || self.symbol_fallback_paths.is_empty()
        {
            return;
        }
        let paths = std::mem::take(&mut self.symbol_fallback_paths);
        let user_prefix = self.symbol_user_prefix;
        #[cfg(not(target_arch = "wasm32"))]
        {
            let (tx, rx) = std::sync::mpsc::channel();
            let thread_paths = paths.clone();
            if std::thread::Builder::new()
                .name("aterm-symbol-parse".into())
                .spawn(move || {
                    let _ = tx.send(build_symbol_chain(&thread_paths, user_prefix));
                })
                .is_ok()
            {
                self.symbol_parse_rx = Some(rx);
                return;
            }
        }
        self.symbol_chain = build_symbol_chain(&paths, user_prefix);
    }

    fn symbol_chain_pick(&mut self, ch: char) -> Option<usize> {
        self.ensure_symbol_fallback();
        let incidental =
            aterm_grapheme::is_emoji_presentation(ch) || font_chain::is_private_use(ch);
        let nerd_owned = bundled::ACTIVE && !incidental && embedded_symbols_own_text_point(ch);
        let yield_private_use = self.foreign_private_use_yields(ch);
        self.symbol_chain.iter().position(|face| {
            if incidental && symbol_face_is_text_only_backstop(face) {
                return false;
            }
            if nerd_owned && face.path.as_deref().is_some_and(bundled::is_bundled_id) {
                return false;
            }
            if yield_private_use && chain_face_is_foreign_private_use(face) {
                return false;
            }
            aterm_font::Face::parse(&face.bytes, face.index)
                .ok()
                .and_then(|parsed| parsed.glyph_index(ch))
                .is_some_and(|g| g.0 != 0)
        })
    }

    fn foreign_private_use_yields(&self, ch: char) -> bool {
        self.private_use_yields_to_embedded(!FOREIGN_PRIVATE_USE_FACES.is_empty(), ch)
    }

    fn private_use_yields_to_embedded(&self, face_yields: bool, ch: char) -> bool {
        face_yields
            && self.runtime_discovery
            && font_chain::is_private_use(ch)
            && embedded_symbols_map(ch)
    }

    fn styled_face_covers(&self, slot: usize, ch: char) -> bool {
        self.styled_faces[slot].as_ref().is_some_and(|sf| {
            sf.covers(ch) && !self.private_use_yields_to_embedded(sf.yields_private_use, ch)
        })
    }

    fn symbol_chain_may_be_chosen(&self) -> bool {
        self.symbol_user_prefix > 0 || self.symbol_chain.iter().any(|face| face.chosen)
    }

    fn symbol_fallback_has(&mut self, ch: char, built_in_yields: bool) -> bool {
        self.symbol_chain_pick(ch)
            .is_some_and(|i| !built_in_yields || self.symbol_chain[i].chosen)
    }

    fn ensure_styled_faces(&mut self) {
        if self.styled_loaded {
            return;
        }
        self.styled_loaded = true;
        self.discover_styled_faces();
        // No glyph has drawn from a face that just arrived, so nothing cached
        // holds its old instance.
        self.refresh_family_instances();
    }

    /// [`Self::ensure_styled_faces`]'s discovery: fill each empty styled slot from the
    /// bundled siblings, the sibling files beside the primary, then its collection.
    fn discover_styled_faces(&mut self) {
        let Some(primary) = self.primary_path.clone() else {
            // A byte-loaded or embedded primary has no sibling file to derive.
            return;
        };
        if bundled::is_bundled_id(&primary) {
            for (slot, bytes) in bundled::styled_siblings(&primary).into_iter().enumerate() {
                if let Some(bytes) = bytes
                    && self.styled_faces[slot].is_none()
                    && face_admissible(bytes, 0)
                {
                    self.styled_faces[slot] = Some(StyledFace {
                        font: LazyFontdue::deferred(),
                        bytes: shared_face_bytes(bytes, 0),
                        index: 0,
                        gid_cache: FxHashMap::default(),
                        id: next_styled_face_id(),
                        yields_private_use: bundled::yields_private_use(bytes),
                        lowered_asterisk: bundled::lowered_asterisk(bytes),
                        named: None,
                        instance: FaceInstance::default(),
                    });
                }
            }
            return;
        }
        let candidates = styled_sibling_paths(&primary);
        for (slot, cands) in candidates.into_iter().enumerate() {
            for p in cands {
                if self.styled_faces[slot].is_some() {
                    break;
                }
                if let Ok(bytes) = font_file::read_font_file(std::path::Path::new(&p))
                    && face_admissible(&bytes, 0)
                {
                    self.styled_faces[slot] = Some(StyledFace {
                        font: LazyFontdue::deferred(),
                        bytes: shared_face_bytes(&bytes, 0),
                        index: 0,
                        gid_cache: FxHashMap::default(),
                        id: next_styled_face_id(),
                        yields_private_use: bundled::yields_private_use(&bytes),
                        lowered_asterisk: bundled::lowered_asterisk(&bytes),
                        named: None,
                        instance: FaceInstance::default(),
                    });
                    break;
                }
            }
        }
        if self.styled_faces.iter().any(Option::is_none)
            && let Some(bytes) = self.shaping_primary_bytes.clone()
        {
            self.fill_styled_from_collection(&bytes, self.primary_index);
        }
    }

    /// Test/debug: the loaded styled-face collection indices `[bold, italic, bold-italic]` after [`Self::ensure_styled_faces`].
    #[doc(hidden)]
    pub fn debug_styled_face_indices(&self) -> [Option<u32>; 3] {
        std::array::from_fn(|i| self.styled_faces[i].as_ref().map(|sf| sf.index))
    }

    /// The resolved primary face, `(bytes, collection index)`, for the GUI chrome (the tray/overlay text `tray_raster` draws), so Settings/About/Palette use the user's terminal font.
    #[must_use]
    pub fn chrome_primary_face(&self) -> Option<(std::sync::Arc<[u8]>, u32)> {
        self.shaping_primary_bytes
            .clone()
            .map(|b| (b, self.primary_index))
    }

    /// The admitted path that supplied the primary face, when it came from a native file.
    #[must_use]
    pub fn primary_source_path(&self) -> Option<&str> {
        self.primary_path.as_deref()
    }

    /// Return the exact retained face sources for this renderer without path resolution, filesystem access, or hashing.
    #[must_use]
    pub fn admitted_font_sources(&self) -> AdmittedFontSources {
        let slice = |bytes: &std::sync::Arc<[u8]>, index| AdmittedFaceSource {
            bytes: AdmittedFontBlob::Slice(std::sync::Arc::clone(bytes)),
            index,
        };
        let vec = |bytes: &crate::font::FaceBytes, index| AdmittedFaceSource {
            bytes: AdmittedFontBlob::Face(bytes.clone()),
            index,
        };
        AdmittedFontSources {
            sealed: self.admitted_sources_sealed,
            primary: self
                .shaping_primary_bytes
                .as_ref()
                .map(|bytes| slice(bytes, self.primary_index)),
            injected_bold: self
                .bold_font_bytes
                .as_ref()
                .map(|bytes| slice(bytes, self.bold_font_index)),
            styled: std::array::from_fn(|slot| {
                self.styled_faces[slot]
                    .as_ref()
                    .map(|face| slice(&face.bytes, face.index))
            }),
            fallback: self
                .fallback_chain
                .iter()
                .map(|face| vec(&face.bytes, face.index))
                .collect(),
            symbol: self
                .symbol_chain
                .iter()
                .map(|face| vec(&face.bytes, face.index))
                .collect(),
            emoji: self
                .color_chain
                .iter()
                .map(|face| vec(&face.bytes, 0))
                .collect(),
        }
    }

    /// Whether this renderer's path-backed generation has crossed the worker-only seal boundary and can be published without later font i/o.
    #[inline]
    #[must_use]
    pub const fn admitted_font_sources_sealed(&self) -> bool {
        self.admitted_sources_sealed
    }

    /// Settle and close one complete path-backed font generation.
    pub fn seal_admitted_font_sources(&mut self) -> AdmittedFontSources {
        self.ensure_fallback();
        self.ensure_symbol_fallback();
        self.ensure_styled_faces();
        self.ensure_color_chain();
        self.block_on_lazy_fallbacks();

        self.clear_fallback_candidates();
        self.symbol_fallback_paths.clear();
        self.symbol_user_prefix = 0;
        self.color_font_paths.clear();
        self.fallback_parse_rx = None;
        self.symbol_parse_rx = None;

        self.runtime_fallback = RuntimeFallback::default();
        self.missing_font_classes = 0;
        self.keys.clear();
        self.styled_keys.clear();
        self.clear_glyph_images();
        self.emoji_keys.clear();
        self.cluster_shapes.clear();
        self.shaped_runs.clear();
        self.admitted_sources_sealed = true;
        self.refresh_drawn_faces();
        self.admitted_font_sources()
    }

    /// What this sealed generation draws each style with, for Settings to name ([`DrawnFaces`]).
    #[inline]
    #[must_use]
    pub fn drawn_faces(&self) -> Option<&DrawnFaces> {
        self.drawn_faces.as_ref()
    }

    fn refresh_drawn_faces(&mut self) {
        self.drawn_faces = self
            .admitted_sources_sealed
            .then(|| self.compute_drawn_faces());
    }

    fn compute_drawn_faces(&self) -> DrawnFaces {
        let regular = self
            .shaping_primary_bytes
            .as_deref()
            .and_then(|bytes| face_family_name(bytes, self.primary_index));
        let present = std::array::from_fn(|slot| self.styled_faces[slot].is_some());
        let styles = [
            StyleBits::BOLD,
            StyleBits::ITALIC,
            StyleBits(StyleBits::BOLD.0 | StyleBits::ITALIC.0),
        ]
        .map(|style| {
            let (face, synthetic) =
                match resolve_styled_face(style, self.bold_font_bytes.is_some(), present) {
                    // An instanced face is named for what it draws ("SF Mono Italic"): its
                    // name table names only its `fvar` default.
                    FacePick::InjectedBold { synthetic } => (
                        self.bold_font_bytes.as_deref().and_then(|bytes| {
                            if let Some(named) = &self.bold_font_named {
                                named_instance_display_name(bytes, self.bold_font_index, named)
                            } else if let Some(coords) = self.bold_font_instance.drawn.as_deref() {
                                bold_instance_name(bytes, self.bold_font_index, coords).or_else(
                                    || {
                                        face_family_name(bytes, self.bold_font_index)
                                            .map(|family| format!("{family} Bold"))
                                    },
                                )
                            } else {
                                face_display_name(bytes, self.bold_font_index)
                            }
                        }),
                        synthetic,
                    ),
                    FacePick::Styled { slot, synthetic } => {
                        let (coords, var_slot, instanced) = self.styled_instance(slot, synthetic);
                        let face = self.styled_faces[slot].as_ref();
                        match (face, coords) {
                            // A face its name chose an instance of is called by that
                            // instance's name where it draws it.
                            (Some(face), _) if var_slot != 2 && face.named.is_some() => (
                                face.named.as_ref().and_then(|named| {
                                    named_instance_display_name(&face.bytes, face.index, named)
                                }),
                                instanced,
                            ),
                            (Some(face), Some(coords)) => {
                                // The style the instance draws: the face's own, and bold
                                // where its bold instance serves a bold-italic cell.
                                let drawn = StyleBits(style.0 & !instanced.0);
                                let words = match (
                                    drawn.contains(StyleBits::BOLD),
                                    drawn.contains(StyleBits::ITALIC),
                                ) {
                                    (true, true) => "Bold Italic",
                                    (true, false) => "Bold",
                                    _ => "Italic",
                                };
                                // At another named instance than its role's own (the
                                // italic of a primary named `SF Mono Light`), that one's
                                // name; a bold by the weight it draws.
                                let named = if drawn.contains(StyleBits::BOLD) {
                                    bold_instance_name(&face.bytes, face.index, &coords)
                                } else {
                                    other_named_instance_name(&face.bytes, face.index, &coords)
                                };
                                (
                                    named.or_else(|| {
                                        face_family_name(&face.bytes, face.index)
                                            .map(|family| format!("{family} {words}"))
                                    }),
                                    instanced,
                                )
                            }
                            (face, _) => (
                                face.and_then(|face| face_display_name(&face.bytes, face.index)),
                                synthetic,
                            ),
                        }
                    }
                    FacePick::Primary
                        if style.contains(StyleBits::BOLD) && self.vf_bold_coords.is_some() =>
                    {
                        (
                            self.vf_bold_coords
                                .as_deref()
                                .zip(self.shaping_primary_bytes.as_deref())
                                .and_then(|(coords, bytes)| {
                                    bold_instance_name(bytes, self.primary_index, coords)
                                })
                                .or_else(|| {
                                    regular.as_ref().map(|family| format!("{family} Bold"))
                                }),
                            StyleBits(style.0 & !StyleBits::BOLD.0),
                        )
                    }
                    FacePick::Primary => (None, style),
                };
            StyleDraw {
                face,
                synthetic: if self.synthetic_styles {
                    synthetic
                } else {
                    StyleBits::REGULAR
                },
            }
        });
        DrawnFaces {
            regular,
            regular_has_ligatures: self.shaping_primary_bytes.as_deref().is_some_and(|bytes| {
                ligature_shaping::font_has_ligature_features(bytes, self.primary_index)
            }),
            styles,
            symbol: self
                .symbol_chain
                .first()
                .and_then(|face| face_family_name(&face.bytes, face.index)),
            emoji: self
                .color_chain
                .first()
                .and_then(|face| face_family_name(&face.bytes, 0)),
        }
    }

    /// Rebuild this complete font generation at `px`/`theme` from retained bytes and parsed immutable faces only.
    pub fn rebuild_from_admitted(&self, px: f32, theme: Theme) -> Result<Self, String> {
        if !self.admitted_sources_sealed {
            return Err(
                "font generation is not sealed; prepare it on the font worker first".into(),
            );
        }
        let primary = self
            .shaping_primary_bytes
            .as_deref()
            .ok_or("font generation has no retained primary bytes")?;
        let mut rebuilt = Self::from_named_face(
            primary,
            self.primary_index,
            self.primary_named.clone(),
            px,
            theme,
        )?;

        if let Some(bytes) = self.bold_font_bytes.as_deref() {
            rebuilt.set_named_bold_face(
                bytes,
                self.bold_font_index,
                self.bold_font_named.clone(),
            )?;
        }
        rebuilt.styled_faces = self.styled_faces.clone();
        rebuilt.styled_loaded = true;
        rebuilt.primary_path = self.primary_path.clone();
        rebuilt.fallback_chain = self.fallback_chain.clone();
        rebuilt.display_layer = self.display_layer.clone();
        rebuilt.clear_fallback_candidates();
        rebuilt
            .cfg_fallback_fonts
            .clone_from(&self.cfg_fallback_fonts);
        rebuilt.symbol_chain = self.symbol_chain.clone();
        rebuilt.symbol_fallback_paths.clear();
        rebuilt.symbol_user_prefix = 0;
        rebuilt.cfg_symbol_font.clone_from(&self.cfg_symbol_font);
        rebuilt.color_chain = self.color_chain.clone();
        rebuilt.color_font_paths.clear();
        rebuilt.cfg_emoji_font.clone_from(&self.cfg_emoji_font);
        rebuilt.fallback_parse_rx = None;
        rebuilt.symbol_parse_rx = None;
        rebuilt.runtime_fallback = self.runtime_fallback.clone();
        rebuilt.runtime_discovery = self.runtime_discovery;
        rebuilt.admitted_sources_sealed = true;

        rebuilt.synthetic_styles = self.synthetic_styles;
        rebuilt.set_text_shaping(self.shaping.clone());
        let _ = rebuilt.set_font_variations(&self.cfg_font_variations, self.font_weight_dark_nudge);
        rebuilt.set_line_height(self.line_height_scale);
        rebuilt.set_adjust_baseline(self.baseline_adjust);
        rebuilt.set_adjust_underline(self.underline_adjust_pos, self.underline_adjust_thick);
        rebuilt.set_underline_skip_descenders(self.underline_skip_descenders);
        rebuilt.set_stem_gamma(self.stem_gamma);
        rebuilt.set_text_blending(self.text_blending);
        rebuilt.set_font_thicken(self.font_thicken);
        rebuilt.set_selection_fg(self.selection_fg);
        rebuilt.set_minimum_contrast(self.min_contrast);
        rebuilt.set_background_opacity(self.bg_opacity);
        rebuilt.set_cursor_opacity(self.cursor_opacity);
        rebuilt.set_selection_inactive(self.selection_inactive);
        rebuilt.set_selection_inactive_bg(self.selection_inactive_bg);
        #[cfg(target_os = "macos")]
        {
            rebuilt.rasterizer = self.rasterizer;
        }
        #[cfg(any(all(unix, not(target_os = "macos")), windows))]
        {
            rebuilt.hint_mode = self.hint_mode;
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            rebuilt.subpix_mode = self.subpix_mode;
        }
        rebuilt.refresh_drawn_faces();
        Ok(rebuilt)
    }

    /// The real bold weight of the primary family for the GUI chrome, `(bytes, collection index)`.
    #[must_use]
    pub fn chrome_bold_face(&mut self) -> Option<(std::sync::Arc<[u8]>, u32)> {
        if let Some(b) = &self.bold_font_bytes {
            return Some((b.clone(), self.bold_font_index));
        }
        self.ensure_styled_faces();
        self.styled_faces[0]
            .as_ref()
            .map(|sf| (sf.bytes.clone(), sf.index))
    }

    /// Every broad-chain face, then every symbol-chain face, this generation has already parsed, in chain precedence order (so first-hit-wins agrees with [`FaceId::Fallback`]).
    pub fn ready_fallback_faces(
        &self,
    ) -> impl Iterator<Item = &std::sync::Arc<crate::font::Font>> + '_ {
        self.fallback_chain
            .iter()
            .chain(self.symbol_chain.iter())
            .filter(|face| face.scope.is_none())
            .filter_map(|face| face.font.parsed())
    }

    /// The cells behind [`Self::ready_fallback_faces`].
    #[must_use]
    pub fn fallback_coverage(&self) -> FallbackCoverage {
        FallbackCoverage {
            faces: self
                .fallback_chain
                .iter()
                .chain(self.symbol_chain.iter())
                .filter(|face| face.scope.is_none())
                .map(|face| (face.font.clone(), face.bytes.clone(), face.index))
                .collect(),
        }
    }

    /// Fork the live terminal's font engine for a renderer-native semantic surface (Settings specimens, Markdown, editor chrome) without borrowing the presentation renderer or consulting a platform text widget.
    #[must_use]
    pub fn fork_semantic_surface(&self, px: f32, theme: Theme) -> Option<Self> {
        if self.admitted_sources_sealed {
            return self.rebuild_from_admitted(px, theme).ok();
        }
        let primary = self.shaping_primary_bytes.as_deref()?;
        let mut fork = Self::from_named_face(
            primary,
            self.primary_index,
            self.primary_named.clone(),
            px,
            theme,
        )
        .ok()?;

        fork.fallback_chain = self.fallback_chain.clone();
        fork.display_layer = self.display_layer.clone();
        fork.cfg_fallback_fonts = self.cfg_fallback_fonts.clone();
        (fork.fallback_paths, fork.fallback_user_prefix) = if fork.fallback_chain.is_empty() {
            fallback_candidate_paths(&fork.cfg_fallback_fonts)
        } else {
            (Vec::new(), 0)
        };
        fork.symbol_chain = self.symbol_chain.clone();
        fork.cfg_symbol_font = self.cfg_symbol_font.clone();
        (fork.symbol_fallback_paths, fork.symbol_user_prefix) = if fork.symbol_chain.is_empty() {
            symbol_fallback_candidate_paths(fork.cfg_symbol_font.as_slice())
        } else {
            (Vec::new(), 0)
        };
        fork.color_chain = self.color_chain.clone();
        fork.cfg_emoji_font = self.cfg_emoji_font.clone();
        fork.color_font_paths = if fork.color_chain.is_empty() {
            color_emoji_candidate_paths(fork.cfg_emoji_font.as_slice())
        } else {
            self.color_font_paths.clone()
        };
        fork.runtime_fallback = self.runtime_fallback.clone();
        fork.runtime_discovery = self.runtime_discovery;
        fork.missing_font_classes = 0;

        if let Some(bytes) = self.bold_font_bytes.as_deref() {
            let _ =
                fork.set_named_bold_face(bytes, self.bold_font_index, self.bold_font_named.clone());
        }
        fork.styled_faces = self.styled_faces.clone();
        fork.primary_path = self.primary_path.clone();
        fork.styled_loaded = self.styled_loaded;
        fork.synthetic_styles = self.synthetic_styles;
        fork.set_text_shaping(self.shaping.clone());
        let _ = fork.set_font_variations(&self.cfg_font_variations, self.font_weight_dark_nudge);

        fork.set_line_height(self.line_height_scale);
        fork.set_adjust_baseline(self.baseline_adjust);
        fork.set_adjust_underline(self.underline_adjust_pos, self.underline_adjust_thick);
        fork.set_underline_skip_descenders(self.underline_skip_descenders);
        fork.set_stem_gamma(self.stem_gamma);
        fork.set_text_blending(self.text_blending);
        fork.set_font_thicken(self.font_thicken);
        #[cfg(target_os = "macos")]
        {
            fork.rasterizer = self.rasterizer;
        }
        #[cfg(any(all(unix, not(target_os = "macos")), windows))]
        {
            fork.hint_mode = self.hint_mode;
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            fork.subpix_mode = self.subpix_mode;
        }
        Some(fork)
    }

    /// Test/debug: force the deterministic portable rasterizer on this renderer (no process-global env var, so it can't race other tests).
    #[doc(hidden)]
    pub fn debug_force_fontdue(&mut self) {
        #[cfg(any(all(unix, not(target_os = "macos")), windows))]
        {
            self.hint_mode = hinted::HintMode::Off;
            self.hint_bank.clear();
            self.clear_glyph_images();
            self.keys.clear();
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                self.subpix_mode = subpixel::SubpixelMode::Off;
                self.subpx_bank.clear();
                self.subpx_glyphs.clear();
            }
        }
        #[cfg(target_os = "macos")]
        {
            self.rasterizer = RasterKind::Fontdue;
            self.ct_cache.clear();
            self.ct_descriptors.clear();
            self.clear_glyph_images();
            self.runtime_fallback = RuntimeFallback::default();
            self.keys.clear();
        }
    }

    #[cfg(target_os = "macos")]
    #[allow(clippy::too_many_arguments)]
    fn ct_glyph(
        &mut self,
        key_ptr: usize,
        bytes: impl Into<crate::font::FaceBytes>,
        index: u32,
        gid: u16,
        px: f32,
        variations: &[(u32, f32)],
        var_slot: u8,
    ) -> Option<(usize, usize, i32, i32, f32, Vec<u8>)> {
        if self.rasterizer != RasterKind::CoreText {
            return None;
        }
        let descs = self
            .ct_descriptors
            .entry(key_ptr)
            .or_insert_with(|| macos_coretext::CtFaceDescriptors::new(bytes.into()));
        let face = self
            .ct_cache
            .entry((key_ptr, index, px.to_bits(), var_slot))
            .or_insert_with(|| {
                descs.as_ref().and_then(|d| {
                    macos_coretext::CtFont::from_descriptors(d, index, px, variations)
                })
            });
        let rg = face.as_ref()?.rasterize(gid, self.font_thicken)?;
        Some((rg.width, rg.height, rg.xmin, rg.ymin, rg.advance, rg.bytes))
    }

    fn vf_bold_mono_raster(
        &mut self,
        ch: char,
        key: GlyphKey,
    ) -> Option<(usize, usize, i32, i32, f32, Vec<u8>)> {
        if key.source != FaceId::Primary || !key.style.contains(StyleBits::BOLD) {
            return None;
        }
        let coords = self.vf_bold_coords.clone()?;
        #[cfg(target_os = "macos")]
        {
            let gid = self.primary_unicode_gid(ch)?;
            let bytes = self.shaping_primary_bytes.clone()?;
            let index = self.primary_index;
            // The portable raster draws the bold instance where CoreText is not the
            // rasterizer, as it does on the other platforms.
            self.ct_glyph(
                bytes.as_ptr() as usize,
                bytes.clone(),
                index,
                gid,
                self.px,
                &coords,
                2,
            )
            .or_else(|| variation::varied_glyph_raster(&bytes, index, &coords, gid, self.px))
        }
        #[cfg(not(target_os = "macos"))]
        {
            let gid = self.primary_unicode_gid(ch)?;
            #[cfg(windows)]
            if let Some(bytes) = self.shaping_primary_bytes.clone()
                && let Some(t) = self.hinted_glyph(
                    bytes.as_ptr() as usize,
                    &bytes,
                    self.primary_index,
                    gid,
                    &coords,
                    2,
                )
            {
                return Some(t);
            }
            let bytes = self.shaping_primary_bytes.as_deref()?;
            variation::varied_glyph_raster(bytes, self.primary_index, &coords, gid, self.px)
        }
    }

    #[cfg(any(all(unix, not(target_os = "macos")), windows))]
    fn hinted_glyph(
        &mut self,
        key_ptr: usize,
        bytes: &[u8],
        index: u32,
        gid: u16,
        variations: &[(u32, f32)],
        var_slot: u8,
    ) -> Option<(usize, usize, i32, i32, f32, Vec<u8>)> {
        let inst = self.hint_bank.instance(
            key_ptr,
            bytes,
            index,
            self.px,
            self.hint_mode,
            variations,
            var_slot,
        )?;
        hinted::hinted_glyph_raster(bytes, index, gid, self.px, &inst, variations)
    }

    #[cfg(any(all(unix, not(target_os = "macos")), windows))]
    fn hinted_primary_char(
        &mut self,
        key: GlyphKey,
        ch: char,
    ) -> Option<(usize, usize, i32, i32, f32, Vec<u8>)> {
        if key.source != FaceId::Primary {
            return None;
        }
        #[cfg(unix)]
        if self.var_coords.is_some() {
            return None;
        }
        let gid = self.primary_unicode_gid(ch)?;
        let bytes = self.shaping_primary_bytes.clone()?;
        let coords = self.var_coords.clone();
        let (vars, slot) = coords.as_deref().map_or((&[][..], 0u8), |c| (c, 1));
        let index = self.primary_index;
        self.hinted_glyph(bytes.as_ptr() as usize, &bytes, index, gid, vars, slot)
    }

    #[cfg(not(any(all(unix, not(target_os = "macos")), windows)))]
    fn hinted_primary_char(
        &mut self,
        _key: GlyphKey,
        _ch: char,
    ) -> Option<(usize, usize, i32, i32, f32, Vec<u8>)> {
        None
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    fn subpx_image(&mut self, key: GlyphKey) -> Option<&SubpxImage> {
        if !self.subpx_glyphs.contains_key(&key) {
            let img = self.subpx_rasterize(key);
            self.subpx_glyphs.insert(key, img);
        }
        self.subpx_glyphs.get(&key).and_then(|o| o.as_ref())
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    fn subpx_rasterize(&mut self, key: GlyphKey) -> Option<SubpxImage> {
        if !matches!(key.source, FaceId::Primary | FaceId::BoldPrimary)
            || key.style.script() != StyleBits::REGULAR
            || key.cell_span != 0
            || self.var_coords.is_some()
        {
            return None;
        }
        let (bytes, index, gid): (std::sync::Arc<[u8]>, u32, u16) = match key.glyph_class {
            GlyphClass::MonoGid => {
                if key.ch_or_id >> LIG_SLICE_SHIFT != 0 {
                    return None;
                }
                let gid = key.ch_or_id as u16;
                if key.source == FaceId::BoldPrimary {
                    // Synthesis and an instanced face stay grayscale: this raster draws a
                    // face as authored.
                    if key.style.0 != StyleBits::REGULAR.0
                        || self.bold_font_instance.drawn.is_some()
                    {
                        return None;
                    }
                    (self.bold_font_bytes.clone()?, self.bold_font_index, gid)
                } else if key.style.0 == StyleBits::REGULAR.0 {
                    (self.shaping_primary_bytes.clone()?, self.primary_index, gid)
                } else {
                    if key.style.contains(StyleBits::BOLD) && self.vf_bold_coords.is_some() {
                        return None;
                    }
                    match self.run_face_pick(key.style) {
                        FacePick::Styled { slot, synthetic }
                            if synthetic.0 == StyleBits::REGULAR.0 =>
                        {
                            let sf = self.styled_faces[slot].as_ref()?;
                            // This raster applies no instance coords.
                            if sf.instance.drawn.is_some() {
                                return None;
                            }
                            (sf.bytes.clone(), sf.index, gid)
                        }
                        FacePick::InjectedBold { synthetic }
                            if synthetic.0 == StyleBits::REGULAR.0
                                && self.bold_font_instance.drawn.is_none() =>
                        {
                            (self.bold_font_bytes.clone()?, 0, gid)
                        }
                        _ => return None,
                    }
                }
            }
            GlyphClass::Mono => {
                if key.source != FaceId::Primary {
                    return None;
                }
                let ch = key.chr()?;
                let styled =
                    key.style.contains(StyleBits::BOLD) || key.style.contains(StyleBits::ITALIC);
                if styled {
                    self.ensure_styled_faces();
                    let (slot, remaining) = self.styled_face_slot(ch, key.style)?;
                    if remaining.0 != StyleBits::REGULAR.0
                        || self.styled_faces[slot]
                            .as_ref()
                            .is_some_and(|sf| sf.instance.drawn.is_some())
                    {
                        return None;
                    }
                    let gid = self.styled_faces[slot]
                        .as_mut()
                        .and_then(|sf| sf.unicode_gid(ch))?;
                    let sf = self.styled_faces[slot].as_ref()?;
                    (sf.bytes.clone(), sf.index, gid)
                } else {
                    let gid = self.primary_unicode_gid(ch)?;
                    (self.shaping_primary_bytes.clone()?, self.primary_index, gid)
                }
            }
            GlyphClass::Rgba | GlyphClass::RgbaGid | GlyphClass::RgbaCluster => return None,
        };
        let hint = if self.hint_mode == hinted::HintMode::Off {
            None
        } else {
            self.subpx_bank.instance_with(
                bytes.as_ptr() as usize,
                &bytes,
                index,
                self.px,
                Some(subpixel::lcd_hint_options()),
                &[],
                0,
            )
        };
        let bgr = self.subpix_mode == subpixel::SubpixelMode::Bgr;
        let (width, height, xmin, ymin, mut b3) =
            subpixel::subpixel_glyph_raster(&bytes, index, gid, self.px, hint.as_deref(), bgr)?;
        stem_darken(&mut b3, &self.stem_lut);
        let ymin = ink_row_extent(height, |y| {
            b3.get(y * width * 3..(y + 1) * width * 3)
                .is_some_and(|row| row.iter().any(|&v| v > 0))
        })
        .and_then(|ink| seated_ymin(ink, height, ymin, self.baseline, self.cell_h))
        .unwrap_or(ymin);
        Some(SubpxImage {
            width,
            height,
            xmin,
            ymin,
            bytes: b3,
        })
    }

    fn fallback_mono_raster(
        &mut self,
        source: FaceId,
        ch: char,
    ) -> (usize, usize, i32, i32, f32, Vec<u8>) {
        #[allow(clippy::type_complexity)]
        let parts: Option<(Option<LazyFontdue>, crate::font::FaceBytes, u32, FaceNorm)> =
            match source {
                FaceId::Fallback => {
                    self.ensure_fallback();
                    self.fallback_pick
                        .get(&ch)
                        .and_then(|&i| self.fallback_chain.get(i))
                        .or_else(|| self.fallback_chain.first())
                        .map(|f| (Some(f.font.clone()), f.bytes.clone(), f.index, f.norm))
                }
                FaceId::SymbolFallback => {
                    let pick = self.symbol_chain_pick(ch);
                    pick.and_then(|i| self.symbol_chain.get(i))
                        .or_else(|| self.symbol_chain.first())
                        .map(|f| (Some(f.font.clone()), f.bytes.clone(), f.index, f.norm))
                }
                FaceId::RuntimeFallback => {
                    if self.runtime_fallback.parts_for(ch).is_none() {
                        let ctx = self.runtime_probe_ctx();
                        if self.admitted_sources_sealed {
                            let _ = self.runtime_fallback.resolve_embedded_only(ch, ctx);
                        } else {
                            let _ = self.runtime_fallback.resolve(ch, ctx);
                        }
                    }
                    self.runtime_fallback
                        .parts_for(ch)
                        .map(|(font, bytes, index, norm)| {
                            (font.map(LazyFontdue::ready), bytes, index, norm)
                        })
                }
                _ => None,
            };
        let Some((font, bytes, index, norm)) = parts else {
            let (m, b) = self.font.rasterize(ch, self.px);
            return (m.width, m.height, m.xmin, m.ymin, m.advance_width, b);
        };
        let scale = self.fallback_face_scale(norm);
        let eff_px = self.px * scale;
        #[cfg(target_os = "macos")]
        let ct = {
            let gid = aterm_font::Face::parse(&bytes, index)
                .ok()
                .and_then(|f| f.glyph_index(ch))
                .map(|g| g.0)
                .filter(|&g| g != 0);
            gid.and_then(|g| {
                let kp = bytes.as_ptr() as usize;
                let b = bytes.clone();
                self.ct_glyph(kp, b, index, g, eff_px, &[], 0)
            })
        };
        #[cfg(not(target_os = "macos"))]
        let ct: Option<(usize, usize, i32, i32, f32, Vec<u8>)> = {
            let _ = index;
            None
        };
        if let Some(t) = ct {
            t
        } else if let Some(f) = font.as_ref().and_then(|lazy| lazy.get(&bytes)) {
            let (m, b) = f.rasterize(ch, eff_px);
            (m.width, m.height, m.xmin, m.ymin, m.advance_width, b)
        } else {
            if font.as_ref().is_some_and(LazyFontdue::known_bad) {
                if source == FaceId::Fallback && self.retire_unparsable_fallback(&bytes) {
                    let _ = self.fallback_has(ch);
                    return self.fallback_mono_raster(source, ch);
                }
                if source == FaceId::SymbolFallback && self.retire_unparsable_symbol_face(&bytes) {
                    return self.fallback_mono_raster(source, ch);
                }
            }
            let (m, b) = self.font.rasterize(ch, self.px);
            (m.width, m.height, m.xmin, m.ymin, m.advance_width, b)
        }
    }

    fn retire_unparsable_fallback(&mut self, bytes: &crate::font::FaceBytes) -> bool {
        let Some(i) = self
            .fallback_chain
            .iter()
            .position(|f| f.bytes.ptr_eq(bytes))
        else {
            return false;
        };
        self.fallback_chain.remove(i);
        self.fallback_pick.clear();
        self.keys.clear();
        self.clear_glyph_images();
        self.clear_face_address_caches();
        self.font_epoch += 1;
        true
    }

    fn retire_unparsable_symbol_face(&mut self, bytes: &crate::font::FaceBytes) -> bool {
        let Some(i) = self.symbol_chain.iter().position(|f| f.bytes.ptr_eq(bytes)) else {
            return false;
        };
        self.symbol_chain.remove(i);
        self.keys.clear();
        self.clear_glyph_images();
        self.clear_face_address_caches();
        self.font_epoch += 1;
        true
    }

    fn retire_unparsable_styled(&mut self, slot: usize) -> bool {
        if slot >= 3 {
            return false;
        }
        self.styled_retire_pending[slot] = None;
        if self.styled_faces[slot].is_none() {
            return false;
        }
        self.styled_faces[slot] = None;
        self.styled_keys.clear();
        self.keys.clear();
        self.clear_glyph_images();
        self.shaped_runs.clear();
        self.clear_face_address_caches();
        self.font_epoch += 1;
        true
    }

    fn apply_pending_styled_retires(&mut self) -> bool {
        if self.styled_retire_pending.iter().all(Option::is_none) {
            return false;
        }
        let mut retired = false;
        for slot in 0..3 {
            let Some(condemned) = self.styled_retire_pending[slot].take() else {
                continue;
            };
            if self.styled_faces[slot]
                .as_ref()
                .is_some_and(|face| face.id == condemned)
            {
                retired |= self.retire_unparsable_styled(slot);
            }
        }
        retired
    }

    fn clear_face_address_caches(&mut self) {
        #[cfg(target_os = "macos")]
        {
            self.ct_cache.clear();
            self.ct_descriptors.clear();
        }
        #[cfg(any(all(unix, not(target_os = "macos")), windows))]
        self.hint_bank.clear();
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            self.subpx_bank.clear();
            self.subpx_glyphs.clear();
        }
    }

    fn fallback_face_scale(&self, norm: FaceNorm) -> f32 {
        if let Some(ideo_em) = norm.ideo_adv_em {
            return fallback_cjk_scale((2 * self.cell_w) as f32, ideo_em * self.px);
        }
        if let (Some(p_em), Some(f_em)) = (self.primary_x_height_em(), norm.x_height_em) {
            return fallback_xheight_scale(p_em * self.px, f_em * self.px);
        }
        1.0
    }

    fn primary_x_height_em(&self) -> Option<f32> {
        let bytes = self.shaping_primary_bytes.as_ref()?;
        let face = aterm_font::Face::parse(bytes, self.primary_index).ok()?;
        let upem = f32::from(face.units_per_em());
        if upem <= 0.0 {
            return None;
        }
        x_height_em_of(&face, upem)
    }

    fn harmonize_fallback_raster(
        &self,
        cells: usize,
        raw: (usize, usize, i32, i32, f32, Vec<u8>),
        deferred: DeferredStyle,
        fit: FallbackFit,
    ) -> (usize, usize, i32, i32, f32, Vec<u8>) {
        let (mut w, mut h, mut xmin, mut ymin, mut adv, mut bytes) = raw;
        let cells = cells.min(2);
        if cells > 0 && w > 0 && h > 0 {
            (w, h, xmin, ymin, bytes) = crop_to_ink(w, h, xmin, ymin, bytes);
            if !adv.is_finite() || adv <= 0.0 {
                adv = w as f32;
            }
            let box_w = cells.saturating_mul(self.cell_w).max(1);
            let scale = match fit {
                FallbackFit::Text => fallback_fit_scale(box_w, self.cell_h, w, h, xmin, adv),
                FallbackFit::Symbol => fallback_ink_fit_scale(box_w, self.cell_h, w, h, xmin, adv),
            };
            if scale < 1.0 {
                let dst_w = ((w as f32 * scale).round() as usize).clamp(1, box_w);
                let dst_h = ((h as f32 * scale).round() as usize).clamp(1, self.cell_h.max(1));
                if let Some(scaled) = resample_mask_area(&bytes, w, h, dst_w, dst_h) {
                    bytes = scaled;
                    xmin = (xmin as f32 * scale).round() as i32;
                    ymin = match fit {
                        FallbackFit::Text => shrink_ymin_about_baseline(ymin, scale),
                        FallbackFit::Symbol => {
                            shrink_ymin_about_ink_centre(ymin, h, dst_h, self.baseline, self.cell_h)
                        }
                    };
                    w = dst_w;
                    h = dst_h;
                    adv *= scale;
                }
            }

            let box_w_i32 = i32::try_from(box_w).unwrap_or(i32::MAX);
            xmin = xmin.saturating_add(wide_center_offset(
                box_w_i32,
                adv.round().clamp(i32::MIN as f32, i32::MAX as f32) as i32,
            ));

            if deferred.bold_px > 0 {
                let (bold, nw) = embolden(&bytes, w, h, deferred.bold_px);
                let keep = nw.min(box_w.max(w));
                if let Some(bold) = crop_mask_columns(&bold, nw, h, 0, keep) {
                    let half = i32::try_from((keep - w) / 2).unwrap_or(0);
                    bytes = bold;
                    w = keep;
                    let max_x = i32::try_from(box_w.saturating_sub(w)).unwrap_or(i32::MAX);
                    xmin = xmin.saturating_sub(half).min(max_x).max(0);
                }
            }

            if deferred.italic {
                let (sheared, nw) = slant(&bytes, w, h, SYNTHETIC_ITALIC_SHEAR);
                let fit_w = nw.min(box_w.max(w));
                let fitted = if fit_w < nw {
                    resample_mask_area(&sheared, nw, h, fit_w, h)
                } else {
                    Some(sheared)
                };
                if let Some(fitted) = fitted {
                    bytes = fitted;
                    w = fit_w;
                    let max_x = i32::try_from(box_w.saturating_sub(w)).unwrap_or(i32::MAX);
                    xmin = xmin.min(max_x).max(0);
                }
            }

            let (skip_x, new_w) = clamp_to_col_band(xmin, w, box_w);
            if (skip_x != 0 || new_w != w)
                && let Some(cropped) = crop_mask_columns(&bytes, w, h, skip_x, new_w)
            {
                bytes = cropped;
                xmin = xmin.saturating_add(i32::try_from(skip_x).unwrap_or(i32::MAX));
                w = new_w;
            }
            adv = box_w as f32;
        }

        let top = self.baseline - h as i32 - ymin;
        let (skip, new_h) = clamp_to_row_band(top, h, self.cell_h);
        if new_h != h {
            let bottom_trim = h - skip - new_h;
            bytes = bytes[skip * w..(skip + new_h) * w].to_vec();
            ymin += bottom_trim as i32;
            h = new_h;
        }
        (w, h, xmin, ymin, adv, bytes)
    }

    fn fill_styled_from_collection(&mut self, bytes: &std::sync::Arc<[u8]>, primary_index: u32) {
        let Some(n) = aterm_font::fonts_in_collection(bytes) else {
            return;
        };
        let family = |face: &aterm_font::Face<'_>| {
            use aterm_font::name_id::{FAMILY, TYPOGRAPHIC_FAMILY};
            [TYPOGRAPHIC_FAMILY, FAMILY].into_iter().find_map(|id| {
                face.names().into_iter().find_map(|name| {
                    (name.name_id == id)
                        .then(|| name_record_text(&name))
                        .flatten()
                        .map(|text| normalize_family(&text))
                })
            })
        };
        let Some(primary_family) = aterm_font::Face::parse(bytes, primary_index)
            .ok()
            .and_then(|face| family(&face))
        else {
            return;
        };
        for (slot, styled_face) in self.styled_faces.iter_mut().enumerate() {
            if styled_face.is_some() {
                continue;
            }
            let target = FaceTarget::for_slot(slot);
            let mut best: Option<(u32, u32)> = None;
            for i in (0..n).filter(|&i| i != primary_index) {
                let Ok(face) = aterm_font::Face::parse(bytes, i) else {
                    continue;
                };
                if !target.suits(&face) || family(&face).as_deref() != Some(&primary_family) {
                    continue;
                }
                let distance = target.distance(&face);
                if best.is_none_or(|(held, _)| distance < held) {
                    best = Some((distance, i));
                }
            }
            if let Some((_, i)) = best
                && face_admissible(bytes, i)
            {
                *styled_face = Some(StyledFace {
                    font: LazyFontdue::deferred(),
                    bytes: bytes.clone(),
                    index: i,
                    gid_cache: FxHashMap::default(),
                    id: next_styled_face_id(),
                    yields_private_use: bundled::yields_private_use(bytes),
                    lowered_asterisk: bundled::lowered_asterisk(bytes),
                    named: None,
                    instance: FaceInstance::default(),
                });
            }
        }
    }

    fn styled_face_slot(&self, ch: char, style: StyleBits) -> Option<(usize, StyleBits)> {
        let pick = resolve_styled_face_lazy(style, false, |slot| self.styled_face_covers(slot, ch));
        match pick {
            FacePick::Styled { slot, synthetic } => Some((slot, synthetic)),
            FacePick::Primary | FacePick::InjectedBold { .. } => None,
        }
    }

    fn run_face_pick(&self, style: StyleBits) -> FacePick {
        resolve_styled_face(
            style,
            self.bold_font.is_some(),
            std::array::from_fn(|i| self.styled_faces[i].is_some()),
        )
    }

    fn load_next_color_face(&mut self) -> bool {
        while !self.color_font_paths.is_empty() {
            if self.color_chain.len() >= MAX_COLOR_FACES {
                self.color_font_paths.clear();
                return false;
            }
            let builtin = self.color_font_paths.len() <= builtin_color_emoji_candidates().count();
            let path = self.color_font_paths.remove(0);
            let relocated =
                relocate_missing_builtin_fonts(std::slice::from_ref(&path), usize::from(!builtin))
                    .pop()
                    .flatten();
            let load = relocated.as_deref().unwrap_or(&path);
            let Ok(bytes) = font_file::admit_font_file(std::path::Path::new(load)) else {
                continue;
            };
            if aterm_font::Face::parse(&bytes, 0).is_err() {
                continue;
            }
            let bytes = intern_discovered_font_bytes(&bytes);
            if self.color_chain.iter().any(|f| f.bytes.same_bytes(&bytes)) {
                continue;
            }
            self.color_chain.push(ColorFace {
                bytes,
                injected: false,
            });
            return true;
        }
        false
    }

    fn ensure_color_font(&mut self) {
        if self.color_chain.is_empty() {
            self.load_next_color_face();
        }
    }

    fn ensure_color_chain(&mut self) {
        while self.load_next_color_face() {}
    }

    fn color_pick(&mut self, ch: char) -> Option<u8> {
        let mut i = 0;
        loop {
            if i == self.color_chain.len() && !self.load_next_color_face() {
                return None;
            }
            if color_face_draws(&self.color_chain[i].bytes, ch) {
                return u8::try_from(i).ok();
            }
            i += 1;
        }
    }

    fn color_font_has(&mut self, ch: char) -> bool {
        self.color_pick(ch).is_some()
    }

    fn color_face_bytes(&self, face: u8) -> Option<&crate::font::FaceBytes> {
        self.color_chain
            .get(usize::from(face))
            .map(|face| &face.bytes)
    }

    #[inline]
    pub const fn cell_size(&self) -> (usize, usize) {
        (self.cell_w, self.cell_h)
    }

    /// The default fg/bg/cursor/selection theme frames paint with (the last [`set_theme`](Self::set_theme), else the constructor's).
    #[inline]
    #[must_use]
    pub const fn theme(&self) -> Theme {
        self.theme
    }

    /// Replace the default fg/bg/cursor/selection theme live (host theme change), so a pane re-themes without being rebuilt.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        if self.font_weight_dark_nudge > 0.0 {
            let _ = self.refresh_variations();
        }
    }

    fn geometry_metrics(&self, px: f32) -> Option<(f32, f32, f32)> {
        self.geometry_metrics_at_coords(px, self.geom_coords.as_deref())
    }

    fn geometry_metrics_at_coords(
        &self,
        px: f32,
        coords: Option<&[(u32, f32)]>,
    ) -> Option<(f32, f32, f32)> {
        if let Some(m) = coords.and_then(|coords| {
            variation::varied_metrics_px(
                self.shaping_primary_bytes.as_deref()?,
                self.primary_index,
                coords,
                px,
            )
        }) {
            return Some((m.ascent, self.floored_descent(m.descent, px), m.line_gap));
        }
        if let Some(t) = self.typo_lm {
            return Some((
                t.ascent * px,
                self.floored_descent(t.descent * px, px),
                t.line_gap * px,
            ));
        }
        self.font
            .horizontal_line_metrics(px)
            .map(|lm| (lm.ascent, self.floored_descent(lm.descent, px), lm.line_gap))
    }

    fn floored_descent(&self, descent: f32, px: f32) -> f32 {
        self.letter_descent.map_or(descent, |letter| {
            letter_floored_descent(descent, letter, px)
        })
    }

    fn varied_metrics(&self, px: f32) -> Option<variation::VariedMetricsPx> {
        let coords = self.geom_coords.as_deref()?;
        let bytes = self.shaping_primary_bytes.as_deref()?;
        variation::varied_metrics_px(bytes, self.primary_index, coords, px)
    }

    fn vf_primary_gid_raster(&self, gid: u16) -> Option<(usize, usize, i32, i32, f32, Vec<u8>)> {
        let coords = self.var_coords.as_deref()?;
        let bytes = self.shaping_primary_bytes.as_deref()?;
        variation::varied_glyph_raster(bytes, self.primary_index, coords, gid, self.px)
    }

    fn vf_bold_gid_raster(&self, gid: u16) -> Option<(usize, usize, i32, i32, f32, Vec<u8>)> {
        let coords = self.vf_bold_coords.as_deref()?;
        let bytes = self.shaping_primary_bytes.as_deref()?;
        variation::varied_glyph_raster(bytes, self.primary_index, coords, gid, self.px)
    }

    fn vf_primary_char_raster(&self, ch: char) -> Option<(usize, usize, i32, i32, f32, Vec<u8>)> {
        let coords = self.var_coords.as_deref()?;
        let bytes = self.shaping_primary_bytes.as_deref()?;
        let mut face = aterm_font::Face::parse(bytes, self.primary_index).ok()?;
        for &(tag, value) in coords {
            let _ = face.set_variation(aterm_font::Tag(tag), value);
        }
        let gid = face.glyph_index(ch)?.0;
        if gid == 0 {
            // Gid 0 is `.notdef`: the caller's plain raster draws it.
            return None;
        }
        variation::varied_glyph_raster_with_face(&face, gid, self.px)
    }

    fn primary_m_advance_px(&self, px: f32) -> f32 {
        if let Some(m) = self.varied_metrics(px) {
            return m.m_advance;
        }
        self.font.metrics('M', px).advance_width
    }

    /// The resolved primary variation coords, `None` for an unvaried primary.
    #[inline]
    #[must_use]
    pub fn variation_coords(&self) -> Option<&[(u32, f32)]> {
        self.var_coords.as_deref()
    }

    /// Test/debug: the bold-instance coords, when the primary's `wght` axis supports a real bold.
    #[doc(hidden)]
    #[must_use]
    pub fn debug_vf_bold_coords(&self) -> Option<&[(u32, f32)]> {
        self.vf_bold_coords.as_deref()
    }

    /// Test/debug: the resident shaping-variation cache mapped back to `(tag, value)`, so tests can pin that it stays in lock-step with [`Self::variation_coords`] (the shaping path borrows this cache instead of rebuilding a `Vec` per row per frame).
    #[doc(hidden)]
    #[must_use]
    pub fn debug_shaping_variations(&self) -> Vec<(u32, f32)> {
        self.shaping_variations
            .iter()
            .map(|&(tag, value)| (tag.0, value))
            .collect()
    }

    fn compute_shaping_variations(&self) -> Vec<(aterm_font::Tag, f32)> {
        self.var_coords
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(|&(tag, value)| (aterm_font::Tag(tag), value))
            .collect()
    }

    /// Install the font variation config.
    pub fn set_font_variations(&mut self, requests: &[(u32, f32)], dark_nudge: f32) -> bool {
        let dark_nudge = if dark_nudge.is_finite() {
            dark_nudge.clamp(0.0, 500.0)
        } else {
            0.0
        };
        if self.cfg_font_variations == requests
            && (self.font_weight_dark_nudge - dark_nudge).abs() < f32::EPSILON
        {
            return false;
        }
        let resolved = self.compute_variations_for(requests, dark_nudge);
        let Some(geometry) = self.derive_geometry_at_coords(
            self.px,
            resolved.geometry.as_deref(),
            self.line_height_scale,
        ) else {
            return false;
        };
        self.cfg_font_variations = requests.to_vec();
        self.font_weight_dark_nudge = dark_nudge;
        self.install_variations(resolved, geometry)
    }

    /// The last-applied variation requests ([`Self::set_font_variations`]), which [`Self::rebuild_from_admitted`] carries so a zoom/theme rebuild keeps the user's instance.
    #[inline]
    #[must_use]
    pub fn font_variation_requests(&self) -> &[(u32, f32)] {
        &self.cfg_font_variations
    }

    /// The active `font_weight_dark_nudge` (0 = off).
    #[inline]
    #[must_use]
    pub const fn font_weight_dark_nudge(&self) -> f32 {
        self.font_weight_dark_nudge
    }

    fn refresh_variations(&mut self) -> Option<bool> {
        let resolved = self.compute_variations();
        let geometry = if self.geom_coords.as_deref() == resolved.geometry.as_deref() {
            (self.cell_w, self.cell_h, self.baseline)
        } else {
            self.derive_geometry_at_coords(
                self.px,
                resolved.geometry.as_deref(),
                self.line_height_scale,
            )?
        };
        Some(self.install_variations(resolved, geometry))
    }

    fn install_variations(&mut self, resolved: ResolvedInstances, geometry: CellGeometry) -> bool {
        let geometry_moved = self.geom_coords.as_deref() != resolved.geometry.as_deref();
        // A weight change moves every style together, a static primary's variable
        // italic too.
        let family_moved = self.refresh_family_instances();
        if !geometry_moved
            && !family_moved
            && self.var_coords.as_deref() == resolved.drawn.as_deref()
            && self.vf_bold_coords.as_deref() == resolved.bold.as_deref()
        {
            return false;
        }
        self.var_coords = resolved.drawn;
        self.geom_coords = resolved.geometry;
        self.vf_bold_coords = resolved.bold;
        self.shaping_variations = self.compute_shaping_variations();
        if geometry_moved {
            self.forget_cell_geometry();
            (self.cell_w, self.cell_h, self.baseline) = geometry;
        }
        self.clear_glyph_images();
        self.keys.clear();
        self.emoji_keys.clear();
        self.styled_keys.clear();
        self.cluster_shapes.clear();
        self.shaped_runs.clear();
        self.fallback_pick.clear();
        self.deco_masks.borrow_mut().clear();
        self.undercurl_masks.borrow_mut().clear();
        #[cfg(target_os = "macos")]
        self.ct_cache.clear();
        #[cfg(any(all(unix, not(target_os = "macos")), windows))]
        self.hint_bank.clear();
        #[cfg(all(unix, not(target_os = "macos")))]
        self.subpx_bank.clear();
        self.refresh_drawn_faces();
        true
    }

    fn compute_variations(&self) -> ResolvedInstances {
        self.compute_variations_for(&self.cfg_font_variations, self.font_weight_dark_nudge)
    }

    fn compute_variations_for(
        &self,
        requests: &[(u32, f32)],
        dark_nudge: f32,
    ) -> ResolvedInstances {
        self.compute_variations_for_face(
            self.shaping_primary_bytes
                .as_deref()
                .map(|bytes| (bytes, self.primary_index, self.primary_named.as_ref())),
            requests,
            dark_nudge,
        )
    }

    /// The instances face `index` of `bytes` draws in the regular role: its regular
    /// instance under `requests` and the dark-theme nudge, or exactly the named instance
    /// its font name chose ([`NamedInstance`]), which neither moves.
    fn compute_variations_for_face(
        &self,
        face: Option<(&[u8], u32, Option<&NamedInstance>)>,
        requests: &[(u32, f32)],
        dark_nudge: f32,
    ) -> ResolvedInstances {
        let unvaried = ResolvedInstances {
            drawn: None,
            geometry: None,
            bold: None,
        };
        let Some((bytes, index, named)) = face else {
            return unvaried;
        };
        let Some(probe) = variation::probe(bytes, index) else {
            return unvaried;
        };
        let axes = &probe.axes;
        let (base, requests, dark_nudge) = match named.and_then(|named| named.paired(axes)) {
            Some(named) => (named, &[][..], 0.0),
            None => (
                variation::resolve_default_coords(axes, probe.regular_coords.as_deref()),
                requests,
                dark_nudge,
            ),
        };
        let kept = self.users_axes(requests);
        let base = variation::apply_requests(axes, base, requests);
        let pair = |coords: &[f32]| -> Vec<(u32, f32)> {
            axes.iter().zip(coords).map(|(a, &v)| (a.tag, v)).collect()
        };
        let instance = |coords: &[f32]| -> VarCoords {
            let all_default = axes
                .iter()
                .zip(coords)
                .all(|(a, &v)| (v - a.def).abs() < 1e-4);
            (!all_default).then(|| pair(coords).into())
        };
        let geometry = instance(&base);
        let wght_idx = axes.iter().position(|a| a.tag == variation::WGHT_TAG);
        let coords = 'nudge: {
            let Some(wi) = wght_idx else {
                break 'nudge base;
            };
            if dark_nudge <= 0.0 || !theme_is_dark(self.theme.bg) {
                break 'nudge base;
            }
            let nudged = variation::apply_requests(
                axes,
                base.clone(),
                &[(variation::WGHT_TAG, base[wi] + dark_nudge)],
            );
            let defaults: Vec<(u32, f32)> = axes.iter().map(|a| (a.tag, a.def)).collect();
            let adv_nudged = variation::varied_metrics_px(bytes, index, &pair(&nudged), self.px)
                .map_or(f32::NAN, |m| m.m_advance);
            let adv_default = variation::varied_metrics_px(bytes, index, &defaults, self.px)
                .map_or(f32::NAN, |m| m.m_advance);
            if !variation::dark_nudge_permitted(adv_nudged, adv_default) {
                break 'nudge base;
            }
            let units =
                |coords: &[f32]| variation::varied_metrics_units(bytes, index, &pair(coords));
            match (units(&base), units(&nudged)) {
                (Some(u0), Some(u1)) if u0 == u1 => nudged,
                _ => base,
            }
        };
        let var_coords = instance(&coords);
        // The face's designed Bold ([`variation::designed_bold_coords`]), else `wght` 700 where
        // that is a weight class above what the regular text draws: else bold is synthetic.
        let vf_bold: VarCoords = wght_idx.and_then(|wi| {
            if let Some(bold) = variation::designed_bold_coords(&probe, &coords, &kept) {
                return Some(pair(&bold).into());
            }
            let bold_w = variation::clamp_axis(&axes[wi], variation::BOLD_WGHT);
            if bold_w >= coords[wi] + variation::BOLD_STEP {
                let bold = variation::apply_requests(
                    axes,
                    coords.clone(),
                    &[(variation::WGHT_TAG, bold_w)],
                );
                Some(pair(&bold).into())
            } else {
                None
            }
        });
        ResolvedInstances {
            drawn: var_coords,
            geometry,
            bold: vf_bold,
        }
    }

    /// The instance face `index` of `bytes` draws at in `role`, under the live
    /// requests and dark-theme nudge: the primary's policy over that face's own `fvar`,
    /// or the named instance its font name chose ([`NamedInstance`]).
    fn family_face_instance(
        &self,
        bytes: &[u8],
        index: u32,
        named: Option<&NamedInstance>,
        role: FaceRole,
    ) -> FaceInstance {
        let Some(probe) = variation::probe(bytes, index) else {
            return FaceInstance::default();
        };
        let axes = &probe.axes;
        let instance = |coords: &[f32]| -> VarCoords {
            let all_default = axes
                .iter()
                .zip(coords)
                .all(|(axis, &value)| (value - axis.def).abs() < 1e-4);
            (!all_default).then(|| {
                axes.iter()
                    .zip(coords)
                    .map(|(axis, &value)| (axis.tag, value))
                    .collect()
            })
        };
        let (requests, dark_nudge) = self.role_requests();
        match (role, named.and_then(|named| named.paired(axes))) {
            (FaceRole::Regular, _) => {
                let resolved = self.compute_variations_for_face(
                    Some((bytes, index, named)),
                    &requests,
                    dark_nudge,
                );
                FaceInstance {
                    drawn: resolved.drawn,
                    bold: resolved.bold,
                }
            }
            // A bold face named at its instance draws that instance, its own bold.
            (FaceRole::Bold, Some(named)) => FaceInstance {
                drawn: instance(&named),
                bold: None,
            },
            (FaceRole::Bold, None) => {
                let base = variation::resolve_default_coords(axes, probe.regular_coords.as_deref());
                let base = variation::apply_requests(axes, base, &requests);
                let kept = self.users_axes(&requests);
                FaceInstance {
                    drawn: instance(&variation::bold_role_coords(&probe, &base, &kept)),
                    // A bold face serves the bold itself.
                    bold: None,
                }
            }
        }
    }

    /// The axes `requests` set that are the user's own (`font_variation`), which a bold role
    /// keeps ([`variation::bold_role_coords`]): none of `wght`, and none where the primary's
    /// font name chose its instance, whose pins ([`Self::role_requests`]) are its design.
    fn users_axes(&self, requests: &[(u32, f32)]) -> Vec<u32> {
        if self.primary_named.is_some() {
            return Vec::new();
        }
        requests
            .iter()
            .map(|&(tag, _)| tag)
            .filter(|&tag| tag != variation::WGHT_TAG)
            .collect()
    }

    /// What a styled face its role weighs ([`FaceRole`]) is drawn under: the user's
    /// variation requests and dark-theme nudge, or, where the primary's font name chose
    /// its instance (`SF Mono Light`, [`NamedInstance`]), that instance on every axis and
    /// no nudge, as the primary draws it: the regular text's weight is the role's, as a
    /// static `JetBrainsMono-Light` finds its `LightItalic` sibling.
    fn role_requests(&self) -> (Vec<(u32, f32)>, f32) {
        if let Some(named) = &self.primary_named
            && let Some(bytes) = self.shaping_primary_bytes.as_deref()
            && let Some(probe) = variation::probe(bytes, self.primary_index)
            && let Some(coords) = named.paired(&probe.axes)
        {
            let pinned = probe
                .axes
                .iter()
                .zip(coords)
                .map(|(axis, value)| (axis.tag, value))
                .collect();
            return (pinned, 0.0);
        }
        (
            self.cfg_font_variations.clone(),
            self.font_weight_dark_nudge,
        )
    }

    /// Re-resolve every styled slot's and the injected bold's instance; whether any
    /// moved (the caller drops the caches that baked the old one).
    fn refresh_family_instances(&mut self) -> bool {
        let mut moved = false;
        for slot in 0..3 {
            let Some((bytes, index, named)) = self.styled_faces[slot]
                .as_ref()
                .map(|face| (face.bytes.clone(), face.index, face.named.clone()))
            else {
                continue;
            };
            let instance =
                self.family_face_instance(&bytes, index, named.as_ref(), FaceRole::of_slot(slot));
            if let Some(face) = self.styled_faces[slot].as_mut()
                && face.instance != instance
            {
                face.instance = instance;
                moved = true;
            }
        }
        let bold = self
            .bold_font_bytes
            .clone()
            .map(|bytes| {
                self.family_face_instance(
                    &bytes,
                    self.bold_font_index,
                    self.bold_font_named.as_ref(),
                    FaceRole::Bold,
                )
            })
            .unwrap_or_default();
        if self.bold_font_instance != bold {
            self.bold_font_instance = bold;
            moved = true;
        }
        moved
    }

    /// `(coords, var_slot, residual)` styled slot `slot` draws a cell at that leaves
    /// `residual` to synthesize: its bold instance where the cell still wants bold
    /// (BOLD then leaves the residual), else its drawn instance. `var_slot` tells the
    /// raster caches one face's instances apart (0 as authored, 1 drawn, 2 bold).
    fn styled_instance(&self, slot: usize, residual: StyleBits) -> (VarCoords, u8, StyleBits) {
        let Some(face) = self.styled_faces.get(slot).and_then(Option::as_ref) else {
            return (None, 0, residual);
        };
        if residual.contains(StyleBits::BOLD)
            && let Some(bold) = face.instance.bold.clone()
        {
            return (Some(bold), 2, StyleBits(residual.0 & !StyleBits::BOLD.0));
        }
        match face.instance.drawn.clone() {
            Some(drawn) => (Some(drawn), 1, residual),
            None => (None, 0, residual),
        }
    }

    fn derive_cell_geometry(&self, px: f32) -> Option<(usize, i32)> {
        if !px.is_finite() || px <= 0.0 || !self.line_height_scale.is_finite() {
            return None;
        }
        let (asc, desc, gap) = self.geometry_metrics(px)?;
        checked_cell_h_baseline(asc, desc, gap, self.line_height_scale, self.baseline_adjust)
    }

    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    #[must_use]
    pub fn debug_geometry_metrics(&self) -> Option<(f32, f32, f32)> {
        self.geometry_metrics(self.px)
    }

    /// Baseline escape hatch (config `adjust_baseline`).
    pub fn set_adjust_baseline(&mut self, px: i32) {
        let px = px.clamp(-64, 64);
        if px == self.baseline_adjust {
            return;
        }
        let previous = self.baseline_adjust;
        self.baseline_adjust = px;
        let Some((cell_h, baseline)) = self.derive_cell_geometry(self.px) else {
            self.baseline_adjust = previous;
            return;
        };
        self.forget_cell_geometry();
        self.cell_h = cell_h;
        self.baseline = baseline;
        self.clear_glyph_images();
    }

    /// The current baseline adjustment (see [`Self::set_adjust_baseline`]).
    #[inline]
    #[must_use]
    pub const fn adjust_baseline(&self) -> i32 {
        self.baseline_adjust
    }

    /// The resolved decoration bands for the current cell geometry.
    #[inline]
    #[must_use]
    pub fn deco_metrics(&self) -> deco::DecoMetrics {
        deco::resolve_deco_metrics(
            self.cell_h,
            self.baseline,
            self.px,
            self.deco_tables,
            self.underline_adjust_pos,
            self.underline_adjust_thick,
        )
    }

    /// [`Self::deco_metrics`] for an arbitrary font size.
    #[inline]
    #[must_use]
    pub fn deco_metrics_for(&self, px: f32) -> deco::DecoMetrics {
        let ((_, cell_h, baseline), px) = self.cell_geometry_and_px(px);
        deco::resolve_deco_metrics(
            cell_h,
            baseline,
            px,
            self.deco_tables,
            self.underline_adjust_pos,
            self.underline_adjust_thick,
        )
    }

    /// Underline escape hatches (config `adjust_underline_position` / `adjust_underline_thickness`).
    pub fn set_adjust_underline(&mut self, pos_px: i32, thick_px: i32) {
        let (pos, thick) = (pos_px.clamp(-64, 64), thick_px.clamp(-64, 64));
        if pos == self.underline_adjust_pos && thick == self.underline_adjust_thick {
            return;
        }
        self.underline_adjust_pos = pos;
        self.underline_adjust_thick = thick;
        self.undercurl_masks.borrow_mut().clear();
    }

    /// Enable or disable descender ink-skip (config `underline_skip_descenders`, default on).
    #[inline]
    pub fn set_underline_skip_descenders(&mut self, on: bool) {
        self.underline_skip_descenders = on;
    }

    /// Whether descender ink-skip is on (see [`Self::set_underline_skip_descenders`]).
    #[inline]
    #[must_use]
    pub const fn underline_skip_descenders(&self) -> bool {
        self.underline_skip_descenders
    }

    /// Fetch (or rasterize and cache) the antialiased undercurl tile for the current base cell box and resolved underline band ([`deco::undercurl_coverage`]).
    #[must_use]
    pub fn undercurl_mask(&self) -> std::sync::Arc<Vec<u8>> {
        let d = self.deco_metrics();
        let key = (self.cell_w, self.cell_h, d.underline_y, d.underline_t);
        let mut cache = self.undercurl_masks.borrow_mut();
        if let Some((_, m)) = cache.iter().find(|(k, _)| *k == key) {
            return m.clone();
        }
        let m = std::sync::Arc::new(deco::undercurl_coverage(self.cell_w, self.cell_h, d));
        cache.push((key, m.clone()));
        m
    }

    /// Re-rasterize at a new pixel size.
    pub fn set_px(&mut self, px: f32) {
        if font_px_unchanged(self.px, px) {
            return;
        }
        let Some((cell_h, baseline)) = self.derive_cell_geometry(px) else {
            return;
        };
        let cell_w = match checked_cell_w_from_advance(self.primary_m_advance_px(px)) {
            Some(width) => width,
            None => return,
        };
        self.px = px;
        self.px_q = GlyphKey::quantize_px(px);
        self.cell_w = cell_w;
        self.cell_h = cell_h;
        self.baseline = baseline;
        self.clear_glyph_images();
        self.keys.clear();
        self.emoji_keys.clear();
        self.styled_keys.clear();
        self.cluster_shapes.clear();
        self.shaped_runs.clear();
        self.fallback_pick.clear();
        self.deco_masks.borrow_mut().clear();
        self.undercurl_masks.borrow_mut().clear();
        #[cfg(target_os = "macos")]
        self.ct_cache.clear();
        #[cfg(any(all(unix, not(target_os = "macos")), windows))]
        self.hint_bank.clear();
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            self.subpx_bank.clear();
            self.subpx_glyphs.clear();
        }
    }

    /// Light per-window size switch (mixed DPI).
    pub fn activate_px(&mut self, px: f32) {
        if font_px_unchanged(self.px, px) {
            return;
        }
        let Some((cell_h, baseline)) = self.derive_cell_geometry(px) else {
            return;
        };
        let cell_w = match checked_cell_w_from_advance(self.primary_m_advance_px(px)) {
            Some(width) => width,
            None => return,
        };
        self.px = px;
        self.px_q = GlyphKey::quantize_px(px);
        self.cell_w = cell_w;
        self.cell_h = cell_h;
        self.baseline = baseline;
        self.keys.clear();
        self.emoji_keys.clear();
        self.styled_keys.clear();
    }

    /// Pure per-window cell geometry at `px`.
    #[must_use]
    pub fn cell_geometry(&self, px: f32) -> (usize, usize, i32) {
        self.cell_geometry_and_px(px).0
    }

    fn cell_geometry_and_px(&self, px: f32) -> (CellGeometry, f32) {
        if px != self.px && font_px_unchanged(self.px, px) {
            return ((self.cell_w, self.cell_h, self.baseline), self.px);
        }
        let memo = self.cell_geometry_memo.get();
        let key = px.to_bits();
        if let Some(geometry) = memo.get(key) {
            return (geometry, px);
        }
        match self.derive_window_cell_geometry(px) {
            Some(geometry) => {
                self.cell_geometry_memo.set(memo.with(key, geometry));
                (geometry, px)
            }
            None => ((self.cell_w, self.cell_h, self.baseline), self.px),
        }
    }

    fn forget_cell_geometry(&mut self) {
        *self.cell_geometry_memo.get_mut() = CellGeometryMemo::default();
    }

    fn derive_window_cell_geometry(&self, px: f32) -> Option<CellGeometry> {
        #[cfg(any(test, debug_assertions))]
        crate::test_counters::count_cell_geometry_derivation();
        self.derive_geometry_at_coords(px, self.geom_coords.as_deref(), self.line_height_scale)
    }

    fn derive_geometry_at_coords(
        &self,
        px: f32,
        coords: Option<&[(u32, f32)]>,
        line_height: f32,
    ) -> Option<CellGeometry> {
        let metrics = self.cell_metrics_at_coords(px, coords)?;
        checked_cell_geometry(&metrics, line_height, self.baseline_adjust)
    }

    fn cell_metrics_at_coords(
        &self,
        px: f32,
        coords: Option<&[(u32, f32)]>,
    ) -> Option<variation::VariedMetricsPx> {
        if !px.is_finite() || px <= 0.0 {
            return None;
        }
        if let Some(mut metrics) = coords.and_then(|coords| {
            variation::varied_metrics_px(
                self.shaping_primary_bytes.as_deref()?,
                self.primary_index,
                coords,
                px,
            )
        }) {
            metrics.descent = self.floored_descent(metrics.descent, px);
            return Some(metrics);
        }
        let (ascent, descent, line_gap) = self.geometry_metrics_at_coords(px, None)?;
        Some(variation::VariedMetricsPx {
            ascent,
            descent,
            line_gap,
            m_advance: self.font.metrics('M', px).advance_width,
        })
    }

    /// The row height a semantic specimen will paint with its candidate line height and variation requests.
    #[must_use]
    pub fn specimen_cell_height(
        &self,
        px: f32,
        line_height: f32,
        requests: &[(u32, f32)],
    ) -> usize {
        self.specimen_cell_size(px, line_height, requests).1
    }

    /// The `(width, height)` cell a semantic specimen will paint with its candidate line height and variation requests.
    #[must_use]
    pub fn specimen_cell_size(
        &self,
        px: f32,
        line_height: f32,
        requests: &[(u32, f32)],
    ) -> (usize, usize) {
        let inherited = self.cell_metrics_at_coords(px, self.geom_coords.as_deref());
        let defaults = inherited
            .as_ref()
            .and_then(|metrics| checked_cell_geometry(metrics, 1.0, 0))
            .is_none()
            .then(|| self.compute_variations_for(&[], 0.0));
        let coords = defaults
            .as_ref()
            .map_or(self.geom_coords.as_deref(), |resolved| {
                resolved.geometry.as_deref()
            });
        let metrics = if defaults.is_some() {
            self.cell_metrics_at_coords(px, coords)
        } else {
            inherited
        };
        let Some(metrics) = metrics else {
            return (self.cell_w, self.cell_h);
        };
        let Some(mut geometry) = checked_cell_geometry(&metrics, 1.0, 0) else {
            return (self.cell_w, self.cell_h);
        };
        let mut scale = 1.0;
        if let Some(inherited_scale) = requested_line_height(scale, self.line_height_scale)
            && let Some(admitted) = checked_cell_geometry(&metrics, inherited_scale, 0)
        {
            scale = inherited_scale;
            geometry = admitted;
        }
        let mut adjustment = 0;
        if let Some(admitted) = checked_cell_geometry(&metrics, scale, self.baseline_adjust) {
            adjustment = self.baseline_adjust;
            geometry = admitted;
        }
        if let Some(candidate) = requested_line_height(scale, line_height)
            && let Some(admitted) = checked_cell_geometry(&metrics, candidate, adjustment)
        {
            scale = candidate;
            geometry = admitted;
        }
        if self.cfg_font_variations == requests && coords == self.geom_coords.as_deref() {
            return (geometry.0, geometry.1);
        }
        let resolved = self.compute_variations_for(requests, 0.0);
        if resolved.geometry.as_deref() == coords {
            return (geometry.0, geometry.1);
        }
        self.cell_metrics_at_coords(px, resolved.geometry.as_deref())
            .and_then(|metrics| checked_cell_geometry(&metrics, scale, adjustment))
            .map_or((geometry.0, geometry.1), |geometry| {
                (geometry.0, geometry.1)
            })
    }

    fn display_layer_face(&self, ch: char) -> Option<&DisplayLayerFace> {
        self.display_layer
            .get(display_mix_face_index(ch, self.display_layer.len()))
    }

    fn display_layer_draws(&self, ch: char) -> bool {
        !self.display_layer.is_empty()
            && !procedural::covers(ch)
            && self.display_layer_face(ch).is_some_and(|layer| {
                layer
                    .face
                    .font
                    .get(&layer.face.bytes)
                    .is_some_and(|f| f.lookup_glyph_index(ch) != 0)
            })
    }

    #[allow(clippy::type_complexity)]
    fn display_layer_raw_raster(
        &mut self,
        ch: char,
    ) -> (
        Option<DisplayFaceFit>,
        (usize, usize, i32, i32, f32, Vec<u8>),
    ) {
        let Some(layer) = self.display_layer_face(ch) else {
            let (m, b) = self.font.rasterize(ch, self.px);
            return (
                None,
                (m.width, m.height, m.xmin, m.ymin, m.advance_width, b),
            );
        };
        let fit = layer.fit;
        let (font, bytes, index) = (
            layer.face.font.clone(),
            layer.face.bytes.clone(),
            layer.face.index,
        );
        let eff_px = self.px * display_face_scale(&fit, self.px, self.cell_w, self.cell_h);
        #[cfg(target_os = "macos")]
        let ct = {
            let gid = aterm_font::Face::parse(&bytes, index)
                .ok()
                .and_then(|f| f.glyph_index(ch))
                .map(|g| g.0)
                .filter(|&g| g != 0);
            gid.and_then(|g| {
                let kp = bytes.as_ptr() as usize;
                self.ct_glyph(kp, bytes.clone(), index, g, eff_px, &[], 0)
            })
        };
        #[cfg(not(target_os = "macos"))]
        let ct: Option<(usize, usize, i32, i32, f32, Vec<u8>)> = {
            let _ = index;
            None
        };
        let raw = if let Some(t) = ct {
            t
        } else if let Some(f) = font.get(&bytes) {
            let (m, b) = f.rasterize(ch, eff_px);
            (m.width, m.height, m.xmin, m.ymin, m.advance_width, b)
        } else {
            let (m, b) = self.font.rasterize(ch, self.px);
            return (
                None,
                (m.width, m.height, m.xmin, m.ymin, m.advance_width, b),
            );
        };
        (Some(fit), raw)
    }

    fn fit_display_glyph(
        &self,
        fit: DisplayFaceFit,
        cells: usize,
        raw: (usize, usize, i32, i32, f32, Vec<u8>),
    ) -> (usize, usize, i32, i32, f32, Vec<u8>) {
        let (w, h, xmin, ymin, adv, bytes) = raw;
        let box_w = cells.clamp(1, 2).saturating_mul(self.cell_w).max(1);
        if w == 0 || h == 0 {
            return (w, h, xmin, ymin, box_w as f32, bytes);
        }
        let (mut w, mut h, mut xmin, mut ymin, mut bytes) = crop_to_ink(w, h, xmin, ymin, bytes);
        let cell_h = self.cell_h.max(1);
        let fits: Vec<DisplayFaceFit> = self.display_layer.iter().map(|face| face.fit).collect();
        let baseline = display_layer_baseline(
            &fits,
            Some(&fit),
            self.px,
            self.cell_w,
            cell_h,
            self.baseline,
        );
        let above = ymin.saturating_add(i32::try_from(h).unwrap_or(i32::MAX));
        let on_the_line = ymin <= 0 && above > 0;
        let mut shrink = cell_h as f32 / h as f32;
        if on_the_line && above > baseline {
            shrink = shrink.min(baseline.max(0) as f32 / above as f32);
        }
        let floor_room = i32::try_from(cell_h).unwrap_or(i32::MAX) - baseline;
        if on_the_line && -ymin > floor_room {
            shrink = shrink.min(floor_room.max(0) as f32 / -ymin as f32);
        }
        let mut adv = adv;
        if shrink < 1.0 && shrink.is_finite() {
            let dst_w = ((w as f32 * shrink).round() as usize).max(1);
            let dst_h = ((h as f32 * shrink).floor() as usize).clamp(1, cell_h);
            if let Some(scaled) = resample_mask_area(&bytes, w, h, dst_w, dst_h) {
                bytes = scaled;
                xmin = (xmin as f32 * shrink).round() as i32;
                ymin = shrink_ymin_about_baseline(ymin, shrink);
                adv *= shrink;
                w = dst_w;
                h = dst_h;
            }
        }
        let h_i = i32::try_from(h).unwrap_or(i32::MAX);
        let cell_h_i = i32::try_from(cell_h).unwrap_or(i32::MAX);
        ymin = ymin.clamp(
            baseline - cell_h_i,
            (baseline - h_i).max(baseline - cell_h_i),
        );
        if w > box_w
            && let Some(squeezed) = resample_mask_area(&bytes, w, h, box_w, h)
        {
            bytes = squeezed;
            w = box_w;
        }
        let slack = i32::try_from(box_w - w.min(box_w)).unwrap_or(0);
        xmin = if fit.proportional {
            slack / 2
        } else {
            let adv = if adv.is_finite() && adv > 0.0 {
                adv.round() as i32
            } else {
                i32::try_from(w).unwrap_or(0)
            };
            xmin.saturating_add(wide_center_offset(
                i32::try_from(box_w).unwrap_or(i32::MAX),
                adv,
            ))
            .clamp(0, slack)
        };
        let ymin = ymin.saturating_add(self.baseline - baseline);
        (w, h, xmin, ymin, box_w as f32, bytes)
    }

    /// Set the line-height multiplier on the cell box (the host's `terminalLineHeight`), re-deriving `cell_h`/`baseline` at the current px without changing the glyph rasterization size.
    pub fn set_line_height(&mut self, scale: f32) {
        let Some(scale) = requested_line_height(self.line_height_scale, scale) else {
            return;
        };
        let prev = self.line_height_scale;
        self.line_height_scale = scale;
        self.forget_cell_geometry();
        let Some((cell_h, baseline)) = self.derive_cell_geometry(self.px) else {
            self.line_height_scale = prev;
            return;
        };
        self.cell_h = cell_h;
        self.baseline = baseline;
        self.clear_glyph_images();
        self.deco_masks.borrow_mut().clear();
        self.undercurl_masks.borrow_mut().clear();
    }

    /// The current line-height multiplier (see [`Self::set_line_height`]).
    #[inline]
    pub const fn line_height(&self) -> f32 {
        self.line_height_scale
    }

    /// Test/diagnostic: the number of cached glyph images, so a test can assert that a resize ([`set_px`](Self::set_px)) or a face swap drops the caches.
    #[doc(hidden)]
    #[inline]
    #[must_use]
    pub fn glyph_cache_len(&self) -> usize {
        self.glyphs.len()
    }

    /// Test/diagnostic: whether one exact raster key is resident (so frame tests can prove both span variants of a fallback scalar went through the draw path).
    #[doc(hidden)]
    #[inline]
    #[must_use]
    pub fn glyph_cache_contains(&self, key: GlyphKey) -> bool {
        self.glyphs.contains_key(&key)
    }

    /// Set the explicit selected-text foreground (theme `selectionForeground`), or `None` to restore the WCAG contrast-floor default.
    #[inline]
    pub fn set_selection_fg(&mut self, fg: Option<u32>) {
        self.selection_fg = fg;
    }

    /// The host's selected-text foreground override.
    #[inline]
    pub const fn selection_fg(&self) -> Option<u32> {
        self.selection_fg
    }

    /// Set the per-cell minimum contrast ratio (xterm's `minimumContrastRatio`, 1..=21).
    #[inline]
    pub fn set_minimum_contrast(&mut self, ratio: f32) {
        self.min_contrast = ratio;
    }

    /// The configured per-cell minimum contrast ratio (`<= 1.0` = off).
    #[inline]
    #[must_use]
    pub const fn minimum_contrast(&self) -> f32 {
        self.min_contrast
    }

    /// The floor glyphs are blitted with.
    #[inline]
    #[must_use]
    pub fn effective_minimum_contrast(&self) -> f32 {
        vibrancy::effective_minimum_contrast(self.min_contrast, self.bg_opacity)
    }

    /// Set the default-background opacity (0..=1, clamped; Ghostty's `background-opacity`).
    #[inline]
    pub fn set_background_opacity(&mut self, opacity: f32) {
        let opacity = if opacity.is_finite() {
            opacity.clamp(0.0, 1.0)
        } else {
            1.0
        };
        if self.bg_opacity != opacity {
            self.invalidate_glyph_frame_ink();
            self.bg_opacity = opacity;
        }
    }

    /// The configured default-background opacity (see [`set_background_opacity`](Self::set_background_opacity)).
    #[inline]
    #[must_use]
    pub const fn background_opacity(&self) -> f32 {
        self.bg_opacity
    }

    /// Set the cursor-fill opacity (0..=1, clamped; Ghostty's `cursor-opacity`).
    #[inline]
    pub fn set_cursor_opacity(&mut self, opacity: f32) {
        self.cursor_opacity = if opacity.is_finite() {
            opacity.clamp(0.0, 1.0)
        } else {
            1.0
        };
    }

    /// The configured cursor-fill opacity (see [`set_cursor_opacity`](Self::set_cursor_opacity)).
    #[inline]
    #[must_use]
    pub const fn cursor_opacity(&self) -> f32 {
        self.cursor_opacity
    }

    #[inline]
    fn bg_transmittance(&self) -> u32 {
        255 - u32::from(vibrancy::bg_quad_alpha(self.bg_opacity))
    }

    /// Mark the pane focused (`false`) or unfocused (`true`).
    #[inline]
    pub fn set_selection_inactive(&mut self, inactive: bool) {
        self.selection_inactive = inactive;
    }

    /// Whether the pane is currently treated as unfocused for selection theming.
    #[inline]
    #[must_use]
    pub const fn selection_inactive(&self) -> bool {
        self.selection_inactive
    }

    /// Set the inactive (unfocused) selection background (0x00RRGGBB), or `None` to derive it from the active selection bg (see [`derive_inactive_selection_bg`]).
    #[inline]
    pub fn set_selection_inactive_bg(&mut self, bg: Option<u32>) {
        self.selection_inactive_bg = bg;
    }

    /// The host's inactive-selection background override, before theme derivation.
    #[inline]
    pub const fn selection_inactive_bg(&self) -> Option<u32> {
        self.selection_inactive_bg
    }

    /// The selection background to fill selected cells with this frame.
    #[inline]
    #[must_use]
    pub fn effective_selection_bg(&self) -> u32 {
        if self.selection_inactive {
            self.selection_inactive_bg.unwrap_or_else(|| {
                derive_inactive_selection_bg(self.theme.selection, self.theme.bg)
            })
        } else {
            self.theme.selection
        }
    }

    /// This frame's selection colour policy and entries, hoisted once (see [`SelectionPalette`]).
    #[must_use]
    pub fn selection_palette<'a>(&self, input: &'a RenderInput) -> SelectionPalette<'a> {
        let policy = SelectionPolicy {
            unset_active: self.theme.selection,
            unset_inactive: self.selection_inactive_bg.unwrap_or_else(|| {
                derive_inactive_selection_bg(self.theme.selection, self.theme.bg)
            }),
            inactive: self.selection_inactive,
            inactive_bg: self.selection_inactive_bg,
            host_fg: self.selection_fg,
            frame_bg: self.frame_bg(input),
        };
        let scalar =
            resolve_selection_colors(&policy, input.selection_bg, input.selection_fg, false);
        SelectionPalette {
            policy,
            scalar,
            entries: &input.selections,
        }
    }

    /// The current interior padding (px per edge).
    #[inline]
    pub const fn pad(&self) -> usize {
        self.pad
    }

    /// Set the base interior padding (px per edge).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AsymmetricPadLayout",
            action = "PickLayout",
            reason = "PickLayout enumerates external configuration inputs (pad, head, initial \
                      request, changed request); no single shipping method performs that \
                      environment choice. The Tier-1 conformance drives set_pad, set_head, and \
                      set_pad_top explicitly over the complete bounded lattice."
        )
    )]
    pub fn set_pad(&mut self, pad: usize) {
        if self.pad == pad {
            return;
        }
        self.pad = pad;
        self.pad_top = None;
    }

    /// The current top-only head band (px).
    #[inline]
    pub const fn head(&self) -> usize {
        self.head
    }

    /// Set the top-only head band (px above the padded grid).
    #[inline]
    pub fn set_head(&mut self, head_px: usize) {
        self.head = head_px;
    }

    /// The effective top interior pad (px).
    #[inline]
    pub fn pad_top(&self) -> usize {
        self.pad_top.unwrap_or(self.pad)
    }

    /// Override the top interior pad only (px above the grid, below the `head` band).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AsymmetricPadLayout",
            action = "ApplyInitialTop",
            project = "aterm_render::project_asymmetric_pad_layout"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AsymmetricPadLayout",
            action = "ApplyChangedTop",
            project = "aterm_render::project_asymmetric_pad_layout"
        )
    )]
    pub fn set_pad_top(&mut self, pad_top: usize) {
        self.pad_top = Some(pad_top.min(self.pad));
    }

    /// The host chrome band currently claiming the padding gutters, if any.
    #[inline]
    #[must_use]
    pub const fn chrome_bleed(&self) -> Option<ChromeBleed> {
        self.chrome_bleed
    }

    /// The frame pixel rows `[y0, y1)` that belong to host chrome, clamped to `frame_h`.
    #[inline]
    #[must_use]
    pub fn chrome_extent_px(&self, frame_h: usize) -> std::ops::Range<usize> {
        self.chrome_bleed.map_or(0..0, |b| {
            let y1 = (self.grid_top() + b.rows * self.cell_h).min(frame_h);
            let y0 = if b.first == 0 {
                0
            } else {
                (self.grid_top() + b.first * self.cell_h).min(y1)
            };
            y0..y1
        })
    }

    /// Declare (or clear, with `None`) the top rows that are host chrome and the tone their surface extends into the window's padding ([`ChromeBleed`]).
    #[inline]
    pub fn set_chrome_bleed(&mut self, bleed: Option<ChromeBleed>) {
        self.chrome_bleed = bleed.filter(|b| b.rows > b.first);
    }

    /// The grid content's y-origin in window px.
    #[inline]
    pub fn grid_top(&self) -> usize {
        self.pad_top.unwrap_or(self.pad) + self.head
    }

    /// Pixel size of a `rows`x`cols` grid including `2·pad` of interior padding and the top-only `head` band.
    #[inline]
    pub const fn frame_size(&self, rows: usize, cols: usize) -> (usize, usize) {
        frame_size_px(rows, cols, self.cell_w, self.cell_h, self.pad, self.head)
    }

    /// Pixel dimensions admitted by the shared framebuffer storage limit.
    #[inline]
    pub fn checked_frame_size(&self, rows: usize, cols: usize) -> Result<(usize, usize), String> {
        checked_frame_size_px(rows, cols, self.cell_w, self.cell_h, self.pad, self.head)
    }

    /// The font baseline (pixels from the cell top to the glyph baseline).
    #[inline]
    pub const fn baseline(&self) -> i32 {
        self.baseline
    }

    /// The pixel size this renderer rasterizes glyphs at.
    #[inline]
    pub const fn px(&self) -> f32 {
        self.px
    }

    /// Set the cursor blink phase.
    #[inline]
    pub fn set_cursor_blink_phase(&mut self, on: bool) {
        self.cursor_blink_phase = on;
    }

    /// The current cursor blink phase (see [`Self::set_cursor_blink_phase`]).
    #[inline]
    pub const fn cursor_blink_phase(&self) -> bool {
        self.cursor_blink_phase
    }

    /// Force the cursor to be drawn as `style` regardless of both the frame's host/effect override and the terminal's DECSCUSR style (`None` restores their normal precedence).
    #[inline]
    pub fn set_cursor_style_override(&mut self, style: Option<CursorStyle>) {
        self.cursor_style_override = style;
    }

    /// The active cursor style override (see [`Self::set_cursor_style_override`]).
    #[inline]
    pub const fn cursor_style_override(&self) -> Option<CursorStyle> {
        self.cursor_style_override
    }

    fn spinner_star_preempts(&self, ch: char) -> bool {
        self.primary_yields_small_dingbats
            && procedural::is_spinner_star(ch)
            && self.px < bundled::SPINNER_STARS_CRISP_PX
    }

    fn keeps_faces_spinner_frame(&self, key: GlyphKey, ch: char) -> bool {
        self.primary_yields_small_dingbats
            && harmonized_fallback_source(key.source)
            && procedural::is_spinner_star(ch)
            && !self.spinner_star_preempts(ch)
    }

    fn fitted_fallback_regular(
        &mut self,
        key: GlyphKey,
        ch: char,
    ) -> (usize, usize, i32, i32, f32, Vec<u8>) {
        let mut raw = self.fallback_mono_raster(key.source, ch);
        stem_darken(&mut raw.5, &self.stem_lut);
        self.harmonize_fallback_raster(
            usize::from(key.cell_span),
            raw,
            DeferredStyle::NONE,
            fallback_fit_class(ch),
        )
    }

    fn even_spinner_frame(
        &mut self,
        key: GlyphKey,
        ch: char,
        img: (usize, usize, i32, i32, Vec<u8>),
    ) -> (usize, usize, i32, i32, Vec<u8>) {
        if img.4.is_empty() || img.0.checked_mul(img.1) != Some(img.4.len()) {
            return img;
        }
        let mut inks = Vec::with_capacity(procedural::SPINNER_STARS.len());
        let (mut own, mut rest) = (None, None);
        for frame in procedural::SPINNER_STARS {
            let frame_key = self.glyph_key(frame);
            if frame_key.source != key.source {
                continue;
            }
            let (w, h, xmin, ymin, _, bytes) = self.fitted_fallback_regular(frame_key, frame);
            let ink = coverage_ink(&bytes);
            if ink > 0.0 {
                inks.push(ink);
            }
            if frame == procedural::SPINNER_REST {
                rest = Some(ink);
            }
            if frame == ch {
                own = Some((w, h, xmin, ymin, bytes, ink));
            }
        }
        let Some((w, h, xmin, ymin, bytes, ink)) = own else {
            return img;
        };
        let evened = evened_ink(&inks, ink, rest, bundled::SPINNER_FRAME_SPREAD).unwrap_or(ink);
        // Claude Code's cycle shows the primary's own `*` between these frames, a glyph this
        // pass does not redraw: a frame heavier than the cycle's spread of it comes down to
        // that, or to the rest where the rest, kept as the face draws it, lies past it.
        let star_key = self.glyph_key('*');
        let star = match star_key.source {
            FaceId::Primary => match self.rasterize(star_key) {
                GlyphImage::Mono { bytes, .. } => coverage_ink(&bytes),
                GlyphImage::Rgba { .. } => 0.0,
            },
            _ => 0.0,
        };
        let target = cycle_cap(star, rest, bundled::SPINNER_CYCLE_SPREAD)
            .map_or(evened, |cap| evened.min(cap));
        if (target - ink).abs() <= 1e-3 {
            return img;
        }
        let bound = (self.cell_w as f32 / 8.0).max(1.0);
        let reach = bound.ceil() as usize + 1;
        let cells = usize::from(key.cell_span);
        let (regular, _, _) = self.outline_in_cell(cells, (w, h, xmin, ymin, &bytes), reach);
        let offset = regular.offset_for_ink(target, bound);
        let (w, h, xmin, ymin, bytes) = img;
        let (field, xmin, ymin) = self.outline_in_cell(cells, (w, h, xmin, ymin, &bytes), reach);
        crop_to_ink(field.w, field.h, xmin, ymin, field.coverage(offset))
    }

    fn outline_in_cell(
        &self,
        cells: usize,
        (w, h, xmin, ymin, bytes): (usize, usize, i32, i32, &[u8]),
        pad: usize,
    ) -> (OutlineField, i32, i32) {
        let box_w = cells.clamp(1, 2) * self.cell_w;
        let top = self.baseline - h as i32 - ymin;
        let room = |before: i32, len: usize, band: usize| {
            let before = usize::try_from(before).unwrap_or(0);
            let after = band.saturating_sub(before + len);
            (before.min(pad), after.min(pad))
        };
        let (left, right) = room(xmin, w, box_w);
        let (above, below) = room(top, h, self.cell_h);
        let (pw, ph) = (left + w + right, above + h + below);
        let mut padded = vec![0u8; pw * ph];
        if w.checked_mul(h) == Some(bytes.len()) {
            for (y, row) in bytes.chunks_exact(w.max(1)).enumerate() {
                let at = (above + y) * pw + left;
                padded[at..at + w].copy_from_slice(row);
            }
        }
        let lift = |n: usize| i32::try_from(n).unwrap_or(0);
        (
            OutlineField::new(&padded, pw, ph, pad),
            xmin - lift(left),
            ymin - lift(below),
        )
    }

    fn lower_asterisk(
        &self,
        asterisk: Option<bundled::LoweredAsterisk>,
        gid: Option<u16>,
        (w, h, xmin, ymin, bytes): (usize, usize, i32, i32, &[u8]),
    ) -> (i32, i32) {
        let Some(a) = asterisk.filter(|a| Some(a.gid) == gid) else {
            return (xmin, ymin);
        };
        let seen = |y: &usize| {
            bytes
                .get(y * w..(y + 1) * w)
                .is_some_and(|row| row.iter().any(|&c| c >= 32))
        };
        let (Some(first), Some(last)) = ((0..h).find(seen), (0..h).rev().find(seen)) else {
            return (xmin, ymin);
        };
        let inked_mid = ymin as f32 + h as f32 - (first + last + 1) as f32 / 2.0;
        let (dx, dy) = a.shift_px(self.px, inked_mid);
        (xmin + dx, ymin + dy)
    }

    fn primary_unicode_gid(&mut self, ch: char) -> Option<u16> {
        if self.private_use_yields_to_embedded(self.primary_yields_private_use, ch) {
            return None;
        }
        if self.primary_yields_wide_characters && aterm_grapheme::char_width(ch) == 2 {
            return None;
        }
        if self.primary_yields_small_dingbats && bundled::is_small_dingbat(ch) {
            return None;
        }
        if let Some(&g) = self.primary_gid_cache.get(&ch) {
            return g;
        }
        let g = self
            .shaping_primary_bytes
            .as_deref()
            .and_then(|b| aterm_font::Face::parse(b, self.primary_index).ok())
            .and_then(|f| f.glyph_index(ch))
            .map(|gid| gid.0)
            .filter(|&gid| gid != 0);
        self.primary_gid_cache.insert(ch, g);
        g
    }

    /// Resolve `ch` to its glyph cache key at this renderer's size through the fallback-aware chain (the primary owns the glyph unless it has none; only then is the fallback loaded and consulted), memoized per char so the hot path pays the face lookups once.
    pub fn glyph_key(&mut self, ch: char) -> GlyphKey {
        self.glyph_key_inner(ch, ScalarPresentation::Default).0
    }

    /// Resolve `ch` under an explicit VS15 text-presentation request.
    pub fn glyph_key_text(&mut self, ch: char) -> GlyphKey {
        qualify_text_presentation_key(self.glyph_key_inner(ch, ScalarPresentation::Text).0)
    }

    fn glyph_key_inner(&mut self, ch: char, presentation: ScalarPresentation) -> (GlyphKey, bool) {
        let memo_key = (ch, presentation);
        if let Some(&key) = self.keys.get(&memo_key) {
            return (key, false);
        }
        if self.display_layer_draws(ch) {
            let mut key =
                GlyphKey::mono_char(FaceId::DisplayMix, ch, StyleBits::REGULAR, self.px_q);
            key.cell_span = fallback_cell_count(ch, &self.shaping) as u8;
            self.keys.insert(memo_key, key);
            return (key, false);
        }
        let policy = font_chain::ChainPolicy {
            runtime_discovery: self.runtime_discovery,
            sealed: self.admitted_sources_sealed,
            wants_emoji: presentation == ScalarPresentation::Default
                && aterm_grapheme::is_emoji_presentation(ch),
            prefers_symbol: font_chain::is_private_use(ch),
        };
        let mut primary_gid: Option<u16> = None;
        let mut probe = RendererProbe {
            r: self,
            ch,
            wants_emoji: policy.wants_emoji,
            primary_gid: &mut primary_gid,
        };
        let resolved = font_chain::resolve_chain(&mut probe, policy);
        let face = match resolved {
            font_chain::Resolution::Provisional => {
                debug_assert!(
                    !self.keys.contains_key(&memo_key),
                    "a provisional `.notdef` must never be memoized — it strands the tofu",
                );
                return (
                    GlyphKey::mono_char(FaceId::Primary, ch, StyleBits::REGULAR, self.px_q),
                    true,
                );
            }
            font_chain::Resolution::Notdef => {
                self.missing_font_classes |= if policy.wants_emoji {
                    MISSING_FONT_CLASS_EMOJI
                } else {
                    MISSING_FONT_CLASS_TEXT
                };
                FaceId::Primary
            }
            font_chain::Resolution::Runtime(_) => FaceId::RuntimeFallback,
            font_chain::Resolution::Face(f) => f,
        };
        let synthetic = face == FaceId::Procedural
            && procedural::covers_symbol(ch)
            && !self.spinner_star_preempts(ch);
        if synthetic {
            self.missing_font_classes |= if policy.wants_emoji {
                MISSING_FONT_CLASS_EMOJI
            } else {
                MISSING_FONT_CLASS_TEXT
            };
        }
        let mut key = match face {
            FaceId::ColorEmoji => {
                GlyphKey::rgba_char_in(self.color_pick(ch).unwrap_or(0), ch, self.px_q)
            }
            FaceId::Primary if primary_gid.is_some() => {
                GlyphKey::mono_gid(primary_gid.unwrap(), StyleBits::REGULAR, self.px_q)
            }
            source => GlyphKey::mono_char(source, ch, StyleBits::REGULAR, self.px_q),
        };
        if synthetic || (key.glyph_class == GlyphClass::Mono && cell_fitted_source(key.source)) {
            key.cell_span = fallback_cell_count(ch, &self.shaping) as u8;
        }
        debug_assert!(
            resolved
                .recovery_slot()
                .is_none_or(|slot| font_chain::slot_for_source(key.source) == Some(slot)),
            "glyph key {key:?} cannot recover the face that {resolved:?} resolved",
        );
        self.keys.insert(memo_key, key);
        (key, false)
    }

    fn runtime_probe_ctx(&self) -> FallbackProbeCtx {
        #[cfg(target_os = "macos")]
        let use_ct = self.rasterizer == RasterKind::CoreText;
        #[cfg(not(target_os = "macos"))]
        let use_ct = false;
        let (primary_weight, primary_italic) = self
            .shaping_primary_bytes
            .as_ref()
            .and_then(|b| aterm_font::Face::parse(b, self.primary_index).ok())
            .map_or((400, false), |f| (f.weight().to_number(), f.is_italic()));
        FallbackProbeCtx {
            use_ct,
            primary_weight,
            primary_italic,
        }
    }

    /// Like [`glyph_key`](Self::glyph_key) but for a bold/italic cell.
    pub fn glyph_key_styled(&mut self, ch: char, style: StyleBits) -> GlyphKey {
        self.glyph_key_styled_inner(ch, style, ScalarPresentation::Default)
    }

    /// A combining overlay follows a small footer cell's script and weight.
    pub fn combining_mark_key(&mut self, mark: char, cell: &RenderCell) -> GlyphKey {
        match cell.script {
            aterm_core::terminal::Script::Normal => self.glyph_key(mark),
            aterm_core::terminal::Script::Sub { .. } => {
                self.glyph_key_styled(mark, cell_style(cell))
            }
        }
    }

    /// Styled twin of [`Self::glyph_key_text`] for an explicit VS15 cell.
    pub fn glyph_key_text_styled(&mut self, ch: char, style: StyleBits) -> GlyphKey {
        self.glyph_key_styled_inner(ch, style, ScalarPresentation::Text)
    }

    fn glyph_key_styled_inner(
        &mut self,
        ch: char,
        style: StyleBits,
        presentation: ScalarPresentation,
    ) -> GlyphKey {
        let script = style.script();
        if script != StyleBits::REGULAR {
            let base = self.glyph_key_styled_inner(ch, style.without_script(), presentation);
            let scalable = matches!(base.glyph_class, GlyphClass::Mono | GlyphClass::MonoGid)
                && !matches!(base.source, FaceId::Procedural | FaceId::ColorEmojiMono);
            return if scalable {
                GlyphKey {
                    style: StyleBits(base.style.0 | script.0),
                    ..base
                }
            } else {
                base
            };
        }
        if style == StyleBits::REGULAR {
            return match presentation {
                ScalarPresentation::Default => self.glyph_key(ch),
                ScalarPresentation::Text => self.glyph_key_text(ch),
            };
        }
        let memo_key = (ch, style, presentation);
        if let Some(&key) = self.styled_keys.get(&memo_key) {
            return key;
        }
        let (base, provisional) = self.glyph_key_inner(ch, presentation);
        let real_bold = style.contains(StyleBits::BOLD)
            && base.source == FaceId::Primary
            && matches!(base.glyph_class, GlyphClass::Mono | GlyphClass::MonoGid);
        let bold_italic_real =
            real_bold && style.contains(StyleBits::ITALIC) && self.bold_font.is_some() && {
                self.ensure_styled_faces();
                self.styled_face_covers(2, ch)
            };
        let key = if real_bold
            && !bold_italic_real
            && let Some(gid) = self.bold_unicode_gid(ch)
        {
            let residual = StyleBits(style.0 & !StyleBits::BOLD.0);
            GlyphKey {
                source: FaceId::BoldPrimary,
                glyph_class: GlyphClass::MonoGid,
                ch_or_id: gid as u32,
                style: residual,
                cell_span: 0,
                px_q: self.px_q,
            }
        } else if base.source == FaceId::Procedural || base.source == FaceId::ColorEmojiMono {
            // Cell-exact or an emoji silhouette: no styled variant.
            base
        } else if base.glyph_class == GlyphClass::MonoGid {
            GlyphKey::mono_char(base.source, ch, style, self.px_q)
        } else if base.glyph_class == GlyphClass::Mono {
            GlyphKey { style, ..base }
        } else {
            base
        };
        let key = if presentation == ScalarPresentation::Text {
            qualify_text_presentation_key(key)
        } else {
            key
        };
        if !provisional {
            self.styled_keys.insert(memo_key, key);
        }
        key
    }

    /// Resolve `ch` for a cell that requested emoji presentation (a VS16-widened base char, see [`RenderCell::emoji_presentation`]).
    pub fn glyph_key_emoji(&mut self, ch: char) -> GlyphKey {
        if let Some(&key) = self.emoji_keys.get(&ch) {
            return key;
        }
        let key = if let Some(face) = self.color_pick(ch) {
            GlyphKey::rgba_char_in(face, ch, self.px_q)
        } else {
            let (key, provisional) = self.glyph_key_inner(ch, ScalarPresentation::Default);
            if provisional {
                return key;
            }
            key
        };
        self.emoji_keys.insert(ch, key);
        key
    }

    /// Shape an emoji grapheme cluster (ZWJ / skin-tone / keycap / flag) on the colour chain, cached.
    pub(crate) fn shape_cluster(&mut self, cluster: &str) -> Option<ClusterShape> {
        if let Some((shape, from_prev)) = self.cluster_shapes.get(cluster) {
            if from_prev {
                self.cluster_shapes.insert(cluster, shape);
            }
            return shape;
        }
        let shape = self.shape_cluster_uncached(cluster);
        self.cluster_shapes.insert(cluster, shape);
        shape
    }

    fn shape_cluster_uncached(&mut self, cluster: &str) -> Option<ClusterShape> {
        let mut i = 0;
        let (face, decision) = loop {
            if i == self.color_chain.len() && !self.load_next_color_face() {
                return None;
            }
            let bytes = &self.color_chain[i].bytes;
            let decision = shape_colour_cluster(bytes, cluster).and_then(|glyphs| {
                let tt = aterm_font::Face::parse(bytes, 0).ok()?;
                colour_cluster_decision(&tt, cluster, &glyphs)
            });
            if let Some(decision) = decision {
                break (u8::try_from(i).ok()?, decision);
            }
            i += 1;
        };
        match decision {
            ColourCluster::Glyph(gid) => Some(ClusterShape::Glyph { face, gid }),
            ColourCluster::Run(run) => self
                .cluster_shapes
                .add_run(emoji_cluster::ClusterRun { face, ..run })
                .map(ClusterShape::Run),
        }
    }

    /// Resolve an emoji cluster to its colour glyph key.
    pub fn glyph_key_cluster(&mut self, cluster: &str) -> Option<GlyphKey> {
        Some(match self.shape_cluster(cluster)? {
            ClusterShape::Glyph { face, gid } => GlyphKey::rgba_gid_in(face, gid, self.px_q),
            ClusterShape::Run(run) => GlyphKey::rgba_cluster(run, self.px_q),
        })
    }

    /// Install the text-shaping config (ligature mode, features, ambiguous width).
    pub fn set_text_shaping(&mut self, shaping: aterm_types::text_shaping::TextShapingConfig) {
        self.invalidate_glyph_frame_ink();
        let ambiguous_width_changed = self.shaping.ambiguous_width != shaping.ambiguous_width;
        self.shaping = shaping;
        if ambiguous_width_changed {
            self.keys.clear();
            self.styled_keys.clear();
            self.emoji_keys.clear();
            self.clear_glyph_images();
            self.deco_masks.borrow_mut().clear();
        }
        let primary: Vec<aterm_types::text_shaping::FontFeature> = self
            .shaping
            .font_features
            .iter()
            .filter(|set| set.font_id == 0)
            .flat_map(|set| set.features.iter().copied())
            .collect();
        let ligatures_on =
            self.shaping.ligature_mode != aterm_types::text_shaping::LigatureMode::Disabled;
        self.resolved_features = ligature_shaping::build_feature_list(&primary, ligatures_on);
        self.engine_features = ligature_shaping::engine_features(&self.resolved_features);
        self.has_user_features = match self.shaping_primary_bytes.as_deref() {
            Some(b) => primary
                .iter()
                .any(|f| ligature_shaping::font_advertises_feature(b, self.primary_index, f.tag)),
            None => false,
        };
        self.shaped_runs.clear();
    }

    /// The current text-shaping config.
    #[inline]
    pub const fn text_shaping(&self) -> &aterm_types::text_shaping::TextShapingConfig {
        &self.shaping
    }

    /// Install the coverage blend mode (config `text_blending`; default [`TextBlending::LinearCorrected`]).
    #[inline]
    pub fn set_text_blending(&mut self, mode: TextBlending) {
        if self.text_blending != mode {
            self.invalidate_glyph_frame_ink();
        }
        self.text_blending = mode;
    }

    /// The active coverage blend mode (the GPU encode reads this off its wrapped CPU face so both backends remap identically).
    #[inline]
    #[must_use]
    pub const fn text_blending(&self) -> TextBlending {
        self.text_blending
    }

    /// Set macOS `font_thicken` (CoreText font smoothing at raster time).
    pub fn set_font_thicken(&mut self, on: bool) {
        if self.font_thicken != on {
            self.font_thicken = on;
            self.clear_glyph_images();
        }
    }

    /// The active macOS `font_thicken` flag.
    #[inline]
    #[must_use]
    pub const fn font_thicken(&self) -> bool {
        self.font_thicken
    }

    /// Set the aesthetic stem gamma (config `stem_gamma`, which the host passes here).
    pub fn set_stem_gamma(&mut self, gamma: f32) {
        let gamma = clamp_stem_gamma(gamma);
        if (gamma - self.stem_gamma).abs() > f32::EPSILON {
            self.stem_gamma = gamma;
            self.stem_lut = build_stem_lut(gamma);
            self.clear_glyph_images();
            #[cfg(all(unix, not(target_os = "macos")))]
            self.subpx_glyphs.clear();
        }
    }

    /// The gamma the stem-darkening LUT was built from (`1.0` = identity).
    #[inline]
    #[must_use]
    pub const fn stem_gamma(&self) -> f32 {
        self.stem_gamma
    }

    /// Set the native grid-fitting mode (config `font_hinting`), returning whether it changed;
    /// macOS has no hinted path and ignores it.
    pub fn set_font_hinting(&mut self, mode: &str) -> bool {
        #[cfg(any(all(unix, not(target_os = "macos")), windows))]
        {
            let mode = if hinted::HintMode::portable_forced() {
                hinted::HintMode::Off
            } else {
                hinted::HintMode::parse(mode)
            };
            if mode == self.hint_mode {
                return false;
            }
            self.hint_mode = mode;
            self.hint_bank.clear();
            self.clear_glyph_images();
            self.keys.clear();
            self.styled_keys.clear();
            #[cfg(all(unix, not(target_os = "macos")))]
            self.subpx_glyphs.clear();
            true
        }
        #[cfg(not(any(all(unix, not(target_os = "macos")), windows)))]
        {
            let _ = mode;
            false
        }
    }

    /// The active grid-fitting mode's canonical spelling (`"full"` / `"light"` / `"off"`; the retired `"native"` reads back as `"light"`, which is what it renders).
    #[inline]
    #[must_use]
    pub const fn font_hinting(&self) -> &'static str {
        #[cfg(any(all(unix, not(target_os = "macos")), windows))]
        {
            match self.hint_mode {
                hinted::HintMode::Full => "full",
                hinted::HintMode::Light => "light",
                hinted::HintMode::Off => "off",
            }
        }
        #[cfg(not(any(all(unix, not(target_os = "macos")), windows)))]
        {
            "off"
        }
    }

    /// Set the Linux subpixel-RGB text mode (config `font_subpixel`).
    pub fn set_font_subpixel(&mut self, mode: &str) -> bool {
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let mode = if hinted::HintMode::portable_forced() {
                subpixel::SubpixelMode::Off
            } else {
                subpixel::SubpixelMode::parse(mode)
            };
            if mode == self.subpix_mode {
                return false;
            }
            self.subpix_mode = mode;
            self.subpx_glyphs.clear();
            self.invalidate_glyph_frame_ink();
            true
        }
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        {
            let _ = mode;
            false
        }
    }

    /// The active Linux subpixel mode's spelling (`"off"` / `"rgb"` / `"bgr"`), the [`Self::set_font_subpixel`] round-trip.
    #[inline]
    #[must_use]
    pub fn font_subpixel(&self) -> &'static str {
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            self.subpix_mode.as_str()
        }
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        {
            "off"
        }
    }

    /// The frame-level precondition shared by the observer and CPU blitter. Individual
    /// keys may still use mono when the LCD rasterizer cannot represent their face/style.
    fn input_uses_lcd_text(&self, input: &RenderInput) -> bool {
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            self.subpix_mode != subpixel::SubpixelMode::Off
                && self.bg_opacity >= 1.0
                && input.wallpaper.is_none()
        }
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        {
            let _ = input;
            false
        }
    }

    /// The configured primary-face `font_features` tags the active font does not advertise in its `GSUB`, so they can never take effect (any feature on a no-`GSUB` font like Menlo, or an unsupported tag like `ss99`).
    #[must_use]
    pub fn unsupported_user_feature_tags(&self) -> Vec<[u8; 4]> {
        let Some(bytes) = self.shaping_primary_bytes.as_ref() else {
            return Vec::new();
        };
        let mut out: Vec<[u8; 4]> = Vec::new();
        for set in self.shaping.font_features.iter().filter(|s| s.font_id == 0) {
            for f in &set.features {
                if !out.contains(&f.tag)
                    && !ligature_shaping::font_advertises_feature(bytes, self.primary_index, f.tag)
                {
                    out.push(f.tag);
                }
            }
        }
        out
    }

    /// The feature list currently applied to ligature shaping runs (base `liga`+`calt` plus the resolved primary-face user `font_features`).
    #[doc(hidden)]
    #[must_use]
    pub fn resolved_features_for_test(&self) -> &[aterm_types::text_shaping::FontFeature] {
        &self.resolved_features
    }

    /// Whether the primary face advertises a `liga`/`calt` `GSUB` feature (probed at construction).
    #[inline]
    #[must_use]
    pub const fn has_ligature_features(&self) -> bool {
        self.has_ligature_features
    }

    #[inline]
    const fn ligatures_globally_off(&self) -> bool {
        matches!(
            self.shaping.ligature_mode,
            aterm_types::text_shaping::LigatureMode::Disabled
        )
    }

    /// Build the per-column glyph plan for row `r`.
    pub fn row_glyph_plan(
        &mut self,
        input: &RenderInput,
        r: usize,
        break_cols: &[usize],
        out: &mut Vec<ColumnGlyph>,
    ) {
        let cols = input.cols;
        let cells = input.cells.get(r).map(Vec::as_slice).unwrap_or(&[]);
        if !ligature_shaping::should_run_shaping(
            self.shaping_primary_bytes.is_some(),
            self.has_user_features,
            self.ligatures_globally_off(),
            self.has_ligature_features,
        ) {
            out.clear();
            out.resize(cols, ColumnGlyph::PerCell);
            return;
        }
        self.ensure_styled_faces();
        let min_run = ligature_shaping::shaping_min_run(self.has_user_features);
        let row_clusters = input.clusters.get(r).map(Vec::as_slice).unwrap_or(&[]);
        let row_images = input.images.get(r).map(Vec::as_slice).unwrap_or(&[]);
        self.shapeable_scratch.clear();
        self.shapeable_scratch.resize(cols, false);
        self.break_mask_scratch.clear();
        self.break_mask_scratch.resize(cols, false);
        for &bc in break_cols {
            if bc < cols {
                self.break_mask_scratch[bc] = true;
            }
        }
        ligature_run_boundaries_into(input, r, &mut self.run_boundary_scratch);
        let any_face_yields = self.primary_yields_private_use
            || (self.bold_font.is_some() && self.bold_yields_private_use)
            || self
                .styled_faces
                .iter()
                .flatten()
                .any(|sf| sf.yields_private_use);
        #[allow(
            clippy::needless_range_loop,
            reason = "index drives disjoint field borrows (read break_mask_scratch, write shapeable_scratch) plus cluster/image lookups by column"
        )]
        for c in 0..cols.min(cells.len()) {
            let has_cluster = cluster_for(row_clusters, c).is_some();
            let img = image_covers(row_images, c);
            let is_procedural = procedural::covers(cells[c].ch);
            let yielded = self.private_use_yields_to_embedded(any_face_yields, cells[c].ch)
                || (self.primary_yields_small_dingbats && bundled::is_small_dingbat(cells[c].ch));
            let layered = self.display_layer_draws(cells[c].ch);
            self.shapeable_scratch[c] =
                ligature_shaping::cell_is_shapeable(&cells[c], has_cluster, img)
                    && !self.break_mask_scratch[c]
                    && !is_procedural
                    && !yielded
                    && !layered;
        }
        let style_of = |c: usize| cell_style(&cells[c]);
        let rb = self.shaping_primary_bytes.as_deref();
        let rb_index = self.primary_index;
        let injected_bold = self
            .bold_font_bytes
            .as_deref()
            .map(|b| (b, self.bold_font_index));
        let styled_bytes: [Option<(&[u8], u32)>; 3] = std::array::from_fn(|i| {
            self.styled_faces[i]
                .as_ref()
                .map(|sf| (sf.bytes.as_ref(), sf.index))
        });
        // The instance each styled run shapes at, by `styled_row_faces`' slot (3 = the
        // injected bold).
        let styled_vars: [Option<&[(u32, f32)]>; 4] = std::array::from_fn(|i| match i {
            0..=2 => self.styled_faces[i]
                .as_ref()
                .and_then(|sf| sf.instance.drawn.as_deref()),
            _ => self.bold_font_instance.drawn.as_deref(),
        });
        let primary_vars: &[(aterm_font::Tag, f32)] = &self.shaping_variations;
        let mut row_face: Option<Option<ligature_shaping::RowShaper<'_>>> = None;
        let mut styled_row_faces: [Option<Option<ligature_shaping::RowShaper<'_>>>; 4] =
            std::array::from_fn(|_| None);
        let cache = &mut self.shaped_runs;
        let features = &self.engine_features;
        let shaping_buffer = &mut self.shaping_buffer;
        let admit_collapsed = self.shaping.admit_collapsed;
        ligature_shaping::plan_row_runs(
            cells,
            cols,
            &self.shapeable_scratch,
            &self.run_boundary_scratch,
            input.visual_to_logical_row(r),
            min_run,
            style_of,
            |run, run_chars, style| {
                match cache.get(style, run) {
                    Some(ShapedHit::Cur(c)) => return c.clone(),
                    Some(ShapedHit::Prev(c)) => {
                        let promoted = c.clone();
                        cache.insert(style, Box::<str>::from(run), promoted.clone());
                        return promoted;
                    }
                    None => {}
                }
                let present = [
                    styled_bytes[0].is_some(),
                    styled_bytes[1].is_some(),
                    styled_bytes[2].is_some(),
                ];
                let pick = resolve_styled_face(style, injected_bold.is_some(), present);
                let res = if matches!(pick, FacePick::Primary) {
                    let face = row_face.get_or_insert_with(|| {
                        rb.and_then(|b| {
                            let mut f = aterm_font::Face::parse(b, rb_index).ok()?;
                            for &(tag, value) in primary_vars {
                                f.set_variation(tag, value);
                            }
                            Some(ligature_shaping::RowShaper::new(
                                aterm_font::shape::ShapeFace::new(f),
                                features,
                            ))
                        })
                    });
                    face.as_mut()
                        .and_then(|f| f.shape(run, run_chars, admit_collapsed, shaping_buffer))
                } else {
                    // Preserve each sibling's collection index, and shape at
                    // the face's own instance (`styled_vars`).
                    let (slot, source) = match pick {
                        FacePick::Styled { slot, .. } => (slot, styled_bytes[slot]),
                        FacePick::InjectedBold { .. } => (3, injected_bold),
                        FacePick::Primary => (3, None),
                    };
                    let face = styled_row_faces[slot].get_or_insert_with(|| {
                        source.and_then(|(b, idx)| {
                            let mut f = aterm_font::Face::parse(b, idx).ok()?;
                            for &(tag, value) in styled_vars[slot].unwrap_or(&[]) {
                                f.set_variation(aterm_font::Tag(tag), value);
                            }
                            Some(ligature_shaping::RowShaper::new(
                                aterm_font::shape::ShapeFace::new(f),
                                features,
                            ))
                        })
                    });
                    face.as_mut()
                        .and_then(|f| f.shape(run, run_chars, admit_collapsed, shaping_buffer))
                };
                let res = res.map(std::sync::Arc::new);
                cache.insert(style, Box::<str>::from(run), res.clone());
                res
            },
            &mut self.shape_run_scratch,
            &mut self.shape_chars_scratch,
            out,
        );
    }

    /// The glyph key for a ligated column.
    #[inline]
    pub const fn ligature_key(&self, gid: u16, style: StyleBits) -> GlyphKey {
        GlyphKey::mono_gid(gid, style, self.px_q)
    }

    /// The glyph key for cell `k` of a collapsed (Cascadia n:1) ligature drawing wide glyph `gid` at the run's SGR `style`.
    #[inline]
    pub const fn ligature_slice_key(&self, gid: u16, k: u16, style: StyleBits) -> GlyphKey {
        GlyphKey::mono_gid_slice(gid, k, style, self.px_q)
    }

    /// The columns of row `r` forced per-cell this frame for ligature purposes (the cursor cell under `LigatureMode::CursorDisabled`).
    #[inline]
    pub fn ligature_break_cols_for_row_into(
        &self,
        input: &RenderInput,
        r: usize,
        out: &mut Vec<usize>,
    ) {
        ligature_break_cols_into(input, r, &self.shaping, out);
    }

    fn retain_glyph_working_set(&mut self) {
        let frame = self.glyph_frame;
        let mut bytes = 0usize;
        self.glyphs.retain(|_, g| {
            let keep = g.used.get() == frame;
            if keep {
                bytes = bytes.saturating_add(g.img.byte_len());
            }
            keep
        });
        #[cfg(all(unix, not(target_os = "macos")))]
        self.subpx_glyphs.retain(|k, _| self.glyphs.contains_key(k));
        self.glyph_bytes = bytes;
        self.glyph_kept = (self.glyphs.len(), bytes);
        self.chrome_room_memo = None;
    }

    fn clear_glyph_images(&mut self) {
        self.invalidate_glyph_frame_ink();
        self.glyphs.clear();
        self.glyph_generation = self.glyph_generation.wrapping_add(1);
        #[cfg(all(unix, not(target_os = "macos")))]
        self.subpx_glyphs.clear();
        self.glyph_bytes = 0;
        self.glyph_kept = (0, 0);
        self.chrome_room_memo = None;
    }

    fn icon_metrics(&mut self, bold: bool) -> procedural::IconMetrics {
        let style = if bold {
            StyleBits::BOLD
        } else {
            StyleBits::REGULAR
        };
        let l = self.glyph_key_styled('l', style);
        let stem = match &self.cached_glyph(l).img {
            GlyphImage::Mono {
                width,
                height,
                bytes,
                ..
            } if *height > 0 && *width > 0 => {
                let row = height / 2;
                bytes[row * width..(row + 1) * width]
                    .iter()
                    .map(|&c| f32::from(c))
                    .sum::<f32>()
                    / 255.0
            }
            _ => (self.cell_w as f32 / 8.0).max(1.0),
        };
        let h = self.glyph_key_styled('H', style);
        let cap = self
            .glyph_ink_box_cached(h)
            .map_or(self.cell_h as f32 * 0.55, |(hh, _)| hh as f32);
        let dot = self.glyph_key_styled('\u{25cf}', style);
        let own = matches!(dot.source, FaceId::Primary | FaceId::BoldPrimary)
            && self.primary_unicode_gid('\u{25cf}').is_some();
        let disc = own
            .then(|| self.glyph_ink_box_cached(dot))
            .flatten()
            .map_or(cap, |(hh, _)| hh as f32);
        procedural::IconMetrics {
            baseline: self.baseline as f32,
            cap,
            stem,
            head: self.ascii_ink_room().0 as f32,
            disc,
        }
    }

    /// The rows `[y0, y1)` of the face's single underline in a cell.
    #[must_use]
    pub fn chrome_ring_floor(&self) -> (usize, usize) {
        deco::underline_band(UnderlineStyle::Single, self.cell_h, self.deco_metrics())
            .unwrap_or((self.cell_h.saturating_sub(2), self.cell_h.saturating_sub(1)))
    }

    /// The glyph key cell `c` of a frame row (`cells`) draws instead of its character's, when the row's chrome raster names a band icon there.
    #[must_use]
    pub fn chrome_icon_key(
        &self,
        raster: Option<&ChromeRaster>,
        cells: &[RenderCell],
        c: usize,
    ) -> Option<GlyphKey> {
        let icon = raster?.icon_at(c)?;
        let cell = cells.get(c)?;
        let style = if cell.bold {
            StyleBits::BOLD
        } else {
            StyleBits::REGULAR
        };
        Some(band_icon_key(
            icon,
            style,
            chrome_icon_spills(icon, cells, c),
            self.px_q,
        ))
    }

    /// The room a chrome row that keeps its words clear of its rail ([`ChromeRaster::clear_rail`]) has in one cell.
    pub fn chrome_room_for(&mut self, input: &RenderInput) -> (usize, usize) {
        if !input.chrome_rasters.iter().any(|m| m.clear_rail) {
            return (0, 0);
        }
        self.ascii_ink_room()
    }

    /// The same `(head, foot)` room at this renderer's current size, asked before a frame exists.
    pub fn band_room(&mut self) -> (usize, usize) {
        self.ascii_ink_room()
    }

    fn ascii_ink_room(&mut self) -> (usize, usize) {
        if let Some((h, b, room)) = self.chrome_room_memo
            && h == self.cell_h
            && b == self.baseline
        {
            return room;
        }
        let (baseline, cell_h) = (self.baseline, self.cell_h as i32);
        let (mut top, mut bottom) = (cell_h, 0i32);
        for ch in '\u{21}'..='\u{7E}' {
            let regular = self.glyph_key(ch);
            let bold = self.glyph_key_styled(ch, StyleBits::BOLD);
            for key in [regular, bold] {
                if let Some((h, ymin)) = self.glyph_ink_box_cached(key) {
                    let h = i32::try_from(h).unwrap_or(i32::MAX);
                    top = top.min(baseline - (ymin + h));
                    bottom = bottom.max(baseline - ymin);
                }
            }
        }
        let head = usize::try_from(top.clamp(0, cell_h)).unwrap_or(0);
        let foot = usize::try_from((cell_h - bottom).clamp(0, cell_h)).unwrap_or(0);
        let room = (head, foot.min(self.cell_h - head));
        self.chrome_room_memo = Some((self.cell_h, self.baseline, room));
        room
    }

    /// The rasterized image for `key`, cached.
    #[inline]
    pub fn glyph_image(&mut self, key: GlyphKey) -> &GlyphImage {
        &self.cached_glyph(key).img
    }

    fn cached_glyph(&mut self, key: GlyphKey) -> &CachedGlyph {
        if !self.glyphs.contains_key(&key) {
            let mut img = self.rasterize(key);
            seat_ink_on_the_cell_floor(&mut img, self.baseline, self.cell_h);
            self.glyph_bytes = self.glyph_bytes.saturating_add(img.byte_len());
            self.glyphs.insert(key, CachedGlyph::new(img));
        }
        let entry = &self.glyphs[&key];
        entry.used.set(self.glyph_frame);
        entry
    }

    /// [`glyph_ink_box`] for `key`'s raster, read from the memo the cache entry carries (computed once when the entry is built, dropped with it).
    #[inline]
    pub fn glyph_ink_box_cached(&mut self, key: GlyphKey) -> Option<(usize, i32)> {
        self.cached_glyph(key).ink
    }

    /// Pre-rasterize printable ASCII (U+0020..=U+007E), regular and bold, so the first frame's atlas build pulls warm `GlyphImage`s.
    pub fn prewarm_ascii(&mut self) {
        for ch in '\u{20}'..='\u{7E}' {
            let key = self.glyph_key(ch);
            let _ = self.glyph_image(key);
            let bold = self.glyph_key_styled(ch, StyleBits::BOLD);
            let _ = self.glyph_image(bold);
        }
    }

    fn rasterize_ligature_slice(&mut self, key: GlyphKey, k: u16) -> GlyphImage {
        let cell_w = self.cell_w;
        let gid = key.ch_or_id as u16;
        let parent = GlyphKey::mono_gid(gid, key.style, key.px_q);
        let img = self.glyph_image(parent);
        let (pw, ph, pxmin, pymin) = (img.width(), img.height(), img.xmin(), img.ymin());
        let pbytes = match img {
            GlyphImage::Mono { bytes, .. } => bytes.as_slice(),
            GlyphImage::Rgba { .. } => &[],
        };
        let bytes =
            ligature_shaping::extract_cell_slice(pbytes, pw, ph, pxmin, cell_w, i64::from(k));
        GlyphImage::Mono {
            width: cell_w,
            height: ph,
            xmin: 0,
            ymin: pymin,
            advance: cell_w as f32,
            bytes,
        }
    }

    /// A small colour emoji ([`small_emoji_keyed`]): its full raster resampled to the rows
    /// [`Script::small_emoji_rows`](aterm_core::terminal::Script::small_emoji_rows) names, by the
    /// share of the cell they are, and centred in the same two-cell box. Its key keeps the colour
    /// face's index where a script key keeps its nudge, so no nudge is read from it.
    fn rasterize_small_emoji(&mut self, key: GlyphKey) -> GlyphImage {
        let twin = GlyphKey {
            style: key.style.without_script(),
            ..key
        };
        let (baseline, cell_h) = (self.baseline, self.cell_h);
        let (box_w, _) = self.color_emoji_box();
        let (top, rows) = aterm_core::terminal::Script::small_emoji_rows(cell_h);
        let full = self.glyph_image(twin).clone();
        let GlyphImage::Rgba {
            width,
            height,
            ymin,
            advance,
            ref bytes,
            ..
        } = full
        else {
            return full;
        };
        if width == 0 || height == 0 || cell_h == 0 || rows == 0 {
            return full;
        }
        let k = rows as f32 / cell_h as f32;
        let dw = ((width as f32 * k).round() as usize).max(1);
        let dh = ((height as f32 * k).round() as usize).clamp(1, rows);
        // The full raster's top below the cell's, scaled into the small rows.
        let inset = (baseline - ymin - height as i32).max(0) as f32;
        let at = (top as i32 + (inset * k).round() as i32).min((top + rows - dh) as i32);
        GlyphImage::Rgba {
            width: dw,
            height: dh,
            xmin: (box_w.saturating_sub(dw) / 2) as i32,
            ymin: baseline - dh as i32 - at,
            advance,
            bytes: resample_rgba(bytes, width, height, dw, dh),
        }
    }

    fn rasterize_script(&mut self, key: GlyphKey) -> GlyphImage {
        if matches!(
            key.glyph_class,
            GlyphClass::Rgba | GlyphClass::RgbaGid | GlyphClass::RgbaCluster
        ) {
            return self.rasterize_small_emoji(key);
        }
        let nudge = ((key.ch_or_id >> SUB_NUDGE_SHIFT) as u8).cast_signed();
        let twin = GlyphKey {
            style: key.style.without_script(),
            ch_or_id: key.ch_or_id & ((1 << SUB_NUDGE_SHIFT) - 1),
            ..key
        };
        let shift = f32::from(nudge) * self.cell_w as f32
            / f32::from(aterm_core::terminal::Script::NUDGES_PER_CELL);
        let (baseline, cell_h) = (self.baseline, self.cell_h);
        match self.glyph_image(twin) {
            GlyphImage::Mono {
                width,
                height,
                xmin,
                ymin,
                advance,
                bytes,
            } => {
                let (scale, x, y) =
                    script_drop((*xmin, *ymin, *width, *height), *advance, baseline, cell_h);
                match resample_mask_placed(bytes, *width, *height, scale, x + shift, y) {
                    Some((bytes, w, h, left, top)) => GlyphImage::Mono {
                        width: w,
                        height: h,
                        xmin: left,
                        ymin: baseline - top - h as i32,
                        advance: *advance,
                        bytes,
                    },
                    None => GlyphImage::Mono {
                        width: *width,
                        height: *height,
                        xmin: *xmin,
                        ymin: *ymin,
                        advance: *advance,
                        bytes: bytes.clone(),
                    },
                }
            }
            rgba @ GlyphImage::Rgba { .. } => rgba.clone(),
        }
    }

    fn rasterize(&mut self, key: GlyphKey) -> GlyphImage {
        debug_assert_eq!(
            key.px_q, self.px_q,
            "GlyphKey.px_q must match the renderer's own size this slice"
        );
        if key.style.script() != StyleBits::REGULAR {
            return self.rasterize_script(key);
        }
        match key.glyph_class {
            GlyphClass::Mono => {
                if key.source == FaceId::Procedural && key.ch_or_id & REGIONAL_PAIR_BIT != 0 {
                    return self.rasterize_regional_pair(key);
                }
                if key.source == FaceId::Procedural
                    && key.ch_or_id & CHROME_ICON_BIT != 0
                    && let Some(icon) = BandIcon::from_index(
                        key.ch_or_id & !(CHROME_ICON_BIT | CHROME_ICON_SPILL_BIT),
                    )
                {
                    let spill = key.ch_or_id & CHROME_ICON_SPILL_BIT != 0;
                    let m = self.icon_metrics(key.style.contains(StyleBits::BOLD));
                    return GlyphImage::Mono {
                        width: procedural::band_icon_width(self.cell_w, spill),
                        height: self.cell_h,
                        xmin: if spill { -(self.cell_w as i32) } else { 0 },
                        ymin: self.baseline - self.cell_h as i32,
                        advance: self.cell_w as f32,
                        bytes: procedural::band_icon_coverage(
                            icon,
                            self.cell_w,
                            self.cell_h,
                            m,
                            spill,
                        ),
                    };
                }
                if key.source == FaceId::Procedural {
                    let cp = key.ch_or_id & !(SHADE_PHASE_X_BIT | SHADE_PHASE_Y_BIT);
                    let (phase_x, phase_y) = (
                        key.ch_or_id & SHADE_PHASE_X_BIT != 0,
                        key.ch_or_id & SHADE_PHASE_Y_BIT != 0,
                    );
                    if let Some(ch) = char::from_u32(cp)
                        && let Some(bytes) = procedural::coverage_phased(
                            ch,
                            self.cell_w,
                            self.cell_h,
                            phase_x,
                            phase_y,
                        )
                    {
                        return GlyphImage::Mono {
                            width: self.cell_w,
                            height: self.cell_h,
                            xmin: 0,
                            ymin: self.baseline - self.cell_h as i32,
                            advance: self.cell_w as f32,
                            bytes,
                        };
                    }
                    let span = usize::from(key.cell_span).clamp(1, 2);
                    if let Some(ch) = char::from_u32(cp)
                        && let Some(bytes) =
                            procedural::symbol_coverage(ch, self.cell_w, self.cell_h, span)
                    {
                        return GlyphImage::Mono {
                            width: self.cell_w * span,
                            height: self.cell_h,
                            xmin: 0,
                            ymin: self.baseline - self.cell_h as i32,
                            advance: (self.cell_w * span) as f32,
                            bytes,
                        };
                    }
                }
                let ch = key.chr().unwrap_or(char::REPLACEMENT_CHARACTER);
                if key.source == FaceId::ColorEmojiMono
                    && let Some(img) = self.rasterize_color_emoji_mono(ch)
                {
                    return img;
                }
                let mut synth_style = key.style;
                let styled_slot = if key.source == FaceId::Primary
                    && (key.style.contains(StyleBits::BOLD)
                        || key.style.contains(StyleBits::ITALIC))
                {
                    self.ensure_styled_faces();
                    self.styled_face_slot(ch, key.style)
                } else {
                    None
                };
                let mut layer_fit: Option<DisplayFaceFit> = None;
                let (gw, gh, gxmin, gymin, gadv, mut bytes): (
                    usize,
                    usize,
                    i32,
                    i32,
                    f32,
                    Vec<u8>,
                ) = if let Some((slot, remaining)) = styled_slot {
                    synth_style = remaining;
                    let (coords, var_slot, instanced_residual) =
                        self.styled_instance(slot, remaining);
                    let gid = self.styled_faces[slot]
                        .as_mut()
                        .and_then(|sf| sf.unicode_gid(ch));
                    let src = match (gid, self.styled_faces[slot].as_ref()) {
                        (Some(g), Some(sf)) => {
                            Some((sf.bytes.as_ptr() as usize, sf.bytes.clone(), sf.index, g))
                        }
                        _ => None,
                    };
                    #[cfg(target_os = "macos")]
                    let ct = src.clone().and_then(|(kp, b, idx, g)| {
                        let vars = coords.clone();
                        self.ct_glyph(
                            kp,
                            b,
                            idx,
                            g,
                            self.px,
                            vars.as_deref().unwrap_or(&[]),
                            var_slot,
                        )
                    });
                    // Linux declines an instanced face as it declines a varied primary.
                    #[cfg(any(all(unix, not(target_os = "macos")), windows))]
                    let ct: Option<(usize, usize, i32, i32, f32, Vec<u8>)> = {
                        #[cfg(unix)]
                        let src = src.clone().filter(|_| coords.is_none());
                        src.clone().and_then(|(kp, b, idx, g)| {
                            let vars = coords.clone();
                            self.hinted_glyph(
                                kp,
                                &b,
                                idx,
                                g,
                                vars.as_deref().unwrap_or(&[]),
                                var_slot,
                            )
                        })
                    };
                    #[cfg(not(any(unix, windows)))]
                    let ct: Option<(usize, usize, i32, i32, f32, Vec<u8>)> = {
                        let _ = var_slot;
                        None
                    };
                    // Where the native raster declined, the instance's outline, never the
                    // `fvar` default fontdue would draw.
                    let ct = ct.or_else(|| {
                        let (_, bytes, index, gid) = src.as_ref()?;
                        variation::varied_glyph_raster(
                            bytes,
                            *index,
                            coords.as_deref()?,
                            *gid,
                            self.px,
                        )
                    });
                    if ct.is_some() && coords.is_some() {
                        synth_style = instanced_residual;
                    }
                    let (gw, gh, gxmin, gymin, gadv, bytes) = if let Some(t) = ct {
                        t
                    } else {
                        let (m, b) = match self.styled_faces[slot]
                            .as_ref()
                            .and_then(StyledFace::parsed)
                        {
                            Some(font) => match gid {
                                Some(g) => font.rasterize_indexed(g, self.px),
                                None => font.rasterize(ch, self.px),
                            },
                            None => self.font.rasterize(ch, self.px),
                        };
                        (m.width, m.height, m.xmin, m.ymin, m.advance_width, b)
                    };
                    let asterisk = self.styled_faces[slot]
                        .as_ref()
                        .and_then(|sf| sf.lowered_asterisk);
                    let (gxmin, gymin) =
                        self.lower_asterisk(asterisk, gid, (gw, gh, gxmin, gymin, &bytes));
                    (gw, gh, gxmin, gymin, gadv, bytes)
                } else if let Some(t) = self.vf_bold_mono_raster(ch, key) {
                    synth_style = StyleBits(synth_style.0 & !StyleBits::BOLD.0);
                    t
                } else if key.source == FaceId::DisplayMix {
                    let (fit, raw) = self.display_layer_raw_raster(ch);
                    layer_fit = fit;
                    raw
                } else if matches!(
                    key.source,
                    FaceId::Fallback | FaceId::SymbolFallback | FaceId::RuntimeFallback
                ) {
                    self.fallback_mono_raster(key.source, ch)
                } else if let Some(t) = self.hinted_primary_char(key, ch) {
                    t
                } else if key.source == FaceId::Primary
                    && let Some(t) = self.vf_primary_char_raster(ch)
                {
                    t
                } else {
                    let face = match key.source {
                        FaceId::Primary
                        | FaceId::Procedural
                        | FaceId::ColorEmoji
                        | FaceId::ColorEmojiMono
                        | FaceId::Fallback
                        | FaceId::SymbolFallback
                        | FaceId::RuntimeFallback
                        | FaceId::DisplayMix => &self.font,
                        FaceId::BoldPrimary => self.bold_font.as_ref().unwrap_or(&self.font),
                    };
                    let (m, b) = face.rasterize(ch, self.px);
                    (m.width, m.height, m.xmin, m.ymin, m.advance_width, b)
                };
                stem_darken(&mut bytes, &self.stem_lut);
                let synth_style = if self.synthetic_styles {
                    synth_style
                } else {
                    StyleBits::REGULAR
                };
                let synth_style =
                    if self.synthetic_styles && layer_fit.is_some_and(|fit| fit.embolden) {
                        StyleBits(synth_style.0 | StyleBits::BOLD.0)
                    } else {
                        synth_style
                    };
                let defer_italic = harmonized_fallback_source(key.source)
                    && key.cell_span > 0
                    && synth_style.contains(StyleBits::ITALIC);
                let fit = fallback_fit_class(ch);
                let defer_bold = harmonized_fallback_source(key.source)
                    && key.cell_span > 0
                    && fit == FallbackFit::Symbol
                    && synth_style.contains(StyleBits::BOLD);
                let mut pre_style = synth_style;
                if defer_italic {
                    pre_style = StyleBits(pre_style.0 & !StyleBits::ITALIC.0);
                }
                if defer_bold {
                    pre_style = StyleBits(pre_style.0 & !StyleBits::BOLD.0);
                }
                let (width, bytes) = apply_synthetic_style(pre_style, gw, gh, bytes, self.px);
                let xmin = gxmin;
                let (width, height, xmin, ymin, advance, bytes) = if let Some(layer) = layer_fit {
                    self.fit_display_glyph(
                        layer,
                        usize::from(key.cell_span),
                        (width, gh, xmin, gymin, gadv, bytes),
                    )
                } else if harmonized_fallback_source(key.source)
                    || (key.cell_span > 0
                        && matches!(key.source, FaceId::Primary | FaceId::BoldPrimary))
                {
                    self.harmonize_fallback_raster(
                        usize::from(key.cell_span),
                        (width, gh, xmin, gymin, gadv, bytes),
                        DeferredStyle {
                            bold_px: if defer_bold {
                                synthetic_bold_px(self.px)
                            } else {
                                0
                            },
                            italic: defer_italic,
                        },
                        fit,
                    )
                } else {
                    (width, gh, xmin, gymin, gadv, bytes)
                };
                let (width, height, xmin, ymin, bytes) = if self.keeps_faces_spinner_frame(key, ch)
                {
                    self.even_spinner_frame(key, ch, (width, height, xmin, ymin, bytes))
                } else {
                    (width, height, xmin, ymin, bytes)
                };
                GlyphImage::Mono {
                    width,
                    height,
                    xmin,
                    ymin,
                    advance,
                    bytes,
                }
            }
            GlyphClass::Rgba => {
                let ch = key.chr().unwrap_or(char::REPLACEMENT_CHARACTER);
                self.rasterize_color_emoji(key.colour_face(), ch)
                    .unwrap_or_else(empty_rgba)
            }
            GlyphClass::RgbaGid => {
                let gid = aterm_font::GlyphId(key.ch_or_id as u16);
                self.rasterize_color_emoji_gid(key.colour_face(), gid)
                    .unwrap_or_else(empty_rgba)
            }
            GlyphClass::RgbaCluster => self
                .rasterize_color_emoji_run(key.ch_or_id)
                .unwrap_or_else(empty_rgba),
            GlyphClass::MonoGid => {
                let slice_hi = key.ch_or_id >> LIG_SLICE_SHIFT;
                if slice_hi != 0 {
                    return self.rasterize_ligature_slice(key, (slice_hi - 1) as u16);
                }
                let gid = key.ch_or_id as u16;
                let pick = if key.source == FaceId::BoldPrimary {
                    FacePick::InjectedBold {
                        synthetic: key.style,
                    }
                } else if key.style == StyleBits::REGULAR {
                    FacePick::Primary
                } else {
                    self.run_face_pick(key.style)
                };
                // A styled or injected pick's instance and what it leaves to synthesize; the
                // primary's are picked in each arm below.
                let (face_vars, face_var_slot, instanced_synth): (VarCoords, u8, StyleBits) =
                    match &pick {
                        FacePick::Styled { slot, synthetic } => {
                            self.styled_instance(*slot, *synthetic)
                        }
                        FacePick::InjectedBold { synthetic } => {
                            let drawn = self.bold_font_instance.drawn.clone();
                            let var_slot = u8::from(drawn.is_some());
                            (drawn, var_slot, *synthetic)
                        }
                        FacePick::Primary => (None, 0, key.style),
                    };
                // Only CoreText and the Windows hinter take an instance slot.
                #[cfg(not(any(target_os = "macos", windows)))]
                let _ = face_var_slot;
                #[cfg(target_os = "macos")]
                let (ct, vf_bold_drawn) = {
                    let src: Option<(usize, std::sync::Arc<[u8]>, u32)> = match &pick {
                        FacePick::Primary => self
                            .shaping_primary_bytes
                            .clone()
                            .map(|b| (b.as_ptr() as usize, b, self.primary_index)),
                        FacePick::InjectedBold { .. } => self
                            .bold_font_bytes
                            .clone()
                            .map(|b| (b.as_ptr() as usize, b, self.bold_font_index)),
                        FacePick::Styled { slot, .. } => self.styled_faces[*slot]
                            .as_ref()
                            .map(|sf| (sf.bytes.as_ptr() as usize, sf.bytes.clone(), sf.index)),
                    };
                    let mut vf_bold = false;
                    let ct = src.and_then(|(kp, b, idx)| {
                        let (vars, slot): (VarCoords, u8) = match &pick {
                            FacePick::Primary
                                if key.style.contains(StyleBits::BOLD)
                                    && self.vf_bold_coords.is_some() =>
                            {
                                vf_bold = true;
                                (self.vf_bold_coords.clone(), 2)
                            }
                            FacePick::Primary if self.var_coords.is_some() => {
                                (self.var_coords.clone(), 1)
                            }
                            FacePick::Primary => (None, 0),
                            FacePick::InjectedBold { .. } | FacePick::Styled { .. } => {
                                (face_vars.clone(), face_var_slot)
                            }
                        };
                        self.ct_glyph(
                            kp,
                            b,
                            idx,
                            gid,
                            self.px,
                            vars.as_deref().unwrap_or(&[]),
                            slot,
                        )
                    });
                    let ct = ct.or_else(|| {
                        vf_bold = matches!(pick, FacePick::Primary)
                            && key.style.contains(StyleBits::BOLD);
                        vf_bold.then(|| self.vf_bold_gid_raster(gid)).flatten()
                    });
                    let drawn = vf_bold && ct.is_some();
                    (ct, drawn)
                };
                #[cfg(all(unix, not(target_os = "macos")))]
                let (ct, vf_bold_drawn) = if let Some(t) = (matches!(pick, FacePick::Primary)
                    && key.style.contains(StyleBits::BOLD))
                .then(|| self.vf_bold_gid_raster(gid))
                .flatten()
                {
                    (Some(t), true)
                } else {
                    let src: Option<(usize, std::sync::Arc<[u8]>, u32)> = match &pick {
                        FacePick::Primary if self.var_coords.is_none() => self
                            .shaping_primary_bytes
                            .clone()
                            .map(|b| (b.as_ptr() as usize, b, self.primary_index)),
                        FacePick::Primary => None,
                        // An instanced face declines as a varied primary does: the
                        // instanced outline raster below keeps its cells.
                        FacePick::InjectedBold { .. } | FacePick::Styled { .. }
                            if face_vars.is_some() =>
                        {
                            None
                        }
                        FacePick::InjectedBold { .. } => self
                            .bold_font_bytes
                            .clone()
                            .map(|b| (b.as_ptr() as usize, b, self.bold_font_index)),
                        FacePick::Styled { slot, .. } => self.styled_faces[*slot]
                            .as_ref()
                            .map(|sf| (sf.bytes.as_ptr() as usize, sf.bytes.clone(), sf.index)),
                    };
                    let hinted =
                        src.and_then(|(kp, b, idx)| self.hinted_glyph(kp, &b, idx, gid, &[], 0));
                    (hinted, false)
                };
                #[cfg(not(unix))]
                #[allow(clippy::type_complexity)]
                let (ct, vf_bold_drawn): (
                    Option<(usize, usize, i32, i32, f32, Vec<u8>)>,
                    bool,
                ) = {
                    let bold_instance = matches!(pick, FacePick::Primary)
                        && key.style.contains(StyleBits::BOLD)
                        && self.vf_bold_coords.is_some();
                    #[cfg(windows)]
                    let hinted = {
                        let src: Option<(usize, std::sync::Arc<[u8]>, u32)> = match &pick {
                            FacePick::Primary => self
                                .shaping_primary_bytes
                                .clone()
                                .map(|b| (b.as_ptr() as usize, b, self.primary_index)),
                            FacePick::InjectedBold { .. } => self
                                .bold_font_bytes
                                .clone()
                                .map(|b| (b.as_ptr() as usize, b, self.bold_font_index)),
                            FacePick::Styled { slot, .. } => self.styled_faces[*slot]
                                .as_ref()
                                .map(|sf| (sf.bytes.as_ptr() as usize, sf.bytes.clone(), sf.index)),
                        };
                        src.and_then(|(kp, b, idx)| {
                            let (vars, slot): (VarCoords, u8) = match &pick {
                                FacePick::Primary if bold_instance => {
                                    (self.vf_bold_coords.clone(), 2)
                                }
                                FacePick::Primary if self.var_coords.is_some() => {
                                    (self.var_coords.clone(), 1)
                                }
                                FacePick::Primary => (None, 0),
                                FacePick::InjectedBold { .. } | FacePick::Styled { .. } => {
                                    (face_vars.clone(), face_var_slot)
                                }
                            };
                            self.hinted_glyph(
                                kp,
                                &b,
                                idx,
                                gid,
                                vars.as_deref().unwrap_or(&[]),
                                slot,
                            )
                        })
                    };
                    #[cfg(not(windows))]
                    let hinted: Option<(usize, usize, i32, i32, f32, Vec<u8>)> = None;
                    let ct = hinted.or_else(|| {
                        bold_instance
                            .then(|| self.vf_bold_gid_raster(gid))
                            .flatten()
                    });
                    let drawn = bold_instance && ct.is_some();
                    (ct, drawn)
                };
                let mut face_instanced =
                    ct.is_some() && face_vars.is_some() && !matches!(pick, FacePick::Primary);
                let (gw, gh, gxmin, gymin, gadv, mut bytes) = if let Some(t) = ct {
                    t
                } else if let Some(t) = matches!(pick, FacePick::Primary)
                    .then(|| self.vf_primary_gid_raster(gid))
                    .flatten()
                {
                    t
                } else if let Some(t) = face_vars.as_deref().and_then(|coords| {
                    let (bytes, index) = match &pick {
                        FacePick::Styled { slot, .. } => self.styled_faces[*slot]
                            .as_ref()
                            .map(|sf| (sf.bytes.clone(), sf.index))?,
                        FacePick::InjectedBold { .. } => {
                            (self.bold_font_bytes.clone()?, self.bold_font_index)
                        }
                        FacePick::Primary => return None,
                    };
                    variation::varied_glyph_raster(&bytes, index, coords, gid, self.px)
                }) {
                    face_instanced = true;
                    t
                } else {
                    if let FacePick::Styled { slot, .. } = pick
                        && !self.styled_faces[slot]
                            .as_ref()
                            .is_some_and(|sf| sf.parsed().is_some())
                    {
                        if let Some(id) = self.styled_faces[slot].as_ref().map(|sf| sf.id)
                            && self.styled_retire_pending[slot] != Some(id)
                        {
                            self.styled_retire_pending[slot] = Some(id);
                            self.font_epoch += 1;
                        }
                        return GlyphImage::Mono {
                            width: 0,
                            height: 0,
                            xmin: 0,
                            ymin: 0,
                            advance: 0.0,
                            bytes: Vec::new(),
                        };
                    }
                    let face = match &pick {
                        FacePick::InjectedBold { .. } => {
                            self.bold_font.as_ref().unwrap_or(&self.font)
                        }
                        FacePick::Styled { slot, .. } => self.styled_faces[*slot]
                            .as_ref()
                            .and_then(StyledFace::parsed)
                            .map_or(self.font.as_ref(), |f| f.as_ref()),
                        FacePick::Primary => &self.font,
                    };
                    let (m, b) = face.rasterize_indexed(gid, self.px);
                    (m.width, m.height, m.xmin, m.ymin, m.advance_width, b)
                };
                let asterisk = match &pick {
                    FacePick::Primary => self.primary_lowered_asterisk,
                    FacePick::InjectedBold { .. } => self.bold_lowered_asterisk,
                    FacePick::Styled { slot, .. } => self.styled_faces[*slot]
                        .as_ref()
                        .and_then(|sf| sf.lowered_asterisk),
                };
                let (gxmin, gymin) =
                    self.lower_asterisk(asterisk, Some(gid), (gw, gh, gxmin, gymin, &bytes));
                stem_darken(&mut bytes, &self.stem_lut);
                let synth = match pick {
                    FacePick::Primary => key.style,
                    FacePick::InjectedBold { .. } | FacePick::Styled { .. } if face_instanced => {
                        instanced_synth
                    }
                    FacePick::InjectedBold { synthetic } | FacePick::Styled { synthetic, .. } => {
                        synthetic
                    }
                };
                let synth = if vf_bold_drawn {
                    StyleBits(synth.0 & !StyleBits::BOLD.0)
                } else {
                    synth
                };
                let synth = if self.synthetic_styles {
                    synth
                } else {
                    StyleBits::REGULAR
                };
                let (width, bytes) = apply_synthetic_style(synth, gw, gh, bytes, self.px);
                let (width, height, xmin, ymin, advance, bytes) = if key.cell_span > 0 {
                    self.harmonize_fallback_raster(
                        usize::from(key.cell_span),
                        (width, gh, gxmin, gymin, gadv, bytes),
                        DeferredStyle::NONE,
                        FallbackFit::Symbol,
                    )
                } else {
                    (width, gh, gxmin, gymin, gadv, bytes)
                };
                GlyphImage::Mono {
                    width,
                    height,
                    xmin,
                    ymin,
                    advance,
                    bytes,
                }
            }
        }
    }

    fn rasterize_color_emoji_mono(&mut self, ch: char) -> Option<GlyphImage> {
        let pick = self.color_pick(ch)?;
        let bytes = self.color_face_bytes(pick)?;
        let face = aterm_font::Face::parse(bytes, 0).ok()?;
        let gid = face.glyph_index(ch)?;
        let box_w = self.cell_w.max(1);
        let box_h = self.cell_h.max(1);
        let raster = pick_glyph_raster(&face, gid, box_h)?;
        if !matches!(raster.format, aterm_font::RasterImageFormat::PNG) {
            return None;
        }
        let (src, src_w, src_h) = decode_png_rgba8(raster.data)?;

        let scale = (box_w as f32 / src_w as f32).min(box_h as f32 / src_h as f32);
        let dst_w = ((src_w as f32 * scale).round() as usize).clamp(1, box_w);
        let dst_h = ((src_h as f32 * scale).round() as usize).clamp(1, box_h);
        let rgba = resample_rgba(&src, src_w, src_h, dst_w, dst_h);
        let coverage: Vec<u8> = rgba.as_chunks::<4>().0.iter().map(|px| px[3]).collect();

        let xmin = ((box_w - dst_w) / 2) as i32;
        let top_inset = ((box_h as i32 - dst_h as i32) / 2).max(0);
        let ymin = self.baseline - dst_h as i32 - top_inset;
        Some(GlyphImage::Mono {
            width: dst_w,
            height: dst_h,
            xmin,
            ymin,
            advance: box_w as f32,
            bytes: coverage,
        })
    }

    fn rasterize_color_emoji(&mut self, face: u8, ch: char) -> Option<GlyphImage> {
        let bytes = self.color_face_bytes(face)?;
        let gid = aterm_font::Face::parse(bytes, 0).ok()?.glyph_index(ch)?;
        self.rasterize_color_emoji_gid(face, gid)
    }

    fn rasterize_color_emoji_gid(
        &mut self,
        face: u8,
        gid: aterm_font::GlyphId,
    ) -> Option<GlyphImage> {
        let bytes = self.color_face_bytes(face)?;
        let face = aterm_font::Face::parse(bytes, 0).ok()?;
        let (box_w, box_h) = self.color_emoji_box();
        let (rgba, w, h) = fit_color_glyph(&face, gid, box_w, box_h)?;
        Some(self.place_color_emoji(rgba, w, h))
    }

    fn rasterize_color_emoji_run(&mut self, id: u32) -> Option<GlyphImage> {
        let run = self.cluster_shapes.run(id)?;
        let bytes = self.color_face_bytes(run.face)?;
        let face = aterm_font::Face::parse(bytes, 0).ok()?;
        let (box_w, box_h) = self.color_emoji_box();
        if run
            .glyphs
            .iter()
            .all(|g| colr_only(&face, aterm_font::GlyphId(g.gid)))
        {
            let glyphs: Vec<(aterm_font::GlyphId, f32, f32)> = run
                .glyphs
                .iter()
                .map(|g| (aterm_font::GlyphId(g.gid), g.x as f32, g.y as f32))
                .collect();
            let rgba = colr::rasterize_colr_run(&face, &glyphs, box_w, box_h)?;
            return Some(self.place_color_emoji(rgba, box_w, box_h));
        }
        let (rgba, w, h) = emoji_cluster::composite(run, box_w, box_h, |gid, w, h| {
            fit_color_glyph(&face, aterm_font::GlyphId(gid), w, h)
        })?;
        Some(self.place_color_emoji(rgba, w, h))
    }

    fn rasterize_regional_pair(&mut self, key: GlyphKey) -> GlyphImage {
        let index = |shift: u32| (key.ch_or_id >> shift) & 0x1F;
        let letters = [index(5), index(0)].map(|i| char::from_u32(u32::from('A') + i));
        let cell_w = i32::try_from(self.cell_w).unwrap_or(i32::MAX);
        let mut placed = Vec::new();
        for (cell, letter) in (0i32..).zip(letters) {
            let Some(letter) = letter else { continue };
            let letter_key = self.glyph_key_styled(letter, key.style);
            if let GlyphImage::Mono {
                width,
                height,
                xmin,
                ymin,
                bytes,
                ..
            } = self.glyph_image(letter_key)
            {
                placed.push((cell * cell_w + *xmin, *ymin, *width, *height, bytes.clone()));
            }
        }
        let width = 2 * self.cell_w;
        let top = placed.iter().map(|&(_, y, _, h, _)| y + h as i32).max();
        let floor = placed.iter().map(|&(_, y, ..)| y).min();
        let (Some(top), Some(floor)) = (top, floor) else {
            return GlyphImage::Mono {
                width: 0,
                height: 0,
                xmin: 0,
                ymin: 0,
                advance: width as f32,
                bytes: Vec::new(),
            };
        };
        let height = usize::try_from(top - floor).unwrap_or(0);
        let mut out = vec![0u8; width * height];
        for (x0, y, w, h, bytes) in placed {
            let row0 = usize::try_from(top - (y + h as i32)).unwrap_or(0);
            for r in 0..h {
                for c in 0..w {
                    let Ok(x) = usize::try_from(x0 + c as i32) else {
                        continue;
                    };
                    if x < width {
                        let dst = &mut out[(row0 + r) * width + x];
                        *dst = (*dst).max(bytes[r * w + c]);
                    }
                }
            }
        }
        GlyphImage::Mono {
            width,
            height,
            xmin: 0,
            ymin: floor,
            advance: width as f32,
            bytes: out,
        }
    }

    fn color_emoji_box(&self) -> (usize, usize) {
        ((2 * self.cell_w).max(1), self.cell_h.max(1))
    }

    fn place_color_emoji(&self, bytes: Vec<u8>, width: usize, height: usize) -> GlyphImage {
        let (box_w, box_h) = self.color_emoji_box();
        let xmin = (box_w.saturating_sub(width) / 2) as i32;
        let top_inset = ((box_h as i32 - height as i32) / 2).max(0);
        GlyphImage::Rgba {
            width,
            height,
            xmin,
            ymin: self.baseline - height as i32 - top_inset,
            advance: box_w as f32,
            bytes,
        }
    }

    fn evict_glyph_caches_if_large(&mut self) {
        const GLYPH_CACHE_CAP: usize = 16_384;
        const GLYPH_CACHE_BYTES: usize = 12 * 1024 * 1024;
        const SHAPED_RUN_CAP: usize = 16_384;
        const CLUSTER_CAP: usize = 16_384;
        let (kept_entries, kept_bytes) = self.glyph_kept;
        if self.glyphs.len() > GLYPH_CACHE_CAP.max(kept_entries.saturating_mul(2))
            || self.glyph_bytes > GLYPH_CACHE_BYTES.max(kept_bytes.saturating_mul(2))
        {
            self.retain_glyph_working_set();
            self.keys.clear();
            self.emoji_keys.clear();
            self.styled_keys.clear();
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                let kept = &self.glyphs;
                self.subpx_glyphs.retain(|key, _| kept.contains_key(key));
            }
            self.deco_masks.borrow_mut().clear();
        }
        const CHAR_MEMO_CAP: usize = 65_536;
        if self.primary_gid_cache.len() > CHAR_MEMO_CAP {
            self.primary_gid_cache.clear();
        }
        if self.bold_gid_cache.len() > CHAR_MEMO_CAP {
            self.bold_gid_cache.clear();
        }
        if self.fallback_pick.len() > CHAR_MEMO_CAP {
            self.fallback_pick.clear();
        }
        if self.shaped_runs.cur_len() > SHAPED_RUN_CAP {
            self.shaped_runs.rotate();
        }
        if self.cluster_shapes.cur_len() > CLUSTER_CAP {
            self.cluster_shapes.rotate();
        }
    }

    fn begin_frame_boundary(&mut self) {
        self.evict_glyph_caches_if_large();
        self.glyph_frame = self.glyph_frame.wrapping_add(1);
        self.apply_pending_styled_retires();
    }

    /// The GPU backend's crossing of [`begin_frame_boundary`](Self::begin_frame_boundary).
    pub fn evict_caches_if_large(&mut self) {
        self.begin_frame_boundary();
    }

    /// The most draw-and-settle passes an owned-frame snapshot ([`Self::render_input`] and its GPU twin) takes.
    pub const SNAPSHOT_SETTLE_PASSES: usize = 4;

    /// Render an owned snapshot through [`Self::try_render_input`].
    pub fn render_input(&mut self, input: &RenderInput) -> Frame {
        self.try_render_input(input)
            .expect("renderable framebuffer")
    }

    /// Render an owned snapshot, rejecting excessive dimensions or a failed framebuffer reservation before drawing.
    pub fn try_render_input(&mut self, input: &RenderInput) -> Result<Frame, String> {
        let mut wc = WindowCpu::new();
        let mut pass = 1;
        loop {
            let epoch = self.font_epoch;
            let _ = self.try_render_input_impl(&mut wc, input, false)?;
            let _ = self.settle_in_flight_fallback_parses();
            if self.font_epoch == epoch || pass == Self::SNAPSHOT_SETTLE_PASSES {
                break;
            }
            pass += 1;
        }
        let _ = self.present_rendered_frame(&mut wc, input, false);
        Ok(wc.into_presented_frame())
    }

    /// Render a pre-extracted snapshot like [`render_input`](Self::render_input) but return a [`RenderView`] borrowing the renderer's persistent damage cache.
    pub fn render_input_cached<'a>(
        &mut self,
        wc: &'a mut WindowCpu,
        input: &RenderInput,
    ) -> RenderView<'a> {
        self.try_render_input_cached(wc, input)
            .expect("renderable framebuffer")
    }

    /// Render into the retained cache.
    pub fn try_render_input_cached<'a>(
        &mut self,
        wc: &'a mut WindowCpu,
        input: &RenderInput,
    ) -> Result<RenderView<'a>, String> {
        self.try_render_input_impl(wc, input, true)
    }

    fn try_render_input_impl<'a>(
        &mut self,
        wc: &'a mut WindowCpu,
        input: &RenderInput,
        preserve_cache: bool,
    ) -> Result<RenderView<'a>, String> {
        let (w, h) = self.checked_frame_size(input.rows, input.cols)?;
        let len = w * h;
        let mut new_pixels = Vec::new();
        let pixels = match wc.cache.as_mut() {
            Some(cache) => &mut cache.pixels,
            None => &mut new_pixels,
        };
        pixels
            .try_reserve_exact(len.saturating_sub(pixels.len()))
            .map_err(|e| format!("cannot reserve framebuffer: {e}"))?;
        let (y0, y1) = scroll_translate::grid_band_px(
            self.grid_top(),
            self.cell_h,
            input.grid_top_row,
            input.grid_bot_row,
            h,
        );
        if preserve_cache && input.scroll_frac_px != 0 && y0 < y1 {
            wc.present_scratch
                .try_reserve_exact(len.saturating_sub(wc.present_scratch.len()))
                .map_err(|e| format!("cannot reserve translated framebuffer: {e}"))?;
        }
        if wc.cache.is_none() {
            wc.cache = Some(RenderCache {
                pixels: new_pixels,
                width: 0,
                height: 0,
                grid_top: 0,
                input: RenderInput::empty(),
                cursor_blink_phase: self.cursor_blink_phase,
                cursor_style_override: self.cursor_style_override,
                font_epoch: self.font_epoch,
            });
        }
        let (w, h) = self.render_core(wc, input);
        if !preserve_cache {
            return Ok(Self::cached_view(wc, w, h));
        }
        Ok(self.present_rendered_frame(wc, input, true))
    }

    fn present_rendered_frame<'a>(
        &mut self,
        wc: &'a mut WindowCpu,
        input: &RenderInput,
        preserve_cache: bool,
    ) -> RenderView<'a> {
        let cache = wc.cache.as_ref().expect("rendered cache");
        let (w, h) = (cache.width, cache.height);
        let (y0, y1) = scroll_translate::grid_band_px(
            self.grid_top(),
            self.cell_h,
            input.grid_top_row,
            input.grid_bot_row,
            h,
        );
        let frac = i64::from(input.scroll_frac_px);
        let apron_ready = scroll_translate::incoming_row_applies(input)
            && scroll_translate::incoming_strip(y0, y1, frac).is_some()
            && self.rasterize_apron_row(input, &mut wc.apron);
        if !preserve_cache {
            let pixels = &mut wc.cache.as_mut().expect("rendered cache").pixels;
            scroll_translate::translate_grid_band_in_place(pixels, w, y0, y1, frac);
            if apron_ready && wc.apron.width() == w {
                scroll_translate::paint_incoming_strip(pixels, w, y0, y1, frac, wc.apron.band());
            }
            wc.last_translated = false;
            return Self::cached_view(wc, w, h);
        }
        Self::present_view(wc, w, h, y0, y1, frac, apron_ready)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AsymmetricPadLayout",
            action = "RenderWithLayoutCache",
            project = "aterm_render::project_asymmetric_pad_layout"
        )
    )]
    fn render_core(&mut self, wc: &mut WindowCpu, input: &RenderInput) -> (usize, usize) {
        self.begin_frame_boundary();
        let cell = (self.cell_w, self.cell_h);
        wc.image_cache
            .begin_frame(input.images.iter().flatten().map(|(_, image)| {
                let (fp_w, fp_h) = placement_footprint(&image.image, cell);
                (&image.image, fp_w, fp_h)
            }));
        self.poll_fallback_parses();
        self.chrome_room = self.chrome_room_for(input);
        self.glyph_plan_row = None;
        self.bg_runs_total.set(0);
        self.bg_runs_at_base.set(0);
        self.bg_fills_emitted.set(0);
        let (rows, cols) = (input.rows, input.cols);
        let (w, h) = self.frame_size(rows, cols);
        ensure_wallpaper_px(wc, input, w, h);
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            self.subpx_frame = self.input_uses_lcd_text(input);
        }

        let mut dirty = std::mem::take(&mut wc.dirty_scratch);
        let mut shift = std::mem::take(&mut wc.shift_scratch);
        let mut scroll_delta: Option<i32> = None;
        let decision = match &wc.cache {
            Some(c)
                if c.width == w
                    && c.height == h
                    && c.grid_top == self.grid_top()
                    && c.font_epoch == self.font_epoch =>
            {
                let d = compute_dirty_rows(
                    &c.input,
                    input,
                    c.cursor_blink_phase,
                    c.cursor_style_override,
                    self.cursor_blink_phase,
                    self.cursor_style_override,
                    self.row_bands(),
                    &mut dirty,
                );
                let baseline = match &d {
                    DirtyDecision::FullRepaint => rows,
                    DirtyDecision::Rows(_) => dirty.iter().filter(|&&b| b).count(),
                };
                let gate_hit = matches!(&d, DirtyDecision::Rows(r) if r.is_gate_hit());
                if !gate_hit
                    && let Some(delta) = scroll_shift_plan(
                        &c.input,
                        input,
                        c.cursor_blink_phase,
                        c.cursor_style_override,
                        self.cursor_blink_phase,
                        self.cursor_style_override,
                        self.row_bands(),
                        RetainedRows::Diff,
                        &mut shift,
                    )
                {
                    let n = shift.iter().filter(|&&b| b).count();
                    if n < baseline || matches!(&d, DirtyDecision::FullRepaint) {
                        scroll_delta = Some(delta);
                        std::mem::swap(&mut dirty, &mut shift);
                    }
                }
                d
            }
            _ => DirtyDecision::FullRepaint,
        };
        wc.shift_scratch = shift;
        let dirty_rows = match decision {
            DirtyDecision::FullRepaint if scroll_delta.is_none() => {
                wc.dirty_scratch = dirty;
                wc.last_damage = DamageOutcome::Full;
                self.full_render(wc, input, w, h);
                return (w, h);
            }
            DirtyDecision::FullRepaint => DirtyRows::rows_only(),
            DirtyDecision::Rows(_) if scroll_delta.is_some() => DirtyRows::rows_only(),
            DirtyDecision::Rows(d) => d,
        };

        let mut cache = wc.cache.take().expect("reusable implies Some");
        debug_assert_eq!(cache.pixels.len(), w * h);

        if let Some(delta) = scroll_delta {
            let y0 = self.grid_top().min(h);
            let y1 = (self.grid_top() + rows.saturating_mul(self.cell_h)).min(h);
            scroll_translate::translate_grid_band_in_place(
                &mut cache.pixels,
                w,
                y0,
                y1,
                i64::from(delta) * self.cell_h as i64,
            );
        }

        let gate_hit = dirty_rows.is_gate_hit();
        debug_assert_eq!(
            gate_hit,
            is_unchanged_frame(
                &cache.input,
                cache.cursor_blink_phase,
                cache.cursor_style_override,
                input,
                self.cursor_blink_phase,
                self.cursor_style_override,
                self.row_bands(),
            ),
            "DirtyRows::is_gate_hit must agree with is_unchanged_frame"
        );
        if gate_hit {
            wc.cache = Some(cache);
            wc.dirty_scratch = dirty;
            wc.last_damage = DamageOutcome::GateHit;
            return (w, h);
        }
        if scroll_delta.is_none() {
            self.mark_overshoot_rows(&cache.input, input, &mut dirty);
        }
        self.overshoot_surprise = false;

        let band_bg = self.frame_bg(input) | (self.bg_transmittance() << 24);
        let base = wc.wallpaper_px.as_ref().map(|p| p.px.as_slice());
        self.composite_free(
            &mut wc.image_cache,
            &mut cache.pixels,
            w,
            h,
            input,
            dirty
                .iter()
                .enumerate()
                .filter_map(|(r, &d)| d.then_some(r)),
            Some(band_bg),
            base,
            Some(&dirty),
        );
        let mut apron_band = std::mem::take(&mut wc.apron_band);
        for r in 0..rows.saturating_sub(1) {
            if dirty[r] && !dirty[r + 1] && self.row_may_ink_above(input, r + 1) {
                self.draw_lower_apron(
                    &mut wc.image_cache,
                    &mut cache.pixels,
                    w,
                    h,
                    input,
                    r,
                    &mut apron_band,
                );
            }
        }
        wc.apron_band = apron_band;
        if std::mem::take(&mut self.overshoot_surprise) {
            wc.cache = Some(cache);
            wc.dirty_scratch = dirty;
            wc.last_damage = DamageOutcome::Full;
            self.full_render(wc, input, w, h);
            return (w, h);
        }
        self.draw_glow(&mut cache.pixels, w, h, input, Some(&dirty));
        self.draw_fx(
            &mut cache.pixels,
            w,
            h,
            input,
            FxSlot::Glow,
            FxBlend::Exact,
            Some(&dirty),
        );
        self.draw_glow_halo(&mut cache.pixels, w, h, input, Some(&dirty));
        self.draw_nova(&mut cache.pixels, w, h, input, Some(&dirty));
        self.draw_fx(
            &mut cache.pixels,
            w,
            h,
            input,
            FxSlot::Over,
            FxBlend::Exact,
            Some(&dirty),
        );
        self.draw_rain_add(&mut cache.pixels, w, h, input, Some(&dirty));
        self.draw_decorations(&mut cache.pixels, w, h, input, Some(&dirty));
        #[cfg(any(test, debug_assertions))]
        self.stamp_legacy_sprites(
            &mut cache.pixels,
            w,
            h,
            input,
            dirty
                .iter()
                .enumerate()
                .filter_map(|(r, &d)| d.then_some(r)),
            false,
        );
        self.draw_fx(
            &mut cache.pixels,
            w,
            h,
            input,
            FxSlot::Over,
            FxBlend::Linear,
            Some(&dirty),
        );
        self.draw_cursor(&mut cache.pixels, w, h, input);

        clone_damage_cache_from_stamped(&mut cache.input, input);
        cache.cursor_blink_phase = self.cursor_blink_phase;
        cache.cursor_style_override = self.cursor_style_override;
        wc.cache = Some(cache);
        wc.dirty_scratch = dirty;
        wc.last_damage = match scroll_delta {
            Some(delta_rows) => DamageOutcome::Scroll { delta_rows },
            None => DamageOutcome::Rows,
        };
        (w, h)
    }

    fn present_view(
        wc: &mut WindowCpu,
        w: usize,
        h: usize,
        y0: usize,
        y1: usize,
        frac: i64,
        apron_ready: bool,
    ) -> RenderView<'_> {
        if frac == 0 || y0 >= y1 {
            wc.last_translated = false;
            return Self::cached_view(wc, w, h);
        }
        wc.last_translated = true;
        let WindowCpu {
            cache,
            present_scratch,
            apron,
            ..
        } = wc;
        {
            let src = &cache
                .as_ref()
                .expect("render_core populated the cache")
                .pixels;
            let mag = frac.unsigned_abs() as usize;
            let moved = (y1 - y0).saturating_sub(mag);
            present_scratch.clear();
            if frac > 0 {
                present_scratch.extend_from_slice(&src[..y0 * w]);
                present_scratch.extend_from_slice(&src[(y1 - moved) * w..y1 * w]);
                present_scratch.extend_from_slice(&src[(y0 + moved) * w..]);
            } else {
                present_scratch.extend_from_slice(&src[..(y1 - moved) * w]);
                present_scratch.extend_from_slice(&src[y0 * w..(y0 + moved) * w]);
                present_scratch.extend_from_slice(&src[y1 * w..]);
            }
        }
        if apron_ready && apron.width() == w {
            scroll_translate::paint_incoming_strip(present_scratch, w, y0, y1, frac, apron.band());
        }
        RenderView::Borrowed {
            width: w,
            height: h,
            pixels: present_scratch,
        }
    }

    fn cached_view(wc: &WindowCpu, w: usize, h: usize) -> RenderView<'_> {
        let pixels = &wc.cache.as_ref().expect("cache populated by render").pixels;
        RenderView::Borrowed {
            width: w,
            height: h,
            pixels,
        }
    }

    /// Pixel-row band `[y0, y1)` of grid row `r` (of `rows`) in a frame of height `h`.
    #[inline]
    pub fn row_pixel_band(&self, r: usize, rows: usize, h: usize) -> (usize, usize) {
        fx_raster::band_px(r, rows, h, self.grid_top(), self.cell_h)
    }

    /// Where this renderer's cell-row bands lie in window pixels.
    #[inline]
    #[must_use]
    pub fn row_bands(&self) -> RowBands {
        RowBands::new(self.grid_top(), self.cell_h)
    }

    #[inline]
    fn frame_bg(&self, input: &RenderInput) -> u32 {
        if input.default_bg == aterm_core::render::COLOR_UNSET {
            self.theme.bg
        } else {
            input.default_bg
        }
    }

    #[inline]
    fn frame_cursor(&self, input: &RenderInput) -> u32 {
        if input.cursor_color == aterm_core::render::COLOR_UNSET {
            self.theme.cursor
        } else {
            input.cursor_color
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AsymmetricPadLayout",
            action = "PrimeLayoutCache",
            project = "aterm_render::project_asymmetric_pad_layout"
        )
    )]
    fn full_render(&mut self, wc: &mut WindowCpu, input: &RenderInput, w: usize, h: usize) {
        let rows = input.rows;
        self.glyph_plan_row = None;
        let font_epoch = self.font_epoch;
        let bg = self.frame_bg(input) | (self.bg_transmittance() << 24);
        let mut cache = wc.cache.take();
        let mut pixels = match cache.as_mut() {
            Some(c) => std::mem::take(&mut c.pixels),
            None => Vec::new(),
        };
        pixels.clear();
        match wc
            .wallpaper_px
            .as_ref()
            .map(|p| p.px.as_slice())
            .filter(|wp| wp.len() == w.saturating_mul(h))
        {
            Some(wp) => pixels.extend_from_slice(wp),
            None => pixels.resize(w.saturating_mul(h), bg),
        }
        let mut ic = std::mem::take(&mut wc.image_cache);
        self.composite_free(&mut ic, &mut pixels, w, h, input, 0..rows, None, None, None);
        self.draw_glow(&mut pixels, w, h, input, None);
        self.draw_fx(&mut pixels, w, h, input, FxSlot::Glow, FxBlend::Exact, None);
        self.draw_glow_halo(&mut pixels, w, h, input, None);
        self.draw_nova(&mut pixels, w, h, input, None);
        self.draw_fx(&mut pixels, w, h, input, FxSlot::Over, FxBlend::Exact, None);
        self.draw_rain_add(&mut pixels, w, h, input, None);
        self.draw_decorations(&mut pixels, w, h, input, None);
        #[cfg(any(test, debug_assertions))]
        self.stamp_legacy_sprites(&mut pixels, w, h, input, 0..rows, false);
        self.draw_fx(
            &mut pixels,
            w,
            h,
            input,
            FxSlot::Over,
            FxBlend::Linear,
            None,
        );
        self.draw_cursor(&mut pixels, w, h, input);
        wc.image_cache = ic;

        wc.cache = Some(match cache {
            Some(mut c) => {
                c.pixels = pixels;
                c.width = w;
                c.height = h;
                c.grid_top = self.grid_top();
                clone_damage_cache_from_stamped(&mut c.input, input);
                c.cursor_blink_phase = self.cursor_blink_phase;
                c.cursor_style_override = self.cursor_style_override;
                c.font_epoch = font_epoch;
                c
            }
            None => RenderCache {
                pixels,
                width: w,
                height: h,
                grid_top: self.grid_top(),
                input: {
                    let mut damage_input = RenderInput::empty();
                    clone_damage_cache_from_stamped(&mut damage_input, input);
                    damage_input
                },
                cursor_blink_phase: self.cursor_blink_phase,
                cursor_style_override: self.cursor_style_override,
                font_epoch,
            },
        });
    }

    fn fill_chrome_bleed(
        &self,
        pixels: &mut [u32],
        w: usize,
        h: usize,
        input: &RenderInput,
        r: usize,
    ) {
        let Some(bleed) = self.chrome_bleed.filter(|b| (b.first..b.rows).contains(&r)) else {
            return;
        };
        let (y0, y1) = self.row_band_px(r, h);
        if y0 >= y1 {
            return;
        }
        let color = bleed.color & 0x00ff_ffff;
        let (left_tone, right_tone) = bleed.gutter_tones(r);
        let right = w.saturating_sub(self.pad);
        let band_h = y1 - y0;
        self.fill_rect(
            pixels,
            w,
            h,
            0,
            y0,
            self.pad,
            band_h,
            left_tone & 0x00ff_ffff,
        );
        self.fill_rect(
            pixels,
            w,
            h,
            right,
            y0,
            self.pad,
            band_h,
            right_tone & 0x00ff_ffff,
        );
        if r == 0 {
            self.fill_rect(pixels, w, h, 0, 0, w, self.grid_top(), color);
            if bleed.top_extends_cells && y0 > 0 && y0 < h && w > 0 {
                let src = y0 * w;
                if src + w <= pixels.len() {
                    let (above, rest) = pixels.split_at_mut(src);
                    let line = &rest[..w];
                    for y in 0..y0 {
                        above[y * w..y * w + w].copy_from_slice(line);
                    }
                }
            }
        }
        if let Some(seam) = bleed
            .seam
            .filter(|_| r + 1 == bleed.rows && !chrome_rail_covers_gutters(input.chrome_raster(r)))
        {
            let deco = self.deco_metrics();
            let t = deco.underline_t.max(1).min(band_h);
            let uy = y0 + deco.underline_y.min(band_h - t);
            let seam = seam & 0x00ff_ffff;
            self.fill_rect(pixels, w, h, 0, uy, self.pad, t, seam);
            self.fill_rect(pixels, w, h, right, uy, self.pad, t, seam);
        }
    }

    fn fill_chrome_raster(
        &self,
        pixels: &mut [u32],
        w: usize,
        h: usize,
        input: &RenderInput,
        r: usize,
    ) {
        let Some(m) = input.chrome_raster(r) else {
            return;
        };
        let (y0, y1) = self.row_band_px(r, h);
        if y0 >= y1 {
            return;
        }
        let rail_y0 = y1.saturating_sub(chrome_fit(m, self.chrome_room).1).max(y0);
        visit_chrome_raster_runs(
            m,
            w,
            self.pad,
            self.cell_w,
            input.cols,
            |x0, x1, color, rail| {
                let (ya, yb) = if rail { (rail_y0, y1) } else { (y0, y1) };
                for y in ya..yb {
                    let row = &mut pixels[y * w..(y + 1) * w];
                    row[x0..x1].fill(color & 0x00ff_ffff);
                }
            },
        );
        if !m.rings.is_empty() {
            let floor = self.chrome_ring_floor();
            for (x0, x1, ya, yb, color) in
                chrome_ring_runs(m, w, self.pad, self.cell_w, self.cell_h, floor)
            {
                for y in (y0 + ya).min(y1)..(y0 + yb).min(y1) {
                    pixels[y * w + x0..y * w + x1].fill(color);
                }
            }
        }
    }

    #[inline]
    fn row_band_px(&self, r: usize, h: usize) -> (usize, usize) {
        let y0 = (self.grid_top() + r * self.cell_h).min(h);
        let y1 = (self.grid_top() + (r + 1) * self.cell_h).min(h);
        (y0, y1)
    }

    /// Whether row `r` of `input` may draw glyph ink above its own band (upward overshoot: accented capitals at some sizes, tall marks, a fallback face taller than the primary).
    #[must_use]
    pub fn row_may_ink_above(&mut self, input: &RenderInput, r: usize) -> bool {
        let Some(cells) = input.cells.get(r) else {
            return false;
        };
        if input.combining.get(r).is_some_and(|m| !m.is_empty())
            || input.clusters.get(r).is_some_and(|c| !c.is_empty())
            || !sparse_row_slice(&input.fire_halo, r).is_empty()
        {
            return true;
        }
        let cells = &cells[..cells.len().min(input.cols)];
        (0..cells.len()).any(|c| self.cell_may_ink_above(cells, c))
    }

    fn cell_may_ink_above(&mut self, row: &[RenderCell], c: usize) -> bool {
        let cell = &row[c];
        if cell.wide || cell.ch == ' ' || cell.ch.is_control() {
            return false;
        }
        let trust = self.overshoot_trust;
        if trust == OvershootTrust::Nothing {
            return true;
        }
        if procedural::covers(cell.ch)
            || (trust == OvershootTrust::AsciiInCell && cell.ch.is_ascii_graphic())
        {
            return false;
        }
        let key = self.resolve_cell_key_for_span(None, cell, materialized_cell_span(row, c));
        let top = self.baseline;
        self.glyphs.get(&key).is_none_or(|g| {
            g.ink
                .is_some_and(|(ink_h, ink_ymin)| ink_ymin + ink_h as i32 > top)
        })
    }

    /// Check row `r`'s drawn glyph ink against [`Self::row_may_ink_above`].
    pub fn catch_unpredicted_overshoot(
        &mut self,
        input: &RenderInput,
        r: usize,
        ink_top: i32,
        band_top: i32,
    ) -> bool {
        if ink_top >= band_top || self.row_may_ink_above(input, r) {
            return false;
        }
        self.overshoot_trust = match self.overshoot_trust {
            OvershootTrust::AsciiInCell => OvershootTrust::Rasters,
            OvershootTrust::Rasters | OvershootTrust::Nothing => OvershootTrust::Nothing,
        };
        true
    }

    /// Whether a raster has caught [`Self::row_may_ink_above`] wrong, so it takes less on trust than its default (printable ASCII and the procedural cells in their cell).
    #[must_use]
    pub fn overshoot_rule_widened(&self) -> bool {
        self.overshoot_trust != OvershootTrust::AsciiInCell
    }

    /// The row above every marked row whose upward overshoot may differ from the last frame's, marked in `dirty` (a row-damage verdict of [`compute_dirty_rows`] for `prev` → `cur`).
    pub fn mark_overshoot_rows(
        &mut self,
        prev: &RenderInput,
        cur: &RenderInput,
        dirty: &mut [bool],
    ) {
        let rows = dirty.len().min(cur.rows);
        let stamped = row_stamps_usable(prev, cur, rows);
        let mut memo = ImageEqMemo::default();
        // A letter's knock-out ring reaches [`KNOCK_REACH`] pixels over its row: a body lying
        // there is cut by the row's letters (`Self::cut_knock_layers`), so their change repaints
        // the row above it too.
        let mut bodies = std::mem::take(&mut self.knock_cut.spans);
        bodies.clear();
        let top = self.grid_top() as i64;
        for l in prev
            .fx_layers
            .iter()
            .chain(&cur.fx_layers)
            .filter(|l| l.knock)
        {
            let e = l.extent();
            let dy = match l.space {
                aterm_core::render::FxSpace::Window => 0,
                aterm_core::render::FxSpace::Grid => top,
            };
            bodies.push((i64::from(e.y0) + dy, i64::from(e.y1) + dy));
        }
        let ch = self.cell_h as i64;
        let reach = i64::from(KNOCK_REACH);
        for r in 1..rows {
            if dirty[r]
                && !dirty[r - 1]
                && row_glyph_ink_may_differ(prev, cur, r, stamped, &mut memo)
                && (bodies.iter().any(|&(y0, y1)| {
                    let edge = top + r as i64 * ch;
                    y0 < edge && y1 > edge - reach
                }) || self.row_may_ink_above(prev, r)
                    || self.row_may_ink_above(cur, r))
            {
                dirty[r - 1] = true;
            }
        }
        self.knock_cut.spans = bodies;
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_lower_apron(
        &mut self,
        ic: &mut ImageCache,
        pixels: &mut [u32],
        w: usize,
        h: usize,
        input: &RenderInput,
        r: usize,
        band: &mut Vec<u32>,
    ) {
        let src = r + 1;
        let (sy0, sy1) = self.row_band_px(src, h);
        let (by0, _) = self.row_band_px(r, h);
        if sy0 >= sy1 || by0 >= sy0 {
            return;
        }
        band.clear();
        band.extend_from_slice(&pixels[sy0 * w..sy1 * w]);
        let ctx = self.row_ctx(input, src, self.selection_palette(input));
        self.fg_clip = (by0 as i32, sy0 as i32);
        self.render_row_fg(ic, pixels, w, h, input, src, &ctx);
        self.fg_clip = (i32::MIN, i32::MAX);
        pixels[sy0 * w..sy1 * w].copy_from_slice(band);
    }

    #[allow(clippy::too_many_arguments)]
    fn composite_free<I>(
        &mut self,
        ic: &mut ImageCache,
        pixels: &mut [u32],
        w: usize,
        h: usize,
        input: &RenderInput,
        rows: I,
        band_bg: Option<u32>,
        base: Option<&[u32]>,
        dirty: Option<&[bool]>,
    ) where
        I: Iterator<Item = usize> + Clone,
    {
        let sel = self.selection_palette(input);
        for r in rows.clone() {
            if let Some(bg) = band_bg {
                let (y0, y1) = self.row_pixel_band(r, input.rows, h);
                base_span(pixels, y0 * w, y1 * w, bg, base);
            }
            let ctx = self.row_ctx(input, r, sel);
            self.render_row_bg(pixels, w, h, input, &ctx);
            self.fill_chrome_bleed(pixels, w, h, input, r);
            self.fill_chrome_raster(pixels, w, h, input, r);
            self.render_row_images_below_bg(ic, pixels, w, h, input, r, &ctx);
        }
        // The rain's rows lie on the backgrounds, under the light; the companions' bodies
        // (the knock layers) stand on the light, under the ink, cut where their letters lie
        // (`Self::cut_knock_layers`): drawn before the light, a cat whose head reached into
        // the typed row wore the rainbow ribbon's colours (`FxLinearUnder` then
        // `FxKnockUnder` on the GPU).
        self.draw_picked_fx(
            pixels,
            w,
            h,
            input,
            (FxSlot::Under, FxBlend::Linear, Knock::Not),
            dirty,
        );
        self.draw_trail(pixels, w, h, input, dirty);
        self.draw_glow_under(pixels, w, h, input, dirty);
        self.draw_fx(pixels, w, h, input, FxSlot::Under, FxBlend::Exact, dirty);
        self.draw_fire_patch(pixels, w, h, input, dirty);
        self.draw_knock_layers(pixels, w, h, input, dirty);
        #[cfg(any(test, debug_assertions))]
        self.stamp_legacy_sprites(pixels, w, h, input, rows.clone(), true);
        for r in rows {
            let ctx = self.row_ctx(input, r, sel);
            if dirty.is_some_and(|d| r > 0 && !d[r - 1]) {
                self.fg_clip.0 = ctx.y0 as i32;
            }
            self.render_row_fg(ic, pixels, w, h, input, r, &ctx);
            self.fg_clip.0 = i32::MIN;
        }
    }

    /// The companions' bodies (the knock layers), drawn over the under-glyph light and cut
    /// where their letters lie ([`Self::cut_knock_layers`]).
    fn draw_knock_layers(
        &mut self,
        pixels: &mut [u32],
        w: usize,
        h: usize,
        input: &RenderInput,
        dirty: Option<&[bool]>,
    ) {
        let pick = (FxSlot::Under, FxBlend::Linear, Knock::Only);
        if !self.cut_knock_layers(input, w, h) {
            self.draw_picked_fx(pixels, w, h, input, pick, dirty);
            return;
        }
        let layers = std::mem::take(&mut self.knock_cut.layers);
        let frame = self.fx_frame(w, h, input.rows);
        fx_raster::draw_picked_layers(
            pixels,
            &frame,
            &layers,
            &input.fx_occluders,
            pick,
            dirty,
            &mut self.fx_scratch,
        );
        self.knock_cut.layers = layers;
    }

    /// THE KNOCK-OUT: each companion's body (a knock layer, [`FxLayer::knock`]: the pet, its
    /// departures, the word-cats, the idle cat) is cut where its letters lie, under every
    /// glyph on it, the glyph's coverage dilated by two pixels ([`KNOCK_DILATE_OFFSETS`]), so
    /// the ink reads against what lies under the body there (the page, the trail's bed and
    /// light, a selection, a wallpaper, the glass) and not against the coat. The bodies stand
    /// on the under-glyph light: a page laid round the letters on a body instead met the bed
    /// and light beside it in a hard edge across the letter. Returns whether any body was
    /// cut; the frame's fx layers with the cut bodies are then `self.knock_cut.layers`, each
    /// cut body one straight-RGBA sprite drawn 1:1 over its drawn box, its alpha taken away
    /// under the rings. A cut that did not change since the last frame keeps its content
    /// (`Arc`), so a body resting on letters repaints nothing. The GPU renderer draws the
    /// same layers.
    pub fn cut_knock_layers(&mut self, input: &RenderInput, w: usize, h: usize) -> bool {
        #[cfg(any(test, debug_assertions))]
        if self.knock_off {
            return false;
        }
        if !input.fx_layers.iter().any(|l| l.knock) {
            return false;
        }
        // A glyph plan kept from another frame (the GPU's twin renders no rows) is stale.
        self.glyph_plan_row = None;
        let frame = self.fx_frame(w, h, input.rows);
        let mut cut = std::mem::take(&mut self.knock_cut);
        cut.next.clear();
        cut.layers.clear();
        let mut any = false;
        for layer in &input.fx_layers {
            let Some((o, area, sp)) = fx_raster::knock_sprite_area(&frame, layer) else {
                cut.layers.push(layer.clone());
                continue;
            };
            let n = ((area[2] - area[0]) as usize) * ((area[3] - area[1]) as usize);
            cut.mask.clear();
            cut.mask.resize(n, 0);
            if !self.knock_ring_mask(input, area, &mut cut.mask) {
                cut.layers.push(layer.clone());
                continue;
            }
            let key = knock_cut_key(layer, sp, o, area, &cut.mask);
            let content = match cut.held.iter().find(|(k, _)| *k == key) {
                Some((_, c)) => Arc::clone(c),
                None => {
                    let Some(art) =
                        fx_raster::cut_sprite_art(o, area, sp, &cut.mask, &mut cut.texels)
                    else {
                        cut.layers.push(layer.clone());
                        continue;
                    };
                    let ink = layer.content.palette()[sp.ink as usize];
                    Arc::new(aterm_core::render::FxContent::new(
                        vec![aterm_core::render::FxItem::Sprite(
                            aterm_core::render::FxSprite {
                                x: 0,
                                y: 0,
                                dw: art.w(),
                                dh: art.h(),
                                flip_x: false,
                                art: Arc::new(art),
                                ink: 0,
                                clip: aterm_core::render::PxRect::EVERYWHERE,
                            },
                        )],
                        vec![ink],
                    ))
                }
            };
            cut.next.push((key, Arc::clone(&content)));
            let (gx, gy) = match layer.space {
                aterm_core::render::FxSpace::Window => (0, 0),
                aterm_core::render::FxSpace::Grid => (frame.pad as i32, frame.grid_top as i32),
            };
            let mut cut_layer = layer.clone();
            cut_layer.content = content;
            cut_layer.offset = (area[0] - gx, area[1] - gy);
            cut.layers.push(cut_layer);
            any = true;
        }
        std::mem::swap(&mut cut.held, &mut cut.next);
        self.knock_cut = cut;
        any
    }

    /// The frame's fx layers with the cut bodies, after [`Self::cut_knock_layers`] returned
    /// true.
    #[must_use]
    pub fn cut_layers(&self) -> &[aterm_core::render::FxLayer] {
        &self.knock_cut.layers
    }

    /// The knock-out's rings over `area` (`[x0, y0, x1, y1)`, frame pixels) into `mask` (one
    /// word a pixel, row-major, each channel the share of the pixel cut): every glyph within
    /// [`KNOCK_REACH`] of the area stamps its coverage at each of [`KNOCK_DILATE_OFFSETS`],
    /// cut to its row's band ([`Self::cut_knock_layers`]). Returns whether any pixel was cut.
    fn knock_ring_mask(&mut self, input: &RenderInput, area: [i32; 4], mask: &mut [u32]) -> bool {
        let ch = self.cell_h as i32;
        let reach = KNOCK_REACH;
        let sel = self.selection_palette(input);
        for r in 0..input.rows {
            let ctx = self.row_ctx(input, r, sel);
            let y0 = ctx.y0 as i32;
            if y0 - reach >= area[3] || y0 + ch + reach <= area[1] {
                continue;
            }
            self.knock_row_ring(input, &ctx, area, mask);
        }
        mask.iter().any(|&m| m != 0)
    }

    /// One row of [`Self::knock_ring_mask`].
    fn knock_row_ring(
        &mut self,
        input: &RenderInput,
        ctx: &RowCtx<'_>,
        area: [i32; 4],
        mask: &mut [u32],
    ) {
        let RowCtx {
            cells,
            row: r,
            y0,
            cw,
            scale,
            anchor_y,
            uniform,
            ..
        } = *ctx;
        let reach = KNOCK_REACH;
        let cols = input.cols;
        let pad_x = self.pad;
        let row_images = input.images.get(r).map(Vec::as_slice).unwrap_or(&[]);
        let has_images = !row_images.is_empty();
        let row_clusters = input.clusters.get(r).map(Vec::as_slice).unwrap_or(&[]);
        let has_clusters = !row_clusters.is_empty();
        let row_raster = input.chrome_raster(r);
        let mut plan = std::mem::take(&mut self.glyph_plan_scratch);
        for (c, cell) in cells.iter().take(cols).enumerate() {
            if (has_images && image_covers(row_images, c))
                || cell.wide
                || cell.ch == ' '
                || cell.ch.is_control()
            {
                continue;
            }
            let (x, cell_w, scale, anchor_y) = if uniform {
                (pad_x + c * cw, cw, scale, anchor_y)
            } else {
                let p = self.mixed_cell_place(input, r, c, y0);
                (p.x, p.w, p.scale, p.anchor_y)
            };
            if (x + cell_w) as i32 + reach <= area[0] || x as i32 - reach >= area[2] {
                continue;
            }
            if self.glyph_plan_row != Some(r) {
                let mut break_cols = std::mem::take(&mut self.ligature_break_scratch);
                ligature_break_cols_into(input, r, &self.shaping, &mut break_cols);
                self.row_glyph_plan(input, r, &break_cols, &mut plan);
                self.ligature_break_scratch = break_cols;
                self.glyph_plan_row = Some(r);
            }
            let cluster = if has_clusters {
                cluster_for(row_clusters, c)
            } else {
                None
            };
            let key = match plan.get(c).copied().unwrap_or(ColumnGlyph::PerCell) {
                ColumnGlyph::Ligated(gid) => self.ligature_key(gid, cell_style(cell)),
                ColumnGlyph::LigatedSlice { gid, k, .. } => {
                    self.ligature_slice_key(gid, k, cell_style(cell))
                }
                ColumnGlyph::PerCell => {
                    self.resolve_cell_key_for_span(cluster, cell, materialized_cell_span(cells, c))
                }
            };
            let key = self.chrome_icon_key(row_raster, cells, c).unwrap_or(key);
            let key = shade_phase_key(key, x, y0);
            self.blit_glyph_ring(mask, area, x as i32, anchor_y, key, scale);
        }
        self.glyph_plan_scratch = plan;
    }

    /// Stamp `key`'s coverage at each of [`KNOCK_DILATE_OFFSETS`] into `mask` over `area`, cut
    /// to `scale`'s clip ([`Self::knock_ring_mask`]).
    fn blit_glyph_ring(
        &mut self,
        mask: &mut [u32],
        area: [i32; 4],
        cell_x: i32,
        cell_y: i32,
        key: GlyphKey,
        scale: Scale,
    ) {
        let baseline = self.baseline;
        let entry = self.cached_glyph(key);
        let GlyphImage::Mono {
            width,
            height,
            xmin,
            ymin,
            bytes,
            ..
        } = &entry.img
        else {
            return;
        };
        let (width, height, xmin, ymin) = (*width, *height, *xmin, *ymin);
        if width == 0 || height == 0 {
            return;
        }
        let ring = &entry.ring;
        let (xs, ys) = (scale.xs.max(1), scale.ys.max(1));
        let gx0 = cell_x + xmin * xs as i32;
        let gy0 = cell_y + ys as i32 * (baseline - height as i32 - ymin);
        let (gw, gh) = ((width * xs) as i32, (height * ys) as i32);
        let reach = KNOCK_REACH;
        if gx0 + gw + reach <= area[0]
            || gx0 - reach >= area[2]
            || gy0 + gh + reach <= area[1]
            || gy0 - reach >= area[3]
        {
            return;
        }
        let stride = (area[2] - area[0]) as usize;
        let clip_y = (
            scale.clip_y0.max(area[1]) - area[1],
            scale.clip_y1.min(area[3]) - area[1],
        );
        let clip_x = (
            scale.clip_x0.max(area[0]) - area[0],
            scale.clip_x1.min(area[2]) - area[0],
        );
        if clip_y.0 >= clip_y.1 || clip_x.0 >= clip_x.1 {
            return;
        }
        if xs == 1 && ys == 1 {
            // One blit of the glyph's ring ([`knock_ring`]), built once per glyph.
            let ring = ring.get_or_init(|| knock_ring(width, height, bytes));
            blit_mono_with(
                mask,
                stride,
                gx0 - reach - area[0],
                gy0 - reach - area[1],
                width + 2 * reach as usize,
                height + 2 * reach as usize,
                ring,
                0x00FF_FFFF,
                255,
                1,
                1,
                clip_y,
                clip_x,
                knock_cover,
            );
            return;
        }
        // A line drawn at twice the size scales the raster as it blits: the offsets are
        // frame pixels, so each is stamped.
        let near = knock_near_stamp(bytes);
        for &(dx, dy) in &KNOCK_DILATE_OFFSETS {
            let stamp = match &near {
                Some(near) if dx.abs() <= 1 && dy.abs() <= 1 => near.as_slice(),
                _ => &bytes[..],
            };
            blit_mono_with(
                mask,
                stride,
                gx0 + dx - area[0],
                gy0 + dy - area[1],
                width,
                height,
                stamp,
                0x00FF_FFFF,
                255,
                xs,
                ys,
                clip_y,
                clip_x,
                knock_cover,
            );
        }
    }

    /// Draw the old sprite path ([`legacy_sprite_oracle`]) where the old
    /// composite drew it: a differential test's oracle.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn set_legacy_sprites(&mut self, sprites: Option<legacy_sprite_oracle::LegacySprites>) {
        self.legacy_sprites = sprites;
    }

    #[cfg(any(test, debug_assertions))]
    fn stamp_legacy_sprites(
        &mut self,
        pixels: &mut [u32],
        w: usize,
        h: usize,
        input: &RenderInput,
        rows: impl Iterator<Item = usize> + Clone,
        under: bool,
    ) {
        let Some(l) = self.legacy_sprites.take() else {
            return;
        };
        let (pad, top) = (self.pad, self.grid_top());
        let mut mask = Vec::new();
        for s in l.sprites.iter().filter(|s| s.under == under) {
            // The old path's knock-out, as the layers' ([`Self::cut_knock_layers`]): the
            // under-text sprite cut where its letters lie.
            let area = legacy_sprite_oracle::frame_box(s, pad, top, w, h);
            let cut = match area {
                Some(area) if under && !self.knock_off => {
                    mask.clear();
                    mask.resize(((area[2] - area[0]) * (area[3] - area[1])) as usize, 0);
                    self.knock_ring_mask(input, area, &mut mask)
                        .then_some((area, mask.as_slice()))
                }
                _ => None,
            };
            for r in rows.clone() {
                let (y_lo, y_hi) = self.row_pixel_band(r, input.rows, h);
                legacy_sprite_oracle::stamp(pixels, w, h, pad, top, s, y_lo, y_hi, cut);
            }
        }
        self.legacy_sprites = Some(l);
    }

    /// A test's negative control: draw companions under the text without
    /// the knock-out ([`Self::cut_knock_layers`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub fn set_knock_out(&mut self, on: bool) {
        self.knock_off = !on;
    }

    /// Whether the knock-out draws ([`Self::cut_knock_layers`]); off
    /// only for a test's negative control ([`Self::set_knock_out`]). The GPU
    /// renderer reads it to build the same stamps.
    #[inline]
    #[must_use]
    pub const fn knock_out_on(&self) -> bool {
        #[cfg(any(test, debug_assertions))]
        {
            !self.knock_off
        }
        #[cfg(not(any(test, debug_assertions)))]
        {
            true
        }
    }
}

struct RowCtx<'a> {
    cells: &'a [RenderCell],
    ink_row: &'a [InkCell],
    char_fg_row: &'a [CharFg],
    fire_halo_row: &'a [FireHaloCell],
    row: usize,
    y0: usize,
    cw: usize,
    scale: Scale,
    anchor_y: i32,
    uniform: bool,
    sel: SelectionPalette<'a>,
    bg_t: u32,
    frame_bg: u32,
    uniform_default_bg: bool,
}

/// What a row's glyph loop and its fire halo stamps read of the row beside its [`RowCtx`]: its
/// images over the text, its clusters, its chrome raster and its column plan.
#[derive(Clone, Copy)]
struct RowGlyphs<'a> {
    images: &'a [(usize, aterm_core::grid::extra::ImageRef)],
    clusters: &'a [(usize, Box<str>)],
    raster: Option<&'a ChromeRaster>,
    plan: &'a [ColumnGlyph],
}

/// Where a cell's glyph is drawn and which raster: [`Renderer::cell_glyph`].
struct CellGlyph<'a> {
    x: usize,
    w: usize,
    scale: Scale,
    anchor_y: i32,
    key: GlyphKey,
    cluster: Option<&'a str>,
}

#[derive(Clone, Copy)]
struct CellPlace {
    x: usize,
    w: usize,
    lo: usize,
    hi: usize,
    scale: Scale,
    anchor_y: i32,
}

#[inline]
fn clip_span_to_run(x: usize, w: usize, lo: usize, hi: usize) -> Option<(usize, usize)> {
    let x0 = x.max(lo);
    let x1 = x.saturating_add(w).min(hi);
    x1.checked_sub(x0).filter(|d| *d > 0).map(|d| (x0, d))
}

impl Renderer {
    /// Where cell `c` of `ctx`'s row draws its glyph, and which raster: what the glyph loop
    /// blits and the fire halo stamps lay under it, so the two never disagree. `None` for a cell
    /// that draws no glyph: one an image covers, a wide character's continuation, a blank, a
    /// control, a column past the row.
    #[inline]
    fn cell_glyph<'a>(
        &mut self,
        input: &RenderInput,
        ctx: &RowCtx<'a>,
        src: RowGlyphs<'a>,
        c: usize,
    ) -> Option<CellGlyph<'a>> {
        let cell = ctx.cells.get(c).filter(|_| c < input.cols)?;
        if (!src.images.is_empty() && image_covers(src.images, c))
            || cell.wide
            || cell.ch == ' '
            || cell.ch.is_control()
        {
            return None;
        }
        let (x, w, scale, anchor_y) = if ctx.uniform {
            (self.pad + c * ctx.cw, ctx.cw, ctx.scale, ctx.anchor_y)
        } else {
            let p = self.mixed_cell_place(input, ctx.row, c, ctx.y0);
            (p.x, p.w, p.scale, p.anchor_y)
        };
        let cluster = if src.clusters.is_empty() {
            None
        } else {
            cluster_for(src.clusters, c)
        };
        let key = match src.plan.get(c).copied().unwrap_or(ColumnGlyph::PerCell) {
            ColumnGlyph::Ligated(gid) => self.ligature_key(gid, cell_style(cell)),
            ColumnGlyph::LigatedSlice { gid, k, .. } => {
                self.ligature_slice_key(gid, k, cell_style(cell))
            }
            ColumnGlyph::PerCell => {
                self.resolve_cell_key_for_span(cluster, cell, materialized_cell_span(ctx.cells, c))
            }
        };
        let key = self
            .chrome_icon_key(src.raster, ctx.cells, c)
            .unwrap_or(key);
        Some(CellGlyph {
            x,
            w,
            scale,
            anchor_y,
            key: shade_phase_key(key, x, ctx.y0),
            cluster,
        })
    }

    /// Stamp the fire contrast halos of `ctx`'s row that land in frame rows `lo..hi`, before the
    /// ink drawn there (the fire-halo contract: over the flame, under the ink). Each band stamps
    /// the halos that land in it before its own ink, so a halo lies under every row's ink, as
    /// the GPU's halo stream, drawn before its glyph stream, lays it (SPECS `layer-z-order`).
    fn stamp_fire_halos(
        &mut self,
        pixels: &mut [u32],
        w: usize,
        input: &RenderInput,
        ctx: &RowCtx<'_>,
        src: RowGlyphs<'_>,
        (lo, hi): (i32, i32),
    ) {
        let (lo, hi) = (lo.max(self.fg_clip.0), hi.min(self.fg_clip.1));
        if lo >= hi {
            return;
        }
        let mut next_col = 0usize;
        for hc in ctx.fire_halo_row {
            let c = usize::from(hc.col);
            if c < next_col {
                continue;
            }
            if c >= input.cols.min(ctx.cells.len()) {
                break;
            }
            next_col = c + 1;
            let Some(g) = self.cell_glyph(input, ctx, src, c) else {
                continue;
            };
            let scale = Scale {
                clip_y0: g.scale.clip_y0.max(lo),
                clip_y1: g.scale.clip_y1.min(hi),
                ..g.scale
            };
            self.blit_glyph_halo(
                pixels,
                w,
                g.x as i32,
                g.anchor_y,
                g.key,
                scale,
                fire_halo_alpha(hc.strength),
            );
        }
    }

    /// Stamp, in `ctx`'s row band, the halos of the row below that lift into it
    /// ([`HALO_DILATE_OFFSETS`] reach up to 2 px above a glyph), before this row's ink.
    fn stamp_lifted_fire_halos(
        &mut self,
        pixels: &mut [u32],
        w: usize,
        input: &RenderInput,
        ctx: &RowCtx<'_>,
    ) {
        let below = ctx.row + 1;
        let band = (ctx.y0 as i32, (ctx.y0 + self.cell_h) as i32);
        if below >= input.rows
            || band.0.max(self.fg_clip.0) >= band.1.min(self.fg_clip.1)
            || sparse_row_slice(&input.fire_halo, below).is_empty()
        {
            return;
        }
        let next = self.row_ctx(input, below, ctx.sel);
        let mut plan = std::mem::take(&mut self.halo_plan_scratch);
        let mut break_cols = std::mem::take(&mut self.ligature_break_scratch);
        ligature_break_cols_into(input, below, &self.shaping, &mut break_cols);
        self.row_glyph_plan(input, below, &break_cols, &mut plan);
        self.ligature_break_scratch = break_cols;
        let src = RowGlyphs {
            images: input.images.get(below).map(Vec::as_slice).unwrap_or(&[]),
            clusters: input.clusters.get(below).map(Vec::as_slice).unwrap_or(&[]),
            raster: input.chrome_raster(below),
            plan: &plan,
        };
        self.stamp_fire_halos(pixels, w, input, &next, src, band);
        self.halo_plan_scratch = plan;
    }

    fn mixed_cell_place(
        &self,
        input: &RenderInput,
        row: usize,
        col: usize,
        y0: usize,
    ) -> CellPlace {
        let (x, w, cx0, cx1) = cell_x_run(input, row, col, self.cell_w);
        let (x, lo, hi) = (self.pad + x, self.pad + cx0, self.pad + cx1);
        let (line_size, _, _) = input.line_size_run_at(row, col);
        let (mut scale, anchor_y) = chrome_lifted(
            input,
            row,
            y0,
            self.cell_h,
            self.chrome_room,
            row_scale(line_size, y0, self.cell_h, row + 1 == input.rows),
        );
        scale.clip_x0 = scale.clip_x0.max(lo as i32);
        scale.clip_x1 = scale.clip_x1.min(hi as i32);
        CellPlace {
            x,
            w,
            lo,
            hi,
            scale,
            anchor_y,
        }
    }

    fn row_ctx<'a>(
        &self,
        input: &'a RenderInput,
        r: usize,
        sel: SelectionPalette<'a>,
    ) -> RowCtx<'a> {
        let cells = input.cells.get(r).map(Vec::as_slice).unwrap_or(&[]);
        let y0 = self.grid_top() + r * self.cell_h;
        let line_size = input
            .line_sizes
            .get(r)
            .copied()
            .unwrap_or(LineSize::SingleWidth);
        let cw = row_cell_w(line_size, self.cell_w);
        let (scale, anchor_y) = chrome_lifted(
            input,
            r,
            y0,
            self.cell_h,
            self.chrome_room,
            row_scale(line_size, y0, self.cell_h, r + 1 == input.rows),
        );
        let uniform = row_is_uniform(input, r);
        let ink_row = sparse_row_slice(&input.ink, r);
        let char_fg_row = sparse_row_slice(&input.char_fg, r);
        let fire_halo_row = sparse_row_slice(&input.fire_halo, r);
        let bg_t = self.bg_transmittance();
        let frame_bg = self.frame_bg(input);
        let uniform_default_bg = default_bg_spans(input, r).is_empty();
        RowCtx {
            cells,
            ink_row,
            char_fg_row,
            fire_halo_row,
            row: r,
            y0,
            cw,
            scale,
            anchor_y,
            uniform,
            sel,
            bg_t,
            frame_bg,
            uniform_default_bg,
        }
    }

    #[inline]
    fn cell_default_bg(&self, input: &RenderInput, ctx: &RowCtx<'_>, col: usize) -> u32 {
        if ctx.uniform_default_bg {
            ctx.frame_bg
        } else {
            resolved_default_bg_at(input, ctx.row, col, self.theme.bg)
        }
    }

    /// `(total, at_base)` background runs resolved by the last render (the `bg_runs_*` diagnostic).
    #[inline]
    #[must_use]
    pub fn last_bg_runs(&self) -> (u32, u32) {
        (self.bg_runs_total.get(), self.bg_runs_at_base.get())
    }

    /// Background `fill_rect`s the last render emitted from the materialized prefix of [`Self::render_row_bg`] (the `bg_fills_emitted` diagnostic).
    #[inline]
    #[must_use]
    pub fn last_bg_fills(&self) -> u32 {
        self.bg_fills_emitted.get()
    }

    #[inline]
    fn note_bg_fill(&self) {
        self.bg_fills_emitted
            .set(self.bg_fills_emitted.get().saturating_add(1));
    }

    #[inline]
    fn note_bg_run(&self, color: u32, band_base: Option<u32>) {
        self.bg_runs_total
            .set(self.bg_runs_total.get().saturating_add(1));
        if band_base == Some(color) {
            self.bg_runs_at_base
                .set(self.bg_runs_at_base.get().saturating_add(1));
        }
    }

    fn render_row_bg(
        &self,
        pixels: &mut [u32],
        w: usize,
        h: usize,
        input: &RenderInput,
        ctx: &RowCtx<'_>,
    ) {
        let cols = input.cols;
        let RowCtx {
            cells,
            row,
            y0,
            cw,
            uniform,
            sel,
            bg_t,
            ..
        } = *ctx;
        let wallpaper = input.wallpaper.is_some();
        let band_base = if wallpaper {
            None
        } else {
            Some(ctx.frame_bg | (bg_t << 24))
        };
        let may_have_sel = input.any_selection();
        let resolve = |c: usize, cell: &RenderCell| -> Option<u32> {
            if may_have_sel
                && let Some(hit) =
                    input.selection_hit(row, c, cells.get(c + 1).is_some_and(|n| n.wide), cell.wide)
            {
                Some(sel.at(hit).bg)
            } else {
                let cell_bg = rgb_to_u32(cell.bg);
                if cell_bg == self.cell_default_bg(input, ctx, c) {
                    if wallpaper {
                        None
                    } else if bg_t != 0 {
                        Some(cell_bg | (bg_t << 24))
                    } else {
                        Some(cell_bg)
                    }
                } else {
                    Some(cell_bg)
                }
            }
        };
        if uniform {
            let n = cells.len().min(cols);
            let mut run_start = 0usize;
            let mut run_bg: Option<u32> = None;
            for (c, cell) in cells[..n].iter().enumerate() {
                let bg = resolve(c, cell);
                if c == run_start {
                    run_bg = bg;
                } else if bg != run_bg {
                    if let Some(color) = run_bg {
                        self.note_bg_run(color, band_base);
                        if run_bg != band_base {
                            self.note_bg_fill();
                            let x = self.pad + run_start * cw;
                            self.fill_rect(
                                pixels,
                                w,
                                h,
                                x,
                                y0,
                                (c - run_start) * cw,
                                self.cell_h,
                                color,
                            );
                        }
                    }
                    run_start = c;
                    run_bg = bg;
                }
            }
            if let Some(color) = run_bg {
                self.note_bg_run(color, band_base);
                if run_bg != band_base {
                    self.note_bg_fill();
                    let x = self.pad + run_start * cw;
                    self.fill_rect(
                        pixels,
                        w,
                        h,
                        x,
                        y0,
                        (n - run_start) * cw,
                        self.cell_h,
                        color,
                    );
                }
            }
        } else {
            for (c, cell) in cells.iter().take(cols).enumerate() {
                if let Some(color) = resolve(c, cell) {
                    self.note_bg_run(color, band_base);
                    if band_base != Some(color) {
                        self.note_bg_fill();
                        let p = self.mixed_cell_place(input, row, c, y0);
                        if let Some((x, rw)) = clip_span_to_run(p.x, p.w, p.lo, p.hi) {
                            self.fill_rect(pixels, w, h, x, y0, rw, self.cell_h, color);
                        }
                    }
                }
            }
        }

        let materialized = cells.len().min(cols);
        if materialized >= cols {
            return;
        }
        let selection_reaches_tail = input
            .selection_row_span(row)
            .is_some_and(|(_, end)| usize::from(end) >= materialized);
        if !selection_reaches_tail && ctx.uniform_default_bg {
            return;
        }
        for c in materialized..cols {
            let bg = if let Some(hit) = input.selection_hit(row, c, false, false) {
                sel.at(hit).bg
            } else {
                if wallpaper {
                    continue;
                }
                let default_bg = self.cell_default_bg(input, ctx, c);
                if default_bg == ctx.frame_bg {
                    continue;
                }
                default_bg | (bg_t << 24)
            };
            if uniform {
                self.fill_rect(pixels, w, h, self.pad + c * cw, y0, cw, self.cell_h, bg);
            } else {
                let p = self.mixed_cell_place(input, row, c, y0);
                if let Some((x, rw)) = clip_span_to_run(p.x, p.w, p.lo, p.hi) {
                    self.fill_rect(pixels, w, h, x, y0, rw, self.cell_h, bg);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_row_images_below_bg(
        &self,
        ic: &mut ImageCache,
        pixels: &mut [u32],
        w: usize,
        h: usize,
        input: &RenderInput,
        r: usize,
        ctx: &RowCtx<'_>,
    ) {
        let row_images = input.images.get(r).map(Vec::as_slice).unwrap_or(&[]);
        if !row_images
            .iter()
            .any(|(_, image)| kitty_image_is_below_non_default_bg(image.image.z_index))
        {
            return;
        }
        let cols = input.cols;
        let pad_x = self.pad;
        let RowCtx {
            cells,
            y0,
            cw,
            uniform,
            sel,
            ..
        } = *ctx;
        for &(c, ref image) in row_images {
            if c >= cols || !kitty_image_is_below_non_default_bg(image.image.z_index) {
                continue;
            }
            if uniform {
                self.blit_image_cell(ic, pixels, w, h, pad_x + c * cw, y0, image, w);
            } else {
                let p = self.mixed_cell_place(input, r, c, y0);
                if p.x < p.hi {
                    self.blit_image_cell(ic, pixels, w, h, p.x, y0, image, p.hi);
                }
            }
        }
        for (c, cell) in cells.iter().take(cols).enumerate() {
            if !image_below_non_default_bg_covers(row_images, c) {
                continue;
            }
            let is_wide_lead = cells.get(c + 1).is_some_and(|next| next.wide);
            let cell_bg = rgb_to_u32(cell.bg);
            let cover = if let Some(hit) = input.selection_hit(r, c, is_wide_lead, cell.wide) {
                sel.at(hit).bg
            } else if cell_bg != self.cell_default_bg(input, ctx, c) {
                cell_bg
            } else {
                continue;
            };
            if uniform {
                self.fill_rect(pixels, w, h, pad_x + c * cw, y0, cw, self.cell_h, cover);
            } else {
                let p = self.mixed_cell_place(input, r, c, y0);
                if let Some((x, rw)) = clip_span_to_run(p.x, p.w, p.lo, p.hi) {
                    self.fill_rect(pixels, w, h, x, y0, rw, self.cell_h, cover);
                }
            }
        }

        let materialized = cells.len().min(cols);
        for &(c, ref image) in row_images {
            if c < materialized
                || c >= cols
                || !kitty_image_is_below_non_default_bg(image.image.z_index)
            {
                continue;
            }
            let Some(hit) = input.selection_hit(r, c, false, false) else {
                continue;
            };
            let cover = sel.at(hit).bg;
            if uniform {
                self.fill_rect(pixels, w, h, pad_x + c * cw, y0, cw, self.cell_h, cover);
            } else {
                let p = self.mixed_cell_place(input, r, c, y0);
                if let Some((x, rw)) = clip_span_to_run(p.x, p.w, p.lo, p.hi) {
                    self.fill_rect(pixels, w, h, x, y0, rw, self.cell_h, cover);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_row_fg(
        &mut self,
        ic: &mut ImageCache,
        pixels: &mut [u32],
        w: usize,
        h: usize,
        input: &RenderInput,
        r: usize,
        ctx: &RowCtx<'_>,
    ) {
        let cols = input.cols;
        let pad_x = self.pad;
        self.fg_ink_top = i32::MAX;
        let min_contrast = self.effective_minimum_contrast();
        let RowCtx {
            cells,
            ink_row,
            char_fg_row,
            y0,
            cw,
            uniform,
            sel,
            ..
        } = *ctx;
        let row_images = input.images.get(r).map(Vec::as_slice).unwrap_or(&[]);
        let mut plan = std::mem::take(&mut self.glyph_plan_scratch);
        if self.glyph_plan_row != Some(r) {
            let mut break_cols = std::mem::take(&mut self.ligature_break_scratch);
            ligature_break_cols_into(input, r, &self.shaping, &mut break_cols);
            self.row_glyph_plan(input, r, &break_cols, &mut plan);
            self.ligature_break_scratch = break_cols;
        }
        for &(c, ref image) in row_images {
            if image.image.z_index >= 0
                || kitty_image_is_below_non_default_bg(image.image.z_index)
                || c >= cols
            {
                continue;
            }
            if uniform {
                self.blit_image_cell(ic, pixels, w, h, pad_x + c * cw, y0, image, w);
            } else {
                let p = self.mixed_cell_place(input, r, c, y0);
                if p.x < p.hi {
                    self.blit_image_cell(ic, pixels, w, h, p.x, y0, image, p.hi);
                }
            }
        }
        let row_clusters = input.clusters.get(r).map(Vec::as_slice).unwrap_or(&[]);
        let row_combining = input.combining.get(r).map(Vec::as_slice).unwrap_or(&[]);
        let has_combining = !row_combining.is_empty();
        let mut ink_walk = InkWalk::new(ink_row);
        let mut char_fg_walk = CharFgWalk::new(char_fg_row);
        let fire_present = !input.fire_patch.is_empty();
        let row_raster = input.chrome_raster(r);
        let raster_split = row_raster.and_then(|m| m.split);
        let may_have_sel = input.any_selection();
        let glyphs = RowGlyphs {
            images: row_images,
            clusters: row_clusters,
            raster: row_raster,
            plan: &plan,
        };
        if fire_present {
            let top = if r == 0 { i32::MIN } else { y0 as i32 };
            self.stamp_fire_halos(pixels, w, input, ctx, glyphs, (top, i32::MAX));
            self.stamp_lifted_fire_halos(pixels, w, input, ctx);
        }
        type FgMemo = ((u32, [u8; 3], Option<usize>), (u32, u32));
        let mut fg_memo: Option<FgMemo> = None;
        for (c, cell) in cells.iter().take(cols).enumerate() {
            if let Some(CellGlyph {
                x,
                w: cw,
                scale,
                anchor_y,
                key,
                cluster,
            }) = self.cell_glyph(input, ctx, glyphs, c)
            {
                let base_fg = match ink_walk.at(c as u16) {
                    Some(ink) => rgb_to_u32(ink),
                    None => char_fg_walk
                        .at(c as u16)
                        .unwrap_or_else(|| rgb_to_u32(cell.fg)),
                };
                let hit = if may_have_sel {
                    input.selection_hit(r, c, cells.get(c + 1).is_some_and(|n| n.wide), cell.wide)
                } else {
                    None
                };
                let selected = hit.is_some();
                let SelectionColors {
                    bg: sel_bg,
                    fg: sel_fg,
                } = sel.at_hit(hit);
                let fg_key = (base_fg, cell.bg, hit);
                let (fg, bg_under) = match fg_memo {
                    Some((k, pair)) if k == fg_key => pair,
                    _ => {
                        let cell_bg_u32 = rgb_to_u32(cell.bg);
                        let eff_fg = effective_glyph_fg(
                            sel_fg,
                            min_contrast,
                            base_fg,
                            cell_bg_u32,
                            selected,
                            sel_bg,
                        );
                        let under = if selected { sel_bg } else { cell_bg_u32 };
                        fg_memo = Some((fg_key, (eff_fg, under)));
                        (eff_fg, under)
                    }
                };
                match raster_split.filter(|sp| usize::from(sp.col) == c && !selected) {
                    Some(sp) => {
                        let at = i32::try_from(sp.x).unwrap_or(i32::MAX);
                        let left_fg =
                            effective_glyph_fg(sel_fg, min_contrast, sp.ink, sp.bg, false, sel_bg);
                        let left = Scale {
                            clip_x1: scale.clip_x1.min(at),
                            ..scale
                        };
                        let right = Scale {
                            clip_x0: scale.clip_x0.max(at),
                            ..scale
                        };
                        self.blit(pixels, w, x as i32, anchor_y, key, left_fg, sp.bg, left);
                        self.blit(pixels, w, x as i32, anchor_y, key, fg, bg_under, right);
                    }
                    None => self.blit(pixels, w, x as i32, anchor_y, key, fg, bg_under, scale),
                }
                if has_combining
                    && cluster.is_none()
                    && let Some(marks) = combining_for(row_combining, c)
                {
                    let mut stack = MarkStack::default();
                    for &m in marks {
                        let mk = self.combining_mark_key(m, cell);
                        let (gw, gh, xmin, ink) = {
                            let cg = self.cached_glyph(mk);
                            (cg.img.width(), cg.img.height(), cg.img.xmin(), cg.ink)
                        };
                        if gw == 0 || gh == 0 {
                            continue;
                        }
                        let Some((ink_h, ink_ymin)) = ink else {
                            continue;
                        };
                        let cx = mark_cell_x_at(x as i32, cw, gw, xmin, scale)
                            + mark_script_shift(cell.script, cw);
                        let dy = stack.dy(ink_h, ink_ymin, self.baseline, self.cell_h, scale.ys);
                        self.blit(pixels, w, cx, anchor_y + dy, mk, fg, bg_under, scale);
                    }
                }
            }
        }
        for &(c, ref image) in row_images {
            if image.image.z_index < 0 || c >= cols {
                continue;
            }
            if uniform {
                self.blit_image_cell(ic, pixels, w, h, pad_x + c * cw, y0, image, w);
            } else {
                let p = self.mixed_cell_place(input, r, c, y0);
                if p.x < p.hi {
                    self.blit_image_cell(ic, pixels, w, h, p.x, y0, image, p.hi);
                }
            }
        }
        let dm = self.deco_metrics();
        let curly_mask_ok = deco::undercurl_supported(self.cell_w, self.cell_h);
        let mut deco_scratch = std::mem::take(&mut self.deco_scratch);
        let mut skipped_scratch = std::mem::take(&mut self.deco_skip_rect_scratch);
        let mut ink_scratch = std::mem::take(&mut self.ink_scratch);
        let mut keep_spans = std::mem::take(&mut self.keep_spans_scratch);
        let mut curls = std::mem::take(&mut self.curl_scratch);
        curls.clear();
        let mut ink_walk = InkWalk::new(ink_row);
        let mut char_fg_walk = CharFgWalk::new(char_fg_row);
        for (c, cell) in cells.iter().take(cols).enumerate() {
            if cell.wide
                || (matches!(cell.underline, UnderlineStyle::None)
                    && !cell.strikethrough
                    && !cell.overline)
            {
                continue;
            }
            let (x, cw, lo, hi) = if uniform {
                (pad_x + c * cw, cw, 0, usize::MAX)
            } else {
                let p = self.mixed_cell_place(input, r, c, y0);
                (p.x, p.w, p.lo, p.hi)
            };
            let is_wide_lead = cells.get(c + 1).is_some_and(|n| n.wide);
            let dw = if is_wide_lead { 2 * cw } else { cw };
            let base_fg = match ink_walk.at(c as u16) {
                Some(ink) => rgb_to_u32(ink),
                None => char_fg_walk
                    .at(c as u16)
                    .unwrap_or_else(|| rgb_to_u32(cell.fg)),
            };
            let hit = if may_have_sel {
                input.selection_hit(r, c, is_wide_lead, cell.wide)
            } else {
                None
            };
            let selected = hit.is_some();
            let SelectionColors {
                bg: sel_bg,
                fg: sel_fg,
            } = sel.at_hit(hit);
            let inks = deco_inks(cell, sel_fg, min_contrast, base_fg, selected, sel_bg);
            let ucolor = inks.underline;
            if curly_mask_ok && matches!(cell.underline, UnderlineStyle::Curly) {
                curls.push((c, x, cw, lo, hi, dw, ucolor));
            }
            underline_rects_into(
                &mut deco_scratch,
                cell.underline,
                x,
                y0,
                dw,
                cw,
                self.cell_h,
                dm,
                curly_mask_ok,
            );
            let skip = !deco_scratch.is_empty()
                && self.underline_keep_spans_with_metrics(
                    input,
                    r,
                    c,
                    plan.get(c).copied().unwrap_or(ColumnGlyph::PerCell),
                    x,
                    dw,
                    dm,
                    &mut ink_scratch,
                    &mut keep_spans,
                );
            if skip {
                skipped_scratch.clear();
                for &rect in &deco_scratch {
                    deco::intersect_rect_spans(&mut skipped_scratch, rect, &keep_spans);
                }
                for &[rx, ry, rw, rh] in &skipped_scratch {
                    if let Some((fx, fw)) = clip_span_to_run(rx, rw, lo, hi) {
                        self.fill_rect(pixels, w, h, fx, ry, fw, rh, ucolor);
                    }
                }
            } else {
                for &[rx, ry, rw, rh] in &deco_scratch {
                    if let Some((fx, fw)) = clip_span_to_run(rx, rw, lo, hi) {
                        self.fill_rect(pixels, w, h, fx, ry, fw, rh, ucolor);
                    }
                }
            }
            for (rect, ink) in [
                (
                    cell.strikethrough
                        .then(|| strike_rect(x, y0, dw, self.cell_h, dm))
                        .flatten(),
                    inks.strike,
                ),
                (
                    cell.overline
                        .then(|| overline_rect(x, y0, dw, self.cell_h, dm))
                        .flatten(),
                    inks.overline,
                ),
            ] {
                let Some([rx, ry, rw, rh]) = rect else {
                    continue;
                };
                if let Some((fx, fw)) = clip_span_to_run(rx, rw, lo, hi) {
                    self.fill_rect(pixels, w, h, fx, ry, fw, rh, ink);
                }
            }
        }
        if !curls.is_empty() {
            let mask = self.undercurl_mask();
            for &(c, x, cw, lo, hi, dw, ucolor) in &curls {
                let skip = self.underline_keep_spans_with_metrics(
                    input,
                    r,
                    c,
                    plan.get(c).copied().unwrap_or(ColumnGlyph::PerCell),
                    x,
                    dw,
                    dm,
                    &mut ink_scratch,
                    &mut keep_spans,
                );
                if !skip {
                    keep_spans.clear();
                    keep_spans.push((x, dw));
                }
                if !uniform {
                    keep_spans.retain_mut(|s| match clip_span_to_run(s.0, s.1, lo, hi) {
                        Some(v) => {
                            *s = v;
                            true
                        }
                        None => false,
                    });
                }
                self.blend_undercurl(pixels, w, h, y0, cw, dm, ucolor, &mask, &keep_spans);
            }
        }
        self.deco_scratch = deco_scratch;
        self.deco_skip_rect_scratch = skipped_scratch;
        self.ink_scratch = ink_scratch;
        self.keep_spans_scratch = keep_spans;
        self.curl_scratch = curls;
        self.glyph_plan_scratch = plan;
        self.glyph_plan_row = Some(r);
        if self.catch_unpredicted_overshoot(input, r, self.fg_ink_top, y0 as i32) {
            self.overshoot_surprise = true;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn blend_undercurl(
        &self,
        pixels: &mut [u32],
        w: usize,
        h: usize,
        y0: usize,
        rcw: usize,
        dm: deco::DecoMetrics,
        ucolor: u32,
        mask: &[u8],
        spans: &[(usize, usize)],
    ) {
        let bcw = self.cell_w;
        if bcw == 0 || rcw == 0 {
            return;
        }
        let pad = self.pad;
        let (band_top, band_bottom) = deco::undercurl_band(self.cell_h, dm);
        for &(sx, sw) in spans {
            for mx in sx..sx + sw {
                if mx < pad || mx >= w {
                    continue;
                }
                let tcol = deco::undercurl_tile_col(mx, pad, rcw, bcw);
                for yy in band_top..band_bottom {
                    let cov = mask[yy * bcw + tcol];
                    if cov == 0 {
                        continue;
                    }
                    let py = y0 + yy;
                    if py >= h {
                        continue;
                    }
                    let idx = py * w + mx;
                    pixels[idx] = blend(pixels[idx], ucolor, cov);
                }
            }
        }
    }

    /// Descender ink-skip: the kept absolute-x spans of cell `(r, c)`'s underline after zeroing coverage within a 1px dilation of the cell's own descender ink (strictly below the baseline row; letter bottoms on the baseline never count).
    #[allow(clippy::too_many_arguments)]
    pub fn underline_keep_spans_into(
        &mut self,
        input: &RenderInput,
        r: usize,
        c: usize,
        plan_entry: ColumnGlyph,
        x0: usize,
        dw: usize,
        ink: &mut Vec<bool>,
        out: &mut Vec<(usize, usize)>,
    ) -> bool {
        if !self.underline_skip_descenders || dw == 0 {
            return false;
        }
        let dm = self.deco_metrics();
        self.underline_keep_spans_with_metrics(input, r, c, plan_entry, x0, dw, dm, ink, out)
    }

    #[allow(clippy::too_many_arguments)]
    fn underline_keep_spans_with_metrics(
        &mut self,
        input: &RenderInput,
        r: usize,
        c: usize,
        plan_entry: ColumnGlyph,
        x0: usize,
        dw: usize,
        dm: deco::DecoMetrics,
        ink: &mut Vec<bool>,
        out: &mut Vec<(usize, usize)>,
    ) -> bool {
        if !self.underline_skip_descenders || dw == 0 {
            return false;
        }
        let line_size = if row_is_uniform(input, r) {
            input
                .line_sizes
                .get(r)
                .copied()
                .unwrap_or(LineSize::SingleWidth)
        } else {
            input.line_size_run_at(r, c).0
        };
        if !matches!(line_size, LineSize::SingleWidth) {
            return false;
        }
        let Some(row_cells) = input.cells.get(r) else {
            return false;
        };
        let Some(cell) = row_cells.get(c) else {
            return false;
        };
        let band = deco::underline_band(cell.underline, self.cell_h, dm);
        let Some((by0, by1)) = band else {
            return false;
        };
        if cell.ch == ' ' || cell.ch.is_control() || input.image_hides_glyph_at(r, c) {
            return false;
        }
        let is_wide_lead = row_cells.get(c + 1).is_some_and(|next| next.wide);
        let cell_cols = if cell.wide || is_wide_lead { 2 } else { 1 };
        if !deco::ink_skip_applies_to_width(cell_cols) {
            return false;
        }
        let key = match plan_entry {
            ColumnGlyph::Ligated(gid) => self.ligature_key(gid, cell_style(cell)),
            ColumnGlyph::LigatedSlice { gid, k, .. } => {
                self.ligature_slice_key(gid, k, cell_style(cell))
            }
            ColumnGlyph::PerCell => self.resolve_cell_key_for_span(
                input.cluster_at(r, c),
                cell,
                materialized_cell_span(row_cells, c),
            ),
        };
        let raster = input.chrome_raster(r);
        let key = self.chrome_icon_key(raster, row_cells, c).unwrap_or(key);
        let cell_h = self.cell_h as i32;
        let room = self.chrome_room_for(input);
        let lift = raster.map_or(0, |m| {
            i32::try_from(chrome_fit(m, room).0)
                .unwrap_or(0)
                .min(cell_h)
        });
        let baseline = self.baseline - lift;
        let img = self.glyph_image(key);
        let (gw, gh, xmin, ymin) = (img.width(), img.height(), img.xmin(), img.ymin());
        if gw == 0 || gh == 0 {
            return false;
        }
        let ink_at = |gx: usize, gy: usize| match img {
            GlyphImage::Mono { bytes, .. } => bytes[gy * gw + gx] > 0,
            GlyphImage::Rgba { bytes, .. } => bytes[(gy * gw + gx) * 4 + 3] > 0,
        };
        let gy0 = baseline - gh as i32 - ymin;
        let (ylo, mut yhi) = ((by0 as i32 - 1).max(baseline + 1), by1 as i32 + 1);
        if lift > 0 {
            yhi = yhi.min(cell_h - lift);
        }
        ink.clear();
        ink.resize(dw, false);
        let gy_start = (ylo - gy0).clamp(0, gh as i32) as usize;
        let gy_end = (yhi - gy0).clamp(0, gh as i32) as usize;
        if gy_start >= gy_end {
            return false;
        }
        let mut any = false;
        for gy in gy_start..gy_end {
            for gx in 0..gw {
                if ink_at(gx, gy) {
                    let col = xmin + gx as i32;
                    if col >= 0 && (col as usize) < dw {
                        ink[col as usize] = true;
                        any = true;
                    }
                }
            }
        }
        if !any {
            return false;
        }
        out.clear();
        deco::keep_spans_after_ink(ink, out);
        for s in out.iter_mut() {
            s.0 += x0;
        }
        true
    }

    fn draw_trail(
        &self,
        pixels: &mut [u32],
        w: usize,
        h: usize,
        input: &RenderInput,
        dirty: Option<&[bool]>,
    ) {
        if input.cursor_trail.is_empty() {
            return;
        }
        let (rows, cols) = (input.rows, input.cols);
        let bg_t = self.bg_transmittance();
        let grid_w = cols * self.cell_w;
        for t in &input.cursor_trail {
            if t.row >= rows || t.col >= cols {
                continue;
            }
            if let Some(d) = dirty
                && !d.get(t.row).copied().unwrap_or(false)
            {
                continue;
            }
            let y0 = self.grid_top() + t.row * self.cell_h;
            let place = if row_is_uniform(input, t.row) {
                let cw = row_cell_w(input.line_sizes[t.row], self.cell_w);
                if (t.col + 1) * cw > grid_w {
                    None
                } else {
                    Some((self.pad + t.col * cw, cw))
                }
            } else {
                let p = self.mixed_cell_place(input, t.row, t.col, y0);
                clip_span_to_run(p.x, p.w, p.lo, p.hi)
            };
            let Some((x0, cw)) = place else {
                continue;
            };
            let default_bg = resolved_default_bg_at(input, t.row, t.col, self.theme.bg);
            let cell_bg = input
                .cells
                .get(t.row)
                .and_then(|r| r.get(t.col))
                .map_or(default_bg, |c| rgb_to_u32(c.bg));
            let mut color = blend(cell_bg, input.cursor_trail_color, t.alpha);
            if bg_t != 0 && cell_bg == default_bg {
                color |= bg_t << 24;
            }
            self.fill_rect(pixels, w, h, x0, y0, cw, self.cell_h, color);
        }
    }

    fn draw_glow(
        &self,
        pixels: &mut [u32],
        w: usize,
        h: usize,
        input: &RenderInput,
        dirty: Option<&[bool]>,
    ) {
        self.draw_glow_stream(
            pixels,
            (w, h),
            input.rows,
            (&input.cursor_glow_add, &input.cursor_glow_add_ramps),
            dirty,
        );
    }

    /// A window-space glow stream, its quads and ramps in draw order.
    #[inline]
    fn draw_glow_stream(
        &self,
        pixels: &mut [u32],
        (w, h): (usize, usize),
        rows: usize,
        (quads, ramps): (&[GlowQuad], &GlowRamps),
        dirty: Option<&[bool]>,
    ) {
        use glow_ramps::Lights;
        glow_ramps::each_laid(quads, ramps, |laid| match laid {
            Lights::Quads(run) => self.draw_flat_add(pixels, w, h, rows, run, dirty, 0, 0),
            Lights::Ramp(ramp) => {
                let row = usize::from(ramp.row);
                if row < rows && dirty.is_none_or(|d| d.get(row).copied().unwrap_or(false)) {
                    glow_ramps::draw_ramp(pixels, (w, h), ramp, &ramps.ends, (0, 0));
                }
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_flat_add(
        &self,
        pixels: &mut [u32],
        w: usize,
        h: usize,
        rows: usize,
        quads: &[GlowQuad],
        dirty: Option<&[bool]>,
        off_x: usize,
        off_y: usize,
    ) {
        if quads.is_empty() {
            return;
        }
        for q in quads {
            if q.row as usize >= rows {
                continue;
            }
            if let Some(d) = dirty
                && !d.get(q.row as usize).copied().unwrap_or(false)
            {
                continue;
            }
            let x0 = off_x + q.x as usize;
            let y0 = off_y + q.y as usize;
            let premul = q.color;
            let xend = (x0 + q.w as usize).min(w);
            if x0 >= xend {
                continue;
            }
            let (c0, c1) = (
                FxInk::over(premul, q.alpha),
                FxInk::over(q.color2, q.alpha2),
            );
            let qw = u32::from(q.w);
            for y in y0..(y0 + q.h as usize).min(h) {
                fx_raster::fill_glow_span(&mut pixels[y * w + x0..y * w + xend], 0, qw, c0, c1);
            }
        }
    }

    /// The frame the fx engine slices a `w`×`h` buffer of `rows` grid rows against under this renderer's geometry.
    #[inline]
    #[must_use]
    pub fn fx_frame(&self, w: usize, h: usize, rows: usize) -> FxFrame {
        FxFrame {
            w,
            h,
            pad: self.pad,
            grid_top: self.grid_top(),
            cell_h: self.cell_h,
            rows,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_fx(
        &mut self,
        pixels: &mut [u32],
        w: usize,
        h: usize,
        input: &RenderInput,
        slot: FxSlot,
        blend: FxBlend,
        dirty: Option<&[bool]>,
    ) {
        self.draw_picked_fx(pixels, w, h, input, (slot, blend, Knock::Any), dirty);
    }

    /// [`Self::draw_fx`] of the layers `pick` takes ([`Knock`]).
    fn draw_picked_fx(
        &mut self,
        pixels: &mut [u32],
        w: usize,
        h: usize,
        input: &RenderInput,
        pick: (FxSlot, FxBlend, Knock),
        dirty: Option<&[bool]>,
    ) {
        if input.fx_layers.is_empty() {
            return;
        }
        let frame = self.fx_frame(w, h, input.rows);
        fx_raster::draw_picked_layers(
            pixels,
            &frame,
            &input.fx_layers,
            &input.fx_occluders,
            pick,
            dirty,
            &mut self.fx_scratch,
        );
    }

    #[inline]
    fn draw_glow_under(
        &self,
        pixels: &mut [u32],
        w: usize,
        h: usize,
        input: &RenderInput,
        dirty: Option<&[bool]>,
    ) {
        self.draw_glow_stream(
            pixels,
            (w, h),
            input.rows,
            (&input.glow_under, &input.glow_under_ramps),
            dirty,
        );
    }

    fn draw_fire_patch(
        &self,
        pixels: &mut [u32],
        w: usize,
        h: usize,
        input: &RenderInput,
        dirty: Option<&[bool]>,
    ) {
        if input.fire_patch.is_empty() {
            return;
        }
        for mode in [FireMode::Add, FireMode::Over] {
            for q in input.fire_patch.iter().filter(|q| q.mode == mode) {
                if q.row as usize >= input.rows {
                    continue;
                }
                if let Some(d) = dirty
                    && !d.get(q.row as usize).copied().unwrap_or(false)
                {
                    continue;
                }
                let x0 = q.x as usize;
                let y0 = q.y as usize;
                let xend = (x0 + q.w as usize).min(w);
                if x0 >= xend {
                    continue;
                }
                let fp = fire_field::FireFieldParams {
                    base_y: q.base_y as i32,
                    peak_h: q.peak_h as i32,
                    phase: q.phase,
                    temp: q.temp as i32,
                    strength: q.strength as i32,
                    lean: q.lean as i32,
                    cov_cap: q.cov_cap as i32,
                    cell_h: q.cell_h as i32,
                    top_fade_y: self.pad as i32,
                };
                let pc = fire_field::fire_precomp(&fp);
                for y in y0..(y0 + q.h as usize).min(h) {
                    let tf = fire_field::fire_top_fade(y as i32, &fp);
                    let mut sampler = fire_field::FireRow::new(y as i32, x0 as i32, &fp, &pc);
                    let row = &mut pixels[y * w..y * w + w];
                    for (x, px) in row.iter_mut().enumerate().take(xend).skip(x0) {
                        let c = sampler.core(x as i32);
                        match mode {
                            FireMode::Add => {
                                let pm = fire_field::fire_shade_add(&c, &fp, tf);
                                if pm != 0 {
                                    *px = add_sat(*px, pm);
                                }
                            }
                            FireMode::Over => {
                                let (rgb, a) = fire_field::fire_shade_over(&c, &fp, tf);
                                if a != 0 {
                                    *px = over_rgb(*px, rgb, a);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[inline]
    fn draw_nova(
        &self,
        pixels: &mut [u32],
        w: usize,
        h: usize,
        input: &RenderInput,
        dirty: Option<&[bool]>,
    ) {
        self.draw_flat_add(
            pixels,
            w,
            h,
            input.rows,
            &input.nova_add,
            dirty,
            self.pad,
            self.grid_top(),
        );
    }

    #[inline]
    fn draw_glow_halo(
        &self,
        pixels: &mut [u32],
        w: usize,
        h: usize,
        input: &RenderInput,
        dirty: Option<&[bool]>,
    ) {
        self.draw_radial_add(pixels, w, h, input.rows, &input.glow_halo, dirty, 0, 0);
    }

    #[inline]
    fn draw_rain_add(
        &self,
        pixels: &mut [u32],
        w: usize,
        h: usize,
        input: &RenderInput,
        dirty: Option<&[bool]>,
    ) {
        self.draw_radial_add(
            pixels,
            w,
            h,
            input.rows,
            &input.rain_add,
            dirty,
            self.pad,
            self.grid_top(),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_radial_add(
        &self,
        pixels: &mut [u32],
        w: usize,
        h: usize,
        rows: usize,
        quads: &[RainHalo],
        dirty: Option<&[bool]>,
        off_x: usize,
        off_y: usize,
    ) {
        if quads.is_empty() {
            return;
        }
        for mode in [HaloMode::Add, HaloMode::Over] {
            for q in quads.iter().filter(|q| q.mode == mode) {
                if q.row as usize >= rows {
                    continue;
                }
                if let Some(d) = dirty
                    && !d.get(q.row as usize).copied().unwrap_or(false)
                {
                    continue;
                }
                let x0 = off_x + q.x as usize;
                let y0 = off_y + q.y as usize;
                let color = q.color;
                let over_cap = halo_over_cap(color);
                let xend = (x0 + q.w as usize).min(w);
                if x0 >= xend {
                    continue;
                }
                let cx = off_x as i32 + q.cx as i32;
                let cy = off_y as i32 + q.cy as i32;
                let rx2 = (q.rx as i32) * (q.rx as i32);
                let ry2 = (q.ry as i32) * (q.ry as i32);
                for y in y0..(y0 + q.h as usize).min(h) {
                    let ny = halo_row_ny(y as i32 - cy, ry2);
                    if ny >= 256 {
                        continue;
                    }
                    let span = ((((256 - ny) * rx2) >> 8) as u32).isqrt() as i32 + 1;
                    let xs = (cx - span).max(x0 as i32).max(0) as usize;
                    let xe = ((cx + span + 1).max(0) as usize).min(xend);
                    let row = &mut pixels[y * w..y * w + w];
                    for (x, px) in row.iter_mut().enumerate().take(xe).skip(xs) {
                        let wt = halo_weight(x as i32 - cx, ny, rx2);
                        if wt == 0 {
                            continue;
                        }
                        let wt = wt.min(255) as u8;
                        *px = match mode {
                            HaloMode::Add => add_sat(*px, premul_rgb(color, wt)),
                            HaloMode::Over => over_rgb(*px, color, wt.min(over_cap)),
                        };
                    }
                }
            }
        }
    }

    fn draw_decorations(
        &self,
        pixels: &mut [u32],
        w: usize,
        h: usize,
        input: &RenderInput,
        dirty: Option<&[bool]>,
    ) {
        if input.word_decorations.is_empty() {
            return;
        }
        let (rows, cols) = (input.rows, input.cols);
        let bcw = self.cell_w;
        let ch = self.cell_h;
        if bcw == 0 || ch == 0 {
            return;
        }
        for d in &input.word_decorations {
            let (row, col) = (d.row as usize, d.col as usize);
            if row >= rows || col >= cols {
                continue;
            }
            if let Some(dd) = dirty
                && !dd.get(row).copied().unwrap_or(false)
            {
                continue;
            }
            let (rcw, ox, lo, hi) = if row_is_uniform(input, row) {
                let rcw = row_cell_w(input.line_sizes[row], self.cell_w);
                let ox = self.pad as isize + (col * rcw) as isize + d.dx as isize;
                (rcw, ox, 0, w)
            } else {
                let p = self.mixed_cell_place(input, row, col, self.grid_top() + row * self.cell_h);
                (p.w, p.x as isize + d.dx as isize, p.lo, p.hi.min(w))
            };
            if rcw == 0 {
                continue;
            }
            if matches!(d.blend, DecoBlend::Add) && input.any_selection() {
                let row_cells = &input.cells[row];
                let is_wide_lead = row_cells.get(col + 1).is_some_and(|n| n.wide);
                let cell_wide = row_cells.get(col).is_some_and(|n| n.wide);
                if input.selection_contains_cell(row, col, is_wide_lead, cell_wide) {
                    continue;
                }
            }
            let mask = self.deco_mask(d.glyph, bcw, ch);
            let oy = self.grid_top() as isize + (row * self.cell_h) as isize + d.dy as isize;
            let one_to_one = rcw == bcw;
            for my in 0..ch {
                let py = oy + my as isize;
                if py < 0 || py as usize >= h {
                    continue;
                }
                let prow = py as usize * w;
                for mx in 0..rcw {
                    let sx = if one_to_one {
                        mx
                    } else {
                        ((((mx as f32 + 0.5) / rcw as f32) * bcw as f32) as usize).min(bcw - 1)
                    };
                    let cov = mask[my * bcw + sx];
                    if cov == 0 {
                        continue;
                    }
                    let px = ox + mx as isize;
                    if px < lo as isize || px as usize >= hi {
                        continue;
                    }
                    let a = ((u32::from(cov) * u32::from(d.alpha) + 127) / 255) as u8;
                    if a == 0 {
                        continue;
                    }
                    let idx = prow + px as usize;
                    pixels[idx] = match d.blend {
                        DecoBlend::Add => add_sat(pixels[idx], premul_rgb(d.color, a)),
                        DecoBlend::Over => blend(pixels[idx], d.color, a),
                    };
                }
            }
        }
    }

    fn deco_mask(&self, glyph: DecoGlyph, bcw: usize, ch: usize) -> std::sync::Arc<Vec<u8>> {
        let key = (glyph, bcw, ch);
        let mut cache = self.deco_masks.borrow_mut();
        if let Some((_, m)) = cache.iter().find(|(k, _)| *k == key) {
            return m.clone();
        }
        let m = std::sync::Arc::new(crate::procedural::deco_coverage(glyph, bcw, ch));
        cache.push((key, m.clone()));
        m
    }

    fn draw_cursor(&mut self, pixels: &mut [u32], w: usize, h: usize, input: &RenderInput) {
        let (rows, cols) = (input.rows, input.cols);
        let (cr, cc) = (input.cursor_row, input.cursor_col);
        let style = effective_cursor_style(input, self.cursor_style_override);
        let min_contrast = self.effective_minimum_contrast();
        if cr < rows
            && cc < cols
            && input.cursor_visible
            && cursor_shown(style, self.cursor_blink_phase)
        {
            let (pad_x, pad_y) = (self.pad, self.grid_top());
            let y0 = pad_y + cr * self.cell_h;
            let uniform = row_is_uniform(input, cr);
            let (cw, scale, anchor_y, x0, run_lo, run_hi) = if uniform {
                let line_size = input.line_sizes[cr];
                let cw = row_cell_w(line_size, self.cell_w);
                let (scale, anchor_y) = chrome_lifted(
                    input,
                    cr,
                    y0,
                    self.cell_h,
                    self.chrome_room,
                    row_scale(line_size, y0, self.cell_h, cr + 1 == rows),
                );
                (cw, scale, anchor_y, pad_x + cc * cw, 0, usize::MAX)
            } else {
                let p = self.mixed_cell_place(input, cr, cc, y0);
                (p.w, p.scale, p.anchor_y, p.x, p.lo, p.hi)
            };
            let is_block = matches!(style, CursorStyle::BlinkingBlock | CursorStyle::SteadyBlock);
            let wide_lead = input.cells[cr].get(cc + 1).is_some_and(|n| n.wide);
            let cur_w = if is_block && wide_lead { 2 * cw } else { cw };
            let cursor_cell_bg = input.cells[cr].get(cc).map_or_else(
                || resolved_default_bg_at(input, cr, cc, self.theme.bg),
                |cell| rgb_to_u32(cell.bg),
            );
            let base_fill = match input.cursor_fill_override {
                Some(fill) => fill,
                None => self.frame_cursor(input),
            };
            let cursor_fill = floor_cursor_fill(base_fill, cursor_cell_bg, min_contrast);
            let cursor_cov = (self.cursor_opacity * 255.0).round() as u8;
            if cursor_cov < 255 {
                for_each_cursor_rect(style, x0, y0, cur_w, self.cell_h, |[rx, ry, rw, rh]| {
                    if let Some((fx, fw)) = clip_span_to_run(rx, rw, run_lo, run_hi) {
                        self.blend_rect(pixels, w, h, fx, ry, fw, rh, cursor_fill, cursor_cov);
                    }
                });
                return;
            }
            for_each_cursor_rect(style, x0, y0, cur_w, self.cell_h, |[rx, ry, rw, rh]| {
                if let Some((fx, fw)) = clip_span_to_run(rx, rw, run_lo, run_hi) {
                    self.fill_rect(pixels, w, h, fx, ry, fw, rh, cursor_fill);
                }
            });
            if is_block
                && let Some(cell) = input.cells[cr].get(cc).copied()
                && !cell.wide
                && cell.ch != ' '
                && !cell.ch.is_control()
            {
                let mut plan = std::mem::take(&mut self.glyph_plan_scratch);
                if self.glyph_plan_row == Some(cr) {
                    #[cfg(debug_assertions)]
                    {
                        let mut dbg_breaks = Vec::new();
                        ligature_break_cols_into(input, cr, &self.shaping, &mut dbg_breaks);
                        let mut dbg_plan = Vec::new();
                        self.row_glyph_plan(input, cr, &dbg_breaks, &mut dbg_plan);
                        debug_assert_eq!(
                            plan, dbg_plan,
                            "cursor-row glyph-plan reuse diverged from a fresh plan"
                        );
                    }
                } else {
                    let mut break_cols = std::mem::take(&mut self.ligature_break_scratch);
                    ligature_break_cols_into(input, cr, &self.shaping, &mut break_cols);
                    self.row_glyph_plan(input, cr, &break_cols, &mut plan);
                    self.ligature_break_scratch = break_cols;
                }
                let clip = Scale {
                    clip_x0: scale.clip_x0.max(x0 as i32),
                    clip_x1: scale.clip_x1.min((x0 + cur_w) as i32),
                    ..scale
                };
                let cut_color = rgb_to_u32(cell.bg);
                let (lo, hi) = cursor_cutout_cols(&plan, cc);
                for c in lo..=hi {
                    let Some(src) = input.cells[cr].get(c).copied() else {
                        continue;
                    };
                    if src.wide || src.ch == ' ' || src.ch.is_control() {
                        continue;
                    }
                    let key = match plan.get(c).copied().unwrap_or(ColumnGlyph::PerCell) {
                        ColumnGlyph::Ligated(gid) => self.ligature_key(gid, cell_style(&src)),
                        ColumnGlyph::LigatedSlice { gid, k, .. } => {
                            self.ligature_slice_key(gid, k, cell_style(&src))
                        }
                        ColumnGlyph::PerCell => {
                            let cluster = cluster_for(&input.clusters[cr], c);
                            self.resolve_cell_key_for_span(
                                cluster,
                                &src,
                                materialized_cell_span(&input.cells[cr], c),
                            )
                        }
                    };
                    let cx = if uniform {
                        pad_x + c * cw
                    } else {
                        self.mixed_cell_place(input, cr, c, y0).x
                    };
                    let key = shade_phase_key(key, cx, y0);
                    self.blit(
                        pixels,
                        w,
                        cx as i32,
                        anchor_y,
                        key,
                        cut_color,
                        cursor_fill,
                        clip,
                    );
                }
                self.glyph_plan_scratch = plan;
                self.glyph_plan_row = Some(cr);
            }
        }
    }

    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn blend_rect(
        &self,
        px: &mut [u32],
        w: usize,
        h: usize,
        x0: usize,
        y0: usize,
        rw: usize,
        rh: usize,
        color: u32,
        t: u8,
    ) {
        let x1 = (x0 + rw).min(w);
        if x0 >= x1 {
            return;
        }
        for y in y0..(y0 + rh).min(h) {
            let base = y * w;
            for p in &mut px[base + x0..base + x1] {
                *p = blend(*p, color, t);
            }
        }
    }

    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn fill_rect(
        &self,
        px: &mut [u32],
        w: usize,
        h: usize,
        x0: usize,
        y0: usize,
        rw: usize,
        rh: usize,
        color: u32,
    ) {
        let x1 = (x0 + rw).min(w);
        if x0 >= x1 {
            return;
        }
        for y in y0..(y0 + rh).min(h) {
            let base = y * w;
            px[base + x0..base + x1].fill(color);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn blit_image_cell(
        &self,
        ic: &mut ImageCache,
        px: &mut [u32],
        w: usize,
        h: usize,
        x0: usize,
        y0: usize,
        image: &aterm_core::grid::extra::ImageRef,
        x_clip: usize,
    ) {
        let cw = self.cell_w;
        let ch = self.cell_h;
        let lift = usize::from(image.image.band_lift_px);
        let (fp_w, fp_h) = placement_footprint(&image.image, (cw, ch));
        let decoded = ic.get_or_decode(&image.image, fp_w, fp_h);
        if decoded.rgba.is_empty() || decoded.w == 0 {
            return;
        }
        let sx0 = image.cell_col as usize * cw;
        let lip = if image.cell_row == 0 { lift } else { 0 };
        let sy0 = image.cell_row as usize * ch + lift - lip;
        let scaled = (decoded.w, decoded.h) != (fp_w, fp_h);
        let n = cw
            .min(w.min(x_clip).saturating_sub(x0))
            .min(fp_w.saturating_sub(sx0));
        for dy in 0..ch + lip {
            let py = (y0 + dy).saturating_sub(lip);
            if py >= h {
                break;
            }
            let fy = sy0 + dy;
            if fy >= fp_h {
                break;
            }
            let sy = if scaled {
                placement_raster_coord(fy, fp_h, decoded.h)
            } else {
                fy
            };
            for dx in 0..n {
                let pxx = x0 + dx;
                let sx = if scaled {
                    placement_raster_coord(sx0 + dx, fp_w, decoded.w)
                } else {
                    sx0 + dx
                };
                let si = (sy * decoded.w + sx) * 4;
                let (r, g, b, a) = (
                    decoded.rgba[si],
                    decoded.rgba[si + 1],
                    decoded.rgba[si + 2],
                    decoded.rgba[si + 3],
                );
                let di = py * w + pxx;
                px[di] = blend(px[di], rgb_to_u32([r, g, b]), a);
            }
        }
    }

    /// Resolve a cell to its glyph key.
    pub fn resolve_cell_key(&mut self, cluster: Option<&str>, cell: &RenderCell) -> GlyphKey {
        if cell.text_presentation {
            let key = self.glyph_key_text_styled(cell.ch, cell_style(cell));
            return sub_nudged(key, cell.script);
        }
        if let Some(cl) = cluster {
            if let Some(k) = self.glyph_key_cluster(cl) {
                return small_emoji_keyed(k, cell.script);
            }
            if let Some(k) = regional_pair_key(cl, cell_style(cell), self.px_q) {
                return small_emoji_keyed(k, cell.script);
            }
        }
        if cell.emoji_presentation {
            small_emoji_keyed(self.glyph_key_emoji(cell.ch), cell.script)
        } else {
            let key = self.glyph_key_styled(cell.ch, cell_style(cell));
            sub_nudged(key, cell.script)
        }
    }

    /// Resolve a materialized cell like [`Self::resolve_cell_key`], then stamp the raster box into its [`GlyphKey`] identity.
    pub fn resolve_cell_key_for_span(
        &mut self,
        cluster: Option<&str>,
        cell: &RenderCell,
        cell_span: u8,
    ) -> GlyphKey {
        let mut key = self.resolve_cell_key(cluster, cell);
        let primary_mono = matches!(key.glyph_class, GlyphClass::Mono | GlyphClass::MonoGid);
        let text_presentation = cell.text_presentation
            && matches!(key.source, FaceId::Primary | FaceId::BoldPrimary)
            && primary_mono;
        let two_cell = self.primary_fits_two_cell_glyphs
            && cell_span == 1
            && cluster.is_none()
            && key.source == FaceId::Primary
            && primary_mono
            && bundled::is_two_cell_glyph(cell.ch);
        if text_presentation || two_cell {
            key.cell_span = 1;
        } else if key.glyph_class == GlyphClass::Mono
            && (cell_fitted_source(key.source) || is_synthetic_symbol_key(key))
        {
            key.cell_span = cell_span.min(2);
        }
        key
    }

    #[allow(clippy::too_many_arguments)]
    fn blit(
        &mut self,
        px: &mut [u32],
        stride: usize,
        cell_x: i32,
        cell_y: i32,
        key: GlyphKey,
        color: u32,
        bg: u32,
        scale: Scale,
    ) {
        let baseline = self.baseline;
        let corrected = self.text_blending == TextBlending::LinearCorrected;
        let (clip_y0, clip_y1) = (
            scale.clip_y0.max(self.fg_clip.0),
            scale.clip_y1.min(self.fg_clip.1),
        );
        #[cfg(all(unix, not(target_os = "macos")))]
        if self.subpx_frame
            && scale.xs.max(1) == 1
            && scale.ys.max(1) == 1
            && let Some(img) = self.subpx_image(key)
        {
            if img.width == 0 || img.height == 0 {
                return;
            }
            let gx0 = cell_x + img.xmin;
            let gy0 = cell_y + (baseline - img.height as i32 - img.ymin);
            let reach = gy0.max(scale.clip_y0);
            blit_subpx_1x(
                px,
                stride,
                gx0,
                gy0,
                img.width,
                img.height,
                &img.bytes,
                color,
                bg,
                corrected,
                clip_y0,
                clip_y1,
                scale.clip_x0,
                scale.clip_x1,
            );
            self.fg_ink_top = self.fg_ink_top.min(reach);
            return;
        }
        let mut bank = self.alpha_memo.take();
        let entry = self.cached_glyph(key);
        let (img, ink) = (&entry.img, entry.ink);
        let (width, height, xmin, ymin) = (img.width(), img.height(), img.xmin(), img.ymin());
        if width == 0 || height == 0 {
            self.alpha_memo = bank;
            return;
        }
        let (xs, ys) = (scale.xs.max(1), scale.ys.max(1));
        let (clip_x0, clip_x1) = (scale.clip_x0, scale.clip_x1);
        let gx0 = cell_x + xmin * xs as i32;
        let gy0 = cell_y + ys as i32 * (baseline - height as i32 - ymin);
        let reach = ink.map_or(i32::MAX, |(ink_h, ink_ymin)| {
            (cell_y + ys as i32 * (baseline - ink_ymin - ink_h as i32)).max(scale.clip_y0)
        });
        let one_to_one = xs == 1 && ys == 1;
        match img {
            GlyphImage::Mono { bytes, .. } => {
                let memo =
                    corrected.then(|| bank.get_or_insert_with(Box::default).way_for(color, bg));
                if one_to_one {
                    blit_mono_1x(
                        px, stride, gx0, gy0, width, height, bytes, color, clip_y0, clip_y1,
                        clip_x0, clip_x1, memo,
                    );
                } else {
                    blit_mono_scaled(
                        px, stride, gx0, gy0, width, height, bytes, color, xs, ys, clip_y0,
                        clip_y1, clip_x0, clip_x1, memo,
                    );
                }
            }
            GlyphImage::Rgba { bytes, .. } => {
                if one_to_one {
                    blit_rgba_1x(
                        px, stride, gx0, gy0, width, height, bytes, clip_y0, clip_y1, clip_x0,
                        clip_x1,
                    );
                } else {
                    blit_rgba_scaled(
                        px, stride, gx0, gy0, width, height, bytes, xs, ys, clip_y0, clip_y1,
                        clip_x0, clip_x1,
                    );
                }
            }
        }
        self.alpha_memo = bank;
        self.fg_ink_top = self.fg_ink_top.min(reach);
    }

    #[allow(clippy::too_many_arguments)]
    fn blit_glyph_halo(
        &mut self,
        px: &mut [u32],
        stride: usize,
        cell_x: i32,
        cell_y: i32,
        key: GlyphKey,
        scale: Scale,
        alpha: u8,
    ) {
        let baseline = self.baseline;
        let (clip_y0, clip_y1) = (
            scale.clip_y0.max(self.fg_clip.0),
            scale.clip_y1.min(self.fg_clip.1),
        );
        let entry = self.cached_glyph(key);
        let GlyphImage::Mono {
            width,
            height,
            xmin,
            ymin,
            bytes,
            ..
        } = &entry.img
        else {
            return;
        };
        let (width, height, xmin, ymin) = (*width, *height, *xmin, *ymin);
        if width == 0 || height == 0 {
            return;
        }
        let (xs, ys) = (scale.xs.max(1), scale.ys.max(1));
        let gx0 = cell_x + xmin * xs as i32;
        let gy0 = cell_y + ys as i32 * (baseline - height as i32 - ymin);
        let lift = HALO_DILATE_OFFSETS
            .iter()
            .map(|&(_, dy)| dy)
            .min()
            .unwrap_or(0);
        let reach = entry.ink.map_or(i32::MAX, |(ink_h, ink_ymin)| {
            (cell_y + ys as i32 * (baseline - ink_ymin - ink_h as i32) + lift).max(scale.clip_y0)
        });
        for &(dx, dy) in &HALO_DILATE_OFFSETS {
            blit_mono_halo(
                px,
                stride,
                gx0 + dx,
                gy0 + dy,
                width,
                height,
                bytes,
                HALO_IN_FIRE_RGB,
                alpha,
                xs,
                ys,
                clip_y0,
                clip_y1,
                scale.clip_x0,
                scale.clip_x1,
            );
        }
        self.fg_ink_top = self.fg_ink_top.min(reach);
    }
}

/// The visible sub-span of a glyph's destination extent under a clip window.
#[inline]
#[must_use]
pub fn clip_span(gx0: i64, len: i64, clip_lo: i64, clip_hi: i64) -> Option<(i64, i64)> {
    let lo = gx0.max(clip_lo);
    let hi = (gx0 + len).min(clip_hi);
    (lo < hi).then_some((lo, hi))
}

#[inline]
#[allow(clippy::too_many_arguments)]
fn fb_at(
    x: i32,
    y: i32,
    stride: usize,
    px_len: usize,
    clip_y0: i32,
    clip_y1: i32,
    clip_x0: i32,
    clip_x1: i32,
) -> Option<usize> {
    if x < 0
        || y < clip_y0
        || y >= clip_y1
        || y < 0
        || x < clip_x0
        || x >= clip_x1
        || x as usize >= stride
    {
        return None;
    }
    let idx = y as usize * stride + x as usize;
    (idx < px_len).then_some(idx)
}

#[allow(clippy::too_many_arguments)]
fn blit_mono_scaled(
    px: &mut [u32],
    stride: usize,
    gx0: i32,
    gy0: i32,
    width: usize,
    height: usize,
    bytes: &[u8],
    color: u32,
    xs: usize,
    ys: usize,
    clip_y0: i32,
    clip_y1: i32,
    clip_x0: i32,
    clip_x1: i32,
    mut memo: Option<&mut AlphaMemo>,
) {
    let px_len = px.len();
    let cmasked = color & 0x00FF_FFFF;
    for j in 0..height {
        let srow = &bytes[j * width..j * width + width];
        for (i, &cov) in srow.iter().enumerate() {
            if cov == 0 {
                continue;
            }
            for sy in 0..ys {
                let y = gy0 + (j * ys + sy) as i32;
                for sx in 0..xs {
                    if let Some(idx) = fb_at(
                        gx0 + (i * xs + sx) as i32,
                        y,
                        stride,
                        px_len,
                        clip_y0,
                        clip_y1,
                        clip_x0,
                        clip_x1,
                    ) {
                        px[idx] = if cov == 255 {
                            cmasked
                        } else if let Some(memo) = memo.as_deref_mut() {
                            blend_text_pre(px[idx], color, cov, memo)
                        } else {
                            blend(px[idx], color, cov)
                        };
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn blit_mono_1x(
    px: &mut [u32],
    stride: usize,
    gx0: i32,
    gy0: i32,
    width: usize,
    height: usize,
    bytes: &[u8],
    color: u32,
    clip_y0: i32,
    clip_y1: i32,
    clip_x0: i32,
    clip_x1: i32,
    mut memo: Option<&mut AlphaMemo>,
) {
    let px_len = px.len() as i64;
    let cmasked = color & 0x00FF_FFFF;
    let gx = gx0 as i64;
    let w = width as i64;
    for j in 0..height {
        let y = gy0 + j as i32;
        if y < clip_y0 || y >= clip_y1 || y < 0 {
            continue;
        }
        let yrow = y as i64 * stride as i64;
        let x_lo = (clip_x0 as i64).max(0);
        let x_cap = (stride as i64).min(px_len - yrow).min(clip_x1 as i64);
        let Some((vx0, vx1)) = clip_span(gx, w, x_lo, x_cap) else {
            continue;
        };
        let (i_lo, i_hi) = ((vx0 - gx) as usize, (vx1 - gx) as usize);
        let row_off = j * width;
        let srow = &bytes[row_off + i_lo..row_off + i_hi];
        let drow = &mut px[(yrow + vx0) as usize..(yrow + vx1) as usize];
        for (dst, &cov) in drow.iter_mut().zip(srow) {
            if cov == 0 {
                continue;
            }
            *dst = if cov == 255 {
                cmasked
            } else if let Some(memo) = memo.as_deref_mut() {
                blend_text_pre(*dst, color, cov, memo)
            } else {
                blend(*dst, color, cov)
            };
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
#[derive(Clone, Copy)]
struct SubpxChanPre {
    fg_l: f32,
    bg_l: f32,
    fg_s: f32,
    bg_s: f32,
    degenerate: bool,
}

#[cfg(all(unix, not(target_os = "macos")))]
struct SubpxBlendPre {
    corrected: bool,
    ch: [SubpxChanPre; 3],
}

#[cfg(all(unix, not(target_os = "macos")))]
impl SubpxBlendPre {
    fn new(fg: u32, bg: u32, corrected: bool) -> Self {
        let mk = |sh: u32| {
            let fb = ((fg >> sh) & 0xff) as usize;
            let bb = ((bg >> sh) & 0xff) as usize;
            let fg_l = SRGB_TO_LINEAR_LUT[fb];
            let bg_l = SRGB_TO_LINEAR_LUT[bb];
            SubpxChanPre {
                fg_l,
                bg_l,
                fg_s: fb as f32 / 255.0,
                bg_s: bb as f32 / 255.0,
                degenerate: (fg_l - bg_l).abs() < TEXT_BLEND_EPS,
            }
        };
        Self {
            corrected,
            ch: [mk(16), mk(8), mk(0)],
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn blend_text_subpx(dst: u32, fg: u32, pre: &SubpxBlendPre, cov: [u8; 3]) -> u32 {
    let ch = |i: usize, sh: u32| -> (u32, f32) {
        let t = cov[i];
        let d = (dst >> sh) & 0xff;
        let f = (fg >> sh) & 0xff;
        match t {
            0 => (d, 0.0),
            255 => (f, 1.0),
            _ => {
                let mut a = f32::from(t) / 255.0;
                let p = &pre.ch[i];
                if pre.corrected && !p.degenerate {
                    let blend_l = srgb_to_linear(p.fg_s * a + p.bg_s * (1.0 - a));
                    a = ((blend_l - p.bg_l) / (p.fg_l - p.bg_l)).clamp(0.0, 1.0);
                }
                (blend_channel_linear(d, f, a), a)
            }
        }
    };
    let (r, _) = ch(0, 16);
    let (g, ag) = ch(1, 8);
    let (b, _) = ch(2, 0);
    let tt = (((dst >> 24) & 0xff) as f32 * (1.0 - ag)).round() as u32;
    (tt << 24) | (r << 16) | (g << 8) | b
}

#[cfg(all(unix, not(target_os = "macos")))]
#[allow(clippy::too_many_arguments)]
fn blit_subpx_1x(
    px: &mut [u32],
    stride: usize,
    gx0: i32,
    gy0: i32,
    width: usize,
    height: usize,
    bytes: &[u8],
    color: u32,
    bg: u32,
    corrected: bool,
    clip_y0: i32,
    clip_y1: i32,
    clip_x0: i32,
    clip_x1: i32,
) {
    let pre = SubpxBlendPre::new(color, bg, corrected);
    let px_len = px.len() as i64;
    let gx = gx0 as i64;
    let w = width as i64;
    for j in 0..height {
        let y = gy0 + j as i32;
        if y < clip_y0 || y >= clip_y1 || y < 0 {
            continue;
        }
        let yrow = y as i64 * stride as i64;
        let x_lo = (clip_x0 as i64).max(0);
        let x_cap = (stride as i64).min(px_len - yrow).min(clip_x1 as i64);
        let Some((vx0, vx1)) = clip_span(gx, w, x_lo, x_cap) else {
            continue;
        };
        let (i_lo, i_hi) = ((vx0 - gx) as usize, (vx1 - gx) as usize);
        let row_off = j * width * 3;
        let base = yrow + gx;
        for i in i_lo..i_hi {
            let o = row_off + i * 3;
            let cov = [bytes[o], bytes[o + 1], bytes[o + 2]];
            if cov == [0, 0, 0] {
                continue;
            }
            let idx = (base + i as i64) as usize;
            px[idx] = blend_text_subpx(px[idx], color, &pre, cov);
        }
    }
}

/// Fire contrast-halo ink.
pub const HALO_IN_FIRE_RGB: u32 = 0x0014_0803;

/// Per-copy straight source-over alpha of [`HALO_IN_FIRE_RGB`] at full engulfment (`strength == 255`).
pub const HALO_IN_FIRE_ALPHA: u8 = 235;

/// The stamp alpha at zero strength.
pub const HALO_IN_FIRE_ALPHA_FLOOR: u8 = 90;

/// The strength-scaled per-copy stamp alpha for one [`FireHaloCell`].
#[inline]
#[must_use]
pub const fn fire_halo_alpha(strength: u8) -> u8 {
    let floor = HALO_IN_FIRE_ALPHA_FLOOR as u32;
    let span = HALO_IN_FIRE_ALPHA as u32 - floor;
    (floor + strength as u32 * span / 255) as u8
}

/// The device-px offsets a fire contrast halo stamps its glyph at ([`HALO_IN_FIRE_RGB`]).
pub const HALO_DILATE_OFFSETS: [(i32, i32); 14] = [
    (-1, -1),
    (0, -1),
    (1, -1),
    (-1, 0),
    (1, 0),
    (-1, 1),
    (0, 1),
    (1, 1),
    (0, -2),
    (0, 2),
    (-2, 0),
    (2, 0),
    (-3, 0),
    (3, 0),
];

/// The knock-out's dilation ([`Renderer::cut_knock_layers`]): the
/// glyph's coverage stamped at every offset within two pixels (a disc, the
/// corners off). An anti-aliased edge pixel cuts only its own share of the
/// body, so it takes two pixels to cut a whole pixel beyond the ink's last
/// soft pixel.
pub const KNOCK_DILATE_OFFSETS: [(i32, i32); 21] = [
    (0, 0),
    (-1, -1),
    (0, -1),
    (1, -1),
    (-1, 0),
    (1, 0),
    (-1, 1),
    (0, 1),
    (1, 1),
    (0, -2),
    (0, 2),
    (-2, 0),
    (2, 0),
    (-1, -2),
    (1, -2),
    (-1, 2),
    (1, 2),
    (-2, -1),
    (-2, 1),
    (2, -1),
    (2, 1),
];

/// The coverage from which a glyph pixel cuts the whole of each pixel beside it out of a body
/// ([`Renderer::cut_knock_layers`]) on Windows, whose default face (Iosevka Term NL, weight 400)
/// lays stems about a pixel and a third wide: 44% of a pixel, under the least coverage that reads
/// 4.5:1 against its page as [`blend_text`]'s corrected blend lays it (white on black, 45.5%;
/// `#d0d0d0` on `#111318`, 56%; `#2b2d33` on `#f6f7f9`, 66%). Such a stem ends in partly covered
/// pixels with page on three sides, and the soft cut alone (each pixel cutting its own share)
/// left a twentieth of a companion's tan coat beside one at 18 px, which took its 4.51:1 to
/// 4.35:1 against the page. `None` on macOS and Linux, whose defaults (SF Mono, JetBrains Mono)
/// keep the knock-out's laws with the soft cut: every stamp is the coverage itself, so the ring
/// is its coverage's shares alone and their frames are the ones measured there.
pub const KNOCK_INK_LEVEL: Option<u8> = if cfg!(windows) { Some(112) } else { None };

/// How far [`KNOCK_DILATE_OFFSETS`] reach from the glyph, on either axis: a ring meets only the
/// bodies within this many pixels of the glyph's box.
pub const KNOCK_REACH: i32 = {
    let mut reach = 0;
    let mut i = 0;
    while i < KNOCK_DILATE_OFFSETS.len() {
        let (dx, dy) = KNOCK_DILATE_OFFSETS[i];
        reach = max_i32(max_i32(reach, dx.abs()), dy.abs());
        i += 1;
    }
    reach
};

const fn max_i32(a: i32, b: i32) -> i32 {
    if a > b { a } else { b }
}

/// The knock-out's resident state ([`Renderer::cut_knock_layers`]): the frame's layers with
/// the cut bodies, last frame's cuts by key (kept while unchanged) and this frame's, and the
/// mask and texel scratch.
#[derive(Debug, Default)]
struct KnockCut {
    layers: Vec<aterm_core::render::FxLayer>,
    held: Vec<(u64, Arc<aterm_core::render::FxContent>)>,
    next: Vec<(u64, Arc<aterm_core::render::FxContent>)>,
    mask: Vec<u32>,
    texels: Vec<u8>,
    /// The bodies' pixel rows a damage pass reads (`Renderer::mark_overshoot_rows`).
    spans: Vec<(i64, i64)>,
}

/// A cut body's key: its layer, its art's content address and its ink, where it lands and its
/// rings.
fn knock_cut_key(
    layer: &aterm_core::render::FxLayer,
    sp: &aterm_core::render::FxSprite,
    o: (i64, i64),
    area: [i32; 4],
    mask: &[u32],
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = aterm_hash::FxHasher::default();
    layer.id.hash(&mut h);
    sp.art.id().hash(&mut h);
    layer
        .content
        .palette()
        .get(sp.ink as usize)
        .map(|k| (k.color, k.alpha))
        .hash(&mut h);
    (sp.dw, sp.dh, sp.flip_x, o, area).hash(&mut h);
    mask.hash(&mut h);
    h.finish()
}

#[allow(clippy::too_many_arguments)]
#[inline]
fn blit_mono_halo(
    px: &mut [u32],
    stride: usize,
    gx0: i32,
    gy0: i32,
    width: usize,
    height: usize,
    bytes: &[u8],
    color: u32,
    alpha: u8,
    xs: usize,
    ys: usize,
    clip_y0: i32,
    clip_y1: i32,
    clip_x0: i32,
    clip_x1: i32,
) {
    blit_mono_with(
        px,
        stride,
        gx0,
        gy0,
        width,
        height,
        bytes,
        color,
        alpha,
        xs,
        ys,
        (clip_y0, clip_y1),
        (clip_x0, clip_x1),
        blend,
    );
}

/// [`blit_mono_halo`] composited by `mix(destination, color, coverage)`.
#[allow(clippy::too_many_arguments)]
fn blit_mono_with(
    px: &mut [u32],
    stride: usize,
    gx0: i32,
    gy0: i32,
    width: usize,
    height: usize,
    bytes: &[u8],
    color: u32,
    alpha: u8,
    xs: usize,
    ys: usize,
    (clip_y0, clip_y1): (i32, i32),
    (clip_x0, clip_x1): (i32, i32),
    mix: impl Fn(u32, u32, u8) -> u32,
) {
    if alpha == 0 {
        return;
    }
    if xs == 1 && ys == 1 {
        let px_len = px.len() as i64;
        let gx = gx0 as i64;
        let w = width as i64;
        let x_lo = (clip_x0 as i64).max(0);
        for j in 0..height {
            let y = gy0 + j as i32;
            if y < clip_y0 || y >= clip_y1 || y < 0 {
                continue;
            }
            let yrow = y as i64 * stride as i64;
            let x_cap = (stride as i64).min(px_len - yrow).min(clip_x1 as i64);
            let Some((vx0, vx1)) = clip_span(gx, w, x_lo, x_cap) else {
                continue;
            };
            let (i_lo, i_hi) = ((vx0 - gx) as usize, (vx1 - gx) as usize);
            let row_off = j * width;
            let srow = &bytes[row_off + i_lo..row_off + i_hi];
            let drow = &mut px[(yrow + vx0) as usize..(yrow + vx1) as usize];
            if alpha == 255 {
                for (dst, &cov) in drow.iter_mut().zip(srow) {
                    if cov != 0 {
                        *dst = mix(*dst, color, cov);
                    }
                }
            } else {
                for (dst, &cov) in drow.iter_mut().zip(srow) {
                    if cov == 0 {
                        continue;
                    }
                    let a = ((cov as u32 * alpha as u32 + 127) / 255) as u8;
                    if a != 0 {
                        *dst = mix(*dst, color, a);
                    }
                }
            }
        }
        return;
    }
    let px_len = px.len();
    for j in 0..height {
        let srow = &bytes[j * width..j * width + width];
        for (i, &cov) in srow.iter().enumerate() {
            if cov == 0 {
                continue;
            }
            let a = if alpha == 255 {
                cov
            } else {
                ((cov as u32 * alpha as u32 + 127) / 255) as u8
            };
            if a == 0 {
                continue;
            }
            for sy in 0..ys {
                let y = gy0 + (j * ys + sy) as i32;
                for sx in 0..xs {
                    if let Some(idx) = fb_at(
                        gx0 + (i * xs + sx) as i32,
                        y,
                        stride,
                        px_len,
                        clip_y0,
                        clip_y1,
                        clip_x0,
                        clip_x1,
                    ) {
                        px[idx] = mix(px[idx], color, a);
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn blit_rgba_scaled(
    px: &mut [u32],
    stride: usize,
    gx0: i32,
    gy0: i32,
    width: usize,
    height: usize,
    bytes: &[u8],
    xs: usize,
    ys: usize,
    clip_y0: i32,
    clip_y1: i32,
    clip_x0: i32,
    clip_x1: i32,
) {
    let px_len = px.len();
    for j in 0..height {
        let srow = &bytes[j * width * 4..(j * width + width) * 4];
        for (i, texel) in srow.as_chunks::<4>().0.iter().enumerate() {
            let a = texel[3];
            if a == 0 {
                continue;
            }
            let rgb = ((texel[0] as u32) << 16) | ((texel[1] as u32) << 8) | (texel[2] as u32);
            for sy in 0..ys {
                let y = gy0 + (j * ys + sy) as i32;
                for sx in 0..xs {
                    if let Some(idx) = fb_at(
                        gx0 + (i * xs + sx) as i32,
                        y,
                        stride,
                        px_len,
                        clip_y0,
                        clip_y1,
                        clip_x0,
                        clip_x1,
                    ) {
                        px[idx] = if a == 255 {
                            rgb
                        } else {
                            blend(px[idx], rgb, a)
                        };
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn blit_rgba_1x(
    px: &mut [u32],
    stride: usize,
    gx0: i32,
    gy0: i32,
    width: usize,
    height: usize,
    bytes: &[u8],
    clip_y0: i32,
    clip_y1: i32,
    clip_x0: i32,
    clip_x1: i32,
) {
    let px_len = px.len() as i64;
    let gx = gx0 as i64;
    let w = width as i64;
    for j in 0..height {
        let y = gy0 + j as i32;
        if y < clip_y0 || y >= clip_y1 || y < 0 {
            continue;
        }
        let yrow = y as i64 * stride as i64;
        let x_lo = (clip_x0 as i64).max(0);
        let x_cap = (stride as i64).min(px_len - yrow).min(clip_x1 as i64);
        let Some((vx0, vx1)) = clip_span(gx, w, x_lo, x_cap) else {
            continue;
        };
        let (i_lo, i_hi) = ((vx0 - gx) as usize, (vx1 - gx) as usize);
        let srow = &bytes[(j * width + i_lo) * 4..(j * width + i_hi) * 4];
        let drow = &mut px[(yrow + vx0) as usize..(yrow + vx1) as usize];
        for (dst, texel) in drow.iter_mut().zip(srow.as_chunks::<4>().0) {
            let a = texel[3];
            if a == 0 {
                continue;
            }
            let rgb = ((texel[0] as u32) << 16) | ((texel[1] as u32) << 8) | (texel[2] as u32);
            *dst = if a == 255 { rgb } else { blend(*dst, rgb, a) };
        }
    }
}

/// Whether a cursor of `style` is drawn at this blink phase.
#[inline]
#[must_use]
pub const fn cursor_shown(style: CursorStyle, blink_phase: bool) -> bool {
    match style {
        CursorStyle::Hidden => false,
        CursorStyle::BlinkingBlock | CursorStyle::BlinkingUnderline | CursorStyle::BlinkingBar => {
            blink_phase
        }
        _ => true,
    }
}

/// Visit the pixel rects (`[x, y, w, h]`) a cursor of `style` paints in the cursor colour for the cell at `(x0, y0)` of size `cell_w` x `cell_h`.
pub fn for_each_cursor_rect(
    style: CursorStyle,
    x0: usize,
    y0: usize,
    cell_w: usize,
    cell_h: usize,
    mut visit: impl FnMut([usize; 4]),
) {
    match style {
        CursorStyle::BlinkingBlock | CursorStyle::SteadyBlock => {
            visit([x0, y0, cell_w, cell_h]);
        }
        CursorStyle::BlinkingUnderline | CursorStyle::SteadyUnderline => {
            let t = (cell_h / 8).max(2).min(cell_h);
            visit([x0, y0 + cell_h - t, cell_w, t]);
        }
        CursorStyle::BlinkingBar | CursorStyle::SteadyBar => {
            let t = (cell_w / 8).max(2).min(cell_w);
            visit([x0, y0, t, cell_h]);
        }
        CursorStyle::HollowBlock => {
            let t = (cell_h / 16).max(1).min(cell_w.min(cell_h));
            let mid = cell_h.saturating_sub(2 * t);
            visit([x0, y0, cell_w, t]);
            visit([x0, y0 + cell_h - t, cell_w, t]);
            visit([x0, y0 + t, t, mid]);
            visit([x0 + cell_w - t, y0 + t, t, mid]);
        }
        CursorStyle::Bolt => {
            let (wf, hf) = (cell_w as f32, cell_h as f32);
            for y in 0..cell_h {
                let u = (y as f32 + 0.5) / hf;
                let (l, r) = if u < 0.52 {
                    let l = 0.52 - 0.615 * u;
                    (l, l + 0.40)
                } else {
                    let t = (u - 0.52) / 0.48;
                    let l = 0.42 - 0.32 * t;
                    (l, l + 0.40 - 0.34 * t)
                };
                let lx = ((l * wf) as usize).min(cell_w.saturating_sub(1));
                let rx = ((r * wf).ceil() as usize).clamp(lx + 1, cell_w.max(lx + 1));
                visit([x0 + lx, y0 + y, rx - lx, 1]);
            }
        }
        _ => {}
    }
}

/// The columns the block-cursor cut-out repaints (each clipped to the cursor rect).
#[inline]
#[must_use]
pub fn cursor_cutout_cols(plan: &[ColumnGlyph], cc: usize) -> (usize, usize) {
    let ligated = |c: usize| matches!(plan.get(c), Some(ColumnGlyph::Ligated(_)));
    if !ligated(cc) {
        return (cc, cc);
    }
    let mut lo = cc;
    while lo > 0 && ligated(lo - 1) {
        lo -= 1;
    }
    let mut hi = cc;
    while hi.checked_add(1).is_some_and(ligated) {
        hi += 1;
    }
    (lo, hi)
}

/// The pixel rects (`[x, y, w, h]`) of a cell's underline, in the underline colour.
#[allow(clippy::too_many_arguments)]
pub fn underline_rects_into(
    out: &mut Vec<[usize; 4]>,
    style: UnderlineStyle,
    x0: usize,
    y0: usize,
    w: usize,
    cell_w: usize,
    cell_h: usize,
    deco: deco::DecoMetrics,
    curly_mask: bool,
) {
    out.clear();
    if matches!(style, UnderlineStyle::None) || w == 0 || cell_h == 0 {
        return;
    }
    let t = deco.underline_t.clamp(1, cell_h);
    let uy = y0 + deco.underline_y.min(cell_h - t);
    match style {
        UnderlineStyle::None => {}
        UnderlineStyle::Single => out.push([x0, uy, w, t]),
        UnderlineStyle::Double => {
            let gap = (2 * t).max(2);
            let top = uy.saturating_sub(gap).max(y0);
            out.push([x0, top, w, t]);
            out.push([x0, uy, w, t]);
        }
        UnderlineStyle::Curly if curly_mask => {}
        UnderlineStyle::Curly => {
            let amp = t.min(uy - y0);
            pattern_rects_into(out, x0, w, uy, t, |x| !deco::squarewave_up(x, cell_h));
            pattern_rects_into(out, x0, w, uy - amp, t, |x| deco::squarewave_up(x, cell_h));
        }
        UnderlineStyle::Dotted => {
            pattern_rects_into(out, x0, w, uy, t, |x| deco::dotted_on(x, t));
        }
        UnderlineStyle::Dashed => {
            pattern_rects_into(out, x0, w, uy, t, |x| deco::dashed_on(x, cell_w));
        }
    }
}

fn pattern_rects_into(
    out: &mut Vec<[usize; 4]>,
    x0: usize,
    w: usize,
    yy: usize,
    t: usize,
    on: impl Fn(usize) -> bool,
) {
    let mut x = x0;
    let end = x0 + w;
    while x < end {
        if on(x) {
            let s = x;
            while x < end && on(x) {
                x += 1;
            }
            out.push([s, yy, x - s, t]);
        } else {
            x += 1;
        }
    }
}

/// The one pixel rect of a cell's strikethrough band, through the glyph middle, from the OS/2 metrics.
#[inline]
#[must_use]
pub fn strike_rect(
    x0: usize,
    y0: usize,
    w: usize,
    cell_h: usize,
    deco: deco::DecoMetrics,
) -> Option<[usize; 4]> {
    if w == 0 || cell_h == 0 {
        return None;
    }
    let t = deco.strike_t.clamp(1, cell_h);
    let sy = y0 + deco.strike_y.min(cell_h - t);
    Some([x0, sy, w, t])
}

/// The one pixel rect of a cell's overline band.
#[inline]
#[must_use]
pub fn overline_rect(
    x0: usize,
    y0: usize,
    w: usize,
    cell_h: usize,
    deco: deco::DecoMetrics,
) -> Option<[usize; 4]> {
    if w == 0 || cell_h == 0 {
        return None;
    }
    let t = deco.underline_t.clamp(1, cell_h);
    Some([x0, y0, w, t])
}

#[inline]
fn cluster_for(row_clusters: &[(usize, Box<str>)], col: usize) -> Option<&str> {
    row_clusters
        .binary_search_by_key(&col, |(c, _)| *c)
        .ok()
        .map(|i| row_clusters[i].1.as_ref())
}

#[inline]
fn combining_for(row_combining: &[(usize, Box<[char]>)], col: usize) -> Option<&[char]> {
    row_combining
        .binary_search_by_key(&col, |(c, _)| *c)
        .ok()
        .map(|i| row_combining[i].1.as_ref())
}

#[inline]
fn image_covers(row_images: &[(usize, aterm_core::grid::extra::ImageRef)], col: usize) -> bool {
    aterm_core::render::images_in_row_at(row_images, col)
        .iter()
        .any(|(_, image)| image.image.z_index >= 0)
}

#[inline]
fn any_double_height(input: &RenderInput) -> bool {
    let is_dh =
        |ls: &LineSize| matches!(ls, LineSize::DoubleHeightTop | LineSize::DoubleHeightBottom);
    input.line_sizes.iter().any(is_dh)
        || input
            .line_size_spans
            .iter()
            .any(|spans| spans.iter().any(|s| is_dh(&s.line_size)))
}

#[inline]
fn row_spans(input: &RenderInput, r: usize) -> &[LineSizeSpan] {
    input.line_size_spans.get(r).map_or(&[][..], Vec::as_slice)
}

#[inline]
fn default_bg_spans(input: &RenderInput, r: usize) -> &[DefaultBgSpan] {
    input.default_bg_spans.get(r).map_or(&[][..], Vec::as_slice)
}

/// The live default background for viewport cell (`row`, `col`), resolved against the theme.
#[must_use]
#[inline]
pub fn resolved_default_bg_at(input: &RenderInput, row: usize, col: usize, theme_bg: u32) -> u32 {
    let bg = input.default_bg_at(row, col);
    if bg == aterm_core::render::COLOR_UNSET {
        theme_bg
    } else {
        bg
    }
}

/// Kitty graphics z-index boundary for the below-cell-background tier.
pub const KITTY_IMAGE_BELOW_BG_Z_THRESHOLD: i32 = i32::MIN / 2;

/// Whether a Kitty image belongs to the deepest z tier (below non-default cell backgrounds as well as text).
#[must_use]
#[inline]
pub const fn kitty_image_is_below_non_default_bg(z_index: i32) -> bool {
    z_index < KITTY_IMAGE_BELOW_BG_Z_THRESHOLD
}

#[inline]
fn image_below_non_default_bg_covers(
    row_images: &[(usize, aterm_core::grid::extra::ImageRef)],
    col: usize,
) -> bool {
    aterm_core::render::images_in_row_at(row_images, col)
        .iter()
        .any(|(_, image)| kitty_image_is_below_non_default_bg(image.image.z_index))
}

#[derive(Default)]
struct ImageEqMemo {
    verdicts: FxHashMap<(usize, usize), bool>,
}

impl ImageEqMemo {
    #[inline]
    fn image_eq(
        &mut self,
        a: &std::sync::Arc<aterm_core::grid::extra::ImageData>,
        b: &std::sync::Arc<aterm_core::grid::extra::ImageData>,
    ) -> bool {
        if std::sync::Arc::ptr_eq(a, b) {
            return true;
        }
        let key = (
            std::sync::Arc::as_ptr(a) as usize,
            std::sync::Arc::as_ptr(b) as usize,
        );
        if let Some(&verdict) = self.verdicts.get(&key) {
            return verdict;
        }
        let verdict = **a == **b;
        self.verdicts.insert(key, verdict);
        verdict
    }

    #[inline]
    fn rows_eq(
        &mut self,
        a: &[(usize, aterm_core::grid::extra::ImageRef)],
        b: &[(usize, aterm_core::grid::extra::ImageRef)],
    ) -> bool {
        a.len() == b.len()
            && a.iter().zip(b).all(|((ca, ia), (cb, ib))| {
                ca == cb
                    && ia.cell_row == ib.cell_row
                    && ia.cell_col == ib.cell_col
                    && self.image_eq(&ia.image, &ib.image)
            })
    }
}

#[inline]
fn row_differs(a: &RenderInput, b: &RenderInput, r: usize, memo: &mut ImageEqMemo) -> bool {
    row_differs_shifted(a, r, b, r, memo)
}

#[inline]
fn row_cells_differ(a: &RenderInput, ra: usize, b: &RenderInput, rb: usize) -> bool {
    a.cells[ra] != b.cells[rb]
        || a.clusters[ra] != b.clusters[rb]
        || a.combining[ra] != b.combining[rb]
}

#[inline]
fn row_meta_differs(
    a: &RenderInput,
    ra: usize,
    b: &RenderInput,
    rb: usize,
    memo: &mut ImageEqMemo,
) -> bool {
    a.line_sizes[ra] != b.line_sizes[rb]
        || row_spans(a, ra) != row_spans(b, rb)
        || default_bg_spans(a, ra) != default_bg_spans(b, rb)
        || a.visual_to_logical_row(ra) != b.visual_to_logical_row(rb)
        || !memo.rows_eq(&a.images[ra], &b.images[rb])
}

#[inline]
fn row_differs_shifted(
    a: &RenderInput,
    ra: usize,
    b: &RenderInput,
    rb: usize,
    memo: &mut ImageEqMemo,
) -> bool {
    row_cells_differ(a, ra, b, rb) || row_meta_differs(a, ra, b, rb, memo)
}

fn row_glyph_ink_may_differ(
    prev: &RenderInput,
    cur: &RenderInput,
    r: usize,
    stamped: bool,
    memo: &mut ImageEqMemo,
) -> bool {
    let cursor_on = |i: &RenderInput| i.cursor_visible && i.cursor_row == r;
    let halo_on = |i: &RenderInput| !sparse_row_slice(&i.fire_halo, r).is_empty();
    if cursor_on(prev) || cursor_on(cur) || halo_on(prev) || halo_on(cur) {
        return true;
    }
    let cells_differ = match (prev.row_rev.get(r), cur.row_rev.get(r)) {
        (Some(&p), Some(&c)) if stamped && p != 0 && c != 0 => p != c,
        _ => row_cells_differ(cur, r, prev, r),
    };
    cells_differ
        || row_meta_differs(cur, r, prev, r, memo)
        || sparse_row_slice(&prev.ink, r) != sparse_row_slice(&cur.ink, r)
        || sparse_row_slice(&prev.char_fg, r) != sparse_row_slice(&cur.char_fg, r)
        || prev.chrome_raster(r) != cur.chrome_raster(r)
}

/// Whether the two snapshots' per-row revision lanes may be compared to decide the cell half of the row diff (`row_cells_differ`).
#[inline]
#[must_use]
pub fn row_revisions_comparable(prev_input: &RenderInput, input: &RenderInput) -> bool {
    prev_input.rows == input.rows
        && prev_input.cols == input.cols
        && row_stamps_usable(prev_input, input, input.rows)
}

#[inline]
fn row_stamps_usable(prev_input: &RenderInput, input: &RenderInput, rows: usize) -> bool {
    prev_input.row_rev_lane != 0
        && prev_input.row_rev_lane == input.row_rev_lane
        && prev_input.row_rev.len() == rows
        && input.row_rev.len() == rows
        && prev_input.snapshot_seq == prev_input.shifted_fill_seq
        && input.snapshot_seq == input.shifted_fill_seq
        && prev_input.row_shift == input.row_shift
        && prev_input.engine_alt == input.engine_alt
        && prev_input.engine_row_order == aterm_core::render::RowOrder::Logical
        && input.engine_row_order == aterm_core::render::RowOrder::Logical
        && prev_input.display_offset == 0
        && input.display_offset == 0
        && prev_input.base_y == input.base_y
        && prev_input.absolute_row_revision == input.absolute_row_revision
}

/// Update `dst` from `src` like [`RenderInput::clone_from`], but when [`row_revisions_comparable`] holds, skip copying `cells[r]`, `clusters[r]`, and `combining[r]` for rows whose non-zero `row_rev[r]` stamp is unchanged.
#[inline]
pub fn clone_input_stamped(dst: &mut RenderInput, src: &RenderInput) {
    clone_input_stamped_inner(dst, src, false);
}

/// Update `dst` from `src` like [`RenderInput::clone_damage_cache_from`], dropping the four effect-stream payloads when their compact damage metadata is valid, and also skipping `cells[r]`, `clusters[r]` and `combining[r]` copies for rows whose non-zero `row_rev[r]` stamp is unchanged under [`row_revisions_comparable`].
#[inline]
pub fn clone_damage_cache_from_stamped(dst: &mut RenderInput, src: &RenderInput) {
    clone_input_stamped_inner(dst, src, true);
}

fn clone_input_stamped_inner(dst: &mut RenderInput, src: &RenderInput, drop_effect_payloads: bool) {
    let rows = src.rows;
    let can_skip_rows = row_revisions_comparable(dst, src)
        && dst.cells.len() == rows
        && src.cells.len() == rows
        && dst.clusters.len() == rows
        && src.clusters.len() == rows
        && dst.combining.len() == rows
        && src.combining.len() == rows;
    if !can_skip_rows {
        if drop_effect_payloads {
            dst.clone_damage_cache_from(src);
        } else {
            dst.clone_from(src);
        }
        return;
    }
    let RenderInput {
        rows: src_rows,
        cols: src_cols,
        cells,
        clusters,
        combining,
        images,
        line_sizes,
        line_size_spans,
        default_bg_spans,
        visual_to_logical,
        chrome_rasters,
        cursor_row,
        cursor_col,
        cursor_visible,
        cursor_style,
        cursor_effect_style_override,
        cursor_color,
        cursor_fill_override,
        bloom_gate,
        selection,
        selection_clip,
        selections,
        selection_bg,
        selection_fg,
        default_bg,
        wallpaper,
        display_offset,
        base_y,
        scroll_frac_px,
        grid_top_row,
        grid_bot_row,
        cursor_trail,
        cursor_trail_color,
        cursor_glow_add,
        cursor_glow_add_ramps,
        cursor_glow_add_damage,
        glow_halo,
        glow_halo_damage,
        glow_under,
        glow_under_ramps,
        glow_under_damage,
        fire_patch,
        fire_patch_damage,
        fx_clip,
        word_decorations,
        ink,
        char_fg,
        fire_halo,
        nova_add,
        rain_add,
        fx_layers,
        fx_occluders,
        fx_prefetch,
        snapshot_seq,
        shifted_fill_seq,
        row_shift,
        engine_alt,
        grid_instance,
        engine_row_order,
        absolute_row_revision,
        row_rev_lane,
        row_rev,
        history_renumber_epoch,
        holder,
        screen_mutations,
        apron_row,
        apron_stamp,
        default_fg,
        implicit_blank,
        content_seq,
        drawn_at,
        process_sequence,
        input_hot,
        terminal_id,
        extract_gen,
        engine_fill_seq,
        composed_fill_seq,
        chrome_icon_spares: _,
    } = src;

    for r in 0..rows {
        let prev_rev = dst.row_rev[r];
        let cur_rev = row_rev[r];
        if prev_rev == 0
            || cur_rev == 0
            || prev_rev != cur_rev
            || dst.cells[r].len() != cells[r].len()
        {
            dst.cells[r].clone_from(&cells[r]);
            dst.clusters[r].clone_from(&clusters[r]);
            dst.combining[r].clone_from(&combining[r]);
        }
    }
    dst.rows = *src_rows;
    dst.cols = *src_cols;
    dst.images.clone_from(images);
    dst.line_sizes.clone_from(line_sizes);
    dst.line_size_spans.clone_from(line_size_spans);
    dst.default_bg_spans.clone_from(default_bg_spans);
    dst.visual_to_logical.clone_from(visual_to_logical);
    dst.chrome_rasters.clone_from(chrome_rasters);
    dst.cursor_row = *cursor_row;
    dst.cursor_col = *cursor_col;
    dst.cursor_visible = *cursor_visible;
    dst.cursor_style = *cursor_style;
    dst.cursor_effect_style_override = *cursor_effect_style_override;
    dst.cursor_color = *cursor_color;
    dst.cursor_fill_override = *cursor_fill_override;
    dst.bloom_gate = *bloom_gate;
    dst.selection.clone_from(selection);
    dst.selection_clip = *selection_clip;
    dst.selections.clone_from(selections);
    dst.selection_bg = *selection_bg;
    dst.selection_fg = *selection_fg;
    dst.default_bg = *default_bg;
    dst.wallpaper.clone_from(wallpaper);
    dst.display_offset = *display_offset;
    dst.base_y = *base_y;
    dst.scroll_frac_px = *scroll_frac_px;
    dst.grid_top_row = *grid_top_row;
    dst.grid_bot_row = *grid_bot_row;
    dst.cursor_trail.clone_from(cursor_trail);
    dst.cursor_trail_color = *cursor_trail_color;
    if drop_effect_payloads {
        if cursor_glow_add_damage.is_valid() {
            dst.cursor_glow_add.clear();
            dst.cursor_glow_add_ramps.clear();
        } else {
            dst.cursor_glow_add.clone_from(cursor_glow_add);
            dst.cursor_glow_add_ramps.clone_from(cursor_glow_add_ramps);
        }
        if glow_halo_damage.is_valid() {
            dst.glow_halo.clear();
        } else {
            dst.glow_halo.clone_from(glow_halo);
        }
        if glow_under_damage.is_valid() {
            dst.glow_under.clear();
            dst.glow_under_ramps.clear();
        } else {
            dst.glow_under.clone_from(glow_under);
            dst.glow_under_ramps.clone_from(glow_under_ramps);
        }
        if fire_patch_damage.is_valid() {
            dst.fire_patch.clear();
        } else {
            dst.fire_patch.clone_from(fire_patch);
        }
    } else {
        if dst
            .cursor_glow_add_damage
            .same_content(cursor_glow_add_damage)
            != Some(true)
            || dst.cursor_glow_add.len() != cursor_glow_add.len()
            || dst.cursor_glow_add_ramps.items.len() != cursor_glow_add_ramps.items.len()
        {
            dst.cursor_glow_add.clone_from(cursor_glow_add);
            dst.cursor_glow_add_ramps.clone_from(cursor_glow_add_ramps);
        }
        if dst.glow_halo_damage.same_content(glow_halo_damage) != Some(true)
            || dst.glow_halo.len() != glow_halo.len()
        {
            dst.glow_halo.clone_from(glow_halo);
        }
        if dst.glow_under_damage.same_content(glow_under_damage) != Some(true)
            || dst.glow_under.len() != glow_under.len()
            || dst.glow_under_ramps.items.len() != glow_under_ramps.items.len()
        {
            dst.glow_under.clone_from(glow_under);
            dst.glow_under_ramps.clone_from(glow_under_ramps);
        }
        if dst.fire_patch_damage.same_content(fire_patch_damage) != Some(true)
            || dst.fire_patch.len() != fire_patch.len()
        {
            dst.fire_patch.clone_from(fire_patch);
        }
    }
    dst.cursor_glow_add_damage
        .clone_from(cursor_glow_add_damage);
    dst.glow_halo_damage.clone_from(glow_halo_damage);
    dst.glow_under_damage.clone_from(glow_under_damage);
    dst.fire_patch_damage.clone_from(fire_patch_damage);
    dst.fx_clip = *fx_clip;
    dst.word_decorations.clone_from(word_decorations);
    dst.ink.clone_from(ink);
    dst.char_fg.clone_from(char_fg);
    dst.fire_halo.clone_from(fire_halo);
    dst.nova_add.clone_from(nova_add);
    dst.rain_add.clone_from(rain_add);
    dst.fx_layers.clone_from(fx_layers);
    dst.fx_occluders.clone_from(fx_occluders);
    if drop_effect_payloads {
        dst.fx_prefetch.clear();
    } else {
        dst.fx_prefetch.clone_from(fx_prefetch);
    }
    dst.snapshot_seq = *snapshot_seq;
    dst.shifted_fill_seq = *shifted_fill_seq;
    dst.row_shift = *row_shift;
    dst.engine_alt = *engine_alt;
    dst.grid_instance = *grid_instance;
    dst.engine_row_order = *engine_row_order;
    dst.absolute_row_revision = *absolute_row_revision;
    dst.row_rev_lane = *row_rev_lane;
    dst.row_rev.clone_from(row_rev);
    dst.history_renumber_epoch = *history_renumber_epoch;
    dst.holder = *holder;
    dst.screen_mutations = *screen_mutations;
    dst.apron_row.clone_from(apron_row);
    dst.apron_stamp = *apron_stamp;
    dst.default_fg = *default_fg;
    dst.implicit_blank = *implicit_blank;
    dst.content_seq = *content_seq;
    dst.drawn_at = *drawn_at;
    dst.process_sequence = *process_sequence;
    dst.input_hot = *input_hot;
    dst.terminal_id = *terminal_id;
    dst.extract_gen = *extract_gen;
    dst.engine_fill_seq = *engine_fill_seq;
    dst.composed_fill_seq = *composed_fill_seq;
}

#[cfg(clean_islands)]
macro_rules! render_trust_island {
    ($($item:item)*) => {
        $($item)*
    };
}

#[cfg(not(clean_islands))]
macro_rules! render_trust_island {
    ($($tt:tt)*) => {};
}

render_trust_island! {
    clean {
        namespace StampedInputCloneAndFallback

        -- Model of a stamped row (revision stamp + payload value).
        structure StampedRow where
            rev : Nat
            cells : Nat

        def stampedCloneRow (dst src : StampedRow) : StampedRow :=
            if Nat.beq dst.rev 0 || Nat.beq src.rev 0 || !(Nat.beq dst.rev src.rev) then
                src
            else
                dst

        -- When a non-zero stamp matches and the stamp soundness invariant holds
        -- (equal non-zero stamps imply equal cell payloads), skipping the row copy
        -- yields the exact same row as a full clone.
        theorem stamped_clone_same_rev_preserves_cells (payload : Nat) :
            stampedCloneRow { rev := 1, cells := payload } { rev := 1, cells := payload }
            = { rev := 1, cells := payload } := rfl

        -- When a non-zero stamp advances, stampedCloneRow copies src.
        theorem stamped_clone_changed_rev_copies_src (dstCells srcCells : Nat) :
            stampedCloneRow { rev := 1, cells := dstCells } { rev := 2, cells := srcCells }
            = { rev := 2, cells := srcCells } := rfl

        -- When the stamp is zero (unstamped host row), stampedCloneRow copies src.
        theorem stamped_clone_zero_rev_copies_src (dstCells srcCells : Nat) :
            stampedCloneRow { rev := 0, cells := dstCells } { rev := 0, cells := srcCells }
            = { rev := 0, cells := srcCells } := rfl

        -- RuntimeFallback decision entry: 0 represents None, Nat.succ idx represents Some(idx).
        def retainPositiveDecision (decision : Nat) : Nat :=
            if Nat.beq decision 0 then 0 else decision

        theorem retain_positive_preserves_some (idx : Nat) :
            retainPositiveDecision (Nat.succ idx) = Nat.succ idx := rfl

        theorem retain_positive_drops_none :
            retainPositiveDecision 0 = 0 := rfl

        -- When dst equals bg_px (bg masked to 24-bit RGB), blend_text_pre
        -- returns the pre-blended bg_blended slot computed for (fg, bg_px, a),
        -- which is identical to evaluating the full linear-light blend on dst.
        def blendTextFastOrFull (dst bgPx bgBlended fullBlended : Nat) : Nat :=
            if Nat.beq dst bgPx then bgBlended else fullBlended

        theorem alpha_memo_bg_fast_path_sound (blended : Nat) :
            blendTextFastOrFull 1 1 blended blended = blended := rfl

        theorem alpha_memo_non_bg_uses_full_blend (bgBlended fullBlended : Nat) :
            blendTextFastOrFull 2 1 bgBlended fullBlended = fullBlended := rfl

        end StampedInputCloneAndFallback
    }
}

#[inline]
fn mark(dirty: &mut [bool], r: usize) {
    if let Some(slot) = dirty.get_mut(r) {
        *slot = true;
    }
}

#[inline]
fn effect_stream_changed<T: PartialEq>(
    prev: &[T],
    current: &[T],
    prev_damage: &EffectStreamDamage,
    current_damage: &EffectStreamDamage,
) -> bool {
    if let Some(same) = prev_damage.same_content(current_damage) {
        return !same;
    }
    if prev_damage.is_valid() {
        return !(prev_damage.was_empty() == Some(true) && current.is_empty());
    }
    prev != current
}

/// [`effect_stream_changed`] for a glow stream, its ramps beside its quads.
#[inline]
fn glow_stream_changed(
    (prev, prev_ramps): (&[GlowQuad], &GlowRamps),
    (current, current_ramps): (&[GlowQuad], &GlowRamps),
    prev_damage: &EffectStreamDamage,
    current_damage: &EffectStreamDamage,
) -> bool {
    if let Some(same) = prev_damage.same_content(current_damage) {
        return !same;
    }
    if prev_damage.is_valid() {
        return !(prev_damage.was_empty() == Some(true)
            && current.is_empty()
            && current_ramps.is_empty());
    }
    prev != current || prev_ramps != current_ramps
}

/// Every row a glow stream's quads and ramps are tagged with.
#[inline]
fn glow_stream_rows<'a>(
    quads: &'a [GlowQuad],
    ramps: &'a GlowRamps,
) -> impl Iterator<Item = usize> + 'a {
    quads
        .iter()
        .map(|quad| usize::from(quad.row))
        .chain(ramps.items.iter().map(|ramp| usize::from(ramp.row)))
}

#[inline]
fn mark_effect_stream_rows(
    dirty: &mut [bool],
    prev_damage: &EffectStreamDamage,
    current_damage: &EffectStreamDamage,
    fallback_rows: impl Iterator<Item = usize>,
) {
    if prev_damage.is_valid() {
        prev_damage.mark_rows(dirty);
    }
    if current_damage.is_valid() && prev_damage.is_valid() {
        current_damage.mark_rows(dirty);
    } else {
        for row in fallback_rows {
            mark(dirty, row);
        }
    }
}

#[inline]
fn mark_selection_row(dirty: &mut [bool], r: usize) {
    mark(dirty, r);
    if r > 0 {
        mark(dirty, r - 1);
    }
}

/// The verdict of [`compute_dirty_rows`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DirtyDecision {
    /// Not reusable: geometry, scrollback or live default bg changed, or either frame has a double-height row (a DECDHL glyph spans two bands).
    FullRepaint,
    /// Reusable.
    Rows(DirtyRows),
}

/// The reusable-frame flags both the CPU gate and the GPU scissor read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirtyRows {
    /// Whether any row's render-relevant inputs differ (`row_differs`; cursor rows excluded).
    pub any_dirty: bool,
    /// Whether the cursor's shown-ness, position or effective style changed.
    pub cursor_changed: bool,
    /// Whether the blink phase or the cursor-style override changed.
    pub blink_or_override_changed: bool,
    /// Whether the cursor aurora (`cursor_glow_add`) changed.
    pub glow_changed: bool,
    /// Whether the glow halo (`glow_halo`) changed.
    pub glow_halo_changed: bool,
    /// Whether the under-glyph light (`glow_under`) changed.
    pub glow_under_changed: bool,
    /// Whether the fire field (`fire_patch`) changed.
    pub fire_patch_changed: bool,
    /// Whether the comet trail (`cursor_trail` / `cursor_trail_color`) changed.
    pub trail_changed: bool,
    /// Whether the sparkle-word decorations (`word_decorations`) changed.
    pub deco_changed: bool,
    /// Whether the animated-ink overrides (`ink`) changed.
    pub ink_changed: bool,
    /// Whether the charred-ink overrides (`char_fg`) changed.
    pub char_fg_changed: bool,
    /// Whether the fire contrast-halo strengths (`fire_halo`) changed.
    pub fire_halo_changed: bool,
    /// Whether the supernova light (`nova_add`) changed.
    pub nova_changed: bool,
    /// Whether the rain head halos (`rain_add`) changed.
    pub rain_add_changed: bool,
    /// Whether the fx layers (`fx_layers`) differ from the prior frame by identity, or the occluders (`fx_occluders`) differ while either frame draws a layer.
    pub fx_changed: bool,
}

impl DirtyRows {
    /// A verdict carrying only per-row content damage (the flags live in the caller-owned `dirty` scratch).
    #[inline]
    #[must_use]
    pub(crate) const fn rows_only() -> Self {
        Self {
            any_dirty: true,
            cursor_changed: false,
            blink_or_override_changed: false,
            glow_changed: false,
            glow_halo_changed: false,
            glow_under_changed: false,
            fire_patch_changed: false,
            trail_changed: false,
            deco_changed: false,
            ink_changed: false,
            char_fg_changed: false,
            fire_halo_changed: false,
            nova_changed: false,
            rain_add_changed: false,
            fx_changed: false,
        }
    }

    /// True iff the frame is pixel-identical to the prior one.
    #[inline]
    #[must_use]
    pub const fn is_gate_hit(&self) -> bool {
        !self.any_dirty
            && !self.cursor_changed
            && !self.blink_or_override_changed
            && !self.glow_changed
            && !self.glow_halo_changed
            && !self.glow_under_changed
            && !self.fire_patch_changed
            && !self.trail_changed
            && !self.deco_changed
            && !self.ink_changed
            && !self.char_fg_changed
            && !self.fire_halo_changed
            && !self.nova_changed
            && !self.rain_add_changed
            && !self.fx_changed
    }
}

/// Resolve the cursor shape painted this frame.
#[inline]
#[must_use]
pub fn effective_cursor_style(
    input: &RenderInput,
    renderer_cursor_style_override: Option<CursorStyle>,
) -> CursorStyle {
    renderer_cursor_style_override
        .or(input.cursor_effect_style_override)
        .unwrap_or(input.cursor_style)
}

#[inline]
fn cursor_shown_in(
    input: &RenderInput,
    blink_phase: bool,
    cursor_style_override: Option<CursorStyle>,
) -> bool {
    let style = effective_cursor_style(input, cursor_style_override);
    input.cursor_row < input.rows
        && input.cursor_col < input.cols
        && input.cursor_visible
        && cursor_shown(style, blink_phase)
}

/// Visit a [`ChromeRaster`] as one-colour runs `(x0, x1, color, rail)` in frame px over a `w`-px frame whose cells start at `pad` and are `cell_w` wide, without allocating.
pub fn visit_chrome_raster_runs(
    m: &ChromeRaster,
    w: usize,
    pad: usize,
    cell_w: usize,
    cols: usize,
    mut emit: impl FnMut(usize, usize, u32, bool),
) {
    let cell_w = cell_w.max(1);
    let owned = |x: usize| x >= pad && x < pad + cols * cell_w && m.owns((x - pad) / cell_w);
    let mut push = |px: &[u32], rail: bool| {
        let n = px.len().min(w);
        let mut x = 0;
        while x < n {
            let c = px[x];
            if (rail && c == ChromeRaster::KEEP) || owned(x) {
                x += 1;
                continue;
            }
            let start = x;
            while x < n && px[x] == c && !owned(x) {
                x += 1;
            }
            emit(start, x, c, rail);
        }
    };
    push(&m.ground, false);
    if m.rail_h > 0 {
        push(&m.rail, true);
    }
}

/// The outlined capsules of a [`ChromeRaster`] row as one-colour rectangles `(x0, x1, ya, yb, color)`.
#[must_use]
pub fn chrome_ring_runs(
    m: &ChromeRaster,
    w: usize,
    pad: usize,
    cell_w: usize,
    cell_h: usize,
    floor: (usize, usize),
) -> Vec<(usize, usize, usize, usize, u32)> {
    let mut out: Vec<(usize, usize, usize, usize, u32)> = Vec::new();
    if m.rings.is_empty() || cell_w == 0 || cell_h < 4 {
        return out;
    }
    fn enc(l: f32) -> u32 {
        let l = l.clamp(0.0, 1.0);
        let c = if l <= 0.003_130_8 {
            12.92 * l
        } else {
            1.055f32.mul_add(l.powf(1.0 / 2.4), -0.055)
        };
        (c * 255.0).round().clamp(0.0, 255.0) as u32
    }
    let split = |c: u32| {
        [(c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff].map(|v| SRGB_TO_LINEAR_LUT[v as usize])
    };
    let by1 = floor.1.clamp(1, cell_h);
    let (fy0, fy1) = (floor.0.min(by1), by1);
    let t = (fy1 - fy0).clamp(1, (cell_h / 6).max(1)) as f32;
    let by0 = (cell_h - by1).min(cell_h / 8);
    let inset_x = cell_w / 4;
    const SURE: f32 = 0.75;
    const SS: usize = 4;
    let mut prev: Vec<(usize, usize, u32)> = Vec::new();
    let mut runs: Vec<(usize, usize, u32)> = Vec::new();
    for ring in &m.rings {
        let (start, end) = (usize::from(ring.start), usize::from(ring.end));
        if end <= start {
            continue;
        }
        let bx0 = pad + start * cell_w + inset_x;
        let bx1 = (pad + end * cell_w).saturating_sub(inset_x).min(w);
        if bx1 <= bx0 + 2 || by1 <= by0 + 2 {
            continue;
        }
        let cols = chrome_ring_floor_cols(ring);
        let (lx0, lx1) = (pad + cols.start * cell_w, (pad + cols.end * cell_w).min(w));
        let (sx0, sx1) = (pad + start * cell_w, (pad + end * cell_w).min(w));
        let (cx, cy) = ((bx0 + bx1) as f32 * 0.5, (by0 + by1) as f32 * 0.5);
        let (hw, hh) = ((bx1 - bx0) as f32 * 0.5, (by1 - by0) as f32 * 0.5);
        let rad = hh.min(hw);
        let sd = |px: f32, py: f32| {
            let qx = (px - cx).abs() - (hw - rad);
            let qy = (py - cy).abs() - (hh - rad);
            qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - rad
        };
        let (ring_l, inner_l) = (split(ring.ring), split(ring.inner));
        let (ring_c, inner_c) = (ring.ring & 0x00ff_ffff, ring.inner & 0x00ff_ffff);
        prev.clear();
        let mut prev_y = by0;
        for y in by0..by1 {
            runs.clear();
            let on_floor = (fy0..fy1).contains(&y);
            let seam = ring.seam.filter(|_| on_floor).map(|c| c & 0x00ff_ffff);
            let (xa, xb) = if seam.is_some() {
                (sx0, sx1)
            } else {
                (bx0, bx1)
            };
            for x in xa..xb {
                let floored = on_floor && (lx0..lx1).contains(&x);
                let seamed = seam.filter(|_| !floored);
                let d = sd(x as f32 + 0.5, y as f32 + 0.5);
                let color = if d > SURE {
                    match seamed {
                        Some(c) => c,
                        None => continue,
                    }
                } else if d < -t - SURE || (floored && d < -SURE) {
                    inner_c
                } else if d < -SURE && d > -t + SURE {
                    ring_c
                } else {
                    let (mut n_out, mut n_ring, mut n_in) = (0u32, 0u32, 0u32);
                    for sy in 0..SS {
                        for sx in 0..SS {
                            let px = x as f32 + (sx as f32 + 0.5) / SS as f32;
                            let py = y as f32 + (sy as f32 + 0.5) / SS as f32;
                            let d = sd(px, py);
                            if d > 0.0 {
                                n_out += 1;
                            } else if d > -t && !floored {
                                n_ring += 1;
                            } else {
                                n_in += 1;
                            }
                        }
                    }
                    let total = (SS * SS) as u32;
                    if n_out == total {
                        match seamed {
                            Some(c) => c,
                            None => continue,
                        }
                    } else if n_ring == total {
                        ring_c
                    } else if n_in == total {
                        inner_c
                    } else {
                        let under = seamed.unwrap_or_else(|| {
                            m.ground
                                .get(x)
                                .copied()
                                .filter(|_| !m.owns((x.saturating_sub(pad)) / cell_w))
                                .unwrap_or(ring.inner)
                        });
                        let out_l = split(under);
                        let total = total as f32;
                        let ch = |k: usize| {
                            enc((out_l[k] * n_out as f32
                                + ring_l[k] * n_ring as f32
                                + inner_l[k] * n_in as f32)
                                / total)
                        };
                        (ch(0) << 16) | (ch(1) << 8) | ch(2)
                    }
                };
                match runs.last_mut() {
                    Some(r) if r.1 == x && r.2 == color => r.1 = x + 1,
                    _ => runs.push((x, x + 1, color)),
                }
            }
            if runs != prev {
                out.extend(prev.iter().map(|&(a, b, c)| (a, b, prev_y, y, c)));
                std::mem::swap(&mut prev, &mut runs);
                prev_y = y;
            }
        }
        out.extend(prev.iter().map(|&(a, b, c)| (a, b, prev_y, by1, c)));
    }
    out
}

/// The cells of `ring` whose single underline, in the ring's colour, is the capsule's floor.
#[inline]
#[must_use]
pub fn chrome_ring_floor_cols(ring: &ChromeRing) -> std::ops::Range<usize> {
    let (start, end) = (usize::from(ring.start), usize::from(ring.end));
    if end < start + 3 {
        return start..start;
    }
    start + 1..end - 1
}

#[inline]
fn scroll_blittable_content(input: &RenderInput) -> bool {
    fn absent<T>(payload: &[T], damage: &aterm_core::render::EffectStreamDamage) -> bool {
        damage.was_empty().unwrap_or(payload.is_empty())
    }

    input.cursor_trail.is_empty()
        && absent(&input.cursor_glow_add, &input.cursor_glow_add_damage)
        && (input.cursor_glow_add_damage.is_valid() || input.cursor_glow_add_ramps.is_empty())
        && absent(&input.glow_halo, &input.glow_halo_damage)
        && absent(&input.glow_under, &input.glow_under_damage)
        && (input.glow_under_damage.is_valid() || input.glow_under_ramps.is_empty())
        && absent(&input.fire_patch, &input.fire_patch_damage)
        && input.word_decorations.iter().all(|d| d.dy == 0)
        && input.char_fg.is_empty()
        && input.fire_halo.is_empty()
        && input.nova_add.is_empty()
        && input.rain_add.is_empty()
        && !input.any_selection()
}

#[inline]
fn row_overlays_differ_shifted(a: &RenderInput, ra: usize, b: &RenderInput, rb: usize) -> bool {
    let (ia, ib) = (sparse_row_slice(&a.ink, ra), sparse_row_slice(&b.ink, rb));
    if ia.len() != ib.len()
        || ia
            .iter()
            .zip(ib)
            .any(|(x, y)| (x.col, x.color) != (y.col, y.color))
    {
        return true;
    }
    !row_decorations(a, ra).eq(row_decorations(b, rb))
}

#[inline]
fn row_decorations(input: &RenderInput, r: usize) -> impl Iterator<Item = WordDecoration> + '_ {
    input
        .word_decorations
        .iter()
        .filter(move |d| usize::from(d.row) == r)
        .map(|d| WordDecoration { row: 0, ..*d })
}

const OVERSHOOT_APRON_ROWS: usize = 2;

/// The scroll-blit planner.
#[inline]
#[allow(clippy::too_many_arguments)]
pub fn scroll_blit_plan(
    prev: &RenderInput,
    input: &RenderInput,
    prev_blink_phase: bool,
    prev_cursor_style_override: Option<CursorStyle>,
    cur_blink_phase: bool,
    cur_cursor_style_override: Option<CursorStyle>,
    bands: RowBands,
    dirty: &mut Vec<bool>,
) -> Option<i32> {
    scroll_shift_plan(
        prev,
        input,
        prev_blink_phase,
        prev_cursor_style_override,
        cur_blink_phase,
        cur_cursor_style_override,
        bands,
        RetainedRows::Rigid,
        dirty,
    )
}

/// What [`scroll_shift_plan`] does with a retained row whose content did not survive the shift.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetainedRows {
    /// Refuse the whole plan.
    Rigid,
    /// Keep the plan and mark that row dirty, so the damaged tail re-rasterizes it over the shifted pixels.
    Diff,
}

/// The scroll planner proper.
#[allow(clippy::too_many_arguments)]
pub fn scroll_shift_plan(
    prev: &RenderInput,
    input: &RenderInput,
    prev_blink_phase: bool,
    prev_cursor_style_override: Option<CursorStyle>,
    cur_blink_phase: bool,
    cur_cursor_style_override: Option<CursorStyle>,
    bands: RowBands,
    retained: RetainedRows,
    dirty: &mut Vec<bool>,
) -> Option<i32> {
    let cell_h = bands.cell_h;
    let rows = input.rows;
    let da = (input.base_y - i64::from(input.display_offset))
        - (prev.base_y - i64::from(prev.display_offset));
    if da == 0 || da.unsigned_abs() as usize >= rows {
        return None;
    }
    let da = da as i32;
    if prev.rows != rows
        || prev.cols != input.cols
        || prev.default_bg != input.default_bg
        || any_double_height(prev)
        || any_double_height(input)
    {
        return None;
    }
    if prev.wallpaper.is_some() || input.wallpaper.is_some() {
        return None;
    }
    if !scroll_blittable_content(prev) || !scroll_blittable_content(input) {
        return None;
    }
    let prev_shown = cursor_shown_in(prev, prev_blink_phase, prev_cursor_style_override);
    let cur_shown = cursor_shown_in(input, cur_blink_phase, cur_cursor_style_override);
    if prev_shown != cur_shown {
        return None;
    }
    let prev_style = effective_cursor_style(prev, prev_cursor_style_override);
    let cur_style = effective_cursor_style(input, cur_cursor_style_override);
    if cur_shown
        && !(prev.cursor_row == input.cursor_row
            && prev.cursor_col == input.cursor_col
            && prev_style == cur_style
            && prev.cursor_color == input.cursor_color
            && prev.cursor_fill_override == input.cursor_fill_override
            && prev_blink_phase == cur_blink_phase)
    {
        return None;
    }
    let mut image_memo = ImageEqMemo::default();
    if retained == RetainedRows::Rigid {
        for r in 0..rows {
            let src = r as i64 + i64::from(da);
            if (0..rows as i64).contains(&src)
                && (row_differs_shifted(input, r, prev, src as usize, &mut image_memo)
                    || row_overlays_differ_shifted(input, r, prev, src as usize))
            {
                return None;
            }
        }
    }
    dirty.clear();
    dirty.resize(rows, false);
    for (r, row_dirty) in dirty.iter_mut().enumerate() {
        let src = r as i64 + i64::from(da);
        if !(0..rows as i64).contains(&src) {
            // No in-range source: the row is newly exposed.
            *row_dirty = true;
        } else if retained == RetainedRows::Diff
            && (row_differs_shifted(input, r, prev, src as usize, &mut image_memo)
                || row_overlays_differ_shifted(input, r, prev, src as usize))
        {
            *row_dirty = true;
        }
    }
    let raster_rows = input.chrome_rasters.iter().map(|m| i64::from(m.row)).chain(
        prev.chrome_rasters.iter().flat_map(|m| {
            let p = i64::from(m.row);
            [p, p - i64::from(da)]
        }),
    );
    for r in raster_rows {
        if (0..rows as i64).contains(&r) {
            dirty[r as usize] = true;
        }
    }
    for q in 0..rows {
        if !dirty[q] {
            continue;
        }
        for k in 1..=OVERSHOOT_APRON_ROWS as i64 {
            let rr = q as i64 - k;
            if rr >= 0 && (0..rows as i64).contains(&(rr + i64::from(da))) {
                mark(dirty, rr as usize);
            }
        }
    }
    if da < 0 {
        let bottom = rows - 1;
        if (0..rows as i64).contains(&(bottom as i64 + i64::from(da))) {
            mark(dirty, bottom);
        }
    }
    if (i64::from(da) * cell_h as i64) & 1 != 0 {
        for r in 0..rows {
            let src = r as i64 + i64::from(da);
            if (0..rows as i64).contains(&src) && row_has_y_phase_cell(input, r) {
                mark(dirty, r);
            }
        }
    }
    if cur_shown {
        mark(dirty, input.cursor_row);
        let ghost = i64::from(input.cursor_row as i32 - da);
        if (0..rows as i64).contains(&ghost) {
            dirty[ghost as usize] = true;
        }
    }
    fx_damage::mark_fx_scroll(
        dirty,
        bands,
        (&prev.fx_layers, &prev.fx_occluders),
        (&input.fx_layers, &input.fx_occluders),
        da,
    );
    Some(da)
}

#[inline]
const fn cell_is_y_phase_sensitive(ch: char) -> bool {
    matches!(ch as u32, 0x2591..=0x2593)
}

#[inline]
fn row_has_y_phase_cell(input: &RenderInput, r: usize) -> bool {
    input
        .cells
        .get(r)
        .is_some_and(|row| row.iter().any(|c| cell_is_y_phase_sensitive(c.ch)))
}

/// The shared dirty-row computation.
#[allow(clippy::too_many_arguments)]
pub fn compute_dirty_rows(
    prev_input: &RenderInput,
    input: &RenderInput,
    prev_blink_phase: bool,
    prev_cursor_style_override: Option<CursorStyle>,
    cur_blink_phase: bool,
    cur_cursor_style_override: Option<CursorStyle>,
    bands: RowBands,
    dirty: &mut Vec<bool>,
) -> DirtyDecision {
    let (rows, cols) = (input.rows, input.cols);

    let anchor_eq = prev_input.base_y - i64::from(prev_input.display_offset)
        == input.base_y - i64::from(input.display_offset);
    let reusable = prev_input.rows == rows
        && prev_input.cols == cols
        && (prev_input.display_offset == input.display_offset || anchor_eq)
        && prev_input.default_bg == input.default_bg
        && prev_input.wallpaper.as_ref().map(std::sync::Arc::as_ptr)
            == input.wallpaper.as_ref().map(std::sync::Arc::as_ptr)
        && !any_double_height(prev_input)
        && !any_double_height(input);
    if !reusable {
        return DirtyDecision::FullRepaint;
    }

    dirty.clear();
    dirty.resize(rows, false);
    let mut image_memo = ImageEqMemo::default();
    let mut any_dirty = false;
    let stamped = row_stamps_usable(prev_input, input, rows);
    for (r, d) in dirty.iter_mut().enumerate() {
        let differs = if stamped {
            let (prev_rev, cur_rev) = (prev_input.row_rev[r], input.row_rev[r]);
            let cells_differ = if prev_rev != 0 && cur_rev != 0 {
                prev_rev != cur_rev
            } else {
                row_cells_differ(input, r, prev_input, r)
            };
            cells_differ || row_meta_differs(input, r, prev_input, r, &mut image_memo)
        } else {
            row_differs(input, prev_input, r, &mut image_memo)
        };
        if differs {
            *d = true;
            any_dirty = true;
        }
    }
    if prev_input.chrome_rasters != input.chrome_rasters {
        let raster_rows = prev_input
            .chrome_rasters
            .iter()
            .chain(&input.chrome_rasters)
            .map(|m| usize::from(m.row));
        for r in raster_rows {
            if r < rows && prev_input.chrome_raster(r) != input.chrome_raster(r) {
                dirty[r] = true;
                any_dirty = true;
            }
        }
    }
    if prev_input.selection != input.selection
        || prev_input.selection_clip != input.selection_clip
        || prev_input.selections != input.selections
        || prev_input.selection_bg != input.selection_bg
        || prev_input.selection_fg != input.selection_fg
        || prev_input.display_offset != input.display_offset
    {
        for r in 0..rows {
            let materialized = prev_input
                .cells
                .get(r)
                .map_or(0, Vec::len)
                .max(input.cells.get(r).map_or(0, Vec::len))
                .min(input.cols);
            let selected_at = |frame: &RenderInput, col: usize| {
                let cells = frame.cells.get(r).map_or(&[][..], Vec::as_slice);
                frame
                    .selection_hit(
                        r,
                        col,
                        cells.get(col + 1).is_some_and(|cell| cell.wide),
                        cells.get(col).is_some_and(|cell| cell.wide),
                    )
                    .map(|hit| {
                        frame
                            .selections
                            .get(hit)
                            .map_or((frame.selection_bg, frame.selection_fg, false), |entry| {
                                (entry.bg, entry.fg, entry.inactive)
                            })
                    })
            };
            if !prev_input
                .selection_row_keys(r)
                .eq(input.selection_row_keys(r))
                || (0..materialized)
                    .any(|col| selected_at(prev_input, col) != selected_at(input, col))
            {
                mark_selection_row(dirty, r);
                any_dirty = true;
            }
        }
    }
    let prev_style = effective_cursor_style(prev_input, prev_cursor_style_override);
    let prev_shown = prev_input.cursor_row < rows
        && prev_input.cursor_col < cols
        && prev_input.cursor_visible
        && cursor_shown(prev_style, prev_blink_phase);
    let cur_style = effective_cursor_style(input, cur_cursor_style_override);
    let cur_shown = input.cursor_row < rows
        && input.cursor_col < cols
        && input.cursor_visible
        && cursor_shown(cur_style, cur_blink_phase);
    let cursor_changed = prev_shown != cur_shown
        || (cur_shown
            && (prev_input.cursor_row != input.cursor_row
                || prev_input.cursor_col != input.cursor_col
                || prev_style != cur_style
                || prev_input.cursor_color != input.cursor_color
                || prev_input.cursor_fill_override != input.cursor_fill_override));
    if prev_shown {
        mark(dirty, prev_input.cursor_row);
    }
    if cur_shown {
        mark(dirty, input.cursor_row);
    }

    let blink_or_override_changed = prev_blink_phase != cur_blink_phase
        || prev_cursor_style_override != cur_cursor_style_override
        || prev_input.cursor_effect_style_override != input.cursor_effect_style_override;

    let trail_changed = prev_input.cursor_trail != input.cursor_trail
        || (prev_input.cursor_trail_color != input.cursor_trail_color
            && !input.cursor_trail.is_empty());
    if trail_changed {
        for t in &prev_input.cursor_trail {
            mark(dirty, t.row);
        }
        for t in &input.cursor_trail {
            mark(dirty, t.row);
        }
    }

    let glow_changed = glow_stream_changed(
        (
            &prev_input.cursor_glow_add,
            &prev_input.cursor_glow_add_ramps,
        ),
        (&input.cursor_glow_add, &input.cursor_glow_add_ramps),
        &prev_input.cursor_glow_add_damage,
        &input.cursor_glow_add_damage,
    );
    if glow_changed {
        mark_effect_stream_rows(
            dirty,
            &prev_input.cursor_glow_add_damage,
            &input.cursor_glow_add_damage,
            glow_stream_rows(
                &prev_input.cursor_glow_add,
                &prev_input.cursor_glow_add_ramps,
            )
            .chain(glow_stream_rows(
                &input.cursor_glow_add,
                &input.cursor_glow_add_ramps,
            )),
        );
    }

    let glow_halo_changed = effect_stream_changed(
        &prev_input.glow_halo,
        &input.glow_halo,
        &prev_input.glow_halo_damage,
        &input.glow_halo_damage,
    );
    if glow_halo_changed {
        mark_effect_stream_rows(
            dirty,
            &prev_input.glow_halo_damage,
            &input.glow_halo_damage,
            prev_input
                .glow_halo
                .iter()
                .map(|halo| usize::from(halo.row))
                .chain(input.glow_halo.iter().map(|halo| usize::from(halo.row))),
        );
    }

    let glow_under_changed = glow_stream_changed(
        (&prev_input.glow_under, &prev_input.glow_under_ramps),
        (&input.glow_under, &input.glow_under_ramps),
        &prev_input.glow_under_damage,
        &input.glow_under_damage,
    );
    if glow_under_changed {
        mark_effect_stream_rows(
            dirty,
            &prev_input.glow_under_damage,
            &input.glow_under_damage,
            glow_stream_rows(&prev_input.glow_under, &prev_input.glow_under_ramps)
                .chain(glow_stream_rows(&input.glow_under, &input.glow_under_ramps)),
        );
    }

    let fire_patch_changed = effect_stream_changed(
        &prev_input.fire_patch,
        &input.fire_patch,
        &prev_input.fire_patch_damage,
        &input.fire_patch_damage,
    );
    if fire_patch_changed {
        mark_effect_stream_rows(
            dirty,
            &prev_input.fire_patch_damage,
            &input.fire_patch_damage,
            prev_input
                .fire_patch
                .iter()
                .map(|patch| usize::from(patch.row))
                .chain(input.fire_patch.iter().map(|patch| usize::from(patch.row))),
        );
    }

    let deco_changed = prev_input.word_decorations != input.word_decorations;
    if deco_changed {
        for d in prev_input
            .word_decorations
            .iter()
            .chain(&input.word_decorations)
        {
            let r = d.row as usize;
            mark(dirty, r);
            if d.dy < 0 {
                mark(dirty, r.wrapping_sub(1));
            } else if d.dy > 0 {
                mark(dirty, r + 1);
            }
        }
    }

    let ink_changed = prev_input.ink != input.ink;
    if ink_changed {
        for i in prev_input.ink.iter().chain(&input.ink) {
            mark(dirty, i.row as usize);
        }
    }

    let char_fg_changed = prev_input.char_fg != input.char_fg;
    if char_fg_changed {
        for i in prev_input.char_fg.iter().chain(&input.char_fg) {
            mark(dirty, i.row as usize);
        }
    }

    let fire_halo_changed = prev_input.fire_halo != input.fire_halo;
    if fire_halo_changed {
        for i in prev_input.fire_halo.iter().chain(&input.fire_halo) {
            mark(dirty, i.row as usize);
        }
    }

    let nova_changed = prev_input.nova_add != input.nova_add;
    if nova_changed {
        for q in prev_input.nova_add.iter().chain(&input.nova_add) {
            mark(dirty, q.row as usize);
        }
    }

    let rain_add_changed = prev_input.rain_add != input.rain_add;
    if rain_add_changed {
        for q in prev_input.rain_add.iter().chain(&input.rain_add) {
            mark(dirty, q.row as usize);
        }
    }

    let fx_changed = fx_damage::mark_fx_changes(
        dirty,
        bands,
        (&prev_input.fx_layers, &prev_input.fx_occluders),
        (&input.fx_layers, &input.fx_occluders),
    );

    if input.word_decorations.iter().any(|d| d.dy != 0) {
        loop {
            let mut grew = false;
            for d in &input.word_decorations {
                if d.dy == 0 {
                    continue;
                }
                let r = d.row as usize;
                if !dirty.get(r).copied().unwrap_or(false) {
                    continue;
                }
                let n = if d.dy < 0 { r.wrapping_sub(1) } else { r + 1 };
                if n < rows && !dirty[n] {
                    dirty[n] = true;
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }
    }

    DirtyDecision::Rows(DirtyRows {
        any_dirty,
        cursor_changed,
        blink_or_override_changed,
        glow_changed,
        glow_halo_changed,
        glow_under_changed,
        fire_patch_changed,
        trail_changed,
        deco_changed,
        ink_changed,
        char_fg_changed,
        fire_halo_changed,
        nova_changed,
        rain_add_changed,
        fx_changed,
    })
}

/// The full-frame gate-hit predicate.
#[must_use]
pub fn is_unchanged_frame(
    prev_input: &RenderInput,
    prev_blink_phase: bool,
    prev_cursor_style_override: Option<CursorStyle>,
    input: &RenderInput,
    cur_blink_phase: bool,
    cur_cursor_style_override: Option<CursorStyle>,
    bands: RowBands,
) -> bool {
    thread_local! {
        static UNCHANGED_SCRATCH: std::cell::RefCell<Vec<bool>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }
    UNCHANGED_SCRATCH.with_borrow_mut(|dirty| {
        match compute_dirty_rows(
            prev_input,
            input,
            prev_blink_phase,
            prev_cursor_style_override,
            cur_blink_phase,
            cur_cursor_style_override,
            bands,
            dirty,
        ) {
            DirtyDecision::FullRepaint => false,
            DirtyDecision::Rows(d) => d.is_gate_hit(),
        }
    })
}

/// How a glyph is enlarged for a DEC line-size row.
#[derive(Clone, Copy)]
pub struct Scale {
    pub xs: usize,
    pub ys: usize,
    pub clip_y0: i32,
    pub clip_y1: i32,
    pub clip_x0: i32,
    pub clip_x1: i32,
}

impl Scale {
    /// Ordinary single-size row.
    pub const NORMAL: Scale = Scale {
        xs: 1,
        ys: 1,
        clip_y0: i32::MIN,
        clip_y1: i32::MAX,
        clip_x0: i32::MIN,
        clip_x1: i32::MAX,
    };
}

/// The on-screen scale and glyph anchor for a row of `line_size` at top pixel `y0` with cell height `ch`.
#[inline]
#[must_use]
pub const fn row_scale(
    line_size: LineSize,
    y0: usize,
    ch: usize,
    is_last_row: bool,
) -> (Scale, i32) {
    let (y0, ch) = (y0 as i32, ch as i32);
    let band_clip_y1 = if is_last_row { i32::MAX } else { y0 + ch };
    match line_size {
        LineSize::DoubleWidth => (
            Scale {
                xs: 2,
                ys: 1,
                clip_y1: band_clip_y1,
                ..Scale::NORMAL
            },
            y0,
        ),
        LineSize::DoubleHeightTop => (
            Scale {
                xs: 2,
                ys: 2,
                clip_y0: y0,
                clip_y1: y0 + ch,
                ..Scale::NORMAL
            },
            y0,
        ),
        LineSize::DoubleHeightBottom => (
            Scale {
                xs: 2,
                ys: 2,
                clip_y0: y0,
                clip_y1: y0 + ch,
                ..Scale::NORMAL
            },
            y0 - ch,
        ),
        _ => (
            Scale {
                clip_y1: band_clip_y1,
                ..Scale::NORMAL
            },
            y0,
        ),
    }
}

/// Whether row `m`'s pixel raster lays a rail across the window's gutters, edge to edge (the band's closing seam drawn as a rail, `band::compose_band`).
#[inline]
#[must_use]
pub fn chrome_rail_covers_gutters(m: Option<&ChromeRaster>) -> bool {
    m.is_some_and(|m| {
        m.rail_h > 0
            && m.rail.first().is_some_and(|&c| c != ChromeRaster::KEEP)
            && m.rail.last().is_some_and(|&c| c != ChromeRaster::KEEP)
    })
}

/// How a chrome row that keeps its words clear of its rail ([`ChromeRaster::clear_rail`]) fits both into one cell, given the face's `room`.
#[inline]
#[must_use]
pub fn chrome_fit(m: &ChromeRaster, room: (usize, usize)) -> (usize, usize) {
    let rail_h = usize::from(m.rail_h);
    if !m.clear_rail || rail_h == 0 {
        return (0, rail_h);
    }
    let (head, foot) = room;
    let rail = rail_h.min((head + foot).saturating_sub(1)).max(1);
    let lift = (rail + 1).saturating_sub(foot).min(head);
    (lift, rail)
}

/// Row `r`'s glyph placement `placed` (its [`row_scale`]), lifted when the row keeps its words clear of its rail ([`chrome_fit`]).
#[inline]
#[must_use]
pub fn chrome_lifted(
    input: &RenderInput,
    r: usize,
    y0: usize,
    ch: usize,
    room: (usize, usize),
    placed: (Scale, i32),
) -> (Scale, i32) {
    let lift = input
        .chrome_raster(r)
        .map_or(0, |m| chrome_fit(m, room).0.min(ch));
    if lift == 0 {
        return placed;
    }
    let (mut scale, anchor_y) = placed;
    scale.clip_y0 = scale.clip_y0.max(y0 as i32);
    scale.clip_y1 = scale.clip_y1.min((y0 + ch - lift) as i32);
    (scale, anchor_y - lift as i32)
}

/// The on-screen cell advance (px) for a row of `line_size`.
#[inline]
#[must_use]
pub const fn row_cell_w(line_size: LineSize, cell_w: usize) -> usize {
    if matches!(line_size, LineSize::SingleWidth) {
        cell_w
    } else {
        cell_w * 2
    }
}

/// Horizontal placement of composite column `col` in `row`.
#[inline]
#[must_use]
pub fn cell_x_run(
    input: &RenderInput,
    row: usize,
    col: usize,
    cell_w: usize,
) -> (usize, usize, usize, usize) {
    let (line_size, start_col, end_col) = input.line_size_run_at(row, col);
    let w = row_cell_w(line_size, cell_w);
    let clip_x0 = start_col * cell_w;
    let clip_x1 = end_col * cell_w;
    (
        clip_x0 + col.saturating_sub(start_col) * w,
        w,
        clip_x0,
        clip_x1,
    )
}

/// Whether one DEC line size governs every column of `row`, so a column loop can hoist `row_cell_w` and use `col * rcw`.
#[inline]
#[must_use]
pub fn row_is_uniform(input: &RenderInput, row: usize) -> bool {
    input
        .line_size_spans
        .get(row)
        .is_none_or(std::vec::Vec::is_empty)
}

/// Destination rect and atlas uv for the visible part of an atlas glyph under `scale`, for the GPU.
#[inline]
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn glyph_quad(
    cell_left: f32,
    anchor_y: i32,
    baseline: i32,
    scale: Scale,
    ax: u32,
    ay: u32,
    gw: u32,
    gh: u32,
    xmin: i32,
    ymin: i32,
    aw: f32,
    ah: f32,
) -> Option<([f32; 4], [f32; 4])> {
    let (xs, ys) = (scale.xs.max(1), scale.ys.max(1));
    let gx0 = cell_left + (xmin * xs as i32) as f32;
    let gy0 = (anchor_y + ys as i32 * (baseline - gh as i32 - ymin)) as f32;
    let full_h = (gh as usize * ys) as f32;
    let vy0 = gy0.max(scale.clip_y0 as f32);
    let vy1 = (gy0 + full_h).min(scale.clip_y1 as f32);
    if vy1 <= vy0 {
        return None;
    }
    let full_w = (gw as usize * xs) as f32;
    let vx0 = gx0.max(scale.clip_x0 as f32);
    let vx1 = (gx0 + full_w).min(scale.clip_x1 as f32);
    if vx1 <= vx0 {
        return None;
    }
    let vw = vx1 - vx0;
    let u_left = (vx0 - gx0) / xs as f32;
    let vh = vy1 - vy0;
    let v_top = (vy0 - gy0) / ys as f32;
    let rect = [vx0, vy0, vw, vh];
    let uv = [
        (ax as f32 + u_left) / aw,
        (ay as f32 + v_top) / ah,
        (vw / xs as f32) / aw,
        (vh / ys as f32) / ah,
    ];
    Some((rect, uv))
}

/// Horizontal cell origin (the `cell_x`/`cell_left` for [`Renderer::blit`] / [`glyph_quad`])
/// that centres a combining mark's ink in column `c`.
#[inline]
#[must_use]
pub fn mark_cell_x(c: usize, rcw: usize, gw: usize, xmin: i32, scale: Scale) -> i32 {
    mark_cell_x_at((c * rcw) as i32, rcw, gw, xmin, scale)
}

/// [`mark_cell_x`] from the cell's left pixel instead of its column, for a composed row where the column's DEC line-size run places the cell ([`cell_x_run`]).
#[inline]
#[must_use]
pub fn mark_cell_x_at(cell_left: i32, rcw: usize, gw: usize, xmin: i32, scale: Scale) -> i32 {
    let xs = scale.xs.max(1) as i32;
    cell_left + (rcw as i32 - gw as i32 * xs) / 2 - xmin * xs
}

/// The sideways script offset after a mark has been centred in its cell.
#[inline]
#[must_use]
pub fn mark_script_shift(script: aterm_core::terminal::Script, cell_width: usize) -> i32 {
    match script {
        aterm_core::terminal::Script::Normal => 0,
        aterm_core::terminal::Script::Sub { nudge } => (f32::from(nudge) * cell_width as f32
            / f32::from(aterm_core::terminal::Script::NUDGES_PER_CELL))
        .round() as i32,
    }
}

/// Running vertical placement of one cell's combining marks, so a second mark on the same side of the base stacks on the first instead of overprinting it.
#[derive(Clone, Copy, Debug, Default)]
pub struct MarkStack {
    above_top: Option<i32>,
    below_bottom: Option<i32>,
}

impl MarkStack {
    /// Device-pixel y offset for the cell's next mark, whose ink box ([`glyph_ink_box`]) is `ink_h` rows tall with its bottom edge `ink_ymin` rows above the baseline, in a cell `cell_h` rows tall with the baseline `baseline` rows down, under vertical scale `ys`.
    #[inline]
    #[must_use]
    pub fn dy(
        &mut self,
        ink_h: usize,
        ink_ymin: i32,
        baseline: i32,
        cell_h: usize,
        ys: usize,
    ) -> i32 {
        let (ink_h, cell_h, ys) = (ink_h as i32, cell_h as i32, ys.max(1) as i32);
        if ink_ymin >= 0 {
            let want = self.above_top.map_or(0, |top| (top + 1 - ink_ymin).max(0));
            let room = (baseline - ink_h - ink_ymin).max(0);
            let lift = want.min(room);
            let top = ink_ymin + lift + ink_h;
            self.above_top = Some(self.above_top.map_or(top, |t| t.max(top)));
            -lift * ys
        } else if ink_ymin + ink_h <= 0 {
            let want = self
                .below_bottom
                .map_or(0, |bottom| (ink_ymin + ink_h + 1 - bottom).max(0));
            let room = (cell_h - (baseline - ink_ymin)).max(0);
            let drop = want.min(room);
            let bottom = ink_ymin - drop;
            self.below_bottom = Some(self.below_bottom.map_or(bottom, |b| b.min(bottom)));
            drop * ys
        } else {
            0
        }
    }
}

#[cfg(test)]
#[path = "lib_mark_stack_tests.rs"]
mod mark_stack_tests;

fn ligature_break_cols_into(
    input: &RenderInput,
    r: usize,
    shaping: &aterm_types::text_shaping::TextShapingConfig,
    cols: &mut Vec<usize>,
) {
    cols.clear();
    if matches!(
        shaping.ligature_mode,
        aterm_types::text_shaping::LigatureMode::CursorDisabled
    ) && input.cursor_visible
        && input.cursor_row == r
        && input.cursor_col < input.cols
    {
        cols.push(input.cursor_col);
    }
    if input.any_selection() {
        let n = input.cols.min(input.cells.get(r).map_or(0, Vec::len));
        let row_cells = &input.cells[r];
        let selected = |c: usize| {
            let cell = &row_cells[c];
            let is_wide_lead = row_cells.get(c + 1).is_some_and(|next| next.wide);
            input.selection_contains_cell(r, c, is_wide_lead, cell.wide)
        };
        for c in 0..n {
            if selected(c) {
                cols.push(c);
            }
        }
    }
}

fn ligature_run_boundaries_into(input: &RenderInput, r: usize, boundaries: &mut Vec<bool>) {
    boundaries.clear();
    boundaries.resize(input.cols, false);
    for span in row_spans(input, r) {
        if span.start_col > 0 && span.start_col < input.cols {
            boundaries[span.start_col] = true;
        }
    }
}

/// The style bits for a cell.
#[inline]
#[must_use]
pub const fn cell_style(cell: &RenderCell) -> StyleBits {
    let mut bits = 0u8;
    if cell.bold {
        bits |= StyleBits::BOLD.0;
    }
    if cell.italic {
        bits |= StyleBits::ITALIC.0;
    }
    match cell.script {
        aterm_core::terminal::Script::Normal => {}
        aterm_core::terminal::Script::Sub { .. } => bits |= StyleBits::SUB.0,
    }
    StyleBits(bits)
}

/// How large a sub-script glyph ([`StyleBits::SUB`]) is drawn, both ways, relative to its cell's glyph.
pub const SCRIPT_SCALE: f32 = 0.7;

/// The highest a sub-script glyph's ink reaches, as a fraction of the cell height from its top.
pub const SUB_CEILING: f32 = 0.42;

/// Where a sub-script glyph lands, given the raster `(xmin, ymin, width, height)` of a glyph placed the renderers' way (left `cell_x + xmin`, bottom `cell_y + baseline - ymin`).
#[inline]
#[must_use]
pub fn script_drop(
    (xmin, ymin, _, height): (i32, i32, usize, usize),
    advance: f32,
    baseline: i32,
    cell_h: usize,
) -> (f32, f32, f32) {
    let floor = cell_h as f32;
    let base = baseline as f32;
    let top = base - ymin as f32 - height as f32;
    let rise = base - top;
    let ceiling = (floor * SUB_CEILING).ceil();
    let line = floor - (floor - base) * SCRIPT_SCALE;
    let s = if rise > 0.0 {
        SCRIPT_SCALE.min(((line - ceiling) / rise).max(0.0))
    } else {
        SCRIPT_SCALE
    };
    let cx = advance / 2.0;
    let x = cx + (xmin as f32 - cx) * s;
    let y = (line - rise * s).max(ceiling);
    (s, x, y)
}

fn embolden(cov: &[u8], w: usize, h: usize, e: usize) -> (Vec<u8>, usize) {
    if e == 0 || w == 0 || h == 0 {
        return (cov.to_vec(), w);
    }
    let nw = w + e;
    let mut out = vec![0u8; nw * h];
    for y in 0..h {
        let row = &cov[y * w..y * w + w];
        let orow = &mut out[y * nw..y * nw + nw];
        for (x, slot) in orow.iter_mut().enumerate() {
            let mut m = 0u8;
            for k in 0..=e {
                if let Some(&v) = x.checked_sub(k).and_then(|i| row.get(i)) {
                    m = m.max(v);
                }
            }
            *slot = m;
        }
    }
    (out, nw)
}

fn coverage_ink(cov: &[u8]) -> f32 {
    cov.iter().map(|&c| f32::from(c)).sum::<f32>() / 255.0
}

fn evened_ink(inks: &[f32], own: f32, anchor: Option<f32>, spread: f32) -> Option<f32> {
    let (lo, hi) = inks.iter().fold((f32::INFINITY, 0.0f32), |(lo, hi), &i| {
        (lo.min(i), hi.max(i))
    });
    if inks.len() < 2 || lo <= 0.0 || own <= 0.0 || spread <= 1.0 || hi <= lo * spread {
        return None;
    }
    let centre = anchor.map_or((lo * hi).sqrt(), |a| a.clamp(lo, hi));
    let keep = spread.ln() / (hi / lo).ln();
    Some(centre * (own / centre).powf(keep))
}

/// The most ink a frame of a cycle may hold beside a glyph of ink `star` shown between its
/// frames that is not redrawn: `spread` times the star's, a hundredth inside (an outline's
/// offset meets an ink sum only as nearly as its pixels allow), and never under `rest`, the
/// frame the cycle keeps as its face draws it: thinned, the rest's own strokes go soft.
/// `None` where there is no such glyph.
fn cycle_cap(star: f32, rest: Option<f32>, spread: f32) -> Option<f32> {
    (star > 0.0 && spread > 1.0).then(|| (0.99 * spread * star).max(rest.unwrap_or(0.0)))
}

struct OutlineField {
    w: usize,
    h: usize,
    dist: Vec<f32>,
}

impl OutlineField {
    fn new(cov: &[u8], w: usize, h: usize, reach: usize) -> OutlineField {
        let r = isize::try_from(reach).unwrap_or(0);
        let near: Vec<(isize, isize, f32)> = (-r..=r)
            .flat_map(|dy| (-r..=r).map(move |dx| (dx, dy)))
            .filter(|&d| d != (0, 0))
            .map(|(dx, dy)| (dx, dy, (dx as f32).hypot(dy as f32)))
            .collect();
        let at = |x: isize, y: isize| -> f32 {
            if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
                return 0.0;
            }
            f32::from(cov[y as usize * w + x as usize]) / 255.0
        };
        let far = reach as f32 + 0.5;
        let mut dist = Vec::with_capacity(w * h);
        for y in 0..h as isize {
            for x in 0..w as isize {
                let c = at(x, y);
                let d = if c > 0.0 && c < 1.0 {
                    c - 0.5
                } else if c >= 1.0 {
                    near.iter()
                        .map(|&(dx, dy, len)| (at(x + dx, y + dy), len))
                        .filter(|&(q, _)| q < 1.0)
                        .map(|(q, len)| len + q - 0.5)
                        .fold(far, f32::min)
                } else {
                    -near
                        .iter()
                        .map(|&(dx, dy, len)| (at(x + dx, y + dy), len))
                        .filter(|&(q, _)| q > 0.0)
                        .map(|(q, len)| len - (q - 0.5))
                        .fold(far, f32::min)
                };
                dist.push(d);
            }
        }
        OutlineField { w, h, dist }
    }

    fn coverage(&self, offset: f32) -> Vec<u8> {
        self.dist
            .iter()
            .map(|d| ((0.5 + d + offset).clamp(0.0, 1.0) * 255.0).round() as u8)
            .collect()
    }

    fn ink(&self, offset: f32) -> f32 {
        self.dist
            .iter()
            .map(|d| (0.5 + d + offset).clamp(0.0, 1.0))
            .sum()
    }

    fn offset_for_ink(&self, target: f32, bound: f32) -> f32 {
        let (mut lo, mut hi) = (-bound, bound);
        if self.ink(hi) <= target {
            return hi;
        }
        if self.ink(lo) >= target {
            return lo;
        }
        for _ in 0..24 {
            let mid = (lo + hi) / 2.0;
            if self.ink(mid) < target {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        (lo + hi) / 2.0
    }
}

#[cfg(test)]
#[path = "lib_outline_field_tests.rs"]
mod outline_field_tests;

fn slant(cov: &[u8], w: usize, h: usize, shear: f32) -> (Vec<u8>, usize) {
    if w == 0 || h == 0 {
        return (cov.to_vec(), w);
    }
    let max_off = (((h - 1) as f32) * shear).floor() as usize;
    let nw = w + max_off + 1;
    let mut out = vec![0u8; nw * h];
    for y in 0..h {
        let shift = ((h - 1 - y) as f32) * shear;
        let i = shift.floor() as usize;
        let f = shift - i as f32;
        let (row_in, row_out) = (&cov[y * w..y * w + w], &mut out[y * nw..(y + 1) * nw]);
        for (x, &c) in row_in.iter().enumerate() {
            if c == 0 {
                continue;
            }
            let cf = f32::from(c);
            row_out[x + i] = row_out[x + i].saturating_add((cf * (1.0 - f)).round() as u8);
            if f > 0.0 {
                row_out[x + i + 1] = row_out[x + i + 1].saturating_add((cf * f).round() as u8);
            }
        }
    }
    (out, nw)
}

fn synthetic_bold_px(px: f32) -> usize {
    (px / 18.0).round().max(1.0) as usize
}

fn apply_synthetic_style(
    style: StyleBits,
    w: usize,
    h: usize,
    bytes: Vec<u8>,
    px: f32,
) -> (usize, Vec<u8>) {
    let (mut w, mut bytes) = (w, bytes);
    if style.contains(StyleBits::BOLD) {
        let e = synthetic_bold_px(px);
        let (b, nw) = embolden(&bytes, w, h, e);
        bytes = b;
        w = nw;
    }
    if style.contains(StyleBits::ITALIC) {
        let (b, nw) = slant(&bytes, w, h, SYNTHETIC_ITALIC_SHEAR);
        bytes = b;
        w = nw;
    }
    (w, bytes)
}

/// sRGB EOTF — gamma-encoded channel (0..1) → linear-light (0..1), the proper piecewise curve (not a bare `pow(2.2)`, which is visibly wrong near black where terminal text edges live).
#[inline]
#[must_use]
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Inverse sRGB OETF.
#[inline]
#[must_use]
pub fn linear_to_srgb(l: f32) -> f32 {
    if l <= 0.003_130_8 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

static SRGB_TO_LINEAR_LUT: std::sync::LazyLock<[f32; 256]> =
    std::sync::LazyLock::new(|| std::array::from_fn(|i| srgb_to_linear(i as f32 / 255.0)));

/// [`srgb_to_linear`] of the 8-bit channel `channel`, read from a table of its 256 values.
#[must_use]
#[inline]
pub fn srgb8_to_linear(channel: u8) -> f32 {
    SRGB_TO_LINEAR_LUT[usize::from(channel)]
}

const LIN2SRGB_N: usize = 4096;
static LINEAR_TO_SRGB_LUT: std::sync::LazyLock<[u8; LIN2SRGB_N + 1]> =
    std::sync::LazyLock::new(|| {
        std::array::from_fn(|i| {
            (linear_to_srgb(i as f32 / LIN2SRGB_N as f32) * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8
        })
    });

#[inline]
fn blend_channel_linear(bg: u32, fg: u32, a: f32) -> u32 {
    let bl = SRGB_TO_LINEAR_LUT[bg as usize];
    let fl = SRGB_TO_LINEAR_LUT[fg as usize];
    let l = fl * a + bl * (1.0 - a);
    let idx = (l * LIN2SRGB_N as f32)
        .round()
        .clamp(0.0, LIN2SRGB_N as f32) as usize;
    LINEAR_TO_SRGB_LUT[idx] as u32
}

/// How glyph coverage is composited over the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextBlending {
    /// Pure linear-light alpha-over.
    Linear,
    /// Linear-light blending + perceptual coverage remap (the default).
    #[default]
    LinearCorrected,
}

/// Luminance gap below which the corrected-alpha remap is skipped.
pub const TEXT_BLEND_EPS: f32 = 0.001;

/// Ghostty's linear-corrected alpha remap.
///
/// ```text
/// blend_l = linearize(unlinearize(fg_l)·a + unlinearize(bg_l)·(1-a))
/// a_corr  = clamp((blend_l - bg_l) / (fg_l - bg_l), 0, 1)
/// ```
#[inline]
#[must_use]
pub fn correct_alpha(fg_l: f32, bg_l: f32, a: f32) -> f32 {
    if (fg_l - bg_l).abs() < TEXT_BLEND_EPS {
        return a;
    }
    let blend_l = srgb_to_linear(linear_to_srgb(fg_l) * a + linear_to_srgb(bg_l) * (1.0 - a));
    ((blend_l - bg_l) / (fg_l - bg_l)).clamp(0.0, 1.0)
}

/// Relative luminance (BT.709, the WCAG luminance the contrast floors compare) of a packed `0x00RRGGBB` sRGB pixel, via the per-byte LUT.
#[inline]
#[must_use]
pub fn linear_luminance(rgb: u32) -> f32 {
    let l = |sh: u32| SRGB_TO_LINEAR_LUT[((rgb >> sh) & 0xff) as usize];
    0.2126 * l(16) + 0.7152 * l(8) + 0.0722 * l(0)
}

/// The text coverage blend.
#[inline]
#[must_use]
pub fn blend_text(dst: u32, fg: u32, bg: u32, t: u8, corrected: bool) -> u32 {
    TextLevel::new(fg, bg, t, corrected).over(dst)
}

/// One coverage level `t` of `fg`'s text on a `bg` cell, as [`blend_text`] lays it over any
/// ground: its corrected alpha (three sRGB transfers, two of them `powf`) is worked out once,
/// so a reader that lays one glyph level over many grounds (the glass yield's glyph judge)
/// blends each exactly as [`blend_text`] does, which is this.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextLevel {
    fg: u32,
    t: u8,
    corrected: bool,
    /// The corrected alpha, read only where `corrected` and `t` is neither `0` nor `255`.
    a: f32,
}

impl TextLevel {
    /// Level `t` of `fg` on `bg`, gamma-corrected or not.
    #[inline]
    #[must_use]
    pub fn new(fg: u32, bg: u32, t: u8, corrected: bool) -> Self {
        let a = if corrected && t != 0 && t != 255 {
            correct_alpha(
                linear_luminance(fg),
                linear_luminance(bg),
                f32::from(t) / 255.0,
            )
        } else {
            0.0
        };
        Self {
            fg,
            t,
            corrected,
            a,
        }
    }

    /// The level laid over `dst`: [`blend_text`]`(dst, fg, bg, t, corrected)`.
    #[inline]
    #[must_use]
    pub fn over(&self, dst: u32) -> u32 {
        let (fg, a) = (self.fg, self.a);
        if self.t == 0 {
            return dst & 0x00ff_ffff;
        }
        if self.t == 255 {
            return fg & 0x00ff_ffff;
        }
        if !self.corrected {
            return blend(dst, fg, self.t);
        }
        let ch = |sh: u32| blend_channel_linear((dst >> sh) & 0xff, (fg >> sh) & 0xff, a);
        let tt = (((dst >> 24) & 0xff) as f32 * (1.0 - a)).round() as u32;
        (tt << 24) | (ch(16) << 16) | (ch(8) << 8) | ch(0)
    }

    /// [`Self::over`]'s red, green and blue as tables of the ground's own channel: each channel
    /// blends apart from the others, so `over(dst)` lays `tables[0][red]`, `tables[1][green]`
    /// and `tables[2][blue]` of `dst`'s.
    #[must_use]
    pub fn channel_tables(&self) -> [[u8; 256]; 3] {
        let mut out = [[0; 256]; 3];
        for (table, sh) in out.iter_mut().zip([16_u32, 8, 0]) {
            for (v, slot) in (0_u32..).zip(table.iter_mut()) {
                *slot = ((self.over(v << sh) >> sh) & 0xff) as u8;
            }
        }
        out
    }
}

#[derive(Clone, Copy)]
struct TextBlendPre {
    fg_l: f32,
    bg_l: f32,
    fg_s: f32,
    bg_s: f32,
    degenerate: bool,
}

impl TextBlendPre {
    #[inline]
    fn new(fg: u32, bg: u32) -> Self {
        let fg_l = linear_luminance(fg);
        let bg_l = linear_luminance(bg);
        Self {
            fg_l,
            bg_l,
            fg_s: linear_to_srgb(fg_l),
            bg_s: linear_to_srgb(bg_l),
            degenerate: (fg_l - bg_l).abs() < TEXT_BLEND_EPS,
        }
    }
}

#[inline]
fn correct_alpha_pre(pre: &TextBlendPre, a: f32) -> f32 {
    if pre.degenerate {
        return a;
    }
    let blend_l = srgb_to_linear(pre.fg_s * a + pre.bg_s * (1.0 - a));
    ((blend_l - pre.bg_l) / (pre.fg_l - pre.bg_l)).clamp(0.0, 1.0)
}

struct AlphaMemo {
    key: Option<(u32, u32)>,
    fg_px: u32,
    bg_px: u32,
    pre: TextBlendPre,
    epoch: u32,
    slots: [u64; 256],
    bg_blended: [u32; 256],
}

impl AlphaMemo {
    #[inline]
    fn new() -> Self {
        Self {
            key: None,
            fg_px: 0,
            bg_px: 0,
            pre: TextBlendPre::new(0, 0),
            epoch: 1,
            slots: [0; 256],
            bg_blended: [0; 256],
        }
    }

    #[inline]
    fn rekey(&mut self, fg: u32, bg: u32) {
        self.key = Some((fg, bg));
        self.fg_px = fg & 0x00ff_ffff;
        self.bg_px = bg & 0x00ff_ffff;
        self.pre = TextBlendPre::new(fg, bg);
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.slots = [0; 256];
            self.epoch = 1;
        }
    }

    #[inline]
    fn alpha(&mut self, t: u8) -> f32 {
        let slot = self.slots[t as usize];
        if (slot >> 32) as u32 == self.epoch {
            f32::from_bits(slot as u32)
        } else {
            self.fill(t)
        }
    }

    #[inline]
    fn blend_bg(&mut self, t: u8) -> u32 {
        let idx = t as usize;
        if (self.slots[idx] >> 32) as u32 != self.epoch {
            let _ = self.fill(t);
        }
        self.bg_blended[idx]
    }

    #[cold]
    #[inline(never)]
    fn fill(&mut self, t: u8) -> f32 {
        let a = correct_alpha_pre(&self.pre, f32::from(t) / 255.0);
        let fg = self.fg_px;
        let bg = self.bg_px;
        let ch = |sh: u32| blend_channel_linear((bg >> sh) & 0xff, (fg >> sh) & 0xff, a);
        let idx = t as usize;
        self.bg_blended[idx] = (ch(16) << 16) | (ch(8) << 8) | ch(0);
        self.slots[idx] = (u64::from(self.epoch) << 32) | u64::from(a.to_bits());
        a
    }
}

const ALPHA_MEMO_WAYS: usize = 8;

struct AlphaMemoBank {
    ways: [AlphaMemo; ALPHA_MEMO_WAYS],
    next: usize,
}

impl Default for AlphaMemoBank {
    fn default() -> Self {
        Self {
            ways: std::array::from_fn(|_| AlphaMemo::new()),
            next: 0,
        }
    }
}

impl AlphaMemoBank {
    #[inline]
    fn way_for(&mut self, fg: u32, bg: u32) -> &mut AlphaMemo {
        let want = Some((fg, bg));
        if let Some(i) = self.ways.iter().position(|way| way.key == want) {
            return &mut self.ways[i];
        }
        let i = self.next;
        self.next = (self.next + 1) % ALPHA_MEMO_WAYS;
        self.ways[i].rekey(fg, bg);
        &mut self.ways[i]
    }
}

#[inline]
fn blend_text_pre(dst: u32, fg: u32, t: u8, memo: &mut AlphaMemo) -> u32 {
    debug_assert_eq!(memo.key.map(|(f, _)| f), Some(fg), "memo keyed on other fg");
    if dst == memo.bg_px {
        return memo.blend_bg(t);
    }
    let a = memo.alpha(t);
    let ch = |sh: u32| blend_channel_linear((dst >> sh) & 0xff, (fg >> sh) & 0xff, a);
    let tt = (((dst >> 24) & 0xff) as f32 * (1.0 - a)).round() as u32;
    (tt << 24) | (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

pub const STEM_GAMMA_MIN: f32 = 0.30;
pub const STEM_GAMMA_MAX: f32 = 3.0;

/// Shared stem-gamma clamp policy.
#[inline]
#[must_use]
pub const fn clamp_stem_gamma(gamma: f32) -> f32 {
    if gamma.is_finite() {
        gamma.clamp(STEM_GAMMA_MIN, STEM_GAMMA_MAX)
    } else {
        1.0
    }
}

const DEFAULT_STEM_GAMMA: f32 = 1.0;

fn build_stem_lut(gamma: f32) -> [u8; 256] {
    let mut t = [0u8; 256];
    for (c, slot) in t.iter_mut().enumerate() {
        let v = (c as f32 / 255.0).powf(gamma) * 255.0;
        *slot = v.round().clamp(0.0, 255.0) as u8;
    }
    t
}

#[inline]
fn stem_darken(cov: &mut [u8], lut: &[u8; 256]) {
    for c in cov.iter_mut() {
        *c = lut[*c as usize];
    }
}

#[inline]
fn empty_rgba() -> GlyphImage {
    GlyphImage::Rgba {
        width: 0,
        height: 0,
        xmin: 0,
        ymin: 0,
        advance: 0.0,
        bytes: Vec::new(),
    }
}

/// Pack an `[r, g, b]` triple into the framebuffer's `0x00RRGGBB` format.
#[inline]
#[must_use]
pub const fn rgb_to_u32([r, g, b]: [u8; 3]) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

/// Theme polarity.
#[inline]
#[must_use]
pub fn theme_is_dark(bg: u32) -> bool {
    let [_, r, g, b] = bg.to_be_bytes();
    !aterm_messages::ink::bg_is_light([r, g, b])
}

#[cfg(test)]
#[path = "lib_theme_polarity_tests.rs"]
mod theme_polarity_tests;

#[inline]
fn contrast_ratio(a: u32, b: u32) -> f32 {
    let (la, lb) = (linear_luminance(a), linear_luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// The two colours one selection band paints with, after the renderer's policy has resolved the terminal's live OSC state against the theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionColors {
    /// The band fill (`0x00RRGGBB`).
    pub bg: u32,
    /// The selected-text ink, or `None` for the automatic WCAG contrast floor.
    pub fg: Option<u32>,
}

#[derive(Clone, Copy, Debug)]
struct SelectionPolicy {
    unset_active: u32,
    unset_inactive: u32,
    inactive: bool,
    inactive_bg: Option<u32>,
    host_fg: Option<u32>,
    frame_bg: u32,
}

/// Per-frame selection colour resolver.
#[derive(Clone, Copy)]
pub struct SelectionPalette<'a> {
    policy: SelectionPolicy,
    scalar: SelectionColors,
    entries: &'a [aterm_core::render::PaneSelection],
}

impl SelectionPalette<'_> {
    /// The colours for the selection index [`RenderInput::selection_hit`] returned.
    #[must_use]
    #[inline]
    pub fn at(&self, index: usize) -> SelectionColors {
        match self.entries.get(index) {
            Some(entry) => {
                resolve_selection_colors(&self.policy, entry.bg, entry.fg, entry.inactive)
            }
            None => self.scalar,
        }
    }

    /// The scalar path's colours.
    #[must_use]
    #[inline]
    #[cfg(test)]
    pub fn scalar(&self) -> SelectionColors {
        self.scalar
    }

    /// [`Self::at`] for a `selection_hit` result.
    #[must_use]
    #[inline]
    pub fn at_hit(&self, hit: Option<usize>) -> SelectionColors {
        match hit {
            Some(index) => self.at(index),
            None => self.scalar,
        }
    }
}

#[must_use]
#[inline]
fn resolve_selection_colors(
    policy: &SelectionPolicy,
    live_bg: u32,
    live_fg: u32,
    entry_inactive: bool,
) -> SelectionColors {
    let inactive = policy.inactive || entry_inactive;
    let bg = if live_bg == aterm_core::render::COLOR_UNSET {
        if inactive {
            policy.unset_inactive
        } else {
            policy.unset_active
        }
    } else {
        let active = live_bg & 0x00ff_ffff;
        if inactive {
            policy
                .inactive_bg
                .unwrap_or_else(|| derive_inactive_selection_bg(active, policy.frame_bg))
        } else {
            active
        }
    };
    let fg = match live_fg {
        aterm_core::render::COLOR_DYNAMIC => None,
        aterm_core::render::COLOR_UNSET => policy.host_fg,
        color => Some(color & 0x00ff_ffff),
    };
    SelectionColors { bg, fg }
}

/// Floor a glyph's fg contrast against the selection background to 4.5:1 (xterm's minimumContrastRatio applied to the selection band), so selected text stays legible.
#[inline]
#[must_use]
pub fn floor_selection_fg(fg: u32, selection_bg: u32) -> u32 {
    floor_fg_contrast(fg, selection_bg, 4.5)
}

#[inline]
fn contrast_target(bg: u32) -> u32 {
    let l = linear_luminance(bg);
    if (l + 0.05) * (l + 0.05) >= 0.05 * 1.05 {
        0x0000_0000
    } else {
        0x00ff_ffff
    }
}

/// Whether [`floor_fg_contrast`] would change `fg`, including unattainable or NaN floors.
#[inline]
#[must_use]
pub fn fg_contrast_would_change(fg: u32, bg: u32, min: f32) -> bool {
    if contrast_ratio(fg, bg) >= min {
        return false;
    }
    // An accepted candidate cannot equal the rejected input. If none is accepted,
    // the floor returns this target, which may already be fg for an impossible floor.
    contrast_target(bg) != fg
}

/// Raise `fg`'s WCAG contrast against `bg` toward `min`.
/// If no tested mix meets the floor, return the higher-contrast black or white target.
#[inline]
#[must_use]
pub fn floor_fg_contrast(fg: u32, bg: u32, min: f32) -> u32 {
    if contrast_ratio(fg, bg) >= min {
        return fg;
    }
    let target = contrast_target(bg);
    let mix = |fg: u32, shift: u32, t: f32| -> u32 {
        let f = ((fg >> shift) & 0xff) as f32;
        let g = ((target >> shift) & 0xff) as f32;
        ((f + (g - f) * t).round() as u32) & 0xff
    };
    let mut step = 1u32;
    while step <= 10 {
        let t = step as f32 / 10.0;
        let cand = (mix(fg, 16, t) << 16) | (mix(fg, 8, t) << 8) | mix(fg, 0, t);
        if contrast_ratio(cand, bg) >= min {
            return cand;
        }
        step += 1;
    }
    target
}

/// The per-cell minimum-contrast floor (xterm's `minimumContrastRatio`).
#[inline]
#[must_use]
pub fn floor_min_contrast_fg(fg: u32, bg: u32, min_contrast: f32) -> u32 {
    if min_contrast <= 1.0 || fg == bg {
        return fg;
    }
    floor_fg_contrast(fg, bg, min_contrast)
}

/// The effective ink a glyph paints with.
#[inline]
#[must_use]
pub fn effective_glyph_fg(
    selection_fg: Option<u32>,
    min_contrast: f32,
    fg: u32,
    bg: u32,
    selected: bool,
    sel_bg: u32,
) -> u32 {
    if selected {
        selection_fg.unwrap_or_else(|| floor_selection_fg(fg, sel_bg))
    } else {
        floor_min_contrast_fg(fg, bg, min_contrast)
    }
}

/// The three inks a cell's line decorations paint with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecoInks {
    /// Underline / undercurl ink.
    pub underline: u32,
    /// Strikethrough ink: always the effective glyph fg.
    pub strike: u32,
    /// Overline ink.
    pub overline: u32,
}

/// Resolve all three decoration inks for one cell, in the one place both backends call, so a new channel or floor lands on both at once.
#[inline]
#[must_use]
pub fn deco_inks(
    cell: &RenderCell,
    selection_fg: Option<u32>,
    min_contrast: f32,
    base_fg: u32,
    selected: bool,
    sel_bg: u32,
) -> DecoInks {
    let bg = rgb_to_u32(cell.bg);
    let glyph = effective_glyph_fg(selection_fg, min_contrast, base_fg, bg, selected, sel_bg);
    let explicit = |c: Option<[u8; 3]>| match c.map(rgb_to_u32) {
        Some(c) if selected => floor_selection_fg(c, sel_bg),
        Some(c) => floor_min_contrast_fg(c, bg, min_contrast),
        None => glyph,
    };
    DecoInks {
        underline: explicit(cell.underline_color),
        strike: glyph,
        overline: explicit(cell.overline_color),
    }
}

/// Floor the cursor fill against the bg of the cell it sits on when the per-cell minimum-contrast knob is on (`min_contrast > 1`), so an OSC 12 or themed cursor never vanishes into the background.
#[inline]
#[must_use]
pub fn floor_cursor_fill(cursor: u32, cell_bg: u32, min_contrast: f32) -> u32 {
    if min_contrast <= 1.0 {
        return cursor;
    }
    floor_fg_contrast(cursor, cell_bg, min_contrast)
}

mod sparse {
    pub trait SparseCell {
        type V: Copy;
        const NAME: &'static str;
        fn row(&self) -> u16;
        fn col(&self) -> u16;
        fn value(&self) -> Self::V;
    }
}
use sparse::SparseCell;

impl SparseCell for InkCell {
    type V = [u8; 3];
    const NAME: &'static str = "ink";
    #[inline]
    fn row(&self) -> u16 {
        self.row
    }
    #[inline]
    fn col(&self) -> u16 {
        self.col
    }
    #[inline]
    fn value(&self) -> [u8; 3] {
        self.color
    }
}

impl SparseCell for CharFg {
    type V = u32;
    const NAME: &'static str = "char_fg";
    #[inline]
    fn row(&self) -> u16 {
        self.row
    }
    #[inline]
    fn col(&self) -> u16 {
        self.col
    }
    #[inline]
    fn value(&self) -> u32 {
        self.fg
    }
}

impl SparseCell for FireHaloCell {
    type V = u8;
    const NAME: &'static str = "fire_halo";
    #[inline]
    fn row(&self) -> u16 {
        self.row
    }
    #[inline]
    fn col(&self) -> u16 {
        self.col
    }
    #[inline]
    fn value(&self) -> u8 {
        self.strength
    }
}

/// One viewport row's run of a sparse override stream ([`InkCell`], [`CharFg`], [`FireHaloCell`]).
#[inline]
#[must_use]
pub fn sparse_row_slice<T: SparseCell>(v: &[T], row: usize) -> &[T] {
    let Ok(row) = u16::try_from(row) else {
        return &[];
    };
    let lo = v.partition_point(|c| c.row() < row);
    let hi = lo + v[lo..].partition_point(|c| c.row() <= row);
    let s = &v[lo..hi];
    debug_assert!(
        s.windows(2).all(|w| w[0].col() < w[1].col()),
        "RenderInput.{} row {row} must be sorted by col with unique cells",
        T::NAME
    );
    s
}

/// [`sparse_row_slice`] over the Emberforge contrast-halo stream.
#[inline]
#[must_use]
pub fn fire_halo_row_slice(fire_halo: &[FireHaloCell], row: u16) -> &[FireHaloCell] {
    sparse_row_slice(fire_halo, usize::from(row))
}

/// Lockstep merge-walk over one row's slice ([`sparse_row_slice`]).
pub struct SparseRowWalk<'a, T> {
    slice: &'a [T],
    i: usize,
}

impl<'a, T: SparseCell> SparseRowWalk<'a, T> {
    #[inline]
    #[must_use]
    pub const fn new(slice: &'a [T]) -> Self {
        Self { slice, i: 0 }
    }

    /// The override at `col`, if any.
    #[inline]
    pub fn at(&mut self, col: u16) -> Option<T::V> {
        while let Some(c) = self.slice.get(self.i) {
            if c.col() < col {
                self.i += 1;
            } else if c.col() == col {
                return Some(c.value());
            } else {
                return None;
            }
        }
        None
    }
}

/// The animated-ink walk.
pub type InkWalk<'a> = SparseRowWalk<'a, InkCell>;
/// The Emberforge glyph-ink walk.
pub type CharFgWalk<'a> = SparseRowWalk<'a, CharFg>;
/// The Emberforge contrast-halo walk.
pub type FireHaloWalk<'a> = SparseRowWalk<'a, FireHaloCell>;

#[cfg(test)]
#[path = "lib_ink_walk_tests.rs"]
mod ink_walk_tests;

#[inline]
fn atlas_texels_valid(atlas: &SceneAtlas) -> bool {
    let need = (atlas.width as usize)
        .saturating_mul(atlas.height as usize)
        .saturating_mul(4);
    atlas.width != 0 && atlas.height != 0 && atlas.rgba.len() >= need
}

#[inline]
fn base_span(pixels: &mut [u32], start: usize, end: usize, bg: u32, base: Option<&[u32]>) {
    let end = end.min(pixels.len());
    if start >= end {
        return;
    }
    match base {
        Some(wp) if wp.len() == pixels.len() => pixels[start..end].copy_from_slice(&wp[start..end]),
        _ => pixels[start..end].fill(bg),
    }
}

fn ensure_wallpaper_px(wc: &mut WindowCpu, input: &RenderInput, w: usize, h: usize) {
    let Some(src_arc) = input.wallpaper.as_ref() else {
        wc.wallpaper_px = None;
        return;
    };
    let src = src_arc.as_ref();
    if src.width as usize != w || src.height as usize != h || !atlas_texels_valid(src) {
        wc.wallpaper_px = None;
        return;
    }
    let key = std::sync::Arc::as_ptr(src_arc) as usize;
    let need = w.saturating_mul(h);
    if wc
        .wallpaper_px
        .as_ref()
        .is_some_and(|p| p.key == key && p.px.len() == need)
    {
        return;
    }
    let mut px = wc.wallpaper_px.take().map(|p| p.px).unwrap_or_default();
    px.clear();
    px.reserve(need);
    px.extend(
        src.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .take(need)
            .map(|t| rgb_to_u32([t[0], t[1], t[2]])),
    );
    wc.wallpaper_px = Some(WallpaperPx { key, px });
}

/// Default inactive (unfocused) selection background when the host gives no `selectionInactiveBackground`.
#[inline]
#[must_use]
pub fn derive_inactive_selection_bg(active_selection: u32, theme_bg: u32) -> u32 {
    blend(theme_bg, active_selection, 128)
}

/// Blend `fg` over `bg` by coverage `t` (0..=255) per channel.
#[inline]
#[must_use]
pub fn blend_rgb(bg: u32, fg: u32, t: u8) -> u32 {
    blend(bg, fg, t)
}

/// Premultiply a `0x00RRGGBB` colour by coverage `a` (0..=255) per channel, round-half.
#[inline]
#[must_use]
pub const fn premul_rgb(rgb: u32, a: u8) -> u32 {
    let a = a as u32;
    let r = (((rgb >> 16) & 0xff) * a + 127) / 255;
    let g = (((rgb >> 8) & 0xff) * a + 127) / 255;
    let b = ((rgb & 0xff) * a + 127) / 255;
    (r << 16) | (g << 8) | b
}

/// The gradient law of a [`GlowQuad`] ([`GlowQuad::color2`]).
///
/// ```text
/// c(i) = (c0·(2w − m) + c1·m + w) / (2w),   m = 2i + 1
/// ```
#[must_use]
#[inline]
pub const fn glow_lerp(c0: u32, c1: u32, i: u32, w: u32) -> u32 {
    let w = if w < 1 { 1 } else { w };
    let i_clamped = if i < w - 1 { i } else { w - 1 };
    let m = 2 * i_clamped + 1;
    (c0 * (2 * w - m) + c1 * m + w) / (2 * w)
}

/// [`glow_lerp`] over a packed `0x00RRGGBB` word, per channel.
#[must_use]
#[inline]
pub const fn glow_lerp_rgb(c0: u32, c1: u32, i: u32, w: u32) -> u32 {
    if c0 == c1 {
        return c0 & 0x00FF_FFFF;
    }
    let w = if w < 1 { 1 } else { w };
    let i_clamped = if i < w - 1 { i } else { w - 1 };
    let m = 2 * i_clamped + 1;
    let denom = 2 * w;
    let w0 = denom - m;
    let r = (((c0 >> 16) & 0xff) * w0 + ((c1 >> 16) & 0xff) * m + w) / denom;
    let g = (((c0 >> 8) & 0xff) * w0 + ((c1 >> 8) & 0xff) * m + w) / denom;
    let b = ((c0 & 0xff) * w0 + (c1 & 0xff) * m + w) / denom;
    (r << 16) | (g << 8) | b
}

/// Saturating per-channel add of a premultiplied light colour onto `dst` (`min(255, dst_c + premul_c)`).
#[inline]
#[must_use]
pub const fn add_sat(dst: u32, premul: u32) -> u32 {
    let r = ((dst >> 16) & 0xff) + ((premul >> 16) & 0xff);
    let g = ((dst >> 8) & 0xff) + ((premul >> 8) & 0xff);
    let b = (dst & 0xff) + (premul & 0xff);
    let r = if r > 255 { 255 } else { r };
    let g = if g > 255 { 255 } else { g };
    let b = if b > 255 { 255 } else { b };
    (dst & 0xFF00_0000) | (r << 16) | (g << 8) | b
}

/// Source-over a premultiplied light colour `premul` onto `dst` at opacity `a` (0..=255), per channel.
#[inline]
#[must_use]
pub const fn over_premul(dst: u32, premul: u32, a: u8) -> u32 {
    if a == 0 {
        return add_sat(dst, premul);
    }
    let inv = 255 - a as u32;
    let r = ((premul >> 16) & 0xff) + (((dst >> 16) & 0xff) * inv + 127) / 255;
    let g = ((premul >> 8) & 0xff) + (((dst >> 8) & 0xff) * inv + 127) / 255;
    let b = (premul & 0xff) + ((dst & 0xff) * inv + 127) / 255;
    let r = if r > 255 { 255 } else { r };
    let g = if g > 255 { 255 } else { g };
    let b = if b > 255 { 255 } else { b };
    (dst & 0xFF00_0000) | (r << 16) | (g << 8) | b
}

/// Source-over a straight colour `rgb` onto `dst` at coverage `a` (0..=255), per channel, round-half.
#[inline]
#[must_use]
pub const fn over_rgb(dst: u32, rgb: u32, a: u8) -> u32 {
    let a = a as u32;
    let inv = 255 - a;
    let r = (((rgb >> 16) & 0xff) * a + ((dst >> 16) & 0xff) * inv + 127) / 255;
    let g = (((rgb >> 8) & 0xff) * a + ((dst >> 8) & 0xff) * inv + 127) / 255;
    let b = ((rgb & 0xff) * a + (dst & 0xff) * inv + 127) / 255;
    (dst & 0xFF00_0000) | (r << 16) | (g << 8) | b
}

/// Per-row y term of the [`RainHalo`] elliptical falloff, `ny = (dy²·256)/ry²` (`>= 256` means the row lies outside the ellipse), hoisted out of the pixel loop.
#[inline]
#[must_use]
pub const fn halo_row_ny(dy: i32, ry2: i32) -> i32 {
    (dy * dy * 256) / ry2
}

/// Per-pixel [`RainHalo`] falloff weight, `((256 − nsq)² >> 8)` clamped at 0 with `nsq = (dx²·256)/rx² + ny`.
#[inline]
#[must_use]
pub const fn halo_weight(dx: i32, ny: i32, rx2: i32) -> i32 {
    halo_weight_at(halo_col_nx(dx, rx2), ny)
}

/// The per-column x term of the same falloff, `nx = (dx²·256)/rx²`.
#[inline]
#[must_use]
pub const fn halo_col_nx(dx: i32, rx2: i32) -> i32 {
    (dx * dx * 256) / rx2
}

/// [`halo_weight`] with both axis terms already resolved.
#[inline]
#[must_use]
pub const fn halo_weight_at(nx: i32, ny: i32) -> i32 {
    let mut wt = 256 - (nx + ny);
    if wt < 0 {
        wt = 0;
    }
    (wt * wt) / 256
}

/// The per-pixel source-over alpha ceiling of a [`HaloMode::Over`] veil, carried in the high byte of its straight `color`.
#[inline]
#[must_use]
pub const fn halo_over_cap(color: u32) -> u8 {
    let cap = (color >> 24) as u8;
    if cap == 0 { 255 } else { cap }
}

/// One vertex of an additive-light polyline (the cursor comet beam) in window-absolute pixels.
#[derive(Clone, Copy, Debug)]
pub struct BeamVertex {
    /// Window-absolute x in pixels (sub-pixel).
    pub x: f32,
    /// Window-absolute y in pixels (sub-pixel).
    pub y: f32,
    /// Resolved colour `0x00RRGGBB` at this vertex (interpolated along the segment).
    pub color: u32,
    /// Coverage `0..=255` at this vertex (already scaled by time-fade × intensity).
    pub cov: u8,
}

#[inline]
fn lerp_rgb_f(a: u32, b: u32, t: f32) -> u32 {
    if a == b {
        return a;
    }
    let t = t.clamp(0.0, 1.0);
    let m = |sh: u32| -> u32 {
        let ca = ((a >> sh) & 0xff) as f32;
        let cb = ((b >> sh) & 0xff) as f32;
        (ca + (cb - ca) * t + 0.5) as u32
    };
    (m(16) << 16) | (m(8) << 8) | m(0)
}

/// The effects-box clip and row-tag anchor for the beam rasterizers.
#[derive(Clone, Copy)]
pub struct BeamClip {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
    pub cell_h: i32,
    pub origin_y: i32,
}
impl BeamClip {
    /// Identity clip for a bare grid (tests, benches, demos).
    #[inline]
    #[must_use]
    pub const fn grid(w_px: usize, h_px: usize, cell_h: usize) -> Self {
        Self {
            x0: 0,
            y0: 0,
            x1: w_px as i32,
            y1: h_px as i32,
            cell_h: cell_h as i32,
            origin_y: 0,
        }
    }
}

/// Lay the uniform rect `(x, y, w, h)` clipped to `clip`, a piece per cell
/// row band it crosses: through [`BeamRows::rect`] where `stacked` (a
/// column's pieces, one above the next), else each as its own quad.
#[allow(clippy::too_many_arguments)]
fn push_glow_rect<S: BeamRows + ?Sized>(
    out: &mut S,
    clip: BeamClip,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    premul: u32,
    alpha: u8,
    stacked: bool,
) {
    if w <= 0 || h <= 0 || (premul == 0 && alpha == 0) || clip.cell_h <= 0 {
        return;
    }
    let x0 = x.max(clip.x0);
    let x1 = (x + w).min(clip.x1);
    let y0 = y.max(clip.y0);
    let y1 = (y + h).min(clip.y1);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let mut yy = y0;
    while yy < y1 {
        let row_rel = (yy - clip.origin_y).div_euclid(clip.cell_h);
        let band_end = (clip.origin_y + (row_rel + 1) * clip.cell_h).min(y1);
        let (tag, piece) = (
            row_rel.max(0) as u16,
            (
                x0 as u16,
                yy as u16,
                (x1 - x0) as u16,
                (band_end - yy) as u16,
            ),
        );
        if stacked {
            out.rect(row_rel, tag, piece, (premul, alpha));
        } else {
            out.quad(GlowQuad {
                row: tag,
                x: piece.0,
                y: piece.1,
                w: piece.2,
                h: piece.3,
                color: premul,
                alpha,
                color2: premul,
                alpha2: alpha,
            });
        }
        yy = band_end;
    }
}

/// One slab of a beam: across its minor axis, an antialiased edge piece,
/// the full-coverage core and the far edge. An x-major slab's pieces stack
/// in its column, laid as one ramp's rows where `out` lays ramps and a run
/// of three lights rides one ([`BeamRows::rides`]).
#[allow(clippy::too_many_arguments)]
fn emit_aa_slab<S: BeamRows + ?Sized>(
    out: &mut S,
    clip: BeamClip,
    horizontal: bool,
    c: f32,
    half: f32,
    p: i32,
    len: i32,
    color: u32,
    cov: u8,
    over: bool,
) {
    let lo = c - half;
    let hi = c + half;
    if hi <= lo {
        return;
    }
    let cov = cov as f32;
    let il = lo.floor() as i32;
    let ih = hi.ceil() as i32;
    // A stacked slab's run within one band carries at most its edge, a core
    // piece and its far edge: where three lights cannot ride one ramp, its
    // pieces lay as the quads the sink would keep.
    let stacked = !horizontal && ih - il > 1 && out.rides(3);
    if stacked {
        out.slab();
    }
    let mut slab = |a: i32, width: i32, f: f32| {
        if width <= 0 {
            return;
        }
        let aa = (cov * f.clamp(0.0, 1.0)) as u8;
        if aa == 0 {
            return;
        }
        let premul = premul_rgb(color, aa);
        let alpha = if over { aa } else { 0 };
        if horizontal {
            push_glow_rect(out, clip, a, p, width, len, premul, alpha, false);
        } else {
            push_glow_rect(out, clip, p, a, len, width, premul, alpha, stacked);
        }
    };
    if ih - il <= 1 {
        slab(il, 1, hi - lo);
        return;
    }
    let cs = lo.ceil() as i32;
    let ce = hi.floor() as i32;
    if cs > il {
        slab(il, 1, cs as f32 - lo);
    }
    if ce > cs {
        slab(cs, ce - cs, 1.0);
    }
    slab(ce, 1, hi - ce as f32);
}

fn rdp_simplify(verts: &[BeamVertex], tol: f32) -> Vec<BeamVertex> {
    let n = verts.len();
    if n < 3 {
        return verts.to_vec();
    }
    let mut keep = vec![false; n];
    keep[0] = true;
    keep[n - 1] = true;
    let mut stack = vec![(0usize, n - 1)];
    while let Some((s, e)) = stack.pop() {
        if e <= s + 1 {
            continue;
        }
        let (ax, ay) = (verts[s].x, verts[s].y);
        let (dx, dy) = (verts[e].x - ax, verts[e].y - ay);
        let len = (dx * dx + dy * dy).sqrt().max(1e-6);
        let (mut idx, mut dmax) = (0usize, -1.0f32);
        for (i, v) in verts.iter().enumerate().take(e).skip(s + 1) {
            let d = ((v.x - ax) * dy - (v.y - ay) * dx).abs() / len;
            if d > dmax {
                dmax = d;
                idx = i;
            }
        }
        if dmax > tol {
            keep[idx] = true;
            stack.push((s, idx));
            stack.push((idx, e));
        }
    }
    verts
        .iter()
        .zip(keep)
        .filter_map(|(v, k)| k.then_some(*v))
        .collect()
}

#[cfg(test)]
fn rdp_keep_indices(run: &[CometSample], tol: f32) -> Vec<usize> {
    let (mut keep, mut stack, mut kept) = (Vec::new(), Vec::new(), Vec::new());
    rdp_keep_indices_into(run, tol, &mut keep, &mut stack, &mut kept);
    kept
}

fn rdp_keep_indices_into(
    run: &[CometSample],
    tol: f32,
    keep: &mut Vec<bool>,
    stack: &mut Vec<(usize, usize)>,
    kept: &mut Vec<usize>,
) {
    kept.clear();
    let n = run.len();
    if n < 3 {
        kept.extend(0..n);
        return;
    }
    keep.clear();
    keep.resize(n, false);
    keep[0] = true;
    keep[n - 1] = true;
    stack.clear();
    stack.push((0usize, n - 1));
    while let Some((s, e)) = stack.pop() {
        if e <= s + 1 {
            continue;
        }
        let (ax, ay) = (run[s].x, run[s].y);
        let (dx, dy) = (run[e].x - ax, run[e].y - ay);
        let len = (dx * dx + dy * dy).sqrt().max(1e-6);
        let (mut idx, mut dmax) = (0usize, -1.0f32);
        for (i, v) in run.iter().enumerate().take(e).skip(s + 1) {
            let d = ((v.x - ax) * dy - (v.y - ay) * dx).abs() / len;
            if d > dmax {
                dmax = d;
                idx = i;
            }
        }
        if dmax > tol {
            keep[idx] = true;
            stack.push((s, idx));
            stack.push((idx, e));
        }
    }
    kept.extend((0..n).filter(|&i| keep[i]));
}

/// The caller-owned working buffers of the layered beam rasterizer ([`layered_beam_quads_with`]).
#[derive(Debug, Default)]
pub struct BeamScratch {
    styles: Vec<u32>,
    keep: Vec<bool>,
    stack: Vec<(usize, usize)>,
    kept: Vec<usize>,
    verts: Vec<BeamVertex>,
    feathered: Vec<BeamVertex>,
}

impl BeamScratch {
    /// No buffers yet, for an owner built in a `const fn`.
    pub const EMPTY: Self = Self {
        styles: Vec::new(),
        keep: Vec::new(),
        stack: Vec::new(),
        kept: Vec::new(),
        verts: Vec::new(),
        feathered: Vec::new(),
    };

    fn simplify(&mut self, run: &[CometSample], straighten: f32) {
        if straighten > 0.0 && run.len() > 2 {
            rdp_keep_indices_into(
                run,
                straighten,
                &mut self.keep,
                &mut self.stack,
                &mut self.kept,
            );
        } else {
            self.kept.clear();
            self.kept.extend(0..run.len());
        }
    }

    /// The vertices of `run` a layered beam laid with `straighten` keeps
    /// ([`comet_glow_quads`]; every one at 0), into `out`: the line it draws,
    /// for a caller that cuts it finer before laying it unstraightened.
    #[inline]
    pub fn straighten_into(
        &mut self,
        run: &[CometSample],
        straighten: f32,
        out: &mut Vec<CometSample>,
    ) {
        self.simplify(run, straighten);
        out.clear();
        out.extend(self.kept.iter().map(|&i| run[i]));
    }
}

/// Rasterize a thick antialiased additive polyline (the cursor comet beam) into per-cell-row [`GlowQuad`]s, or ramps where `out` lays them ([`BeamRows`]).
#[inline]
pub fn comet_beam<S: BeamRows + ?Sized>(
    out: &mut S,
    clip: BeamClip,
    verts: &[BeamVertex],
    thickness: f32,
    step: usize,
    straighten: f32,
) {
    beam_polyline(out, clip, verts, thickness, step, straighten, false);
}

/// [`comet_beam`]'s source-over twin, for a light ground where additive light cannot show.
#[inline]
pub fn comet_beam_over<S: BeamRows + ?Sized>(
    out: &mut S,
    clip: BeamClip,
    verts: &[BeamVertex],
    thickness: f32,
    step: usize,
    straighten: f32,
) {
    beam_polyline(out, clip, verts, thickness, step, straighten, true);
}

#[allow(clippy::too_many_arguments)]
fn beam_polyline<S: BeamRows + ?Sized>(
    out: &mut S,
    clip: BeamClip,
    verts: &[BeamVertex],
    thickness: f32,
    step: usize,
    straighten: f32,
    over: bool,
) {
    if verts.len() < 2 || thickness <= 0.0 || clip.cell_h <= 0 {
        return;
    }
    let simplified;
    let verts: &[BeamVertex] = if straighten > 0.0 && verts.len() > 2 {
        simplified = rdp_simplify(verts, straighten);
        &simplified
    } else {
        verts
    };
    let half_perp = (thickness * 0.5).max(0.5);
    let step = step.max(1) as f32;
    let mut prev: Option<(bool, i32, i32, bool)> = None;
    for w in verts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let seg = (dx * dx + dy * dy).sqrt();
        if seg < 1e-3 {
            continue;
        }
        let horizontal = dy.abs() >= dx.abs();
        let major = if horizontal { dy.abs() } else { dx.abs() };
        let half =
            (half_perp * seg / major.max(1e-3)).min(half_perp * std::f32::consts::SQRT_2 + 0.5);
        let (m0, m1) = if horizontal { (a.y, b.y) } else { (a.x, b.x) };
        let stepi = step.max(1.0) as i32;
        let (start, end) = (m0.min(m1).floor() as i32, m0.max(m1).ceil() as i32);
        let increasing = m1 >= m0;
        let mut p = start;
        let mut tile_end = end;
        if let Some((ph, ps, pe, pinc)) = prev
            && ph == horizontal
            && pinc == increasing
        {
            if increasing {
                p = p.max(pe);
            } else {
                tile_end = tile_end.min(ps);
            }
        }
        prev = Some((horizontal, start, end, increasing));
        let end = tile_end;
        while p < end {
            let len = stepi.min(end - p).max(1);
            let t = (((p as f32 + len as f32 * 0.5) - m0) / (m1 - m0)).clamp(0.0, 1.0);
            let cov = (a.cov as f32 + (b.cov as f32 - a.cov as f32) * t) as u8;
            if cov > 0 {
                let color = lerp_rgb_f(a.color, b.color, t);
                if horizontal {
                    emit_aa_slab(
                        out,
                        clip,
                        true,
                        a.x + t * dx,
                        half,
                        p,
                        len,
                        color,
                        cov,
                        over,
                    );
                } else {
                    emit_aa_slab(
                        out,
                        clip,
                        false,
                        a.y + t * dy,
                        half,
                        p,
                        len,
                        color,
                        cov,
                        over,
                    );
                }
            }
            p += len;
        }
    }
}

impl Eq for AdmittedFontBlob {}

/// The largest share of a reach the ribbon's full-strength core may take, so at least half of every reach is falloff and the profile never degenerates into a hard-edged bar.
pub const RIBBON_CORE_SHARE: f32 = 0.5;

/// Cubic Hermite smoothstep over `0..1`, clamping its input.
#[inline]
#[must_use]
pub const fn smoothstep01(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// The ribbon's transverse profile.
#[inline]
#[must_use]
pub fn ribbon_profile(d: f32, core: f32, reach: f32, shoulder: f32) -> f32 {
    if reach.is_nan() || reach <= 0.0 {
        return 0.0;
    }
    let d = d.abs();
    if d.is_nan() || d >= reach {
        return 0.0;
    }
    let core = if core.is_finite() {
        core.clamp(0.0, reach * RIBBON_CORE_SHARE)
    } else {
        0.0
    };
    let melt = smoothstep01((reach - d) / (reach - core));
    if shoulder == 1.0 {
        return melt;
    }
    let bump = smoothstep01((core - d) / core.max(f32::MIN_POSITIVE));
    melt * (shoulder + (1.0 - shoulder) * bump)
}

/// The glass's transverse profile (`docs/design/PRISM-GLASS.md` §2.2, §15.2 1.7): whole light
/// from the spine to within `melt` px of the `reach`, melting to nothing over those px
/// (`smoothstep01((reach - d) / melt)`), and nothing at or past the reach. A `melt` that is not
/// a positive number is a hard edge. [`ribbon_profile`] cannot draw it: its core keeps at least
/// half of every reach for its falloff ([`RIBBON_CORE_SHARE`]).
#[inline]
#[must_use]
pub fn glass_profile(d: f32, reach: f32, melt: f32) -> f32 {
    if reach.is_nan() || reach <= 0.0 {
        return 0.0;
    }
    let d = d.abs();
    if d.is_nan() || d >= reach {
        return 0.0;
    }
    if melt.is_nan() || melt <= 0.0 {
        return 1.0;
    }
    smoothstep01((reach - d) / melt)
}

/// How a ribbon's light falls off above its spine: [`ribbon_profile`] at a shoulder, or the
/// glass's [`glass_profile`] at a melt, px. Below the spine every ribbon is
/// [`ribbon_profile`] at shoulder `1`.
#[derive(Clone, Copy, Debug)]
enum Across {
    Shoulder(f32),
    Melt(f32),
}

impl Across {
    /// The light `d` px above the spine of a reach `reach` whose core is `core`.
    #[inline]
    fn above(self, d: f32, core: f32, reach: f32) -> f32 {
        match self {
            Self::Shoulder(shoulder) => ribbon_profile(d, core, reach, shoulder),
            Self::Melt(melt) => glass_profile(d, reach, melt),
        }
    }
}

/// The 4x4 ordered (Bayer) dither, normalized to `0..1` and offset by half a step so its mean is exactly 0.5.
pub const BAYER4: [[f32; 4]; 4] = [
    [0.5 / 16.0, 8.5 / 16.0, 2.5 / 16.0, 10.5 / 16.0],
    [12.5 / 16.0, 4.5 / 16.0, 14.5 / 16.0, 6.5 / 16.0],
    [3.5 / 16.0, 11.5 / 16.0, 1.5 / 16.0, 9.5 / 16.0],
    [15.5 / 16.0, 7.5 / 16.0, 13.5 / 16.0, 5.5 / 16.0],
];

/// One vertex of the ribbon polyline, in window-absolute pixels (the producer folds in the grid origin).
#[derive(Clone, Copy, Debug)]
pub struct RibbonVertex {
    /// Window-absolute x in pixels (sub-pixel).
    pub x: f32,
    /// The mark's full-strength centreline here (sub-pixel).
    pub spine: f32,
    /// How far the profile reaches above the spine before it is exactly zero.
    pub up: f32,
    /// How far the profile reaches below the spine before it is exactly zero.
    pub dn: f32,
    /// The full-strength half-width above the spine.
    pub core_up: f32,
    /// The full-strength half-width below the spine.
    pub core_dn: f32,
    /// Resolved colour `0x00RRGGBB` here (interpolated along the segment).
    pub color: u32,
    /// Coverage at the spine, `0..=255`, with time-fade, intensity, specular and drain already folded in.
    pub cov: f32,
    /// Extra coverage at the spine above `cov`, `0..=255`.
    pub lift: f32,
    /// How far above the spine the lift's bump extends before it is zero, in device pixels.
    pub lift_span: f32,
}

/// The share of the downward reach the spine's lift melts over, inside the next row's ink-free top margin (the glyph band starts `0.22` of a cell below the cell top; the reach is `0.25..0.29`), with zero slope at its end.
pub const RIBBON_LIFT_DN_MELT: f32 = 0.45;

/// The lift's transverse profile.
#[inline]
#[must_use]
pub fn ribbon_lift_profile(d: f32, lift_span: f32, dn: f32) -> f32 {
    if d.is_nan() {
        return 0.0;
    }
    let span = if d < 0.0 {
        lift_span
    } else {
        dn * RIBBON_LIFT_DN_MELT
    };
    if !span.is_finite() || span <= 0.0 {
        return 0.0;
    }
    1.0 - smoothstep01(d.abs() / span)
}

/// Rasterize the ribbon polyline (x-major) into per-cell-row [`GlowQuad`]s.
#[allow(clippy::too_many_arguments)]
pub fn ribbon_beam(
    out: &mut Vec<GlowQuad>,
    clip: BeamClip,
    verts: &[RibbonVertex],
    shoulder: f32,
    step: usize,
    max_quads: usize,
    blend: GlowBlend,
) -> bool {
    ribbon_beam_into(out, clip, verts, shoulder, step, max_quads, blend)
}

/// [`ribbon_beam`] into any [`BeamRows`] sink: a slab's one-pixel rows laid
/// as its quads or, through a [`RampSink`], as one ramp per band, the
/// `max_quads` budget counting each lit row alike.
#[allow(clippy::too_many_arguments)]
pub fn ribbon_beam_into<S: BeamRows>(
    out: &mut S,
    clip: BeamClip,
    verts: &[RibbonVertex],
    shoulder: f32,
    step: usize,
    max_quads: usize,
    blend: GlowBlend,
) -> bool {
    let start = out.mark();
    if ribbon_beam_ordered(out, clip, verts, shoulder, step, max_quads, blend, false) {
        return true;
    }
    if !verts.windows(2).any(|w| w[1].x < w[0].x) {
        return false;
    }
    out.truncate(start);
    ribbon_beam_ordered(out, clip, verts, shoulder, step, max_quads, blend, true)
}

#[allow(clippy::too_many_arguments)]
fn ribbon_beam_ordered<S: BeamRows>(
    out: &mut S,
    clip: BeamClip,
    verts: &[RibbonVertex],
    shoulder: f32,
    step: usize,
    max_quads: usize,
    blend: GlowBlend,
    head_first_reversed: bool,
) -> bool {
    if verts.len() < 2 || clip.cell_h <= 0 {
        return true;
    }
    let step = step.clamp(1, i32::MAX as usize) as i32;
    for w in verts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let span = b.x - a.x;
        if !span.is_finite() || span.abs() < 1e-3 {
            continue;
        }
        let (start, end) = (a.x.min(b.x).ceil() as i32, a.x.max(b.x).ceil() as i32);
        let width = end - start;
        if width <= 0 {
            continue;
        }
        let slabs = (width - 1) / step + 1;
        let reverse = head_first_reversed && span < 0.0;
        let mut p = start + if reverse { (slabs - 1) * step } else { 0 };
        let advance = if reverse { -step } else { step };
        for i in 0..slabs {
            let len = step.min(end - p).max(1);
            let t = ((p as f32 + len as f32 * 0.5 - a.x) / span).clamp(0.0, 1.0);
            let lerp = |u: f32, v: f32| u + (v - u) * t;
            let spine = lerp(a.spine, b.spine);
            let up = lerp(a.up, b.up);
            let dn = lerp(a.dn, b.dn);
            let core_up = lerp(a.core_up, b.core_up);
            let core_dn = lerp(a.core_dn, b.core_dn);
            let cov = lerp(a.cov, b.cov);
            let lift = lerp(a.lift, b.lift).max(0.0);
            let lift_span = lerp(a.lift_span, b.lift_span);
            let top = spine - up;
            let bot = spine + dn;
            if !top.is_nan() && !bot.is_nan() && bot > top && cov >= 1.0 {
                let y0 = top.floor() as i32;
                let y1 = bot.ceil() as i32;
                let dither_x = p.div_euclid(step).rem_euclid(4) as usize;
                let rect_x0 = p.max(clip.x0);
                let rect_x1 = (p + len).min(clip.x1);
                let t0 = ((rect_x0 as f32 - a.x) / span).clamp(0.0, 1.0);
                let t1 = ((rect_x1 as f32 - a.x) / span).clamp(0.0, 1.0);
                let col0 = lerp_rgb_f(a.color, b.color, t0);
                let col1 = lerp_rgb_f(a.color, b.color, t1);
                let mut row_rel = (y0 - clip.origin_y).div_euclid(clip.cell_h);
                let mut band_end = clip.origin_y + (row_rel + 1) * clip.cell_h;
                out.slab();
                for y in y0..y1 {
                    if out.laid() + 2 > max_quads {
                        return false;
                    }
                    if y >= band_end {
                        row_rel += 1;
                        band_end += clip.cell_h;
                    }
                    let lo = (y as f32).max(top);
                    let hi = ((y + 1) as f32).min(bot);
                    let cover = hi - lo;
                    if cover <= 0.0 {
                        continue;
                    }
                    let d = (lo + hi) * 0.5 - spine;
                    let across = if d < 0.0 {
                        ribbon_profile(d, core_up, up, shoulder)
                    } else {
                        ribbon_profile(d, core_dn, dn, 1.0)
                    };
                    let level = if lift == 0.0 {
                        cov * across
                    } else {
                        cov * across + lift * ribbon_lift_profile(d, lift_span, dn)
                    };
                    let c = (level * cover + BAYER4[y.rem_euclid(4) as usize][dither_x]) as u8;
                    if c > 0 {
                        let premul0 = premul_rgb(col0, c);
                        let premul1 = if col0 == col1 {
                            premul0
                        } else {
                            premul_rgb(col1, c)
                        };
                        if (premul0 != 0 || premul1 != 0)
                            && rect_x1 > rect_x0
                            && y >= clip.y0
                            && y < clip.y1
                        {
                            let alpha = match blend {
                                GlowBlend::Add => 0,
                                GlowBlend::Over => c,
                            };
                            out.row(
                                row_rel,
                                row_rel.max(0) as u16,
                                (rect_x0 as u16, y as u16, (rect_x1 - rect_x0) as u16),
                                (premul0, alpha),
                                (premul1, alpha),
                            );
                        }
                    }
                }
            }
            if i + 1 < slabs {
                p += advance;
            }
        }
    }
    true
}

/// [`ribbon_beam_into`] with each slab's slope drawn per device row
/// (`docs/design/PRISM-GLASS.md` §2.2, the wedge): a slab whose top or
/// bottom edge stands more than `edge_px` higher at one of its pixel edges
/// than at the other is a wedge, and every other slab is laid exactly as
/// [`ribbon_beam_into`] lays it, at its centre. A wedge's row is laid as
/// the runs between the pixel edges where the row meets the profile's
/// lines ([`RibbonAt::lines`]) and where a run's chord would stray from
/// the row's own light ([`WEDGE_LEVEL_SLACK`]), each run's ends the row's
/// level at its pixel edges under the segment's own lerp: a run whose ends
/// stand at one level is flat, and another a ramp whose ends carry the segment's
/// colours at its pixel edges premultiplied by their own opacities, so a
/// zero-opacity end carries no colour; a run dark at both ends lays
/// nothing. The dither keeps the slab's screen ordinal, and the clip, the
/// bands, the budget and the head-first retry are [`ribbon_beam_into`]'s.
#[allow(clippy::too_many_arguments)]
pub fn ribbon_wedge_into<S: BeamRows>(
    out: &mut S,
    clip: BeamClip,
    verts: &[RibbonVertex],
    shoulder: f32,
    step: usize,
    max_quads: usize,
    blend: GlowBlend,
    edge_px: f32,
) -> bool {
    ribbon_wedge_walk(
        out,
        clip,
        verts,
        (Across::Shoulder(shoulder), step, max_quads, blend, edge_px),
    )
}

/// The edge step the light glass lays its slabs by ([`ribbon_wedge_into`]'s and
/// [`ribbon_glass_into`]'s `edge_px`, `docs/design/PRISM-GLASS.md` §15.2 1.7): a slab whose
/// top climbs more than half a pixel across it is a wedge.
pub const WEDGE_EDGE_PX: f32 = 0.5;

/// The light glass's body (`docs/design/PRISM-GLASS.md` §2.2, §15.2 1.7): [`ribbon_wedge_into`]
/// source-over, each vertex's light above its spine the glass's [`glass_profile`] at `melt`
/// px (its top edge) in place of [`ribbon_profile`], and below it [`ribbon_profile`]'s at
/// shoulder `1` (the glass lays its vertices with no reach below). The walk, the wedges at
/// `edge_px`, the clip, the bands, the budget and the head-first retry are
/// [`ribbon_wedge_into`]'s.
#[inline]
pub fn ribbon_glass_into<S: BeamRows>(
    out: &mut S,
    clip: BeamClip,
    verts: &[RibbonVertex],
    step: usize,
    max_quads: usize,
    edge_px: f32,
    melt: f32,
) -> bool {
    ribbon_wedge_walk(
        out,
        clip,
        verts,
        (
            Across::Melt(melt),
            step,
            max_quads,
            GlowBlend::Over,
            edge_px,
        ),
    )
}

/// [`ribbon_wedge_into`]'s walk, and its head-first retry where the budget ran out.
#[inline]
fn ribbon_wedge_walk<S: BeamRows>(
    out: &mut S,
    clip: BeamClip,
    verts: &[RibbonVertex],
    walk: (Across, usize, usize, GlowBlend, f32),
) -> bool {
    let start = out.mark();
    if ribbon_wedge_ordered(out, clip, verts, walk, false) {
        return true;
    }
    if !verts.windows(2).any(|w| w[1].x < w[0].x) {
        return false;
    }
    out.truncate(start);
    ribbon_wedge_ordered(out, clip, verts, walk, true)
}

/// [`ribbon_beam_ordered`]'s walk, each slab laid as a wedge where its
/// edges slope ([`ribbon_wedge_into`]).
fn ribbon_wedge_ordered<S: BeamRows>(
    out: &mut S,
    clip: BeamClip,
    verts: &[RibbonVertex],
    (across, step, max_quads, blend, edge_px): (Across, usize, usize, GlowBlend, f32),
    head_first_reversed: bool,
) -> bool {
    if verts.len() < 2 || clip.cell_h <= 0 {
        return true;
    }
    let step = step.clamp(1, i32::MAX as usize) as i32;
    for w in verts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let span = b.x - a.x;
        if !span.is_finite() || span.abs() < 1e-3 {
            continue;
        }
        let (start, end) = (a.x.min(b.x).ceil() as i32, a.x.max(b.x).ceil() as i32);
        let width = end - start;
        if width <= 0 {
            continue;
        }
        let slabs = (width - 1) / step + 1;
        let reverse = head_first_reversed && span < 0.0;
        let mut p = start + if reverse { (slabs - 1) * step } else { 0 };
        let advance = if reverse { -step } else { step };
        for i in 0..slabs {
            let len = step.min(end - p).max(1);
            let rect = (p.max(clip.x0), (p + len).min(clip.x1));
            let at = |x: i32| RibbonAt::lerp(&a, &b, ((x as f32 - a.x) / span).clamp(0.0, 1.0));
            let (e0, e1) = (at(rect.0), at(rect.1));
            let edges = [e0.top(), e1.top(), e0.bot(), e1.bot()];
            let wedge = rect.1 > rect.0
                && edges.iter().all(|v| v.is_finite())
                && ((edges[1] - edges[0]).abs() > edge_px || (edges[3] - edges[2]).abs() > edge_px);
            #[cfg(any(test, debug_assertions))]
            if matches!(across, Across::Melt(_)) {
                crate::test_counters::count_glass_slab();
            }
            let laid = if wedge {
                ribbon_wedge_rows(
                    out,
                    clip,
                    (&a, &b),
                    p.div_euclid(step),
                    rect,
                    across,
                    max_quads,
                    blend,
                )
            } else {
                ribbon_slab_rows(
                    out,
                    clip,
                    (&a, &b),
                    (p, len, step),
                    across,
                    max_quads,
                    blend,
                )
            };
            if !laid {
                return false;
            }
            if i + 1 < slabs {
                p += advance;
            }
        }
    }
    true
}

/// The ribbon's geometry at one point of a segment, each field its
/// vertices' lerp there.
#[derive(Clone, Copy, Debug)]
struct RibbonAt {
    spine: f32,
    up: f32,
    dn: f32,
    core_up: f32,
    core_dn: f32,
    cov: f32,
    lift: f32,
    lift_span: f32,
}

impl RibbonAt {
    /// The geometry `t` of the way along `a → b`.
    #[inline]
    fn lerp(a: &RibbonVertex, b: &RibbonVertex, t: f32) -> Self {
        let lerp = |u: f32, v: f32| u + (v - u) * t;
        Self {
            spine: lerp(a.spine, b.spine),
            up: lerp(a.up, b.up),
            dn: lerp(a.dn, b.dn),
            core_up: lerp(a.core_up, b.core_up),
            core_dn: lerp(a.core_dn, b.core_dn),
            cov: lerp(a.cov, b.cov),
            lift: lerp(a.lift, b.lift).max(0.0),
            lift_span: lerp(a.lift_span, b.lift_span),
        }
    }

    /// The mark's top.
    #[inline]
    fn top(&self) -> f32 {
        self.spine - self.up
    }

    /// The mark's bottom.
    #[inline]
    fn bot(&self) -> f32 {
        self.spine + self.dn
    }

    /// The lines a row's level turns at, top to bottom: the mark's top, its
    /// core's top, the spine, its core's bottom and its bottom, each core as
    /// [`ribbon_profile`] clamps it; under a glass's melt the second is where
    /// the melt ends ([`glass_profile`]), the melt's px below the top and no
    /// lower than the spine.
    #[inline]
    fn lines(&self, across: Across) -> [f32; 5] {
        let core = |core: f32, reach: f32| {
            if core.is_finite() && reach > 0.0 {
                core.clamp(0.0, reach * RIBBON_CORE_SHARE)
            } else {
                0.0
            }
        };
        let solid = match across {
            Across::Shoulder(_) => core(self.core_up, self.up),
            Across::Melt(melt) if melt > 0.0 && self.up > 0.0 => (self.up - melt).max(0.0),
            Across::Melt(_) => self.up.max(0.0),
        };
        [
            self.top(),
            self.spine - solid,
            self.spine,
            self.spine + core(self.core_dn, self.dn),
            self.bot(),
        ]
    }

    /// Row `y`'s light here before its dither: [`ribbon_beam_ordered`]'s
    /// row at this geometry, which adds the dither and truncates.
    #[inline]
    fn light(&self, y: i32, across: Across) -> f32 {
        let (top, bot) = (self.top(), self.bot());
        if top.is_nan() || bot.is_nan() || bot <= top || self.cov.is_nan() || self.cov < 1.0 {
            return 0.0;
        }
        let lo = (y as f32).max(top);
        let hi = ((y + 1) as f32).min(bot);
        let cover = hi - lo;
        if cover <= 0.0 {
            return 0.0;
        }
        let d = (lo + hi) * 0.5 - self.spine;
        let shape = if d < 0.0 {
            across.above(d, self.core_up, self.up)
        } else {
            ribbon_profile(d, self.core_dn, self.dn, 1.0)
        };
        let level = if self.lift == 0.0 {
            self.cov * shape
        } else {
            self.cov * shape + self.lift * ribbon_lift_profile(d, self.lift_span, self.dn)
        };
        level * cover
    }
}

/// Lay the slab `(p, len)` of `a → b` as [`ribbon_beam_ordered`] lays it, at
/// its centre; `false` where the budget ran out.
#[inline]
fn ribbon_slab_rows<S: BeamRows>(
    out: &mut S,
    clip: BeamClip,
    (a, b): (&RibbonVertex, &RibbonVertex),
    (p, len, step): (i32, i32, i32),
    across: Across,
    max_quads: usize,
    blend: GlowBlend,
) -> bool {
    let span = b.x - a.x;
    let t = ((p as f32 + len as f32 * 0.5 - a.x) / span).clamp(0.0, 1.0);
    let lerp = |u: f32, v: f32| u + (v - u) * t;
    let spine = lerp(a.spine, b.spine);
    let up = lerp(a.up, b.up);
    let dn = lerp(a.dn, b.dn);
    let core_up = lerp(a.core_up, b.core_up);
    let core_dn = lerp(a.core_dn, b.core_dn);
    let cov = lerp(a.cov, b.cov);
    let lift = lerp(a.lift, b.lift).max(0.0);
    let lift_span = lerp(a.lift_span, b.lift_span);
    let top = spine - up;
    let bot = spine + dn;
    if !top.is_nan() && !bot.is_nan() && bot > top && cov >= 1.0 {
        let y0 = top.floor() as i32;
        let y1 = bot.ceil() as i32;
        let dither_x = p.div_euclid(step).rem_euclid(4) as usize;
        let rect_x0 = p.max(clip.x0);
        let rect_x1 = (p + len).min(clip.x1);
        let t0 = ((rect_x0 as f32 - a.x) / span).clamp(0.0, 1.0);
        let t1 = ((rect_x1 as f32 - a.x) / span).clamp(0.0, 1.0);
        let col0 = lerp_rgb_f(a.color, b.color, t0);
        let col1 = lerp_rgb_f(a.color, b.color, t1);
        let mut row_rel = (y0 - clip.origin_y).div_euclid(clip.cell_h);
        let mut band_end = clip.origin_y + (row_rel + 1) * clip.cell_h;
        out.slab();
        for y in y0..y1 {
            if out.laid() + 2 > max_quads {
                return false;
            }
            if y >= band_end {
                row_rel += 1;
                band_end += clip.cell_h;
            }
            let lo = (y as f32).max(top);
            let hi = ((y + 1) as f32).min(bot);
            let cover = hi - lo;
            if cover <= 0.0 {
                continue;
            }
            let d = (lo + hi) * 0.5 - spine;
            let shape = if d < 0.0 {
                across.above(d, core_up, up)
            } else {
                ribbon_profile(d, core_dn, dn, 1.0)
            };
            let level = if lift == 0.0 {
                cov * shape
            } else {
                cov * shape + lift * ribbon_lift_profile(d, lift_span, dn)
            };
            let c = (level * cover + BAYER4[y.rem_euclid(4) as usize][dither_x]) as u8;
            if c > 0 {
                let premul0 = premul_rgb(col0, c);
                let premul1 = if col0 == col1 {
                    premul0
                } else {
                    premul_rgb(col1, c)
                };
                if (premul0 != 0 || premul1 != 0)
                    && rect_x1 > rect_x0
                    && y >= clip.y0
                    && y < clip.y1
                {
                    let alpha = match blend {
                        GlowBlend::Add => 0,
                        GlowBlend::Over => c,
                    };
                    out.row(
                        row_rel,
                        row_rel.max(0) as u16,
                        (rect_x0 as u16, y as u16, (rect_x1 - rect_x0) as u16),
                        (premul0, alpha),
                        (premul1, alpha),
                    );
                }
            }
        }
    }
    true
}

/// The most pixel edges a wedge's row turns at: its slab's two, where it
/// meets each of the profile's lines ([`RibbonAt::lines`], seven), and
/// where its runs are split ([`WEDGE_LEVEL_SLACK`]).
const WEDGE_KNOTS: usize = 24;

/// How many levels a wedge row's run may stray from the row's own light
/// at a quarter of its way before it is split there: a run of a profile's
/// short edge stays one ramp, and a long melt is laid as the chords that
/// follow it.
const WEDGE_LEVEL_SLACK: f32 = 4.0;

/// Lay the wedge over the columns `x0..x1` of `a → b`, the slab `ordinal`
/// steps from the screen's left ([`ribbon_wedge_into`]); `false` where the
/// budget ran out.
#[allow(clippy::too_many_arguments)]
fn ribbon_wedge_rows<S: BeamRows>(
    out: &mut S,
    clip: BeamClip,
    (a, b): (&RibbonVertex, &RibbonVertex),
    ordinal: i32,
    (x0, x1): (i32, i32),
    across: Across,
    max_quads: usize,
    blend: GlowBlend,
) -> bool {
    #[cfg(any(test, debug_assertions))]
    crate::test_counters::count_wedge_slab();
    let span = b.x - a.x;
    let t = |x: i32| ((x as f32 - a.x) / span).clamp(0.0, 1.0);
    let (e0, e1) = (RibbonAt::lerp(a, b, t(x0)), RibbonAt::lerp(a, b, t(x1)));
    // The lines run on past the segment's ends, so a slab overhanging its
    // vertex still meets them where the segment does.
    let line = |x: i32| RibbonAt::lerp(a, b, (x as f32 - a.x) / span).lines(across);
    let (l0, l1) = (line(x0), line(x1));
    let y0 = e0.top().min(e1.top()).floor().max(clip.y0 as f32) as i32;
    let y1 = e0.bot().max(e1.bot()).ceil().min(clip.y1 as f32) as i32;
    let dither_x = ordinal.rem_euclid(4) as usize;
    let alpha = |c: u8| match blend {
        GlowBlend::Add => 0,
        GlowBlend::Over => c,
    };
    out.slab();
    for y in y0..y1 {
        let row_rel = (y - clip.origin_y).div_euclid(clip.cell_h);
        let dither = BAYER4[y.rem_euclid(4) as usize][dither_x];
        let light = |x: i32| RibbonAt::lerp(a, b, t(x)).light(y, across);
        // The pixel edges, between the slab's own, where the row's centre
        // meets each line, the nearest, and where its bottom meets the
        // mark's top and its top the mark's bottom, the first beyond them
        // on the dark side: the row's light is near linear between them, and
        // nothing beyond the last two. `Some(above)`: the row is dark where
        // the line stands at or above (`true`) or at or below the meeting.
        let centre = y as f32 + 0.5;
        let meets = [
            (0, (y + 1) as f32, Some(true)),
            (0, centre, None),
            (1, centre, None),
            (2, centre, None),
            (3, centre, None),
            (4, centre, None),
            (4, y as f32, Some(false)),
        ];
        let mut knots = [(x0, 0.0f32); WEDGE_KNOTS];
        knots[1].0 = x1;
        let mut n = 2;
        for (line, at, dark) in meets {
            let (f0, f1) = (l0[line], l1[line]);
            let u = (at - f0) / (f1 - f0);
            if u > 0.0 && u < 1.0 {
                let meet = x0 as f32 + u * (x1 - x0) as f32;
                let k = match dark {
                    None => meet.round() as i32,
                    Some(above) => {
                        // The first pixel edge past the meeting the row is
                        // dark at, under the segment's own clamped lerp.
                        let toward = if (f1 > f0) == above { 1 } else { -1 };
                        let mut k = if toward > 0 {
                            meet.ceil()
                        } else {
                            meet.floor()
                        } as i32;
                        while k > x0 && k < x1 && light(k) > 0.0 {
                            k += toward;
                        }
                        k
                    }
                };
                if k > x0 && k < x1 {
                    knots[n].0 = k;
                    n += 1;
                }
            }
        }
        knots[..n].sort_unstable_by_key(|k| k.0);
        for k in &mut knots[..n] {
            k.1 = light(k.0);
        }
        // A run whose chord strays from the row's own light at a quarter of
        // its way (where a smoothstep's chord strays most) by more than
        // WEDGE_LEVEL_SLACK is split there, before the dither, so the screen's
        // dither never moves a knot.
        let mut i = 0;
        while i + 1 < n {
            let ((k0, c0), (k1, c1)) = (knots[i], knots[i + 1]);
            let width = k1 - k0;
            if width >= 4 && n < WEDGE_KNOTS {
                let stray = |k: i32| {
                    let c = light(k);
                    let chord = c0 + (c1 - c0) * (k - k0) as f32 / width as f32;
                    ((c - chord).abs(), k, c)
                };
                let (early, late) = (stray(k0 + width / 4), stray(k1 - width / 4));
                let worst = if late.0 > early.0 { late } else { early };
                if worst.0 > WEDGE_LEVEL_SLACK {
                    knots.copy_within(i + 1..n, i + 2);
                    knots[i + 1] = (worst.1, worst.2);
                    n += 1;
                    continue;
                }
            }
            i += 1;
        }
        // The run being laid, `(from, its level, to, its level)`: a flat
        // run grows over the next while that one stands at its level too.
        let mut run: Option<(i32, u8, i32, u8)> = None;
        let mut lay = |(from, c0, to, c1): (i32, u8, i32, u8)| {
            let premul0 = premul_rgb(lerp_rgb_f(a.color, b.color, t(from)), c0);
            let premul1 = premul_rgb(lerp_rgb_f(a.color, b.color, t(to)), c1);
            if premul0 == 0 && premul1 == 0 {
                return true;
            }
            if out.laid() + 2 > max_quads {
                return false;
            }
            out.row(
                row_rel,
                row_rel.max(0) as u16,
                (from as u16, y as u16, (to - from) as u16),
                (premul0, alpha(c0)),
                (premul1, alpha(c1)),
            );
            true
        };
        let level = |(k, v): (i32, f32)| (k, (v + dither) as u8);
        let mut from = level(knots[0]);
        for &knot in &knots[1..n] {
            let (to, c) = level(knot);
            if to == from.0 {
                continue;
            }
            run = match run {
                Some((x, c0, _, c1)) if c0 == c1 && c == c1 => Some((x, c0, to, c)),
                Some(done) => {
                    if !lay(done) {
                        return false;
                    }
                    Some((from.0, from.1, to, c))
                }
                None => Some((from.0, from.1, to, c)),
            };
            from = (to, c);
        }
        if let Some(done) = run
            && !lay(done)
        {
            return false;
        }
    }
    true
}

/// The y-major twin of [`ribbon_beam`], its exact transpose.
#[allow(clippy::too_many_arguments)]
pub fn ribbon_beam_v(
    out: &mut Vec<GlowQuad>,
    clip: BeamClip,
    verts: &[RibbonVertex],
    shoulder: f32,
    step: usize,
    max_quads: usize,
    blend: GlowBlend,
) -> bool {
    let start = out.len();
    if ribbon_beam_v_ordered(out, clip, verts, shoulder, step, max_quads, blend, false) {
        return true;
    }
    if !verts.windows(2).any(|w| w[1].x < w[0].x) {
        return false;
    }
    out.truncate(start);
    ribbon_beam_v_ordered(out, clip, verts, shoulder, step, max_quads, blend, true)
}

#[allow(clippy::too_many_arguments)]
fn ribbon_beam_v_ordered(
    out: &mut Vec<GlowQuad>,
    clip: BeamClip,
    verts: &[RibbonVertex],
    shoulder: f32,
    step: usize,
    max_quads: usize,
    blend: GlowBlend,
    head_first_reversed: bool,
) -> bool {
    if verts.len() < 2 || clip.cell_h <= 0 {
        return true;
    }
    let step = step.clamp(1, i32::MAX as usize) as i32;
    for w in verts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let span = b.x - a.x;
        if !span.is_finite() || span.abs() < 1e-3 {
            continue;
        }
        let (start, end) = (a.x.min(b.x).ceil() as i32, a.x.max(b.x).ceil() as i32);
        let width = end - start;
        if width <= 0 {
            continue;
        }
        let slabs = (width - 1) / step + 1;
        let reverse = head_first_reversed && span < 0.0;
        let mut p = start + if reverse { (slabs - 1) * step } else { 0 };
        let advance = if reverse { -step } else { step };
        for i in 0..slabs {
            let len = step.min(end - p).max(1);
            let t = ((p as f32 + len as f32 * 0.5 - a.x) / span).clamp(0.0, 1.0);
            let lerp = |u: f32, v: f32| u + (v - u) * t;
            let spine = lerp(a.spine, b.spine);
            let up = lerp(a.up, b.up);
            let dn = lerp(a.dn, b.dn);
            let core_up = lerp(a.core_up, b.core_up);
            let core_dn = lerp(a.core_dn, b.core_dn);
            let cov = lerp(a.cov, b.cov);
            let lift = lerp(a.lift, b.lift).max(0.0);
            let lift_span = lerp(a.lift_span, b.lift_span);
            let left = spine - up;
            let right = spine + dn;
            if !left.is_nan() && !right.is_nan() && right > left && cov >= 1.0 {
                let color = lerp_rgb_f(a.color, b.color, t);
                let x0 = left.floor() as i32;
                let x1 = right.ceil() as i32;
                let dither_p = p.div_euclid(step).rem_euclid(4) as usize;
                let rect_y0 = p.max(clip.y0);
                let rect_y1 = (p + len).min(clip.y1);
                let bands = if rect_y1 > rect_y0 {
                    ((rect_y1 - 1 - clip.origin_y).div_euclid(clip.cell_h)
                        - (rect_y0 - clip.origin_y).div_euclid(clip.cell_h)
                        + 1) as usize
                } else {
                    1
                };
                for x in x0..x1 {
                    if out.len() + bands + 1 > max_quads {
                        return false;
                    }
                    let lo = (x as f32).max(left);
                    let hi = ((x + 1) as f32).min(right);
                    let cover = hi - lo;
                    if cover <= 0.0 {
                        continue;
                    }
                    let d = (lo + hi) * 0.5 - spine;
                    let across = if d < 0.0 {
                        ribbon_profile(d, core_up, up, shoulder)
                    } else {
                        ribbon_profile(d, core_dn, dn, 1.0)
                    };
                    let level = if lift == 0.0 {
                        cov * across
                    } else {
                        cov * across + lift * ribbon_lift_profile(d, lift_span, dn)
                    };
                    let c = (level * cover + BAYER4[x.rem_euclid(4) as usize][dither_p]) as u8;
                    if c > 0 {
                        let premul = premul_rgb(color, c);
                        if premul != 0 && rect_y1 > rect_y0 && x >= clip.x0 && x < clip.x1 {
                            let alpha = match blend {
                                GlowBlend::Add => 0,
                                GlowBlend::Over => c,
                            };
                            let mut yy = rect_y0;
                            while yy < rect_y1 {
                                let row_rel = (yy - clip.origin_y).div_euclid(clip.cell_h);
                                let band_end =
                                    (clip.origin_y + (row_rel + 1) * clip.cell_h).min(rect_y1);
                                out.push(GlowQuad {
                                    row: row_rel.max(0) as u16,
                                    x: x as u16,
                                    y: yy as u16,
                                    w: 1,
                                    h: (band_end - yy) as u16,
                                    color: premul,
                                    alpha,
                                    color2: premul,
                                    alpha2: alpha,
                                });
                                yy = band_end;
                            }
                        }
                    }
                }
            }
            if i + 1 < slabs {
                p += advance;
            }
        }
    }
    true
}

/// One sample along the cursor comet polyline (a swept cell centre).
#[derive(Clone, Copy, Debug)]
pub struct CometSample {
    /// Window-absolute x in pixels (sub-pixel).
    pub x: f32,
    /// Window-absolute y in pixels (sub-pixel).
    pub y: f32,
    /// Coverage 0..=255 at this sample (time-fade × intensity already applied).
    pub cov: u8,
    /// Path position, tail 0.0 → head 1.0.
    pub pos: f32,
}

const MIX_WHITE: u32 = 0x00FF_FFFF;

/// The comet bloom layers.
pub const COMET_LAYERS: [(f32, f32, u32, f32, f32, usize); 4] = [
    (5.5, 0.16, MIX_WHITE, 0.0, 0.0, 3),
    (3.0, 0.30, MIX_WHITE, 0.0, 0.0, 2),
    (1.8, 0.55, MIX_WHITE, 0.12, 0.0, 2),
    (1.0, 1.00, MIX_WHITE, 0.25, 0.45, 1),
];

/// The phaser bloom layers (same tuple shape as [`COMET_LAYERS`]).
pub const PHASER_LAYERS: [(f32, f32, u32, f32, f32, usize); 3] = [
    (1.0, 0.95, MIX_WHITE, 0.06, 0.0, 1),
    (1.45, 0.36, MIX_WHITE, 0.0, 0.0, 2),
    (2.0, 0.14, MIX_WHITE, 0.0, 0.0, 3),
];

/// The sparkle ribbon layers (same tuple shape as [`COMET_LAYERS`]).
pub const SPARKLE_LAYERS: [(f32, f32, u32, f32, f32, usize); 3] = [
    (1.0, 1.00, MIX_WHITE, 0.10, 0.0, 1),
    (1.45, 0.40, MIX_WHITE, 0.0, 0.0, 2),
    (2.0, 0.16, MIX_WHITE, 0.0, 0.0, 3),
];

/// The light a bloom-layer stack lays on its own axis per unit of sample coverage, per channel `[r, g, b]`.
#[must_use]
pub fn layer_stack_lift(
    layers: &[(f32, f32, u32, f32, f32, usize)],
    style: u32,
    pos: f32,
) -> [f32; 3] {
    let mut lift = [0.0f32; 3];
    for &(_, cmul, target, mbase, mpos, _) in layers {
        let color = if mbase > 0.0 || mpos > 0.0 {
            lerp_rgb_f(style, target, (mbase + mpos * pos).min(1.0))
        } else {
            style
        };
        for (c, sh) in lift.iter_mut().zip([16u32, 8, 0]) {
            *c += cmul * ((color >> sh) & 0xff) as f32 / 255.0;
        }
    }
    lift
}

/// The deep-space nebula haze wrapped around the photon beam's outer layers (`0x00RRGGBB`).
pub const BEAM_SPACE_HAZE: u32 = 0x005A_3BD6;

/// The beam tube layers (same tuple shape as [`COMET_LAYERS`]): a white-hot core in an ice
/// rod, sleeved in a faint haze no wider than two and a half rods, so the tube keeps to its
/// own row. Its sleeve once reached three and five and a half rods: at a full-power rod a third
/// of a cell thick, the outer one stood a row and three quarters tall, and over a Backspace's
/// erased cells the tube lay as a white slab between two nested lavender boxes over the
/// letters of the rows above and below (composed r3's judges; the after-land lows' W6).
pub const BEAM_LAYERS: [(f32, f32, u32, f32, f32, usize); 4] = [
    (0.55, 1.00, MIX_WHITE, 0.55, 0.30, 1),
    (1.0, 0.95, MIX_WHITE, 0.20, 0.08, 1),
    (1.6, 0.58, BEAM_SPACE_HAZE, 0.18, 0.0, 1),
    (2.5, 0.24, BEAM_SPACE_HAZE, 0.55, 0.0, 2),
];

/// The laser filament layers (same tuple shape as [`COMET_LAYERS`]).
pub const LASER_LAYERS: [(f32, f32, u32, f32, f32, usize); 3] = [
    (2.9, 0.60, MIX_WHITE, 0.0, 0.0, 1),
    (1.5, 0.95, MIX_WHITE, 0.10, 0.0, 1),
    (0.6, 1.00, MIX_WHITE, 0.26, 0.42, 1),
];

/// Emit the full layered-bloom comet beam for one contiguous run of swept-cell samples (broad aura under a white-hot core) into per-cell-row [`GlowQuad`]s.
#[inline]
pub fn comet_glow_quads<S: BeamRows + ?Sized>(
    out: &mut S,
    clip: BeamClip,
    run: &[CometSample],
    core_px: f32,
    straighten: f32,
    color_at: &dyn Fn(f32) -> u32,
) {
    layered_beam_quads(out, clip, run, core_px, straighten, color_at, &COMET_LAYERS);
}

/// Emit the phaser streak ([`PHASER_LAYERS`]: a near-cell-height band of spectrum under a tight halo), as [`comet_glow_quads`] does the comet.
#[inline]
pub fn phaser_streak_quads<S: BeamRows + ?Sized>(
    out: &mut S,
    clip: BeamClip,
    run: &[CometSample],
    core_px: f32,
    straighten: f32,
    color_at: &dyn Fn(f32) -> u32,
) {
    layered_beam_quads(
        out,
        clip,
        run,
        core_px,
        straighten,
        color_at,
        &PHASER_LAYERS,
    );
}

/// Emit the beam tube ([`BEAM_LAYERS`]: a solid luminous rod in a deep-space sleeve), as [`comet_glow_quads`] does the comet.
#[inline]
pub fn beam_glow_quads<S: BeamRows + ?Sized>(
    out: &mut S,
    clip: BeamClip,
    run: &[CometSample],
    core_px: f32,
    straighten: f32,
    color_at: &dyn Fn(f32) -> u32,
) {
    layered_beam_quads(out, clip, run, core_px, straighten, color_at, &BEAM_LAYERS);
}

/// Shared body of [`comet_glow_quads`] / [`phaser_streak_quads`] / [`beam_glow_quads`], without the per-call scratch they build.
///
/// `feather` rounds each layer's two ends off: a layer fades from nothing at each end of the
/// run to its own light over `feather` of its half-thickness (`0`: flat ends, every layer
/// whole to the run's last pixel). A wide sleeve laid whole to its ends stands as a box, its
/// ends hard edges as tall as the sleeve is thick (Beam's haze, the after-land lows' W6).
#[allow(clippy::too_many_arguments)]
pub fn layered_beam_quads_with<S: BeamRows + ?Sized>(
    scratch: &mut BeamScratch,
    out: &mut S,
    clip: BeamClip,
    run: &[CometSample],
    (core_px, feather): (f32, f32),
    straighten: f32,
    color_at: &dyn Fn(f32) -> u32,
    layers: &[(f32, f32, u32, f32, f32, usize)],
) {
    if run.len() < 2 {
        return;
    }
    scratch.simplify(run, straighten);
    let BeamScratch {
        styles,
        kept,
        verts,
        feathered,
        ..
    } = scratch;
    styles.clear();
    styles.extend(kept.iter().map(|&i| color_at(run[i].pos)));
    for &(tmul, cmul, target, mbase, mpos, lstep) in layers {
        verts.clear();
        for (&i, &style) in kept.iter().zip(styles.iter()) {
            let s = &run[i];
            let color = if mbase > 0.0 || mpos > 0.0 {
                lerp_rgb_f(style, target, (mbase + mpos * s.pos).min(1.0))
            } else {
                style
            };
            verts.push(BeamVertex {
                x: s.x,
                y: s.y,
                color,
                cov: (s.cov as f32 * cmul) as u8,
            });
        }
        let thickness = core_px * tmul;
        if feather > 0.0 && feather_ends(verts, feathered, feather * 0.5 * thickness) {
            comet_beam(out, clip, feathered, thickness, lstep, 0.0);
        } else {
            comet_beam(out, clip, verts, thickness, lstep, 0.0);
        }
    }
}

/// `verts` with each end faded from nothing over `span` px of its length (or half the
/// polyline's length, if shorter), into `out`: a vertex at each end at no light, and one
/// `span` in from it at the light the polyline lays there. `false` (and `out` untouched) for a
/// polyline with no length.
fn feather_ends(verts: &[BeamVertex], out: &mut Vec<BeamVertex>, span: f32) -> bool {
    let seg = |a: &BeamVertex, b: &BeamVertex| (b.x - a.x).hypot(b.y - a.y);
    let total: f32 = verts.windows(2).map(|w| seg(&w[0], &w[1])).sum();
    if verts.len() < 2 || !total.is_finite() || total <= 1e-3 {
        return false;
    }
    let d = span.min(total * 0.5).max(0.0);
    // The vertex `at` px along the polyline.
    let at = |at: f32| -> BeamVertex {
        let mut walked = 0.0f32;
        for w in verts.windows(2) {
            let l = seg(&w[0], &w[1]);
            if walked + l >= at && l > 0.0 {
                let t = ((at - walked) / l).clamp(0.0, 1.0);
                let (a, b) = (w[0], w[1]);
                return BeamVertex {
                    x: a.x + (b.x - a.x) * t,
                    y: a.y + (b.y - a.y) * t,
                    color: lerp_rgb_f(a.color, b.color, t),
                    cov: (f32::from(a.cov) + (f32::from(b.cov) - f32::from(a.cov)) * t) as u8,
                };
            }
            walked += l;
        }
        verts[verts.len() - 1]
    };
    out.clear();
    out.push(BeamVertex { cov: 0, ..verts[0] });
    out.push(at(d));
    let mut walked = 0.0f32;
    for w in verts.windows(2) {
        walked += seg(&w[0], &w[1]);
        if walked > d && walked < total - d {
            out.push(w[1]);
        }
    }
    out.push(at(total - d));
    out.push(BeamVertex {
        cov: 0,
        ..verts[verts.len() - 1]
    });
    true
}

#[inline]
fn layered_beam_quads<S: BeamRows + ?Sized>(
    out: &mut S,
    clip: BeamClip,
    run: &[CometSample],
    core_px: f32,
    straighten: f32,
    color_at: &dyn Fn(f32) -> u32,
    layers: &[(f32, f32, u32, f32, f32, usize)],
) {
    layered_beam_quads_with(
        &mut BeamScratch::default(),
        out,
        clip,
        run,
        (core_px, 0.0),
        straighten,
        color_at,
        layers,
    );
}

#[inline]
fn blend(bg: u32, fg: u32, t: u8) -> u32 {
    if t == 0 {
        return bg;
    }
    if t == 255 {
        return fg & 0x00ff_ffff;
    }
    let a = t as f32 / 255.0;
    let ch = |sh: u32| blend_channel_linear((bg >> sh) & 0xff, (fg >> sh) & 0xff, a);
    let tt = if bg >> 24 == 0 {
        0
    } else {
        ((bg >> 24) as f32 * (1.0 - a)).round() as u32
    };
    (tt << 24) | (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

const IMAGE_MAX_DIMENSION: u32 = 4096;

/// Decode an 8-bit PNG to packed RGBA8, `(rgba, width, height)`, through the one hardened decoder ([`aterm_png::decode_rgba8`]) under the inline-image cap `IMAGE_MAX_DIMENSION`.
#[inline]
#[must_use]
pub fn decode_png_rgba8(bytes: &[u8]) -> Option<(Vec<u8>, usize, usize)> {
    aterm_png::decode_rgba8(bytes, IMAGE_MAX_DIMENSION)
}

/// Decode an inline-image payload (iTerm2 OSC 1337 / sixel / Kitty) to RGBA8 filling the footprint pixel box `fp_w × fp_h`.
#[inline]
#[must_use]
pub fn decode_image_to_footprint(
    bytes: &[u8],
    format: aterm_core::grid::extra::ImageFormat,
    fp_w: usize,
    fp_h: usize,
    scaling: aterm_core::grid::extra::ImageScaling,
    source_rect: Option<aterm_core::grid::extra::SourceRect>,
) -> Option<Vec<u8>> {
    decode_image_to_raster(
        bytes,
        format,
        (fp_w, fp_h),
        (fp_w, fp_h),
        scaling,
        source_rect,
    )
}

fn decode_image_to_raster(
    bytes: &[u8],
    format: aterm_core::grid::extra::ImageFormat,
    footprint: (usize, usize),
    raster: (usize, usize),
    scaling: aterm_core::grid::extra::ImageScaling,
    source_rect: Option<aterm_core::grid::extra::SourceRect>,
) -> Option<Vec<u8>> {
    use aterm_core::grid::extra::{ImageFormat, ImageScaling};
    use std::borrow::Cow;
    let ((fp_w, fp_h), (box_w, box_h)) = (footprint, raster);
    if fp_w == 0 || fp_h == 0 || box_w == 0 || box_h == 0 {
        return None;
    }
    let (src, src_w, src_h): (Cow<'_, [u8]>, usize, usize) = match format {
        ImageFormat::RawRgba8 { width, height } => {
            let (w, h) = (width as usize, height as usize);
            if w == 0 || h == 0 || bytes.len() < w.checked_mul(h)?.checked_mul(4)? {
                return None;
            }
            (Cow::Borrowed(&bytes[..w * h * 4]), w, h)
        }
        ImageFormat::Png => {
            let (decoded, w, h) = decode_png_rgba8(bytes)?;
            (Cow::Owned(decoded), w, h)
        }
        ImageFormat::Unknown => return None,
    };
    let (src, src_w, src_h) = match source_rect {
        None => (src, src_w, src_h),
        Some(rect) => {
            let (x, y, w, h) =
                rect.clamp_to(u32::try_from(src_w).ok()?, u32::try_from(src_h).ok()?)?;
            let (w, h) = (w as usize, h as usize);
            let cropped = crop_rgba(&src, src_w, x as usize, y as usize, w, h);
            (Cow::Owned(cropped), w, h)
        }
    };
    let unscaled = (box_w, box_h) == (fp_w, fp_h);
    if (src_w, src_h) == (box_w, box_h)
        && (unscaled || scaling != ImageScaling::PixelExact)
        && let Cow::Owned(owned) = src
    {
        return Some(owned);
    }
    Some(match scaling {
        ImageScaling::Fit => fit_rgba_into_footprint(&src, src_w, src_h, box_w, box_h),
        ImageScaling::Stretch => resample_rgba(&src, src_w, src_h, box_w, box_h),
        ImageScaling::PixelExact if unscaled => {
            place_rgba_at_origin(&src, src_w, src_h, fp_w, fp_h)
        }
        ImageScaling::PixelExact => {
            let shrink = |n: usize, to: usize, of: usize| {
                usize::try_from((n as u64 * to as u64).div_ceil(of as u64))
                    .unwrap_or(usize::MAX)
                    .max(1)
            };
            let (sw, sh) = (shrink(src_w, box_w, fp_w), shrink(src_h, box_h, fp_h));
            let small = resample_rgba(&src, src_w, src_h, sw, sh);
            place_rgba_at_origin(&small, sw, sh, box_w, box_h)
        }
    })
}

/// The most pixels an inline-image placement is decoded to, by either renderer.
pub const MAX_PLACEMENT_RASTER_PX: usize = 8192 * 4096;

/// The decoded image bytes either renderer keeps for the images one frame draws.
pub const MAX_IMAGE_WORKING_SET_BYTES: usize = 2 * MAX_PLACEMENT_RASTER_PX * 4;

/// The longest side a placement raster has.
pub const MAX_PLACEMENT_RASTER_SIDE: usize = 16_384;

/// The `(w, h)` a `fp_w × fp_h` footprint's raster is decoded at.
#[inline]
#[must_use]
pub fn placement_raster_size(fp_w: usize, fp_h: usize) -> (usize, usize) {
    raster_size_at_most(fp_w, fp_h, MAX_PLACEMENT_RASTER_PX)
}

/// [`placement_raster_size`] held to at most `cap_px` pixels too (one frame's [`frame_raster_cap`]).
#[inline]
#[must_use]
pub fn placement_raster_size_within(fp_w: usize, fp_h: usize, cap_px: usize) -> (usize, usize) {
    let full = placement_raster_size(fp_w, fp_h);
    if full.0.saturating_mul(full.1) <= cap_px {
        full
    } else {
        raster_size_at_most(fp_w, fp_h, cap_px.clamp(1, MAX_PLACEMENT_RASTER_PX))
    }
}

fn raster_size_at_most(fp_w: usize, fp_h: usize, px: usize) -> (usize, usize) {
    let px = px as u128;
    const SIDE: u128 = MAX_PLACEMENT_RASTER_SIDE as u128;
    let (w, h) = (fp_w as u128, fp_h as u128);
    if w == 0 || h == 0 || (w <= SIDE && h <= SIDE && w * h <= px) {
        return (fp_w, fp_h);
    }
    let (mut rw, mut rh) = if w * h > px {
        ((px * w / h).isqrt(), (px * h / w).isqrt())
    } else {
        (w, h)
    };
    if rw > SIDE {
        rh = rh * SIDE / rw;
        rw = SIDE;
    }
    if rh > SIDE {
        rw = rw * SIDE / rh;
        rh = SIDE;
    }
    let (mut rw, mut rh) = (rw.clamp(1, w), rh.clamp(1, h));
    if rw * rh > px {
        if rh == 1 {
            rw = px;
        } else {
            rh = px;
        }
    }
    (rw as usize, rh as usize)
}

/// The most pixels each of one frame's placements is decoded to, so their rasters together fit [`MAX_IMAGE_WORKING_SET_BYTES`].
#[must_use]
pub fn frame_raster_cap(demands: &mut [usize]) -> usize {
    const BUDGET: usize = MAX_IMAGE_WORKING_SET_BYTES / 4;
    let total = demands.iter().fold(0usize, |sum, &d| sum.saturating_add(d));
    if total <= BUDGET {
        return MAX_PLACEMENT_RASTER_PX;
    }
    demands.sort_unstable();
    let mut left = BUDGET;
    for (i, &demand) in demands.iter().enumerate() {
        let share = left / (demands.len() - i);
        if demand > share {
            return share.max(1);
        }
        left -= demand;
    }
    MAX_PLACEMENT_RASTER_PX
}

/// A placement's footprint in device px at `cell` px cells.
#[inline]
#[must_use]
pub fn placement_footprint(
    image: &aterm_core::grid::extra::ImageData,
    (cell_w, cell_h): (usize, usize),
) -> (usize, usize) {
    (
        usize::from(image.cols) * cell_w,
        usize::from(image.rows) * cell_h + usize::from(image.band_lift_px),
    )
}

#[inline]
fn placement_raster_coord(f: usize, fp: usize, raster: usize) -> usize {
    let at = (2 * f as u64 + 1) * raster as u64 / (2 * fp.max(1) as u64);
    usize::try_from(at).unwrap_or(usize::MAX)
}

/// One placement decoded for drawing.
pub struct PlacementRaster {
    /// Raster width in pixels.
    pub w: usize,
    /// Raster height in pixels.
    pub h: usize,
    /// `w * h * 4` straight-alpha RGBA bytes, or empty.
    pub rgba: Vec<u8>,
}

/// Decode `image` for its `fp_w × fp_h` footprint box exactly as both renderers draw it.
#[inline]
#[must_use]
pub fn decode_placement(
    image: &aterm_core::grid::extra::ImageData,
    fp_w: usize,
    fp_h: usize,
) -> Option<PlacementRaster> {
    decode_placement_within(image, fp_w, fp_h, MAX_PLACEMENT_RASTER_PX)
}

/// [`decode_placement`] to a raster of at most `cap_px` pixels ([`placement_raster_size_within`]).
#[must_use]
pub fn decode_placement_within(
    image: &aterm_core::grid::extra::ImageData,
    fp_w: usize,
    fp_h: usize,
    cap_px: usize,
) -> Option<PlacementRaster> {
    let (w, h) = placement_raster_size_within(fp_w, fp_h, cap_px);
    let decode = |footprint, raster| {
        decode_image_to_raster(
            &image.bytes,
            image.format,
            footprint,
            raster,
            image.scaling,
            image.source_rect,
        )
    };
    let (ox, oy) = image.cell_offset_px;
    let (ox, oy) = (usize::from(ox), usize::from(oy));
    if (ox, oy) == (0, 0) {
        let rgba = decode((fp_w, fp_h), (w, h))?;
        return Some(PlacementRaster { w, h, rgba });
    }
    if ox >= fp_w || oy >= fp_h {
        return None;
    }
    let (rx, ry) = (
        placement_raster_coord(ox, fp_w, w),
        placement_raster_coord(oy, fp_h, h),
    );
    let (area_w, area_h) = (w - rx, h - ry);
    let area = decode((fp_w - ox, fp_h - oy), (area_w, area_h))?;
    let mut rgba = vec![0u8; w * h * 4];
    let row_bytes = area_w * 4;
    for (y, row) in area.chunks_exact(row_bytes).enumerate() {
        let d = ((ry + y) * w + rx) * 4;
        rgba[d..d + row_bytes].copy_from_slice(row);
    }
    Some(PlacementRaster { w, h, rgba })
}

fn crop_rgba(src: &[u8], src_w: usize, x: usize, y: usize, w: usize, h: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(w * h * 4);
    for row in y..y + h {
        let start = (row * src_w + x) * 4;
        out.extend_from_slice(&src[start..start + w * 4]);
    }
    out
}

fn place_rgba_at_origin(
    src: &[u8],
    src_w: usize,
    src_h: usize,
    fp_w: usize,
    fp_h: usize,
) -> Vec<u8> {
    if (src_w, src_h) == (fp_w, fp_h) {
        return src.to_vec();
    }
    let mut out = vec![0u8; fp_w * fp_h * 4];
    let copy_w = src_w.min(fp_w);
    let copy_h = src_h.min(fp_h);
    let row_bytes = copy_w * 4;
    for y in 0..copy_h {
        let s = y * src_w * 4;
        let d = y * fp_w * 4;
        out[d..d + row_bytes].copy_from_slice(&src[s..s + row_bytes]);
    }
    out
}

/// The largest `w × h` with `src`'s aspect ratio that fits inside `fp_w × fp_h`.
#[doc(hidden)]
#[inline]
#[must_use]
pub fn aspect_fit_box(src_w: usize, src_h: usize, fp_w: usize, fp_h: usize) -> (usize, usize) {
    let (sw, sh) = (src_w as u64, src_h as u64);
    let (bw, bh) = (fp_w as u64, fp_h as u64);
    if sw == 0 || sh == 0 {
        return (fp_w, fp_h);
    }
    let (w, h) = match (sw * bh).cmp(&(sh * bw)) {
        std::cmp::Ordering::Equal => (bw, bh),
        std::cmp::Ordering::Greater => (bw, ((sh * bw) + sw / 2) / sw),
        std::cmp::Ordering::Less => (((sw * bh) + sh / 2) / sh, bh),
    };
    ((w.max(1) as usize).min(fp_w), (h.max(1) as usize).min(fp_h))
}

fn fit_rgba_into_footprint(
    src: &[u8],
    src_w: usize,
    src_h: usize,
    fp_w: usize,
    fp_h: usize,
) -> Vec<u8> {
    let (dst_w, dst_h) = aspect_fit_box(src_w, src_h, fp_w, fp_h);
    let mut out = vec![0u8; fp_w * fp_h * 4];
    let off_x = (fp_w - dst_w) / 2;
    let off_y = (fp_h - dst_h) / 2;
    resample_rgba_into(
        src,
        src_w,
        src_h,
        &mut out,
        fp_w,
        (off_x, off_y),
        dst_w,
        dst_h,
    );
    out
}

/// Which bitmap strike (ppem) to rasterize from, given the strikes that carry the glyph and the target box size.
#[doc(hidden)]
#[inline]
#[must_use]
pub fn select_strike_ppem(strikes: &[u16], target: u16) -> Option<u16> {
    strikes
        .iter()
        .copied()
        .filter(|&s| s >= target)
        .min()
        .or_else(|| strikes.iter().copied().max())
}

/// The integer overlap, in units of `1/dst`, of source texel `s` (`[s, s+1)` source px) with the box-filter footprint of destination texel `d` (`[d·src/dst, (d+1)·src/dst)`), so the weights are exact rationals over `dst`.
#[doc(hidden)]
#[inline]
#[must_use]
pub fn area_overlap(d: usize, s: usize, src: usize, dst: usize) -> u64 {
    let (d, s, src, dst) = (d as u64, s as u64, src as u64, dst as u64);
    let lo = (d * src).max(s * dst);
    let hi = ((d + 1) * src).min((s + 1) * dst);
    hi.saturating_sub(lo)
}

fn pick_glyph_raster<'a>(
    face: &aterm_font::Face<'a>,
    gid: aterm_font::GlyphId,
    target_px: usize,
) -> Option<aterm_font::RasterGlyphImage<'a>> {
    let target = u16::try_from(target_px).unwrap_or(u16::MAX);
    let tables = face.tables();
    if let Some(sbix) = tables.sbix {
        let carrying: Vec<u16> = sbix
            .strikes
            .into_iter()
            .filter(|s| s.get(gid).is_some())
            .map(|s| s.pixels_per_em)
            .collect();
        let want = select_strike_ppem(&carrying, target)?;
        return sbix
            .strikes
            .into_iter()
            .filter(|s| s.pixels_per_em == want)
            .find_map(|s| s.get(gid));
    }
    face.glyph_raster_image(gid, target)
        .or_else(|| face.glyph_raster_image(gid, u16::MAX))
}

#[cfg(any(not(target_os = "macos"), test))]
fn shape_colour_cluster_opentype(bytes: &[u8], cluster: &str) -> Option<Vec<(u32, i32, i32, i32)>> {
    let face = aterm_font::shape::ShapeFace::from_slice(bytes, 0)?;
    let mut buffer = aterm_font::shape::UnicodeBuffer::new();
    buffer.push_str(cluster);
    let shaped = aterm_font::shape::shape(&face, &[], buffer);
    Some(
        shaped
            .glyph_infos()
            .iter()
            .zip(shaped.glyph_positions())
            .map(|(i, p)| (i.glyph_id, p.x_advance, p.x_offset, p.y_offset))
            .collect(),
    )
}

fn shape_colour_cluster(
    bytes: &crate::font::FaceBytes,
    cluster: &str,
) -> Option<Vec<(u32, i32, i32, i32)>> {
    #[cfg(target_os = "macos")]
    {
        let shaped = macos_coretext::shape_emoji(bytes, cluster)?;
        Some(
            shaped
                .iter()
                .map(|g| (u32::from(g.gid), g.x_advance, g.x_offset, g.y_offset))
                .collect(),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        shape_colour_cluster_opentype(bytes, cluster)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ColourCluster {
    /// One colour glyph: the face ligated the whole cluster.
    Glyph(u16),
    Run(emoji_cluster::ClusterRun),
}

fn colour_cluster_decision(
    face: &aterm_font::Face<'_>,
    cluster: &str,
    glyphs: &[(u32, i32, i32, i32)],
) -> Option<ColourCluster> {
    let drawable = |gid: u16| colour_drawable(face, aterm_font::GlyphId(gid));
    if let [(gid, ..)] = glyphs[..] {
        let gid = u16::try_from(gid).ok()?;
        return drawable(gid).then_some(ColourCluster::Glyph(gid));
    }
    if aterm_grapheme::str_grid_columns(cluster) != 2 {
        return None;
    }
    let marks = glyphs
        .iter()
        .filter_map(|&(gid, ..)| u16::try_from(gid).ok())
        .filter(|&gid| drawable(gid))
        .all(|gid| colr_only(face, aterm_font::GlyphId(gid)));
    emoji_cluster::layout_run(glyphs.iter().copied(), drawable, marks).map(ColourCluster::Run)
}

#[inline]
fn colr_only(face: &aterm_font::Face<'_>, gid: aterm_font::GlyphId) -> bool {
    face.is_color_glyph(gid) && face.glyph_raster_image(gid, u16::MAX).is_none()
}

#[inline]
fn colour_drawable(face: &aterm_font::Face<'_>, gid: aterm_font::GlyphId) -> bool {
    gid.0 != 0 && (face.glyph_raster_image(gid, u16::MAX).is_some() || face.is_color_glyph(gid))
}

fn fit_color_glyph(
    face: &aterm_font::Face<'_>,
    gid: aterm_font::GlyphId,
    box_w: usize,
    box_h: usize,
) -> Option<(Vec<u8>, usize, usize)> {
    let Some(raster) = pick_glyph_raster(face, gid, box_h)
        .filter(|r| matches!(r.format, aterm_font::RasterImageFormat::PNG))
    else {
        let rgba = crate::colr::rasterize_colr(face, gid, box_w, box_h)?;
        return Some((rgba, box_w, box_h));
    };
    let (src, src_w, src_h) = decode_png_rgba8(raster.data)?;
    let scale = (box_w as f32 / src_w as f32).min(box_h as f32 / src_h as f32);
    let dst_w = ((src_w as f32 * scale).round() as usize).clamp(1, box_w);
    let dst_h = ((src_h as f32 * scale).round() as usize).clamp(1, box_h);
    Some((
        resample_rgba(&src, src_w, src_h, dst_w, dst_h),
        dst_w,
        dst_h,
    ))
}

/// Resample a packed straight-alpha sRGB RGBA8 image to `dw`x`dh` through a premultiplied linear-light bracket.
#[doc(hidden)]
#[must_use]
pub fn resample_rgba(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    let mut out = vec![0u8; dw * dh * 4];
    resample_rgba_into(src, sw, sh, &mut out, dw, (0, 0), dw, dh);
    out
}

#[allow(clippy::too_many_arguments)]
fn resample_rgba_into(
    src: &[u8],
    sw: usize,
    sh: usize,
    out: &mut [u8],
    out_w: usize,
    at: (usize, usize),
    dw: usize,
    dh: usize,
) {
    if dw == 0 || dh == 0 {
        return;
    }
    let (ox, oy) = at;
    let row_out = |dy: usize| {
        let d = ((oy + dy) * out_w + ox) * 4;
        d..d + dw * 4
    };
    if sw == 0 || sh == 0 {
        for dy in 0..dh {
            out[row_out(dy)].fill(0);
        }
        return;
    }
    if (sw, sh) == (dw, dh) {
        for (dy, line) in src[..dw * dh * 4].chunks_exact(dw * 4).enumerate() {
            for (o, px) in out[row_out(dy)]
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(line.as_chunks::<4>().0)
            {
                *o = if px[3] != 0 { *px } else { [0; 4] };
            }
        }
        return;
    }
    let area = dw * 2 <= sw || dh * 2 <= sh;
    let mut rows = PremulRows::new(src, sw);
    let mut filtered = vec![0f32; dw * 4];
    let spans: Vec<(usize, usize)> = if area {
        (0..dw)
            .map(|dx| (dx * sw / dw, ((dx + 1) * sw - 1) / dw))
            .collect()
    } else {
        Vec::new()
    };
    for dy in 0..dh {
        if area {
            area_row_premul(&mut rows, &spans, (sw, sh), (dw, dh), dy, &mut filtered);
        } else {
            bilinear_row_premul(&mut rows, (sw, sh), (dw, dh), dy, &mut filtered);
        }
        for (o, px) in out[row_out(dy)]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(filtered.as_chunks::<4>().0)
        {
            let a = px[3];
            if a <= 0.0 {
                *o = [0; 4];
                continue;
            }
            let enc = |l: f32| {
                let idx = ((l / a).clamp(0.0, 1.0) * LIN2SRGB_N as f32).round() as usize;
                LINEAR_TO_SRGB_LUT[idx]
            };
            *o = [
                enc(px[0]),
                enc(px[1]),
                enc(px[2]),
                (a * 255.0).round().clamp(0.0, 255.0) as u8,
            ];
        }
    }
}

struct PremulRows<'a> {
    src: &'a [u8],
    sw: usize,
    slots: [(Option<usize>, Vec<f32>); 2],
}

impl<'a> PremulRows<'a> {
    fn new(src: &'a [u8], sw: usize) -> Self {
        let slot = || (None, Vec::with_capacity(sw * 4));
        Self {
            src,
            sw,
            slots: [slot(), slot()],
        }
    }

    fn load(&mut self, y: usize, keep: Option<usize>) -> usize {
        if let Some(i) = self.slots.iter().position(|(row, _)| *row == Some(y)) {
            return i;
        }
        let (r0, r1) = (self.slots[0].0, self.slots[1].0);
        let i = if keep.is_some() && r0 == keep {
            1
        } else if keep.is_some() && r1 == keep {
            0
        } else {
            usize::from(r1 < r0)
        };
        let line = &self.src[y * self.sw * 4..(y + 1) * self.sw * 4];
        let (row, pm) = &mut self.slots[i];
        *row = Some(y);
        pm.clear();
        for px in line.as_chunks::<4>().0 {
            let a = f32::from(px[3]) / 255.0;
            pm.push(SRGB_TO_LINEAR_LUT[px[0] as usize] * a);
            pm.push(SRGB_TO_LINEAR_LUT[px[1] as usize] * a);
            pm.push(SRGB_TO_LINEAR_LUT[px[2] as usize] * a);
            pm.push(a);
        }
        i
    }

    #[inline]
    fn row(&self, i: usize) -> &[f32] {
        &self.slots[i].1
    }
}

fn bilinear_row_premul(
    rows: &mut PremulRows<'_>,
    (sw, sh): (usize, usize),
    (dw, dh): (usize, usize),
    dy: usize,
    out: &mut [f32],
) {
    let fx = sw as f32 / dw as f32;
    let fy = sh as f32 / dh as f32;
    let sy = ((dy as f32 + 0.5) * fy - 0.5).max(0.0);
    let y0 = sy.floor() as usize;
    let y1 = (y0 + 1).min(sh - 1);
    let wy = sy - y0 as f32;
    let i0 = rows.load(y0, Some(y1));
    let i1 = rows.load(y1, Some(y0));
    let (r0, r1) = (rows.row(i0), rows.row(i1));
    for dx in 0..dw {
        let sx = ((dx as f32 + 0.5) * fx - 0.5).max(0.0);
        let x0 = sx.floor() as usize;
        let x1 = (x0 + 1).min(sw - 1);
        let wx = sx - x0 as f32;
        let di = dx * 4;
        for c in 0..4 {
            let p00 = r0[x0 * 4 + c];
            let p10 = r0[x1 * 4 + c];
            let p01 = r1[x0 * 4 + c];
            let p11 = r1[x1 * 4 + c];
            let top = p00 + (p10 - p00) * wx;
            let bot = p01 + (p11 - p01) * wx;
            out[di + c] = top + (bot - top) * wy;
        }
    }
}

fn area_row_premul(
    rows: &mut PremulRows<'_>,
    spans: &[(usize, usize)],
    (sw, sh): (usize, usize),
    (dw, dh): (usize, usize),
    dy: usize,
    out: &mut [f32],
) {
    let norm = 1.0 / (sw as f32 * sh as f32);
    let y0 = dy * sh / dh;
    let y1 = ((dy + 1) * sh - 1) / dh;
    out.fill(0.0);
    for y in y0..=y1 {
        let wy = area_overlap(dy, y, sh, dh) as f32;
        let i = rows.load(y, None);
        let line = rows.row(i);
        for (dx, &(x0, x1)) in spans.iter().enumerate() {
            let acc = &mut out[dx * 4..dx * 4 + 4];
            for x in x0..=x1 {
                let w = wy * area_overlap(dx, x, sw, dw) as f32;
                let si = x * 4;
                for (c, a) in acc.iter_mut().enumerate() {
                    *a += line[si + c] * w;
                }
            }
        }
    }
    for a in out.iter_mut() {
        *a *= norm;
    }
}

impl Rasterizer for Renderer {
    #[inline]
    fn cell_size(&self) -> (usize, usize) {
        Renderer::cell_size(self)
    }
    #[inline]
    fn render_input(&mut self, input: &RenderInput) -> Frame {
        Renderer::render_input(self, input)
    }
    #[inline]
    fn set_cursor_blink_phase(&mut self, on: bool) {
        Renderer::set_cursor_blink_phase(self, on)
    }
    #[inline]
    fn set_cursor_style_override(&mut self, style: Option<CursorStyle>) {
        Renderer::set_cursor_style_override(self, style)
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "lib_ribbon_beam_tests.rs"]
mod ribbon_beam_tests;

#[cfg(test)]
#[path = "lib_comet_beam_tests.rs"]
mod comet_beam_tests;

#[cfg(test)]
#[path = "lib_subpixel_seat_tests.rs"]
mod subpixel_seat_tests;

#[cfg(all(test, feature = "embedded-font"))]
#[path = "lib_glyph_cache_accounting_tests.rs"]
mod glyph_cache_accounting_tests;

#[cfg(all(test, feature = "embedded-font"))]
#[path = "lib_prewarm_tests.rs"]
mod prewarm_tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "lib_ct_nocopy_tests.rs"]
mod ct_nocopy_tests;

#[cfg(test)]
#[path = "../tests/font_oracle/corpus.rs"]
#[allow(dead_code)]
mod oracle_corpus;

#[cfg(test)]
#[path = "lib_emoji_oracle_tests.rs"]
mod emoji_oracle;

/// Creates a `.noindex` scratch directory for tests with a short UTF-8-safe prefix.
#[cfg(test)]
pub(crate) fn test_scratch_dir(tag: &str) -> aterm_tempfile::TempDir {
    let end = tag.floor_char_boundary(24);
    let prefix = format!("aterm-render-{}-", &tag[..end]);
    aterm_tempfile::Builder::new()
        .prefix(&prefix)
        .suffix(".noindex")
        .tempdir()
        .expect("create .noindex scratch dir")
}

#[cfg(test)]
#[path = "lib_test_scratch_tests.rs"]
mod test_scratch_tests;
