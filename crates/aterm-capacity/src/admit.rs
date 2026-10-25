// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ADMISSION: a [`Waiter`] enters the ledger, and each [`Waiter::poll`] reads the machine and
//! the claims ahead of it and either admits it (a [`Claim`]) or names what it is short of
//! ([`Shortfall`]). [`admit_with`] polls until it is admitted, saying once a minute what it
//! waits for; it never gives up, because the need fits once the holders end. A need that can
//! never fit ([`never`]) is refused before it enters.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::floor::GIB;
use crate::ledger::{Held, Laid, Ledger, Record};
use crate::reading::{Machine, Reading, TreeUse, Volume};
use crate::{Admit, Need};

/// A claim's own work reaches the 5-minute load within about this long: until then the cores it was granted count against later grants ([`Claim::cores`]).
pub const LOAD5_REACH: Duration = Duration::from_secs(300);

/// How soon a waiter looks again when another waiter held the admissions' lock: that lock is held only while one waiter reads the ledger, so this is no wait for capacity, and says a line only once one holder has kept it a whole [`Pace::line_every`] (stopped, say, while it decides).
const TURN_GAP: Duration = Duration::from_millis(20);

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

/// Why `need` can never be admitted on the machine `reading` reads: more memory than the Mac has, or more disk than its volume holds above the floor. `None` when it can.
#[must_use]
pub fn never(need: &Need, reading: &Reading) -> Option<String> {
    if let Some(physical) = reading.physical
        && need.mem > physical
    {
        return Some(format!(
            "{}: needs {}, {HOST} has {} of memory",
            need.what,
            gib(need.mem),
            gib(physical)
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

/// One thing a waiter is short of.
#[derive(Clone, Debug, PartialEq)]
pub enum Short {
    /// Memory: it needs `need`; `available` is free now, and the claims ahead reserve `reserved` of it.
    Memory {
        /// Its peak.
        need: u64,
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
        /// What the claims ahead on that volume still reserve.
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
            Short::Memory { .. } | Short::Disk { .. } | Short::Load { .. } | Short::Quiet { .. }
        )
    }
}

impl fmt::Display for Short {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Short::Memory {
                need,
                available,
                reserved,
            } => {
                let room = available.saturating_sub(*reserved);
                write!(
                    f,
                    "memory short by {}: needs {}, {} available less {} reserved",
                    gib(need.saturating_sub(room)),
                    gib(*need),
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
    /// The disk it still reserves on the waiter's volume.
    pub disk: u64,
    /// Whether it is a quiet claim.
    pub quiet: bool,
}

impl fmt::Display for Holder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        if self.mem > 0 {
            parts.push(format!("{} of memory", gib(self.mem)));
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

/// A claim in the ledger and what its holder's process tree uses now.
pub(crate) struct Other<'a> {
    pub(crate) record: &'a Record,
    /// The holder's tree ([`crate::Procs::trees`]): the same for every claim one process holds, and credited once among them ([`reservations`]).
    pub(crate) used: TreeUse,
}

/// What each claim of `ahead` still reserves, `(memory, disk on volume)`, in `ahead`'s order. A
/// holder's tree use is credited once across its claims (those held first, then those waiting,
/// each oldest first), each taking what it can of its peak: one process holding several claims
/// is never credited with its whole tree for each.
fn reservations(ahead: &[&Other<'_>], volume: Option<&Volume>) -> Vec<(u64, u64)> {
    let mut credit: HashMap<u32, TreeUse> = HashMap::new();
    for o in ahead {
        credit.entry(o.record.pid).or_insert(o.used);
    }
    let mut order: Vec<usize> = (0..ahead.len()).collect();
    order.sort_by_key(|&i| (ahead[i].record.held.is_none(), ahead[i].record.order()));
    let mut out = vec![(0, 0); ahead.len()];
    for i in order {
        let record = ahead[i].record;
        let left = credit.entry(record.pid).or_default();
        let disk = match (volume, record.need.path.as_deref()) {
            (Some(v), Some(mount)) if mount == v.mount => record.need.disk,
            _ => 0,
        };
        let mem_taken = record.need.mem.min(left.footprint);
        let disk_taken = disk.min(left.written);
        left.footprint -= mem_taken;
        left.written -= disk_taken;
        out[i] = (record.need.mem - mem_taken, disk - disk_taken);
    }
    out
}

/// Whether `need` fits now beside every claim ahead of the waiter at `mine` (held, or waiting since earlier): `Ok(cores granted)`, or what it is short of.
pub(crate) fn decide(
    need: &Need,
    mine: (u64, u32, u64),
    reading: &Reading,
    others: &[Other<'_>],
    now: u64,
) -> Result<u32, Shortfall> {
    let ahead: Vec<&Other<'_>> = others
        .iter()
        .filter(|o| o.record.held.is_some() || o.record.order() < mine)
        .collect();
    let volume = reading.volume.as_ref();
    let reserves = reservations(&ahead, volume);
    let mut shorts = Vec::new();
    let (mut want_mem, mut want_disk, mut want_quiet) = (false, false, false);
    if need.mem > 0
        && let Some(available) = reading.available.or(reading.physical)
    {
        let reserved: u64 = reserves.iter().map(|r| r.0).sum();
        if available.saturating_sub(reserved) < need.mem {
            want_mem = true;
            shorts.push(Short::Memory {
                need: need.mem,
                available,
                reserved,
            });
        }
    }
    if need.disk > 0
        && let Some(v) = volume
    {
        let reserved: u64 = reserves.iter().map(|r| r.1).sum();
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
            .map(|(o, &(mem, disk))| Holder {
                pid: o.record.pid,
                what: o.record.need.what.clone(),
                checkout: o.record.checkout.clone(),
                held: o.record.held.is_some(),
                age: Duration::from_millis(now.saturating_sub(o.record.start_ms)),
                mem,
                disk,
                quiet: o.record.need.quiet.is_some(),
            })
            .filter(|h| {
                (want_mem && h.mem > 0) || (want_disk && h.disk > 0) || (want_quiet && h.quiet)
            })
            .collect();
        return Err(Shortfall { shorts, holders });
    }
    if need.cores == 0 {
        return Ok(0);
    }
    let reach = u64::try_from(LOAD5_REACH.as_millis()).unwrap_or(u64::MAX);
    let recent: u32 = others
        .iter()
        .filter_map(|o| o.record.held)
        .filter(|h| now.saturating_sub(h.admitted_ms) < reach)
        .map(|h| h.cores)
        .sum();
    let idle = reading
        .idle_cores()
        .unwrap_or_else(|| f64::from(reading.cores));
    let idle = if idle.is_finite() {
        idle.floor() as u32
    } else {
        0
    };
    Ok(idle.saturating_sub(recent).clamp(1, need.cores))
}

/// An admitted claim: held until it is dropped, or until its holder dies.
#[derive(Debug)]
pub struct Claim {
    laid: Laid,
    record: Record,
    waited: Duration,
}

impl Claim {
    /// The cores it was granted: as many of those it asked for as were idle, at least one; 0 when it asked for none.
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

    /// Its file in the ledger.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.laid.path()
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

/// A claim in the queue, not yet admitted. It leaves the ledger when dropped.
#[derive(Debug)]
pub struct Waiter {
    ledger: Ledger,
    record: Record,
    laid: Laid,
    /// The path whose volume the reading is taken for.
    at: Option<PathBuf>,
    since: Instant,
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
        let (record, laid) = ledger.lay(&need, now_ms())?;
        Ok(Waiter {
            ledger: ledger.clone(),
            record,
            laid,
            at,
            since: Instant::now(),
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
        let polled = match found {
            Ok(cores) => self.grant(cores),
            Err(short) => Polled::Waiting(self, short),
        };
        drop(turn);
        polled
    }

    /// Read the machine and the ledger, and decide.
    fn look(&self, machine: &dyn Machine) -> io::Result<Result<u32, Shortfall>> {
        let reading = machine.read(self.at.as_deref());
        let records = self.ledger.scan(true)?;
        let mine = self.record.order();
        let records: Vec<&Record> = records.iter().filter(|r| r.order() != mine).collect();
        let procs = records
            .iter()
            .any(|r| r.need.mem > 0 || r.need.disk > 0)
            .then(|| machine.procs())
            .flatten();
        let mut claimants: Vec<u32> = records.iter().map(|r| r.pid).collect();
        claimants.push(self.record.pid);
        let trees = procs.map(|p| p.trees(&claimants)).unwrap_or_default();
        let others: Vec<Other<'_>> = records
            .iter()
            .map(|r| Other {
                record: r,
                used: trees.get(&r.pid).copied().unwrap_or_default(),
            })
            .collect();
        Ok(decide(&self.record.need, mine, &reading, &others, now_ms()))
    }

    /// Mark it admitted with `cores` granted.
    fn grant(self, cores: u32) -> Polled {
        let Waiter {
            ledger,
            mut record,
            mut laid,
            at,
            since,
        } = self;
        let held = Held {
            admitted_ms: now_ms(),
            cores,
        };
        match ledger.admit(&mut laid, &mut record, held) {
            Ok(()) => Polled::Admitted(Claim {
                laid,
                record,
                waited: since.elapsed(),
            }),
            Err(e) => Polled::Waiting(
                Waiter {
                    ledger,
                    record,
                    laid,
                    at,
                    since,
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
        let (cores, would) = match self.look(machine) {
            Ok(Ok(cores)) => (cores, None),
            Ok(Err(short)) => (self.record.need.cores, Some(short)),
            Err(e) => (
                self.record.need.cores,
                Some(Shortfall::only(Short::Ledger(e.to_string()))),
            ),
        };
        let polled = self.grant(cores);
        drop(turn);
        match polled {
            Polled::Admitted(claim) => Ok((claim, would)),
            Polled::Waiting(_, short) => Err(io::Error::other(short.to_string())),
        }
    }

    /// Its wait line.
    fn line(&self, short: &Shortfall) -> String {
        format!(
            "capacity: {} waits ({}): {short}",
            self.record.need.what,
            span(self.since.elapsed())
        )
    }
}

/// Why [`admit`] admitted nothing.
#[derive(Debug)]
pub enum Error {
    /// The need can never fit on this machine ([`never`]).
    Never(String),
    /// The ledger could not be used.
    Ledger(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Never(why) => write!(f, "{why}; it can never be admitted here"),
            Error::Ledger(e) => write!(f, "the capacity ledger could not be used: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// Admit `need` in this user's ledger on the live machine, saying on stderr what it waits for ([`admit_with`]).
///
/// A process that already holds claims and needs another enters a [`Waiter`] and polls it
/// instead, ending held work while it waits: this call counts the caller's own held claims
/// against the new one, and they are let go only after it returns.
///
/// # Errors
/// [`Error::Never`] for a need that can never fit; [`Error::Ledger`] when there is no home or the ledger cannot be written.
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
    let at = need.path.clone().or_else(|| std::env::current_dir().ok());
    let reading = machine.read(at.as_deref());
    let refused = never(&need, &reading);
    if mode == Admit::Wait
        && let Some(why) = refused
    {
        return Err(Error::Never(why));
    }
    let what = need.what.clone();
    let mut waiter = Waiter::enter(ledger, machine, need).map_err(Error::Ledger)?;
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
    loop {
        match waiter.poll(machine) {
            Polled::Admitted(claim) => {
                if said.is_some() {
                    say(&format!(
                        "capacity: {what} admitted after {}",
                        span(claim.waited())
                    ));
                }
                return Ok(claim);
            }
            Polled::Waiting(w, short) if matches!(short.shorts[..], [Short::Turn { .. }]) => {
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
                std::thread::sleep(pace.poll.min(TURN_GAP));
            }
            Polled::Waiting(w, short) => {
                turn_since = None;
                if said.is_none_or(|t| t.elapsed() >= pace.line_every) {
                    say(&w.line(&short));
                    said = Some(Instant::now());
                }
                waiter = w;
                std::thread::sleep(pace.poll);
            }
        }
    }
}
