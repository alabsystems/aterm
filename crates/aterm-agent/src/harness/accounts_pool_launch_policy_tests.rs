// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::RefCell;

#[test]
fn prepared_launch_refuses_a_login_renewed_in_the_same_directory() {
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        for off in [false, true] {
            let world = World::new("launch-renewed");
            let work = world.two(cli);
            world.on_main(cli, &[]);
            wall(&world, cli, "main", NOW + HOUR);
            let store = world.seat().store(cli);
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
            if off {
                pin(&world, cli, "work").unwrap();
                std::fs::write(world.cfg.join("aterm.toml"), "[harness]\naccounts='off'\n")
                    .unwrap();
            }
            let result =
                super::super::launch_env_using(&world.seat(), cli, &no_usage, None, |row| {
                    let env = login_env(cli, row, &world.home)?;
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
                    Ok(env)
                });
            assert!(result.is_err(), "{cli:?}, off={off}: {result:?}");
        }
    }
}

fn edit_roster(world: &World, cli: Cli, edit: impl FnOnce(&mut Roster)) {
    let path = world.cfg.join(cli.accounts().roster_file);
    let mut roster = Roster::load_for(cli, &path).unwrap();
    edit(&mut roster);
    std::fs::write(
        path,
        accounts::append_rows_for(cli, "", &roster.rows).unwrap(),
    )
    .unwrap();
}

fn credential(dir: &Path, cli: Cli) {
    let path = dir.join(cli.connector().stores[0]);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "{}").unwrap();
}

fn run_case(cli: Cli, off: bool, case: i64) -> (bool, bool) {
    let world = World::new("launch-policy");
    let work = world.two(cli);
    let spare = world.root.join("spare");
    credential(&spare, cli);
    edit_roster(&world, cli, |roster| {
        roster.rows.push(Row {
            label: "spare".into(),
            class: accounts::Class::Shared,
            dir: Some(spare.to_str().unwrap().into()),
            ..Row::default()
        });
    });
    world.on_main(cli, &[]);
    wall(&world, cli, "main", NOW + HOUR);
    let store = world.seat().store(cli);
    if case != 23 {
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
    if off {
        pin(&world, cli, "work").unwrap();
        std::fs::write(world.cfg.join("aterm.toml"), "[harness]\naccounts='off'\n").unwrap();
    }
    let files = [
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.wake_path(),
    ];
    let after = RefCell::new(None);
    let mut held = None;
    let mut prepared = None;
    let seat = Seat {
        now: NOW + 2,
        ..world.seat()
    };
    let result = super::super::launch_env_using(&seat, cli, &no_usage, None, |row| {
        assert_eq!(row.label, "work");
        let env = login_env(cli, row, &world.home)?;
        prepared = Some(env.clone());
        match case {
            2..=4 => {
                let dir = if case == 2 {
                    world.root.join("renewed")
                } else {
                    work.clone()
                };
                credential(&dir, cli);
                let at = if case == 4 { NOW - 2 * MIN } else { NOW - MIN };
                store
                    .transact_logins(at, &mut None, |state, logins| {
                        Ok(accounts::note_renewed(
                            state,
                            logins,
                            "work",
                            work.to_str(),
                            dir.to_str().unwrap(),
                            None,
                            at,
                        ))
                    })
                    .unwrap();
            }
            5 => edit_roster(&world, cli, |roster| {
                roster
                    .rows
                    .iter_mut()
                    .find(|row| row.label == "work")
                    .unwrap()
                    .browser = Some("changed".into());
            }),
            6 => edit_roster(&world, cli, |roster| {
                roster.rows.retain(|row| row.label != "work")
            }),
            7 => std::fs::write(world.cfg.join(cli.accounts().roster_file), "[[broken").unwrap(),
            8 => std::fs::rename(
                work.join(cli.connector().stores[0]),
                world.root.join("kept-credential"),
            )
            .unwrap(),
            9 => {
                store
                    .transact(NOW, &mut None, |state| {
                        state.accounts.entry("work".into()).or_default().auth =
                            accounts::Auth::SignedOut;
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            10 => {
                store
                    .transact(NOW + 1, &mut None, |state| {
                        Ok(accounts::note_wall(
                            state,
                            "work",
                            "five_hour",
                            Some(NOW + HOUR),
                            NOW + 1,
                            NOW + 1,
                        ))
                    })
                    .unwrap();
            }
            11 | 24 => {
                store
                    .transact(NOW, &mut None, |state| {
                        state.active = Some(if case == 11 { "spare" } else { "missing" }.into());
                        state.pinned = true;
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            12 => {
                store
                    .transact_logins(NOW, &mut None, |_, logins| {
                        accounts::note_served_login(logins, "work", NOW);
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            13 => edit_roster(&world, cli, |roster| {
                roster
                    .rows
                    .iter_mut()
                    .find(|row| row.label == "spare")
                    .unwrap()
                    .browser = Some("independent".into());
            }),
            14 | 23 => {
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
            15 => std::fs::write(store.logins_file(), "not JSON").unwrap(),
            16 => std::fs::write(store.state_file(), "not JSON").unwrap(),
            17 => std::fs::rename(store.logins_file(), world.root.join("kept-logins")).unwrap(),
            18 => held = Some(store.acquire().unwrap()),
            19 => std::fs::write(
                world.cfg.join("aterm.toml"),
                if off {
                    "[harness]\naccounts='early'\n"
                } else {
                    "[harness]\naccounts='off'\n"
                },
            )
            .unwrap(),
            20 => {
                let path = world.home.join(".codex/config.toml");
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, "cli_auth_credentials_store='keyring'\n").unwrap();
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
                        state.active = Some("work".into());
                        state.pinned = !off;
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            25 => {
                store
                    .transact(NOW, &mut None, |state| {
                        Ok(accounts::note_sample(
                            state,
                            "work",
                            Sample {
                                session_pct: Some(10),
                                at: NOW,
                                ..Sample::default()
                            },
                            NOW,
                        ))
                    })
                    .unwrap();
            }
            26 => std::fs::rename(store.state_file(), world.root.join("kept-state")).unwrap(),
            27 => std::fs::rename(
                world.cfg.join(cli.accounts().roster_file),
                world.root.join("kept-roster"),
            )
            .unwrap(),
            1 => {}
            _ => unreachable!(),
        }
        *after.borrow_mut() = Some(files.each_ref().map(|path| std::fs::read(path).ok()));
        Ok(env)
    });
    drop(held);
    let after = after
        .into_inner()
        .unwrap_or_else(|| panic!("{cli:?}, off={off}, case={case}: never prepared: {result:?}"));
    if let Ok(Some(env)) = &result {
        assert_eq!(env.label, "work");
        assert_eq!(Some(env), prepared.as_ref());
    } else {
        assert!(
            result.is_err(),
            "managed refusal became unmanaged: {result:?}"
        );
    }
    (
        result.is_ok(),
        files.each_ref().map(|path| std::fs::read(path).ok()) != after,
    )
}

pub(crate) fn launch_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherProvider" => {
            next.insert("provider", state["provider"] + 1);
        }
        "OffWithPin" => {
            next.insert("off", 1);
        }
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Prepare" => {
            next.insert("phase", 1);
        }
        "Finish" => {
            let cli = match state["provider"] {
                1 => Cli::Codex,
                2 => Cli::Grok,
                3 => Cli::Kimi,
                _ => unreachable!(),
            };
            let (returned, wrote) = run_case(cli, state["off"] == 1, state["case"]);
            next.insert("phase", 2);
            next.insert("returned", i64::from(returned));
            next.insert("wrote", i64::from(wrote));
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn launch_policy_conforms_for_automatic_and_pinned_launches() {
    let model = aterm_models_harness::account_pool_launch_policy_model();
    assert_eq!(model.mutant_dials(), (1..=29).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let states = aterm_spec::interp::reachable_states(&model);
    let mut steps = Vec::new();
    for state in &states {
        for action in &model.actions {
            if model.action_enabled(action.name, state) {
                steps.push((state, action.name));
            }
        }
    }
    // Each Finish builds a World of its own (temp dir, store, lock file), so the steps run side by
    // side on as many workers as the kernel serves file system calls fastest for
    // (`aterm_spec::fs_lane::PLACES`; the lane counts libtest's test threads, not these): case 18's
    // six held-lock launches wait out LOCK_LADDER that many at a time, not in turn. The
    // thread-local seams on the launch path (an `aterm_spec::xref::StepEvidence` window,
    // `aterm_durable::seam::Recording`, `FailingDirs`) are unset here, as on each worker thread;
    // one opened here would not reach the steps.
    let nexts =
        aterm_spec::check::each_on(aterm_spec::fs_lane::PLACES, &steps, |&(state, action)| {
            launch_transition(state, action)
        });
    let mut checked = 0;
    for (&(state, action), next) in steps.iter().zip(&nexts) {
        assert!(
            model.successors(action, state).contains(next),
            "{action}: {state:?} -> {next:?}"
        );
        checked += usize::from(action == "Finish");
    }
    assert_eq!(checked, 3 * 2 * 27);
    let mut stale = model.init_state();
    stale.insert("case", 3);
    stale.insert("phase", 1);
    let mut stale = launch_transition(&stale, "Finish");
    stale.insert("returned", 1);
    assert!(!model.check_invariant("NoStaleLaunch", &stale));
}

#[test]
fn final_launch_check_preserves_failed_overlay_exclusions() {
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        let world = World::new("launch-fallback");
        world.two(cli);
        world.on_main(cli, &[]);
        wall(&world, cli, "main", NOW + HOUR);
        let spare = world.root.join("spare");
        credential(&spare, cli);
        edit_roster(&world, cli, |roster| {
            roster.rows.push(Row {
                label: "spare".into(),
                class: accounts::Class::Shared,
                dir: Some(spare.to_str().unwrap().into()),
                ..Row::default()
            });
        });
        let mut prepared = Vec::new();
        let result = super::super::launch_env_using(&world.seat(), cli, &no_usage, None, |row| {
            prepared.push(row.label.clone());
            if row.label == "work" {
                return Err(io::Error::other("overlay unavailable"));
            }
            login_env(cli, row, &world.home)
        })
        .unwrap()
        .unwrap();
        assert_eq!(prepared, ["work", "spare"]);
        assert_eq!(result.label, "spare");
    }
}
