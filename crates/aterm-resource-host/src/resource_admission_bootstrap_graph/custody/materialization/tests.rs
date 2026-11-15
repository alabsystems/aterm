// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual descriptor/cache-lifetime controls, no authenticated Root is issued.
use super::super::tests::Inputs;
use super::*;

#[test]
fn contents_observation_comes_from_the_same_retained_workspace_description() {
    let source = Inputs::new();
    let parent = aterm_tempfile::tempdir().unwrap();
    let deadline = source.original.deadline;
    let workspace = atpkg::private_input::MaterializationWorkspace::create(
        parent.path(),
        "workspace",
        deadline,
    )
    .unwrap();
    let expected = workspace
        .with_contents_directory(|original| {
            let meta = spawnfd::descriptor_metadata(std::os::fd::AsRawFd::as_raw_fd(&original))?;
            Ok(trust_resource_controller::FileIdentity {
                device: meta.dev(),
                inode: meta.ino(),
            })
        })
        .unwrap();
    assert_ne!(expected, source.original.request.source_directory);
    assert_eq!(
        observe_contents(&workspace, &source.original, deadline).unwrap(),
        expected
    );
    workspace.check_current().unwrap();
    source.original.check_current(deadline).unwrap();
    let cache = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(workspace.cargo_home().join(".package-cache-mutate"))
        .unwrap();
    assert!(
        matches!(
            aterm_toolchain::record_lock::RecordLock::try_lock(cache, 0),
            Err(std::fs::TryLockError::WouldBlock)
        ),
        "observation released the actual private cache guard"
    );
}

#[test]
fn failed_contents_observation_quarantines_without_releasing_the_cache_guard() {
    let source = Inputs::new();
    let parent = aterm_tempfile::tempdir().unwrap();
    let workspace = atpkg::private_input::MaterializationWorkspace::create(
        parent.path(),
        "workspace",
        source.original.deadline,
    )
    .unwrap();
    let error = observe_contents(
        &workspace,
        &source.original,
        Instant::now() - Duration::from_millis(1),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(workspace.check_current().is_err());
    let cache = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(workspace.cargo_home().join(".package-cache-mutate"))
        .unwrap();
    assert!(
        matches!(
            aterm_toolchain::record_lock::RecordLock::try_lock(cache, 0),
            Err(std::fs::TryLockError::WouldBlock)
        ),
        "refusal released the actual private cache guard"
    );
}
