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

/// Yields candidate line numbers by walking posting keys offset from `base`.
pub(super) struct CandidateSource {
    base: usize,
    keys: CandidateKeys,
}

enum CandidateKeys {
    Empty,
    FilteredForward {
        candidates: std::vec::IntoIter<u32>,
        filters: Vec<Vec<u32>>,
    },
    FilteredBackward {
        candidates: std::vec::IntoIter<u32>,
        filters: Vec<Vec<u32>>,
    },
    Range(Range<u32>),
    RangeRev(std::iter::Rev<Range<u32>>),
}

/// Splits off and decodes the smallest posting list as the driver, leaving the rest compressed.
fn split_smallest_first(
    mut postings: Vec<&SparseBitmap>,
) -> Option<(Vec<u32>, Vec<&SparseBitmap>)> {
    if postings.is_empty() {
        return None;
    }
    postings.sort_unstable_by_key(|bitmap| bitmap.len());
    let primary = postings.remove(0).to_vec();
    Some((primary, postings))
}

impl CandidateSource {
    #[inline]
    pub(super) const fn empty() -> Self {
        Self {
            base: 0,
            keys: CandidateKeys::Empty,
        }
    }

    #[inline]
    pub(super) const fn range(base: usize, keys: Range<u32>) -> Self {
        Self {
            base,
            keys: CandidateKeys::Range(keys),
        }
    }

    #[inline]
    pub(super) fn range_rev(base: usize, keys: Range<u32>) -> Self {
        Self {
            base,
            keys: CandidateKeys::RangeRev(keys.rev()),
        }
    }

    /// Builds a lazy ascending intersection from borrowed posting lists starting at `from_key`.
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

    /// Builds a lazy descending intersection from borrowed posting lists below `before_key`.
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

/// Advances a case-sensitive literal sweep of `text` from `from_byte`, yielding the next non-empty match and resume byte offset.
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

/// Lazy ascending iterator over search matches.
pub(crate) struct SearchMatchIterator<'a> {
    index: &'a SearchIndex,
    searcher: Searcher<'a>,
    candidates: CandidateSource,
    current_line: Option<usize>,
    next_byte: usize,
}

impl<'a> SearchMatchIterator<'a> {
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

/// Lazy descending iterator over search matches (newest to oldest, right to left).
pub(crate) struct SearchMatchReverseIterator<'a> {
    index: &'a SearchIndex,
    searcher: Searcher<'a>,
    candidates: CandidateSource,
    current_line: Option<usize>,
    before_byte: usize,
}

impl<'a> SearchMatchReverseIterator<'a> {
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

    #[test]
    fn forward_literal_matches_equal_a_str_find_reference() {
        let lines = ["BANANA split", "aXaXa here", "café au café", "no hit"];
        let mut index = SearchIndex::new();
        for (i, text) in lines.iter().enumerate() {
            index.index_line(i, text);
        }
        for needle in ["ANA", "aX", "café", "X", "z"] {
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
