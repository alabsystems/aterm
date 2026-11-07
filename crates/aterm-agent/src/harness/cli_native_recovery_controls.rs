// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::harness::{account_work, codex_enrollment as enrollment, wake_deadline};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

struct WriterHost {
    now: i64,
    work: account_work::Scope,
    store: accounts::Store,
    sleeper: wake_deadline::Sleeper,
    before: mpsc::Sender<wake_deadline::Sleeper>,
    before_release: Mutex<mpsc::Receiver<()>>,
    entered: mpsc::Sender<bool>,
    writer_release: Mutex<mpsc::Receiver<()>>,
    after_now: AtomicBool,
}

impl AccountsHost for WriterHost {
    fn work_scope(&self) -> Option<account_work::Scope> {
        Some(self.work.clone())
    }
    fn work_cancelled(&self) -> bool {
        if self.after_now.swap(false, Ordering::AcqRel) {
            let held = self.store.try_acquire().is_ok_and(|lock| lock.is_none());
            let _ = self.entered.send(held);
            self.writer_release
                .lock()
                .unwrap()
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("controller releases actual publication writer");
        }
        self.work.cancelled()
    }
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("local recovery starts no status process")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("local recovery starts no login")
    }
    fn renew_flow(&self, _: &str, _: &str, _: &RenewBrowser) -> Result<i32, String> {
        panic!("local recovery starts no provider flow")
    }
    fn vendor_sign_in(
        &self,
        _: &crate::harness::login_drive::Request<'_>,
    ) -> crate::harness::login_drive::Drove {
        panic!("a completed native response cannot dispatch another provider")
    }
    fn codex_account(&self, _: &Path) -> Option<String> {
        panic!("recovery verifies the local response without a provider observation")
    }
    fn now(&self) -> i64 {
        let _ = self.before.send(self.sleeper);
        self.before_release
            .lock()
            .unwrap()
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("controller releases post-materialization phase");
        self.after_now.store(true, Ordering::Release);
        self.now
    }
}

#[derive(Clone, Copy)]
enum Change {
    Live,
    Selection,
    Scope,
    Sleep,
    Signer,
}

struct OriginalBudgetHost {
    now: i64,
    work: account_work::Scope,
    store: accounts::Store,
    sleeper: wake_deadline::Sleeper,
    entered: mpsc::Sender<(wake_deadline::Sleeper, PathBuf)>,
    release: Mutex<mpsc::Receiver<()>>,
    provider_done: AtomicBool,
    gated: AtomicBool,
    calls: std::cell::Cell<usize>,
    stage: RefCell<Option<PathBuf>>,
}

impl AccountsHost for OriginalBudgetHost {
    fn work_scope(&self) -> Option<account_work::Scope> {
        Some(self.work.clone())
    }
    fn work_cancelled(&self) -> bool {
        if self.provider_done.load(Ordering::Acquire) && !self.gated.load(Ordering::Acquire) {
            match self.store.try_acquire().expect("publication writer probe") {
                Some(writer) => drop(writer),
                None => {
                    self.gated.store(true, Ordering::Release);
                    let _ = self.entered.send((
                        self.sleeper,
                        self.stage.borrow().clone().expect("completed native stage"),
                    ));
                    self.release
                        .lock()
                        .unwrap()
                        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                        .expect("controller releases original publication writer");
                }
            }
        }
        self.work.cancelled()
    }
    fn now(&self) -> i64 {
        self.now
    }
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("native completion needs no status process")
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("native completion needs no login process")
    }
    fn renew_flow(&self, _: &str, _: &str, _: &RenewBrowser) -> Result<i32, String> {
        panic!("native completion needs no second flow")
    }
    fn codex_account(&self, home: &Path) -> Option<String> {
        crate::harness::codex_app_login::read_account_email(home)
    }
    fn vendor_sign_in(
        &self,
        request: &crate::harness::login_drive::Request<'_>,
    ) -> crate::harness::login_drive::Drove {
        self.calls.set(self.calls.get() + 1);
        *self.stage.borrow_mut() = Some(request.staging.to_path_buf());
        let drove = crate::harness::codex_app_login::tests::native_enrollment_exchange_fixture(
            request,
            None,
            self.now,
            Some(&self.work),
            "main@example.test",
        );
        self.provider_done.store(true, Ordering::Release);
        drove
    }
}

#[test]
fn original_native_completion_keeps_its_exchange_budget_inside_the_publication_writer() {
    for change in 0..3 {
        let bed = Bed::new(Cli::Codex);
        let request = enrollment_request(&bed);
        let old_auth = std::fs::read(bed.dirs[0].join("auth.json")).unwrap();
        let lifetime = account_work::Lifetime::default();
        let scope = lifetime.enter().unwrap();
        let host_work = scope.clone();
        let (work, control) = enrollment::owned_work(request, scope);
        let (entered_send, entered_receive) = mpsc::channel();
        let (release_send, release_receive) = mpsc::channel();
        let env = bed.env.clone();
        let store = bed.store.clone();
        let worker = std::thread::spawn(move || {
            let seam = wake_deadline::SleepSeam::hold();
            seam.sleeper().slept(std::time::Duration::from_secs(37));
            let host = OriginalBudgetHost {
                now: env.now + 1,
                work: host_work,
                store,
                sleeper: seam.sleeper(),
                entered: entered_send,
                release: Mutex::new(release_receive),
                provider_done: AtomicBool::new(false),
                gated: AtomicBool::new(false),
                calls: std::cell::Cell::new(0),
                stage: RefCell::new(None),
            };
            let result = work.run_with_host(&env, &host);
            let calls = host.calls.get();
            let stage = host.stage.borrow().clone();
            drop(host);
            (result, calls, stage)
        });
        let entered = entered_receive.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
        if change == 1
            && let Ok((sleeper, _)) = &entered
        {
            sleeper.slept(aterm_spec::wait::MUST_HAPPEN + std::time::Duration::from_secs(1));
        }
        let removed = if change == 2 {
            entered
                .as_ref()
                .ok()
                .map(|(_, stage)| std::fs::remove_file(stage.join("auth.json.refreshed")))
        } else {
            None
        };
        let released = release_send.send(());
        let joined = worker.join();
        // Recover the real worker and release every gate before inspecting the outcome.
        assert!(
            entered.is_ok(),
            "native completion reached its actual publication writer"
        );
        assert!(released.is_ok());
        if let Some(removed) = removed {
            removed.unwrap();
        }
        let (result, calls, stage) = joined.unwrap();
        let result = result.unwrap();
        let stage = stage.unwrap();
        control
            .retired()
            .expect("joined original work retired")
            .finish(&bed.env)
            .unwrap();
        assert_eq!(calls, 1);
        let records = bed
            .store
            .read_account_records(Some(&bed.path), bed.env.now)
            .unwrap();
        if change != 0 {
            assert_eq!(result.phase, enrollment::Phase::Running);
            assert_eq!(result.generation, None);
            assert_eq!(stage.join("auth.json.refreshed").is_file(), change != 2);
            assert!(stage.join("auth.json").is_file());
            assert!(!stage.join(accounts::RENEWAL_CHECKPOINT_FILE).exists());
            assert_eq!(
                std::fs::read(bed.dirs[0].join("auth.json")).unwrap(),
                old_auth
            );
        } else {
            assert_eq!(result.phase, enrollment::Phase::Applied);
            assert!(result.generation.is_some());
            assert_eq!(
                records.logins.get("main").unwrap().dir.as_deref(),
                stage.to_str()
            );
            assert!(stage.join(accounts::RENEWAL_CHECKPOINT_FILE).is_file());
            assert!(!stage.join("auth.json.refreshed").exists());
        }
        assert!(bed.store.signin_now().unwrap().is_some());
        lifetime
            .pause()
            .unwrap()
            .try_wait()
            .expect("original scopes retired after join");
    }
}

#[test]
fn native_recovery_rechecks_the_real_publication_writer_after_selection_sleep_scope_and_signer_changes()
 {
    use crate::harness::codex_app_login::EnrollmentCut;
    for change in [
        Change::Live,
        Change::Selection,
        Change::Scope,
        Change::Sleep,
        Change::Signer,
    ] {
        let bed = Bed::new(Cli::Codex);
        let previous_selection = bed.store.request_pin("other", bed.env.now).unwrap();
        let request = enrollment_request(&bed);
        let old_auth = std::fs::read(bed.dirs[0].join("auth.json")).unwrap();
        let original_lifetime = account_work::Lifetime::default();
        let original_host = SealedEnrollmentHost {
            now: bed.env.now + 1,
            cut: Some(EnrollmentCut::AfterAuthFlush),
            calls: std::cell::Cell::new(0),
            stage: RefCell::new(None),
        };
        let (original, original_control) =
            enrollment::owned_work(request.clone(), original_lifetime.enter().unwrap());
        assert_eq!(
            original
                .run_with_host(&bed.env, &original_host)
                .unwrap()
                .phase,
            enrollment::Phase::Running
        );
        original_control
            .retired()
            .unwrap()
            .finish(&bed.env)
            .unwrap();
        let before = bed
            .store
            .read_account_records(Some(&bed.path), bed.env.now)
            .unwrap();
        let old_binding = before.logins.get("main").unwrap().clone();
        let stage = original_host.stage.borrow().clone().unwrap();
        let journal = stage.join("auth.json.refreshed");
        let response = std::fs::read(&journal).unwrap();
        let candidate = std::fs::read(stage.join("auth.json")).unwrap();
        let lifetime = account_work::Lifetime::default();
        let scope = lifetime.enter().unwrap();
        let host_work = scope.clone();
        let (work, control) = enrollment::owned_work(request.clone(), scope);
        let (before_send, before_receive) = mpsc::channel();
        let (before_release, before_wait) = mpsc::channel();
        let (entered_send, entered_receive) = mpsc::channel();
        let (writer_release, writer_wait) = mpsc::channel();
        let env = bed.env.clone();
        let store = bed.store.clone();
        let worker = std::thread::spawn(move || {
            let seam = wake_deadline::SleepSeam::hold();
            seam.sleeper().slept(std::time::Duration::from_secs(37));
            let host = WriterHost {
                now: env.now + 2,
                work: host_work,
                store,
                sleeper: seam.sleeper(),
                before: before_send,
                before_release: Mutex::new(before_wait),
                entered: entered_send,
                writer_release: Mutex::new(writer_wait),
                after_now: AtomicBool::new(false),
            };
            work.run_with_host(&env, &host)
        });
        let prepared = before_receive.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
        let replacement = if prepared.is_ok() && matches!(change, Change::Selection) {
            Some(bed.store.request_pin("other", bed.env.now + 3))
        } else {
            None
        };
        let held_writer = prepared
            .as_ref()
            .ok()
            .and_then(|_| bed.store.acquire_within(&[]).ok().flatten());
        let held_actual_writer = held_writer.is_some();
        let released_before = before_release.send(());
        drop(held_writer);
        let entered = entered_receive.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
        let paused = if entered.is_ok() && matches!(change, Change::Scope) {
            lifetime.pause()
        } else {
            None
        };
        if entered.is_ok()
            && matches!(change, Change::Sleep)
            && let Ok(sleeper) = &prepared
        {
            sleeper.slept(crate::harness::login_drive::HANG + std::time::Duration::from_secs(1));
        }
        let signin = bed.store.shared_dir().join(accounts::SIGNIN_LOCK_FILE);
        let kept_signin = bed._root.path().join("original-signer");
        let replaced_signer = if entered.is_ok() && matches!(change, Change::Signer) {
            Some(
                std::fs::rename(&signin, &kept_signin)
                    .and_then(|()| aterm_uds::atomic_file::write(&signin, b"", 0o600)),
            )
        } else {
            None
        };
        let released_writer = writer_release.send(());
        let joined = worker.join();
        let restored_signer = if kept_signin.exists() {
            Some(std::fs::rename(&kept_signin, &signin))
        } else {
            None
        };
        // Every controller gate and real file lock is released before any assertion.
        assert!(prepared.is_ok());
        assert!(held_actual_writer);
        assert!(released_before.is_ok());
        assert!(
            entered.unwrap(),
            "the callback ran while the actual account writer was held"
        );
        assert!(released_writer.is_ok());
        if let Some(replaced) = replaced_signer {
            replaced.unwrap();
        }
        if let Some(restored) = restored_signer {
            restored.unwrap();
        }
        let result = joined.unwrap().unwrap();
        control
            .retired()
            .expect("joined host and work released every recovery scope")
            .finish(&bed.env)
            .unwrap();
        let records = bed
            .store
            .read_account_records(Some(&bed.path), bed.env.now)
            .unwrap();
        if matches!(change, Change::Live) {
            assert_eq!(result.phase, enrollment::Phase::Applied);
            assert!(result.generation.is_some());
            assert_eq!(
                records.logins.get("main").unwrap().dir.as_deref(),
                stage.to_str()
            );
            assert!(stage.join(accounts::RENEWAL_CHECKPOINT_FILE).is_file());
            assert!(!journal.exists());
        } else {
            assert_eq!(result.phase, enrollment::Phase::Running);
            assert_eq!(result.generation, None);
            assert_eq!(records.logins.get("main"), Some(&old_binding));
            assert_eq!(std::fs::read(&journal).unwrap(), response);
            assert_eq!(std::fs::read(stage.join("auth.json")).unwrap(), candidate);
            assert!(!stage.join(accounts::RENEWAL_CHECKPOINT_FILE).exists());
        }
        let expected_selection = replacement
            .map(|value| value.unwrap())
            .unwrap_or(previous_selection);
        assert_eq!(
            records
                .state
                .pending_pin
                .as_ref()
                .and_then(accounts::PendingSelection::account),
            Some(&expected_selection)
        );
        assert_eq!(
            std::fs::read(bed.dirs[0].join("auth.json")).unwrap(),
            old_auth
        );
        assert_eq!(original_host.calls.get(), 1);
        assert!(bed.store.signin_now().unwrap().is_some());
        if let Some(paused) = paused {
            paused
                .try_wait()
                .expect("all stopped recovery resources retired");
        } else {
            lifetime
                .pause()
                .unwrap()
                .try_wait()
                .expect("all recovery resources retired");
        }
        original_lifetime
            .pause()
            .unwrap()
            .try_wait()
            .expect("original resources retired");
    }
}

#[test]
fn applied_native_cleanup_is_targeted_and_refuses_a_replaced_physical_stage() {
    use crate::harness::codex_app_login::EnrollmentCut;
    let bed = Bed::new(Cli::Codex);
    let request = enrollment_request(&bed);
    let lifetime = account_work::Lifetime::default();
    let host = SealedEnrollmentHost {
        now: bed.env.now + 1,
        cut: Some(EnrollmentCut::AfterAuthFlush),
        calls: std::cell::Cell::new(0),
        stage: RefCell::new(None),
    };
    let (original, original_control) =
        enrollment::owned_work(request.clone(), lifetime.enter().unwrap());
    assert_eq!(
        original.run_with_host(&bed.env, &host).unwrap().phase,
        enrollment::Phase::Running
    );
    original_control
        .retired()
        .unwrap()
        .finish(&bed.env)
        .unwrap();
    let stage = host.stage.borrow().clone().unwrap();
    let journal = stage.join("auth.json.refreshed");
    let response = std::fs::read(&journal).unwrap();
    let (recovery, _) = enrollment::owned_work(request.clone(), lifetime.enter().unwrap());
    assert_eq!(
        recovery.run_with_host(&bed.env, &host).unwrap().phase,
        enrollment::Phase::Applied
    );
    assert!(!journal.exists());
    let candidate = std::fs::read(stage.join("auth.json")).unwrap();
    let committed = bed.store.load_logins(bed.env.now).unwrap().0;

    // Restore the exact original response to represent a crash after Applied
    // committed but before its receipt cleanup, without inventing a new receipt.
    aterm_uds::atomic_file::write(&journal, &response, 0o600).unwrap();
    let other = bed.store.claim_staging_dir("unrelated").unwrap();
    let other_journal = other.join("auth.json.refreshed");
    aterm_uds::atomic_file::write(&other_journal, b"unrelated custody", 0o600).unwrap();
    cleanup_codex_enrollments(&bed.store, &bed.path, bed.env.now, &other);
    assert_eq!(std::fs::read(&journal).unwrap(), response);
    assert_eq!(std::fs::read(&other_journal).unwrap(), b"unrelated custody");
    cleanup_codex_enrollments(&bed.store, &bed.path, bed.env.now, &stage);
    assert!(!journal.exists());
    assert_eq!(std::fs::read(&other_journal).unwrap(), b"unrelated custody");

    let kept = bed._root.path().join("kept-applied-stage");
    std::fs::rename(&stage, &kept).unwrap();
    aterm_uds::private_file::create_private_dir(&stage).unwrap();
    aterm_uds::atomic_file::write(&journal, &response, 0o600).unwrap();
    aterm_uds::atomic_file::write(&stage.join("auth.json"), &candidate, 0o600).unwrap();
    cleanup_codex_enrollments(&bed.store, &bed.path, bed.env.now, &stage);
    assert_eq!(std::fs::read(&journal).unwrap(), response);
    assert_eq!(std::fs::read(stage.join("auth.json")).unwrap(), candidate);
    assert_eq!(std::fs::read(&other_journal).unwrap(), b"unrelated custody");
    assert_eq!(bed.store.load_logins(bed.env.now).unwrap().0, committed);
    assert_eq!(host.calls.get(), 1);
    assert!(bed.store.signin_now().unwrap().is_some());
    lifetime
        .pause()
        .unwrap()
        .try_wait()
        .expect("all original and recovery resources retired");
}
