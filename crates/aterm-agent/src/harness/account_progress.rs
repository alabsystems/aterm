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

/// Separately observed local work, without an account decision or retry promise.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ActiveSignIn {
    pub label: String,
    pub route: Option<String>,
    pub observed_at: i64,
}

/// An observed session decision, not an instruction to activate credentials.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AccountProgress {
    /// This pane's session policy withdrew future automatic recovery authority.
    /// Independently observed work can still retain its in-flight phase.
    pub forecasts_paused: bool,
    pub observed_at: i64,
    pub account: Option<String>,
    pub known: Option<usize>,
    pub eligible: Option<usize>,
    pub phase: ProgressPhase,
    pub wall: Option<String>,
    pub details: Vec<String>,
    /// Validated historical provider facts, separate from free-text route records.
    /// Fresh scoped pool outcomes can accompany a captured decision without renewing it.
    pub login_diagnostics: Vec<super::accounts_sync::LoginDiagnostic>,
    /// Current scoped sign-in work independent of the retained decision or execution gate.
    /// An empty observation grants no authority and does not prove other work is absent.
    pub independent_work: Vec<ActiveSignIn>,
    /// Earliest observed reevaluation or retry event, not a promise of account readiness.
    /// Independent holds on one unchanged account end at their latest deadline; alternatives
    /// and normal decision rechecks contribute their earliest event. A live execution gate
    /// keeps its own recheck deadline when login activity is added.
    pub next_at: Option<i64>,
    pub own_claim: Option<OwnClaimFinding>,
    /// Why the session stays on its account, as the turn-end advice's closed word says it
    /// (`pinned`, `other-home`, `not-a-stored-login`, `keyring-store`, `off`, …), where the
    /// observer that built this progress held one. The host that pushes the word's sentence into
    /// [`Self::details`] sets it; a reader that says its own sentence goes by the word.
    pub stay: Option<String>,
}

impl AccountProgress {
    /// This progress as the glass shows it: each account it names by roster label (the decided
    /// account, a switch's or a sign-in's target, each independent sign-in) renamed by `name`,
    /// the label's shown name (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §2.5, owner
    /// ruling 2026-11-09: an account is named by its email wherever it is shown). Display only:
    /// the labels stay the keys everywhere else. A detail that leads with a label
    /// ([`enrich_inventory_with_identity`]'s `<label>: signed out`) is renamed too: its head up
    /// to the first `: `, where that head holds no blank (a label never does; a finding's fixed
    /// words do) and `name` gives it another name.
    #[must_use]
    pub fn named(&self, name: impl Fn(&str) -> String) -> Self {
        let mut out = self.clone();
        for detail in &mut out.details {
            if let Some((head, rest)) = detail.split_once(": ")
                && !head.is_empty()
                && !head.contains(char::is_whitespace)
            {
                let shown = name(head);
                if shown != head {
                    *detail = format!("{shown}: {rest}");
                }
            }
        }
        if let Some(account) = &mut out.account {
            *account = name(account);
        }
        match &mut out.phase {
            ProgressPhase::Switching { to } | ProgressPhase::Verifying { to } => *to = name(to),
            ProgressPhase::SigningIn { label, .. } => *label = name(label),
            ProgressPhase::Unknown
            | ProgressPhase::Paused
            | ProgressPhase::Watching
            | ProgressPhase::Disabled
            | ProgressPhase::Limited
            | ProgressPhase::Waiting
            | ProgressPhase::Probing
            | ProgressPhase::Ready => {}
        }
        for work in &mut out.independent_work {
            work.label = name(&work.label);
        }
        out
    }

    #[must_use]
    pub fn unknown(observed_at: i64) -> Self {
        Self {
            forecasts_paused: false,
            observed_at,
            account: None,
            known: None,
            eligible: None,
            phase: ProgressPhase::Unknown,
            wall: None,
            details: Vec::new(),
            login_diagnostics: Vec::new(),
            independent_work: Vec::new(),
            next_at: None,
            own_claim: None,
            stay: None,
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
#[expect(
    clippy::too_many_arguments,
    reason = "Current workflow policy and bound login decisions use separate captured rosters"
)]
pub fn enrich_inventory(
    progress: &mut AccountProgress,
    roster: &Roster,
    raw_roster: &Roster,
    state: &State,
    logins: &Logins,
    enrolled: &BTreeSet<String>,
    me: &Me<'_>,
    point: Option<&Point>,
) {
    enrich_inventory_with_identity(
        progress,
        roster,
        raw_roster,
        state,
        logins,
        enrolled,
        me,
        point,
        |_| true,
    );
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
    raw_roster: &Roster,
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
        if state.account(label).current_auth(row) == accounts::Auth::SignedOut {
            progress.details.push(format!("{label}: signed out"));
        }
        let raw = raw_roster.row(label);
        if let Some(task) = raw.and_then(|raw| accounts::current_sign_in_task(raw, logins)) {
            let action = match task.action {
                accounts::SignInAction::Browser => "browser sign-in needed",
                accounts::SignInAction::Login => "CLI sign-in needed",
                accounts::SignInAction::Renew => "sign-in at this Mac's terminal needed",
            };
            progress.details.push(format!("{label}: {action}"));
            progress
                .details
                .push(task.action.next_step(accounts::Cli::Claude, label));
        }
        if let Some(attempt) = raw.and_then(|raw| accounts::current_sign_in_attempt(raw, logins)) {
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
#[path = "account_progress_tests.rs"]
mod tests;
