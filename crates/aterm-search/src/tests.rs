// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// CRITICAL: an empty query returns nothing, on every path, without looping.
///
/// This catches the bug where `"text".find("")` returns `Some(0)` forever.
#[test]
fn empty_query_search_with_positions() {
    let mut index = SearchIndex::new();
    index.index_line(0, "test content");
    index.index_line(1, "more content");

    assert!(index.search_with_positions("").is_empty(), "positions");
    assert_eq!(index.search_from_line("", 0).count(), 0, "forward iterator");
    assert_eq!(
        index.search_before_line("", 10).count(),
        0,
        "reverse iterator"
    );
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

/// A repeated first-byte prefix must not make case-insensitive verification
/// quadratic: literals of three bytes or more use a linear-time search over
/// one reusable folded buffer.
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

/// Regex pattern exceeding `MAX_REGEX_PATTERN_LEN` is rejected before compilation.
///
/// Prevents ReDoS via compilation by bounding pattern length at the index
/// layer.
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

// =============================================================================
// Generation counter tests (#7271)
// =============================================================================

/// Every index mutation bumps the generation exactly once: per scrollback
/// line (the batch method too), per `index_visible_content` call, per
/// `clear`, and per `invalidate` — which leaves the content searchable.
#[test]
fn generation_bumps_once_per_mutation() {
    let mut search = TerminalSearch::new();
    assert_eq!(search.generation(), 0);
    search.index_scrollback_line("line 0");
    assert_eq!(search.generation(), 1);
    search.index_scrollback_lines(vec!["a", "b", "c"]);
    assert_eq!(search.generation(), 4, "one per batched line");
    search.index_visible_content(4, vec!["line A", "line B"]);
    assert_eq!(search.generation(), 5, "one per visible-content call");
    search.invalidate();
    assert_eq!(search.generation(), 6);
    assert_eq!(search.search("line A").len(), 1, "invalidate keeps content");
    search.clear();
    assert_eq!(search.generation(), 7);
}

/// The owned batch seam is result-identical to borrowed indexing and advances
/// the generation once for the whole batch.
#[test]
fn owned_numbered_content_matches_borrowed_indexing() {
    let mut borrowed = TerminalSearch::new();
    borrowed.index_visible_content(7, ["prefix needle suffix", "second needle"]);

    let mut first_owned = String::with_capacity(128);
    first_owned.push_str("prefix needle suffix");
    let first_buffer = first_owned.as_ptr();
    let first_capacity = first_owned.capacity();

    let mut owned = TerminalSearch::new();
    owned.index_numbered_content_owned([(7, first_owned), (8, String::from("second needle"))]);

    let retained = owned.index.lines.get(&7).expect("owned row is retained");
    assert_eq!(
        retained.as_ptr(),
        first_buffer,
        "owned indexing must retain the caller's allocation"
    );
    assert_eq!(retained.capacity(), first_capacity);

    let borrowed_results = borrowed
        .search_results_opts("needle", true, false)
        .expect("borrowed search is valid");
    let owned_results = owned
        .search_results_opts("needle", true, false)
        .expect("owned search is valid");
    assert_eq!(owned_results, borrowed_results);
    assert_eq!(owned.generation(), 1, "owned batch bumps exactly once");
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

// Regression for the lowercased-trigram fast path: gating the lowered pass
// behind `lower_need` must keep insert/remove/rebuild symmetric. An
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

    // 5. Regex path.
    let re = index
        .search_with_positions_opts(combining, true, true)
        .expect("regex search should not error");
    assert!(re.is_empty(), "zero-width match must be skipped (regex)");

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
    let hits: Vec<usize> = index.search("OLDESTKEPT").collect();
    assert_eq!(
        hits,
        vec![total - 8],
        "the oldest of the newest-8 lines survives eviction and stays searchable"
    );
}

// ---------------------------------------------------------------------------
// SA-1 incremental narrowing: differential battery.
//
// The contract under test: `search_literal_narrowed(q, case, None)` equals the
// forward batch path exactly, and `search_literal_narrowed(q + c, case,
// Some(frame_of(q)))` equals the batch path for `q + c` — for EVERY prefix
// step, both case modes, over corpora chosen to hit the treacherous shapes
// (Kelvin sign, sigma forms, prepend clusters, zero-width characters,
// combining marks, overlapping matches). The frame is OCCURRENCE lines, so
// zero-display-width matches (dropped from results) still keep their line
// narrable — the exact hazard a reported-match frame would lose.
// ---------------------------------------------------------------------------

/// Corpus with every shape the narrowing proof cares about.
fn narrowing_corpus() -> Vec<&'static str> {
    vec![
        "plain ascii find line",
        // U+FB01/sharp-s: this index folds with `char::to_lowercase`, which
        // leaves both alone — kept as non-ASCII lines that exercise the
        // Unicode fold + byte-map arms, NOT as multi-char-fold cases.
        "\u{fb01}nd zzz",
        "stra\u{df}e stop",
        "temperature 300\u{212a} now", // Kelvin sign folds to ascii k
        "\u{0600}1x prepended digit",  // prepend cluster: "1" spans no column
        "a\u{200b}b zero width",       // U+200B: zero-width-only occurrence
        "a\u{0301}x combining acute",  // combining mark cluster
        "BANANA banana bAnAnA",        // overlapping + mixed case
        "no hits here at all",
        "find find find",
        "\u{03a3}\u{03c3}\u{03c2} sigma forms", // final-sigma folding
        "",
        "xfind suffix and findx prefix",
    ]
}

fn narrowing_index() -> SearchIndex {
    let mut index = SearchIndex::new();
    for (i, line) in narrowing_corpus().into_iter().enumerate() {
        index.index_line(i, line);
    }
    index
}

fn batch_forward(index: &SearchIndex, query: &str, case_sensitive: bool) -> SearchResults {
    index
        .search_results_opts_direction(query, case_sensitive, false, SearchDirection::Forward)
        .expect("literal search cannot fail")
}

/// Type `query` one char at a time, narrowing each step from the previous
/// step's occurrence frame, asserting batch equality at EVERY step. Returns
/// how many steps actually ran off a frame (the reach signal).
fn type_and_check(index: &SearchIndex, query: &str, case_sensitive: bool) -> usize {
    let mut frame: Option<LineFrame> = None;
    let mut narrowed_steps = 0usize;
    let mut prefix = String::new();
    for c in query.chars() {
        prefix.push(c);
        if frame.is_some() {
            narrowed_steps += 1;
        }
        let step = index.search_literal_narrowed(&prefix, case_sensitive, frame.as_ref());
        let batch = batch_forward(index, &prefix, case_sensitive);
        assert_eq!(
            step.results,
            batch,
            "narrowed step for {prefix:?} (case_sensitive={case_sensitive}, framed={}) \
             must equal the batch path",
            narrowed_steps > 0
        );
        frame = step.occurrence_lines;
    }
    narrowed_steps
}

#[test]
fn narrowed_typing_equals_batch_for_every_prefix_step() {
    let index = narrowing_index();
    for case_sensitive in [false, true] {
        for query in [
            "find",
            "fin",
            "in",
            "ss",
            "st",
            "1x",
            "ban",
            "BANANA",
            "k n",
            "\u{03c2}",
            "a\u{0301}x",
            "zzz",
            "q",
            "\u{200b}b",
            "\u{0301}x",
        ] {
            let narrowed = type_and_check(&index, query, case_sensitive);
            assert_eq!(
                narrowed,
                query.chars().count() - 1,
                "reach: every step after the first must run off a frame \
                 (query {query:?}, case_sensitive={case_sensitive})"
            );
        }
    }
}

/// THE zero-width hazard, isolated — with the shapes this engine's fold and
/// column map really produce. `visit_matches` drops any match whose ORIGINAL
/// span resolves to zero display columns (`start_col == end_col`), so a line
/// can hold a genuine occurrence of `q` with ZERO reported matches while
/// `q + c` matches for real:
///
/// - a PREPEND cluster (U+0600 + "1"): the digit lives inside the cluster, so
///   "1" alone spans no column, while "1x" crosses the cluster boundary;
/// - a zero-width character (U+200B): it spans no column on its own, but the
///   two-char query straddling it into the next cell does.
///
/// (The multi-char-fold intuition — U+FB01 as "fi", sharp s as "ss" — is NOT
/// one of them here: this index folds with `char::to_lowercase`, which leaves
/// both alone, so those lines carry no such occurrence at all.)
///
/// A reported-match frame would lose these lines outright; the occurrence
/// frame must keep them.
#[test]
fn occurrence_frame_keeps_zero_width_only_lines_narrable() {
    // (line text, zero-width-only query, one-char extension that DOES match)
    let cases: &[(&str, &str, &str)] = &[
        ("\u{0600}1x prepended digit", "1", "1x"),
        ("a\u{200b}b zero width", "\u{200b}", "\u{200b}b"),
        ("x\u{0301}y combining acute", "\u{0301}", "\u{0301}y"),
    ];
    for &(text, short, grown) in cases {
        let mut index = SearchIndex::new();
        index.index_line(0, text);
        index.index_line(1, "unrelated");

        let step_short = index.search_literal_narrowed(short, false, None);
        assert!(
            step_short.results.matches.is_empty(),
            "precondition ({text:?}): {short:?} resolves to zero display columns and is dropped"
        );
        let frame = step_short
            .occurrence_lines
            .expect("uncapped walk must produce a frame");
        assert!(
            frame.contains(0),
            "the occurrence frame must retain {text:?} despite zero reported matches"
        );

        let step_grown = index.search_literal_narrowed(grown, false, Some(&frame));
        assert_eq!(step_grown.results, batch_forward(&index, grown, false));
        assert!(
            step_grown.results.matches.iter().any(|m| m.line == 0),
            "{grown:?} crossing the cluster boundary is a real, reported match on {text:?}"
        );
    }
}

/// The narrowing COST GUARD's input: `literal_candidate_bound` must report
/// what the engine's own candidate machinery would really visit — `None` when
/// there is no selectivity to offer (sub-trigram range scan), `Some(0)` when
/// a trigram is absent outright, and otherwise the SMALLEST involved posting
/// list (the list the candidate driver walks).
#[test]
fn literal_candidate_bound_reports_the_engines_own_candidate_count() {
    let mut index = SearchIndex::new();
    for i in 0..40 {
        index.index_line(i, &format!("svc-api request completed size=100 seq={i}"));
    }
    index.index_line(40, "svc-api request completed size=977 seq=rare");

    // Sub-trigram: the engine range-scans every retained line.
    assert_eq!(index.literal_candidate_bound("s", true), None);
    assert_eq!(index.literal_candidate_bound("si", true), None);

    // Common query: bounded by the smallest posting list, which every line is
    // in — a frame of the same size is allowed to win the tie.
    let common = index
        .literal_candidate_bound("size=", true)
        .expect("a trigram-length query has a bound");
    assert!(
        common >= 40,
        "every line carries the common query ({common})"
    );

    // Rare query: the "e=9" trigram exists on exactly one line, so the engine
    // would visit one candidate — far fewer than a frame inherited from the
    // common prefix.
    assert_eq!(index.literal_candidate_bound("size=9", true), Some(1));
    assert!(
        index.literal_candidate_bound("size=9", true).unwrap() < common,
        "the rare keystroke is exactly where a frame must be abandoned"
    );

    // Absent content: nothing to visit at all.
    assert_eq!(index.literal_candidate_bound("zzz-absent", true), Some(0));

    // Case-insensitive uses the folded query's trigrams.
    assert_eq!(
        index.literal_candidate_bound("SIZE=9", false),
        index.literal_candidate_bound("size=9", true)
    );

    // The bound is honest about what the walk really costs: verifying the
    // engine's candidates yields the same results as verifying everything.
    let bounded = index
        .search_literal_narrowed("size=9", true, None)
        .results
        .matches
        .len();
    assert_eq!(bounded, 1);
}

/// Backspace: a frame re-verifies ITS OWN query too (occurrences ⊇ matches),
/// so popping the stack needs no index work either.
#[test]
fn frame_reverifies_its_own_query_after_backspace() {
    let index = narrowing_index();
    let step_f = index.search_literal_narrowed("f", false, None);
    let frame_f = step_f.occurrence_lines.clone().expect("frame");
    let step_fi = index.search_literal_narrowed("fi", false, Some(&frame_f));
    let frame_fi = step_fi.occurrence_lines.expect("frame");
    // Backspace from "fin" to "fi": re-verify "fi" against its own frame.
    let replay = index.search_literal_narrowed("fi", false, Some(&frame_fi));
    assert_eq!(replay.results, batch_forward(&index, "fi", false));
    // And from "fi" to "f" using the deeper frame's ancestor.
    let replay_f = index.search_literal_narrowed("f", false, Some(&frame_f));
    assert_eq!(replay_f.results, batch_forward(&index, "f", false));
}

/// Capped walks: results still equal the FORWARD batch edge exactly, and no
/// frame is produced (a truncated frame would break the subset property).
#[test]
fn capped_narrowed_walk_matches_forward_batch_and_yields_no_frame() {
    let mut index = SearchIndex::new();
    // 1_000 lines × 120 overlapping 'a' matches ≫ MAX_SEARCH_MATCHES.
    let line = "a".repeat(120);
    for i in 0..1_000 {
        index.index_line(i, &line);
    }
    for case_sensitive in [false, true] {
        let step = index.search_literal_narrowed("a", case_sensitive, None);
        let batch = batch_forward(&index, "a", case_sensitive);
        assert_eq!(
            step.results, batch,
            "capped case_sensitive={case_sensitive}"
        );
        assert_eq!(step.results.matches.len(), MAX_SEARCH_MATCHES);
        assert!(step.results.incomplete, "the cap reports honestly");
        assert!(
            step.occurrence_lines.is_none(),
            "a capped walk must not seed a frame"
        );
    }
}

/// The eviction honesty pair rides through narrowing untouched: after the
/// index evicts, both paths report the same `incomplete` + watermark.
#[test]
fn narrowed_walk_carries_eviction_honesty_like_batch() {
    let mut index = SearchIndex::with_max_cached_lines(8);
    for i in 0..40 {
        index.index_line(i, &format!("needle {i}"));
    }
    assert!(index.results_may_be_incomplete(), "tiny cap must evict");
    let step = index.search_literal_narrowed("needle", false, None);
    let batch = batch_forward(&index, "needle", false);
    assert_eq!(step.results, batch);
    assert!(step.results.incomplete);
    let frame = step.occurrence_lines.expect("uncapped");
    let step2 = index.search_literal_narrowed("needle ", false, Some(&frame));
    assert_eq!(step2.results, batch_forward(&index, "needle ", false));
}

/// SA-3 (set-diff changed-row re-index): an arbitrary in-place edit script
/// must leave the index RESULT-identical to a from-scratch build over the
/// final texts — across shared / old-only / new-only trigram classes, the
/// unchanged-row skip, Unicode↔ASCII rewrites (the lowered-pass symmetry),
/// sigma folding, and the emptied-list prune. This is the oracle the
/// remove-all/insert-all → symmetric-difference rewrite is gated on.
#[test]
fn changed_row_reindex_matches_from_scratch_rebuild() {
    let script: &[(usize, &str)] = &[
        (0, "hello world"),
        (1, "GET /api/v1/items 200"),
        (2, "stra\u{df}e \u{fb01}nd"),
        (3, "needle alpha"),
        (0, "hello worlq"),           // tail edit: almost all trigrams shared
        (1, "GET /api/v1/items 500"), // digit edit mid-list (rows above exist)
        (2, "plain ascii now"),       // full rewrite, Unicode → ASCII
        (3, "needle alpha"),          // unchanged: the skip path
        (3, "NEEDLE ALPHA"),          // case flip: lowered pass diverges
        (0, "zzz totally new"),       // full rewrite, disjoint trigrams
        (4, "\u{03a3}\u{03c2} sigma forms"),
        (4, "\u{03c3} sigma forms"), // sigma-fold: lowered trigrams shared
    ];
    let mut incremental = SearchIndex::new();
    for &(row, text) in script {
        incremental.index_line(row, text);
    }
    let mut last = std::collections::BTreeMap::new();
    for &(row, text) in script {
        last.insert(row, text);
    }
    let mut fresh = SearchIndex::new();
    for (&row, &text) in &last {
        fresh.index_line(row, text);
    }

    for query in [
        "hello",
        "world",
        "worlq",
        "200",
        "500",
        "items",
        "needle",
        "NEEDLE",
        "alpha",
        "stra",
        "\u{df}",
        "\u{fb01}nd",
        "ascii",
        "zzz",
        "sigma",
        "\u{03c3}",
        "GET",
        "get",
        "tot",
    ] {
        for case_sensitive in [false, true] {
            let a = incremental
                .search_results_opts(query, case_sensitive, false)
                .expect("literal search cannot fail");
            let b = fresh
                .search_results_opts(query, case_sensitive, false)
                .expect("literal search cannot fail");
            assert_eq!(
                a, b,
                "incremental edits vs from-scratch: query {query:?} \
                 case_sensitive={case_sensitive}"
            );
        }
    }

    // Negative control: content that existed only pre-edit is really gone —
    // the old-only removals (and the emptied-list prune) did run.
    for stale in ["world 200", "worl\u{0064} ", "\u{df}e"] {
        assert!(
            incremental
                .search_results_opts(stale, true, false)
                .expect("ok")
                .matches
                .len()
                == fresh
                    .search_results_opts(stale, true, false)
                    .expect("ok")
                    .matches
                    .len(),
            "stale probe {stale:?}"
        );
    }
}

/// The set-diff must also hold when the SAME text bounces A→B→A: the row ends
/// where it started and every posting list must read as if never touched.
#[test]
fn changed_row_reindex_round_trip_is_identity() {
    let mut bounced = SearchIndex::new();
    let mut untouched = SearchIndex::new();
    for i in 0..20 {
        bounced.index_line(i, &format!("filler row {i} needle"));
        untouched.index_line(i, &format!("filler row {i} needle"));
    }
    bounced.index_line(7, "completely different text");
    bounced.index_line(7, "filler row 7 needle");

    for query in ["filler", "needle", "row 7", "different", "complet"] {
        for case_sensitive in [false, true] {
            assert_eq!(
                bounced
                    .search_results_opts(query, case_sensitive, false)
                    .expect("ok"),
                untouched
                    .search_results_opts(query, case_sensitive, false)
                    .expect("ok"),
                "A→B→A bounce must be observationally identity ({query:?})"
            );
        }
    }
}

/// `drop_history_below` (the complete-retention twin of `retain_history_from`)
/// must leave the index OBSERVATIONALLY IDENTICAL to a from-scratch build over
/// the surviving rows: same matches, same completeness (complete!), same
/// watermark — the equivalence `Terminal::indexed_search`'s incremental
/// refresh rests on. `retain_history_from` on the same input is the contrast:
/// identical matches but an honest `incomplete` flag.
#[test]
fn drop_history_below_matches_fresh_rebuild_including_completeness() {
    let mut dropped = SearchIndex::new();
    let mut retained = SearchIndex::new();
    for i in 0..40 {
        dropped.index_line(i, &format!("needle row {i}"));
        retained.index_line(i, &format!("needle row {i}"));
    }
    dropped.drop_history_below(15);
    retained.retain_history_from(15);

    let mut fresh = SearchIndex::new();
    for i in 15..40 {
        fresh.index_line(i, &format!("needle row {i}"));
    }

    let rows = |idx: &SearchIndex| -> Vec<usize> {
        idx.search_with_positions("needle")
            .into_iter()
            .map(|m| m.line)
            .collect()
    };
    assert_eq!(rows(&dropped), (15..40).collect::<Vec<_>>());
    assert_eq!(rows(&dropped), rows(&fresh), "matches equal a fresh build");
    assert_eq!(
        rows(&dropped),
        rows(&retained),
        "both twins drop the same rows"
    );

    // The observable divergence between the twins is EXACTLY the honesty pair.
    assert!(
        !dropped.results_may_be_incomplete(),
        "complete-retention drop must NOT report incompleteness (fresh-build parity)"
    );
    assert_eq!(dropped.lowest_retained_line(), 0, "fresh-build parity");
    assert!(
        retained.results_may_be_incomplete(),
        "index-side eviction keeps reporting honestly"
    );
    assert_eq!(retained.lowest_retained_line(), 15);

    // Dropped rows are really gone from the posting lists, not merely masked:
    // queries that ONLY ever matched below the cut return nothing, exactly as
    // in the fresh build. (The probes must be rows with no numeric extension
    // in range — "row 3" would also match rows 30..=39, which SURVIVE the
    // cut, and would pass for the wrong reason.)
    for below_cut in ["row 7", "row 14"] {
        assert!(
            dropped.search_with_positions(below_cut).is_empty(),
            "{below_cut:?} only ever matched below the cut"
        );
        assert!(
            fresh.search_with_positions(below_cut).is_empty(),
            "fresh-build parity for {below_cut:?}"
        );
    }
    // Positive control: a SURVIVING row stays findable, so the drop did not
    // simply empty the postings.
    assert_eq!(dropped.search_with_positions("row 39").len(), 1);
    assert_eq!(
        dropped.search_with_positions("row 39").len(),
        fresh.search_with_positions("row 39").len()
    );
}

/// A no-op drop (at or below the current first cached line) must change
/// nothing — the guard the per-search refresh relies on when retention did
/// not advance between searches.
#[test]
fn drop_history_below_is_a_noop_at_or_below_the_first_cached_line() {
    let mut idx = SearchIndex::new();
    for i in 5..25 {
        idx.index_line(i, &format!("needle {i}"));
    }
    let before: Vec<usize> = idx
        .search_with_positions("needle")
        .into_iter()
        .map(|m| m.line)
        .collect();
    idx.drop_history_below(5);
    idx.drop_history_below(0);
    let after: Vec<usize> = idx
        .search_with_positions("needle")
        .into_iter()
        .map(|m| m.line)
        .collect();
    assert_eq!(before, after);
    assert!(!idx.results_may_be_incomplete());
}

/// `len()` is one past the highest line ever indexed (sparse indexing counts
/// the gap), and re-indexing an existing line does not move it.
#[test]
fn len_is_a_high_water_mark() {
    let mut index = SearchIndex::new();
    index.index_line(4, "test line");
    assert_eq!(
        index.len(),
        5,
        "a sparse first line counts the gap below it"
    );
    index.index_line(1, "test line");
    assert_eq!(index.len(), 5, "a lower line does not lower the mark");
    index.index_line(4, "other text");
    assert_eq!(index.len(), 5, "re-indexing leaves the mark unchanged");
    index.index_line(6, "test line");
    assert_eq!(index.len(), 7);
}
