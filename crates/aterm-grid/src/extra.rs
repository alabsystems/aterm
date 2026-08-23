// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Cell extras for rarely-used attributes.
//!
//! ## Design
//!
//! The packed 8-byte `Cell` handles common attributes efficiently.
//! For rare features like hyperlinks and combining characters, we use
//! an external lookup table (`CellExtras`) to avoid bloating every cell.
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
//! This keeps the common case fast (8-byte cells) while supporting
//! all terminal features when needed.
//!
//! ## Memory Optimization (M9)
//!
//! `CellExtra` uses packed storage for RGB colors:
//! - Bitflags track which fields are present (avoids Option discriminants)
//! - Three RGB colors packed into a single 9-byte array
//! - Saves ~16 bytes per extra vs naive Option<[u8; 3]> fields
//!
//! ## Usage
//!
//! - Hyperlinks: OSC 8 sequences set hyperlinks on cells
//! - Combining characters: Unicode combining marks (U+0300-U+036F, etc.)
//! - Underline colors: SGR 58/59 for colored underlines
//!
//! ## Verification
//!
//! Kani proofs (5 symbolic, genuine verification):
//! - `cell_coord_hash_consistent` - CellCoord PartialEq reflexivity (symbolic row/col)
//! - `combining_mark_range_valid` - Combining mark detection over U+0300..U+036F (symbolic codepoint)
//! - `hyperlink_data_box_niche` - `Option<Arc<HyperlinkData>>` uses niche optimization (8 bytes)
//! - `fnv1a_nonzero_single_byte` - FNV-1a hash non-zero for all 256 byte values
//! - `fnv1a_nonzero_two_bytes` - FNV-1a hash non-zero for all 2-byte sequences

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
/// Packed into a single heap allocation to save 24 bytes in `CellExtra`
/// (one 8-byte pointer vs two 16-byte `Option<Arc<str>>` fields).
/// Hyperlinks are rare (most cells have none), so the extra indirection is negligible.
///
/// Held by `Arc`, not `Box`: an OSC 8 run writes the SAME (url, id) pair to every
/// covered cell, so the bulk writer builds one payload per run and hands each cell
/// a refcount bump instead of its own malloc (`ls --hyperlink=always`, hyperlinked
/// man pages and OSC 8 diagnostics otherwise issue thousands of small mallocs per
/// screenful). The sharing is unobservable: every single-cell mutator goes through
/// `Arc::make_mut`, so a per-cell url/id write copies-on-write exactly as it did
/// when each cell owned its own `Box`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HyperlinkData {
    /// Hyperlink URL (OSC 8).
    pub url: Arc<str>,
    /// Hyperlink ID (OSC 8 `id=` parameter).
    /// Used to group cells into the same hyperlink span for hover/click.
    /// When None, cells with identical URLs are grouped (fallback behavior).
    pub id: Option<Arc<str>>,
}

/// Source encoding of an inline image's raw payload.
///
/// The engine does NOT decode pixels (it carries no image-codec dependency);
/// it stores the raw bytes plus this hint, and the renderer decodes once. Only
/// PNG is decoded today; an unknown format degrades to drawing nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    /// PNG (`\x89PNG\r\n…`). The one container format the renderer decodes.
    Png,
    /// Already-decoded, packed RGBA8 pixels (`[r, g, b, a]` per pixel,
    /// row-major over `width`). Used by the sixel path, which decodes the
    /// raster in the engine (the `aterm-sixel` crate) since sixel has no
    /// container the renderer's PNG decoder could read. `ImageData.bytes` then
    /// holds exactly `4 * width * height` bytes; the renderer resamples them to
    /// the footprint directly (no codec). This keeps the engine codec-free.
    RawRgba8 {
        /// Source raster width in pixels.
        width: u16,
        /// Source raster height in pixels.
        height: u16,
    },
    /// Anything else (JPEG, GIF, …) — kept verbatim but not drawn yet.
    Unknown,
}

/// An inline image placed on the grid (iTerm2 OSC 1337 `File=`).
///
/// Decoupled from the cells: the (possibly large) payload is stored ONCE behind
/// an `Arc` and every covered cell holds a cheap [`ImageRef`] into it with its
/// own sub-cell coordinates. The engine keeps the RAW (undecoded) bytes — it has
/// no image codec — and the renderer decodes them to RGBA a single time, keyed
/// by the `Arc`'s pointer identity, then blits the cell's slice of the result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageData {
    /// Raw, undecoded payload (e.g. the PNG file bytes) as delivered.
    pub bytes: Vec<u8>,
    /// Encoding hint for the renderer's decoder.
    pub format: ImageFormat,
    /// Footprint width in CELLS (how many columns the image spans).
    pub cols: u16,
    /// Footprint height in CELLS (how many rows the image spans).
    pub rows: u16,
    /// Kitty `z=` stacking order. `< 0` draws BEHIND the cell's text (the glyph
    /// paints on top); `>= 0` (the default for iTerm2/Sixel and `z=0` Kitty) draws
    /// OVER the cell, which keeps the historical "image owns the cell" behavior.
    pub z_index: i32,
    /// CHROME-BAND LIFT, in device px: how far this image's raster extends ABOVE
    /// its first covered cell row, into the window's chrome band (`pad_top +
    /// head`). `0` for every terminal-content image — the engine's OSC 1337 /
    /// sixel / Kitty constructors never set it, so nothing an application prints
    /// can draw outside its own cells. Non-zero ONLY for the host's tab-strip
    /// band raster (`aterm-gui`'s pixel band), whose design needs the one canvas
    /// a cell-quantised footprint cannot give it: the full optical band from the
    /// window's top edge down. Renderers honour it by (a) decoding the footprint
    /// `lift` px taller than `rows·cell_h` and (b) letting the FIRST footprint
    /// row's tile paint `[y0 − lift, y0)` as well as its own cell band; rows past
    /// the first read their source `lift` px lower. With `0` both clauses are
    /// arithmetic no-ops, byte-identical to the pre-lift renderers.
    pub band_lift_px: u16,
}

/// A single cell's reference into a placed [`ImageData`].
///
/// The shared image lives behind the `Arc`; `cell_row`/`cell_col` say which tile
/// of the `rows`×`cols` footprint THIS cell paints (0-indexed from the image's
/// top-left). The renderer maps that tile to a pixel sub-rect of the decoded
/// image. Cheap to clone (one `Arc` bump + two `u16`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    /// The shared image this cell belongs to.
    pub image: Arc<ImageData>,
    /// Row of this cell within the image footprint (0-indexed).
    pub cell_row: u16,
    /// Column of this cell within the image footprint (0-indexed).
    pub cell_col: u16,
}

/// Kitty graphics Unicode placeholder data for a cell.
///
/// Stored in `CellExtra` when the parser encounters a placeholder character
/// (U+10EEEE with combining characters encoding image/placement coordinates).
/// The renderer uses this to draw the corresponding sub-region of a Kitty image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KittyPlaceholderData {
    /// Image ID (from diacritics on the base placeholder character).
    pub image_id: u32,
    /// Placement ID (0 = default placement).
    pub placement_id: u32,
    /// Row offset within the image placement (0-indexed).
    pub row: u32,
    /// Column offset within the image placement (0-indexed).
    pub col: u32,
}

/// Extra attributes for a cell that don't fit in the packed 8-byte structure.
///
/// These are rare attributes that most cells don't need:
/// - Hyperlinks (OSC 8)
/// - Colored underlines (SGR 58/59)
/// - True color RGB (foreground/background)
/// - Zero-width combining characters
/// - Complex characters (non-BMP, grapheme clusters)
/// - Kitty graphics Unicode placeholders
///
/// ## Memory Layout (M9 optimization + perf-memory boxing)
///
/// Uses packed storage to minimize size:
/// - `flags: u16` - presence bitflags + extended flags (bits 3-15)
/// - `colors: [u8; 9]` - packed RGB: underline[0-2], fg[3-5], bg[6-8]
/// - `hyperlink: Option<Arc<HyperlinkData>>` - packed URL+ID (8 bytes via niche),
///   shared across the cells of one OSC 8 run (copy-on-write on per-cell edits)
/// - `complex_char: Option<Box<Arc<str>>>` - boxed; rare, so the 8-byte niche
///   pointer replaces the 16-byte inline `Arc<str>` fat pointer on the common path
/// - `combining: SmallVec<char, 2>` - inline for the common case
/// - `kitty_placeholder: Option<Box<KittyPlaceholderData>>` - niche optimized (8 bytes)
/// - `image: Option<Box<ImageRef>>` - inline-image ref, niche optimized (8 bytes)
///
/// Total: ~72 bytes (64 after boxing `complex_char`, plus 8 for the niche
/// `image` pointer). Boxing only adds an allocation when a grapheme-cluster
/// complex char (or inline image) is actually present; the values returned by
/// the accessors are byte-identical. `CellExtra` is allocated only for cells
/// that carry any extra at all (rare), so the cost is paid only by those cells.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CellExtra {
    /// Bitflags: bits 0-2 track color presence, bits 3-15 for extended flags.
    flags: u16,

    /// Packed RGB colors: [underline_r, underline_g, underline_b, fg_r, fg_g, fg_b, bg_r, bg_g, bg_b].
    /// Only valid if corresponding HAS_* flag is set.
    colors: [u8; 9],

    /// Indexed underline color palette index (#7445).
    ///
    /// When `Some(idx)`, the underline color is an indexed palette color that
    /// should be resolved from the live palette at render time. This allows
    /// OSC 4 palette changes to dynamically update underline colors.
    /// When `Some`, the `HAS_UNDERLINE_COLOR` flag is also set and `colors[0..3]`
    /// hold the index in `[0]` (the RGB bytes are unused in this mode).
    underline_color_idx: Option<u8>,

    /// Hyperlink URL and optional ID (OSC 8), packed into a single allocation.
    /// Uses niche optimization: None = null pointer = 0 overhead.
    /// `Arc` (not `Box`) so one OSC 8 run allocates once and every covered cell
    /// takes a refcount bump — see [`HyperlinkData`].
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

    /// Kitty graphics Unicode placeholder data.
    /// Present when this cell represents part of a Kitty image via the
    /// Unicode placeholder protocol (U+10EEEE with combining diacritics).
    kitty_placeholder: Option<Box<KittyPlaceholderData>>,

    /// Inline image reference (iTerm2 OSC 1337 `File=`).
    /// Present when this cell is covered by an inline image; the renderer paints
    /// the cell's tile of the shared image and skips the glyph. Boxed for niche
    /// optimization (8 bytes when absent), mirroring `kitty_placeholder`.
    image: Option<Box<ImageRef>>,
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
            || self.kitty_placeholder.is_some()
            || self.image.is_some()
            || self.underline_color_idx.is_some()
    }

    /// Get extended flags (bits 3-15 of flags field).
    #[must_use]
    #[inline]
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
                    // COPY-ON-WRITE IS MANDATORY, not an optimization: the bulk
                    // run writer shares ONE payload across every cell of an OSC 8
                    // run, so mutating in place here would silently rewrite the
                    // whole run's link. `make_mut` reproduces the old per-cell
                    // `Box` semantics exactly, and never clones on the
                    // per-character path, which builds its own payload (refcount
                    // 1) before touching it.
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

    /// Attach an already-built — and possibly SHARED — hyperlink payload.
    ///
    /// Replaces URL and ID together, matching [`Self::set_hyperlink`]'s contract
    /// (which clears any stale ID), but without the per-cell allocation: the bulk
    /// run writer builds one `Arc<HyperlinkData>` per OSC 8 run and gives each
    /// covered cell a refcount bump instead of its own `HyperlinkData` malloc.
    /// Sharing stays unobservable because every single-cell mutator
    /// (`set_hyperlink`, `set_hyperlink_id`) copies-on-write via `Arc::make_mut`.
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
        // Always clear indexed state when setting explicit RGB (#7445).
        self.underline_color_idx = None;
    }

    /// Set the underline color from packed u32 format (0xTT_XXXXXX).
    ///
    /// Type byte TT determines interpretation:
    /// - `0x01`: RGB color — stores `[R, G, B]` from the low 24 bits.
    /// - `0x02`: Indexed palette color (#7445) — stores the palette index and
    ///   the renderer resolves from the live palette at draw time so that
    ///   OSC 4 palette changes update the underline color dynamically.
    /// - Other: Treated as RGB (backwards compatibility).
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

    /// Whether the underline color is an indexed palette color (#7445).
    ///
    /// When true, [`underline_color_index`](Self::underline_color_index) returns
    /// the palette index and the renderer should resolve from the live palette.
    #[must_use]
    #[inline]
    pub fn is_underline_color_indexed(&self) -> bool {
        self.underline_color_idx.is_some()
    }

    /// Get the palette index for an indexed underline color (#7445).
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
    /// flags, and graphics placeholders. Used by DECSERA's selective wipe,
    /// which turns a cell into a space (`set_char`) but must keep its
    /// out-of-line truecolor/underline overflow while dropping any stale
    /// combining mark or orphaned grapheme-cluster string that would otherwise
    /// leak into copy/selection/search/render (mirrors the #7456 cleanup for
    /// the selective-wipe path).
    #[inline]
    pub fn clear_char_marks(&mut self) {
        self.combining.clear();
        self.complex_char = None;
    }

    /// Get the Kitty graphics placeholder data for this cell.
    #[must_use]
    #[inline]
    pub fn kitty_placeholder(&self) -> Option<&KittyPlaceholderData> {
        self.kitty_placeholder.as_deref()
    }

    /// Set the Kitty graphics placeholder data for this cell.
    #[inline]
    pub fn set_kitty_placeholder(&mut self, data: Option<KittyPlaceholderData>) {
        self.kitty_placeholder = data.map(Box::new);
    }

    /// Get the inline image reference for this cell (iTerm2 OSC 1337).
    #[must_use]
    #[inline]
    pub fn image(&self) -> Option<&ImageRef> {
        self.image.as_deref()
    }

    /// Set the inline image reference for this cell (iTerm2 OSC 1337).
    #[inline]
    pub fn set_image(&mut self, image: Option<ImageRef>) {
        self.image = image.map(Box::new);
    }

    /// Calculate memory used by this extra.
    #[must_use]
    pub fn memory_used(&self) -> usize {
        let base = std::mem::size_of::<Self>();
        // Upper bound, as before: the URL bytes were already counted once per
        // cell even though the `Arc<str>` is shared, and the `HyperlinkData`
        // payload is now shared across the cells of a run too. Over-reporting is
        // the safe direction for a memory budget — it never under-counts.
        let hyperlink_mem = self.hyperlink.as_ref().map_or(0, |data| {
            std::mem::size_of::<HyperlinkData>()
                + data.url.len()
                + data
                    .id
                    .as_ref()
                    .map_or(0, |s| std::mem::size_of::<Arc<str>>() + s.len())
        });
        let complex_char_mem = self.complex_char.as_ref().map_or(0, |s| {
            // Boxed Arc<str>: the box holds the 16-byte fat pointer, plus the
            // backing string bytes shared via the Arc.
            std::mem::size_of::<Arc<str>>() + s.len()
        });
        let combining_mem = if self.combining.spilled() {
            self.combining.capacity() * std::mem::size_of::<char>()
        } else {
            0 // Inline storage, already counted in base
        };
        let placeholder_mem = if self.kitty_placeholder.is_some() {
            std::mem::size_of::<KittyPlaceholderData>()
        } else {
            0
        };
        // The image payload is shared via Arc (counted once at the source, not
        // per covered cell); each cell pays only the boxed ImageRef.
        let image_mem = self
            .image
            .as_ref()
            .map_or(0, |_| std::mem::size_of::<ImageRef>());
        base + hyperlink_mem + complex_char_mem + combining_mem + placeholder_mem + image_mem
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
#[cfg(any(test, kani, feature = "testing"))]
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

#[cfg(kani)]
#[path = "extra_kani_proofs.rs"]
mod proofs;
