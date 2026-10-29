// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State as ModelState;

const NOW: i64 = 1_790_000_000;

fn address(value: i64) -> Option<&'static str> {
    match value {
        0 => None,
        1 => Some("alice@example.test"),
        2 => Some("bob@example.test"),
        _ => unreachable!(),
    }
}

fn observe(state: &ModelState, polling: bool, variant: u8) -> Vec<(&'static str, i64)> {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root
        .path()
        .join(if variant == 1 { ".codex" } else { "login" });
    let claims = |value| {
        let mut claims = json!({"https://api.openai.com/auth":{
            "chatgpt_user_id":"same-user", "chatgpt_account_id":"same-workspace"
        }});
        if let Some(email) = address(value) {
            let email = if variant == 2 {
                format!("  {}  ", email.to_uppercase())
            } else {
                email.into()
            };
            if variant == 1 {
                claims.as_object_mut().unwrap().insert(
                    "https://api.openai.com/profile".into(),
                    json!({"email":email}),
                );
            } else {
                claims
                    .as_object_mut()
                    .unwrap()
                    .insert("email".into(), json!(email));
            }
        }
        make_jwt(&claims)
    };
    let token = if variant == 2 && state["access_email"] == 0 {
        "opaque-access".into()
    } else {
        claims(state["access_email"])
    };
    let id_token = if variant == 2 && state["email"] == 0 {
        "opaque-id".into()
    } else {
        claims(state["email"])
    };
    write_auth_doc(
        &home,
        &json!({"auth_mode":"chatgpt", "owner_note":"retained", "tokens":{
            "id_token":id_token, "access_token":token
        }}),
    )
    .unwrap();
    let path = home.join(AUTH_FILE);
    let bytes = std::fs::read(&path).unwrap();
    let mut row = crate::harness::accounts::Row {
        label: "named".into(),
        dir: (variant != 1).then(|| home.to_string_lossy().into_owned()),
        email: address(state["expected"]).map(str::to_owned),
        ..Default::default()
    };
    if variant == 1 && row.email.is_some() {
        row.identity_email = row.email.take();
        row.email = Some("explicit-mailbox-alias@example.test".into());
    }
    if !polling {
        let result = crate::harness::accounts_pool::login_env(
            crate::ai::connector::Cli::Codex,
            &row,
            root.path(),
        );
        match &result {
            Ok(env) => {
                assert_eq!(env.label, row.label);
                assert_eq!(env.var, "CODEX_AUTH_PATH");
                assert_eq!(
                    env.value,
                    row.dir
                        .as_ref()
                        .map(|_| path.to_string_lossy().into_owned())
                );
            }
            Err(why) => {
                assert_eq!(why.kind(), std::io::ErrorKind::InvalidData);
                assert!(why.to_string().contains("expected account"));
                assert!(!why.to_string().contains("@example.test"));
            }
        }
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        return vec![("prepared", i64::from(result.is_ok()))];
    }
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let endpoint = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        loop {
            let (mut stream, _) = listener.accept().unwrap();
            let Some(request) = read_http_request(&mut stream) else {
                break;
            };
            requests.push(request);
            send_http_json(
                &mut stream,
                "200 OK",
                &json!({"rate_limit":{
                    "primary_window":{"used_percent":25.0, "limit_window_seconds":18000}
                }}),
            );
        }
        requests
    });
    let base = format!("http://{endpoint}");
    let usage = read_usage_with_expected_email(
        if variant == 2 { &path } else { &home },
        &format!("{base}/usage"),
        &format!("{base}/oauth/token"),
        Duration::from_secs(60),
        NOW,
        row.expected_identity_email(),
    );
    let _ = TcpStream::connect(endpoint);
    let seen = server.join().unwrap();
    let scoped = |requests: &[RecordedHttp]| {
        requests.iter().all(|request| {
            request.method == "GET"
                && request.target == "/usage"
                && request.body.is_empty()
                && request
                    .headers
                    .iter()
                    .filter(|(key, _)| key == "authorization")
                    .map(|(_, value)| value.as_str())
                    .collect::<Vec<_>>()
                    == [format!("Bearer {token}").as_str()]
        })
    };
    if let Some(usage) = &usage {
        assert_eq!(usage.sample.session_pct, Some(25));
        assert_eq!(usage.sample.at, NOW);
        let mut corrupt = seen.clone();
        corrupt[0]
            .headers
            .iter_mut()
            .find(|(key, _)| key == "authorization")
            .unwrap()
            .1 = "Bearer wrong-credential".into();
        assert!(!scoped(&corrupt), "request observer ignored the credential");
    }
    vec![
        ("usage", i64::from(usage.is_some())),
        ("requests", i64::try_from(seen.len()).unwrap()),
        (
            "preserved",
            i64::from(std::fs::read(path).unwrap() == bytes),
        ),
        ("scoped", i64::from(scoped(&seen))),
    ]
}

pub(crate) fn row_identity_transition(state: &ModelState, action: &str) -> ModelState {
    let mut next = state.clone();
    match action {
        "Expected" => {
            next.insert("expected", state["expected"] + 1);
        }
        "Email" => {
            next.insert("email", state["email"] + 1);
        }
        "AccessEmail" => {
            next.insert("access_email", state["access_email"] + 1);
        }
        "Replace" => {
            next.insert("changed", 1);
            next.insert(
                "access_email",
                if state["access_email"] < 2 {
                    state["access_email"] + 1
                } else {
                    0
                },
            );
        }
        "Prepare" | "Poll" => {
            let polling = action == "Poll";
            next.insert("phase", if polling { 2 } else { 1 });
            if !polling {
                next.insert("prepared_email", state["email"]);
                next.insert("prepared_access", state["access_email"]);
            }
            let observed = observe(state, polling, 0);
            for variant in 1..=2 {
                assert_eq!(
                    observed,
                    observe(state, polling, variant),
                    "variant={variant}: {state:?}"
                );
            }
            for (field, value) in observed {
                next.insert(field, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn named_row_checks_each_credential_snapshot() {
    let model = aterm_models_harness::codex_row_identity_model();
    assert_eq!(model.mutant_dials(), (1..=8).collect::<Vec<_>>());
    let states = aterm_spec::interp::prove_and_catch(&model);
    assert!(states > 30);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = row_identity_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let prepared = model
        .successors("Prepare", &model.init_state())
        .pop()
        .unwrap();
    let good = model.successors("Poll", &prepared).pop().unwrap();
    for (field, law) in [
        ("prepared", "Preparation"),
        ("usage", "Usage"),
        ("requests", "Requests"),
        ("preserved", "Preservation"),
        ("scoped", "Scope"),
    ] {
        let mut corrupt = good.clone();
        corrupt.insert(field, 0);
        assert!(
            !model.check_invariant(law, &corrupt),
            "{law} ignored its negative control"
        );
    }
}

#[test]
fn named_usage_refuses_reloaded_and_refreshed_email_changes() {
    for case in 0..3 {
        let root = aterm_tempfile::tempdir().unwrap();
        let token = |email, nonce| {
            make_jwt(&json!({
                "email":email, "jti":nonce, "exp":NOW + 7200,
                "https://api.openai.com/auth":{"chatgpt_user_id":"same-user"}
            }))
        };
        let original = token("alice@example.test", "old");
        let replacement = token("bob@example.test", "replacement");
        let doc = json!({"auth_mode":"chatgpt", "owner_note":"retained", "tokens":{
            "id_token":original, "access_token":original, "refresh_token":"old-refresh"
        }});
        write_auth_doc(root.path(), &doc).unwrap();
        let before = std::fs::read(root.path().join(AUTH_FILE)).unwrap();
        let stored = load_stored_auth(root.path()).unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let endpoint = listener.local_addr().unwrap();
        let disk = root.path().to_path_buf();
        let old = original.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream).unwrap();
            if case == 2 {
                write_auth_doc(
                    &disk,
                    &json!({"auth_mode":"chatgpt", "tokens":{
                        "id_token":replacement, "access_token":replacement
                    }}),
                )
                .unwrap();
                send_http_json(&mut stream, "401 Unauthorized", &json!({}));
            } else {
                send_http_json(
                    &mut stream,
                    "200 OK",
                    &json!({
                        "id_token":if case == 1 { old } else { replacement.clone() },
                        "access_token":replacement, "refresh_token":"replacement-refresh"
                    }),
                );
            }
            let (mut extra, _) = listener.accept().unwrap();
            assert!(
                read_http_request(&mut extra).is_none(),
                "wrong identity caused another HTTP request"
            );
            request
        });
        let base = format!("http://{endpoint}");
        if case == 2 {
            assert!(
                read_usage_with_expected_email(
                    root.path(),
                    &format!("{base}/usage"),
                    &format!("{base}/oauth/token"),
                    Duration::from_secs(60),
                    NOW,
                    Some("alice@example.test")
                )
                .is_none()
            );
        } else {
            assert!(
                refresh_stored_auth(
                    root.path(),
                    &stored,
                    &format!("{base}/oauth/token"),
                    Duration::from_secs(60),
                    NOW,
                    Some("alice@example.test")
                )
                .is_none()
            );
            assert_eq!(std::fs::read(root.path().join(AUTH_FILE)).unwrap(), before);
        }
        let _ = TcpStream::connect(endpoint);
        let request = server.join().unwrap();
        if case == 2 {
            assert_eq!(request.method, "GET");
            assert!(
                request
                    .headers
                    .contains(&("authorization".into(), format!("Bearer {original}")))
            );
            assert_eq!(
                read_account_email(root.path()).as_deref(),
                Some("bob@example.test")
            );
        } else {
            assert_eq!(request.method, "POST");
        }
    }
}
