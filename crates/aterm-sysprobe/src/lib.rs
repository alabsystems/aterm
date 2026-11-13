// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-sysprobe` — in-process readers of another process and of the machine.

#![warn(missing_docs)]
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_types,
        reason = "test code: a stand-in child may inherit descriptors"
    )
)]

use std::io::Read as _;
use std::path::Path;
use std::time::Instant;

pub mod activity;
pub mod blockio;
#[cfg(target_os = "macos")]
mod blockio_macos;
pub mod caps;
pub mod coalition_heat;
pub mod coalitions;
pub mod cpu_hardware;
pub mod cpu_policy;
pub mod dstate;
pub mod file_events;
mod process_thermal;
pub use process_thermal::{ProcessThermal, ProcessThermalState, process_thermal};
pub mod identity;
pub mod iflist;
#[cfg(target_os = "macos")]
mod iokit;
pub mod jobs;
pub mod kproc;
pub mod linux;
mod machine;
pub mod machine_type;
#[cfg(target_os = "macos")]
mod macos;
pub mod neural_engine;
pub mod notify;
#[cfg(any(target_os = "linux", test))]
mod nvml;
pub mod paths;
#[cfg(windows)]
pub mod peb;
pub mod power;
/// `KERN_PROCARGS2`, a process's exec path, argument words and initial environment: the one reader ([`procargs::read`], macOS) and the one parser ([`procargs::parse`]) every crate reads it through (aterm-uds re-exports it as `aterm_uds::procargs`).
pub mod procargs;
pub mod procs;
pub mod rapl;
pub mod room;
pub mod runq;
pub mod rusage;
pub mod situation;
#[cfg(target_os = "macos")]
mod situation_macos;
pub mod slots;
pub mod timeline;
pub mod volumes;
#[cfg(all(windows, target_pointer_width = "64"))]
mod win;
pub mod win_gpu;
pub mod win_net;
#[cfg(windows)]
mod win_paths;
#[cfg(all(windows, target_pointer_width = "64"))]
mod win_pdh;
pub mod windows;

pub use procs::{
    CoreSplit, ProcessReading, Sweep, SweepOpts, pid_version, processes, processes_keeping,
    processes_with,
};

pub use caps::{Cap, CapSource, Caps, Why};
pub use machine::{
    Capacity, Cluster, Commit, CoreTicks, CpuLayout, GpuClient, GpuClients, GpuDriver, GpuReading,
    LinkCounters, LinkKind, LinkReading, MAX_GPU_CLIENTS, MAX_LINKS, MachineWords, MemoryDetail,
    Probe, TcpCounts, TcpSummary, Throttle, ThrottleLevel, VmCounters, VolumeReading,
    WorkqueueLimits, capacity, caps, counts_toward_machine, cpu_layout, creator_of, gpu,
    gpu_clients, gpu_driver, gpu_if_read, layout_by_rank, level_names, load_average, machine_words,
    memory_detail, net_links, net_links_into, open_socket_count, tcp_after_publish, tcp_counts,
    tcp_counts_into, tcp_summary, tcp_summary_into, volume, withdrawn_cores, workqueue_limits,
};
pub use room::{TaskRoom, task_room};

/// Whether this Mac's kernel offers the hypervisor Virtualization.framework runs on
/// (`kern.hv_support` is 1), read by `sysctlbyname` with no framework loaded: the vz lane's
/// `virtualization` hardware row (`docs/DESIGN-vz-guest-2026-10-23.md` §13). `None` off macOS,
/// or where the kernel has no such name.
#[must_use]
pub fn hv_support() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        macos::sysctl_int(c"kern.hv_support").map(|v| v == 1)
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Whether this macOS runs as a virtual machine's guest (`kern.hv_vmm_present` is 1), read by
/// `sysctlbyname`: an identity aterm's vault makes there is a lab identity
/// (`aterm_secrets::LabMark::VirtualMachine`, `docs/DESIGN-alab-one-mesh-2026-11-05.md` A12).
/// `None` off macOS, or where the kernel has no such name.
#[must_use]
pub fn hv_vmm_present() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        macos::sysctl_int(c"kern.hv_vmm_present").map(|v| v == 1)
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// A path buffer (`PROC_PIDPATHINFO_MAXSIZE`).
pub const PATH_CAP: usize = 4096;
/// Every file read (`/proc`) stops here: 4 KiB.
pub const READ_CAP: usize = 4096;

pub(crate) fn parse_uint<T: std::str::FromStr>(s: &str) -> Option<T> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

pub(crate) fn parse_int<T: std::str::FromStr>(s: &str) -> Option<T> {
    let digits = s.strip_prefix('-').unwrap_or(s);
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// The kernel's memory-pressure level (macOS `kern.memorystatus_vm_pressure_level`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MemoryLevel {
    /// Nothing to reclaim.
    Normal,
    /// The kernel is compressing and swapping.
    Warn,
    /// The kernel is about to kill processes.
    Critical,
}

impl MemoryLevel {
    /// Return the canonical lowercase name (`"normal"`, `"warn"`, `"critical"`).
    #[inline]
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Warn => "warn",
            Self::Critical => "critical",
        }
    }

    /// Return `true` when memory pressure is above `Normal` (`Warn` or `Critical`).
    #[inline]
    #[must_use]
    pub const fn is_elevated(self) -> bool {
        !matches!(self, Self::Normal)
    }

    /// Return `true` when memory pressure is `Critical`.
    #[inline]
    #[must_use]
    pub const fn is_critical(self) -> bool {
        matches!(self, Self::Critical)
    }
}

/// The working directory of process `pid`, read from the OS: `None` where it cannot be read (the process is gone, not ours, or the platform has no reader here). macOS asks `proc_pidinfo(PROC_PIDVNODEPATHINFO)`, Linux resolves `/proc/<pid>/cwd`, Windows reads the process environment block's `CurrentDirectory` (`peb`: a process of this account that grants `PROCESS_VM_READ`, never an elevated one), spelled as that process's own `GetCurrentDirectoryW` answers it — no trailing separator but at a root, and an 8.3 short name kept if the process was handed one.
#[must_use]
pub fn process_cwd(pid: u32) -> Option<std::path::PathBuf> {
    #[cfg(target_os = "macos")]
    {
        macos::process_cwd(pid)
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link(format!("/proc/{pid}/cwd"))
            .ok()
            .filter(|p| p.is_absolute())
    }
    #[cfg(windows)]
    {
        peb::cwd(pid)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = pid;
        None
    }
}

/// This thread's critical-error and open-file system boxes kept off until it drops ([`quiet_errors`]).
#[must_use = "the boxes are off only while the guard lives"]
pub struct QuietErrors {
    /// The thread's mode to restore (Windows).
    #[cfg(windows)]
    _quiet: win_paths::Quiet,
    /// Keeps the guard on the thread that made it.
    _this_thread: std::marker::PhantomData<*const ()>,
}

/// Turn this thread's critical-error and open-file system boxes off until the answer drops: `SetThreadErrorMode(SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX)`, every other bit of the thread's mode kept and the old mode restored on drop — thread-scoped, never the process-wide `SetErrorMode`.
pub fn quiet_errors() -> QuietErrors {
    QuietErrors {
        #[cfg(windows)]
        _quiet: win_paths::quiet(),
        _this_thread: std::marker::PhantomData,
    }
}

/// The 8.3 short spelling of the existing `path` as the file system generated it (Windows' `GetShortPathNameW`: `C:\Users\ANDREW~1\x`), for a test that must start a process in, or report, a short-spelled path (`volumes::Table::long_spelling`'s callers' tests).
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn short_spelling(path: &std::path::Path) -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        win_paths::short_path(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

/// Are this thread's critical-error and open-file system boxes both off now (`GetThreadErrorMode` holds both [`quiet_errors`] bits)? The seam a test reads inside a thread that must hold the guard.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn errors_quiet() -> bool {
    #[cfg(windows)]
    {
        win_paths::errors_quiet()
    }
    #[cfg(not(windows))]
    {
        true
    }
}

/// The executable of process `pid`, read from the OS: `None` where it cannot be read (the process is gone, not ours, or the platform has no reader here). macOS asks `proc_pidpath`, Linux resolves `/proc/<pid>/exe`, Windows asks `QueryFullProcessImageNameW`.
#[must_use]
pub fn process_path(pid: u32) -> Option<std::path::PathBuf> {
    #[cfg(target_os = "macos")]
    {
        macos::process_path(pid)
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link(format!("/proc/{pid}/exe"))
            .ok()
            .filter(|p| p.is_absolute())
    }
    #[cfg(windows)]
    {
        win_paths::process_path(pid)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = pid;
        None
    }
}

/// The kernel's name for process `pid` when it LEADS a process group no other process is in, else `None`: the executable's name as the kernel keeps it (`p_comm` on macOS: `zsh` for a login shell whose `argv[0]` is `-zsh`, the new image's name the moment an `exec` replaces it), read now.
#[must_use]
pub fn lone_group_leader(pid: u32) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        macos::lone_group_leader(pid)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        None
    }
}

/// The members of the process group `leader` leads, other than the leader, whose STANDARD INPUT is the very device the leader's is: the programs the leader handed its terminal to without a group of their own.
#[must_use]
pub fn stdin_sharers(leader: u32) -> Option<Vec<u32>> {
    #[cfg(target_os = "macos")]
    {
        macos::stdin_sharers(leader)
    }
    #[cfg(target_os = "linux")]
    {
        linux_stdin_sharers(leader)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = leader;
        None
    }
}

/// [`stdin_sharers`] on Linux: the leader's children still in its group
/// whose `/proc/<pid>/fd/0` is the leader's own device.
#[cfg(target_os = "linux")]
fn linux_stdin_sharers(leader: u32) -> Option<Vec<u32>> {
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
    let dir = |pid: u32| Path::new("/proc").join(pid.to_string());
    let input = |pid: u32| {
        std::fs::metadata(dir(pid).join("fd").join("0"))
            .ok()
            .filter(|m| m.file_type().is_char_device())
            .map(|m| (m.dev(), m.ino()))
    };
    let group = |pid: u32| with_bounded(&dir(pid).join("stat"), linux::stat_pgrp).flatten();
    if group(leader)? != leader {
        return None;
    }
    let device = input(leader)?;
    let tasks = std::fs::read_dir(dir(leader).join("task")).ok()?;
    let (mut read, mut sharers) = (false, Vec::new());
    for task in tasks.flatten().take(MAX_GROUP_PIDS) {
        let Some(children) = with_bounded(&task.path().join("children"), linux::children) else {
            continue;
        };
        read = true;
        for child in children {
            if !sharers.contains(&child)
                && group(child) == Some(leader)
                && input(child) == Some(device)
            {
                sharers.push(child);
            }
        }
    }
    read.then_some(sharers)
}

/// At most this many members of one process group are measured ([`group_memory`]): a 1 KiB pid buffer.
pub const MAX_GROUP_PIDS: usize = 256;

/// One process group's memory ([`group_memory`]): the PHYSICAL FOOTPRINT of every member this user may read, summed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupMemory {
    /// The process group measured: the group of the pid asked about.
    pub pgid: u32,
    /// The members' physical footprints summed, MiB.
    pub footprint_mib: u64,
    /// How many members were measured (another user's are skipped).
    pub measured: u32,
}

impl GroupMemory {
    /// Encode this `GroupMemory` reading into 16 little-endian bytes (`pgid`, `measured`, `footprint_mib`).
    #[inline]
    #[must_use]
    pub const fn encode_le(self) -> [u8; 16] {
        [
            self.pgid as u8,
            (self.pgid >> 8) as u8,
            (self.pgid >> 16) as u8,
            (self.pgid >> 24) as u8,
            self.measured as u8,
            (self.measured >> 8) as u8,
            (self.measured >> 16) as u8,
            (self.measured >> 24) as u8,
            self.footprint_mib as u8,
            (self.footprint_mib >> 8) as u8,
            (self.footprint_mib >> 16) as u8,
            (self.footprint_mib >> 24) as u8,
            (self.footprint_mib >> 32) as u8,
            (self.footprint_mib >> 40) as u8,
            (self.footprint_mib >> 48) as u8,
            (self.footprint_mib >> 56) as u8,
        ]
    }

    /// Decode 16 little-endian bytes into a `GroupMemory` reading.
    #[inline]
    #[must_use]
    pub const fn decode_le(bytes: [u8; 16]) -> Self {
        let pgid = (bytes[0] as u32)
            | ((bytes[1] as u32) << 8)
            | ((bytes[2] as u32) << 16)
            | ((bytes[3] as u32) << 24);
        let measured = (bytes[4] as u32)
            | ((bytes[5] as u32) << 8)
            | ((bytes[6] as u32) << 16)
            | ((bytes[7] as u32) << 24);
        let footprint_mib = (bytes[8] as u64)
            | ((bytes[9] as u64) << 8)
            | ((bytes[10] as u64) << 16)
            | ((bytes[11] as u64) << 24)
            | ((bytes[12] as u64) << 32)
            | ((bytes[13] as u64) << 40)
            | ((bytes[14] as u64) << 48)
            | ((bytes[15] as u64) << 56);
        Self {
            pgid,
            footprint_mib,
            measured,
        }
    }
}

/// The memory of `pid`'s whole PROCESS GROUP ([`GroupMemory`]): a Claude Code's Bun process and
/// the children it forks into its own group (on Linux their anonymous memory, resident or swapped
/// out, [`linux::Root::group_memory`]). Windows has no process group, so there it is the memory of
/// `pid`'s PROCESS TREE: the private commit of `pid`, of each live process whose chain of live
/// parents reaches it, and of each orphan started in one of that tree's consoles since `pid` was
/// (Windows gives a process whose parent exited no new parent, which is how Git Bash leaves every
/// script it runs; [`windows::tree_adopting`]), from one sweep ([`windows::tree_memory`]), with
/// `pid` as the group's id. A pid Windows handed to a newer process is never read as a child (the
/// sweep checks each parent's creation time). Not counted: an orphan with no console, or one whose
/// console's creator has exited too, so the figure is a floor.
#[must_use]
pub fn group_memory(pid: u32) -> Option<GroupMemory> {
    #[cfg(target_os = "macos")]
    {
        macos::group_memory(pid)
    }
    #[cfg(target_os = "linux")]
    {
        linux::Root::system().group_memory(pid)
    }
    #[cfg(all(windows, target_pointer_width = "64"))]
    {
        win::group_memory(pid)
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        all(windows, target_pointer_width = "64")
    )))]
    {
        let _ = pid;
        None
    }
}

/// What one process has used since it started, as a member of a tree: one definition, which a
/// group's read ([`group_usage`]) and a sweep's row ([`activity::member`]) both give, since the
/// group's read is that of the row its usage record makes; 0 for each figure unread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemberUsage {
    /// Its pid.
    pub pid: u32,
    /// When it started (macOS `ri_proc_start_abstime`, Mach absolute ticks): with [`Self::pid`],
    /// which process this is, so a pid taken anew reads as another member.
    pub start: u64,
    /// Its own user plus system CPU time, ns ([`ProcessReading::cpu_ns`]), not its reaped
    /// children's: summed over a tree's members, a child read before its reap would count again
    /// in its parent's figure (twice more under `P_NOCLDWAIT`), and the reap itself runs the
    /// parent, so its own time grows anyway.
    pub cpu_ns: u64,
    /// Time its threads were runnable, on a core or waiting for one, ns (`ri_runnable_time`).
    pub runnable_ns: u64,
    /// Bytes it read from storage and wrote to storage (macOS
    /// `ri_diskio_bytesread + ri_diskio_byteswritten`, saturating). Not its logical writes: a
    /// `write(2)` runs on its CPU time, which counts it already. On Windows the bytes it moved
    /// through any handle ([`ProcessReading::io_read`] plus [`ProcessReading::io_written`]): the
    /// process record there counts no storage bytes apart.
    pub io_bytes: u64,
    /// Its physical footprint now, bytes (`ri_phys_footprint`): the memory it holds, the pages
    /// the kernel compressed or swapped out included, so the memory it would have resident again.
    pub footprint: u64,
    /// The largest physical footprint it has had since it started, bytes
    /// (`ri_lifetime_max_phys_footprint`), which the kernel keeps between two reads.
    pub peak_footprint: u64,
}

/// What each member of process group `pgid` has used ([`MemberUsage`]), read now: the first
/// [`MAX_GROUP_PIDS`] members the kernel lists, less those it will not answer for (another
/// user's), an unreaped zombie's final figures included. `None` for pgid 0 or past `i32::MAX`,
/// when the listing fails, and on Linux. On Windows `pgid` is a pid and the members are its
/// process tree ([`windows::tree_usage`]: the pid, its live descendants and the orphans in its
/// consoles as [`group_memory`] counts them, from one sweep, each with its CPU time, the bytes
/// it moved through any handle, its private commit and that commit's peak);
/// `None` when the pid is not running.
#[must_use]
pub fn group_usage(pgid: u32) -> Option<Vec<MemberUsage>> {
    #[cfg(target_os = "macos")]
    {
        macos::group_usage(pgid)
    }
    #[cfg(all(windows, target_pointer_width = "64"))]
    {
        win::group_usage(pgid)
    }
    #[cfg(not(any(target_os = "macos", all(windows, target_pointer_width = "64"))))]
    {
        let _ = pgid;
        None
    }
}

/// The Windows session this process runs in (`ProcessIdToSessionId`): what a sweep row's
/// [`ProcessReading::uid`] is compared with there, so a process of the same session is this
/// person's and one of session 0 a service's ([`windows::session_class`]). `None` off Windows,
/// where a process's owner is its uid.
#[must_use]
pub fn own_session() -> Option<u32> {
    #[cfg(all(windows, target_pointer_width = "64"))]
    {
        win::own_session()
    }
    #[cfg(not(all(windows, target_pointer_width = "64")))]
    {
        None
    }
}

/// How long since a person last moved this Mac's mouse or pressed a key (IOKit's `IOHIDSystem`
/// `HIDIdleTime`, read from the IORegistry: no WindowServer connection). `None` where unread, and
/// off macOS.
#[must_use]
pub fn hid_idle() -> Option<std::time::Duration> {
    #[cfg(target_os = "macos")]
    {
        macos::hid_idle()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// The machine's model identifier (macOS `hw.model`: `Mac16,6`; Windows the firmware's product
/// name as the registry keeps it, `SystemProductName`), `None` where unread, and on Linux.
#[must_use]
pub fn machine_model() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        macos::machine_model()
    }
    #[cfg(all(windows, target_pointer_width = "64"))]
    {
        win::machine_model()
    }
    #[cfg(not(any(target_os = "macos", all(windows, target_pointer_width = "64"))))]
    {
        None
    }
}

/// The running OS's build (macOS `kern.osversion`: `25B78`; Windows the build number and its
/// update revision, `26200.7171`), which every update of the OS changes, a beta's included,
/// `None` where unread, and on Linux.
#[must_use]
pub fn os_build() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        macos::os_build()
    }
    #[cfg(all(windows, target_pointer_width = "64"))]
    {
        win::os_build()
    }
    #[cfg(not(any(target_os = "macos", all(windows, target_pointer_width = "64"))))]
    {
        None
    }
}

/// The machine's physical memory in MiB (`hw.memsize` on macOS, `MemTotal` in `/proc/meminfo` on Linux, `GlobalMemoryStatusEx`'s `ullTotalPhys` on Windows), `None` where unknown.
#[must_use]
pub fn physical_memory_mib() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        macos::physical_memory_mib()
    }
    #[cfg(target_os = "linux")]
    {
        let text = read_bounded(Path::new("/proc/meminfo"))?;
        let total_kib = linux::mem_total_kib(&text)?;
        (total_kib >= 1024).then_some(total_kib / 1024)
    }
    #[cfg(all(windows, target_pointer_width = "64"))]
    {
        win::physical_memory_mib()
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        all(windows, target_pointer_width = "64")
    )))]
    {
        None
    }
}

/// The kernel's memory pressure level now: on macOS `kern.memorystatus_vm_pressure_level` (one
/// `sysctl`), on Linux PSI held to `MemAvailable` ([`linux::Root::pressure`]: one read of
/// `/proc/pressure/memory` and one of `/proc/meminfo`), and on Windows the level [`memory_detail`]
/// reads (`warn` once the commit charge is at 90 % of its limit or the low-memory notification is
/// set, else `normal`; Windows names no `critical`); `None` where unknown (Linux without PSI,
/// every other platform).
#[must_use]
pub fn memory_pressure() -> Option<MemoryLevel> {
    #[cfg(target_os = "macos")]
    {
        macos::memory_pressure()
    }
    #[cfg(target_os = "linux")]
    {
        linux::Root::system().pressure().ok()
    }
    #[cfg(all(windows, target_pointer_width = "64"))]
    {
        win::memory_detail().and_then(|m| m.pressure)
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        all(windows, target_pointer_width = "64")
    )))]
    {
        None
    }
}

/// KiB swapped in plus out since boot (`HOST_VM_INFO64` swapins plus swapouts times `vm.pagesize` on macOS, `/proc/vmstat` `pswpin` plus `pswpout` times the page size on Linux): the counter whose rate the memory ceiling reads (`warn` plus a thrash). Windows keeps no such count for a reader without privilege, so there it is the page files' pages in use, in KiB: a level, whose rise is what was paged out less what was let go, so a [`SwapMeter`] over it reads the page files' growth (never more than was written) and 0 while they shrink.
#[must_use]
pub fn swapped_kib() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        macos::swapped_kib()
    }
    #[cfg(target_os = "linux")]
    {
        static PAGE_KIB: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        let page_kib = *PAGE_KIB.get_or_init(|| {
            read_bounded_bytes(Path::new("/proc/self/auxv"))
                .ok()
                .and_then(|b| linux::auxv_value(&b, linux::AT_PAGESZ))
                .map(|b| b / 1024)
                .filter(|k| *k > 0)
                .unwrap_or(4)
        });
        let text = read_bounded(Path::new("/proc/vmstat"))?;
        let pages = linux::swap_pages(&text)?;
        Some(pages.saturating_mul(page_kib))
    }
    #[cfg(all(windows, target_pointer_width = "64"))]
    {
        win::swapped_kib()
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        all(windows, target_pointer_width = "64")
    )))]
    {
        None
    }
}

/// The shortest span a [`SwapMeter`] takes a rate over: readings closer than this answer the last rate (two agents measured a millisecond apart must not read a burst, or nothing, off one page).
pub const SWAP_SPAN_MIN: std::time::Duration = std::time::Duration::from_secs(1);

/// The longest: a rate averaged over more than this says nothing about now, so the meter starts again and reads no rate until the next reading.
pub const SWAP_SPAN_MAX: std::time::Duration = std::time::Duration::from_secs(60);

/// THE SWAP RATE of a caller that reads [`swapped_kib`] at its own times (the memory ceiling reads it with each agent's measure, every few seconds): KiB per second over the span since the last reading at least [`SWAP_SPAN_MIN`] before, never one longer than [`SWAP_SPAN_MAX`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SwapMeter {
    /// The reading the next rate is taken from.
    base: Option<(Instant, u64)>,
    /// The last rate taken, KiB/s.
    rate: Option<u32>,
}

impl SwapMeter {
    /// A meter that has read nothing.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            base: None,
            rate: None,
        }
    }

    /// Read the counter `kib` ([`swapped_kib`]; `None`: unknown) at `at`, and answer the swap rate in KiB/s: the rate since the base reading once [`SWAP_SPAN_MIN`] has passed (the base moves here), the last rate before then, and `None` with no base, an unknown counter, or a base older than [`SWAP_SPAN_MAX`] (the meter starts again from this reading).
    pub fn observe(&mut self, at: Instant, kib: Option<u64>) -> Option<u32> {
        let Some(kib) = kib else {
            *self = Self::new();
            return None;
        };
        let Some((base_at, base_kib)) = self.base else {
            self.base = Some((at, kib));
            return None;
        };
        let span = at.saturating_duration_since(base_at);
        if span > SWAP_SPAN_MAX {
            *self = Self {
                base: Some((at, kib)),
                rate: None,
            };
            return None;
        }
        if span < SWAP_SPAN_MIN {
            return self.rate;
        }
        let ms = u64::try_from(span.as_millis()).unwrap_or(u64::MAX).max(1);
        let rate = kib.saturating_sub(base_kib).saturating_mul(1000) / ms;
        self.rate = Some(u32::try_from(rate).unwrap_or(u32::MAX));
        self.base = Some((at, kib));
        self.rate
    }
}

/// Read at most [`READ_CAP`] bytes of `path` into one stack buffer.
#[must_use]
pub fn read_bounded(path: &Path) -> Option<String> {
    read_bounded_bytes(path)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

pub(crate) fn open_nonblock(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::File::open(path)
    }
}

/// The bytes of [`read_bounded`], undecoded (`/proc/self/auxv` is binary): at most [`READ_CAP`], one stack buffer.
pub(crate) fn read_bounded_bytes(path: &Path) -> Result<Vec<u8>, std::io::ErrorKind> {
    let mut f = open_nonblock(path).map_err(|e| e.kind())?;
    let mut buf = [0u8; READ_CAP];
    let mut n = 0;
    while n < READ_CAP {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.kind()),
        }
    }
    Ok(buf[..n].to_vec())
}

/// Read `path` into `buf`, at most `buf.len()` bytes, never to EOF: how many bytes were read, and whether the file holds more than the buffer (one byte past it is asked for).
pub(crate) fn read_into(path: &Path, buf: &mut [u8]) -> Result<(usize, bool), std::io::ErrorKind> {
    let mut f = open_nonblock(path).map_err(|e| e.kind())?;
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => return Ok((n, false)),
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.kind()),
        }
    }
    let mut one = [0u8; 1];
    loop {
        match f.read(&mut one) {
            Ok(k) => return Ok((n, k > 0)),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.kind()),
        }
    }
}

/// `f` over the text of at most [`READ_CAP`] bytes of `path`, read into one stack buffer ([`read_bounded`] without its heap copy); `None` when the file cannot be read.
pub(crate) fn with_bounded<T>(path: &Path, f: impl FnOnce(&str) -> T) -> Option<T> {
    let mut buf = [0u8; READ_CAP];
    let (n, _) = read_into(path, &mut buf).ok()?;
    Some(f(utf8_prefix(&buf[..n])))
}

/// The longest UTF-8 prefix of `bytes`.
pub(crate) fn utf8_prefix(bytes: &[u8]) -> &str {
    match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => std::str::from_utf8(&bytes[..e.valid_up_to()]).unwrap_or_default(),
    }
}

/// `kern.memorystatus_vm_pressure_level` as a level: 1 normal, 2 warn, 4 critical; anything else unknown.
#[inline]
#[must_use]
pub const fn pressure_of(level: i64) -> Option<MemoryLevel> {
    match level {
        1 => Some(MemoryLevel::Normal),
        2 => Some(MemoryLevel::Warn),
        4 => Some(MemoryLevel::Critical),
        _ => None,
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_live_tests;
#[cfg(test)]
mod linux_tests;
#[cfg(test)]
mod tests;
#[cfg(all(test, windows, target_pointer_width = "64"))]
mod win_live_tests;
#[cfg(test)]
mod windows_tests;
