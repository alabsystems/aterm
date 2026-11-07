// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn fixture() -> (World, accounts::ProcessLogin) {
    let world = World::new("execution-intent");
    let work = world.two(Cli::Claude);
    world
        .seat()
        .store(Cli::Claude)
        .transact(NOW, &mut None, |state| {
            state.home_accounts.extend(["main".into(), "work".into()]);
            Ok(Vec::new())
        })
        .unwrap();
    let who = accounts::ProcessLogin {
        login: Some(work.display().to_string()),
        default_home: true,
        route: crate::harness::accounts::AuthRoute::Stored,
    };
    (world, who)
}

fn held(world: &World, hold: bool) {
    let mut roster = world.seat().roster(Cli::Claude).unwrap();
    roster
        .rows
        .iter_mut()
        .find(|row| row.label == "work")
        .unwrap()
        .held_by = hold.then(|| "another-machine".into());
    std::fs::write(
        world.cfg.join(Cli::Claude.accounts().roster_file),
        accounts::append_rows_for(Cli::Claude, "", &roster.rows).unwrap(),
    )
    .unwrap();
}

fn renew(world: &World, who: &accounts::ProcessLogin) {
    let directory = who.login.as_deref().unwrap();
    world
        .seat()
        .store(Cli::Claude)
        .transact_logins(NOW, &mut None, |state, logins| {
            Ok(accounts::note_renewed(
                state,
                logins,
                "work",
                Some(directory),
                directory,
                None,
                NOW,
            ))
        })
        .unwrap();
}

pub(crate) fn transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let (world, who) = fixture();
    if state["captured"] == 1 {
        renew(&world, &who);
    }
    let intent = (state["captured"] != 2)
        .then(|| capture_execution(&world.seat(), Cli::Claude, &who).unwrap());
    if state["revision"] == 1 && state["captured"] != 1 {
        renew(&world, &who);
    }
    held(&world, state["held"] == 1);
    let mut guard = (state["guard"] == 1)
        .then(|| admit_execution(&world.seat(), Cli::Claude, intent.as_ref().unwrap()).unwrap());
    let mut next = state.clone();
    match action {
        "Hold" | "Release" | "Renew" => {
            // The same writer exclusion used by roster and credential publication.
            let store = world.seat().store(Cli::Claude);
            let writer = store.acquire_within(&[]).unwrap();
            if writer.is_some() {
                match action {
                    "Hold" => {
                        next.insert("held", 1);
                    }
                    "Release" => {
                        next.insert("held", 0);
                    }
                    "Renew" => {
                        next.insert("revision", 1);
                    }
                    _ => unreachable!(),
                }
            }
        }
        "Capture" => {
            capture_execution(&world.seat(), Cli::Claude, &who).unwrap();
            next.insert("captured", state["revision"]);
            next.insert("phase", 1);
        }
        "Admit" => {
            let admitted =
                admit_execution(&world.seat(), Cli::Claude, intent.as_ref().unwrap()).is_ok();
            next.insert("phase", 2);
            next.insert("granted", i64::from(admitted));
            next.insert("guard", i64::from(admitted));
        }
        "Retry" => {
            next.insert("phase", 1);
            next.insert("granted", 0);
        }
        "Finish" => {
            drop(guard.take());
            assert!(
                world
                    .seat()
                    .store(Cli::Claude)
                    .acquire_within(&[])
                    .unwrap()
                    .is_some()
            );
            next.insert("phase", 3);
            next.insert("guard", 0);
            next.insert("granted", 0);
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn execution_intent_refines_writer_and_generation_model() {
    let model = aterm_models_harness::account_execution_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) >= 5);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    // Deliberately release the real writer before recording the grant: the
    // implementation can then admit a writer, and the formal law must catch it.
    let (world, who) = fixture();
    let intent = capture_execution(&world.seat(), Cli::Claude, &who).unwrap();
    drop(admit_execution(&world.seat(), Cli::Claude, &intent).unwrap());
    assert!(
        world
            .seat()
            .store(Cli::Claude)
            .acquire_within(&[])
            .unwrap()
            .is_some()
    );
    let mut bad = model.init_state();
    bad.insert("guard", 1);
    bad.insert("held", 1);
    assert!(!model.check_invariant("Custody", &bad));
}

#[test]
fn retained_account_intent_rejects_a_hold_and_recovers_without_reselecting() {
    let (world, who) = fixture();
    let seat = world.seat();
    let intent = capture_execution(&seat, Cli::Claude, &who).unwrap();
    assert!(admit_execution(&seat, Cli::Claude, &intent).is_ok());
    held(&world, true);
    assert!(admit_execution(&seat, Cli::Claude, &intent).is_err());
    held(&world, false);
    assert!(admit_execution(&seat, Cli::Claude, &intent).is_ok());
    assert_eq!(capture_execution(&seat, Cli::Claude, &who).unwrap(), intent);
}

#[test]
fn renewing_even_in_the_same_directory_invalidates_retained_execution_intent() {
    let (world, who) = fixture();
    let seat = world.seat();
    let intent = capture_execution(&seat, Cli::Claude, &who).unwrap();
    let directory = who.login.as_deref().unwrap();
    seat.store(Cli::Claude)
        .transact_logins(NOW, &mut None, |state, logins| {
            Ok(accounts::note_renewed(
                state,
                logins,
                "work",
                Some(directory),
                directory,
                None,
                NOW,
            ))
        })
        .unwrap();
    assert!(admit_execution(&seat, Cli::Claude, &intent).is_err());
    let renewed = capture_execution(&seat, Cli::Claude, &who).unwrap();
    assert_ne!(
        renewed, intent,
        "path equality does not prove credential-generation equality"
    );
    assert!(admit_execution(&seat, Cli::Claude, &renewed).is_ok());
}

#[test]
fn in_place_sign_in_changes_execution_generation_without_renewal() {
    for renewed in [false, true] {
        let (world, who) = fixture();
        if renewed {
            renew(&world, &who);
        }
        let intent = capture_execution(&world.seat(), Cli::Claude, &who).unwrap();
        let before = world.seat().store(Cli::Claude).load_logins(NOW).unwrap().0;
        world
            .seat()
            .store(Cli::Claude)
            .transact_logins(NOW, &mut None, |state, logins| {
                Ok(accounts::note_checked_sign_in(state, logins, "work", NOW))
            })
            .unwrap();
        let after = world.seat().store(Cli::Claude).load_logins(NOW).unwrap().0;
        assert_eq!(before.generation_clock, after.generation_clock);
        assert_ne!(
            before.sign_in_receipts.get("work"),
            after.sign_in_receipts.get("work")
        );
        assert!(admit_execution(&world.seat(), Cli::Claude, &intent).is_err());
        let current = capture_execution(&world.seat(), Cli::Claude, &who).unwrap();
        assert_ne!(intent, current);
        assert!(admit_execution(&world.seat(), Cli::Claude, &current).is_ok());
    }
}

#[test]
fn unowned_login_intents_are_fenced_by_store_generation() {
    let (world, _) = fixture();
    let who = accounts::ProcessLogin {
        login: Some(world.root.join("unowned").display().to_string()),
        default_home: true,
        route: crate::harness::accounts::AuthRoute::Stored,
    };
    let intent = capture_execution(&world.seat(), Cli::Claude, &who).unwrap();
    world
        .seat()
        .store(Cli::Claude)
        .transact_logins(NOW, &mut None, |state, logins| {
            Ok(accounts::note_checked_sign_in(state, logins, "work", NOW))
        })
        .unwrap();
    assert!(admit_execution(&world.seat(), Cli::Claude, &intent).is_err());
}

#[test]
fn execution_obeys_pins_pending_pins_and_strict_selection_configuration() {
    let (world, who) = fixture();
    let intent = capture_execution(&world.seat(), Cli::Claude, &who).unwrap();
    let store = world.seat().store(Cli::Claude);
    store
        .transact(NOW, &mut None, |state| {
            state.pinned = true;
            state.active = Some("main".into());
            Ok(Vec::new())
        })
        .unwrap();
    assert!(admit_execution(&world.seat(), Cli::Claude, &intent).is_err());
    store
        .transact(NOW, &mut None, |state| {
            let events = accounts::note_wall(state, "work", "weekly", Some(NOW + HOUR), NOW, NOW);
            // A new wall releases the old pin. The owner's later explicit
            // re-selection is the limited-row override this fixture exercises.
            state.pinned = true;
            state.active = Some("work".into());
            Ok(events)
        })
        .unwrap();
    assert!(
        admit_execution(&world.seat(), Cli::Claude, &intent).is_ok(),
        "an explicit pin keeps its existing limited-row policy"
    );
    store
        .transact(NOW, &mut None, |state| {
            state.pinned = false;
            state.active = None;
            Ok(Vec::new())
        })
        .unwrap();
    aterm_uds::atomic_file::write(
        &world.cfg.join("aterm.toml"),
        b"[harness]\naccounts = \"off\"\n",
        0o600,
    )
    .unwrap();
    assert!(
        admit_execution(&world.seat(), Cli::Claude, &intent).is_err(),
        "off does not retain a named unpinned pool route"
    );
    aterm_uds::atomic_file::write(&world.cfg.join("aterm.toml"), b"not valid TOML = [", 0o600)
        .unwrap();
    assert!(admit_execution(&world.seat(), Cli::Claude, &intent).is_err());
    aterm_uds::atomic_file::write(&world.cfg.join("aterm.toml"), b"", 0o600).unwrap();
    store
        .transact(NOW, &mut None, |state| {
            state.pinned = true;
            state.active = Some("work".into());
            Ok(Vec::new())
        })
        .unwrap();
    assert!(admit_execution(&world.seat(), Cli::Claude, &intent).is_ok());
    store.request_pin("main", NOW).unwrap();
    assert!(
        admit_execution(&world.seat(), Cli::Claude, &intent).is_err(),
        "pending owner selection also constrains execution"
    );
}

#[cfg(unix)]
#[test]
fn aliases_keep_namespace_identity_but_cannot_make_managed_credentials_external() {
    let (world, who) = fixture();
    let intent = capture_execution(&world.seat(), Cli::Claude, &who).unwrap();
    let alias = world.root.join("alias");
    std::os::unix::fs::symlink(&world.root, &alias).unwrap();
    let config = alias.join("cfg");
    let state = alias.join("state");
    let home = alias.join("home");
    let seat = Seat {
        config_dir: &config,
        state: &state,
        home: &home,
        now: NOW,
    };
    assert!(admit_execution(&seat, Cli::Claude, &intent).is_ok());
    let mut alternate = who.clone();
    alternate.login = Some(alias.join("logins/claude/work").display().to_string());
    assert_eq!(
        capture_execution(&seat, Cli::Claude, &alternate).unwrap(),
        intent
    );
    assert!(intent.matches_process(&alternate, true, None));
    for path in [
        home.join(".claude"),
        PathBuf::from(alternate.login.unwrap()),
        world.seat().store(Cli::Claude).logins_dir().join("unbound"),
    ] {
        assert!(admit_external_execution(&seat, Cli::Claude, &[path]).is_err());
    }
    assert!(
        admit_external_execution(&seat, Cli::Claude, &[world.root.join("custom-home")]).is_ok()
    );
}

#[test]
fn execution_grant_retains_the_actual_account_writer_lock() {
    let (world, who) = fixture();
    let seat = world.seat();
    let store = seat.store(Cli::Claude);
    let intent = capture_execution(&seat, Cli::Claude, &who).unwrap();
    let guard = admit_execution(&seat, Cli::Claude, &intent).unwrap();
    assert!(
        store.acquire_within(&[]).unwrap().is_none(),
        "an account writer cannot cross the grant"
    );
    drop(guard);
    assert!(
        store.acquire_within(&[]).unwrap().is_some(),
        "negative control: releasing the grant admits writers"
    );
}

#[cfg(unix)]
#[test]
fn a_writable_policy_cannot_authorize_a_retained_execution() {
    use std::os::unix::fs::PermissionsExt as _;

    let (world, who) = fixture();
    let intent = capture_execution(&world.seat(), Cli::Claude, &who).unwrap();
    let config = world.cfg.join("aterm.toml");
    aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o664)).unwrap();
    assert!(capture_execution(&world.seat(), Cli::Claude, &who).is_err());
    assert!(admit_execution(&world.seat(), Cli::Claude, &intent).is_err());
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(admit_execution(&world.seat(), Cli::Claude, &intent).is_ok());
}

#[test]
fn execution_environment_must_keep_the_captured_route_and_login() {
    let (world, who) = fixture();
    let intent = capture_execution(&world.seat(), Cli::Claude, &who).unwrap();
    assert!(intent.matches_process(&who, true, None));
    assert!(!intent.matches_process(&who, false, None));
    let mut different = who.clone();
    different.login = None;
    assert!(!intent.matches_process(&different, true, None));
    different.route = crate::harness::accounts::AuthRoute::Outside;
    assert!(!intent.matches_process(&different, true, None));
    assert!(ExecutionIntent::external().matches_process(&different, true, None));
    assert!(
        !ExecutionIntent::external().matches_process(&who, true, None),
        "a vanished external override cannot fall back onto the pool"
    );
    assert!(ExecutionIntent::external().matches_process(&who, false, None));
}

#[test]
fn execution_writer_child() {
    let Some(path) = std::env::var_os("ATERM_EXECUTION_WRITER_FIXTURE") else {
        return;
    };
    let store = accounts::Store::at(Path::new(&path));
    let writer = store.acquire_within(&[]).unwrap();
    println!(
        "execution-writer={}",
        if writer.is_some() {
            "available"
        } else {
            "blocked"
        }
    );
}

#[test]
fn execution_grant_excludes_an_independent_writer_process() {
    let (world, who) = fixture();
    let intent = capture_execution(&world.seat(), Cli::Claude, &who).unwrap();
    let guard = admit_execution(&world.seat(), Cli::Claude, &intent).unwrap();
    let probe = |expected: &str| {
        let output = aterm_uds::deadline::output_until(
            aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "harness::accounts_pool::tests::execution::execution_writer_child",
                    "--nocapture",
                ])
                .env("ATERM_EXECUTION_WRITER_FIXTURE", &world.state),
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(expected),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    };
    probe("execution-writer=blocked");
    drop(guard);
    probe("execution-writer=available");
}

#[test]
fn stored_execution_intent_does_not_change_namespaces_or_adopt_an_unowned_login() {
    let (world, who) = fixture();
    let seat = world.seat();
    let intent = capture_execution(&seat, Cli::Claude, &who).unwrap();
    let other = World::new("another-execution-namespace");
    assert!(admit_execution(&other.seat(), Cli::Claude, &intent).is_err());
    let unowned = accounts::ProcessLogin {
        login: Some(world.root.join("unowned").display().to_string()),
        ..who
    };
    let before = capture_execution(&seat, Cli::Claude, &unowned).unwrap();
    let mut roster = seat.roster(Cli::Claude).unwrap();
    roster
        .rows
        .iter_mut()
        .find(|row| row.label == "work")
        .unwrap()
        .dir
        .clone_from(&unowned.login);
    std::fs::write(
        world.cfg.join(Cli::Claude.accounts().roster_file),
        accounts::append_rows_for(Cli::Claude, "", &roster.rows).unwrap(),
    )
    .unwrap();
    assert!(admit_execution(&seat, Cli::Claude, &before).is_err());
}

#[test]
fn intent_storage_is_bounded_canonical_and_unknown_fields_cannot_grant_authority() {
    let (world, who) = fixture();
    let intent = capture_execution(&world.seat(), Cli::Claude, &who).unwrap();
    for intent in [intent, ExecutionIntent::external()] {
        let value = intent.to_value();
        assert_eq!(ExecutionIntent::from_value(&value), Some(intent));
        let mut extended = value;
        extended
            .as_object_mut()
            .unwrap()
            .insert("unrecognized".into(), aterm_json::Value::Bool(true));
        assert_eq!(ExecutionIntent::from_value(&extended), None);
    }
    assert_eq!(
        ExecutionIntent::from_value(&aterm_json::json!({"v": 2, "route": "external"})),
        None
    );
    assert_eq!(
        ExecutionIntent::from_value(&aterm_json::json!({"v": 1, "route": "unattributed"})),
        None
    );
}

/// The lab key of each login email [`token_fixture`]'s roster names.
fn token_keyer(_realm: &str, email: &str) -> Option<String> {
    match email {
        "main@example.com" => Some("ac-00000000000000a1".to_owned()),
        "work@example.com" => Some("ac-00000000000000b2".to_owned()),
        _ => None,
    }
}

/// [`fixture`], its rows naming their login emails, and `work`'s login on its own vault token.
/// Run under [`token_keyer`].
fn token_fixture() -> (World, accounts::ProcessLogin, accounts::TokenEntry) {
    let (world, stored) = fixture();
    let mut roster = world.seat().roster(Cli::Claude).unwrap();
    for row in &mut roster.rows {
        row.email = Some(format!("{}@example.com", row.label));
    }
    std::fs::write(
        world.cfg.join(Cli::Claude.accounts().roster_file),
        accounts::append_rows_for(Cli::Claude, "", &roster.rows).unwrap(),
    )
    .unwrap();
    let entry = accounts::TokenEntry::of_row(roster.row("work").unwrap()).unwrap();
    let who = accounts::ProcessLogin {
        route: accounts::AuthRoute::VaultToken { entry },
        ..stored
    };
    (world, who, entry)
}

#[test]
fn a_token_route_captures_a_token_intent_for_the_account_its_marker_names() {
    crate::harness::journal::with_keyer(token_keyer, || {
        let (world, who, entry) = token_fixture();
        let seat = world.seat();
        let intent = capture_execution(&seat, Cli::Claude, &who).unwrap();
        let ExecutionIntent::Token {
            owner, entry: got, ..
        } = &intent
        else {
            panic!("a token intent: {intent:?}");
        };
        assert_eq!((owner.as_deref(), *got), (Some("work"), entry));
        let value = intent.to_value();
        assert_eq!(value["route"].as_str(), Some("token"));
        assert_eq!(value["entry"].as_str(), Some(entry.name().as_str()));
        assert_eq!(ExecutionIntent::from_value(&value), Some(intent.clone()));
        let mut meta = value.clone();
        meta.as_object_mut().unwrap().insert(
            "entry".into(),
            aterm_json::json!(format!("{}-meta", entry.name())),
        );
        assert_eq!(ExecutionIntent::from_value(&meta), None, "a record's name");
        // The process matches on either of the row's routes, never another account's token.
        let roster = seat.roster(Cli::Claude).unwrap();
        let main = accounts::TokenEntry::of_row(roster.row("main").unwrap()).unwrap();
        let stored = accounts::ProcessLogin {
            route: accounts::AuthRoute::Stored,
            ..who.clone()
        };
        let elsewhere = accounts::ProcessLogin {
            route: accounts::AuthRoute::VaultToken { entry: main },
            ..who.clone()
        };
        let outside = accounts::ProcessLogin {
            route: accounts::AuthRoute::Outside,
            ..who.clone()
        };
        assert_eq!(intent.owner_entry(&roster), Some(entry));
        assert!(intent.matches_process(&who, true, None));
        assert!(intent.matches_process(&stored, true, None));
        assert!(!intent.matches_process(&elsewhere, true, None));
        assert!(!intent.matches_process(&outside, true, None));
        assert!(!intent.matches_process(&who, false, None));
        // A stored intent admits the row's token only where the owner's entry is given.
        let retained = capture_execution(&seat, Cli::Claude, &stored).unwrap();
        assert!(matches!(retained, ExecutionIntent::Stored { .. }));
        let owner = retained.owner_entry(&roster);
        assert_eq!(owner, Some(entry));
        assert!(retained.matches_process(&who, true, owner));
        assert!(!retained.matches_process(&who, true, None));
        assert!(!retained.matches_process(&elsewhere, true, owner));
        // A marker naming another account's entry is no launch on this one.
        assert!(capture_execution(&seat, Cli::Claude, &elsewhere).is_err());
    });
}

#[test]
fn a_token_intent_is_refused_on_a_held_row_a_lease_elsewhere_and_an_off_clock() {
    crate::harness::journal::with_keyer(token_keyer, || {
        let refusal = |world: &World, intent: &ExecutionIntent| match admit_execution(
            &world.seat(),
            Cli::Claude,
            intent,
        ) {
            Ok(_) => panic!("admitted: {intent:?}"),
            Err(why) => why,
        };
        let (world, who, _) = token_fixture();
        let intent = capture_execution(&world.seat(), Cli::Claude, &who).unwrap();
        // This scratch Mac's vault holds no token: the standing refuses it last of all.
        assert!(refusal(&world, &intent).contains("execution token"));
        held(&world, true);
        assert!(refusal(&world, &intent).contains("held by another machine"));

        let (world, who, _) = token_fixture();
        let intent = capture_execution(&world.seat(), Cli::Claude, &who).unwrap();
        world
            .seat()
            .store(Cli::Claude)
            .transact(NOW, &mut None, |state| {
                state.note_lease("work", "m7", NOW + 3600, NOW);
                Ok(Vec::new())
            })
            .unwrap();
        let why = refusal(&world, &intent);
        assert!(
            why.contains(accounts::WhyNot::LeasedElsewhere { until: NOW + 3600 }.as_str()),
            "{why}"
        );

        let (world, who, _) = token_fixture();
        let intent = capture_execution(&world.seat(), Cli::Claude, &who).unwrap();
        world
            .seat()
            .store(Cli::Claude)
            .transact(NOW, &mut None, |state| {
                state.home_accounts.remove("work");
                state.clock = Some(accounts::Clock {
                    standing: accounts::ClockStanding::Off { ahead: true },
                    by_s: 600,
                    against: 2,
                    of: 3,
                    peers: Vec::new(),
                    until: NOW + 3600,
                });
                Ok(Vec::new())
            })
            .unwrap();
        let why = refusal(&world, &intent);
        assert!(why.contains(accounts::WhyNot::ClockOff.as_str()), "{why}");
    });
}
