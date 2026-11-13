// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::native_document_host::{
    AdvisoryLockRefusal, HeldAdvisoryLock, acquire_advisory_lock_within,
};
use std::fs::File;
use std::time::{Duration, Instant};

fn token() -> (aterm_tempfile::TempDir, std::path::PathBuf) {
    let dir = crate::test_scratch::short_dir("socket owner scheduling");
    let path = dir.path().join("fixture.token");
    assert!(write_fresh_token(&path, &"0".repeat(64)).is_some());
    (dir, path)
}

fn release_after_contention(
    file: File,
    left: Duration,
    probe: HeldAdvisoryLock,
) -> Result<HeldAdvisoryLock, AdvisoryLockRefusal> {
    let (file, refusal) = HeldAdvisoryLock::try_lock_keeping(file)
        .expect_err("the first observation must see the competing native probe");
    assert!(matches!(refusal, std::fs::TryLockError::WouldBlock));
    if left.is_zero() {
        return Err(AdvisoryLockRefusal::Busy);
    }
    drop(probe);
    acquire_advisory_lock_within(file, left)
}

#[test]
fn startup_can_follow_a_delayed_probe_but_still_has_a_hang_detector() {
    let (_dir, path) = token();
    for (elapsed, expected) in [
        (Duration::from_millis(200), true),
        (Duration::from_secs(61), false),
    ] {
        let probe = HeldAdvisoryLock::lock(File::open(&path).unwrap()).unwrap();
        let held = SocketOwner::hold_at(&path, Instant::now() - elapsed, |file, left| {
            release_after_contention(file, left, probe)
        });
        assert_eq!(held.is_some(), expected, "elapsed={elapsed:?}");
        if held.is_some() {
            assert_eq!(
                aterm_uds::ownerlock::probe(&path),
                aterm_uds::ownerlock::Owner::Live
            );
        }
        drop(held);
        assert!(SocketOwner::hold(&path).is_some());
    }
}
