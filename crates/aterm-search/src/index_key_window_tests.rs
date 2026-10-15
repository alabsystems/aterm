// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Line numbers past `u32::MAX` (R15 content-1).
//!
//! Every terminal caller keys the index by ABSOLUTE row, a counter that only
//! grows and outlives live upgrades. When the postings held the line number
//! itself as `u32`, every row past `u32::MAX` saturated onto one key and
//! every literal and regex find answered "0 matches", complete. These pin the
//! base-relative keys: whatever the rows are numbered, every path a line
//! number enters or leaves the index by gives back the line it was fed.

use super::*;
use crate::TerminalSearch;

const TWO_32: usize = 1 << 32;

/// The four rows the probes index, and which of them each query finds.
const ROWS: [&str; 4] = ["first line", "hello world", "other text", "hello again"];

fn lines_of(matches: &[SearchMatch]) -> Vec<usize> {
    let mut lines: Vec<usize> = matches.iter().map(|m| m.line).collect();
    lines.dedup();
    lines
}

/// Every query shape, at windows below, straddling and far past `u32::MAX`,
/// through the terminal-facing API the find bar and `ctl search` use.
#[test]
fn rows_past_u32_max_are_found_at_their_own_line_numbers() {
    for base in [
        0,
        TWO_32 - 100,
        // Straddles: rows 2^32-2, 2^32-1 (= u32::MAX), 2^32 and 2^32+1.
        TWO_32 - 2,
        TWO_32,
        TWO_32 + 1_000,
        1 << 40,
    ] {
        let mut search = TerminalSearch::new();
        search.index_visible_content(base, ROWS);
        let hello = vec![base + 1, base + 3];
        for direction in [SearchDirection::Forward, SearchDirection::Backward] {
            for (query, case_sensitive, is_regex, want) in [
                ("hello", true, false, hello.clone()),
                ("HELLO", false, false, hello.clone()),
                ("hel+o", true, true, hello.clone()),
                ("^hello", false, true, hello.clone()),
                // Two bytes: the range scan, not the trigram postings.
                ("h", false, false, vec![base + 1, base + 2, base + 3]),
                ("lo", true, false, hello.clone()),
            ] {
                let got = search
                    .search_results_opts_direction(query, case_sensitive, is_regex, direction)
                    .expect("valid query");
                assert_eq!(
                    lines_of(&got.matches),
                    want,
                    "base={base} {query:?} case={case_sensitive} regex={is_regex} {direction:?}"
                );
                assert!(
                    !got.incomplete,
                    "base={base} {query:?}: nothing was evicted"
                );
            }
        }
        assert_eq!(lines_of(&search.search("hello")), hello, "base={base}");

        // Point navigation, both modes and both directions, wrap included.
        for is_regex in [false, true] {
            let next = search
                .find_direction_opts(
                    "hello",
                    true,
                    is_regex,
                    DirectedFind {
                        anchor: (base + 1, 0),
                        direction: SearchDirection::Forward,
                        inclusive: false,
                        wrap: true,
                    },
                )
                .expect("valid query")
                .map(|m| m.line);
            assert_eq!(next, Some(base + 3), "base={base} regex={is_regex} next");
            let wrapped = search
                .find_direction_opts(
                    "hello",
                    true,
                    is_regex,
                    DirectedFind {
                        anchor: (base + 1, 0),
                        direction: SearchDirection::Backward,
                        inclusive: false,
                        wrap: true,
                    },
                )
                .expect("valid query")
                .map(|m| m.line);
            assert_eq!(
                wrapped,
                Some(base + 3),
                "base={base} regex={is_regex} previous wraps to the newest"
            );
        }

        // A narrowing frame carries the lines out and back in.
        let seed = search.search_literal_narrowed("hel", false, None);
        let frame = seed.occurrence_lines.expect("uncapped walk yields a frame");
        assert_eq!(frame.lines().collect::<Vec<_>>(), hello, "base={base}");
        assert!(frame.contains(base + 3) && !frame.contains(base + 2));
        let grown = search.search_literal_narrowed("hello", false, Some(&frame));
        assert_eq!(lines_of(&grown.results.matches), hello, "base={base}");
    }
}

/// A frame names lines, not keys: one handed out before the index re-keyed
/// itself still finds the same lines afterwards.
#[test]
fn a_frame_survives_the_index_rekeying_under_it() {
    let base = TWO_32 + 7;
    let mut index = SearchIndex::new();
    index.index_line(base + 10, "needle ten");
    index.index_line(base + 11, "nothing");
    let frame = index
        .search_literal_narrowed("need", true, None)
        .occurrence_lines
        .expect("frame");
    // An older line moves the window down: every key changes.
    index.index_line(base, "needle zero");
    assert_ne!(index.key_base, base + 10, "the window moved");
    let narrowed = index.search_literal_narrowed("needle", true, Some(&frame));
    assert_eq!(lines_of(&narrowed.results.matches), vec![base + 10]);
    let all = index.search_literal_narrowed("needle", true, None);
    assert_eq!(lines_of(&all.results.matches), vec![base, base + 10]);
}

/// The window follows output that runs on past `MAX_KEY_SPAN` rows: the
/// cached lines still inside the moved window keep being found, nothing is
/// reported lost, and the next rows append without moving it again.
#[test]
fn an_index_kept_across_four_billion_rows_rekeys_and_stays_complete() {
    let start = 10usize;
    let top = start + MAX_KEY_SPAN;
    let mut index = SearchIndex::new();
    index.index_line(start, "old row");
    // The top of the window, then the terminal stops retaining the old row
    // (an incremental refresh): a dense window, keyed from far below it.
    for line in top - 2..=top {
        index.index_line(line, &format!("kept needle {line}"));
    }
    index.drop_history_below(top - 2);
    for line in top + 1..=top + 3 {
        index.index_line(line, &format!("new needle {line}"));
    }
    assert_eq!(
        index.key_base,
        top - 2,
        "re-keyed at the oldest cached line"
    );
    index.index_line(top + 4, "newer needle");
    assert_eq!(
        index.key_base,
        top - 2,
        "appends fit without moving it again"
    );
    let want: Vec<usize> = (top - 2..=top + 4).collect();
    for (query, is_regex) in [("needle", false), ("ne+dle", true), ("n", false)] {
        let got = index
            .search_results_opts(query, true, is_regex)
            .expect("valid query");
        assert_eq!(lines_of(&got.matches), want, "{query:?}");
        assert!(!got.incomplete, "{query:?}: nothing fell out of the window");
    }
}

/// Lines the moved window cannot hold are evicted the way the cache cap
/// evicts: gone, and the results say so from exactly the window's floor.
///
/// Literal (trigram) queries only: these windows are sparse on purpose, and
/// the regex and two-byte paths walk the whole numeric range between the
/// oldest and newest cached line.
#[test]
fn lines_that_cannot_share_the_newest_lines_window_are_reported_evicted() {
    let mut index = SearchIndex::new();
    index.index_line(5, "ancient needle");
    let far = 5 + TWO_32 + 100;
    index.index_line(far, "fresh needle");
    let floor = far - MAX_KEY_SPAN;
    let got = index
        .search_results_opts("needle", true, false)
        .expect("literal");
    assert_eq!(lines_of(&got.matches), vec![far]);
    assert!(
        got.incomplete,
        "the ancient line is gone, and the result says so"
    );
    assert_eq!(got.lowest_retained_line, floor);

    // An older line than the window can reach is refused the same way, and a
    // line inside it moves the window down rather than being lost.
    index.index_line(floor - 1, "too old needle");
    index.index_line(floor + 1, "old enough needle");
    for case_sensitive in [true, false] {
        let got = index
            .search_results_opts("needle", case_sensitive, false)
            .expect("literal");
        assert_eq!(lines_of(&got.matches), vec![floor + 1, far]);
        assert_eq!(got.lowest_retained_line, floor);
    }
}

/// Lines indexed newest-first move the window once, not once per line.
#[test]
fn a_descending_run_of_lines_moves_the_window_once() {
    let top = TWO_32 + 50;
    let mut index = SearchIndex::new();
    index.index_line(top, "row needle");
    index.index_line(top - 1, "row needle");
    let base = index.key_base;
    assert_eq!(base, top - MAX_KEY_SPAN, "as low as the newest line allows");
    for line in (top - 40..top - 1).rev() {
        index.index_line(line, "row needle");
    }
    assert_eq!(index.key_base, base);
    let got = index
        .search_results_opts("needle", true, false)
        .expect("literal");
    assert_eq!(lines_of(&got.matches), (top - 40..=top).collect::<Vec<_>>());
    assert!(!got.incomplete);
}
