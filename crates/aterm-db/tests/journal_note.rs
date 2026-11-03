// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The journal's typing hook, asserted structurally (`docs/DESIGN-alab-lab-model-2026-11-01.md`
//! §8, §12, step 8; finding 19: never a latency budget): once its folder exists, noting an event
//! allocates nothing, writes exactly one line with one write, and at the spool's cap writes
//! nothing and returns at once. One test runs in this binary, so the counting allocator sees only
//! it; the other is the ignored measurement `verify perf journal-note` runs alone.

#![cfg(unix)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use aterm_db::alab::journal::{Appended, Event, Kind, Pk, Source, Spool, Window, Word};

static COUNTING: AtomicBool = AtomicBool::new(false);
static CALLS: AtomicUsize = AtomicUsize::new(0);

struct Allocator;

// SAFETY: every allocation and deallocation is forwarded unchanged to System.
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            CALLS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: the caller supplies the valid allocation layout.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            CALLS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: the caller supplies the valid allocation layout.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            CALLS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: the caller supplies System's allocation and its layout.
        unsafe { System.realloc(ptr, layout, size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller supplies System's allocation and its layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

/// The allocations `f` makes on this thread's watch.
fn allocations<T>(f: impl FnOnce() -> T) -> (T, usize) {
    CALLS.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    let result = f();
    COUNTING.store(false, Ordering::Relaxed);
    (result, CALLS.load(Ordering::Relaxed))
}

/// The widest event a producer makes: a sub-account's reading with ten-digit times.
fn widest(t: u64) -> Event {
    Event {
        t: 9_999_999_000 + t,
        kind: Kind::Quota,
        account: Pk::new("ac-0123456789abcdef"),
        cli: Word::new("codex-alab").unwrap(),
        sub: Word::new("0123456789ab"),
        window: Some(Window::WeekModel),
        pct_x10: Some(1000),
        reset: Some(9_999_999_999),
        res: Some(aterm_db::alab::journal::Res::Minute),
        until: None,
        plan: None,
        src: Source::SessionUsage,
        why: None,
        to: None,
    }
}

/// NEGATIVE CONTROL: with `Spool::append` copying its line to the heap before the write
/// (`let line = line.to_vec();` then `write_all(&line)`), the warm append allocates:
/// `` an append allocates nothing left: 1 right: 0 ``.
#[test]
fn noting_an_event_allocates_nothing_and_writes_one_bounded_line() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let dir = scratch.path().join("alab");
    let spool = Spool::at(dir.clone());
    let file = dir.join(aterm_db::alab::journal::spool::SPOOL);
    // Warm: the first append makes the folder.
    assert_eq!(spool.append(&widest(1)).unwrap(), Appended::Written);
    let before = std::fs::metadata(&file).unwrap().len();
    let (appended, calls) = allocations(|| spool.append(&widest(2)));
    assert_eq!(appended.unwrap(), Appended::Written);
    assert_eq!(calls, 0, "an append allocates nothing");
    let text = std::fs::read(&file).unwrap();
    let grown = &text[usize::try_from(before).unwrap()..];
    assert_eq!(grown.iter().filter(|b| **b == b'\n').count(), 1, "one line");
    assert!(grown.len() <= aterm_db::alab::journal::event::MAX_EVENT + 1);
    assert_eq!(
        Event::from_line(&grown[..grown.len() - 1]),
        Some(widest(2)),
        "the line is the event"
    );
    // At the cap the event is dropped, never waited for, and nothing is written.
    let cap = aterm_db::alab::journal::spool::MAX_SPOOL;
    std::fs::OpenOptions::new()
        .write(true)
        .open(&file)
        .unwrap()
        .set_len(cap - 10)
        .unwrap();
    let (full, calls) = allocations(|| spool.append(&widest(3)));
    assert_eq!(full.unwrap(), Appended::Full);
    assert_eq!(calls, 0, "a full spool allocates nothing either");
    assert_eq!(std::fs::metadata(&file).unwrap().len(), cap - 10);
}

/// MEASURED (`targo --unverified verify perf journal-note`): one note's latency over 20,000 notes
/// into a spool that starts empty, as p50, p95 and p99 microseconds (design §10 row 19: the test
/// above holds the hook structurally, this reports its p99). Ignored: the lane runs it alone in an
/// optimized build and reads its line; the number is reported, never judged here.
#[test]
#[ignore = "a measurement: verify perf journal-note runs it"]
fn note_latency_reports_its_percentiles() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let spool = Spool::at(scratch.path().join("alab"));
    let mut samples: Vec<u128> = (0..20_000)
        .map(|n| {
            let start = std::time::Instant::now();
            let appended = spool.append(&widest(n));
            let took = start.elapsed().as_micros();
            assert_eq!(appended.unwrap(), Appended::Written);
            took
        })
        .collect();
    samples.sort_unstable();
    let at = |q: usize| samples[(samples.len() * q / 100).min(samples.len() - 1)];
    println!(
        "JOURNAL-NOTE n={} p50_us={} p95_us={} p99_us={}",
        samples.len(),
        at(50),
        at(95),
        at(99)
    );
}
