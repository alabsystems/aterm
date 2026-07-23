// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows actuator for [`crate::Limits`].
//!
//! Two lanes, honest about what each does:
//!
//! * [`apply_limits`] (the [`crate::Limits::apply`] body): a documented,
//!   capability-gated NO-OP. POSIX rlimits do not exist here and there is no
//!   per-process `setrlimit` seam to run before exec (Windows has no `fork`), so
//!   [`crate::rlimits_actuated`] returns `false` and launchers print the one-line
//!   posture notice — an unlimited child is never silent (house rule: never
//!   overstate the security posture).
//! * [`apply_to_job`] (the [`crate::Limits::apply_to_job`] body): the REAL Job
//!   Object confinement lane. Given the Job Object the ConPTY spawn seam already
//!   creates (`aterm-pty`), it folds the requested resource limits (memory, CPU
//!   time, the active-process cap) into the job's extended-limit info and, for a
//!   hardened profile, installs Job Object UI restrictions — so the kernel
//!   enforces them on the child and everything it spawns. This is
//!   query-modify-write, so any `LimitFlags` the caller already set (e.g.
//!   `KILL_ON_JOB_CLOSE`) are preserved.
//!
//! NOTE (wiring): the ConPTY spawn seam does not yet CALL [`apply_to_job`] (it is
//! sketched as the follow-up at the `aterm-pty` job-assignment step), so this
//! lane is dormant until that one-line call lands — the child is only actually
//! confined once the seam invokes it against the suspended child's job.
//!
//! The cap gate in `lib.rs` runs on BOTH entry points (a weak `Cap<Sandbox>` can
//! never actuate — SEC-2).

use std::ffi::c_void;
use std::io;
use std::os::windows::io::RawHandle;

use crate::Limits;

/// Windows: POSIX-style resource limits are NOT actuated by `apply` — return
/// `Ok(())` so the capability-gated spawn proceeds, with the posture surfaced
/// honestly via [`crate::rlimits_actuated`] and the launchers' startup notices.
/// The real Windows resource lane is [`apply_to_job`], invoked at the ConPTY
/// spawn seam against the child's Job Object.
pub(crate) fn apply_limits(_limits: &Limits) -> io::Result<()> {
    Ok(())
}

/// `JobObjectExtendedLimitInformation` — the info class carrying the resource
/// limits (memory / CPU-time) plus the `LimitFlags` bitset.
const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS: i32 = 9;
/// `JOB_OBJECT_LIMIT_PROCESS_TIME`: enforce `PerProcessUserTimeLimit`.
const JOB_OBJECT_LIMIT_PROCESS_TIME: u32 = 0x0000_0002;
/// `JOB_OBJECT_LIMIT_PROCESS_MEMORY`: enforce `ProcessMemoryLimit` (per-process
/// committed-memory ceiling — the closest Windows analog to POSIX `RLIMIT_AS`).
const JOB_OBJECT_LIMIT_PROCESS_MEMORY: u32 = 0x0000_0100;
/// `JOB_OBJECT_LIMIT_ACTIVE_PROCESS`: enforce `ActiveProcessLimit` (max number
/// of concurrently-active processes in the job — a spawn-storm / fork-bomb cap).
const JOB_OBJECT_LIMIT_ACTIVE_PROCESS: u32 = 0x0000_0008;
/// `100`-nanosecond ticks per second, the unit of `PerProcessUserTimeLimit`.
const HUNDRED_NS_PER_SEC: i64 = 10_000_000;

/// `JobObjectBasicUIRestrictions` — the info class carrying the UI-restriction
/// bitset (a separate `SetInformationJobObject` write from the extended limits).
const JOB_OBJECT_BASIC_UI_RESTRICTIONS_CLASS: i32 = 4;
// UI restriction flags (winnt.h) a confined shell must not reach through the
// shared window station.
const JOB_OBJECT_UILIMIT_READCLIPBOARD: u32 = 0x0000_0002;
const JOB_OBJECT_UILIMIT_WRITECLIPBOARD: u32 = 0x0000_0004;
const JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS: u32 = 0x0000_0008;
const JOB_OBJECT_UILIMIT_DISPLAYSETTINGS: u32 = 0x0000_0010;
const JOB_OBJECT_UILIMIT_GLOBALATOMS: u32 = 0x0000_0020;
const JOB_OBJECT_UILIMIT_DESKTOP: u32 = 0x0000_0040;
const JOB_OBJECT_UILIMIT_EXITWINDOWS: u32 = 0x0000_0080;
/// The UI restrictions installed for a hardened job: everything except
/// `JOB_OBJECT_UILIMIT_HANDLES` (which blocks using USER handles created outside
/// the job and can break a shell that launches GUI helpers — the security value
/// of the others is higher and their breakage risk lower for a console child).
const HARDENED_UI_RESTRICTIONS: u32 = JOB_OBJECT_UILIMIT_READCLIPBOARD
    | JOB_OBJECT_UILIMIT_WRITECLIPBOARD
    | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
    | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
    | JOB_OBJECT_UILIMIT_GLOBALATOMS
    | JOB_OBJECT_UILIMIT_DESKTOP
    | JOB_OBJECT_UILIMIT_EXITWINDOWS;

/// Fold the requested resource limits into `job`'s extended-limit info so the
/// kernel enforces them on every process in the job.
///
/// QUERY-MODIFY-WRITE: the current info is read first, so any `LimitFlags` the
/// caller already installed (the ConPTY seam sets `KILL_ON_JOB_CLOSE`) are
/// preserved — this only OR-s in the memory/CPU flags for the fields it sets.
///
/// Mapping (honest scope):
/// * [`Limits::address_space`] → `ProcessMemoryLimit` + `LIMIT_PROCESS_MEMORY`.
/// * [`Limits::cpu_seconds`] → `PerProcessUserTimeLimit` + `LIMIT_PROCESS_TIME`.
/// * [`Limits::active_processes`] → `ActiveProcessLimit` + `LIMIT_ACTIVE_PROCESS`.
/// * [`Limits::restrict_ui`] → a second write of `JobObjectBasicUIRestrictions`
///   locking the job out of the shared window station (clipboard, desktop,
///   display / system parameters, global atoms, `ExitWindows`).
/// * [`Limits::open_files`] / [`Limits::file_size`] have NO Job Object analog and
///   stay unactuated (a handle-count / file-size cap is not a job limit class).
///
/// # Errors
/// The first `QueryInformationJobObject` / `SetInformationJobObject` OS error
/// (e.g. an invalid job handle) — the caller must fail closed.
pub(crate) fn apply_to_job(limits: &Limits, job: RawHandle) -> io::Result<()> {
    // SAFETY: a zeroed extended-limit struct is a valid all-fields-clear value;
    // the query below overwrites it with the job's real state.
    let mut info: JobObjectExtendedLimitInformation = unsafe { std::mem::zeroed() };
    let size = u32::try_from(std::mem::size_of::<JobObjectExtendedLimitInformation>())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "job info size overflow"))?;
    let mut ret_len: u32 = 0;
    // SAFETY: `job` is only read; `info` is a valid out-buffer of `size` bytes and
    // `ret_len` a valid out-param.
    let queried = unsafe {
        QueryInformationJobObject(
            job,
            JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
            std::ptr::addr_of_mut!(info).cast::<c_void>(),
            size,
            &mut ret_len,
        )
    };
    if queried == 0 {
        return Err(io::Error::last_os_error());
    }

    if let Some(bytes) = limits.address_space {
        info.ProcessMemoryLimit = usize::try_from(bytes).unwrap_or(usize::MAX);
        info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
    }
    if let Some(secs) = limits.cpu_seconds {
        // PerProcessUserTimeLimit is in 100 ns ticks; saturate rather than wrap on
        // an absurd (multi-millennia) request so the cap is never silently tiny.
        info.BasicLimitInformation.PerProcessUserTimeLimit = i64::try_from(secs)
            .unwrap_or(i64::MAX)
            .saturating_mul(HUNDRED_NS_PER_SEC);
        info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_TIME;
    }
    if let Some(max) = limits.active_processes {
        info.BasicLimitInformation.ActiveProcessLimit = max;
        info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
    }

    // SAFETY: `job` is a valid job handle and `info` a fully-initialized struct of
    // the stated size.
    let set = unsafe {
        SetInformationJobObject(
            job,
            JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
            std::ptr::addr_of_mut!(info).cast::<c_void>(),
            size,
        )
    };
    if set == 0 {
        return Err(io::Error::last_os_error());
    }

    // UI restrictions ride a SEPARATE info class, so they need their own write
    // AFTER the extended limits land (a failure here still fails closed).
    if limits.restrict_ui {
        apply_ui_restrictions(job)?;
    }
    Ok(())
}

/// Install the [`HARDENED_UI_RESTRICTIONS`] on `job` via a
/// `JobObjectBasicUIRestrictions` write — a separate info class from the
/// extended limits, so it is its own `SetInformationJobObject` call.
///
/// # Errors
/// The `SetInformationJobObject` OS error (e.g. an invalid job handle).
fn apply_ui_restrictions(job: RawHandle) -> io::Result<()> {
    let restrictions = JobObjectBasicUiRestrictions {
        UIRestrictionsClass: HARDENED_UI_RESTRICTIONS,
    };
    let size =
        u32::try_from(std::mem::size_of::<JobObjectBasicUiRestrictions>()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "ui-restrictions size overflow")
        })?;
    // SAFETY: `job` is only read; `restrictions` is a fully-initialized struct of
    // the stated size, read-only for the call's duration.
    let set = unsafe {
        SetInformationJobObject(
            job,
            JOB_OBJECT_BASIC_UI_RESTRICTIONS_CLASS,
            std::ptr::addr_of!(restrictions).cast::<c_void>(),
            size,
        )
    };
    if set == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

// --- Job Object info structs (repr(C), matching the Win32 headers). ---

/// `JOBOBJECT_BASIC_LIMIT_INFORMATION`.
#[repr(C)]
#[allow(non_snake_case)]
struct JobObjectBasicLimitInformation {
    PerProcessUserTimeLimit: i64,
    PerJobUserTimeLimit: i64,
    LimitFlags: u32,
    MinimumWorkingSetSize: usize,
    MaximumWorkingSetSize: usize,
    ActiveProcessLimit: u32,
    Affinity: usize,
    PriorityClass: u32,
    SchedulingClass: u32,
}

/// `IO_COUNTERS`.
#[repr(C)]
#[allow(non_snake_case)]
struct IoCounters {
    ReadOperationCount: u64,
    WriteOperationCount: u64,
    OtherOperationCount: u64,
    ReadTransferCount: u64,
    WriteTransferCount: u64,
    OtherTransferCount: u64,
}

/// `JOBOBJECT_EXTENDED_LIMIT_INFORMATION`.
#[repr(C)]
#[allow(non_snake_case)]
struct JobObjectExtendedLimitInformation {
    BasicLimitInformation: JobObjectBasicLimitInformation,
    IoInfo: IoCounters,
    ProcessMemoryLimit: usize,
    JobMemoryLimit: usize,
    PeakProcessMemoryUsed: usize,
    PeakJobMemoryUsed: usize,
}

/// `JOBOBJECT_BASIC_UI_RESTRICTIONS`.
#[repr(C)]
#[allow(non_snake_case)]
struct JobObjectBasicUiRestrictions {
    UIRestrictionsClass: u32,
}

// Hand-rolled kernel32 binding in the same direct style as
// `aterm-pty::windows::ffi` (std already links kernel32).
#[allow(non_snake_case)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetInformationJobObject(
        hJob: RawHandle,
        JobObjectInformationClass: i32,
        lpJobObjectInformation: *const c_void,
        cbJobObjectInformationLength: u32,
    ) -> i32;
    fn QueryInformationJobObject(
        hJob: RawHandle,
        JobObjectInformationClass: i32,
        lpJobObjectInformation: *mut c_void,
        cbJobObjectInformationLength: u32,
        lpReturnLength: *mut u32,
    ) -> i32;
}

#[cfg(test)]
#[allow(non_snake_case)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateJobObjectW(lpJobAttributes: *mut c_void, lpName: *const u16) -> RawHandle;
    fn CloseHandle(hObject: RawHandle) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RAII wrapper so a test job handle is always closed even on assert-unwind.
    struct TestJob(RawHandle);
    impl TestJob {
        fn create() -> Self {
            // SAFETY: NULL attributes/name → a fresh anonymous job.
            let h = unsafe { CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) };
            assert!(!h.is_null(), "CreateJobObjectW failed");
            TestJob(h)
        }
        fn handle(&self) -> RawHandle {
            self.0
        }
        fn query(&self) -> JobObjectExtendedLimitInformation {
            // SAFETY: zeroed struct is valid out-memory; the query fills it.
            let mut info: JobObjectExtendedLimitInformation = unsafe { std::mem::zeroed() };
            let size = std::mem::size_of::<JobObjectExtendedLimitInformation>() as u32;
            let mut ret: u32 = 0;
            // SAFETY: valid handle + out-buffer of `size` bytes.
            let ok = unsafe {
                QueryInformationJobObject(
                    self.0,
                    JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
                    std::ptr::addr_of_mut!(info).cast::<c_void>(),
                    size,
                    &mut ret,
                )
            };
            assert_ne!(ok, 0, "QueryInformationJobObject failed");
            info
        }
        fn query_ui(&self) -> u32 {
            let mut ui = JobObjectBasicUiRestrictions {
                UIRestrictionsClass: 0,
            };
            let size = std::mem::size_of::<JobObjectBasicUiRestrictions>() as u32;
            let mut ret: u32 = 0;
            // SAFETY: valid handle + out-buffer of `size` bytes.
            let ok = unsafe {
                QueryInformationJobObject(
                    self.0,
                    JOB_OBJECT_BASIC_UI_RESTRICTIONS_CLASS,
                    std::ptr::addr_of_mut!(ui).cast::<c_void>(),
                    size,
                    &mut ret,
                )
            };
            assert_ne!(ok, 0, "QueryInformationJobObject (UI) failed");
            ui.UIRestrictionsClass
        }
    }
    impl Drop for TestJob {
        fn drop(&mut self) {
            // SAFETY: `self.0` is the job handle we created and never closed.
            unsafe { CloseHandle(self.0) };
        }
    }

    #[test]
    fn apply_to_job_installs_memory_and_cpu_limits() {
        let job = TestJob::create();
        let mem = 4u64 * 1024 * 1024 * 1024; // 4 GiB
        let cpu = 90u64; // seconds
        apply_to_job(
            &Limits {
                cpu_seconds: Some(cpu),
                address_space: Some(mem),
                ..Default::default()
            },
            job.handle(),
        )
        .expect("apply_to_job should succeed on a valid job");

        let info = job.query();
        assert_eq!(
            info.ProcessMemoryLimit, mem as usize,
            "ProcessMemoryLimit must equal the requested address space"
        );
        assert_ne!(
            info.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_PROCESS_MEMORY,
            0,
            "LIMIT_PROCESS_MEMORY flag must be set"
        );
        assert_eq!(
            info.BasicLimitInformation.PerProcessUserTimeLimit,
            (cpu as i64) * HUNDRED_NS_PER_SEC,
            "PerProcessUserTimeLimit must be cpu_seconds in 100 ns ticks"
        );
        assert_ne!(
            info.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_PROCESS_TIME,
            0,
            "LIMIT_PROCESS_TIME flag must be set"
        );
    }

    #[test]
    fn apply_to_job_preserves_preexisting_limit_flags() {
        // The ConPTY seam sets KILL_ON_JOB_CLOSE (0x2000) before this runs; the
        // query-modify-write must NOT clear it while adding the memory flag.
        const KILL_ON_JOB_CLOSE: u32 = 0x2000;
        let job = TestJob::create();
        // Pre-set KILL_ON_JOB_CLOSE the way aterm-pty does.
        {
            let mut info: JobObjectExtendedLimitInformation = unsafe { std::mem::zeroed() };
            info.BasicLimitInformation.LimitFlags = KILL_ON_JOB_CLOSE;
            let size = std::mem::size_of::<JobObjectExtendedLimitInformation>() as u32;
            let ok = unsafe {
                SetInformationJobObject(
                    job.handle(),
                    JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
                    std::ptr::addr_of_mut!(info).cast::<c_void>(),
                    size,
                )
            };
            assert_ne!(ok, 0, "pre-set KILL_ON_JOB_CLOSE failed");
        }

        apply_to_job(
            &Limits {
                address_space: Some(2 * 1024 * 1024 * 1024),
                ..Default::default()
            },
            job.handle(),
        )
        .expect("apply_to_job should succeed");

        let flags = job.query().BasicLimitInformation.LimitFlags;
        assert_ne!(
            flags & KILL_ON_JOB_CLOSE,
            0,
            "KILL_ON_JOB_CLOSE must be preserved by the query-modify-write"
        );
        assert_ne!(
            flags & JOB_OBJECT_LIMIT_PROCESS_MEMORY,
            0,
            "the memory flag must be added alongside the preserved flag"
        );
    }

    #[test]
    fn apply_to_job_installs_active_process_cap() {
        let job = TestJob::create();
        apply_to_job(
            &Limits {
                active_processes: Some(64),
                ..Default::default()
            },
            job.handle(),
        )
        .expect("apply_to_job should succeed");

        let info = job.query();
        assert_eq!(
            info.BasicLimitInformation.ActiveProcessLimit, 64,
            "ActiveProcessLimit must equal the requested cap"
        );
        assert_ne!(
            info.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
            0,
            "LIMIT_ACTIVE_PROCESS flag must be set"
        );
    }

    #[test]
    fn apply_to_job_installs_ui_restrictions_when_requested() {
        let job = TestJob::create();
        apply_to_job(
            &Limits {
                restrict_ui: true,
                ..Default::default()
            },
            job.handle(),
        )
        .expect("apply_to_job should succeed");
        assert_eq!(
            job.query_ui(),
            HARDENED_UI_RESTRICTIONS,
            "the hardened UI-restriction bitset must be installed"
        );
    }

    #[test]
    fn apply_to_job_leaves_ui_unrestricted_by_default() {
        let job = TestJob::create();
        // inherit() has restrict_ui = false → no UI-restriction write.
        apply_to_job(&Limits::inherit(), job.handle()).expect("apply_to_job should succeed");
        assert_eq!(
            job.query_ui(),
            0,
            "no UI restrictions requested → the job's window-station access is untouched"
        );
    }

    #[test]
    fn shell_default_hardens_the_job() {
        // The hardened opt-in must actuate memory, cpu-adjacent caps, the
        // active-process bound, AND the UI restrictions in one apply.
        let job = TestJob::create();
        apply_to_job(&Limits::shell_default(), job.handle())
            .expect("apply_to_job with shell_default should succeed");
        let info = job.query();
        assert_ne!(
            info.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
            0,
            "shell_default must bound the active-process count"
        );
        assert_eq!(
            job.query_ui(),
            HARDENED_UI_RESTRICTIONS,
            "shell_default must install the UI restrictions"
        );
    }

    #[test]
    fn apply_to_job_with_no_limits_leaves_flags_untouched() {
        let job = TestJob::create();
        apply_to_job(&Limits::inherit(), job.handle())
            .expect("apply_to_job with all-None limits is a valid no-op write");
        let info = job.query();
        assert_eq!(
            info.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_PROCESS_MEMORY,
            0,
            "no memory limit requested → no memory flag"
        );
        assert_eq!(
            info.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_PROCESS_TIME,
            0,
            "no cpu limit requested → no time flag"
        );
    }

    #[test]
    fn apply_to_job_fails_closed_on_invalid_handle() {
        // A null "job" is not a valid job object; the query must fail and the
        // actuator must surface the error (never silently succeed).
        let err = apply_to_job(&Limits::shell_default(), std::ptr::null_mut()).unwrap_err();
        assert_ne!(
            err.kind(),
            io::ErrorKind::Other,
            "should be a concrete OS error, got {err:?}"
        );
    }
}
