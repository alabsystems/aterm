// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHY: who uses the machine's CPU, read per resource coalition
//! (`docs/DESIGN-resource-details-2026-10-20.md`, "RD"; SA below is
//! `docs/DESIGN-system-awareness-2026-10-16.md`), the facts the cause engine
//! and `sys why` read.
//!
//! The host reads the kernel's coalitions, each coalition lead's identity and
//! the machine's timeline (`aterm-sysprobe`) on split ticks only and hands
//! them over as plain numbers in [`Sample`]; [`Vitals::absorb`] makes one
//! [`WhyFacts`] per split tick that has a window, and every snapshot after it
//! carries those facts until the next.
//!
//! * [`split()`]: the machine's busy CPU over ONE matched window, the last
//!   split tick to this one, in five parts that add up exactly: the caller's
//!   tab, the rest of aterm (its coalitions `C(I)`, SA §7.1), others
//!   (coalitions holding a live process of this user), services (the rest),
//!   and a signed residual (work no listed coalition read: kernel threads, a
//!   coalition reaped inside the window). A delegated job's members under a
//!   launcher outside aterm count in their tab, not in their launcher, capped
//!   at the launcher's own figure (RD §14.3).
//! * [`name`] and [`leads`]: a coalition is named by its earliest-born live
//!   member, never its lowest pid (macOS reuses pids after 99,999), and one
//!   with no member left keeps its last name and part (RD §2.4).
//! * With the lane off (Linux and Windows have no coalitions; macOS's launch
//!   self-check missed), the facts are the built split by subtraction
//!   ([`crate::Split`]), [`Via::Subtraction`], each whole reading's: none
//!   after one that made no split.
//! * [`detect()`]: the causes that hold over each split tick's window; the
//!   facts carry them, the episodes open after them ([`Episodes`]) and the
//!   Deck's clause ([`ClauseHold`]).
//! * [`account()`]: one subject's account for a caller's tab, its `you` role
//!   among it, the one reading `sys why` and the CPU close-up render; and
//!   [`bare_subject`], what a bare `sys why` answers for.
//!
//! Pure: no clock, no platform, no I/O.
//!
//! [`Sample`]: crate::Sample
//! [`Vitals::absorb`]: crate::Vitals::absorb

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::model::{Membership, Sample, Snapshot, TabRoot};

pub mod account;
pub mod detect;
pub mod episodes;
pub mod split;

#[cfg(any(test, debug_assertions))]
pub mod fixtures;

pub use account::{Account, Bare, Role, Shown, WhySubject, You, account, bare_subject, you_role};
pub use detect::{
    ACTIVITIES, Activity, ActivityId, Candidate, CauseId, Context, Evidence, Term, WakeFact, detect,
};
pub use episodes::{ClauseHold, DeckCause, Episodes, OpenCause, deck_cause};
pub use split::{CoalBase, SplitIn, WINDOW_MAX_NS, split};

/// Whether the coalition lane reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CoalLane {
    /// This OS has no coalitions (Linux, Windows): the split is by
    /// subtraction.
    #[default]
    Unsupported,
    /// macOS answered the launch self-check: the split is by coalition.
    On,
    /// macOS's self-check missed, or a read did not read as the layout the
    /// readers hold: the split is by subtraction.
    Off,
}

impl CoalLane {
    /// Whether the split is by coalition.
    #[must_use]
    pub fn on(self) -> bool {
        self == Self::On
    }

    /// The unavailable word a reply prints for the coalitions:
    /// `lane_off:coalitions` on every OS whose lane does not read (the built
    /// views' word, D39), or `None` while it reads.
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
    /// [`Self::cpu_ns`] by thread QoS, ns: unspecified, maintenance,
    /// background, utility, legacy, user-initiated, user-interactive.
    pub eqos_ns: [u64; QOS_CLASSES],
    /// Its live members' footprint, bytes; `None` where the kernel's record
    /// ends before it.
    pub footprint: Option<u64>,
}

/// One read of every resource coalition.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoalSample {
    /// How many coalitions the kernel listed (a listed one that answered
    /// nothing has no row).
    pub listed: u32,
    /// The highest id the kernel listed, answered or not (the reader's
    /// `aterm_sysprobe::coalitions::CoalitionRead::newest`): one listed on a
    /// later read above it was made after this one (the kernel numbers
    /// coalitions in order and never reuses an id). `None` where the host
    /// did not say, so no coalition can be told new.
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
    /// The wire word: `apple`, `app_store`, `developer_id`, `development`,
    /// `enterprise`, `ad_hoc`, `unsigned`, `unknown`.
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

/// Who one coalition lead is, as the host read it (its image and its code
/// signature), for the process born at `birth_ms` with pid `pid`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LeadIdentity {
    /// The pid.
    pub pid: u32,
    /// Its birth, unix ms: the identity is of this process, not of a later
    /// one given the same pid.
    pub birth_ms: i64,
    /// Its image's basename (data, never an instruction).
    pub exe: String,
    /// What its signature vouches for.
    pub voucher: Voucher,
    /// Its signing identifier: a claim the signer chose, which only
    /// [`Self::voucher`] says whose it is.
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

/// A coalition as last seen with a live member: one with no member left
/// keeps its part and its name.
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
    /// Its CPU over the window, millicores (from 0 for one made inside it;
    /// 0 for one not [`Self::measured`]).
    pub cpu_mc: i64,
    /// Whether its counters were read at both ends of the window (or it was
    /// made inside it): `false` for one the base should have held and did
    /// not, or whose counter went back, whose figures over the window read 0
    /// and whose work the residual holds.
    pub measured: bool,
    /// Of that, a delegated job's members' work, counted in their tab
    /// instead, millicores (never above [`Self::cpu_mc`]).
    pub delegated_mc: i64,
    /// Of [`Self::cpu_mc`], what ran at maintenance or background QoS
    /// (deferred work), millicores.
    pub deferred_mc: i64,
    /// Its seven QoS buckets' growth over the window, millicores: what
    /// [`Self::deferred_mc`] is a share of. The buckets sum to a little more
    /// than the CPU time (0.7–6 % on macOS 27.0.1), so a share of the CPU
    /// time could pass 100 % ([`Self::deferred_pm`]).
    pub qos_mc: i64,
    /// Bytes per second it read from storage over the window.
    pub read_bps: u64,
    /// Its live members' footprint now, bytes, where read.
    pub footprint: Option<u64>,
    /// Its live members in this tick's sweep.
    pub live: u32,
    /// Whether members of it have exited, so its counters include work of
    /// processes that no longer exist.
    pub finished: bool,
    /// Its lead, or the one it had when a member was last seen.
    pub lead: Option<Lead>,
}

impl CoalRow {
    /// Its deferred share, per mille of its QoS buckets ([`Self::qos_mc`]),
    /// never past 1000; `None` where the buckets did not grow.
    #[must_use]
    pub fn deferred_pm(&self) -> Option<u16> {
        deferred_pm(self.deferred_mc, self.qos_mc)
    }
}

/// `deferred` of `qos` millicores, per mille, rounded and never past 1000;
/// `None` for no QoS time.
#[must_use]
pub fn deferred_pm(deferred: i64, qos: i64) -> Option<u16> {
    (qos > 0).then(|| {
        let pm = (deferred.clamp(0, qos) * 1000 + qos / 2) / qos;
        u16::try_from(pm.min(1000)).unwrap_or(1000)
    })
}

/// The busy CPU over one matched window, millicores: every figure is over the
/// same window, and `global = aterm + delegated + others + services +
/// unaccounted` exactly.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoalSplit {
    /// How it was made.
    pub via: Via,
    /// Whether every coalition was read and every process swept at both ends
    /// of the window, so every row is measured (`exact`), or not
    /// (`heuristic`, as every split by subtraction is).
    pub exact: bool,
    /// Whether the kernel's list of coalitions stopped short at either end
    /// of the window (`cannot what=coalition_list why=truncated`).
    pub list_truncated: bool,
    /// Coalitions the kernel listed (0 by subtraction).
    pub listed: u32,
    /// The machine's busy figure (the cores' own ticks).
    pub global_mc: i64,
    /// aterm's coalitions.
    pub aterm_mc: i64,
    /// A delegated job's members' work under launchers outside aterm, moved
    /// to aterm's side (0 by subtraction, where aterm already holds it).
    pub delegated_mc: i64,
    /// The user's other coalitions, less the delegated work.
    pub others_mc: i64,
    /// Every other coalition, less the delegated work; by subtraction, the
    /// busy figure less aterm and others, never below 0.
    pub services_mc: i64,
    /// What no part holds, signed and never clamped.
    pub unaccounted_mc: i64,
    /// Each tab's live work over the window, by key, in the host's order:
    /// its processes alive at both ends of it, delegated members included.
    pub tabs: Vec<(u64, i64)>,
    /// The live work over the window of aterm's processes in no tab: itself,
    /// its helpers, and a delegated job's members whose tab closed (so it is
    /// of [`Self::aterm_mc`] plus [`Self::delegated_mc`], not of aterm's
    /// coalitions alone).
    pub own_mc: i64,
    /// Every listed coalition that answered, busiest first (none by
    /// subtraction), each rounded on its own: a part's rows may not sum to
    /// the part, which is rounded once.
    pub rows: Vec<CoalRow>,
}

/// The split's five parts with one tab's work taken out of aterm's,
/// millicores: they add up to [`CoalSplit::global_mc`] exactly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Parts {
    /// The tab's live work.
    pub tab: i64,
    /// aterm's coalitions and the delegated work, less the tab's.
    pub aterm_rest: i64,
    /// The user's other work.
    pub others: i64,
    /// Services.
    pub services: i64,
    /// The signed residual: the split's, plus the tab's live figure past
    /// aterm's, where it ran ahead.
    pub unaccounted: i64,
}

impl CoalSplit {
    /// The parts with tab `tab`'s work (the caller's) out of aterm's: none
    /// for no tab, or one the split did not read. A rest of aterm below 0
    /// (the tab's live figure read ahead of aterm's counters) is 0, and the
    /// residual takes it, signed.
    #[must_use]
    pub fn parts(&self, tab: Option<u64>) -> Parts {
        let tab = tab
            .and_then(|key| self.tabs.iter().find(|(k, _)| *k == key))
            .map_or(0, |(_, mc)| *mc);
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

/// What one split tick read: the split, its window, and the machine's
/// timeline. Every snapshot carries the last ones (`Snapshot::why`).
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
    /// Whether the split tick's sweep stopped at its cap, so each tab's
    /// figure may be short of its work.
    pub procs_truncated: bool,
    /// The machine's timeline, where read.
    pub timeline: Option<TimelineSample>,
    /// When userspace started, unix ms: the earliest birth among the live
    /// processes above pid 1 (launchd outlives a userspace restart), where
    /// births were read and the sweep was whole.
    pub userspace_ms: Option<i64>,
    /// This aterm's user, where read.
    pub self_uid: Option<u32>,
    /// The causes that hold over the window, ranked ([`detect()`]).
    pub causes: Vec<Candidate>,
    /// The episodes open after this tick, ranked the same way: covering
    /// causes only, since a context keeps no episode.
    pub open: Vec<OpenCause>,
    /// The Deck's cause clause after this tick ([`deck_cause`] reads it).
    pub clause: Option<DeckCause>,
}

/// A defect the negative controls inject into the real split or its cause
/// engine (`tests/conformance_scope_balance.rs`,
/// `tests/conformance_explain.rs`), through
/// [`Vitals::with_fault`](crate::Vitals::with_fault).
#[cfg(any(test, debug_assertions))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fault {
    /// With the lane on, services takes the positive residual, as the split
    /// by subtraction does.
    SplitBySubtraction,
    /// The tab's figure is its last sweep's rate (a 1 s window) against the
    /// split's window.
    TabOverProcsWindow,
    /// A named activity's members are matched by signing id alone, under
    /// any voucher (`tests/conformance_explain.rs`).
    MatchBySigningIdOnly,
    /// The Deck's clause shows and goes with its candidate, with no hold.
    NoHold,
}

#[cfg(any(test, debug_assertions))]
impl Fault {
    /// `facts` with this defect in their split by coalition (`snap` the
    /// snapshot they are of); facts by subtraction as they are. The cause
    /// engine's defects act in `WhyState::explain`, not here.
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
            Self::MatchBySigningIdOnly | Self::NoHold => {}
        }
        facts
    }
}

/// Each coalition's lead among `members` (`(pid, coalition, birth_ms)`, every
/// live process whose coalition and birth were read): its earliest-born
/// member, ties to the lower pid, never pid 0. As `(coalition, pid)`, by
/// coalition.
#[must_use]
pub fn leads(members: &[(u32, u64, i64)]) -> Vec<(u64, u32)> {
    let mut first: HashMap<u64, (i64, u32)> = HashMap::new();
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

/// Name `read`'s coalitions in `names` from this tick's sweep `procs`: each
/// one with a live member takes its part (aterm's for one holding this
/// instance, `self_pid`, or a tab's shell, so a tab adopted from a
/// predecessor counts; others for one holding a process of user `self_uid`;
/// else services) and its lead, with the identity `identities` hold for that
/// pid and birth. One with no live member keeps what it had; one with no row
/// in `read` (gone, or listed and answering nothing this time) is forgotten.
/// Each coalition's live members, by id.
pub fn name(
    names: &mut HashMap<u64, Named>,
    read: &CoalSample,
    procs: &[crate::model::ProcSample],
    self_pid: u32,
    self_uid: u32,
    tabs: &[TabRoot],
    identities: &HashMap<(u32, i64), LeadIdentity>,
) -> HashMap<u64, u32> {
    let roots: HashSet<u32> = std::iter::once(self_pid)
        .chain(tabs.iter().map(|t| t.shell_pid).filter(|p| *p != 0))
        .collect();
    let listed: HashSet<u64> = read.rows.iter().map(|r| r.id).collect();
    let mut live: HashMap<u64, u32> = HashMap::new();
    let mut parts: HashMap<u64, Part> = HashMap::new();
    let mut members = Vec::new();
    let mut born: HashMap<u32, (i64, Option<u32>)> = HashMap::new();
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
        let lead = identities.get(&(pid, birth_ms)).map_or(
            Lead {
                pid,
                birth_ms,
                uid,
                ..Lead::default()
            },
            |id| Lead {
                pid,
                birth_ms,
                uid,
                exe: Some(id.exe.clone()),
                voucher: id.voucher,
                signing_id: id.signing_id.clone(),
                team: id.team.clone(),
            },
        );
        names.insert(
            c,
            Named {
                part,
                lead: Some(lead),
            },
        );
    }
    // Live members none of whose births were read: the part, no name.
    for (c, part) in parts.into_iter().filter(|(c, _)| listed.contains(c)) {
        names.insert(c, Named { part, lead: None });
    }
    live
}

/// The facts' running state between split ticks: the base the next window
/// starts at, each coalition's last name, each lead identity read, and the
/// last facts.
#[derive(Debug, Default)]
pub(crate) struct WhyState {
    base: Option<CoalBase>,
    names: HashMap<u64, Named>,
    /// Each lead identity the host read, by pid and birth: the host sends
    /// one once, and it is kept while that process is swept (or, on a sweep
    /// cut at its cap, while it leads a listed coalition).
    identities: HashMap<(u32, i64), LeadIdentity>,
    facts: Option<Arc<WhyFacts>>,
    episodes: Episodes,
    clause: ClauseHold,
    /// Whether a read misread the record's layout ([`misread`]): the lane
    /// stays off for good, since the kernel's layout does not change under a
    /// running process.
    misread: bool,
    /// The defect injected into each split by coalition (negative controls
    /// only).
    #[cfg(any(test, debug_assertions))]
    pub(crate) fault: Option<Fault>,
}

impl WhyState {
    /// The lane sample `s` reads under: its host's word, or `Off` once a
    /// read misread the layout.
    pub(crate) fn lane(&self, s: &Sample) -> CoalLane {
        if s.coal_lane.on() && self.misread {
            CoalLane::Off
        } else {
            s.coal_lane
        }
    }

    /// The facts snapshot `snap` (of sample `s`) carries: new ones for a
    /// split tick with a window, else the last ones. `attribution` is the
    /// model's pass over `s`'s sweep: each process's scopes and, for a
    /// delegated job's member, its job (`None` where the pass made none).
    pub(crate) fn absorb(
        &mut self,
        s: &Sample,
        snap: &Snapshot,
        attribution: Option<(&[Membership], &[Option<usize>])>,
    ) -> Option<Arc<WhyFacts>> {
        self.misread = self.misread || (s.coal_lane.on() && misread(s));
        let lane = self.lane(s);
        // With the lane off no covering cause holds, so none stays open and
        // the clause never shows (RD §3.3): a misread that turns it off ends
        // every episode and the clause's hold at once.
        if !lane.on() {
            self.episodes = Episodes::default();
            self.clause = ClauseHold::default();
        }
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
            // A split tick that only set the base (the first, or the first
            // after a gap) evaluates nothing: it ends every episode and
            // resets the clause's hold, and the facts it carries hold none.
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
            // With the lane off the facts are each whole reading's split: one
            // that made none (an owner outside aterm unread) leaves none,
            // never the last one's figures as if they were now (RD §3.2).
            None if !lane.on() && s.procs.is_some() => self.facts = None,
            None => {}
        }
        self.facts.clone()
    }

    /// `facts` with the causes that hold over their window, the episodes
    /// open after it and the Deck's clause.
    fn explain(&mut self, mut facts: WhyFacts, s: &Sample, snap: &Snapshot) -> WhyFacts {
        #[cfg(any(test, debug_assertions))]
        let vouched = self.fault != Some(Fault::MatchBySigningIdOnly);
        #[cfg(any(test, debug_assertions))]
        {
            self.clause.no_hold = self.fault == Some(Fault::NoHold);
        }
        #[cfg(not(any(test, debug_assertions)))]
        let vouched = true;
        facts.causes = detect::detect_as(&facts, snap, vouched);
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
        self.identities
            .extend(s.leads.iter().map(|l| ((l.pid, l.birth_ms), l.clone())));
        let (Some(read), Some(procs), Some(uid)) =
            (s.coalitions.as_ref(), s.procs.as_deref(), s.self_uid)
        else {
            return None;
        };
        // A process gone, or another under its pid: its identity goes. A
        // sweep cut at its cap misses live processes, so there one not swept
        // keeps its identity while it leads a listed coalition: the map holds
        // no more than the sweep and the list.
        let swept: HashMap<u32, Option<i64>> = procs.iter().map(|p| (p.pid, p.birth_ms)).collect();
        let leading: HashSet<(u32, i64)> = if s.procs_truncated {
            self.names
                .values()
                .filter_map(|n| n.lead.as_ref().map(|l| (l.pid, l.birth_ms)))
                .collect()
        } else {
            HashSet::new()
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
        let now = CoalBase::of(s, read, procs, uid)?;
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
/// sum to before the read is a misread. A footprint counts each compressed
/// page at its full size, swapped out or not, so on a Mac under pressure the
/// sum legitimately passes physical memory; a counter read from another slot
/// of the record (cycles, instructions, energy) reads terabytes at once.
const MISREAD_X: u64 = 8;

/// Whether `s`'s coalitions hold more footprint than any machine of its
/// memory could ([`MISREAD_X`]): a record whose layout moved under the
/// readers. It catches a gross misread only; the launch self-check's
/// `cpu_time_eqos_len` is the layout's first guard.
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

/// The facts of the built split by subtraction, for snapshot `snap` (of
/// sample `s`) with the lane `lane`; `None` without a split.
fn by_subtraction(s: &Sample, snap: &Snapshot, lane: CoalLane) -> Option<WhyFacts> {
    let built = snap.split?;
    Some(WhyFacts {
        at_ns: snap.at_ns,
        at_unix_ms: snap.at_unix_ms,
        window_ns: snap.procs_window_ns?,
        lane,
        split: CoalSplit::by_subtraction(&built, &snap.tabs),
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
