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
        stored_login: true,
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
        stored_login: true,
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
    assert!(intent.matches_process(&alternate, true));
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
    assert!(intent.matches_process(&who, true));
    assert!(!intent.matches_process(&who, false));
    let mut different = who.clone();
    different.login = None;
    assert!(!intent.matches_process(&different, true));
    different.stored_login = false;
    assert!(!intent.matches_process(&different, true));
    assert!(ExecutionIntent::external().matches_process(&different, true));
    assert!(
        !ExecutionIntent::external().matches_process(&who, true),
        "a vanished external override cannot fall back onto the pool"
    );
    assert!(ExecutionIntent::external().matches_process(&who, false));
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
