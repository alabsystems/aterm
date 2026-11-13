// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_lane_that_runs_no_census_asks_nothing_and_starts_no_worker() {
    let mut lane = CensusLane::new(false);
    let now = Instant::now();
    assert_eq!(lane.step(now, now), None);
    assert!(lane.ask.is_none() && lane.asked_at.is_none());
}

#[test]
fn a_pass_is_taken_once_and_an_ask_waits_out_its_period() {
    let (tx, rx) = std::sync::mpsc::sync_channel::<()>(1);
    let t0 = Instant::now();
    let mut lane = CensusLane {
        live: true,
        ask: Some(tx),
        done: Arc::new(Mutex::new(None)),
        asked_at: None,
    };
    assert_eq!(lane.step(t0, t0), None);
    assert!(rx.try_recv().is_ok(), "the first reading asks");
    let pass = Arc::new(CensusPass {
        listed: 7,
        ..CensusPass::default()
    });
    *lane.done.lock().expect("unpoisoned") = Some(Arc::clone(&pass));
    let later = t0 + Duration::from_secs(1);
    assert_eq!(lane.step(later, t0), Some(pass));
    assert!(rx.try_recv().is_err(), "within the period, no ask");
    assert_eq!(lane.step(later, t0), None, "taken once");
    let due = t0 + Duration::from_millis(CENSUS_PERIOD_MS);
    let _ = lane.step(due, t0);
    assert!(rx.try_recv().is_ok(), "asked again at the period");
    // A worker gone ends the census rather than asking forever.
    drop(rx);
    let _ = lane.step(due + Duration::from_millis(CENSUS_PERIOD_MS), t0);
    assert!(!lane.live && lane.ask.is_none());
}
