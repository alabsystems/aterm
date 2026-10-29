// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State as ModelState;

fn preparation_case(moving: bool, default: bool, case: i64) -> ([(&'static str, i64); 8], bool) {
    let cli = Cli::Codex;
    let world = World::new("preparation-fallback");
    let work = world.two(cli);
    world.on_main(cli, &[]);
    let failed = if default { "main" } else { "work" };
    let other = if default { "work" } else { "main" };
    let source = world.root.join("source");
    if moving {
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("auth.json"), "{}").unwrap();
        let path = world.cfg.join(cli.accounts().roster_file);
        let mut roster = world.seat().roster(cli).unwrap();
        roster.rows.push(Row {
            label: "source".into(),
            dir: Some(source.to_string_lossy().into_owned()),
            ..Row::default()
        });
        // Make the selected failure precede the alternative in candidate order.
        for row in &mut roster.rows {
            if row.label == other {
                row.class = accounts::Class::Shared;
            }
        }
        std::fs::write(
            path,
            accounts::append_rows_for(cli, "", &roster.rows).unwrap(),
        )
        .unwrap();
    }
    let store = world.seat().store(cli);
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            // Give every row its own generation so an excluded renewal tests
            // its fence, rather than the conservative legacy global fence.
            for row in world.seat().roster(cli).unwrap().rows {
                logins
                    .labels
                    .insert(row.label, accounts::Binding::default());
            }
            state.active = Some(
                if moving && !matches!(case, 2 | 4 | 8) {
                    "source"
                } else {
                    failed
                }
                .into(),
            );
            state.proven = case != 4;
            state.pinned = case == 2;
            if case == 11 {
                for row in world.seat().roster(cli).unwrap().rows {
                    accounts::note_auth(state, &row.label, accounts::Auth::Ok, NOW);
                }
            }
            if case == 4 {
                state.canary = Some(accounts::Canary {
                    sid: "first-canary".into(),
                    at: NOW - HOUR,
                    took: false,
                });
            }
            if case == 7 && moving {
                state.rotations = vec![NOW - MIN; 4];
            }
            Ok(accounts::note_sample(
                state,
                failed,
                Sample {
                    session_pct: Some(10),
                    at: NOW,
                    ..Sample::default()
                },
                NOW,
            ))
        })
        .unwrap();
    let mut attempted = Vec::new();
    let prepared = std::cell::Cell::new(false);
    let first_failed = std::cell::Cell::new(false);
    let mut prepare = |row: &Row| {
        attempted.push(row.label.clone());
        if row.label == failed || case == 1 {
            first_failed.set(first_failed.get() || row.label == failed);
            return Err(io::Error::other("synthetic preparation failure"));
        }
        assert_eq!(row.label, other);
        if case == 3 {
            store
                .transact(NOW, &mut None, |state| {
                    state.active = Some(failed.into());
                    state.pinned = true;
                    Ok(Vec::new())
                })
                .unwrap();
        }
        if case == 5 {
            store
                .transact(NOW, &mut None, |state| {
                    state.active = Some(failed.into());
                    state.proven = false;
                    state.canary = Some(accounts::Canary {
                        sid: "concurrent-canary".into(),
                        at: NOW,
                        took: false,
                    });
                    Ok(Vec::new())
                })
                .unwrap();
        }
        if case == 6 {
            let renewed = world.root.join("renewed");
            std::fs::create_dir_all(&renewed).unwrap();
            std::fs::write(renewed.join("auth.json"), "{}").unwrap();
            store
                .transact_logins(NOW, &mut None, |state, logins| {
                    Ok(accounts::note_renewed(
                        state,
                        logins,
                        failed,
                        if default {
                            None
                        } else {
                            Some(work.to_str().unwrap())
                        },
                        renewed.to_str().unwrap(),
                        None,
                        NOW,
                    ))
                })
                .unwrap();
        }
        if case == 9 {
            let path = world.cfg.join(cli.accounts().roster_file);
            let mut roster = world.seat().roster(cli).unwrap();
            roster
                .rows
                .iter_mut()
                .find(|row| row.label == failed)
                .unwrap()
                .browser = Some("new-profile".into());
            std::fs::write(
                path,
                accounts::append_rows_for(cli, "", &roster.rows).unwrap(),
            )
            .unwrap();
        }
        if case == 10 {
            store
                .transact(NOW, &mut None, |state| {
                    Ok(accounts::note_auth(
                        state,
                        failed,
                        accounts::Auth::Ok,
                        NOW + 1,
                    ))
                })
                .unwrap();
        }
        if case == 11 {
            let roster = world.seat().roster(cli).unwrap();
            let enrolled = accounts::enrolled_for(cli, &roster);
            let transitions = if moving {
                vec![("source", failed)]
            } else {
                vec![(failed, other), (other, failed)]
            };
            store
                .transact(NOW, &mut None, |state| {
                    let mut events = Vec::new();
                    for (from, to) in &transitions {
                        events.push(
                            accounts::begin_rotation(
                                state,
                                &roster,
                                &enrolled,
                                Some(from),
                                accounts::Mover {
                                    sid: "racer",
                                    account: from,
                                },
                                to,
                                NOW,
                            )
                            .unwrap(),
                        );
                        events.extend(accounts::prove(state, to, NOW));
                    }
                    Ok(events)
                })
                .unwrap();
        }
        let env = login_env(cli, row, &world.home)?;
        prepared.set(true);
        Ok(env)
    };
    let (returned, target, details) = if moving {
        let auth = source.join("auth.json").to_string_lossy().into_owned();
        let env = [("CODEX_AUTH_PATH", auth.as_str())];
        let mut session = session(cli, Supervisor::Aterm, Mode::Early, Path::new("/opt/codex"));
        session.env = &env;
        session.point = Point::Wall {
            window: "five_hour".into(),
            at: None,
        };
        let result = super::session_move_using(&world.seat(), &session, |row| {
            prepare(row)
                .ok()
                .and_then(|env| relaunch::pool_line(&session.relaunch, cli, &env).ok())
        });
        let target = match &result.advice {
            Advice::Move { to, .. } => to == other,
            _ => result.line.is_none(),
        };
        let details = match case {
            1 | 2 | 4 | 8 => result == Moved::stay(StayWhy::NoLine),
            3 | 5 | 6 | 9 | 10 | 11 => result == Moved::stay(StayWhy::NothingDue),
            _ => result.line.is_some(),
        };
        (result.line.is_some(), target, details)
    } else {
        let result = super::super::launch_env_using(&world.seat(), cli, &no_usage, None, prepare);
        let target = result
            .as_ref()
            .ok()
            .and_then(|env| env.as_ref())
            .is_none_or(|env| env.label == other);
        let details = match &result {
            Err(why) if case == 1 || case == 2 => why.contains("synthetic preparation failure"),
            Err(why) if case == 3 || case == 5 || case == 11 => {
                why.contains("active account changed")
            }
            Err(why) if case == 6 || case == 9 || case == 10 => {
                why.contains("failed account policy or login changed")
            }
            Ok(Some(_)) => !matches!(case, 1 | 2 | 3 | 5 | 6 | 9 | 10 | 11),
            _ => false,
        };
        (result.is_ok_and(|env| env.is_some()), target, details)
    };
    assert_eq!(attempted.first().map(String::as_str), Some(failed));
    assert_eq!(attempted.iter().filter(|label| *label == failed).count(), 1);
    let after = world.load(cli);
    let rows = world.seat().roster(cli).unwrap().rows.len();
    assert_eq!(rows, if moving { 3 } else { 2 });
    if returned && moving {
        assert_eq!(after.active.as_deref(), Some(other));
    }
    if case == 2 || case == 3 {
        assert!(after.pinned);
        assert_eq!(after.active.as_deref(), Some(failed));
    }
    if case == 4 || case == 5 {
        assert!(!after.proven);
        assert_eq!(
            after.canary.as_ref().unwrap().sid,
            if case == 4 {
                "first-canary"
            } else {
                "concurrent-canary"
            }
        );
    }
    (
        [
            ("returned", i64::from(returned)),
            ("attempts", i64::try_from(attempted.len()).unwrap()),
            (
                "committed",
                i64::try_from(
                    after.rotations.len()
                        - usize::from(case == 7 && moving) * 4
                        - if case == 11 {
                            if moving { 1 } else { 2 }
                        } else {
                            0
                        },
                )
                .unwrap(),
            ),
            ("target", i64::from(target)),
            ("details", i64::from(details)),
            (
                "retained",
                i64::from(after.account(failed).auth != accounts::Auth::SignedOut),
            ),
            ("rows", i64::try_from(rows).unwrap()),
            ("failed", i64::from(first_failed.get())),
        ],
        prepared.get(),
    )
}

pub(crate) fn preparation_transition(state: &ModelState, action: &str) -> ModelState {
    let mut next = state.clone();
    match action {
        "ChooseMove" => {
            next.insert("moving", 1);
            next.insert("rows", 3);
        }
        "FailedDefault" => {
            next.insert("default", 1);
        }
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "FailFirst" | "TryAlternative" | "Finish" => {
            let (observed, prepared) =
                preparation_case(state["moving"] == 1, state["default"] == 1, state["case"]);
            let attempts = observed
                .iter()
                .find(|(key, _)| *key == "attempts")
                .unwrap()
                .1;
            match action {
                "FailFirst" => {
                    next.insert("attempts", 1);
                    for (key, value) in observed {
                        if matches!(key, "failed" | "rows" | "retained") {
                            next.insert(key, value);
                        }
                    }
                    next.insert("phase", if attempts == 1 { 3 } else { 1 });
                    if attempts == 1 {
                        for (key, value) in observed {
                            next.insert(key, value);
                        }
                    }
                }
                "TryAlternative" => {
                    next.insert("attempts", attempts);
                    next.insert("prepared", i64::from(prepared));
                    next.insert("phase", if prepared { 2 } else { 3 });
                    if !prepared {
                        for (key, value) in observed {
                            next.insert(key, value);
                        }
                    }
                }
                "Finish" => {
                    next.insert("phase", 3);
                    for (key, value) in observed {
                        next.insert(key, value);
                    }
                }
                _ => unreachable!(),
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn preparation_fallback_conforms_for_default_and_dedicated_rows() {
    let model = aterm_models_harness::account_pool_preparation_model();
    assert_eq!(model.mutant_dials(), (1..=11).collect::<Vec<_>>());
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 172);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = preparation_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let failed = model
        .successors("FailFirst", &model.init_state())
        .pop()
        .unwrap();
    let prepared = model.successors("TryAlternative", &failed).pop().unwrap();
    let good = model.successors("Finish", &prepared).pop().unwrap();
    for (field, value, law) in [
        ("returned", 0, "Result"),
        ("attempts", 3, "Attempts"),
        ("committed", 1, "Commit"),
        ("target", 0, "Target"),
        ("details", 0, "Details"),
        ("failed", 0, "FailureObserved"),
        ("rows", 1, "RosterBound"),
        ("retained", 0, "Evidence"),
    ] {
        let mut bad = good.clone();
        bad.insert(field, value);
        assert!(
            !model.check_invariant(law, &bad),
            "{law} missed its negative control"
        );
    }
}
