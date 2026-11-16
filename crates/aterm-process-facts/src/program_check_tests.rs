// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const D: Daemon = Daemon {
    pid: 690,
    start_s: 1_791_000_000,
    start_us: 1,
};

fn reading(daemon: Daemon, workers: u64, at_limit: bool) -> Reading {
    Reading {
        daemon,
        workers,
        at_limit,
        limit: Some(90),
    }
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
fn count_backlog_needs_the_single_read_threshold_or_two_positive_readings() {
    for (count, before, expected) in [
        (0, None, false),
        (0, Some(3), false),
        (1, None, false),
        (1, Some(0), false),
        (1, Some(1), true),
        (1, Some(99), true),
        (2, None, false),
        (2, Some(1), true),
        (3, None, true),
        (3, Some(0), true),
        (usize::MAX, None, true),
    ] {
        assert_eq!(
            held_count_backed_up(count, before),
            expected,
            "{count}, {before:?}"
        );
    }
}
