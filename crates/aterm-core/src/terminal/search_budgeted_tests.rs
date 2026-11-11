// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::{MAX_MATCHES_PER_STEP, Terminal, allocate_cursor};
use crate::search::SearchResults;
use std::sync::atomic::AtomicU64;

fn one_shot(t: &mut Terminal, query: &str, case_sensitive: bool, is_regex: bool) -> SearchResults {
    t.indexed_search()
        .search_results_opts(query, case_sensitive, is_regex)
        .expect("one-shot search")
}

fn drive(
    t: &mut Terminal,
    query: &str,
    case_sensitive: bool,
    is_regex: bool,
    budget: usize,
) -> (SearchResults, usize) {
    let mut cursor = None;
    let mut steps = 0usize;
    let mut matches = Vec::new();
    let mut search_id = None;
    loop {
        let step = t
            .search_budgeted(query, case_sensitive, is_regex, cursor, budget)
            .expect("budgeted step");
        steps += 1;
        assert!(steps <= 10_000, "budgeted search must terminate");
        assert!(
            step.results.matches.len() <= MAX_MATCHES_PER_STEP,
            "one turn must bound result materialization"
        );
        let incomplete = step.results.incomplete;
        let lowest_retained_line = step.results.lowest_retained_line;
        let cut_short_lines = step.results.cut_short_lines;
        if let Some(existing) = search_id {
            assert!(!step.reset, "a valid resume must not reset the stream");
            assert_eq!(step.search_id, existing, "search identity is stable");
        } else {
            assert!(step.reset, "the first step starts a new result stream");
            search_id = Some(step.search_id);
        }
        matches.extend(step.results.matches);
        if step.complete {
            assert_eq!(step.cursor, None, "a complete step carries no cursor");
            return (
                SearchResults::new(matches, incomplete, lowest_retained_line)
                    .with_cut_short_lines(cut_short_lines),
                steps,
            );
        }
        assert!(step.cursor.is_some(), "an incomplete step carries a cursor");
        cursor = step.cursor;
    }
}

fn seeded_terminal() -> Terminal {
    let mut t = Terminal::new(6, 40);
    for i in 0..50 {
        t.process(format!("filler {i} with NEEDLE-{i} inside\r\n").as_bytes());
    }
    t.process(b"case Needle mixed\r\n");
    t
}

#[test]
fn budgeted_completion_equals_one_shot_over_a_corrupt_tiered_segment() {
    for budget in [1, 3, 7, 1_000] {
        let (mut t, _) = crate::terminal::search_index::tests::corrupt_tiered_terminal();
        let oracle = one_shot(&mut t, "NEEDLE_", true, false);
        assert_eq!(oracle.matches.len(), 2);
        let (got, _) = drive(&mut t, "NEEDLE_", true, false, budget);
        assert_eq!(got, oracle, "budget={budget}");
    }
}

#[test]
fn budgeted_completion_equals_one_shot_all_modes() {
    let cases: &[(&str, bool, bool)] = &[
        ("NEEDLE", true, false),
        ("needle", false, false),
        ("le", false, false),
        ("NEEDLE-[0-9]+", true, true),
        ("needle-1?7", false, true),
    ];
    for &(query, case_sensitive, is_regex) in cases {
        for budget in [1, 7, 1_000] {
            let mut t = seeded_terminal();
            let oracle = one_shot(&mut t, query, case_sensitive, is_regex);
            let (got, steps) = drive(&mut t, query, case_sensitive, is_regex, budget);
            assert_eq!(
                got, oracle,
                "query={query:?} cs={case_sensitive} rx={is_regex} budget={budget}"
            );
            let total = t.grid().scrollback_lines() + usize::from(t.rows());
            assert_eq!(
                steps,
                total.div_ceil(budget),
                "step count must reflect the row budget (budget={budget})"
            );
        }
    }
}

#[test]
fn invalid_regex_errors_and_keeps_no_state() {
    let mut t = seeded_terminal();
    assert!(t.search_budgeted("f(oo", false, true, None, 5).is_err());
    let (got, _) = drive(&mut t, "NEEDLE", true, false, 9);
    assert_eq!(got, one_shot(&mut t, "NEEDLE", true, false));
}

#[test]
fn an_unaffordable_regex_row_is_reported_not_passed_off() {
    for (pattern, row) in [
        ("(?:x?){1020}z", format!("{}z", "x".repeat(4095))),
        (r"\w+=|\w", "a".repeat(4096)),
    ] {
        for budget in [1, usize::MAX] {
            let mut t = Terminal::new(3, 4096);
            t.process(format!("before z\r\n{row}\r\nafter z").as_bytes());
            let oracle = t
                .indexed_search()
                .search_results_opts(pattern, true, true)
                .expect("a row out of budget is reported, not refused");
            assert_eq!(oracle.cut_short_lines, 1, "{pattern:?}: {oracle:?}");
            assert!(oracle.incomplete, "{pattern:?}");
            assert!(
                oracle.matches.iter().any(|m| m.start_col == 6),
                "{pattern:?}: the row after the long one is searched: {oracle:?}"
            );
            let (got, _) = drive(&mut t, pattern, true, true, budget);
            assert_eq!(got, oracle, "{pattern:?} budget={budget}");
        }
    }
}

#[test]
fn foreign_terminal_cursor_restarts_with_a_unique_token() {
    let mut a = seeded_terminal();
    let mut b = seeded_terminal();
    let a_first = a
        .search_budgeted("NEEDLE", true, false, None, 5)
        .expect("terminal a");
    let b_first = b
        .search_budgeted("NEEDLE", true, false, None, 5)
        .expect("terminal b");
    assert_ne!(a_first.cursor, b_first.cursor, "live tokens are unique");

    let restarted = b
        .search_budgeted("NEEDLE", true, false, a_first.cursor, 5)
        .expect("foreign cursor restarts");
    assert_eq!(restarted.rows_fed, 5, "foreign token starts at row zero");
    assert_ne!(restarted.cursor, a_first.cursor);
    assert_ne!(restarted.cursor, b_first.cursor);
    assert!(restarted.reset);
    assert_ne!(restarted.search_id, b_first.search_id);
}

#[test]
fn cursor_allocator_never_wraps() {
    let counter = AtomicU64::new(u64::MAX - 1);
    assert_eq!(allocate_cursor(&counter), Some(u64::MAX - 1));
    assert_eq!(allocate_cursor(&counter), None);
    assert_eq!(allocate_cursor(&counter), None);
}

#[test]
fn search_summary_reads_completed_index_with_zero_rebuild() {
    let mut t = seeded_terminal();
    let (driven, _steps) = drive(&mut t, "NEEDLE", true, false, 7);
    assert_eq!(
        t.search_index_rebuilds(),
        0,
        "the budgeted scan must not build the one-shot index"
    );

    let before = t.search_index_rebuilds();
    let summary = t
        .search_summary_results("NEEDLE", true, false)
        .expect("summary over retained index");
    assert_eq!(
        t.search_index_rebuilds(),
        before,
        "search_summary over a just-completed budgeted index must not rebuild"
    );
    assert_eq!(
        summary, driven,
        "the retained-index read equals the budgeted completion"
    );

    let oracle = one_shot(&mut t, "NEEDLE", true, false);
    assert_eq!(summary, oracle, "retained read equals the one-shot search");

    t.process(b"NEEDLE-appended after completion\r\n");
    let before_miss = t.search_index_rebuilds();
    let missed = t
        .search_summary_results("NEEDLE", true, false)
        .expect("summary after content change");
    assert!(
        t.search_index_rebuilds() > before_miss,
        "a retained-key miss must rebuild the one-shot index"
    );
    assert_eq!(
        missed,
        one_shot(&mut t, "NEEDLE", true, false),
        "the fallback result reflects the new content"
    );
}

#[test]
fn a_released_completed_index_is_rebuilt_by_the_next_summary() {
    let mut t = seeded_terminal();
    drive(&mut t, "NEEDLE", true, false, 7);
    t.release_search_index();
    let before = t.search_index_rebuilds();
    let after_release = t
        .search_summary_results("NEEDLE", true, false)
        .expect("summary after release");
    assert!(
        t.search_index_rebuilds() > before,
        "after release the summary must rebuild from the buffer"
    );
    assert_eq!(after_release, one_shot(&mut t, "NEEDLE", true, false));
}
