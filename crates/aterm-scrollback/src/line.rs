// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Line representation for scrollback storage.
//!
//! Hot-tier lines hold text plus RLE-compressed attributes (`(style, count)`
//! runs instead of per-cell styles); the warm and cold tiers hold them serialized
//! and compressed.

use aterm_alloc::SmallVec;
use aterm_rle::Rle;
use std::sync::Arc;

/// Inline storage for line content (no heap allocation for short lines).
///
/// The hot tier stores `Line` structs contiguously, so the inline buffer is paid
/// on every line: 32 bytes keeps short prompts allocation-free (128 made `Line`
/// 304 bytes); longer lines spill to an exactly sized heap `Vec`.
const INLINE_SIZE: usize = 32;

/// Compact cell styling for RLE storage: packed foreground and background
/// colours and visual flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellAttrs {
    /// Packed foreground color.
    /// Format: 0xTT_RRGGBB where TT indicates type:
    /// - 0x00: Indexed color (RRGGBB = 0x00_00_XX where XX is index)
    /// - 0x01: True color RGB
    /// - 0xFF: Default color
    pub fg: u32,
    /// Packed background color (same format as fg).
    pub bg: u32,
    /// Cell flags (bold, italic, underline, etc.) plus the main-cell WIDE bit,
    /// and on a cell's first character [`Self::OPENS_CELL`]. Excludes
    /// WIDE_CONTINUATION/PROTECTED and COMPLEX.
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

    /// Create new cell attributes.
    #[must_use]
    pub const fn new(fg: u32, bg: u32, flags: u16) -> Self {
        Self { fg, bg, flags }
    }

    /// Check if these are default attributes.
    #[must_use]
    #[inline]
    pub const fn is_default(&self) -> bool {
        self.fg == DEFAULT_FG && self.bg == DEFAULT_BG && self.flags == 0
    }

    /// Mask for visual flags and write-time width geometry preserved in scrollback.
    /// Includes bits 0-9 (bold through WIDE) plus bits 11-13 (superscript,
    /// subscript, curly_underline). Excludes WIDE_CONTINUATION/PROTECTED (bit
    /// 10), bit 14 (a live cell never sets it; a stored one means
    /// [`Self::OPENS_CELL`]), and COMPLEX (bit 15), which cannot be
    /// interpreted independently from their live-grid context.
    const VISUAL_FLAGS_MASK: u16 = 0x3BFF; // bits 0-9, 11-13

    /// Stored-line geometry on a character: it opens a cell of its own,
    /// though a walk of the line's text would fold it into the cell before
    /// (flag halves or a skin tone and its base the grid drew apart, a cell
    /// after one ending in a ZWJ). The writer of a line sets it where that walk
    /// would fold a cell's first character; every walk of a stored line ends a
    /// unit before it. Bit 14 of the flag word, which no live cell sets.
    pub const OPENS_CELL: u16 = 1 << 14;

    /// Create from raw cell values, filtering to visual flags plus main-cell
    /// write-time width geometry.
    #[must_use]
    #[inline]
    pub const fn from_raw(fg: u32, bg: u32, flags: u16) -> Self {
        Self {
            fg,
            bg,
            flags: flags & Self::VISUAL_FLAGS_MASK,
        }
    }

    /// Whether the character these attributes are stored on opens a cell
    /// ([`Self::OPENS_CELL`]).
    #[must_use]
    #[inline]
    pub const fn opens_cell(self) -> bool {
        self.flags & Self::OPENS_CELL != 0
    }

    /// These attributes with [`Self::OPENS_CELL`] set to `opens`: the rendition
    /// a cell draws with is the same either way.
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

// Trust-lane access idiom for the attrs RLE: all access goes through `attr_runs`
// (`map_or(&[], Rle::runs)`, a method passed as a fn item, as `Line::hyperlinks`
// does with `SmallVec::as_slice`). A direct `rle.runs()` / `rle.get(..)` call is
// MIR-inlined with the `Vec` internals it wraps, and the Trust lane's
// hardened-unsafe check fails closed on std's unsafe blocks there. The fn-item
// spelling keeps the callee a plain cross-crate call; `runs.len()` is
// `run_count()`, and a linear walk of `runs` gives `Rle::get`'s value.

fn attr_runs(attrs: Option<&Rle<CellAttrs>>) -> &[aterm_rle::Run<CellAttrs>] {
    attrs.map_or(&[], Rle::runs)
}

#[path = "line_codec.rs"]
mod line_codec;
#[path = "line_codec_block.rs"]
mod line_codec_block;
#[cfg(test)]
pub(crate) use line_codec_block::take_records_walked;
// The line codec is public: it is the wire form of `TerminalCheckpoint` grid
// bodies, used by aterm-grid and aterm-core.
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

/// A scrollback line: text, RLE-compressed attributes, flags, and sidecar spans
/// (hyperlinks, underline colours, images).
#[derive(Debug, Default)]
pub struct Line {
    /// Line content bytes (normally UTF-8; see `as_str`).
    content: LineContent,
    /// RLE-compressed cell attributes; `None` when every cell is default.
    ///
    /// Boxed: plain-text lines pay a niche pointer instead of an inline `Rle`, and
    /// the hot tier stores `Line`s contiguously, so struct size is resident memory.
    attrs: Option<Box<Rle<CellAttrs>>>,
    /// Line flags.
    flags: LineFlags,
    /// Hyperlink spans. Boxed: most lines have none, so the common path pays a
    /// niche pointer instead of an inline `SmallVec`; the box keeps ≤2 spans
    /// inline.
    hyperlinks: Option<Box<SmallVec<HyperlinkSpan, 2>>>,
    /// Underline-colour spans (SGR 58): coalesced runs of one packed colour,
    /// boxed like `hyperlinks`. A sidecar rather than a `CellAttrs` field keeps
    /// the attr wire format fixed and costs nothing per styled run.
    underline_colors: Option<Box<SmallVec<UnderlineColorSpan, 2>>>,
    /// Inline-image spans: one per run of columns painting one footprint row, each
    /// holding the shared payload `Arc`; boxed like `hyperlinks`.
    ///
    /// Without this a picture would not survive scrolling off: the live grid keeps
    /// per-cell `ImageRef`s in a side table the row leaves behind. One inline slot:
    /// a line carries one placement's row unless an overwrite split it.
    images: Option<Box<SmallVec<ImageSpan, 1>>>,
}

impl Clone for Line {
    // Hand-written only so the skip can attach (a derive cannot carry it); the
    // body is the derive expansion.
    //
    // Skip: a deep clone is allocation-total (each field clone fails only on
    // allocation failure, the idiomatic-alloc class), and its std `Clone::clone`
    // callees are unbindable for the verifier. With the skip, callers get an
    // expected-absent-callee assumption row instead of an unprovable fatal.
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
    /// Line flags for metadata.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub(crate) struct LineFlags: u8 {
        /// Line is wrapped (continuation of previous line).
        const WRAPPED = 1 << 0;
        // Bits 1-7 unused.
    }
}

impl Line {
    /// Create a new empty line.
    ///
    /// ENSURES: self.is_empty()
    /// ENSURES: !self.has_attrs()
    /// ENSURES: !self.has_hyperlinks()
    /// ENSURES: !self.is_wrapped()
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a line from bytes (no attributes).
    ///
    /// ENSURES: self.len() == bytes.len()
    /// ENSURES: !self.has_attrs()
    /// ENSURES: !self.has_hyperlinks()
    #[must_use]
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

    /// Create a line with text and RLE-compressed attributes.
    ///
    /// The attrs RLE should have the same length as the character count in text.
    #[must_use]
    pub fn with_attrs(text: &str, attrs: Rle<CellAttrs>) -> Self {
        Self::from_parts(LineContent::from_bytes(text.as_bytes()), attrs, Vec::new())
    }

    /// Create a line with text, attributes, and hyperlinks preserved from the grid.
    #[must_use]
    pub fn with_hyperlinks(
        text: &str,
        attrs: Rle<CellAttrs>,
        hyperlinks: Vec<HyperlinkSpan>,
    ) -> Self {
        Self::from_parts(LineContent::from_bytes(text.as_bytes()), attrs, hyperlinks)
    }

    /// [`with_hyperlinks`](Self::with_hyperlinks) taking ownership of the text.
    ///
    /// Row-to-line conversion builds the text into a `String` it then drops;
    /// handing it over saves a second allocation and copy for every line over 32
    /// bytes. The stored bytes are identical.
    #[must_use]
    pub fn with_hyperlinks_owned(
        text: String,
        attrs: Rle<CellAttrs>,
        hyperlinks: Vec<HyperlinkSpan>,
    ) -> Self {
        Self::from_parts(LineContent::from_vec(text.into_bytes()), attrs, hyperlinks)
    }

    /// Get the RLE-compressed attributes, if any.
    #[must_use]
    #[inline]
    pub fn attrs(&self) -> Option<&Rle<CellAttrs>> {
        self.attrs.as_deref()
    }

    /// Get the attribute for a specific character index.
    ///
    /// Returns default attributes if the line has no stored attributes
    /// or if the index is out of bounds.
    ///
    /// ENSURES: !self.has_attrs() implies result == CellAttrs::DEFAULT
    #[must_use]
    pub fn get_attr(&self, char_idx: usize) -> CellAttrs {
        let idx = u32::try_from(char_idx).unwrap_or(u32::MAX);
        // Linear walk of the runs instead of `rle.get(idx)`: the same run (default
        // past the total length) in the gate-provable slice shape (see
        // `attr_runs`). The `saturating_add` never fires: the cumulative sum fits
        // in u32 by the Rle invariant. Runs per line are few.
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

    /// Sequential attr reader over this line's runs.
    ///
    /// [`get_attr`](Self::get_attr) rescans from the start on every call, so a
    /// per-cell walk costs O(cols × runs). The cursor remembers the last run:
    /// non-decreasing indices cost O(runs) per line in total, and a backward index
    /// rewinds. Same result as `get_attr` at every index.
    #[must_use]
    pub fn attr_cursor(&self) -> AttrRunCursor<'_> {
        AttrRunCursor {
            runs: attr_runs(self.attrs.as_deref()),
            i: 0,
            run_start: 0,
        }
    }

    /// Check if this line has styled content (non-default attributes).
    #[must_use]
    #[inline]
    #[cfg(test)]
    pub fn has_attrs(&self) -> bool {
        self.attrs.is_some()
    }

    /// Get the hyperlink URL at a column, if any.
    #[must_use]
    pub fn get_hyperlink(&self, col: u16) -> Option<&Arc<str>> {
        // Explicit loop over the plain slice: the Trust lane (`ship trust run`) proves slice
        // iteration, while the MIR-inlined `SmallVec::iter` internals fail its
        // hardened-unsafe check.
        for span in self.hyperlinks()? {
            if span.contains(col) {
                return Some(&span.url);
            }
        }
        None
    }

    /// Get the full hyperlink span (URL and OSC 8 ID) at a column, if any.
    #[must_use]
    #[allow(
        clippy::manual_find,
        reason = "explicit slice loop is the lowerable form for the Trust lane (`ship trust run`); the `.iter().find(..)` rewrite MIR-inlines SmallVec::iter internals its hardened-unsafe check fails on"
    )]
    pub fn get_hyperlink_span(&self, col: u16) -> Option<&HyperlinkSpan> {
        // Explicit slice loop (see `get_hyperlink`).
        for span in self.hyperlinks()? {
            if span.contains(col) {
                return Some(span);
            }
        }
        None
    }

    /// Check if this line has any hyperlinks.
    #[must_use]
    #[inline]
    pub fn has_hyperlinks(&self) -> bool {
        // Closure-free match over the plain slice: slice `is_empty` is a primitive
        // the gate proves, while the MIR-inlined `SmallVec::is_empty` internals
        // fail its hardened-unsafe check.
        match self.hyperlinks() {
            Some(spans) => !spans.is_empty(),
            None => false,
        }
    }

    /// Get the number of hyperlink spans.
    #[must_use]
    #[inline]
    pub fn hyperlink_count(&self) -> usize {
        // Closure-free match (see `has_hyperlinks`).
        match self.hyperlinks() {
            Some(spans) => spans.len(),
            None => 0,
        }
    }

    /// Get the hyperlink spans.
    #[must_use]
    #[inline]
    pub fn hyperlinks(&self) -> Option<&[HyperlinkSpan]> {
        self.hyperlinks.as_deref().map(SmallVec::as_slice)
    }

    /// Drop every hyperlink span and nothing else: text, attrs, flags, underline
    /// colours and images stay, so the line renders the same with no link under it.
    ///
    /// A self-update carries each screen line as one record the wire bounds
    /// (`16 KiB + cols * 512` bytes), and a few OSC 8 links with 8 KiB URLs exceed
    /// that; dropping only the links loses that line's link destinations rather
    /// than the whole screen. `hyperlinks` goes back to `None`, so the line
    /// serializes exactly like one that never had a link.
    ///
    /// ENSURES: !self.has_hyperlinks()
    // Skip: drop glue for the replaced Option<Box<SmallVec>> sidecar (std/alloc
    // internals, the drop-glue lane). Field replacement only; unit-tested.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn clear_hyperlinks(&mut self) {
        self.hyperlinks = None;
    }

    /// Get the underline-colour spans (SGR 58), if any.
    #[must_use]
    #[inline]
    pub fn underline_colors(&self) -> Option<&[UnderlineColorSpan]> {
        self.underline_colors.as_deref().map(SmallVec::as_slice)
    }

    /// Set the underline-colour spans, preserving them when this line scrolls
    /// into (and back out of) scrollback. Empty input stores `None` so the
    /// common no-SGR-58 case keeps its niche pointer and serializes as v3.
    // Skip: sidecar drop glue (see `clear_hyperlinks`).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn set_underline_colors(&mut self, spans: Vec<UnderlineColorSpan>) {
        self.underline_colors = if spans.is_empty() {
            None
        } else {
            Some(Box::new(SmallVec::from_vec(spans)))
        };
    }

    /// Get the packed underline colour (`0xTT_XXXXXX`) at a column, if any.
    #[must_use]
    pub fn get_underline_color(&self, col: u16) -> Option<u32> {
        // Explicit slice loop (see `get_hyperlink`).
        for span in self.underline_colors()? {
            if span.contains(col) {
                return Some(span.color);
            }
        }
        None
    }

    /// Check if this line has any underline colours.
    #[must_use]
    #[inline]
    #[cfg(test)]
    pub fn has_underline_colors(&self) -> bool {
        // Closure-free match (see `has_hyperlinks`).
        match self.underline_colors() {
            Some(spans) => !spans.is_empty(),
            None => false,
        }
    }

    /// Get the number of underline-colour spans.
    #[must_use]
    #[inline]
    #[cfg(test)]
    pub fn underline_color_count(&self) -> usize {
        // Closure-free match (see `has_hyperlinks`).
        match self.underline_colors() {
            Some(spans) => spans.len(),
            None => 0,
        }
    }

    /// Get the inline-image spans, if any.
    #[must_use]
    #[inline]
    pub fn images(&self) -> Option<&[ImageSpan]> {
        self.images.as_deref().map(SmallVec::as_slice)
    }

    /// Set the inline-image spans, kept when this line scrolls into and back out of
    /// scrollback. Empty input stores `None`, keeping the niche pointer.
    // Skip: sidecar drop glue (see `clear_hyperlinks`).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn set_images(&mut self, spans: Vec<ImageSpan>) {
        self.images = if spans.is_empty() {
            None
        } else {
            Some(Box::new(SmallVec::from_vec(spans)))
        };
    }

    /// Get the inline-image span covering a column, if any.
    #[must_use]
    #[allow(
        clippy::manual_find,
        reason = "explicit slice loop is the lowerable form for the Trust lane (`ship trust run`); the `.iter().find(..)` rewrite MIR-inlines SmallVec::iter internals its hardened-unsafe check fails on"
    )]
    #[cfg(test)]
    pub fn get_image(&self, col: u16) -> Option<&ImageSpan> {
        // Explicit slice loop (see `get_hyperlink`).
        for span in self.images()? {
            if span.contains(col) {
                return Some(span);
            }
        }
        None
    }

    /// Check if this line carries any inline image.
    #[must_use]
    #[inline]
    pub fn has_images(&self) -> bool {
        // Closure-free match (see `has_hyperlinks`).
        match self.images() {
            Some(spans) => !spans.is_empty(),
            None => false,
        }
    }

    /// Get the number of inline-image spans.
    #[must_use]
    #[inline]
    pub fn image_count(&self) -> usize {
        // Closure-free match (see `has_hyperlinks`).
        match self.images() {
            Some(spans) => spans.len(),
            None => 0,
        }
    }

    /// Get the content as bytes.
    #[must_use]
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        self.content.as_bytes()
    }

    /// Get the length in bytes.
    #[must_use]
    #[inline]
    pub fn len(&self) -> usize {
        self.content.len()
    }

    /// Check if empty.
    ///
    /// ENSURES: result == (self.len() == 0)
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.content.is_empty()
    }

    /// Check if wrapped.
    #[must_use]
    #[inline]
    pub fn is_wrapped(&self) -> bool {
        self.flags.contains(LineFlags::WRAPPED)
    }

    /// Set wrapped flag.
    ///
    /// ENSURES: self.is_wrapped() == wrapped
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

    /// A clone for the rewrap passthrough: identical content, attrs and sidecars,
    /// with the line flags reset (`WRAPPED` false), as the rewrap's `build_line`
    /// leaves the first row of a rebuilt line. Lives here because the flags are
    /// `pub(crate)`.
    // Skip: field-wise clone plus a flag reset, the idiomatic-alloc class
    // `Clone for Line` already skips.
    #[cfg_attr(trust_verify, trust::skip)]
    #[must_use]
    pub fn cloned_for_rewrap(&self) -> Line {
        let mut clone = self.clone();
        clone.flags = LineFlags::empty();
        clone
    }

    /// Get content as a string slice (returns None if not valid UTF-8).
    #[must_use]
    // Skip: `str::from_utf8` is the hardened strict-reject class; non-UTF-8 content
    // reads as None and stays byte-exact via `as_bytes`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn as_str(&self) -> Option<&str> {
        std::str::from_utf8(self.as_bytes()).ok()
    }

    /// Calculate memory used by this line.
    ///
    /// Saturating throughout: every operand is memory this process holds, so the
    /// sum always fits; saturation only discharges the Trust lane's overflow
    /// obligations.
    #[must_use]
    pub fn memory_used(&self) -> usize {
        let base = std::mem::size_of::<Self>();
        let content_mem = match &self.content {
            LineContent::Inline(_) => 0, // Already counted in size_of
            LineContent::Heap(v) => v.capacity(),
        };
        // Closure-free shapes below, for the Trust lane (see `attr_runs`).
        let attrs_mem = if self.attrs.is_some() {
            // Boxed: the Rle struct and its runs Vec are both on the heap.
            std::mem::size_of::<Rle<CellAttrs>>().saturating_add(
                attr_runs(self.attrs.as_deref())
                    .len()
                    .saturating_mul(std::mem::size_of::<aterm_rle::Run<CellAttrs>>()),
            )
        } else {
            0
        };
        let hyperlinks_mem = match self.hyperlinks() {
            // The boxed SmallVec (heap, not in size_of::<Line>), spans spilled past
            // 2, and the URL and id strings.
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
        // Underline-colour spans: the boxed SmallVec plus runs spilled past the 2
        // inline. Spans are Copy, so there is no per-span heap to walk.
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
        // Inline images: the boxed SmallVec plus each span's struct and row share
        // of the payload (see `ImageSpan::memory_used`).
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

    /// Calculate the number of attribute runs (for compression stats).
    #[must_use]
    #[cfg(test)]
    pub fn attr_run_count(&self) -> usize {
        // `runs.len()` is `run_count()`, via the gate-provable access (see `attr_runs`).
        attr_runs(self.attrs.as_deref()).len()
    }
}

/// Sequential run cursor over a [`Line`]'s attribute RLE (see
/// [`Line::attr_cursor`]); borrows the runs, no allocation.
#[derive(Debug)]
pub struct AttrRunCursor<'a> {
    runs: &'a [aterm_rle::Run<CellAttrs>],
    /// Index of the run the cursor is parked on.
    i: usize,
    /// Cumulative char index where `runs[i]` starts.
    run_start: u32,
}

impl AttrRunCursor<'_> {
    /// The attribute at `char_idx`, identical to [`Line::get_attr`]; amortized
    /// O(1) for non-decreasing indices.
    #[must_use]
    pub fn attr_at(&mut self, char_idx: usize) -> CellAttrs {
        let idx = u32::try_from(char_idx).unwrap_or(u32::MAX);
        if idx < self.run_start {
            // Backward query: rewind (rare; the materialize walk is monotone).
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
    // Skip: `from_utf8_lossy` display conversion (hardened byte-loss class);
    // Display only — the byte-exact content stays in `as_bytes`.
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
