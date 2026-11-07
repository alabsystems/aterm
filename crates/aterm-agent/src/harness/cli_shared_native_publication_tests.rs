// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::{account_work, wake_deadline};
use std::sync::{Mutex, mpsc};

#[derive(Clone, Copy, Debug)]
enum Phase {
    Returned,
    Proof,
    Authority,
    WriterRefusal,
    FalseNoStart,
}

#[derive(Clone, Copy, Debug)]
enum Stop {
    Live,
    Sleep,
    Cancel,
}

struct Gate {
    phase: Phase,
    store: accounts::Store,
    sleeper: wake_deadline::Sleeper,
    ready: mpsc::Sender<(wake_deadline::Sleeper, PathBuf)>,
    release: Mutex<mpsc::Receiver<()>>,
    fired: AtomicBool,
    completed: AtomicBool,
    identity_observed: AtomicBool,
    stage: Mutex<Option<PathBuf>>,
}

impl Gate {
    fn wait(&self, staging: &Path) {
        if !self.fired.swap(true, Ordering::AcqRel) {
            let _ = self.ready.send((self.sleeper, staging.to_owned()));
            self.release
                .lock()
                .unwrap()
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("controller releases genuine shared native completion");
        }
    }
}

struct NativeProvider {
    gate: Arc<Gate>,
    calls: AtomicUsize,
    observations: AtomicUsize,
    admitted: Mutex<Option<accounts::Logins>>,
}

impl shared_provider::Provider for NativeProvider {
    fn execute(
        &self,
        context: &shared_provider::Context<'_>,
        row: &accounts::Row,
        staging: &Path,
        _: &RenewBrowser,
    ) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let work = context
            .host
            .work_scope()
            .expect("original counted native operation");
        let route = Cli::Codex
            .accounts()
            .signin
            .as_ref()
            .unwrap()
            .route(row.flow(Cli::Codex))
            .unwrap();
        let request = crate::harness::login_drive::Request {
            publication: context.publication,
            enrollment: None,
            recovery: None,
            admission: None,
            cli: Cli::Codex,
            route,
            row,
            staging,
            store: context.store,
            hosts: route.hosts.of(row.vendor.region).unwrap(),
        };
        let drove = crate::harness::codex_app_login::tests::native_ordinary_exchange_fixture(
            &request,
            context.env.now + 1,
            &work,
            EMAIL,
        );
        assert!(matches!(
            drove.outcome,
            crate::harness::login_drive::Outcome::SignedIn { .. }
        ));
        *self.gate.stage.lock().unwrap() = Some(staging.to_owned());
        self.gate.completed.store(true, Ordering::Release);
        *self.admitted.lock().unwrap() =
            Some(context.store.load_logins(context.env.now + 1).unwrap().0);
        if matches!(self.gate.phase, Phase::Returned | Phase::FalseNoStart) {
            self.gate.wait(staging);
        }
        Ok(())
    }

    fn execute_attempt(
        &self,
        context: &shared_provider::Context<'_>,
        row: &accounts::Row,
        staging: &Path,
        browser: &RenewBrowser,
    ) -> shared_provider::Execution {
        match self.execute(context, row, staging, browser) {
            Ok(()) if matches!(self.gate.phase, Phase::FalseNoStart) => {
                shared_provider::Execution::NotStarted("injected adapter no-start".into())
            }
            Ok(()) => shared_provider::Execution::Completed,
            Err(why) => shared_provider::Execution::Unknown(why),
        }
    }

    fn observe(
        &self,
        _: &shared_provider::Context<'_>,
        _: &accounts::Row,
        staging: &Path,
    ) -> Result<Option<Box<dyn shared_provider::LoginProof>>, shared_provider::ObservationError>
    {
        self.observations.fetch_add(1, Ordering::SeqCst);
        Ok(Some(Box::new(NativeProof {
            staging: staging.to_owned(),
            gate: self.gate.clone(),
        })))
    }
}

struct NativeProof {
    staging: PathBuf,
    gate: Arc<Gate>,
}

struct NativeAuthority {
    inner: Arc<Approval>,
    gate: Arc<Gate>,
}

impl Authority for NativeAuthority {
    fn prepare(&self, row: &accounts::Row) -> Result<Grant, String> {
        self.inner.prepare(row)
    }
    fn current(&self, grant: &Grant, row: &accounts::Row) -> Result<(), String> {
        self.inner.current(grant, row)?;
        if matches!(self.gate.phase, Phase::Authority)
            && self.gate.completed.load(Ordering::Acquire)
        {
            let stage = self.gate.stage.lock().unwrap().clone().unwrap();
            self.gate.wait(&stage);
            self.inner.current(grant, row)?;
        }
        Ok(())
    }
    fn request_recovery(
        &self,
        grant: &Grant,
        row: &accounts::Row,
        intent: &accounts::shared::Intent,
    ) -> Result<(), String> {
        self.inner.request_recovery(grant, row, intent)
    }
}

impl shared_provider::LoginProof for NativeProof {
    fn identity(&self) -> &str {
        self.gate.identity_observed.store(true, Ordering::Release);
        EMAIL
    }
    fn principal(&self) -> [u8; 32] {
        profile::principal_id("provider-uuid").unwrap()
    }
    fn subscription(&self) -> Option<&str> {
        None
    }
    fn check_current(&self) -> Result<(), String> {
        if crate::harness::codex_app_login::read_account_email(&self.staging).as_deref()
            != Some(EMAIL)
        {
            return Err("actual saved native identity changed".into());
        }
        if matches!(self.gate.phase, Phase::Proof) {
            match self
                .gate
                .store
                .try_acquire()
                .map_err(|why| why.to_string())?
            {
                Some(writer) => drop(writer),
                None => self.gate.wait(&self.staging),
            }
        }
        Ok(())
    }
}

struct NativeHost(account_work::Scope, Arc<Gate>);

impl AccountsHost for NativeHost {
    fn work_scope(&self) -> Option<account_work::Scope> {
        Some(self.0.clone())
    }
    // The producer receives the real scope; a later inert host cannot hide its cancellation.
    fn work_cancelled(&self) -> bool {
        false
    }
    fn now(&self) -> i64 {
        if matches!(self.1.phase, Phase::WriterRefusal)
            && self.1.identity_observed.load(Ordering::Acquire)
        {
            let stage = self.1.stage.lock().unwrap().clone().unwrap();
            self.1.wait(&stage);
        }
        NOW + 1
    }
    fn current_exe(&self) -> Option<PathBuf> {
        Some(std::env::current_exe().unwrap())
    }
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("local native proof uses no status process")
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("one native exchange only")
    }
    fn renew_flow(&self, _: &str, _: &str, _: &RenewBrowser) -> Result<i32, String> {
        panic!("one native exchange only")
    }
}

#[test]
fn shared_native_completion_keeps_original_work_before_observation_and_after_final_proof_reads() {
    for phase in [
        Phase::Returned,
        Phase::Proof,
        Phase::Authority,
        Phase::WriterRefusal,
        Phase::FalseNoStart,
    ] {
        for stop in [Stop::Live, Stop::Sleep, Stop::Cancel] {
            if matches!(phase, Phase::FalseNoStart) && !matches!(stop, Stop::Live) {
                continue;
            }
            let (_tmp, env, approval, _) = fixture(0);
            let store = env.accounts_store_for(Cli::Codex).unwrap();
            let roster = env.accounts_roster_for(Cli::Codex).unwrap();
            let roster_before = fs::read(&roster).ok();
            let lifetime = account_work::Lifetime::default();
            let scope = lifetime.enter().unwrap();
            let cancellation = scope.cancellation();
            let (ready_send, ready_receive) = mpsc::channel();
            let (release_send, release_receive) = mpsc::channel();
            let (returned_send, returned_receive) = mpsc::channel();
            let worker_store = store.clone();
            let worker_roster = roster.clone();
            let worker_approval = approval.clone();
            let worker_env = env.clone();
            let worker = std::thread::spawn(move || {
                let seam = wake_deadline::SleepSeam::hold();
                seam.sleeper().slept(std::time::Duration::from_secs(37));
                let provider = Arc::new(NativeProvider {
                    gate: Arc::new(Gate {
                        phase,
                        store: worker_store.clone(),
                        sleeper: seam.sleeper(),
                        ready: ready_send,
                        release: Mutex::new(release_receive),
                        fired: AtomicBool::new(false),
                        completed: AtomicBool::new(false),
                        identity_observed: AtomicBool::new(false),
                        stage: Mutex::new(None),
                    }),
                    calls: AtomicUsize::new(0),
                    observations: AtomicUsize::new(0),
                    admitted: Mutex::new(None),
                });
                let mut candidate = candidate(&worker_approval);
                candidate.authority = Arc::new(NativeAuthority {
                    inner: worker_approval,
                    gate: provider.gate.clone(),
                });
                candidate.cli = Cli::Codex;
                candidate.provider = provider.clone();
                let signer = adoption_sign_in(&worker_store, &worker_roster, &candidate);
                let host = NativeHost(scope, provider.gate.clone());
                let result = super::super::super::shared_adoption::adopt(
                    &worker_env,
                    &host,
                    &worker_store,
                    &signer,
                    &worker_roster,
                    candidate,
                );
                let admitted = provider.admitted.lock().unwrap().clone();
                let _ = returned_send.send(());
                (
                    result,
                    provider.calls.load(Ordering::SeqCst),
                    provider.observations.load(Ordering::SeqCst),
                    admitted,
                )
            });
            let ready = ready_receive.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
            let bytes = ready
                .as_ref()
                .ok()
                .map(|(_, stage)| fs::read(stage.join("auth.json")));
            let held = if ready.is_ok() && matches!(phase, Phase::WriterRefusal) {
                store.try_acquire().unwrap()
            } else {
                None
            };
            if let Ok((sleeper, _)) = &ready {
                if matches!(phase, Phase::Authority) {
                    approval.live.store(false, Ordering::SeqCst);
                }
                match stop {
                    Stop::Live => {}
                    Stop::Sleep => sleeper
                        .slept(aterm_spec::wait::MUST_HAPPEN + std::time::Duration::from_secs(1)),
                    Stop::Cancel => cancellation.cancel(),
                }
            }
            let released = release_send.send(());
            let returned = if matches!(phase, Phase::WriterRefusal) {
                Some(returned_receive.recv_timeout(aterm_spec::wait::MUST_HAPPEN))
            } else {
                None
            };
            let writer_held = held.is_some();
            drop(held);
            let joined = worker.join();
            let (_, stage) = ready.expect("actual shared native publication phase");
            assert!(released.is_ok());
            if let Some(returned) = returned {
                assert!(writer_held);
                returned.expect("shared adoption returns while the real writer is held");
            }
            let (result, calls, observations, admitted) = joined.unwrap();
            assert_eq!(calls, 1);
            assert_eq!(
                fs::read(stage.join("auth.json")).unwrap(),
                bytes.unwrap().unwrap()
            );
            if matches!(stop, Stop::Live)
                && !matches!(
                    phase,
                    Phase::Authority | Phase::WriterRefusal | Phase::FalseNoStart
                )
            {
                assert!(result.unwrap());
                assert_eq!(observations, 1);
            } else {
                assert_eq!(
                    result.unwrap_err().is_deferred(),
                    !matches!(stop, Stop::Live) || matches!(phase, Phase::FalseNoStart),
                    "{phase:?} {stop:?}"
                );
                assert_eq!(
                    observations,
                    usize::from(matches!(phase, Phase::Proof | Phase::WriterRefusal))
                );
                assert_eq!(store.load_logins(NOW + 1).unwrap().0, admitted.unwrap());
                assert_eq!(fs::read(&roster).ok(), roster_before);
                assert_eq!(approval.recoveries.load(Ordering::SeqCst), 0);
            }
            assert!(cancellation.retired());
            lifetime
                .pause()
                .unwrap()
                .try_wait()
                .expect("shared native work positively retired");
            assert!(store.signin_now().unwrap().is_some());
        }
    }
}
