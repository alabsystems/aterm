// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::Cell;

const TTL: Duration = Duration::from_secs(60);
const SUSPEND_NS: u64 = 6 * 60 * 60 * 1_000_000_000;

#[test]
fn one_engine_processes_clock_counts_suspend_and_refuses_unknown_or_regression() {
    let instant = Instant::now();
    let observed = ObservedAt {
        instant,
        continuous_ns: Some(10_000_000_000),
    };
    let recent = ObservedAt {
        instant: instant + Duration::from_secs(1),
        continuous_ns: Some(11_000_000_000),
    };
    assert!(observed.within(recent, TTL));
    assert_eq!(observed.age_at(recent), Some(Duration::from_secs(1)));
    let resumed = ObservedAt {
        continuous_ns: Some(11_000_000_000 + SUSPEND_NS),
        ..recent
    };
    assert!(!observed.within(resumed, TTL));
    assert!(
        recent.instant.duration_since(observed.instant) < TTL,
        "the old ordering-clock-only rule would accept this six-hour-old input"
    );
    for now in [
        ObservedAt {
            continuous_ns: None,
            ..recent
        },
        ObservedAt {
            continuous_ns: Some(1),
            ..recent
        },
        ObservedAt {
            instant: instant - Duration::from_secs(1),
            ..recent
        },
    ] {
        assert_eq!(observed.age_at(now), None);
        assert!(!observed.within(now, TTL));
    }
    assert_eq!(
        ObservedAt {
            continuous_ns: None,
            ..observed
        }
        .age_at(recent),
        None
    );
}

thread_local! {
    static CLOCK: Cell<Option<(Instant, u64)>> = const { Cell::new(None) };
}

fn inclusive() -> Option<u64> {
    CLOCK.with(|clock| clock.get().map(|(_, ns)| ns))
}

fn suspend_at_instant() -> Instant {
    CLOCK.with(|clock| {
        let (instant, ns) = clock.get().unwrap();
        clock.set(Some((instant, ns + SUSPEND_NS)));
        instant
    })
}

#[test]
fn one_engine_processes_clock_brackets_suspend_at_both_read_boundaries() {
    let instant = Instant::now();
    CLOCK.with(|clock| clock.set(Some((instant, 0))));
    let original = ObservedAt::capture(suspend_at_instant, inclusive);
    let after = ObservedAt {
        instant,
        continuous_ns: inclusive(),
    };
    assert!(
        !original.within(after, TTL),
        "capture keeps the pre-suspend start"
    );

    CLOCK.with(|clock| clock.set(Some((instant, 0))));
    let original = ObservedAt {
        instant,
        continuous_ns: Some(0),
    };
    let now = ObservedAt::now(suspend_at_instant, inclusive);
    assert!(
        !original.within(now, TTL),
        "current age includes the inter-read suspend"
    );

    CLOCK.with(|clock| clock.set(Some((instant, 0))));
    let wrong_end = ObservedAt::capture(suspend_at_instant, inclusive);
    assert!(
        original.within(wrong_end, TTL),
        "using the start ordering for a current check is a false-fresh negative control"
    );
    CLOCK.with(|clock| clock.set(None));
}
