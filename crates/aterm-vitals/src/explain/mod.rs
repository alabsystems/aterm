// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Who uses the machine's CPU, read per resource coalition: the facts the cause engine and `sys
//! why` read. RD is `docs/DESIGN-resource-details-2026-10-20.md`, SA
//! `docs/DESIGN-system-awareness-2026-10-16.md`.

use std::hash::{BuildHasherDefault, Hasher};
use std::sync::Arc;

use crate::model::{Membership, Sample, Snapshot, TabRoot};

pub mod account;
pub mod detect;
pub mod episodes;
pub mod reap;
pub mod split;

#[cfg(any(test, debug_assertions))]
pub mod fixtures;

pub use account::{
    Account, Bare, Role, Shown, WhySubject, You, account, account_of, bare_subject, you_role,
};
pub use detect::{
    ACTIVITIES, Activity, ActivityId, Candidate, CauseId, Context, Evidence, Term, WakeFact, detect,
};
pub use episodes::{ClauseHold, DeckCause, Episodes, OpenCause, deck_cause};
pub use reap::{ReapLedger, ReapTotals, Scope};
pub use split::{CoalBase, SplitIn, WINDOW_MAX_NS, split};

/// A map keyed by the kernel's integer ids (pids, coalition ids, a pid and
/// its birth), hashed by [`IdHasher`].
pub type IdMap<K, V> = std::collections::HashMap<K, V, BuildHasherDefault<IdHasher>>;

/// A set of the kernel's integer ids, hashed by [`IdHasher`].
pub type IdSet<K> = std::collections::HashSet<K, BuildHasherDefault<IdHasher>>;

/// The split's hasher for integer ids (FxHash's multiply-rotate, rustc's): a split tick hashes ids
/// some ten thousand times (700 processes, 650 coalitions), where std's SipHash cost about half the
/// split's own CPU. The ids are the kernel's, which no process picks, so a keyed hash buys nothing.
/// Local, since this crate depends on nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct IdHasher(u64);

impl IdHasher {
    const K: u64 = 0xf135_7aea_2e62_a9c5;

    fn add(&mut self, word: u64) {
        self.0 = self.0.wrapping_add(word).wrapping_mul(Self::K);
    }
}

impl Hasher for IdHasher {
    fn write(&mut self, bytes: &[u8]) {
        let (words, tail) = bytes.as_chunks::<8>();
        for w in words {
            self.add(u64::from_le_bytes(*w));
        }
        let mut last = [0u8; 8];
        last[..tail.len()].copy_from_slice(tail);
        self.add(u64::from_le_bytes(last) ^ ((tail.len() as u64) << 56));
    }

    fn write_u8(&mut self, n: u8) {
        self.add(u64::from(n));
    }

    fn write_u16(&mut self, n: u16) {
        self.add(u64::from(n));
    }

    fn write_u32(&mut self, n: u32) {
        self.add(u64::from(n));
    }

    fn write_u64(&mut self, n: u64) {
        self.add(n);
    }

    fn write_usize(&mut self, n: usize) {
        self.add(n as u64);
    }

    fn finish(&self) -> u64 {
        // The multiply leaves the low bits, which pick the bucket, the
        // weakest: rotate the strong high bits down (rustc-hash 2's finish).
        self.0.rotate_left(26)
    }
}

/// Whether the coalition lane reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CoalLane {
    /// This OS has no coalitions (Linux, Windows): the split is by subtraction.
    #[default]
    Unsupported,
    /// macOS answered the launch self-check: the split is by coalition.
    On,
    /// macOS's self-check missed, or a read did not match the record layout the readers hold: the
    /// split is by subtraction.
    Off,
}

impl CoalLane {
    /// Whether the split is by coalition.
    #[must_use]
    pub fn on(self) -> bool {
        self == Self::On
    }

    /// The unavailable word a reply prints for the coalitions (`lane_off:coalitions`, the other
    /// views' `lane_off:<lane>` form) wherever the lane does not read; `None` while it reads.
    #[must_use]
    pub fn word(self) -> Option<&'static str> {
        (!self.on()).then_some("lane_off:coalitions")
    }
}

/// The thread QoS classes a coalition's CPU time is split by.
pub const QOS_CLASSES: usize = 7;

/// One resource coalition's cumulative counters, as the host read them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoalUsage {
    /// Its id.
    pub id: u64,
    /// Tasks that joined it since it was made.
    pub tasks_started: u64,
    /// Tasks of it that have exited.
    pub tasks_exited: u64,
    /// Its members' CPU time, exited members' included, ns.
    pub cpu_ns: u64,
    /// Bytes its members read from storage, exited members' included.
    pub bytes_read: u64,
    /// [`Self::cpu_ns`] by thread QoS, ns: unspecified, maintenance, background, utility, legacy,
    /// user-initiated, user-interactive.
    pub eqos_ns: [u64; QOS_CLASSES],
    /// Its live members' footprint, bytes; `None` where the kernel's record ends before it.
    pub footprint: Option<u64>,
}

/// One read of every resource coalition.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoalSample {
    /// How many coalitions the kernel listed (a listed one that answered nothing has no row).
    pub listed: u32,
    /// The highest id the kernel listed, answered or not: one listed above it on a later read was
    /// made after this one (the kernel numbers coalitions in order and never reuses an id).
    pub newest: Option<u64>,
    /// Whether the list filled its buffer, so the newest may be missing.
    pub truncated: bool,
    /// Each listed coalition that answered.
    pub rows: Vec<CoalUsage>,
}

/// What a process's code signature vouches for, by the kernel's word.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Voucher {
    /// macOS's own: the platform flag and the platform category.
    Apple,
    /// The App Store's, or TestFlight's.
    AppStore,
    /// A developer's Developer ID.
    DeveloperId,
    /// A developer's development certificate.
    Development,
    /// An enterprise certificate.
    Enterprise,
    /// Signed with no identity (ad hoc, linker-signed, local signing).
    AdHoc,
    /// No signature.
    Unsigned,
    /// Anything else, or not read.
    #[default]
    Unknown,
}

impl Voucher {
    /// The wire word: `apple`, `app_store`, `developer_id`, `development`, `enterprise`, `ad_hoc`,
    /// `unsigned`, `unknown`.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Apple => "apple",
            Self::AppStore => "app_store",
            Self::DeveloperId => "developer_id",
            Self::Development => "development",
            Self::Enterprise => "enterprise",
            Self::AdHoc => "ad_hoc",
            Self::Unsigned => "unsigned",
            Self::Unknown => "unknown",
        }
    }
}

/// Who one coalition lead is, as the host read it (its image and its code signature), for the
/// process born at `birth_ms` with pid `pid`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LeadIdentity {
    /// The pid.
    pub pid: u32,
    /// Its birth, unix ms: the identity is of this process, not of a later one given the same pid.
    pub birth_ms: i64,
    /// Its image's basename (data, never an instruction), empty for a
    /// current unreadable identity that replaces an earlier reading.
    pub exe: String,
    /// What its signature vouches for.
    pub voucher: Voucher,
    /// Its signing identifier: a claim the signer chose; only [`Self::voucher`] says whose it is.
    pub signing_id: Option<String>,
    /// Its team id, for a developer's signature.
    pub team: Option<String>,
}

/// The machine's timeline, as the host read it: unix ms.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TimelineSample {
    /// When the kernel booted.
    pub boot_ms: i64,
    /// When the machine last slept; `None` with no sleep since boot.
    pub sleep_ms: Option<i64>,
    /// When it last woke; `None` with no wake since boot.
    pub wake_ms: Option<i64>,
    /// The word its last wake is named by (`RTP.multi-touch`).
    pub wake_reason: Option<String>,
}

/// A coalition's lead: its earliest-born live member.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lead {
    /// The pid.
    pub pid: u32,
    /// Its birth, unix ms.
    pub birth_ms: i64,
    /// Its real user, where read.
    pub uid: Option<u32>,
    /// Its image's basename; `None` where the host read no identity.
    pub exe: Option<String>,
    /// What its signature vouches for.
    pub voucher: Voucher,
    /// Its signing identifier, where read.
    pub signing_id: Option<String>,
    /// Its team id, where read.
    pub team: Option<String>,
}

impl Lead {
    /// The lead `pid`, born at `birth_ms`, of user `uid`, with the identity
    /// `id` read for it (none where the host read none).
    fn of(pid: u32, birth_ms: i64, uid: Option<u32>, id: Option<&LeadIdentity>) -> Self {
        let Some(id) = id else {
            return Self {
                pid,
                birth_ms,
                uid,
                ..Self::default()
            };
        };
        Self {
            pid,
            birth_ms,
            uid,
            exe: (!id.exe.is_empty()).then(|| id.exe.clone()),
            voucher: id.voucher,
            signing_id: id.signing_id.clone(),
            team: id.team.clone(),
        }
    }

    /// Whether this is [`Self::of`] the same arguments, field for field,
    /// without copying a word.
    fn is(&self, pid: u32, birth_ms: i64, uid: Option<u32>, id: Option<&LeadIdentity>) -> bool {
        let Self {
            pid: p,
            birth_ms: b,
            uid: u,
            exe,
            voucher,
            signing_id,
            team,
        } = self;
        let none = LeadIdentity::default();
        let id = id.unwrap_or(&none);
        (*p, *b, *u) == (pid, birth_ms, uid)
            && exe.as_deref() == (!id.exe.is_empty()).then_some(id.exe.as_str())
            && *voucher == id.voucher
            && *signing_id == id.signing_id
            && *team == id.team
    }

    /// Its team id where it reads as one (ten capitals or digits): the only
    /// form `team=` and the lens's chip print.
    #[must_use]
    pub fn team_id(&self) -> Option<&str> {
        self.team.as_deref().filter(|t| {
            t.len() == 10
                && t.bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        })
    }
}

/// Where a coalition's work counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Part {
    /// aterm's: it holds this instance, or a tab's shell (`C(I)`).
    Aterm,
    /// The user's other work: it holds a live process of this user.
    Others,
    /// Everything else: other users' work and the kernel's.
    #[default]
    Services,
}

impl Part {
    /// The word: `aterm`, `others`, `services`.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Aterm => "aterm",
            Self::Others => "others",
            Self::Services => "services",
        }
    }
}

/// A coalition as last seen with a live member: one with no member left keeps its part and its
/// name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Named {
    /// Where its work counts.
    pub part: Part,
    /// Its lead; `None` when no live member's birth was read.
    pub lead: Option<Lead>,
}

/// How a split was made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Via {
    /// Per coalition: every part is its coalitions' counters.
    Coalitions,
    /// By subtraction: services is the busy figure less aterm and others.
    #[default]
    Subtraction,
}

impl Via {
    /// The word: `coalitions`, `subtraction`.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Coalitions => "coalitions",
            Self::Subtraction => "subtraction",
        }
    }
}

/// One listed coalition over the window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoalRow {
    /// Its id.
    pub id: u64,
    /// Where its work counts.
    pub part: Part,
    /// Its CPU over the window, millicores (from 0 for one made inside it; 0 for one not
    /// [`Self::measured`]).
    pub cpu_mc: i64,
    /// Whether its counters were read at both ends of the window (or it was made inside it).
    /// `false` for one the base should have held and did not, or whose counter went back: its
    /// figures over the window read 0, and the residual holds its work.
    pub measured: bool,
    /// Of that, a delegated job's members' work, counted in their tab instead, millicores (never
    /// above [`Self::cpu_mc`]).
    pub delegated_mc: i64,
    /// Of [`Self::cpu_mc`], what ran at maintenance or background QoS (deferred work), millicores.
    pub deferred_mc: i64,
    /// Its seven QoS buckets' growth over the window, millicores: what [`Self::deferred_mc`] is a
    /// share of.
    pub qos_mc: i64,
    /// Bytes per second it read from storage over the window.
    pub read_bps: u64,
    /// Its live members' footprint now, bytes, where read.
    pub footprint: Option<u64>,
    /// Its live members in this tick's sweep.
    pub live: u32,
    /// The tabs its live members are in, by key, sorted, when it has live members and all are in
    /// tabs; `None` otherwise. A caller scoped to a tab sees the row only when they are all that
    /// tab's (never aterm's own coalition: aterm is in no tab).
    pub tabs: Option<Vec<u64>>,
    /// Whether members of it have exited, so its counters include work of processes now gone.
    pub finished: bool,
    /// Its lead, or the one it had when a member was last seen.
    pub lead: Option<Lead>,
}

impl CoalRow {
    /// Its deferred share, per mille of its QoS buckets ([`Self::qos_mc`]), never past 1000; `None`
    /// where the buckets did not grow.
    #[must_use]
    pub fn deferred_pm(&self) -> Option<u16> {
        deferred_pm(self.deferred_mc, self.qos_mc)
    }
}

/// `deferred` of `qos` millicores, per mille, rounded and never past 1000; `None` for no QoS time.
#[must_use]
pub fn deferred_pm(deferred: i64, qos: i64) -> Option<u16> {
    (qos > 0).then(|| {
        let pm =
            (i128::from(deferred.clamp(0, qos)) * 1000 + i128::from(qos) / 2) / i128::from(qos);
        u16::try_from(pm.min(1000)).unwrap_or(1000)
    })
}

/// The busy CPU over one matched window, millicores: every figure is over the same window, and
/// `global = aterm + delegated + others + services + unaccounted` exactly.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoalSplit {
    /// How it was made.
    pub via: Via,
    /// Whether every coalition was read and every process swept at both ends of the window, so
    /// every row is measured (`exact`), or not (`heuristic`, as every split by subtraction is).
    pub exact: bool,
    /// Whether the kernel's list of coalitions stopped short at either end of the window
    /// (`cannot what=coalition_list why=truncated`).
    pub list_truncated: bool,
    /// Coalitions the kernel listed (0 by subtraction).
    pub listed: u32,
    /// The machine's busy figure (the cores' own ticks).
    pub global_mc: i64,
    /// aterm's coalitions.
    pub aterm_mc: i64,
    /// A delegated job's members' work under launchers outside aterm, moved to aterm's side (0 by
    /// subtraction, where aterm already holds it).
    pub delegated_mc: i64,
    /// The user's other coalitions, less the delegated work.
    pub others_mc: i64,
    /// Every other coalition, less the delegated work; by subtraction, the busy figure less aterm
    /// and others, never below 0.
    pub services_mc: i64,
    /// What no part holds, signed and never clamped.
    pub unaccounted_mc: i64,
    /// Each tab's work over the window, by its key ([`TabRoot::tab`]; a tab split into panes is
    /// one), in the host's order: where the reap credit's ledger read both ends of the window
    /// ([`reap`]), its members' own CPU and their reaped children's (finished work included),
    /// else its processes alive at both ends of it; delegated members' live work included.
    pub tabs: Vec<(u64, i64)>,
    /// The same over the window for aterm's processes in no tab (itself, its helpers, what
    /// closed tabs left, and a delegated job's members whose tab closed), so of
    /// [`Self::aterm_mc`] plus [`Self::delegated_mc`], not of aterm's coalitions alone.
    pub own_mc: i64,
    /// [`Self::aterm_mc`] plus [`Self::delegated_mc`] less every figure
    /// ([`Self::own_mc`] and [`Self::tabs`]), never below 0: work in no
    /// figure, so in no tab's: with the reap credit, a process that left for
    /// launchd before a sweep read it, a member's tail before launchd reaped
    /// it, the counters' skew; without it, every process not alive at both
    /// ends of the window. Taken from the parts' nanoseconds and rounded
    /// once, so the parts' own rounding never reads as a millicore of it; 0
    /// where aterm's coalitions held only the figures' work
    /// ([`Self::skew_mc`]).
    pub unattributed_mc: i64,
    /// What aterm's coalitions' counters ran past their members' figures
    /// over a window in which none of them made or reaped a member (the
    /// kernel's task counters at both ends) and the sweep found every live
    /// member (as many as the counters hold) in a figure that read it at
    /// both ends of the window: no process came or went, so none of it is
    /// finished work, only the skew between a coalition's counter and its
    /// members' times, which the headless run read at 2.70–3.78 ms, each
    /// giving back what the counter fell behind in the windows before (RD
    /// §3.5, §7.3). Rounded once from ns, as [`Self::unattributed_mc`]
    /// (which reads 0 then); 0 in any other window, and by subtraction.
    pub skew_mc: i64,
    /// The tabs, by key, whose figure the reap credit's ledger knows is short of their work over
    /// the window, so only a lower bound however little is [`Self::unattributed_mc`] ([`reap`]'s
    /// missed steps): a member's tail where launchd reaped it, work taken out against members
    /// gone with their parent that the reaper's reaped time fell short of, a member of aterm's
    /// back from a delegated job, a process first read since the last reading. None by
    /// subtraction, which reads no finished work.
    pub short_tabs: Vec<u64>,
    /// Every listed coalition that answered, busiest first (none by subtraction), rounded as a set
    /// so they add up to the parts' total (`global_mc - unaccounted_mc`); a part's rows may still
    /// differ from the part, rounded once, by a millicore or two.
    pub rows: Vec<CoalRow>,
}

/// The split's five parts with one tab's work taken out of aterm's, millicores: they add up to
/// [`CoalSplit::global_mc`] exactly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Parts {
    /// The tab's work ([`CoalSplit::tabs`]).
    pub tab: i64,
    /// aterm's coalitions and the delegated work, less the tab's.
    pub aterm_rest: i64,
    /// The user's other work.
    pub others: i64,
    /// Services.
    pub services: i64,
    /// The signed residual: the split's, plus any of the tab's figure past aterm's.
    pub unaccounted: i64,
}

impl CoalSplit {
    /// The parts with tab `tab`'s work (the caller's) out of aterm's: none for no tab or one the
    /// split did not read.
    #[must_use]
    pub fn parts(&self, tab: Option<u64>) -> Parts {
        self.parts_of(tab.as_slice())
    }

    /// [`Self::parts`] for the tab whose split panes are the sessions `panes`.
    #[must_use]
    pub fn parts_of(&self, panes: &[u64]) -> Parts {
        let tab: i64 = self
            .tabs
            .iter()
            .filter(|(k, _)| panes.contains(k))
            .map(|(_, mc)| *mc)
            .sum();
        let rest = self.aterm_mc + self.delegated_mc - tab;
        Parts {
            tab,
            aterm_rest: rest.max(0),
            others: self.others_mc,
            services: self.services_mc,
            unaccounted: self.unaccounted_mc + rest.min(0),
        }
    }
}

/// What one split tick read: the split, its window, and the machine's timeline.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WhyFacts {
    /// The split tick's stamp, ns on the host's monotonic clock.
    pub at_ns: u64,
    /// Its wall clock, unix ms.
    pub at_unix_ms: i64,
    /// The window every figure is over, ns.
    pub window_ns: u64,
    /// Whether the lane read.
    pub lane: CoalLane,
    /// The split.
    pub split: CoalSplit,
    /// Whether the split tick's sweep stopped at its cap, so a tab's figure may fall short.
    pub procs_truncated: bool,
    /// The machine's timeline, where read.
    pub timeline: Option<TimelineSample>,
    /// When userspace started, unix ms: the earliest birth among the live processes above pid 1
    /// (launchd outlives a userspace restart), where births were read and the sweep was whole.
    pub userspace_ms: Option<i64>,
    /// This aterm's user, where read.
    pub self_uid: Option<u32>,
    /// The causes that hold over the window, ranked ([`detect()`]).
    pub causes: Vec<Candidate>,
    /// The episodes open after this tick, ranked the same way: covering causes only, since a
    /// context keeps no episode.
    pub open: Vec<OpenCause>,
    /// The Deck's cause clause after this tick ([`deck_cause`] reads it).
    pub clause: Option<DeckCause>,
}

/// A defect the negative controls (`tests/conformance_scope_balance.rs`,
/// `tests/conformance_explain.rs`) inject into the real split or its cause engine, through
/// [`Vitals::with_fault`](crate::Vitals::with_fault).
#[cfg(any(test, debug_assertions))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fault {
    /// With the lane on, services takes the positive residual, as the split by subtraction does.
    SplitBySubtraction,
    /// The tab's figure is its last sweep's rate (a 1 s window) against the split's window.
    TabOverProcsWindow,
    /// A delegated job's members' work stays in their launcher's part as
    /// well as in their tab, the residual still read off the coalitions'
    /// own total: the shared launcher is charged for the tabs' work.
    ChargeSharedLauncher,
    /// Aterm's part is its live members' work: what members that exited
    /// inside the window ran (`unattributed_mc`) leaves it for services, the
    /// reaper's (SA's `DropReapedChildTime`).
    DropReapedChildTime,
    /// A named activity's members are matched by signing id alone, under any voucher.
    MatchBySigningIdOnly,
    /// The Deck's clause shows and goes with its candidate, with no hold.
    NoHold,
    /// `unexplained` reads only the three parts, so the split's unaccounted residual goes uncovered
    /// however large.
    IgnoreUnaccounted,
    /// Unattributed work up to a fixed [`Fault::TOLERANCE_MC`] is taken
    /// for the counters' skew whatever was read: the fudge the task
    /// counters' rule ([`CoalSplit::skew_mc`]) replaces, which calls a small
    /// exit none (`tests/conformance_explain.rs`).
    FixedTolerance,
}

#[cfg(any(test, debug_assertions))]
impl Fault {
    /// [`Fault::FixedTolerance`]'s fudge, millicores.
    pub const TOLERANCE_MC: i64 = 5;

    /// `facts` with this defect in their split by coalition (`snap` the
    /// snapshot they are of); facts by subtraction as they are.
    fn inject(self, mut facts: WhyFacts, snap: &Snapshot) -> WhyFacts {
        let split = &mut facts.split;
        if split.via != Via::Coalitions {
            return facts;
        }
        match self {
            Self::SplitBySubtraction => {
                split.services_mc += split.unaccounted_mc.max(0);
                split.unaccounted_mc = split.unaccounted_mc.min(0);
            }
            Self::TabOverProcsWindow => {
                split.tabs = snap
                    .tabs
                    .iter()
                    .map(|t| (t.key, millicores(t.scope.cores.raw)))
                    .collect();
            }
            Self::ChargeSharedLauncher => {
                for r in &split.rows {
                    match r.part {
                        Part::Others => split.others_mc += r.delegated_mc,
                        Part::Services => split.services_mc += r.delegated_mc,
                        Part::Aterm => {}
                    }
                }
            }
            Self::DropReapedChildTime => {
                let reaped = split.unattributed_mc.min(split.aterm_mc);
                split.aterm_mc -= reaped;
                split.services_mc += reaped;
            }
            Self::FixedTolerance => {
                if split.unattributed_mc <= Self::TOLERANCE_MC {
                    split.unattributed_mc = 0;
                }
            }
            Self::MatchBySigningIdOnly | Self::NoHold | Self::IgnoreUnaccounted => {}
        }
        facts
    }
}

/// Each coalition's lead among `members` (`(pid, coalition, birth_ms)` of every live process whose
/// coalition and birth were read): its earliest-born member, ties to the lower pid, never pid 0.
#[must_use]
pub fn leads(members: &[(u32, u64, i64)]) -> Vec<(u64, u32)> {
    let mut first: IdMap<u64, (i64, u32)> = IdMap::default();
    for &(pid, coalition, birth) in members.iter().filter(|m| m.0 != 0) {
        first
            .entry(coalition)
            .and_modify(|best| *best = (*best).min((birth, pid)))
            .or_insert((birth, pid));
    }
    let mut out: Vec<(u64, u32)> = first.into_iter().map(|(c, (_, pid))| (c, pid)).collect();
    out.sort_unstable();
    out
}

/// Names `read`'s coalitions in `names` from this tick's sweep `procs` and returns each
/// coalition's live member count. Each one with a live member takes its part (aterm's when it
/// holds this instance, `self_pid`, or a tab's shell, so a tab adopted from a predecessor counts;
/// others' when it holds a process of user `self_uid`; else services) and its lead, with the
/// identity `identities` hold for that pid and birth.
pub fn name(
    names: &mut IdMap<u64, Named>,
    read: &CoalSample,
    procs: &[crate::model::ProcSample],
    self_pid: u32,
    self_uid: u32,
    tabs: &[TabRoot],
    identities: &IdMap<(u32, i64), LeadIdentity>,
) -> IdMap<u64, u32> {
    let roots: IdSet<u32> = std::iter::once(self_pid)
        .chain(tabs.iter().map(|t| t.shell_pid).filter(|p| *p != 0))
        .collect();
    let listed: IdSet<u64> = read.rows.iter().map(|r| r.id).collect();
    let mut live: IdMap<u64, u32> = IdMap::default();
    let mut parts: IdMap<u64, Part> = IdMap::default();
    let mut members = Vec::new();
    let mut born: IdMap<u32, (i64, Option<u32>)> = IdMap::default();
    for p in procs {
        let Some(c) = p.coalition else { continue };
        *live.entry(c).or_default() += 1;
        let part = if roots.contains(&p.pid) {
            Part::Aterm
        } else if p.uid == Some(self_uid) {
            Part::Others
        } else {
            Part::Services
        };
        let best = parts.entry(c).or_insert(part);
        if part == Part::Aterm || (part == Part::Others && *best == Part::Services) {
            *best = part;
        }
        if let Some(birth) = p.birth_ms {
            members.push((p.pid, c, birth));
            born.insert(p.pid, (birth, p.uid));
        }
    }
    names.retain(|id, _| listed.contains(id));
    for (c, pid) in leads(&members) {
        let Some(part) = parts.remove(&c).filter(|_| listed.contains(&c)) else {
            continue;
        };
        let (birth_ms, uid) = born[&pid];
        let id = identities.get(&(pid, birth_ms));
        let named = names.entry(c).or_default();
        named.part = part;
        // The lead named last tick, where it is this one as read now, stays
        // as it is: most leads do not move, and their words are not copied.
        if !named
            .lead
            .as_ref()
            .is_some_and(|l| l.is(pid, birth_ms, uid, id))
        {
            named.lead = Some(Lead::of(pid, birth_ms, uid, id));
        }
    }
    for (c, part) in parts.into_iter().filter(|(c, _)| listed.contains(c)) {
        names.insert(c, Named { part, lead: None });
    }
    live
}

/// The facts' running state between split ticks: the base the next window starts at, each
/// coalition's last name, each lead identity read, and the last facts.
#[derive(Debug, Default)]
pub(crate) struct WhyState {
    base: Option<CoalBase>,
    names: IdMap<u64, Named>,
    /// Each lead's last identity, by pid and birth, replaced by each host
    /// reading (including a current unknown). Kept while that process is
    /// swept, or while it leads a listed coalition on a truncated sweep.
    identities: IdMap<(u32, i64), LeadIdentity>,
    facts: Option<Arc<WhyFacts>>,
    episodes: Episodes,
    clause: ClauseHold,
    /// The tabs' and aterm's CPU, live and reaped, read at each whole
    /// reading while splits are taken ([`reap`]).
    reap: ReapLedger,
    /// Whether a read misread the record's layout ([`misread`]): the lane
    /// stays off for good, since the kernel's layout does not change under a
    /// running process.
    misread: bool,
    /// The defect injected into each split by coalition (negative controls only).
    #[cfg(any(test, debug_assertions))]
    pub(crate) fault: Option<Fault>,
}

impl WhyState {
    /// The lane sample `s` reads under: its host's word, or `Off` once a read misread the layout.
    pub(crate) fn lane(&self, s: &Sample) -> CoalLane {
        if s.coal_lane.on() && self.misread {
            CoalLane::Off
        } else {
            s.coal_lane
        }
    }

    /// The facts snapshot `snap` (of sample `s`) carries: new ones for a split tick with a window,
    /// else the last ones.
    pub(crate) fn absorb(
        &mut self,
        s: &Sample,
        snap: &Snapshot,
        attribution: Option<(&[Membership], &[Option<usize>])>,
    ) -> Option<Arc<WhyFacts>> {
        self.misread = self.misread || (s.coal_lane.on() && misread(s));
        let lane = self.lane(s);
        if !lane.on() {
            self.episodes = Episodes::default();
            self.clause = ClauseHold::default();
        }
        self.step_reap(s, lane, attribution);
        let fresh = match s.coal_lane {
            CoalLane::On if self.misread => by_subtraction(s, snap, CoalLane::Off),
            CoalLane::On => self.by_coalitions(s, attribution),
            lane => by_subtraction(s, snap, lane),
        };
        match fresh {
            Some(facts) => {
                #[cfg(any(test, debug_assertions))]
                let facts = match self.fault {
                    Some(fault) => fault.inject(facts, snap),
                    None => facts,
                };
                self.facts = Some(Arc::new(self.explain(facts, s, snap)));
            }
            None if s.coal_lane.on() && !self.misread && s.coalitions.is_some() => {
                self.episodes = Episodes::default();
                self.clause = ClauseHold::default();
                if let Some(f) = self
                    .facts
                    .as_mut()
                    .filter(|f| !f.open.is_empty() || f.clause.is_some())
                {
                    let f = Arc::make_mut(f);
                    f.open.clear();
                    f.clause = None;
                }
            }
            None if !lane.on() && s.procs.is_some() => self.facts = None,
            None => {}
        }
        self.facts.clone()
    }

    /// Steps the reap credit's ledger on sample `s`'s sweep: on a split
    /// tick, and on every whole reading between split ticks under 60 s
    /// apart, so a member is seen as soon as a sweep can see it; never with
    /// the lane off, where no split reads it.
    fn step_reap(
        &mut self,
        s: &Sample,
        lane: CoalLane,
        attribution: Option<(&[Membership], &[Option<usize>])>,
    ) {
        let (Some(procs), Some(uid), Some((members, owners))) =
            (s.procs.as_deref(), s.self_uid, attribution)
        else {
            return;
        };
        let taking = s.coalitions.is_some()
            || self
                .base
                .as_ref()
                .is_some_and(|b| s.at_ns.saturating_sub(b.at_ns) <= WINDOW_MAX_NS);
        if !lane.on() || !taking {
            return;
        }
        let delegated: Vec<bool> = owners.iter().map(Option::is_some).collect();
        self.reap.step(&reap::StepIn {
            at_ns: s.at_ns,
            at_unix_ms: s.at_unix_ms,
            procs,
            members,
            delegated: &delegated,
            tabs: &s.tabs,
            self_uid: uid,
            truncated: s.procs_truncated,
        });
    }

    /// `facts` with the causes that hold over their window, the episodes
    /// open after it and the Deck's clause.
    fn explain(&mut self, mut facts: WhyFacts, s: &Sample, snap: &Snapshot) -> WhyFacts {
        #[cfg(any(test, debug_assertions))]
        let (vouched, residual) = (
            self.fault != Some(Fault::MatchBySigningIdOnly),
            self.fault != Some(Fault::IgnoreUnaccounted),
        );
        #[cfg(any(test, debug_assertions))]
        {
            self.clause.no_hold = self.fault == Some(Fault::NoHold);
        }
        #[cfg(not(any(test, debug_assertions)))]
        let (vouched, residual) = (true, true);
        facts.causes = detect::detect_as(&facts, snap, vouched, residual);
        facts.open = self
            .episodes
            .step(s.mono_ms, s.at_unix_ms, &facts.causes, s.procs_truncated);
        facts.clause = self.clause.step(s.mono_ms, &facts.open, &facts.split);
        facts
    }

    fn by_coalitions(
        &mut self,
        s: &Sample,
        attribution: Option<(&[Membership], &[Option<usize>])>,
    ) -> Option<WhyFacts> {
        for l in &s.leads {
            // An identity read as before is left in place: no copy of its words.
            match self.identities.get_mut(&(l.pid, l.birth_ms)) {
                Some(kept) if kept == l => {}
                Some(kept) => kept.clone_from(l),
                None => {
                    self.identities.insert((l.pid, l.birth_ms), l.clone());
                }
            }
        }
        let (Some(read), Some(procs), Some(uid)) =
            (s.coalitions.as_ref(), s.procs.as_deref(), s.self_uid)
        else {
            return None;
        };
        let swept: IdMap<u32, Option<i64>> = procs.iter().map(|p| (p.pid, p.birth_ms)).collect();
        let leading: IdSet<(u32, i64)> = if s.procs_truncated {
            self.names
                .values()
                .filter_map(|n| n.lead.as_ref().map(|l| (l.pid, l.birth_ms)))
                .collect()
        } else {
            IdSet::default()
        };
        self.identities.retain(|key, _| match swept.get(&key.0) {
            Some(birth) => birth.is_none_or(|b| b == key.1),
            None => leading.contains(key),
        });
        let live = name(
            &mut self.names,
            read,
            procs,
            s.self_pid,
            uid,
            &s.tabs,
            &self.identities,
        );
        let mut now = CoalBase::of(s, read, procs, uid)?;
        now.reap = self.reap.stepped_at(s.at_ns).then(|| self.reap.totals());
        let base = self.base.replace(now)?;
        let now = self.base.as_ref()?;
        let window_ns = now.window_after(&base)?;
        let (members, owners) = attribution?;
        let delegated: Vec<bool> = owners.iter().map(Option::is_some).collect();
        let split = split(&SplitIn {
            base: &base,
            now,
            procs,
            members,
            delegated: &delegated,
            self_uid: uid,
            tabs: &s.tabs,
            names: &self.names,
            live: &live,
            listed: read.listed,
            reap: Some(&self.reap),
        });
        Some(WhyFacts {
            at_ns: s.at_ns,
            at_unix_ms: s.at_unix_ms,
            window_ns,
            lane: CoalLane::On,
            split,
            procs_truncated: s.procs_truncated,
            timeline: s.timeline.clone(),
            userspace_ms: userspace_ms(s),
            self_uid: Some(uid),
            ..WhyFacts::default()
        })
    }
}

/// How many times memory and swap together the coalitions' footprints may
/// sum to before the read is a misread.
const MISREAD_X: u64 = 8;

/// Whether `s`'s coalitions hold more footprint than any machine of its
/// memory could ([`MISREAD_X`]): a record whose layout moved under the
/// readers.
fn misread(s: &Sample) -> bool {
    let (Some(read), Some(mem)) = (s.coalitions.as_ref(), s.mem.as_ref()) else {
        return false;
    };
    let held: u64 = read
        .rows
        .iter()
        .filter_map(|r| r.footprint)
        .fold(0, u64::saturating_add);
    let room = mem.total.saturating_add(mem.swap_total);
    mem.total > 0 && held > room.saturating_mul(MISREAD_X)
}

/// The facts of the snapshot's own split by subtraction, for snapshot `snap`
/// (of sample `s`) with the lane `lane`; `None` without a split.
fn by_subtraction(s: &Sample, snap: &Snapshot, lane: CoalLane) -> Option<WhyFacts> {
    let built = snap.split?;
    Some(WhyFacts {
        at_ns: snap.at_ns,
        at_unix_ms: snap.at_unix_ms,
        window_ns: snap.procs_window_ns?,
        lane,
        split: CoalSplit::by_subtraction(&built, &snap.tabs, &s.tabs),
        procs_truncated: snap.procs_truncated,
        timeline: s.timeline.clone(),
        userspace_ms: userspace_ms(s),
        self_uid: s.self_uid,
        ..WhyFacts::default()
    })
}

/// When userspace started: the earliest birth among `s`'s swept processes
/// above pid 1; `None` for a sweep cut at its cap, which the kernel's
/// newest-first list cuts at the oldest, the boot-time daemons.
fn userspace_ms(s: &Sample) -> Option<i64> {
    if s.procs_truncated {
        return None;
    }
    s.procs
        .as_deref()?
        .iter()
        .filter(|p| p.pid > 1)
        .filter_map(|p| p.birth_ms)
        .min()
}

/// Cores-worth as millicores, rounded.
fn millicores(cores: f32) -> i64 {
    (f64::from(cores) * 1000.0).round() as i64
}

#[cfg(test)]
#[path = "split_tests.rs"]
mod split_tests;

#[cfg(test)]
#[path = "explain_tests.rs"]
mod explain_tests;
