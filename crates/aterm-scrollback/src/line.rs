// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Line representation for scrollback storage.
//!
//! Lines can be stored in different formats depending on tier:
//! - Hot: Full Line with content + RLE-compressed attributes
//! - Warm/Cold: Serialized bytes (compressed)
//!
//! ## RLE Attribute Compression
//!
//! Terminal lines often have runs of cells with identical attributes (e.g.,
//! a prompt in one color, then text in another). RLE compression stores
//! `(style, count)` pairs instead of per-cell styles.
//!
//! Typical compression: 80-column line with 3 color regions → 3 runs vs 80 cells.

use aterm_alloc::SmallVec;
use aterm_rle::Rle;
use std::sync::Arc;

/// Maximum inline storage for line content (avoids heap allocation for short lines).
///
/// Tuned down from 128 (perf-memory): the hot tier stores `Line` structs
/// contiguously in a `VecDeque`, so the inline buffer is paid on *every* stored
/// line regardless of its actual length. 128 bytes inline made `Line` 304 bytes
/// even for a 5-char prompt. A 32-byte inline buffer keeps short prompts/words
/// allocation-free while shrinking the stored `Line` struct dramatically; longer
/// lines spill to a right-sized heap `Vec` (one allocation of exactly `len`).
/// This is a pure storage-location change — `as_bytes()`/`len()` and
/// serialization return byte-identical results either way.
const INLINE_SIZE: usize = 32;

// ============================================================================
// Cell Attributes for RLE Compression
// ============================================================================

/// Compressed cell attributes for RLE storage.
///
/// This is a compact representation of cell styling that can be efficiently
/// RLE-encoded. It captures the essential visual attributes:
/// - Foreground color (packed)
/// - Background color (packed)
/// - Cell flags (bold, italic, underline, etc.)
///
/// ## Memory Layout
///
/// ```text
/// ┌─────────────────────────────────────────────────────────────┐
/// │ fg: u32 (4 bytes) - Packed foreground color                 │
/// │   Format: 0xTT_RRGGBB where TT = type (default/indexed/rgb) │
/// ├─────────────────────────────────────────────────────────────┤
/// │ bg: u32 (4 bytes) - Packed background color                 │
/// │   Format: 0xTT_RRGGBB where TT = type (default/indexed/rgb) │
/// ├─────────────────────────────────────────────────────────────┤
/// │ flags: u16 (2 bytes) - Visual attribute flags               │
/// │   Bits 0-7: bold, dim, italic, underline, blink, inverse... │
/// └─────────────────────────────────────────────────────────────┘
/// Total: 10 bytes per unique style (vs 8 bytes per cell uncompressed)
/// ```
///
/// ## Compression Benefit
///
/// An 80-column line with plain text: 80 cells × 8 bytes = 640 bytes
/// With RLE (1 style run): ~15 bytes (10 bytes style + 5 bytes overhead)
///
/// An 80-column prompt line with 3 color regions:
/// - Uncompressed: 640 bytes
/// - RLE: ~45 bytes (3 runs × 10 bytes + overhead)
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
    /// Cell flags (bold, italic, underline, etc.) plus the main-cell WIDE bit.
    /// Excludes WIDE_CONTINUATION/PROTECTED, USES_STYLE_ID, and COMPLEX.
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
    /// 10), USES_STYLE_ID (bit 14), and COMPLEX (bit 15), which cannot be
    /// interpreted independently from their live-grid context.
    const VISUAL_FLAGS_MASK: u16 = 0x3BFF; // bits 0-9, 11-13

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
}

// ── Strict-gate access idiom for the attrs RLE ─────────────────────────────
//
// All attrs access below goes through `self.attrs.as_deref().map_or(&[],
// Rle::runs)` — the METHOD PASSED AS A FUNCTION ITEM to an Option combinator
// (exactly the shape `Line::hyperlinks` uses with `SmallVec::as_slice`, which
// the gate proves). A direct `rle.run_count()` / `rle.runs()` / `rle.get(..)`
// call gets MIR-inlined into the calling function together with the `Vec`
// internals it wraps, and the strict gate's hardened-unsafe check then fails
// closed on std's unsafe blocks inside the inlined body (whose SAFETY
// comments it cannot resolve across crates). The fn-item-through-`map_or`
// spelling keeps the callee a plain (non-inlined) cross-crate call —
// Conditional, like every other dependency call — with identical results:
// `runs.len()` IS `run_count()` (its exact definition), and a linear walk of
// `runs` yields exactly `Rle::get`'s value (see `get_attr`).

fn attr_runs(attrs: Option<&Rle<CellAttrs>>) -> &[aterm_rle::Run<CellAttrs>] {
    attrs.map_or(&[], Rle::runs)
}

#[path = "line_codec.rs"]
mod line_codec;
#[path = "line_codec_block.rs"]
mod line_codec_block;
// Re-exported publicly (B.3.2): the block codec is the on-the-wire form for
// `TerminalCheckpoint` grid bodies and must be callable from aterm-core.
pub(crate) use line_codec::{MAX_DECODE_PAGE_LINES, count_page_lines};
pub use line_codec::{
    deserialize_lines, deserialize_lines_strict, deserialize_lines_tail_strict,
    deserialize_page_lines, serialize_lines,
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

/// A scrollback line.
///
/// Contains the text content, RLE-compressed attributes, and metadata.
///
/// ## Attribute Compression
///
/// When lines scroll off the visible grid into scrollback, we preserve their
/// styling via RLE compression. This stores runs of identical attributes
/// instead of per-cell data.
///
/// Example: A line with "Hello " (green) + "World" (default):
/// - Text: "Hello World" (11 bytes)
/// - Attrs: [(green, 6), (default, 5)] (~24 bytes for 2 runs)
/// - vs uncompressed: 11 cells × 8 bytes = 88 bytes
#[derive(Debug, Default)]
pub struct Line {
    /// Line content (UTF-8 text).
    content: LineContent,
    /// RLE-compressed cell attributes (colors and flags per character).
    /// None if all cells have default attributes (optimization for plain text).
    ///
    /// Boxed: plain-text lines have no attrs, so the common path pays an 8-byte
    /// niche pointer instead of carrying the 56-byte `Rle` inline. Styled lines
    /// (the rarer case) pay one extra heap allocation. The hot tier stores
    /// `Line` structs in a `VecDeque`, so shrinking the struct directly reduces
    /// resident scrollback memory.
    attrs: Option<Box<Rle<CellAttrs>>>,
    /// Line flags.
    flags: LineFlags,
    /// Hyperlink spans (typically None or 1-3 spans per line).
    ///
    /// Boxed: most lines have no hyperlinks, so the common path pays an 8-byte
    /// niche pointer instead of an 88-byte inline `SmallVec`. When present, the
    /// boxed `SmallVec<HyperlinkSpan, 2>` still keeps ≤2 spans inline.
    hyperlinks: Option<Box<SmallVec<HyperlinkSpan, 2>>>,
    /// Underline-colour spans (SGR 58), coalesced runs of a shared packed
    /// colour. `None` for the overwhelming common case (no SGR 58) — same
    /// boxed-niche rationale as `hyperlinks`. A sidecar (rather than a
    /// `CellAttrs` field) keeps the hot RLE-attr wire format frozen; underline
    /// colour is rare enough that the per-styled-run cost of widening every
    /// attr run would not pay for itself.
    underline_colors: Option<Box<SmallVec<UnderlineColorSpan, 2>>>,
}

impl Clone for Line {
    // Hand-written ONLY so the skip can attach (a derive cannot carry it);
    // the body is exactly the derive expansion — field-wise clone.
    //
    // Skip: deep-cloning a line is allocation-total — every field clone
    // (String / Box / SmallVec) has no panic path of its own and can abort
    // only on allocation failure, the idiomatic-alloc-panic class this
    // campaign skips (`hex_encode` precedent). The absent std
    // `Clone::clone` callees inside are unbindable for the fail-closed
    // verifier; with the skip, callers demote to an expected-absent-callee
    // assumption row instead of inheriting an unprovable fatal.
    #[cfg_attr(trust_verify, trust::skip)]
    fn clone(&self) -> Self {
        Self {
            content: self.content.clone(),
            attrs: self.attrs.clone(),
            flags: self.flags,
            hyperlinks: self.hyperlinks.clone(),
            underline_colors: self.underline_colors.clone(),
        }
    }
}

aterm_types::bitflags! {
    /// Line flags for metadata.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub(crate) struct LineFlags: u8 {
        /// Line is wrapped (continuation of previous line).
        const WRAPPED = 1 << 0;
        /// Line contains search match.
        const HAS_MATCH = 1 << 1;
        /// Line has been modified.
        const DIRTY = 1 << 2;
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
        }
    }

    /// Shared body of the text constructors: the "don't store what is
    /// all-default / empty" normalization, applied to already-built content.
    ///
    /// Factored so the borrowing and owning forms cannot drift — they differ
    /// only in how `content` was produced.
    fn from_parts(
        content: LineContent,
        attrs: Rle<CellAttrs>,
        hyperlinks: Vec<HyperlinkSpan>,
    ) -> Self {
        // Optimization: if empty or all attrs are default, don't store them
        let is_all_default = attrs.run_count() == 0
            || (attrs.run_count() == 1
                && attrs.runs().first().is_some_and(|r| r.value.is_default()));

        let attrs = if is_all_default {
            None
        } else {
            Some(Box::new(attrs))
        };

        // Optimization: if no hyperlinks, don't allocate
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
        }
    }

    /// Create a line with text and RLE-compressed attributes.
    ///
    /// This is the primary constructor when converting from grid Row to scrollback Line.
    /// The attrs RLE should have the same length as the character count in text.
    #[must_use]
    pub fn with_attrs(text: &str, attrs: Rle<CellAttrs>) -> Self {
        Self::from_parts(LineContent::from_bytes(text.as_bytes()), attrs, Vec::new())
    }

    /// Create a line with text, attributes, and hyperlinks.
    ///
    /// This is the full constructor for preserving hyperlinks from the visible grid
    /// when lines scroll into scrollback.
    #[must_use]
    pub fn with_hyperlinks(
        text: &str,
        attrs: Rle<CellAttrs>,
        hyperlinks: Vec<HyperlinkSpan>,
    ) -> Self {
        Self::from_parts(LineContent::from_bytes(text.as_bytes()), attrs, hyperlinks)
    }

    /// [`with_hyperlinks`](Self::with_hyperlinks) taking OWNERSHIP of the text.
    ///
    /// The row→line materialization sites all build the text into a `String`
    /// they drop immediately afterwards, so the borrowing form makes every line
    /// over 32 bytes (any full-width output row) allocate a second buffer,
    /// memcpy into it, and free the first. Handing the buffer over instead
    /// makes that one allocation and zero copies, on the per-line path every
    /// row pays as it ages out of the ring. The stored bytes — and therefore
    /// serialization, search, and `memory_used()` — are identical.
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
        // Linear walk of the runs slice instead of `rle.get(idx)`: identical
        // result — `Rle::get` locates the run whose cumulative-length range
        // contains `idx` (None past `total_length == sum(run.length)`, the
        // Rle-maintained invariant) — but via the strict-gate-provable slice
        // shape (see `attr_runs`). The `saturating_add` can never fire under
        // that same invariant (the true cumulative sum fits in u32). Runs per
        // line are few (one per style change), so the scan stays cheap on the
        // paths that call this per cell.
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

    /// Sequential attr reader over this line's RLE runs (audit E6a).
    ///
    /// [`get_attr`](Self::get_attr) rescans the runs from the START on every
    /// call, so a per-cell materialization walk pays O(cols × runs) — the
    /// accidental attr term the scrolled-frame audit flagged. The cursor
    /// remembers the last run and its cumulative start: monotone
    /// non-decreasing indices (a left-to-right cell walk) advance it forward
    /// in amortized O(1) — O(runs) TOTAL per line — and a backward index
    /// rewinds to the start (correct, just unaccelerated). Byte-identical to
    /// `get_attr` at every index.
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
    pub fn has_attrs(&self) -> bool {
        self.attrs.is_some()
    }

    /// Get hyperlink URL at column, if any.
    ///
    /// Returns the URL of the hyperlink at the given column position,
    /// or None if no hyperlink exists at that column.
    #[must_use]
    pub fn get_hyperlink(&self, col: u16) -> Option<&Arc<str>> {
        // Iterate the plain slice (via `hyperlinks()`) with an explicit loop:
        // identical result to the previous `.iter().find(..).map(..)`, but the
        // strict Trust gate proves slice iteration directly, whereas the
        // MIR-inlined `SmallVec::iter` internals fail its hardened-unsafe check.
        for span in self.hyperlinks()? {
            if span.contains(col) {
                return Some(&span.url);
            }
        }
        None
    }

    /// Get the full hyperlink span at column, if any.
    ///
    /// Returns the span (including URL and OSC 8 ID) at the given column,
    /// or None if no hyperlink exists at that column.
    #[must_use]
    #[allow(
        clippy::manual_find,
        reason = "explicit slice loop is the lowerable form for the strict Trust gate; the `.iter().find(..)` rewrite MIR-inlines SmallVec::iter internals its hardened-unsafe check fails on"
    )]
    pub fn get_hyperlink_span(&self, col: u16) -> Option<&HyperlinkSpan> {
        // Explicit slice loop for the same strict-gate reason as
        // `get_hyperlink` (identical result to `.iter().find(..)`).
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
        // Closure-free match over the plain slice: identical to the previous
        // `.as_ref().is_some_and(|h| !h.is_empty())`, but slice `is_empty` is a
        // primitive the gate proves, while the MIR-inlined `SmallVec::is_empty`
        // (→ `Vec::len`) internals fail its hardened-unsafe check.
        match self.hyperlinks() {
            Some(spans) => !spans.is_empty(),
            None => false,
        }
    }

    /// Get the number of hyperlink spans.
    #[must_use]
    #[inline]
    pub fn hyperlink_count(&self) -> usize {
        // Closure-free match over the plain slice (see `has_hyperlinks`):
        // identical to the previous `.map_or(0, |h| h.len())`.
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

    /// Get the underline-colour spans (SGR 58), if any.
    #[must_use]
    #[inline]
    pub fn underline_colors(&self) -> Option<&[UnderlineColorSpan]> {
        self.underline_colors.as_deref().map(SmallVec::as_slice)
    }

    /// Set the underline-colour spans, preserving them when this line scrolls
    /// into (and back out of) scrollback. Empty input stores `None` so the
    /// common no-SGR-58 case keeps its niche pointer and serializes as v3.
    // Skip: the residual row is drop glue for the replaced Option<Box<SmallVec>>
    // sidecar (std/alloc internals through SmallVec — the drop-glue lane).
    // Field replacement only; unit-tested round-trip.
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
        // Explicit slice loop (same strict-gate rationale as `get_hyperlink`):
        // identical result to `.iter().find(..).map(..)`.
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
    pub fn has_underline_colors(&self) -> bool {
        // Closure-free match over the plain slice (see `has_hyperlinks`).
        match self.underline_colors() {
            Some(spans) => !spans.is_empty(),
            None => false,
        }
    }

    /// Get the number of underline-colour spans.
    #[must_use]
    #[inline]
    pub fn underline_color_count(&self) -> usize {
        // Closure-free match over the plain slice (see `hyperlink_count`).
        match self.underline_colors() {
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
        // Inherent `insert`/`remove` (not the `|=`/`-=` op traits): the
        // operator call sites render the std TRAIT path, which misses the
        // locally-bundled impl body and mints an absent-callee row; the
        // inherent methods resolve directly. Same bits, same result.
        if wrapped {
            self.flags.insert(LineFlags::WRAPPED);
        } else {
            self.flags.remove(LineFlags::WRAPPED);
        }
    }

    /// A clone for the rewrap passthrough (RFL-4a): identical content, attrs
    /// and sidecars, with the line FLAGS reset to the freshly-rebuilt state —
    /// `WRAPPED` false and the transient `HAS_MATCH`/`DIRTY` render/search
    /// bits cleared. That is exactly the flag state the rewrap's `build_line`
    /// gives every output line, so a passed-through line is indistinguishable
    /// from a rebuilt one (the flags are `pub(crate)`, which is why this
    /// lives here and not at the call site).
    // Skip: field-wise clone plus a flag reset — the idiomatic-alloc class
    // `Clone for Line` above already skips, nothing new can panic here.
    #[cfg_attr(trust_verify, trust::skip)]
    #[must_use]
    pub fn cloned_for_rewrap(&self) -> Line {
        let mut clone = self.clone();
        clone.flags = LineFlags::empty();
        clone
    }

    /// Get content as a string slice (returns None if not valid UTF-8).
    #[must_use]
    // Skip: `str::from_utf8` is the hardened strict-reject class; `.ok()` is
    // the fail-closed contract (non-UTF-8 content reads as None, bytes stay
    // byte-exact via `as_bytes`). Display-side accessor.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn as_str(&self) -> Option<&str> {
        std::str::from_utf8(self.as_bytes()).ok()
    }

    /// Calculate memory used by this line.
    ///
    /// All sums below are spelled `saturating_add`/`saturating_mul`: every
    /// operand is the byte size of memory this process actually holds, so the
    /// true total always fits in `usize` and the saturation can never fire on
    /// a real path — it just discharges the strict L0 gate's unconstrained-
    /// input overflow counterexamples.
    #[must_use]
    pub fn memory_used(&self) -> usize {
        let base = std::mem::size_of::<Self>();
        let content_mem = match &self.content {
            LineContent::Inline(_) => 0, // Already counted in size_of
            LineContent::Heap(v) => v.capacity(),
        };
        // Closure-free shapes below (`attr_runs` fn-item access + match, not
        // `map_or` closures): identical folds, but the strict gate's
        // hardened-unsafe check fails closed on the callee internals that
        // direct calls MIR-inline into closures (see `attr_runs`).
        let attrs_mem = if self.attrs.is_some() {
            // Boxed: the Rle struct now lives on the heap (no longer counted in
            // size_of::<Line>), plus the runs Vec it owns. `runs.len()` IS
            // `run_count()` (its exact definition).
            std::mem::size_of::<Rle<CellAttrs>>().saturating_add(
                attr_runs(self.attrs.as_deref())
                    .len()
                    .saturating_mul(std::mem::size_of::<aterm_rle::Run<CellAttrs>>()),
            )
        } else {
            0
        };
        let hyperlinks_mem = match self.hyperlinks() {
            // Boxed: the SmallVec now lives on the heap (no longer counted in
            // size_of::<Line>). We additionally count:
            // - Spilled heap allocation (if > 2 spans)
            // - Arc<str> heap allocations for URLs
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
        // Underline-colour spans: boxed SmallVec (heap when present) plus any
        // spilled runs past the 2 kept inline. Each span is Copy (no owned
        // allocations), so no per-span heap walk is needed (unlike hyperlinks).
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
        base.saturating_add(content_mem)
            .saturating_add(attrs_mem)
            .saturating_add(hyperlinks_mem)
            .saturating_add(underline_colors_mem)
    }

    /// Calculate the number of attribute runs (for compression stats).
    #[must_use]
    pub fn attr_run_count(&self) -> usize {
        // `runs.len()` IS `run_count()` (its exact definition), via the
        // strict-gate-provable fn-item access shape (see `attr_runs`); the
        // `None` arm's `&[]` has len 0, matching the previous `map_or(0, ..)`.
        attr_runs(self.attrs.as_deref()).len()
    }
}

/// Sequential run-cursor over a [`Line`]'s attribute RLE — see
/// [`Line::attr_cursor`]. Borrows the line's runs; cheap to construct (no
/// allocation), one per materialized row.
#[derive(Debug)]
pub struct AttrRunCursor<'a> {
    runs: &'a [aterm_rle::Run<CellAttrs>],
    /// Index of the run the cursor is parked on.
    i: usize,
    /// Cumulative char index where `runs[i]` starts.
    run_start: u32,
}

impl AttrRunCursor<'_> {
    /// The attribute at `char_idx` — byte-identical to
    /// [`Line::get_attr`] at every index, amortized O(1) for monotone
    /// non-decreasing indices. Same overflow discipline as `get_attr`: the
    /// `saturating_add` can never fire under the Rle total-length invariant.
    #[must_use]
    pub fn attr_at(&mut self, char_idx: usize) -> CellAttrs {
        let idx = u32::try_from(char_idx).unwrap_or(u32::MAX);
        if idx < self.run_start {
            // Backward query: rewind (rare — the materialize walk is
            // monotone; correctness over speed here).
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
        }
    }
}

impl std::fmt::Display for Line {
    // Skip: `from_utf8_lossy` display conversion (hardened byte-loss class);
    // Display only — the byte-exact content stays in `as_bytes`.
    #[cfg_attr(trust_verify, trust::skip)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `write_str` instead of `write!(f, "{}", ..)`: identical output, but
        // no `format_args!` expansion (whose embedded unsafe `fmt::Arguments`
        // construction the strict Trust gate cannot lower and fails closed on).
        f.write_str(&String::from_utf8_lossy(self.as_bytes()))
    }
}

#[cfg(test)]
#[path = "line_tests.rs"]
mod tests;
