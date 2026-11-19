// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The judgement behind a [`crate::patience::Patience`], driven at instants and readings the test
//! names: no process is read and nothing sleeps.

use std::cell::Cell;
use std::time::{Duration, Instant};

use crate::patience::{Judge, Spent};

/// The window every judgement here names a hang after; its tick is 500 ms.
const WINDOW: Duration = Duration::from_secs(2);

/// A judgement driven at instants counted from its start.
struct Driven {
    judge: Judge,
    began: Instant,
}

impl Driven {
    fn new() -> Self {
        let began = Instant::now();
        Self {
            judge: Judge::new(WINDOW, began),
            began,
        }
    }

    /// The judgement `ms` after the start, where a reading then gives `reading`, and whether it
    /// took one.
    fn at(&mut self, ms: u64, reading: Option<u64>) -> (Option<Spent>, bool) {
        let read = Cell::new(false);
        let spent = self.judge.at(self.began + Duration::from_millis(ms), || {
            read.set(true);
            reading
        });
        (spent, read.get())
    }
}

#[test]
fn a_reading_that_never_changes_is_stalled_at_the_first_reading_a_window_on_and_not_before() {
    let mut wait = Driven::new();
    assert_eq!(
        wait.at(0, Some(7)),
        (None, false),
        "no reading at the start"
    );
    assert_eq!(
        wait.at(499, Some(7)),
        (None, false),
        "nor within its first tick"
    );
    assert_eq!(
        wait.at(500, Some(7)),
        (None, true),
        "the first opens the window"
    );
    assert_eq!(wait.at(999, Some(7)), (None, false), "at most one a tick");
    for ms in [1100, 1700, 2300] {
        assert_eq!(
            wait.at(ms, Some(7)),
            (None, true),
            "{ms} ms: within the window"
        );
    }
    assert_eq!(
        wait.at(2600, Some(7)),
        (None, false),
        "a window on, but no reading was due to show it"
    );
    assert_eq!(
        wait.at(2800, Some(7)),
        (Some(Spent::Stalled(WINDOW)), true),
        "the first reading a window after the window opened"
    );
}

#[test]
fn a_reading_that_keeps_changing_keeps_the_window_open() {
    let mut wait = Driven::new();
    for (generation, ms) in (500..=40_000).step_by(500).enumerate() {
        let generation = u64::try_from(generation).expect("a small generation");
        assert_eq!(
            wait.at(ms, Some(generation)),
            (None, true),
            "{ms} ms: the tree advanced at every reading"
        );
    }
}

#[test]
fn an_unread_reading_opens_the_window_again() {
    let mut wait = Driven::new();
    for ms in [500, 1000, 1500] {
        assert_eq!(wait.at(ms, Some(7)), (None, true), "{ms} ms");
    }
    assert_eq!(wait.at(2000, None), (None, true), "unread: no evidence");
    for ms in [2500, 3000, 3500, 4000] {
        assert_eq!(
            wait.at(ms, Some(7)),
            (None, true),
            "{ms} ms: the window opened again at the unread reading and at the next"
        );
    }
    assert_eq!(
        wait.at(4500, Some(7)),
        (Some(Spent::Stalled(WINDOW)), true),
        "a window after the reading that followed the unread one"
    );
}

#[test]
fn the_guard_ends_a_wait_whose_tree_keeps_working() {
    let guard = aterm_spec::wait::WORKING_GUARD;
    let guard_ms = u64::try_from(guard.as_millis()).expect("a guard in milliseconds");
    let mut wait = Driven::new();
    let mut generation = 0;
    for ms in (500..guard_ms).step_by(500) {
        generation += 1;
        assert_eq!(wait.at(ms, Some(generation)), (None, true), "{ms} ms");
    }
    assert_eq!(
        wait.at(guard_ms, Some(generation + 1)),
        (Some(Spent::Guard(guard)), true),
        "the guard, though the tree still works"
    );
}
