// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::vitals_host::{MIN_WINDOW, Reader};

thread_local! {
    static READS: Cell<u32> = const { Cell::new(0) };
    static CLIENTS: Cell<u32> = const { Cell::new(1) };
    static NAME: RefCell<String> = RefCell::new("stand-in".into());
    static COMPLETE: Cell<bool> = const { Cell::new(true) };
}

/// A counting stand-in: [`CLIENTS`] clients of this process named [`NAME`], each with 1 ms of
/// GPU time per read so far.
fn counting() -> Option<aterm_sysprobe::GpuClients> {
    let n = READS.with(|r| {
        r.set(r.get() + 1);
        r.get()
    });
    let rows = (0..CLIENTS.with(Cell::get))
        .map(|i| aterm_sysprobe::GpuClient {
            entry: u64::from(i) + 1,
            pid: std::process::id(),
            name: NAME.with(|n| n.borrow().clone()),
            gpu_ns: u64::from(n) * 1_000_000,
        })
        .collect();
    Some(aterm_sysprobe::GpuClients {
        rows,
        entries: (0..CLIENTS.with(Cell::get))
            .map(|i| u64::from(i) + 1)
            .collect(),
        children: CLIENTS.with(Cell::get),
        truncated: false,
        complete: COMPLETE.with(Cell::get),
    })
}

fn reads() -> u32 {
    READS.with(Cell::get)
}

fn bars() -> Request {
    Request {
        self_pid: std::process::id(),
        cadence_ms: 1_000,
        deck: true,
        ..Request::default()
    }
}

#[test]
fn the_clients_are_read_on_split_ticks_and_pulls_only() {
    let t0 = Instant::now();
    let mut lane = GpuLane::new(counting);
    let bars = bars();
    let hidden = Request {
        machine_only: true,
        ..bars.clone()
    };
    let engine = Request {
        jobs_now: true,
        ..bars.clone()
    };
    let pull = Request {
        pull: true,
        deck: false,
        ..bars.clone()
    };
    let at = |s: u64| t0 + Duration::from_secs(s);
    assert!(
        lane.cold(at(0)),
        "a lane that never read: a first pull reads twice"
    );
    assert!(lane.read(&bars, at(0)).is_some(), "the first tick");
    assert_eq!(lane.check, GpuCheck::On);
    assert!(lane.read(&bars, at(1)).is_none(), "inside the split period");
    assert!(lane.read(&hidden, at(10)).is_none(), "a hidden deck's");
    assert!(lane.read(&engine, at(10)).is_none(), "the engine's");
    assert_eq!(reads(), 1);
    assert!(lane.read(&bars, at(10)).is_some(), "the next tick");
    assert!(lane.read(&pull, at(10) + MIN_WINDOW).is_some(), "a pull");
    assert!(
        lane.read(&pull, at(10) + MIN_WINDOW).is_none(),
        "not twice at once"
    );
    assert_eq!(reads(), 3);
    assert!(!lane.cold(at(11)));
    assert!(
        lane.cold(at(10) + SPLIT_WINDOW_MAX),
        "near the model's window"
    );
}

#[test]
fn the_lane_turns_off_after_two_reads_with_no_client() {
    CLIENTS.with(|c| c.set(0));
    let t0 = Instant::now();
    let mut lane = GpuLane::new(counting);
    let at = |s: u64| t0 + Duration::from_secs(s);
    assert!(lane.read(&bars(), at(0)).is_none());
    assert_eq!(lane.check, GpuCheck::Recheck, "one re-check owed");
    CLIENTS.with(|c| c.set(1));
    assert!(lane.read(&bars(), at(5)).is_some(), "a client appeared");
    assert_eq!(lane.check, GpuCheck::On);

    CLIENTS.with(|c| c.set(0));
    let mut lane = GpuLane::new(counting);
    assert!(lane.read(&bars(), at(0)).is_none());
    assert!(lane.read(&bars(), at(5)).is_none());
    assert_eq!(lane.check, GpuCheck::Off);
    let before = reads();
    assert!(lane.read(&bars(), at(10)).is_none());
    assert!(!lane.due(&bars(), at(10)) && !lane.cold(at(10)));
    assert_eq!(reads(), before, "an off lane reads nothing");
}

/// A reader that answers nothing marks the lane unsupported, not off for keys gone missing,
/// at its first read.
#[test]
fn no_accelerator_turns_the_lane_off_as_unsupported() {
    let mut lane = GpuLane::new(GpuLane::INERT);
    let t0 = Instant::now();
    assert!(lane.read(&bars(), t0).is_none());
    assert_eq!(lane.check, GpuCheck::Unsupported);
    let later = t0 + Duration::from_secs(10);
    assert!(!lane.due(&bars(), later) && !lane.cold(later));
}

thread_local! {
    /// Whether [`answering`] answers.
    static ANSWERS: Cell<bool> = const { Cell::new(false) };
}

/// [`counting`], where [`ANSWERS`] says so; otherwise nothing, as Windows' reader answers in a
/// collection that failed or while DirectX's adapter rows are stale.
fn answering() -> Option<aterm_sysprobe::GpuClients> {
    if ANSWERS.with(Cell::get) {
        counting()
    } else {
        READS.with(|r| r.set(r.get() + 1));
        None
    }
}

/// Where a read that answers nothing can pass (Windows), the lane keeps reading on the split
/// cadence and turns on once the reader answers, and no pull reads twice for it meanwhile.
#[test]
fn a_lane_whose_reader_answers_nothing_for_now_reads_again() {
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let mut lane = GpuLane {
        passing: true,
        ..GpuLane::new(answering)
    };
    assert!(lane.read(&bars(), at(0)).is_none());
    assert_eq!(lane.check, GpuCheck::Unanswered);
    assert!(
        lane.due(&bars(), at(5)),
        "read again on the next split tick"
    );
    assert!(!lane.cold(at(5)), "no pull reads twice for it");
    assert!(lane.read(&bars(), at(5)).is_none());
    assert_eq!(lane.check, GpuCheck::Unanswered);
    ANSWERS.with(|a| a.set(true));
    assert!(
        lane.read(&bars(), at(10)).is_some(),
        "the GPU answers again"
    );
    assert_eq!(lane.check, GpuCheck::On);
    assert_eq!(reads(), 3);

    // NEGATIVE CONTROL: where nothing answered lasts (macOS), the first such read turns the lane
    // unsupported, and it never reads again.
    ANSWERS.with(|a| a.set(false));
    let mut lane = GpuLane::new(answering);
    assert!(lane.read(&bars(), at(0)).is_none());
    assert_eq!(lane.check, GpuCheck::Unsupported);
    ANSWERS.with(|a| a.set(true));
    assert!(lane.read(&bars(), at(5)).is_none());
    assert_eq!(reads(), 4);
}

/// The read's completeness reaches the sample the model takes.
#[test]
fn a_read_says_whether_it_saw_every_client() {
    fn with_unread_child() -> Option<aterm_sysprobe::GpuClients> {
        counting().map(|mut read| {
            read.entries.push(99);
            read
        })
    }
    let t0 = Instant::now();
    let mut lane = GpuLane::new(with_unread_child);
    let (rows, entries, complete) = lane.read(&bars(), t0).expect("a reading");
    assert_eq!(rows.len(), 1);
    assert_eq!(entries, [1, 99], "keep children without counter rows");
    assert!(complete);
    COMPLETE.with(|c| c.set(false));
    let next = t0 + Duration::from_secs(5);
    assert_eq!(lane.read(&bars(), next).map(|(_, _, c)| c), Some(false));
}

/// The model turns two reads into this process's share; the inert arm a test's reader
/// starts with reads nothing.
#[test]
fn the_reader_reads_the_clients_in_whole_readings_only() {
    let mut reader = Reader::new(Arc::default());
    assert_eq!(reader.gpu.check, GpuCheck::Unsupported, "the inert arm");
    assert!(!reader.gpu.cold(Instant::now()), "never makes a pull cold");
    assert!(reader.gpu.read(&bars(), Instant::now()).is_none());

    let me = std::process::id();
    let sweep = aterm_sysprobe::processes().expect("a sweep");
    let comm = sweep
        .procs
        .iter()
        .find(|p| p.pid == me)
        .expect("this process");
    NAME.with(|n| n.borrow_mut().clone_from(&comm.name));
    reader.gpu = GpuLane::new(counting);
    let hidden = Request {
        machine_only: true,
        ..bars()
    };
    let _ = reader.read_machine(&hidden);
    assert_eq!(reads(), 0, "a hidden deck's reading");
    let _ = reader.read(&bars());
    assert_eq!(reads(), 1, "the first split tick");
    let _ = reader.read(&bars());
    assert_eq!(reads(), 1, "inside the split period");
    reader.gpu.last = reader.gpu.last.and_then(|t| t.checked_sub(MIN_WINDOW));
    let pull = Request {
        pull: true,
        ..bars()
    };
    let snap = reader.read(&pull);
    assert_eq!(reads(), 2, "a pull");
    let row = snap
        .procs
        .iter()
        .find(|p| p.pid == me)
        .expect("this process's row");
    assert!(row.gpu.is_some_and(|g| g > 0.0), "{row:?}");
    assert!(
        snap.aterm.as_ref().is_some_and(|a| a.gpu == row.gpu),
        "{snap:?}"
    );
}
