// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_toolchain::record_lock::RecordLock;
use std::os::fd::{AsRawFd as _, OwnedFd};

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
    let error = exec_as_app_with_guards(
        program,
        &[program.into()],
        &[],
        1,
        &[std::os::fd::AsFd::as_fd(&guard)],
    );
    assert!(error.raw_os_error().is_some());
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
    let error = exec_as_app_with_guards(program, &[program.into()], &[], 1, &borrowed);
    assert!(error.raw_os_error().is_some());
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
