// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Process-wide scrollback byte-budget sharing.
//!
//! A host running several terminals in one memory space (a wasm renderer worker's panes, a
//! daemon's sessions) needs a global cap on top of each `Scrollback`'s per-pane budget.
//! Equal shares, applied at touch time: a registered pane's effective budget is
//! `min(configured, global / live_panes)` (a global of `0` leaves it alone). Each owner
//! polls [`ScrollbackBudgetShare::pending_effective`] at its mutation points and forwards a
//! change to `Terminal::set_memory_budget`: no cross-pane locking, two atomic loads on the
//! hot path. No pane borrows another's share, so Σ(applied shares) ≤ global holds at every
//! quiescent point (spec `shared_budget_model`, bound in
//! `tests/conformance_shared_budget.rs`).

use std::sync::atomic::{AtomicUsize, Ordering};

/// Module-wide scrollback budget in bytes; `0` (the default) is per-pane budgets only.
static GLOBAL_BUDGET_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Live registered panes ([`ScrollbackBudgetShare`] instances).
static LIVE_SHARES: AtomicUsize = AtomicUsize::new(0);

/// Set the module-wide scrollback budget (bytes; `0` = unlimited), applied to each pane at
/// its next touch.
pub fn set_global_scrollback_budget(bytes: usize) {
    GLOBAL_BUDGET_BYTES.store(bytes, Ordering::Relaxed);
}

/// One pane's membership in the global scrollback budget, owned by the embedder next to
/// its `Terminal` (so test and tool terminals do not count). Registers on construction,
/// deregisters on drop.
#[derive(Debug)]
pub struct ScrollbackBudgetShare {
    /// The pane's own configured budget (bytes).
    configured: usize,
    /// The effective budget last returned by [`Self::pending_effective`]
    /// (`usize::MAX` = never applied, so the first poll always fires).
    applied: usize,
}

impl ScrollbackBudgetShare {
    /// Register a pane with its configured per-pane budget (bytes).
    #[must_use]
    pub fn register(configured_bytes: usize) -> Self {
        LIVE_SHARES.fetch_add(1, Ordering::Relaxed);
        Self {
            configured: configured_bytes,
            applied: usize::MAX,
        }
    }

    /// Change this pane's configured per-pane budget (bytes). The new
    /// effective value surfaces on the next [`Self::pending_effective`].
    pub fn set_configured(&mut self, bytes: usize) {
        self.configured = bytes;
    }

    /// The pane's configured per-pane budget (bytes).
    #[must_use]
    pub fn configured(&self) -> usize {
        self.configured
    }

    /// Effective budget now: `min(configured, global / live_panes)`, floored at 1 byte
    /// (the `Scrollback` clamp); an unset global passes `configured` through.
    #[must_use]
    pub fn effective(&self) -> usize {
        let global = GLOBAL_BUDGET_BYTES.load(Ordering::Relaxed);
        if global == 0 {
            return self.configured.max(1);
        }
        let live = LIVE_SHARES.load(Ordering::Relaxed).max(1);
        self.configured.min(global / live).max(1)
    }

    /// Poll at the pane's mutation points: `Some(effective_bytes)` when the
    /// effective budget changed since last applied (forward it to
    /// `Terminal::set_memory_budget`), `None` otherwise.
    pub fn pending_effective(&mut self) -> Option<usize> {
        let effective = self.effective();
        if effective == self.applied {
            return None;
        }
        self.applied = effective;
        Some(effective)
    }
}

impl Drop for ScrollbackBudgetShare {
    fn drop(&mut self) {
        // Saturating: a mismatched count must not wrap into "billions of
        // panes" and zero every survivor's share.
        let _ = LIVE_SHARES.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
            Some(n.saturating_sub(1))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The statics are process-wide; tests that touch them must not interleave.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn with_clean_globals(f: impl FnOnce()) {
        let _guard = SERIAL.lock().expect("serial lock");
        set_global_scrollback_budget(0);
        assert_eq!(
            LIVE_SHARES.load(Ordering::Relaxed),
            0,
            "test left a share registered"
        );
        f();
        set_global_scrollback_budget(0);
    }

    #[test]
    fn unset_global_passes_configured_through() {
        with_clean_globals(|| {
            let mut share = ScrollbackBudgetShare::register(64);
            assert_eq!(share.pending_effective(), Some(64), "first poll applies");
            assert_eq!(share.pending_effective(), None, "second poll is settled");
        });
    }

    #[test]
    fn global_divides_equally_and_min_wins() {
        with_clean_globals(|| {
            set_global_scrollback_budget(100);
            let mut a = ScrollbackBudgetShare::register(64);
            let mut b = ScrollbackBudgetShare::register(30);
            // Share 50: a is capped by it, b by its own smaller budget.
            assert_eq!(a.pending_effective(), Some(50));
            assert_eq!(b.pending_effective(), Some(30));
            // A membership change re-fires the poll on the survivor.
            drop(b);
            assert_eq!(a.pending_effective(), Some(64), "sole pane: min(64, 100)");
            assert_eq!(a.pending_effective(), None);
        });
    }

    #[test]
    fn reconfigure_and_global_change_surface_on_next_poll() {
        with_clean_globals(|| {
            let mut share = ScrollbackBudgetShare::register(500);
            assert_eq!(share.pending_effective(), Some(500));
            set_global_scrollback_budget(200);
            assert_eq!(share.pending_effective(), Some(200));
            share.set_configured(120);
            assert_eq!(share.pending_effective(), Some(120));
            // Zero budgets floor at 1 byte (the Scrollback clamp), never 0.
            share.set_configured(0);
            assert_eq!(share.pending_effective(), Some(1));
        });
    }
}
