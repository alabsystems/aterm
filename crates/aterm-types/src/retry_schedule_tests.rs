// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const MINUTE: Duration = Duration::from_secs(60);

#[test]
fn doubles_from_the_first_pause_up_to_the_longest() {
    let schedule = Backoff::new(MINUTE, 15 * MINUTE);
    let pauses: Vec<u64> = (0..7)
        .map(|tries| schedule.after(tries).as_secs() / 60)
        .collect();
    assert_eq!(pauses, [0, 1, 2, 4, 8, 15, 15]);
}

#[test]
fn never_overflows_into_a_stop() {
    let schedule = Backoff::new(MINUTE, 60 * MINUTE);
    assert_eq!(schedule.after(u32::MAX), 60 * MINUTE);
    assert_eq!(schedule.due(u64::MAX - 1, u32::MAX), u64::MAX);
}

#[test]
fn a_first_pause_past_the_longest_waits_the_longest() {
    let schedule = Backoff::new(60 * MINUTE, 15 * MINUTE);
    assert_eq!(schedule.after(1), 15 * MINUTE);
    assert_eq!(schedule.due(100, 1), 100 + 15 * 60);
}
