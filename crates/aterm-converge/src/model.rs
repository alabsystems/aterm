// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared convergence vocabulary: host reads, probe verdicts, outcomes, layers, and report rows.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::time::{Duration, Instant};

/// What one read of the host learned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Read<T> {
    /// The host holds this value.
    Value(T),
    /// Nothing is set: the documented "does not exist" answer.
    Absent,
    /// Nothing could be learned, and why.
    Unusable(String),
}

/// What [`crate::Item::probe`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// Already as wanted. `now` is the value, for the report.
    Ok {
        /// The current value, rendered.
        now: String,
    },
    /// Differs: `apply` would change `from` to `to`.
    Change {
        /// The current value, rendered (`absent` when nothing is set).
        from: String,
        /// The wanted value, rendered.
        to: String,
    },
    /// Only a person can make this change; `how` says where.
    Person {
        /// The step's id (`aterm machines screen <host> <step>` takes a machines one).
        step: String,
        /// One line: the System Settings path and what to switch.
        how: String,
    },
    /// A precondition is missing, so nothing was or will be attempted.
    Blocked {
        /// Which precondition, in one line.
        why: String,
    },
    /// Blocked on another item in [`crate::Item::after`].
    Waits {
        /// The id of the item whose change lifts the block.
        on: String,
        /// Which precondition, in one line.
        why: String,
    },
    /// The setting does not exist on this host.
    NotApplicable {
        /// Why it does not apply, in one line.
        why: String,
    },
    /// Deferred until the Mac is idle or `--now` is passed.
    Deferred {
        /// What it waits for, in one line.
        why: String,
    },
    /// The current state could not be measured.
    Unusable {
        /// What went wrong, in one line.
        why: String,
    },
}

/// Whether a run only plans or also applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// Probe and plan; change nothing.
    Plan,
    /// Probe, plan, apply each change, and read it back.
    Apply,
}

/// What happened to one item in one run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    /// Already as wanted.
    Ok,
    /// Differs; a plan run left it.
    Planned,
    /// Changed, and the read-back confirms it.
    Changed,
    /// Needs a person.
    Person,
    /// A precondition is missing.
    Blocked,
    /// Does not exist on this host.
    NotApplicable,
    /// Waits for the Mac to be idle (or `--now`).
    Deferred,
    /// Could not be measured.
    Unknown,
    /// The change was attempted and did not land.
    Failed,
}

impl Outcome {
    /// Whether this outcome makes a run's exit status nonzero.
    #[inline]
    #[must_use]
    pub const fn is_failure(self) -> bool {
        matches!(self, Self::Failed | Self::Unknown)
    }
}

/// When a change is felt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Effect {
    /// At once.
    Now,
    /// After the named process relaunches (the item relaunches it).
    Relaunch,
    /// At the user's next login.
    Login,
    /// After a restart.
    Restart,
}

/// Which part of the desired state an item belongs to (design §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Layer {
    /// Every machine that runs aterm.
    Base,
    /// A bare `aterm init`: the person's own desktop.
    Workstation,
    /// `aterm init --develop`: what testing a checkout needs.
    Develop,
    /// A release host, which `ship provision` owns.
    Release,
}

/// What an item's apply may do, and so which surfaces apply it (design §3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Class {
    /// Re-lay what a pass owns without network, prompts, or moving user files.
    Relay,
    /// An action allowed only when a person explicitly asked for it.
    Asked,
    /// The network: a signed index, a vendor host, a git remote, a pinned download.
    Install,
    /// Root, once.
    Elevate,
    /// Probed and explained only; applied by a person or nobody.
    Person,
    /// A change to the person's own UI (keyboard, Dock, accounts).
    Preference,
}

/// Where an item may run (design §3.2, preconditions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Requires {
    /// Any shell of the user's own account (`HOME` is the passwd home, not root or a scratch root).
    #[default]
    Account,
    /// That, and the user owns `/dev/console`.
    Console,
}

/// Why an item reads as it does, and the one remedy (design §3.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Explain {
    /// One line.
    pub why: String,
    /// What makes it right.
    pub remedy: Remedy,
}

/// The one act that makes an item right.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Remedy {
    /// `aterm pkg repair` (or a bare `aterm doctor`) fixes it.
    Repair,
    /// `aterm init --only <only>`, an item or a layer.
    Init {
        /// The item id or layer name.
        only: String,
    },
    /// `aterm init --develop`: remedy for `develop.*` items.
    Develop,
    /// `targo --unverified verify provision <args>`, the remedy for checkout tools.
    Provision {
        /// The verb's arguments.
        args: String,
    },
    /// A person, in System Settings.
    Person {
        /// The pane (or the step a controller takes the owner to).
        pane: String,
        /// What to switch there.
        how: String,
    },
    /// The next pass, once online.
    Network,
    /// An explicit door, e.g. `aterm pkg install --default-set`.
    Command(String),
    /// Nothing aterm can do (another OS, the hardware).
    None,
}

impl Remedy {
    /// The command that carries the remedy out, spelled once for every surface that prints
    /// it: `aterm pkg repair`, `aterm init --only <only>`, `aterm init --develop`,
    /// `targo --unverified verify provision <args>`, or the explicit door; `None` for a
    /// person's act in System Settings, the next pass, or nothing.
    #[must_use]
    pub fn command(&self) -> Option<String> {
        match self {
            Self::Repair => Some("aterm pkg repair".to_owned()),
            Self::Init { only } => Some(format!("aterm init --only {only}")),
            Self::Develop => Some("aterm init --develop".to_owned()),
            Self::Provision { args } => Some(
                format!("targo --unverified verify provision {args}")
                    .trim_end()
                    .to_owned(),
            ),
            Self::Command(command) => Some(command.clone()),
            Self::Person { .. } | Self::Network | Self::None => None,
        }
    }
}

/// Why an apply stood aside without changing anything and without failing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StoodAside {
    /// Another run held a lock the change needs (atpkg's exit 75); the line says whose.
    Lock(String),
    /// A network the change needs is down (atpkg's exit 69).
    Offline(String),
    /// Another actor must finish a prerequisite, such as an admin approval or background pass.
    Waiting(String),
    /// The account has not granted permission the change needs (exit 77).
    Permission(String),
}

impl StoodAside {
    /// The one line a report carries.
    #[inline]
    #[must_use]
    pub fn why(&self) -> &str {
        match self {
            Self::Lock(why) | Self::Offline(why) | Self::Waiting(why) | Self::Permission(why) => {
                why
            }
        }
    }

    /// Exit status for a run whose only incomplete rows stood aside (`75`, `69`, or `77`).
    #[inline]
    #[must_use]
    pub const fn exit_status(&self) -> u8 {
        match self {
            Self::Lock(_) | Self::Waiting(_) => 75,
            Self::Offline(_) => 69,
            Self::Permission(_) => 77,
        }
    }
}

impl fmt::Display for StoodAside {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.why())
    }
}

/// One item's row in a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemReport {
    /// Stable id, e.g. `keyboard.caps-lock.0-0-0@alice`.
    pub id: String,
    /// The family (`keyboard`, `power`, …): the `--only <table>` merge key.
    pub family: String,
    /// One human line.
    pub title: String,
    /// What happened.
    pub outcome: Outcome,
    /// The value before (rendered), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// The value wanted or now held (rendered).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Failure/block/unknown detail, or what a person step requires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// The command that undoes a change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revert: Option<String>,
    /// When the change is felt.
    pub effect: Effect,
    /// Family-specific output a caller acts on (a GitHub public key line).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The item's layer (`None` on rows not produced by an item).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<Layer>,
    /// The item's class; `None` as for `layer`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<Class>,
    /// Finding and remedy for any row not left `Ok` or `Changed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explain: Option<Explain>,
    /// Set when the apply stood aside (`Blocked` on lock, network, or permission).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stood_aside: Option<StoodAside>,
    /// Whether leaving this row wrong fails a surface under [`ItemReport::fails_surface`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub essential: bool,
    /// The wall time the engine spent on this row, in milliseconds: its item's probe, plan,
    /// apply and read-back, or the decision that left it unprobed; 0 on a row a surface
    /// reports with no item behind it (a refusal, missing evidence). What a surface reads
    /// before the engine runs (the doctor's facts, the packages' read) is in its report's
    /// [`Report::phases`]. `None` on a report written before rows were timed and on a row
    /// no run made (a machines agent's job rows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
}

impl ItemReport {
    /// Whether this row fails a surface under the essential-item policy.
    #[inline]
    #[must_use]
    pub const fn fails_surface(&self) -> bool {
        match self.outcome {
            Outcome::Ok | Outcome::Changed | Outcome::NotApplicable | Outcome::Deferred => false,
            Outcome::Failed => true,
            Outcome::Planned | Outcome::Person | Outcome::Blocked | Outcome::Unknown => {
                self.essential
            }
        }
    }
}

/// The schema of [`Report`], the one envelope every `--json` surface prints.
pub const SCHEMA: u32 = 1;

/// What a surface was asked to do, as its [`Report`] says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReportMode {
    /// Probed, planned and applied (`aterm init`).
    Apply,
    /// Probed and planned; nothing changed (`aterm init --plan`).
    Plan,
    /// Probed against a surface's rule without changing anything (`--check`, `doctor`).
    Check,
}

/// Readiness summary of reported checks, independent of a surface's failure policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Readiness {
    /// Every reported check is satisfied or explicitly not applicable (false if empty).
    pub all_green: bool,
    /// The requested selectors; empty means the surface's full default scope.
    pub scope: Vec<String>,
    /// Checks already satisfied or changed and confirmed by read-back.
    pub satisfied: usize,
    /// Checks explicitly inapplicable to this host or accepted configuration.
    pub not_applicable: usize,
    /// Changed configuration awaiting login or restart according to the row's effect.
    pub pending_activation: Vec<String>,
    /// Stable ids of checks still planned, manual, blocked, deferred, unknown, or failed.
    pub unresolved: Vec<String>,
}

impl Readiness {
    /// Summarize the evidence in `items` without changing outcomes or failure policy.
    #[must_use]
    pub fn of(items: &[ItemReport], scope: &[String]) -> Self {
        let mut readiness = Self {
            all_green: false,
            scope: scope.to_vec(),
            satisfied: 0,
            not_applicable: 0,
            pending_activation: Vec::new(),
            unresolved: Vec::new(),
        };
        for row in items {
            match row.outcome {
                Outcome::Changed if matches!(row.effect, Effect::Login | Effect::Restart) => {
                    readiness.pending_activation.push(row.id.clone());
                }
                Outcome::Ok | Outcome::Changed => readiness.satisfied += 1,
                Outcome::NotApplicable => readiness.not_applicable += 1,
                Outcome::Planned
                | Outcome::Person
                | Outcome::Blocked
                | Outcome::Deferred
                | Outcome::Unknown
                | Outcome::Failed => readiness.unresolved.push(row.id.clone()),
            }
        }
        readiness.all_green = !items.is_empty()
            && readiness.unresolved.is_empty()
            && readiness.pending_activation.is_empty();
        readiness
    }
}

/// Report of one run in the shared `--json` schema ([`SCHEMA`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// [`SCHEMA`].
    pub schema: u32,
    /// What was asked.
    pub mode: ReportMode,
    /// Whether the surface's exit rule held (exit status `0` iff `true`).
    pub complete: bool,
    /// One row per item, in run order.
    pub items: Vec<ItemReport>,
    /// Optional stricter readiness assessment used by `aterm doctor`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<Readiness>,
    /// Each phase of the run and its wall time in milliseconds ([`Laps`]), by name.
    /// Empty, and not written, on a report written before surfaces timed their phases.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub phases: Phases,
}

impl Report {
    /// The report of a run that asked `mode` and ended `complete`, at [`SCHEMA`].
    #[must_use]
    pub const fn new(mode: ReportMode, complete: bool, items: Vec<ItemReport>) -> Self {
        Self {
            schema: SCHEMA,
            mode,
            complete,
            items,
            readiness: None,
            phases: BTreeMap::new(),
        }
    }
}

/// A report's [`Report::phases`]: each phase's name and its wall time in milliseconds.
pub type Phases = BTreeMap<String, u64>;

/// `elapsed` in whole milliseconds; a span past `u64::MAX` of them reads as that.
#[must_use]
pub fn millis(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

/// A run's phase clock. Each [`Laps::lap`] charges the time since the one before (or since
/// [`Laps::start`]) to a phase, adding to what that phase already holds, so a surface that
/// laps after every phase accounts for each moment of its run once. Displayed in run
/// order with the total, for the run's one log line.
#[derive(Debug)]
pub struct Laps {
    since: Instant,
    laps: Vec<(&'static str, Duration)>,
}

impl Laps {
    /// The clock of a run starting now.
    #[must_use]
    pub fn start() -> Self {
        Self {
            since: Instant::now(),
            laps: Vec::new(),
        }
    }

    /// Charge the time since the last lap to `phase`.
    pub fn lap(&mut self, phase: &'static str) {
        let now = Instant::now();
        let took = now.saturating_duration_since(self.since);
        self.since = now;
        match self.laps.iter_mut().find(|(name, _)| *name == phase) {
            Some((_, total)) => *total = total.saturating_add(took),
            None => self.laps.push((phase, took)),
        }
    }

    /// Each phase's total, for [`Report::phases`].
    #[must_use]
    pub fn phases(&self) -> Phases {
        self.laps
            .iter()
            .map(|&(name, took)| (name.to_owned(), millis(took)))
            .collect()
    }
}

impl fmt::Display for Laps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut total = Duration::ZERO;
        for &(name, took) in &self.laps {
            write!(f, "{name} {} ms, ", millis(took))?;
            total = total.saturating_add(took);
        }
        write!(f, "total {} ms", millis(total))
    }
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
