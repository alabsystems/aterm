// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ADMISSION: a [`Waiter`] enters the ledger, and each [`Waiter::poll`] reads the machine and
//! the claims ahead of it and either admits it (a [`Claim`]) or names what it is short of
//! ([`Shortfall`]). [`admit_with`] polls until it is admitted, saying once a minute what it
//! waits for; it gives up only on a disk need nothing it waits for can meet (UNMET, below), or
//! where its need asks to end behind holders that make no progress ([`Need::ends_when_stuck`],
//! [`Error::Stuck`]). Memory, cores and the quiet machine
//! come back as the claims ahead end, so a need of them fits once its holders end. Disk mostly
//! does not: what a claim wrote stays on its volume once it ends (a build's caches), so a
//! claim's end gives back only the part of its reservation it never wrote, unless its disk is
//! scratch ([`Need::scratch`]: a pilot's cold build), whose writes may be removed by then; only
//! space freed (the build cache's relief, the window's disk watch, a person)
//! raises what is free beyond that. A waiter whose disk need is more than its volume holds above
//! the floor now is beyond the room (UNSERVABLE, [`Short::unservable`]): its relief runs at once
//! ([`Relief::relieve`]); it waits while another process holds scratch claims there whose
//! writes could bring it within the room if returned, and is relieved again once one it saw held
//! has ended; the next look observes what was actually returned. A look still UNSERVABLE after
//! its relief, with no such claim held, ends the
//! wait (UNMET, [`Error::Unmet`]): nothing it would wait for frees disk, so the wait could never
//! end, and its caller says what to free and how to start it at once where it can. While it is
//! beyond the room it reserves no disk against the waiters behind it, and it reserves again once
//! its need fits (`reservations`). A need its relief brought
//! within the room waits, short only of what claims reserve: every claim that fits is admitted
//! around it meanwhile, and their writes can take room the relief gave, so the look at which it
//! fits the room but is short asks the relief once more, sized for them, where a claim reserves
//! disk that did not at the look its last relief was sized for; a look that finds it
//! UNSERVABLE again is relieved again, and refused if that relief leaves it UNSERVABLE too
//! with no scratch claim held that could meet it.
//! A relief, or any pass that runs while it waits, can remove what
//! a need was sized on (a run's own build directory, which the run then writes again from
//! empty): the need sized again after each relief and at the look that admits a claim that
//! waited ([`Relief::resized`]) re-enters the wait when it grew. SIGINT, SIGTERM or SIGHUP ends the wait within a nap,
//! `--admit now`'s wait for the ledger's lock included ([`aterm_toolchain::interrupt`]), as its
//! caller can ([`admit_until`]): the claim leaves the queue and [`Error::Stopped`] names what it
//! was short of. A need that can never fit
//! ([`never`]) is refused before it enters. A load-only quiet claim with a [`crate::Patience`] waits for
//! the load within it: past its reservation age it holds back claimed work that asked after it
//! ([`Short::Reserved`]), and at its bound it starts whatever the load ([`QuietStart`]).
//!
//! QUIET-BURST. A quiet claim with a patience, and a measured one ([`Need::timing`]) whatever
//! covers it, waits for a quiet GPU too ([`Short::Gpu`]) until its patience bound, as it waits
//! for the load: another process's long command buffers starve the frames timed work renders,
//! and neither the load nor a CPU window shows them. At its bound the GPU holds it back no longer,
//! and how busy it read is stated ([`QuietStart::gpu`]); a CPU window and the load still decide
//! whether a measured claim is qualified or loaded.
//!
//! QUIET-FIRST. A claim of a process that holds the quiet machine (a timed stage's memory, which
//! verify claims once it holds its run's quiet claim) waits for the held claims alone, never
//! behind a waiter ([`counted`]): every other timed run waits for that quiet claim, so a turn in
//! the queue would hold the quiet machine for nothing while the run measured nothing.
//!
//! ALONE. A memory need is admitted beside the claims ahead of it only while what is available,
//! less what they reserve, holds it. Where it does not, and no other claim holds memory or waits
//! ahead for it, what it is short of is work no claim holds (a person's apps, another session's
//! plain build): no claim's end gives that back, so a wait for it has no bound. Such a need, if
//! it asks for no margin and is below what can ever be available ([`Reading::reachable`]), is
//! admitted ALONE at once ([`Alone`]), and so is one that asks to be ([`crate::Need::alone`])
//! once nothing else holds memory; while a claim is held alone, every
//! other memory claim waits ([`Short::Alone`]), so the machine runs one heavy claim at a time.
//! Waits stay first in, first out behind claims that hold memory: a need short of memory waits
//! for every claim ahead that holds it, and is admitted, beside what is left or alone, once they
//! end. A waiter of the same process is not ahead of its memory: the process orders its own
//! claims (verify's run budget), so its later claim that fits never waits for an earlier one's
//! room. Nor is any waiter ahead of a process that holds the quiet machine (QUIET-FIRST): it
//! overtakes them by the timed stage it runs. Bounded so, every memory wait ends once the claims
//! ahead of it end. A need with a margin (a VM's boot, which pins its memory) is never admitted
//! alone: it waits for room beside it.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::floor::GIB;
use crate::ledger::{Held, Laid, Ledger, Record, millis};
use crate::overrun::{Overrun, overruns_of};
use crate::reading::{Machine, Procs, Reading, TreeUse, Volume};
use crate::{Admit, Need};

/// The 5-minute load average's time constant (macOS's and Linux's alike): `t` after a claim of
/// `n` jobs was admitted, its work has put `n·(1 − e^(−t/this))` into the 5-minute load, which a
/// later grant does not count as unclaimed work ([`Claim::cores`]).
pub const LOAD5_TIME_CONSTANT: Duration = Duration::from_secs(300);

/// How soon a waiter looks again when another waiter held the admissions' lock: that lock is held only while one waiter reads the ledger, so this is no wait for capacity, and says a line only once one holder has kept it a whole [`Pace::line_every`] (stopped, say, while it decides).
const TURN_GAP: Duration = Duration::from_millis(20);

/// The most processes one claim is credited with ([`Claim::credit`]).
pub const CREDIT_MAX: usize = 16;

/// What a need that can never fit names the machine as.
const HOST: &str = if cfg!(target_os = "macos") {
    "this Mac"
} else {
    "this machine"
};

/// How often a waiter looks again ([`Pace::poll`]) and how often it says it still waits ([`Pace::line_every`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pace {
    /// Between two looks.
    pub poll: Duration,
    /// Between two wait lines; the first is said at the first look that waits.
    pub line_every: Duration,
}

impl Default for Pace {
    fn default() -> Self {
        Pace {
            poll: Duration::from_secs(2),
            line_every: Duration::from_secs(60),
        }
    }
}

impl Pace {
    /// How soon a look that met another admission holding the ledger's lock looks again:
    /// [`Self::poll`], or 20 ms where that is longer. The lock is held only while one admission
    /// decides, so this is no wait for capacity.
    #[must_use]
    pub fn turn_gap(self) -> Duration {
        self.poll.min(TURN_GAP)
    }
}

/// `bytes` as a person reads it: whole GiB bare (`48 GiB`), else to a tenth (`0.5 GiB`).
#[must_use]
pub fn gib(bytes: u64) -> String {
    let g = u128::from(GIB);
    tenths_of_gib((u128::from(bytes) * 10 + g / 2) / g)
}

/// `tenths` tenths of a GiB, as [`gib`] writes them.
fn tenths_of_gib(tenths: u128) -> String {
    if tenths % 10 == 0 {
        format!("{} GiB", tenths / 10)
    } else {
        format!("{}.{} GiB", tenths / 10, tenths % 10)
    }
}

/// [`gib`] rounded up to the tenth: an amount to free, never below what is short, and never
/// `0 GiB` for a shortfall of a byte.
fn gib_up(bytes: u64) -> String {
    tenths_of_gib((u128::from(bytes) * 10).div_ceil(u128::from(GIB)))
}

/// A span as a person reads it: `45s`, `3m05s`, `2h10m`.
#[must_use]
pub fn span(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m{:02}s", s / 60, s % 60),
        _ => format!("{}h{:02}m", s / 3600, s % 3600 / 60),
    }
}

/// Now, unix ms (0 before the epoch).
fn now_ms() -> u64 {
    aterm_time::unix_millis().unwrap_or(0)
}

/// TEST SEAM: append timestamp, pid and admission phase to this debug-only trace file.
#[cfg(debug_assertions)]
pub const TRACE_ENV: &str = "ATERM_CAPACITY_TRACE";

fn trace(phase: &str) {
    #[cfg(debug_assertions)]
    {
        use std::io::Write as _;
        let Some(path) = std::env::var_os(TRACE_ENV).filter(|path| !path.is_empty()) else {
            return;
        };
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
        }
        if let Ok(mut file) = opts.open(path)
            && file.metadata().is_ok_and(|m| m.is_file())
        {
            let line = format!("{} {} {phase}\n", now_ms(), std::process::id());
            let _ = file.write_all(line.as_bytes());
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = phase;
}

/// Why `need` can never be admitted on the machine `reading` reads: all the memory that can ever be available there or more, its margin included ([`Reading::reachable`]: physical memory less what is wired, which what is available never reaches), unless it asks to be admitted alone ([`Need::alone`]), which takes all the machine has; or more disk than its volume holds above the floor. `None` when it can.
#[must_use]
pub fn never(need: &Need, reading: &Reading) -> Option<String> {
    if need.mem > 0
        && !need.alone
        && let (Some(physical), Some(reach)) = (reading.physical, reading.reachable())
        && need.mem.saturating_add(need.margin) >= reach
    {
        let ceiling = if reach < physical {
            format!(
                "{HOST} can never have more than {} of its {} of memory available ({} of it is wired)",
                gib(reach),
                gib(physical),
                gib(physical - reach)
            )
        } else {
            format!("{HOST} has {} of memory", gib(physical))
        };
        return Some(format!(
            "{}: needs {}, {ceiling}",
            need.what,
            mem_and_margin(need.mem, need.margin)
        ));
    }
    if need.disk > 0
        && let Some(v) = &reading.volume
        && need.disk > v.ceiling()
    {
        return Some(format!(
            "{}: needs {} of disk on {}, which holds {} above its {} floor",
            need.what,
            gib(need.disk),
            v.mount.display(),
            gib(v.ceiling()),
            gib(v.floor)
        ));
    }
    None
}

/// `16 GiB`, or with a margin `8 GiB and 8 GiB spare beside it`.
fn mem_and_margin(mem: u64, margin: u64) -> String {
    if margin == 0 {
        gib(mem)
    } else {
        format!("{} and {} spare beside it", gib(mem), gib(margin))
    }
}

/// One thing a waiter is short of.
#[derive(Clone, Debug, PartialEq)]
pub enum Short {
    /// A Release claim ends repeatable Background work.
    Release,
    /// No core remains after held grants and higher ranked reservations.
    Cores,
    /// A typed timing need has no qualifying actual CPU observation yet.
    Timing(crate::TimingIssue),
    /// Memory: it needs `need`, with `margin` spare beside it; `available` is free now, and the claims ahead reserve `reserved` of it.
    Memory {
        /// Its peak.
        need: u64,
        /// What must stay available beside it, unreserved ([`Need::margin`]).
        margin: u64,
        /// Available now.
        available: u64,
        /// What the claims ahead still reserve.
        reserved: u64,
    },
    /// Disk on the volume mounted at `mount`.
    Disk {
        /// Its need.
        need: u64,
        /// The volume.
        mount: PathBuf,
        /// Free above the floor now.
        above_floor: u64,
        /// The volume's floor.
        floor: u64,
        /// What the claims ahead on that volume still reserve: never an unservable waiter's
        /// need ([`Short::unservable`]).
        reserved: u64,
        /// For a need [`Short::unservable`], the scratch claims other processes hold on that
        /// volume ([`Need::scratch`]: a pilot's cold build), each by its place in the queue
        /// ([`Record::order`]), where what they may give back by their end would bring it within
        /// the room: it waits while any is held, then rechecks what actually returned, and is
        /// refused ([`Error::Unmet`]) once none is held. Empty where their upper bound could not
        /// meet it (or there are none): no wait would meet it.
        held: Vec<(u64, u32, u64)>,
    },
    /// The machine is not quiet: the 1-minute load is above what the claim allows.
    Load {
        /// The 1-minute load now.
        load1: f64,
        /// The most it allows.
        max: f64,
    },
    /// The GPU is not quiet: busier than a quiet claim allows ([`crate::QUIET_GPU_BUSY`]), at its
    /// most over the look's window ([`Machine::gpu_busy`]). Another process's GPU work starves
    /// the frames timed work renders whatever the load.
    Gpu {
        /// How busy, percent.
        busy: u8,
        /// The most it allows.
        max: u8,
    },
    /// Another quiet claim holds the quiet machine, or waits ahead for it.
    Quiet {
        /// Its holder's pid.
        pid: u32,
        /// What it is.
        what: String,
        /// Whether it holds (else it waits ahead).
        held: bool,
    },
    /// A quiet claim that asked earlier reserves the machine for its timed work: claimed work
    /// that asks after it waits until it has started and its first minutes have run
    /// ([`crate::Patience`]).
    Reserved {
        /// Its holder's pid.
        pid: u32,
        /// What it is.
        what: String,
        /// Whether its timed work runs (else it waits for a quiet machine).
        held: bool,
        /// The most its reservation still lasts.
        left: Duration,
    },
    /// The kernel's memory pressure is critical ([`Reading::pressure`]): it is about to end
    /// processes, so no memory claim is admitted, whatever it reads as available, until the
    /// pressure eases. A claim waits for it, and is never refused for it; at `warn`, the kernel
    /// compressing and swapping, a claim is admitted by what is available as at `normal`.
    Pressure,
    /// Another claim holds the machine's memory ALONE ([`Alone`]): no memory claim is admitted
    /// beside it, whatever its size, until it ends.
    Alone {
        /// Its holder's pid.
        pid: u32,
        /// What it is.
        what: String,
    },
    /// Another waiter held the admissions' lock at this look.
    Turn {
        /// Its pid, where it wrote one.
        holder: Option<u32>,
    },
    /// The ledger could not be read or written.
    Ledger(String),
}

impl Short {
    /// Whether this is short of something a claim can hold.
    const fn held_by_claims(&self) -> bool {
        matches!(
            self,
            Short::Memory { .. }
                | Short::Disk { .. }
                | Short::Load { .. }
                | Short::Quiet { .. }
                | Short::Reserved { .. }
                | Short::Alone { .. }
        )
    }

    /// Whether this is the machine too busy for a quiet claim: the load, a measured claim's CPU
    /// window, or the GPU, what its patience waits for ([`crate::Patience`]).
    const fn machine_busy(&self) -> bool {
        matches!(
            self,
            Short::Load { .. } | Short::Timing(_) | Short::Gpu { .. }
        )
    }

    /// Whether a [`Relief`] may free some of this: what claims hold, but no reservation, which
    /// only time ends.
    const fn relievable(&self) -> bool {
        self.held_by_claims() && !matches!(self, Short::Reserved { .. })
    }

    /// Whether this is a disk need beyond the room (UNSERVABLE): more than its volume holds
    /// above the floor now, before any reservation. What a claim wrote stays on the volume once
    /// it ends unless its disk is scratch ([`Need::scratch`]), so only space freed, or what such
    /// a claim may give back, can admit it: its relief runs, it waits while scratch claims are
    /// held there whose upper bound could meet it (`held`), then rechecks what actually returned;
    /// a look still UNSERVABLE after its relief with none held is refused ([`Error::Unmet`]).
    /// Meanwhile it reserves no disk against the waiters
    /// behind it (`reservations`, by the same test, `beyond_room`).
    #[must_use]
    pub const fn unservable(&self) -> bool {
        matches!(self, Short::Disk { need, above_floor, .. } if beyond_room(*need, *above_floor))
    }
}

/// Whether a disk need of `need` bytes is more than `above_floor`, what its volume holds above
/// the floor now (UNSERVABLE): the one test [`Short::unservable`], which relieves or refuses a
/// wait, and `unservable`, which decides what a waiter reserves, share.
const fn beyond_room(need: u64, above_floor: u64) -> bool {
    need > above_floor
}

impl fmt::Display for Short {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Short::Release => f.write_str("a Release claim ends background work"),
            Short::Cores => f.write_str("all cores are held or reserved by higher priority work"),
            Short::Timing(why) => write!(f, "measurement readiness: {why}"),
            Short::Memory {
                need,
                margin,
                available,
                reserved,
            } => {
                let room = available.saturating_sub(*reserved);
                write!(
                    f,
                    "memory short by {}: needs {}, {} available less {} reserved",
                    gib(need.saturating_add(*margin).saturating_sub(room)),
                    mem_and_margin(*need, *margin),
                    gib(*available),
                    gib(*reserved)
                )
            }
            Short::Disk {
                need,
                mount,
                above_floor,
                floor,
                reserved,
                ..
            } => {
                let room = above_floor.saturating_sub(*reserved);
                write!(
                    f,
                    "disk short by {}: needs {} on {}, {} free above its {} floor less {} reserved",
                    gib(need.saturating_sub(room)),
                    gib(*need),
                    mount.display(),
                    gib(*above_floor),
                    gib(*floor),
                    gib(*reserved)
                )
            }
            Short::Load { load1, max } => {
                write!(f, "waiting for a quiet machine: load {load1:.1} > {max:.1}")
            }
            Short::Gpu { busy, max } => {
                write!(f, "waiting for a quiet GPU: {busy}% busy > {max}%")
            }
            Short::Quiet { pid, what, held } => write!(
                f,
                "pid {pid} (\"{what}\") {} the quiet machine",
                if *held { "holds" } else { "waits ahead for" }
            ),
            Short::Reserved {
                pid,
                what,
                held,
                left,
            } => write!(
                f,
                "pid {pid} (\"{what}\") reserves the machine for timed work {}: claimed work that \
                 asked after it waits, {} more at most",
                if *held {
                    "that has started"
                } else {
                    "while it waits for a quiet machine"
                },
                span(*left)
            ),
            Short::Pressure => f.write_str(
                "memory pressure is critical: the kernel is about to end processes, so no memory \
                 claim is admitted until it eases",
            ),
            Short::Alone { pid, what } => write!(
                f,
                "pid {pid} (\"{what}\") holds the machine's memory alone: no memory claim is \
                 admitted beside it until it ends"
            ),
            Short::Turn { holder: Some(pid) } => write!(f, "pid {pid} holds the ledger's lock"),
            Short::Turn { holder: None } => f.write_str("another waiter holds the ledger's lock"),
            Short::Ledger(e) => write!(f, "the ledger: {e}"),
        }
    }
}

/// A claim ahead of a waiter: held, or waiting since earlier.
#[derive(Clone, Debug, PartialEq)]
pub struct Holder {
    /// Its pid.
    pub pid: u32,
    /// What it is.
    pub what: String,
    /// Where it runs.
    pub checkout: String,
    /// Whether it is admitted (else it waits ahead).
    pub held: bool,
    /// How long since it was made.
    pub age: Duration,
    /// The memory it still reserves: its peak less the share of its holder's tree footprint credited to it.
    pub mem: u64,
    /// The memory it claims, its peak ([`Need::mem`]): where its tree already holds all of it,
    /// it reserves none ([`Self::mem`]) and still holds it.
    pub claimed: u64,
    /// Whether it holds the machine's memory alone ([`Short::Alone`]).
    pub alone: bool,
    /// The disk it still reserves on the waiter's volume.
    pub disk: u64,
    /// Whether it is a quiet claim.
    pub quiet: bool,
    /// Which claim it is: its place in the queue ([`Record::order`]).
    pub order: (u64, u32, u64),
    /// How long its holder's process tree, with the processes it is credited with
    /// ([`Self::credit`]), has made no progress, once that reached the waiter's bound
    /// ([`aterm_toolchain::stall`]): read by the waiter, of a held claim only.
    pub stalled: Option<Duration>,
    /// The processes it is credited with ([`Record::credit`]: a VM's, outside its holder's tree),
    /// whose work is its holder's.
    pub credit: Vec<u32>,
    /// What its holder's tree holds past the memory its held claims claim, in pages the kernel
    /// compressed or swapped among them ([`Overrun`]): of a held claim of memory only, read by
    /// the waiter; `None` where it holds less than a tenth of a GiB past them.
    pub overrun: Option<Overrun>,
}

impl fmt::Display for Holder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "pid {} {} ",
            self.pid,
            if self.held {
                "holds"
            } else {
                "waits ahead for"
            },
        )?;
        let mut any = false;
        let mut join = |f: &mut fmt::Formatter<'_>| -> fmt::Result {
            if any {
                f.write_str(" and ")?;
            }
            any = true;
            Ok(())
        };
        if self.alone {
            join(f)?;
            write!(f, "{} of memory alone", gib(self.claimed))?;
        } else if self.mem > 0 {
            join(f)?;
            write!(f, "{} of memory", gib(self.mem))?;
        } else if self.claimed > 0 {
            join(f)?;
            write!(
                f,
                "{} of memory, all of it in use by its tree",
                gib(self.claimed)
            )?;
        }
        if self.disk > 0 {
            join(f)?;
            write!(f, "{} of disk", gib(self.disk))?;
        }
        if self.quiet {
            join(f)?;
            f.write_str("the quiet machine")?;
        }
        if !any {
            f.write_str("a claim")?;
        }
        write!(f, " for {}: \"{}\"", span(self.age), self.what)?;
        if !self.checkout.is_empty() {
            write!(f, " in {}", self.checkout)?;
        }
        if let Some(o) = self.overrun {
            write!(f, ", its tree holds {o}")?;
        }
        if let Some(span) = self.stalled {
            write!(f, ", {}", aterm_toolchain::stall::no_progress(span))?;
        }
        Ok(())
    }
}

/// What a waiter is short of, and the claims ahead of it that hold what it needs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shortfall {
    /// Each thing it is short of.
    pub shorts: Vec<Short>,
    /// The claims ahead that reserve any of it.
    pub holders: Vec<Holder>,
}

impl Shortfall {
    fn only(short: Short) -> Shortfall {
        Shortfall {
            shorts: vec![short],
            holders: Vec::new(),
        }
    }
}

/// The claims ahead that reserve any of the disk `short` is short of, each by its place in the
/// queue ([`Holder::order`]); none where it is short of no disk.
fn disk_reservers(short: &Shortfall) -> Vec<(u64, u32, u64)> {
    if !short.shorts.iter().any(|s| matches!(s, Short::Disk { .. })) {
        return Vec::new();
    }
    short
        .holders
        .iter()
        .filter(|h| h.disk > 0)
        .map(|h| h.order)
        .collect()
}

/// The claims held on a volume a disk need of `short` is beyond the room of, which it waits for
/// ([`Short::Disk`]'s `held`); none where it is short of no such need.
fn held_disk(short: &Shortfall) -> Vec<(u64, u32, u64)> {
    short
        .shorts
        .iter()
        .filter_map(|s| match s {
            Short::Disk { held, .. } if s.unservable() => Some(held.iter().copied()),
            _ => None,
        })
        .flatten()
        .collect()
}

impl fmt::Display for Shortfall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, s) in self.shorts.iter().enumerate() {
            if i > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{s}")?;
        }
        if !self.shorts.iter().any(Short::held_by_claims) {
            return Ok(());
        }
        if self.holders.is_empty() {
            write!(f, "; no claim holds it: other work on {HOST}")
        } else {
            for h in &self.holders {
                write!(f, "; {h}")?;
            }
            Ok(())
        }
    }
}

/// A claim in the ledger and what its holder's process tree, and the processes it is credited with, use now.
#[derive(Clone)]
pub(crate) struct Other<'a> {
    pub(crate) record: &'a Record,
    /// The holder's tree ([`crate::Procs::trees`]): the same for every claim one process holds, and credited once among them ([`reservations`]).
    pub(crate) used: TreeUse,
    /// The trees of the processes this claim alone is credited with ([`Record::credit`]).
    pub(crate) credited: TreeUse,
}

/// What one claim still reserves against the claims that asked after it, as their looks reckon
/// it ([`reserved`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reserve {
    /// Memory, bytes: its peak less the share of its holder's tree footprint, and of the
    /// processes credited to it, credited to it.
    pub mem: u64,
    /// Disk on the volume it is reckoned on, bytes: its need less what it has written, `0` for a
    /// claim that is [`Self::unservable`]. `None` where it needs no disk on that volume (or none
    /// was read).
    pub disk: Option<u64>,
    /// Whether it is a waiter whose disk need is more than the volume it is reckoned on holds
    /// above the floor now (UNSERVABLE, the test [`Short::unservable`] relieves or refuses a wait
    /// by): no claim's end can make room for it, only space freed, so it reserves no disk against
    /// the claims behind it.
    pub unservable: bool,
}

/// What each of `records` (a ledger's claims, [`crate::Ledger::list`]) still reserves against a
/// claim that asked after them all, in `records`' order, with `trees` the use of their holders'
/// and credited processes' trees ([`crate::Procs::trees`]; `None`, a look with no process
/// sweep): the figures such a waiter's look holds its need to, by the same reckoning, each
/// credited process counted once, toward the first claim that names it. Each claim is reckoned
/// on the volume its disk is on, read now among `volumes`, as the look of a claim behind it on
/// that volume reckons it; a claim whose volume is not among them reserves no disk there
/// ([`Reserve::disk`] `None`). What `aterm pkg capacity` lists beside each claim.
#[must_use]
pub fn reserved(
    records: &[Record],
    trees: Option<&HashMap<u32, TreeUse>>,
    volumes: &[Volume],
) -> Vec<Reserve> {
    let records: Vec<&Record> = records.iter().collect();
    let taken = records.iter().map(|r| r.pid).collect();
    let none = HashMap::new();
    let others = others(&records, taken, trees.unwrap_or(&none));
    let ahead: Vec<&Other<'_>> = others.iter().collect();
    let mut out = reservations(&ahead, None);
    for v in volumes {
        let on = reservations(&ahead, Some(v));
        for ((slot, reserve), r) in out.iter_mut().zip(on).zip(&records) {
            if r.need.path.as_deref() == Some(v.mount.as_path()) {
                *slot = reserve;
            }
        }
    }
    out
}

/// Each of `records` with its holder's tree use and the use of the processes it is credited
/// with, from `trees`: a credited process is counted toward the first claim that names it, and
/// never where `taken` (the claimants' own pids) holds it already.
pub(crate) fn others<'r>(
    records: &[&'r Record],
    mut taken: HashSet<u32>,
    trees: &HashMap<u32, TreeUse>,
) -> Vec<Other<'r>> {
    records
        .iter()
        .map(|r| {
            let mut credited = TreeUse::default();
            for pid in &r.credit {
                if r.accounting == crate::Accounting::Holder
                    && *pid > 1
                    && taken.insert(*pid)
                    && let Some(u) = trees.get(pid)
                {
                    credited.footprint = credited.footprint.saturating_add(u.footprint);
                    credited.whole = credited.whole.saturating_add(u.whole);
                    credited.written = credited.written.saturating_add(u.written);
                }
            }
            Other {
                record: r,
                used: if r.accounting != crate::Accounting::Holder {
                    TreeUse::default()
                } else {
                    trees.get(&r.pid).copied().unwrap_or_default()
                },
                credited: if r.accounting != crate::Accounting::Holder {
                    TreeUse::default()
                } else {
                    credited
                },
            }
        })
        .collect()
}

/// What each claim of `ahead` still reserves, in `ahead`'s order.
/// A claim first takes what it can of its peak from the processes it alone is credited with (a
/// VM's, outside its holder's tree); then a holder's tree use is credited once across its claims
/// (those held first, then those waiting, each oldest first), each taking what it can of what
/// is left: one process holding several claims is never credited with its whole tree for each.
/// A claim whose holder measures its own writes ([`Record::wrote`]) is credited those instead of
/// any tree's bytes written, which miss the writes of children that have ended (a build's
/// compilers) and count bytes written over.
///
/// A waiter whose disk need is more than the volume holds above the floor now ([`unservable`])
/// reserves none of it: no claim's end can make room for it, so reserving it would hold every
/// waiter behind it to a wait only space freed ends, though their own needs fit now. Its memory
/// is reserved as any other's, since memory does come back as holders end. Once its need fits
/// the room again it reserves its disk again, in its turn. A held claim always reserves.
fn reservations(ahead: &[&Other<'_>], volume: Option<&Volume>) -> Vec<Reserve> {
    let mut credit: HashMap<u32, TreeUse> = HashMap::with_capacity(ahead.len());
    for o in ahead {
        if o.record.accounting == crate::Accounting::Holder {
            credit.entry(o.record.pid).or_insert(o.used);
        }
    }
    let mut order: Vec<usize> = (0..ahead.len()).collect();
    order.sort_unstable_by_key(|&i| {
        (
            ahead[i].record.held.is_none(),
            !ahead[i].record.need.leads,
            ahead[i].record.need.rank,
            ahead[i].record.order(),
        )
    });
    let mut out = vec![Reserve::default(); ahead.len()];
    for i in order {
        let record = ahead[i].record;
        let on = volume.filter(|v| record.need.path.as_deref() == Some(v.mount.as_path()));
        let unservable = on.is_some_and(|v| unservable(record, v));
        let disk = if on.is_some() && !unservable {
            record.need.disk
        } else {
            0
        };
        // Neither durable cohort custody nor a transferred ordinary guard may use a
        // numeric holder's tree as credit. That PID can have died and been reused while
        // the actual native guard still holds the original admitted memory reservation.
        // Their different lifetime/reap protocols remain the ledger's responsibility.
        if record.accounting != crate::Accounting::Holder {
            out[i] = Reserve {
                mem: record.need.mem,
                disk: on.map(|_| disk),
                unservable,
            };
            continue;
        }
        let own = ahead[i].credited;
        let mem = record.need.mem - record.need.mem.min(own.footprint);
        let left = credit.entry(record.pid).or_default();
        let mem_taken = mem.min(left.footprint);
        left.footprint -= mem_taken;
        let disk_left = match record.wrote {
            Some(wrote) => disk - disk.min(wrote),
            None => {
                let disk = disk - disk.min(own.written);
                let taken = disk.min(left.written);
                left.written -= taken;
                disk - taken
            }
        };
        out[i] = Reserve {
            mem: mem - mem_taken,
            disk: on.map(|_| disk_left),
            unservable,
        };
    }
    out
}

#[cfg(test)]
#[path = "admit_detached_tests.rs"]
mod detached_accounting_tests;

/// Whether `other` is a scratch claim ([`Need::scratch`]) held on `volume` by a process other
/// than `pid` (the waiter's): one whose end may give back what it wrote there, which a disk need
/// beyond the room can wait for ([`Short::Disk`]). The waiter's own process lets its claims go
/// only once its wait is over, so a wait for them would never end.
fn scratch_on(other: &Other<'_>, pid: u32, volume: &Volume) -> bool {
    let r = other.record;
    r.held.is_some()
        && r.pid != pid
        && r.need.scratch
        && r.need.disk > 0
        && r.need.path.as_deref() == Some(volume.mount.as_path())
}

/// What `other`, a scratch claim ([`scratch_on`]), could give back by its end: what it has
/// written, as its holder measures it ([`Record::wrote`]), or, unmeasured, its whole need, the
/// most it says it writes. The look after its end reads what actually returned.
fn could_give_back(other: &Other<'_>) -> u64 {
    other.record.wrote.unwrap_or(other.record.need.disk)
}

/// Whether `record` waits for more disk than `volume` holds above the floor now (UNSERVABLE,
/// [`Short::unservable`]): its own look holds its whole need to that room, so it is admitted
/// only once space is freed, whatever claim ends first. A held claim never is.
const fn unservable(record: &Record, volume: &Volume) -> bool {
    record.held.is_none() && beyond_room(record.need.disk, volume.above_floor())
}

/// Whether a quiet claim held by `pid` itself or by one of its ancestors in `procs` covers a quiet
/// need of `pid`: work a quiet claim's holder starts runs under that claim (verify's measuring
/// child, a drive's second window), so it never waits behind its own holder, nor behind a quiet
/// claim that waits for that holder to end.
pub(crate) fn covered(pid: u32, records: &[&Record], procs: Option<&Procs>) -> bool {
    let line = std::cell::OnceCell::new();
    records.iter().any(|r| {
        r.accounting == crate::Accounting::Holder
            && r.held.is_some()
            && r.need.quiet.is_some()
            && (r.pid == pid
                || line
                    .get_or_init(|| procs.map(|p| p.ancestors(pid)).unwrap_or_default())
                    .contains(&r.pid))
    })
}

/// Whether the process `pid` or one of its ancestors in `procs` holds a measured claim admitted
/// loaded at its patience bound ([`crate::TimingCause::Loaded`]): the machine stayed busy through
/// that claim's patience, so work under it waits for none of its own ([`crate::AtBound`]). An
/// expired loaded claim its process still holds counts too: its replacement decides on a fresh
/// window at once rather than wait out the same busy machine again.
fn loaded_cover(pid: u32, records: &[&Record], procs: Option<&Procs>) -> bool {
    let line = std::cell::OnceCell::new();
    records.iter().any(|r| {
        r.held.is_some()
            && r.accounting == crate::Accounting::Holder
            && r.need.quiet_policy == crate::QuietPolicy::Timing
            && r.timing.cause == crate::TimingCause::Loaded
            && (r.pid == pid
                || line
                    .get_or_init(|| procs.map(|p| p.ancestors(pid)).unwrap_or_default())
                    .contains(&r.pid))
    })
}

/// The claim among `records` that covers the builds of the process `pid` ([`covered_memory`]):
/// the first held one of memory that asked to cover its tree ([`Need::covers`]) whose holder is
/// `pid` or one of its ancestors in `procs`, or that credits `pid` or one of them ([`credits`]).
fn covering_of<'r>(pid: u32, records: &[&'r Record], procs: Option<&Procs>) -> Option<&'r Record> {
    let ancestors = std::cell::OnceCell::new();
    let line = || ancestors.get_or_init(|| procs.map(|p| p.ancestors(pid)).unwrap_or_default());
    records.iter().copied().find(|r| {
        r.held.is_some()
            && r.accounting == crate::Accounting::Holder
            && r.need.mem > 0
            && r.need.covers
            && (r.pid == pid || line().contains(&r.pid) || credits(r, pid, line()))
    })
}

/// CREDIT IS KINSHIP: whether the held claim `r` credits `pid` or one of `ancestors` (`pid`'s,
/// parent first) with its use ([`Record::credit`]), so `pid` runs that claim's work as its
/// holder's own tree would. A build's claim kept by a holder past the build's end credits what
/// the build left running (BUILD-ADMIT): a nested build or claim under one of those is the
/// claimed work going on, covered by that claim and never held to it, as under the claim's own
/// holder; uncovered, it would claim again, and beside a claim admitted alone wait for the claim
/// that waits for it.
fn credits(r: &Record, pid: u32, ancestors: &[u32]) -> bool {
    r.accounting == crate::Accounting::Holder
        && r.held.is_some()
        && r.credit.iter().any(|c| *c == pid || ancestors.contains(c))
}

/// Whether a held claim covers the builds of the process `pid` (BUILD-ADMIT): `pid` itself, or
/// one of its ancestors in `procs`, holds a claim of memory that asked to cover its tree's builds
/// ([`Need::covers`]), or is credited by one ([`Record::credit`]: what a build that claim was kept
/// for left running). Modelled on [`covered`], which does this for quiet claims. Required, not a
/// nicety: [`counted`] keeps a nested claim from waiting on its ancestors, but every third party
/// still counts both its reservation and its ancestor's, since [`Procs::trees`] credits each
/// process to its nearest claimant, so the ancestor's stays near its whole need. A claim that
/// waits covers nothing, and nor does one of no memory (a disk claim) or one that did not ask
/// (a VM's, whose memory lies outside the tree its holder runs).
#[must_use]
pub fn covered_memory(pid: u32, records: &[&Record], procs: Option<&Procs>) -> bool {
    covering_of(pid, records, procs).is_some()
}

/// THE CLAIM THAT COVERS THE BUILDS OF `pid` ([`covered_memory`]), read from `ledger` on
/// `machine`; `None` where none does. `marker` is the [`crate::COVER_ENV`] value `pid`
/// inherited: [`crate::NO_COVER`] covers nothing; a claim's marker ([`Record::cover_marker`])
/// whose holder is `pid` or one of its ancestors ([`Machine::lineage`]) is answered by that
/// holder's claims alone ([`Ledger::of_pid`]), and covers only while that claim is held and
/// covers, never by another claim of its holder's; any other marker, or none, leaves every claim
/// to answer.
///
/// # Errors
/// The ledger could not be read.
pub fn covering(
    ledger: &Ledger,
    machine: &dyn Machine,
    pid: u32,
    marker: Option<&str>,
) -> io::Result<Option<Record>> {
    let lineage = std::cell::OnceCell::new();
    let line = || lineage.get_or_init(|| machine.lineage(pid)).as_ref();
    let marker = marker.map(str::trim);
    if marker == Some(crate::NO_COVER) {
        return Ok(None);
    }
    let named = marker.and_then(|m| Some((m, Record::marker_holder(m)?)));
    if let Some((marker, holder)) =
        named.filter(|(_, h)| *h == pid || line().is_some_and(|p| p.ancestors(pid).contains(h)))
    {
        return Ok(ledger.of_pid(holder)?.into_iter().find(|r| {
            r.accounting == crate::Accounting::Holder
                && r.cover_marker() == marker
                && r.held.is_some()
                && r.need.mem > 0
                && r.need.covers
        }));
    }
    let all = ledger.list()?;
    if !all
        .iter()
        .any(|r| r.held.is_some() && r.need.mem > 0 && r.need.covers)
    {
        return Ok(None);
    }
    let all: Vec<&Record> = all.iter().collect();
    Ok(covering_of(pid, &all, line()).cloned())
}

/// THE DISK CLAIM THAT COVERS THE RUN `pid` ([`crate::DISK_COVER_ENV`]), read from `ledger` on
/// `machine`; `None` where none does. `marker` is the claim's name `pid` inherited
/// ([`Record::cover_marker`]): it covers only where its holder is `pid` or one of its ancestors
/// ([`Machine::lineage`]), and then only by that one claim of the holder's ([`Ledger::of_pid`]),
/// held, accounted to its holder, and of disk, and only where the run's writes, landing at `on`,
/// land on that claim's volume (the mount `machine` reads for `on` is the one its record names).
/// Nothing else covers: not another claim of the holder's, not any other ancestor's disk claim (a
/// VM's disk, which its own work writes), not a claim that still waits or that ended (its file's
/// lock let go, whatever pid holds the number now), not one on another volume or on none read
/// (which reserves nothing where the run writes), and not a marker of another form. Unlike
/// [`covering`], no marker, or one that names nothing of the line, leaves nothing to answer: a
/// run under no named claim claims for itself.
///
/// # Errors
/// The holder's claims could not be read.
pub fn covering_disk(
    ledger: &Ledger,
    machine: &dyn Machine,
    pid: u32,
    marker: &str,
    on: &Path,
) -> io::Result<Option<Record>> {
    let marker = marker.trim();
    let Some(holder) = Record::marker_holder(marker) else {
        return Ok(None);
    };
    if holder != pid
        && !machine
            .lineage(pid)
            .is_some_and(|line| line.ancestors(pid).contains(&holder))
    {
        return Ok(None);
    }
    let Some(by) = ledger.of_pid(holder)?.into_iter().find(|r| {
        r.accounting == crate::Accounting::Holder
            && r.cover_marker() == marker
            && r.held.is_some()
            && r.need.disk > 0
    }) else {
        return Ok(None);
    };
    let mount = machine.read(Some(on)).volume.map(|v| v.mount);
    Ok((mount.is_some() && by.need.path == mount).then_some(by))
}

/// The claims a waiter of the process `pid` is held to: every other claim but those its
/// ancestors hold or wait for, and the held ones that credit it or an ancestor ([`credits`]). A waiter whose ancestor holds a claim is that admitted work going
/// on (verify's run starting the lab's VM boot, which claims the VM's disk for itself), so it is
/// held only to the other HELD claims: never to its ancestors', which end only once it has run,
/// nor to any waiter, which waits for that work in turn. Held to either, it would wait for what
/// waits for it, with no end. Its need is then checked beside the other held claims alone: what
/// its ancestors have yet to write may overrun the room by up to their reservation, into the
/// floor, which is what the floor is kept for. Without a process sweep no ancestor is known,
/// and every claim counts.
///
/// LEADS-FIRST: a waiter inside held work still counts each SURE lead that is none of its kin
/// ([`Need::leads`]: a release slice's compiler beside the release's tree, [`sure_leads`]), which
/// [`decide`] puts ahead of it. A sure lead fits once every held claim but the PARENTS has ended,
/// a parent being a held claim that may end only once a waiter under it has run (its holder is
/// that waiter's process or an ancestor of it, or it credits one). So counting it closes no
/// cycle: the lead waits only for held claims and for the sure leads ahead of it, and neither
/// waits for the waiters that count it. What is available is read as it is, memory no claim
/// holds left out of it, so that memory makes a lead unsure rather than holding the waiters that
/// count it.
///
/// QUIET-FIRST: a waiter whose own process holds the quiet machine (verify's timed stage, which
/// claims its memory once it holds the run's quiet claim) is held to the held claims alone, its
/// own included, and to no waiter. Every other timed run waits for its quiet claim, which is held
/// for the run's timed work alone, so a wait behind the queue would hold the quiet machine for
/// nothing while the run measured nothing.
pub(crate) fn counted<'r>(
    records: Vec<&'r Record>,
    pid: u32,
    procs: Option<&crate::Procs>,
    available: Option<u64>,
) -> Vec<&'r Record> {
    let ancestors = procs.map(|p| p.ancestors(pid)).unwrap_or_default();
    let kin = |r: &Record| kin_of(r, pid, &ancestors);
    let measuring = records.iter().any(|r| {
        r.accounting == crate::Accounting::Holder
            && r.pid == pid
            && r.held.is_some()
            && r.need.quiet.is_some()
    });
    let inside = measuring || records.iter().any(|r| r.held.is_some() && kin(r));
    let sure = if inside && !measuring {
        sure_leads(&records, (pid, &ancestors), procs, available)
    } else {
        Vec::new()
    };
    records
        .into_iter()
        .filter(|r| !kin(r) && (!inside || r.held.is_some() || sure.contains(&r.order())))
        .collect()
}

/// Whether `r` is a claim of the process `pid`'s kin: one its ancestors (`ancestors`) hold, or a
/// held one that credits it or an ancestor ([`credits`]), which its own admission leaves out.
fn kin_of(r: &Record, pid: u32, ancestors: &[u32]) -> bool {
    r.accounting == crate::Accounting::Holder
        && (ancestors.contains(&r.pid) || credits(r, pid, ancestors))
}

/// The SURE leads among `records` ([`counted`]), by their order: the waiters that lead, in rank
/// and arrival order, while each fits, its need with its margin, in the room it would have once
/// every held claim none of its kin but the PARENTS has ended. A parent is a held claim whose
/// holder is a waiting process (the asking waiter, `asker`, with its ancestors, among them) or one
/// of its ancestors in `procs`, or one that credits a waiting process or an ancestor
/// ([`credits`]), each its kin: it may end only once that waiter has run. That room is what is
/// `available` now, less what each parent's holder has yet to use of its claims, plus what each
/// other holder's tree uses now, at most its claims, which its end gives back; a claim no tree
/// measures gives back only its reservation. A parent held ALONE leaves no lead room
/// ([`Short::Alone`]). The first lead that does not fit ends them, since each lead waits for the
/// leads ahead of it. With no process sweep or no reading of memory, none is sure.
fn sure_leads(
    records: &[&Record],
    asker: (u32, &[u32]),
    procs: Option<&crate::Procs>,
    available: Option<u64>,
) -> Vec<(u64, u32, u64)> {
    let (Some(procs), Some(available)) = (procs, available) else {
        return Vec::new();
    };
    let mut leads: Vec<&Record> = records
        .iter()
        .copied()
        .filter(|r| r.held.is_none() && r.need.leads)
        .collect();
    if leads.is_empty() {
        return Vec::new();
    }
    leads.sort_unstable_by_key(|r| (r.need.rank, r.order()));
    let mut lines: HashMap<u32, Vec<u32>> = HashMap::from([(asker.0, asker.1.to_vec())]);
    for r in records.iter().filter(|r| r.held.is_none()) {
        lines.entry(r.pid).or_insert_with(|| procs.ancestors(r.pid));
    }
    let is_parent = |r: &Record| {
        r.accounting == crate::Accounting::Holder
            && lines
                .iter()
                .any(|(pid, line)| r.pid == *pid || kin_of(r, *pid, line))
    };
    let held: Vec<(&Record, bool)> = records
        .iter()
        .copied()
        .filter(|r| r.held.is_some())
        .map(|r| (r, is_parent(r)))
        .collect();
    let mut roots: Vec<u32> = held
        .iter()
        .filter(|(r, _)| r.accounting == crate::Accounting::Holder)
        .map(|(r, _)| r.pid)
        .collect();
    roots.sort_unstable();
    roots.dedup();
    let trees = procs.trees(&roots);
    let mut sure = Vec::with_capacity(leads.len());
    for lead in leads {
        let line = lines.get(&lead.pid).map_or(&[][..], Vec::as_slice);
        let theirs: Vec<(&Record, bool)> = held
            .iter()
            .copied()
            .filter(|(r, _)| !kin_of(r, lead.pid, line))
            .collect();
        if theirs
            .iter()
            .any(|(r, parent)| *parent && r.held.is_some_and(|h| h.alone))
        {
            break;
        }
        // Each holder's claims none of the lead's kin, and whether one of them is a parent.
        let mut holders: HashMap<u32, (u64, bool)> = HashMap::new();
        for (r, parent) in theirs
            .iter()
            .filter(|(r, _)| r.accounting == crate::Accounting::Holder)
        {
            let slot = holders.entry(r.pid).or_default();
            slot.0 = slot.0.saturating_add(r.need.mem);
            slot.1 |= *parent;
        }
        let (mut back, mut owed) = (0u64, 0u64);
        for (pid, (claimed, parent)) in holders {
            let used = trees.get(&pid).map_or(0, |t| t.footprint);
            if parent {
                owed = owed.saturating_add(claimed.saturating_sub(used));
            } else {
                back = back.saturating_add(used.min(claimed));
            }
        }
        let room = available.saturating_add(back).saturating_sub(owed);
        if room < lead.need.mem.saturating_add(lead.need.margin) {
            break;
        }
        sure.push(lead.order());
    }
    sure
}

/// The quiet claim whose reservation holds `mine` back at `now` (unix ms), among `records` (the
/// claims `counted` holds it to), and the most that reservation still lasts: one that asked
/// before `mine` and reserves now ([`Record::reserves`]), whose holder is none of `mine`'s kin
/// (`kin`: `mine`'s own process or a descendant of it, whose timed work is part of `mine`'s run;
/// a claim of `mine`'s ancestors, whose timed work `mine` is, `counted` has left out already).
/// Only claimed work is held back, a need of memory, cores or disk: quiet claims take turns
/// among themselves already, and a need of nothing adds no load.
pub(crate) fn reserving<'r>(
    mine: &Record,
    records: &[&'r Record],
    kin: &dyn Fn(u32) -> bool,
    now: u64,
) -> Option<(&'r Record, Duration)> {
    let need = &mine.need;
    if need.quiet.is_some() || (need.mem == 0 && need.cores == 0 && need.disk == 0) {
        return None;
    }
    records
        .iter()
        .filter(|r| {
            (r.held.is_some() || (r.need.rank, r.order()) < (mine.need.rank, mine.order()))
                && (matches!(
                    r.accounting,
                    crate::Accounting::Whole | crate::Accounting::CohortOwned
                ) || !kin(r.pid))
        })
        .find_map(|r| r.reserves(now).map(|left| (*r, left)))
}

/// What a look that admits a claim grants it ([`decide`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Grant {
    /// The cores it is granted (CORES-GRANT, [`ranked_grant`]).
    pub(crate) cores: u32,
    /// How its memory was admitted alone; `None` beside other claims.
    pub(crate) alone: Option<Alone>,
}

/// How a memory claim was admitted ALONE ([`Claim::alone`], the rule in [`mod@admit`]): no other
/// claim held memory or waited ahead for it, and it asked to be ([`Need::alone`]), or what was
/// available did not hold its need, below what can ever be available ([`Reading::reachable`]),
/// short of memory no claim held. No other memory claim is
/// admitted while it is held ([`Short::Alone`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Alone {
    /// Its memory, its margin included, bytes.
    pub need: u64,
    /// What was available at the look that admitted it, bytes; `None` where unread.
    pub available: Option<u64>,
    /// Whether it asked to run alone ([`Need::alone`]).
    pub asked: bool,
}

impl fmt::Display for Alone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let available = self.available.map_or_else(|| "?".to_owned(), gib);
        if self.asked {
            write!(
                f,
                "admitted alone, as it asked: it needs {} ({available} available) and no other \
                 claim held memory, so no other memory claim is admitted while it runs",
                gib(self.need)
            )
        } else {
            write!(
                f,
                "admitted alone: it needs {} and {available} was available, short of memory no \
                 claim holds (other work on {HOST}), so no other memory claim is admitted while \
                 it runs",
                gib(self.need)
            )
        }
    }
}

/// Whether `need` fits now beside every claim ahead of the waiter at `mine` (held, or waiting ahead: one that leads where `need` does not ([`Need::leads`]), else one of a higher rank or the same rank since earlier, less the disk of a waiter no claim's end can admit: [`reservations`]), and, for a need of memory, under memory pressure short of critical ([`Short::Pressure`]), with no claim held alone ([`Short::Alone`]), or alone where it is short only of memory no claim holds and either asked to be or is below what can ever be available (ALONE, [`mod@admit`]): `Ok(what it is granted)`, or what it is short of. A waiter of `mine`'s own process (`mine.1`) is not ahead of its memory.
pub(crate) fn decide(
    need: &Need,
    mine: (u64, u32, u64),
    reading: &Reading,
    others: &[Other<'_>],
    now: u64,
) -> Result<Grant, Shortfall> {
    let ahead: Vec<&Other<'_>> = others
        .iter()
        .filter(|o| {
            o.record.held.is_some()
                || (!o.record.need.leads, o.record.need.rank, o.record.order())
                    < (!need.leads, need.rank, mine)
        })
        .collect();
    let volume = reading.volume.as_ref();
    let reserves = reservations(&ahead, volume);
    // An ordinary same-process waiter is ordered by its holder and reserves no memory
    // against this one. Independently owned cohort budgets never use that exemption.
    let sibling = |o: &Other<'_>| {
        o.record.accounting != crate::Accounting::CohortOwned
            && o.record.held.is_none()
            && o.record.pid == mine.1
    };
    let mut shorts = Vec::new();
    if need.rank == crate::Rank::Background
        && others
            .iter()
            .any(|o| o.record.need.rank == crate::Rank::Release)
    {
        shorts.push(Short::Release);
    }
    let (mut want_mem, mut want_disk, mut want_quiet) = (false, false, false);
    // The scratch claims held on the volume a disk need beyond the room waits for
    // ([`Short::Disk`]).
    let mut held_disk: Vec<(u64, u32, u64)> = Vec::new();
    let mut want_alone = None;
    let mut alone = None;
    if need.mem > 0 && reading.pressure == Some(aterm_sysprobe::MemoryLevel::Critical) {
        shorts.push(Short::Pressure);
    }
    if need.mem > 0 {
        let wanted = need.mem.saturating_add(need.margin);
        if let Some(o) = ahead
            .iter()
            .find(|o| o.record.held.is_some_and(|h| h.alone))
        {
            want_alone = Some(o.record.order());
            shorts.push(Short::Alone {
                pid: o.record.pid,
                what: o.record.need.what.clone(),
            });
        } else if let Some(available) = reading
            .available
            .or(reading.physical)
            .or(need.alone.then_some(0))
        {
            let reserved: u64 = reserves
                .iter()
                .zip(&ahead)
                .filter(|(_, o)| !sibling(o))
                .map(|(r, _)| r.mem)
                .fold(0, u64::saturating_add);
            let fits = !need.alone && available.saturating_sub(reserved) >= wanted;
            let claimed = ahead.iter().any(|o| o.record.need.mem > 0 && !sibling(o));
            let reachable = reading.reachable().is_none_or(|reach| wanted < reach);
            if !fits && !claimed && (need.alone || (need.margin == 0 && reachable)) {
                alone = Some(Alone {
                    need: wanted,
                    available: reading.available.or(reading.physical),
                    asked: need.alone,
                });
            } else if !fits {
                want_mem = true;
                shorts.push(Short::Memory {
                    need: need.mem,
                    margin: need.margin,
                    available,
                    reserved,
                });
            }
        }
    }
    if need.disk > 0
        && let Some(v) = volume
    {
        let reserved: u64 = reserves
            .iter()
            .filter_map(|r| r.disk)
            .fold(0, u64::saturating_add);
        if v.above_floor().saturating_sub(reserved) < need.disk {
            want_disk = true;
            let scratch: Vec<&&Other<'_>> =
                ahead.iter().filter(|o| scratch_on(o, mine.1, v)).collect();
            let back = scratch
                .iter()
                .map(|o| could_give_back(o))
                .fold(0, u64::saturating_add);
            if beyond_room(need.disk, v.above_floor())
                && !beyond_room(need.disk, v.above_floor().saturating_add(back))
            {
                held_disk = scratch.iter().map(|o| o.record.order()).collect();
            }
            shorts.push(Short::Disk {
                need: need.disk,
                mount: v.mount.clone(),
                above_floor: v.above_floor(),
                floor: v.floor,
                reserved,
                held: held_disk.clone(),
            });
        }
    }
    if let Some(max) = need.quiet {
        if let Some(o) = ahead.iter().find(|o| o.record.need.quiet.is_some()) {
            want_quiet = true;
            shorts.push(Short::Quiet {
                pid: o.record.pid,
                what: o.record.need.what.clone(),
                held: o.record.held.is_some(),
            });
        }
        if let Some(load1) = reading.load1
            && load1 > max
        {
            shorts.push(Short::Load { load1, max });
        }
    }
    let cores = ranked_grant(need, mine, reading, others, now);
    if need.cores > 0 && cores == 0 {
        shorts.push(Short::Cores);
    }
    if !shorts.is_empty() {
        let overruns = overruns_of(others);
        let holders = ahead
            .iter()
            .zip(&reserves)
            .filter(|(o, r)| {
                (want_mem && o.record.need.mem > 0 && !sibling(o))
                    || (want_disk && r.disk.unwrap_or(0) > 0)
                    || held_disk.contains(&o.record.order())
                    || (want_quiet && o.record.need.quiet.is_some())
                    || want_alone == Some(o.record.order())
            })
            .map(|(o, r)| Holder {
                pid: o.record.pid,
                what: o.record.need.what.clone(),
                checkout: o.record.checkout.clone(),
                held: o.record.held.is_some(),
                age: Duration::from_millis(now.saturating_sub(o.record.start_ms)),
                mem: r.mem,
                claimed: o.record.need.mem,
                alone: o.record.held.is_some_and(|h| h.alone),
                disk: r.disk.unwrap_or(0),
                quiet: o.record.need.quiet.is_some(),
                order: o.record.order(),
                stalled: None,
                credit: o.record.credit.clone(),
                overrun: overruns
                    .get(&o.record.pid)
                    .filter(|_| {
                        o.record.accounting == crate::Accounting::Holder
                            && o.record.held.is_some()
                            && o.record.need.mem > 0
                    })
                    .copied(),
            })
            .collect();
        return Err(Shortfall { shorts, holders });
    }
    Ok(Grant { cores, alone })
}

/// Bound every new grant by physical room. Earlier waiters in rank/arrival order reserve
/// their requested room without changing any running grant.
fn ranked_grant(
    need: &Need,
    mine: (u64, u32, u64),
    reading: &Reading,
    others: &[Other<'_>],
    now: u64,
) -> u32 {
    let held = others
        .iter()
        .filter_map(|o| o.record.held.map(|h| h.cores))
        .fold(0u32, u32::saturating_add);
    let reserved = others
        .iter()
        .filter(|o| {
            o.record.held.is_none() && (o.record.need.rank, o.record.order()) < (need.rank, mine)
        })
        .map(|o| o.record.need.cores)
        .fold(0u32, u32::saturating_add);
    let room = reading.cores.saturating_sub(held).saturating_sub(reserved);
    if need.rank == crate::Rank::Release {
        return need.cores.min(room);
    }
    grant_iter(
        need.cores,
        reading,
        others.iter().filter(|o| {
            o.record.held.is_some() || (o.record.need.rank, o.record.order()) < (need.rank, mine)
        }),
        now,
    )
    .min(room)
}

/// The fair share before `ranked_grant` applies reservations and the physical core bound.
/// - The room is the cores less what every held claim was granted. Past about half
///   the bound of unclaimed load that no longer keeps a quiet waiter's room; its reservation
///   ([`Short::Reserved`]) does, holding back a cores claim made after it like any claimed work.
/// - The parties are this claim, earlier claims of cores not yet admitted (whatever each waits
///   for: memory, disk, cores or a reservation), and the work no claim
///   holds, counted in machines' worth: the 5-minute load less what the held claims' jobs have
///   put into it ([`LOAD5_TIME_CONSTANT`]), over the cores, since a plain build runs a job a core.
///   A load that reads as no number is the load unread.
///
/// A share is rounded to the nearest core, at least one, never more than asked; `0` when it
/// asked for none. On 14 cores: alone on an idle machine, all 14; at a 5-minute load of 60, 3
/// alone and 2 each for two claims; 1 only past a load of about 117, or once the held claims
/// hold nearly every core. Unclaimed work is not held to its share (a plain build runs its job
/// a core whatever the load), so under load a grant is what is fair, not what the scheduler
/// gives it. CAP-1's first rule granted the idle cores alone: 1 at any 5-minute load above 12
/// on 14 cores, which lengthened the claimed work without quieting the machine.
fn grant_iter<'a, 'b: 'a>(
    asked: u32,
    reading: &Reading,
    others: impl Iterator<Item = &'a Other<'b>>,
    now: u64,
) -> u32 {
    if asked == 0 {
        return 0;
    }
    let cores = f64::from(reading.cores.max(1));
    let tau = LOAD5_TIME_CONSTANT.as_secs_f64();
    let (mut held, mut in_load, mut waiting) = (0.0, 0.0, 0.0);
    for o in others.filter(|o| o.record.need.cores > 0) {
        match o.record.held {
            Some(h) => {
                let jobs = f64::from(h.cores);
                let age = Duration::from_millis(now.saturating_sub(h.admitted_ms)).as_secs_f64();
                held += jobs;
                in_load += jobs * (1.0 - (-age / tau).exp());
            }
            None => waiting += 1.0,
        }
    }
    let unclaimed = reading.load5.map_or(0.0, |l| (l - in_load).max(0.0));
    let share = (cores - held).max(0.0) / (1.0 + waiting + unclaimed / cores);
    if share.is_finite() && share >= 1.0 {
        // At least 1.0, and the cores bound it: a whole number of cores.
        (share.round() as u32).min(asked)
    } else {
        1
    }
}

/// How a quiet claim started ([`Claim::quiet`]), for its tool's report.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QuietStart {
    /// The 1-minute load at the look that admitted it.
    pub load1: Option<f64>,
    /// Its bound.
    pub max: f64,
    /// How busy the GPU was at that look, percent ([`Short::Gpu`]).
    pub gpu: Option<u8>,
    /// Whether it started at the end of its patience ([`crate::Patience::start`]), the load
    /// or the GPU still above its bound.
    pub at_bound: bool,
    /// How long it reserved the machine before it started; zero when it never did.
    pub reserved: Duration,
}

impl fmt::Display for QuietStart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let started = if self.at_bound {
            "started at its bound"
        } else {
            "started"
        };
        match self.load1 {
            Some(l) => {
                let to = if l > self.max { ">" } else { "<=" };
                write!(f, "{started} under load {l:.1} {to} {:.1}", self.max)?;
            }
            None => write!(f, "{started} with the load unread")?,
        }
        if let Some(busy) = self.gpu {
            write!(f, ", GPU {busy}% busy")?;
            if busy > crate::QUIET_GPU_BUSY {
                write!(f, " > {}%", crate::QUIET_GPU_BUSY)?;
            }
        }
        if !self.reserved.is_zero() {
            write!(
                f,
                ", having reserved the machine for {}",
                span(self.reserved)
            )?;
        }
        Ok(())
    }
}

/// An admitted claim. Ordinary claims end with their holder; armed or bound cohort claims
/// retain their complete reservation independently of holder death and descriptor closure.
#[derive(Debug)]
pub struct Claim {
    _background: Option<crate::background::Guard>,
    laid: Laid,
    record: Record,
    waited: Duration,
    /// When its admission first entered the queue, unix ms ([`Self::asked_ms`]).
    asked_ms: u64,
    /// Where its measured writes lie in its file, once written ([`Self::wrote`]).
    wrote_at: Option<u64>,
    /// How a quiet claim started; `None` for any other, a load-only one a held quiet claim
    /// covers, or one `--admit now` admitted.
    quiet: Option<QuietStart>,
    /// How its memory was admitted alone; `None` beside other claims.
    alone: Option<Alone>,
    /// What it was short of at the last look that found it waiting; `None` when it never waited.
    short: Option<Shortfall>,
    /// This process actually admitted this claim; descriptor adoption supplies no new timing observation.
    entry_custody: bool,
    /// A loaded claim of its process or an ancestor spent its patience ([`loaded_cover`]): a
    /// stated entry decides on its first window, as for a loaded admission.
    spent: bool,
}

impl Claim {
    /// Consume this original ordinary, admitted, non-covering memory-only claim into
    /// close-only custody. Its exact declared peak remains in the original ledger until the
    /// final actual guard closes; detached accounting grants no numeric holder discounts.
    ///
    /// # Errors
    /// Returns this original claim with the error, including all cohort writer/recovery
    /// custody unchanged when that claim kind is unsupported. A partial ordinary intent
    /// remains error-closed and can only be retried by this same original claim.
    #[cfg(unix)]
    #[expect(
        clippy::result_large_err,
        reason = "returning original Claim preserves its exact writer and pending custody on refusal"
    )]
    pub fn into_retained_guard(mut self) -> Result<crate::RetainedClaim, (Self, io::Error)> {
        if self.laid.is_cohort()
            || !self.entry_custody
            || self.record.accounting != crate::Accounting::Holder
            || self.record.held.is_none()
            || !crate::ledger::memory_only(&self.record.need)
        {
            return Err((
                self,
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "only an original admitted non-covering ordinary memory-only claim can detach",
                ),
            ));
        }
        let prepared = (|| -> io::Result<()> {
            if !self.laid.detach_pending() {
                let record = crate::retained_claim::read_record(self.descriptor()?, self.path())?;
                crate::retained_claim::check_record(
                    &record,
                    self.path(),
                    crate::Accounting::Holder,
                )?;
                if record != self.record {
                    return Err(io::Error::other(
                        "the original admitted record changed before detachment",
                    ));
                }
            }
            self.laid.detach_memory()
        })();
        if let Err(error) = prepared {
            return Err((self, error));
        }
        self.record.accounting = crate::Accounting::Detached;
        let path = self.path().to_owned();
        let Self { laid, record, .. } = self;
        Ok(crate::RetainedClaim::original(
            laid.into_close_only(),
            path,
            record,
        ))
    }

    #[cfg(unix)]
    pub(crate) fn check_retained_description(&self) -> io::Result<()> {
        if !self.entry_custody {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "retained CPU binding requires the original admission",
            ));
        }
        self.laid.check_retained_description()
    }

    #[cfg(unix)]
    pub(crate) fn check_cohort_namespace(
        &self,
        namespace: &crate::cohort_namespace::Namespace,
    ) -> io::Result<()> {
        self.laid.check_cohort_namespace(namespace)
    }

    /// Its measured hold: the actual admission time, its patience and its quiet bound, while that
    /// hold lasts.
    fn timing_hold(&self) -> Result<(u64, crate::Patience, f64), crate::TimingIssue> {
        use crate::TimingIssue;
        let admitted = self
            .record
            .timing
            .admitted_ms
            .filter(|at| Some(*at) == self.record.held.map(|h| h.admitted_ms))
            .ok_or(TimingIssue::UnknownAdmission)?;
        let patience = self
            .record
            .need
            .patience
            .ok_or(TimingIssue::UnknownAdmission)?;
        let now = now_ms();
        if now < admitted || now >= admitted.saturating_add(millis(patience.hold)) {
            return Err(TimingIssue::Expired);
        }
        let max = self
            .record
            .need
            .quiet
            .ok_or(TimingIssue::UnknownAdmission)?;
        Ok((admitted, patience, max))
    }

    /// The observed resource-admission CPU interval, not a fresh measurement-entry permit.
    /// # Errors
    /// Missing/forced/loaded/adopted provenance, a weak policy, unavailable load, or expired hold.
    pub fn timing_admission(&self) -> Result<crate::CpuWindow, crate::TimingIssue> {
        match self.timing_protection()? {
            crate::Protection::Qualified(window) => Ok(window),
            crate::Protection::Loaded(_) => Err(crate::TimingIssue::Loaded),
        }
    }

    /// What its measured admission protects: a quiet window ([`crate::Protection::Qualified`]), or
    /// a machine that stayed busy through its patience, for a need that runs loaded
    /// ([`crate::Protection::Loaded`]): its work runs and states it.
    /// # Errors
    /// Missing/forced/adopted provenance, a weak policy, unavailable load, or expired hold.
    pub fn timing_protection(&self) -> Result<crate::Protection, crate::TimingIssue> {
        use crate::{QuietPolicy, TimingCause, TimingIssue};
        if !self.entry_custody || self.record.need.quiet_policy != QuietPolicy::Timing {
            return Err(TimingIssue::UnknownAdmission);
        }
        match self.record.timing.cause {
            TimingCause::Now => return Err(TimingIssue::Now),
            TimingCause::Unknown => return Err(TimingIssue::UnknownAdmission),
            TimingCause::Wait | TimingCause::Loaded => {}
        }
        let (_, _, max) = self.timing_hold()?;
        let load1 = self
            .record
            .timing
            .admission_load1
            .filter(|n| n.is_finite() && *n >= 0.0)
            .ok_or(TimingIssue::LoadUnknown)?;
        let window = self
            .record
            .timing
            .admission
            .ok_or(TimingIssue::UnknownAdmission)?;
        if self.record.timing.cause == TimingCause::Loaded {
            return Ok(crate::Protection::Loaded(crate::Observed {
                window,
                load1,
                max,
            }));
        }
        if load1 > max {
            return Err(TimingIssue::LoadUnknown);
        }
        if window.ready() {
            Ok(crate::Protection::Qualified(window))
        } else {
            Err(TimingIssue::UnknownAdmission)
        }
    }

    /// Observe CPU readiness after all compilation, immediately before measurement callbacks.
    /// A retained parent/admission sample is never reused as this observation.
    /// # Errors
    /// Unknown/forced/loaded/adopted admission, expired protection, unavailable/reset CPU or load,
    /// a machine still busy at its entry's patience ([`crate::TimingIssue::Loaded`]) or with no
    /// window then ([`crate::TimingIssue::Blind`]), interruption, or failure to retain the
    /// observation. No callback has run on failure.
    pub fn measurement_entry(&mut self) -> Result<crate::CpuWindow, crate::TimingIssue> {
        let watch = aterm_toolchain::interrupt::Watch::enter();
        self.measurement_entry_on(&crate::Live, Pace::default(), &mut |d| {
            watch.nap(d, &|| false)
        })
    }

    pub(crate) fn measurement_entry_on(
        &mut self,
        machine: &dyn Machine,
        pace: Pace,
        sleep: &mut dyn FnMut(Duration) -> bool,
    ) -> Result<crate::CpuWindow, crate::TimingIssue> {
        self.entry_on(machine, pace, sleep, false)
            .map(|entry| entry.window)
    }

    /// [`Self::measurement_entry`] for work that runs on a busy machine and states it
    /// ([`crate::AtBound::RunLoaded`]): a loaded admission enters on one fresh window, and a quiet
    /// one whose entry stays busy through its entry's patience enters then; each says why in
    /// [`crate::StatedEntry::unqualified`] (owner ruling 2026-11-05: a busy machine never blocks a
    /// release).
    /// # Errors
    /// What [`Self::measurement_entry`] refuses, but a busy machine: no window at its entry's
    /// patience ([`crate::TimingIssue::Blind`]) included.
    pub fn stated_entry(&mut self) -> Result<crate::StatedEntry, crate::TimingIssue> {
        let watch = aterm_toolchain::interrupt::Watch::enter();
        self.stated_entry_on(&crate::Live, Pace::default(), &mut |d| {
            watch.nap(d, &|| false)
        })
    }

    pub(crate) fn stated_entry_on(
        &mut self,
        machine: &dyn Machine,
        pace: Pace,
        sleep: &mut dyn FnMut(Duration) -> bool,
    ) -> Result<crate::StatedEntry, crate::TimingIssue> {
        self.entry_on(machine, pace, sleep, true)
    }

    /// The measurement entry: strict ([`Self::measurement_entry`]) or `stated`
    /// ([`Self::stated_entry`]).
    fn entry_on(
        &mut self,
        machine: &dyn Machine,
        pace: Pace,
        sleep: &mut dyn FnMut(Duration) -> bool,
        stated: bool,
    ) -> Result<crate::StatedEntry, crate::TimingIssue> {
        use crate::{Observed, Protection, TimingIssue, Unqualified};
        let protection = if stated {
            self.timing_protection()?
        } else {
            Protection::Qualified(self.timing_admission()?)
        };
        let admitted_loaded = match protection {
            Protection::Qualified(_) => None,
            Protection::Loaded(observed) => Some(observed),
        };
        let (admitted, patience, bound) = self.timing_hold()?;
        let expires = admitted.saturating_add(millis(patience.hold));
        self.record.timing.entry = None;
        self.record.timing.entry_load1 = None;
        self.laid
            .append(&Record::timing_line(self.record.timing))
            .map_err(|_| TimingIssue::Persistence)?;
        let mut unqualified: Vec<Unqualified> = admitted_loaded
            .map(Unqualified::Admission)
            .into_iter()
            .collect();
        let mut previous: Option<crate::CpuPoint> = None;
        // The last read's window, if it formed one, with its load: what a busy entry enters on.
        let mut last: Option<Observed> = None;
        // The entry's patience counts on the counters' own clock, from its first point.
        let mut began = None;
        loop {
            let now = now_ms();
            if now < admitted || now >= expires {
                return Err(TimingIssue::Expired);
            }
            let point = machine.cpu_point().ok_or(TimingIssue::Unavailable)?;
            let first = *began.get_or_insert(point.at);
            let spent = point
                .at
                .checked_duration_since(first)
                .is_some_and(|d| d >= patience.start);
            let window = previous
                .as_ref()
                .and_then(|from| crate::CpuWindow::between(from, &point));
            let had_previous = previous.replace(point).is_some();
            match window {
                Some(window) => {
                    let load1 = machine
                        .read(None)
                        .load1
                        .filter(|n| n.is_finite() && *n >= 0.0)
                        .ok_or(TimingIssue::LoadUnknown)?;
                    let observed = Observed {
                        window,
                        load1,
                        max: bound,
                    };
                    if window.ready() && load1 <= bound {
                        return self.enter(observed, unqualified, expires);
                    }
                    // A loaded admission, or one a loaded claim above it spared its wait, has
                    // spent its patience already: its first window decides, and a busy one is
                    // stated.
                    if stated && (admitted_loaded.is_some() || self.spent) {
                        unqualified.push(Unqualified::Entry(observed));
                        return self.enter(observed, unqualified, expires);
                    }
                    last = Some(observed);
                }
                // A read after another that formed no window: the counters broke.
                None if had_previous => last = None,
                None => {}
            }
            // At its entry's patience, once a window could have formed, the last read decides.
            if had_previous && spent {
                return match last {
                    Some(observed) if stated => {
                        unqualified.push(Unqualified::Entry(observed));
                        self.enter(observed, unqualified, expires)
                    }
                    Some(_) => Err(TimingIssue::Loaded),
                    None => Err(TimingIssue::Blind),
                };
            }
            if sleep(pace.poll) {
                return Err(TimingIssue::Interrupted);
            }
        }
    }

    /// Enter the measurement on `observed`, retained under the claim with why it is unqualified.
    fn enter(
        &mut self,
        observed: crate::Observed,
        unqualified: Vec<crate::Unqualified>,
        expires: u64,
    ) -> Result<crate::StatedEntry, crate::TimingIssue> {
        if now_ms() >= expires {
            return Err(crate::TimingIssue::Expired);
        }
        self.record.timing.entry = Some(observed.window);
        self.record.timing.entry_load1 = Some(observed.load1);
        self.laid
            .append(&Record::timing_line(self.record.timing))
            .map_err(|_| crate::TimingIssue::Persistence)?;
        Ok(crate::StatedEntry {
            window: observed.window,
            load1: observed.load1,
            unqualified,
        })
    }
    /// How its memory was admitted ALONE ([`Alone`]): while it is held, no other memory claim is
    /// admitted. `None` for a claim admitted beside others.
    #[must_use]
    pub const fn alone(&self) -> Option<Alone> {
        self.alone
    }

    /// What it was short of at the last look that found it waiting, and who held that: what its
    /// wait was for. `None` for a claim that never waited, or waited only for the ledger's lock.
    #[must_use]
    pub const fn waited_for(&self) -> Option<&Shortfall> {
        self.short.as_ref()
    }

    /// The cores it was granted: its share of the cores the held claims leave, at least one and never more than it asked (CORES-GRANT); 0 when it asked for none.
    #[must_use]
    pub fn cores(&self) -> u32 {
        self.record.held.map_or(self.record.need.cores, |h| h.cores)
    }

    /// Its record, as other sessions read it.
    #[must_use]
    pub const fn record(&self) -> &Record {
        &self.record
    }

    /// How long it waited, by this process's monotonic clock (`Instant`, which on macOS stops
    /// while the Mac sleeps): what its lines say. Zero for an adopted claim. Its telemetry rows
    /// say its wait by the wall clock instead ([`crate::telemetry::wait`]).
    #[must_use]
    pub const fn waited(&self) -> Duration {
        self.waited
    }

    /// When its admission first entered the ledger's queue, unix ms: its record's `start_ms`,
    /// the stamp its file was laid with, except for a need a relief resized and entered again,
    /// whose record is the later entry's while its wait counts from the first. An adopted claim
    /// reads its record's. With its grant's `admitted_ms`, the wait its telemetry rows say
    /// ([`crate::telemetry::wait`]).
    #[must_use]
    pub const fn asked_ms(&self) -> u64 {
        self.asked_ms
    }

    /// TEST: the wall clock ran `by_ms` ahead of this process's monotonic one while it waited (a
    /// Mac that slept through the wait): its ask and its record's laying read that much earlier,
    /// its [`Self::waited`] as this process's clock read it.
    #[cfg(test)]
    pub(crate) fn slept_while_waiting(&mut self, by_ms: u64) {
        self.asked_ms -= by_ms;
        self.record.start_ms -= by_ms;
    }

    /// How a quiet claim started: the load and the GPU, whether at its bound, how long it
    /// reserved the machine. `None` for any other claim, a load-only one a held quiet claim
    /// covers, or one `--admit now` admitted.
    #[must_use]
    pub const fn quiet(&self) -> Option<QuietStart> {
        self.quiet
    }

    /// Its file in the ledger.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.laid.path()
    }

    /// Credit it with `pids`, processes outside its holder's tree that run its work (a VM the
    /// system parents to launchd, not to the run that booted it): their trees' use counts
    /// against its reservation alone, so memory they already hold is not reserved again. Each
    /// call names the whole set, at most [`CREDIT_MAX`]; pids 0 and 1 and this process (whose
    /// tree is the holder's already) are left out.
    ///
    /// # Errors
    /// The record could not be written, or the cohort namespace refuses numeric pid credit;
    /// the claim keeps the credit it had.
    pub fn credit(&mut self, pids: &[u32]) -> io::Result<()> {
        #[cfg(unix)]
        if self.laid.is_cohort() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "cohort claims require actual child binding, not numeric pid credit",
            ));
        }
        let me = std::process::id();
        let mut pids: Vec<u32> = pids
            .iter()
            .copied()
            .filter(|p| *p > 1 && *p != me)
            .collect();
        pids.sort_unstable();
        pids.dedup();
        pids.truncate(CREDIT_MAX);
        self.laid.append(&Record::credit_line(&pids))?;
        self.record.credit = pids;
        Ok(())
    }

    /// A close-on-exec copy of its file's descriptor, which shares its lock: what a holder that
    /// keeps the claim for another process is handed ([`Self::hand_off`], [`Self::adopt`]).
    ///
    /// # Errors
    /// The `dup`'s.
    #[cfg(unix)]
    pub fn descriptor(&self) -> io::Result<std::os::fd::OwnedFd> {
        self.laid.descriptor()
    }

    /// HAND IT OFF to the holder a copy of its descriptor was given ([`Self::descriptor`]): this
    /// process forgets it, neither letting its lock go nor removing its file, so the claim is
    /// held for as long as the holder holds that copy and ends when the holder lets it go or
    /// dies for an ordinary claim. Cohort custody also persists until proven group drain.
    /// Its own descriptor closes at this process's exec or exit (it is close-on-exec),
    /// which lets nothing go. Dropping a claim instead lets the lock go for every copy.
    #[cfg(unix)]
    pub fn hand_off(self) {
        self.laid.hand_off();
    }

    /// ADOPT the claim at `path` whose descriptor `fd` is a copy of ([`Self::descriptor`], handed
    /// on by [`Self::hand_off`]'s process): `fd` must open the file at `path`, a fresh open of it
    /// must find its lock held, and `fd` must take that lock, so the holder is `fd`'s description
    /// ([`Ledger`]'s proofs), before its record is read; a refusal lets nothing go. The claim is
    /// then this process's. Dropping an ordinary claim removes the file; cohort binding and
    /// unknown custody survive adoption and remain until drain. Adoption records no new
    /// measured writes and no admission's details.
    ///
    /// # Errors
    /// A proof that fails, or its record unread.
    #[cfg(unix)]
    pub fn adopt(fd: std::os::fd::OwnedFd, path: &Path) -> io::Result<Claim> {
        let (record, laid) = Laid::adopt(fd, path, None)?;
        Ok(Claim {
            _background: crate::background::Guard::start(
                &Ledger::at(
                    path.parent()
                        .ok_or_else(|| io::Error::other("claim has no parent"))?,
                ),
                record.need.rank,
            )?,
            laid,
            asked_ms: record.start_ms,
            record,
            waited: Duration::ZERO,
            wrote_at: None,
            quiet: None,
            alone: None,
            short: None,
            entry_custody: false,
            spent: false,
        })
    }

    /// Say that its work has written `bytes` toward its disk need since it was admitted, as its
    /// holder measured them (the growth of the directories it writes): the volume's free space
    /// has lost them already, so a waiter counts only the rest of the need as reserved.
    ///
    /// # Errors
    /// The claim file could not be written; the figure it held before stands.
    pub fn wrote(&mut self, bytes: u64) -> io::Result<()> {
        self.laid.wrote(&mut self.wrote_at, bytes)?;
        self.record.wrote = Some(bytes);
        Ok(())
    }

    /// A separate write capability for an authenticated controller transfer. This descriptor
    /// is never part of ordinary child guards; possessing a read-only guard cannot mint it.
    ///
    /// # Errors
    /// The claim is not cohort-aware, its durable state changed, or duplicating the writer fails.
    #[cfg(unix)]
    pub fn cohort_writer_descriptor(&self) -> io::Result<std::os::fd::OwnedFd> {
        self.laid.cohort_writer_descriptor()
    }

    /// Capture the original epoch and guard for a lifecycle journal before handing work to
    /// a child. Persisting this observation does not prove closure or grant a writer.
    /// # Errors
    /// The claim is legacy or its held namespace, physical origin or journal cannot validate.
    #[cfg(unix)]
    pub fn cohort_closure_ref(&self) -> io::Result<crate::CohortClosureRef> {
        self.laid.cohort_closure_ref()
    }

    /// Adopt cohort controller custody with both its actual read-only lock description and
    /// separately transferred read-write authority. A guard alone cannot create a writer.
    ///
    /// # Errors
    /// The guard, writer, original inode, shared lock, namespace or custody journal do not match.
    #[cfg(unix)]
    pub fn adopt_cohort(
        fd: std::os::fd::OwnedFd,
        writer: std::os::fd::OwnedFd,
        path: &Path,
    ) -> io::Result<Claim> {
        let (record, laid) = Laid::adopt(fd, path, Some(writer))?;
        Ok(Claim {
            _background: None,
            laid,
            asked_ms: record.start_ms,
            record,
            waited: Duration::ZERO,
            wrote_at: None,
            quiet: None,
            alone: None,
            short: None,
            entry_custody: false,
            spent: false,
        })
    }

    /// Durably require a later authenticated boot of this host to close spawned work.
    /// Call before the first ARM. Group death, signaling and holder death cannot close
    /// bound work in its original boot. This policy establishes no containment.
    /// An exact no-child cancellation still permits releasing an unused claim.
    ///
    /// # Errors
    /// The claim is not an admitted private cohort claim, has already armed, has uncertain
    /// custody, or the policy cannot be durably committed.
    #[cfg(unix)]
    pub fn require_boot_closure(&mut self) -> io::Result<()> {
        self.laid.require_boot_closure()
    }

    /// Durably ARM this admitted private cohort claim before passing the returned one-use
    /// attempt to the controlled spawn. A partial failure conservatively retains the budget.
    ///
    /// # Errors
    /// The claim is legacy, not admitted, already armed, has uncertain state, has a stale owner
    /// or namespace, or its journal cannot be durably written.
    #[cfg(unix)]
    pub fn arm_cohort(&mut self) -> io::Result<aterm_uds::spawnfd::BirthAttempt> {
        let attempt = self.laid.arm_cohort()?;
        if self.record.accounting != crate::Accounting::CohortOwned {
            self.record.accounting = crate::Accounting::Whole;
        }
        Ok(attempt)
    }

    /// Durably arm and bind one actual native child before acknowledging its launch.
    /// The command starts a dedicated group; this is not containment of descendants that
    /// deliberately escape that group. It does not transition the account's ledger.
    ///
    /// # Errors
    /// ARM or controlled spawn refuses, or durable BIND fails. A failed BIND returns the
    /// actual owned child in the error; a genuine no-child failure returns its cancellation
    /// receipt. The controller must recover those outcomes without inferring release from
    /// an error, a closed descriptor or the leader's exit.
    #[cfg(unix)]
    pub fn launch_cohort(
        &mut self,
        command: &mut aterm_uds::spawnfd::Command,
    ) -> Result<crate::cohort::launch::CohortChild, crate::cohort::launch::LaunchError> {
        crate::cohort::launch::launch(self, command)
    }

    /// Launch another owned child into this claim's already bound group. The original
    /// leader must still be live and owned here; a saved group number supplies no authority.
    /// Members share the full existing reservation, which remains charged until group drain.
    /// The returned child can run concurrently with the leader and other members.
    ///
    /// # Errors
    /// The claim's durable custody or original birth is stale, unknown or mismatched, the
    /// leader has ended or changed group, or native spawn refuses. Session-creating and
    /// tethered commands cannot join this group. No failure cancels or releases its claim.
    #[cfg(unix)]
    pub fn launch_cohort_member(
        &self,
        leader: &mut crate::cohort::launch::CohortChild,
        command: &mut aterm_uds::spawnfd::Command,
    ) -> io::Result<aterm_uds::spawnfd::Child> {
        crate::cohort::launch::launch_member(self, leader, command)
    }

    /// Launch a new host-owned dedicated process group under this claim's original root.
    /// Durable subgroup ARM precedes native spawn and BIND precedes success. The complete
    /// claim remains reserved until the root and every registered subgroup have drained;
    /// a pending unknown ARM retains it. This does not contain foreign escaping descendants.
    /// Subgroup admission remains bounded by the durable record's byte budget.
    ///
    /// # Errors
    /// The actual root is not live, current and original, custody is stale or full, native
    /// spawn fails, or persistence fails. `Unbound` preserves the real child for retry.
    #[cfg(unix)]
    pub fn launch_subgroup(
        &mut self,
        root: &mut crate::cohort::launch::CohortChild,
        command: &mut aterm_uds::spawnfd::Command,
    ) -> Result<crate::cohort::launch::CohortChild, crate::cohort::launch::LaunchError> {
        crate::cohort::launch::launch_subgroup(self, root, command)
    }

    /// Retry durable subgroup BIND using the actual one-use spawn receipt retained by an
    /// `Unbound` launch error. An error returns that same actual child again.
    ///
    /// # Errors
    /// The receipt or durable ARM does not match, the child was replaced, or persistence fails.
    #[cfg(unix)]
    pub fn bind_subgroup(
        &mut self,
        spawned: aterm_uds::spawnfd::Spawned,
    ) -> Result<crate::cohort::launch::CohortChild, crate::cohort::launch::LaunchError> {
        crate::cohort::launch::bind_subgroup(self, spawned)
    }

    /// Cancel only a subgroup attempt's exact controlled no-child receipt.
    ///
    /// # Errors
    /// The receipt does not match an outstanding subgroup ARM or durable cancellation fails.
    #[cfg(unix)]
    pub fn cancel_subgroup(&mut self, proof: aterm_uds::spawnfd::NotSpawned) -> io::Result<()> {
        self.laid.cancel_subgroup(proof)
    }

    #[cfg(unix)]
    pub(crate) fn check_subgroup_cancel(
        &self,
        proof: &aterm_uds::spawnfd::NotSpawned,
    ) -> io::Result<()> {
        self.laid.check_subgroup_cancel(proof)
    }

    /// Durably retire a registered actual subgroup only after its scoped kernel drain.
    /// The original root and all other scopes remain; covered snapshots bound sequential
    /// campaign history. BootOnly still retains the claim through this authenticated boot.
    ///
    /// # Errors
    /// The receipt is stale, foreign, replaced or unregistered, the group is live/unknown,
    /// or the retirement or snapshot cannot be durably committed.
    #[cfg(unix)]
    pub fn retire_subgroup(
        &mut self,
        subgroup: &crate::cohort::launch::CohortChild,
    ) -> io::Result<()> {
        crate::cohort::launch::retire_subgroup(self, subgroup)
    }

    #[cfg(unix)]
    pub(crate) fn arm_subgroup(
        &mut self,
        root: &crate::cohort::Cohort,
    ) -> io::Result<aterm_uds::spawnfd::BirthAttempt> {
        self.laid.arm_subgroup(root)
    }

    #[cfg(unix)]
    pub(crate) fn bind_subgroup_actual(
        &mut self,
        spawned: &mut aterm_uds::spawnfd::Spawned,
    ) -> io::Result<crate::cohort::Cohort> {
        self.laid.bind_subgroup(spawned)
    }

    #[cfg(unix)]
    pub(crate) fn retire_subgroup_actual(
        &mut self,
        key: &aterm_uds::spawnfd::BirthKey,
        cohort: &crate::cohort::Cohort,
    ) -> io::Result<()> {
        self.laid.retire_subgroup(key, cohort)
    }

    /// Bind the actual controlled child to its exact durable ARM before acknowledging work.
    /// Guard closure and holder death subsequently leave the entire reservation in the ledger.
    ///
    /// # Errors
    /// The actual child, attempt, owner, scope or namespace does not match, or durable binding
    /// fails. Such an error is no proof that no child started; keep custody and supervise drain.
    #[cfg(unix)]
    pub fn bind_cohort(
        &mut self,
        spawned: &mut aterm_uds::spawnfd::Spawned,
    ) -> io::Result<crate::cohort::Cohort> {
        self.laid.bind_cohort(spawned)
    }

    /// Cancel only this exact attempt's controlled no-child result in its original process.
    /// A generic spawn error, a decoded key and a stale fork copy supply no such authority.
    ///
    /// # Errors
    /// The genuine receipt does not match the current ARM/owner/scope, or persistence fails.
    #[cfg(unix)]
    pub fn cancel_cohort(&mut self, proof: aterm_uds::spawnfd::NotSpawned) -> io::Result<()> {
        self.laid.cancel_cohort(proof)?;
        if self.record.accounting != crate::Accounting::CohortOwned {
            self.record.accounting = crate::Accounting::Holder;
        }
        Ok(())
    }

    #[cfg(unix)]
    pub(crate) fn check_cohort_cancel(
        &self,
        proof: &aterm_uds::spawnfd::NotSpawned,
    ) -> io::Result<()> {
        self.laid.check_cohort_cancel(proof)
    }

    /// Complete custody only after conclusive kernel drain, recording settlement durably.
    /// A locally retained failed BIND is retried first; an unbound ARM stays unknown.
    ///
    /// # Errors
    /// The claim is legacy, the child group is live or unknown, ARM remains unbound, a pending
    /// event belongs to another process, or journal/namespace persistence fails.
    #[cfg(unix)]
    pub fn finish_cohort(&mut self) -> io::Result<()> {
        self.laid.finish_cohort()?;
        if self.record.accounting != crate::Accounting::CohortOwned {
            self.record.accounting = crate::Accounting::Holder;
        }
        Ok(())
    }

    #[cfg(unix)]
    pub(crate) fn prepare_gnu_epoch(
        &mut self,
        epoch: &crate::retained_gnu::Epoch,
    ) -> io::Result<()> {
        self.laid.prepare_gnu_epoch(epoch)
    }

    #[cfg(unix)]
    pub(crate) fn check_gnu_scope_drained(&mut self) -> io::Result<()> {
        self.laid.check_gnu_scope_drained()
    }

    #[cfg(unix)]
    pub(crate) fn retire_gnu_epoch(
        &mut self,
        epoch: &crate::retained_gnu::Epoch,
    ) -> io::Result<()> {
        self.laid.retire_gnu_epoch(epoch)
    }

    /// Whether this actual captured token is the durably bound observation. This supplies no
    /// new spawn or signalling authority and does not infer drain from a reused holder pid.
    #[cfg(unix)]
    #[must_use]
    pub fn cohort_bound(&self, cohort: &crate::cohort::Cohort) -> bool {
        self.laid.cohort_bound(cohort)
    }
}

/// What one look at the ledger found ([`Waiter::poll`]).
#[derive(Debug)]
pub enum Polled {
    /// Timing readiness could not be qualified; no work callback may use this result.
    Unqualified(crate::TimingIssue),
    /// Admitted: hold the claim while the work runs.
    Admitted(Claim),
    /// Still waiting, short of what the shortfall names.
    Waiting(Waiter, Shortfall),
    /// Its claim file was removed while it waited (the ledger's directory cleaned): it is in no
    /// other session's queue and can never be admitted, so the wait is over. The error names
    /// the file.
    Gone(io::Error),
}

/// A claim in the queue, not yet admitted. It leaves the ledger when dropped. Until it is
/// admitted its wait is marked where a ceiling over this process's tree reads it
/// ([`aterm_toolchain::waits`]), saying at each look what it is short of and who holds that.
/// Each look reads the process tree of every held claim it waits behind, with the processes
/// that claim is credited with ([`aterm_toolchain::stall`], what it read shared by this process's
/// waiters), and names a holder whose tree has made no progress for its bound
/// ([`Holder::stalled`]); once every claim it waits behind is held by one, its mark says so
/// ([`aterm_toolchain::waits::Stuck`]), and one whose need asks to ([`Need::ends_when_stuck`])
/// ends its wait on the second such look in a row. Nobody signals the holder.
#[derive(Debug)]
pub struct Waiter {
    ledger: Ledger,
    record: Record,
    laid: Laid,
    /// The path whose volume the reading is taken for.
    at: Option<PathBuf>,
    since: Instant,
    /// When its admission first entered the queue, unix ms ([`Claim::asked_ms`]): its record's
    /// `start_ms`, or the first entry's for a need entered again.
    asked_ms: u64,
    mark: Option<aterm_toolchain::waits::Mark>,
    /// How long a holder's tree may make no progress before it is called stuck: the need's
    /// [`Need::stuck_ends`], else [`aterm_toolchain::stall::STALLED_AFTER`].
    stalled_after: Duration,
    /// How many of its looks in a row that read the holders, the last included, found its wait
    /// stuck behind them ([`stuck`]): a look that only met the ledger's lock, or could not read
    /// the ledger, read none and changes nothing.
    stuck_looks: u32,
    /// What the last look that read them found.
    stuck: Option<aterm_toolchain::waits::Stuck>,
    /// Timing-only observation storage; ordinary resource waiters carry no counter payload.
    cpu_point: Option<Box<crate::CpuPoint>>,
}

/// WHAT THIS PROCESS'S WAITERS HAVE READ OF THE HOLDERS THEY WAIT BEHIND, by ledger and holder
/// pid ([`Waiter::read_holders`]): a process's runnable time and the bytes it wrote only grow, so
/// the reads say at what rate a tree worked between them ([`aterm_toolchain::stall::Stall`]), and
/// a waiter that comes later counts a holder's stall from the last read of it that found it
/// working, not from its own first look (a verify run's later stage behind the holder its earlier
/// stage waited behind), judging it over the window the first waiter to read it set. An entry no
/// waiter has read for [`STALL_KEPT`] goes.
type StallsByPid = std::collections::BTreeMap<u32, (aterm_toolchain::stall::Stall, Instant)>;

static STALLS: Mutex<std::collections::BTreeMap<PathBuf, StallsByPid>> =
    Mutex::new(std::collections::BTreeMap::new());

/// How long [`STALLS`] keeps what a waiter read of a holder that no waiter has read since.
const STALL_KEPT: Duration = Duration::from_secs(3600);

impl Waiter {
    /// Enter `ledger`'s queue for `need`, read on `machine` (its disk need is placed on the volume it reads).
    ///
    /// # Errors
    /// The ledger's directory or the claim file could not be made, locked or written.
    pub fn enter(ledger: &Ledger, machine: &dyn Machine, need: Need) -> io::Result<Waiter> {
        Self::enter_accounted(ledger, machine, need, crate::Accounting::Holder)
    }

    /// Queue an independent cohort budget, without numeric creator-tree credit or coverage.
    /// This immutable reservation law starts before admission and survives ARM and cancellation.
    ///
    /// # Errors
    /// The ledger is not a private cohort ledger, or entering its queue fails.
    pub fn enter_cohort(ledger: &Ledger, machine: &dyn Machine, need: Need) -> io::Result<Waiter> {
        if !ledger.cohort_aware()? {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "cohort-owned admission requires a private cohort ledger",
            ));
        }
        Self::enter_accounted(ledger, machine, need, crate::Accounting::CohortOwned)
    }

    fn enter_accounted(
        ledger: &Ledger,
        machine: &dyn Machine,
        mut need: Need,
        accounting: crate::Accounting,
    ) -> io::Result<Waiter> {
        if machine.release_run() {
            need.rank = crate::Rank::Release;
        }
        if need.rank == crate::Rank::Background {
            #[cfg(unix)]
            // SAFETY: getpgrp has no pointer arguments. Background work may authorize
            // cancellation only of its own dedicated group, never the agent's group.
            if ledger.cohort_aware()? || unsafe { libc::getpgrp() } != std::process::id() as i32 {
                return Err(io::Error::other(
                    "background admission requires a dedicated process group and holder ledger",
                ));
            }
            #[cfg(not(unix))]
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "background process groups require Unix",
            ));
        }
        let at = need.path.clone().or_else(|| std::env::current_dir().ok());
        need.path = (need.disk > 0)
            .then(|| machine.read(at.as_deref()).volume.map(|v| v.mount))
            .flatten();
        let mark = aterm_toolchain::waits::Mark::begin(&format!("capacity: {} waits", need.what));
        let stalled_after = need
            .stuck_ends
            .unwrap_or(aterm_toolchain::stall::STALLED_AFTER);
        let (record, laid) = if accounting == crate::Accounting::Holder {
            ledger.lay(&need, now_ms())?
        } else {
            ledger.lay_accounted(&need, now_ms(), accounting)?
        };
        Ok(Waiter {
            ledger: ledger.clone(),
            asked_ms: record.start_ms,
            record,
            laid,
            at,
            since: Instant::now(),
            mark,
            stalled_after,
            stuck_looks: 0,
            stuck: None,
            cpu_point: None,
        })
    }

    /// The holders its wait is stuck behind, and for how long, once two looks in a row that read
    /// them found every claim it waits behind held by one that has made no progress for its
    /// bound; `None` before that, or once one works again.
    #[must_use]
    pub fn stuck(&self) -> Option<&aterm_toolchain::waits::Stuck> {
        self.stuck.as_ref().filter(|_| self.stuck_looks >= 2)
    }

    /// Its record, as other sessions read it.
    #[must_use]
    pub const fn record(&self) -> &Record {
        &self.record
    }

    /// Look once: admitted if the need fits beside every claim ahead, else what it is short of.
    /// Reap ordinary dead holders and conclusively drained cohorts; retain uncertain custody.
    /// A waiter whose own claim file is gone is [`Polled::Gone`], whoever polls it: its grant
    /// could never be written, so a wait that went on would go on for ever.
    #[must_use]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "CapacityCohort",
            action = "AdmitSecond",
            project = "aterm_capacity::ledger::failure_tests::project"
        )
    )]
    pub fn poll(self, machine: &dyn Machine) -> Polled {
        self.poll_until(machine, &|| false)
    }

    /// As [`Self::poll`], a quiet claim's read of the GPU ended early once `stopped` says so
    /// ([`Machine::gpu_busy`]), so an interrupt ends a quiet wait within a nap, as any other.
    #[must_use]
    pub fn poll_until(self, machine: &dyn Machine, stopped: &dyn Fn() -> bool) -> Polled {
        self.poll_mode(machine, false, stopped)
    }

    fn poll_mode(
        mut self,
        machine: &dyn Machine,
        forced_now: bool,
        stopped: &dyn Fn() -> bool,
    ) -> Polled {
        // `try_exists` follows a link: one to nothing is no claim file either.
        if matches!(self.laid.path().try_exists(), Ok(false)) {
            return Polled::Gone(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "its claim file {} was removed while it waited",
                    self.laid.path().display()
                ),
            ));
        }
        // A quiet claim's GPU is read over a window, before the turn: no other waiter's look
        // waits on it. A forced look waits for nothing, and a measured claim's first look forms
        // no CPU window, so it admits nothing: one reading says either.
        let warming =
            self.record.need.quiet_policy == crate::QuietPolicy::Timing && self.cpu_point.is_none();
        let gpu = self
            .record
            .need
            .quiet
            .filter(|_| !forced_now && !warming)
            .and_then(|_| machine.gpu_busy(stopped));
        let turn = match self.ledger.try_turn() {
            Ok(Ok(turn)) => turn,
            Ok(Err(holder)) => {
                return Polled::Waiting(self, Shortfall::only(Short::Turn { holder }));
            }
            Err(e) => return Polled::Waiting(self, Shortfall::only(Short::Ledger(e.to_string()))),
        };
        let found = match self.look(machine, gpu) {
            Ok(found) => found,
            Err(e) => return Polled::Waiting(self, Shortfall::only(Short::Ledger(e.to_string()))),
        };
        if let Err(short) = &found.decided
            && let Some(issue) = short.shorts.iter().find_map(|s| match s {
                Short::Timing(issue) if !issue.retryable() => Some(*issue),
                _ => None,
            })
        {
            return Polled::Unqualified(issue);
        }
        let mut waiter = self;
        // First next for the quiet machine: its patience counts from this look, and every other
        // waiter reads when from its record. Unwritten, the next look tries again.
        if found.next && waiter.laid.append(&Record::next_line(found.now)).is_ok() {
            waiter.record.next_ms = Some(found.now);
        }
        let timed = found.timed();
        let polled = match found.decided {
            Ok((granted, quiet)) => waiter.grant(granted, quiet, timed, forced_now),
            Err(mut short) => {
                let stuck = waiter.read_holders(&mut short, found.procs.as_ref(), &found.waits);
                waiter.stuck_looks = if stuck.is_some() {
                    waiter.stuck_looks.saturating_add(1)
                } else {
                    0
                };
                let line = waiter.line(&short);
                if let Some(mark) = &mut waiter.mark {
                    mark.say_stuck(&line, stuck.as_ref());
                }
                waiter.stuck = stuck;
                Polled::Waiting(waiter, short)
            }
        };
        drop(turn);
        polled
    }

    /// Read in `procs` the process tree of each held claim's holder `short` names, with the
    /// processes that claim is credited with ([`Holder::credit`]) and whether one of them waits
    /// in its turn (its mark among `waits`), book it in [`STALLS`], and name on it how long that
    /// has made no progress once it reaches [`Waiter::stalled_after`]; then whether the wait is
    /// [`stuck`] behind them.
    fn read_holders(
        &self,
        short: &mut Shortfall,
        procs: Option<&Procs>,
        waits: &[aterm_toolchain::waits::Waiting],
    ) -> Option<aterm_toolchain::waits::Stuck> {
        let now = Instant::now();
        let dir = self.ledger.dir();
        let mut stalls = STALLS.lock().unwrap_or_else(PoisonError::into_inner);
        if short.holders.iter().any(|h| h.held) {
            let dir_stalls = match stalls.get_mut(dir) {
                Some(entry) => entry,
                None => stalls.entry(dir.to_path_buf()).or_default(),
            };
            for h in short.holders.iter_mut().filter(|h| h.held) {
                let work = procs.and_then(|p| p.work(h.pid, &h.credit, waits));
                // Judged busy over a minute, or over the bound where that is shorter (a test's
                // compressed one): a tree is never judged over more than it may stall.
                let over = aterm_toolchain::stall::BUSY_OVER.min(self.stalled_after);
                let (stall, read) = dir_stalls
                    .entry(h.pid)
                    .or_insert_with(|| (aterm_toolchain::stall::Stall::over(over), now));
                *read = now;
                h.stalled = stall.stuck(h.pid, work, now, self.stalled_after);
            }
        }
        stalls.retain(|_, by_pid| {
            by_pid.retain(|_, (_, read)| now.saturating_duration_since(*read) < STALL_KEPT);
            !by_pid.is_empty()
        });
        drop(stalls);
        stuck(&short.holders)
    }

    /// Read the machine and the ledger, and decide; a quiet claim holds the GPU to
    /// [`crate::QUIET_GPU_BUSY`] by `gpu` ([`Machine::gpu_busy`]), else by this reading's.
    fn look(&mut self, machine: &dyn Machine, gpu: Option<u8>) -> io::Result<Found> {
        let now = now_ms();
        trace("look.read.begin");
        let reading = machine.read(self.at.as_deref());
        let gpu = gpu.or(reading.gpu_busy);
        let measured = self.record.need.quiet_policy == crate::QuietPolicy::Timing;
        let cpu = if measured {
            match machine.cpu_point() {
                None => {
                    self.cpu_point = None;
                    Err(crate::TimingIssue::Unavailable)
                }
                Some(point) => {
                    let window = self
                        .cpu_point
                        .as_deref()
                        .and_then(|prior| crate::CpuWindow::between(prior, &point));
                    let had_prior = self.cpu_point.is_some();
                    if let Some(prior) = self.cpu_point.as_mut() {
                        **prior = point;
                    } else {
                        self.cpu_point = Some(Box::new(point));
                    }
                    window.ok_or(if had_prior {
                        crate::TimingIssue::CounterChanged
                    } else {
                        crate::TimingIssue::Warming
                    })
                }
            }
        } else {
            Err(crate::TimingIssue::Unavailable)
        };
        trace("look.read.end");
        trace("look.scan.begin");
        let records = self.ledger.scan(true)?;
        trace("look.scan.end");
        let mine = self.record.order();
        let records: Vec<&Record> = records.iter().filter(|r| r.order() != mine).collect();
        let procs = (!records.is_empty())
            .then(|| {
                trace("look.procs.begin");
                let procs = machine.procs();
                trace("look.procs.end");
                procs
            })
            .flatten();
        let waits = if procs.is_some() {
            machine.waits()
        } else {
            Vec::new()
        };
        // Coverage reads the claims before `counted` leaves out the ancestors': an ancestor's held
        // quiet claim is what covers this one.
        let mut need = std::borrow::Cow::Borrowed(&self.record.need);
        // Ancestor coverage is resource exclusion only. Even a measured parent supplies no
        // CPU window here: this waiter samples its own admission and later its own entry.
        if self.record.accounting == crate::Accounting::Holder
            && need.quiet.is_some()
            && covered(self.record.pid, &records, procs.as_ref())
        {
            need.to_mut().quiet = None;
        }
        let bound = if measured {
            self.record.need.quiet
        } else {
            need.quiet
        };
        // Past its patience the load and the GPU hold a quiet claim back no longer: only another
        // quiet claim does, so two timed runs never overlap.
        let past = bound.is_some()
            && self
                .record
                .need
                .patience
                .zip(self.record.next_ms)
                .is_some_and(|(p, next)| now >= next.saturating_add(millis(p.start)));
        // A loaded claim its own process or an ancestor holds has already spent the patience of
        // the work under it: this need's first window decides, quiet or loaded.
        let spent = self.record.accounting == crate::Accounting::Holder
            && measured
            && self.record.need.at_bound == crate::AtBound::RunLoaded
            && loaded_cover(self.record.pid, &records, procs.as_ref());
        let past = past || spent;
        if past && need.quiet.is_some() {
            need.to_mut().quiet = Some(f64::INFINITY);
        }
        let records = if self.record.accounting == crate::Accounting::CohortOwned {
            records
        } else {
            counted(
                records,
                self.record.pid,
                procs.as_ref(),
                reading.available.or(reading.physical),
            )
        };
        let mut claimants = HashSet::with_capacity(records.len() + 1);
        claimants.extend(records.iter().map(|r| r.pid));
        claimants.insert(self.record.pid);
        let trees = procs
            .as_ref()
            .filter(|_| !records.is_empty())
            .map(|p| {
                let mut roots: Vec<u32> = Vec::with_capacity(
                    claimants.len() + records.iter().map(|r| r.credit.len()).sum::<usize>(),
                );
                roots.extend(claimants.iter().copied());
                roots.extend(records.iter().flat_map(|r| r.credit.iter().copied()));
                p.trees(&roots)
            })
            .unwrap_or_default();
        let others = others(&records, claimants, &trees);
        let mut decided = decide(&need, mine, &reading, &others, now);
        let mut loaded = None;
        if measured {
            use crate::TimingIssue;
            let load = reading.load1.filter(|n| n.is_finite() && *n >= 0.0);
            // Before its bound a busy or broken window is waited out; at it a busy one is the
            // machine's load, stated or refused, and a missing one ends the wait (blind).
            let issue = match (load, cpu) {
                (None, _) => Some(TimingIssue::LoadUnknown),
                (Some(_), Err(TimingIssue::CounterChanged)) if past => Some(TimingIssue::Blind),
                (Some(_), Err(e)) => Some(e),
                (Some(load1), Ok(window)) if past => {
                    let max = self.record.need.quiet.unwrap_or(f64::INFINITY);
                    if window.ready() && load1 <= max {
                        None
                    } else if self.record.need.at_bound == crate::AtBound::RunLoaded {
                        loaded = Some(crate::Observed { window, load1, max });
                        None
                    } else {
                        Some(TimingIssue::Loaded)
                    }
                }
                (Some(_), Ok(window)) => (!window.ready()).then_some(TimingIssue::Busy),
            };
            if let Some(issue) = issue {
                match &mut decided {
                    Ok(_) => decided = Err(Shortfall::only(Short::Timing(issue))),
                    Err(short) => short.shorts.push(Short::Timing(issue)),
                }
            }
            if !past
                && let (Some(load1), Some(max)) = (reading.load1, self.record.need.quiet)
                && load1 > max
                && !decided
                    .as_ref()
                    .is_err_and(|s| s.shorts.iter().any(|s| matches!(s, Short::Load { .. })))
            {
                match &mut decided {
                    Ok(_) => decided = Err(Shortfall::only(Short::Load { load1, max })),
                    Err(short) => short.shorts.push(Short::Load { load1, max }),
                }
            }
        }
        // QUIET-BURST: a quiet claim short of its patience waits for a quiet GPU too, a measured
        // one whatever covers it (`bound` is its own), as it waits for the load. One with no
        // patience waits for the load alone.
        if !past
            && bound.is_some()
            && self.record.need.patience.is_some()
            && let Some(busy) = gpu.filter(|b| *b > crate::QUIET_GPU_BUSY)
        {
            let short = Short::Gpu {
                busy,
                max: crate::QUIET_GPU_BUSY,
            };
            match &mut decided {
                Ok(_) => decided = Err(Shortfall::only(short)),
                Err(s) => s.shorts.push(short),
            }
        }
        let me = self.record.pid;
        let kin = |pid: u32| {
            pid == me
                || procs
                    .as_ref()
                    .is_some_and(|p| p.ancestors(pid).contains(&me))
        };
        if let Some((r, left)) = reserving(&self.record, &records, &kin, now) {
            let short = Short::Reserved {
                pid: r.pid,
                what: r.need.what.clone(),
                held: r.held.is_some(),
                left,
            };
            let holder = Holder {
                pid: r.pid,
                what: r.need.what.clone(),
                checkout: r.checkout.clone(),
                held: r.held.is_some(),
                age: Duration::from_millis(now.saturating_sub(r.start_ms)),
                mem: 0,
                claimed: 0,
                alone: false,
                disk: 0,
                quiet: true,
                order: r.order(),
                stalled: None,
                credit: r.credit.clone(),
                overrun: None,
            };
            decided = match decided {
                Ok(_) => Err(Shortfall {
                    shorts: vec![short],
                    holders: vec![holder],
                }),
                Err(mut s) => {
                    s.shorts.push(short);
                    if !s.holders.iter().any(|h| h.pid == holder.pid && h.quiet) {
                        s.holders.push(holder);
                    }
                    Err(s)
                }
            };
        }
        let next = bound.is_some()
            && self.record.need.patience.is_some()
            && self.record.next_ms.is_none()
            && decided.as_ref().is_err_and(|s| {
                s.shorts.iter().any(Short::machine_busy)
                    && !s.shorts.iter().any(|x| matches!(x, Short::Quiet { .. }))
            });
        let reserved = self
            .record
            .need
            .patience
            .zip(self.record.next_ms)
            .map_or(0, |(p, next)| {
                now.saturating_sub(next.saturating_add(millis(p.reserve)))
            });
        let decided = decided.map(|granted| {
            let quiet = bound.map(|max| QuietStart {
                load1: reading.load1,
                max,
                gpu,
                at_bound: past
                    && (reading.load1.is_some_and(|l| l > max)
                        || gpu.is_some_and(|b| b > crate::QUIET_GPU_BUSY)),
                reserved: Duration::from_millis(reserved),
            });
            (granted, quiet)
        });
        let cores = match &decided {
            Ok((granted, _)) => granted.cores,
            Err(_) => ranked_grant(&need, mine, &reading, &others, now),
        };
        Ok(Found {
            cores,
            decided,
            cpu: cpu.ok(),
            load1: reading.load1,
            loaded: loaded.is_some(),
            spent,
            next,
            now,
            procs,
            waits,
        })
    }

    /// Mark it admitted with `granted` (its cores, and whether its memory runs alone), a quiet
    /// claim having started as `quiet` says.
    fn grant(
        self,
        granted: Grant,
        quiet: Option<QuietStart>,
        timed: Timed,
        forced_now: bool,
    ) -> Polled {
        trace("grant.begin");
        let Timed {
            cpu,
            load1,
            loaded,
            spent,
        } = timed;
        let Waiter {
            ledger,
            mut record,
            mut laid,
            at,
            since,
            asked_ms,
            mark,
            stalled_after,
            stuck_looks,
            stuck,
            cpu_point,
        } = self;
        let actual_admitted_ms = aterm_time::unix_millis();
        let held = Held {
            admitted_ms: actual_admitted_ms.unwrap_or_default(),
            cores: granted.cores,
            alone: granted.alone.is_some(),
        };
        let admitted = ledger.admit(&mut laid, &mut record, held).and_then(|()| {
            if record.need.quiet_policy == crate::QuietPolicy::Timing {
                record.timing = crate::TimingEvidence {
                    // A grant that is neither forced nor loaded is a quiet one: a window that does
                    // not say so leaves its admission unqualified (the check below refuses it).
                    cause: if forced_now {
                        crate::TimingCause::Now
                    } else if loaded {
                        crate::TimingCause::Loaded
                    } else {
                        crate::TimingCause::Wait
                    },
                    admitted_ms: actual_admitted_ms,
                    admission: cpu,
                    entry: None,
                    admission_load1: load1,
                    entry_load1: None,
                };
                laid.append(&Record::timing_line(record.timing))?;
            }
            crate::background::Guard::start(&ledger, record.need.rank)
        });
        let polled = match admitted {
            Ok(background) => {
                let claim = Claim {
                    _background: background,
                    laid,
                    record,
                    waited: since.elapsed(),
                    asked_ms,
                    wrote_at: None,
                    quiet,
                    alone: granted.alone,
                    short: None,
                    entry_custody: true,
                    spent,
                };
                let checked = match claim.record.need.at_bound {
                    crate::AtBound::RunLoaded => claim.timing_protection().map(drop),
                    crate::AtBound::Refuse => claim.timing_admission().map(drop),
                };
                if claim.record.need.quiet_policy == crate::QuietPolicy::Timing
                    && let Err(issue) = checked
                {
                    Polled::Unqualified(issue)
                } else {
                    Polled::Admitted(claim)
                }
            }
            Err(e) if record.held.is_some() => Polled::Gone(e),
            Err(e) => Polled::Waiting(
                Waiter {
                    ledger,
                    record,
                    laid,
                    at,
                    since,
                    asked_ms,
                    mark,
                    stalled_after,
                    stuck_looks,
                    stuck,
                    cpu_point,
                },
                Shortfall::only(Short::Ledger(e.to_string())),
            ),
        };
        trace("grant.end");
        polled
    }

    /// One look under `--admit now`: admit it room or not, with what it would have waited for;
    /// on a cohort-aware ledger it only polls, so it meets the same checks as any other look.
    /// Either way it decides only in its turn: while another admission holds the ledger's lock
    /// it admits nothing and hands the waiter back ([`Forced::Turn`]) to look again, which
    /// [`admit_with`]'s `now` does every [`Pace::turn_gap`] and the Trust lane's launch gate at
    /// its next pass.
    ///
    /// # Errors
    /// The ledger could not be read or marked admitted; on a cohort-aware ledger, also a look
    /// short of anything but the ledger's lock.
    pub fn force(mut self, machine: &dyn Machine) -> io::Result<Forced> {
        if self.ledger.cohort_aware()? {
            return match self.poll_mode(machine, true, &|| false) {
                Polled::Admitted(claim) => Ok(Forced::Admitted(claim, None)),
                Polled::Waiting(waiter, short)
                    if matches!(short.shorts[..], [Short::Turn { .. }]) =>
                {
                    Ok(Forced::Turn(waiter, short))
                }
                Polled::Waiting(_, short) => Err(io::Error::other(short.to_string())),
                Polled::Gone(error) => Err(error),
                Polled::Unqualified(why) => Err(io::Error::other(why)),
            };
        }
        let turn = match self.ledger.try_turn()? {
            Ok(turn) => turn,
            Err(holder) => {
                return Ok(Forced::Turn(self, Shortfall::only(Short::Turn { holder })));
            }
        };
        let found = self.look(machine, None)?;
        // An override may bypass resource-only memory/disk admission, never a Release's
        // core reservation or the physical core bound.
        if self.record.need.cores > 0 && found.cores == 0 {
            return Ok(Forced::Turn(self, Shortfall::only(Short::Cores)));
        }
        if self.record.need.rank == crate::Rank::Background
            && found
                .decided
                .as_ref()
                .is_err_and(|s| s.shorts.contains(&Short::Release))
        {
            return Ok(Forced::Turn(self, Shortfall::only(Short::Release)));
        }
        let asked = Grant {
            cores: found.cores,
            alone: None,
        };
        let timed = Timed {
            loaded: false,
            spent: false,
            ..found.timed()
        };
        let (granted, would) = match found.decided {
            Ok((granted, _)) => (granted, None),
            Err(short) => (asked, Some(short)),
        };
        let polled = self.grant(granted, None, timed, true);
        drop(turn);
        match polled {
            Polled::Admitted(claim) => Ok(Forced::Admitted(claim, would)),
            Polled::Waiting(_, short) => Err(io::Error::other(short.to_string())),
            Polled::Gone(e) => Err(e),
            Polled::Unqualified(why) => Err(io::Error::other(why)),
        }
    }

    /// Its wait line: for a quiet claim next for the quiet machine, when its patience reserves the
    /// machine and when its wait reaches the bound.
    /// A measured need that runs loaded starts there on a busy machine and says so, a strict one
    /// is refused; load-only admission starts whatever the load, and a wait the GPU alone holds
    /// back starts whatever the GPU.
    pub(crate) fn line(&self, short: &Shortfall) -> String {
        use std::fmt::Write as _;
        let mut line = format!(
            "capacity: {} waits ({}): {short}",
            self.record.need.what,
            span(self.since.elapsed())
        );
        let waits_machine = short.shorts.iter().any(Short::machine_busy);
        if let (true, Some(p), Some(next)) = (
            waits_machine,
            self.record.need.patience,
            self.record.next_ms,
        ) {
            let gpu = short.shorts.iter().any(|s| matches!(s, Short::Gpu { .. }));
            let cpu = short
                .shorts
                .iter()
                .any(|s| matches!(s, Short::Load { .. } | Short::Timing(_)));
            let now = now_ms();
            // Whole seconds to go, rounded up: a person reads `2m00s` for the minute and 59.9 s left.
            let at = |d: Duration| {
                let ms = next.saturating_add(millis(d)).saturating_sub(now);
                Duration::from_secs(ms.div_ceil(1000))
            };
            let start = span(at(p.start));
            match at(p.reserve) {
                Duration::ZERO => line.push_str("; it reserves the machine, and "),
                d => {
                    let _ = write!(line, "; it reserves the machine in {}, and ", span(d));
                }
            }
            if !cpu {
                let _ = write!(line, "starts whatever the GPU in {start}");
            } else if self.record.need.quiet_policy != crate::QuietPolicy::Timing {
                let whatever = if gpu {
                    "the load and the GPU"
                } else {
                    "the load"
                };
                let _ = write!(line, "starts whatever {whatever} in {start}");
            } else if self.record.need.at_bound == crate::AtBound::RunLoaded {
                let _ = write!(
                    line,
                    "starts loaded at its bound in {start}, saying so, unless the machine is quiet first"
                );
            } else {
                let _ = write!(
                    line,
                    "refuses its work at its bound in {start} unless the machine is quiet"
                );
            }
        }
        line
    }

    /// What a wait for the ledger's lock alone says, by the wait loop's rule: its line for
    /// `short` (which names the lock's holder) and how long the lock has kept it, once that is a
    /// whole [`Pace::line_every`] since `since` (the first look that met the lock) and no line
    /// was `said` in the last [`Pace::line_every`]; `None` before that.
    #[must_use]
    pub fn turn_line(
        &self,
        short: &Shortfall,
        pace: Pace,
        since: Instant,
        said: Option<Instant>,
    ) -> Option<String> {
        (since.elapsed() >= pace.line_every && said.is_none_or(|t| t.elapsed() >= pace.line_every))
            .then(|| format!("{}, for {}", self.line(short), span(since.elapsed())))
    }
}

/// Whether a wait behind `holders` is stuck: when it names any, and every one is a held claim
/// whose holder has made no progress for the waiter's bound ([`Holder::stalled`]), how long the
/// last of them to work has made none. A claim that waits ahead looks again every few seconds, so
/// it works, and a short that no claim holds has no holder to be stuck.
pub(crate) fn stuck(holders: &[Holder]) -> Option<aterm_toolchain::waits::Stuck> {
    let mut min_span: Option<Duration> = None;
    for h in holders {
        let span = h.stalled.filter(|_| h.held)?;
        min_span = Some(min_span.map_or(span, |cur| cur.min(span)));
    }
    let span = min_span?;
    let mut pids: Vec<u32> = holders.iter().map(|h| h.pid).collect();
    pids.sort_unstable();
    pids.dedup();
    Some(aterm_toolchain::waits::Stuck {
        holders: pids,
        span,
    })
}

/// What one look found ([`Waiter::look`]): admitted with what it is granted ([`Grant`]) and, for
/// a quiet claim, how it started; or what it is short of.
struct Found {
    cores: u32,
    decided: Result<(Grant, Option<QuietStart>), Shortfall>,
    cpu: Option<crate::CpuWindow>,
    load1: Option<f64>,
    /// A measured need that runs loaded met its bound on a busy machine ([`crate::AtBound`]).
    loaded: bool,
    /// A loaded claim of its process or an ancestor spent its patience ([`loaded_cover`]).
    spent: bool,
    /// The waiter is first next for the quiet machine at this look ([`Record::next_ms`]).
    next: bool,
    /// When it looked, unix ms.
    now: u64,
    /// The processes it read: `None` where it read none (no claim ahead, or no sweep).
    procs: Option<Procs>,
    /// The waits it read in this user's waits, with the processes ([`Machine::waits`]).
    waits: Vec<aterm_toolchain::waits::Waiting>,
}

/// What a look found of a measured need's CPU and load, for its grant.
#[derive(Clone, Copy)]
struct Timed {
    cpu: Option<crate::CpuWindow>,
    load1: Option<f64>,
    /// It met its bound on a busy machine and runs loaded.
    loaded: bool,
    /// A loaded claim above it spent its patience ([`loaded_cover`]).
    spent: bool,
}

impl Found {
    const fn timed(&self) -> Timed {
        Timed {
            cpu: self.cpu,
            load1: self.load1,
            loaded: self.loaded,
            spent: self.spent,
        }
    }
}

/// What one forced look found ([`Waiter::force`]).
#[derive(Debug)]
pub enum Forced {
    /// Admitted, room or not, with what it would have waited for: `None` where it had room.
    Admitted(Claim, Option<Shortfall>),
    /// Another admission held the ledger's lock while it decided, so it decided nothing: the
    /// waiter, still in the queue, and the short that names that lock's holder.
    Turn(Waiter, Shortfall),
}

/// Why [`admit`] admitted nothing.
#[derive(Debug)]
pub enum Error {
    /// Required measured timing readiness was not established; no acceptance callback ran.
    Timing(crate::TimingIssue),
    /// The need can never fit on this machine ([`never`]).
    Never(String),
    /// A disk need still more than its volume has free above the floor once its relief freed
    /// what it could ([`Short::unservable`] at the look that followed that relief), with no
    /// scratch claim held there whose end could meet it ([`Short::Disk`]'s `held`): what other
    /// claims wrote stays once they end, and the relief has nothing left to free toward it, so
    /// only space freed by hand would admit it and its wait would never end. The claim has left
    /// the queue. Its caller adds the remedy it takes (verify's `--admit now`), if it has one.
    Unmet {
        /// What it is ([`Need::what`]).
        what: String,
        /// What it was short of at that look.
        short: Shortfall,
        /// What holds the most of the short volume, as its relief names it
        /// ([`Relief::holders`]).
        holders: Option<String>,
        /// Whether a relief ran ([`admit_relieved`]); [`admit_with`] has none.
        relieved: bool,
    },
    /// The ledger could not be used.
    Ledger(io::Error),
    /// An interrupt, the caller ([`admit_until`]), or a conservative Background stop before a
    /// [`crate::Admit::Preemptible`] grant ended the wait. The latter means a Release arrived or
    /// ledger failure made its absence unknowable. The claim has left the queue.
    Stopped {
        /// The interrupt ([`aterm_toolchain::interrupt`]); `None` when the caller or the
        /// conservative Background stop ended it.
        signal: Option<i32>,
        /// What it was short of at its last look; `None` when it never looked.
        short: Option<Shortfall>,
    },
    /// Every claim the wait was behind was held by a holder that had made no progress for the
    /// need's bound, on two looks in a row ([`Need::ends_when_stuck`]). The claim has left the
    /// queue; nobody signalled the holders.
    Stuck {
        /// The holders, and how long the last of them to work has made none.
        stuck: aterm_toolchain::waits::Stuck,
        /// What it was short of at its last look, each holder named with how long it has made
        /// no progress ([`Holder::stalled`]).
        short: Shortfall,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Timing(why) => write!(f, "timing not qualified: {why}"),
            Error::Never(why) => write!(f, "{why}; it can never be admitted here"),
            Error::Unmet {
                what,
                short,
                holders,
                relieved,
            } => {
                let beyond = || short.shorts.iter().filter(|s| s.unservable());
                write!(f, "{what}: ")?;
                for (i, s) in beyond().enumerate() {
                    if i > 0 {
                        f.write_str("; ")?;
                    }
                    write!(f, "{s}")?;
                }
                f.write_str(if *relieved {
                    ", once its relief freed what it could"
                } else {
                    ", with no relief to free it"
                })?;
                f.write_str(
                    ": what other claims wrote stays once they end, and no held scratch claim \
                     can return enough of what it wrote, so a wait would never end",
                )?;
                if let Some(holders) = holders {
                    write!(f, "; {holders}")?;
                }
                f.write_str(". Free ")?;
                for (i, s) in beyond().enumerate() {
                    if let Short::Disk {
                        need,
                        mount,
                        above_floor,
                        ..
                    } = s
                    {
                        if i > 0 {
                            f.write_str(" and ")?;
                        }
                        write!(
                            f,
                            "at least {} on {}",
                            gib_up(need.saturating_sub(*above_floor)),
                            mount.display()
                        )?;
                    }
                }
                Ok(())
            }
            Error::Ledger(e) => write!(f, "the capacity ledger could not be used: {e}"),
            Error::Stopped { signal, short } => {
                match signal {
                    Some(sig) => write!(
                        f,
                        "interrupted ({}) while it waited",
                        aterm_toolchain::interrupt::name(*sig)
                    )?,
                    None => f.write_str("stopped while it waited")?,
                }
                match short {
                    Some(short) => write!(f, ": {short}"),
                    None => f.write_str(", before its first look"),
                }
            }
            Error::Stuck { stuck, short } => write!(
                f,
                "gave up its wait: every claim it waited behind is held by a holder that has made \
                 no progress for {} (none was signalled): {short}",
                span(stuck.span)
            ),
        }
    }
}

impl std::error::Error for Error {}

/// Admit `need` in this user's ledger on the live machine, saying on stderr what it waits for ([`admit_with`]).
///
/// A process that already holds claims and needs another enters a [`Waiter`] and polls it
/// instead, ending held work while it waits: this call counts the caller's own held claims
/// against the new one, and they are let go only after it returns. A process its ancestor's
/// held claim started is not held to that claim (`counted`): a tool that holds a claim may
/// start another that claims for itself.
///
/// # Errors
/// [`Error::Never`] for a need that can never fit; [`Error::Unmet`] for a disk need still more
/// than its volume holds above the floor once its relief ran (here none), with no scratch
/// claim held there that could meet it, which no wait can admit;
/// [`Error::Ledger`] when there is no home, the ledger cannot be written, or the claim's file is
/// removed while it waits; [`Error::Stuck`] for a need that asks to end its wait behind holders
/// that make no progress ([`Need::ends_when_stuck`]) once it is.
/// [`Error::Timing`] when requested Timing lacks qualifying CPU/load/provenance or uses an override.
pub fn admit(need: Need, mode: Admit) -> Result<Claim, Error> {
    let ledger = crate::Ledger::user().ok_or_else(|| {
        Error::Ledger(io::Error::new(
            io::ErrorKind::NotFound,
            "no home directory to keep the ledger under",
        ))
    })?;
    admit_with(
        &ledger,
        &crate::Live,
        need,
        mode,
        Pace::default(),
        &mut |line| {
            use std::io::Write as _;
            let _ = writeln!(io::stderr(), "{line}");
        },
    )
}

/// Make one ordinary, non-overriding admission attempt. A caller with several safe
/// resource envelopes can try them widest first and then wait on its smallest one.
/// `None` keeps no queue position or reservation. Unlike `Admit::Now`, a shortfall
/// never admits work. A positive margin also prevents admission ALONE.
///
/// # Errors
/// The ledger cannot be read or written, or requested timing is unavailable.
pub fn try_admit_with(
    ledger: &Ledger,
    machine: &dyn Machine,
    need: Need,
) -> Result<Option<Claim>, Error> {
    if never(&need, &machine.read(need.path.as_deref())).is_some() {
        return Ok(None);
    }
    match Waiter::enter(ledger, machine, need)
        .map_err(Error::Ledger)?
        .poll(machine)
    {
        Polled::Admitted(claim) => Ok(Some(claim)),
        Polled::Waiting(_, _) => Ok(None),
        Polled::Gone(error) => Err(Error::Ledger(error)),
        Polled::Unqualified(issue) => Err(Error::Timing(issue)),
    }
}

/// What a waiter can do about a shortfall besides wait: free what it can before it says it waits
/// ([`Self::relieve`]), and name what holds the rest beyond the claims a wait line names
/// ([`Self::holders`]). [`admit_relieved`] calls it; [`admit_with`] has none. Each is part of the
/// wait, so each gives up what is left of its work once `stopped` says an interrupt came.
pub trait Relief {
    /// Called at the first look that finds the need short of something a claim can hold, before
    /// any wait line: free what can be freed toward it, saying through `say` what was done. The
    /// next look follows at once, so a need the relief met is admitted with no wait, and a disk
    /// need it left [`Short::unservable`] is refused ([`Error::Unmet`]) unless scratch claims
    /// held on its volume could meet it, which it waits for. Called again
    /// at a look at which a disk need no claim's end could meet ([`Short::unservable`]) fits the
    /// room once more but is still short, where a claim reserves that disk that did not at the
    /// look the last call was sized for: a claim admitted around it while it could not fit holds
    /// the rest, which no earlier call was sized for. Where every claim that reserves it was
    /// counted by the last call, that call was sized for them, and it waits. Called afresh, as at
    /// a first look, once its need sized again ([`Self::resized`]) has re-entered the wait, at
    /// a look that finds a disk need [`Short::unservable`] once more after one that found it within
    /// the room, and at a look that finds it still [`Short::unservable`] once a scratch claim it
    /// waited for since the last call has ended.
    fn relieve(&mut self, short: &Shortfall, say: &mut dyn FnMut(&str), stopped: &dyn Fn() -> bool);

    /// Words a wait line ends with: what holds the rest (the largest users of a short volume).
    fn holders(&mut self, short: &Shortfall, stopped: &dyn Fn() -> bool) -> Option<String>;

    /// Whether [`Self::relieve`] can free anything, which [`Error::Unmet`] says: `true`, as by
    /// default; [`admit_with`]'s relief is none.
    fn relieves(&self) -> bool {
        true
    }

    /// Asked at each look that waits short of something a relief can free once a relief has run:
    /// whether what it may take grew since its last call (a workspace removed meanwhile, whose
    /// build directory nobody builds into again), so it runs again at once. It is asked as often
    /// as the wait looks, so it is cheap and frees nothing itself. `false`, as by default,
    /// waits.
    fn renewed(&mut self, stopped: &dyn Fn() -> bool) -> bool {
        let _ = stopped;
        false
    }

    /// Asked after each [`Self::relieve`], and at the look that admits a claim that waited
    /// (before it is returned): the waiter's disk need sized again, saying through `say` why,
    /// since a relief, or any pass that ran while it waited, can remove what it was sized on (a
    /// run's own build directory, which the run then writes again from empty); `None` where it
    /// stands, as by default. A need above the one it waits with re-enters the wait with it
    /// (an admitted claim let go first), behind the claims that asked meanwhile (they were
    /// reckoned against the smaller need), its wait counted from when it first entered; one that
    /// can never fit there ([`never`]) is refused instead. One at or below it changes nothing. A
    /// claim admitted at its first look, having waited for nothing, is not asked.
    fn resized(&mut self, say: &mut dyn FnMut(&str), stopped: &dyn Fn() -> bool) -> Option<u64> {
        let _ = (say, stopped);
        None
    }
}

/// [`admit_with`]'s relief: none.
struct NoRelief;

impl Relief for NoRelief {
    fn relieve(
        &mut self,
        _short: &Shortfall,
        _say: &mut dyn FnMut(&str),
        _stopped: &dyn Fn() -> bool,
    ) {
    }

    fn holders(&mut self, _short: &Shortfall, _stopped: &dyn Fn() -> bool) -> Option<String> {
        None
    }

    fn relieves(&self) -> bool {
        false
    }
}

/// Admit `need` in `ledger` on `machine`: resource-only admission refuses a need that can never
/// fit under `wait`, starts under `now` as soon as no other admission holds the ledger's lock,
/// or waits for room. Timing instead requires qualifying CPU/load and refuses overrides. Say what
/// it is short of and who holds it at the first look and every [`Pace::line_every`] after. A wait
/// only for the ledger's lock, under either, is said once one holder has kept it a whole
/// [`Pace::line_every`], naming that holder.
///
/// # Errors
/// As [`admit`].
pub fn admit_with(
    ledger: &Ledger,
    machine: &dyn Machine,
    need: Need,
    mode: Admit,
    pace: Pace,
    say: &mut dyn FnMut(&str),
) -> Result<Claim, Error> {
    admit_waiting(
        ledger,
        machine,
        need,
        mode,
        pace,
        &mut NoRelief,
        say,
        &|| false,
    )
}

/// [`admit_with`], with `relief` asked to free what it can at the first look that waits, before
/// any wait line, and to name the rest's holders on each line after.
///
/// # Errors
/// As [`admit`].
pub fn admit_relieved(
    ledger: &Ledger,
    machine: &dyn Machine,
    need: Need,
    mode: Admit,
    pace: Pace,
    relief: &mut dyn Relief,
    say: &mut dyn FnMut(&str),
) -> Result<Claim, Error> {
    admit_waiting(ledger, machine, need, mode, pace, relief, say, &|| false)
}

/// [`admit_with`], its wait ended by `stopped` too (an interrupt its caller caught before the
/// wait began), which it asks before each look and through each nap (under `now`, through each
/// nap after a look that met the ledger's lock): the claim then leaves the queue, and
/// [`Error::Stopped`] names what it was short of.
///
/// # Errors
/// As [`admit`], and [`Error::Stopped`].
pub fn admit_until(
    ledger: &Ledger,
    machine: &dyn Machine,
    need: Need,
    mode: Admit,
    pace: Pace,
    say: &mut dyn FnMut(&str),
    stopped: &dyn Fn() -> bool,
) -> Result<Claim, Error> {
    admit_waiting(
        ledger,
        machine,
        need,
        mode,
        pace,
        &mut NoRelief,
        say,
        stopped,
    )
}

/// The one admission [`admit_with`], [`admit_relieved`] and [`admit_until`] run: `relief` asked
/// at the first look that waits, and an interrupt or `stopped` looked for through every nap
/// ([`aterm_toolchain::interrupt::Watch`]): the wait ends within [`aterm_toolchain::interrupt::NAP`]
/// of one, and one that came during the look that admitted it ends it all the same. A need that
/// asks to ([`Need::ends_when_stuck`]) ends at the look its waiter believes stuck
/// ([`Waiter::stuck`]), whatever that look is short of. Under `now` it waits only for the
/// ledger's lock, holding a watch from the first look that met it, so an interrupt or `stopped`
/// ends that wait the same way.
///
/// Each decision, admitted or not, is one `kind=admission` row in this Mac's telemetry log where
/// `ledger` is this user's own ([`crate::telemetry`]).
#[expect(
    clippy::too_many_arguments,
    reason = "admission's six, the relief that frees room and the interrupt that ends its wait"
)]
fn admit_waiting(
    ledger: &Ledger,
    machine: &dyn Machine,
    need: Need,
    mode: Admit,
    pace: Pace,
    relief: &mut dyn Relief,
    say: &mut dyn FnMut(&str),
    stopped: &dyn Fn() -> bool,
) -> Result<Claim, Error> {
    let asked = crate::telemetry::Asked::of(&need);
    let decided = admit_deciding(ledger, machine, need, mode, pace, relief, say, stopped);
    crate::telemetry::decided(ledger, &asked, decided.as_ref());
    decided
}

/// [`admit_waiting`]'s decision, before its row.
#[expect(
    clippy::too_many_arguments,
    reason = "admission's six, the relief that frees room and the interrupt that ends its wait"
)]
fn admit_deciding(
    ledger: &Ledger,
    machine: &dyn Machine,
    mut need: Need,
    mode: Admit,
    pace: Pace,
    relief: &mut dyn Relief,
    say: &mut dyn FnMut(&str),
    stopped: &dyn Fn() -> bool,
) -> Result<Claim, Error> {
    let background = mode == Admit::Preemptible && !machine.release_run();
    if background {
        need.rank = crate::Rank::Background;
    }
    if need.quiet_policy == crate::QuietPolicy::Timing && mode == Admit::Now {
        return Err(Error::Timing(crate::TimingIssue::Now));
    }
    let watch = (mode != Admit::Now).then(aterm_toolchain::interrupt::Watch::enter);
    // A Background holder starts its group guard only after admission. Observe Release here too,
    // so one already queued cannot be waited out in the uncovered interval before that guard
    // exists. A failed ledger read supplies no authority to start repeatable work beside a
    // release. Ordinary work is deliberately absent: rank and resource reservations order it
    // ahead of the Background admission where their needs overlap.
    let release_arrived = || {
        background
            && ledger.list().map_or(true, |rows| {
                rows.iter().any(|row| row.need.rank == crate::Rank::Release)
            })
    };
    let caller_stopped = stopped;
    let stopped = || caller_stopped() || release_arrived();
    let ended = || match watch
        .as_ref()
        .and_then(aterm_toolchain::interrupt::Watch::take)
    {
        Some(sig) => Some(Some(sig)),
        None => stopped().then_some(None),
    };
    let interrupted = || watch.as_ref().is_some_and(|w| w.caught().is_some()) || stopped();
    let nap = |d: Duration| match &watch {
        Some(w) => {
            w.nap(d, &stopped);
        }
        None => std::thread::sleep(d),
    };
    if mode != Admit::Now
        && let Some(signal) = ended()
    {
        return Err(Error::Stopped {
            signal,
            short: None,
        });
    }
    let at = need.path.clone().or_else(|| std::env::current_dir().ok());
    let reading = machine.read(at.as_deref());
    let refused = never(&need, &reading);
    if mode != Admit::Now
        && let Some(why) = refused
    {
        return Err(Error::Never(why));
    }
    let what = need.what.clone();
    // The need it waits with, entered again once a relief's resizing grew its disk.
    let mut entered = need.clone();
    let mut waiter = Waiter::enter(ledger, machine, need).map_err(Error::Ledger)?;
    // When it first entered: a need entered again keeps it, so its wait counts from there, by
    // either clock.
    let first = waiter.since;
    let first_ms = waiter.asked_ms;
    if mode == Admit::Now {
        // It overrides room, never the ledger's lock: one that meets another admission deciding
        // (a verify run admits several stages at once) waits its turn as the wait loop does,
        // saying so by the same rule. That wait holds a watch from the first look that met the
        // lock, so an interrupt or `stopped` ends it as either ends a wait for room, and one that
        // came during the look that then admitted it ends it all the same. A look that had its
        // turn at once holds none: an interrupt during it reaches the process's own handling.
        let mut turn: Option<(aterm_toolchain::interrupt::Watch, Instant)> = None;
        let mut said: Option<Instant> = None;
        let mut last: Option<Shortfall> = None;
        let (claim, would) = loop {
            match waiter.force(machine).map_err(Error::Ledger)? {
                Forced::Admitted(claim, would) => {
                    if let Some((watch, _)) = &turn {
                        let signal = watch.take();
                        if signal.is_some() || stopped() {
                            drop(claim);
                            return Err(Error::Stopped {
                                signal,
                                short: last,
                            });
                        }
                    }
                    break (claim, would);
                }
                Forced::Turn(w, short) => {
                    let (watch, since) = turn.get_or_insert_with(|| {
                        (aterm_toolchain::interrupt::Watch::enter(), Instant::now())
                    });
                    if let Some(line) = w.turn_line(&short, pace, *since, said) {
                        say(&line);
                        said = Some(Instant::now());
                    }
                    watch.nap(pace.turn_gap(), &stopped);
                    let signal = watch.take();
                    if signal.is_some() || stopped() {
                        drop(w);
                        return Err(Error::Stopped {
                            signal,
                            short: Some(short),
                        });
                    }
                    waiter = w;
                    last = Some(short);
                }
            }
        };
        match (refused, would) {
            (Some(why), _) => {
                say(&format!(
                    "capacity: {what} admitted by --admit now, though it can never fit: {why}"
                ));
            }
            (None, Some(short)) => {
                say(&format!(
                    "capacity: {what} admitted by --admit now; it would wait: {short}"
                ));
            }
            (None, None) => {}
        }
        return Ok(claim);
    }
    let mut said: Option<Instant> = None;
    let mut turn_since: Option<Instant> = None;
    let mut last: Option<Shortfall> = None;
    // The claims that reserved disk at the look the last relief was sized for (none where that
    // look was short of no disk); `None` before the first relief, and again once a need sized
    // again has re-entered the wait.
    let mut sized_for: Option<Vec<(u64, u32, u64)>> = None;
    // Whether any look found it short: a claim admitted at its first look waited for nothing,
    // and nothing it was sized on could go meanwhile.
    let mut waited = false;
    // Whether the last look found a disk need no claim's end can meet ([`Short::unservable`]).
    // Claims that fit are admitted around it meanwhile and write into the room, so space freed
    // toward its need alone can still leave it short of what they hold: the look at which it
    // fits the room again but is short asks the relief once more, sized for them, where a claim
    // reserves that disk that the last relief was not sized for. Where it counted every one,
    // asking again would only repeat it, whatever they reserve now (a figure alone can fall as
    // one claim ends while another is admitted).
    let mut unservable = false;
    // Whether the last relief ran at a look that found the disk need unservable, with no look
    // since that found it within the room: that relief freed what it could toward it, so a look
    // still unservable with no scratch claim held that could meet it ends the wait ([`Error::Unmet`])
    // rather than waiting for space that no claim's end and no relief frees. A look unservable
    // after one within the room is relieved first, as at a first look.
    let mut spent = false;
    // The scratch claims it waited for at any look since the last relief ([`Short::Disk`]'s
    // `held`): one may have returned what it wrote by the time it ends, and the relief may free
    // more once it has, so a look that finds one of them gone rechecks the room and, if the need
    // is still unservable, relieves again as at a first look before it can refuse.
    let mut held_at: Vec<(u64, u32, u64)> = Vec::new();
    let relieve_now =
        |sized_for: Option<&[(u64, u32, u64)]>, unservable: bool, short: &Shortfall| {
            sized_for.is_none_or(|before| {
                unservable
                    && short
                        .shorts
                        .iter()
                        .any(|s| matches!(s, Short::Disk { .. }) && !s.unservable())
                    && disk_reservers(short)
                        .iter()
                        .any(|claim| !before.contains(claim))
            })
        };
    // The waiter `need` enters again, its wait counted from when it first entered.
    let enter_again = |need: &Need| -> Result<Waiter, Error> {
        let mut again = Waiter::enter(ledger, machine, need.clone()).map_err(Error::Ledger)?;
        again.since = first;
        again.asked_ms = first_ms;
        Ok(again)
    };
    loop {
        if let Some(signal) = ended() {
            drop(waiter);
            return Err(Error::Stopped {
                signal,
                short: last,
            });
        }
        // A claim file removed while it waits (the ledger's directory cleaned) is in no other
        // session's queue and can never be named a claim: the wait ends, saying so.
        let polled = waiter.poll_until(machine, &interrupted);
        if let Polled::Waiting(w, short) = &polled
            && entered.stuck_ends.is_some()
            && let Some(stuck) = w.stuck()
        {
            let stuck = Error::Stuck {
                stuck: stuck.clone(),
                short: short.clone(),
            };
            drop(polled);
            return Err(stuck);
        }
        match polled {
            Polled::Gone(e) => return Err(Error::Ledger(e)),
            Polled::Unqualified(why) => return Err(Error::Timing(why)),
            Polled::Admitted(mut claim) => {
                claim.short = last
                    .clone()
                    .filter(|s| !matches!(s.shorts[..], [Short::Turn { .. }]));
                if let Some(signal) = ended() {
                    drop(claim);
                    return Err(Error::Stopped {
                        signal,
                        short: last,
                    });
                }
                if waited
                    && let Some(disk) = relief
                        .resized(say, &interrupted)
                        .filter(|disk| *disk > entered.disk)
                {
                    drop(claim);
                    entered.disk = disk;
                    if let Some(why) = never(&entered, &machine.read(at.as_deref())) {
                        return Err(Error::Never(why));
                    }
                    waiter = enter_again(&entered)?;
                    sized_for = None;
                    unservable = false;
                    spent = false;
                    held_at.clear();
                    continue;
                }
                if said.is_some() {
                    let how = claim
                        .quiet()
                        .filter(|q| q.at_bound || !q.reserved.is_zero())
                        .map_or_else(String::new, |q| format!(": {q}"));
                    let alone = claim.alone().map_or_else(String::new, |a| format!("; {a}"));
                    say(&format!(
                        "capacity: {what} admitted after {}{how}{alone}",
                        span(claim.waited())
                    ));
                }
                return Ok(claim);
            }
            Polled::Waiting(w, short) if matches!(short.shorts[..], [Short::Turn { .. }]) => {
                waited = true;
                let since = *turn_since.get_or_insert_with(Instant::now);
                if let Some(line) = w.turn_line(&short, pace, since, said) {
                    say(&line);
                    said = Some(Instant::now());
                }
                waiter = w;
                if last.is_none() {
                    last = Some(short);
                }
                nap(pace.turn_gap());
            }
            Polled::Waiting(w, short)
                if short.shorts.iter().any(Short::relievable)
                    && (relieve_now(sized_for.as_deref(), unservable, &short)
                        || (short.shorts.iter().any(Short::unservable)
                            && (!spent
                                || held_at.iter().any(|c| !held_disk(&short).contains(c))))
                        || relief.renewed(&interrupted)) =>
            {
                waited = true;
                sized_for = Some(disk_reservers(&short));
                unservable = short.shorts.iter().any(Short::unservable);
                spent = unservable;
                held_at = held_disk(&short);
                relief.relieve(&short, say, &interrupted);
                waiter = match relief.resized(say, &interrupted) {
                    Some(disk) if disk > entered.disk => {
                        entered.disk = disk;
                        if let Some(why) = never(&entered, &machine.read(at.as_deref())) {
                            return Err(Error::Never(why));
                        }
                        drop(w);
                        sized_for = None;
                        unservable = false;
                        spent = false;
                        held_at.clear();
                        enter_again(&entered)?
                    }
                    _ => w,
                };
                last = Some(short);
            }
            Polled::Waiting(w, short) => {
                waited = true;
                turn_since = None;
                unservable = short.shorts.iter().any(Short::unservable);
                if unservable && spent && held_disk(&short).is_empty() {
                    let holders = relief.holders(&short, &interrupted);
                    if let Some(signal) = ended() {
                        drop(w);
                        return Err(Error::Stopped {
                            signal,
                            short: Some(short),
                        });
                    }
                    drop(w);
                    return Err(Error::Unmet {
                        what,
                        short,
                        holders,
                        relieved: relief.relieves(),
                    });
                }
                for claim in held_disk(&short) {
                    if !held_at.contains(&claim) {
                        held_at.push(claim);
                    }
                }
                spent &= unservable;
                if said.is_none_or(|t| t.elapsed() >= pace.line_every) {
                    let mut line = w.line(&short);
                    if let Some(more) = relief.holders(&short, &interrupted) {
                        line.push_str("; ");
                        line.push_str(&more);
                    }
                    say(&line);
                    said = Some(Instant::now());
                }
                waiter = w;
                last = Some(short);
                nap(pace.poll);
            }
        }
    }
}

#[cfg(test)]
mod waiting_completion_tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// A caller stop observed by an interruptible holder scan wins over the refusal that scan
    /// was preparing. Negative control: without the check after `holders`, this is `Unmet`.
    #[test]
    fn a_stop_during_unmet_holder_discovery_is_reported_as_stopped() {
        struct StopsInHolders<'a>(&'a AtomicBool);

        impl Relief for StopsInHolders<'_> {
            fn relieve(
                &mut self,
                _short: &Shortfall,
                _say: &mut dyn FnMut(&str),
                _stopped: &dyn Fn() -> bool,
            ) {
            }

            fn holders(
                &mut self,
                _short: &Shortfall,
                _stopped: &dyn Fn() -> bool,
            ) -> Option<String> {
                self.0.store(true, Ordering::SeqCst);
                Some("holder scan stopped".to_owned())
            }
        }

        let scratch = aterm_tempfile::tempdir().unwrap();
        let ledger = Ledger::at(scratch.path().join("ledger"));
        let mut reading = Reading::of_gib(48, 40, 14);
        reading.volume = Some(Volume {
            mount: PathBuf::from("/vol"),
            total: 40 * GIB,
            free: 20 * GIB,
            floor: 4 * GIB,
            room: 2 * GIB,
        });
        let machine = crate::Fixed::new(reading);
        let stopped = AtomicBool::new(false);
        let mut relief = StopsInHolders(&stopped);
        let got = admit_waiting(
            &ledger,
            &machine,
            Need::new("verify").disk(27 * GIB, "/vol"),
            Admit::Wait,
            Pace::default(),
            &mut relief,
            &mut |_| {},
            &|| stopped.load(Ordering::SeqCst),
        );

        let Err(Error::Stopped {
            signal: None,
            short: Some(short),
        }) = &got
        else {
            panic!("the stop during holder discovery wins, got {got:?}");
        };
        assert!(short.shorts.iter().any(Short::unservable), "{short}");
        assert!(ledger.list().unwrap().is_empty(), "the stopped claim left");
    }
}

#[cfg(test)]
mod timing_completion_tests {
    use super::*;

    #[test]
    fn protection_expiring_during_work_cannot_qualify_its_result() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ledger = Ledger::at(scratch.path().join("ledger"));
        let machine = crate::tests::TimingMachine::new(&[(0, 0), (1, 99), (2, 198), (3, 297)]);
        let mut claim = crate::tests::timed_in(&ledger, &machine);
        claim
            .measurement_entry_on(&machine, Pace::default(), &mut |_| false)
            .unwrap();
        claim.record.need.quiet_policy = crate::QuietPolicy::Unknown;
        assert_eq!(
            claim.timing_admission(),
            Err(crate::TimingIssue::UnknownAdmission)
        );
        claim.record.need.quiet_policy = crate::QuietPolicy::Timing;
        {
            // Inject completion after the EXISTING hold, without waiting or changing its duration.
            let admitted =
                now_ms().saturating_sub(millis(claim.record.need.patience.unwrap().hold));
            claim.record.held.as_mut().unwrap().admitted_ms = admitted;
            claim.record.timing.admitted_ms = Some(admitted);
        }
        assert_eq!(claim.timing_admission(), Err(crate::TimingIssue::Expired));
    }
}

#[cfg(all(test, unix))]
#[path = "cohort_budget_tests.rs"]
mod cohort_budget_tests;
