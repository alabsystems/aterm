// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::RefCell;

#[path = "accounts_pool_observation_tests.rs"]
mod observations;

#[test]
fn a_pooled_turn_end_cannot_prove_a_known_wrong_identity() {
    for actual in [1, 2] {
        let provider = Cli::Codex;
        let world = World::new("identity-served");
        let (directory, label) = setup(&world, provider, false, 1);
        let path = profile_path(&world, &directory, provider, false, false);
        let bytes = credential(&path, provider, actual);
        let store = world.seat().store(provider);
        let canary = accounts::Canary {
            sid: "s-one".into(),
            at: NOW,
            took: false,
        };
        store
            .transact(NOW, &mut None, |state| {
                state.active = Some(label.clone());
                state.proven = false;
                state.canary = Some(canary.clone());
                state.undo.push(accounts::Undo::default());
                Ok(Vec::new())
            })
            .unwrap();
        let roster = world.seat().bound_roster(provider).unwrap();
        let mut s = session(
            provider,
            Supervisor::Aterm,
            Mode::Early,
            Path::new("/opt/codex"),
        );
        s.sample = None;
        let who = accounts::ProcessLogin {
            login: Some(directory.display().to_string()),
            default_home: true,
            stored_login: true,
        };
        let _ = super::record_session(&world.seat(), &s, &who, roster.row(&label).unwrap());
        let after = store.load(NOW).unwrap().0;
        assert_eq!(after.proven, actual == 1, "credential identity {actual}");
        if actual == 2 {
            assert_eq!(after.canary, Some(canary));
            assert_eq!(after.undo.len(), 1);
            assert!(
                !store
                    .tail(100)
                    .unwrap()
                    .iter()
                    .any(|event| event.kind == accounts::Kind::Proven)
            );
        }
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

fn cli(provider: i64) -> Cli {
    if provider == 0 {
        Cli::Claude
    } else {
        Cli::Codex
    }
}

fn email(kind: i64) -> &'static str {
    match kind {
        1 => "one@example.test",
        2 => "two@example.test",
        _ => unreachable!(),
    }
}

fn profile_path(
    world: &World,
    directory: &Path,
    provider: Cli,
    default: bool,
    legacy: bool,
) -> PathBuf {
    if provider == Cli::Codex {
        directory.join("auth.json")
    } else if legacy {
        directory.join(".config.json")
    } else if default {
        world.home.join(".claude.json")
    } else {
        directory.join(".claude.json")
    }
}

fn credential(path: &Path, provider: Cli, kind: i64) -> Vec<u8> {
    let document = if provider == Cli::Claude {
        if kind == 0 {
            aterm_json::json!({"oauthAccount":{"accountUuid":"opaque-account"}})
        } else {
            aterm_json::json!({"oauthAccount":{"emailAddress":email(kind),"accountUuid":"named-account"}})
        }
    } else {
        let claims = if kind == 0 {
            aterm_json::json!({"sub":"opaque-account"})
        } else {
            aterm_json::json!({"email":email(kind)})
        };
        let token = crate::harness::codex_app_login::tests::make_jwt(&claims);
        aterm_json::json!({"auth_mode":"chatgpt", "tokens":{"id_token":token,"access_token":token}})
    };
    let bytes = aterm_json::to_vec(&document).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, &bytes).unwrap();
    bytes
}

fn setup(world: &World, provider: Cli, default: bool, expected: i64) -> (PathBuf, String) {
    let work = world.two(provider);
    let label = if default { "main" } else { "work" };
    let directory = if default {
        world.home.join(provider.accounts().default_home)
    } else {
        work
    };
    std::fs::create_dir_all(&directory).unwrap();
    let path = world.cfg.join(provider.accounts().roster_file);
    let mut roster = world.seat().roster(provider).unwrap();
    let row = roster
        .rows
        .iter_mut()
        .find(|row| row.label == label)
        .unwrap();
    if expected != 0 {
        row.email = Some("delivery-alias@example.test".into());
        row.identity_email = Some(email(expected).to_ascii_uppercase());
    }
    std::fs::write(
        path,
        accounts::append_rows_for(provider, "", &roster.rows).unwrap(),
    )
    .unwrap();
    (directory, label.into())
}

fn prepare_case(provider: i64, default: bool, expected: i64, kind: i64, legacy: bool) -> bool {
    let world = World::new("identity-prepare");
    let provider = cli(provider);
    let (directory, label) = setup(&world, provider, default, expected);
    let path = profile_path(&world, &directory, provider, default, legacy);
    let bytes = credential(&path, provider, kind);
    let roster = world.seat().roster(provider).unwrap();
    let result = super::super::login_env(provider, roster.row(&label).unwrap(), &world.home);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    result.is_ok()
}

fn finish_case(
    provider: i64,
    route: i64,
    default: bool,
    expected: i64,
    seen: i64,
    current: i64,
    legacy: bool,
) -> [(&'static str, i64); 5] {
    let world = World::new("identity-finish");
    let provider = cli(provider);
    let (directory, label) = setup(&world, provider, default, expected);
    let path = profile_path(&world, &directory, provider, default, legacy);
    credential(&path, provider, seen);
    world.on_main(provider, &[]);
    let other = if default { "work" } else { "main" };
    let store = world.seat().store(provider);
    store
        .transact(NOW, &mut None, |state| {
            state.active = Some(if route == 2 { other } else { &label }.into());
            state.pinned = route == 1;
            Ok(Vec::new())
        })
        .unwrap();
    if route == 1 {
        std::fs::write(world.cfg.join("aterm.toml"), "[harness]\naccounts='off'\n").unwrap();
    }
    let files = [
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.wake_path(),
    ];
    let captured = RefCell::new(None);
    let evidence = RefCell::new(None);
    let mut prepare = |row: &Row| -> io::Result<LoginEnv> {
        assert_eq!(row.label, label);
        let env = super::super::login_env(provider, row, &world.home)?;
        *evidence.borrow_mut() = Some(credential(&path, provider, current));
        *captured.borrow_mut() = Some(files.each_ref().map(|file| std::fs::read(file).ok()));
        Ok(env)
    };
    let returned = if route == 2 {
        let roster = world.seat().roster(provider).unwrap();
        let (name, value) = accounts::login_env_for(provider, roster.row(other).unwrap());
        let env: Vec<_> = value
            .as_deref()
            .map(|value| (name, value))
            .into_iter()
            .collect();
        let mut session = session(
            provider,
            Supervisor::Aterm,
            Mode::Early,
            Path::new("/opt/provider"),
        );
        session.env = &env;
        session.point = Point::Wall {
            window: "five_hour".into(),
            at: Some(NOW - MIN),
        };
        let moved = super::session_move_using(&world.seat(), &session, |row| {
            let env = prepare(row).ok()?;
            relaunch::pool_line(&session.relaunch, provider, &env).ok()
        });
        if moved.line.is_some() {
            assert!(matches!(moved.advice, Advice::Move { ref to, .. } if to == &label));
        }
        moved.line.is_some()
    } else {
        let result =
            super::super::launch_env_using(&world.seat(), provider, &no_usage, None, &mut prepare);
        match result {
            Ok(Some(env)) => {
                assert_eq!(env.label, label);
                true
            }
            Err(_) => false,
            Ok(None) => panic!("managed admission became unmanaged"),
        }
    };
    let captured = captured
        .into_inner()
        .expect("prepared before identity changed");
    let preserved = std::fs::read(&path).unwrap() == evidence.into_inner().unwrap();
    [
        ("phase", 2),
        ("returned", i64::from(returned)),
        (
            "rotated",
            i64::try_from(world.load(provider).rotations.len()).unwrap(),
        ),
        (
            "unchanged",
            i64::from(files.each_ref().map(|file| std::fs::read(file).ok()) == captured),
        ),
        ("preserved", i64::from(preserved)),
    ]
}

pub(crate) fn identity_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    if !matches!(action, "Prepare" | "Finish") {
        account_pool_identity_environment();
    }
    match action {
        "Provider" => {
            next.insert("provider", 1);
        }
        "Route" => {
            next.insert("route", state["route"] + 1);
        }
        "Default" => {
            next.insert("default", 1);
        }
        "Expected" => {
            next.insert("expected", state["expected"] + 1);
        }
        "Credential" | "Change" => {
            next.insert("credential", state["credential"] + 1);
        }
        "Unknown" => {
            next.insert("credential", 0);
        }
        "Prepare" => {
            let ok = prepare_case(
                state["provider"],
                state["default"] == 1,
                state["expected"],
                state["credential"],
                false,
            );
            if state["provider"] == 0 {
                assert_eq!(
                    ok,
                    prepare_case(
                        0,
                        state["default"] == 1,
                        state["expected"],
                        state["credential"],
                        true
                    )
                );
            }
            next.insert("seen", state["credential"]);
            next.insert("prepared", i64::from(ok));
            next.insert("phase", if ok { 1 } else { 2 });
        }
        "Finish" => {
            let result = finish_case(
                state["provider"],
                state["route"],
                state["default"] == 1,
                state["expected"],
                state["seen"],
                state["credential"],
                false,
            );
            if state["provider"] == 0 {
                assert_eq!(
                    result,
                    finish_case(
                        0,
                        state["route"],
                        state["default"] == 1,
                        state["expected"],
                        state["seen"],
                        state["credential"],
                        true
                    )
                );
            }
            for (field, value) in result {
                next.insert(field, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn pool_identity_conforms_through_preparation_and_final_admission() {
    let model = aterm_models_harness::account_pool_identity_model();
    assert_eq!(model.mutant_dials(), (1..=9).collect::<Vec<_>>());
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = identity_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let mut state = model.init_state();
    state.insert("phase", 1);
    state.insert("expected", 1);
    state.insert("seen", 1);
    state.insert("prepared", 1);
    state.insert("credential", 2);
    let good = identity_transition(&state, "Finish");
    let mut stale = good.clone();
    stale.insert("returned", 1);
    assert!(!model.check_invariant("Admission", &stale));
    let mut altered = good;
    altered.insert("preserved", 0);
    assert!(!model.check_invariant("Evidence", &altered));
}

#[test]
fn bad_claude_identity_uses_an_alternative_unless_pinned() {
    for default in [false, true] {
        for (pinned, off) in [(false, false), (true, false), (true, true)] {
            let world = World::new("identity-fallback");
            let (directory, label) = setup(&world, Cli::Claude, default, 1);
            let path = profile_path(&world, &directory, Cli::Claude, default, false);
            let bytes = credential(&path, Cli::Claude, 2);
            world.on_main(Cli::Claude, &[]);
            world
                .seat()
                .store(Cli::Claude)
                .transact(NOW, &mut None, |state| {
                    state.active = Some(label.clone());
                    state.pinned = pinned;
                    Ok(Vec::new())
                })
                .unwrap();
            if off {
                std::fs::write(world.cfg.join("aterm.toml"), "[harness]\naccounts='off'\n")
                    .unwrap();
            }
            let result = super::super::launch_env(&world.seat(), Cli::Claude, &no_usage);
            if pinned {
                assert!(result.is_err_and(|why| why.contains("expected account")));
            } else {
                assert_eq!(
                    result.unwrap().unwrap().label,
                    if default { "work" } else { "main" }
                );
            }
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
    }
}

#[test]
fn claude_identity_uses_its_own_legacy_profile_and_preserves_unknown_compatibility() {
    for default in [false, true] {
        let world = World::new("identity-legacy");
        let (directory, label) = setup(&world, Cli::Claude, default, 1);
        let plain = profile_path(&world, &directory, Cli::Claude, default, false);
        let legacy = profile_path(&world, &directory, Cli::Claude, default, true);
        let plain_bytes = credential(&plain, Cli::Claude, 1);
        let legacy_bytes = credential(&legacy, Cli::Claude, 2);
        let roster = world.seat().roster(Cli::Claude).unwrap();
        let row = roster.row(&label).unwrap();
        assert!(super::super::login_env(Cli::Claude, row, &world.home).is_err());
        assert_eq!(std::fs::read(&plain).unwrap(), plain_bytes);
        assert_eq!(std::fs::read(&legacy).unwrap(), legacy_bytes);
        for unknown in [b"{}".as_slice(), b"{", b""] {
            std::fs::write(&legacy, unknown).unwrap();
            assert!(super::super::login_env(Cli::Claude, row, &world.home).is_ok());
            assert_eq!(std::fs::read(&legacy).unwrap(), unknown);
        }
    }
}
