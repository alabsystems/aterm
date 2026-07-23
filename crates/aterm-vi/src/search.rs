// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Search match navigation for vi mode n/N motions.
//!
//! Stores a sorted list of match start positions and provides circular
//! focus navigation (next wraps to first, previous wraps to last).
//! The actual search engine lives elsewhere (e.g. `aterm-search` crate);
//! this module handles only cursor navigation through results.

use super::types::ViPoint;

/// Search match state for vi mode n/N navigation.
///
/// Match positions are stored sorted in document order. The caller
/// populates matches via [`set_matches`](Self::set_matches); this
/// struct handles circular navigation with [`focus_next`](Self::focus_next)
/// and [`focus_prev`](Self::focus_prev).
#[derive(Debug, Clone, Default)]
pub struct ViSearchState {
    /// Match start positions, sorted in document order.
    matches: Vec<ViPoint>,
    /// Index of the currently focused match.
    focused: Option<usize>,
}

impl ViSearchState {
    /// Create an empty search state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the match positions. Must be sorted in document order:
    /// [`focus_next`](Self::focus_next)/[`focus_prev`](Self::focus_prev)
    /// binary-search the list, so unsorted input yields wrong navigation
    /// (no memory unsafety). No debug_assert here: a caller-contract
    /// assert is a reachable panic the L0 gate cannot prove away.
    pub fn set_matches(&mut self, matches: Vec<ViPoint>) {
        self.focused = None;
        self.matches = matches;
    }

    /// Clear all matches and reset focus.
    pub fn clear(&mut self) {
        self.matches.clear();
        self.focused = None;
    }

    /// Number of matches.
    #[must_use]
    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// Whether there are any matches.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }

    /// Currently focused match index.
    #[must_use]
    pub fn focused_index(&self) -> Option<usize> {
        self.focused
    }

    /// Currently focused match position.
    #[must_use]
    pub fn focused_point(&self) -> Option<ViPoint> {
        // Written as a match (not `and_then`) so the verifier does not have
        // to model a `&self`-capturing closure aggregate (unsupported MIR).
        match self.focused {
            Some(i) => self.matches.get(i).copied(),
            None => None,
        }
    }

    /// Focus the next match after `after`. Wraps to the first match
    /// if no match exists past the cursor.
    pub fn focus_next(&mut self, after: ViPoint) -> Option<ViPoint> {
        if self.matches.is_empty() {
            return None;
        }

        // Binary search: first element where *m > after.
        let idx = self.matches.partition_point(|m| *m <= after);
        let idx = if idx >= self.matches.len() { 0 } else { idx };

        self.focused = Some(idx);
        self.matches.get(idx).copied()
    }

    /// Focus the previous match before `before`. Wraps to the last
    /// match if no match exists before the cursor.
    pub fn focus_prev(&mut self, before: ViPoint) -> Option<ViPoint> {
        if self.matches.is_empty() {
            return None;
        }

        // Binary search: first element where *m >= before.
        let idx = self.matches.partition_point(|m| *m < before);
        // Both saturating_subs are exact: the early return above guarantees
        // `len() >= 1`, and the else branch runs only when `idx >= 1`.
        let idx = if idx == 0 {
            self.matches.len().saturating_sub(1)
        } else {
            idx.saturating_sub(1)
        };

        self.focused = Some(idx);
        self.matches.get(idx).copied()
    }

    /// Check if a point is a match start position.
    #[must_use]
    pub fn is_match(&self, point: ViPoint) -> bool {
        self.matches.binary_search(&point).is_ok()
    }

    /// Check if a point is the currently focused match.
    #[must_use]
    pub fn is_focused(&self, point: ViPoint) -> bool {
        self.focused_point() == Some(point)
    }
}

#[cfg(kani)]
#[path = "search_kani_proofs.rs"]
mod kani_proofs;

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_matches() -> Vec<ViPoint> {
        vec![
            ViPoint::new(0, 5),
            ViPoint::new(1, 10),
            ViPoint::new(3, 0),
            ViPoint::new(3, 20),
        ]
    }

    #[test]
    fn test_focus_next_basic() {
        let mut s = ViSearchState::new();
        s.set_matches(sample_matches());

        // Cursor before all matches: should focus first.
        let result = s.focus_next(ViPoint::new(0, 0));
        assert_eq!(result, Some(ViPoint::new(0, 5)));
        assert_eq!(s.focused_index(), Some(0));
    }

    #[test]
    fn test_focus_next_between_matches() {
        let mut s = ViSearchState::new();
        s.set_matches(sample_matches());

        // Cursor between match 1 and match 2.
        let result = s.focus_next(ViPoint::new(2, 0));
        assert_eq!(result, Some(ViPoint::new(3, 0)));
        assert_eq!(s.focused_index(), Some(2));
    }

    #[test]
    fn test_focus_next_wraps_around() {
        let mut s = ViSearchState::new();
        s.set_matches(sample_matches());

        // Cursor after all matches: wraps to first.
        let result = s.focus_next(ViPoint::new(5, 0));
        assert_eq!(result, Some(ViPoint::new(0, 5)));
        assert_eq!(s.focused_index(), Some(0));
    }

    #[test]
    fn test_focus_next_on_match_advances() {
        let mut s = ViSearchState::new();
        s.set_matches(sample_matches());

        // Cursor exactly on a match: should advance to next.
        let result = s.focus_next(ViPoint::new(0, 5));
        assert_eq!(result, Some(ViPoint::new(1, 10)));
        assert_eq!(s.focused_index(), Some(1));
    }

    #[test]
    fn test_focus_prev_basic() {
        let mut s = ViSearchState::new();
        s.set_matches(sample_matches());

        // Cursor after all matches: should focus last.
        let result = s.focus_prev(ViPoint::new(5, 0));
        assert_eq!(result, Some(ViPoint::new(3, 20)));
        assert_eq!(s.focused_index(), Some(3));
    }

    #[test]
    fn test_focus_prev_between_matches() {
        let mut s = ViSearchState::new();
        s.set_matches(sample_matches());

        // Cursor between match 1 and match 2.
        let result = s.focus_prev(ViPoint::new(2, 0));
        assert_eq!(result, Some(ViPoint::new(1, 10)));
        assert_eq!(s.focused_index(), Some(1));
    }

    #[test]
    fn test_focus_prev_wraps_around() {
        let mut s = ViSearchState::new();
        s.set_matches(sample_matches());

        // Cursor before all matches: wraps to last.
        let result = s.focus_prev(ViPoint::new(0, 0));
        assert_eq!(result, Some(ViPoint::new(3, 20)));
        assert_eq!(s.focused_index(), Some(3));
    }

    #[test]
    fn test_focus_prev_on_match_retreats() {
        let mut s = ViSearchState::new();
        s.set_matches(sample_matches());

        // Cursor exactly on a match: should retreat to previous.
        let result = s.focus_prev(ViPoint::new(3, 0));
        assert_eq!(result, Some(ViPoint::new(1, 10)));
        assert_eq!(s.focused_index(), Some(1));
    }

    #[test]
    fn test_focus_empty_returns_none() {
        let mut s = ViSearchState::new();
        assert_eq!(s.focus_next(ViPoint::new(0, 0)), None);
        assert_eq!(s.focus_prev(ViPoint::new(0, 0)), None);
        assert_eq!(s.focused_index(), None);
    }

    #[test]
    fn test_is_match() {
        let mut s = ViSearchState::new();
        s.set_matches(sample_matches());

        assert!(s.is_match(ViPoint::new(0, 5)));
        assert!(s.is_match(ViPoint::new(3, 20)));
        assert!(!s.is_match(ViPoint::new(0, 6)));
        assert!(!s.is_match(ViPoint::new(2, 0)));
    }

    #[test]
    fn test_is_focused() {
        let mut s = ViSearchState::new();
        s.set_matches(sample_matches());

        assert!(!s.is_focused(ViPoint::new(0, 5)));

        s.focus_next(ViPoint::new(0, 0));
        assert!(s.is_focused(ViPoint::new(0, 5)));
        assert!(!s.is_focused(ViPoint::new(1, 10)));
    }

    #[test]
    fn test_clear_resets_state() {
        let mut s = ViSearchState::new();
        s.set_matches(sample_matches());
        s.focus_next(ViPoint::new(0, 0));

        s.clear();
        assert!(s.is_empty());
        assert_eq!(s.match_count(), 0);
        assert_eq!(s.focused_index(), None);
    }

    #[test]
    fn test_set_matches_resets_focus() {
        let mut s = ViSearchState::new();
        s.set_matches(sample_matches());
        s.focus_next(ViPoint::new(0, 0));
        assert_eq!(s.focused_index(), Some(0));

        s.set_matches(vec![ViPoint::new(10, 0)]);
        assert_eq!(s.focused_index(), None);
        assert_eq!(s.match_count(), 1);
    }

    #[test]
    fn test_single_match_wraps_to_self() {
        let mut s = ViSearchState::new();
        s.set_matches(vec![ViPoint::new(2, 5)]);

        // focus_next from before: finds it.
        assert_eq!(s.focus_next(ViPoint::new(0, 0)), Some(ViPoint::new(2, 5)));

        // focus_next from on the match: wraps to same (only match).
        assert_eq!(s.focus_next(ViPoint::new(2, 5)), Some(ViPoint::new(2, 5)));

        // focus_prev from after: finds it.
        assert_eq!(s.focus_prev(ViPoint::new(5, 0)), Some(ViPoint::new(2, 5)));

        // focus_prev from on the match: wraps to same (only match).
        assert_eq!(s.focus_prev(ViPoint::new(2, 5)), Some(ViPoint::new(2, 5)));
    }

    #[test]
    fn test_scrollback_matches() {
        let mut s = ViSearchState::new();
        s.set_matches(vec![
            ViPoint::new(-5, 3),  // scrollback
            ViPoint::new(-1, 10), // scrollback
            ViPoint::new(0, 0),   // visible top
            ViPoint::new(10, 5),  // visible
        ]);

        // Navigate from visible area into scrollback.
        let result = s.focus_prev(ViPoint::new(0, 0));
        assert_eq!(result, Some(ViPoint::new(-1, 10)));

        // Navigate from scrollback forward.
        let result = s.focus_next(ViPoint::new(-1, 10));
        assert_eq!(result, Some(ViPoint::new(0, 0)));
    }

    #[test]
    fn test_vi_point_ordering() {
        // Document order: negative lines first, then by column.
        assert!(ViPoint::new(-5, 0) < ViPoint::new(-1, 0));
        assert!(ViPoint::new(-1, 0) < ViPoint::new(0, 0));
        assert!(ViPoint::new(0, 0) < ViPoint::new(0, 5));
        assert!(ViPoint::new(0, 5) < ViPoint::new(1, 0));
    }
}
