// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Disk watch progress beat.

use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// Entries between two beats.
pub const EVERY: usize = 4096;

/// The handler a beat calls.
pub type Handler = Arc<dyn Fn() + Send + Sync>;

static HANDLER: Mutex<Option<Handler>> = Mutex::new(None);
static COUNT: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// This thread runs a walk that is not the watch's ([`quietly`]).
    static QUIET: Cell<bool> = const { Cell::new(false) };
}

/// Run `f` with this thread's beats silenced ([`beat`], [`tick`]).
pub fn quietly<T>(f: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            QUIET.with(|q| q.set(self.0));
        }
    }
    let _restore = Restore(QUIET.with(|q| q.replace(true)));
    f()
}

/// Whether this thread's beats are silenced ([`quietly`]).
fn quiet() -> bool {
    QUIET.with(Cell::get)
}

/// Register the one handler (the window's disk board), replacing any.
pub fn set_handler(handler: Handler) {
    *HANDLER.lock().unwrap_or_else(PoisonError::into_inner) = Some(handler);
}

/// Beat now: a step of a pass finished.
pub fn beat() {
    if quiet() {
        return;
    }
    COUNT.store(0, Ordering::Relaxed);
    let handler = HANDLER
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    if let Some(h) = handler {
        h();
    }
}

/// Count `n` entries a walk read or a delete removed, beating once every [`EVERY`].
pub fn tick(n: usize) {
    if quiet() {
        return;
    }
    let before = COUNT.fetch_add(n, Ordering::Relaxed);
    if crosses(before, n) {
        beat();
    }
}

/// Whether counting `n` more entries past `before` crosses a multiple of [`EVERY`]: one beat.
#[must_use]
const fn crosses(before: usize, n: usize) -> bool {
    before.saturating_add(n) / EVERY > before / EVERY
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_walk_beats_every_four_thousand_entries() {
        assert!(!crosses(0, EVERY - 1));
        assert!(crosses(EVERY - 1, 1));
        assert!(!crosses(EVERY, EVERY - 1));
        assert!(crosses(10, 3 * EVERY));
        assert!(!crosses(0, 0));
    }

    #[test]
    fn a_quiet_thread_never_beats() {
        static HEARD: Mutex<Vec<std::thread::ThreadId>> = Mutex::new(Vec::new());
        set_handler(Arc::new(|| {
            HEARD
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(std::thread::current().id());
        }));
        let me = std::thread::current().id();
        let heard = || {
            HEARD
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .filter(|t| **t == me)
                .count()
        };
        quietly(|| {
            beat();
            tick(3 * EVERY);
        });
        assert_eq!(heard(), 0, "a quiet walk beat");
        let failed = std::panic::catch_unwind(|| quietly(|| panic!("a survey that fails")));
        assert!(failed.is_err());
        beat();
        assert_eq!(heard(), 1, "the watch's own beat is heard");
    }
}
