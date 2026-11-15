// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Who uses the machine's CPU, read per resource coalition: the facts the cause engine and `sys
//! why` read. RD is `docs/DESIGN-resource-details-2026-10-20.md`, SA
//! `docs/DESIGN-system-awareness-2026-10-16.md`.

use std::hash::{BuildHasherDefault, Hasher};
use std::sync::Arc;

use crate::model::{Membership, Sample, Snapshot};

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
    /// This OS has no coalition reader (Linux, Windows): use process and reaper evidence.
    #[default]
    Unsupported,
    /// macOS answered the launch self-check: the split is by coalition.
    On,
    /// macOS's self-check missed, or a read did not match the record layout the readers hold: the
    /// split uses process and reaper evidence.
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
    /// Proved app ownership, including independent app subsets moved out of mixed coalitions.
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
    /// A foreign member was observed in this coalition. This proof persists after it exits.
    pub mixed: bool,
    /// At least one app-owned member has been observed; used to separate a mixed remainder.
    pub has_app: bool,
    /// A foreign member's user class was unread or disagreed with another member's class.
    pub unknown_owner: bool,
    /// Where the coalition's unowned remainder belongs when full app ownership is unproved.
    pub outside: Part,
    /// Its lead; `None` when no live member's birth was read.
    pub lead: Option<Lead>,
}

/// How a split was made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Via {
    /// Measured coalition counters with independently proved owner subsets for mixed groups.
    Coalitions,
    /// Process and reaper evidence; unproved work stays in the signed residual.
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
    /// Independently attributed app members' own CPU moved out of an outside coalition,
    /// millicores. Different kernel read cuts can put this past [`Self::cpu_mc`].
    pub delegated_mc: i64,
    /// Whole-coalition ownership is mixed or incomplete; its raw total has no single owner.
    pub ownership_mixed: bool,
    /// Independently measured foreign members' own CPU when ownership is mixed.
    pub outside_floor_mc: i64,
    /// Of [`Self::cpu_mc`], what ran at maintenance or background QoS (deferred work), millicores.
    pub deferred_mc: i64,
    /// Its seven QoS buckets' growth over the window, millicores: what [`Self::deferred_mc`] is a
    /// share of.
    pub qos_mc: i64,
    /// Bytes per second it read from storage over the window.
    pub read_bps: u64,
    /// Its live members' footprint now, bytes, where read.
    pub footprint: Option<u64>,
    /// A complete current active census proves every live member is app-owned. Footprint
    /// is instantaneous, so this proof is independent of the CPU window's earlier owners.
    pub footprint_aterm: bool,
    /// A complete current census proves the foreign members share this user class. This
    /// instantaneous footprint proof is independent of historical CPU ownership uncertainty.
    pub footprint_outside: Option<Part>,
    /// This tick's readable memory charges of proved app members in a foreign coalition.
    /// Kept separately so a complete subset can move to aterm without moving its launcher's
    /// other work, and an unread subset leaves the coalition explicitly mixed.
    pub owned_footprint: crate::model::MemoryCharge,
    /// The current active census or foreign owner identities were incomplete, so the
    /// coalition's footprint cannot be divided among owners from this sweep.
    pub footprint_ownership_incomplete: bool,
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
    /// Partition this instantaneous coalition charge using only its current ownership proof.
    /// CPU's historical owner and window uncertainty cannot classify current memory.
    pub(crate) fn footprint_account(&self) -> FootprintAccount {
        let outside_part = self
            .footprint_outside
            .filter(|part| matches!(part, Part::Others | Part::Services));
        let owner = if self.footprint_aterm {
            FootprintOwner::Aterm
        } else if self.owned_footprint.members > 0 {
            FootprintOwner::Mixed
        } else {
            outside_part.map_or(FootprintOwner::Unknown, FootprintOwner::Outside)
        };
        let mut account = FootprintAccount {
            owner,
            total: self.footprint,
            outside_part,
            aterm: Default::default(),
            outside: Default::default(),
            unknown: Default::default(),
            incomplete: false,
        };
        if self.footprint == Some(0) {
            match owner {
                FootprintOwner::Aterm => account.aterm.add(Some(0)),
                FootprintOwner::Outside(_) => account.outside.add(Some(0)),
                _ => account.unknown.add(Some(0)),
            }
        } else if self.footprint_ownership_incomplete {
            account.owner = if self.owned_footprint.members > 0 {
                FootprintOwner::Mixed
            } else {
                FootprintOwner::Unknown
            };
            account.unknown.add(self.footprint);
            account.incomplete = true;
        } else if self.footprint_aterm {
            account.aterm.add(self.footprint);
        } else if self.owned_footprint.members > 0 {
            let owned = self.owned_footprint;
            if let Some(total) = self.footprint
                && owned.unread == 0
                && owned.bytes <= total
            {
                account.aterm.add(Some(owned.bytes));
                if owned.bytes == total {
                    account.owner = FootprintOwner::Aterm;
                } else if outside_part.is_some() && owned.members < self.live {
                    account.outside.add(Some(total - owned.bytes));
                } else {
                    account.unknown.add(Some(total - owned.bytes));
                    account.incomplete = true;
                }
            } else {
                account.unknown.add(self.footprint);
                account.incomplete = true;
            }
        } else if outside_part.is_some() {
            account.outside.add(self.footprint);
        } else {
            account.unknown.add(self.footprint);
            account.incomplete = true;
        }
        account
    }

    /// Its deferred share, per mille of its QoS buckets ([`Self::qos_mc`]), never past 1000; `None`
    /// where the buckets did not grow.
    #[must_use]
    pub fn deferred_pm(&self) -> Option<u16> {
        deferred_pm(self.deferred_mc, self.qos_mc)
    }
}

/// The proved owner of a current coalition footprint, independently of CPU history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FootprintOwner {
    Aterm,
    Outside(Part),
    Mixed,
    Unknown,
}

/// One coalition's conserved current charge account. Each known byte appears in one bucket.
pub(crate) struct FootprintAccount {
    pub owner: FootprintOwner,
    pub total: Option<u64>,
    pub outside_part: Option<Part>,
    pub aterm: crate::model::MemoryCharge,
    pub outside: crate::model::MemoryCharge,
    pub unknown: crate::model::MemoryCharge,
    pub incomplete: bool,
}

impl FootprintAccount {
    pub(crate) fn outside_bytes(&self) -> Option<u64> {
        if self.total == Some(0) || self.owner == FootprintOwner::Aterm {
            return Some(0);
        }
        if self.incomplete {
            return None;
        }
        self.outside.value()
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
    /// every row is measured and every scope's CPU charge is unambiguous (`exact`), or not
    /// (`heuristic`, as every process-only account is).
    pub exact: bool,
    /// App and outside scopes are lower bounds because a coalition has mixed or unread owners.
    pub ownership_incomplete: bool,
    /// Whether the kernel's list of coalitions stopped short at either end of the window
    /// (`cannot what=coalition_list why=truncated`).
    pub list_truncated: bool,
    /// Coalitions the kernel listed (0 for a process-only account).
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
    /// Every other measured coalition, less the delegated work. By subtraction, only observed
    /// other-user processes' own CPU; unknown services remain in [`Self::unaccounted_mc`].
    pub services_mc: i64,
    /// What no part holds, signed and never clamped.
    pub unaccounted_mc: i64,
    /// Each tab's work over the window, by its key ([`crate::model::TabRoot::tab`]; a tab split into panes is
    /// one), in the host's order: where the reap credit's ledger read both ends of the window
    /// ([`reap`]), its members' own CPU and their reaped children's (finished work included),
    /// else its processes alive at both ends of it; delegated members' live work included.
    pub tabs: Vec<(u64, i64)>,
    /// Independently observed own CPU for each tab, without any reaped-child charge.
    pub tab_floors: Vec<(u64, i64)>,
    /// Tabs whose reaped-counter charge may exceed actual CPU in this window.
    pub ambiguous_tabs: Vec<u64>,
    /// The same over the window for aterm's processes in no tab (itself, its helpers, what
    /// closed tabs left, and a delegated job's members whose tab closed), so of
    /// [`Self::aterm_mc`] plus [`Self::delegated_mc`], not of aterm's coalitions alone.
    pub own_mc: i64,
    /// Independently observed own CPU outside tabs, millicores.
    pub own_floor_mc: i64,
    /// Whether the charge outside tabs may exceed actual CPU.
    pub own_ambiguous: bool,
    /// [`Self::aterm_mc`] plus [`Self::delegated_mc`] less every supported actual-CPU figure
    /// ([`Self::own_actual`] and [`Self::tab_actual_of`]), never below 0: work in no
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
    /// (which reads 0 then); 0 in any other window and without coalition counters.
    pub skew_mc: i64,
    /// The tabs, by key, whose figure the reap credit's ledger knows is short of their work over
    /// the window, so only a lower bound however little is [`Self::unattributed_mc`] ([`reap`]'s
    /// missed steps): a member's tail where launchd reaped it, work taken out against members
    /// gone with their parent that the reaper's reaped time fell short of, a member of aterm's
    /// back from a delegated job, a process first read since the last reading. None by
    /// subtraction, which reads no finished work.
    pub short_tabs: Vec<u64>,
    /// Every listed coalition that answered, busiest first (none in a process-only account), rounded as a
    /// set to their measured total. A mixed row retains its raw coalition total, while only
    /// independently attributed owner floors enter the scope totals.
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
    /// Reconciled counter charge of these tabs, which can still repeat or miss inherited work.
    #[must_use]
    pub fn tab_charge_of(&self, panes: &[u64]) -> i64 {
        self.tabs
            .iter()
            .filter(|(k, _)| panes.contains(k))
            .map(|(_, mc)| *mc)
            .sum()
    }

    /// The supported actual-CPU figure: exact charge where unambiguous, otherwise only the
    /// independently measured own-CPU floor. Charges remain available separately.
    #[must_use]
    pub fn tab_actual_of(&self, panes: &[u64]) -> i64 {
        self.tabs
            .iter()
            .filter(|(k, _)| panes.contains(k))
            .map(|(k, charge)| {
                let floor = self
                    .tab_floors
                    .iter()
                    .find(|(key, _)| key == k)
                    .map_or(0, |(_, mc)| *mc);
                if self.ownership_incomplete
                    || self.unaccounted_mc > 0
                    || self.ambiguous_tabs.contains(k)
                    || floor > *charge
                {
                    floor
                } else {
                    *charge
                }
            })
            .sum()
    }

    /// Actual CPU outside tabs, or its independent floor when the charge is ambiguous.
    #[must_use]
    pub fn own_actual(&self) -> i64 {
        if self.ownership_incomplete || self.own_ambiguous || self.own_floor_mc > self.own_mc {
            self.own_floor_mc
        } else {
            self.own_mc
        }
    }

    /// The parts with tab `tab`'s work (the caller's) out of aterm's: none for no tab or one the
    /// split did not read.
    #[must_use]
    pub fn parts(&self, tab: Option<u64>) -> Parts {
        self.parts_of(tab.as_slice())
    }

    /// [`Self::parts`] for the tab whose split panes are the sessions `panes`.
    #[must_use]
    pub fn parts_of(&self, panes: &[u64]) -> Parts {
        self.parts_of_coverage(panes, false)
    }

    /// Owner parts with source coverage that may be incomplete independently of coalition reads.
    #[must_use]
    pub fn parts_of_coverage(&self, panes: &[u64], source_incomplete: bool) -> Parts {
        let tab = self.tab_actual_of(panes);
        if self.ownership_incomplete || source_incomplete {
            let tab = self
                .tab_floors
                .iter()
                .filter(|(key, _)| panes.contains(key))
                .map(|(_, cpu)| *cpu)
                .sum();
            let aterm_rest = self.own_floor_mc
                + self
                    .tab_floors
                    .iter()
                    .filter(|(key, _)| !panes.contains(key))
                    .map(|(_, cpu)| *cpu)
                    .sum::<i64>();
            return Parts {
                tab,
                aterm_rest,
                others: self.others_mc,
                services: self.services_mc,
                unaccounted: self.global_mc - tab - aterm_rest - self.others_mc - self.services_mc,
            };
        }
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
    /// Some app/session roots were unproved, or historical native CPU evidence was lost or
    /// could not be timed. Carried facts retain this CPU gap after identity or source recovery.
    pub provenance_incomplete: bool,
    /// Current footprint ownership was unproved at this source reading. Historical CPU
    /// source loss is separate and does not invalidate a complete later memory census.
    pub memory_provenance_incomplete: bool,
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
    /// Falsely charge the positive residual to services instead of preserving unknown work.
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
    /// snapshot they are of); current process-only facts as they are.
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
                split.tab_floors.clone_from(&split.tabs);
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
/// coalition's live member count. Whole-coalition app ownership requires every active member
/// to be swept and proved app-owned. A foreign member permanently prevents that claim for this
/// coalition, even after it exits. Mixed coalitions retain their outside part; the split moves
/// only independently attributed app work. Lead identities come from `identities`.
pub fn name(
    names: &mut IdMap<u64, Named>,
    read: &CoalSample,
    procs: &[crate::model::ProcSample],
    self_uid: u32,
    membership: &[Membership],
    delegated: &[Option<usize>],
    identities: &IdMap<(u32, i64), LeadIdentity>,
) -> IdMap<u64, u32> {
    let listed: IdSet<u64> = read.rows.iter().map(|r| r.id).collect();
    let active: IdMap<u64, Option<u64>> = read
        .rows
        .iter()
        .map(|r| (r.id, r.tasks_started.checked_sub(r.tasks_exited)))
        .collect();
    let mut live: IdMap<u64, u32> = IdMap::default();
    let mut parts: IdMap<u64, Part> = IdMap::default();
    let mut foreign_parts: IdMap<u64, Part> = IdMap::default();
    let mut unknown_owners: IdSet<u64> = IdSet::default();
    let mut foreign: IdSet<u64> = IdSet::default();
    let mut owned: IdSet<u64> = IdSet::default();
    let mut app: IdSet<u64> = IdSet::default();
    let mut members = Vec::new();
    let mut born: IdMap<u32, (i64, Option<u32>)> = IdMap::default();
    for ((p, member), owner) in procs.iter().zip(membership).zip(delegated) {
        let Some(c) = p.coalition else { continue };
        *live.entry(c).or_default() += 1;
        let ours = member.aterm || owner.is_some();
        if member.aterm || owner.is_some() {
            app.insert(c);
        }
        if ours {
            owned.insert(c);
        } else {
            foreign.insert(c);
        }
        let part = if p.uid == Some(self_uid) {
            Part::Others
        } else {
            Part::Services
        };
        let best = parts.entry(c).or_insert(part);
        if part == Part::Others && *best == Part::Services {
            *best = part;
        }
        if !ours {
            if p.uid.is_none() {
                unknown_owners.insert(c);
            }
            let outside = foreign_parts.entry(c).or_insert(part);
            if *outside != part {
                unknown_owners.insert(c);
            }
            if part == Part::Others {
                *outside = part;
            }
        }
        if let Some(birth) = p.birth_ms {
            members.push((p.pid, c, birth));
            born.insert(p.pid, (birth, p.uid));
        }
    }
    // An unread listed record cannot erase prior mixed ownership. Coalition IDs are never
    // reused; forget a name only when a complete answer set proves that coalition gone.
    if !read.truncated && read.rows.len() == read.listed as usize {
        names.retain(|id, _| listed.contains(id));
    }
    for (c, part) in &mut parts {
        let named = names.entry(*c).or_default();
        let was_mixed = named.mixed;
        named.mixed |= foreign.contains(c);
        named.has_app |= app.contains(c);
        named.unknown_owner |= unknown_owners.contains(c);
        named.outside =
            foreign_parts
                .get(c)
                .copied()
                .unwrap_or(if was_mixed { named.outside } else { *part });
        let census = active.get(c).copied().flatten() == Some(u64::from(live[c]));
        *part = if owned.contains(c) && !named.mixed && census {
            Part::Aterm
        } else {
            named.outside
        };
    }
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
        let named = names.entry(c).or_default();
        named.part = part;
        named.lead = None;
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
    /// The tabs' and aterm's CPU, live and reaped, read at each whole reading ([`reap`]).
    reap: ReapLedger,
    /// The last whole reading used by the resource rows' scope histories.
    scope_base: Option<(u64, ReapTotals, IdMap<crate::delegated::ProcessKey, u64>)>,
    /// Whether a read misread the record's layout ([`misread`]): the lane
    /// stays off for good, since the kernel's layout does not change under a
    /// running process.
    misread: bool,
    /// The defect injected into each split by coalition (negative controls only).
    #[cfg(any(test, debug_assertions))]
    pub(crate) fault: Option<Fault>,
}

/// One whole sweep's common-window CPU account, separate from process-live rates.
#[derive(Clone, Debug, PartialEq)]
pub struct ScopeCpu {
    /// Duration between the two whole readings, nanoseconds.
    pub window_ns: u64,
    /// App CPU supported by the matched coalition account, including proved mixed-owner floors.
    pub app_measured: Option<f64>,
    /// Whether the measured app total is only a lower bound of app ownership/coverage.
    pub app_incomplete: bool,
    /// Non-tab reconciled counter charge, in cores.
    pub own_charge: f64,
    /// Non-tab CPU proved by each observed process's own counter, in cores.
    pub own_floor: f64,
    /// Whether the non-tab reaped remainder cannot be equated with actual CPU.
    pub own_ambiguous: bool,
    /// Whether any contribution was unread or missed in this interval.
    pub missed: bool,
    /// Logical tabs, one row per tab key.
    pub tabs: Vec<ScopeCpuTab>,
    /// Independently confirmed CPU for each currently open pane. A logical tab can hold
    /// additional work from a pane that has closed.
    pub panes: Vec<ScopeCpuPane>,
}

/// A pane's independently observed own-counter CPU over the common window.
#[derive(Clone, Debug, PartialEq)]
pub struct ScopeCpuPane {
    /// Pane/session key.
    pub key: u64,
    /// Confirmed CPU in cores.
    pub floor: f64,
}

/// A logical tab's contribution to the common-window CPU account.
#[derive(Clone, Debug, PartialEq)]
pub struct ScopeCpuTab {
    /// Logical tab key.
    pub key: u64,
    /// Stable identity assigned by the reap ledger while the logical tab stays open.
    pub id: u64,
    /// Reconciled counter charge, in cores.
    pub charge: f64,
    /// Proved own-counter CPU, in cores.
    pub floor: f64,
    /// Whether inherited reaped-counter charge can include repeated accounting.
    pub ambiguous: bool,
    /// Whether the interval missed or could not read some tab work.
    pub missed: bool,
}

impl ScopeCpu {
    /// Actual non-tab CPU supported by this account; a lower bound when ambiguous.
    #[must_use]
    pub fn own_actual(&self) -> f64 {
        if self.own_ambiguous {
            self.own_floor
        } else {
            self.own_charge.max(self.own_floor)
        }
    }
    /// Actual app CPU supported by this account, including every logical tab once.
    #[must_use]
    pub fn all_actual(&self) -> f64 {
        let assigned = self.own_actual() + self.tabs.iter().map(ScopeCpuTab::actual).sum::<f64>();
        self.app_measured.map_or(assigned, |app| app.max(assigned))
    }
    /// App kernel counter charge, which is separate from proved actual CPU.
    #[must_use]
    pub fn all_charge(&self) -> f64 {
        self.own_charge + self.tabs.iter().map(|t| t.charge).sum::<f64>()
    }
}

/// Fresh raw facts prepared before the resource rows, explained only after their snapshot is
/// complete. The coalition baseline advances once for a whole reading.
pub(crate) struct PreparedWhy {
    lane: CoalLane,
    facts: Option<WhyFacts>,
}

impl PreparedWhy {
    pub(crate) fn memory_owners(&self) -> Option<crate::model::MemoryOwners> {
        let facts = self.facts.as_ref().filter(|facts| facts.lane.on())?;
        Some(crate::model::MemoryOwners::of_coalitions(
            &facts.split,
            facts.memory_provenance_incomplete,
        ))
    }
    pub(crate) fn apply_app(&self, s: &Sample, cpu: &mut ScopeCpu) {
        let Some(facts) = self
            .facts
            .as_ref()
            .filter(|f| f.lane.on() && f.window_ns == cpu.window_ns)
        else {
            return;
        };
        cpu.app_measured =
            Some((facts.split.aterm_mc + facts.split.delegated_mc).max(0) as f64 / 1000.0);
        cpu.app_incomplete = facts.split.ownership_incomplete
            || facts.split.list_truncated
            || facts.split.rows.len() < facts.split.listed as usize
            || facts
                .split
                .rows
                .iter()
                .any(|row| row.part == Part::Aterm && !row.measured)
            || s.procs_truncated
            || s.native_incomplete
            || s.native_source_lost
            || s.ownership_incomplete
            || !s.unproved_tabs.is_empty();
        let assigned = cpu.own_actual() + cpu.tabs.iter().map(ScopeCpuTab::actual).sum::<f64>();
        cpu.app_incomplete |= cpu.app_measured.is_some_and(|app| app < assigned);
    }
}

impl ScopeCpuTab {
    /// Actual CPU supported by this account; a lower bound when ambiguous.
    #[must_use]
    pub fn actual(&self) -> f64 {
        if self.ambiguous {
            self.floor
        } else {
            self.charge.max(self.floor)
        }
    }
}

impl WhyState {
    pub(crate) fn has_pending_native(&self) -> bool {
        self.reap.has_pending_native()
    }
    /// Count finished children before the resource rows append this reading. The explanation
    /// and the rows share this ledger, so short jobs have the same owner on both surfaces.
    pub(crate) fn scope_cpu(
        &mut self,
        s: &Sample,
        attribution: Option<(&[Membership], &[Option<usize>])>,
    ) -> Option<ScopeCpu> {
        self.step_reap(s, attribution);
        if !self.reap.stepped_at(s.at_ns) {
            return None;
        }
        if let Some(read) = &s.coalitions {
            self.reap.retain_coalitions(read);
        }
        let now = self.reap.totals();
        let procs = s.procs.as_deref()?;
        let counters = procs
            .iter()
            .filter_map(|p| {
                Some((
                    crate::delegated::ProcessKey {
                        pid: p.pid,
                        start: p.start?,
                    },
                    p.cpu_ns?,
                ))
            })
            .collect();
        let (at, base, before) = self.scope_base.replace((s.at_ns, now.clone(), counters))?;
        let window = s
            .at_ns
            .checked_sub(at)
            .filter(|dt| *dt > 0 && *dt <= WINDOW_MAX_NS)?;
        let mut extra_own = 0_u64;
        let mut extra_tabs = IdMap::<u64, u64>::default();
        let mut extra_panes = IdMap::<u64, u64>::default();
        let mut extra_missing = IdSet::<Option<u64>>::default();
        let (members, owners) = attribution?;
        for ((p, member), owner) in procs.iter().zip(members).zip(owners) {
            if owner.is_none() {
                continue;
            }
            let tab = member
                .tab
                .and_then(|index| s.tabs.get(index))
                .map(|t| t.tab);
            let delta = p
                .start
                .and_then(|start| before.get(&crate::delegated::ProcessKey { pid: p.pid, start }))
                .zip(p.cpu_ns)
                .and_then(|(before, now)| now.checked_sub(*before));
            if let Some(delta) = delta {
                if let Some(tab) = tab {
                    let sum = extra_tabs.entry(tab).or_default();
                    *sum = sum.saturating_add(delta);
                    if let Some(pane) = member
                        .tab
                        .and_then(|index| s.tabs.get(index))
                        .map(|root| root.key)
                    {
                        let sum = extra_panes.entry(pane).or_default();
                        *sum = sum.saturating_add(delta);
                    }
                } else {
                    extra_own = extra_own.saturating_add(delta);
                }
            } else {
                extra_missing.insert(tab);
            }
        }
        let tabs: Vec<_> = now
            .tabs
            .iter()
            .map(|tab| {
                let (cpu, missed) = now.since(&base, tab.key);
                let (floor, ambiguous) = now.floor_since(&base, tab.key);
                let extra = extra_tabs.get(&tab.key).copied().unwrap_or(0);
                ScopeCpuTab {
                    key: tab.key,
                    id: tab.id,
                    charge: cpu.saturating_add(extra) as f64 / window as f64,
                    floor: floor.saturating_add(extra) as f64 / window as f64,
                    ambiguous: ambiguous || floor > cpu,
                    missed: missed || extra_missing.contains(&Some(tab.key)),
                }
            })
            .collect();
        let panes = s
            .tabs
            .iter()
            .map(|pane| ScopeCpuPane {
                key: pane.key,
                floor: now
                    .pane_floor_since(&base, pane.key)
                    .saturating_add(extra_panes.get(&pane.key).copied().unwrap_or(0))
                    as f64
                    / window as f64,
            })
            .collect();
        let raw_tabs = now.tabs.iter().fold(0_u64, |sum, t| {
            sum.saturating_add(now.since(&base, t.key).0)
        });
        let (own_floor, own_ambiguous) = now.own_floor_since(&base);
        let own_charge = now.all.saturating_sub(base.all).saturating_sub(raw_tabs);
        Some(ScopeCpu {
            window_ns: window,
            app_measured: None,
            app_incomplete: true,
            own_charge: own_charge.saturating_add(extra_own) as f64 / window as f64,
            own_floor: own_floor.saturating_add(extra_own) as f64 / window as f64,
            own_ambiguous: own_ambiguous || own_floor > own_charge,
            missed: now.missed > base.step
                || !extra_missing.is_empty()
                || s.ownership_incomplete
                || s.native_source_lost,
            tabs,
            panes,
        })
    }

    /// The lane sample `s` reads under: its host's word, or `Off` once a read misread the layout.
    pub(crate) fn lane(&self, s: &Sample) -> CoalLane {
        if s.coal_lane.on() && self.misread {
            CoalLane::Off
        } else {
            s.coal_lane
        }
    }

    /// Capture counters before Rows without requiring a partly built Snapshot.
    pub(crate) fn prepare(
        &mut self,
        s: &Sample,
        attribution: Option<(&[Membership], &[Option<usize>])>,
    ) -> PreparedWhy {
        self.misread = self.misread || (s.coal_lane.on() && misread(s));
        let lane = self.lane(s);
        if !lane.on() {
            self.episodes = Episodes::default();
            self.clause = ClauseHold::default();
        }
        if !self.reap.stepped_at(s.at_ns) {
            self.step_reap(s, attribution);
        }
        let facts = lane
            .on()
            .then(|| self.by_coalitions(s, attribution))
            .flatten();
        PreparedWhy { lane, facts }
    }

    /// Finish causes and episodes using the complete snapshot, reusing this reading's already
    /// prepared coalition facts rather than advancing their baseline a second time.
    pub(crate) fn finish(
        &mut self,
        s: &Sample,
        snap: &Snapshot,
        prepared: PreparedWhy,
    ) -> Option<Arc<WhyFacts>> {
        let lane = prepared.lane;
        let fresh = if lane.on() {
            prepared.facts
        } else {
            by_subtraction(s, snap, lane)
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

    /// Steps the shared CPU ledger on each whole sweep, independently of the coalition lane.
    fn step_reap(&mut self, s: &Sample, attribution: Option<(&[Membership], &[Option<usize>])>) {
        self.reap
            .ingest_terminals(&s.native_terminals, s.native_incomplete);
        let (Some(procs), Some(uid), Some((members, owners))) =
            (s.procs.as_deref(), s.self_uid, attribution)
        else {
            return;
        };
        let delegated: Vec<bool> = owners.iter().map(Option::is_some).collect();
        self.reap.step_with_terminals(
            &reap::StepIn {
                at_ns: s.at_ns,
                at_unix_ms: s.at_unix_ms,
                cpu_cut: s.native_cpu_cut,
                procs,
                members,
                delegated: &delegated,
                tabs: &s.tabs,
                self_uid: uid,
                truncated: s.procs_truncated,
            },
            &s.native_terminals,
            s.native_incomplete,
        );
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
            uid,
            attribution?.0,
            attribution?.1,
            &self.identities,
        );
        let mut now = CoalBase::of(s, read, procs)?;
        now.exclusive = read
            .rows
            .iter()
            .filter(|row| {
                self.names
                    .get(&row.id)
                    .is_some_and(|named| named.part == Part::Aterm)
                    && row.tasks_started.checked_sub(row.tasks_exited)
                        == Some(u64::from(live.get(&row.id).copied().unwrap_or(0)))
            })
            .map(|row| row.id)
            .collect();
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
            provenance_incomplete: s.native_incomplete
                || s.native_source_lost
                || s.ownership_incomplete
                || !s.unproved_tabs.is_empty(),
            memory_provenance_incomplete: s.native_incomplete
                || s.ownership_incomplete
                || !s.unproved_tabs.is_empty(),
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

/// The facts of the snapshot's own process account, for snapshot `snap`
/// (of sample `s`) with the lane `lane`; `None` without a split.
fn by_subtraction(s: &Sample, snap: &Snapshot, lane: CoalLane) -> Option<WhyFacts> {
    let built = snap.split?;
    let mut split = CoalSplit::by_subtraction(&built, &snap.tabs, &s.tabs);
    if let Some(cpu) = &snap.scoped_cpu {
        split.aterm_mc = (cpu.all_actual() * 1000.0).floor() as i64;
        split.own_mc = (cpu.own_charge * 1000.0).round() as i64;
        split.own_floor_mc = (cpu.own_floor * 1000.0).floor() as i64;
        split.own_ambiguous = cpu.own_ambiguous;
        split.tabs = cpu
            .tabs
            .iter()
            .map(|t| (t.key, (t.charge * 1000.0).round() as i64))
            .collect();
        split.tab_floors = cpu
            .tabs
            .iter()
            .map(|t| (t.key, (t.floor * 1000.0).floor() as i64))
            .collect();
        split.ambiguous_tabs = cpu
            .tabs
            .iter()
            .filter(|t| t.ambiguous)
            .map(|t| t.key)
            .collect();
        split.short_tabs = cpu
            .tabs
            .iter()
            .filter(|t| t.missed || cpu.missed)
            .map(|t| t.key)
            .collect();
    }
    // Unknown services and app work stay in the residual. Only independently observed
    // other-user process CPU belongs to the services floor.
    split.unaccounted_mc = split.global_mc - split.aterm_mc - split.others_mc - split.services_mc;
    Some(WhyFacts {
        at_ns: snap.at_ns,
        at_unix_ms: snap.at_unix_ms,
        window_ns: snap
            .scoped_cpu
            .as_ref()
            .map(|cpu| cpu.window_ns)
            .or(snap.procs_window_ns)?,
        lane,
        split,
        procs_truncated: snap.procs_truncated,
        provenance_incomplete: s.native_incomplete
            || s.native_source_lost
            || s.ownership_incomplete
            || !s.unproved_tabs.is_empty(),
        memory_provenance_incomplete: s.native_incomplete
            || s.ownership_incomplete
            || !s.unproved_tabs.is_empty(),
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
