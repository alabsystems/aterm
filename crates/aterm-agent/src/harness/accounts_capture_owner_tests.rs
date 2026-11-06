// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Original automatic renewal resources stay on the caller stack until capture settles.

use super::*;
use std::sync::{Arc, Condvar, Mutex, mpsc};

#[derive(Debug, PartialEq, Eq)]
enum Step {
    Retrying,
    Returned,
}

struct Staging(PathBuf);

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn scoped_detached_workers_retain_admission_until_unstarted_work_drops() {
    for vault in [false, true] {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let store = accounts::Store::at(tmp.path());
        let signin = store.signin_now().unwrap().unwrap();
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let permit = AutoRenewPermit::claim(&guard, "owned", Instant::now(), true).unwrap();
        let queued = Mutex::new(Vec::<Box<dyn FnOnce() + Send>>::new());
        let spawn = |work| {
            queued.lock().unwrap().push(work);
            Ok(())
        };
        let lifetime = crate::harness::account_work::Lifetime::default();
        let scope = lifetime.enter().unwrap();
        with_scope_effects(
            scope,
            UpkeepEffects {
                spawn_renewal: &spawn,
                spawn_vault: &spawn,
                ..live_effects()
            },
            |fx| {
                if vault {
                    (fx.spawn_vault)(Box::new(move || {
                        let _signin = signin;
                        let _permit = permit;
                    }))
                    .unwrap();
                } else {
                    start_auto_renew(
                        permit,
                        move || {
                            let _signin = signin;
                        },
                        fx.spawn_renewal,
                    )
                    .unwrap();
                }
            },
        );
        assert!(store.signin_now().unwrap().is_none());
        assert!(guard.lock().unwrap().in_flight);
        let pause = lifetime
            .pause()
            .unwrap()
            .try_wait()
            .expect_err("detached work remains owned");
        queued.lock().unwrap().clear();
        let drained = pause.try_wait().expect("all actual work has retired");
        assert!(store.signin_now().unwrap().is_some());
        assert!(!guard.lock().unwrap().in_flight);
        assert!(drained.resume().is_ok());
    }
}

#[test]
fn cancelled_upkeep_and_rotation_do_not_read_or_prepare_account_state() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let lifetime = crate::harness::account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let pause = lifetime.pause().unwrap();
    let home = tmp.path().join("not-created-home");
    let state = tmp.path().join("not-created-state");
    let roster = tmp.path().join("not-created-config/accounts.toml");
    assert_eq!(
        upkeep_live_scoped(scope.clone(), &home, &state, &roster, 1, UpkeepStep::Renew),
        Upkept::Off
    );
    assert!(!auto_renew_next_account_interactive_for_scoped(
        scope,
        Cli::Claude,
        &home,
        &state,
        &roster,
        1
    ));
    assert!(!home.exists());
    assert!(!state.exists());
    assert!(!roster.parent().unwrap().exists());
    assert!(
        pause
            .try_wait()
            .expect("all actual work has retired")
            .resume()
            .is_ok()
    );
}

#[test]
fn scoped_permits_restore_only_cancelled_or_unstarted_retry_claims() {
    for kind in 0..3 {
        for mode in 0..4 {
            let at = Instant::now();
            let before = at - std::time::Duration::from_secs(1_801);
            let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
            guard
                .lock()
                .unwrap()
                .last_attempt
                .insert("owned".into(), before);
            let source = accounts::shared::Source {
                network: [1; 32],
                account: [2; 32],
            };
            let grant = crate::harness::cli::shared_renewal::Grant {
                source,
                decision: [3; 32],
                generation: 1,
                configuration: [4; 32],
                identity: "synthetic@example.test".into(),
                expected_principal: None,
                required_browser_profile: None,
            };
            let native = crate::harness::native_keys::PreparationId {
                key: "owned".into(),
                revision: [5; 32],
            };
            guard
                .lock()
                .unwrap()
                .shared_preparations
                .insert("owned".into(), (source, grant.decision));
            guard
                .lock()
                .unwrap()
                .native_preparations
                .insert("owned".into(), native.revision);
            let mut permit = match kind {
                0 => AutoRenewPermit::claim(&guard, "owned", at, true),
                1 => AutoRenewPermit::claim_native_preparation(&guard, at, &native),
                _ => AutoRenewPermit::claim_preparation(&guard, "owned", at, &grant),
            }
            .unwrap();
            let lifetime = crate::harness::account_work::Lifetime::default();
            let scope = lifetime.enter().unwrap();
            let completion = permit.follow_scope(Some(&scope));
            let paused = Arc::new(Mutex::new(None));
            let from_work = paused.clone();
            let from_lifetime = lifetime.clone();
            let queued = Mutex::new(Vec::<Box<dyn FnOnce() + Send>>::new());
            let spawn = |work| {
                queued.lock().unwrap().push(work);
                Ok(())
            };
            with_scope_effects(
                scope,
                UpkeepEffects {
                    spawn_renewal: &spawn,
                    ..live_effects()
                },
                |fx| {
                    start_auto_renew(
                        permit,
                        move || {
                            if mode == 2 {
                                completion.complete();
                                *from_work.lock().unwrap() = from_lifetime.pause();
                            }
                        },
                        fx.spawn_renewal,
                    )
                    .unwrap();
                },
            );
            if mode == 1 {
                *paused.lock().unwrap() = lifetime.pause();
            }
            let newer = at + std::time::Duration::from_secs(1);
            if mode == 3 {
                guard
                    .lock()
                    .unwrap()
                    .last_attempt
                    .insert("owned".into(), newer);
            }
            if matches!(mode, 1 | 2) {
                queued.lock().unwrap().pop().unwrap()();
            } else {
                queued.lock().unwrap().clear();
            }
            let pause = paused
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| lifetime.pause().unwrap());
            let drained = pause.try_wait().expect("original work and permit retired");
            let held = guard.lock().unwrap();
            assert!(!held.in_flight);
            assert_eq!(
                held.last_attempt.get("owned").copied(),
                Some(match mode {
                    2 => at,
                    3 => newer,
                    _ => before,
                })
            );
            drop(held);
            assert!(drained.resume().is_ok());
        }
    }
}

#[test]
fn issuer_unavailability_restores_retry_without_closing_local_scope_and_completion_wins() {
    for completed in [false, true] {
        let at = Instant::now();
        let before = at - std::time::Duration::from_secs(1_801);
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        guard
            .lock()
            .unwrap()
            .last_attempt
            .insert("owned".into(), before);
        let lifetime = crate::harness::account_work::Lifetime::default();
        let scope = lifetime.enter().unwrap();
        let mut permit = AutoRenewPermit::claim(&guard, "owned", at, true).unwrap();
        let receipt = permit.follow_scope(Some(&scope));
        permit.start();
        receipt.defer();
        if completed {
            receipt.complete();
            receipt.defer();
        }
        assert!(
            !scope.cancelled(),
            "issuer unavailability is not local cancellation"
        );
        drop(permit);
        let state = guard.lock().unwrap();
        assert!(!state.in_flight);
        assert_eq!(
            state.last_attempt.get("owned").copied(),
            Some(if completed { at } else { before })
        );
        drop(state);
        drop(scope);
        assert!(
            lifetime
                .pause()
                .unwrap()
                .try_wait()
                .expect("all owners retired")
                .resume()
                .is_ok()
        );
    }
}

#[test]
fn uncertain_capture_wait_retains_the_original_signin_permit_and_staging() {
    for inject_failure in [false, true] {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let store = accounts::Store::at(tmp.path());
        let signin = store.signin_now().unwrap().unwrap();
        let staging = store.claim_staging_dir("work").unwrap();
        let retained_staging = Staging(staging.clone());
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let permit = AutoRenewPermit::claim(&guard, "work", Instant::now(), true).unwrap();
        let recovery = Arc::new((Mutex::new(false), Condvar::new()));
        let recover_wait = recovery.clone();
        let (events, received) = mpsc::channel();
        let retry_event = events.clone();
        let returned = Arc::new(Mutex::new(None));
        let captured = returned.clone();
        let lifetime = crate::harness::account_work::Lifetime::default();
        let scope = lifetime.enter().unwrap();
        let witness = Arc::new(super::super::align::CaptureCleanupWitness::default());
        let child_witness = witness.clone();
        let mut worker = None;
        start_auto_renew(
            permit,
            move || {
                let _signin = signin;
                let _staging = retained_staging;
                let mut cmd = aterm_uds::spawnfd::command(crate::test_program::kept("/bin/sleep"));
                cmd.arg("120");
                let mut first = true;
                let result = super::super::align::capture_with_owning_wait(
                    cmd,
                    move |child| {
                        if inject_failure && first {
                            first = false;
                            return Err(io::Error::from(io::ErrorKind::WouldBlock));
                        }
                        if inject_failure {
                            let _ = retry_event.send(Step::Retrying);
                            let (ready, changed) = &*recover_wait;
                            let mut ready = ready
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            while !*ready {
                                ready = changed
                                    .wait(ready)
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                            }
                        }
                        child.wait()
                    },
                    child_witness,
                );
                *captured.lock().unwrap() = Some(result);
            },
            |work| {
                let work = scope.wrap(work);
                worker = Some(std::thread::spawn(move || {
                    work();
                    // The actual start_auto_renew wrapper and its permit have dropped here.
                    let _ = events.send(Step::Returned);
                }));
                Ok(())
            },
        )
        .unwrap();

        let step = received.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
        let admission = store.signin_now();
        let permit_held = guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .in_flight;
        let staging_held = staging.is_dir();
        let paused = lifetime.pause().unwrap().try_wait();
        let reaped_before_recovery = witness
            .child_reaped
            .load(std::sync::atomic::Ordering::Acquire);
        let joined_before_recovery = witness
            .reader_joined
            .load(std::sync::atomic::Ordering::Acquire);
        // Recover and join before assertions, including on the old return-on-error path.
        let (ready, changed) = &*recovery;
        *ready
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        changed.notify_all();
        let joined = worker.unwrap().join();

        joined.unwrap();
        let admission = admission.unwrap();
        if inject_failure {
            assert!(
                paused.is_err(),
                "unsettled capture retired its App work scope"
            );
            assert!(
                !reaped_before_recovery,
                "the original owning wait was not yet positive"
            );
            assert!(
                joined_before_recovery,
                "the reader must join before retained wait retries"
            );
            assert_eq!(
                step.unwrap(),
                Step::Retrying,
                "capture returned while owning wait was uncertain"
            );
            assert!(
                admission.is_none(),
                "original machine sign-in lock was released before settlement"
            );
            assert!(
                permit_held,
                "original automatic permit was released before settlement"
            );
            assert!(
                staging_held,
                "staging was removed while capture was unsettled"
            );
        } else {
            assert!(paused.is_ok());
            assert_eq!(step.unwrap(), Step::Returned);
            assert!(admission.is_some());
            assert!(!permit_held);
            assert!(!staging_held);
            assert_eq!(
                returned.lock().unwrap().as_ref().unwrap().as_ref().unwrap(),
                &super::super::align::Ended::Stopped
            );
        }
        drop(admission);
        assert!(store.signin_now().unwrap().is_some());
        assert!(!guard.lock().unwrap().in_flight);
        assert!(!staging.exists());
        let drained = match paused {
            Ok(drained) => drained,
            Err(pause) => pause.try_wait().expect("all actual work has retired"),
        };
        assert!(drained.resume().is_ok());
        assert!(
            witness
                .child_reaped
                .load(std::sync::atomic::Ordering::Acquire)
        );
        assert!(
            witness
                .reader_joined
                .load(std::sync::atomic::Ordering::Acquire)
        );
    }
}
