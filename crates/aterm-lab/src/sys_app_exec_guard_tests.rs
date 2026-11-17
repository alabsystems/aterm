// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_toolchain::record_lock::RecordLock;
use std::os::fd::{AsFd as _, AsRawFd as _, OwnedFd};

/// The error of a native exec of the missing `program`, once the exec was tried. The exec
/// refuses (`WouldBlock` with no OS error, before it duplicates any guard) while this process
/// reads the process table, and another test of this binary may be reading it: on a loaded Mac
/// one read outlasts the second the gate waits, so the refusal is waited out, not judged.
fn tried_exec(program: &str, guards: &[std::os::fd::BorrowedFd<'_>]) -> std::io::Error {
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "this process's process-table reads settle, so the native exec is tried",
        || {
            let error = exec_as_app_with_guards(program, &[program.into()], &[], 1, guards);
            let refused =
                error.kind() == std::io::ErrorKind::WouldBlock && error.raw_os_error().is_none();
            (!refused).then_some(error)
        },
    )
}

#[test]
fn app_exec_refuses_process_reads_and_reopens_after_failure() {
    const CASE: &str =
        "sys::app_exec_guard_tests::app_exec_refuses_process_reads_and_reopens_after_failure";
    if !aterm_uds::test_binary::isolated_lifecycle(CASE) {
        return;
    }
    let shell = aterm_uds::deadline::stand_in(std::path::Path::new("/bin/sh"));
    let program = shell.to_str().unwrap();
    let argv = [
        program.into(),
        "-c".into(),
        "printf 'unguarded-native-app-exec\\n'; exit 37".into(),
    ];
    let file = std::fs::File::open("/dev/null").unwrap();
    let flags = || {
        // SAFETY: the owned file remains live across this read-only query.
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0);
        flags
    };
    let before = flags();
    let read = aterm_sysprobe::exec_gate::reading().expect("the helper's gate starts open");
    for guarded in [false, true] {
        let error = if guarded {
            exec_as_app_with_guards(program, &argv, &[], 1, &[file.as_fd()])
        } else {
            exec_as_app(program, &argv, &[], 1)
        };
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock, "{error}");
        assert_eq!(flags(), before);
        assert!(aterm_sysprobe::exec_gate::reading().is_some());
    }
    drop(read);

    let world = aterm_tempfile::tempdir().unwrap();
    let missing = world.path().join("missing-program");
    let program = missing.to_str().unwrap();
    for guarded in [false, true] {
        let error = if guarded {
            exec_as_app_with_guards(program, &[program.into()], &[], 1, &[file.as_fd()])
        } else {
            exec_as_app(program, &[program.into()], &[], 1)
        };
        assert_eq!(error.raw_os_error(), Some(libc::ENOENT), "{error}");
        assert_eq!(flags(), before);
        assert!(aterm_sysprobe::exec_gate::reading().is_some());
    }
    drop(world);
    let program = shell.to_str().unwrap();
    let script = format!(
        "test -r /dev/fd/{} || exit 38; printf 'native-app-exec-reopened\\n'; exit 0",
        file.as_raw_fd(),
    );
    let argv = [program.into(), "-c".into(), script];
    let error = exec_as_app_with_guards(program, &argv, &[], 1, &[file.as_fd()]);
    panic!("native app exec did not dispatch after reopening: {error}");
}

#[test]
fn failed_app_exec_preserves_original_guard_flags_and_closes_temporary_sources() {
    let world = aterm_tempfile::tempdir().unwrap();
    let path = world.path().join("window.lock");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    let original = RecordLock::try_lock(file, 0).unwrap().into_close_only();
    let guard: OwnedFd = original.try_clone_fd().unwrap();
    let flags = |fd| {
        // SAFETY: the test retains the owned descriptor across this query.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(flags >= 0);
        flags
    };
    let before = flags(guard.as_raw_fd());
    assert_ne!(before & libc::FD_CLOEXEC, 0);
    let missing = world.path().join("missing-program");
    let program = missing.to_str().unwrap();
    let error = tried_exec(program, &[std::os::fd::AsFd::as_fd(&guard)]);
    assert!(error.raw_os_error().is_some(), "{error}");
    assert_eq!(flags(guard.as_raw_fd()), before);
    let open = || std::fs::File::open(&path).unwrap();
    assert!(matches!(
        RecordLock::try_lock(open(), 0),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    drop(original);
    assert!(matches!(
        RecordLock::try_lock(open(), 0),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    drop(guard);
    let _new = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "failed native exec closes its private temporary guard source",
        || match RecordLock::try_lock(open(), 0) {
            Ok(guard) => Some(guard),
            Err(std::fs::TryLockError::WouldBlock) => None,
            Err(std::fs::TryLockError::Error(error)) => panic!("cannot reacquire: {error}"),
        },
    );
}

#[test]
fn failed_two_role_exec_preserves_both_original_descriptions() {
    let world = aterm_tempfile::tempdir().unwrap();
    let mut originals = Vec::new();
    let mut descriptors = Vec::new();
    let mut paths = Vec::new();
    for name in ["lane", "memory"] {
        let path = world.path().join(name);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let original = RecordLock::try_lock(file, 0).unwrap().into_close_only();
        descriptors.push(original.try_clone_fd().unwrap());
        originals.push(original);
        paths.push(path);
    }
    let flags = |fd| {
        // SAFETY: both actual owned descriptors remain live through the query.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(flags >= 0);
        flags
    };
    let before: Vec<_> = descriptors.iter().map(|fd| flags(fd.as_raw_fd())).collect();
    let borrowed: Vec<_> = descriptors.iter().map(std::os::fd::AsFd::as_fd).collect();
    let missing = world.path().join("missing");
    let program = missing.to_str().unwrap();
    let error = tried_exec(program, &borrowed);
    assert!(error.raw_os_error().is_some(), "{error}");
    for (fd, before) in descriptors.iter().zip(before) {
        assert_eq!(flags(fd.as_raw_fd()), before);
    }
    drop(originals);
    for path in &paths {
        assert!(matches!(
            RecordLock::try_lock(std::fs::File::open(path).unwrap(), 0),
            Err(std::fs::TryLockError::WouldBlock)
        ));
    }
    drop(descriptors);
    for path in paths {
        let _reacquired = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "both temporary native guard sources to close",
            || match RecordLock::try_lock(std::fs::File::open(&path).unwrap(), 0) {
                Ok(held) => Some(held),
                Err(std::fs::TryLockError::WouldBlock) => None,
                Err(why) => panic!("native guard reacquisition: {why}"),
            },
        );
    }
}
