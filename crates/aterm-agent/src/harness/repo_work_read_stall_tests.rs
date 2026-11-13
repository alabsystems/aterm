// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;
use std::time::Duration;

std::thread_local! {
    static SKIP: Cell<u32> = const { Cell::new(0) };
    static NEXT: Cell<u64> = const { Cell::new(0) };
    static EVERY: Cell<u64> = const { Cell::new(0) };
    static SPENT: Cell<u64> = const { Cell::new(0) };
}

/// The next read this thread starts is starved `ms` (0: none).
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn set(ms: u64) {
    set_after(0, ms);
}

/// Starve the read after `skip` reads, clearing the time already spent.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn set_after(skip: u32, ms: u64) {
    SKIP.with(|k| k.set(skip));
    NEXT.with(|n| n.set(ms));
    EVERY.with(|e| e.set(0));
    SPENT.with(|s| s.set(0));
}

/// Whether the read [`set_after`] named is still to come: a probe that returned with it pending
/// never reached it, so a probe told to starve any later read is this same uncut one.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn pending() -> bool {
    NEXT.with(Cell::get) != 0
}

/// Starve every read this thread starts `ms`, clearing the time already spent.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn every(ms: u64) {
    set_after(0, 0);
    EVERY.with(|e| e.set(ms));
}

/// The time this thread's reads have lost so far: how far the probe's clock
/// runs ahead of the machine's, which every deadline is set on
/// ([`super::Reads::ended`]).
pub(super) fn lost() -> Duration {
    Duration::from_millis(SPENT.with(Cell::get))
}

/// The time this thread's reads have lost so far, the read starting now included.
pub(super) fn spent() -> Duration {
    let skipped = SKIP.with(|k| {
        let left = k.get();
        k.set(left.saturating_sub(1));
        left > 0
    });
    let next = if skipped {
        0
    } else {
        NEXT.with(|n| n.replace(0))
    };
    let next = next.saturating_add(EVERY.with(Cell::get));
    SPENT.with(|s| {
        s.set(s.get().saturating_add(next));
        Duration::from_millis(s.get())
    })
}
