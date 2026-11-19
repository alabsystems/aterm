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
/// `work`'s token (`entry`) written to a scratch vault `world`'s home seats, minted on `m22` a
/// day before [`NOW`]: the vault's lab (its server lives as long as the lab does), the token's
/// key and its record.
#[cfg(unix)]
fn vault_token(
    world: &World,
    entry: accounts::TokenEntry,
) -> (
    crate::harness::accounts_tokens::tests::Lab,
    aterm_secrets::claude_token::Key,
    aterm_secrets::claude_token::Meta,
) {
    use aterm_secrets::claude_token::{self, ExpiryFrom, Key, Meta};
    use aterm_secrets::client::{MemberWritten, OnBusy};
    let lab = crate::harness::accounts_tokens::tests::Lab::in_home(&world.home);
    let key = Key::parse(entry.key().as_str()).expect("a key");
    let now = u64::try_from(NOW).unwrap();
    let meta = Meta {
        email: "work@example.com".to_owned(),
        account_id: "c-0123456789ab".to_owned(),
        minted_at: now - 86_400,
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
        &aterm_secrets::SecretBytes::from(crate::harness::setup_token::tests::token().as_bytes()),
        &meta,
        OnBusy::Refuse(std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN),
    )
    .expect("the write");
    assert!(
        matches!(written, MemberWritten::Posted | MemberWritten::SignedHere),
        "{written:?}"
    );
    (lab, key, meta)
}

/// The token table's row of `work`'s token `key` ([`vault_token`]'s `meta`), minted at
/// `minted_at`, in the vault.
#[cfg(unix)]
fn token_row(
    key: &aterm_secrets::claude_token::Key,
    meta: &aterm_secrets::claude_token::Meta,
    minted_at: u64,
) -> crate::harness::accounts_tokens::TokenRow {
    use crate::harness::accounts_tokens::{InVault, TokenRow, UsedOn, Verification};
    TokenRow {
        key: key.as_str().to_owned(),
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
    }
}

/// `work`'s token in use here ([`vault_token`]): the table's row for it is a replacement that
/// waits on its basis, in use on the Mac that made it, which admits the vault's token
/// ([`a_token_intent_is_judged_by_this_macs_facts_of_the_vaults_token`]'s control). Its lab
/// serves the vault while it lives.
#[cfg(unix)]
fn token_in_use(
    world: &World,
    entry: accounts::TokenEntry,
) -> (
    crate::harness::accounts_tokens::tests::Lab,
    aterm_secrets::claude_token::Key,
    aterm_secrets::claude_token::Meta,
) {
    use crate::harness::accounts_tokens::{self as tokens, InVault, TokenRow};
    let (lab, key, meta) = vault_token(world, entry);
    tokens::update_table(&world.seat().store(Cli::Claude), |table| {
        table.rows.insert(
            key.as_str().to_owned(),
            TokenRow {
                vault: InVault::Waiting("it waits for the vault's new key".to_owned()),
                ..token_row(&key, &meta, meta.minted_at + 600)
            },
        );
    })
    .expect("the waiting replacement's row");
    (lab, key, meta)
}

#[cfg(unix)]
#[test]
fn a_token_intent_is_judged_by_this_macs_facts_of_the_vaults_token() {
    use crate::harness::accounts_tokens::{self as tokens, TokenRow, Verdict, Verification};
    crate::harness::journal::with_keyer(token_keyer, || {
        let (world, who, entry) = token_fixture();
        let (_lab, key, meta) = token_in_use(&world, entry);
        let store = world.seat().store(Cli::Claude);
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
                    ..token_row(&key, &meta, meta.minted_at)
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

/// A login directory `name` of Claude Code's under `world`'s logins, holding a stand-in of the
/// file it keeps a login in, so the login is made here ([`accounts::login_made`]).
fn made_login(world: &World, name: &str) -> PathBuf {
    let dir = world.root.join("logins").join(Cli::Claude.id()).join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".credentials.json"), "{}").unwrap();
    dir
}

/// Row `label` bound to the login `dir`, leaving `own_dir` (`None`: the default home's), as a
/// renewal or a sign-in into a new directory records it ([`accounts::note_renewed`]).
fn renew_into(world: &World, label: &str, own_dir: Option<&Path>, dir: &Path) {
    world
        .seat()
        .store(Cli::Claude)
        .transact_logins(NOW, &mut None, |state, logins| {
            Ok(accounts::note_renewed(
                state,
                logins,
                label,
                own_dir.map(|dir| dir.to_str().unwrap()),
                dir.to_str().unwrap(),
                None,
                NOW,
            ))
        })
        .unwrap();
}

/// A process on its stored login at `login` (`None`: the default home's).
fn on(login: Option<&Path>) -> accounts::ProcessLogin {
    accounts::ProcessLogin {
        login: login.map(|login| login.display().to_string()),
        default_home: true,
        route: accounts::AuthRoute::Stored,
    }
}

/// A relaunch line's token judgment that serves no row ([`relaunch_rebind`]'s `serves`): a line
/// on the stored login, as one a native upgrade or a build outside atpkg's store types.
fn no_token(_: &Row) -> bool {
    false
}

/// `world`'s roster file rewritten from `rows`.
fn write_rows(world: &World, rows: &[Row]) {
    std::fs::write(
        world.cfg.join(Cli::Claude.accounts().roster_file),
        accounts::append_rows_for(Cli::Claude, "", rows).unwrap(),
    )
    .unwrap();
}

/// A Claude Code profile under `dir` that names `email`.
fn profile_naming(dir: &Path, email: &str) {
    std::fs::write(
        dir.join(".claude.json"),
        aterm_json::to_vec(&aterm_json::json!({"oauthAccount": {"emailAddress": email}})).unwrap(),
    )
    .unwrap();
}

/// AN IN-PLACE RELAUNCH OFF A LOGIN ITS OWN ROW LEFT MOVES ONTO THE ROW'S CURRENT LOGIN (the
/// upgrade held for good at `account-integrity` once its row was signed into a new login): row
/// `work` was renewed from `D1` into `D2` while the process still runs on `D1`. The relaunch starts
/// on `D2` ([`Rebind::Row`]), the very login a cold relaunch of the same process starts on, and
/// the capture of `D2` for a rebind is `work`'s: admitted at the plan and at exec, its record
/// naming the purpose `rebind`. The cold relaunch, which replaces no live process, captures the
/// same login for a selection. CONTROLS: `D1` itself is never run again: a same-login or a
/// selection's capture of it is refused as retained, in words that say so, not as an integrity
/// fault, and a rebind's capture of it, which no row holds, as a change under way; a sign-in
/// after the capture moves the credential generation and refuses the rebind's intent at exec.
#[test]
fn an_in_place_relaunch_off_a_login_its_own_row_left_moves_onto_the_rows_current_login() {
    let (world, who) = fixture();
    let seat = world.seat();
    let d1 = PathBuf::from(who.login.clone().unwrap());
    let d2 = made_login(&world, "work-renewed");
    renew_into(&world, "work", Some(&d1), &d2);
    let rebind = relaunch_rebind(&seat, &who, &mut no_token)
        .unwrap()
        .expect("a rebind");
    let Rebind::Row(current) = &rebind else {
        panic!("the row's own login: {rebind:?}");
    };
    assert_eq!(
        (current.label.as_str(), current.value.as_deref()),
        ("work", d2.to_str())
    );
    let cold = super::super::cold_launch_admitted(
        &seat,
        Cli::Claude,
        who.clone(),
        &NoUsage,
        Some(&world.exe),
    )
    .unwrap();
    assert_eq!(
        cold.pick.as_ref(),
        Some(current),
        "a cold relaunch starts on the same login"
    );
    assert_eq!(
        cold.intent,
        capture_execution(&seat, Cli::Claude, &on(Some(&d2)), Purpose::Select).unwrap(),
        "a cold relaunch selects the row's current login"
    );

    let intent = capture_execution(&seat, Cli::Claude, &on(Some(&d2)), Purpose::Rebind).unwrap();
    let ExecutionIntent::Stored { owner, purpose, .. } = &intent else {
        panic!("a stored intent: {intent:?}");
    };
    assert_eq!(
        (owner.as_deref(), *purpose),
        (Some("work"), Purpose::Rebind)
    );
    let value = intent.to_value();
    assert_eq!(value["purpose"].as_str(), Some("rebind"));
    assert_eq!(ExecutionIntent::from_value(&value), Some(intent.clone()));
    assert!(execution_admission(&seat, Cli::Claude, &intent).is_ok());
    assert!(admit_execution(&seat, Cli::Claude, &intent).is_ok());

    for purpose in [Purpose::SameLogin, Purpose::Select] {
        let refusal = capture_execution(&seat, Cli::Claude, &who, purpose).unwrap_err();
        assert_eq!(
            refusal.kind,
            RefusalKind::Retained,
            "{purpose:?}: {refusal:?}"
        );
        assert_eq!(
            refusal.why,
            "execution login is one its account left for a newer login"
        );
        assert_eq!(refusal.kind.word(), "account-retained");
        assert!(!refusal.kind.policy());
    }
    assert_eq!(
        refused_kind(capture_execution(&seat, Cli::Claude, &who, Purpose::Rebind)),
        RefusalKind::Changing
    );
    seat.store(Cli::Claude)
        .transact_logins(NOW, &mut None, |state, logins| {
            Ok(accounts::note_checked_sign_in(state, logins, "work", NOW))
        })
        .unwrap();
    assert_eq!(
        refused_kind(execution_admission(&seat, Cli::Claude, &intent)),
        RefusalKind::Changing,
        "a new credential generation"
    );
}

/// A COLD RELAUNCH OFF A LOGIN ITS ROW LEFT SELECTS: its agent already ended, so it starts a new
/// runner on the row's current login and no running credential stands for the account's pin or
/// `accounts = "off"` standing, as for the raw relaunch after its agent ended ([`relaunch`]'s
/// `purpose_of` with no live process). Row `work` left `D1` for `D2`: with the owner's pin on
/// `main`, the cold relaunch of the process on `D1` is refused in the very words a selection's
/// capture of `D2` is (`account-policy-pin`); with rotation off and no pin, likewise, for naming a
/// login of its own. CONTROL: an in-place rebind's capture of `D2` admits under either, its
/// account being the one the live process it replaces runs on. NEGATIVE CONTROL (applied to
/// `cold_launch_admitted`'s capture, then reverted): a cold start that captures
/// [`Purpose::Rebind`] is admitted past the pin.
#[test]
fn a_cold_relaunch_off_a_login_its_row_left_selects_so_a_pin_or_off_refuses_it() {
    let (world, who) = fixture();
    let seat = world.seat();
    let d1 = PathBuf::from(who.login.clone().unwrap());
    let d2 = made_login(&world, "work-renewed");
    renew_into(&world, "work", Some(&d1), &d2);
    let new = on(Some(&d2));
    let cold_refused = |select: &Refusal| {
        assert_eq!(select.kind, RefusalKind::Pin, "{select:?}");
        match super::super::cold_launch_admitted(
            &seat,
            Cli::Claude,
            who.clone(),
            &NoUsage,
            Some(&world.exe),
        ) {
            Ok(admitted) => panic!("a cold relaunch past the policy: {:?}", admitted.intent),
            Err(why) => assert_eq!(why, select.why),
        }
        assert!(capture_execution(&seat, Cli::Claude, &new, Purpose::Rebind).is_ok());
    };
    let store = seat.store(Cli::Claude);
    store
        .transact(NOW, &mut None, |state| {
            state.pinned = true;
            state.active = Some("main".into());
            Ok(Vec::new())
        })
        .unwrap();
    cold_refused(&capture_execution(&seat, Cli::Claude, &new, Purpose::Select).unwrap_err());
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
    cold_refused(&capture_execution(&seat, Cli::Claude, &new, Purpose::Select).unwrap_err());
}

/// A REBIND OFF THE DEFAULT LOGIN ASKS ITS NEW LOGIN'S SIGN-IN AND NO LIMIT (m36: the default
/// home's row `m16` signed into a login directory of its own while at its usage limit): the
/// process on the default home's login relaunches on `m16`'s new login, and a rebind's capture of
/// it admits although `m16` is at a wall. CONTROL: a selection's capture of the same login refuses
/// it as limited, so the fixture's row is limited and only the rebind skips the selection policy;
/// the default login itself is refused as retained.
#[test]
fn a_rebind_off_the_default_login_asks_its_new_logins_sign_in_and_no_limit() {
    let (world, who) = signed_out_default();
    let seat = world.seat();
    let d2 = made_login(&world, "m16-renewed");
    renew_into(&world, "m16", None, &d2);
    wall(&world, Cli::Claude, "m16", NOW + 3 * HOUR);
    let rebind = relaunch_rebind(&seat, &who, &mut no_token)
        .unwrap()
        .expect("a rebind");
    let Rebind::Row(current) = &rebind else {
        panic!("the row's own login: {rebind:?}");
    };
    assert_eq!(
        (current.label.as_str(), current.value.as_deref()),
        ("m16", d2.to_str())
    );
    let new = on(Some(&d2));
    let intent = capture_execution(&seat, Cli::Claude, &new, Purpose::Rebind).unwrap();
    let ExecutionIntent::Stored { owner, .. } = &intent else {
        panic!("a stored intent: {intent:?}");
    };
    assert_eq!(owner.as_deref(), Some("m16"));
    assert!(admit_execution(&seat, Cli::Claude, &intent).is_ok());
    assert_eq!(
        refused_kind(capture_execution(&seat, Cli::Claude, &new, Purpose::Select)),
        RefusalKind::Limited
    );
    assert_eq!(
        refused_kind(capture_execution(
            &seat,
            Cli::Claude,
            &who,
            Purpose::SameLogin
        )),
        RefusalKind::Retained
    );
}

/// A ROW WHOSE NEW LOGIN CANNOT SERVE HERE FALLS TO THE POOL'S PICK: `m16`'s new login is not made
/// on this Mac, so the relaunch starts on the pool's pick, `work-2` ([`Rebind::Pick`], a selection);
/// with rotation off it is refused as signed out, in words that name the row; and an intent that
/// rebinds onto the unmade login is refused at exec as signed out.
#[test]
fn a_rebind_whose_new_login_is_not_made_here_takes_the_pools_pick_or_waits_for_a_sign_in() {
    let (world, who) = signed_out_default();
    let seat = world.seat();
    let d2 = world
        .root
        .join("logins")
        .join(Cli::Claude.id())
        .join("m16-unmade");
    std::fs::create_dir_all(&d2).unwrap();
    renew_into(&world, "m16", None, &d2);
    let rebind = relaunch_rebind(&seat, &who, &mut no_token)
        .unwrap()
        .expect("a rebind");
    let Rebind::Pick(pick) = &rebind else {
        panic!("the pool's pick: {rebind:?}");
    };
    assert_eq!(pick.label, "work-2");
    let refusal =
        capture_execution(&seat, Cli::Claude, &on(Some(&d2)), Purpose::Rebind).unwrap_err();
    assert_eq!(refusal.kind, RefusalKind::SignedOut, "{refusal:?}");
    assert_eq!(
        refusal.why,
        "execution account's new login is not signed in here"
    );
    aterm_uds::atomic_file::write(
        &world.cfg.join("aterm.toml"),
        "[harness]\naccounts='off'\n".as_ref(),
        0o600,
    )
    .unwrap();
    let refusal = relaunch_rebind(&seat, &who, &mut no_token).unwrap_err();
    assert_eq!(refusal.kind, RefusalKind::SignedOut, "{refusal:?}");
    assert_eq!(
        refusal.why,
        "execution account m16's new login is not signed in here, and no other account serves"
    );
}

/// WHAT NO REBIND MOVES STAYS REFUSED, EACH ON ITS OWN WORD: a login whose account is no longer
/// listed (its binding kept) is retained, in words that say the account is gone; a login several
/// rows left (two rows each left the default home's login) with no login file there to name one of
/// them is no one account's, an integrity fault, for an in-place relaunch and a cold one alike
/// ([`a_login_several_rows_left_rebinds_onto_the_row_its_login_file_names`] has the file); and a
/// left login whose own file names an
/// account no row lists (m65) is never filed under the row that left it, so it is not rebound and
/// its capture is an integrity fault. CONTROL: with the file naming the row's own address, the
/// same login rebinds.
#[test]
fn a_login_no_rebind_moves_is_refused_as_retained_or_as_an_integrity_fault() {
    let (world, who) = fixture();
    let seat = world.seat();
    let d1 = PathBuf::from(who.login.clone().unwrap());
    renew_into(
        &world,
        "work",
        Some(&d1),
        &made_login(&world, "work-renewed"),
    );
    let rows = seat.roster(Cli::Claude).unwrap().rows;
    write_rows(&world, &rows[..1]);
    assert_eq!(relaunch_rebind(&seat, &who, &mut no_token).unwrap(), None);
    let refusal = capture_execution(&seat, Cli::Claude, &who, Purpose::SameLogin).unwrap_err();
    assert_eq!(
        (refusal.kind, refusal.why.as_str()),
        (
            RefusalKind::Retained,
            "execution login belongs to an account no longer listed here"
        )
    );

    let (world, who) = signed_out_default();
    let seat = world.seat();
    renew_into(&world, "m16", None, &made_login(&world, "m16-renewed"));
    renew_into(
        &world,
        "work-2",
        None,
        &made_login(&world, "work-2-renewed"),
    );
    let refusal = relaunch_rebind(&seat, &who, &mut no_token).unwrap_err();
    assert_eq!(
        (refusal.kind, refusal.why.as_str()),
        (
            RefusalKind::Integrity,
            "execution login was left by several accounts"
        )
    );
    let Err(cold) = super::super::cold_launch_admitted(
        &seat,
        Cli::Claude,
        who.clone(),
        &NoUsage,
        Some(&world.exe),
    ) else {
        panic!("a cold relaunch admitted a login several rows left");
    };
    assert!(cold.contains("left by several accounts"), "{cold}");

    for (named, rebinds) in [("work@example.com", true), ("other@example.com", false)] {
        let (world, who) = fixture();
        let seat = world.seat();
        let mut rows = seat.roster(Cli::Claude).unwrap().rows;
        rows[1].email = Some("work@example.com".into());
        write_rows(&world, &rows);
        let d1 = PathBuf::from(who.login.clone().unwrap());
        let d2 = made_login(&world, "work-renewed");
        profile_naming(&d2, "work@example.com");
        profile_naming(&d1, named);
        renew_into(&world, "work", Some(&d1), &d2);
        let rebind = relaunch_rebind(&seat, &who, &mut no_token).unwrap();
        assert_eq!(rebind.is_some(), rebinds, "{named}: {rebind:?}");
        let refusal = capture_execution(&seat, Cli::Claude, &who, Purpose::SameLogin).unwrap_err();
        if rebinds {
            assert_eq!(refusal.kind, RefusalKind::Retained, "{refusal:?}");
        } else {
            assert_eq!(
                (refusal.kind, refusal.why.as_str()),
                (
                    RefusalKind::Integrity,
                    "execution login names another account than the one that left it"
                )
            );
        }
    }
}

/// A LOGIN SEVERAL ROWS LEFT IS THE ROW ITS LOGIN FILE NAMES (two default-home rows each signed
/// into a login of its own, the process still on the default login): `m16` and `work-2` each left
/// the default home's login, so the records alone cannot say whose account the process runs on,
/// but the login file there names one row's address. The in-place relaunch starts on that row's
/// current login ([`Rebind::Row`]), and a cold relaunch of the same process starts there too,
/// selecting it, whichever row the file names. CONTROL: with no file read, the same login is an
/// integrity fault for both
/// ([`a_login_no_rebind_moves_is_refused_as_retained_or_as_an_integrity_fault`]).
///
/// NEGATIVE CONTROL (2026-11-17, `rebinds` refusing several leavers whatever the attribution, as
/// before the login file was read; restored with cp from a backup, confirmed with cmp): both
/// relaunches are refused as `execution login was left by several accounts`.
#[test]
fn a_login_several_rows_left_rebinds_onto_the_row_its_login_file_names() {
    for named in ["work-2", "m16"] {
        let (world, who) = signed_out_default();
        let seat = world.seat();
        let mut rows = seat.roster(Cli::Claude).unwrap().rows;
        for row in &mut rows {
            row.email = Some(format!("{}@example.com", row.label));
        }
        write_rows(&world, &rows);
        let m16 = made_login(&world, "m16-renewed");
        let work_2 = made_login(&world, "work-2-renewed");
        renew_into(&world, "m16", None, &m16);
        renew_into(&world, "work-2", None, &work_2);
        profile_naming(&world.home, &format!("{named}@example.com"));
        let to = if named == "m16" { &m16 } else { &work_2 };
        let rebind = match relaunch_rebind(&seat, &who, &mut no_token) {
            Ok(Some(rebind)) => rebind,
            other => panic!("{named}: {other:?}"),
        };
        let Rebind::Row(current) = &rebind else {
            panic!("{named}: the named row's own login: {rebind:?}");
        };
        assert_eq!(
            (current.label.as_str(), current.value.as_deref()),
            (named, to.to_str()),
        );
        let cold = match super::super::cold_launch_admitted(
            &seat,
            Cli::Claude,
            who.clone(),
            &NoUsage,
            Some(&world.exe),
        ) {
            Ok(cold) => cold,
            Err(why) => panic!("{named}: the cold relaunch is refused: {why}"),
        };
        assert_eq!(cold.pick.as_ref(), Some(current), "{named}");
        assert_eq!(
            cold.intent,
            capture_execution(&seat, Cli::Claude, &on(Some(to)), Purpose::Select).unwrap(),
            "{named}"
        );
    }
}

/// A TOKEN SESSION ON A LOGIN SEVERAL ROWS LEFT IS THE ROW ITS MARKER NAMES: the process runs on
/// `work`'s vault token on `work`'s login `D1`, which `work` left for `D2` and which `main`'s
/// records list as left too. The marker names `work`'s entry, the token the process runs on, so
/// the in-place relaunch starts on `D2` ([`Rebind::Row`]). CONTROL: a marker naming `main`'s entry
/// moves onto `main`'s login, never `work`'s.
///
/// NEGATIVE CONTROL (2026-11-17, `believed_row`'s token arm taking only a lone row that left the
/// login; restored with cp from a backup, confirmed with cmp): the token relaunch is not rebound.
#[cfg(unix)]
#[test]
fn a_token_session_on_a_login_several_rows_left_is_the_row_its_marker_names() {
    crate::harness::journal::with_keyer(token_keyer, || {
        let (world, who, _) = token_fixture();
        let seat = world.seat();
        let d1 = PathBuf::from(who.login.clone().unwrap());
        let d2 = made_login(&world, "work-renewed");
        let main_2 = made_login(&world, "main-renewed");
        renew_into(&world, "work", Some(&d1), &d2);
        renew_into(&world, "main", Some(&d1), &main_2);
        let roster = seat.roster(Cli::Claude).unwrap();
        let main = accounts::TokenEntry::of_row(roster.row("main").unwrap()).unwrap();
        let on_main = accounts::ProcessLogin {
            route: accounts::AuthRoute::VaultToken { entry: main },
            ..who.clone()
        };
        for (who, label, to) in [(&who, "work", &d2), (&on_main, "main", &main_2)] {
            let rebind = match relaunch_rebind(&seat, who, &mut no_token) {
                Ok(Some(rebind)) => rebind,
                other => panic!("{label}: {other:?}"),
            };
            let Rebind::Row(current) = &rebind else {
                panic!("{label}: the row's own login: {rebind:?}");
            };
            assert_eq!(
                (current.label.as_str(), current.value.as_deref()),
                (label, to.to_str()),
            );
        }
    });
}

/// A LOGIN IS A ROW'S ONLY WHERE IT IS SHOWN TO BE THAT ROW'S DIRECTORY: a process on a spelling of
/// row `work`'s login directory that differs only in letter case, never shown to be one directory
/// (a leaf not made yet here, as two directories of a case-sensitive volume are), is neither
/// `work`'s, whose identity and credential generation say nothing of it, nor no account's: it is
/// refused as an integrity fault, `work` held by another Mac or not. A rebind's intent for `work`
/// never stands for a process on such a spelling of a login `work` left. CONTROLS: a path that
/// resolves to `work`'s directory (a link to it) is `work`'s, so another Mac's hold refuses it as
/// held; the rebind's intent stands for a process on the left login as spelled; and a login no
/// row holds, retains or spells is an unowned login, captured. NEGATIVE CONTROL (applied to
/// `rows_at`, then to `left_directory`, each reverted to the collision rule): the spelling is
/// filed under `work`, held and then admitted with `work` as its owner, and the intent stands for
/// a process on the spelling.
#[cfg(unix)]
#[test]
fn a_login_is_a_rows_only_where_it_is_shown_to_be_that_rows_directory() {
    let (world, _) = fixture();
    let seat = world.seat();
    let parent = world.root.join("logins").join(Cli::Claude.id());
    std::fs::create_dir_all(&parent).unwrap();
    let fresh = parent.join("Fresh");
    let mut rows = seat.roster(Cli::Claude).unwrap().rows;
    rows[1].dir = Some(fresh.display().to_string());
    let spelled = on(Some(&parent.join("FRESH")));
    for held_by in [Some("another-machine"), None] {
        rows[1].held_by = held_by.map(str::to_owned);
        write_rows(&world, &rows);
        let refusal =
            capture_execution(&seat, Cli::Claude, &spelled, Purpose::SameLogin).unwrap_err();
        assert_eq!(
            (refusal.kind, refusal.why.as_str()),
            (
                RefusalKind::Integrity,
                "execution login differs from an account's login only in letter case"
            ),
            "{held_by:?}"
        );
    }
    std::fs::create_dir_all(&fresh).unwrap();
    std::os::unix::fs::symlink(&fresh, parent.join("alias")).unwrap();
    rows[1].held_by = Some("another-machine".into());
    write_rows(&world, &rows);
    assert_eq!(
        refused_kind(capture_execution(
            &seat,
            Cli::Claude,
            &on(Some(&parent.join("alias"))),
            Purpose::SameLogin
        )),
        RefusalKind::Held
    );
    let unowned = capture_execution(
        &seat,
        Cli::Claude,
        &on(Some(&parent.join("stale"))),
        Purpose::SameLogin,
    )
    .unwrap();
    assert!(
        matches!(&unowned, ExecutionIntent::Stored { owner: None, .. }),
        "{unowned:?}"
    );

    let (world, _) = fixture();
    let seat = world.seat();
    let parent = world.root.join("logins").join(Cli::Claude.id());
    std::fs::create_dir_all(&parent).unwrap();
    let gone = parent.join("Gone");
    let mut rows = seat.roster(Cli::Claude).unwrap().rows;
    rows[1].dir = Some(gone.display().to_string());
    write_rows(&world, &rows);
    let d2 = made_login(&world, "work-renewed");
    renew_into(&world, "work", Some(&gone), &d2);
    let roster = bound_roster(&seat, Cli::Claude).unwrap();
    let rebind = capture_execution(&seat, Cli::Claude, &on(Some(&d2)), Purpose::Rebind).unwrap();
    assert!(rebind.replaces_process(&on(Some(&gone)), true, Some(&roster)));
    assert!(!rebind.replaces_process(&on(Some(&parent.join("GONE"))), true, Some(&roster)));
}

/// A REBIND'S INTENT STANDS FOR A COMMAND THAT REPLACES THE PROCESS ON THE LOGIN ITS ROW LEFT
/// (the native upgrade's check of the live process against the intent): it matches the process
/// on `D1`, which `work` left, as it matches one on `D2` itself. CONTROLS: not without the bound
/// roster, under another user home, on a login `work` never held, on the default login another
/// row holds, or as a selection's intent onto the same login.
#[test]
fn a_rebind_intent_replaces_only_a_process_on_a_login_its_own_row_left() {
    let (world, who) = fixture();
    let seat = world.seat();
    let d1 = PathBuf::from(who.login.clone().unwrap());
    let d2 = made_login(&world, "work-renewed");
    renew_into(&world, "work", Some(&d1), &d2);
    let roster = bound_roster(&seat, Cli::Claude).unwrap();
    let new = on(Some(&d2));
    let rebind = capture_execution(&seat, Cli::Claude, &new, Purpose::Rebind).unwrap();
    assert!(rebind.replaces_process(&who, true, Some(&roster)));
    assert!(rebind.replaces_process(&new, true, Some(&roster)));
    assert!(!rebind.replaces_process(&who, true, None));
    assert!(!rebind.replaces_process(&who, false, Some(&roster)));
    let stranger = made_login(&world, "stranger");
    assert!(!rebind.replaces_process(&on(Some(&stranger)), true, Some(&roster)));
    assert!(!rebind.replaces_process(&on(None), true, Some(&roster)));
    let select = capture_execution(&seat, Cli::Claude, &new, Purpose::Select).unwrap();
    assert!(!select.replaces_process(&who, true, Some(&roster)));
    assert!(select.replaces_process(&new, true, Some(&roster)));
}

/// A RELAUNCH ON ITS ROW'S VAULT TOKEN MOVES OFF A LOGIN THE ROW LEFT TOO: the process runs on
/// `work`'s token (its marker names `work`'s entry) on `work`'s login `D1`, and `work` was renewed
/// into `D2`. No row holds `D1`, so the token route, resolved against the row that holds the
/// login, read as no account's: the relaunch kept `D1`, refused for good as retained. The token is
/// `work`'s, the row that left `D1`: the in-place relaunch starts on `D2` ([`Rebind::Row`]), its
/// token intent for a rebind is `work`'s and is admitted with `work`'s token in use here, and a
/// cold relaunch starts on `D2` too. CONTROLS: `D1` itself stays refused as retained, and a marker
/// naming `main`'s entry on `D1` is no account's own credential there, so it is not rebound and
/// its capture stays refused. NEGATIVE CONTROL (applied to `believed_row`, its token arm dropped,
/// then reverted): the token relaunch is not rebound.
#[cfg(unix)]
#[test]
fn a_relaunch_on_its_rows_vault_token_moves_off_a_login_the_row_left() {
    crate::harness::journal::with_keyer(token_keyer, || {
        let (world, who, entry) = token_fixture();
        let seat = world.seat();
        let d1 = PathBuf::from(who.login.clone().unwrap());
        let d2 = made_login(&world, "work-renewed");
        renew_into(&world, "work", Some(&d1), &d2);
        let rebind = relaunch_rebind(&seat, &who, &mut no_token)
            .unwrap()
            .expect("a rebind");
        let Rebind::Row(current) = &rebind else {
            panic!("the row's own login: {rebind:?}");
        };
        assert_eq!(
            (current.label.as_str(), current.value.as_deref()),
            ("work", d2.to_str())
        );
        let cold = super::super::cold_launch_admitted(
            &seat,
            Cli::Claude,
            who.clone(),
            &NoUsage,
            Some(&world.exe),
        )
        .unwrap();
        assert_eq!(
            cold.pick.as_ref(),
            Some(current),
            "a cold relaunch starts on the same login"
        );
        let moved = accounts::ProcessLogin {
            login: Some(d2.display().to_string()),
            ..who.clone()
        };
        let intent = capture_execution(&seat, Cli::Claude, &moved, Purpose::Rebind).unwrap();
        let ExecutionIntent::Token {
            owner,
            entry: got,
            purpose,
            ..
        } = &intent
        else {
            panic!("a token intent: {intent:?}");
        };
        assert_eq!(
            (owner.as_deref(), *got, *purpose),
            (Some("work"), entry, Purpose::Rebind)
        );
        let _lab = token_in_use(&world, entry);
        if let Err(refusal) = execution_admission(&seat, Cli::Claude, &intent) {
            panic!("the rebind's token intent is refused: {refusal:?}");
        }
        assert_eq!(
            refused_kind(capture_execution(
                &seat,
                Cli::Claude,
                &who,
                Purpose::SameLogin
            )),
            RefusalKind::Retained
        );
        let roster = seat.roster(Cli::Claude).unwrap();
        let main = accounts::TokenEntry::of_row(roster.row("main").unwrap()).unwrap();
        let elsewhere = accounts::ProcessLogin {
            route: accounts::AuthRoute::VaultToken { entry: main },
            ..who.clone()
        };
        assert_eq!(
            relaunch_rebind(&seat, &elsewhere, &mut no_token).unwrap(),
            None
        );
        assert!(capture_execution(&seat, Cli::Claude, &elsewhere, Purpose::SameLogin).is_err());
    });
}

/// A REBIND'S PICK NEVER TAKES A ROW ITS LINE WOULD START ON A LAPSED STORED LOGIN (m21's shape:
/// the row's new login is not made here, so the relaunch takes the pool's pick): `m16`'s new login
/// is not made here. `work-2`'s stored login reads signed out here and a fresh reading of the
/// vault names it as on its token, so rotation reads it runnable and picks it first; `work-3` is
/// signed in on a login of its own. Where the line's token judgment would not serve `work-2` (a
/// native upgrade's line, a build outside atpkg's store), the pick is `work-3`: a line on
/// `work-2`'s stored login is refused as signed out on every pass while the records stand. The
/// judgment is asked of `work-2` alone, once. Where it would serve, the pick is `work-2`, its line
/// on the token. CONTROL: rotation's own judgment reads `work-2` runnable.
///
/// NEGATIVE CONTROL (2026-11-17, `records_pick` choosing with `accounts::launch_row` unguarded, as
/// before; restored with cp from a backup, confirmed with cmp): the pick is `work-2` whatever the
/// judgment.
#[test]
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "a launch starts on a vault token only on macOS"
)]
fn a_rebinds_pick_never_takes_a_row_its_line_would_start_on_a_lapsed_login() {
    let (world, who) = signed_out_default();
    let seat = world.seat();
    let work_3 = made_login(&world, "work-3");
    let mut rows = seat.roster(Cli::Claude).unwrap().rows;
    let mut third = rows[1].clone();
    third.label = "work-3".into();
    third.dir = Some(work_3.display().to_string());
    rows.push(third);
    write_rows(&world, &rows);
    let store = seat.store(Cli::Claude);
    store
        .transact(NOW, &mut None, |state| {
            state.home_accounts.insert("work-3".into());
            Ok(accounts::note_auth(
                state,
                "work-2",
                accounts::Auth::SignedOut,
                NOW,
            ))
        })
        .unwrap();
    let read: std::collections::BTreeMap<String, u64> =
        [("work-2".to_owned(), u64::try_from(NOW + 86_400).unwrap())].into();
    assert!(store.note_tokens_here(Some(&read), NOW).unwrap());
    let d2 = world
        .root
        .join("logins")
        .join(Cli::Claude.id())
        .join("m16-unmade");
    std::fs::create_dir_all(&d2).unwrap();
    renew_into(&world, "m16", None, &d2);
    let roster = seat.roster(Cli::Claude).unwrap();
    let (state, _) = store.load_judged(NOW, &roster).unwrap();
    assert!(
        accounts::condition(roster.row("work-2").unwrap(), &state, true, NOW).runs(),
        "CONTROL: on its token, the row runs"
    );

    let mut asked = Vec::new();
    let mut judged = |row: &Row| {
        asked.push(row.label.clone());
        false
    };
    let rebind = relaunch_rebind(&seat, &who, &mut judged)
        .unwrap()
        .expect("a rebind");
    let Rebind::Pick(pick) = &rebind else {
        panic!("the pool's pick: {rebind:?}");
    };
    assert_eq!(
        (pick.label.as_str(), pick.value.as_deref()),
        ("work-3", work_3.to_str())
    );
    assert_eq!(asked, ["work-2"]);

    let rebind = relaunch_rebind(&seat, &who, &mut |_| true)
        .unwrap()
        .expect("a rebind");
    let Rebind::Pick(pick) = &rebind else {
        panic!("the pool's pick: {rebind:?}");
    };
    assert_eq!(pick.label, "work-2", "the line serves its token");
}

/// [`signed_out_default`] in m21's shape: row `m16` left the default home's login for `D2`
/// (returned), a login signed in on another Mac and so not made here, and a fresh reading of the
/// vault names it as on its token here, so its vault token alone keeps it running here; it is the
/// active row, and `work-2` is signed in on a login of its own. `change` edits the accounts state
/// last. CONTROL: `m16` runs here on its token, with no login made here.
fn on_its_token_alone(
    change: impl FnOnce(&mut accounts::State),
) -> (World, accounts::ProcessLogin, PathBuf) {
    let (world, who) = signed_out_default();
    let seat = world.seat();
    let d2 = world
        .root
        .join("logins")
        .join(Cli::Claude.id())
        .join("m16-elsewhere");
    std::fs::create_dir_all(&d2).unwrap();
    renew_into(&world, "m16", None, &d2);
    let store = seat.store(Cli::Claude);
    let read: std::collections::BTreeMap<String, u64> =
        [("m16".to_owned(), u64::try_from(NOW + 86_400).unwrap())].into();
    assert!(store.note_tokens_here(Some(&read), NOW).unwrap());
    store
        .transact(NOW, &mut None, |state| {
            state.active = Some("m16".into());
            change(state);
            Ok(Vec::new())
        })
        .unwrap();
    let roster = store
        .bound(&seat.roster(Cli::Claude).unwrap(), NOW)
        .unwrap();
    let (state, _) = store.load_judged(NOW, &roster).unwrap();
    let enrolled = accounts::enrolled_in(Cli::Claude, &roster, seat.home);
    assert_eq!(
        accounts::login_here(
            roster.row("m16").unwrap(),
            &state,
            enrolled.contains("m16"),
            NOW
        ),
        accounts::LoginHere::OnToken,
        "CONTROL: m16 runs here on its token alone"
    );
    (world, who, d2)
}

/// A REBIND WHOSE ROW RUNS HERE ONLY ON ITS VAULT TOKEN MOVES ONTO THAT TOKEN WHERE ITS LINE SERVES
/// IT (m21's shape, [`on_its_token_alone`]: `m16`'s new login `D2` is not made here, its vault
/// token keeps it running here, it is the active row and fresh, and `work-2` is signed in). Where
/// the line's token judgment serves `m16`, the pick is `m16`'s own `D2`, a selection onto its
/// token, the judgment asked of `m16` alone, once; where it does not, the line would start on
/// `m16`'s lapsed stored login, and the pick is `work-2`. A usage limit on `m16`, which a
/// selection onto its token refuses, gives `work-2` too. The owner's pin on `m16` holds the same
/// way: the pick is `m16`'s `D2` where the line serves its token, and none (signed out) where it
/// does not, never `work-2` past the pin.
///
/// NEGATIVE CONTROL (2026-11-17, `records_pick` as before: its pick of `ran` itself thrown away,
/// the pool's pick never past it; restored with cp from a backup, confirmed with cmp): where the
/// line serves `m16`'s token, the relaunch waits signed out, pinned or not.
#[test]
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "a launch starts on a vault token only on macOS"
)]
fn a_rebind_whose_row_runs_here_only_on_its_token_moves_onto_that_token() {
    let picked = |rebind: Result<Option<Rebind>, Refusal>| {
        let rebind = rebind.unwrap().expect("a rebind");
        let Rebind::Pick(pick) = &rebind else {
            panic!("the pool's pick: {rebind:?}");
        };
        (pick.label.clone(), pick.value.clone())
    };

    let (world, who, d2) = on_its_token_alone(|_| {});
    let seat = world.seat();
    let mut asked = Vec::new();
    let mut serving = |row: &Row| {
        asked.push(row.label.clone());
        true
    };
    assert_eq!(
        picked(relaunch_rebind(&seat, &who, &mut serving)),
        ("m16".to_owned(), Some(d2.display().to_string())),
        "the line serves m16's token"
    );
    assert_eq!(asked, ["m16"]);
    let mut asked = Vec::new();
    let mut declining = |row: &Row| {
        asked.push(row.label.clone());
        false
    };
    let (label, _) = picked(relaunch_rebind(&seat, &who, &mut declining));
    assert_eq!(label, "work-2", "the line does not serve m16's token");
    assert_eq!(asked, ["m16"]);

    let (world, who, _) = on_its_token_alone(|state| {
        accounts::note_wall(state, "m16", "weekly", Some(NOW + 86_400), NOW, NOW);
    });
    let (label, _) = picked(relaunch_rebind(&world.seat(), &who, &mut |_| true));
    assert_eq!(label, "work-2", "a limit holds m16");

    let (world, who, d2) = on_its_token_alone(|state| state.pinned = true);
    let seat = world.seat();
    assert_eq!(
        picked(relaunch_rebind(&seat, &who, &mut |_| true)),
        ("m16".to_owned(), Some(d2.display().to_string())),
        "pinned, the line serves m16's token"
    );
    let refusal = relaunch_rebind(&seat, &who, &mut no_token).unwrap_err();
    assert_eq!(refusal.kind, RefusalKind::SignedOut, "{refusal:?}");
}

/// A REBIND WHOSE ROW'S TOKEN DOES NOT CARRY ITS LINE STILL WAITS FOR A SIGN-IN: as
/// [`a_rebind_whose_row_runs_here_only_on_its_token_moves_onto_that_token`], but `work-2` is held
/// by another Mac, so no other row serves. Where the line's token judgment does not serve `m16` (a
/// native upgrade's line, a build outside atpkg's store), the relaunch is refused as signed out,
/// in words that name the row, never planned onto `m16`'s lapsed stored login. CONTROL: where it
/// serves, the pick is `m16`'s `D2`.
///
/// NEGATIVE CONTROL (2026-11-17, `relaunch_keeps` without its token guard; restored with cp from a
/// backup, confirmed with cmp): the line that does not serve `m16`'s token is picked onto `D2`.
#[test]
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "a launch starts on a vault token only on macOS"
)]
fn a_rebind_whose_rows_token_does_not_carry_its_line_still_waits_for_a_sign_in() {
    let (world, who, d2) = on_its_token_alone(|_| {});
    let seat = world.seat();
    let mut rows = seat.roster(Cli::Claude).unwrap().rows;
    rows.iter_mut()
        .find(|row| row.label == "work-2")
        .unwrap()
        .held_by = Some("another-machine".into());
    write_rows(&world, &rows);
    let refusal = relaunch_rebind(&seat, &who, &mut no_token).unwrap_err();
    assert_eq!(refusal.kind, RefusalKind::SignedOut, "{refusal:?}");
    assert_eq!(
        refusal.why,
        "execution account m16's new login is not signed in here, and no other account serves"
    );
    let rebind = relaunch_rebind(&seat, &who, &mut |_| true)
        .unwrap()
        .expect("a rebind");
    let Rebind::Pick(pick) = &rebind else {
        panic!("the pool's pick: {rebind:?}");
    };
    assert_eq!(
        (pick.label.as_str(), pick.value.as_deref()),
        ("m16", d2.to_str()),
        "CONTROL: the line serves m16's token"
    );
}

/// A READMISSION WHOSE ROW LEFT ITS LOGIN SINCE THE PLAN STOPS AT ONCE: the line, typed once the
/// agent ended, names the login its plan named, and no pass plans the intent again to move it onto
/// the row's current login. A same-login intent on `work`'s login `D1`, captured before `work` was
/// renewed into `D2`, is moved at readmission; so is the in-place relaunch's rebind onto `D2` once
/// `work` renews again into `D3` before the line. The relaunch then stops (`account-moved`),
/// never refused as a change that waits out its stale clock on a login that can never be admitted
/// again. CONTROLS: before the signal the same rebind is refused as a change under way (the next
/// pass plans again), and a renewal that keeps `D2` readmits the rebind under a new generation.
///
/// NEGATIVE CONTROL (2026-11-17, `owner_left` answering false; restored with cp from a backup,
/// confirmed with cmp): both readmissions are refused, the same-login one as retained and the
/// rebind as a change.
#[test]
fn a_readmission_whose_row_left_its_login_since_the_plan_stops_at_once() {
    let moved =
        |what: &str, readmitted: Result<(ExecutionGuard, Option<ExecutionIntent>), Unadmitted>| {
            match readmitted {
                Err(Unadmitted::Moved(refusal)) => {
                    assert_eq!(refusal.kind, RefusalKind::Changing, "{what}: {refusal:?}");
                    assert_eq!(
                        refusal.why,
                        "execution login is one its account left for a newer login since its plan",
                        "{what}"
                    );
                }
                Ok((_, renewed)) => panic!("{what}: readmitted: {renewed:?}"),
                Err(other) => panic!("{what}: {other:?}"),
            }
        };
    let (world, who) = fixture();
    let seat = world.seat();
    let d1 = PathBuf::from(who.login.clone().unwrap());
    let same = capture_execution(&seat, Cli::Claude, &who, Purpose::SameLogin).unwrap();
    let d2 = made_login(&world, "work-renewed");
    renew_into(&world, "work", Some(&d1), &d2);
    moved(
        "a same-login intent",
        execution_readmission(&seat, Cli::Claude, &same),
    );

    let rebind = capture_execution(&seat, Cli::Claude, &on(Some(&d2)), Purpose::Rebind).unwrap();
    renew_into(&world, "work", Some(&d2), &d2);
    let (guard, renewed) = execution_readmission(&seat, Cli::Claude, &rebind).unwrap();
    drop(guard);
    assert!(
        renewed.is_some(),
        "CONTROL: a renewal in place moves the generation"
    );
    let d3 = made_login(&world, "work-renewed-again");
    renew_into(&world, "work", Some(&d2), &d3);
    assert_eq!(
        refused_kind(execution_admission(&seat, Cli::Claude, &rebind)),
        RefusalKind::Changing,
        "CONTROL: before the signal, a change the next pass plans again"
    );
    moved(
        "a rebind's intent",
        execution_readmission(&seat, Cli::Claude, &rebind),
    );
}
