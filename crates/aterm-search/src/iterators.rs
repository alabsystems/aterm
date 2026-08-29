// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Lazy match iterators for forward and reverse search navigation.

use std::ops::Range;

use crate::bitmap::SparseBitmap;

use super::index::SearchIndex;
use super::types::SearchMatch;
use crate::bytesearch::Searcher;
use crate::grapheme::ColumnMap;

/// Source of candidate line numbers for search iteration.
///
/// For short queries (< 3 chars), uses a lazy range to avoid O(n) allocation.
/// For trigram queries, decodes the involved posting lists ONCE (a transient
/// per-query working set) and drives navigation off the smallest decoded list,
/// membership-checking the rest with a binary search. The compressed store is
/// not seekable, so the decode is what makes both directions — and the
/// double-ended reverse walk — cheap without a per-probe decode. The driver is
/// picked by the O(1) compressed count and range-bounded BEFORE the filters are
/// decoded, so a bound that empties it costs no decode at all.
pub(super) enum CandidateSource {
    /// Empty source.
    Empty,
    /// Smallest decoded posting list, range-bounded at the navigation cursor,
    /// with lazy membership checks in the other query posting lists. Used by
    /// case-insensitive forward search.
    FilteredForward {
        /// Smallest posting list (decoded), range-bounded at the cursor.
        candidates: std::vec::IntoIter<u32>,
        /// Remaining posting lists (decoded ascending) every yielded candidate
        /// must occupy — probed by binary search.
        filters: Vec<Vec<u32>>,
    },
    /// Reverse counterpart of [`FilteredForward`](Self::FilteredForward).
    FilteredBackward {
        /// Smallest posting list (decoded), range-bounded at the cursor.
        candidates: std::vec::IntoIter<u32>,
        /// Remaining posting lists (decoded ascending) every yielded candidate
        /// must occupy — probed by binary search.
        filters: Vec<Vec<u32>>,
    },
    /// Lazy ascending range of line numbers (for short-query forward search).
    Range(Range<u32>),
    /// Lazy descending range of line numbers (for short-query backward search).
    RangeRev(std::iter::Rev<Range<u32>>),
}

/// Split the smallest posting list off as the navigation driver, decoding ONLY
/// that one and leaving the rest borrowed. The candidate SET (values present in
/// EVERY list) is independent of which list drives, so choosing the smallest is
/// a pure cost optimization that preserves byte-identical results.
///
/// `SparseBitmap::len()` is O(1) on the compressed form (it reads the cached
/// count), so the driver is identified without decoding anything — the same
/// sort-by-len-first trick `SearchIndex::intersect_trigrams` already uses. The
/// remaining lists stay compressed so the caller can range-bound the driver
/// FIRST and skip decoding them entirely when the bound leaves nothing to
/// probe.
fn split_smallest_first(
    mut postings: Vec<&SparseBitmap>,
) -> Option<(Vec<u32>, Vec<&SparseBitmap>)> {
    if postings.is_empty() {
        return None;
    }
    // Ascending by size: the driver is the shortest list to walk, and the
    // filters keep the cheapest rejector first for the `.all()` probe in
    // `next_candidate`.
    postings.sort_unstable_by_key(|bitmap| bitmap.len());
    let primary = postings.remove(0).to_vec();
    Some((primary, postings))
}

impl CandidateSource {
    /// Build a lazy ascending intersection from borrowed posting lists.
    pub(super) fn from_postings_forward(postings: Vec<&SparseBitmap>, from_line: u32) -> Self {
        let Some((mut primary, rest)) = split_smallest_first(postings) else {
            return Self::Empty;
        };
        let start = primary.partition_point(|&v| v < from_line);
        primary.drain(..start);
        // Bound the driver BEFORE decoding the filters. The filters are only
        // ever membership-probed by a yielded candidate, so with no candidates
        // left (an anchored find past the last hit, or the wrap leg of a
        // directed find) their decode is pure waste — hundreds of KB of
        // `Vec<u32>` allocated and dropped untouched for a common trigram over a
        // deep index. An empty `FilteredForward` and `Empty` are
        // indistinguishable through `next_candidate`, the enum's only method.
        if primary.is_empty() {
            return Self::Empty;
        }
        let filters: Vec<Vec<u32>> = rest.into_iter().map(SparseBitmap::to_vec).collect();
        Self::FilteredForward {
            candidates: primary.into_iter(),
            filters,
        }
    }

    /// Build a lazy descending intersection from borrowed posting lists.
    pub(super) fn from_postings_backward(postings: Vec<&SparseBitmap>, before_line: u32) -> Self {
        let Some((mut primary, rest)) = split_smallest_first(postings) else {
            return Self::Empty;
        };
        let end = primary.partition_point(|&v| v < before_line);
        primary.truncate(end);
        // Bound first, decode second — see `from_postings_forward`.
        if primary.is_empty() {
            return Self::Empty;
        }
        let filters: Vec<Vec<u32>> = rest.into_iter().map(SparseBitmap::to_vec).collect();
        Self::FilteredBackward {
            candidates: primary.into_iter(),
            filters,
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
            } => candidates.find(|candidate| {
                filters
                    .iter()
                    .all(|list| list.binary_search(candidate).is_ok())
            }),
            Self::FilteredBackward {
                candidates,
                filters,
            } => candidates.rfind(|candidate| {
                filters
                    .iter()
                    .all(|list| list.binary_search(candidate).is_ok())
            }),
            Self::Range(range) => range.next(),
            Self::RangeRev(rev) => rev.next(),
        }
    }
}

/// Advance a case-sensitive literal sweep of `text` from byte `from_byte`,
/// yielding the next match plus the byte offset to resume the sweep at.
///
/// This is the SINGLE source of the forward literal scan: the batch path drives
/// it across the index's candidate lines via [`SearchMatchIterator`], and the
/// budgeted engine drives it over the one row it just indexed. Keeping one body
/// is what makes budgeted results equal to one-shot results by construction
/// rather than by reimplementation.
///
/// The needle arrives as a PREPARED [`Searcher`] rather than a `&str` because
/// this is called once per match: preparing it here would recompute the
/// needle's critical factorization for every occurrence on the line.
///
/// Semantics (unchanged from the loop this was extracted from): byte spans that
/// resolve to zero display columns are skipped, and the resume offset advances
/// by ONE character past the match start so overlapping matches are preserved.
#[inline]
pub(crate) fn next_literal_match(
    line: usize,
    text: &str,
    searcher: &Searcher<'_>,
    col_map: &ColumnMap,
    from_byte: usize,
) -> Option<(SearchMatch, usize)> {
    let mut next_byte = from_byte;
    while let Some(tail) = text.get(next_byte..) {
        // E9b: the crate's own Two-Way substring search for the forward literal
        // verify, matching the reverse iterator's `rfind`. Byte-identical to
        // `tail.find(query)`: a
        // byte-aligned occurrence of a valid-UTF-8 needle in a valid-UTF-8
        // haystack is necessarily char-aligned, so the offset is the same one
        // str::find's two-way scan returns — just found with the faster scanner.
        let relative = searcher.find_in(tail.as_bytes())?;
        let abs_pos = next_byte.checked_add(relative)?;
        let match_end = abs_pos.checked_add(searcher.needle().len())?;
        let step = text
            .get(abs_pos..)
            .and_then(|suffix| suffix.chars().next())
            .map_or(1, char::len_utf8);
        next_byte = abs_pos.checked_add(step)?;
        let start_col = col_map.byte_to_column(abs_pos);
        let end_col = col_map.byte_to_column(match_end);
        if start_col != end_col {
            return Some((SearchMatch::new(line, start_col, end_col), next_byte));
        }
    }
    None
}

/// Lazy iterator over search matches with early termination support.
///
/// This iterator yields matches one at a time without collecting all matches
/// first. Combined with range queries on the underlying bitmap, this enables
/// O(log n) search for find_next/find_prev operations.
pub(crate) struct SearchMatchIterator<'a> {
    /// The search index.
    index: &'a SearchIndex,
    /// The query, prepared once for the whole walk.
    searcher: Searcher<'a>,
    /// Candidate line numbers source.
    candidates: CandidateSource,
    /// Candidate line currently being verified, if any.
    current_line: Option<usize>,
    /// Next byte boundary to search on the current line.
    next_byte: usize,
}

impl<'a> SearchMatchIterator<'a> {
    /// Create a new match iterator from a candidate source.
    pub(super) fn new(index: &'a SearchIndex, query: &'a str, candidates: CandidateSource) -> Self {
        Self {
            index,
            searcher: Searcher::new(query.as_bytes()),
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
                if let Some((found, resume)) =
                    next_literal_match(line_num, text, &self.searcher, col_map, self.next_byte)
                {
                    self.next_byte = resume;
                    return Some(found);
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
    /// The query, prepared once for the whole walk.
    ///
    /// One preparation per query, not per match: this iterator calls
    /// [`Searcher::rfind_in`] once for every occurrence on a line, and each of
    /// those used to re-derive the needle's critical factorization.
    searcher: Searcher<'a>,
    /// Candidate line numbers source (yields in descending order).
    candidates: CandidateSource,
    /// Candidate line currently being verified, if any.
    current_line: Option<usize>,
    /// Exclusive byte-start boundary for the next overlapping reverse match.
    before_byte: usize,
}

impl<'a> SearchMatchReverseIterator<'a> {
    /// Create a new reverse match iterator.
    pub(super) fn new(index: &'a SearchIndex, query: &'a str, candidates: CandidateSource) -> Self {
        Self {
            index,
            searcher: Searcher::new(query.as_bytes()),
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
                let query_len = self.searcher.needle().len();
                let prefix_end = text
                    .len()
                    .min(self.before_byte.saturating_add(query_len.saturating_sub(1)));
                // A REVERSE scan, so this stops at the first match to the left
                // of the cursor rather than walking the line from 0 to find it:
                // the whole reverse walk of a line costs one pass over it, not
                // one pass per match.
                let abs_pos = self.searcher.rfind_in(text.as_bytes().get(..prefix_end)?);
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

#[cfg(test)]
mod memmem_forward_tests {
    use crate::SearchIndex;

    /// E9b: the forward case-sensitive literal path (now memmem-backed) yields
    /// exactly what a str::find reference scan would — same lines, same start
    /// columns, same order — including overlapping matches and a multi-byte
    /// UTF-8 needle. Guards the memmem swap against any offset/boundary drift.
    #[test]
    fn forward_literal_matches_equal_a_str_find_reference() {
        let lines = ["BANANA split", "aXaXa here", "café au café", "no hit"];
        let mut index = SearchIndex::new();
        for (i, text) in lines.iter().enumerate() {
            index.index_line(i, text);
        }
        for needle in ["ANA", "aX", "café", "X", "z"] {
            // Reference: str::find sweep per line, column = char index of the
            // byte offset, advancing one char to preserve overlaps.
            let mut reference: Vec<(usize, usize)> = Vec::new();
            for (line, text) in lines.iter().enumerate() {
                let mut start = 0;
                while let Some(rel) = text[start..].find(needle) {
                    let byte = start + rel;
                    let col = text[..byte].chars().count();
                    reference.push((line, col));
                    let step = text[byte..].chars().next().map_or(1, char::len_utf8);
                    start = byte + step;
                }
            }
            let got: Vec<(usize, usize)> = index
                .search_with_positions(needle)
                .into_iter()
                .map(|m| (m.line, m.start_col))
                .collect();
            assert_eq!(got, reference, "needle {needle:?}");
        }
    }
}
