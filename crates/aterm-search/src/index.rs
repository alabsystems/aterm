// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Core trigram search index with bloom filter acceleration.

use std::borrow::Cow;
use std::collections::BTreeSet;

use aterm_hash::{FxBuildHasher, FxHashMap, FxHashSet};
use aterm_types::text_shaping::AmbiguousWidth;

use crate::bitmap::SparseBitmap;

use super::bloom::BloomFilter;
use super::iterators::{
    CandidateSource, SearchMatchIterator, SearchMatchReverseIterator, literal_occurrence,
};
#[cfg(test)]
use super::types::SearchResult;
use super::types::{DirectedFind, LineFrame, SearchDirection, SearchMatch, SearchResults};
use crate::cells::LineCells;
use crate::grapheme::{
    ColumnMap, LowerByteCursors, LowerNeed, MapSource, SearchColumns, lower_fold, lower_fold_char,
    lower_fold_into, lower_need,
};

/// Default maximum number of lines kept in the search index cache before oldest-quarter eviction.
pub const DEFAULT_MAX_CACHED_LINES: usize = 100_000;

/// Upper bound on the matches collected by one batch search query.
pub const MAX_SEARCH_MATCHES: usize = 100_000;

/// Eviction low-water mark as a fraction `NUM/DEN` of `max_cached_lines`: once over capacity, [`SearchIndex::index_line`] drops the cache to `NUM/DEN · max`, keeping the newest lines (hysteresis, so eviction does not re-trigger every line).
pub(crate) const EVICTION_RETAIN_NUM: usize = 3;
pub(crate) const EVICTION_RETAIN_DEN: usize = 4;

/// Smallest `max_cached_lines` that keeps the newest `retained` lines safe from eviction.
#[inline]
#[must_use]
pub const fn max_cached_for_retained(retained: usize) -> usize {
    retained
        .saturating_mul(EVICTION_RETAIN_DEN)
        .div_ceil(EVICTION_RETAIN_NUM)
}

#[cfg(test)]
thread_local! {
    static SEARCH_FROM_LINE_CANDIDATES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static CASE_INSENSITIVE_CANDIDATE_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static CASE_INSENSITIVE_LOWERED_LINES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static CASE_INSENSITIVE_MATCH_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static LAST_UNIQUE_POSTING_LISTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn count_search_from_line_candidate() {
    SEARCH_FROM_LINE_CANDIDATES.with(|c| c.set(c.get().saturating_add(1)));
}

#[cfg(test)]
fn count_case_insensitive_candidate_visit() {
    CASE_INSENSITIVE_CANDIDATE_VISITS.with(|c| c.set(c.get().saturating_add(1)));
}

#[cfg(test)]
fn count_case_insensitive_lowered_line() {
    CASE_INSENSITIVE_LOWERED_LINES.with(|c| c.set(c.get().saturating_add(1)));
}

/// Trigram search index with a bloom filter, posting lists, and a bounded line cache.
#[derive(Clone, Debug)]
pub struct SearchIndex {
    /// Bloom filter for fast negative lookups.
    bloom: BloomFilter,
    /// Trigram -> line numbers mapping.
    trigrams: FxHashMap<[u8; 3], SparseBitmap>,
    /// Cached line text by line number, for match verification.
    pub(super) lines: FxHashMap<usize, String>,
    /// Column maps of the cached lines, built at index time and reused by every search.
    pub(super) column_maps: FxHashMap<usize, ColumnMap>,
    /// Reused lowering buffer for the non-ASCII lowered-trigram pass, so its capacity
    /// survives across lines.
    lower_scratch: String,
    /// The line number posting key 0 stands for.
    key_base: usize,
    /// Total number of indexed lines.
    line_count: usize,
    /// Lowest line number currently present in `lines`; short-query ranges start here.
    first_cached_line: usize,
    /// Next line number to index (for incremental indexing).
    next_line: usize,
    /// Maximum lines to keep in cache before eviction.
    max_cached_lines: usize,
    /// Lowest line number still retained in the index: 0 until eviction drops the
    /// oldest lines.
    lowest_retained_line: usize,
    /// Whether results may be incomplete: latched by eviction, and set either way by
    /// [`set_history_prefix_eviction`](Self::set_history_prefix_eviction).
    eviction_occurred: bool,
    /// Guards the one-time `aterm_log` warning emitted on first eviction.
    first_eviction_warned: bool,
    /// Cached lines whose text stops short of the line's end
    /// ([`LineCells::is_truncated`]): every batch result counts them in
    /// [`SearchResults::cut_short_lines`].
    truncated_lines: BTreeSet<usize>,
    /// How wide the grid draws an East Asian Ambiguous character in the lines indexed here (the terminal's `ambiguous_width` policy): every column a match reports is measured under it.
    pub(super) ambiguous_width: AmbiguousWidth,
}

const MAX_KEY: u32 = u32::MAX - 1;

const MAX_KEY_SPAN: usize = MAX_KEY as usize;

/// The bloom filter represents the posting dictionary, not each occurrence in
/// every row.
#[inline]
fn insert_posting(
    trigrams: &mut FxHashMap<[u8; 3], SparseBitmap>,
    bloom: &mut BloomFilter,
    trigram: [u8; 3],
    line: u32,
) {
    let bitmap = trigrams.entry(trigram).or_default();
    if bitmap.is_empty() {
        bloom.insert_bytes(&trigram);
    }
    bitmap.insert(line);
}

/// Intersect posting lists sorted ascending by length into one owned bitmap. Two or
/// more lists start from a borrowed `&a & &b`; only a lone list is cloned.
#[cfg(test)]
fn intersect_posting_lists(sorted_lists: &[&SparseBitmap]) -> SparseBitmap {
    debug_assert!(!sorted_lists.is_empty());
    if sorted_lists.len() == 1 {
        return sorted_lists[0].clone();
    }
    let mut result: SparseBitmap = sorted_lists[0] & sorted_lists[1];
    for bitmap in &sorted_lists[2..] {
        result &= *bitmap;
    }
    result
}

/// Reusable per-query case-insensitive line matcher.
pub(crate) struct CaseInsensitiveMatcher {
    lower_query: crate::bytesearch::OwnedSearcher,
    lower_buf: String,
    /// Query classification stays fixed while the matcher visits its candidate rows.
    ascii_query: bool,
}

/// A match visit's completion and raw occurrence facts, including zero-column spans.
pub(crate) struct MatchVisit {
    pub(crate) completed: bool,
    pub(crate) had_occurrence: bool,
}

fn visit_occurrences<const REV: bool>(
    line_num: usize,
    query_len: usize,
    col_map: &impl SearchColumns,
    mut byte_map: Option<&mut LowerByteCursors<'_, REV>>,
    mut positions: impl Iterator<Item = usize>,
    mut visit: impl FnMut(SearchMatch) -> bool,
) -> MatchVisit {
    let mut result = MatchVisit {
        completed: true,
        had_occurrence: false,
    };
    let Some(mut abs_pos) = positions.next() else {
        return result;
    };
    let col_map = col_map.get();
    loop {
        #[cfg(test)]
        CASE_INSENSITIVE_MATCH_VISITS.with(|count| count.set(count.get() + 1));
        result.had_occurrence = true;
        let match_end = abs_pos.saturating_add(query_len);
        // Starts and ends each stay monotone, even when overlapping spans interleave.
        let (orig_start, orig_end) = byte_map
            .as_mut()
            .map_or((abs_pos, match_end), |map| map.span(abs_pos, match_end));
        let (start_col, end_col) = col_map.span(orig_start, orig_end);
        if start_col != end_col && !visit(SearchMatch::new(line_num, start_col, end_col)) {
            result.completed = false;
            break;
        }
        let Some(next) = positions.next() else {
            break;
        };
        abs_pos = next;
    }
    result
}

impl CaseInsensitiveMatcher {
    #[inline]
    pub(crate) fn new(query: &str) -> Self {
        let lower_query = lower_fold(query);
        let ascii_query = lower_query.is_ascii();
        Self {
            lower_query: lower_query.into(),
            lower_buf: String::new(),
            ascii_query,
        }
    }

    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.lower_query.is_empty()
    }

    /// Visit matches in the selected byte/column order, reporting textual occurrences even when
    /// their zero-column spans are omitted from the visitor.
    #[inline]
    pub(crate) fn visit_matches<const REV: bool>(
        &mut self,
        line_num: usize,
        text: &str,
        col_map: &impl SearchColumns,
        visit: impl FnMut(SearchMatch) -> bool,
    ) -> MatchVisit {
        self.visit_matches_anchored::<REV>(line_num, text, col_map, None, visit)
    }

    /// Restrict ASCII identity rows to a point anchor; other maps retain column filtering.
    #[inline]
    pub(crate) fn visit_matches_anchored<const REV: bool>(
        &mut self,
        line_num: usize,
        text: &str,
        col_map: &impl SearchColumns,
        anchor: Option<(usize, bool)>,
        visit: impl FnMut(SearchMatch) -> bool,
    ) -> MatchVisit {
        let empty = MatchVisit {
            completed: true,
            had_occurrence: false,
        };
        if self.lower_query.is_empty() {
            return empty;
        }

        if col_map.is_ascii() {
            if !self.ascii_query {
                return empty;
            }
            let searcher = self.lower_query.searcher_ascii();
            let window = anchor
                .and_then(|(col, inclusive)| {
                    col_map.get().identity_match_window::<REV>(
                        text.len(),
                        col,
                        inclusive,
                        self.lower_query.len(),
                    )
                })
                .unwrap_or(0..text.len());
            let offset = window.start;
            return visit_occurrences::<REV>(
                line_num,
                self.lower_query.len(),
                col_map,
                None,
                searcher
                    .matches_in::<REV>(&text.as_bytes()[window])
                    .map(|at| offset.saturating_add(at)),
                visit,
            );
        }

        #[cfg(test)]
        count_case_insensitive_lowered_line();

        self.lower_buf.clear();
        self.lower_buf
            .extend(text.chars().flat_map(lower_fold_char));
        let mut byte_map = LowerByteCursors::new(text, self.lower_buf.len());
        let searcher = self.lower_query.searcher();
        visit_occurrences::<REV>(
            line_num,
            self.lower_query.len(),
            col_map,
            Some(&mut byte_map),
            searcher.matches_in::<REV>(self.lower_buf.as_bytes()),
            visit,
        )
    }
}

impl SearchIndex {
    pub(crate) fn compile_regex(
        query: &str,
        case_sensitive: bool,
    ) -> Result<aterm_regex::Regex, SearchOptionsError> {
        if query.len() > MAX_REGEX_PATTERN_LEN {
            return Err(SearchOptionsError::PatternTooLong);
        }
        let pattern = if case_sensitive {
            query.to_string()
        } else {
            format!("(?i){query}")
        };
        aterm_regex::RegexBuilder::new(&pattern)
            .size_limit(REGEX_SIZE_LIMIT)
            .step_limit(REGEX_STEP_LIMIT)
            .build()
            .map_err(|e| SearchOptionsError::InvalidRegex(e.to_string()))
    }

    /// Take (read and reset) `search_from_line` candidate count.
    #[cfg(test)]
    pub(crate) fn take_search_from_line_candidates() -> usize {
        SEARCH_FROM_LINE_CANDIDATES.with(|c| {
            let value = c.get();
            c.set(0);
            value
        })
    }

    /// Take (read and reset) the case-insensitive candidate-visit count.
    #[cfg(test)]
    pub(crate) fn take_case_insensitive_candidate_visits() -> usize {
        CASE_INSENSITIVE_CANDIDATE_VISITS.with(|c| {
            let value = c.get();
            c.set(0);
            value
        })
    }

    /// Take (read and reset) the count of per-line buffered lowering passes.
    #[cfg(test)]
    pub(crate) fn take_case_insensitive_lowered_lines() -> usize {
        CASE_INSENSITIVE_LOWERED_LINES.with(|c| {
            let value = c.get();
            c.set(0);
            value
        })
    }

    /// Take the count of raw folded occurrences considered by the visitor.
    #[cfg(test)]
    pub(crate) fn take_case_insensitive_match_visits() -> usize {
        CASE_INSENSITIVE_MATCH_VISITS.with(|count| count.replace(0))
    }

    #[cfg(test)]
    pub(crate) fn take_last_unique_posting_lists() -> usize {
        LAST_UNIQUE_POSTING_LISTS.with(|count| {
            let value = count.get();
            count.set(0);
            value
        })
    }

    #[cfg(test)]
    pub(crate) fn bloom_is_saturated(&self) -> bool {
        self.bloom.is_saturated()
    }

    /// Create a new search index with the default cache cap ([`DEFAULT_MAX_CACHED_LINES`]).
    #[must_use]
    pub fn new() -> Self {
        Self {
            bloom: BloomFilter::with_capacity(1000),
            trigrams: FxHashMap::default(),
            lines: FxHashMap::default(),
            column_maps: FxHashMap::default(),
            lower_scratch: String::new(),
            key_base: 0,
            line_count: 0,
            first_cached_line: usize::MAX,
            next_line: 0,
            max_cached_lines: DEFAULT_MAX_CACHED_LINES,
            lowest_retained_line: 0,
            eviction_occurred: false,
            first_eviction_warned: false,
            truncated_lines: BTreeSet::new(),
            ambiguous_width: AmbiguousWidth::Single,
        }
    }

    /// Create a search index with expected capacity and the default cache cap.
    #[must_use]
    pub fn with_capacity(expected_lines: usize) -> Self {
        Self::with_capacity_and_max(expected_lines, DEFAULT_MAX_CACHED_LINES)
    }

    /// Create a new search index with an explicit cache cap: the distinct lines kept before eviction drops the oldest quarter, clamped to at least 1.
    #[must_use]
    #[cfg(test)]
    pub fn with_max_cached_lines(max_cached_lines: usize) -> Self {
        let mut index = Self::new();
        index.set_max_cached_lines(max_cached_lines);
        index
    }

    /// Create a new search index with expected capacity and an explicit cap.
    #[must_use]
    pub fn with_capacity_and_max(expected_lines: usize, max_cached_lines: usize) -> Self {
        let expected_lines = expected_lines.min(max_cached_lines.max(1));
        const MAX_CAPACITY_HINT: usize = 1 << 20;
        let line_hint = if expected_lines > MAX_CAPACITY_HINT {
            MAX_CAPACITY_HINT
        } else {
            expected_lines
        };
        Self {
            bloom: BloomFilter::with_capacity(1000),
            trigrams: FxHashMap::with_capacity_and_hasher(line_hint / 10, FxBuildHasher),
            lines: FxHashMap::with_capacity_and_hasher(line_hint, FxBuildHasher),
            column_maps: FxHashMap::with_capacity_and_hasher(line_hint, FxBuildHasher),
            lower_scratch: String::new(),
            key_base: 0,
            line_count: 0,
            first_cached_line: usize::MAX,
            next_line: 0,
            max_cached_lines: max_cached_lines.max(1),
            lowest_retained_line: 0,
            eviction_occurred: false,
            first_eviction_warned: false,
            truncated_lines: BTreeSet::new(),
            ambiguous_width: AmbiguousWidth::Single,
        }
    }

    /// Set the `ambiguous_width` policy used to measure column positions.
    pub fn set_ambiguous_width(&mut self, ambiguous: AmbiguousWidth) {
        if ambiguous == self.ambiguous_width {
            return;
        }
        self.ambiguous_width = ambiguous;
        for (line, text) in &self.lines {
            let walked = self
                .column_maps
                .get(line)
                .is_none_or(|map| matches!(map.source(), MapSource::Walk(_)));
            if walked {
                self.column_maps
                    .insert(*line, ColumnMap::new(text, ambiguous));
            }
        }
    }

    /// The `ambiguous_width` policy columns are measured under.
    #[inline]
    #[must_use]
    pub const fn ambiguous_width(&self) -> AmbiguousWidth {
        self.ambiguous_width
    }

    /// Index a line at a specific line number.
    pub fn index_line(&mut self, line_num: usize, text: &str) {
        self.index_line_cow(line_num, Cow::Borrowed(text), None);
    }

    /// Shared borrowed/owned indexing primitive; owned text moves into the line cache without a second copy.
    pub(crate) fn index_line_cow(
        &mut self,
        line_num: usize,
        text: Cow<'_, str>,
        cells: Option<&LineCells>,
    ) {
        let text_ref = text.as_ref();
        let ambiguous = self.ambiguous_width;
        let column_map = || match cells {
            Some(cells) => ColumnMap::with_cells(text_ref, cells),
            None => ColumnMap::new(text_ref, ambiguous),
        };

        if self.lines.get(&line_num).is_some_and(|old| old == text_ref) {
            if let Some(cells) = cells {
                if let Some(map) = self.column_maps.get_mut(&line_num) {
                    map.refresh_cells(text_ref, cells);
                } else {
                    self.column_maps.insert(line_num, column_map());
                }
                self.set_truncated(line_num, cells.is_truncated());
            }
            return;
        }

        if !self.make_room_for(line_num) {
            return;
        }
        let key = self.posting_key(line_num);
        if let Some(old_text) = self.lines.remove(&line_num) {
            self.reindex_changed_row_postings(key, &old_text, text_ref);
        } else {
            for window in text_ref.as_bytes().windows(3) {
                insert_posting(
                    &mut self.trigrams,
                    &mut self.bloom,
                    [window[0], window[1], window[2]],
                    key,
                );
            }

            match lower_need(text_ref) {
                LowerNeed::None => {}
                LowerNeed::Ascii => {
                    for window in text_ref.as_bytes().windows(3) {
                        insert_posting(
                            &mut self.trigrams,
                            &mut self.bloom,
                            [
                                window[0].to_ascii_lowercase(),
                                window[1].to_ascii_lowercase(),
                                window[2].to_ascii_lowercase(),
                            ],
                            key,
                        );
                    }
                }
                LowerNeed::Unicode => {
                    lower_fold_into(text_ref, &mut self.lower_scratch);
                    for window in self.lower_scratch.as_bytes().windows(3) {
                        insert_posting(
                            &mut self.trigrams,
                            &mut self.bloom,
                            [window[0], window[1], window[2]],
                            key,
                        );
                    }
                }
            }
        }

        let column_map = column_map();
        self.lines.insert(line_num, text.into_owned());
        self.column_maps.insert(line_num, column_map);
        self.set_truncated(line_num, cells.is_some_and(LineCells::is_truncated));
        self.first_cached_line = self.first_cached_line.min(line_num);
        self.line_count = self.line_count.max(line_num.saturating_add(1));
        self.next_line = self.next_line.max(line_num.saturating_add(1));

        if self.lines.len() > self.max_cached_lines {
            self.evict_oldest_lines();
        }

        if self.bloom.is_saturated() {
            self.rebuild_bloom();
        }
    }

    /// Index a line at the next available line number.
    pub fn push_line(&mut self, text: &str) -> usize {
        let line_num = self.next_line;
        self.index_line(line_num, text);
        line_num
    }

    fn set_truncated(&mut self, line: usize, truncated: bool) {
        if truncated {
            self.truncated_lines.insert(line);
        } else if !self.truncated_lines.is_empty() {
            self.truncated_lines.remove(&line);
        }
    }

    fn drop_truncated_below(&mut self, floor: usize) {
        if !self.truncated_lines.is_empty() {
            self.truncated_lines = self.truncated_lines.split_off(&floor);
        }
    }

    fn with_truncated_lines(&self, results: SearchResults) -> SearchResults {
        if self.truncated_lines.is_empty() {
            return results;
        }
        let cut_short = results
            .cut_short_lines
            .saturating_add(self.truncated_lines.len());
        results.with_cut_short_lines(cut_short)
    }

    /// The posting key of `line`: its offset from
    /// [`key_base`](Self::key_base).
    #[inline]
    fn posting_key(&self, line: usize) -> u32 {
        u32::try_from(line.saturating_sub(self.key_base)).unwrap_or(u32::MAX)
    }

    fn line_range(&self, first: usize, end: usize) -> CandidateSource<'_> {
        let keys = self.posting_key(first)..self.posting_key(end);
        match self.held_keys_if_sparse(&keys) {
            Some(held) => CandidateSource::listed(self.key_base, held),
            None => CandidateSource::range(self.key_base, keys),
        }
    }

    fn line_range_rev(&self, first: usize, end: usize) -> CandidateSource<'_> {
        let keys = self.posting_key(first)..self.posting_key(end);
        match self.held_keys_if_sparse(&keys) {
            Some(held) => CandidateSource::listed_rev(self.key_base, held),
            None => CandidateSource::range_rev(self.key_base, keys),
        }
    }

    /// The keys of the cached lines in `keys`, sorted, when `keys` is so much wider than the cache
    /// that listing them costs less than walking it: a line number no line holds yields nothing,
    /// and far-apart lines leave up to [`MAX_KEY_SPAN`] of them, so a search stays bound by the
    /// lines held and not by how far apart their numbers are. Walking a number took about a
    /// quarter of what listing a held line did (release, 2026-10-31), and a walk can stop at its
    /// first match, hence four.
    fn held_keys_if_sparse(&self, keys: &std::ops::Range<u32>) -> Option<Vec<u32>> {
        let span = usize::try_from(keys.end.saturating_sub(keys.start)).unwrap_or(usize::MAX);
        if span <= self.lines.len().saturating_mul(4) {
            return None;
        }
        let mut held: Vec<u32> = self
            .lines
            .keys()
            .filter_map(|line| u32::try_from(line.checked_sub(self.key_base)?).ok())
            .filter(|key| keys.contains(key))
            .collect();
        held.sort_unstable();
        Some(held)
    }

    fn postings_forward<'a>(
        &self,
        postings: Vec<&'a SparseBitmap>,
        from_line: usize,
    ) -> CandidateSource<'a> {
        CandidateSource::from_postings_forward(self.key_base, postings, self.posting_key(from_line))
    }

    fn postings_backward<'a>(
        &self,
        postings: Vec<&'a SparseBitmap>,
        before_line: usize,
    ) -> CandidateSource<'a> {
        CandidateSource::from_postings_backward(
            self.key_base,
            postings,
            self.posting_key(before_line),
        )
    }

    fn make_room_for(&mut self, line: usize) -> bool {
        if line
            .checked_sub(self.key_base)
            .is_some_and(|offset| offset <= MAX_KEY_SPAN)
        {
            return true;
        }
        if self.lines.is_empty() {
            debug_assert!(self.trigrams.is_empty());
            self.key_base = line;
            return true;
        }
        let newest = self.lines.keys().copied().max().unwrap_or(line).max(line);
        let floor = newest.saturating_sub(MAX_KEY_SPAN);
        if line < floor {
            self.note_window_eviction(floor);
            return false;
        }
        let new_base = if line < self.key_base {
            floor
        } else {
            self.evict_below_window(floor).unwrap_or(line).min(line)
        };
        self.rekey(new_base);
        true
    }

    fn rekey(&mut self, new_base: usize) {
        let up = new_base < self.key_base;
        let by = if up {
            self.key_base - new_base
        } else {
            new_base - self.key_base
        };
        if let Ok(by) = u32::try_from(by) {
            for bitmap in self.trigrams.values_mut() {
                bitmap.shift(up, by);
            }
        } else {
            debug_assert!(self.trigrams.is_empty());
        }
        self.key_base = new_base;
    }

    fn evict_below_window(&mut self, floor: usize) -> Option<usize> {
        let before = self.lines.len();
        let mut oldest: Option<usize> = None;
        self.lines.retain(|&line, _| {
            let keep = line >= floor;
            if keep {
                oldest = Some(oldest.map_or(line, |o| o.min(line)));
            }
            keep
        });
        if self.lines.len() != before {
            self.column_maps.retain(|&line, _| line >= floor);
            self.drop_truncated_below(floor);
            let watermark = self.posting_key(floor);
            self.trigrams.retain(|_, bitmap| {
                bitmap.drop_below(watermark);
                !bitmap.is_empty()
            });
            self.first_cached_line = oldest.unwrap_or(usize::MAX);
            self.note_window_eviction(floor);
        }
        oldest
    }

    fn note_window_eviction(&mut self, floor: usize) {
        if !self.lines.is_empty() {
            self.first_cached_line = self.first_cached_line.max(floor);
        }
        self.lowest_retained_line = self.lowest_retained_line.max(floor);
        self.eviction_occurred = true;
        if !self.first_eviction_warned {
            self.first_eviction_warned = true;
            aterm_log::warn!(
                "search index spans more than {} lines; results incomplete below line {}",
                MAX_KEY_SPAN,
                floor,
            );
        }
    }

    fn posting_trigram_set(&mut self, text: &str) -> FxHashSet<[u8; 3]> {
        let bytes = text.as_bytes();
        let mut set: FxHashSet<[u8; 3]> =
            FxHashSet::with_capacity_and_hasher(bytes.len().saturating_mul(2), FxBuildHasher);
        for window in bytes.windows(3) {
            set.insert([window[0], window[1], window[2]]);
        }
        match lower_need(text) {
            LowerNeed::None => {}
            LowerNeed::Ascii => {
                for window in bytes.windows(3) {
                    set.insert([
                        window[0].to_ascii_lowercase(),
                        window[1].to_ascii_lowercase(),
                        window[2].to_ascii_lowercase(),
                    ]);
                }
            }
            LowerNeed::Unicode => {
                lower_fold_into(text, &mut self.lower_scratch);
                for window in self.lower_scratch.as_bytes().windows(3) {
                    set.insert([window[0], window[1], window[2]]);
                }
            }
        }
        set
    }

    fn reindex_changed_row_postings(&mut self, key: u32, old_text: &str, text: &str) {
        let old_set = self.posting_trigram_set(old_text);
        let bytes = text.as_bytes();
        let mut new_seen: FxHashSet<[u8; 3]> =
            FxHashSet::with_capacity_and_hasher(bytes.len().saturating_mul(2), FxBuildHasher);

        for window in bytes.windows(3) {
            let trigram: [u8; 3] = [window[0], window[1], window[2]];
            if new_seen.insert(trigram) && !old_set.contains(&trigram) {
                insert_posting(&mut self.trigrams, &mut self.bloom, trigram, key);
            }
        }
        match lower_need(text) {
            LowerNeed::None => {}
            LowerNeed::Ascii => {
                for window in bytes.windows(3) {
                    let trigram: [u8; 3] = [
                        window[0].to_ascii_lowercase(),
                        window[1].to_ascii_lowercase(),
                        window[2].to_ascii_lowercase(),
                    ];
                    if new_seen.insert(trigram) && !old_set.contains(&trigram) {
                        insert_posting(&mut self.trigrams, &mut self.bloom, trigram, key);
                    }
                }
            }
            LowerNeed::Unicode => {
                lower_fold_into(text, &mut self.lower_scratch);
                for window in self.lower_scratch.as_bytes().windows(3) {
                    let trigram: [u8; 3] = [window[0], window[1], window[2]];
                    if new_seen.insert(trigram) && !old_set.contains(&trigram) {
                        insert_posting(&mut self.trigrams, &mut self.bloom, trigram, key);
                    }
                }
            }
        }
        for trigram in old_set {
            if new_seen.contains(&trigram) {
                continue;
            }
            if let Some(bitmap) = self.trigrams.get_mut(&trigram) {
                bitmap.remove(key);
                if bitmap.is_empty() {
                    self.trigrams.remove(&trigram);
                }
            }
        }
    }

    fn evict_oldest_lines(&mut self) {
        let target =
            self.max_cached_lines.saturating_mul(EVICTION_RETAIN_NUM) / EVICTION_RETAIN_DEN;
        let to_evict = self.lines.len().saturating_sub(target);
        if to_evict == 0 {
            return;
        }

        let mut line_nums: Vec<usize> = self.lines.keys().copied().collect();

        let evict_count = if to_evict < line_nums.len() {
            to_evict
        } else {
            line_nums.len()
        };
        // Only the oldest partition and the first retained row matter; ordering
        // either partition would sort rows whose relative order is never used.
        let next_retained = if evict_count < line_nums.len() {
            let (_, first_retained, _) = line_nums.select_nth_unstable(evict_count);
            Some(*first_retained)
        } else {
            None
        };
        for &line in line_nums.iter().take(evict_count) {
            self.lines.remove(&line);
            self.column_maps.remove(&line);
        }

        self.lowest_retained_line = next_retained.unwrap_or(self.next_line);
        self.first_cached_line = next_retained.unwrap_or(usize::MAX);
        self.eviction_occurred = true;
        self.drop_truncated_below(self.lowest_retained_line);

        let watermark = self.posting_key(self.lowest_retained_line);
        self.trigrams.retain(|_, bitmap| {
            bitmap.drop_below(watermark);
            !bitmap.is_empty()
        });

        if !self.first_eviction_warned {
            self.first_eviction_warned = true;
            aterm_log::warn!(
                "search index full at {} lines; results incomplete below line {}",
                self.max_cached_lines,
                self.lowest_retained_line,
            );
        }

        self.rebuild_bloom();
    }

    fn rebuild_bloom(&mut self) {
        let capacity = self.trigrams.len().max(1000);
        self.bloom = if capacity > BloomFilter::MAX_EFFECTIVE_CAPACITY {
            BloomFilter::with_capacity(BloomFilter::MAX_EFFECTIVE_CAPACITY)
        } else {
            BloomFilter::with_capacity(capacity)
        };
        for trigram in self.trigrams.keys() {
            self.bloom.insert_bytes(trigram);
        }
    }

    /// Whether `query` might have matches (bloom filter check): `false` means definitely none.
    #[must_use]
    pub fn might_contain(&self, query: &str) -> bool {
        let bytes = query.as_bytes();

        if bytes.len() < 3 {
            return true;
        }

        for window in bytes.windows(3) {
            if !self.bloom.might_contain_bytes(window) {
                return false;
            }
        }
        true
    }

    /// Intersect the posting lists for every trigram in `query` (3+ bytes; the caller
    /// handles empty, short and bloom-rejected queries).
    #[cfg(test)]
    fn intersect_trigrams(&self, query: &str) -> Option<SparseBitmap> {
        let mut posting_lists = self.posting_lists(query)?;
        if posting_lists.is_empty() {
            return None;
        }

        posting_lists.sort_unstable_by_key(|b| b.len());
        Some(intersect_posting_lists(&posting_lists))
    }

    fn posting_lists(&self, query: &str) -> Option<Vec<&SparseBitmap>> {
        // Small queries need only a sorted reference vector. Once distinct
        // postings exceed this bounded insertion cost, hash membership keeps
        // long, high-diversity queries from doing quadratic shifts.
        const SORTED_LIMIT: usize = 32;
        let mut posting_lists: Vec<&SparseBitmap> = Vec::new();
        if query.len() <= SORTED_LIMIT + 2 {
            // Materializing at most 32 references is bounded scratch and keeps
            // the ordinary short-query sort on its compact fast path.
            for window in query.as_bytes().windows(3) {
                let trigram = [window[0], window[1], window[2]];
                posting_lists.push(self.trigrams.get(&trigram)?);
            }
            posting_lists.sort_unstable_by_key(|bitmap| std::ptr::from_ref(*bitmap));
            posting_lists.dedup_by(|left, right| std::ptr::eq(*left, *right));
        } else {
            let mut seen: Option<FxHashSet<*const SparseBitmap>> = None;
            for window in query.as_bytes().windows(3) {
                let trigram: [u8; 3] = [window[0], window[1], window[2]];
                let bitmap = self.trigrams.get(&trigram)?;
                let key = std::ptr::from_ref(bitmap);
                if let Some(seen) = &mut seen {
                    if seen.insert(key) {
                        posting_lists.push(bitmap);
                    }
                } else if let Err(at) =
                    posting_lists.binary_search_by_key(&key, |bitmap| std::ptr::from_ref(*bitmap))
                {
                    posting_lists.insert(at, bitmap);
                    if posting_lists.len() == SORTED_LIMIT {
                        seen = Some(
                            posting_lists
                                .iter()
                                .map(|bitmap| std::ptr::from_ref(*bitmap))
                                .collect(),
                        );
                    }
                }
            }
        }
        #[cfg(test)]
        LAST_UNIQUE_POSTING_LISTS.with(|count| count.set(posting_lists.len()));
        Some(posting_lists)
    }

    fn literal_candidates_forward(
        &self,
        lower_query: &str,
        from_line: usize,
    ) -> CandidateSource<'_> {
        if lower_query.is_empty() || self.lines.is_empty() {
            return CandidateSource::empty();
        }

        let first = from_line.max(self.first_cached_line);
        if lower_query.len() < 3 {
            return self.line_range(first, self.line_count);
        }
        if !self.might_contain(lower_query) {
            return CandidateSource::empty();
        }
        let Some(postings) = self.posting_lists(lower_query) else {
            return CandidateSource::empty();
        };
        self.postings_forward(postings, first)
    }

    fn literal_candidates_backward(
        &self,
        lower_query: &str,
        before_line: usize,
    ) -> CandidateSource<'_> {
        if lower_query.is_empty() || self.lines.is_empty() {
            return CandidateSource::empty();
        }

        let upper = before_line.min(self.line_count);
        if lower_query.len() < 3 {
            let lower = self.first_cached_line.min(upper);
            return self.line_range_rev(lower, upper);
        }
        if !self.might_contain(lower_query) {
            return CandidateSource::empty();
        }
        let Some(postings) = self.posting_lists(lower_query) else {
            return CandidateSource::empty();
        };
        self.postings_backward(postings, upper)
    }

    /// Candidate lines for `query`: false positives possible, false negatives never.
    #[cfg(test)]
    pub fn search(&self, query: &str) -> impl Iterator<Item = usize> + '_ + use<'_> {
        let bytes = query.as_bytes();
        let base = self.key_base;
        let line = move |key: u32| base.saturating_add(key as usize);

        if bytes.len() < 3 {
            let first = self.first_cached_line.min(self.line_count);
            return SearchResult::All(self.posting_key(first)..self.posting_key(self.line_count))
                .map(line);
        }

        if !self.might_contain(query) {
            return SearchResult::None.map(line);
        }

        let Some(result) = self.intersect_trigrams(query) else {
            return SearchResult::None.map(line);
        };
        SearchResult::Bitmap(Box::new(result.into_iter())).map(line)
    }

    /// Verified matches of `query` with column positions, at most [`MAX_SEARCH_MATCHES`].
    pub fn search_with_positions(&self, query: &str) -> Vec<SearchMatch> {
        if query.is_empty() {
            return Vec::new();
        }
        self.search_from_line(query, self.first_cached_line.min(self.line_count))
            .take(MAX_SEARCH_MATCHES)
            .collect()
    }

    /// Get the number of indexed lines.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.line_count
    }

    /// Returns true if the index is empty.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.line_count == 0
    }

    /// Clear the index, including the eviction watermark and the incomplete-results signal.
    pub fn clear(&mut self) {
        self.bloom.clear();
        self.trigrams.clear();
        self.lines.clear();
        self.column_maps.clear();
        self.key_base = 0;
        self.line_count = 0;
        self.first_cached_line = usize::MAX;
        self.next_line = 0;
        self.lowest_retained_line = 0;
        self.eviction_occurred = false;
        self.first_eviction_warned = false;
        self.truncated_lines.clear();
    }

    /// Release the index's backing allocations, not just its contents.
    pub fn release(&mut self) {
        self.bloom = BloomFilter::with_capacity(1000);
        self.trigrams = FxHashMap::default();
        self.lines = FxHashMap::default();
        self.column_maps = FxHashMap::default();
        self.lower_scratch = String::new();
        self.key_base = 0;
        self.line_count = 0;
        self.first_cached_line = usize::MAX;
        self.next_line = 0;
        self.lowest_retained_line = 0;
        self.eviction_occurred = false;
        self.first_eviction_warned = false;
        self.truncated_lines = BTreeSet::new();
    }

    /// Set the maximum number of cached lines before eviction, clamped to at least 1.
    #[cfg(test)]
    pub fn set_max_cached_lines(&mut self, max: usize) {
        self.max_cached_lines = max.max(1);
    }

    /// State, in both directions, whether the bulk build that just finished omitted an older history prefix the source still holds.
    pub(crate) fn set_history_prefix_eviction(&mut self, lowest_retained_line: Option<usize>) {
        match lowest_retained_line {
            Some(base) if base > 0 => {
                self.lowest_retained_line = base;
                self.eviction_occurred = true;
            }
            _ => {
                self.lowest_retained_line = 0;
                self.eviction_occurred = false;
            }
        }
    }

    /// Drop cached absolute rows below a newly-retained history boundary.
    #[cfg(test)]
    pub(crate) fn retain_history_from(&mut self, first_retained_line: usize) {
        self.drop_history_below(first_retained_line);
        self.lowest_retained_line = self.lowest_retained_line.max(first_retained_line);
        self.eviction_occurred = true;
    }

    /// Drop cached absolute rows below `first_retained_line` without recording an eviction.
    pub fn drop_history_below(&mut self, first_retained_line: usize) {
        let old_first = self.first_cached_line.min(first_retained_line);
        if first_retained_line <= old_first {
            return;
        }
        let distance = first_retained_line.saturating_sub(old_first);
        if distance <= self.lines.len().saturating_mul(2) {
            for line in old_first..first_retained_line {
                self.lines.remove(&line);
                self.column_maps.remove(&line);
            }
        } else {
            self.lines.retain(|line, _| *line >= first_retained_line);
            self.column_maps
                .retain(|line, _| *line >= first_retained_line);
        }
        self.drop_truncated_below(first_retained_line);
        let watermark = self.posting_key(first_retained_line);
        self.trigrams.retain(|_, bitmap| {
            bitmap.drop_below(watermark);
            !bitmap.is_empty()
        });
        self.first_cached_line = if self.lines.is_empty() {
            usize::MAX
        } else {
            first_retained_line.min(self.line_count)
        };
    }

    /// Current maximum cached-lines cap.
    #[must_use]
    #[cfg(test)]
    pub fn max_cached_lines(&self) -> usize {
        self.max_cached_lines
    }

    /// Oldest line number still retained (`0` until eviction).
    #[inline]
    #[must_use]
    pub const fn lowest_retained_line(&self) -> usize {
        self.lowest_retained_line
    }

    /// Whether search results may be incomplete because older lines were evicted.
    #[inline]
    #[must_use]
    pub const fn results_may_be_incomplete(&self) -> bool {
        self.eviction_occurred
    }

    /// Get cached line content by line number.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn get_line(&self, line_num: usize) -> Option<&str> {
        self.lines.get(&line_num).map(|s| s.as_str())
    }

    /// Forward matches of `query` from `from_line` (inclusive), oldest first, range bounded on the trigram index so `find_next` skips earlier lines.
    pub(crate) fn search_from_line<'a>(
        &'a self,
        query: &'a str,
        from_line: usize,
    ) -> SearchMatchIterator<'a> {
        let bytes = query.as_bytes();

        let empty = CandidateSource::empty;

        if query.is_empty() {
            return SearchMatchIterator::new(self, query, empty());
        }

        if bytes.len() < 3 {
            let first = from_line.max(self.first_cached_line.min(self.line_count));
            let source = self.line_range(first, self.line_count);
            return SearchMatchIterator::new(self, query, source);
        }

        if !self.might_contain(query) {
            return SearchMatchIterator::new(self, query, empty());
        }

        let Some(postings) = self.posting_lists(query) else {
            return SearchMatchIterator::new(self, query, empty());
        };

        let source = self.postings_forward(postings, from_line);
        SearchMatchIterator::new(self, query, source)
    }

    /// Backward matches of `query` on lines before `before_line` (exclusive), newest first, for `find_prev`.
    pub(crate) fn search_before_line<'a>(
        &'a self,
        query: &'a str,
        before_line: usize,
    ) -> SearchMatchReverseIterator<'a> {
        let bytes = query.as_bytes();

        let empty = CandidateSource::empty;

        if query.is_empty() {
            return SearchMatchReverseIterator::new(self, query, empty());
        }

        if bytes.len() < 3 {
            let upper = before_line.min(self.line_count);
            let lower = self.first_cached_line.min(upper);
            let source = self.line_range_rev(lower, upper);
            return SearchMatchReverseIterator::new(self, query, source);
        }

        if !self.might_contain(query) {
            return SearchMatchReverseIterator::new(self, query, empty());
        }

        let Some(postings) = self.posting_lists(query) else {
            return SearchMatchReverseIterator::new(self, query, empty());
        };

        let source = self.postings_backward(postings, before_line);
        SearchMatchReverseIterator::new(self, query, source)
    }
}

const MAX_REGEX_PATTERN_LEN: usize = 1024;

/// Maximum compiled regex size (bytes) passed to `RegexBuilder::size_limit`: 128 KiB, 2,048 instructions at `aterm-regex`'s 64 bytes per instruction, well below the engine's 10 MiB default.
pub(crate) const REGEX_SIZE_LIMIT: usize = 128 * 1024;

/// Scan budget (work units) for one line up to a full-width row, passed to `aterm_regex::RegexBuilder::step_limit` and the floor of every line's budget ([`regex_line_budget`]); about 4.8 ms of matching in release.
pub(crate) const REGEX_STEP_LIMIT: u64 = 1 << 22;

/// Work units per byte a line longer than a full-width row gets: [`REGEX_STEP_LIMIT`] spread over the 4,096 columns it was sized for.
pub(crate) const REGEX_STEP_PER_BYTE: u64 = REGEX_STEP_LIMIT / 4096;

/// The most one line's scan may cost: 2^26 units, about 77 ms in release, which is `aterm-regex`'s own default step limit.
pub(crate) const REGEX_LINE_STEP_CEILING: u64 = 1 << 26;

/// The scan budget for one line of `len` bytes: `REGEX_STEP_PER_BYTE · len`, at least [`REGEX_STEP_LIMIT`] and at most [`REGEX_LINE_STEP_CEILING`].
#[inline]
pub(crate) fn regex_line_budget(len: usize) -> u64 {
    u64::try_from(len)
        .unwrap_or(u64::MAX)
        .saturating_mul(REGEX_STEP_PER_BYTE)
        .clamp(REGEX_STEP_LIMIT, REGEX_LINE_STEP_CEILING)
}

/// Every match of `re` on one line, under that line's own budget ([`regex_line_budget`]): the one regex scan of a line every search path runs, the batch, point navigation and the budgeted engine alike.
#[inline]
pub(crate) fn scan_line<'r, 't>(
    re: &'r aterm_regex::Regex,
    text: &'t str,
) -> aterm_regex::Matches<'r, 't> {
    re.find_iter_with_limit(text, regex_line_budget(text.len()))
}

/// Result of one [`SearchIndex::search_literal_narrowed`] step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NarrowedSearch {
    /// Forward batch results over the candidate set.
    pub results: SearchResults,
    /// Ascending retained lines containing the query at the fold level.
    pub occurrence_lines: Option<LineFrame>,
}

/// Error returned when search options are invalid (e.g., a malformed regex).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchOptionsError {
    /// The regex pattern is invalid.
    InvalidRegex(String),
    /// The regex pattern exceeds the maximum allowed length.
    PatternTooLong,
    /// A point search found no match before a line exhausted its regex scan budget.
    ScanCutShort,
}

impl std::fmt::Display for SearchOptionsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRegex(e) => {
                f.write_str("invalid regex: ")?;
                f.write_str(e)
            }
            Self::PatternTooLong => {
                f.write_str("pattern exceeds maximum length (")?;
                std::fmt::Display::fmt(&MAX_REGEX_PATTERN_LEN, f)?;
                f.write_str(" bytes)")
            }
            Self::ScanCutShort => f.write_str(
                "no match found, but a line too long for the regex scan budget \
                 was not searched to its end",
            ),
        }
    }
}

impl std::error::Error for SearchOptionsError {}

impl SearchIndex {
    /// `search_with_positions_opts_direction`, forward.
    #[cfg(test)]
    pub fn search_with_positions_opts(
        &self,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
    ) -> Result<Vec<SearchMatch>, SearchOptionsError> {
        self.search_with_positions_opts_direction(
            query,
            case_sensitive,
            is_regex,
            SearchDirection::Forward,
        )
    }

    /// [`search_results_opts_direction`](Self::search_results_opts_direction)'s matches alone.
    #[cfg(test)]
    pub fn search_with_positions_opts_direction(
        &self,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
        direction: SearchDirection,
    ) -> Result<Vec<SearchMatch>, SearchOptionsError> {
        let (matches, cut_short) =
            self.search_matches(query, case_sensitive, is_regex, direction)?;
        if cut_short > 0 {
            return Err(SearchOptionsError::ScanCutShort);
        }
        Ok(matches)
    }

    fn search_matches(
        &self,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
        direction: SearchDirection,
    ) -> Result<(Vec<SearchMatch>, usize), SearchOptionsError> {
        if query.is_empty() {
            return Ok((Vec::new(), 0));
        }

        if case_sensitive && !is_regex {
            return Ok((
                match direction {
                    SearchDirection::Forward => self.search_with_positions(query),
                    SearchDirection::Backward => {
                        let mut matches: Vec<_> = self
                            .search_before_line(query, self.line_count)
                            .take(MAX_SEARCH_MATCHES)
                            .collect();
                        matches.reverse();
                        matches
                    }
                },
                0,
            ));
        }

        if is_regex {
            return self.search_regex(query, case_sensitive, direction);
        }

        Ok((self.search_case_insensitive(query, direction), 0))
    }

    /// Forward search with options, including eviction completeness metadata.
    pub fn search_results_opts(
        &self,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
    ) -> Result<SearchResults, SearchOptionsError> {
        self.search_results_opts_direction(
            query,
            case_sensitive,
            is_regex,
            SearchDirection::Forward,
        )
    }

    /// Search with options and retain the capped edge selected by `direction`.
    pub fn search_results_opts_direction(
        &self,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
        direction: SearchDirection,
    ) -> Result<SearchResults, SearchOptionsError> {
        let (matches, cut_short) =
            self.search_matches(query, case_sensitive, is_regex, direction)?;
        let capped = matches.len() >= MAX_SEARCH_MATCHES;
        Ok(self.with_truncated_lines(
            SearchResults::new(
                matches,
                self.results_may_be_incomplete() || capped,
                self.lowest_retained_line(),
            )
            .with_cut_short_lines(cut_short),
        ))
    }

    fn search_regex(
        &self,
        query: &str,
        case_sensitive: bool,
        direction: SearchDirection,
    ) -> Result<(Vec<SearchMatch>, usize), SearchOptionsError> {
        let re = Self::compile_regex(query, case_sensitive)?;
        let mut matches = Vec::new();
        let mut cut_short = 0usize;
        let first = self.first_cached_line.min(self.line_count);
        let mut line_nums = match direction {
            SearchDirection::Forward => self.line_range(first, self.line_count),
            SearchDirection::Backward => self.line_range_rev(first, self.line_count),
        };
        'lines: while let Some(line_num) = line_nums.next_candidate() {
            #[cfg(test)]
            count_search_from_line_candidate();
            let Some(text) = self.lines.get(&line_num) else {
                continue;
            };
            let fallback;
            let col_map = match self.column_maps.get(&line_num) {
                Some(cm) => cm,
                None => {
                    fallback = ColumnMap::new(text, self.ambiguous_width);
                    &fallback
                }
            };
            let mut scan = scan_line(&re, text);
            match direction {
                SearchDirection::Forward => {
                    for cap in scan.by_ref() {
                        // Skip zero-length matches (e.g. `^`, `\b`, `x*` at
                        // non-matching positions), including byte spans that
                        // resolve to zero display columns.
                        if cap.start() == cap.end() {
                            continue;
                        }
                        let (start_col, end_col) = col_map.span(cap.start(), cap.end());
                        if start_col == end_col {
                            continue;
                        }
                        matches.push(SearchMatch::new(line_num, start_col, end_col));
                        if matches.len() >= MAX_SEARCH_MATCHES {
                            break 'lines;
                        }
                    }
                    if scan.step_limit_exceeded() && !self.truncated_lines.contains(&line_num) {
                        cut_short = cut_short.saturating_add(1);
                    }
                }
                SearchDirection::Backward => {
                    let base = matches.len();
                    let remaining = MAX_SEARCH_MATCHES.saturating_sub(base);
                    if remaining == 0 {
                        break 'lines;
                    }
                    let mut next_replace = 0;
                    for cap in scan.by_ref() {
                        if cap.start() == cap.end() {
                            continue;
                        }
                        let (start_col, end_col) = col_map.span(cap.start(), cap.end());
                        if start_col != end_col {
                            let found = SearchMatch::new(line_num, start_col, end_col);
                            if matches.len() < MAX_SEARCH_MATCHES {
                                matches.push(found);
                            } else {
                                matches[base + next_replace] = found;
                                next_replace += 1;
                                if next_replace == remaining {
                                    next_replace = 0;
                                }
                            }
                        }
                    }
                    if scan.step_limit_exceeded() && !self.truncated_lines.contains(&line_num) {
                        cut_short = cut_short.saturating_add(1);
                    }
                    // The row's suffix is a ring in the final result buffer.
                    // Restore encounter order before reversing this row.
                    matches[base..].rotate_left(next_replace);
                    matches[base..].reverse();
                    if matches.len() >= MAX_SEARCH_MATCHES {
                        break 'lines;
                    }
                }
            }
        }
        if direction == SearchDirection::Backward {
            matches.reverse();
        }
        Ok((matches, cut_short))
    }

    fn search_case_insensitive(&self, query: &str, direction: SearchDirection) -> Vec<SearchMatch> {
        let mut matcher = CaseInsensitiveMatcher::new(query);
        if matcher.is_empty() {
            return Vec::new();
        }
        let mut matches = Vec::new();

        match direction {
            SearchDirection::Forward => {
                let mut candidates = self.literal_candidates_forward(&matcher.lower_query, 0);
                while let Some(line_num) = candidates.next_candidate() {
                    #[cfg(test)]
                    count_case_insensitive_candidate_visit();
                    let Some(text) = self.lines.get(&line_num) else {
                        continue;
                    };
                    let fallback;
                    let col_map = match self.column_maps.get(&line_num) {
                        Some(cm) => cm,
                        None => {
                            fallback = ColumnMap::new(text, self.ambiguous_width);
                            &fallback
                        }
                    };
                    let visit = matcher.visit_matches::<false>(line_num, text, col_map, |found| {
                        matches.push(found);
                        matches.len() < MAX_SEARCH_MATCHES
                    });
                    if !visit.completed {
                        break;
                    }
                }
            }
            SearchDirection::Backward => {
                let mut candidates =
                    self.literal_candidates_backward(&matcher.lower_query, self.line_count);
                while let Some(line_num) = candidates.next_candidate() {
                    #[cfg(test)]
                    count_case_insensitive_candidate_visit();
                    let Some(text) = self.lines.get(&line_num) else {
                        continue;
                    };
                    let fallback;
                    let col_map = match self.column_maps.get(&line_num) {
                        Some(cm) => cm,
                        None => {
                            fallback = ColumnMap::new(text, self.ambiguous_width);
                            &fallback
                        }
                    };
                    let visit = matcher.visit_matches::<true>(line_num, text, col_map, |found| {
                        matches.push(found);
                        matches.len() < MAX_SEARCH_MATCHES
                    });
                    if !visit.completed {
                        break;
                    }
                }
                matches.reverse();
            }
        }
        matches
    }

    /// Upper bound on candidate lines this index would visit for a literal `query`.
    #[must_use]
    pub fn literal_candidate_bound(&self, query: &str, case_sensitive: bool) -> Option<u64> {
        let folded;
        let effective = if case_sensitive {
            query
        } else {
            folded = lower_fold(query);
            folded.as_str()
        };
        if effective.len() < 3 {
            return None;
        }
        if self.lines.is_empty() || !self.might_contain(effective) {
            return Some(0);
        }
        let Some(postings) = self.posting_lists(effective) else {
            return Some(0);
        };
        Some(
            postings
                .iter()
                .map(|bitmap| bitmap.len())
                .min()
                .unwrap_or(0),
        )
    }

    /// Run one incremental-search narrowing step over an optional candidate-line list.
    pub fn search_literal_narrowed(
        &self,
        query: &str,
        case_sensitive: bool,
        prev_lines: Option<&LineFrame>,
    ) -> NarrowedSearch {
        if query.is_empty() {
            return NarrowedSearch {
                results: self.with_truncated_lines(SearchResults::new(
                    Vec::new(),
                    self.results_may_be_incomplete(),
                    self.lowest_retained_line(),
                )),
                occurrence_lines: None,
            };
        }

        enum Candidates<'a, I: Iterator<Item = usize>> {
            Engine(CandidateSource<'a>),
            Frame(I),
        }
        impl<I: Iterator<Item = usize>> Candidates<'_, I> {
            fn next(&mut self) -> Option<usize> {
                match self {
                    Candidates::Engine(source) => source.next_candidate(),
                    Candidates::Frame(iter) => iter.next(),
                }
            }
        }

        let mut matcher = (!case_sensitive).then(|| CaseInsensitiveMatcher::new(query));
        let mut candidates = match prev_lines {
            Some(lines) => Candidates::Frame(lines.lines()),
            None => Candidates::Engine(match &matcher {
                Some(matcher) => self.literal_candidates_forward(&matcher.lower_query, 0),
                None => {
                    let first = self.first_cached_line.min(self.line_count);
                    if query.len() < 3 {
                        self.line_range(first, self.line_count)
                    } else if !self.might_contain(query) {
                        CandidateSource::empty()
                    } else if let Some(postings) = self.posting_lists(query) {
                        self.postings_forward(postings, first)
                    } else {
                        CandidateSource::empty()
                    }
                }
            }),
        };

        let mut matches: Vec<SearchMatch> = Vec::new();
        let mut occurrence_lines = LineFrame::with_base(self.key_base);
        let mut capped = false;
        let searcher = crate::bytesearch::Searcher::new(query.as_bytes());
        'lines: while let Some(line_num) = candidates.next() {
            let Some(text) = self.lines.get(&line_num) else {
                continue;
            };
            let fallback;
            let col_map = match self.column_maps.get(&line_num) {
                Some(map) => map,
                None => {
                    fallback = ColumnMap::new(text, self.ambiguous_width);
                    &fallback
                }
            };
            match matcher.as_mut() {
                None => {
                    let mut had_occurrence = false;
                    for at in searcher.matches_in::<false>(text.as_bytes()) {
                        let found = literal_occurrence(line_num, at, query.len(), col_map);
                        had_occurrence = true;
                        if found.start_col == found.end_col {
                            continue;
                        }
                        matches.push(found);
                        if matches.len() >= MAX_SEARCH_MATCHES {
                            occurrence_lines.push_offset(self.posting_key(line_num));
                            capped = true;
                            break 'lines;
                        }
                    }
                    if had_occurrence {
                        occurrence_lines.push_offset(self.posting_key(line_num));
                    }
                }
                Some(matcher) => {
                    let visit = matcher.visit_matches::<false>(line_num, text, col_map, |found| {
                        matches.push(found);
                        matches.len() < MAX_SEARCH_MATCHES
                    });
                    if visit.had_occurrence {
                        occurrence_lines.push_offset(self.posting_key(line_num));
                    }
                    if !visit.completed {
                        capped = true;
                        break 'lines;
                    }
                }
            }
        }

        let capped = capped || matches.len() >= MAX_SEARCH_MATCHES;
        NarrowedSearch {
            results: self.with_truncated_lines(SearchResults::new(
                matches,
                self.results_may_be_incomplete() || capped,
                self.lowest_retained_line(),
            )),
            occurrence_lines: (!capped).then_some(occurrence_lines),
        }
    }

    /// Find one regex match at the directional anchor without materializing a capped global batch.
    #[cfg(test)]
    pub(crate) fn find_regex_from(
        &self,
        query: &str,
        case_sensitive: bool,
        anchor_line: usize,
        anchor_col: usize,
        direction: SearchDirection,
        inclusive: bool,
    ) -> Result<Option<SearchMatch>, SearchOptionsError> {
        let re = Self::compile_regex(query, case_sensitive)?;
        let (found, cut_short) =
            self.find_compiled_regex_from(&re, anchor_line, anchor_col, direction, inclusive);
        Self::point_answer(found, cut_short)
    }

    fn point_answer(
        found: Option<SearchMatch>,
        cut_short: bool,
    ) -> Result<Option<SearchMatch>, SearchOptionsError> {
        if found.is_none() && cut_short {
            return Err(SearchOptionsError::ScanCutShort);
        }
        Ok(found)
    }

    /// Regex point navigation with one compilation shared by the first pass and optional wrap pass.
    pub(crate) fn find_regex_direction(
        &self,
        query: &str,
        case_sensitive: bool,
        find: DirectedFind,
    ) -> Result<Option<SearchMatch>, SearchOptionsError> {
        let DirectedFind {
            anchor: (anchor_line, anchor_col),
            direction,
            inclusive,
            wrap,
        } = find;
        let re = Self::compile_regex(query, case_sensitive)?;
        let (found, cut_short) =
            self.find_compiled_regex_from(&re, anchor_line, anchor_col, direction, inclusive);
        if found.is_some() || !wrap {
            return Self::point_answer(found, cut_short);
        }
        let (wrapped, wrap_cut_short) = match direction {
            SearchDirection::Forward => self.find_compiled_regex_from(
                &re,
                self.first_cached_line.min(self.line_count),
                0,
                direction,
                true,
            ),
            SearchDirection::Backward => self.find_compiled_regex_from(
                &re,
                self.line_count.saturating_sub(1),
                usize::MAX,
                direction,
                true,
            ),
        };
        Self::point_answer(wrapped, cut_short || wrap_cut_short)
    }

    fn find_compiled_regex_from(
        &self,
        re: &aterm_regex::Regex,
        anchor_line: usize,
        anchor_col: usize,
        direction: SearchDirection,
        inclusive: bool,
    ) -> (Option<SearchMatch>, bool) {
        let mut cut_short = false;
        let lower = self.first_cached_line.min(self.line_count);
        let mut candidates = match direction {
            SearchDirection::Forward => self.line_range(anchor_line.max(lower), self.line_count),
            SearchDirection::Backward => {
                self.line_range_rev(lower, anchor_line.saturating_add(1).min(self.line_count))
            }
        };
        while let Some(line_num) = candidates.next_candidate() {
            let Some(text) = self.lines.get(&line_num) else {
                continue;
            };
            let fallback;
            let col_map = match self.column_maps.get(&line_num) {
                Some(map) => map,
                None => {
                    fallback = ColumnMap::new(text, self.ambiguous_width);
                    &fallback
                }
            };
            let mut scan = scan_line(re, text);
            match direction {
                SearchDirection::Forward => {
                    for cap in scan.by_ref() {
                        if cap.start() == cap.end() {
                            continue;
                        }
                        let (start_col, end_col) = col_map.span(cap.start(), cap.end());
                        if start_col == end_col {
                            continue;
                        }
                        let qualifies = line_num > anchor_line
                            || if inclusive {
                                start_col >= anchor_col
                            } else {
                                start_col > anchor_col
                            };
                        if qualifies {
                            return (
                                Some(SearchMatch::new(line_num, start_col, end_col)),
                                cut_short,
                            );
                        }
                    }
                    cut_short |= scan.step_limit_exceeded();
                }
                SearchDirection::Backward => {
                    let mut found = None;
                    for cap in scan.by_ref() {
                        if cap.start() == cap.end() {
                            continue;
                        }
                        let (start_col, end_col) = col_map.span(cap.start(), cap.end());
                        if start_col == end_col {
                            continue;
                        }
                        let qualifies = line_num < anchor_line
                            || if inclusive {
                                start_col <= anchor_col
                            } else {
                                start_col < anchor_col
                            };
                        if !qualifies && line_num == anchor_line {
                            break;
                        }
                        if qualifies {
                            found = Some(SearchMatch::new(line_num, start_col, end_col));
                        }
                    }
                    cut_short |= scan.step_limit_exceeded();
                    if found.is_some() {
                        return (found, cut_short);
                    }
                }
            }
        }
        (None, cut_short)
    }

    /// Find the next case-insensitive literal match without building a batch.
    #[cfg(test)]
    pub(crate) fn find_next_case_insensitive(
        &self,
        query: &str,
        after_line: usize,
        after_col: usize,
    ) -> Option<SearchMatch> {
        self.find_next_case_insensitive_from(query, after_line, after_col, false)
    }

    pub(crate) fn find_next_case_insensitive_from(
        &self,
        query: &str,
        anchor_line: usize,
        anchor_col: usize,
        inclusive: bool,
    ) -> Option<SearchMatch> {
        let mut matcher = CaseInsensitiveMatcher::new(query);
        if matcher.is_empty() {
            return None;
        }
        let mut candidates = self.literal_candidates_forward(&matcher.lower_query, anchor_line);
        while let Some(line_num) = candidates.next_candidate() {
            #[cfg(test)]
            count_case_insensitive_candidate_visit();
            let Some(text) = self.lines.get(&line_num) else {
                continue;
            };
            let fallback;
            let col_map = match self.column_maps.get(&line_num) {
                Some(cm) => cm,
                None => {
                    fallback = ColumnMap::new(text, self.ambiguous_width);
                    &fallback
                }
            };
            let mut found = None;
            matcher.visit_matches_anchored::<false>(
                line_num,
                text,
                col_map,
                (line_num == anchor_line).then_some((anchor_col, inclusive)),
                |candidate| {
                    if candidate.line > anchor_line
                        || (candidate.line == anchor_line
                            && if inclusive {
                                candidate.start_col >= anchor_col
                            } else {
                                candidate.start_col > anchor_col
                            })
                    {
                        found = Some(candidate);
                        false
                    } else {
                        true
                    }
                },
            );
            if found.is_some() {
                return found;
            }
        }
        None
    }

    /// Find the previous case-insensitive literal match without building a batch.
    #[cfg(test)]
    pub(crate) fn find_prev_case_insensitive(
        &self,
        query: &str,
        before_line: usize,
        before_col: usize,
    ) -> Option<SearchMatch> {
        self.find_prev_case_insensitive_from(query, before_line, before_col, false)
    }

    pub(crate) fn find_prev_case_insensitive_from(
        &self,
        query: &str,
        anchor_line: usize,
        anchor_col: usize,
        inclusive: bool,
    ) -> Option<SearchMatch> {
        let mut matcher = CaseInsensitiveMatcher::new(query);
        if matcher.is_empty() {
            return None;
        }
        let exclusive_upper = anchor_line.saturating_add(1);
        let mut candidates =
            self.literal_candidates_backward(&matcher.lower_query, exclusive_upper);
        while let Some(line_num) = candidates.next_candidate() {
            #[cfg(test)]
            count_case_insensitive_candidate_visit();
            if line_num == anchor_line && anchor_col == 0 && !inclusive {
                continue;
            }
            let Some(text) = self.lines.get(&line_num) else {
                continue;
            };
            let fallback;
            let col_map = match self.column_maps.get(&line_num) {
                Some(cm) => cm,
                None => {
                    fallback = ColumnMap::new(text, self.ambiguous_width);
                    &fallback
                }
            };
            let mut found = None;
            matcher.visit_matches_anchored::<true>(
                line_num,
                text,
                col_map,
                (line_num == anchor_line).then_some((anchor_col, inclusive)),
                |candidate| {
                    if candidate.line < anchor_line
                        || (candidate.line == anchor_line
                            && if inclusive {
                                candidate.start_col <= anchor_col
                            } else {
                                candidate.start_col < anchor_col
                            })
                    {
                        found = Some(candidate);
                        false
                    } else {
                        true
                    }
                },
            );
            if found.is_some() {
                return found;
            }
        }
        None
    }
}

impl Default for SearchIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "index_bloom_tests.rs"]
mod index_bloom_tests;

#[cfg(test)]
mod case_insensitive_preparation_tests {
    use super::*;

    #[test]
    fn row_modes_and_directions_prepare_once_per_query() {
        let mut matcher = CaseInsensitiveMatcher::new("AABA");
        let before = crate::bytesearch::factorizations();
        for row in 0..128 {
            for text in ["x aABA y aaba z", "é aABA y aaba z"] {
                let map = ColumnMap::new(text, AmbiguousWidth::Single);
                let mut forward = Vec::new();
                let visit = matcher.visit_matches::<false>(row, text, &map, |found| {
                    forward.push((found.start_col, found.end_col));
                    true
                });
                assert!(visit.completed && visit.had_occurrence);
                assert_eq!(forward, [(2, 6), (9, 13)]);
                let mut backward = Vec::new();
                let visit = matcher.visit_matches::<true>(row, text, &map, |found| {
                    backward.push((found.start_col, found.end_col));
                    true
                });
                assert!(visit.completed && visit.had_occurrence);
                assert_eq!(backward, [(9, 13), (2, 6)]);
            }
        }
        let prepared = crate::bytesearch::factorizations() - before;
        assert!(prepared <= 4, "prepared {prepared} times for one query");
    }
}

/// The regex scan budget at the entry points that run a search-box pattern over
/// terminal content (`REGEX_STEP_LIMIT` derives it).
#[cfg(test)]
mod regex_scan_budget_tests {
    use super::*;

    const EXPENSIVE: &str = "(?:x?){1020}z";

    fn index_with_a_wide_line() -> SearchIndex {
        let mut index = SearchIndex::new();
        index.index_line(0, "an ordinary line of terminal output");
        index.index_line(1, &"x".repeat(4096));
        index.index_line(2, "another ordinary line");
        index
    }

    #[test]
    fn a_pattern_that_cannot_finish_a_line_is_reported_never_passed_off() {
        let mut index = index_with_a_wide_line();
        index.index_line(3, "a z at the end");
        let got = index
            .search_results_opts(EXPENSIVE, true, true)
            .expect("a line out of budget is reported, not refused");
        assert_eq!(got.cut_short_lines, 1, "{got:?}");
        assert!(
            got.incomplete,
            "a cut-short line makes the results incomplete"
        );
        assert_eq!(
            got.matches.iter().map(|m| m.line).collect::<Vec<_>>(),
            vec![3],
            "the other lines are still searched"
        );

        let err = index
            .search_with_positions_opts(EXPENSIVE, true, true)
            .expect_err("a matches-only answer cannot say a line was cut short");
        assert_eq!(err, SearchOptionsError::ScanCutShort);

        let find = |index: &SearchIndex, anchor: (usize, usize)| {
            index.find_regex_direction(
                EXPENSIVE,
                true,
                DirectedFind {
                    anchor,
                    direction: SearchDirection::Forward,
                    inclusive: true,
                    wrap: true,
                },
            )
        };
        assert_eq!(
            find(&index, (0, 0)).map(|m| m.map(|m| m.line)),
            Ok(Some(3)),
            "the walk goes on past the line it could not finish"
        );
        let wide_only = index_with_a_wide_line();
        assert_eq!(
            find(&wide_only, (0, 0)),
            Err(SearchOptionsError::ScanCutShort),
            "no match found, but the wide line's rest is unread: not \"no match\""
        );
    }

    #[test]
    fn a_line_read_short_of_its_end_is_counted_cut_short() {
        let wide = "x".repeat(4096);
        let mut cut = LineCells::identity(wide.len());
        cut.mark_truncated();
        let queries = [
            ("ordinary", true, false),
            ("ORDINARY", false, false),
            ("ordin[a-z]+", true, true),
            (EXPENSIVE, true, true),
        ];
        let mut index = index_with_a_wide_line();
        let whole: Vec<usize> = queries
            .iter()
            .map(|&(q, cs, rx)| {
                index
                    .search_results_opts(q, cs, rx)
                    .unwrap()
                    .cut_short_lines
            })
            .collect();
        assert_eq!(whole, [0, 0, 0, 1]);

        index.index_line_cow(1, Cow::Borrowed(&wide), Some(&cut));
        for &(query, case_sensitive, is_regex) in &queries {
            let got = index
                .search_results_opts(query, case_sensitive, is_regex)
                .expect("searched");
            assert_eq!(got.cut_short_lines, 1, "{query:?}: {got:?}");
            assert!(got.incomplete, "{query:?}");
        }
        for case_sensitive in [true, false] {
            assert_eq!(
                index
                    .search_literal_narrowed("ordinary", case_sensitive, None)
                    .results,
                index
                    .search_results_opts("ordinary", case_sensitive, false)
                    .unwrap(),
                "narrowed equals batch"
            );
        }

        index.index_line_cow(
            1,
            Cow::Borrowed(&wide),
            Some(&LineCells::identity(wide.len())),
        );
        let got = index.search_results_opts("ordinary", true, false).unwrap();
        assert_eq!((got.cut_short_lines, got.incomplete), (0, false));

        index.index_line_cow(1, Cow::Borrowed(&wide), Some(&cut));
        index.drop_history_below(2);
        let got = index.search_results_opts("ordinary", true, false).unwrap();
        assert_eq!((got.cut_short_lines, got.incomplete), (0, false));
        let mut evicting = SearchIndex::with_max_cached_lines(4);
        let mut cut_ordinary = LineCells::identity("ordinary".len());
        cut_ordinary.mark_truncated();
        evicting.index_line_cow(0, Cow::Borrowed("ordinary"), Some(&cut_ordinary));
        for line in 1..=4 {
            evicting.index_line(line, "ordinary");
        }
        assert!(evicting.lowest_retained_line() > 0, "line 0 was evicted");
        let got = evicting
            .search_results_opts("ordinary", true, false)
            .unwrap();
        assert_eq!(got.cut_short_lines, 0, "{got:?}");
    }

    #[test]
    fn the_line_budget_scales_with_the_line_under_a_ceiling() {
        assert_eq!(regex_line_budget(0), REGEX_STEP_LIMIT);
        assert_eq!(regex_line_budget(120), REGEX_STEP_LIMIT);
        assert_eq!(regex_line_budget(4096), REGEX_STEP_LIMIT);
        assert_eq!(regex_line_budget(8192), 2 * REGEX_STEP_LIMIT);
        assert_eq!(regex_line_budget(64 * 1024), REGEX_LINE_STEP_CEILING);
        assert_eq!(regex_line_budget(4_300_000), REGEX_LINE_STEP_CEILING);
        assert_eq!(regex_line_budget(usize::MAX), REGEX_LINE_STEP_CEILING);
        let mut last = 0;
        for len in (0..200_000).step_by(997) {
            let budget = regex_line_budget(len);
            assert!(budget >= last, "monotone at {len}");
            assert!(
                budget
                    <= REGEX_STEP_PER_BYTE
                        .saturating_mul(len as u64)
                        .max(REGEX_STEP_LIMIT),
                "never more per byte than a full-width row at {len}"
            );
            last = budget;
        }
    }

    #[test]
    fn ordinary_patterns_over_a_long_joined_line_find_their_matches() {
        let unit = "function a(b){return b+1};var needle_x=[1,2,3];";
        for (len, pattern, case_sensitive) in [
            (100_000, "needle", true),
            (1_000_000, r"\w+\(", true),
            (1_000_000, "[a-z]+=", true),
            (4_300_000, "needle", true),
            (4_300_000, "NEEDLE_X", false),
        ] {
            let mut long = unit.repeat(len / unit.len() + 1);
            long.truncate(len);
            let units = len / unit.len();
            let mut index = SearchIndex::new();
            index.index_line(0, "$ echo needle one");
            index.index_line(1, &long);
            index.index_line(2, "$ echo needle after");
            let got = index
                .search_results_opts(pattern, case_sensitive, true)
                .unwrap_or_else(|e| panic!("{len} {pattern:?} must not be refused: {e}"));
            assert_eq!(got.cut_short_lines, 0, "{len} {pattern:?}");
            assert!(!got.incomplete, "{len} {pattern:?}");
            let on_long = got.matches.iter().filter(|m| m.line == 1).count();
            assert!(
                on_long >= units,
                "{len} {pattern:?}: {on_long} of {units}+ on the long line"
            );
            let short_lines = [0, 2].map(|line| got.matches.iter().any(|m| m.line == line));
            assert_eq!(
                short_lines,
                [pattern == "needle"; 2],
                "{len} {pattern:?}: the short lines around it"
            );
        }
    }

    #[test]
    fn the_pathological_pattern_over_a_long_line_stops_at_the_ceiling() {
        let long = "x".repeat(4_300_000);
        assert_eq!(regex_line_budget(long.len()), REGEX_LINE_STEP_CEILING);
        let mut index = SearchIndex::new();
        index.index_line(0, "before z");
        index.index_line(1, &long);
        index.index_line(2, "after z");
        let got = index
            .search_results_opts(EXPENSIVE, true, true)
            .expect("reported, not refused");
        assert_eq!(got.cut_short_lines, 1);
        assert!(got.incomplete);
        assert_eq!(
            got.matches.iter().map(|m| m.line).collect::<Vec<_>>(),
            vec![0, 2]
        );
    }
}

#[cfg(test)]
#[path = "index_key_window_tests.rs"]
mod index_key_window_tests;
