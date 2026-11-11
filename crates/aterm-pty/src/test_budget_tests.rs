// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn lease_child() {
    let Some(dir) = std::env::var_os("ATERM_PTY_LEASE_TEST_DIR") else {
        return;
    };
    let lease = Lease::take_in(Path::new(&dir), Duration::ZERO, || {});
    if std::env::var_os("ATERM_PTY_LEASE_EXPECT_HELD").is_some() {
        assert_eq!(lease.err().unwrap().kind(), std::io::ErrorKind::WouldBlock);
    } else {
        assert!(lease.is_ok());
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
    let lease = Lease::take_in(&dir, Duration::ZERO, || {}).unwrap();
    child(true);
    drop(lease);
    child(false);
    let panic = std::panic::catch_unwind(|| {
        let _lease = Lease::take_in(&dir, Duration::ZERO, || {}).unwrap();
        panic!("fixture failed");
    });
    assert!(panic.is_err());
    child(false);
}

#[test]
fn an_independent_open_waits_for_the_whole_fixture_and_drop_releases_it() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = root.path().join("budget");
    let mut first = Some(Lease::take_in(&dir, Duration::ZERO, || {}).unwrap());
    assert_eq!(
        Lease::take_in(&dir, Duration::ZERO, || {})
            .err()
            .unwrap()
            .kind(),
        std::io::ErrorKind::WouldBlock,
    );
    let mut observed = false;
    let second = Lease::take_in(&dir, Duration::from_secs(60), || {
        observed = true;
        first.take();
    })
    .unwrap();
    assert!(
        observed,
        "the second holder cannot enter while the first is live"
    );
    drop(second);
    assert!(Lease::take_in(&dir, Duration::ZERO, || {}).is_ok());
}
