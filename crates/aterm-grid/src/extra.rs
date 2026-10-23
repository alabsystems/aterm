// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Cell extras: rare attributes kept in a side table (`CellExtras`) keyed by cell position.

use aterm_alloc::SmallVec;
use std::sync::Arc;

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HyperlinkData {
    /// Hyperlink URL (OSC 8).
    pub url: Arc<str>,
    /// Hyperlink ID (OSC 8 `id=` parameter).
    pub id: Option<Arc<str>>,
}

pub use aterm_types::{ImageData, ImageFormat, ImageScaling, KittyPlacementTag, SourceRect};

/// A single cell's reference into a placed [`ImageData`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    /// The shared image this cell belongs to.
    pub image: Arc<ImageData>,
    /// Row of this cell within the image footprint (0-indexed).
    pub cell_row: u16,
    /// Column of this cell within the image footprint (0-indexed).
    pub cell_col: u16,
    /// Which Kitty placement stamped this cell, or `None` for non-Kitty placements.
    pub kitty: Option<KittyPlacementTag>,
}

impl ImageRef {
    /// Where this layer stacks among the images sharing its cell, lowest first.
    #[must_use]
    #[inline]
    pub fn draw_key(&self) -> (i32, u32, u32) {
        aterm_types::layer_order(self.image.z_index, self.kitty)
    }
}

/// Maximum image layers one cell holds.
pub const MAX_IMAGE_LAYERS: usize = 16;

/// Rare cell attributes stored outside the packed 8-byte `Cell`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CellExtra {
    /// Bitflags: bits 0-2 track color presence, bits 3-15 for extended flags.
    flags: u16,
    /// Packed RGB.
    colors: [u8; 9],
    /// Indexed underline color.
    underline_color_idx: Option<u8>,
    /// Hyperlink URL and optional ID (OSC 8) in one allocation.
    hyperlink: Option<Arc<HyperlinkData>>,
    /// Complex character string (non-BMP, grapheme clusters).
    complex_char: Option<Box<Arc<str>>>,
    /// Zero-width combining characters (inline up to two).
    combining: SmallVec<char, 2>,
    /// The inline images covering this cell.
    image: Option<Box<SmallVec<ImageRef, 1>>>,
}

impl CellExtra {
    /// Check if this extra has any data (non-empty).
    #[must_use]
    #[inline]
    pub const fn has_data(&self) -> bool {
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
    pub const fn extended_flags(&self) -> u16 {
        self.flags >> extra_flags::EXTENDED_SHIFT
    }

    /// Set extended flags (bits 3-15 of flags field).
    #[inline]
    pub const fn set_extended_flags(&mut self, ext_flags: u16) {
        self.flags =
            (self.flags & extra_flags::COLOR_MASK) | (ext_flags << extra_flags::EXTENDED_SHIFT);
    }

    /// Get the hyperlink URL.
    #[must_use]
    #[inline]
    pub fn hyperlink(&self) -> Option<&Arc<str>> {
        self.hyperlink.as_ref().map(|data| &data.url)
    }

    /// Set the hyperlink URL, clearing the ID.
    #[inline]
    pub fn set_hyperlink(&mut self, url: Option<Arc<str>>) {
        match url {
            Some(url) => {
                if let Some(data) = &mut self.hyperlink {
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

    /// Attach an already-built.
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

    /// Set the hyperlink ID (OSC 8 `id=` parameter); a no-op without a URL.
    #[inline]
    pub fn set_hyperlink_id(&mut self, id: Option<Arc<str>>) {
        if let Some(data) = &mut self.hyperlink {
            Arc::make_mut(data).id = id;
        }
    }

    /// Get a packed RGB color by array offset and presence flag.
    #[must_use]
    #[inline]
    const fn get_color(&self, offset: usize, flag: u16) -> Option<[u8; 3]> {
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
    const fn set_color(&mut self, offset: usize, flag: u16, rgb: Option<[u8; 3]>) {
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
    pub const fn underline_color(&self) -> Option<[u8; 3]> {
        self.get_color(0, extra_flags::HAS_UNDERLINE_COLOR)
    }

    /// Set the underline color from RGB, clearing any indexed palette color.
    #[inline]
    pub const fn set_underline_color(&mut self, color: Option<[u8; 3]>) {
        self.set_color(0, extra_flags::HAS_UNDERLINE_COLOR, color);
        self.underline_color_idx = None;
    }

    /// Set the underline color from packed u32 format (0xTT_XXXXXX).
    #[inline]
    pub const fn set_underline_color_u32(&mut self, color: Option<u32>) {
        match color {
            Some(c) if (c >> 24) & 0xFF == 0x02 => {
                let index = (c & 0xFF) as u8;
                self.underline_color_idx = Some(index);
                self.flags &= !extra_flags::HAS_UNDERLINE_COLOR;
            }
            Some(c) => {
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

    /// The underline colour in packed `0xTT_XXXXXX` form.
    #[must_use]
    #[inline]
    pub const fn underline_color_u32(&self) -> Option<u32> {
        if let Some(index) = self.underline_color_idx {
            return Some(0x02_00_00_00 | (index as u32));
        }
        let Some([r, g, b]) = self.underline_color() else {
            return None;
        };
        Some(0x01_00_00_00 | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32))
    }

    /// Whether the underline color is an indexed palette color.
    #[must_use]
    #[inline]
    pub const fn is_underline_color_indexed(&self) -> bool {
        self.underline_color_idx.is_some()
    }

    /// Get the palette index of an indexed underline color.
    #[must_use]
    #[inline]
    pub const fn underline_color_index(&self) -> Option<u8> {
        self.underline_color_idx
    }

    /// Get foreground RGB color.
    #[must_use]
    #[inline]
    pub const fn fg_rgb(&self) -> Option<[u8; 3]> {
        self.get_color(3, extra_flags::HAS_FG_RGB)
    }

    /// Set foreground RGB color.
    #[inline]
    pub const fn set_fg_rgb(&mut self, rgb: Option<[u8; 3]>) {
        self.set_color(3, extra_flags::HAS_FG_RGB, rgb);
    }

    /// Get background RGB color.
    #[must_use]
    #[inline]
    pub const fn bg_rgb(&self) -> Option<[u8; 3]> {
        self.get_color(6, extra_flags::HAS_BG_RGB)
    }

    /// Set background RGB color.
    #[inline]
    pub const fn set_bg_rgb(&mut self, rgb: Option<[u8; 3]>) {
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

    /// Add a combining character up to [`Self::MAX_COMBINING`].
    #[inline]
    pub fn add_combining(&mut self, c: char) {
        if self.combining.len() < Self::MAX_COMBINING {
            self.combining.push(c);
        }
    }

    /// Maximum combining characters per cell (prevents DoS).
    pub const MAX_COMBINING: usize = 16;

    /// Strip combining marks and the complex-char string.
    #[inline]
    pub fn clear_char_marks(&mut self) {
        self.combining.clear();
        self.complex_char = None;
    }

    /// Whether this entry holds anything besides truecolor fg/bg bytes.
    #[must_use]
    #[inline]
    pub(crate) const fn has_claimable_data(&self) -> bool {
        self.hyperlink.is_some()
            || self.complex_char.is_some()
            || !self.combining.is_empty()
            || self.image.is_some()
            || self.underline_color_idx.is_some()
            || self.flags & !(extra_flags::HAS_FG_RGB | extra_flags::HAS_BG_RGB) != 0
    }

    /// Drop everything [`Self::has_claimable_data`] counts.
    #[inline]
    pub(crate) fn retain_overflow_colors(&mut self) -> bool {
        self.hyperlink = None;
        self.complex_char = None;
        self.combining.clear();
        self.image = None;
        self.underline_color_idx = None;
        self.flags &= extra_flags::HAS_FG_RGB | extra_flags::HAS_BG_RGB;
        self.colors[..3].fill(0);
        self.has_data()
    }

    /// The inline images covering this cell, bottom layer first (draw order).
    #[must_use]
    #[inline]
    pub fn images(&self) -> &[ImageRef] {
        self.image
            .as_deref()
            .map_or(&[], |layers| layers.as_slice())
    }

    /// Mutable slice of inline images covering this cell, bottom layer first.
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

    /// Make `image` the cell's only image (`None`: no image).
    #[inline]
    pub fn set_image(&mut self, image: Option<ImageRef>) {
        self.image = image.map(|image| {
            let mut layers = SmallVec::new();
            layers.push(image);
            Box::new(layers)
        });
    }

    /// Add `image` as a layer in [`ImageRef::draw_key`] order.
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

    /// Calculate memory used by this extra.
    #[must_use]
    pub fn memory_used(&self) -> usize {
        let base = std::mem::size_of::<Self>();
        let hyperlink_mem = self.hyperlink.as_ref().map_or(0, |data| {
            std::mem::size_of::<HyperlinkData>()
                .saturating_add(data.url.len())
                .saturating_add(data.id.as_ref().map_or(0, |s| {
                    std::mem::size_of::<Arc<str>>().saturating_add(s.len())
                }))
        });
        let complex_char_mem = self.complex_char.as_ref().map_or(0, |s| {
            std::mem::size_of::<Arc<str>>().saturating_add(s.len())
        });
        let combining_mem = if self.combining.spilled() {
            self.combining
                .capacity()
                .saturating_mul(std::mem::size_of::<char>())
        } else {
            0 // Inline storage, already counted in base
        };
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

/// Uniform extras for a contiguous column range.
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

/// Encode a packed `0xTT_XXXXXX` underline colour as an SGR 58 parameter.
#[must_use]
pub fn underline_color_sgr(packed: u32) -> String {
    if (packed >> 24) & 0xFF == 0x02 {
        format!("58:5:{}", packed & 0xFF)
    } else {
        let (r, g, b) = ((packed >> 16) & 0xFF, (packed >> 8) & 0xFF, packed & 0xFF);
        format!("58:2::{r}:{g}:{b}")
    }
}

/// Check if a character lies in one of the Unicode combining-mark blocks.
#[cfg(any(test, debug_assertions))]
#[must_use]
#[inline]
pub const fn is_combining_mark(c: char) -> bool {
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
