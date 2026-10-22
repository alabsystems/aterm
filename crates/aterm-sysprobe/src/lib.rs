// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-sysprobe` — in-process readers of another process and of the
//! machine.
//!
//! * [`process_cwd`], [`process_path`] — another process's working directory
//!   and executable, without forking `lsof` (the harness, the Claude Code
//!   footer, `aterm ctl` session queries); on Windows `peb`, the one reader
//!   of what another process was started with and where it works now.
//! * [`group_memory`] — one process group's physical footprint (the agent
//!   memory ceiling, `aterm_agent::supervise::memory`).
//! * [`lone_group_leader`] — the name of a process that leads a group no
//!   other process is in (a file citation's shell in front).
//! * [`stdin_sharers`] — the members of a group that read its leader's own
//!   input device: a program an agent handed its terminal to (a bare
//!   Ctrl-R/Ctrl-S's owner, `aterm-gui`'s `App::front_agent_composer`).
//! * [`physical_memory_mib`], [`machine_memory`], [`memory_pressure`],
//!   [`swapped_kib`] and the [`SwapMeter`] — the machine's memory, the
//!   kernel's pressure level and the swap rate the ceiling moves with.
//! * [`volumes`] — the mount table, a fresh reading of one volume's free
//!   space, and the pure lookups between a path and its mount (the disk
//!   watch, `aterm harness disk`, `status disk=` and `await sys`), and
//!   [`volumes::file_id`], one file's device and number on every platform.
//! * [`paths`] — the one rule that places a path under a directory across
//!   Windows' spellings of one location (verbatim or plain, any case),
//!   pure and tested on every platform.
//! * The system-awareness stats' readers: [`Probe::core_ticks`] (every
//!   logical core's ticks), [`Probe::throttle`] (the throttle level),
//!   [`cpu_layout`] (the clusters the OS names), [`withdrawn_cores`] (the
//!   cores the OS took away), [`memory_detail`] (memory by the OS's classes,
//!   swap, commit, pressure), [`gpu`] (the GPU's device-wide use), [`volume`]
//!   (the volume a path lives on), [`capacity`] (a cgroup's ceiling),
//!   [`net_links`] (every network interface's counters, with
//!   [`counts_toward_machine`] naming the machine's own links;
//!   [`net_links_into`] reads them into a buffer the caller keeps; the
//!   macOS message layouts in [`iflist`]), [`process_limits`] (the process
//!   table's ceilings), [`pid_counts`] (how many processes the machine and
//!   one user run) and [`load_average`] (the kernel's load averages).
//! * [`processes`] — one sweep of the process table: each process's parent,
//!   group, state, CPU time, footprint, disk bytes and name (the stats'
//!   aterm and tab scopes, and the job units of the stuck-process engine);
//!   [`processes_with`] reads each one's resource coalition too.
//! * [`coalitions`] — lane L1, the kernel's resource coalitions (macOS):
//!   every one's CPU time by QoS, bytes read and live footprint, one
//!   process's coalition, and the self-check a caller asks before it trusts
//!   them.
//! * [`jobs`] — the job probe: for each job member the stuck-process engine
//!   names, its run state and threads' states, counters (CPU, system calls,
//!   wakeups, context switches, disk and logical bytes), terminal and its
//!   foreground group, and descriptors (an output file's growth, pipe wait
//!   flags, TCP state, an event queue's interests), with lanes L6 and L7.
//! * [`caps`] — what of all that the running machine can say ([`Caps`]),
//!   and why a figure is unavailable ([`Why`]): never a zero.
//! * [`kproc`] — the kernel's process records as `ps` and `lsof` read them
//!   (groups, terminals, start times, argument words, the mapped image's
//!   inode), without running either (the harness's job reads, `aterm
//!   fabric`'s broker search).
//! * [`slots`] — Trust's `build-slot` flock files, read without a lock:
//!   which a live build holds, which are free, and which a dead wrapper's
//!   orphaned build still occupies (the occupancy facts, `await sys
//!   slots>=…`).
//!
//! # No timer
//!
//! Nothing here wakes itself or holds a thread: every reader answers once,
//! when its caller asks.
//!
//! # Every platform `cfg` lives here
//!
//! The macOS arm (the private `macos` module) reads libproc, `sysctlbyname`,
//! Mach host and processor statistics and the IORegistry (the private
//! `iokit` module's declarations, held to the SDK by `tests/iokit_sdk.rs`).
//! The Linux arm reads `/proc` and `/sys` through std ([`linux`]'s parsers
//! are pure functions over `&str`, and its [`linux::Root`] readers read
//! under any root directory, so both compile and are tested on every
//! platform against fixture trees), and an NVIDIA card's busy figure
//! through NVML (the private `nvml` module, `libnvidia-ml.so.1` opened at
//! run time by aterm-dl), which only the system root asks. The Windows arm
//! (the private `win` module, 64-bit Windows) calls kernel32 and ntdll and
//! hands every buffer to [`windows`]'s decoders, which are pure and tested
//! on every platform.
//! The volumes, file identities, path spellings and other processes'
//! parameters are read through the private `win_paths` module's `kernel32`
//! and `ntdll` declarations (each struct held to the SDK's layout by a test),
//! the readers' assembly pure and tested on every platform. Every other
//! platform, and every reader Windows has no arm for, answers `None`.
//!
//! # Bounded
//!
//! A path is read into a [`PATH_CAP`]-byte buffer. A file read (the Linux
//! arm's `/proc`) goes through [`read_bounded`], one [`READ_CAP`]-byte stack
//! buffer, never to EOF: a device path that never ends would take the
//! machine down. `/proc/stat`, whose per-core lines outgrow 4 KiB on a big
//! machine, is read the same way into a [`linux::STAT_CAP`]-byte buffer the
//! [`Probe`] keeps.

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

pub mod caps;
pub mod coalitions;
pub mod iflist;
#[cfg(target_os = "macos")]
mod iokit;
pub mod jobs;
pub mod kproc;
pub mod linux;
mod machine;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod nvml;
pub mod paths;
#[cfg(windows)]
pub mod peb;
/// `KERN_PROCARGS2`, a process's exec path, argument words and initial environment: the one
/// reader ([`procargs::read`], macOS) and the one parser ([`procargs::parse`]) every crate
/// reads it through (aterm-uds re-exports it as `aterm_uds::procargs`).
pub mod procargs;
pub mod procs;
pub mod slots;
pub mod volumes;
#[cfg(all(windows, target_pointer_width = "64"))]
mod win;
#[cfg(windows)]
mod win_paths;
pub mod windows;

pub use procs::{ProcessReading, Sweep, SweepOpts, processes, processes_with};

pub use caps::{Cap, CapSource, Caps, Why};
pub use machine::{
    Capacity, Cluster, Commit, CoreTicks, CpuLayout, GpuReading, LinkCounters, LinkKind,
    LinkReading, MAX_LINKS, MachineWords, MemoryDetail, Probe, ProcessLimits, Throttle,
    ThrottleLevel, VmCounters, VolumeReading, WorkqueueLimits, capacity, caps,
    counts_toward_machine, cpu_layout, gpu, layout_by_rank, level_names, load_average,
    machine_words, memory_detail, net_links, net_links_into, pid_counts, process_limits, volume,
    withdrawn_cores, workqueue_limits,
};

/// A path buffer (`PROC_PIDPATHINFO_MAXSIZE`).
pub const PATH_CAP: usize = 4096;
/// Every file read (`/proc`) stops here: 4 KiB.
pub const READ_CAP: usize = 4096;

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

/// The working directory of process `pid`, read from the OS: `None` where it
/// cannot be read (the process is gone, not ours, or the platform has no
/// reader here). macOS asks `proc_pidinfo(PROC_PIDVNODEPATHINFO)`, Linux
/// resolves `/proc/<pid>/cwd`, Windows reads the process environment block's
/// `CurrentDirectory` ([`peb`]: a process of this account that grants
/// `PROCESS_VM_READ`, never an elevated one), spelled as that process's own
/// `GetCurrentDirectoryW` answers it — no trailing separator but at a root,
/// and an 8.3 short name kept if the process was handed one. What an agent's
/// session is ABOUT — Claude Code's folder-trust dialog asks for exactly this
/// directory — where the shell reported none (no OSC 7).
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

/// This thread's critical-error and open-file system boxes kept off until it
/// drops ([`quiet_errors`]).
///
/// Neither `Send` nor `Sync`, on every platform: the mode it sets and
/// restores is its OWN thread's. Dropped on another thread, it would leave
/// its own thread quiet for good and hand the other one a mode that was
/// never its own; the compiler refuses that move.
#[must_use = "the boxes are off only while the guard lives"]
pub struct QuietErrors {
    /// The thread's mode to restore (Windows).
    #[cfg(windows)]
    _quiet: win_paths::Quiet,
    /// Keeps the guard on the thread that made it.
    _this_thread: std::marker::PhantomData<*const ()>,
}

/// Turn this thread's critical-error and open-file system boxes off until
/// the answer drops: `SetThreadErrorMode(SEM_FAILCRITICALERRORS |
/// SEM_NOOPENFILEERRORBOX)`, every other bit of the thread's mode kept and
/// the old mode restored on drop — thread-scoped, never the process-wide
/// `SetErrorMode`. On Windows a file-system call on an empty card reader or
/// optical drive (a `canonicalize` of a tab's working directory on `E:\`, a
/// listing that spells a process's 8.3 path long) otherwise puts a "There
/// is no disk in the drive" box on the owner's desktop and parks the asking
/// thread until someone answers it; with the boxes off the call fails with
/// `ERROR_NOT_READY` and the caller's unknown arm answers. A thread that
/// asks file systems about paths other processes and tabs report — the
/// window's disk-watch look and first reading, the path-user scan's naming
/// helper, `aterm harness disk` — holds one for its whole life; every reader
/// of this crate that may reach a file system holds one already. Guards
/// nest: each restores the mode it found, so each drops on its own thread
/// in the reverse order of making ([`QuietErrors`] cannot leave it). Nothing
/// elsewhere: no other platform raises such a box.
pub fn quiet_errors() -> QuietErrors {
    QuietErrors {
        #[cfg(windows)]
        _quiet: win_paths::quiet(),
        _this_thread: std::marker::PhantomData,
    }
}

/// The 8.3 short spelling of the existing `path` as the file system
/// generated it (Windows' `GetShortPathNameW`: `C:\Users\ANDREW~1\x`), for
/// a test that must start a process in, or report, a short-spelled path
/// (`volumes::Table::long_spelling`'s callers' tests). `None` where none was
/// generated (a volume that makes no short names, a name that needs none)
/// or the path does not exist, and on every other platform. A test seam.
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

/// Are this thread's critical-error and open-file system boxes both off now
/// (`GetThreadErrorMode` holds both [`quiet_errors`] bits)? The seam a test
/// reads inside a thread that must hold the guard. `true` off Windows: no
/// other platform raises such a box. A test seam, as [`short_spelling`] is:
/// every caller outside this crate is a test's.
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

/// The executable of process `pid`, read from the OS: `None` where it cannot
/// be read (the process is gone, not ours, or the platform has no reader
/// here). macOS asks `proc_pidpath`, Linux resolves `/proc/<pid>/exe`. In
/// process, so a caller that asks about many pids forks nothing per pid.
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
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        None
    }
}

/// The kernel's name for process `pid` when it LEADS a process group no
/// other process is in, else `None`: the executable's name as the kernel
/// keeps it (`p_comm` on macOS: `zsh` for a
/// login shell whose `argv[0]` is `-zsh`, the new image's name the moment
/// an `exec` replaces it), read now. A citation asks it of a shell in front
/// (`aterm-gui`'s `path_link::runs_here`): a non-interactive shell keeps the
/// children it starts in its own group, so a script whose body is `ssh prod`
/// leads a group the ssh is in, and its name alone says nothing of where its
/// output comes from.
///
/// macOS lists the group (`PROC_PIDT_SHORTBSDINFO`, then
/// `proc_listpids(PROC_PGRP_ONLY)`) and requires a complete, positive listing
/// of the leader alone. Linux has no complete group reader here: a leader's
/// own children miss orphaned grandchildren, so it answers `None`, as do
/// other unsupported platforms. Unreadable or possibly truncated answers
/// also return `None`. This is a snapshot, not a process-lifetime guarantee.
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

/// The members of the process group `leader` leads, other than the leader,
/// whose STANDARD INPUT is the very device the leader's is: the programs the
/// leader handed its terminal to without a group of their own. Claude Code
/// runs `$EDITOR` so (inherited stdio, no `setpgid`), and the editor then
/// reads the keys while the terminal's foreground group is still the one
/// `claude` leads. A helper whose input is a pipe, a file or `/dev/null` (an
/// MCP server, a hook) is not one of them. A snapshot, read now.
///
/// `Some(empty)` when no member shares it. `None` where it cannot be read:
/// `leader` leads no group, its standard input is not a device (a pipe, a
/// file, closed), the group cannot be listed, or the platform has no reader
/// here (every platform but macOS and Linux).
///
/// macOS lists the group (`proc_listpids(PROC_PGRP_ONLY)`, at most
/// [`MAX_GROUP_PIDS`] members) and compares each member's descriptor 0 with
/// the leader's (`proc_pidfdinfo(PROC_PIDFDVNODEPATHINFO)`'s `vst_dev` and
/// `vst_ino`, the layout [`jobs::darwin::vnode`] decodes). Linux has no
/// complete group reader here ([`lone_group_leader`]), so it reads the
/// leader's own children (`/proc/<pid>/task/<tid>/children`,
/// [`linux::children`]) that are still in its group, comparing
/// `/proc/<pid>/fd/0`'s device and inode: a grandchild counts only through a
/// child that shares the input too (a `sh -c "$EDITOR …"` does), and a kernel
/// without those files answers `None`.
///
/// Cost on macOS: one short info, one group listing and one descriptor read
/// per member; no sweep of the process table, no subprocess.
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

/// At most this many members of one process group are measured
/// ([`group_memory`]): a 1 KiB pid buffer. An agent's group is its own
/// process and the few helpers it forks without a group of their own.
pub const MAX_GROUP_PIDS: usize = 256;

/// One process group's memory ([`group_memory`]): the PHYSICAL FOOTPRINT of
/// every member this user may read, summed.
///
/// Why the footprint, not the resident set (measured on a 128 GB Mac, a live
/// Claude Code): `top` read `MEM 214G CMPRS 187G`
/// for a process whose `ps` RSS was 29.8 GiB. The compressor and swap hold
/// most of a runaway agent's pages, so the resident set UNDER-reads it
/// sevenfold exactly when it matters; the footprint (`ri_phys_footprint`,
/// Activity Monitor's `Memory`, what the kernel's own memory limits count)
/// includes them.
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

/// The memory of `pid`'s whole PROCESS GROUP ([`GroupMemory`]): a Claude
/// Code's Bun process and the children it forks into its own group. A child
/// that took a group of its own (Claude Code's Bash tool starts each shell
/// with one: every `zsh -c source …/shell-snapshots/…` leads its own group)
/// is not counted, so a build the agent runs is not the
/// agent's memory. `None` where it cannot be read: the pid is gone or not
/// ours, the platform has no reader here (every platform but macOS).
///
/// Cost: one `PROC_PIDT_SHORTBSDINFO`, one `proc_listpids(PROC_PGRP_ONLY)`
/// and one `proc_pid_rusage` per member — no sweep of the process table, no
/// subprocess.
#[must_use]
pub fn group_memory(pid: u32) -> Option<GroupMemory> {
    #[cfg(target_os = "macos")]
    {
        macos::group_memory(pid)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        None
    }
}

/// The machine's physical memory in MiB (`hw.memsize` on macOS, `MemTotal` in
/// `/proc/meminfo` on Linux), `None` where unknown.
#[must_use]
pub fn physical_memory_mib() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        macos::physical_memory_mib()
    }
    #[cfg(target_os = "linux")]
    {
        let text = read_bounded(Path::new("/proc/meminfo"))?;
        let m = linux::meminfo(&text)?;
        (m.total_kib >= 1024).then_some(m.total_kib / 1024)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// The machine's memory now ([`machine_memory`]), MiB.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MachineMemory {
    /// Available: free, inactive and purgeable pages — memory the kernel can
    /// hand out without paging anything out ([`available_mib_of`]).
    pub available_mib: u64,
    /// In use, as Activity Monitor's `Memory Used` counts it
    /// ([`used_mib_of`]).
    pub used_mib: u64,
}

impl MachineMemory {
    /// Return `available_mib.saturating_add(used_mib)`.
    #[inline]
    #[must_use]
    pub const fn total_estimated_mib(self) -> u64 {
        self.available_mib.saturating_add(self.used_mib)
    }

    /// Encode this `MachineMemory` reading into 16 little-endian bytes (`available_mib`, `used_mib`).
    #[inline]
    #[must_use]
    pub const fn encode_le(self) -> [u8; 16] {
        [
            self.available_mib as u8,
            (self.available_mib >> 8) as u8,
            (self.available_mib >> 16) as u8,
            (self.available_mib >> 24) as u8,
            (self.available_mib >> 32) as u8,
            (self.available_mib >> 40) as u8,
            (self.available_mib >> 48) as u8,
            (self.available_mib >> 56) as u8,
            self.used_mib as u8,
            (self.used_mib >> 8) as u8,
            (self.used_mib >> 16) as u8,
            (self.used_mib >> 24) as u8,
            (self.used_mib >> 32) as u8,
            (self.used_mib >> 40) as u8,
            (self.used_mib >> 48) as u8,
            (self.used_mib >> 56) as u8,
        ]
    }

    /// Decode 16 little-endian bytes into a `MachineMemory` reading.
    #[inline]
    #[must_use]
    pub const fn decode_le(bytes: [u8; 16]) -> Self {
        let available_mib = (bytes[0] as u64)
            | ((bytes[1] as u64) << 8)
            | ((bytes[2] as u64) << 16)
            | ((bytes[3] as u64) << 24)
            | ((bytes[4] as u64) << 32)
            | ((bytes[5] as u64) << 40)
            | ((bytes[6] as u64) << 48)
            | ((bytes[7] as u64) << 56);
        let used_mib = (bytes[8] as u64)
            | ((bytes[9] as u64) << 8)
            | ((bytes[10] as u64) << 16)
            | ((bytes[11] as u64) << 24)
            | ((bytes[12] as u64) << 32)
            | ((bytes[13] as u64) << 40)
            | ((bytes[14] as u64) << 48)
            | ((bytes[15] as u64) << 56);
        Self {
            available_mib,
            used_mib,
        }
    }
}

/// The machine's memory available and in use now (`HOST_VM_INFO64` on macOS,
/// `/proc/meminfo` on Linux), `None` where unknown. What the memory ceiling
/// moves with (`aterm_agent::supervise::memory`).
#[must_use]
pub fn machine_memory() -> Option<MachineMemory> {
    #[cfg(target_os = "macos")]
    {
        macos::machine_memory()
    }
    #[cfg(target_os = "linux")]
    {
        let text = read_bounded(Path::new("/proc/meminfo"))?;
        let m = linux::meminfo(&text)?;
        let total_mib = m.total_kib / 1024;
        let available_mib = (m.available_kib / 1024).min(total_mib);
        let used_mib = m.total_kib.saturating_sub(m.available_kib) / 1024;
        Some(MachineMemory {
            available_mib,
            used_mib,
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// Memory available, MiB: free plus inactive plus purgeable pages, from
/// `HOST_VM_INFO64` (`free_count` already holds the speculative pages,
/// which `vm_stat` prints apart). Never more than `physical_mib` where that
/// is known: a purgeable page may also sit on the inactive queue, so the sum
/// can count one twice. `None` for a zero page size.
#[inline]
#[must_use]
pub const fn available_mib_of(
    free: u64,
    inactive: u64,
    purgeable: u64,
    page_bytes: u64,
    physical_mib: Option<u64>,
) -> Option<u64> {
    if page_bytes == 0 {
        return None;
    }
    let pages = free.saturating_add(inactive).saturating_add(purgeable);
    let mib = pages.saturating_mul(page_bytes) >> 20;
    let clamped = match physical_mib {
        Some(p) if p < mib => p,
        _ => mib,
    };
    Some(clamped)
}

/// The kernel's memory pressure level now
/// (`kern.memorystatus_vm_pressure_level`, one `sysctl`), `None` where
/// unknown (every platform but macOS).
#[must_use]
pub fn memory_pressure() -> Option<MemoryLevel> {
    #[cfg(target_os = "macos")]
    {
        macos::memory_pressure()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// KiB swapped in plus out since boot (`HOST_VM_INFO64` swapins plus
/// swapouts times `vm.pagesize` on macOS, `/proc/vmstat` `pswpin` plus
/// `pswpout` times the page size on Linux): the counter whose rate the memory
/// ceiling reads (`warn` plus a thrash). A [`SwapMeter`] turns it into a rate.
/// `None` where unknown.
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
                .and_then(|b| linux::auxv_value(&b, 6))
                .map(|b| b / 1024)
                .filter(|k| *k > 0)
                .unwrap_or(4)
        });
        let text = read_bounded(Path::new("/proc/vmstat"))?;
        let pages = linux::swap_pages(&text)?;
        Some(pages.saturating_mul(page_kib))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// The shortest span a [`SwapMeter`] takes a rate over: readings closer
/// than this answer the last rate (two agents measured a millisecond apart
/// must not read a burst, or nothing, off one page).
pub const SWAP_SPAN_MIN: std::time::Duration = std::time::Duration::from_secs(1);

/// The longest: a rate averaged over more than this says nothing about now,
/// so the meter starts again and reads no rate until the next reading.
pub const SWAP_SPAN_MAX: std::time::Duration = std::time::Duration::from_secs(60);

/// THE SWAP RATE of a caller that reads [`swapped_kib`] at its own times
/// (the memory ceiling reads it with each agent's measure, every few
/// seconds): KiB per second over the span since the last reading at least
/// [`SWAP_SPAN_MIN`] before, never one longer than [`SWAP_SPAN_MAX`]. Pure
/// (every instant is the caller's), so the host keeps one and feeds it.
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

    /// Read the counter `kib` ([`swapped_kib`]; `None`: unknown) at `at`,
    /// and answer the swap rate in KiB/s: the rate since the base reading
    /// once [`SWAP_SPAN_MIN`] has passed (the base moves here), the last
    /// rate before then, and `None` with no base, an unknown counter, or a
    /// base older than [`SWAP_SPAN_MAX`] (the meter starts again from this
    /// reading). A counter that went backwards reads a rate of zero.
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

/// Read at most [`READ_CAP`] bytes of `path` into one stack buffer. `None`
/// when it cannot be opened or read. A file longer than the cap is cut there
/// (the parsers read their facts from the head and ignore a torn last line);
/// a device that never ends costs 4 KiB, not the machine.
#[must_use]
pub fn read_bounded(path: &Path) -> Option<String> {
    read_bounded_bytes(path)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// The bytes of [`read_bounded`], undecoded (`/proc/self/auxv` is binary):
/// at most [`READ_CAP`], one stack buffer.
pub(crate) fn read_bounded_bytes(path: &Path) -> Result<Vec<u8>, std::io::ErrorKind> {
    let mut f = std::fs::File::open(path).map_err(|e| e.kind())?;
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

/// Read `path` into `buf`, at most `buf.len()` bytes, never to EOF: how many
/// bytes were read, and whether the file holds more than the buffer (one
/// byte past it is asked for). No heap: the caller owns the buffer.
pub(crate) fn read_into(path: &Path, buf: &mut [u8]) -> Result<(usize, bool), std::io::ErrorKind> {
    let mut f = std::fs::File::open(path).map_err(|e| e.kind())?;
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

/// `f` over the text of at most [`READ_CAP`] bytes of `path`, read into one
/// stack buffer ([`read_bounded`] without its heap copy); `None` when the
/// file cannot be read. Bytes that are not UTF-8 end the text where they
/// start. Compiled everywhere: [`linux::links_under_into`] reads a fixture root
/// through it on any host.
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

// ---------------------------------------------------------------------------
// Pure pieces the platform arms share (compiled everywhere, tested here).
// ---------------------------------------------------------------------------

/// Memory in use, MiB, as Activity Monitor's `Memory Used` counts it: app
/// memory (internal pages less purgeable ones) plus wired plus the pages the
/// compressor occupies, from `HOST_VM_INFO64`. `None` for a zero page size.
#[inline]
#[must_use]
pub const fn used_mib_of(
    internal: u64,
    purgeable: u64,
    wired: u64,
    compressor: u64,
    page_bytes: u64,
) -> Option<u32> {
    if page_bytes == 0 {
        return None;
    }
    let pages = internal
        .saturating_sub(purgeable)
        .saturating_add(wired)
        .saturating_add(compressor);
    let mib = pages.saturating_mul(page_bytes) >> 20;
    if mib <= (u32::MAX as u64) {
        Some(mib as u32)
    } else {
        None
    }
}

/// `kern.memorystatus_vm_pressure_level` as a level: 1 normal, 2 warn,
/// 4 critical; anything else unknown.
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

#[cfg(test)]
mod linux_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod windows_tests;
