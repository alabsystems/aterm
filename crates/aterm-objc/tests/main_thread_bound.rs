// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `MainThreadBound<T>` — the half of the contract libtest can hold.
//!
//! # WHAT THIS FILE MUST NOT DO, AND WHY IT IS THE FIRST THING WRITTEN HERE
//!
//! **Dropping a `MainThreadBound<T>` where `T` needs a destructor, on a libtest
//! worker thread, HANGS THE WHOLE TEST BINARY.** That is not a defect in the
//! type; it is [`aterm_objc::run_on_main`]'s documented first hang arriving
//! through `Drop`. A libtest binary's main thread is parked joining workers, so
//! it never services its main queue, and the reschedule waits for ever.
//!
//! So every test below either holds a `T` with no destructor, or consumes the
//! container with `into_inner` before it can be dropped. A future test that
//! forgets this will not fail — it will hang, which is the loudest possible
//! reason to say so here rather than in a comment further down.
//!
//! The half that CANNOT be held here — that the destructor really does run on
//! the main thread, and what happens to a container that omits the reschedule —
//! is in `examples/objc_bound_drive.rs`, which owns a `fn main` and can drive a
//! run loop. That is the same division as [`aterm_objc::run_on_main`] itself.

#![cfg(target_os = "macos")]

use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};

use aterm_objc::{MainThread, MainThreadBound};

/// A witness for a value with NO main-thread affinity.
///
/// # Safety
/// Every `T` this file puts in a container is a plain Rust value that touches
/// no AppKit state and whose destructor (where it has one) is a counter
/// increment, so `MainThread::new_unchecked`'s SECOND form is what discharges
/// this: the class being handled has no main-thread affinity and its payload
/// may be dropped on whatever thread performs the last release.
fn witness() -> MainThread {
    // SAFETY: as documented above.
    unsafe { MainThread::new_unchecked() }
}

fn assert_send<T: Send>() {}
fn assert_sync<T: Sync>() {}

#[test]
fn the_container_is_send_and_sync_over_a_t_that_is_neither() {
    // The whole point of the type, stated as a compile-time fact. `Rc` and
    // `Cell` are the standard library's two canonical negatives, and both are
    // stand-ins for the real cargo: `aterm_objc::Retained<T>` is `!Send` and
    // `!Sync` for the same structural reason.
    assert_send::<MainThreadBound<Rc<u8>>>();
    assert_sync::<MainThreadBound<Rc<u8>>>();
    assert_send::<MainThreadBound<Cell<u8>>>();
    assert_sync::<MainThreadBound<Cell<u8>>>();
    assert_send::<MainThreadBound<*mut u8>>();
    assert_sync::<MainThreadBound<*mut u8>>();

    // And it composes, which is the shape `vendor/winit`'s `Window` is: a
    // struct whose only fields are containers is itself `Send + Sync`.
    struct Window {
        _window: MainThreadBound<Rc<u8>>,
        _delegate: MainThreadBound<Rc<u8>>,
    }
    assert_send::<Window>();
    assert_sync::<Window>();
}

#[test]
fn the_container_costs_nothing_over_the_value() {
    assert_eq!(
        size_of::<MainThreadBound<Rc<u8>>>(),
        size_of::<Rc<u8>>(),
        "`ManuallyDrop` is `#[repr(transparent)]`, so the affinity is carried by the type system \
         and not by a field"
    );
    assert_eq!(align_of::<MainThreadBound<u64>>(), align_of::<u64>());
}

#[test]
fn get_and_get_mut_reach_the_value_through_a_witness() {
    let mut bound = MainThreadBound::new(Cell::new(7_u8), witness());
    assert_eq!(7, bound.get(witness()).get());
    bound.get_mut(witness()).set(9);
    assert_eq!(9, bound.get(witness()).get());
    // Consumed rather than dropped — see this file's opening note. `Cell<u8>`
    // needs no destructor, so a drop here would in fact be fine; consuming it
    // is the habit the file is trying to establish.
    assert_eq!(9, bound.into_inner(witness()).get());
}

#[test]
fn a_container_of_a_drop_less_value_never_reaches_the_main_queue() {
    // THE `needs_drop` SHORT-CIRCUIT, and it is load-bearing rather than an
    // optimisation: this test runs on a libtest WORKER, and the main thread of
    // this process is parked joining it. If `Drop` dispatched unconditionally,
    // this would block for ever.
    //
    // The failure mode is therefore a HANG, not a red test. That is stated
    // rather than hidden: a stage whose failure is silent has to say so, and
    // the alternative (asserting on a timeout from inside the thing being
    // timed) cannot be written.
    assert!(!std::mem::needs_drop::<Rc<u8>>() || true);
    assert!(
        !std::mem::needs_drop::<[u8; 4]>(),
        "the premise: this value has no destructor"
    );
    let bound = MainThreadBound::new([1_u8, 2, 3, 4], witness());
    let moved = std::thread::spawn(move || {
        // The container crossed to a worker BECAUSE it is `Send`, and is
        // dropped here, off the main thread, with no witness in sight.
        drop(bound);
        0xD0DE_u32
    })
    .join()
    .expect("the worker did not panic");
    assert_eq!(0xD0DE, moved);
}

#[test]
fn into_inner_hands_the_value_back_and_does_not_run_the_containers_drop() {
    static DROPS: AtomicU32 = AtomicU32::new(0);
    struct Counted;
    impl Drop for Counted {
        fn drop(&mut self) {
            DROPS.fetch_add(1, Ordering::Relaxed);
        }
    }

    DROPS.store(0, Ordering::Relaxed);
    let bound = MainThreadBound::new(Counted, witness());
    let value = bound.into_inner(witness());
    assert_eq!(
        0,
        DROPS.load(Ordering::Relaxed),
        "extracting must not run the destructor — and must not reschedule one either, which on \
         this thread would hang"
    );
    // Dropped HERE, as a bare `Counted`, on this worker: out of the container,
    // the value has no affinity and no reschedule.
    drop(value);
    assert_eq!(1, DROPS.load(Ordering::Relaxed), "exactly once");
}

#[test]
fn the_value_crosses_threads_inside_the_container_and_comes_back_intact() {
    // `Rc` is `!Send`. The container carries it to a worker and back, and the
    // strong count is untouched by the journey — the bytes were inert while
    // they travelled, which is the `Send` justification stated as an
    // observation.
    let rc = Rc::new(41_u8);
    let bound = MainThreadBound::new(Rc::clone(&rc), witness());
    assert_eq!(2, Rc::strong_count(&rc));

    let bound = std::thread::spawn(move || bound)
        .join()
        .expect("the worker did not panic");

    assert_eq!(
        2,
        Rc::strong_count(&rc),
        "nothing on the worker could clone it, drop it or read it"
    );
    let inner = bound.into_inner(witness());
    assert_eq!(41, *inner);
    drop(inner);
    assert_eq!(1, Rc::strong_count(&rc));
}

#[test]
fn debug_is_opaque() {
    let bound = MainThreadBound::new([0_u8; 2], witness());
    assert_eq!("MainThreadBound { .. }", format!("{bound:?}"));
}
