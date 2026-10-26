// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_cli::macos_doctor::InitialSetupWithheldStopTest;
use aterm_types::macos_setup::{Access, AccessOutcome};

/// Keep a failed assertion from leaving the shared Stop saver held, or letting
/// its scratch state disappear before the queued write has ended.
struct ReleaseStop;

impl Drop for ReleaseStop {
    fn drop(&mut self) {
        change(|script| script.stop_hold = false);
        wait_until("the held Stop save ends", stop_worker_idle);
    }
}

fn hold_stop_save(root: &Path, revision: u64) -> ReleaseStop {
    change(|script| {
        script.stop_root = Some(root.to_owned());
        script.stop_hold = true;
    });
    register_admitted_run(revision, send_admission(), false, None);
    ReleaseStop
}

fn stop_before_follow_up_effect(root: &Path, revision: u64) {
    assert!(INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire));
    assert!(stop_initial_setup());
    wait_until("the Stop saver reaches its held write", || {
        script()
            .stop_attempts
            .iter()
            .any(|(target, _)| *target == revision)
    });
    assert!(setup_stop_requested(revision));
    assert!(
        script().stops.is_empty(),
        "the durable Stop is still delayed"
    );
    assert!(!root.join("stop-request.json").exists());
}

/// The grant schedules a worker independently of the parked setup run. Stop
/// must reach that worker even when its durable write has not landed yet.
#[test]
fn withheld_follow_up_honors_a_local_stop_before_its_durable_save() {
    let _globals = globals();
    let _io = TestIo::install();
    let root = aterm_tempfile::tempdir().unwrap();
    let mut setup = InitialSetupWithheldStopTest::unfinished(root.path()).unwrap();
    let revision = setup.revision().unwrap();
    let before = snapshot(root.path());
    assert!(!before.finished);
    assert!(before.run_allowed);
    assert!(setup.owed().unwrap());
    assert_eq!(setup.notification_center_writes(), 0);
    let _release = hold_stop_save(root.path(), revision);

    access_fact_observed(Access::FullDiskAccess, AccessOutcome::Granted);
    assert_eq!(script().follow_ups, 1, "the grant queues its follow-up");
    let (observed, reservation) = finish_withheld_preferences_with(|reserve, stopped| {
        setup.follow_up(
            || {
                let reserved = reserve();
                stop_before_follow_up_effect(root.path(), revision);
                reserved
            },
            stopped,
        )
    });
    observed.result.unwrap();
    assert_eq!(
        setup.notification_center_writes(),
        0,
        "a separate follow-up cannot cross this process's Stop"
    );
    assert!(setup.owed().unwrap(), "the clear remains owed");
    assert!(!root.path().join("stop-request.json").exists());
    drop(reservation);
}

/// An ordinary grant finishes its owed clear once, and a later accepted run
/// keeps its own authority even while an earlier run's Stop save is delayed.
#[test]
fn withheld_follow_up_allows_an_unstopped_or_newly_admitted_run() {
    for earlier_stop in [false, true] {
        let _globals = globals();
        let _io = TestIo::install();
        let root = aterm_tempfile::tempdir().unwrap();
        let mut setup = InitialSetupWithheldStopTest::unfinished(root.path()).unwrap();
        let revision = setup.revision().unwrap();
        let _release = hold_stop_save(root.path(), revision);
        if earlier_stop {
            assert!(stop_initial_setup());
            wait_until("the old Stop saver reaches its held write", || {
                !script().stop_attempts.is_empty()
            });
            let next = setup.choose().unwrap();
            assert!(next > revision);
            register_admitted_run(next, send_admission(), false, None);
            assert!(setup_stop_requested(revision));
            assert!(!setup_stop_requested(next));
        }

        access_fact_observed(Access::FullDiskAccess, AccessOutcome::Granted);
        assert_eq!(script().follow_ups, 1);
        let (observed, reservation) =
            finish_withheld_preferences_with(|reserve, stopped| setup.follow_up(reserve, stopped));
        assert!(observed.result.unwrap().is_some());
        assert_eq!(setup.notification_center_writes(), 1);
        assert!(!setup.owed().unwrap());
        drop(reservation);
        let (observed, reservation) =
            finish_withheld_preferences_with(|reserve, stopped| setup.follow_up(reserve, stopped));
        assert!(observed.result.unwrap().is_none());
        assert!(reservation.is_none(), "nothing owed takes no slot");
        assert_eq!(setup.notification_center_writes(), 1);
    }
}

/// Stop withdraws new setup effects; putting back a previous Restore's
/// originals after a verified grant is independent recovery, and still runs.
#[test]
fn withheld_follow_up_still_finishes_a_waiting_restore_under_local_stop() {
    let _globals = globals();
    let _io = TestIo::install();
    let root = aterm_tempfile::tempdir().unwrap();
    let mut setup = InitialSetupWithheldStopTest::waiting_restore(root.path()).unwrap();
    let revision = setup.revision().unwrap();
    let before = setup.notification_center_writes();
    assert!(setup.owed().unwrap());
    let _release = hold_stop_save(root.path(), revision);

    access_fact_observed(Access::FullDiskAccess, AccessOutcome::Granted);
    assert_eq!(script().follow_ups, 1);
    let (observed, reservation) = finish_withheld_preferences_with(|reserve, stopped| {
        setup.follow_up(
            || {
                let reserved = reserve();
                stop_before_follow_up_effect(root.path(), revision);
                reserved
            },
            stopped,
        )
    });
    let report = observed.result.unwrap().expect("the waiting Restore runs");
    assert!(report.restore.is_some());
    assert_eq!(setup.notification_center_writes(), before + 1);
    assert!(!setup.owed().unwrap());
    assert!(setup_stop_requested(revision));
    assert!(!root.path().join("stop-request.json").exists());
    drop(reservation);
}
