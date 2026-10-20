// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BEAT: the disk watch's sign that it is making progress (design
//! `docs/DESIGN-disk-awareness-2026-10-17.md` §3.1). A pass that takes many
//! minutes beats throughout — every [`EVERY`] entries any of its walks reads
//! and every [`EVERY`] a delete removes — so a wait that holds a demand reads
//! a beat a minute old as a blocked syscall or a dead thread, never as a long
//! pass (`await sys`'s `why=watch-stalled`). A beat is a timestamp the host
//! keeps; nothing here sleeps or waits.
//!
//! The host registers the one handler ([`set_handler`]); without one a beat
//! does nothing. The count is one atomic add per entry and a lock only every
//! [`EVERY`] entries.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// Entries between two beats.
pub const EVERY: usize = 4096;

/// The handler a beat calls.
pub type Handler = Arc<dyn Fn() + Send + Sync>;

static HANDLER: Mutex<Option<Handler>> = Mutex::new(None);
static COUNT: AtomicUsize = AtomicUsize::new(0);

/// Register the one handler (the window's disk board), replacing any.
pub fn set_handler(handler: Handler) {
    *HANDLER.lock().unwrap_or_else(PoisonError::into_inner) = Some(handler);
}

/// Beat now: a step of a pass finished (a look began or ended, a directory
/// was sized, a unit's step was taken).
pub fn beat() {
    COUNT.store(0, Ordering::Relaxed);
    let handler = HANDLER
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    if let Some(h) = handler {
        h();
    }
}

/// Count `n` entries a walk read or a delete removed, beating once every
/// [`EVERY`].
pub fn tick(n: usize) {
    let before = COUNT.fetch_add(n, Ordering::Relaxed);
    if crosses(before, n) {
        beat();
    }
}

/// Whether counting `n` more entries past `before` crosses a multiple of
/// [`EVERY`]: one beat for the count, however many it crosses.
#[must_use]
fn crosses(before: usize, n: usize) -> bool {
    before.saturating_add(n) / EVERY > before / EVERY
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A beat every [`EVERY`] entries, wherever the counts fall: one entry
    /// short of it is none, the entry that reaches it is one, and a count
    /// that crosses several is one beat, not one per thousand.
    #[test]
    fn a_walk_beats_every_four_thousand_entries() {
        assert!(!crosses(0, EVERY - 1));
        assert!(crosses(EVERY - 1, 1));
        assert!(!crosses(EVERY, EVERY - 1));
        assert!(crosses(10, 3 * EVERY));
        assert!(!crosses(0, 0));
    }
}
