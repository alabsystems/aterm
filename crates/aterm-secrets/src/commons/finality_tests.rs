// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const DAY: u64 = 24 * 60 * 60 * 1000;

/// More than half of the recent seats is final, half is not, and seats silent for longer than
/// the window, or that signed only after the cut, do not count either way.
/// NEGATIVE CONTROL: with `recent` keeping every seat that signed before the cut (no 30-day
/// window), the dead seats 4-6 count and the store whose dead seats outnumber its live ones never
/// finalizes: the first assertion fails with `left: {1, 2, 3, 4, 5, 6}`.
#[test]
fn more_than_half_of_the_seats_heard_in_30_days_finalize() {
    let cut = 100 * DAY;
    let seats: BTreeSet<u8> = (1..=7).collect();
    // Seats 1-3 signed within the window; 4-6 last signed 40 days before the cut; 7 signed only
    // after it.
    let last = |seat: &u8| match seat {
        1..=3 => Some(cut - DAY * u64::from(*seat)),
        4..=6 => Some(cut - 40 * DAY),
        _ => None,
    };
    let heard = recent(&seats, cut, last);
    assert_eq!(heard, BTreeSet::from([1, 2, 3]));
    assert!(
        is_final(&heard, &BTreeSet::from([1, 2])),
        "two of three recent"
    );
    assert!(!is_final(&heard, &BTreeSet::from([1])), "one of three");
    // Shares from seats that are not recent count for nothing.
    assert!(!is_final(&heard, &BTreeSet::from([1, 4, 5, 6, 7])));
    let even: BTreeSet<u8> = BTreeSet::from([1, 2, 3, 4]);
    assert!(
        !is_final(&even, &BTreeSet::from([1, 2])),
        "half is not more"
    );
    assert!(!is_final(&BTreeSet::<u8>::new(), &BTreeSet::from([1])));
    // The window's edges: exactly 30 days before counts, a record after the cut does not.
    let edge = recent(&BTreeSet::from([1, 2]), cut, |seat| {
        Some(if *seat == 1 {
            cut - HEARD_WITHIN_MS
        } else {
            cut + 1
        })
    });
    assert_eq!(edge, BTreeSet::from([1]));
}
