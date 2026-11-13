// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! App-owned automatic work admissions and retirement across cancellation epochs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};

#[derive(Default)]
struct Epoch {
    cancelled: AtomicBool,
}

#[derive(Default)]
struct Status {
    cancelled: AtomicBool,
    retired: AtomicBool,
}

struct State {
    epoch: Arc<Epoch>,
    sealed: bool,
    operations: usize,
}

struct Shared {
    state: Mutex<State>,
    retired: Condvar,
}

#[path = "account_work_release.rs"]
mod release_operation;
pub(super) use release_operation::{ReleaseMonitor, ReleaseOperation};
#[path = "account_work_maintenance.rs"]
mod maintenance;
pub use maintenance::AdmissionOwner;
pub(in crate::harness) use maintenance::{MaintenanceEpoch, MaintenanceOwner};

/// Admission owner for automatic account work.
#[derive(Clone)]
pub struct Lifetime(Arc<Shared>);

impl Default for Lifetime {
    fn default() -> Self {
        Self(Arc::new(Shared {
            state: Mutex::new(State {
                epoch: Arc::new(Epoch::default()),
                sealed: false,
                operations: 0,
            }),
            retired: Condvar::new(),
        }))
    }
}

// Both normal App work and a genuine weak same-App maintenance locator use
// this single current-epoch admission gate. No namespace is created here.
fn counted_enter(shared: &Arc<Shared>) -> Option<Scope> {
    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    if state.sealed || state.epoch.cancelled.load(Ordering::Acquire) {
        return None;
    }
    state.operations = state.operations.checked_add(1)?;
    Some(Scope(Arc::new(Operation {
        shared: shared.clone(),
        epoch: state.epoch.clone(),
        status: Arc::new(Status::default()),
        #[cfg(test)]
        dropping: None,
    })))
}

impl Lifetime {
    /// Counts a new operation unless admissions are closed.
    pub fn enter(&self) -> Option<Scope> {
        counted_enter(&self.0)
    }

    /// A distinct counted release turn may settle retained cleanup while
    /// ordinary admission is closed, but never after retirement is sealed.
    pub(super) fn release_turn(&self) -> Option<ReleaseTurn> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.sealed {
            return None;
        }
        state.operations = state.operations.checked_add(1)?;
        Some(ReleaseTurn {
            shared: self.0.clone(),
            operation: None,
        })
    }

    /// Closes admissions and cancels every scope in the current epoch.
    pub fn pause(&self) -> Option<Pause> {
        let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.epoch.cancelled.load(Ordering::Acquire) {
            return None;
        }
        state.epoch.cancelled.store(true, Ordering::Release);
        Some(Pause {
            shared: self.0.clone(),
            epoch: state.epoch.clone(),
        })
    }
}

struct Operation {
    shared: Arc<Shared>,
    epoch: Arc<Epoch>,
    status: Arc<Status>,
    #[cfg(test)]
    dropping: Option<std::sync::mpsc::Sender<()>>,
}

impl Drop for Operation {
    fn drop(&mut self) {
        // Final Scope loss stops work before accounting can wait on State.
        self.status.cancelled.store(true, Ordering::Release);
        #[cfg(test)]
        if let Some(dropping) = self.dropping.take() {
            let _ = dropping.send(());
        }
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.operations -= 1;
        // A unique Drained token can only observe zero after this publication.
        self.status.retired.store(true, Ordering::Release);
        if state.operations == 0 {
            self.shared.retired.notify_all();
        }
    }
}

/// Release-only counted custody. It has no Scope/cancellation/work conversion.
pub(super) struct ReleaseTurn {
    shared: Arc<Shared>,
    operation: Option<OperationIdentity>,
}
impl ReleaseTurn {
    pub(super) fn operation_identity(&self) -> Option<&OperationIdentity> {
        self.operation.as_ref()
    }
}

/// Weak identity of one actual counted operation, never a work admission or a
/// persisted identity. It does not keep scopes or credentials alive.
#[derive(Clone)]
pub(super) struct OperationIdentity(Weak<Operation>);
impl PartialEq for OperationIdentity {
    fn eq(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for OperationIdentity {}
impl std::fmt::Debug for OperationIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OperationIdentity(redacted)")
    }
}
impl OperationIdentity {
    pub(super) fn matches_scope(&self, scope: &Scope) -> bool {
        Weak::ptr_eq(&self.0, &Arc::downgrade(&scope.0))
    }
}
impl Drop for ReleaseTurn {
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
    pub(super) fn operation_identity(&self) -> OperationIdentity {
        OperationIdentity(Arc::downgrade(&self.0))
    }

    /// A cancellation handle which does not keep this operation alive.
    pub fn cancellation(&self) -> Cancellation {
        Cancellation {
            owner: Arc::downgrade(&self.0.shared),
            epoch: self.0.epoch.clone(),
            status: self.0.status.clone(),
        }
    }

    /// Whether this operation or its original admission epoch has been cancelled.
    pub fn cancelled(&self) -> bool {
        self.0.status.cancelled.load(Ordering::Acquire)
            || self.0.epoch.cancelled.load(Ordering::Acquire)
    }

    /// Counts release-only cleanup against this exact admitted epoch. It may
    /// settle cancellation while ordinary work is paused, never after sealing.
    pub(super) fn release_turn(&self) -> Option<ReleaseTurn> {
        let mut state = self
            .0
            .shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if state.sealed || !Arc::ptr_eq(&state.epoch, &self.0.epoch) {
            return None;
        }
        state.operations = state.operations.checked_add(1)?;
        Some(ReleaseTurn {
            shared: self.0.shared.clone(),
            operation: Some(self.operation_identity()),
        })
    }

    /// Retains this scope until all work captures have retired, even without a run.
    pub fn wrap<W: FnOnce() + Send + 'static>(self, work: W) -> impl FnOnce() + Send + 'static {
        let owned = Wrapped { work, scope: self };
        move || owned.run()
    }
}

/// Cancels one operation without retaining any of its work captures.
#[derive(Clone)]
pub struct Cancellation {
    owner: Weak<Shared>,
    epoch: Arc<Epoch>,
    status: Arc<Status>,
}

impl Cancellation {
    /// Whether this original operation stopped or has already retired.
    pub fn cancelled(&self) -> bool {
        self.status.cancelled.load(Ordering::Acquire)
            || self.epoch.cancelled.load(Ordering::Acquire)
    }
    /// Lifetime namespace only, independent of whether the operation has retired.
    pub fn owned_by(&self, lifetime: &Lifetime) -> bool {
        self.owner.ptr_eq(&Arc::downgrade(&lifetime.0))
    }
    /// Requests cancellation of this exact operation and all its scope clones.
    pub fn cancel(&self) {
        self.status.cancelled.store(true, Ordering::Release);
    }

    /// Whether every scope is gone and this operation's count has retired.
    /// This is a work-lifetime observation, not enrollment publication authority.
    pub fn retired(&self) -> bool {
        self.status.retired.load(Ordering::Acquire)
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
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.operations != 0 {
            drop(state);
            return Err(self);
        }
        state.sealed = true;
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
        state.sealed = true;
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
        if !state.epoch.cancelled.load(Ordering::Acquire)
            || state.operations != 0
            || !Arc::ptr_eq(&state.epoch, &self.epoch)
        {
            drop(state);
            return Err(self);
        }
        state.epoch = Arc::new(Epoch::default());
        state.sealed = false;
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

#[cfg(test)]
#[path = "account_work_scope_release_tests.rs"]
mod scope_release_tests;
