// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Process/module-wide scrollback byte-budget sharing (audit E1).
//!
//! A host embedding SEVERAL terminals in one memory space (orc: every pane of a
//! renderer worker lives in one wasm module; a daemon holds many sessions in one
//! process) needs TWO budget knobs, not one: the per-pane budget each
//! `Scrollback` already enforces, and a GLOBAL cap so N panes cannot multiply
//! the per-pane budget into an OOM. This module is the global half.
//!
//! POLICY — equal shares, applied at touch time. The effective budget of a
//! registered pane is `min(configured, global / live_panes)` (an unset global —
//! `0` — leaves the configured budget alone). Panes only mutate their own
//! `Scrollback` (a wasm pane is owned by JS; a daemon session by its own task),
//! so a share change is APPLIED when that pane is next touched: the owner calls
//! [`ScrollbackBudgetShare::pending_effective`] at its mutation points and
//! forwards any returned value to `Terminal::set_memory_budget`. Between a
//! membership/global change and a pane's next touch its old share stays in
//! force — bounded staleness (never more than one touch), zero cross-pane
//! locking, and O(2 atomic loads) on the hot path.
//!
//! Equal division deliberately trades utilization for predictability: a busy
//! pane cannot borrow an idle pane's share, so the global bound needs no usage
//! accounting on the ingest path and Σ(applied shares) ≤ global holds at every
//! quiescent point (spec: `shared_budget_model`, Tier-1-bound in
//! `tests/conformance_shared_budget.rs`).

use std::sync::atomic::{AtomicUsize, Ordering};

/// Module-wide scrollback budget in bytes. `0` = unlimited (per-pane budgets
/// only) — the default, so embedders opt in explicitly.
static GLOBAL_BUDGET_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Live registered panes ([`ScrollbackBudgetShare`] instances).
static LIVE_SHARES: AtomicUsize = AtomicUsize::new(0);

/// Set the module-wide scrollback budget (bytes; `0` = unlimited). Takes effect
/// on each pane as it is next touched (see module docs).
pub fn set_global_scrollback_budget(bytes: usize) {
    GLOBAL_BUDGET_BYTES.store(bytes, Ordering::Relaxed);
}

/// The module-wide scrollback budget (bytes; `0` = unlimited).
#[must_use]
pub fn global_scrollback_budget() -> usize {
    GLOBAL_BUDGET_BYTES.load(Ordering::Relaxed)
}

/// One pane's membership in the global scrollback budget.
///
/// Owned by the embedder NEXT TO its `Terminal` (not inside it — most
/// `Terminal`s are test/tool instances that must not distort a host's share
/// arithmetic). Registers on construction, deregisters on drop.
#[derive(Debug)]
pub struct ScrollbackBudgetShare {
    /// The pane's own configured budget (bytes) — the cap the host asked for.
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

    /// Effective budget under the current global/membership state:
    /// `min(configured, global / live_panes)`, floored at 1 byte (the
    /// `Scrollback` clamp), with an unset global passing `configured` through.
    #[must_use]
    pub fn effective(&self) -> usize {
        let global = GLOBAL_BUDGET_BYTES.load(Ordering::Relaxed);
        if global == 0 {
            return self.configured.max(1);
        }
        let live = LIVE_SHARES.load(Ordering::Relaxed).max(1);
        self.configured.min(global / live).max(1)
    }

    /// Poll at the pane's mutation points: returns `Some(effective_bytes)` when
    /// the effective budget CHANGED since last applied (the caller forwards it
    /// to `Terminal::set_memory_budget`, which evicts to fit), `None` when the
    /// applied value is already current.
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
            // Two live panes: share = 50. a is capped by the share, b by its
            // own smaller configured budget.
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
