// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only account recovery facts shared by session and command projections.

use std::collections::BTreeSet;

use super::accounts::{self, Logins, Me, Point, Roster, State};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ProgressPhase {
    Unknown,
    Watching,
    Disabled,
    Limited,
    Waiting,
    Probing,
    Ready,
    Switching {
        to: String,
    },
    Verifying {
        to: String,
    },
    SigningIn {
        label: String,
        route: Option<String>,
    },
}

/// An observed session decision, not an instruction to activate credentials.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AccountProgress {
    pub observed_at: i64,
    pub account: Option<String>,
    pub known: Option<usize>,
    pub eligible: Option<usize>,
    pub phase: ProgressPhase,
    pub wall: Option<String>,
    pub details: Vec<String>,
    pub next_at: Option<i64>,
}

impl AccountProgress {
    #[must_use]
    pub fn unknown(observed_at: i64) -> Self {
        Self {
            observed_at,
            account: None,
            known: None,
            eligible: None,
            phase: ProgressPhase::Unknown,
            wall: None,
            details: Vec::new(),
            next_at: None,
        }
    }
}

/// Add inventory and retained sign-in findings to an already observed decision.
/// Uses the caller's actual session identity and trigger; it neither runs advice
/// nor substitutes measured capacity for an unread account. Eligibility stays
/// unknown when no trigger was judged.
pub fn enrich_inventory(
    progress: &mut AccountProgress,
    roster: &Roster,
    state: &State,
    logins: &Logins,
    enrolled: &BTreeSet<String>,
    me: &Me<'_>,
    point: Option<&Point>,
) {
    progress.known = Some(roster.rows.len());
    let candidates = point.map(|point| {
        accounts::candidates(
            roster,
            state,
            enrolled,
            me.label(),
            point,
            progress.observed_at,
        )
    });
    progress.eligible = candidates.as_ref().map(|candidates| {
        candidates
            .iter()
            .filter(|(_, result)| result.is_ok())
            .count()
    });
    for row in &roster.rows {
        if Some(row.label.as_str()) == me.label() {
            continue;
        }
        if !accounts::needs_sign_in(
            row,
            state,
            logins,
            row.dir.is_none() || enrolled.contains(&row.label),
            progress.observed_at,
        ) {
            continue;
        }
        let Some(binding) = logins.get(&row.label) else {
            continue;
        };
        let label = &row.label;
        if let Some(task) = binding.sign_in_task {
            let action = match task.action {
                accounts::SignInAction::Browser => "browser sign-in needed",
                accounts::SignInAction::Login => "CLI sign-in needed",
            };
            progress.details.push(format!("{label}: {action}"));
            progress
                .details
                .push(task.action.next_step(accounts::Cli::Claude, label));
        }
        let Some(attempt) = &binding.sign_in_attempt else {
            continue;
        };
        let outcome = match attempt.outcome {
            accounts::AttemptOutcome::NoRoute => "no usable sign-in route",
            accounts::AttemptOutcome::Failed => "sign-in route failed",
            accounts::AttemptOutcome::SignedIn => continue,
        };
        progress.details.push(format!(
            "{label}: {outcome}; first recorded {}: {}",
            super::usage::rfc3339_utc(attempt.since),
            attempt.routes
        ));
    }
    for (label, result) in candidates.into_iter().flatten() {
        if let Err(why) = result {
            progress.details.push(match why {
                accounts::WhyNot::Me => continue,
                accounts::WhyNot::Limited { until } => format!(
                    "{label}: limited until {}",
                    super::usage::rfc3339_utc(until)
                ),
                other => format!("{label}: {}", other.as_str()),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_human_steps_are_visible_only_while_sign_in_is_needed() {
        let roster = Roster {
            rows: vec![accounts::Row {
                label: "alternate".into(),
                dir: Some("/synthetic/alternate".into()),
                ..Default::default()
            }],
        };
        let state = State::default();
        let me = Me {
            account: Some("current"),
            left: None,
            default_home: true,
            stored_login: true,
        };
        let point = Point::Wall {
            window: "seven_day".into(),
            at: Some(90),
        };
        let project = |logins: &Logins, enrolled: &BTreeSet<String>| {
            let mut progress = AccountProgress::unknown(100);
            enrich_inventory(
                &mut progress,
                &roster,
                &state,
                logins,
                enrolled,
                &me,
                Some(&point),
            );
            progress
        };
        let baseline = project(&Logins::default(), &BTreeSet::new());
        for action in [
            accounts::SignInAction::Browser,
            accounts::SignInAction::Login,
        ] {
            let mut logins = Logins::default();
            accounts::note_sign_in_task(&mut logins, "alternate", action, 90);
            let progress = project(&logins, &BTreeSet::new());
            assert_eq!(
                (progress.known, progress.eligible),
                (baseline.known, baseline.eligible)
            );
            assert_eq!(progress.details[0].split(':').next(), Some("alternate"));
            let instruction = action.next_step(accounts::Cli::Claude, "alternate");
            assert!(progress.details.contains(&instruction));
            let ready = project(&logins, &BTreeSet::from(["alternate".into()]));
            assert_eq!(ready.eligible, Some(1));
            assert!(!ready.details.contains(&instruction));
            assert!(ready.details.is_empty());
        }
    }

    #[test]
    fn inventory_keeps_the_real_session_and_does_not_invent_capacity_or_a_decision() {
        let roster = Roster {
            rows: vec![
                accounts::Row {
                    label: "current".into(),
                    ..Default::default()
                },
                accounts::Row {
                    label: "missing".into(),
                    dir: Some("/synthetic/missing".into()),
                    ..Default::default()
                },
                accounts::Row {
                    label: "unlogged".into(),
                    dir: Some("/synthetic/login".into()),
                    ..Default::default()
                },
            ],
        };
        let state = State::default();
        let mut logins = Logins::default();
        logins.labels.insert(
            "unlogged".into(),
            accounts::Binding {
                sign_in_attempt: Some(accounts::SignInAttempt {
                    outcome: accounts::AttemptOutcome::NoRoute,
                    routes: "profile(none)".into(),
                    since: 80,
                }),
                ..Default::default()
            },
        );
        let mut progress = AccountProgress::unknown(100);
        progress.account = Some("current".into());
        progress.phase = ProgressPhase::Limited;
        enrich_inventory(
            &mut progress,
            &roster,
            &state,
            &logins,
            &BTreeSet::new(),
            &Me {
                account: Some("current"),
                left: None,
                default_home: true,
                stored_login: true,
            },
            Some(&Point::Wall {
                window: "seven_day".into(),
                at: Some(90),
            }),
        );
        assert_eq!((progress.known, progress.eligible), (Some(3), Some(0)));
        assert_eq!(progress.phase, ProgressPhase::Limited);
        assert_eq!(progress.next_at, None);
        assert!(!progress.details.is_empty());
        assert_eq!(progress.details[0].split(':').next(), Some("unlogged"));
        assert!(
            progress
                .details
                .iter()
                .all(|line| line.split(':').next() != Some("current"))
        );
        assert!(state.accounts.is_empty());

        progress = AccountProgress::unknown(100);
        enrich_inventory(
            &mut progress,
            &roster,
            &state,
            &logins,
            &BTreeSet::new(),
            &Me {
                account: Some("current"),
                left: None,
                default_home: true,
                stored_login: true,
            },
            None,
        );
        assert_eq!((progress.known, progress.eligible), (Some(3), None));
        assert!(
            progress
                .details
                .iter()
                .any(|line| line.starts_with("unlogged:"))
        );
    }
}
