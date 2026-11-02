// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn records() -> AccountRecords {
    AccountRecords {
        roster: accounts::Roster {
            rows: vec![accounts::Row {
                label: "one".into(),
                ..Default::default()
            }],
        },
        ..Default::default()
    }
}

fn fact<'a>(view: &'a LabAccounts, key: &str) -> &'a str {
    &view.rows[0]
        .facts
        .iter()
        .find(|(name, _)| name == key)
        .expect("fact")
        .1
}

#[test]
fn an_unobserved_account_has_no_invented_budget_or_login() {
    let view = project(&records(), 200_000);
    assert!(fact(&view, "Last observed sign-in").starts_with("Unknown"));
    for key in ["5-hour quota", "Weekly quota"] {
        let value = fact(&view, key);
        assert!(value.starts_with("Unknown"), "{value}");
        assert!(!value.contains('%'));
    }
    assert!(
        !view.rows[0]
            .facts
            .iter()
            .any(|(key, _)| key.contains("eligible"))
    );
    assert!(
        view.rows[0]
            .facts
            .iter()
            .all(|(_, value)| !value.contains("Ready"))
    );
}

#[test]
fn recorded_human_steps_take_precedence_without_exposing_raw_routes() {
    for auth in [Auth::Unseen, Auth::Ok, Auth::SignedOut] {
        for outcome in [
            None,
            Some(AttemptOutcome::NoRoute),
            Some(AttemptOutcome::Failed),
            Some(AttemptOutcome::SignedIn),
        ] {
            for action in [SignInAction::Browser, SignInAction::Login] {
                let mut records = records();
                records.state.accounts.insert(
                    "one".into(),
                    accounts::Account {
                        auth,
                        auth_at: Some(100),
                        ..Default::default()
                    },
                );
                records.logins.labels.insert(
                    "one".into(),
                    accounts::Binding {
                        sign_in_task: Some(accounts::SignInTask { action, at: 110 }),
                        sign_in_attempt: outcome.map(|outcome| accounts::SignInAttempt {
                            outcome,
                            routes: "credential-marker https://private.invalid/code secret@example.invalid".into(),
                            since: 120,
                        }),
                        ..Default::default()
                    },
                );
                let before = records.clone();
                let view = project(&records, 200_000);
                assert_eq!(
                    fact(&view, "Next step"),
                    action.next_step(Cli::Claude, "one")
                );
                assert_eq!(
                    fact(&view, "Sign-in task observed"),
                    recorded_time(Some(110))
                );
                assert!(
                    view.rows[0]
                        .facts
                        .iter()
                        .any(|(key, _)| key == "Recorded sign-in task")
                );
                let text = format!("{view:?}");
                for forbidden in [
                    "credential-marker",
                    "private.invalid",
                    "secret@example.invalid",
                ] {
                    assert!(!text.contains(forbidden), "{text}");
                }
                assert_eq!(records, before);
            }
        }
    }
}

fn assert_recovery_inspection(view: &LabAccounts) {
    let step = fact(view, "Next step");
    assert!(step.contains("aterm harness accounts"), "{step}");
    for provider_action in ["accounts login", "accounts renew", "accounts browser"] {
        assert!(!step.contains(provider_action), "{step}");
    }
    assert!(
        view.rows[0]
            .facts
            .iter()
            .all(|(key, _)| key != "Recorded sign-in task")
    );
}

#[test]
fn auth_without_a_recorded_human_task_only_points_to_recovery_inspection() {
    for (auth, inspect) in [
        (Auth::Unseen, false),
        (Auth::Ok, false),
        (Auth::SignedOut, true),
    ] {
        for binding in [
            None,
            Some(accounts::Binding::default()),
            Some(accounts::Binding {
                sign_in_attempt: Some(accounts::SignInAttempt {
                    outcome: AttemptOutcome::SignedIn,
                    routes: "past successful route".into(),
                    since: 120,
                }),
                ..Default::default()
            }),
        ] {
            let mut records = records();
            records.state.accounts.insert(
                "one".into(),
                accounts::Account {
                    auth,
                    auth_at: Some(100),
                    ..Default::default()
                },
            );
            if let Some(binding) = binding {
                records.logins.labels.insert("one".into(), binding);
            }
            let before = records.clone();
            let view = project(&records, 200_000);
            if inspect {
                assert_recovery_inspection(&view);
            } else {
                assert!(view.rows[0].facts.iter().all(|(key, _)| key != "Next step"));
            }
            assert_eq!(records, before);
        }
    }
}

#[test]
fn automatic_failure_guidance_does_not_invent_a_human_step_or_expose_routes() {
    for auth in [Auth::Unseen, Auth::Ok, Auth::SignedOut] {
        for outcome in [AttemptOutcome::NoRoute, AttemptOutcome::Failed] {
            let mut records = records();
            records.state.accounts.insert(
                "one".into(),
                accounts::Account {
                    auth,
                    auth_at: Some(100),
                    ..Default::default()
                },
            );
            records.logins.labels.insert(
                "one".into(),
                accounts::Binding {
                    sign_in_attempt: Some(accounts::SignInAttempt {
                        outcome,
                        routes:
                            "credential-marker https://private.invalid/code secret@example.invalid"
                                .into(),
                        since: 120,
                    }),
                    ..Default::default()
                },
            );
            let before = records.clone();
            let view = project(&records, 200_000);
            assert_recovery_inspection(&view);
            assert_eq!(fact(&view, "Attempt observed"), recorded_time(Some(120)));
            let text = format!("{view:?}");
            for forbidden in [
                "credential-marker",
                "private.invalid",
                "secret@example.invalid",
            ] {
                assert!(!text.contains(forbidden), "{text}");
            }
            assert_eq!(records, before);
            records
                .logins
                .labels
                .get_mut("one")
                .unwrap()
                .sign_in_attempt
                .as_mut()
                .unwrap()
                .routes = "different private metadata".into();
            assert_eq!(project(&records, 200_000), view);
            assert_eq!(before.state, records.state);
            assert_eq!(before.roster, records.roster);
        }
    }
}

#[test]
fn a_successful_past_attempt_is_not_a_current_login_or_quota_reading() {
    let mut records = records();
    records.logins.labels.insert(
        "one".into(),
        accounts::Binding {
            renewed_at: Some(100),
            sign_in_attempt: Some(accounts::SignInAttempt {
                outcome: AttemptOutcome::SignedIn,
                routes: "old route metadata".into(),
                since: 100,
            }),
            ..Default::default()
        },
    );
    let view = project(&records, 200_000);
    assert!(fact(&view, "Last observed sign-in").starts_with("Unknown"));
    assert!(fact(&view, "Last automatic sign-in").contains("current sign-in is unverified"));
    assert!(fact(&view, "5-hour quota").starts_with("Unknown"));
    assert!(fact(&view, "Weekly quota").starts_with("Unknown"));
}

#[test]
fn retained_quota_preserves_its_times_and_becomes_unknown_after_reset_or_refill() {
    let mut records = records();
    records.state.accounts.insert(
        "one".into(),
        accounts::Account {
            windows: [(
                accounts::SESSION_LIMIT.into(),
                accounts::WindowFig {
                    pct_x10: 425,
                    reset: 300,
                    at: 100,
                    by: "opaque-machine-id".into(),
                    source: accounts::SOURCE_TAP.into(),
                    epoch_at: None,
                },
            )]
            .into(),
            ..Default::default()
        },
    );
    let view = project(&records, 200_000);
    assert!(fact(&view, "5-hour quota").contains("42.5%"));
    assert_eq!(
        fact(&view, "5-hour quota observed"),
        recorded_time(Some(100))
    );
    assert_eq!(
        fact(&view, "5-hour recorded reset"),
        recorded_time(Some(300))
    );
    assert!(fact(&view, "Weekly quota").starts_with("Unknown"));
    let expired = project(&records, 300_000);
    assert!(fact(&expired, "5-hour quota").starts_with("Unknown"));
    assert_eq!(
        fact(&expired, "5-hour quota observed"),
        fact(&view, "5-hour quota observed")
    );
    records
        .state
        .accounts
        .get_mut("one")
        .unwrap()
        .windows
        .get_mut(accounts::SESSION_LIMIT)
        .unwrap()
        .epoch_at = Some(150);
    let cut = project(&records, 200_000);
    assert!(fact(&cut, "5-hour quota").starts_with("Unknown"));
    assert!(!format!("{view:?}").contains("opaque-machine-id"));
}

/// A 5-HOUR WINDOW CLAUDE CODE DOES NOT REPORT IS SAID SO, NOT "NO RETAINED QUOTA READING": the
/// usage tap's newest reading of the row carried the week and no 5-hour window (Claude Code hands
/// one only while the API reports it), so the 5-hour quota says `Not reported` with when it was
/// read, and the week keeps its figure. Negative controls: a reading that carried the window
/// shows its figure, a `/usage` panel's week-only sample (a narrow pane may cut a row) and a row
/// with no reading stay `Unknown; no retained quota reading`, and a figure retained from another
/// reading of the login is shown in place of the gap.
#[test]
fn a_five_hour_window_claude_code_does_not_report_is_said_so() {
    const AT: i64 = 1_792_000_000;
    let now_ms = u64::try_from(AT + 60).expect("positive") * 1000;
    let week = accounts::TapRead {
        seven_day: Some(accounts::WindowRead {
            pct_x10: 10,
            reset: AT + 3 * 86_400,
            refilled: None,
        }),
        at: AT,
        seen_before: true,
        ..Default::default()
    };
    let mut tapped = records();
    accounts::note_reading(&mut tapped.state, "one", &week, AT);
    let view = project(&tapped, now_ms);
    let quota = fact(&view, "5-hour quota");
    assert_eq!(
        quota,
        format!(
            "Not reported; Claude Code's usage reading of {} carried the week but no 5-hour window, and no retained record holds one",
            recorded_time(Some(AT))
        )
    );
    assert!(!quota.contains('%'), "{quota}");
    assert_eq!(fact(&view, "Weekly quota"), "1% used (retained)");

    let mut carried = records();
    let both = accounts::TapRead {
        five_hour: Some(accounts::WindowRead {
            pct_x10: 425,
            reset: AT + 3600,
            refilled: None,
        }),
        ..week
    };
    accounts::note_reading(&mut carried.state, "one", &both, AT);
    let view = project(&carried, now_ms);
    assert!(fact(&view, "5-hour quota").contains("42.5%"));

    let mut panel = records();
    accounts::note_sample(
        &mut panel.state,
        "one",
        accounts::Sample {
            week_pct: Some(1),
            week_reset: Some(AT + 3 * 86_400),
            at: AT,
            ..Default::default()
        },
        AT,
    );
    let view = project(&panel, now_ms);
    assert_eq!(
        fact(&view, "5-hour quota"),
        "Unknown; no retained quota reading"
    );
    let view = project(&records(), now_ms);
    assert_eq!(
        fact(&view, "5-hour quota"),
        "Unknown; no retained quota reading"
    );

    let mut held = records();
    accounts::note_reading(&mut held.state, "one", &both, AT - 60);
    let busier = accounts::TapRead {
        seven_day: Some(accounts::WindowRead {
            pct_x10: 20,
            reset: AT + 3 * 86_400,
            refilled: None,
        }),
        ..week
    };
    accounts::note_reading(&mut held.state, "one", &busier, AT);
    assert_eq!(
        held.state.accounts["one"]
            .sample
            .as_ref()
            .map(|s| (s.session_pct, s.week_pct)),
        Some((Some(43), Some(2))),
        "the week-only reading was booked over the held window"
    );
    let view = project(&held, now_ms);
    assert!(fact(&view, "5-hour quota").contains("42.5%"));
    assert_eq!(fact(&view, "5-hour quota source"), "Usage tap");
}

#[test]
fn unrelated_private_metadata_cannot_enter_the_projection() {
    let mut records = records();
    records.roster.rows[0].dir = Some("/private/first-login".into());
    records.roster.rows[0].held_by = Some("own-machine".into());
    records.logins.labels.insert(
        "one".into(),
        accounts::Binding {
            dir: Some("/private/first-binding".into()),
            ..Default::default()
        },
    );
    let before = project(&records, 200_000);
    let row = &mut records.roster.rows[0];
    row.dir = Some("/private/credential-marker".into());
    row.email = Some("secret@example.invalid".into());
    row.identity_email = Some("identity@example.invalid".into());
    row.browser = Some("private-browser-profile".into());
    row.held_by = Some("opaque-machine-id".into());
    row.was = vec![Some("/private/old-login".into())];
    let binding = records.logins.labels.get_mut("one").unwrap();
    binding.dir = Some("/private/new-binding".into());
    binding.proved_dir = Some("/private/proved-login".into());
    binding.proved_email = Some("proof@example.invalid".into());
    binding.subscription = Some("private-subscription-marker".into());
    records.state.machine = "opaque-machine-id".into();
    records.notes = vec!["credential-marker raw source error".into()];
    let after = project(&records, 200_000);
    assert_eq!(after.rows, before.rows);
    assert!(!format!("{after:?}").contains("elsewhere"));
    for forbidden in [
        "credential-marker",
        "example.invalid",
        "private-browser",
        "opaque-machine-id",
        "raw source error",
    ] {
        assert!(!format!("{after:?}").contains(forbidden));
    }
}

#[test]
fn an_unavailable_snapshot_is_distinct_from_an_observed_empty_roster() {
    let unavailable = unavailable("Local account records could not be read consistently");
    let empty = project(&AccountRecords::default(), 200_000);
    assert_eq!(unavailable.observed_at_ms, None);
    assert_eq!(empty.observed_at_ms, Some(200_000));
    assert_ne!(unavailable.status, empty.status);
    assert!(unavailable.rows.is_empty() && empty.rows.is_empty());
}

#[test]
fn invalid_times_are_unknown_and_future_quota_does_not_claim_current_capacity() {
    let unknown = recorded_time(None);
    for at in [0, -1, LAST_TIMESTAMP + 1, i64::MAX] {
        assert_eq!(recorded_time(Some(at)), unknown);
    }
    for at in [1, LAST_TIMESTAMP] {
        assert_ne!(recorded_time(Some(at)), unknown);
    }
    let mut records = records();
    records.state.accounts.insert(
        "one".into(),
        accounts::Account {
            auth: Auth::Ok,
            auth_at: Some(250),
            windows: [(
                accounts::SESSION_LIMIT.into(),
                accounts::WindowFig {
                    pct_x10: 425,
                    reset: 400,
                    at: 250,
                    by: "opaque-machine-id".into(),
                    source: accounts::SOURCE_TAP.into(),
                    epoch_at: None,
                },
            )]
            .into(),
            ..Default::default()
        },
    );
    let view = project(&records, 200_000);
    assert!(fact(&view, "5-hour quota").starts_with("Unknown"));
    assert!(!fact(&view, "5-hour quota").contains('%'));
    assert!(fact(&view, "5-hour quota observed").contains("future-dated"));
    assert!(fact(&view, "Sign-in observed").contains("future-dated"));
    assert!(fact(&view, "Last observed sign-in").contains("not checked now"));
}
