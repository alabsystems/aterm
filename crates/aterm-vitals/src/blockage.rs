// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Blockage the Processes row reads (stats deck A4 §6.5): unreaped zombies by parent,
//! machine-wide, and threads held in an uninterruptible wait.
//!
//! - **Zombies** come from the process sweep every whole reading already takes (each process's
//!   status and parent, other users' included), grouped by parent in the model's [`ZombieBook`].
//!   Jobs reads immutable [`ZombieFacts`] from that same observation, without a second history
//!   or a different aging cutoff. A zombie counts once it has been one for
//!   [`ZOMBIE_AGE_MS`], so a child its parent is about to reap counts nowhere. No tab filter:
//!   another app's pile is the machine's (§6.4).
//! - **Uninterruptible waits**: on macOS, every live process's BSD status reads running, so only
//!   its threads say it, and other users' threads are refused: the host's own-thread census
//!   (`aterm_sysprobe::dstate`) reads this user's processes on its own worker every
//!   [`CENSUS_PERIOD_MS`], and a process is held once two passes running found it so. On Linux
//!   the sweep's own state `D` covers every user, held once seen for [`KERNEL_WAIT_MS`], and
//!   `/proc/stat`'s `procs_blocked` counts the machine's tasks waiting on I/O now (the kernel's
//!   `nr_iowait()`, which leaves out a task in `D` on a lock or a mount outside I/O wait).
//! - **What blocks at one reading** is one rule, [`Blocked::judge`], over those two and the jobs
//!   engine's stuck work ([`StuckWork`]): the worst [`Blockage`] that holds, which the strip's
//!   blockage ring keeps as each box's share and worst kind ([`BlockageBox`], A4 §6.2's cap).
//!   It judges a reading, with no hold: the episodes with their holds are the event detector's.

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

/// How old a zombie sweep, a census pass or a jobs pass may be and still say what blocks now
/// ([`Blocked::judge`]): the census's own bound.
pub const BLOCKAGE_STALE_MS: u64 = CENSUS_STALE_MS;

/// How long a pile has stood, and within how long it gained a zombie, while it reads as growing:
/// a minute (A4 §6.4: "≥ 1/min for a minute").
pub const PILE_GROWS_MS: u64 = 60_000;

/// Every zombie the sweeps have seen: when each was first seen and under which parent, when
/// each parent's pile began, and which left a standing pile while one first seen as it left
/// would still count as new. The model notes every whole reading; consumers share its immutable
/// observation facts rather than keeping another history at their own cadence.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ZombieBook {
    seen: HashMap<u32, (u64, u32)>,
    parents: HashMap<u32, u64>,
    /// Zombies a sweep within [`ZOMBIE_AGE_MS`] + [`PILE_GROWS_MS`] found gone (reaped, or
    /// under another parent) from a pile that still stands: `(parent, first seen, found gone)`,
    /// in order.
    gone: Vec<(u32, u64, u64)>,
}

impl ZombieBook {
    /// Notes one sweep's zombies, each `(pid, parent)`, at `at_ms`: one gone or under another
    /// parent is forgotten, and a parent with none left loses its pile's start.
    pub fn note(&mut self, at_ms: u64, zombies: impl IntoIterator<Item = (u32, u32)>) {
        let live: HashMap<u32, u32> = zombies.into_iter().collect();
        let gone = &mut self.gone;
        gone.retain(|&(_, _, left)| at_ms.saturating_sub(left) < ZOMBIE_AGE_MS + PILE_GROWS_MS);
        self.seen.retain(|pid, (first, ppid)| {
            let stays = live.get(pid).is_some_and(|pp| pp == ppid);
            if !stays {
                gone.push((*ppid, *first, at_ms));
            }
            stays
        });
        for (&pid, &ppid) in &live {
            self.seen.entry(pid).or_insert((at_ms, ppid));
            self.parents.entry(ppid).or_insert(at_ms);
        }
        let parents: HashSet<u32> = live.values().copied().collect();
        self.parents.retain(|p, _| parents.contains(p));
        self.gone.retain(|(p, ..)| parents.contains(p));
        self.gone.sort_unstable();
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

    /// Whether `ppid`'s pile grows at `at_ms`: its first zombie came to count at least
    /// [`PILE_GROWS_MS`] ago (each counts once seen [`ZOMBIE_AGE_MS`]), and the zombies that
    /// came to count within that time outnumber those it counted before it that have gone. One
    /// that went weighs against the pile for as long as a zombie first seen as it left would be
    /// among the new ([`ZOMBIE_AGE_MS`] + [`PILE_GROWS_MS`]): a zombie counts late and leaves at
    /// once, so without that a pile whose parent reaps one child as the next exits would dip
    /// and read as grown a minute on. A pile found whole and never added to stands steady,
    /// whatever its size, and so does one its parent reaps as fast as it fills, or faster; one
    /// whose parent left it empty at a sweep starts again.
    #[must_use]
    pub fn grows(&self, ppid: u32, at_ms: u64) -> bool {
        let counted_for = |first: u64| at_ms.saturating_sub(first).checked_sub(ZOMBIE_AGE_MS);
        let stood = self
            .pile_since(ppid)
            .and_then(counted_for)
            .is_some_and(|stood| stood >= PILE_GROWS_MS);
        let came = self
            .seen
            .values()
            .filter(|&&(_, parent)| parent == ppid)
            .filter_map(|&(first, _)| counted_for(first))
            .filter(|&counted| counted < PILE_GROWS_MS)
            .count();
        let went = self
            .gone
            .iter()
            .filter(|&&(parent, first, left)| {
                parent == ppid
                    && at_ms.saturating_sub(left) < ZOMBIE_AGE_MS + PILE_GROWS_MS
                    && counted_for(first).is_some_and(|counted| counted >= PILE_GROWS_MS)
            })
            .count();
        stood && came > went
    }
}

/// New zombies a minute, × 10, of a pile of `count` begun `elapsed_ms` ago, over at least a
/// minute: a pile found whole at the first sweep reads its count a minute, not a burst.
#[must_use]
pub fn per_min_deci(count: u32, elapsed_ms: u64) -> u64 {
    u64::from(count) * 600_000 / elapsed_ms.max(60_000)
}

/// One canonical aged parent pile at a model observation, independent of display limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ZombiePile {
    parent: u32,
    count: u32,
    since_ms: u64,
    elapsed_ms: u64,
    per_min_deci: u64,
    growing: bool,
}

impl ZombiePile {
    /// The parent retaining these zombies.
    #[must_use]
    pub const fn parent(&self) -> u32 {
        self.parent
    }

    /// All of this parent's zombies aged at the canonical observation.
    #[must_use]
    pub const fn count(&self) -> u32 {
        self.count
    }

    /// When this parent's standing pile began, on [`Sample::mono_ms`]'s clock.
    #[must_use]
    pub const fn since_ms(&self) -> u64 {
        self.since_ms
    }

    /// The pile's elapsed time at the canonical observation, never reconstructed from its rate.
    #[must_use]
    pub const fn elapsed_ms(&self) -> u64 {
        self.elapsed_ms
    }

    /// Its rate, evaluated at the same observation as its count.
    #[must_use]
    pub const fn per_min_deci(&self) -> u64 {
        self.per_min_deci
    }

    /// Growth after the model's whole-sweep and cap-recovery qualification.
    #[must_use]
    pub const fn growing(&self) -> bool {
        self.growing
    }
}

/// Immutable facts of a supported current process census, after the sole model book notes it.
/// These contain every aged parent pile, with no display cap and no mutable history.
#[derive(Debug, PartialEq, Eq)]
pub struct ZombieFacts {
    seq: u64,
    at_ns: u64,
    mono_ms: u64,
    whole: bool,
    piles: Vec<ZombiePile>,
}

impl ZombieFacts {
    /// The existing model snapshot sequence that produced these facts.
    #[must_use]
    pub const fn seq(&self) -> u64 {
        self.seq
    }

    /// The original sample's timestamp, not a later assessment's clock.
    #[must_use]
    pub const fn at_ns(&self) -> u64 {
        self.at_ns
    }

    /// The canonical time at which the model judged age and pile rates.
    #[must_use]
    pub const fn mono_ms(&self) -> u64 {
        self.mono_ms
    }

    /// Whether the source process census reached its end without a cap.
    #[must_use]
    pub const fn whole(&self) -> bool {
        self.whole
    }

    /// Every canonical aged parent pile, ordered by parent PID.
    #[must_use]
    pub fn piles(&self) -> &[ZombiePile] {
        &self.piles
    }

    /// A parent's complete observed pile, rather than a job's child-membership subset.
    #[must_use]
    pub fn parent_pile(&self, parent: u32) -> Option<&ZombiePile> {
        self.piles
            .binary_search_by_key(&parent, ZombiePile::parent)
            .ok()
            .map(|index| &self.piles[index])
    }
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
    /// Whether any pile grows ([`ZombieBook::grows`]): the pile column's critical ink (A4 §6.3).
    /// Never from a sweep stopped at its cap, nor until a whole sweep after one has stood
    /// [`ZOMBIE_AGE_MS`] + [`PILE_GROWS_MS`]: the book forgets a zombie a cut sweep did not
    /// reach and dates it anew when the next sees it, which is no growth.
    pub growing: bool,
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
    /// Processes whose thread list was cut at the probe's cap with none of the threads read
    /// uninterruptible: any may hold one past the cut, so [`Self::marked`] names at least the
    /// processes so.
    pub cut_unmarked: u32,
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
    /// Its thread list was cut at the probe's cap: [`Self::uninterruptible`] counts at least
    /// those.
    pub cut: bool,
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
    /// The process has more threads than were read: [`Self::threads`] counts at least those.
    pub threads_cut: bool,
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
    /// Whether no read behind [`Self::procs`] was cut at a cap: `false` where the sweep stopped
    /// at its process cap, or a census pass cut a thread list and found none uninterruptible in
    /// what it read ([`CensusPass::cut_unmarked`]). [`Self::procs`] then names at least the
    /// processes so. A census cover that took more than one pass is told by [`Self::pass`], not
    /// here.
    pub whole: bool,
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

/// What the jobs engine's newest pass found stuck, as the host hands it in on a reading
/// ([`Sample::stuck_work`]): the model reads no job itself (A4 §6.5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StuckWork {
    /// Jobs judged stuck (φ ≥ 8, [`crate::jobs::Verdict::Stuck`]).
    pub jobs: u32,
    /// Orphans burning a core with no writes ([`crate::jobs::Findings::busy_orphans`]).
    pub busy_orphans: u32,
    /// How long before the reading the pass ran, ms.
    pub age_ms: u64,
}

impl StuckWork {
    /// What `view`, a pass `age_ms` old, found stuck.
    #[must_use]
    pub fn of(view: &crate::jobs::JobsView, age_ms: u64) -> Self {
        let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        Self {
            jobs: count(
                view.rows
                    .iter()
                    .filter(|r| r.verdict == crate::jobs::Verdict::Stuck)
                    .count(),
            ),
            busy_orphans: count(view.findings.busy_orphans.len()),
            age_ms,
        }
    }

    /// The stuck jobs and busy orphans together.
    #[must_use]
    pub const fn count(&self) -> u32 {
        self.jobs.saturating_add(self.busy_orphans)
    }
}

/// What blocks the processes' flow, least to worst (A4 §6.2's cap): the warn kinds in the
/// well's order (⧖ under ☠ under ⊘, §6.4), and the growing pile, the one kind in critical
/// ink, over them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Blockage {
    /// A process held in an uninterruptible wait ([`DStateView::held`]).
    Uninterruptible = 1,
    /// Unreaped zombies in piles that do not grow.
    SteadyPile = 2,
    /// A stuck job or a busy orphan ([`StuckWork`]).
    Stuck = 3,
    /// A zombie pile that grows ([`ZombieBook::grows`]).
    GrowingPile = 4,
}

impl Blockage {
    /// Every kind, least first.
    pub const ALL: [Self; 4] = [
        Self::Uninterruptible,
        Self::SteadyPile,
        Self::Stuck,
        Self::GrowingPile,
    ];

    /// The kind's rank, 1 to 4: what the blockage ring takes as a reading's value (0 is clear).
    #[must_use]
    pub const fn rank(self) -> u8 {
        self as u8
    }

    /// The kind of `rank`; `None` for 0 or any other number.
    #[must_use]
    pub fn of_rank(rank: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.rank() == rank)
    }

    /// The kind's word: `d_state`, `steady_pile`, `stuck`, `growing_pile`. No `sys` view says
    /// it yet.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Uninterruptible => "d_state",
            Self::SteadyPile => "steady_pile",
            Self::Stuck => "stuck",
            Self::GrowingPile => "growing_pile",
        }
    }

    /// Whether the cap draws it in critical ink: the growing pile alone (A4 §6.2).
    #[must_use]
    pub const fn critical(self) -> bool {
        matches!(self, Self::GrowingPile)
    }
}

/// What blocks at one reading.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Blocked {
    /// None of the three readers answered for now: no time of the reading is judged.
    #[default]
    Unread,
    /// At least one answered, and nothing it read blocks.
    Clear,
    /// The worst kind that holds.
    By(Blockage),
}

impl Blocked {
    /// The per-reading rule (A4 §6.2, §6.5): the worst of a zombie pile (growing or steady), stuck
    /// work and a held uninterruptible wait, each from a reading no older than
    /// [`BLOCKAGE_STALE_MS`]. A reader that did not answer, or answered too long ago, says
    /// nothing either way; with none answering the reading is [`Self::Unread`], never clear.
    #[must_use]
    pub fn judge(zombies: &Zombies, dstate: &DState, stuck: Option<&StuckWork>) -> Self {
        let fresh = |age_ms: u64| age_ms <= BLOCKAGE_STALE_MS;
        let pile = match zombies {
            Zombies::Read(v) if fresh(v.age_ms) => Some(v),
            _ => None,
        };
        let waits = match dstate {
            DState::Read(v) if fresh(v.age_ms) => Some(v),
            _ => None,
        };
        let stuck = stuck.filter(|s| fresh(s.age_ms));
        if pile.is_none() && waits.is_none() && stuck.is_none() {
            return Self::Unread;
        }
        let pile = pile.filter(|v| v.count > 0).map(|v| {
            if v.growing {
                Blockage::GrowingPile
            } else {
                Blockage::SteadyPile
            }
        });
        let stuck = stuck.filter(|s| s.count() > 0).map(|_| Blockage::Stuck);
        let waits = waits
            .filter(|v| v.held() > 0)
            .map(|_| Blockage::Uninterruptible);
        pile.max(stuck).max(waits).map_or(Self::Clear, Self::By)
    }

    /// What the blockage ring takes of this reading: the kind's rank (0 where clear) and whether
    /// it counts toward the box's blocked share; `None` where unread.
    #[must_use]
    pub fn reading(self) -> Option<(f64, bool)> {
        match self {
            Self::Unread => None,
            Self::Clear => Some((0.0, false)),
            Self::By(kind) => Some((f64::from(kind.rank()), true)),
        }
    }
}

/// What one strip box of the blockage ring says (A4 §6.2's cap): the ring takes each reading's
/// [`Blocked::reading`], so a box's over time is the share a blockage held and its largest
/// reading the worst kind.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockageBox {
    /// The share of the box's read time a blockage held, 0 to 1: the cap's length.
    pub share: f64,
    /// The worst kind read in the box; `None` where it read clear throughout.
    pub worst: Option<Blockage>,
}

impl BlockageBox {
    /// What `agg`, a bucket of the blockage ring, says: its worst kind is the worst whose rank
    /// its largest reading reaches. `None` for a bucket that read nothing.
    #[must_use]
    pub fn of(agg: &crate::ring::Agg) -> Option<Self> {
        agg.is_sound().then(|| Self {
            share: agg.cell().over_share(),
            worst: Blockage::ALL
                .into_iter()
                .rev()
                .find(|k| agg.max >= f64::from(k.rank())),
        })
    }
}

/// The model's blockage state between readings.
#[derive(Debug, Default)]
pub(crate) struct Book {
    zombies: ZombieBook,
    /// Only this absorb's supported census; an absent input cannot renew an old observation.
    current_zombie_facts: Option<Arc<ZombieFacts>>,
    /// The newest sweep's zombies and when it ran (ms on [`Sample::mono_ms`]'s clock).
    zombies_view: Option<(u64, ZombiesView)>,
    /// Whether the newest sweep stopped at its cap.
    zombies_cut: bool,
    /// From when the zombie book tells growth again after a cut sweep
    /// ([`ZombiesView::growing`]), ms on the same clock.
    zombies_grow_from: u64,
    /// The newest census pass.
    census: Option<Arc<CensusPass>>,
    /// Linux's sweep: when each process (pid and start) was first seen in `D`.
    d_since: HashMap<(u32, Option<u64>), u64>,
    /// Linux's newest sweep's processes in `D`, when it ran, and whether it read the whole
    /// process table.
    d_view: Option<(u64, Vec<DStuck>, bool)>,
}

impl Book {
    /// Takes one reading: its sweep's zombies and `D` states, where it swept (`membership` its
    /// processes' scopes, one per process), and its census pass or `procs_blocked`.
    pub(crate) fn absorb(
        &mut self,
        seq: u64,
        s: &Sample,
        membership: Option<&[Membership]>,
    ) -> (Zombies, DState) {
        let at = s.mono_ms;
        self.current_zombie_facts = None;
        if let Some(procs) = s.procs.as_deref() {
            self.note_sweep(at, procs, membership, &s.tabs, !s.procs_truncated);
            if s.zombie_read == ZombieRead::Sweep {
                self.current_zombie_facts = self.zombie_facts(seq, s).map(Arc::new);
            }
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
                Some((then, procs, whole)) => DState::Read(DStateView {
                    scope: DScope::Machine,
                    procs: procs.clone(),
                    blocked: *blocked,
                    age_ms: at.saturating_sub(*then),
                    whole: *whole,
                    pass: None,
                }),
                None => DState::Unread(Unread::NotSampled),
            },
        };
        (zombies, dstate)
    }

    pub(crate) fn current_zombie_facts(&self) -> Option<Arc<ZombieFacts>> {
        self.current_zombie_facts.as_ref().map(Arc::clone)
    }

    fn zombie_facts(&self, seq: u64, s: &Sample) -> Option<ZombieFacts> {
        let at = s.mono_ms;
        let whole = !s.procs_truncated;
        let judged = whole && at >= self.zombies_grow_from;
        let piles = self
            .zombies
            .piles(at)
            .into_iter()
            .map(|(parent, count)| {
                let since_ms = self.zombies.pile_since(parent)?;
                let elapsed_ms = at.saturating_sub(since_ms);
                Some(ZombiePile {
                    parent,
                    count,
                    since_ms,
                    elapsed_ms,
                    per_min_deci: per_min_deci(count, elapsed_ms),
                    growing: judged && self.zombies.grows(parent, at),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(ZombieFacts {
            seq,
            at_ns: s.at_ns,
            mono_ms: at,
            whole,
            piles,
        })
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
                    threads_cut: m.cut,
                    held: m.held,
                })
                .collect(),
            blocked: None,
            age_ms,
            whole: pass.cut_unmarked == 0,
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
        if !whole {
            self.zombies_cut = true;
        } else if std::mem::take(&mut self.zombies_cut) {
            self.zombies_grow_from = at.saturating_add(ZOMBIE_AGE_MS + PILE_GROWS_MS);
        }
        let judged = whole && at >= self.zombies_grow_from;
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
                growing: judged && list.iter().any(|z| self.zombies.grows(z.ppid, at)),
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
                threads_cut: false,
                held: at.saturating_sub(since) >= KERNEL_WAIT_MS,
            });
        }
        stuck.sort_by_key(|d| d.pid);
        self.d_since = d_since;
        self.d_view = Some((at, stuck, whole));
    }
}

#[cfg(test)]
#[path = "blockage_tests.rs"]
mod tests;
