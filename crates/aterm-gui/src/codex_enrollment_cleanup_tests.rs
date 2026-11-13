// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::tests::{fixture, serial};
use super::*;
use crate::App;

fn native_fixture() -> (
    enrollment::NativeRowRecoveryFixture,
    crate::self_claim::Proven,
    String,
) {
    let (_world, _, proof, sid) = fixture();
    let native = enrollment::NativeRowRecoveryFixture::new(&caller(&sid, &proof));
    native.retire_originals().unwrap();
    native.cancel_first_after_publication_claim().unwrap();
    (native, proof, sid)
}
fn remove_job(sid: &str, command: &Command, job: &Arc<Job>) {
    registry().lock().unwrap().remove(
        &(sid.to_owned(), command.kind(), command.id().to_owned()),
        job,
    );
}

#[test]
fn paused_cold_cleanup_is_counted_before_dispatch_and_independent_of_begin_budget() {
    let _serial = serial();
    let (native, proof, sid) = native_fixture();
    let request = native.requests()[0].clone();
    let env = native.env();
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    let before = store.load_logins(env.now).unwrap().0;
    let roster = env.accounts_roster_for(accounts::Cli::Codex).unwrap();
    std::fs::write(&roster, "not [valid TOML").unwrap();
    let normal_keys: Vec<_> = (0..MAX_JOBS)
        .map(|index| {
            let command = Command::Poll(format!("{index:032x}"));
            let key = (sid.clone(), command.kind(), command.id().to_owned());
            jobs().lock().unwrap().insert(
                key.clone(),
                Arc::new(Job {
                    cleanup_owner: None,
                    command,
                    proof: proof.clone(),
                    started: std::time::Instant::now(),
                    intent_sequence: 0,
                    result: Mutex::new(None),
                }),
            );
            key
        })
        .collect();
    let lifetime = account_work::Lifetime::default();
    let pause = lifetime.pause().unwrap();
    assert!(lifetime.enter().is_none());
    let command = Command::Poll(request.id.clone());
    let queued = std::cell::RefCell::new(None);
    let job = submit(
        env,
        &sid,
        &proof,
        &command,
        &lifetime,
        || true,
        |work| {
            assert!(App::enrollment_retirement_pending(&lifetime));
            assert!(jobs().try_lock().is_ok());
            assert!(controls().try_lock().is_ok());
            assert!(registry().try_lock().is_ok());
            assert!(cleanup_controls().try_lock().is_ok());
            *queued.borrow_mut() = Some(work);
            Ok(())
        },
    )
    .unwrap()
    .unwrap();
    assert!(
        job.result.lock().unwrap().is_none(),
        "counted Pending precedes Waiting"
    );
    let pause = match pause.try_wait() {
        Err(pause) => pause,
        Ok(_) => panic!("queued release owns the drain count"),
    };
    let foreign = account_work::Lifetime::default();
    assert!(
        submit(
            env,
            &sid,
            &proof,
            &command,
            &foreign,
            || true,
            |_| panic!("foreign App cannot dispatch")
        )
        .is_err()
    );
    assert!(foreign.pause().unwrap().try_wait().is_ok());
    queued.borrow_mut().take().unwrap()();
    assert_eq!(
        job.result
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()["phase"]
            .as_str(),
        Some("cancelled")
    );
    assert!(!App::enrollment_retirement_pending(&lifetime));
    let drained = pause
        .try_wait()
        .unwrap_or_else(|_| panic!("actual cleanup retired"));
    let after = store.load_logins(env.now).unwrap().0;
    assert_eq!(before.labels, after.labels);
    assert_eq!(before.generation_clock, after.generation_clock);
    assert_eq!(std::fs::read_to_string(&roster).unwrap(), "not [valid TOML");
    remove_job(&sid, &command, &job);
    let replay = submit(
        env,
        &sid,
        &proof,
        &command,
        &lifetime,
        || panic!("terminal replay is metadata only"),
        |_| panic!("sealed terminal cannot dispatch"),
    )
    .unwrap()
    .unwrap();
    assert!(replay.result.lock().unwrap().is_some());
    drop(drained);
    for key in normal_keys {
        jobs().lock().unwrap().entries.remove(&key);
    }
}

#[test]
fn cold_cleanup_rejects_changed_body_and_caller_before_admission() {
    let _serial = serial();
    let (native, proof, sid) = native_fixture();
    let request = native.requests()[0].clone();
    let lifetime = account_work::Lifetime::default();
    let before = native
        .env()
        .accounts_store_for(accounts::Cli::Codex)
        .unwrap()
        .load_logins(native.env().now)
        .unwrap()
        .0;
    for command in [
        Command::Enroll {
            operation: request.id.clone(),
            revision: "f".repeat(64),
            account: accounts_control::account_id(&request.label),
        },
        Command::Enroll {
            operation: request.id.clone(),
            revision: request.revision.clone(),
            account: "f".repeat(64),
        },
    ] {
        assert!(
            submit(
                native.env(),
                &sid,
                &proof,
                &command,
                &lifetime,
                || true,
                |_| panic!("changed body dispatch")
            )
            .is_err()
        );
    }
    let foreign = crate::self_claim::Proven {
        pid: proof.pid + 1,
        start: proof.start.clone(),
    };
    assert!(
        submit(
            native.env(),
            &sid,
            &foreign,
            &Command::Poll(request.id.clone()),
            &lifetime,
            || true,
            |_| panic!("foreign caller dispatch")
        )
        .unwrap()
        .is_none()
    );
    assert!(
        submit(
            native.env(),
            &sid,
            &proof,
            &Command::Poll("f".repeat(32)),
            &lifetime,
            || true,
            |_| panic!("unknown operation dispatch")
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(
        native
            .env()
            .accounts_store_for(accounts::Cli::Codex)
            .unwrap()
            .load_logins(native.env().now)
            .unwrap()
            .0,
        before
    );
    assert!(lifetime.pause().unwrap().try_wait().is_ok());
}

#[test]
fn cold_cleanup_failed_dispatch_rolls_back_before_its_pending_count_retires() {
    let _serial = serial();
    let (native, proof, sid) = native_fixture();
    let request = native.requests()[0].clone();
    let command = Command::Poll(request.id.clone());
    let lifetime = account_work::Lifetime::default();
    let pause = std::cell::RefCell::new(Some(lifetime.pause().unwrap()));
    let store = native
        .env()
        .accounts_store_for(accounts::Cli::Codex)
        .unwrap();
    let before = store.load_logins(native.env().now).unwrap().0;
    assert!(
        submit(
            native.env(),
            &sid,
            &proof,
            &command,
            &lifetime,
            || true,
            |work| {
                drop(work); // Failed spawn drops its callback before returning.
                let waiting = pause.borrow_mut().take().unwrap();
                let waiting = match waiting.try_wait() {
                    Err(waiting) => waiting,
                    Ok(_) => panic!("rollback still owns the unstarted Pending work"),
                };
                *pause.borrow_mut() = Some(waiting);
                Err(std::io::Error::other("injected thread dispatch failure"))
            }
        )
        .is_err()
    );
    assert!(!pending(&lifetime));
    assert!(!registry().lock().unwrap().entries.contains_key(&(
        sid.clone(),
        command.kind(),
        command.id().to_owned()
    )));
    assert_eq!(store.load_logins(native.env().now).unwrap().0, before);
    let drained = pause
        .into_inner()
        .unwrap()
        .try_wait()
        .unwrap_or_else(|_| panic!("failed dispatch leaves no Pending count"));
    assert!(
        submit(
            native.env(),
            &sid,
            &proof,
            &command,
            &lifetime,
            || true,
            |_| panic!("sealed cleanup dispatch")
        )
        .is_err()
    );
    drop(drained);
}

#[test]
fn cold_cleanup_can_complete_before_dispatch_returns() {
    let _serial = serial();
    let (native, proof, sid) = native_fixture();
    let command = Command::Cancel(native.requests()[0].id.clone());
    let lifetime = account_work::Lifetime::default();
    let pause = lifetime.pause().unwrap();
    let job = submit(
        native.env(),
        &sid,
        &proof,
        &command,
        &lifetime,
        || true,
        |work| {
            assert!(App::enrollment_retirement_pending(&lifetime));
            assert!(registry().try_lock().is_ok());
            assert!(cleanup_controls().try_lock().is_ok());
            work();
            Ok(())
        },
    )
    .unwrap()
    .unwrap();
    assert!(job.result.lock().unwrap().as_ref().unwrap().is_ok());
    assert!(!App::enrollment_retirement_pending(&lifetime));
    assert!(pause.try_wait().is_ok());
    remove_job(&sid, &command, &job);
}

#[test]
fn cold_cleanup_write_failure_retains_nonce_for_actual_app_settlement() {
    let _serial = serial();
    let (native, proof, sid) = native_fixture();
    let request = native.requests()[0].clone();
    let command = Command::Poll(request.id.clone());
    let env = native.env();
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    let before = store.load_logins(env.now).unwrap().0;
    let lifetime = account_work::Lifetime::default();
    let pause = lifetime.pause().unwrap();
    let queued = std::cell::RefCell::new(None);
    let job = submit(
        env,
        &sid,
        &proof,
        &command,
        &lifetime,
        || true,
        |work| {
            *queued.borrow_mut() = Some(work);
            Ok(())
        },
    )
    .unwrap()
    .unwrap();
    let control = cleanup_controls()
        .lock()
        .unwrap()
        .get(&(sid.clone(), request.id.clone()))
        .cloned()
        .unwrap();
    // This metadata-only transaction does not touch wake. Obstruct its
    // actual retained publication witness after counted admission instead.
    let witness = store.logins_witness_file();
    let original_witness = witness.with_extension("cold-retirement-held");
    std::fs::rename(&witness, &original_witness).unwrap();
    std::fs::create_dir(&witness).unwrap();
    queued.borrow_mut().take().unwrap()();
    assert!(job.result.lock().unwrap().as_ref().unwrap().is_err());
    assert!(control.control.work_retired());
    assert!(
        control.control.retired().is_some(),
        "actual nonce receipt survives write uncertainty"
    );
    assert!(App::enrollment_retirement_pending(&lifetime));
    let foreign = account_work::Lifetime::default();
    assert!(App::seal_enrollment_retirement(&foreign));
    assert!(App::enrollment_retirement_pending(&lifetime));
    std::fs::remove_dir(&witness).unwrap();
    std::fs::rename(&original_witness, &witness).unwrap();
    let drained = pause
        .try_wait()
        .unwrap_or_else(|_| panic!("kernel resources really retired"));
    assert!(App::seal_enrollment_retirement(&lifetime));
    assert!(!App::enrollment_retirement_pending(&lifetime));
    let after = store.load_logins(env.now).unwrap().0;
    assert_eq!(
        after
            .enrollment
            .retained(&request.id, &request.caller)
            .unwrap()
            .1
            .phase,
        enrollment::Phase::Cancelled
    );
    assert_eq!(before.labels, after.labels);
    assert_eq!(before.generation_clock, after.generation_clock);
    remove_job(&sid, &command, &job);
    drop(drained);
}
