// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::{fs, time::Duration};

#[test]
fn failed_preflight_returns_original_inputs_before_creating_a_namespace() {
    let world = aterm_tempfile::tempdir().unwrap();
    let source = Producer::create(world.path(), "source")
        .unwrap()
        .finish()
        .unwrap();
    let refused = MaterializationAttempt::create(
        world.path(),
        "attempt",
        vec![source.lend().unwrap()],
        Instant::now(),
    )
    .err()
    .expect("expired input endpoint refuses");
    assert_eq!(refused.error().kind(), io::ErrorKind::TimedOut);
    assert!(!world.path().join("attempt").exists());
    let (_, original) = refused.into_parts();
    assert_eq!(original.len(), 1);
    original[0].check_current().unwrap();
    assert!(
        source.retire().is_err(),
        "the actual original use remains held"
    );
}

#[test]
fn private_cache_cannot_be_reopened_as_another_materialization_attempt() {
    let world = aterm_tempfile::tempdir().unwrap();
    let source = Producer::create(world.path(), "source")
        .unwrap()
        .finish()
        .unwrap();
    let attempt = MaterializationAttempt::create(
        world.path(),
        "attempt",
        vec![source.lend().unwrap()],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    assert_eq!(attempt.cargo_home(), attempt.path().join("cargo-home"));
    attempt.check_current().unwrap();
    fs::write(
        attempt.cargo_home().join("cache-sentinel"),
        b"original private cache",
    )
    .unwrap();
    drop(attempt);
    let namespace = world.path().join("attempt");
    assert_eq!(
        fs::read(namespace.join(super::super::STATE)).unwrap(),
        super::super::QUARANTINED
    );
    let refused = MaterializationAttempt::create(
        world.path(),
        "attempt",
        vec![source.lend().unwrap()],
        Instant::now() + Duration::from_secs(60),
    )
    .err()
    .expect("quarantined attempt is never adopted");
    assert_eq!(refused.error().kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(
        fs::read(namespace.join("contents/cargo-home/cache-sentinel")).unwrap(),
        b"original private cache"
    );
}

#[test]
fn cache_identity_failure_is_monotone_even_if_the_original_name_is_restored() {
    let world = aterm_tempfile::tempdir().unwrap();
    let source = Producer::create(world.path(), "source")
        .unwrap()
        .finish()
        .unwrap();
    let attempt = MaterializationAttempt::create(
        world.path(),
        "attempt",
        vec![source.lend().unwrap()],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    let lock = attempt.cargo_home().join(".package-cache-mutate");
    let original = attempt.cargo_home().join("original-cache-lock");
    fs::rename(&lock, &original).unwrap();
    fs::write(&lock, b"replacement").unwrap();
    assert!(attempt.check_current().is_err());
    fs::remove_file(&lock).unwrap();
    fs::rename(&original, &lock).unwrap();
    assert!(
        attempt.check_current().is_err(),
        "name restoration cannot undo refusal"
    );
    let original_lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(world.path().join("attempt/.input-owner.lock"))
        .unwrap();
    assert!(matches!(
        aterm_toolchain::record_lock::RecordLock::try_lock(original_lock, 0),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    assert_eq!(
        fs::read(world.path().join("attempt/.input-owner")).unwrap(),
        super::super::QUARANTINED
    );
}

#[test]
fn rejected_postcheck_keeps_actual_cache_custody_until_the_bundle_is_dropped() {
    let world = aterm_tempfile::tempdir().unwrap();
    let source = Producer::create(world.path(), "source")
        .unwrap()
        .finish()
        .unwrap();
    let attempt = MaterializationAttempt::create(
        world.path(),
        "attempt",
        vec![source.lend().unwrap()],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    let cache = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(attempt.cargo_home().join(".package-cache-mutate"))
        .unwrap();
    attempt.quarantine();
    assert!(attempt.check_current().is_err());
    assert!(matches!(
        aterm_toolchain::record_lock::RecordLock::try_lock(cache.try_clone().unwrap(), 0),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    drop(attempt);
    aterm_toolchain::record_lock::RecordLock::try_lock(cache, 0).unwrap();
}

#[test]
fn host_death_leaves_a_nonadoptable_private_cache_namespace() {
    use std::os::unix::process::ExitStatusExt as _;
    const ROLE: &str = "ATPKG_MATERIALIZATION_LOSS_PARENT";
    const EXACT: &str = "private_input::materialization::tests::host_death_leaves_a_nonadoptable_private_cache_namespace";
    if let Some(parent) = std::env::var_os(ROLE) {
        let parent = Path::new(&parent);
        let source = Producer::create(parent, "source")
            .unwrap()
            .finish()
            .unwrap();
        let attempt = MaterializationAttempt::create(
            parent,
            "attempt",
            vec![source.lend().unwrap()],
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap();
        fs::write(
            attempt.cargo_home().join("cache-sentinel"),
            b"created by lost host",
        )
        .unwrap();
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
    let namespace = world.path().join("attempt");
    assert_eq!(
        fs::read(namespace.join(super::super::STATE)).unwrap(),
        super::super::BUILDING
    );
    assert_eq!(
        fs::read(namespace.join("contents/cargo-home/cache-sentinel")).unwrap(),
        b"created by lost host"
    );
    let source = Producer::create(world.path(), "new-source")
        .unwrap()
        .finish()
        .unwrap();
    let refused = MaterializationAttempt::create(
        world.path(),
        "attempt",
        vec![source.lend().unwrap()],
        Instant::now() + Duration::from_secs(60),
    )
    .err()
    .expect("dead host's existing namespace cannot mint a new owner");
    assert_eq!(refused.error().kind(), io::ErrorKind::AlreadyExists);
}
