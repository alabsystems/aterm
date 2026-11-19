// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn named_self_cpu_recorded_scene_never_calls_the_live_reader() {
    for reads in [true, false] {
        let probe = Probe {
            read: |_| panic!("a scene must supply its own counters"),
            reads,
        };
        assert!(probe.sample(Instant::now(), true, true).is_none());
    }
}

/// A READER THAT READS THIS PROCESS'S THREADS IS READ ONLY WHERE ASKED, ONE THAT READS NOTHING AT
/// EVERY READING: a reader that reads is never called unasked and answers asked; one that reads
/// nothing (the native reader off macOS) answers its `unsupported` asked or not; the native reader
/// reads where `aterm_sysprobe::self_cpu::READS` says (macOS). NEGATIVE CONTROL: a sample that
/// asks a reader only where asked, whatever it reads, answers `None` unasked of the reader that
/// reads nothing.
#[test]
fn a_reader_that_reads_nothing_answers_unasked() {
    let reads = Probe {
        read: |_| Err("read"),
        reads: true,
    };
    let unasked = Probe {
        read: |_| panic!("a reader that reads is read only where asked"),
        reads: true,
    };
    assert!(unasked.sample(Instant::now(), false, false).is_none());
    assert_eq!(reads.sample(Instant::now(), false, true), Some(Err("read")));
    let nothing = Probe {
        read: |_| Err("unsupported"),
        reads: false,
    };
    for asked in [false, true] {
        assert_eq!(
            nothing.sample(Instant::now(), false, asked),
            Some(Err("unsupported")),
            "asked {asked}"
        );
    }
    assert_eq!(Probe::NATIVE.reads, cfg!(target_os = "macos"));
}

#[test]
fn named_self_cpu_inert_reader_exposes_missing_evidence() {
    use aterm_vitals::self_cpu::{OwnCpuState, OwnCpuView};
    let mut state = OwnCpuState::default();
    let cut = Probe::INERT.sample(Instant::now(), false, true).unwrap();
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
            split: Some(raw::CpuSplit {
                user_ns: thread,
                system_ns: 0,
            }),
            queue: None,
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
