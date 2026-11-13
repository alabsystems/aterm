// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

thread_local! {
    static REAL: Cell<bool> = const { Cell::new(false) };
}

/// A test park's deadline unless [`real`] is held: a hang detector.
pub(crate) const HANG_DETECTOR: Duration = Duration::from_secs(60);

/// Holds the real budget on this thread until dropped.
#[must_use = "the park runs on the real budget only while the guard is held"]
pub(crate) struct Real(());

impl Drop for Real {
    fn drop(&mut self) {
        REAL.with(|real| real.set(false));
    }
}

pub(crate) fn real() -> Real {
    REAL.with(|real| real.set(true));
    Real(())
}

/// The budget the park's deadline is stamped from.
pub(crate) fn deadline_budget(budget: Duration) -> Duration {
    if REAL.with(Cell::get) {
        budget
    } else {
        HANG_DETECTOR
    }
}

static WATCHED: Mutex<Vec<(usize, bool)>> = Mutex::new(Vec::new());
static GAVE_UP: Condvar = Condvar::new();

fn address<T>(lock: &Mutex<T>) -> usize {
    std::ptr::from_ref(lock).addr()
}

/// A bounded take of `lock` ran past its deadline.
pub(super) fn gave_up<T>(lock: &Mutex<T>) {
    let at = address(lock);
    let mut watched = WATCHED.lock().unwrap_or_else(PoisonError::into_inner);
    for (watched_at, gave_up) in watched.iter_mut() {
        if *watched_at == at {
            *gave_up = true;
        }
    }
    GAVE_UP.notify_all();
}

/// Watch `lock` from now.
pub(crate) fn watch<T>(lock: &Mutex<T>) {
    WATCHED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((address(lock), false));
}

/// Blocks until a bounded take of the [`watch`]ed `lock` has given up, and stops watching it.
pub(crate) fn wait_gave_up<T>(lock: &Mutex<T>) {
    let at = address(lock);
    let watched = WATCHED.lock().unwrap_or_else(PoisonError::into_inner);
    let (mut watched, waited) = GAVE_UP
        .wait_timeout_while(watched, HANG_DETECTOR, |watched| {
            !watched.contains(&(at, true))
        })
        .unwrap_or_else(PoisonError::into_inner);
    assert!(
        !waited.timed_out(),
        "no bounded take of the watched lock gave up"
    );
    watched.retain(|(watched_at, _)| *watched_at != at);
}
