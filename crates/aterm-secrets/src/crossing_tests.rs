// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn numeric_capacities_must_represent_each_others_possible_history() {
    let bound = RecordsBound::of;
    for (left, right, expected) in [
        (bound(4096, false), bound(8192, false), true),
        (bound(4096, false), bound(8192, true), false),
        (bound(4096, true), bound(8192, false), true),
        (bound(8192, true), bound(8192, true), true),
        (bound(8192, true), bound(16384, true), false),
        (bound(4096, true), bound(1000, false), false),
        (bound(1000, false), bound(1000, false), true),
        (bound(0, false), bound(4096, false), false),
    ] {
        assert_eq!(left.can_exchange(right), expected, "{left:?} / {right:?}");
        assert_eq!(right.can_exchange(left), expected, "{right:?} / {left:?}");
    }
}

/// The wire form reads back, and only exactly it does.
///
/// NEGATIVE CONTROL (in the test): each near miss (another version, a leading zero, a sign,
/// a doubled space, a trailing word, another state, a bound past `u32`) is no bound.
#[test]
fn a_records_bound_reads_back_and_nothing_near_it_does() {
    for bound in [
        RecordsBound::of(4_096, false),
        RecordsBound::of(16_384, true),
        RecordsBound::of(1, true),
        RecordsBound::of(u32::MAX as usize, false),
    ] {
        assert_eq!(RecordsBound::parse(&bound.value()), Some(bound));
    }
    assert_eq!(RecordsBound::of(usize::MAX, true).held, u32::MAX);
    assert_eq!(RecordsBound::of(4_096, true).value(), "1 4096 crossed");
    for near in [
        "",
        "1",
        "1 4096",
        "2 4096 crossed",
        "01 4096 crossed",
        "1 04096 crossed",
        "1 0 crossed",
        "1 +4096 crossed",
        "1 -4096 crossed",
        "1  4096 crossed",
        "1 4096  crossed",
        " 1 4096 crossed",
        "1 4096 crossed ",
        "1 4096 crossed extra",
        "1 4096 Crossed",
        "1 4096 open",
        "1 4294967296 crossed",
        "1 99999999999 crossed",
        "1\t4096\tcrossed",
        "1 4,096 crossed",
    ] {
        assert_eq!(RecordsBound::parse(near), None, "{near:?}");
    }
}

/// The line is the legacy bound itself: a log at it has not crossed, one record more has.
#[test]
fn a_log_crosses_one_record_past_the_legacy_bound() {
    assert!(!past_legacy(LEGACY_RECORDS));
    assert!(past_legacy(LEGACY_RECORDS + 1));
}

/// A mark is stored once, durably, and stays; one a test refuses is not stored.
///
/// NEGATIVE CONTROL (in the test): the refused write leaves nothing at the path.
#[cfg(unix)]
#[test]
fn a_mark_is_stored_and_stays() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let mark = Mark::at(root.path().join("records.crossed"));
    assert!(!mark.stored().expect("readable"));
    inject(
        mark.path(),
        Faults {
            fail_store: true,
            ..Faults::default()
        },
    );
    assert!(mark.store().is_err());
    assert!(
        !mark.stored().expect("readable"),
        "a refused write stores nothing"
    );
    inject(mark.path(), Faults::default());
    mark.store().expect("stored");
    mark.store().expect("stored again, as it was");
    assert!(mark.stored().expect("readable"));
    // Whatever is there counts: a mark cut short is still a mark.
    std::fs::write(mark.path(), b"").expect("truncated");
    assert!(mark.stored().expect("readable"));
}
