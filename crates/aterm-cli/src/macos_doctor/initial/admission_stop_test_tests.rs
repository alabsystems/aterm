// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn admission_stop_target_survives_terminal_publication_capture_loss() {
    for restart in [false, true] {
        for fault in [
            InitialSetupAdmissionFault::BeforeFinalPublicationUnknown,
            InitialSetupAdmissionFault::FinalPublicationUnknown,
        ] {
            let root = aterm_tempfile::tempdir().unwrap();
            let mut owner = if restart {
                InitialSetupAdmissionStopTest::restart(root.path()).unwrap()
            } else {
                InitialSetupAdmissionStopTest::startup(root.path()).unwrap()
            };
            assert!(owner.persist(fault).is_err());
            assert!(!owner.pending());
            let revision = owner.snapshot().unwrap().revision;
            assert_eq!(owner.stop_target().unwrap(), Some(revision));
            let before = owner.snapshot().unwrap().observation;
            assert_eq!(
                owner.persist(InitialSetupAdmissionFault::None).unwrap(),
                None
            );
            assert_eq!(owner.snapshot().unwrap().observation, before);
            let replacement = owner.choose().unwrap();
            assert!(replacement > revision);
            owner.stop_revision(revision).unwrap();
            assert!(!owner.snapshot().unwrap().finished);
            assert_eq!(owner.recover().unwrap(), replacement);
        }
    }
}

#[test]
fn admission_stop_before_a_confirmed_progress_never_guesses_a_target() {
    for fault in [
        InitialSetupAdmissionFault::PlanPublicationUnknown,
        InitialSetupAdmissionFault::ProgressPublicationUnknown,
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut owner = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
        assert!(owner.persist(fault).is_err());
        assert!(!owner.pending());
        assert!(owner.stop_target().is_err());
        let revision = owner.choose().unwrap();
        if fault == InitialSetupAdmissionFault::PlanPublicationUnknown {
            assert_eq!(
                revision, 1,
                "a later choice can use the unreserved next revision"
            );
        }
        assert!(owner.stop_target().is_err());
        let before = owner.snapshot().unwrap().observation;
        assert_eq!(
            owner.persist(InitialSetupAdmissionFault::None).unwrap(),
            None
        );
        assert_eq!(owner.snapshot().unwrap().observation, before);
    }
}

#[test]
fn admission_stop_knows_an_ordinary_failed_final_save_without_admitting_again() {
    for restart in [false, true] {
        for fault in [
            InitialSetupAdmissionFault::BeforeFinal,
            InitialSetupAdmissionFault::AfterFinal,
        ] {
            let root = aterm_tempfile::tempdir().unwrap();
            let mut owner = if restart {
                InitialSetupAdmissionStopTest::restart(root.path()).unwrap()
            } else {
                InitialSetupAdmissionStopTest::startup(root.path()).unwrap()
            };
            assert!(owner.persist(fault).is_err());
            assert!(owner.pending());
            let revision = owner.snapshot().unwrap().revision;
            assert_eq!(owner.stop_target().unwrap(), Some(revision));
            owner.stop_revision(revision).unwrap();
            let before = owner.snapshot().unwrap().observation;
            assert!(owner.recover().is_err());
            assert_eq!(owner.snapshot().unwrap().observation, before);
        }
    }
}

#[test]
fn admission_stop_of_a_staged_target_holds_a_fresh_startup_without_reauthorization() {
    for (stopped, empty) in [(false, false), (true, false), (true, true)] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut owner = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
        if empty {
            owner.plan.selection = Selection::none();
        }
        assert!(
            owner
                .persist(InitialSetupAdmissionFault::BeforeFinalPublicationUnknown)
                .is_err()
        );
        let target = owner.snapshot().unwrap().revision;
        if stopped {
            owner.stop_revision(target).unwrap();
            let before = owner.snapshot().unwrap().observation;
            assert!(
                owner.recover().is_err(),
                "a stopped staged run was admitted again"
            );
            let snapshot = owner.snapshot().unwrap();
            assert!(snapshot.finished && !snapshot.permissions_allowed);
            assert_eq!(snapshot.revision, target);
            assert_eq!(snapshot.observation, before);
        } else {
            assert!(
                owner.recover().unwrap() > target,
                "unstopped staging remains recoverable"
            );
        }
    }
}

#[test]
fn admission_stop_confirms_only_the_owned_pinned_failed_progress_publication() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut owner = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
    assert!(
        owner
            .persist(InitialSetupAdmissionFault::ProgressAfter)
            .is_err()
    );
    assert!(owner.pending());
    let target = owner.snapshot().unwrap().revision;
    assert_eq!(owner.stop_target().unwrap(), Some(target));
    let newer = owner.choose().unwrap();
    assert!(newer > target);
    let before = owner.snapshot().unwrap().observation;
    assert_eq!(
        owner.persist(InitialSetupAdmissionFault::None).unwrap(),
        None
    );
    assert_eq!(owner.stop_target().unwrap(), Some(target));
    assert_eq!(owner.snapshot().unwrap().observation, before);
}

#[test]
fn admission_stop_restart_cleanup_failure_keeps_its_successfully_staged_target() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut owner = InitialSetupAdmissionStopTest::restart(root.path()).unwrap();
    assert!(
        owner
            .persist(InitialSetupAdmissionFault::StagedCleanup)
            .is_err()
    );
    assert!(!owner.pending());
    let target = owner.snapshot().unwrap().revision;
    assert_eq!(owner.stop_target().unwrap(), Some(target));
    owner.stop_revision(target).unwrap();
    let before = owner.snapshot().unwrap().observation;
    assert!(owner.recover().is_err());
    assert!(owner.snapshot().unwrap().finished);
    assert_eq!(
        owner.persist(InitialSetupAdmissionFault::None).unwrap(),
        None
    );
    assert_eq!(owner.snapshot().unwrap().observation, before);
}
