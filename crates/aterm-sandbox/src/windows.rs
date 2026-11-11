// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows actuator for [`crate::Limits`].

use std::ffi::c_void;
use std::io;
use std::os::windows::io::RawHandle;

use crate::Limits;
#[cfg(test)]
use crate::job_fold::{
    HUNDRED_NS_PER_SEC_TEST as HUNDRED_NS_PER_SEC, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOB_OBJECT_LIMIT_PROCESS_TIME,
};
use crate::job_fold::{JobObjectExtendedLimitInformation, fold_job_limits};

/// No-op: Windows limits are installed by [`apply_to_job`].
pub(crate) fn apply_limits(_limits: &Limits) -> io::Result<()> {
    Ok(())
}

const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS: i32 = 9;

const JOB_OBJECT_BASIC_UI_RESTRICTIONS_CLASS: i32 = 4;
const JOB_OBJECT_UILIMIT_READCLIPBOARD: u32 = 0x0000_0002;
const JOB_OBJECT_UILIMIT_WRITECLIPBOARD: u32 = 0x0000_0004;
const JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS: u32 = 0x0000_0008;
const JOB_OBJECT_UILIMIT_DISPLAYSETTINGS: u32 = 0x0000_0010;
const JOB_OBJECT_UILIMIT_GLOBALATOMS: u32 = 0x0000_0020;
const JOB_OBJECT_UILIMIT_DESKTOP: u32 = 0x0000_0040;
const JOB_OBJECT_UILIMIT_EXITWINDOWS: u32 = 0x0000_0080;
const HARDENED_UI_RESTRICTIONS: u32 = JOB_OBJECT_UILIMIT_READCLIPBOARD
    | JOB_OBJECT_UILIMIT_WRITECLIPBOARD
    | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
    | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
    | JOB_OBJECT_UILIMIT_GLOBALATOMS
    | JOB_OBJECT_UILIMIT_DESKTOP
    | JOB_OBJECT_UILIMIT_EXITWINDOWS;

/// Fold the requested resource limits into `job`'s extended-limit info so the kernel enforces them
/// on every process in the job.
/// # Errors
/// The first `QueryInformationJobObject` / `SetInformationJobObject` OS error (e.g. an invalid job
/// handle) — the caller must fail closed.
pub(crate) fn apply_to_job(limits: &Limits, job: RawHandle) -> io::Result<()> {
    // SAFETY: a zeroed extended-limit struct is a valid all-fields-clear value; the query below
    // overwrites it with the job's real state.
    let mut info: JobObjectExtendedLimitInformation = unsafe { std::mem::zeroed() };
    let size = u32::try_from(std::mem::size_of::<JobObjectExtendedLimitInformation>())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "job info size overflow"))?;
    let mut ret_len: u32 = 0;
    // SAFETY: `job` is only read; `info` is a valid out-buffer of `size` bytes and `ret_len` a
    // valid out-param.
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

    fold_job_limits(&mut info, limits);

    // SAFETY: `job` is a valid job handle and `info` a fully-initialized struct of the stated size.
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

    if limits.restrict_ui {
        apply_ui_restrictions(job)?;
    }
    Ok(())
}

fn apply_ui_restrictions(job: RawHandle) -> io::Result<()> {
    let restrictions = JobObjectBasicUiRestrictions {
        UIRestrictionsClass: HARDENED_UI_RESTRICTIONS,
    };
    let size =
        u32::try_from(std::mem::size_of::<JobObjectBasicUiRestrictions>()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "ui-restrictions size overflow")
        })?;
    // SAFETY: `job` is only read; `restrictions` is a fully-initialized struct of the stated size,
    // read-only for the call's duration.
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

#[repr(C)]
#[allow(non_snake_case)]
struct JobObjectBasicUiRestrictions {
    UIRestrictionsClass: u32,
}

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
#[path = "windows_tests.rs"]
mod tests;
