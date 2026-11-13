// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Update check lane deduplication and completion tracking.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, TryLockError};

use crate::Source;

#[derive(Default)]
pub(crate) struct Completion {
    sequence: u64,
    build: u64,
    source: Option<Source>,
}

impl Completion {
    fn answers(&self, before: u64, build: u64, source: &Source) -> bool {
        self.sequence != before
            && self.build == build
            && self.source.as_ref().is_some_and(|completed| {
                completed.owner.eq_ignore_ascii_case(&source.owner)
                    && completed.repo.eq_ignore_ascii_case(&source.repo)
            })
    }
}

pub(crate) struct Lane {
    state: Mutex<Completion>,
    sequence: AtomicU64,
}

impl Lane {
    pub(crate) const fn new() -> Self {
        Self {
            state: Mutex::new(Completion {
                sequence: 0,
                build: 0,
                source: None,
            }),
            sequence: AtomicU64::new(0),
        }
    }

    /// The lane, if no other checker holds it. A lane poisoned by a panicked
    /// checker is recovered here (its completion invalidated, the poison
    /// cleared), so the only refusal a caller ever sees is "busy".
    pub(crate) fn try_lock(&self) -> Option<MutexGuard<'_, Completion>> {
        match self.state.try_lock() {
            Ok(guard) => Some(guard),
            Err(TryLockError::Poisoned(poisoned)) => Some(self.recover_poisoned(poisoned)),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    fn recover_poisoned<'a>(
        &'a self,
        poisoned: std::sync::PoisonError<MutexGuard<'a, Completion>>,
    ) -> MutexGuard<'a, Completion> {
        let mut guard = poisoned.into_inner();
        *guard = Completion::default();
        self.state.clear_poison();
        guard
    }

    pub(crate) fn complete(&self, guard: &mut Completion, build: u64, source: &Source) {
        guard.sequence = self
            .sequence
            .fetch_add(1, Ordering::Release)
            .wrapping_add(1);
        guard.build = build;
        guard.source = Some(source.clone());
    }

    /// Run or join an exact completion that raced this call. Return false when
    /// another check still owns the lane; an explicit request never queues
    /// behind unbounded network work.
    pub(crate) fn run_or_join(&self, build: u64, source: &Source, check: impl FnOnce()) -> bool {
        let before = self.sequence.load(Ordering::Acquire);
        let Some(mut guard) = self.try_lock() else {
            return false;
        };
        self.finish_join(&mut guard, before, build, source, check);
        true
    }

    fn finish_join(
        &self,
        guard: &mut Completion,
        before: u64,
        build: u64,
        source: &Source,
        check: impl FnOnce(),
    ) {
        if !guard.answers(before, build, source) {
            check();
            self.complete(guard, build, source);
        }
    }
}

/// Drops both guards before the scheduler sleeps; `interval == 0` (single-check mode)
/// does not wait.
pub(crate) fn after_skip<G, L>(gate: G, lane: L, interval: u64, wait: impl FnOnce()) {
    drop(gate);
    drop(lane);
    if interval != 0 {
        wait();
    }
}

#[cfg(test)]
#[path = "check_lane_tests.rs"]
mod tests;
