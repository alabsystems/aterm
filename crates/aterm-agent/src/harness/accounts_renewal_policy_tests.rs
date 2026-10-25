// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;

pub(crate) fn renewal_transition(
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
        "Publish" => {
            let case = state["case"];
            let root = aterm_tempfile::TempDir::new().unwrap();
            let store = Store::at(root.path());
            let path = root.path().join(ACCOUNTS_FILE);
            let raw = Roster {
                rows: vec![
                    Row {
                        label: "main".into(),
                        email: Some("owner@example.test".into()),
                        ..Row::default()
                    },
                    Row {
                        label: "other".into(),
                        dir: Some(root.path().join("other").to_str().unwrap().into()),
                        ..Row::default()
                    },
                ],
            };
            std::fs::write(&path, append_rows("", &raw.rows).unwrap()).unwrap();
            let mut initial = State::default();
            let mut bindings = Logins::default();
            note_renewed(
                &mut initial,
                &mut bindings,
                "main",
                None,
                root.path().join("old").to_str().unwrap(),
                None,
                NOW - 1,
            );
            store.save(&initial).unwrap();
            store.save_logins(&bindings).unwrap();
            if state["case"] == 15 {
                store.save_logins(&Logins::default()).unwrap();
            } else if !matches!(state["case"], 16 | 17) {
                store
                    .transact_logins(NOW - 1, &mut None, |state, logins| {
                        Ok(note_renewed(
                            state,
                            logins,
                            "main",
                            None,
                            root.path().join("old").to_str().unwrap(),
                            None,
                            NOW - 1,
                        ))
                    })
                    .unwrap();
            }
            let (mut bindings, _) = store.load_logins(NOW).unwrap();
            let captured = bound(&raw, &bindings).row("main").unwrap().clone();
            let captured_logins = bindings.clone();
            let staging = store.claim_staging_dir("main").unwrap();
            std::fs::write(staging.join(".claude.json"), b"private fixture").unwrap();
            match state["case"] {
                2 | 3 => {
                    let mut changed = raw.clone();
                    if state["case"] == 2 {
                        changed.rows[0].identity_email = Some("changed@example.test".into());
                    } else {
                        changed.rows[1].class = Class::Shared;
                    }
                    std::fs::write(&path, append_rows("", &changed.rows).unwrap()).unwrap();
                }
                4 => std::fs::rename(&path, root.path().join("kept-roster")).unwrap(),
                5 => std::fs::write(&path, "[[invalid").unwrap(),
                6 => {
                    note_renewed(
                        &mut initial,
                        &mut bindings,
                        "main",
                        captured.dir.as_deref(),
                        root.path().join("concurrent").to_str().unwrap(),
                        None,
                        NOW,
                    );
                    store.save_logins(&bindings).unwrap();
                }
                8 => {
                    std::fs::rename(store.logins_file(), root.path().join("kept-bindings")).unwrap()
                }
                9 => std::fs::write(&path, "").unwrap(),
                10 | 11 => {
                    store
                        .transact_logins(NOW, &mut None, |state, logins| {
                            let was = logins.get("main").unwrap().was.clone();
                            let events = note_renewed(
                                state,
                                logins,
                                "main",
                                None,
                                captured.dir.as_deref().unwrap(),
                                None,
                                if case == 10 { NOW - 1 } else { NOW - 2 },
                            );
                            logins.labels.get_mut("main").unwrap().was = was;
                            Ok(events)
                        })
                        .unwrap();
                }
                12 => {
                    for (from, to) in [("main", "moved"), ("moved", "main")] {
                        store
                            .transact_logins(NOW, &mut None, |state, logins| {
                                move_account(state, logins, from, to);
                                Ok(Vec::new())
                            })
                            .unwrap();
                    }
                }
                13 | 16 => {
                    store
                        .transact_logins(NOW, &mut None, |state, logins| {
                            Ok(note_renewed(
                                state,
                                logins,
                                "other",
                                None,
                                root.path().join("other").to_str().unwrap(),
                                None,
                                NOW,
                            ))
                        })
                        .unwrap();
                }
                14 => {
                    store
                        .transact_logins(NOW, &mut None, |_, logins| {
                            note_served_login(logins, "main", NOW);
                            Ok(Vec::new())
                        })
                        .unwrap();
                }
                15 => {
                    store
                        .transact_logins(NOW, &mut None, |_, logins| {
                            logins.labels.insert("main".into(), Binding::default());
                            Ok(Vec::new())
                        })
                        .unwrap();
                }
                _ => {}
            }
            if state["case"] > 9 {
                assert_eq!(
                    bound(&raw, &store.load_logins(NOW).unwrap().0).row("main"),
                    Some(&captured)
                );
            }
            let files = [
                store.state_file(),
                store.logins_file(),
                store.ledger_path(),
                store.wake_path(),
            ];
            let before = files.each_ref().map(|path| std::fs::read(path).ok());
            let _lock = (state["case"] == 7).then(|| store.acquire().unwrap());
            let result = store.transact_renewal_within(
                &[],
                NOW,
                &mut None,
                RenewalPolicy {
                    roster_path: &path,
                    roster: &raw,
                    row: &captured,
                    logins: &captured_logins,
                },
                |state, logins| {
                    write_renewal_checkpoint(
                        "main",
                        "owner@example.test",
                        staging.to_str().unwrap(),
                        None,
                        NOW,
                        logins,
                    )
                    .map_err(|e| e.to_string())?;
                    Ok(note_renewed(
                        state,
                        logins,
                        "main",
                        None,
                        staging.to_str().unwrap(),
                        None,
                        NOW,
                    ))
                },
            );
            let published = result.is_ok();
            assert_eq!(staging.join(RENEWAL_CHECKPOINT_FILE).exists(), published);
            if published {
                assert_eq!(
                    store
                        .load_logins(NOW)
                        .unwrap()
                        .0
                        .get("main")
                        .unwrap()
                        .dir
                        .as_deref(),
                    staging.to_str()
                );
            } else {
                assert_eq!(
                    files.each_ref().map(|path| std::fs::read(path).ok()),
                    before
                );
                assert_eq!(
                    std::fs::read(staging.join(".claude.json")).unwrap(),
                    b"private fixture"
                );
            }
            next.insert("phase", 2);
            next.insert("published", i64::from(published));
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn renewal_publication_conforms_and_a_changed_policy_cannot_leave_a_checkpoint() {
    let model = aterm_models_harness::account_renewal_policy_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = renewal_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let mut changed = model.init_state();
    changed.insert("case", 2);
    changed.insert("phase", 1);
    let mut forged = renewal_transition(&changed, "Publish");
    forged.insert("published", 1);
    assert!(!model.check_invariant("CurrentPolicyOnly", &forged));
}
