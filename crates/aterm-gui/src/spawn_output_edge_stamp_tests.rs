// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::stamp_output_arrival;
use std::sync::atomic::{AtomicU64, Ordering};

#[test]
fn first_unpresented_edge_wins_and_a_present_reopens_it() {
    let stamp = AtomicU64::new(0);
    stamp_output_arrival(&stamp, 10);
    stamp_output_arrival(&stamp, 20);
    stamp_output_arrival(&stamp, 30);
    assert_eq!(
        stamp.load(Ordering::Relaxed),
        10,
        "a multi-batch burst is measured from where the terminal first fell behind"
    );
    assert_eq!(stamp.swap(0, Ordering::Relaxed), 10);
    stamp_output_arrival(&stamp, 40);
    assert_eq!(
        stamp.load(Ordering::Relaxed),
        40,
        "the next edge arms freely once a present has booked the previous one"
    );
}
