// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::{Cell, RefCell};

#[test]
fn session_move_refuses_policy_already_stale_before_preparation() {
    for follows in [false, true] {
        for (observed, configured) in
            [Mode::Early, Mode::Wall, Mode::Off]
                .into_iter()
                .flat_map(|observed| {
                    [Mode::Early, Mode::Wall, Mode::Off].map(|configured| (observed, configured))
                })
        {
            let cli = Cli::Codex;
            let world = World::new("move-stale-policy");
            world.two(cli);
            world.on_main(cli, &[]);
            let store = world.seat().store(cli);
            if follows {
                store
                    .transact(NOW, &mut None, |state| {
                        state.active = Some("work".into());
                        state.proven = true;
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            std::fs::write(
                world.cfg.join("aterm.toml"),
                format!("[harness]\naccounts = '{}'\n", configured.as_str()),
            )
            .unwrap();
            let before = world.load(cli);
            let prepared = Cell::new(false);
            let mut s = session(cli, Supervisor::Aterm, observed, Path::new("/opt/provider"));
            s.point = Point::Wall {
                window: "five_hour".into(),
                at: Some(NOW - MIN),
            };
            let result = super::super::session_move_using(&world.seat(), &s, |_| {
                prepared.set(true);
                Some("prepared stale-policy command".into())
            });
            let after = world.load(cli);
            assert!(after.accounts["main"].sample.is_some());
            if observed != configured || observed == Mode::Off {
                let why = if observed == configured {
                    StayWhy::Off
                } else {
                    StayWhy::NothingDue
                };
                assert_eq!(
                    result,
                    Moved::stay(why),
                    "observed={observed:?}, configured={configured:?}, follows={follows}"
                );
                assert!(!prepared.get());
                assert_eq!(after.active, before.active);
                assert_eq!(after.rotations, before.rotations);
            } else {
                assert!(
                    result.line.is_some(),
                    "current policy permits the wall move"
                );
                assert!(prepared.get());
                assert_eq!(after.active.as_deref(), Some("work"));
                assert_eq!(after.rotations.len(), usize::from(!follows));
            }
        }
    }
}

#[test]
fn prepared_session_move_refuses_a_target_renewed_in_the_same_directory() {
    let cli = Cli::Codex;
    for follows in [false, true] {
        let world = World::new("move-binding");
        let work = world.two(cli);
        world.on_main(cli, &[]);
        let store = world.seat().store(cli);
        store
            .transact_logins(NOW - MIN, &mut None, |state, logins| {
                logins
                    .labels
                    .insert("main".into(), accounts::Binding::default());
                let events = accounts::note_renewed(
                    state,
                    logins,
                    "work",
                    work.to_str(),
                    work.to_str().unwrap(),
                    None,
                    NOW - MIN,
                );
                if follows {
                    state.active = Some("work".into());
                    state.proven = true;
                }
                Ok(events)
            })
            .unwrap();
        let files = [
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            store.wake_path(),
        ];
        let after = RefCell::new(None);
        let s = session(
            cli,
            Supervisor::Aterm,
            Mode::Early,
            Path::new("/opt/provider"),
        );
        let result = super::super::session_move_using(&world.seat(), &s, |row| {
            let env = login_env(cli, row, &world.home).unwrap();
            let line = relaunch::pool_line(&s.relaunch, cli, &env).unwrap();
            // Same directory and timestamp: only the retained generation changes.
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
            *after.borrow_mut() = Some(files.each_ref().map(|path| std::fs::read(path).ok()));
            Some(line)
        });
        assert!(
            result.line.is_none(),
            "{cli:?}, follows={follows}: {result:?}"
        );
        assert_eq!(
            files.each_ref().map(|path| std::fs::read(path).ok()),
            after.into_inner().unwrap()
        );
    }
}

fn edit_roster(world: &World, cli: Cli, edit: impl FnOnce(&mut Roster)) {
    let path = world.cfg.join(cli.accounts().roster_file);
    let mut roster = Roster::load_for(cli, &path).unwrap();
    edit(&mut roster);
    std::fs::write(
        &path,
        accounts::append_rows_for(cli, "", &roster.rows).unwrap(),
    )
    .unwrap();
}

fn run_case(cli: Cli, follows: bool, case: i64) -> (bool, i64) {
    let world = World::new("move-policy");
    let work = world.two(cli);
    let spare = world.root.join("spare");
    let credential = spare.join(cli.connector().stores[0]);
    std::fs::create_dir_all(credential.parent().unwrap()).unwrap();
    std::fs::write(credential, "{}").unwrap();
    edit_roster(&world, cli, |roster| {
        roster.rows.push(Row {
            label: "spare".into(),
            class: accounts::Class::Shared,
            dir: Some(spare.to_str().unwrap().into()),
            ..Row::default()
        });
    });
    world.on_main(cli, &[]);
    let store = world.seat().store(cli);
    store
        .transact_logins(NOW - MIN, &mut None, |state, logins| {
            // An inserted default-home binding gets a real generation without
            // changing the process's routing. Case 18 retains a legacy absence.
            if case != 18 {
                logins
                    .labels
                    .insert("main".into(), accounts::Binding::default());
            }
            let events = accounts::note_renewed(
                state,
                logins,
                "work",
                work.to_str(),
                work.to_str().unwrap(),
                None,
                NOW - MIN,
            );
            if follows {
                state.active = Some("work".into());
                state.proven = true;
            }
            Ok(events)
        })
        .unwrap();
    let files = [
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.wake_path(),
    ];
    let after = RefCell::new(None);
    let rotations = Cell::new(0);
    let mut held = None;
    let s = session(
        cli,
        Supervisor::Aterm,
        Mode::Early,
        Path::new("/opt/provider"),
    );
    if matches!(case, 23 | 24) {
        let mode = if case == 23 { "off" } else { "wall" };
        std::fs::write(
            world.cfg.join("aterm.toml"),
            format!("[harness]\naccounts = '{mode}'\n"),
        )
        .unwrap();
    }
    if cli != Cli::Codex {
        let result = super::super::session_move_using(&world.seat(), &s, |_| {
            panic!("unmeasured running-session moves must not prepare a relaunch")
        });
        assert_eq!(result, Moved::stay(StayWhy::Unmeasured));
        assert!(world.load(cli).rotations.is_empty());
        return (false, 0);
    }
    let result = super::super::session_move_using(&world.seat(), &s, |row| {
        let env = login_env(cli, row, &world.home).unwrap();
        let line = relaunch::pool_line(&s.relaunch, cli, &env).unwrap();
        match case {
            2..=4 => {
                let next = if case == 2 {
                    world.root.join("renewed")
                } else {
                    work.clone()
                };
                std::fs::create_dir_all(&next).unwrap();
                std::fs::write(next.join(cli.connector().stores[0]), "{}").unwrap();
                let at = if case == 4 { NOW - 2 * MIN } else { NOW - MIN };
                store
                    .transact_logins(at, &mut None, |state, logins| {
                        Ok(accounts::note_renewed(
                            state,
                            logins,
                            "work",
                            work.to_str(),
                            next.to_str().unwrap(),
                            None,
                            at,
                        ))
                    })
                    .unwrap();
            }
            5 => {
                store
                    .transact_logins(NOW, &mut None, |_, logins| {
                        logins.labels.get_mut("main").unwrap().proved_by =
                            Some(accounts::Proof::Status);
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            6 | 7 => edit_roster(&world, cli, |roster| {
                let label = if case == 6 { "work" } else { "main" };
                roster
                    .rows
                    .iter_mut()
                    .find(|row| row.label == label)
                    .unwrap()
                    .browser = Some("changed".into());
            }),
            8 => edit_roster(&world, cli, |roster| {
                roster.rows.retain(|row| row.label != "work")
            }),
            9 => std::fs::write(world.cfg.join(cli.accounts().roster_file), "[[broken").unwrap(),
            10 => std::fs::rename(
                work.join(cli.connector().stores[0]),
                world.root.join("kept-credential"),
            )
            .unwrap(),
            11 => wall(&world, cli, "work", NOW + HOUR),
            12 => {
                store
                    .transact(NOW, &mut None, |state| {
                        state.active = Some("main".into());
                        state.pinned = true;
                        state.proven = true;
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            13 => {
                store
                    .transact_logins(NOW, &mut None, |_, logins| {
                        accounts::note_served_login(logins, "work", NOW);
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            14 => edit_roster(&world, cli, |roster| {
                roster
                    .rows
                    .iter_mut()
                    .find(|row| row.label == "spare")
                    .unwrap()
                    .browser = Some("independent".into());
            }),
            15 | 18 => {
                store
                    .transact_logins(NOW, &mut None, |state, logins| {
                        Ok(accounts::note_renewed(
                            state,
                            logins,
                            "spare",
                            spare.to_str(),
                            spare.to_str().unwrap(),
                            None,
                            NOW,
                        ))
                    })
                    .unwrap();
            }
            16 => std::fs::rename(store.logins_file(), world.root.join("kept-bindings")).unwrap(),
            17 => held = Some(store.acquire().unwrap()),
            19 => std::fs::write(
                world.cfg.join("aterm.toml"),
                "[harness]\naccounts = 'off'\n",
            )
            .unwrap(),
            20 => {
                let codex_dir = world.home.join(".codex");
                std::fs::create_dir_all(&codex_dir).unwrap();
                std::fs::write(
                    codex_dir.join("config.toml"),
                    "cli_auth_credentials_store = 'keyring'\n",
                )
                .unwrap();
            }
            21 => {
                store
                    .transact_logins(NOW, &mut None, |_, logins| {
                        logins.labels.remove("work");
                        Ok(Vec::new())
                    })
                    .unwrap();
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
            22 => {
                store
                    .transact(NOW, &mut None, |state| {
                        state.active = Some(if follows { "main" } else { "work" }.into());
                        state.proven = true;
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            1 => {}
            _ => unreachable!(),
        }
        rotations.set(world.load(cli).rotations.len());
        *after.borrow_mut() = Some(files.each_ref().map(|path| std::fs::read(path).ok()));
        Some(line)
    });
    drop(held);
    let after = after.into_inner();
    assert_eq!(
        after.is_none(),
        matches!(case, 23 | 24),
        "{cli:?}, follows={follows}, case={case}: unexpected prepare state: {result:?}"
    );
    if result.line.is_none()
        && let Some(after) = after
    {
        assert_eq!(
            files.each_ref().map(|path| std::fs::read(path).ok()),
            after,
            "refusal published account state"
        );
    }
    let committed = world.load(cli).rotations.len() - rotations.get();
    (result.line.is_some(), committed as i64)
}

pub(crate) fn move_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherProvider" => {
            next.insert("provider", state["provider"] + 1);
        }
        "FollowActive" => {
            next.insert("follows", 1);
        }
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Choose" => {
            next.insert("phase", 1);
        }
        "Finish" => {
            let cli = match state["provider"] {
                1 => Cli::Codex,
                2 => Cli::Grok,
                3 => Cli::Kimi,
                _ => unreachable!(),
            };
            let (line, committed) = run_case(cli, state["follows"] == 1, state["case"]);
            next.insert("phase", 2);
            next.insert("line", i64::from(line));
            next.insert("committed", committed);
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn session_move_policy_conforms_for_rotation_and_following() {
    let model = aterm_models_harness::account_pool_move_policy_model();
    assert_eq!(model.mutant_dials(), (1..=29).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let mut checked = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = move_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
                checked += usize::from(action.name == "Finish");
            }
        }
    }
    assert_eq!(checked, 2 * 24 + 2 * 2);
    let mut stale = model.init_state();
    stale.insert("case", 3);
    stale.insert("phase", 1);
    let mut stale = move_transition(&stale, "Finish");
    stale.insert("line", 1);
    assert!(!model.check_invariant("NoStaleLine", &stale));
    for case in [23, 24] {
        let mut initial = model.init_state();
        initial.insert("case", case);
        initial.insert("phase", 1);
        let mut bad = move_transition(&initial, "Finish");
        bad.insert("line", 1);
        assert!(!model.check_invariant("NoStaleLine", &bad));
        bad.insert("committed", 1);
        assert!(!model.check_invariant("RefusalChangesNoRotation", &bad));
    }
}
