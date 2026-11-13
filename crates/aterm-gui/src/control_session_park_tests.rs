// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::{OnHangup, Park, Parked, Stretch};
use crate::park::Lane;
use crate::session_watch::{Subscription, Watchers};

static SLICES: AtomicUsize = AtomicUsize::new(0);

fn slice(left: Duration) -> Duration {
    SLICES.fetch_add(1, Ordering::SeqCst);
    left.min(Duration::from_millis(2))
}

fn there() -> bool {
    false
}

const LANE: Lane = Lane {
    slice,
    hung_up: there,
};

fn old_park(sub: &Subscription, until: Instant, mut look: impl FnMut() -> bool) -> usize {
    let mut looks = 0;
    loop {
        looks += 1;
        if look() || Instant::now() >= until {
            return looks;
        }
        let left = until
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1));
        let _ = sub.wait((LANE.slice)(left));
    }
}

#[test]
fn a_hangup_slice_does_not_rerun_the_predicate() {
    let watchers = std::sync::Arc::<Watchers>::default();
    let sub = Subscription::single(&watchers);
    let alive = || false;
    let mut park = Park::new(&sub, &alive, OnHangup::Abort, Stretch::None);
    park.lane = LANE;

    SLICES.store(0, Ordering::SeqCst);
    let mut looks = 0;
    let ended = park.run(
        Instant::now() + Duration::from_millis(100),
        |_, _, _| {
            looks += 1;
            (None::<()>, None)
        },
        || {},
    );
    assert!(matches!(ended, Parked::Deadline));
    let slices = SLICES.load(Ordering::SeqCst);
    assert_eq!(looks, 2, "entry and close, over {slices} slices");

    SLICES.store(0, Ordering::SeqCst);
    let looks = old_park(&sub, Instant::now() + Duration::from_millis(100), || false);
    let slices = SLICES.load(Ordering::SeqCst);
    assert_eq!(looks, slices + 1, "the old loop looked after every slice");
}
