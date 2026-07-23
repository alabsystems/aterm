// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

/// No false negatives: if line contains query, search finds it.
///
/// Symbolic over line count: proves that for any number of lines (1-3)
/// all containing the query substring, every line appears in results.
#[kani::proof]
#[kani::unwind(16)] // "hello world test" → 14 trigrams, bloom K=7; needs max(15, 8)=15, +1 margin
fn no_false_negatives_symbolic() {
    let mut index = SearchIndex::new();

    let count: usize = kani::any();
    kani::assume(count >= 1 && count <= 3);

    // Index `count` lines, each containing "world" (trigram "wor" present)
    for i in 0..count {
        index.index_line(i, "hello world test");
    }

    let query = "wor"; // 3+ chars required for trigram
    let results: Vec<_> = index.search(query).collect();

    // Every indexed line must appear — no false negatives
    for i in 0..count {
        let line_u32 = i as u32;
        kani::assert(
            results.contains(&line_u32),
            "false negative: indexed line not found in search results",
        );
    }
}

/// Index length is consistent with indexed lines.
// TODO(#7932): tautology — strengthen or delete — T1: constructor round-trip field == any-binding
#[kani::proof]
#[kani::unwind(10)] // "test line" → 7 trigrams, bloom K=7; needs max(8, 8)=8, +2 margin
fn index_length_consistent() {
    let mut index = SearchIndex::new();

    let count: usize = kani::any();
    kani::assume(count <= 5);

    for i in 0..count {
        index.index_line(i, "test line");
    }

    kani::assert(index.len() == count, "index length mismatch");
}

/// CRITICAL: Empty query must return empty results and terminate.
///
/// This proof catches the infinite loop bug where `"".find("")` returns
/// `Some(0)` infinitely. The fix is to check for empty query early.
/// Symbolic over line count: proves empty query returns empty for any
/// number of indexed lines (0-3).
#[kani::proof]
#[kani::unwind(16)]
fn empty_query_returns_empty_results() {
    let mut index = SearchIndex::new();

    let count: usize = kani::any();
    kani::assume(count <= 3);

    for i in 0..count {
        index.index_line(i, "test content");
    }

    // Empty query MUST return empty results regardless of index size
    let matches = index.search_with_positions("");

    kani::assert(
        matches.is_empty(),
        "empty query must return empty results for any index size",
    );
}

/// Empty query must be handled correctly by might_contain.
///
/// Symbolic over line count: proves might_contain("") returns true
/// regardless of how many lines (0-3) are indexed.
#[kani::proof]
#[kani::unwind(12)]
fn empty_query_might_contain_safe() {
    let mut index = SearchIndex::new();

    let count: usize = kani::any();
    kani::assume(count <= 3);

    for i in 0..count {
        index.index_line(i, "test");
    }

    // Empty query is short (<3 chars) so might_contain returns true
    // This is safe - the actual search will handle it
    let result = index.might_contain("");
    kani::assert(
        result == true,
        "empty query returns true for might_contain regardless of index size",
    );
}

/// Search match positions must be valid bounds.
///
/// Symbolic over line count: proves bounds validity holds regardless of
/// how many lines (1-3) are in the index. Verifies output positions
/// are always within indexed content for any index size.
#[kani::proof]
#[kani::unwind(12)] // "hello world" → 9 trigrams, bloom K=7; needs max(10, 8)=10, +2 margin
fn search_match_bounds_valid_symbolic() {
    let mut index = SearchIndex::new();

    let line = "hello world";
    let line_len = line.len();

    let count: usize = kani::any();
    kani::assume(count >= 1 && count <= 3);

    for i in 0..count {
        index.index_line(i, line);
    }

    let query = "wor";
    let matches = index.search_with_positions(query);

    for m in matches.iter() {
        // Line number must be within indexed range
        kani::assert(m.line < count, "line number must be < indexed count");
        // Start column must be less than end column
        kani::assert(m.start_col < m.end_col, "start_col must be < end_col");
        // End column must not exceed line length
        kani::assert(m.end_col <= line_len, "end_col must be <= line length");
        // Match length must equal query length
        kani::assert(
            m.end_col - m.start_col == query.len(),
            "match length must equal query length",
        );
    }
}

/// Search on empty index returns no matches for any query length.
///
/// Symbolic over query: proves that queries of length 1-4 all return
/// empty from an empty index (sub-trigram short-circuits, trigram finds nothing).
#[kani::proof]
#[kani::unwind(10)]
fn search_empty_index_returns_empty() {
    let index = SearchIndex::new();

    let query_len: u8 = kani::any();
    kani::assume(query_len >= 1 && query_len <= 4);

    // Build a query of symbolic length from "test" prefix
    let query = &"test"[..query_len as usize];
    let matches = index.search_with_positions(query);

    kani::assert(
        matches.is_empty(),
        "empty index must return empty results for any query length",
    );
}

/// Short query (< 3 chars) returns empty — sub-trigram queries cannot match.
///
/// Symbolic over line count and query length: proves sub-trigram queries
/// return empty regardless of how many lines (1-3) are indexed, for
/// any query of length 1 or 2.
#[kani::proof]
#[kani::unwind(12)]
fn short_query_safe() {
    let mut index = SearchIndex::new();

    let count: usize = kani::any();
    kani::assume(count >= 1 && count <= 3);

    for i in 0..count {
        index.index_line(i, "test");
    }

    let query_len: u8 = kani::any();
    kani::assume(query_len >= 1 && query_len <= 2);

    // Build a short query of symbolic length from "ab" prefix
    let query = &"ab"[..query_len as usize];

    // Queries shorter than 3 chars can't use trigrams
    // but must still work correctly (no panic, and return empty).
    let matches = index.search_with_positions(query);

    kani::assert(
        matches.is_empty(),
        "sub-trigram query must return empty regardless of index size",
    );
}

/// Query longer than line content handles correctly.
///
/// Symbolic over line count: proves that a query longer than any
/// indexed content returns empty for any number of lines (1-3).
#[kani::proof]
#[kani::unwind(16)]
fn query_longer_than_content_returns_empty() {
    let mut index = SearchIndex::new();

    let count: usize = kani::any();
    kani::assume(count >= 1 && count <= 3);

    for i in 0..count {
        index.index_line(i, "hi");
    }

    // Query is longer than any indexed content
    let matches = index.search_with_positions("hello world");

    kani::assert(
        matches.is_empty(),
        "query longer than content must return empty for any index size",
    );
}

/// TerminalSearch empty query is safe.
///
/// Symbolic over line count: proves empty query returns empty
/// regardless of how many scrollback lines (1-3) are indexed.
#[kani::proof]
#[kani::unwind(16)]
fn terminal_search_empty_query_safe() {
    let mut search = TerminalSearch::new();

    let count: usize = kani::any();
    kani::assume(count >= 1 && count <= 3);

    for _ in 0..count {
        search.index_scrollback_line("test line");
    }

    let matches = search.search("");

    kani::assert(
        matches.is_empty(),
        "terminal search empty query must return empty for any number of indexed lines",
    );
}

/// find_next with empty query is safe.
///
/// Symbolic over line count and starting position: proves find_next
/// with empty query returns None for any number of indexed lines (1-3)
/// and any starting position.
#[kani::proof]
#[kani::unwind(16)]
fn find_next_empty_query_safe() {
    let mut search = TerminalSearch::new();

    let count: usize = kani::any();
    kani::assume(count >= 1 && count <= 3);

    for _ in 0..count {
        search.index_scrollback_line("test");
    }

    let after_line: usize = kani::any();
    let after_col: usize = kani::any();
    kani::assume(after_line <= 5);
    kani::assume(after_col <= 10);

    let result = search.find_next("", after_line, after_col);

    kani::assert(
        result.is_none(),
        "find_next with empty query must return None for any position and index size",
    );
}

/// find_prev with empty query is safe.
///
/// Symbolic over line count and starting position: proves find_prev
/// with empty query returns None for any number of indexed lines (1-3)
/// and any starting position.
#[kani::proof]
#[kani::unwind(16)]
fn find_prev_empty_query_safe() {
    let mut search = TerminalSearch::new();

    let count: usize = kani::any();
    kani::assume(count >= 1 && count <= 3);

    for _ in 0..count {
        search.index_scrollback_line("test");
    }

    let before_line: usize = kani::any();
    let before_col: usize = kani::any();
    kani::assume(before_line <= 100);
    kani::assume(before_col <= 20);

    let result = search.find_prev("", before_line, before_col);

    kani::assert(
        result.is_none(),
        "find_prev with empty query must return None for any position and index size",
    );
}

/// find_prev at line 0 with col 0 returns None regardless of index size.
///
/// Symbolic over line count: proves that `find_prev(query, 0, 0)` always
/// returns None because no content can precede the origin position,
/// regardless of how many lines are indexed.
#[kani::proof]
#[kani::unwind(14)] // "test content" → 10 trigrams, bloom K=7; needs max(11, 8)=11, +3 margin
fn find_prev_at_origin_symbolic() {
    let mut search = TerminalSearch::new();

    let count: usize = kani::any();
    kani::assume(count >= 1 && count <= 3);

    for _ in 0..count {
        search.index_scrollback_line("test content");
    }

    let result = search.find_prev("test", 0, 0);

    kani::assert(
        result.is_none(),
        "find_prev at (0,0) must return None — nothing is before the origin",
    );
}

/// search_before_line(query, 0) returns no matches for any query.
///
/// Symbolic over line count: proves that search_before_line(query, 0)
/// returns empty regardless of how many lines (1-3) are indexed,
/// because the range [0..0) is always empty.
#[kani::proof]
#[kani::unwind(16)]
fn search_before_line_zero_empty() {
    let mut index = SearchIndex::new();

    let count: usize = kani::any();
    kani::assume(count >= 1 && count <= 3);

    for i in 0..count {
        index.index_line(i, "test content");
    }

    // before_line=0 means "search lines in range [0..0)" which is empty
    let matches: Vec<_> = index.search_before_line("test", 0).collect();

    kani::assert(
        matches.is_empty(),
        "search_before_line with before_line=0 must return empty for any index size",
    );
}

/// find_next with symbolic position always returns valid result or None.
///
/// Symbolic over after_line and after_col: proves that for any starting
/// position in the search space, `find_next` either returns a match that
/// is strictly after the given position, or None.
#[kani::proof]
#[kani::unwind(12)] // "hello world"/"hello again" → 9 trigrams, bloom K=7; needs max(10, 8)=10, +2 margin
fn find_next_position_contract_symbolic() {
    let mut search = TerminalSearch::new();
    search.index_scrollback_line("hello world");
    search.index_scrollback_line("test data");
    search.index_scrollback_line("hello again");

    let after_line: usize = kani::any();
    let after_col: usize = kani::any();
    kani::assume(after_line <= 5);
    kani::assume(after_col <= 20);

    let result = search.find_next("hel", after_line, after_col);

    if let Some(m) = result {
        // Match must be strictly after the given position
        kani::assert(
            m.line > after_line || (m.line == after_line && m.start_col > after_col),
            "find_next result must be after the search position",
        );
        // Match bounds must be valid
        kani::assert(m.start_col < m.end_col, "start_col must be < end_col");
        kani::assert(
            m.end_col - m.start_col == 3,
            "match length must equal query length",
        );
    }
}

/// find_prev with symbolic position always returns valid result or None.
///
/// Symmetric to `find_next_position_contract_symbolic`: proves that for
/// any starting position, `find_prev` returns a match strictly before
/// the given position, or None.
#[kani::proof]
#[kani::unwind(12)] // "hello world"/"hello again" → 9 trigrams, bloom K=7; needs max(10, 8)=10, +2 margin
fn find_prev_position_contract_symbolic() {
    let mut search = TerminalSearch::new();
    search.index_scrollback_line("hello world");
    search.index_scrollback_line("test data");
    search.index_scrollback_line("hello again");

    let before_line: usize = kani::any();
    let before_col: usize = kani::any();
    kani::assume(before_line <= 5);
    kani::assume(before_col <= 20);

    let result = search.find_prev("hel", before_line, before_col);

    if let Some(m) = result {
        // Match must be strictly before the given position
        kani::assert(
            m.line < before_line || (m.line == before_line && m.start_col < before_col),
            "find_prev result must be before the search position",
        );
        // Match bounds must be valid
        kani::assert(m.start_col < m.end_col, "start_col must be < end_col");
        kani::assert(
            m.end_col - m.start_col == 3,
            "match length must equal query length",
        );
    }
}

// ========================================================================
// Gap coverage: eviction, iterator completeness, iterator ordering
// Part of #2875
// ========================================================================

/// Eviction bounds the cache: after indexing beyond max_cached_lines,
/// lines.len() <= max_cached_lines and the most recently indexed line
/// survives. Non-evicted lines remain searchable.
///
/// Symbolic over cache capacity: proves the eviction invariants hold
/// for any max_cached_lines in [2, 4], indexing capacity+1 lines.
#[kani::proof]
#[kani::unwind(16)]
fn evict_oldest_lines_cache_bounded() {
    let max: usize = kani::any();
    kani::assume(max >= 2 && max <= 4);

    let mut index = SearchIndex::new();
    index.set_max_cached_lines(max);

    // Index max+1 lines to trigger eviction
    for i in 0..=max {
        index.index_line(i, "hello world test");
    }

    // (a) Cache must be bounded after eviction
    kani::assert(
        index.lines.len() <= max,
        "cache must be <= max_cached_lines after eviction for any capacity",
    );

    // (b) Most recently indexed line must survive
    kani::assert(
        index.lines.contains_key(&max),
        "most recent line must survive eviction for any capacity",
    );

    // (c) Non-evicted lines remain in search results (no false negatives
    //     for lines that are still cached)
    let results: Vec<u32> = index.search("wor").collect();
    for (&line_num, _) in &index.lines {
        let line_u32 = line_num as u32;
        kani::assert(
            results.contains(&line_u32),
            "non-evicted line must remain searchable for any capacity",
        );
    }
}

/// Eviction with symbolic capacity: for any max_cached_lines in [2,4],
/// the cache size invariant holds after indexing max+1 lines.
#[kani::proof]
#[kani::unwind(16)]
fn evict_oldest_lines_symbolic_capacity() {
    let max: usize = kani::any();
    kani::assume(max >= 2 && max <= 4);

    let mut index = SearchIndex::new();
    index.set_max_cached_lines(max);

    // Index max+1 lines to trigger exactly one eviction
    for i in 0..=max {
        index.index_line(i, "test content here");
    }

    // Post-eviction: cache at or below capacity
    kani::assert(
        index.lines.len() <= max,
        "cache must be <= max after eviction",
    );

    // index.len() tracks total lines, not just cached
    kani::assert(
        index.len() == max + 1,
        "index length must reflect all indexed lines",
    );
}

/// SearchMatchIterator completeness: overlapping "aa" in "aaa" yields
/// both matches at columns 0 and 1 regardless of the starting line.
/// Proves advancement logic does not skip overlapping matches.
///
/// Symbolic over start_line: proves overlapping matches are found
/// from any search start position (0-2).
#[kani::proof]
#[kani::unwind(16)]
fn search_match_iterator_completeness() {
    let mut index = SearchIndex::new();

    let line_num: usize = kani::any();
    kani::assume(line_num <= 2);

    // "aaa" contains "aa" at byte positions 0 and 1 (overlapping)
    index.index_line(line_num, "aaa");

    let matches: Vec<SearchMatch> = index.search_from_line("aa", line_num).collect();

    kani::assert(
        matches.len() == 2,
        "must find both overlapping 'aa' in 'aaa' from any starting line",
    );
    kani::assert(
        matches[0].start_col == 0,
        "first match at column 0 for any line",
    );
    kani::assert(
        matches[1].start_col == 1,
        "second match at column 1 for any line",
    );
    kani::assert(
        matches[0].line == line_num && matches[1].line == line_num,
        "all matches must be on the indexed line",
    );
}

/// SearchMatchReverseIterator yields matches in descending column order
/// within a single line.
///
/// Symbolic over line number: proves reverse ordering holds regardless
/// of which line number (0-2) the content is indexed on.
#[kani::proof]
#[kani::unwind(20)]
fn search_match_reverse_iterator_ordering() {
    let mut index = SearchIndex::new();

    let line_num: usize = kani::any();
    kani::assume(line_num <= 2);

    index.index_line(line_num, "abc abc abc");

    // search_before_line needs an exclusive upper bound > line_num
    let matches: Vec<SearchMatch> = index.search_before_line("abc", line_num + 1).collect();

    kani::assert(
        matches.len() == 3,
        "must find all 3 'abc' matches on any line",
    );
    // Reverse order: rightmost first
    kani::assert(
        matches[0].start_col >= matches[1].start_col,
        "reverse iterator: match[0].col >= match[1].col for any line",
    );
    kani::assert(
        matches[1].start_col >= matches[2].start_col,
        "reverse iterator: match[1].col >= match[2].col for any line",
    );
    // All matches must be on the correct line
    for m in &matches {
        kani::assert(
            m.line == line_num,
            "all matches must be on the indexed line",
        );
    }
}
