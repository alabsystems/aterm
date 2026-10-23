// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Row-to-line conversion for scrollback.

use std::cell::OnceCell;
use std::collections::VecDeque;
use std::sync::Arc;

use aterm_alloc::SmallVec;
use aterm_rle::Rle;
use aterm_scrollback::{CellAttrs, HyperlinkSpan, ImageSpan, Line, UnderlineColorSpan};

use super::Grid;
use super::scroll_materialize::walk_folds_across;
use crate::Cell;
use crate::CellExtra;
use crate::CellFlags;
use crate::PackedColor;
use crate::Row;
use crate::{CellCoord, CellExtras};

use std::sync::atomic::{AtomicBool, Ordering};

/// Headless scrollback-text-only toggle (default off): the scroll path skips
/// per-cell extras extraction, so scrollback keeps text but not colour/style.
static SCROLLBACK_TEXT_ONLY: AtomicBool = AtomicBool::new(false);

/// Enable/disable the headless scrollback-text-only fast path ([`SCROLLBACK_TEXT_ONLY`]).
#[inline]
pub fn set_scrollback_text_only(enabled: bool) {
    SCROLLBACK_TEXT_ONLY.store(enabled, Ordering::Relaxed);
}

/// Whether the headless scrollback-text-only fast path is active.
#[must_use]
#[inline]
pub fn scrollback_text_only() -> bool {
    SCROLLBACK_TEXT_ONLY.load(Ordering::Relaxed)
}

/// Whether `cells[idx]` is a genuine wide-character continuation spacer rather than a DECSCA-protected cell: `PROTECTED` and `WIDE_CONTINUATION` share bit 10, and skipping protected cells would drop them from scrollback.
#[inline]
pub(in crate::grid) const fn is_spacer(cells: &[Cell], idx: usize) -> bool {
    cells[idx].is_wide_continuation()
        && !cells[idx].is_wide()
        && idx > 0
        && cells[idx - 1].is_wide()
}

/// The flags that send a cell down the per-cell path: a complex codepoint lives
/// in the overflow table, and a wide cell or spacer makes [`is_spacer`] consult
/// its neighbour (`PROTECTED` shares the spacer bit, so it rides along).
const RUN_DISQUALIFY: CellFlags =
    CellFlags(CellFlags::COMPLEX.0 | CellFlags::WIDE.0 | CellFlags::WIDE_CONTINUATION.0);

/// A cell's plain printable-ASCII byte, or `None` when it needs the per-cell
/// path.
#[inline]
const fn plain_ascii_byte(cell: &Cell) -> Option<u8> {
    if cell.flags().intersects(RUN_DISQUALIFY) {
        return None;
    }
    let c = cell.char() as u32;
    if c >= 0x20 && c < 0x80 {
        Some(c as u8)
    } else {
        None
    }
}

/// The attributes of `cell` exactly as the per-cell loop computed them.
#[inline]
const fn cell_attrs_of(cell: &Cell) -> CellAttrs {
    let fg_raw = match cell.fg_color() {
        Some(c) => c.0,
        None => PackedColor::DEFAULT_FG.0,
    };
    let bg_raw = match cell.bg_color() {
        Some(c) => c.0,
        None => PackedColor::DEFAULT_BG.0,
    };
    CellAttrs::from_raw(fg_raw, bg_raw, cell.flags().bits())
}

/// Append the maximal run of printable ASCII under one attribute at `from` as
/// one `push_str` and one [`AttrRunBuilder::extend`], and return where it ended.
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

/// Whether any cell carries something [`Grid::extract_row_extras_into`] can
/// extract without the extras map: a complex codepoint or an RGB overflow
/// colour.
#[inline]
fn row_has_extractable_cells(cells: &[Cell]) -> bool {
    let mut found = false;
    for cell in cells {
        found |= cell.is_complex() | cell.fg_needs_overflow() | cell.bg_needs_overflow();
    }
    found
}

const _: () = assert!(std::mem::size_of::<Cell>() == 8);

/// Compact snapshot of a grid row's raw cells and extras, taken at scroll time and converted to a [`Line`] on first read ([`to_line`](Self::to_line)).
#[derive(Debug, Clone)]
pub(crate) struct DeferredLine {
    /// Raw cells copied from the row (8 bytes each).
    cells: Vec<Cell>,
    /// Number of occupied cells (`Row::len`).
    len: u16,
    /// Preserved extras; `None` exactly when empty (plain-text rows), which
    /// `heap_memory_used` and the materialize readers rely on.
    extras: Option<Box<ScrolledRowExtras>>,
    /// Whether the row was soft-wrapped.
    wrapped: bool,
    /// The materialized line, filled on first read.
    cached: OnceCell<Line>,
}

impl DeferredLine {
    /// Snapshot `row`'s cells into `scratch`, a recycled body from the [`CellPool`] (or `Vec::new()`).
    #[inline]
    pub(crate) fn new(row: &Row, extras: ScrolledRowExtras, scratch: Vec<Cell>) -> Self {
        Self::new_boxed(row, (!extras.is_empty()).then(|| Box::new(extras)), scratch)
    }

    /// [`new`](Self::new) for a caller that already owns the extras in a `Box` (scroll-off popping `ring_extras`): no unbox/re-box per line under `term_lock`.
    #[inline]
    pub(crate) fn new_boxed(
        row: &Row,
        extras: Option<Box<ScrolledRowExtras>>,
        mut scratch: Vec<Cell>,
    ) -> Self {
        let len = row.written_len();
        scratch.clear();
        if len != 0 {
            scratch.extend_from_slice(&row.as_slice()[..len as usize]);
        }
        Self {
            cells: scratch,
            len,
            extras: extras.filter(|b| !b.is_empty()),
            wrapped: row.is_wrapped(),
            cached: OnceCell::new(),
        }
    }

    /// The materialized [`Line`], converted on first call and cached.
    #[inline]
    pub(crate) fn to_line(&self) -> &Line {
        self.cached.get_or_init(|| self.materialize())
    }

    /// Estimated heap bytes owned by this line (not `size_of::<DeferredLine>()`, which the container counts): the cell body, everything the extras retain and the materialized line once read.
    #[inline]
    pub(crate) fn heap_memory_used(&self) -> usize {
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

    /// What staging this line costs: its spine slot, its cell body's capacity (a recycled body can exceed the row) and everything its extras retain, link payload included.
    #[inline]
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
    #[inline]
    fn extras_bytes(&self) -> usize {
        self.extras
            .as_deref()
            .map_or(0, ScrolledRowExtras::retained_memory_used)
    }

    /// The OSC 8 payload this line holds (`ScrolledRowExtras::link_bytes`).
    #[inline]
    pub(crate) fn link_bytes(&self) -> usize {
        self.extras
            .as_deref()
            .map_or(0, ScrolledRowExtras::link_bytes)
    }

    /// Drop this line's links (and the materialized line holding them), keeping text, colours and images; returns the link bytes freed.
    pub(crate) fn strip_hyperlinks(&mut self) -> usize {
        let Some(extras) = self.extras.as_deref_mut() else {
            return 0;
        };
        let freed = extras.link_bytes();
        if freed == 0 {
            return 0;
        }
        extras.strip_hyperlinks();
        if extras.is_empty() {
            self.extras = None;
        }
        self.cached = OnceCell::new();
        freed
    }

    /// Convert into an owned [`Line`], dropping the cell body.
    #[cfg(test)]
    pub(crate) fn into_line(self) -> Line {
        self.into_line_and_body().0
    }

    /// Convert into an owned [`Line`], returning the cell body to `pool` for the next scroll-off instead of freeing it.
    #[inline]
    pub(crate) fn into_line_recycled(self, pool: &mut CellPool) -> Line {
        let (line, body) = self.into_line_and_body();
        pool.put(body);
        line
    }

    /// Shared body of the two `into_line` forms: the line and its cell body,
    /// taken first so the cached early return can recycle it too.
    fn into_line_and_body(mut self) -> (Line, Vec<Cell>) {
        let body = std::mem::take(&mut self.cells);

        if let Some(line) = self.cached.into_inner() {
            return (line, body);
        }

        let default_extras = ScrolledRowExtras::default();
        let extras = self.extras.as_deref().unwrap_or(&default_extras);
        let line = cells_to_line(&body[..self.len as usize], extras, self.wrapped);
        (line, body)
    }

    /// Perform the O(cols) conversion from raw cells to Line.
    fn materialize(&self) -> Line {
        let default_extras = ScrolledRowExtras::default();
        let extras = self.extras.as_deref().unwrap_or(&default_extras);
        cells_to_line(&self.cells[..self.len as usize], extras, self.wrapped)
    }
}

/// The stored [`Line`] for a row's first `cells` (spacers included) and the
/// extras extracted from it at scroll time (RGB overflow colours already
/// resolved): every cell's text in column order, with its attributes on each of
/// its characters.
fn cells_to_line(cells: &[Cell], extras: &ScrolledRowExtras, wrapped: bool) -> Line {
    #[cfg(any(test, debug_assertions))]
    super::count_row_to_line_op();

    let mut line = if cells.is_empty() {
        let mut line = Line::new();
        attach_image_spans(&mut line, extras);
        line
    } else if extras.is_empty() {
        // No complex chars and no overflow colours: every cell is one BMP
        plain_cells_to_line(cells)
    } else {
        styled_cells_to_line(cells, extras)
    };
    if wrapped {
        line.set_wrapped(true);
    }
    line
}

/// [`cells_to_line`] for cells with no extras.
fn plain_cells_to_line(cells: &[Cell]) -> Line {
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

    Line::with_hyperlinks_owned(text, attrs_rle.finish(), Vec::new())
}

/// [`cells_to_line`] for cells with extras: hyperlinks, complex chars,
/// combining marks, RGB colours, underline colours and images.
fn styled_cells_to_line(cells: &[Cell], extras: &ScrolledRowExtras) -> Line {
    let mut text = String::with_capacity(cells.len());
    let mut attrs_rle = AttrRunBuilder::empty();
    let mut cursors = RowToLineCursorState::default();
    let mut prev: Option<usize> = None;

    for (physical_col, cell) in cells.iter().enumerate() {
        if is_spacer(cells, physical_col) {
            continue;
        }

        let col_u16 = u16::try_from(physical_col).unwrap_or(u16::MAX);
        let at = text.len();
        let char_count = push_cell_text(&mut text, *cell, extras, &mut cursors, col_u16);
        let opens = prev.is_some_and(|prev| walk_folds_across(&text, prev, at));
        prev = Some(at);
        let fg_raw = resolve_cell_color(
            cell.fg_needs_overflow(),
            cell.fg_color().map_or(PackedColor::DEFAULT_FG.0, |c| c.0),
            &extras.rgb_fg,
            &mut cursors.rgb_fg_idx,
            col_u16,
            PackedColor::DEFAULT_FG.0,
        );
        let bg_raw = resolve_cell_color(
            cell.bg_needs_overflow(),
            cell.bg_color().map_or(PackedColor::DEFAULT_BG.0, |c| c.0),
            &extras.rgb_bg,
            &mut cursors.rgb_bg_idx,
            col_u16,
            PackedColor::DEFAULT_BG.0,
        );

        let attrs = CellAttrs::from_raw(fg_raw, bg_raw, cell.flags().bits());
        if opens {
            attrs_rle.push(attrs.with_opens_cell(true));
            push_repeated_attrs(&mut attrs_rle, attrs, char_count.saturating_sub(1));
        } else {
            push_repeated_attrs(&mut attrs_rle, attrs, char_count);
        }
        push_combining_marks(
            &mut text,
            &mut attrs_rle,
            attrs,
            extras,
            &mut cursors,
            col_u16,
        );
    }

    let mut line = Line::with_hyperlinks_owned(text, attrs_rle.finish(), extras.hyperlinks.clone());
    if !extras.underline_colors.is_empty() {
        line.set_underline_colors(coalesce_underline_spans(&extras.underline_colors));
    }
    attach_image_spans(&mut line, extras);
    line
}

/// Staging buffer between the ring and the tiered store in `GridStorage`.
#[derive(Debug)]
pub(crate) struct LazyBuffer {
    /// Pending deferred lines, ordered oldest to newest.
    lines: VecDeque<DeferredLine>,
    /// Recycled cell bodies ([`CellPool`]), kept here so every producer and
    /// consumer is on the recycling path and the bytes count in
    /// [`memory_used`](Self::memory_used).
    pool: CellPool,
    /// The flood cut at the front of `lines`, if open ([`FloodCut`]): logically
    /// the oldest row, which `len`, `get_line` and the drains present as one
    /// marker line.
    cut: Option<FloodCut>,
    /// Running sum of [`DeferredLine::staged_bytes`] over `lines`, O(1) so the
    /// flood path can weigh the backlog against the store's headroom on every
    /// scrolled row.
    staged_bytes: usize,
    /// Running sum of [`DeferredLine::link_bytes`] over `lines`: the staged
    /// rows' share of the history link budget (`Grid::history_link_budget`).
    link_bytes: usize,
    /// `lines[..linkless_front]` hold no link bytes: where the next
    /// [`strip_oldest_links`](Self::strip_oldest_links) resumes, so a flood
    /// visits each staged row once.
    linkless_front: usize,
    /// Rows staged since the last bounded drain opportunity
    /// ([`take_arrivals`](Self::take_arrivals)): the stream's rate, as
    /// `Grid::trickle_lazy_bounded` judges it.
    arrivals_since_drain: u64,
}

/// A hole at the front of the staged lines (the rows the flood path dropped
/// via [`LazyBuffer::shed_oldest`] / [`LazyBuffer::drop_oldest`]), shown as one
/// marker row instead of closing up silently.
#[derive(Debug, Default)]
struct FloodCut {
    /// Lines missing here that the configured line limit would still hold (what
    /// the marker says): grows as the flood drops rows the limit would keep,
    /// shrinks as the limit's window slides past the hole's oldest rows.
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
    #[inline]
    fn line(&self) -> &Line {
        self.line.get_or_init(|| flood_marker_line(self.dropped))
    }

    /// The marker line, owned — for a drain that promotes the cut into the store.
    #[inline]
    fn into_line(self) -> Line {
        let dropped = self.dropped;
        self.line
            .into_inner()
            .unwrap_or_else(|| flood_marker_line(dropped))
    }

    /// Make the marker name `dropped` lines (any built marker line is then
    /// stale).
    #[inline]
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

/// The text every flood marker row starts with: a reader telling the marker from content matches on this, not on the wording after the count.
pub(crate) const FLOOD_MARKER_PREFIX: &str = "— aterm dropped ";

/// The one row that stands in for `dropped` lines lost at a flood cut.
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

/// Bounded free-list of `Vec<Cell>` bodies recycled between deferred lines.
#[derive(Debug, Default)]
pub(crate) struct CellPool {
    /// Emptied cell bodies awaiting reuse (LIFO: the most recently returned
    /// body is the most likely to still be cache-warm).
    bufs: Vec<Vec<Cell>>,
    /// Running sum of the pooled bodies' capacities, in cells, so the cap check
    /// in `put` and `memory_used` stay O(1).
    pooled_cells: usize,
}

/// Free-list depth cap, sized off the drain batch: the compression worker
/// returns 256 bodies per batch, so this holds two batches of headroom for a
/// worker that wakes late.
const POOL_MAX_BUFFERS: usize = 512;

/// Total pooled cells cap (512 KiB of `Cell`s): the count cap alone scales with
/// width (512 × cols × 8 B, 4 MB at 1000 columns).
const POOL_MAX_CELLS: usize = 64 * 1024;

impl CellPool {
    /// Take a body for a new deferred line, or a fresh empty `Vec` when the
    /// pool is dry (the pool is an optimization, never a requirement).
    #[inline]
    fn take(&mut self) -> Vec<Cell> {
        match self.bufs.pop() {
            Some(buf) => {
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
        if cap == 0
            || self.bufs.len() >= POOL_MAX_BUFFERS
            || self.pooled_cells + cap > POOL_MAX_CELLS
        {
            return;
        }
        self.pooled_cells += cap;
        self.bufs.push(buf);
    }

    /// Drop every pooled body and the free-list's own allocation.
    #[inline]
    fn clear(&mut self) {
        self.bufs = Vec::new();
        self.pooled_cells = 0;
    }

    /// Heap bytes held by the free-list (bodies + the `Vec<Vec<Cell>>` spine).
    #[inline]
    const fn memory_used(&self) -> usize {
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
    pub(crate) const fn len(&self) -> usize {
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

    /// Push a pre-built deferred line to the back of the buffer.
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

    /// Rows staged since the previous call (see the `arrivals_since_drain` field), resetting the count: called once per bounded drain opportunity.
    #[inline]
    pub(crate) const fn take_arrivals(&mut self) -> u64 {
        let arrivals = self.arrivals_since_drain;
        self.arrivals_since_drain = 0;
        arrivals
    }

    /// Snapshot `row` into a pooled cell body and stage it.
    #[inline]
    pub(crate) fn push_row(&mut self, row: &Row, extras: ScrolledRowExtras) {
        let scratch = self.pool.take();
        self.stage(DeferredLine::new(row, extras, scratch));
    }

    /// [`push_row`](Self::push_row) for extras already in a `Box` ([`DeferredLine::new_boxed`]).
    #[inline]
    pub(crate) fn push_row_boxed(&mut self, row: &Row, extras: Option<Box<ScrolledRowExtras>>) {
        let scratch = self.pool.take();
        self.stage(DeferredLine::new_boxed(row, extras, scratch));
    }

    /// Number of pending lines: the staged rows plus the flood marker when a cut is open (a real history row to every reader).
    #[inline]
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.lines.len() + usize::from(self.cut.is_some())
    }

    /// Whether there are no staged rows and no open cut (a lone marker is still a row a drain must promote).
    #[inline]
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.cut.is_none()
    }

    /// Lines the open flood cut's marker names — `0` when no cut is open.
    #[inline]
    #[must_use]
    pub(crate) const fn cut_dropped(&self) -> u64 {
        match &self.cut {
            Some(cut) => cut.dropped,
            None => 0,
        }
    }

    /// Staged content rows: [`len`](Self::len) without the marker row.
    #[inline]
    #[must_use]
    pub(crate) fn staged_rows(&self) -> usize {
        self.lines.len()
    }

    /// How many of the oldest staged rows must go for the rest to fit in `budget` bytes (as the `staged_bytes` field sums them), never taking the buffer below `keep` rows (the marker counts as one, as in [`len`](Self::len)).
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

    /// Whether staged rows exceed `budget`, before a caller walks their weights.
    #[must_use]
    #[inline]
    pub(crate) const fn outweighs(&self, budget: usize) -> bool {
        self.staged_bytes > budget
    }

    /// Estimated peak at `line_cap` rows if incoming rows keep the average retained weight of the newest `sample` rows.
    #[must_use]
    pub(crate) fn projected_peak(&self, line_cap: usize, sample: usize) -> usize {
        let (rows, bytes) = self
            .lines
            .iter()
            .rev()
            .take(sample)
            .fold((0usize, 0usize), |(rows, bytes), line| {
                (rows + 1, bytes + line.staged_bytes())
            });
        if rows == 0 {
            return self.staged_bytes;
        }
        let per_row = bytes / rows;
        let to_come = line_cap.saturating_sub(self.len());
        self.staged_bytes
            .saturating_add(per_row.saturating_mul(to_come))
            .max(per_row.saturating_mul(line_cap))
    }

    /// The running staged-byte sum (see the field).
    #[cfg(test)]
    pub(crate) fn staged_bytes(&self) -> usize {
        self.assert_running_sums();
        self.staged_bytes
    }

    /// Test-only: both running sums equal a fresh walk, whichever path moved the rows, and the strip cursor's prefix holds no link.
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
    pub(crate) const fn cut_open(&self) -> bool {
        self.cut.is_some()
    }

    /// Whether the buffer has exceeded the drain threshold.
    #[inline]
    #[must_use]
    pub(crate) fn should_drain(&self) -> bool {
        self.lines.len() > DRAIN_THRESHOLD
    }

    /// Drain all pending lines as materialized [`Line`]s, oldest first.
    pub(crate) fn drain_all(&mut self) -> impl Iterator<Item = Line> + '_ {
        let marker = self.cut.take().map(FloodCut::into_line);
        self.staged_bytes = 0;
        self.link_bytes = 0;
        self.linkless_front = 0;
        let pool = &mut self.pool;
        marker.into_iter().chain(
            self.lines
                .drain(..)
                .map(move |deferred| deferred.into_line_recycled(&mut *pool)),
        )
    }

    /// Drain up to `n` of the oldest pending lines, oldest first: the bounded [`drain_all`](Self::drain_all) the compression worker uses to keep each term-lock hold short.
    pub(crate) fn drain_front(&mut self, n: usize) -> impl Iterator<Item = Line> + '_ {
        let n = n.min(self.len());
        let marker = if n > 0 {
            self.cut.take().map(FloodCut::into_line)
        } else {
            None
        };
        let n = n - usize::from(marker.is_some());
        let (leaving, leaving_links) =
            self.lines.range(..n).fold((0, 0), |(bytes, links), line| {
                (bytes + line.staged_bytes(), links + line.link_bytes())
            });
        self.staged_bytes -= leaving;
        self.link_bytes -= leaving_links;
        self.linkless_front = self.linkless_front.saturating_sub(n);
        let pool = &mut self.pool;
        marker.into_iter().chain(
            self.lines
                .drain(..n)
                .map(move |deferred| deferred.into_line_recycled(&mut *pool)),
        )
    }

    /// Drain up to `n` of the newest staged rows, oldest first, never the flood marker: a width resize taking back the overflow it staged itself (`Grid::restore_reflowed_scrollback`).
    pub(crate) fn drain_back(&mut self, n: usize) -> impl Iterator<Item = Line> + '_ {
        let start = self.lines.len() - n.min(self.lines.len());
        let (leaving, leaving_links) = self
            .lines
            .range(start..)
            .fold((0, 0), |(bytes, links), line| {
                (bytes + line.staged_bytes(), links + line.link_bytes())
            });
        self.staged_bytes -= leaving;
        self.link_bytes -= leaving_links;
        self.linkless_front = self.linkless_front.min(start);
        let pool = &mut self.pool;
        self.lines
            .drain(start..)
            .map(move |deferred| deferred.into_line_recycled(&mut *pool))
    }

    /// How many of the newest staged rows make up the logical line the newest one belongs to: its head and its continuations, or every staged row when no head is staged.
    pub(crate) fn trailing_run(&self) -> usize {
        self.lines
            .iter()
            .rev()
            .position(|line| !line.wrapped)
            .map_or(self.lines.len(), |continuations| continuations + 1)
    }

    /// A line by index (0 = oldest: the flood marker when a cut is open), materialized on first access.
    #[must_use]
    #[inline]
    pub(crate) fn get_line(&self, idx: usize) -> Option<&Line> {
        match &self.cut {
            Some(cut) if idx == 0 => Some(cut.line()),
            Some(_) => self.lines.get(idx - 1).map(DeferredLine::to_line),
            None => self.lines.get(idx).map(DeferredLine::to_line),
        }
    }

    /// Clear all pending lines and the recycling pool.
    #[inline]
    pub(crate) fn clear(&mut self) {
        self.lines.clear();
        self.staged_bytes = 0;
        self.link_bytes = 0;
        self.linkless_front = 0;
        self.pool.clear();
        self.cut = None;
    }

    /// Drop the recycled cell bodies, keeping the staged lines: after a width change the pooled capacities fit the old column count.
    #[inline]
    pub(crate) fn clear_pool(&mut self) {
        self.pool.clear();
    }

    /// Test-only: pooled cell-body count.
    #[cfg(test)]
    pub(crate) const fn pooled_bodies(&self) -> usize {
        self.pool.len()
    }

    /// Drop the `n` oldest deferred lines without materializing them, as a loss: the hole opens (or widens) the buffer's flood cut, so readers see one marker row naming how many lines are missing.
    pub(crate) fn drop_oldest(&mut self, n: usize) -> u64 {
        let n = self.discard_front(n) as u64;
        if n == 0 {
            return 0;
        }
        let cut = self.cut.get_or_insert_with(FloodCut::default);
        let dropped = cut.dropped.saturating_add(n);
        cut.set(dropped)
    }

    /// The flood path's drop: discard the `n` oldest staged rows (never the marker) and make the cut name `hole` lines, the missing lines the configured limit would still hold (`Grid::drop_flood_overflow`).
    pub(crate) fn shed_oldest(&mut self, n: usize, hole: u64) -> u64 {
        self.discard_front(n);
        if hole == 0 {
            self.cut = None;
            return 0;
        }
        self.cut.get_or_insert_with(FloodCut::default).set(hole)
    }

    /// Hand back the deque spine a flood grew: `VecDeque::drain` never shrinks, so a burst staging ~90k rows would otherwise keep ~10 MB of empty spine for the session.
    pub(crate) fn release_spine_slack(&mut self) {
        let floor = self.lines.len().max(DRAIN_THRESHOLD);
        if self.lines.capacity() > floor.saturating_mul(4) {
            self.lines.shrink_to(floor.saturating_mul(2));
        }
    }

    /// The deque spine's capacity, in rows.
    #[cfg(test)]
    pub(crate) fn spine_capacity(&self) -> usize {
        self.lines.capacity()
    }

    /// The shared body of the two drops: discard up to `n` front rows, never
    /// the cut.
    fn discard_front(&mut self, n: usize) -> usize {
        let n = n.min(self.lines.len());
        let pool = &mut self.pool;
        for mut deferred in self.lines.drain(..n) {
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
    pub(crate) const fn link_bytes(&self) -> usize {
        self.link_bytes
    }

    /// Strip the links of the oldest staged rows that hold any until at least `want` link bytes are freed or none are left; returns the bytes freed.
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

    /// Estimated bytes held by the staged (not yet drained) lines: the backing `VecDeque` capacity plus each deferred line's heap, so the ring byte watermark sees the backlog under flood backpressure.
    pub(crate) fn memory_used(&self) -> usize {
        let mut total = self
            .lines
            .capacity()
            .saturating_mul(std::mem::size_of::<DeferredLine>());
        for line in &self.lines {
            total = total.saturating_add(line.heap_memory_used());
        }
        total = total.saturating_add(self.pool.memory_used());
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

/// Attach a row's coalesced inline-image spans to the [`Line`] it produced
/// (one refcount bump per span).
fn attach_image_spans(line: &mut Line, extras: &ScrolledRowExtras) {
    if !extras.images.is_empty() {
        line.set_images(extras.images.clone());
    }
}

/// A scrolled-off row's cell extras (hyperlinks, complex chars, combining marks, RGB colours, underline colours, images), extracted before `shift_rows_up_by` discards them.
#[derive(Debug, Clone, Default)]
pub struct ScrolledRowExtras {
    /// Hyperlink spans (coalesced from per-cell URLs).
    pub hyperlinks: Vec<HyperlinkSpan>,
    /// Complex character strings keyed by physical column (`is_complex()` cells).
    pub complex_chars: Vec<(u16, Arc<str>)>,
    /// Combining characters keyed by physical column.
    pub combining: Vec<(u16, SmallVec<char, 2>)>,
    /// Resolved foreground colours keyed by physical column (RGB overflow cells).
    pub rgb_fg: Vec<(u16, [u8; 3])>,
    /// Resolved background colours keyed by physical column (as `rgb_fg`).
    pub rgb_bg: Vec<(u16, [u8; 3])>,
    /// Packed SGR 58 underline colours keyed by physical column (`0xTT_XXXXXX`; `0x01` = RGB, `0x02` = indexed), kept packed so an indexed colour re-resolves against the live palette, as the live cell does.
    pub underline_colors: Vec<(u16, u32)>,
    /// Inline-image spans (OSC 1337 / sixel), one per contiguous run of columns painting one footprint row: one `Arc` bump per run, not per column.
    pub images: Vec<ImageSpan>,
}

impl ScrolledRowExtras {
    /// Estimated bytes retained by this boxed extras value: vector capacities, out-of-line strings (shared ones charged per holder) and each image's per-row share.
    pub(crate) fn retained_memory_used(&self) -> usize {
        const fn capacity_bytes<T>(values: &Vec<T>) -> usize {
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
    #[inline]
    pub(crate) const fn is_empty(&self) -> bool {
        self.hyperlinks.is_empty()
            && self.complex_chars.is_empty()
            && self.combining.is_empty()
            && self.rgb_fg.is_empty()
            && self.rgb_bg.is_empty()
            && self.underline_colors.is_empty()
            && self.images.is_empty()
    }

    /// The OSC 8 share of [`retained_memory_used`](Self::retained_memory_used): the span vector's capacity plus each span's URL and `id=` bytes, what [`strip_hyperlinks`](Self::strip_hyperlinks) gives back.
    #[must_use]
    pub(crate) fn link_bytes(&self) -> usize {
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

    /// Drop this row's links, spans and vector both, keeping its text, colours and images: the history link budget's eviction (`Grid::history_link_budget`).
    #[inline]
    pub(crate) fn strip_hyperlinks(&mut self) {
        self.hyperlinks = Vec::new();
    }

    /// Clear all fields, retaining `Vec` capacities so the struct can be recycled as extraction scratch (see `extract_row_extras_into`).
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

/// Monotone read cursors over the four column-sorted `ScrolledRowExtras` vectors.
#[derive(Default)]
pub(in crate::grid) struct RowToLineCursorState {
    complex_idx: usize,
    combining_idx: usize,
    pub(in crate::grid) rgb_fg_idx: usize,
    pub(in crate::grid) rgb_bg_idx: usize,
}

impl Grid {
    /// Convert a row to a [`Line`] without extras (test helper).
    #[cfg(test)]
    pub(crate) fn row_to_line_static(row: &Row) -> Line {
        Self::row_to_line_with_stored_extras(row, &ScrolledRowExtras::default())
    }

    /// Convert a row to a [`Line`] using extras extracted at scroll time (RGB overflow colours already resolved).
    pub(crate) fn row_to_line_with_stored_extras(row: &Row, extras: &ScrolledRowExtras) -> Line {
        // As a staged line reads it (`DeferredLine::new_boxed`): a recorded
        Self::row_to_line_with_stored_extras_at_len(row, extras, row.written_len())
    }

    /// [`Self::row_to_line_with_stored_extras`] with an explicit cell count.
    pub(crate) fn row_to_line_with_stored_extras_at_len(
        row: &Row,
        extras: &ScrolledRowExtras,
        len: u16,
    ) -> Line {
        let len = usize::from(len.min(row.cols()));
        cells_to_line(&row.as_slice()[..len], extras, row.is_wrapped())
    }

    /// Extract a row's extras (hyperlinks, complex chars, combining marks and RGB colours) before `shift_rows_up_by` discards them.
    pub(crate) fn extract_row_extras(
        row: &Row,
        extras: &CellExtras,
        row_idx: u16,
    ) -> ScrolledRowExtras {
        let mut result = ScrolledRowExtras::default();
        Self::extract_row_extras_into(&mut result, row, extras, row_idx);
        result
    }

    /// [`Grid::extract_row_extras`] into a caller's struct (cleared first), so the scroll path can recycle a popped `ring_extras` allocation instead of reallocating its `Vec`s per styled row.
    pub(crate) fn extract_row_extras_into(
        result: &mut ScrolledRowExtras,
        row: &Row,
        extras: &CellExtras,
        row_idx: u16,
    ) {
        result.clear();
        if scrollback_text_only() {
            Self::extract_hyperlinks_only_into(result, row, extras, row_idx);
            return;
        }
        Self::extract_image_spans_into(result, row, extras, row_idx);
        let len = row.len() as usize;
        if len == 0 {
            return;
        }

        if !extras.has_any_data() {
            return;
        }

        let probe_map = extras.map_may_have_row(row_idx, len);

        if !probe_map && !row_has_extractable_cells(&row.as_slice()[..len]) {
            return;
        }

        if result.rgb_fg.capacity() == 0 || result.rgb_bg.capacity() == 0 {
            let cap_cells = &row.as_slice()[..len];
            let (mut n_fg, mut n_bg) = (0usize, 0usize);
            for (i, c) in cap_cells.iter().enumerate() {
                // Mirror the main loop's spacer skip so the reserve matches what
                if is_spacer(cap_cells, i) {
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

        let mut current_span: Option<(u16, Arc<str>, Option<Arc<str>>)> = None;

        let cells = &row.as_slice()[..len];
        for (physical_col, cell) in cells.iter().enumerate() {
            if is_spacer(cells, physical_col) {
                continue;
            }

            let col_u16 = u16::try_from(physical_col).unwrap_or(u16::MAX);
            let coord = CellCoord::new(row_idx, col_u16);

            if cell.is_complex() {
                if let Some(arc) = extras.complex_char_arc_for(row_idx, col_u16) {
                    result.complex_chars.push((col_u16, Arc::clone(arc)));
                } else if let Some(c) = extras.complex_codepoint_for(row_idx, col_u16) {
                    let mut buf = [0u8; 4];
                    let s = c.encode_utf8(&mut buf);
                    result.complex_chars.push((col_u16, Arc::from(s)));
                }
            }

            if cell.fg_needs_overflow()
                && let Some(rgb) = extras.fg_rgb_for(row_idx, col_u16)
            {
                result.rgb_fg.push((col_u16, rgb));
            }

            if cell.bg_needs_overflow()
                && let Some(rgb) = extras.bg_rgb_for(row_idx, col_u16)
            {
                result.rgb_bg.push((col_u16, rgb));
            }

            if let Some(extra) = probe_map.then(|| extras.get(coord)).flatten() {
                if !extra.combining().is_empty() {
                    result
                        .combining
                        .push((col_u16, SmallVec::from_slice(extra.combining())));
                }

                if let Some(packed) = extra.underline_color_u32() {
                    result.underline_colors.push((col_u16, packed));
                }

                let url = extra.hyperlink().cloned();
                let id = extra.hyperlink_id().cloned();
                match (&current_span, url) {
                    (None, Some(new_url)) => {
                        current_span = Some((col_u16, new_url, id));
                    }
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

    /// Coalesce this row's inline-image cells into [`ImageSpan`]s so a placement
    /// survives the trip into scrollback.
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
            let layers = if cell.has_extras() {
                extras
                    .get(CellCoord::new(row_idx, col_u16))
                    .map_or(&[][..], CellExtra::images)
            } else {
                &[]
            };
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

    /// Hyperlink-only extraction for the scrollback-text-only fast path: OSC 8
    /// spans (end_col exclusive), coalesced as [`Self::extract_row_extras_into`]
    /// does.
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

    /// Extract a row's extras and convert it to a [`Line`] in one step.
    #[cfg(test)]
    pub(crate) fn row_to_line_with_hyperlinks(
        row: &Row,
        extras: &CellExtras,
        row_idx: u16,
    ) -> Line {
        let extracted = Self::extract_row_extras(row, extras, row_idx);
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
#[inline]
pub(in crate::grid) fn next_complex_char<'a>(
    extras: &'a ScrolledRowExtras,
    cursors: &mut RowToLineCursorState,
    col_u16: u16,
) -> Option<&'a Arc<str>> {
    next_keyed(&extras.complex_chars, &mut cursors.complex_idx, col_u16)
}

/// The value a history row's column-keyed extras list (ascending by column) holds for `col_u16`, if the cursor `idx` is on it, advancing past it.
#[inline]
pub(in crate::grid) const fn next_keyed<'a, T>(
    entries: &'a [(u16, T)],
    idx: &mut usize,
    col_u16: u16,
) -> Option<&'a T> {
    if *idx < entries.len() && entries[*idx].0 == col_u16 {
        let value = &entries[*idx].1;
        *idx += 1;
        Some(value)
    } else {
        None
    }
}

/// Consume this column's stored combining marks, if the cursor is on them.
#[inline]
pub(in crate::grid) fn next_combining<'a>(
    extras: &'a ScrolledRowExtras,
    cursors: &mut RowToLineCursorState,
    col_u16: u16,
) -> Option<&'a [char]> {
    next_keyed(&extras.combining, &mut cursors.combining_idx, col_u16).map(|marks| marks.as_slice())
}

/// Resolve one channel of a cell's colour the way the Line serializer does: inline when the cell carries it, else the column's pre-resolved stored RGB (extraction resolved RGB overflow at scroll time), else the default.
#[inline]
pub(in crate::grid) const fn resolve_cell_color(
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

    match next_keyed(stored_colors, color_idx, col_u16) {
        Some(&[r, g, b]) => PackedColor::rgb(r, g, b).0,
        None => default_raw,
    }
}

/// Accumulates a materializing line's attribute runs, allocating nothing until
/// a second distinct `CellAttrs` appears.
struct AttrRunBuilder {
    /// Runs already closed out. Empty (and unallocated) until the first spill.
    rle: Rle<CellAttrs>,
    /// Value of the open run, seeded to `DEFAULT` so a plain line's first cell
    /// merges into it instead of taking the cold path.
    value: CellAttrs,
    /// How many cells the open run covers; `0` means nothing opened yet.
    len: u32,
    /// Whether any run was closed into `rle`: tells "one run, and it is
    /// default" (drop it) from "several runs, the last one default" (keep them).
    spilled: bool,
}

impl AttrRunBuilder {
    /// Not named `new`: the lock-order census resolves one-hop held calls by
    /// callee name, so a `new` would merge with every other `new` in the tree.
    const fn empty() -> Self {
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
    #[inline]
    fn extend(&mut self, attrs: CellAttrs, count: u32) {
        if count == 0 {
            return;
        }
        if self.value == attrs {
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

    /// Close the open run and yield the `Rle` for `Line::from_parts`.
    fn finish(mut self) -> Rle<CellAttrs> {
        if self.len != 0 && (self.spilled || !self.value.is_default()) {
            self.rle.extend_with(self.value, self.len);
        }
        self.rle
    }
}

fn push_repeated_attrs(attrs_rle: &mut AttrRunBuilder, attrs: CellAttrs, char_count: usize) {
    // One run of `char_count` copies, not a push per character.
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

/// Coalesce per-column packed underline colours (ascending by column) into `start, end)` `UnderlineColorSpan`s; a colour change or a column gap starts a new span, so filling the spans reproduces each column's colour.
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
}

#[cfg(test)]
mod extract_gate_tests {
    use super::*;
    use crate::{CellFlags, PackedColor};

    /// The map-empty gate gives no false negative: an RGB colour held only in
    /// the dense ring (mode bit on the cell, nothing in the map) still extracts.
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
        let extracted = Grid::extract_row_extras(row, grid.extras(), 0);
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
            Grid::extract_row_extras(row, grid.extras(), 0)
        };
        assert!(before.is_empty(), "a plain row extracts nothing");

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
            Grid::extract_row_extras(row, grid.extras(), 0)
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

    /// `is_empty` lets a plain row skip its boxed extras: over every subset of
    /// the seven fields it is true exactly when none is populated.
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
