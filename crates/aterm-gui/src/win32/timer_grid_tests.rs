// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{TimerGrid, high_resolution_timer_available, timer_grid};

#[test]
fn the_machine_s_tick_is_raised_only_where_the_loop_has_no_timer_of_its_own() {
    assert_eq!(timer_grid(true), TimerGrid::LoopTimer);
    assert_eq!(timer_grid(false), TimerGrid::Period);
}

/// Every Windows a build runs on (10 1803 and later) makes the loop's timer, so no
/// test host asks the machine for a 1 ms tick.
#[test]
fn this_windows_makes_the_loop_s_timer() {
    assert!(high_resolution_timer_available());
    assert_eq!(
        timer_grid(high_resolution_timer_available()),
        TimerGrid::LoopTimer
    );
}
