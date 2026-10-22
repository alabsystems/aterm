// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The terminal's full-history search index: one slot per terminal, and the protocol every
//! host builds and queries it through.
//!
//! The index over the retained scrollback plus visible rows depends only on which grid is
//! active, its content and its coordinate frame, so one index serves every query whose
//! [`SearchKey`] is of the generation it was built under. The slot keeps it with that key,
//! the layout it indexes soft wraps in ([`SearchLayout`]), and the narrowing frames a find
//! bar read off it ([`NarrowStack`]), which die with it. The slot lives in the `Terminal`,
//! so the index dies with the terminal, and checkpoint restore and history carry release it.
//!
//! A search runs in three steps, so the O(window) work never runs under the lock a GUI host
//! keeps its terminal behind (shared with the PTY reader and the renderer):
//!
//! 1. [`Terminal::search_plan`], one hold: capture the key, carry the anchor into its
//!    soft-wrap frame, and either answer from the slot or plan a build (the window from
//!    nothing, or a refresh of the slot's index).
//! 2. [`Terminal::read_search_chunk`], one bounded hold per call, copies the build's rows and
//!    notes whether a hold saw another generation or frame than the key's; a refresh's first
//!    chunk names its rows first, the ones that may have changed. [`SearchPlan::build`]
//!    indexes them with no lock held.
//! 3. [`Terminal::finish_search`], one hold: the endpoint check, and the slot's upkeep
//!    (keep a whole build, take the narrowing frames back).
//!
//! No hold frees an index: that is O(window) too (every indexed line, its cells and the
//! trigram posting lists), and so is freeing the narrowing frames read off one. What a plan's
//! hold takes out of the slot rides in the plan and is freed by [`SearchPlan::build`]; what
//! [`Terminal::finish_search`] and [`Terminal::take_search_index`] let go of comes back as a
//! [`SearchDiscard`], which the host drops once its lock is released.
//!
//! Finding the row a soft-wrapped line starts on walks its run one row at a time, a
//! per-row history read each. The plan's hold walks at most one run (the anchor's); the
//! refresh's walks over the runs at its two boundaries run in the first chunk's hold.
//!
//! [`Terminal::indexed_search`] runs the three in one `&mut` borrow, for a host that owns
//! its terminal (the web host, `search_summary`).
//!
//! The window is the newest `max_lines` rows ([`SearchKey::window`]), indexed into an index
//! that retains exactly that many, so the index itself never evicts; rows the depth cap
//! leaves unread mark the results incomplete from the window's first row. A refresh costs
//! O(churn) rather than O(window):
//!
//! - Append: rows at or above the previous `indexed_end` are new; feed them.
//! - Replace: rows at or above the previous visible base may have been edited in place;
//!   re-feed them (unchanged rows are cheap).
//! - EvictBelow: `drop_history_below` removes the rows that left the window, with
//!   fresh-build semantics.
//! - Reflow, alt-screen switch, protected-footer splice (`absolute_row_revision`), Kitty
//!   unscroll (`history_renumber_epoch`), a new depth or layout: full rebuild.
//!
//! The refresh runs only when it provably equals the full build over the current window
//! ([`SearchKey::refreshable_from`] and [`Terminal::try_refresh_search_index`]); the tests
//! compare it with `legacy_results`, a from-scratch oracle.

use std::ops::Range;
use std::sync::Arc;

use super::search_budgeted::BudgetedSearchState;
use super::search_plan::{BuiltSearch, Made, PlanKind, RowCopy};
use super::{
    NarrowStack, SearchKey, SearchLayout, SearchPlan, SearchRequest, SearchRows, SearchVerdict,
    Terminal, WrapFrame, join_wrapped_rows,
};
use crate::search::{DEFAULT_MAX_CACHED_LINES, TerminalSearch};

/// What [`Terminal::indexed_search`] asks for: the default depth, one line per row. The
/// budgeted search verifies the same window ([`Terminal::search_budgeted`]).
pub(crate) const ONE_SHOT_SEARCH: SearchRequest = SearchRequest {
    depth_cap: DEFAULT_MAX_CACHED_LINES,
    layout: SearchLayout::Physical,
    anchor: None,
};

/// A terminal's search index and what it was built for.
pub(crate) struct SearchSlot {
    /// The key the index was built under: the hit test ([`SearchKey::same_generation`]),
    /// the refresh guard ([`SearchKey::refreshable_from`]), and through its window the
    /// visible base a refresh re-reads from.
    key: SearchKey,
    /// How the index lays out soft wraps; a request for the other layout misses.
    layout: SearchLayout,
    /// Shared with the queries reading it off the lock; never changed while shared.
    index: Arc<TerminalSearch>,
    /// The narrowing frames read off `index`, lent to one hit at a time.
    narrow: Option<NarrowStack>,
}

/// Search state a hold took out of the terminal and left its caller to free
/// ([`Terminal::finish_search`], [`Terminal::take_search_index`]); a building plan carries
/// its own ([`SearchPlan::build`]). Freeing an index is O(window), about 100k allocations at
/// the default depth, so no hold does it: a host that keeps its terminal behind a lock drops
/// this after releasing the lock.
#[must_use = "dropping it frees what it holds: drop it once the terminal's lock is released"]
#[derive(Default)]
pub struct SearchDiscard {
    /// A slot the terminal no longer holds: displaced by a plan or a kept build, or taken.
    slot: Option<SearchSlot>,
    /// A search's index the slot does not hold: a build it did not keep, or a hit's share of
    /// an index (the last one when a refresh displaced that index meanwhile).
    index: Option<Arc<TerminalSearch>>,
    /// Narrowing frames that lost their place: a refreshed index's, a build's the slot did
    /// not keep, or a hit's that the slot no longer wants.
    narrow: Option<NarrowStack>,
    /// A budgeted search taken with the slot.
    #[expect(dead_code, reason = "held only to be freed when the discard drops")]
    budgeted: Option<BudgetedSearchState>,
}

impl Terminal {
    /// Plan one search, in one hold: capture its key ([`SearchKey::capture`]), carry the
    /// request's anchor into the key's soft-wrap frame, and decide how the index is had.
    ///
    /// - The slot's index is of the key's generation and layout: a hit, which shares the
    ///   index and borrows its narrowing frames.
    /// - It is of an earlier generation the key may be refreshed from: the plan takes it
    ///   out of the slot ([`Self::try_refresh_search_index`]), and its first chunk names
    ///   the rows that may differ ([`Self::read_search_chunk`]).
    /// - Otherwise the slot is emptied now, so the terminal never holds two indexes of its
    ///   window, and the plan names the whole window.
    ///
    /// The only soft-wrap run this hold walks is the anchor's, and it frees nothing: what it
    /// takes out of the slot and does not build on (an index the plan does not refresh, a
    /// refreshed index's narrowing frames) rides in the plan to [`SearchPlan::build`].
    #[must_use]
    pub fn search_plan(&mut self, request: &SearchRequest) -> SearchPlan {
        let key = SearchKey::capture(self, request.depth_cap);
        let wrap = match request.layout {
            SearchLayout::Physical => WrapFrame::unjoined(&key),
            SearchLayout::Logical => WrapFrame::under_key(self, &key, request.anchor),
        };
        let hit = self
            .search_slot
            .as_mut()
            .filter(|slot| slot.layout == request.layout && slot.key.same_generation(&key))
            .map(|slot| PlanKind::Hit {
                index: Arc::clone(&slot.index),
                narrow: slot.narrow.take(),
            });
        let kind = hit.unwrap_or_else(|| {
            PlanKind::Build(Box::new(
                match self.try_refresh_search_index(&key, request.layout) {
                    Some(SearchSlot {
                        key: previous,
                        index,
                        narrow,
                        ..
                    }) => RowCopy::refresh(
                        index,
                        previous,
                        SearchDiscard {
                            narrow,
                            ..SearchDiscard::default()
                        },
                    ),
                    None => RowCopy::window(
                        &key.window(),
                        SearchDiscard {
                            slot: self.search_slot.take(),
                            ..SearchDiscard::default()
                        },
                    ),
                },
            ))
        });
        SearchPlan {
            request: *request,
            key,
            wrap,
            kind,
        }
    }

    /// The refresh decision: the slot, taken out of the terminal, when bringing its index up
    /// to `key` provably equals building the window afresh. `None` leaves the slot alone.
    /// Given the guards:
    ///
    /// - The same layout, frame and depth ([`SearchKey::refreshable_from`]): no splice,
    ///   rewrap or renumbering moved a row's key, every line is measured as it was, and
    ///   the window only advanced. Rows below the previous visible base are immutable
    ///   history, and retention and the depth cap only drop the oldest rows.
    /// - `drop_history_below` removes the rows that left the window with fresh-build
    ///   semantics, and the copy ([`Self::search_copy_spans`]) re-reads every row whose
    ///   indexed line may differ, as the build reads it.
    /// - The index retains exactly the window's rows, so no cap eviction fires on either
    ///   path.
    ///
    /// A query still reading the index (an earlier hit) does not stop the refresh: the
    /// build copies a shared index instead of taking it over. The bloom filter keeps stale
    /// bits from replaced rows (false-positive candidates only; saturation self-heals via
    /// `rebuild_bloom`).
    #[inline]
    fn try_refresh_search_index(
        &mut self,
        key: &SearchKey,
        layout: SearchLayout,
    ) -> Option<SearchSlot> {
        self.search_slot
            .take_if(|slot| slot.layout == layout && key.refreshable_from(&slot.key))
    }

    /// The absolute rows a refresh under `key` from `previous` copies, ascending, each span
    /// starting a logical line: the rows whose indexed line may differ from what
    /// `previous`'s index holds. Named in the hold of the plan's first chunk
    /// ([`Self::name_search_copy`]), which also checks that hold's key against the plan's.
    ///
    /// A refresh re-reads from the older of the two visible bases (a row edited while
    /// visible and scrolled into history since must not keep stale text; a rows-grow that
    /// reveals history hands rows back to the screen, editable again), clipped at the
    /// window's floor. Joining soft wraps adds two boundaries where a line can straddle
    /// the rows a refresh keeps and the rows it re-reads, each found by a walk over one
    /// run, a row at a time and as far as [`Self::logical_origin`] walks:
    ///
    /// - The re-read starts at the head of the run the row above the boundary belongs to:
    ///   that row is immutable history, and the boundary row itself may have been rewritten
    ///   out of the run since, so the run's head must be re-joined either way. A run too
    ///   deep to walk back over starts the re-read at the floor.
    /// - The run the floor now lands in lost its head with the rows below the floor; a
    ///   fresh build starts a line at the floor with that run's remaining rows, so they
    ///   are re-read as their own span.
    fn search_copy_spans(
        &self,
        key: &SearchKey,
        layout: SearchLayout,
        previous: &SearchKey,
    ) -> Vec<Range<usize>> {
        let (orphans, start) = self.search_copy_start(key, layout, previous);
        let end = key.window().indexed_end;
        orphans
            .into_iter()
            .chain(std::iter::once(start..end))
            .collect()
    }

    /// [`Self::search_copy_spans`]' decision: the rows of a run the floor orphaned, when
    /// they are re-read as a span of their own, and the row the re-read of the rest of the
    /// window starts at.
    fn search_copy_start(
        &self,
        key: &SearchKey,
        layout: SearchLayout,
        previous: &SearchKey,
    ) -> (Option<Range<usize>>, usize) {
        let window = key.window();
        let floor = window.retained_base;
        let was = previous.window();
        let start = was.visible_base.min(window.visible_base).max(floor);
        if layout == SearchLayout::Physical || start <= floor {
            return (None, start);
        }
        let row = |abs: usize| u64::try_from(abs).unwrap_or(u64::MAX);
        let at = |abs: u64| usize::try_from(abs).unwrap_or(usize::MAX);
        let start = self
            .logical_origin(row(start).saturating_sub(1), row(floor))
            .map_or(floor, at);
        if start <= floor || was.retained_base >= floor || !self.abs_row_wrap_flag(row(floor)) {
            return (None, start);
        }
        let orphans_end = at(self.logical_run_end(row(floor), row(start)));
        if orphans_end >= start {
            (None, floor)
        } else {
            (Some(floor..orphans_end), start)
        }
    }

    /// Copy the plan's next rows, at most `max_rows` of them (at least one), in this one
    /// hold, through [`Self::read_search_rows`]; nothing on a hit. A refresh's first chunk
    /// names its rows before it copies one ([`Self::search_copy_spans`]: two walks of a
    /// soft-wrap run at most). The hold's key is compared with the plan's, so a copy that
    /// read two generations or frames, or rows named in a hold of another generation, says
    /// so at [`Self::finish_search`].
    pub fn read_search_chunk(&self, plan: &mut SearchPlan, max_rows: usize) {
        self.name_search_copy(plan);
        let PlanKind::Build(copy) = &mut plan.kind else {
            return;
        };
        let now = SearchKey::capture(self, plan.request.depth_cap);
        copy.torn |= !plan.key.same_generation(&now);
        copy.frame_moved |= !plan.key.same_frame(&now);
        let mut skip = copy.rows.text.len();
        let mut budget = max_rows.max(1);
        for span in &copy.spans {
            let len = span.len();
            if skip >= len {
                skip -= len;
                continue;
            }
            let take = (len - skip).min(budget);
            let first = u64::try_from(span.start.saturating_add(skip)).unwrap_or(u64::MAX);
            self.read_search_rows(first, take, &mut copy.rows);
            budget -= take;
            skip = 0;
            if budget == 0 {
                break;
            }
        }
    }

    /// Name a refresh's rows ([`Self::search_copy_spans`]) from the key its base was built
    /// under, once, before any row is copied. In the first chunk's hold rather than the
    /// plan's: a terminal that moves in between makes that hold's key check fail, so rows
    /// named from another generation are never kept.
    fn name_search_copy(&self, plan: &mut SearchPlan) {
        if let PlanKind::Build(copy) = &mut plan.kind
            && let Some(previous) = copy.pending.take()
        {
            copy.name_spans(self.search_copy_spans(&plan.key, plan.request.layout, &previous));
        }
    }

    /// End a search in one hold: compare the terminal with the key the search began under,
    /// keep what the slot can use, and hand back what it let go of, unfreed.
    ///
    /// A hit hands its narrowing frames back while the slot still holds the index they
    /// were read off. A build is counted ([`Self::search_index_rebuilds`]), and kept when
    /// its copy is whole: then it is the index of its key's generation whether or not the
    /// terminal has moved on since. Current, it answers the next search; stale, it is the
    /// base the next search refreshes from, so it fills an empty slot but never displaces
    /// a current one. A torn copy may mix two contents and is answered from once, never
    /// kept.
    ///
    /// The [`SearchDiscard`] holds the build the slot did not keep, the slot a kept build
    /// displaced, or the hit's share of its index with the frames the slot did not take
    /// back: when a refresh displaced the hit's index meanwhile, that share is the last.
    pub fn finish_search(&mut self, built: BuiltSearch) -> (SearchVerdict, SearchDiscard) {
        let now = SearchKey::capture(self, built.request.depth_cap);
        let verdict = SearchVerdict {
            consistent: !built.torn && built.key.same_generation(&now),
            frame_stable: !built.frame_moved && built.key.same_frame(&now),
        };
        let mut discard = SearchDiscard::default();
        match built.made {
            Made::Served => {
                discard.narrow = match self
                    .search_slot
                    .as_mut()
                    .filter(|slot| Arc::ptr_eq(&slot.index, &built.index))
                {
                    Some(slot) => std::mem::replace(&mut slot.narrow, built.narrow),
                    None => built.narrow,
                };
                discard.index = Some(built.index);
            }
            Made::Refreshed | Made::Rebuilt => {
                self.search_index_rebuilds = self.search_index_rebuilds.wrapping_add(1);
                if built.made == Made::Refreshed {
                    self.search_index_refreshes = self.search_index_refreshes.wrapping_add(1);
                }
                if !built.torn && (verdict.consistent || self.search_slot.is_none()) {
                    discard.slot = self.search_slot.replace(SearchSlot {
                        key: built.key,
                        layout: built.request.layout,
                        index: built.index,
                        narrow: built.narrow,
                    });
                } else {
                    discard.index = Some(built.index);
                    discard.narrow = built.narrow;
                }
            }
        }
        (verdict, discard)
    }

    /// A full-content search index, reused while the active grid's content is unchanged. It
    /// covers the newest [`DEFAULT_MAX_CACHED_LINES`] retained lines, one per row, keyed by
    /// absolute row (history at `oldest + i`, visible rows at `oldest + scrollback + r`), so
    /// each [`SearchMatch::line`](crate::search::SearchMatch) is an absolute row. Query with
    /// `indexed_search(...).search_results_opts(pat, case, regex)`. A miss refreshes or
    /// rebuilds; the result is immutable so callers cannot alter cached coordinates.
    pub fn indexed_search(&mut self) -> &TerminalSearch {
        let mut plan = self.search_plan(&ONE_SHOT_SEARCH);
        self.read_search_chunk(&mut plan, usize::MAX);
        // Nothing moves inside this borrow, so the build is whole and current and stays; the
        // borrow is no lock, so what it displaced is freed here.
        let _ = self.finish_search(plan.build());
        &self
            .search_slot
            .as_ref()
            .expect("a finished search under &mut leaves its index in the slot")
            .index
    }

    /// Search-index cache misses so far (rebuilds and refreshes), never a reuse.
    /// Introspection only.
    #[must_use]
    #[inline]
    pub const fn search_index_rebuilds(&self) -> u64 {
        self.search_index_rebuilds
    }

    /// Incremental refreshes among those misses; `search_index_rebuilds() -
    /// search_index_refreshes()` is the number of full rebuilds. Introspection only.
    #[must_use]
    #[inline]
    pub const fn search_index_refreshes(&self) -> u64 {
        self.search_index_refreshes
    }

    /// Take the search index's heap out of the terminal, unfreed: the slot and any budgeted
    /// search (in flight or retained for `search_summary`). The whole slot goes: an in-place
    /// [`SearchIndex::release`](aterm_search::SearchIndex::release) would leave the key
    /// matching an emptied index, a false empty hit.
    #[inline]
    pub fn take_search_index(&mut self) -> SearchDiscard {
        SearchDiscard {
            slot: self.search_slot.take(),
            budgeted: self.budgeted_search.take(),
            ..SearchDiscard::default()
        }
    }

    /// [`Self::take_search_index`], freed in this call.
    #[inline]
    pub fn release_search_index(&mut self) {
        drop(self.take_search_index());
    }
}

/// The windowed builder: index a plan's copied rows, with no lock held. Its line set,
/// order and absolute-row keys define search results.
///
/// From nothing, an index that retains exactly the window's rows; on a refresh, the base
/// with the rows below the window dropped, taken over when no query still reads it and
/// copied otherwise. Each span's rows are joined per `layout` (the span's first row starts
/// a line: the index has nothing older to join it to) and indexed at their absolute rows,
/// a re-read row replacing the line it had. The completeness verdict is restated from the
/// window every time: `Some(floor)` while the depth cap leaves older retained history
/// unread, else `None`, so an index outliving the condition that made it partial (a
/// `clear`, a scrollback eviction, a raised cap) does not go on calling an exact count
/// incomplete.
pub(super) fn build_search_index(
    key: &SearchKey,
    layout: SearchLayout,
    copy: RowCopy,
) -> TerminalSearch {
    let window = key.window();
    let mut index = match copy.base {
        Some(base) => {
            let mut index = Arc::unwrap_or_clone(base);
            index.drop_history_below(window.retained_base);
            index
        }
        None => TerminalSearch::with_capacity_and_max(window.retained_total, key.max_lines)
            .with_ambiguous_width(key.ambiguous),
    };
    let SearchRows {
        text: mut lines,
        mut cells,
        mut wrapped,
    } = copy.rows;
    if layout == SearchLayout::Logical {
        let mut at = 0usize;
        for span in &copy.spans {
            let next = at.saturating_add(span.len()).min(lines.len());
            if let (Some(lines), Some(cells), Some(wrapped)) = (
                lines.get_mut(at..next),
                cells.get_mut(at..next),
                wrapped.get_mut(at..next),
            ) {
                if let Some(first) = wrapped.first_mut() {
                    *first = false;
                }
                join_wrapped_rows(lines, cells, wrapped, usize::from(key.cols));
            }
            at = next;
        }
    }
    let rows = copy.spans.into_iter().flatten();
    index.index_numbered_cells_owned(
        rows.zip(lines.into_iter().zip(cells))
            .map(|(row, (text, cells))| (row, text, cells)),
    );
    index.set_history_prefix_eviction((window.skipped_prefix > 0).then_some(window.retained_base));
    index
}

#[cfg(test)]
pub(crate) mod tests {
    use super::Terminal;

    /// The from-scratch oracle: the index built the uncached, per-line way.
    pub(crate) fn legacy_results(t: &Terminal, pat: &str) -> Vec<(usize, usize, usize)> {
        use crate::search::TerminalSearch;
        let grid = t.grid();
        let oldest = grid.oldest_absolute_row();
        let scrollback = grid.scrollback_lines();
        let rows = t.rows() as usize;
        let mut search = TerminalSearch::new().with_ambiguous_width(t.ambiguous_width());
        let hist_base = usize::try_from(oldest).unwrap_or(usize::MAX);
        let history: Vec<_> = (0..scrollback)
            .map(|i| {
                let (text, cells) = grid
                    .get_history_line(i)
                    .map(|l| {
                        let text = l.to_string();
                        let cells = super::super::history_line_text_cells(grid, &l, &text);
                        (text, cells)
                    })
                    .unwrap_or_default();
                (hist_base + i, text, cells)
            })
            .collect();
        search.index_numbered_cells_owned(history);
        let vis_base = hist_base.saturating_add(scrollback);
        let visible: Vec<_> = (0..rows)
            .map(|r| {
                let (text, cells) = t.written_line_cells(r as i32).unwrap_or_default();
                (vis_base + r, text, cells)
            })
            .collect();
        search.index_numbered_cells_owned(visible);
        let res = search
            .search_results_opts(pat, false, false)
            .expect("search ok");
        res.matches
            .iter()
            .map(|m| (m.line, m.start_col, m.len()))
            .collect()
    }

    /// History through a tiered store with a corrupt warm block between a needle in cold
    /// and one in hot: a skipping reader would shift later lines onto neighbours' rows.
    pub(crate) fn corrupt_tiered_terminal() -> (Terminal, usize) {
        use aterm_grid::Grid;
        use aterm_scrollback::Scrollback;
        // Cold holds the needle at 2, hot the one at 38; the corrupt block sits at the
        // warm front between them and nothing evicts it.
        let mut sb = Scrollback::with_block_size(4, 12, 10_000_000, 4);
        for i in 0..40 {
            match i {
                2 => sb.push_str("NEEDLE_before the corrupt block"),
                38 => sb.push_str("NEEDLE_after the corrupt block"),
                _ => sb.push_str(&format!("carried {i:03}")),
            }
        }
        assert!(
            sb.cold_line_count() > 2,
            "the first needle must sit in cold"
        );
        let corrupt = 5;
        sb.inject_corrupted_warm_block(corrupt);
        let mut t = Terminal::with_grid(Grid::with_tiered_scrollback(6, 40, 5, sb));
        for i in 0..20 {
            t.process(format!("live {i:03}\r\n").as_bytes());
        }
        assert_eq!(
            t.grid()
                .history_lines_from(0)
                .filter(Option::is_none)
                .count(),
            corrupt,
            "the fixture's corrupt block is still in the store"
        );
        (t, corrupt)
    }

    /// Over a corrupt tiered segment the cached index equals the oracle, and the needle
    /// after the corrupt block lands on its own row.
    #[test]
    fn search_over_a_corrupt_tiered_segment_keeps_absolute_rows() {
        let (mut t, corrupt) = corrupt_tiered_terminal();
        let got = cached_results(&mut t, "NEEDLE_");
        assert_eq!(got, legacy_results(&t, "NEEDLE_"));
        assert_eq!(got.len(), 2, "both needles are found: {got:?}");
        let oldest = usize::try_from(t.grid().oldest_absolute_row()).expect("row");
        // The corrupt block's placeholder rows sit between the needles.
        assert_eq!(got[0].0, oldest + 2);
        assert_eq!(got[1].0, oldest + 38 + corrupt);
    }

    /// A history line longer than the read bound (a crafted checkpoint can store one) is
    /// indexed up to the bound and counted as a line not searched to its end, by the
    /// full build, the refresh and the budgeted search alike, until it leaves the
    /// history.
    #[test]
    fn a_history_line_past_the_read_bound_is_counted_cut_short() {
        use crate::grid::Grid;
        use crate::scrollback::{Line, Scrollback};
        use crate::terminal::selection::MAX_SCROLLBACK_LINE_SCAN_BYTES;
        let terminal = |len: usize| {
            let mut line = b"NEEDLE ".to_vec();
            line.resize(len, b'A');
            let mut scrollback = Scrollback::with_defaults();
            scrollback.push_line(Line::from_bytes(&line));
            Terminal::with_grid(Grid::with_tiered_scrollback(4, 80, 1000, scrollback))
        };
        let summary = |results: &crate::search::SearchResults| {
            (
                results.matches.len(),
                results.cut_short_lines,
                results.incomplete,
            )
        };
        let search = |t: &mut Terminal, regex: bool| {
            let got = t
                .indexed_search()
                .search_results_opts("needle", false, regex)
                .expect("searched");
            summary(&got)
        };
        // NEGATIVE CONTROL: a line exactly at the bound is read whole.
        let mut whole = terminal(MAX_SCROLLBACK_LINE_SCAN_BYTES);
        assert_eq!(search(&mut whole, false), (1, 0, false));

        let mut t = terminal(MAX_SCROLLBACK_LINE_SCAN_BYTES + 1);
        for regex in [false, true] {
            assert_eq!(search(&mut t, regex), (1, 1, true), "regex {regex}");
            let step = t
                .search_budgeted("needle", false, regex, None, 100)
                .expect("budgeted search");
            assert!(step.complete, "one step covers the fixture");
            assert_eq!(
                summary(&step.results),
                (1, 1, true),
                "budgeted, regex {regex}"
            );
        }
        let refreshes = t.search_index_refreshes();
        t.process(b"more output\r\n");
        assert_eq!(search(&mut t, false), (1, 1, true), "the refreshed index");
        assert!(
            t.search_index_refreshes() > refreshes,
            "precondition: a refresh served it"
        );
        t.process(b"\x1b[3J");
        assert_eq!(search(&mut t, false), (0, 0, false), "the line was erased");
    }

    /// Absolute rows only grow and survive live upgrades, so a long-lived tab passes row
    /// `u32::MAX`: find (full build, refresh, regex and literal) keeps answering there.
    #[test]
    fn find_keeps_working_once_absolute_rows_pass_u32_max() {
        for start in [(1u64 << 32) - 30, (1u64 << 32) + 10] {
            let mut t = Terminal::new(6, 40);
            t.grid_mut().continue_absolute_numbering(start);
            for i in 0..40 {
                t.process(format!("filler {i}\r\n").as_bytes());
            }
            t.process(b"hello world\r\n");
            let first = cached_results(&mut t, "hello");
            assert_eq!(first, legacy_results(&t, "hello"), "start={start}");
            assert_eq!(first.len(), 1, "start={start}: {first:?}");
            let row = first[0].0;
            assert!(row as u64 >= start, "start={start}: row {row} is absolute");
            let regex = t
                .indexed_search()
                .search_results_opts("hel+o", false, true)
                .expect("valid regex");
            assert_eq!(
                regex.matches.iter().map(|m| m.line).collect::<Vec<_>>(),
                vec![row],
                "start={start}"
            );

            let refreshes = t.search_index_refreshes();
            for i in 0..10 {
                t.process(format!("more {i}\r\n").as_bytes());
            }
            t.process(b"hello again\r\n");
            let second = cached_results(&mut t, "hello");
            assert!(
                t.search_index_refreshes() > refreshes,
                "the incremental refresh served it"
            );
            assert_eq!(second, legacy_results(&t, "hello"), "start={start}");
            assert_eq!(second.len(), 2, "start={start}: {second:?}");
            assert_eq!(
                second[0].0, row,
                "start={start}: the first hit kept its row"
            );
        }
    }

    pub(crate) fn cached_results(t: &mut Terminal, pat: &str) -> Vec<(usize, usize, usize)> {
        let res = t
            .indexed_search()
            .search_results_opts(pat, false, false)
            .expect("search ok");
        res.matches
            .iter()
            .map(|m| (m.line, m.start_col, m.len()))
            .collect()
    }

    /// A line's matches do not change when it scrolls into history: both are read as
    /// written, trailing blanks kept, so `foo$` and a query ending in printed spaces answer
    /// the same in either place, for the full build, the refresh and the budgeted search.
    #[test]
    fn a_rows_matches_do_not_change_when_it_scrolls_into_history() {
        let regex = |t: &mut Terminal, pat: &str| -> Vec<(usize, usize, usize)> {
            let res = t
                .indexed_search()
                .search_results_opts(pat, false, true)
                .expect("search ok");
            res.matches
                .iter()
                .map(|m| (m.line, m.start_col, m.len()))
                .collect()
        };
        let budgeted = |t: &mut Terminal, pat: &str| -> Vec<(usize, usize, usize)> {
            let step = t
                .search_budgeted(pat, false, true, None, 100)
                .expect("budgeted search");
            assert!(step.complete, "one step covers the fixture");
            step.results
                .matches
                .iter()
                .map(|m| (m.line, m.start_col, m.len()))
                .collect()
        };
        let mut t = Terminal::new(6, 20);
        t.process(b"foo   \r\nbar \x1b[41m  \x1b[0m\r\nbaz\r\nfoo");
        assert_eq!(
            t.grid().scrollback_lines(),
            0,
            "precondition: all on screen"
        );
        let queries = ["foo$", "bar$", "foo  ", "bar   "];
        let on_screen: Vec<_> = queries.iter().map(|q| regex(&mut t, q)).collect();
        assert_eq!(
            on_screen[2],
            vec![(0, 0, 5)],
            "the spaces the program printed are on the line: {on_screen:?}"
        );
        assert_eq!(
            on_screen[0],
            vec![(3, 0, 3)],
            "only the line that ends at `foo` ends at `foo`"
        );
        let budgeted_on_screen: Vec<_> = queries.iter().map(|q| budgeted(&mut t, q)).collect();
        assert_eq!(budgeted_on_screen, on_screen, "the budgeted search");
        assert_eq!(
            t.abs_row_text(0).as_deref(),
            Some("foo   "),
            "a match's snippet is the line it came from"
        );

        let refreshes = t.search_index_refreshes();
        t.process(b"\r\nqux");
        let refreshed: Vec<_> = queries.iter().map(|q| regex(&mut t, q)).collect();
        assert!(
            t.search_index_refreshes() > refreshes,
            "precondition: a refresh served it"
        );
        assert_eq!(refreshed, on_screen, "the refreshed index");

        t.process(b"\r\n1\r\n2\r\n3\r\n4\r\n5\r\n6");
        assert!(
            t.grid().scrollback_lines() >= 4,
            "precondition: every fixture line is history now"
        );
        let in_history: Vec<_> = queries.iter().map(|q| regex(&mut t, q)).collect();
        assert_eq!(in_history, on_screen);
        let budgeted_in_history: Vec<_> = queries.iter().map(|q| budgeted(&mut t, q)).collect();
        assert_eq!(budgeted_in_history, on_screen, "the budgeted search");
        assert_eq!(t.abs_row_text(0).as_deref(), Some("foo   "), "the snippet");
    }

    /// Where the grid paints `needle`'s first character on visible `row`.
    fn painted_col(t: &Terminal, row: usize, needle: char) -> Option<usize> {
        let cols = usize::from(t.cols());
        (0..cols).find(|&col| {
            t.cell_grapheme(row, col).as_deref() == Some(needle.encode_utf8(&mut [0; 4]))
        })
    }

    /// The grid folds a character into the cell before it only when that cell is the one
    /// before the cursor, so cells written out of order or joined by DCH stay apart where a
    /// walk of the text joins them. Every reader (cached build and refresh, oracle,
    /// budgeted, literal and regex) finds `XYZ` where the grid paints it.
    #[test]
    fn find_reports_matches_in_the_cells_the_grid_drew_them_in() {
        let cases: &[(&str, &str)] = &[
            // Control: written in order, the grid pairs the flag.
            ("in order", "\u{1f1e6}\u{1f1e7}XYZ"),
            (
                "flag halves, right one first",
                "\x1b[1;3H\u{1f1e7}\x1b[1;1H\u{1f1e6}\x1b[1;5HXYZ",
            ),
            (
                "skin tone before its base",
                "\x1b[1;3H\u{1f3fd}\x1b[1;1H\u{1f44d}\x1b[1;5HXYZ",
            ),
            (
                "a cell ending in ZWJ, the next drawn first",
                "\x1b[1;2H\u{e9}\x1b[1;1Ha\u{200d}\x1b[1;3HXYZ",
            ),
            (
                "flag halves brought together by DCH",
                "\u{1f1e6}q\u{1f1e7}XYZ\x1b[1;3H\x1b[P",
            ),
            (
                "skin tone brought to its base by DCH",
                "\u{1f44d}q\u{1f3fd}XYZ\x1b[1;3H\x1b[P",
            ),
            // VS15 narrows a wide glyph and empties its right half, which reads as a
            // blank: a walk of the text counts the glyph's two columns and the blank.
            ("a wide glyph narrowed by VS15", "\u{231a}\u{fe0e}XYZ"),
        ];
        let budgeted = |t: &mut Terminal, pat: &str, regex: bool| -> Vec<(usize, usize, usize)> {
            let step = t
                .search_budgeted(pat, false, regex, None, 100)
                .expect("budgeted search");
            assert!(step.complete, "one step covers the fixture");
            step.results
                .matches
                .iter()
                .map(|m| (m.line, m.start_col, m.len()))
                .collect()
        };
        let mut wrong = Vec::new();
        for (label, input) in cases {
            let mut t = Terminal::new(4, 20);
            t.process(input.as_bytes());
            let painted = painted_col(&t, 0, 'X').expect("X is painted");
            let expected = vec![(0, painted, 3)];
            let regex = t
                .indexed_search()
                .search_results_opts("X.Z", false, true)
                .expect("regex search")
                .matches
                .iter()
                .map(|m| (m.line, m.start_col, m.len()))
                .collect::<Vec<_>>();
            let found = [
                ("cached", cached_results(&mut t, "xyz")),
                ("oracle", legacy_results(&t, "xyz")),
                ("budgeted", budgeted(&mut t, "XYZ", false)),
                ("regex", regex),
                ("budgeted regex", budgeted(&mut t, "X.Z", true)),
            ];
            for (reader, got) in found {
                if got != expected {
                    wrong.push(format!(
                        "{label} ({reader}): painted {expected:?}, found {got:?}"
                    ));
                }
            }
            let refreshes = t.search_index_refreshes();
            t.process(b"\x1b[3;1Hmore");
            let refreshed = cached_results(&mut t, "xyz");
            assert!(
                t.search_index_refreshes() > refreshes,
                "{label}: a refresh served it"
            );
            if refreshed != expected {
                wrong.push(format!(
                    "{label} (refresh): painted {expected:?}, found {refreshed:?}"
                ));
            }
        }
        assert!(
            wrong.is_empty(),
            "find names cells the match is not drawn in:\n  {}",
            wrong.join("\n  ")
        );
    }

    /// A redraw that moves cells under unchanged text moves the refreshed matches with them.
    #[test]
    fn a_redraw_that_moves_cells_under_the_same_text_moves_the_matches() {
        let mut t = Terminal::new(4, 20);
        t.process("\x1b[1;3H\u{1f1e7}\x1b[1;1H\u{1f1e6}\x1b[1;5HXYZ".as_bytes());
        assert_eq!(cached_results(&mut t, "xyz"), vec![(0, 4, 3)]);
        t.process("\x1b[1;1H\x1b[2K\u{1f1e6}\u{1f1e7}XYZ".as_bytes());
        assert_eq!(
            t.written_line_text(0).as_deref(),
            Some("\u{1f1e6}\u{1f1e7}XYZ")
        );
        assert_eq!(
            painted_col(&t, 0, 'X'),
            Some(2),
            "precondition: the pair folded"
        );
        assert_eq!(cached_results(&mut t, "xyz"), vec![(0, 2, 3)]);
    }

    /// Printable ASCII after a cell ending in a ZWJ: the grid draws the `Q` of `❤️‍Qa` in a
    /// cell of its own, two columns right of the emoji, on either history tier, and find
    /// reports it there.
    #[test]
    fn ascii_after_a_zwj_is_found_where_either_history_tier_draws_it() {
        use aterm_grid::test_counters::take_ring_fast_materialize;
        for (tier, ring) in [("ring tier", None), ("past the ring", Some(1))] {
            let mut t = match ring {
                None => Terminal::new(3, 12),
                Some(ring) => Terminal::with_scrollback(
                    3,
                    12,
                    ring,
                    crate::scrollback::Scrollback::with_defaults(),
                ),
            };
            t.process("\u{2764}\u{fe0f}\u{200d}Qa\r\nmore\r\n\r\n\r\n\r\n".as_bytes());
            let oldest = t.grid().scrollback_lines() - 1;
            let _ = take_ring_fast_materialize();
            let _ = t.grid().materialize_scrollback_row_full(oldest, t.cols());
            assert_eq!(
                take_ring_fast_materialize(),
                usize::from(ring.is_none()),
                "precondition: the row is read from the {tier}"
            );
            t.scroll_to_top();
            let painted: Vec<_> = (0..4)
                .map(|col| t.display_cell_grapheme(0, col).expect("in range"))
                .collect();
            t.scroll_to_bottom();
            assert_eq!(
                painted,
                ["\u{2764}\u{fe0f}\u{200d}", "", "Q", "a"],
                "{tier}: the glass draws Q in a cell of its own"
            );
            assert_eq!(cached_results(&mut t, "q"), vec![(0, 2, 1)], "{tier}");
            let step = t
                .search_budgeted("Q", false, false, None, 100)
                .expect("budgeted search");
            assert!(step.complete, "one step covers the fixture");
            let budgeted: Vec<_> = step
                .results
                .matches
                .iter()
                .map(|m| (m.line, m.start_col, m.len()))
                .collect();
            assert_eq!(budgeted, vec![(0, 2, 1)], "{tier}: the budgeted search");
        }
    }

    /// The shapes of `find_reports_matches_in_the_cells_the_grid_drew_them_in`, scrolled into
    /// history: `XYZ` is found where either tier draws it. The ring tier draws a row from
    /// its own cells, which keep the shapes apart; a row past the ring from its stored line,
    /// whose walk joins them.
    #[test]
    fn find_reports_matches_where_either_history_tier_draws_them() {
        use aterm_grid::test_counters::take_ring_fast_materialize;
        let cases: &[(&str, &str)] = &[
            ("in order", "\u{1f1e6}\u{1f1e7}XYZ"),
            (
                "flag halves, right one first",
                "\x1b[1;3H\u{1f1e7}\x1b[1;1H\u{1f1e6}\x1b[1;5HXYZ",
            ),
            (
                "skin tone before its base",
                "\x1b[1;3H\u{1f3fd}\x1b[1;1H\u{1f44d}\x1b[1;5HXYZ",
            ),
            (
                "a cell ending in ZWJ, the next drawn first",
                "\x1b[1;2H\u{e9}\x1b[1;1Ha\u{200d}\x1b[1;3HXYZ",
            ),
            (
                "flag halves brought together by DCH",
                "\u{1f1e6}q\u{1f1e7}XYZ\x1b[1;3H\x1b[P",
            ),
            ("a wide glyph narrowed by VS15", "\u{231a}\u{fe0e}XYZ"),
        ];
        let mut wrong = Vec::new();
        for (tier, ring) in [("ring tier", None), ("past the ring", Some(1))] {
            for (label, input) in cases {
                let mut t = match ring {
                    None => Terminal::new(4, 20),
                    Some(ring) => Terminal::with_scrollback(
                        4,
                        20,
                        ring,
                        crate::scrollback::Scrollback::with_defaults(),
                    ),
                };
                t.process(input.as_bytes());
                t.process(b"\x1b[4;1H\r\n\r\n\r\n\r\n");
                let oldest = t.grid().scrollback_lines() - 1;
                let _ = take_ring_fast_materialize();
                let _ = t.grid().materialize_scrollback_row_full(oldest, t.cols());
                assert_eq!(
                    take_ring_fast_materialize(),
                    usize::from(ring.is_none()),
                    "{tier}, {label}: precondition: the row is drawn from the {tier}"
                );
                t.scroll_to_top();
                let painted = (0..usize::from(t.cols()))
                    .find(|&col| t.display_cell_grapheme(0, col).as_deref() == Some("X"))
                    .expect("X is painted");
                t.scroll_to_bottom();
                let expected = vec![(0, painted, 3)];
                let step = t
                    .search_budgeted("XYZ", false, false, None, 100)
                    .expect("budgeted search");
                assert!(step.complete, "one step covers the fixture");
                let budgeted: Vec<_> = step
                    .results
                    .matches
                    .iter()
                    .map(|m| (m.line, m.start_col, m.len()))
                    .collect();
                for (reader, got) in [
                    ("cached", cached_results(&mut t, "xyz")),
                    ("oracle", legacy_results(&t, "xyz")),
                    ("budgeted", budgeted),
                ] {
                    if got != expected {
                        wrong.push(format!(
                            "{tier}, {label} ({reader}): painted {expected:?}, found {got:?}"
                        ));
                    }
                }
            }
        }
        assert!(
            wrong.is_empty(),
            "find names history cells the match is not drawn in:\n  {}",
            wrong.join("\n  ")
        );
    }

    /// A terminal whose `ambiguous_width` policy is `"wide"`.
    fn wide_terminal(rows: u16, cols: u16) -> Terminal {
        let mut t = Terminal::new(rows, cols);
        t.apply_config(&crate::config::TerminalConfig {
            ambiguous_width_double: true,
            ..Default::default()
        });
        t
    }

    /// With `ambiguous_width = "wide"` find reports the cells an East Asian Ambiguous
    /// character is drawn in (two), one-shot and budgeted.
    #[test]
    fn find_measures_ambiguous_characters_as_the_grid_draws_them() {
        let mut t = wide_terminal(4, 20);
        t.process("→→foo bar\r\n─── foo".as_bytes());
        assert!(
            t.grid().is_wide_continuation_at(0, 3) && t.grid().is_wide_continuation_at(1, 5),
            "precondition: the grid draws them wide"
        );
        assert_eq!(cached_results(&mut t, "foo"), vec![(0, 4, 3), (1, 7, 3)]);
        assert_eq!(legacy_results(&t, "foo"), cached_results(&mut t, "foo"));
        let budgeted = t
            .search_budgeted("foo", false, false, None, 100)
            .expect("budgeted search");
        assert!(budgeted.complete);
        let budgeted: Vec<_> = budgeted
            .results
            .matches
            .iter()
            .map(|m| (m.line, m.start_col, m.len()))
            .collect();
        assert_eq!(budgeted, vec![(0, 4, 3), (1, 7, 3)]);
    }

    /// A live switch to `"wide"` moves no content generation, so the next search must not
    /// serve the narrow index for a line written after it.
    #[test]
    fn a_live_switch_to_wide_ambiguous_width_is_not_served_from_the_narrow_index() {
        let mut t = Terminal::new(4, 20);
        t.process(b"plain foo\r\n");
        assert_eq!(cached_results(&mut t, "foo"), vec![(0, 6, 3)]);
        t.apply_config(&crate::config::TerminalConfig {
            ambiguous_width_double: true,
            ..Default::default()
        });
        t.process("→→foo".as_bytes());
        assert_eq!(cached_results(&mut t, "foo"), vec![(0, 6, 3), (1, 4, 3)]);
    }

    /// A live switch with no new output: the cached index, a resumed budgeted search and the
    /// summary each answer as a fresh build would. A row written before the switch keeps
    /// the cells it was drawn in (`apply_config` reflows nothing), and find reads them.
    #[test]
    fn a_live_switch_with_no_new_output_is_not_served_from_a_cache() {
        let mut t = Terminal::new(4, 20);
        t.process("→→foo\r\nx\r\nfoo".as_bytes());
        let narrow = cached_results(&mut t, "foo");
        let step = t
            .search_budgeted("foo", false, false, None, 100)
            .expect("budgeted search");
        assert!(step.complete, "precondition: a completed budgeted scan");
        let partial = t
            .search_budgeted("foo", false, false, None, 1)
            .expect("budgeted search");
        let cursor = partial.cursor.expect("precondition: a scan in flight");

        t.apply_config(&crate::config::TerminalConfig {
            ambiguous_width_double: true,
            ..Default::default()
        });
        let fresh = legacy_results(&t, "foo");
        assert_eq!(
            painted_col(&t, 0, 'f'),
            Some(2),
            "the grid reflowed nothing"
        );
        assert_eq!(
            fresh, narrow,
            "a row keeps the cells it was drawn in: {fresh:?}"
        );
        assert_eq!(fresh.first(), Some(&(0, 2, 3)));
        assert_eq!(cached_results(&mut t, "foo"), fresh, "the cached index");

        let resumed = t
            .search_budgeted("foo", false, false, Some(cursor), 100)
            .expect("budgeted search");
        assert!(
            resumed.reset,
            "the in-flight scan restarts under the new policy"
        );

        let mut t = Terminal::new(4, 20);
        t.process("→→foo\r\nx\r\nfoo".as_bytes());
        let step = t
            .search_budgeted("foo", false, false, None, 100)
            .expect("budgeted search");
        assert!(step.complete, "precondition: a completed budgeted scan");
        t.apply_config(&crate::config::TerminalConfig {
            ambiguous_width_double: true,
            ..Default::default()
        });
        let summary: Vec<_> = t
            .search_summary_results("foo", false, false)
            .expect("summary")
            .matches
            .iter()
            .map(|m| (m.line, m.start_col, m.len()))
            .collect();
        assert_eq!(summary, fresh, "the summary");
    }

    /// After `set_scrollback_line_limit` evicts lines, search returns no evicted rows, on
    /// the tiered store and on the ring-only grid (the wasm engines' shape).
    #[test]
    fn retention_shrink_invalidates_the_cached_index() {
        // A small ring, so the bulk of history lives in the store.
        let sb = aterm_scrollback::Scrollback::new(64, 512, 8_000_000);
        let mut tiered = Terminal::with_scrollback(6, 40, 8, sb);
        let check = |t: &mut Terminal, label: &str| {
            for i in 0..200 {
                t.process(format!("needle-{i}\r\n").as_bytes());
            }
            let before = cached_results(t, "needle-");
            assert_eq!(before.len(), 200, "{label}: all lines retained pre-shrink");

            t.set_scrollback_line_limit(Some(50));

            let after = cached_results(t, "needle-");
            assert_eq!(
                after,
                legacy_results(t, "needle-"),
                "{label}: the post-shrink search equals a from-scratch rebuild (not stale)"
            );
            let oldest = usize::try_from(t.grid().oldest_absolute_row()).unwrap();
            assert!(
                after.iter().all(|&(line, _, _)| line >= oldest),
                "{label}: no match may point at an evicted absolute row (< {oldest})"
            );
            assert!(
                after.len() < before.len(),
                "{label}: the shrink really evicted matches"
            );
        };
        check(&mut tiered, "tiered");

        let mut ring = Terminal::new(6, 40);
        check(&mut ring, "ring-only");
    }

    /// A memory-budget shrink evicts cold scrollback, so the search cache invalidates.
    #[test]
    fn memory_budget_shrink_invalidates_the_cached_index() {
        // Small hot/warm tiers spill history to the cold tier, which a tight budget evicts.
        let sb = aterm_scrollback::Scrollback::new(8, 16, 64_000_000);
        let mut t = Terminal::with_scrollback(6, 40, 8, sb);
        for i in 0..400 {
            t.process(format!("needle-{i}\r\n").as_bytes());
        }
        let before = cached_results(&mut t, "needle-");
        assert_eq!(before.len(), 400, "all lines retained + found pre-shrink");
        let lines_before = t.grid().scrollback_lines();

        // An EnforcementFailed (the hot tier holds active data) is ignored: the cold
        // eviction still happened.
        let _ = t.set_memory_budget(4096);
        assert!(
            t.grid().scrollback_lines() < lines_before,
            "a tight budget must really evict retained lines \
             ({} -> {})",
            lines_before,
            t.grid().scrollback_lines()
        );

        // No write since the prime: the cache still rebuilds.
        let after = cached_results(&mut t, "needle-");
        assert_eq!(
            after,
            legacy_results(&t, "needle-"),
            "the post-budget-shrink search equals a from-scratch rebuild (not stale)"
        );
        let oldest = usize::try_from(t.grid().oldest_absolute_row()).unwrap();
        assert!(
            after.iter().all(|&(line, _, _)| line >= oldest),
            "no match may point at an evicted absolute row (< {oldest})"
        );
        assert!(
            after.len() < before.len(),
            "the budget shrink really evicted matches ({} -> {})",
            before.len(),
            after.len()
        );
    }

    /// `release_search_index` makes the next search rebuild, identically (never a false
    /// empty hit).
    #[test]
    fn release_drops_cache_then_next_search_rebuilds_identically() {
        let mut t = Terminal::new(6, 40);
        t.process(b"NEEDLE_alpha\r\n");
        for i in 0..20 {
            t.process(format!("filler line {i}\r\n").as_bytes());
        }
        let r1 = cached_results(&mut t, "NEEDLE_alpha");
        assert_eq!(r1.len(), 1);
        let rebuilds = t.search_index_rebuilds();

        t.release_search_index();
        let r2 = cached_results(&mut t, "NEEDLE_alpha");
        assert_eq!(
            t.search_index_rebuilds(),
            rebuilds + 1,
            "release must force the next search to rebuild (real reclaim)"
        );
        assert_eq!(
            r2, r1,
            "post-release results are byte-identical (not empty)"
        );
    }

    /// A pure viewport scroll moves no `content_seq`, so the cache is reused.
    #[test]
    fn viewport_scroll_reuses_cache() {
        let mut t = Terminal::new(6, 40);
        t.process(b"NEEDLE_alpha\r\n");
        for i in 0..20 {
            t.process(format!("filler line {i}\r\n").as_bytes());
        }
        let r1 = cached_results(&mut t, "NEEDLE_alpha");
        let rebuilds = t.search_index_rebuilds();
        let gen_before = t.content_seq();

        t.grid_mut().scroll_display(5);
        assert_eq!(
            t.content_seq(),
            gen_before,
            "a pure viewport scroll must NOT bump content_seq"
        );

        let r2 = cached_results(&mut t, "NEEDLE_alpha");
        assert_eq!(
            t.search_index_rebuilds(),
            rebuilds,
            "a pure viewport scroll must NOT invalidate the search cache"
        );
        assert_eq!(r1, r2, "results identical across a viewport scroll");
    }

    /// `restore_checkpoint` replaces the grids, whose fresh stamps could collide with the
    /// cache key, so it releases the index and the next search rebuilds.
    #[test]
    fn restore_checkpoint_drops_the_cached_search_index() {
        let mut source = Terminal::new(6, 40);
        for i in 0..30 {
            source.process(format!("alpha {i} needle\r\n").as_bytes());
        }
        let saved = source.checkpoint();

        let mut target = Terminal::new(6, 40);
        for i in 0..30 {
            target.process(format!("beta {i} other\r\n").as_bytes());
        }
        let primed = cached_results(&mut target, "needle");
        assert!(
            primed.is_empty(),
            "precondition: no needle before the restore"
        );

        target.restore_checkpoint(&saved);
        let rebuilds_before = target.search_index_rebuilds();
        let after = cached_results(&mut target, "needle");
        assert_eq!(
            target.search_index_rebuilds(),
            rebuilds_before + 1,
            "the restored buffer must be indexed afresh, never served from the \
             pre-restore cache"
        );
        assert_eq!(
            after,
            legacy_results(&target, "needle"),
            "post-restore search must equal a from-scratch rebuild"
        );
        assert!(!after.is_empty(), "the restored content is searchable");
    }

    /// A long deterministic mix of appends, edits, erases, scrollback clears, resizes,
    /// alt-screen switches, retention shrinks and Kitty unscrolls, searching after every
    /// step: the cached path equals `legacy_results` each time, over both arms.
    #[test]
    fn scripted_mutation_mixture_keeps_the_refresh_equal_to_a_rebuild() {
        let sb = aterm_scrollback::Scrollback::new(64, 512, 8_000_000);
        let mut t = Terminal::with_scrollback(8, 40, 64, sb);
        t.set_scrollback_line_limit(Some(600));

        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut roll = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };

        let mut written = 0usize;
        let mut on_alt = false;
        let mut deepest = 0usize;
        let full_rebuilds = |t: &Terminal| t.search_index_rebuilds() - t.search_index_refreshes();
        let refreshes_at_start = t.search_index_refreshes();
        let fulls_at_start = full_rebuilds(&t);

        for step in 0..140u64 {
            let r = roll();
            match r % 12 {
                // Streaming output, most of the script, so history accumulates.
                0..=6 => {
                    for _ in 0..=(r >> 8) % 14 {
                        t.process(format!("needle {written} alpha row\r\n").as_bytes());
                        written += 1;
                    }
                }
                // In-place edit of visible rows (the only mutable rows).
                7 => {
                    t.process(b"\x1b[H");
                    t.process(format!("beta {step} needle overwrite").as_bytes());
                }
                8 => {
                    if r % 48 == 8 {
                        t.process(b"\x1b[3J");
                    } else {
                        t.process(b"\x1b[2J");
                    }
                }
                // Height-only reclassifies rows without moving keys; a width change
                // rewraps history (rebuild fence).
                9 => {
                    if r % 2 == 0 {
                        let cols = t.cols();
                        t.resize(4 + u16::try_from(r % 8).unwrap_or(0), cols);
                    } else {
                        let rows = t.rows();
                        t.resize(rows, 24 + u16::try_from(r % 24).unwrap_or(0));
                    }
                }
                // Alt-screen switch (rebuild fence, both directions).
                10 => {
                    if on_alt {
                        t.process(b"\x1b[?1049l");
                    } else {
                        t.process(b"\x1b[?1049h");
                    }
                    on_alt = !on_alt;
                }
                // Kitty CSI +T unscroll (seen only via the epoch), and sometimes a
                // retention shrink.
                _ => {
                    if !on_alt {
                        let n = 1 + usize::try_from(r % 3).unwrap_or(0);
                        let _ = t.grid_mut().unscroll_from_scrollback(n);
                    }
                    if r % 3 == 0 {
                        t.set_scrollback_line_limit(Some(
                            250 + usize::try_from(r % 350).unwrap_or(0),
                        ));
                    }
                }
            }
            deepest = deepest.max(t.grid().scrollback_lines());

            for query in ["needle", "alpha", "beta"] {
                assert_eq!(
                    cached_results(&mut t, query),
                    legacy_results(&t, query),
                    "step {step} (roll {r:#x}), query {query:?}: the cached path \
                     must equal a from-scratch rebuild"
                );
            }
        }

        assert!(
            deepest >= 150,
            "the script must build REAL history depth for the oracle to mean \
             anything (deepest scrollback was {deepest})"
        );
        assert!(
            t.search_index_refreshes() > refreshes_at_start,
            "the script must exercise the incremental refresh arm"
        );
        assert!(
            full_rebuilds(&t) > fulls_at_start,
            "the script must also exercise the full-rebuild fallback arm"
        );
    }

    /// After streaming output every miss is an incremental refresh equal to the oracle.
    #[test]
    fn streaming_output_refreshes_incrementally_and_equals_legacy() {
        let mut t = Terminal::new(6, 40);
        for i in 0..50 {
            t.process(format!("seed line {i} needle\r\n").as_bytes());
        }
        let r0 = cached_results(&mut t, "needle");
        assert_eq!(r0, legacy_results(&t, "needle"));
        let full_rebuilds = |t: &Terminal| t.search_index_rebuilds() - t.search_index_refreshes();
        let full_before = full_rebuilds(&t);

        for batch in 0..5 {
            for i in 0..8 {
                t.process(format!("batch {batch} line {i} needle\r\n").as_bytes());
            }
            let refreshes_before = t.search_index_refreshes();
            let got = cached_results(&mut t, "needle");
            assert_eq!(
                t.search_index_refreshes(),
                refreshes_before + 1,
                "a streaming-output miss must be served by the churn refresh"
            );
            assert_eq!(
                got,
                legacy_results(&t, "needle"),
                "refreshed results must equal a from-scratch rebuild"
            );
        }
        assert_eq!(
            full_rebuilds(&t),
            full_before,
            "no full O(total) rebuild may hide inside the churn loop"
        );
    }

    /// Ring-cap eviction while streaming (a capped session's steady state) stays on the
    /// refresh path and matches the oracle's matches and completeness.
    #[test]
    fn ring_eviction_steady_state_refreshes_and_equals_legacy() {
        let mut t = crate::terminal::TerminalBuilder::new()
            .size(6, 40)
            .ring_buffer_size(30)
            .build();
        for i in 0..60 {
            t.process(format!("fill {i} needle\r\n").as_bytes());
        }
        let _ = cached_results(&mut t, "needle");
        // Enough single-row advances to force several amortized compactions, with Unicode
        // columns and overlapping case-folded postings.
        for batch in 0..160 {
            let oldest_before = t.grid().oldest_absolute_row();
            t.process(format!("more {batch} NEEDLE 日本語 needle\r\n").as_bytes());
            assert!(
                t.grid().oldest_absolute_row() > oldest_before,
                "precondition: the full ring must be evicting between searches"
            );
            let refreshes_before = t.search_index_refreshes();
            let got = cached_results(&mut t, "needle");
            assert_eq!(
                t.search_index_refreshes(),
                refreshes_before + 1,
                "ring-eviction churn must stay on the refresh path"
            );
            assert_eq!(got, legacy_results(&t, "needle"));
            for query in ["NEEDLE", "日本語", "more"] {
                assert_eq!(cached_results(&mut t, query), legacy_results(&t, query));
            }
            let res = t
                .indexed_search()
                .search_results_opts("needle", false, false)
                .expect("search ok");
            assert!(
                !res.incomplete,
                "grid-retention drops are fresh-build-complete, never sticky-incomplete"
            );
        }
    }

    /// A width change (history rewrapped) and a Kitty CSI +T unscroll (history renumbered)
    /// both fall back to the full rebuild and advance the renumber epoch. NEGATIVE CONTROL:
    /// a height change at the same width takes the refresh arm.
    #[test]
    fn width_change_and_unscroll_rebuild_instead_of_refreshing() {
        let mut t = Terminal::new(6, 40);
        for i in 0..40 {
            t.process(format!("wrapline {i} needle\r\n").as_bytes());
        }
        let _ = cached_results(&mut t, "needle");
        let refreshes = t.search_index_refreshes();
        t.resize(8, 40);
        let got = cached_results(&mut t, "needle");
        assert_eq!(
            t.search_index_refreshes(),
            refreshes + 1,
            "a height change at the same width refreshes"
        );
        assert_eq!(got, legacy_results(&t, "needle"));
        let _ = cached_results(&mut t, "needle");
        let refreshes = t.search_index_refreshes();
        let epoch = t.grid().history_renumber_epoch();
        t.resize(8, 33);
        // The `cols` fence alone would pass the refresh assertion below; without the
        // width-reflow epoch bump a later width change restoring `cols` would refresh over
        // renumbered keys.
        assert!(
            t.grid().history_renumber_epoch() > epoch,
            "a width rewrap renumbers history and must advance the epoch"
        );
        let got = cached_results(&mut t, "needle");
        assert_eq!(
            t.search_index_refreshes(),
            refreshes,
            "a width change must NOT take the refresh arm (history rewrapped)"
        );
        assert_eq!(got, legacy_results(&t, "needle"));

        // Unscroll arm: a tiered store, since a ring-only grid routes unscroll to a plain
        // region scroll.
        let sb = aterm_scrollback::Scrollback::new(8, 64, 8_000_000);
        let mut t = Terminal::with_scrollback(6, 40, 8, sb);
        for i in 0..80 {
            t.process(format!("uline {i} needle\r\n").as_bytes());
        }
        let _ = cached_results(&mut t, "needle");
        let epoch = t.grid().history_renumber_epoch();
        let removed = t.grid_mut().unscroll_from_scrollback(3);
        assert!(
            removed > 0,
            "precondition: unscroll must remove history lines"
        );
        assert!(
            t.grid().history_renumber_epoch() > epoch,
            "unscroll must advance the renumber epoch (the rebuild fence)"
        );
        let refreshes = t.search_index_refreshes();
        let got = cached_results(&mut t, "needle");
        assert_eq!(
            t.search_index_refreshes(),
            refreshes,
            "a history renumbering must force a full rebuild"
        );
        assert_eq!(got, legacy_results(&t, "needle"));
    }

    /// A request for the logical layout (soft-wrap runs joined) at `depth_cap`.
    fn logical(depth_cap: usize) -> crate::terminal::SearchRequest {
        crate::terminal::SearchRequest {
            depth_cap,
            layout: crate::terminal::SearchLayout::Logical,
            anchor: None,
        }
    }

    /// Copy the rest of a plan's rows, a few per hold, as a GUI host copies them.
    fn copy_rows(t: &Terminal, plan: &mut crate::terminal::SearchPlan) {
        while plan.rows_to_read() > 0 {
            t.read_search_chunk(plan, 3);
        }
    }

    /// One case-insensitive forward query over a built index, as `(row, col, len)` hits.
    fn hits_of(built: &mut crate::terminal::BuiltSearch, pat: &str) -> Vec<(usize, usize, usize)> {
        let (results, _) = built
            .query(
                pat,
                false,
                false,
                crate::search::SearchDirection::Forward,
                false,
            )
            .expect("a literal query");
        results
            .matches
            .iter()
            .map(|m| (m.line, m.start_col, m.len()))
            .collect()
    }

    /// One logical-layout search through the whole protocol: plan, chunked copy, build,
    /// query, finish.
    fn logical_search(
        t: &mut Terminal,
        depth_cap: usize,
        pat: &str,
    ) -> (Vec<(usize, usize, usize)>, crate::terminal::SearchVerdict) {
        let mut plan = t.search_plan(&logical(depth_cap));
        copy_rows(t, &mut plan);
        let mut built = plan.build();
        let hits = hits_of(&mut built, pat);
        (hits, t.finish_search(built).0)
    }

    /// The same search over an index built afresh (the slot released first).
    fn fresh_logical(t: &mut Terminal, depth_cap: usize, pat: &str) -> Vec<(usize, usize, usize)> {
        t.release_search_index();
        logical_search(t, depth_cap, pat).0
    }

    /// The spans a build plan copies, named as its first chunk names them, no row copied.
    fn spans_of(
        t: &Terminal,
        plan: &mut crate::terminal::SearchPlan,
    ) -> Vec<std::ops::Range<usize>> {
        t.name_search_copy(plan);
        match &plan.kind {
            super::PlanKind::Build(copy) => copy.spans.clone(),
            super::PlanKind::Hit { .. } => Vec::new(),
        }
    }

    fn needles(t: &mut Terminal, n: usize) {
        for i in 0..n {
            t.process(format!("needle {i}\r\n").as_bytes());
        }
    }

    /// Output between the plan and the copy tears the copy, a cold build's or a refresh's
    /// (whose first chunk then names its rows off the moved terminal): the search is still
    /// answered from the rows it read, the verdict says it is not the terminal's, and the
    /// index is not kept, so the next plan builds again. NEGATIVE CONTROL: the same search
    /// with no output is consistent and leaves the slot answering the next plan.
    #[test]
    fn a_build_torn_by_output_is_answered_but_not_installed() {
        for (refresh, torn) in [(false, false), (false, true), (true, false), (true, true)] {
            let mut t = Terminal::new(4, 20);
            needles(&mut t, 10);
            if refresh {
                let _ = logical_search(&mut t, 1_000, "needle");
                t.process(b"needle 10\r\n");
            }
            let refreshes = t.search_index_refreshes();
            let mut plan = t.search_plan(&logical(1_000));
            assert!(
                !plan.is_hit(),
                "refresh={refresh} torn={torn}: precondition"
            );
            if torn {
                t.process(b"needle late\r\n");
            }
            copy_rows(&t, &mut plan);
            let mut built = plan.build();
            assert!(
                hits_of(&mut built, "needle").len() >= 10,
                "refresh={refresh} torn={torn}"
            );
            assert_eq!(
                t.finish_search(built).0,
                crate::terminal::SearchVerdict {
                    consistent: !torn,
                    frame_stable: true,
                },
                "refresh={refresh} torn={torn}: output moves the generation, not the frame"
            );
            assert_eq!(
                t.search_index_refreshes() - refreshes,
                u64::from(refresh),
                "refresh={refresh} torn={torn}: precondition"
            );
            assert_eq!(
                t.search_plan(&logical(1_000)).is_hit(),
                !torn,
                "refresh={refresh} torn={torn}"
            );
        }
    }

    /// A hit's query runs off the lock; a mutation between the query and the finish is
    /// reported, never hidden: output makes it inconsistent with the frame standing; a
    /// screen switch, a width or ambiguous-width change and a history renumbering move
    /// the frame too. The ambiguous-width switch and the Kitty `CSI +T` unscroll move no
    /// `content_seq`, so only the key's other fields see them. NEGATIVE CONTROL: no
    /// mutation is consistent.
    #[test]
    fn a_hit_query_raced_by_output_reports_inconsistent() {
        let verdict = |consistent, frame_stable| crate::terminal::SearchVerdict {
            consistent,
            frame_stable,
        };
        let races: [(
            &str,
            Option<fn(&mut Terminal)>,
            crate::terminal::SearchVerdict,
        ); 6] = [
            ("none", None, verdict(true, true)),
            (
                "output",
                Some(|t| {
                    t.process(b"more\r\n");
                }),
                verdict(false, true),
            ),
            (
                "alt switch",
                Some(|t| {
                    t.process(b"\x1b[?1049h");
                }),
                verdict(false, false),
            ),
            (
                "ambiguous width",
                Some(|t| {
                    t.apply_config(&crate::config::TerminalConfig {
                        ambiguous_width_double: true,
                        ..Default::default()
                    });
                }),
                verdict(false, false),
            ),
            ("width", Some(|t| t.resize(4, 30)), verdict(false, false)),
            (
                "unscroll",
                Some(|t| {
                    t.process(b"\x1b[2+T");
                }),
                verdict(false, false),
            ),
        ];
        for (label, race, want) in races {
            // A tiered store under the ring: a ring-only grid routes an unscroll to a
            // plain region scroll.
            let mut t = Terminal::with_scrollback(
                4,
                40,
                8,
                aterm_scrollback::Scrollback::new(64, 512, 8_000_000),
            );
            needles(&mut t, 40);
            let _ = logical_search(&mut t, 1_000, "needle");
            let plan = t.search_plan(&logical(1_000));
            assert!(plan.is_hit(), "{label}: precondition");
            let mut built = plan.build();
            assert_eq!(hits_of(&mut built, "needle").len(), 40, "{label}");
            if let Some(race) = race {
                race(&mut t);
            }
            assert_eq!(t.finish_search(built).0, want, "{label}");
        }
    }

    /// A soft-wrap run is indexed as one line keyed at the row it starts on, so a refresh
    /// whose boundary lands inside a run re-reads from the run's head: the head is
    /// re-joined with what the boundary row holds now. NEGATIVE CONTROL: the same refresh
    /// copied from the boundary row leaves the head holding the rows it lost.
    #[test]
    fn refresh_rejoins_the_run_its_boundary_lands_in() {
        let setup = || {
            let mut t = Terminal::new(4, 10);
            // "0123456789" scrolls into history, "ABCDEFGHIJ" stays the top visible row,
            // a continuation of it.
            t.process(b"0123456789ABCDEFGHIJ\r\naa\r\nbb\r\n");
            assert_eq!(
                logical_search(&mut t, 1_000, "9A").0.len(),
                1,
                "the run is whole"
            );
            // Rewrite the top visible row out of the run, then scroll it into history.
            t.process(b"\x1b[H\x1b[2Kzz\x1b[4;1H\r\n");
            t
        };
        let mut t = setup();
        let refreshes = t.search_index_refreshes();
        let mut plan = t.search_plan(&logical(1_000));
        assert_eq!(
            spans_of(&t, &mut plan).first().map(|span| span.start),
            Some(0),
            "from the head"
        );
        copy_rows(&t, &mut plan);
        let mut built = plan.build();
        assert!(
            hits_of(&mut built, "9A").is_empty(),
            "the head is re-joined"
        );
        assert_eq!(
            hits_of(&mut built, "0123456789").len(),
            1,
            "with its own row's text"
        );
        let _ = t.finish_search(built);
        assert_eq!(
            t.search_index_refreshes(),
            refreshes + 1,
            "precondition: a refresh"
        );

        let mut t = setup();
        let mut plan = t.search_plan(&logical(1_000));
        let _ = spans_of(&t, &mut plan);
        if let super::PlanKind::Build(copy) = &mut plan.kind {
            copy.spans[0].start = 1;
        }
        copy_rows(&t, &mut plan);
        assert_eq!(hits_of(&mut plan.build(), "9A").len(), 1, "a stale head");
    }

    /// When the window's floor advances into a soft-wrap run, the run's head leaves the
    /// window with the rows below the floor; a fresh build starts a line at the floor with
    /// the run's remaining rows, and so does the refresh. NEGATIVE CONTROL: the refresh
    /// without that span leaves those rows as empty continuations of a head it dropped.
    #[test]
    fn a_refresh_rereads_the_run_its_window_floor_lands_in() {
        // A soft-wrap run on rows 1 to 3 of a 4x10 screen, short lines after it.
        let output = || {
            let mut t = Terminal::new(4, 10);
            t.process(b"s0\r\n0123456789ABCDEFGHIJKLMNOPQRST\r\n");
            for i in 0..5 {
                t.process(format!("t{i}\r\n").as_bytes());
            }
            t
        };
        // A depth window whose floor is the run's second row, then one line more, so the
        // floor advances to its third.
        let depth = {
            let t = output();
            t.grid().scrollback_lines() + usize::from(t.rows()) - 2
        };
        let setup = || {
            let mut t = output();
            // The window's line at row 2 holds rows 2 and 3: a hit straddles them.
            assert_eq!(
                logical_search(&mut t, depth, "JK").0,
                vec![(2, 9, 2)],
                "floor at row 2"
            );
            t.process(b"t5\r\n");
            t
        };
        let mut t = setup();
        assert_eq!(
            t.grid().oldest_absolute_row(),
            0,
            "precondition: nothing evicted"
        );
        let refreshes = t.search_index_refreshes();
        let mut plan = t.search_plan(&logical(depth));
        assert_eq!(
            plan.key().window().retained_base,
            3,
            "the floor is inside the run"
        );
        let spans = spans_of(&t, &mut plan);
        assert_eq!(spans.len(), 2, "{spans:?}");
        assert!(
            spans[0].start == 3 && spans[0].end < spans[1].start,
            "{spans:?}"
        );
        copy_rows(&t, &mut plan);
        let mut built = plan.build();
        let refreshed = hits_of(&mut built, "KLMNO");
        assert!(
            hits_of(&mut built, "JK").is_empty(),
            "row 2 left the window"
        );
        let _ = t.finish_search(built);
        assert_eq!(
            t.search_index_refreshes(),
            refreshes + 1,
            "precondition: a refresh"
        );
        assert_eq!(refreshed, vec![(3, 0, 5)]);
        assert_eq!(refreshed, fresh_logical(&mut t, depth, "KLMNO"));

        let mut t = setup();
        let mut plan = t.search_plan(&logical(depth));
        let _ = spans_of(&t, &mut plan);
        if let super::PlanKind::Build(copy) = &mut plan.kind {
            copy.spans.remove(0);
        }
        copy_rows(&t, &mut plan);
        assert!(
            hits_of(&mut plan.build(), "KLMNO").is_empty(),
            "the orphaned rows"
        );
    }

    /// The narrowing frames read off an index are kept beside it and die with it: a hit on
    /// a new generation's index starts with none. NEGATIVE CONTROL: on an unchanged key the
    /// next keystroke narrows off the frame the last one left.
    #[test]
    fn narrow_stack_dies_with_the_index() {
        let mut t = Terminal::new(4, 40);
        for i in 0..30 {
            t.process(format!("find {i}\r\n").as_bytes());
        }
        let served_off_a_frame = |t: &mut Terminal, pat: &str| {
            let _ = crate::terminal::take_narrowed_query_steps();
            let (hits, verdict) = logical_search(t, 1_000, pat);
            assert!(verdict.consistent && !hits.is_empty(), "{pat}");
            crate::terminal::take_narrowed_query_steps() == 1
        };
        assert!(
            !served_off_a_frame(&mut t, "fi"),
            "the build seeds the first frame"
        );
        assert!(
            served_off_a_frame(&mut t, "fin"),
            "an unchanged key keeps it"
        );
        t.process(b"find more\r\n");
        assert!(
            !served_off_a_frame(&mut t, "find"),
            "the refreshed index has no frames"
        );
        assert!(
            served_off_a_frame(&mut t, "find "),
            "and keeps the ones its query left"
        );
    }

    /// A query still reading the slot's index does not turn the next refresh into a
    /// rebuild of the window: the refresh copies the index it shares and brings the copy
    /// up to date, leaving the reader's index as it was. NEGATIVE CONTROL: a cold plan over
    /// the same terminal reads the whole window.
    #[test]
    fn a_refresh_racing_a_query_on_the_old_index_copies_it_and_leaves_it_whole() {
        let mut t = Terminal::new(4, 40);
        needles(&mut t, 30);
        let _ = logical_search(&mut t, 1_000, "needle");
        let reader = t.search_plan(&logical(1_000));
        assert!(reader.is_hit(), "precondition");
        let mut reader = reader.build();
        t.process(b"needle late\r\n");

        let mut refresh = t.search_plan(&logical(1_000));
        assert!(!refresh.is_hit());
        assert_eq!(
            refresh.rows_to_read(),
            refresh.key().window().retained_total,
            "the plan's hold names no refresh rows: the window bounds them"
        );
        let named: usize = spans_of(&t, &mut refresh)
            .iter()
            .map(ExactSizeIterator::len)
            .sum();
        assert!(named <= 4 + 2, "a refresh, not the window: {named}");
        assert_eq!(refresh.rows_to_read(), named);
        copy_rows(&t, &mut refresh);
        let mut built = refresh.build();
        let refreshed = hits_of(&mut built, "needle");
        assert!(t.finish_search(built).0.consistent);
        assert_eq!(refreshed.len(), 31);

        assert_eq!(
            hits_of(&mut reader, "needle").len(),
            30,
            "the reader's index is unchanged"
        );
        assert!(!t.finish_search(reader).0.consistent);

        assert_eq!(refreshed, fresh_logical(&mut t, 1_000, "needle"));
        t.release_search_index();
        let cold = t.search_plan(&logical(1_000));
        let window = cold.key().window().retained_total;
        assert_eq!(
            cold.rows_to_read(),
            window,
            "a cold plan reads all {window} rows"
        );
    }

    /// No hold frees an index, since freeing one is O(window): a plan's hold leaves what it
    /// displaced in the plan, which `build` frees, and `finish_search` and
    /// `take_search_index` hand what they let go of back in a discard. The cases: a plan over
    /// another frame, a torn build the slot does not keep, a kept build displacing the
    /// slot's, a hit whose index a refresh displaced meanwhile, and a release. NEGATIVE
    /// CONTROL: the index dies when the build runs or the discard drops, so each held the
    /// last reference.
    #[test]
    fn no_hold_frees_an_index() {
        use std::sync::{Arc, Weak};

        use crate::search::TerminalSearch;
        let slot_index = |t: &Terminal| -> Weak<TerminalSearch> {
            Arc::downgrade(&t.search_slot.as_ref().expect("a kept index").index)
        };
        let warm = || {
            let mut t = Terminal::new(4, 40);
            needles(&mut t, 30);
            let _ = logical_search(&mut t, 1_000, "needle");
            t
        };

        let mut t = warm();
        let displaced = slot_index(&t);
        t.resize(4, 30);
        let mut plan = t.search_plan(&logical(1_000));
        assert!(
            t.search_slot.is_none(),
            "another frame: the slot is emptied"
        );
        assert!(displaced.upgrade().is_some(), "the plan's hold freed it");
        copy_rows(&t, &mut plan);
        let built = plan.build();
        assert!(displaced.upgrade().is_none(), "the build frees it");
        let _ = t.finish_search(built);

        let mut t = Terminal::new(4, 40);
        needles(&mut t, 30);
        let mut plan = t.search_plan(&logical(1_000));
        t.process(b"needle late\r\n");
        copy_rows(&t, &mut plan);
        let built = plan.build();
        let torn = Arc::downgrade(&built.index);
        let (verdict, discard) = t.finish_search(built);
        assert!(
            !verdict.consistent && t.search_slot.is_none(),
            "precondition"
        );
        assert!(torn.upgrade().is_some(), "the finish freed a torn build");
        drop(discard);
        assert!(torn.upgrade().is_none());

        let mut t = Terminal::new(4, 40);
        needles(&mut t, 30);
        let mut first = t.search_plan(&logical(1_000));
        let mut second = t.search_plan(&logical(1_000));
        copy_rows(&t, &mut first);
        copy_rows(&t, &mut second);
        let (first, second) = (first.build(), second.build());
        let kept = Arc::downgrade(&first.index);
        let _ = t.finish_search(first);
        let (verdict, discard) = t.finish_search(second);
        assert!(verdict.consistent, "precondition");
        assert!(
            kept.upgrade().is_some(),
            "the finish freed the slot it displaced"
        );
        drop(discard);
        assert!(kept.upgrade().is_none());

        let mut t = warm();
        let reader = t.search_plan(&logical(1_000));
        assert!(reader.is_hit(), "precondition");
        let reader = reader.build();
        t.process(b"needle late\r\n");
        let mut refresh = t.search_plan(&logical(1_000));
        copy_rows(&t, &mut refresh);
        let _ = t.finish_search(refresh.build());
        let shared = Arc::downgrade(&reader.index);
        let (_, discard) = t.finish_search(reader);
        assert!(
            shared.upgrade().is_some(),
            "the finish freed a hit's index a refresh displaced"
        );
        drop(discard);
        assert!(shared.upgrade().is_none());

        let mut t = warm();
        let resident = slot_index(&t);
        let discard = t.take_search_index();
        assert!(t.search_slot.is_none(), "taken");
        assert!(resident.upgrade().is_some(), "the take freed it");
        drop(discard);
        assert!(resident.upgrade().is_none());
    }
}
