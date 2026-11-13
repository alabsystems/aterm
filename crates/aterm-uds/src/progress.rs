// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether a long step on this thread is still going. Per thread, because one step's progress says
//! nothing of another's: a tick on any other thread tells no one.

use std::cell::Cell;
use std::time::{Duration, Instant};

/// The least time between two reports of [`tick`]: the gap between them is this plus whatever step
/// is under way.
pub const EVERY: Duration = Duration::from_secs(5);

thread_local! {
    /// Told of this thread's ticks ([`heard_by`]).
    static HEARS: Cell<Option<fn()>> = const { Cell::new(None) };
    /// When [`HEARS`] was last told.
    static TOLD: Cell<Option<Instant>> = const { Cell::new(None) };
    /// This thread's ticks, heard or not ([`ticks_here`]).
    #[cfg(any(test, debug_assertions))]
    static TICKS: Cell<usize> = const { Cell::new(0) };
}

/// Run `step` with `hears` told of this thread's ticks: at the first, then at least [`EVERY`] apart.
/// Whatever heard them before hears them again once `step` ends or panics.
pub fn heard_by<T>(hears: fn(), step: impl FnOnce() -> T) -> T {
    struct Restore(Option<fn()>, Option<Instant>);
    impl Drop for Restore {
        fn drop(&mut self) {
            HEARS.set(self.0);
            TOLD.set(self.1);
        }
    }
    let _restore = Restore(HEARS.replace(Some(hears)), TOLD.replace(None));
    step()
}

/// A long step on this thread is still going, called from the loop that makes it long. Cheap enough
/// per entry: with nothing listening it reads no clock.
pub fn tick() {
    #[cfg(any(test, debug_assertions))]
    TICKS.set(TICKS.get().saturating_add(1));
    if HEARS.get().is_some() {
        tick_at(Instant::now());
    }
}

fn tick_at(now: Instant) {
    let Some(hears) = HEARS.get() else {
        return;
    };
    if due(TOLD.get(), now) {
        TOLD.set(Some(now));
        hears();
    }
}

fn due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|at| now.saturating_duration_since(at) >= EVERY)
}

/// This thread's ticks so far, heard or not, for a test of what ticks.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn ticks_here() -> usize {
    TICKS.get()
}

#[cfg(test)]
#[path = "progress_tests.rs"]
mod tests;
