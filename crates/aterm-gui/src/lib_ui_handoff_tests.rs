// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::Cell;
use std::sync::atomic::AtomicU32;

#[test]
fn no_waiter_means_no_pause() {
    let waiting = AtomicU32::new(0);
    let reads = Cell::new(0u32);
    for _ in 0..1000 {
        yield_to_ui_waiter_until(&waiting, UI_HANDOFF_SPIN, || {
            reads.set(reads.get() + 1);
            Instant::now()
        });
    }
    assert_eq!(
        reads.get(),
        0,
        "the flood fast path must not read the clock"
    );
}

#[test]
fn a_registered_waiter_is_waited_for_until_it_takes_the_lock() {
    const CLEARS_ON_READ: u64 = 5;
    let waiting = AtomicU32::new(1);
    let base = Instant::now();
    let reads = Cell::new(0u64);
    yield_to_ui_waiter_until(&waiting, UI_HANDOFF_SPIN, || {
        let n = reads.get();
        reads.set(n + 1);
        if n == CLEARS_ON_READ {
            // The waiter has been woken and has taken the lock.
            waiting.store(0, Ordering::Release);
        }
        base + Duration::from_micros(n)
    });
    assert_eq!(waiting.load(Ordering::Acquire), 0);
    assert_eq!(
        reads.get(),
        CLEARS_ON_READ + 1,
        "the releaser must spin up to the clear and return on the very next check: \
         fewer reads means it re-took the lock underneath the waiter, more means it \
         kept spinning after the handoff"
    );
}

#[test]
fn a_stuck_waiter_cannot_strand_the_releaser() {
    let waiting = AtomicU32::new(1);
    let base = Instant::now();
    let reads = Cell::new(0u64);
    yield_to_ui_waiter_until(&waiting, Duration::from_micros(200), || {
        let n = reads.get();
        reads.set(n + 1);
        base + Duration::from_micros(n * 20)
    });
    assert_eq!(
        waiting.load(Ordering::Acquire),
        1,
        "the waiter never cleared: the bound is the only thing that ended the spin"
    );
    assert_eq!(
        reads.get(),
        11,
        "`t0` plus ten 20 µs steps — the spin ends AT the bound, not before it and \
         not after it"
    );
}

/// A bounded read of a busy terminal waits asleep, registered for the hand-over: this holder
/// lets go only once its waiter has slept for it, which a waiter that yields between its
/// tries never does.
#[test]
fn a_bounded_read_sleeps_for_the_handover_and_never_yields() {
    let term = Arc::new(Mutex::new(Terminal::new(2, 10)));
    let waiting = Arc::new(AtomicU32::new(0));
    let watch = handover_pauses_for_test::watch(&waiting);
    let held = term_lock(&term);
    let (t2, w2) = (term.clone(), waiting.clone());
    let reader =
        std::thread::spawn(move || term_lock_ui_within(&t2, &w2, UI_READ_HANDOFF).is_some());
    let slept = aterm_spec::wait::within(aterm_spec::wait::MUST_HAPPEN, Duration::ZERO, || {
        (waiting.load(Ordering::Acquire) > 0 && watch.pauses() > 0).then_some(())
    });
    drop(held);
    yield_to_ui_waiter(&waiting);
    let read = reader.join().expect("the reader");
    assert!(
        slept.is_some(),
        "timed out waiting for the read to sleep as a registered waiter"
    );
    assert!(read, "the read took the handed-over terminal");
    assert_eq!(
        waiting.load(Ordering::Acquire),
        0,
        "and left no registration"
    );
}

#[test]
fn term_lock_ui_registers_only_while_blocked_and_books_the_wait() {
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let waiting = Arc::new(AtomicU32::new(0));
    let site = metrics::TermWaitSite::Press;
    let before = metrics::METRICS.window.redraw.term_wait[site.index()]
        .copy_out()
        .n();
    {
        let _g = term_lock_ui(&term, &waiting, site);
        assert_eq!(waiting.load(Ordering::Acquire), 0);
    }
    let held = term_lock(&term);
    let (t2, w2) = (term.clone(), waiting.clone());
    let acquirer = std::thread::spawn(move || {
        let g = term_lock_ui(&t2, &w2, site);
        let while_held = w2.load(Ordering::Acquire);
        drop(g);
        while_held
    });
    let t0 = Instant::now();
    while waiting.load(Ordering::Acquire) == 0 {
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "acquirer never registered"
        );
        aterm_uds::deadline::poll_yield();
    }
    drop(held);
    assert_eq!(
        acquirer.join().unwrap(),
        0,
        "registration must clear on entry"
    );
    assert_eq!(waiting.load(Ordering::Acquire), 0);
    assert!(
        metrics::METRICS.window.redraw.term_wait[site.index()]
            .copy_out()
            .n()
            >= before + 2,
        "both acquisitions must book a sample (n counts every acquisition)"
    );
}
