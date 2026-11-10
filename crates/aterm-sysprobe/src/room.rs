// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The machine's task tables: what each holds now and the most it may hold, the four pairs the
//! Processes row's Room weighs (`docs/DESIGN-stats-deck-flow-2026-11-02.md` §8.1), and the run
//! queue the read of the thread table answers beside them ([`crate::runq`]).

use crate::runq::RunQueue;

/// What the machine's task tables hold and the most each may hold. Each figure is `None` where
/// this OS does not say it (and a limit where it is unlimited).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TaskRoom {
    /// Processes on the machine: macOS every pid (`proc_listallpids`), Linux every task
    /// (`/proc/loadavg`, since each thread takes a pid there), Windows every process.
    pub procs: Option<u64>,
    /// The most [`Self::procs`] may be: macOS `kern.maxproc`, Linux the smaller of
    /// `kernel.pid_max` and `kernel.threads-max`; `None` on Windows.
    pub procs_max: Option<u64>,
    /// Processes this user runs (macOS `proc_listpids(PROC_RUID_ONLY)`); `None` elsewhere.
    pub user_procs: Option<u64>,
    /// The most one user may run: macOS the smaller of `kern.maxprocperuid` and
    /// `RLIMIT_NPROC`, Linux `RLIMIT_NPROC`; `None` where unlimited, and on Windows.
    pub user_procs_max: Option<u64>,
    /// Threads on the machine: macOS the default processor set's
    /// (`processor_set_statistics(PROCESSOR_SET_LOAD_INFO)`), Linux every task, Windows every
    /// thread.
    pub threads: Option<u64>,
    /// The most [`Self::threads`] may be: macOS `kern.num_threads` (xnu's `thread_max`, a limit
    /// despite its name), Linux `kernel.threads-max`; `None` on Windows.
    pub threads_max: Option<u64>,
    /// Open files on the machine: macOS `kern.num_files`, Linux `fs.file-nr`'s allocated less
    /// its free; `None` on Windows.
    pub files: Option<u64>,
    /// The most [`Self::files`] may be: macOS `kern.maxfiles`, Linux `fs.file-max`; `None` on
    /// Windows.
    pub files_max: Option<u64>,
    /// The machine's run queue from the read that counts its threads ([`crate::runq`]): macOS
    /// the processor set's `load_average` (held to its `mach_factor`), Linux `/proc/loadavg`'s
    /// running count; [`RunQueue::Unsupported`] on Windows.
    pub run_queue: RunQueue,
}

/// The machine's [`TaskRoom`], its per-user pair for real user `uid`.
#[must_use]
pub fn task_room(uid: u32) -> TaskRoom {
    #[cfg(target_os = "macos")]
    {
        imp::task_room(uid)
    }
    #[cfg(target_os = "linux")]
    {
        let _ = uid;
        crate::linux::Root::system().task_room(crate::machine::nproc_rlimit())
    }
    #[cfg(all(windows, target_pointer_width = "64"))]
    {
        let _ = uid;
        crate::win::task_room()
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        all(windows, target_pointer_width = "64")
    )))]
    {
        let _ = uid;
        TaskRoom::default()
    }
}

/// `fs.file-nr` (`allocated free max`) as the files open: the allocated less the free (the
/// kernel has reported 0 free since 2.6, so this is the allocated). `None` for a line that does
/// not read as three counts.
#[must_use]
pub fn file_nr_open(text: &str) -> Option<u64> {
    let mut fields = text.split_whitespace();
    let allocated: u64 = crate::parse_uint(fields.next()?)?;
    let free: u64 = crate::parse_uint(fields.next()?)?;
    crate::parse_uint::<u64>(fields.next()?)?;
    fields
        .next()
        .is_none()
        .then(|| allocated.saturating_sub(free))
}

/// `PROCESSOR_SET_LOAD_INFO` (`mach/processor_info.h`).
#[cfg(target_os = "macos")]
const PROCESSOR_SET_LOAD_INFO: std::ffi::c_int = 4;

/// `struct processor_set_load_info` (`mach/processor_info.h`): four `integer_t`s, of which the
/// reader reads `thread_count`, `load_average` and `mach_factor` (`task_count` keeps its place).
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PsetLoadInfo {
    _task_count: std::ffi::c_int,
    thread_count: std::ffi::c_int,
    load_average: std::ffi::c_int,
    mach_factor: std::ffi::c_int,
}

/// The processor-set layouts and flavor this reader declares, held to the SDK headers by
/// `tests/proc_info_sdk.rs`.
#[cfg(target_os = "macos")]
pub const SDK_ROWS: &[(&str, i64)] = &[
    ("PROCESSOR_SET_LOAD_INFO", PROCESSOR_SET_LOAD_INFO as i64),
    (
        "sizeof(struct processor_set_load_info)",
        std::mem::size_of::<PsetLoadInfo>() as i64,
    ),
    (
        "offsetof(struct processor_set_load_info, thread_count)",
        std::mem::offset_of!(PsetLoadInfo, thread_count) as i64,
    ),
    (
        "offsetof(struct processor_set_load_info, load_average)",
        std::mem::offset_of!(PsetLoadInfo, load_average) as i64,
    ),
    (
        "offsetof(struct processor_set_load_info, mach_factor)",
        std::mem::offset_of!(PsetLoadInfo, mach_factor) as i64,
    ),
    (
        "PROCESSOR_SET_LOAD_INFO_COUNT",
        (std::mem::size_of::<PsetLoadInfo>() / std::mem::size_of::<std::ffi::c_int>()) as i64,
    ),
];

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{CStr, c_int};
    use std::sync::OnceLock;

    use super::{PROCESSOR_SET_LOAD_INFO, PsetLoadInfo, RunQueue, TaskRoom};

    unsafe extern "C" {
        fn processor_set_default(
            host: libc::mach_port_t,
            default_set: *mut libc::mach_port_t,
        ) -> libc::kern_return_t;
        fn processor_set_statistics(
            pset: libc::mach_port_t,
            flavor: c_int,
            info_out: *mut libc::integer_t,
            info_out_cnt: *mut libc::mach_msg_type_number_t,
        ) -> libc::kern_return_t;
    }

    /// The default processor set's name port, taken once per process (each take adds a user
    /// reference, so it is kept rather than taken at every reading).
    fn pset() -> Option<libc::mach_port_t> {
        static PSET: OnceLock<Option<libc::mach_port_t>> = OnceLock::new();
        *PSET.get_or_init(|| {
            let mut pset: libc::mach_port_t = 0;
            // SAFETY: `pset` is a writable port name; the host port is this task's send right.
            let kr = unsafe { processor_set_default(crate::macos::host_port(), &raw mut pset) };
            (kr == 0 && pset != 0).then_some(pset)
        })
    }

    /// The default processor set's load info (macOS has one set), unprivileged through its name
    /// port; `None` where the call fails or answers short.
    fn pset_load() -> Option<PsetLoadInfo> {
        let mut info = PsetLoadInfo::default();
        let words = std::mem::size_of::<PsetLoadInfo>() / std::mem::size_of::<c_int>();
        let mut count = libc::mach_msg_type_number_t::try_from(words).ok()?;
        // SAFETY: `info` is writable for `count` `integer_t`s, the count passed in, and the
        // kernel writes at most that many, answering how many in `count`.
        let kr = unsafe {
            processor_set_statistics(
                pset()?,
                PROCESSOR_SET_LOAD_INFO,
                (&raw mut info).cast::<libc::integer_t>(),
                &raw mut count,
            )
        };
        let whole = usize::try_from(count).ok() == Some(words);
        (kr == 0 && whole).then_some(info)
    }

    /// The most processors the set's averages can count: the machine's logical cores, the one
    /// count [`crate::cpu_layout`] gives (`aterm_capacity`'s and the vitals' too), read once.
    fn cores() -> Option<u64> {
        static CORES: OnceLock<Option<u64>> = OnceLock::new();
        *CORES.get_or_init(|| {
            u64::try_from(crate::cpu_layout().logical_cores())
                .ok()
                .filter(|n| *n > 0)
        })
    }

    /// Every thread on the machine, the set's `thread_count`, and its run queue
    /// ([`crate::runq::of_pset`]), from one read.
    fn threads_and_queue() -> (Option<u64>, RunQueue) {
        let Some(info) = pset_load() else {
            return (None, RunQueue::Failed);
        };
        let threads = u64::try_from(info.thread_count).ok().filter(|n| *n > 0);
        let queue = cores().map_or(RunQueue::Failed, |cores| {
            crate::runq::of_pset(
                i64::from(info.load_average),
                i64::from(info.mach_factor),
                cores,
            )
        });
        (threads, queue)
    }

    pub(super) fn task_room(uid: u32) -> TaskRoom {
        let positive = |name: &CStr| {
            crate::macos::sysctl_int(name)
                .and_then(|v| u64::try_from(v).ok())
                .filter(|v| *v > 0)
        };
        let (procs, user_procs) = crate::macos::pid_counts(uid);
        let (threads, run_queue) = threads_and_queue();
        TaskRoom {
            procs,
            procs_max: positive(c"kern.maxproc"),
            user_procs,
            user_procs_max: crate::machine::smaller(
                positive(c"kern.maxprocperuid"),
                crate::machine::nproc_rlimit(),
            ),
            threads,
            threads_max: positive(c"kern.num_threads"),
            files: crate::macos::sysctl_int(c"kern.num_files").and_then(|v| u64::try_from(v).ok()),
            files_max: positive(c"kern.maxfiles"),
            run_queue,
        }
    }
}

#[cfg(test)]
#[path = "room_tests.rs"]
mod tests;
