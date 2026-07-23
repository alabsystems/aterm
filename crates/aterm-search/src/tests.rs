// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn index_and_search() {
    let mut index = SearchIndex::new();

    index.index_line(0, "hello world");
    index.index_line(1, "goodbye world");
    index.index_line(2, "hello there");

    // Search for "world"
    let results: Vec<_> = index.search("world").collect();
    assert!(results.contains(&0));
    assert!(results.contains(&1));
    assert!(!results.contains(&2));

    // Search for "hello"
    let results: Vec<_> = index.search("hello").collect();
    assert!(results.contains(&0));
    assert!(results.contains(&2));
}

#[test]
fn empty_query() {
    let mut index = SearchIndex::new();
    index.index_line(0, "test");

    // Short queries return all lines
    let results: Vec<_> = index.search("ab").collect();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], 0, "short query should return the indexed line");
}

/// CRITICAL: Empty query must return empty results without infinite loop.
///
/// This catches the bug where `"text".find("")` returns `Some(0)` forever.
#[test]
fn empty_query_search_with_positions() {
    let mut index = SearchIndex::new();
    index.index_line(0, "test content");
    index.index_line(1, "more content");

    // Empty query MUST return empty results (not infinite loop)
    let matches = index.search_with_positions("");
    assert!(matches.is_empty(), "empty query must return empty results");
}

/// Empty query through TerminalSearch API.
#[test]
fn empty_query_terminal_search() {
    let mut search = TerminalSearch::new();
    search.index_scrollback_line("test line");

    assert!(search.search("").is_empty());
    assert!(
        search.find_next("", 0, 0).is_none(),
        "empty query find_next should return None"
    );
    assert!(
        search.find_prev("", 10, 0).is_none(),
        "empty query find_prev should return None"
    );
}

#[test]
fn no_matches() {
    let mut index = SearchIndex::new();
    index.index_line(0, "hello world");

    let results: Vec<_> = index.search("xyz").collect();
    assert!(results.is_empty());
}

#[test]
fn bloom_filter_rejection() {
    let mut index = SearchIndex::new();
    index.index_line(0, "hello world");

    // Positive: trigrams present in indexed content should pass
    assert!(index.might_contain("hello"));
    assert!(index.might_contain("world"));

    // Negative: trigrams completely absent from indexed content should be rejected.
    // "zzqxj" shares no trigrams with "hello world", so the bloom filter must reject it.
    assert!(
        !index.might_contain("zzqxj"),
        "bloom filter should reject query with no matching trigrams"
    );

    // Short queries (<3 chars) bypass the bloom filter and always return true
    assert!(index.might_contain("zz"));
}

#[test]
fn search_with_positions() {
    let mut index = SearchIndex::new();
    index.index_line(0, "hello hello");
    index.index_line(1, "hello world");

    let matches = index.search_with_positions("hello");

    // Line 0 has 2 matches + line 1 has 1 match = 3 total
    assert_eq!(matches.len(), 3);

    // Line 0 should have two matches
    let line0_matches: Vec<_> = matches.iter().filter(|m| m.line == 0).collect();
    assert_eq!(line0_matches.len(), 2);

    // Verify positions
    assert_eq!(line0_matches[0].start_col, 0);
    assert_eq!(line0_matches[0].end_col, 5);
    assert_eq!(line0_matches[1].start_col, 6);
    assert_eq!(line0_matches[1].end_col, 11);
}

#[test]
fn search_ordered() {
    let mut index = SearchIndex::new();
    index.index_line(0, "test line 0");
    index.index_line(1, "test line 1");
    index.index_line(2, "test line 2");

    // Forward search
    let fwd = index.search_ordered("test", SearchDirection::Forward);
    assert_eq!(fwd[0].line, 0);
    assert_eq!(fwd[1].line, 1);
    assert_eq!(fwd[2].line, 2);

    // Backward search
    let bwd = index.search_ordered("test", SearchDirection::Backward);
    assert_eq!(bwd[0].line, 2);
    assert_eq!(bwd[1].line, 1);
    assert_eq!(bwd[2].line, 0);
}

#[test]
fn push_line() {
    let mut index = SearchIndex::new();

    let n0 = index.push_line("line 0");
    let n1 = index.push_line("line 1");
    let n2 = index.push_line("line 2");

    assert_eq!(n0, 0);
    assert_eq!(n1, 1);
    assert_eq!(n2, 2);
    assert_eq!(index.len(), 3);
}

#[test]
fn terminal_search_basic() {
    let mut search = TerminalSearch::new();

    search.index_scrollback_line("scrollback line 1");
    search.index_scrollback_line("scrollback line 2");
    search.index_scrollback_line("scrollback line 3");

    let matches = search.search("scrollback");
    assert_eq!(matches.len(), 3);
    // Each match should start at column 0 (where "scrollback" begins in each line)
    for (i, m) in matches.iter().enumerate() {
        assert_eq!(m.line, i, "match {i} should be on line {i}");
        assert_eq!(m.start_col, 0, "match {i} should start at column 0");
    }

    assert_eq!(search.indexed_scrollback_count(), 3);
}

#[test]
fn terminal_search_find_next_prev() {
    let mut search = TerminalSearch::new();

    search.index_scrollback_line("match here");
    search.index_scrollback_line("no match");
    search.index_scrollback_line("match again");

    // Find next from before start (should find first match at line 0, col 0)
    let next = search.find_next("match", 0, 0);
    // This should NOT find line 0, col 0 since we want AFTER (0, 0)
    // But "no match" at line 1 has "match" starting at col 3
    let m = next.expect("find_next should find 'match' in 'no match' at line 1");
    assert_eq!(m.line, 1);
    assert_eq!(m.start_col, 3);

    // Find next from after line 1 match
    let next = search
        .find_next("match", 1, 7)
        .expect("find_next should find 'match again' at line 2");
    assert_eq!(next.line, 2);

    // Find prev from end — "match again" at line 2, col 0
    let prev = search.find_prev("match", 3, 0);
    let m = prev.unwrap();
    assert_eq!(m.line, 2);
    assert_eq!(m.start_col, 0);
    assert_eq!(m.end_col, 5);

    // Find prev from line 2 — "no match" at line 1, col 3
    let prev = search.find_prev("match", 2, 0);
    let m = prev.unwrap();
    assert_eq!(m.line, 1);
    assert_eq!(m.start_col, 3);
    assert_eq!(m.end_col, 8);

    // Find prev from line 1, col 0 — "match here" at line 0, col 0
    let prev = search.find_prev("match", 1, 0);
    let m = prev.unwrap();
    assert_eq!(m.line, 0);
    assert_eq!(m.start_col, 0);
    assert_eq!(m.end_col, 5);
}

/// Verify that find_next/find_prev same-line column filtering uses strict
/// inequality (> / <), not inclusive (>= / <=). An off-by-one here would
/// cause find_next to re-find the current match or find_prev to skip valid
/// matches at the boundary.
#[test]
fn find_next_prev_same_line_column_boundary() {
    let mut search = TerminalSearch::new();
    // "test test" has "test" at cols 0 and 5
    search.index_scrollback_line("test test");

    // find_next from (0, 0) should skip the match AT col 0 and find col 5
    let m = search.find_next("test", 0, 0).unwrap();
    assert_eq!(m.line, 0);
    assert_eq!(m.start_col, 5);

    // find_next from (0, 4) should find col 5 (strictly after col 4)
    let m = search.find_next("test", 0, 4).unwrap();
    assert_eq!(m.line, 0);
    assert_eq!(m.start_col, 5);

    // find_next from (0, 5) should NOT find col 5 (not strictly after col 5)
    assert!(
        search.find_next("test", 0, 5).is_none(),
        "find_next at exact match col should not re-find same match"
    );

    // find_prev from (0, 5) should find col 0 (strictly before col 5)
    let m = search
        .find_prev("test", 0, 5)
        .expect("find_prev from col 5 should find col 0");
    assert_eq!(m.line, 0);
    assert_eq!(m.start_col, 0);

    // find_prev from (0, 1) should find col 0 (strictly before col 1)
    let m = search.find_prev("test", 0, 1).unwrap();
    assert_eq!(m.line, 0);
    assert_eq!(m.start_col, 0);

    // find_prev from (0, 0) should NOT find col 0 (not strictly before col 0)
    assert!(
        search.find_prev("test", 0, 0).is_none(),
        "find_prev at col 0 should not find match at col 0"
    );
}

#[test]
fn search_match_struct() {
    let m = SearchMatch::new(5, 10, 15);
    assert_eq!(m.line, 5);
    assert_eq!(m.start_col, 10);
    assert_eq!(m.end_col, 15);
}

#[test]
fn index_clear() {
    let mut index = SearchIndex::new();
    index.index_line(0, "test");
    assert!(!index.is_empty());

    index.clear();
    assert!(index.is_empty());
    assert_eq!(index.len(), 0);
}

#[test]
fn get_line() {
    let mut index = SearchIndex::new();
    index.index_line(5, "hello");

    assert_eq!(index.get_line(5), Some("hello"));
    assert_eq!(index.get_line(0), None);
}

#[test]
fn reindex_line() {
    let mut index = SearchIndex::new();
    index.index_line(0, "original");

    let results: Vec<_> = index.search("original").collect();
    assert!(results.contains(&0));

    // Reindex same line with different content
    index.index_line(0, "updated");

    let results: Vec<_> = index.search("original").collect();
    assert!(results.is_empty());

    let results: Vec<_> = index.search("updated").collect();
    assert!(results.contains(&0));
}

#[test]
fn search_from_line_basic() {
    let mut index = SearchIndex::new();
    index.index_line(0, "test line zero");
    index.index_line(1, "test line one");
    index.index_line(2, "test line two");
    index.index_line(3, "test line three");
    index.index_line(4, "test line four");

    // Search from line 2 - should only find lines 2, 3, 4
    let matches: Vec<_> = index.search_from_line("test", 2).collect();
    assert_eq!(matches.len(), 3);
    assert!(matches.iter().all(|m| m.line >= 2));
    assert_eq!(matches[0].line, 2);
    assert_eq!(matches[1].line, 3);
    assert_eq!(matches[2].line, 4);
}

#[test]
fn search_from_line_empty_query() {
    let mut index = SearchIndex::new();
    index.index_line(0, "test");

    let matches: Vec<_> = index.search_from_line("", 0).collect();
    assert!(matches.is_empty());
}

#[test]
fn search_from_line_short_query() {
    let mut index = SearchIndex::new();
    index.index_line(0, "ab test");
    index.index_line(1, "ab test");
    index.index_line(2, "ab test");

    // Short queries (<3 chars) return all lines from from_line
    let matches: Vec<_> = index.search_from_line("ab", 1).collect();
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0].line, 1);
    assert_eq!(matches[1].line, 2);
}

#[test]
fn search_from_line_no_matches() {
    let mut index = SearchIndex::new();
    index.index_line(0, "hello world");
    index.index_line(1, "goodbye world");

    let matches: Vec<_> = index.search_from_line("xyz", 0).collect();
    assert!(matches.is_empty());
}

#[test]
fn search_before_line_basic() {
    let mut index = SearchIndex::new();
    index.index_line(0, "test line zero");
    index.index_line(1, "test line one");
    index.index_line(2, "test line two");
    index.index_line(3, "test line three");
    index.index_line(4, "test line four");

    // Search before line 3 - should only find lines 0, 1, 2 in reverse order
    let matches: Vec<_> = index.search_before_line("test", 3).collect();
    assert_eq!(matches.len(), 3);
    assert!(matches.iter().all(|m| m.line < 3));
    // Should be in reverse order (newest to oldest)
    assert_eq!(matches[0].line, 2);
    assert_eq!(matches[1].line, 1);
    assert_eq!(matches[2].line, 0);
}

#[test]
fn search_before_line_empty_query() {
    let mut index = SearchIndex::new();
    index.index_line(0, "test");

    let matches: Vec<_> = index.search_before_line("", 10).collect();
    assert!(matches.is_empty());
}

#[test]
fn search_iterator_early_termination() {
    let mut index = SearchIndex::new();
    // Index 1000 lines with "test"
    for i in 0..1000 {
        index.index_line(i, &format!("test line {i}"));
    }

    // Using the iterator with early termination (via .next())
    // should not need to process all 1000 lines
    let mut iter = index.search_from_line("test", 500);
    let first = iter.next().unwrap();
    assert_eq!(first.line, 500);
    assert_eq!(first.start_col, 0);

    // We can continue iteration if needed
    let second = iter.next().unwrap();
    assert_eq!(second.line, 501);
    assert_eq!(second.start_col, 0);
}

#[test]
fn search_match_iterator_multiple_matches_per_line() {
    let mut index = SearchIndex::new();
    index.index_line(0, "test test test");
    index.index_line(1, "test");

    let matches: Vec<_> = index.search_from_line("test", 0).collect();
    // Line 0 has 3 matches, line 1 has 1 match
    assert_eq!(matches.len(), 4);
    assert_eq!(matches[0].line, 0);
    assert_eq!(matches[0].start_col, 0);
    assert_eq!(matches[1].line, 0);
    assert_eq!(matches[1].start_col, 5);
    assert_eq!(matches[2].line, 0);
    assert_eq!(matches[2].start_col, 10);
    assert_eq!(matches[3].line, 1);
}

#[test]
fn search_reverse_iterator_multiple_matches_per_line() {
    let mut index = SearchIndex::new();
    index.index_line(0, "test test test");
    index.index_line(1, "test");

    let matches: Vec<_> = index.search_before_line("test", 2).collect();
    // Should be in reverse order: line 1, then line 0 (right to left)
    assert_eq!(matches.len(), 4);
    assert_eq!(matches[0].line, 1);
    // Line 0's matches should be in reverse column order
    assert_eq!(matches[1].line, 0);
    assert_eq!(matches[1].start_col, 10); // rightmost first
    assert_eq!(matches[2].line, 0);
    assert_eq!(matches[2].start_col, 5);
    assert_eq!(matches[3].line, 0);
    assert_eq!(matches[3].start_col, 0); // leftmost last
}

#[test]
fn find_next_optimized() {
    let mut search = TerminalSearch::new();

    // Index many lines to test that we don't scan all of them
    for i in 0..100 {
        search.index_scrollback_line(&format!("match at line {i}"));
    }

    // Find next from line 50 — "match" at col 0 is excluded (not strictly after col 0),
    // so result is line 51 at col 0
    let m = search.find_next("match", 50, 0).unwrap();
    assert_eq!(m.line, 51);
    assert_eq!(m.start_col, 0);

    // Find from middle of line 50 — "match" at col 0 is before col 5, so skipped
    let m = search.find_next("match", 50, 5).unwrap();
    assert_eq!(m.line, 51);
    assert_eq!(m.start_col, 0);
}

/// Verify `find_next` lazily visits only the first suffix candidate and never
/// touches prefix matches before `after_line`.
#[test]
fn find_next_range_query_skips_prefix_matches() {
    fn measure_range_candidates(prefix_matches: usize) -> usize {
        let mut search = TerminalSearch::new();
        for i in 0..prefix_matches {
            search.index_scrollback_line(&format!("prefix needle line {i}"));
        }

        let from_line = prefix_matches;
        let suffix_matches = 8usize;
        for i in 0..suffix_matches {
            search.index_scrollback_line(&format!("suffix needle line {i}"));
        }

        // Clear any prior counter state from earlier tests.
        let _ = SearchIndex::take_search_from_line_candidates();

        let next = search
            .find_next("needle", from_line, 0)
            .expect("expected match in suffix region");
        assert_eq!(next.line, from_line, "first match should be at from_line");

        SearchIndex::take_search_from_line_candidates()
    }

    let small_candidates = measure_range_candidates(64);
    let large_candidates = measure_range_candidates(4096);

    assert_eq!(small_candidates, 1, "small-prefix run visits one candidate");
    assert_eq!(large_candidates, 1, "large-prefix run visits one candidate");
}

#[test]
fn find_prev_optimized() {
    let mut search = TerminalSearch::new();

    // Index many lines
    for i in 0..100 {
        search.index_scrollback_line(&format!("match at line {i}"));
    }

    // Find prev from line 50, col 100 — "match" at col 0 is before col 100, so found
    let m = search.find_prev("match", 50, 100).unwrap();
    assert_eq!(m.line, 50);
    assert_eq!(m.start_col, 0);

    // Find prev from line 50 col 0 — "match" at col 0 is NOT before col 0, so line 49
    let m = search.find_prev("match", 50, 0).unwrap();
    assert_eq!(m.line, 49);
    assert_eq!(m.start_col, 0);
}

/// Regression: searching for a multi-byte UTF-8 query that appears multiple times
/// must not panic when advancing past the first match.
///
/// The bug: `start = abs_pos + 1` advances by one *byte*, which can land in the
/// middle of a multi-byte character. `text[start..]` then panics because `start`
/// is not a char boundary.
#[test]
fn search_multibyte_query_no_panic() {
    let mut index = SearchIndex::new();
    // "日本語" repeated: each char is 3 bytes
    index.index_line(0, "日本語 日本語");

    // Searching for a multi-byte string should find both occurrences without panic
    let matches = index.search_with_positions("日本語");
    assert_eq!(matches.len(), 2, "should find both occurrences");
    assert_eq!(matches[0].start_col, 0);
    // "日本語 " = 3 CJK chars × 2 columns + 1 space = 7 columns
    assert_eq!(matches[1].start_col, 7);
}

/// Regression: overlapping multi-byte matches via the iterator path.
#[test]
fn search_iterator_multibyte_no_panic() {
    let mut index = SearchIndex::new();
    // "ああああ" — each "あ" is 3 bytes. Searching for "ああ" should find
    // overlapping matches at byte offsets 0 and 3 (not 0 and 1).
    index.index_line(0, "ああああ");

    let matches: Vec<_> = index.search_from_line("ああ", 0).collect();
    // "ああああ" has 3 overlapping "ああ" matches; each "あ" is 2 columns wide
    assert_eq!(matches.len(), 3, "should find 3 overlapping matches");
    assert_eq!(matches[0].start_col, 0); // column 0
    assert_eq!(matches[1].start_col, 2); // column 2 (after 1st "あ")
    assert_eq!(matches[2].start_col, 4); // column 4 (after 2nd "あ")
}

/// Regression: single multi-byte character query on a line with that char repeated.
#[test]
fn search_single_multibyte_char_repeated() {
    let mut index = SearchIndex::new();
    index.index_line(0, "日日日");

    // "日" is 3 bytes. After finding at byte 0, advancing by +1 lands at byte 1
    // (middle of the character), causing a panic on `text[1..]`.
    let matches = index.search_with_positions("日");
    assert_eq!(matches.len(), 3, "should find all three occurrences");
    // Each CJK character is 2 display columns wide
    assert_eq!(matches[0].start_col, 0, "first 日 at column 0");
    assert_eq!(matches[1].start_col, 2, "second 日 at column 2");
    assert_eq!(matches[2].start_col, 4, "third 日 at column 4");
}

/// Verify `index_scrollback_lines` batch method indexes all lines.
///
/// Previously a full coverage gap — no test, no Kani proof.
#[test]
fn terminal_search_index_scrollback_lines_batch() {
    let mut search = TerminalSearch::new();

    let lines = vec!["first line", "second line", "third line"];
    search.index_scrollback_lines(lines);

    assert_eq!(search.indexed_scrollback_count(), 3);
    let matches = search.search("line");
    assert_eq!(matches.len(), 3, "should find 'line' in all three lines");
    assert_eq!(matches[0].line, 0);
    assert_eq!(matches[1].line, 1);
    assert_eq!(matches[2].line, 2);
}

/// Verify `index_visible_content` indexes at correct base offset.
///
/// Previously a full coverage gap — no test, no Kani proof.
#[test]
fn terminal_search_index_visible_content() {
    let mut search = TerminalSearch::new();

    // Index 3 scrollback lines first
    search.index_scrollback_line("scrollback 0");
    search.index_scrollback_line("scrollback 1");
    search.index_scrollback_line("scrollback 2");

    // Index visible grid starting at line 3
    let visible = vec!["visible line A", "visible line B"];
    search.index_visible_content(3, visible);

    // Verify scrollback content still searchable
    let matches = search.search("scrollback");
    assert_eq!(matches.len(), 3, "scrollback lines should be found");

    // Verify visible content at correct line numbers
    let matches = search.search("visible");
    assert_eq!(matches.len(), 2, "visible lines should be found");
    assert_eq!(matches[0].line, 3, "first visible line at index 3");
    assert_eq!(matches[1].line, 4, "second visible line at index 4");
}

/// Verify `SearchMatch` helper methods.
#[test]
fn search_match_helpers() {
    let m = SearchMatch::new(5, 10, 15);
    assert_eq!(m.len(), 5, "match length should be end_col - start_col");
    assert!(!m.is_empty(), "match with length 5 should not be empty");

    // Edge case: zero-width match
    let empty = SearchMatch::new(0, 5, 5);
    assert_eq!(empty.len(), 0);
    assert!(empty.is_empty());
}

/// `find_prev` with `before_line = usize::MAX` must not overflow.
///
/// This edge case can occur when callers use `usize::MAX` as a high sentinel.
#[test]
fn find_prev_usize_max_does_not_overflow() {
    let mut search = TerminalSearch::new();
    search.index_scrollback_line("test content here");
    let result = search.find_prev("test", usize::MAX, 0);
    let m = result.expect("find_prev with usize::MAX should not panic and should find a match");
    assert_eq!(m.line, 0);
    assert_eq!(m.start_col, 0);
}

/// `search_before_line(query, 0)` must return empty results without panic.
///
/// This is the degenerate case: "search for matches before line 0" means
/// no lines qualify. Must terminate cleanly for both short and trigram queries.
#[test]
fn search_before_line_zero_returns_empty() {
    let mut index = SearchIndex::new();
    index.index_line(0, "test line zero");
    index.index_line(1, "test line one");

    // Trigram-length query
    let matches: Vec<_> = index.search_before_line("test", 0).collect();
    assert!(matches.is_empty(), "no lines exist before line 0");

    // Short query (bypasses trigram index)
    let matches: Vec<_> = index.search_before_line("te", 0).collect();
    assert!(
        matches.is_empty(),
        "short query also returns empty before line 0"
    );
}

/// `find_prev` at line 0, col 0 with content at line 0 returns None.
///
/// Edge case: the only match is AT the boundary position, which is not
/// "before" it (strict inequality).
#[test]
fn find_prev_boundary_at_first_line() {
    let mut search = TerminalSearch::new();
    search.index_scrollback_line("match here");

    // before_line=0, before_col=0 means "find match before position (0,0)"
    // "match" is at (0,0) which is NOT strictly before (0,0)
    assert!(search.find_prev("match", 0, 0).is_none());

    // before_line=0, before_col=1 means "find match before position (0,1)"
    // "match" at col 0 IS strictly before col 1
    let m = search
        .find_prev("match", 0, 1)
        .expect("invariant: match at col 0 is before col 1");
    assert_eq!(m.line, 0);
    assert_eq!(m.start_col, 0);
}

/// `find_next` at the very last indexed line returns None when no match follows.
#[test]
fn find_next_past_last_line() {
    let mut search = TerminalSearch::new();
    search.index_scrollback_line("match line 0");
    search.index_scrollback_line("match line 1");

    // Searching from beyond the last line
    assert!(search.find_next("match", 2, 0).is_none());
    assert!(search.find_next("match", 100, 0).is_none());

    // Searching from last match position (1, 0) skips it (strict >)
    let m = search
        .find_next("match", 0, 0)
        .expect("invariant: line 1 has 'match' after position (0,0)");
    assert_eq!(m.line, 1, "should find match at line 1");

    // No match strictly after (1, 0)
    assert!(search.find_next("match", 1, 0).is_none());
}

/// Verify `SearchIndex::with_capacity` works like `new` but pre-allocates.
#[test]
fn search_index_with_capacity() {
    let mut index = SearchIndex::with_capacity(100);

    // Should behave identically to new()
    assert!(index.is_empty());
    assert_eq!(index.len(), 0);

    index.index_line(0, "test content");
    assert_eq!(index.len(), 1);
    let results: Vec<_> = index.search("test").collect();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], 0);
}

/// Scaling test: search for a common 1-char term on a 10K-line buffer.
///
/// Verifies that ColumnMap-based search produces correct results at scale.
/// The old O(M × G) implementation would scan each line's graphemes from 0
/// per match; this test confirms correctness of the O(G + M·log G) path.
#[test]
fn search_10k_lines_common_char() {
    let mut index = SearchIndex::with_capacity(10_000);
    // Each line has ~10 occurrences of 'e' across 80 chars
    let line = "the quick brown fox jumped over the lazy fence near the hedge";
    for i in 0..10_000 {
        index.index_line(i, line);
    }

    // "e" is <3 chars, so trigram index falls back to all-lines scan.
    // This exercises the full byte_to_column → ColumnMap path on every line.
    let matches = index.search_with_positions("e");

    // Count expected: "e" appears 7 times in the line
    let expected_per_line = line.matches('e').count();
    assert_eq!(
        matches.len(),
        10_000 * expected_per_line,
        "should find {expected_per_line} matches per line × 10,000 lines"
    );

    // Verify column positions are valid (non-decreasing within each line)
    for window in matches.windows(2) {
        if window[0].line == window[1].line {
            assert!(
                window[0].start_col < window[1].start_col,
                "columns must increase within a line: {} vs {}",
                window[0].start_col,
                window[1].start_col,
            );
        }
    }
}

/// Round-8: the case-insensitive ASCII path switched from an O(L·Q) per-start
/// compare to str::find. Verify it still reports every (overlapping) match at the
/// correct columns, matching the historical semantics.
#[test]
fn case_insensitive_ascii_search_reports_overlapping_matches() {
    let mut index = SearchIndex::new();
    // Uppercase content, lowercase-folding query; overlapping "ana" in "banana".
    index.index_line(0, "BANANA");
    let matches = index
        .search_with_positions_opts("ana", false, false)
        .unwrap();
    assert_eq!(
        matches.len(),
        2,
        "overlapping 'ana' in 'banana' → positions 1 and 3"
    );
    assert_eq!((matches[0].start_col, matches[0].end_col), (1, 4));
    assert_eq!((matches[1].start_col, matches[1].end_col), (3, 6));

    // A non-matching query returns nothing (and does not scan quadratically).
    let none = index
        .search_with_positions_opts("xyz", false, false)
        .unwrap();
    assert!(none.is_empty());
}

/// Round-8: batch queries are bounded at MAX_SEARCH_MATCHES so repetitive content
/// cannot allocate one SearchMatch per occurrence unbounded; search_results_opts
/// reports the truncation via `incomplete`.
#[test]
fn batch_search_caps_matches_and_flags_incomplete() {
    let mut index = SearchIndex::with_capacity(30_000);
    // 7 'e's per line × 20,000 lines = 140,000 potential matches > MAX_SEARCH_MATCHES.
    let line = "the quick brown fox jumped over the lazy fence near the hedge";
    let per_line = line.matches('e').count();
    let lines_needed = MAX_SEARCH_MATCHES / per_line + 1_000;
    for i in 0..lines_needed {
        index.index_line(i, line);
    }

    // Case-sensitive batch path.
    let cs = index.search_with_positions("e");
    assert_eq!(
        cs.len(),
        MAX_SEARCH_MATCHES,
        "case-sensitive batch must cap at MAX_SEARCH_MATCHES"
    );

    // Case-insensitive batch path.
    let ci = index.search_with_positions_opts("e", false, false).unwrap();
    assert_eq!(
        ci.len(),
        MAX_SEARCH_MATCHES,
        "case-insensitive batch must cap at MAX_SEARCH_MATCHES"
    );

    // Deterministic cap: it must keep the LOWEST-line matches, not an arbitrary
    // hash-order subset (codex round-8 follow-up — self.lines is a hash map, so the
    // trigram-less scan sorts line numbers before capping).
    assert!(
        ci.iter().any(|m| m.line == 0),
        "capped result must include the lowest line"
    );
    let max_line = ci.iter().map(|m| m.line).max().unwrap();
    assert!(
        max_line < lines_needed - 1,
        "capped result must drop the highest lines (kept up to {max_line} of {lines_needed})"
    );

    // Reverse navigation must retain the opposite edge. Returning the oldest
    // capped batch and merely selecting its last element would make Cmd-R miss
    // the true newest ~40K occurrences.
    for (case_sensitive, label) in [(true, "case-sensitive"), (false, "case-insensitive")] {
        let reverse = index
            .search_with_positions_opts_direction(
                "e",
                case_sensitive,
                false,
                SearchDirection::Backward,
            )
            .unwrap();
        assert_eq!(reverse.len(), MAX_SEARCH_MATCHES, "{label} reverse cap");
        assert_eq!(
            reverse.last().map(|m| m.line),
            Some(lines_needed - 1),
            "{label} reverse search must retain the newest line"
        );
        assert!(
            reverse.first().is_some_and(|m| m.line > 0),
            "{label} reverse cap must drop the oldest matches"
        );
        assert!(
            reverse
                .windows(2)
                .all(|pair| (pair[0].line, pair[0].start_col) <= (pair[1].line, pair[1].start_col)),
            "direction-aware batches remain in coordinate order"
        );
    }

    // The bundled entry point flags the truncation as incomplete.
    let results = index.search_results_opts("e", false, false).unwrap();
    assert_eq!(results.matches.len(), MAX_SEARCH_MATCHES);
    assert!(
        results.incomplete,
        "a capped result set must be reported as incomplete, not exhaustive"
    );

    // Point navigation is independent of both global capped edges. These
    // anchors deliberately sit in the portion omitted by the corresponding
    // forward/backward batch.
    let terminal = TerminalSearch {
        index,
        indexed_scrollback_lines: 0,
        generation: 0,
    };
    for case_sensitive in [true, false] {
        let newest = terminal
            .find_direction_opts(
                "e",
                case_sensitive,
                false,
                DirectedFind {
                    anchor: (lines_needed - 1, 0),
                    direction: SearchDirection::Forward,
                    inclusive: true,
                    wrap: true,
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(newest.line, lines_needed - 1);

        let oldest = terminal
            .find_direction_opts(
                "e",
                case_sensitive,
                false,
                DirectedFind {
                    anchor: (0, usize::MAX),
                    direction: SearchDirection::Backward,
                    inclusive: true,
                    wrap: true,
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(oldest.line, 0);
    }
}

/// The cap applies while scanning a single adversarial line, not only between
/// lines. Forward and reverse iterators stream one overlap at a time and never
/// build an unbounded per-line match vector before `take` can stop them.
#[test]
fn single_line_overlaps_are_streamed_under_the_batch_cap() {
    let mut index = SearchIndex::new();
    index.index_line(0, &"a".repeat(MAX_SEARCH_MATCHES + 32));

    let forward = index.search_with_positions("a");
    assert_eq!(forward.len(), MAX_SEARCH_MATCHES);
    assert_eq!(forward.first().map(|found| found.start_col), Some(0));
    assert_eq!(
        forward.last().map(|found| found.start_col),
        Some(MAX_SEARCH_MATCHES - 1)
    );

    let backward = index
        .search_with_positions_opts_direction("a", true, false, SearchDirection::Backward)
        .unwrap();
    assert_eq!(backward.len(), MAX_SEARCH_MATCHES);
    assert_eq!(backward.first().map(|found| found.start_col), Some(32));
    assert_eq!(
        backward.last().map(|found| found.start_col),
        Some(MAX_SEARCH_MATCHES + 31)
    );
}

/// Round-8 codex follow-up: the regex batch path also caps, and must iterate
/// lines in ascending order so the cap keeps the lowest-line matches
/// deterministically (self.lines is a hash map).
#[cfg(feature = "regex")]
#[test]
fn regex_batch_search_caps_matches_deterministically() {
    let mut index = SearchIndex::with_capacity(30_000);
    // 7 'e' matches per line; index enough lines to exceed the cap without hitting
    // the line-eviction watermark (mirrors the case-insensitive cap test).
    let line = "the quick brown fox jumped over the lazy fence near the hedge";
    let per_line = line.matches('e').count();
    let lines_needed = MAX_SEARCH_MATCHES / per_line + 1_000;
    for i in 0..lines_needed {
        index.index_line(i, line);
    }
    let results = index.search_results_opts("e", true, true).unwrap();
    assert_eq!(
        results.matches.len(),
        MAX_SEARCH_MATCHES,
        "regex batch must cap"
    );
    assert!(
        results.incomplete,
        "capped regex result must be flagged incomplete"
    );
    assert!(
        results.matches.iter().any(|m| m.line == 0),
        "capped regex result must keep the lowest line"
    );
    let max_line = results.matches.iter().map(|m| m.line).max().unwrap();
    assert!(
        max_line < lines_needed - 1,
        "capped regex result must drop the highest lines (kept up to {max_line} of {lines_needed})"
    );

    let reverse = index
        .search_results_opts_direction("e", true, true, SearchDirection::Backward)
        .unwrap();
    assert_eq!(reverse.matches.len(), MAX_SEARCH_MATCHES);
    assert_eq!(
        reverse.matches.last().map(|m| m.line),
        Some(lines_needed - 1),
        "reverse regex search must retain the newest line"
    );
    assert!(reverse.matches.first().is_some_and(|m| m.line > 0));
    assert_eq!(
        index
            .find_regex_from(
                "e",
                true,
                lines_needed - 1,
                0,
                SearchDirection::Forward,
                true,
            )
            .unwrap()
            .map(|m| m.line),
        Some(lines_needed - 1),
    );
    assert_eq!(
        index
            .find_regex_from("e", true, 0, usize::MAX, SearchDirection::Backward, true,)
            .unwrap()
            .map(|m| m.line),
        Some(0),
    );
}

#[test]
fn bloom_rebuild_tracks_trigram_volume_for_bulk_scrollback() {
    let mut index = SearchIndex::new();
    let query = "needle_2975";
    let cols = 160;

    for line in 0..512 {
        let marker = if line % 20 == 0 { query } else { "filler" };
        let mut content = format!("{line:05} {marker} ");
        if content.len() < cols {
            content.push_str(&"x".repeat(cols - content.len()));
        } else if content.len() > cols {
            content.truncate(cols);
        }
        index.index_line(line, &content);
    }

    assert!(
        !index.bloom_is_saturated(),
        "bulk scrollback indexing should leave the rebuilt bloom filter usable"
    );

    let results: Vec<_> = index.search(query).collect();
    assert!(
        !results.is_empty(),
        "bulk scrollback corpus should remain searchable after bloom rebuild"
    );
}

// Regression: regex patterns that produce zero-length matches (^, \b, x*)
// must be filtered to prevent `SearchMatch` with `start_col == end_col`.
// Such matches cause `saturating_sub(1)` in `convert_search_match` to produce
// backwards (end < start) or incorrect (fake 1-char) Match objects.
// Part of algorithm audit for #5455 regex find bar.
#[cfg(feature = "regex")]
#[test]
fn regex_search_filters_zero_length_matches() {
    let mut index = SearchIndex::new();
    index.index_line(0, "hello world");
    index.index_line(1, "foo bar");

    // Pattern `^` matches start of each line with zero length.
    // All matches are zero-length anchors, so filtering should yield empty results.
    let results = index
        .search_with_positions_opts("^", true, true)
        .expect("valid regex");
    assert!(
        results.is_empty(),
        "pure anchor pattern `^` should produce 0 results after zero-length filtering, got {}",
        results.len(),
    );

    // Pattern `\b` matches word boundaries with zero length.
    // All matches are zero-length boundaries, so filtering should yield empty results.
    let results = index
        .search_with_positions_opts(r"\b", true, true)
        .expect("valid regex");
    assert!(
        results.is_empty(),
        "pure boundary pattern `\\b` should produce 0 results after zero-length filtering, got {}",
        results.len(),
    );

    // `x*` on input without `x`: all matches are zero-length → empty results.
    let results = index
        .search_with_positions_opts("x*", true, true)
        .expect("valid regex");
    assert!(
        results.is_empty(),
        "`x*` on non-x input should produce 0 results after zero-length filtering, got {}",
        results.len(),
    );
}

// Verify that regex zero-length filtering preserves valid non-zero matches.
// `x*` on input containing `x` should return the `x` match and drop zero-length positions.
#[cfg(feature = "regex")]
#[test]
fn regex_search_preserves_nonzero_matches_alongside_zero_length() {
    let mut index = SearchIndex::new();
    index.index_line(0, "axb");
    index.index_line(1, "no match here");

    let results = index
        .search_with_positions_opts("x*", true, true)
        .expect("valid regex");
    assert!(
        !results.is_empty(),
        "`x*` on input containing `x` should return at least one non-zero match",
    );
    for m in &results {
        assert!(
            !m.is_empty(),
            "all returned matches must be non-empty: line {} col {} start==end=={}",
            m.line,
            m.start_col,
            m.start_col,
        );
    }
}

/// Performance proof: `search_before_line` returns candidates in strictly
/// descending line order. This validates that the O(n) `.reverse()` on the
/// already-sorted bitmap range output produces the correct reverse ordering
/// (previously used O(n log n) `.sort_unstable_by(descending)` redundantly).
#[test]
fn search_before_line_descending_order_at_scale() {
    let mut index = SearchIndex::new();
    // Index 500 lines with the same trigram-matchable content.
    for i in 0..500 {
        index.index_line(i, &format!("needle at line {i}"));
    }

    let matches: Vec<_> = index.search_before_line("needle", 500).collect();
    assert_eq!(matches.len(), 500);

    // Verify strict descending line order.
    for window in matches.windows(2) {
        assert!(
            window[0].line > window[1].line,
            "search_before_line must return descending line order: \
             line {} should be > line {}",
            window[0].line,
            window[1].line,
        );
    }
    // First match is the highest line (499), last is line 0.
    assert_eq!(matches[0].line, 499);
    assert_eq!(matches[499].line, 0);
}

/// Performance proof: `search_before_line` with a large prefix region and a
/// small suffix region only materializes candidates in the queried range, not
/// the entire index. Verifies that the bitmap range query `..before_line`
/// correctly bounds the candidate set.
#[test]
fn search_before_line_range_bounded() {
    let mut index = SearchIndex::new();
    // 1000 lines total, all matching "needle".
    for i in 0..1000 {
        index.index_line(i, &format!("needle line {i}"));
    }

    // Search before line 10 — should return exactly 10 matches (lines 0..10).
    let matches: Vec<_> = index.search_before_line("needle", 10).collect();
    assert_eq!(
        matches.len(),
        10,
        "should only find 10 matches before line 10"
    );
    assert_eq!(
        matches[0].line, 9,
        "first match should be line 9 (descending)"
    );
    assert_eq!(matches[9].line, 0, "last match should be line 0");
}

/// Case-insensitive search finds matches regardless of letter case (ASCII fast path).
#[test]
fn case_insensitive_ascii_search() {
    let mut index = SearchIndex::new();
    index.index_line(0, "Hello World");
    index.index_line(1, "HELLO WORLD");
    index.index_line(2, "hello world");
    index.index_line(3, "no match here");

    let matches = index
        .search_with_positions_opts("hello", false, false)
        .unwrap();
    assert_eq!(matches.len(), 3, "should find 3 case-insensitive matches");
    let lines: Vec<_> = matches.iter().map(|m| m.line).collect();
    assert!(lines.contains(&0));
    assert!(lines.contains(&1));
    assert!(lines.contains(&2));
    // All matches start at column 0
    for m in &matches {
        assert_eq!(m.start_col, 0);
        assert_eq!(m.end_col, 5);
    }
}

/// Case-insensitive search with non-ASCII text uses the buffer-reuse path.
#[test]
fn case_insensitive_non_ascii_search() {
    let mut index = SearchIndex::new();
    index.index_line(0, "ΔΕΛΤΑ data");
    index.index_line(1, "δελτα data");
    index.index_line(2, "no greek here");

    let matches = index
        .search_with_positions_opts("δελτα", false, false)
        .unwrap();
    assert_eq!(
        matches.len(),
        2,
        "should find Greek matches case-insensitively"
    );
    let lines: Vec<_> = matches.iter().map(|m| m.line).collect();
    assert!(lines.contains(&0));
    assert!(lines.contains(&1));
}

/// Regression: case-insensitive search must match across all three forms of
/// Greek sigma (capital Σ, medial σ, word-final ς).
///
/// `str::to_lowercase` applies the contextual Final_Sigma rule (a word-final
/// Σ lowercases to ς), while the per-line scan buffer and `LowerByteMap` use
/// the non-contextual `char::to_lowercase` (always σ). Before the fix the
/// trigram index/query (built with `str::to_lowercase`) and the scan buffer
/// disagreed on a word-final Σ, so indexing the Greek word for "street"
/// (ΟΔΟΣ) and searching for it case-insensitively returned 0 matches for
/// every form of the query. The shared lower+sigma-fold routine collapses
/// Σ/σ/ς to a single canonical form so all forms match.
#[test]
fn case_insensitive_greek_final_sigma_unified() {
    let mut index = SearchIndex::new();
    // ΟΔΟΣ — Greek for "street", word-final capital Σ (U+03A3).
    index.index_line(0, "\u{039F}\u{0394}\u{039F}\u{03A3}");
    index.index_line(1, "no greek here");

    // All three sigma forms of the same word must match case-insensitively:
    //  - capital form ΟΔΟΣ (final Σ)
    //  - lowercase medial sigma οδοσ (U+03C3)
    //  - lowercase final sigma οδος (U+03C2)
    for (label, query) in [
        ("capital ΟΔΟΣ", "\u{039F}\u{0394}\u{039F}\u{03A3}"),
        ("medial sigma οδοσ", "\u{03BF}\u{03B4}\u{03BF}\u{03C3}"),
        ("final sigma οδος", "\u{03BF}\u{03B4}\u{03BF}\u{03C2}"),
    ] {
        let matches = index
            .search_with_positions_opts(query, false, false)
            .unwrap();
        let lines: Vec<usize> = matches.iter().map(|m| m.line).collect();
        assert!(
            lines.contains(&0),
            "case-insensitive search for {label} must match the indexed ΟΔΟΣ line, got {lines:?}"
        );
        assert!(
            !lines.contains(&1),
            "{label} must not match the non-Greek line, got {lines:?}"
        );
    }
}

/// Case-insensitive search finds multiple overlapping matches on the same line.
#[test]
fn case_insensitive_overlapping_matches() {
    let mut index = SearchIndex::new();
    index.index_line(0, "aAaAa");

    let matches = index
        .search_with_positions_opts("aa", false, false)
        .unwrap();
    // "aAaAa" lowered = "aaaaa", matches at offsets 0,1,2,3
    assert_eq!(matches.len(), 4, "should find 4 overlapping matches");
}

/// A repeated first-byte prefix used to make the candidate-at-every-byte memchr
/// verifier quadratic. Three-byte-and-longer literals now use `str::find`'s
/// linear-time search over one reusable folded buffer instead.
#[test]
fn case_insensitive_repeated_prefix_uses_linear_matcher_path() {
    let mut index = SearchIndex::new();
    let adversarial = format!("{}b", "a".repeat(128)).repeat(1_000);
    index.index_line(0, &adversarial);
    let query = format!("{}b", "a".repeat(512));

    let _ = SearchIndex::take_case_insensitive_lowered_lines();
    let matches = index
        .search_with_positions_opts(&query, false, false)
        .unwrap();
    assert!(matches.is_empty());
    assert_eq!(
        SearchIndex::take_case_insensitive_lowered_lines(),
        1,
        "the trigram-positive repeated-prefix miss is scanned once by the linear matcher"
    );

    // Add one actual match. Both trigram candidates are each buffered once,
    // never checked once per possible first-byte position.
    index.index_line(1, &query);
    let _ = SearchIndex::take_case_insensitive_lowered_lines();
    let matches = index
        .search_with_positions_opts(&query, false, false)
        .unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].line, 1);
    assert_eq!(SearchIndex::take_case_insensitive_lowered_lines(), 2);
}

/// Point navigation borrows and range-bounds posting lists. Even a ubiquitous
/// one-trigram token never clones/intersects the full tree before yielding the
/// neighboring match, and repeated trigrams collapse to one membership source.
#[test]
fn case_sensitive_point_navigation_builds_no_owned_intersection() {
    let mut search = TerminalSearch::with_capacity(10_000);
    for _ in 0..10_000 {
        search.index_scrollback_line("hit aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    }

    let _ = SearchIndex::take_owned_intersection_builds();
    let _ = SearchIndex::take_search_from_line_candidates();
    let next = search
        .find_direction_opts(
            "hit",
            true,
            false,
            DirectedFind {
                anchor: (9_998, 0),
                direction: SearchDirection::Forward,
                inclusive: false,
                wrap: false,
            },
        )
        .unwrap()
        .expect("last line match");
    assert_eq!((next.line, next.start_col), (9_999, 0));
    assert_eq!(SearchIndex::take_owned_intersection_builds(), 0);
    assert_eq!(SearchIndex::take_search_from_line_candidates(), 2);

    let repeated = "a".repeat(32);
    let _ = SearchIndex::take_last_unique_posting_lists();
    let _ = search
        .find_direction_opts(
            &repeated,
            true,
            false,
            DirectedFind {
                anchor: (9_999, 0),
                direction: SearchDirection::Forward,
                inclusive: true,
                wrap: false,
            },
        )
        .unwrap();
    assert_eq!(
        SearchIndex::take_last_unique_posting_lists(),
        1,
        "thirty identical overlapping trigrams share one posting filter"
    );
    assert_eq!(SearchIndex::take_owned_intersection_builds(), 0);
}

/// Deterministic 100K-line regression for the default short-query path.
///
/// The work counters pin three performance properties without relying on wall
/// time: candidates are consumed lazily in line order, ASCII lines never enter
/// the lowercase-copy path, and navigation ignores the 99,998-line prefix.
#[test]
fn case_insensitive_100k_ascii_short_query_work_is_bounded() {
    let mut index = SearchIndex::with_capacity(100_000);
    for line in 0..100_000 {
        index.index_line(line, "A");
    }

    let _ = SearchIndex::take_case_insensitive_candidate_visits();
    let _ = SearchIndex::take_case_insensitive_lowered_lines();
    let common = index.search_with_positions_opts("a", false, false).unwrap();
    assert_eq!(common.len(), 100_000);
    assert_eq!(common.first().map(|m| m.line), Some(0));
    assert_eq!(common.last().map(|m| m.line), Some(99_999));
    assert_eq!(
        SearchIndex::take_case_insensitive_candidate_visits(),
        100_000,
        "one common-char match per line requires exactly one visit per retained line"
    );
    assert_eq!(
        SearchIndex::take_case_insensitive_lowered_lines(),
        0,
        "ASCII lines must not be copied into a lowercase buffer"
    );

    let miss = index
        .search_with_positions_opts("zz", false, false)
        .unwrap();
    assert!(miss.is_empty());
    assert_eq!(
        SearchIndex::take_case_insensitive_candidate_visits(),
        100_000,
        "a two-byte miss has no postings, so it performs one allocation-free scan per line"
    );
    assert_eq!(SearchIndex::take_case_insensitive_lowered_lines(), 0);

    let next = index
        .find_next_case_insensitive("a", 99_998, 0)
        .expect("the last line is the next strict match");
    assert_eq!((next.line, next.start_col), (99_999, 0));
    assert_eq!(
        SearchIndex::take_case_insensitive_candidate_visits(),
        2,
        "navigation visits the boundary line and first suffix line, not the prefix"
    );

    let prev = index
        .find_prev_case_insensitive("a", 99_999, 0)
        .expect("the preceding line is the previous strict match");
    assert_eq!((prev.line, prev.start_col), (99_998, 0));
    assert_eq!(
        SearchIndex::take_case_insensitive_candidate_visits(),
        2,
        "reverse navigation visits the boundary line and first prefix line only"
    );
}

/// A sparse high-base index must not make a short query walk the empty numeric
/// prefix merely to preserve deterministic ordering.
#[test]
fn case_insensitive_short_query_skips_sparse_empty_prefix() {
    let mut index = SearchIndex::new();
    index.index_line(1_000_000, "Needle A");
    let _ = SearchIndex::take_case_insensitive_candidate_visits();

    let matches = index.search_with_positions_opts("a", false, false).unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].line, 1_000_000);
    assert_eq!(
        SearchIndex::take_case_insensitive_candidate_visits(),
        1,
        "the lazy range starts at the first cached line"
    );
}

#[test]
fn case_sensitive_reverse_short_query_skips_sparse_empty_prefix() {
    let mut index = SearchIndex::new();
    index.index_line(1_000_000, "A");
    let _ = SearchIndex::take_search_from_line_candidates();

    let matches = index
        .search_with_positions_opts_direction("A", true, false, SearchDirection::Backward)
        .unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].line, 1_000_000);
    assert_eq!(
        SearchIndex::take_search_from_line_candidates(),
        1,
        "reverse short search must visit the retained row, not one million empty row IDs"
    );
}

/// Trigram-aware case-insensitive navigation visits only posting-list
/// candidates and terminates at the first real match.
#[test]
fn case_insensitive_navigation_visits_only_trigram_candidates() {
    let mut index = SearchIndex::with_capacity(10_000);
    for line in 0..10_000 {
        index.index_line(line, "xxxxxxxx");
    }
    index.index_line(9_999, "the NEEDLE is here");
    let _ = SearchIndex::take_case_insensitive_candidate_visits();

    let found = index
        .find_next_case_insensitive("needle", 9_000, 0)
        .expect("mixed-case candidate should match");
    assert_eq!(found.line, 9_999);
    assert_eq!(
        SearchIndex::take_case_insensitive_candidate_visits(),
        1,
        "non-candidate lines between the cursor and match are never visited"
    );
}

#[test]
fn terminal_search_option_navigation_is_ordered_and_case_insensitive() {
    let mut search = TerminalSearch::new();
    search.index_scrollback_line("Alpha alpha ALPHA");
    search.index_scrollback_line("prefix ALPHA suffix");

    let next = search
        .find_next_opts("alpha", 0, 0, false, false)
        .unwrap()
        .unwrap();
    assert_eq!((next.line, next.start_col, next.end_col), (0, 6, 11));

    let prev = search
        .find_prev_opts("alpha", 0, usize::MAX, false, false)
        .unwrap()
        .unwrap();
    assert_eq!((prev.line, prev.start_col, prev.end_col), (0, 12, 17));

    let next_line = search
        .find_next_opts("alpha", 0, 12, false, false)
        .unwrap()
        .unwrap();
    assert_eq!((next_line.line, next_line.start_col), (1, 7));

    let exact = search
        .find_next_opts("ALPHA", 0, 0, true, false)
        .unwrap()
        .unwrap();
    assert_eq!((exact.line, exact.start_col), (0, 12));
}

/// Manual release-mode timing companion for the deterministic scaling tests.
///
/// Run with:
/// `cargo test --release -p aterm-search measure_case_insensitive_100k_ascii -- --ignored --nocapture`
///
/// There is deliberately no wall-clock assertion: CI load makes those brittle.
#[test]
#[ignore = "manual release-mode performance measurement"]
fn measure_case_insensitive_100k_ascii() {
    use std::time::Instant;

    let mut index = SearchIndex::with_capacity(100_000);
    for line in 0..100_000 {
        index.index_line(line, "needle A");
    }

    for query in ["a", "z"] {
        let started = Instant::now();
        let matches = index
            .search_with_positions_opts(query, false, false)
            .unwrap();
        eprintln!(
            "100k ASCII case-insensitive query {query:?}: {:?}, {} matches",
            started.elapsed(),
            matches.len()
        );
    }

    let search = TerminalSearch {
        index,
        indexed_scrollback_lines: 0,
        generation: 0,
    };
    for (case_sensitive, label) in [(true, "case-sensitive"), (false, "case-insensitive")] {
        let iterations = 10_000;
        let started = Instant::now();
        for _ in 0..iterations {
            let found = search
                .find_direction_opts(
                    "needle",
                    case_sensitive,
                    false,
                    DirectedFind {
                        anchor: (99_998, 0),
                        direction: SearchDirection::Forward,
                        inclusive: false,
                        wrap: false,
                    },
                )
                .unwrap();
            std::hint::black_box(found);
        }
        eprintln!(
            "100k ASCII {label} adjacent point navigation: {:?}/op",
            started.elapsed() / iterations
        );
    }
}

/// Regex pattern exceeding `MAX_REGEX_PATTERN_LEN` is rejected before compilation.
///
/// Prevents ReDoS via compilation by bounding pattern length at the index
/// layer, matching the streaming engine's existing `max_pattern_len` guard.
/// Part of #7203.
#[cfg(feature = "regex")]
#[test]
fn regex_pattern_too_long_rejected() {
    let mut index = SearchIndex::new();
    index.index_line(0, "test content");

    // Exactly at the limit (1024 bytes) should succeed
    let at_limit = "a".repeat(1024);
    let result = index.search_with_positions_opts(&at_limit, true, true);
    assert!(
        result.is_ok(),
        "pattern at exactly 1024 bytes should be accepted"
    );

    // One byte over the limit should be rejected
    let over_limit = "a".repeat(1025);
    let result = index.search_with_positions_opts(&over_limit, true, true);
    assert_eq!(
        result,
        Err(SearchOptionsError::PatternTooLong),
        "pattern exceeding 1024 bytes should return PatternTooLong"
    );
}

/// Regex compilation with bounded `size_limit` rejects patterns that would
/// produce oversized NFA/DFA automata. This verifies the `RegexBuilder`
/// size limits are effective.
#[cfg(feature = "regex")]
#[test]
fn regex_compilation_size_limit_enforced() {
    let mut index = SearchIndex::new();
    index.index_line(0, "test content");

    // A pattern with many large alternations can blow up NFA size even within
    // the 1024-byte length limit. This pattern generates a large automaton.
    // Use 100 unique 8-char alternations (~999 bytes with separators).
    let alternatives: Vec<String> = (0..100).map(|i| format!("alt{i:05}x")).collect();
    let pattern = alternatives.join("|");
    assert!(
        pattern.len() <= 1024,
        "test pattern should fit in length limit"
    );

    // This should either succeed or fail with InvalidRegex (due to size_limit),
    // but must never panic or hang.
    let result = index.search_with_positions_opts(&pattern, true, true);
    assert!(
        matches!(result, Ok(_) | Err(SearchOptionsError::InvalidRegex(_))),
        "large-automaton pattern should succeed or return InvalidRegex, got {result:?}"
    );
}

// =============================================================================
// Generation counter tests (#7271)
// =============================================================================

/// Generation counter starts at 0 and bumps on each index mutation.
#[test]
fn generation_counter_starts_at_zero() {
    let search = TerminalSearch::new();
    assert_eq!(search.generation(), 0);
}

/// Each `index_scrollback_line` bumps the generation.
#[test]
fn generation_bumps_on_index_scrollback_line() {
    let mut search = TerminalSearch::new();
    assert_eq!(search.generation(), 0);

    search.index_scrollback_line("line 0");
    assert_eq!(search.generation(), 1);

    search.index_scrollback_line("line 1");
    assert_eq!(search.generation(), 2);
}

/// `index_scrollback_lines` bumps once per line (batch method).
#[test]
fn generation_bumps_on_index_scrollback_lines_batch() {
    let mut search = TerminalSearch::new();
    search.index_scrollback_lines(vec!["a", "b", "c"]);
    assert_eq!(
        search.generation(),
        3,
        "batch of 3 lines should bump generation 3 times"
    );
}

/// `index_visible_content` bumps the generation once per call.
#[test]
fn generation_bumps_on_index_visible_content() {
    let mut search = TerminalSearch::new();
    search.index_visible_content(0, vec!["line A", "line B"]);
    assert_eq!(search.generation(), 1);

    search.index_visible_content(2, vec!["line C"]);
    assert_eq!(search.generation(), 2);
}

/// `clear` bumps the generation.
#[test]
fn generation_bumps_on_clear() {
    let mut search = TerminalSearch::new();
    search.index_scrollback_line("content");
    let gen_before_clear = search.generation();

    search.clear();
    assert_eq!(
        search.generation(),
        gen_before_clear + 1,
        "clear should bump generation"
    );
}

/// `invalidate` bumps the generation without modifying index content.
#[test]
fn generation_bumps_on_invalidate() {
    let mut search = TerminalSearch::new();
    search.index_scrollback_line("content");
    let gen_after_index = search.generation();

    search.invalidate();
    assert_eq!(
        search.generation(),
        gen_after_index + 1,
        "invalidate should bump generation"
    );

    // Content is still searchable after invalidate (it only signals staleness)
    let matches = search.search("content");
    assert_eq!(
        matches.len(),
        1,
        "invalidate does not remove content from index"
    );
}

/// Snapshot-and-compare pattern: detect stale results.
#[test]
fn generation_detects_stale_results() {
    let mut search = TerminalSearch::new();
    search.index_scrollback_line("findme here");
    search.index_scrollback_line("findme there");

    // Snapshot generation before searching
    let gen_snapshot = search.generation();
    let results = search.search("findme");
    assert_eq!(results.len(), 2);

    // Mutate the index (simulating grid change)
    search.index_scrollback_line("new line with findme");

    // Generation changed => cached results are stale
    assert_ne!(
        search.generation(),
        gen_snapshot,
        "generation should differ after mutation"
    );
}

/// Read-only operations (search, find_next, find_prev, might_contain)
/// do NOT bump the generation.
#[test]
fn generation_stable_across_read_operations() {
    let mut search = TerminalSearch::new();
    search.index_scrollback_line("needle in haystack");
    let gen_snapshot = search.generation();

    let _ = search.search("needle");
    assert_eq!(
        search.generation(),
        gen_snapshot,
        "search should not bump generation"
    );

    let _ = search.find_next("needle", 0, 0);
    assert_eq!(
        search.generation(),
        gen_snapshot,
        "find_next should not bump generation"
    );

    let _ = search.find_prev("needle", 1, 0);
    assert_eq!(
        search.generation(),
        gen_snapshot,
        "find_prev should not bump generation"
    );

    let _ = search.might_contain("needle");
    assert_eq!(
        search.generation(),
        gen_snapshot,
        "might_contain should not bump generation"
    );
}

/// Eviction with sparse (widely-spaced) line numbers is O(n log n) in cached
/// entries, not O(gap_size). Indexes 10 lines at positions 1M, 2M, ..., 10M,
/// triggers eviction, and verifies the oldest lines are evicted while the
/// newest survive (#7246).
#[test]
fn eviction_with_sparse_line_numbers() {
    let mut index = SearchIndex::new();
    // Allow only 8 cached lines — inserting 10 will trigger eviction.
    index.set_max_cached_lines(8);

    // Index 10 lines at positions 1_000_000, 2_000_000, ..., 10_000_000.
    for i in 1..=10 {
        let line_num = i * 1_000_000;
        index.index_line(line_num, &format!("sparse line {i}"));
    }

    // Eviction should have removed the oldest entries to stay at/below capacity.
    // target = 8 * 3/4 = 6, so each eviction pass removes down to 6 lines.
    // After all insertions the cache should have at most 8 lines.
    assert!(
        index.lines.len() <= 8,
        "cache should be at most 8 lines after eviction, got {}",
        index.lines.len(),
    );

    // The newest lines (highest line numbers) must survive eviction.
    assert!(
        index.get_line(10_000_000).is_some(),
        "newest line (10M) should survive eviction"
    );
    assert!(
        index.get_line(9_000_000).is_some(),
        "line 9M should survive eviction"
    );
    assert!(
        index.get_line(8_000_000).is_some(),
        "line 8M should survive eviction"
    );

    // The oldest line (1M) should have been evicted.
    assert!(
        index.get_line(1_000_000).is_none(),
        "oldest line (1M) should be evicted"
    );

    // Trigram index should still work for surviving lines.
    let matches = index.search_with_positions("sparse");
    assert!(
        !matches.is_empty(),
        "surviving lines should still be searchable"
    );
    // All matches should be from surviving (non-evicted) lines.
    for m in &matches {
        assert!(
            index.get_line(m.line).is_some(),
            "match at line {} should reference a cached line",
            m.line,
        );
    }
}

/// DL-2: eviction sets the incomplete-results signal and the retained-line
/// watermark, and the cap is configurable via the public API.
///
/// Indexing past a small configured cap must:
/// 1. flip `results_may_be_incomplete()` from false to true,
/// 2. advance `lowest_retained_line()` above 0,
/// 3. keep `len()` growing to the highest indexed line (caller sees no shrink),
/// 4. surface the same signal through `search_results_opts`.
#[test]
fn eviction_sets_incomplete_signal_and_watermark() {
    // Cap configurable through the constructor (not the old hard-coded const).
    let mut index = SearchIndex::with_max_cached_lines(8);
    assert_eq!(index.max_cached_lines(), 8, "cap must be configurable");

    // Before any eviction: complete results, watermark at the origin.
    assert!(
        !index.results_may_be_incomplete(),
        "fresh index must report complete results"
    );
    assert_eq!(index.lowest_retained_line(), 0);

    // Index 40 contiguous lines, far past the cap of 8 → triggers eviction.
    for i in 0..40 {
        index.index_line(i, &format!("alpha bravo line {i}"));
    }

    // (1) The incomplete signal must now be set.
    assert!(
        index.results_may_be_incomplete(),
        "indexing past the cap must mark results incomplete"
    );

    // (2) The retained-line watermark advanced past the origin.
    assert!(
        index.lowest_retained_line() > 0,
        "lowest_retained_line must advance after eviction, got {}",
        index.lowest_retained_line()
    );

    // (3) len() still reflects the highest indexed line — it does NOT shrink,
    //     which is exactly why a silent eviction was dangerous before DL-2.
    assert_eq!(
        index.len(),
        40,
        "line_count keeps growing even though oldest lines were evicted"
    );

    // The oldest line is gone; the newest survives.
    assert!(
        index.get_line(0).is_none(),
        "oldest line must have been evicted"
    );
    assert!(
        index.get_line(39).is_some(),
        "newest line must survive eviction"
    );

    // (4) search_results_opts propagates the signal + watermark.
    let results = index.search_results_opts("alpha", true, false).unwrap();
    assert!(
        results.incomplete,
        "SearchResults must report incomplete after eviction"
    );
    assert_eq!(
        results.lowest_retained_line,
        index.lowest_retained_line(),
        "SearchResults watermark must match the index watermark"
    );
    // Every returned match references a still-retained (non-evicted) line.
    for m in &results.matches {
        assert!(
            m.line >= results.lowest_retained_line,
            "match at line {} must be at/above watermark {}",
            m.line,
            results.lowest_retained_line
        );
        assert!(index.get_line(m.line).is_some());
    }
}

/// DL-2: the cap is configurable after construction via `set_max_cached_lines`,
/// a 0 cap is clamped to 1, and `clear()` resets the incomplete signal.
#[test]
fn cap_is_configurable_and_clear_resets_signal() {
    let mut index = SearchIndex::new();
    assert_eq!(
        index.max_cached_lines(),
        DEFAULT_MAX_CACHED_LINES,
        "default cap is the documented constant"
    );

    // Reconfigure the cap at runtime; 0 must clamp to 1.
    index.set_max_cached_lines(0);
    assert_eq!(index.max_cached_lines(), 1, "0 cap clamps to 1");

    index.set_max_cached_lines(4);
    assert_eq!(index.max_cached_lines(), 4);

    // Exceed the cap → eviction → incomplete.
    for i in 0..20 {
        index.index_line(i, &format!("charlie delta {i}"));
    }
    assert!(index.results_may_be_incomplete());
    assert!(index.lowest_retained_line() > 0);

    // clear() must reset the eviction signal and watermark.
    index.clear();
    assert!(
        !index.results_may_be_incomplete(),
        "clear must reset the incomplete signal"
    );
    assert_eq!(
        index.lowest_retained_line(),
        0,
        "clear must reset the retained-line watermark"
    );
    assert_eq!(index.len(), 0, "clear empties the index");
}

/// DL-2: `TerminalSearch` forwards the eviction signal so the future
/// `cmd_search` can read it without reaching into `SearchIndex`.
#[test]
fn terminal_search_surfaces_eviction_signal() {
    let mut search = TerminalSearch::with_capacity_and_max(16, 8);

    for i in 0..40 {
        search.index_scrollback_line(&format!("echo foxtrot line {i}"));
    }

    assert!(
        search.results_may_be_incomplete(),
        "TerminalSearch must surface the incomplete signal after eviction"
    );
    assert!(
        search.lowest_retained_line() > 0,
        "TerminalSearch must surface the retained-line watermark"
    );

    let results = search.search_results_opts("foxtrot", true, false).unwrap();
    assert!(results.incomplete);
    assert_eq!(results.lowest_retained_line, search.lowest_retained_line());
    assert!(!results.is_empty(), "surviving lines remain searchable");
}

/// Case-insensitive search uses the trigram index instead of full scan (#7273).
///
/// Verifies that:
/// 1. Lowercased trigrams are indexed at index time
/// 2. Case-insensitive queries use the trigram index for candidate filtering
/// 3. Non-matching trigrams are correctly rejected (not a full scan)
#[test]
fn case_insensitive_search_uses_trigram_index() {
    let mut index = SearchIndex::new();
    // Index 100 lines, only 3 contain "Hello" (mixed case)
    for i in 0..100 {
        if i == 10 || i == 50 || i == 90 {
            index.index_line(i, &format!("Hello World line {i}"));
        } else {
            index.index_line(i, &format!("zzzzz nothing line {i}"));
        }
    }

    // Case-insensitive search for "hello" should find exactly 3 matches
    let matches = index
        .search_with_positions_opts("hello", false, false)
        .unwrap();
    assert_eq!(
        matches.len(),
        3,
        "case-insensitive 'hello' should find 3 matches"
    );
    let lines: Vec<_> = matches.iter().map(|m| m.line).collect();
    assert!(lines.contains(&10));
    assert!(lines.contains(&50));
    assert!(lines.contains(&90));

    // Verify the trigram index contains lowercased trigrams by checking
    // that the bloom filter accepts the lowercased query
    assert!(
        index.might_contain("hel"),
        "bloom filter should accept lowercased trigram 'hel' from 'Hello'"
    );

    // A query with no matching trigrams should return empty even for
    // case-insensitive search, proving the trigram filter is active
    let no_match = index
        .search_with_positions_opts("xyzqj", false, false)
        .unwrap();
    assert!(
        no_match.is_empty(),
        "trigram filter should reject non-matching case-insensitive query"
    );
}

// Regression for the lowercased-trigram fast path: gating the lowered pass
// behind `needs_lower` must keep insert/remove/rebuild symmetric. An
// all-lowercase line skips the lowered pass entirely, while a mixed-case line
// runs it; overwriting one with the other (which drives `remove_trigrams` on
// the old text) must not leak or double-delete shared trigrams.
#[test]
fn lowercase_fast_path_keeps_insert_remove_symmetric() {
    let mut index = SearchIndex::new();

    // All-lowercase ASCII line: lowering is the identity, so the lowered pass
    // is skipped. Case-insensitive search must still find it.
    index.index_line(0, "hello world");
    // Mixed-case line: the lowered pass runs.
    index.index_line(1, "Hello World");

    let ci = index
        .search_with_positions_opts("hello", false, false)
        .unwrap();
    let lines: Vec<usize> = ci.iter().map(|m| m.line).collect();
    assert!(lines.contains(&0), "all-lowercase line must be searchable");
    assert!(lines.contains(&1), "mixed-case line must be searchable");

    // Overwrite the mixed-case line with all-lowercase text. This removes the
    // old mixed-case trigrams (including the lowered pass) and inserts only the
    // original-case pass. The uppercase substring must no longer be present and
    // the new lowercase content must be found case-sensitively.
    index.index_line(1, "hello world");
    let cs_upper = index.search_with_positions("Hello");
    assert!(
        cs_upper.is_empty(),
        "old mixed-case content must be fully removed after overwrite"
    );
    let cs_lower = index.search_with_positions("hello world");
    let cs_lines: Vec<usize> = cs_lower.iter().map(|m| m.line).collect();
    assert!(cs_lines.contains(&0));
    assert!(cs_lines.contains(&1));

    // Overwrite an all-lowercase line with mixed-case text: the reverse
    // transition. Case-insensitive search must still match the new content.
    index.index_line(0, "Foo Bar Baz");
    let ci2 = index
        .search_with_positions_opts("foo bar", false, false)
        .unwrap();
    assert!(
        ci2.iter().any(|m| m.line == 0),
        "mixed-case overwrite of a lowercase line must be searchable"
    );
}

/// Regression (round 11): a query that resolves to ZERO display columns
/// (`start_col == end_col`) must not be emitted as a match. Such a match spans
/// no visible columns and would `saturating_sub(1)` into an inverted highlight
/// span in downstream consumers (`convert_search_match`).
///
/// A combining mark (U+0301, width 0) that shares its grapheme with the base
/// character is the canonical trigger: `text.find` locates a nonzero-byte
/// occurrence, but both endpoints map to the same display column. This exercises
/// every push site: `search_with_positions` (case-sensitive literal), the
/// non-ASCII case-insensitive path, the forward/reverse match iterators, and —
/// when built with `regex` — the regex path's post-resolution guard.
#[test]
fn zero_display_width_matches_are_skipped() {
    let mut index = SearchIndex::new();
    // "c" carries a combining acute accent; the accent is width 0 and lives in
    // the same grapheme as 'c', so a match on the accent alone spans 0 columns.
    index.index_line(0, "abc\u{0301}def");

    let combining = "\u{0301}";

    // 1. Case-sensitive literal path (search_with_positions / site 522).
    assert!(
        index.search_with_positions(combining).is_empty(),
        "zero-width combining-mark match must be skipped (case-sensitive literal)"
    );

    // 2. Non-ASCII case-insensitive path (search_case_insensitive slow path).
    let ci = index
        .search_with_positions_opts(combining, false, false)
        .expect("case-insensitive literal search should not error");
    assert!(
        ci.is_empty(),
        "zero-width match must be skipped (case-insensitive)"
    );

    // 3. Forward match iterator (iterators.rs).
    let fwd: Vec<_> = index.search_from_line(combining, 0).collect();
    assert!(
        fwd.is_empty(),
        "zero-width match must be skipped (forward iterator)"
    );

    // 4. Reverse match iterator (iterators.rs).
    let rev: Vec<_> = index.search_before_line(combining, usize::MAX).collect();
    assert!(
        rev.is_empty(),
        "zero-width match must be skipped (reverse iterator)"
    );

    // 5. Regex path (only compiled with the `regex` feature).
    #[cfg(feature = "regex")]
    {
        let re = index
            .search_with_positions_opts(combining, true, true)
            .expect("regex search should not error");
        assert!(re.is_empty(), "zero-width match must be skipped (regex)");
    }

    // Positive control: a real, non-zero-width query on the SAME line still
    // matches, so the assertions above are not passing vacuously.
    let real = index.search_with_positions("abc");
    assert_eq!(real.len(), 1, "a real (width>0) match must still be found");
    assert_eq!(real[0].line, 0);
    assert!(
        real[0].start_col < real[0].end_col,
        "a real match must span at least one display column"
    );
}

/// `max_cached_for_retained` sizes a cap so the engine's 3/4 eviction low-water mark still
/// keeps the newest `retained` lines. Pins the arithmetic invariant (and its minimality)
/// across a range, then proves it end-to-end: an index sized to retain 8 lines still finds
/// a needle on the OLDEST of those 8 after indexing far past capacity.
#[test]
fn max_cached_for_retained_protects_the_newest_lines() {
    for retained in 0..64usize {
        let cap = max_cached_for_retained(retained);
        // The eviction target `floor(3·cap/4)` never drops below the retained floor.
        assert!(
            cap.saturating_mul(3) / 4 >= retained,
            "cap {cap} for retained {retained}: eviction target {} < {retained}",
            cap.saturating_mul(3) / 4
        );
        // ...and the cap is MINIMAL — one smaller would violate the floor.
        if retained > 0 {
            assert!(
                (cap - 1).saturating_mul(3) / 4 < retained,
                "cap {cap} is not the smallest that retains {retained}"
            );
        }
    }

    // End-to-end: retain the newest 8, index 200 lines, needle on the oldest-kept line.
    let cap = max_cached_for_retained(8);
    let mut index = SearchIndex::with_capacity_and_max(cap, cap);
    let total = 200usize;
    for i in 0..total {
        let text = if i == total - 8 {
            "OLDESTKEPT needle".to_string()
        } else {
            format!("filler {i}")
        };
        index.index_line(i, &text);
    }
    let hits: Vec<u32> = index.search("OLDESTKEPT").collect();
    assert_eq!(
        hits,
        vec![u32::try_from(total - 8).unwrap()],
        "the oldest of the newest-8 lines survives eviction and stays searchable"
    );
}
