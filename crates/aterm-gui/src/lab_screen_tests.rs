// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
    let writer =
        crate::qos::spawn_lane_thread("lab-test-writer", crate::qos::Role::Background, move || {
            let _held = ctx.timeline.lock().unwrap();
            ready_tx.send(()).unwrap();
            // One-minute hang detector: an accidental full builder would wait
            // for this writer and fail the test once the detector releases it.
            release_rx.recv_timeout(Duration::from_secs(60)).is_ok()
        })
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
        format!("{RECOVERY_HISTORY_PREFIX}{RECOVERY_HISTORY_PREFIX}{EXECUTION_PREPARED_FINDING}"),
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
    assert!(projected.iter().any(
        |(_, value)| value == &format!("{RECOVERY_HISTORY_PREFIX}{EXECUTION_PREPARED_FINDING}")
    ));
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
    app.network_source = Some(Arc::new(move |_| Some(Arc::clone(&source))));
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
