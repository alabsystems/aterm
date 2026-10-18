// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-sysprobe` — in-process readers of another process and of the
//! machine's memory.
//!
//! * [`process_cwd`], [`process_path`] — another process's working directory
//!   and executable, without forking `lsof` (the harness, the Claude Code
//!   footer, `aterm ctl` session queries).
//! * [`group_memory`] — one process group's physical footprint (the agent
//!   memory ceiling, `aterm_agent::supervise::memory`).
//! * [`physical_memory_mib`], [`machine_memory`], [`memory_pressure`],
//!   [`swapped_kib`] and the [`SwapMeter`] — the machine's memory, the
//!   kernel's pressure level and the swap rate the ceiling moves with.
//!
//! # No timer
//!
//! Nothing here wakes itself or holds a thread: every reader answers once,
//! when its caller asks.
//!
//! # Every platform `cfg` lives here
//!
//! The macOS arm (the private `macos` module) reads libproc, `sysctlbyname`
//! and Mach host statistics. The Linux arm reads `/proc` through std alone
//! ([`linux`]'s parsers are pure functions over `&str`, compiled and tested
//! on every platform). Every other platform answers `None`.
//!
//! # Bounded
//!
//! A path is read into a [`PATH_CAP`]-byte buffer. A file read (the Linux
//! arm's `/proc`) goes through [`read_bounded`], one [`READ_CAP`]-byte stack
//! buffer, never to EOF: a device path that never ends would take the
//! machine down.

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

pub mod linux;
#[cfg(target_os = "macos")]
mod macos;

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

/// The working directory of process `pid`, read from the OS: `None` where it
/// cannot be read (the process is gone, not ours, or the platform has no
/// reader here). macOS asks `proc_pidinfo(PROC_PIDVNODEPATHINFO)`, Linux
/// resolves `/proc/<pid>/cwd`. What an agent's session is ABOUT — Claude
/// Code's folder-trust dialog asks for exactly this directory — where the
/// shell reported none (no OSC 7).
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
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        None
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

/// At most this many members of one process group are measured
/// ([`group_memory`]): a 1 KiB pid buffer. An agent's group is its own
/// process and the few helpers it forks without a group of their own.
pub const MAX_GROUP_PIDS: usize = 256;

/// One process group's memory ([`group_memory`]): the PHYSICAL FOOTPRINT of
/// every member this user may read, summed.
///
/// WHY THE FOOTPRINT, NOT THE RESIDENT SET (measured 2026-09-29 on the
/// owner's 128 GB Mac, a live Claude Code): `top` read `MEM 214G CMPRS 187G`
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

/// The memory of `pid`'s whole PROCESS GROUP ([`GroupMemory`]): a Claude
/// Code's Bun process and the children it forks into its own group. A child
/// that took a group of its own (Claude Code's Bash tool starts each shell
/// with one: measured 2026-09-29, every `zsh -c source …/shell-snapshots/…`
/// led its own group) is not counted, so a build the agent runs is not the
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
#[must_use]
pub fn available_mib_of(
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
    Some(physical_mib.map_or(mib, |p| mib.min(p)))
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

// ---------------------------------------------------------------------------
// Pure pieces the platform arms share (compiled everywhere, tested here).
// ---------------------------------------------------------------------------

/// Memory in use, MiB, as Activity Monitor's `Memory Used` counts it: app
/// memory (internal pages less purgeable ones) plus wired plus the pages the
/// compressor occupies, from `HOST_VM_INFO64`. `None` for a zero page size.
#[must_use]
#[cfg(any(target_os = "macos", test))]
pub fn used_mib_of(
    internal: u64,
    purgeable: u64,
    wired: u64,
    compressor: u64,
    page_bytes: u64,
) -> Option<u32> {
    if page_bytes == 0 {
        return None;
    }
    let pages = internal.saturating_sub(purgeable) + wired + compressor;
    u32::try_from(pages.saturating_mul(page_bytes) >> 20).ok()
}

/// `kern.memorystatus_vm_pressure_level` as a level: 1 normal, 2 warn,
/// 4 critical; anything else unknown.
#[must_use]
#[cfg(any(target_os = "macos", test))]
pub fn pressure_of(level: i64) -> Option<MemoryLevel> {
    match level {
        1 => Some(MemoryLevel::Normal),
        2 => Some(MemoryLevel::Warn),
        4 => Some(MemoryLevel::Critical),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
