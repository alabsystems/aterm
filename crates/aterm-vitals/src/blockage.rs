// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Blockage the Processes row reads (stats deck A4 §6.5): unreaped zombies by parent,
//! machine-wide, and threads held in an uninterruptible wait.
//!
//! - **Zombies** come from the process sweep every whole reading already takes (each process's
//!   status and parent, other users' included), grouped by parent in the model's [`ZombieBook`].
//!   The jobs engine keeps a book of the same type, noted only at its own passes (every 2 s or
//!   30 s), so `sys jobs` may name a pile later than this row, or miss a zombie reaped between
//!   its passes. A zombie counts once it has been one for
//!   [`ZOMBIE_AGE_MS`], so a child its parent is about to reap counts nowhere. No tab filter:
//!   another app's pile is the machine's (§6.4).
//! - **Uninterruptible waits**: on macOS, every live process's BSD status reads running, so only
//!   its threads say it, and other users' threads are refused: the host's own-thread census
//!   (`aterm_sysprobe::dstate`) reads this user's processes on its own worker every
//!   [`CENSUS_PERIOD_MS`], and a process is held once two passes running found it so. On Linux
//!   the sweep's own state `D` covers every user, held once seen for [`KERNEL_WAIT_MS`], and
//!   `/proc/stat`'s `procs_blocked` counts the machine's tasks waiting on I/O now (the kernel's
//!   `nr_iowait()`, which leaves out a task in `D` on a lock or a mount outside I/O wait).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use crate::jobs::{KERNEL_WAIT_MS, ZOMBIE_AGE_MS, ZombieParent};
use crate::model::{Membership, ProcSample, ProcState, Sample, TabRoot, Unread};

/// The census's cadence: a pass every 30 s, after a whole reading (A4 §6.5). The host's worker
/// runs at it.
pub const CENSUS_PERIOD_MS: u64 = 30_000;

/// How old the newest census pass may be and still stand for now: three periods. Older, the
/// census reads unread (its worker stopped, or no whole reading asked for a pass), never a stale
/// count.
pub const CENSUS_STALE_MS: u64 = 3 * CENSUS_PERIOD_MS;

/// Every zombie the sweeps have seen: when each was first seen and under which parent, and when
/// each parent's pile began. The Processes row reads the model's, noted at every whole reading;
/// the jobs engine's findings read their own, noted at each of its passes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ZombieBook {
    seen: HashMap<u32, (u64, u32)>,
    parents: HashMap<u32, u64>,
}

impl ZombieBook {
    /// Notes one sweep's zombies, each `(pid, parent)`, at `at_ms`: one gone or under another
    /// parent is forgotten, and a parent with none left loses its pile's start.
    pub fn note(&mut self, at_ms: u64, zombies: impl IntoIterator<Item = (u32, u32)>) {
        let live: HashMap<u32, u32> = zombies.into_iter().collect();
        self.seen
            .retain(|pid, (_, ppid)| live.get(pid).is_some_and(|pp| pp == ppid));
        for (&pid, &ppid) in &live {
            self.seen.entry(pid).or_insert((at_ms, ppid));
            self.parents.entry(ppid).or_insert(at_ms);
        }
        let parents: HashSet<u32> = live.values().copied().collect();
        self.parents.retain(|p, _| parents.contains(p));
    }

    /// When `pid` was first seen a zombie, and its parent.
    #[must_use]
    pub fn seen(&self, pid: u32) -> Option<(u64, u32)> {
        self.seen.get(&pid).copied()
    }

    /// When `ppid`'s pile began.
    #[must_use]
    pub fn pile_since(&self, ppid: u32) -> Option<u64> {
        self.parents.get(&ppid).copied()
    }

    /// Each parent's zombies seen at least [`ZOMBIE_AGE_MS`] by `at_ms`.
    #[must_use]
    pub fn piles(&self, at_ms: u64) -> BTreeMap<u32, u32> {
        let mut by_parent: BTreeMap<u32, u32> = BTreeMap::new();
        for &(first, ppid) in self.seen.values() {
            if at_ms.saturating_sub(first) >= ZOMBIE_AGE_MS {
                *by_parent.entry(ppid).or_default() += 1;
            }
        }
        by_parent
    }
}

/// New zombies a minute, × 10, of a pile of `count` begun `elapsed_ms` ago, over at least a
/// minute: a pile found whole at the first sweep reads its count a minute, not a burst.
#[must_use]
pub fn per_min_deci(count: u32, elapsed_ms: u64) -> u64 {
    u64::from(count) * 600_000 / elapsed_ms.max(60_000)
}

/// Whether the host's process sweep reads each process's state, which the zombie count needs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ZombieRead {
    /// The sweep reads each process's state (macOS, Linux): its zombies are counted.
    #[default]
    Sweep,
    /// The sweep reads no process state (Windows), so it tells no zombie: unread, never 0.
    Unsupported,
}

/// Unreaped zombies at one reading, machine-wide.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Zombies {
    /// The newest sweep's piles.
    Read(ZombiesView),
    /// No sweep has run yet, or this host's sweep reads no process state.
    Unread(Unread),
}

impl Default for Zombies {
    fn default() -> Self {
        Self::Unread(Unread::NotSampled)
    }
}

impl Zombies {
    /// The count, where read.
    #[must_use]
    pub fn count(&self) -> Option<u32> {
        match self {
            Self::Read(v) => Some(v.count),
            Self::Unread(_) => None,
        }
    }
}

/// The newest sweep's zombies, by parent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ZombiesView {
    /// Zombies seen at least [`ZOMBIE_AGE_MS`], every user's: the pile column's count.
    pub count: u32,
    /// Their parents, the largest pile first (lower pid first among equals).
    pub piles: Vec<ZombieParent>,
    /// Whether the sweep read the whole process table: one stopped at its cap counts at least
    /// these.
    pub whole: bool,
    /// How long before this reading its sweep ran, ms: 0 on a reading that swept.
    pub age_ms: u64,
}

/// What the host read of uninterruptible waits at one reading.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum DStateRead {
    /// No reader on this OS.
    #[default]
    Unsupported,
    /// macOS: the own-thread census on its own worker; the pass that ended since the last
    /// reading, where one did.
    Census(Option<Arc<CensusPass>>),
    /// Linux: the sweep's process states (`D`), every user's, and `/proc/stat`'s
    /// `procs_blocked`, the machine's tasks waiting on I/O now, where read.
    Sweep {
        /// `procs_blocked`.
        blocked: Option<u64>,
    },
}

/// One census pass, as the host's worker hands it over (`aterm_sysprobe::dstate::Pass`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CensusPass {
    /// When it ended, ms on [`Sample::mono_ms`]'s clock.
    pub at_mono_ms: u64,
    /// The processes it found holding a thread uninterruptibly, pid ascending.
    pub marked: Vec<CensusMark>,
    /// This user's processes listed.
    pub listed: u32,
    /// Processes it read.
    pub read: u32,
    /// Processes whose threads it could not read.
    pub refused: u32,
    /// Whether it ended a cover: every listed process read since the cover began.
    pub cover_done: bool,
    /// Passes the cover it ended took, where it ended one.
    pub passes_in_cover: u32,
    /// Its thread time, ns.
    pub thread_ns: u64,
    /// Whether the list of this user's processes was refused (nothing read).
    pub list_refused: bool,
}

/// One process a census pass found holding a thread uninterruptibly.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CensusMark {
    /// Its pid.
    pub pid: u32,
    /// Its name.
    pub name: String,
    /// Its threads so.
    pub uninterruptible: u32,
    /// The pass before found the same process so too: held for two passes running (≥ 30 s).
    pub held: bool,
}

/// Whose processes an uninterruptible count covers: the word `sys` says as `dstate_scope`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DScope {
    /// This user's (macOS: other users' threads are refused).
    Own,
    /// Every user's (Linux's sweep).
    Machine,
}

impl DScope {
    /// The word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Own => "own",
            Self::Machine => "machine",
        }
    }
}

/// A process found in an uninterruptible wait.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DStuck {
    /// Its pid.
    pub pid: u32,
    /// Its name.
    pub name: String,
    /// Its threads so, where its threads were read (the census); `None` where the sweep read the
    /// process's state alone.
    pub threads: Option<u32>,
    /// Held: two census passes running, or the sweep's state for at least [`KERNEL_WAIT_MS`].
    pub held: bool,
}

/// Uninterruptible waits at one reading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DStateView {
    /// Whose processes it covers.
    pub scope: DScope,
    /// The processes found so, pid ascending.
    pub procs: Vec<DStuck>,
    /// Linux: the machine's tasks waiting on I/O now (`procs_blocked`); `None` elsewhere or unread.
    pub blocked: Option<u64>,
    /// How long before this reading the pass or sweep behind [`Self::procs`] ran, ms.
    pub age_ms: u64,
    /// The census pass behind it, with its cost; `None` off macOS.
    pub pass: Option<Arc<CensusPass>>,
}

impl DStateView {
    /// The processes held.
    #[must_use]
    pub fn held(&self) -> usize {
        self.procs.iter().filter(|p| p.held).count()
    }
}

/// Uninterruptible waits at one reading, or why unread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DState {
    /// Read.
    Read(DStateView),
    /// Not read, and why.
    Unread(Unread),
}

impl Default for DState {
    fn default() -> Self {
        Self::Unread(Unread::NotSampled)
    }
}

/// The model's blockage state between readings.
#[derive(Debug, Default)]
pub(crate) struct Book {
    zombies: ZombieBook,
    /// The newest sweep's zombies and when it ran (ms on [`Sample::mono_ms`]'s clock).
    zombies_view: Option<(u64, ZombiesView)>,
    /// The newest census pass.
    census: Option<Arc<CensusPass>>,
    /// Linux's sweep: when each process (pid and start) was first seen in `D`.
    d_since: HashMap<(u32, Option<u64>), u64>,
    /// Linux's newest sweep's processes in `D`, and when it ran.
    d_view: Option<(u64, Vec<DStuck>)>,
}

impl Book {
    /// Takes one reading: its sweep's zombies and `D` states, where it swept (`membership` its
    /// processes' scopes, one per process), and its census pass or `procs_blocked`.
    pub(crate) fn absorb(
        &mut self,
        s: &Sample,
        membership: Option<&[Membership]>,
    ) -> (Zombies, DState) {
        let at = s.mono_ms;
        if let Some(procs) = s.procs.as_deref() {
            self.note_sweep(at, procs, membership, &s.tabs, !s.procs_truncated);
        }
        let zombies = match s.zombie_read {
            ZombieRead::Unsupported => Zombies::Unread(Unread::Unsupported),
            ZombieRead::Sweep => {
                self.zombies_view
                    .as_ref()
                    .map_or_else(Zombies::default, |(then, v)| {
                        Zombies::Read(ZombiesView {
                            age_ms: at.saturating_sub(*then),
                            ..v.clone()
                        })
                    })
            }
        };
        let dstate = match &s.dstate {
            DStateRead::Unsupported => DState::Unread(Unread::Unsupported),
            DStateRead::Census(pass) => {
                if let Some(p) = pass {
                    self.census = Some(Arc::clone(p));
                }
                self.census_view(at)
            }
            // Until a sweep ran, no process is known either way: unread, `procs_blocked` or not.
            DStateRead::Sweep { blocked } => match &self.d_view {
                Some((then, procs)) => DState::Read(DStateView {
                    scope: DScope::Machine,
                    procs: procs.clone(),
                    blocked: *blocked,
                    age_ms: at.saturating_sub(*then),
                    pass: None,
                }),
                None => DState::Unread(Unread::NotSampled),
            },
        };
        (zombies, dstate)
    }

    fn census_view(&self, at: u64) -> DState {
        let Some(pass) = self.census.as_ref() else {
            return DState::Unread(Unread::NotSampled);
        };
        let age_ms = at.saturating_sub(pass.at_mono_ms);
        if pass.list_refused {
            return DState::Unread(Unread::Refused);
        }
        if age_ms > CENSUS_STALE_MS {
            return DState::Unread(Unread::NotSampled);
        }
        DState::Read(DStateView {
            scope: DScope::Own,
            procs: pass
                .marked
                .iter()
                .map(|m| DStuck {
                    pid: m.pid,
                    name: m.name.clone(),
                    threads: Some(m.uninterruptible),
                    held: m.held,
                })
                .collect(),
            blocked: None,
            age_ms,
            pass: Some(Arc::clone(pass)),
        })
    }

    fn note_sweep(
        &mut self,
        at: u64,
        procs: &[ProcSample],
        membership: Option<&[Membership]>,
        tabs: &[TabRoot],
        whole: bool,
    ) {
        self.zombies.note(
            at,
            procs
                .iter()
                .filter(|p| p.state == ProcState::Zombie)
                .map(|p| (p.pid, p.ppid)),
        );
        let piles = self.zombies.piles(at);
        let mut named: HashMap<u32, (&str, Option<u64>)> = HashMap::with_capacity(piles.len());
        if !piles.is_empty() {
            for (i, p) in procs.iter().enumerate() {
                if piles.contains_key(&p.pid) {
                    let tab = membership
                        .and_then(|m| m.get(i))
                        .and_then(|m| m.tab)
                        .and_then(|t| tabs.get(t))
                        .map(|t| t.key);
                    named.insert(p.pid, (p.name.as_str(), tab));
                }
            }
        }
        let mut list: Vec<ZombieParent> = piles
            .into_iter()
            .map(|(ppid, count)| {
                let (name, tab) = named.get(&ppid).copied().unwrap_or(("", None));
                let since = self.zombies.pile_since(ppid).unwrap_or(at);
                ZombieParent {
                    ppid,
                    name: name.to_owned(),
                    count,
                    per_min_deci: per_min_deci(count, at.saturating_sub(since)),
                    tab,
                }
            })
            .collect();
        list.sort_by_key(|z| (std::cmp::Reverse(z.count), z.ppid));
        self.zombies_view = Some((
            at,
            ZombiesView {
                count: list.iter().map(|z| z.count).sum(),
                piles: list,
                whole,
                age_ms: 0,
            },
        ));

        let mut d_since = HashMap::new();
        let mut stuck = Vec::new();
        for p in procs
            .iter()
            .filter(|p| p.state == ProcState::Uninterruptible)
        {
            let key = (p.pid, p.start);
            let since = self.d_since.get(&key).copied().unwrap_or(at);
            d_since.insert(key, since);
            stuck.push(DStuck {
                pid: p.pid,
                name: p.name.clone(),
                threads: None,
                held: at.saturating_sub(since) >= KERNEL_WAIT_MS,
            });
        }
        stuck.sort_by_key(|d| d.pid);
        self.d_since = d_since;
        self.d_view = Some((at, stuck));
    }
}

#[cfg(test)]
#[path = "blockage_tests.rs"]
mod tests;
