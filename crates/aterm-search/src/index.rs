// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Core trigram search index with bloom filter acceleration.

use aterm_hash::{FxBuildHasher, FxHashMap};

use crate::bitmap::SparseBitmap;

use super::bloom::BloomFilter;
use super::iterators::{CandidateSource, SearchMatchIterator, SearchMatchReverseIterator};
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
/// One- and two-byte ASCII queries take the allocation-free memchr path. Longer
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
}

impl SearchIndex {
    #[cfg(feature = "regex")]
    pub(crate) fn compile_regex(
        query: &str,
        case_sensitive: bool,
    ) -> Result<regex::Regex, SearchOptionsError> {
        if query.len() > MAX_REGEX_PATTERN_LEN {
            return Err(SearchOptionsError::PatternTooLong);
        }
        let pattern = if case_sensitive {
            query.to_string()
        } else {
            format!("(?i){query}")
        };
        regex::RegexBuilder::new(&pattern)
            .size_limit(REGEX_SIZE_LIMIT)
            .dfa_size_limit(REGEX_DFA_SIZE_LIMIT)
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
        }
    }

    /// Index a line at a specific line number.
    ///
    /// This overwrites any existing content at that line number.
    pub fn index_line(&mut self, line_num: usize, text: &str) {
        // Remove old trigrams if this line was previously indexed.
        // Use remove() to move the old String out (avoids clone).
        if let Some(old_text) = self.lines.remove(&line_num) {
            self.remove_trigrams(line_num, &old_text);
        }

        let bytes = text.as_bytes();
        let line_u32 = line_as_u32(line_num);

        // Add all trigrams from this line (original case).
        for window in bytes.windows(3) {
            let trigram: [u8; 3] = [window[0], window[1], window[2]];
            self.bloom.insert_bytes(&trigram);
            self.trigrams.entry(trigram).or_default().insert(line_u32);
        }

        // Also insert Unicode-lowercased trigrams for case-insensitive
        // bloom filter and posting-list acceleration (#7273, #7398, #7470).
        // Uses full Unicode lowercasing so non-ASCII characters
        // (e.g., Ä→ä, É→é) are indexed correctly.
        //
        // Skip this pass entirely when lowercasing cannot change any byte: for
        // pure-ASCII text with no uppercase letter, `to_lowercase()` is the
        // identity, so the lowered trigrams equal the original-case ones already
        // inserted above. The predicate lives in `lower_need` and is shared with
        // `remove_trigrams`/`rebuild_bloom` so insert/remove/rebuild stay
        // symmetric by construction.
        //
        // Neither non-identity arm allocates per line any more: pure-ASCII text
        // lowers per byte in place off the ORIGINAL window (lowering an ASCII
        // string never changes its byte length, so the windows correspond
        // one-to-one), and the Unicode arm folds into a reused scratch buffer.
        // The old shape built a fresh capacity-less `String` per line — ~5
        // reallocations for an 80-column line — only to walk it once and drop
        // it, on the per-line primitive every index build runs.
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
                    self.trigrams.entry(trigram).or_default().insert(line_u32);
                }
            }
            LowerNeed::Unicode => {
                lower_fold_into(text, &mut self.lower_scratch);
                for window in self.lower_scratch.as_bytes().windows(3) {
                    let trigram: [u8; 3] = [window[0], window[1], window[2]];
                    self.bloom.insert_bytes(&trigram);
                    self.trigrams.entry(trigram).or_default().insert(line_u32);
                }
            }
        }

        // Cache the line content and precomputed column map (#7373).
        self.lines.insert(line_num, text.to_string());
        self.column_maps.insert(line_num, ColumnMap::new(text));
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
        // to restore its effectiveness.
        if self.bloom.is_saturated() {
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

    /// Remove trigrams for a line (internal helper).
    ///
    /// Prunes empty bitmaps from the trigrams map to prevent unbounded growth (#2111).
    fn remove_trigrams(&mut self, line_num: usize, text: &str) {
        let bytes = text.as_bytes();
        let line_u32 = line_as_u32(line_num);

        // Remove original-case trigrams.
        for window in bytes.windows(3) {
            let trigram: [u8; 3] = [window[0], window[1], window[2]];
            if let Some(bitmap) = self.trigrams.get_mut(&trigram) {
                bitmap.remove(line_u32);
                if bitmap.is_empty() {
                    self.trigrams.remove(&trigram);
                }
            }
        }

        // Remove Unicode-lowercased trigrams (#7398, #7470).
        //
        // Mirrors `index_line` arm for arm through the shared `lower_need`
        // classifier: when lowercasing is the identity (pure-ASCII, no
        // uppercase) the lowered pass was never inserted, so it must not be
        // removed either, or removal would double-delete shared trigrams and
        // leak/corrupt posting lists. The ASCII arm derives the same lowered
        // trigrams from the original bytes; the Unicode arm reuses the index's
        // scratch buffer. Both produce byte-identical trigrams to the insert.
        match lower_need(text) {
            LowerNeed::None => {}
            LowerNeed::Ascii => {
                for window in bytes.windows(3) {
                    let trigram: [u8; 3] = [
                        window[0].to_ascii_lowercase(),
                        window[1].to_ascii_lowercase(),
                        window[2].to_ascii_lowercase(),
                    ];
                    if let Some(bitmap) = self.trigrams.get_mut(&trigram) {
                        bitmap.remove(line_u32);
                        if bitmap.is_empty() {
                            self.trigrams.remove(&trigram);
                        }
                    }
                }
            }
            LowerNeed::Unicode => {
                lower_fold_into(text, &mut self.lower_scratch);
                for window in self.lower_scratch.as_bytes().windows(3) {
                    let trigram: [u8; 3] = [window[0], window[1], window[2]];
                    if let Some(bitmap) = self.trigrams.get_mut(&trigram) {
                        bitmap.remove(line_u32);
                        if bitmap.is_empty() {
                            self.trigrams.remove(&trigram);
                        }
                    }
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
        let watermark = line_as_u32(self.lowest_retained_line);
        self.trigrams.retain(|_, bitmap| {
            bitmap.drop_below(watermark);
            !bitmap.is_empty()
        });

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

        // Rebuild bloom filter from remaining lines (#7270).
        self.rebuild_bloom();
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
/// Caps NFA/DFA memory to 1 MiB, well below the `regex` crate's 10 MiB
/// default. This bounds compilation time for deeply nested alternations and
/// large repetition counts that could otherwise cause ReDoS via compilation.
#[cfg(feature = "regex")]
const REGEX_SIZE_LIMIT: usize = 1 << 20; // 1 MiB

/// Maximum DFA size (bytes) passed to `RegexBuilder::dfa_size_limit`.
///
/// Caps DFA cache to 1 MiB. The DFA is built lazily during matching, so this
/// bounds per-query memory even for patterns that pass the NFA size gate.
#[cfg(feature = "regex")]
const REGEX_DFA_SIZE_LIMIT: usize = 1 << 20; // 1 MiB

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
            Ok(self.find_compiled_regex_from(&re, anchor_line, anchor_col, direction, inclusive))
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
            if found.is_some() || !wrap {
                return Ok(found);
            }
            Ok(match direction {
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
            })
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
        re: &regex::Regex,
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
