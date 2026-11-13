// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn required_window_start_pumps_after_scheduling_delay() {
    for (age, expected) in [(21, true), (61, false)] {
        let started = std::time::Instant::now() - std::time::Duration::from_secs(age);
        let mut created = false;
        pump_window_start(started, || {
            created = true;
            false
        });
        assert_eq!(
            created, expected,
            "only a live startup pumps the required creation"
        );
    }
}
