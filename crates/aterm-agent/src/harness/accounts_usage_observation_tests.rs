// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;

pub(crate) fn usage_observation_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Choose" => {
            next.insert("phase", 1);
        }
        "Record" => {
            let root = aterm_tempfile::TempDir::new().unwrap();
            let store = Store::at(root.path());
            let path = root.path().join("accounts.toml");
            let case = state["case"];
            let row = Row {
                label: "main".into(),
                dir: (!matches!(case, 11 | 18))
                    .then(|| root.path().join("original").to_string_lossy().into_owned()),
                email: Some("owner@example.test".into()),
                ..Row::default()
            };
            std::fs::write(&path, row_text(&row).unwrap()).unwrap();
            let raw = Roster::load(&path).unwrap();
            let mut bindings = Logins::default();
            if !matches!(case, 11 | 18) {
                note_renewed(
                    &mut State::default(),
                    &mut bindings,
                    "main",
                    row.dir.as_deref(),
                    &root.path().join("renewed").to_string_lossy(),
                    None,
                    NOW,
                );
                store.save_logins(&bindings).unwrap();
            }
            let roster = bound(&raw, &bindings);
            let captured = roster.row("main").unwrap().clone();
            let mut who = ProcessLogin {
                login: captured.dir.clone(),
                default_home: true,
                stored_login: true,
            };
            let mut initial = State::default();
            note_auth(&mut initial, "main", Auth::Ok, NOW);
            store.save(&initial).unwrap();
            match case {
                2 => {
                    who.login.clone_from(&row.dir);
                }
                3 => {
                    note_renewed(
                        &mut State::default(),
                        &mut bindings,
                        "main",
                        captured.dir.as_deref(),
                        &root.path().join("newest").to_string_lossy(),
                        None,
                        NOW,
                    );
                    store.save_logins(&bindings).unwrap();
                }
                4 | 13 | 14 | 15 => {
                    let mut changed = row.clone();
                    match case {
                        4 => changed.email = Some("another@example.test".into()),
                        13 => changed.browser = Some("another-profile".into()),
                        14 => changed.class = Class::Shared,
                        15 => changed.identity_email = Some("another@example.test".into()),
                        _ => unreachable!(),
                    }
                    std::fs::write(&path, row_text(&changed).unwrap()).unwrap();
                }
                5 => who.stored_login = false,
                6 => who.default_home = false,
                7 => std::fs::rename(store.logins_file(), root.path().join("retained-bindings"))
                    .unwrap(),
                8 => std::fs::write(&path, "[[invalid").unwrap(),
                10 => std::fs::rename(&path, root.path().join("retained-roster")).unwrap(),
                12 => {
                    who.login.clone_from(&row.dir);
                    let current = Row {
                        dir: captured.dir.clone(),
                        ..row.clone()
                    };
                    let other = Row {
                        label: "another".into(),
                        email: Some("another@example.test".into()),
                        ..row.clone()
                    };
                    std::fs::write(
                        &path,
                        format!(
                            "{}\n{}",
                            row_text(&current).unwrap(),
                            row_text(&other).unwrap()
                        ),
                    )
                    .unwrap();
                }
                16 => {
                    who.login.clone_from(&row.dir);
                    bindings.labels.get_mut("main").unwrap().was.clear();
                    store.save_logins(&bindings).unwrap();
                }
                17 => {
                    note_renewed(
                        &mut State::default(),
                        &mut bindings,
                        "main",
                        captured.dir.as_deref(),
                        &root.path().join("newest").to_string_lossy(),
                        None,
                        NOW,
                    );
                    store.save_logins(&bindings).unwrap();
                    let changed = Row {
                        email: Some("another@example.test".into()),
                        ..row.clone()
                    };
                    std::fs::write(&path, row_text(&changed).unwrap()).unwrap();
                }
                18 => {
                    note_renewed(
                        &mut State::default(),
                        &mut bindings,
                        "main",
                        None,
                        &root.path().join("first-renewal").to_string_lossy(),
                        None,
                        NOW,
                    );
                    store.save_logins(&bindings).unwrap();
                }
                _ => {}
            }
            let before = (
                std::fs::read(store.state_file()).unwrap(),
                std::fs::read(store.logins_file()).ok(),
                std::fs::read(store.ledger_path()).ok(),
                std::fs::read(store.wake_path()).ok(),
            );
            let before_bindings = store.load_logins(NOW).map(|read| read.0);
            let held = (case == 9).then(|| store.acquire().unwrap());
            let mut written = false;
            let result = store.observe_account_within(
                &[],
                NOW,
                LoginObservation {
                    roster_path: &path,
                    row: &captured,
                    who: &who,
                },
                |state, _| {
                    written = true;
                    Ok(note_wall(
                        state,
                        "main",
                        "five_hour",
                        Some(NOW + 3600),
                        NOW,
                        NOW,
                    ))
                },
            );
            drop(held);
            assert_eq!(result.is_ok(), written);
            let after = (
                std::fs::read(store.state_file()).unwrap(),
                std::fs::read(store.logins_file()).ok(),
                std::fs::read(store.ledger_path()).ok(),
                std::fs::read(store.wake_path()).ok(),
            );
            if written {
                assert!(
                    store
                        .load(NOW)
                        .unwrap()
                        .0
                        .account("main")
                        .walls
                        .contains_key("five_hour")
                );
            }
            next.insert("phase", 2);
            next.insert("written", i64::from(written));
            next.insert("preserved", i64::from(written || before == after));
            next.insert(
                "bindings",
                i64::from(before_bindings == store.load_logins(NOW).map(|read| read.0)),
            );
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn account_observations_follow_retained_logins_without_crossing_policy_changes() {
    let model = aterm_models_harness::account_usage_observation_model();
    assert_eq!(model.mutant_dials(), (1..=20).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 15);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = usage_observation_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for (case, field, value, law) in [
        (4, "written", 1, "OnlyAttributed"),
        (3, "written", 0, "RetainedProgress"),
        (4, "preserved", 0, "RefusalPreserves"),
        (1, "bindings", 0, "BindingsUnchanged"),
    ] {
        let mut state = model.init_state();
        state.insert("case", case);
        state.insert("phase", 1);
        let mut bad = usage_observation_transition(&state, "Record");
        bad.insert(field, value);
        assert!(!model.check_invariant(law, &bad), "{law}");
    }
}
