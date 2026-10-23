// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Process-wide scrollback byte-budget sharing across live panes.

use std::sync::atomic::{AtomicUsize, Ordering};

static GLOBAL_BUDGET_BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE_SHARES: AtomicUsize = AtomicUsize::new(0);

/// Set the module-wide scrollback budget in bytes (`0` = unlimited).
#[inline]
pub fn set_global_scrollback_budget(bytes: usize) {
    GLOBAL_BUDGET_BYTES.store(bytes, Ordering::Relaxed);
}

/// One pane's membership in the global scrollback budget, registered until dropped.
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
    #[inline]
    #[must_use]
    pub fn register(configured_bytes: usize) -> Self {
        LIVE_SHARES.fetch_add(1, Ordering::Relaxed);
        Self {
            configured: configured_bytes,
            applied: usize::MAX,
        }
    }

    /// Change this pane's configured per-pane budget (bytes).
    #[inline]
    pub const fn set_configured(&mut self, bytes: usize) {
        self.configured = bytes;
    }

    /// The pane's configured per-pane budget (bytes).
    #[inline]
    #[must_use]
    pub const fn configured(&self) -> usize {
        self.configured
    }

    /// Effective budget (`min(configured, global / live_panes)`, floored at 1 byte; a `0` global
    /// passes `configured` through).
    #[inline]
    #[must_use]
    pub fn effective(&self) -> usize {
        let global = GLOBAL_BUDGET_BYTES.load(Ordering::Relaxed);
        if global == 0 {
            return self.configured.max(1);
        }
        let live = LIVE_SHARES.load(Ordering::Relaxed).max(1);
        self.configured.min(global / live).max(1)
    }

    /// Returns `Some(effective_bytes)` when the effective budget changed since last applied.
    #[inline]
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
        let _ = LIVE_SHARES.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
            Some(n.saturating_sub(1))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

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
    fn reconfigure_and_global_change_surface_on_next_poll() {
        with_clean_globals(|| {
            let mut share = ScrollbackBudgetShare::register(500);
            assert_eq!(share.pending_effective(), Some(500));
            set_global_scrollback_budget(200);
            assert_eq!(share.pending_effective(), Some(200));
            share.set_configured(120);
            assert_eq!(share.pending_effective(), Some(120));
            share.set_configured(0);
            assert_eq!(share.pending_effective(), Some(1));
        });
    }
}
