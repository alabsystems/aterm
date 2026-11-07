// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn cancellation_observers_complete_while_the_actual_state_mutex_is_held() {
    for paused in [false, true] {
        let lifetime = Lifetime::default();
        let scope = lifetime.enter().unwrap();
        let cancellation = scope.cancellation();
        let pause = paused.then(|| lifetime.pause().unwrap());
        let state = lifetime.0.state.lock().unwrap();
        let worker_scope = scope.clone();
        let worker_cancellation = cancellation.clone();
        let worker_lifetime = lifetime.clone();
        let (send, receive) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let observed = (
                worker_scope.cancelled(),
                worker_cancellation.cancelled(),
                worker_cancellation.retired(),
                worker_cancellation.owned_by(&worker_lifetime),
            );
            let _ = send.send(observed);
        });
        let observed = receive.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
        // An observation must arrive before unlock; release even on a regression.
        drop(state);
        let joined = worker.join();
        joined.unwrap();
        assert_eq!(observed.unwrap(), (paused, paused, false, true));
        drop(scope);
        assert!(cancellation.retired());
        assert!(cancellation.cancelled());
        if let Some(pause) = pause {
            pause.try_wait().unwrap().resume().unwrap();
            assert!(
                cancellation.cancelled(),
                "old epoch never becomes live again"
            );
            assert!(!lifetime.enter().unwrap().cancelled());
        }
    }
}

#[test]
fn final_scope_drop_stops_work_before_its_blocked_count_retirement() {
    let lifetime = Lifetime::default();
    let mut scope = lifetime.enter().unwrap();
    let other = lifetime.enter().unwrap();
    let other_cancellation = other.cancellation();
    let (dropping_send, dropping_receive) = mpsc::channel();
    Arc::get_mut(&mut scope.0).unwrap().dropping = Some(dropping_send);
    let cancellation = scope.cancellation();
    let state = lifetime.0.state.lock().unwrap();
    let worker = std::thread::spawn(move || drop(scope));
    let dropping = dropping_receive.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    let counted = state.operations;
    let observer_cancellation = cancellation.clone();
    let (send, receive) = mpsc::channel();
    let observer = std::thread::spawn(move || {
        let _ = send.send((
            observer_cancellation.cancelled(),
            observer_cancellation.retired(),
            other_cancellation.cancelled(),
        ));
    });
    let observed = receive.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    drop(state);
    let joined = worker.join();
    let observer_joined = observer.join();
    dropping.expect("actual final Operation destructor entered");
    joined.unwrap();
    observer_joined.unwrap();
    assert_eq!(counted, 2, "blocked destructor has not retired its count");
    assert_eq!(observed.unwrap(), (true, false, false));
    assert!(cancellation.retired());
    assert!(cancellation.cancelled());
    assert!(!other.cancelled());
    let pause = lifetime.pause().unwrap();
    let pause = pause
        .try_wait()
        .expect_err("unrelated original work still owns its count");
    drop(other);
    let drained = pause.wait();
    assert!(cancellation.retired());
    drained.resume().unwrap();
    assert!(!lifetime.enter().unwrap().cancelled());
}

#[test]
fn actual_drained_receipt_observes_final_retirement_already_published() {
    let lifetime = Lifetime::default();
    let mut scope = lifetime.enter().unwrap();
    let (dropping_send, dropping_receive) = mpsc::channel();
    Arc::get_mut(&mut scope.0).unwrap().dropping = Some(dropping_send);
    let cancellation = scope.cancellation();
    let pause = lifetime.pause().unwrap();
    let state = lifetime.0.state.lock().unwrap();
    let worker = std::thread::spawn(move || drop(scope));
    let dropping = dropping_receive.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    let counted = state.operations;
    let observer_cancellation = cancellation.clone();
    let (send, receive) = mpsc::channel();
    let observer = std::thread::spawn(move || {
        let _ = send.send((
            observer_cancellation.cancelled(),
            observer_cancellation.retired(),
        ));
    });
    let observed = receive.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    let drain_cancellation = cancellation.clone();
    let (drained_send, drained_receive) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let drained = pause.wait();
        let _ = drained_send.send((drained, drain_cancellation.retired()));
    });
    drop(state);
    let joined = worker.join();
    let observer_joined = observer.join();
    let received = drained_receive.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    let waiter_joined = waiter.join();
    dropping.expect("actual final Operation destructor entered");
    joined.unwrap();
    observer_joined.unwrap();
    waiter_joined.unwrap();
    assert_eq!(counted, 1);
    assert_eq!(observed.unwrap(), (true, false));
    let (drained, retired) = received.unwrap();
    assert!(
        retired,
        "count-zero proof must follow retirement publication"
    );
    assert!(cancellation.retired());
    drained.resume().unwrap();
    assert!(cancellation.cancelled());
    assert!(!lifetime.enter().unwrap().cancelled());
}

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
        signin: Option<accounts::SigninTurn>,
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

#[test]
fn operation_cancellation_is_isolated_and_does_not_own_retirement() {
    let lifetime = Lifetime::default();
    let first = lifetime.enter().unwrap();
    let detached = first.clone();
    let other = lifetime.enter().unwrap();
    let cancellation = first.cancellation();
    assert!(!cancellation.cancelled());
    cancellation.cancel();
    assert!(cancellation.cancelled());
    assert!(first.cancelled());
    assert!(detached.cancelled());
    assert!(!other.cancelled());
    assert!(!cancellation.retired());
    drop(first);
    assert!(!cancellation.retired());
    drop(detached);
    assert!(cancellation.retired());
    assert!(cancellation.cancelled());
    assert!(!other.cancelled());
    assert!(!lifetime.enter().unwrap().cancelled());
}

#[test]
fn cancellation_observation_follows_epoch_pause_without_retaining_work() {
    let lifetime = Lifetime::default();
    let work = lifetime.enter().unwrap();
    let cancellation = work.cancellation();
    assert!(!cancellation.cancelled());
    let paused = lifetime.pause().unwrap();
    assert!(cancellation.cancelled());
    drop(work);
    assert!(cancellation.retired());
    paused.try_wait().unwrap().resume().unwrap();
    assert!(cancellation.cancelled());
    assert!(!lifetime.enter().unwrap().cancelled());
}

#[test]
fn release_turn_is_counted_before_seal_and_cannot_invalidate_drained() {
    let lifetime = Lifetime::default();
    let pause = lifetime.pause().unwrap();
    assert!(lifetime.enter().is_none());
    let release = lifetime
        .release_turn()
        .expect("retained cleanup remains possible before seal");
    let pause = pause
        .try_wait()
        .expect_err("release actor owns counted retirement custody");
    drop(release);
    let drained = pause.try_wait().unwrap();
    assert!(
        lifetime.release_turn().is_none(),
        "late cleanup cannot invalidate issued Drained"
    );
    assert_eq!(lifetime.0.state.lock().unwrap().operations, 0);
    drained.resume().unwrap();
    assert!(lifetime.release_turn().is_some());
}

#[test]
fn late_release_waiter_cannot_enter_after_blocking_drain_seals() {
    let lifetime = Lifetime::default();
    let (send, receive) = mpsc::channel();
    let other = lifetime.clone();
    let worker = std::thread::spawn(move || {
        receive.recv_timeout(Duration::from_secs(60)).unwrap();
        other.release_turn().is_none()
    });
    let drained = lifetime.pause().unwrap().wait();
    send.send(()).unwrap();
    assert!(worker.join().unwrap());
    assert_eq!(lifetime.0.state.lock().unwrap().operations, 0);
    drained.resume().unwrap();
}
