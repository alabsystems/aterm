// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn setup_status_reads_its_private_journal_and_scripted_accounts_through_admission() {
    use aterm_cli::macos_doctor::{InitialSetupAdmissionStopTest, initial_setup_observation_at};
    use aterm_types::macos_setup::{Read, Standing, SyncKind};

    let _globals = globals();
    let _io = TestIo::install();
    OBSERVED_ROWS.lock().unwrap().rows.clear();
    let root = aterm_tempfile::tempdir().unwrap();
    let mut plan = left_as_is();
    plan.selection.set(Setting::Secrets, true);
    let writer = InitialSetupAdmissionStopTest::restart(root.path())
        .unwrap()
        .with_plan(plan.clone());
    let earlier = writer.choose().unwrap();
    let revision = writer.choose().unwrap();
    assert!(
        revision > earlier,
        "a journal identity distinct from its first run"
    );
    let accounts = Observation {
        subject: Subject::Sync(SyncKind::Accounts),
        standing: Standing::Waiting,
        now: Some("scripted account status".into()),
        after_login: None,
        wanted: "Provisioned".into(),
        note: Some("the test's account observation".into()),
        detail: None,
        read: Read::Unstamped,
    };
    change(|script| {
        script.status_root = Some(root.path().to_owned());
        script.accounts_observation = Some(accounts.clone());
        script.alab = true;
        script.admits = [Some(revision)].into();
    });

    let identity = initial_setup_observation_at(root.path()).unwrap();
    let busy = take_initial_setup_slot().unwrap();
    assert_eq!(busy.observation.as_ref(), Some(&identity));
    assert_eq!(script().observation_reads, 1);
    let status = initial_setup_status();
    assert!(status.busy && status.alab);
    assert_eq!(status.plan, Some(plan.clone()));
    assert_eq!(status.run.as_ref().map(|run| run.revision), Some(revision));
    assert!(status.run.as_ref().is_some_and(|run| run.allowed));
    assert!(status.observed.contains(&accounts));
    {
        let script = script();
        assert_eq!(
            (
                script.status_reads,
                script.accounts_reads,
                script.alab_reads
            ),
            (1, 1, 1)
        );
    }
    drop(busy);

    let (receipt, accepted) = aterm_gui::InitialSetupApplyReceipt::detached(true);
    let before = script().status_reads;
    assert!(restart_initial_setup(plan, false, None, receipt));
    assert_eq!(accepted.recv_timeout(HANG), Ok(true));
    wait_until("the admitted run's status publication ends", || {
        let finished = script().finished_runs == 1;
        finished && test_io_idle()
    });
    {
        let script = script();
        assert_eq!(script.runs, [revision]);
        assert!(
            script.observation_reads > 1,
            "the worker reads its operation identity"
        );
        assert!(script.status_reads > before, "the worker publishes status");
        assert_eq!(script.accounts_reads, script.status_reads);
        assert_eq!(script.alab_reads, script.status_reads);
    }
    let status = initial_setup_status();
    assert!(!status.busy && !status.queued);
    assert!(status.alab && status.observed.contains(&accounts));
    assert_eq!(status.run.as_ref().map(|run| run.revision), Some(revision));

    let mut without_secrets = status.plan.unwrap();
    without_secrets.selection.set(Setting::Secrets, false);
    let next = writer.with_plan(without_secrets.clone()).choose().unwrap();
    change(|script| script.alab = false);
    let before = {
        let script = script();
        (
            script.status_reads,
            script.accounts_reads,
            script.alab_reads,
        )
    };
    let status = initial_setup_status();
    assert_eq!(status.plan, Some(without_secrets));
    assert_eq!(status.run.as_ref().map(|run| run.revision), Some(next));
    assert!(!status.alab && !status.busy && !status.queued);
    assert!(
        !status
            .observed
            .iter()
            .any(|row| row.subject == accounts.subject)
    );
    let script = script();
    assert_eq!(
        (
            script.status_reads,
            script.accounts_reads,
            script.alab_reads
        ),
        (before.0 + 1, before.1, before.2 + 1)
    );
}

#[test]
fn setup_io_retains_a_busy_slots_final_status_until_it_has_published() {
    let _globals = globals();
    let io = TestIo::install();
    let root = aterm_tempfile::tempdir().unwrap();
    change(|script| {
        script.status_root = Some(root.path().to_owned());
        script.accounts_hold = true;
    });
    assert!(
        snapshot(root.path())
            .plan
            .selection
            .contains(Setting::Secrets)
    );
    let busy = take_initial_setup_slot().unwrap();
    let (ended, done) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        drop(busy);
        ended.send(()).unwrap();
    });
    wait_until("the final status reaches its account observation", || {
        script().accounts_reads == 1
    });
    assert!(!INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire));
    let retained = TEST_IO_OPERATIONS.load(std::sync::atomic::Ordering::Acquire) > 0;
    drop(io);
    assert_eq!(done.recv_timeout(HANG), Ok(()));
    worker.join().unwrap();
    assert!(retained, "the final publication still owns its provider");
    assert_eq!(
        TEST_IO_OPERATIONS.load(std::sync::atomic::Ordering::Acquire),
        0
    );
}

#[test]
fn setup_io_keeps_restore_error_tail_work_and_releases_unwound_operations() {
    let _globals = globals();
    {
        let io = TestIo::install();
        change(|script| script.restore_preflight_hold = true);
        let busy = take_initial_setup_slot().unwrap();
        let (ended, done) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            run_setup_restore(busy, |_, _| aterm_cli::macos_doctor::InitialSetupObserved {
                result: Err("injected restore failure".into()),
                observation: None,
            });
            ended.send(()).unwrap();
        });
        wait_until(
            "the failed restore reaches its follow-up after releasing busy",
            || script().restore_preflight_reads == 1,
        );
        assert!(!INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire));
        assert!(
            script().runs.is_empty(),
            "no normal run carried this error path"
        );
        let retained = TEST_IO_OPERATIONS.load(std::sync::atomic::Ordering::Acquire) > 0;
        drop(io);
        assert_eq!(done.recv_timeout(HANG), Ok(()));
        worker.join().unwrap();
        assert!(
            retained,
            "the error's post-busy work still owns its provider"
        );
    }
    {
        let io = TestIo::install();
        let busy = take_initial_setup_slot().unwrap();
        let (ended, done) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_setup_restore(busy, |_, _| panic!("injected restore unwind"));
            }));
            ended.send(result.is_err()).unwrap();
        });
        assert_eq!(done.recv_timeout(HANG), Ok(true));
        worker.join().unwrap();
        assert!(!INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire));
        assert!(
            script().runs.is_empty(),
            "no normal run carried this unwind path"
        );
        drop(io);
    }
    assert_eq!(
        TEST_IO_OPERATIONS.load(std::sync::atomic::Ordering::Acquire),
        0
    );
}
