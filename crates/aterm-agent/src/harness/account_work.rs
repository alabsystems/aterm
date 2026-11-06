// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! App-owned automatic work admissions and retirement across cancellation epochs.

use std::sync::{Arc, Condvar, Mutex};

#[derive(Default)]
struct Epoch;

struct State {
    epoch: Arc<Epoch>,
    closed: bool,
    operations: usize,
}

struct Shared {
    state: Mutex<State>,
    retired: Condvar,
}

/// Admission owner for automatic account work.
#[derive(Clone)]
pub struct Lifetime(Arc<Shared>);

impl Default for Lifetime {
    fn default() -> Self {
        Self(Arc::new(Shared {
            state: Mutex::new(State {
                epoch: Arc::new(Epoch),
                closed: false,
                operations: 0,
            }),
            retired: Condvar::new(),
        }))
    }
}

impl Lifetime {
    /// Counts a new operation unless admissions are closed.
    pub fn enter(&self) -> Option<Scope> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return None;
        }
        state.operations = state.operations.checked_add(1)?;
        Some(Scope(Arc::new(Operation {
            shared: self.0.clone(),
            epoch: state.epoch.clone(),
        })))
    }

    /// Closes admissions and cancels every scope in the current epoch.
    pub fn pause(&self) -> Option<Pause> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return None;
        }
        state.closed = true;
        Some(Pause {
            shared: self.0.clone(),
            epoch: state.epoch.clone(),
        })
    }
}

struct Operation {
    shared: Arc<Shared>,
    epoch: Arc<Epoch>,
}

impl Drop for Operation {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.operations -= 1;
        if state.operations == 0 {
            self.shared.retired.notify_all();
        }
    }
}

/// A counted operation; detached clones retain that same operation.
#[derive(Clone)]
pub struct Scope(Arc<Operation>);

impl Scope {
    /// Whether the operation's original admission epoch has been closed.
    pub fn cancelled(&self) -> bool {
        let state = self
            .0
            .shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        state.closed || !Arc::ptr_eq(&state.epoch, &self.0.epoch)
    }

    /// Retains this scope until all work captures have retired, even without a run.
    pub fn wrap<W: FnOnce() + Send + 'static>(self, work: W) -> impl FnOnce() + Send + 'static {
        let owned = Wrapped { work, scope: self };
        move || owned.run()
    }
}

// Field order also governs an unstarted closure's retirement.
struct Wrapped<W> {
    work: W,
    scope: Scope,
}

impl<W: FnOnce()> Wrapped<W> {
    fn run(self) {
        let Self { work, scope } = self;
        work();
        drop(scope);
    }
}

/// Exclusive closed-epoch token. Dropping it leaves admissions closed.
pub struct Pause {
    shared: Arc<Shared>,
    epoch: Arc<Epoch>,
}

impl Pause {
    /// Returns immediately with retirement proof only when no operation remains.
    ///
    /// # Errors
    /// Returns the still-closed pause token while any operation remains.
    pub fn try_wait(self) -> Result<Drained, Self> {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.operations != 0 {
            drop(state);
            return Err(self);
        }
        drop(state);
        Ok(Drained {
            shared: self.shared,
            epoch: self.epoch,
        })
    }

    /// Waits for all admitted operations, including detached clones, to retire.
    /// Call this on a background worker, never the event loop.
    pub fn wait(self) -> Drained {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        while state.operations != 0 {
            state = self
                .shared
                .retired
                .wait(state)
                .unwrap_or_else(|e| e.into_inner());
        }
        drop(state);
        Drained {
            shared: self.shared,
            epoch: self.epoch,
        }
    }
}

/// A retired closed epoch; dropping it leaves admissions closed.
pub struct Drained {
    shared: Arc<Shared>,
    epoch: Arc<Epoch>,
}

impl Drained {
    /// Reopens only this still-current, closed and retired epoch.
    ///
    /// # Errors
    /// Returns the token unchanged if the epoch or retirement no longer matches.
    pub fn resume(self) -> Result<(), Self> {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.closed || state.operations != 0 || !Arc::ptr_eq(&state.epoch, &self.epoch) {
            drop(state);
            return Err(self);
        }
        state.epoch = Arc::new(Epoch);
        state.closed = false;
        Ok(())
    }
}

#[cfg(test)]
#[path = "account_work_tests.rs"]
mod tests;

impl std::fmt::Debug for Lifetime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Lifetime { .. }")
    }
}
impl std::fmt::Debug for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Scope { .. }")
    }
}
impl std::fmt::Debug for Pause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pause { .. }")
    }
}
impl std::fmt::Debug for Drained {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Drained { .. }")
    }
}
