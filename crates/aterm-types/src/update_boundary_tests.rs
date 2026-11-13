// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;

/// The hang detector for what must happen: a minute, never a latency budget.
const MUST_HAPPEN: Duration = Duration::from_secs(60);

/// Wait, at most [`MUST_HAPPEN`], until `holds` does.
fn until(what: &str, holds: impl Fn() -> bool) {
    let deadline = Instant::now() + MUST_HAPPEN;
    while !holds() {
        assert!(Instant::now() < deadline, "{what} never happened");
        aterm_uds::deadline::poll_yield();
    }
}

/// Checks that pass, and never run.
fn unchecked() -> Result<(), ()> {
    Ok(())
}

/// A clone's publish rename that asks for its guard while a handoff is pending does not
/// enter its critical section: it waits, holding nothing, until the handoff is refused, then
/// checks again and goes on once. Before the latch it took the guard at once, after the
/// handoff had read the guard free, and the Commit's `_exit` could cut it in half.
#[test]
fn an_item_waits_for_a_pending_handoff_and_goes_on_once_it_is_refused() {
    let boundary = Boundary::new();
    let latch = boundary.latch_handoff(MUST_HAPPEN);
    assert!(boundary.handoff_pending());
    let checked = AtomicU32::new(0);
    let (ran, finished) = mpsc::channel();
    std::thread::scope(|scope| {
        let item = scope.spawn(|| {
            let _guard = boundary
                .hold(GuardedItem::ClonePublishRename, || {
                    checked.fetch_add(1, Ordering::SeqCst);
                    Ok::<(), ()>(())
                })
                .expect("its checks pass");
            ran.send(()).unwrap();
        });
        until("the rename waiting for the handoff", || {
            boundary.waiting() == 1
        });
        assert!(
            !boundary.is_held(GuardedItem::ClonePublishRename),
            "nothing is in its critical section while the handoff is pending"
        );
        assert!(finished.try_recv().is_err(), "the rename has not run");
        // The handoff is refused: its latch drops, and the rename checks again and goes on.
        drop(latch);
        assert!(!boundary.handoff_pending());
        finished
            .recv_timeout(MUST_HAPPEN)
            .expect("the rename runs once the handoff is refused");
        item.join().unwrap();
    });
    assert!(finished.try_recv().is_err(), "once");
    assert_eq!(checked.load(Ordering::SeqCst), 1, "checked again once");
    assert_eq!(boundary.waiting(), 0);
    assert_eq!(boundary.any_held(), None);
}

/// An item that waited out a handoff checks again what it acts on before it enters its
/// section: what its checks read before the wait may have changed meanwhile (the person
/// walked away, the vault changed, a policy turned off). A check that now fails stands the
/// item aside, holding nothing; with no wait the checks never run, so nothing is read twice.
/// Before, the item entered its section on checks made before a wait of up to eight minutes.
#[test]
fn an_item_that_waited_for_a_handoff_checks_again_and_stands_aside_on_a_failed_check() {
    let boundary = Boundary::new();
    let gone = AtomicBool::new(false);
    let recheck = || {
        if gone.load(Ordering::SeqCst) {
            Err("the person walked away while the handoff was pending")
        } else {
            Ok(())
        }
    };
    // No handoff pending: no wait, so no check runs again.
    gone.store(true, Ordering::SeqCst);
    let guard = boundary
        .hold(GuardedItem::SignOutSequence, recheck)
        .expect("no wait, no check again");
    drop(guard);
    gone.store(false, Ordering::SeqCst);
    let latch = boundary.latch_handoff(MUST_HAPPEN);
    let held = std::thread::scope(|scope| {
        let item = scope.spawn(|| {
            boundary
                .hold(GuardedItem::SignOutSequence, recheck)
                .map(|_guard| ())
        });
        until("the sign-out waiting for the handoff", || {
            boundary.waiting() == 1
        });
        gone.store(true, Ordering::SeqCst);
        drop(latch);
        item.join().unwrap()
    });
    assert_eq!(
        held,
        Err("the person walked away while the handoff was pending"),
        "the item stood aside on its check after the wait"
    );
    assert_eq!(boundary.any_held(), None, "it took no guard");
    assert_eq!(boundary.waiting(), 0);
}

/// The wait is bounded by the handoff's own bound: past it, the item goes on (and the
/// handoff's Commit reads the guard again). On the handoff's own thread nothing waits, since
/// the handoff could not go on meanwhile.
#[test]
fn the_wait_ends_at_the_handoffs_bound_and_never_on_its_own_thread() {
    let boundary = Boundary::new();
    {
        let _spent = boundary.latch_handoff(Duration::ZERO);
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let _guard = boundary
                        .hold(GuardedItem::SignOutSequence, unchecked)
                        .expect("past the bound it goes on");
                })
                .join()
                .unwrap();
        });
    }
    let _latch = boundary.latch_handoff(MUST_HAPPEN);
    let guard = boundary
        .hold(GuardedItem::LaunchdBootstrap, unchecked)
        .expect("the handoff's own thread never waits");
    assert_eq!(boundary.any_held(), Some(GuardedItem::LaunchdBootstrap));
    drop(guard);
    assert_eq!(boundary.waiting(), 0);
}

/// A handoff that panics releases its latch as the panic unwinds, so no item waits for a
/// handoff that is gone; a later latch is never released by an earlier one's drop.
#[test]
fn a_panicking_handoff_releases_its_latch() {
    let boundary = Boundary::new();
    std::thread::scope(|scope| {
        let handoff = scope.spawn(|| {
            let _latch = boundary.latch_handoff(MUST_HAPPEN);
            panic!("injected: the handoff panics");
        });
        assert!(handoff.join().is_err());
    });
    assert!(!boundary.handoff_pending(), "the unwinding released it");
    let earlier = boundary.latch_handoff(MUST_HAPPEN);
    let later = boundary.latch_handoff(MUST_HAPPEN);
    drop(earlier);
    assert!(
        boundary.handoff_pending(),
        "the later handoff is still pending"
    );
    drop(later);
    assert!(!boundary.handoff_pending());
}

/// Holding a guard shows in `any_held` until it drops; a nested hold of the same kind keeps
/// it held until the last releases. Registries are separate: a test's is not the process's.
#[test]
fn a_held_guard_shows_until_the_last_holder_drops() {
    let boundary = Boundary::new();
    assert_eq!(boundary.any_held(), None);
    let outer = boundary
        .hold(GuardedItem::ClonePublishRename, unchecked)
        .unwrap();
    assert_eq!(boundary.any_held(), Some(GuardedItem::ClonePublishRename));
    let inner = boundary
        .hold(GuardedItem::ClonePublishRename, unchecked)
        .unwrap();
    drop(inner);
    assert_eq!(
        boundary.any_held(),
        Some(GuardedItem::ClonePublishRename),
        "the outer still holds"
    );
    assert!(
        !std::ptr::eq(&boundary, Boundary::process()),
        "a test's registry is its own"
    );
    drop(outer);
    assert_eq!(
        boundary.any_held(),
        None,
        "released when the last holder drops"
    );
}
