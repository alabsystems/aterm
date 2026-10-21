// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vocabulary every surface shares: what a read learned, what a probe decided, what
//! happened, where an item belongs, and the row one item contributes to a report.

use serde::{Deserialize, Serialize};
use std::fmt;

/// What one read of the host learned.
///
/// ABSENT AND UNUSABLE ARE NOT THE SAME ANSWER. `Absent` is a measurement: the tool said,
/// in its documented way, that nothing is set, which is the OS default. `Unusable` is the
/// absence of one: the tool did not run, ran out of time, or answered something the reader
/// does not understand. Nothing may report an `Unusable` read as the default. (atpkg's
/// Universal Control read and the machines families both keep this rule through this one
/// type.)
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
    /// Blocked on what another item, one of [`crate::Item::after`], puts in place (git's
    /// settings before the Command Line Tools exist). A plan that changes that item first
    /// reports this one planned; any other block is [`Probe::Blocked`].
    Waits {
        /// The id of the item whose change lifts the block.
        on: String,
        /// Which precondition, in one line.
        why: String,
    },
    /// The setting does not exist on this host (an Intel Mac, a desktop, an older macOS):
    /// nothing to do, and nothing wrong.
    NotApplicable {
        /// Why it does not apply, in one line.
        why: String,
    },
    /// The change would disrupt someone using the Mac (relaunching their Dock, swapping an
    /// app they have open); left for a run when the Mac is idle, or `--now`.
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
    /// Whether this outcome makes a run's exit status nonzero: a change that did not land,
    /// or a state nobody could measure.
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
    /// Lay again what a pass lays and owns. No network, no prompt, nothing a person made
    /// is moved: the one class an unattended lane applies.
    Relay,
    /// An act the code allows only when a person asked for it (stale copies to the Trash,
    /// copied commands moved aside).
    Asked,
    /// The network: a signed index, a vendor host, a git remote, a pinned download.
    Install,
    /// Root, once.
    Elevate,
    /// A person in System Settings, or in what is theirs (PATH, an rc line, a settings key),
    /// or nothing on this machine (free disk): probed and explained, applied by nobody.
    Person,
    /// A change to the person's own UI (keyboard, Dock, accounts).
    Preference,
}

/// Where an item may run (design §3.2, preconditions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Requires {
    /// Any shell of the user's own account: HOME is the passwd home, not root, not under
    /// a private state root.
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
    /// `aterm init --develop`.
    Develop,
    /// `targo --unverified verify provision <args>` (`--linux`, `--record`, or nothing).
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

/// Why an apply stood aside without changing anything and without failing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StoodAside {
    /// Another run held a lock the change needs (atpkg's exit 75); the line says whose.
    Lock(String),
    /// A network the change needs is down (atpkg's exit 69).
    Offline(String),
}

impl StoodAside {
    /// The one line a report carries.
    #[must_use]
    pub fn why(&self) -> &str {
        match self {
            Self::Lock(why) | Self::Offline(why) => why,
        }
    }

    /// The exit status a run whose only incomplete rows stood aside this way ends with:
    /// 75 (`EX_TEMPFAIL`) for a lock, 69 (`EX_UNAVAILABLE`) offline (design §3.3).
    #[must_use]
    pub const fn exit_status(&self) -> u8 {
        match self {
            Self::Lock(_) => 75,
            Self::Offline(_) => 69,
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
    /// Why it failed, is blocked or unknown; for a person step, what to do. A machines
    /// `exec` row carries its program's standard error here even when it succeeded.
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
    /// The item's layer. `None` on a row no item made (a machines agent's own rows) and
    /// on a report written before the engine moved here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<Layer>,
    /// The item's class; `None` as for `layer`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<Class>,
    /// Why the row reads as it does and the one remedy, on every row the run did not
    /// leave `Ok` or `Changed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explain: Option<Explain>,
    /// Set when the apply stood aside (a held lock, no network); the row is `Blocked`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stood_aside: Option<StoodAside>,
    /// The item's [`crate::Item::essential`]: whether this row left wrong fails the surface
    /// that ran it ([`ItemReport::fails_surface`]). A structural fault and an advisory both
    /// plan a change, so `outcome` alone cannot tell a reader which one decided the exit.
    /// Written only when true.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub essential: bool,
}

impl ItemReport {
    /// Whether this row fails the surface that ran it: a change that did not land, or an
    /// essential item the run left other than done (`Ok`, `Changed`), not applicable or
    /// waiting for an idle Mac. One rule for the surface's exit and for a reader of its
    /// `--json` (the machines toolset probe) naming the rows that decided it.
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
    /// Probed against a surface's rule; nothing changed (`aterm init --check`,
    /// `aterm pkg doctor`).
    Check,
}

/// The report of one run, in the one schema `--json` prints on every surface (design
/// §3.3): `aterm init` and `aterm pkg doctor` today.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// [`SCHEMA`].
    pub schema: u32,
    /// What was asked.
    pub mode: ReportMode,
    /// Whether the surface's exit rule held: its exit status is 0 exactly when this is
    /// true (a stood-aside init exits 75 or 69, never 0).
    pub complete: bool,
    /// One row per item, in run order.
    pub items: Vec<ItemReport>,
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_or_unmeasured_item_fails_the_run_and_a_person_step_does_not() {
        assert!(Outcome::Failed.is_failure());
        assert!(Outcome::Unknown.is_failure());
        assert!(!Outcome::Person.is_failure());
        assert!(!Outcome::Blocked.is_failure());
        assert!(!Outcome::NotApplicable.is_failure());
        assert!(!Outcome::Deferred.is_failure());
        assert!(!Outcome::Planned.is_failure());
    }

    fn row() -> ItemReport {
        ItemReport {
            id: "base.clt".into(),
            family: "developer".into(),
            title: "the Command Line Tools".into(),
            outcome: Outcome::Blocked,
            from: Some("absent".into()),
            to: Some("installed".into()),
            why: Some("no network".into()),
            revert: None,
            effect: Effect::Now,
            detail: None,
            layer: Some(Layer::Base),
            class: Some(Class::Elevate),
            explain: Some(Explain {
                why: "no network".into(),
                remedy: Remedy::Init {
                    only: "base.clt".into(),
                },
            }),
            stood_aside: Some(StoodAside::Offline("no network".into())),
            essential: true,
        }
    }

    #[test]
    fn a_row_round_trips_through_json_with_every_new_field() {
        let mut rows = vec![row()];
        for remedy in [
            Remedy::Repair,
            Remedy::Develop,
            Remedy::Provision {
                args: "--linux".into(),
            },
            Remedy::Person {
                pane: "Privacy & Security".into(),
                how: "turn on aterm".into(),
            },
            Remedy::Network,
            Remedy::Command("aterm pkg install --default-set".into()),
            Remedy::None,
        ] {
            let mut r = row();
            r.explain = Some(Explain {
                why: "x".into(),
                remedy,
            });
            r.stood_aside = Some(StoodAside::Lock("held".into()));
            rows.push(r);
        }
        for r in rows {
            let text = aterm_json::to_string(&r).unwrap();
            assert_eq!(
                aterm_json::from_str::<ItemReport>(&text).unwrap(),
                r,
                "{text}"
            );
        }
    }

    #[test]
    fn a_row_written_before_the_new_fields_still_reads() {
        // A machines agent older than this crate reports rows without them.
        let old = r#"{"id":"power.sleep","family":"power","title":"sleep","outcome":"changed","effect":"now"}"#;
        let r: ItemReport = aterm_json::from_str(old).unwrap();
        assert_eq!(r.outcome, Outcome::Changed);
        assert_eq!((r.layer, r.class), (None, None));
        assert!(r.explain.is_none() && r.stood_aside.is_none() && !r.essential);
        // And a row without them writes none of their keys, as before.
        let text = aterm_json::to_string(&r).unwrap();
        assert!(
            !text.contains("layer") && !text.contains("explain") && !text.contains("essential"),
            "{text}"
        );
    }

    /// A structural fault and an advisory both plan a change, so `outcome` alone cannot say
    /// which row made a surface exit 1: only an essential row left wrong does, and a change
    /// that did not land fails any surface. Done, does not apply and waits for an idle Mac
    /// never fail it.
    #[test]
    fn only_an_essential_row_left_wrong_fails_its_surface() {
        let mut r = row();
        for (outcome, essential_fails) in [
            (Outcome::Ok, false),
            (Outcome::Changed, false),
            (Outcome::NotApplicable, false),
            (Outcome::Deferred, false),
            (Outcome::Planned, true),
            (Outcome::Person, true),
            (Outcome::Blocked, true),
            (Outcome::Unknown, true),
            (Outcome::Failed, true),
        ] {
            r.outcome = outcome;
            r.essential = true;
            assert_eq!(r.fails_surface(), essential_fails, "essential {outcome:?}");
            r.essential = false;
            assert_eq!(
                r.fails_surface(),
                outcome == Outcome::Failed,
                "advisory {outcome:?}"
            );
        }
    }

    /// The one envelope every `--json` surface prints: the schema, the mode, whether the
    /// surface's exit rule held, and the rows.
    #[test]
    fn a_report_round_trips_in_the_one_schema() {
        let mut advisory = row();
        advisory.essential = false;
        advisory.outcome = Outcome::Planned;
        let report = Report::new(ReportMode::Check, false, vec![row(), advisory]);
        let text = aterm_json::to_string(&report).unwrap();
        assert!(
            text.starts_with(r#"{"schema":1,"mode":"check","complete":false,"items":[{"#),
            "{text}"
        );
        assert_eq!(text.matches(r#""essential":true"#).count(), 1, "{text}");
        assert_eq!(aterm_json::from_str::<Report>(&text).unwrap(), report);
        for mode in [ReportMode::Apply, ReportMode::Plan, ReportMode::Check] {
            let text = aterm_json::to_string(&Report::new(mode, true, Vec::new())).unwrap();
            assert_eq!(aterm_json::from_str::<Report>(&text).unwrap().mode, mode);
        }
    }

    #[test]
    fn standing_aside_ends_with_atpkgs_own_exit_statuses() {
        assert_eq!(StoodAside::Lock(String::new()).exit_status(), 75);
        assert_eq!(StoodAside::Offline(String::new()).exit_status(), 69);
    }
}
