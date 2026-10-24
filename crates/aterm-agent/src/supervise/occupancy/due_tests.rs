// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The K clock, by tables of looks: when a due line raises, and what never moves its clock.

use super::*;
use crate::supervise::occupancy::fault::{self, Fault};

const LOOK: i64 = LOOK_EVERY_MS;

fn first_raise(
    clock: &mut DueClock<u8>,
    from: i64,
    to: i64,
    guard: impl Fn(i64) -> Option<Guard>,
) -> Option<i64> {
    for look in from..=to {
        let now = look * LOOK;
        clock.observe(&[(1, guard(look))], now);
        if !clock.sample(K_LOOKS, now).is_empty() {
            return Some(look);
        }
    }
    None
}

fn draft(_: i64) -> Option<Guard> {
    Some(Guard::Draft)
}

#[test]
fn a_due_line_raises_at_look_k_not_before() {
    let mut clock = DueClock::default();
    assert_eq!(first_raise(&mut clock, 0, 30, draft), Some(K_LOOKS));
    assert_eq!(clock.looks(&1, K_LOOKS * LOOK), Some(K_LOOKS));
    clock.observe(&[(1, None)], 12 * LOOK);
    assert_eq!(
        clock.sample(K_LOOKS, 12 * LOOK),
        [ReadyIdle {
            key: 1,
            guard: None,
            since_ms: 0,
        }],
        "it stands raised, naming the guard that holds it now"
    );
    let late = fault::with(Fault::LateRaise, || {
        first_raise(&mut DueClock::default(), 0, 30, draft)
    });
    assert_eq!(late, Some(K_LOOKS + 1));
}

#[test]
fn a_guard_that_changes_its_word_keeps_the_clock() {
    let flap = |look: i64| match look {
        0..=3 => Some(Guard::Draft),
        4..=6 => Some(Guard::Person),
        _ => Some(Guard::Draft),
    };
    let mut clock = DueClock::default();
    assert_eq!(first_raise(&mut clock, 0, 30, flap), Some(K_LOOKS));
    let due = clock.get(&1).expect("due");
    assert_eq!(
        (due.since_ms, due.first, due.last),
        (0, Some(Guard::Draft), Some(Guard::Draft))
    );
    let restarted = fault::with(Fault::KFromLastWord, || {
        first_raise(&mut DueClock::default(), 0, 30, flap)
    });
    assert!(
        restarted.is_some_and(|look| look > K_LOOKS),
        "{restarted:?}"
    );
}

#[test]
fn a_person_holds_the_raise_and_a_hand_over_past_k_raises_at_once() {
    let handed = |look: i64| {
        if look < 12 {
            Some(Guard::Person)
        } else {
            Some(Guard::Draft)
        }
    };
    let mut clock = DueClock::default();
    assert_eq!(first_raise(&mut clock, 0, 30, handed), Some(12));
    assert_eq!(clock.get(&1).map(|d| d.since_ms), Some(0));
    let person = |_: i64| Some(Guard::Person);
    assert_eq!(first_raise(&mut DueClock::default(), 0, 60, person), None);
    assert_eq!(
        first_raise(&mut DueClock::default(), 0, 30, draft),
        Some(K_LOOKS)
    );
}

#[test]
fn a_line_told_before_k_never_raises() {
    let mut clock = DueClock::default();
    assert_eq!(first_raise(&mut clock, 0, 8, draft), None);
    clock.observe(&[], 9 * LOOK);
    assert!(clock.sample(K_LOOKS, 9 * LOOK).is_empty());
    assert_eq!(clock.get(&1), None, "forgotten");
    assert_eq!(
        first_raise(&mut clock, 10, 19, draft),
        None,
        "its own clock"
    );
    assert_eq!(first_raise(&mut clock, 20, 20, draft), Some(20));
}

#[test]
fn a_seeded_successor_raises_on_time() {
    let succeed = || {
        let mut clock = DueClock::default();
        clock.seed(1, 0);
        clock.seed(1, 5 * LOOK);
        first_raise(&mut clock, K_LOOKS, 40, draft)
    };
    assert_eq!(succeed(), Some(K_LOOKS));
    assert_eq!(
        fault::with(Fault::KUnseeded, succeed),
        Some(2 * K_LOOKS),
        "ten looks late"
    );
}

#[test]
fn each_key_keeps_its_own_clock() {
    let mut clock: DueClock<u8> = DueClock::default();
    clock.seed(9, 0);
    clock.observe(&[(1, Some(Guard::Draft))], 0);
    clock.observe(
        &[(1, Some(Guard::Draft)), (2, Some(Guard::Memory))],
        5 * LOOK,
    );
    assert_eq!(clock.get(&9), None);
    let raised = clock.sample(K_LOOKS, 15 * LOOK);
    assert_eq!(
        raised
            .iter()
            .map(|r| (r.key, r.since_ms))
            .collect::<Vec<_>>(),
        [(1, 0), (2, 5 * LOOK)]
    );
    assert_eq!(
        clock.sample(K_LOOKS, 16 * LOOK).len(),
        2,
        "raised stays raised"
    );
}

/// A READ OF WHAT STANDS TAKES NO LOOK: `standing` names the keys a sample raised, with the guard
/// holding each now, and raises none itself, so a line past K looks unsampled stays unraised
/// until a look samples it. CONTROL: the sample at the same instant raises it.
#[test]
fn what_stands_is_read_without_a_look() {
    let mut clock: DueClock<u8> = DueClock::default();
    clock.observe(&[(1, Some(Guard::Draft))], 0);
    clock.observe(
        &[(1, Some(Guard::Draft)), (2, Some(Guard::Memory))],
        5 * LOOK,
    );
    assert_eq!(
        clock.sample(K_LOOKS, 10 * LOOK).len(),
        1,
        "key 1 at look 10"
    );
    clock.observe(
        &[(1, Some(Guard::Person)), (2, Some(Guard::Memory))],
        15 * LOOK,
    );
    let standing = [ReadyIdle {
        key: 1,
        guard: Some(Guard::Person),
        since_ms: 0,
    }];
    assert_eq!(clock.standing(), standing, "key 2, past K, unsampled");
    assert_eq!(clock.standing(), standing, "and a read again moves nothing");
    assert_eq!(
        clock
            .sample(K_LOOKS, 15 * LOOK)
            .iter()
            .map(|r| r.key)
            .collect::<Vec<_>>(),
        [1, 2]
    );
}

/// A CLOCK WITH NO K NEVER RAISES: [`K_NEVER`], what a tape recorded before the loop had a K
/// clock replays under, raises nothing however long a line stands due, a look late or not.
#[test]
fn a_clock_with_no_k_never_raises() {
    let never = || {
        let mut clock: DueClock<u8> = DueClock::default();
        clock.observe(&[(1, Some(Guard::Draft))], 0);
        clock.observe(&[(1, Some(Guard::Draft))], i64::MAX);
        clock.sample(K_NEVER, i64::MAX)
    };
    assert!(never().is_empty());
    assert!(fault::with(Fault::LateRaise, never).is_empty());
}
