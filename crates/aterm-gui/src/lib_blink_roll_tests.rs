// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_stale_deadline_rolls_onto_the_grid_and_a_due_or_future_one_does_not() {
    let origin = Instant::now();
    assert_eq!(
        roll_stale_blink_forward(origin + BLINK_INTERVAL / 3, origin),
        None
    );
    assert_eq!(
        roll_stale_blink_forward(origin, origin + BLINK_INTERVAL / 2),
        None
    );
    for (late, next, flips) in [
        (BLINK_INTERVAL, 2, false),
        (BLINK_INTERVAL * 2 + BLINK_INTERVAL / 2, 3, true),
    ] {
        let roll = roll_stale_blink_forward(origin, origin + late).expect("stale");
        assert_eq!(roll.next, origin + BLINK_INTERVAL * next, "{late:?}");
        assert!(roll.next > origin + late, "{late:?}");
        assert_eq!(roll.phase_flips, flips, "{late:?}");
    }
}
