// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A join consumes only a check completed during that request, for its source/build.
//! Holding the lane for a dedup decision does not manufacture a completed check.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, TryLockError, TryLockResult};

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

    pub(crate) fn try_lock(&self) -> TryLockResult<MutexGuard<'_, Completion>> {
        match self.state.try_lock() {
            Err(TryLockError::Poisoned(poisoned)) => Ok(self.recover_poisoned(poisoned)),
            result => result,
        }
    }

    fn recover_poisoned<'a>(
        &'a self,
        poisoned: std::sync::PoisonError<MutexGuard<'a, Completion>>,
    ) -> MutexGuard<'a, Completion> {
        let mut guard = poisoned.into_inner();
        // A panic cannot supply a completed-check answer. Invalidate before
        // clearing poison, while no other caller can acquire the lane.
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

    pub(crate) fn run_or_join(&self, build: u64, source: &Source, check: impl FnOnce()) {
        let before = self.sequence.load(Ordering::Acquire);
        let joined = match self.try_lock() {
            Ok(mut guard) => {
                check();
                self.complete(&mut guard, build, source);
                return;
            }
            Err(TryLockError::Poisoned(poisoned)) => {
                let mut guard = self.recover_poisoned(poisoned);
                check();
                self.complete(&mut guard, build, source);
                return;
            }
            Err(TryLockError::WouldBlock) => true,
        };
        if joined {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| self.recover_poisoned(poisoned));
            self.finish_join(&mut guard, before, build, source, check);
        }
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

/// Both guards are retired before the scheduler can sleep. The callback is also
/// omitted for the documented interval=0 single-check mode.
pub(crate) fn after_skip<G, L>(gate: G, lane: L, interval: u64, wait: impl FnOnce()) {
    drop(gate);
    drop(lane);
    if interval != 0 {
        wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(owner: &str) -> Source {
        Source {
            owner: owner.into(),
            repo: "channel".into(),
        }
    }

    #[test]
    fn a_join_requires_a_new_completion_for_the_requested_source_and_build() {
        let model = aterm_spec::derive::native_update_check_join_model();
        for fresh in [false, true] {
            for same_source in [false, true] {
                for same_build in [false, true] {
                    let lane = Lane::new();
                    let completed_source = source("one");
                    let requested_source = source(if same_source { "ONE" } else { "two" });
                    let mut guard = lane.try_lock().unwrap();
                    let before = if fresh {
                        let before = lane.sequence.load(Ordering::Acquire);
                        lane.complete(&mut guard, 42, &completed_source);
                        before
                    } else {
                        lane.complete(&mut guard, 42, &completed_source);
                        lane.sequence.load(Ordering::Acquire)
                    };
                    let requested_build = if same_build { 42 } else { 43 };
                    let mut state = model.init_state();
                    state.insert("fresh", i64::from(fresh));
                    state.insert("same_source", i64::from(same_source));
                    state.insert("same_build", i64::from(same_build));
                    let expected_join = model.action_enabled("Join", &state);
                    let mut checks = 0;
                    lane.finish_join(
                        &mut guard,
                        before,
                        requested_build,
                        &requested_source,
                        || checks += 1,
                    );
                    assert_eq!(
                        checks == 0,
                        expected_join,
                        "real joined transaction: {state:?}"
                    );
                    assert_eq!(guard.build, requested_build);
                    assert!(
                        guard
                            .source
                            .as_ref()
                            .unwrap()
                            .owner
                            .eq_ignore_ascii_case(&requested_source.owner)
                    );
                    if !expected_join {
                        state.insert("joined", 1);
                        assert!(
                            !model.check_invariant("ExactCompletedRequest", &state),
                            "historical unconditional join must be rejected"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_panicked_checker_invalidates_completion_and_the_next_cycle_runs() {
        let lane = Lane::new();
        let source = source("one");
        lane.run_or_join(42, &source, || {});
        let panicked = std::panic::catch_unwind(|| {
            lane.run_or_join(42, &source, || panic!("injected checker panic"));
        });
        assert!(panicked.is_err());
        assert!(
            lane.state.is_poisoned(),
            "the fixture really poisoned the shipping mutex"
        );
        let model = aterm_spec::derive::native_update_check_join_model();
        let mut historical = model.init_state();
        historical.insert("poisoned", 1);
        historical.insert("recovered", 1);
        // Former code took and dropped the guard without clearing poison.
        let Err(TryLockError::Poisoned(poisoned)) = lane.state.try_lock() else {
            panic!("expected the historical poisoned-lock arm");
        };
        drop(poisoned.into_inner());
        assert!(lane.state.is_poisoned());
        assert!(!model.check_invariant("RecoveryClearsPoison", &historical));

        // This is precisely the periodic loop's entry, not a test-only repair.
        let mut guard = lane
            .try_lock()
            .expect("periodic check can acquire immediately");
        assert!(!lane.state.is_poisoned());
        assert!(
            guard.source.is_none(),
            "no old or interrupted completion can be joined"
        );
        historical.insert("poisoned", i64::from(lane.state.is_poisoned()));
        historical.insert("fresh", i64::from(guard.source.is_some()));
        assert!(model.check_invariant("RecoveryClearsPoison", &historical));
        assert!(!model.action_enabled("Join", &historical));
        lane.complete(&mut guard, 42, &source);
        drop(guard);
        let mut checked = false;
        lane.run_or_join(42, &source, || checked = true);
        assert!(checked);
        assert!(!lane.state.is_poisoned());

        // The blocking join may be the first observer of another worker's panic.
        let _ = std::panic::catch_unwind(|| {
            lane.run_or_join(42, &source, || panic!("second injected panic"));
        });
        let guard = lane
            .state
            .lock()
            .unwrap_or_else(|poisoned| lane.recover_poisoned(poisoned));
        assert!(guard.source.is_none());
        drop(guard);
        assert!(lane.try_lock().is_ok());
    }

    #[test]
    fn a_dedup_wait_releases_both_locks_and_does_not_block_manual_check() {
        let staging = crate::paths::Staging::scratch("checker-wait-release");
        let file_path = staging.root.join("checker.lock");
        let lane = Lane::new();
        let guard = lane.try_lock().unwrap();
        let gate = aterm_update_core::FileLock::acquire(&file_path).unwrap();
        let model = aterm_spec::derive::native_update_check_wait_model();
        let mut historical = model.init_state();
        historical.insert("sleeping", 1);
        historical.insert("file_held", 0);
        assert!(
            !model.check_invariant("NoSleepingCheckOwner", &historical),
            "retaining the real local guard across sleep is the caught negative control"
        );
        assert!(matches!(lane.try_lock(), Err(TryLockError::WouldBlock)));
        let mut waited = false;
        after_skip(gate, guard, 30, || {
            waited = true;
            let lane_free = lane.try_lock().is_ok();
            // SAMPLED ONCE. `after_skip` has dropped `gate`, and a dropped
            // `FileLock` is free at once (`LOCK_UN`, 2026-09-24) even while a
            // sibling test's fork still holds a copy of its descriptor — so the
            // five-second poll this used to need to wait out that window is gone,
            // and one `WouldBlock` here means the lock really is held across the
            // sleep.
            let file_free =
                aterm_update_core::FileLock::acquire_within(&file_path, std::time::Duration::ZERO)
                    .is_ok();
            let mut state = model.init_state();
            state.insert("sleeping", 1);
            state.insert("lane_held", i64::from(!lane_free));
            state.insert("file_held", i64::from(!file_free));
            assert!(
                model.check_invariant("NoSleepingCheckOwner", &state),
                "{state:?}"
            );
            let mut checked = false;
            lane.run_or_join(42, &source("one"), || checked = true);
            assert!(checked, "a manual check can run while the scheduler waits");
        });
        assert!(waited);
        let mut once_waited = false;
        after_skip((), lane.try_lock().unwrap(), 0, || once_waited = true);
        assert!(
            !once_waited,
            "interval=0 dedup must stop without another wait"
        );
        assert!(lane.try_lock().is_ok());
        let _ = std::fs::remove_dir_all(staging.root);
    }
}
