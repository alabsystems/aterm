// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual fresh-owner/FD controls. No callback supplies native child funding.
use super::*;
use std::io::Write as _;
use std::os::unix::fs::MetadataExt as _;

fn actual() -> (aterm_tempfile::TempDir, MaterializationWorkspace) {
    let parent = aterm_tempfile::tempdir().unwrap();
    let workspace = MaterializationWorkspace::create(
        parent.path(),
        "workspace",
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    (parent, workspace)
}

fn locked(workspace: &MaterializationWorkspace) -> bool {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(workspace.cargo_home().join(".package-cache-mutate"))
        .unwrap();
    matches!(
        aterm_toolchain::record_lock::RecordLock::try_lock(file, 0),
        Err(std::fs::TryLockError::WouldBlock)
    )
}

#[test]
fn callback_receives_same_original_directory_and_can_write_only_fresh_output() {
    let (_parent, workspace) = actual();
    let named = std::fs::metadata(workspace.path()).unwrap();
    let deadline = workspace.deadline();
    workspace
        .with_contents_directory(|directory| {
            let actual = std::fs::File::from(directory.try_clone_to_owned()?).metadata()?;
            assert_eq!((actual.dev(), actual.ino()), (named.dev(), named.ino()));
            let mut output = std::fs::File::from(aterm_dirfd::openat(
                directory,
                "payload",
                aterm_dirfd::OFlags::WRONLY
                    | aterm_dirfd::OFlags::CREATE
                    | aterm_dirfd::OFlags::EXCL
                    | aterm_dirfd::OFlags::NOFOLLOW
                    | aterm_dirfd::OFlags::CLOEXEC,
                aterm_dirfd::Mode::from_bits(0o600),
            )?);
            output.write_all(b"unvalidated archive bytes")
        })
        .unwrap();
    assert_eq!(
        std::fs::read(workspace.path().join("payload")).unwrap(),
        b"unvalidated archive bytes"
    );
    assert_eq!(workspace.deadline(), deadline);
    workspace.check_current().unwrap();
    assert!(locked(&workspace));
}

#[test]
fn callback_error_and_unwind_quarantine_without_releasing_actual_cache_guards() {
    let (_parent, workspace) = actual();
    let error = workspace
        .with_contents_directory::<()>(|_| Err(io::ErrorKind::PermissionDenied.into()))
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(workspace.check_current().is_err());
    assert!(locked(&workspace));
    let (_other_parent, other) = actual();
    let panic = catch_unwind(AssertUnwindSafe(|| {
        let _: io::Result<()> = other.with_contents_directory(|_| panic!("owned callback failure"));
    }));
    assert!(panic.is_err());
    assert!(other.check_current().is_err());
    assert!(locked(&other));
}

#[test]
fn actual_descriptor_role_drift_is_sticky_even_after_flag_restoration() {
    let (_parent, workspace) = actual();
    let mut raw = None;
    assert!(
        workspace
            .with_contents_directory(|directory| {
                raw = Some(directory.as_raw_fd());
                // SAFETY: this test owns the still-retained actual workspace FD.
                assert_eq!(
                    unsafe { libc::fcntl(directory.as_raw_fd(), libc::F_SETFD, 0) },
                    0
                );
                Ok(())
            })
            .is_err()
    );
    // SAFETY: workspace still owns this same descriptor; no close/reuse occurred.
    assert_eq!(
        unsafe { libc::fcntl(raw.unwrap(), libc::F_SETFD, libc::FD_CLOEXEC) },
        0
    );
    assert!(workspace.check_current().is_err());
    assert!(locked(&workspace));
}
