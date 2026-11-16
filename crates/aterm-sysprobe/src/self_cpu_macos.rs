// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Self-task Mach observations. Every returned send right and array has one owner.

use std::time::Instant;

use super::resources::{Release, ThreadList};
use super::{
    Coverage, CpuCounter, Cut, Operation, ProcessIdentity, ReadError, ThreadReading,
    observe_threads, process_counter, stamp, thread_counter, thread_name,
};

type Port = u32;

const THREAD_IDENTIFIER_INFO: u32 = 4;
const THREAD_EXTENDED_INFO: u32 = 5;
const IDENTIFIER_WORDS: u32 = 6;
const EXTENDED_WORDS: u32 = 28;

/// `thread_identifier_info` (`mach/thread_info.h`). Port names and pthread handles are not IDs.
#[repr(C)]
#[derive(Default)]
struct IdentifierInfo {
    thread_id: u64,
    thread_handle: u64,
    dispatch_qaddr: u64,
}

const _: () = assert!(size_of::<IdentifierInfo>() == 24);
const _: () = assert!(size_of::<libc::proc_threadinfo>() == 112);

pub(super) const SDK_ROWS: &[(&str, i64)] = &[
    ("THREAD_IDENTIFIER_INFO", THREAD_IDENTIFIER_INFO as i64),
    ("THREAD_IDENTIFIER_INFO_COUNT", IDENTIFIER_WORDS as i64),
    (
        "sizeof(struct thread_identifier_info)",
        size_of::<IdentifierInfo>() as i64,
    ),
    (
        "offsetof(struct thread_identifier_info, thread_id)",
        std::mem::offset_of!(IdentifierInfo, thread_id) as i64,
    ),
    ("THREAD_EXTENDED_INFO", THREAD_EXTENDED_INFO as i64),
    ("THREAD_EXTENDED_INFO_COUNT", EXTENDED_WORDS as i64),
    (
        "sizeof(struct thread_extended_info)",
        size_of::<libc::proc_threadinfo>() as i64,
    ),
    (
        "offsetof(struct thread_extended_info, pth_user_time)",
        std::mem::offset_of!(libc::proc_threadinfo, pth_user_time) as i64,
    ),
    (
        "offsetof(struct thread_extended_info, pth_system_time)",
        std::mem::offset_of!(libc::proc_threadinfo, pth_system_time) as i64,
    ),
    (
        "offsetof(struct thread_extended_info, pth_name)",
        std::mem::offset_of!(libc::proc_threadinfo, pth_name) as i64,
    ),
    ("sizeof(((struct thread_extended_info *)0)->pth_name)", 64),
];

unsafe extern "C" {
    static mach_task_self_: Port;
    fn task_threads(task: Port, list: *mut *mut Port, count: *mut u32) -> i32;
    fn thread_info(thread: Port, flavor: u32, info: *mut i32, count: *mut u32) -> i32;
    fn mach_port_deallocate(task: Port, name: Port) -> i32;
}

struct MachRelease {
    task: Port,
}

impl Release for MachRelease {
    fn thread(&self, port: Port) -> Result<(), i32> {
        // SAFETY: the list transfers one send right per entry to this task, released once.
        let code = unsafe { mach_port_deallocate(self.task, port) };
        (code == 0).then_some(()).ok_or(code)
    }

    fn array(&self, address: usize, bytes: usize) -> Result<(), i32> {
        // SAFETY: task_threads transferred this initialized out-of-line mapping to this task.
        let code = unsafe {
            libc::vm_deallocate(
                self.task,
                address as libc::vm_address_t,
                bytes as libc::vm_size_t,
            )
        };
        (code == 0).then_some(()).ok_or(code)
    }
}

fn thread_list() -> Result<ThreadList<MachRelease>, ReadError> {
    let mut list = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: libSystem keeps this task's send right alive for this process's lifetime.
    let task = unsafe { mach_task_self_ };
    // SAFETY: both out-parameters are live slots. On success each entry is a new send right
    // and the array is an out-of-line mapping in this task, both transferred to the guard.
    let code = unsafe { task_threads(task, &raw mut list, &raw mut count) };
    if code != 0 {
        return Err(ReadError::Native {
            operation: Operation::ThreadList,
            code,
        });
    }
    let count = count as usize;
    if (count > 0 && list.is_null()) || count > isize::MAX as usize / size_of::<Port>() {
        return Err(ReadError::Unexpected {
            operation: Operation::ThreadList,
        });
    }
    // SAFETY: a successful task_threads call supplied exactly count initialized port names;
    // this guard owns the array and every right, including entries beyond MAX_THREADS.
    Ok(unsafe { ThreadList::new(list, count, MachRelease { task }) })
}

fn identifier(port: Port) -> Result<u64, ReadError> {
    let mut info = IdentifierInfo::default();
    let mut words = IDENTIFIER_WORDS;
    // SAFETY: info is the flavor's exact writable layout, words counts its u32-sized words.
    let code = unsafe {
        thread_info(
            port,
            THREAD_IDENTIFIER_INFO,
            (&raw mut info).cast(),
            &raw mut words,
        )
    };
    if code != 0 {
        return Err(ReadError::Native {
            operation: Operation::ThreadIdentity,
            code,
        });
    }
    if words != IDENTIFIER_WORDS || info.thread_id == 0 {
        return Err(ReadError::Unexpected {
            operation: Operation::ThreadIdentity,
        });
    }
    Ok(info.thread_id)
}

fn extended(port: Port) -> Result<(Option<String>, CpuCounter), ReadError> {
    // SAFETY: the record consists only of integer fields and a byte array.
    let mut info: libc::proc_threadinfo = unsafe { std::mem::zeroed() };
    let mut words = EXTENDED_WORDS;
    // SAFETY: info has the THREAD_EXTENDED_INFO layout, verified by SDK_ROWS; words is its
    // writable capacity in natural_t-sized words, not bytes.
    let code = unsafe {
        thread_info(
            port,
            THREAD_EXTENDED_INFO,
            (&raw mut info).cast(),
            &raw mut words,
        )
    };
    if code != 0 {
        return Err(ReadError::Native {
            operation: Operation::ThreadCpu,
            code,
        });
    }
    if words != EXTENDED_WORDS {
        return Err(ReadError::Unexpected {
            operation: Operation::ThreadCpu,
        });
    }
    let name = info.pth_name.map(|b| b as u8);
    Ok((
        thread_name(&name),
        thread_counter(info.pth_user_time, info.pth_system_time)?,
    ))
}

fn threads() -> Result<(Vec<ThreadReading>, Coverage), ReadError> {
    let list = thread_list()?;
    let (threads, coverage) = observe_threads(list.ports(), |port| {
        let id = identifier(port).ok()?;
        let (name, cpu) = match extended(port) {
            Ok((name, cpu)) => (name, Some(cpu)),
            Err(_) => (None, None),
        };
        Some(ThreadReading { id, name, cpu })
    });
    // Return errors only after trying every release, never abandoning the remaining rights.
    list.close()?;
    Ok((threads, coverage))
}

fn process(pid: u32, timebase: (u32, u32)) -> Result<(u64, CpuCounter), ReadError> {
    // SAFETY: rusage_info_v4 contains only integer fields and bytes.
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    // SAFETY: info is the writable layout of RUSAGE_INFO_V4. This reads the caller's own PID,
    // never a requested process, and no child time is included in the selected fields.
    let code = unsafe {
        libc::proc_pid_rusage(
            pid as libc::pid_t,
            crate::rusage::RUSAGE_INFO_V4,
            (&raw mut info).cast::<libc::rusage_info_t>(),
        )
    };
    if code != 0 {
        return Err(ReadError::Native {
            operation: Operation::ProcessCpu,
            code: std::io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EIO),
        });
    }
    if info.ri_proc_start_abstime == 0 {
        return Err(ReadError::Identity);
    }
    Ok((
        info.ri_proc_start_abstime,
        process_counter(
            info.ri_user_time,
            info.ri_system_time,
            timebase.0,
            timebase.1,
        )?,
    ))
}

pub(super) fn read(epoch: Instant) -> Result<Cut, ReadError> {
    let begin_ns = stamp(epoch, Instant::now())?;
    let timebase = crate::procs::kernel_timebase().ok_or(ReadError::Timebase)?;
    let pid = std::process::id();
    let version = crate::pid_version(pid).ok_or(ReadError::Identity)?;
    let (start, process_before) = process(pid, timebase)?;
    let (threads, coverage) = threads()?;
    let (end_start, process_after) = process(pid, timebase)?;
    let end_version = crate::pid_version(pid).ok_or(ReadError::Identity)?;
    if start != end_start || version != end_version {
        return Err(ReadError::ProcessChanged);
    }
    Ok(Cut {
        process: ProcessIdentity {
            pid,
            start,
            version,
        },
        begin_ns,
        end_ns: stamp(epoch, Instant::now())?,
        process_before,
        process_after,
        threads,
        coverage,
    })
}

#[cfg(test)]
#[path = "self_cpu_macos_tests.rs"]
mod tests;
