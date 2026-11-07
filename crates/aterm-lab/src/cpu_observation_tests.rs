// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn idle_mode() -> Mode {
    Mode::Idle {
        settle: Duration::ZERO,
        band: Band::App,
    }
}

#[test]
fn control_peer_requires_exact_pid_and_exec_version() {
    let identity = Identity {
        pid: 14,
        birth: aterm_uds::process::Birth(100),
        idversion: Some(3),
    };
    let token = |pid, version| {
        let mut words = [0; 8];
        words[5] = pid;
        words[7] = version;
        Some(AuditToken(words))
    };
    assert!(peer_matches(identity, token(14, 3)));
    assert!(!peer_matches(identity, token(15, 3)));
    assert!(!peer_matches(identity, token(14, 4)));
    assert!(!peer_matches(identity, None));
    assert!(!peer_matches(
        Identity {
            idversion: None,
            ..identity
        },
        token(14, 3)
    ));
}

#[test]
fn cpu_requires_both_boundaries_and_monotone_uninterrupted_identity() {
    let mut threads = Threads::default();
    let sample = |ns| {
        vec![ThreadCpu {
            tid: 8,
            name: "aterm-temporal-writer".into(),
            ns,
        }]
    };
    threads.observe(Some(&sample(10)), true);
    threads.observe(Some(&sample(15)), false);
    assert_eq!(threads.0[&8].cpu.delta(), Some(5));
    threads.observe(None, false);
    threads.observe(Some(&sample(20)), false);
    assert_eq!(threads.0[&8].cpu.delta(), None);
    let mut late = Threads::default();
    late.observe(Some(&[]), true);
    late.observe(Some(&sample(20)), false);
    assert_eq!(late.0[&8].cpu.delta(), None);
    let mut reset = Metric::default();
    reset.observe(Some(20), true);
    reset.observe(Some(19), false);
    assert_eq!(reset.delta(), None);
}

#[test]
fn missing_gauges_do_not_invent_zero_or_erase_observed_lower_bound() {
    let mut metric = Metric::default();
    metric.observe(None, true);
    assert_eq!(metric.max_observed, None);
    metric.observe(Some(12), false);
    metric.observe(Some(0), false);
    metric.observe(None, false);
    assert_eq!(metric.max_observed, Some(12));
    assert_eq!(metric.last, None);
    assert_eq!(metric.missing, 2);
    assert_eq!(metric.delta(), None);
}

#[test]
fn exited_or_renamed_thread_never_looks_like_completed_zero_work() {
    let mut threads = Threads::default();
    threads.observe(
        Some(&[ThreadCpu {
            tid: 4,
            name: "aterm-pty-reader-0".into(),
            ns: 4,
        }]),
        true,
    );
    threads.observe(
        Some(&[ThreadCpu {
            tid: 4,
            name: "other".into(),
            ns: 9,
        }]),
        false,
    );
    assert_eq!(threads.0[&4].cpu.delta(), None);
    assert_eq!(threads.0[&4].cpu.last, None);
}

fn background_sample(ns: u64) -> Vec<ThreadCpu> {
    [
        "main",
        "aterm-netplace-keeper",
        "aterm-netplace-follow",
        "aterm-board-keeper",
        "aterm-watchdog",
        "aterm-pty-reader-0",
        "aterm-temporal-writer",
    ]
    .iter()
    .enumerate()
    .map(|(i, name)| ThreadCpu {
        tid: i as u64 + 1,
        name: (*name).into(),
        ns,
    })
    .collect()
}

#[test]
fn idle_cpu_all_scope_observes_background_work_without_video_threads() {
    let mut threads = Threads::new(idle_mode());
    threads.observe(Some(&background_sample(10)), true);
    threads.observe(Some(&background_sample(15)), false);
    let report = threads.report();
    assert_eq!(
        report.len(),
        7,
        "background CPU cannot disappear behind the recording filter"
    );
    for item in report {
        assert_eq!(item.get("cpu_delta_ns").and_then(Value::as_u64), Some(5));
        assert_eq!(
            item.get("missing_observations").and_then(Value::as_u64),
            Some(0)
        );
    }
}

#[test]
fn idle_cpu_recording_scope_keeps_existing_reader_writer_selection() {
    let mut threads = Threads::new(Mode::Recording);
    threads.observe(Some(&background_sample(10)), true);
    threads.observe(Some(&background_sample(15)), false);
    let report = threads.report();
    assert_eq!(report.len(), 2);
    assert!(report.iter().all(|item| {
        matches!(
            item.get("name").and_then(Value::as_str),
            Some("aterm-pty-reader-0" | "aterm-temporal-writer")
        ) && item.get("cpu_delta_ns").and_then(Value::as_u64) == Some(5)
    }));
}

#[test]
fn idle_cpu_exact_deltas_refuse_incomplete_or_changed_lifetimes_but_keep_known_zero() {
    let snapshot = |ns| ThreadCpuObservation {
        threads: vec![ThreadCpu {
            tid: 1,
            name: "main".into(),
            ns,
        }],
        missing_tids: Vec::new(),
        list_complete: true,
    };
    let first = snapshot(10);
    let mut zero = Threads::new(idle_mode());
    assert!(zero.observe_snapshot(Some(&first), true));
    assert!(zero.observe_snapshot(Some(&first), false));
    assert_eq!(zero.0[&1].cpu.delta(), Some(0));
    for mut last in [snapshot(20), snapshot(9)] {
        // Each case changes one independent precondition of an exact window.
        if last.threads[0].ns == 20 {
            last.missing_tids.push(2);
        }
        let mut threads = Threads::new(idle_mode());
        threads.observe_snapshot(Some(&first), true);
        threads.observe_snapshot(Some(&last), false);
        assert_eq!(threads.0[&1].cpu.delta(), None);
    }
    let mut truncated = snapshot(20);
    truncated.list_complete = false;
    let mut renamed = snapshot(20);
    renamed.threads[0].name = "other".into();
    for last in [truncated, renamed] {
        let mut threads = Threads::new(idle_mode());
        threads.observe_snapshot(Some(&first), true);
        threads.observe_snapshot(Some(&last), false);
        assert_eq!(threads.0[&1].cpu.delta(), None);
    }
    let mut missing = Threads::new(idle_mode());
    missing.observe_snapshot(Some(&first), true);
    missing.observe_snapshot(None, false);
    missing.observe_snapshot(Some(&snapshot(20)), false);
    assert_eq!(missing.0[&1].cpu.delta(), None);
    let mut late = Threads::new(idle_mode());
    late.observe_snapshot(None, true);
    late.observe_snapshot(Some(&snapshot(20)), false);
    assert_eq!(late.0[&1].cpu.delta(), None);
}

#[test]
fn idle_cpu_missing_read_cannot_erase_counter_decrease() {
    for (recovered, decreased) in [(19, true), (21, false)] {
        let mut metric = Metric::default();
        metric.observe(Some(20), true);
        metric.observe(None, false);
        metric.observe(Some(recovered), false);
        assert_eq!(metric.reset, decreased);
        assert_eq!(metric.max_observed, Some(20.max(recovered)));
        assert_eq!(metric.missing, 1);
        assert_eq!(metric.delta(), None);
    }
}
