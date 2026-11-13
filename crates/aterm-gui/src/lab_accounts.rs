// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded local account metadata for the Lab page; collection runs off the UI thread.

use std::path::Path;

use aterm_agent::harness::account_stats::{self, Missing, Tap};
use aterm_agent::harness::accounts::{
    self, AccountRecords, AttemptOutcome, Auth, Cli, SignInAction,
};
use aterm_agent::harness::alab_seen;

const LAST_TIMESTAMP: i64 = 253_402_300_799;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LabAccounts {
    pub observed_at_ms: Option<u64>,
    pub status: String,
    pub rows: Vec<LabAccount>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LabAccount {
    pub label: String,
    pub facts: Vec<(String, String)>,
    /// The quiet offer's one step for its vault token, where one stands
    /// (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.9).
    pub offer: Option<crate::token_shown::TokenOffer>,
}

/// Read only the consistent local metadata snapshot. An admitted interrupted publication
/// may finish its existing recovery; no provider CLI, profile or credential contents are opened.
pub(crate) fn collect(config: &Path, now_ms: u64) -> LabAccounts {
    let Some(state) = crate::harness_host::harness_state() else {
        return unavailable("The local account state location is unavailable");
    };
    let Some(roster) = accounts::roster_beside_for(config, Cli::Claude) else {
        return unavailable("The local account roster location is unavailable");
    };
    let now = seconds(now_ms);
    let store = accounts::Store::at(&state).bounded_by(&[]);
    match store.read_account_records(Some(&roster), now) {
        // What the journal holds now, never waited on: the first ask starts its read, and a later
        // collection shows it.
        Ok(records) => {
            let mut lab = project_with(
                &records,
                aterm_agent::harness::alab_seen::current(false).as_deref(),
                now_ms,
            );
            // The token facts the window last read, read now where a minute has passed (§5.9).
            crate::token_shown::refresh(now);
            with_tokens(&mut lab, crate::token_shown::shown().as_deref());
            lab
        }
        Err(_) => unavailable("Local account records could not be read consistently"),
    }
}

/// Each row's vault token, value-free, and the quiet offer's step where one stands; nothing
/// before the window's first read.
pub(crate) fn with_tokens(lab: &mut LabAccounts, tokens: Option<&crate::token_shown::Tokens>) {
    let Some(tokens) = tokens else {
        return;
    };
    for row in &mut lab.rows {
        let Some(token) = tokens.of(Some(&row.label), None) else {
            continue;
        };
        row.facts.push(("Vault token".into(), token.words.clone()));
        if let Some(offer) = &token.offer {
            row.facts
                .push(("Vault token offer".into(), offer.line.clone()));
        }
        row.offer.clone_from(&token.offer);
    }
}

/// The lab journal's facts of one account: who last used it, retained sign-in records with
/// their ages, and each window with its reading's age, Mac and source.
fn journal_facts(found: &alab_seen::AccountSeen, now: i64, facts: &mut Vec<(String, String)>) {
    if let Some(last) = &found.last_use {
        facts.push((
            "Last used by (lab journal)".into(),
            alab_seen::when(&last.by, last.t, now),
        ));
    }
    if let Some(records) = alab_seen::signed_in_words(&found.signed_in, now) {
        facts.push(("Sign-in records (lab journal)".into(), records));
    }
    for window in &found.windows {
        facts.push((
            format!("Lab {} quota", window.window),
            alab_seen::window_line(window, now),
        ));
    }
}

fn unavailable(status: &str) -> LabAccounts {
    LabAccounts {
        observed_at_ms: None,
        status: status.into(),
        rows: Vec::new(),
    }
}

fn seconds(now_ms: u64) -> i64 {
    i64::try_from(now_ms / 1_000).unwrap_or(i64::MAX)
}

fn recorded_time(at: Option<i64>) -> String {
    at.filter(|at| valid_time(*at))
        .map(aterm_agent::harness::usage::rfc3339_utc)
        .unwrap_or_else(|| "Not recorded".into())
}

fn valid_time(at: i64) -> bool {
    (1..=LAST_TIMESTAMP).contains(&at)
}

fn observation_time(at: Option<i64>, now: i64) -> String {
    let shown = recorded_time(at);
    if at.is_some_and(|at| valid_time(at) && at > now) {
        format!("{shown} (future-dated record)")
    } else {
        shown
    }
}

#[cfg(test)]
fn project(records: &AccountRecords, now_ms: u64) -> LabAccounts {
    project_with(records, None, now_ms)
}

/// The rows of `records`, each with what the lab journal `seen` says of its account (last used
/// by, sign-in records, each window), read from the journal and never from a provider.
fn project_with(
    records: &AccountRecords,
    seen: Option<&alab_seen::Seen>,
    now_ms: u64,
) -> LabAccounts {
    let now = seconds(now_ms);
    let rows: Vec<_> = records.roster.rows.iter().map(|row| {
        let account = records.state.accounts.get(&row.label).cloned().unwrap_or_default();
        let binding = records.logins.get(&row.label);
        let configured = records.raw_roster.row(&row.label);
        let current_task = configured.and_then(|raw| accounts::current_sign_in_task(raw, &records.logins));
        let current_attempt = configured.and_then(|raw| accounts::current_sign_in_attempt(raw, &records.logins));
        let next_step = current_task
            .map(|task| task.action.next_step(Cli::Claude, &row.label))
            .or_else(|| {
                (account.current_auth(row) == Auth::SignedOut
                    || current_attempt.is_some_and(|attempt| attempt.outcome.failing()))
                    .then(|| "Run aterm harness accounts to inspect recovery details".into())
            });
        let auth = match account.auth {
            Auth::Unseen => "Unknown; no sign-in observation",
            Auth::Ok => "Signed in on record; not checked now",
            Auth::SignedOut => "Signed out on record",
        };
        let selection = match records.state.active.as_deref() {
            Some(active) if active == row.label.as_str() && records.state.pinned => "Selected locally; pinned",
            Some(active) if active == row.label.as_str() => "Selected locally",
            Some(_) => "Not selected locally",
            None => "No local selection recorded",
        };
        let mut facts = vec![
            ("Evidence".into(), "Retained local records; current provider identity and sign-in were not checked".into()),
            ("Class".into(), row.class.as_str().into()),
            ("Selection".into(), selection.into()),
            ("Last observed sign-in".into(), auth.into()),
            ("Sign-in observed".into(), observation_time(account.auth_at, now)),
            ("Sign-in record applicability".into(), if account.auth == Auth::Unseen {
                "No sign-in observation to apply"
            } else if account.current_auth(row) != Auth::Unseen {
                "Applies to the configured login; provider not checked now"
            } else {
                "Historical only; sign-in on the current login is unconfirmed"
            }.into()),
            ("Login routing".into(), if row.dir.is_some() {
                "Named local login configured; contents not checked"
            } else {
                "Default login configured; contents not checked"
            }.into()),
            ("Renewal binding".into(), if binding.is_some() { "Recorded" } else { "No binding recorded" }.into()),
        ];
        if row.held_by.is_some() {
            facts.push(("Pool assignment".into(), "Assignment on record; owner not checked here".into()));
        }
        if let Some(binding) = binding {
            facts.push(("Last recorded renewal".into(), observation_time(binding.renewed_at, now)));
            if binding.notice_end.is_some() {
                facts.push(("Expiry notice on record".into(), recorded_time(binding.notice_end)));
            }
            facts.push(("Current expiry estimate".into(), match accounts::login_forecast(row, &records.logins, &records.state) {
                Some(by) if valid_time(by) => format!("{} (estimate)", recorded_time(Some(by))),
                Some(_) => "Unknown; the recorded expiry timestamp is invalid".into(),
                None => "Unknown; expiry has not been observed on this login".into(),
            }));
            if binding.served_at.is_some() {
                facts.push(("Last recorded served turn".into(), observation_time(binding.served_at, now)));
            }
            if let Some(task) = binding.sign_in_task {
                facts.push(("Recorded sign-in task".into(), match task.action {
                    SignInAction::Browser => "Browser sign-in requested on record",
                    SignInAction::Login => "CLI sign-in requested on record",
                }.into()));
                facts.push(("Sign-in task observed".into(), observation_time(Some(task.at), now)));
                facts.push(("Sign-in task applicability".into(), if current_task.is_some() {
                    "Applies to the configured workflow; completion unverified"
                } else {
                    "Historical only; not a step for the current workflow"
                }.into()));
            }
            if let Some(attempt) = &binding.sign_in_attempt {
                facts.push(("Last automatic sign-in".into(), match attempt.outcome {
                    AttemptOutcome::NoRoute => "No usable sign-in route was found",
                    AttemptOutcome::Failed => "The sign-in attempt failed",
                    AttemptOutcome::Blocked => "The sign-in page stopped the attempt",
                    AttemptOutcome::SignedIn => "Succeeded on record; current sign-in is unverified",
                }.into()));
                facts.push(("Attempt observed".into(), observation_time(Some(attempt.since), now)));
                facts.push(("Attempt applicability".into(), if current_attempt.is_some() {
                    "Applies to the configured sign-in workflow"
                } else {
                    "Historical only; not evidence for the current workflow"
                }.into()));
            } else {
                facts.push(("Last automatic sign-in".into(), "No attempt recorded".into()));
            }
        }
        if let Some(step) = next_step {
            facts.push(("Next step".into(), step));
        }
        // These are historical observations under the admitted label. No account key is
        // supplied as proof of today's provider identity, and no live usage read is started.
        let windows = account_stats::held_windows(&account, None, None, Tap::On, now);
        for window in windows.windows {
            let name = match window.id {
                accounts::SESSION_LIMIT => "5-hour",
                accounts::WEEK_LIMIT => "Weekly",
                _ => continue,
            };
            let quota = match window.current(now) {
                Some(fig) if !valid_time(fig.at) || !valid_time(fig.reset) => "Unknown; the recorded quota timestamps are invalid".into(),
                Some(fig) if fig.at > now => "Unknown now; the quota observation is future-dated".into(),
                Some(fig) if fig.source == account_stats::SOURCE_WALL => "Limit reached on record".into(),
                Some(fig) => format!("{} (retained)", account_stats::left(fig.pct_x10, !fig.pct_x10.is_multiple_of(10))),
                None => match window.missing {
                    Some(Missing::WindowReset) => "Unknown now; the recorded window reset".into(),
                    Some(Missing::ResetUsed) => "Unknown now; a reset followed this reading".into(),
                    // Claude Code hands a window only while the API reports it, and nothing
                    // retained holds one: say so, with when, rather than "no reading".
                    Some(Missing::NotReported) => format!(
                        "Not reported; Claude Code's usage reading of {} carried the week but no 5-hour window, and no retained record holds one",
                        observation_time(account.sample.as_ref().map(|s| s.at), now)
                    ),
                    _ => "Unknown; no retained quota reading".into(),
                },
            };
            facts.push((format!("{name} quota"), quota));
            if let Some(fig) = window.fig {
                facts.push((format!("{name} quota observed"), observation_time(Some(fig.at), now)));
                facts.push((format!("{name} recorded reset"), recorded_time(Some(fig.reset))));
                let source = match fig.source.as_str() {
                    accounts::SOURCE_TAP => "Usage tap",
                    accounts::SOURCE_SAMPLE => "Usage panel",
                    account_stats::SOURCE_WALL => "Limit observation",
                    account_stats::SOURCE_WARNING => "Usage warning",
                    _ => "Retained quota observation",
                };
                facts.push((format!("{name} quota source"), source.into()));
            }
        }
        // What every Mac's journal says of this account, beside this Mac's own records.
        if let Some(found) = seen.and_then(|seen| {
            let pk = alab_seen::row_key(Cli::Claude, row)?;
            seen.accounts.get(&pk)
        }) {
            journal_facts(found, now, &mut facts);
        }
        LabAccount { label: row.label.clone(), facts, offer: None }
    }).collect();
    let status = if rows.is_empty() {
        "No configured Claude account rows; a default login may be unlisted".into()
    } else if records.notes.is_empty() {
        format!(
            "{} configured Claude account(s); provider sign-in is not checked here",
            rows.len()
        )
    } else {
        "Local account records read; some local metadata is unavailable".into()
    };
    LabAccounts {
        observed_at_ms: Some(now_ms),
        status,
        rows,
    }
}

#[cfg(test)]
#[path = "lab_accounts_tests.rs"]
mod tests;
