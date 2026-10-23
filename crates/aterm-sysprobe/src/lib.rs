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

pub mod caps;
pub mod coalitions;
pub mod identity;
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
/// `KERN_PROCARGS2`, a process's exec path, argument words and initial environment: the one reader ([`procargs::read`], macOS) and the one parser ([`procargs::parse`]) every crate reads it through (aterm-uds re-exports it as `aterm_uds::procargs`).
pub mod procargs;
pub mod procs;
pub mod slots;
pub mod timeline;
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

/// The working directory of process `pid`, read from the OS: `None` where it cannot be read (the process is gone, not ours, or the platform has no reader here). macOS asks `proc_pidinfo(PROC_PIDVNODEPATHINFO)`, Linux resolves `/proc/<pid>/cwd`, Windows reads the process environment block's `CurrentDirectory` ([`peb`]: a process of this account that grants `PROCESS_VM_READ`, never an elevated one), spelled as that process's own `GetCurrentDirectoryW` answers it — no trailing separator but at a root, and an 8.3 short name kept if the process was handed one.
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

/// The executable of process `pid`, read from the OS: `None` where it cannot be read (the process is gone, not ours, or the platform has no reader here). macOS asks `proc_pidpath`, Linux resolves `/proc/<pid>/exe`.
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

/// The memory of `pid`'s whole PROCESS GROUP ([`GroupMemory`]): a Claude Code's Bun process and the children it forks into its own group.
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

/// The machine's physical memory in MiB (`hw.memsize` on macOS, `MemTotal` in `/proc/meminfo` on Linux), `None` where unknown.
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
    /// Available: free, inactive and purgeable pages — memory the kernel can hand out without paging anything out ([`available_mib_of`]).
    pub available_mib: u64,
    /// In use, as Activity Monitor's `Memory Used` counts it ([`used_mib_of`]).
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

/// The machine's memory available and in use now (`HOST_VM_INFO64` on macOS, `/proc/meminfo` on Linux), `None` where unknown.
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

/// Memory available, MiB: free plus inactive plus purgeable pages, from `HOST_VM_INFO64` (`free_count` already holds the speculative pages, which `vm_stat` prints apart).
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

/// The kernel's memory pressure level now (`kern.memorystatus_vm_pressure_level`, one `sysctl`), `None` where unknown (every platform but macOS).
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

/// KiB swapped in plus out since boot (`HOST_VM_INFO64` swapins plus swapouts times `vm.pagesize` on macOS, `/proc/vmstat` `pswpin` plus `pswpout` times the page size on Linux): the counter whose rate the memory ceiling reads (`warn` plus a thrash).
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

/// The bytes of [`read_bounded`], undecoded (`/proc/self/auxv` is binary): at most [`READ_CAP`], one stack buffer.
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

/// Read `path` into `buf`, at most `buf.len()` bytes, never to EOF: how many bytes were read, and whether the file holds more than the buffer (one byte past it is asked for).
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

/// Memory in use, MiB, as Activity Monitor's `Memory Used` counts it: app memory (internal pages less purgeable ones) plus wired plus the pages the compressor occupies, from `HOST_VM_INFO64`.
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

#[cfg(test)]
mod linux_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod windows_tests;
