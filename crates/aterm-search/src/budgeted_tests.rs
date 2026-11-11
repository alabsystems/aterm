// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn deferred_projection_preserves_stored_cells_and_width_policy() {
    for ambiguous in [AmbiguousWidth::Single, AmbiguousWidth::Double] {
        let mut cells = LineCells::new();
        cells.push(0..1, 4, 1);
        cells.push(1..3, 9, 2);
        cells.push(3..4, 20, 0);
        for (query, sensitive, regex, expected) in [
            ("a", true, false, vec![(4, 5)]),
            ("É", false, false, vec![(9, 11)]),
            ("é", true, true, vec![(9, 11)]),
            ("aé", true, false, vec![(4, 11)]),
            ("B", true, false, vec![]),
            ("b", false, false, vec![]),
            ("B", true, true, vec![]),
            ("absent", false, true, vec![]),
        ] {
            let mut search = BudgetedSearch::new(query, sensitive, regex, 0, 1)
                .unwrap()
                .with_ambiguous_width(ambiguous);
            search.feed_row_cells("aéB".into(), &cells);
            assert_eq!(
                search
                    .results()
                    .matches
                    .iter()
                    .map(|m| (m.start_col, m.end_col))
                    .collect::<Vec<_>>(),
                expected,
                "{query:?} {ambiguous:?}"
            );
        }
        let mut merged = LineCells::new();
        merged.push(0..2, 7, 3);
        for (sensitive, regex) in [(true, false), (false, false), (true, true)] {
            let query = if regex { "a|b" } else { "a" };
            let mut search = BudgetedSearch::new(query, sensitive, regex, 0, 1).unwrap();
            search.feed_row_cells("ab".into(), &merged);
            assert!(
                search
                    .results()
                    .matches
                    .iter()
                    .all(|m| (m.start_col, m.end_col) == (7, 10))
            );
            assert_eq!(search.result_count(), if regex { 2 } else { 1 });
        }
        let mut omitted = LineCells::new();
        omitted.push(0..1, 4, 1);
        omitted.mark_truncated();
        for (query, sensitive, regex) in
            [("b", true, false), ("B", false, false), ("b|z", true, true)]
        {
            let mut search = BudgetedSearch::new(query, sensitive, regex, 0, 1).unwrap();
            search.feed_row_cells("abc".into(), &omitted);
            assert_eq!(search.result_count(), 0);
            assert_eq!(search.results().cut_short_lines, 1);
        }
        let mut text = BudgetedSearch::new("a", true, false, 0, 1)
            .unwrap()
            .with_ambiguous_width(ambiguous);
        text.feed_row_owned("→a".into());
        let start = if ambiguous == AmbiguousWidth::Double {
            2
        } else {
            1
        };
        assert_eq!(
            (
                text.results().matches[0].start_col,
                text.results().matches[0].end_col
            ),
            (start, start + 1)
        );
    }
}

fn one_shot(
    rows: &[&str],
    base: usize,
    query: &str,
    case_sensitive: bool,
    is_regex: bool,
) -> SearchResults {
    let mut index = SearchIndex::new();
    for (i, row) in rows.iter().enumerate() {
        index.index_line(base + i, row);
    }
    index
        .search_results_opts(query, case_sensitive, is_regex)
        .expect("oracle search must succeed")
}

fn depth_window_index(
    rows: &[&str],
    cells: Option<&[LineCells]>,
    base: usize,
    cap: usize,
) -> SearchIndex {
    let skipped = rows.len().saturating_sub(cap);
    let mut index = SearchIndex::with_capacity_and_max(rows.len() - skipped, cap);
    for (i, row) in rows.iter().enumerate().skip(skipped) {
        index.index_line_cow(base + i, Cow::Borrowed(row), cells.map(|cells| &cells[i]));
    }
    index.set_history_prefix_eviction((skipped > 0).then_some(base + skipped));
    index
}

fn drive(mut search: BudgetedSearch, rows: &[&str]) -> SearchResults {
    while !search.is_complete() {
        search.feed_row(rows[search.rows_fed()]);
    }
    search.results()
}

fn budgeted(
    rows: &[&str],
    base: usize,
    query: &str,
    case_sensitive: bool,
    is_regex: bool,
) -> SearchResults {
    let search = BudgetedSearch::new(query, case_sensitive, is_regex, base, rows.len())
        .expect("budgeted construction must succeed");
    drive(search, rows)
}

fn sample_rows() -> Vec<&'static str> {
    vec![
        "the quick brown fox",
        "NEEDLE in a haystack",
        "needle needle needle",
        "",
        "aaaaaa overlapping aaa runs",
        "wide \u{4f60}\u{597d} needle after CJK",
        "N\u{e9}edle with accents \u{c4}rger",
        "tail needleneedle",
        "unrelated filler line",
        "last NEEDLE",
    ]
}

#[test]
fn owned_feed_matches_borrowed_and_batch_unicode_coordinates() {
    let rows = [
        "e\u{301} 日本語 👩🏽‍💻 NEEDLE needle",
        "\tΟΣ ος οσ\r\nneedle",
        "tail e\u{301} 日本語 👩🏽‍💻",
    ];
    for (query, case_sensitive, is_regex) in [
        ("needle", true, false),
        ("οσ", false, false),
        ("e\u{301}|日本語|👩🏽‍💻", true, true),
        ("NEEDLE", false, true),
        ("", true, false),
        ("", false, true),
    ] {
        let mut owned = BudgetedSearch::new(query, case_sensitive, is_regex, 7, rows.len())
            .expect("budgeted construction");
        let mut borrowed = BudgetedSearch::new(query, case_sensitive, is_regex, 7, rows.len())
            .expect("budgeted construction");
        for row in rows {
            let mut text = String::with_capacity(4096);
            text.push_str(row);
            owned.feed_row_owned(text);
            borrowed.feed_row(row);
            assert_eq!(owned.results(), borrowed.results());
            assert_eq!(owned.rows_fed(), borrowed.rows_fed());
        }
        assert!(owned.is_complete());
        assert_eq!(
            owned.results(),
            one_shot(&rows, 7, query, case_sensitive, is_regex),
            "query={query:?} cs={case_sensitive} rx={is_regex}"
        );
    }
}

#[test]
fn past_its_cap_the_search_equals_the_depth_window_not_the_eviction_schedule() {
    let rows: Vec<String> = (0..100).map(|i| format!("needle row {i}")).collect();
    let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
    let oracle = depth_window_index(&rows, None, 5, 10)
        .search_results_opts("needle", true, false)
        .expect("oracle search");
    assert!(
        oracle.incomplete,
        "the rows past the window are not searched"
    );
    assert_eq!(oracle.lowest_retained_line, 5 + 90);
    assert_eq!(oracle.matches.len(), 10, "every row of the window");
    let mut search = BudgetedSearch::with_max_cached_lines("needle", true, false, 5, 100, 10)
        .expect("budgeted construction");
    for row in &rows {
        search.feed_row(row);
    }
    assert_eq!(search.results(), oracle);
    let mut evicting = SearchIndex::with_max_cached_lines(10);
    for (i, row) in rows.iter().enumerate() {
        evicting.index_line(5 + i, row);
    }
    let scheduled = evicting
        .search_results_opts("needle", true, false)
        .expect("search");
    assert_eq!(
        scheduled.lowest_retained_line,
        5 + 92,
        "the low-water schedule"
    );
    assert_ne!(scheduled, oracle);
}

#[test]
fn the_row_feed_equals_one_shot_across_all_modes() {
    let rows = sample_rows();
    let cases: &[(&str, bool, bool)] = &[
        ("needle", true, false),  // case-sensitive literal
        ("needle", false, false), // case-insensitive literal
        ("NEEDLE", false, false), // case-insensitive, uppercase query
        ("aa", true, false),      // short query (no trigram accel)
        ("aa", false, false),
        ("\u{e9}", false, false),      // Unicode short query
        ("\u{c4}rger", false, false),  // Unicode folding
        ("ne+dle", true, true),        // regex, case-sensitive
        ("ne+dle", false, true),       // regex, case-insensitive
        ("n..dle", true, true),        // regex with wildcards
        ("x{0,3}needle", false, true), // regex with optional prefix
    ];
    for &(query, case_sensitive, is_regex) in cases {
        let oracle = one_shot(&rows, 37, query, case_sensitive, is_regex);
        let got = budgeted(&rows, 37, query, case_sensitive, is_regex);
        assert_eq!(
            got, oracle,
            "query={query:?} cs={case_sensitive} rx={is_regex}"
        );
    }
}

#[test]
fn match_cap_matches_one_shot_and_reports_incomplete() {
    let row = "a".repeat(80);
    let rows: Vec<&str> = (0..1_300).map(|_| row.as_str()).collect();
    let oracle = one_shot(&rows, 0, "a", true, false);
    assert!(oracle.incomplete, "cap must be reported as incomplete");
    assert_eq!(oracle.matches.len(), MAX_SEARCH_MATCHES);
    let got = budgeted(&rows, 0, "a", true, false);
    assert_eq!(got, oracle);
}

#[test]
fn combined_match_cap_and_depth_window_equals_one_shot() {
    const CACHE_LINES: usize = 10;
    const ROWS: usize = 20;
    let dense = "a".repeat(MAX_SEARCH_MATCHES / 2 + 1);
    let rows: Vec<&str> = (0..ROWS).map(|_| dense.as_str()).collect();

    let oracle = depth_window_index(&rows, None, 0, CACHE_LINES)
        .search_results_opts("a", true, false)
        .expect("oracle search");
    assert!(
        oracle.incomplete,
        "cap plus the depth window must be reported"
    );
    assert_eq!(oracle.matches.len(), MAX_SEARCH_MATCHES);
    assert!(
        oracle
            .matches
            .iter()
            .all(|m| m.line >= oracle.lowest_retained_line),
        "oracle only returns the window"
    );

    let search =
        BudgetedSearch::with_max_cached_lines("a", true, false, 0, rows.len(), CACHE_LINES)
            .expect("budgeted construction");
    assert_eq!(drive(search, &rows), oracle);
}

#[test]
fn invalid_regex_fails_at_construction() {
    let err = BudgetedSearch::new("f(oo", false, true, 0, 10);
    assert!(matches!(err, Err(SearchOptionsError::InvalidRegex(_))));
}

#[test]
fn a_row_the_regex_scan_cannot_finish_is_reported_like_the_one_shot() {
    let wide = format!("{}z", "x".repeat(4095));
    let run = "a".repeat(4096);
    for (pattern, row) in [("(?:x?){1020}z", wide.as_str()), (r"\w+=|\w", run.as_str())] {
        let rows = ["x z aa", row, "after z aa", "x z aa again"];
        let mut index = SearchIndex::new();
        for (i, text) in rows.iter().enumerate() {
            index.index_line(i, text);
        }
        let oracle = index
            .search_results_opts(pattern, true, true)
            .expect("a scan out of budget is reported, not refused");
        assert_eq!(oracle.cut_short_lines, 1, "{pattern:?}: {oracle:?}");
        assert!(oracle.incomplete, "{pattern:?}");
        assert!(
            oracle.matches.iter().any(|m| m.line == 2)
                && oracle.matches.iter().any(|m| m.line == 3),
            "{pattern:?}: the rows after the long one are still searched: {oracle:?}"
        );
        let mut search = BudgetedSearch::new(pattern, true, true, 0, rows.len()).expect("compiles");
        search.feed_row(rows[0]);
        assert_eq!(
            search.results().cut_short_lines,
            0,
            "{pattern:?}: no row has run out yet"
        );
        for text in &rows[1..] {
            search.feed_row(text);
        }
        assert_eq!(search.results(), oracle, "{pattern:?}");
        assert_eq!(search.results_range(0, usize::MAX), oracle, "{pattern:?}");
    }
}

#[test]
fn a_row_read_short_of_its_end_is_reported_like_the_one_shot() {
    let wide = "x".repeat(4096);
    let rows = [
        "x z aa",
        wide.as_str(),
        "after z aa",
        "x z aa again",
        "z tail",
    ];
    let cells: Vec<LineCells> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let mut cells = LineCells::identity(row.len());
            if i == 1 {
                cells.mark_truncated();
            }
            cells
        })
        .collect();
    for (pattern, case_sensitive, is_regex, cap, cut_short) in [
        ("aa", true, false, 100, 1),
        ("AA", false, false, 100, 1),
        ("z", true, true, 100, 1),
        ("(?:x?){1020}z", true, true, 100, 1),
        ("z", true, true, 3, 0),
    ] {
        let oracle = depth_window_index(&rows, Some(&cells), 0, cap)
            .search_results_opts(pattern, case_sensitive, is_regex)
            .expect("searched");
        assert_eq!(oracle.cut_short_lines, cut_short, "{pattern:?}: {oracle:?}");
        let mut search = BudgetedSearch::with_max_cached_lines(
            pattern,
            case_sensitive,
            is_regex,
            0,
            rows.len(),
            cap,
        )
        .expect("compiles");
        for (row, cells) in rows.iter().zip(&cells) {
            search.feed_row_cells((*row).to_string(), cells);
        }
        assert_eq!(search.results(), oracle, "{pattern:?} cap {cap}");
    }
}

#[test]
fn partial_results_are_a_prefix_and_overfeed_is_a_noop() {
    let rows = sample_rows();
    let mut search =
        BudgetedSearch::new("needle", false, false, 0, rows.len()).expect("construction");
    let mut previous = 0usize;
    let mut seen: Vec<SearchMatch> = Vec::new();
    for row in &rows {
        search.feed_row(row);
        let partial = search.results();
        assert!(partial.matches.len() >= previous, "match count is monotone");
        assert_eq!(
            &partial.matches[..seen.len()],
            seen.as_slice(),
            "earlier matches are stable across slices"
        );
        previous = partial.matches.len();
        seen = partial.matches;
    }
    assert!(search.is_complete());
    let done = search.results();
    search.feed_row("needle beyond the window");
    assert_eq!(search.results(), done, "overfeed must not change results");
    assert_eq!(done, one_shot(&rows, 0, "needle", false, false));
}

#[test]
fn result_ranges_are_bounded_and_stable_past_the_cap() {
    let rows: Vec<String> = (0..20).map(|i| format!("needle row {i}")).collect();
    let mut search =
        BudgetedSearch::with_max_cached_lines("needle", true, false, 0, rows.len(), 10)
            .expect("construction");

    for row in rows.iter().take(14) {
        search.feed_row(row);
    }
    let first = search.results_range(0, 2);
    assert!(first.matches.len() <= 2);
    assert!(first.matches.iter().all(|m| m.line >= 10));
    assert!(first.incomplete);
    assert_eq!(first.lowest_retained_line, 10);

    for row in rows.iter().skip(14) {
        search.feed_row(row);
    }
    let final_results = search.results();
    assert_eq!(
        &final_results.matches[..first.matches.len()],
        first.matches.as_slice(),
        "an emitted delta must remain a prefix of the finished search"
    );
    let second = search.results_range(first.matches.len(), 2);
    assert!(second.matches.len() <= 2);
    assert_eq!(second.lowest_retained_line, first.lowest_retained_line);
    assert_eq!(
        [first.matches, second.matches].concat(),
        final_results.matches[..4]
    );
}
