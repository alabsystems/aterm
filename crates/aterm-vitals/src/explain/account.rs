// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ACCOUNT: one subject's figure, its parts, the causes that hold over its
//! window and the caller tab's role in it (RD §4.2, §5.5–§5.6), the one
//! reading `sys why` and the CPU close-up's Why now both render.
//!
//! * [`account`]: the CPU's or memory's account for the caller's tab.
//! * [`you_role`]: a tab's role by its share of the figure: `main` (at least
//!   half, the largest part, above the floor), `part` (at least a fifth, or
//!   the largest part, above the floor), `minor` (some, under a fifth; or
//!   below the floor, on a calm machine), `none`. A sweep cut at its cap
//!   reads `unavailable(truncated)`, never `none`. On the CPU, work aterm's
//!   coalitions did that no live process of it holds (reaped between sweeps;
//!   reap credit is not read) may be the tab's: while there is any, the
//!   tab's figure is a lower bound, and the role is taken both without it and
//!   with it; where the two differ it reads `unavailable(not_sampled)`.
//!   A tab the split never read (opened after its tick) reads
//!   `unavailable(warming)` (on a cut sweep, `truncated`), never `none`.
//!   The machine's contexts (`throttled`, `pressure`) need no window: they
//!   are read off the snapshot itself, always, so a reply carrying an older
//!   split tick's facts reads the machine as a bare `sys why` does, and one
//!   with no split made yet (before the lane's first window, or where none
//!   can be: Windows reads no process's owner) reads them all the same.
//! * [`bare_subject`]: what `sys why` with no subject answers for (RD §5.6).
//!
//! Pure.

use std::sync::Arc;

use super::detect::{self, Candidate, CauseId, Context, FLOOR_PM, above_floor, throttle};
use super::{CoalLane, Parts, WhyFacts};
use crate::model::Snapshot;

/// The memory floor a tab's footprint must pass for a role above `minor`,
/// bytes (and [`FLOOR_PM`] of used).
pub const MEM_FLOOR_BYTES: u64 = 1 << 30;
/// Busy below this share of the cores is calm, per mille (the `quiet`
/// preset's line).
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

/// What `sys why` with no subject answers for.
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
    /// Not read, with the unavailable word: `truncated` (the sweep stopped
    /// at its cap), `not_sampled` (finished work may be the tab's; or, with
    /// the lane off, no split could be made, an owner outside aterm
    /// unread), `warming` (no figure yet).
    Unavailable(&'static str),
}

impl Role {
    /// The wire word (`main`, `part`, `minor`, `none`), or the unavailable
    /// word.
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
    /// Whether it is `minor` only because the figure is below the floor (a
    /// calm machine): `basis=machine_calm`.
    pub calm: bool,
    /// Its figure: CPU millicores (live), or footprint bytes; `None` where
    /// unread.
    pub figure: Option<i64>,
    /// That of the subject's figure, per mille.
    pub share_pm: Option<u16>,
    /// On the CPU, for a caller in a tab, while there is any: aterm's work
    /// no live process holds, millicores, which may be the tab's, so
    /// [`Self::figure`] is a lower bound (`cpu_at_least_mc=`).
    pub unattributed_mc: Option<i64>,
}

/// A cause that holds over the window, as a reply prints it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    /// The cause.
    pub cause: Candidate,
    /// When its episode opened, unix ms; `None` where none is open (a cold
    /// pull's first window), and for a context, which says when by its
    /// evidence (`since=`) and prints no `opened=`.
    pub opened_unix_ms: Option<i64>,
}

/// One subject's account for one caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    /// The subject.
    pub subject: WhySubject,
    /// The facts it reads; `None` before the first split tick, and where no
    /// split can be made.
    pub facts: Option<Arc<WhyFacts>>,
    /// The CPU's five parts with the caller's tab out of aterm's (CPU only);
    /// `None` with no facts, and for a tab the split never read, whose role
    /// is then unread while [`Self::facts`]' split still holds the others,
    /// the services and the unaccounted.
    pub parts: Option<Parts>,
    /// The caller tab's role.
    pub you: You,
    /// The causes that hold over the window, ranked: covering causes by what
    /// they cover, then the contexts, the machine's (`throttled`,
    /// `pressure`) as the snapshot reads them (memory: `pressure` alone).
    pub causes: Vec<Shown>,
    /// The headline: the top covering cause and at most one context that
    /// holds for it; with none, the first context that holds (memory:
    /// `pressure`).
    pub headline: Option<(CauseId, Option<Context>)>,
    /// Whether nothing needs doing (`do class=none`): what could need it was
    /// read (memory's pressure; for the CPU, the thermal state too, and its
    /// causes as [`bare_subject`] reads them: with the lane on, over a
    /// window), every covering cause shown is a named activity, or none is,
    /// and neither `pressure` nor `throttled` holds. Otherwise aterm offers
    /// no step yet, and [`Self::cannot`] leads with `(remedy, unsupported)`
    /// in its place.
    pub nothing_needed: bool,
    /// What cannot be read, `(what, why)`, one per limit the reply touches.
    pub cannot: Vec<(&'static str, &'static str)>,
}

/// `part` of `whole`, per mille, rounded and never past 1000 (a tab's live
/// figure can read ahead of the busy one); `None` for no whole.
fn per_mille(part: i64, whole: i64) -> Option<u16> {
    (whole > 0).then(|| {
        let pm = (part.max(0) * 1000 + whole / 2) / whole;
        u16::try_from(pm.min(1000)).unwrap_or(1000)
    })
}

/// The role of a figure `fig` of `whole` (`largest`: no other part is
/// larger; `above`: it passes the subject's floor), and whether it is
/// `minor` by the floor alone (`basis=machine_calm`).
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

/// `subject`'s account for a caller in tab `you` (the host's key), from
/// snapshot `snap`.
#[must_use]
pub fn account(subject: WhySubject, you: Option<u64>, snap: &Snapshot) -> Account {
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
        WhySubject::Cpu => cpu_you(facts.as_deref(), lane, you, snap),
        WhySubject::Mem => (None, mem_you(you, snap)),
    };
    // The CPU's bound rule: finished work that may be the tab's.
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
    // With no covering cause, the causes are contexts, in their order.
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
    // Nothing is known needless where what it rests on was not read: with
    // the lane on, no window yet (with it off no covering cause holds),
    // memory's pressure unread (Linux with no PSI), or, for the CPU, the
    // thermal state unread.
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

/// The CPU's parts for tab `tab`, and the tab's role (the bound rule), under
/// the lane `lane`.
fn cpu_you(
    facts: Option<&WhyFacts>,
    lane: CoalLane,
    tab: Option<u64>,
    snap: &Snapshot,
) -> (Option<Parts>, You) {
    let unread = |word| You {
        tab,
        role: Role::Unavailable(word),
        calm: false,
        figure: None,
        share_pm: None,
        unattributed_mc: None,
    };
    let Some(f) = facts else {
        // No split yet: the lane's first window is to come, or, with the
        // lane off, the split by subtraction could not be made (an owner
        // outside aterm unread), as the built views say.
        let word = if lane.on() || snap.procs_window_ns.is_none() {
            "warming"
        } else {
            "not_sampled"
        };
        return (None, unread(word));
    };
    let split = &f.split;
    // The tab's figures are the split tick's sweep's, cut or not.
    let cut = f.procs_truncated || snap.procs_truncated || snap.aterm.is_none();
    // A tab the split never read (opened after its tick) has no figure yet,
    // never 0: neither its role nor its parts; a cut sweep is judged first.
    if tab.is_some_and(|k| split.tabs.iter().all(|t| t.0 != k)) {
        return (None, unread(if cut { "truncated" } else { "warming" }));
    }
    let parts = split.parts(tab);
    if cut {
        return (Some(parts), unread("truncated"));
    }
    let whole = split.global_mc;
    let live = split.own_mc + split.tabs.iter().map(|t| t.1).sum::<i64>();
    let unattributed = (split.aterm_mc + split.delegated_mc - live).max(0);
    // The role at the tab's live figure, then with every unattributed
    // millicore of aterm's moved from aterm's rest into the tab.
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

/// The tab `tab`'s role in used memory, by its footprint.
fn mem_you(tab: Option<u64>, snap: &Snapshot) -> You {
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
    let fig = match tab {
        Some(k) => match snap.tab(k).map(|t| t.scope.footprint) {
            Some(Some(fp)) => i64::try_from(fp).unwrap_or(i64::MAX),
            Some(None) => return unread("not_sampled"),
            // A tab no sweep holds yet: no figure, never 0.
            None => return unread("warming"),
        },
        None => 0,
    };
    // The parts beside the tab: the rest of aterm, and each coalition outside
    // it, where the lane read every one's footprint (unread, no part can be
    // told larger, so no role above `part`).
    let aterm = snap.aterm.as_ref().and_then(|a| a.footprint).unwrap_or(0);
    let rest = i64::try_from(aterm).unwrap_or(i64::MAX) - fig;
    let outside = snap
        .why
        .as_deref()
        .filter(|f| f.lane == CoalLane::On)
        .and_then(|f| {
            f.split
                .rows
                .iter()
                .filter(|r| r.part != super::Part::Aterm)
                .try_fold(0, |most: i64, r| {
                    Some(most.max(i64::try_from(r.footprint?).unwrap_or(i64::MAX)))
                })
        });
    let largest = outside.is_some_and(|o| fig >= o && fig >= rest);
    let above =
        u64::try_from(fig).is_ok_and(|b| b >= MEM_FLOOR_BYTES) && fig * 1000 >= whole * FLOOR_PM;
    let (role, calm) = you_role(fig, whole, largest, above);
    You {
        tab,
        role,
        calm,
        figure: Some(fig),
        share_pm: per_mille(fig, whole),
        unattributed_mc: None,
    }
}

/// What `sys why` with no subject answers for, the first that holds (RD
/// §5.6): memory at pressure warn or above, or swap moving; the CPU when
/// throttled (as `throttled` holds: Low Power Mode too), when a covering
/// cause of at least [`BARE_CAUSE_MC`] holds or is open, or when its cores
/// are unread; calm with every deviation read and none there (pressure
/// normal, swap traffic read and still, the thermal state read and no
/// throttle), the causes read (with the lane on, a window's facts) and none
/// holding or open, and the cores under [`CALM_PM`] busy, since an unknown is
/// never calm; else whichever of the two is the larger share of its
/// capacity.
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
    // A pull's own window prints what holds now, though no episode has
    // opened: both count, so the bare answer never says calm beside a
    // cause the CPU's reply prints.
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
    // With the lane on, no cause is known not to hold before its first
    // window is read.
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
