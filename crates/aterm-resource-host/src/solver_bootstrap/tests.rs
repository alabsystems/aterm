// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real original children/private sockets/native restrictions. This controlled
//! entry does not stand in for an AY image, root claim or production recipe.
use super::*;
use std::{
    fs::OpenOptions,
    io::Write as _,
    os::{
        fd::FromRawFd as _,
        unix::{fs::OpenOptionsExt as _, process::ExitStatusExt as _},
    },
    time::Duration,
};

const ROLE: &str = "ATERM_TEST_PRIVATE_SOLVER_BOOTSTRAP";

/// The existing library-test initializer calls this before libtest/ordinary work.
pub(crate) fn entry() {
    let Some(mode) = std::env::var_os(ROLE) else {
        return;
    };
    let run = || -> io::Result<()> {
        let fd = std::env::var("ATERM_TEST_SOLVER_FD")
            .map_err(|_| invalid())?
            .parse::<i32>()
            .map_err(|_| invalid())?;
        if fd < 3 {
            return Err(invalid());
        }
        // SAFETY: duplicate the locator without adopting or closing any existing
        // owner. Invalid/unopen descriptors fail before constructing OwnedFd.
        let copied = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
        if copied < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the successful native duplication returned a fresh owned fd.
        let fd = unsafe { OwnedFd::from_raw_fd(copied) };
        let accepted = receive(
            CtlStream::from(fd),
            crate::fixture_deadline(Instant::now()),
            |peer| {
                if process::read_birth(peer.pid as i32) != Some(process::Birth(peer.birth)) {
                    return Err(invalid());
                }
                // Controlled same-image parent only, not a production Store/claim
                // authenticator. No wire PID or path is promoted to authority.
                validate_image(peer.pid)
            },
        )?;
        if accepted.attempt != if mode == "actor" { [1; 16] } else { [21; 16] }
            || accepted.units != 3
            || accepted.footprint_mib == 0
            || accepted.monotonic_ns == 0
        {
            return Err(invalid());
        }
        if mode == "handoff" {
            // This is the first action after the receiver's one-shot channel
            // transfer, before kernel probes, journal reads or payload work.
            drop(accepted);
            let marker = std::env::var_os("ATERM_TEST_SOLVER_WORK").ok_or_else(invalid)?;
            std::fs::write(marker, b"original lane handed off")?;
            std::thread::sleep(Duration::from_secs(120));
            return Ok(());
        }
        // Ready is sent only after the combined seal; exercise its actual kernel
        // behavior as well as the Host's read-only fork observation.
        // SAFETY: an unexpected fork child only calls _exit, and its original
        // parent positively waits it before reporting the failed restriction.
        let forked = unsafe { libc::fork() };
        if forked == 0 {
            // SAFETY: no Rust cleanup or allocator use in the fork child.
            unsafe { libc::_exit(91) };
        }
        if forked > 0 {
            let mut status = 0;
            loop {
                // SAFETY: the original unreaped child created immediately above.
                let waited = unsafe { libc::waitpid(forked, &mut status, 0) };
                if waited == forked {
                    break;
                }
                if waited < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                    break;
                }
            }
            return Err(invalid());
        }
        if io::Error::last_os_error().raw_os_error() != Some(libc::EPERM) {
            return Err(invalid());
        }
        let args = [c"true".as_ptr(), std::ptr::null()];
        // SAFETY: fixed complete argv. A broken restriction replaces only this
        // owned payload, which then fails to create the required work marker.
        if unsafe { libc::execv(c"/usr/bin/true".as_ptr(), args.as_ptr()) } != -1
            || io::Error::last_os_error().raw_os_error() != Some(libc::EPERM)
        {
            return Err(invalid());
        }
        let journal = std::env::var_os("ATERM_TEST_SOLVER_JOURNAL").ok_or_else(invalid)?;
        let journal = std::fs::read(journal)?;
        if mode == "actor" {
            let started: aterm_json::Value = aterm_json::from_slice(&journal)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
            if started["pid"].as_u64() != Some(u64::from(accepted.pid))
                || started["birth"].as_u64() != Some(accepted.birth)
                || started["monotonic_ns"].as_u64() != Some(accepted.monotonic_ns)
                || started["unix_ns"].as_u64().is_none_or(|clock| clock == 0)
                || started["requested_units"].as_u64() != Some(u64::from(accepted.units))
                || started["requested_footprint_mib"].as_u64()
                    != Some(u64::from(accepted.footprint_mib))
            {
                return Err(invalid());
            }
        } else if journal
            != record(
                accepted.attempt,
                accepted.pid,
                accepted.birth,
                accepted.units,
                accepted.footprint_mib,
                accepted.monotonic_ns,
            )
        {
            return Err(invalid());
        }
        let marker = std::env::var_os("ATERM_TEST_SOLVER_WORK").ok_or_else(invalid)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(marker)?;
        writeln!(
            file,
            "run {} {} {}",
            accepted.units, accepted.footprint_mib, accepted.monotonic_ns
        )?;
        aterm_durable::durable(&file)?;
        if mode == "linger" {
            // Accepted's lifetime cannot disable the process-lifetime watcher.
            // The original Host endpoint remains live until the parent test
            // explicitly closes it; this body otherwise outlives its deadline.
            drop(accepted);
            std::thread::sleep(Duration::from_secs(120));
            return Ok(());
        }
        if mode == "allocate" {
            // Finite, identical work under each requested final native limit.
            let mut pages = vec![0_u8; 256 * 1024 * 1024];
            for page in pages.chunks_mut(4096) {
                page[0] = 1;
            }
            std::hint::black_box(&pages);
            std::thread::sleep(Duration::from_millis(250));
        }
        Ok(())
    };
    let result = run();
    if let Err(error) = &result {
        eprintln!("private solver bootstrap: {error}");
    }
    std::process::exit(if result.is_ok() { 0 } else { 75 });
}

fn validate_image(pid: u32) -> io::Result<()> {
    let image = process::LoadedImage::read(pid as i32).ok_or_else(invalid)?;
    if image.matches_file(&std::env::current_exe()?) != Some(true) {
        return Err(invalid());
    }
    Ok(())
}

fn record(attempt: [u8; 16], pid: u32, birth: u64, units: u32, mib: u32, clock: u64) -> Vec<u8> {
    let mut bytes = attempt.to_vec();
    bytes.extend(pid.to_be_bytes());
    bytes.extend(birth.to_be_bytes());
    bytes.extend(units.to_be_bytes());
    bytes.extend(mib.to_be_bytes());
    bytes.extend(clock.to_be_bytes());
    bytes
}

fn persist(root: &std::path::Path, intent: &RunIntent) -> io::Result<()> {
    let mut journal = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.join("journal"))?;
    journal.write_all(&record(
        intent.attempt,
        intent.pid,
        intent.birth,
        intent.units,
        intent.footprint_mib,
        intent.monotonic_ns,
    ))?;
    aterm_durable::durable(&journal)?;
    // Creation is part of this controlled commit, so retain its directory entry too.
    aterm_durable::durable(&std::fs::File::open(root)?)
}

struct Child(spawnfd::Child);
impl Drop for Child {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn spawn(root: &std::path::Path, mib: u32, mode: &str) -> (Owner, Child) {
    let limit = spawnfd::FootprintLimit::new(mib).unwrap();
    let mut owner =
        Owner::new([21; 16], 3, limit, crate::fixture_deadline(Instant::now())).unwrap();
    let mut command = spawnfd::command(std::env::current_exe().unwrap());
    command
        .env_clear()
        .env(ROLE, mode)
        .env("ATERM_TEST_SOLVER_FD", "9")
        .env("ATERM_TEST_SOLVER_WORK", root.join("work"))
        .env("ATERM_TEST_SOLVER_JOURNAL", root.join("journal"))
        .current_dir(root)
        .setsid(true)
        .start_suspended(true)
        .stdout(spawnfd::Stdio::null())
        .stderr(spawnfd::Stdio::piped())
        .footprint_limit(limit);
    command
        .inherit_guards_at(vec![owner.extra(9).unwrap()])
        .unwrap();
    let child = Child(command.spawn().unwrap());
    (owner, child)
}

fn ready(owner: &mut Owner, child: &mut Child) {
    while !owner.poll_ready(&mut child.0, validate_image).unwrap() {
        assert!(Instant::now() < owner.deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn wait(child: &mut Child) -> std::process::ExitStatus {
    let deadline = crate::fixture_deadline(Instant::now());
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn final_setexec_hold_transfers_once_before_the_single_combined_seal() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "solver_bootstrap::tests::final_setexec_hold_transfers_once_before_the_single_combined_seal",
    ) {
        return;
    }
    crate::runtime::test_endpoint().unwrap();
    for mib in [512, 64] {
        let root = aterm_tempfile::tempdir().unwrap();
        let limit = spawnfd::FootprintLimit::new(mib).unwrap();
        let deadline = crate::fixture_deadline(Instant::now());
        let mut owner = Owner::new([21; 16], 3, limit, deadline).unwrap();
        let mut selected = spawnfd::command(std::env::current_exe().unwrap());
        selected
            .env_clear()
            .env(ROLE, "allocate")
            .env("ATERM_TEST_SOLVER_FD", "9")
            .env("ATERM_TEST_SOLVER_WORK", root.path().join("work"))
            .env("ATERM_TEST_SOLVER_JOURNAL", root.path().join("journal"))
            .current_dir(root.path())
            .footprint_limit(limit);
        let (mut prelude, mut command) =
            crate::metadata_prelude::Owner::new_for_child_seal(&selected, &[9], deadline).unwrap();
        command
            .stdout(spawnfd::Stdio::null())
            .stderr(spawnfd::Stdio::piped());
        command
            .inherit_guards_at(vec![owner.extra(9).unwrap(), prelude.extra().unwrap()])
            .unwrap();
        prelude.configured(&mut command).unwrap();
        assert!(
            command.get_footprint_limit().is_none(),
            "intermediate remains uncapped"
        );
        let mut child = Child(command.spawn().unwrap());
        let original = process::read_lineage(child.0.id() as i32).unwrap();
        while !prelude.poll(&mut child.0, validate_image).unwrap() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        let executed = process::read_lineage(child.0.id() as i32).unwrap();
        assert_eq!(
            (
                executed.identity.pid,
                executed.identity.birth,
                executed.uniqueid,
                executed.parent,
                executed.pgid
            ),
            (
                original.identity.pid,
                original.identity.birth,
                original.uniqueid,
                original.parent,
                original.pgid
            ),
        );
        assert!(original.identity.idversion.is_some() && executed.identity.idversion.is_some());
        assert_ne!(
            executed.identity.idversion, original.identity.idversion,
            "actual SETEXEC changes the image version without replacing its lifetime"
        );
        assert!(!crate::metadata_prelude::no_fork(child.0.id()).unwrap());
        assert!(
            prelude.take_retirement().is_none(),
            "unsealed target is not contained"
        );
        let target = prelude.take_prepared_target().unwrap();
        assert!(
            prelude.take_prepared_target().is_err(),
            "hold transfers only once"
        );
        owner.bind_prepared(target).unwrap();
        ready(&mut owner, &mut child);
        assert!(!root.path().join("work").exists());
        let contained = owner.take_retirement().unwrap();
        assert!(contained.matches(child.0.id(), original.identity.birth.0, child.0.id() as i32));
        assert!(owner.take_retirement().is_none());
        while !owner
            .poll_run(
                &mut child.0,
                validate_image,
                || false,
                |intent| persist(root.path(), intent),
            )
            .unwrap()
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        let status = wait(&mut child);
        let mut error = String::new();
        child
            .0
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut error)
            .unwrap();
        assert!(root.path().join("work").exists(), "{status:?}: {error}");
        if mib == 512 {
            assert!(status.success(), "{status:?}: {error}");
        } else {
            assert_eq!(status.signal(), Some(libc::SIGKILL), "{error}");
        }
    }
}

#[test]
fn actual_sealed_child_waits_for_durable_run_and_retains_final_limit() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "solver_bootstrap::tests::actual_sealed_child_waits_for_durable_run_and_retains_final_limit",
    ) {
        return;
    }
    for mib in [512, 64] {
        let root = aterm_tempfile::tempdir().unwrap();
        let (mut owner, mut child) = spawn(root.path(), mib, "allocate");
        ready(&mut owner, &mut child);
        assert!(!root.path().join("work").exists());
        assert!(child.0.try_wait().unwrap().is_none());
        let commits = std::cell::Cell::new(0);
        while !owner
            .poll_run(
                &mut child.0,
                validate_image,
                || false,
                |intent| {
                    assert_eq!(commits.replace(1), 0, "same Run must commit only once");
                    assert_eq!(intent.attempt, [21; 16]);
                    assert!(intent.pid > 1 && intent.birth > 0);
                    assert_eq!(intent.units, 3);
                    assert_eq!(intent.footprint_mib, mib);
                    assert!(!root.path().join("work").exists());
                    assert!(intent.monotonic_ns > 0);
                    persist(root.path(), intent)
                },
            )
            .unwrap()
        {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(commits.get(), 1);
        let status = wait(&mut child);
        let mut error = String::new();
        child
            .0
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut error)
            .unwrap();
        assert!(
            root.path().join("work").exists(),
            "no post-Run body: {status:?}: {error}"
        );
        if mib == 512 {
            assert!(status.success(), "{status:?}: {error}");
        } else {
            assert_eq!(status.signal(), Some(libc::SIGKILL), "{error}");
        }
    }
}

#[test]
fn losing_the_original_host_endpoint_stops_the_sealed_running_child() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "solver_bootstrap::tests::losing_the_original_host_endpoint_stops_the_sealed_running_child",
    ) {
        return;
    }
    enum LossStage {
        Ready,
        RunDelivered,
        Handoff,
        Running,
    }
    for stage in [
        LossStage::Ready,
        LossStage::RunDelivered,
        LossStage::Handoff,
        LossStage::Running,
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mode = if matches!(stage, LossStage::Handoff) {
            "handoff"
        } else {
            "linger"
        };
        let (mut owner, mut child) = spawn(root.path(), 512, mode);
        ready(&mut owner, &mut child);
        assert!(!root.path().join("work").exists());
        if matches!(stage, LossStage::Ready) {
            // Sealing and Ready have completed, but no Run was committed or sent.
            drop(owner);
            assert_eq!(wait(&mut child).code(), Some(75));
            assert!(!root.path().join("work").exists());
            continue;
        }
        while !owner
            .poll_run(
                &mut child.0,
                validate_image,
                || false,
                |intent| persist(root.path(), intent),
            )
            .unwrap()
        {
            std::thread::sleep(Duration::from_millis(2));
        }
        if matches!(stage, LossStage::RunDelivered) {
            // Do not wait for payload work or watcher scheduling: the original
            // endpoint closes immediately after the complete Run write. Work
            // may race a valid Run; loss must still stop this same child.
            drop(owner);
            assert_eq!(wait(&mut child).code(), Some(75));
            continue;
        }
        let deadline = crate::fixture_deadline(Instant::now());
        while !root.path().join("work").exists() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "payload exited before post-Run work"
            );
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        if matches!(stage, LossStage::Handoff) {
            // No grace sleep: close as soon as the child reports that the
            // original endpoint entered the watcher handoff.
            drop(owner);
            assert_eq!(wait(&mut child).code(), Some(75));
            continue;
        }
        std::thread::sleep(Duration::from_millis(20));
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "a live Host must not stop its payload"
        );
        drop(owner);
        assert_eq!(wait(&mut child).code(), Some(75));
    }
}

#[test]
fn failed_durable_commit_never_runs_or_retries_the_original_attempt() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (mut owner, mut child) = spawn(root.path(), 512, "plain");
    ready(&mut owner, &mut child);
    assert!(
        owner
            .poll_run(
                &mut child.0,
                validate_image,
                || false,
                |_| Err(io::ErrorKind::WouldBlock.into())
            )
            .is_err()
    );
    assert!(
        owner
            .poll_run(
                &mut child.0,
                validate_image,
                || false,
                |_| panic!("must not retry an uncertain durable commit")
            )
            .is_err()
    );
    assert!(!root.path().join("work").exists());
    drop(owner); // Private EOF, not a synthesized solver failure or scope receipt.
    assert_eq!(wait(&mut child).code(), Some(75));
    assert!(!root.path().join("work").exists());
}

#[test]
fn cancellation_after_durable_intent_prevents_the_first_run_byte() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (mut owner, mut child) = spawn(root.path(), 512, "plain");
    ready(&mut owner, &mut child);
    let cancelled = std::cell::Cell::new(false);
    assert!(
        owner
            .poll_run(
                &mut child.0,
                validate_image,
                || cancelled.get(),
                |intent| {
                    persist(root.path(), intent)?;
                    cancelled.set(true);
                    Ok(())
                }
            )
            .is_err()
    );
    assert_eq!(owner.written, 0);
    assert!(!root.path().join("work").exists());
    assert!(
        owner
            .poll_run(
                &mut child.0,
                validate_image,
                || false,
                |_| panic!("post-commit cancellation cannot retry")
            )
            .is_err()
    );
    drop(owner);
    assert_eq!(wait(&mut child).code(), Some(75));
}

#[test]
fn bootstrap_records_refuse_cross_attempt_kind_overflow_and_truncation() {
    let frame = Frame {
        kind: 1,
        attempt: [1; 16],
        challenge: [2; 32],
        pid: 20,
        birth: 40,
        footprint_mib: 64,
        units: 3,
        monotonic_ns: 0,
    };
    let bytes = frame.encode();
    assert!(Frame::decode(&bytes).unwrap() == frame);
    for end in 0..LEN {
        assert!(Frame::decode(&bytes[..end]).is_err());
    }
    let mut extra = bytes.to_vec();
    extra.push(0);
    assert!(Frame::decode(&extra).is_err());
    for (offset, value) in [(8, 9_u64), (64, u64::MAX), (80, 0), (88, 0), (96, 1)] {
        let mut bad = bytes;
        bad[offset..offset + 8].copy_from_slice(&value.to_be_bytes());
        assert!(Frame::decode(&bad).is_err());
    }
    let mut other = frame.response(2, 0);
    other.attempt[0] ^= 1;
    assert!(other != frame.response(2, 0));
}
