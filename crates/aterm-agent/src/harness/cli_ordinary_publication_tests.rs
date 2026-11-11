// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::{account_work, wake_deadline};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

#[derive(Clone, Copy, Debug)]
enum Phase {
    Writer,
    Identity,
    ReturnedEmail,
    Policy,
    AdapterFailure,
    FalseNoStart,
}

#[derive(Clone, Copy, Debug)]
enum Stop {
    Live,
    Sleep,
    Cancel,
    Credential,
    Directory,
}

struct NativeHost {
    now: i64,
    work: account_work::Scope,
    store: accounts::Store,
    phase: Phase,
    sleeper: wake_deadline::Sleeper,
    ready: mpsc::Sender<(wake_deadline::Sleeper, PathBuf)>,
    release: Mutex<mpsc::Receiver<()>>,
    provider_done: AtomicBool,
    gated: AtomicBool,
    calls: std::cell::Cell<usize>,
    stage: RefCell<Option<PathBuf>>,
    admitted: RefCell<Option<accounts::Logins>>,
}

impl NativeHost {
    fn gate(&self) {
        if !self.gated.swap(true, Ordering::AcqRel) {
            let _ = self
                .ready
                .send((self.sleeper, self.stage.borrow().clone().unwrap()));
            self.release
                .lock()
                .unwrap()
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("controller releases native publication phase");
        }
    }
}

impl AccountsHost for NativeHost {
    fn now(&self) -> i64 {
        if matches!(self.phase, Phase::Policy) && self.provider_done.load(Ordering::Acquire) {
            self.gate();
        }
        self.now
    }
    fn work_scope(&self) -> Option<account_work::Scope> {
        Some(self.work.clone())
    }
    fn work_cancelled(&self) -> bool {
        if matches!(self.phase, Phase::Writer | Phase::ReturnedEmail)
            && self.provider_done.load(Ordering::Acquire)
            && !self.gated.load(Ordering::Acquire)
        {
            match self.store.try_acquire().expect("actual writer probe") {
                Some(writer) => drop(writer),
                None => self.gate(),
            }
        }
        // Deliberately inert: cancellation must travel through the producer's original weak scope.
        false
    }
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("native renewal needs no status process")
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("native renewal needs no login process")
    }
    fn renew_flow(&self, _: &str, _: &str, _: &RenewBrowser) -> Result<i32, String> {
        panic!("native renewal needs no second flow")
    }
    fn codex_account(&self, home: &Path) -> Option<String> {
        let email = crate::harness::codex_app_login::read_account_email(home);
        if matches!(self.phase, Phase::Identity) {
            self.gate();
        }
        email
    }
    fn vendor_sign_in(
        &self,
        request: &crate::harness::login_drive::Request<'_>,
    ) -> crate::harness::login_drive::Drove {
        self.calls.set(self.calls.get() + 1);
        *self.stage.borrow_mut() = Some(request.staging.to_path_buf());
        let email = if matches!(self.phase, Phase::Writer | Phase::Policy) {
            "main@example.test"
        } else {
            "other@example.test"
        };
        let mut drove = if request.enrollment.is_some() {
            crate::harness::codex_app_login::tests::native_enrollment_exchange_fixture(
                request,
                None,
                self.now,
                Some(&self.work),
                email,
            )
        } else {
            crate::harness::codex_app_login::tests::native_ordinary_exchange_fixture(
                request, self.now, &self.work, email,
            )
        };
        // This route reports identity separately, after the genuine native response was saved.
        assert!(matches!(
            drove.outcome,
            crate::harness::login_drive::Outcome::SignedIn { .. }
        ));
        if !matches!(self.phase, Phase::ReturnedEmail) {
            drove.outcome = crate::harness::login_drive::Outcome::SignedIn {
                email: None,
                page: false,
            };
        }
        *self.admitted.borrow_mut() = Some(self.store.load_logins(self.now).unwrap().0);
        self.provider_done.store(true, Ordering::Release);
        if matches!(self.phase, Phase::AdapterFailure | Phase::FalseNoStart) {
            self.gate();
            drove.outcome = if matches!(self.phase, Phase::FalseNoStart) {
                crate::harness::login_drive::Outcome::NotRun("injected adapter no-start".into())
            } else {
                crate::harness::login_drive::Outcome::Unverified("injected adapter failure".into())
            };
        }
        drove
    }
}

#[test]
fn fully_completed_enrollment_keeps_original_work_before_wrong_account_observation() {
    use crate::harness::codex_enrollment as enrollment;
    for stop in [Stop::Live, Stop::Sleep, Stop::Cancel] {
        let bed = Bed::new(Cli::Codex);
        let request = enrollment_request(&bed);
        let old_auth = std::fs::read(bed.dirs[0].join("auth.json")).unwrap();
        let roster = std::fs::read(&bed.path).unwrap();
        let lifetime = account_work::Lifetime::default();
        let scope = lifetime.enter().unwrap();
        let cancellation = scope.cancellation();
        let (work, control) = enrollment::owned_work(request, scope.clone());
        let (ready_send, ready_receive) = mpsc::channel();
        let (release_send, release_receive) = mpsc::channel();
        let env = bed.env.clone();
        let store = bed.store.clone();
        let worker = std::thread::spawn(move || {
            let seam = wake_deadline::SleepSeam::hold();
            seam.sleeper().slept(std::time::Duration::from_secs(37));
            let host = NativeHost {
                now: env.now + 1,
                work: scope,
                store,
                phase: Phase::ReturnedEmail,
                sleeper: seam.sleeper(),
                ready: ready_send,
                release: Mutex::new(release_receive),
                provider_done: AtomicBool::new(false),
                gated: AtomicBool::new(false),
                calls: std::cell::Cell::new(0),
                stage: RefCell::new(None),
                admitted: RefCell::new(None),
            };
            let result = work.run_with_host(&env, &host);
            let admitted = host.admitted.borrow().clone();
            let calls = host.calls.get();
            drop(host);
            (result, admitted, calls)
        });
        let ready = ready_receive.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
        let candidate = ready
            .as_ref()
            .ok()
            .map(|(_, stage)| std::fs::read(stage.join("auth.json")));
        let response = ready
            .as_ref()
            .ok()
            .map(|(_, stage)| std::fs::read(stage.join("auth.json.refreshed")));
        if let Ok((sleeper, _)) = &ready {
            match stop {
                Stop::Live => {}
                Stop::Sleep => {
                    sleeper.slept(aterm_spec::wait::MUST_HAPPEN + std::time::Duration::from_secs(1))
                }
                Stop::Cancel => cancellation.cancel(),
                Stop::Credential | Stop::Directory => unreachable!(),
            }
        }
        let released = release_send.send(());
        let joined = worker.join();
        let (_, stage) =
            ready.expect("fully completed enrollment reached its actual observation writer");
        assert!(released.is_ok());
        let (result, admitted, calls) = joined.unwrap();
        let result = result.unwrap();
        assert_eq!(calls, 1);
        assert_eq!(
            result.phase,
            if matches!(stop, Stop::Live) {
                enrollment::Phase::Refused
            } else {
                enrollment::Phase::Running
            }
        );
        assert_eq!(result.generation, None);
        assert_eq!(
            std::fs::read(stage.join("auth.json")).unwrap(),
            candidate.unwrap().unwrap()
        );
        assert_eq!(
            std::fs::read(stage.join("auth.json.refreshed")).unwrap(),
            response.unwrap().unwrap()
        );
        assert_eq!(
            std::fs::read(bed.dirs[0].join("auth.json")).unwrap(),
            old_auth
        );
        assert_eq!(std::fs::read(&bed.path).unwrap(), roster);
        assert!(!stage.join(accounts::RENEWAL_CHECKPOINT_FILE).exists());
        if !matches!(stop, Stop::Live) {
            assert_eq!(
                bed.store.load_logins(bed.env.now + 1).unwrap().0,
                admitted.unwrap()
            );
        }
        assert!(cancellation.retired());
        control.retired().unwrap().finish(&bed.env).unwrap();
        lifetime
            .pause()
            .unwrap()
            .try_wait()
            .expect("original enrollment resources retired after join");
        assert!(bed.store.signin_now().unwrap().is_some());
    }
}

#[test]
fn ordinary_native_publication_and_wrong_account_evidence_keep_the_original_work_budget() {
    for phase in [
        Phase::Writer,
        Phase::Identity,
        Phase::ReturnedEmail,
        Phase::Policy,
        Phase::AdapterFailure,
        Phase::FalseNoStart,
    ] {
        for stop in [
            Stop::Live,
            Stop::Sleep,
            Stop::Cancel,
            Stop::Credential,
            Stop::Directory,
        ] {
            if matches!(phase, Phase::AdapterFailure | Phase::FalseNoStart)
                && !matches!(stop, Stop::Live)
            {
                continue;
            }
            if !matches!(phase, Phase::Writer) && matches!(stop, Stop::Credential | Stop::Directory)
            {
                continue;
            }
            let bed = Bed::new(Cli::Codex);
            let old_auth = std::fs::read(bed.dirs[0].join("auth.json")).unwrap();
            let roster_bytes = std::fs::read(&bed.path).unwrap();
            let lifetime = account_work::Lifetime::default();
            let scope = lifetime.enter().unwrap();
            let cancellation = scope.cancellation();
            let (ready_send, ready_receive) = mpsc::channel();
            let (release_send, release_receive) = mpsc::channel();
            let env = bed.env.clone();
            let store = bed.store.clone();
            let path = bed.path.clone();
            let worker = std::thread::spawn(move || {
                let seam = wake_deadline::SleepSeam::hold();
                seam.sleeper().slept(std::time::Duration::from_secs(37));
                let host = NativeHost {
                    now: env.now + 1,
                    work: scope,
                    store: store.clone(),
                    phase,
                    sleeper: seam.sleeper(),
                    ready: ready_send,
                    release: Mutex::new(release_receive),
                    provider_done: AtomicBool::new(false),
                    gated: AtomicBool::new(false),
                    calls: std::cell::Cell::new(0),
                    stage: RefCell::new(None),
                    admitted: RefCell::new(None),
                };
                let records = store.read_account_records(Some(&path), env.now).unwrap();
                let attempted = EnrollmentAttempt::default();
                let result = renew_one_for_operation(
                    Cli::Codex,
                    env.now,
                    &host,
                    &store,
                    (records.roster.row("main").unwrap(), &path, &records.logins),
                    (
                        records.raw_roster.row("main").unwrap().dir.as_deref(),
                        crate::harness::sign_in_ladder::asked_fixture(Cli::Codex, "main"),
                    ),
                    None,
                    Some(&attempted),
                    None,
                    &mut Vec::new(),
                    &mut Vec::new(),
                );
                let admitted = host.admitted.borrow().clone();
                let calls = host.calls.get();
                drop(host);
                (result, admitted, calls, attempted.provider_attempted.get())
            });
            let ready = ready_receive.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
            let candidate = ready
                .as_ref()
                .ok()
                .map(|(_, stage)| std::fs::read(stage.join("auth.json")));
            let policy_change = if matches!(phase, Phase::Policy) && ready.is_ok() {
                Some(aterm_uds::atomic_file::write(&bed.path, b"[", 0o600))
            } else {
                None
            };
            let replacement = ready.as_ref().ok().and_then(|(_, stage)| match stop {
                Stop::Credential => Some((|| -> std::io::Result<Option<PathBuf>> {
                    let mut bytes = std::fs::read(stage.join("auth.json"))?;
                    bytes.push(b' ');
                    aterm_uds::atomic_file::write(&stage.join("auth.json"), &bytes, 0o600)?;
                    Ok(None)
                })()),
                Stop::Directory => Some((|| -> std::io::Result<Option<PathBuf>> {
                    let kept = stage.with_extension("original-native");
                    std::fs::rename(stage, &kept)?;
                    aterm_types::fs_restricted::ensure_private_dir(stage)?;
                    let bytes = std::fs::read(kept.join("auth.json"))?;
                    aterm_uds::atomic_file::write(&stage.join("auth.json"), &bytes, 0o600)?;
                    Ok(Some(kept))
                })()),
                _ => None,
            });
            if let Ok((sleeper, _)) = &ready {
                match stop {
                    Stop::Live => {}
                    Stop::Sleep => sleeper
                        .slept(aterm_spec::wait::MUST_HAPPEN + std::time::Duration::from_secs(1)),
                    Stop::Cancel => cancellation.cancel(),
                    Stop::Credential | Stop::Directory => {}
                }
            }
            let released = release_send.send(());
            let joined = worker.join();
            // Always release the actual worker before checking phase or state assertions.
            let (_, stage) = ready.expect("native response reached the actual publication phase");
            assert!(released.is_ok());
            let (result, admitted, calls, attempted) = joined.unwrap();
            assert!(
                attempted,
                "a genuine completed response cannot become a no-start"
            );
            if let Some(changed) = policy_change {
                changed.unwrap();
            }
            let kept = replacement.and_then(|changed| changed.unwrap());
            assert_eq!(calls, 1);
            let candidate = candidate.unwrap().unwrap();
            let mut expected = candidate.clone();
            if matches!(stop, Stop::Credential) {
                expected.push(b' ');
            }
            assert_eq!(std::fs::read(stage.join("auth.json")).unwrap(), expected);
            if let Some(kept) = kept {
                assert_eq!(std::fs::read(kept.join("auth.json")).unwrap(), candidate);
            }
            assert_eq!(
                std::fs::read(bed.dirs[0].join("auth.json")).unwrap(),
                old_auth
            );
            assert_eq!(
                std::fs::read(&bed.path).unwrap(),
                if matches!(phase, Phase::Policy) {
                    b"[".to_vec()
                } else {
                    roster_bytes
                }
            );
            if matches!(stop, Stop::Live)
                && !matches!(
                    phase,
                    Phase::Policy | Phase::AdapterFailure | Phase::FalseNoStart
                )
            {
                assert_eq!(
                    result,
                    if matches!(phase, Phase::Writer) {
                        Renewal::Renewed
                    } else {
                        Renewal::WrongAccount
                    }
                );
                assert_eq!(
                    stage.join(accounts::RENEWAL_CHECKPOINT_FILE).exists(),
                    matches!(phase, Phase::Writer)
                );
            } else {
                assert_eq!(
                    result,
                    if matches!(stop, Stop::Live) && matches!(phase, Phase::Policy) {
                        Renewal::Failed
                    } else {
                        Renewal::Deferred
                    },
                    "{phase:?} {stop:?}"
                );
                assert_eq!(
                    bed.store.load_logins(bed.env.now + 1).unwrap().0,
                    admitted.unwrap()
                );
                assert!(!stage.join(accounts::RENEWAL_CHECKPOINT_FILE).exists());
            }
            assert!(cancellation.retired());
            lifetime
                .pause()
                .unwrap()
                .try_wait()
                .expect("all original operation scopes retired");
            assert!(bed.store.signin_now().unwrap().is_some());
        }
    }
}
