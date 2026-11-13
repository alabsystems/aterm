// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::transition_window_gauge;
use std::sync::atomic::{AtomicU64, Ordering};

#[test]
fn independent_window_leases_are_idempotent_and_release_on_drop_transition() {
    let count = AtomicU64::new(0);
    let mut a = false;
    let mut b = false;

    transition_window_gauge(&mut a, true, &count);
    transition_window_gauge(&mut a, true, &count);
    assert_eq!(count.load(Ordering::Relaxed), 1);

    transition_window_gauge(&mut b, true, &count);
    assert_eq!(count.load(Ordering::Relaxed), 2);

    transition_window_gauge(&mut a, false, &count);
    transition_window_gauge(&mut a, false, &count);
    assert_eq!(
        count.load(Ordering::Relaxed),
        1,
        "one window releasing cannot clear its held sibling"
    );

    transition_window_gauge(&mut b, false, &count);
    assert_eq!(count.load(Ordering::Relaxed), 0);
}
