// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::types::SearchState;
use super::StreamingSearch;

impl StreamingSearch {
    /// Verify INV-SEARCH-1: Current index is valid.
    #[must_use]
    pub(crate) fn verify_current_index_valid(&self) -> bool {
        self.current_index == 0 || self.current_index <= self.results.len()
    }

    /// Verify INV-SEARCH-2: Result positions are internally consistent.
    ///
    /// Requires `start_col < end_col` (strict inequality) and `match_len > 0`,
    /// matching the INV-SEARCH-2c Kani assertion. Zero-display-width matches
    /// (e.g., combining marks) must be filtered before insertion.
    #[must_use]
    pub(crate) fn verify_result_positions_valid(&self) -> bool {
        #[cfg(kani)]
        {
            let mut index = 0;
            while index < self.results.len() {
                let result = &self.results[index];
                if !(result.start_col < result.end_col
                    && result.match_len == result.end_col.saturating_sub(result.start_col))
                {
                    return false;
                }
                index += 1;
            }
            true
        }

        #[cfg(not(kani))]
        self.results.iter().all(|m| {
            m.start_col < m.end_col && m.match_len == m.end_col.saturating_sub(m.start_col)
        })
    }

    /// Verify INV-SEARCH-3: Memory bounded.
    #[must_use]
    pub(crate) fn verify_memory_bounded(&self) -> bool {
        self.results.len() <= self.config.max_results
    }

    /// Verify INV-SEARCH-4: No duplicate results.
    #[must_use]
    pub(crate) fn verify_no_duplicates(&self) -> bool {
        #[cfg(kani)]
        {
            let len = self.results.len();
            let mut left = 0;
            while left < len {
                let mut right = left + 1;
                while right < len {
                    if self.results[left].same_position(&self.results[right]) {
                        return false;
                    }
                    right += 1;
                }
                left += 1;
            }
            return true;
        }

        #[cfg(not(kani))]
        for (idx, left) in self.results.iter().enumerate() {
            for right in self.results.iter().skip(idx + 1) {
                if left.same_position(right) {
                    return false;
                }
            }
        }
        true
    }

    /// Verify INV-SEARCH-5: Scan progress consistent with state.
    #[must_use]
    pub(crate) fn verify_scan_progress_consistent(&self) -> bool {
        match self.state {
            SearchState::Idle => self.scan_progress == -1,
            SearchState::Searching => self.scan_progress >= 0,
            SearchState::HasResults | SearchState::NoResults => self.scan_progress == -1,
        }
    }

    /// Verify INV-SEARCH-6: Total matches >= stored results.
    #[must_use]
    pub(crate) fn verify_total_matches_consistent(&self) -> bool {
        self.total_matches >= self.results.len()
    }

    /// Verify all safety invariants.
    #[must_use]
    pub(crate) fn verify_all_invariants(&self) -> bool {
        self.verify_current_index_valid()
            && self.verify_result_positions_valid()
            && self.verify_memory_bounded()
            && self.verify_no_duplicates()
            && self.verify_scan_progress_consistent()
            && self.verify_total_matches_consistent()
    }
}
