// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

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

    assert!(index.might_contain("hello"));
    assert!(index.might_contain("world"));

    assert!(
        !index.might_contain("zzqxj"),
        "bloom filter should reject query with no matching trigrams"
    );

    assert!(index.might_contain("zz"));
}

#[test]
fn terminal_search_find_next_prev() {
    let mut search = TerminalSearch::new();

    search.index_scrollback_line("match here");
    search.index_scrollback_line("no match");
    search.index_scrollback_line("match again");

    let next = search.find_next("match", 0, 0);
    let m = next.expect("find_next should find 'match' in 'no match' at line 1");
    assert_eq!(m.line, 1);
    assert_eq!(m.start_col, 3);

    let next = search
        .find_next("match", 1, 7)
        .expect("find_next should find 'match again' at line 2");
    assert_eq!(next.line, 2);

    let prev = search.find_prev("match", 3, 0);
    let m = prev.unwrap();
    assert_eq!(m.line, 2);
    assert_eq!(m.start_col, 0);
    assert_eq!(m.end_col, 5);

    let prev = search.find_prev("match", 2, 0);
    let m = prev.unwrap();
    assert_eq!(m.line, 1);
    assert_eq!(m.start_col, 3);
    assert_eq!(m.end_col, 8);

    let prev = search.find_prev("match", 1, 0);
    let m = prev.unwrap();
    assert_eq!(m.line, 0);
    assert_eq!(m.start_col, 0);
    assert_eq!(m.end_col, 5);
}

#[test]
fn find_next_prev_same_line_column_boundary() {
    let mut search = TerminalSearch::new();
    search.index_scrollback_line("test test");

    let m = search.find_next("test", 0, 0).unwrap();
    assert_eq!(m.line, 0);
    assert_eq!(m.start_col, 5);

    let m = search.find_next("test", 0, 4).unwrap();
    assert_eq!(m.line, 0);
    assert_eq!(m.start_col, 5);

    assert!(
        search.find_next("test", 0, 5).is_none(),
        "find_next at exact match col should not re-find same match"
    );

    let m = search
        .find_prev("test", 0, 5)
        .expect("find_prev from col 5 should find col 0");
    assert_eq!(m.line, 0);
    assert_eq!(m.start_col, 0);

    let m = search.find_prev("test", 0, 1).unwrap();
    assert_eq!(m.line, 0);
    assert_eq!(m.start_col, 0);

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

    let matches: Vec<_> = index.search_before_line("test", 3).collect();
    assert_eq!(matches.len(), 3);
    assert!(matches.iter().all(|m| m.line < 3));
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
    assert_eq!(matches.len(), 4);
    assert_eq!(matches[0].line, 1);
    assert_eq!(matches[1].line, 0);
    assert_eq!(matches[1].start_col, 10); // rightmost first
    assert_eq!(matches[2].line, 0);
    assert_eq!(matches[2].start_col, 5);
    assert_eq!(matches[3].line, 0);
    assert_eq!(matches[3].start_col, 0); // leftmost last
}

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
fn search_iterator_multibyte_no_panic() {
    let mut index = SearchIndex::new();
    index.index_line(0, "ああああ");

    let matches: Vec<_> = index.search_from_line("ああ", 0).collect();
    assert_eq!(matches.len(), 3, "should find 3 overlapping matches");
    assert_eq!(matches[0].start_col, 0); // column 0
    assert_eq!(matches[1].start_col, 2); // column 2 (after 1st "あ")
    assert_eq!(matches[2].start_col, 4); // column 4 (after 2nd "あ")
}

#[test]
fn search_single_multibyte_char_repeated() {
    let mut index = SearchIndex::new();
    index.index_line(0, "日日日");

    let matches = index.search_with_positions("日");
    assert_eq!(matches.len(), 3, "should find all three occurrences");
    assert_eq!(matches[0].start_col, 0, "first 日 at column 0");
    assert_eq!(matches[1].start_col, 2, "second 日 at column 2");
    assert_eq!(matches[2].start_col, 4, "third 日 at column 4");
}

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

#[test]
fn terminal_search_index_visible_content() {
    let mut search = TerminalSearch::new();

    search.index_scrollback_line("scrollback 0");
    search.index_scrollback_line("scrollback 1");
    search.index_scrollback_line("scrollback 2");

    let visible = vec!["visible line A", "visible line B"];
    search.index_visible_content(3, visible);

    let matches = search.search("scrollback");
    assert_eq!(matches.len(), 3, "scrollback lines should be found");

    let matches = search.search("visible");
    assert_eq!(matches.len(), 2, "visible lines should be found");
    assert_eq!(matches[0].line, 3, "first visible line at index 3");
    assert_eq!(matches[1].line, 4, "second visible line at index 4");
}

#[test]
fn find_prev_usize_max_does_not_overflow() {
    let mut search = TerminalSearch::new();
    search.index_scrollback_line("test content here");
    let result = search.find_prev("test", usize::MAX, 0);
    let m = result.expect("find_prev with usize::MAX should not panic and should find a match");
    assert_eq!(m.line, 0);
    assert_eq!(m.start_col, 0);
}

#[test]
fn search_before_line_zero_returns_empty() {
    let mut index = SearchIndex::new();
    index.index_line(0, "test line zero");
    index.index_line(1, "test line one");

    let matches: Vec<_> = index.search_before_line("test", 0).collect();
    assert!(matches.is_empty(), "no lines exist before line 0");

    let matches: Vec<_> = index.search_before_line("te", 0).collect();
    assert!(
        matches.is_empty(),
        "short query also returns empty before line 0"
    );
}

#[test]
fn find_next_past_last_line() {
    let mut search = TerminalSearch::new();
    search.index_scrollback_line("match line 0");
    search.index_scrollback_line("match line 1");

    assert!(search.find_next("match", 2, 0).is_none());
    assert!(search.find_next("match", 100, 0).is_none());

    let m = search
        .find_next("match", 0, 0)
        .expect("invariant: line 1 has 'match' after position (0,0)");
    assert_eq!(m.line, 1, "should find match at line 1");

    assert!(search.find_next("match", 1, 0).is_none());
}

#[test]
fn case_insensitive_ascii_search_reports_overlapping_matches() {
    let mut index = SearchIndex::new();
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

    let none = index
        .search_with_positions_opts("xyz", false, false)
        .unwrap();
    assert!(none.is_empty());
}

#[test]
fn batch_search_caps_matches_and_flags_incomplete() {
    let mut index = SearchIndex::with_capacity(30_000);
    let line = "the quick brown fox jumped over the lazy fence near the hedge";
    let per_line = line.matches('e').count();
    let lines_needed = MAX_SEARCH_MATCHES / per_line + 1_000;
    for i in 0..lines_needed {
        index.index_line(i, line);
    }

    let cs = index.search_with_positions("e");
    assert_eq!(
        cs.len(),
        MAX_SEARCH_MATCHES,
        "case-sensitive batch must cap at MAX_SEARCH_MATCHES"
    );

    let ci = index.search_with_positions_opts("e", false, false).unwrap();
    assert_eq!(
        ci.len(),
        MAX_SEARCH_MATCHES,
        "case-insensitive batch must cap at MAX_SEARCH_MATCHES"
    );

    assert!(
        ci.iter().any(|m| m.line == 0),
        "capped result must include the lowest line"
    );
    let max_line = ci.iter().map(|m| m.line).max().unwrap();
    assert!(
        max_line < lines_needed - 1,
        "capped result must drop the highest lines (kept up to {max_line} of {lines_needed})"
    );

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

    let results = index.search_results_opts("e", false, false).unwrap();
    assert_eq!(results.matches.len(), MAX_SEARCH_MATCHES);
    assert!(
        results.incomplete,
        "a capped result set must be reported as incomplete, not exhaustive"
    );

    let terminal = TerminalSearch {
        index,
        indexed_scrollback_lines: 0,
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

#[test]
fn regex_batch_search_caps_matches_deterministically() {
    let mut index = SearchIndex::with_capacity(30_000);
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

#[test]
fn regex_search_filters_zero_length_matches() {
    let mut index = SearchIndex::new();
    index.index_line(0, "hello world");
    index.index_line(1, "foo bar");

    let results = index
        .search_with_positions_opts("^", true, true)
        .expect("valid regex");
    assert!(
        results.is_empty(),
        "pure anchor pattern `^` should produce 0 results after zero-length filtering, got {}",
        results.len(),
    );

    let results = index
        .search_with_positions_opts(r"\b", true, true)
        .expect("valid regex");
    assert!(
        results.is_empty(),
        "pure boundary pattern `\\b` should produce 0 results after zero-length filtering, got {}",
        results.len(),
    );

    let results = index
        .search_with_positions_opts("x*", true, true)
        .expect("valid regex");
    assert!(
        results.is_empty(),
        "`x*` on non-x input should produce 0 results after zero-length filtering, got {}",
        results.len(),
    );
}

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

#[test]
fn case_insensitive_greek_final_sigma_unified() {
    let mut index = SearchIndex::new();
    index.index_line(0, "\u{039F}\u{0394}\u{039F}\u{03A3}");
    index.index_line(1, "no greek here");

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

    index.index_line(1, &query);
    let _ = SearchIndex::take_case_insensitive_lowered_lines();
    let matches = index
        .search_with_positions_opts(&query, false, false)
        .unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].line, 1);
    assert_eq!(SearchIndex::take_case_insensitive_lowered_lines(), 2);
}

#[test]
fn case_sensitive_point_navigation_stops_at_the_neighbouring_match() {
    let mut search = TerminalSearch::with_capacity(10_000);
    for _ in 0..10_000 {
        search.index_scrollback_line("hit aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    }

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
}

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

#[test]
fn regex_pattern_too_long_rejected() {
    let mut index = SearchIndex::new();
    index.index_line(0, "test content");

    let at_limit = "a".repeat(1024);
    let result = index.search_with_positions_opts(&at_limit, true, true);
    assert!(
        result.is_ok(),
        "pattern at exactly 1024 bytes should be accepted"
    );

    let over_limit = "a".repeat(1025);
    let result = index.search_with_positions_opts(&over_limit, true, true);
    assert_eq!(
        result,
        Err(SearchOptionsError::PatternTooLong),
        "pattern exceeding 1024 bytes should return PatternTooLong"
    );
}

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
}

#[test]
fn eviction_with_sparse_line_numbers() {
    let mut index = SearchIndex::new();
    index.set_max_cached_lines(8);

    for i in 1..=10 {
        let line_num = i * 1_000_000;
        index.index_line(line_num, &format!("sparse line {i}"));
    }

    assert!(
        index.lines.len() <= 8,
        "cache should be at most 8 lines after eviction, got {}",
        index.lines.len(),
    );

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

    assert!(
        index.get_line(1_000_000).is_none(),
        "oldest line (1M) should be evicted"
    );

    let matches = index.search_with_positions("sparse");
    assert!(
        !matches.is_empty(),
        "surviving lines should still be searchable"
    );
    for m in &matches {
        assert!(
            index.get_line(m.line).is_some(),
            "match at line {} should reference a cached line",
            m.line,
        );
    }
}

#[test]
fn eviction_sets_incomplete_signal_and_watermark() {
    let mut index = SearchIndex::with_max_cached_lines(8);
    assert_eq!(index.max_cached_lines(), 8, "cap must be configurable");

    assert!(
        !index.results_may_be_incomplete(),
        "fresh index must report complete results"
    );
    assert_eq!(index.lowest_retained_line(), 0);

    for i in 0..40 {
        index.index_line(i, &format!("alpha bravo line {i}"));
    }

    assert!(
        index.results_may_be_incomplete(),
        "indexing past the cap must mark results incomplete"
    );

    assert!(
        index.lowest_retained_line() > 0,
        "lowest_retained_line must advance after eviction, got {}",
        index.lowest_retained_line()
    );

    assert_eq!(
        index.len(),
        40,
        "line_count keeps growing even though oldest lines were evicted"
    );

    assert!(
        index.get_line(0).is_none(),
        "oldest line must have been evicted"
    );
    assert!(
        index.get_line(39).is_some(),
        "newest line must survive eviction"
    );

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

#[test]
fn cap_is_configurable_and_clear_resets_signal() {
    let mut index = SearchIndex::new();
    assert_eq!(
        index.max_cached_lines(),
        DEFAULT_MAX_CACHED_LINES,
        "default cap is the documented constant"
    );

    index.set_max_cached_lines(0);
    assert_eq!(index.max_cached_lines(), 1, "0 cap clamps to 1");

    index.set_max_cached_lines(4);
    assert_eq!(index.max_cached_lines(), 4);

    for i in 0..20 {
        index.index_line(i, &format!("charlie delta {i}"));
    }
    assert!(index.results_may_be_incomplete());
    assert!(index.lowest_retained_line() > 0);

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

#[test]
fn lowercase_fast_path_keeps_insert_remove_symmetric() {
    let mut index = SearchIndex::new();

    index.index_line(0, "hello world");
    index.index_line(1, "Hello World");

    let ci = index
        .search_with_positions_opts("hello", false, false)
        .unwrap();
    let lines: Vec<usize> = ci.iter().map(|m| m.line).collect();
    assert!(lines.contains(&0), "all-lowercase line must be searchable");
    assert!(lines.contains(&1), "mixed-case line must be searchable");

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

    index.index_line(0, "Foo Bar Baz");
    let ci2 = index
        .search_with_positions_opts("foo bar", false, false)
        .unwrap();
    assert!(
        ci2.iter().any(|m| m.line == 0),
        "mixed-case overwrite of a lowercase line must be searchable"
    );
}

#[test]
fn zero_display_width_matches_are_skipped() {
    let mut index = SearchIndex::new();
    index.index_line(0, "abc\u{0301}def");

    let combining = "\u{0301}";

    assert!(
        index.search_with_positions(combining).is_empty(),
        "zero-width combining-mark match must be skipped (case-sensitive literal)"
    );

    let ci = index
        .search_with_positions_opts(combining, false, false)
        .expect("case-insensitive literal search should not error");
    assert!(
        ci.is_empty(),
        "zero-width match must be skipped (case-insensitive)"
    );

    let fwd: Vec<_> = index.search_from_line(combining, 0).collect();
    assert!(
        fwd.is_empty(),
        "zero-width match must be skipped (forward iterator)"
    );

    let rev: Vec<_> = index.search_before_line(combining, usize::MAX).collect();
    assert!(
        rev.is_empty(),
        "zero-width match must be skipped (reverse iterator)"
    );

    let re = index
        .search_with_positions_opts(combining, true, true)
        .expect("regex search should not error");
    assert!(re.is_empty(), "zero-width match must be skipped (regex)");

    let real = index.search_with_positions("abc");
    assert_eq!(real.len(), 1, "a real (width>0) match must still be found");
    assert_eq!(real[0].line, 0);
    assert!(
        real[0].start_col < real[0].end_col,
        "a real match must span at least one display column"
    );
}

#[test]
fn max_cached_for_retained_protects_the_newest_lines() {
    for retained in 0..64usize {
        let cap = max_cached_for_retained(retained);
        assert!(
            cap.saturating_mul(3) / 4 >= retained,
            "cap {cap} for retained {retained}: eviction target {} < {retained}",
            cap.saturating_mul(3) / 4
        );
        if retained > 0 {
            assert!(
                (cap - 1).saturating_mul(3) / 4 < retained,
                "cap {cap} is not the smallest that retains {retained}"
            );
        }
    }

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

/// Corpus with every shape the narrowing proof cares about.
fn narrowing_corpus() -> Vec<&'static str> {
    vec![
        "plain ascii find line",
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

/// Type `query` one char at a time.
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

#[test]
fn occurrence_frame_keeps_zero_width_only_lines_narrable() {
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

#[test]
fn literal_candidate_bound_reports_the_engines_own_candidate_count() {
    let mut index = SearchIndex::new();
    for i in 0..40 {
        index.index_line(i, &format!("svc-api request completed size=100 seq={i}"));
    }
    index.index_line(40, "svc-api request completed size=977 seq=rare");

    assert_eq!(index.literal_candidate_bound("s", true), None);
    assert_eq!(index.literal_candidate_bound("si", true), None);

    let common = index
        .literal_candidate_bound("size=", true)
        .expect("a trigram-length query has a bound");
    assert!(
        common >= 40,
        "every line carries the common query ({common})"
    );

    assert_eq!(index.literal_candidate_bound("size=9", true), Some(1));
    assert!(
        index.literal_candidate_bound("size=9", true).unwrap() < common,
        "the rare keystroke is exactly where a frame must be abandoned"
    );

    assert_eq!(index.literal_candidate_bound("zzz-absent", true), Some(0));

    assert_eq!(
        index.literal_candidate_bound("SIZE=9", false),
        index.literal_candidate_bound("size=9", true)
    );

    let bounded = index
        .search_literal_narrowed("size=9", true, None)
        .results
        .matches
        .len();
    assert_eq!(bounded, 1);
}

#[test]
fn frame_reverifies_its_own_query_after_backspace() {
    let index = narrowing_index();
    let step_f = index.search_literal_narrowed("f", false, None);
    let frame_f = step_f.occurrence_lines.clone().expect("frame");
    let step_fi = index.search_literal_narrowed("fi", false, Some(&frame_f));
    let frame_fi = step_fi.occurrence_lines.expect("frame");
    let replay = index.search_literal_narrowed("fi", false, Some(&frame_fi));
    assert_eq!(replay.results, batch_forward(&index, "fi", false));
    let replay_f = index.search_literal_narrowed("f", false, Some(&frame_f));
    assert_eq!(replay_f.results, batch_forward(&index, "f", false));
}

#[test]
fn capped_narrowed_walk_matches_forward_batch_and_yields_no_frame() {
    let mut index = SearchIndex::new();
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
    assert_eq!(dropped.search_with_positions("row 39").len(), 1);
    assert_eq!(
        dropped.search_with_positions("row 39").len(),
        fresh.search_with_positions("row 39").len()
    );
}

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
