// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::model::{ClusterView, CoreView, CpuView};
fn anomaly(at: u64) -> Snapshot {
    let mut e = CoreView {
        measured: true,
        ..CoreView::default()
    };
    e.load.raw = 1.0;
    Snapshot {
        at_ns: at * 1_000_000_000,
        at_unix_ms: at as i64 * 1000,
        global: crate::model::ScopeView {
            wait: Some(4.0),
            ..Default::default()
        },
        procs_window_ns: Some(1_000_000_000),
        cores_window_ns: Some(1_000_000_000),
        cpu: Some(CpuView {
            cores: vec![
                CoreView {
                    measured: true,
                    ..CoreView::default()
                },
                e,
            ],
            clusters: vec![
                ClusterView {
                    name: "Performance".into(),
                    short: "P".into(),
                    cores: vec![0],
                },
                ClusterView {
                    name: "Efficiency".into(),
                    short: "E".into(),
                    cores: vec![1],
                },
            ],
            ..CpuView::default()
        }),
        hardware: Some(Sample::unavailable("refused:ioreport_subscription")),
        ..Snapshot::default()
    }
}
#[test]
fn sustained_episode_keeps_bounded_raw_evidence_and_survives_recovery() {
    let mut r = Recorder::default();
    let mut model = crate::model::Vitals::new();
    let mut reading = crate::model::Sample {
        at_ns: 1_000_000_000,
        procs: Some(vec![crate::model::ProcSample {
            pid: 123,
            start: Some(7),
            cpu_ns: Some(1_000_000_000),
            child_cpu_ns: Some(u64::MAX),
            reaps_unwaited: Some(true),
            version: Some(3),
            ..Default::default()
        }]),
        ..Default::default()
    };
    model.absorb(&reading);
    reading.at_ns += 1_000_000_000;
    let worker = model.absorb(&reading).procs.remove(0);
    for at in 1..31 {
        let mut sample = anomaly(at);
        sample.procs.push(worker.clone());
        assert!(r.observe(&sample, None).is_none());
    }
    let mut sample = anomaly(31);
    sample.procs.push(worker.clone());
    let first = r.observe(&sample, None).unwrap();
    assert_eq!(first.since_ns, 1_000_000_000);
    assert_eq!(first.frames.len(), 8);
    assert_eq!(first.frames[0].at_ns, first.since_ns);
    assert!(
        first
            .frames
            .iter()
            .all(|f| f.hardware.as_ref().unwrap().channels.is_err())
    );
    let mut recovered = anomaly(32);
    let mut unknown = recovered.clone();
    unknown.cpu.as_mut().unwrap().cores[0].measured = false;
    assert_eq!(
        r.observe(&unknown, None).unwrap().resolved_ns,
        None,
        "unread core counters do not establish recovery"
    );
    recovered.global.wait = Some(0.0);
    let kept = r.observe(&recovered, None).unwrap();
    assert_eq!(kept.resolved_ns, Some(recovered.at_ns));
    assert_eq!(kept.frames, first.frames);
    assert!(
        kept.frames.iter().all(|f| {
            f.workers.len() == 1 && f.workers[0].cpu_accounting == worker.cpu_accounting
        }),
        "exited workers retain their own and reaped-child counters"
    );
    let exported = crate::sys::hardware::bundle_json(&kept);
    let Some(crate::sys::json::Jv::Arr(frames)) = exported.get("frames") else {
        panic!("missing retained frames");
    };
    for frame in frames {
        let Some(crate::sys::json::Jv::Arr(workers)) = frame.get("workers") else {
            panic!("missing retained workers");
        };
        let accounting = workers[0].get("cpu_accounting").unwrap();
        assert_eq!(
            accounting.get("self_ns"),
            Some(&crate::sys::json::Jv::s("1000000000"))
        );
        assert_eq!(
            accounting.get("reaped_children_ns"),
            Some(&crate::sys::json::Jv::s("18446744073709551615"))
        );
    }
}
#[test]
fn unknown_or_discontinuous_observations_do_not_certify_sustained_anomaly() {
    let mut r = Recorder::default();
    assert!(!r.pending(), "nothing observed: no demand for CPU Stats");
    assert!(r.observe(&anomaly(1), None).is_none());
    assert!(
        r.pending(),
        "the first anomalous reading asks for CPU Stats"
    );
    assert!(
        r.observe(&anomaly(50), None).is_none(),
        "a gap is not sustained evidence"
    );
    let mut s = anomaly(51);
    s.cpu.as_mut().unwrap().cores[1].measured = false;
    assert!(r.observe(&s, None).is_none());
    assert!(
        r.pending.is_none(),
        "unknown E-core data cannot certify the anomaly"
    );
    assert!(!r.pending(), "and asks for no CPU Stats");
}

#[test]
fn underuse_thresholds_do_not_claim_every_performance_core_is_idle() {
    let mut s = anomaly(1);
    s.cpu.as_mut().unwrap().cores[0].load.raw = 0.25;
    s.cpu.as_mut().unwrap().cores[1].load.raw = 0.9;
    assert_eq!(
        all_e_busy_p_idle(&s),
        Some(true),
        "threshold includes partially busy P cores"
    );
    s.cpu.as_mut().unwrap().cores[0].load.raw = 0.251;
    assert_eq!(all_e_busy_p_idle(&s), Some(false));
    s.cpu.as_mut().unwrap().cores[0].load.raw = 0.0;
    s.cpu.as_mut().unwrap().cores[1].load.raw = 0.89;
    assert_eq!(all_e_busy_p_idle(&s), Some(false));
    s.cpu.as_mut().unwrap().cores[1].load.raw = 1.0;
    s.global.wait = Some(0.99);
    assert_eq!(all_e_busy_p_idle(&s), Some(false));
}
