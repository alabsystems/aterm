// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{EDR_REQUERY_INTERVAL, edr_refresh_due, edr_requery_due};
use std::time::{Duration, Instant};

#[test]
fn requery_recurs_on_a_stationary_window() {
    let now = Instant::now();
    assert!(edr_requery_due(None, now, EDR_REQUERY_INTERVAL));
    assert!(!edr_requery_due(Some(now), now, EDR_REQUERY_INTERVAL));
    let half = now + EDR_REQUERY_INTERVAL / 2;
    assert!(!edr_requery_due(Some(now), half, EDR_REQUERY_INTERVAL));
    let after = now + EDR_REQUERY_INTERVAL + Duration::from_millis(1);
    assert!(edr_requery_due(Some(now), after, EDR_REQUERY_INTERVAL));
}

#[test]
fn an_hdr_surface_stamped_while_8bit_is_due_at_once() {
    let t = Instant::now();
    let next_frame = t + Duration::from_millis(16);
    assert!(
        edr_refresh_due(
            Some(true),
            Some((t, false)),
            next_frame,
            EDR_REQUERY_INTERVAL
        ),
        "an HDR surface stamped while 8-bit is re-queried at the next redraw"
    );
    assert!(
        !edr_requery_due(Some(t), next_frame, EDR_REQUERY_INTERVAL),
        "control: the time throttle alone holds that re-query off"
    );
}

#[test]
fn the_gate_is_the_throttle_except_for_an_unseeded_hdr_surface() {
    let t = Instant::now();
    let next_frame = t + Duration::from_millis(16);
    let elapsed = t + EDR_REQUERY_INTERVAL;
    for hdr in [false, true] {
        assert!(
            edr_refresh_due(Some(hdr), None, t, EDR_REQUERY_INTERVAL),
            "a never-stamped GPU surface (hdr={hdr}) is due"
        );
    }
    for (stamped_hdr, hdr) in [(false, false), (true, true), (true, false)] {
        let last = Some((t, stamped_hdr));
        assert!(
            !edr_refresh_due(Some(hdr), last, next_frame, EDR_REQUERY_INTERVAL),
            "stamped hdr={stamped_hdr}, now hdr={hdr}: held within the interval"
        );
        assert!(
            edr_refresh_due(Some(hdr), last, elapsed, EDR_REQUERY_INTERVAL),
            "stamped hdr={stamped_hdr}, now hdr={hdr}: due once the interval elapses"
        );
    }
    for last in [None, Some((t, false)), Some((t, true))] {
        assert!(
            !edr_refresh_due(None, last, elapsed, EDR_REQUERY_INTERVAL),
            "no GPU present, stamp {last:?}: never due"
        );
    }
}
