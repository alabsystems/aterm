// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-sysprobe` — the strain row's samplers (design §10.14, ruling 210).
//!
//! The strain engine (`aterm_messages::strain`) decides whether very heavy
//! system use is worth a row on the band and what it is called. It never
//! reads the machine. This crate does, and nothing else:
//!
//! * [`Probe::reading`] — one [`Reading`]: CPU busy ticks, memory pressure and
//!   level, pages swapped, heat, Low Power Mode. Counters are cumulative; the
//!   engine takes the deltas.
//! * [`Probe::scan`] — one sweep of the process table as [`ProcRow`]s:
//!   cumulative CPU time and footprint for every pid this user may read, and
//!   `None` for the rest ([`Usage::Refused`]: another user's process — their
//!   time is the services', by subtraction, never a daemon's name).
//!
//! # No timer
//!
//! Nothing here wakes itself. The host owns the one deadline
//! (`StrainTracker::next_sample`) and calls `reading`/`scan` on its probe thread
//! only while the engine is Suspect or Open; a calm machine is never sampled.
//!
//! # Every platform `cfg` lives here
//!
//! So the host adds none. The macOS arm (the private `macos` module) reads
//! Mach host statistics, `sysctlbyname`, libproc and `NSProcessInfo`. The
//! Linux arm ([`linux::Sampler`]) reads `/proc` and the thermal zones under
//! a configurable root, so it is tested here against a fixture tree; a Linux
//! build samples the live `/proc` and `/sys` (ruling 227). Every other
//! platform reads a [`Reading`] with every field `None` (and a `None` field
//! is never heavy) and an empty sweep. Windows reads nothing.
//!
//! # Bounded
//!
//! A sweep takes at most [`MAX_PIDS`] pids. A path is read into a
//! [`PATH_CAP`]-byte buffer, once per process image (cached, and the cache
//! given to every row each sweep), and only for rows that can matter:
//! [`NAMED_ROWS`] by CPU and as many by footprint, then any past
//! [`NAME_FLOOR_CPU_NS`] or [`NAME_FLOOR_KIB`], at most [`MAX_PATH_READS`]
//! new reads a sweep. A file read (the Linux arm's `/proc`) goes through
//! [`read_bounded`], one [`READ_CAP`]-byte stack buffer, never to EOF: a
//! device path that never ends would take the machine down.

#![warn(missing_docs)]
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_types,
        reason = "test code: a stand-in child may inherit descriptors"
    )
)]

use std::collections::HashMap;
use std::io::Read as _;
use std::path::Path;

pub use aterm_messages::Instant;
pub use aterm_messages::strain::{MemoryLevel, ProcRow, Psi, Reading, Thermal};

pub mod linux;
#[cfg(target_os = "macos")]
mod macos;

/// At most this many pids per sweep (a 16 KiB pid buffer on macOS).
pub const MAX_PIDS: usize = 4096;
/// The rows whose executable path is read first each sweep: the top this
/// many by CPU since the last sweep, and the top this many by footprint.
pub const NAMED_ROWS: usize = 8;
/// Past those, every row above a floor — this much CPU since the last sweep
/// (50 milli-cores over a 4 s sweep)…
pub const NAME_FLOOR_CPU_NS: u64 = 200_000_000;
/// …or this footprint (64 MiB) — has its path read too, so an app's helpers
/// are summed into the app however many there are (a browser runs dozens).
pub const NAME_FLOOR_KIB: u64 = 64 * 1024;
/// New path reads per sweep, at most; a path is read once per process image
/// and cached, so a steady machine reads none.
pub const MAX_PATH_READS: usize = 64;
/// A path buffer (`PROC_PIDPATHINFO_MAXSIZE`).
pub const PATH_CAP: usize = 4096;
/// Every file read (`/proc`, thermal zones) stops here: 4 KiB.
pub const READ_CAP: usize = 4096;

/// The machine's samplers. Holds only what the next reading needs (the tick
/// accumulator, the last sweep's CPU times, a small path cache); no thread,
/// no timer. `Send`, so the host can build it on its probe thread or move it
/// there.
pub struct Probe {
    #[cfg(target_os = "macos")]
    imp: macos::Mac,
    #[cfg(target_os = "linux")]
    imp: linux::Sampler,
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    cores: u16,
}

impl Default for Probe {
    fn default() -> Self {
        Self::new()
    }
}

impl Probe {
    /// Read the fixed facts once (cores, physical memory, page size, the
    /// timebase). Cheap; samples nothing that changes.
    #[must_use]
    pub fn new() -> Self {
        Self {
            #[cfg(target_os = "macos")]
            imp: macos::Mac::new(),
            #[cfg(target_os = "linux")]
            imp: linux::Sampler::new(linux::Roots::live()),
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            cores: logical_cores(),
        }
    }

    /// One reading of the machine, stamped when it was taken. On a platform
    /// with no sampler (neither macOS nor Linux) every field is `None`.
    ///
    /// Not `read`: the lock-order census takes a zero-argument `.read()` for
    /// `RwLock::read`, and two in one scope for a re-entrant lock.
    #[must_use]
    pub fn reading(&mut self) -> Reading {
        #[cfg(target_os = "macos")]
        {
            self.imp.reading()
        }
        #[cfg(target_os = "linux")]
        {
            self.imp.reading(Instant::now())
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let mut r = Reading::empty(Instant::now());
            r.cores = self.cores;
            r
        }
    }

    /// One sweep of the process table (cumulative CPU times; the engine takes
    /// the deltas). Empty on a platform with no sampler (neither macOS nor
    /// Linux).
    #[must_use]
    pub fn scan(&mut self) -> Vec<ProcRow> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            self.imp.scan()
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Vec::new()
        }
    }
}

/// The platform's name for busy time no visible process accounts for — the
/// strain engine's `StrainConfig::services_noun`: `macOS services` on macOS,
/// `system services` everywhere else. Here so the host carries no platform
/// `cfg` of its own.
pub const SERVICES_NOUN: &str = if cfg!(target_os = "macos") {
    aterm_messages::strain::MACOS_SERVICES
} else {
    aterm_messages::strain::SYSTEM_SERVICES
};

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
/// `pswpout` times the page size on Linux): the counter whose rate the strain
/// row prints as `swap N MB/s` ([`Reading::swap_pages`] is the same count in
/// pages). A [`SwapMeter`] turns it into a rate. `None` where unknown.
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
                .map(|b| (b / 1024) as u64)
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

/// Logical cores, `0` where unknown.
#[must_use]
pub fn logical_cores() -> u16 {
    std::thread::available_parallelism().map_or(0, |n| u16::try_from(n.get()).unwrap_or(u16::MAX))
}

/// Read at most [`READ_CAP`] bytes of `path` into one stack buffer. `None`
/// when it cannot be opened or read. A file longer than the cap is cut there
/// (the parsers read their facts from the head and ignore a torn last line);
/// a device that never ends costs 4 KiB, not the machine.
#[must_use]
pub fn read_bounded(path: &Path) -> Option<String> {
    read_bounded_kind(path).ok()
}

/// [`read_bounded`], saying why a read failed: a `/proc/<pid>` file the
/// kernel refuses (`PermissionDenied`: another user's process under
/// `hidepid`) keeps its row unmeasured, one that is gone (`NotFound`) has
/// none.
pub(crate) fn read_bounded_kind(path: &Path) -> Result<String, std::io::ErrorKind> {
    Ok(String::from_utf8_lossy(&read_bounded_bytes(path)?).into_owned())
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

/// Mach absolute-time ticks to nanoseconds: `ticks × numer / denom`, in 128
/// bits, saturating. On Apple silicon `rusage_info` times are in these ticks
/// (timebase 125/3: 12 000 193 ticks were 500 ms of CPU), on Intel 1/1.
#[must_use]
#[cfg(any(target_os = "macos", test))]
pub fn ticks_to_ns(ticks: u64, numer: u32, denom: u32) -> u64 {
    if denom == 0 {
        return 0;
    }
    let ns = u128::from(ticks) * u128::from(numer) / u128::from(denom);
    u64::try_from(ns).unwrap_or(u64::MAX)
}

/// What a process's usage read answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Usage {
    /// Read: cumulative CPU nanoseconds and footprint in KiB.
    Measured {
        /// User + system time, nanoseconds.
        cpu_ns: u64,
        /// Physical footprint, KiB.
        footprint_kib: u64,
    },
    /// Refused (EPERM: another user's process). The row is KEPT, unmeasured:
    /// its ppid still carries a chain, and its time is the services'.
    Refused,
    /// The process is gone (ESRCH): no row.
    Gone,
}

/// Who a process is, as far as the sweep read it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ident {
    /// Its parent.
    pub ppid: u32,
    /// Its effective uid.
    pub uid: u32,
    /// Its `comm` (at most 16 bytes).
    pub comm: String,
}

/// One sweep row from what was read: `None` only when the process is gone.
/// A refused usage keeps the row with `cpu_ns` and `footprint_kib` `None`;
/// an unreadable identity keeps it with an empty name and no parent.
#[must_use]
pub fn row_of(pid: u32, ident: Option<Ident>, usage: Usage, our_uid: u32) -> Option<ProcRow> {
    let (cpu_ns, footprint_kib) = match usage {
        Usage::Gone => return None,
        Usage::Refused => (None, None),
        Usage::Measured {
            cpu_ns,
            footprint_kib,
        } => (Some(cpu_ns), Some(footprint_kib)),
    };
    let ident = ident.unwrap_or_default();
    Some(ProcRow {
        pid,
        ppid: ident.ppid,
        uid_is_ours: ident.uid == our_uid,
        name: ident.comm,
        bundle: None,
        cpu_ns,
        footprint_kib,
    })
}

/// The rows worth a path read, in the order to read them: the top
/// [`NAMED_ROWS`] by CPU since `prev` (the last sweep's cumulative times; a
/// pid it lacks counts from zero) and the top [`NAMED_ROWS`] by footprint,
/// then every other row past [`NAME_FLOOR_CPU_NS`] or [`NAME_FLOOR_KIB`],
/// heaviest CPU first. Indices into `rows`, each once, only rows that
/// measured something above zero. The caller reads at most
/// [`MAX_PATH_READS`] new paths from the head of this list.
#[must_use]
pub fn rows_to_name(rows: &[ProcRow], prev: &HashMap<u32, u64>) -> Vec<usize> {
    let delta = |r: &ProcRow| {
        r.cpu_ns.map_or(0, |ns| {
            ns.saturating_sub(prev.get(&r.pid).copied().unwrap_or(0))
        })
    };
    let mut by_cpu: Vec<usize> = (0..rows.len()).filter(|&i| delta(&rows[i]) > 0).collect();
    by_cpu.sort_by_key(|&i| std::cmp::Reverse(delta(&rows[i])));
    by_cpu.truncate(NAMED_ROWS);
    let mut by_mem: Vec<usize> = (0..rows.len())
        .filter(|&i| rows[i].footprint_kib.unwrap_or(0) > 0)
        .collect();
    by_mem.sort_by_key(|&i| std::cmp::Reverse(rows[i].footprint_kib.unwrap_or(0)));
    by_mem.truncate(NAMED_ROWS);
    for i in by_mem {
        if !by_cpu.contains(&i) {
            by_cpu.push(i);
        }
    }
    let mut floor: Vec<usize> = (0..rows.len())
        .filter(|&i| {
            delta(&rows[i]) >= NAME_FLOOR_CPU_NS
                || rows[i].footprint_kib.unwrap_or(0) >= NAME_FLOOR_KIB
        })
        .filter(|i| !by_cpu.contains(i))
        .collect();
    floor.sort_by_key(|&i| std::cmp::Reverse(delta(&rows[i])));
    by_cpu.extend(floor);
    by_cpu
}

/// Give every row its cached executable path: `cache` holds
/// `pid -> (comm, path)` from earlier sweeps, and a row takes it only while
/// its pid still runs the same `comm` (a reused pid is a new image). No
/// syscall: this is how a helper that led an earlier sweep stays in its app
/// on every later one.
pub fn fill_cached_paths(rows: &mut [ProcRow], cache: &HashMap<u32, (String, Option<String>)>) {
    for row in rows.iter_mut().filter(|r| r.bundle.is_none()) {
        if let Some((comm, path)) = cache.get(&row.pid)
            && *comm == row.name
        {
            row.bundle.clone_from(path);
        }
    }
}

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

/// The four Mach CPU-state tick counters (`user`, `system`, `idle`, `nice`)
/// accumulated into 64 bits. Each kernel counter is a 32-bit `natural_t`
/// summed over every core, so on a many-core machine it wraps within weeks
/// of uptime; each step adds the WRAPPING difference, so the `(busy, total)`
/// the engine diffs never runs backwards.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg(any(target_os = "macos", test))]
pub struct CpuTicks {
    last: Option<[u32; 4]>,
    busy: u64,
    total: u64,
}

#[cfg(any(target_os = "macos", test))]
impl CpuTicks {
    /// Fold one read of `[user, system, idle, nice]`; returns `(busy, total)`.
    pub fn advance(&mut self, now: [u32; 4]) -> (u64, u64) {
        let [user, system, idle, nice] = match self.last {
            None => now.map(u64::from),
            Some(last) => {
                let mut d = [0u64; 4];
                for (i, v) in d.iter_mut().enumerate() {
                    *v = u64::from(now[i].wrapping_sub(last[i]));
                }
                d
            }
        };
        self.last = Some(now);
        let busy = user + system + nice;
        self.busy = self.busy.saturating_add(busy);
        self.total = self.total.saturating_add(busy + idle);
        (self.busy, self.total)
    }
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

/// The memory gauge, permille, from `kern.memorystatus_level` (the percent
/// of memory still FREE): `1000 − 10·level` — how little the kernel has
/// left, a pressure gauge; never read as bytes in use (that is
/// [`used_mib_of`]). Out of range is unknown.
#[must_use]
#[cfg(any(target_os = "macos", test))]
pub fn used_of_level(level: i64) -> Option<u16> {
    let level = u16::try_from(level).ok().filter(|l| *l <= 100)?;
    Some(1000 - 10 * level)
}

/// `NSProcessInfoThermalState` (0 nominal … 3 critical); anything else is
/// unknown.
#[must_use]
#[cfg(any(target_os = "macos", test))]
pub fn thermal_of(state: isize) -> Option<Thermal> {
    match state {
        0 => Some(Thermal::Nominal),
        1 => Some(Thermal::Fair),
        2 => Some(Thermal::Serious),
        3 => Some(Thermal::Critical),
        _ => None,
    }
}

/// A NUL-terminated C `char` array (a `comm`) as text, lossily.
#[must_use]
#[cfg(any(target_os = "macos", test))]
pub fn c_text(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

#[cfg(test)]
mod tests;
