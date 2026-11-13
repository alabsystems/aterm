// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn previews_do_not_reserve_and_legacy_recovery_enters_an_era_once() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path();
    assert_eq!(next(root, 8).unwrap(), 9);
    assert!(!root.join(RECORD).exists());
    recover(root, Some(8)).unwrap();
    assert_eq!(next(root, 0).unwrap(), LEGACY_RECOVERY + 1);
    reserve(root, LEGACY_RECOVERY + 1).unwrap();
    recover(root, None).unwrap();
    assert_eq!(next(root, 0).unwrap(), LEGACY_RECOVERY + 2);

    let existing = aterm_tempfile::tempdir().unwrap();
    reserve(existing.path(), 8).unwrap();
    recover(existing.path(), Some(12)).unwrap();
    assert_eq!(next(existing.path(), 0).unwrap(), 13);

    let readable = aterm_tempfile::tempdir().unwrap();
    recover(readable.path(), Some(LEGACY_RECOVERY + 9)).unwrap();
    assert_eq!(next(readable.path(), 0).unwrap(), LEGACY_RECOVERY + 10);
}

#[test]
fn a_failed_progress_publication_keeps_its_reservation_and_owned_retry_target() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path();
    let _lock = HeldLock::take(root).unwrap();
    let plan = Plan::from(Selection::none());
    let proposed = Progress {
        revision: next(root, 0).unwrap(),
        ..Progress::for_plan(&plan)
    };
    let path = root.join("initial-setup.json");
    std::fs::create_dir(&path).unwrap();
    assert!(save_progress(root, proposed).is_err());
    assert_eq!(next(root, 0).unwrap(), proposed.revision + 1);
    std::fs::remove_dir(&path).unwrap();
    save_progress(root, proposed).unwrap();
    assert_eq!(progress_file(root).unwrap(), Some(proposed));
    assert_eq!(next(root, 0).unwrap(), proposed.revision + 1);
}

#[test]
fn exhausted_allocations_never_restart_from_zero() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path();
    reserve(root, u64::MAX).unwrap();
    assert!(next(root, 0).is_err());
    assert!(next(root, u64::MAX).is_err());
    assert!(reserved(root, u64::MAX).unwrap());
}

/// A counter that does not parse is rebuilt past every run number a record names, in the
/// recovery era, and the next reservation keeps it beside itself and writes it again: never
/// a wedge, never a number used again. One a newer aterm wrote is left as it is.
#[test]
fn a_damaged_run_counter_is_rebuilt_past_every_named_run_and_kept_aside() {
    for bytes in [
        &b"not json"[..],
        br#"{"profile":"other"}"#,
        br#"{"version":1}"#,
    ] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let doctor = scratch.path().join("doctor");
        std::fs::create_dir(&doctor).unwrap();
        let root = doctor.as_path();
        std::fs::write(root.join(RECORD), bytes).unwrap();
        assert_eq!(next(root, 0).unwrap(), LEGACY_RECOVERY + 1, "an empty Mac");
        write_json(
            &root.join(STOP_REQUEST),
            &json!({"profile": PROFILE, "revision": LEGACY_RECOVERY + 7}),
        )
        .unwrap();
        std::fs::write(
            scratch
                .path()
                .join(aterm_types::macos_setup::access_record::FILE),
            format!("{} full_disk_access granted\n", LEGACY_RECOVERY + 9),
        )
        .unwrap();
        assert_eq!(next(root, 0).unwrap(), LEGACY_RECOVERY + 10);
        assert!(damaged(root).unwrap());
        reserve(root, 3).unwrap();
        assert_eq!(
            std::fs::read(root.join("run-revision.json.unreadable")).unwrap(),
            bytes
        );
        assert!(!damaged(root).unwrap());
        assert_eq!(next(root, 0).unwrap(), LEGACY_RECOVERY + 10);
    }
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path();
    let newer =
        aterm_json::to_vec(&json!({"profile": PROFILE, "version": 2, "revision": 4})).unwrap();
    std::fs::write(root.join(RECORD), &newer).unwrap();
    assert!(next(root, 0).is_err_and(|why| setup_is_newer(&why)));
    assert!(reserve(root, 5).is_err());
    assert!(damaged(root).is_err());
    assert_eq!(std::fs::read(root.join(RECORD)).unwrap(), newer);
}

#[test]
fn an_upgrade_bridge_keeps_its_reserved_target_across_a_gap() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path();
    let _lock = HeldLock::take(root).unwrap();
    let mut selected = Selection::none();
    selected.set(Setting::Permissions, true);
    let previous = Plan::from(selected);
    let prior = Progress {
        revision: 1,
        admitted: true,
        ..Progress::for_plan(&previous)
    };
    save_plan_as(root, &previous, true).unwrap();
    save_progress(root, prior).unwrap();
    reserve(root, 9).unwrap();
    let full = Plan::default();
    let pending = Progress {
        revision: next(root, prior.revision).unwrap(),
        ..Progress::for_plan(&full)
    };
    scope::UpgradeProgress::new(Some(previous), Some(prior), pending)
        .save(root, &full)
        .unwrap();
    assert_eq!(progress(root).unwrap(), Some(pending));
    let reserved = std::fs::read(root.join(RECORD)).unwrap();
    assert_eq!(next(root, prior.revision).unwrap(), pending.revision + 1);
    assert_eq!(std::fs::read(root.join(RECORD)).unwrap(), reserved);

    std::fs::remove_file(root.join(RECORD)).unwrap();
    assert!(progress(root).is_err(), "a gap needs its reservation");
    reserve(root, pending.revision).unwrap();
    assert_eq!(progress(root).unwrap(), Some(pending));
    save_progress(root, pending).unwrap();
    assert_eq!(progress_file(root).unwrap(), Some(pending));
    assert!(!pending.admitted);
}
