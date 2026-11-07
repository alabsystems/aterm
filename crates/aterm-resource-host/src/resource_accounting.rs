// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Collect only at the original owned child's reaping boundary. No PID-based
//! sampler, separate waiter or decoded observation can supply these fields.
use aterm_types::resource_admission::{AccountingObservation, AccountingScope, CpuTimes};
use aterm_uds::spawnfd::{Child, Rusage};
use std::{io, process::ExitStatus};

pub(crate) const fn supported() -> bool {
    cfg!(all(
        target_pointer_width = "64",
        any(target_os = "macos", target_os = "linux")
    ))
}

fn observation(usage: Rusage) -> AccountingObservation {
    let cpu_times = u64::try_from(usage.user.as_nanos())
        .ok()
        .zip(u64::try_from(usage.system.as_nanos()).ok())
        .map(|(user_ns, system_ns)| CpuTimes { user_ns, system_ns });
    AccountingObservation {
        scope: AccountingScope::ChildAndWaitedDescendantsV1,
        cpu_times,
        peak_rss_bytes: Some(usage.maxrss),
    }
}

/// Reap the same original child once. Unsupported accounting retains its real
/// status with explicitly absent fields; ownership errors never fabricate one.
pub(crate) fn try_reap(
    child: &mut Child,
    accounted: bool,
) -> io::Result<Option<(ExitStatus, AccountingObservation)>> {
    if !accounted {
        return child
            .try_wait()
            .map(|status| status.map(|status| (status, AccountingObservation::uncollected())));
    }
    match child.try_wait_with_rusage() {
        Ok(usage) => Ok(usage.map(|usage| (usage.status, observation(usage)))),
        Err(error) if error.kind() == io::ErrorKind::Unsupported => child
            .try_wait()
            .map(|status| status.map(|status| (status, AccountingObservation::uncollected()))),
        Err(error) => Err(error),
    }
}

#[cfg(all(
    test,
    target_pointer_width = "64",
    any(target_os = "macos", target_os = "linux")
))]
mod tests {
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
}
