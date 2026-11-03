// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure Windows Job Object limit-folding core ([`fold_job_limits`]).

use crate::Limits;

pub(crate) const JOB_OBJECT_LIMIT_PROCESS_TIME: u32 = 0x0000_0002;
pub(crate) const JOB_OBJECT_LIMIT_ACTIVE_PROCESS: u32 = 0x0000_0008;
pub(crate) const JOB_OBJECT_LIMIT_PROCESS_MEMORY: u32 = 0x0000_0100;
const HUNDRED_NS_PER_SEC: i64 = 10_000_000;
#[cfg(all(windows, test))]
pub(crate) const HUNDRED_NS_PER_SEC_TEST: i64 = HUNDRED_NS_PER_SEC;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(non_snake_case)]
pub(crate) struct JobObjectBasicLimitInformation {
    pub(crate) PerProcessUserTimeLimit: i64,
    pub(crate) PerJobUserTimeLimit: i64,
    pub(crate) LimitFlags: u32,
    pub(crate) MinimumWorkingSetSize: usize,
    pub(crate) MaximumWorkingSetSize: usize,
    pub(crate) ActiveProcessLimit: u32,
    pub(crate) Affinity: usize,
    pub(crate) PriorityClass: u32,
    pub(crate) SchedulingClass: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(non_snake_case)]
pub(crate) struct IoCounters {
    pub(crate) ReadOperationCount: u64,
    pub(crate) WriteOperationCount: u64,
    pub(crate) OtherOperationCount: u64,
    pub(crate) ReadTransferCount: u64,
    pub(crate) WriteTransferCount: u64,
    pub(crate) OtherTransferCount: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(non_snake_case)]
pub(crate) struct JobObjectExtendedLimitInformation {
    pub(crate) BasicLimitInformation: JobObjectBasicLimitInformation,
    pub(crate) IoInfo: IoCounters,
    pub(crate) ProcessMemoryLimit: usize,
    pub(crate) JobMemoryLimit: usize,
    pub(crate) PeakProcessMemoryUsed: usize,
    pub(crate) PeakJobMemoryUsed: usize,
}

/// Fold the requested resource limits into `info` while preserving pre-existing `LimitFlags` and
/// unrelated fields (`sandbox-job-fold`).
pub(crate) fn fold_job_limits(info: &mut JobObjectExtendedLimitInformation, limits: &Limits) {
    if let Some(bytes) = limits.address_space {
        info.ProcessMemoryLimit = usize::try_from(bytes).unwrap_or(usize::MAX);
        info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
    }
    if let Some(secs) = limits.cpu_seconds {
        info.BasicLimitInformation.PerProcessUserTimeLimit = i64::try_from(secs)
            .unwrap_or(i64::MAX)
            .saturating_mul(HUNDRED_NS_PER_SEC);
        info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_TIME;
    }
    if let Some(max) = limits.active_processes {
        info.BasicLimitInformation.ActiveProcessLimit = max;
        info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
    }
}

#[cfg(test)]
mod tests {
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
    }
}
