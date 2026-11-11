// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

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
    const KILL_ON_JOB_CLOSE: u32 = 0x2000;
    let job = TestJob::create();
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
        HARDENED_UI_RESTRICTIONS, 0x0000_00FE,
        "hardened UI bitset must pin the 7 Win32 UILIMIT flags (0xFE)"
    );
    assert_eq!(
        job.query_ui(),
        HARDENED_UI_RESTRICTIONS,
        "the hardened UI-restriction bitset must be installed"
    );
}

#[test]
fn apply_to_job_leaves_ui_unrestricted_by_default() {
    let job = TestJob::create();
    apply_to_job(&Limits::inherit(), job.handle()).expect("apply_to_job should succeed");
    assert_eq!(
        job.query_ui(),
        0,
        "no UI restrictions requested → the job's window-station access is untouched"
    );
}

#[test]
fn shell_default_hardens_the_job() {
    let job = TestJob::create();
    apply_to_job(&Limits::shell_default(), job.handle())
        .expect("apply_to_job with shell_default should succeed");
    let info = job.query();
    assert_ne!(
        info.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_PROCESS_MEMORY,
        0,
        "shell_default must bound per-process memory on Windows"
    );
    assert_eq!(
        info.ProcessMemoryLimit,
        16 * 1024 * 1024 * 1024,
        "shell_default must set a 16 GiB ProcessMemoryLimit"
    );
    assert_ne!(
        info.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
        0,
        "shell_default must bound the active-process count"
    );
    assert_eq!(
        info.BasicLimitInformation.ActiveProcessLimit, 512,
        "shell_default must cap ActiveProcessLimit at 512"
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

/// NEGATIVE CONTROL: `Limits::apply_to_job` rejects an `Untrusted` capability with
/// `PermissionDenied` before touching the invalid handle, and fails closed with
/// `ERROR_INVALID_HANDLE` (`Some(6)`) once authorized.
#[test]
fn apply_to_job_fails_closed_on_invalid_handle() {
    let auth = unsafe { aterm_cap::Authority::root_authority() };
    let weak: aterm_cap::Cap<crate::Sandbox> = auth.grant(aterm_cap::Tier::Untrusted);
    let denied = Limits::shell_default()
        .apply_to_job(&weak, std::ptr::null_mut())
        .unwrap_err();
    assert_eq!(
        denied.kind(),
        io::ErrorKind::PermissionDenied,
        "cap gate must reject Untrusted before querying the null job handle"
    );

    let trusted: aterm_cap::Cap<crate::Sandbox> = auth.grant(aterm_cap::Tier::Trusted);
    let err = Limits::shell_default()
        .apply_to_job(&trusted, std::ptr::null_mut())
        .unwrap_err();
    assert_eq!(
        err.raw_os_error(),
        Some(6),
        "null job handle must fail with ERROR_INVALID_HANDLE (6), got {err:?}"
    );
}
