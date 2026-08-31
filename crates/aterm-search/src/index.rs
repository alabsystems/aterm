// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Core trigram search index with bloom filter acceleration.

use std::borrow::Cow;

use aterm_hash::{FxBuildHasher, FxHashMap, FxHashSet};

use crate::bitmap::SparseBitmap;

use super::bloom::BloomFilter;
use super::iterators::{
    CandidateSource, SearchMatchIterator, SearchMatchReverseIterator, next_literal_match,
};
use super::types::{DirectedFind, SearchDirection, SearchMatch, SearchResult, SearchResults};
use crate::grapheme::{
    ColumnMap, LowerByteMap, LowerNeed, lower_fold, lower_fold_char, lower_fold_into, lower_need,
};
use crate::literal::AsciiCaseInsensitiveMatches;

/// Default maximum number of lines to keep in the search index cache.
/// Eviction triggers when cache exceeds this limit, removing the oldest 25%.
///
/// This is the *default* only; callers that need a different bound should use
/// [`SearchIndex::with_max_cached_lines`] (or
/// [`SearchIndex::set_max_cached_lines`]). The default value is unchanged from
/// the original hard-coded constant — behavior is preserved; the cap is now
/// configurable and eviction is now observable (see
/// [`SearchIndex::results_may_be_incomplete`]).
pub const DEFAULT_MAX_CACHED_LINES: usize = 100_000;

/// Upper bound on the number of matches a single batch query
/// ([`SearchIndex::search_with_positions`], `search_case_insensitive`,
/// `search_regex`) will collect before it stops scanning.
///
/// Without this bound, a query for a short, highly repetitive substring over a
/// full scrollback (up to [`DEFAULT_MAX_CACHED_LINES`] lines, each up to the
/// terminal width in matches) allocates one [`SearchMatch`] per occurrence —
/// on the order of 10^7–10^8 records (hundreds of MB to >1 GB, ~24 B each) for a
/// single query, all under the terminal lock. Batch queries cap out here;
/// [`SearchIndex::search_results_opts`] reports the truncation via the same
/// `incomplete` signal used for eviction so callers (cmd_search / the AI) do not
/// treat a capped result set as exhaustive.
///
/// The value is chosen well above realistic result sets (a common single-char
/// term over a 10K-line buffer is ~70K matches, all legitimately returned) so
/// interactive highlight/count behavior is unchanged, while still bounding the
/// worst case to ~2.4 MB of `SearchMatch` records instead of >1 GB.
pub const MAX_SEARCH_MATCHES: usize = 100_000;

/// Eviction low-water mark as a fraction `NUM/DEN` of `max_cached_lines`:
/// [`SearchIndex::index_line`] drops the cache to
/// `EVICTION_RETAIN_NUM/EVICTION_RETAIN_DEN · max_cached_lines` once it exceeds
/// capacity (hysteresis, so eviction isn't re-triggered every single line), always
/// keeping the NEWEST lines. A caller that must guarantee a FLOOR of retained lines
/// — e.g. the terminal's visible screen, which must stay searchable no matter how
/// small the configured history cap — sizes its capacity via
/// [`max_cached_for_retained`], the inverse of this mark. Single source of truth for
/// the ratio: [`evict_oldest_lines`], `max_cached_for_retained`, and the
/// lifecycle oracle's cap-eviction mirror all read it.
pub(crate) const EVICTION_RETAIN_NUM: usize = 3;
pub(crate) const EVICTION_RETAIN_DEN: usize = 4;

/// Number of oldest rows that will be absent after indexing `total_lines`
/// distinct, ascending line numbers into an index with `max_cached_lines`.
///
/// The budgeted builder uses this to avoid verifying rows that the completed
/// index will evict.  That is more than an optimization: if the global match
/// cap is reached in those doomed rows, stopping verification there would
/// otherwise omit newer matches that the final one-shot index still sees.
/// Keep this calculation coupled to [`SearchIndex::evict_oldest_lines`] by
/// deriving it from the same low-water constants.
pub(crate) fn final_evicted_prefix(total_lines: usize, max_cached_lines: usize) -> usize {
    let max_cached_lines = max_cached_lines.max(1);
    if total_lines <= max_cached_lines {
        return 0;
    }

    let target = max_cached_lines.saturating_mul(EVICTION_RETAIN_NUM) / EVICTION_RETAIN_DEN;
    // Each eviction occurs immediately after the cache grows to max + 1 and
    // removes exactly this many distinct ascending rows.  The subtraction is
    // strictly positive on this branch (including the max=1/target=0 edge).
    let rows_per_eviction = max_cached_lines.saturating_add(1).saturating_sub(target);
    let rows_after_first = total_lines.saturating_sub(max_cached_lines.saturating_add(1));
    let eviction_count = 1usize.saturating_add(rows_after_first / rows_per_eviction);
    eviction_count
        .saturating_mul(rows_per_eviction)
        .min(total_lines)
}

/// The smallest `max_cached_lines` capacity that keeps the newest `retained` lines
/// safe from eviction. Because [`SearchIndex::index_line`] evicts down to a
/// `EVICTION_RETAIN_NUM/EVICTION_RETAIN_DEN` (3/4) low-water mark, a cap of exactly
/// `retained` would let the oldest `retained/4` of those lines be dropped; this
/// returns the inverse — the smallest `max` with `floor(3·max/4) >= retained`.
///
/// Used to keep an always-searchable window (the live terminal screen) indexed even
/// under a tiny configured scrollback cap: `max_cached_lines >=
/// max_cached_for_retained(visible_rows)` guarantees the whole visible screen
/// survives eviction. `retained == 0` returns 0 (nothing to protect).
#[must_use]
pub fn max_cached_for_retained(retained: usize) -> usize {
    // floor(NUM·max/DEN) >= retained  ⟺  max >= ceil(DEN·retained/NUM); and for an
    // integer target `retained`, floor(NUM·max/DEN) >= retained ⟺ NUM·max/DEN >= retained.
    retained
        .saturating_mul(EVICTION_RETAIN_DEN)
        .div_ceil(EVICTION_RETAIN_NUM)
}

// Deterministic test instrumentation for range-query candidate counts.
//
// This tracks how many candidate line IDs a lazy navigation iterator actually
// visits, allowing scaling tests to verify that `find_next` does not depend on
// prefix matches before the search start line.
#[cfg(test)]
thread_local! {
    static SEARCH_FROM_LINE_CANDIDATES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static CASE_INSENSITIVE_CANDIDATE_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static CASE_INSENSITIVE_LOWERED_LINES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static OWNED_INTERSECTION_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
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

/// Search index using trigrams with bloom filter acceleration.
///
/// The index maintains:
/// - A bloom filter for instant negative lookups
/// - A trigram map for candidate line identification
/// - Line content cache for match verification (with capacity-based eviction)
#[derive(Debug)]
pub struct SearchIndex {
    /// Bloom filter for fast negative lookups.
    bloom: BloomFilter,
    /// Trigram -> line numbers mapping.
    trigrams: FxHashMap<[u8; 3], SparseBitmap>,
    /// Cached line content for match verification.
    /// Maps line number to line text. Evicted when exceeding `max_cached_lines`.
    pub(super) lines: FxHashMap<usize, String>,
    /// Cached column maps for search hit lines.
    /// Built at index time and reused across searches to avoid O(G)-per-query
    /// reconstruction (#7373).
    pub(super) column_maps: FxHashMap<usize, ColumnMap>,
    /// Reused lowering buffer for the non-ASCII lowered-trigram pass.
    ///
    /// Same trick as [`CaseInsensitiveMatcher::lower_buf`]: the lowered text is
    /// walked once and thrown away, so keeping ONE buffer whose capacity
    /// survives across lines turns a malloc/realloc/free chain per indexed line
    /// into a memcpy into already-owned memory. Purely a scratch cell — never
    /// read outside the call that fills it, so it carries no state.
    lower_scratch: String,
    /// Total number of indexed lines.
    line_count: usize,
    /// Lowest line number currently present in `lines`.
    ///
    /// Separate from the public eviction watermark: sparse visible-content
    /// indexing can begin above line zero without an eviction ever occurring.
    /// Short-query ranges start here so they do not walk an empty prefix.
    first_cached_line: usize,
    /// Next line number to index (for incremental indexing).
    next_line: usize,
    /// Maximum lines to keep in cache before eviction.
    max_cached_lines: usize,
    /// Lowest line number still retained in the index.
    ///
    /// Starts at 0 and advances each time eviction drops the oldest cached
    /// lines. Any match at a line below this watermark has been evicted and can
    /// no longer be returned, even though [`len`](Self::len) (which tracks the
    /// highest indexed line) keeps growing. Exposed via
    /// [`lowest_retained_line`](Self::lowest_retained_line) so callers can tell
    /// the AI which range of scrollback is actually searchable.
    lowest_retained_line: usize,
    /// Whether eviction has ever dropped lines from this index.
    ///
    /// Once true, search results may be incomplete: matches in evicted lines
    /// are silently absent. Surfaced via
    /// [`results_may_be_incomplete`](Self::results_may_be_incomplete) so the
    /// future `cmd_search` can flag truncated results to the AI rather than
    /// presenting them as exhaustive.
    eviction_occurred: bool,
    /// Guards the one-time `aterm_log` warning emitted on first eviction.
    first_eviction_warned: bool,
    /// Whether this index maintains the trigram postings and the bloom filter.
    ///
    /// `true` for every index built through the public constructors — the query
    /// pipeline needs both. `false` only for the private columns-only index
    /// ([`columns_only_with_max_cached_lines`](Self::columns_only_with_max_cached_lines))
    /// the budgeted engine builds, which reads back nothing but `column_maps`.
    /// Never mutated after construction; it is a mode, not state, so `clear`
    /// and `release` leave it alone.
    maintain_trigrams: bool,
}

/// Convert a line number to u32 for `SparseBitmap` storage.
///
/// Line numbers are bounded by scrollback limits (max ~1M lines) which fits
/// in u32 (max ~4B). Saturates at `u32::MAX` for defensive safety.
fn line_as_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Intersect a pre-sorted (ascending by len) slice of posting list references
/// into a single owned bitmap.
///
/// Avoids cloning the smallest list when there are 2+ lists: the first
/// intersection uses `&a & &b` (borrowed on both sides) which constructs the
/// result directly. Only when there is exactly one list does a clone occur,
/// because we need an owned bitmap for downstream consumers (#7375).
fn intersect_posting_lists(sorted_lists: &[&SparseBitmap]) -> SparseBitmap {
    #[cfg(test)]
    OWNED_INTERSECTION_BUILDS.with(|count| count.set(count.get().saturating_add(1)));
    debug_assert!(!sorted_lists.is_empty());
    if sorted_lists.len() == 1 {
        return sorted_lists[0].clone();
    }
    // First pair: borrow-borrow intersection avoids cloning the smallest list.
    let mut result: SparseBitmap = sorted_lists[0] & sorted_lists[1];
    for bitmap in &sorted_lists[2..] {
        result &= *bitmap;
    }
    result
}

/// Reusable per-query case-insensitive line matcher.
///
/// One- and two-byte ASCII queries take the allocation-free byte-scanner path. Longer
/// queries use the standard library's linear-time substring search over one
/// reusable lowercase buffer. Unicode lowercasing can change byte length and
/// therefore additionally requires a [`LowerByteMap`] to preserve original
/// display columns.
///
/// `pub(crate)` so the budgeted engine ([`crate::BudgetedSearch`]) verifies each
/// row with the EXACT matcher the batch path uses (results-equality by
/// construction, not by reimplementation).
pub(crate) struct CaseInsensitiveMatcher {
    lower_query: String,
    lower_buf: String,
}

impl CaseInsensitiveMatcher {
    pub(crate) fn new(query: &str) -> Self {
        Self {
            lower_query: lower_fold(query),
            lower_buf: String::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.lower_query.is_empty()
    }

    /// Visit matches in ascending byte/column order.
    ///
    /// Returns `false` when `visit` requests early termination.
    #[inline]
    pub(crate) fn visit_matches(
        &mut self,
        line_num: usize,
        text: &str,
        col_map: &ColumnMap,
        mut visit: impl FnMut(SearchMatch) -> bool,
    ) -> bool {
        if self.lower_query.is_empty() {
            return true;
        }

        let ascii = text.is_ascii() && self.lower_query.is_ascii();
        if ascii && self.lower_query.len() <= 2 {
            for abs_pos in AsciiCaseInsensitiveMatches::new(text, &self.lower_query) {
                let Some(match_end) = abs_pos.checked_add(self.lower_query.len()) else {
                    break;
                };
                let start_col = col_map.byte_to_column(abs_pos);
                let end_col = col_map.byte_to_column(match_end);
                if start_col != end_col && !visit(SearchMatch::new(line_num, start_col, end_col)) {
                    return false;
                }
            }
            return true;
        }

        #[cfg(test)]
        count_case_insensitive_lowered_line();

        self.lower_buf.clear();
        if ascii {
            self.lower_buf.extend(
                text.bytes()
                    .map(|byte| char::from(byte.to_ascii_lowercase())),
            );
        } else {
            self.lower_buf
                .extend(text.chars().flat_map(lower_fold_char));
        }
        let byte_map = (!ascii).then(|| LowerByteMap::new(text));
        let mut start = 0;
        while let Some(tail) = self.lower_buf.get(start..) {
            let Some(pos) = tail.find(&self.lower_query) else {
                break;
            };
            let Some(abs_pos) = start.checked_add(pos) else {
                break;
            };
            let Some(match_end) = abs_pos.checked_add(self.lower_query.len()) else {
                break;
            };
            let orig_start = byte_map
                .as_ref()
                .map_or(abs_pos, |map| map.map_to_original(abs_pos));
            let orig_end = byte_map
                .as_ref()
                .map_or(match_end, |map| map.map_to_original(match_end));
            let start_col = col_map.byte_to_column(orig_start);
            let end_col = col_map.byte_to_column(orig_end);
            if start_col != end_col && !visit(SearchMatch::new(line_num, start_col, end_col)) {
                return false;
            }
            // Advance by one lowered character to preserve overlapping matches
            // while remaining on a UTF-8 boundary.
            let step = self
                .lower_buf
                .get(abs_pos..)
                .and_then(|s| s.chars().next())
                .map_or(1, char::len_utf8);
            let Some(next_start) = abs_pos.checked_add(step) else {
                break;
            };
            start = next_start;
        }
        true
    }

    /// Fold-level occurrence: does `text` contain the folded query AT ALL —
    /// including occurrences whose original span resolves to ZERO display
    /// columns, which [`visit_matches`](Self::visit_matches) deliberately
    /// drops from results (`start_col == end_col`)?
    ///
    /// Incremental narrowing frames (isearch, SA-1) must be built from
    /// OCCURRENCES, not reported matches. A line can hold a genuine
    /// occurrence of `q` and report NO match, while `q + c` reports one:
    /// inside a prepend cluster (U+0600 + digit) or across a zero-width
    /// character (U+200B) or a combining mark, the short query spans no
    /// display column and the one-char-longer query crosses the boundary and
    /// does — so a reported-match frame would silently lose that line.
    /// Fold-level containment carries the exact prefix property narrowing
    /// rests on (`lower_fold(q + c) == lower_fold(q) + lower_fold(c)`,
    /// per-char by construction of [`lower_fold_into`]), with no
    /// width/column semantics in the argument at all.
    pub(crate) fn has_occurrence(&mut self, text: &str) -> bool {
        if self.lower_query.is_empty() {
            return false;
        }
        if text.is_ascii() && self.lower_query.is_ascii() {
            // Allocation-free ASCII containment (any needle length): per-byte
            // ASCII lowering IS the fold for ASCII text, so this equals
            // "folded text contains folded query" without materializing either.
            return AsciiCaseInsensitiveMatches::new(text, &self.lower_query)
                .next()
                .is_some();
        }
        // Same fold the scan buffer and the indexed trigrams use, so the
        // occurrence notion is byte-identical to the matcher's.
        self.lower_buf.clear();
        self.lower_buf
            .extend(text.chars().flat_map(lower_fold_char));
        self.lower_buf.contains(self.lower_query.as_str())
    }
}

impl SearchIndex {
    #[cfg(feature = "regex")]
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
            .dfa_size_limit(REGEX_DFA_SIZE_LIMIT)
            .step_limit(REGEX_STEP_LIMIT)
            .build()
            .map_err(|e| SearchOptionsError::InvalidRegex(e.to_string()))
    }

    /// The error a scan that ran out of budget reports.
    ///
    /// It is deliberately the same variant a malformed pattern gets: from the
    /// caller's side both mean "this pattern cannot be used", both are shown to
    /// whoever typed it, and every call site already handles it. What it must
    /// never be is `Ok` with a short list — the matches such a scan found are a
    /// prefix of the truth, and a prefix presented as the whole is the wrong
    /// answer this budget exists to prevent.
    #[cfg(feature = "regex")]
    fn regex_scan_budget_exhausted() -> SearchOptionsError {
        SearchOptionsError::InvalidRegex(format!(
            "pattern is too expensive to scan: it exhausted the {REGEX_STEP_LIMIT}-unit \
             per-line budget before finishing a line, so the results would be incomplete"
        ))
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
    /// One- and two-byte ASCII queries stay on the allocation-free short path;
    /// longer ASCII and all Unicode queries increment this counter.
    #[cfg(test)]
    pub(crate) fn take_case_insensitive_lowered_lines() -> usize {
        CASE_INSENSITIVE_LOWERED_LINES.with(|c| {
            let value = c.get();
            c.set(0);
            value
        })
    }

    /// Take (read and reset) the count of owned posting-list intersections.
    /// Point navigation must keep this at zero: it uses borrowed, range-bounded
    /// posting iterators and stops at the first verified match.
    #[cfg(test)]
    pub(crate) fn take_owned_intersection_builds() -> usize {
        OWNED_INTERSECTION_BUILDS.with(|count| {
            let value = count.get();
            count.set(0);
            value
        })
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

    /// Create a new search index with the default cache cap
    /// ([`DEFAULT_MAX_CACHED_LINES`]).
    #[must_use]
    pub fn new() -> Self {
        Self {
            bloom: BloomFilter::with_capacity(100_000),
            trigrams: FxHashMap::default(),
            lines: FxHashMap::default(),
            column_maps: FxHashMap::default(),
            lower_scratch: String::new(),
            line_count: 0,
            first_cached_line: usize::MAX,
            next_line: 0,
            max_cached_lines: DEFAULT_MAX_CACHED_LINES,
            lowest_retained_line: 0,
            eviction_occurred: false,
            first_eviction_warned: false,
            maintain_trigrams: true,
        }
    }

    /// Create a new search index with expected capacity.
    ///
    /// Uses the default cache cap ([`DEFAULT_MAX_CACHED_LINES`]); pair with
    /// [`set_max_cached_lines`](Self::set_max_cached_lines) or use
    /// [`with_capacity_and_max`](Self::with_capacity_and_max) to override it.
    #[must_use]
    pub fn with_capacity(expected_lines: usize) -> Self {
        Self::with_capacity_and_max(expected_lines, DEFAULT_MAX_CACHED_LINES)
    }

    /// Create a new search index with an explicit cache cap.
    ///
    /// `max_cached_lines` is the number of distinct line entries kept before
    /// eviction drops the oldest 25%. A value of 0 is clamped to 1 so the index
    /// can always hold the most recent line. See
    /// [`results_may_be_incomplete`](Self::results_may_be_incomplete) for the
    /// eviction signal.
    #[must_use]
    pub fn with_max_cached_lines(max_cached_lines: usize) -> Self {
        let mut index = Self::new();
        index.set_max_cached_lines(max_cached_lines);
        index
    }

    /// Create a new search index with expected capacity and an explicit cap.
    #[must_use]
    pub fn with_capacity_and_max(expected_lines: usize, max_cached_lines: usize) -> Self {
        // Bound the map capacity HINTS so the Trust L0 gate can prove the
        // pre-allocations finite. Hints only affect pre-reservation, never
        // observable behavior: the maps grow on demand past the hint exactly
        // as before, and real callers pass line counts far below this bound
        // (line numbers are bounded by scrollback limits, max ~1M lines).
        const MAX_CAPACITY_HINT: usize = 1 << 20;
        let line_hint = if expected_lines > MAX_CAPACITY_HINT {
            MAX_CAPACITY_HINT
        } else {
            expected_lines
        };
        // Branch-duplicated construction, equivalent to the previous
        // `BloomFilter::with_capacity(expected_lines.max(1000))` on every
        // input: the middle arm's cap already saturates the filter's internal
        // MAX_BITS size cap, so it constructs the identical filter for any
        // larger input. Spelled as a three-way branch on the raw parameter
        // (no phi-merged clamp locals) because the Trust L0 gate's allocation
        // recognizer needs the comparisons to directly dominate the
        // allocating call with the count in the compared local itself.
        let bloom = if expected_lines < 1000 {
            BloomFilter::with_capacity(1000)
        } else if expected_lines > BloomFilter::MAX_EFFECTIVE_CAPACITY {
            BloomFilter::with_capacity(BloomFilter::MAX_EFFECTIVE_CAPACITY)
        } else {
            BloomFilter::with_capacity(expected_lines)
        };
        Self {
            bloom,
            trigrams: FxHashMap::with_capacity_and_hasher(line_hint / 10, FxBuildHasher),
            lines: FxHashMap::with_capacity_and_hasher(line_hint, FxBuildHasher),
            column_maps: FxHashMap::with_capacity_and_hasher(line_hint, FxBuildHasher),
            lower_scratch: String::new(),
            line_count: 0,
            first_cached_line: usize::MAX,
            next_line: 0,
            max_cached_lines: max_cached_lines.max(1),
            lowest_retained_line: 0,
            eviction_occurred: false,
            first_eviction_warned: false,
            maintain_trigrams: true,
        }
    }

    /// A line/column-map cache with NO trigram postings and NO bloom filter.
    ///
    /// The budgeted engine ([`crate::BudgetedSearch`]) verifies every row it
    /// feeds directly against that row's text and its cached [`ColumnMap`] — it
    /// deliberately never re-enters the query pipeline (see the comment in
    /// `BudgetedSearch::verify_row`). So every trigram insert, every bloom bit
    /// and, worst, every `rebuild_bloom` sweep it paid was dead work — and the
    /// sweep is O(all cached lines) landing inside a turn the caller sized for a
    /// handful of rows, exactly the stall the budgeted API exists to prevent.
    ///
    /// Everything the budgeted engine *does* observe is untouched: `lines`,
    /// `column_maps`, the `first_cached_line`/`line_count`/`next_line`
    /// counters and the eviction schedule (including `lowest_retained_line`)
    /// all run through the same code as a full index, so the watermark still
    /// matches `final_evicted_prefix`'s closed form line for line.
    ///
    /// Deliberately NOT public: querying an index built this way returns EMPTY
    /// results, because the postings the query pipeline consults do not exist.
    /// The query entry points `debug_assert!` the flag so a future refactor
    /// that queries such an index trips in tests instead of silently answering
    /// nothing.
    #[must_use]
    pub(crate) fn columns_only_with_max_cached_lines(max_cached_lines: usize) -> Self {
        // Start from the zero-capacity constructor: identical state to
        // `with_max_cached_lines` (both leave every map at capacity 0) except
        // that the filter it builds and we immediately discard is the ~1e4-bit
        // floor rather than `new()`'s ~1e6-bit (~128 KB) default.
        let mut index = Self::with_capacity_and_max(0, max_cached_lines);
        index.maintain_trigrams = false;
        // Nothing is ever inserted into or read from this filter;
        // `with_size(0)` floors it at a single 64-bit word.
        index.bloom = BloomFilter::with_size(0);
        index
    }

    /// Index a line at a specific line number.
    ///
    /// This overwrites any existing content at that line number.
    pub fn index_line(&mut self, line_num: usize, text: &str) {
        self.index_line_cow(line_num, Cow::Borrowed(text));
    }

    /// Shared borrowed/owned indexing primitive. Owned text moves directly
    /// into the retained line cache instead of being copied a second time.
    pub(crate) fn index_line_cow(&mut self, line_num: usize, text: Cow<'_, str>) {
        let text_ref = text.as_ref();

        // Remove old trigrams if this line was previously indexed.
        // Use remove() to move the old String out (avoids clone).
        //
        // Re-indexing a row whose text is IDENTICAL is the common case on the
        // interactive path: the GUI re-feeds the previously visible screen into
        // a reused index on every search, and absolute row numbers are stable,
        // so those rows arrive unchanged. For them the remove-then-reinsert
        // below is the identity on the posting lists — an expensive identity,
        // because `SparseBitmap::remove` and (for a row that is not past the
        // list's tail) `SparseBitmap::insert` are both decode-modify-re-encode
        // over the WHOLE list, twice per trigram occurrence. Detect that case
        // and skip only the posting-list work. The skip is state-identical, not
        // merely results-identical: removing then re-inserting the same row for
        // the same trigrams restores the same membership set, and
        // `rebuild_from_sorted`/`push_varint` produce a canonical minimal
        // encoding, so the `deltas`/`first`/`last`/`count` left in place are
        // byte-for-byte what the round trip would have rebuilt. (Every pruning
        // path — `evict_oldest_lines`, `retain_history_from` — drops from
        // `lines` and the postings under the same watermark, so a row still
        // present in `lines` is still present in each of its posting lists.)
        let mut replaced_old: Option<String> = None;
        let unchanged = match self.lines.remove(&line_num) {
            Some(old_text) if old_text == text_ref => {
                // Put the owned String straight back: no reallocation here, and
                // no `text.to_string()` below.
                self.lines.insert(line_num, old_text);
                true
            }
            Some(old_text) => {
                // A GENUINELY changed row (SA-3): defer the posting work to the
                // set-diff path below, which touches only the trigrams whose
                // membership actually changes instead of round-tripping every
                // posting list of BOTH texts (remove-all + insert-all was
                // ~2 × row-trigrams full-list decode/re-encode cycles for an
                // edit that typically shares almost all its trigrams — the
                // echoed prompt line, a partial last row).
                replaced_old = Some(old_text);
                false
            }
            None => false,
        };

        let bytes = text_ref.as_bytes();
        let line_u32 = line_as_u32(line_num);

        // A columns-only index (the budgeted engine) keeps no postings and no
        // bloom filter, so both passes below are pure waste for it — including
        // the Unicode arm's fold into the scratch buffer. Everything after this
        // block (the line/column-map cache, the counters, eviction) still runs
        // identically for it.
        if self.maintain_trigrams
            && let Some(old_text) = replaced_old
        {
            self.reindex_changed_row_postings(line_u32, &old_text, text_ref);
        } else if self.maintain_trigrams {
            // Add all trigrams from this line (original case).
            for window in bytes.windows(3) {
                let trigram: [u8; 3] = [window[0], window[1], window[2]];
                // The bloom insert is deliberately NOT skipped for an unchanged
                // row. `bloom.item_count()` drives `is_saturated()` and
                // therefore the `rebuild_bloom` cadence, which is observable
                // (`bloom_is_saturated`, the lifecycle differential oracles),
                // so it must see exactly the inserts it saw before. Hashing
                // three bytes is trivial next to the posting-list round trip
                // skipped below.
                self.bloom.insert_bytes(&trigram);
                if !unchanged {
                    self.trigrams.entry(trigram).or_default().insert(line_u32);
                }
            }

            // Also insert Unicode-lowercased trigrams for case-insensitive
            // bloom filter and posting-list acceleration (#7273, #7398, #7470).
            // Uses full Unicode lowercasing so non-ASCII characters
            // (e.g., Ä→ä, É→é) are indexed correctly.
            //
            // Skip this pass entirely when lowercasing cannot change any byte:
            // for pure-ASCII text with no uppercase letter, `to_lowercase()` is
            // the identity, so the lowered trigrams equal the original-case ones
            // already inserted above. The predicate lives in `lower_need` and is
            // shared with `reindex_changed_row_postings`/`rebuild_bloom` so insert/remove/
            // rebuild stay symmetric by construction.
            //
            // Neither non-identity arm allocates per line any more: pure-ASCII
            // text lowers per byte in place off the ORIGINAL window (lowering an
            // ASCII string never changes its byte length, so the windows
            // correspond one-to-one), and the Unicode arm folds into a reused
            // scratch buffer. The old shape built a fresh capacity-less `String`
            // per line — ~5 reallocations for an 80-column line — only to walk
            // it once and drop it, on the per-line primitive every index build
            // runs.
            match lower_need(text_ref) {
                LowerNeed::None => {}
                LowerNeed::Ascii => {
                    for window in bytes.windows(3) {
                        let trigram: [u8; 3] = [
                            window[0].to_ascii_lowercase(),
                            window[1].to_ascii_lowercase(),
                            window[2].to_ascii_lowercase(),
                        ];
                        // Bloom always, postings only when the row's text
                        // changed (see the original-case pass above).
                        self.bloom.insert_bytes(&trigram);
                        if !unchanged {
                            self.trigrams.entry(trigram).or_default().insert(line_u32);
                        }
                    }
                }
                LowerNeed::Unicode => {
                    lower_fold_into(text_ref, &mut self.lower_scratch);
                    for window in self.lower_scratch.as_bytes().windows(3) {
                        let trigram: [u8; 3] = [window[0], window[1], window[2]];
                        // Bloom always, postings only when the row's text
                        // changed (see the original-case pass above).
                        self.bloom.insert_bytes(&trigram);
                        if !unchanged {
                            self.trigrams.entry(trigram).or_default().insert(line_u32);
                        }
                    }
                }
            }
        }

        // Cache the line content and precomputed column map (#7373). An
        // unchanged row already has both: its `String` was put straight back
        // above, and a `ColumnMap` is a pure function of the text, so the
        // cached one is already the map this call would rebuild.
        if !unchanged {
            let column_map = ColumnMap::new(text_ref);
            self.lines.insert(line_num, text.into_owned());
            self.column_maps.insert(line_num, column_map);
        }
        self.first_cached_line = self.first_cached_line.min(line_num);
        self.line_count = self.line_count.max(line_num.saturating_add(1));
        self.next_line = self.next_line.max(line_num.saturating_add(1));

        // Evict oldest cached lines if over capacity
        if self.lines.len() > self.max_cached_lines {
            self.evict_oldest_lines();
        }

        // Rebuild bloom filter if saturated (#7243). When the estimated FPR
        // exceeds 50%, the bloom filter returns true for most queries, making
        // it useless as a negative filter. Rebuild from remaining cached lines
        // to restore its effectiveness. A columns-only index has no filter to
        // saturate (nothing is ever inserted, so `is_saturated()` is already
        // constant-false) — the guard makes that explicit rather than paying an
        // `exp()` + `powi(7)` per row to rediscover it.
        if self.maintain_trigrams && self.bloom.is_saturated() {
            self.rebuild_bloom();
        }
    }

    /// Index a line at the next available line number.
    ///
    /// Returns the assigned line number.
    pub fn push_line(&mut self, text: &str) -> usize {
        let line_num = self.next_line;
        self.index_line(line_num, text);
        line_num
    }

    /// Collect the full trigram set one text contributes to the POSTING maps:
    /// the original-case windows plus the lowered pass exactly when
    /// [`lower_need`] says the insert path ran it (the shared classifier that
    /// keeps insert/remove/rebuild symmetric by construction — #7398, #7470).
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

    /// Posting + bloom maintenance for a row whose text GENUINELY changed
    /// (SA-3): touch only the trigrams whose membership actually changes.
    ///
    /// The predecessor (`remove_trigrams(old)` + insert-every-new-trigram)
    /// paid a full posting-list decode → splice → re-encode round trip TWICE
    /// per trigram occurrence of BOTH texts — O(list length) each, ~160 round
    /// trips for one edited 80-column row, even though a typical in-place edit
    /// (the echoed prompt line, a partial last row) shares almost every
    /// trigram between old and new text. For a SHARED trigram the
    /// remove-then-reinsert was the identity on the list — an EXPENSIVE
    /// identity. This does set-difference instead:
    ///
    /// - old ∩ new: posting list untouched. State-identical, not merely
    ///   results-identical: every mutator writes the canonical minimal
    ///   delta+varint encoding, so remove(row)-then-insert(row) would have
    ///   rebuilt byte-for-byte what was already there (the same canonicality
    ///   argument the unchanged-row skip in [`index_line`] rests on — even a
    ///   shared trigram whose list momentarily emptied and was pruned would be
    ///   recreated with the identical single-value encoding).
    /// - old ∖ new: removed (and pruned when emptied — #2111), exactly as
    ///   `remove_trigrams` did.
    /// - new ∖ old: inserted, exactly as the insert pass did. The `new_seen`
    ///   gate also spares repeated windows the old path's per-duplicate
    ///   decode + binary-search no-op on non-tail rows.
    ///
    /// The BLOOM inserts are NOT diffed: `bloom.item_count()` drives the
    /// `is_saturated()` rebuild cadence, which is observable
    /// (`bloom_is_saturated`, the lifecycle differential oracles), so every
    /// new-text window is inserted exactly as before. Cost scales with the
    /// EDIT's trigram delta, not the row's trigram count; the O(list-length)
    /// cost per genuinely-changed trigram remains (that is the posting
    /// CONTAINER's shape — blocked postings are the follow-up category fix).
    ///
    /// [`index_line`]: Self::index_line
    /// [`lower_need`]: crate::grapheme::lower_need
    fn reindex_changed_row_postings(&mut self, line_u32: u32, old_text: &str, text: &str) {
        let old_set = self.posting_trigram_set(old_text);
        let bytes = text.as_bytes();
        let mut new_seen: FxHashSet<[u8; 3]> =
            FxHashSet::with_capacity_and_hasher(bytes.len().saturating_mul(2), FxBuildHasher);

        // Original-case pass: bloom ALWAYS (cadence parity — see above),
        // postings only for trigrams the old text did not already contribute.
        for window in bytes.windows(3) {
            let trigram: [u8; 3] = [window[0], window[1], window[2]];
            self.bloom.insert_bytes(&trigram);
            if new_seen.insert(trigram) && !old_set.contains(&trigram) {
                self.trigrams.entry(trigram).or_default().insert(line_u32);
            }
        }
        // Lowered pass, gated by the SHARED classifier (see `index_line`).
        match lower_need(text) {
            LowerNeed::None => {}
            LowerNeed::Ascii => {
                for window in bytes.windows(3) {
                    let trigram: [u8; 3] = [
                        window[0].to_ascii_lowercase(),
                        window[1].to_ascii_lowercase(),
                        window[2].to_ascii_lowercase(),
                    ];
                    self.bloom.insert_bytes(&trigram);
                    if new_seen.insert(trigram) && !old_set.contains(&trigram) {
                        self.trigrams.entry(trigram).or_default().insert(line_u32);
                    }
                }
            }
            LowerNeed::Unicode => {
                lower_fold_into(text, &mut self.lower_scratch);
                // Disjoint-field borrows (scratch read, bloom/trigrams
                // written) — the exact pattern `index_line`'s Unicode arm
                // already uses.
                for window in self.lower_scratch.as_bytes().windows(3) {
                    let trigram: [u8; 3] = [window[0], window[1], window[2]];
                    self.bloom.insert_bytes(&trigram);
                    if new_seen.insert(trigram) && !old_set.contains(&trigram) {
                        self.trigrams.entry(trigram).or_default().insert(line_u32);
                    }
                }
            }
        }
        // Removals: trigrams only the OLD text contributed (prune emptied
        // lists exactly as `remove_trigrams` did — #2111).
        for trigram in old_set {
            if new_seen.contains(&trigram) {
                continue;
            }
            if let Some(bitmap) = self.trigrams.get_mut(&trigram) {
                bitmap.remove(line_u32);
                if bitmap.is_empty() {
                    self.trigrams.remove(&trigram);
                }
            }
        }
    }

    /// Evict the oldest 25% of cached lines when capacity is exceeded.
    ///
    /// Collects and sorts cached line numbers to evict in O(n log n) instead
    /// of scanning linearly through gaps. With sparse line numbers (e.g.,
    /// visible content at line 50000 after scrollback lines 0-99), the
    /// previous approach was O(gap_size) which caused visible stalls. (#7246)
    fn evict_oldest_lines(&mut self) {
        // Saturating: only reachable when `lines.len() > max_cached_lines`,
        // so the cap is far below usize::MAX / 3 on every real path and the
        // saturation is dead code carrying the no-overflow proof for the
        // Trust L0 gate. The 3/4 low-water ratio is named once (shared with
        // `max_cached_for_retained`, the inverse callers size a retained floor by).
        let target =
            self.max_cached_lines.saturating_mul(EVICTION_RETAIN_NUM) / EVICTION_RETAIN_DEN;
        let to_evict = self.lines.len().saturating_sub(target);
        if to_evict == 0 {
            return;
        }

        // Collect and sort line numbers to find the oldest entries.
        let mut line_nums: Vec<usize> = self.lines.keys().copied().collect();
        line_nums.sort_unstable();

        // Remove the oldest `to_evict` entries. Explicit comparison instead
        // of `Ord::min` so the gate sees the dominating bound for the slice
        // below (same value always).
        let evict_count = if to_evict < line_nums.len() {
            to_evict
        } else {
            line_nums.len()
        };
        // `take` instead of slicing `line_nums[..evict_count]`: take() is
        // panic-free, so the L0 gate has no slice-bounds obligation to carry
        // across the (opaque to it) Vec length. Identical iteration: take(n)
        // stops at min(n, len) and evict_count <= len by the guard above.
        for &line in line_nums.iter().take(evict_count) {
            self.lines.remove(&line);
            self.column_maps.remove(&line);
        }

        // Advance the retained-line watermark to the smallest remaining line.
        // Matches below this line are gone and can no longer be returned, so
        // callers must treat results that span this range as incomplete.
        self.lowest_retained_line = line_nums
            .get(evict_count)
            .copied()
            .unwrap_or(self.next_line);
        self.first_cached_line = self.lowest_retained_line;
        self.eviction_occurred = true;

        // Trim every posting list to the new watermark in ONE front-drain, then
        // prune emptied trigrams. Equivalent to removing each evicted line's
        // trigrams one-by-one — every row below the watermark is evicted, so a
        // posting entry is dropped iff its row is — but O(total_postings)
        // instead of the O(evicted·len) of one-at-a-time front removals (the
        // sortedvec container's front remove is a tail shift). See
        // SparseBitmap::drop_below and the eviction-identity oracle.
        //
        // Skipped wholesale by a columns-only index: it has no postings to trim.
        // The watermark bookkeeping above is NOT skipped — it is what keeps the
        // budgeted engine's eviction schedule identical to the batch one.
        if self.maintain_trigrams {
            let watermark = line_as_u32(self.lowest_retained_line);
            self.trigrams.retain(|_, bitmap| {
                bitmap.drop_below(watermark);
                !bitmap.is_empty()
            });
        }

        // Warn once: results are now potentially incomplete for the lifetime of
        // this index. Repeated eviction passes do not re-warn (avoids log spam).
        if !self.first_eviction_warned {
            self.first_eviction_warned = true;
            aterm_log::warn!(
                "search index exceeded {} cached lines; evicting oldest entries — \
                 search results may be incomplete below line {} (oldest indexed line is now {})",
                self.max_cached_lines,
                self.lowest_retained_line,
                self.lowest_retained_line,
            );
        }

        // Rebuild bloom filter from remaining lines (#7270). A columns-only
        // index has no filter to rebuild, and this sweep is O(all cached lines ×
        // line length) — the one unbounded chunk of work that could land inside
        // a single budgeted turn.
        if self.maintain_trigrams {
            self.rebuild_bloom();
        }
    }

    /// Rebuild the bloom filter sized for the current trigram load.
    ///
    /// The bloom filter stores trigrams, not lines. Rebuilding it with only
    /// `lines.len()` capacity badly underestimates the true insert volume for
    /// wide scrollback lines and causes immediate re-saturation, which in turn
    /// can trigger a rebuild on nearly every indexed line. Use the current
    /// trigram insert count as the rebuild target so the resized filter tracks
    /// actual load rather than line cardinality (#7243).
    fn rebuild_bloom(&mut self) {
        let capacity = self.bloom.item_count().max(self.lines.len()).max(1000);
        // Branch-duplicated construction: BloomFilter::with_capacity
        // saturates its size cap at MAX_EFFECTIVE_CAPACITY anyway, so both
        // arms construct the IDENTICAL filter for any input in the first arm.
        // The L0 gate's allocation recognizer needs the comparison to
        // directly dominate the allocating call (a phi-merged clamp is not
        // recognized), hence the duplication.
        self.bloom = if capacity > BloomFilter::MAX_EFFECTIVE_CAPACITY {
            BloomFilter::with_capacity(BloomFilter::MAX_EFFECTIVE_CAPACITY)
        } else {
            BloomFilter::with_capacity(capacity)
        };
        // Trigram extraction via `get` + let-else instead of `window[k]`
        // indexing: windows(3) only ever yields 3-byte slices, so the
        // `continue` is dead on every input — the panic-free spelling removes
        // the slice-bounds obligations the L0 gate's transport consistently
        // fails to carry for this function (index_line keeps the indexed
        // shape, which proves there). Identical insert set.
        for text in self.lines.values() {
            // Insert original-case trigrams.
            for window in text.as_bytes().windows(3) {
                let (Some(&a), Some(&b), Some(&c)) = (window.first(), window.get(1), window.get(2))
                else {
                    continue;
                };
                self.bloom.insert_bytes(&[a, b, c]);
            }
            // Insert Unicode-lowercased trigrams for case-insensitive
            // bloom filter acceleration (#7273, #7470).
            //
            // Mirrors `index_line` through the shared `lower_need` classifier:
            // skip the lowered pass when lowercasing is the identity so the
            // rebuilt filter sees the same insert set the incremental path
            // produced. Neither arm allocates — this loop runs over EVERY
            // cached line (up to the 100k cap) on each rebuild, so a per-line
            // throwaway `String` here was the same waste multiplied by the
            // cache size.
            match lower_need(text) {
                LowerNeed::None => {}
                LowerNeed::Ascii => {
                    for window in text.as_bytes().windows(3) {
                        let (Some(&a), Some(&b), Some(&c)) =
                            (window.first(), window.get(1), window.get(2))
                        else {
                            continue;
                        };
                        self.bloom.insert_bytes(&[
                            a.to_ascii_lowercase(),
                            b.to_ascii_lowercase(),
                            c.to_ascii_lowercase(),
                        ]);
                    }
                }
                LowerNeed::Unicode => {
                    lower_fold_into(text, &mut self.lower_scratch);
                    for window in self.lower_scratch.as_bytes().windows(3) {
                        let (Some(&a), Some(&b), Some(&c)) =
                            (window.first(), window.get(1), window.get(2))
                        else {
                            continue;
                        };
                        self.bloom.insert_bytes(&[a, b, c]);
                    }
                }
            }
        }
    }

    /// Check if a query might have matches (bloom filter check).
    ///
    /// Returns `false` if definitely no matches exist.
    /// Returns `true` if matches are possible (verify with actual search).
    #[must_use]
    pub fn might_contain(&self, query: &str) -> bool {
        let bytes = query.as_bytes();

        // For short queries, we can't use the bloom filter effectively
        if bytes.len() < 3 {
            return true;
        }

        // Check if all query trigrams might exist
        for window in bytes.windows(3) {
            if !self.bloom.might_contain_bytes(window) {
                return false;
            }
        }
        true
    }

    /// Intersect the posting lists for every trigram in `query`.
    ///
    /// Shared core of `search`, `search_from_line`, and `search_before_line`:
    /// each caller handles its own empty-query / short-query / bloom-filter
    /// guards and candidate source, then defers to this for the intersection.
    ///
    /// Returns `None` when no matches are possible (a trigram is missing or the
    /// query yields no posting lists), and `Some(bitmap)` with the intersection
    /// otherwise. Caller is responsible for ensuring `query` has 3+ bytes.
    fn intersect_trigrams(&self, query: &str) -> Option<SparseBitmap> {
        let mut posting_lists = self.posting_lists(query)?;
        if posting_lists.is_empty() {
            return None;
        }

        // Sort by size so intersect_posting_lists starts from the smallest.
        posting_lists.sort_unstable_by_key(|b| b.len());
        Some(intersect_posting_lists(&posting_lists))
    }

    /// Borrow the posting list for each trigram in `query`.
    fn posting_lists(&self, query: &str) -> Option<Vec<&SparseBitmap>> {
        let mut posting_lists = Vec::new();
        for window in query.as_bytes().windows(3) {
            let trigram: [u8; 3] = [window[0], window[1], window[2]];
            posting_lists.push(self.trigrams.get(&trigram)?);
        }
        // Repeated trigrams point at the same posting bitmap. Deduplicate by
        // identity so a long/repetitive query does not repeat the same BTree
        // membership test (or owned intersection) once per query position.
        posting_lists.sort_unstable_by_key(|bitmap| std::ptr::from_ref(*bitmap));
        posting_lists.dedup_by(|left, right| std::ptr::eq(*left, *right));
        #[cfg(test)]
        LAST_UNIQUE_POSTING_LISTS.with(|count| count.set(posting_lists.len()));
        Some(posting_lists)
    }

    /// Build a lazy ascending candidate source for a literal query.
    fn literal_candidates_forward(&self, lower_query: &str, from_line: usize) -> CandidateSource {
        if lower_query.is_empty() || self.lines.is_empty() {
            return CandidateSource::Empty;
        }

        let first = from_line.max(self.first_cached_line);
        if lower_query.len() < 3 {
            return CandidateSource::Range(line_as_u32(first)..line_as_u32(self.line_count));
        }
        if !self.might_contain(lower_query) {
            return CandidateSource::Empty;
        }
        let Some(postings) = self.posting_lists(lower_query) else {
            return CandidateSource::Empty;
        };
        CandidateSource::from_postings_forward(postings, line_as_u32(first))
    }

    /// Build a lazy descending candidate source for a folded literal query.
    fn literal_candidates_backward(
        &self,
        lower_query: &str,
        before_line: usize,
    ) -> CandidateSource {
        if lower_query.is_empty() || self.lines.is_empty() {
            return CandidateSource::Empty;
        }

        let upper = before_line.min(self.line_count);
        if lower_query.len() < 3 {
            let lower = self.first_cached_line.min(upper);
            return CandidateSource::RangeRev((line_as_u32(lower)..line_as_u32(upper)).rev());
        }
        if !self.might_contain(lower_query) {
            return CandidateSource::Empty;
        }
        let Some(postings) = self.posting_lists(lower_query) else {
            return CandidateSource::Empty;
        };
        CandidateSource::from_postings_backward(postings, line_as_u32(upper))
    }

    /// Search for a query string.
    ///
    /// Returns line numbers that might contain the query.
    /// Results may include false positives but never false negatives.
    pub fn search(&self, query: &str) -> impl Iterator<Item = u32> + '_ + use<'_> {
        let bytes = query.as_bytes();

        if bytes.len() < 3 {
            // Can't use trigram index for short queries
            // Fall back to retained lines (caller must verify). Starting at the
            // first cached absolute row avoids walking a potentially huge empty
            // numeric prefix after long-running terminal history eviction.
            let first = self.first_cached_line.min(self.line_count);
            return SearchResult::All(line_as_u32(first)..line_as_u32(self.line_count));
        }

        // Quick bloom filter check
        if !self.might_contain(query) {
            return SearchResult::None;
        }

        let Some(result) = self.intersect_trigrams(query) else {
            return SearchResult::None;
        };
        SearchResult::Bitmap(Box::new(result.into_iter()))
    }

    /// Search with match verification and position extraction.
    ///
    /// Returns actual matches with column positions.
    /// This verifies candidates against cached line content.
    pub fn search_with_positions(&self, query: &str) -> Vec<SearchMatch> {
        // Empty query returns no matches (prevents infinite loop in find)
        if query.is_empty() {
            return Vec::new();
        }
        self.search_from_line(query, self.first_cached_line.min(self.line_count))
            .take(MAX_SEARCH_MATCHES)
            .collect()
    }

    /// Search and return matches in the specified direction.
    ///
    /// Returns an iterator over matches sorted by line number.
    pub fn search_ordered(&self, query: &str, direction: SearchDirection) -> Vec<SearchMatch> {
        let mut matches = self.search_with_positions(query);

        match direction {
            SearchDirection::Forward => {
                matches.sort_by_key(|m| (m.line, m.start_col));
            }
            SearchDirection::Backward => {
                matches
                    .sort_by_key(|m| (std::cmp::Reverse(m.line), std::cmp::Reverse(m.start_col)));
            }
        }

        matches
    }

    /// Get the number of indexed lines.
    #[must_use]
    pub fn len(&self) -> usize {
        self.line_count
    }

    /// Returns true if the index is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.line_count == 0
    }

    /// Clear the index.
    ///
    /// Resets the eviction watermark and incomplete-results signal: a cleared
    /// index has not evicted anything, so [`results_may_be_incomplete`] is
    /// false again until the cache cap is next exceeded.
    ///
    /// [`results_may_be_incomplete`]: Self::results_may_be_incomplete
    pub fn clear(&mut self) {
        self.bloom.clear();
        self.trigrams.clear();
        self.lines.clear();
        self.column_maps.clear();
        self.line_count = 0;
        self.first_cached_line = usize::MAX;
        self.next_line = 0;
        self.lowest_retained_line = 0;
        self.eviction_occurred = false;
        self.first_eviction_warned = false;
    }

    /// Release the index's backing allocations, not just its logical contents.
    ///
    /// [`clear`](Self::clear) empties every container but a `HashMap`/`Vec`
    /// RETAINS the capacity it grew to, so a cleared index still holds the peak
    /// heap of its busiest moment — a logical clear frees nothing observable to
    /// the OS. `release` instead REPLACES each container with a fresh empty one,
    /// dropping the grown allocation back to the allocator, and resets the same
    /// watermarks `clear` does. This is the primitive an idle-eviction policy
    /// calls to actually reclaim a dormant document's footprint; a later
    /// `index_line` regrows the maps from empty. The reset is byte-for-byte the
    /// same OBSERVABLE state as `clear` (a subsequent search behaves
    /// identically) — only the retained capacity differs.
    pub fn release(&mut self) {
        // Assign fresh containers (not `.clear()`) so the old backing buffers
        // are dropped, not merely emptied-in-place. Bloom drops to its floor
        // capacity, matching a freshly constructed index.
        self.bloom = BloomFilter::with_capacity(1000);
        self.trigrams = FxHashMap::default();
        self.lines = FxHashMap::default();
        self.column_maps = FxHashMap::default();
        self.lower_scratch = String::new();
        self.line_count = 0;
        self.first_cached_line = usize::MAX;
        self.next_line = 0;
        self.lowest_retained_line = 0;
        self.eviction_occurred = false;
        self.first_eviction_warned = false;
    }

    /// Set the maximum number of cached lines before eviction.
    ///
    /// A value of 0 is clamped to 1 so the index always retains at least the
    /// most recent line. Lowering the cap below the current cache size does not
    /// retroactively evict — eviction happens on the next [`index_line`] that
    /// exceeds the new cap.
    ///
    /// [`index_line`]: Self::index_line
    pub fn set_max_cached_lines(&mut self, max: usize) {
        self.max_cached_lines = max.max(1);
    }

    /// Record that a bulk-built index intentionally omitted an older history
    /// prefix. Result completeness and the retained watermark remain honest
    /// without copying or indexing rows outside the configured suffix.
    pub(crate) fn mark_history_prefix_evicted(&mut self, lowest_retained_line: usize) {
        if lowest_retained_line == 0 {
            return;
        }
        self.lowest_retained_line = self.lowest_retained_line.max(lowest_retained_line);
        self.first_cached_line = self.first_cached_line.max(lowest_retained_line);
        self.eviction_occurred = true;
    }

    /// Drop cached absolute rows below a newly-retained history boundary.
    /// Small ordinary scroll deltas walk only the removed numeric prefix; a
    /// sparse/large jump switches to one bounded cache scan.
    pub(crate) fn retain_history_from(&mut self, first_retained_line: usize) {
        let old_first = self.first_cached_line.min(first_retained_line);
        if first_retained_line <= old_first {
            return;
        }
        let distance = first_retained_line.saturating_sub(old_first);
        let stale: Vec<usize> = if distance <= self.lines.len().saturating_mul(2) {
            (old_first..first_retained_line).collect()
        } else {
            self.lines
                .keys()
                .copied()
                .filter(|line| *line < first_retained_line)
                .collect()
        };
        for line in stale {
            self.lines.remove(&line);
            self.column_maps.remove(&line);
        }
        // Batch-trim postings below the retained boundary in one front-drain
        // (see evict_oldest_lines): every removed row is < first_retained_line,
        // so trimming each posting list to the watermark drops exactly their
        // entries without an O(rows·len) per-row front-remove.
        let watermark = line_as_u32(first_retained_line);
        self.trigrams.retain(|_, bitmap| {
            bitmap.drop_below(watermark);
            !bitmap.is_empty()
        });
        self.first_cached_line = first_retained_line.min(self.line_count);
        self.lowest_retained_line = self.lowest_retained_line.max(first_retained_line);
        self.eviction_occurred = true;
    }

    /// Drop cached absolute rows below `first_retained_line` WITHOUT recording
    /// an eviction — the complete-retention twin of [`retain_history_from`].
    ///
    /// [`retain_history_from`] models "rows the INDEX can no longer serve": an
    /// honesty event (results become incomplete, the retained watermark
    /// advances). This models "rows the TERMINAL no longer retains at all":
    /// after grid retention advances (a full ring evicting one line per
    /// append, a scrollback-limit or memory-budget shrink), the dropped rows
    /// are not un-searchable content — they are nonexistent content, and a
    /// from-scratch rebuild over the surviving rows would start a FRESH index
    /// reporting COMPLETE results. The terminal's incremental refresh
    /// (`Terminal::indexed_search`) uses this so its observable state
    /// (matches, [`results_may_be_incomplete`], [`lowest_retained_line`]) is
    /// byte-identical to that from-scratch rebuild — the refresh's
    /// behavior-identity contract — instead of diverging into a sticky
    /// `incomplete` the rebuild path never reported.
    ///
    /// Callers must only use this when the dropped rows are really gone from
    /// the source buffer; for index-side capacity trimming keep
    /// [`retain_history_from`], which reports honestly.
    ///
    /// [`retain_history_from`]: Self::retain_history_from
    /// [`results_may_be_incomplete`]: Self::results_may_be_incomplete
    /// [`lowest_retained_line`]: Self::lowest_retained_line
    pub fn drop_history_below(&mut self, first_retained_line: usize) {
        let old_first = self.first_cached_line.min(first_retained_line);
        if first_retained_line <= old_first {
            return;
        }
        // Same two-strategy stale-key collection as `retain_history_from`:
        // small ordinary advances walk the removed numeric prefix; a sparse or
        // large jump switches to one bounded cache scan.
        let distance = first_retained_line.saturating_sub(old_first);
        let stale: Vec<usize> = if distance <= self.lines.len().saturating_mul(2) {
            (old_first..first_retained_line).collect()
        } else {
            self.lines
                .keys()
                .copied()
                .filter(|line| *line < first_retained_line)
                .collect()
        };
        for line in stale {
            self.lines.remove(&line);
            self.column_maps.remove(&line);
        }
        // One batched posting front-drain, exactly as `retain_history_from`
        // (see there); only the honesty bookkeeping differs.
        let watermark = line_as_u32(first_retained_line);
        self.trigrams.retain(|_, bitmap| {
            bitmap.drop_below(watermark);
            !bitmap.is_empty()
        });
        self.first_cached_line = first_retained_line.min(self.line_count);
        // Deliberately NOT touched: `lowest_retained_line` /
        // `eviction_occurred`. A from-scratch rebuild over the surviving rows
        // reports complete results with a zero watermark, and this drop must
        // be observationally indistinguishable from that rebuild. The bloom
        // filter keeps the dropped rows' stale bits (it is a NEGATIVE filter:
        // stale bits cost false-positive candidate visits, never results);
        // saturation-triggered rebuilds resize it from live lines exactly as
        // on the incremental append path.
    }

    /// Current maximum cached-lines cap.
    #[must_use]
    pub fn max_cached_lines(&self) -> usize {
        self.max_cached_lines
    }

    /// The oldest line number still retained in the index.
    ///
    /// Returns 0 until eviction occurs. After eviction this is the lowest line
    /// that can still produce a match; any match in scrollback below this line
    /// has been dropped from the index. Callers (e.g. `cmd_search`) can report
    /// the searchable range `[lowest_retained_line(), len())` to the AI.
    #[must_use]
    pub fn lowest_retained_line(&self) -> usize {
        self.lowest_retained_line
    }

    /// Whether search results may be incomplete due to eviction.
    ///
    /// `false` means every indexed line is still cached and search is
    /// exhaustive over the indexed range. `true` means the cache cap has been
    /// exceeded at least once and the oldest lines were dropped, so matches
    /// below [`lowest_retained_line`](Self::lowest_retained_line) are silently
    /// absent. The future `cmd_search` should pass this through so the AI is
    /// told results are truncated rather than treating them as exhaustive.
    #[must_use]
    pub fn results_may_be_incomplete(&self) -> bool {
        self.eviction_occurred
    }

    /// Get cached line content by line number.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn get_line(&self, line_num: usize) -> Option<&str> {
        self.lines.get(&line_num).map(|s| s.as_str())
    }

    /// Search for matches starting from a given line (O(log n) for first match).
    ///
    /// Returns an iterator over matches in forward order (oldest to newest),
    /// starting from `from_line`. This is efficient for `find_next` operations
    /// as it uses range queries on the trigram index to skip earlier lines.
    ///
    /// # Arguments
    /// * `query` - The search query (must be 3+ chars for trigram indexing)
    /// * `from_line` - Start searching from this line number (inclusive)
    pub(crate) fn search_from_line<'a>(
        &'a self,
        query: &'a str,
        from_line: usize,
    ) -> SearchMatchIterator<'a> {
        // A columns-only index has no postings to consult, so querying it would
        // silently answer "no matches". Trip loudly in tests instead.
        debug_assert!(
            self.maintain_trigrams,
            "search_from_line on a columns-only index returns no matches"
        );
        let bytes = query.as_bytes();

        let empty = || CandidateSource::Empty;

        // Empty query returns no matches
        if query.is_empty() {
            return SearchMatchIterator::new(self, query, empty());
        }

        if bytes.len() < 3 {
            // Can't use trigram index for short queries — use lazy range
            // instead of collecting all line numbers into a Vec (avoids O(n) alloc)
            let first = from_line.max(self.first_cached_line.min(self.line_count));
            let source = CandidateSource::Range(line_as_u32(first)..line_as_u32(self.line_count));
            return SearchMatchIterator::new(self, query, source);
        }

        // Quick bloom filter check
        if !self.might_contain(query) {
            return SearchMatchIterator::new(self, query, empty());
        }

        let Some(postings) = self.posting_lists(query) else {
            return SearchMatchIterator::new(self, query, empty());
        };

        // Borrow the smallest posting range and membership-check the remaining
        // lists lazily. No full BTree clone/intersection is built before the
        // first match, even for a one-trigram query present on every line.
        let source = CandidateSource::from_postings_forward(postings, line_as_u32(from_line));
        SearchMatchIterator::new(self, query, source)
    }

    /// Search for matches up to a given line for backward iteration.
    ///
    /// Returns an iterator over matches in reverse order (newest to oldest),
    /// only considering lines before `before_line`. This is efficient for
    /// `find_prev` operations.
    ///
    /// # Arguments
    /// * `query` - The search query (must be 3+ chars for trigram indexing)
    /// * `before_line` - Only search lines before this line number (exclusive)
    pub(crate) fn search_before_line<'a>(
        &'a self,
        query: &'a str,
        before_line: usize,
    ) -> SearchMatchReverseIterator<'a> {
        // See `search_from_line`: a columns-only index cannot answer a query.
        debug_assert!(
            self.maintain_trigrams,
            "search_before_line on a columns-only index returns no matches"
        );
        let bytes = query.as_bytes();

        let empty = || CandidateSource::Empty;

        // Empty query returns no matches
        if query.is_empty() {
            return SearchMatchReverseIterator::new(self, query, empty());
        }

        if bytes.len() < 3 {
            // Can't use trigram index for short queries — use lazy reversed range
            // instead of collecting all line numbers into a Vec (avoids O(n) alloc)
            let upper = before_line.min(self.line_count);
            let lower = self.first_cached_line.min(upper);
            let source = CandidateSource::RangeRev((line_as_u32(lower)..line_as_u32(upper)).rev());
            return SearchMatchReverseIterator::new(self, query, source);
        }

        // Quick bloom filter check
        if !self.might_contain(query) {
            return SearchMatchReverseIterator::new(self, query, empty());
        }

        let Some(postings) = self.posting_lists(query) else {
            return SearchMatchReverseIterator::new(self, query, empty());
        };

        let source = CandidateSource::from_postings_backward(postings, line_as_u32(before_line));
        SearchMatchReverseIterator::new(self, query, source)
    }
}

/// Maximum pattern length for regex compilation (bytes).
///
/// Matches the streaming engine's default `max_pattern_len` (1024). Patterns
/// beyond this limit are rejected before compilation to bound CPU cost.
#[allow(dead_code)]
const MAX_REGEX_PATTERN_LEN: usize = 1024;

/// Maximum compiled regex size (bytes) passed to `RegexBuilder::size_limit`.
///
/// Caps the NFA at 128 KiB — 2,048 instructions at `aterm-regex`'s
/// 64-byte-per-instruction charge — well below the engine's 10 MiB default.
/// Mirrors `aterm-observe`'s constant of the same name, which carries the full
/// derivation.
///
/// It bounds **both** compile time and scan time. Deeply nested alternations
/// and large repetition counts are the ReDoS-via-compilation shape, and a Pike
/// VM's scan is linear in the haystack with the program size as its constant —
/// so the same ceiling is what stops a thirteen-byte pattern such as
/// `(?:x?){2000}z` (4,002 instructions, 37 ms per 4,096-column row under the
/// old 1 MiB ceiling) from being recompiled and re-scanned on every keystroke.
/// 128 KiB is the smallest value that still admits a 1,024-byte literal
/// pattern, which [`MAX_REGEX_PATTERN_LEN`] permits.
#[cfg(feature = "regex")]
pub(crate) const REGEX_SIZE_LIMIT: usize = 128 * 1024; // 128 KiB

/// Maximum DFA size (bytes) passed to `RegexBuilder::dfa_size_limit`.
///
/// Retained for source compatibility and still passed: `aterm-regex` is a pure
/// Pike VM, so there is no lazy DFA for it to bound today, and the builder
/// documents the setting as inert rather than repurposing it silently.
/// Per-query memory is already bounded by [`REGEX_SIZE_LIMIT`] — the VM's
/// thread set is capped by the program size. Mirrors `aterm-observe`.
#[cfg(feature = "regex")]
pub(crate) const REGEX_DFA_SIZE_LIMIT: usize = 1 << 20; // 1 MiB

/// Scan budget (work units) for one search, passed to
/// `aterm_regex::RegexBuilder::step_limit`. About 4.8 ms of matching per line
/// on the m21 box in release.
///
/// [`REGEX_SIZE_LIMIT`] bounds the compiled program; this bounds what running
/// that program over a line costs, and only the two together bound a query. A
/// Pike VM's scan is linear in the haystack with the program size as its
/// constant, so the cost of a pattern is the *product* — and both factors come
/// from the search box. Measured: the heaviest program the 128 KiB ceiling
/// still admits needs ~16.7M units (19 ms) to cross one 4,096-column line,
/// which over a 20,000-line scrollback is minutes per keystroke; this ceiling
/// refuses it after ~4.2M.
///
/// Nothing real comes near it — the most expensive pattern aterm itself ships
/// (the IPv6 selection rule) needs ~3,100 units for one search over a full-width
/// line, three orders of magnitude below. Exhaustion is reported as
/// [`SearchOptionsError::InvalidRegex`] rather than as a short result list:
/// a truncated result set that looks exhaustive is a wrong answer, and a
/// refusal naming the cause is not. Mirrors `aterm-observe`.
#[cfg(feature = "regex")]
pub(crate) const REGEX_STEP_LIMIT: u64 = 1 << 22;

/// Result of one [`SearchIndex::search_literal_narrowed`] step: the forward
/// batch results plus the occurrence-line frame that seeds the next
/// incremental-search narrowing step (SA-1 isearch).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NarrowedSearch {
    /// Batch results, ascending — identical to the forward batch path over
    /// the same candidate universe (see the method docs).
    pub results: SearchResults,
    /// Ascending, deduplicated retained lines whose text CONTAINS the query
    /// at the fold level — occurrences, a strict superset of reported-match
    /// lines (zero-display-width occurrences count; reported matches do not
    /// include them). `None` when the walk hit the batch cap: a truncated
    /// frame would break the narrowing subset property, so the caller must
    /// reseed from the engine.
    pub occurrence_lines: Option<Vec<u32>>,
}

/// Error returned when search options are invalid (e.g., regex feature not enabled).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, aterm_error::Error)]
pub enum SearchOptionsError {
    /// Regex was requested but the feature is not compiled in.
    #[error("regex feature not enabled")]
    RegexNotEnabled,
    /// The regex pattern is invalid.
    #[error("invalid regex: {0}")]
    InvalidRegex(String),
    /// The regex pattern exceeds the maximum allowed length.
    #[error("pattern exceeds maximum length ({MAX_REGEX_PATTERN_LEN} bytes)")]
    PatternTooLong,
}

impl SearchIndex {
    /// Search with match verification, supporting case-insensitive and regex modes.
    ///
    /// When `case_sensitive` is true and `is_regex` is false, this delegates to
    /// the trigram-accelerated `search_with_positions`. Otherwise, it scans all
    /// cached lines directly.
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

    /// Search with options while retaining the result-cap edge for `direction`.
    ///
    /// Results are always returned in ascending coordinate order. When more than
    /// [`MAX_SEARCH_MATCHES`] exist, forward search retains the oldest matches and
    /// backward search retains the newest matches. This lets an interactive
    /// reverse search start at the true newest occurrence without allocating an
    /// unbounded intermediate result set.
    pub fn search_with_positions_opts_direction(
        &self,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
        direction: SearchDirection,
    ) -> Result<Vec<SearchMatch>, SearchOptionsError> {
        if query.is_empty() {
            return Ok(Vec::new());
        }

        // Fast path: case-sensitive literal → trigram-accelerated search
        if case_sensitive && !is_regex {
            return Ok(match direction {
                SearchDirection::Forward => self.search_with_positions(query),
                SearchDirection::Backward => {
                    let mut matches: Vec<_> = self
                        .search_before_line(query, self.line_count)
                        .take(MAX_SEARCH_MATCHES)
                        .collect();
                    matches.reverse();
                    matches
                }
            });
        }

        if is_regex {
            return self.search_regex(query, case_sensitive, direction);
        }

        // Case-insensitive literal search
        Ok(self.search_case_insensitive(query, direction))
    }

    /// Search with options, returning matches bundled with the eviction signal.
    ///
    /// Identical matching to [`search_with_positions_opts`], but wraps the
    /// result in [`SearchResults`] so the caller learns whether eviction may
    /// have dropped matches ([`results_may_be_incomplete`]) and which line is
    /// the oldest still searchable ([`lowest_retained_line`]). This is the
    /// entry point intended for `cmd_search`, which must tell the AI when
    /// results are truncated.
    ///
    /// [`search_with_positions_opts`]: Self::search_with_positions_opts
    /// [`results_may_be_incomplete`]: Self::results_may_be_incomplete
    /// [`lowest_retained_line`]: Self::lowest_retained_line
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
    ///
    /// See [`search_with_positions_opts_direction`](Self::search_with_positions_opts_direction).
    pub fn search_results_opts_direction(
        &self,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
        direction: SearchDirection,
    ) -> Result<SearchResults, SearchOptionsError> {
        // A columns-only index keeps no postings and no bloom filter, so the
        // trigram-accelerated arms below would report "no matches" rather than
        // fail. Trip loudly in tests if one is ever queried (the budgeted engine
        // verifies rows itself and must never reach here).
        debug_assert!(
            self.maintain_trigrams,
            "search_results_opts on a columns-only index returns no matches"
        );
        let matches =
            self.search_with_positions_opts_direction(query, case_sensitive, is_regex, direction)?;
        // A full result set indicates the per-query MAX_SEARCH_MATCHES cap was hit
        // (round-8): report it via the same `incomplete` signal used for eviction so
        // callers do not treat a capped result set as exhaustive. (Over-reports when a
        // query has exactly MAX_SEARCH_MATCHES real matches — acceptable and honest.)
        let capped = matches.len() >= MAX_SEARCH_MATCHES;
        Ok(SearchResults::new(
            matches,
            self.results_may_be_incomplete() || capped,
            self.lowest_retained_line(),
        ))
    }

    /// Regex search across all cached lines.
    fn search_regex(
        &self,
        query: &str,
        case_sensitive: bool,
        direction: SearchDirection,
    ) -> Result<Vec<SearchMatch>, SearchOptionsError> {
        #[cfg(feature = "regex")]
        {
            let re = Self::compile_regex(query, case_sensitive)?;
            let mut matches = Vec::new();
            // Absolute row IDs occupy a bounded retained range. Range-scan it
            // directly instead of allocating and O(n log n)-sorting every hash
            // key for each typed regex character.
            let first = self.first_cached_line.min(self.line_count);
            let mut line_nums = match direction {
                SearchDirection::Forward => {
                    CandidateSource::Range(line_as_u32(first)..line_as_u32(self.line_count))
                }
                SearchDirection::Backward => CandidateSource::RangeRev(
                    (line_as_u32(first)..line_as_u32(self.line_count)).rev(),
                ),
            };
            'lines: while let Some(line_u32) = line_nums.next_candidate() {
                let line_num = line_u32 as usize;
                let Some(text) = self.lines.get(&line_num) else {
                    continue;
                };
                // Use cached column map when available (#7373).
                let fallback;
                let col_map = match self.column_maps.get(&line_num) {
                    Some(cm) => cm,
                    None => {
                        fallback = ColumnMap::new(text);
                        &fallback
                    }
                };
                match direction {
                    SearchDirection::Forward => {
                        for cap in re.find_iter(text) {
                            // Skip zero-length matches (e.g. `^`, `\b`, `x*` at
                            // non-matching positions), including byte spans that
                            // resolve to zero display columns.
                            if cap.start() == cap.end() {
                                continue;
                            }
                            let start_col = col_map.byte_to_column(cap.start());
                            let end_col = col_map.byte_to_column(cap.end());
                            if start_col == end_col {
                                continue;
                            }
                            matches.push(SearchMatch::new(line_num, start_col, end_col));
                            if matches.len() >= MAX_SEARCH_MATCHES {
                                break 'lines;
                            }
                        }
                    }
                    SearchDirection::Backward => {
                        // Regex iterators are forward-only within one line. A
                        // terminal line is width-bounded, so buffer just this
                        // line and consume it right-to-left; the global result
                        // vector remains capped.
                        let mut line_matches = Vec::new();
                        for cap in re.find_iter(text) {
                            if cap.start() == cap.end() {
                                continue;
                            }
                            let start_col = col_map.byte_to_column(cap.start());
                            let end_col = col_map.byte_to_column(cap.end());
                            if start_col != end_col {
                                line_matches.push(SearchMatch::new(line_num, start_col, end_col));
                            }
                        }
                        for found in line_matches.into_iter().rev() {
                            matches.push(found);
                            if matches.len() >= MAX_SEARCH_MATCHES {
                                break 'lines;
                            }
                        }
                    }
                }
            }
            // One check for the whole walk: `re` is compiled here and used
            // nowhere else, so its sticky flag can only have been set by the
            // lines just scanned. If any line was abandoned mid-scan, the list
            // below is missing matches — say so instead of returning it.
            if re.step_limit_exceeded() {
                return Err(Self::regex_scan_budget_exhausted());
            }
            // Backward collection is globally newest/rightmost first. One
            // linear reversal restores the ascending order expected by GUI
            // mapping without an O(k log k) result sort.
            if direction == SearchDirection::Backward {
                matches.reverse();
            }
            Ok(matches)
        }
        #[cfg(not(feature = "regex"))]
        {
            let _ = (query, case_sensitive, direction);
            Err(SearchOptionsError::RegexNotEnabled)
        }
    }

    /// Case-insensitive literal search across all cached lines.
    ///
    /// Uses lowercased trigrams for bloom filter negative filtering before
    /// scanning lines. For queries >= 3 bytes, this rejects lines that
    /// definitely do not contain the query, avoiding the full O(n) scan
    /// for most lines. Part of #7273.
    ///
    /// Uses an ASCII fast path (zero heap allocation) for the common case where
    /// the folded query and line are pure ASCII. Candidate line IDs are yielded
    /// lazily in ascending order: short queries use a numeric retained-line
    /// range, while trigram queries consume the ordered intersection bitmap.
    /// This avoids the former all-lines `Vec` allocation and sort. Unicode lines
    /// fall back to one reusable lowercase buffer. See #6726.
    fn search_case_insensitive(&self, query: &str, direction: SearchDirection) -> Vec<SearchMatch> {
        let mut matcher = CaseInsensitiveMatcher::new(query);
        if matcher.is_empty() {
            return Vec::new();
        }
        let mut matches = Vec::new();

        match direction {
            SearchDirection::Forward => {
                let mut candidates = self.literal_candidates_forward(&matcher.lower_query, 0);
                while let Some(line_u32) = candidates.next_candidate() {
                    #[cfg(test)]
                    count_case_insensitive_candidate_visit();
                    let line_num = line_u32 as usize;
                    let Some(text) = self.lines.get(&line_num) else {
                        continue;
                    };
                    let fallback;
                    let col_map = match self.column_maps.get(&line_num) {
                        Some(cm) => cm,
                        None => {
                            fallback = ColumnMap::new(text);
                            &fallback
                        }
                    };
                    let completed = matcher.visit_matches(line_num, text, col_map, |found| {
                        matches.push(found);
                        matches.len() < MAX_SEARCH_MATCHES
                    });
                    if !completed {
                        break;
                    }
                }
            }
            SearchDirection::Backward => {
                let mut candidates =
                    self.literal_candidates_backward(&matcher.lower_query, self.line_count);
                while let Some(line_u32) = candidates.next_candidate() {
                    #[cfg(test)]
                    count_case_insensitive_candidate_visit();
                    let line_num = line_u32 as usize;
                    let Some(text) = self.lines.get(&line_num) else {
                        continue;
                    };
                    let fallback;
                    let col_map = match self.column_maps.get(&line_num) {
                        Some(cm) => cm,
                        None => {
                            fallback = ColumnMap::new(text);
                            &fallback
                        }
                    };
                    let remaining = MAX_SEARCH_MATCHES.saturating_sub(matches.len());
                    if remaining == 0 {
                        break;
                    }
                    // Matches within one line arrive left-to-right. Retain at
                    // most the newest `remaining`, then append them right-to-left.
                    let mut line_matches = std::collections::VecDeque::new();
                    matcher.visit_matches(line_num, text, col_map, |found| {
                        if line_matches.len() == remaining {
                            line_matches.pop_front();
                        }
                        line_matches.push_back(found);
                        true
                    });
                    matches.extend(line_matches.into_iter().rev());
                    if matches.len() >= MAX_SEARCH_MATCHES {
                        break;
                    }
                }
                matches.reverse();
            }
        }
        matches
    }

    /// Cheap UPPER BOUND on how many candidate lines this index's OWN
    /// machinery would visit for a literal `query` — the number a narrowing
    /// frame has to beat to be worth using.
    ///
    /// `None` means the engine has no selectivity to offer: a query shorter
    /// than one trigram range-scans every retained line, so any frame (a
    /// subset of retained lines) is at least as cheap. Otherwise the bound is
    /// the SMALLEST involved posting list — the candidate driver walks exactly
    /// that list and binary-searches the rest — with `Some(0)` for a bloom
    /// rejection or an absent trigram (the engine visits nothing at all).
    ///
    /// Why callers need it (SA-1): narrowing is only a win while the frame is
    /// smaller than the index's own candidate set. The keystroke that turns a
    /// COMMON prefix into a RARE query inverts that — a purpose-made probe
    /// over 60k log-shaped lines measured the "size=" → "size=9" step at
    /// 2.60 ms off a 60 000-line frame against 0.085 ms through the trigram
    /// intersection, a 30x regression on exactly the keystroke whose latency
    /// the user feels most. A frame is a subset property, not a cost
    /// guarantee; policy layers compare against this bound and pass
    /// `prev_lines: None` when the engine would visit fewer lines.
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
            // Range scan over every retained line: no selectivity to offer.
            return None;
        }
        if self.lines.is_empty() || !self.might_contain(effective) {
            return Some(0);
        }
        // A missing trigram means the intersection is empty before it starts.
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

    /// One incremental-search narrowing step (SA-1, isearch): the FORWARD
    /// literal batch walk over an optional EXPLICIT candidate-line list,
    /// returning the batch results PLUS the fold-level occurrence lines that
    /// seed the next step.
    ///
    /// ## Contract
    ///
    /// - `prev_lines: None` runs the engine's own candidate machinery
    ///   (short-query range / bloom + posting intersection) and is
    ///   results-identical to
    ///   [`search_results_opts_direction`](Self::search_results_opts_direction)
    ///   with [`SearchDirection::Forward`] — same per-line verifiers
    ///   (`next_literal_match` / `CaseInsensitiveMatcher::visit_matches`),
    ///   same candidate enumeration, same cap edge (oldest retained). Pinned
    ///   by the differential battery in `tests.rs`.
    /// - `prev_lines: Some(frame)` replaces candidate enumeration with the
    ///   given ascending line list and touches NO posting list and NO bloom
    ///   probe — the per-keystroke win. The caller must pass a frame obtained
    ///   from a previous step for a query of which this `query` is a
    ///   byte-prefix extension, against the SAME index state. Soundness: a
    ///   reported match of `query` on a line implies the line's folded text
    ///   contains `lower_fold(query)` (case-insensitive) / its bytes contain
    ///   `query` (case-sensitive), which implies containment of every prefix's
    ///   fold — so every match line of the extended query is in the previous
    ///   query's OCCURRENCE frame. Results are then the complete match set,
    ///   identical to the batch walk (which merely enumerates a superset of
    ///   candidate lines and verifies each identically).
    /// - Direction: an UNCAPPED result is the complete ascending match set,
    ///   which both directions of the batch path return verbatim; a CAPPED
    ///   result equals the FORWARD batch's oldest-retaining edge only, so a
    ///   backward caller must fall back to the batch path when
    ///   `results.matches.len() >= MAX_SEARCH_MATCHES`.
    /// - `occurrence_lines` is `None` when the walk was capped: a truncated
    ///   frame would break the subset property, so the caller reseeds.
    ///
    /// Cost: O(candidate lines × line verify) with `Some(frame)` — no posting
    /// decode, no intersection, no range scan; exactly the category SA-1
    /// deletes for the grown-by-one-char keystroke.
    pub fn search_literal_narrowed(
        &self,
        query: &str,
        case_sensitive: bool,
        prev_lines: Option<&[u32]>,
    ) -> NarrowedSearch {
        // A columns-only index cannot answer queries — mirror the batch
        // entry points' loud tripwire.
        debug_assert!(
            self.maintain_trigrams,
            "search_literal_narrowed on a columns-only index returns no matches"
        );
        if query.is_empty() {
            // Batch parity: an empty query yields no matches; the honesty pair
            // is the index's own (search_results_opts_direction shape).
            return NarrowedSearch {
                results: SearchResults::new(
                    Vec::new(),
                    self.results_may_be_incomplete(),
                    self.lowest_retained_line(),
                ),
                occurrence_lines: None,
            };
        }

        /// Either the engine's candidate machinery or an explicit frame —
        /// both yield ascending, deduplicated retained line ids.
        enum Candidates<'a> {
            Engine(CandidateSource),
            Frame(std::iter::Copied<std::slice::Iter<'a, u32>>),
        }
        impl Candidates<'_> {
            fn next(&mut self) -> Option<u32> {
                match self {
                    Candidates::Engine(source) => source.next_candidate(),
                    Candidates::Frame(iter) => iter.next(),
                }
            }
        }

        let mut matcher = (!case_sensitive).then(|| CaseInsensitiveMatcher::new(query));
        let mut candidates = match prev_lines {
            Some(lines) => Candidates::Frame(lines.iter().copied()),
            None => Candidates::Engine(match &matcher {
                // Case-insensitive seed: exactly `search_case_insensitive`'s
                // forward candidate construction.
                Some(matcher) => self.literal_candidates_forward(&matcher.lower_query, 0),
                // Case-sensitive seed: exactly `search_with_positions` →
                // `search_from_line(query, first_cached_line.min(line_count))`.
                None => {
                    let first = self.first_cached_line.min(self.line_count);
                    if query.len() < 3 {
                        CandidateSource::Range(line_as_u32(first)..line_as_u32(self.line_count))
                    } else if !self.might_contain(query) {
                        CandidateSource::Empty
                    } else if let Some(postings) = self.posting_lists(query) {
                        CandidateSource::from_postings_forward(postings, line_as_u32(first))
                    } else {
                        CandidateSource::Empty
                    }
                }
            }),
        };

        let mut matches: Vec<SearchMatch> = Vec::new();
        let mut occurrence_lines: Vec<u32> = Vec::new();
        let mut capped = false;
        // ONE preparation of the needle for the whole sweep: the literal
        // verifier below runs per match and the occurrence probe per line, and
        // both used to re-derive the needle's critical factorization.
        let searcher = crate::bytesearch::Searcher::new(query.as_bytes());
        'lines: while let Some(line_u32) = candidates.next() {
            let line_num = line_u32 as usize;
            let Some(text) = self.lines.get(&line_num) else {
                continue;
            };
            let fallback;
            let col_map = match self.column_maps.get(&line_num) {
                Some(map) => map,
                None => {
                    fallback = ColumnMap::new(text);
                    &fallback
                }
            };
            let mut line_matched = false;
            match matcher.as_mut() {
                // Case-sensitive: the SINGLE shared literal verifier — the
                // same per-line sequence `SearchMatchIterator` yields.
                None => {
                    let mut from_byte = 0usize;
                    while let Some((found, resume)) =
                        next_literal_match(line_num, text, &searcher, col_map, from_byte)
                    {
                        from_byte = resume;
                        line_matched = true;
                        matches.push(found);
                        if matches.len() >= MAX_SEARCH_MATCHES {
                            // Batch parity: `.take(MAX_SEARCH_MATCHES)` stops
                            // exactly here, mid-line included.
                            occurrence_lines.push(line_u32);
                            capped = true;
                            break 'lines;
                        }
                    }
                    // Occurrence, not match: a byte-level containment probe so
                    // zero-display-width matches still keep the line in the
                    // frame (see `CaseInsensitiveMatcher::has_occurrence`).
                    if line_matched || searcher.find_in(text.as_bytes()).is_some() {
                        occurrence_lines.push(line_u32);
                    }
                }
                // Case-insensitive: the batch arm's exact visitor + cap.
                Some(matcher) => {
                    let completed = matcher.visit_matches(line_num, text, col_map, |found| {
                        line_matched = true;
                        matches.push(found);
                        matches.len() < MAX_SEARCH_MATCHES
                    });
                    if line_matched || matcher.has_occurrence(text) {
                        occurrence_lines.push(line_u32);
                    }
                    if !completed {
                        capped = true;
                        break 'lines;
                    }
                }
            }
        }

        // Cap detection mirrors `search_results_opts_direction`: a full result
        // set reports incomplete (over-reporting on exactly-MAX real matches
        // is the batch path's documented, honest behavior).
        let capped = capped || matches.len() >= MAX_SEARCH_MATCHES;
        NarrowedSearch {
            results: SearchResults::new(
                matches,
                self.results_may_be_incomplete() || capped,
                self.lowest_retained_line(),
            ),
            occurrence_lines: (!capped).then_some(occurrence_lines),
        }
    }

    /// Find one regex match at the directional anchor without materializing a
    /// capped global batch. Used by interactive navigation when total matches
    /// exceed [`MAX_SEARCH_MATCHES`].
    #[cfg(all(test, feature = "regex"))]
    pub(crate) fn find_regex_from(
        &self,
        query: &str,
        case_sensitive: bool,
        anchor_line: usize,
        anchor_col: usize,
        direction: SearchDirection,
        inclusive: bool,
    ) -> Result<Option<SearchMatch>, SearchOptionsError> {
        #[cfg(feature = "regex")]
        {
            let re = Self::compile_regex(query, case_sensitive)?;
            let found =
                self.find_compiled_regex_from(&re, anchor_line, anchor_col, direction, inclusive);
            if re.step_limit_exceeded() {
                return Err(Self::regex_scan_budget_exhausted());
            }
            Ok(found)
        }
        #[cfg(not(feature = "regex"))]
        {
            let _ = (
                query,
                case_sensitive,
                anchor_line,
                anchor_col,
                direction,
                inclusive,
            );
            Err(SearchOptionsError::RegexNotEnabled)
        }
    }

    /// Regex point navigation with one compilation shared by the first pass
    /// and optional wrap pass.
    #[allow(
        clippy::too_many_arguments,
        reason = "private twin of the stable point-search policy surface"
    )]
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
        #[cfg(feature = "regex")]
        {
            let re = Self::compile_regex(query, case_sensitive)?;
            let found =
                self.find_compiled_regex_from(&re, anchor_line, anchor_col, direction, inclusive);
            // Checked after each pass, before the answer is used: a `None` from
            // a walk that abandoned a line is not "there is no next match", and
            // navigating past a match the scan never finished reading would
            // move the cursor to the wrong place.
            if re.step_limit_exceeded() {
                return Err(Self::regex_scan_budget_exhausted());
            }
            if found.is_some() || !wrap {
                return Ok(found);
            }
            let wrapped = match direction {
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
            if re.step_limit_exceeded() {
                return Err(Self::regex_scan_budget_exhausted());
            }
            Ok(wrapped)
        }
        #[cfg(not(feature = "regex"))]
        {
            let _ = (
                query,
                case_sensitive,
                anchor_line,
                anchor_col,
                direction,
                inclusive,
                wrap,
            );
            Err(SearchOptionsError::RegexNotEnabled)
        }
    }

    #[cfg(feature = "regex")]
    fn find_compiled_regex_from(
        &self,
        re: &aterm_regex::Regex,
        anchor_line: usize,
        anchor_col: usize,
        direction: SearchDirection,
        inclusive: bool,
    ) -> Option<SearchMatch> {
        let lower = self.first_cached_line.min(self.line_count);
        let mut candidates = match direction {
            SearchDirection::Forward => CandidateSource::Range(
                line_as_u32(anchor_line.max(lower))..line_as_u32(self.line_count),
            ),
            SearchDirection::Backward => CandidateSource::RangeRev(
                (line_as_u32(lower)
                    ..line_as_u32(anchor_line.saturating_add(1).min(self.line_count)))
                    .rev(),
            ),
        };
        while let Some(line_u32) = candidates.next_candidate() {
            let line_num = line_u32 as usize;
            let Some(text) = self.lines.get(&line_num) else {
                continue;
            };
            let fallback;
            let col_map = match self.column_maps.get(&line_num) {
                Some(map) => map,
                None => {
                    fallback = ColumnMap::new(text);
                    &fallback
                }
            };
            match direction {
                SearchDirection::Forward => {
                    for cap in re.find_iter(text) {
                        if cap.start() == cap.end() {
                            continue;
                        }
                        let start_col = col_map.byte_to_column(cap.start());
                        let end_col = col_map.byte_to_column(cap.end());
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
                            return Some(SearchMatch::new(line_num, start_col, end_col));
                        }
                    }
                }
                SearchDirection::Backward => {
                    let mut found = None;
                    for cap in re.find_iter(text) {
                        if cap.start() == cap.end() {
                            continue;
                        }
                        let start_col = col_map.byte_to_column(cap.start());
                        let end_col = col_map.byte_to_column(cap.end());
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
                    if found.is_some() {
                        return found;
                    }
                }
            }
        }
        None
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
        while let Some(line_u32) = candidates.next_candidate() {
            #[cfg(test)]
            count_case_insensitive_candidate_visit();
            let line_num = line_u32 as usize;
            let Some(text) = self.lines.get(&line_num) else {
                continue;
            };
            let fallback;
            let col_map = match self.column_maps.get(&line_num) {
                Some(cm) => cm,
                None => {
                    fallback = ColumnMap::new(text);
                    &fallback
                }
            };
            let mut found = None;
            matcher.visit_matches(line_num, text, col_map, |candidate| {
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
            });
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
        while let Some(line_u32) = candidates.next_candidate() {
            #[cfg(test)]
            count_case_insensitive_candidate_visit();
            let line_num = line_u32 as usize;
            let Some(text) = self.lines.get(&line_num) else {
                continue;
            };
            let fallback;
            let col_map = match self.column_maps.get(&line_num) {
                Some(cm) => cm,
                None => {
                    fallback = ColumnMap::new(text);
                    &fallback
                }
            };
            let mut found = None;
            matcher.visit_matches(line_num, text, col_map, |candidate| {
                if candidate.line < anchor_line
                    || (candidate.line == anchor_line
                        && if inclusive {
                            candidate.start_col <= anchor_col
                        } else {
                            candidate.start_col < anchor_col
                        })
                {
                    found = Some(candidate);
                    true
                } else {
                    // Matches are ascending within the line. Once the boundary
                    // is reached, no later match on this line can qualify.
                    false
                }
            });
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

/// The regex **scan** budget, at the two entry points that take a pattern from
/// the search box and run it over untrusted terminal content.
///
/// `REGEX_SIZE_LIMIT` bounds what compiles; these tests are about what a
/// compiled pattern then costs to run, which is a separate axis and used to be
/// unbounded. See `REGEX_STEP_LIMIT`.
#[cfg(all(test, feature = "regex"))]
mod regex_scan_budget_tests {
    use super::*;

    /// A thirteen-byte pattern that compiles happily under the 128 KiB program
    /// ceiling (2,042 instructions) and then costs ~16.7M work units — ~19 ms
    /// in release — to cross a single 4,096-column line.
    const EXPENSIVE: &str = "(?:x?){1020}z";

    fn index_with_a_wide_line() -> SearchIndex {
        let mut index = SearchIndex::new();
        index.index_line(0, "an ordinary line of terminal output");
        index.index_line(1, &"x".repeat(4096));
        index.index_line(2, "another ordinary line");
        index
    }

    /// The budget is actually passed. Without this the two constants below are
    /// documentation rather than configuration.
    #[test]
    fn the_compiled_pattern_carries_the_scan_budget() {
        let re = SearchIndex::compile_regex("needle", true).expect("compiles");
        assert_eq!(re.step_limit(), REGEX_STEP_LIMIT);
        assert!(!re.step_limit_exceeded());
    }

    /// A pattern too expensive to finish a line is **refused**, not answered
    /// with a short list. The distinction is the whole point: a truncated
    /// result set presented as exhaustive is a wrong answer, and search results
    /// are read by `cmd_search` and by a human deciding what is in their
    /// scrollback.
    #[test]
    fn a_pattern_that_cannot_finish_a_line_is_refused_not_truncated() {
        let index = index_with_a_wide_line();
        let started = std::time::Instant::now();
        let err = index
            .search_with_positions_opts(EXPENSIVE, true, true)
            .expect_err("a scan that could not finish must not return matches");
        assert!(
            matches!(err, SearchOptionsError::InvalidRegex(ref m) if m.contains("too expensive")),
            "the refusal must name the cause: {err:?}"
        );
        assert!(
            started.elapsed().as_secs() < 5,
            "the scan took {:?}; the budget is not bounding it",
            started.elapsed()
        );

        // Point navigation refuses the same way, rather than reporting "no next
        // match" and moving the cursor somewhere wrong.
        let err = index
            .find_regex_direction(
                EXPENSIVE,
                true,
                DirectedFind {
                    anchor: (0, 0),
                    direction: SearchDirection::Forward,
                    inclusive: true,
                    wrap: true,
                },
            )
            .expect_err("navigation must refuse too");
        assert!(
            matches!(err, SearchOptionsError::InvalidRegex(_)),
            "{err:?}"
        );
    }

    /// Ordinary patterns over the same content are untouched: the budget is
    /// three orders of magnitude above what a real query costs.
    #[test]
    fn ordinary_regex_queries_are_unaffected() {
        let mut index = SearchIndex::new();
        index.index_line(0, "commit 66390b5c8f2a1b3c landed at 192.168.0.1");
        index.index_line(1, &"user@example.com ".repeat(240));
        index.index_line(2, "nothing here");

        for (pattern, want_lines) in [
            (r"\b[0-9a-f]{7,40}\b", vec![0usize]),
            (r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}", vec![1]),
            (r"\d+\.\d+\.\d+\.\d+", vec![0]),
        ] {
            let matches = index
                .search_with_positions_opts(pattern, true, true)
                .unwrap_or_else(|e| panic!("{pattern:?} must not be refused: {e}"));
            let mut lines: Vec<usize> = matches.iter().map(|m| m.line).collect();
            lines.dedup();
            assert_eq!(lines, want_lines, "{pattern:?}");
        }
    }
}
