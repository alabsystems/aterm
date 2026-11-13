// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::os::unix::ffi::OsStrExt as _;

fn build() -> (aterm_tempfile::TempDir, PathBuf, PathBuf) {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let build = scratch.path().join("store/tool/1");
    std::fs::create_dir_all(build.join("bin")).unwrap();
    let exe = build.join("bin/tool");
    std::fs::write(&exe, b"inert owned data, never executed").unwrap();
    (scratch, build, exe)
}

#[test]
fn permanent_unproved_starts_survive_age_and_boot_changes_until_their_own_proof() {
    let old = Some(std::time::UNIX_EPOCH);
    assert!(
        !age_holds("", old, None),
        "the ordinary blank mark can expire"
    );
    for awake in [Some(0), Some(u64::MAX), None] {
        assert!(age_holds(PERMANENT_STAMP, old, awake));
    }
    let (_scratch, build, exe) = build();
    let first = hold_first_exec_at(&exe, Some(1000)).unwrap();
    let second = hold_first_exec_at(&exe, Some(1000)).unwrap();
    std::fs::write(first.dir.join(STAMP), PERMANENT_STAMP).unwrap();
    std::fs::write(second.dir.join(STAMP), PERMANENT_STAMP).unwrap();
    let second_dir = second.dir.clone();
    for awake in [Some(0), Some(u64::MAX), None] {
        assert!(held_unproven_at(&build, awake));
        assert!(first_exec_removal(&build).is_err());
        assert!(first_exec_program_removal(build.parent().unwrap()).is_err());
    }
    first.ran();
    assert!(second_dir.join(STAMP).is_file());
    assert!(held_unproven_at(&build, Some(u64::MAX)));
    second.ran();
    assert!(!held_unproven_at(&build, Some(u64::MAX)));
    drop(first_exec_removal(&build).unwrap());
}

#[test]
fn a_program_removal_fences_new_builds_and_every_aside_alias() {
    let (_scratch, build, exe) = build();
    let program = build.parent().unwrap();
    let aside = aterm_toolchain::layout::aside_of(&build).unwrap();
    std::fs::rename(&build, &aside).unwrap();
    std::os::unix::fs::symlink(&aside, &build).unwrap();
    assert_eq!(
        key(&build).unwrap(),
        key(&aside).unwrap(),
        "one ordinary/aside key"
    );

    let held = hold_first_exec(&exe).unwrap();
    assert!(first_exec_removal(&build).is_err());
    assert!(first_exec_removal(&aside).is_err());
    assert!(first_exec_program_removal(program).is_err());
    held.ran();

    let removing = first_exec_program_removal(program).unwrap();
    let later = program.join("2");
    std::fs::create_dir_all(later.join("bin")).unwrap();
    let later_exe = later.join("bin/tool");
    std::fs::write(&later_exe, b"new inert build").unwrap();
    assert!(
        matches!(hold_first_exec(&later_exe),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock),
        "a build made after enumeration cannot start during whole-program deletion"
    );
    assert!(matches!(hold_first_exec(&exe),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock));
    drop(removing);
    let held = hold_first_exec(&later_exe).unwrap();
    assert!(held_unproven(&later));
    held.ran();
    assert!(!held_unproven(&later));
}

#[test]
fn a_replaced_mark_and_unknown_program_inventory_never_become_empty() {
    let (_scratch, build, exe) = build();
    let hold = hold_first_exec_at(&exe, Some(1_000)).unwrap();
    let stamp = hold.dir.join(STAMP);
    std::fs::rename(&stamp, hold.dir.join("older")).unwrap();
    std::fs::write(&stamp, b"1000").unwrap();
    hold.ran();
    assert!(held_unproven_at(&build, Some(1_000)));
    assert!(
        held_unproven_at(&build, Some(u64::MAX)),
        "unknown extra data is not aged away as an ordinary owned mark"
    );

    let unknown = build.parent().unwrap().join("not-a-directory");
    std::fs::write(&unknown, b"unknown inventory").unwrap();
    assert_eq!(held_build_in(&unknown), Some(unknown.clone()));
    let incomplete = held_build_among(
        build.parent().unwrap(),
        Ok(vec![Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unread entry",
        ))]
        .into_iter()),
    );
    assert!(incomplete.is_some());
}

#[test]
fn legacy_scalar_reads_are_bounded_and_special_files_keep_the_build() {
    let (_scratch, build, _) = build();
    let mark = sidecar_path(&build, UNPROVEN_SUFFIX).unwrap();
    std::fs::write(&mark, b"1000").unwrap();
    assert!(legacy_holds(&mark, Some(1_000)));
    let spent = u64::try_from(aterm_uds::deadline::FIRST_EXEC_HANG.as_millis()).unwrap();
    assert!(!legacy_holds(&mark, Some(1_000 + spent)));
    assert!(
        !legacy_holds(&mark, Some(999)),
        "restart ends the prior assessment"
    );
    std::fs::write(&mark, vec![b'0'; STAMP_BYTES + 1]).unwrap();
    assert!(legacy_holds(&mark, Some(1_000)));
    std::fs::remove_file(&mark).unwrap();
    let path = std::ffi::CString::new(mark.as_os_str().as_bytes()).unwrap();
    // SAFETY: a NUL-terminated owned scratch name; no pointer is retained.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    assert!(
        legacy_holds(&mark, Some(1_000)),
        "FIFO opens nonblocking and refuses as nonregular"
    );
}

#[test]
fn invalid_stamps_never_establish_expiry_or_a_restart() {
    let old = Some(std::time::UNIX_EPOCH);
    assert!(
        !age_holds("", old, None),
        "the intentional blank stamp uses mtime"
    );
    assert!(
        !age_holds("1000", old, Some(999)),
        "a valid prior boot stamp expires"
    );
    let spent = u64::try_from(aterm_uds::deadline::FIRST_EXEC_HANG.as_millis()).unwrap();
    assert!(age_holds("1000", old, Some(1000)));
    assert!(!age_holds("1000", old, Some(1000 + spent)));
    assert!(age_holds("0", old, Some(0)));
    assert!(!age_holds("0", old, Some(spent)));
    for malformed in [
        "garbage",
        "+1000",
        "01000",
        "18446744073709551616",
        "-1",
        " \n",
    ] {
        for awake in [Some(0), Some(u64::MAX), None] {
            assert!(age_holds(malformed, old, awake), "{malformed:?}, {awake:?}");
        }
    }
    let (_scratch, build, exe) = build();
    let hold = hold_first_exec_at(&exe, Some(1000)).unwrap();
    for malformed in ["garbage", "+1000", "01000", "18446744073709551616"] {
        std::fs::write(hold.dir.join(STAMP), malformed).unwrap();
        assert!(
            held_unproven_at(&build, Some(0)),
            "{malformed}: not a prior boot"
        );
        assert!(
            held_unproven_at(&build, Some(u64::MAX)),
            "{malformed}: not expired"
        );
    }
    hold.ran();
    let legacy = sidecar_path(&build, UNPROVEN_SUFFIX).unwrap();
    for malformed in ["garbage", "+1000", "01000", "18446744073709551616"] {
        std::fs::write(&legacy, malformed).unwrap();
        assert!(held_unproven_at(&build, Some(0)), "legacy {malformed}");
    }
}

#[test]
fn generic_removal_checks_the_actual_tree_and_orders_new_registration() {
    let (_scratch, build, exe) = build();
    let held_a = hold_first_exec(&exe).unwrap();
    let other = build.with_file_name("2.incoming-4242");
    let other_exe = other.join("bin/tool");
    std::fs::create_dir_all(other_exe.parent().unwrap()).unwrap();
    std::fs::write(&other_exe, b"inert unrelated scratch").unwrap();
    let held_b = hold_first_exec(&other_exe).unwrap();
    assert!(
        super::super::remove_tree(&other).is_err(),
        "check scratch's mark, not build 2"
    );
    assert!(other_exe.is_file());
    held_b.ran();
    let mut registration_refused = false;
    super::super::remove_tree_after_admission(&other, || {
        registration_refused = matches!(hold_first_exec(&other_exe),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock);
    })
    .unwrap();
    assert!(
        registration_refused,
        "a starter cannot slip between check and delete"
    );
    assert!(!other.exists(), "unused B is reclaimed while A is held");
    assert!(held_unproven(&build));
    held_a.ran();

    let gate = first_exec_removal(&build).unwrap();
    let foreign = build.parent().unwrap().with_file_name("foreign").join("1");
    std::fs::create_dir_all(&foreign).unwrap();
    assert!(super::super::remove_tree_under_gate(&foreign, &gate).is_err());
    assert!(
        foreign.is_dir(),
        "another program's gate cannot admit this tree"
    );
}

#[test]
fn recovery_keeps_a_held_superseded_tree_and_restores_an_unheld_one() {
    let (_scratch, build, exe) = build();
    let held_a = hold_first_exec(&exe).unwrap();
    let missing = build.with_file_name("2");
    let superseded = missing.with_file_name("2.superseded-4242");
    let old_exe = superseded.join("bin/tool");
    std::fs::create_dir_all(old_exe.parent().unwrap()).unwrap();
    std::fs::write(&old_exe, b"inert parked tree").unwrap();
    let held_old = hold_first_exec(&old_exe).unwrap();
    assert!(!super::super::recover_interrupted_swap(&missing));
    assert!(old_exe.is_file() && !missing.exists());
    held_old.ran();
    assert!(super::super::recover_interrupted_swap(&missing));
    assert!(missing.join("bin/tool").is_file() && !superseded.exists());
    assert!(
        held_unproven(&build),
        "unrelated A remains held during B's recovery"
    );
    held_a.ran();
}
