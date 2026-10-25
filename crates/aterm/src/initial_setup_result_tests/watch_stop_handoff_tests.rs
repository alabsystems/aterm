// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_cli::macos_doctor::ProvisionalSetupWatchTest;

struct ScratchUpgrade<'a, 'root> {
    setup: &'a mut ProvisionalSetupWatchTest<'root>,
    fault: Option<(usize, bool)>,
    after: fn(&ProvisionalSetupWatchTest<'_>),
    unread: bool,
    lose_publication: Option<(usize, bool)>,
}

impl WatchUpgrade for ScratchUpgrade<'_, '_> {
    fn pending(&self) -> bool {
        self.setup.upgrade_pending()
    }
    fn widens_from(&self) -> Option<u64> {
        self.setup.widens_from()
    }
    fn revision(&self) -> Result<u64, String> {
        self.setup
            .snapshot()
            .map(|s| s.revision)
            .map_err(|e| e.to_string())
    }
    fn persist(&mut self) -> Result<Option<aterm_cli::macos_doctor::SetupUpgrade>, String> {
        let result = if let Some((at, published)) = self.lose_publication.take() {
            self.setup.lose_upgrade_publication_at(at, published)
        } else {
            self.setup.upgrade_outcome(self.fault.take())
        };
        (self.after)(self.setup);
        result
    }
    fn published(&self) -> Result<Option<aterm_cli::macos_doctor::SetupUpgrade>, String> {
        if self.unread {
            self.setup.published_upgrade_after_failed_read()
        } else {
            self.setup.published_upgrade()
        }
    }
    fn forget(&mut self) {
        self.setup.forget_upgrade();
    }
}

fn stop_after_publish(_: &ProvisionalSetupWatchTest<'_>) {
    assert!(stop_initial_setup());
    wait_until("the predecessor's Stop is durable", stop_worker_idle);
}

fn require_owed_after_publish(setup: &ProvisionalSetupWatchTest<'_>) {
    stop_after_publish(setup);
    assert!(
        !setup.snapshot().unwrap().finished,
        "the predecessor's late Stop does not hold this staged target"
    );
    assert!(
        !settle_setup_stop(Duration::ZERO),
        "the published upgrade still owes its exact target's Stop"
    );
}

#[test]
fn watch_stop_handoff_keeps_a_published_upgrade_owed_until_its_target_stop_is_saved() {
    let _globals = globals();
    for fault in [None, Some((3, false)), Some((3, true))] {
        let _io = TestIo::install();
        let root = aterm_tempfile::tempdir().unwrap();
        let mut setup = ProvisionalSetupWatchTest::new(root.path()).unwrap();
        let original = setup.snapshot().unwrap().revision;
        change(|script| script.stop_root = Some(root.path().to_owned()));
        // The predecessor has completed its backend pass, but its worker still owns
        // the UI slot and registration while publishing the result and mending.
        set_setup_run(Some(original));
        let busy = take_initial_setup_slot().unwrap();
        let mut widened = Widened::default();
        let looked = look_setup_upgrade_with(
            &mut widened,
            &mut ScratchUpgrade {
                setup: &mut setup,
                fault,
                after: require_owed_after_publish,
                unread: false,
                lose_publication: None,
            },
        );
        assert_eq!(looked.is_err(), fault.is_some());
        wait_until("the upgrade's own Stop is durable", || {
            script().stops.contains(&(original + 1))
        });
        assert!(settle_setup_stop(HANG));
        let stopped = setup.snapshot().unwrap();
        assert!(stopped.finished && !stopped.permissions_allowed);
        assert_eq!(stopped.revision, original + 1);
        assert!(script().runs.is_empty(), "no new stage was dispatched");
        drop(busy);
    }
}

#[test]
fn watch_stop_handoff_no_stop_and_unpublished_failures_leave_no_shutdown_debt() {
    let _globals = globals();
    for stop in [false, true] {
        let _io = TestIo::install();
        let root = aterm_tempfile::tempdir().unwrap();
        let mut setup = ProvisionalSetupWatchTest::new(root.path()).unwrap();
        let original = setup.snapshot().unwrap().revision;
        change(|script| script.stop_root = Some(root.path().to_owned()));
        set_setup_run(Some(original));
        let mut widened = Widened::default();
        let looked = look_setup_upgrade_with(
            &mut widened,
            &mut ScratchUpgrade {
                setup: &mut setup,
                fault: stop.then_some((0, false)),
                after: if stop { stop_after_publish } else { |_| {} },
                unread: false,
                lose_publication: None,
            },
        );
        assert_eq!(looked.is_err(), stop);
        assert!(settle_setup_stop(Duration::ZERO));
        let snapshot = setup.snapshot().unwrap();
        assert_eq!(snapshot.revision, original + u64::from(!stop));
        assert_eq!(script().stops, if stop { vec![original] } else { vec![] });
    }
}

#[test]
fn watch_stop_handoff_metadata_cannot_stop_a_newer_explicit_plan() {
    let _globals = globals();
    for replaced in [false, true] {
        let _io = TestIo::install();
        let root = aterm_tempfile::tempdir().unwrap();
        let mut setup = ProvisionalSetupWatchTest::new(root.path()).unwrap();
        let original = setup.snapshot().unwrap().revision;
        set_setup_run(Some(original));
        let mut widened = Widened::default();
        let target = look_setup_upgrade_with(
            &mut widened,
            &mut ScratchUpgrade {
                setup: &mut setup,
                fault: None,
                after: |_| {},
                unread: false,
                lose_publication: None,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(widened.to, target);
        assert!(stop_initial_setup());
        wait_until("the old Stop is saved", stop_worker_idle);
        if replaced {
            setup.choose().unwrap();
        }
        let current = setup.snapshot().unwrap().revision;
        assert_eq!(current, target + u64::from(replaced));
        let mut sent = StartupSent::default();
        sent.send();
        let claim = StartupClaim::claim().unwrap().unwrap();
        let prepared = PreparedSetup::Authorized {
            claim,
            revision: current,
        };
        sent.inherit_widened(&prepared, widened);
        run_prepared_setup(prepared, None, sent);
        assert_eq!(script().runs, [current]);
        assert_eq!(
            setup_stop_requested(current),
            !replaced,
            "only the exact upgrade inherits its earlier Stop"
        );
    }
}

#[test]
fn watch_stop_handoff_read_failures_keep_unknown_publication_debt_without_rebinding() {
    let _globals = globals();
    for lost in [false, true] {
        let _io = TestIo::install();
        let root = aterm_tempfile::tempdir().unwrap();
        let mut setup = ProvisionalSetupWatchTest::new(root.path()).unwrap();
        let original = setup.snapshot().unwrap().revision;
        set_setup_run(Some(original));
        change(|script| script.stop_root = Some(root.path().to_owned()));
        let mut widened = Widened::default();
        let mut port = ScratchUpgrade {
            setup: &mut setup,
            fault: (!lost).then_some((3, false)),
            after: stop_after_publish,
            unread: !lost,
            lose_publication: lost.then_some((0, true)),
        };
        assert!(look_setup_upgrade_with(&mut widened, &mut port).is_err());
        assert!(port.setup.upgrade_pending());
        assert!(
            !settle_setup_stop(Duration::ZERO),
            "an unread target is not an unwritten one"
        );
        let before = port.setup.snapshot().unwrap().observation;
        port.unread = false;
        let retried = look_setup_upgrade_with(&mut widened, &mut port);
        if lost {
            assert!(retried.is_err());
            assert_eq!(
                port.setup.snapshot().unwrap().observation,
                before,
                "lost publication ownership never authorizes another write"
            );
            assert!(!settle_setup_stop(Duration::ZERO));
            assert_eq!(script().stops, [original]);
        } else {
            wait_until("the repaired target Stop is saved", || {
                script().stops.contains(&(original + 1))
            });
            assert!(settle_setup_stop(HANG));
            assert!(port.setup.snapshot().unwrap().finished);
        }
    }
}

#[test]
fn watch_stop_handoff_late_identity_loss_still_saves_its_reserved_target() {
    let _globals = globals();
    // A successful atomic checkpoint already reserved the target before physical
    // progress, or physical progress returned Ok before the final save failed.
    for fault in [(1, false), (3, false), (3, true)] {
        let _io = TestIo::install();
        let root = aterm_tempfile::tempdir().unwrap();
        let mut setup = ProvisionalSetupWatchTest::new(root.path()).unwrap();
        let original = setup.snapshot().unwrap().revision;
        change(|script| script.stop_root = Some(root.path().to_owned()));
        set_setup_run(Some(original));
        let busy = take_initial_setup_slot().unwrap();
        let mut widened = Widened::default();
        assert!(
            look_setup_upgrade_with(
                &mut widened,
                &mut ScratchUpgrade {
                    setup: &mut setup,
                    fault: None,
                    after: require_owed_after_publish,
                    unread: false,
                    lose_publication: Some(fault),
                },
            )
            .is_err()
        );
        wait_until("the exact Stop queue finishes", stop_worker_idle);
        assert_eq!(
            script().stops,
            [original, original + 1],
            "losing write ownership does not erase an already reserved Stop target"
        );
        assert!(settle_setup_stop(Duration::ZERO));
        let stopped = setup.snapshot().unwrap();
        assert_eq!(stopped.revision, original + 1);
        assert!(stopped.finished && !stopped.permissions_allowed);
        assert!(
            setup.recover().is_err(),
            "relaunch cannot resume the target"
        );
        let before = setup.snapshot().unwrap().observation;
        assert!(setup.upgrade_outcome(None).is_err());
        assert_eq!(setup.snapshot().unwrap().observation, before);
        assert!(script().runs.is_empty());
        drop(busy);
    }
}

#[test]
fn watch_stop_handoff_late_identity_loss_never_targets_a_newer_explicit_choice() {
    let _globals = globals();
    let _io = TestIo::install();
    let root = aterm_tempfile::tempdir().unwrap();
    let mut setup = ProvisionalSetupWatchTest::new(root.path()).unwrap();
    let original = setup.snapshot().unwrap().revision;
    change(|script| script.stop_root = Some(root.path().to_owned()));
    set_setup_run(Some(original));
    let mut widened = Widened::default();
    assert!(
        look_setup_upgrade_with(
            &mut widened,
            &mut ScratchUpgrade {
                setup: &mut setup,
                fault: None,
                after: |_| {},
                unread: false,
                lose_publication: Some((3, true)),
            },
        )
        .is_err()
    );
    let target = setup.snapshot().unwrap().revision;
    assert_eq!(target, original + 1);
    assert!(script().stops.is_empty());
    assert!(settle_setup_stop(Duration::ZERO));
    assert_eq!(setup.recover().unwrap(), target);
    assert!(stop_initial_setup());
    wait_until("the predecessor's Stop finishes", stop_worker_idle);
    let mut sent = StartupSent::default();
    sent.send();
    setup.choose().unwrap();
    let current = setup.snapshot().unwrap().revision;
    assert_eq!(current, target + 1);
    // The same ended writer can offer only its older Stop evidence after
    // replacement. It cannot adopt the current publication or admit again.
    assert!(
        look_setup_upgrade_with(
            &mut widened,
            &mut ScratchUpgrade {
                setup: &mut setup,
                fault: None,
                after: |_| {},
                unread: false,
                lose_publication: None,
            },
        )
        .is_err()
    );
    wait_until("the old evidence's Stop queue finishes", stop_worker_idle);
    let before = setup.snapshot().unwrap().observation;
    assert!(setup.upgrade_outcome(None).is_err());
    assert_eq!(setup.snapshot().unwrap().observation, before);
    assert!(script().stops.iter().all(|saved| *saved < current));
    let claim = StartupClaim::claim().unwrap().unwrap();
    let prepared = PreparedSetup::Authorized {
        claim,
        revision: current,
    };
    sent.inherit_widened(&prepared, widened);
    run_prepared_setup(prepared, None, sent);
    assert_eq!(script().runs, [current]);
    assert!(!setup_stop_requested(current));
    assert!(!setup.snapshot().unwrap().finished);
    assert!(settle_setup_stop(Duration::ZERO));
}

#[test]
fn watch_stop_handoff_an_older_requeue_never_replaces_the_current_targets_stop() {
    let _globals = globals();
    let _io = TestIo::install();
    let root = aterm_tempfile::tempdir().unwrap();
    let mut setup = ProvisionalSetupWatchTest::new(root.path()).unwrap();
    let original = setup.snapshot().unwrap().revision;
    let target = setup.upgrade_outcome(None).unwrap().unwrap().to;
    change(|script| {
        script.stop_root = Some(root.path().to_owned());
        script.stop_hold = true;
    });
    // An older save already runs when the exact current target is handed over.
    save_setup_stop(0);
    wait_until("the older writer starts", || stop_saves().saving == Some(0));
    set_setup_run(Some(target));
    assert!(stop_initial_setup());
    save_setup_stop(original);
    assert_eq!(stop_saves().wanted, Some(target));
    change(|script| script.stop_hold = false);
    wait_until("the current target is saved", stop_worker_idle);
    assert_eq!(script().stops, [0, target]);
    save_setup_stop(original);
    assert!(settle_setup_stop(Duration::ZERO));
    assert_eq!(script().stop_attempts.len(), 2);
    let snapshot = setup.snapshot().unwrap();
    assert_eq!(snapshot.revision, target);
    assert!(snapshot.finished && !snapshot.permissions_allowed);
}

fn refuse_the_first_target_save(setup: &ProvisionalSetupWatchTest<'_>) {
    stop_after_publish(setup);
    change(|script| script.stop_failures = 1);
}

fn assert_target_retry_is_owned(target: u64) {
    let saves = stop_saves();
    assert!(
        saves.wanted == Some(target)
            || saves.saving == Some(target)
            || saves.saved.is_some_and(|saved| saved >= target),
        "orderly shutdown must retry the failed confirmed target"
    );
}

#[test]
fn watch_stop_retry_settle_retries_the_failed_exact_target_before_registration() {
    let _globals = globals();
    let _io = TestIo::install();
    let root = aterm_tempfile::tempdir().unwrap();
    let mut setup = ProvisionalSetupWatchTest::new(root.path()).unwrap();
    let original = setup.snapshot().unwrap().revision;
    change(|script| script.stop_root = Some(root.path().to_owned()));
    set_setup_run(Some(original));
    let mut widened = Widened::default();
    let target = look_setup_upgrade_with(
        &mut widened,
        &mut ScratchUpgrade {
            setup: &mut setup,
            fault: None,
            after: refuse_the_first_target_save,
            unread: false,
            lose_publication: None,
        },
    )
    .unwrap()
    .unwrap();
    wait_until("the first exact-target save fails", stop_worker_idle);
    assert_eq!(script().stops, [original]);
    assert_eq!(script().stop_attempts.len(), 2);
    assert!(!setup.snapshot().unwrap().finished);
    let _ = settle_setup_stop(Duration::ZERO);
    assert_target_retry_is_owned(target);
    assert!(settle_setup_stop(HANG));
    assert_eq!(script().stops, [original, target]);
    assert!(setup.snapshot().unwrap().finished);
    assert!(script().runs.is_empty());
}

#[test]
fn watch_stop_retry_keeps_coverage_after_the_earlier_admission_retires() {
    let _globals = globals();
    let _io = TestIo::install();
    let root = aterm_tempfile::tempdir().unwrap();
    let mut setup = ProvisionalSetupWatchTest::new(root.path()).unwrap();
    let target = setup.upgrade_outcome(None).unwrap().unwrap();
    change(|script| {
        script.stop_root = Some(root.path().to_owned());
        script.stop_failures = 1;
    });
    let earlier = UnderWay::new(send_admission());
    assert!(
        !stop_initial_setup(),
        "the earlier admission has no registered run"
    );
    let watch = UnderWay::new(send_admission());
    assert!(!stop_covers(watch.0));
    watch.published(target.from, target.to);
    wait_until("the first inherited target save fails", stop_worker_idle);
    drop(earlier);
    drop(watch);
    assert!(script().stops.is_empty());
    assert!(!setup.snapshot().unwrap().finished);
    assert!(
        setup_stop_requested(target.to),
        "the handed Stop still gates its target after both admissions retire"
    );
    let _ = settle_setup_stop(Duration::ZERO);
    assert_target_retry_is_owned(target.to);
    assert!(settle_setup_stop(HANG));
    assert_eq!(script().stops, [target.to]);
    assert!(setup.snapshot().unwrap().finished);
}

#[test]
fn watch_stop_retry_a_queued_target_is_owed_even_without_a_worker() {
    let saves = StopSaves {
        wanted: Some(41),
        ..StopSaves::default()
    };
    assert!(
        saves.owed(0),
        "a failed worker spawn has not saved its queued Stop"
    );
}
