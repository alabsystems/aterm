// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const D: Daemon = Daemon {
    pid: 690,
    start_s: 1_791_000_000,
    start_us: 1,
};
const MIN: u64 = 60_000;

fn reading(daemon: Daemon, workers: u64, at_limit: bool) -> Reading {
    Reading {
        daemon,
        workers,
        at_limit,
        limit: Some(90),
    }
}

fn run(readings: &[(u64, Option<Reading>)]) -> Vec<State> {
    let mut judge = Judge::new();
    readings
        .iter()
        .map(|(at, r)| judge.absorb(*at, r.as_ref()))
        .collect()
}

#[test]
fn daemon_started_us_is_microseconds_since_the_epoch() {
    assert_eq!(D.started_us(), 1_791_000_000_000_001);
    for (start_s, start_us) in [(u64::MAX, 0), (u64::MAX / 1_000_000, 999_999)] {
        let late = Daemon {
            start_s,
            start_us,
            ..D
        };
        assert_eq!(late.started_us(), u64::MAX, "saturates: {late:?}");
    }
}

#[test]
fn a_pool_full_for_five_minutes_is_stuck_and_one_below_it_is_not() {
    let full = Some(reading(D, 95, true));
    let states = run(&[
        (0, Some(reading(D, 6, false))),
        (MIN, full),
        (2 * MIN, full),
        (6 * MIN, full),
        (7 * MIN, full),
    ]);
    assert_eq!(
        states,
        [
            State::Answering,
            State::Full { since_ms: MIN },
            State::Full { since_ms: MIN },
            State::Stuck { since_ms: MIN },
            State::Stuck { since_ms: MIN },
        ]
    );
    let busy = Some(reading(D, 40, false));
    let states = run(&[(MIN, busy), (2 * MIN, busy), (6 * MIN, busy)]);
    assert!(states.iter().all(|s| *s == State::Answering), "{states:?}");
}

#[test]
fn a_full_pool_that_drains_starts_over() {
    let full = Some(reading(D, 90, true));
    let states = run(&[
        (0, full),
        (4 * MIN, full),
        (5 * MIN - 1, Some(reading(D, 3, false))),
        (6 * MIN, full),
        (10 * MIN, full),
    ]);
    assert_eq!(
        states,
        [
            State::Full { since_ms: 0 },
            State::Full { since_ms: 0 },
            State::Answering,
            State::Full { since_ms: 6 * MIN },
            State::Full { since_ms: 6 * MIN },
        ]
    );
}

#[test]
fn a_new_syspolicyd_answers_and_a_missing_reading_changes_nothing() {
    let full = Some(reading(D, 95, true));
    let again = Daemon {
        pid: 489,
        start_s: 1_792_597_850,
        start_us: 7,
    };
    let states = run(&[
        (0, full),
        (5 * MIN, full),
        (6 * MIN, None),
        (7 * MIN, Some(reading(again, 95, true))),
        (8 * MIN, Some(reading(again, 4, false))),
    ]);
    assert_eq!(
        states,
        [
            State::Full { since_ms: 0 },
            State::Stuck { since_ms: 0 },
            State::Stuck { since_ms: 0 },
            State::Full { since_ms: 7 * MIN },
            State::Answering,
        ]
    );
    assert_eq!(Judge::new().absorb(0, None), State::Unread);
    let mut judge = Judge::new();
    judge.absorb(0, full.as_ref());
    judge.absorb(5 * MIN, full.as_ref());
    assert_eq!(judge.absorb_none_running(), State::Answering);
    assert_eq!(
        judge.absorb(6 * MIN, full.as_ref()),
        State::Full { since_ms: 6 * MIN }
    );
}

#[test]
fn the_kernel_flag_alone_reads_full() {
    let flagged = Reading {
        daemon: D,
        workers: 12,
        at_limit: true,
        limit: None,
    };
    assert!(flagged.full());
    assert!(
        !Reading {
            at_limit: false,
            ..flagged
        }
        .full()
    );
    assert!(!reading(D, 90, false).full());
    assert!(!reading(D, 100, false).full());
    assert!(reading(D, 95, true).full());
}

#[test]
fn the_status_words() {
    assert_eq!(State::Unread.as_str(), "-");
    assert_eq!(State::Answering.as_str(), "ok");
    assert_eq!(State::Full { since_ms: 1 }.as_str(), "full");
    assert_eq!(State::Stuck { since_ms: 1 }.as_str(), "stuck");
    assert_eq!(State::Stuck { since_ms: 9 }.since_ms(), Some(9));
    assert_eq!(State::Answering.since_ms(), None);
}
