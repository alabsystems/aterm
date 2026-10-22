// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Budgeted, resumable full-content search.
//!
//! [`Terminal::indexed_search`] answers repeat queries in O(1), but the first query after a
//! content change pays the whole index rebuild in one call. [`Terminal::search_budgeted`]
//! runs the same search in slices: each call verifies at most `row_budget` rows and
//! delivers at most 4,096 match deltas via [`aterm_search::BudgetedSearch`], then returns a
//! cursor, so the caller can yield between slices and cancel a superseded search.
//!
//! A cursor is valid only for its `(query, case_sensitive, is_regex)` and the snapshot key
//! `(alternate_screen, content_seq())` the one-shot index uses. Any mismatch (changed
//! content or pattern, a dropped or forged cursor) starts over from row zero and the step
//! says `reset`. A completed search is consistent because every slice saw the same key.

use super::search_index::ONE_SHOT_SEARCH;
use super::search_rows::history_row_for_search;
use super::{SearchKey, Terminal};
use crate::search::{BudgetedSearch, SearchOptionsError, SearchResults};
use std::sync::atomic::{AtomicU64, Ordering};

/// Maximum match records copied out per turn, so a dense search does not keep marshalling
/// an ever-growing prefix (verification stays row-budgeted).
const MAX_MATCHES_PER_STEP: usize = 4_096;

/// Cursor token source, unique module-wide so a cursor from another live Terminal is never
/// accepted. Separate wasm module instances are distinct cursor domains: hosts keep a
/// cursor with its originating engine.
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

// Piece-wise `write_str` + `Display::fmt`, never a runtime-argument `format_args!` (the
// Trust lane fails closed on its unsafe constructor).
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

/// Lifecycle of a retained budgeted search: `InFlight` keeps the cursor live; `Complete`
/// keeps the verified matches for `search_summary` but retires the cursor, so presenting it
/// restarts.
#[derive(PartialEq, Eq)]
enum ScanPhase {
    InFlight,
    Complete,
}

/// Budgeted search state in `Terminal::budgeted_search`: at most one per terminal, a new
/// search supersedes it.
pub(crate) struct BudgetedSearchState {
    /// Cursor token issued to the caller; must match to resume.
    cursor: u64,
    /// Snapshot key: which grid was active when the search started.
    alt_screen: bool,
    /// Snapshot key: the active grid's `content_seq()` at start.
    content_gen: u64,
    /// The query the cursor was issued for.
    query: String,
    /// Case sensitivity the cursor was issued for.
    case_sensitive: bool,
    /// Regex mode the cursor was issued for.
    is_regex: bool,
    /// The `ambiguous_width` policy at start (a live switch moves no `content_seq`).
    ambiguous_width: aterm_types::text_shaping::AmbiguousWidth,
    /// Scrollback line count at start (any change bumps `content_seq`); rows below it are
    /// history lines.
    scrollback: usize,
    /// The incremental engine: verified matches + progress; no retained rows.
    engine: BudgetedSearch,
    /// Number of stable match records already delivered to the caller.
    emitted_matches: usize,
    /// A `Complete` state stays for `search_summary` until a fresh start supersedes it or
    /// `release_search_index()` frees it; its cursor is spent.
    phase: ScanPhase,
}

/// One slice of a budgeted search: the results so far plus resume state.
#[derive(Debug)]
pub struct BudgetedSearchStep {
    /// The new match delta plus incompleteness/watermark metadata. Once `complete`, the
    /// concatenated `matches` equal a one-shot [`Terminal::indexed_search`] query.
    pub results: SearchResults,
    /// Every row has been verified and every match delta delivered.
    pub complete: bool,
    /// Token to resume with; `None` once complete.
    pub cursor: Option<u64>,
    /// Stable identity of this logical search, kept on the completing step too; a changed
    /// identity means earlier deltas belong to a superseded snapshot.
    pub search_id: u64,
    /// This call started a fresh logical search (first call, supersession, or a stale,
    /// changed-query or foreign cursor): clear accumulated deltas before appending.
    pub reset: bool,
    /// Rows consumed so far (monotone within one search; restarts reset it).
    pub rows_fed: usize,
    /// Total rows this search will consume.
    pub total_rows: usize,
}

impl Terminal {
    /// Run at most `row_budget` rows (at least 1) of a full-content search, resuming from
    /// `resume_cursor` while it is valid (module docs). A turn may drain a bounded match
    /// backlog without advancing `rows_fed`. Drive to `complete` by passing each step's
    /// cursor back and appending its result deltas. A row whose regex scan runs out of
    /// budget is counted in `cut_short_lines`, as the one-shot search counts it.
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
                // A completed state is never resumable; it is kept for `search_summary`.
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
            // Fresh start: whatever was in flight is superseded and its cursor dies.
            self.budgeted_search = None;
            let grid = &self.grid;
            let oldest = usize::try_from(grid.oldest_absolute_row()).unwrap_or(usize::MAX);
            let scrollback = grid.scrollback_lines();
            let total_rows = scrollback.saturating_add(usize::from(self.rows()));
            // The one-shot index's window: the rows past it are fed unverified.
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

        // Taken out so row reads (&self) don't conflict with feeding the engine.
        let mut state = self
            .budgeted_search
            .take()
            .expect("budgeted_search populated above");

        // Drain a large match backlog before scanning more rows, bounding both the payload
        // and the per-turn copying.
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
        // Visible rows: the walk above fed every history row below `end` (`hist_end <= end
        // <= total_rows`), so every row left here is visible.
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
        // Kept either way: incomplete for resume, complete so a same-key `search_summary`
        // reads its matches without rebuilding (the spent cursor cannot resume it).
        state.phase = if complete {
            ScanPhase::Complete
        } else {
            ScanPhase::InFlight
        };
        self.budgeted_search = Some(state);
        Ok(step)
    }

    /// Feed the history rows below `end`. The results-equality contract needs the same row
    /// sources (and bounded history read) as the one-shot build, which copies its rows
    /// through `Terminal::read_search_rows`: the same dense walk (item `k` is
    /// `get_history_line(start + k)`, an unreadable line a `None` placeholder, empty text),
    /// padded with placeholders should the walk be shorter than the keyed retained count.
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

    /// Drop any budgeted search (its matches and scratch; an outstanding cursor would
    /// restart). Call when the search UI closes or a query is abandoned between slices.
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

    /// Results for a federated `search_summary`: a budgeted search for the same
    /// `(query, case_sensitive, is_regex)` completed over the current snapshot answers
    /// directly, with no rebuild ([`search_index_rebuilds`](Self::search_index_rebuilds)
    /// unchanged); a miss falls back to [`indexed_search`](Self::indexed_search). Both give
    /// identical results (`budgeted_completion_equals_one_shot_all_modes`).
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
mod tests {
    use super::{MAX_MATCHES_PER_STEP, Terminal, allocate_cursor};
    use crate::search::SearchResults;
    use std::sync::atomic::AtomicU64;

    fn one_shot(
        t: &mut Terminal,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
    ) -> SearchResults {
        t.indexed_search()
            .search_results_opts(query, case_sensitive, is_regex)
            .expect("one-shot search")
    }

    /// Drive a budgeted search to completion at `budget` rows per step: the concatenated
    /// deltas and the step count.
    fn drive(
        t: &mut Terminal,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
        budget: usize,
    ) -> (SearchResults, usize) {
        let mut cursor = None;
        let mut steps = 0usize;
        let mut matches = Vec::new();
        let mut search_id = None;
        loop {
            let step = t
                .search_budgeted(query, case_sensitive, is_regex, cursor, budget)
                .expect("budgeted step");
            steps += 1;
            assert!(steps <= 10_000, "budgeted search must terminate");
            assert!(
                step.results.matches.len() <= MAX_MATCHES_PER_STEP,
                "one turn must bound result materialization"
            );
            let incomplete = step.results.incomplete;
            let lowest_retained_line = step.results.lowest_retained_line;
            let cut_short_lines = step.results.cut_short_lines;
            if let Some(existing) = search_id {
                assert!(!step.reset, "a valid resume must not reset the stream");
                assert_eq!(step.search_id, existing, "search identity is stable");
            } else {
                assert!(step.reset, "the first step starts a new result stream");
                search_id = Some(step.search_id);
            }
            matches.extend(step.results.matches);
            if step.complete {
                assert_eq!(step.cursor, None, "a complete step carries no cursor");
                return (
                    SearchResults::new(matches, incomplete, lowest_retained_line)
                        .with_cut_short_lines(cut_short_lines),
                    steps,
                );
            }
            assert!(step.cursor.is_some(), "an incomplete step carries a cursor");
            cursor = step.cursor;
        }
    }

    fn seeded_terminal() -> Terminal {
        let mut t = Terminal::new(6, 40);
        for i in 0..50 {
            t.process(format!("filler {i} with NEEDLE-{i} inside\r\n").as_bytes());
        }
        t.process(b"case Needle mixed\r\n");
        t
    }

    /// Over a corrupt tiered segment every budget reproduces the one-shot results: both
    /// walk history the same dense way.
    #[test]
    fn budgeted_completion_equals_one_shot_over_a_corrupt_tiered_segment() {
        for budget in [1, 3, 7, 1_000] {
            let (mut t, _) = crate::terminal::search_index::tests::corrupt_tiered_terminal();
            let oracle = one_shot(&mut t, "NEEDLE_", true, false);
            assert_eq!(oracle.matches.len(), 2);
            let (got, _) = drive(&mut t, "NEEDLE_", true, false, budget);
            assert_eq!(got, oracle, "budget={budget}");
        }
    }

    /// For every filter mode and several budgets, driving to completion equals the one-shot
    /// search: matches, incomplete flag and watermark.
    #[test]
    fn budgeted_completion_equals_one_shot_all_modes() {
        let cases: &[(&str, bool, bool)] = &[
            ("NEEDLE", true, false),
            ("needle", false, false),
            ("le", false, false),
            ("NEEDLE-[0-9]+", true, true),
            ("needle-1?7", false, true),
        ];
        for &(query, case_sensitive, is_regex) in cases {
            for budget in [1, 7, 1_000] {
                let mut t = seeded_terminal();
                let oracle = one_shot(&mut t, query, case_sensitive, is_regex);
                let (got, steps) = drive(&mut t, query, case_sensitive, is_regex, budget);
                assert_eq!(
                    got, oracle,
                    "query={query:?} cs={case_sensitive} rx={is_regex} budget={budget}"
                );
                let total = t.grid().scrollback_lines() + usize::from(t.rows());
                assert_eq!(
                    steps,
                    total.div_ceil(budget),
                    "step count must reflect the row budget (budget={budget})"
                );
            }
        }
    }

    /// An invalid regex propagates the engine error and retains no state.
    #[test]
    fn invalid_regex_errors_and_keeps_no_state() {
        let mut t = seeded_terminal();
        assert!(t.search_budgeted("f(oo", false, true, None, 5).is_err());
        let (got, _) = drive(&mut t, "NEEDLE", true, false, 9);
        assert_eq!(got, one_shot(&mut t, "NEEDLE", true, false));
    }

    /// A row the per-line regex budget cannot afford is reported as the one-shot search
    /// reports it: counted in `cut_short_lines`, results incomplete, the rows around it
    /// searched. Both shapes: one search over budget, and a `find_iter` whose searches each
    /// fit but together re-walk the row quadratically.
    #[test]
    fn an_unaffordable_regex_row_is_reported_not_passed_off() {
        for (pattern, row) in [
            ("(?:x?){1020}z", format!("{}z", "x".repeat(4095))),
            (r"\w+=|\w", "a".repeat(4096)),
        ] {
            for budget in [1, usize::MAX] {
                let mut t = Terminal::new(3, 4096);
                t.process(format!("before z\r\n{row}\r\nafter z").as_bytes());
                let oracle = t
                    .indexed_search()
                    .search_results_opts(pattern, true, true)
                    .expect("a row out of budget is reported, not refused");
                assert_eq!(oracle.cut_short_lines, 1, "{pattern:?}: {oracle:?}");
                assert!(oracle.incomplete, "{pattern:?}");
                assert!(
                    oracle.matches.iter().any(|m| m.start_col == 6),
                    "{pattern:?}: the row after the long one is searched: {oracle:?}"
                );
                let (got, _) = drive(&mut t, pattern, true, true, budget);
                assert_eq!(got, oracle, "{pattern:?} budget={budget}");
            }
        }
    }

    /// A cursor from another live Terminal restarts, even with matching query and content.
    #[test]
    fn foreign_terminal_cursor_restarts_with_a_unique_token() {
        let mut a = seeded_terminal();
        let mut b = seeded_terminal();
        let a_first = a
            .search_budgeted("NEEDLE", true, false, None, 5)
            .expect("terminal a");
        let b_first = b
            .search_budgeted("NEEDLE", true, false, None, 5)
            .expect("terminal b");
        assert_ne!(a_first.cursor, b_first.cursor, "live tokens are unique");

        let restarted = b
            .search_budgeted("NEEDLE", true, false, a_first.cursor, 5)
            .expect("foreign cursor restarts");
        assert_eq!(restarted.rows_fed, 5, "foreign token starts at row zero");
        assert_ne!(restarted.cursor, a_first.cursor);
        assert_ne!(restarted.cursor, b_first.cursor);
        assert!(restarted.reset);
        assert_ne!(restarted.search_id, b_first.search_id);
    }

    /// The allocator fails closed at the boundary instead of wrapping to a reissued token.
    #[test]
    fn cursor_allocator_never_wraps() {
        let counter = AtomicU64::new(u64::MAX - 1);
        assert_eq!(allocate_cursor(&counter), Some(u64::MAX - 1));
        assert_eq!(allocate_cursor(&counter), None);
        assert_eq!(allocate_cursor(&counter), None);
    }

    /// After a budgeted search completes, `search_summary` reads its retained results with
    /// no rebuild; once content changes, the fallback rebuilds.
    #[test]
    fn search_summary_reads_completed_index_with_zero_rebuild() {
        let mut t = seeded_terminal();
        let (driven, _steps) = drive(&mut t, "NEEDLE", true, false, 7);
        assert_eq!(
            t.search_index_rebuilds(),
            0,
            "the budgeted scan must not build the one-shot index"
        );

        let before = t.search_index_rebuilds();
        let summary = t
            .search_summary_results("NEEDLE", true, false)
            .expect("summary over retained index");
        assert_eq!(
            t.search_index_rebuilds(),
            before,
            "search_summary over a just-completed budgeted index must not rebuild"
        );
        assert_eq!(
            summary, driven,
            "the retained-index read equals the budgeted completion"
        );

        // After the no-rebuild check: this builds the one-shot index.
        let oracle = one_shot(&mut t, "NEEDLE", true, false);
        assert_eq!(summary, oracle, "retained read equals the one-shot search");

        // Negative control: changed content misses the retained key and rebuilds.
        t.process(b"NEEDLE-appended after completion\r\n");
        let before_miss = t.search_index_rebuilds();
        let missed = t
            .search_summary_results("NEEDLE", true, false)
            .expect("summary after content change");
        assert!(
            t.search_index_rebuilds() > before_miss,
            "a retained-key miss must rebuild the one-shot index"
        );
        assert_eq!(
            missed,
            one_shot(&mut t, "NEEDLE", true, false),
            "the fallback result reflects the new content"
        );
    }

    /// `release_search_index` frees a completed search's retained results, so a later
    /// `search_summary` rebuilds.
    #[test]
    fn a_released_completed_index_is_rebuilt_by_the_next_summary() {
        let mut t = seeded_terminal();
        drive(&mut t, "NEEDLE", true, false, 7);
        t.release_search_index();
        let before = t.search_index_rebuilds();
        let after_release = t
            .search_summary_results("NEEDLE", true, false)
            .expect("summary after release");
        assert!(
            t.search_index_rebuilds() > before,
            "after release the summary must rebuild from the buffer"
        );
        assert_eq!(after_release, one_shot(&mut t, "NEEDLE", true, false));
    }
}
