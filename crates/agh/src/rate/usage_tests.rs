// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The book at `<root>/agh/rate`, as `Dirs::rate` lays it out under a state root: the root need
/// not be private (a windows temporary folder is not), and the book makes the two below it so.
fn book_under(root: &std::path::Path) -> RateBook {
    RateBook::at(root.join("agh").join("rate"))
}

#[test]
fn concurrent_rows_are_complete_and_storage_bound_is_explicit() {
    let root = crate::test_fixtures::scratch_dir("rate-usage");
    let book = book_under(root.path());
    std::thread::scope(|scope| {
        for caller in 0..8 {
            let book = &book;
            scope.spawn(move || {
                for at in 0..16 {
                    book.record_call(at, &aterm_json::json!({"caller": caller, "at": at}))
                        .unwrap();
                }
            });
        }
    });
    let rows = book.calls_in_hour(0).unwrap();
    assert_eq!(rows.len(), 128);
    let distinct: std::collections::BTreeSet<_> = rows
        .iter()
        .map(|row| (row["caller"].as_u64().unwrap(), row["at"].as_u64().unwrap()))
        .collect();
    assert_eq!(distinct.len(), 128);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(root.path().join("agh/rate/calls/0.jsonl"))
        .unwrap();
    file.set_len(HOUR_MAX as u64).unwrap();
    let error = book
        .record_call(0, &aterm_json::json!({"at": 0}))
        .unwrap_err();
    assert!(
        error
            .why
            .contains(&format!("{HOUR_MAX}-byte machine-hour bound"))
    );
    assert!(
        book.calls_in_hour(0)
            .unwrap_err()
            .why
            .contains("incomplete")
    );
    // A full hour leaves the next hour writable.
    book.record_call(3600, &aterm_json::json!({"at": 3600}))
        .unwrap();
    assert_eq!(book.calls_in_hour(1).unwrap().len(), 1);
}

#[test]
fn retention_bounds_total_storage_and_preserves_the_full_recent_window() {
    let root = crate::test_fixtures::scratch_dir("rate-retention");
    let book = book_under(root.path());
    for hour in 0..48 {
        book.record_call(hour * 3600, &aterm_json::json!({"at":hour * 3600}))
            .unwrap();
    }
    assert!(book.calls_in_hour(23).is_err());
    for hour in 24..48 {
        assert_eq!(book.calls_in_hour(hour).unwrap().len(), 1);
    }
    assert_eq!(
        std::fs::read_dir(root.path().join("agh/rate/calls"))
            .unwrap()
            .count(),
        25
    );
}

/// What this book records under aterm's state root is what the fleet survey reads there
/// (`aterm_ctl::github_usage::survey`): the hour a call was recorded in, by its attempts.
/// NEGATIVE CONTROL (2026-11-17, on Windows): with `github_usage::calls_dir` naming
/// `<state>/agh/calls`, this failed, both hours `status=unrecorded`.
#[test]
fn the_fleet_survey_reads_the_hour_the_book_recorded_under_the_state_root() {
    let root = crate::test_fixtures::scratch_dir("rate-survey");
    // `Dirs::under(root)` lays agh out as `Dirs::from_env` does over the state root
    // `<root>/state`.
    let book = RateBook::at(crate::config::Dirs::under(root.path()).rate());
    book.record_call(
        7200,
        &aterm_json::json!({"as":"dev","caller":"cli","path":"/rate_limit","status":200}),
    )
    .unwrap();
    let survey = aterm_ctl::github_usage::survey(Some(&root.path().join("state")), 7200);
    assert!(
        survey
            .iter()
            .any(|line| line.starts_with("github_rate hour=2 status=recorded attempts=1 ")),
        "{survey:?}"
    );
}
