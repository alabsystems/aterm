// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_back_off_doubles_from_its_first_hold_to_its_ceiling() {
    assert_eq!(back_off(0), FIRST);
    assert_eq!(back_off(1), FIRST);
    assert_eq!(back_off(2), FIRST * 2);
    assert_eq!(back_off(6), FIRST * 32);
    assert_eq!(back_off(7), CEILING);
    assert_eq!(back_off(u32::MAX), CEILING);
}

#[test]
fn a_hold_holds_only_its_own_lane_until_its_end() {
    let t0 = Instant::now();
    let mut held = Unverified::default();
    assert_eq!(held.hold("s-a", Domain::Posts, 7, t0), (1, FIRST));
    assert!(held.holds("s-a", Domain::Posts, t0));
    assert!(!held.holds("s-b", Domain::Posts, t0), "another sender");
    assert!(
        !held.holds("s-a", Domain::Receipts, t0),
        "the same session's receipts"
    );
    assert_eq!(held.next_end(t0), Some(t0 + FIRST));
    assert!(!held.expire(t0), "not over yet");
    assert!(held.expire(t0 + FIRST), "its end is told once");
    assert!(!held.expire(t0 + FIRST));
    assert!(!held.holds("s-a", Domain::Posts, t0 + FIRST));
    assert_eq!(held.next_end(t0 + FIRST), None);
}

#[test]
fn the_same_pin_answered_again_waits_longer_and_another_pin_starts_over() {
    let t0 = Instant::now();
    let mut held = Unverified::default();
    held.hold("s-a", Domain::Posts, 7, t0);
    let t1 = t0 + FIRST;
    assert!(held.expire(t1));
    assert_eq!(held.hold("s-a", Domain::Posts, 7, t1), (2, FIRST * 2));
    assert_eq!(held.next_end(t1), Some(t1 + FIRST * 2));
    let mut at = t1;
    for _ in 0..8 {
        at += CEILING;
        held.hold("s-a", Domain::Posts, 7, at);
    }
    assert_eq!(held.next_end(at), Some(at + CEILING), "at most the ceiling");
    assert_eq!(held.hold("s-a", Domain::Posts, 9, at), (1, FIRST));
}

#[test]
fn a_whole_listing_forgets_the_lanes_it_no_longer_names() {
    let t0 = Instant::now();
    let mut held = Unverified::default();
    held.hold("s-a", Domain::Posts, 7, t0);
    held.hold("s-b", Domain::Receipts, 8, t0);
    held.retain_listed(|sid, domain| sid == "s-b" && domain == Domain::Receipts);
    assert!(!held.holds("s-a", Domain::Posts, t0));
    assert!(held.holds("s-b", Domain::Receipts, t0));
}
