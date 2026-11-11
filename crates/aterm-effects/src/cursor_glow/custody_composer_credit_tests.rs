// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_partly_paid_wide_press_never_becomes_an_exact_narrow_glyph() {
    let at = Instant::now();
    let before = at - Duration::from_millis(1);
    let mut keys = PressCredits::default();
    keys.bank(at, 2, Some('界'));
    assert!(keys.exact_echo_run(at, before, &['界', '\0']).is_some());
    keys.retire_newest_cells(1);
    assert!(!keys.fresh_glyph(at, TYPE_HINT_FRESH_DUR, '界'));
    assert_eq!(keys.oldest_fresh_glyph(at, TYPE_HINT_FRESH_DUR), None);
    assert_eq!(keys.exact_echo_run(at, before, &['界']), None);
    assert_eq!(keys.exact_echo_run(at, before, &['界', '\0']), None);
}

#[test]
fn split_and_merge_keep_the_wide_identity_with_its_press() {
    let at = Instant::now();
    let before = at - Duration::from_millis(1);
    for park in [false, true] {
        for occupied_destination in [false, true] {
            let mut source = PressCredits::default();
            source.bank(at, 2, Some('語'));
            let later = if park {
                source.split_off_for_park(before, 0)
            } else {
                source.split_off_after(before)
            };
            let mut destination = PressCredits::default();
            if occupied_destination {
                destination.bank(at, 2, Some('界'));
                destination.retire_newest();
                destination.bank(at, 1, Some('x'));
            }
            destination.merge(later);
            assert!(
                destination
                    .exact_echo_run(at, before, &['語', '\0'])
                    .is_some()
            );
            assert_eq!(destination.exact_echo_run(at, before, &['界', '\0']), None);
            assert_eq!(destination.exact_echo_run(at, before, &['語']), None);
            assert!(source.exact_echo_run(at, before, &['語', '\0']).is_none());
        }
    }
}

#[test]
fn slot_reuse_and_partial_spending_do_not_reinterpret_wide_identity() {
    let at = Instant::now();
    let before = at - Duration::from_millis(1);
    let mut keys = PressCredits::default();
    keys.bank(at, 2, Some('界'));
    for _ in 0..TYPED_STAMP_DEPTH {
        keys.bank(at, 1, Some('x'));
    }
    assert_eq!(keys.exact_echo_run(at, before, &['界', '\0']), None);
    assert!(keys.exact_echo_run(at, before, &['x']).is_some());
    keys.forget();
    keys.bank(at, 2, Some('語'));
    keys.spend_counting(at, 1);
    assert_eq!(keys.exact_echo_run(at, before, &['語', '\0']), None);
    assert_eq!(keys.exact_echo_run(at, before, &['語']), None);
    assert_eq!(keys.oldest_fresh_glyph(at, TYPE_HINT_FRESH_DUR), None);
}
