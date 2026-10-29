// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State as ModelState;

const NOW: i64 = 1_790_000_000;

fn represented_read(state: &ModelState, variant: u8) -> [(&'static str, i64); 8] {
    let home = aterm_tempfile::tempdir().unwrap();
    let credential_home = home
        .path()
        .join(if variant == 14 { ".codex" } else { "login" });
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let issuer = format!("http://{address}");
    let token = |user: i64, email: &str| {
        let mut auth = json!({"chatgpt_account_id":"claimed-workspace", "chatgpt_plan_type":"pro"});
        if user != 0 {
            let name = if variant == 3 {
                if user == 1 { "User-A" } else { "user-a" }
            } else if user == 1 {
                "user-a"
            } else {
                "user-b"
            };
            let name = if variant == 2 {
                format!(" {name} ")
            } else {
                name.into()
            };
            let legacy = variant == 1
                || (variant == 13 && email == "access@example.test")
                || (variant == 16 && email == "identity@example.test")
                || variant == 15;
            if variant == 15 {
                auth.as_object_mut()
                    .unwrap()
                    .insert("chatgpt_user_id".into(), json!(" \t "));
            }
            auth.as_object_mut().unwrap().insert(
                if legacy { "user_id" } else { "chatgpt_user_id" }.into(),
                json!(name),
            );
        } else if variant == 2 {
            auth.as_object_mut()
                .unwrap()
                .insert("chatgpt_user_id".into(), json!(" \t "));
        }
        let mut claims = json!({
            "exp": if variant == 12 { NOW - 1 } else { NOW + 7200 },
            "https://api.openai.com/auth":auth,
        });
        if variant != 9 {
            claims
                .as_object_mut()
                .unwrap()
                .insert("email".into(), json!(email));
        }
        make_jwt(&claims)
    };
    let decoded = state["decoded"] == 1;
    let id_token = if !decoded || variant == 6 {
        "opaque-id".into()
    } else {
        token(state["id"], "identity@example.test")
    };
    let access_token = if !decoded || variant == 4 {
        "opaque-access".into()
    } else {
        token(state["access"], "access@example.test")
    };
    let mut doc = json!({
        "auth_mode": if variant == 7 { "chatgptAuthTokens" } else { "chatgpt" },
        "owner_note":"keep this independent metadata",
        "tokens": {
            "id_token":id_token, "access_token":access_token,
            "refresh_token":"retained-refresh", "account_id":"selected-workspace",
            "login_owner_id":"retained-owner",
            "oauth_provenance":{"issuer":issuer, "client_id":CLIENT_ID}
        }
    });
    {
        let tokens = doc
            .as_object_mut()
            .unwrap()
            .get_mut("tokens")
            .unwrap()
            .as_object_mut()
            .unwrap();
        if variant == 5 {
            tokens.remove("id_token");
        }
        if variant == 17 {
            tokens.insert("id_token".into(), json!(17));
        }
        if variant == 11 {
            tokens.remove("account_id");
        }
    }
    if variant == 8 {
        doc.as_object_mut().unwrap().remove("auth_mode");
    }
    write_auth_doc(&credential_home, &doc).unwrap();
    let path = credential_home.join(AUTH_FILE);
    let input = if variant == 10 {
        path.as_path()
    } else {
        credential_home.as_path()
    };
    let before = std::fs::read(&path).unwrap();
    let unchanged = || std::fs::read(&path).is_ok_and(|bytes| bytes == before);
    let admitted = load_stored_auth(input).is_some();
    let identity = read_account_in_home(input);
    let email = read_account_email(input);
    let row = crate::harness::accounts::Row {
        label: "candidate".into(),
        dir: (variant != 14).then(|| credential_home.to_string_lossy().into_owned()),
        ..crate::harness::accounts::Row::default()
    };
    let prepared = crate::harness::accounts_pool::login_env(
        crate::ai::connector::Cli::Codex,
        &row,
        home.path(),
    );
    let environment_ok = |env: &crate::harness::accounts_pool::LoginEnv| {
        env.label == row.label
            && env.var == "CODEX_AUTH_PATH"
            && env.value == (variant != 14).then(|| path.to_string_lossy().into_owned())
    };
    let preparation_details = match &prepared {
        Ok(env) => environment_ok(env),
        Err(why) => {
            why.kind() == std::io::ErrorKind::InvalidData
                && why.to_string().contains("different users")
        }
    };
    let expected = decoded.then(|| Account {
        email: (variant != 9).then(|| {
            if matches!(variant, 5 | 6 | 17) {
                "access@example.test"
            } else {
                "identity@example.test"
            }
            .into()
        }),
        plan: Some("pro".into()),
    });
    let details_ok = |got: &Option<Account>, email: &Option<String>| match got {
        None => email.is_none(),
        Some(got) => expected.as_ref() == Some(got) && email == &got.email,
    };
    let server = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for _ in 0..4 {
            let (mut stream, _) = listener.accept().unwrap();
            let Some(request) = read_http_request(&mut stream) else {
                break;
            };
            seen.push(request);
            send_http_json(
                &mut stream,
                "200 OK",
                &json!({
                    "rate_limit": {
                        "allowed":true, "limit_reached":false,
                        "primary_window":{"used_percent":25.0, "limit_window_seconds":18000, "reset_at":NOW + 18000}
                    }
                }),
            );
        }
        seen
    });
    let usage = read_usage_with(
        input,
        &format!("{issuer}/usage"),
        &format!("{issuer}/oauth/token"),
        Duration::from_secs(60),
        NOW,
    );
    // Wake the mock listener without an HTTP request after the synchronous reader ends.
    let _ = TcpStream::connect(address);
    let seen = server.join().unwrap();
    let expected_workspace = if variant == 11 {
        decoded.then_some("claimed-workspace")
    } else {
        Some("selected-workspace")
    };
    let scoped = |requests: &[RecordedHttp]| {
        requests.iter().all(|request| {
            let headers = |name: &str| {
                request
                    .headers
                    .iter()
                    .filter(|(key, _)| key == name)
                    .map(|(_, value)| value.as_str())
                    .collect::<Vec<_>>()
            };
            request.method == "GET"
                && request.target == "/usage"
                && request.body.is_empty()
                && headers("authorization") == vec![format!("Bearer {access_token}").as_str()]
                && headers("chatgpt-account-id")
                    == expected_workspace.into_iter().collect::<Vec<_>>()
        })
    };
    let preserved = unchanged();
    let request_scope = scoped(&seen);
    let details = details_ok(&identity, &email) && preparation_details;
    if state["id"] == 1 && state["access"] == 2 && variant == 0 {
        let mut corrupt = doc.clone();
        corrupt
            .as_object_mut()
            .unwrap()
            .insert("owner_note".into(), json!("changed independent metadata"));
        write_auth_doc(&credential_home, &corrupt).unwrap();
        assert!(
            !unchanged(),
            "file observer missed a valid changed document"
        );
        std::fs::write(&path, &before).unwrap();
        assert!(unchanged());
    }
    if state["id"] == 1 && state["access"] == 1 && variant == 0 {
        let mut bad = seen.clone();
        bad[0]
            .headers
            .iter_mut()
            .find(|(key, _)| key == "chatgpt-account-id")
            .unwrap()
            .1 = "other-workspace".into();
        assert!(!scoped(&bad), "request observer missed wrong workspace");
        let mut bad_identity = identity.clone().unwrap();
        bad_identity.email = Some("wrong@example.test".into());
        assert!(
            !details_ok(&Some(bad_identity), &email),
            "identity observer missed wrong email"
        );
        let mut bad_environment = prepared.as_ref().unwrap().clone();
        bad_environment.value = Some("wrong-login".into());
        assert!(
            !environment_ok(&bad_environment),
            "environment observer missed wrong login"
        );
    }
    [
        ("admitted", i64::from(admitted)),
        ("identity", i64::from(identity.is_some())),
        ("usage", i64::from(usage.is_some())),
        ("requests", i64::try_from(seen.len()).unwrap()),
        ("prepared", i64::from(prepared.is_ok())),
        ("preserved", i64::from(preserved)),
        ("scoped", i64::from(request_scope)),
        ("details", i64::from(details)),
    ]
}

fn read_projection(state: &ModelState) -> [(&'static str, i64); 8] {
    let mut first = None;
    for variant in 0..=17 {
        if state["decoded"] == 0 && !matches!(variant, 0 | 7 | 8 | 10 | 11 | 14) {
            continue;
        }
        if variant == 4 && state["access"] != 0 {
            continue;
        }
        if matches!(variant, 5 | 6 | 17) && state["id"] != 0 {
            continue;
        }
        if variant == 12
            && (state["id"] == 0 || state["access"] == 0 || state["id"] == state["access"])
        {
            continue;
        }
        let result = represented_read(state, variant);
        if let Some(first) = first {
            assert_eq!(result, first, "variant {variant}: {state:?}");
        } else {
            first = Some(result);
        }
    }
    first.unwrap()
}

pub(crate) fn stored_identity_transition(state: &ModelState, action: &str) -> ModelState {
    let mut next = state.clone();
    match action {
        "AdvanceId" => {
            next.insert("id", state["id"] + 1);
        }
        "AdvanceAccess" => {
            next.insert("access", state["access"] + 1);
        }
        "Opaque" => {
            next.insert("decoded", 0);
        }
        "Read" => {
            next.insert("phase", 1);
            for (key, value) in read_projection(state) {
                next.insert(key, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn stored_credentials_conform_before_identity_and_usage_reads() {
    codex_stored_identity_environment();
    let model = aterm_models_harness::codex_stored_identity_model();
    assert_eq!(model.mutant_dials(), (1..=11).collect::<Vec<_>>());
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 20);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = stored_identity_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let good = model.successors("Read", &model.init_state()).pop().unwrap();
    for (field, value, law) in [
        ("admitted", 0, "Admission"),
        ("identity", 0, "Identity"),
        ("usage", 0, "Usage"),
        ("requests", 0, "Requests"),
        ("prepared", 0, "Preparation"),
        ("preserved", 0, "Evidence"),
        ("scoped", 0, "Scope"),
        ("details", 0, "Details"),
    ] {
        let mut bad = good.clone();
        bad.insert(field, value);
        assert!(
            !model.check_invariant(law, &bad),
            "{law} missed its negative control"
        );
    }
}
