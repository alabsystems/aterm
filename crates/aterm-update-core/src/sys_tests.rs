// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// A dropped lock is free immediately even while a cloned descriptor remains open.
#[cfg(unix)]
#[test]
fn a_dropped_lock_is_free_while_a_copy_of_its_descriptor_lives() {
    let dir = crate::test_scratch_dir("lock-dup");
    let p = dir.path().join("dup.lock");
    let held = FileLock::acquire(&p).expect("acquire");
    let childs_copy = held._file.try_clone().expect("dup the lock's descriptor");
    drop(held);
    FileLock::acquire_within(&p, std::time::Duration::ZERO)
        .expect("free the moment its guard drops, whatever else holds the descriptor");
    drop(childs_copy);
    let open = || {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&p)
            .expect("open")
    };
    let held = FileLock::lock_open(open()).expect("lock an open file");
    let childs_copy = held._file.try_clone().expect("dup");
    drop(held);
    FileLock::acquire_within(&p, std::time::Duration::ZERO).expect("free");
    drop(childs_copy);
}

/// `FileLock::acquire` sets `FD_CLOEXEC` on its descriptor.
#[cfg(unix)]
#[test]
fn file_lock_is_close_on_exec() {
    use std::os::fd::AsRawFd;

    let dir = crate::test_scratch_dir("lock-cloexec");
    let p = dir.path().join("cloexec.lock");
    let lock = FileLock::acquire(&p).expect("acquire");
    // SAFETY: F_GETFD only reads descriptor flags from this live File.
    let flags = unsafe { libc::fcntl(lock._file.as_raw_fd(), libc::F_GETFD) };
    assert!(flags >= 0);
    assert_ne!(
        flags & libc::FD_CLOEXEC,
        0,
        "successful exec must release the apply transaction atomically"
    );
    drop(lock);
    std::fs::remove_file(&p).expect("remove lock before symlink");
    let decoy = dir.path().join("decoy.lock");
    std::os::unix::fs::symlink(&decoy, &p).expect("symlink");
    assert!(
        FileLock::acquire(&p).is_err(),
        "a symlinked lock path must never be followed"
    );
    assert!(
        !decoy.exists(),
        "symlinked lock target must never be created"
    );
}

#[cfg(windows)]
#[test]
fn acquire_fails_fast_on_permanent_error() {
    let dir = crate::test_scratch_dir("no-such-dir");
    let missing = dir.path().join("absent").join("lock");
    let err = match FileLock::acquire_within(&missing, std::time::Duration::from_secs(120)) {
        Ok(_) => panic!("acquire must fail on a missing parent"),
        Err(e) => e,
    };
    assert_ne!(err.kind(), std::io::ErrorKind::TimedOut);
    assert!(!matches!(err.raw_os_error(), Some(32 | 33)));
}

#[test]
fn same_volume_true_within_a_dir_false_for_missing() {
    let d = crate::test_scratch_dir("utf8-tag-with-émoji-🚀-exceeding-twenty-bytes");
    assert!(
        d.path().to_string_lossy().ends_with(".noindex"),
        "scratch dir must carry .noindex suffix: {}",
        d.path().display()
    );
    let a = d.path().join("sv-a");
    let b = d.path().join("sv-b");
    std::fs::write(&a, b"x").unwrap();
    std::fs::write(&b, b"y").unwrap();
    assert!(
        same_volume(&a, &b),
        "two files in the same dir are same-volume"
    );
    assert!(
        !same_volume(&a, Path::new("/no/such/path-aterm-xyz")),
        "a missing path is not same-volume (fail closed)"
    );
    let free_lock = d.path().join("sv-max.lock");
    drop(
        FileLock::acquire_within(&free_lock, std::time::Duration::MAX)
            .expect("Duration::MAX must not overflow Instant::now() + limit"),
    );
}
