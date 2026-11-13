// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::time::Duration;

#[test]
fn lease_child() {
    let Some(dir) = std::env::var_os("ATERM_PTY_LEASE_TEST_DIR") else {
        return;
    };
    if std::env::var_os("ATERM_PTY_LEASE_EXPECT_HELD").is_some() {
        let probe = PrivateLock::take(
            Path::new(&dir),
            "dense-fixture.lock",
            Duration::ZERO,
            "held",
        );
        assert_eq!(probe.err().unwrap().kind(), std::io::ErrorKind::WouldBlock);
    } else {
        assert!(Lease::take_in(Path::new(&dir), || {}).is_ok());
    }
    println!("pty-lease-probe-complete");
}

#[test]
fn another_process_observes_the_lease_and_panic_releases_it() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = root.path().join("budget");
    let child = |held: bool| {
        let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        cmd.args(["--exact", "test_budget::tests::lease_child", "--nocapture"])
            .env("ATERM_PTY_LEASE_TEST_DIR", &dir)
            .env_remove("ATERM_PTY_LEASE_EXPECT_HELD");
        if held {
            cmd.env("ATERM_PTY_LEASE_EXPECT_HELD", "1");
        }
        let output = aterm_uds::deadline::output_within(&mut cmd, Duration::from_secs(60)).unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line.ends_with("pty-lease-probe-complete")),
            "the child must actually execute the lease probe"
        );
    };
    let lease = Lease::take_in(&dir, || {}).unwrap();
    child(true);
    drop(lease);
    child(false);
    let panic = std::panic::catch_unwind(|| {
        let _lease = Lease::take_in(&dir, || {}).unwrap();
        panic!("fixture failed");
    });
    assert!(panic.is_err());
    child(false);
}

#[test]
fn an_independent_open_waits_for_the_whole_fixture_and_drop_releases_it() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = root.path().join("budget");
    let mut first = Some(Lease::take_in(&dir, || {}).unwrap());
    assert_eq!(
        PrivateLock::take(&dir, "dense-fixture.lock", Duration::ZERO, "held")
            .err()
            .unwrap()
            .kind(),
        std::io::ErrorKind::WouldBlock,
    );
    let mut observed = false;
    let second = Lease::take_in(&dir, || {
        observed = true;
        first.take();
    })
    .unwrap();
    assert!(
        observed,
        "the second holder cannot enter while the first is live"
    );
    drop(second);
    assert!(PrivateLock::take(&dir, "dense-fixture.lock", Duration::ZERO, "held").is_ok());
}

#[test]
fn abandoned_lease_child() {
    let Some(dir) = std::env::var_os("ATERM_PTY_CRASH_TEST_DIR") else {
        return;
    };
    let _lease = Lease::take_in(Path::new(&dir), || {}).unwrap();
    println!("pty-lease-crash-ready");
    std::io::Write::flush(&mut std::io::stdout()).unwrap();
    aterm_uds::process::kill_self_now();
}

#[test]
fn a_dead_holder_does_not_keep_dense_fixture_admission() {
    use std::os::unix::process::ExitStatusExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = root.path().join("crashed-budget");
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "test_budget::tests::abandoned_lease_child",
            "--nocapture",
        ])
        .env("ATERM_PTY_CRASH_TEST_DIR", &dir);
    let output = aterm_uds::deadline::output_within(&mut child, Duration::from_secs(60)).unwrap();
    assert_eq!(output.status.signal(), Some(libc::SIGKILL));
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.ends_with("pty-lease-crash-ready"))
    );
    let mut next = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    next.args(["--exact", "test_budget::tests::lease_child", "--nocapture"])
        .env("ATERM_PTY_LEASE_TEST_DIR", &dir)
        .env_remove("ATERM_PTY_LEASE_EXPECT_HELD");
    let output = aterm_uds::deadline::output_within(&mut next, Duration::from_secs(60)).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.ends_with("pty-lease-probe-complete"))
    );
}

#[test]
fn fixture_outliving_its_admission_parent() {
    use std::io::Read as _;
    use std::os::fd::{FromRawFd as _, OwnedFd};
    let Some(dir) = std::env::var_os("ATERM_PTY_ORPHAN_FIXTURE_DIR") else {
        return;
    };
    let dir = Path::new(&dir);
    let guard = std::env::var("ATERM_PTY_ORPHAN_GUARD").ok().map(|guard| {
        let guard = guard.parse::<std::os::fd::RawFd>().unwrap();
        // SAFETY: the admission parent explicitly transferred this unclaimed guard through exec.
        let guard = unsafe { OwnedFd::from_raw_fd(guard) };
        aterm_uds::keep_out_of_children(&guard).unwrap();
        guard
    });
    let (master, slave) = crate::open_pty_pair().unwrap();
    // SAFETY: open_pty_pair returned two distinct owned descriptors, transferred exactly once.
    let pair = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    std::fs::write(dir.join("fixture-ready"), b"real PTY pair held").unwrap();
    std::io::stdin().read_to_end(&mut Vec::new()).unwrap();
    drop(pair);
    drop(guard);
    std::fs::write(dir.join("fixture-closed"), b"real PTY pair closed").unwrap();
}

#[test]
fn admission_parent_with_live_fixture() {
    let Some(dir) = std::env::var_os("ATERM_PTY_ORPHAN_PARENT_DIR") else {
        return;
    };
    let dir = Path::new(&dir);
    let lease = Lease::take_in(dir, || {}).unwrap();
    let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "test_budget::tests::fixture_outliving_its_admission_parent",
            "--nocapture",
        ])
        .env("ATERM_PTY_ORPHAN_FIXTURE_DIR", dir)
        .stdin(aterm_uds::spawnfd::Stdio::inherit())
        .stdout(aterm_uds::spawnfd::Stdio::null());
    let guard =
        aterm_uds::kept_out(|| std::os::fd::AsFd::as_fd(&lease).try_clone_to_owned()).unwrap();
    command.inherit_guards(vec![guard]).unwrap();
    let guard = command.guard_targets().next().unwrap();
    command.env("ATERM_PTY_ORPHAN_GUARD", guard.to_string());
    let _child = command.spawn().unwrap();
    assert!(
        aterm_spec::wait::within(Duration::from_secs(60), Duration::from_millis(10), || {
            dir.join("fixture-ready").exists().then_some(())
        })
        .is_some()
    );
    aterm_uds::process::kill_self_now();
}

#[test]
fn a_live_fixture_keeps_admission_after_its_parent_is_killed() {
    use std::os::unix::process::ExitStatusExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = root.path().join("orphan-budget");
    let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "test_budget::tests::admission_parent_with_live_fixture",
            "--nocapture",
        ])
        .env("ATERM_PTY_ORPHAN_PARENT_DIR", &dir)
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::null());
    let mut parent = command.spawn().unwrap();
    let input = parent.stdin.take().unwrap();
    let status =
        aterm_spec::wait::within(Duration::from_secs(60), Duration::from_millis(10), || {
            parent.try_wait().unwrap()
        });
    let held = PrivateLock::take(&dir, "dense-fixture.lock", Duration::ZERO, "held")
        .is_err_and(|error| error.kind() == std::io::ErrorKind::WouldBlock);
    // Release the real fixture before judging the admission result, including the negative control.
    drop(input);
    if status.is_none() {
        aterm_uds::deadline::kill_within(&mut parent, Duration::from_secs(60));
    }
    let closed =
        aterm_spec::wait::within(Duration::from_secs(60), Duration::from_millis(10), || {
            dir.join("fixture-closed").exists().then_some(())
        });
    assert_eq!(status.unwrap().signal(), Some(libc::SIGKILL));
    assert!(closed.is_some(), "the fixture must close its real PTY pair");
    assert!(
        held,
        "a live fixture must retain admission after its parent dies"
    );
    assert!(
        PrivateLock::take(&dir, "dense-fixture.lock", Duration::ZERO, "held").is_ok(),
        "the closed fixture must release admission"
    );
}
