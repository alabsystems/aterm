// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Immutable Lab observations and the bounded account reader behind its native page.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_agent::harness::account_progress::AccountProgress;
use aterm_types::network_state::NetworkStateSnapshot;

use crate::lab_accounts::LabAccounts;

const REREAD: Duration = Duration::from_secs(10);
const READER_WATCH: Duration = Duration::from_secs(60);

#[cfg(test)]
#[path = "lab_reader_activation_tests.rs"]
mod activation_tests;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LabSession {
    pub(crate) local_id: u64,
    pub(crate) title: String,
    pub(crate) facts: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LabState {
    pub(crate) network: Arc<NetworkStateSnapshot>,
    pub(crate) accounts: LabAccounts,
    pub(crate) sessions: Vec<LabSession>,
    pub(crate) collecting: bool,
}

impl Default for LabState {
    fn default() -> Self {
        Self {
            network: crate::network_deck::inactive_snapshot(),
            accounts: unobserved_accounts("Account records have not been observed"),
            sessions: Vec::new(),
            collecting: false,
        }
    }
}

fn unobserved_accounts(status: &str) -> LabAccounts {
    LabAccounts {
        observed_at_ms: None,
        status: status.into(),
        rows: Vec::new(),
    }
}

#[derive(Default)]
pub(crate) struct LabScreen {
    accounts: Option<LabAccounts>,
    sequence: u64,
    pending: Option<u64>,
    pending_started: Option<Instant>,
    overdue: bool,
    next_read: Option<Instant>,
}

impl LabScreen {
    /// Account row `index`'s vault token offer, as the page last showed it.
    pub(crate) fn account_offer(&self, index: usize) -> Option<crate::token_shown::TokenOffer> {
        self.accounts
            .as_ref()
            .and_then(|lab| lab.rows.get(index)?.offer.clone())
    }

    fn record_accounts(&mut self, accounts: LabAccounts) {
        self.accounts = Some(match self.accounts.take() {
            Some(mut previous)
                if accounts.observed_at_ms.is_none() && previous.observed_at_ms.is_some() =>
            {
                previous.status = format!(
                    "Refresh unavailable; showing retained account observation. {}",
                    accounts.status
                );
                previous
            }
            _ => accounts,
        });
    }
}

fn progress_facts(progress: Option<&AccountProgress>) -> Vec<(String, String)> {
    progress_facts_at(
        progress,
        aterm_time::unix_secs().and_then(|now| i64::try_from(now).ok()),
    )
}

pub(crate) fn progress_facts_at(
    progress: Option<&AccountProgress>,
    now: Option<i64>,
) -> Vec<(String, String)> {
    let Some(progress) = progress else {
        return vec![(
            "Recovery".into(),
            "Unknown; no account decision has been observed for this session".into(),
        )];
    };
    let count = |n: Option<usize>| n.map_or_else(|| "Unknown".into(), |n| n.to_string());
    let mut facts = vec![
        (
            "Recovery".into(),
            match &progress.phase {
                aterm_agent::harness::account_progress::ProgressPhase::SigningIn {
                    label, ..
                } => format!("Signing in {label}"),
                _ => aterm_agent::harness::footer::account_progress_phase(progress),
            },
        ),
        (
            "Account".into(),
            progress.account.clone().unwrap_or_else(|| "Unknown".into()),
        ),
        ("Known accounts".into(), count(progress.known)),
        ("Eligible alternatives".into(), count(progress.eligible)),
        (
            "Limit".into(),
            progress
                .wall
                .clone()
                .unwrap_or_else(|| "Not observed".into()),
        ),
        (
            "Decision observed".into(),
            observed_seconds(progress.observed_at),
        ),
        (
            "Next event".into(),
            progress.next_at.map_or_else(
                || "Not observed; no retry time promised".into(),
                observed_seconds,
            ),
        ),
    ];
    for work in &progress.independent_work {
        facts.push((
            "Local pool work".into(),
            aterm_agent::harness::footer::account_pool_work_text(work),
        ));
        facts.push((
            "Pool work observed".into(),
            observed_seconds(work.observed_at),
        ));
    }
    use aterm_agent::harness::account_progress::OwnClaimBlocker;
    let observation_confirmed = now.is_some_and(|now| {
        (1..=253_402_300_799).contains(&now)
            && (1..=253_402_300_799).contains(&progress.observed_at)
            && progress.observed_at <= now
    });
    match progress.own_claim {
        None => facts.push((
            "Own account claim".into(),
            "No retained blocker observation; current claim unknown".into(),
        )),
        Some(finding) => {
            facts.push((
                "Own account claim".into(),
                if observation_confirmed {
                    match finding.blocker {
                        OwnClaimBlocker::HeldElsewhere { .. } => {
                            "Another Mac held this account at the decision"
                        }
                        OwnClaimBlocker::CannotRecord => {
                            "This Mac could not record a claim at the decision"
                        }
                    }
                } else {
                    "Claim blocker observation time is unconfirmed; current claim unknown"
                }
                .into(),
            ));
            if let OwnClaimBlocker::HeldElsewhere { until } = finding.blocker {
                facts.push(("Claim recorded until".into(), observed_seconds(until)));
                let timing = match now {
                    Some(now)
                        if observation_confirmed
                            && (1..=253_402_300_799).contains(&until)
                            && progress.observed_at < until =>
                    {
                        if until <= now {
                            "Recorded lease expired; current claim unknown"
                        } else {
                            "Recorded lease only; current ownership is not rechecked here"
                        }
                    }
                    _ => "Current claim unknown; recorded lease timing is unconfirmed",
                };
                facts.push(("Claim timing".into(), timing.into()));
            }
            if finding.pinned {
                facts.push((
                    "Switch blocker".into(),
                    if observation_confirmed {
                        "A pinned account prevented switching at the decision"
                    } else {
                        "Pinned selection reported with unconfirmed observation time; current block unknown"
                    }
                    .into(),
                ));
            }
        }
    }
    // Retained details include arbitrary persisted route text. The account
    // projection below supplies typed sign-in outcomes without exposing it.
    facts.extend(
        progress
            .details
            .iter()
            .filter(|detail| safe_finding(detail))
            .enumerate()
            .map(|(n, detail)| (format!("Finding {}", n + 1), detail.clone())),
    );
    for diagnostic in &progress.login_diagnostics {
        if now.is_some_and(|now| {
            (1..=253_402_300_799).contains(&now)
                && (1..=253_402_300_799).contains(&diagnostic.completed_at)
                && diagnostic.completed_at <= now
        }) {
            facts.push((
                "Last local sign-in".into(),
                format!("{}: {}", diagnostic.label(), diagnostic.words()),
            ));
            facts.push((
                "Sign-in finished".into(),
                observed_seconds(diagnostic.completed_at),
            ));
        }
    }
    facts.push(("Scope".into(), "This session's supervisor observations; activity on other processes or machines may be unobserved".into()));
    facts
}

/// Only fixed producer words may cross from the free-text recovery record.
fn safe_finding(detail: &str) -> bool {
    use aterm_agent::harness::account_progress as progress;
    // A historical wrapper never admits its free-text payload: the exact existing fixed
    // finding below must still match after one wrapper is removed.
    let detail = detail
        .strip_prefix(progress::RECOVERY_HISTORY_PREFIX)
        .unwrap_or(detail);
    if let Some(previous) = detail.strip_prefix(progress::EXECUTION_HISTORY_PREFIX) {
        return matches!(
            previous,
            progress::EXECUTION_PREPARED_FINDING
                | progress::EXECUTION_ROSTER_UNREAD_FINDING
                | progress::EXECUTION_CONTEXT_UNREAD_FINDING
                | progress::EXECUTION_RECORDS_UNREAD_FINDING
        );
    }
    matches!(
        detail,
        "Automatic account rotation is off"
            | aterm_agent::harness::footer::UNOBSERVED_RECOVERY_FINDING
            | aterm_agent::harness::account_progress::IDENTITY_MISMATCH_FINDING
            | aterm_agent::harness::account_progress::BINDINGS_CHANGED_FINDING
            | aterm_agent::harness::account_progress::EXECUTION_PREPARED_FINDING
            | aterm_agent::harness::account_progress::EXECUTION_ROSTER_UNREAD_FINDING
            | aterm_agent::harness::account_progress::EXECUTION_CONTEXT_UNREAD_FINDING
            | aterm_agent::harness::account_progress::EXECUTION_RECORDS_UNREAD_FINDING
            | aterm_agent::harness::account_progress::EXECUTION_REFRESH_FINDING
            | aterm_agent::harness::account_progress::OBSERVER_BUSY_FINDING
            | progress::SESSION_AUTOPILOT_OFF_FINDING
            | progress::RECOVERY_RESUME_FINDING
            | "No account roster is available"
            | "This process is not bound to a listed account"
            | "This process uses a separate account home"
            | "This process does not use a stored login"
            | "Waiting for the selected account to answer"
            | "No eligible alternate account"
            | "Waiting for the account-switch limit to reset"
            | "The selected account is pinned"
            | "Keeping an expiring reset offer"
            | "No account switch is due"
            | "An account usage check is due"
            | "Waiting until restart is safe"
            | "Restart held or signal refused"
            | "Account restart was refused"
            | "Automatic switching has not been verified for this CLI build"
            | "Another supervisor owns account switching"
            | "The CLI uses its own OS credential store"
            | "The alternate account could not be prepared for restart"
            | "The limit was cleared; the session may continue"
            | "Usage check requested"
            | "Account state is unavailable"
            | "No accounts are listed"
            | "The account roster could not be read"
            | "Stored logins could not be read"
            | "This process's account identity is unavailable"
            | "Account records could not be read consistently"
            | "Account restart was refused; awaiting selection rollback"
            | "Waiting for a decision under the current account policy"
            | "Awaiting a recovery decision"
    )
}

fn observed_seconds(seconds: i64) -> String {
    match u64::try_from(seconds) {
        Ok(value @ 1..=253_402_300_799) => aterm_types::rfc3339::format_rfc3339(value),
        _ => "Unknown".into(),
    }
}

impl crate::App {
    #[cfg(unix)]
    pub(crate) fn lab_page_present(&self) -> bool {
        self.view_store
            .iter()
            .any(|(view, _)| self.lab_page_in(view))
    }

    pub(crate) fn lab_page_in(&self, view: crate::tab_model::ViewId) -> bool {
        matches!(self.native_runtime.view_state(view),
            Some(crate::native_app::AppViewState::Settings(state))
                if state.route == crate::native_settings::SettingsRoute::Lab)
    }

    /// Only a Lab page on screen ([`Self::view_on_screen`]) owns the periodic read; hidden
    /// Settings, a tab behind another and a view a zoomed tab hides never keep it awake.
    pub(crate) fn lab_page_shown(&self) -> bool {
        self.view_on_screen(|view| self.lab_page_in(view))
    }

    pub(crate) fn lab_state(&self, sessions: &[crate::harness_screen::HarnessSession]) -> LabState {
        let live: std::collections::BTreeSet<_> = self
            .store
            .try_read()
            .ok()
            .map(|store| {
                store
                    .snapshot()
                    .iter()
                    .filter(|handle| handle.state != crate::session_store::SessionState::Exited)
                    .map(|handle| handle.local_id)
                    .collect()
            })
            .unwrap_or_default();
        let mut accounts = self
            .lab_screen
            .accounts
            .clone()
            .unwrap_or_else(|| unobserved_accounts("Account records have not been observed"));
        if accounts.observed_at_ms.is_some_and(|observed| {
            aterm_time::unix_millis().is_none_or(|now| now < observed || now - observed > 60_000)
        }) {
            accounts.status = format!(
                "Retained observation; current account records are unknown. {}",
                accounts.status
            );
        }
        if self.lab_screen.overdue {
            accounts.status = format!(
                "The account reader has not completed. No second reader will start until it finishes; any rows below are retained observations. {}",
                accounts.status
            );
        }
        LabState {
            network: crate::network_deck::fresh_snapshot(
                Arc::clone(&self.network_snapshot),
                aterm_time::unix_millis(),
            ),
            accounts,
            sessions: sessions
                .iter()
                .filter(|s| live.contains(&s.local_id) && self.pool.get(s.local_id).is_some())
                .filter_map(|session| {
                    use aterm_phase::Program;
                    let entry = self.pool.get(session.local_id)?;
                    let timeline = entry.ctx.timeline.try_lock().ok()?;
                    let claude = match crate::harness_host::identified_agent(timeline.agent()) {
                        Some(Program::Claude) => true,
                        Some(
                            Program::Codex
                            | Program::Agy
                            | Program::Muse
                            | Program::Grok
                            | Program::Kimi
                            | Program::Generic,
                        )
                        | None => false,
                    };
                    if !claude {
                        return None;
                    }
                    let progress = timeline
                        .claude_footer()
                        .and_then(|facts| facts.account_progress_view());
                    Some(LabSession {
                        local_id: session.local_id,
                        title: format!("{} · {}", session.title, session.place),
                        facts: progress_facts(progress.as_deref()),
                    })
                })
                .collect(),
            collecting: self.lab_screen.pending.is_some(),
        }
    }

    pub(crate) fn lab_state_deadline(&self) -> Option<Instant> {
        self.lab_state_deadline_at(Instant::now())
    }

    // The first read is due on this tick; a second clock sample would keep moving it ahead
    // of service_lab_state's caller. A candidate must not start a reader whose wake it drops.
    fn lab_state_deadline_at(&self, now: Instant) -> Option<Instant> {
        if self.incoming_handoff_pending || !self.lab_page_shown() {
            return None;
        }
        if self.lab_screen.pending.is_some() {
            return (!self.lab_screen.overdue)
                .then(|| self.lab_screen.pending_started.unwrap_or(now) + READER_WATCH);
        }
        Some(self.lab_screen.next_read.unwrap_or(now))
    }

    /// Periodic observations use published names and nonblocking timeline reads,
    /// leaving unrelated harness/ledger projections and locks alone.
    pub(crate) fn publish_native_lab_state(&mut self) {
        let Some(names) = self.native_runtime.settings_lab_session_names() else {
            return;
        };
        let state = self.lab_state(&names);
        self.harness_screen.revision = self.harness_screen.revision.saturating_add(1);
        let revision = self.harness_screen.revision;
        if self.native_runtime.replace_settings_lab(state, revision) {
            self.dispatch_to_settings_views(&crate::native_app::AppEvent::HarnessChanged {
                revision,
            });
        }
    }

    pub(crate) fn service_lab_state(&mut self, now: Instant) {
        if self
            .lab_state_deadline_at(now)
            .is_some_and(|due| due <= now)
        {
            if self.lab_screen.pending.is_some() {
                self.lab_screen.overdue = true;
                self.publish_native_lab_state();
            } else {
                self.start_lab_accounts_refresh();
            }
        }
    }

    pub(crate) fn start_lab_accounts_refresh(&mut self) {
        if self.incoming_handoff_pending || self.lab_screen.pending.is_some() {
            return;
        }
        let now = Instant::now();
        self.lab_screen.next_read = Some(now + REREAD);
        let Some(proxy) = self.proxy.clone() else {
            self.lab_screen.record_accounts(unobserved_accounts(
                "Account observer is unavailable in this instance",
            ));
            self.publish_native_lab_state();
            return;
        };
        let config = crate::app_config::config_path();
        self.lab_screen.sequence = self.lab_screen.sequence.wrapping_add(1);
        let sequence = self.lab_screen.sequence;
        let spawn = crate::qos::spawn_lane_thread(
            "aterm-lab-accounts",
            crate::qos::Role::Background,
            move || {
                let result = std::panic::catch_unwind(|| {
                    match (config.as_deref(), aterm_time::unix_millis()) {
                        (Some(config), Some(now)) => crate::lab_accounts::collect(config, now),
                        _ => unobserved_accounts(
                            "Account location or observation clock is unavailable",
                        ),
                    }
                })
                .unwrap_or_else(|_| unobserved_accounts("Account reader did not complete"));
                let _ = proxy.send_event(crate::Wake::LabAccountsCollected {
                    sequence,
                    result: Box::new(result),
                });
            },
        );
        match spawn {
            Ok(_) => {
                self.lab_screen.pending = Some(sequence);
                self.lab_screen.pending_started = Some(now);
                self.lab_screen.overdue = false;
            }
            Err(_) => {
                self.lab_screen
                    .record_accounts(unobserved_accounts("Account reader could not start"));
            }
        }
        self.publish_native_lab_state();
    }

    pub(crate) fn finish_lab_accounts(&mut self, sequence: u64, accounts: LabAccounts) {
        if self.lab_screen.pending != Some(sequence) {
            return;
        }
        self.lab_screen.pending = None;
        self.lab_screen.pending_started = None;
        self.lab_screen.overdue = false;
        self.lab_screen.next_read = Some(Instant::now() + REREAD);
        self.lab_screen.record_accounts(accounts);
        self.publish_native_lab_state();
    }
}

#[cfg(test)]
#[path = "lab_screen_tests.rs"]
mod tests;
