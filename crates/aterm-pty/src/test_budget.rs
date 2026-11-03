// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Admission for tests that hold many real PTYs at once. A machine has one PTY
//! table, shared by test binaries and their children; CPU parallelism does not
//! describe its capacity. Dense fixtures take this lease before their first
//! pair and keep it until every pair has closed. The lease does not retry PTY
//! creation or hide a failed allocation.

use std::path::Path;
use std::time::Duration;

use aterm_uds::private_file::{PrivateLock, create_private_dir};

/// One machine-local admission for a dense PTY fixture. The file lock also
/// coordinates separate worktrees and child test processes. It is not inherited
/// through exec and is explicitly unlocked before its descriptor closes.
pub struct Lease {
    _lock: PrivateLock,
}

impl Lease {
    /// Wait for the previous dense fixture to close its pairs. One minute is a
    /// hang detector for a fixture that retained its lease, not an allocation
    /// retry budget. Every caller must inherit the runner's temporary root.
    ///
    /// # Errors
    /// The shared private directory cannot be created or validated, or a holder
    /// fails to release the lease within one minute.
    pub fn take() -> std::io::Result<Self> {
        let quiet = aterm_tempfile::quiet_in(&std::env::temp_dir());
        std::fs::create_dir_all(&quiet)?;
        // SAFETY: geteuid reads this process's effective uid and has no preconditions.
        let uid = unsafe { libc::geteuid() };
        let dir = quiet.join(format!("aterm-pty-tests-{uid}.noindex"));
        Self::take_in(&dir, Duration::from_secs(60), || {})
    }

    fn take_in(dir: &Path, within: Duration, held: impl FnMut()) -> std::io::Result<Self> {
        create_private_dir(dir)?;
        let lock = PrivateLock::take_with(
            dir,
            "dense-fixture.lock",
            within,
            "a dense PTY fixture did not release its test lease",
            held,
        )?;
        Ok(Self { _lock: lock })
    }
}

#[cfg(test)]
mod tests {
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
            let output =
                aterm_uds::deadline::output_within(&mut cmd, Duration::from_secs(60)).unwrap();
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
}
