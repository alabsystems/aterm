// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn named_self_cpu_recorded_scene_never_calls_the_live_reader() {
    let probe = Probe {
        read: |_| panic!("a scene must supply its own counters"),
    };
    assert!(probe.sample(Instant::now(), true).is_none());
}

#[test]
fn named_self_cpu_inert_reader_exposes_missing_evidence() {
    use aterm_vitals::self_cpu::{OwnCpuState, OwnCpuView};
    let mut state = OwnCpuState::default();
    let cut = Probe::INERT.sample(Instant::now(), false).unwrap();
    assert!(matches!(
        state.absorb(Some(cut.as_ref().map_err(|why| *why))),
        OwnCpuView::Unavailable("not_sampled")
    ));
}

#[test]
fn named_self_cpu_native_projection_keeps_published_counter_precision() {
    use aterm_sysprobe::self_cpu as raw;
    use aterm_vitals::self_cpu::{OwnCpuState, OwnCpuView};
    let make = |begin, process, thread, precision| raw::Cut {
        process: raw::ProcessIdentity {
            pid: 42,
            start: 12,
            version: -7,
        },
        begin_ns: begin,
        end_ns: begin + 10,
        process_before: raw::CpuCounter {
            ns: process,
            precision_ns: 0,
        },
        process_after: raw::CpuCounter {
            ns: process,
            precision_ns: 0,
        },
        threads: vec![raw::ThreadReading {
            id: 1,
            name: Some("aterm-vitals".into()),
            cpu: Some(raw::CpuCounter {
                ns: thread,
                precision_ns: precision,
            }),
        }],
        coverage: raw::Coverage {
            listed: 1,
            inspected: 1,
            unread: 0,
            capped: false,
        },
    };
    let before = project(make(1, 0, 0, 100));
    let after = project(make(100, 1000, 850, 0));
    let mut state = OwnCpuState::default();
    assert!(matches!(
        state.absorb(Some(Ok(&before))),
        OwnCpuView::Warming
    ));
    let OwnCpuView::Observed(changes) = state.absorb(Some(Ok(&after))) else {
        panic!("counter observations")
    };
    assert_eq!(changes.groups[0].growth.lower_ns, 750);
    assert_eq!(changes.groups[0].growth.upper_ns, 850);
}
