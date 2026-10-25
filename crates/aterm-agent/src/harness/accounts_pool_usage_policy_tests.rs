// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

pub(crate) fn usage_transition(
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
        "PollAndChoose" => {
            let world = World::new("usage-policy");
            let work = world.two(Cli::Codex);
            world.on_main(Cli::Codex, &[]);
            wall(&world, Cli::Codex, "main", NOW + HOUR);
            let store = world.seat().store(Cli::Codex);
            if state["case"] >= 13 {
                store
                    .transact_logins(NOW - MIN, &mut None, |state, logins| {
                        Ok(accounts::note_renewed(
                            state,
                            logins,
                            "work",
                            work.to_str(),
                            work.to_str().unwrap(),
                            None,
                            NOW - MIN,
                        ))
                    })
                    .unwrap();
            }
            let path = world.cfg.join(Cli::Codex.accounts().roster_file);
            let raw = Roster::load_for(Cli::Codex, &path).unwrap();
            let after_read = std::sync::Mutex::new(None);
            let files = [
                store.state_file(),
                store.logins_file(),
                store.ledger_path(),
                store.wake_path(),
            ];
            let read = |_: Cli, home: &Path, _: i64| {
                if home != work {
                    return None;
                }
                match state["case"] {
                    3 | 4 => {
                        store
                            .transact_logins(NOW, &mut None, |state, logins| {
                                Ok(accounts::note_renewed(
                                    state,
                                    logins,
                                    "work",
                                    work.to_str(),
                                    world.root.join("renewed").to_str().unwrap(),
                                    None,
                                    NOW,
                                ))
                            })
                            .unwrap();
                    }
                    5 | 6 => {
                        let mut changed = raw.clone();
                        changed.rows[1].identity_email = Some("changed@example.test".into());
                        std::fs::write(
                            &path,
                            accounts::append_rows_for(Cli::Codex, "", &changed.rows).unwrap(),
                        )
                        .unwrap();
                    }
                    7 | 8 => {
                        std::fs::write(&path, "").unwrap();
                    }
                    9 | 11 => {
                        std::fs::write(&path, "[[broken").unwrap();
                    }
                    10 | 12 => {
                        std::fs::rename(&path, world.root.join("kept-roster")).unwrap();
                    }
                    13 | 14 => {
                        store
                            .transact_logins(NOW, &mut None, |state, logins| {
                                Ok(accounts::note_renewed(
                                    state,
                                    logins,
                                    "work",
                                    work.to_str(),
                                    work.to_str().unwrap(),
                                    None,
                                    NOW,
                                ))
                            })
                            .unwrap();
                    }
                    15 | 16 => {
                        store
                            .transact_logins(NOW - MIN, &mut None, |state, logins| {
                                Ok(accounts::note_renewed(
                                    state,
                                    logins,
                                    "work",
                                    work.to_str(),
                                    work.to_str().unwrap(),
                                    None,
                                    NOW - MIN,
                                ))
                            })
                            .unwrap();
                    }
                    _ => {}
                }
                *after_read.lock().unwrap() =
                    Some(files.each_ref().map(|path| std::fs::read(path).ok()));
                matches!(state["case"], 1 | 3 | 5 | 7 | 9 | 10 | 13 | 15).then(|| Usage {
                    sample: near_wall(),
                    wall: (state["case"] > 2).then_some(("five_hour", Some(NOW + HOUR))),
                })
            };
            let result = super::super::launch_env(&world.seat(), Cli::Codex, &read);
            let published = store.load(NOW).unwrap().0.account("work").sample.is_some();
            let launched = result.as_ref().is_ok_and(|env| env.is_some());
            if state["case"] > 2 {
                assert_eq!(
                    files.each_ref().map(|path| std::fs::read(path).ok()),
                    after_read.into_inner().unwrap().unwrap()
                );
                assert_eq!(std::fs::read(work.join("auth.json")).unwrap(), b"{}");
            }
            next.insert("phase", 2);
            next.insert("published", i64::from(published));
            next.insert("launched", i64::from(launched));
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn polling_and_launch_conform_to_current_policy() {
    let model = aterm_models_harness::account_pool_usage_policy_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = usage_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let mut changed = model.init_state();
    changed.insert("case", 3);
    changed.insert("phase", 1);
    let mut forged = usage_transition(&changed, "PollAndChoose");
    forged.insert("published", 1);
    forged.insert("launched", 1);
    assert!(!model.check_invariant("CurrentPublication", &forged));
    assert!(!model.check_invariant("CurrentLaunch", &forged));
}

#[test]
fn a_public_usage_poll_with_no_result_still_refuses_changed_policy() {
    let world = World::new("empty-poll-policy");
    let work = world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    let seat = world.seat();
    let roster = seat.bound_roster(Cli::Codex).unwrap();
    let enrolled = accounts::enrolled_for(Cli::Codex, &roster);
    let store = seat.store(Cli::Codex);
    let before = std::fs::read(store.state_file()).unwrap();
    let read = |_: Cli, home: &Path, _: i64| {
        if home == work {
            std::fs::write(world.cfg.join(Cli::Codex.accounts().roster_file), "").unwrap();
        }
        None
    };
    assert!(record_usage(&seat, Cli::Codex, &roster, &enrolled, &read).is_err());
    assert_eq!(std::fs::read(store.state_file()).unwrap(), before);
}
