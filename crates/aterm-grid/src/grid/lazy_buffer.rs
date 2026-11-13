// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The staging buffer between the ring and the tiered store ([`LazyBuffer`]): rows that left the
//! ring, kept as raw cells until the store takes them as [`Line`]s or a flood sheds them. Every
//! staged row's cells sit back to back in one arena, oldest row first, beside a compact record
//! of each row. Staging a row appends to both; shedding the oldest advances both fronts. A flood
//! that stages one row and sheds one on every line feed touches only their two ends, and
//! allocates nothing per row.

use std::cell::OnceCell;
use std::collections::VecDeque;

use aterm_rle::Rle;
use aterm_scrollback::{CellAttrs, ImageSpan, Line};

use super::scroll_convert::{ScrolledRowExtras, cells_to_line, cells_to_line_owned};
use crate::{Cell, CellFlags, Row};

/// A staged row: where its cells sit in the arena, the shape a reader rebuilds around them and
/// what it keeps beside them.
#[derive(Debug)]
struct StagedRow {
    /// The arena position of the row's first stored cell, counted from the first cell ever
    /// staged (see [`LazyBuffer::base`]).
    start: u64,
    /// Number of occupied cells (`Row::written_len`).
    len: u16,
    /// Leading cells of the row not stored, each [`Cell::EMPTY`]: output never wrote them
    /// ([`Row::blank_lead`]), and a reader gets them back blank.
    lead: u16,
    /// Whether the row was soft-wrapped.
    wrapped: bool,
    /// Preserved extras; `None` exactly when empty (plain-text rows), which the running sums and
    /// the materialize readers rely on.
    extras: Option<Box<ScrolledRowExtras>>,
    /// The materialized line, filled on first read. Boxed: few staged rows are read before a
    /// drain, and the box keeps the record at 32 bytes.
    cached: OnceCell<Box<Line>>,
}

/// What a staged row costs besides its cells and extras: its record.
const ROW_RECORD_BYTES: usize = std::mem::size_of::<StagedRow>();

/// What staging the widest row with no extras adds to the staged bytes: its record and
/// `u16::MAX` cells.
pub(crate) const WIDEST_PLAIN_ROW_BYTES: usize =
    ROW_RECORD_BYTES + u16::MAX as usize * std::mem::size_of::<Cell>();

impl StagedRow {
    /// Number of cells the arena holds for this row: its occupied cells past the blank lead.
    #[inline]
    fn stored(&self) -> usize {
        usize::from(self.len - self.lead)
    }

    /// What staging this row costs: its record, its occupied cells and everything its extras
    /// retain, link payload included. The blank lead counts though the arena does not hold it:
    /// the weight is what the row is, however staging copied it.
    #[inline]
    fn weight(&self) -> usize {
        // A record and at most `u16::MAX` cells: no overflow before the extras.
        (ROW_RECORD_BYTES + usize::from(self.len) * std::mem::size_of::<Cell>())
            .saturating_add(self.extras_bytes())
    }

    /// Everything the extras retain, boxed struct included.
    #[inline]
    fn extras_bytes(&self) -> usize {
        self.extras
            .as_deref()
            .map_or(0, ScrolledRowExtras::retained_memory_used)
    }

    /// The OSC 8 payload this row holds (`ScrolledRowExtras::link_bytes`).
    #[inline]
    fn link_bytes(&self) -> usize {
        self.extras
            .as_deref()
            .map_or(0, ScrolledRowExtras::link_bytes)
    }

    /// Heap bytes this row holds outside the arena and its record: its extras and the
    /// materialized line once read.
    #[inline]
    fn heap_memory_used(&self) -> usize {
        self.extras_bytes()
            .saturating_add(self.cached.get().map_or(0, |line| line.memory_used()))
    }

    /// Drop this row's links (and the materialized line holding them), keeping text, colours and
    /// images; returns the link bytes freed.
    fn strip_hyperlinks(&mut self) -> usize {
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

    /// Whether this row holds an image accepted by `selected`.
    fn any_image(&self, selected: &dyn Fn(&ImageSpan) -> bool) -> bool {
        self.extras
            .as_deref()
            .is_some_and(|extras| extras.images.iter().any(selected))
    }

    /// Inline-image spans held by this row, when non-empty.
    fn image_spans(&self) -> Option<&[ImageSpan]> {
        let spans = self.extras.as_deref()?.images.as_slice();
        (!spans.is_empty()).then_some(spans)
    }

    /// Replace this row's inline-image spans, returning its weight before and after.
    fn set_images(&mut self, spans: Vec<ImageSpan>) -> (usize, usize) {
        let before = self.weight();
        if spans.is_empty() {
            if let Some(extras) = self.extras.as_deref_mut() {
                extras.images.clear();
                if extras.is_empty() {
                    self.extras = None;
                }
            }
        } else if let Some(extras) = self.extras.as_deref_mut() {
            extras.images = spans;
        } else {
            self.extras = Some(Box::new(ScrolledRowExtras {
                images: spans,
                ..ScrolledRowExtras::default()
            }));
        }
        self.cached = OnceCell::new();
        (before, self.weight())
    }

    /// Retain accepted image spans and invalidate a previously materialized copy.
    fn retain_images(&mut self, keep: &mut dyn FnMut(&ImageSpan) -> bool) -> bool {
        let Some(extras) = self.extras.as_deref_mut() else {
            return false;
        };
        let count = extras.images.len();
        extras.images.retain(|span| keep(span));
        if count == extras.images.len() {
            return false;
        }
        if extras.is_empty() {
            self.extras = None;
        }
        self.cached = OnceCell::new();
        true
    }
}

/// Staging buffer between the ring and the tiered store in `GridStorage`.
#[derive(Debug)]
pub(crate) struct LazyBuffer {
    /// The staged rows, oldest first.
    rows: VecDeque<StagedRow>,
    /// Every staged row's stored cells, back to back in row order: `rows[0]`'s from the front,
    /// each next row's right after the last one's, and nothing after the newest row's.
    arena: VecDeque<Cell>,
    /// The arena position of `arena`'s front cell: the cells staged before it, ever, minus those
    /// a drain of the newest rows took back.
    base: u64,
    /// The flood cut at the front of `rows`, if open ([`FloodCut`]): logically the oldest row,
    /// which `len`, `get_line` and the drains present as one marker line.
    cut: Option<FloodCut>,
    /// Running sum of the rows' weights ([`StagedRow::weight`]), O(1) so the flood path can weigh
    /// the backlog against the store's headroom on every scrolled row.
    staged_bytes: usize,
    /// Running sum of the rows' link payloads: the staged rows' share of the history link budget
    /// (`Grid::history_link_budget`).
    link_bytes: usize,
    /// Number of staged rows with image spans, so [`any_image`](Self::any_image) skips a
    /// text-only backlog without visiting a row.
    image_rows: usize,
    /// `rows[..linkless_front]` hold no link bytes: where the next
    /// [`strip_oldest_links`](Self::strip_oldest_links) resumes, so a flood visits each staged
    /// row once.
    linkless_front: usize,
    /// Rows staged since the last bounded drain opportunity ([`take_arrivals`](Self::take_arrivals)):
    /// the stream's rate, as `Grid::trickle_lazy_bounded` judges it.
    arrivals_since_drain: u64,
}

/// A hole at the front of the staged rows (the rows the flood path dropped via
/// [`LazyBuffer::shed_oldest`] / [`LazyBuffer::drop_oldest`]), shown as one marker row instead of
/// closing up silently.
#[derive(Debug, Default)]
struct FloodCut {
    /// Lines missing here that the configured line limit would still hold (what the marker
    /// says): grows as the flood drops rows the limit would keep, shrinks as the limit's window
    /// slides past the hole's oldest rows.
    dropped: u64,
    /// The largest `dropped` this cut has reached: the grid's loss counter grows by each new
    /// peak, so a count that shrinks and grows back is not counted twice.
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

    /// Make the marker name `dropped` lines (any built marker line is then stale).
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

/// The text every flood marker row starts with: a reader telling the marker from content matches
/// on this, not on the wording after the count.
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

const DRAIN_THRESHOLD: usize = 1000;

/// The arena capacity, in cells, that [`LazyBuffer::release_spine_slack`] lets an emptied arena
/// keep: a synchronous drain's batch of rows of 128 columns, one MiB.
const ARENA_FLOOR_CELLS: usize = (DRAIN_THRESHOLD + 1) * 128;

impl LazyBuffer {
    /// Whether a staged row holds an image accepted by `selected`.
    pub(crate) fn any_image(&self, selected: &dyn Fn(&ImageSpan) -> bool) -> bool {
        self.image_rows != 0 && self.rows.iter().any(|row| row.any_image(selected))
    }

    /// Every inline-image spans the staged rows hold, oldest row first.
    pub(crate) fn images(&self) -> impl Iterator<Item = &ImageSpan> {
        self.rows
            .iter()
            .filter_map(StagedRow::image_spans)
            .flatten()
    }

    /// Retain accepted image spans without materializing or draining staged text.
    pub(crate) fn retain_images(&mut self, keep: &mut dyn FnMut(&ImageSpan) -> bool) -> bool {
        let mut changed = false;
        for row in &mut self.rows {
            let before = row.weight();
            let had_images = usize::from(row.image_spans().is_some());
            if row.retain_images(keep) {
                self.image_rows -= had_images - usize::from(row.image_spans().is_some());
                self.staged_bytes = self
                    .staged_bytes
                    .saturating_sub(before)
                    .saturating_add(row.weight());
                changed = true;
            }
        }
        changed
    }

    /// Create a new empty lazy buffer.
    pub(crate) const fn new() -> Self {
        Self {
            rows: VecDeque::new(),
            arena: VecDeque::new(),
            base: 0,
            cut: None,
            staged_bytes: 0,
            link_bytes: 0,
            image_rows: 0,
            linkless_front: 0,
            arrivals_since_drain: 0,
        }
    }

    /// The one place a row joins the buffer, so no running count can miss one: its cells
    /// `lead..len` of `row` go to the back of the arena and its record to the back of `rows`.
    #[inline]
    fn stage(&mut self, row: &Row, extras: Option<Box<ScrolledRowExtras>>) {
        let len = row.written_len();
        let lead = row.blank_lead().min(len);
        let cells = &row.as_slice()[..usize::from(len)];
        let (blank, written) = cells.split_at(usize::from(lead));
        debug_assert!(
            blank.iter().all(|cell| *cell == Cell::EMPTY),
            "a cell before the row's written window is not empty"
        );
        let start = self.base + self.arena.len() as u64;
        // A line's lone glyph (or a few) is pushed: the slice copy is a `memcpy` call, which
        // costs more than such a copy.
        if written.len() <= 4 {
            for &cell in written {
                self.arena.push_back(cell);
            }
        } else {
            self.arena.extend(written);
        }
        let staged = StagedRow {
            start,
            len,
            lead,
            wrapped: row.is_wrapped(),
            extras: extras.filter(|extras| !extras.is_empty()),
            cached: OnceCell::new(),
        };
        self.staged_bytes += staged.weight();
        if staged.extras.is_some() {
            self.link_bytes += staged.link_bytes();
            self.image_rows += usize::from(staged.image_spans().is_some());
        }
        self.arrivals_since_drain = self.arrivals_since_drain.saturating_add(1);
        self.rows.push_back(staged);
    }

    /// Rows staged since the previous call (see the `arrivals_since_drain` field), resetting the
    /// count: called once per bounded drain opportunity.
    #[inline]
    pub(crate) const fn take_arrivals(&mut self) -> u64 {
        let arrivals = self.arrivals_since_drain;
        self.arrivals_since_drain = 0;
        arrivals
    }

    /// Stage a copy of `row` with its extras.
    #[inline]
    pub(crate) fn push_row(&mut self, row: &Row, extras: ScrolledRowExtras) {
        self.stage(row, (!extras.is_empty()).then(|| Box::new(extras)));
    }

    /// [`push_row`](Self::push_row) for extras already in a `Box` (scroll-off popping
    /// `ring_extras`): no unbox/re-box per line under `term_lock`.
    #[inline]
    pub(crate) fn push_row_boxed(&mut self, row: &Row, extras: Option<Box<ScrolledRowExtras>>) {
        self.stage(row, extras);
    }

    /// Number of pending lines: the staged rows plus the flood marker when a cut is open (a real
    /// history row to every reader).
    #[inline]
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.rows.len() + usize::from(self.cut.is_some())
    }

    /// Whether there are no staged rows and no open cut (a lone marker is still a row a drain
    /// must promote).
    #[inline]
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.cut.is_none()
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
        self.rows.len()
    }

    /// How many of the oldest staged rows must go for the rest to fit in `budget` bytes (as the
    /// `staged_bytes` field sums them), never taking the buffer below `keep` rows (the marker
    /// counts as one, as in [`len`](Self::len)).
    #[must_use]
    pub(crate) fn front_rows_over(&self, budget: usize, keep: usize) -> usize {
        let mut excess = self.staged_bytes.saturating_sub(budget);
        let removable = self.len().saturating_sub(keep).min(self.rows.len());
        let mut n = 0;
        while excess > 0 && n < removable {
            excess = excess.saturating_sub(self.rows[n].weight());
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

    /// Estimated peak at `line_cap` rows if incoming rows keep the average weight of the newest
    /// `sample` rows.
    #[must_use]
    pub(crate) fn projected_peak(&self, line_cap: usize, sample: usize) -> usize {
        let (rows, bytes) = self
            .rows
            .iter()
            .rev()
            .take(sample)
            .fold((0usize, 0usize), |(rows, bytes), row| {
                (rows + 1, bytes + row.weight())
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

    /// Test-only: what staging a plain row of `cells` occupied cells costs.
    #[cfg(test)]
    pub(crate) const fn row_bytes(cells: usize) -> usize {
        ROW_RECORD_BYTES + cells * std::mem::size_of::<Cell>()
    }

    /// Test-only: how many cells the arena holds for staged row `idx` (0 = oldest staged row).
    #[cfg(test)]
    pub(crate) fn stored_cells(&self, idx: usize) -> usize {
        self.rows[idx].stored()
    }

    /// Test-only: running counts equal a fresh walk, whichever path moved the rows, the arena
    /// holds the rows' cells back to back, and the strip cursor's prefix holds no link.
    #[cfg(test)]
    pub(crate) fn assert_running_sums(&self) {
        assert_eq!(
            self.staged_bytes,
            self.rows.iter().map(StagedRow::weight).sum::<usize>(),
            "the running staged-byte sum drifted from the rows it sums"
        );
        assert_eq!(
            self.link_bytes,
            self.rows.iter().map(StagedRow::link_bytes).sum::<usize>(),
            "the running link-byte sum drifted from the rows it sums"
        );
        assert!(
            self.rows
                .range(..self.linkless_front.min(self.rows.len()))
                .all(|row| row.link_bytes() == 0),
            "a staged row before the strip cursor still holds a link"
        );
        assert_eq!(
            self.image_rows,
            self.rows
                .iter()
                .filter(|row| row.image_spans().is_some())
                .count(),
            "the image-row count drifted from staged rows"
        );
        let mut next = self.base;
        for row in &self.rows {
            assert_eq!(
                row.start, next,
                "a staged row's cells do not follow the last row's"
            );
            next += row.stored() as u64;
        }
        assert_eq!(
            next,
            self.base + self.arena.len() as u64,
            "the arena holds cells past the newest row's"
        );
    }

    /// Test-only: how many staged rows' cells straddle the arena's ring (read as two runs).
    #[cfg(test)]
    pub(crate) fn straddling_rows(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| row.stored() > 1 && !cells_of(&self.arena, self.base, row).1.is_empty())
            .count()
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
        self.rows.len() > DRAIN_THRESHOLD
    }

    /// `row` converted into a [`Line`] ([`materialize`]).
    #[inline]
    fn materialize(&self, row: &StagedRow) -> Line {
        materialize(&self.arena, self.base, row)
    }

    /// Take the oldest staged row's weight and counts out of the running sums.
    #[inline]
    fn unaccount(&mut self, row: &StagedRow) {
        self.staged_bytes -= row.weight();
        if row.extras.is_some() {
            self.link_bytes -= row.link_bytes();
            self.image_rows -= usize::from(row.image_spans().is_some());
        }
    }

    /// The oldest staged row as a [`Line`] (its read copy, or converted now), removed with its
    /// cells.
    fn pop_front_line(&mut self) -> Option<Line> {
        let mut row = self.rows.pop_front()?;
        self.unaccount(&row);
        self.linkless_front = self.linkless_front.saturating_sub(1);
        let line = match row.cached.take() {
            Some(line) => *line,
            None => materialize_owned(&self.arena, self.base, &mut row),
        };
        let stored = row.stored();
        self.arena.drain(..stored);
        self.base += stored as u64;
        Some(line)
    }

    /// Drain all pending lines as materialized [`Line`]s, oldest first.
    pub(crate) fn drain_all(&mut self) -> Drain<'_> {
        self.drain_front(usize::MAX)
    }

    /// Everything staged, moved out whole: a width change's job materializes it on the reflow
    /// worker (`PendingScrollbackReflow`) rather than by [`drain_all`](Self::drain_all) under
    /// the caller's lock, which takes 61 ms for the 99,871 rows a flood leaves staged (measured
    /// at the desk). The arrival count stays.
    pub(crate) fn take_staged(&mut self) -> Self {
        Self {
            rows: std::mem::take(&mut self.rows),
            arena: std::mem::take(&mut self.arena),
            base: std::mem::take(&mut self.base),
            cut: self.cut.take(),
            staged_bytes: std::mem::take(&mut self.staged_bytes),
            link_bytes: std::mem::take(&mut self.link_bytes),
            image_rows: std::mem::take(&mut self.image_rows),
            linkless_front: std::mem::take(&mut self.linkless_front),
            arrivals_since_drain: 0,
        }
    }

    /// Put back what [`take_staged`](Self::take_staged) moved out, into a buffer nothing was
    /// staged in since. The arrival count stays this buffer's.
    pub(crate) fn restore_staged(&mut self, staged: Self) {
        debug_assert!(self.is_empty(), "restore_staged over staged rows");
        *self = Self {
            arrivals_since_drain: self.arrivals_since_drain,
            ..staged
        };
    }

    /// Drain up to `n` of the oldest pending lines, oldest first: the bounded
    /// [`drain_all`](Self::drain_all) the compression worker uses to keep each term-lock hold
    /// short. Lines the caller does not take are dropped when the [`Drain`] is.
    pub(crate) fn drain_front(&mut self, n: usize) -> Drain<'_> {
        let n = n.min(self.len());
        let marker = if n > 0 {
            self.cut.take().map(FloodCut::into_line)
        } else {
            None
        };
        let rows = n - usize::from(marker.is_some());
        Drain {
            buffer: self,
            marker,
            rows,
        }
    }

    /// Drain up to `n` of the newest staged rows, oldest first, never the flood marker: a width
    /// resize taking back the overflow it staged itself (`Grid::restore_reflowed_scrollback`).
    pub(crate) fn drain_back(&mut self, n: usize) -> std::vec::IntoIter<Line> {
        let keep = self.rows.len() - n.min(self.rows.len());
        let Some(first) = self.rows.get(keep) else {
            return Vec::new().into_iter();
        };
        let cells_end = usize::try_from(first.start - self.base).unwrap_or(usize::MAX);
        for row in self.rows.range(keep..) {
            self.staged_bytes -= row.weight();
            if row.extras.is_some() {
                self.link_bytes -= row.link_bytes();
                self.image_rows -= usize::from(row.image_spans().is_some());
            }
        }
        let (arena, base) = (&self.arena, self.base);
        let lines: Vec<Line> = self
            .rows
            .drain(keep..)
            .map(|mut row| match row.cached.take() {
                Some(line) => *line,
                None => materialize_owned(arena, base, &mut row),
            })
            .collect();
        self.arena.truncate(cells_end);
        self.linkless_front = self.linkless_front.min(keep);
        lines.into_iter()
    }

    /// How many of the newest staged rows make up the logical line the newest one belongs to: its
    /// head and its continuations, or every staged row when no head is staged.
    pub(crate) fn trailing_run(&self) -> usize {
        self.rows
            .iter()
            .rev()
            .position(|row| !row.wrapped)
            .map_or(self.rows.len(), |continuations| continuations + 1)
    }

    /// A line by index (0 = oldest: the flood marker when a cut is open), materialized on first
    /// access.
    #[must_use]
    #[inline]
    pub(crate) fn get_line(&self, idx: usize) -> Option<&Line> {
        let row = match &self.cut {
            Some(cut) if idx == 0 => return Some(cut.line()),
            Some(_) => self.rows.get(idx - 1)?,
            None => self.rows.get(idx)?,
        };
        Some(&**row.cached.get_or_init(|| Box::new(self.materialize(row))))
    }

    /// Inline-image spans on staged line `idx` (`0` = oldest, including the flood marker),
    /// without materializing text.
    #[must_use]
    #[inline]
    pub(crate) fn line_images(&self, idx: usize) -> Option<&[ImageSpan]> {
        let row = match &self.cut {
            Some(_) if idx == 0 => return None,
            Some(_) => self.rows.get(idx - 1)?,
            None => self.rows.get(idx)?,
        };
        row.image_spans()
    }

    /// Replace inline-image spans on staged line `idx` (`0` = oldest, including the flood
    /// marker), reconciling the running sums.
    pub(crate) fn set_line_images(&mut self, idx: usize, spans: Vec<ImageSpan>) -> bool {
        let row = match &self.cut {
            Some(_) if idx == 0 => return false,
            Some(_) => self.rows.get_mut(idx - 1),
            None => self.rows.get_mut(idx),
        };
        let Some(row) = row else {
            return false;
        };
        let had_images = usize::from(row.image_spans().is_some());
        let (before, after) = row.set_images(spans);
        self.image_rows = self.image_rows - had_images + usize::from(row.image_spans().is_some());
        self.staged_bytes = self
            .staged_bytes
            .saturating_sub(before)
            .saturating_add(after);
        true
    }

    /// Clear all pending lines, handing back the arena and the record spine a flood grew.
    #[inline]
    pub(crate) fn clear(&mut self) {
        self.rows.clear();
        self.arena.clear();
        self.base = 0;
        self.staged_bytes = 0;
        self.link_bytes = 0;
        self.image_rows = 0;
        self.linkless_front = 0;
        self.cut = None;
        self.release_spine_slack();
    }

    /// Drop the `n` oldest staged rows without materializing them, as a loss: the hole opens (or
    /// widens) the buffer's flood cut, so readers see one marker row naming how many lines are
    /// missing.
    pub(crate) fn drop_oldest(&mut self, n: usize) -> u64 {
        let n = self.discard_front(n) as u64;
        if n == 0 {
            return 0;
        }
        let cut = self.cut.get_or_insert_with(FloodCut::default);
        let dropped = cut.dropped.saturating_add(n);
        cut.set(dropped)
    }

    /// The flood path's drop: discard the `n` oldest staged rows (never the marker) and make the
    /// cut name `hole` lines, the missing lines the configured limit would still hold
    /// (`Grid::drop_flood_overflow`).
    pub(crate) fn shed_oldest(&mut self, n: usize, hole: u64) -> u64 {
        self.discard_front(n);
        if hole == 0 {
            self.cut = None;
            return 0;
        }
        self.cut.get_or_insert_with(FloodCut::default).set(hole)
    }

    /// Hand back the capacity a flood grew: neither the record spine nor the arena shrinks as
    /// rows leave, so a burst staging ~90k rows would otherwise keep their memory for the
    /// session.
    pub(crate) fn release_spine_slack(&mut self) {
        let floor = self.rows.len().max(DRAIN_THRESHOLD);
        if self.rows.capacity() > floor.saturating_mul(4) {
            self.rows.shrink_to(floor.saturating_mul(2));
        }
        let floor = self.arena.len().max(ARENA_FLOOR_CELLS);
        if self.arena.capacity() > floor.saturating_mul(4) {
            self.arena.shrink_to(floor.saturating_mul(2));
        }
    }

    /// The record spine's capacity, in rows.
    #[cfg(test)]
    pub(crate) fn spine_capacity(&self) -> usize {
        self.rows.capacity()
    }

    /// The arena's capacity, in cells.
    #[cfg(test)]
    pub(crate) fn arena_capacity(&self) -> usize {
        self.arena.capacity()
    }

    /// Remove the `n` oldest staged rows (never the marker) with their cells; returns how many
    /// went.
    fn discard_front(&mut self, n: usize) -> usize {
        let n = n.min(self.rows.len());
        if n == 0 {
            return 0;
        }
        // Popped one at a time: a flood sheds one row per line feed, and `drain`'s iterator and
        // drop guard cost more than the row itself.
        for _ in 0..n {
            if let Some(row) = self.rows.pop_front() {
                self.unaccount(&row);
            }
        }
        let end = self
            .rows
            .front()
            .map_or(self.base + self.arena.len() as u64, |row| row.start);
        let cells = usize::try_from(end - self.base).unwrap_or(usize::MAX);
        if cells == 1 {
            self.arena.pop_front();
        } else {
            self.arena.drain(..cells);
        }
        self.base = end;
        self.linkless_front = self.linkless_front.saturating_sub(n);
        n
    }

    /// The staged rows' OSC 8 payload (the running sum), O(1).
    #[inline]
    #[must_use]
    pub(crate) const fn link_bytes(&self) -> usize {
        self.link_bytes
    }

    /// Strip the links of the oldest staged rows that hold any until at least `want` link bytes
    /// are freed or none are left; returns the bytes freed.
    pub(crate) fn strip_oldest_links(&mut self, want: usize) -> usize {
        let mut freed = 0;
        while freed < want && self.linkless_front < self.rows.len() {
            let row = &mut self.rows[self.linkless_front];
            self.linkless_front += 1;
            let before = row.weight();
            let links = row.strip_hyperlinks();
            if links == 0 {
                continue;
            }
            self.staged_bytes -= before - row.weight();
            self.link_bytes -= links;
            freed += links;
        }
        freed
    }

    /// Estimated bytes held by the staged (not yet drained) lines: the record spine's and the
    /// arena's capacities plus each row's extras and read copy, so the ring byte watermark sees
    /// the backlog under flood backpressure.
    pub(crate) fn memory_used(&self) -> usize {
        let mut total = self
            .rows
            .capacity()
            .saturating_mul(ROW_RECORD_BYTES)
            .saturating_add(
                self.arena
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Cell>()),
            );
        for row in &self.rows {
            total = total.saturating_add(row.heap_memory_used());
        }
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

/// `row`'s stored cells in `arena` (whose front cell is at position `base`), as at most two runs
/// (the second is empty unless the row's cells wrap around the arena's ring).
#[inline]
fn cells_of<'a>(arena: &'a VecDeque<Cell>, base: u64, row: &StagedRow) -> (&'a [Cell], &'a [Cell]) {
    let from = usize::try_from(row.start - base).unwrap_or(usize::MAX);
    let to = from + row.stored();
    let (front, back) = arena.as_slices();
    if to <= front.len() {
        (&front[from..to], &[])
    } else if from >= front.len() {
        (&back[from - front.len()..to - front.len()], &[])
    } else {
        #[cfg(test)]
        let to = to - usize::from(crate::test_counters::short_straddle_read());
        (&front[from..], &back[..to - front.len()])
    }
}

/// `row` (its cells in `arena`, whose front cell is at position `base`) converted into a
/// [`Line`], its blank lead put back.
fn materialize(arena: &VecDeque<Cell>, base: u64, row: &StagedRow) -> Line {
    let default_extras = ScrolledRowExtras::default();
    let extras = row.extras.as_deref().unwrap_or(&default_extras);
    let (cells, rest) = cells_of(arena, base, row);
    if row.lead == 0 && rest.is_empty() {
        return cells_to_line(cells, extras, row.wrapped);
    }
    cells_to_line(&whole_cells(row, cells, rest), extras, row.wrapped)
}

/// [`materialize`] for a row leaving the buffer: its extras are taken from it, and their link
/// and image spans move into the line instead of being cloned.
fn materialize_owned(arena: &VecDeque<Cell>, base: u64, row: &mut StagedRow) -> Line {
    let extras = row.extras.take().map(|extras| *extras).unwrap_or_default();
    let (cells, rest) = cells_of(arena, base, row);
    if row.lead == 0 && rest.is_empty() {
        return cells_to_line_owned(cells, extras, row.wrapped);
    }
    cells_to_line_owned(&whole_cells(row, cells, rest), extras, row.wrapped)
}

/// `row`'s occupied cells in one slice: its blank lead, then its stored `cells` and the `rest`
/// an arena wrap split off them.
fn whole_cells(row: &StagedRow, cells: &[Cell], rest: &[Cell]) -> Vec<Cell> {
    let mut whole = Vec::with_capacity(usize::from(row.len));
    whole.resize(usize::from(row.lead), Cell::EMPTY);
    whole.extend_from_slice(cells);
    whole.extend_from_slice(rest);
    whole
}

/// The lines [`LazyBuffer::drain_front`] takes, oldest first: the flood marker when the cut was
/// open, then the staged rows, each converted as it is taken. Dropping it before the end drops
/// the rows it did not yield, as a drain of a range does.
pub(crate) struct Drain<'a> {
    buffer: &'a mut LazyBuffer,
    marker: Option<Line>,
    rows: usize,
}

impl Iterator for Drain<'_> {
    type Item = Line;

    #[inline]
    fn next(&mut self) -> Option<Line> {
        if let Some(marker) = self.marker.take() {
            return Some(marker);
        }
        if self.rows == 0 {
            return None;
        }
        self.rows -= 1;
        self.buffer.pop_front_line()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.rows + usize::from(self.marker.is_some());
        (left, Some(left))
    }
}

impl ExactSizeIterator for Drain<'_> {}

impl Drop for Drain<'_> {
    fn drop(&mut self) {
        self.buffer.discard_front(self.rows);
    }
}
