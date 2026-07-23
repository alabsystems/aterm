// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Lazy match iterators for forward and reverse search navigation.

use std::ops::Range;

use crate::bitmap::{SparseBitmap, SparseBitmapRange};

use super::index::SearchIndex;
use super::types::SearchMatch;
use crate::grapheme::ColumnMap;

/// Source of candidate line numbers for search iteration.
///
/// For short queries (< 3 chars), uses a lazy range to avoid O(n) allocation.
/// For trigram queries, uses a lazy owned bitmap iterator in either direction.
/// `BTreeSet`'s consuming iterator is double-ended, so backward navigation does
/// not need to collect or sort candidate line IDs.
pub(super) enum CandidateSource<'a> {
    /// Empty source.
    Empty,
    /// Borrowed posting-list range with lazy membership checks in the other
    /// query posting lists. Used by case-insensitive forward search.
    FilteredForward {
        /// Smallest posting list, range-bounded at the navigation cursor.
        candidates: SparseBitmapRange<'a>,
        /// Remaining posting lists that every yielded candidate must occupy.
        filters: Vec<&'a SparseBitmap>,
    },
    /// Reverse counterpart of [`FilteredForward`](Self::FilteredForward).
    FilteredBackward {
        /// Smallest posting list, range-bounded at the navigation cursor.
        candidates: SparseBitmapRange<'a>,
        /// Remaining posting lists that every yielded candidate must occupy.
        filters: Vec<&'a SparseBitmap>,
    },
    /// Lazy ascending range of line numbers (for short-query forward search).
    Range(Range<u32>),
    /// Lazy descending range of line numbers (for short-query backward search).
    RangeRev(std::iter::Rev<Range<u32>>),
}

impl<'a> CandidateSource<'a> {
    /// Build a lazy ascending intersection from borrowed posting lists.
    pub(super) fn from_postings_forward(
        mut postings: Vec<&'a SparseBitmap>,
        from_line: u32,
    ) -> Self {
        if postings.is_empty() {
            return Self::Empty;
        }
        postings.sort_unstable_by_key(|bitmap| bitmap.len());
        let primary = postings.remove(0);
        Self::FilteredForward {
            candidates: primary.range_from(from_line),
            filters: postings,
        }
    }

    /// Build a lazy descending intersection from borrowed posting lists.
    pub(super) fn from_postings_backward(
        mut postings: Vec<&'a SparseBitmap>,
        before_line: u32,
    ) -> Self {
        if postings.is_empty() {
            return Self::Empty;
        }
        postings.sort_unstable_by_key(|bitmap| bitmap.len());
        let primary = postings.remove(0);
        Self::FilteredBackward {
            candidates: primary.range_before(before_line),
            filters: postings,
        }
    }

    /// Get the next candidate line number.
    #[inline]
    pub(super) fn next_candidate(&mut self) -> Option<u32> {
        match self {
            Self::Empty => None,
            Self::FilteredForward {
                candidates,
                filters,
            } => candidates
                .find(|&&candidate| filters.iter().all(|bitmap| bitmap.contains(candidate)))
                .copied(),
            Self::FilteredBackward {
                candidates,
                filters,
            } => candidates
                .rfind(|&&candidate| filters.iter().all(|bitmap| bitmap.contains(candidate)))
                .copied(),
            Self::Range(range) => range.next(),
            Self::RangeRev(rev) => rev.next(),
        }
    }
}

/// Lazy iterator over search matches with early termination support.
///
/// This iterator yields matches one at a time without collecting all matches
/// first. Combined with range queries on the underlying bitmap, this enables
/// O(log n) search for find_next/find_prev operations.
pub(crate) struct SearchMatchIterator<'a> {
    /// The search index.
    index: &'a SearchIndex,
    /// The query string.
    query: &'a str,
    /// Candidate line numbers source.
    candidates: CandidateSource<'a>,
    /// Candidate line currently being verified, if any.
    current_line: Option<usize>,
    /// Next byte boundary to search on the current line.
    next_byte: usize,
}

impl<'a> SearchMatchIterator<'a> {
    /// Create a new match iterator from a candidate source.
    pub(super) fn new(
        index: &'a SearchIndex,
        query: &'a str,
        candidates: CandidateSource<'a>,
    ) -> Self {
        Self {
            index,
            query,
            candidates,
            current_line: None,
            next_byte: 0,
        }
    }
}

impl Iterator for SearchMatchIterator<'_> {
    type Item = SearchMatch;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(line_num) = self.current_line
                && let Some(text) = self.index.lines.get(&line_num)
            {
                let fallback;
                let col_map = match self.index.column_maps.get(&line_num) {
                    Some(map) => map,
                    None => {
                        fallback = ColumnMap::new(text);
                        &fallback
                    }
                };
                while let Some(tail) = text.get(self.next_byte..) {
                    let Some(relative) = tail.find(self.query) else {
                        break;
                    };
                    let abs_pos = self.next_byte.checked_add(relative)?;
                    let match_end = abs_pos.checked_add(self.query.len())?;
                    let step = text
                        .get(abs_pos..)
                        .and_then(|suffix| suffix.chars().next())
                        .map_or(1, char::len_utf8);
                    self.next_byte = abs_pos.checked_add(step)?;
                    let start_col = col_map.byte_to_column(abs_pos);
                    let end_col = col_map.byte_to_column(match_end);
                    if start_col != end_col {
                        return Some(SearchMatch::new(line_num, start_col, end_col));
                    }
                }
            }
            self.current_line = self
                .candidates
                .next_candidate()
                .map(|line_u32| line_u32 as usize);
            self.current_line?;
            self.next_byte = 0;
            #[cfg(test)]
            super::index::count_search_from_line_candidate();
        }
    }
}

/// Reverse iterator over search matches.
///
/// Yields matches in reverse order (newest to oldest, right to left).
pub(crate) struct SearchMatchReverseIterator<'a> {
    /// The search index.
    index: &'a SearchIndex,
    /// The query string.
    query: &'a str,
    /// Candidate line numbers source (yields in descending order).
    candidates: CandidateSource<'a>,
    /// Candidate line currently being verified, if any.
    current_line: Option<usize>,
    /// Exclusive byte-start boundary for the next overlapping reverse match.
    before_byte: usize,
}

impl<'a> SearchMatchReverseIterator<'a> {
    /// Create a new reverse match iterator.
    pub(super) fn new(
        index: &'a SearchIndex,
        query: &'a str,
        candidates: CandidateSource<'a>,
    ) -> Self {
        Self {
            index,
            query,
            candidates,
            current_line: None,
            before_byte: usize::MAX,
        }
    }
}

impl Iterator for SearchMatchReverseIterator<'_> {
    type Item = SearchMatch;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(line_num) = self.current_line
                && self.before_byte != 0
                && let Some(text) = self.index.lines.get(&line_num)
            {
                let query_len = self.query.len();
                let prefix_end = text
                    .len()
                    .min(self.before_byte.saturating_add(query_len.saturating_sub(1)));
                let abs_pos = memchr::memmem::rfind(
                    text.as_bytes().get(..prefix_end)?,
                    self.query.as_bytes(),
                );
                if let Some(abs_pos) = abs_pos {
                    self.before_byte = abs_pos;
                    let match_end = abs_pos.checked_add(query_len)?;
                    let fallback;
                    let col_map = match self.index.column_maps.get(&line_num) {
                        Some(map) => map,
                        None => {
                            fallback = ColumnMap::new(text);
                            &fallback
                        }
                    };
                    let start_col = col_map.byte_to_column(abs_pos);
                    let end_col = col_map.byte_to_column(match_end);
                    if start_col != end_col {
                        return Some(SearchMatch::new(line_num, start_col, end_col));
                    }
                    continue;
                }
            }
            self.current_line = self
                .candidates
                .next_candidate()
                .map(|line_u32| line_u32 as usize);
            self.current_line?;
            self.before_byte = usize::MAX;
            #[cfg(test)]
            super::index::count_search_from_line_candidate();
        }
    }
}
