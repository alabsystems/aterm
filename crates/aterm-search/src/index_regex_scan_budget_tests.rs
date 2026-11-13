// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

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
