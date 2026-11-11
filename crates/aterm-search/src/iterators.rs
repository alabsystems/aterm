// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Lazy match iterators for forward and reverse search navigation.

use std::borrow::Cow;
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
    // One positioned list is already the intersection; no alignment is needed.
    if let [cursor] = cursors {
        return if backward {
            cursor.next_back()
        } else {
            cursor.next()
        };
    }
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

/// The borrowed match walk and its columns always describe the same candidate line.
struct LiteralLine<'a, const REV: bool> {
    line: usize,
    columns: Cow<'a, ColumnMap>,
    matches: Matches<'a, 'a, REV>,
    offset: usize,
}

/// Lazy literal matches over candidate lines, with resumable byte walks in either direction.
pub(crate) struct LiteralMatchIterator<'a, const REV: bool> {
    index: &'a SearchIndex,
    searcher: Searcher<'a>,
    candidates: CandidateSource<'a>,
    current: Option<LiteralLine<'a, REV>>,
    anchor: Option<(usize, usize, bool)>,
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
            current: None,
            anchor: None,
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
            if let Some(current) = &mut self.current {
                for at in current.matches.by_ref() {
                    let found = literal_occurrence(
                        current.line,
                        current.offset.saturating_add(at),
                        self.searcher.needle().len(),
                        &current.columns,
                    );
                    if found.start_col != found.end_col {
                        return Some(found);
                    }
                }
            }
            let Some(line_num) = self.candidates.next_candidate() else {
                self.current = None;
                return None;
            };
            if let Some(text) = self.index.lines.get(&line_num) {
                // The index is immutably borrowed for the iterator's lifetime. Retain its
                // map while walking this line, including across yielded occurrences.
                let map = self.index.column_maps.get(&line_num).map_or_else(
                    || Cow::Owned(ColumnMap::new(text, self.index.ambiguous_width)),
                    Cow::Borrowed,
                );
                let mut first = 0;
                let mut end = text.len();
                if let Some((line, col, inclusive)) = self.anchor
                    && line_num == line
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
                let matches = self
                    .searcher
                    .matches_in::<REV>(&text.as_bytes()[first..end]);
                if let Some(current) = &mut self.current {
                    current.line = line_num;
                    current.columns = map;
                    current.matches = matches;
                    current.offset = first;
                } else {
                    self.current = Some(LiteralLine {
                        line: line_num,
                        columns: map,
                        matches,
                        offset: first,
                    });
                }
            } else {
                self.current = None;
            }
            #[cfg(test)]
            super::index::count_search_from_line_candidate();
        }
    }
}

#[cfg(test)]
#[path = "iterators_memmem_forward_tests.rs"]
mod memmem_forward_tests;
