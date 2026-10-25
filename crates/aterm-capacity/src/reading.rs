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

#[cfg(any(test, debug_assertions))]
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
    /// Memory the kernel can hand out without paging anything out (free, inactive and purgeable pages), bytes.
    pub available: Option<u64>,
    /// Physical memory, bytes.
    pub physical: Option<u64>,
    /// Swap in use, bytes.
    pub swap_used: Option<u64>,
    /// Swap allocated, bytes.
    pub swap_total: Option<u64>,
    /// The volume holding the path the reading was taken for.
    pub volume: Option<Volume>,
}

impl Reading {
    /// Cores the 5-minute load leaves idle (`aterm ctl await sys`'s `cores_idle`): the cores less the load, never below 0.
    #[must_use]
    pub fn idle_cores(&self) -> Option<f64> {
        self.load5.map(|l| (f64::from(self.cores) - l).max(0.0))
    }

    /// The live machine now, its volume the one holding `path` ([`Live`]).
    #[must_use]
    pub fn now(path: &Path) -> Reading {
        Live.read(Some(path))
    }

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
            swap_used: Some(0),
            swap_total: Some(0),
            volume: None,
        }
    }
}

/// One process, as the ledger reads it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Proc {
    /// Its pid.
    pub pid: u32,
    /// Its parent's pid.
    pub ppid: u32,
    /// Its memory, bytes (macOS its physical footprint, Linux `RssAnon + VmSwap`): 0 where unread.
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
        let cores = u32::try_from(aterm_sysprobe::cpu_layout().logical_cores())
            .unwrap_or(u32::MAX)
            .max(1);
        let load = aterm_sysprobe::load_average();
        let detail = aterm_sysprobe::memory_detail();
        let physical = aterm_sysprobe::physical_memory_mib()
            .map(|m| m << 20)
            .or_else(|| detail.map(|d| d.total).filter(|t| *t > 0));
        let available = aterm_sysprobe::machine_memory().map(|m| m.available_mib << 20);
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
            available,
            physical,
            swap_used: detail.map(|d| d.swap_used),
            swap_total: detail.map(|d| d.swap_total),
            volume,
        }
    }

    fn procs(&self) -> Option<Procs> {
        let sweep = aterm_sysprobe::processes()?;
        Some(Procs::of(
            sweep
                .procs
                .iter()
                .map(|p| Proc {
                    pid: p.pid,
                    ppid: p.ppid,
                    footprint: p.footprint.unwrap_or(0),
                    written: p.disk_written.unwrap_or(0),
                })
                .collect(),
        ))
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
