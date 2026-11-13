// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_uds::spawnfd;
use std::time::{Duration, Instant};

#[test]
fn an_actual_legacy_wait_cannot_be_relabelled_as_measured_zero() {
    let mut child = spawnfd::command("/usr/bin/true").spawn().unwrap();
    assert!(child.wait().unwrap().success());
    assert!(child.is_reaped());
    // The actual original wait retained status but no accounting. A late
    // measurement request cannot fabricate zero counters from that cache.
    assert!(try_reap(&mut child, true).is_err());
}

#[test]
fn original_owning_wait_preserves_success_failure_and_cancel_accounting() {
    use std::os::unix::process::ExitStatusExt as _;
    for (script, cancel, expected) in [
        ("exit 0", false, Some(0)),
        ("exit 7", false, Some(7)),
        ("exec /bin/sleep 60", true, None),
    ] {
        let mut command = spawnfd::command("/bin/sh");
        command.arg("-c").arg(script);
        let mut child = command.spawn().unwrap();
        if cancel {
            child.kill().unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        let (status, observed) = loop {
            if let Some(result) = try_reap(&mut child, true).unwrap() {
                break result;
            }
            assert!(
                Instant::now() < deadline,
                "original owning wait did not complete"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(status.code(), expected);
        if cancel {
            assert_eq!(status.signal(), Some(9));
        }
        // This reads the same child's cached owning-wait result. It does
        // not perform a second native wait or select a process by PID.
        let original = child.try_wait_with_rusage().unwrap().unwrap();
        assert_eq!(observed, observation(original));
        assert!(observed.cpu_times.is_some());
        assert!(observed.peak_rss_bytes.is_some());
        assert_eq!(observed.scope, AccountingScope::ChildAndWaitedDescendantsV1);
    }
}
