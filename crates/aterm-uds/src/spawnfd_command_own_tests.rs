// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::Command;
use std::cell::Cell;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};

#[test]
fn a_temporary_process_group_is_restored_after_an_unwind() {
    let mut command = Command::new("/bin/cat");
    command.process_group(17);
    let reached_injected_unwind = Cell::new(false);

    let unwound = catch_unwind(AssertUnwindSafe(|| {
        command.with_dedicated_process_group(|command| {
            assert_eq!(command.pgroup, Some(0));
            reached_injected_unwind.set(true);
            panic!("injected spawn unwind");
        });
    }));

    assert!(unwound.is_err());
    assert!(reached_injected_unwind.get());
    assert_eq!(command.pgroup, Some(17));
}

#[test]
fn a_dedicated_group_spawn_restores_the_configured_group() {
    let mut configured = Command::new("/bin/sleep");
    configured.arg("30").process_group(17);
    let mut child = configured
        .spawn_in_dedicated_process_group()
        .expect("dedicated spawn");
    child.kill().expect("kill");
    child.wait().expect("wait");
    assert_eq!(configured.pgroup, Some(17));

    let mut plain = Command::new("/bin/sleep");
    plain.arg("30");
    let mut child = plain
        .spawn_in_dedicated_process_group()
        .expect("dedicated spawn");
    child.kill().expect("kill");
    child.wait().expect("wait");
    assert_eq!(plain.pgroup, None);
}

#[test]
fn a_joined_group_is_restored_after_an_unwind() {
    let mut command = Command::new("/bin/cat");
    command.process_group(17);
    let reached_injected_unwind = Cell::new(false);

    let unwound = catch_unwind(AssertUnwindSafe(|| {
        command.with_temporary_process_group(4242, |command| {
            assert_eq!(command.pgroup, Some(4242));
            reached_injected_unwind.set(true);
            panic!("injected spawn unwind");
        });
    }));

    assert!(unwound.is_err());
    assert!(reached_injected_unwind.get());
    assert_eq!(command.pgroup, Some(17));
}

#[test]
fn joining_a_group_restores_the_configured_group_on_success_and_error() {
    let mut leader_command = Command::new("/bin/sleep");
    leader_command.arg("30");
    let mut leader = leader_command
        .spawn_in_dedicated_process_group()
        .expect("dedicated spawn");
    let group = i32::try_from(leader.id()).expect("pid");

    let mut configured = Command::new("/bin/sleep");
    configured.arg("30").process_group(17);
    let joined = configured.spawn_in_process_group(group);
    let mut plain = Command::new("/bin/sleep");
    plain.arg("30");
    let missing = plain.spawn_in_process_group(i32::MAX);

    let joined_ok = joined.is_ok();
    if let Ok(mut joined) = joined {
        joined.kill().expect("kill");
        joined.wait().expect("wait");
    }
    let missing_ok = missing.is_ok();
    if let Ok(mut missing) = missing {
        missing.kill().expect("kill");
        missing.wait().expect("wait");
    }
    leader.kill().expect("kill");
    leader.wait().expect("wait");

    assert!(joined_ok, "join the leader's group");
    assert_eq!(configured.pgroup, Some(17));
    assert!(!missing_ok, "a missing group is an error");
    assert_eq!(plain.pgroup, None);
}

#[test]
fn joining_refuses_contradictory_policies_without_touching_the_command() {
    for group in [1, 0, -1, i32::MIN] {
        let mut command = Command::new("/bin/sleep");
        command.arg("30").process_group(17);
        let error = command
            .spawn_in_process_group(group)
            .expect_err("a group of at most 1");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{error}");
        assert_eq!(command.pgroup, Some(17));
        assert!(!command.setsid);
        assert!(!command.tether);
    }

    let mut session = Command::new("/bin/sleep");
    session.arg("30").process_group(17).setsid(true);
    let error = session
        .spawn_in_process_group(4242)
        .expect_err("a new session beside a join");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{error}");
    assert_eq!(session.pgroup, Some(17));
    assert!(session.setsid, "the refusal does not reset the session");

    let mut tethered = Command::new("/bin/sleep");
    tethered.arg("30").process_group(17);
    tethered.tether = true;
    let error = tethered
        .spawn_in_process_group(4242)
        .expect_err("a tether beside a join");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{error}");
    assert_eq!(tethered.pgroup, Some(17));
    assert!(tethered.tether, "the refusal does not reset the tether");
}

/// A tethered spawn's exec status shares the spawn lock as every other spawn does: its pipe is
/// made while a spawn is in flight, without waiting for it (a creation through
/// [`crate::kept_out`] holds the lock exclusively, so it would wait for every spawn in flight and
/// queue every later one behind it), and neither end carries the fork-exclusion mark, so the
/// spawn that hands its write end takes the shared hold. NEGATIVE CONTROL: making the pipe through
/// `crate::kept_out` waits out the hang detector for the spawn held here (and, on a kernel that
/// keeps the mark, has the spawn handing the write end hold the lock exclusively).
#[cfg(target_vendor = "apple")]
#[test]
fn a_tethered_spawns_exec_status_shares_the_spawn_lock() {
    use std::os::fd::AsRawFd as _;
    use std::time::{Duration, Instant};
    const FD_CLOFORK: i32 = 2;
    /// A hang detector, never a latency budget.
    const HANG: Duration = Duration::from_secs(60);
    let in_flight = crate::spawn_lock::spawning();
    let maker = std::thread::spawn(super::ExecStatus::new);
    let started = Instant::now();
    while !maker.is_finished() {
        assert!(
            started.elapsed() < HANG,
            "the exec status waited for a spawn in flight"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    drop(in_flight);
    let status = maker.join().expect("the maker").expect("an exec status");
    for (end, fd) in [
        ("read", status.read.as_raw_fd()),
        ("write", status.write.as_raw_fd()),
    ] {
        // SAFETY: `F_GETFD` reads the flags of a descriptor `status` holds open.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(flags >= 0, "the {end} end's flags");
        assert_eq!(
            flags & libc::FD_CLOEXEC,
            libc::FD_CLOEXEC,
            "the {end} end is close-on-exec"
        );
        assert_eq!(
            flags & FD_CLOFORK,
            0,
            "the {end} end is not kept out of children"
        );
    }
    let handing = crate::spawn_lock::handing(&[status.write.as_raw_fd()]);
    assert_ne!(
        crate::spawn_lock::seam::exclusive_holder(),
        Some(std::thread::current().id()),
        "the spawn handing the write end holds the lock exclusively"
    );
    drop(handing);
}
