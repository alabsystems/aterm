// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const JOURNAL_ACCOUNT: &str = "ac-0123456789abcdef";

fn journal_keyer(realm: &str, email: &str) -> Option<String> {
    match (realm, email) {
        ("claude", "owner-a@example.test") => Some(JOURNAL_ACCOUNT.into()),
        ("claude", "owner-b@example.test") => Some("ac-fedcba9876543210".into()),
        _ => None,
    }
}

fn records() -> AccountRecords {
    let roster = accounts::Roster {
        rows: vec![accounts::Row {
            label: "one".into(),
            ..Default::default()
        }],
    };
    AccountRecords {
        raw_roster: roster.clone(),
        roster,
        ..Default::default()
    }
}

fn admitted_observation(
    records: &mut AccountRecords,
    observe: impl FnOnce(&mut accounts::Logins) -> Vec<accounts::Event>,
) {
    let root = aterm_tempfile::tempdir().unwrap();
    let store = accounts::Store::at(root.path());
    let path = root.path().join(accounts::ACCOUNTS_FILE);
    std::fs::write(
        &path,
        accounts::append_rows_for(Cli::Claude, "", &records.raw_roster.rows).unwrap(),
    )
    .unwrap();
    store
        .observe_admitted_workflow_for_test(&path, "one", 200, |raw, state, logins| {
            assert_eq!(raw, &records.raw_roster);
            state.accounts.clone_from(&records.state.accounts);
            observe(logins)
        })
        .unwrap();
    *records = store.read_account_records(Some(&path), 200).unwrap();
    assert!(
        accounts::current_workflow(records.raw_roster.row("one").unwrap(), &records.logins)
            .is_some()
    );
}

fn journal_records() -> AccountRecords {
    let mut records = records();
    let row = &mut records.roster.rows[0];
    row.email = Some("alias@example.test".into());
    row.identity_email = Some("owner-a@example.test".into());
    records.raw_roster = records.roster.clone();
    records
}

fn project_journal(records: &AccountRecords, seen: &alab_seen::Seen, now_ms: u64) -> LabAccounts {
    aterm_agent::harness::journal::with_keyer(journal_keyer, || {
        project_with(records, Some(seen), now_ms)
    })
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
fn account_projection_keeps_every_row_in_a_large_roster() {
    let mut records = records();
    records.roster.rows = (0..256)
        .map(|index| accounts::Row {
            label: format!("account-{index}"),
            ..Default::default()
        })
        .collect();
    let view = project(&records, 200_000);
    assert_eq!(view.rows.len(), records.roster.rows.len());
    for (shown, source) in view.rows.iter().zip(&records.roster.rows) {
        assert_eq!(shown.label, source.label);
    }
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
                admitted_observation(&mut records, |logins| {
                    accounts::note_sign_in_task(logins, "one", action, 110);
                    outcome.map_or_else(Vec::new, |outcome| {
                        accounts::note_sign_in_attempt(
                            logins,
                            "one",
                            outcome,
                            "credential-marker https://private.invalid/code secret@example.invalid",
                            120,
                        )
                    })
                });
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

#[test]
fn historical_workflow_records_do_not_drive_current_next_steps() {
    for task_retired in [false, true] {
        for attempt_retired in [false, true] {
            let mut records = records();
            records.logins = accounts::Logins::from_json(
                &aterm_json::json!({
                    "schema": accounts::LOGINS_SCHEMA,
                    "generation_clock": 0,
                    "workflow_clock": 1,
                    "workflows": {
                        "one": {
                            "token": 1,
                            "policy": null,
                            "own_dir": null,
                            "dir": null,
                            "legacy_origin": "one",
                            "task_retired": task_retired,
                            "attempt_retired": attempt_retired
                        }
                    },
                    "labels": {
                        "one": {
                            "dir": null,
                            "was": [],
                            "renewal_generation": 0,
                            "sign_in_task": { "action": "browser", "at": 110 },
                            "sign_in_attempt": {
                                "outcome": "failed",
                                "routes": "private-workflow-route",
                                "since": 120
                            }
                        }
                    }
                })
                .to_string(),
                200,
            )
            .unwrap();
            let before = records.clone();
            let view = project(&records, 200_000);
            assert_eq!(
                fact(&view, "Sign-in task observed"),
                recorded_time(Some(110))
            );
            assert_eq!(fact(&view, "Attempt observed"), recorded_time(Some(120)));
            assert!(fact(&view, "Sign-in task applicability").starts_with("Historical only"));
            assert!(fact(&view, "Attempt applicability").starts_with("Historical only"));
            assert!(view.rows[0].facts.iter().all(|(key, _)| key != "Next step"));
            assert!(!format!("{view:?}").contains("private-workflow-route"));
            assert_eq!(records, before);
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
fn a_replaced_logins_auth_is_history_without_current_recovery_guidance() {
    for auth in [Auth::Ok, Auth::SignedOut] {
        for retired in [false, true] {
            let mut records = records();
            records.state = accounts::State::from_json(
                &aterm_json::json!({
                    "schema": accounts::SCHEMA,
                    "accounts": {
                        "one": {
                            "auth": auth.as_str(),
                            "auth_at": 100,
                            "auth_retired": retired
                        }
                    }
                })
                .to_string(),
                200,
            )
            .unwrap();
            let before = records.clone();
            let view = project(&records, 200_000);
            assert_eq!(
                fact(&view, "Last observed sign-in"),
                if auth == Auth::Ok {
                    "Signed in on record; not checked now"
                } else {
                    "Signed out on record"
                }
            );
            assert_eq!(fact(&view, "Sign-in observed"), recorded_time(Some(100)));
            assert_eq!(
                fact(&view, "Sign-in record applicability").starts_with("Historical only"),
                retired
            );
            if auth == Auth::SignedOut && !retired {
                assert_recovery_inspection(&view);
            } else {
                assert!(view.rows[0].facts.iter().all(|(key, _)| key != "Next step"));
            }
            assert_eq!(records, before);
        }
    }
}

fn lifetime_records(
    renewal_retired: bool,
    notice_retired: bool,
    notice_end: i64,
) -> AccountRecords {
    let mut records = records();
    records.logins.labels.insert(
        "one".into(),
        accounts::Binding {
            renewed_at: Some(100),
            notice_end: Some(notice_end),
            served_at: Some(150),
            lifetime_s: accounts::LIFETIME_MIN_S,
            ..Default::default()
        },
    );
    let mut encoded: aterm_json::Value = aterm_json::from_str(&records.logins.to_json()).unwrap();
    let binding = encoded
        .as_object_mut()
        .unwrap()
        .get_mut("labels")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .get_mut("one")
        .unwrap()
        .as_object_mut()
        .unwrap();
    for (name, retired) in [
        ("renewal_retired", renewal_retired),
        ("notice_retired", notice_retired),
        ("served_retired", true),
        ("ended_retired", true),
    ] {
        binding.insert(name.into(), retired.into());
    }
    records.logins = accounts::Logins::from_json(&encoded.to_string(), 200).unwrap();
    records
}

#[test]
fn historical_expiry_dates_do_not_become_a_current_estimate_with_auth_only() {
    for retained_auth in [false, true] {
        for retired in [false, true] {
            let mut records = lifetime_records(retired, retired, 150);
            if retained_auth {
                records.state.accounts.insert(
                    "one".into(),
                    accounts::Account {
                        auth: Auth::Ok,
                        auth_at: Some(200),
                        ..Default::default()
                    },
                );
            }
            let before = records.clone();
            let view = project(&records, 200_000);
            assert_eq!(
                fact(&view, "Last recorded renewal"),
                recorded_time(Some(100))
            );
            assert_eq!(
                fact(&view, "Expiry notice on record"),
                recorded_time(Some(150))
            );
            assert_eq!(
                fact(&view, "Last recorded served turn"),
                recorded_time(Some(150))
            );
            if retired {
                assert!(fact(&view, "Current expiry estimate").starts_with("Unknown"));
            } else {
                assert!(
                    fact(&view, "Current expiry estimate").starts_with(&recorded_time(Some(150)))
                );
            }
            assert_eq!(records, before);
        }
    }
}

#[test]
fn a_current_expiry_notice_does_not_revive_an_older_renewal_estimate() {
    let notice_end = 100 + accounts::LIFETIME_MIN_S + 7200;
    let records = lifetime_records(true, false, notice_end);
    let before = records.clone();
    let view = project(&records, 200_000);
    assert!(fact(&view, "Current expiry estimate").starts_with(&recorded_time(Some(notice_end))));
    assert_eq!(
        fact(&view, "Last recorded renewal"),
        recorded_time(Some(100))
    );
    assert_eq!(
        fact(&view, "Expiry notice on record"),
        recorded_time(Some(notice_end))
    );
    assert_eq!(records, before);
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
            admitted_observation(&mut records, |logins| {
                accounts::note_sign_in_attempt(
                    logins,
                    "one",
                    outcome,
                    "credential-marker https://private.invalid/code secret@example.invalid",
                    120,
                )
            });
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

/// A row whose account the lab journal knows shows who last used it, retained sign-in records
/// with their ages, and each window, by its current identity. A row edited to another identity
/// shows none of the old account's journal facts, even while its stored key names the old one.
/// NEGATIVE CONTROL: using the stored account key keeps the old journal facts after the
/// identity edit, failing the assertion that none remain.
#[test]
fn a_row_shows_what_the_lab_journal_says_of_its_account() {
    let mut records = journal_records();
    records.state.accounts.insert(
        "one".into(),
        accounts::Account {
            pk: Some(JOURNAL_ACCOUNT.into()),
            ..Default::default()
        },
    );
    let mut seen = alab_seen::Seen {
        at: 200,
        ..Default::default()
    };
    seen.accounts.insert(
        JOURNAL_ACCOUNT.into(),
        alab_seen::AccountSeen {
            last_use: Some(alab_seen::LastUse {
                by: "m17".into(),
                t: 200 - 7_200,
            }),
            signed_in: vec![
                alab_seen::SignedIn {
                    on: "m17".into(),
                    state: "in".into(),
                    t: 100,
                },
                alab_seen::SignedIn {
                    on: "m3".into(),
                    state: "out".into(),
                    t: 500,
                },
            ],
            windows: vec![alab_seen::WindowSeen {
                window: "5h".into(),
                sub: None,
                figure: Some(alab_seen::Figure::Used {
                    pct_x10: 420,
                    reset: None,
                }),
                read: Some((140, "m17".into(), "statusline".into())),
                wall_until: None,
            }],
            history: Vec::new(),
        },
    );
    let original = seen.clone();
    let view = project_journal(&records, &seen, 200_000);
    assert_eq!(
        fact(&view, "Last used by (lab journal)"),
        "m17, about 2 h ago (its clock)"
    );
    assert_eq!(
        fact(&view, "Sign-in records (lab journal)"),
        "m17 (in), about 1 min ago (its clock); m3 (out), 5 min ahead (its clock)"
    );
    assert_eq!(
        fact(&view, "Lab 5h quota"),
        "5h 42% used, reset unknown, read on m17, about 1 min ago (its clock) (statusline)"
    );
    let later = project_journal(&records, &seen, 3_800_000);
    assert_eq!(
        fact(&later, "Sign-in records (lab journal)"),
        "m17 (in), about 1 h ago (its clock); m3 (out), about 55 min ago (its clock)"
    );
    assert_eq!(
        fact(&later, "Lab 5h quota"),
        "5h 42% used, reset unknown, read on m17, about 1 h ago (its clock) (statusline)"
    );
    records.roster.rows[0].identity_email = Some("owner-b@example.test".into());
    records.raw_roster = records.roster.clone();
    let other = project_journal(&records, &seen, 200_000);
    assert!(
        !other.rows[0]
            .facts
            .iter()
            .any(|(key, _)| key.contains("lab journal"))
    );
    assert_eq!(
        records.state.accounts["one"].pk.as_deref(),
        Some(JOURNAL_ACCOUNT)
    );
    assert_eq!(seen, original);
}

/// A fresh Lab metadata projection must not repeat a cached journal wall or positive quota
/// after its known end. Future bounds and observations with unknown bounds stay on record.
/// NEGATIVE CONTROL: rendering the raw cached window keeps `100% used` and `at its limit`
/// beside the local quota's already withdrawn reading.
#[test]
fn lab_account_refresh_ages_cached_journal_windows_without_reading_again() {
    let mut records = journal_records();
    records.state.accounts.insert(
        "one".into(),
        accounts::Account {
            pk: Some(JOURNAL_ACCOUNT.into()),
            windows: [(
                accounts::SESSION_LIMIT.into(),
                accounts::WindowFig {
                    pct_x10: 1000,
                    reset: 300,
                    at: 200,
                    by: "m17".into(),
                    source: accounts::SOURCE_TAP.into(),
                    epoch_at: None,
                },
            )]
            .into(),
            ..Default::default()
        },
    );
    let window = |name: &str, reset, wall_until| alab_seen::WindowSeen {
        window: name.into(),
        sub: None,
        figure: Some(alab_seen::Figure::Used {
            pct_x10: 1000,
            reset,
        }),
        read: Some((200, "m17".into(), "statusline".into())),
        wall_until,
    };
    let mut future_read = window("credits", None, Some(None));
    future_read.read = Some((350, "m17".into(), "statusline".into()));
    let seen = alab_seen::Seen {
        at: 200,
        accounts: [(
            JOURNAL_ACCOUNT.into(),
            alab_seen::AccountSeen {
                windows: vec![
                    window("5h", Some(300), Some(Some(300))),
                    window("7d", Some(400), Some(Some(400))),
                    future_read,
                    window("day", None, None),
                ],
                ..Default::default()
            },
        )]
        .into(),
    };
    let original = seen.clone();
    let before = project_journal(&records, &seen, 299_000);
    assert!(fact(&before, "5-hour quota").contains("100%"));
    assert!(fact(&before, "Lab 5h quota").contains("100% used"));
    assert!(fact(&before, "Lab 5h quota").contains("at its limit"));
    assert!(
        fact(&before, "Lab credits quota")
            .contains("read on m17, 51 s ahead (its clock) (statusline)")
    );
    for now_ms in [300_000, 301_000] {
        let view = project_journal(&records, &seen, now_ms);
        assert_eq!(view.observed_at_ms, Some(now_ms));
        assert!(fact(&view, "5-hour quota").starts_with("Unknown"));
        let expired = fact(&view, "Lab 5h quota");
        assert!(expired.contains("not read since"), "{expired}");
        assert!(!expired.contains('%') && !expired.contains("at its limit"));
        assert!(expired.contains("read on m17, about 1 min ago (its clock) (statusline)"));
        let future = fact(&view, "Lab 7d quota");
        assert!(future.contains("100% used") && future.contains("at its limit until"));
        let unknown_wall = fact(&view, "Lab credits quota");
        assert!(unknown_wall.contains("100% used, reset unknown"));
        assert!(unknown_wall.contains("at its limit"));
        let unknown_reset = fact(&view, "Lab day quota");
        assert!(unknown_reset.contains("100% used, reset unknown"));
        assert!(!unknown_reset.contains("at its limit"));
    }
    let later = project_journal(&records, &seen, 360_000);
    assert!(fact(&later, "Lab 5h quota").contains("not read since"));
    assert!(
        fact(&later, "Lab credits quota")
            .contains("read on m17, about 10 s ago (its clock) (statusline)")
    );
    assert_eq!(seen, original);
}
