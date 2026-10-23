// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CPU and memory attribution for `sys why` ([`account`], [`you_role`], [`bare_subject`]).

use std::sync::Arc;

use super::detect::{self, Candidate, CauseId, Context, FLOOR_PM, above_floor, throttle};
use super::{CoalLane, Parts, WhyFacts};
use crate::model::Snapshot;

/// Minimum memory footprint in bytes (alongside [`FLOOR_PM`]) for a role above `minor`.
pub const MEM_FLOOR_BYTES: u64 = 1 << 30;
/// Busy below this share of the cores is calm, per mille (the `quiet` preset's line).
pub const CALM_PM: i64 = 250;
/// A covering cause this large makes the CPU the bare answer, millicores.
pub const BARE_CAUSE_MC: i64 = 1000;

/// What a `sys why` reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhySubject {
    /// The machine's CPU.
    Cpu,
    /// The machine's memory.
    Mem,
}

/// Subject selected by `sys why` when none is specified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bare {
    /// A subject's full reply.
    Subject(WhySubject),
    /// Calm: a sentence and the `you` line.
    Calm,
}

/// A tab's role in a figure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// At least half, the largest part, above the floor.
    Main,
    /// At least a fifth, or the largest part, above the floor.
    Part,
    /// Some, under a fifth, or below the floor.
    Minor,
    /// None of it.
    None,
    /// Not read, with the unavailable word.
    Unavailable(&'static str),
}

impl Role {
    /// The wire word (`main`, `part`, `minor`, `none`), or the unavailable word.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Part => "part",
            Self::Minor => "minor",
            Self::None => "none",
            Self::Unavailable(w) => w,
        }
    }
}

/// The caller tab's share of the figure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct You {
    /// The tab, by the host's key; `None` for a caller in no tab.
    pub tab: Option<u64>,
    /// Its role.
    pub role: Role,
    /// Whether it is `minor` only because the figure is below the floor (a calm machine).
    pub calm: bool,
    /// Its figure: CPU millicores (live), or footprint bytes; `None` where unread.
    pub figure: Option<i64>,
    /// That of the subject's figure, per mille.
    pub share_pm: Option<u16>,
    /// On the CPU, for a caller in a tab, while there is any ([`Self::figure`]).
    pub unattributed_mc: Option<i64>,
}

impl You {
    /// Returns the human-readable tab attribution clause for this role.
    #[must_use]
    pub fn clause(&self) -> &'static str {
        match self.role {
            Role::Main => "this tab is the main cause",
            Role::Part => "this tab is part of the cause",
            Role::Minor => "this tab is not the cause",
            Role::None => "this tab uses none of it",
            Role::Unavailable(_) if self.figure.is_some() && self.unattributed_mc.is_none() => {
                "whether this tab is the largest part is not read now"
            }
            Role::Unavailable(_) => "this tab's share is not read now",
        }
    }
}

/// A cause that holds over the window, as a reply prints it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    /// The cause.
    pub cause: Candidate,
    /// When its episode opened, unix ms.
    pub opened_unix_ms: Option<i64>,
}

/// One subject's account for one caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    /// The subject.
    pub subject: WhySubject,
    /// The facts it reads; `None` before the first split tick, and where no split can be made.
    pub facts: Option<Arc<WhyFacts>>,
    /// The CPU's five parts with the caller's tab out of aterm's (CPU only) ([`Self::facts`]).
    pub parts: Option<Parts>,
    /// The caller tab's role.
    pub you: You,
    /// The causes that hold over the window, ranked.
    pub causes: Vec<Shown>,
    /// The headline: the top covering cause and at most one context that holds for it.
    pub headline: Option<(CauseId, Option<Context>)>,
    /// Whether nothing needs doing (`do class=none`) ([`bare_subject`], [`Self::cannot`]).
    pub nothing_needed: bool,
    /// What cannot be read, `(what, why)`, one per limit the reply touches.
    pub cannot: Vec<(&'static str, &'static str)>,
}

impl Account {
    /// Returns whether the CPU's tab share is a lower bound and the rest of aterm is an upper
    /// bound.
    #[must_use]
    pub fn bounded(&self, snap: &Snapshot) -> bool {
        let cut = snap.procs_truncated || self.facts.as_deref().is_some_and(|f| f.procs_truncated);
        cut || self.you.unattributed_mc.is_some_and(|u| u > 0) || self.parts.is_none()
    }

    /// Returns whether the CPU's tab figure is a lower bound (`tab_mc=≥`).
    #[must_use]
    pub fn tab_at_least(&self, snap: &Snapshot) -> bool {
        self.bounded(snap) || !snap.delegated.is_empty()
    }
}

fn per_mille(part: i64, whole: i64) -> Option<u16> {
    (whole > 0).then(|| {
        let pm = (part.max(0) * 1000 + whole / 2) / whole;
        u16::try_from(pm.min(1000)).unwrap_or(1000)
    })
}

/// Role of `fig` out of `whole` (`largest` indicates no other part is larger).
#[must_use]
pub fn you_role(fig: i64, whole: i64, largest: bool, above: bool) -> (Role, bool) {
    if fig <= 0 {
        return (Role::None, false);
    }
    let share = per_mille(fig, whole).map_or(1000, i64::from);
    if share < 200 && !largest {
        (Role::Minor, false)
    } else if !above {
        (Role::Minor, true)
    } else if share >= 500 && largest {
        (Role::Main, false)
    } else {
        (Role::Part, false)
    }
}

/// `subject`'s account for a caller in tab `you` (the host's key), from snapshot `snap`.
#[must_use]
pub fn account(subject: WhySubject, you: Option<u64>, snap: &Snapshot) -> Account {
    account_of(subject, you.as_slice(), snap)
}

/// Computes `subject`'s account for a caller tab spanning `panes` from snapshot `snap`.
#[must_use]
pub fn account_of(subject: WhySubject, panes: &[u64], snap: &Snapshot) -> Account {
    let facts = snap.why.clone();
    let lane = facts.as_ref().map_or(snap.coal_lane, |f| f.lane);
    let mut cannot = Vec::new();
    if let Some(w) = lane.word() {
        cannot.push(("coalitions", w));
    }
    if facts.as_ref().is_some_and(|f| f.split.list_truncated) {
        cannot.push(("coalition_list", "truncated"));
    }
    let (parts, you) = match subject {
        WhySubject::Cpu => cpu_you(facts.as_deref(), lane, panes, snap),
        WhySubject::Mem => (None, mem_you(panes, snap)),
    };
    if subject == WhySubject::Cpu && facts.is_some() && you.role == Role::Unavailable("not_sampled")
    {
        cannot.push(("finished_tab_work", "not_sampled"));
    }
    let open = facts.as_ref().map_or(&[][..], |f| f.open.as_slice());
    let held = match facts.as_deref() {
        Some(f) if subject == WhySubject::Cpu => detect::with_machine(f.causes.clone(), snap),
        _ => detect::machine(snap),
    };
    let causes: Vec<Shown> = held
        .iter()
        .filter(|c| subject == WhySubject::Cpu || c.id == CauseId::Context(Context::Pressure))
        .map(|c| Shown {
            cause: c.clone(),
            opened_unix_ms: open.iter().find(|o| o.id == c.id).map(|o| o.opened_unix_ms),
        })
        .collect();
    let first = causes.first().map(|s| (s.cause.id, None));
    let headline = match subject {
        WhySubject::Cpu => causes
            .iter()
            .find(|s| s.cause.id.covering())
            .map(|s| {
                let context = Context::ORDER
                    .into_iter()
                    .find(|c| s.cause.contexts.contains(c));
                (s.cause.id, context)
            })
            .or(first),
        WhySubject::Mem => first,
    };
    for s in &causes {
        if let CauseId::Activity(a) = s.cause.id {
            cannot.extend_from_slice(a.activity().cannot);
        }
    }
    if subject == WhySubject::Mem {
        cannot.push(("wired_by_process", "needs_privilege:root"));
        cannot.push(("uncharged_memory", "unsupported"));
    }
    let pressure_read = snap
        .mem
        .as_ref()
        .is_some_and(|m| m.sample.pressure.rank().is_some());
    let read = match subject {
        WhySubject::Cpu => {
            pressure_read && snap.thermal.is_some() && (facts.is_some() || !lane.on())
        }
        WhySubject::Mem => pressure_read,
    };
    let nothing_needed = read
        && causes.iter().all(|s| match s.cause.id {
            CauseId::Activity(_) => true,
            CauseId::Context(c) => !matches!(c, Context::Pressure | Context::Throttled),
            _ => false,
        });
    if !nothing_needed {
        cannot.insert(0, ("remedy", "unsupported"));
    }
    Account {
        subject,
        facts,
        parts,
        you,
        causes,
        headline,
        nothing_needed,
        cannot,
    }
}

fn cpu_you(
    facts: Option<&WhyFacts>,
    lane: CoalLane,
    panes: &[u64],
    snap: &Snapshot,
) -> (Option<Parts>, You) {
    let tab = panes.first().copied();
    let unread = |word| You {
        tab,
        role: Role::Unavailable(word),
        calm: false,
        figure: None,
        share_pm: None,
        unattributed_mc: None,
    };
    let Some(f) = facts else {
        let word = if lane.on() || snap.procs_window_ns.is_none() {
            "warming"
        } else {
            "not_sampled"
        };
        return (None, unread(word));
    };
    let split = &f.split;
    let cut = f.procs_truncated || snap.procs_truncated || snap.aterm.is_none();
    if !panes.is_empty() && !split.tabs.iter().any(|t| panes.contains(&t.0)) {
        return (None, unread(if cut { "truncated" } else { "warming" }));
    }
    let parts = split.parts_of(panes);
    if cut {
        return (Some(parts), unread("truncated"));
    }
    let whole = split.global_mc;
    let live = split.own_mc + split.tabs.iter().map(|t| t.1).sum::<i64>();
    let unattributed = (split.aterm_mc + split.delegated_mc - live).max(0);
    let role_at = |extra: i64| {
        let fig = parts.tab + extra;
        let others = [
            (parts.aterm_rest - extra).max(0),
            parts.others,
            parts.services,
            parts.unaccounted,
        ];
        let largest = others.iter().all(|p| fig >= *p);
        you_role(fig, whole, largest, above_floor(fig, whole))
    };
    let (role, calm) = role_at(0);
    let bound = role_at(unattributed).0;
    let mut you = You {
        tab,
        role,
        calm,
        figure: Some(parts.tab),
        share_pm: per_mille(parts.tab, whole),
        unattributed_mc: (tab.is_some() && unattributed > 0).then_some(unattributed),
    };
    if tab.is_some() && bound != role {
        you.role = Role::Unavailable("not_sampled");
        you.calm = false;
    }
    (Some(parts), you)
}

fn mem_you(panes: &[u64], snap: &Snapshot) -> You {
    let tab = panes.first().copied();
    let unread = |word| You {
        tab,
        role: Role::Unavailable(word),
        calm: false,
        figure: None,
        share_pm: None,
        unattributed_mc: None,
    };
    if snap.procs_truncated || snap.aterm.is_none() {
        return unread("truncated");
    }
    let Some(used) = snap.mem.as_ref().map(|m| m.sample.used()) else {
        return unread("unsupported");
    };
    let whole = i64::try_from(used).unwrap_or(i64::MAX);
    let mut fig = 0i64;
    for &k in panes {
        match snap.tab(k).map(|t| t.scope.footprint) {
            Some(Some(fp)) => fig = fig.saturating_add(i64::try_from(fp).unwrap_or(i64::MAX)),
            Some(None) => return unread("not_sampled"),
            None => return unread("warming"),
        }
    }
    let bytes = |b: u64| i64::try_from(b).unwrap_or(i64::MAX);
    let rest = snap
        .aterm
        .as_ref()
        .and_then(|a| a.footprint)
        .map(|b| bytes(b) - fig);
    let outside: Vec<Option<i64>> = match snap.why.as_deref().filter(|f| f.lane == CoalLane::On) {
        Some(f) => f
            .split
            .rows
            .iter()
            .filter(|r| r.part != super::Part::Aterm)
            .map(|r| r.footprint.map(bytes))
            .collect(),
        None => vec![None],
    };
    let beside: Vec<Option<i64>> = std::iter::once(rest).chain(outside).collect();
    let largest = if beside.iter().flatten().any(|&p| p > fig) {
        Some(false)
    } else {
        beside.iter().all(Option::is_some).then_some(true)
    };
    let above =
        u64::try_from(fig).is_ok_and(|b| b >= MEM_FLOOR_BYTES) && fig * 1000 >= whole * FLOOR_PM;
    let (mut role, mut calm) = you_role(fig, whole, largest.unwrap_or(false), above);
    if largest.is_none() && you_role(fig, whole, true, above).0 != role {
        let lane = snap.why.as_deref().map_or(snap.coal_lane, |f| f.lane);
        let word = match lane.word() {
            Some(off) => off,
            None if snap.why.is_none() => "warming",
            None => "not_sampled",
        };
        (role, calm) = (Role::Unavailable(word), false);
    }
    You {
        tab,
        role,
        calm,
        figure: Some(fig),
        share_pm: per_mille(fig, whole),
        unattributed_mc: None,
    }
}

/// Select the default subject for a bare `sys why` query.
#[must_use]
pub fn bare_subject(snap: &Snapshot) -> Bare {
    let mem = snap.mem.as_ref();
    let pressed = detect::pressed(snap).is_some();
    let swapping = mem
        .and_then(|m| m.rates)
        .is_some_and(|r| r.swap_bps() >= crate::events::SWAP_ACTIVE_BPS);
    if pressed || swapping {
        return Bare::Subject(WhySubject::Mem);
    }
    let facts = snap.why.as_deref();
    let covering = facts.and_then(|f| {
        let held = f
            .causes
            .iter()
            .filter(|c| c.id.covering())
            .map(|c| c.covers_mc);
        let open = f.open.iter().map(|o| o.covers_mc);
        held.chain(open).max()
    });
    if throttle(snap).is_some() || covering.is_some_and(|mc| mc >= BARE_CAUSE_MC) {
        return Bare::Subject(WhySubject::Cpu);
    }
    let cores = snap.cpu.as_ref().map_or(0, |c| c.total());
    let capacity = i64::try_from(cores).unwrap_or(0) * 1000;
    let busy = facts.map_or_else(
        || {
            snap.cpu
                .as_ref()
                .map_or(0, |c| (f64::from(c.busy.raw) * 1000.0).round() as i64)
        },
        |f| f.split.global_mc,
    );
    let Some(cpu_pm) = per_mille(busy, capacity).map(i64::from) else {
        return Bare::Subject(WhySubject::Cpu);
    };
    let settled = mem.is_some_and(|m| m.sample.pressure.rank() == Some(0) && m.rates.is_some())
        && snap.thermal.is_some();
    let causes_read = facts.is_some() || !snap.coal_lane.on();
    if settled && causes_read && covering.is_none() && cpu_pm < CALM_PM {
        return Bare::Calm;
    }
    let mem_pm = mem.map_or(0, |m| {
        per_mille(
            i64::try_from(m.sample.used()).unwrap_or(i64::MAX),
            i64::try_from(m.sample.total).unwrap_or(i64::MAX),
        )
        .map_or(0, i64::from)
    });
    if mem_pm > cpu_pm {
        Bare::Subject(WhySubject::Mem)
    } else {
        Bare::Subject(WhySubject::Cpu)
    }
}
