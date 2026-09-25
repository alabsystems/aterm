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
//! Linux arm is, this round, only its PURE parsers ([`linux`]) over `&str`
//! with fixture tests — there is no live Linux sampler yet, so a Linux build reads like
//! every other platform: a [`Reading`] with every field `None` (and a `None`
//! field is never heavy) and an empty sweep. Windows reads nothing.
//!
//! # Bounded
//!
//! A sweep takes at most [`MAX_PIDS`] pids; a path is read into a
//! [`PATH_CAP`]-byte buffer, once per process image (cached, and the cache
//! given to every row each sweep), and only for rows that can matter —
//! [`NAMED_ROWS`] by CPU and as many by footprint, then any past
//! [`NAME_FLOOR_CPU_NS`] or [`NAME_FLOOR_KIB`] — at most [`MAX_PATH_READS`]
//! new reads a sweep; a
//! file read (the Linux arm's `/proc`) goes through [`read_bounded`], one
//! [`READ_CAP`]-byte stack buffer — never read-to-EOF, which on a device path
//! that never ends is how this repository once took a machine down.

#![deny(missing_docs)]

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
    #[cfg(not(target_os = "macos"))]
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
            #[cfg(not(target_os = "macos"))]
            cores: logical_cores(),
        }
    }

    /// One reading of the machine, stamped when it was taken. On a platform
    /// with no sampler every field is `None`.
    ///
    /// Named `reading`, not `read` (the spec's sketch): a zero-argument
    /// `.read()` is `RwLock::read` to the lock-order census, which scans
    /// every crate the GUI links, and two readings in one scope read to it as
    /// a re-entrant lock.
    #[must_use]
    pub fn reading(&mut self) -> Reading {
        #[cfg(target_os = "macos")]
        {
            self.imp.reading()
        }
        #[cfg(not(target_os = "macos"))]
        {
            let mut r = Reading::empty(Instant::now());
            r.cores = self.cores;
            r
        }
    }

    /// One sweep of the process table (cumulative CPU times; the engine takes
    /// the deltas). Empty on a platform with no sampler.
    #[must_use]
    pub fn scan(&mut self) -> Vec<ProcRow> {
        #[cfg(target_os = "macos")]
        {
            self.imp.scan()
        }
        #[cfg(not(target_os = "macos"))]
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
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = [0u8; READ_CAP];
    let mut n = 0;
    while n < READ_CAP {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    Some(String::from_utf8_lossy(&buf[..n]).into_owned())
}

// ---------------------------------------------------------------------------
// Pure pieces the platform arms share (compiled everywhere, tested here).
// ---------------------------------------------------------------------------

/// Mach absolute-time ticks to nanoseconds: `ticks × numer / denom`, in 128
/// bits, saturating. On Apple silicon `rusage_info` times are in these ticks
/// (timebase 125/3: 12 000 193 ticks were 500 ms of CPU), on Intel 1/1.
#[must_use]
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
pub struct CpuTicks {
    last: Option<[u32; 4]>,
    busy: u64,
    total: u64,
}

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
pub fn used_of_level(level: i64) -> Option<u16> {
    let level = u16::try_from(level).ok().filter(|l| *l <= 100)?;
    Some(1000 - 10 * level)
}

/// `NSProcessInfoThermalState` (0 nominal … 3 critical); anything else is
/// unknown.
#[must_use]
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
pub fn c_text(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

#[cfg(test)]
mod tests;
