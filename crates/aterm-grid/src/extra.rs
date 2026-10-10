// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Cell extras for rarely-used attributes.
//!
//! ## Design
//!
//! The packed 8-byte `Cell` handles common attributes. Rare features such as
//! hyperlinks and combining characters live in an external lookup table
//! (`CellExtras`) so they do not bloat every cell.
//!
//! ## Storage Strategy
//!
//! ```text
//! Cell (8 bytes)           CellExtras (HashMap)
//! ┌────────────────┐       ┌──────────────────────────────┐
//! │ codepoint+flags│       │ (row, col) -> CellExtra      │
//! │ fg color       │──────▶│   - hyperlink: Option<Arc>   │
//! │ bg color       │       │   - underline_color: Option  │
//! └────────────────┘       │   - combining: SmallVec      │
//!                          └──────────────────────────────┘
//! ```
//!
//! `CellExtra` packs its RGB colors: bitflags record which fields are present
//! (no Option discriminants) and three RGB colors share one 9-byte array.
//!
//! ## Usage
//!
//! - Hyperlinks: OSC 8 sequences set hyperlinks on cells
//! - Combining characters: Unicode combining marks (U+0300-U+036F, etc.)
//! - Underline colors: SGR 58/59 for colored underlines

use aterm_alloc::SmallVec;
use std::sync::Arc;

// Re-export CellExtras from sibling module so existing import paths work.
pub use crate::extra_collection::CellExtras;

/// Coordinate for cell extras lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellCoord {
    /// Row index (0-indexed from top of grid).
    pub row: u16,
    /// Column index (0-indexed).
    pub col: u16,
}

impl CellCoord {
    /// Create a new cell coordinate.
    #[must_use]
    #[inline]
    pub const fn new(row: u16, col: u16) -> Self {
        Self { row, col }
    }
}

/// Bitflags for CellExtra presence tracking.
///
/// These flags indicate which optional fields have values set.
/// Using bitflags avoids the overhead of multiple Option discriminants.
mod extra_flags {
    /// Underline color is present (bytes 0-2 of colors array).
    pub(crate) const HAS_UNDERLINE_COLOR: u16 = 1 << 0;
    /// Foreground RGB is present (bytes 3-5 of colors array).
    pub(crate) const HAS_FG_RGB: u16 = 1 << 1;
    /// Background RGB is present (bytes 6-8 of colors array).
    pub(crate) const HAS_BG_RGB: u16 = 1 << 2;
    /// Mask for color presence flags.
    pub(crate) const COLOR_MASK: u16 = HAS_UNDERLINE_COLOR | HAS_FG_RGB | HAS_BG_RGB;
    /// Bits 3-15 are available for extended flags.
    pub(crate) const EXTENDED_SHIFT: u32 = 3;
}

/// Hyperlink data: URL and optional ID from OSC 8 sequences.
///
/// One heap allocation instead of two `Option<Arc<str>>` fields keeps
/// `CellExtra` 24 bytes smaller; hyperlinks are rare, so the indirection is
/// negligible.
///
/// Held by `Arc`: an OSC 8 run writes the same (url, id) pair to every covered
/// cell, so the bulk writer builds one payload per run and each cell takes a
/// refcount bump instead of a malloc. Every single-cell mutator goes through
/// `Arc::make_mut`, so the sharing is unobservable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HyperlinkData {
    /// Hyperlink URL (OSC 8).
    pub url: Arc<str>,
    /// Hyperlink ID (OSC 8 `id=` parameter).
    /// Used to group cells into the same hyperlink span for hover/click.
    /// When None, cells with identical URLs are grouped (fallback behavior).
    pub id: Option<Arc<str>>,
}

// The image payload (and a Kitty placement's tag) lives in the vocabulary
// crate: history (`aterm_scrollback::ImageSpan`) must hold the same `Arc` a
// live cell does, and `aterm-scrollback` sits below this crate, so a type
// defined here could only reach it by copying (defeating the renderer's
// pointer-keyed decode cache and the memory budget). Re-exported so
// `aterm_grid::ImageData` stays the name callers use.
pub use aterm_types::{ImageData, ImageFormat, ImageScaling, KittyPlacementTag, SourceRect};

/// A single cell's reference into a placed [`ImageData`].
///
/// The shared image lives behind the `Arc`; `cell_row`/`cell_col` say which
/// tile of the `rows`×`cols` footprint this cell paints (0-indexed from the
/// image's top-left). The renderer maps that tile to a pixel sub-rect of the
/// decoded image. Cheap to clone (one `Arc` bump + two `u16` + a `Copy` tag).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    /// The shared image this cell belongs to.
    pub image: Arc<ImageData>,
    /// Row of this cell within the image footprint (0-indexed).
    pub cell_row: u16,
    /// Column of this cell within the image footprint (0-indexed).
    pub cell_col: u16,
    /// Which Kitty placement stamped this cell, or `None` for every image that
    /// is not a Kitty placement (iTerm2, sixel, host rasters). Kitty's delete
    /// selectors address placements by this tag, so no Kitty command can erase
    /// a picture another protocol drew.
    pub kitty: Option<KittyPlacementTag>,
}

impl ImageRef {
    /// Where this layer stacks among the images sharing its cell, lowest
    /// first ([`aterm_types::layer_order`]).
    #[must_use]
    pub fn draw_key(&self) -> (i32, u32, u32) {
        aterm_types::layer_order(self.image.z_index, self.kitty)
    }
}

/// Most image layers one cell holds: a placement stacked past it pushes the
/// lowest layer out (a DoS bound — each layer is a boxed ref per covered cell).
pub const MAX_IMAGE_LAYERS: usize = 16;

/// Extra attributes for a cell that don't fit in the packed 8-byte structure.
///
/// These are rare attributes that most cells don't need:
/// - Hyperlinks (OSC 8)
/// - Colored underlines (SGR 58/59)
/// - True color RGB (foreground/background)
/// - Zero-width combining characters
/// - Complex characters (non-BMP, grapheme clusters)
///
/// A Kitty Unicode placeholder needs no field of its own: it is its character,
/// combining marks and colours, decoded at render time.
///
/// ## Memory layout
///
/// - `flags: u16` - presence bitflags + extended flags (bits 3-15)
/// - `colors: [u8; 9]` - packed RGB: underline[0-2], fg[3-5], bg[6-8]
/// - `hyperlink: Option<Arc<HyperlinkData>>` - packed URL+ID (8 bytes via niche),
///   shared across the cells of one OSC 8 run (copy-on-write on per-cell edits)
/// - `complex_char: Option<Box<Arc<str>>>` - boxed so the common path pays an
///   8-byte niche pointer instead of a 16-byte fat pointer
/// - `combining: SmallVec<char, 2>` - inline for the common case
/// - `image: Option<Box<SmallVec<ImageRef, 1>>>` - inline-image layers, niche
///   optimized (8 bytes)
///
/// Total: 64 bytes. Boxing adds an allocation only when a grapheme-cluster
/// complex char or inline image is present. `CellExtra` exists only for cells
/// carrying some extra.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CellExtra {
    /// Bitflags: bits 0-2 track color presence, bits 3-15 for extended flags.
    flags: u16,

    /// Packed RGB colors: [underline_r, underline_g, underline_b, fg_r, fg_g, fg_b, bg_r, bg_g, bg_b].
    /// Only valid if corresponding HAS_* flag is set.
    colors: [u8; 9],

    /// Indexed underline color palette index.
    ///
    /// When `Some(idx)`, the underline color is an indexed palette color
    /// resolved from the live palette at render time, so OSC 4 palette changes
    /// update it. The `HAS_UNDERLINE_COLOR` flag is then clear (see
    /// `set_underline_color_u32`).
    underline_color_idx: Option<u8>,

    /// Hyperlink URL and optional ID (OSC 8), packed into a single allocation.
    /// Uses niche optimization: None = null pointer = 0 overhead.
    /// `Arc` so one OSC 8 run allocates once and every covered cell takes a
    /// refcount bump; see [`HyperlinkData`].
    hyperlink: Option<Arc<HyperlinkData>>,

    /// Complex character string (non-BMP, grapheme clusters, combining marks).
    /// Only used when Cell.flags.is_complex() is true.
    ///
    /// Boxed: complex chars are rare, so the common path pays an 8-byte niche
    /// pointer rather than carrying a 16-byte `Arc<str>` fat pointer inline.
    complex_char: Option<Box<Arc<str>>>,

    /// Zero-width combining characters.
    /// Most cells have 0-2 combining chars; SmallVec avoids allocation.
    combining: SmallVec<char, 2>,

    /// The inline images covering this cell, bottom layer first (never empty
    /// when present). One for an iTerm2 or sixel image, which owns its cells;
    /// Kitty placements stack in [`ImageRef::draw_key`] order. The renderer
    /// paints each layer's tile of its shared image in that order. Boxed for
    /// niche optimization (8 bytes when absent).
    image: Option<Box<SmallVec<ImageRef, 1>>>,
}

impl CellExtra {
    /// Check if this extra has any data (non-empty).
    #[must_use]
    #[inline]
    pub fn has_data(&self) -> bool {
        self.hyperlink.is_some()
            || self.complex_char.is_some()
            || !self.combining.is_empty()
            || self.flags != 0
            || self.image.is_some()
            || self.underline_color_idx.is_some()
    }

    /// Get extended flags (bits 3-15 of flags field).
    #[must_use]
    #[inline]
    #[cfg(test)]
    pub fn extended_flags(&self) -> u16 {
        self.flags >> extra_flags::EXTENDED_SHIFT
    }

    /// Set extended flags (bits 3-15 of flags field).
    #[inline]
    pub fn set_extended_flags(&mut self, ext_flags: u16) {
        // Preserve color presence bits (0-2), set extended flags (3-15)
        self.flags =
            (self.flags & extra_flags::COLOR_MASK) | (ext_flags << extra_flags::EXTENDED_SHIFT);
    }

    /// Get the hyperlink URL.
    #[must_use]
    #[inline]
    pub fn hyperlink(&self) -> Option<&Arc<str>> {
        self.hyperlink.as_ref().map(|data| &data.url)
    }

    /// Set the hyperlink URL.
    ///
    /// Clears the ID — URL and ID are coupled (both from the same OSC 8 sequence).
    /// Caller must call `set_hyperlink_id` after this to set the new ID, if any.
    #[inline]
    pub fn set_hyperlink(&mut self, url: Option<Arc<str>>) {
        match url {
            Some(url) => {
                if let Some(data) = &mut self.hyperlink {
                    // Copy-on-write is required: the bulk run writer shares one
                    // payload across an OSC 8 run, so mutating in place would
                    // rewrite the whole run's link. The per-character path builds
                    // its own payload (refcount 1), so it never clones here.
                    let data = Arc::make_mut(data);
                    data.url = url;
                    data.id = None; // Clear stale ID from previous hyperlink
                } else {
                    self.hyperlink = Some(Arc::new(HyperlinkData { url, id: None }));
                }
            }
            None => {
                self.hyperlink = None;
            }
        }
    }

    /// Attach an already-built, possibly shared, hyperlink payload.
    ///
    /// Replaces URL and ID together, like [`Self::set_hyperlink`] (which clears
    /// any stale ID), without a per-cell allocation: the bulk run writer builds
    /// one `Arc<HyperlinkData>` per OSC 8 run. Single-cell mutators
    /// (`set_hyperlink`, `set_hyperlink_id`) copy-on-write via `Arc::make_mut`.
    #[inline]
    pub(crate) fn set_hyperlink_data(&mut self, data: Option<Arc<HyperlinkData>>) {
        self.hyperlink = data;
    }

    /// Get the hyperlink ID (OSC 8 `id=` parameter).
    #[must_use]
    #[inline]
    pub fn hyperlink_id(&self) -> Option<&Arc<str>> {
        self.hyperlink.as_ref().and_then(|data| data.id.as_ref())
    }

    /// Set the hyperlink ID (OSC 8 `id=` parameter).
    ///
    /// Only takes effect if a hyperlink URL is already set (ID without URL is orphaned).
    #[inline]
    pub fn set_hyperlink_id(&mut self, id: Option<Arc<str>>) {
        if let Some(data) = &mut self.hyperlink {
            // Copy-on-write for the same reason as `set_hyperlink`: the payload
            // may be shared with every other cell of the run.
            Arc::make_mut(data).id = id;
        }
    }

    /// Get a packed RGB color by array offset and presence flag.
    #[must_use]
    #[inline]
    fn get_color(&self, offset: usize, flag: u16) -> Option<[u8; 3]> {
        if self.flags & flag != 0 {
            Some([
                self.colors[offset],
                self.colors[offset + 1],
                self.colors[offset + 2],
            ])
        } else {
            None
        }
    }

    /// Set a packed RGB color by array offset and presence flag.
    #[inline]
    fn set_color(&mut self, offset: usize, flag: u16, rgb: Option<[u8; 3]>) {
        match rgb {
            Some([r, g, b]) => {
                self.colors[offset] = r;
                self.colors[offset + 1] = g;
                self.colors[offset + 2] = b;
                self.flags |= flag;
            }
            None => {
                self.flags &= !flag;
            }
        }
    }

    /// Get the underline color as RGB.
    #[must_use]
    #[inline]
    pub fn underline_color(&self) -> Option<[u8; 3]> {
        self.get_color(0, extra_flags::HAS_UNDERLINE_COLOR)
    }

    /// Set the underline color from RGB.
    ///
    /// Clears the indexed palette index — this is an explicit RGB color.
    #[inline]
    pub fn set_underline_color(&mut self, color: Option<[u8; 3]>) {
        self.set_color(0, extra_flags::HAS_UNDERLINE_COLOR, color);
        // Always clear indexed state when setting explicit RGB.
        self.underline_color_idx = None;
    }

    /// Set the underline color from packed u32 format (0xTT_XXXXXX).
    ///
    /// Type byte TT determines interpretation:
    /// - `0x01`: RGB color — stores `[R, G, B]` from the low 24 bits.
    /// - `0x02`: Indexed palette color — stores the palette index, resolved
    ///   from the live palette at draw time so OSC 4 changes apply.
    /// - Other: treated as RGB.
    #[inline]
    pub fn set_underline_color_u32(&mut self, color: Option<u32>) {
        match color {
            Some(c) if (c >> 24) & 0xFF == 0x02 => {
                // Indexed palette color — store index for render-time resolution.
                let index = (c & 0xFF) as u8;
                self.underline_color_idx = Some(index);
                // Clear HAS_UNDERLINE_COLOR so underline_color() returns None
                // (the indexed color is resolved at render time from the palette).
                // has_data() still returns true via underline_color_idx.is_some().
                self.flags &= !extra_flags::HAS_UNDERLINE_COLOR;
            }
            Some(c) => {
                // RGB color — strip type byte, store R/G/B.
                let r = ((c >> 16) & 0xFF) as u8;
                let g = ((c >> 8) & 0xFF) as u8;
                let b = (c & 0xFF) as u8;
                self.set_underline_color(Some([r, g, b]));
            }
            None => {
                self.flags &= !extra_flags::HAS_UNDERLINE_COLOR;
                self.underline_color_idx = None;
            }
        }
    }

    /// The underline colour in packed `0xTT_XXXXXX` form, the inverse of
    /// [`set_underline_color_u32`](Self::set_underline_color_u32).
    ///
    /// `0x02_0000II` for an indexed colour (so it re-resolves against the live
    /// palette after a round trip), `0x01_RRGGBB` for RGB, `None` when no SGR 58
    /// colour is set. The indexed form is checked first; `set_underline_color_u32`
    /// clears `HAS_UNDERLINE_COLOR` when storing an index, so both are never
    /// present.
    ///
    /// Scrollback and the wide-spacer mirror carry this form, so a colour is
    /// not flattened to RGB across either boundary.
    #[must_use]
    #[inline]
    pub fn underline_color_u32(&self) -> Option<u32> {
        if let Some(index) = self.underline_color_idx {
            return Some(0x02_00_00_00 | u32::from(index));
        }
        let [r, g, b] = self.underline_color()?;
        Some(0x01_00_00_00 | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b))
    }

    /// Whether the underline color is an indexed palette color.
    ///
    /// When true, [`underline_color_index`](Self::underline_color_index) returns
    /// the palette index and the renderer should resolve from the live palette.
    #[must_use]
    #[inline]
    pub fn is_underline_color_indexed(&self) -> bool {
        self.underline_color_idx.is_some()
    }

    /// Get the palette index for an indexed underline color.
    ///
    /// Returns `Some(index)` when the underline color is an indexed palette
    /// color, `None` otherwise.
    #[must_use]
    #[inline]
    pub fn underline_color_index(&self) -> Option<u8> {
        self.underline_color_idx
    }

    /// Get foreground RGB color.
    #[must_use]
    #[inline]
    pub fn fg_rgb(&self) -> Option<[u8; 3]> {
        self.get_color(3, extra_flags::HAS_FG_RGB)
    }

    /// Set foreground RGB color.
    #[inline]
    pub fn set_fg_rgb(&mut self, rgb: Option<[u8; 3]>) {
        self.set_color(3, extra_flags::HAS_FG_RGB, rgb);
    }

    /// Get background RGB color.
    #[must_use]
    #[inline]
    pub fn bg_rgb(&self) -> Option<[u8; 3]> {
        self.get_color(6, extra_flags::HAS_BG_RGB)
    }

    /// Set background RGB color.
    #[inline]
    pub fn set_bg_rgb(&mut self, rgb: Option<[u8; 3]>) {
        self.set_color(6, extra_flags::HAS_BG_RGB, rgb);
    }

    /// Get complex character string.
    #[must_use]
    #[inline]
    pub fn complex_char(&self) -> Option<&Arc<str>> {
        self.complex_char.as_deref()
    }

    /// Set complex character string.
    #[inline]
    pub fn set_complex_char(&mut self, s: Option<Arc<str>>) {
        self.complex_char = s.map(Box::new);
    }

    /// Get the combining characters.
    #[must_use]
    #[inline]
    pub fn combining(&self) -> &[char] {
        &self.combining
    }

    /// Add a combining character.
    ///
    /// Combining characters are appended to the base character.
    /// Example: 'e' + U+0301 (combining acute accent) = 'é'
    #[inline]
    pub fn add_combining(&mut self, c: char) {
        // Limit to prevent DoS with excessive combining marks
        if self.combining.len() < Self::MAX_COMBINING {
            self.combining.push(c);
        }
    }

    /// Maximum combining characters per cell (prevents DoS).
    pub const MAX_COMBINING: usize = 16;

    /// Strip the character-bearing fields: combining marks and the
    /// complex-char string.
    ///
    /// Preserves SGR colors (`fg_rgb`/`bg_rgb`/`underline_color`), hyperlinks,
    /// flags and graphics placeholders. Used by DECSERA's selective wipe, which
    /// turns a cell into a space (`set_char`) but keeps its truecolor/underline
    /// overflow while dropping stale combining marks or grapheme-cluster
    /// strings that would leak into copy/selection/search/render.
    #[inline]
    pub fn clear_char_marks(&mut self) {
        self.combining.clear();
        self.complex_char = None;
    }

    /// The inline images covering this cell, bottom layer first (draw order).
    #[must_use]
    #[inline]
    pub fn images(&self) -> &[ImageRef] {
        self.image
            .as_deref()
            .map_or(&[], |layers| layers.as_slice())
    }

    /// The inline images covering this cell, bottom layer first, for an edit
    /// that keeps each layer's [`ImageRef::draw_key`] (a Kitty frame swap).
    #[inline]
    pub fn images_mut(&mut self) -> &mut [ImageRef] {
        self.image
            .as_deref_mut()
            .map_or(&mut [], |layers| layers.as_mut_slice())
    }

    /// The topmost inline image covering this cell.
    #[must_use]
    #[inline]
    pub fn image(&self) -> Option<&ImageRef> {
        self.images().last()
    }

    /// Make `image` the cell's only image (`None`: no image) — an iTerm2 or
    /// sixel image owns the cells it covers.
    #[inline]
    pub fn set_image(&mut self, image: Option<ImageRef>) {
        self.image = image.map(|image| {
            let mut layers = SmallVec::new();
            layers.push(image);
            Box::new(layers)
        });
    }

    /// Add `image` as a layer, in [`ImageRef::draw_key`] order (after the
    /// layers it ties with), keeping at most [`MAX_IMAGE_LAYERS`]: the lowest
    /// layer goes first.
    pub fn stack_image(&mut self, image: ImageRef) {
        let layers = self.image.get_or_insert_with(Box::default);
        let key = image.draw_key();
        let at = layers.partition_point(|layer| layer.draw_key() <= key);
        layers.push(image);
        layers.as_mut_slice()[at..].rotate_right(1);
        if layers.len() > MAX_IMAGE_LAYERS {
            layers.as_mut_slice().rotate_left(1);
            layers.pop();
        }
    }

    /// Keep only the image layers `keep` accepts, in their order.
    pub fn retain_images(&mut self, mut keep: impl FnMut(&ImageRef) -> bool) {
        let Some(layers) = self.image.as_mut() else {
            return;
        };
        let slice = layers.as_mut_slice();
        let mut kept = 0;
        for at in 0..slice.len() {
            if keep(&slice[at]) {
                slice.swap(kept, at);
                kept += 1;
            }
        }
        while layers.len() > kept {
            layers.pop();
        }
        if layers.is_empty() {
            self.image = None;
        }
    }

    /// Calculate memory used by this extra. Saturating throughout, as in
    /// `Grid::memory_used`: exact whenever the sum fits, `usize::MAX` otherwise.
    #[must_use]
    pub fn memory_used(&self) -> usize {
        let base = std::mem::size_of::<Self>();
        // An upper bound: the URL bytes are counted per cell even though the
        // `Arc<str>` and the `HyperlinkData` payload are shared across a run.
        // Over-reporting is the safe direction for a memory budget.
        let hyperlink_mem = self.hyperlink.as_ref().map_or(0, |data| {
            std::mem::size_of::<HyperlinkData>()
                .saturating_add(data.url.len())
                .saturating_add(data.id.as_ref().map_or(0, |s| {
                    std::mem::size_of::<Arc<str>>().saturating_add(s.len())
                }))
        });
        let complex_char_mem = self.complex_char.as_ref().map_or(0, |s| {
            // Boxed Arc<str>: the box holds the 16-byte fat pointer, plus the
            // backing string bytes shared via the Arc.
            std::mem::size_of::<Arc<str>>().saturating_add(s.len())
        });
        let combining_mem = if self.combining.spilled() {
            self.combining
                .capacity()
                .saturating_mul(std::mem::size_of::<char>())
        } else {
            0 // Inline storage, already counted in base
        };
        // The image payload is shared via Arc (counted once at the source, not
        // per covered cell); each cell pays only its boxed layer refs.
        let image_mem = self.image.as_ref().map_or(0, |layers| {
            std::mem::size_of::<SmallVec<ImageRef, 1>>().saturating_add(if layers.spilled() {
                layers
                    .capacity()
                    .saturating_mul(std::mem::size_of::<ImageRef>())
            } else {
                0
            })
        });
        base.saturating_add(hyperlink_mem)
            .saturating_add(complex_char_mem)
            .saturating_add(combining_mem)
            .saturating_add(image_mem)
    }
}

/// Uniform extras to apply to a contiguous column range.
///
/// Used by [`CellExtras::set_range_uniform`] to batch-apply the same
/// extras to every cell in an ASCII run, replacing per-cell `get_or_create`
/// overhead with a single cache invalidation and coord computation.
pub struct UniformExtras<'a> {
    /// Foreground RGB (true-color).
    pub fg_rgb: Option<[u8; 3]>,
    /// Background RGB (true-color).
    pub bg_rgb: Option<[u8; 3]>,
    /// Underline color as packed u32 (0xTT_RRGGBB).
    pub underline_color: Option<u32>,
    /// Extended flags (bits 3-15 of CellExtra flags field).
    pub extended_flags: u16,
    /// Hyperlink URL (OSC 8).
    pub hyperlink: Option<&'a Arc<str>>,
    /// Hyperlink ID (OSC 8 `id=` parameter).
    pub hyperlink_id: Option<&'a Arc<str>>,
}

/// Check if a character is a Unicode combining mark.
///
/// Combining marks include:
/// - Combining Diacritical Marks (U+0300-U+036F)
/// - Combining Diacritical Marks Extended (U+1AB0-U+1AFF)
/// - Combining Diacritical Marks Supplement (U+1DC0-U+1DFF)
/// - Combining Diacritical Marks for Symbols (U+20D0-U+20FF)
/// - Combining Half Marks (U+FE20-U+FE2F)
#[cfg(any(test, feature = "testing"))]
#[must_use]
#[inline]
pub fn is_combining_mark(c: char) -> bool {
    matches!(c,
        '\u{0300}'..='\u{036F}' |  // Combining Diacritical Marks
        '\u{1AB0}'..='\u{1AFF}' |  // Combining Diacritical Marks Extended
        '\u{1DC0}'..='\u{1DFF}' |  // Combining Diacritical Marks Supplement
        '\u{20D0}'..='\u{20FF}' |  // Combining Diacritical Marks for Symbols
        '\u{FE20}'..='\u{FE2F}'    // Combining Half Marks
    )
}

#[cfg(test)]
#[path = "extra_tests.rs"]
mod tests;
