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
mod tests {
    use super::*;
    use aterm_agent::harness::account_progress::ProgressPhase;

    #[test]
    fn wrapped_claude_is_observed_and_a_stale_reader_is_not_an_agent() {
        use aterm_phase::Program;
        for program in ["node", "bun"] {
            let app = crate::App::headless_for_test();
            let ctx = app.pool.get(0).unwrap().ctx.clone();
            {
                let mut timeline = ctx.timeline.lock().unwrap();
                timeline.note_foreground_group(7);
                timeline.set_program(7, Some(program.into()));
                timeline.set_agent_reader_program_for_test(Some(Program::Claude));
            }
            let snapshot = app.harness_state();
            assert_eq!(
                snapshot.lab.sessions.len(),
                1,
                "{program} owns Claude's reader"
            );
            assert_eq!(snapshot.lab.sessions[0].local_id, 0);
            {
                let mut timeline = ctx.timeline.lock().unwrap();
                timeline.set_program(7, Some("vim".into()));
                timeline.set_agent_reader_program_for_test(Some(Program::Claude));
            }
            assert!(
                app.lab_state(&snapshot.sessions).sessions.is_empty(),
                "a cached name and stale reader do not make vim Claude"
            );
        }
    }

    #[test]
    fn a_transcript_only_limit_exposes_unknown_recovery_without_a_retry_promise() {
        use aterm_agent::harness::{
            footer::{FooterFacts, UNOBSERVED_RECOVERY_FINDING},
            session_usage::Wall,
        };
        let mut footer = FooterFacts::default();
        footer.usage.get_or_insert_with(Default::default).wall = Some(Wall {
            label: "5h",
            resets: "3pm".into(),
        });
        let mut progress = footer.account_progress_view().unwrap().into_owned();
        progress.details.push(format!(
            "{UNOBSERVED_RECOVERY_FINDING}: private-route-value"
        ));
        let facts = progress_facts(Some(&progress));
        for (name, value) in [
            ("Recovery", "State unknown"),
            ("Known accounts", "Unknown"),
            ("Eligible alternatives", "Unknown"),
            ("Decision observed", "Unknown"),
            ("Next event", "Not observed; no retry time promised"),
            ("Finding 1", UNOBSERVED_RECOVERY_FINDING),
        ] {
            assert!(facts.contains(&(name.into(), value.into())), "{facts:?}");
        }
        assert!(
            facts
                .iter()
                .any(|(name, value)| name == "Limit" && value.contains("provider reset: 3pm"))
        );
        assert!(!format!("{facts:?}").contains("private-route-value"));
        assert!(footer.account_progress.is_none());
    }

    #[test]
    fn an_unavailable_reader_start_preserves_history_but_an_observed_empty_roster_clears_it() {
        let mut app = crate::App::headless_for_test();
        let previous = LabAccounts {
            observed_at_ms: Some(100_000),
            status: "Previously observed".into(),
            rows: vec![crate::lab_accounts::LabAccount {
                label: "alternate".into(),
                facts: vec![("Sign-in".into(), "Unknown".into())],
                offer: None,
            }],
        };
        app.lab_screen.accounts = Some(previous.clone());
        assert!(app.proxy.is_none());
        for _ in 0..2 {
            app.start_lab_accounts_refresh();
            let kept = app.lab_screen.accounts.as_ref().unwrap();
            assert_eq!(kept.observed_at_ms, previous.observed_at_ms);
            assert_eq!(kept.rows, previous.rows);
            assert_eq!(kept.status.matches("Refresh unavailable").count(), 1);
            assert!(
                kept.status
                    .ends_with("Account observer is unavailable in this instance")
            );
            assert!(app.lab_screen.pending.is_none());
            assert!(app.lab_screen.next_read.is_some());
        }
        let empty = LabAccounts {
            observed_at_ms: Some(200_000),
            status: "No configured accounts".into(),
            rows: Vec::new(),
        };
        app.lab_screen.record_accounts(empty.clone());
        assert_eq!(
            app.lab_screen.accounts,
            Some(empty),
            "an observed deletion is not a failed refresh"
        );
    }

    #[test]
    fn periodic_publication_does_not_wait_on_a_session_writer_or_rebuild_the_harness() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let ctx = app.pool.get(0).unwrap().ctx.clone();
        {
            let mut timeline = ctx.timeline.lock().unwrap();
            timeline.note_foreground_group(7);
            timeline.set_program(7, Some("claude".into()));
        }
        assert!(app.open_settings_tab_in_window(wid, crate::native_settings::SettingsRoute::Lab));
        assert_eq!(
            app.front_control_surface(),
            crate::control::FrontControlSurface::HarnessSettings
        );
        let (instance, _) = app.active_native_view(wid).unwrap();
        let snapshot = |app: &crate::App| match app.native_runtime.app(instance).unwrap() {
            crate::native_app::NativeApp::Settings(settings) => settings.harness_snapshot().clone(),
            _ => panic!("Settings"),
        };
        let before = snapshot(&app);
        assert_eq!(before.sessions.len(), 1);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let writer = crate::qos::spawn_lane_thread(
            "lab-test-writer",
            crate::qos::Role::Background,
            move || {
                let _held = ctx.timeline.lock().unwrap();
                ready_tx.send(()).unwrap();
                // One-minute hang detector: an accidental full builder would wait
                // for this writer and fail the test once the detector releases it.
                release_rx.recv_timeout(Duration::from_secs(60)).is_ok()
            },
        )
        .unwrap();
        ready_rx.recv_timeout(Duration::from_secs(60)).unwrap();
        app.lab_screen.accounts = Some(unobserved_accounts("New Lab observation"));
        app.publish_native_lab_state();
        let _ = release_tx.send(());
        assert!(
            writer.join().unwrap(),
            "publication waited for the unrelated writer"
        );
        let mut after = snapshot(&app);
        assert_eq!(after.lab.accounts.status, "New Lab observation");
        after.lab = before.lab.clone();
        assert_eq!(after, before, "only Lab observations moved");
    }

    #[test]
    fn unknown_counts_are_not_zero_and_ready_is_not_a_running_switch() {
        let mut progress = AccountProgress::unknown(100);
        progress.phase = ProgressPhase::Ready;
        let facts = progress_facts(Some(&progress));
        assert!(facts.contains(&("Recovery".into(), "Account eligible".into())));
        assert!(facts.contains(&("Eligible alternatives".into(), "Unknown".into())));
        assert!(facts.contains(&("Decision observed".into(), observed_seconds(100))));
        assert!(
            !facts
                .iter()
                .any(|(_, value)| value.contains("Confirmed running"))
        );
        progress.phase = ProgressPhase::Waiting;
        progress.eligible = Some(0);
        progress.wall = Some("weekly allowance".into());
        progress.details.push("alternate: signed out".into());
        progress
            .details
            .push("No eligible alternate account".into());
        progress.next_at = Some(200);
        let facts = progress_facts(Some(&progress));
        assert!(facts.contains(&("Eligible alternatives".into(), "0".into())));
        assert!(facts.contains(&("Limit".into(), "weekly allowance".into())));
        assert!(facts.contains(&("Finding 1".into(), "No eligible alternate account".into())));
        assert!(facts.contains(&("Next event".into(), observed_seconds(200))));
    }

    #[test]
    fn own_claim_blockers_reach_lab_as_typed_retained_evidence() {
        use aterm_agent::harness::{
            account_progress::{OwnClaimBlocker, enrich_inventory},
            accounts::{Logins, Me, Point, Roster, Row, State},
        };
        const NOW: i64 = 1_790_000_000;
        let roster = Roster {
            rows: ["own", "selected"]
                .map(|label| Row {
                    label: label.into(),
                    dir: Some(format!("/synthetic/{label}")),
                    ..Default::default()
                })
                .into(),
        };
        let enrolled = ["own".to_owned(), "selected".to_owned()].into();
        let me = Me {
            account: Some("own"),
            left: None,
            default_home: true,
            route: aterm_agent::harness::accounts::AuthRoute::Stored,
        };
        let unrecorded = State {
            machine: "0a0b".into(),
            active: Some("selected".into()),
            proven: true,
            pinned: true,
            unrecorded: true,
            home_accounts: ["selected".to_owned()].into(),
            ..Default::default()
        };
        let observe = |state: &State| {
            let mut progress = AccountProgress::unknown(NOW);
            progress.account = Some("own".into());
            progress.phase = ProgressPhase::Waiting;
            enrich_inventory(
                &mut progress,
                &roster,
                &roster,
                state,
                &Logins::default(),
                &enrolled,
                &me,
                Some(&Point::TurnEnd),
            );
            progress
        };
        let cannot_record = observe(&unrecorded);
        assert_eq!(
            cannot_record.own_claim.unwrap().blocker,
            OwnClaimBlocker::CannotRecord
        );
        assert!(
            cannot_record.own_claim.unwrap().pinned,
            "the pin is on the selected account, not the process's account"
        );
        let facts = progress_facts_at(Some(&cannot_record), Some(NOW));
        let value = |facts: &[(String, String)], name: &str| {
            facts
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        };
        assert!(
            value(&facts, "Own account claim")
                .unwrap()
                .contains("could not record")
        );
        assert!(value(&facts, "Switch blocker").is_some());
        assert!(value(&facts, "Claim recorded until").is_none());

        let mut leased = unrecorded.clone();
        leased.unrecorded = false;
        leased.pinned = false;
        leased.note_lease("own", "private-holder-token", NOW + 60, NOW - 1);
        let mut held = observe(&leased);
        let original = held.clone();
        let facts = progress_facts_at(Some(&held), Some(NOW + 1));
        assert_eq!(
            value(&facts, "Decision observed"),
            Some(observed_seconds(NOW))
        );
        assert_eq!(
            value(&facts, "Claim recorded until"),
            Some(observed_seconds(NOW + 60))
        );
        assert!(
            value(&facts, "Claim timing")
                .unwrap()
                .contains("not rechecked")
        );
        assert!(value(&facts, "Switch blocker").is_none());
        let expired = progress_facts_at(Some(&held), Some(NOW + 60));
        assert!(value(&expired, "Claim timing").unwrap().contains("expired"));
        assert_eq!(
            value(&expired, "Claim recorded until"),
            value(&facts, "Claim recorded until")
        );
        assert_eq!(value(&expired, "Next event"), value(&facts, "Next event"));
        assert_eq!(
            held, original,
            "reading expiry does not renew or erase the observation"
        );
        for (observed_at, now) in [(0, Some(NOW)), (NOW + 2, Some(NOW)), (NOW, None)] {
            held.observed_at = observed_at;
            let facts = progress_facts_at(Some(&held), now);
            assert!(
                value(&facts, "Claim timing")
                    .unwrap()
                    .contains("unconfirmed")
            );
            assert!(
                value(&facts, "Own account claim")
                    .unwrap()
                    .contains("unconfirmed")
            );
            let mut unconfirmed_pin = cannot_record.clone();
            unconfirmed_pin.observed_at = observed_at;
            let facts = progress_facts_at(Some(&unconfirmed_pin), now);
            assert!(
                value(&facts, "Own account claim")
                    .unwrap()
                    .contains("unconfirmed")
            );
            assert!(
                value(&facts, "Switch blocker")
                    .unwrap()
                    .contains("unconfirmed")
            );
        }
        held = original;
        held.own_claim.as_mut().unwrap().blocker = OwnClaimBlocker::HeldElsewhere { until: 0 };
        let invalid = progress_facts_at(Some(&held), Some(NOW));
        assert_eq!(
            value(&invalid, "Claim recorded until"),
            Some("Unknown".into())
        );
        assert!(
            value(&invalid, "Claim timing")
                .unwrap()
                .contains("unconfirmed")
        );

        leased.clear_lease("own");
        leased.note_lease("selected", "private-holder-token", NOW + 60, NOW - 1);
        let mut other_account = observe(&leased);
        assert!(other_account.own_claim.is_none());
        other_account.details.push("own: another Mac holds it until private-claim-value; the pin on private-pin-value keeps the session from moving".into());
        let facts = progress_facts_at(Some(&other_account), Some(NOW));
        assert!(
            value(&facts, "Own account claim")
                .unwrap()
                .contains("unknown")
        );
        assert!(value(&facts, "Claim recorded until").is_none());
        assert!(value(&facts, "Switch blocker").is_none());
        for facts in [facts, progress_facts_at(Some(&cannot_record), Some(NOW))] {
            let words = format!("{facts:?}");
            for private in [
                "private-holder-token",
                "private-claim-value",
                "private-pin-value",
                "/synthetic/",
            ] {
                assert!(!words.contains(private));
            }
        }
    }

    #[test]
    fn unavailable_recovery_keeps_only_fixed_historical_findings_on_the_native_page() {
        use aterm_agent::harness::account_progress::{
            EXECUTION_PREPARED_FINDING, OBSERVER_BUSY_FINDING, RECOVERY_HISTORY_PREFIX,
        };
        let mut progress = AccountProgress::unknown(100);
        progress.phase = ProgressPhase::Waiting;
        progress.details = vec![
            EXECUTION_PREPARED_FINDING.into(),
            "private-route-value".into(),
            format!("{RECOVERY_HISTORY_PREFIX}{EXECUTION_PREPARED_FINDING}: private-suffix"),
            format!(
                "{RECOVERY_HISTORY_PREFIX}{RECOVERY_HISTORY_PREFIX}{EXECUTION_PREPARED_FINDING}"
            ),
        ];
        let facts = aterm_agent::harness::footer::FooterFacts {
            account_progress: Some(progress),
            account_progress_busy: true,
            ..Default::default()
        };
        let view = facts.account_progress_view().unwrap();
        let projected = progress_facts_at(Some(&view), Some(100));
        assert!(
            projected
                .iter()
                .any(|(_, value)| value == OBSERVER_BUSY_FINDING)
        );
        assert!(
            projected.iter().any(|(_, value)| value
                == &format!("{RECOVERY_HISTORY_PREFIX}{EXECUTION_PREPARED_FINDING}"))
        );
        let words = format!("{projected:?}");
        assert!(!words.contains("private-route-value"));
        assert!(!words.contains("private-suffix"));
        assert!(!words.contains(&format!(
            "{RECOVERY_HISTORY_PREFIX}{RECOVERY_HISTORY_PREFIX}"
        )));
    }

    #[test]
    fn retained_route_text_never_enters_the_native_page() {
        use aterm_agent::harness::account_progress::{
            EXECUTION_HISTORY_PREFIX, EXECUTION_PREPARED_FINDING, EXECUTION_REFRESH_FINDING,
        };
        let mut progress = AccountProgress::unknown(100);
        progress.phase = ProgressPhase::SigningIn {
            label: "alternate".into(),
            route: Some("private-route-value".into()),
        };
        progress
            .details
            .push("alternate: sign-in route failed: private-secret-value".into());
        for finding in [
            aterm_agent::harness::account_progress::IDENTITY_MISMATCH_FINDING,
            aterm_agent::harness::account_progress::BINDINGS_CHANGED_FINDING,
            aterm_agent::harness::account_progress::EXECUTION_PREPARED_FINDING,
            aterm_agent::harness::account_progress::EXECUTION_ROSTER_UNREAD_FINDING,
            aterm_agent::harness::account_progress::EXECUTION_CONTEXT_UNREAD_FINDING,
            aterm_agent::harness::account_progress::EXECUTION_RECORDS_UNREAD_FINDING,
        ] {
            progress.details.push(finding.into());
            progress
                .details
                .push(format!("{finding}: private-identity-value"));
        }
        let historical = format!("{EXECUTION_HISTORY_PREFIX}{EXECUTION_PREPARED_FINDING}");
        progress.details.push(historical.clone());
        progress.details.push(EXECUTION_REFRESH_FINDING.into());
        progress
            .details
            .push(format!("{EXECUTION_HISTORY_PREFIX}private-secret-value"));
        progress
            .details
            .push(format!("{historical}: private-identity-value"));
        let facts = progress_facts(Some(&progress));
        let words = format!("{facts:?}");
        assert!(words.contains("Signing in alternate"));
        assert!(!words.contains("private-route-value"));
        assert!(!words.contains("private-secret-value"));
        assert!(!words.contains("private-identity-value"));
        assert!(facts.iter().any(|(_, value)| value == &historical));
        assert!(
            facts
                .iter()
                .any(|(_, value)| value == EXECUTION_REFRESH_FINDING)
        );
        for finding in [
            aterm_agent::harness::account_progress::IDENTITY_MISMATCH_FINDING,
            aterm_agent::harness::account_progress::BINDINGS_CHANGED_FINDING,
            aterm_agent::harness::account_progress::EXECUTION_PREPARED_FINDING,
            aterm_agent::harness::account_progress::EXECUTION_ROSTER_UNREAD_FINDING,
            aterm_agent::harness::account_progress::EXECUTION_CONTEXT_UNREAD_FINDING,
            aterm_agent::harness::account_progress::EXECUTION_RECORDS_UNREAD_FINDING,
        ] {
            assert!(facts.iter().any(|(_, value)| value == finding));
        }
    }

    #[test]
    fn hidden_or_occluded_pages_own_no_account_or_network_timer() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.config
            .sys
            .get_or_insert_with(Default::default)
            .stats_bars = Some("off".into());
        let source = crate::network_deck::inactive_snapshot();
        app.network_source = Some(Arc::new(move || Some(Arc::clone(&source))));
        assert_eq!(app.lab_state_deadline(), None);
        assert_eq!(app.network_state_deadline(), None);
        assert!(app.open_settings_tab_in_window(wid, crate::native_settings::SettingsRoute::Lab));
        let due = Instant::now() + REREAD;
        app.lab_screen.next_read = Some(due);
        app.network_next_poll = Some(due);
        assert_eq!(app.lab_state_deadline(), Some(due));
        assert_eq!(app.network_state_deadline(), Some(due));
        app.windows.get_mut(&wid).unwrap().occluded = true;
        assert_eq!(app.lab_state_deadline(), None);
        assert_eq!(app.network_state_deadline(), None);
        app.windows.get_mut(&wid).unwrap().occluded = false;
        assert!(app.open_settings_tab_in_window(wid, crate::native_settings::SettingsRoute::Home));
        assert_eq!(app.lab_state_deadline(), None);
        assert_eq!(app.network_state_deadline(), None);
    }

    #[test]
    fn a_completion_must_belong_to_the_one_pending_reader() {
        let mut app = crate::App::headless_for_test();
        app.lab_screen.pending = Some(3);
        let records = unobserved_accounts("publication unavailable");
        app.finish_lab_accounts(2, records.clone());
        assert!(app.lab_screen.accounts.is_none());
        assert_eq!(app.lab_screen.pending, Some(3));
        app.finish_lab_accounts(3, records.clone());
        assert_eq!(app.lab_screen.accounts, Some(records));
        assert_eq!(app.lab_screen.pending, None);
        app.finish_lab_accounts(3, unobserved_accounts("obsolete duplicate"));
        assert_eq!(
            app.lab_screen.accounts.as_ref().unwrap().status,
            "publication unavailable"
        );
    }

    #[test]
    fn a_failed_refresh_keeps_explicitly_retained_evidence() {
        let mut app = crate::App::headless_for_test();
        let mut previous = unobserved_accounts("One recorded account");
        previous.observed_at_ms = Some(100_000);
        previous.rows.push(crate::lab_accounts::LabAccount {
            label: "alternate".into(),
            facts: vec![("Authentication".into(), "Signed out".into())],
            offer: None,
        });
        app.lab_screen.accounts = Some(previous.clone());
        app.lab_screen.pending = Some(1);
        app.finish_lab_accounts(1, unobserved_accounts("Account publication busy"));
        let kept = app.lab_screen.accounts.as_ref().unwrap();
        assert_eq!(kept.observed_at_ms, previous.observed_at_ms);
        assert_eq!(kept.rows, previous.rows);
        assert!(
            kept.status
                .starts_with("Refresh unavailable; showing retained")
        );
    }

    #[test]
    fn an_overdue_reader_is_reported_without_launching_another() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_settings_tab_in_window(wid, crate::native_settings::SettingsRoute::Lab));
        let began = Instant::now();
        app.lab_screen.pending = Some(7);
        app.lab_screen.pending_started = Some(began);
        assert_eq!(app.lab_state_deadline(), Some(began + READER_WATCH));
        app.service_lab_state(began + READER_WATCH);
        assert!(app.lab_screen.overdue);
        assert!(
            app.lab_state(&[])
                .accounts
                .status
                .contains("reader has not completed")
        );
        assert_eq!(app.lab_state_deadline(), None);
        app.start_lab_accounts_refresh();
        assert_eq!(app.lab_screen.pending, Some(7));
        app.finish_lab_accounts(7, unobserved_accounts("Completed without records"));
        assert!(!app.lab_screen.overdue);
        assert_eq!(app.lab_screen.pending, None);
        assert!(app.lab_state_deadline().is_some());
    }
}
