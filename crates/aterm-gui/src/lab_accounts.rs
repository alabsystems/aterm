// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded local account metadata for the Lab page; collection runs off the UI thread.

use std::path::Path;

use aterm_agent::harness::account_stats::{self, Missing, Tap};
use aterm_agent::harness::accounts::{
    self, AccountRecords, AttemptOutcome, Auth, Cli, SignInAction,
};

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
        Ok(records) => project(&records, now_ms),
        Err(_) => unavailable("Local account records could not be read consistently"),
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

fn project(records: &AccountRecords, now_ms: u64) -> LabAccounts {
    let now = seconds(now_ms);
    let rows: Vec<_> = records.roster.rows.iter().take(accounts::MAX_ACCOUNTS).map(|row| {
        let account = records.state.accounts.get(&row.label).cloned().unwrap_or_default();
        let binding = records.logins.get(&row.label);
        let next_step = binding.and_then(|binding| binding.sign_in_task)
            .map(|task| task.action.next_step(Cli::Claude, &row.label))
            .or_else(|| {
                (account.auth == Auth::SignedOut
                    || binding.and_then(|binding| binding.sign_in_attempt.as_ref())
                        .is_some_and(|attempt| attempt.outcome.failing()))
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
            facts.push(("Login renewed".into(), observation_time(binding.renewed_at, now)));
            if let Some(by) = accounts::forecast(binding) {
                facts.push(("Renewal forecast".into(), recorded_time(Some(by))));
            }
            if let Some(task) = binding.sign_in_task {
                facts.push(("Recorded sign-in task".into(), match task.action {
                    SignInAction::Browser => "Browser sign-in needed",
                    SignInAction::Login => "CLI sign-in needed",
                }.into()));
                facts.push(("Sign-in task observed".into(), observation_time(Some(task.at), now)));
            }
            if let Some(attempt) = &binding.sign_in_attempt {
                facts.push(("Last automatic sign-in".into(), match attempt.outcome {
                    AttemptOutcome::NoRoute => "No usable sign-in route was found",
                    AttemptOutcome::Failed => "The sign-in attempt failed",
                    AttemptOutcome::Blocked => "The sign-in page stopped the attempt",
                    AttemptOutcome::SignedIn => "Succeeded on record; current sign-in is unverified",
                }.into()));
                facts.push(("Attempt observed".into(), observation_time(Some(attempt.since), now)));
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
                Some(fig) => format!("{} used (retained)", account_stats::percent(fig.pct_x10, !fig.pct_x10.is_multiple_of(10))),
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
        LabAccount { label: row.label.clone(), facts }
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
