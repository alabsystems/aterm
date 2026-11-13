// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{REFRESH_RATE_REREAD_INTERVAL, refresh_rate_read_due};
use std::time::{Duration, Instant};

#[test]
fn a_same_monitor_move_is_throttled_and_a_monitor_change_or_focus_never_is() {
    let now = Instant::now();
    let interval = REFRESH_RATE_REREAD_INTERVAL;
    let half = now + interval / 2;
    let after = now + interval + Duration::from_millis(1);
    for (case, same, forced, last, at, due) in [
        ("first moved", true, false, None, now, true),
        ("moved just read", true, false, Some(now), now, false),
        ("moved half in", true, false, Some(now), half, false),
        (
            "moved at the boundary",
            true,
            false,
            Some(now),
            now + interval,
            true,
        ),
        ("moved past it", true, false, Some(now), after, true),
        ("other monitor", false, false, Some(now), now, true),
        ("other monitor half in", false, false, Some(now), half, true),
        ("other monitor, never read", false, false, None, now, true),
        ("focused just read", true, true, Some(now), now, true),
        ("focused half in", true, true, Some(now), half, true),
    ] {
        assert_eq!(
            refresh_rate_read_due(same, forced, last, at, interval),
            due,
            "{case}"
        );
    }
}
