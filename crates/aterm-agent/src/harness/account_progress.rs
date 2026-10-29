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
    // The current account may need sign-in too; evidence is independent of
    // whether the candidate selector can switch to it.
    for row in &roster.rows {
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
        let before = progress.details.len();
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
        if let Some(attempt) = &binding.sign_in_attempt {
            let outcome = match attempt.outcome {
                accounts::AttemptOutcome::NoRoute => Some("no usable sign-in route"),
                accounts::AttemptOutcome::Failed => Some("sign-in route failed"),
                accounts::AttemptOutcome::SignedIn => None,
            };
            if let Some(outcome) = outcome {
                progress.details.push(format!(
                    "{label}: {outcome}; first recorded {}: {}",
                    super::usage::rfc3339_utc(attempt.since),
                    attempt.routes
                ));
            }
        }
        if Some(label.as_str()) == me.label()
            && progress.eligible == Some(0)
            && matches!(
                progress.phase,
                ProgressPhase::Waiting | ProgressPhase::Limited
            )
        {
            // Keep this account's notes together, ahead of generic waiting
            // reasons, without disturbing an active recovery action.
            let added = progress.details.len() - before;
            progress.details.rotate_right(added);
        }
    }
    for (label, result) in candidates.into_iter().flatten() {
        if let Err(why) = result {
            progress.details.push(match why {
                accounts::WhyNot::Me => continue,
                accounts::WhyNot::Limited { until } => format!(
                    "{label}: limited until {}",
                    super::usage::rfc3339_utc(until)
                ),
                accounts::WhyNot::LeasedElsewhere { until } => format!(
                    "{label}: leased elsewhere until {}",
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
    fn current_blocker_leads_only_when_recovery_has_no_eligible_alternate() {
        let roster = Roster {
            rows: ["current", "alternate"]
                .map(|label| accounts::Row {
                    label: label.into(),
                    dir: Some(format!("/synthetic/{label}")),
                    ..Default::default()
                })
                .into(),
        };
        let mut state = State::default();
        let mut logins = Logins::default();
        accounts::note_login_end(&mut state, &mut logins, "current", 80);
        accounts::note_sign_in_task(&mut logins, "current", accounts::SignInAction::Login, 90);
        let point = Point::Wall {
            window: "seven_day".into(),
            at: Some(90),
        };
        let me = Me {
            account: Some("current"),
            left: None,
            default_home: true,
            stored_login: true,
        };
        let generic = "synthetic supervisor reason";
        for (phase, judged, alternate_ready, prefer) in [
            (ProgressPhase::Waiting, true, false, true),
            (ProgressPhase::Limited, true, false, true),
            (ProgressPhase::Waiting, true, true, false),
            (ProgressPhase::Waiting, false, false, false),
            (
                ProgressPhase::SigningIn {
                    label: "alternate".into(),
                    route: None,
                },
                true,
                false,
                false,
            ),
            (
                ProgressPhase::Switching {
                    to: "alternate".into(),
                },
                true,
                false,
                false,
            ),
            (
                ProgressPhase::Verifying {
                    to: "alternate".into(),
                },
                true,
                false,
                false,
            ),
            (ProgressPhase::Ready, true, false, false),
            (ProgressPhase::Probing, true, false, false),
            (ProgressPhase::Watching, true, false, false),
            (ProgressPhase::Disabled, true, false, false),
            (ProgressPhase::Unknown, true, false, false),
        ] {
            let mut progress = AccountProgress::unknown(100);
            progress.phase = phase.clone();
            progress.wall = Some("weekly".into());
            progress.next_at = Some(160);
            progress.details.push(generic.into());
            let mut enrolled = BTreeSet::from(["current".into()]);
            if alternate_ready {
                enrolled.insert("alternate".into());
            }
            enrich_inventory(
                &mut progress,
                &roster,
                &state,
                &logins,
                &enrolled,
                &me,
                judged.then_some(&point),
            );
            assert_eq!(progress.phase, phase);
            assert_eq!(progress.known, Some(2));
            assert_eq!(
                progress.eligible,
                judged.then_some(usize::from(alternate_ready))
            );
            assert_eq!(progress.next_at, Some(160));
            assert_eq!(progress.wall.as_deref(), Some("weekly"));
            assert!(progress.details.iter().any(|line| line == generic));
            if prefer {
                assert!(progress.details[0].starts_with("current:"));
                for narrow in [false, true] {
                    assert!(
                        super::super::footer::account_progress_text(&progress, narrow)
                            .contains(&progress.details[0])
                    );
                }
            } else {
                assert_eq!(progress.details[0], generic);
            }
        }
    }

    #[test]
    fn current_login_failure_is_visible_without_becoming_a_rotation_candidate() {
        let roster = Roster {
            rows: vec![
                accounts::Row {
                    label: "current".into(),
                    dir: Some("/synthetic/current".into()),
                    ..Default::default()
                },
                accounts::Row {
                    label: "alternate".into(),
                    ..Default::default()
                },
            ],
        };
        let mut state = State::default();
        let mut logins = Logins::default();
        accounts::note_login_end(&mut state, &mut logins, "current", 80);
        accounts::note_sign_in_task(&mut logins, "current", accounts::SignInAction::Login, 85);
        accounts::note_sign_in_attempt(
            &mut logins,
            "current",
            accounts::AttemptOutcome::NoRoute,
            "profile(none)",
            90,
        );
        // `alternate`, the default login, is signed in here as `accounts::enrolled_for` holds it.
        let enrolled = BTreeSet::from(["current".into(), "alternate".into()]);
        let project = |state: &State| {
            let mut progress = AccountProgress::unknown(100);
            progress.phase = ProgressPhase::Waiting;
            enrich_inventory(
                &mut progress,
                &roster,
                state,
                &logins,
                &enrolled,
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
            progress
        };
        let failed = project(&state);
        assert_eq!((failed.known, failed.eligible), (Some(2), Some(1)));
        assert_eq!(failed.phase, ProgressPhase::Waiting);
        assert!(
            failed.details.contains(
                &accounts::SignInAction::Login.next_step(accounts::Cli::Claude, "current")
            )
        );
        assert!(
            failed
                .details
                .iter()
                .any(|line| { line.starts_with("current:") && line.contains("profile(none)") })
        );
        let recovered = project(&State::default());
        assert_eq!(recovered.eligible, failed.eligible);
        assert!(recovered.details.is_empty());
    }

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
