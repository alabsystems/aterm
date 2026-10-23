// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared convergence vocabulary: host reads, probe verdicts, outcomes, layers, and report rows.

use serde::{Deserialize, Serialize};
use std::fmt;

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

/// Why an apply stood aside without changing anything and without failing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StoodAside {
    /// Another run held a lock the change needs (atpkg's exit 75); the line says whose.
    Lock(String),
    /// A network the change needs is down (atpkg's exit 69).
    Offline(String),
    /// The change waits on another person's action (such as admin approval).
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

    #[test]
    fn readiness_is_stricter_than_the_failure_policy_and_requires_evidence() {
        assert!(!Readiness::of(&[], &[]).all_green);
        let mut r = row();
        r.essential = false;
        for outcome in [
            Outcome::Ok,
            Outcome::Changed,
            Outcome::NotApplicable,
            Outcome::Planned,
            Outcome::Person,
            Outcome::Blocked,
            Outcome::Deferred,
            Outcome::Unknown,
            Outcome::Failed,
        ] {
            r.outcome = outcome;
            let ready = matches!(
                outcome,
                Outcome::Ok | Outcome::Changed | Outcome::NotApplicable
            );
            let assessment = Readiness::of(std::slice::from_ref(&r), &["base".into()]);
            assert_eq!(assessment.all_green, ready, "{outcome:?}");
            assert_eq!(assessment.scope, ["base"]);
            assert_eq!(assessment.unresolved.is_empty(), ready, "{outcome:?}");
            assert_eq!(r.fails_surface(), outcome == Outcome::Failed);
        }
    }

    #[test]
    fn optional_readiness_round_trips_without_changing_older_reports() {
        let mut report = Report::new(ReportMode::Check, false, vec![row()]);
        let old = aterm_json::to_string(&report).unwrap();
        assert!(!old.contains("readiness"));
        assert_eq!(aterm_json::from_str::<Report>(&old).unwrap(), report);
        report.readiness = Some(Readiness::of(&report.items, &["base".into()]));
        let text = aterm_json::to_string(&report).unwrap();
        assert_eq!(aterm_json::from_str::<Report>(&text).unwrap(), report);
        assert!(text.contains(r#""all_green":false"#));
    }

    #[test]
    fn changed_settings_await_login_or_restart_without_inventing_pending_work_for_ok_rows() {
        let mut r = row();
        for effect in [
            Effect::Now,
            Effect::Relaunch,
            Effect::Login,
            Effect::Restart,
        ] {
            r.effect = effect;
            r.outcome = Outcome::Changed;
            let readiness = Readiness::of(std::slice::from_ref(&r), &[]);
            let pending = matches!(effect, Effect::Login | Effect::Restart);
            assert_eq!(readiness.all_green, !pending, "{effect:?}");
            assert_eq!(readiness.pending_activation.len(), usize::from(pending));
            assert_eq!(readiness.satisfied, usize::from(!pending));
            r.outcome = Outcome::Ok;
            assert!(Readiness::of(std::slice::from_ref(&r), &[]).all_green);
        }
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
        let old = r#"{"id":"power.sleep","family":"power","title":"sleep","outcome":"changed","effect":"now"}"#;
        let r: ItemReport = aterm_json::from_str(old).unwrap();
        assert_eq!(r.outcome, Outcome::Changed);
        assert_eq!((r.layer, r.class), (None, None));
        assert!(r.explain.is_none() && r.stood_aside.is_none() && !r.essential);
        let text = aterm_json::to_string(&r).unwrap();
        assert!(
            !text.contains("layer") && !text.contains("explain") && !text.contains("essential"),
            "{text}"
        );
    }

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
    fn standing_aside_ends_with_the_reason_specific_exit_status() {
        assert_eq!(StoodAside::Lock(String::new()).exit_status(), 75);
        assert_eq!(StoodAside::Offline(String::new()).exit_status(), 69);
        assert_eq!(StoodAside::Waiting(String::new()).exit_status(), 75);
        assert_eq!(StoodAside::Permission(String::new()).exit_status(), 77);
    }
}
