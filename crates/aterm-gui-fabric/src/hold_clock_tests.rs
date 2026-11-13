// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::time::Duration;

#[test]
fn only_aggregate_edges_change_history_and_only_release_changes_its_clock() {
    let now = Instant::now();
    let later = now + Duration::from_secs(1);
    let mut history = HoldHistory::default();
    let clear = history.observation(false);
    assert!(clear.agrees_unheld(clear));
    assert_eq!(clear.released_at(), None);

    history.moved(false, false, now);
    assert_eq!(history.observation(false), clear);
    history.moved(false, true, now);
    let held = history.observation(true);
    assert!(held.is_held());
    assert_eq!(held.released_at(), None);
    assert!(!held.agrees_unheld(held));
    history.moved(true, true, later);
    assert_eq!(history.observation(true), held);

    history.moved(true, false, later);
    let released = history.observation(false);
    assert_eq!(released.released_at(), Some(later));
    assert!(released.agrees_unheld(released));
    assert!(!released.agrees_unheld(clear));
    history.moved(false, false, later + Duration::from_secs(1));
    assert_eq!(history.observation(false), released);
    history.moved(false, true, later + Duration::from_secs(2));
    assert_eq!(history.observation(true).released_at(), Some(later));
}

#[test]
fn equal_release_timestamps_cannot_make_a_stale_observation_current() {
    let now = Instant::now();
    let mut history = HoldHistory::default();
    history.moved(false, true, now);
    history.moved(true, false, now);
    let first = history.observation(false);

    history.moved(false, true, now);
    history.moved(true, false, now);
    let second = history.observation(false);
    assert_eq!(first.released_at(), second.released_at());
    assert_ne!(
        first, second,
        "the clock alone cannot identify a hold episode"
    );
    assert!(!second.agrees_unheld(first));
    assert!(second.agrees_unheld(second));
}

#[test]
fn an_exhausted_generation_never_wraps_or_proves_continuity_again() {
    let now = Instant::now();
    let mut history = HoldHistory {
        generation: Some(u64::MAX),
        released_at: None,
    };
    history.moved(true, false, now);
    let exhausted = history.observation(false);
    assert_eq!(history.generation, None);
    assert_eq!(exhausted.released_at(), Some(now));
    assert!(!exhausted.agrees_unheld(exhausted));

    history.moved(false, true, now);
    history.moved(true, false, now + Duration::from_secs(1));
    let later = history.observation(false);
    assert_eq!(history.generation, None);
    assert!(!later.agrees_unheld(exhausted));
    assert!(!later.agrees_unheld(later));
}
