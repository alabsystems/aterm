// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real native returning job and actual dependency-free offline Targo fetch.
//! These judge lifetime/retirement custody, not canonical source/seed meaning,
//! the resident submission endpoint, or full Bootstrap availability.

use super::*;
use crate::private_input::{MaterializationAttempt, Producer};

fn cache_locked(path: &Path) -> bool {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.join(".package-cache-mutate"))
        .unwrap();
    matches!(
        aterm_toolchain::record_lock::RecordLock::try_lock(file, 0),
        Err(std::fs::TryLockError::WouldBlock)
    )
}

pub(super) fn actual_offline_fetch(world: &Path, seat: &Seat<'_>) {
    let source = Producer::create(world, "source").unwrap().finish().unwrap();
    let attempt = MaterializationAttempt::create(
        world,
        "materialize",
        vec![source.lend().unwrap()],
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    let destination = attempt.path().to_path_buf();
    let cache = attempt.cargo_home().to_path_buf();
    std::fs::write(
        destination.join("Cargo.toml"),
        "[package]\nname = \"custody-fetch\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[lib]\npath = \"lib.rs\"\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(destination.join("lib.rs"), "").unwrap();
    std::fs::write(
        destination.join("Cargo.lock"),
        "version = 4\n[[package]]\nname = \"custody-fetch\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    let targo = std::env::var_os("CARGO").expect("actual Targo test producer");
    let mut discriminator = command(&targo);
    discriminator.args(["--unverified", "--version"]);
    let version =
        aterm_uds::deadline::output_within(&mut discriminator, aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).starts_with("targo "));

    let mut owner = Owner::new(1, 32).unwrap();
    let mut fetch = command(targo);
    fetch
        // Fetch is non-compiling; the maintained dependency preparer uses the
        // same native verb. Targo refuses a compilation-only lane switch here.
        .args(["fetch", "--locked", "--offline"])
        .arg("--manifest-path")
        .arg(destination.join("Cargo.toml"))
        .current_dir(&destination)
        .env("CARGO_HOME", &cache)
        .env("CARGO_NET_OFFLINE", "true");
    let accepted = owner
        .submit_materialization(held(world, seat), &mut fetch, Duration::ZERO, attempt)
        .unwrap();
    assert!(accepted.publication.is_ok());
    let by = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let completed = loop {
        if let Some(done) = owner
            .tick(&mut |_, _| Ok(JobControl::Wait), &mut |_| {})
            .unwrap()
            .pop()
        {
            break done;
        }
        assert!(Instant::now() < by, "actual materialization fetch hang");
        owner.wait(by).unwrap();
    };
    assert_eq!(completed.id.sequence(), accepted.attempt.sequence);
    assert!(completed.result.as_ref().unwrap().succeeded());
    assert!(
        cache_locked(&cache),
        "native completion still owns the cache"
    );
    let attempt = completed
        .try_into_retained::<MaterializationAttempt>()
        .unwrap();
    attempt.check_current().unwrap();
    assert_eq!(attempt.path(), destination);
    assert_eq!(attempt.cargo_home(), cache);
    assert!(
        cache_locked(&cache),
        "same returned bundle retains the lock"
    );
    drop(attempt);
    assert!(!cache_locked(&cache));
    assert!(
        MaterializationAttempt::create(
            world,
            "materialize",
            vec![source.lend().unwrap()],
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
        .is_err(),
        "settled cache is not an adoptable next attempt"
    );
}

pub(super) fn revoked_input_starts_no_child(world: &Path, seat: &Seat<'_>) {
    let source = Producer::create(world, "source").unwrap().finish().unwrap();
    let attempt = MaterializationAttempt::create(
        world,
        "materialize",
        vec![source.lend().unwrap()],
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    let cache = attempt.cargo_home().to_path_buf();
    drop(source);
    let marker = world.join("must-not-start");
    let mut target = command("/usr/bin/touch");
    target.arg(&marker);
    let mut owner = Owner::new(1, 32).unwrap();
    let refused = owner
        .submit_materialization(held(world, seat), &mut target, Duration::ZERO, attempt)
        .unwrap_err();
    assert!(!marker.exists());
    assert!(owner.is_empty());
    assert!(
        cache_locked(&cache),
        "refusal retains the exact original attempt"
    );
    drop(refused);
    assert!(!cache_locked(&cache));
}
