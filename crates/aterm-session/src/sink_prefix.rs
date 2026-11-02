// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Optional ownership of the exact bytes one accepted frame queued for the kernel.

use super::{AcceptedOrder, Shared, WriteReceipt};
use std::sync::{Arc, Condvar, Mutex, Weak};

/// An owned receipt for one frame's admitted bytes. Later terminal replies or operator bytes
/// own no part of this receipt, and retaining it keeps no sink descriptor alive.
#[derive(Clone)]
pub struct KernelPrefix {
    sink: Weak<Shared>,
    progress: Arc<Progress>,
}

struct Progress {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    pending: usize,
    in_flight: usize,
    sealed: bool,
    failed: bool,
}

impl State {
    fn settled(&self) -> bool {
        self.sealed && self.pending == 0 && self.in_flight == 0
    }
}

impl std::fmt::Debug for KernelPrefix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self
            .progress
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        f.debug_struct("KernelPrefix")
            .field("pending", &state.pending)
            .field("in_flight", &state.in_flight)
            .field("sealed", &state.sealed)
            .field("failed", &state.failed)
            .finish()
    }
}

impl PartialEq for KernelPrefix {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.progress, &other.progress)
    }
}

impl Eq for KernelPrefix {}

impl KernelPrefix {
    fn new(sink: &Arc<Shared>) -> Self {
        Self {
            sink: Arc::downgrade(sink),
            progress: Arc::new(Progress {
                state: Mutex::new(State::default()),
                changed: Condvar::new(),
            }),
        }
    }

    pub(super) fn belongs_to(&self, sink: &Arc<Shared>) -> bool {
        Weak::ptr_eq(&self.sink, &Arc::downgrade(sink))
    }

    /// Reserve while the sink holds the same spill lock that admits these actual bytes.
    #[cfg(unix)]
    pub(super) fn admit(&self, bytes: usize) {
        let mut state = self
            .progress
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        state.pending = state
            .pending
            .checked_add(bytes)
            .expect("one frame's admitted bytes fit usize");
    }

    #[cfg(unix)]
    pub(super) fn delivered(&self, bytes: usize) {
        let mut state = self
            .progress
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        debug_assert!(bytes <= state.pending);
        state.pending = state.pending.saturating_sub(bytes);
        drop(state);
        self.progress.changed.notify_all();
    }

    /// Reserve under the spill lock before a native write can release that lock.
    /// Clearing the queued bytes cannot retire this custody until the call returns.
    #[cfg(unix)]
    pub(super) fn native_write(&self) -> NativeWrite<'_> {
        let mut state = self
            .progress
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        state.in_flight = state
            .in_flight
            .checked_add(1)
            .expect("one frame's native writes fit usize");
        NativeWrite { prefix: self }
    }

    #[cfg(unix)]
    pub(super) fn cancelled(&self, bytes: usize) {
        if bytes == 0 {
            return;
        }
        let mut state = self
            .progress
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        debug_assert!(bytes <= state.pending);
        state.pending = state.pending.saturating_sub(bytes);
        state.failed = true;
        drop(state);
        self.progress.changed.notify_all();
    }

    #[cfg(unix)]
    pub(super) fn failed(&self) {
        self.progress
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .failed = true;
        self.progress.changed.notify_all();
    }

    fn seal(&self) {
        self.progress
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .sealed = true;
        self.progress.changed.notify_all();
    }

    pub(super) fn wait(&self) -> bool {
        let state = self
            .progress
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let state = self
            .progress
            .changed
            .wait_while(state, |state| !state.settled())
            .unwrap_or_else(|p| p.into_inner());
        !state.failed
    }
}

/// A stack-only reservation for an actual native write, including returned-write accounting.
/// Only prefix-aware frames acquire this reservation; ordinary writes allocate no owner.
#[cfg(unix)]
pub(super) struct NativeWrite<'a> {
    prefix: &'a KernelPrefix,
}

#[cfg(unix)]
impl Drop for NativeWrite<'_> {
    fn drop(&mut self) {
        let mut state = self
            .prefix
            .progress
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        debug_assert!(state.in_flight > 0);
        state.in_flight = state.in_flight.saturating_sub(1);
        drop(state);
        self.prefix.progress.changed.notify_all();
    }
}

/// Seals all direct/error paths without making every ordinary write allocate prefix state.
#[derive(Debug)]
pub(super) struct AcceptedFrame {
    pub(super) order: AcceptedOrder,
    pub(super) prefix: Option<KernelPrefix>,
}

impl AcceptedFrame {
    pub(super) fn new(sink: &Arc<Shared>, order: AcceptedOrder, tracked: bool) -> Self {
        Self {
            order,
            prefix: tracked.then(|| KernelPrefix::new(sink)),
        }
    }

    pub(super) fn receipt(&self, accepted: usize, direct: bool) -> WriteReceipt {
        WriteReceipt {
            accepted,
            order: (accepted > 0).then_some(self.order),
            direct,
            prefix: (accepted > 0).then(|| self.prefix.clone()).flatten(),
        }
    }
}

impl Drop for AcceptedFrame {
    fn drop(&mut self) {
        if let Some(prefix) = &self.prefix {
            prefix.seal();
        }
    }
}

#[cfg(all(test, unix))]
#[path = "sink_prefix_tests.rs"]
mod tests;
