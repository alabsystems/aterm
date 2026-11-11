// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Budgeted, resumable full-content search across scrollback and visible rows.

use super::search_index::ONE_SHOT_SEARCH;
use super::search_rows::history_row_for_search;
use super::{SearchKey, Terminal};
use crate::search::{BudgetedSearch, SearchOptionsError, SearchResults};
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_MATCHES_PER_STEP: usize = 4_096;

static NEXT_BUDGETED_SEARCH_CURSOR: AtomicU64 = AtomicU64::new(1);

#[allow(deprecated)] // Trust renamed this to try_update; stable x86 slice still exposes fetch_update.
#[inline]
fn allocate_cursor(counter: &AtomicU64) -> Option<u64> {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .ok()
}

/// Error returned while starting a budgeted search.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetedSearchError {
    /// The query/options were invalid.
    SearchOptions(SearchOptionsError),
    /// The non-repeating cursor space is exhausted: fail closed rather than wrap.
    CursorSpaceExhausted,
}

impl std::fmt::Display for BudgetedSearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SearchOptions(e) => {
                f.write_str("invalid search options: ")?;
                std::fmt::Display::fmt(e, f)
            }
            Self::CursorSpaceExhausted => f.write_str("budgeted-search cursor space exhausted"),
        }
    }
}

impl std::error::Error for BudgetedSearchError {}

impl From<SearchOptionsError> for BudgetedSearchError {
    #[inline]
    fn from(value: SearchOptionsError) -> Self {
        Self::SearchOptions(value)
    }
}

#[derive(PartialEq, Eq)]
enum ScanPhase {
    InFlight,
    Complete,
}

/// Budgeted search state in `Terminal::budgeted_search`: at most one per terminal, a new
/// search supersedes it.
pub(crate) struct BudgetedSearchState {
    cursor: u64,
    alt_screen: bool,
    content_gen: u64,
    query: String,
    case_sensitive: bool,
    is_regex: bool,
    ambiguous_width: aterm_types::text_shaping::AmbiguousWidth,
    scrollback: usize,
    engine: BudgetedSearch,
    emitted_matches: usize,
    phase: ScanPhase,
}

/// One slice of a budgeted search: the results so far plus resume state.
#[derive(Debug)]
pub struct BudgetedSearchStep {
    /// The new match delta plus incompleteness and watermark metadata.
    pub results: SearchResults,
    /// Every row has been verified and every match delta delivered.
    pub complete: bool,
    /// Token to resume with; `None` once complete.
    pub cursor: Option<u64>,
    /// Stable identity of this logical search, kept on the completing step too.
    pub search_id: u64,
    /// Whether this call started a fresh logical search.
    pub reset: bool,
    /// Rows consumed so far (monotone within one search; restarts reset it).
    pub rows_fed: usize,
    /// Total rows this search will consume.
    pub total_rows: usize,
}

impl Terminal {
    /// Run at most `row_budget` rows (at least 1) of a full-content search, resuming from
    /// `resume_cursor` while it is valid.
    ///
    /// # Errors
    /// Pattern errors from the engine (invalid or oversized regex, regex feature disabled),
    /// and an exhausted cursor space. No state is retained after an error.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "budgeted_search_resume",
            action = "Start",
            project = "conformance_budgeted_search::project_step"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "budgeted_search_resume",
            action = "BuggyScanOnlyCompletion",
            reason = "Buggy=1 negative control only: a step is complete only once every match \
                      delta is delivered (`&& emitted_matches >= result_count`), so the dense \
                      scan turn never reports complete with its backlog held; the Tier-1 replays \
                      the slipped step and the healthy model rejects it"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "budgeted_search_resume",
            action = "StartComplete",
            project = "conformance_budgeted_search::project_step"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "budgeted_search_resume",
            action = "StartBacklog",
            project = "conformance_budgeted_search::project_step"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "budgeted_search_resume",
            action = "Resume",
            project = "conformance_budgeted_search::project_step"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "budgeted_search_resume",
            action = "FinishScan",
            project = "conformance_budgeted_search::project_step"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "budgeted_search_resume",
            action = "Supersede",
            project = "conformance_budgeted_search::project_step"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "budgeted_search_resume",
            action = "ContentRestart",
            project = "conformance_budgeted_search::project_step"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "budgeted_search_resume",
            action = "QueryRestart",
            project = "conformance_budgeted_search::project_step"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "budgeted_search_resume",
            action = "ForgedRestart",
            project = "conformance_budgeted_search::project_step"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "budgeted_search_resume",
            action = "RestartComplete",
            project = "conformance_budgeted_search::project_step"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "budgeted_search_resume",
            action = "Drain",
            project = "conformance_budgeted_search::project_step"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "budgeted_search_resume",
            action = "DrainComplete",
            project = "conformance_budgeted_search::project_step"
        )
    )]
    pub fn search_budgeted(
        &mut self,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
        resume_cursor: Option<u64>,
        row_budget: usize,
    ) -> Result<BudgetedSearchStep, BudgetedSearchError> {
        let key_alt = self.modes.alternate_screen;
        let key_gen = self.content_seq();

        let resumable = match (resume_cursor, &self.budgeted_search) {
            (Some(cursor), Some(state)) => {
                state.phase == ScanPhase::InFlight
                    && state.cursor == cursor
                    && state.alt_screen == key_alt
                    && state.content_gen == key_gen
                    && state.query == query
                    && state.case_sensitive == case_sensitive
                    && state.is_regex == is_regex
                    && state.ambiguous_width == self.ambiguous_width()
            }
            _ => false,
        };

        let reset = !resumable;
        if reset {
            self.budgeted_search = None;
            let grid = &self.grid;
            let oldest = usize::try_from(grid.oldest_absolute_row()).unwrap_or(usize::MAX);
            let scrollback = grid.scrollback_lines();
            let total_rows = scrollback.saturating_add(usize::from(self.rows()));
            let window = SearchKey::capture(self, ONE_SHOT_SEARCH.depth_cap).max_lines;
            let engine = BudgetedSearch::with_max_cached_lines(
                query,
                case_sensitive,
                is_regex,
                oldest,
                total_rows,
                window,
            )?
            .with_ambiguous_width(self.ambiguous_width());
            let cursor = allocate_cursor(&NEXT_BUDGETED_SEARCH_CURSOR)
                .ok_or(BudgetedSearchError::CursorSpaceExhausted)?;
            self.budgeted_search = Some(BudgetedSearchState {
                cursor,
                alt_screen: key_alt,
                content_gen: key_gen,
                query: query.to_string(),
                case_sensitive,
                is_regex,
                ambiguous_width: self.ambiguous_width(),
                scrollback,
                engine,
                emitted_matches: 0,
                phase: ScanPhase::InFlight,
            });
        }

        let mut state = self
            .budgeted_search
            .take()
            .expect("budgeted_search populated above");

        let backlog = state
            .engine
            .result_count()
            .saturating_sub(state.emitted_matches);
        let end = if backlog >= MAX_MATCHES_PER_STEP {
            state.engine.rows_fed()
        } else {
            state
                .engine
                .rows_fed()
                .saturating_add(row_budget.max(1))
                .min(state.engine.total_rows())
        };
        self.feed_budgeted_history(&mut state, end);
        while state.engine.rows_fed() < end {
            let i = state.engine.rows_fed();
            let (text, cells) = i
                .checked_sub(state.scrollback)
                .and_then(|visible_row| u16::try_from(visible_row).ok())
                .and_then(|r| self.written_line_cells(i32::from(r)))
                .unwrap_or_default();
            state.engine.feed_row_cells(text, &cells);
        }

        let results = state
            .engine
            .results_range(state.emitted_matches, MAX_MATCHES_PER_STEP);
        state.emitted_matches = state.emitted_matches.saturating_add(results.matches.len());
        let complete =
            state.engine.is_complete() && state.emitted_matches >= state.engine.result_count();
        let step = BudgetedSearchStep {
            results,
            complete,
            cursor: (!complete).then_some(state.cursor),
            search_id: state.cursor,
            reset,
            rows_fed: state.engine.rows_fed(),
            total_rows: state.engine.total_rows(),
        };
        state.phase = if complete {
            ScanPhase::Complete
        } else {
            ScanPhase::InFlight
        };
        self.budgeted_search = Some(state);
        Ok(step)
    }

    fn feed_budgeted_history(&self, state: &mut BudgetedSearchState, end: usize) {
        let hist_start = state.engine.rows_fed();
        let hist_end = end.min(state.scrollback);
        if hist_start >= hist_end {
            return;
        }
        let lines = self
            .grid
            .history_lines_from(hist_start)
            .chain(std::iter::repeat_with(|| None))
            .take(hist_end - hist_start);
        for line in lines {
            let (text, cells) = history_row_for_search(&self.grid, line);
            state.engine.feed_row_cells(text, &cells);
        }
    }

    /// Drop any budgeted search (its matches and scratch; an outstanding cursor restarts).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "budgeted_search_resume",
            action = "Cancel",
            project = "conformance_budgeted_search::project_cancel"
        )
    )]
    #[inline]
    pub fn cancel_budgeted_search(&mut self) {
        self.budgeted_search = None;
    }

    /// Results for a federated `search_summary`, reading a matching completed budgeted search
    /// directly or falling back to [`indexed_search`](Self::indexed_search).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            reason = "BudgetedSearchResume models the search_budgeted STEP lifecycle; \
                      search_summary_results is a pure read over the retained completed \
                      results (equal to the modeled completed set by the \
                      budgeted/one-shot equivalence oracle), not a lifecycle transition"
        )
    )]
    pub fn search_summary_results(
        &mut self,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
    ) -> Result<SearchResults, SearchOptionsError> {
        let key_alt = self.modes.alternate_screen;
        let key_gen = self.content_seq();
        if let Some(state) = &self.budgeted_search {
            if state.phase == ScanPhase::Complete
                && state.alt_screen == key_alt
                && state.content_gen == key_gen
                && state.query == query
                && state.case_sensitive == case_sensitive
                && state.is_regex == is_regex
                && state.ambiguous_width == self.ambiguous_width()
            {
                return Ok(state.engine.results());
            }
        }
        self.indexed_search()
            .search_results_opts(query, case_sensitive, is_regex)
    }
}

#[cfg(test)]
#[path = "search_budgeted_tests.rs"]
mod tests;
