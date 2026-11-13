// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`super::System::processes`] and [`super::System::process_argv`] on the live system, read in
//! this process with no child: the process table in one read (`KERN_PROC_ALL`,
//! [`aterm_sysprobe::kproc::records`]), each process's executable as the kernel holds it
//! ([`aterm_uds::holders::exe_of`]), and one process's argument words only once its one thread
//! has run ([`aterm_uds::process::thread_ran`]). [`super::System::process_paths`] reads the table
//! through [`paths`].

use super::Process;
#[cfg(any(target_os = "macos", test))]
use aterm_uds::holders::Exe;
use std::ffi::OsString;
use std::path::PathBuf;

/// Every live process the kernel lists ([`listed`]); `None` where the table cannot be read (and
/// off macOS).
pub(super) fn processes() -> Option<Vec<Process>> {
    #[cfg(target_os = "macos")]
    {
        Some(listed(
            aterm_sysprobe::kproc::records()?,
            aterm_uds::holders::exe_of,
        ))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// The rows of one table read: each record's pid, parent, controlling terminal and short name,
/// with the executable `exe_of` names for it. Pid 0 (the kernel's own), a zombie and a process
/// that exited before its executable was read are left out; one the kernel names no executable
/// for keeps `exe: None`.
#[cfg(any(target_os = "macos", test))]
fn listed(
    records: Vec<aterm_sysprobe::kproc::Record>,
    mut exe_of: impl FnMut(u32) -> Exe,
) -> Vec<Process> {
    records
        .into_iter()
        .filter(|record| record.pid > 0 && !record.zombie)
        .filter_map(|record| {
            let exe = match exe_of(record.pid) {
                Exe::At(path) => Some(path),
                Exe::Unnamed => None,
                Exe::Exited => return None,
            };
            Some(Process {
                pid: record.pid,
                ppid: record.ppid,
                terminal: record.tty.is_some(),
                exe,
                comm: record.comm,
            })
        })
        .collect()
}

/// Every listed process's executable ([`super::System::process_paths`]); `None` where the table
/// was not read, or any process in it has no executable the kernel names: one unnamed process
/// keeps a live installer from disappearing into an apparently complete list.
pub(super) fn paths(rows: Option<Vec<Process>>) -> Option<Vec<PathBuf>> {
    rows?.into_iter().map(|row| row.exe).collect()
}

/// `process`'s argument words ([`super::System::process_argv`], [`argv_once_ran`]); `None` off
/// macOS.
pub(super) fn argv(process: &Process) -> Option<Vec<OsString>> {
    #[cfg(target_os = "macos")]
    {
        argv_once_ran(
            process,
            aterm_uds::process::thread_ran,
            aterm_uds::process::read_process_argv,
            aterm_uds::holders::exe_of,
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = process;
        None
    }
}

/// [`argv`] over its readers: `ran` is asked first, and `read` only once it answers `Some(true)`
/// (M29); the words are kept only while `exe_of`, asked after the read, names the executable
/// `process` lists (an exec between the reads leaves no other program's words), as aterm-ctl's
/// census keeps an aterm's. `read` itself keeps them only while the same process holds the pid
/// ([`aterm_uds::process::read_process_argv`]). No read is atomic with an exec: one into a held
/// program after `ran` answered can still hang `read`, which nothing here bounds
/// (`docs/DESIGN-leftover-processes-2026-10-16.md` §6.4).
#[cfg(any(target_os = "macos", test))]
fn argv_once_ran(
    process: &Process,
    ran: impl FnOnce(i32) -> Option<bool>,
    read: impl FnOnce(i32) -> Option<Vec<OsString>>,
    exe_of: impl FnOnce(u32) -> Exe,
) -> Option<Vec<OsString>> {
    let pid = i32::try_from(process.pid).ok()?;
    if ran(pid) != Some(true) {
        return None;
    }
    let words = read(pid)?;
    let same = match exe_of(process.pid) {
        Exe::At(path) => process.exe.as_ref() == Some(&path),
        Exe::Unnamed => process.exe.is_none(),
        Exe::Exited => false,
    };
    same.then_some(words)
}

#[cfg(test)]
#[path = "processes_tests.rs"]
mod tests;
