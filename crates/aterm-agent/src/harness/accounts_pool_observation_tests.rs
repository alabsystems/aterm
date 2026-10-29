// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State as ModelState;

fn evidence(path: &Path, provider: Cli, kind: i64) -> Vec<u8> {
    if kind != 3 {
        return credential(path, provider, kind);
    }
    assert_eq!(provider, Cli::Codex);
    let token = |user: &str| {
        crate::harness::codex_app_login::tests::make_jwt(&aterm_json::json!({
            "email": email(1), "https://api.openai.com/auth": {"chatgpt_user_id":user}
        }))
    };
    let bytes = aterm_json::to_vec(&aterm_json::json!({
        "auth_mode":"chatgpt", "tokens":{"id_token":token("one"),"access_token":token("two")}
    }))
    .unwrap();
    std::fs::write(path, &bytes).unwrap();
    bytes
}

fn observe_case(state: &ModelState, legacy: bool, capture_late: bool) -> [(&'static str, i64); 11] {
    let provider = cli(state["provider"]);
    let default = state["default"] == 1;
    let world = World::new("observed-identity");
    let (directory, label) = setup(&world, provider, default, state["expected"]);
    let path = profile_path(&world, &directory, provider, default, legacy);
    let original = evidence(&path, provider, state["credential"]);
    let store = world.seat().store(provider);
    let captured = world
        .seat()
        .bound_roster(provider)
        .unwrap()
        .row(&label)
        .unwrap()
        .clone();
    let replacement = world.root.join("replacement");
    let replacement_path = profile_path(&world, &replacement, provider, false, false);
    let mut preserved = vec![(path, original)];
    if state["binding"] == 1 {
        let decoy = if state["credential"] == 1 { 2 } else { 1 };
        preserved.push((
            replacement_path.clone(),
            credential(&replacement_path, provider, decoy),
        ));
        store
            .transact_logins(NOW, &mut None, |state, logins| {
                Ok(accounts::note_renewed(
                    state,
                    logins,
                    &label,
                    captured.dir.as_deref(),
                    replacement.to_str().unwrap(),
                    None,
                    NOW,
                ))
            })
            .unwrap();
    }
    store
        .transact(NOW, &mut None, |state| {
            state.active = Some(label.clone());
            state.proven = false;
            state.canary = Some(accounts::Canary {
                sid: "s-one".into(),
                at: NOW,
                took: false,
            });
            state.undo.push(accounts::Undo::default());
            Ok(accounts::note_auth(
                state,
                &label,
                accounts::Auth::SignedOut,
                NOW,
            ))
        })
        .unwrap();
    let who = accounts::ProcessLogin {
        login: if state["binding"] == 2 {
            Some(world.root.join("unknown").display().to_string())
        } else if default {
            None
        } else {
            Some(directory.display().to_string())
        },
        default_home: true,
        stored_login: true,
    };
    let mut s = session(
        provider,
        Supervisor::Aterm,
        Mode::Early,
        Path::new("/opt/provider"),
    );
    if state["point"] == 1 {
        s.point = Point::Wall {
            window: "session".into(),
            at: Some(NOW),
        };
    }
    let latest = world.seat().bound_roster(provider).unwrap();
    let row = if capture_late {
        latest.row(&label).unwrap()
    } else {
        &captured
    };
    let files = [
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.wake_path(),
    ];
    let before = files.each_ref().map(|file| std::fs::read(file).ok());
    let accepted = super::super::record_session(&world.seat(), &s, &who, row).is_ok();
    let after = store.load(NOW).unwrap().0;
    let account = after.account(&label);
    [
        ("phase", 1),
        ("accepted", i64::from(accepted)),
        ("proven", i64::from(after.proven)),
        ("canary", i64::from(after.canary.is_some())),
        ("undo", i64::try_from(after.undo.len()).unwrap()),
        ("auth", i64::from(account.auth == accounts::Auth::Ok)),
        ("session", i64::from(after.sessions.contains_key("s-one"))),
        ("sample", i64::from(account.sample.is_some())),
        ("wall", i64::from(account.walls.contains_key("session"))),
        (
            "unchanged",
            i64::from(before == files.each_ref().map(|file| std::fs::read(file).ok())),
        ),
        (
            "preserved",
            i64::from(
                preserved
                    .iter()
                    .all(|(path, bytes)| std::fs::read(path).unwrap() == *bytes),
            ),
        ),
    ]
}

pub(crate) fn observation_transition(state: &ModelState, action: &str) -> ModelState {
    let mut next = state.clone();
    match action {
        "Provider" | "Default" | "Expected" | "Credential" | "Binding" | "Point" => {
            super::super::super::account_pool_observation_environment();
            let field = match action {
                "Provider" => "provider",
                "Default" => "default",
                "Expected" => "expected",
                "Credential" => "credential",
                "Binding" => "binding",
                "Point" => "point",
                _ => unreachable!(),
            };
            next.insert(field, state[field] + 1);
        }
        "Observe" => {
            let result = observe_case(state, false, false);
            assert_eq!(
                result,
                observe_case(state, false, true),
                "capture after renewal: {state:?}"
            );
            if state["provider"] == 0 {
                assert_eq!(
                    result,
                    observe_case(state, true, false),
                    "legacy Claude profile: {state:?}"
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
fn pool_observation_identity_conforms_for_current_and_retained_logins() {
    let model = aterm_models_harness::account_pool_observation_identity_model();
    assert_eq!(model.mutant_dials(), (1..=13).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = observation_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let mut state = model.init_state();
    state.insert("expected", 1);
    state.insert("credential", 2);
    let good = observation_transition(&state, "Observe");
    for (field, value, law) in [
        ("accepted", 1, "Admission"),
        ("proven", 1, "Proof"),
        ("canary", 0, "Canary"),
        ("undo", 0, "Undo"),
        ("auth", 1, "Auth"),
        ("session", 1, "Session"),
        ("sample", 1, "Sample"),
        ("wall", 1, "Wall"),
        ("unchanged", 0, "Records"),
        ("preserved", 0, "Evidence"),
    ] {
        let mut bad = good.clone();
        bad.insert(field, value);
        assert!(!model.check_invariant(law, &bad), "{law} negative control");
    }
}
#[test]
fn an_observation_uses_the_identity_policy_current_inside_the_writer() {
    let provider = Cli::Codex;
    let world = World::new("observation-identity-policy");
    let (directory, label) = setup(&world, provider, false, 1);
    let path = profile_path(&world, &directory, provider, false, false);
    let bytes = credential(&path, provider, 1);
    let captured = world
        .seat()
        .bound_roster(provider)
        .unwrap()
        .row(&label)
        .unwrap()
        .clone();
    let mut roster = world.seat().roster(provider).unwrap();
    roster
        .rows
        .iter_mut()
        .find(|row| row.label == label)
        .unwrap()
        .identity_email = Some(email(2).into());
    std::fs::write(
        world.cfg.join(provider.accounts().roster_file),
        accounts::append_rows_for(provider, "", &roster.rows).unwrap(),
    )
    .unwrap();
    let store = world.seat().store(provider);
    let files = [
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.wake_path(),
    ];
    let before = files.each_ref().map(|file| std::fs::read(file).ok());
    let who = accounts::ProcessLogin {
        login: Some(directory.display().to_string()),
        default_home: true,
        stored_login: true,
    };
    let s = session(
        provider,
        Supervisor::Aterm,
        Mode::Early,
        Path::new("/opt/codex"),
    );
    assert!(super::super::record_session(&world.seat(), &s, &who, &captured).is_err());
    assert_eq!(
        before,
        files.each_ref().map(|file| std::fs::read(file).ok())
    );
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[test]
fn the_public_session_path_refuses_wrong_identity_before_preparing_a_move() {
    for provider in [Cli::Claude, Cli::Codex] {
        for default in [false, true] {
            let world = World::new("session-observed-identity");
            let (directory, _label) = setup(&world, provider, default, 1);
            let path = profile_path(&world, &directory, provider, default, false);
            credential(&path, provider, 2);
            let text = match provider.accounts().seam {
                crate::ai::connector::Seam::LoginFile { file, .. } => {
                    directory.join(file).display().to_string()
                }
                _ => directory.display().to_string(),
            };
            let env = [(provider.accounts().seam.var(), text.as_str())];
            let mut s = session(
                provider,
                Supervisor::Aterm,
                Mode::Early,
                Path::new("/opt/provider"),
            );
            if !default {
                s.env = &env;
            }
            let store = world.seat().store(provider);
            let files = [
                store.state_file(),
                store.logins_file(),
                store.ledger_path(),
                store.wake_path(),
            ];
            let before = files.each_ref().map(|file| std::fs::read(file).ok());
            let moved = super::super::session_move_using(&world.seat(), &s, |_| {
                panic!("wrong identity reached move preparation")
            });
            assert_eq!(moved.advice, Advice::Stay(StayWhy::NothingDue));
            assert!(moved.line.is_none());
            assert_eq!(
                before,
                files.each_ref().map(|file| std::fs::read(file).ok())
            );
        }
    }
}
