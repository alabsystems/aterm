// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_vitals::explain::{CoalSample, CoalUsage};
use aterm_vitals::model::ProcSample;
use std::cell::Cell;

thread_local! {
    static NOW: Cell<Option<u64>> = const { Cell::new(Some(0)) };
    static CLOCK_READS: Cell<usize> = const { Cell::new(0) };
}

fn clock() -> Option<u64> {
    CLOCK_READS.with(|n| n.set(n.get() + 1));
    NOW.with(Cell::get)
}
fn wakes() -> Wakes {
    NOW.with(|n| n.set(n.get().map(|v| v + 7)));
    Some((vec![("Output", 9)], false))
}
fn sample() -> Sample {
    Sample {
        self_pid: 41,
        procs: Some(vec![ProcSample {
            pid: 41,
            start: Some(9),
            version: Some(3),
            cpu_ns: Some(4),
            child_cpu_ns: Some(11),
            coalition: Some(7),
            ..ProcSample::default()
        }]),
        coalitions: Some(CoalSample {
            listed: 1,
            rows: vec![CoalUsage {
                id: 7,
                cpu_ns: 1000,
                ..CoalUsage::default()
            }],
            ..CoalSample::default()
        }),
        ..Sample::default()
    }
}
fn capture() -> BootCapture {
    BootCapture::new(None, Some("boot-1".into()), Probes { clock, wakes })
}

#[test]
fn the_adapter_retains_independent_sources_and_brackets_the_wake_read() {
    let mut capture = capture();
    NOW.set(Some(179_000_000_000));
    let before = capture.begin();
    capture.finish(&sample(), HistoricalReported::default(), before);
    NOW.set(Some(901_000_000_000));
    let before = capture.begin();
    let history = HistoricalReported {
        own_ns: 200,
        tabs_ns: 300,
        incomplete: false,
    };
    let report = capture.finish(&sample(), history, before);
    let boot = report.book.boot("boot-1").unwrap();
    assert_eq!(boot.targets, [Some(0), Some(0)]);
    assert_eq!(boot.observations.len(), 1);
    let observation = &boot.observations[0];
    assert_eq!(observation.before_ns, Some(901_000_000_000));
    assert_eq!(observation.after_ns, Some(901_000_000_007));
    assert_eq!(observation.at_ns, 901_000_000_007);
    assert_eq!(
        observation.process_cpu,
        RawCounters {
            own_ns: Some(4),
            inherited_ns: Some(11)
        }
    );
    assert_eq!(
        observation.coalition_cpu,
        RawCounters {
            own_ns: Some(1000),
            inherited_ns: None
        }
    );
    assert_eq!(observation.historical_reported, history);
    assert_eq!(observation.wakes.as_ref().unwrap().variants[0].count, 9);
}

#[test]
fn resolved_targets_add_no_more_probe_reads_or_report_allocations() {
    let mut capture = capture();
    NOW.set(Some(901_000_000_000));
    let before = capture.begin();
    let first = capture.finish(&sample(), HistoricalReported::default(), before);
    let reads = CLOCK_READS.get();
    assert_eq!(capture.after(), None);
    assert_eq!(capture.begin(), None);
    let next = capture.finish(&sample(), HistoricalReported::default(), None);
    assert!(Arc::ptr_eq(&first, &next));
    assert_eq!(CLOCK_READS.get(), reads);
}

#[test]
fn missing_boot_identity_arms_and_reads_nothing() {
    let mut capture = BootCapture::new(
        None,
        None,
        Probes {
            clock: || panic!("unsupported identity must not read the clock"),
            wakes: || panic!("unsupported identity must not read UI state"),
        },
    );
    assert_eq!(capture.after(), None);
    assert_eq!(capture.begin(), None);
    let report = capture.finish(&sample(), HistoricalReported::default(), None);
    assert!(report.book.boots.is_empty());
    assert_eq!(report.boot_id, None);
}

#[test]
fn a_missing_end_clock_cannot_backdate_new_wake_observations() {
    let mut capture = BootCapture::new(
        None,
        Some("boot-1".into()),
        Probes {
            clock,
            wakes: || {
                NOW.set(None);
                Some((vec![("Output", 1)], false))
            },
        },
    );
    NOW.set(Some(901_000_000_000));
    let before = capture.begin();
    let report = capture.finish(&sample(), HistoricalReported::default(), before);
    assert!(report.book.boots.is_empty());
    assert_eq!(capture.after(), None);
    assert_eq!(
        report.failure.as_deref(),
        Some("continuous_clock_unavailable")
    );

    capture.probes.wakes = wakes;
    NOW.set(Some(180_000_000_000));
    let before = capture.begin();
    let recovered = capture.finish(&sample(), HistoricalReported::default(), before);
    let boot = recovered.book.boot("boot-1").unwrap();
    assert!(boot.coverage.contains(Coverage::COLLECTION_GAP));
    assert!(
        boot.observations[0]
            .coverage
            .contains(Coverage::COLLECTION_GAP)
    );
    assert!(recovered.failure.is_none());
}

#[cfg(unix)]
#[test]
fn an_existing_report_opportunity_retries_the_final_cut_without_recapturing_it() {
    let directory = aterm_tempfile::tempdir().unwrap();
    let parent = directory.path().canonicalize().unwrap();
    aterm_uds::private_file::create_private_dir(&parent).unwrap();
    let path = parent.join(crate::vitals_host::boot_cost::FILE);
    aterm_uds::atomic_file::write(&path, b"invalid boot record", 0o600).unwrap();
    let mut capture = BootCapture::new(
        Some(path.clone()),
        Some("boot-1".into()),
        Probes { clock, wakes },
    );
    let settle = |capture: &mut BootCapture| {
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "boot capture persistence",
            || {
                capture.poll();
                (!capture.store.view().busy).then_some(())
            },
        );
    };
    settle(&mut capture);
    NOW.set(Some(901_000_000_000));
    let before = capture.begin();
    capture.finish(&sample(), HistoricalReported::default(), before);
    settle(&mut capture);
    let pending = capture.collector.book().clone();
    let failed = capture.report();
    assert!(failed.dirty);
    assert_eq!(failed.persistence, "failed");
    assert_eq!(capture.after(), None, "completed cuts arm no retry timer");
    assert_eq!(std::fs::read(&path).unwrap(), b"invalid boot record");

    std::fs::remove_file(&path).unwrap();
    NOW.set(Some(901_000_000_007 + RETRY_NS));
    assert!(capture.report().busy);
    settle(&mut capture);
    let saved = capture.report();
    assert_eq!(saved.persistence, "saved");
    assert!(!saved.dirty);
    assert_eq!(saved.book, pending);
    assert_eq!(
        boot_cost::decode(&std::fs::read(path).unwrap()).unwrap(),
        pending
    );
}

#[test]
fn unrepresentable_wake_names_are_missing_evidence_not_missing_cpu() {
    let mut capture = BootCapture::new(
        None,
        Some("boot-1".into()),
        Probes {
            clock,
            wakes: || Some((vec![("a name with spaces", 17)], false)),
        },
    );
    NOW.set(Some(901_000_000_000));
    let before = capture.begin();
    let report = capture.finish(&sample(), HistoricalReported::default(), before);
    let boot = report
        .book
        .boot("boot-1")
        .expect("CPU capture still retained");
    let observed = &boot.observations[0];
    assert_eq!(observed.process_cpu.own_ns, Some(4));
    assert!(observed.wakes.is_none());
    assert!(observed.coverage.contains(Coverage::WAKE_MISSING));
}
