// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::time::Duration;

fn point(at: Instant, user: u64, idle: u64) -> CpuPoint {
    CpuPoint {
        at,
        unix_ms: 100,
        ticks: vec![CoreTicks {
            user,
            idle,
            ..CoreTicks::default()
        }],
    }
}

#[test]
fn component_resets_cannot_be_hidden_by_growth_elsewhere() {
    let start = Instant::now();
    let mut before = point(start, 100, 100);
    before.ticks[0].system = 100;
    let mut after = point(start + Duration::from_secs(2), 99, 1000);
    after.ticks[0].system = 1000;
    assert!(CpuWindow::between(&before, &after).is_none());
    after.ticks[0].user = 100;
    assert!(CpuWindow::between(&before, &after).is_some());
    after.ticks.push(CoreTicks::default());
    assert!(CpuWindow::between(&before, &after).is_none());
}

#[test]
fn zero_empty_and_overflow_intervals_are_unknown() {
    let now = Instant::now();
    let before = point(now, 0, 0);
    assert!(CpuWindow::between(&before, &before).is_none());
    assert!(CpuWindow::between(&before, &point(now + Duration::from_secs(1), 0, 0)).is_none());
    let mut after = point(now + Duration::from_secs(1), u64::MAX, 1);
    assert!(CpuWindow::between(&before, &after).is_none());
    after.ticks.clear();
    assert!(CpuWindow::between(&before, &after).is_none());
}

#[test]
fn readiness_reuses_the_existing_strict_quiet_cpu_criterion() {
    let now = Instant::now();
    let before = point(now, 0, 0);
    for (busy, idle, ready) in [
        (24, 76, true),
        (25, 75, false),
        (0, 100, true),
        (100, 0, false),
    ] {
        let window =
            CpuWindow::between(&before, &point(now + Duration::from_secs(2), busy, idle)).unwrap();
        assert_eq!(window.ready(), ready);
        assert_eq!(CpuWindow::read(&window.value()), Some(window));
    }
}

#[test]
fn malformed_or_legacy_proof_never_retains_a_ready_observation() {
    let now = Instant::now();
    let window = CpuWindow::between(
        &point(now, 0, 0),
        &point(now + Duration::from_secs(2), 1, 99),
    )
    .unwrap();
    let evidence = TimingEvidence {
        cause: TimingCause::Wait,
        admitted_ms: Some(100),
        admission: Some(window),
        entry: Some(window),
        admission_load1: Some(1.0),
        entry_load1: Some(2.0),
    };
    assert_eq!(TimingEvidence::read(&evidence.value()), evidence);
    assert_eq!(TimingEvidence::read(&json!({})), TimingEvidence::default());
    let mut value = evidence.value();
    value.as_object_mut().unwrap().insert(
        "entry".into(),
        json!({"provider":"invented","total_ticks":100}),
    );
    assert_eq!(TimingEvidence::read(&value), TimingEvidence::default());
}
