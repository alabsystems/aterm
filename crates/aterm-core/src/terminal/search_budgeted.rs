// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! P1.1: budgeted, resumable full-content search.
//!
//! [`Terminal::indexed_search`] (P1.0b) made REPEAT queries O(1), but the first
//! query after a content change still pays the whole index rebuild in ONE call
//! — hundreds of milliseconds at deep scrollback, all of it blocking the
//! caller's event loop (in the wasm render worker, that freezes input echo).
//! [`Terminal::search_budgeted`] performs the SAME search in caller-sized
//! slices: each call indexes + verifies at most `row_budget` rows and delivers
//! at most 4,096 stable match deltas (reusing the index's incremental
//! `index_line` construction via
//! [`aterm_search::BudgetedSearch`]) and returns a cursor to continue, so the
//! caller can yield between slices and CANCEL a superseded search instead of
//! finishing it.
//!
//! ## Cursor validity — stale cursors restart, never lie
//!
//! A step's cursor is valid only for the exact `(query, case_sensitive,
//! is_regex)` it was issued for AND the exact content snapshot key
//! `(alternate_screen, content_seq())` — the same complete staleness key the
//! cached one-shot index uses (see `search_index.rs`). Any mismatch (content
//! changed mid-search, a different pattern, a cursor from a dropped search, a
//! forged value) silently STARTS OVER from row zero: the step returned for a
//! stale cursor is the first slice of a FRESH search, never a continuation
//! over changed content. Callers detect the restart by `rows_fed <=
//! row_budget` (and a new cursor value); results from a completed search are
//! always internally consistent because completion requires every slice to
//! have observed the same snapshot key.

use super::Terminal;
use super::selection::{MAX_SCROLLBACK_LINE_SCAN_BYTES, line_text_bounded};
use crate::search::{BudgetedSearch, SearchOptionsError, SearchResults};
use std::sync::atomic::{AtomicU64, Ordering};

/// Maximum match records copied out on one resume turn. Index/verification
/// remains row-budgeted; this second bound prevents dense searches from
/// repeatedly marshalling an ever-growing 100k-match prefix.
const MAX_MATCHES_PER_STEP: usize = 4_096;

/// Linked-engine-module token source. A cursor is meaningful only while its
/// owning Terminal and search state are alive, so module-wide uniqueness
/// prevents a cursor from another live Terminal in that module from being
/// accepted accidentally. Separate wasm CPU/GPU module instances are distinct
/// cursor domains; hosts must retain the cursor with its originating engine.
static NEXT_BUDGETED_SEARCH_CURSOR: AtomicU64 = AtomicU64::new(1);

#[allow(deprecated)] // Trust renamed this to try_update; stable x86 slice still exposes fetch_update.
fn allocate_cursor(counter: &AtomicU64) -> Option<u64> {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .ok()
}

/// Error returned while starting a budgeted search.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, aterm_error::Error)]
pub enum BudgetedSearchError {
    /// The query/options were invalid.
    #[error("invalid search options: {0}")]
    SearchOptions(SearchOptionsError),
    /// The process-wide non-repeating cursor space was exhausted. The search
    /// fails closed instead of wrapping and accepting an old/foreign token.
    #[error("budgeted-search cursor space exhausted")]
    CursorSpaceExhausted,
}

impl From<SearchOptionsError> for BudgetedSearchError {
    fn from(value: SearchOptionsError) -> Self {
        Self::SearchOptions(value)
    }
}

/// In-flight budgeted search state. Stored in `Terminal::budgeted_search`;
/// at most one search is live per terminal (a new search supersedes it).
pub(crate) struct BudgetedSearchState {
    /// Cursor token issued to the caller; must match to resume.
    cursor: u64,
    /// Snapshot key: which grid was active when the search started.
    alt_screen: bool,
    /// Snapshot key: the active grid's `content_seq()` at start.
    content_gen: u64,
    /// The query + options the cursor was issued for.
    query: String,
    /// Case sensitivity the cursor was issued for.
    case_sensitive: bool,
    /// Regex mode the cursor was issued for.
    is_regex: bool,
    /// Scrollback line count at start (fixed while the snapshot key holds:
    /// any change bumps `content_seq`). Rows below this are history lines.
    scrollback: usize,
    /// The incremental engine: partial index + verified matches + progress.
    engine: BudgetedSearch,
    /// Number of stable match records already delivered to the caller.
    emitted_matches: usize,
}

/// One slice of a budgeted search: the results so far plus resume state.
#[derive(Debug)]
pub struct BudgetedSearchStep {
    /// Newly discovered match delta plus current incompleteness/watermark
    /// metadata. Append each step's `matches` in order; once `complete`, the
    /// concatenation equals a one-shot [`Terminal::indexed_search`] query.
    pub results: SearchResults,
    /// Whether every retained row has been indexed + verified and every match
    /// delta has been delivered.
    pub complete: bool,
    /// Token to resume with; `None` once complete.
    pub cursor: Option<u64>,
    /// Stable identity of this logical search, retained even on the completing
    /// step when `cursor` is `None`. A changed identity means previously
    /// accumulated deltas belong to a superseded snapshot.
    pub search_id: u64,
    /// True when this call started a fresh logical search (first call,
    /// explicit supersession, or stale/query/foreign-cursor restart). Callers
    /// accumulating deltas must clear the old set before appending this step.
    pub reset: bool,
    /// Rows consumed so far (monotone within one search; restarts reset it).
    pub rows_fed: usize,
    /// Total rows this search will consume.
    pub total_rows: usize,
}

impl Terminal {
    /// Run at most `row_budget` rows of a full-content search, resuming from
    /// `resume_cursor` when it is still valid (see module docs for the cursor
    /// staleness contract; `row_budget` is clamped to at least 1). A turn may
    /// drain a bounded match backlog without advancing `rows_fed`; it still
    /// makes delivery progress. Drive to `complete` by passing each step's
    /// cursor back and appending the returned result deltas.
    ///
    /// # Errors
    /// Propagates pattern errors from the engine (invalid/oversized regex,
    /// regex feature disabled), and fails closed if the process-wide
    /// non-repeating cursor space is exhausted. No state is retained after an
    /// error.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "budgeted_search_resume",
            action = "Start",
            project = "conformance_budgeted_search::project_step"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "budgeted_search_resume",
            action = "StartComplete",
            project = "conformance_budgeted_search::project_step"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "budgeted_search_resume",
            action = "StartBacklog",
            project = "conformance_budgeted_search::project_step"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "budgeted_search_resume",
            action = "Resume",
            project = "conformance_budgeted_search::project_step"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "budgeted_search_resume",
            action = "FinishScan",
            project = "conformance_budgeted_search::project_step"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "budgeted_search_resume",
            action = "Supersede",
            project = "conformance_budgeted_search::project_step"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "budgeted_search_resume",
            action = "ContentRestart",
            project = "conformance_budgeted_search::project_step"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "budgeted_search_resume",
            action = "QueryRestart",
            project = "conformance_budgeted_search::project_step"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "budgeted_search_resume",
            action = "ForgedRestart",
            project = "conformance_budgeted_search::project_step"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "budgeted_search_resume",
            action = "RestartComplete",
            project = "conformance_budgeted_search::project_step"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "budgeted_search_resume",
            action = "Drain",
            project = "conformance_budgeted_search::project_step"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
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
                state.cursor == cursor
                    && state.alt_screen == key_alt
                    && state.content_gen == key_gen
                    && state.query == query
                    && state.case_sensitive == case_sensitive
                    && state.is_regex == is_regex
            }
            _ => false,
        };

        let reset = !resumable;
        if reset {
            // Fresh start: whatever was in flight is superseded (its cursor
            // dies here — a later resume attempt restarts again).
            self.budgeted_search = None;
            let grid = &self.grid;
            let oldest = usize::try_from(grid.oldest_absolute_row()).unwrap_or(usize::MAX);
            let scrollback = grid.scrollback_lines();
            let total_rows = scrollback.saturating_add(usize::from(self.rows()));
            let engine = BudgetedSearch::new(query, case_sensitive, is_regex, oldest, total_rows)?;
            let cursor = allocate_cursor(&NEXT_BUDGETED_SEARCH_CURSOR)
                .ok_or(BudgetedSearchError::CursorSpaceExhausted)?;
            self.budgeted_search = Some(BudgetedSearchState {
                cursor,
                alt_screen: key_alt,
                content_gen: key_gen,
                query: query.to_string(),
                case_sensitive,
                is_regex,
                scrollback,
                engine,
                emitted_matches: 0,
            });
        }

        // Take the state out so row reads (&self) don't conflict with feeding
        // the engine (&mut state); restored below unless complete.
        let mut state = self
            .budgeted_search
            .take()
            .expect("budgeted_search populated above");

        // A large undispatched match backlog is drained before scanning more
        // rows. This keeps both returned payload and per-turn copying bounded.
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
        while state.engine.rows_fed() < end {
            let i = state.engine.rows_fed();
            // Same row sources (and bounded history read) as the one-shot
            // build in `search_index.rs::build_search_index` — the
            // results-equality contract depends on it.
            let text = if i < state.scrollback {
                self.grid
                    .get_history_line(i)
                    .map(|l| line_text_bounded(l.as_bytes(), MAX_SCROLLBACK_LINE_SCAN_BYTES))
                    .unwrap_or_default()
            } else {
                let visible_row = i - state.scrollback;
                u16::try_from(visible_row)
                    .ok()
                    .and_then(|r| self.get_line_text(i32::from(r), None))
                    .unwrap_or_default()
            };
            state.engine.feed_row(&text);
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
        if !complete {
            self.budgeted_search = Some(state);
        }
        Ok(step)
    }

    /// Drop any in-flight budgeted search (frees its partial index; any
    /// outstanding cursor becomes stale and would restart). Call when the
    /// search UI closes or a query is abandoned between slices.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "budgeted_search_resume",
            action = "Cancel",
            project = "conformance_budgeted_search::project_cancel"
        )
    )]
    pub fn cancel_budgeted_search(&mut self) {
        self.budgeted_search = None;
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

    /// Drive a budgeted search to completion at `budget` rows per step.
    /// Returns the concatenated stable deltas plus the number of steps taken.
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
                    SearchResults::new(matches, incomplete, lowest_retained_line),
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

    /// Resume-equality oracle at the terminal level: for every filter mode and
    /// several budgets, driving to completion equals the one-shot cached-index
    /// search — matches, incomplete flag, and watermark alike.
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

    /// The per-step budget is respected exactly: each step advances `rows_fed`
    /// by at most `budget` (exactly `budget` until the final partial step),
    /// and a zero budget is clamped to 1 so progress is guaranteed.
    #[test]
    fn each_step_consumes_at_most_the_budget() {
        let mut t = seeded_terminal();
        let budget = 7usize;
        let mut cursor = None;
        let mut prev_rows = 0usize;
        loop {
            let step = t
                .search_budgeted("NEEDLE", true, false, cursor, budget)
                .expect("step");
            let advanced = step.rows_fed - prev_rows;
            assert!(advanced >= 1, "every step makes progress");
            assert!(
                advanced <= budget,
                "step consumed {advanced} > budget {budget}"
            );
            if step.complete {
                assert_eq!(step.rows_fed, step.total_rows);
                break;
            }
            assert_eq!(advanced, budget, "non-final steps consume the full budget");
            prev_rows = step.rows_fed;
            cursor = step.cursor;
        }

        // Zero budget: clamped to one row per step, still terminates.
        let mut t = seeded_terminal();
        let (got, steps) = drive(&mut t, "NEEDLE", true, false, 0);
        assert_eq!(got, one_shot(&mut t, "NEEDLE", true, false));
        assert_eq!(steps, t.grid().scrollback_lines() + usize::from(t.rows()));
    }

    /// Stale cursors restart safely: after a content write invalidates the
    /// snapshot key, resuming the old cursor starts a FRESH search (progress
    /// reset, new cursor) whose completed results reflect the NEW content —
    /// never a stale mixture, never a panic.
    #[test]
    fn content_change_between_slices_restarts_from_scratch() {
        let mut t = seeded_terminal();
        let first = t
            .search_budgeted("NEEDLE", true, false, None, 5)
            .expect("first slice");
        assert!(!first.complete);

        // Content changes between slices (the P1 cancellation race).
        t.process(b"NEEDLE-fresh after the first slice\r\n");

        let resumed = t
            .search_budgeted("NEEDLE", true, false, first.cursor, 5)
            .expect("stale resume");
        assert!(
            resumed.rows_fed <= 5,
            "a stale cursor must restart from row zero, not continue at {}",
            resumed.rows_fed
        );
        assert_ne!(
            resumed.cursor, first.cursor,
            "the restarted search must issue a fresh cursor"
        );

        let mut cursor = resumed.cursor;
        let mut final_results = resumed.results.clone();
        while cursor.is_some() {
            let step = t
                .search_budgeted("NEEDLE", true, false, cursor, 5)
                .expect("resume");
            final_results.matches.extend(step.results.matches);
            final_results.incomplete = step.results.incomplete;
            final_results.lowest_retained_line = step.results.lowest_retained_line;
            cursor = step.cursor;
        }
        let oracle = one_shot(&mut t, "NEEDLE", true, false);
        assert_eq!(
            final_results, oracle,
            "post-restart results match new content"
        );
        // Sanity: the new content really is part of the result set (51 seeded
        // NEEDLE rows + the post-slice write).
        assert_eq!(final_results.matches.len(), 51);
    }

    /// A different pattern with an old cursor is a NEW search for the new
    /// pattern; a forged/dropped cursor value restarts too. Cancel frees the
    /// in-flight state and stales its cursor.
    #[test]
    fn pattern_change_forged_cursor_and_cancel_all_restart() {
        let mut t = seeded_terminal();
        let first = t
            .search_budgeted("NEEDLE", true, false, None, 5)
            .expect("first slice");

        // Different pattern + old cursor => fresh search for the new pattern.
        let switched = t
            .search_budgeted("filler", true, false, first.cursor, 5)
            .expect("switched pattern");
        assert!(switched.rows_fed <= 5, "new pattern restarts progress");
        assert_ne!(switched.cursor, first.cursor);

        // Forged cursor => fresh search, no panic.
        let forged = t
            .search_budgeted("filler", true, false, Some(0xDEAD_BEEF), 5)
            .expect("forged cursor");
        assert!(forged.rows_fed <= 5);

        // Cancel: the outstanding cursor goes stale; resuming restarts.
        let live = t
            .search_budgeted("filler", true, false, forged.cursor, 5)
            .expect("live resume");
        assert!(live.rows_fed > forged.rows_fed, "valid cursor resumes");
        t.cancel_budgeted_search();
        let after_cancel = t
            .search_budgeted("filler", true, false, live.cursor, 5)
            .expect("post-cancel resume");
        assert!(
            after_cancel.rows_fed <= 5,
            "a cancelled search's cursor must restart, not resume"
        );
    }

    /// An invalid regex propagates the engine error and retains no state.
    #[test]
    fn invalid_regex_errors_and_keeps_no_state() {
        let mut t = seeded_terminal();
        assert!(t.search_budgeted("f(oo", false, true, None, 5).is_err());
        // The next valid search starts cleanly.
        let (got, _) = drive(&mut t, "NEEDLE", true, false, 9);
        assert_eq!(got, one_shot(&mut t, "NEEDLE", true, false));
    }

    /// A stale-content restart that also completes in one turn still exposes
    /// an explicit reset and fresh search identity, even though no resume
    /// cursor remains. This prevents hosts from appending new-snapshot deltas
    /// to matches accumulated before invalidation.
    #[test]
    fn completing_stale_restart_explicitly_resets_delta_stream() {
        let mut t = seeded_terminal();
        let first = t
            .search_budgeted("NEEDLE", true, false, None, 1)
            .expect("initial partial search");
        assert!(first.reset);
        assert!(!first.complete);
        assert!(
            !first.results.matches.is_empty(),
            "pre-restart delta exists"
        );

        t.process(b"NEEDLE-fresh after partial search\r\n");
        let restarted = t
            .search_budgeted("NEEDLE", true, false, first.cursor, usize::MAX)
            .expect("one-turn stale restart");
        assert!(restarted.complete);
        assert_eq!(
            restarted.cursor, None,
            "completing step has no resume token"
        );
        assert!(restarted.reset, "host must be told to clear earlier deltas");
        assert_ne!(
            restarted.search_id, first.search_id,
            "fresh snapshot identity"
        );

        let oracle = one_shot(&mut t, "NEEDLE", true, false);
        assert_eq!(
            restarted.results, oracle,
            "fresh one-turn delta is complete"
        );
        assert_ne!(
            first.results.matches.len() + restarted.results.matches.len(),
            oracle.matches.len(),
            "blindly appending the pre-reset delta would be detectably wrong"
        );
    }

    /// Process-wide tokens prevent a cursor from one live Terminal from being
    /// accepted by another, even when query/options/content generations match.
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

    /// The allocator fails closed at the numeric boundary instead of wrapping
    /// to a token that may have been issued before.
    #[test]
    fn cursor_allocator_never_wraps() {
        let counter = AtomicU64::new(u64::MAX - 1);
        assert_eq!(allocate_cursor(&counter), Some(u64::MAX - 1));
        assert_eq!(allocate_cursor(&counter), None);
        assert_eq!(allocate_cursor(&counter), None);
    }

    /// Dense results are delivered as bounded deltas. Once indexing finishes,
    /// a later turn drains the remainder without cloning/resending the prefix.
    #[test]
    fn dense_results_are_delta_bounded_and_concatenate_to_one_shot() {
        let mut t = Terminal::new(6, 40);
        for _ in 0..220 {
            t.process(format!("{}\r\n", "a".repeat(30)).as_bytes());
        }
        let oracle = one_shot(&mut t, "a", true, false);

        let first = t
            .search_budgeted("a", true, false, None, usize::MAX)
            .expect("first dense slice");
        assert!(!first.complete, "more than one output chunk is required");
        assert_eq!(first.results.matches.len(), MAX_MATCHES_PER_STEP);
        assert_eq!(first.rows_fed, first.total_rows, "indexing finished");

        let second = t
            .search_budgeted("a", true, false, first.cursor, usize::MAX)
            .expect("drain dense remainder");
        assert!(second.complete);
        assert_eq!(
            second.rows_fed, first.rows_fed,
            "drain-only turn scans no rows"
        );
        assert!(second.results.matches.len() <= MAX_MATCHES_PER_STEP);

        let got = SearchResults::new(
            [first.results.matches, second.results.matches].concat(),
            second.results.incomplete,
            second.results.lowest_retained_line,
        );
        assert_eq!(got, oracle);
    }

    /// Spec-link lock: activating one refinement makes coverage all-or-nothing.
    /// Compare inventory, not source text, so a misspelled/stripped attribute is
    /// caught by the same compiled registry the global closure gate consumes.
    #[test]
    fn budgeted_resume_refinement_actions_are_complete() {
        let model_actions: std::collections::BTreeSet<_> =
            aterm_spec::derive::budgeted_search_resume_model()
                .actions
                .into_iter()
                .map(|action| action.name)
                .collect();
        let anchors: Vec<_> = aterm_spec::xref::refinements()
            .filter(|anchor| anchor.machine == "budgeted_search_resume")
            .collect();
        let anchored_actions: std::collections::BTreeSet<_> =
            anchors.iter().map(|anchor| anchor.action).collect();
        assert_eq!(
            anchored_actions, model_actions,
            "every BudgetedSearchResume action must have a compiled #[refines] anchor"
        );
        assert_eq!(
            anchors.len(),
            model_actions.len(),
            "one unambiguous source anchor is expected per model action"
        );
        assert!(
            anchors.iter().all(|anchor| !anchor.project.is_empty()),
            "every budgeted-search refinement must name its Tier-1 projection"
        );
    }
}
