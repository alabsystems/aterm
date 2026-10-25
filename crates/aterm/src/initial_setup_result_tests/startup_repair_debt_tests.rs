// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_cli::macos_doctor::{
    InitialSetupAdmissionFault, InitialSetupAdmissionStopTest,
    set_aside_unreadable_setup_at_observing,
    set_aside_unreadable_setup_at_observing_before_checkpoint,
};

/// Recovery has already repaired Plan and Decision when allocation reads its counter.
/// A passing filesystem failure here leaves a real readable, unreserved prefix.
fn fail_repair_before_checkpoint(sent: &mut StartupSent, root: &Path) {
    let counter = root.join("run-revision.json");
    let prior = std::fs::read(&counter).unwrap();
    std::fs::write(root.join("first-run.json"), b"{").unwrap();
    let mut observed = None;
    let mut installed = false;
    let result = sent.observe_repair_with(|published| {
        set_aside_unreadable_setup_at_observing_before_checkpoint(
            root,
            false,
            || {
                std::fs::remove_file(&counter).unwrap();
                std::fs::create_dir(&counter).unwrap();
                installed = true;
            },
            |revision| {
                observed = Some(revision);
                published(revision);
            },
        )
    });
    assert!(installed, "recovery reached its readable checkpoint prefix");
    std::fs::remove_dir(&counter).unwrap();
    std::fs::write(&counter, prior).unwrap();
    assert!(result.is_err());
    assert_eq!(observed, None);
    assert!(InitialSetupAdmissionStopTest::startup(root).is_ok());
}

fn choose_newer(setup: &InitialSetupAdmissionStopTest<'_>) -> u64 {
    let sent = send_admission();
    let revision = setup.choose().unwrap();
    // The same successful-admission bookkeeping as LiveSetupRestart::admitted.
    retire_unconfirmed_admissions(sent);
    register_admitted_run(revision, sent, false, None);
    revision
}

#[test]
fn startup_repair_debt_a_noop_cannot_revive_uncertainty_retired_by_a_newer_choice() {
    let _globals = globals();
    for during_retry in [false, true] {
        let _io = TestIo::install();
        let root = aterm_tempfile::tempdir().unwrap();
        change(|script| script.stop_root = Some(root.path().to_owned()));
        let mut setup = InitialSetupAdmissionStopTest::restart(root.path()).unwrap();
        let prior = setup
            .persist(InitialSetupAdmissionFault::None)
            .unwrap()
            .unwrap();
        let mut sent = StartupSent::default();
        sent.send();
        fail_repair_before_checkpoint(&mut sent, root.path());

        let mut chosen = None;
        if !during_retry {
            chosen = Some(choose_newer(&setup));
        }
        let mut successor = None;
        let mut observed = None;
        let result = sent.observe_repair_with(|published| {
            if during_retry {
                chosen = Some(choose_newer(&setup));
            }
            successor = Some(setup.snapshot().unwrap().observation);
            set_aside_unreadable_setup_at_observing(root.path(), false, |revision| {
                observed = Some(revision);
                published(revision);
            })
        });
        assert!(
            result.unwrap().is_none(),
            "the newer choice retired the old recovery"
        );
        assert_eq!(observed, None, "a no-op acquires no successor checkpoint");
        let chosen = chosen.unwrap();
        assert!(chosen > prior);
        assert_eq!(setup.admission().unwrap().unwrap().revision, chosen);
        assert_eq!(setup.snapshot().unwrap().observation, successor.unwrap());
        drop(sent);

        assert!(stop_initial_setup());
        wait_until("the newer run's exact Stop is saved", || {
            stop_worker_idle() && script().stops.contains(&chosen)
        });
        assert!(
            settle_setup_stop(Duration::ZERO),
            "a superseded repair must not recreate abandoned unknown Stop debt"
        );
        assert!(setup.snapshot().unwrap().finished);
        assert!(script().runs.is_empty());
        clear_setup_run(chosen);
    }
}

#[test]
fn startup_repair_debt_a_new_failed_mutation_after_retirement_still_owes_its_stop() {
    let _globals = globals();
    let _io = TestIo::install();
    let root = aterm_tempfile::tempdir().unwrap();
    change(|script| script.stop_root = Some(root.path().to_owned()));
    let mut setup = InitialSetupAdmissionStopTest::restart(root.path()).unwrap();
    setup
        .persist(InitialSetupAdmissionFault::None)
        .unwrap()
        .unwrap();
    let mut sent = StartupSent::default();
    sent.send();
    fail_repair_before_checkpoint(&mut sent, root.path());
    let chosen = choose_newer(&setup);
    assert!(stop_saves().under_way.iter().all(|debt| !debt.unconfirmed));

    // This is another actual repair attempt, after the admission retired the first error.
    fail_repair_before_checkpoint(&mut sent, root.path());
    drop(sent);
    assert!(stop_initial_setup());
    wait_until("the known older target's Stop is saved", || {
        stop_worker_idle() && script().stops.contains(&chosen)
    });
    assert!(
        !settle_setup_stop(Duration::ZERO),
        "a new mutation error must not inherit the earlier error's retirement"
    );
    assert!(script().runs.is_empty());
    clear_setup_run(chosen);
}
