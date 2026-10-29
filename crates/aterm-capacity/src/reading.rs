// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ONE LOOK AT THE MACHINE: cores, load, memory, swap and one volume ([`Reading`]), and each
//! process's parent, footprint and bytes written ([`Procs`]), read through a [`Machine`]: the
//! live one ([`Live`], aterm-sysprobe's readers) or a fixed one a test sets ([`Fixed`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
#[cfg(any(test, debug_assertions))]
use std::sync::{Mutex, PoisonError};

use aterm_sysprobe::volumes::{self, Table, Unread};

use crate::floor::GIB;

/// One volume, as a [`Reading`] holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    /// Where it is mounted.
    pub mount: PathBuf,
    /// Its size, bytes.
    pub total: u64,
    /// Bytes an unprivileged writer may still write (`f_bavail`).
    pub free: u64,
    /// The floor the window's disk watch holds it to ([`crate::floor::Watch`]: the owner's knobs, raised to the room), or the emergency line where the owner turned that floor off ([`crate::floor::admission_floor`]): admission keeps free space above it.
    pub floor: u64,
    /// The room the floor is raised to: the second-largest daily two-hour fall of free space the watch measured over a week ([`crate::room`]), or `disk.room_gib` where written.
    pub room: u64,
}

impl Volume {
    /// Bytes free above the floor.
    #[must_use]
    pub const fn above_floor(&self) -> u64 {
        self.free.saturating_sub(self.floor)
    }

    /// The most a disk need can ever have here: the volume's size above its floor.
    #[must_use]
    pub const fn ceiling(&self) -> u64 {
        self.total.saturating_sub(self.floor)
    }
}

/// THE MACHINE NOW. Each figure is `None` where this OS does not report it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reading {
    /// Logical cores.
    pub cores: u32,
    /// The 1-minute load average.
    pub load1: Option<f64>,
    /// The 5-minute load average.
    pub load5: Option<f64>,
    /// Memory a claim can take now, bytes: on macOS what [`claimable`] reads (free pages, the
    /// inactive ones file-backed pages could fill, purgeable pages, and what compressing the
    /// anonymous inactive rest frees at the compressor's ratio); elsewhere what the kernel
    /// reports available (`aterm_sysprobe::machine_memory`, Linux's `MemAvailable`).
    pub available: Option<u64>,
    /// Physical memory, bytes.
    pub physical: Option<u64>,
    /// Memory wired down, bytes, which [`Self::available`] never holds: macOS's wired pages
    /// (the kernel's own, and what a driver holds wired for a process, Metal's buffers on Apple
    /// silicon), Linux's unreclaimable slab, kernel stacks and page tables. Read only where
    /// [`Self::available`] is, since only there is admission held to what is available.
    pub wired: Option<u64>,
    /// Swap in use, bytes.
    pub swap_used: Option<u64>,
    /// Swap allocated, bytes.
    pub swap_total: Option<u64>,
    /// The volume holding the path the reading was taken for.
    pub volume: Option<Volume>,
    /// The kernel's memory pressure level (macOS `kern.memorystatus_vm_pressure_level`,
    /// `aterm_sysprobe::memory_pressure`): at [`aterm_sysprobe::MemoryLevel::Critical`], about to
    /// end processes, no memory claim is admitted ([`crate::Short::Pressure`]). `None` off macOS.
    pub pressure: Option<aterm_sysprobe::MemoryLevel>,
}

impl Reading {
    /// Cores the 5-minute load leaves idle (`aterm ctl await sys`'s `cores_idle`): the cores less the load, never below 0.
    #[must_use]
    pub fn idle_cores(&self) -> Option<f64> {
        self.load5.map(|l| (f64::from(self.cores) - l).max(0.0))
    }

    /// THE MOST MEMORY THAT CAN EVER BE AVAILABLE HERE, bytes: physical memory less what is
    /// wired ([`Self::wired`]), or all of it where nothing wired is read. Available memory
    /// ([`Self::available`]) never holds a wired page, an active one, nor what the compressor
    /// occupies, so it always stays below this, and a need of this much or more never fits
    /// ([`crate::never`] refuses it, unless it asks to run alone). This Mac (48 GiB,
    /// 2026-10-25): 3.6 GiB wired, so 44.4 GiB, with 21 to 27 GiB of free, inactive and
    /// purgeable pages under its usual load. `None` where physical memory is unread.
    #[must_use]
    pub fn reachable(&self) -> Option<u64> {
        self.physical
            .map(|p| p.saturating_sub(self.wired.filter(|w| *w < p).unwrap_or(0)))
    }

    /// The live machine now, its volume the one holding `path` ([`Live`]).
    #[must_use]
    pub fn now(path: &Path) -> Reading {
        Live.read(Some(path))
    }

    /// The reading in one line, as a log carries it: `load 3.2 on 14 cores, 30.1 GiB of 48 GiB
    /// available, swap 0.3 GiB`; a figure this OS does not report is left out.
    #[must_use]
    pub fn brief(&self) -> String {
        between(self, self)
    }
}

/// Two readings of one machine in one line, `from` then `to`, a figure shown once where it held:
/// `load 3.2→5.1 on 14 cores, 30.1→29.8 GiB of 48 GiB available, swap 0.3 GiB`. What a run's
/// log says the machine did while a step ran, so a step refused as starved explains itself.
#[must_use]
pub fn between(from: &Reading, to: &Reading) -> String {
    let pair = |a: String, b: String| if a == b { a } else { format!("{a}→{b}") };
    let load = |r: &Reading| {
        r.load1
            .map_or_else(|| "?".to_owned(), |l| format!("{l:.1}"))
    };
    let gib = |b: Option<u64>| b.map_or_else(|| "?".to_owned(), |b| format!("{:.1}", tenths(b)));
    let mut parts = vec![format!(
        "load {} on {} cores",
        pair(load(from), load(to)),
        to.cores
    )];
    if from.available.is_some() || to.available.is_some() {
        let of = to
            .physical
            .map(|p| format!(" of {}", crate::gib(p)))
            .unwrap_or_default();
        parts.push(format!(
            "{} GiB{of} available",
            pair(gib(from.available), gib(to.available))
        ));
    }
    if from.swap_used.is_some() || to.swap_used.is_some() {
        parts.push(format!(
            "swap {} GiB",
            pair(gib(from.swap_used), gib(to.swap_used))
        ));
    }
    parts.join(", ")
}

/// `bytes` in GiB to a tenth.
fn tenths(bytes: u64) -> f64 {
    (bytes as f64 / GIB as f64 * 10.0).round() / 10.0
}

/// THE MEMORY A CLAIM CAN TAKE ON A MAC NOW, bytes (CLAIMABLE), from one read of its page
/// queues `d`; `None` where they are unread (off macOS, whose kernels report what is available
/// themselves). It counts
/// - the free pages, the speculative ones included;
/// - the inactive pages as far as file-backed pages could fill the inactive queue: the kernel
///   drops a file-backed page and reads it back from its file when it is touched;
/// - the purgeable pages, which their owners let the kernel drop;
/// - of the anonymous inactive pages the rest of that queue holds, what compressing them frees at
///   the ratio the compressor holds now (`1 - occupied / stored`): the kernel compresses an idle
///   anonymous page before it swaps anything. With nothing in the compressor no ratio is read,
///   and they count for nothing;
///
/// and never more than physical memory. Not the kernel's `kern.memorystatus_level`, which counts
/// the active queue too: on this 24 GiB Mac on 2026-10-29 it read 78% while the free, active,
/// inactive and speculative pages were 77% of memory, and it read 85% beside 12.5 GB of swap
/// the night before. Not every inactive page either (`aterm_sysprobe::available_mib_of`, what
/// the window and the agents' ceilings read): an anonymous one is given back only by compressing
/// or swapping it. At one look that night (85% by `memory_pressure`, pressure normal, 0.09 GiB
/// free, 9.92 GiB inactive, 2.62 GiB file-backed, the compressor holding 1.51 GiB in 0.64),
/// this reads about 6.9 GiB, every inactive page 10.0, and the file-backed ones alone 2.7.
#[must_use]
pub fn claimable(d: &aterm_sysprobe::MemoryDetail) -> Option<u64> {
    let inactive = d.inactive?;
    let purgeable = d.purgeable?;
    let free = d.free.saturating_add(d.speculative.unwrap_or(0));
    let dropped = inactive.min(d.file_backed);
    let anonymous = inactive - dropped;
    let compressed = match (d.compressed, d.stored) {
        (Some(occupied), Some(stored)) if stored > 0 && occupied < stored => {
            let freed = u128::from(anonymous) * u128::from(stored - occupied) / u128::from(stored);
            u64::try_from(freed).unwrap_or(anonymous)
        }
        _ => 0,
    };
    let room = free
        .saturating_add(dropped)
        .saturating_add(purgeable)
        .saturating_add(compressed);
    Some(if d.total > 0 { room.min(d.total) } else { room })
}

/// This machine's logical cores, at least one.
#[must_use]
pub fn cores() -> u32 {
    u32::try_from(aterm_sysprobe::cpu_layout().logical_cores())
        .unwrap_or(u32::MAX)
        .max(1)
}

impl Reading {
    /// A reading of `physical` GiB with `available` GiB of it available, `cores` cores and no load: what a test sets.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn of_gib(physical: u64, available: u64, cores: u32) -> Reading {
        Reading {
            cores,
            load1: Some(0.0),
            load5: Some(0.0),
            available: Some(available.saturating_mul(GIB)),
            physical: Some(physical.saturating_mul(GIB)),
            wired: None,
            swap_used: Some(0),
            swap_total: Some(0),
            volume: None,
            pressure: None,
        }
    }

    /// This reading at memory pressure `level` ([`Self::pressure`]): what a test sets.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub const fn at_pressure(mut self, level: aterm_sysprobe::MemoryLevel) -> Reading {
        self.pressure = Some(level);
        self
    }

    /// This reading with `gib` GiB of its memory wired ([`Self::wired`]): what a test sets.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub const fn wired_gib(mut self, gib: u64) -> Reading {
        self.wired = Some(gib.saturating_mul(GIB));
        self
    }
}

/// One process, as the ledger reads it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Proc {
    /// Its pid.
    pub pid: u32,
    /// Its parent's pid.
    pub ppid: u32,
    /// The memory it holds now, bytes ([`Proc::of_reading`]: the lesser of its footprint and its
    /// resident size, on macOS its physical footprint and resident size, on Linux `RssAnon`): 0
    /// where unread.
    pub footprint: u64,
    /// Bytes it has written to disk since it started: 0 where unread (Linux).
    pub written: u64,
}

/// A process tree's use: what a claimant already takes, so its claims' reservations count only the rest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TreeUse {
    /// The tree's footprints, summed.
    pub footprint: u64,
    /// The tree's bytes written, summed.
    pub written: u64,
}

/// Every process of one sweep.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Procs {
    procs: Vec<Proc>,
}

impl Procs {
    /// These processes.
    #[must_use]
    pub fn of(procs: Vec<Proc>) -> Procs {
        Procs { procs }
    }

    /// The processes `pid` descends from, its parent first, as far as the sweep holds them.
    #[must_use]
    pub fn ancestors(&self, pid: u32) -> Vec<u32> {
        let parent: HashMap<u32, u32> = self.procs.iter().map(|p| (p.pid, p.ppid)).collect();
        let mut out = Vec::new();
        let mut at = pid;
        while let Some(&up) = parent.get(&at) {
            if up == at || up == pid || out.contains(&up) {
                break;
            }
            out.push(up);
            at = up;
        }
        out
    }

    /// The use of `pid` and every process descended from it.
    #[must_use]
    pub fn tree(&self, pid: u32) -> TreeUse {
        self.trees(&[pid]).remove(&pid).unwrap_or_default()
    }

    /// The use of each of `roots` and the processes descended from it, each process counted
    /// once, toward its nearest ancestor among `roots` (itself when it is one): a claimant's
    /// tree never holds another claimant's, so no process is credited to two claims.
    #[must_use]
    pub fn trees(&self, roots: &[u32]) -> HashMap<u32, TreeUse> {
        let parent: HashMap<u32, u32> = self.procs.iter().map(|p| (p.pid, p.ppid)).collect();
        let mut out: HashMap<u32, TreeUse> =
            roots.iter().map(|r| (*r, TreeUse::default())).collect();
        for p in &self.procs {
            let mut at = p.pid;
            let mut steps = 0usize;
            let owner = loop {
                if out.contains_key(&at) {
                    break Some(at);
                }
                match parent.get(&at) {
                    Some(&up) if up != at && steps < self.procs.len() => {
                        at = up;
                        steps += 1;
                    }
                    _ => break None,
                }
            };
            if let Some(sum) = owner.and_then(|o| out.get_mut(&o)) {
                sum.footprint = sum.footprint.saturating_add(p.footprint);
                sum.written = sum.written.saturating_add(p.written);
            }
        }
        out
    }
}

/// What the ledger reads of the machine: the live one ([`Live`]) or a fixed one ([`Fixed`]), so no test reaches a platform reader.
pub trait Machine {
    /// The machine now, its volume the one holding `path` (`None`: no volume).
    fn read(&self, path: Option<&Path>) -> Reading;

    /// Every process now; `None` where the platform has no sweep.
    fn procs(&self) -> Option<Procs>;
}

/// The volume holding `path`, read now through the mount table `table` ([`aterm_sysprobe::volumes`]), as the window's disk watch reads its volume: a local volume is placed by the path's nearest existing ancestor, with the platform's error boxes off.
///
/// # Errors
/// [`Unread::NoMount`] when no mount holds `path`, or the volume's own read error.
pub fn volume_of(path: &Path, table: &Table) -> Result<volumes::Reading, Unread> {
    let path = table.long_spelling(path);
    let spelled = volumes::mount_of(&path, table).ok_or(Unread::NoMount)?;
    if spelled.automounted || !spelled.local {
        return volumes::read(spelled);
    }
    let _quiet = aterm_sysprobe::quiet_errors();
    #[cfg(test)]
    VOLUME_ERRORS_QUIET.set(Some(aterm_sysprobe::errors_quiet()));
    let placed = path
        .ancestors()
        .find_map(|a| std::fs::canonicalize(a).ok())
        .ok_or(Unread::NoMount)?;
    volumes::read_path(&placed, table)
}

#[cfg(test)]
thread_local! {
    /// Whether the platform's error boxes were off while [`volume_of`] placed a local volume.
    pub(crate) static VOLUME_ERRORS_QUIET: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// The live machine, through aterm-sysprobe.
#[derive(Clone, Copy, Debug, Default)]
pub struct Live;

impl Machine for Live {
    fn read(&self, path: Option<&Path>) -> Reading {
        let cores = cores();
        let load = aterm_sysprobe::load_average();
        let detail = aterm_sysprobe::memory_detail();
        let physical = aterm_sysprobe::physical_memory_mib()
            .map(|m| m << 20)
            .or_else(|| detail.map(|d| d.total).filter(|t| *t > 0));
        let available = detail
            .as_ref()
            .and_then(claimable)
            .or_else(|| aterm_sysprobe::machine_memory().map(|m| m.available_mib << 20));
        let volume = path
            .and_then(|p| volume_of(p, &volumes::table()).ok())
            .map(|v| {
                let now_s =
                    aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(i64::MAX));
                let watch = crate::floor::Watch::kept();
                let key = v.key.spelled();
                Volume {
                    floor: crate::floor::admission_floor(&watch.lines(&key, v.total, now_s)),
                    room: watch.room(&key, v.total, now_s),
                    mount: v.mount,
                    total: v.total,
                    free: v.free,
                }
            });
        Reading {
            cores,
            load1: load.map(|l| l[0]),
            load5: load.map(|l| l[1]),
            wired: available.and(detail.map(|d| d.wired)),
            available,
            physical,
            swap_used: detail.map(|d| d.swap_used),
            swap_total: detail.map(|d| d.swap_total),
            volume,
            pressure: detail.and_then(|d| d.pressure),
        }
    }

    fn procs(&self) -> Option<Procs> {
        let sweep = aterm_sysprobe::processes()?;
        Some(Procs::of(
            sweep.procs.iter().map(Proc::of_reading).collect(),
        ))
    }
}

impl Proc {
    /// Process `p` as the ledger reads it: the memory it holds is its footprint, or its resident
    /// size where that is less (macOS `ri_resident_size`, Linux `RssAnon`:
    /// `ProcessReading::resident`). A footprint counts the pages the kernel compressed or swapped
    /// out, which a claim's room never held, and a macOS resident size counts the file-backed
    /// pages a process maps, which a footprint does not; so the lesser leaves the compressed and
    /// swapped pages out where they outnumber the file-backed resident ones (an aterm-gui compile
    /// on this Mac, 2026-10-29: 14 GB of footprint, 8.7 GiB resident): a tree whose 10 GiB of
    /// footprint holds 2 GiB of resident pages, credited 10 GiB, has its 12 GiB claim reserve 2
    /// GiB of the room it would take back as it touches them; credited 2 GiB, it reserves 10.
    /// Where the file-backed resident pages outnumber the compressed and swapped ones, the
    /// footprint is credited whole, compressed pages included.
    #[must_use]
    pub fn of_reading(p: &aterm_sysprobe::ProcessReading) -> Proc {
        let footprint = p.footprint.unwrap_or(0);
        Proc {
            pid: p.pid,
            ppid: p.ppid,
            footprint: p.resident.map_or(footprint, |r| r.min(footprint)),
            written: p.disk_written.unwrap_or(0),
        }
    }
}

/// A machine a test sets: the reading and the processes it answers until set again. Its volume is the one the reading holds, whatever path is asked about.
#[cfg(any(test, debug_assertions))]
#[derive(Debug, Default)]
pub struct Fixed {
    reading: Mutex<Reading>,
    procs: Mutex<Option<Procs>>,
}

#[cfg(any(test, debug_assertions))]
impl Fixed {
    /// A machine that answers `reading`, and no processes.
    #[must_use]
    pub fn new(reading: Reading) -> Fixed {
        Fixed {
            reading: Mutex::new(reading),
            procs: Mutex::new(None),
        }
    }

    /// Answer `reading` from now on.
    pub fn set(&self, reading: Reading) {
        *self.reading.lock().unwrap_or_else(PoisonError::into_inner) = reading;
    }

    /// Answer `procs` from now on.
    pub fn set_procs(&self, procs: Option<Procs>) {
        *self.procs.lock().unwrap_or_else(PoisonError::into_inner) = procs;
    }
}

#[cfg(any(test, debug_assertions))]
impl Machine for Fixed {
    fn read(&self, _path: Option<&Path>) -> Reading {
        self.reading
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn procs(&self) -> Option<Procs> {
        self.procs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}
