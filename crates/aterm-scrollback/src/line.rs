// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Scrollback line representation with RLE-compressed attributes and sidecar spans.
use aterm_alloc::SmallVec;
use aterm_rle::Rle;
use std::sync::Arc;

/// Inline storage for line content (no heap allocation for short lines).
const INLINE_SIZE: usize = 32;

/// Compact cell styling for RLE storage: packed foreground/background colors and visual flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellAttrs {
    /// Packed foreground color (`0xTT_RRGGBB`).
    pub fg: u32,
    /// Packed background color (`0xTT_RRGGBB`).
    pub bg: u32,
    /// Visual cell flags, wide-char bit, and [`Self::OPENS_CELL`].
    pub flags: u16,
}

/// Default fg color (0xFF_FFFFFF - default type marker + white placeholder).
const DEFAULT_FG: u32 = 0xFF_FF_FF_FF;
/// Default bg color (0xFF_000000 - default type marker + black placeholder).
const DEFAULT_BG: u32 = 0xFF_00_00_00;

impl CellAttrs {
    /// Default cell attributes (default colors, no flags).
    pub const DEFAULT: Self = Self {
        fg: DEFAULT_FG,
        bg: DEFAULT_BG,
        flags: 0,
    };

    /// Creates cell attributes from raw fields.
    #[must_use]
    #[inline]
    pub const fn new(fg: u32, bg: u32, flags: u16) -> Self {
        Self { fg, bg, flags }
    }

    /// Returns whether these are default attributes.
    #[must_use]
    #[inline]
    pub const fn is_default(&self) -> bool {
        self.fg == DEFAULT_FG && self.bg == DEFAULT_BG && self.flags == 0
    }

    /// Mask for visual flags and write-time width geometry preserved in scrollback.
    const VISUAL_FLAGS_MASK: u16 = 0x3BFF;

    /// Flag marking a character that opens a new cell ([`Self::OPENS_CELL`]).
    pub const OPENS_CELL: u16 = 1 << 14;

    /// Creates attributes from raw cell values, masking to visual and width flags.
    #[must_use]
    #[inline]
    pub const fn from_raw(fg: u32, bg: u32, flags: u16) -> Self {
        Self {
            fg,
            bg,
            flags: flags & Self::VISUAL_FLAGS_MASK,
        }
    }

    /// Returns whether [`Self::OPENS_CELL`] is set.
    #[must_use]
    #[inline]
    pub const fn opens_cell(self) -> bool {
        self.flags & Self::OPENS_CELL != 0
    }

    /// Returns a copy with [`Self::OPENS_CELL`] set to `opens`.
    #[must_use]
    #[inline]
    pub const fn with_opens_cell(self, opens: bool) -> Self {
        let flags = self.flags & !Self::OPENS_CELL;
        Self {
            flags: if opens {
                flags | Self::OPENS_CELL
            } else {
                flags
            },
            ..self
        }
    }
}

#[inline]
fn attr_runs(attrs: Option<&Rle<CellAttrs>>) -> &[aterm_rle::Run<CellAttrs>] {
    attrs.map_or(&[], Rle::runs)
}

#[path = "line_codec.rs"]
mod line_codec;
#[path = "line_codec_block.rs"]
mod line_codec_block;
#[cfg(test)]
pub(crate) use line_codec_block::take_records_walked;
// Re-exported wire codec for TerminalCheckpoint grid bodies.
pub(crate) use line_codec::{MAX_DECODE_PAGE_LINES, count_page_lines, serialize_pages};
pub use line_codec::{
    deserialize_lines, deserialize_lines_strict, deserialize_lines_strict_dropping_over_cap_links,
    deserialize_lines_tail_strict, deserialize_page_lines, max_linked_record_bytes,
    serialize_lines, serialize_lines_bounded,
};

#[path = "line_content.rs"]
mod line_content;
pub(crate) use line_content::LineContent;

#[path = "hyperlink_span.rs"]
mod hyperlink_span;
pub use hyperlink_span::HyperlinkSpan;

#[path = "underline_color_span.rs"]
mod underline_color_span;
pub use underline_color_span::UnderlineColorSpan;

#[path = "image_span.rs"]
mod image_span;
pub use image_span::ImageSpan;

/// Scrollback line with text, RLE-compressed attributes, flags, and sidecar spans.
#[derive(Debug, Default)]
pub struct Line {
    /// Line content bytes (normally UTF-8; see `as_str`).
    content: LineContent,
    /// RLE-compressed cell attributes; `None` when every cell is default.
    attrs: Option<Box<Rle<CellAttrs>>>,
    /// Line flags.
    flags: LineFlags,
    /// Hyperlink spans. Boxed: most lines have none, so the common path pays a
    /// niche pointer instead of an inline `SmallVec`; the box keeps ≤2 spans
    /// inline.
    hyperlinks: Option<Box<SmallVec<HyperlinkSpan, 2>>>,
    /// Underline-colour spans (SGR 58): coalesced runs of one packed colour,
    /// boxed like `hyperlinks`.
    underline_colors: Option<Box<SmallVec<UnderlineColorSpan, 2>>>,
    /// Inline-image spans: one per run of columns painting one footprint row, each
    /// holding the shared payload `Arc`; boxed like `hyperlinks`.
    images: Option<Box<SmallVec<ImageSpan, 1>>>,
}

impl Clone for Line {
    #[cfg_attr(trust_verify, trust::skip)]
    fn clone(&self) -> Self {
        Self {
            content: self.content.clone(),
            attrs: self.attrs.clone(),
            flags: self.flags,
            hyperlinks: self.hyperlinks.clone(),
            underline_colors: self.underline_colors.clone(),
            images: self.images.clone(),
        }
    }
}

aterm_types::bitflags! {
    /// Line metadata flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub(crate) struct LineFlags: u8 {
        /// Line wraps from the previous line.
        const WRAPPED = 1 << 0;
    }
}

impl Line {
    /// Creates an empty line.
    #[must_use]
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a line from raw bytes with default attributes.
    #[must_use]
    #[inline]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            content: LineContent::from_bytes(bytes),
            attrs: None,
            flags: LineFlags::empty(),
            hyperlinks: None,
            underline_colors: None,
            images: None,
        }
    }

    /// Shared body of the text constructors: stores no all-default attrs or empty
    /// hyperlinks, so the borrowing and owning forms cannot drift.
    fn from_parts(
        content: LineContent,
        attrs: Rle<CellAttrs>,
        hyperlinks: Vec<HyperlinkSpan>,
    ) -> Self {
        let is_all_default = attrs.run_count() == 0
            || (attrs.run_count() == 1
                && attrs.runs().first().is_some_and(|r| r.value.is_default()));

        let attrs = if is_all_default {
            None
        } else {
            Some(Box::new(attrs))
        };

        let hyperlinks = if hyperlinks.is_empty() {
            None
        } else {
            Some(Box::new(SmallVec::from_vec(hyperlinks)))
        };

        Self {
            content,
            attrs,
            flags: LineFlags::empty(),
            hyperlinks,
            underline_colors: None,
            images: None,
        }
    }

    /// Creates a line with text and RLE-compressed attributes.
    #[must_use]
    pub fn with_attrs(text: &str, attrs: Rle<CellAttrs>) -> Self {
        Self::from_parts(LineContent::from_bytes(text.as_bytes()), attrs, Vec::new())
    }

    /// Creates a line with text, attributes, and hyperlink spans.
    #[must_use]
    pub fn with_hyperlinks(
        text: &str,
        attrs: Rle<CellAttrs>,
        hyperlinks: Vec<HyperlinkSpan>,
    ) -> Self {
        Self::from_parts(LineContent::from_bytes(text.as_bytes()), attrs, hyperlinks)
    }

    /// [`Self::with_hyperlinks`] taking ownership of `text`.
    #[must_use]
    pub fn with_hyperlinks_owned(
        text: String,
        attrs: Rle<CellAttrs>,
        hyperlinks: Vec<HyperlinkSpan>,
    ) -> Self {
        Self::from_parts(LineContent::from_vec(text.into_bytes()), attrs, hyperlinks)
    }

    /// Returns the RLE-compressed attributes, if any.
    #[must_use]
    #[inline]
    pub fn attrs(&self) -> Option<&Rle<CellAttrs>> {
        self.attrs.as_deref()
    }

    /// Returns the attributes for `char_idx`, or [`CellAttrs::DEFAULT`] if unstyled.
    #[must_use]
    pub fn get_attr(&self, char_idx: usize) -> CellAttrs {
        let idx = u32::try_from(char_idx).unwrap_or(u32::MAX);
        let mut run_start: u32 = 0;
        for run in attr_runs(self.attrs.as_deref()) {
            let run_end = run_start.saturating_add(run.length);
            if idx < run_end {
                return run.value;
            }
            run_start = run_end;
        }
        CellAttrs::DEFAULT
    }

    /// Returns a sequential [`AttrRunCursor`] over this line's attribute runs.
    #[must_use]
    #[inline]
    pub fn attr_cursor(&self) -> AttrRunCursor<'_> {
        AttrRunCursor {
            runs: attr_runs(self.attrs.as_deref()),
            i: 0,
            run_start: 0,
        }
    }

    /// Returns whether this line has non-default attributes.
    #[must_use]
    #[inline]
    #[cfg(test)]
    pub const fn has_attrs(&self) -> bool {
        self.attrs.is_some()
    }

    /// Returns the hyperlink URL at `col`, if any.
    #[must_use]
    pub fn get_hyperlink(&self, col: u16) -> Option<&Arc<str>> {
        for span in self.hyperlinks()? {
            if span.contains(col) {
                return Some(&span.url);
            }
        }
        None
    }

    /// Returns the full [`HyperlinkSpan`] at `col`, if any.
    #[must_use]
    #[allow(
        clippy::manual_find,
        reason = "explicit slice loop is the lowerable form for the Trust lane (`ship trust run`); the `.iter().find(..)` rewrite MIR-inlines SmallVec::iter internals its hardened-unsafe check fails on"
    )]
    pub fn get_hyperlink_span(&self, col: u16) -> Option<&HyperlinkSpan> {
        for span in self.hyperlinks()? {
            if span.contains(col) {
                return Some(span);
            }
        }
        None
    }

    /// Returns whether this line has any hyperlinks.
    #[must_use]
    #[inline]
    pub fn has_hyperlinks(&self) -> bool {
        match self.hyperlinks() {
            Some(spans) => !spans.is_empty(),
            None => false,
        }
    }

    /// Returns the number of hyperlink spans.
    #[must_use]
    #[inline]
    pub fn hyperlink_count(&self) -> usize {
        match self.hyperlinks() {
            Some(spans) => spans.len(),
            None => 0,
        }
    }

    /// Returns the hyperlink spans, if any.
    #[must_use]
    #[inline]
    pub fn hyperlinks(&self) -> Option<&[HyperlinkSpan]> {
        self.hyperlinks.as_deref().map(SmallVec::as_slice)
    }

    /// Removes all hyperlink spans while preserving text, attributes, and other sidecars.
    #[cfg_attr(trust_verify, trust::skip)]
    #[inline]
    pub fn clear_hyperlinks(&mut self) {
        self.hyperlinks = None;
    }

    /// Returns the SGR 58 underline-color spans, if any.
    #[must_use]
    #[inline]
    pub fn underline_colors(&self) -> Option<&[UnderlineColorSpan]> {
        self.underline_colors.as_deref().map(SmallVec::as_slice)
    }

    /// Sets the SGR 58 underline-color spans (`None` when `spans` is empty).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn set_underline_colors(&mut self, spans: Vec<UnderlineColorSpan>) {
        self.underline_colors = if spans.is_empty() {
            None
        } else {
            Some(Box::new(SmallVec::from_vec(spans)))
        };
    }

    /// Returns the packed underline color (`0xTT_XXXXXX`) at `col`, if any.
    #[must_use]
    pub fn get_underline_color(&self, col: u16) -> Option<u32> {
        for span in self.underline_colors()? {
            if span.contains(col) {
                return Some(span.color);
            }
        }
        None
    }

    /// Returns whether this line has any underline colors.
    #[must_use]
    #[inline]
    #[cfg(test)]
    pub fn has_underline_colors(&self) -> bool {
        match self.underline_colors() {
            Some(spans) => !spans.is_empty(),
            None => false,
        }
    }

    /// Returns the number of underline-color spans.
    #[must_use]
    #[inline]
    #[cfg(test)]
    pub fn underline_color_count(&self) -> usize {
        match self.underline_colors() {
            Some(spans) => spans.len(),
            None => 0,
        }
    }

    /// Returns the inline-image spans, if any.
    #[must_use]
    #[inline]
    pub fn images(&self) -> Option<&[ImageSpan]> {
        self.images.as_deref().map(SmallVec::as_slice)
    }

    /// Sets the inline-image spans (`None` when `spans` is empty).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn set_images(&mut self, spans: Vec<ImageSpan>) {
        self.images = if spans.is_empty() {
            None
        } else {
            Some(Box::new(SmallVec::from_vec(spans)))
        };
    }

    /// Returns the inline-image span covering `col`, if any.
    #[must_use]
    #[allow(
        clippy::manual_find,
        reason = "explicit slice loop is the lowerable form for the Trust lane (`ship trust run`); the `.iter().find(..)` rewrite MIR-inlines SmallVec::iter internals its hardened-unsafe check fails on"
    )]
    #[cfg(test)]
    pub fn get_image(&self, col: u16) -> Option<&ImageSpan> {
        for span in self.images()? {
            if span.contains(col) {
                return Some(span);
            }
        }
        None
    }

    /// Returns whether this line has any inline images.
    #[must_use]
    #[inline]
    pub fn has_images(&self) -> bool {
        match self.images() {
            Some(spans) => !spans.is_empty(),
            None => false,
        }
    }

    /// Returns the number of inline-image spans.
    #[must_use]
    #[inline]
    pub fn image_count(&self) -> usize {
        match self.images() {
            Some(spans) => spans.len(),
            None => 0,
        }
    }

    /// Returns the line content as bytes.
    #[must_use]
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        self.content.as_bytes()
    }

    /// Returns the content length in bytes.
    #[must_use]
    #[inline]
    pub const fn len(&self) -> usize {
        self.content.len()
    }

    /// Returns whether the line is empty.
    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.content.is_empty()
    }

    /// Returns whether the wrapped flag is set.
    #[must_use]
    #[inline]
    pub const fn is_wrapped(&self) -> bool {
        self.flags.contains(LineFlags::WRAPPED)
    }

    /// Sets the wrapped flag.
    #[inline]
    pub fn set_wrapped(&mut self, wrapped: bool) {
        // Inherent `insert`/`remove`, not `|=`/`-=`: the operator path resolves to
        // the std trait and mints an absent-callee row.
        if wrapped {
            self.flags.insert(LineFlags::WRAPPED);
        } else {
            self.flags.remove(LineFlags::WRAPPED);
        }
    }

    /// Clones the line with its flags reset (`WRAPPED` cleared) for rewrapping.
    #[cfg_attr(trust_verify, trust::skip)]
    #[must_use]
    pub fn cloned_for_rewrap(&self) -> Line {
        let mut clone = self.clone();
        clone.flags = LineFlags::empty();
        clone
    }

    /// Returns the line content as a UTF-8 string slice, or `None` if invalid UTF-8.
    #[must_use]
    #[cfg_attr(trust_verify, trust::skip)]
    #[inline]
    pub fn as_str(&self) -> Option<&str> {
        std::str::from_utf8(self.as_bytes()).ok()
    }

    /// Returns the total memory used by this line in bytes.
    #[must_use]
    pub fn memory_used(&self) -> usize {
        let base = std::mem::size_of::<Self>();
        let content_mem = match &self.content {
            LineContent::Inline(_) => 0,
            LineContent::Heap(v) => v.capacity(),
        };
        let attrs_mem = if self.attrs.is_some() {
            std::mem::size_of::<Rle<CellAttrs>>().saturating_add(
                attr_runs(self.attrs.as_deref())
                    .len()
                    .saturating_mul(std::mem::size_of::<aterm_rle::Run<CellAttrs>>()),
            )
        } else {
            0
        };
        let hyperlinks_mem = match self.hyperlinks() {
            Some(spans) => {
                let boxed = std::mem::size_of::<SmallVec<HyperlinkSpan, 2>>();
                let heap_spans = if spans.len() > 2 {
                    spans
                        .len()
                        .saturating_mul(std::mem::size_of::<HyperlinkSpan>())
                } else {
                    0
                };
                let mut url_mem = 0usize;
                let mut id_mem = 0usize;
                for s in spans {
                    url_mem = url_mem.saturating_add(s.url.len());
                    if let Some(id) = s.id.as_ref() {
                        id_mem = id_mem.saturating_add(id.len());
                    }
                }
                boxed
                    .saturating_add(heap_spans)
                    .saturating_add(url_mem)
                    .saturating_add(id_mem)
            }
            None => 0,
        };
        let underline_colors_mem = match self.underline_colors() {
            Some(spans) => {
                let boxed = std::mem::size_of::<SmallVec<UnderlineColorSpan, 2>>();
                let heap_spans = if spans.len() > 2 {
                    spans
                        .len()
                        .saturating_mul(std::mem::size_of::<UnderlineColorSpan>())
                } else {
                    0
                };
                boxed.saturating_add(heap_spans)
            }
            None => 0,
        };
        let images_mem = match self.images() {
            Some(spans) => {
                let boxed = std::mem::size_of::<SmallVec<ImageSpan, 1>>();
                let mut payload = 0usize;
                for s in spans {
                    payload = payload.saturating_add(s.memory_used());
                }
                boxed.saturating_add(payload)
            }
            None => 0,
        };
        base.saturating_add(content_mem)
            .saturating_add(attrs_mem)
            .saturating_add(hyperlinks_mem)
            .saturating_add(underline_colors_mem)
            .saturating_add(images_mem)
    }

    /// Returns the number of attribute runs.
    #[must_use]
    #[cfg(test)]
    pub fn attr_run_count(&self) -> usize {
        // `runs.len()` is `run_count()`, via the gate-provable access (see `attr_runs`).
        attr_runs(self.attrs.as_deref()).len()
    }
}

/// Sequential cursor over a [`Line`]'s attribute RLE runs (see [`Line::attr_cursor`]).
#[derive(Debug)]
pub struct AttrRunCursor<'a> {
    runs: &'a [aterm_rle::Run<CellAttrs>],
    /// Index of the run the cursor is parked on.
    i: usize,
    /// Cumulative char index where `runs[i]` starts.
    run_start: u32,
}

impl AttrRunCursor<'_> {
    /// Returns the attributes at `char_idx` in amortized O(1) for non-decreasing indices.
    #[must_use]
    #[inline]
    pub fn attr_at(&mut self, char_idx: usize) -> CellAttrs {
        let idx = u32::try_from(char_idx).unwrap_or(u32::MAX);
        if idx < self.run_start {
            self.i = 0;
            self.run_start = 0;
        }
        while let Some(run) = self.runs.get(self.i) {
            let run_end = self.run_start.saturating_add(run.length);
            if idx < run_end {
                return run.value;
            }
            self.run_start = run_end;
            self.i += 1;
        }
        CellAttrs::DEFAULT
    }
}

impl From<&str> for Line {
    fn from(s: &str) -> Self {
        Self {
            content: LineContent::from_bytes(s.as_bytes()),
            attrs: None,
            flags: LineFlags::empty(),
            hyperlinks: None,
            underline_colors: None,
            images: None,
        }
    }
}

impl std::fmt::Display for Line {
    #[cfg_attr(trust_verify, trust::skip)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `write_str`, not `write!`: no `format_args!` expansion for the `verify
        // trust` lane to fail closed on.
        f.write_str(&String::from_utf8_lossy(self.as_bytes()))
    }
}

#[cfg(test)]
#[path = "line_tests.rs"]
mod tests;
