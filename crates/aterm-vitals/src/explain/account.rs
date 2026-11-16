// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CPU and memory attribution for `sys why` ([`account`], [`you_role`], [`bare_subject`]).

use std::sync::Arc;

use super::detect::{self, Candidate, CauseId, Context, FLOOR_PM, throttle};
use super::{CoalLane, Parts, WhyFacts};
use crate::model::Snapshot;
use crate::published_cpu::CpuExecutionWindow;

/// Minimum memory footprint in bytes (alongside [`FLOOR_PM`]) for a role above `minor`.
pub const MEM_FLOOR_BYTES: u64 = 1 << 30;

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
    /// Calm: a sentence, the `you` line and the attribution limits.
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

/// The caller tab's CPU contribution or memory footprint prominence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct You {
    /// The tab, by the host's key; `None` for a caller in no tab.
    pub tab: Option<u64>,
    /// Its role.
    pub role: Role,
    /// Whether it is `minor` only because the figure is below the floor (a calm machine).
    pub calm: bool,
    /// Its diagnostic published CPU growth per observation interval, or footprint bytes;
    /// `None` where unread. CPU growth does not establish execution in that interval.
    pub figure: Option<i64>,
    /// No physical CPU or RAM share is established by the diagnostic counter accounts.
    pub share_pm: Option<u16>,
    /// Missing physical CPU bound, unavailable without execution-window evidence.
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
            Role::Unavailable("execution_window_unknown") => "this tab's share is not read now",
            Role::Unavailable(_) if self.figure.is_some() && self.unattributed_mc.is_none() => {
                "whether this tab is the largest part is not read now"
            }
            Role::Unavailable(_) => "this tab's share is not read now",
        }
    }

    /// The CPU tab clause beside a sentence that names no cause: [`Self::clause`] in words of the
    /// figure alone, since "part of the cause" beside "no cause open" says two things at once.
    #[must_use]
    pub fn share_clause(&self) -> &'static str {
        match self.role {
            Role::Main => "this tab uses most of it",
            Role::Part => "this tab uses a large part of it",
            Role::Minor => "this tab uses little of it",
            Role::None | Role::Unavailable(_) => self.clause(),
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
    /// macOS's services that hold past the large bar now, largest first (memory only)
    /// ([`crate::services::over_in`]).
    pub services: Vec<crate::services::RowOver>,
    /// The restart the reply offers (`do class=restart`): the largest service's whose restart is
    /// due ([`crate::services::RowOver::restart_due`]) and that the person did not put off
    /// ([`Snapshot::services_filed`]).
    pub restart: Option<&'static crate::services::Restart>,
    /// Whether a restart was due but the person put it off for that process.
    pub restart_filed: bool,
    /// Whether nothing needs doing (`do class=none`) ([`bare_subject`], [`Self::cannot`]).
    pub nothing_needed: bool,
    /// What cannot be read, `(what, why)`, one per limit the reply touches.
    pub cannot: Vec<(&'static str, &'static str)>,
}

fn per_mille(part: i64, whole: i64) -> Option<u16> {
    (whole > 0).then(|| {
        let pm = (i128::from(part.max(0)) * 1000 + i128::from(whole) / 2) / i128::from(whole);
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
    // Ownership coverage does not bound native counter publication lag. The process sweep
    // also cannot witness every lifetime or a delegation over a socket.
    if subject == WhySubject::Cpu && facts.is_some() {
        cannot.push(("cpu_execution_window", "execution_window_unknown"));
        cannot.push(("every_process_start", "not_sampled"));
        cannot.push(("between_sweep_orphans", "not_sampled"));
        cannot.push(("socket_delegation", "unsupported"));
    }
    // Retained CPU causes or episodes cannot qualify an execution window. Independent current
    // machine contexts remain evidence even when no physical CPU attribution can be made.
    let held = detect::machine(snap);
    let causes: Vec<Shown> = held
        .iter()
        .filter(|c| subject == WhySubject::Cpu || c.id == CauseId::Context(Context::Pressure))
        .map(|c| Shown {
            cause: c.clone(),
            opened_unix_ms: None,
        })
        .collect();
    let headline = causes.first().map(|shown| (shown.cause.id, None));
    if subject == WhySubject::Mem {
        cannot.push(("wired_by_process", "needs_privilege:root"));
        cannot.push(("uncharged_memory", "unsupported"));
    }
    let pressure_read = snap
        .mem
        .as_ref()
        .is_some_and(|m| m.sample.pressure.rank().is_some());
    let read = match subject {
        WhySubject::Cpu => false,
        WhySubject::Mem => pressure_read,
    };
    let services = match subject {
        WhySubject::Mem => crate::services::over_in(snap),
        WhySubject::Cpu => Vec::new(),
    };
    let pressed = memory_pressed(snap);
    let (filed, due): (Vec<_>, Vec<_>) = services
        .iter()
        .filter(|o| o.restart_due(pressed))
        .partition(|o| snap.services_filed.contains(&o.episode));
    let restart = due.first().and_then(|o| o.restart);
    let restart_filed = !filed.is_empty();
    let nothing_needed = read
        && restart.is_none()
        && !restart_filed
        && causes.iter().all(|s| match s.cause.id {
            CauseId::Activity(_) => true,
            CauseId::Context(c) => !matches!(c, Context::Pressure | Context::Throttled),
            _ => false,
        });
    if !nothing_needed && restart.is_none() && !restart_filed {
        cannot.insert(0, ("remedy", "unsupported"));
    }
    Account {
        subject,
        facts,
        parts,
        you,
        causes,
        headline,
        services,
        restart,
        restart_filed,
        nothing_needed,
        cannot,
    }
}

/// Whether memory is under pressure now: the kernel's level past normal, or swap moving.
fn memory_pressed(snap: &Snapshot) -> bool {
    detect::pressed(snap).is_some()
        || snap
            .mem
            .as_ref()
            .and_then(|m| m.rates)
            .is_some_and(|r| r.swap_bps() >= crate::events::SWAP_ACTIVE_BPS)
}

/// Whether the sweep that feeds the tab's share stalled at `snap` ([`crate::lanes`]).
fn sweep_stalled(snap: &Snapshot) -> bool {
    crate::lanes::stalls(&snap.stalled, "why").is_some()
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
    let stalled = sweep_stalled(snap);
    let Some(f) = facts else {
        let word = if stalled {
            "stalled"
        } else if lane.on() || snap.procs_window_ns.is_none() {
            "warming"
        } else {
            "not_sampled"
        };
        return (None, unread(word));
    };
    let split = &f.split;
    let cut = f.procs_truncated || snap.procs_truncated || snap.aterm.is_none();
    if !panes.is_empty() && !split.tabs.iter().any(|t| panes.contains(&t.0)) {
        let word = match (stalled, cut) {
            (true, _) => "stalled",
            (false, true) => "truncated",
            (false, false) => "warming",
        };
        return (None, unread(word));
    }
    let parts = split.parts_of_coverage(panes, f.provenance_incomplete);
    if cut {
        return (
            Some(parts),
            unread(if stalled { "stalled" } else { "truncated" }),
        );
    }
    let you = You {
        tab,
        role: match split.cpu_accounting.execution_window {
            CpuExecutionWindow::Unavailable => Role::Unavailable("execution_window_unknown"),
        },
        calm: false,
        figure: Some(split.tab_reported_of(panes)),
        share_pm: None,
        unattributed_mc: None,
    };
    (Some(parts), you)
}

/// Physical used RAM supplies footprint prominence thresholds only. These unlike
/// accounts do not form a physical partition or a percentage of RAM attributable to a tab.
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
    if sweep_stalled(snap) {
        return unread("stalled");
    }
    if snap.procs_truncated || snap.aterm.is_none() {
        return unread("truncated");
    }
    let Some(used) = snap.mem.as_ref().map(|m| m.sample.used()) else {
        return unread("unsupported");
    };
    let whole = i64::try_from(used).unwrap_or(i64::MAX);
    let mut fig = 0i64;
    for &k in panes {
        if snap.tab(k).is_some_and(|t| t.scope.footprint_unread > 0) {
            return unread("refused");
        }
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
        .filter(|a| a.footprint_unread == 0)
        .and_then(|a| a.footprint)
        .map(|b| bytes(b) - fig);
    let outside: Vec<Option<i64>> = match snap.why.as_deref().filter(|f| {
        f.lane == CoalLane::On && f.at_ns == snap.at_ns && !f.memory_provenance_incomplete
    }) {
        Some(f) => {
            let mut outside: Vec<_> = f
                .split
                .rows
                .iter()
                .map(|r| r.footprint_account().outside_bytes().map(bytes))
                .collect();
            if f.split.list_truncated || f.split.rows.len() < f.split.listed as usize {
                outside.push(None);
            }
            outside
        }
        None => vec![None],
    };
    let beside: Vec<Option<i64>> = std::iter::once(rest).chain(outside).collect();
    let largest = if beside.iter().flatten().any(|&p| p > fig) {
        Some(false)
    } else {
        beside.iter().all(Option::is_some).then_some(true)
    };
    let above = u64::try_from(fig).is_ok_and(|b| b >= MEM_FLOOR_BYTES)
        && i128::from(fig) * 1000 >= i128::from(whole) * i128::from(FLOOR_PM);
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
        share_pm: None,
        unattributed_mc: None,
    }
}

/// Select the default subject for a bare `sys why` query.
#[must_use]
pub fn bare_subject(snap: &Snapshot) -> Bare {
    let huge_service = crate::services::over_in(snap)
        .iter()
        .any(|o| o.size == crate::services::Size::Huge);
    if memory_pressed(snap) || huge_service {
        return Bare::Subject(WhySubject::Mem);
    }
    if throttle(snap).is_some() {
        return Bare::Subject(WhySubject::Cpu);
    }
    // Low or zero published growth cannot establish a calm execution interval, and unlike
    // memory footprint no native CPU source supplies the physical share this comparison needs.
    Bare::Subject(WhySubject::Cpu)
}

#[cfg(test)]
#[path = "account_tests.rs"]
mod tests;
