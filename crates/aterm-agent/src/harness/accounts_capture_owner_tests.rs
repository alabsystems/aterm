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
