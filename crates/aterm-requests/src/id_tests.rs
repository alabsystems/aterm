// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// 2026-11-09 14:11:00 UTC.
const AT: u64 = 1_794_233_460_000;

#[test]
fn an_id_names_its_utc_minute_and_requester_and_counts_within_them() {
    let head = head_of("87be5228c0ffee0087be5228c0ffee00");
    assert_eq!(head, "87be5228");
    assert_eq!(prefix(AT, &head), "r-20261109-1411-87be5228-");
    assert_eq!(nth(&prefix(AT, &head), 1), "r-20261109-1411-87be5228-01");
    assert_eq!(nth(&prefix(AT, &head), 100), "r-20261109-1411-87be5228-100");
    // The last millisecond of a UTC day, and the first of the next, name different dates.
    let midnight = 1_767_225_600_000; // 2026-01-01T00:00:00Z
    assert_eq!(prefix(midnight - 1, "local"), "r-20251231-2359-local-");
    assert_eq!(prefix(midnight, "local"), "r-20260101-0000-local-");
    // A Mac with no node id names itself `local`; a mesh node's `n-` keeps its letters only.
    assert_eq!(head_of(""), "local");
    assert_eq!(head_of("n-87be5228c0ffee00"), "n87be522");
    for id in [
        nth(&prefix(AT, &head), 1),
        nth(&prefix(midnight - 1, "local"), 7),
        nth(&prefix(AT, &head_of("n-87be5228c0ffee00")), 12),
    ] {
        assert_eq!(aterm_types::control_verbs::request_id(&id), Ok(id.as_str()));
    }
}

#[test]
fn the_counter_restarts_each_minute_and_per_requester_head() {
    let one = prefix(AT, "87be5228");
    let next_minute = prefix(AT + 60_000, "87be5228");
    let other = prefix(AT, "local");
    assert_ne!(one, next_minute);
    assert_ne!(one, other);
    assert_eq!(count_of(&nth(&one, 3), &one), Some(3));
    assert_eq!(count_of(&nth(&one, 3), &next_minute), None);
    assert_eq!(count_of(&nth(&one, 3), &other), None);
    assert_eq!(count_of(&format!("{one}1x"), &one), None);
}
