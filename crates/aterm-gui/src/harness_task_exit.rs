// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The admitted host runner that carries a session's task across an agent's exit.
//! This is an exclusion for passive exit settlement, never execution authority.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use aterm_phase::Program;
use aterm_session::LaunchNonce;

use super::{Switches, WorkerJob};

/// The actual runner's live admission, separate from a claim or a drive lease.
pub(crate) struct Admission {
    agent: Program,
    revoked: Arc<AtomicBool>,
    switches: Arc<Switches>,
}

impl Admission {
    pub(super) fn of(job: &WorkerJob) -> Self {
        Self {
            agent: job.agent,
            revoked: Arc::clone(&job.revoked),
            switches: Arc::clone(&job.switches),
        }
    }

    fn current(&self) -> bool {
        !self.revoked.load(Ordering::SeqCst) && self.switches.on_for(self.agent)
    }
}

struct State {
    nonce: LaunchNonce,
    admission: Admission,
    live: AtomicBool,
    local: u64,
    wake: Arc<dyn Fn(u64) + Send + Sync>,
}

/// Session metadata observes the runner without keeping it alive. A temporary upgraded read
/// also cannot extend its responsibility: only the unique guard controls `State::live`.
#[derive(Clone, Default)]
pub(crate) struct Slot(Weak<State>);

impl std::fmt::Debug for Slot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Slot")
            .field("retained", &(self.0.strong_count() != 0))
            .finish()
    }
}

impl Slot {
    /// Whether this session has an actual admitted runner. A restart names its observed
    /// agent; passive settlement stands down for any runner carrying the current session.
    pub(crate) fn current(&self, nonce: LaunchNonce, agent: Option<Program>) -> bool {
        self.0.upgrade().is_some_and(|state| {
            state.nonce == nonce
                && agent.is_none_or(|agent| state.admission.agent == agent)
                && state.live.load(Ordering::SeqCst)
                && state.admission.current()
        })
    }
}

/// Sole ownership of one successfully spawned runner's task-exit responsibility.
/// Neither worker handles, metadata nor readers own a copy of this guard.
pub(crate) struct Runner(Option<Arc<State>>);

impl Drop for Runner {
    fn drop(&mut self) {
        if let Some(state) = self.0.take() {
            state.live.store(false, Ordering::SeqCst);
            let local = state.local;
            let wake = Arc::clone(&state.wake);
            drop(state);
            // The passive watcher retained its ended source. Wake only after this runner's
            // responsibility ended, including unwinding, so it can now settle an unowned exit.
            wake(local);
        }
    }
}

/// Publish from inside the successfully spawned thread, immediately before its entry point.
/// The host reaps a session's finished runner before starting its replacement.
pub(crate) fn publish(
    ctx: &Arc<crate::SessionCtx>,
    local: u64,
    admission: Admission,
    wake: Arc<dyn Fn(u64) + Send + Sync>,
) -> Runner {
    let state = Arc::new(State {
        nonce: ctx.nonce,
        admission,
        live: AtomicBool::new(true),
        local,
        wake,
    });
    ctx.meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .task_exit_owner = Slot(Arc::downgrade(&state));
    Runner(Some(state))
}

#[cfg(test)]
#[path = "harness_task_exit_tests.rs"]
pub(crate) mod tests;
