// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A program started to outlive the tab that asked for it: no console, none of this process's
//! handles, and outside the job this process runs in.
//!
//! A tab's shell runs in a job that ends every process it holds when the tab closes
//! (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`), and a child is put in its parent's
//! jobs. A program meant to outlive the tab (the keep-awake holder `aterm acaffeine on` starts)
//! is started instead with `CREATE_BREAKAWAY_FROM_JOB` where this process's job lets a child
//! leave it, and otherwise as the child of the nearest of this process's ancestors that job does
//! not hold (`PROC_THREAD_ATTRIBUTE_PARENT_PROCESS`): Windows puts a new process in the jobs of
//! the parent it is given, not its creator's, and in a tab that ancestor is the aterm window
//! itself. Being that ancestor's child, it is below no tab's shell either, so the sweep that ends
//! a closed tab's programs outside its job ([`super::outside_job`]) never reaches it.
//!
//! An ancestor is named only once vouched for, as the sweep vouches for a child: its
//! kernel-kept parent link (`NtQueryInformationProcess`), and a start no later than its child's,
//! read on a handle held from the check to the creation, so a pid reused since is never named.
//! The program gets the environment and directory it is given, and the token of the parent it
//! is started under.

use std::collections::HashSet;
use std::ffi::{OsString, c_void};
use std::io;
use std::path::Path;

use super::cmdline::{build_command_line, build_env_block, wide_nul};
use super::console_guard;
use super::ffi::{self, HANDLE, Handle};

/// `DETACHED_PROCESS`: the program gets no console.
const DETACHED_PROCESS: u32 = 0x0000_0008;
/// `CREATE_BREAKAWAY_FROM_JOB`: the program leaves the creator's job, where the job allows it.
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
/// `JOB_OBJECT_LIMIT_BREAKAWAY_OK`: a child may ask to leave the job.
const JOB_OBJECT_LIMIT_BREAKAWAY_OK: u32 = 0x0800;
/// `JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK`: every child leaves the job.
const JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK: u32 = 0x1000;
/// `PROC_THREAD_ATTRIBUTE_PARENT_PROCESS`.
const PROC_THREAD_ATTRIBUTE_PARENT_PROCESS: usize = 0x0002_0000;
/// `OpenProcess` right to name a process as a new one's parent.
const PROCESS_CREATE_PROCESS: u32 = 0x0080;
/// `ERROR_MORE_DATA`: a job's process list did not fit.
const ERROR_MORE_DATA: i32 = 234;
/// The most ancestors a walk looks at before it gives up.
const MAX_ANCESTORS: usize = 64;

/// How [`start_outside_job`] started a program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detached {
    /// With `CREATE_BREAKAWAY_FROM_JOB`: this process's job lets a child leave it.
    BrokeAway {
        /// The program's pid.
        pid: u32,
    },
    /// As the child of `parent`, the nearest of this process's ancestors its job does not hold
    /// (in a tab, the aterm window), in that ancestor's jobs.
    Under {
        /// The program's pid.
        pid: u32,
        /// The ancestor it was started under.
        parent: u32,
    },
}

/// Whether this process runs in a job (`IsProcessInJob` with no job named): `false` also when
/// Windows does not answer, as nothing is then known to end it.
#[must_use]
pub fn in_a_job() -> bool {
    let mut held = 0_i32;
    // SAFETY: the current-process pseudo handle, no job (any job), and an out-param.
    let ok = unsafe { ffi::IsProcessInJob(ffi::GetCurrentProcess(), 0, &mut held) };
    ok != 0 && held != 0
}

/// Start `program` with `args` (after its own name), the environment `env` and the working
/// directory `cwd`, detached (no console, none of this process's handles, no standard handles),
/// and outside the job this process runs in (module docs). Nothing waits for it.
///
/// # Errors
///
/// This process is in no job ([`in_a_job`]: nothing to leave, so it is started as any child
/// is); its job lets no child leave and no ancestor outside it could be vouched for or opened
/// to start a child under (one elevated where this process is not, say); or the program did
/// not start.
pub fn start_outside_job(
    program: &Path,
    args: &[OsString],
    env: &[(OsString, OsString)],
    cwd: &Path,
) -> io::Result<Detached> {
    if !in_a_job() {
        return Err(io::Error::other("this process is in no job"));
    }
    let limits = own_job_limits().unwrap_or(0);
    let launch = Launch::new(program, args, env, cwd);
    let mut refused = None;
    if limits & (JOB_OBJECT_LIMIT_BREAKAWAY_OK | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK) != 0 {
        match launch.start(CREATE_BREAKAWAY_FROM_JOB, None) {
            Ok(pid) => return Ok(Detached::BrokeAway { pid }),
            // A job further out may hold its children: start it under an ancestor instead.
            Err(e) => refused = Some(e),
        }
    }
    let job = own_job_pids()?;
    let parent = ancestor_outside(&job).map_err(|why| match refused {
        Some(breakaway) => io::Error::new(why.kind(), format!("{why}; a breakaway: {breakaway}")),
        None => why,
    })?;
    let pid = launch.start(0, Some(parent.process.raw()))?;
    Ok(Detached::Under {
        pid,
        parent: parent.pid,
    })
}

/// What a start needs, encoded once.
struct Launch {
    application: Vec<u16>,
    command_line: Vec<u16>,
    environment: Vec<u16>,
    directory: Vec<u16>,
}

impl Launch {
    fn new(program: &Path, args: &[OsString], env: &[(OsString, OsString)], cwd: &Path) -> Self {
        let mut argv = Vec::with_capacity(args.len() + 1);
        argv.push(program.as_os_str().to_owned());
        argv.extend(args.iter().cloned());
        Self {
            application: wide_nul(program.as_os_str()),
            command_line: build_command_line(&argv),
            environment: build_env_block(env),
            directory: wide_nul(cwd.as_os_str()),
        }
    }

    /// Start it detached, with `flags` beside, as the child of `parent` (a handle with
    /// `PROCESS_CREATE_PROCESS`) or of this process: its pid.
    fn start(&self, flags: u32, parent: Option<HANDLE>) -> io::Result<u32> {
        let mut list = super::AttrList::new(1)?;
        let mut parent_raw = parent.unwrap_or(0);
        if parent.is_some() {
            // SAFETY: an initialized one-slot list; the value points at a handle that outlives
            // the CreateProcessW below, with its size.
            let ok = unsafe {
                ffi::UpdateProcThreadAttribute(
                    list.ptr(),
                    0,
                    PROC_THREAD_ATTRIBUTE_PARENT_PROCESS,
                    std::ptr::addr_of_mut!(parent_raw).cast(),
                    std::mem::size_of::<HANDLE>(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        // SAFETY: a zeroed STARTUPINFOEXW is valid; its size, the null standard handles and
        // the list are set below.
        let mut si: ffi::STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        si.StartupInfo.cb = std::mem::size_of::<ffi::STARTUPINFOEXW>() as u32;
        // No standard handles: a value inherited from no one could name a handle of the
        // program's own.
        si.StartupInfo.dwFlags = ffi::STARTF_USESTDHANDLES;
        si.lpAttributeList = list.ptr();
        // SAFETY: PROCESS_INFORMATION is plain out-memory.
        let mut pi: ffi::PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        let mut command_line = self.command_line.clone();
        let mut environment = self.environment.clone();
        // SAFETY: NUL-terminated application name and directory, a writable NUL-terminated
        // command line, a double-NUL-terminated UTF-16 environment block, and a list and parent
        // handle that live across the call; no handle is inherited.
        let created = unsafe {
            ffi::CreateProcessW(
                self.application.as_ptr(),
                command_line.as_mut_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
                DETACHED_PROCESS
                    | ffi::CREATE_UNICODE_ENVIRONMENT
                    | ffi::EXTENDED_STARTUPINFO_PRESENT
                    | flags,
                environment.as_mut_ptr().cast::<c_void>(),
                self.directory.as_ptr(),
                &si.StartupInfo,
                &mut pi,
            )
        };
        if created == 0 {
            return Err(io::Error::last_os_error());
        }
        drop(Handle::new(pi.hThread));
        drop(Handle::new(pi.hProcess));
        Ok(pi.dwProcessId)
    }
}

/// This process's job's limit flags (`JobObjectExtendedLimitInformation` of its immediate
/// job); `None` when Windows does not say (in no job, say).
fn own_job_limits() -> Option<u32> {
    // SAFETY: a zeroed extended limit record is a valid out-buffer.
    let mut info: ffi::JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: no job handle names this process's own job; a buffer of the stated size.
    let ok = unsafe {
        ffi::QueryInformationJobObject(
            0,
            ffi::JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
            std::ptr::addr_of_mut!(info).cast::<c_void>(),
            std::mem::size_of::<ffi::JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(info.BasicLimitInformation.LimitFlags)
}

/// Every process this process's immediate job holds, the list grown until it is whole.
fn own_job_pids() -> io::Result<HashSet<u32>> {
    // The list's two counts take the first 8 bytes, then one `ULONG_PTR` per process.
    let header = 8 / std::mem::size_of::<usize>();
    let mut room = 256_usize;
    loop {
        let mut buf = vec![0_usize; header + room];
        let bytes = u32::try_from(std::mem::size_of_val(buf.as_slice()))
            .map_err(|_| io::Error::other("this process's job holds too many processes"))?;
        // SAFETY: no job handle names this process's own job; a writable, pointer-aligned
        // buffer of `bytes`.
        let ok = unsafe {
            ffi::QueryInformationJobObject(
                0,
                ffi::JOB_OBJECT_BASIC_PROCESS_ID_LIST_CLASS,
                buf.as_mut_ptr().cast::<c_void>(),
                bytes,
                std::ptr::null_mut(),
            )
        };
        let error = io::Error::last_os_error();
        // SAFETY: the buffer starts with the list's two `u32` counts.
        let (assigned, listed) = unsafe {
            let counts = buf.as_ptr().cast::<u32>();
            (counts.read() as usize, counts.add(1).read() as usize)
        };
        if ok != 0 && listed >= assigned {
            return Ok(buf[header..header + listed.min(room)]
                .iter()
                .map(|&pid| pid as u32)
                .collect());
        }
        if ok == 0 && error.raw_os_error() != Some(ERROR_MORE_DATA) {
            return Err(error);
        }
        room = assigned.max(room).saturating_mul(2);
        if room > 1 << 20 {
            return Err(io::Error::other(
                "this process's job holds too many processes",
            ));
        }
    }
}

/// An ancestor vouched for and opened to start a child under.
struct Ancestor {
    pid: u32,
    /// Opened with `PROCESS_CREATE_PROCESS`.
    process: Handle,
}

/// The nearest of this process's ancestors that `job` (its immediate job's processes) does not
/// hold, each step vouched for (module docs) and the last opened to start a child under.
fn ancestor_outside(job: &HashSet<u32>) -> io::Result<Ancestor> {
    // The current-process pseudo handle needs no closing; `Handle` ignores a 0 only, so it is
    // kept raw.
    // SAFETY: no arguments; the pseudo handle is always valid.
    let own = unsafe { ffi::GetCurrentProcess() };
    let mut child_born = console_guard::started(own)
        .ok_or_else(|| io::Error::other("this process's start cannot be read"))?;
    let mut child_parent = parent_of(own);
    // A step's parent is vouched for by its start: the process that started the child was
    // alive at that start, so a process that took its pid since started after the child.
    for _ in 0..MAX_ANCESTORS {
        let Some(pid) = child_parent.filter(|&pid| pid != 0) else {
            break;
        };
        // SAFETY: plain OpenProcess; the handle is owned by the guard.
        let process = Handle::new(unsafe {
            ffi::OpenProcess(ffi::PROCESS_QUERY_LIMITED_INFORMATION, 0, pid)
        });
        if process.raw() == 0 {
            return Err(io::Error::other(format!(
                "ancestor {pid} cannot be read: {}",
                io::Error::last_os_error()
            )));
        }
        let Some(born) = console_guard::started(process.raw()).filter(|&born| born <= child_born)
        else {
            return Err(io::Error::other(format!(
                "pid {pid} started after its child: the parent is gone"
            )));
        };
        if !job.contains(&pid) {
            // While `process` is held, `pid` names no other process: open it again for the
            // right to start a child under it.
            // SAFETY: plain OpenProcess; the handle is owned by the guard.
            let creator = Handle::new(unsafe {
                ffi::OpenProcess(
                    PROCESS_CREATE_PROCESS | ffi::PROCESS_QUERY_LIMITED_INFORMATION,
                    0,
                    pid,
                )
            });
            if creator.raw() == 0 {
                return Err(io::Error::other(format!(
                    "ancestor {pid}, outside this process's job, refused a child: {}",
                    io::Error::last_os_error()
                )));
            }
            return Ok(Ancestor {
                pid,
                process: creator,
            });
        }
        child_born = born;
        child_parent = parent_of(process.raw());
    }
    Err(io::Error::other(
        "no ancestor outside this process's job was found",
    ))
}

/// The kernel-kept parent of the process behind `process` (a handle with at least
/// `PROCESS_QUERY_LIMITED_INFORMATION`).
fn parent_of(process: HANDLE) -> Option<u32> {
    let mut info = ffi::PROCESS_BASIC_INFORMATION::default();
    // SAFETY: a process handle the caller holds and an out-buffer of the stated size; no
    // return length is asked for.
    let status = unsafe {
        ffi::NtQueryInformationProcess(
            process,
            ffi::PROCESS_BASIC_INFORMATION_CLASS,
            std::ptr::addr_of_mut!(info).cast(),
            std::mem::size_of::<ffi::PROCESS_BASIC_INFORMATION>() as u32,
            std::ptr::null_mut(),
        )
    };
    (status >= 0)
        .then(|| u32::try_from(info.InheritedFromUniqueProcessId).ok())
        .flatten()
}
