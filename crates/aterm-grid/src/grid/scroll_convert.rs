// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Row-to-line conversion for scrollback.
//!
//! Converts between visible grid rows and scrollback [`Line`]s.
//! Used by scroll operations when pushing rows to or recovering rows from
//! the scrollback buffer.
//!
//! ## Lazy Scrollback Promotion
//!
//! [`DeferredLine`] captures a grid row's raw cell bytes + extras at scroll
//! time via O(1) memcpy, deferring the O(cols) text/attrs conversion until
//! the line is actually read. This eliminates the `row_to_line_with_stored_extras`
//! bottleneck during burst output when scrollback is never read.

use std::cell::OnceCell;
use std::collections::VecDeque;
use std::sync::Arc;

use aterm_alloc::SmallVec;
use aterm_rle::Rle;
use aterm_scrollback::{CellAttrs, HyperlinkSpan, Line, UnderlineColorSpan};

use super::Grid;
use crate::Cell;
use crate::PackedColor;
use crate::Row;
use crate::StyleTable;
use crate::{CellCoord, CellExtras};

use std::sync::atomic::{AtomicBool, Ordering};

/// Process-global "headless scrollback-text-only" toggle (default off). When on,
/// the scroll path skips per-cell extras extraction so scrollback keeps text but
/// not colour/style — a ~10% throughput win for headless embeddings that read
/// scrollback as text. Process-global (not per-grid) because an embedding is
/// uniformly headless or GUI; aterm's GUI never enables it.
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
/// `Cell::is_wide_continuation()` returns `true` for both. Row→line
/// materialization SKIPS continuation spacers — if it skipped protected cells
/// too, every DECSCA-protected character would vanish from scrollback (and thus
/// from `line`/search/copy of history). A true spacer has bit 10 set, is not
/// itself `WIDE`, and immediately follows a `WIDE` main cell.
#[inline]
// `pub(in crate::grid)`: the ring fast materializer
// (`scroll_materialize::materialize_from_row_extras`) must skip EXACTLY the
// cells this skips, or its columns diverge from the Line round trip's. Shared,
// not re-implemented — a second copy of this predicate is a silent divergence
// waiting to happen.
pub(in crate::grid) fn is_spacer(cells: &[Cell], idx: usize) -> bool {
    cells[idx].is_wide_continuation()
        && !cells[idx].is_wide()
        && idx > 0
        && cells[idx - 1].is_wide()
}

/// Whether any cell in `cells` carries something
/// [`Grid::extract_row_extras_into`] could extract WITHOUT the extras map: a
/// style id to resolve against the table, a complex-char codepoint, or an RGB
/// overflow colour.
///
/// Deliberately does NOT test `Cell::has_extras()`. This is only ever consulted
/// when the map is EMPTY, where `extras.get(coord)` returns `None` for every
/// column no matter what that flag says — so ignoring it is exact, and it also
/// means a STALE flag (bulk clears leave those behind by design, see
/// `grid/proofs_kani_extras_invariants.rs`) cannot defeat the gate.
///
/// Branch-free fold (`|`, not `||`): the case this exists for is the row that
/// has nothing, which visits every cell either way, so an unconditional
/// accumulate beats a short-circuit whose branch is unpredictable. All four
/// tests are bit tests on the 8-byte cell the loop is already streaming.
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

/// Cell layout version guard. If the Cell layout changes, deferred lines
/// created under the old layout must not be materialized as-is.
/// Bump this when `Cell`'s `repr(C, packed)` layout changes.
pub(crate) const CELL_LAYOUT_VERSION: u8 = 1;

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
/// For an 80-column ASCII row: ~640 bytes of cell data (heap Vec),
/// plus the `ScrolledRowExtras` (often `None` for plain text). This is
/// larger than the ~160-byte `Line` equivalent, but the deferred line is
/// short-lived — it converts on first read or drains in bulk when the lazy
/// buffer exceeds its threshold.
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
    /// Cell layout version at creation time.
    #[allow(dead_code, reason = "safety guard for future Cell layout changes")]
    layout_version: u8,
    /// Cached materialized Line. Populated on first access.
    cached: OnceCell<Line>,
}

impl DeferredLine {
    /// Create a deferred line by snapshotting a Row's cell data into `scratch`
    /// — a recycled cell body from the [`CellPool`], or `Vec::new()` when the
    /// caller has none.
    ///
    /// This is O(cells) memcpy but avoids the O(cols) text extraction,
    /// RLE attribute building, and String allocation of full conversion.
    ///
    /// LATENCY: taking the body from the pool instead of `to_vec()`-ing a fresh
    /// one removes one malloc (and later one free) PER SCROLLED LINE from
    /// inside the PTY reader's `term_lock` hold — the hold the UI thread's
    /// keystroke-echo present waits behind. `clear` + `extend_from_slice` fully
    /// defines both the length and every element from the row slice, so a
    /// recycled body carries NO state from its previous line: content is
    /// byte-identical to the old `to_vec()`.
    pub(crate) fn new(row: &Row, extras: ScrolledRowExtras, scratch: Vec<Cell>) -> Self {
        // Box only when there is something to carry — the `None ⟺ empty`
        // encoding `heap_memory_used` and the materialize readers depend on.
        Self::new_boxed(row, (!extras.is_empty()).then(|| Box::new(extras)), scratch)
    }

    /// [`new`](Self::new) for a caller that ALREADY owns the extras in a `Box`.
    ///
    /// The tiered scroll-off path pops the evicted row's extras out of
    /// `ring_extras` as an `Option<Box<ScrolledRowExtras>>` and hands them
    /// straight here, so the value is never moved out of its box only to be
    /// re-boxed — one `Box` malloc AND one free removed per extras-carrying
    /// scrolled line, from inside the PTY reader's `term_lock` hold (the same
    /// hold the [`CellPool`] exists to keep the allocator out of).
    pub(crate) fn new_boxed(
        row: &Row,
        extras: Option<Box<ScrolledRowExtras>>,
        mut scratch: Vec<Cell>,
    ) -> Self {
        let len = row.len();
        scratch.clear();
        if len != 0 {
            scratch.extend_from_slice(&row.as_slice()[..len as usize]);
        }
        Self {
            cells: scratch,
            len,
            // `filter` keeps the `None ⟺ empty` encoding exactly as the old
            // `is_empty()` test did: an empty box is dropped rather than stored,
            // so `heap_memory_used` (which adds a flat struct size for `Some`)
            // and the two `materialize` readers see the same shape as before.
            extras: extras.filter(|b| !b.is_empty()),
            wrapped: row.is_wrapped(),
            layout_version: CELL_LAYOUT_VERSION,
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

    /// Estimated HEAP bytes owned by this deferred line (excludes
    /// `size_of::<DeferredLine>()` itself — the container counts that via its
    /// backing capacity). Extras are counted shallow, matching the
    /// `ring_extras` convention in `Grid::memory_used`.
    pub(crate) fn heap_memory_used(&self) -> usize {
        let mut total = self.cells.capacity() * std::mem::size_of::<Cell>();
        if self.extras.is_some() {
            total += std::mem::size_of::<ScrolledRowExtras>();
        }
        if let Some(line) = self.cached.get() {
            total += line.memory_used();
        }
        total
    }

    /// Convert into an owned [`Line`], consuming the deferred line AND its cell
    /// body. Test-only: every production consumption site goes through
    /// [`into_line_recycled`](Self::into_line_recycled) so the body comes back to
    /// the pool — a non-recycling variant on the flood path would silently
    /// reintroduce the per-newline free/malloc this pool exists to remove.
    ///
    /// Returns the cached line if already materialized, otherwise performs
    /// the conversion.
    #[cfg(test)]
    pub(crate) fn into_line(self) -> Line {
        self.into_line_and_body().0
    }

    /// [`into_line`](Self::into_line), returning the now-free cell body to
    /// `pool` for the next scroll-off to fill.
    ///
    /// Every consumption site the flood path reaches goes through here: without
    /// it the bodies the reader just allocated are handed straight back to the
    /// allocator at drain time, and the next 256/1000 scrolled lines malloc
    /// again inside the reader's lock hold.
    pub(crate) fn into_line_recycled(self, pool: &mut CellPool) -> Line {
        let (line, body) = self.into_line_and_body();
        pool.put(body);
        line
    }

    /// Shared body of the two `into_line` forms: yields the materialized line
    /// AND the cell `Vec` it was built from (already emptied of meaning — the
    /// caller either drops it or pools it). Taking the body out FIRST keeps the
    /// already-cached early-return recycling too; that path owns a body just as
    /// much as the materializing one.
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
            return line;
        }

        // Delegate to the same conversion logic used by the eager path.
        // Build a temporary view that mimics what row_to_line_with_stored_extras does.
        let cells = &self.cells[..self.len as usize];

        // Fast path: no extras.
        if extras.is_empty() {
            return Self::materialize_no_extras(cells, self.wrapped);
        }

        Self::materialize_with_extras(cells, extras, self.wrapped)
    }

    /// Fast-path materialization for rows with no extras.
    fn materialize_no_extras(cells: &[Cell], wrapped: bool) -> Line {
        let mut text = String::with_capacity(cells.len());
        let mut attrs_rle = AttrRunBuilder::empty();

        for (idx, cell) in cells.iter().enumerate() {
            if is_spacer(cells, idx) {
                continue;
            }
            text.push(cell.char());
            let fg_raw = cell.fg_color().map_or(PackedColor::DEFAULT_FG.0, |c| c.0);
            let bg_raw = cell.bg_color().map_or(PackedColor::DEFAULT_BG.0, |c| c.0);
            attrs_rle.push(CellAttrs::from_raw(fg_raw, bg_raw, cell.flags().bits()));
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
    /// [`CellPool`]). Lives HERE, not beside the buffer, so every producer and
    /// every consumer of a `DeferredLine` is on the recycling path by
    /// construction — a future consumption site cannot silently forget to
    /// return the body, and the pool's bytes fold into
    /// [`memory_used`](Self::memory_used) automatically.
    pool: CellPool,
}

/// Maximum number of deferred lines before automatic drain to tiered scrollback.
const DRAIN_THRESHOLD: usize = 1000;

/// Bounded free-list of `Vec<Cell>` bodies, recycled between deferred lines.
///
/// WHY: every row that scrolls off the ring into tiered scrollback becomes a
/// `DeferredLine`, whose cell snapshot used to be a fresh `to_vec()` — one
/// malloc now, one free at drain time, PER NEWLINE. That malloc runs inside the
/// single `term_lock` hold the PTY reader takes for a whole read batch: at 80
/// columns a 64 KiB batch is ~800 newlines, so ~800 malloc/free pairs sit on the
/// hold that the UI thread's keystroke-echo present is blocked behind. The
/// bodies are strictly transient (staged, materialized, freed), so handing them
/// back to the next scroll-off costs nothing and removes the allocator from the
/// per-newline path entirely.
///
/// BOUND: production takes one body per scrolled line; consumption returns them
/// in batches (the GUI compression worker drains 256 at a time, the inline drain
/// up to `DRAIN_THRESHOLD`). So the pool is fullest right after a drain and
/// empty just before the next one — the bodies in flight (staged + pooled) stay
/// roughly constant rather than being new footprint. It is capped twice anyway,
/// by buffer COUNT and by total pooled CELLS, so neither a very deep backlog nor
/// a very wide window can turn the free-list into idle megabytes; whatever it
/// does hold is reported through [`LazyBuffer::memory_used`], so the ring byte
/// watermark still sees it.
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

/// Free-list depth cap. Sized off the DRAIN BATCH, not the visible row count:
/// the GUI's off-thread compression worker returns 256 bodies per batch, so a
/// visible-rows-sized pool (~50) would recycle a fifth of them and leave the
/// rest to the allocator — the malloc storm this pool exists to remove would
/// mostly survive. Two batches of headroom absorbs a worker that wakes late.
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
        self.bufs.capacity() * std::mem::size_of::<Vec<Cell>>()
            + self.pooled_cells * std::mem::size_of::<Cell>()
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
        }
    }

    /// Push a pre-built deferred line to the back of the buffer. Test-only:
    /// production stages rows through [`push_row`](Self::push_row) so the cell
    /// body always comes from the pool.
    #[cfg(test)]
    pub(crate) fn push(&mut self, deferred: DeferredLine) {
        self.lines.push_back(deferred);
    }

    /// Snapshot `row` into a POOLED cell body and stage it.
    ///
    /// The allocation-free counterpart of `push(DeferredLine::new(..))` and the
    /// only form the scroll-off path should use: it is what keeps the malloc out
    /// of the reader's `term_lock` hold (see [`CellPool`]).
    #[inline]
    pub(crate) fn push_row(&mut self, row: &Row, extras: ScrolledRowExtras) {
        let scratch = self.pool.take();
        self.lines
            .push_back(DeferredLine::new(row, extras, scratch));
    }

    /// [`push_row`](Self::push_row) for a caller that already owns the extras in
    /// a `Box` — the tiered scroll-off path, which pops them out of
    /// `ring_extras`. See [`DeferredLine::new_boxed`]: the box is moved through
    /// whole instead of being unboxed and re-boxed.
    #[inline]
    pub(crate) fn push_row_boxed(&mut self, row: &Row, extras: Option<Box<ScrolledRowExtras>>) {
        let scratch = self.pool.take();
        self.lines
            .push_back(DeferredLine::new_boxed(row, extras, scratch));
    }

    /// Number of pending deferred lines.
    #[inline]
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.lines.len()
    }

    /// Whether the buffer is empty.
    #[inline]
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.lines.is_empty()
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
        // Disjoint field borrows: the drain owns `lines`, the recycling owns
        // `pool`. Each materialized line hands its cell body back for the next
        // scroll-off to fill.
        let pool = &mut self.pool;
        self.lines
            .drain(..)
            .map(move |deferred| deferred.into_line_recycled(&mut *pool))
    }

    /// Drain up to `n` of the OLDEST pending lines (front of the buffer),
    /// converting each to a materialized [`Line`], in oldest-to-newest order.
    ///
    /// The bounded counterpart of [`drain_all`](Self::drain_all): THRU-5's
    /// off-thread compression worker drains the backlog in bounded batches so
    /// each term-lock hold stays short, instead of the reader thread paying a
    /// whole ~1000-line compression spike inline on its PTY-drain critical path.
    /// `n` is clamped to the buffer length, so front-to-`n` is always valid.
    pub(crate) fn drain_front(&mut self, n: usize) -> impl Iterator<Item = Line> + '_ {
        let n = n.min(self.lines.len());
        // The steady-state consumption site under flood: the worker's bounded
        // batch is exactly where the reader's per-newline bodies come back.
        let pool = &mut self.pool;
        self.lines
            .drain(..n)
            .map(move |deferred| deferred.into_line_recycled(&mut *pool))
    }

    /// Get a line by index within the lazy buffer (0 = oldest).
    ///
    /// Triggers materialization via `OnceCell` on first access.
    #[must_use]
    pub(crate) fn get_line(&self, idx: usize) -> Option<&Line> {
        self.lines.get(idx).map(DeferredLine::to_line)
    }

    /// Clear all pending lines, and release the recycling pool with them.
    ///
    /// Every caller is a history-INVALIDATING path (scrollback erase, store
    /// detach with nothing to drain into, resize drain) — never the flood path —
    /// so the right behaviour is to hand the memory back rather than keep a
    /// cache alive across a "user cleared history" event. The pool refills for
    /// free on the next drain.
    pub(crate) fn clear(&mut self) {
        self.lines.clear();
        self.pool.clear();
    }

    /// Drop the recycled cell bodies without touching the staged lines.
    ///
    /// Called on a WIDTH change: pooled capacities were sized for the old
    /// column count, so reusing them afterwards is either wasted memory (width
    /// shrank) or a guaranteed realloc on first fill (width grew). Note this is
    /// a footprint guard, NOT a correctness one — `DeferredLine::new` clears and
    /// refills the body from the row slice, so a stale-width body could never
    /// leak old content into a line.
    pub(crate) fn clear_pool(&mut self) {
        self.pool.clear();
    }

    /// Test-only: pooled cell-body count.
    #[cfg(test)]
    pub(crate) fn pooled_bodies(&self) -> usize {
        self.pool.len()
    }

    /// Drop the `n` oldest deferred lines (front of the buffer) without
    /// materializing them. Bounds the buffer while the tiered store is detached
    /// for a reflow and cannot absorb it (audit #4).
    pub(crate) fn drop_oldest(&mut self, n: usize) {
        let n = n.min(self.lines.len());
        // Recycle even here: this is the FLOOD path (the compression worker fell
        // behind), i.e. exactly when the reader is allocating a body per newline
        // under its lock. Dropping these bodies to the allocator instead would
        // leave the pool dry precisely when it is needed most.
        let pool = &mut self.pool;
        for mut deferred in self.lines.drain(..n) {
            pool.put(std::mem::take(&mut deferred.cells));
        }
    }

    /// Estimated bytes held by the staged (not yet drained) lines: the
    /// backing `VecDeque` capacity plus each deferred line's heap (Wave-3
    /// adversarial review: under flood backpressure this buffer holds up to
    /// the THRU-5 cap of raw ~8 B/cell rows — real memory the ring byte
    /// watermark previously could not see). O(staged) per call; the buffer
    /// is drain-bounded, and this runs only on poll-time accounting queries.
    pub(crate) fn memory_used(&self) -> usize {
        let mut total = self.lines.capacity() * std::mem::size_of::<DeferredLine>();
        for line in &self.lines {
            total += line.heap_memory_used();
        }
        // Bodies parked in the recycling pool are REAL retained heap (bounded by
        // POOL_MAX_CELLS) — same reason the staged lines are counted: memory the
        // ring byte watermark must be able to see.
        total += self.pool.memory_used();
        total
    }
}

impl Default for LazyBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Preserved CellExtras data for a ring buffer scrollback row.
///
/// When rows scroll from the visible grid into ring buffer scrollback,
/// their CellExtras are extracted before `shift_rows_up_by` discards them.
/// This extends the #4149 hyperlink-only pattern to also capture complex
/// chars, combining marks, and RGB colors (#4215).
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
}

impl ScrolledRowExtras {
    /// True when all fields are empty (common case: plain ASCII text).
    ///
    /// Used to avoid allocating a boxed extras struct for rows that have
    /// no overflow data, saving 120 bytes per plain-text ring buffer row.
    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.hyperlinks.is_empty()
            && self.complex_chars.is_empty()
            && self.combining.is_empty()
            && self.rgb_fg.is_empty()
            && self.rgb_bg.is_empty()
            && self.underline_colors.is_empty()
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
    }
}

/// Monotone read cursors over the four column-sorted `ScrolledRowExtras`
/// vectors. They are CURSORS, not lookups: each advances only when its next
/// entry's column matches the column being emitted, so an entry for a skipped
/// (spacer) column, or an out-of-order entry, is simply never consumed. The
/// ring fast materializer shares this state and the same three consumers below,
/// so it inherits that behaviour instead of re-deriving it with random access
/// — which would differ exactly where the data is unusual.
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
    /// Used for ring buffer scrollback rows whose extras were preserved
    /// in `ring_extras` at scroll time (#4149, #4215).
    ///
    /// Uses extracted complex chars, combining marks, and resolved colors
    /// instead of placeholder values. RGB overflow colors and StyleId colors
    /// are both pre-resolved into the extras vectors at extraction time.
    pub(crate) fn row_to_line_with_stored_extras(row: &Row, extras: &ScrolledRowExtras) -> Line {
        Self::row_to_line_with_stored_extras_at_len(row, extras, row.len())
    }

    /// [`Self::row_to_line_with_stored_extras`] with an explicit cell count —
    /// the width-reflow seams pass the FULL row width for a row whose logical
    /// line CONTINUES on the next physical row (fixwave5): autowrap filled
    /// such a row to its last column, so its trailing blank cells are real
    /// content, and the default `row.len()` trim would erode one mid-line
    /// space per chunk boundary on every width sweep.
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
            return line;
        }

        // Fast path: no extras (common case for plain text).
        // Skips per-cell extras lookup, overflow resolution, combining marks,
        // and hyperlink cloning. ~40% faster for 80-col ASCII rows.
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

        for (idx, cell) in cells.iter().enumerate() {
            #[cfg(any(test, feature = "testing"))]
            super::count_row_to_line_cell();

            if is_spacer(cells, idx) {
                continue;
            }

            // No complex chars possible — char_data is always a BMP codepoint.
            text.push(cell.char());

            // No overflow or style_id — read inline colors directly.
            let fg_raw = cell.fg_color().map_or(PackedColor::DEFAULT_FG.0, |c| c.0);
            let bg_raw = cell.bg_color().map_or(PackedColor::DEFAULT_BG.0, |c| c.0);
            attrs_rle.push(CellAttrs::from_raw(fg_raw, bg_raw, cell.flags().bits()));
        }

        let mut line = Line::with_hyperlinks_owned(text, attrs_rle.finish(), Vec::new());
        if row.is_wrapped() {
            line.set_wrapped(true);
        }
        line
    }

    /// Extract all CellExtras data from a row before shift_rows_up_by discards them.
    ///
    /// Captures hyperlinks (#4149), complex chars, combining marks, RGB
    /// colors (#4215), and StyleId-resolved colors (#5890) so they survive
    /// the transition into ring buffer scrollback.
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
        // Headless scrollback-text-only mode (opt-in, default off): skip
        // per-cell colour/style extraction on scroll. ~faster on colour-heavy
        // floods. Only for embeddings that read scrollback as text (e.g. Orca's
        // text-only serialize_ansi). OSC-8 hyperlink spans are STILL collected
        // (a cheap hyperlink-only pass) so links that scroll off-screen remain
        // recoverable from scrollback. The visible grid is untouched, so
        // visible_sha and the differential oracle are unaffected.
        if scrollback_text_only() {
            Self::extract_hyperlinks_only_into(result, row, extras, row_idx);
            return;
        }
        let len = row.len() as usize;
        if len == 0 {
            return;
        }

        // Quick check: skip iteration when no overflow data exists.
        // StyleId cells need resolution even without CellExtras, so check both.
        // The per-row HAS_STYLE_ID flag avoids scanning cells on plain-text
        // rows even when other rows in the grid use style interning (#7872).
        // Previously this used a grid-level sticky flag that forced every
        // scrolled row to scan, even if only a prompt row ever had styles.
        //
        // Use has_any_data() instead of is_empty() to account for ring-buffer-only
        // entries (complex chars, RGB colors) that bypass the HashMap on the write
        // hot path. is_empty() only checks the HashMap and would silently drop
        // ring-buffer data on scroll.
        //
        // NOTE for the next reader: `has_style_id()` is a TEST/KANI-only signal
        // in practice. `RowFlags::HAS_STYLE_ID` is only ever ORIGINATED by
        // `row/style_id_write.rs`, which is
        // `#[cfg(any(test, kani, feature = "testing"))]` (`Row::mark_has_style_id`
        // has no production caller; `Row::set` merely propagates the bit). So in
        // the shipped binary this short-circuit rests entirely on
        // `has_any_data()` — do not build an optimization on the assumption that
        // a styled production row reports `has_style_id()`.
        if !extras.has_any_data() && !row.has_style_id() {
            return;
        }

        // ROW-LOCAL GATE for the map-empty case.
        //
        // The grid-global gate above is STICKY: `has_any_data()` is
        // `!data.is_empty() || complex_ring.is_some() || rgb_ring.is_some()`, and
        // the two dense rings are allocated on the first truecolor / non-BMP
        // write ANYWHERE in the grid and are never freed. One coloured shell
        // prompt therefore signs every plain row that scrolls off for the rest of
        // the session up to the two O(cols) passes below — the reserve count and
        // the per-cell walk with its `is_spacer` neighbour peek and its
        // `extras.get(coord)` probe — inside the PTY reader's `term_lock` hold.
        //
        // When the MAP is empty, that whole pass can produce nothing unless a
        // cell says so: every remaining source is announced by the cell's own
        // bits — USES_STYLE_ID (style table), COMPLEX (char ring), fg/bg RGB
        // overflow modes (colour ring) — and those bits are written by the very
        // operation that fills the ring, so the fold is EXACT. No new flag to
        // keep in sync, and no dependency on the `HAS_EXTRAS` ⇔ entry invariant
        // (which this branch does not need: an empty map answers `None` for
        // every column regardless).
        //
        // If the map is NON-empty nothing changes — an entry can sit on any row
        // and only the walk can find it, so that case runs exactly as before.
        if extras.is_empty() && !row_has_extractable_cells(&row.as_slice()[..len]) {
            return;
        }

        // Pre-size the rgb vectors: RGB-overflow rows and style-id rows both
        // otherwise pay repeated growth reallocs (4 → 8 → …) per Vec on the
        // scroll hot path. Counting is one cheap pass over the L1-resident
        // cells; skipped when the (recycled) vectors already have capacity.
        //
        // The gate deliberately does NOT test `row.has_style_id()` (see the note
        // above: that flag is test/kani-only). Gating on it made this whole block
        // dead in the shipped binary and left every truecolor row re-growing
        // `rgb_fg`/`rgb_bg` from capacity 0 — plain truecolor writes go through
        // `set_rgb_ring_range`, which sets no row flag.
        //
        // fg and bg overflow independently (`set_rgb_ring_range` takes them as
        // separate `Option`s), so count them separately — reserving both from one
        // combined count would leave wasted capacity riding into the ring for the
        // line's lifetime, invisible to `DeferredLine::heap_memory_used` (which
        // accounts extras shallow). The counts are an upper bound (`fg_rgb_for`
        // can return `None` if the ring evicted the entry); over-reserving by a
        // few is harmless, and it never under-reserves.
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
            // the cell scrolls off and loses access to the table (#5890).
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

            if let Some(extra) = extras.get(coord) {
                // Combining marks (#4215)
                if !extra.combining().is_empty() {
                    result
                        .combining
                        .push((col_u16, SmallVec::from_slice(extra.combining())));
                }

                // SGR 58 underline colour: capture the PACKED form (indexed or
                // RGB) so it survives into scrollback and re-resolves against the
                // live palette on restore, exactly like the live cell (#7445).
                if let Some(packed) = packed_underline_color(extra) {
                    result.underline_colors.push((col_u16, packed));
                }

                // Hyperlink span coalescing (#4149, #4390)
                // Use col_u16 (physical column) to match restore_hyperlinks.
                // Extract both URL and ID from the OSC 8 sequence.
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
    /// Test-only: production code uses `row_to_line_with_stored_extras` which
    /// takes pre-extracted extras from `ring_extras` (#4149, #4215).
    ///
    /// This function extracts extras and builds the line in one step.
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
        // and Row::fmt in row/fmt.rs. Without this, search finds different
        // content for the same row depending on whether it's visible or in
        // scrollback (#7471).
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
/// inline when the cell carries it, else the column's PRE-RESOLVED stored RGB
/// (extraction already resolved RGB overflow AND `StyleId` at scroll time,
/// #5890), else the default. Shared with the ring fast materializer.
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

/// Accumulates a materializing line's attribute runs, allocating NOTHING until
/// a second distinct `CellAttrs` actually appears.
///
/// WHY: `Line::from_parts` throws the whole `Rle` away when it holds zero runs
/// or one DEFAULT run — which is the shape of every plain-text line, the
/// commonest thing a terminal ever scrolls. The old code still built that `Rle`
/// in full: the first `push` allocated its `runs` vector, every later cell went
/// through `push` -> `extend_with` -> `remaining_capacity` -> `runs.last_mut()`,
/// and the finished object was dropped unread. Holding the open run in two
/// locals instead makes the plain line allocation-free and turns the per-cell
/// step into one comparison.
///
/// EQUIVALENCE. The emitted `Rle` is run-for-run identical to the old one in
/// every case `from_parts` KEEPS, and the only case it differs is the one
/// `from_parts` discards:
///   * no cells             -> 0 runs (was: 0 runs)          -> `attrs = None`
///   * one DEFAULT run      -> 0 runs (was: 1 default run)   -> `attrs = None`
///   * one non-default run  -> 1 run  (was: 1 run, same)     -> stored
///   * two or more runs     -> same runs, same order         -> stored
///
/// A spill only happens on a value CHANGE, so a spilled builder always emits at
/// least two runs and can never collapse back into the discarded shape.
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

    /// PER-CELL HOT PATH — keep it to one comparison and one add.
    ///
    /// The `len == 0` case needs no test of its own: a fresh builder's open run
    /// is `(DEFAULT, 0)`, so a default first cell merges (giving `(DEFAULT, n)`,
    /// which is right) and a styled first cell falls into `open_new_run`, which
    /// flushes nothing because `len` is 0.
    ///
    /// `open_new_run` is deliberately a SEPARATE, non-inlined function: folding
    /// its `Rle::extend_with` call into this one made the whole thing too big to
    /// inline into the per-cell materialization loop, and the measured result
    /// was a 6-11% REGRESSION on the very workloads this change exists to speed
    /// up. The split is what makes the fast path a straight-line compare.
    #[inline]
    fn extend(&mut self, attrs: CellAttrs, count: u32) {
        if count == 0 {
            return;
        }
        if self.value == attrs {
            // A row is at most `u16::MAX` cells wide and each cell contributes a
            // bounded number of characters, so this sum cannot reach `u32::MAX`;
            // the saturating form just discharges the overflow obligation, and
            // `Rle::extend_with` clamps to the remaining capacity at flush
            // exactly as the old per-cell pushes did.
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
    // `char_count` copies of ONE value is one run by construction, so hand the
    // builder the count instead of looping a per-character push. Identical
    // result: a loop of single pushes clamps each to `min(1, remaining)` for a
    // total of `min(n, remaining)`, which is what one counted extend does.
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

/// Pack a cell's SGR 58 underline colour into the `0xTT_XXXXXX` form used by
/// [`CellExtra::set_underline_color_u32`](crate::CellExtra::set_underline_color_u32),
/// preserving the RGB (`0x01`) vs indexed (`0x02`) distinction so a restored
/// indexed colour re-resolves against the live palette. Indexed and explicit RGB
/// are mutually exclusive on a `CellExtra`; the index is checked first because it
/// carries the palette-resolution semantics.
fn packed_underline_color(extra: &crate::CellExtra) -> Option<u32> {
    if let Some(index) = extra.underline_color_index() {
        return Some(0x02_00_00_00 | u32::from(index));
    }
    let [r, g, b] = extra.underline_color()?;
    Some(0x01_00_00_00 | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b))
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
mod extract_gate_tests {
    use super::*;
    use crate::{CellFlags, PackedColor};

    /// The map-empty gate must never produce a FALSE NEGATIVE: a row whose only
    /// datum is an RGB colour in the dense ring still has to extract it.
    ///
    /// This is the shape `set_range_uniform`'s RGB-only fast path produces — the
    /// value in the ring, the mode bit on the cell, nothing in the map — i.e.
    /// exactly the state the new gate reasons about.
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

    /// An armed ring somewhere else in the grid must not change WHAT a plain row
    /// extracts — only what it costs. Before the gate, that armed ring put this
    /// row through the full per-cell pass; after it, the row is decided from its
    /// own cells. The extracted result is empty either way, and that identity is
    /// the whole behaviour claim.
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
