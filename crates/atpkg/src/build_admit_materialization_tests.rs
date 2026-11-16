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
    let completed = completed.try_into_materialization().unwrap();
    completed.check_current().unwrap();
    assert_eq!(completed.path(), destination);
    assert_eq!(completed.cargo_home(), cache);
    let input = completed
        .finish_with(|original| {
            assert_eq!(original.path(), destination);
            assert_eq!(original.cargo_home(), cache);
            assert!(original.path().join("Cargo.lock").is_file());
            original.check_current()
        })
        .unwrap();
    input.check_current().unwrap();
    assert_eq!(input.path(), destination);
    assert_eq!(input.cargo_home(), cache);
    assert!(
        cache_locked(&cache),
        "same returned bundle retains the lock"
    );
    drop(input);
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

pub(super) fn a_different_success_cannot_finish_materialization(world: &Path, seat: &Seat<'_>) {
    let source = Producer::create(world, "source").unwrap().finish().unwrap();
    let attempt = MaterializationAttempt::create(
        world,
        "failed-materialization",
        vec![source.lend().unwrap()],
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    let cache = attempt.cargo_home().to_owned();
    let mut jobs = Jobs::new(1).unwrap();
    let mut target = command("/bin/sh");
    target
        .args([
            "-c",
            "printf 'materialization-ready\\n'; read input; exit 7",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let ticket = jobs
        .submit_returning(held(world, seat), &mut target, Duration::ZERO, attempt)
        .unwrap();
    assert!(until(&mut jobs, |jobs| !jobs.jobs[&ticket.id.sequence].starting).is_empty());
    let child = jobs
        .jobs
        .get_mut(&ticket.id.sequence)
        .unwrap()
        .owned
        .as_mut()
        .unwrap()
        .child_mut();
    // Establish execution before triggering the intended nonzero exit. An
    // early launcher/transport refusal is a different hostile control.
    aterm_uds::deadline::prints(
        child,
        "materialization-ready",
        aterm_spec::wait::MUST_HAPPEN,
    );
    drop(child.stdin.take());
    let mut failed = until(&mut jobs, Jobs::is_empty).pop().unwrap();
    assert!(!failed.result.as_ref().unwrap().succeeded());
    jobs.submit_retained(
        held(world, seat),
        &mut command("/usr/bin/true"),
        Duration::ZERO,
        (),
    )
    .unwrap();
    let successful = until(&mut jobs, Jobs::is_empty).pop().unwrap();
    assert!(successful.result.as_ref().unwrap().succeeded());
    assert_ne!(failed.id, successful.id);
    failed.id = successful.id;
    failed.result = successful.result;
    let refused = failed
        .try_into_materialization()
        .err()
        .expect("edited public success cannot finish the failed original materialization");
    assert!(cache_locked(&cache));
    assert!(
        std::fs::read_to_string(world.join("failed-materialization/.input-owner"))
            .unwrap()
            .contains("materializing")
    );
    drop(refused);
    assert!(!cache_locked(&cache));
    assert!(
        std::fs::read_to_string(world.join("failed-materialization/.input-owner"))
            .unwrap()
            .contains("quarantined")
    );
}
