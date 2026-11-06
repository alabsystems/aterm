// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Lazy match iterators for forward and reverse search navigation.

use std::ops::Range;

use crate::bitmap::{PostingCursor, SparseBitmap};

use super::index::SearchIndex;
use super::types::SearchMatch;
use crate::bytesearch::{Matches, Searcher};
use crate::grapheme::ColumnMap;

#[cfg(test)]
thread_local! {
    static LITERAL_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn take_literal_visits() -> usize {
    LITERAL_VISITS.with(|count| count.replace(0))
}

/// Yields candidate line numbers by walking posting keys offset from `base`.
pub(super) struct CandidateSource<'a> {
    base: usize,
    keys: CandidateKeys<'a>,
}

enum CandidateKeys<'a> {
    Empty,
    Listed(std::vec::IntoIter<u32>),
    ListedRev(std::iter::Rev<std::vec::IntoIter<u32>>),
    Postings {
        cursors: Vec<PostingCursor<'a>>,
        backward: bool,
    },
    Range(Range<u32>),
    RangeRev(std::iter::Rev<Range<u32>>),
}

impl<'a> CandidateSource<'a> {
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

    /// The sorted `keys`, ascending.
    #[inline]
    pub(super) fn listed(base: usize, keys: Vec<u32>) -> Self {
        Self {
            base,
            keys: CandidateKeys::Listed(keys.into_iter()),
        }
    }

    /// The sorted `keys`, descending.
    #[inline]
    pub(super) fn listed_rev(base: usize, keys: Vec<u32>) -> Self {
        Self {
            base,
            keys: CandidateKeys::ListedRev(keys.into_iter().rev()),
        }
    }

    /// Builds a lazy ascending intersection from borrowed posting lists starting at `from_key`.
    pub(super) fn from_postings_forward(
        base: usize,
        postings: Vec<&'a SparseBitmap>,
        from_key: u32,
    ) -> Self {
        Self::postings(base, postings, from_key, false)
    }

    /// Builds a lazy descending intersection from borrowed posting lists below `before_key`.
    pub(super) fn from_postings_backward(
        base: usize,
        postings: Vec<&'a SparseBitmap>,
        before_key: u32,
    ) -> Self {
        Self::postings(base, postings, before_key, true)
    }

    fn postings(
        base: usize,
        mut postings: Vec<&'a SparseBitmap>,
        boundary: u32,
        backward: bool,
    ) -> Self {
        if postings.is_empty() || postings.iter().any(|p| p.is_empty()) {
            return Self::empty();
        }
        postings.sort_unstable_by_key(|p| p.len());
        let cursors = postings
            .into_iter()
            .map(|posting| {
                let mut cursor = posting.cursor();
                if backward {
                    cursor.seek_backward(boundary);
                } else {
                    cursor.seek_forward(boundary);
                }
                cursor
            })
            .collect();
        Self {
            base,
            keys: CandidateKeys::Postings { cursors, backward },
        }
    }

    #[inline]
    pub(super) fn next_candidate(&mut self) -> Option<usize> {
        let key = match &mut self.keys {
            CandidateKeys::Empty => None,
            CandidateKeys::Listed(keys) => keys.next(),
            CandidateKeys::ListedRev(keys) => keys.next(),
            CandidateKeys::Postings { cursors, backward } => next_intersection(cursors, *backward),
            CandidateKeys::Range(range) => range.next(),
            CandidateKeys::RangeRev(rev) => rev.next(),
        }?;
        Some(self.base.saturating_add(key as usize))
    }
}

/// Merge compressed lists monotonically, retaining the unmatched value in each cursor.
fn next_intersection(cursors: &mut [PostingCursor<'_>], backward: bool) -> Option<u32> {
    let mut target = cursors.first()?.peek(backward)?;
    'align: loop {
        for cursor in cursors.iter_mut() {
            if backward {
                if let Some(exclusive) = target.checked_add(1) {
                    cursor.seek_backward(exclusive);
                }
            } else {
                cursor.seek_forward(target);
            }
            let value = cursor.peek(backward)?;
            if value != target {
                target = value;
                continue 'align;
            }
        }
        for cursor in cursors {
            if backward {
                cursor.next_back();
            } else {
                cursor.next();
            }
        }
        return Some(target);
    }
}

/// Map a textual occurrence to display columns, retaining even a zero-column span.
#[inline]
pub(crate) fn literal_occurrence(
    line: usize,
    at: usize,
    query_len: usize,
    col_map: &ColumnMap,
) -> SearchMatch {
    #[cfg(test)]
    LITERAL_VISITS.with(|count| count.set(count.get() + 1));
    let (start_col, end_col) = col_map.span(at, at.saturating_add(query_len));
    SearchMatch::new(line, start_col, end_col)
}

/// Lazy ascending iterator over search matches.
pub(crate) type SearchMatchIterator<'a> = LiteralMatchIterator<'a, false>;

/// Lazy descending iterator over search matches.
pub(crate) type SearchMatchReverseIterator<'a> = LiteralMatchIterator<'a, true>;

/// Lazy literal matches over candidate lines, with resumable byte walks in either direction.
pub(crate) struct LiteralMatchIterator<'a, const REV: bool> {
    index: &'a SearchIndex,
    searcher: Searcher<'a>,
    candidates: CandidateSource<'a>,
    current_line: Option<usize>,
    current_matches: Option<Matches<'a, 'a, REV>>,
    anchor: Option<(usize, usize, bool)>,
    current_offset: usize,
}

impl<'a, const REV: bool> LiteralMatchIterator<'a, REV> {
    #[inline]
    pub(super) const fn new(
        index: &'a SearchIndex,
        query: &'a str,
        candidates: CandidateSource<'a>,
    ) -> Self {
        Self {
            index,
            searcher: Searcher::new(query.as_bytes()),
            candidates,
            current_line: None,
            current_matches: None,
            anchor: None,
            current_offset: 0,
        }
    }

    /// Bound the anchor row's byte walk when its columns are the identity map.
    pub(crate) const fn with_anchor(mut self, line: usize, col: usize, inclusive: bool) -> Self {
        self.anchor = Some((line, col, inclusive));
        self
    }
}

impl<const REV: bool> Iterator for LiteralMatchIterator<'_, REV> {
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
                if let Some(matches) = &mut self.current_matches {
                    for at in matches.by_ref() {
                        let found = literal_occurrence(
                            line_num,
                            self.current_offset.saturating_add(at),
                            self.searcher.needle().len(),
                            col_map,
                        );
                        if found.start_col != found.end_col {
                            return Some(found);
                        }
                    }
                }
            }
            self.current_line = self.candidates.next_candidate();
            self.current_line?;
            self.current_matches = self
                .current_line
                .and_then(|line| self.index.lines.get(&line))
                .map(|text| {
                    let mut first = 0;
                    let mut end = text.len();
                    if let Some((line, col, inclusive)) = self.anchor
                        && self.current_line == Some(line)
                        && let Some(map) = self.index.column_maps.get(&line)
                        && let Some(window) = map.identity_match_window::<REV>(
                            text.len(),
                            col,
                            inclusive,
                            self.searcher.needle().len(),
                        )
                    {
                        first = window.start;
                        end = window.end;
                    }
                    self.current_offset = first;
                    self.searcher
                        .matches_in::<REV>(&text.as_bytes()[first..end])
                });
            #[cfg(test)]
            super::index::count_search_from_line_candidate();
        }
    }
}

#[cfg(test)]
mod memmem_forward_tests {
    use crate::SearchIndex;

    #[test]
    fn compressed_intersections_match_set_reference_at_every_boundary() {
        use super::CandidateSource;
        use crate::bitmap::SparseBitmap;
        for seed in 0..24u32 {
            let lists: Vec<_> = (0..4)
                .map(|i| {
                    let mut list = SparseBitmap::new();
                    for key in 0..256u32 {
                        if (key.wrapping_mul(17 + i) + seed) % (i + 2) != 0 {
                            list.insert(key.saturating_mul(16_383));
                        }
                    }
                    list.drop_below(seed.saturating_mul(16_383));
                    list
                })
                .collect();
            for count in 1..=lists.len() {
                let reference: Vec<_> = lists[0]
                    .to_vec()
                    .into_iter()
                    .filter(|key| lists[1..count].iter().all(|list| list.contains(*key)))
                    .collect();
                for boundary in [0, 1, seed * 16_383, 100 * 16_383, u32::MAX] {
                    let mut forward = CandidateSource::from_postings_forward(
                        99,
                        lists[..count].iter().collect(),
                        boundary,
                    );
                    let mut backward = CandidateSource::from_postings_backward(
                        99,
                        lists[..count].iter().collect(),
                        boundary,
                    );
                    let got: Vec<_> = std::iter::from_fn(|| forward.next_candidate()).collect();
                    let want: Vec<_> = reference
                        .iter()
                        .filter(|&&v| v >= boundary)
                        .map(|&v| 99 + v as usize)
                        .collect();
                    assert_eq!(got, want);
                    let got: Vec<_> = std::iter::from_fn(|| backward.next_candidate()).collect();
                    let want: Vec<_> = reference
                        .iter()
                        .rev()
                        .filter(|&&v| v < boundary)
                        .map(|&v| 99 + v as usize)
                        .collect();
                    assert_eq!(got, want);
                }
            }
        }
    }

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
