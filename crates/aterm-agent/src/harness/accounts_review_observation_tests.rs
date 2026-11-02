// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ai::store::{Ended, Outcome};

const NOW: i64 = 1_800_000_000;

fn review_case(provider: i64, location: i64, outcome: i64, case: i64) -> [bool; 5] {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let cli = if provider == 0 {
        Cli::Codex
    } else {
        Cli::Claude
    };
    let home = root.path().join(cli.accounts().default_home);
    let store = accounts::Store::at_for(root.path(), cli, root.path());
    let path = root.path().join(cli.accounts().roster_file);
    let mut row = Row {
        label: "account".into(),
        dir: (location == 1).then(|| root.path().join("login").display().to_string()),
        ..Row::default()
    };
    fs::write(
        &path,
        accounts::append_rows_for(cli, "", &[row.clone()]).unwrap(),
    )
    .unwrap();
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            accounts::note_served_login(logins, "account", NOW - 20);
            accounts::note_auth(state, "foreign", accounts::Auth::Ok, NOW - 20);
            Ok(accounts::note_auth(
                state,
                "account",
                if outcome == 0 {
                    accounts::Auth::SignedOut
                } else {
                    accounts::Auth::Ok
                },
                NOW - 10,
            ))
        })
        .unwrap();
    if matches!(case, 12 | 13) {
        store
            .transact_logins(NOW, &mut None, |_, logins| {
                logins.labels.remove("account");
                Ok(Vec::new())
            })
            .unwrap();
    }
    let mut login = accounts::login_env_for(cli, &row);
    if case == 8 {
        login.1 = Some(
            root.path()
                .join("different/auth.json")
                .display()
                .to_string(),
        );
    }
    let witness = capture_review_login(&store, cli, &path, "account", &login, NOW);
    match case {
        1 => {
            store
                .transact_logins(NOW, &mut None, |_, logins| {
                    accounts::note_served_login(logins, "account", NOW - 5);
                    Ok(Vec::new())
                })
                .unwrap();
        }
        2 => {
            store
                .transact_logins(NOW, &mut None, |_, logins| {
                    logins.labels.get_mut("account").unwrap().renewal_generation += 1;
                    Ok(Vec::new())
                })
                .unwrap();
        }
        3 | 4 => {
            if case == 3 {
                row.email = Some("replacement@example.test".into());
            } else {
                row.held_by = Some("m2".into());
            }
            fs::write(&path, accounts::append_rows_for(cli, "", &[row]).unwrap()).unwrap();
        }
        5 | 9 | 12 => {
            store
                .transact_logins(NOW, &mut None, |state, logins| {
                    accounts::move_account(state, logins, "account", "away")?;
                    Ok(Vec::new())
                })
                .unwrap();
            if matches!(case, 9 | 12) {
                store
                    .transact_logins(NOW, &mut None, |state, logins| {
                        accounts::move_account(state, logins, "away", "account")?;
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
        }
        10 => {
            store
                .transact_logins(NOW, &mut None, |state, logins| {
                    Ok(accounts::note_renewed(
                        state,
                        logins,
                        "foreign",
                        None,
                        root.path().join("foreign").to_str().unwrap(),
                        None,
                        NOW,
                    ))
                })
                .unwrap();
        }
        11 | 13 => {
            store
                .transact_logins(NOW, &mut None, |state, logins| {
                    accounts::forget_account(state, logins, "account");
                    Ok(accounts::note_auth(
                        state,
                        "account",
                        if outcome == 0 {
                            accounts::Auth::SignedOut
                        } else {
                            accounts::Auth::Ok
                        },
                        NOW - 10,
                    ))
                })
                .unwrap();
        }
        14 | 15 if outcome == 2 => {
            let cleared_at = if case == 15 { NOW } else { NOW + 1 };
            store
                .transact(cleared_at, &mut None, |state| {
                    Ok(vec![accounts::pin(
                        state,
                        &accounts::Roster::load(&path)?,
                        &["account".to_owned()].into_iter().collect(),
                        "account",
                        cleared_at,
                    )?])
                })
                .unwrap();
        }
        16 => {
            if location == 0 {
                record_default_sign_in(&store, cli, &path, witness.as_ref().unwrap(), &home, NOW)
                    .unwrap();
            } else {
                store
                    .transact_logins(NOW, &mut None, |state, logins| {
                        let mut events =
                            accounts::note_auth(state, "account", accounts::Auth::SignedOut, NOW);
                        events.extend(accounts::note_signed_back(state, logins, "account", NOW));
                        Ok(events)
                    })
                    .unwrap();
            }
        }
        _ => {}
    }
    let finish = if case > 13 { NOW + 2 } else { NOW };
    let before = store.load(finish).unwrap().0;
    let before_logins = store.load_logins(finish).unwrap().0;
    let other_store = accounts::Store::at_for(&root.path().join("other"), cli, root.path());
    let other_roster = root.path().join("other-roster.toml");
    let result = witness
        .as_ref()
        .ok_or_else(|| "no matching login witness".to_owned())
        .and_then(|witness| {
            record_review_login(
                if case == 6 { &other_store } else { &store },
                cli,
                if case == 7 { &other_roster } else { &path },
                witness,
                &home,
                match outcome {
                    0 => Outcome::Worked,
                    1 => Outcome::Ended(Ended::ReviewAuth),
                    2 => Outcome::Limited(Some(NOW + 3600)),
                    _ => unreachable!(),
                },
                finish,
            )
        });
    let after = store.load(finish).unwrap().0;
    let after_logins = store.load_logins(finish).unwrap().0;
    let applied = result.is_ok()
        && after
            .accounts
            .get("account")
            .is_some_and(|account| match outcome {
                0 => {
                    account.auth == accounts::Auth::Ok
                        && after_logins.sign_in_receipts.contains_key("account")
                }
                1 => account.auth == accounts::Auth::SignedOut,
                2 => account
                    .walls
                    .get("five_hour")
                    .is_some_and(|wall| !wall.cleared),
                _ => unreachable!(),
            });
    [
        result.is_ok(),
        before == after && before_logins.to_json() == after_logins.to_json(),
        applied,
        before.accounts.get("foreign") == after.accounts.get("foreign")
            && before_logins.get("foreign") == after_logins.get("foreign"),
        before_logins.generation_clock != after_logins.generation_clock,
    ]
}

pub(crate) fn review_observation_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Provider" => {
            next.insert("provider", state["provider"] + 1);
        }
        "Location" => {
            next.insert("location", state["location"] + 1);
        }
        "Outcome" => {
            next.insert("outcome", state["outcome"] + 1);
        }
        "Case" => {
            next.insert("case", state["case"] + 1);
        }
        "Record" => {
            next.insert("phase", 1);
            for (field, value) in ["accepted", "preserved", "applied", "foreign", "fenced"]
                .into_iter()
                .zip(review_case(
                    state["provider"],
                    state["location"],
                    state["outcome"],
                    state["case"],
                ))
            {
                next.insert(field, i64::from(value));
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn review_outcomes_conform_to_the_captured_account_and_login() {
    let model = aterm_models_harness::account_review_observation_model();
    assert_eq!(model.mutant_dials(), (1..=9).collect::<Vec<_>>());
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 408);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = review_observation_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let current = review_observation_transition(&model.init_state(), "Record");
    let mut stale = model.init_state();
    stale.insert("case", 2);
    let stale = review_observation_transition(&stale, "Record");
    for (state, field, law) in [
        (&current, "accepted", "CurrentAccepted"),
        (&current, "applied", "CurrentApplied"),
        (&stale, "accepted", "StaleRefused"),
        (&stale, "preserved", "RefusalPreserves"),
        (&current, "foreign", "ForeignPreserved"),
        (&current, "fenced", "NoCredentialFence"),
    ] {
        let mut bad = state.clone();
        bad.insert(field, 1 - state[field]);
        assert!(!model.check_invariant(law, &bad));
    }
}
