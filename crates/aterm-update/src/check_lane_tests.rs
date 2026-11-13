// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn a_busy_check_is_reported_without_running_or_waiting_for_its_owner() {
    let lane = Lane::new();
    let request = source("one");
    let owner = lane.try_lock().unwrap();
    std::thread::scope(|scope| {
        let (sent, received) = std::sync::mpsc::channel();
        let (lane, request) = (&lane, &request);
        scope.spawn(move || {
            let completed = lane.run_or_join(42, request, || panic!("duplicate check"));
            let _ = sent.send(completed);
        });
        let result = received.recv_timeout(std::time::Duration::from_secs(60));
        drop(owner);
        assert!(!result.unwrap(), "the active owner remains untouched");
    });
    assert_eq!(lane.sequence.load(Ordering::Acquire), 0);
    let mut checked = false;
    assert!(lane.run_or_join(42, &request, || checked = true));
    assert!(
        checked,
        "a later explicit request can run after the owner retires"
    );
    let completion = lane.try_lock().unwrap();
    assert!(completion.answers(0, 42, &request));
}

fn source(owner: &str) -> Source {
    Source {
        owner: owner.into(),
        repo: "channel".into(),
    }
}

#[test]
fn a_join_requires_a_new_completion_for_the_requested_source_and_build() {
    let model = aterm_models_update::native_update_check_join_model();
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
    let model = aterm_models_update::native_update_check_join_model();
    let mut historical = model.init_state();
    historical.insert("poisoned", 1);
    historical.insert("recovered", 1);
    let Err(TryLockError::Poisoned(poisoned)) = lane.state.try_lock() else {
        panic!("expected the historical poisoned-lock arm");
    };
    drop(poisoned.into_inner());
    assert!(lane.state.is_poisoned());
    assert!(!model.check_invariant("RecoveryClearsPoison", &historical));

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

    let _ = std::panic::catch_unwind(|| {
        lane.run_or_join(42, &source, || panic!("second injected panic"));
    });
    let guard = lane
        .state
        .lock()
        .unwrap_or_else(|poisoned| lane.recover_poisoned(poisoned));
    assert!(guard.source.is_none());
    drop(guard);
    assert!(lane.try_lock().is_some());
}

#[test]
fn a_dedup_wait_releases_both_locks_and_does_not_block_manual_check() {
    let staging = crate::paths::Staging::scratch("checker-wait-release");
    let file_path = staging.root.join("checker.lock");
    let lane = Lane::new();
    let guard = lane.try_lock().unwrap();
    let gate = aterm_update_core::FileLock::acquire(&file_path).unwrap();
    let model = aterm_models_update::native_update_check_wait_model();
    let mut historical = model.init_state();
    historical.insert("sleeping", 1);
    historical.insert("file_held", 0);
    assert!(
        !model.check_invariant("NoSleepingCheckOwner", &historical),
        "retaining the real local guard across sleep is the caught negative control"
    );
    assert!(lane.try_lock().is_none(), "the lane is held");
    let mut waited = false;
    after_skip(gate, guard, 30, || {
        waited = true;
        let lane_free = lane.try_lock().is_some();
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
    assert!(lane.try_lock().is_some());
    let _ = std::fs::remove_dir_all(&staging.root);
}
