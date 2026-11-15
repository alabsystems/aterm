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
        .then(|| capture_execution(&world.seat(), Cli::Claude, &who, Purpose::Select).unwrap());
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
            capture_execution(&world.seat(), Cli::Claude, &who, Purpose::Select).unwrap();
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
    let intent = capture_execution(&world.seat(), Cli::Claude, &who, Purpose::Select).unwrap();
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
    let intent = capture_execution(&seat, Cli::Claude, &who, Purpose::Select).unwrap();
    assert!(admit_execution(&seat, Cli::Claude, &intent).is_ok());
    held(&world, true);
    assert!(admit_execution(&seat, Cli::Claude, &intent).is_err());
    held(&world, false);
    assert!(admit_execution(&seat, Cli::Claude, &intent).is_ok());
    assert_eq!(
        capture_execution(&seat, Cli::Claude, &who, Purpose::Select).unwrap(),
        intent
    );
}

#[test]
fn renewing_even_in_the_same_directory_invalidates_retained_execution_intent() {
    let (world, who) = fixture();
    let seat = world.seat();
    let intent = capture_execution(&seat, Cli::Claude, &who, Purpose::Select).unwrap();
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
    let renewed = capture_execution(&seat, Cli::Claude, &who, Purpose::Select).unwrap();
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
        let intent = capture_execution(&world.seat(), Cli::Claude, &who, Purpose::Select).unwrap();
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
        let current = capture_execution(&world.seat(), Cli::Claude, &who, Purpose::Select).unwrap();
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
    let intent = capture_execution(&world.seat(), Cli::Claude, &who, Purpose::Select).unwrap();
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
    let intent = capture_execution(&world.seat(), Cli::Claude, &who, Purpose::Select).unwrap();
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
    let intent = capture_execution(&world.seat(), Cli::Claude, &who, Purpose::Select).unwrap();
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
        capture_execution(&seat, Cli::Claude, &alternate, Purpose::Select).unwrap(),
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
    let intent = capture_execution(&seat, Cli::Claude, &who, Purpose::Select).unwrap();
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
    let intent = capture_execution(&world.seat(), Cli::Claude, &who, Purpose::Select).unwrap();
    let config = world.cfg.join("aterm.toml");
    aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o664)).unwrap();
    assert!(capture_execution(&world.seat(), Cli::Claude, &who, Purpose::Select).is_err());
    assert!(admit_execution(&world.seat(), Cli::Claude, &intent).is_err());
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(admit_execution(&world.seat(), Cli::Claude, &intent).is_ok());
}

#[test]
fn execution_environment_must_keep_the_captured_route_and_login() {
    let (world, who) = fixture();
    let intent = capture_execution(&world.seat(), Cli::Claude, &who, Purpose::Select).unwrap();
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
    let intent = capture_execution(&world.seat(), Cli::Claude, &who, Purpose::Select).unwrap();
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
    let intent = capture_execution(&seat, Cli::Claude, &who, Purpose::Select).unwrap();
    let other = World::new("another-execution-namespace");
    assert!(admit_execution(&other.seat(), Cli::Claude, &intent).is_err());
    let unowned = accounts::ProcessLogin {
        login: Some(world.root.join("unowned").display().to_string()),
        ..who
    };
    let before = capture_execution(&seat, Cli::Claude, &unowned, Purpose::Select).unwrap();
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
    let intent = capture_execution(&world.seat(), Cli::Claude, &who, Purpose::Select).unwrap();
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
        let intent = capture_execution(&seat, Cli::Claude, &who, Purpose::Select).unwrap();
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
        let retained = capture_execution(&seat, Cli::Claude, &stored, Purpose::Select).unwrap();
        assert!(matches!(retained, ExecutionIntent::Stored { .. }));
        let owner = retained.owner_entry(&roster);
        assert_eq!(owner, Some(entry));
        assert!(retained.matches_process(&who, true, owner));
        assert!(!retained.matches_process(&who, true, None));
        assert!(!retained.matches_process(&elsewhere, true, owner));
        // A marker naming another account's entry is no launch on this one.
        assert!(capture_execution(&seat, Cli::Claude, &elsewhere, Purpose::Select).is_err());
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
        let intent = capture_execution(&world.seat(), Cli::Claude, &who, Purpose::Select).unwrap();
        // This scratch Mac's vault holds no token: the standing refuses it last of all.
        assert!(refusal(&world, &intent).contains("execution token"));
        held(&world, true);
        assert!(refusal(&world, &intent).contains("held by another machine"));

        let (world, who, _) = token_fixture();
        let intent = capture_execution(&world.seat(), Cli::Claude, &who, Purpose::Select).unwrap();
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
        let intent = capture_execution(&world.seat(), Cli::Claude, &who, Purpose::Select).unwrap();
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

/// A TOKEN INTENT IS JUDGED BY THIS MAC'S FACTS OF THE TOKEN THE VAULT HOLDS
/// (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.6): this scratch Mac's vault holds `work`'s
/// token, made here, whose windows were found to differ here, and a `--replace` of it waits in
/// this Mac's vault writes, so the table's row for the account is the waiting token's and the
/// vault's token's facts are its kept row. Admission refuses the intent, as the plan and the
/// wrapper refuse to serve it (`Table::row_for`). CONTROL: with no kept row the same token is
/// judged by the waiting token's row (awaiting its basis, in use on the Mac that made it) and
/// admitted. NEGATIVE CONTROL (applied to `token_in_use_here`, then reverted): judged by
/// `table.rows.get(..)`, the intent is admitted with the kept row in place.
#[cfg(unix)]
#[test]
fn a_token_intent_is_judged_by_this_macs_facts_of_the_vaults_token() {
    use crate::harness::accounts_tokens::{
        self as tokens, InVault, TokenRow, UsedOn, Verdict, Verification, tests::Lab,
    };
    use aterm_secrets::claude_token::{self, ExpiryFrom, Key, Meta};
    use aterm_secrets::client::{MemberWritten, OnBusy};
    crate::harness::journal::with_keyer(token_keyer, || {
        let (world, who, entry) = token_fixture();
        let lab = Lab::in_home(&world.home);
        let key = Key::parse(entry.key().as_str()).expect("a key");
        let now = u64::try_from(NOW).unwrap();
        let minted_at = now - 86_400;
        let meta = Meta {
            email: "work@example.com".to_owned(),
            account_id: "c-0123456789ab".to_owned(),
            minted_at,
            expires_at: now + 300 * 86_400,
            expiry_from: ExpiryFrom::Printed,
            minted_on: "m22".to_owned(),
            cli: "2.1.318".to_owned(),
            minted_by: None,
            verified: None,
        };
        let written = claude_token::write(
            &lab.paths,
            &key,
            &aterm_secrets::SecretBytes::from(
                crate::harness::setup_token::tests::token().as_bytes(),
            ),
            &meta,
            OnBusy::Refuse(std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN),
        )
        .expect("the write");
        assert!(
            matches!(written, MemberWritten::Posted | MemberWritten::SignedHere),
            "{written:?}"
        );
        let row = |minted_at: u64| TokenRow {
            key: key.as_str().to_owned(),
            email: meta.email.clone(),
            account_id: Some(meta.account_id.clone()),
            label: Some("work".to_owned()),
            entry: key.entry(),
            minted_at: Some(minted_at),
            expires_at: Some(meta.expires_at),
            expiry_from: Some("printed".to_owned()),
            minted_on: Some("m22".to_owned()),
            signer: Some("m22".to_owned()),
            used_on: UsedOn::default(),
            vault: InVault::InVault,
            verified: None,
            verification: Verification::default(),
            removed_at: None,
        };
        let store = world.seat().store(Cli::Claude);
        tokens::update_table(&store, |table| {
            table.rows.insert(
                key.as_str().to_owned(),
                TokenRow {
                    vault: InVault::Waiting("it waits for the vault's new key".to_owned()),
                    ..row(minted_at + 600)
                },
            );
        })
        .expect("the waiting replacement's row");
        let intent = capture_execution(&world.seat(), Cli::Claude, &who, Purpose::Select).unwrap();
        assert!(
            admit_execution(&world.seat(), Cli::Claude, &intent).is_ok(),
            "CONTROL: judged by the waiting token's row, the vault's token would be admitted"
        );
        tokens::update_table(&store, |table| {
            table.replaced.insert(
                key.as_str().to_owned(),
                TokenRow {
                    verification: Verification {
                        verdict: Some(Verdict::Differ),
                        ..Verification::default()
                    },
                    ..row(minted_at)
                },
            );
        })
        .expect("the vault's token's kept row");
        match admit_execution(&world.seat(), Cli::Claude, &intent) {
            Ok(_) => panic!("admitted a token whose windows differ here"),
            Err(why) => assert!(
                why.contains("execution token is not in use here: unverified (windows differ)"),
                "{why}"
            ),
        }
    });
}

/// A roster whose `m16` row is the default home's login (`dir` unset), with a second row
/// `work-2` on its own directory, both home accounts here, `m16`'s last word on record signed
/// out; and the process on `m16`'s login (the default home, no login selector).
fn signed_out_default() -> (World, accounts::ProcessLogin) {
    let world = World::new("execution-same-login");
    let work = world.work_dir(Cli::Claude);
    std::fs::write(
        world.cfg.join(Cli::Claude.accounts().roster_file),
        format!(
            "[[account]]\nlabel = \"m16\"\nclass = \"dedicated\"\n\n[[account]]\n\
             label = \"work-2\"\nclass = \"dedicated\"\ndir = {}\n",
            toml(&work)
        ),
    )
    .unwrap();
    world
        .seat()
        .store(Cli::Claude)
        .transact(NOW, &mut None, |state| {
            state.home_accounts.extend(["m16".into(), "work-2".into()]);
            accounts::note_auth(state, "m16", accounts::Auth::SignedOut, NOW);
            Ok(Vec::new())
        })
        .unwrap();
    let who = accounts::ProcessLogin {
        login: None,
        default_home: true,
        route: accounts::AuthRoute::Stored,
    };
    (world, who)
}

/// The refusal `result` holds, by kind.
fn refused_kind<T: std::fmt::Debug>(result: Result<T, Refusal>) -> RefusalKind {
    match result {
        Ok(admitted) => panic!("admitted: {admitted:?}"),
        Err(refusal) => refusal.kind,
    }
}

/// A SAME-LOGIN RELAUNCH CHECKS INTEGRITY, NOT SELECTION POLICY (the five Claude Code tabs left
/// at `upgrade=pending/…/account`): the process runs on the default home's login, which roster
/// row `m16` owns and whose recorded word reads signed out. A capture for the same login admits
/// it, at the plan and again at exec, and names the row it runs on. NEGATIVE CONTROL: a selection
/// on the very same fixture refuses it as signed out, so the fixture reads signed out and a new
/// run onto the row stays guarded.
#[test]
fn a_same_login_capture_admits_a_row_recorded_signed_out_that_a_selection_refuses() {
    let (world, who) = signed_out_default();
    let seat = world.seat();
    let roster = seat.roster(Cli::Claude).unwrap();
    let state = world.load(Cli::Claude);
    assert_eq!(
        accounts::condition(roster.row("m16").unwrap(), &state, true, NOW),
        accounts::Condition::SignedOut,
        "the fixture's row reads signed out"
    );
    let intent = capture_execution(&seat, Cli::Claude, &who, Purpose::SameLogin).unwrap();
    let ExecutionIntent::Stored { owner, purpose, .. } = &intent else {
        panic!("a stored intent: {intent:?}");
    };
    assert_eq!(
        (owner.as_deref(), *purpose),
        (Some("m16"), Purpose::SameLogin)
    );
    assert_eq!(intent.purpose(), Some(Purpose::SameLogin));
    assert!(execution_admission(&seat, Cli::Claude, &intent).is_ok());
    assert!(admit_execution(&seat, Cli::Claude, &intent).is_ok());
    let refusal = capture_execution(&seat, Cli::Claude, &who, Purpose::Select).unwrap_err();
    assert_eq!(refusal.kind, RefusalKind::SignedOut, "{refusal:?}");
    assert!(refusal.kind.policy());
    assert_eq!(refusal.why, "execution account is not signed in");
}

/// A STORED-LOGIN INTENT ONTO A ROW ONLY ITS VAULT TOKEN KEEPS IN ROTATION IS REFUSED AS SIGNED
/// OUT: `m16`'s stored login reads signed out here and a fresh reading of the vault names it as on
/// its token, so rotation may pick it ([`accounts::LoginHere::OnToken`]). Where the token route
/// then sends the session to the stored login (a program that is no atpkg store build, a vault
/// read that was busy, a refusal since the reading), the intent is a stored one, and it is refused
/// as the selection refuses any signed-out row: Claude Code is never relaunched on the lapsed
/// login. NEGATIVE CONTROL: the reading makes [`accounts::condition`] read the row runnable, the
/// judgment that admitted such an intent.
#[test]
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "a launch starts on a vault token only on macOS"
)]
fn a_stored_intent_onto_a_row_only_its_token_keeps_in_rotation_is_refused_as_signed_out() {
    let (world, who) = signed_out_default();
    let seat = world.seat();
    let read: std::collections::BTreeMap<String, u64> =
        [("m16".to_owned(), u64::try_from(NOW + 86_400).unwrap())].into();
    assert!(
        seat.store(Cli::Claude)
            .note_tokens_here(Some(&read), NOW)
            .unwrap()
    );
    let roster = seat.roster(Cli::Claude).unwrap();
    let (state, _) = seat.store(Cli::Claude).load_judged(NOW, &roster).unwrap();
    let row = roster.row("m16").unwrap();
    assert_eq!(
        accounts::login_here(row, &state, true, NOW),
        accounts::LoginHere::OnToken
    );
    assert!(
        accounts::condition(row, &state, true, NOW).runs(),
        "NEGATIVE CONTROL: on its token, the row runs"
    );
    assert_eq!(
        accounts::stored_login_condition(row, &state, true, NOW),
        accounts::Condition::SignedOut
    );
    let refusal = capture_execution(&seat, Cli::Claude, &who, Purpose::Select).unwrap_err();
    assert_eq!(refusal.kind, RefusalKind::SignedOut, "{refusal:?}");
    assert_eq!(refusal.why, "execution account is not signed in");
}

/// A COLD RELAUNCH NEVER KEEPS OR TAKES A ROW ITS VAULT TOKEN WOULD NOT SERVE (L1's launch-time
/// token guard): `m16`'s stored login reads signed out here and a fresh reading of the vault names
/// it as on its token, so rotation reads it runnable, but this Mac holds no vault, so the token
/// judgment serves no row. The session that ran on `m16` is relaunched on `work-2`; with
/// `work-2` signed out too, the relaunch is refused; and with rotation off, the relaunch that
/// keeps the row it ran on is refused as signed out. None of them starts Claude Code on `m16`'s
/// lapsed stored login. NEGATIVE CONTROL: rotation's own judgment reads `m16` runnable, the
/// judgment that kept such a row.
#[test]
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "a launch starts on a vault token only on macOS"
)]
fn a_cold_relaunch_never_keeps_or_takes_a_row_its_token_would_not_serve() {
    let (world, _) = signed_out_default();
    let seat = world.seat();
    let read: std::collections::BTreeMap<String, u64> =
        [("m16".to_owned(), u64::try_from(NOW + 86_400).unwrap())].into();
    assert!(
        seat.store(Cli::Claude)
            .note_tokens_here(Some(&read), NOW)
            .unwrap()
    );
    let roster = seat.roster(Cli::Claude).unwrap();
    let row = roster.row("m16").unwrap();
    let enrolled = accounts::enrolled_in(Cli::Claude, &roster, &world.home);
    let (state, _) = seat.store(Cli::Claude).load_judged(NOW, &roster).unwrap();
    assert!(
        accounts::condition(row, &state, enrolled.contains("m16"), NOW).runs(),
        "NEGATIVE CONTROL: on its token, the row runs"
    );
    let relaunch = || super::super::cold_launch_env(&seat, Cli::Claude, &[], &no_usage, None);
    let moved = relaunch().expect("admitted").expect("a move");
    assert_eq!(moved.label, "work-2", "{moved:?}");

    seat.store(Cli::Claude)
        .transact(NOW, &mut None, |state| {
            Ok(accounts::note_auth(
                state,
                "work-2",
                accounts::Auth::SignedOut,
                NOW,
            ))
        })
        .unwrap();
    let refused = relaunch().expect_err("nothing serves");
    assert!(refused.contains("no usable account"), "{refused}");

    aterm_uds::atomic_file::write(
        &world.cfg.join("aterm.toml"),
        "[harness]\naccounts='off'\n".as_ref(),
        0o600,
    )
    .unwrap();
    assert_eq!(
        relaunch().expect_err("kept on a token that would not serve it"),
        "cold relaunch account is not signed in"
    );
}

/// A SAME-LOGIN INTENT KEEPS EVERY INTEGRITY CHECK: a renewal between capture and admission
/// changes the credential generation and refuses it; a profile naming another account refuses its
/// capture; and an intent's purpose is part of it, so a selection's copy of a same-login intent
/// is held to the selection policy at exec and refused there. NEGATIVE CONTROL: the same intents
/// admit before each change.
#[test]
fn a_same_login_intent_keeps_generation_identity_and_its_purpose() {
    let (world, who) = fixture();
    let seat = world.seat();
    let intent = capture_execution(&seat, Cli::Claude, &who, Purpose::SameLogin).unwrap();
    assert!(execution_admission(&seat, Cli::Claude, &intent).is_ok());
    renew(&world, &who);
    assert_eq!(
        refused_kind(execution_admission(&seat, Cli::Claude, &intent)),
        RefusalKind::Changing,
        "a renewal is a new credential generation, whatever the purpose"
    );

    let (world, who) = fixture();
    let seat = world.seat();
    assert!(capture_execution(&seat, Cli::Claude, &who, Purpose::SameLogin).is_ok());
    let mut roster = seat.roster(Cli::Claude).unwrap();
    roster
        .rows
        .iter_mut()
        .find(|row| row.label == "work")
        .unwrap()
        .email = Some("work@example.com".into());
    std::fs::write(
        world.cfg.join(Cli::Claude.accounts().roster_file),
        accounts::append_rows_for(Cli::Claude, "", &roster.rows).unwrap(),
    )
    .unwrap();
    assert!(capture_execution(&seat, Cli::Claude, &who, Purpose::SameLogin).is_ok());
    std::fs::write(
        Path::new(who.login.as_deref().unwrap()).join(".claude.json"),
        aterm_json::to_vec(&aterm_json::json!({
            "oauthAccount": {"emailAddress": "other@example.com"}
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        refused_kind(capture_execution(
            &seat,
            Cli::Claude,
            &who,
            Purpose::SameLogin
        )),
        RefusalKind::Integrity,
        "a login whose profile names another account is no same login"
    );

    let (world, who) = signed_out_default();
    let seat = world.seat();
    let same = capture_execution(&seat, Cli::Claude, &who, Purpose::SameLogin).unwrap();
    let ExecutionIntent::Stored {
        namespace,
        login,
        owner,
        generation,
        ..
    } = same.clone()
    else {
        panic!("a stored intent: {same:?}");
    };
    let select = ExecutionIntent::Stored {
        namespace,
        login,
        owner,
        generation,
        purpose: Purpose::Select,
    };
    assert_ne!(select, same);
    assert!(execution_admission(&seat, Cli::Claude, &same).is_ok());
    assert_eq!(
        refused_kind(execution_admission(&seat, Cli::Claude, &select)),
        RefusalKind::SignedOut,
        "admission applies the intent's own purpose"
    );
    let (world, who) = fixture();
    assert_ne!(
        capture_execution(&world.seat(), Cli::Claude, &who, Purpose::SameLogin).unwrap(),
        capture_execution(&world.seat(), Cli::Claude, &who, Purpose::Select).unwrap(),
        "a capture for one purpose never stands for the other's"
    );
}

/// A SAME-LOGIN INTENT SKIPS THE SELECTION POLICY, NEVER CUSTODY, CLAIMS OR THE WRITER: another
/// Mac's hold on the row, another Mac's senior claim and a credential held for preparation only
/// refuse a same-login intent as they refuse a selection's (the turn-end advice makes a session
/// they refuse yield, so no relaunch starts a new turn on it), each on its own kind, which no
/// purpose skips; the owner's pin on another row and a usage limit refuse only the selection's;
/// and a held writer refuses both as a lock. CONTROL: both intents admit once each custody or
/// claims change is undone, so each refusal is that change's.
///
/// NEGATIVE CONTROL (2026-11-13, restored with cp from a backup, confirmed with cmp): with
/// custody and claims checked for a selection alone, the same-login intent on the held row
/// admits.
#[test]
fn a_same_login_intent_skips_selection_policy_but_not_custody_claims_or_the_writer() {
    let (world, who) = fixture();
    let seat = world.seat();
    let same = capture_execution(&seat, Cli::Claude, &who, Purpose::SameLogin).unwrap();
    let select = capture_execution(&seat, Cli::Claude, &who, Purpose::Select).unwrap();
    let store = seat.store(Cli::Claude);
    let both = |kind: RefusalKind, what: &str| {
        for intent in [&same, &select] {
            let refusal = execution_admission(&seat, Cli::Claude, intent).unwrap_err();
            assert_eq!(refusal.kind, kind, "{what}: {intent:?}: {refusal:?}");
            assert!(!refusal.kind.policy(), "{what}: every purpose checks it");
            assert_eq!(
                refused_kind(capture_execution(
                    &seat,
                    Cli::Claude,
                    &who,
                    intent.purpose().unwrap()
                )),
                kind,
                "{what}: the capture refuses it too"
            );
        }
    };
    let neither = |what: &str| {
        for intent in [&same, &select] {
            assert!(
                execution_admission(&seat, Cli::Claude, intent).is_ok(),
                "{what}: {intent:?}"
            );
        }
    };
    held(&world, true);
    both(RefusalKind::Held, "another Mac's hold");
    held(&world, false);
    neither("control: the hold lifted");
    store
        .transact(NOW, &mut None, |state| {
            state.home_accounts.remove("work");
            state.note_lease("work", "m7", NOW + 3600, NOW);
            Ok(Vec::new())
        })
        .unwrap();
    both(RefusalKind::Lease, "another Mac's senior claim");
    store
        .transact(NOW, &mut None, |state| {
            state.leases.clear();
            state.home_accounts.insert("work".into());
            state.prepared_accounts.insert("work".into());
            Ok(Vec::new())
        })
        .unwrap();
    both(RefusalKind::Lease, "a credential held for preparation only");
    store
        .transact(NOW, &mut None, |state| {
            state.prepared_accounts.clear();
            Ok(Vec::new())
        })
        .unwrap();
    neither("control: the claims admit this Mac");
    store
        .transact(NOW, &mut None, |state| {
            state.pinned = true;
            state.active = Some("main".into());
            Ok(Vec::new())
        })
        .unwrap();
    assert_eq!(
        refused_kind(execution_admission(&seat, Cli::Claude, &select)),
        RefusalKind::Pin
    );
    assert!(execution_admission(&seat, Cli::Claude, &same).is_ok());
    store
        .transact(NOW, &mut None, |state| {
            state.pinned = false;
            state.active = None;
            Ok(accounts::note_wall(
                state,
                "work",
                "weekly",
                Some(NOW + HOUR),
                NOW,
                NOW,
            ))
        })
        .unwrap();
    assert_eq!(
        refused_kind(execution_admission(&seat, Cli::Claude, &select)),
        RefusalKind::Limited
    );
    let guard = execution_admission(&seat, Cli::Claude, &same).unwrap();
    assert_eq!(
        refused_kind(execution_admission(&seat, Cli::Claude, &same)),
        RefusalKind::Lock,
        "the writer still excludes a second admission"
    );
    drop(guard);
}

/// A same-login intent persists with its purpose and reads back as itself; a record written
/// before purposes (none named) reads as a selection's, the stricter; any other purpose word, or
/// a selection's purpose spelled out, conveys no authority.
#[test]
fn a_records_purpose_round_trips_and_a_record_naming_none_reads_as_a_selection() {
    let (world, who) = fixture();
    let same = capture_execution(&world.seat(), Cli::Claude, &who, Purpose::SameLogin).unwrap();
    let value = same.to_value();
    assert_eq!(value["purpose"].as_str(), Some("same-login"));
    assert_eq!(ExecutionIntent::from_value(&value), Some(same.clone()));
    let mut legacy = value.clone();
    legacy.as_object_mut().unwrap().remove("purpose");
    let read = ExecutionIntent::from_value(&legacy).unwrap();
    assert_eq!(read.purpose(), Some(Purpose::Select));
    assert_eq!(
        read,
        capture_execution(&world.seat(), Cli::Claude, &who, Purpose::Select).unwrap()
    );
    assert_eq!(
        read.to_value(),
        legacy,
        "a selection's record names no purpose"
    );
    for word in ["select", "SameLogin", ""] {
        let mut other = value.clone();
        other
            .as_object_mut()
            .unwrap()
            .insert("purpose".into(), aterm_json::json!(word));
        assert_eq!(ExecutionIntent::from_value(&other), None, "{word}");
    }
}

/// A PIN'S REFUSAL NAMES THE CHECK THAT REFUSED, NOT ALWAYS THE PIN: a selection under the
/// owner's pin on the login's own row `m16`, which reads signed out, waits as signed out (the pin
/// names this very account); the same pin while `m16` is held for preparation only waits on the
/// lease; a pin on another row waits on the pin whatever that row's standing, signed out
/// included, as does a pin that names no row. NEGATIVE CONTROL: with no pin, the selection on
/// `m16` refuses as signed out too, and a same-login intent under each pin admits.
#[test]
fn a_pins_refusal_names_the_check_that_refused() {
    let (world, who) = signed_out_default();
    let seat = world.seat();
    let store = seat.store(Cli::Claude);
    let select = || capture_execution(&seat, Cli::Claude, &who, Purpose::Select);
    let same = capture_execution(&seat, Cli::Claude, &who, Purpose::SameLogin).unwrap();
    assert_eq!(refused_kind(select()), RefusalKind::SignedOut, "control");
    let pin = |label: &str| {
        let label = label.to_owned();
        store
            .transact(NOW, &mut None, move |state| {
                state.pinned = true;
                state.active = Some(label);
                Ok(Vec::new())
            })
            .unwrap();
    };
    pin("m16");
    let refusal = select().unwrap_err();
    assert_eq!(refusal.kind, RefusalKind::SignedOut, "{refusal:?}");
    assert!(refusal.why.contains("m16 is not signed in"), "{refusal:?}");
    assert!(execution_admission(&seat, Cli::Claude, &same).is_ok());
    store
        .transact(NOW, &mut None, |state| {
            state.prepared_accounts.insert("m16".into());
            Ok(Vec::new())
        })
        .unwrap();
    assert_eq!(refused_kind(select()), RefusalKind::Lease);
    store
        .transact(NOW, &mut None, |state| {
            state.prepared_accounts.clear();
            accounts::note_auth(state, "work-2", accounts::Auth::SignedOut, NOW);
            Ok(Vec::new())
        })
        .unwrap();
    pin("work-2");
    let refusal = select().unwrap_err();
    assert_eq!(refusal.kind, RefusalKind::Pin, "{refusal:?}");
    assert!(
        refusal.why.contains("work-2 is not signed in"),
        "{refusal:?}"
    );
    assert!(execution_admission(&seat, Cli::Claude, &same).is_ok());
    pin("ghost");
    assert_eq!(refused_kind(select()), RefusalKind::Pin);
    assert!(execution_admission(&seat, Cli::Claude, &same).is_ok());
}

/// A TOKEN INTENT'S REFUSAL NAMES WHAT LIFTS IT, WHATEVER ITS PURPOSE: a seated vault that holds
/// no token for `work` refuses its token intent on the token (a new one lifts it), never as a
/// login filed under another account; a vault that cannot be read (this scratch home has none)
/// and a token table that does not read are records aterm cannot read yet. NEGATIVE CONTROL:
/// each intent's capture admits, so no refusal is the route's own integrity.
#[cfg(unix)]
#[test]
fn a_token_intents_refusal_names_what_lifts_it() {
    crate::harness::journal::with_keyer(token_keyer, || {
        let (world, who, _) = token_fixture();
        let lab = crate::harness::accounts_tokens::tests::Lab::new();
        let seated = Seat {
            home: &lab.paths.home,
            ..world.seat()
        };
        for purpose in [Purpose::SameLogin, Purpose::Select] {
            let intent = capture_execution(&seated, Cli::Claude, &who, purpose).unwrap();
            let refusal = execution_admission(&seated, Cli::Claude, &intent).unwrap_err();
            assert_eq!(refusal.kind, RefusalKind::Token, "{purpose:?}: {refusal:?}");
            assert!(refusal.why.contains("not in use here: none"), "{refusal:?}");
            assert!(!refusal.kind.policy());
        }
        let seat = world.seat();
        let intent = capture_execution(&seat, Cli::Claude, &who, Purpose::SameLogin).unwrap();
        let refusal = execution_admission(&seat, Cli::Claude, &intent).unwrap_err();
        assert_eq!(refusal.kind, RefusalKind::Unread, "{refusal:?}");
        assert!(refusal.why.contains("could not be read"), "{refusal:?}");
        let store = seat.store(Cli::Claude);
        aterm_uds::private_file::write_private(
            &store.shared_dir(),
            crate::harness::accounts_tokens::TABLE_FILE,
            b"not a table",
        )
        .unwrap();
        let refusal = execution_admission(&seat, Cli::Claude, &intent).unwrap_err();
        assert_eq!(refusal.kind, RefusalKind::Unread, "{refusal:?}");
        assert!(refusal.why.contains("execution token table"), "{refusal:?}");
    });
}

/// A READMISSION TAKES A NEW CREDENTIAL GENERATION FOR THE SAME LOGIN, NEVER ANOTHER OWNER (an
/// upgrade's relaunch whose agent ended while a renewal landed, left at its shell for good): a
/// renewal of the login after its intent was captured refuses that intent's admission as a change
/// (CONTROL: the generation moved), while its readmission admits the login captured afresh for
/// the same purpose, which names the same namespace, selector, owner and purpose under a new
/// generation and is itself admitted. An unchanged intent readmits as itself. A selection's intent
/// readmits only past its policy. A login filed under another row since (the row renamed) is
/// moved, which no later look admits; a held writer is a refusal that lifts, never a move.
///
/// NEGATIVE CONTROL (2026-11-13, restored with cp from a backup, confirmed with cmp): with the
/// fresh capture never taken in place, the renewed same-login intent reads as moved.
#[test]
fn a_readmission_takes_a_new_generation_for_the_same_login_and_never_another_owner() {
    let (world, who) = fixture();
    let seat = world.seat();
    let same = capture_execution(&seat, Cli::Claude, &who, Purpose::SameLogin).unwrap();
    let select = capture_execution(&seat, Cli::Claude, &who, Purpose::Select).unwrap();
    let (guard, renewed) = execution_readmission(&seat, Cli::Claude, &same).unwrap();
    assert_eq!(renewed, None, "an unchanged intent readmits as itself");
    drop(guard);
    renew(&world, &who);
    assert_eq!(
        refused_kind(execution_admission(&seat, Cli::Claude, &same)),
        RefusalKind::Changing,
        "control: the renewal moved the credential generation"
    );
    for retained in [&same, &select] {
        let (guard, renewed) = execution_readmission(&seat, Cli::Claude, retained).unwrap();
        drop(guard);
        let renewed = renewed.expect("the intent captured afresh");
        let (
            ExecutionIntent::Stored {
                namespace,
                login,
                owner,
                generation,
                purpose,
            },
            ExecutionIntent::Stored {
                namespace: was_namespace,
                login: was_login,
                owner: was_owner,
                generation: was_generation,
                purpose: was_purpose,
            },
        ) = (&renewed, retained)
        else {
            panic!("stored intents: {renewed:?} {retained:?}");
        };
        assert_eq!(
            (namespace, login, owner, purpose),
            (was_namespace, was_login, was_owner, was_purpose)
        );
        assert_ne!(generation, was_generation, "a new generation");
        assert!(execution_admission(&seat, Cli::Claude, &renewed).is_ok());
    }
    held(&world, true);
    assert!(matches!(
        execution_readmission(&seat, Cli::Claude, &same),
        Err(Unadmitted::Refused(Refusal {
            kind: RefusalKind::Held,
            ..
        }))
    ));
    held(&world, false);
    let writer = seat
        .store(Cli::Claude)
        .acquire_within(&[])
        .unwrap()
        .unwrap();
    assert!(matches!(
        execution_readmission(&seat, Cli::Claude, &same),
        Err(Unadmitted::Refused(Refusal {
            kind: RefusalKind::Lock,
            ..
        }))
    ));
    drop(writer);
    let mut roster = seat.roster(Cli::Claude).unwrap();
    roster
        .rows
        .iter_mut()
        .find(|row| row.label == "work")
        .unwrap()
        .label = "work-b".into();
    std::fs::write(
        world.cfg.join(Cli::Claude.accounts().roster_file),
        accounts::append_rows_for(Cli::Claude, "", &roster.rows).unwrap(),
    )
    .unwrap();
    match execution_readmission(&seat, Cli::Claude, &same) {
        Err(Unadmitted::Moved(refusal)) => {
            assert_eq!(refusal.kind, RefusalKind::Changing, "{refusal:?}");
            assert!(refusal.why.contains("another account"), "{refusal:?}");
        }
        Ok((_, renewed)) => panic!("a login under another row readmitted: {renewed:?}"),
        Err(other) => panic!("{other:?}"),
    }
    assert_eq!(
        refused_kind(execution_admission(&seat, Cli::Claude, &same)),
        RefusalKind::Changing
    );
}
