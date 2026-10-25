// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;

pub(crate) fn observation_transition(
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
            let raw_row = Row {
                label: "main".into(),
                dir: (state["case"] != 11)
                    .then(|| root.path().join("original").to_string_lossy().into_owned()),
                email: Some("owner@example.test".into()),
                ..Row::default()
            };
            std::fs::write(&path, row_text(&raw_row).unwrap()).unwrap();
            let raw = Roster::load(&path).unwrap();
            let mut bindings = Logins::default();
            if state["case"] != 11 {
                note_renewed(
                    &mut State::default(),
                    &mut bindings,
                    "main",
                    raw_row.dir.as_deref(),
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
            let mut initial = State {
                active: Some("main".into()),
                proven: false,
                ..State::default()
            };
            note_auth(&mut initial, "main", Auth::SignedOut, NOW);
            store.save(&initial).unwrap();
            match state["case"] {
                2 => {
                    who.login.clone_from(&raw_row.dir);
                    assert_eq!(who.me(&roster).left, Some("main"));
                }
                3 => {
                    note_renewed(
                        &mut State::default(),
                        &mut bindings,
                        "main",
                        captured.dir.as_deref(),
                        &root.path().join("renewed-again").to_string_lossy(),
                        None,
                        NOW,
                    );
                    store.save_logins(&bindings).unwrap();
                }
                4 => {
                    let changed = Row {
                        email: Some("changed@example.test".into()),
                        ..raw_row.clone()
                    };
                    std::fs::write(&path, row_text(&changed).unwrap()).unwrap();
                }
                5 => who.stored_login = false,
                6 => who.default_home = false,
                7 => {
                    std::fs::rename(store.logins_file(), root.path().join("retained-bindings"))
                        .unwrap();
                }
                8 => {
                    std::fs::write(&path, "[[invalid").unwrap();
                }
                10 => {
                    std::fs::rename(&path, root.path().join("retained-roster")).unwrap();
                }
                _ => {}
            }
            let snapshot = std::fs::read(store.state_file()).unwrap();
            let binding_bytes = std::fs::read(store.logins_file()).ok();
            let _held = (state["case"] == 9).then(|| store.acquire().unwrap());
            let mut written = false;
            let result = store.observe_login_within(
                &[],
                NOW,
                LoginObservation {
                    roster_path: &path,
                    row: &captured,
                    who: &who,
                },
                |state, logins| {
                    written = true;
                    note_served_login(logins, "main", NOW);
                    Ok(prove(state, "main", NOW).into_iter().collect())
                },
            );
            assert_eq!(result.is_ok(), written);
            if written {
                assert!(store.load(NOW).unwrap().0.proven);
                assert_eq!(
                    store
                        .load_logins(NOW)
                        .unwrap()
                        .0
                        .get("main")
                        .unwrap()
                        .served_at,
                    Some(NOW)
                );
            } else {
                assert_eq!(std::fs::read(store.state_file()).unwrap(), snapshot);
                assert_eq!(std::fs::read(store.logins_file()).ok(), binding_bytes);
                assert!(!store.ledger_path().exists());
                assert!(!store.wake_path().exists());
            }
            next.insert("phase", 2);
            next.insert("written", i64::from(written));
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn login_observation_conforms_and_old_or_changed_logins_cannot_prove_a_replacement() {
    let model = aterm_models_harness::account_login_observation_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = observation_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let mut stale = model.init_state();
    stale.insert("case", 3);
    stale.insert("phase", 1);
    let mut forged = observation_transition(&stale, "Record");
    forged.insert("written", 1);
    assert!(!model.check_invariant("OnlyCurrentRecords", &forged));
}
