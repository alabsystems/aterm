// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::model::{ClusterView, CoreView, CpuView};

fn waiting() -> Snapshot {
    Snapshot {
        procs_window_ns: Some(1_000_000_000),
        cores_window_ns: Some(1_000_000_000),
        global: crate::model::ScopeView {
            wait: Some(4.0),
            ..Default::default()
        },
        cpu: Some(CpuView {
            cores: vec![
                CoreView {
                    measured: true,
                    ..CoreView::default()
                };
                4
            ],
            clusters: vec![ClusterView {
                name: "Performance".into(),
                short: "P".into(),
                cores: vec![0, 1, 2, 3],
            }],
            ..CpuView::default()
        }),
        ..Snapshot::default()
    }
}

#[test]
fn idle_performance_is_an_observation_even_without_backgrounding() {
    let mut s = waiting();
    assert_eq!(
        idle_performance(&s),
        Some(IdlePerformance {
            capacity: 4,
            busy: 0.0,
            wait: 4.0
        })
    );
    s.global.wait = Some(0.9);
    assert_eq!(idle_performance(&s), None);
    s.global.wait = Some(4.0);
    for c in &mut s.cpu.as_mut().unwrap().cores {
        c.load.raw = 0.3;
    }
    assert_eq!(idle_performance(&s), None);
}

#[test]
fn partial_unknown_and_malformed_readings_do_not_invent_the_observation() {
    let s = waiting();
    let mut partial = s.clone();
    partial.machine_only = true;
    assert_eq!(idle_performance(&partial), None);
    let mut missing = s.clone();
    missing.procs_window_ns = None;
    assert_eq!(idle_performance(&missing), None);
    let mut no_p = s.clone();
    no_p.cpu.as_mut().unwrap().clusters[0].short = "E".into();
    assert_eq!(idle_performance(&no_p), None);
    let mut unknown = s.clone();
    unknown.global.wait = None;
    assert_eq!(idle_performance(&unknown), None);
    let mut nan = s.clone();
    nan.global.wait = Some(f32::NAN);
    assert_eq!(idle_performance(&nan), None);
    let mut bad = s;
    bad.cpu.as_mut().unwrap().clusters[0].cores.push(99);
    assert_eq!(idle_performance(&bad), None);
}
