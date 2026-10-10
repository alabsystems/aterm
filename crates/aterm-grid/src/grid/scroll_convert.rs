// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Row-to-line conversion for scrollback.
//!
//! Converts between visible grid rows and scrollback [`Line`]s for scroll
//! operations pushing rows to, or recovering rows from, the scrollback.
//!
//! ## Lazy scrollback promotion
//!
//! [`DeferredLine`] captures a grid row's raw cell bytes and extras at scroll
//! time with a memcpy, deferring the O(cols) text/attrs conversion until the
//! line is read, so burst output never read back skips the conversion.

use std::cell::OnceCell;
use std::collections::VecDeque;
use std::sync::Arc;

use aterm_alloc::SmallVec;
use aterm_rle::Rle;
use aterm_scrollback::{CellAttrs, HyperlinkSpan, ImageSpan, Line, UnderlineColorSpan};

use super::Grid;
use crate::Cell;
use crate::CellExtra;
use crate::CellFlags;
use crate::PackedColor;
use crate::Row;
use crate::StyleTable;
use crate::{CellCoord, CellExtras};

use std::sync::atomic::{AtomicBool, Ordering};

/// Process-global headless scrollback-text-only toggle (default off). When on,
/// the scroll path skips per-cell extras extraction, so scrollback keeps text
/// but not colour/style: a throughput win for headless embeddings that read
/// scrollback as text. Process-global because an embedding is uniformly
/// headless or GUI; aterm's GUI never enables it.
static SCROLLBACK_TEXT_ONLY: AtomicBool = AtomicBool::new(false);

/// Enable/disable the headless scrollback-text-only fast path (see
/// [`SCROLLBACK_TEXT_ONLY`]). Off by default, so the GUI and the differential
/// oracle keep full-fidelity scrollback.
pub fn set_scrollback_text_only(enabled: bool) {
    SCROLLBACK_TEXT_ONLY.store(enabled, Ordering::Relaxed);
}

/// Whether the headless scrollback-text-only fast path is active.
#[must_use]
pub fn scrollback_text_only() -> bool {
    SCROLLBACK_TEXT_ONLY.load(Ordering::Relaxed)
}

/// Whether `cells[idx]` is a genuine wide-character continuation spacer (the
/// blank right half of a CJK glyph), as opposed to a DECSCA-protected cell.
///
/// `PROTECTED` and `WIDE_CONTINUATION` share bit 10, so the raw
/// `Cell::is_wide_continuation()` returns `true` for both. Materialization
/// skips spacers; skipping protected cells too would drop them from
/// scrollback (and `line`/search/copy of history). A true spacer has bit 10
/// set, is not itself `WIDE`, and follows a `WIDE` main cell.
#[inline]
// `pub(in crate::grid)`: the ring fast materializer
// (`scroll_materialize::materialize_from_row_extras`) must skip exactly the
// cells this skips, so it shares this predicate.
pub(in crate::grid) fn is_spacer(cells: &[Cell], idx: usize) -> bool {
    cells[idx].is_wide_continuation()
        && !cells[idx].is_wide()
        && idx > 0
        && cells[idx - 1].is_wide()
}

/// The flags that make a cell need the per-cell path: a complex codepoint
/// lives in the overflow table, a wide cell or a continuation spacer makes
/// [`is_spacer`] consult its neighbour, and `PROTECTED` shares its bit with
/// `WIDE_CONTINUATION` so it rides along (conservatively — a protected cell
/// takes the per-cell path).
const RUN_DISQUALIFY: CellFlags =
    CellFlags(CellFlags::COMPLEX.0 | CellFlags::WIDE.0 | CellFlags::WIDE_CONTINUATION.0);

/// A cell's plain printable-ASCII byte, or `None` when it needs the per-cell
/// path. `char_data` is a BMP codepoint whenever `COMPLEX` is clear, so this
/// is one mask and one range test.
#[inline]
fn plain_ascii_byte(cell: &Cell) -> Option<u8> {
    if cell.flags().intersects(RUN_DISQUALIFY) {
        return None;
    }
    let c = cell.char() as u32;
    (0x20..0x80).contains(&c).then_some(c as u8)
}

/// The attributes of `cell` exactly as the per-cell loop computed them.
#[inline]
fn cell_attrs_of(cell: &Cell) -> CellAttrs {
    let fg_raw = cell.fg_color().map_or(PackedColor::DEFAULT_FG.0, |c| c.0);
    let bg_raw = cell.bg_color().map_or(PackedColor::DEFAULT_BG.0, |c| c.0);
    CellAttrs::from_raw(fg_raw, bg_raw, cell.flags().bits())
}

/// Append the maximal run of cells at `from` that need no per-cell decision,
/// and return where it ended.
///
/// Ordinary output is runs of printable ASCII under one attribute, so a run is
/// found once and costs one `push_str` and one [`AttrRunBuilder::extend`]
/// instead of a per-cell [`is_spacer`] peek, `char()` decode, colour maps and
/// RLE compare. Cells that do not qualify (wide, complex, a continuation
/// spacer, a non-ASCII or control codepoint, or an attribute change) fall
/// through to the caller's per-cell body, so the emitted `Line` is identical.
#[inline]
fn append_plain_run(
    cells: &[Cell],
    from: usize,
    text: &mut String,
    attrs_rle: &mut AttrRunBuilder,
) -> usize {
    let Some(first) = plain_ascii_byte(&cells[from]) else {
        return from;
    };
    let attrs = cell_attrs_of(&cells[from]);
    let flags = cells[from].flags().bits();
    // A stack chunk keeps the text append to one `push_str` per 64 cells
    // instead of one `push` per cell; every byte in it is ASCII by
    // construction, so the `from_utf8` is infallible and the caller's
    // `String` stays valid UTF-8.
    let mut chunk = [0u8; 64];
    chunk[0] = first;
    let mut n = 1usize;
    let mut end = from + 1;
    while end < cells.len() {
        let cell = &cells[end];
        if cell.flags().bits() != flags {
            break;
        }
        let Some(byte) = plain_ascii_byte(cell) else {
            break;
        };
        if cell_attrs_of(cell) != attrs {
            break;
        }
        if n == chunk.len() {
            text.push_str(std::str::from_utf8(&chunk).unwrap_or_default());
            n = 0;
        }
        chunk[n] = byte;
        n += 1;
        end += 1;
    }
    text.push_str(std::str::from_utf8(&chunk[..n]).unwrap_or_default());
    attrs_rle.extend(attrs, u32::try_from(end - from).unwrap_or(u32::MAX));
    end
}

/// Whether any cell in `cells` carries something
/// [`Grid::extract_row_extras_into`] could extract without the extras map: a
/// style id, a complex-char codepoint, or an RGB overflow colour.
///
/// Does not test `Cell::has_extras()`: this is consulted only when the map
/// holds no entry on the row, where every lookup returns `None` regardless,
/// so ignoring the flag is exact and a stale flag (bulk clears leave them)
/// cannot defeat the gate.
///
/// Branch-free fold (`|`, not `||`): the common row has nothing and visits
/// every cell anyway, so accumulating beats an unpredictable short-circuit.
#[inline]
fn row_has_extractable_cells(cells: &[Cell]) -> bool {
    let mut found = false;
    for cell in cells {
        found |= cell.uses_style_id()
            | cell.is_complex()
            | cell.fg_needs_overflow()
            | cell.bg_needs_overflow();
    }
    found
}

// Compile-time guard: DeferredLine depends on Cell being exactly 8 bytes.
const _: () = assert!(std::mem::size_of::<Cell>() == 8);

/// Compact snapshot of a grid row's raw cell data, taken at scroll time.
///
/// Stores raw cell bytes + extras without the O(cols) text/attrs conversion
/// that `row_to_line_with_stored_extras` performs. Conversion to [`Line`]
/// happens lazily on first access via [`to_line`](Self::to_line).
///
/// ## Memory
///
/// For an 80-column ASCII row: ~640 bytes of cell data plus the
/// `ScrolledRowExtras` (often `None`), larger than the ~160-byte `Line`, but
/// short-lived: it converts on first read or drains in bulk when the lazy
/// buffer passes its threshold.
#[derive(Debug, Clone)]
pub(crate) struct DeferredLine {
    /// Raw cell data, copied from the Row at scroll time.
    /// Each cell is 8 bytes (repr(C, packed)).
    cells: Vec<Cell>,
    /// Number of occupied cells (Row::len equivalent).
    len: u16,
    /// Preserved extras (hyperlinks, complex chars, combining marks, RGB).
    /// `None` for the common case of plain-text rows (avoids 120-byte alloc).
    extras: Option<Box<ScrolledRowExtras>>,
    /// Whether the row was wrapped (soft line continuation).
    wrapped: bool,
    /// Cached materialized Line. Populated on first access.
    cached: OnceCell<Line>,
}

impl DeferredLine {
    /// Create a deferred line by snapshotting a Row's cells into `scratch`, a
    /// recycled cell body from the [`CellPool`] (or `Vec::new()`).
    ///
    /// O(cells) memcpy, avoiding text extraction, RLE attribute building and
    /// String allocation. Using a pooled body keeps malloc/free out of the PTY
    /// reader's `term_lock` hold, which the UI's keystroke echo waits on.
    /// `clear` + `extend_from_slice` fully defines the body from the row, so a
    /// recycled body carries no state from its previous line.
    pub(crate) fn new(row: &Row, extras: ScrolledRowExtras, scratch: Vec<Cell>) -> Self {
        // Box only when there is something to carry — the `None ⟺ empty`
        // encoding `heap_memory_used` and the materialize readers depend on.
        Self::new_boxed(row, (!extras.is_empty()).then(|| Box::new(extras)), scratch)
    }

    /// [`new`](Self::new) for a caller that already owns the extras in a `Box`.
    ///
    /// The tiered scroll-off path pops the evicted row's extras out of
    /// `ring_extras` as an `Option<Box<ScrolledRowExtras>>` and passes them
    /// here, avoiding an unbox/re-box per scrolled line inside the reader's
    /// `term_lock` hold.
    pub(crate) fn new_boxed(
        row: &Row,
        extras: Option<Box<ScrolledRowExtras>>,
        mut scratch: Vec<Cell>,
    ) -> Self {
        // A line stores content only, so a recorded early-wrap hole stays out
        // of it, also when the wrap blanked it with a background colour (the
        // row's length then covers it). The hole is then the cell the line
        // leaves unfilled, which a row rebuilt from it records again
        // (`fill_row_cells`).
        let len = if row.has_early_wrap_hole() {
            row.len().min(row.cols().saturating_sub(1))
        } else {
            row.len()
        };
        scratch.clear();
        if len != 0 {
            scratch.extend_from_slice(&row.as_slice()[..len as usize]);
        }
        Self {
            cells: scratch,
            len,
            // `filter` keeps the `None ⟺ empty` encoding: an empty box is
            // dropped rather than stored, which `heap_memory_used` and the
            // `materialize` readers rely on.
            extras: extras.filter(|b| !b.is_empty()),
            wrapped: row.is_wrapped(),
            cached: OnceCell::new(),
        }
    }

    /// Get or compute the materialized [`Line`].
    ///
    /// First call performs the O(cols) conversion; subsequent calls return
    /// the cached result. Uses `OnceCell` for interior mutability.
    pub(crate) fn to_line(&self) -> &Line {
        self.cached.get_or_init(|| self.materialize())
    }

    /// Estimated heap bytes owned by this deferred line (excluding
    /// `size_of::<DeferredLine>()`, which the container counts via its
    /// capacity): the cell body, everything the extras retain (links, complex
    /// text, image shares; `ScrolledRowExtras::retained_memory_used`) and the
    /// materialized line once read.
    pub(crate) fn heap_memory_used(&self) -> usize {
        // Saturating throughout, as in `Grid::memory_used`.
        let mut total = self
            .cells
            .capacity()
            .saturating_mul(std::mem::size_of::<Cell>())
            .saturating_add(self.extras_bytes());
        if let Some(line) = self.cached.get() {
            total = total.saturating_add(line.memory_used());
        }
        total
    }

    /// What staging this line costs: its spine slot, its cell body's capacity
    /// (a recycled body can exceed the row) and everything its extras retain,
    /// so the flood path's memory check (`LazyBuffer::front_rows_over`) sees
    /// link payload. Fixed while staged except through
    /// [`strip_hyperlinks`](Self::strip_hyperlinks), which the buffer
    /// re-weighs. The lazily materialized `cached` line is excluded: it appears
    /// only on read, and a figure that moves under `&self` could not be kept as
    /// a running sum.
    pub(crate) fn staged_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(
                self.cells
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Cell>()),
            )
            .saturating_add(self.extras_bytes())
    }

    /// Everything the extras retain, boxed struct included.
    fn extras_bytes(&self) -> usize {
        self.extras
            .as_deref()
            .map_or(0, ScrolledRowExtras::retained_memory_used)
    }

    /// The OSC 8 payload this line holds (`ScrolledRowExtras::link_bytes`).
    pub(crate) fn link_bytes(&self) -> usize {
        self.extras
            .as_deref()
            .map_or(0, ScrolledRowExtras::link_bytes)
    }

    /// Drop this line's links, keeping its text, colours and images, and the
    /// materialized line built with them; returns the link bytes freed.
    pub(crate) fn strip_hyperlinks(&mut self) -> usize {
        let Some(extras) = self.extras.as_deref_mut() else {
            return 0;
        };
        let freed = extras.link_bytes();
        if freed == 0 {
            return 0;
        }
        extras.strip_hyperlinks();
        // The `None ⟺ empty` encoding `heap_memory_used` and the readers use.
        if extras.is_empty() {
            self.extras = None;
        }
        // A line materialized before the strip still holds the links' `Arc`s.
        self.cached = OnceCell::new();
        freed
    }

    /// Convert into an owned [`Line`], consuming the deferred line and its cell
    /// body. Test-only: production uses
    /// [`into_line_recycled`](Self::into_line_recycled), which returns the body
    /// to the pool.
    ///
    /// Returns the cached line if already materialized, otherwise converts.
    #[cfg(test)]
    pub(crate) fn into_line(self) -> Line {
        self.into_line_and_body().0
    }

    /// [`into_line`](Self::into_line), returning the freed cell body to `pool`
    /// for the next scroll-off. Every consumption site on the flood path uses
    /// this, so drained bodies are reused instead of freed and reallocated.
    pub(crate) fn into_line_recycled(self, pool: &mut CellPool) -> Line {
        let (line, body) = self.into_line_and_body();
        pool.put(body);
        line
    }

    /// Shared body of the two `into_line` forms: yields the materialized line
    /// and the cell `Vec` it was built from, for the caller to drop or pool.
    /// Taking the body out first lets the cached early-return recycle too.
    fn into_line_and_body(mut self) -> (Line, Vec<Cell>) {
        let body = std::mem::take(&mut self.cells);

        if let Some(line) = self.cached.into_inner() {
            return (line, body);
        }

        #[cfg(any(test, feature = "testing"))]
        super::count_row_to_line_op();

        // cached was empty — materialize from the cell data we still own.
        let default_extras = ScrolledRowExtras::default();
        let extras = self.extras.as_deref().unwrap_or(&default_extras);
        let len = self.len as usize;
        if len == 0 {
            let mut line = Line::new();
            if self.wrapped {
                line.set_wrapped(true);
            }
            attach_image_spans(&mut line, extras);
            return (line, body);
        }
        let cells = &body[..len];
        let line = if extras.is_empty() {
            Self::materialize_no_extras(cells, self.wrapped)
        } else {
            Self::materialize_with_extras(cells, extras, self.wrapped)
        };
        (line, body)
    }

    /// Perform the O(cols) conversion from raw cells to Line.
    fn materialize(&self) -> Line {
        #[cfg(any(test, feature = "testing"))]
        super::count_row_to_line_op();

        let default_extras = ScrolledRowExtras::default();
        let extras = self.extras.as_deref().unwrap_or(&default_extras);

        if self.len == 0 {
            let mut line = Line::new();
            if self.wrapped {
                line.set_wrapped(true);
            }
            attach_image_spans(&mut line, extras);
            return line;
        }

        let cells = &self.cells[..self.len as usize];

        if extras.is_empty() {
            return Self::materialize_no_extras(cells, self.wrapped);
        }

        Self::materialize_with_extras(cells, extras, self.wrapped)
    }

    /// Fast-path materialization for rows with no extras.
    fn materialize_no_extras(cells: &[Cell], wrapped: bool) -> Line {
        let mut text = String::with_capacity(cells.len());
        let mut attrs_rle = AttrRunBuilder::empty();

        let mut idx = 0;
        while idx < cells.len() {
            let run_end = append_plain_run(cells, idx, &mut text, &mut attrs_rle);
            if run_end > idx {
                idx = run_end;
                continue;
            }
            let cell = &cells[idx];
            if !is_spacer(cells, idx) {
                text.push(cell.char());
                attrs_rle.push(cell_attrs_of(cell));
            }
            idx += 1;
        }

        let mut line = Line::with_hyperlinks_owned(text, attrs_rle.finish(), Vec::new());
        if wrapped {
            line.set_wrapped(true);
        }
        line
    }

    /// Full materialization with extras (hyperlinks, complex chars, combining, RGB).
    fn materialize_with_extras(cells: &[Cell], extras: &ScrolledRowExtras, wrapped: bool) -> Line {
        let mut text = String::with_capacity(cells.len());
        let mut attrs_rle = AttrRunBuilder::empty();
        let mut cursors = RowToLineCursorState::default();

        for (physical_col, cell) in cells.iter().enumerate() {
            if is_spacer(cells, physical_col) {
                continue;
            }

            let col_u16 = u16::try_from(physical_col).unwrap_or(u16::MAX);
            let char_count = push_cell_text(&mut text, *cell, extras, &mut cursors, col_u16);
            let fg_raw = resolve_cell_color(
                cell.fg_needs_overflow() || cell.uses_style_id(),
                cell.fg_color().map_or(PackedColor::DEFAULT_FG.0, |c| c.0),
                &extras.rgb_fg,
                &mut cursors.rgb_fg_idx,
                col_u16,
                PackedColor::DEFAULT_FG.0,
            );
            let bg_raw = resolve_cell_color(
                cell.bg_needs_overflow() || cell.uses_style_id(),
                cell.bg_color().map_or(PackedColor::DEFAULT_BG.0, |c| c.0),
                &extras.rgb_bg,
                &mut cursors.rgb_bg_idx,
                col_u16,
                PackedColor::DEFAULT_BG.0,
            );

            let attrs = CellAttrs::from_raw(fg_raw, bg_raw, cell.flags().bits());
            push_repeated_attrs(&mut attrs_rle, attrs, char_count);
            push_combining_marks(
                &mut text,
                &mut attrs_rle,
                attrs,
                extras,
                &mut cursors,
                col_u16,
            );
        }

        let mut line =
            Line::with_hyperlinks_owned(text, attrs_rle.finish(), extras.hyperlinks.clone());
        if !extras.underline_colors.is_empty() {
            line.set_underline_colors(coalesce_underline_spans(&extras.underline_colors));
        }
        attach_image_spans(&mut line, extras);
        if wrapped {
            line.set_wrapped(true);
        }
        line
    }
}

/// Staging buffer for deferred scrollback lines.
///
/// Sits between the ring buffer and tiered scrollback in `GridStorage`.
/// Lines are pushed here as `DeferredLine` during scroll_up (O(1) memcpy)
/// and drained to tiered scrollback either:
/// - On demand when scrollback is accessed (read triggers materialization)
/// - In bulk when the buffer exceeds `DRAIN_THRESHOLD`
/// - At checkpoint/snapshot time
#[derive(Debug)]
pub(crate) struct LazyBuffer {
    /// Pending deferred lines, ordered oldest to newest.
    lines: VecDeque<DeferredLine>,
    /// Free-list of cell bodies recycled between deferred lines (see
    /// [`CellPool`]). Kept inside the buffer so every producer and consumer of
    /// a `DeferredLine` is on the recycling path, and the pool's bytes are
    /// counted in [`memory_used`](Self::memory_used).
    pool: CellPool,
    /// The hole the flood path has cut at the FRONT of `lines`, if one is open
    /// (see [`FloodCut`]). Logically the buffer's oldest row: `len`, `get_line`
    /// and both drains present it as one marker line ahead of `lines`.
    cut: Option<FloodCut>,
    /// Running sum of [`DeferredLine::staged_bytes`] over `lines`: what the
    /// staged rows actually hold, kept O(1) so the flood path can weigh the
    /// backlog against the store's memory headroom on every scrolled row.
    ///
    /// A staged row copies only the row's occupied cells (`Row::len`), so a
    /// flood of short lines costs a fraction of `rows × cols`; charging the full
    /// width would cap short-line floods in wide windows far below the budget.
    staged_bytes: usize,
    /// Running sum of [`DeferredLine::link_bytes`] over `lines`: the staged
    /// rows' share of the history link budget (`Grid::history_link_budget`).
    link_bytes: usize,
    /// `lines[..linkless_front]` hold no link bytes: where the next
    /// [`strip_oldest_links`](Self::strip_oldest_links) resumes, so a flood
    /// visits each staged row once. Front removals shift it down with the rows.
    linkless_front: usize,
    /// Rows staged since the last bounded drain opportunity took the count
    /// ([`take_arrivals`](Self::take_arrivals)): the stream's rate as the
    /// host's drain sees it, used by `Grid::trickle_lazy_bounded`. Arrivals,
    /// not drops: a cut opened late in an interval has dropped few rows while
    /// the stream still delivers thousands.
    arrivals_since_drain: u64,
}

/// A hole at the front of the staged lines (the lines the flood path dropped
/// from the oldest end via [`LazyBuffer::shed_oldest`] /
/// [`LazyBuffer::drop_oldest`]), shown as one marker row in their place
/// instead of closing up silently.
///
/// Kept inside the buffer because every reader of staged history already
/// walks it: `get_line` (reached by `Grid::try_get_history_line` without
/// draining), `len`, and the two drains used by the compression worker, the
/// reflow re-attach and the offload paths. A count kept beside the buffer
/// would have to be taught to each reader.
///
/// A count rather than a pre-built line: the flood path moves the cut once per
/// scrolled row under the term lock, so it must be an integer store. The
/// marker line is built on first read and discarded when the count moves.
#[derive(Debug, Default)]
struct FloodCut {
    /// Lines missing here that the configured line limit would still hold (what
    /// the marker says). Grows as the flood drops rows the limit would keep,
    /// and shrinks when the limit's window slides past the hole's oldest rows.
    /// A drain reaching the front closes the cut; a later drop opens a new cut
    /// behind the promoted lines.
    dropped: u64,
    /// The largest `dropped` this cut has reached: the grid's loss counter
    /// grows by each new peak, so a count that shrinks and grows back is not
    /// counted twice.
    peak: u64,
    /// The marker line for `dropped`, built lazily; reset by [`Self::set`].
    line: OnceCell<Line>,
}

impl FloodCut {
    /// The marker line for the current count.
    fn line(&self) -> &Line {
        self.line.get_or_init(|| flood_marker_line(self.dropped))
    }

    /// The marker line, owned — for a drain that promotes the cut into the store.
    fn into_line(self) -> Line {
        let dropped = self.dropped;
        self.line
            .into_inner()
            .unwrap_or_else(|| flood_marker_line(dropped))
    }

    /// Make the marker name `dropped` lines (any built marker line is then
    /// stale). Returns how far that raised the cut's peak: the lines to add to
    /// the grid's loss counter.
    fn set(&mut self, dropped: u64) -> u64 {
        if dropped != self.dropped {
            self.dropped = dropped;
            self.line = OnceCell::new();
        }
        let raised = dropped.saturating_sub(self.peak);
        self.peak = self.peak.max(dropped);
        raised
    }
}

/// The text every flood marker row starts with — a reader that needs to tell
/// the marker from content (tests, a driver scanning `line` output) matches on
/// this, not on the wording after the count.
pub(crate) const FLOOD_MARKER_PREFIX: &str = "— aterm dropped ";

/// The one row that stands in for `dropped` lines lost at a flood cut.
///
/// States that lines are gone, that output came too fast to keep, and that a
/// file is the way to keep a stream that large. Rendered dim (the grid has no
/// system-row style, and dim reads as "not the program's output"). Fits 80
/// columns for counts up to seven digits, count first so a clipped row still
/// shows it. Otherwise ordinary text, so `search`, `copy` and `line` find it
/// (searching `aterm dropped` explains a missing match). Built only from the
/// cut's own counter, so the count is always right.
pub(crate) fn flood_marker_line(dropped: u64) -> Line {
    let noun = if dropped == 1 { "line" } else { "lines" };
    let text = format!(
        "{FLOOD_MARKER_PREFIX}{dropped} {noun} here: output too fast to keep; save it to a file —"
    );
    let attrs = CellAttrs::new(
        CellAttrs::DEFAULT.fg,
        CellAttrs::DEFAULT.bg,
        CellFlags::DIM.bits(),
    );
    let mut rle = Rle::new();
    rle.extend_with(
        attrs,
        u32::try_from(text.chars().count()).unwrap_or(u32::MAX),
    );
    Line::with_hyperlinks_owned(text, rle, Vec::new())
}

/// Maximum number of deferred lines before automatic drain to tiered scrollback.
const DRAIN_THRESHOLD: usize = 1000;

/// Bounded free-list of `Vec<Cell>` bodies, recycled between deferred lines.
///
/// Every row scrolling off the ring becomes a `DeferredLine` with a cell
/// snapshot. Allocating it fresh would put one malloc and one free per newline
/// inside the PTY reader's `term_lock` hold for a whole read batch, which the
/// UI's keystroke echo waits behind. The bodies are transient, so handing them
/// to the next scroll-off removes the allocator from the per-newline path.
///
/// Bound: production takes one body per scrolled line; consumption returns
/// them in batches (the compression worker drains 256 at a time, the inline
/// drain up to `DRAIN_THRESHOLD`), so bodies in flight stay roughly constant.
/// Capped by buffer count and by total pooled cells, so a deep backlog or a
/// wide window cannot turn it into idle megabytes; what it holds is reported
/// through [`LazyBuffer::memory_used`].
#[derive(Debug, Default)]
pub(crate) struct CellPool {
    /// Emptied cell bodies awaiting reuse (LIFO: the most recently returned
    /// body is the most likely to still be cache-warm).
    bufs: Vec<Vec<Cell>>,
    /// Running sum of the pooled bodies' CAPACITIES, in cells. Maintained
    /// incrementally so both the budget check on the hot `put` and
    /// `memory_used` stay O(1) instead of walking the free-list.
    pooled_cells: usize,
}

/// Free-list depth cap, sized off the drain batch: the compression worker
/// returns 256 bodies per batch, so this holds two batches of headroom for a
/// worker that wakes late.
const POOL_MAX_BUFFERS: usize = 512;

/// Total pooled cells cap — 512 KiB of `Cell` bodies. The count cap alone is a
/// per-width bound (512 × cols × 8 B), which a 1000-column window would turn
/// into 4 MB of idle free-list; this makes the ceiling absolute.
const POOL_MAX_CELLS: usize = 64 * 1024;

impl CellPool {
    /// Take a body for a new deferred line, or a fresh empty `Vec` when the
    /// pool is dry (the pool is an optimization, never a requirement).
    #[inline]
    fn take(&mut self) -> Vec<Cell> {
        match self.bufs.pop() {
            Some(buf) => {
                // Capacity is immutable while a body sits in the pool, so this
                // exactly undoes the `put` that added it.
                self.pooled_cells = self.pooled_cells.saturating_sub(buf.capacity());
                buf
            }
            None => Vec::new(),
        }
    }

    /// Return a consumed body to the free-list, or drop it if that would push
    /// the pool past either cap.
    #[inline]
    fn put(&mut self, buf: Vec<Cell>) {
        let cap = buf.capacity();
        // A zero-capacity body owns no allocation, so pooling it buys nothing
        // and would let empty-row churn crowd real bodies out of the depth cap.
        if cap == 0
            || self.bufs.len() >= POOL_MAX_BUFFERS
            || self.pooled_cells + cap > POOL_MAX_CELLS
        {
            return;
        }
        self.pooled_cells += cap;
        self.bufs.push(buf);
    }

    /// Drop every pooled body AND the free-list's own backing allocation.
    fn clear(&mut self) {
        self.bufs = Vec::new();
        self.pooled_cells = 0;
    }

    /// Heap bytes held by the free-list (bodies + the `Vec<Vec<Cell>>` spine).
    fn memory_used(&self) -> usize {
        self.bufs
            .capacity()
            .saturating_mul(std::mem::size_of::<Vec<Cell>>())
            .saturating_add(
                self.pooled_cells
                    .saturating_mul(std::mem::size_of::<Cell>()),
            )
    }

    /// Test-only: pooled body count, for the recycling/bound tests.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.bufs.len()
    }
}

impl LazyBuffer {
    /// Create a new empty lazy buffer.
    pub(crate) fn new() -> Self {
        Self {
            lines: VecDeque::new(),
            pool: CellPool::default(),
            cut: None,
            staged_bytes: 0,
            link_bytes: 0,
            linkless_front: 0,
            arrivals_since_drain: 0,
        }
    }

    /// Push a pre-built deferred line to the back of the buffer. Test-only:
    /// production stages rows through [`push_row`](Self::push_row) so the cell
    /// body always comes from the pool.
    #[cfg(test)]
    pub(crate) fn push(&mut self, deferred: DeferredLine) {
        self.stage(deferred);
    }

    /// The one place a line joins `lines`, so no running count can miss one.
    #[inline]
    fn stage(&mut self, deferred: DeferredLine) {
        self.staged_bytes += deferred.staged_bytes();
        self.link_bytes += deferred.link_bytes();
        self.arrivals_since_drain = self.arrivals_since_drain.saturating_add(1);
        self.lines.push_back(deferred);
    }

    /// Rows staged since the previous call (see the `arrivals_since_drain`
    /// field), resetting the count: called once per bounded drain opportunity.
    pub(crate) fn take_arrivals(&mut self) -> u64 {
        std::mem::take(&mut self.arrivals_since_drain)
    }

    /// Snapshot `row` into a pooled cell body and stage it.
    ///
    /// The allocation-free counterpart of `push(DeferredLine::new(..))` and the
    /// form the scroll-off path uses (see [`CellPool`]).
    #[inline]
    pub(crate) fn push_row(&mut self, row: &Row, extras: ScrolledRowExtras) {
        let scratch = self.pool.take();
        self.stage(DeferredLine::new(row, extras, scratch));
    }

    /// [`push_row`](Self::push_row) for a caller that already owns the extras in
    /// a `Box` (the tiered scroll-off path, popping them from `ring_extras`);
    /// see [`DeferredLine::new_boxed`].
    #[inline]
    pub(crate) fn push_row_boxed(&mut self, row: &Row, extras: Option<Box<ScrolledRowExtras>>) {
        let scratch = self.pool.take();
        self.stage(DeferredLine::new_boxed(row, extras, scratch));
    }

    /// Number of pending lines: the staged rows plus the flood marker row when
    /// a cut is open (it is a real history row to every reader, so it counts
    /// like one — `scrollback_lines()` and the drains agree on it).
    #[inline]
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.lines.len() + usize::from(self.cut.is_some())
    }

    /// Whether the buffer is empty — no staged rows AND no open cut (a lone
    /// marker is still a row a drain must promote).
    #[inline]
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.cut.is_none()
    }

    /// Lines the open flood cut's marker names — `0` when no cut is open.
    #[inline]
    #[must_use]
    pub(crate) fn cut_dropped(&self) -> u64 {
        self.cut.as_ref().map_or(0, |cut| cut.dropped)
    }

    /// Staged CONTENT rows — [`len`](Self::len) without the marker row.
    #[inline]
    #[must_use]
    pub(crate) fn staged_rows(&self) -> usize {
        self.lines.len()
    }

    /// How many of the oldest staged rows must go for the rest to fit in
    /// `budget` bytes (as the `staged_bytes` field sums them), never taking the
    /// buffer below `keep` rows (the marker counts as one, as in
    /// [`len`](Self::len)).
    ///
    /// O(1) while the backlog fits; otherwise one step per row returned, each
    /// then dropped, so the flood path stays amortized O(1) per newline.
    #[must_use]
    pub(crate) fn front_rows_over(&self, budget: usize, keep: usize) -> usize {
        let mut excess = self.staged_bytes.saturating_sub(budget);
        let removable = self.len().saturating_sub(keep).min(self.lines.len());
        let mut n = 0;
        while excess > 0 && n < removable {
            excess = excess.saturating_sub(self.lines[n].staged_bytes());
            n += 1;
        }
        n
    }

    /// The running staged-byte sum (see the field). Test-only: pins that the
    /// sum always equals a fresh walk, whichever path removed the rows.
    #[cfg(test)]
    pub(crate) fn staged_bytes(&self) -> usize {
        self.assert_running_sums();
        self.staged_bytes
    }

    /// Test-only: both running sums equal a fresh walk, whichever path moved
    /// the rows, and the strip cursor's prefix holds no link.
    #[cfg(test)]
    pub(crate) fn assert_running_sums(&self) {
        assert_eq!(
            self.staged_bytes,
            self.lines
                .iter()
                .map(DeferredLine::staged_bytes)
                .sum::<usize>(),
            "the running staged-byte sum drifted from the rows it sums"
        );
        assert_eq!(
            self.link_bytes,
            self.lines
                .iter()
                .map(DeferredLine::link_bytes)
                .sum::<usize>(),
            "the running link-byte sum drifted from the rows it sums"
        );
        assert!(
            self.lines
                .range(..self.linkless_front.min(self.lines.len()))
                .all(|line| line.link_bytes() == 0),
            "a staged row before the strip cursor still holds a link"
        );
    }

    /// Whether a flood cut is open at the front (see [`FloodCut`]).
    #[inline]
    #[must_use]
    pub(crate) fn cut_open(&self) -> bool {
        self.cut.is_some()
    }

    /// Whether the buffer has exceeded the drain threshold.
    #[inline]
    #[must_use]
    pub(crate) fn should_drain(&self) -> bool {
        self.lines.len() > DRAIN_THRESHOLD
    }

    /// Drain all pending lines, converting each to a materialized [`Line`].
    ///
    /// Returns an iterator of Lines in oldest-to-newest order.
    pub(crate) fn drain_all(&mut self) -> impl Iterator<Item = Line> + '_ {
        // An open cut is the oldest row: it goes first, so the store gets the
        // marker where the hole is. Taking it closes the cut; a later drop opens
        // a new one behind these promoted lines.
        let marker = self.cut.take().map(FloodCut::into_line);
        // Every staged row leaves, consumed or not: `VecDeque::Drain` removes
        // the whole range when it is dropped.
        self.staged_bytes = 0;
        self.link_bytes = 0;
        self.linkless_front = 0;
        // Disjoint field borrows: the drain owns `lines`, the recycling owns
        // `pool`. Each materialized line hands its cell body back for the next
        // scroll-off to fill.
        let pool = &mut self.pool;
        marker.into_iter().chain(
            self.lines
                .drain(..)
                .map(move |deferred| deferred.into_line_recycled(&mut *pool)),
        )
    }

    /// Drain up to `n` of the oldest pending lines (front of the buffer),
    /// converting each to a materialized [`Line`], in oldest-to-newest order.
    ///
    /// The bounded counterpart of [`drain_all`](Self::drain_all), used by the
    /// off-thread compression worker so each term-lock hold stays short. `n` is
    /// clamped to the buffer length.
    pub(crate) fn drain_front(&mut self, n: usize) -> impl Iterator<Item = Line> + '_ {
        let n = n.min(self.len());
        // The marker is the oldest row, so a batch that takes anything takes
        // it first and it counts against `n` like the row it is (see
        // `drain_all` for why it must lead).
        let marker = if n > 0 {
            self.cut.take().map(FloodCut::into_line)
        } else {
            None
        };
        let n = n - usize::from(marker.is_some());
        // Weighed before the drain: the range leaves whether or not the
        // iterator is consumed, and it is O(n) like the drain itself.
        let (leaving, leaving_links) =
            self.lines.range(..n).fold((0, 0), |(bytes, links), line| {
                (bytes + line.staged_bytes(), links + line.link_bytes())
            });
        self.staged_bytes -= leaving;
        self.link_bytes -= leaving_links;
        self.linkless_front = self.linkless_front.saturating_sub(n);
        // The steady-state consumption site under flood: the worker's bounded
        // batch is exactly where the reader's per-newline bodies come back.
        let pool = &mut self.pool;
        marker.into_iter().chain(
            self.lines
                .drain(..n)
                .map(move |deferred| deferred.into_line_recycled(&mut *pool)),
        )
    }

    /// Get a line by index within the lazy buffer (0 = oldest — the flood
    /// marker when a cut is open, the oldest staged row otherwise).
    ///
    /// Triggers materialization via `OnceCell` on first access.
    #[must_use]
    pub(crate) fn get_line(&self, idx: usize) -> Option<&Line> {
        match &self.cut {
            Some(cut) if idx == 0 => Some(cut.line()),
            Some(_) => self.lines.get(idx - 1).map(DeferredLine::to_line),
            None => self.lines.get(idx).map(DeferredLine::to_line),
        }
    }

    /// Clear all pending lines, and release the recycling pool with them.
    ///
    /// Every caller invalidates history (scrollback erase, store detach with
    /// nothing to drain into, resize drain), never the flood path, so the
    /// memory goes back to the allocator. The pool refills on the next drain.
    pub(crate) fn clear(&mut self) {
        self.lines.clear();
        self.staged_bytes = 0;
        self.link_bytes = 0;
        self.linkless_front = 0;
        self.pool.clear();
        // The hole is part of the history being invalidated: a marker for
        // lines the user just erased would be a hole in nothing.
        self.cut = None;
    }

    /// Drop the recycled cell bodies without touching the staged lines.
    ///
    /// Called on a width change: pooled capacities were sized for the old
    /// column count, so reuse would waste memory or realloc. A footprint guard
    /// only: `DeferredLine::new` refills the body from the row slice.
    pub(crate) fn clear_pool(&mut self) {
        self.pool.clear();
    }

    /// Test-only: pooled cell-body count.
    #[cfg(test)]
    pub(crate) fn pooled_bodies(&self) -> usize {
        self.pool.len()
    }

    /// Drop the `n` oldest deferred lines without materializing them, as a
    /// loss: the hole opens (or widens) the buffer's flood cut, so readers see
    /// one marker row naming how many lines are missing. Bounds the buffer
    /// while the tiered store is detached for a reflow, where there is no
    /// telling which rows the limit would keep.
    pub(crate) fn drop_oldest(&mut self, n: usize) -> u64 {
        let n = self.discard_front(n) as u64;
        if n == 0 {
            return 0;
        }
        let cut = self.cut.get_or_insert_with(FloodCut::default);
        let dropped = cut.dropped.saturating_add(n);
        cut.set(dropped)
    }

    /// The flood path's drop: discard the `n` oldest staged rows (never the
    /// marker) and make the cut name `hole` lines — the missing lines the
    /// configured limit would still hold, as `Grid::drop_flood_overflow`
    /// works them out. `0` means the top of history has moved past any hole:
    /// an open marker goes, as the store's own trim evicts a promoted one, and
    /// the loss it named stays in the grid's counter. Otherwise a cut opens if
    /// none is open. Returns the lines to add to the loss counter (see
    /// [`FloodCut::set`]).
    pub(crate) fn shed_oldest(&mut self, n: usize, hole: u64) -> u64 {
        self.discard_front(n);
        if hole == 0 {
            self.cut = None;
            return 0;
        }
        self.cut.get_or_insert_with(FloodCut::default).set(hole)
    }

    /// Hand back the deque spine a flood grew: `VecDeque::drain` never shrinks,
    /// so a burst staging ~90k rows would otherwise keep ~10 MB of empty spine
    /// for the session. Shrinks only past 4× the larger of the live length and
    /// the drain threshold, and then to 2× it, so the steady inline drain never
    /// reallocates and a long post-flood drain returns memory in a few
    /// geometric steps. Call after a drain's iterator has been consumed.
    pub(crate) fn release_spine_slack(&mut self) {
        let floor = self.lines.len().max(DRAIN_THRESHOLD);
        if self.lines.capacity() > floor.saturating_mul(4) {
            self.lines.shrink_to(floor.saturating_mul(2));
        }
    }

    /// The deque spine's capacity, in rows. Test-only: pins that a drained
    /// flood backlog does not keep its peak allocation.
    #[cfg(test)]
    pub(crate) fn spine_capacity(&self) -> usize {
        self.lines.capacity()
    }

    /// The shared body of the two drops: discard up to `n` front rows, never
    /// the cut. Returns how many were discarded.
    fn discard_front(&mut self, n: usize) -> usize {
        let n = n.min(self.lines.len());
        // Recycle here too: this is the flood path, exactly when the reader is
        // taking a body per newline and the pool is most needed.
        let pool = &mut self.pool;
        for mut deferred in self.lines.drain(..n) {
            // Weighed before the body is taken: its capacity is part of it.
            self.staged_bytes -= deferred.staged_bytes();
            self.link_bytes -= deferred.link_bytes();
            pool.put(std::mem::take(&mut deferred.cells));
        }
        self.linkless_front = self.linkless_front.saturating_sub(n);
        n
    }

    /// The staged rows' OSC 8 payload (the running sum), O(1).
    #[inline]
    #[must_use]
    pub(crate) fn link_bytes(&self) -> usize {
        self.link_bytes
    }

    /// Strip the links of the oldest staged rows that hold any until at least
    /// `want` link bytes are freed or none are left; returns the bytes freed.
    /// The rows keep their text, colours and images, and their place: this
    /// opens no cut.
    pub(crate) fn strip_oldest_links(&mut self, want: usize) -> usize {
        let mut freed = 0;
        while freed < want && self.linkless_front < self.lines.len() {
            let line = &mut self.lines[self.linkless_front];
            self.linkless_front += 1;
            let before = line.staged_bytes();
            let links = line.strip_hyperlinks();
            if links == 0 {
                continue;
            }
            self.staged_bytes -= before - line.staged_bytes();
            self.link_bytes -= links;
            freed += links;
        }
        freed
    }

    /// Estimated bytes held by the staged (not yet drained) lines: the backing
    /// `VecDeque` capacity plus each deferred line's heap, so the ring byte
    /// watermark sees the backlog under flood backpressure. O(staged) per call;
    /// used only by poll-time accounting.
    pub(crate) fn memory_used(&self) -> usize {
        // Saturating throughout, as in `Grid::memory_used`.
        let mut total = self
            .lines
            .capacity()
            .saturating_mul(std::mem::size_of::<DeferredLine>());
        for line in &self.lines {
            total = total.saturating_add(line.heap_memory_used());
        }
        // Pooled bodies are real retained heap (bounded by POOL_MAX_CELLS).
        total = total.saturating_add(self.pool.memory_used());
        // An open cut is a counter plus, once read, one short line.
        if let Some(cut) = &self.cut {
            total = total
                .saturating_add(std::mem::size_of::<FloodCut>())
                .saturating_add(cut.line.get().map_or(0, Line::memory_used));
        }
        total
    }
}

impl Default for LazyBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Attach a row's coalesced inline-image spans to the [`Line`] it produced.
///
/// Called from every line-producing site, including the zero-length short
/// circuits: `place_image` writes no glyph, so an image-only row has `len == 0`
/// and would otherwise lose its picture between the ring and the store.
///
/// The clone is one refcount bump per span; the raster is never copied.
fn attach_image_spans(line: &mut Line, extras: &ScrolledRowExtras) {
    if !extras.images.is_empty() {
        line.set_images(extras.images.clone());
    }
}

/// Preserved CellExtras data for a ring buffer scrollback row.
///
/// When rows scroll from the visible grid into ring buffer scrollback, their
/// CellExtras (hyperlinks, complex chars, combining marks, RGB colors) are
/// extracted before `shift_rows_up_by` discards them.
///
/// Fields are sorted by physical column for cursor-based lookup during
/// line reconstruction.
#[derive(Debug, Clone, Default)]
pub struct ScrolledRowExtras {
    /// Hyperlink spans (coalesced from per-cell URLs).
    pub hyperlinks: Vec<HyperlinkSpan>,
    /// Complex character strings keyed by physical column.
    /// Only populated for cells where `is_complex()` is true.
    pub complex_chars: Vec<(u16, Arc<str>)>,
    /// Combining characters keyed by physical column.
    pub combining: Vec<(u16, SmallVec<char, 2>)>,
    /// Resolved foreground colors keyed by physical column.
    /// Populated for RGB overflow cells and StyleId cells (resolved at extraction).
    pub rgb_fg: Vec<(u16, [u8; 3])>,
    /// Resolved background colors keyed by physical column.
    /// Populated for RGB overflow cells and StyleId cells (resolved at extraction).
    pub rgb_bg: Vec<(u16, [u8; 3])>,
    /// Packed SGR 58 underline colours keyed by physical column
    /// (`0xTT_XXXXXX`; `0x01` = RGB, `0x02` = indexed). The packed form — not a
    /// resolved RGB triple — is kept so an indexed underline colour re-resolves
    /// against the live palette at render time, matching the live cell. Rare
    /// (SGR 58 is seldom used), so this is empty for essentially all rows.
    pub underline_colors: Vec<(u16, u32)>,
    /// Inline-image spans (iTerm2 OSC 1337 / sixel), coalesced into one span
    /// per contiguous run of columns painting one footprint row. An image
    /// covers a rectangle, so a span states a full-width footprint row with one
    /// `Arc` bump instead of one per column. Empty for rows without a picture.
    pub images: Vec<ImageSpan>,
}

impl ScrolledRowExtras {
    /// Estimated bytes retained by this boxed extras value, including vector
    /// capacities and out-of-line strings. Shared strings are conservatively
    /// charged per holder; image payloads use the same per-row share as lines.
    ///
    /// Every holder charges this — the ring (`RingExtras`), the lazy buffer's
    /// staged rows (`DeferredLine`) and the resize undo — so a row's links,
    /// complex text and pictures cost the same wherever it waits.
    pub(crate) fn retained_memory_used(&self) -> usize {
        // Saturating throughout, as in `Grid::memory_used`.
        fn capacity_bytes<T>(values: &Vec<T>) -> usize {
            values.capacity().saturating_mul(std::mem::size_of::<T>())
        }
        let mut total = std::mem::size_of::<Self>()
            .saturating_add(self.link_bytes())
            .saturating_add(capacity_bytes(&self.complex_chars))
            .saturating_add(capacity_bytes(&self.combining))
            .saturating_add(capacity_bytes(&self.rgb_fg))
            .saturating_add(capacity_bytes(&self.rgb_bg))
            .saturating_add(capacity_bytes(&self.underline_colors))
            .saturating_add(capacity_bytes(&self.images));
        for (_, text) in &self.complex_chars {
            total = total.saturating_add(text.len());
        }
        for (_, marks) in &self.combining {
            if marks.spilled() {
                total = total
                    .saturating_add(marks.capacity().saturating_mul(std::mem::size_of::<char>()));
            }
        }
        for span in &self.images {
            // The vector capacity above already includes ImageSpan itself.
            total = total.saturating_add(span.image.per_row_bytes());
        }
        total
    }

    /// True when all fields are empty (common case: plain ASCII text).
    ///
    /// Lets plain-text rows skip allocating a boxed extras struct.
    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.hyperlinks.is_empty()
            && self.complex_chars.is_empty()
            && self.combining.is_empty()
            && self.rgb_fg.is_empty()
            && self.rgb_bg.is_empty()
            && self.underline_colors.is_empty()
            && self.images.is_empty()
    }

    /// The OSC 8 share of [`retained_memory_used`](Self::retained_memory_used):
    /// the span vector's capacity plus each span's URL and `id=` bytes, which
    /// is what [`strip_hyperlinks`](Self::strip_hyperlinks) gives back.
    ///
    /// An `Arc` held by several spans is charged once per span, the convention
    /// `retained_memory_used` and the store's `Line::memory_used` share. It is
    /// an upper bound, and over-counts only one link covering several rows:
    /// each OSC 8 mints its own `Arc` (`handle_osc_8`), so a few hundred links
    /// to one URL are a few hundred real copies, while one link's run charges
    /// rows × its length. Under the 25 MiB default allowance
    /// (`Grid::history_link_budget`) one run must cover over 3,000 rows at the
    /// 8 KiB URL cap before it costs any link, and at a 100-byte URL more rows
    /// than any ring holds. Exact sharing would couple a row's charge to its
    /// neighbours' (or need a refcount walk) on the scroll path.
    #[must_use]
    pub(crate) fn link_bytes(&self) -> usize {
        // Saturating throughout, as in `Grid::memory_used`.
        let mut total = self
            .hyperlinks
            .capacity()
            .saturating_mul(std::mem::size_of::<HyperlinkSpan>());
        for span in &self.hyperlinks {
            total = total
                .saturating_add(span.url.len())
                .saturating_add(span.id.as_ref().map_or(0, |id| id.len()));
        }
        total
    }

    /// Drop this row's links, spans and vector both, keeping its text, colours
    /// and images: the history link budget's eviction (`Grid::history_link_budget`).
    #[inline]
    pub(crate) fn strip_hyperlinks(&mut self) {
        self.hyperlinks = Vec::new();
    }

    /// Clear all fields, retaining `Vec` capacities so the struct can be
    /// recycled as extraction scratch (see `extract_row_extras_into`).
    #[inline]
    pub(crate) fn clear(&mut self) {
        self.hyperlinks.clear();
        self.complex_chars.clear();
        self.combining.clear();
        self.rgb_fg.clear();
        self.rgb_bg.clear();
        self.underline_colors.clear();
        self.images.clear();
    }
}

/// Monotone read cursors over the four column-sorted `ScrolledRowExtras`
/// vectors. Each advances only when its next entry's column matches the
/// column being emitted, so an entry for a skipped (spacer) column or an
/// out-of-order entry is never consumed. The ring fast materializer shares
/// this state and consumers, so it behaves identically.
#[derive(Default)]
pub(in crate::grid) struct RowToLineCursorState {
    complex_idx: usize,
    combining_idx: usize,
    pub(in crate::grid) rgb_fg_idx: usize,
    pub(in crate::grid) rgb_bg_idx: usize,
}

impl Grid {
    /// Convert a Row to a Line without extras (test helper).
    ///
    /// Delegates to `row_to_line_with_stored_extras` with empty extras.
    #[cfg(test)]
    pub(crate) fn row_to_line_static(row: &Row) -> Line {
        Self::row_to_line_with_stored_extras(row, &ScrolledRowExtras::default())
    }

    /// Convert a Row to a Line using pre-extracted CellExtras data.
    ///
    /// Used for ring buffer scrollback rows whose extras were preserved in
    /// `ring_extras` at scroll time. RGB overflow and StyleId colors are
    /// pre-resolved into the extras vectors at extraction time.
    pub(crate) fn row_to_line_with_stored_extras(row: &Row, extras: &ScrolledRowExtras) -> Line {
        Self::row_to_line_with_stored_extras_at_len(row, extras, row.len())
    }

    /// [`Self::row_to_line_with_stored_extras`] with an explicit cell count.
    /// The width-reflow seams pass the full row width for a row whose logical
    /// line continues on the next row: autowrap filled it, so trailing blanks
    /// are content, and trimming to `row.len()` would erode one space per chunk
    /// boundary on every width sweep.
    pub(crate) fn row_to_line_with_stored_extras_at_len(
        row: &Row,
        extras: &ScrolledRowExtras,
        len: u16,
    ) -> Line {
        #[cfg(any(test, feature = "testing"))]
        super::count_row_to_line_op();

        let len = usize::from(len.min(row.cols()));
        if len == 0 {
            let mut line = Line::new();
            if row.is_wrapped() {
                line.set_wrapped(true);
            }
            attach_image_spans(&mut line, extras);
            return line;
        }

        // Fast path: no extras (common for plain text). Skips per-cell extras
        // lookup, overflow resolution, combining marks and hyperlink cloning.
        if extras.is_empty() {
            return Self::row_to_line_no_extras(row, len);
        }

        let mut text = String::with_capacity(len);
        let mut attrs_rle = AttrRunBuilder::empty();
        let mut cursors = RowToLineCursorState::default();

        let cells = &row.as_slice()[..len];
        for (physical_col, cell) in cells.iter().enumerate() {
            #[cfg(any(test, feature = "testing"))]
            super::count_row_to_line_cell();

            if is_spacer(cells, physical_col) {
                continue;
            }

            let col_u16 = u16::try_from(physical_col).unwrap_or(u16::MAX);
            let char_count = push_cell_text(&mut text, *cell, extras, &mut cursors, col_u16);
            let fg_raw = resolve_cell_color(
                cell.fg_needs_overflow() || cell.uses_style_id(),
                cell.fg_color().map_or(PackedColor::DEFAULT_FG.0, |c| c.0),
                &extras.rgb_fg,
                &mut cursors.rgb_fg_idx,
                col_u16,
                PackedColor::DEFAULT_FG.0,
            );
            let bg_raw = resolve_cell_color(
                cell.bg_needs_overflow() || cell.uses_style_id(),
                cell.bg_color().map_or(PackedColor::DEFAULT_BG.0, |c| c.0),
                &extras.rgb_bg,
                &mut cursors.rgb_bg_idx,
                col_u16,
                PackedColor::DEFAULT_BG.0,
            );

            let attrs = CellAttrs::from_raw(fg_raw, bg_raw, cell.flags().bits());
            push_repeated_attrs(&mut attrs_rle, attrs, char_count);
            push_combining_marks(
                &mut text,
                &mut attrs_rle,
                attrs,
                extras,
                &mut cursors,
                col_u16,
            );
        }

        let mut line =
            Line::with_hyperlinks_owned(text, attrs_rle.finish(), extras.hyperlinks.clone());
        if !extras.underline_colors.is_empty() {
            line.set_underline_colors(coalesce_underline_spans(&extras.underline_colors));
        }
        attach_image_spans(&mut line, extras);
        if row.is_wrapped() {
            line.set_wrapped(true);
        }
        line
    }

    /// Fast-path row-to-line for rows with no extras (no hyperlinks, complex
    /// chars, combining marks, or RGB overflow). Inlines all per-cell logic
    /// to avoid function call overhead and extras cursor tracking.
    fn row_to_line_no_extras(row: &Row, len: usize) -> Line {
        let cells = &row.as_slice()[..len];
        let mut text = String::with_capacity(len);
        let mut attrs_rle = AttrRunBuilder::empty();

        let mut idx = 0;
        while idx < cells.len() {
            // The census counts CELLS, not runs, so it is stepped for every
            // cell the walk covers whichever arm covers it.
            let run_end = append_plain_run(cells, idx, &mut text, &mut attrs_rle);
            if run_end > idx {
                #[cfg(any(test, feature = "testing"))]
                for _ in idx..run_end {
                    super::count_row_to_line_cell();
                }
                idx = run_end;
                continue;
            }

            #[cfg(any(test, feature = "testing"))]
            super::count_row_to_line_cell();

            let cell = &cells[idx];
            if !is_spacer(cells, idx) {
                // No complex chars possible — char_data is always a BMP codepoint.
                text.push(cell.char());
                // No overflow or style_id — read inline colors directly.
                attrs_rle.push(cell_attrs_of(cell));
            }
            idx += 1;
        }

        let mut line = Line::with_hyperlinks_owned(text, attrs_rle.finish(), Vec::new());
        if row.is_wrapped() {
            line.set_wrapped(true);
        }
        line
    }

    /// Extract all CellExtras data from a row before shift_rows_up_by discards
    /// them: hyperlinks, complex chars, combining marks, RGB colors and
    /// StyleId-resolved colors, so they survive into ring buffer scrollback.
    pub(crate) fn extract_row_extras(
        row: &Row,
        extras: &CellExtras,
        row_idx: u16,
        styles: &StyleTable,
    ) -> ScrolledRowExtras {
        let mut result = ScrolledRowExtras::default();
        Self::extract_row_extras_into(&mut result, row, extras, row_idx, styles);
        result
    }

    /// Like [`Grid::extract_row_extras`], but writes into a caller-provided
    /// struct (cleared first), allowing the scroll hot path to recycle a
    /// previously popped `ring_extras` allocation instead of reallocating
    /// the inner `Vec`s for every scrolled styled row.
    pub(crate) fn extract_row_extras_into(
        result: &mut ScrolledRowExtras,
        row: &Row,
        extras: &CellExtras,
        row_idx: u16,
        styles: &StyleTable,
    ) {
        result.clear();
        // Headless scrollback-text-only mode (opt-in): skip per-cell
        // colour/style extraction on scroll, for embeddings that read
        // scrollback as text. OSC-8 hyperlink spans are still collected (a
        // cheap pass) so off-screen links stay recoverable; inline images are
        // not. The visible grid is untouched.
        if scrollback_text_only() {
            Self::extract_hyperlinks_only_into(result, row, extras, row_idx);
            return;
        }
        // Inline images first, over the full physical width: every gate below
        // is driven by `row.len()`, and `place_image` writes no glyph, so an
        // image-only row has `len == 0` and would return before its image cells
        // were seen.
        Self::extract_image_spans_into(result, row, extras, row_idx);
        let len = row.len() as usize;
        if len == 0 {
            return;
        }

        // Skip iteration when no overflow data exists. StyleId cells need
        // resolution even without CellExtras, so check both; the per-row
        // HAS_STYLE_ID flag avoids scanning plain rows.
        //
        // `has_any_data()` rather than `is_empty()`: ring-only entries (complex
        // chars, RGB colors) bypass the HashMap on the write path and would
        // otherwise be dropped on scroll.
        //
        // `RowFlags::HAS_STYLE_ID` is only originated by
        // `row/style_id_write.rs` (test-only), so in the shipped binary
        // this short-circuit rests on `has_any_data()` alone.
        if !extras.has_any_data() && !row.has_style_id() {
            return;
        }

        // Whether the map can hold an entry on this row. `false` is exact
        // (`map_may_have_row`): every `extras.get` below would miss, so the
        // walk skips those probes. With a handful of entries elsewhere on the
        // screen, one scan of the map replaces `len` hash probes per scrolled
        // row. An empty map is always `false`.
        let probe_map = extras.map_may_have_row(row_idx, len);

        // Row-local gate for rows with no map entry.
        //
        // The gate above is sticky: `has_any_data()` includes the dense rings,
        // which are allocated on the first truecolor / non-BMP write anywhere
        // and never freed, so one coloured prompt would send every later plain
        // row through the two O(cols) passes below inside the PTY reader's
        // `term_lock` hold.
        //
        // With no map entry on the row, the only remaining sources are
        // announced by the cell's own bits (USES_STYLE_ID, COMPLEX, fg/bg RGB
        // overflow modes), written by the same operations that fill the rings,
        // so this check is exact. With an entry on the row the walk runs.
        if !probe_map && !row_has_extractable_cells(&row.as_slice()[..len]) {
            return;
        }

        // Pre-size the rgb vectors so RGB-overflow and style-id rows do not pay
        // repeated growth reallocs on the scroll path; one counting pass over
        // the cells, skipped when recycled vectors already have capacity.
        //
        // Not gated on `row.has_style_id()` (test-only; plain truecolor
        // writes via `set_rgb_ring_range` set no row flag).
        //
        // fg and bg overflow independently, so count them separately: extra
        // capacity would ride into the ring unseen by
        // `DeferredLine::heap_memory_used` (shallow extras). The counts are an
        // upper bound (the ring may have evicted an entry), which is harmless.
        if result.rgb_fg.capacity() == 0 || result.rgb_bg.capacity() == 0 {
            let cap_cells = &row.as_slice()[..len];
            let (mut n_fg, mut n_bg) = (0usize, 0usize);
            for (i, c) in cap_cells.iter().enumerate() {
                // Mirror the main loop's spacer skip so the reserve matches what
                // is actually pushed.
                if is_spacer(cap_cells, i) {
                    continue;
                }
                if c.uses_style_id() {
                    n_fg += 1;
                    n_bg += 1;
                    continue;
                }
                if c.fg_needs_overflow() {
                    n_fg += 1;
                }
                if c.bg_needs_overflow() {
                    n_bg += 1;
                }
            }
            if n_fg > 0 {
                result.rgb_fg.reserve(n_fg);
            }
            if n_bg > 0 {
                result.rgb_bg.reserve(n_bg);
            }
        }

        // Track open hyperlink span: (start_col, url, id)
        let mut current_span: Option<(u16, Arc<str>, Option<Arc<str>>)> = None;
        // One-entry cache for StyleId resolution: styled runs share the same
        // id across consecutive cells, so this skips most table lookups.
        let mut last_style: Option<(crate::StyleId, [u8; 3], [u8; 3])> = None;

        let cells = &row.as_slice()[..len];
        for (physical_col, cell) in cells.iter().enumerate() {
            if is_spacer(cells, physical_col) {
                continue;
            }

            let col_u16 = u16::try_from(physical_col).unwrap_or(u16::MAX);
            let coord = CellCoord::new(row_idx, col_u16);

            // StyleId cells: resolve colors from the style table now, before
            // the cell scrolls off and loses access to the table.
            if cell.uses_style_id() {
                let sid = cell.style_id();
                let resolved = match last_style {
                    Some((cached_id, fg, bg)) if cached_id == sid => Some((fg, bg)),
                    _ => styles.get(sid).map(|style| {
                        let (r, g, b) = style.fg.to_rgb();
                        let fg = [r, g, b];
                        let (r, g, b) = style.bg.to_rgb();
                        let bg = [r, g, b];
                        last_style = Some((sid, fg, bg));
                        (fg, bg)
                    }),
                };
                if let Some((fg, bg)) = resolved {
                    result.rgb_fg.push((col_u16, fg));
                    result.rgb_bg.push((col_u16, bg));
                }
            }

            // Complex character: prefer full Arc<str> from HashMap (preserves
            // multi-codepoint ZWJ sequences), fall back to ring buffer codepoint.
            if cell.is_complex() {
                if let Some(arc) = extras.complex_char_arc_for(row_idx, col_u16) {
                    result.complex_chars.push((col_u16, Arc::clone(arc)));
                } else if let Some(c) = extras.complex_codepoint_for(row_idx, col_u16) {
                    let mut buf = [0u8; 4];
                    let s = c.encode_utf8(&mut buf);
                    result.complex_chars.push((col_u16, Arc::from(s)));
                }
            }

            // RGB foreground — ring buffer or HashMap. Extracted outside the
            // extras.get(coord) block so ring-buffer-only RGB cells (from
            // set_rgb_ring_range hot path) are not missed.
            if cell.fg_needs_overflow()
                && !cell.uses_style_id()
                && let Some(rgb) = extras.fg_rgb_for(row_idx, col_u16)
            {
                result.rgb_fg.push((col_u16, rgb));
            }

            // RGB background — same ring-first lookup.
            if cell.bg_needs_overflow()
                && !cell.uses_style_id()
                && let Some(rgb) = extras.bg_rgb_for(row_idx, col_u16)
            {
                result.rgb_bg.push((col_u16, rgb));
            }

            if let Some(extra) = probe_map.then(|| extras.get(coord)).flatten() {
                // Combining marks
                if !extra.combining().is_empty() {
                    result
                        .combining
                        .push((col_u16, SmallVec::from_slice(extra.combining())));
                }

                // SGR 58 underline colour: capture the packed form (indexed or
                // RGB) so it re-resolves against the live palette on restore,
                // like the live cell.
                if let Some(packed) = extra.underline_color_u32() {
                    result.underline_colors.push((col_u16, packed));
                }

                // Hyperlink span coalescing, keyed by physical column
                // (col_u16) to match restore_hyperlinks; carries URL and ID.
                let url = extra.hyperlink().cloned();
                let id = extra.hyperlink_id().cloned();
                match (&current_span, url) {
                    (None, Some(new_url)) => {
                        current_span = Some((col_u16, new_url, id));
                    }
                    // Same URL pointer AND same ID → extend existing span.
                    // Two OSC 8 sequences with the same URL but different IDs
                    // are distinct hyperlinks and must not be coalesced.
                    (Some((_, prev_url, prev_id)), Some(ref new_url))
                        if Arc::ptr_eq(prev_url, new_url) && *prev_id == id => {}
                    (Some((start, prev_url, prev_id)), next) => {
                        result.hyperlinks.push(HyperlinkSpan::with_id(
                            *start,
                            col_u16,
                            prev_url.clone(),
                            prev_id.clone(),
                        ));
                        current_span = next.map(|u| (col_u16, u, id));
                    }
                    (None, None) => {}
                }
            } else {
                // No extras at this cell — close any open hyperlink span
                if let Some((start, prev_url, prev_id)) = current_span.take() {
                    result
                        .hyperlinks
                        .push(HyperlinkSpan::with_id(start, col_u16, prev_url, prev_id));
                }
            }
        }

        if let Some((start, url, id)) = current_span {
            let end_col = u16::try_from(len).unwrap_or(u16::MAX);
            result
                .hyperlinks
                .push(HyperlinkSpan::with_id(start, end_col, url, id));
        }
    }

    /// Coalesce this row's inline-image cells into [`ImageSpan`]s, so a
    /// placement survives the row's trip into scrollback.
    ///
    /// Scans the full physical width (`Row::cols`), not `Row::len()`: an image
    /// cell carries only a `CellExtra` (`place_image` writes no glyph), so an
    /// image-only row has `len` 0. `Terminal::images_row_into` scans
    /// `0..grid.cols()` on the render side for the same reason.
    ///
    /// Two gates keep the scan off other rows: the sticky
    /// [`CellExtras::any_image`] flag (one bool per scrolled row for sessions
    /// that never drew a picture), then each cell's `HAS_EXTRAS` bit.
    ///
    /// Cells of each image layer coalesce left to right while they stay on the
    /// same placement (`Arc::ptr_eq` and Kitty tag), footprint row and
    /// consecutive footprint columns: one span per layer per footprint row
    /// normally, with a break where an overwrite or clipped edge interrupted
    /// the picture. Spans are stored in
    /// [`ImageRef::draw_key`](crate::ImageRef::draw_key) order, the order the
    /// restore stacks them.
    fn extract_image_spans_into(
        result: &mut ScrolledRowExtras,
        row: &Row,
        extras: &CellExtras,
        row_idx: u16,
    ) {
        if !extras.any_image() {
            return;
        }
        let cells = row.as_slice();
        // Open runs, each the span it will close as plus the footprint column
        // its next cell must paint.
        let mut open: Vec<(ImageSpan, u16)> = Vec::new();
        let mut done: Vec<ImageSpan> = Vec::new();
        let continues = |(span, next): &(ImageSpan, u16), layer: &crate::ImageRef| {
            Arc::ptr_eq(&span.image, &layer.image)
                && span.kitty == layer.kitty
                && span.image_row == layer.cell_row
                && *next == layer.cell_col
        };
        for (physical_col, cell) in cells.iter().enumerate() {
            let Ok(col_u16) = u16::try_from(physical_col) else {
                break;
            };
            // The image refs live in the map, and the map is only ever probed
            // for a cell whose own flag says it has an entry.
            let layers = if cell.has_extras() {
                extras
                    .get(CellCoord::new(row_idx, col_u16))
                    .map_or(&[][..], CellExtra::images)
            } else {
                &[]
            };
            // Close every run this column does not continue.
            let mut at = 0;
            while at < open.len() {
                if layers.iter().any(|layer| continues(&open[at], layer)) {
                    at += 1;
                } else {
                    let (mut span, _) = open.swap_remove(at);
                    span.end_col = col_u16;
                    done.push(span);
                }
            }
            for layer in layers {
                if let Some(run) = open.iter_mut().find(|run| continues(run, layer)) {
                    run.1 = run.1.saturating_add(1);
                } else {
                    let span = ImageSpan {
                        kitty: layer.kitty,
                        ..ImageSpan::new(
                            col_u16,
                            col_u16,
                            layer.cell_row,
                            layer.cell_col,
                            Arc::clone(&layer.image),
                        )
                    };
                    open.push((span, layer.cell_col.saturating_add(1)));
                }
            }
        }
        let end_col = u16::try_from(cells.len()).unwrap_or(u16::MAX);
        done.extend(open.into_iter().map(|(mut span, _)| {
            span.end_col = end_col;
            span
        }));
        done.sort_by_key(|span| aterm_types::layer_order(span.image.z_index, span.kitty));
        result.images.extend(done);
    }

    /// Hyperlink-only extraction for the scrollback-text-only fast path: coalesce
    /// runs of cells sharing an OSC-8 url+id into [`HyperlinkSpan`]s (end_col
    /// exclusive), skipping the colour/style/complex/rgb work the full path does.
    /// Mirrors the hyperlink coalescing in [`Self::extract_row_extras_into`].
    fn extract_hyperlinks_only_into(
        result: &mut ScrolledRowExtras,
        row: &Row,
        extras: &CellExtras,
        row_idx: u16,
    ) {
        let len = row.len() as usize;
        if len == 0 || !extras.has_any_data() {
            return;
        }
        // Open hyperlink span: (start_col, url, id).
        let mut current_span: Option<(u16, Arc<str>, Option<Arc<str>>)> = None;
        let cells = &row.as_slice()[..len];
        for physical_col in 0..len {
            if is_spacer(cells, physical_col) {
                continue;
            }
            let col_u16 = u16::try_from(physical_col).unwrap_or(u16::MAX);
            let coord = CellCoord::new(row_idx, col_u16);
            let url = extras.get(coord).and_then(|e| e.hyperlink().cloned());
            let id = extras.get(coord).and_then(|e| e.hyperlink_id().cloned());
            match (&current_span, url) {
                (None, Some(new_url)) => {
                    current_span = Some((col_u16, new_url, id));
                }
                // Same url pointer AND same id → extend; differing id is a
                // distinct link and must not coalesce.
                (Some((_, prev_url, prev_id)), Some(ref new_url))
                    if Arc::ptr_eq(prev_url, new_url) && *prev_id == id => {}
                (Some((start, prev_url, prev_id)), next) => {
                    result.hyperlinks.push(HyperlinkSpan::with_id(
                        *start,
                        col_u16,
                        prev_url.clone(),
                        prev_id.clone(),
                    ));
                    current_span = next.map(|u| (col_u16, u, id));
                }
                (None, None) => {}
            }
        }
        if let Some((start, url, id)) = current_span {
            let end_col = u16::try_from(len).unwrap_or(u16::MAX);
            result
                .hyperlinks
                .push(HyperlinkSpan::with_id(start, end_col, url, id));
        }
    }

    /// Convert a Row to a Line, preserving all CellExtras data.
    ///
    /// Test-only: production uses `row_to_line_with_stored_extras`, which takes
    /// pre-extracted extras from `ring_extras`. This extracts and builds in one
    /// step.
    #[cfg(test)]
    pub(crate) fn row_to_line_with_hyperlinks(
        row: &Row,
        extras: &CellExtras,
        row_idx: u16,
        styles: &StyleTable,
    ) -> Line {
        let extracted = Self::extract_row_extras(row, extras, row_idx, styles);
        Self::row_to_line_with_stored_extras(row, &extracted)
    }
}

fn push_cell_text(
    text: &mut String,
    cell: Cell,
    extras: &ScrolledRowExtras,
    cursors: &mut RowToLineCursorState,
    col_u16: u16,
) -> usize {
    if !cell.is_complex() {
        // NUL (empty cell) → space, matching row_text() in content_queries.rs
        // and Row::fmt in row/fmt.rs, so search sees the same content visible
        // or in scrollback.
        let ch = cell.char();
        text.push(if ch == '\0' { ' ' } else { ch });
        return 1;
    }

    if let Some(value) = next_complex_char(extras, cursors, col_u16) {
        let char_count = value.chars().count();
        text.push_str(value);
        char_count
    } else {
        text.push('\u{FFFD}');
        1
    }
}

/// Consume this column's stored complex-char string, if the cursor is on it.
///
/// The one place the complex cursor advances. Shared by [`push_cell_text`] (the
/// Line serializer) and the ring fast materializer.
pub(in crate::grid) fn next_complex_char<'a>(
    extras: &'a ScrolledRowExtras,
    cursors: &mut RowToLineCursorState,
    col_u16: u16,
) -> Option<&'a Arc<str>> {
    let (_, value) = extras
        .complex_chars
        .get(cursors.complex_idx)
        .filter(|(col, _)| *col == col_u16)?;
    cursors.complex_idx += 1;
    Some(value)
}

/// Consume this column's stored combining marks, if the cursor is on them.
///
/// The one place the combining cursor advances. Shared by
/// [`push_combining_marks`] and the ring fast materializer.
pub(in crate::grid) fn next_combining<'a>(
    extras: &'a ScrolledRowExtras,
    cursors: &mut RowToLineCursorState,
    col_u16: u16,
) -> Option<&'a [char]> {
    let (_, combining) = extras
        .combining
        .get(cursors.combining_idx)
        .filter(|(col, _)| *col == col_u16)?;
    cursors.combining_idx += 1;
    Some(combining.as_slice())
}

/// Resolve one channel of a cell's colour the way the Line serializer does:
/// inline when the cell carries it, else the column's pre-resolved stored RGB
/// (extraction resolved RGB overflow and `StyleId` at scroll time), else the
/// default. Shared with the ring fast materializer.
pub(in crate::grid) fn resolve_cell_color(
    needs_stored: bool,
    inline_raw: u32,
    stored_colors: &[(u16, [u8; 3])],
    color_idx: &mut usize,
    col_u16: u16,
    default_raw: u32,
) -> u32 {
    if !needs_stored {
        return inline_raw;
    }

    if let Some((_, [r, g, b])) = stored_colors
        .get(*color_idx)
        .filter(|(col, _)| *col == col_u16)
    {
        *color_idx += 1;
        PackedColor::rgb(*r, *g, *b).0
    } else {
        default_raw
    }
}

/// Accumulates a materializing line's attribute runs, allocating nothing until
/// a second distinct `CellAttrs` appears.
///
/// `Line::from_parts` discards an `Rle` with zero runs or one default run,
/// which is every plain-text line. Holding the open run in two locals makes
/// that case allocation-free and the per-cell step one comparison.
///
/// The emitted `Rle` matches a push-per-cell `Rle` in every case
/// `from_parts` keeps:
///   * no cells             -> 0 runs                          -> `attrs = None`
///   * one default run      -> 0 runs (push-per-cell: 1 run)   -> `attrs = None`
///   * one non-default run  -> 1 run                           -> stored
///   * two or more runs     -> same runs, same order           -> stored
///
/// A spill only happens on a value change, so a spilled builder always emits
/// at least two runs.
struct AttrRunBuilder {
    /// Runs already closed out. Empty (and unallocated) until the first spill.
    rle: Rle<CellAttrs>,
    /// Value of the OPEN run. Seeded to `DEFAULT` so the first cell of a plain
    /// line merges into it instead of taking the cold path.
    value: CellAttrs,
    /// How many cells the open run covers. `0` means "nothing opened yet", and
    /// because a fresh builder's `value` is `DEFAULT` a zero-length open run
    /// merges correctly with whatever arrives first.
    len: u32,
    /// Whether any run was ever closed into `rle`. Distinguishes "one run, and
    /// it is default" (drop it) from "several runs, the last happens to be
    /// default" (keep them) — the case a naive `is_default()` test would lose.
    spilled: bool,
}

impl AttrRunBuilder {
    /// Deliberately NOT named `new`: the lock-order census resolves one-hop
    /// held calls by callee NAME, so a helper called `new` would merge with
    /// every other `new` in the tree.
    fn empty() -> Self {
        Self {
            rle: Rle::new(),
            value: CellAttrs::DEFAULT,
            len: 0,
            spilled: false,
        }
    }

    #[inline]
    fn push(&mut self, attrs: CellAttrs) {
        self.extend(attrs, 1);
    }

    /// Per-cell hot path: one comparison and one add.
    ///
    /// `len == 0` needs no separate test: a fresh builder's open run is
    /// `(DEFAULT, 0)`, so a default first cell merges and a styled one goes to
    /// `open_new_run`, which flushes nothing.
    ///
    /// `open_new_run` is a separate non-inlined function: folding its
    /// `Rle::extend_with` in here made this too big to inline into the
    /// per-cell materialization loop, which measurably regressed it.
    #[inline]
    fn extend(&mut self, attrs: CellAttrs, count: u32) {
        if count == 0 {
            return;
        }
        if self.value == attrs {
            // A row is at most `u16::MAX` cells with bounded characters each, so
            // this cannot reach `u32::MAX`; saturating discharges the overflow
            // obligation, and `Rle::extend_with` clamps at flush.
            self.len = self.len.saturating_add(count);
            return;
        }
        self.open_new_run(attrs, count);
    }

    /// Cold path: close the open run (if any) into the `Rle` and start a new one.
    #[inline(never)]
    fn open_new_run(&mut self, attrs: CellAttrs, count: u32) {
        if self.len != 0 {
            self.rle.extend_with(self.value, self.len);
            self.spilled = true;
        }
        self.value = attrs;
        self.len = count;
    }

    /// Close the open run and yield the `Rle` to hand to `Line::from_parts`.
    ///
    /// A single all-default run is dropped rather than emitted: that is the
    /// value `from_parts` collapses to `None` anyway, and not emitting it is
    /// what keeps a plain line's materialization allocation-free.
    fn finish(mut self) -> Rle<CellAttrs> {
        if self.len != 0 && (self.spilled || !self.value.is_default()) {
            self.rle.extend_with(self.value, self.len);
        }
        self.rle
    }
}

fn push_repeated_attrs(attrs_rle: &mut AttrRunBuilder, attrs: CellAttrs, char_count: usize) {
    // `char_count` copies of one value is one run, so extend by the count
    // instead of pushing per character (same clamping to the remaining
    // capacity).
    attrs_rle.extend(attrs, u32::try_from(char_count).unwrap_or(u32::MAX));
}

fn push_combining_marks(
    text: &mut String,
    attrs_rle: &mut AttrRunBuilder,
    attrs: CellAttrs,
    extras: &ScrolledRowExtras,
    cursors: &mut RowToLineCursorState,
    col_u16: u16,
) {
    let Some(combining) = next_combining(extras, cursors, col_u16) else {
        return;
    };

    for &c in combining {
        text.push(c);
        attrs_rle.push(attrs);
    }
}

/// Coalesce per-column packed underline colours (ascending by physical column)
/// into [`UnderlineColorSpan`]s: consecutive columns sharing one colour merge
/// into a single `[start, end)` span; a colour change or a column gap starts a
/// new span. Restoring by filling each span's `[start, end)` range therefore
/// reproduces the exact per-column colours (a gap stays uncoloured).
pub(in crate::grid) fn coalesce_underline_spans(per_col: &[(u16, u32)]) -> Vec<UnderlineColorSpan> {
    let mut spans: Vec<UnderlineColorSpan> = Vec::new();
    for &(col, color) in per_col {
        if let Some(last) = spans.last_mut()
            && last.color == color
            && last.end_col == col
        {
            last.end_col = col.saturating_add(1);
            continue;
        }
        spans.push(UnderlineColorSpan::new(col, col.saturating_add(1), color));
    }
    spans
}

#[cfg(test)]
mod plain_run_equivalence_tests {
    use super::*;

    // A per-cell reference loop, independent of append_plain_run and
    // cell_attrs_of, checked against both shipping entry points.
    fn old_cell_loop(cells: &[Cell], wrapped: bool) -> Line {
        let mut text = String::new();
        let mut attrs = AttrRunBuilder::empty();
        for (idx, cell) in cells.iter().enumerate() {
            if is_spacer(cells, idx) {
                continue;
            }
            text.push(cell.char());
            let fg = cell.fg_color().map_or(PackedColor::DEFAULT_FG.0, |c| c.0);
            let bg = cell.bg_color().map_or(PackedColor::DEFAULT_BG.0, |c| c.0);
            attrs.push(CellAttrs::from_raw(fg, bg, cell.flags().bits()));
        }
        let mut line = Line::with_hyperlinks_owned(text, attrs.finish(), Vec::new());
        line.set_wrapped(wrapped);
        line
    }

    fn equivalent(actual: &Line, expected: &Line) -> bool {
        actual.to_string() == expected.to_string()
            && actual.attrs().map(Rle::runs) == expected.attrs().map(Rle::runs)
            && actual.is_wrapped() == expected.is_wrapped()
            // This is also the history representation carried by an update.
            && aterm_scrollback::serialize_lines(std::slice::from_ref(actual))
                == aterm_scrollback::serialize_lines(std::slice::from_ref(expected))
    }

    fn check_both_paths(cells: &[Cell], wrapped: bool) {
        let len = u16::try_from(cells.len()).expect("bounded fixture");
        let mut grid = Grid::new(1, len.max(1));
        let row = grid.row_mut(0).expect("fixture row");
        for (col, cell) in cells.iter().enumerate() {
            assert!(row.set(u16::try_from(col).expect("bounded column"), *cell));
        }
        // The explicit full-width seam must retain trailing spaces too.
        row.update_len(len);
        row.set_wrapped(wrapped);
        let expected = old_cell_loop(cells, wrapped);
        let eager =
            Grid::row_to_line_with_stored_extras_at_len(row, &ScrolledRowExtras::default(), len);
        assert!(
            equivalent(&eager, &expected),
            "eager len={len}, wrapped={wrapped}"
        );
        let deferred = DeferredLine::new(row, ScrolledRowExtras::default(), Vec::new());
        assert!(
            equivalent(deferred.to_line(), &expected),
            "deferred len={len}, wrapped={wrapped}"
        );
        // Consuming an uncached deferred row is a separate shipping caller.
        let uncached = DeferredLine::new(row, ScrolledRowExtras::default(), Vec::new());
        let (consumed, _) = uncached.into_line_and_body();
        assert!(
            equivalent(&consumed, &expected),
            "consumed len={len}, wrapped={wrapped}"
        );
    }

    fn styled(ch: char, color: u8, flags: CellFlags) -> Cell {
        Cell::with_style(
            ch,
            PackedColor::indexed(color),
            PackedColor::DEFAULT_BG,
            flags,
        )
    }

    #[test]
    fn plain_run_materialization_matches_old_loop_at_chunk_boundaries() {
        for len in [0, 1, 63, 64, 65, 127, 128, 129] {
            for wrapped in [false, true] {
                check_both_paths(&vec![Cell::new('x'); len], wrapped);
                check_both_paths(&vec![styled('x', 2, CellFlags::BOLD); len], wrapped);
                check_both_paths(&vec![Cell::EMPTY; len], wrapped);
                let alternating: Vec<_> = (0..len)
                    .map(|i| styled('x', if i % 2 == 0 { 1 } else { 2 }, CellFlags::BOLD))
                    .collect();
                check_both_paths(&alternating, wrapped);
            }
        }
        // Attribute boundaries independently straddle text-chunk boundaries.
        for boundary in [63, 64, 65, 127, 128] {
            let mut cells = vec![styled('a', 1, CellFlags::BOLD); 129];
            cells[boundary..].fill(styled('b', 2, CellFlags::ITALIC));
            check_both_paths(&cells, false);
            check_both_paths(&cells, true);
        }
    }

    #[test]
    fn plain_run_materialization_matches_old_loop_across_fallbacks() {
        let fallback = [
            Cell::new('\0'),
            Cell::new('\u{1f}'),
            Cell::new('\u{7f}'),
            Cell::new('\u{80}'),
            Cell::new('é'),
            styled('世', 2, CellFlags::WIDE),
            styled(' ', 2, CellFlags::WIDE_CONTINUATION),
            styled('!', 2, CellFlags::PROTECTED),
            styled('x', 2, CellFlags::COMPLEX),
            Cell::from_raw_parts(0xD800, crate::PackedColors::DEFAULT, CellFlags::empty()),
        ];
        for boundary in [0, 1, 63, 64, 65, 127, 128] {
            let mut cells = vec![Cell::new('a'); boundary];
            cells.extend_from_slice(&fallback);
            cells.extend(std::iter::repeat_n(Cell::new('z'), 65));
            check_both_paths(&cells, false);
            check_both_paths(&cells, true);
        }
        let expected = old_cell_loop(&fallback, false);
        assert_eq!(
            expected.to_string(),
            "\0\u{1f}\u{7f}\u{80}é世!\u{fffd}\u{fffd}"
        );
    }

    #[test]
    fn plain_run_equivalence_rejects_lost_text_attributes_and_wrap() {
        let cells = vec![styled('x', 2, CellFlags::BOLD); 65];
        let expected = old_cell_loop(&cells, true);
        assert!(
            equivalent(&expected, &expected),
            "positive comparator control"
        );
        assert!(
            !equivalent(&old_cell_loop(&cells[..64], true), &expected),
            "dropping the first byte after a chunk must be caught"
        );
        let mut wrong_attrs = cells.clone();
        wrong_attrs[64] = Cell::new('x');
        let wrong_attrs = old_cell_loop(&wrong_attrs, true);
        assert_eq!(wrong_attrs.to_string(), expected.to_string());
        assert!(
            !equivalent(&wrong_attrs, &expected),
            "text equality alone must not hide a lost attribute at the chunk boundary"
        );
        assert!(
            !equivalent(&old_cell_loop(&cells, false), &expected),
            "losing the wrapped bit must be caught"
        );
    }
}

#[cfg(test)]
mod extract_gate_tests {
    use super::*;
    use crate::{CellFlags, PackedColor};

    /// The map-empty gate never gives a false negative: a row whose only datum
    /// is an RGB colour in the dense ring (as `set_range_uniform`'s RGB-only
    /// path leaves it: value in the ring, mode bit on the cell, nothing in the
    /// map) still extracts it.
    #[test]
    fn map_empty_row_with_ring_rgb_still_extracts() {
        let mut grid = Grid::new(3, 8);
        assert!(
            grid.row_mut(0).expect("row 0").write_char_styled(
                0,
                'X',
                PackedColor::rgb(9, 8, 7),
                PackedColor::DEFAULT_BG,
                CellFlags::empty(),
            ),
            "precondition: the styled write must land"
        );
        grid.extras_mut()
            .set_rgb_ring_range(0, 0, 1, Some([9, 8, 7]), None, 3, 8);
        assert!(
            grid.extras().is_empty(),
            "precondition: RGB-only data belongs in the ring, not the map — \
             otherwise this test does not exercise the map-empty gate"
        );

        let row = grid.storage.row(0).expect("row 0");
        let extracted = Grid::extract_row_extras(row, grid.extras(), 0, grid.styles());
        assert_eq!(
            extracted.rgb_fg,
            vec![(0u16, [9u8, 8, 7])],
            "the gate dropped a truecolor cell that lives in the ring"
        );
    }

    /// An armed ring elsewhere in the grid changes only the cost of extracting
    /// a plain row, not the result, which is empty either way.
    #[test]
    fn armed_ring_does_not_change_a_plain_row() {
        let mut grid = Grid::new(3, 8);
        grid.set_cursor(0, 0);
        for ch in "hello".chars() {
            grid.write_char(ch);
        }

        let before = {
            let row = grid.storage.row(0).expect("row 0");
            Grid::extract_row_extras(row, grid.extras(), 0, grid.styles())
        };
        assert!(before.is_empty(), "a plain row extracts nothing");

        // Arm the grid-global sticky gate on a DIFFERENT row, the way a
        // truecolor prompt does.
        grid.extras_mut()
            .set_rgb_ring_range(2, 0, 4, Some([1, 2, 3]), None, 3, 8);
        assert!(
            grid.extras().has_any_data(),
            "precondition: the sticky grid-global gate is now armed"
        );
        assert!(
            grid.extras().is_empty(),
            "precondition: the map is still empty — only the ring was armed"
        );

        let after = {
            let row = grid.storage.row(0).expect("row 0");
            Grid::extract_row_extras(row, grid.extras(), 0, grid.styles())
        };
        assert!(
            after.is_empty(),
            "an armed ring on another row changed what a plain row extracts"
        );
    }
}

#[cfg(test)]
mod scrolled_row_extras_tests {
    use std::sync::Arc;

    use aterm_alloc::SmallVec;
    use aterm_scrollback::{HyperlinkSpan, ImageSpan};
    use aterm_types::{ImageData, ImageFormat, ImageScaling};

    use super::ScrolledRowExtras;

    /// `is_empty` is what lets a plain row skip its boxed extras, so a false
    /// positive drops scrollback extras: over every subset of the seven
    /// fields, it is true exactly when none is populated.
    #[test]
    fn is_empty_iff_every_field_is_empty() {
        let image = Arc::new(ImageData {
            bytes: vec![0x89, b'P', b'N', b'G'],
            format: ImageFormat::Png,
            cols: 1,
            rows: 1,
            z_index: 0,
            band_lift_px: 0,
            cell_offset_px: (0, 0),
            scaling: ImageScaling::Fit,
            source_rect: None,
        });
        for subset in 0u8..(1 << 7) {
            let has = |field: u8| subset >> field & 1 == 1;
            let mut extras = ScrolledRowExtras::default();
            if has(0) {
                extras
                    .hyperlinks
                    .push(HyperlinkSpan::new(0, 1, Arc::from("https://example.com")));
            }
            if has(1) {
                extras.complex_chars.push((0, Arc::from("abc")));
            }
            if has(2) {
                extras
                    .combining
                    .push((0, SmallVec::from_slice(&['\u{0300}'])));
            }
            if has(3) {
                extras.rgb_fg.push((0, [255, 0, 0]));
            }
            if has(4) {
                extras.rgb_bg.push((0, [0, 0, 255]));
            }
            if has(5) {
                extras.underline_colors.push((0, 0x01_FF_00_00));
            }
            if has(6) {
                extras
                    .images
                    .push(ImageSpan::new(0, 1, 0, 0, Arc::clone(&image)));
            }
            assert_eq!(extras.is_empty(), subset == 0, "fields {subset:07b}");
        }
    }
}
