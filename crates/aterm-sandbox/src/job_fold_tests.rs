// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn seeded_info(flags: u32) -> JobObjectExtendedLimitInformation {
    JobObjectExtendedLimitInformation {
        BasicLimitInformation: JobObjectBasicLimitInformation {
            PerProcessUserTimeLimit: 77,
            PerJobUserTimeLimit: 88,
            LimitFlags: flags,
            MinimumWorkingSetSize: 4096,
            MaximumWorkingSetSize: 8192,
            ActiveProcessLimit: 9,
            Affinity: 3,
            PriorityClass: 2,
            SchedulingClass: 5,
        },
        IoInfo: IoCounters {
            ReadOperationCount: 1,
            WriteOperationCount: 2,
            OtherOperationCount: 3,
            ReadTransferCount: 4,
            WriteTransferCount: 5,
            OtherTransferCount: 6,
        },
        ProcessMemoryLimit: 1234,
        JobMemoryLimit: 5678,
        PeakProcessMemoryUsed: 90,
        PeakJobMemoryUsed: 91,
    }
}

/// `fold_job_limits` leaves all fields untouched for `Limits::inherit()`, preserves
/// pre-existing `LimitFlags` (such as `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE = 0x2000`), and
/// saturates `PerProcessUserTimeLimit` at `i64::MAX` instead of wrapping negative.
/// NEGATIVE CONTROL: overwriting `LimitFlags` instead of bitwise-ORing drops `0x2000`, and
/// plain multiplication on `u64::MAX` CPU seconds wraps or panics instead of saturating.
#[test]
fn fold_job_limits_preserves_flags_is_identity_on_inherit_and_saturates_time() {
    const KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;
    let baseline = seeded_info(KILL_ON_JOB_CLOSE);
    let mut untouched = baseline;
    fold_job_limits(&mut untouched, &Limits::inherit());
    assert_eq!(untouched, baseline);

    let mut folded = baseline;
    fold_job_limits(
        &mut folded,
        &Limits {
            cpu_seconds: Some(u64::MAX),
            address_space: Some(16 * 1024 * 1024),
            active_processes: Some(512),
            ..Limits::inherit()
        },
    );
    assert_eq!(
        folded.BasicLimitInformation.LimitFlags,
        KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_PROCESS_MEMORY
            | JOB_OBJECT_LIMIT_PROCESS_TIME
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
    );
    assert_eq!(folded.ProcessMemoryLimit, 16 * 1024 * 1024);
    assert_eq!(folded.BasicLimitInformation.ActiveProcessLimit, 512);
    assert_eq!(
        folded.BasicLimitInformation.PerProcessUserTimeLimit,
        i64::MAX
    );
    assert_eq!(folded.JobMemoryLimit, baseline.JobMemoryLimit);
    assert_eq!(folded.IoInfo, baseline.IoInfo);

    let mut max_bounds = baseline;
    fold_job_limits(
        &mut max_bounds,
        &Limits {
            cpu_seconds: Some(i64::MAX as u64),
            address_space: Some(u64::MAX),
            ..Limits::inherit()
        },
    );
    assert_eq!(
        max_bounds.BasicLimitInformation.PerProcessUserTimeLimit,
        i64::MAX
    );
    assert_eq!(max_bounds.ProcessMemoryLimit, usize::MAX);
}
