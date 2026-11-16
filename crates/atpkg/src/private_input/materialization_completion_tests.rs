// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::private_input::Producer;
use std::{fs, time::Duration};

fn cache_is_locked(path: &Path) -> bool {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.join(".package-cache-mutate"))
        .unwrap();
    matches!(
        aterm_toolchain::record_lock::RecordLock::try_lock(file, 0),
        Err(std::fs::TryLockError::WouldBlock)
    )
}

// Lifecycle-only fixtures. The real native-return gate is exercised by the
// build_admit actual-offline-fetch and returning-completion controls.
fn completed_fixture(
    parent: &Path,
    name: &str,
    source: &super::super::Owner,
) -> CompletedMaterialization {
    let attempt = MaterializationAttempt::create(
        parent,
        name,
        vec![source.lend().unwrap()],
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    CompletedMaterialization::from_native_return(attempt)
}

#[test]
fn postchecked_materialization_keeps_the_same_namespace_cache_and_input_guards() {
    let world = aterm_tempfile::tempdir().unwrap();
    let source = Producer::create(world.path(), "source")
        .unwrap()
        .finish()
        .unwrap();
    let completed = completed_fixture(world.path(), "attempt", &source);
    let path = completed.path().to_owned();
    let cache = completed.cargo_home().to_owned();
    let input = completed
        .finish_with(|original| {
            assert_eq!(original.path(), path);
            assert_eq!(original.cargo_home(), cache);
            assert!(cache_is_locked(&cache));
            original.check_current()
        })
        .unwrap();
    input.check_current().unwrap();
    assert_eq!(input.path(), path);
    assert_eq!(input.cargo_home(), cache);
    assert!(cache_is_locked(&cache));
    assert_eq!(
        fs::read(world.path().join("attempt/.input-owner")).unwrap(),
        super::super::READY
    );
    drop(source);
    assert!(
        input.check_current().is_err(),
        "upstream owner loss still refuses"
    );
    assert!(
        cache_is_locked(&cache),
        "refusal does not release the original cache"
    );
    assert_eq!(
        fs::read(world.path().join("attempt/.input-owner")).unwrap(),
        super::super::QUARANTINED
    );
    drop(input);
    assert!(!cache_is_locked(&cache));
}

#[test]
fn ready_cache_directory_drift_quarantines_without_releasing_original_custody() {
    use std::os::unix::fs::DirBuilderExt as _;
    let world = aterm_tempfile::tempdir().unwrap();
    let source = Producer::create(world.path(), "source")
        .unwrap()
        .finish()
        .unwrap();
    let input = completed_fixture(world.path(), "attempt", &source)
        .finish_with(|original| original.check_current())
        .unwrap();
    let cache = input.cargo_home().to_owned();
    let displaced = input.path().join("original-cargo-home");
    fs::rename(&cache, &displaced).unwrap();
    fs::DirBuilder::new().mode(0o700).create(&cache).unwrap();
    assert!(input.check_current().is_err());
    assert!(cache_is_locked(&displaced));
    assert!(!cache.join(".package-cache-mutate").exists());
    fs::remove_dir(&cache).unwrap();
    fs::rename(&displaced, &cache).unwrap();
    assert!(
        input.check_current().is_err(),
        "restoration cannot revive READY"
    );
    assert!(cache_is_locked(&cache));
    assert_eq!(
        fs::read(world.path().join("attempt/.input-owner")).unwrap(),
        super::super::QUARANTINED
    );
    drop(input);
    assert!(!cache_is_locked(&cache));
}

#[test]
fn failed_postcheck_returns_the_original_completion_and_cannot_be_retried() {
    let world = aterm_tempfile::tempdir().unwrap();
    let source = Producer::create(world.path(), "source")
        .unwrap()
        .finish()
        .unwrap();
    let completed = completed_fixture(world.path(), "attempt", &source);
    let cache = completed.cargo_home().to_owned();
    let error = completed
        .finish_with(|_| Err(io::Error::other("semantic postcheck refused")))
        .err()
        .expect("failed postcheck must refuse");
    assert_eq!(error.error().to_string(), "semantic postcheck refused");
    assert!(cache_is_locked(&cache));
    let (_, original) = error.into_parts();
    assert_eq!(original.cargo_home(), cache);
    assert!(original.check_current().is_err());
    let error = original
        .finish_with(|_| panic!("quarantined attempt cannot rerun postchecks"))
        .err()
        .expect("quarantine is permanent");
    assert!(cache_is_locked(&cache));
    drop(error);
    assert!(!cache_is_locked(&cache));
}

#[test]
fn restored_cache_path_does_not_erase_a_postcheck_observed_failure() {
    let world = aterm_tempfile::tempdir().unwrap();
    let source = Producer::create(world.path(), "source")
        .unwrap()
        .finish()
        .unwrap();
    let completed = completed_fixture(world.path(), "attempt", &source);
    let cache = completed.cargo_home().to_owned();
    let error = completed
        .finish_with(|original| {
            let lock = original.cargo_home().join(".package-cache-mutate");
            let displaced = original.cargo_home().join("original-lock");
            fs::rename(&lock, &displaced)?;
            fs::write(&lock, b"replacement")?;
            assert!(original.check_current().is_err());
            fs::remove_file(&lock)?;
            fs::rename(&displaced, &lock)?;
            Ok(()) // Ignoring the earlier refusal cannot revive the original.
        })
        .err()
        .expect("restoring a path cannot restore eligibility");
    assert!(cache_is_locked(&cache));
    let (_, original) = error.into_parts();
    assert!(original.check_current().is_err());
    assert_eq!(
        fs::read(world.path().join("attempt/.input-owner")).unwrap(),
        super::super::QUARANTINED
    );
}

#[test]
fn postcheck_unwind_quarantines_the_original_namespace() {
    let world = aterm_tempfile::tempdir().unwrap();
    let source = Producer::create(world.path(), "source")
        .unwrap()
        .finish()
        .unwrap();
    let completed = completed_fixture(world.path(), "attempt", &source);
    let cache = completed.cargo_home().to_owned();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = completed.finish_with(|_| panic!("failed semantic validator"));
        }))
        .is_err()
    );
    assert_eq!(
        fs::read(world.path().join("attempt/.input-owner")).unwrap(),
        super::super::QUARANTINED
    );
    assert!(
        !cache_is_locked(&cache),
        "native-returned resources drop only after native settlement"
    );
    assert!(
        MaterializationAttempt::create(
            world.path(),
            "attempt",
            vec![source.lend().unwrap()],
            Instant::now() + aterm_spec::wait::MUST_HAPPEN
        )
        .is_err()
    );
}

#[test]
fn a_failed_producer_transition_returns_the_same_native_completion_and_guards() {
    let world = aterm_tempfile::tempdir().unwrap();
    let source = Producer::create(world.path(), "source")
        .unwrap()
        .finish()
        .unwrap();
    let completed = completed_fixture(world.path(), "attempt", &source);
    let path = completed.path().to_owned();
    let cache = completed.cargo_home().to_owned();
    let error = completed
        .finish_with_transition(
            |_| Ok(()),
            |producer| {
                // Real lifecycle drift after the last input bookend, before finish's
                // own original checks. This is not a fabricated successful owner.
                fs::write(
                    world.path().join("attempt/.input-owner"),
                    b"changed transition input",
                )
                .unwrap();
                producer.finish()
            },
        )
        .err()
        .expect("failed original transition must retain its producer");
    let (_, original) = error.into_parts();
    assert_eq!(original.path(), path);
    assert_eq!(original.cargo_home(), cache);
    assert!(original.check_current().is_err());
    assert!(cache_is_locked(&cache));
    assert_eq!(
        fs::read(world.path().join("attempt/.input-owner")).unwrap(),
        super::super::QUARANTINED
    );
    drop(original);
    assert!(!cache_is_locked(&cache));
}

#[test]
fn the_original_endpoint_expiring_during_postchecks_never_publishes_ready() {
    let world = aterm_tempfile::tempdir().unwrap();
    let source = Producer::create(world.path(), "source")
        .unwrap()
        .finish()
        .unwrap();
    // This finite endpoint is the behavior under test, not a latency claim.
    let endpoint = Instant::now() + Duration::from_secs(2);
    let attempt = MaterializationAttempt::create(
        world.path(),
        "attempt",
        vec![source.lend().unwrap()],
        endpoint,
    )
    .unwrap();
    let completed = CompletedMaterialization::from_native_return(attempt);
    let cache = completed.cargo_home().to_owned();
    let error = completed
        .finish_with(|original| {
            while let Some(left) = endpoint.checked_duration_since(Instant::now()) {
                std::thread::park_timeout(left);
            }
            assert_eq!(
                original.check_current().unwrap_err().kind(),
                io::ErrorKind::TimedOut
            );
            Ok(())
        })
        .err()
        .expect("elapsed original endpoint must refuse");
    assert!(cache_is_locked(&cache));
    assert_eq!(
        fs::read(world.path().join("attempt/.input-owner")).unwrap(),
        super::super::QUARANTINED
    );
    drop(error);
    assert!(!cache_is_locked(&cache));
}

#[test]
fn successful_transition_cannot_publish_after_distinct_input_drift() {
    let world = aterm_tempfile::tempdir().unwrap();
    let first = Producer::create(world.path(), "first")
        .unwrap()
        .finish()
        .unwrap();
    let second = Producer::create(world.path(), "second")
        .unwrap()
        .finish()
        .unwrap();
    let attempt = MaterializationAttempt::create(
        world.path(),
        "attempt",
        vec![first.lend().unwrap(), second.lend().unwrap()],
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    let completed = CompletedMaterialization::from_native_return(attempt);
    let path = completed.path().to_owned();
    let lifecycle = world.path().join("attempt/.input-owner");
    let cache = completed.cargo_home().to_owned();
    let marker = world.path().join("second/.input-owner");
    let original = fs::read(&marker).unwrap();
    let entered = std::cell::Cell::new(false);
    let refused = completed.finish_with_transition(
        |original| original.check_current(),
        |producer| {
            entered.set(true);
            fs::write(&marker, b"distinct input changed during actual transition").unwrap();
            // The attempt's real producer remains valid: this is a successful
            // lifecycle transition, not a fabricated ReadyWorkspace or failure.
            let owner = producer.finish().expect("same actual producer finishes");
            assert_eq!(fs::read(&lifecycle).unwrap(), super::super::READY);
            Ok(owner)
        },
    );
    assert!(entered.get(), "the transition must actually run");
    let (_, original_completion) = refused
        .err()
        .expect("final input drift must refuse")
        .into_parts();
    assert_eq!(original_completion.path(), path);
    assert_eq!(original_completion.cargo_home(), cache);
    assert!(cache_is_locked(&cache));
    fs::write(&marker, original).unwrap();
    assert!(
        original_completion.check_current().is_err(),
        "restoring input bytes cannot revive the refused ready owner"
    );
    assert_eq!(fs::read(&lifecycle).unwrap(), super::super::QUARANTINED);
    let retry =
        original_completion.finish_with(|_| panic!("quarantine must refuse before validator"));
    assert!(retry.is_err());
    assert!(cache_is_locked(&cache));
    drop(retry);
    assert!(!cache_is_locked(&cache));
}

#[test]
fn successful_transition_cannot_publish_after_original_endpoint() {
    let world = aterm_tempfile::tempdir().unwrap();
    let source = Producer::create(world.path(), "source")
        .unwrap()
        .finish()
        .unwrap();
    // This endpoint defines the behavior; it is not a latency measurement.
    let end = Instant::now() + Duration::from_secs(2);
    let attempt =
        MaterializationAttempt::create(world.path(), "attempt", vec![source.lend().unwrap()], end)
            .unwrap();
    let completed = CompletedMaterialization::from_native_return(attempt);
    let path = completed.path().to_owned();
    let lifecycle = world.path().join("attempt/.input-owner");
    let cache = completed.cargo_home().to_owned();
    let entered = std::cell::Cell::new(false);
    let refused = completed.finish_with_transition(
        |original| original.check_current(),
        |producer| {
            entered.set(true);
            while let Some(left) = end.checked_duration_since(Instant::now()) {
                std::thread::park_timeout(left);
            }
            let owner = producer
                .finish()
                .expect("the actual producer transition succeeds");
            assert_eq!(fs::read(&lifecycle).unwrap(), super::super::READY);
            Ok(owner)
        },
    );
    assert!(
        entered.get(),
        "expiry before the transition is not this control"
    );
    let (error, original_completion) = refused
        .err()
        .expect("final original expiry must refuse")
        .into_parts();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(original_completion.path(), path);
    assert_eq!(original_completion.cargo_home(), cache);
    assert!(original_completion.check_current().is_err());
    assert!(cache_is_locked(&cache));
    assert_eq!(fs::read(&lifecycle).unwrap(), super::super::QUARANTINED);
    drop(original_completion);
    assert!(!cache_is_locked(&cache));
}
