// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;

fn completion_identity_case(
    shared: bool,
    alternate: bool,
    kind: i64,
    variant: u8,
) -> [(&'static str, i64); 10] {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let store = accounts::Store::at_for(&root.path().join("state"), Cli::Claude, &home);
    let path = root.path().join("roster.toml");
    let target = if alternate { "second" } else { "first" };
    let expected = format!("{target}@example.test");
    let mut roster = Roster {
        rows: ["first", "second"]
            .into_iter()
            .map(|label| Row {
                label: label.into(),
                email: Some(format!("{label}@example.test")),
                dir: Some(store.login_dir(label).unwrap().to_str().unwrap().into()),
                ..Row::default()
            })
            .collect(),
    };
    if variant & 1 != 0 {
        let row = roster
            .rows
            .iter_mut()
            .find(|row| row.label == target)
            .unwrap();
        row.email = Some("delivery-alias@example.test".into());
        row.identity_email = Some(expected.clone());
    }
    fs::write(&path, format_roster_toml(&roster).unwrap()).unwrap();
    if shared {
        let reserved = store.claim_staging_dir("managed").unwrap();
        store
            .transact_logins(NOW - 2, &mut None, |_, logins| {
                logins.shared.begin(
                    accounts::shared::Intent {
                        source: accounts::shared::Source {
                            network: [1; 32],
                            account: [2; 32],
                        },
                        decision: [3; 32],
                        generation: 1,
                        configuration: [4; 32],
                        request: [5; 16],
                        label: "managed".into(),
                        identity: "managed@example.test".into(),
                        staging: reserved.to_str().unwrap().into(),
                        before_generation: None,
                        begun_at: NOW - 2,
                    },
                    super::super::profile::principal_id("managed"),
                )?;
                Ok(Vec::new())
            })
            .unwrap();
    }
    let logins = store.load_logins(NOW).unwrap().0;
    let captured = accounts::bound(&roster, &logins);
    let staging = store.claim_staging_dir("first").unwrap();
    assert!((0..=4).contains(&kind));
    let mut oauth = aterm_json::json!({
        "accountUuid": if variant & 4 != 0 { "managed" } else { "unmanaged" }
    });
    if matches!(kind, 1 | 2) {
        let email = aterm_json::json!(if kind == 2 {
            if variant & 8 != 0 {
                if alternate {
                    "first@example.test"
                } else {
                    "second@example.test"
                }
            } else {
                "foreign@example.test"
            }
        } else {
            &expected
        });
        oauth
            .as_object_mut()
            .unwrap()
            .insert("emailAddress".into(), email);
    }
    if variant & 16 != 0 {
        oauth.as_object_mut().unwrap().remove("accountUuid");
    }
    let bytes = if kind == 0 {
        b"opaque profile fixture".to_vec()
    } else {
        aterm_json::to_vec(&aterm_json::json!({"oauthAccount": oauth})).unwrap()
    };
    let profile = staging.join(if variant & 2 != 0 {
        ".config.json"
    } else {
        ".claude.json"
    });
    let status = super::super::align::Captured {
        stdout: aterm_json::to_vec(&aterm_json::json!({
            "loggedIn": true, "authMethod": "claude.ai", "email": expected,
            "subscriptionType": "max"
        }))
        .unwrap(),
        code: Some(0),
    };
    // The local file is independent of the provider status already captured.
    if variant & 2 != 0 {
        fs::write(staging.join(".claude.json"), br#"{"oauthAccount":{"emailAddress":"wrong-plain@example.test","accountUuid":"managed"}}"#).unwrap();
    }
    fs::write(&profile, &bytes).unwrap();
    if kind == 4 {
        fs::rename(&profile, root.path().join("retained-profile")).unwrap();
        if variant & 2 != 0 {
            fs::rename(
                staging.join(".claude.json"),
                root.path().join("retained-plain-profile"),
            )
            .unwrap();
        }
        // The checkpoint writer can leave its private working directory after a
        // crash. That directory also cannot establish vendor initialization.
        fs::create_dir(staging.join(".renewal-check-interrupted")).unwrap();
    }
    let files = [
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.wake_path(),
    ];
    let before = files.each_ref().map(|path| fs::read(path).ok());
    let notified = std::cell::RefCell::new(Vec::new());
    let outcome = complete_staging_renewal_result(
        &store,
        accounts::RenewalPolicy {
            roster_path: &path,
            roster: &roster,
            row: captured.row("first").unwrap(),
            logins: &logins,
        },
        &staging,
        staging.to_str().unwrap(),
        (&status, Some("fixture route")),
        NOW,
        &|label| {
            // A callback can take the writer and observe the committed binding.
            let writer = store
                .try_acquire()
                .unwrap()
                .expect("publication released its writer");
            drop(writer);
            let bound = store.load_logins(NOW).unwrap().0;
            assert_eq!(bound.get(label).unwrap().dir.as_deref(), staging.to_str());
            notified.borrow_mut().push(label.to_owned());
        },
    );
    let after = store.load_logins(NOW).unwrap().0;
    let bound = after
        .get(target)
        .is_some_and(|row| row.dir.as_deref() == staging.to_str())
        && ["first", "second"]
            .iter()
            .filter(|label| **label != target)
            .all(|label| {
                after
                    .get(label)
                    .is_none_or(|row| row.dir.as_deref() != staging.to_str())
            });
    [
        ("published", i64::from(outcome.published())),
        ("bound", i64::from(bound)),
        (
            "metadata",
            i64::from(files.each_ref().map(|path| fs::read(path).ok()) == before),
        ),
        (
            "preserved",
            i64::from(
                fs::read(if kind == 4 {
                    root.path().join("retained-profile")
                } else {
                    profile
                })
                .ok()
                .as_ref()
                    == Some(&bytes),
            ),
        ),
        (
            "checkpoint",
            i64::from(staging.join(accounts::RENEWAL_CHECKPOINT_FILE).exists()),
        ),
        ("retry", i64::from(outcome.can_retry_authentication())),
        (
            "notified",
            match notified.borrow().as_slice() {
                [] => 0,
                [label] if label == target => 1,
                _ => 2,
            },
        ),
        (
            "requested",
            i64::from(
                after
                    .get("first")
                    .and_then(|binding| binding.sign_in_attempt.as_ref())
                    .is_some_and(|attempt| attempt.outcome == accounts::AttemptOutcome::SignedIn),
            ),
        ),
        (
            "credited",
            i64::from(
                after
                    .get(target)
                    .and_then(|binding| binding.sign_in_attempt.as_ref())
                    .is_some_and(|attempt| attempt.outcome == accounts::AttemptOutcome::SignedIn),
            ),
        ),
        (
            "wrong",
            i64::from(
                after
                    .get("first")
                    .is_some_and(|binding| binding.wrong_at == Some(NOW)),
            ),
        ),
    ]
}

pub(crate) fn completion_identity_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Shared" => {
            next.insert("shared", 1);
        }
        "Alternate" => {
            next.insert("alternate", 1);
        }
        "Credential" => {
            next.insert("credential", state["credential"] + 1);
        }
        "Complete" => {
            let observed = completion_identity_case(
                state["shared"] == 1,
                state["alternate"] == 1,
                state["credential"],
                0,
            );
            for variant in 1..4 {
                assert_eq!(
                    completion_identity_case(
                        state["shared"] == 1,
                        state["alternate"] == 1,
                        state["credential"],
                        variant
                    ),
                    observed,
                    "{state:?}, variant={variant}"
                );
            }
            next.insert("phase", 1);
            for (name, value) in observed {
                next.insert(name, value);
            }
        }
        _ => panic!("unknown completion action"),
    }
    next
}

#[test]
fn automatic_completion_identity_conforms_and_catches_false_publications() {
    let model = aterm_models_harness::account_completion_identity_model();
    assert_eq!(model.mutant_dials(), (1..=17).collect::<Vec<_>>());
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = completion_identity_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let mut before = model.init_state();
    before.insert("credential", 2);
    let refused = model.successors("Complete", &before).pop().unwrap();
    for (field, value, law) in [
        ("published", 1, "Publication"),
        ("bound", 1, "Association"),
        ("checkpoint", 1, "Checkpoint"),
        ("metadata", 0, "Refusal"),
        ("preserved", 0, "Evidence"),
        ("retry", 1, "NoFallback"),
        ("notified", 1, "Notification"),
    ] {
        let mut bad = refused.clone();
        bad.insert(field, value);
        assert!(!model.check_invariant(law, &bad), "{law}");
    }
    let mut before = model.init_state();
    before.insert("alternate", 1);
    before.insert("credential", 1);
    let completed = model.successors("Complete", &before).pop().unwrap();
    for (field, value, law) in [
        ("requested", 1, "RequestedCredit"),
        ("credited", 0, "MatchedCredit"),
        ("wrong", 0, "WrongAccount"),
        ("notified", 0, "Notification"),
        ("notified", 2, "Notification"),
    ] {
        let mut bad = completed.clone();
        bad.insert(field, value);
        assert!(!model.check_invariant(law, &bad), "{law}");
    }
}

#[test]
fn automatic_completion_cannot_replace_missing_vendor_files_with_its_own_checkpoint() {
    for shared in [false, true] {
        for alternate in [false, true] {
            for variant in 0..4 {
                expect_completion(shared, alternate, 4, variant, false);
            }
        }
    }
}

fn expect_completion(shared: bool, alternate: bool, kind: i64, variant: u8, allowed: bool) {
    assert_eq!(
        completion_identity_case(shared, alternate, kind, variant),
        [
            ("published", i64::from(allowed)),
            ("bound", i64::from(allowed)),
            ("metadata", i64::from(!allowed)),
            ("preserved", 1),
            ("checkpoint", i64::from(allowed)),
            ("retry", 0),
            ("notified", i64::from(allowed)),
            ("requested", i64::from(allowed && !alternate)),
            ("credited", i64::from(allowed)),
            ("wrong", i64::from(allowed && alternate)),
        ],
        "shared={shared}, alternate={alternate}, kind={kind}, variant={variant}"
    );
}

#[test]
fn alternate_completion_credits_the_authenticated_account_and_marks_the_requested_mismatch() {
    for shared in [false, true] {
        expect_completion(shared, true, 1, 0, true);
    }
}

#[test]
fn automatic_completion_never_reselects_the_row_named_by_a_replaced_profile() {
    for shared in [false, true] {
        for alternate in [false, true] {
            for variant in 8..12 {
                expect_completion(shared, alternate, 2, variant, false);
            }
        }
    }
}

#[test]
fn automatic_completion_keeps_shared_and_missing_principal_restrictions() {
    for shared in [false, true] {
        for alternate in [false, true] {
            for condition in [4, 16] {
                for variant in 0..4 {
                    expect_completion(shared, alternate, 1, condition | variant, !shared);
                }
            }
        }
    }
}
