// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only account recovery facts shared by session and command projections.

use std::collections::BTreeSet;

use super::accounts::{self, Logins, Me, Point, Roster, State};

/// Fixed explanation safe for projections that omit account-specific findings.
pub const IDENTITY_MISMATCH_FINDING: &str = "Alternate login identity mismatch";

/// The captured decision cannot classify a different account binding.
pub const BINDINGS_CHANGED_FINDING: &str = "Account bindings changed; eligibility unknown";

/// Fixed findings from the execution gate, independent of optional account rotation.
pub const EXECUTION_PREPARED_FINDING: &str = "Account has preparation-only access";
pub const EXECUTION_ROSTER_UNREAD_FINDING: &str = "Account roster is unreadable";
pub const EXECUTION_CONTEXT_UNREAD_FINDING: &str = "Account execution context is unconfirmed";
pub const EXECUTION_RECORDS_UNREAD_FINDING: &str = "Account execution records are unreadable";
pub const EXECUTION_REFRESH_FINDING: &str = "Awaiting a fresh account execution check";
pub const EXECUTION_HISTORY_PREFIX: &str = "Previous execution check: ";
pub const OBSERVER_BUSY_FINDING: &str = "Account observer is busy; current recovery is unknown";
pub const RECOVERY_HISTORY_PREFIX: &str = "Previous recovery observation: ";
pub const SESSION_AUTOPILOT_OFF_FINDING: &str =
    "Session Autopilot is off; no further account recovery is scheduled";
pub const RECOVERY_RESUME_FINDING: &str =
    "Awaiting a fresh account recovery decision after Autopilot Off";

#[cfg(test)]
#[path = "account_progress_ranking_tests.rs"]
mod ranking_tests;

#[cfg(test)]
#[path = "account_progress_identity_conformance.rs"]
mod identity_conformance;

#[cfg(any(test, debug_assertions))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountProgressRanking",
        action = "Case",
        reason = "Native fixture chooses trigger, current preparation or lease authority, and alternate capacity"
    )
)]
#[doc(hidden)]
pub fn account_progress_ranking_environment() {}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ProgressPhase {
    Unknown,
    /// Selection is paused, or awaits a decision after its supervisor resumed.
    Paused,
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

/// A blocker observed for the session's own account, without account or machine labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OwnClaimBlocker {
    HeldElsewhere { until: i64 },
    CannotRecord,
}

/// Claim evidence from the same retained decision as the account inventory.
/// It grants no current execution or switching authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OwnClaimFinding {
    pub blocker: OwnClaimBlocker,
    pub pinned: bool,
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
    /// Validated historical provider facts, separate from free-text route records.
    pub login_diagnostics: Vec<super::accounts_sync::LoginDiagnostic>,
    pub next_at: Option<i64>,
    pub own_claim: Option<OwnClaimFinding>,
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
            login_diagnostics: Vec::new(),
            next_at: None,
            own_claim: None,
        }
    }
}

/// Add inventory and retained sign-in findings to an already observed decision.
/// Uses the caller's actual session identity and trigger; it neither runs advice
/// nor substitutes measured capacity for an unread account. Eligibility stays
/// unknown when no trigger was judged.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountProgressRanking",
        action = "Project",
        project = "aterm_agent::harness::account_progress::ranking_tests::ranking_transition"
    )
)]
pub fn enrich_inventory(
    progress: &mut AccountProgress,
    roster: &Roster,
    state: &State,
    logins: &Logins,
    enrolled: &BTreeSet<String>,
    me: &Me<'_>,
    point: Option<&Point>,
) {
    enrich_inventory_with_identity(progress, roster, state, logins, enrolled, me, point, |_| {
        true
    });
}

/// Add inventory using the same captured identity admission as the caller's decision.
/// The predicate is asked only about policy-eligible alternatives. A contradiction
/// excludes a candidate without changing its recorded authentication state.
#[expect(
    clippy::too_many_arguments,
    reason = "The existing projection inputs share the decision's captured identity predicate"
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountProgressIdentity",
        action = "Project",
        project = "aterm_agent::harness::account_progress::identity_conformance::identity_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountProgressIdentity",
        action = "Case",
        reason = "The native fixture chooses captured policy and identity verdicts"
    )
)]
pub fn enrich_inventory_with_identity(
    progress: &mut AccountProgress,
    roster: &Roster,
    state: &State,
    logins: &Logins,
    enrolled: &BTreeSet<String>,
    me: &Me<'_>,
    point: Option<&Point>,
    mut identity_matches: impl FnMut(&str) -> bool,
) {
    progress.known = Some(roster.rows.len());
    let candidates = point.map(|point| {
        let ranked_at =
            accounts::effective_ranking_point(roster, state, me, point, progress.observed_at);
        accounts::candidates(
            roster,
            state,
            enrolled,
            me.label(),
            ranked_at.as_ref(),
            progress.observed_at,
        )
    });
    let identity_rejected: BTreeSet<_> = candidates
        .as_ref()
        .into_iter()
        .flatten()
        .filter(|(label, result)| result.is_ok() && !identity_matches(label))
        .map(|(label, _)| label.clone())
        .collect();
    if !identity_rejected.is_empty() {
        progress.details.push(IDENTITY_MISMATCH_FINDING.into());
    }
    progress.eligible = candidates.as_ref().map(|candidates| {
        candidates
            .iter()
            .filter(|(label, result)| result.is_ok() && !identity_rejected.contains(label))
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
        let before = progress.details.len();
        let label = &row.label;
        if state.account(label).auth == accounts::Auth::SignedOut {
            progress.details.push(format!("{label}: signed out"));
        }
        let binding = logins.get(label);
        if let Some(task) = binding.and_then(|binding| binding.sign_in_task) {
            let action = match task.action {
                accounts::SignInAction::Browser => "browser sign-in needed",
                accounts::SignInAction::Login => "CLI sign-in needed",
            };
            progress.details.push(format!("{label}: {action}"));
            progress
                .details
                .push(task.action.next_step(accounts::Cli::Claude, label));
        }
        if let Some(attempt) = binding.and_then(|binding| binding.sign_in_attempt.as_ref()) {
            let outcome = match attempt.outcome {
                accounts::AttemptOutcome::NoRoute => Some("no usable sign-in route"),
                accounts::AttemptOutcome::Failed => Some("sign-in route failed"),
                accounts::AttemptOutcome::Blocked => Some("sign-in route stopped by its page"),
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
        if identity_rejected.contains(&label) {
            progress.details.push(format!(
                "{label}: stored login does not match expected identity"
            ));
            continue;
        }
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
    // The other rows' reasons say only why none can take the session; why it may not keep its own
    // account leads while it waits, as that is why it waits (a turn end's yield,
    // docs/DESIGN-fluid-swarm-2026-10-28.md F19).
    progress.own_claim = None;
    if let Some((finding, line)) = own_claim_note(roster, state, enrolled, me, progress.observed_at)
    {
        if progress.account.as_deref() == me.label() {
            progress.own_claim = Some(finding);
        }
        if progress.phase == ProgressPhase::Waiting {
            progress.details.insert(0, line);
        } else {
            progress.details.push(line);
        }
    }
}

/// Why this Mac may not keep the session's own account at `now` as its claims stand
/// ([`State::lease_verdict`]: another Mac's senior live claim, or claims this Mac cannot record
/// and the account not one of its home accounts), naming the owner's pin where it keeps the
/// session from moving ([`accounts::begin_rotation`] admits no rotation off a pinned `active` that
/// still runs); `None` where the claims leave the account to this Mac, and for a preparation-only
/// account, whose hold the execution gate reports in its own fixed words
/// ([`EXECUTION_PREPARED_FINDING`]). The cause leads, after the label, as the composer rule shows
/// only a detail's first 32 columns.
fn own_claim_note(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    me: &Me<'_>,
    now: i64,
) -> Option<(OwnClaimFinding, String)> {
    let label = me.label()?;
    let blocker = match state.lease_verdict(label, now) {
        Err(accounts::WhyNot::LeasedElsewhere { until }) => {
            OwnClaimBlocker::HeldElsewhere { until }
        }
        Err(accounts::WhyNot::Unclaimed) => OwnClaimBlocker::CannotRecord,
        _ => return None,
    };
    let why = match blocker {
        OwnClaimBlocker::HeldElsewhere { until } => format!(
            "another Mac holds it until {}",
            super::usage::rfc3339_utc(until)
        ),
        OwnClaimBlocker::CannotRecord => "this Mac cannot claim it: it cannot record account \
                                        claims, and this is not one of its home accounts"
            .to_owned(),
    };
    let pin = state
        .active
        .as_deref()
        .filter(|_| state.pinned)
        .and_then(|active| roster.row(active))
        .filter(|row| {
            let here = enrolled.contains(&row.label);
            accounts::condition(row, state, here, now).runs()
        });
    let finding = OwnClaimFinding {
        blocker,
        pinned: pin.is_some(),
    };
    let line = match pin {
        Some(row) => format!(
            "{label}: {why}; the pin on {} keeps the session from moving",
            row.label
        ),
        None => format!("{label}: {why}"),
    };
    Some((finding, line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_signed_out_account_is_explained_without_a_sign_in_attempt() {
        let roster = Roster {
            rows: vec![accounts::Row {
                label: "current".into(),
                ..Default::default()
            }],
        };
        let mut state = State::default();
        accounts::note_auth(&mut state, "current", accounts::Auth::SignedOut, 90);
        let mut progress = AccountProgress::unknown(100);
        progress.phase = ProgressPhase::Waiting;
        progress
            .details
            .push("No eligible alternate account".into());
        enrich_inventory(
            &mut progress,
            &roster,
            &state,
            &Logins::default(),
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
        assert_eq!(progress.known, Some(1));
        assert_eq!(progress.eligible, Some(0));
        assert_eq!(progress.phase, ProgressPhase::Waiting);
        assert!(progress.details[0].contains("current: signed out"));
        for narrow in [false, true] {
            assert!(
                super::super::footer::account_progress_text(&progress, narrow)
                    .contains("current: signed out")
            );
        }
    }

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

    /// A session whose turn end waits because this Mac may not keep its own account
    /// (`docs/DESIGN-fluid-swarm-2026-10-28.md` F19: claims this Mac cannot record off its home
    /// accounts, or another Mac's senior claim) says why first on the tab's account fact, ahead of
    /// the other rows' reasons, and names the owner's pin where that keeps the session from
    /// moving. Out of a wait the line goes last; with the claims leaving the account to this Mac,
    /// nothing is said of it, nor of a preparation-only account, whose hold the execution gate
    /// reports in its own fixed words.
    ///
    /// Negative controls, in separate builds: with `own_claim_note`'s line filtered out of
    /// `enrich_inventory_with_identity`, the first assertion failed with `left: Some("main")`,
    /// `right: Some("pool-a")`: the walled home row's reason led. With `own_claim_note` saying any
    /// other refusal of `lease_verdict` in its own words (`Err(other) => other.as_str()`, the arm
    /// it had before the preparation hold reached `lease_verdict`), the last assertion failed:
    /// `["pool-a: prepared: awaiting execution authority", "main: limited until
    /// 2026-09-21T17:15:50Z"]`.
    #[test]
    fn a_wait_on_an_account_this_mac_may_not_keep_says_why_first() {
        const NOW: i64 = 1_790_000_000;
        let roster = Roster {
            rows: ["main", "pool-a"]
                .map(|label| accounts::Row {
                    label: label.into(),
                    dir: Some(format!("/synthetic/{label}")),
                    ..Default::default()
                })
                .into(),
        };
        let enrolled = BTreeSet::from(["main".to_string(), "pool-a".to_string()]);
        let me = Me {
            account: Some("pool-a"),
            left: None,
            default_home: true,
            stored_login: true,
        };
        let mut unclaimed = State {
            machine: "0a0b".into(),
            active: Some("pool-a".into()),
            proven: true,
            unrecorded: true,
            home_accounts: BTreeSet::from(["main".to_string()]),
            ..State::default()
        };
        let walled = accounts::note_wall(
            &mut unclaimed,
            "main",
            "five_hour",
            Some(NOW + 3 * 3600),
            NOW - 60,
            NOW - 60,
        );
        assert!(!walled.is_empty());
        let observe = |state: &State, phase: ProgressPhase, now| {
            let mut progress = AccountProgress::unknown(now);
            progress.account = Some("pool-a".into());
            progress.phase = phase;
            enrich_inventory(
                &mut progress,
                &roster,
                state,
                &Logins::default(),
                &enrolled,
                &me,
                Some(&Point::TurnEnd),
            );
            progress
        };
        let read = |state: &State, phase| observe(state, phase, NOW).details;
        let label = |line: &String| line.split(':').next().map(str::to_owned);
        let first = |details: &[String]| details.first().and_then(label);

        let waits = read(&unclaimed, ProgressPhase::Waiting);
        assert_eq!(first(&waits), Some("pool-a".to_string()));
        assert_eq!(
            observe(&unclaimed, ProgressPhase::Waiting, NOW).own_claim,
            Some(OwnClaimFinding {
                blocker: OwnClaimBlocker::CannotRecord,
                pinned: false,
            })
        );
        assert!(
            waits
                .iter()
                .skip(1)
                .any(|line| label(line).as_deref() == Some("main")),
            "the walled home row's reason follows: {waits:?}"
        );

        let pinned = State {
            pinned: true,
            ..unclaimed.clone()
        };
        let held = read(&pinned, ProgressPhase::Waiting);
        assert!(
            observe(&pinned, ProgressPhase::Waiting, NOW)
                .own_claim
                .unwrap()
                .pinned
        );
        assert!(
            held[0]
                .strip_prefix(waits[0].as_str())
                .is_some_and(|rest| rest.contains("pool-a")),
            "the pin is named after the cause: {held:?}"
        );

        let mut senior = unclaimed.clone();
        senior.unrecorded = false;
        senior.note_lease("pool-a", "1c1d", NOW + 3600, NOW - 60);
        let elsewhere = read(&senior, ProgressPhase::Waiting);
        let captured = observe(&senior, ProgressPhase::Waiting, NOW);
        assert_eq!(
            captured.own_claim,
            Some(OwnClaimFinding {
                blocker: OwnClaimBlocker::HeldElsewhere { until: NOW + 3600 },
                pinned: false,
            })
        );
        assert_eq!(captured.observed_at, NOW);
        assert!(
            observe(&senior, ProgressPhase::Waiting, NOW + 3600)
                .own_claim
                .is_none()
        );
        assert_eq!(first(&elsewhere), Some("pool-a".to_string()));
        assert!(elsewhere[0].contains(&super::super::usage::rfc3339_utc(NOW + 3600)));

        let ready = read(&unclaimed, ProgressPhase::Ready);
        assert_ne!(first(&ready), Some("pool-a".to_string()));
        assert_eq!(ready.last().and_then(label), Some("pool-a".to_string()));

        let mut recorded = unclaimed.clone();
        recorded.unrecorded = false;
        assert!(
            read(&recorded, ProgressPhase::Waiting)
                .iter()
                .all(|line| label(line).as_deref() != Some("pool-a")),
            "claims this Mac records leave its own account unremarked"
        );
        assert!(
            observe(&recorded, ProgressPhase::Waiting, NOW)
                .own_claim
                .is_none()
        );

        let mut prepared = unclaimed.clone();
        prepared.prepared_accounts.insert("pool-a".to_string());
        let gated = read(&prepared, ProgressPhase::Waiting);
        assert!(
            gated
                .iter()
                .all(|line| label(line).as_deref() != Some("pool-a")),
            "a preparation-only account is the execution gate's to report: {gated:?}"
        );
        assert!(
            observe(&prepared, ProgressPhase::Waiting, NOW)
                .own_claim
                .is_none()
        );

        for account in [None, Some("main")] {
            let mut changed = captured.clone();
            changed.account = account.map(str::to_owned);
            enrich_inventory(
                &mut changed,
                &roster,
                &senior,
                &Logins::default(),
                &enrolled,
                &me,
                Some(&Point::TurnEnd),
            );
            assert!(
                changed.own_claim.is_none(),
                "a stale binding cannot retain the own-account finding"
            );
        }
    }
}
