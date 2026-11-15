// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::{fs, os::unix::fs::PermissionsExt as _, time::Duration};

fn create(parent: &Path) -> MaterializationWorkspace {
    MaterializationWorkspace::create(
        parent,
        "workspace",
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap()
}

fn locked(path: &Path) -> bool {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    matches!(
        aterm_toolchain::record_lock::RecordLock::try_lock(file, 0),
        Err(std::fs::TryLockError::WouldBlock)
    )
}

#[test]
fn fresh_workspace_owns_its_actual_producer_cache_and_unchanged_endpoint_without_input_uses() {
    let world = aterm_tempfile::tempdir().unwrap();
    let by = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let workspace = MaterializationWorkspace::create(world.path(), "workspace", by).unwrap();
    assert_eq!(workspace.deadline(), by);
    assert_eq!(workspace.cargo_home(), workspace.path().join("cargo-home"));
    workspace.check_current().unwrap();
    assert_eq!(workspace.deadline(), by);
    assert!(locked(&world.path().join("workspace/.input-owner.lock")));
    assert!(locked(
        &workspace.cargo_home().join(".package-cache-mutate")
    ));
    assert_eq!(
        fs::read(world.path().join("workspace/.input-owner")).unwrap(),
        super::super::BUILDING
    );
    assert_eq!(
        fs::metadata(workspace.cargo_home())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn preexpired_workspace_starts_no_namespace_and_existing_data_is_never_adopted() {
    let world = aterm_tempfile::tempdir().unwrap();
    let error = MaterializationWorkspace::create(world.path(), "workspace", Instant::now())
        .err()
        .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(!world.path().join("workspace").exists());
    fs::create_dir(world.path().join("existing")).unwrap();
    fs::write(world.path().join("existing/sentinel"), b"existing data").unwrap();
    let error = MaterializationWorkspace::create(
        world.path(),
        "existing",
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .err()
    .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(
        fs::read(world.path().join("existing/sentinel")).unwrap(),
        b"existing data"
    );
    assert!(!world.path().join("existing/.input-owner").exists());
}

#[test]
fn actual_cache_admission_failure_quarantines_partial_namespace_without_adoption_or_removal() {
    let world = aterm_tempfile::tempdir().unwrap();
    let result = MaterializationWorkspace::create_with(
        world.path(),
        "workspace",
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        |cache| {
            let lock = cache.join(".package-cache-mutate");
            fs::write(&lock, b"invalid shared lock mode")?;
            fs::set_permissions(lock, fs::Permissions::from_mode(0o666))
        },
    );
    assert!(result.is_err());
    let root = world.path().join("workspace");
    assert_eq!(
        fs::read(root.join(".input-owner")).unwrap(),
        super::super::QUARANTINED
    );
    assert_eq!(
        fs::read(root.join("contents/cargo-home/.package-cache-mutate")).unwrap(),
        b"invalid shared lock mode"
    );
    assert!(
        MaterializationWorkspace::create(
            world.path(),
            "workspace",
            Instant::now() + aterm_spec::wait::MUST_HAPPEN
        )
        .is_err()
    );
}

#[test]
fn existing_foreign_cache_alias_cannot_replace_the_freshly_created_cache() {
    use std::os::unix::fs::{DirBuilderExt as _, symlink};
    let world = aterm_tempfile::tempdir().unwrap();
    let foreign = world.path().join("preexisting-cache");
    fs::DirBuilder::new().mode(0o700).create(&foreign).unwrap();
    fs::write(foreign.join("sentinel"), b"existing foreign cache").unwrap();
    let result = MaterializationWorkspace::create_with(
        world.path(),
        "workspace",
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        |cache| {
            fs::remove_dir(cache)?;
            symlink(&foreign, cache)
        },
    );
    eprintln!(
        "fresh-cache accepted foreign alias: {}; foreign mutate lock created: {}",
        result.is_ok(),
        foreign.join(".package-cache-mutate").exists()
    );
    assert!(
        result.is_err(),
        "preexisting foreign cache was adopted as a fresh materialization cache"
    );
    assert!(
        !foreign.join(".package-cache-mutate").exists(),
        "refused creation touched the foreign cache"
    );
    assert_eq!(
        fs::read(foreign.join("sentinel")).unwrap(),
        b"existing foreign cache"
    );
}

#[test]
fn existing_foreign_directory_cannot_replace_fresh_cache_before_acquisition() {
    use std::os::unix::fs::DirBuilderExt as _;
    let world = aterm_tempfile::tempdir().unwrap();
    let foreign = world.path().join("foreign");
    fs::DirBuilder::new().mode(0o700).create(&foreign).unwrap();
    fs::write(foreign.join("sentinel"), b"foreign private cache").unwrap();
    let result = MaterializationWorkspace::create_with(
        world.path(),
        "workspace",
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        |cache| {
            fs::remove_dir(cache)?;
            fs::rename(&foreign, cache)
        },
    );
    assert!(result.is_err());
    let replacement = world.path().join("workspace/contents/cargo-home");
    assert!(!replacement.join(".package-cache-mutate").exists());
    assert_eq!(
        fs::read(replacement.join("sentinel")).unwrap(),
        b"foreign private cache"
    );
}

#[test]
fn fresh_cache_privacy_drift_revokes_the_same_workspace_after_restoration() {
    let world = aterm_tempfile::tempdir().unwrap();
    let workspace = create(world.path());
    fs::set_permissions(workspace.cargo_home(), fs::Permissions::from_mode(0o777)).unwrap();
    assert!(workspace.check_current().is_err());
    fs::set_permissions(workspace.cargo_home(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(workspace.check_current().is_err());
    assert!(locked(
        &workspace.cargo_home().join(".package-cache-mutate")
    ));
}

#[test]
fn constructor_final_deadline_refuses_after_real_partial_materialization() {
    let world = aterm_tempfile::tempdir().unwrap();
    let by = Instant::now() + Duration::from_millis(100);
    let result = MaterializationWorkspace::create_with(world.path(), "workspace", by, |cache| {
        fs::write(cache.join("partial"), b"actual partial output")?;
        std::thread::sleep(by.saturating_duration_since(Instant::now()) + Duration::from_millis(2));
        Ok(())
    });
    assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::TimedOut));
    assert_eq!(
        fs::read(world.path().join("workspace/.input-owner")).unwrap(),
        super::super::QUARANTINED
    );
    assert_eq!(
        fs::read(world.path().join("workspace/contents/cargo-home/partial")).unwrap(),
        b"actual partial output"
    );
}

#[test]
fn quarantine_keeps_original_kernel_locks_until_drop_and_does_not_authorize_reuse() {
    let world = aterm_tempfile::tempdir().unwrap();
    let workspace = create(world.path());
    let cache = workspace.cargo_home().join(".package-cache-mutate");
    let producer = world.path().join("workspace/.input-owner.lock");
    fs::write(workspace.path().join("partial"), b"kept output").unwrap();
    workspace.quarantine();
    assert!(workspace.check_current().is_err());
    assert!(locked(&cache));
    assert!(locked(&producer));
    drop(workspace);
    assert!(!locked(&cache));
    assert!(!locked(&producer));
    assert_eq!(
        fs::read(world.path().join("workspace/.input-owner")).unwrap(),
        super::super::QUARANTINED
    );
    assert_eq!(
        fs::read(world.path().join("workspace/contents/partial")).unwrap(),
        b"kept output"
    );
    assert!(
        MaterializationWorkspace::create(
            world.path(),
            "workspace",
            Instant::now() + aterm_spec::wait::MUST_HAPPEN
        )
        .is_err()
    );
}

#[test]
fn observed_cache_lock_replacement_is_sticky_while_same_original_guard_stays_held() {
    let world = aterm_tempfile::tempdir().unwrap();
    let workspace = create(world.path());
    let lock = workspace.cargo_home().join(".package-cache-mutate");
    let original = workspace.cargo_home().join("old-lock");
    fs::rename(&lock, &original).unwrap();
    fs::write(&lock, b"replacement").unwrap();
    assert!(workspace.check_current().is_err());
    assert!(locked(&original));
    fs::remove_file(&lock).unwrap();
    fs::rename(original, &lock).unwrap();
    assert!(workspace.check_current().is_err());
    assert!(locked(&lock));
}

#[test]
fn replaced_namespace_only_quarantines_the_original_and_keeps_its_cache_locked() {
    let world = aterm_tempfile::tempdir().unwrap();
    let workspace = create(world.path());
    let root = world.path().join("workspace");
    let original = world.path().join("original");
    fs::rename(&root, &original).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(root.join("sentinel"), b"foreign namespace").unwrap();
    assert!(workspace.check_current().is_err());
    assert_eq!(
        fs::read(original.join(".input-owner")).unwrap(),
        super::super::QUARANTINED
    );
    assert!(!root.join(".input-owner").exists());
    assert_eq!(
        fs::read(root.join("sentinel")).unwrap(),
        b"foreign namespace"
    );
    assert!(locked(
        &original.join("contents/cargo-home/.package-cache-mutate")
    ));
}

#[test]
fn actual_host_death_leaves_fresh_workspace_unavailable_for_adoption() {
    use std::os::unix::process::ExitStatusExt as _;
    const ROLE: &str = "ATPKG_FRESH_WORKSPACE_HOST";
    const EXACT: &str = "private_input::workspace::tests::actual_host_death_leaves_fresh_workspace_unavailable_for_adoption";
    if let Some(parent) = std::env::var_os(ROLE) {
        let workspace = create(Path::new(&parent));
        fs::write(workspace.path().join("partial"), b"lost host output").unwrap();
        aterm_uds::process::kill_self_now();
    }
    let world = aterm_tempfile::tempdir().unwrap();
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    child
        .args(["--exact", EXACT, "--nocapture"])
        .env(ROLE, world.path());
    let output =
        aterm_uds::deadline::output_within(&mut child, aterm_spec::wait::MUST_HAPPEN).unwrap();
    assert_eq!(output.status.signal(), Some(libc::SIGKILL));
    let root = world.path().join("workspace");
    assert_eq!(
        fs::read(root.join(".input-owner")).unwrap(),
        super::super::BUILDING
    );
    assert_eq!(
        fs::read(root.join("contents/partial")).unwrap(),
        b"lost host output"
    );
    assert!(!locked(
        &root.join("contents/cargo-home/.package-cache-mutate")
    ));
    assert!(
        MaterializationWorkspace::create(
            world.path(),
            "workspace",
            Instant::now() + aterm_spec::wait::MUST_HAPPEN
        )
        .is_err()
    );
}
