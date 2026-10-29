// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ADMISSION: a [`Waiter`] enters the ledger, and each [`Waiter::poll`] reads the machine and
//! the claims ahead of it and either admits it (a [`Claim`]) or names what it is short of
//! ([`Shortfall`]). [`admit_with`] polls until it is admitted, saying once a minute what it
//! waits for; it never gives up. Memory, cores and the quiet machine come back as the claims
//! ahead end, so a need of them fits once its holders end. Disk does not: what a claim wrote
//! stays on its volume once it ends (a build's caches), so a claim's end gives back only the
//! part of its reservation it never wrote, and only space freed (the build cache's relief, the
//! window's disk watch, a person) raises what is free. A waiter whose disk need is more than its
//! volume holds above the floor now is one no claim's end can admit (UNSERVABLE,
//! [`Short::unservable`]): it reserves no disk against the waiters behind it, which are admitted
//! around it while it waits for space, and it reserves again in its turn once its need fits
//! (`reservations`). That is the trade: such a waiter is overtaken by every claim that fits
//! while it cannot, and those claims' writes take room that space freed for it gave, so it is
//! admitted only once space freed reaches its need beside what they still hold. The look at
//! which it fits the room again but is short asks the relief once more, sized for them, where a
//! claim reserves that disk that did not at the look its last relief was sized for
//! ([`Relief::relieve`]); where nothing more can be freed it waits for a person, as it did when
//! it held every claim behind it. A relief, or any pass that runs while it waits, can remove what
//! a need was sized on (a run's own build directory, which the run then writes again from
//! empty): the need sized again after each relief and at the look that admits a claim that
//! waited ([`Relief::resized`]) re-enters the wait when it grew. SIGINT, SIGTERM or SIGHUP ends the wait within a nap
//! ([`aterm_toolchain::interrupt`]), as its caller can ([`admit_until`]): the claim leaves the
//! queue and [`Error::Stopped`] names what it was short of. A need that can never fit
//! ([`never`]) is refused before it enters. A quiet claim with a [`crate::Patience`] waits for
//! the load within it: past its reservation age it holds back claimed work that asked after it
//! ([`Short::Reserved`]), and at its bound it starts whatever the load ([`QuietStart`]).
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
//! room. Bounded so, every memory wait ends once the claims ahead of it end. A need with a margin
//! (a VM's boot, which pins its memory) is never admitted alone: it waits for room beside it.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::floor::GIB;
use crate::ledger::{Held, Laid, Ledger, Record, millis};
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

/// `bytes` as a person reads it: whole GiB bare (`48 GiB`), else to a tenth (`0.5 GiB`).
#[must_use]
pub fn gib(bytes: u64) -> String {
    let g = u128::from(GIB);
    let tenths = (u128::from(bytes) * 10 + g / 2) / g;
    if tenths % 10 == 0 {
        format!("{} GiB", tenths / 10)
    } else {
        format!("{}.{} GiB", tenths / 10, tenths % 10)
    }
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
    },
    /// The machine is not quiet: the 1-minute load is above what the claim allows.
    Load {
        /// The 1-minute load now.
        load1: f64,
        /// The most it allows.
        max: f64,
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

    /// Whether a [`Relief`] may free some of this: what claims hold, but no reservation, which
    /// only time ends.
    const fn relievable(&self) -> bool {
        self.held_by_claims() && !matches!(self, Short::Reserved { .. })
    }

    /// Whether this is a disk need no claim's end can meet (UNSERVABLE): more than its volume
    /// holds above the floor now, before any reservation. What a claim wrote stays on the volume
    /// once it ends, so only space freed admits it; while it waits it reserves no disk against
    /// the waiters behind it (`reservations`, by the same test, `beyond_room`).
    #[must_use]
    pub const fn unservable(&self) -> bool {
        matches!(self, Short::Disk { need, above_floor, .. } if beyond_room(*need, *above_floor))
    }
}

/// Whether a disk need of `need` bytes is more than `above_floor`, what its volume holds above
/// the floor now (UNSERVABLE): the one test [`Short::unservable`], which words a wait line, and
/// `unservable`, which decides what a waiter reserves, share.
const fn beyond_room(need: u64, above_floor: u64) -> bool {
    need > above_floor
}

/// What a waiter's line ([`Waiter::line`]), and no other line, says of a need that is
/// [`Short::unservable`]: what admits it and how to start it now. A claim `--admit now` started
/// holds its whole need, and one an interrupt ended waits no more, so neither says it.
pub(crate) const UNSERVABLE: &str = "; no claim's end can make room for its disk, which is more \
     than is free above the floor (what a claim wrote stays on the volume): it waits for space to \
     be freed, holding back no claim behind it, and `--admit now` starts it at once";

impl fmt::Display for Short {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
}

impl fmt::Display for Holder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        if self.alone {
            parts.push(format!("{} of memory alone", gib(self.claimed)));
        } else if self.mem > 0 {
            parts.push(format!("{} of memory", gib(self.mem)));
        } else if self.claimed > 0 {
            parts.push(format!(
                "{} of memory, all of it in use by its tree",
                gib(self.claimed)
            ));
        }
        if self.disk > 0 {
            parts.push(format!("{} of disk", gib(self.disk)));
        }
        if self.quiet {
            parts.push("the quiet machine".to_owned());
        }
        if parts.is_empty() {
            parts.push("a claim".to_owned());
        }
        write!(
            f,
            "pid {} {} {} for {}: \"{}\"",
            self.pid,
            if self.held {
                "holds"
            } else {
                "waits ahead for"
            },
            parts.join(" and "),
            span(self.age),
            self.what
        )?;
        if !self.checkout.is_empty() {
            write!(f, " in {}", self.checkout)?;
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
    let short_of_disk = short.shorts.iter().any(|s| matches!(s, Short::Disk { .. }));
    short
        .holders
        .iter()
        .filter(|h| short_of_disk && h.disk > 0)
        .map(|h| h.order)
        .collect()
}

impl fmt::Display for Shortfall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let shorts: Vec<String> = self.shorts.iter().map(ToString::to_string).collect();
        f.write_str(&shorts.join("; "))?;
        if !self.shorts.iter().any(Short::held_by_claims) {
            return Ok(());
        }
        if self.holders.is_empty() {
            write!(f, "; no claim holds it: other work on {HOST}")
        } else {
            let holders: Vec<String> = self.holders.iter().map(ToString::to_string).collect();
            write!(f, "; {}", holders.join("; "))
        }
    }
}

/// A claim in the ledger and what its holder's process tree, and the processes it is credited with, use now.
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
    /// above the floor now (UNSERVABLE, the test [`Short::unservable`] words a wait line by): no
    /// claim's end can make room for it, only space freed, so it reserves no disk against the
    /// claims behind it.
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
                if *pid > 1
                    && taken.insert(*pid)
                    && let Some(u) = trees.get(pid)
                {
                    credited.footprint = credited.footprint.saturating_add(u.footprint);
                    credited.written = credited.written.saturating_add(u.written);
                }
            }
            Other {
                record: r,
                used: trees.get(&r.pid).copied().unwrap_or_default(),
                credited,
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
    let mut credit: HashMap<u32, TreeUse> = HashMap::new();
    for o in ahead {
        credit.entry(o.record.pid).or_insert(o.used);
    }
    let mut order: Vec<usize> = (0..ahead.len()).collect();
    order.sort_by_key(|&i| (ahead[i].record.held.is_none(), ahead[i].record.order()));
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
    let holders: Vec<u32> = records
        .iter()
        .filter(|r| r.held.is_some() && r.need.quiet.is_some())
        .map(|r| r.pid)
        .collect();
    holders.contains(&pid)
        || procs.is_some_and(|p| p.ancestors(pid).iter().any(|a| holders.contains(a)))
}

/// The claims a waiter of the process `pid` is held to: every other claim but those its
/// ancestors hold or wait for. A waiter whose ancestor holds a claim is that admitted work going
/// on (verify's run starting the lab's VM boot, which claims the VM's disk for itself), so it is
/// held only to the other HELD claims: never to its ancestors', which end only once it has run,
/// nor to any waiter, which waits for that work in turn. Held to either, it would wait for what
/// waits for it, with no end. Its need is then checked beside the other held claims alone: what
/// its ancestors have yet to write may overrun the room by up to their reservation, into the
/// floor, which is what the floor is kept for. Without a process sweep no ancestor is known,
/// and every claim counts.
pub(crate) fn counted<'r>(
    records: Vec<&'r Record>,
    pid: u32,
    procs: Option<&crate::Procs>,
) -> Vec<&'r Record> {
    let ancestors = procs.map(|p| p.ancestors(pid)).unwrap_or_default();
    let inside = records
        .iter()
        .any(|r| r.held.is_some() && ancestors.contains(&r.pid));
    records
        .into_iter()
        .filter(|r| !ancestors.contains(&r.pid) && (!inside || r.held.is_some()))
        .collect()
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
        .filter(|r| r.order() < mine.order() && !kin(r.pid))
        .find_map(|r| r.reserves(now).map(|left| (*r, left)))
}

/// What a look that admits a claim grants it ([`decide`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Grant {
    /// The cores it is granted (CORES-GRANT, [`grant`]).
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

/// Whether `need` fits now beside every claim ahead of the waiter at `mine` (held, or waiting since earlier, less the disk of a waiter no claim's end can admit: [`reservations`]), and, for a need of memory, under memory pressure short of critical ([`Short::Pressure`]), with no claim held alone ([`Short::Alone`]), or alone where it is short only of memory no claim holds and either asked to be or is below what can ever be available (ALONE, [`mod@admit`]): `Ok(what it is granted)`, or what it is short of. A waiter of `mine`'s own process (`mine.1`) is not ahead of its memory.
pub(crate) fn decide(
    need: &Need,
    mine: (u64, u32, u64),
    reading: &Reading,
    others: &[Other<'_>],
    now: u64,
) -> Result<Grant, Shortfall> {
    let ahead: Vec<&Other<'_>> = others
        .iter()
        .filter(|o| o.record.held.is_some() || o.record.order() < mine)
        .collect();
    let volume = reading.volume.as_ref();
    let reserves = reservations(&ahead, volume);
    // A waiter of this claim's own process: its process orders its own claims, so it holds none
    // of this one's memory back.
    let sibling = |o: &Other<'_>| o.record.held.is_none() && o.record.pid == mine.1;
    let mut shorts = Vec::new();
    let (mut want_mem, mut want_disk, mut want_quiet) = (false, false, false);
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
            shorts.push(Short::Disk {
                need: need.disk,
                mount: v.mount.clone(),
                above_floor: v.above_floor(),
                floor: v.floor,
                reserved,
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
    if !shorts.is_empty() {
        let holders = ahead
            .iter()
            .zip(&reserves)
            .filter(|(o, r)| {
                (want_mem && o.record.need.mem > 0 && !sibling(o))
                    || (want_disk && r.disk.unwrap_or(0) > 0)
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
            })
            .collect();
        return Err(Shortfall { shorts, holders });
    }
    Ok(Grant {
        cores: grant(need.cores, reading, others, now),
        alone,
    })
}

/// THE CORES A CLAIM ASKING `asked` IS GRANTED, never waited for (CORES-GRANT): its share of
/// the room, split evenly between the parties that contend for it.
/// - The room is the cores less what every held claim was granted, so claims together hold
///   about a job a core at most (a grant is at least one, and `--admit now` short of anything
///   else takes all it asked): half the quiet bound ([`crate::quiet_bound`]). Past about half
///   the bound of unclaimed load that no longer keeps a quiet waiter's room; its reservation
///   ([`Short::Reserved`]) does, holding back a cores claim made after it like any claimed work.
/// - The parties are this claim, every other claim of cores not yet admitted (whatever it waits
///   for: memory, disk or a reservation, since no claim waits for cores), and the work no claim
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
pub(crate) fn grant(asked: u32, reading: &Reading, others: &[Other<'_>], now: u64) -> u32 {
    if asked == 0 {
        return 0;
    }
    let cores = f64::from(reading.cores.max(1));
    let tau = LOAD5_TIME_CONSTANT.as_secs_f64();
    let (mut held, mut in_load, mut waiting) = (0.0, 0.0, 0.0);
    for o in others.iter().filter(|o| o.record.need.cores > 0) {
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
    /// Whether it started at the end of its patience ([`crate::Patience::start`]), the load
    /// still above its bound.
    pub at_bound: bool,
    /// How long it reserved the machine before it started; zero when it never did.
    pub reserved: Duration,
}

impl fmt::Display for QuietStart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.load1 {
            Some(l) if self.at_bound => write!(
                f,
                "started at its bound under load {l:.1} > {:.1}",
                self.max
            )?,
            Some(l) => write!(f, "started under load {l:.1} <= {:.1}", self.max)?,
            None => f.write_str("started with the load unread")?,
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

/// An admitted claim: held until it is dropped, or until its holder dies.
#[derive(Debug)]
pub struct Claim {
    laid: Laid,
    record: Record,
    waited: Duration,
    /// Where its measured writes lie in its file, once written ([`Self::wrote`]).
    wrote_at: Option<u64>,
    /// How a quiet claim started; `None` for any other, one a held quiet claim covers, or one
    /// `--admit now` admitted.
    quiet: Option<QuietStart>,
    /// How its memory was admitted alone; `None` beside other claims.
    alone: Option<Alone>,
    /// What it was short of at the last look that found it waiting; `None` when it never waited.
    short: Option<Shortfall>,
}

impl Claim {
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

    /// How long it waited.
    #[must_use]
    pub const fn waited(&self) -> Duration {
        self.waited
    }

    /// How a quiet claim started: the load, whether at its bound, how long it reserved the
    /// machine. `None` for any other claim, one a held quiet claim covers, or one `--admit now`
    /// admitted.
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
    /// The record could not be written; it keeps the credit it had.
    pub fn credit(&mut self, pids: &[u32]) -> io::Result<()> {
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
}

/// What one look at the ledger found ([`Waiter::poll`]).
#[derive(Debug)]
pub enum Polled {
    /// Admitted: hold the claim while the work runs.
    Admitted(Claim),
    /// Still waiting, short of what the shortfall names.
    Waiting(Waiter, Shortfall),
}

/// A claim in the queue, not yet admitted. It leaves the ledger when dropped. Until it is
/// admitted its wait is marked where a ceiling over this process's tree reads it
/// ([`aterm_toolchain::waits`]), saying at each look what it is short of and who holds that.
#[derive(Debug)]
pub struct Waiter {
    ledger: Ledger,
    record: Record,
    laid: Laid,
    /// The path whose volume the reading is taken for.
    at: Option<PathBuf>,
    since: Instant,
    mark: Option<aterm_toolchain::waits::Mark>,
}

impl Waiter {
    /// Enter `ledger`'s queue for `need`, read on `machine` (its disk need is placed on the volume it reads).
    ///
    /// # Errors
    /// The ledger's directory or the claim file could not be made, locked or written.
    pub fn enter(ledger: &Ledger, machine: &dyn Machine, mut need: Need) -> io::Result<Waiter> {
        let at = need.path.clone().or_else(|| std::env::current_dir().ok());
        need.path = (need.disk > 0)
            .then(|| machine.read(at.as_deref()).volume.map(|v| v.mount))
            .flatten();
        let mark = aterm_toolchain::waits::Mark::begin(&format!("capacity: {} waits", need.what));
        let (record, laid) = ledger.lay(&need, now_ms())?;
        Ok(Waiter {
            ledger: ledger.clone(),
            record,
            laid,
            at,
            since: Instant::now(),
            mark,
        })
    }

    /// Its record, as other sessions read it.
    #[must_use]
    pub const fn record(&self) -> &Record {
        &self.record
    }

    /// Look once: admitted if the need fits beside every claim ahead, else what it is short of. A dead holder's file is removed on the way.
    #[must_use]
    pub fn poll(self, machine: &dyn Machine) -> Polled {
        let turn = match self.ledger.try_turn() {
            Ok(Ok(turn)) => turn,
            Ok(Err(holder)) => {
                return Polled::Waiting(self, Shortfall::only(Short::Turn { holder }));
            }
            Err(e) => return Polled::Waiting(self, Shortfall::only(Short::Ledger(e.to_string()))),
        };
        let found = match self.look(machine) {
            Ok(found) => found,
            Err(e) => return Polled::Waiting(self, Shortfall::only(Short::Ledger(e.to_string()))),
        };
        let mut waiter = self;
        // First next for the quiet machine: its patience counts from this look, and every other
        // waiter reads when from its record. Unwritten, the next look tries again.
        if found.next && waiter.laid.append(&Record::next_line(found.now)).is_ok() {
            waiter.record.next_ms = Some(found.now);
        }
        let polled = match found.decided {
            Ok((granted, quiet)) => waiter.grant(granted, quiet),
            Err(short) => {
                let line = waiter.line(&short);
                if let Some(mark) = &mut waiter.mark {
                    mark.say(&line);
                }
                Polled::Waiting(waiter, short)
            }
        };
        drop(turn);
        polled
    }

    /// Read the machine and the ledger, and decide.
    fn look(&self, machine: &dyn Machine) -> io::Result<Found> {
        let now = now_ms();
        let reading = machine.read(self.at.as_deref());
        let records = self.ledger.scan(true)?;
        let mine = self.record.order();
        let records: Vec<&Record> = records.iter().filter(|r| r.order() != mine).collect();
        let procs = (!records.is_empty()).then(|| machine.procs()).flatten();
        // Coverage reads the claims before `counted` leaves out the ancestors': an ancestor's held
        // quiet claim is what covers this one.
        let mut need = self.record.need.clone();
        if need.quiet.is_some() && covered(self.record.pid, &records, procs.as_ref()) {
            need.quiet = None;
        }
        let bound = need.quiet;
        // Past its patience the load holds a quiet claim back no longer: only another quiet claim
        // does, so two timed runs never overlap.
        let past = bound.is_some()
            && self
                .record
                .need
                .patience
                .zip(self.record.next_ms)
                .is_some_and(|(p, next)| now >= next.saturating_add(millis(p.start)));
        if past {
            need.quiet = Some(f64::INFINITY);
        }
        let records = counted(records, self.record.pid, procs.as_ref());
        let mut claimants: Vec<u32> = records.iter().map(|r| r.pid).collect();
        claimants.push(self.record.pid);
        let mut roots = claimants.clone();
        roots.extend(records.iter().flat_map(|r| r.credit.iter().copied()));
        let trees = procs.as_ref().map(|p| p.trees(&roots)).unwrap_or_default();
        let others = others(&records, claimants.into_iter().collect(), &trees);
        let mut decided = decide(&need, mine, &reading, &others, now);
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
                s.shorts.iter().any(|x| matches!(x, Short::Load { .. }))
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
                at_bound: past && reading.load1.is_some_and(|l| l > max),
                reserved: Duration::from_millis(reserved),
            });
            (granted, quiet)
        });
        Ok(Found { decided, next, now })
    }

    /// Mark it admitted with `granted` (its cores, and whether its memory runs alone), a quiet
    /// claim having started as `quiet` says.
    fn grant(self, granted: Grant, quiet: Option<QuietStart>) -> Polled {
        let Waiter {
            ledger,
            mut record,
            mut laid,
            at,
            since,
            mark,
        } = self;
        let held = Held {
            admitted_ms: now_ms(),
            cores: granted.cores,
            alone: granted.alone.is_some(),
        };
        match ledger.admit(&mut laid, &mut record, held) {
            Ok(()) => Polled::Admitted(Claim {
                laid,
                record,
                waited: since.elapsed(),
                wrote_at: None,
                quiet,
                alone: granted.alone,
                short: None,
            }),
            Err(e) => Polled::Waiting(
                Waiter {
                    ledger,
                    record,
                    laid,
                    at,
                    since,
                    mark,
                },
                Shortfall::only(Short::Ledger(e.to_string())),
            ),
        }
    }

    /// Admit it at once, room or not (`--admit now`), with what it would have waited for.
    ///
    /// # Errors
    /// The claim file could not be marked admitted.
    fn force(self, machine: &dyn Machine) -> io::Result<(Claim, Option<Shortfall>)> {
        let turn = self.ledger.try_turn().ok().and_then(Result::ok);
        let asked = Grant {
            cores: self.record.need.cores,
            alone: None,
        };
        let (granted, would) = match self.look(machine).map(|found| found.decided) {
            Ok(Ok((granted, _))) => (granted, None),
            Ok(Err(short)) => (asked, Some(short)),
            Err(e) => (asked, Some(Shortfall::only(Short::Ledger(e.to_string())))),
        };
        let polled = self.grant(granted, None);
        drop(turn);
        match polled {
            Polled::Admitted(claim) => Ok((claim, would)),
            Polled::Waiting(_, short) => Err(io::Error::other(short.to_string())),
        }
    }

    /// Its wait line: for a disk need no claim's end can meet, that only space freed admits it
    /// and `--admit now` starts it ([`UNSERVABLE`]); for a quiet claim next for the quiet
    /// machine, when its patience reserves the machine and when it starts whatever the load.
    pub(crate) fn line(&self, short: &Shortfall) -> String {
        let mut line = format!(
            "capacity: {} waits ({}): {short}",
            self.record.need.what,
            span(self.since.elapsed())
        );
        if short.shorts.iter().any(Short::unservable) {
            line.push_str(UNSERVABLE);
        }
        let waits_load = short.shorts.iter().any(|s| matches!(s, Short::Load { .. }));
        if let (true, Some(p), Some(next)) =
            (waits_load, self.record.need.patience, self.record.next_ms)
        {
            let now = now_ms();
            // Whole seconds to go, rounded up: a person reads `2m00s` for the minute and 59.9 s left.
            let at = |d: Duration| {
                let ms = next.saturating_add(millis(d)).saturating_sub(now);
                Duration::from_secs(ms.div_ceil(1000))
            };
            let start = span(at(p.start));
            match at(p.reserve) {
                Duration::ZERO => line.push_str(&format!(
                    "; it reserves the machine, and starts whatever the load in {start}"
                )),
                d => line.push_str(&format!(
                    "; it reserves the machine in {}, and starts whatever the load in {start}",
                    span(d)
                )),
            }
        }
        line
    }
}

/// What one look found ([`Waiter::look`]): admitted with what it is granted ([`Grant`]) and, for
/// a quiet claim, how it started; or what it is short of.
struct Found {
    decided: Result<(Grant, Option<QuietStart>), Shortfall>,
    /// The waiter is first next for the quiet machine at this look ([`Record::next_ms`]).
    next: bool,
    /// When it looked, unix ms.
    now: u64,
}

/// Why [`admit`] admitted nothing.
#[derive(Debug)]
pub enum Error {
    /// The need can never fit on this machine ([`never`]).
    Never(String),
    /// The ledger could not be used.
    Ledger(io::Error),
    /// An interrupt, or the caller ([`admit_until`]), ended the wait. The claim has left the
    /// queue.
    Stopped {
        /// The interrupt ([`aterm_toolchain::interrupt`]); `None` when the caller stopped it.
        signal: Option<i32>,
        /// What it was short of at its last look; `None` when it never looked.
        short: Option<Shortfall>,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Never(why) => write!(f, "{why}; it can never be admitted here"),
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
/// [`Error::Never`] for a need that can never fit; [`Error::Ledger`] when there is no home, the ledger cannot be written, or the claim's file is removed while it waits.
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

/// What a waiter can do about a shortfall besides wait: free what it can before it says it waits
/// ([`Self::relieve`]), and name what holds the rest beyond the claims a wait line names
/// ([`Self::holders`]). [`admit_relieved`] calls it; [`admit_with`] has none. Each is part of the
/// wait, so each gives up what is left of its work once `stopped` says an interrupt came.
pub trait Relief {
    /// Called at the first look that finds the need short of something a claim can hold, before
    /// any wait line: free what can be freed toward it, saying through `say` what was done. The
    /// next look follows at once, so a need the relief met is admitted with no wait. Called again
    /// at a look at which a disk need no claim's end could meet ([`Short::unservable`]) fits the
    /// room once more but is still short, where a claim reserves that disk that did not at the
    /// look the last call was sized for: a claim admitted around it while it could not fit holds
    /// the rest, which no earlier call was sized for. Where every claim that reserves it was
    /// counted by the last call, that call was sized for them, and it waits. Called afresh, as at
    /// a first look, once its need sized again ([`Self::resized`]) has re-entered the wait.
    fn relieve(&mut self, short: &Shortfall, say: &mut dyn FnMut(&str), stopped: &dyn Fn() -> bool);

    /// Words a wait line ends with: what holds the rest (the largest users of a short volume).
    fn holders(&mut self, short: &Shortfall, stopped: &dyn Fn() -> bool) -> Option<String>;

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
}

/// Admit `need` in `ledger` on `machine`: refused at once when it can never fit (`--admit wait` only), admitted at once under `--admit now` (saying what it would have waited for), else waiting with no deadline and saying through `say` what it is short of and who holds it, at the first look and every [`Pace::line_every`] after. A wait only for the ledger's lock is said once one holder has kept it a whole [`Pace::line_every`], naming that holder.
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
/// wait began), which it asks before each look and through each nap: the claim then leaves the
/// queue, and [`Error::Stopped`] names what it was short of.
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
/// of one, and one that came during the look that admitted it ends it all the same.
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
    let watch = (mode == Admit::Wait).then(aterm_toolchain::interrupt::Watch::enter);
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
            w.nap(d, stopped);
        }
        None => std::thread::sleep(d),
    };
    let at = need.path.clone().or_else(|| std::env::current_dir().ok());
    let reading = machine.read(at.as_deref());
    let refused = never(&need, &reading);
    if mode == Admit::Wait
        && let Some(why) = refused
    {
        return Err(Error::Never(why));
    }
    let what = need.what.clone();
    // The need it waits with, entered again once a relief's resizing grew its disk.
    let mut entered = need.clone();
    let mut waiter = Waiter::enter(ledger, machine, need).map_err(Error::Ledger)?;
    // When it first entered: a need entered again keeps it, so its wait counts from there.
    let first = waiter.since;
    if mode == Admit::Now {
        let (claim, would) = waiter.force(machine).map_err(Error::Ledger)?;
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
        if matches!(waiter.laid.path().try_exists(), Ok(false)) {
            let gone = format!(
                "its claim file {} was removed while it waited",
                waiter.laid.path().display()
            );
            return Err(Error::Ledger(io::Error::new(io::ErrorKind::NotFound, gone)));
        }
        match waiter.poll(machine) {
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
                if since.elapsed() >= pace.line_every
                    && said.is_none_or(|t| t.elapsed() >= pace.line_every)
                {
                    say(&format!(
                        "{}, for {}",
                        w.line(&short),
                        span(since.elapsed())
                    ));
                    said = Some(Instant::now());
                }
                waiter = w;
                if last.is_none() {
                    last = Some(short);
                }
                nap(pace.poll.min(TURN_GAP));
            }
            Polled::Waiting(w, short)
                if relieve_now(sized_for.as_deref(), unservable, &short)
                    && short.shorts.iter().any(Short::relievable) =>
            {
                waited = true;
                sized_for = Some(disk_reservers(&short));
                unservable = short.shorts.iter().any(Short::unservable);
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
