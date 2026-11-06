// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn detached_scope_retains_the_closed_epoch_until_its_last_drop() {
    let lifetime = Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let detached = scope.clone();
    let pause = lifetime.pause().unwrap();
    assert!(scope.cancelled());
    assert!(lifetime.enter().is_none());
    assert!(lifetime.pause().is_none());
    drop(scope);
    assert_eq!(lifetime.0.state.lock().unwrap().operations, 1);
    let pause = pause
        .try_wait()
        .expect_err("detached operation still owns the epoch");
    let (done, drained) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        done.send(pause.wait()).unwrap();
    });
    drop(detached);
    let token = drained.recv_timeout(Duration::from_secs(60)).unwrap();
    worker.join().unwrap();
    assert!(token.resume().is_ok());
    assert!(!lifetime.enter().unwrap().cancelled());
}

#[test]
fn dropped_pause_and_drained_tokens_leave_admissions_closed() {
    let paused = Lifetime::default();
    drop(paused.pause().unwrap());
    assert!(paused.enter().is_none());
    let drained = Lifetime::default();
    drop(drained.pause().unwrap().wait());
    assert!(drained.enter().is_none());
}

#[test]
fn stale_epoch_cannot_reopen_a_newer_closed_epoch() {
    let lifetime = Lifetime::default();
    let original = lifetime.pause().unwrap().wait();
    // Construct a stale token from the actual old identity, not a numeric epoch.
    let stale = Drained {
        shared: original.shared.clone(),
        epoch: original.epoch.clone(),
    };
    assert!(original.resume().is_ok());
    let next = lifetime.pause().unwrap().wait();
    assert!(stale.resume().is_err());
    assert!(lifetime.enter().is_none());
    assert!(next.resume().is_ok());
}

#[cfg(unix)]
#[test]
fn wrapped_work_retires_real_signin_before_its_count_in_every_disposition() {
    use crate::harness::accounts;
    struct Work {
        signin: Option<accounts::Lock>,
        path: std::path::PathBuf,
        lifetime: Lifetime,
        witnessed: Arc<Mutex<Option<(usize, bool, bool)>>>,
    }
    impl Drop for Work {
        fn drop(&mut self) {
            let count = self
                .lifetime
                .0
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .operations;
            let locked = accounts::Store::at(&self.path)
                .signin_now()
                .is_ok_and(|lock| lock.is_none());
            *self.witnessed.lock().unwrap_or_else(|e| e.into_inner()) =
                Some((count, self.signin.is_some(), locked));
            drop(self.signin.take());
        }
    }
    for disposition in ["run", "unstarted", "refused", "panic"] {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let store = accounts::Store::at(tmp.path());
        let lifetime = Lifetime::default();
        let witnessed = Arc::new(Mutex::new(None));
        let work = Work {
            signin: Some(store.signin_now().unwrap().unwrap()),
            path: tmp.path().to_path_buf(),
            lifetime: lifetime.clone(),
            witnessed: witnessed.clone(),
        };
        let wrapped = lifetime.enter().unwrap().wrap(move || {
            let _work = work;
            assert!(store.signin_now().unwrap().is_none());
            assert_ne!(disposition, "panic", "synthetic work panic");
        });
        let pause = lifetime.pause().unwrap();
        match disposition {
            "run" => wrapped(),
            "panic" => {
                assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(wrapped)).is_err())
            }
            "refused" => {
                fn refused(_: Box<dyn FnOnce() + Send>) {}
                refused(Box::new(wrapped));
            }
            _ => drop(wrapped),
        }
        assert_eq!(*witnessed.lock().unwrap(), Some((1, true, true)));
        assert_eq!(lifetime.0.state.lock().unwrap().operations, 0);
        assert!(pause.wait().resume().is_ok());
        assert!(
            accounts::Store::at(tmp.path())
                .signin_now()
                .unwrap()
                .is_some()
        );
    }
}
