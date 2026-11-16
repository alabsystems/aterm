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

/// A version-2 bound reads back, and only exactly it does: a format past 1, every number a
/// positive decimal with no leading zero that fits its field.
///
/// NEGATIVE CONTROL (in the test): each near miss (format 1 or 0 in version 2, a version-1
/// bound with a capacity, a leading zero, a capacity past `u64`, a missing or extra word) is no
/// bound.
#[test]
fn vault_fence_a_version_two_bound_reads_back_and_nothing_near_it_does() {
    for bound in [
        RecordsBound::of(4_096, false).at_format(FORMAT_TWO),
        RecordsBound::of(16_384, true).at_format(7),
        RecordsBound {
            bytes: u64::MAX,
            ..RecordsBound::of(1, true).at_format(u32::MAX)
        },
    ] {
        assert_eq!(RecordsBound::parse(&bound.value()), Some(bound));
    }
    assert_eq!(
        RecordsBound::of(4_096, true).at_format(FORMAT_TWO).value(),
        format!("2 4096 {FORMAT_ONE_BYTES} 2 crossed")
    );
    for near in [
        "2 4096 67108864 1 crossed",
        "2 4096 67108864 0 crossed",
        "2 4096 67108864 02 crossed",
        "2 4096 067108864 2 crossed",
        "2 4096 0 2 crossed",
        "2 4096 18446744073709551616 2 crossed",
        "2 4096 67108864 4294967296 crossed",
        "2 4096 67108864 crossed",
        "2 4096 67108864 2 crossed extra",
        "2 4096 67108864 2  crossed",
        "1 4096 67108864 crossed",
        "1 4096 67108864 2 crossed",
        "3 4096 67108864 2 crossed",
    ] {
        assert_eq!(RecordsBound::parse(near), None, "{near:?}");
    }
}

/// Two stores of the format mark at once never lower it: each reads the mark and writes under
/// one lock, so the store of the lower format finds the higher one there, or is overwritten by it.
///
/// NEGATIVE CONTROL: with the read and the write unlocked (as first built), the mark ended at 2
/// after stores of 3 and 2 at once in round 0 (`left: 2, right: 3`); the wire review's probe saw
/// it lowered in 1468 of 3000 rounds.
#[cfg(unix)]
#[test]
fn vault_fence_two_format_stores_at_once_never_lower_the_mark() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    for round in 0..200 {
        let mark = FormatMark::at(root.path().join(format!("records-{round}.format")));
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            for format in [3, FORMAT_TWO] {
                let (mark, barrier) = (&mark, &barrier);
                scope.spawn(move || {
                    barrier.wait();
                    mark.store(format).expect("stored");
                });
            }
        });
        assert_eq!(mark.stored().expect("readable"), 3, "round {round}");
    }
}
