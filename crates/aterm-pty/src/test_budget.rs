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
#[path = "test_budget_tests.rs"]
mod tests;
