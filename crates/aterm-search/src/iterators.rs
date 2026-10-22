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
/// Walks posting keys (`u32` offsets from the index's key base; see
/// `SearchIndex::make_room_for`) and yields line numbers. Built against the base
/// the index has then, and never outlives the borrow that froze it.
pub(super) struct CandidateSource {
    /// The line number key 0 stands for.
    base: usize,
    /// The keys, in the order they are yielded.
    keys: CandidateKeys,
}

/// The key walk behind a [`CandidateSource`].
///
/// Queries under 3 bytes walk a lazy range. Trigram queries decode the involved
/// posting lists once per query, drive off the smallest and binary-search the rest
/// (the compressed store is not seekable).
enum CandidateKeys {
    /// Empty source.
    Empty,
    /// Smallest decoded posting list, bounded at the cursor, filtered by membership
    /// in the other lists.
    FilteredForward {
        /// Smallest posting list (decoded), range-bounded at the cursor.
        candidates: std::vec::IntoIter<u32>,
        /// Remaining posting lists (decoded ascending), probed by binary search.
        filters: Vec<Vec<u32>>,
    },
    /// Reverse counterpart of [`FilteredForward`](Self::FilteredForward).
    FilteredBackward {
        /// Smallest posting list (decoded), range-bounded at the cursor.
        candidates: std::vec::IntoIter<u32>,
        /// Remaining posting lists (decoded ascending), probed by binary search.
        filters: Vec<Vec<u32>>,
    },
    /// Lazy ascending range of keys (for short-query forward search).
    Range(Range<u32>),
    /// Lazy descending range of keys (for short-query backward search).
    RangeRev(std::iter::Rev<Range<u32>>),
}

/// Split off the smallest posting list as the driver, decoding only it. The candidate
/// set (values in every list) does not depend on the driver, and `len()` is O(1) on
/// the compressed form. The rest stay compressed so the caller can bound the driver
/// first and skip decoding them when nothing is left.
fn split_smallest_first(
    mut postings: Vec<&SparseBitmap>,
) -> Option<(Vec<u32>, Vec<&SparseBitmap>)> {
    if postings.is_empty() {
        return None;
    }
    // Ascending by size: the shortest driver, and the cheapest rejector first in
    // `next_candidate`'s `.all()` probe.
    postings.sort_unstable_by_key(|bitmap| bitmap.len());
    let primary = postings.remove(0).to_vec();
    Some((primary, postings))
}

impl CandidateSource {
    /// A source that yields nothing.
    #[inline]
    pub(super) const fn empty() -> Self {
        Self {
            base: 0,
            keys: CandidateKeys::Empty,
        }
    }

    /// Every key in `keys`, ascending, as lines from `base`.
    #[inline]
    pub(super) const fn range(base: usize, keys: Range<u32>) -> Self {
        Self {
            base,
            keys: CandidateKeys::Range(keys),
        }
    }

    /// Every key in `keys`, descending, as lines from `base`.
    #[inline]
    pub(super) fn range_rev(base: usize, keys: Range<u32>) -> Self {
        Self {
            base,
            keys: CandidateKeys::RangeRev(keys.rev()),
        }
    }

    /// Build a lazy ascending intersection from borrowed posting lists,
    /// starting at key `from_key`.
    pub(super) fn from_postings_forward(
        base: usize,
        postings: Vec<&SparseBitmap>,
        from_key: u32,
    ) -> Self {
        let Some((mut primary, rest)) = split_smallest_first(postings) else {
            return Self::empty();
        };
        let start = primary.partition_point(|&v| v < from_key);
        primary.drain(..start);
        // Bound the driver before decoding the filters: with no candidates left (an
        // anchored find past the last hit, the wrap leg of a directed find) decoding
        // them would allocate hundreds of KB for nothing on a common trigram. An empty
        // `FilteredForward` and `Empty` read the same through `next_candidate`.
        if primary.is_empty() {
            return Self::empty();
        }
        let filters: Vec<Vec<u32>> = rest.into_iter().map(SparseBitmap::to_vec).collect();
        Self {
            base,
            keys: CandidateKeys::FilteredForward {
                candidates: primary.into_iter(),
                filters,
            },
        }
    }

    /// Build a lazy descending intersection from borrowed posting lists,
    /// below key `before_key`.
    pub(super) fn from_postings_backward(
        base: usize,
        postings: Vec<&SparseBitmap>,
        before_key: u32,
    ) -> Self {
        let Some((mut primary, rest)) = split_smallest_first(postings) else {
            return Self::empty();
        };
        let end = primary.partition_point(|&v| v < before_key);
        primary.truncate(end);
        // Bound first, decode second — see `from_postings_forward`.
        if primary.is_empty() {
            return Self::empty();
        }
        let filters: Vec<Vec<u32>> = rest.into_iter().map(SparseBitmap::to_vec).collect();
        Self {
            base,
            keys: CandidateKeys::FilteredBackward {
                candidates: primary.into_iter(),
                filters,
            },
        }
    }

    /// Get the next candidate line number.
    #[inline]
    pub(super) fn next_candidate(&mut self) -> Option<usize> {
        let key = match &mut self.keys {
            CandidateKeys::Empty => None,
            CandidateKeys::FilteredForward {
                candidates,
                filters,
            } => candidates.find(|candidate| {
                filters
                    .iter()
                    .all(|list| list.binary_search(candidate).is_ok())
            }),
            CandidateKeys::FilteredBackward {
                candidates,
                filters,
            } => candidates.rfind(|candidate| {
                filters
                    .iter()
                    .all(|list| list.binary_search(candidate).is_ok())
            }),
            CandidateKeys::Range(range) => range.next(),
            CandidateKeys::RangeRev(rev) => rev.next(),
        }?;
        Some(self.base.saturating_add(key as usize))
    }
}

/// Advance a case-sensitive literal sweep of `text` from byte `from_byte`, yielding
/// the next match and the byte offset to resume at.
///
/// The one forward literal scan: the batch path ([`SearchMatchIterator`]) and the
/// budgeted engine both drive it, so their results agree by construction. It takes
/// a prepared [`Searcher`] because it runs once per match. Spans of zero display
/// columns are skipped, and the sweep resumes one character past each match start,
/// so matches overlap.
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
        // A byte-aligned occurrence of a valid UTF-8 needle in valid UTF-8 is
        // char-aligned, so this is the offset `str::find` would return.
        let relative = searcher.find_in(tail.as_bytes())?;
        let abs_pos = next_byte.checked_add(relative)?;
        let match_end = abs_pos.checked_add(searcher.needle().len())?;
        let step = text
            .get(abs_pos..)
            .and_then(|suffix| suffix.chars().next())
            .map_or(1, char::len_utf8);
        next_byte = abs_pos.checked_add(step)?;
        let (start_col, end_col) = col_map.span(abs_pos, match_end);
        if start_col != end_col {
            return Some((SearchMatch::new(line, start_col, end_col), next_byte));
        }
    }
    None
}

/// Lazy ascending iterator over search matches, so a find-next stops at the first hit.
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
    #[inline]
    pub(super) const fn new(
        index: &'a SearchIndex,
        query: &'a str,
        candidates: CandidateSource,
    ) -> Self {
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

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(line_num) = self.current_line
                && let Some(text) = self.index.lines.get(&line_num)
            {
                let fallback;
                let col_map = match self.index.column_maps.get(&line_num) {
                    Some(map) => map,
                    None => {
                        fallback = ColumnMap::new(text, self.index.ambiguous_width);
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
            self.current_line = self.candidates.next_candidate();
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
    /// The query, prepared once for the whole walk: [`Searcher::rfind_in`]
    /// runs once per occurrence on a line.
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
    #[inline]
    pub(super) const fn new(
        index: &'a SearchIndex,
        query: &'a str,
        candidates: CandidateSource,
    ) -> Self {
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

    #[inline]
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
                // A reverse scan finds the nearest match left of the cursor directly,
                // so a line's whole reverse walk costs one pass over it.
                let abs_pos = self.searcher.rfind_in(text.as_bytes().get(..prefix_end)?);
                if let Some(abs_pos) = abs_pos {
                    self.before_byte = abs_pos;
                    let match_end = abs_pos.checked_add(query_len)?;
                    let fallback;
                    let col_map = match self.index.column_maps.get(&line_num) {
                        Some(map) => map,
                        None => {
                            fallback = ColumnMap::new(text, self.index.ambiguous_width);
                            &fallback
                        }
                    };
                    let (start_col, end_col) = col_map.span(abs_pos, match_end);
                    if start_col != end_col {
                        return Some(SearchMatch::new(line_num, start_col, end_col));
                    }
                    continue;
                }
            }
            self.current_line = self.candidates.next_candidate();
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

    /// The forward case-sensitive literal path yields what a `str::find` sweep does:
    /// same lines, columns and order, overlaps and a multi-byte needle included.
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
