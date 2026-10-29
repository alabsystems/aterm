// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CAUSES: what holds over one split window (RD §5.3–§5.4).

use std::collections::HashSet;

use super::{CoalRow, CoalSplit, Lead, Part, Via, Voucher, WhyFacts};
use crate::model::{MemView, Pressure, Snapshot, Thermal, Throttle};

/// A covering cause's floor, millicores (RD §5.3).
pub const FLOOR_MC: i64 = 500;
/// A covering cause's floor as a share of the busy figure, per mille.
pub const FLOOR_PM: i64 = 100;
/// `unexplained` holds from this much of one part's CPU no other cause covers, or of the split's unaccounted residual, millicores.
pub const UNEXPLAINED_MC: i64 = 1000;
/// `startup` holds while userspace is younger than this, ms.
pub const STARTUP_AGE_MS: i64 = 30 * 60 * 1000;
/// A lead born at most this long after userspace started is start-up's, ms.
pub const STARTUP_BORN_MS: i64 = 60 * 1000;
/// `wake` holds while the wake is younger than this, ms.
pub const WAKE_AGE_MS: i64 = 10 * 60 * 1000;
/// `wake` needs this much deferred work in the cause, millicores.
pub const WAKE_DEFERRED_MC: i64 = 1000;

/// A named activity's id: its wire word is [`Self::word`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ActivityId {
    /// Spotlight indexing.
    Spotlight,
}

impl ActivityId {
    /// The wire word: `spotlight`.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Spotlight => "spotlight",
        }
    }

    /// Its row of [`ACTIVITIES`].
    #[must_use]
    pub fn activity(self) -> &'static Activity {
        ACTIVITIES
            .iter()
            .find(|a| a.id == self)
            .unwrap_or(&ACTIVITIES[0])
    }
}

/// One term of an activity's predicate, over its members and the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Term {
    /// The members' CPU is at least this, millicores.
    CpuAtLeast(i64),
    /// They read at least this from storage, bytes per second.
    ReadAtLeast(u64),
    /// At least this share of their QoS time is deferred (maintenance or background), per mille.
    DeferredAtLeast(u16),
}

impl Term {
    fn holds(self, cpu_mc: i64, read_bps: u64, deferred_pm: Option<u16>) -> bool {
        match self {
            Self::CpuAtLeast(mc) => cpu_mc >= mc,
            Self::ReadAtLeast(bps) => read_bps >= bps,
            Self::DeferredAtLeast(pm) => deferred_pm.is_some_and(|d| d >= pm),
        }
    }
}

/// One named activity: macOS's own work with a name people know and no API that reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Activity {
    /// Its id.
    pub id: ActivityId,
    /// The people's word: `Spotlight`.
    pub word: &'static str,
    /// Its members' signing ids, matched only under [`Voucher::Apple`].
    pub members: &'static [&'static str],
    /// It holds when every term of one group holds.
    pub holds: &'static [&'static [Term]],
    /// What cannot be read about it, `(what, why)`: the `cannot` lines.
    pub cannot: &'static [(&'static str, &'static str)],
}

/// The named activities.
pub const ACTIVITIES: &[Activity] = &[Activity {
    id: ActivityId::Spotlight,
    word: "Spotlight",
    members: &[
        "com.apple.mds",
        "com.apple.mds_stores",
        "com.apple.mdworker_shared",
        "com.apple.corespotlightd",
    ],
    holds: &[
        &[Term::CpuAtLeast(200), Term::DeferredAtLeast(500)],
        &[Term::ReadAtLeast(5_000_000), Term::DeferredAtLeast(500)],
    ],
    cannot: &[
        ("index_progress", "unsupported"),
        ("index_folder", "needs_privilege:root"),
    ],
}];

/// A context: it covers nothing and says when or under what a covering cause runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Context {
    /// Shortly after userspace started.
    Startup,
    /// Shortly after a wake.
    Wake,
    /// The machine is throttled (thermal, or Low Power Mode).
    Throttled,
    /// Memory pressure is at warn or above.
    Pressure,
}

impl Context {
    /// The order a headline takes its one context in.
    pub const ORDER: [Self; 4] = [Self::Startup, Self::Wake, Self::Throttled, Self::Pressure];

    /// The wire word.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::Wake => "wake",
            Self::Throttled => "throttled",
            Self::Pressure => "pressure",
        }
    }
}

/// A cause, by what it is: the key its episode is kept under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CauseId {
    /// A named activity.
    Activity(ActivityId),
    /// A tab's work (its panes' summed), by its key ([`crate::model::TabRoot::tab`]).
    TabWork(u64),
    /// aterm itself and its helpers.
    AtermWork,
    /// An app's coalition, by id.
    AppWork(u64),
    /// Work no other cause covers.
    Unexplained,
    /// A context.
    Context(Context),
}

impl CauseId {
    /// The wire word.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Activity(a) => a.word(),
            Self::TabWork(_) => "tab-work",
            Self::AtermWork => "aterm-work",
            Self::AppWork(_) => "app-work",
            Self::Unexplained => "unexplained",
            Self::Context(c) => c.word(),
        }
    }

    /// Whether it covers part of the busy figure (every cause but a context).
    #[must_use]
    pub fn covering(self) -> bool {
        !matches!(self, Self::Context(_))
    }

    /// Whether `startup` and `wake` can hold for it.
    #[must_use]
    pub fn takes_start(self) -> bool {
        matches!(self, Self::Activity(_) | Self::Unexplained)
    }
}

/// A wake, as the timeline read it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WakeFact {
    /// When, unix ms.
    pub at_ms: i64,
    /// The word it is named by.
    pub reason: Option<String>,
    /// How long the machine slept before it, s.
    pub slept_s: Option<u64>,
}

/// What a cause holds on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Evidence {
    /// A named activity's members: their deferred share and their reads.
    Activity {
        /// Deferred, per mille of their QoS time.
        deferred_pm: Option<u16>,
        /// Read from storage, bytes per second.
        read_bps: u64,
    },
    /// Work measured per tab, per process or per coalition: what it covers.
    Measured,
    /// Work of one part no other cause covers.
    Uncovered {
        /// That part: `aterm`, `others` or `services`.
        part: Part,
    },
    /// Work no coalition held over the window: the split's unaccounted
    /// residual (a coalition reaped inside the window, one a truncated list
    /// left out), which no part and no row holds, so nothing covers it.
    Unaccounted,
    /// Userspace's start.
    Startup {
        /// When it started, unix ms.
        userspace_ms: i64,
        /// The wake before it, where read.
        wake: Option<WakeFact>,
    },
    /// The last wake.
    Wake(WakeFact),
    /// The memory pressure.
    Pressure {
        /// Its level.
        level: Pressure,
        /// Swap traffic, bytes per second, where read.
        swap_bps: Option<u64>,
    },
    /// The throttle.
    Throttled {
        /// The thermal level.
        level: Throttle,
        /// Low Power Mode, where the OS says.
        low_power: Option<bool>,
    },
}

/// A cause that holds over one window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// What it is.
    pub id: CauseId,
    /// The busy figure it covers, millicores (0 for a context).
    pub covers_mc: i64,
    /// The coalitions it covers, busiest first.
    pub coalitions: Vec<u64>,
    /// When its work began, by the evidence (unix ms).
    pub since_ms: Option<i64>,
    /// What it holds on.
    pub evidence: Evidence,
    /// The contexts that hold for it, in [`Context::ORDER`].
    pub contexts: Vec<Context>,
}

impl Candidate {
    /// The sources of its evidence, in order: `src=`.
    #[must_use]
    pub fn src(&self) -> &'static [&'static str] {
        match self.id {
            CauseId::Activity(_) => &["signature", "kernel"],
            CauseId::Unexplained => &["derived"],
            _ => &["kernel"],
        }
    }
}

/// Whether `covers_mc` passes the floor of a subject `busy_mc` busy.
#[must_use]
pub fn above_floor(covers_mc: i64, busy_mc: i64) -> bool {
    covers_mc >= FLOOR_MC
        && i128::from(covers_mc) * 1000 >= i128::from(busy_mc) * i128::from(FLOOR_PM)
}

/// The machine's throttle, where it holds: thermal at `light` or above, or Low Power Mode.
pub(crate) fn throttle(snap: &Snapshot) -> Option<Thermal> {
    snap.thermal
        .filter(|t| t.level >= Throttle::Light || t.low_power == Some(true))
}

/// Memory, where its pressure is at warn or above: `pressure` holds.
pub(crate) fn pressed(snap: &Snapshot) -> Option<&MemView> {
    snap.mem
        .as_ref()
        .filter(|m| m.sample.pressure.rank().is_some_and(|r| r >= 1))
}

/// The machine's own contexts that hold in `snap`, in [`Context::ORDER`].
pub(crate) fn machine(snap: &Snapshot) -> Vec<Candidate> {
    let context = |c: Context, evidence: Evidence| Candidate {
        id: CauseId::Context(c),
        covers_mc: 0,
        coalitions: Vec::new(),
        since_ms: None,
        evidence,
        contexts: Vec::new(),
    };
    let mut out = Vec::new();
    if let Some(t) = throttle(snap) {
        out.push(context(
            Context::Throttled,
            Evidence::Throttled {
                level: t.level,
                low_power: t.low_power,
            },
        ));
    }
    if let Some(m) = pressed(snap) {
        out.push(context(
            Context::Pressure,
            Evidence::Pressure {
                level: m.sample.pressure,
                swap_bps: m.rates.map(|r| r.swap_bps().max(0.0).round() as u64),
            },
        ));
    }
    out
}

/// Whether `lead` is a member of `activity`: its signing id is one of the
/// row's and, where `vouched`, Apple vouches for it.
fn member(lead: &Lead, activity: &Activity, vouched: bool) -> bool {
    (!vouched || lead.voucher == Voucher::Apple)
        && lead
            .signing_id
            .as_deref()
            .is_some_and(|id| activity.members.contains(&id))
}

/// A row's figure in its part: a launcher's less the delegated work its tab
/// counts.
fn covered(r: &CoalRow) -> i64 {
    if r.part == Part::Aterm {
        r.cpu_mc
    } else {
        r.cpu_mc - r.delegated_mc
    }
}

/// The causes that hold over `facts`' window.
#[must_use]
pub fn detect(facts: &WhyFacts, snap: &Snapshot) -> Vec<Candidate> {
    detect_as(facts, snap, true, true)
}

/// [`detect`], matching activities' signing ids under Apple's voucher only
/// where `vouched`, and letting `unexplained` cover the split's unaccounted
/// residual only where `residual`.
pub(crate) fn detect_as(
    facts: &WhyFacts,
    snap: &Snapshot,
    vouched: bool,
    residual: bool,
) -> Vec<Candidate> {
    let split = &facts.split;
    let mut out = if split.via == Via::Coalitions {
        covering(facts, vouched, residual)
    } else {
        Vec::new()
    };
    let mut start = None;
    let mut woke = None;
    for c in &mut out {
        if c.id.takes_start() {
            if let Some(e) = startup(facts, c) {
                start.get_or_insert(e);
                c.contexts.push(Context::Startup);
            } else if let Some(w) = wake(facts, c) {
                woke.get_or_insert(w);
                c.contexts.push(Context::Wake);
            }
        }
    }
    let context = |c: Context, since_ms: Option<i64>, evidence: Evidence| Candidate {
        id: CauseId::Context(c),
        covers_mc: 0,
        coalitions: Vec::new(),
        since_ms,
        evidence,
        contexts: Vec::new(),
    };
    if let Some(e) = start {
        let since = match &e {
            Evidence::Startup { userspace_ms, .. } => Some(*userspace_ms),
            _ => None,
        };
        out.push(context(Context::Startup, since, e));
    }
    if let Some(w) = woke {
        out.push(context(Context::Wake, None, Evidence::Wake(w)));
    }
    with_machine(out, snap)
}

/// `causes`, ranked as [`detect()`] ranks them, with the machine's contexts as `snap` reads them.
pub(crate) fn with_machine(mut causes: Vec<Candidate>, snap: &Snapshot) -> Vec<Candidate> {
    let of_machine = |c: Context| matches!(c, Context::Throttled | Context::Pressure);
    causes.retain(|c| !matches!(c.id, CauseId::Context(x) if of_machine(x)));
    let machine = machine(snap);
    let held: Vec<Context> = machine
        .iter()
        .filter_map(|m| match m.id {
            CauseId::Context(c) => Some(c),
            _ => None,
        })
        .collect();
    for c in causes.iter_mut().filter(|c| c.id.covering()) {
        c.contexts.retain(|x| !of_machine(*x));
        c.contexts.extend_from_slice(&held);
    }
    causes.extend(machine);
    causes
}

/// The covering causes of a split by coalition, busiest first; `unexplained`
/// takes the split's unaccounted residual where `residual` and it is larger
/// than every part's uncovered CPU, so work no coalition held past a core is
/// never left uncovered.
fn covering(facts: &WhyFacts, vouched: bool, residual: bool) -> Vec<Candidate> {
    let split = &facts.split;
    let busy = split.global_mc;
    let mut out = Vec::new();
    let named = |r: &CoalRow, a: &Activity| r.lead.as_ref().is_some_and(|l| member(l, a, vouched));
    for a in ACTIVITIES {
        let rows: Vec<&CoalRow> = split.rows.iter().filter(|r| named(r, a)).collect();
        let cpu: i64 = rows.iter().map(|r| covered(r)).sum();
        let read: u64 = rows.iter().fold(0, |s, r| s.saturating_add(r.read_bps));
        let deferred = super::deferred_pm(
            rows.iter().map(|r| r.deferred_mc).sum(),
            rows.iter().map(|r| r.qos_mc).sum(),
        );
        let holds = a
            .holds
            .iter()
            .any(|group| group.iter().all(|t| t.holds(cpu, read, deferred)));
        if holds && above_floor(cpu, busy) {
            out.push(Candidate {
                id: CauseId::Activity(a.id),
                covers_mc: cpu,
                coalitions: rows.iter().map(|r| r.id).collect(),
                since_ms: rows
                    .iter()
                    .filter_map(|r| r.lead.as_ref())
                    .map(|l| l.birth_ms)
                    .min(),
                evidence: Evidence::Activity {
                    deferred_pm: deferred,
                    read_bps: read,
                },
                contexts: Vec::new(),
            });
        }
    }
    let measured = |id: CauseId, covers_mc: i64| Candidate {
        id,
        covers_mc,
        coalitions: Vec::new(),
        since_ms: None,
        evidence: Evidence::Measured,
        contexts: Vec::new(),
    };
    for &(key, mc) in &split.tabs {
        if above_floor(mc, busy) {
            out.push(measured(CauseId::TabWork(key), mc));
        }
    }
    let own = split.own_mc;
    let largest = [split.others_mc, split.services_mc, split.unaccounted_mc]
        .into_iter()
        .chain(split.tabs.iter().map(|t| t.1))
        .all(|part| own > part);
    if largest && above_floor(own, busy) {
        out.push(measured(CauseId::AtermWork, own));
    }
    let app = split
        .rows
        .iter()
        .filter(|r| r.part != Part::Aterm)
        .max_by(|a, b| covered(a).cmp(&covered(b)).then(b.id.cmp(&a.id)));
    if let Some(r) = app.filter(|r| {
        r.lead
            .as_ref()
            .is_some_and(|l| l.uid.is_some() && l.uid == facts.self_uid)
            && !ACTIVITIES.iter().any(|a| named(r, a))
            && above_floor(covered(r), busy)
    }) {
        out.push(Candidate {
            coalitions: vec![r.id],
            since_ms: r.lead.as_ref().map(|l| l.birth_ms),
            ..measured(CauseId::AppWork(r.id), covered(r))
        });
    }
    let taken: HashSet<u64> = out.iter().flat_map(|c| c.coalitions.clone()).collect();
    let (part, left) = uncovered(split, &out, &taken);
    let lost = split.unaccounted_mc;
    if residual && lost > left {
        if lost >= UNEXPLAINED_MC && above_floor(lost, busy) {
            out.push(Candidate {
                evidence: Evidence::Unaccounted,
                ..measured(CauseId::Unexplained, lost)
            });
        }
    } else if left >= UNEXPLAINED_MC && above_floor(left, busy) {
        out.push(Candidate {
            coalitions: split
                .rows
                .iter()
                .filter(|r| part != Part::Aterm && r.part == part && !taken.contains(&r.id))
                .map(|r| r.id)
                .collect(),
            evidence: Evidence::Uncovered { part },
            ..measured(CauseId::Unexplained, left)
        });
    }
    out.sort_by_key(|c| std::cmp::Reverse(c.covers_mc));
    out
}

/// The part (aterm's, others', services') holding the most CPU the covering
/// causes `covered_by` leave, and how much: a tab's or aterm's own work comes
/// out of aterm's, a covered coalition's (one of `taken`) out of its part.
fn uncovered(split: &CoalSplit, covered_by: &[Candidate], taken: &HashSet<u64>) -> (Part, i64) {
    let mut aterm = split.aterm_mc + split.delegated_mc;
    let (mut others, mut services) = (split.others_mc, split.services_mc);
    for c in covered_by {
        if matches!(c.id, CauseId::TabWork(_) | CauseId::AtermWork) {
            aterm -= c.covers_mc;
        }
    }
    for r in split.rows.iter().filter(|r| taken.contains(&r.id)) {
        match r.part {
            Part::Aterm => aterm -= covered(r),
            Part::Others => others -= covered(r),
            Part::Services => services -= covered(r),
        }
    }
    [
        (Part::Services, services),
        (Part::Others, others),
        (Part::Aterm, aterm),
    ]
    .into_iter()
    .fold((Part::Services, i64::MIN), |best, (part, mc)| {
        if mc > best.1 { (part, mc) } else { best }
    })
}

/// The rows `c` covers: its coalitions.
fn rows_of<'a>(facts: &'a WhyFacts, c: &Candidate) -> impl Iterator<Item = &'a CoalRow> + 'a {
    let ids: HashSet<u64> = c.coalitions.iter().copied().collect();
    facts.split.rows.iter().filter(move |r| ids.contains(&r.id))
}

/// `startup` for `c`: userspace under [`STARTUP_AGE_MS`] old, and at least
/// half of `c`'s CPU from coalitions whose lead was born within
/// [`STARTUP_BORN_MS`] of its start, weighted by CPU (never by count).
fn startup(facts: &WhyFacts, c: &Candidate) -> Option<Evidence> {
    let start = facts.userspace_ms?;
    let age = facts.at_unix_ms.checked_sub(start)?;
    if !(0..STARTUP_AGE_MS).contains(&age) || c.covers_mc <= 0 {
        return None;
    }
    let born_end = start.saturating_add(STARTUP_BORN_MS);
    let early: i64 = rows_of(facts, c)
        .filter(|r| {
            r.lead
                .as_ref()
                .is_some_and(|l| (start..=born_end).contains(&l.birth_ms))
        })
        .map(covered)
        .fold(0_i64, i64::saturating_add);
    (i128::from(early) * 2 >= i128::from(c.covers_mc)).then(|| Evidence::Startup {
        userspace_ms: start,
        wake: wake_fact(facts),
    })
}

/// `wake` for `c` (with no `startup`): the wake under [`WAKE_AGE_MS`] old
/// and at least [`WAKE_DEFERRED_MC`] of deferred work in `c`.
fn wake(facts: &WhyFacts, c: &Candidate) -> Option<WakeFact> {
    let w = wake_fact(facts).filter(|w| {
        facts
            .at_unix_ms
            .checked_sub(w.at_ms)
            .is_some_and(|age| (0..WAKE_AGE_MS).contains(&age))
    })?;
    let deferred: i64 = rows_of(facts, c)
        .map(|r| r.deferred_mc)
        .fold(0_i64, i64::saturating_add);
    (deferred >= WAKE_DEFERRED_MC).then_some(w)
}

/// The last wake, as the timeline read it.
fn wake_fact(facts: &WhyFacts) -> Option<WakeFact> {
    let t = facts.timeline.as_ref()?;
    let at_ms = t.wake_ms?;
    Some(WakeFact {
        at_ms,
        reason: t.wake_reason.clone(),
        slept_s: t
            .sleep_ms
            .and_then(|s| at_ms.checked_sub(s))
            .filter(|d| *d >= 0)
            .and_then(|d| u64::try_from(d / 1000).ok()),
    })
}
