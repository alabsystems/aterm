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
/// its own ([`SearchPlan::build`]).
#[must_use = "dropping it frees what it holds: drop it once the terminal's lock is released"]
#[derive(Default)]
/// Search state removed from the terminal for deferred drop outside the lock.
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
    /// to `key` provably equals building the window afresh.
    #[inline]
    fn try_refresh_search_index(
        &mut self,
        key: &SearchKey,
        layout: SearchLayout,
    ) -> Option<SearchSlot> {
        self.search_slot
            .take_if(|slot| slot.layout == layout && key.refreshable_from(&slot.key))
    }

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

    /// Copy up to `max_rows` of `plan`'s next rows in one hold through [`Self::read_search_rows`].
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

    fn name_search_copy(&self, plan: &mut SearchPlan) {
        if let PlanKind::Build(copy) = &mut plan.kind
            && let Some(previous) = copy.pending.take()
        {
            copy.name_spans(self.search_copy_spans(&plan.key, plan.request.layout, &previous));
        }
    }

    /// Finish a search in one hold and return `(SearchVerdict, SearchDiscard)`.
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
    /// search_index_refreshes()` is the number of full rebuilds.
    #[must_use]
    #[inline]
    /// Incremental refreshes among search-index cache misses.
    pub const fn search_index_refreshes(&self) -> u64 {
        self.search_index_refreshes
    }

    /// Take the search index's heap out of the terminal, unfreed: the slot and any budgeted
    /// search (in flight or retained for `search_summary`).
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
#[path = "search_index_tests.rs"]
pub(crate) mod tests;
