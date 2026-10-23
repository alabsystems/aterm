// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The key a full-history search is answered under, and the plan one search follows.

use std::ops::Range;
use std::sync::Arc;

use aterm_types::text_shaping::AmbiguousWidth;

use super::search_index::{SearchDiscard, build_search_index};
use super::{NarrowStack, SearchRows, Terminal, WrapFrame, query_search_index_narrowing};
use crate::search::{
    SearchDirection, SearchMatch, SearchOptionsError, SearchResults, TerminalSearch,
    max_cached_for_retained,
};

/// One terminal's search generation and coordinate frame, read in one lock hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchKey {
    /// Whether the alternate screen was active.
    pub alt: bool,
    /// [`Terminal::content_seq`] content mutation counter.
    pub content_seq: u64,
    /// `Grid::history_renumber_epoch()` retained-row renumbering counter.
    pub renumber_epoch: u64,
    /// [`Terminal::absolute_row_revision`] piecewise row-renumbering counter.
    pub row_revision: u64,
    /// Active [`AmbiguousWidth`] column measurement policy.
    pub ambiguous: AmbiguousWidth,
    /// Grid width in columns.
    pub cols: u16,
    /// `Grid::oldest_absolute_row()`.
    pub oldest: u64,
    /// `Grid::scrollback_lines()`.
    pub scrollback: usize,
    /// Visible rows.
    pub rows: u16,
    /// Maximum lines the index retains (floored at `max_cached_for_retained(rows)`).
    pub max_lines: usize,
}

/// The absolute rows a search over a [`SearchKey`] reads, in the index's line numbering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchWindow {
    /// Absolute row of the top visible row.
    pub visible_base: usize,
    /// Oldest absolute row the index retains.
    pub retained_base: usize,
    /// Rows the index retains, `min(scrollback + rows, max_lines)`.
    pub retained_total: usize,
    /// Retained history older than `retained_base` that the depth cap leaves unread.
    pub skipped_prefix: usize,
    /// Exclusive end of the indexed rows, `retained_base + retained_total`.
    pub indexed_end: usize,
}

impl SearchWindow {
    /// Newest line count when the depth cap skips older history, or `None` when reading all.
    #[must_use]
    #[inline]
    pub const fn searched_lines(&self) -> Option<usize> {
        if self.skipped_prefix > 0 {
            Some(self.retained_total)
        } else {
            None
        }
    }
}

impl SearchKey {
    /// Captures the key of `t` for an index retaining `depth_cap` lines.
    #[must_use]
    #[inline]
    pub fn capture(t: &Terminal, depth_cap: usize) -> Self {
        let grid = t.grid();
        let rows = t.rows();
        Self {
            alt: t.is_alternate_screen(),
            content_seq: t.content_seq(),
            renumber_epoch: grid.history_renumber_epoch(),
            row_revision: t.absolute_row_revision(),
            ambiguous: t.ambiguous_width(),
            cols: grid.cols(),
            oldest: grid.oldest_absolute_row(),
            scrollback: grid.scrollback_lines(),
            rows,
            max_lines: depth_cap.max(max_cached_for_retained(usize::from(rows))),
        }
    }

    /// Whether rows and columns read under `self` keep the same coordinate frame under `now`.
    #[must_use]
    #[inline]
    pub fn same_frame(&self, now: &Self) -> bool {
        self.alt == now.alt
            && self.renumber_epoch == now.renumber_epoch
            && self.row_revision == now.row_revision
            && self.cols == now.cols
            && self.ambiguous == now.ambiguous
    }

    /// Whether an index built under `self` matches the frame, content, and depth of `now`.
    #[must_use]
    #[inline]
    pub fn same_generation(&self, now: &Self) -> bool {
        self.same_frame(now)
            && self.content_seq == now.content_seq
            && self.max_lines == now.max_lines
    }

    /// Whether an index built under `prev` can be incrementally refreshed to `self`.
    #[must_use]
    #[inline]
    pub fn refreshable_from(&self, prev: &Self) -> bool {
        let (was, is) = (prev.window(), self.window());
        self.same_frame(prev)
            && self.max_lines == prev.max_lines
            && was.retained_base <= is.retained_base
            && was.indexed_end <= is.indexed_end
    }

    /// The newest `max_lines` rows of retained history and screen read under this key.
    #[must_use]
    #[inline]
    pub fn window(&self) -> SearchWindow {
        let total = self.scrollback.saturating_add(usize::from(self.rows));
        let retained_total = total.min(self.max_lines);
        let skipped_prefix = total - retained_total;
        let row = |offset: usize| {
            let abs = self
                .oldest
                .saturating_add(u64::try_from(offset).unwrap_or(u64::MAX));
            usize::try_from(abs).unwrap_or(usize::MAX)
        };
        let retained_base = row(skipped_prefix);
        SearchWindow {
            visible_base: row(self.scrollback),
            retained_base,
            retained_total,
            skipped_prefix,
            indexed_end: retained_base.saturating_add(retained_total),
        }
    }
}

/// How an index lays out a soft-wrapped run of grid rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchLayout {
    /// One indexed line per grid row.
    Physical,
    /// Each soft-wrap run joined into a single logical line keyed at its starting row.
    Logical,
}

/// What one full-history search asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchRequest {
    /// Lines the index retains, floored as [`SearchKey::max_lines`] says.
    pub depth_cap: usize,
    /// How soft-wrapped rows are indexed.
    pub layout: SearchLayout,
    /// Optional physical find anchor `(row, col)`.
    pub anchor: Option<(usize, usize)>,
}

/// One search, from the hold that captured its key to the hold that finishes it.
pub struct SearchPlan {
    pub(super) request: SearchRequest,
    pub(super) key: SearchKey,
    pub(super) wrap: WrapFrame,
    pub(super) kind: PlanKind,
}

/// Whether a plan is served from the slot or builds.
pub(super) enum PlanKind {
    /// The slot's index is of the key's generation: query it, and lend the narrowing
    /// frames read off it.
    Hit {
        index: Arc<TerminalSearch>,
        narrow: Option<NarrowStack>,
    },
    /// Rows to copy and index.
    Build(Box<RowCopy>),
}

/// The rows a build copies, and what the holds that copied them saw.
pub(super) struct RowCopy {
    /// The slot's index of an earlier generation, brought up to the key by indexing
    /// `spans` over it ([`Terminal::try_refresh_search_index`]); `None` builds the window
    /// from nothing.
    pub(super) base: Option<Arc<TerminalSearch>>,
    /// The key `base` was built under, until the plan's first chunk names `spans` from it
    /// ([`Terminal::read_search_chunk`]); `spans` is empty while this stands.
    pub(super) pending: Option<SearchKey>,
    /// Absolute rows to copy, ascending and disjoint, each starting a logical line.
    pub(super) spans: Vec<Range<usize>>,
    /// The rows copied so far, the spans' rows in order.
    pub(super) rows: SearchRows,
    /// Some hold saw another generation than the key's: the copy may mix two contents.
    pub(super) torn: bool,
    /// Some hold saw another frame than the key's ([`SearchKey::same_frame`]).
    pub(super) frame_moved: bool,
    /// What the plan's hold took out of the slot and the build has no use for (an index of
    /// another frame or layout, a refreshed index's narrowing frames), freed by
    /// [`SearchPlan::build`].
    pub(super) discard: SearchDiscard,
}

impl RowCopy {
    /// A build of `window` from nothing: every row of it.
    #[inline]
    pub(super) fn window(window: &SearchWindow, discard: SearchDiscard) -> Self {
        let mut copy = Self::unnamed(None, None, discard);
        copy.name_spans(std::iter::once(window.retained_base..window.indexed_end).collect());
        copy
    }

    /// A refresh of `base`, built under `previous`; the first chunk names its rows.
    #[inline]
    pub(super) fn refresh(
        base: Arc<TerminalSearch>,
        previous: SearchKey,
        discard: SearchDiscard,
    ) -> Self {
        Self::unnamed(Some(base), Some(previous), discard)
    }

    /// A copy with no rows named yet.
    #[inline]
    fn unnamed(
        base: Option<Arc<TerminalSearch>>,
        pending: Option<SearchKey>,
        discard: SearchDiscard,
    ) -> Self {
        Self {
            base,
            pending,
            spans: Vec::new(),
            rows: SearchRows::default(),
            torn: false,
            frame_moved: false,
            discard,
        }
    }

    /// Set the rows to copy, before any is copied.
    #[inline]
    pub(super) fn name_spans(&mut self, spans: Vec<Range<usize>>) {
        self.rows = SearchRows::with_capacity(spans.iter().map(ExactSizeIterator::len).sum());
        self.spans = spans;
    }

    /// Rows the spans hold that are not copied yet.
    #[inline]
    fn rows_to_read(&self) -> usize {
        self.spans
            .iter()
            .map(ExactSizeIterator::len)
            .sum::<usize>()
            .saturating_sub(self.rows.text.len())
    }
}

impl SearchPlan {
    /// The key captured when the plan was made.
    #[must_use]
    #[inline]
    pub const fn key(&self) -> &SearchKey {
        &self.key
    }

    /// The soft-wrap frame the plan's hits are re-expressed through.
    #[must_use]
    #[inline]
    pub const fn wrap(&self) -> WrapFrame {
        self.wrap
    }

    /// The terminal's slot holds the index of the key's generation: nothing to copy.
    #[must_use]
    #[inline]
    pub const fn is_hit(&self) -> bool {
        matches!(self.kind, PlanKind::Hit { .. })
    }

    /// Rows remaining to copy before [`SearchPlan::build`].
    #[must_use]
    #[inline]
    pub fn rows_to_read(&self) -> usize {
        match &self.kind {
            PlanKind::Hit { .. } => 0,
            PlanKind::Build(copy) if copy.pending.is_some() => self.key.window().retained_total,
            PlanKind::Build(copy) => copy.rows_to_read(),
        }
    }

    /// Builds or reuses the search index off the terminal lock.
    #[must_use]
    pub fn build(self) -> BuiltSearch {
        let unread = self.rows_to_read();
        let Self {
            request,
            key,
            wrap,
            kind,
        } = self;
        let (index, narrow, made, torn, frame_moved) = match kind {
            PlanKind::Hit { index, narrow } => (index, narrow, Made::Served, false, false),
            PlanKind::Build(mut copy) => {
                drop(std::mem::take(&mut copy.discard));
                let made = if copy.base.is_some() {
                    Made::Refreshed
                } else {
                    Made::Rebuilt
                };
                let torn = copy.torn || unread > 0;
                let frame_moved = copy.frame_moved;
                let index = build_search_index(&key, request.layout, *copy);
                (Arc::new(index), None, made, torn, frame_moved)
            }
        };
        BuiltSearch {
            request,
            key,
            wrap,
            index,
            narrow,
            made,
            torn,
            frame_moved,
        }
    }
}

/// How a [`BuiltSearch`]'s index came to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Made {
    /// The slot's index, shared.
    Served,
    /// An earlier generation's index brought up to the key.
    Refreshed,
    /// Built over the window from nothing.
    Rebuilt,
}

/// A plan's index, ready to query; [`Terminal::finish_search`] ends the search.
pub struct BuiltSearch {
    pub(super) request: SearchRequest,
    pub(super) key: SearchKey,
    pub(super) wrap: WrapFrame,
    pub(super) index: Arc<TerminalSearch>,
    /// The narrowing frames over `index`: lent by the slot on a hit, grown by `query`.
    pub(super) narrow: Option<NarrowStack>,
    pub(super) made: Made,
    pub(super) torn: bool,
    pub(super) frame_moved: bool,
}

impl BuiltSearch {
    /// The key the index was planned under.
    #[must_use]
    #[inline]
    pub const fn key(&self) -> &SearchKey {
        &self.key
    }

    /// The soft-wrap frame hits are re-expressed through.
    #[must_use]
    #[inline]
    pub const fn wrap(&self) -> WrapFrame {
        self.wrap
    }

    /// The index, for a walk the query layer does not make (a lazy point search).
    #[must_use]
    #[inline]
    pub fn index(&self) -> &TerminalSearch {
        &self.index
    }

    /// Runs one query in physical coordinates using incremental narrowing frames.
    #[inline]
    pub fn query(
        &mut self,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
        direction: SearchDirection,
        strict: bool,
    ) -> Result<(SearchResults, Option<SearchMatch>), SearchOptionsError> {
        query_search_index_narrowing(
            &self.index,
            &mut self.narrow,
            query,
            case_sensitive,
            is_regex,
            direction,
            self.request.anchor,
            strict,
            self.wrap,
        )
    }
}

/// How a finished search stands against the terminal ([`Terminal::finish_search`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchVerdict {
    /// Whether the index matches the terminal's generation from start to finish.
    pub consistent: bool,
    /// Whether every reported row kept the same coordinate frame throughout the search.
    pub frame_stable: bool,
}

#[cfg(test)]
mod tests {
    use aterm_types::text_shaping::AmbiguousWidth;

    use super::{SearchKey, SearchWindow};
    use crate::search::max_cached_for_retained;
    use crate::terminal::Terminal;

    fn key() -> SearchKey {
        let mut t = Terminal::new(4, 40);
        for i in 0..30 {
            t.process(format!("line {i}\r\n").as_bytes());
        }
        SearchKey::capture(&t, 1_000)
    }

    /// `k` with the fields `f` changes.
    fn moved(mut k: SearchKey, f: impl FnOnce(&mut SearchKey)) -> SearchKey {
        f(&mut k);
        k
    }

    /// Each verdict compares exactly the fields it names: every field moved alone flips
    /// the verdicts that compare it and leaves the others standing.
    #[test]
    fn each_verdict_compares_every_field_it_names() {
        let k = key();
        assert!(k.same_frame(&k) && k.same_generation(&k) && k.refreshable_from(&k));
        assert_eq!(k.ambiguous, AmbiguousWidth::Single, "precondition");
        for (field, frame) in [
            ("alt", moved(k, |k| k.alt = !k.alt)),
            ("renumber_epoch", moved(k, |k| k.renumber_epoch += 1)),
            ("row_revision", moved(k, |k| k.row_revision += 1)),
            ("cols", moved(k, |k| k.cols -= 1)),
            (
                "ambiguous",
                moved(k, |k| k.ambiguous = AmbiguousWidth::Double),
            ),
        ] {
            assert!(!k.same_frame(&frame), "{field}");
            assert!(!k.same_generation(&frame), "{field}");
            assert!(!frame.refreshable_from(&k), "{field}");
        }
        let output = moved(k, |k| k.content_seq += 1);
        assert!(k.same_frame(&output));
        assert!(!k.same_generation(&output));
        assert!(output.refreshable_from(&k));
        let deeper = moved(k, |k| k.max_lines += 1);
        assert!(k.same_frame(&deeper));
        assert!(!k.same_generation(&deeper));
        assert!(!deeper.refreshable_from(&k));
        let advanced = moved(k, |k| {
            k.oldest += 3;
            k.content_seq += 1;
        });
        assert!(advanced.refreshable_from(&k));
        assert!(
            !k.refreshable_from(&advanced),
            "a retreat of the retained base"
        );
        let shorter = moved(k, |k| k.scrollback -= 1);
        assert!(
            !shorter.refreshable_from(&k),
            "a retreat of the indexed end"
        );
    }

    /// A Kitty `CSI +T` unscroll slides every retained row's key while `content_seq`
    /// stands: only the epoch tells the two keys apart. NEGATIVE CONTROL: every other
    /// field a generation compares is equal across it.
    #[test]
    fn an_unscroll_is_another_frame_at_the_same_content_seq() {
        let sb = aterm_scrollback::Scrollback::new(8, 64, 8_000_000);
        let mut t = Terminal::with_scrollback(6, 40, 8, sb);
        for i in 0..40 {
            t.process(format!("line {i}\r\n").as_bytes());
        }
        let before = SearchKey::capture(&t, 1_000);
        t.process(b"\x1b[2+T");
        let after = SearchKey::capture(&t, 1_000);
        assert_eq!(
            moved(after, |k| k.renumber_epoch = before.renumber_epoch),
            moved(before, |k| (k.oldest, k.scrollback) =
                (after.oldest, after.scrollback)),
            "the unscroll moves no generation field but the epoch"
        );
        assert_ne!(before.renumber_epoch, after.renumber_epoch);
        assert!(!before.same_frame(&after));
        assert!(!before.same_generation(&after));
    }

    /// `capture` reads the terminal's own accessors in one hold and floors the depth cap
    /// at the visible screen; `window` is the newest `max_lines` rows of the retained
    /// history and screen.
    #[test]
    fn capture_floors_the_depth_and_the_window_is_the_newest_rows() {
        let mut t = Terminal::new(4, 40);
        for i in 0..30 {
            t.process(format!("line {i}\r\n").as_bytes());
        }
        let whole = SearchKey::capture(&t, 1_000);
        assert_eq!(whole.cols, t.grid().cols());
        assert_eq!(whole.oldest, t.grid().oldest_absolute_row());
        assert_eq!(whole.scrollback, t.grid().scrollback_lines());
        assert_eq!(whole.ambiguous, t.ambiguous_width());
        let base = usize::try_from(whole.oldest).unwrap();
        let total = whole.scrollback + 4;
        assert_eq!(
            whole.window(),
            SearchWindow {
                visible_base: base + whole.scrollback,
                retained_base: base,
                retained_total: total,
                skipped_prefix: 0,
                indexed_end: base + total,
            }
        );
        assert_eq!(whole.window().searched_lines(), None);

        let floor = max_cached_for_retained(4);
        let tiny = SearchKey::capture(&t, 1);
        assert_eq!(
            tiny.max_lines, floor,
            "the screen stays searchable under any cap"
        );
        assert!(total > floor, "precondition: history past the floor");
        let window = tiny.window();
        assert_eq!(window.retained_total, floor);
        assert_eq!(window.skipped_prefix, total - floor);
        assert_eq!(window.retained_base, base + total - floor);
        assert_eq!(window.indexed_end, base + total);
        assert_eq!(window.searched_lines(), Some(floor));
    }
}
