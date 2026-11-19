// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn retirement_requires_stop_owning_reap_and_exact_deletion_before_releasing_slots() {
    let mut actions = Vec::new();
    retire_with(true, false, |action| {
        actions.push(action);
        Ok(())
    })
    .unwrap();
    assert_eq!(
        actions,
        [
            Action::Stop,
            Action::Join,
            Action::Delete,
            Action::ReleaseSlots
        ]
    );

    // Every failure retains the same allocation and slots. A stop request alone, ECHILD,
    // owning-wait timeout, and failed deletion are each insufficient closure evidence.
    for (failed, why) in [
        (Action::Stop, "stop refused"),
        (Action::Join, "ECHILD"),
        (Action::Join, "owning wait timed out"),
        (Action::Delete, "exact delete failed"),
    ] {
        let mut actions = Vec::new();
        let mut slots_held = true;
        let result = retire_with(true, false, |action| {
            actions.push(action);
            if action == failed {
                return Err(why.to_owned());
            }
            if action == Action::ReleaseSlots {
                slots_held = false;
            }
            Ok(())
        });
        assert_eq!(result.unwrap_err(), why);
        assert!(slots_held, "{why}");
        assert!(!actions.contains(&Action::ReleaseSlots));
        if failed != Action::Delete {
            assert!(!actions.contains(&Action::Delete));
        }
        if failed == Action::Stop {
            assert!(!actions.contains(&Action::Join));
        }
    }
}

#[test]
fn a_never_booted_clone_is_deleted_exactly_and_a_requested_kept_clone_is_stopped() {
    for (running, keep, expected) in [
        (false, false, vec![Action::Delete, Action::ReleaseSlots]),
        (
            true,
            true,
            vec![
                Action::Stop,
                Action::Join,
                Action::Keep,
                Action::ReleaseSlots,
            ],
        ),
    ] {
        let mut actions = Vec::new();
        retire_with(running, keep, |action| {
            actions.push(action);
            Ok(())
        })
        .unwrap();
        assert_eq!(actions, expected);
    }
    let mut actions = Vec::new();
    assert!(
        retire_with(true, true, |action| {
            actions.push(action);
            if action == Action::Keep {
                Err("rename refused".into())
            } else {
                Ok(())
            }
        })
        .is_err()
    );
    assert_eq!(actions, [Action::Stop, Action::Join, Action::Keep]);
}

#[test]
fn observations_are_durable_private_data_and_completion_is_explicit() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().join("owned.vm.json");
    let mut journal = Journal::new(path.clone(), json!({"name":"owned-allocation"})).unwrap();
    let read = || aterm_json::from_slice::<Value>(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(read()["complete"].as_bool(), Some(false));
    journal.event("stop", json!({"exit":0})).unwrap();
    assert_eq!(
        read()["complete"].as_bool(),
        Some(false),
        "stop success is not a retirement receipt"
    );
    journal
        .event("owning-reap", json!({"reaped":true}))
        .unwrap();
    assert_eq!(
        read()["complete"].as_bool(),
        Some(false),
        "reap alone does not confirm exact deletion"
    );
    journal.event("delete", json!({"exit":0})).unwrap();
    journal.complete("deleted").unwrap();
    assert_eq!(read(), journal.value());
    assert_eq!(read()["complete"].as_bool(), Some(true));
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(
        Journal::new(path, json!({"name":"another"})).is_err(),
        "existing receipts are preserved"
    );
}

#[test]
fn an_unpublished_intent_performs_no_operation_or_slot_release() {
    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().join("owned.vm.json");
    let mut journal = Journal::new(path.clone(), json!({"name":"owned-allocation"})).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let mut actual_operations = Vec::new();
    let result = retire_with(true, false, |action| {
        journal.event(action.name(), json!({"phase":"armed"}))?;
        actual_operations.push(action);
        Ok(())
    });
    assert!(result.is_err());
    assert!(
        actual_operations.is_empty(),
        "no action can precede its durable intent"
    );
}

#[test]
fn owning_wait_errors_and_no_progress_are_not_reap_receipts() {
    use std::os::unix::process::ExitStatusExt as _;
    use std::time::Duration;
    let now = std::time::Instant::now();
    let error = wait_for_reap_with(
        Duration::ZERO,
        || Err(std::io::Error::from_raw_os_error(libc::ECHILD)),
        || now,
        || panic!("an owning wait error needs no progress inference"),
        |_| panic!("an owning wait error needs no pause"),
    )
    .unwrap_err();
    assert!(error.contains("owning wait"));
    assert!(!error.contains("hang detector"));
    assert!(
        wait_for_reap_with(
            Duration::ZERO,
            || Ok(None),
            || now,
            || Some(0),
            |_| panic!("a closed no-progress window needs no pause"),
        )
        .unwrap_err()
        .contains("hang detector")
    );
    // A positive owning terminal status is closure even when the status itself was nonzero.
    let terminal = std::process::ExitStatus::from_raw(9);
    assert_eq!(
        wait_for_reap_with(
            Duration::ZERO,
            || Ok(Some(terminal)),
            || now,
            || panic!("a positive owning reap needs no progress inference"),
            |_| panic!("a positive owning reap needs no pause"),
        )
        .unwrap(),
        terminal
    );
}

#[test]
fn an_owning_reap_wait_follows_launcher_progress_and_rechecks_completion() {
    use std::cell::Cell;
    use std::os::unix::process::ExitStatusExt as _;
    use std::time::{Duration, Instant};

    let terminal = std::process::ExitStatus::from_raw(0);
    // A loaded scheduler can stretch each observation by many windows. Changed CPU/I/O
    // generations and unread/incomparable readings each keep the actual owning wait open.
    for readings in [
        [Some(0), Some(1), Some(2), Some(3)],
        [Some(0), None, Some(0), Some(1)],
    ] {
        let step = Cell::new(0);
        let start = Instant::now();
        let status = wait_for_reap_with(
            Duration::from_secs(1),
            || Ok((step.get() == readings.len()).then_some(terminal)),
            || start + Duration::from_secs(u64::try_from(step.get()).unwrap() * 5),
            || readings[step.get()],
            |_| step.set(step.get() + 1),
        )
        .unwrap();
        assert_eq!(status, terminal);
        assert_eq!(step.get(), readings.len());
    }

    let step = Cell::new(0);
    let start = Instant::now();
    let observations = [(0, 0), (5_000, 1), (5_500, 1), (6_100, 1)];
    wait_for_reap_with(
        Duration::from_secs(1),
        || Ok(None),
        || start + Duration::from_millis(observations[step.get()].0),
        || Some(observations[step.get()].1),
        |_| step.set(step.get() + 1),
    )
    .unwrap_err();
    assert_eq!(
        step.get(),
        observations.len() - 1,
        "the quiet window starts at the last observed progress"
    );

    let start = Instant::now();
    let elapsed = Cell::new(Duration::ZERO);
    let polls = Cell::new(0);
    let status = wait_for_reap_with(
        Duration::from_secs(1),
        || {
            let poll = polls.get() + 1;
            polls.set(poll);
            Ok((poll == 3).then_some(terminal))
        },
        || start + elapsed.get(),
        || Some(0),
        |_| elapsed.set(Duration::from_secs(1)),
    )
    .unwrap();
    assert_eq!(
        status, terminal,
        "the final owning reap wins over a stale None"
    );
    assert_eq!(polls.get(), 3);
}

#[test]
fn incomplete_vm_scratch_is_retained_and_confirmed_scratch_is_removed() {
    let record = aterm_tempfile::tempdir().unwrap();
    let source = aterm_tempfile::tempdir().unwrap();
    let kept = source.path().to_owned();
    drop(Scratch::new(source, record.path()).unwrap());
    assert!(kept.is_dir());
    let receipt = std::fs::read_dir(record.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .next()
        .unwrap();
    let receipt: Value = aterm_json::from_slice(&std::fs::read(receipt).unwrap()).unwrap();
    assert_eq!(receipt["complete"].as_bool(), Some(false));
    assert_eq!(receipt["path"].as_str(), kept.to_str());
    // This retained fixture contains no executable or process. Remove exactly its owned dir.
    std::fs::remove_dir(&kept).unwrap();
    let source = aterm_tempfile::tempdir().unwrap();
    let removed = source.path().to_owned();
    let mut scratch = Scratch::new(source, record.path()).unwrap();
    scratch.discard_after_finish();
    drop(scratch);
    assert!(!removed.exists());
}

#[test]
fn failed_final_publication_does_not_supply_a_complete_receipt() {
    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().join("owned.vm.json");
    let mut journal = Journal::new(path.clone(), json!({"name":"owned-allocation"})).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(journal.complete("deleted").is_err());
    assert_eq!(journal.value()["complete"].as_bool(), Some(false));
    assert!(journal.value()["publication_error"].as_str().is_some());
}

#[test]
fn checked_cleanup_preserves_a_red_or_interrupt_and_does_not_hide_a_new_cleanup_failure() {
    for (work, expected) in [
        (crate::Fail::die("observed red"), 1),
        (crate::Fail::exit(3, "work unavailable"), 1),
        (
            crate::Fail {
                code: 130,
                stopped: Some(2),
                msg: None,
            },
            130,
        ),
    ] {
        let stopped = work.stopped;
        let error = after_cleanup::<()>(Err(work), || Err(crate::Fail::die("cleanup unconfirmed")))
            .unwrap_err();
        assert_eq!(error.code, expected);
        assert_eq!(error.stopped, stopped);
        assert!(error.msg.unwrap().contains("cleanup unconfirmed"));
    }
    assert!(after_cleanup(Ok(()), || Ok(())).is_ok());
    assert!(after_cleanup(Ok(()), || Err(crate::Fail::die("cleanup failed"))).is_err());
}

// A pure unstarted VM-state fixture: no Tart command is executed and no native VM exists.
// Its Drop clears the synthetic state before Vm::Drop, even if a regression panics a test.
struct UnstartedVm(crate::mac::Vm);

impl UnstartedVm {
    fn new(root: &Path) -> Self {
        let attempt = aterm_uds::spawnfd::BirthAttempt::new().unwrap();
        let owner = attempt.key().clone();
        let _not_started = attempt.cancel().unwrap();
        Self(crate::mac::Vm {
            tart: "/never-executed-vm-fixture".into(),
            name: "no-native-vm-created".into(),
            identity: crate::mac::VmIdentity::allocate().unwrap(),
            share: root.join("share"),
            log: root.join("never-created.log"),
            run: None,
            clone_pending: None,
            owner,
            allocation_owned: true,
            journal: Journal::new(
                root.join("state-fixture.vm.json"),
                json!({"native_allocation":false}),
            )
            .unwrap(),
            retirement_attempted: false,
            closed: true,
            blocked: None,
            keep: false,
            promoted: false,
            slot: None,
            display_slot: None,
            reservation: None,
            graphics: false,
            shape: None,
            configurations: 0,
        })
    }
}

impl Drop for UnstartedVm {
    fn drop(&mut self) {
        // Only this synthetic fixture's real ledger guard is released: it started no child/VM.
        self.0.closed = true;
        drop(self.0.reservation.take());
    }
}

fn original_admission(root: &Path) -> crate::vm_admit::Admitted {
    use aterm_capacity::{Admit, Fixed, Ledger, Need, Pace, Reading};
    let claim = aterm_capacity::admit_with(
        &Ledger::at(root.join("ledger")),
        &Fixed::new(Reading::of_gib(8, 6, 8)),
        Need::new("unstarted VM state fixture")
            .mem(2 << 20)
            .cores(1),
        Admit::Wait,
        Pace::default(),
        &mut |_| {},
    )
    .unwrap();
    crate::vm_admit::Admitted {
        jobs: claim.cores(),
        claim: Some(claim),
    }
}

#[test]
fn a_mistimed_admission_leaves_the_original_claim_and_ledger_bytes_unchanged() {
    use std::os::unix::fs::MetadataExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let mut vm = UnstartedVm::new(root.path());
    let mut admitted = original_admission(root.path());
    let original = admitted.claim.as_ref().unwrap();
    let path = original.path().to_owned();
    let inode = std::fs::metadata(&path).unwrap().ino();
    let record = original.record().clone();
    let bytes = std::fs::read(&path).unwrap();
    let jobs = admitted.jobs;
    assert!(vm.0.hold_admission(&mut admitted).is_err());
    let still_original = admitted
        .claim
        .as_ref()
        .expect("preflight must precede taking the original claim");
    assert_eq!(still_original.path(), path);
    assert_eq!(still_original.record(), &record);
    assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(admitted.jobs, jobs);
    assert!(vm.0.reservation.is_none());
}

#[test]
fn preboot_publication_failure_retains_the_actual_full_guard_and_blocks_boot() {
    use std::os::unix::fs::MetadataExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let mut vm = UnstartedVm::new(root.path());
    vm.0.closed = false;
    let mut admitted = original_admission(root.path());
    let original = admitted.claim.as_ref().unwrap();
    let path = original.path().to_owned();
    let inode = std::fs::metadata(&path).unwrap().ino();
    let need = original.record().need.clone();
    let held = original.record().held;
    let jobs = admitted.jobs;
    let receipt = vm.0.receipt_path().to_owned();
    std::fs::remove_file(&receipt).unwrap();
    std::fs::create_dir(&receipt).unwrap();
    assert!(vm.0.hold_admission(&mut admitted).is_err());
    assert!(
        admitted.claim.is_none(),
        "the actual converted owner moved to Vm"
    );
    let retained =
        vm.0.reservation
            .as_ref()
            .expect("fallible I/O cannot drop the original full guard");
    assert_eq!(retained.path(), path);
    assert_eq!(retained.metadata().unwrap().ino(), inode);
    assert_eq!(retained.record().need, need);
    assert_eq!(retained.record().held, held);
    assert_eq!(admitted.jobs, jobs);
    assert!(vm.0.blocked.is_some());
    assert!(vm.0.preflight_boot().is_err());
    assert!(vm.0.run.is_none());
    assert!(vm.0.clone_pending.is_none());
}

#[test]
fn configuration_and_shape_boot_preflight_before_native_output_or_counter_changes() {
    for state in [
        "closed",
        "blocked",
        "unknown allocation",
        "retirement attempted",
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut vm = UnstartedVm::new(root.path());
        vm.0.closed = false;
        match state {
            "closed" => vm.0.closed = true,
            "blocked" => vm.0.blocked = Some("custody already quarantined".into()),
            "unknown allocation" => vm.0.allocation_owned = false,
            _ => vm.0.retirement_attempted = true,
        }
        let shape = crate::mac::Shape {
            cpus: 1,
            memory_mib: 1,
            disk_gb: 60,
        };
        let receipt = vm.0.receipt();
        let receipt_bytes = std::fs::read(vm.0.receipt_path()).unwrap();
        assert!(vm.0.configure(shape).is_err(), "{state}");
        vm.0.shape = Some(shape);
        assert!(vm.0.boot().is_err(), "{state}");
        let mut command = aterm_uds::spawnfd::command("/never-executed-vm-fixture");
        assert!(
            vm.0.configuration_output("rejected", "before", &mut command)
                .is_err(),
            "{state}"
        );
        assert_eq!(
            vm.0.configurations, 0,
            "{state}: no attempted configuration"
        );
        assert_eq!(
            vm.0.receipt(),
            receipt,
            "{state}: no native intent or outcome"
        );
        assert_eq!(std::fs::read(vm.0.receipt_path()).unwrap(), receipt_bytes);
        assert_eq!(
            std::fs::read_dir(root.path()).unwrap().count(),
            1,
            "{state}: no output artifacts"
        );
        assert!(vm.0.run.is_none() && vm.0.clone_pending.is_none());
        assert!(vm.0.slot.is_none() && vm.0.display_slot.is_none());
    }
}

#[test]
fn one_original_reservation_is_shared_by_preparation_and_final_vm_without_readmission() {
    let ledger = aterm_tempfile::tempdir().unwrap();
    let prep_root = aterm_tempfile::tempdir().unwrap();
    let final_root = aterm_tempfile::tempdir().unwrap();
    let mut admitted = original_admission(ledger.path());
    let jobs = admitted.jobs;
    let original_path = admitted.claim.as_ref().unwrap().path().to_owned();
    let reservation = crate::mac::retain_admission(&mut admitted).unwrap();
    assert!(admitted.claim.is_none());
    assert_eq!(admitted.jobs, jobs);
    let metadata = reservation.metadata().unwrap();
    use std::os::unix::fs::MetadataExt as _;
    let identity = (metadata.dev(), metadata.ino());
    let record = reservation.record().clone();
    let mut prep = UnstartedVm::new(prep_root.path());
    let mut final_vm = UnstartedVm::new(final_root.path());
    // A mistimed attachment borrows the caller's guard: rejection never consumes it.
    assert!(prep.0.hold_reserved_admission(&reservation).is_err());
    assert_eq!(std::sync::Arc::strong_count(&reservation), 1);
    prep.0.closed = false;
    final_vm.0.closed = false;
    let shape = crate::mac::Shape {
        cpus: 1,
        memory_mib: 2,
        disk_gb: 60,
    };
    prep.0.shape = Some(shape);
    final_vm.0.shape = Some(shape);
    prep.0.hold_reserved_admission(&reservation).unwrap();
    final_vm.0.hold_reserved_admission(&reservation).unwrap();
    assert_eq!(std::sync::Arc::strong_count(&reservation), 3);
    assert!(std::sync::Arc::ptr_eq(
        prep.0.reservation.as_ref().unwrap(),
        &reservation
    ));
    assert!(std::sync::Arc::ptr_eq(
        final_vm.0.reservation.as_ref().unwrap(),
        &reservation
    ));
    assert!(
        prep.0.preflight_boot().is_ok(),
        "a retained reservation is allowed"
    );
    drop(reservation);
    drop(final_vm);
    let retained = prep.0.reservation.as_ref().unwrap();
    assert_eq!(std::sync::Arc::strong_count(retained), 1);
    assert_eq!(retained.path(), original_path);
    let metadata = retained.metadata().unwrap();
    assert_eq!((metadata.dev(), metadata.ino()), identity);
    assert_eq!(retained.record(), &record);
    // This is an unstarted fixture, so its RAII wrapper can release its genuine guard.
    // A real unknown preparation retains the same Arc with its owned child and slots.
}

#[test]
fn guarded_configuration_and_preparation_reject_shapes_outside_the_original_admission() {
    let ledger = aterm_tempfile::tempdir().unwrap();
    let mut admitted = original_admission(ledger.path());
    let reservation = crate::mac::retain_admission(&mut admitted).unwrap();
    let within = crate::mac::Shape {
        cpus: 1,
        memory_mib: 2,
        disk_gb: 60,
    };
    within.check_reservation(&reservation).unwrap();
    for shape in [
        crate::mac::Shape { cpus: 2, ..within },
        crate::mac::Shape {
            memory_mib: 3,
            ..within
        },
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut vm = UnstartedVm::new(root.path());
        vm.0.closed = false;
        vm.0.hold_reserved_admission(&reservation).unwrap();
        assert!(vm.0.configure(shape).is_err());
        assert_eq!(vm.0.configurations, 0);
        assert!(
            vm.0.blocked.is_none(),
            "known shape refusal preserves checked cleanup"
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
        assert!(
            crate::mac::prepared_base_with_admission(
                "/never-executed-vm-fixture",
                "no-native-image",
                &crate::mac::Prep {
                    shape: Some(shape),
                    ..crate::mac::Prep::default()
                },
                root.path(),
                root.path(),
                &reservation,
                &mut || panic!("no preparation action before admission validation"),
            )
            .is_err()
        );
    }
    let root = aterm_tempfile::tempdir().unwrap();
    assert!(
        crate::mac::prepared_base_with_admission(
            "/never-executed-vm-fixture",
            "no-native-image",
            &crate::mac::Prep::default(),
            root.path(),
            root.path(),
            &reservation,
            &mut || panic!("a supplied reservation cannot fall back to unadmitted preparation"),
        )
        .is_err()
    );
}

#[test]
fn a_shape_refusal_after_conversion_retains_the_original_guard_and_blocks_oversized_boot() {
    use std::os::unix::fs::MetadataExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let mut vm = UnstartedVm::new(root.path());
    vm.0.closed = false;
    let shape = crate::mac::Shape {
        cpus: 2,
        memory_mib: 2,
        disk_gb: 60,
    };
    vm.0.shape = Some(shape);
    let mut admitted = original_admission(root.path());
    let original = admitted.claim.as_ref().unwrap();
    let path = original.path().to_owned();
    let inode = std::fs::metadata(&path).unwrap().ino();
    let need = original.record().need.clone();
    let held = original.record().held;
    let jobs = admitted.jobs;
    assert!(vm.0.hold_admission(&mut admitted).is_err());
    assert!(
        admitted.claim.is_none(),
        "the original reservation was converted once"
    );
    let retained =
        vm.0.reservation
            .as_ref()
            .expect("shape refusal must retain the actual guard");
    assert_eq!(retained.path(), path);
    assert_eq!(retained.metadata().unwrap().ino(), inode);
    assert_eq!(retained.record().need, need);
    assert_eq!(retained.record().held, held);
    assert_eq!(admitted.jobs, jobs);
    assert!(
        vm.0.blocked.is_none(),
        "known budget refusal allows checked cleanup"
    );
    assert!(vm.0.configure(shape).is_err());
    assert!(vm.0.boot().is_err());
    assert_eq!(
        vm.0.configurations, 0,
        "neither get/set nor boot was attempted"
    );
    assert!(vm.0.run.is_none() && vm.0.clone_pending.is_none());
    assert!(!root.path().join("never-created.log").exists());
}
