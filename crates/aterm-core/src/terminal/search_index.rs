// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Full-history terminal search index and lock-free query protocol.

use std::ops::Range;
use std::sync::Arc;

use super::search_budgeted::BudgetedSearchState;
use super::search_plan::{BuiltSearch, Made, PlanKind, RowCopy};
use super::{
    NarrowStack, SearchKey, SearchLayout, SearchPlan, SearchRequest, SearchRows, SearchVerdict,
    Terminal, WrapFrame, join_wrapped_rows,
};
use crate::search::{DEFAULT_MAX_CACHED_LINES, TerminalSearch};

/// Default [`SearchRequest`] for [`Terminal::indexed_search`].
pub(crate) const ONE_SHOT_SEARCH: SearchRequest = SearchRequest {
    depth_cap: DEFAULT_MAX_CACHED_LINES,
    layout: SearchLayout::Physical,
    anchor: None,
};

/// A terminal's cached search index and the [`SearchKey`] it was built for.
pub(crate) struct SearchSlot {
    /// The key the index was built under: the hit test ([`SearchKey::same_generation`]),
    key: SearchKey,
    /// How the index lays out soft wraps; a request for the other layout misses.
    layout: SearchLayout,
    /// Shared with the queries reading it off the lock; never changed while shared.
    index: Arc<TerminalSearch>,
    /// The narrowing frames read off `index`, lent to one hit at a time.
    narrow: Option<NarrowStack>,
}

/// Search state a hold took out of the terminal and left its caller to free
#[must_use = "dropping it frees what it holds: drop it once the terminal's lock is released"]
#[derive(Default)]
/// Search state removed from the terminal for the caller to drop outside the lock ([`Terminal::finish_search`]).
pub struct SearchDiscard {
    /// A slot the terminal no longer holds: displaced by a plan or a kept build, or taken.
    slot: Option<SearchSlot>,
    /// A search's index the slot does not hold: a build it did not keep, or a hit's share of
    index: Option<Arc<TerminalSearch>>,
    /// Narrowing frames that lost their place: a refreshed index's, a build's the slot did
    narrow: Option<NarrowStack>,
    /// A budgeted search taken with the slot.
    #[expect(dead_code, reason = "held only to be freed when the discard drops")]
    budgeted: Option<BudgetedSearchState>,
}

impl Terminal {
    /// Plan one search, in one hold: capture its key ([`SearchKey::capture`]), carry the
    #[must_use]
    /// Plan a search in one hold, capturing its [`SearchKey`] and reusing or refreshing the slot.
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

    /// Copy up to `max_rows` of `plan`'s next rows in one hold through [`SearchPlan::copy_chunk`].
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
    fn name_search_copy(&self, plan: &mut SearchPlan) {
        if let PlanKind::Build(copy) = &mut plan.kind
            && let Some(previous) = copy.pending.take()
        {
            copy.name_spans(self.search_copy_spans(&plan.key, plan.request.layout, &previous));
        }
    }

    /// Finish a search in one hold, updating the slot and returning `(SearchVerdict, SearchDiscard)`.
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

    /// Return a full-content [`TerminalSearch`] index, reusing the cached slot when unchanged.
    pub fn indexed_search(&mut self) -> &TerminalSearch {
        let mut plan = self.search_plan(&ONE_SHOT_SEARCH);
        self.read_search_chunk(&mut plan, usize::MAX);
        let _ = self.finish_search(plan.build());
        &self
            .search_slot
            .as_ref()
            .expect("a finished search under &mut leaves its index in the slot")
            .index
    }

    /// Search-index cache misses so far (rebuilds and refreshes), never a reuse.
    #[must_use]
    #[inline]
    /// Search-index cache misses so far (rebuilds and refreshes).
    pub const fn search_index_rebuilds(&self) -> u64 {
        self.search_index_rebuilds
    }

    /// Incremental refreshes among those misses; `search_index_rebuilds() -
    #[must_use]
    #[inline]
    /// Incremental refreshes among search-index cache misses.
    pub const fn search_index_refreshes(&self) -> u64 {
        self.search_index_refreshes
    }

    /// Take the search index's heap out of the terminal, unfreed: the slot and any budgeted
    #[inline]
    /// Take the search index and any budgeted search state out of the terminal for deferred drop.
    pub fn take_search_index(&mut self) -> SearchDiscard {
        SearchDiscard {
            slot: self.search_slot.take(),
            budgeted: self.budgeted_search.take(),
            ..SearchDiscard::default()
        }
    }

    /// [`Self::take_search_index`], freed in this call.
    #[inline]
    /// Drop [`Self::take_search_index`] immediately.
    pub fn release_search_index(&mut self) {
        drop(self.take_search_index());
    }
}

/// Build a [`BuiltSearch`] from a [`SearchPlan`]'s copied rows without holding the terminal lock.
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

    /// Uncached per-line search oracle for tests.
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

    /// Build a terminal with a corrupt warm block between cold and hot needles for tests.
    pub(crate) fn corrupt_tiered_terminal() -> (Terminal, usize) {
        use aterm_grid::Grid;
        use aterm_scrollback::Scrollback;
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
    #[test]
    fn search_over_a_corrupt_tiered_segment_keeps_absolute_rows() {
        let (mut t, corrupt) = corrupt_tiered_terminal();
        let got = cached_results(&mut t, "NEEDLE_");
        assert_eq!(got, legacy_results(&t, "NEEDLE_"));
        assert_eq!(got.len(), 2, "both needles are found: {got:?}");
        let oldest = usize::try_from(t.grid().oldest_absolute_row()).expect("row");
        assert_eq!(got[0].0, oldest + 2);
        assert_eq!(got[1].0, oldest + 38 + corrupt);
    }

    /// A history line longer than the read bound (a crafted checkpoint can store one) is
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
    #[test]
    fn find_reports_matches_in_the_cells_the_grid_drew_them_in() {
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
            (
                "skin tone brought to its base by DCH",
                "\u{1f44d}q\u{1f3fd}XYZ\x1b[1;3H\x1b[P",
            ),
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
    #[test]
    fn retention_shrink_invalidates_the_cached_index() {
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
        let sb = aterm_scrollback::Scrollback::new(8, 16, 64_000_000);
        let mut t = Terminal::with_scrollback(6, 40, 8, sb);
        for i in 0..400 {
            t.process(format!("needle-{i}\r\n").as_bytes());
        }
        let before = cached_results(&mut t, "needle-");
        assert_eq!(before.len(), 400, "all lines retained + found pre-shrink");
        let lines_before = t.grid().scrollback_lines();

        let _ = t.set_memory_budget(4096);
        assert!(
            t.grid().scrollback_lines() < lines_before,
            "a tight budget must really evict retained lines \
             ({} -> {})",
            lines_before,
            t.grid().scrollback_lines()
        );

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
                0..=6 => {
                    for _ in 0..=(r >> 8) % 14 {
                        t.process(format!("needle {written} alpha row\r\n").as_bytes());
                        written += 1;
                    }
                }
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
                9 => {
                    if r % 2 == 0 {
                        let cols = t.cols();
                        t.resize(4 + u16::try_from(r % 8).unwrap_or(0), cols);
                    } else {
                        let rows = t.rows();
                        t.resize(rows, 24 + u16::try_from(r % 24).unwrap_or(0));
                    }
                }
                10 => {
                    if on_alt {
                        t.process(b"\x1b[?1049l");
                    } else {
                        t.process(b"\x1b[?1049h");
                    }
                    on_alt = !on_alt;
                }
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
    #[test]
    fn refresh_rejoins_the_run_its_boundary_lands_in() {
        let setup = || {
            let mut t = Terminal::new(4, 10);
            t.process(b"0123456789ABCDEFGHIJ\r\naa\r\nbb\r\n");
            assert_eq!(
                logical_search(&mut t, 1_000, "9A").0.len(),
                1,
                "the run is whole"
            );
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
    #[test]
    fn a_refresh_rereads_the_run_its_window_floor_lands_in() {
        let output = || {
            let mut t = Terminal::new(4, 10);
            t.process(b"s0\r\n0123456789ABCDEFGHIJKLMNOPQRST\r\n");
            for i in 0..5 {
                t.process(format!("t{i}\r\n").as_bytes());
            }
            t
        };
        let depth = {
            let t = output();
            t.grid().scrollback_lines() + usize::from(t.rows()) - 2
        };
        let setup = || {
            let mut t = output();
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
