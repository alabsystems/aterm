// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use crate::index::{REGEX_DFA_SIZE_LIMIT, REGEX_SIZE_LIMIT, REGEX_STEP_LIMIT};

#[cfg(test)]
use super::super::content::SearchContent;
use super::super::error::SearchError;
#[cfg(test)]
use super::super::types::StreamingMatch;
use super::super::types::{FilterMode, NavigationDirection, SearchState};
use super::StreamingSearch;

/// Set when a regex pattern first exhausts its per-row scan budget, so the warning
/// is logged once per process.
static REGEX_BUDGET_WARNED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

impl StreamingSearch {
    /// Compile `pattern`, prefixed with `(?i)` unless case-sensitive.
    fn compile_regex(&self, pattern: &str) -> Result<aterm_regex::Regex, SearchError> {
        let effective = if self.config.case_sensitive {
            pattern.to_string()
        } else {
            format!("(?i){pattern}")
        };
        aterm_regex::RegexBuilder::new(&effective)
            // The index's limits (`index.rs` derives them): this engine recompiles and
            // rescans every row on each keystroke. The row matcher scales the step budget
            // with line length (`index::scan_line`).
            .size_limit(REGEX_SIZE_LIMIT)
            .dfa_size_limit(REGEX_DFA_SIZE_LIMIT)
            .step_limit(REGEX_STEP_LIMIT)
            .build()
            .map_err(|e| SearchError::InvalidRegex(e.to_string()))
    }

    #[inline]
    fn restart_search_from_beginning(&mut self) {
        self.state = SearchState::Searching;
        self.results.clear();
        self.seen_positions.clear();
        self.current_index = 0;
        self.scan_progress = 0;
        self.total_matches = 0;
        self.bump_generation();
    }

    /// Start a search for `pattern` in `mode`. Model action `Start` of
    /// `aterm_spec::derive::streaming_search_model`.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "streaming_search",
            action = "Start",
            project = "conformance_streaming::project"
        )
    )]
    pub fn start_search(&mut self, pattern: &str, mode: FilterMode) -> Result<(), SearchError> {
        if pattern.is_empty() {
            return Err(SearchError::EmptyPattern);
        }

        if pattern.len() > self.config.max_pattern_len {
            return Err(SearchError::PatternTooLong);
        }

        if mode == FilterMode::Regex {
            self.compiled_regex = Some(self.compile_regex(pattern)?);
        }

        self.pattern = pattern.to_string();
        self.filter_mode = mode;
        self.state = SearchState::Searching;
        self.results.clear();
        self.seen_positions.clear();
        self.current_index = 0;
        self.scan_progress = 0;
        self.total_matches = 0;
        self.bump_generation();

        Ok(())
    }

    /// Update the pattern as the user types: empty cancels, a change restarts.
    ///
    /// Not a model action (pattern content has no scalar twin); `verify_all_invariants`
    /// covers it.
    #[cfg(test)]
    pub fn update_pattern(&mut self, new_pattern: &str) -> Result<(), SearchError> {
        if !matches!(
            self.state,
            SearchState::Searching | SearchState::HasResults | SearchState::NoResults
        ) {
            return Err(SearchError::InvalidState);
        }

        if new_pattern == self.pattern {
            return Ok(()); // No change
        }

        if new_pattern.len() > self.config.max_pattern_len {
            return Err(SearchError::PatternTooLong);
        }

        if new_pattern.is_empty() {
            self.pattern.clear();
            self.state = SearchState::Idle;
            self.results.clear();
            self.seen_positions.clear();
            self.current_index = 0;
            self.scan_progress = -1;
            self.total_matches = 0;
            self.bump_generation();
            self.compiled_regex = None;
        } else {
            // Compile first so a bad regex leaves the search unchanged.
            let compiled = if self.filter_mode == FilterMode::Regex {
                Some(self.compile_regex(new_pattern)?)
            } else {
                None
            };

            self.pattern = new_pattern.to_string();
            self.compiled_regex = compiled;
            self.state = SearchState::Searching;
            self.results.clear();
            self.seen_positions.clear();
            self.current_index = 0;
            self.scan_progress = 0;
            self.total_matches = 0;
            self.bump_generation();
        }

        Ok(())
    }

    /// Scan one row for matches; returns how many it found.
    ///
    /// Model actions `ScanHit`/`ScanMiss` for a row with one or no matches, the last
    /// row's `complete_search` folded in; rows with more are covered by
    /// `verify_all_invariants`.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "streaming_search",
            action = "ScanHit",
            project = "conformance_streaming::project"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "streaming_search",
            action = "ScanMiss",
            project = "conformance_streaming::project"
        )
    )]
    pub fn scan_row(&mut self, row: usize, text: &str, max_rows: usize) -> usize {
        if self.state != SearchState::Searching {
            return 0;
        }

        // Row count bounded by terminal dimensions — fits in isize
        let row_isize = isize::try_from(row).unwrap_or(isize::MAX);
        if self.scan_progress != row_isize {
            return 0;
        }

        if row >= max_rows {
            return 0;
        }

        let matches = self.find_matches_in_row(row, text);
        let match_count = matches.len();

        // The row matcher keeps only the matches a regex scan found before its budget
        // ran out (`index::scan_line`); the compiled pattern's sticky flag records the
        // cut and `incomplete()` exposes it. Keep scanning (the row still advances the
        // model-checked scan) and warn once per process for log-only consumers: a
        // pattern that exhausts its budget on one row will on thousands.
        if self.filter_mode == FilterMode::Regex
            && self
                .compiled_regex
                .as_ref()
                .is_some_and(aterm_regex::Regex::step_limit_exceeded)
            && !REGEX_BUDGET_WARNED.swap(true, core::sync::atomic::Ordering::Relaxed)
        {
            aterm_log::warn!(
                "regex search pattern {:?} exhausted its per-row scan budget; \
                 matches on the rows it could not finish are missing from the results",
                self.pattern,
            );
        }

        // Store up to `max_results`, count every match. The count cannot reach
        // `usize::MAX` (each step needs a real match); saturating carries the L0
        // no-overflow proof.
        for m in matches {
            let pos_key = (m.row, m.start_col);

            // INV-SEARCH-4: No duplicate results
            if self.seen_positions.contains(&pos_key) {
                // Skip duplicates
            } else if self.results.len() >= self.config.max_results {
                // INV-SEARCH-3: Memory bounded - at capacity, count but don't store
                self.total_matches = self.total_matches.saturating_add(1);
            } else {
                self.record_seen_position(pos_key);
                self.results.push(m);
                self.total_matches = self.total_matches.saturating_add(1);
            }
        }

        // `row < max_rows`, so the saturation is dead code carrying the L0 proof.
        let next_row = row.saturating_add(1);
        self.scan_progress = isize::try_from(next_row).unwrap_or(isize::MAX);

        if next_row >= max_rows {
            self.complete_search();
        }

        match_count
    }

    /// Scan every row of `content`, joining soft-wrapped rows into logical lines so a
    /// match can span the wrap.
    #[cfg(test)]
    pub fn scan_all<C: SearchContent>(&mut self, content: &mut C) {
        let max_rows = content.row_count();

        while self.state == SearchState::Searching {
            // scan_progress >= 0 during Searching state — try_from is lossless
            let row = usize::try_from(self.scan_progress).unwrap_or(usize::MAX);
            if row >= max_rows {
                self.complete_search();
                break;
            }

            if let Some(text) = content.get_row_text(row) {
                // Join wrapped continuation rows into one logical line so queries can
                // match across the wrap. Rows are bounded by `max_rows`, so the
                // saturations here are dead code carrying the L0 no-overflow proofs.
                let mut logical_end = row.saturating_add(1);
                while logical_end < max_rows && content.is_row_wrapped(logical_end) {
                    logical_end = logical_end.saturating_add(1);
                }

                if logical_end > row.saturating_add(1) {
                    // A logical line over several rows: record each row's width to map
                    // matches in the joined text back to physical (row, column). No
                    // capacity hint: push growth keeps the allocation provably bounded
                    // for the L0 gate.
                    let mut row_widths: Vec<usize> = Vec::new();
                    row_widths.push(crate::grapheme::display_columns(&text));
                    let mut joined = text;
                    for cont_row in row.saturating_add(1)..logical_end {
                        if let Some(cont_text) = content.get_row_text(cont_row) {
                            let cont_cols = crate::grapheme::display_columns(&cont_text);
                            row_widths.push(cont_cols);
                            joined.push_str(&cont_text);
                        } else {
                            row_widths.push(0);
                        }
                    }

                    // Find matches in the joined text, then remap coordinates.
                    let raw_matches = self.find_matches_in_row(row, &joined);
                    for m in raw_matches {
                        // Find the physical row the match starts on, walked with `get`
                        // so the L0 gate carries no bounds obligation.
                        let mut remaining_col = m.start_col;
                        let mut phys_idx = 0usize;
                        let mut idx = 0usize;
                        while let Some(&width) = row_widths.get(idx) {
                            if width == 0 || remaining_col < width {
                                phys_idx = idx;
                                break;
                            }
                            // Exact: `remaining_col >= width` here.
                            remaining_col = remaining_col.saturating_sub(width);
                            let next_idx = idx.saturating_add(1);
                            if row_widths.get(next_idx).is_none() {
                                phys_idx = idx;
                            }
                            idx = next_idx;
                        }
                        let phys_row = row.saturating_add(phys_idx);
                        let phys_start_col = remaining_col;

                        // Clamp the end to the start row's width.
                        let this_row_width = row_widths.get(phys_idx).copied().unwrap_or(0);
                        let match_end_col = phys_start_col.saturating_add(m.match_len);
                        let phys_end_col = if match_end_col > this_row_width {
                            this_row_width
                        } else {
                            match_end_col
                        };

                        let adjusted = StreamingMatch::new(phys_row, phys_start_col, phys_end_col);
                        // Filter zero-length after remapping.
                        if adjusted.match_len == 0 {
                            continue;
                        }
                        let pos_key = (adjusted.row, adjusted.start_col);
                        if self.seen_positions.contains(&pos_key) {
                            continue;
                        }
                        // Saturating for the L0 gate; see `scan_row`.
                        if self.results.len() >= self.config.max_results {
                            self.total_matches = self.total_matches.saturating_add(1);
                        } else {
                            self.record_seen_position(pos_key);
                            self.results.push(adjusted);
                            self.total_matches = self.total_matches.saturating_add(1);
                        }
                    }

                    // Advance scan progress past all wrapped rows.
                    self.scan_progress = isize::try_from(logical_end).unwrap_or(isize::MAX);
                    if logical_end >= max_rows {
                        self.complete_search();
                    }
                } else {
                    self.scan_row(row, &text, max_rows);
                }
            } else {
                // Missing row: skip it (`row < max_rows`; the saturation is for L0).
                let next_row = row.saturating_add(1);
                self.scan_progress = isize::try_from(next_row).unwrap_or(isize::MAX);
                if next_row >= max_rows {
                    self.complete_search();
                }
            }
        }
    }

    /// Finish the scan. Not a model action: it runs inside the last row's
    /// `ScanHit`/`ScanMiss`.
    fn complete_search(&mut self) {
        if self.state != SearchState::Searching {
            return;
        }

        if self.results.is_empty() {
            self.state = SearchState::NoResults;
            self.current_index = 0;
        } else {
            self.state = SearchState::HasResults;
            self.current_index = 1;
        }

        self.scan_progress = -1;
    }

    /// Cancel the current search.
    ///
    /// Model action `Cancel`.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "streaming_search",
            action = "Cancel",
            project = "conformance_streaming::project"
        )
    )]
    pub fn cancel(&mut self) {
        if !matches!(
            self.state,
            SearchState::Searching | SearchState::HasResults | SearchState::NoResults
        ) {
            return;
        }

        self.state = SearchState::Idle;
        self.pattern.clear();
        self.results.clear();
        self.seen_positions.clear();
        self.current_index = 0;
        self.scan_progress = -1;
        self.total_matches = 0;
        self.bump_generation();
        self.compiled_regex = None;
    }

    // Navigation.

    /// Navigate to the next match.
    ///
    /// Model action `NextMatch`.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "streaming_search",
            action = "NextMatch",
            project = "conformance_streaming::project"
        )
    )]
    pub fn next_match(&mut self) {
        if self.state != SearchState::HasResults || self.results.is_empty() {
            return;
        }

        self.current_index = Self::next_index(
            self.current_index,
            self.results.len(),
            NavigationDirection::Forward,
            self.config.wrap_enabled,
        );
    }

    /// Navigate to the previous match.
    ///
    /// Model action `PrevMatch`.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "streaming_search",
            action = "PrevMatch",
            project = "conformance_streaming::project"
        )
    )]
    pub fn prev_match(&mut self) {
        if self.state != SearchState::HasResults || self.results.is_empty() {
            return;
        }

        self.current_index = Self::next_index(
            self.current_index,
            self.results.len(),
            NavigationDirection::Backward,
            self.config.wrap_enabled,
        );
    }

    /// The 1-based index one step from `idx` in `dir`, wrapping or clamping at the ends
    /// per `wrap` (the model's `NextMatch`/`PrevMatch` update of `cur`).
    pub(crate) fn next_index(
        idx: usize,
        len: usize,
        dir: NavigationDirection,
        wrap: bool,
    ) -> usize {
        if len == 0 {
            return 0;
        }

        match dir {
            NavigationDirection::Forward => {
                if idx >= len {
                    if wrap { 1 } else { idx }
                } else {
                    idx + 1
                }
            }
            NavigationDirection::Backward => {
                if idx <= 1 {
                    if wrap { len } else { idx }
                } else {
                    idx - 1
                }
            }
        }
    }

    // Configuration.

    /// Toggle wrap-around navigation.
    #[cfg(test)]
    pub fn toggle_wrap(&mut self) {
        self.config.wrap_enabled = !self.config.wrap_enabled;
    }

    /// Toggle case sensitivity, recompiling the regex and restarting an active search.
    #[cfg(test)]
    pub fn toggle_case_sensitive(&mut self) {
        self.config.case_sensitive = !self.config.case_sensitive;

        if !self.pattern.is_empty()
            && matches!(
                self.state,
                SearchState::Searching | SearchState::HasResults | SearchState::NoResults
            )
        {
            if self.filter_mode == FilterMode::Regex
                && let Ok(re) = self.compile_regex(&self.pattern.clone())
            {
                self.compiled_regex = Some(re);
            }
            self.restart_search_from_beginning();
        }
    }

    /// Toggle highlight all matches.
    #[cfg(test)]
    pub fn toggle_highlight_all(&mut self) {
        self.config.highlight_all = !self.config.highlight_all;
    }

    /// Set the filter mode, restarting an active search.
    #[cfg(test)]
    pub fn set_filter_mode(&mut self, mode: FilterMode) -> Result<(), SearchError> {
        if mode == self.filter_mode {
            return Ok(());
        }

        if mode == FilterMode::Regex && !self.pattern.is_empty() {
            self.compiled_regex = Some(self.compile_regex(&self.pattern.clone())?);
        }

        self.filter_mode = mode;

        if !self.pattern.is_empty()
            && matches!(
                self.state,
                SearchState::Searching | SearchState::HasResults | SearchState::NoResults
            )
        {
            self.restart_search_from_beginning();
        }

        Ok(())
    }

    // Content changes.

    /// Scan a row added to the terminal; a match moves `NoResults` to `HasResults`.
    ///
    /// Model action `Add` for a row adding exactly one match; rows adding more are
    /// covered by `verify_all_invariants`.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "streaming_search",
            action = "Add",
            project = "conformance_streaming::project"
        )
    )]
    pub fn content_added(&mut self, row: usize, text: &str) {
        if !matches!(self.state, SearchState::HasResults | SearchState::NoResults) {
            return;
        }

        let matches = self.find_matches_in_row(row, text);

        let prev_len = self.results.len();

        for m in matches {
            let pos_key = (m.row, m.start_col);

            if self.seen_positions.contains(&pos_key) {
                continue;
            }

            // At capacity: count, don't store. Saturating for the L0 gate; see `scan_row`.
            if self.results.len() >= self.config.max_results {
                self.total_matches = self.total_matches.saturating_add(1);
                continue;
            }

            self.record_seen_position(pos_key);
            self.results.push(m);
            self.total_matches = self.total_matches.saturating_add(1);
        }

        if self.state == SearchState::NoResults && !self.results.is_empty() {
            self.state = SearchState::HasResults;
        }

        if self.results.len() != prev_len {
            // New matches may belong to rows before existing ones; keep row order for
            // navigation.
            self.results.sort_unstable_by_key(|m| (m.row, m.start_col));
            self.bump_generation();
        }

        if self.current_index == 0 && !self.results.is_empty() {
            self.current_index = 1;
        }
    }

    /// Re-scan `row` after it was rewritten in place (redraw, erase-in-line), replacing
    /// its matches. While `Searching`, only rows already scanned are touched.
    #[cfg(test)]
    pub fn content_modified(&mut self, row: usize, new_text: &str) {
        match self.state {
            SearchState::Searching => {
                // Only already-scanned rows can have stale matches.
                let row_isize = isize::try_from(row).unwrap_or(isize::MAX);
                if row_isize >= self.scan_progress {
                    return; // Will be scanned normally when scan_progress reaches it.
                }
            }
            SearchState::HasResults | SearchState::NoResults => {}
            SearchState::Idle => return,
        }

        let prev_len = self.results.len();

        let removed_count = self.results.iter().filter(|m| m.row == row).count();

        self.results.retain(|m| m.row != row);

        self.seen_positions.retain(|(r, _)| *r != row);

        // Only stored matches are subtracted (see `content_invalidated`).
        self.total_matches = self.total_matches.saturating_sub(removed_count);

        let new_matches = self.find_matches_in_row(row, new_text);

        for m in new_matches {
            let pos_key = (m.row, m.start_col);

            if self.seen_positions.contains(&pos_key) {
                continue;
            }

            // Saturating for the L0 gate; see `scan_row`.
            if self.results.len() >= self.config.max_results {
                self.total_matches = self.total_matches.saturating_add(1);
                continue;
            }

            self.record_seen_position(pos_key);
            self.results.push(m);
            self.total_matches = self.total_matches.saturating_add(1);
        }

        if self.results.len() != prev_len || removed_count > 0 {
            self.bump_generation();
        }

        // Re-sort on any change: a modified row's new matches can sort before later
        // rows' matches.
        if !self.results.is_empty() && (removed_count > 0 || self.results.len() != prev_len) {
            self.results.sort_unstable_by_key(|m| (m.row, m.start_col));
        }

        if self.results.is_empty() {
            if matches!(self.state, SearchState::HasResults | SearchState::NoResults) {
                self.state = SearchState::NoResults;
            }
            self.current_index = 0;
        } else {
            if self.state == SearchState::NoResults {
                self.state = SearchState::HasResults;
            }
            if self.current_index == 0 {
                self.current_index = 1;
            } else if self.current_index > self.results.len() {
                self.current_index = self.results.len();
            }
        }
    }

    /// Restart an active search after a reflow (e.g. a resize that rewraps lines),
    /// since every row index may have changed. Model action `Reflow`.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "streaming_search",
            action = "Reflow",
            project = "conformance_streaming::project"
        )
    )]
    pub fn content_reflowed(&mut self) {
        match self.state {
            SearchState::Searching | SearchState::HasResults | SearchState::NoResults => {
                self.restart_search_from_beginning();
            }
            SearchState::Idle => {}
        }
    }

    /// Drop matches on rows `from_row..=to_row` (scrolled out, cleared).
    ///
    /// Model action `Invalidate` for a range holding exactly one stored match; wider
    /// ranges are covered by `verify_all_invariants`.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "streaming_search",
            action = "Invalidate",
            project = "conformance_streaming::project"
        )
    )]
    pub fn content_invalidated(&mut self, from_row: usize, to_row: usize) {
        if !matches!(self.state, SearchState::HasResults | SearchState::NoResults) {
            return;
        }

        let prev_len = self.results.len();

        self.results.retain(|m| m.row < from_row || m.row > to_row);

        self.seen_positions
            .retain(|(row, _)| *row < from_row || *row > to_row);

        // Only stored matches are subtracted: ones counted past the cap stay in the
        // total. `retain` never grows, so the saturation is dead code for the L0 gate.
        let removed_count = prev_len.saturating_sub(self.results.len());
        self.total_matches = self.total_matches.saturating_sub(removed_count);

        if removed_count > 0 {
            self.bump_generation();
        }

        if self.results.is_empty() {
            self.state = SearchState::NoResults;
            self.current_index = 0;
        } else if self.current_index > self.results.len() {
            self.current_index = self.results.len();
        }
    }
}

#[cfg(test)]
mod tests {
    // Cases the `streaming/tests` suite does not reach.
    use super::super::super::types::{
        FilterMode, NavigationDirection, SearchState, StreamingSearchConfig,
    };
    use super::super::StreamingSearch;

    /// Start a literal search for `pattern` and scan `rows` to completion.
    fn engine_with_matches(pattern: &str, rows: &[&str]) -> StreamingSearch {
        let mut engine = StreamingSearch::new();
        engine.start_search(pattern, FilterMode::Literal).unwrap();
        let max_rows = rows.len();
        for (i, text) in rows.iter().enumerate() {
            engine.scan_row(i, text, max_rows);
        }
        engine
    }

    #[test]
    fn test_next_match_noop_when_no_results() {
        let mut engine = engine_with_matches("xyz", &["hello"]);
        assert_eq!(engine.state(), SearchState::NoResults);
        engine.next_match();
        assert_eq!(engine.current_index(), 0);
    }

    #[test]
    fn test_prev_match_noop_when_idle() {
        let mut engine = StreamingSearch::new();
        engine.prev_match();
        assert_eq!(engine.current_index(), 0);
    }

    #[test]
    fn test_next_index_forward_wrap() {
        assert_eq!(
            StreamingSearch::next_index(3, 3, NavigationDirection::Forward, true),
            1
        );
    }

    #[test]
    fn test_next_index_forward_no_wrap() {
        assert_eq!(
            StreamingSearch::next_index(3, 3, NavigationDirection::Forward, false),
            3
        );
    }

    #[test]
    fn test_next_index_backward_wrap() {
        assert_eq!(
            StreamingSearch::next_index(1, 5, NavigationDirection::Backward, true),
            5
        );
    }

    #[test]
    fn test_next_index_backward_no_wrap() {
        assert_eq!(
            StreamingSearch::next_index(1, 5, NavigationDirection::Backward, false),
            1
        );
    }

    #[test]
    fn test_next_index_empty_len_returns_zero() {
        assert_eq!(
            StreamingSearch::next_index(0, 0, NavigationDirection::Forward, true),
            0
        );
        assert_eq!(
            StreamingSearch::next_index(0, 0, NavigationDirection::Backward, true),
            0
        );
    }

    #[test]
    fn test_cancel_noop_when_idle() {
        let mut engine = StreamingSearch::new();
        let gen_before = engine.generation();
        engine.cancel();
        assert_eq!(engine.state(), SearchState::Idle);
        assert_eq!(engine.generation(), gen_before);
    }

    #[test]
    fn test_content_added_noop_when_idle() {
        let mut engine = StreamingSearch::new();
        let gen_before = engine.generation();
        engine.content_added(0, "needle");
        assert_eq!(engine.generation(), gen_before);
        assert_eq!(engine.result_count(), 0);
    }

    #[test]
    fn test_content_invalidated_all_transitions_to_no_results() {
        let mut engine = engine_with_matches("test", &["test 0", "test 1"]);
        engine.content_invalidated(0, 1);
        assert_eq!(engine.state(), SearchState::NoResults);
        assert_eq!(engine.current_index(), 0);
    }

    #[test]
    fn test_content_invalidated_clamps_current_index() {
        let mut engine = engine_with_matches("test", &["test 0", "test 1", "test 2"]);
        engine.next_match();
        engine.next_match(); // current is now 3
        assert_eq!(engine.current_index(), 3);

        // Remove the last match
        engine.content_invalidated(2, 2);
        assert_eq!(engine.result_count(), 2);
        assert_eq!(engine.current_index(), 2); // clamped
    }

    #[test]
    fn test_content_reflowed_restarts_search() {
        let mut engine = engine_with_matches("test", &["test here"]);
        let gen_before = engine.generation();

        engine.content_reflowed();
        assert_eq!(engine.state(), SearchState::Searching);
        assert_eq!(engine.result_count(), 0);
        assert_eq!(engine.scan_progress(), 0);
        assert!(engine.generation() > gen_before);
    }

    #[test]
    fn test_content_reflowed_noop_when_idle() {
        let mut engine = StreamingSearch::new();
        let gen_before = engine.generation();
        engine.content_reflowed();
        assert_eq!(engine.state(), SearchState::Idle);
        assert_eq!(engine.generation(), gen_before);
    }

    #[test]
    fn test_memory_bound_respected() {
        let config = StreamingSearchConfig {
            max_results: 3,
            ..StreamingSearchConfig::default()
        };
        let mut engine = StreamingSearch::with_config(config);
        engine.start_search("x", FilterMode::Literal).unwrap();
        for i in 0..10 {
            engine.scan_row(i, "x", 10);
        }
        assert_eq!(engine.result_count(), 3);
        assert_eq!(engine.total_matches(), 10);
        assert!(engine.verify_memory_bounded());
    }

    #[test]
    fn test_no_duplicate_results_after_content_added() {
        let mut engine = engine_with_matches("needle", &["needle here"]);
        // Re-add same row content
        engine.content_added(0, "needle here");
        assert!(engine.verify_no_duplicates());
    }
}

/// The regex scan budget in the streaming engine (`index.rs` derives the limits).
#[cfg(test)]
mod regex_scan_budget_tests {
    use super::*;

    /// 13 bytes, 2,042 instructions (inside the 128 KiB ceiling), ~16.7M work units for
    /// one 4,096-column row.
    const EXPENSIVE: &str = "(?:x?){1020}z";

    /// The default config is case-insensitive, and the prepended `(?i)` pushes
    /// `EXPENSIVE` over the size ceiling, so these tests pin case sensitivity.
    fn case_sensitive_engine() -> StreamingSearch {
        StreamingSearch::with_config(crate::streaming::types::StreamingSearchConfig {
            case_sensitive: true,
            ..Default::default()
        })
    }

    /// The budget reaches the engine's own compile path, not just the index's.
    #[test]
    fn the_streaming_engine_compiles_with_the_scan_budget() {
        let mut search = case_sensitive_engine();
        search
            .start_search("needle", FilterMode::Regex)
            .expect("compiles");
        let re = search
            .compiled_regex
            .as_ref()
            .expect("regex mode compiled a pattern");
        assert_eq!(re.step_limit(), REGEX_STEP_LIMIT);
    }

    /// A row the budget cuts short keeps only the matches found before the cut (none
    /// here) and sets the compiled pattern's sticky flag, so `incomplete()` holds
    /// until a new search.
    #[test]
    fn a_row_that_exhausts_the_budget_yields_no_matches() {
        let mut search = case_sensitive_engine();
        search
            .start_search(EXPENSIVE, FilterMode::Regex)
            .expect("a pattern the size ceiling admits");
        let wide = "x".repeat(4096);

        let found = search.scan_row(0, &wide, 1);

        assert_eq!(found, 0, "an unfinishable row must not invent matches");
        assert!(
            search.incomplete(),
            "a miss after exhaustion is not definitive"
        );
        assert!(
            search
                .compiled_regex
                .as_ref()
                .expect("compiled")
                .step_limit_exceeded(),
            "the truncation has to be readable, or short results are unexplainable"
        );

        search
            .start_search("z", FilterMode::Regex)
            .expect("fresh scan");
        search.scan_row(0, "ordinary z", 1);
        assert!(!search.incomplete(), "a new search clears exhaustion");
    }

    #[test]
    fn a_long_logical_line_uses_the_same_budget_as_indexed_search() {
        let unit = "function a(b){return b+1};var needle_x=[1,2,3];";
        let text = unit.repeat(24_000);
        let mut search =
            StreamingSearch::with_config(crate::streaming::types::StreamingSearchConfig {
                case_sensitive: true,
                max_results: 30_000,
                ..Default::default()
            });
        search
            .start_search(r"\w+\(", FilterMode::Regex)
            .expect("compiles");
        let found = search.scan_row(0, &text, 1);
        assert_eq!(found, 24_000, "ordinary matches throughout the joined line");
        assert!(!search.incomplete());
        assert_eq!(
            search.results().last().expect("matches").start_col,
            text.len() - unit.len() + 9
        );
    }

    #[test]
    fn a_result_cap_is_observable_for_literal_and_regex_search() {
        for mode in [FilterMode::Literal, FilterMode::Regex] {
            let mut search =
                StreamingSearch::with_config(crate::streaming::types::StreamingSearchConfig {
                    max_results: 1,
                    ..Default::default()
                });
            search.start_search("x", mode).expect("compiles");
            search.scan_row(0, "x x", 1);
            assert_eq!(search.result_count(), 1);
            assert_eq!(search.total_matches(), 2);
            assert!(search.incomplete());
            search.cancel();
            assert!(!search.incomplete());
        }
    }

    /// An ordinary pattern scans a full-width row normally and reports nothing cut short.
    #[test]
    fn ordinary_patterns_scan_rows_normally() {
        let mut search = case_sensitive_engine();
        search
            .start_search(r"\b[0-9a-f]{7,40}\b", FilterMode::Regex)
            .expect("compiles");
        let row = "commit 66390b5c8f2a1b3c ".repeat(170);
        assert!(
            search.scan_row(0, &row, 1) > 0,
            "a real pattern still finds its matches"
        );
        assert!(
            !search
                .compiled_regex
                .as_ref()
                .expect("compiled")
                .step_limit_exceeded()
        );
    }
}
