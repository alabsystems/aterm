// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn concurrent_rows_are_complete_and_storage_bound_is_explicit() {
    let root = crate::test_fixtures::scratch_dir("rate-usage");
    let book = RateBook::at(root.path().join("rate"));
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
        .open(root.path().join("rate/calls/0.jsonl"))
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
    let book = RateBook::at(root.path().join("rate"));
    for hour in 0..48 {
        book.record_call(hour * 3600, &aterm_json::json!({"at":hour * 3600}))
            .unwrap();
    }
    assert!(book.calls_in_hour(23).is_err());
    for hour in 24..48 {
        assert_eq!(book.calls_in_hour(hour).unwrap().len(), 1);
    }
    assert_eq!(
        std::fs::read_dir(root.path().join("rate/calls"))
            .unwrap()
            .count(),
        25
    );
}
