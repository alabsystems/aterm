// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::App;
use aterm_agent::harness::cli::{AccountsHost, RenewBrowser};
use aterm_agent::harness::{align::Captured, login_drive};
use std::cell::Cell;

pub(super) fn serial() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: OnceLock<Mutex<()>> = OnceLock::new();
    // A test that failed holding it says so itself; its siblings still run.
    SERIAL
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(super) fn fixture() -> (
    aterm_tempfile::TempDir,
    Env,
    crate::self_claim::Proven,
    String,
) {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut env = Env::from_process().unwrap();
    env.home = Some(root.path().join("home"));
    env.state = root.path().join("state");
    env.config = Some(root.path().join("config/aterm.toml"));
    env.now = 1_000_000;
    std::fs::create_dir_all(root.path().join("config")).unwrap();
    let row = accounts::Row {
        label: "main".into(),
        email: Some("main@example.test".into()),
        dir: Some(root.path().join("configured").to_str().unwrap().into()),
        ..Default::default()
    };
    std::fs::write(
        root.path().join("config/accounts-codex.toml"),
        accounts::append_rows_for(accounts::Cli::Codex, "", &[row]).unwrap(),
    )
    .unwrap();
    let proof = crate::self_claim::Proven {
        pid: 42,
        start: "fixture-birth".into(),
    };
    let sid = root
        .path()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    (root, env, proof, sid)
}

#[derive(Clone, Copy)]
enum ProviderEnd {
    Failed,
    Held,
    Panic,
    Preparation(login_drive::NativePreparationStage),
}

#[test]
fn actual_pending_response_settles_before_drain_without_a_source_registry() {
    let _callbacks = enrollment::PendingCustodyFixture::serial_callbacks().unwrap();
    for late in [false, true] {
        let _serial = serial();
        let lifetime = account_work::Lifetime::default();
        let original = lifetime.enter().unwrap();
        let cancellation = original.cancellation();
        let (control, env, mut restore) = enrollment::PendingCustodyFixture::new(&original)
            .unwrap()
            .into_parts();
        drop(original);
        let owned = Arc::new(OwnedControl { control, env });
        let key = ("pending-drain-test".to_owned(), "owned".to_owned());

        let foreign_lifetime = account_work::Lifetime::default();
        let foreign_scope = foreign_lifetime.enter().unwrap();
        let foreign_cancel = foreign_scope.cancellation();
        let (control, env, mut foreign_restore) =
            enrollment::PendingCustodyFixture::new(&foreign_scope)
                .unwrap()
                .into_parts();
        drop(foreign_scope);
        // A wrongly selected foreign retry would now succeed and release its count.
        foreign_restore.restore().unwrap();
        let foreign = Arc::new(OwnedControl { control, env });
        let foreign_key = ("pending-drain-test".to_owned(), "foreign".to_owned());
        {
            let mut table = controls().lock().unwrap();
            table.insert(key.clone(), owned.clone());
            table.insert(foreign_key.clone(), foreign.clone());
        }
        let pause = lifetime.pause().unwrap();
        let persistent = App::settle_enrollment_custody(&lifetime);
        let still_counted = pause.try_wait();
        let refused = still_counted.is_err();
        let held_resources = restore.resources_available();
        if late {
            remove_control(&key, &owned);
        }
        let (swept_send, swept_receive) = std::sync::mpsc::channel();
        let (resume_send, resume_receive) = std::sync::mpsc::channel();
        let (done_send, done_receive) = std::sync::mpsc::channel();
        let worker_lifetime = lifetime.clone();
        let worker = std::thread::spawn(move || {
            let drained = match still_counted {
                Err(pause) => crate::app_account_work::drain_custody_for_test(
                    pause,
                    &worker_lifetime,
                    Some(crate::app_account_work::CustodySweepCheckpoint {
                        swept: swept_send,
                        resume: resume_receive,
                    }),
                ),
                Ok(drained) => drained,
            };
            let _ = done_send.send(drained);
        });
        let swept = swept_receive.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
        let pending_before_restore = owned.has_pending_custody();
        let retired_before_restore = owned.work_retired();
        let foreign_before = (
            foreign.has_pending_custody(),
            foreign.work_retired(),
            foreign_cancel.cancelled(),
        );
        if late {
            controls()
                .lock()
                .unwrap()
                .insert(key.clone(), owned.clone());
        }
        let restored = restore.restore();
        let resumed = resume_send.send(());
        let completed = done_receive.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
        let automatic = completed.is_ok();
        let automatic_resources = restore.resources_released_within(aterm_spec::wait::MUST_HAPPEN);
        let foreign_resources = foreign_restore.resources_available();
        // Rescue real custody before joining even if drain orchestration regresses.
        let settled = owned.settle_custody(&owned.env);
        let pending_after = owned.has_pending_custody();
        let retired_after = owned.work_retired() && cancellation.retired();
        let sealed = automatic && App::seal_enrollment_retirement(&lifetime);
        remove_control(&key, &owned);
        // Teardown of this synthetic private world cannot qualify as settlement:
        // `automatic` was captured before the fallback and must still be true.
        drop(owned);
        let completed = match completed {
            Ok(drained) => Ok(drained),
            Err(_) => done_receive.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        };
        let joined = worker.join();
        let foreign_settled = foreign.settle_custody(&foreign.env);
        let foreign_released_resources =
            foreign_restore.resources_released_within(aterm_spec::wait::MUST_HAPPEN);
        let foreign_pause = foreign_lifetime.pause().unwrap();
        let foreign_retired = foreign_pause.try_wait();
        let foreign_automatic = foreign_retired.is_ok();
        let foreign_sealed = App::seal_enrollment_retirement(&foreign_lifetime);
        remove_control(&foreign_key, &foreign);
        drop(foreign);
        let foreign_drained = match foreign_retired {
            Ok(drained) => Ok(drained),
            Err(pause) => pause.try_wait(),
        };

        joined.unwrap();
        restored.unwrap();
        settled.unwrap();
        foreign_settled.unwrap();
        assert!(
            persistent && refused,
            "actual failed storage retains its original count"
        );
        swept.expect("actual first custody sweep completed before late insertion");
        assert!(resumed.is_ok());
        assert!(pending_before_restore && !retired_before_restore);
        assert_eq!(held_resources.unwrap(), [false; 4]);
        assert_eq!(foreign_before, (true, false, false));
        assert!(
            automatic,
            "the drain itself must retry owned custody before count-zero"
        );
        assert!(!pending_after && retired_after);
        assert_eq!(automatic_resources.unwrap(), [true; 4]);
        assert_eq!(foreign_resources.unwrap(), [false; 4]);
        assert_eq!(foreign_released_resources.unwrap(), [true; 4]);
        assert!(sealed && foreign_sealed && foreign_automatic);
        completed.unwrap().resume().unwrap();
        foreign_drained.unwrap().resume().unwrap();
        assert!(lifetime.enter().is_some());
        assert!(foreign_lifetime.enter().is_some());
    }
}

struct FailedProvider(
    Cell<usize>,
    ProviderEnd,
    std::cell::RefCell<Option<std::path::PathBuf>>,
);
impl AccountsHost for FailedProvider {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("no default auth")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("no default login")
    }
    fn now(&self) -> i64 {
        1_000_000
    }
    fn renew_flow(&self, _: &str, _: &str, _: &RenewBrowser) -> Result<i32, String> {
        panic!("no browser")
    }
    fn vendor_sign_in(&self, request: &login_drive::Request<'_>) -> login_drive::Drove {
        self.0.set(self.0.get() + 1);
        *self.2.borrow_mut() = Some(request.staging.to_owned());
        let outcome = match self.1 {
            ProviderEnd::Failed => login_drive::Outcome::Failed,
            ProviderEnd::Held => {
                aterm_uds::atomic_file::write(&request.staging.join("auth.json"), b"{}", 0o600)
                    .unwrap();
                login_drive::Outcome::CompletedHeld
            }
            ProviderEnd::Panic => panic!("controlled provider unwind after durable admission"),
            ProviderEnd::Preparation(stage) => login_drive::Outcome::PreparationFailed(stage),
        };
        login_drive::Drove {
            outcome,
            host: None,
        }
    }
}

#[test]
fn exact_preparation_diagnostics_are_read_only_and_keep_the_original_operation() {
    let _serial = serial();
    let (_root, env, proof, sid) = fixture();
    let lifetime = account_work::Lifetime::default();
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    let mut expected = Vec::new();
    for (index, stage) in [
        login_drive::NativePreparationStage::RowPolicy,
        login_drive::NativePreparationStage::GrantCustody,
        login_drive::NativePreparationStage::NativeScope,
        login_drive::NativePreparationStage::RowAdmission,
    ]
    .into_iter()
    .enumerate()
    {
        let view = discovery(&env, &sid, &proof).unwrap().unwrap();
        let operation = format!("{index:032x}");
        let command = Command::Enroll {
            operation: operation.clone(),
            revision: view["revision"].as_str().unwrap().into(),
            account: accounts_control::account_id("main"),
        };
        let provider = FailedProvider(
            Cell::new(0),
            ProviderEnd::Preparation(stage),
            Default::default(),
        );
        let refused = run_in(&command, &sid, &proof, &lifetime, &env, |work, env| {
            work.run_with_host(env, &provider)
        })
        .unwrap()
        .unwrap();
        assert_eq!(refused["phase"].as_str(), Some("refused"));
        assert!(refused.get("preparation_stage").is_none());
        assert_eq!(provider.0.get(), 1);
        expected.push((operation, stage));
    }
    let original = std::fs::read(store.logins_file()).unwrap();
    let ledger = std::fs::read(store.ledger_path()).unwrap();
    let paused = lifetime.pause().unwrap();
    for (operation, stage) in expected {
        let command = Command::Diagnostics {
            request: "f".repeat(32),
            operation: operation.clone(),
        };
        assert!(!command.may_publish());
        let diagnosed = run_in(&command, &sid, &proof, &lifetime, &env, |_, _| {
            panic!("diagnostic read cannot drive or recover an enrollment")
        })
        .unwrap()
        .unwrap();
        assert_eq!(
            diagnosed,
            json!({
                "operation": operation,
                "account_id": accounts_control::account_id("main"),
                "phase": "refused", "generation": null,
                "preparation_stage": stage.as_str(),
            })
        );
        assert!(diagnosed.as_object().unwrap().values().all(|value| {
            value.as_str().is_none_or(|text| {
                !text.contains('/') && !text.contains('@') && !text.contains("token")
            })
        }));
        let foreign = crate::self_claim::Proven {
            start: "another-process-birth".into(),
            ..proof.clone()
        };
        assert!(diagnostics(&env, &sid, &foreign, &operation).is_err());
        assert!(diagnostics(&env, "other-session", &proof, &operation).is_err());
    }
    assert!(diagnostics(&env, &sid, &proof, &"e".repeat(32)).is_err());
    assert_eq!(std::fs::read(store.logins_file()).unwrap(), original);
    assert_eq!(std::fs::read(store.ledger_path()).unwrap(), ledger);
    paused.try_wait().unwrap().resume().unwrap();
}

#[test]
fn uncertain_provider_durably_retires_its_resources_before_later_cancel() {
    let _serial = serial();
    let (_root, env, proof, sid) = fixture();
    let view = discovery(&env, &sid, &proof).unwrap().unwrap();
    let command = Command::Enroll {
        operation: "a".repeat(32),
        revision: view["revision"].as_str().unwrap().into(),
        account: accounts_control::account_id("main"),
    };
    let lifetime = account_work::Lifetime::default();
    let provider = FailedProvider(Cell::new(0), ProviderEnd::Failed, Default::default());
    let running = run_in(&command, &sid, &proof, &lifetime, &env, |work, env| {
        work.run_with_host(env, &provider)
    })
    .unwrap()
    .unwrap();
    assert_eq!(running["phase"].as_str(), Some("running"));
    assert_eq!(provider.0.get(), 1);
    let key = (sid.clone(), "a".repeat(32));
    assert!(
        !controls().lock().unwrap().contains_key(&key),
        "the persisted retirement fact replaces the empty runtime handle"
    );
    let wrong = crate::self_claim::Proven {
        start: "replacement-birth".into(),
        ..proof.clone()
    };
    assert!(
        run_in(
            &Command::Poll("a".repeat(32)),
            &sid,
            &wrong,
            &lifetime,
            &env,
            |_, _| panic!("foreign caller cannot dispatch")
        )
        .is_err()
    );
    assert!(
        run_in(
            &Command::Cancel("a".repeat(32)),
            &sid,
            &wrong,
            &lifetime,
            &env,
            |_, _| panic!("foreign caller cannot cancel")
        )
        .is_err()
    );
    let cancelled = run_in(
        &Command::Cancel("a".repeat(32)),
        &sid,
        &proof,
        &lifetime,
        &env,
        |_, _| panic!("cancel cannot dispatch provider"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(cancelled["phase"].as_str(), Some("cancelled"));
    assert_eq!(cancelled["generation"], Value::Null);
    let retained = run_in(&command, &sid, &proof, &lifetime, &env, |_, _| {
        panic!("retained operation cannot redispatch provider")
    })
    .unwrap()
    .unwrap();
    assert_eq!(retained, cancelled);
    assert_eq!(provider.0.get(), 1);
    assert!(
        !controls().lock().unwrap().contains_key(&key),
        "committed cancellation releases the runtime slot"
    );
}

#[test]
fn a_retired_native_request_dispatches_fresh_local_recovery_without_replaying_provider_work() {
    let _serial = serial();
    let (_root, env, proof, sid) = fixture();
    let view = discovery(&env, &sid, &proof).unwrap().unwrap();
    let command = Command::Enroll {
        operation: "c".repeat(32),
        revision: view["revision"].as_str().unwrap().into(),
        account: accounts_control::account_id("main"),
    };
    let lifetime = account_work::Lifetime::default();
    let provider = FailedProvider(Cell::new(0), ProviderEnd::Held, Default::default());
    let first = run_in(&command, &sid, &proof, &lifetime, &env, |work, env| {
        work.run_with_host(env, &provider)
    })
    .unwrap()
    .unwrap();
    let staging = provider.2.borrow().clone().unwrap();
    let bytes = std::fs::read(staging.join("auth.json")).unwrap();
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    let original = store.load_logins(env.now).unwrap().0;
    let request = original
        .enrollment
        .admitted(&"c".repeat(32), &caller(&sid, &proof))
        .unwrap();
    assert!(original.enrollment.recoverable(&request).unwrap());
    assert!(
        run_in(
            &Command::Discover("f".repeat(32)),
            &sid,
            &proof,
            &lifetime,
            &env,
            |_, _| panic!("discovery cannot dispatch recovery")
        )
        .unwrap()
        .is_some()
    );
    assert_eq!(store.load_logins(env.now).unwrap().0, original);
    let dispatched = Cell::new(0);
    let poll = Command::Poll("c".repeat(32));
    for resume in [&command, &poll] {
        let recovered = run_in(resume, &sid, &proof, &lifetime, &env, |work, env| {
            dispatched.set(dispatched.get() + 1);
            work.run_with_host(env, &provider)
        })
        .unwrap()
        .unwrap();
        assert_eq!(recovered, first);
        assert_eq!(recovered["phase"].as_str(), Some("running"));
        assert_eq!(recovered["generation"], Value::Null);
    }
    assert_eq!(dispatched.get(), 2);
    assert_eq!(provider.0.get(), 1);
    assert_eq!(std::fs::read(staging.join("auth.json")).unwrap(), bytes);
    assert_eq!(store.load_logins(env.now).unwrap().0, original);
    // An auth file alone is not a completed-response receipt. The admitted
    // recovery ran locally, retained uncertainty, and never called the provider.
    assert!(!App::enrollment_retirement_pending(&lifetime));
    assert!(lifetime.pause().unwrap().try_wait().is_ok());
}

#[test]
fn a_counted_mutating_poll_repairs_exact_pending_records_without_a_runtime_control() {
    let _serial = serial();
    let (_root, env, proof, sid) = fixture();
    let view = discovery(&env, &sid, &proof).unwrap().unwrap();
    let command = Command::Enroll {
        operation: "f".repeat(32),
        revision: view["revision"].as_str().unwrap().into(),
        account: accounts_control::account_id("main"),
    };
    let lifetime = account_work::Lifetime::default();
    let provider = FailedProvider(Cell::new(0), ProviderEnd::Held, Default::default());
    let running = run_in(&command, &sid, &proof, &lifetime, &env, |work, env| {
        work.run_with_host(env, &provider)
    })
    .unwrap()
    .unwrap();
    assert!(
        !controls()
            .lock()
            .unwrap()
            .contains_key(&(sid.clone(), "f".repeat(32)))
    );
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    let roster = env
        .config
        .as_deref()
        .unwrap()
        .parent()
        .unwrap()
        .join(accounts::Cli::Codex.accounts().roster_file);
    let roster_before = std::fs::read(&roster).unwrap();
    let wake = store.wake_path();
    match std::fs::remove_file(&wake) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("{error}"),
    }
    std::fs::create_dir(&wake).unwrap();
    let prepared =
        store.observe_admitted_workflow_for_test(&roster, "main", env.now, |_, state, _| {
            state.previous = Some("main".into());
            vec![accounts::Event::new(env.now, accounts::Kind::Use, "main")]
        });
    assert!(prepared.is_err());
    assert!(store.load_logins(env.now).is_err());
    let receipt = store.dir.join("sync-commit.json");
    let pending = std::fs::read(&receipt).unwrap();
    let ledger = std::fs::read(store.ledger_path()).unwrap();
    assert!(discovery(&env, &sid, &proof).is_err());
    assert!(diagnostics(&env, &sid, &proof, &"f".repeat(32)).is_err());
    assert_eq!(std::fs::read(&receipt).unwrap(), pending);
    assert_eq!(std::fs::read(store.ledger_path()).unwrap(), ledger);
    std::fs::remove_dir(&wake).unwrap();
    let paused = lifetime.pause().unwrap();
    assert!(
        run_in(
            &Command::Poll("f".repeat(32)),
            &sid,
            &proof,
            &lifetime,
            &env,
            |_, _| panic!("closed work cannot repair or dispatch")
        )
        .is_err()
    );
    assert_eq!(std::fs::read(&receipt).unwrap(), pending);
    paused.try_wait().unwrap().resume().unwrap();
    let progress = run_in(
        &Command::Poll("f".repeat(32)),
        &sid,
        &proof,
        &lifetime,
        &env,
        |work, env| work.run_with_host(env, &provider),
    )
    .unwrap()
    .unwrap();
    assert_eq!(progress, running);
    assert_eq!(provider.0.get(), 1);
    assert!(!store.recover_records(&roster).unwrap());
    assert_eq!(std::fs::read(&roster).unwrap(), roster_before);
    assert_eq!(std::fs::read(store.ledger_path()).unwrap(), ledger);
    assert_eq!(
        store.load(env.now).unwrap().0.previous.as_deref(),
        Some("main")
    );
    assert!(lifetime.pause().unwrap().try_wait().is_ok());
}

#[test]
fn a_drain_seals_original_panicked_work_only_in_its_lifetime_and_preserves_failed_seals() {
    let _serial = serial();
    for ready in [false, true] {
        for refused in [false, true] {
            let (_root, env, proof, sid) = fixture();
            let view = discovery(&env, &sid, &proof).unwrap().unwrap();
            let command = Command::Enroll {
                operation: "d".repeat(32),
                revision: view["revision"].as_str().unwrap().into(),
                account: accounts_control::account_id("main"),
            };
            let mut app = crate::App::headless_for_test();
            let lifetime = app.account_work.clone();
            let provider = FailedProvider(Cell::new(0), ProviderEnd::Panic, Default::default());
            let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = run_in(&command, &sid, &proof, &lifetime, &env, |work, env| {
                    work.run_with_host(env, &provider)
                });
            }));
            assert!(panic.is_err());
            let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
            let request = store
                .load_logins(env.now)
                .unwrap()
                .0
                .enrollment
                .admitted(&"d".repeat(32), &caller(&sid, &proof))
                .unwrap();
            assert!(
                !store
                    .load_logins(env.now)
                    .unwrap()
                    .0
                    .enrollment
                    .recoverable(&request)
                    .unwrap()
            );
            if ready && refused {
                assert!(app.initial_surface_exit_ready());
                assert!(
                    !store
                        .load_logins(env.now)
                        .unwrap()
                        .0
                        .enrollment
                        .recoverable(&request)
                        .unwrap()
                );
                app.resume_drained_accounts();
            }
            let foreign_lifetime = account_work::Lifetime::default();
            let mut foreign_request = request.clone();
            foreign_request.id = "e".repeat(32);
            let (foreign_work, foreign_control) =
                enrollment::owned_work(foreign_request, foreign_lifetime.enter().unwrap());
            let foreign = Arc::new(OwnedControl {
                control: foreign_control,
                env: env.clone(),
            });
            let foreign_key = (sid.clone(), "e".repeat(32));
            controls()
                .lock()
                .unwrap()
                .insert(foreign_key.clone(), Arc::clone(&foreign));
            let held_writer = refused.then(|| store.acquire_within(&[]).unwrap().unwrap());
            let blocker = (!ready).then(|| lifetime.enter().unwrap());
            let pause = lifetime.pause().unwrap();
            let input = if ready {
                crate::app_account_work::DrainInput::Ready(pause.try_wait().unwrap())
            } else {
                crate::app_account_work::DrainInput::Wait(pause)
            };
            let worker_lifetime = lifetime.clone();
            let (sent, received) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                let settlement = crate::app_account_work::settle_accounts(input, &worker_lifetime);
                let _ = sent.send(settlement);
            });
            drop(blocker);
            let settled = received.recv_timeout(Duration::from_secs(60));
            drop(held_writer);
            worker.join().unwrap();
            let settled = settled.unwrap();
            assert_eq!(settled.durably_sealed(), !refused);
            assert!(lifetime.enter().is_none());
            assert!(
                controls()
                    .lock()
                    .unwrap()
                    .get(&foreign_key)
                    .is_some_and(|current| Arc::ptr_eq(current, &foreign))
            );
            assert!(!foreign.work_retired());
            assert_eq!(
                store
                    .load_logins(env.now)
                    .unwrap()
                    .0
                    .enrollment
                    .recoverable(&request)
                    .unwrap(),
                !refused
            );
            if refused {
                let retry_lifetime = lifetime.clone();
                assert!(
                    std::thread::spawn(move || seal_controls(&retry_lifetime))
                        .join()
                        .unwrap()
                );
                assert!(
                    store
                        .load_logins(env.now)
                        .unwrap()
                        .0
                        .enrollment
                        .recoverable(&request)
                        .unwrap()
                );
            }
            drop(settled);
            drop(foreign_work);
            remove_control(&foreign_key, &foreign);
            assert!(!App::enrollment_retirement_pending(&lifetime));
            assert!(foreign_lifetime.pause().unwrap().try_wait().is_ok());
        }
    }
}

#[test]
fn published_job_can_complete_and_retire_before_spawn_returns() {
    let _serial = serial();
    let lifetime = account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let custody = scope.cancellation();
    let job = Arc::new(Job {
        cleanup_owner: None,
        command: Command::Enroll {
            operation: "7".repeat(32),
            revision: "a".repeat(64),
            account: "b".repeat(64),
        },
        proof: crate::self_claim::Proven {
            pid: 42,
            start: "completion-fixture".into(),
        },
        started: std::time::Instant::now(),
        intent_sequence: 0,
        result: Mutex::new(None),
    });
    let key = ("owned-completion".into(), "enroll", job.command.id().into());
    jobs().lock().unwrap().insert(key.clone(), Arc::clone(&job));
    let worker = Arc::clone(&job);
    let worker_key = key.clone();
    let value = json!({"phase":"refused"});
    let result = value.clone();
    assert!(launch(&key, &job, || {
        assert!(
            jobs().try_lock().is_ok(),
            "launch must not hold the registry"
        );
        let task = owned_completion(&job, scope, move || {
            let table = jobs().lock().unwrap();
            assert!(Arc::ptr_eq(
                table.entries.get(&worker_key).unwrap(),
                &worker
            ));
            drop(table);
            Ok(Some(result))
        });
        let (done, retired) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            task();
            done.send(()).unwrap();
        });
        retired.recv_timeout(Duration::from_secs(60)).unwrap();
        thread.join().unwrap();
        Ok(())
    }));
    assert!(custody.retired());
    assert_eq!(*job.result.lock().unwrap(), Some(Ok(Some(value))));
    let mut table = jobs().lock().unwrap();
    assert!(Arc::ptr_eq(table.entries.get(&key).unwrap(), &job));
    table.remove(&key, &job);
    assert!(!table.entries.contains_key(&key));
}

#[test]
fn panicking_owned_work_publishes_failure_before_its_scope_retires() {
    let _serial = serial();
    struct PanicOnDrop;
    impl Drop for PanicOnDrop {
        fn drop(&mut self) {
            panic!("controlled panic payload drop");
        }
    }
    for drop_bomb in [false, true] {
        let lifetime = account_work::Lifetime::default();
        let scope = lifetime.enter().unwrap();
        let custody = scope.cancellation();
        let job = Arc::new(Job {
            cleanup_owner: None,
            command: Command::Enroll {
                operation: "9".repeat(32),
                revision: "a".repeat(64),
                account: "b".repeat(64),
            },
            proof: crate::self_claim::Proven {
                pid: 42,
                start: "panic-completion-fixture".into(),
            },
            started: std::time::Instant::now(),
            intent_sequence: 0,
            result: Mutex::new(None),
        });
        let (mut observer, owned) = std::os::unix::net::UnixStream::pair().unwrap();
        observer
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        let task = owned_completion(&job, scope, move || {
            let _owned = owned;
            if drop_bomb {
                std::panic::panic_any(PanicOnDrop);
            }
            panic!("controlled owned work panic");
        });
        let thread = std::thread::spawn(task);
        thread.join().unwrap();
        assert_eq!(std::io::Read::read(&mut observer, &mut [0]).unwrap(), 0);
        assert_eq!(*job.result.lock().unwrap(), Some(Err(())));
        assert!(custody.retired());
        lifetime
            .pause()
            .unwrap()
            .try_wait()
            .expect("joined work retired");
    }
}

#[test]
fn failed_launch_retires_captures_and_removes_only_its_own_reservation() {
    let _serial = serial();
    for replace in [false, true] {
        let lifetime = account_work::Lifetime::default();
        let scope = lifetime.enter().unwrap();
        let custody = scope.cancellation();
        let job = Arc::new(Job {
            cleanup_owner: None,
            command: Command::Enroll {
                operation: "8".repeat(32),
                revision: "a".repeat(64),
                account: "b".repeat(64),
            },
            proof: crate::self_claim::Proven {
                pid: 42,
                start: "failed-launch-fixture".into(),
            },
            started: std::time::Instant::now(),
            intent_sequence: 0,
            result: Mutex::new(None),
        });
        let replacement = Arc::new(Job {
            cleanup_owner: None,
            command: job.command.clone(),
            proof: crate::self_claim::Proven {
                start: "replacement-fixture".into(),
                ..job.proof.clone()
            },
            started: std::time::Instant::now(),
            intent_sequence: 0,
            result: Mutex::new(None),
        });
        let key = ("failed-launch".into(), "enroll", job.command.id().into());
        let sequence = {
            let mut table = jobs().lock().unwrap();
            table.insert(key.clone(), Arc::clone(&job));
            table.sequence
        };
        let (mut observer, owned) = std::os::unix::net::UnixStream::pair().unwrap();
        observer
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        let task = owned_completion(&job, scope, move || {
            drop(owned);
            panic!("a refused launch cannot run its work")
        });
        assert!(!launch(&key, &job, || {
            drop(task);
            if replace {
                jobs()
                    .lock()
                    .unwrap()
                    .insert(key.clone(), Arc::clone(&replacement));
            }
            Err(std::io::Error::other("controlled launch refusal"))
        }));
        assert!(custody.retired());
        assert_eq!(std::io::Read::read(&mut observer, &mut [0]).unwrap(), 0);
        assert!(matches!(*job.result.lock().unwrap(), Some(Err(()))));
        let mut table = jobs().lock().unwrap();
        assert!(
            table.sequence > sequence,
            "failed admission invalidates old discovery"
        );
        if replace {
            assert!(Arc::ptr_eq(table.entries.get(&key).unwrap(), &replacement));
            assert!(replacement.result.lock().unwrap().is_none());
            table.remove(&key, &replacement);
        }
        assert!(!table.entries.contains_key(&key));
    }
}

#[test]
fn pending_publication_intent_blocks_empty_discovery_before_durable_admission() {
    let _serial = serial();
    for command in [
        Command::Enroll {
            operation: "b".repeat(32),
            revision: "c".repeat(64),
            account: accounts_control::account_id("main"),
        },
        Command::Poll("b".repeat(32)),
    ] {
        let (root, env, proof, sid) = fixture();
        let key = (sid.clone(), command.kind(), command.id().to_owned());
        let job = Arc::new(Job {
            cleanup_owner: None,
            command,
            proof: proof.clone(),
            started: std::time::Instant::now(),
            intent_sequence: 0,
            result: Mutex::new(None),
        });
        jobs().lock().unwrap().insert(key.clone(), Arc::clone(&job));
        assert!(discovery(&env, &sid, &proof).unwrap().is_none());
        let replacement = crate::self_claim::Proven {
            start: "different-birth".into(),
            ..proof.clone()
        };
        assert!(discovery(&env, &sid, &replacement).unwrap().is_some());
        complete(&job, Err(()));
        assert!(discovery(&env, &sid, &proof).unwrap().is_some());
        assert!(
            !env.state.exists(),
            "discovery never creates account custody"
        );
        assert!(!root.path().join("home").exists());
        jobs().lock().unwrap().remove(&key, &job);
    }
}

#[test]
fn unstarted_reservation_removal_invalidates_discovery_without_erasing_a_replacement() {
    let _serial = serial();
    for replace in [false, true] {
        let (_root, env, proof, sid) = fixture();
        let command = Command::Enroll {
            operation: "e".repeat(32),
            revision: "c".repeat(64),
            account: accounts_control::account_id("main"),
        };
        let key = (sid.clone(), "enroll", command.id().to_owned());
        let make_job = || {
            Arc::new(Job {
                cleanup_owner: None,
                command: command.clone(),
                proof: proof.clone(),
                started: std::time::Instant::now(),
                intent_sequence: 0,
                result: Mutex::new(None),
            })
        };
        let job = make_job();
        let lifetime = account_work::Lifetime::default();
        let scope = lifetime.enter().unwrap();
        let mut reservation = Reservation::new(key.clone());
        {
            let mut table = jobs().lock().unwrap();
            reservation.arm(&job, &scope);
            table.insert(key.clone(), Arc::clone(&job));
        }
        drop(scope);
        let pause = lifetime
            .pause()
            .unwrap()
            .try_wait()
            .expect_err("reservation still owns admission");
        assert!(discovery(&env, &sid, &proof).unwrap().is_none());
        let replacement = replace.then(make_job);
        if let Some(replacement) = &replacement {
            jobs()
                .lock()
                .unwrap()
                .insert(key.clone(), Arc::clone(replacement));
        }
        let old = discovery_with(&env, &sid, &proof, || drop(reservation)).unwrap();
        assert!(old.is_none());
        assert!(pause.try_wait().is_ok());
        if let Some(replacement) = replacement {
            assert!(
                jobs()
                    .lock()
                    .unwrap()
                    .entries
                    .get(&key)
                    .is_some_and(|current| Arc::ptr_eq(current, &replacement))
            );
            assert!(discovery(&env, &sid, &proof).unwrap().is_none());
            jobs().lock().unwrap().remove(&key, &replacement);
        } else {
            assert!(!jobs().lock().unwrap().entries.contains_key(&key));
            assert!(discovery(&env, &sid, &proof).unwrap().is_some());
        }
        assert!(!env.state.exists());
    }
}

#[test]
fn retained_native_b_recovers_after_a_publication_without_replaying_authentication() {
    let _serial = serial();
    let (_root, _old_env, proof, sid) = fixture();
    let native = enrollment::NativeRowRecoveryFixture::new(&caller(&sid, &proof));
    native.retire_originals().unwrap();
    let b = native.requests()[1].clone();
    assert_eq!(
        native.publish_first().unwrap().phase,
        enrollment::Phase::Applied
    );
    let env = native.env();
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    let roster = env.accounts_roster_for(accounts::Cli::Codex).unwrap();
    let records = store.peek_account_records(&roster, env.now).unwrap();
    assert_ne!(
        accounts::pool_authority_revision(&records.raw_roster, &records.logins).unwrap(),
        b.revision
    );
    assert!(
        records
            .logins
            .enrollment
            .native_recovery_pending(&b)
            .unwrap()
    );
    let lifetime = account_work::Lifetime::default();
    // Reusing the operation id cannot change its original body.
    for (revision, account) in [
        ("d".repeat(64), accounts_control::account_id(&b.label)),
        (b.revision.clone(), accounts_control::account_id("alpha")),
    ] {
        let changed = Command::Enroll {
            operation: b.id.clone(),
            revision,
            account,
        };
        assert!(
            run_in(&changed, &sid, &proof, &lifetime, env, |_, _| {
                panic!("changed retained request cannot dispatch")
            })
            .is_err()
        );
    }
    let dispatched = Cell::new(0);
    let recovered = run_in(
        &Command::Poll(b.id.clone()),
        &sid,
        &proof,
        &lifetime,
        env,
        |work, env| {
            dispatched.set(dispatched.get() + 1);
            assert_eq!(env.home, native.env().home);
            native.run_recovery(work)
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(dispatched.get(), 1);
    assert_eq!(recovered["phase"].as_str(), Some("applied"));
    assert_eq!(recovered["generation"].as_u64(), Some(2));
    assert!(
        !store
            .load_logins(env.now)
            .unwrap()
            .0
            .enrollment
            .native_recovery_pending(&b)
            .unwrap()
    );
}

#[test]
fn retained_native_cancellation_retires_without_parsing_the_current_roster() {
    let _serial = serial();
    let (_root, _old_env, proof, sid) = fixture();
    let native = enrollment::NativeRowRecoveryFixture::new(&caller(&sid, &proof));
    native.retire_originals().unwrap();
    native.cancel_first_after_publication_claim().unwrap();
    let request = native.requests()[0].clone();
    let env = native.env();
    let roster = env.accounts_roster_for(accounts::Cli::Codex).unwrap();
    std::fs::write(&roster, "not [valid TOML").unwrap();
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    let before = store.load_logins(env.now).unwrap().0;
    assert!(before.enrollment.native_recovery_pending(&request).unwrap());
    let lifetime = account_work::Lifetime::default();
    let dispatched = Cell::new(0);
    let cancelled = run_in(
        &Command::Poll(request.id.clone()),
        &sid,
        &proof,
        &lifetime,
        env,
        |work, _| {
            dispatched.set(dispatched.get() + 1);
            native.run_recovery(work)
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(dispatched.get(), 1);
    assert_eq!(cancelled["phase"].as_str(), Some("cancelled"));
    let after = store.load_logins(env.now).unwrap().0;
    assert_eq!(after.labels, before.labels);
    assert_eq!(after.generation_clock, before.generation_clock);
    assert_eq!(std::fs::read_to_string(roster).unwrap(), "not [valid TOML");
    assert!(!after.enrollment.native_recovery_pending(&request).unwrap());
    for replay in [
        Command::Poll(request.id.clone()),
        Command::Cancel(request.id.clone()),
        Command::Enroll {
            operation: request.id.clone(),
            revision: request.revision.clone(),
            account: accounts_control::account_id(&request.label),
        },
    ] {
        assert_eq!(
            run_in(&replay, &sid, &proof, &lifetime, env, |_, _| {
                panic!("terminal native replay cannot dispatch work")
            })
            .unwrap()
            .unwrap(),
            cancelled
        );
    }
}

#[test]
fn live_native_original_can_be_cancelled_and_retired_with_a_malformed_roster() {
    let _serial = serial();
    let (_root, _old_env, proof, sid) = fixture();
    let native = enrollment::NativeRowRecoveryFixture::new(&caller(&sid, &proof));
    let request = native.requests()[0].clone();
    let env = native.env();
    let lifetime = account_work::Lifetime::default();
    let (work, control) = native.first_original(lifetime.enter().unwrap());
    let control = Arc::new(OwnedControl {
        control,
        env: env.clone(),
    });
    let key = (sid.clone(), request.id.clone());
    controls()
        .lock()
        .unwrap()
        .insert(key.clone(), Arc::clone(&control));
    let roster = env.accounts_roster_for(accounts::Cli::Codex).unwrap();
    std::fs::write(&roster, "not [valid TOML").unwrap();
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    let before = store.load_logins(env.now).unwrap().0;
    assert!(before.enrollment.native_cancellation(&request).unwrap());
    assert!(!before.enrollment.native_recovery_pending(&request).unwrap());
    let cancel = Command::Cancel(request.id.clone());
    let pending = run_in(&cancel, &sid, &proof, &lifetime, env, |_, _| {
        panic!("cancellation cannot dispatch authentication")
    })
    .unwrap()
    .unwrap();
    assert_eq!(pending["phase"].as_str(), Some("cancel_requested"));
    assert!(!control.work_retired());
    drop(work);
    assert!(control.work_retired());
    let cancelled = run_in(&cancel, &sid, &proof, &lifetime, env, |_, _| {
        panic!("retirement cannot dispatch authentication")
    })
    .unwrap()
    .unwrap();
    assert_eq!(cancelled["phase"].as_str(), Some("cancelled"));
    assert!(!controls().lock().unwrap().contains_key(&key));
    let after = store.load_logins(env.now).unwrap().0;
    assert_eq!(after.labels, before.labels);
    assert_eq!(after.generation_clock, before.generation_clock);
    assert_eq!(std::fs::read_to_string(roster).unwrap(), "not [valid TOML");
}

#[test]
fn fresh_enrollment_cannot_use_a_revision_from_before_another_row_was_added() {
    let _serial = serial();
    let (_root, env, proof, sid) = fixture();
    let baseline = discovery(&env, &sid, &proof).unwrap().unwrap();
    let path = env.accounts_roster_for(accounts::Cli::Codex).unwrap();
    let original = std::fs::read_to_string(&path).unwrap();
    let other = accounts::Row {
        label: "other".into(),
        email: Some("other@example.test".into()),
        ..Default::default()
    };
    std::fs::write(
        &path,
        accounts::append_rows_for(accounts::Cli::Codex, &original, &[other]).unwrap(),
    )
    .unwrap();
    let current = discovery(&env, &sid, &proof).unwrap().unwrap();
    assert_ne!(baseline["revision"], current["revision"]);
    let command = Command::Enroll {
        operation: "b".repeat(32),
        revision: baseline["revision"].as_str().unwrap().into(),
        account: accounts_control::account_id("main"),
    };
    let lifetime = account_work::Lifetime::default();
    let refused = run_in(&command, &sid, &proof, &lifetime, &env, |_, _| {
        panic!("fresh stale enrollment cannot dispatch provider or recovery work")
    })
    .unwrap()
    .unwrap();
    assert_eq!(refused["phase"].as_str(), Some("refused"));
    assert_eq!(refused["generation"], Value::Null);
}

#[test]
fn removed_target_gets_a_durable_refusal_that_source_reversion_cannot_revive() {
    let _serial = serial();
    let (_root, env, proof, sid) = fixture();
    let baseline = discovery(&env, &sid, &proof).unwrap().unwrap();
    let path = env.accounts_roster_for(accounts::Cli::Codex).unwrap();
    let original = std::fs::read_to_string(&path).unwrap();
    let command = Command::Enroll {
        operation: "e".repeat(32),
        revision: baseline["revision"].as_str().unwrap().into(),
        account: accounts_control::account_id("main"),
    };
    std::fs::write(&path, "").unwrap();
    let lifetime = account_work::Lifetime::default();
    let refused = run_in(&command, &sid, &proof, &lifetime, &env, |_, _| {
        panic!("an unresolved opaque target has zero provider authority")
    })
    .unwrap()
    .unwrap();
    assert_eq!(refused["phase"].as_str(), Some("refused"));
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    let (logins, _) = store.load_logins(env.now).unwrap();
    assert_eq!(
        aterm_json::from_str::<Value>(&logins.to_json()).unwrap()["schema"].as_u64(),
        Some(8)
    );
    assert!(
        logins
            .enrollment
            .admitted(command.id(), &caller(&sid, &proof))
            .is_err()
    );
    std::fs::write(&path, &original).unwrap();
    for retained in [
        command.clone(),
        Command::Poll(command.id().into()),
        Command::Cancel(command.id().into()),
    ] {
        assert_eq!(
            run_in(&retained, &sid, &proof, &lifetime, &env, |_, _| panic!(
                "a refused ID cannot become a provider attempt"
            ))
            .unwrap()
            .unwrap(),
            refused
        );
    }
    let fresh = discovery(&env, &sid, &proof).unwrap().unwrap();
    let next = Command::Enroll {
        operation: "f".repeat(32),
        revision: fresh["revision"].as_str().unwrap().into(),
        account: accounts_control::account_id("main"),
    };
    let provider = FailedProvider(Cell::new(0), ProviderEnd::Failed, Default::default());
    let running = run_in(&next, &sid, &proof, &lifetime, &env, |work, env| {
        work.run_with_host(env, &provider)
    })
    .unwrap()
    .unwrap();
    assert_eq!(running["phase"].as_str(), Some("running"));
    assert_eq!(provider.0.get(), 1);
    let cancelled = run_in(
        &Command::Cancel(next.id().into()),
        &sid,
        &proof,
        &lifetime,
        &env,
        |_, _| panic!("no provider replay"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(cancelled["phase"].as_str(), Some("cancelled"));
}

#[test]
fn completed_and_removed_start_cannot_hide_behind_an_old_empty_read() {
    let _serial = serial();
    let (_root, env, proof, sid) = fixture();
    let baseline = discovery(&env, &sid, &proof).unwrap().unwrap();
    let command = Command::Enroll {
        operation: "d".repeat(32),
        revision: baseline["revision"].as_str().unwrap().into(),
        account: accounts_control::account_id("main"),
    };
    let key = (sid.clone(), "enroll", command.id().to_owned());
    let job = Arc::new(Job {
        cleanup_owner: None,
        command: command.clone(),
        proof: proof.clone(),
        started: std::time::Instant::now(),
        intent_sequence: 0,
        result: Mutex::new(None),
    });
    let lifetime = account_work::Lifetime::default();
    let provider = FailedProvider(Cell::new(0), ProviderEnd::Failed, Default::default());
    let old = discovery_with(&env, &sid, &proof, || {
        jobs().lock().unwrap().insert(key.clone(), Arc::clone(&job));
        let result = run_in(&command, &sid, &proof, &lifetime, &env, |work, env| {
            work.run_with_host(env, &provider)
        });
        assert!(result.is_ok());
        complete(&job, result);
        jobs().lock().unwrap().remove(&key, &job);
    })
    .unwrap();
    assert!(
        old.is_none(),
        "completion and ACK removal cannot make the old empty image authoritative"
    );
    let fresh = discovery(&env, &sid, &proof).unwrap().unwrap();
    assert_eq!(
        fresh["operations"][0]["operation"].as_str(),
        Some(command.id())
    );
    assert_eq!(provider.0.get(), 1);
    let cancelled = run_in(
        &Command::Cancel(command.id().to_owned()),
        &sid,
        &proof,
        &lifetime,
        &env,
        |_, _| panic!("no provider replay"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(cancelled["phase"].as_str(), Some("cancelled"));
}

#[test]
fn protocol_and_caller_binding_do_not_accept_extra_authority() {
    let operation = "a".repeat(32);
    assert!(
        parse(
            &format!("enrollment-poll schema=1 operation={operation}"),
            false
        )
        .is_some()
    );
    let diagnostic = format!(
        "enrollment-diagnostics schema=1 request={} operation={operation}",
        "b".repeat(32)
    );
    assert_eq!(
        parse(&diagnostic, true),
        Some(Command::Diagnostics {
            request: "b".repeat(32),
            operation: operation.clone(),
        })
    );
    assert!(parse(&diagnostic, false).is_none());
    for bad in [
        diagnostic.replace("schema=1", "schema=2"),
        diagnostic.replace("request=", "caller="),
        format!("{diagnostic} account={}", "c".repeat(64)),
        diagnostic.replace(&operation, &"A".repeat(32)),
    ] {
        assert!(parse(&bad, true).is_none());
    }
    for bad in [
        format!("enrollment-poll schema=2 operation={operation}"),
        format!(
            "enrollment-poll schema=1 operation={operation} account={}",
            "b".repeat(64)
        ),
        format!("enrollment-poll schema=1 operation={}", "A".repeat(32)),
        format!("enrollment-status schema=1 request={operation}"),
    ] {
        assert!(parse(&bad, false).is_none());
    }
    let proof = crate::self_claim::Proven {
        pid: 42,
        start: "first".into(),
    };
    let replacement = crate::self_claim::Proven {
        start: "second".into(),
        ..proof.clone()
    };
    assert_ne!(caller("sid", &proof), caller("sid", &replacement));
    assert_ne!(caller("sid", &proof), caller("other", &proof));
}
