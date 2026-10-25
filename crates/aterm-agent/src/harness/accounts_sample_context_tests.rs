// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn seed(world: &World, at: i64) {
    let work = world.work_dir(Cli::Codex);
    let mut seat = world.seat();
    seat.now = at;
    let roster = seat.bound_roster(Cli::Codex).unwrap();
    let enrolled = accounts::enrolled_for(Cli::Codex, &roster);
    record_usage(&seat, Cli::Codex, &roster, &enrolled, &|_, home, now| {
        (home == work).then(|| Usage {
            sample: Sample {
                at: now,
                ..near_wall()
            },
            wall: Some((accounts::SESSION_LIMIT, Some(NOW + HOUR))),
        })
    })
    .unwrap();
}

#[test]
fn only_an_accepted_current_sample_gets_the_new_login_context() {
    for at in [NOW, NOW - 20, NOW - MIN, NOW + 1] {
        let world = World::new("sample-context-publication");
        let work = world.two(Cli::Codex);
        seed(&world, NOW - MIN);
        let seat = world.seat();
        let store = seat.store(Cli::Codex);
        store
            .transact_logins(NOW - 10, &mut None, |state, logins| {
                Ok(accounts::note_renewed(
                    state,
                    logins,
                    "work",
                    work.to_str(),
                    work.to_str().unwrap(),
                    None,
                    NOW - 10,
                ))
            })
            .unwrap();
        let before = std::fs::read(store.state_file()).unwrap();
        let roster = seat.bound_roster(Cli::Codex).unwrap();
        let enrolled = accounts::enrolled_for(Cli::Codex, &roster);
        record_usage(&seat, Cli::Codex, &roster, &enrolled, &|_, home, _| {
            (home == work).then(|| Usage {
                sample: Sample { at, ..near_wall() },
                wall: None,
            })
        })
        .unwrap();
        let account = world.load(Cli::Codex).account("work");
        let (logins, _) = store.load_logins(NOW).unwrap();
        let context =
            super::super::sample_context(&seat, Cli::Codex, roster.row("work").unwrap(), &logins);
        if at == NOW {
            assert_eq!(account.sample_context, Some(context));
            assert_eq!(account.sample.unwrap().at, NOW);
            assert_eq!(account.before.unwrap().at, NOW - MIN);
            assert!(
                !account.walls.is_empty(),
                "a new cache key does not erase a limit"
            );
        } else {
            assert_ne!(account.sample_context, Some(context));
            assert_eq!(std::fs::read(store.state_file()).unwrap(), before);
        }
    }
}

#[test]
fn a_declined_same_time_sample_does_not_certify_an_old_context() {
    let world = World::new("sample-context-same-time");
    let work = world.two(Cli::Codex);
    seed(&world, NOW - MIN);
    let path = world.cfg.join(Cli::Codex.accounts().roster_file);
    let mut roster = world.seat().roster(Cli::Codex).unwrap();
    roster.rows[1].identity_email = Some("changed@example.test".into());
    std::fs::write(
        path,
        accounts::append_rows_for(Cli::Codex, "", &roster.rows).unwrap(),
    )
    .unwrap();
    let store = world.seat().store(Cli::Codex);
    let before = std::fs::read(store.state_file()).unwrap();
    record_usage(
        &world.seat(),
        Cli::Codex,
        &roster,
        &accounts::enrolled_for(Cli::Codex, &roster),
        &|_, home, _| {
            (home == work).then(|| Usage {
                sample: near_wall(),
                wall: None,
            })
        },
    )
    .unwrap();
    assert_eq!(std::fs::read(store.state_file()).unwrap(), before);
}

#[test]
fn sample_context_round_trips_but_legacy_and_malformed_tags_are_cache_misses() {
    let world = World::new("sample-context-json");
    world.two(Cli::Codex);
    seed(&world, NOW - MIN);
    let state = world.load(Cli::Codex);
    let text = state.to_json();
    assert_eq!(State::from_json(&text, NOW).unwrap(), state);
    for tag in [
        None,
        Some(aterm_json::Value::Null),
        Some("malformed".into()),
        Some("A".repeat(64).into()),
    ] {
        let mut json: aterm_json::Value = aterm_json::from_str(&text).unwrap();
        let account = json
            .as_object_mut()
            .unwrap()
            .get_mut("accounts")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .get_mut("work")
            .unwrap()
            .as_object_mut()
            .unwrap();
        if let Some(tag) = tag {
            account.insert("sample_context".into(), tag);
        } else {
            account.remove("sample_context");
        }
        let loaded = State::from_json(&aterm_json::to_string(&json).unwrap(), NOW).unwrap();
        assert_eq!(loaded.account("work").sample_context, None);
        assert_eq!(loaded.account("work").sample, state.account("work").sample);
        assert_eq!(loaded.account("work").walls, state.account("work").walls);
    }
}

#[test]
fn a_live_tap_replaces_the_sample_without_inheriting_the_polls_context() {
    let world = World::new("sample-context-tap");
    world.two(Cli::Codex);
    seed(&world, NOW - MIN);
    let mut state = world.load(Cli::Codex);
    assert!(state.account("work").sample_context.is_some());
    let events = accounts::note_reading(
        &mut state,
        "work",
        &accounts::TapRead {
            five_hour: Some(accounts::WindowRead {
                pct_x10: 980,
                reset: NOW + 3 * HOUR,
                refilled: None,
            }),
            at: NOW,
            ..accounts::TapRead::default()
        },
        NOW,
    );
    assert!(!events.is_empty());
    assert_eq!(state.account("work").sample_context, None);
}

pub(crate) fn context_transition(
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
        "Poll" => {
            let world = World::new("sample-context");
            let work = world.two(Cli::Codex);
            let case = state["case"];
            if case == 9 {
                world
                    .seat()
                    .store(Cli::Codex)
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
            seed(&world, NOW - if case == 6 { USAGE_FRESH_S } else { MIN });
            let store = world.seat().store(Cli::Codex);
            match case {
                2 | 7 | 8 => {
                    store
                        .transact(NOW, &mut None, |state| {
                            if case == 8 {
                                return Ok(accounts::note_sample(
                                    state,
                                    "work",
                                    Sample {
                                        at: NOW,
                                        ..near_wall()
                                    },
                                    NOW,
                                ));
                            }
                            let account = state.accounts.get_mut("work").unwrap();
                            if case == 2 {
                                account.sample_context = None;
                            } else {
                                account.sample.as_mut().unwrap().at = NOW + 1;
                            }
                            Ok(Vec::new())
                        })
                        .unwrap();
                }
                3 | 4 => {
                    let mut roster = world.seat().roster(Cli::Codex).unwrap();
                    if case == 3 {
                        roster.rows[1].identity_email = Some("other@example.test".into());
                    } else {
                        let dir = world.root.join("another-login");
                        std::fs::create_dir_all(&dir).unwrap();
                        std::fs::write(dir.join("auth.json"), "{}").unwrap();
                        roster.rows[1].dir = Some(dir.to_str().unwrap().into());
                    }
                    std::fs::write(
                        world.cfg.join(Cli::Codex.accounts().roster_file),
                        accounts::append_rows_for(Cli::Codex, "", &roster.rows).unwrap(),
                    )
                    .unwrap();
                }
                5 => {
                    let work = world.work_dir(Cli::Codex);
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
                9 => {
                    let (mut before, _) = store.load_logins(NOW).unwrap();
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
                    let (mut after, _) = store.load_logins(NOW).unwrap();
                    assert_ne!(before, after);
                    before.labels.get_mut("work").unwrap().renewal_generation = 0;
                    after.labels.get_mut("work").unwrap().renewal_generation = 0;
                    before.generation_clock = 0;
                    after.generation_clock = 0;
                    assert_eq!(before, after, "the generation alone invalidates this cache");
                }
                _ => {}
            }
            let files = [
                store.state_file(),
                store.logins_file(),
                store.ledger_path(),
                store.wake_path(),
            ];
            let before = files.each_ref().map(|path| std::fs::read(path).ok());
            let roster = world.seat().bound_roster(Cli::Codex).unwrap();
            let enrolled = accounts::enrolled_for(Cli::Codex, &roster);
            let reads = std::sync::atomic::AtomicUsize::new(0);
            record_usage(
                &world.seat(),
                Cli::Codex,
                &roster,
                &enrolled,
                &|_, home, _| {
                    if home != world.home.join(Cli::Codex.accounts().default_home) {
                        reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    None
                },
            )
            .unwrap();
            next.insert("phase", 2);
            next.insert("reads", reads.into_inner() as i64);
            next.insert(
                "preserved",
                i64::from(before == files.each_ref().map(|path| std::fs::read(path).ok())),
            );
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn pooled_sample_cache_conforms_to_its_login_context() {
    let model = aterm_models_harness::account_sample_context_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = context_transition(&state, action.name);
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
    let mut forged = context_transition(&changed, "Poll");
    forged.insert("reads", 0);
    assert!(!model.check_invariant("CacheMiss", &forged));
    forged.insert("preserved", 0);
    assert!(!model.check_invariant("NoEvidenceNoErasure", &forged));
}
