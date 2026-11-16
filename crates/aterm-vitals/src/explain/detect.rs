// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Independently supported machine contexts and unqualified retained cause records.

use super::{Part, WhyFacts};
use crate::model::{MemView, Pressure, Snapshot, Thermal};
use crate::published_cpu::CpuExecutionWindow;

/// Memory footprint's prominence floor as a share of physical used RAM, per mille.
pub const FLOOR_PM: i64 = 100;

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
    /// The members' reported CPU growth rate is at least this, millicores.
    CpuAtLeast(i64),
    /// They read at least this from storage, bytes per second.
    ReadAtLeast(u64),
    /// At least this share of their QoS time is deferred (maintenance or background), per mille.
    DeferredAtLeast(u16),
}

/// One named activity: macOS's own work with a name people know and no API that reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Activity {
    /// Its id.
    pub id: ActivityId,
    /// The people's word: `Spotlight`.
    pub word: &'static str,
    /// Its members' signing ids, matched only under [`super::Voucher::Apple`].
    pub members: &'static [&'static str],
    /// Recorded activity predicates; these do not establish execution-window CPU causality.
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
    /// The machine is throttled: a thermal level that claims it ([`Thermal::throttled`]), or
    /// Low Power Mode.
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
    /// aterm's work outside tabs: the app, helpers, services and closed tabs' survivors.
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
    /// A retained work attribution record; native published counters produce none.
    Measured,
    /// A retained part diagnostic; it does not establish physical CPU left uncovered.
    Uncovered {
        /// That part: `aterm`, `others` or `services`.
        part: Part,
    },
    /// A retained arithmetic residual, without a physical execution-window interpretation.
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
    /// The throttle: the thermal state, Low Power Mode with it.
    Throttled(Thermal),
}

/// An independent machine context or a retained, unqualified CPU attribution record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// What it is.
    pub id: CauseId,
    /// The retained reported figure, millicores (0 for a current machine context).
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

/// The machine's throttle, where it holds: an exact thermal level of serious or above, a
/// heuristic one of light or above ([`Thermal::throttled`]), or Low Power Mode. Fair names none.
pub(crate) fn throttle(snap: &Snapshot) -> Option<Thermal> {
    snap.thermal
        .filter(|t| t.throttled() || t.low_power == Some(true))
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
        out.push(context(Context::Throttled, Evidence::Throttled(t)));
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

/// Independent machine contexts. Published CPU counter growth does not establish when that
/// work executed, so it cannot supply a covering cause, startup/wake association or absence of work.
#[must_use]
pub fn detect(facts: &WhyFacts, snap: &Snapshot) -> Vec<Candidate> {
    match facts.split.cpu_accounting.execution_window {
        CpuExecutionWindow::Unavailable => machine(snap),
    }
}
