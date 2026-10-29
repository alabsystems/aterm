// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure-Rust tests for Codex's native loopback PKCE sign-in, JWT `auth.json` account reader,
//! `/oauth/token` refresh and `/wham/usage` rate-limit reader over in-process `127.0.0.1:0`
//! sockets.

use std::cell::RefCell;
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::*;

#[path = "codex_stored_identity_tests.rs"]
mod stored_identity_tests;

#[path = "codex_row_identity_tests.rs"]
mod row_identity_tests;

const HOSTS: &[&str] = &["auth.openai.com"];

pub(crate) fn make_jwt(claims: &Value) -> String {
    let header = base64url_encode(br#"{"alg":"RS256","typ":"JWT"}"#).expect("header");
    let body = aterm_json::to_vec(claims).expect("claims json");
    let payload = base64url_encode(&body).expect("payload");
    format!("{header}.{payload}.sig")
}

pub(crate) fn make_chatgpt_jwt(email: Option<&str>, plan: &str, exp: i64) -> String {
    let mut claims = json!({
        "exp": exp,
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": plan,
            "chatgpt_account_id": "acct_native_test",
            "chatgpt_user_id": "user_native_test",
        }
    });
    if let Some(email) = email
        && let Some(obj) = claims.as_object_mut()
    {
        obj.insert("email".to_owned(), Value::String(email.to_owned()));
    }
    make_jwt(&claims)
}

#[derive(Debug, Clone)]
struct RecordedHttp {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: String,
}

fn read_http_request(stream: &mut TcpStream) -> Option<RecordedHttp> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    let header_end = loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
    };
    let head = std::str::from_utf8(&buf[..header_end]).ok()?;
    let mut lines = head.split("\r\n");
    let req_line = lines.next()?;
    let mut words = req_line.split_whitespace();
    let method = words.next()?.to_owned();
    let target = words.next()?.to_owned();
    let mut headers = Vec::new();
    let mut content_len = 0usize;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim().to_owned();
            if key == "content-length" {
                content_len = val.parse().unwrap_or(0);
            }
            headers.push((key, val));
        }
    }
    let body_start = header_end + 4;
    while buf.len() < body_start + content_len {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body_end = (body_start + content_len).min(buf.len());
    let body = String::from_utf8_lossy(&buf[body_start..body_end]).into_owned();
    Some(RecordedHttp {
        method,
        target,
        headers,
        body,
    })
}

fn send_http_json(stream: &mut TcpStream, status: &str, body: &Value) {
    let payload = aterm_json::to_string(body).expect("json response");
    let wire = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    let _ = stream.write_all(wire.as_bytes());
    let _ = stream.flush();
}

fn raw_get_loopback(url: &str) -> (u16, String) {
    let rest = url.strip_prefix("http://").expect("http url");
    let (host_port, path_and_query) = rest.split_once('/').expect("path");
    let mut stream = TcpStream::connect(host_port).expect("connect callback");
    let _ = stream.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN));
    let req =
        format!("GET /{path_and_query} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).expect("write callback");
    let mut resp = String::new();
    let _ = stream.read_to_string(&mut resp);
    let status = resp
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    (status, resp)
}

fn query_map(url: &str) -> Vec<(String, String)> {
    let (_, query) = url.split_once('?').unwrap_or((url, ""));
    parse_query_params(query)
}

/// Runs a complete in-process loopback PKCE login in `home` against a mock `127.0.0.1:0` token
/// server issuing a ChatGPT JWT for `signed_in_email`, delegating the browser handoff to `inner`
/// and delivering the loopback `/auth/callback` once `inner` approves.
pub(crate) fn mock_login_in_home(
    home: &Path,
    signed_in_email: Option<&str>,
    inner: &mut dyn Approver,
    hosts: &[&str],
    expected_email: Option<&str>,
    deadline: Instant,
) -> Drove {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock token server");
    let port = listener.local_addr().expect("addr").port();
    let token_url = format!("http://127.0.0.1:{port}/oauth/token");
    let issued_email = signed_in_email.map(str::to_owned);
    let expected_challenge: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let challenge_for_server = Arc::clone(&expected_challenge);

    let server = std::thread::spawn(move || {
        let _ = listener.set_nonblocking(true);
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let Some(req) = read_http_request(&mut stream) else {
                        return;
                    };
                    let form = parse_query_params(&req.body);
                    let get = |k: &str| {
                        form.iter()
                            .find(|(fk, _)| fk == k)
                            .map(|(_, fv)| fv.as_str())
                    };
                    assert_eq!(get("grant_type"), Some("authorization_code"));
                    assert_eq!(get("client_id"), Some(CLIENT_ID));
                    assert_eq!(get("code"), Some("auth_code_ok"));
                    let verifier = get("code_verifier").expect("code_verifier");
                    let digest = aterm_digest::Sha256::digest(verifier.as_bytes());
                    let computed = base64url_encode(&digest).expect("challenge");
                    if let Some(expected) = challenge_for_server.lock().expect("lock").as_deref() {
                        assert_eq!(
                            computed, expected,
                            "PKCE S256 verifier must match challenge"
                        );
                    }
                    let jwt = make_chatgpt_jwt(issued_email.as_deref(), "pro", 1_900_000_000);
                    send_http_json(
                        &mut stream,
                        "200 OK",
                        &json!({
                            "id_token": jwt,
                            "access_token": jwt,
                            "refresh_token": "rt_issued_1",
                        }),
                    );
                    return;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => return,
            }
        }
    });

    struct CallbackDriver<'a> {
        inner: &'a mut dyn Approver,
        challenge: Arc<Mutex<Option<String>>>,
    }

    impl Approver for CallbackDriver<'_> {
        fn approve(&mut self, handoff: &Handoff<'_>) -> Approval {
            let decision = self.inner.approve(handoff);
            if matches!(decision, Approval::Approved { .. }) {
                let params = query_map(handoff.url);
                let get = |k: &str| {
                    params
                        .iter()
                        .find(|(pk, _)| pk == k)
                        .map(|(_, pv)| pv.clone())
                };
                *self.challenge.lock().expect("lock") = get("code_challenge");
                let redirect = get("redirect_uri").expect("redirect_uri");
                let state = get("state").expect("state");
                let (status, _) =
                    raw_get_loopback(&format!("{redirect}?code=auth_code_ok&state={state}"));
                assert_eq!(status, 200);
                let start = Instant::now();
                while !(handoff.finished)() && start.elapsed() < Duration::from_secs(5) {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            decision
        }
    }

    let endpoints = Endpoints {
        issuer: DEFAULT_ISSUER.to_owned(),
        token_url,
        callback_ports: vec![0],
    };
    let mut driver = CallbackDriver {
        inner,
        challenge: expected_challenge,
    };
    let drove = login_with(
        home,
        &endpoints,
        &mut driver,
        hosts,
        expected_email,
        deadline,
    );
    let _ = server.join();
    drove
}

type Handed = Vec<(Option<String>, Option<String>, Completion)>;

#[derive(Default)]
struct Seen {
    handed: RefCell<Handed>,
}

impl Approver for &Seen {
    fn approve(&mut self, handoff: &Handoff<'_>) -> Approval {
        self.handed.borrow_mut().push((
            https_host(handoff.url),
            handoff.code.map(str::to_owned),
            handoff.completion,
        ));
        Approval::Approved { page: false }
    }
}

#[test]
fn native_pkce_login_verifies_s256_writes_private_auth_json_and_reads_account() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("codex-login");
    let seen = Seen::default();
    let drove = mock_login_in_home(
        &home,
        Some("codex-one@example.com"),
        &mut &seen,
        HOSTS,
        Some("codex-one@example.com"),
        Instant::now() + Duration::from_secs(60),
    );
    assert_eq!(
        drove,
        Drove {
            outcome: Outcome::SignedIn {
                email: Some("codex-one@example.com".to_owned()),
                page: false,
            },
            host: Some("auth.openai.com".to_owned()),
        }
    );
    assert_eq!(
        *seen.handed.borrow(),
        [(
            Some("auth.openai.com".to_owned()),
            None,
            Completion::Loopback
        )]
    );
    assert_eq!(
        drove.why(crate::ai::connector::Flow::Browser),
        "auth.openai.com browser signed-in",
        "the ledger's line keeps no URL, state or email"
    );
    let auth_path = home.join("auth.json");
    assert!(auth_path.is_file(), "auth.json written directly");
    let saved = load_stored_auth(&home).unwrap();
    assert!(
        saved.doc["tokens"]
            .get("login_owner_id")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty()),
        "each completed native login needs an owner ID"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&auth_path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "auth.json must be owner-only 0600");
    }
    assert_eq!(
        read_account_in_home(&home),
        Some(Account {
            email: Some("codex-one@example.com".to_owned()),
            plan: Some("pro".to_owned()),
        })
    );
    assert_eq!(
        read_account_email(&home).as_deref(),
        Some("codex-one@example.com")
    );
    let mut entries: Vec<_> = std::fs::read_dir(&home)
        .expect("read_dir")
        .filter_map(Result::ok)
        .map(|e| e.file_name())
        .collect();
    entries.sort();
    assert_eq!(
        entries,
        [
            std::ffi::OsString::from("auth.json"),
            std::ffi::OsString::from("auth.json.lock"),
        ],
        "no SQLite files created in login directory"
    );

    // Passing a direct `CODEX_AUTH_PATH` `.json` file path before it exists writes the file
    // directly rather than creating `auth.json/auth.json`.
    let explicit_auth = tmp.path().join("explicit-slot").join("auth.json");
    let drove_explicit = mock_login_in_home(
        &explicit_auth,
        Some("codex-two@example.com"),
        &mut &seen,
        HOSTS,
        Some("codex-two@example.com"),
        Instant::now() + Duration::from_secs(60),
    );
    assert!(matches!(drove_explicit.outcome, Outcome::SignedIn { .. }));
    assert!(explicit_auth.is_file());
    assert_eq!(
        read_account_email(&explicit_auth).as_deref(),
        Some("codex-two@example.com")
    );
}

fn login_case(case: i64) -> [(&'static str, i64); 6] {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let original = json!({"auth_mode":"chatgpt", "marker":"keep-on-refusal", "tokens":{
        "access_token":"keep-original", "refresh_token":"keep-refresh",
        "login_owner_id":"previous-owner",
        "oauth_provenance":{"issuer":"https://old.example", "client_id":"old-client"}
    }});
    write_auth_doc(tmp.path(), &original).unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let issuer = format!("http://127.0.0.1:{port}/tenant");
    let endpoints = Endpoints {
        issuer: match case {
            10 => DEFAULT_ISSUER.to_owned(),
            11 => format!("{issuer}///"),
            12 => format!("{issuer}?query"),
            _ => issuer.clone(),
        },
        token_url: if case == 21 {
            "http://192.0.2.1/oauth/token".to_owned()
        } else {
            format!("{issuer}/oauth/token")
        },
        callback_ports: vec![],
    };
    let seen = Arc::new(Mutex::new(Vec::new()));
    let server_seen = Arc::clone(&seen);
    let server_issuer = issuer.clone();
    let server = std::thread::spawn(move || {
        for round in 0..if case == 17 { 2 } else { 1 } {
            let (mut stream, _) = listener.accept().unwrap();
            let Some(request) = read_http_request(&mut stream) else {
                break;
            };
            let jwt = |user: Option<&str>| {
                make_jwt(&json!({
                    "iss":if case == 13 {"https://untrusted.example"} else {server_issuer.as_str()},
                    "jti":round, "email":"owner@example.test",
                    "https://api.openai.com/auth":{"chatgpt_user_id":user, "chatgpt_account_id":"workspace-one"}
                }))
            };
            let id_token = if case == 22 {
                make_jwt(&Value::Null)
            } else if case == 2 || case == 14 {
                "invalid".to_owned()
            } else {
                jwt(if case == 4 || case == 5 {
                    None
                } else {
                    Some("user-a")
                })
            };
            let access_token = if case == 23 {
                make_jwt(&json!([]))
            } else if case == 2 || case == 15 {
                "invalid".to_owned()
            } else {
                jwt(if case == 1 {
                    Some("user-b")
                } else if case == 3 || case == 5 {
                    None
                } else {
                    Some("user-a")
                })
            };
            let mut payload = json!({
                "id_token":id_token, "access_token":access_token, "refresh_token":"new-refresh"
            });
            if case == 6 {
                payload.as_object_mut().unwrap().remove("id_token");
            }
            if case == 7 {
                payload.as_object_mut().unwrap().remove("access_token");
            }
            if case == 8 {
                payload.as_object_mut().unwrap().remove("refresh_token");
            }
            if case == 16 {
                payload
                    .as_object_mut()
                    .unwrap()
                    .insert("login_owner_id".into(), json!("forged-owner"));
                payload.as_object_mut().unwrap().insert(
                    "oauth_provenance".into(),
                    json!({"issuer":"https://forged.example", "client_id":"forged-client"}),
                );
            }
            server_seen.lock().unwrap().push(request);
            send_http_json(
                &mut stream,
                if case == 9 {
                    "400 Bad Request"
                } else {
                    "200 OK"
                },
                &payload,
            );
        }
    });
    let before = std::fs::read(tmp.path().join(AUTH_FILE)).unwrap();
    let wait = if case == 20 {
        Duration::ZERO
    } else {
        Duration::from_secs(60)
    };
    let exchange = || CodeExchange {
        home: tmp.path(),
        endpoints: &endpoints,
        redirect_uri: "http://127.0.0.1/callback",
        code: "code",
        code_verifier: "verifier",
        wait,
        now: 1_790_000_000,
    };
    let perform = || {
        exchange_code_and_persist(
            tmp.path(),
            &endpoints,
            "http://127.0.0.1/callback",
            "code",
            "verifier",
            wait,
            1_790_000_000,
        )
    };
    let mut result = if case == 18 {
        exchange().persist(Some(""))
    } else if case == 19 {
        exchange().persist(None)
    } else {
        perform()
    };
    let first = result.as_ref().and_then(|_| load_stored_auth(tmp.path()));
    if case == 17 {
        result = perform();
    }
    let _ = TcpStream::connect(("127.0.0.1", port));
    server.join().unwrap();
    let after = result.as_ref().and_then(|_| load_stored_auth(tmp.path()));
    let owner = |stored: &StoredAuth| {
        stored.doc["tokens"]
            .get("login_owner_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let mut fresh = after.as_ref().is_none_or(|stored| {
        owner(stored).is_some_and(|owner| {
            owner != "previous-owner"
                && owner != "forged-owner"
                && owner.len() == 43
                && base64url_decode(&owner).is_some_and(|b| b.len() == 32)
        })
    });
    if case == 17 {
        let (first, after) = (first.as_ref().unwrap(), after.as_ref().unwrap());
        fresh &= owner(first) != owner(after)
            && first.access_token != after.access_token
            && first.account_id == after.account_id
            && updated_disk_auth(first, after.clone()).is_none();
    }
    let bound = after.as_ref().is_none_or(|stored| {
        if case == 10 || case == 12 {
            stored.doc["tokens"].get("oauth_provenance").is_none()
        } else {
            stored.doc["tokens"].get("oauth_provenance")
                == Some(&json!({
                    "issuer":endpoints.issuer, "client_id":CLIENT_ID
                }))
                && refresh_authority(stored, "invalid fallback").is_some_and(|authority| {
                    authority.token_url == endpoints.token_url && authority.client_id == CLIENT_ID
                })
        }
    });
    let kept = result.is_some() || std::fs::read(tmp.path().join(AUTH_FILE)).unwrap() == before;
    let seen = seen.lock().unwrap();
    for request in seen.iter() {
        assert_eq!(request.method, "POST");
        assert_eq!(request.target, "/tenant/oauth/token");
        let form = parse_query_params(&request.body);
        assert!(form.contains(&("client_id".into(), CLIENT_ID.into())));
    }
    assert_eq!(
        seen.len(),
        if (18..=21).contains(&case) {
            0
        } else if case == 17 {
            2
        } else {
            1
        }
    );
    [
        ("phase", 1),
        ("result", i64::from(result.is_some())),
        ("posted", i64::from(!seen.is_empty())),
        ("kept", i64::from(kept)),
        ("fresh", i64::from(fresh)),
        ("bound", i64::from(bound)),
    ]
}

pub(crate) fn login_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Exchange" => {
            for (key, value) in login_case(state["case"]) {
                next.insert(key, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn codex_login_publication_conforms_and_rejects_conflicting_token_users() {
    super::codex_login_publication_environment();
    let model = aterm_models_harness::codex_login_publication_model();
    assert_eq!(model.mutant_dials(), (1..=6).collect::<Vec<_>>());
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 48);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = login_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let good = login_transition(&model.init_state(), "Exchange");
    let mut refused_case = model.init_state();
    refused_case.insert("case", 1);
    let refused = login_transition(&refused_case, "Exchange");
    for (key, value, law) in [
        ("result", 0, "Admission"),
        ("posted", 0, "RequestBoundary"),
        ("kept", 0, "PreserveRejected"),
        ("fresh", 0, "FreshOwner"),
        ("bound", 0, "Provenance"),
    ] {
        let mut bad = if law == "PreserveRejected" {
            refused.clone()
        } else {
            good.clone()
        };
        bad.insert(key, value);
        assert!(!model.check_invariant(law, &bad));
    }
}

#[test]
fn off_pin_issuer_state_mismatch_provider_error_and_refusal_are_enforced() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");

    // 1. Off-pin issuer is rejected before binding or handing any URL to the approver.
    let home_off = tmp.path().join("off-pin");
    let seen = Seen::default();
    let off_endpoints = Endpoints {
        issuer: "https://auth.openai.com.example.net".to_owned(),
        token_url: "http://127.0.0.1:9/oauth/token".to_owned(),
        callback_ports: vec![0],
    };
    let drove = login_with(
        &home_off,
        &off_endpoints,
        &mut &seen,
        HOSTS,
        Some("codex-one@example.com"),
        Instant::now() + Duration::from_secs(60),
    );
    assert_eq!(drove.outcome, Outcome::OffPin);
    assert!(seen.handed.borrow().is_empty(), "nothing was opened");
    assert!(!home_off.exists());

    // 2. Wrong `state` returns 400 without marking `finished()`, while provider `error` with
    //    matching `state` marks `finished()` and yields `Outcome::Failed`.
    struct Probe<F>(F);
    impl<F: FnMut(&Handoff<'_>) -> Approval> Approver for Probe<F> {
        fn approve(&mut self, handoff: &Handoff<'_>) -> Approval {
            (self.0)(handoff)
        }
    }

    let home_err = tmp.path().join("provider-err");
    let mut bad_state_finished = true;
    let mut matching_err_finished = false;
    let mut probe = Probe(|handoff: &Handoff<'_>| {
        let params = query_map(handoff.url);
        let get = |k: &str| {
            params
                .iter()
                .find(|(pk, _)| pk == k)
                .map(|(_, pv)| pv.clone())
        };
        let redirect = get("redirect_uri").expect("redirect_uri");
        let state = get("state").expect("state");
        let host_port = redirect
            .strip_prefix("http://")
            .and_then(|s| s.split_once('/'))
            .map(|(hp, _)| hp)
            .expect("host_port");
        {
            let mut rebind = TcpStream::connect(host_port).expect("connect rebind");
            let _ = rebind.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN));
            let mut payload =
                format!("GET /auth/callback?code=forged&state={state} HTTP/1.1\r\nX-Pad: ")
                    .into_bytes();
            payload.extend_from_slice(b"\xff\r\nHost: evil.example\r\nConnection: close\r\n\r\n");
            rebind.write_all(&payload).expect("write rebind");
            let mut resp = String::new();
            let _ = rebind.read_to_string(&mut resp);
            assert!(resp.starts_with("HTTP/1.1 400 "), "{resp:?}");
            assert!(!(handoff.finished)());
        }
        let (bad_status, _) =
            raw_get_loopback(&format!("{redirect}?code=forged&state=wrong_state_value"));
        assert_eq!(bad_status, 400);
        let (dup_state_status, _) =
            raw_get_loopback(&format!("{redirect}?code=forged&state={state}&state=extra"));
        assert_eq!(dup_state_status, 400);
        let (bad_hex_status, _) =
            raw_get_loopback(&format!("{redirect}?code=bad%ZZ&state={state}"));
        assert_eq!(bad_hex_status, 400);
        bad_state_finished = (handoff.finished)();

        let (err_status, _) =
            raw_get_loopback(&format!("{redirect}?error=access_denied&state={state}"));
        assert_eq!(err_status, 200);
        let start = Instant::now();
        while !(handoff.finished)() && start.elapsed() < aterm_spec::wait::MUST_HAPPEN {
            std::thread::sleep(Duration::from_millis(5));
        }
        matching_err_finished = (handoff.finished)();
        Approval::Approved { page: true }
    });
    let err_endpoints = Endpoints {
        issuer: DEFAULT_ISSUER.to_owned(),
        token_url: "http://127.0.0.1:9/oauth/token".to_owned(),
        callback_ports: vec![0],
    };
    let drove = login_with(
        &home_err,
        &err_endpoints,
        &mut probe,
        HOSTS,
        Some("codex-one@example.com"),
        Instant::now() + Duration::from_secs(60),
    );
    assert!(
        !bad_state_finished,
        "CSRF state mismatch must not mark handoff finished"
    );
    assert!(
        matching_err_finished,
        "matching state error callback marks handoff finished"
    );
    assert_eq!(drove.outcome, Outcome::Failed);
    assert!(!home_err.join("auth.json").exists());

    // 3. Approver refusal stops the listener cleanly and returns `Outcome::Refused`.
    let home_refused = tmp.path().join("refused");
    let mut refuse = Probe(|_handoff: &Handoff<'_>| Approval::Refused("policy refusal".to_owned()));
    let drove = login_with(
        &home_refused,
        &err_endpoints,
        &mut refuse,
        HOSTS,
        Some("codex-one@example.com"),
        Instant::now() + Duration::from_secs(60),
    );
    assert_eq!(drove.outcome, Outcome::Refused("policy refusal".to_owned()));
    assert!(!home_refused.exists());

    // 4. Next-line `Want::Url` in `Scan::feed` rejects lines containing spaces/control chars.
    let route = crate::ai::connector::CODEX_SIGNIN
        .route(Some(crate::ai::connector::Flow::Device))
        .expect("codex device route");
    let crate::ai::connector::Drive::Lines { grammar: g, .. } = &route.drive else {
        panic!("expected line route");
    };
    let mut bad_scan = crate::harness::login_drive::Scan::default();
    bad_scan.feed(g, "Follow these steps to sign in:");
    bad_scan.feed(
        g,
        "1. Open this link in your browser and sign in to your account",
    );
    bad_scan.feed(g, "   https://auth.openai.com/codex/device (copy link)");
    assert_eq!(
        bad_scan.url, None,
        "multi-word line after URL lead must not be captured as URL"
    );
    let mut good_scan = crate::harness::login_drive::Scan::default();
    good_scan.feed(
        g,
        "1. Open this link in your browser and sign in to your account",
    );
    good_scan.feed(g, "   https://auth.openai.com/codex/device");
    assert_eq!(
        good_scan.url.as_deref(),
        Some("https://auth.openai.com/codex/device")
    );
}

#[test]
fn account_read_and_stored_jwt_accept_only_chatgpt_accounts() {
    let read: Value = aterm_json::from_str(
        r#"{"account":{"type":"chatgpt","email":"a@example.com","planType":"pro"}}"#,
    )
    .expect("json");
    assert_eq!(
        account_of(&read),
        Some(Account {
            email: Some("a@example.com".to_owned()),
            plan: Some("pro".to_owned()),
        })
    );
    let none: Value =
        aterm_json::from_str(r#"{"account":null,"requiresOpenaiAuth":true}"#).expect("json");
    assert_eq!(account_of(&none), None);
    let key: Value = aterm_json::from_str(r#"{"account":{"type":"apiKey"}}"#).expect("json");
    assert_eq!(account_of(&key), None);

    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let profile_jwt = make_jwt(&json!({
        "https://api.openai.com/profile": { "email": "profile@example.com" },
        "https://api.openai.com/auth": { "chatgpt_plan_type": "team" }
    }));
    let chatgpt_home = tmp.path().join("chatgpt");
    std::fs::create_dir_all(&chatgpt_home).expect("dir");
    std::fs::write(
        chatgpt_home.join("auth.json"),
        aterm_json::to_string(&json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": profile_jwt,
                "access_token": profile_jwt,
                "refresh_token": "rt_1"
            }
        }))
        .expect("json"),
    )
    .expect("write");
    assert_eq!(
        read_account_in_home(&chatgpt_home),
        Some(Account {
            email: Some("profile@example.com".to_owned()),
            plan: Some("team".to_owned()),
        })
    );

    let api_key_home = tmp.path().join("apikey");
    std::fs::create_dir_all(&api_key_home).expect("dir");
    std::fs::write(
        api_key_home.join("auth.json"),
        aterm_json::to_string(&json!({
            "auth_mode": "apikey",
            "OPENAI_API_KEY": "sk-test",
            "tokens": {
                "access_token": profile_jwt
            }
        }))
        .expect("json"),
    )
    .expect("write");
    assert_eq!(read_account_in_home(&api_key_home), None);
}

#[test]
fn native_wham_usage_reads_windows_walls_and_refreshes_expired_or_unauthorized_tokens() {
    use crate::harness::accounts::{SESSION_LIMIT, WEEK_LIMIT};
    let now = 1_790_000_000;

    assert_eq!(
        usage_url_from_base("https://chatgpt.com"),
        "https://chatgpt.com/backend-api/wham/usage"
    );
    assert_eq!(
        usage_url_from_base("https://chatgpt.com/backend-api/"),
        "https://chatgpt.com/backend-api/wham/usage"
    );
    assert_eq!(
        usage_url_from_base("https://codex.enterprise.example"),
        "https://codex.enterprise.example/api/codex/usage"
    );
    assert_eq!(
        usage_url_from_base("https://chatgpt.com.enterprise.example"),
        "https://chatgpt.com.enterprise.example/api/codex/usage",
        "lookalike suffix host must not be rewritten as chatgpt.com"
    );

    let backend_payload = json!({
        "plan_type": "pro",
        "rate_limit": {
            "allowed": true,
            "limit_reached": false,
            "primary_window": {
                "used_percent": 94.2,
                "limit_window_seconds": 18_000,
                "reset_at": 1_790_010_800
            },
            "secondary_window": {
                "used_percent": 31.0,
                "limit_window_seconds": 604_800,
                "reset_at": 1_790_259_200
            }
        }
    });

    // 1. Live HTTP `/wham/usage` + proactive expiry refresh + reactive 401 refresh.
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock backend");
    let port = listener.local_addr().expect("addr").port();
    let seen_requests: Arc<Mutex<Vec<RecordedHttp>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_for_server = Arc::clone(&seen_requests);
    let payload_for_server = backend_payload.clone();
    let refreshed_jwt = make_chatgpt_jwt(Some("codex-one@example.com"), "pro", now + 7200);
    let refreshed_for_server = refreshed_jwt.clone();

    let server = std::thread::spawn(move || {
        for _ in 0..6 {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let Some(req) = read_http_request(&mut stream) else {
                break;
            };
            seen_for_server.lock().expect("lock").push(req.clone());
            match (req.method.as_str(), req.target.as_str()) {
                ("POST", "/oauth/token") => {
                    send_http_json(
                        &mut stream,
                        "200 OK",
                        &json!({
                            "id_token": refreshed_for_server,
                            "access_token": refreshed_for_server,
                            "refresh_token": "rt_rotated_2",
                        }),
                    );
                }
                ("GET", "/backend-api/wham/usage") => {
                    let auth = req
                        .headers
                        .iter()
                        .find(|(k, _)| k == "authorization")
                        .map(|(_, v)| v.as_str())
                        .unwrap_or("");
                    if auth.ends_with(".revoked_token") {
                        send_http_json(
                            &mut stream,
                            "401 Unauthorized",
                            &json!({"error": "revoked"}),
                        );
                    } else {
                        send_http_json(&mut stream, "200 OK", &payload_for_server);
                    }
                }
                _ => send_http_json(&mut stream, "404 Not Found", &json!({})),
            }
        }
    });

    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let usage_url = format!("http://127.0.0.1:{port}/backend-api/wham/usage");
    let token_url = format!("http://127.0.0.1:{port}/oauth/token");

    // Case A: Fresh access token -> 1 GET, 0 refreshes.
    let home_fresh = tmp.path().join("fresh");
    std::fs::create_dir_all(&home_fresh).expect("dir");
    let fresh_jwt = make_chatgpt_jwt(Some("codex-one@example.com"), "pro", now + 3600);
    std::fs::write(
        home_fresh.join("auth.json"),
        aterm_json::to_string(&json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": fresh_jwt,
                "access_token": fresh_jwt,
                "refresh_token": "rt_fresh",
            },
            "last_refresh": "2026-10-25T00:00:00Z"
        }))
        .expect("json"),
    )
    .expect("write");
    let usage = read_usage_with(
        &home_fresh,
        &usage_url,
        &token_url,
        Duration::from_secs(10),
        now,
    )
    .expect("fresh usage");
    assert_eq!(usage.sample.session_pct, Some(95), "94.2% read up");
    assert_eq!(usage.sample.session_reset, Some(1_790_010_800));
    assert_eq!(usage.sample.week_pct, Some(31));
    assert_eq!(usage.sample.week_reset, Some(1_790_259_200));
    assert_eq!(usage.sample.at, now);
    assert!(usage.walls.is_empty());

    // Case B: Near-expiry access token -> proactive POST /oauth/token + GET /wham/usage, updating auth.json.
    let home_expiring = tmp.path().join("expiring");
    std::fs::create_dir_all(&home_expiring).expect("dir");
    let expiring_jwt = make_chatgpt_jwt(Some("codex-one@example.com"), "pro", now + 60);
    std::fs::write(
        home_expiring.join("auth.json"),
        aterm_json::to_string(&json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": expiring_jwt,
                "access_token": expiring_jwt,
                "refresh_token": "rt_expiring",
            }
        }))
        .expect("json"),
    )
    .expect("write");
    let usage_exp = read_usage_with(
        &home_expiring,
        &usage_url,
        &token_url,
        Duration::from_secs(10),
        now,
    )
    .expect("proactively refreshed usage");
    assert_eq!(usage_exp.sample.session_pct, Some(95));
    let persisted_exp =
        std::fs::read_to_string(home_expiring.join("auth.json")).expect("persisted");
    assert!(persisted_exp.contains("rt_rotated_2"), "{persisted_exp}");

    // Case C: Non-expired but revoked token -> 401 -> reactive POST /oauth/token -> retry GET -> 200.
    let home_revoked = tmp.path().join("revoked");
    std::fs::create_dir_all(&home_revoked).expect("dir");
    let revoked_header = base64url_encode(br#"{"alg":"RS256"}"#).expect("h");
    let revoked_payload = base64url_encode(
        aterm_json::to_string(&json!({"exp": now + 3600}))
            .unwrap()
            .as_bytes(),
    )
    .expect("p");
    let revoked_jwt = format!("{revoked_header}.{revoked_payload}.revoked_token");
    std::fs::write(
        home_revoked.join("auth.json"),
        aterm_json::to_string(&json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": fresh_jwt,
                "access_token": revoked_jwt,
                "refresh_token": "rt_revoked",
            }
        }))
        .expect("json"),
    )
    .expect("write");
    let usage_rev = read_usage_with(
        &home_revoked,
        &usage_url,
        &token_url,
        Duration::from_secs(10),
        now,
    )
    .expect("reactively refreshed usage");
    assert_eq!(usage_rev.sample.week_pct, Some(31));

    let _ = server.join();
    let methods: Vec<(String, String)> = seen_requests
        .lock()
        .expect("lock")
        .iter()
        .map(|r| (r.method.clone(), r.target.clone()))
        .collect();
    assert_eq!(
        methods,
        vec![
            ("GET".to_owned(), "/backend-api/wham/usage".to_owned()),
            ("POST".to_owned(), "/oauth/token".to_owned()),
            ("GET".to_owned(), "/backend-api/wham/usage".to_owned()),
            ("GET".to_owned(), "/backend-api/wham/usage".to_owned()),
            ("POST".to_owned(), "/oauth/token".to_owned()),
            ("GET".to_owned(), "/backend-api/wham/usage".to_owned()),
        ]
    );

    // 2. `usage_of` and `usage_from_backend` window and wall invariants (`SPECS.md` acc-codex-usage-of).
    let base_rpc = json!({
        "ordinaryUsageAllowed": true,
        "rateLimits": {
            "rateLimitReachedType": Value::Null,
            "primary": {
                "usedPercent": 94.2,
                "windowDurationMins": 300,
                "resetsAt": 1_790_010_800
            },
            "secondary": {
                "usedPercent": 31,
                "windowDurationMins": 10_080,
                "resetsAt": 1_790_259_200
            }
        }
    });
    let base_str = aterm_json::to_string(&base_rpc).expect("json");
    let with = |from: &str, to: &str| {
        assert!(base_str.contains(from), "{from}");
        let r: Value = aterm_json::from_str(&base_str.replacen(from, to, 1)).expect("json");
        usage_of(&r, now).expect("usage")
    };
    let full = with("\"usedPercent\":94.2", "\"usedPercent\":100.0");
    assert_eq!(full.walls, [(SESSION_LIMIT, Some(1_790_010_800))]);
    let week = with("\"usedPercent\":31", "\"usedPercent\":100");
    assert_eq!(week.walls, [(WEEK_LIMIT, Some(1_790_259_200))]);
    let refused = with(
        "\"ordinaryUsageAllowed\":true",
        "\"ordinaryUsageAllowed\":false",
    );
    assert_eq!(refused.walls, [(SESSION_LIMIT, Some(1_790_010_800))]);
    let reached = with(
        "\"rateLimitReachedType\":null",
        "\"rateLimitReachedType\":\"workspace_member_credits_depleted\"",
    );
    assert!(!reached.walls.is_empty());
    assert_eq!(usage_of(&json!({"accountId": "x"}), now), None);
    assert_eq!(usage_from_backend(&json!({"unexpected": true}), now), None);

    let spend_wall = usage_from_backend(
        &json!({
            "plan_type": "team",
            "spend_control": { "reached": true },
            "rate_limit": {
                "allowed": true,
                "primary_window": {
                    "used_percent": 20.0,
                    "limit_window_seconds": 18_000,
                    "reset_at": 1_790_010_800
                }
            }
        }),
        now,
    )
    .expect("spend wall");
    assert_eq!(spend_wall.walls, [(SESSION_LIMIT, Some(1_790_010_800))]);

    let week_only = usage_of(
        &json!({
            "ordinaryUsageAllowed": true,
            "rateLimits": {
                "rateLimitReachedType": Value::Null,
                "primary": {
                    "usedPercent": 0,
                    "windowDurationMins": 10_080,
                    "resetsAt": 1_793_427_434
                },
                "secondary": Value::Null
            }
        }),
        now,
    )
    .expect("week only");
    assert_eq!(
        (
            week_only.sample.session_pct,
            week_only.sample.week_pct,
            week_only.walls
        ),
        (None, Some(0), Vec::new())
    );

    // `reset_after_seconds` fallback computes `resetsAt = now + secs` when `reset_at` is absent.
    let reset_after = usage_from_backend(
        &json!({
            "plan_type": "pro",
            "rate_limit": {
                "allowed": true,
                "primary_window": {
                    "used_percent": 15.0,
                    "limit_window_seconds": 18_000,
                    "reset_after_seconds": 3_600
                }
            }
        }),
        now,
    )
    .expect("reset_after_seconds");
    assert_eq!(reset_after.sample.session_reset, Some(now + 3_600));

    // URI query decoding keeps `+` literal while decoding `%XX`, and Host validation rejects
    // non-loopback DNS-rebinding headers.
    assert_eq!(
        parse_query_params("code=abc+def%2Bghi&state=st%201"),
        vec![
            ("code".to_owned(), "abc+def+ghi".to_owned()),
            ("state".to_owned(), "st 1".to_owned()),
        ]
    );
    assert!(is_loopback_host_header("127.0.0.1:1455", 1455));
    assert!(is_loopback_host_header("localhost:1455", 1455));
    assert!(!is_loopback_host_header("127.0.0.1:9999", 1455));
    assert!(!is_loopback_host_header("evil.example:1455", 1455));
    assert_eq!(
        parse_callback_request_target(
            b"GET /auth/callback?code=c&state=s HTTP/1.1\r\nX-Bin: \xff\r\nHost: 127.0.0.1:1455\r\n\r\n",
            1455,
        )
        .as_deref(),
        Some("/auth/callback?code=c&state=s"),
    );
    // Missing Host, duplicate Host, non-UTF-8 byte smuggling before a DNS-rebinding Host,
    // obs-fold, CR smuggling, and unterminated oversized header blocks are all rejected.
    assert!(
        parse_callback_request_target(
            b"GET /auth/callback?code=c&state=s HTTP/1.1\r\nConnection: close\r\n\r\n",
            1455,
        )
        .is_none()
    );
    assert!(
        parse_callback_request_target(
            b"GET /auth/callback?code=c&state=s HTTP/1.1\r\nHost: 127.0.0.1:1455\r\nHost: evil.example:1455\r\n\r\n",
            1455,
        )
        .is_none()
    );
    assert!(
        parse_callback_request_target(
            b"GET /auth/callback?code=c&state=s HTTP/1.1\r\nX-Pad: \xff\r\nHost: evil.example:1455\r\n\r\n",
            1455,
        )
        .is_none()
    );
    assert!(
        parse_callback_request_target(
            b"GET /auth/callback?code=c&state=s HTTP/1.1\r\nHost: 127.0.0.1:1455\r\n : smuggled\r\n\r\n",
            1455,
        )
        .is_none()
    );
    assert!(
        parse_callback_request_target(
            b"GET /auth/callback?code=c&state=s HTTP/1.1\rHost: evil.example:1455\r\nHost: 127.0.0.1:1455\r\n\r\n",
            1455,
        )
        .is_none()
    );
    let mut unterminated = b"GET /auth/callback?code=c&state=s HTTP/1.1\r\nX-Pad: ".to_vec();
    unterminated.resize(CALLBACK_REQ_MAX, b'a');
    assert!(parse_callback_request_target(&unterminated, 1455).is_none());

    #[cfg(unix)]
    {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let real = tmp.path().join("real.json");
        std::fs::write(&real, "{}\n").expect("write real");
        let link = tmp.path().join("auth.json");
        std::os::unix::fs::symlink(&real, &link).expect("symlink auth");
        assert!(read_account_in_home(&link).is_none());
        assert!(write_auth_doc(&link, &json!({"auth_mode": "chatgpt"})).is_err());
    }
}

#[test]
fn concurrent_disk_rotation_reloads_before_refresh_and_cleartext_remote_endpoints_are_refused() {
    let now = 1_790_000_000;
    let tmp = aterm_tempfile::tempdir().expect("tempdir");

    // 1. Authority & transport guards block userinfo smuggling and non-loopback cleartext HTTP.
    assert!(is_loopback_endpoint("http://127.0.0.1:1455/wham/usage"));
    assert!(is_loopback_endpoint("http://[::1]:1455/wham/usage"));
    assert!(is_loopback_endpoint("http://localhost/wham/usage"));
    assert!(!is_loopback_endpoint(
        "http://127.0.0.1:80@evil.example/wham/usage"
    ));
    assert!(!is_loopback_endpoint(
        "http://localhost@evil.example/wham/usage"
    ));
    assert!(is_allowed_auth_endpoint(
        "https://chatgpt.com/backend-api/wham/usage"
    ));
    assert!(!is_allowed_auth_endpoint(
        "https://user@chatgpt.com/backend-api/wham/usage"
    ));
    assert!(!is_allowed_auth_endpoint(
        "http://evil.example/backend-api/wham/usage"
    ));
    assert!(!is_allowed_auth_endpoint(
        "https://chatgpt.com/backend-api?evil=1/wham/usage"
    ));
    assert!(!is_allowed_auth_endpoint(
        "https://chatgpt.com/backend-api#frag/wham/usage"
    ));
    assert!(!is_allowed_auth_endpoint(
        "https://chatgpt.com\\@evil.example/wham/usage"
    ));

    let slot_dir = tmp.path().join("slot");
    let user_home = tmp.path().join("user-home");
    std::fs::create_dir_all(&slot_dir).expect("slot_dir");
    std::fs::create_dir_all(user_home.join(".codex")).expect("user .codex");
    for bad_base in [
        "http://evil.example/backend-api",
        "https://chatgpt.com/backend-api?evil=1",
        "https://chatgpt.com/backend-api#frag",
        "https://chatgpt.com:99999/backend-api",
    ] {
        std::fs::write(
            slot_dir.join("config.toml"),
            format!("chatgpt_base_url = {bad_base:?}\n"),
        )
        .expect("bad config");
        assert_eq!(
            resolve_usage_url(&slot_dir.join("auth.json"), &user_home),
            "https://chatgpt.com/backend-api/wham/usage",
            "rejected chatgpt_base_url {bad_base:?} must fall back to HTTPS default"
        );
    }
    std::fs::write(
        slot_dir.join("config.toml"),
        "chatgpt_base_url = \"https://codex.enterprise.example\"\n",
    )
    .expect("https config");
    assert_eq!(
        resolve_usage_url(&slot_dir.join("auth.json"), &user_home),
        "https://codex.enterprise.example/api/codex/usage",
        "explicit auth.json path resolves sibling config.toml"
    );

    // 2. When a concurrent codex-alab session rotates `auth.json` on disk while `read_usage_with`
    //    gets 401 on the old access token, `read_usage_with` reloads `auth.json` from disk rather
    //    than replaying the stale single-use refresh token against `/oauth/token`.
    let auth_file = tmp.path().join("rotated").join("auth.json");
    let stale_jwt = format!(
        "{}.stale_sig",
        make_chatgpt_jwt(Some("codex-rot@example.com"), "pro", now + 3600)
            .rsplit_once('.')
            .expect("jwt")
            .0
    );
    let fresh_jwt = make_chatgpt_jwt(Some("codex-rot@example.com"), "pro", now + 7200);
    write_auth_doc(
        &auth_file,
        &json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": stale_jwt,
                "access_token": stale_jwt,
                "refresh_token": "rt_stale_already_used_by_codex_alab",
                "account_id": "acct_rot"
            },
            "last_refresh": "2026-10-25T00:00:00Z"
        }),
    )
    .expect("seed stale auth");

    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind mock");
    let port = listener.local_addr().expect("addr").port();
    let token_hits = Arc::new(Mutex::new(0usize));
    let token_hits_srv = Arc::clone(&token_hits);
    let auth_file_srv = auth_file.clone();
    let fresh_jwt_srv = fresh_jwt.clone();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let Some(req) = read_http_request(&mut stream) else {
                break;
            };
            match (req.method.as_str(), req.target.as_str()) {
                ("POST", "/oauth/token") => {
                    *token_hits_srv.lock().expect("lock") += 1;
                    send_http_json(
                        &mut stream,
                        "400 Bad Request",
                        &json!({"error": "refresh_token_reused"}),
                    );
                }
                ("GET", "/backend-api/wham/usage") => {
                    let auth = req
                        .headers
                        .iter()
                        .find(|(k, _)| k == "authorization")
                        .map(|(_, v)| v.as_str())
                        .unwrap_or("");
                    if auth.ends_with(".stale_sig") {
                        // Simulate codex-alab having atomically written the rotated token to disk.
                        write_auth_doc(
                            &auth_file_srv,
                            &json!({
                                "auth_mode": "chatgpt",
                                "tokens": {
                                    "id_token": fresh_jwt_srv,
                                    "access_token": fresh_jwt_srv,
                                    "refresh_token": "rt_fresh_from_codex_alab",
                                    "account_id": "acct_rot"
                                },
                                "last_refresh": "2026-10-26T00:00:00Z"
                            }),
                        )
                        .expect("rotate on disk");
                        send_http_json(
                            &mut stream,
                            "401 Unauthorized",
                            &json!({"error": "token_expired"}),
                        );
                    } else {
                        send_http_json(
                            &mut stream,
                            "200 OK",
                            &json!({
                                "plan_type": "pro",
                                "rate_limit": {
                                    "allowed": true,
                                    "primary_window": {
                                        "used_percent": 42.0,
                                        "limit_window_seconds": 18_000,
                                        "reset_at": 1_790_010_800
                                    }
                                }
                            }),
                        );
                    }
                }
                _ => send_http_json(&mut stream, "404 Not Found", &json!({})),
            }
        }
    });

    let usage_url = format!("http://127.0.0.1:{port}/backend-api/wham/usage");
    let token_url = format!("http://127.0.0.1:{port}/oauth/token");
    // Cleartext non-loopback URL is rejected before touching the network.
    assert!(
        read_usage_with(
            &auth_file,
            "http://evil.example/backend-api/wham/usage",
            &token_url,
            Duration::from_secs(5),
            now,
        )
        .is_none()
    );
    let got = read_usage_with(
        &auth_file,
        &usage_url,
        &token_url,
        Duration::from_secs(5),
        now,
    )
    .expect("reloaded rotated token from disk");
    let _ = server.join();
    assert_eq!(got.sample.session_pct, Some(42));
    assert_eq!(
        *token_hits.lock().expect("lock"),
        0,
        "must reload rotated auth.json from disk without replaying stale refresh_token"
    );
}

fn refresh_case(case: i64, early: bool) -> [(&'static str, i64); 6] {
    let now = 1_790_000_000;
    let tmp = aterm_tempfile::tempdir().unwrap();
    let home = tmp.path().join("login");
    let initial = make_chatgpt_jwt(Some("owner@example.test"), "pro", now + 3600);
    let rotated = make_chatgpt_jwt(Some("owner@example.test"), "pro", now + 7200);
    let response = make_chatgpt_jwt(Some("owner@example.test"), "pro", now + 5400);
    let mut initial_doc = json!({
        "auth_mode":"chatgpt", "tokens":{
            "id_token":initial, "access_token":initial,
            "refresh_token":"initial-refresh", "account_id":"acct_native_test"
        }
    });
    if case == 7 {
        let tokens = initial_doc
            .as_object_mut()
            .unwrap()
            .get_mut("tokens")
            .unwrap()
            .as_object_mut()
            .unwrap();
        tokens.remove("account_id");
        tokens.remove("id_token");
        tokens.insert("access_token".into(), json!("initial-opaque-token"));
    }
    write_auth_doc(&home, &initial_doc).unwrap();
    let initial_bytes = std::fs::read(home.join(AUTH_FILE)).unwrap();
    let before = load_stored_auth(&home).unwrap();
    let mut concurrent = initial_doc.clone();
    match case {
        0 | 4 | 5 => {}
        1 | 2 | 7 => {
            concurrent = json!({
                "auth_mode":"chatgpt", "tokens":{
                    "id_token":rotated, "access_token":rotated,
                    "refresh_token":"concurrent-refresh",
                    "account_id":if case == 2 {"another-account"} else {"acct_native_test"}
                }, "concurrent_metadata":"retained"
            });
        }
        3 => {
            concurrent = json!({"auth_mode":"chatgpt", "tokens":{
                "access_token":"concurrent-opaque-token", "refresh_token":"concurrent-refresh"
            }});
        }
        6 => {
            concurrent
                .as_object_mut()
                .unwrap()
                .insert("concurrent_metadata".into(), json!("retained"));
        }
        8 => {
            concurrent
                .as_object_mut()
                .unwrap()
                .get_mut("tokens")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("refresh_token".into(), json!("concurrent-refresh"));
        }
        _ => unreachable!(),
    }
    let mutate = move |home: &Path| {
        match case {
            0 => {}
            4 => std::fs::rename(home.join(AUTH_FILE), home.join("retained-auth.json")).unwrap(),
            5 => {
                std::fs::write(home.join(AUTH_FILE), b"unreadable authentication document").unwrap()
            }
            _ => write_auth_doc(home, &concurrent).unwrap(),
        }
        std::fs::read(home.join(AUTH_FILE)).ok()
    };
    let early_kept = early.then(|| mutate(&home));
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server_home = home.clone();
    let response_for_server = response.clone();
    let posted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let server_posted = Arc::clone(&posted);
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let Some(request) = read_http_request(&mut stream) else {
            // A refused pre-request observation sends no HTTP. The caller's
            // empty connection wakes this listener without a timing assertion.
            return early_kept.unwrap();
        };
        server_posted.store(true, Ordering::SeqCst);
        assert_eq!(
            (request.method.as_str(), request.target.as_str()),
            ("POST", "/oauth/token")
        );
        let kept = if early {
            early_kept.unwrap()
        } else {
            mutate(&server_home)
        };
        send_http_json(
            &mut stream,
            "200 OK",
            &json!({
                "id_token":response_for_server, "access_token":response_for_server,
                "refresh_token":"response-refresh"
            }),
        );
        kept
    });
    let result = refresh_stored_auth(
        &home,
        &before,
        &format!("http://127.0.0.1:{port}/oauth/token"),
        Duration::from_secs(60),
        now,
        None,
    );
    let _ = TcpStream::connect(("127.0.0.1", port));
    let kept = server.join().unwrap();
    let after = std::fs::read(home.join(AUTH_FILE)).ok();
    let current = load_stored_auth(&home);
    let wrote = current.as_ref().is_some_and(|a| a.access_token == response);
    let preserved = if case == 0 {
        true
    } else if case == 6 {
        current
            .as_ref()
            .is_some_and(|a| a.doc["concurrent_metadata"] == json!("retained"))
    } else {
        after == kept
    };
    if case == 4 {
        assert_eq!(
            std::fs::read(home.join("retained-auth.json")).unwrap(),
            initial_bytes
        );
    }
    [
        ("phase", 1),
        ("wrote", i64::from(wrote)),
        ("returned", i64::from(result.is_some())),
        (
            "reused",
            i64::from(
                result
                    .as_ref()
                    .is_some_and(|a| a.refresh_token.as_deref() == Some("concurrent-refresh")),
            ),
        ),
        ("kept", i64::from(preserved)),
        ("posted", i64::from(posted.load(Ordering::SeqCst))),
    ]
}

pub(crate) fn refresh_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "BeforeRequest" => {
            next.insert("early", 1);
        }
        "Refresh" => {
            for (key, value) in refresh_case(state["case"], state["early"] == 1) {
                next.insert(key, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn a_refresh_response_cannot_replace_a_login_changed_during_the_request() {
    super::codex_refresh_publication_environment();
    let model = aterm_models_harness::codex_refresh_publication_model();
    assert_eq!(model.mutant_dials(), (1..=7).collect::<Vec<_>>());
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 36);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = refresh_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let mut rotated = model.init_state();
    rotated.insert("case", 1);
    let mut bad = refresh_transition(&rotated, "Refresh");
    bad.insert("wrote", 1);
    assert!(!model.check_invariant("ReuseKnownRotation", &bad));
    bad.insert("kept", 0);
    assert!(!model.check_invariant("PreserveObserved", &bad));
}

#[test]
fn separately_bounded_responses_retain_large_tokens_across_fresh_file_reads() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let initial_id = make_chatgpt_jwt(Some("owner@example.test"), "pro", 1_900_000_000);
    let large_id = make_jwt(&json!({
        "email":"owner@example.test", "exp":1_900_000_000,
        "https://api.openai.com/auth":{
            "chatgpt_account_id":"acct_native_test", "chatgpt_user_id":"user_native_test"
        },
        "padding":"x".repeat(170_000)
    }));
    let large_refresh = "r".repeat(200_000);
    let large_access = "a".repeat(200_000);
    let metadata = json!({"notes":"m".repeat(100_000),"keep":[1,2,3]});
    write_auth_doc(
        tmp.path(),
        &json!({
            "auth_mode":"chatgpt", "metadata":metadata, "tokens":{
                "id_token":initial_id, "access_token":initial_id,
                "refresh_token":"initial-refresh", "account_id":"acct_native_test"
            }
        }),
    )
    .unwrap();
    let responses = [
        json!({"id_token":large_id, "access_token":"first-access", "refresh_token":"first-refresh"}),
        json!({"access_token":"second-access", "refresh_token":large_refresh}),
        json!({"access_token":large_access}),
        json!({"access_token":"last-access"}),
    ];
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let mut sent = Vec::new();
        for response in responses {
            assert!(aterm_json::to_vec(&response).unwrap().len() <= MESSAGE_MAX);
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream).unwrap();
            let body: Value = aterm_json::from_str(&request.body).unwrap();
            sent.push(body["refresh_token"].as_str().unwrap().to_owned());
            send_http_json(&mut stream, "200 OK", &response);
        }
        sent
    });
    for round in 0..4 {
        let before = load_stored_auth(tmp.path()).expect("fresh bounded file read");
        let refreshed = refresh_stored_auth(
            tmp.path(),
            &before,
            &format!("http://127.0.0.1:{port}/oauth/token"),
            Duration::from_secs(60),
            1_790_000_000 + round,
            None,
        )
        .expect("refresh from the saved replacement");
        let reloaded = load_stored_auth(tmp.path()).expect("reload published auth");
        assert_eq!(refreshed.doc, reloaded.doc);
        assert_eq!(reloaded.doc["metadata"], metadata);
        assert_eq!(reloaded.id_token.as_deref(), Some(large_id.as_str()));
        if round >= 1 {
            assert_eq!(
                reloaded.refresh_token.as_deref(),
                Some(large_refresh.as_str())
            );
        }
        if round == 2 {
            assert_eq!(reloaded.access_token, large_access);
            let bytes = std::fs::read(tmp.path().join(AUTH_FILE)).unwrap();
            assert!(bytes.len() > 2 * MESSAGE_MAX);
            assert!(bytes.len() < AUTH_FILE_MAX);
        }
    }
    assert_eq!(
        server.join().unwrap(),
        [
            "initial-refresh",
            "first-refresh",
            &large_refresh,
            &large_refresh
        ]
    );
}

fn result_case(case: i64) -> [(&'static str, i64); 6] {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let now = 1_790_000_000;
    let jwt = |user: &str, nonce: &str| {
        make_jwt(&json!({
            "jti":nonce, "email":"owner@example.test", "https://api.openai.com/auth":{
                "chatgpt_user_id":user,"chatgpt_account_id":"workspace-a"
            }
        }))
    };
    let old = jwt("user-a", "old");
    let refreshed = jwt("user-a", "refreshed");
    let mut initial = json!({"auth_mode":"chatgpt", "metadata":"original", "tokens":{
        "id_token":old, "access_token":old, "refresh_token":"old-refresh",
        "account_id":"workspace-a", "login_owner_id":"login-a"
    }});
    if case == 10 {
        initial
            .as_object_mut()
            .unwrap()
            .insert("padding".into(), json!("x".repeat(100_000)));
    }
    write_auth_doc(tmp.path(), &initial).unwrap();
    let prior = load_stored_auth(tmp.path()).unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_http_request(&mut stream).unwrap();
        send_http_json(
            &mut stream,
            "200 OK",
            &json!({
                "id_token":refreshed, "access_token":if case == 10 {"x".repeat(200_000)} else {refreshed},
                "refresh_token":"refreshed"
            }),
        );
    });
    let published = RefCell::new(None);
    let later = RefCell::new(std::fs::read(tmp.path().join(AUTH_FILE)).ok());
    let written = std::cell::Cell::new(false);
    let result = refresh_admitted(
        tmp.path(),
        &prior,
        &format!("http://127.0.0.1:{port}/oauth/token"),
        Duration::from_secs(60),
        now,
        |_| true,
        |home, doc| {
            if case == 9 {
                return Err(std::io::Error::other("injected publication failure"));
            }
            write_auth_doc(home, doc)?;
            written.set(true);
            let expected = load_stored_auth(home);
            if case == 10 {
                let bytes = std::fs::read(home.join(AUTH_FILE)).unwrap();
                assert!(bytes.len() > MESSAGE_MAX);
                assert!(
                    expected.is_some(),
                    "a successfully published refresh must survive a bounded reload"
                );
            }
            published.replace(expected);
            let mut changed = doc.clone();
            match case {
                1 | 2 => {
                    let changed_jwt = jwt(if case == 2 { "user-b" } else { "user-a" }, "later");
                    let tokens = changed
                        .as_object_mut()
                        .unwrap()
                        .get_mut("tokens")
                        .unwrap()
                        .as_object_mut()
                        .unwrap();
                    tokens.insert("id_token".into(), json!(changed_jwt));
                    tokens.insert("access_token".into(), json!(changed_jwt));
                    tokens.insert("refresh_token".into(), json!("later-refresh"));
                }
                3 => {
                    changed
                        .as_object_mut()
                        .unwrap()
                        .get_mut("tokens")
                        .unwrap()
                        .as_object_mut()
                        .unwrap()
                        .insert("account_id".into(), json!("workspace-b"));
                }
                4 => {
                    changed
                        .as_object_mut()
                        .unwrap()
                        .get_mut("tokens")
                        .unwrap()
                        .as_object_mut()
                        .unwrap()
                        .insert("login_owner_id".into(), json!("login-b"));
                }
                5 => {
                    std::fs::write(home.join(AUTH_FILE), b"malformed credentials").unwrap();
                }
                6 => {
                    std::fs::remove_file(home.join(AUTH_FILE)).unwrap();
                }
                7 => {
                    changed
                        .as_object_mut()
                        .unwrap()
                        .insert("metadata".into(), json!("later"));
                }
                8 => {
                    changed
                        .as_object_mut()
                        .unwrap()
                        .insert("auth_mode".into(), json!("api-key"));
                }
                _ => {}
            }
            if matches!(case, 1..=4 | 7 | 8) {
                write_auth_doc(home, &changed).unwrap();
            }
            later.replace(std::fs::read(home.join(AUTH_FILE)).ok());
            Ok(())
        },
    )
    .done();
    server.join().unwrap();
    let bound = result.as_ref().is_none_or(|result| {
        published
            .borrow()
            .as_ref()
            .is_some_and(|expected: &StoredAuth| {
                result.doc == expected.doc
                    && result.id_token == expected.id_token
                    && result.access_token == expected.access_token
                    && result.refresh_token == expected.refresh_token
                    && result.account_id == expected.account_id
                    && result.last_refresh_secs == expected.last_refresh_secs
            })
    });
    let kept = std::fs::read(tmp.path().join(AUTH_FILE)).ok() == *later.borrow();
    [
        ("phase", 1),
        ("written", i64::from(written.get())),
        ("result", i64::from(result.is_some())),
        ("bound", i64::from(bound)),
        ("kept", i64::from(kept)),
        ("case", case),
    ]
}

pub(crate) fn result_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Refresh" => {
            for (key, value) in result_case(state["case"]) {
                next.insert(key, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn refreshed_result_is_the_published_snapshot_not_a_later_login() {
    super::codex_refresh_result_environment();
    let model = aterm_models_harness::codex_refresh_result_model();
    assert_eq!(model.mutant_dials(), (1..=5).collect::<Vec<_>>());
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 22);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = result_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let good = result_transition(&model.init_state(), "Refresh");
    for (key, value, law) in [
        ("written", 0, "Publication"),
        ("result", 0, "Result"),
        ("bound", 0, "Snapshot"),
        ("kept", 0, "KeepLater"),
    ] {
        let mut bad = good.clone();
        bad.insert(key, value);
        assert!(!model.check_invariant(law, &bad));
    }
}

fn identity_case(case: i64) -> [(&'static str, i64); 5] {
    let now = 1_790_000_000;
    let tmp = aterm_tempfile::tempdir().unwrap();
    let home = tmp.path().join("identity");
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let jwt = |user: Option<&str>, workspace: &str, nonce: &str| {
        make_jwt(&json!({
            "jti":nonce, "email":"owner@example.test", "https://api.openai.com/auth":{
                "chatgpt_account_id":workspace, "chatgpt_user_id":user
            }
        }))
    };
    let user = if case == 11 || case == 13 {
        None
    } else {
        Some("user-a")
    };
    let initial_jwt = jwt(user, "derived-workspace", "original");
    let mut initial = json!({"auth_mode":"chatgpt", "marker":"keep", "tokens":{
        "id_token":initial_jwt, "access_token":initial_jwt, "refresh_token":"original-refresh"
    }});
    {
        let tokens = initial
            .as_object_mut()
            .unwrap()
            .get_mut("tokens")
            .unwrap()
            .as_object_mut()
            .unwrap();
        if case != 1 {
            tokens.insert("account_id".into(), json!("explicit-workspace"));
        }
        if [0, 4, 5, 15, 16, 17].contains(&case) {
            tokens.insert("login_owner_id".into(), json!("login-a"));
            tokens.insert(
                "oauth_provenance".into(),
                json!({
                    "issuer":if case == 0 {format!("http://127.0.0.1:{port}")}
                        else {DEFAULT_ISSUER.to_owned()}, "client_id":CLIENT_ID
                }),
            );
        }
        if case == 14 {
            tokens.insert(
                "access_token".into(),
                json!(jwt(Some("user-b"), "derived-workspace", "conflict")),
            );
        }
    }
    write_auth_doc(&home, &initial).unwrap();
    let before = if case == 14 {
        // The disk reader now refuses this input before refresh. Exercise the
        // refresh boundary's independent guard with the structural fixture too.
        assert!(load_stored_auth(&home).is_none());
        stored_auth_from_doc(initial.clone()).unwrap()
    } else {
        load_stored_auth(&home).unwrap()
    };
    let is_reload = matches!(case, 2..=7 | 13 | 15..=17);
    let (observed, result, posted, kept) = if is_reload {
        let mut changed = initial.clone();
        let tokens = changed
            .as_object_mut()
            .unwrap()
            .get_mut("tokens")
            .unwrap()
            .as_object_mut()
            .unwrap();
        let rotated = jwt(
            if case == 3 {
                Some("user-b")
            } else if case == 6 {
                None
            } else {
                Some("user-a")
            },
            "derived-workspace",
            "rotated",
        );
        tokens.insert("access_token".into(), json!(rotated));
        tokens.insert("id_token".into(), json!(rotated));
        match case {
            4 => {
                tokens.insert("login_owner_id".into(), json!("login-b"));
            }
            5 => {
                tokens.insert(
                    "oauth_provenance".into(),
                    json!({"issuer":"https://another.example", "client_id":CLIENT_ID}),
                );
            }
            7 => {
                tokens.insert(
                    "id_token".into(),
                    json!(jwt(Some("user-b"), "derived-workspace", "mismatch")),
                );
            }
            15 => {
                let legacy = make_jwt(&json!({"jti":"legacy", "https://api.openai.com/auth":{
                    "chatgpt_account_id":"derived-workspace", "chatgpt_user_id":null, "user_id":"user-a"
                }}));
                tokens.insert("id_token".into(), json!(legacy));
                tokens.insert("access_token".into(), json!(legacy));
            }
            16 => {
                tokens.remove("login_owner_id");
            }
            17 => {
                tokens.remove("oauth_provenance");
            }
            _ => {}
        }
        write_auth_doc(&home, &changed).unwrap();
        let evidence = std::fs::read(home.join(AUTH_FILE)).unwrap();
        let observed = reload_updated_disk_auth(&home, &before);
        if case == 7 {
            assert!(load_stored_auth(&home).is_none());
            assert!(updated_disk_auth(&before, stored_auth_from_doc(changed).unwrap()).is_none());
        }
        let result = if observed.is_some() { 2 } else { 0 };
        let kept = std::fs::read(home.join(AUTH_FILE)).unwrap() == evidence;
        (observed, result, false, kept)
    } else {
        let new_user = if case == 8 {
            Some("user-b")
        } else if case == 12 {
            None
        } else {
            Some("user-a")
        };
        let mut response = json!({
            "id_token":jwt(new_user, "new-default-workspace", "new-id"),
            "access_token":jwt(if case == 9 || case == 10 {Some("user-b")} else {new_user},
                "new-default-workspace", "new-access"),
            "refresh_token":"refreshed"
        });
        if case == 1 || case == 9 {
            response.as_object_mut().unwrap().remove("id_token");
        }
        let posted = Arc::new(AtomicBool::new(false));
        let server_posted = Arc::clone(&posted);
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let Some(request) = read_http_request(&mut stream) else {
                return;
            };
            server_posted.store(true, Ordering::SeqCst);
            assert_eq!(request.target, "/oauth/token");
            send_http_json(&mut stream, "200 OK", &response);
        });
        let evidence = std::fs::read(home.join(AUTH_FILE)).unwrap();
        let observed = refresh_stored_auth(
            &home,
            &before,
            &format!("http://127.0.0.1:{port}/oauth/token"),
            Duration::from_secs(60),
            now,
            None,
        );
        let _ = TcpStream::connect(("127.0.0.1", port));
        server.join().unwrap();
        let result = i64::from(observed.is_some());
        let kept = if observed.is_some() {
            let after = load_stored_auth(&home).unwrap();
            after.doc["marker"] == json!("keep")
                && (case != 1 || (before.id_token.is_some() && after.id_token == before.id_token))
                && ["login_owner_id", "oauth_provenance"]
                    .iter()
                    .all(|field| after.doc["tokens"].get(field) == before.doc["tokens"].get(field))
        } else {
            std::fs::read(home.join(AUTH_FILE)).unwrap() == evidence
        };
        (observed, result, posted.load(Ordering::SeqCst), kept)
    };
    let scoped = observed
        .as_ref()
        .is_none_or(|after| after.account_id == before.account_id);
    [
        ("phase", 1),
        ("result", result),
        ("scoped", i64::from(scoped)),
        ("kept", i64::from(kept)),
        ("posted", i64::from(posted)),
    ]
}

pub(crate) fn identity_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Observe" => {
            for (key, value) in identity_case(state["case"]) {
                next.insert(key, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn codex_refresh_identity_conforms_and_preserves_workspace_and_login() {
    super::codex_refresh_identity_environment();
    let model = aterm_models_harness::codex_refresh_identity_model();
    assert_eq!(model.mutant_dials(), (1..=6).collect::<Vec<_>>());
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 36);
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
    let good = identity_transition(&model.init_state(), "Observe");
    for (key, value, law) in [
        ("scoped", 0, "WorkspaceRetained"),
        ("kept", 0, "EvidenceRetained"),
        ("result", 0, "Decision"),
        ("posted", 0, "RequestBoundary"),
    ] {
        let mut bad = good.clone();
        bad.insert(key, value);
        assert!(!model.check_invariant(law, &bad));
    }
}

fn authority_case(case: i64) -> [(&'static str, i64); 6] {
    let now = 1_790_000_000;
    let tmp = aterm_tempfile::tempdir().unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let issuer = format!("http://127.0.0.1:{port}/tenant");
    let jwt = if case == 16 {
        make_jwt(&json!({"iss":"https://untrusted.example", "aud":"forged-client"}))
    } else {
        make_chatgpt_jwt(Some("owner@example.test"), "pro", now + 7200)
    };
    let mut initial = json!({"auth_mode":"chatgpt", "marker":"keep", "tokens":{
        "id_token":jwt, "access_token":jwt, "refresh_token":"original-refresh",
        "oauth_provenance":{"issuer":issuer, "client_id":"tenant-client"}
    }});
    let tokens = initial
        .as_object_mut()
        .unwrap()
        .get_mut("tokens")
        .unwrap()
        .as_object_mut()
        .unwrap();
    let record = match case {
        0 | 16 => None,
        1 | 22 | 29 => Some(json!({"issuer":issuer, "client_id":"tenant-client"})),
        2 => Some(json!({"issuer":format!("{issuer}///"), "client_id":"tenant-client"})),
        3 => Some(Value::Null),
        4 => Some(json!("invalid")),
        5 => Some(json!({"client_id":"tenant-client"})),
        6 => Some(json!({"issuer":issuer})),
        7 => Some(json!({"issuer":"", "client_id":"tenant-client"})),
        8 => Some(json!({"issuer":issuer, "client_id":"   "})),
        9 => Some(json!({"issuer":"http://192.0.2.1", "client_id":"tenant-client"})),
        10 => Some(
            json!({"issuer":format!("http://user@127.0.0.1:{port}"), "client_id":"tenant-client"}),
        ),
        11 => Some(json!({"issuer":format!("{issuer}?query"), "client_id":"tenant-client"})),
        12 => Some(json!({"issuer":format!("{issuer}#fragment"), "client_id":"tenant-client"})),
        13 => Some(json!({"issuer":format!("{issuer}\\path"), "client_id":"tenant-client"})),
        14 => Some(json!({"issuer":format!("{issuer} path"), "client_id":"tenant-client"})),
        15 => Some(json!({"issuer":"http://127.0.0.1:invalid", "client_id":"tenant-client"})),
        17 => Some(json!({"issuer":issuer, "client_id":"tenant-client", "future":"retained"})),
        18 => Some(json!({"issuer":42, "client_id":"tenant-client"})),
        19 => Some(json!({"issuer":issuer, "client_id":false})),
        20 => Some(json!({"issuer":issuer, "client_id":"tenant\nclient"})),
        21 => {
            Some(json!({"issuer":format!("http://127.0.0.1:{port}/"), "client_id":"tenant-client"}))
        }
        23 => Some(json!({"issuer":"https://", "client_id":"tenant-client"})),
        24 => Some(json!({"issuer":"https:///", "client_id":"tenant-client"})),
        25 => Some(json!({"issuer":"http://127.0.0.1.evil.test", "client_id":"tenant-client"})),
        26 => Some(json!({"issuer":"http://[::1", "client_id":"tenant-client"})),
        27 => Some(json!({"issuer":"http://localhost.evil.test", "client_id":"tenant-client"})),
        28 => Some(json!({"issuer":"http://2130706433", "client_id":"tenant-client"})),
        30 => Some(
            json!({"issuer":format!("HTTP://127.0.0.1:{port}/tenant"), "client_id":"tenant-client"}),
        ),
        _ => unreachable!(),
    };
    if let Some(record) = record {
        tokens.insert("oauth_provenance".into(), record);
    } else {
        tokens.remove("oauth_provenance");
    }
    write_auth_doc(tmp.path(), &initial).unwrap();
    let before = load_stored_auth(tmp.path()).unwrap();
    let evidence = Arc::new(Mutex::new(
        std::fs::read(tmp.path().join(AUTH_FILE)).unwrap(),
    ));
    let server_evidence = Arc::clone(&evidence);
    let server_home = tmp.path().to_path_buf();
    let mut concurrent = initial.clone();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream)?;
        if case == 29 {
            concurrent
                .as_object_mut()
                .unwrap()
                .get_mut("tokens")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(
                    "oauth_provenance".into(),
                    json!({"issuer":"https://changed.example", "client_id":"changed-client"}),
                );
            write_auth_doc(&server_home, &concurrent).unwrap();
            *server_evidence.lock().unwrap() = std::fs::read(server_home.join(AUTH_FILE)).unwrap();
        }
        send_http_json(
            &mut stream,
            "200 OK",
            &json!({
                "id_token":jwt, "access_token":jwt, "refresh_token":"rotated-refresh"
            }),
        );
        Some(request)
    });
    let fallback = if case == 22 {
        "invalid fallback".to_owned()
    } else {
        format!("http://127.0.0.1:{port}/fallback")
    };
    let result = refresh_stored_auth(
        tmp.path(),
        &before,
        &fallback,
        Duration::from_secs(60),
        now,
        None,
    );
    let _ = TcpStream::connect(("127.0.0.1", port));
    let request = server.join().unwrap();
    let target = request.as_ref().is_none_or(|request| {
        request.target
            == if case == 0 || case == 16 {
                "/fallback"
            } else if case == 21 {
                "/oauth/token"
            } else {
                "/tenant/oauth/token"
            }
    });
    let client = request.as_ref().is_none_or(|request| {
        let body: Value = aterm_json::from_str(&request.body).unwrap();
        body["client_id"]
            == json!(if case == 0 || case == 16 {
                CLIENT_ID
            } else {
                "tenant-client"
            })
            && body["refresh_token"] == json!("original-refresh")
    });
    let kept = if let Some(after) = &result {
        after.doc["marker"] == initial["marker"]
            && after.doc["tokens"].get("oauth_provenance")
                == initial["tokens"].get("oauth_provenance")
    } else {
        std::fs::read(tmp.path().join(AUTH_FILE)).unwrap() == *evidence.lock().unwrap()
    };
    [
        ("phase", 1),
        ("result", i64::from(result.is_some())),
        ("posted", i64::from(request.is_some())),
        ("target", i64::from(target)),
        ("client", i64::from(client)),
        ("kept", i64::from(kept)),
    ]
}

pub(crate) fn authority_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Refresh" => {
            for (key, value) in authority_case(state["case"]) {
                next.insert(key, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn codex_refresh_uses_the_recorded_oauth_authority() {
    super::codex_refresh_authority_environment();
    let model = aterm_models_harness::codex_refresh_authority_model();
    assert_eq!(model.mutant_dials(), (1..=6).collect::<Vec<_>>());
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 62);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = authority_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let good = authority_transition(&model.init_state(), "Refresh");
    for (key, value, law) in [
        ("result", 0, "Admission"),
        ("posted", 0, "RequestBoundary"),
        ("target", 0, "TargetAuthority"),
        ("client", 0, "ClientBinding"),
        ("kept", 0, "PreserveEvidence"),
    ] {
        let mut bad = good.clone();
        bad.insert(key, value);
        assert!(!model.check_invariant(law, &bad));
    }
    for (url, accepted) in [
        ("https://auth.openai.com", true),
        ("https://tenant.example/base", true),
        ("http://[::1]:1234", true),
        ("http://LOCALHOST:1234", true),
        ("http://[::2]:1234", false),
        ("https://user:pass@tenant.example", false),
    ] {
        assert_eq!(refresh_endpoint(url).is_some(), accepted, "{url}");
    }
    assert!(is_loopback_endpoint("HTTP://127.0.0.1:1234"));
    assert!(is_loopback_endpoint("http://LOCALHOST:1234"));
    assert!(!is_loopback_endpoint("http://localhost.evil.test:1234"));
}

#[test]
fn auth_reads_are_bounded_regular_non_symlink_files_and_expired_or_portless_login_exits_early() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("bounded");
    std::fs::create_dir_all(&home).expect("create home");
    let auth_path = home.join(AUTH_FILE);
    let jwt = make_chatgpt_jwt(Some("bounded@example.com"), "pro", 1_900_000_000);
    let base = aterm_json::to_string(&json!({
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": jwt,
            "access_token": jwt,
        }
    }))
    .expect("base json");
    let mut exact = base;
    exact.extend(std::iter::repeat_n(
        ' ',
        AUTH_FILE_MAX
            .checked_sub(exact.len())
            .expect("fits in bound"),
    ));
    assert_eq!(exact.len(), AUTH_FILE_MAX);
    std::fs::write(&auth_path, &exact).expect("write exact AUTH_FILE_MAX");
    assert_eq!(
        read_account_email(&home).as_deref(),
        Some("bounded@example.com")
    );

    exact.push(' ');
    std::fs::write(&auth_path, &exact).expect("write AUTH_FILE_MAX + 1");
    assert!(read_account_in_home(&home).is_none());

    std::fs::write(&auth_path, [0xff, 0xfe]).expect("write invalid utf8");
    assert!(read_account_in_home(&home).is_none());

    std::fs::remove_file(&auth_path).expect("remove file");
    std::fs::create_dir(&auth_path).expect("dir at auth.json");
    assert!(read_account_in_home(&home).is_none());
    std::fs::remove_dir(&auth_path).expect("remove dir at auth.json");

    #[cfg(unix)]
    {
        let real_home = tmp.path().join("real_home");
        write_auth_doc(
            &real_home,
            &json!({
                "auth_mode": "chatgpt",
                "tokens": {
                    "id_token": jwt,
                    "access_token": jwt,
                }
            }),
        )
        .expect("seed real_home");
        assert_eq!(
            read_account_email(&real_home).as_deref(),
            Some("bounded@example.com")
        );

        let sym_home = tmp.path().join("sym_home");
        std::os::unix::fs::symlink(&real_home, &sym_home).expect("symlink home dir");
        assert!(
            read_account_in_home(&sym_home).is_none(),
            "symlinked home directory must be refused"
        );

        let fifo_home = tmp.path().join("fifo_home");
        std::fs::create_dir_all(&fifo_home).expect("create fifo_home");
        let fifo_auth = fifo_home.join(AUTH_FILE);
        let made = aterm_uds::spawnfd::command("mkfifo")
            .arg(&fifo_auth)
            .status()
            .expect("mkfifo");
        assert!(made.success(), "mkfifo {}", fifo_auth.display());
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let acct = read_account_in_home(&fifo_home);
            let usage = read_usage_with(
                &fifo_home,
                "https://chatgpt.com/backend-api/wham/usage",
                "https://auth.openai.com/oauth/token",
                Duration::from_secs(5),
                1_790_000_000,
            );
            tx.send((acct, usage)).expect("send fifo result");
        });
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(60)),
            Ok((None, None)),
            "FIFO at auth.json must not block open()"
        );
        reader.join().expect("fifo reader thread");
    }

    let seen = Seen::default();
    let expired = Instant::now();
    let mut approver = &seen;
    let hung = login_with(
        &home,
        &Endpoints {
            issuer: DEFAULT_ISSUER.to_owned(),
            token_url: format!("{DEFAULT_ISSUER}/oauth/token"),
            callback_ports: vec![0],
        },
        &mut approver,
        HOSTS,
        None,
        expired,
    );
    assert_eq!(hung.outcome, Outcome::Hung);
    assert_eq!(hung.host.as_deref(), Some("auth.openai.com"));
    assert!(
        seen.handed.borrow().is_empty(),
        "expired deadline must not invoke approver"
    );

    let mut approver = &seen;
    let off_pin = login_with(
        &home,
        &Endpoints {
            issuer: "https://evil.example".to_owned(),
            token_url: "https://evil.example/oauth/token".to_owned(),
            callback_ports: vec![0],
        },
        &mut approver,
        HOSTS,
        None,
        expired,
    );
    assert_eq!(off_pin.outcome, Outcome::OffPin);
    assert_eq!(off_pin.host.as_deref(), Some("evil.example"));

    let mut approver = &seen;
    let off_pin_token = login_with(
        &home,
        &Endpoints {
            issuer: DEFAULT_ISSUER.to_owned(),
            token_url: "https://unpinned-token.example/oauth/token".to_owned(),
            callback_ports: vec![0],
        },
        &mut approver,
        HOSTS,
        None,
        Instant::now() + Duration::from_secs(30),
    );
    assert_eq!(off_pin_token.outcome, Outcome::OffPin);
    assert_eq!(
        off_pin_token.host.as_deref(),
        Some("unpinned-token.example")
    );
    assert!(
        seen.handed.borrow().is_empty(),
        "unpinned token_url must be refused before opening the browser"
    );

    let mut approver = &seen;
    let portless = login_with(
        &home,
        &Endpoints {
            issuer: DEFAULT_ISSUER.to_owned(),
            token_url: format!("{DEFAULT_ISSUER}/oauth/token"),
            callback_ports: Vec::new(),
        },
        &mut approver,
        HOSTS,
        None,
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    );
    assert!(
        matches!(
            &portless.outcome,
            Outcome::NotRun(msg) if msg.contains("no callback port configured")
        ),
        "unexpected outcome for empty callback_ports: {:?}",
        portless.outcome
    );
    assert!(seen.handed.borrow().is_empty());
}

/// A mock provider on `127.0.0.1:0` answering each request with `answer(method, target)` until
/// an empty connection ends it; the requests it saw come back from the join.
fn provider(
    answer: impl Fn(&str, &str) -> (&'static str, Value) + Send + 'static,
) -> (u16, std::thread::JoinHandle<Vec<RecordedHttp>>) {
    provider_by_request(move |request| answer(&request.method, &request.target))
}

/// A [`provider`] whose `answer` reads the whole request, its headers included.
fn provider_by_request(
    answer: impl Fn(&RecordedHttp) -> (&'static str, Value) + Send + 'static,
) -> (u16, std::thread::JoinHandle<Vec<RecordedHttp>>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock provider");
    let port = listener.local_addr().expect("addr").port();
    let server = std::thread::spawn(move || {
        let mut seen = Vec::new();
        while let Ok((mut stream, _)) = listener.accept() {
            let Some(request) = read_http_request(&mut stream) else {
                break;
            };
            let (status, body) = answer(&request);
            send_http_json(&mut stream, status, &body);
            seen.push(request);
        }
        seen
    });
    (port, server)
}

/// Ends a [`provider`] and returns what it saw.
fn provider_seen(port: u16, server: std::thread::JoinHandle<Vec<RecordedHttp>>) -> Vec<String> {
    let _ = TcpStream::connect(("127.0.0.1", port));
    server
        .join()
        .expect("provider")
        .into_iter()
        .map(|request| format!("{} {}", request.method, request.target))
        .collect()
}

/// A login in `home` whose access token is good for an hour, with `refresh` as its refresh token.
fn hour_login(home: &Path, now: i64, refresh: Option<&str>) -> StoredAuth {
    let access = make_chatgpt_jwt(Some("owner@example.test"), "pro", now + 3600);
    write_auth_doc(
        home,
        &json!({"auth_mode":"chatgpt", "tokens":{
            "id_token":access, "access_token":access, "refresh_token":refresh,
            "account_id":"acct_native_test"
        }}),
    )
    .expect("login");
    load_stored_auth(home).expect("stored login")
}

#[test]
fn a_login_the_provider_refused_reads_signed_out_and_a_failed_refresh_reads_nothing() {
    let now = 1_790_000_000;
    for (token_status, token_body, refresh, want) in [
        (
            "400 Bad Request",
            json!({"error":"invalid_grant"}),
            Some("spent"),
            Reading::SignedOut,
        ),
        (
            "401 Unauthorized",
            json!({"error":{"code":"refresh_token_expired"}}),
            Some("spent"),
            Reading::SignedOut,
        ),
        (
            "400 Bad Request",
            json!({"code":"refresh_token_reused"}),
            Some("spent"),
            Reading::SignedOut,
        ),
        // No refresh token: the refused access token is the whole login, and nothing is posted.
        (
            "500 Internal Server Error",
            json!({}),
            None,
            Reading::SignedOut,
        ),
        // CONTROL: an outage or another refusal says nothing of the login.
        (
            "500 Internal Server Error",
            json!({"error":"server_error"}),
            Some("kept"),
            Reading::Unread,
        ),
        (
            "400 Bad Request",
            json!({"error":"invalid_request"}),
            Some("kept"),
            Reading::Unread,
        ),
    ] {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("login");
        hour_login(&home, now, refresh);
        let (port, server) = provider(move |_, target| match target {
            "/oauth/token" => (token_status, token_body.clone()),
            _ => ("401 Unauthorized", json!({"error":"unauthorized"})),
        });
        let got = read_login_with(
            &home,
            &format!("http://127.0.0.1:{port}/backend-api/wham/usage"),
            &format!("http://127.0.0.1:{port}/oauth/token"),
            aterm_spec::wait::MUST_HAPPEN,
            now,
            None,
        );
        let seen = provider_seen(port, server);
        assert_eq!(got, want, "{token_status} {refresh:?}: {seen:?}");
        assert_eq!(
            seen.iter().filter(|r| r.starts_with("POST")).count(),
            usize::from(refresh.is_some()),
            "{seen:?}"
        );
        // A login is signed out only on a second refusal, asked after a look for another
        // writer's rotation; an outage asks once.
        assert_eq!(
            seen.iter().filter(|r| r.starts_with("GET")).count(),
            if want == Reading::SignedOut { 2 } else { 1 },
            "{seen:?}"
        );
    }
}

/// codex-alab spent the single-use refresh token first (it takes no lock on `auth.json`): aterm's
/// refresh is refused as reused, and the rotated login codex-alab writes is the login, not a
/// sign-out that would end the owner's pin. The rotation lands only after aterm has read the
/// refusal, while it looks for one, and the provider serves the usage to the rotated access
/// token alone, as the real one does: the refused token still reads `401`. Its usage is read
/// with the rotation, and the spent token is not sent again.
#[test]
fn a_refresh_refused_as_reused_reads_the_rotation_another_program_wrote() {
    let now = 1_790_000_000;
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("login");
    hour_login(&home, now, Some("spent-by-codex-alab"));
    let fresh = make_chatgpt_jwt(Some("owner@example.test"), "pro", now + 7200);
    let (posted, refresh_sent) = std::sync::mpsc::channel::<()>();
    // codex-alab's write: once aterm's refresh request reached the provider, it waits for the
    // lock aterm holds until it has read the refusal, then writes the rotation, so the rotation
    // lands after the refusal and inside the look for one.
    let writer = std::thread::spawn({
        let (home, fresh) = (home.clone(), fresh.clone());
        move || {
            refresh_sent
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("aterm's refresh request");
            let _after_refusal =
                AuthLock::within(&home, Instant::now() + aterm_spec::wait::MUST_HAPPEN)
                    .expect("aterm's refresh has its answer");
            write_auth_doc(
                &home,
                &json!({"auth_mode":"chatgpt", "tokens":{
                    "id_token":fresh, "access_token":fresh,
                    "refresh_token":"rotated-by-codex-alab",
                    "account_id":"acct_native_test"
                }}),
            )
            .expect("codex-alab's rotation");
        }
    });
    let rotated_bearer = format!("Bearer {fresh}");
    let (port, server) = provider_by_request(move |request| {
        let bearer = request
            .headers
            .iter()
            .find(|(key, _)| key == "authorization")
            .map(|(_, value)| value.as_str());
        match request.target.as_str() {
            "/oauth/token" => {
                let _ = posted.send(());
                ("400 Bad Request", json!({"code":"refresh_token_reused"}))
            }
            _ if bearer == Some(rotated_bearer.as_str()) => (
                "200 OK",
                json!({"plan_type":"pro", "rate_limit":{"allowed":true, "primary_window":{
                    "used_percent":42.0, "limit_window_seconds":18_000, "reset_at":1_790_010_800
                }}}),
            ),
            _ => ("401 Unauthorized", json!({"error":"unauthorized"})),
        }
    });
    let got = read_login_with(
        &home,
        &format!("http://127.0.0.1:{port}/backend-api/wham/usage"),
        &format!("http://127.0.0.1:{port}/oauth/token"),
        aterm_spec::wait::MUST_HAPPEN,
        now,
        None,
    );
    let seen = provider_seen(port, server);
    writer.join().expect("codex-alab's write");
    let Reading::Usage(usage) = got else {
        panic!("the rotated login is read, not signed out: {got:?} {seen:?}");
    };
    assert_eq!(usage.sample.session_pct, Some(42), "{seen:?}");
    assert_eq!(
        seen.iter().filter(|r| r.starts_with("POST")).count(),
        1,
        "the spent refresh token is not sent again: {seen:?}"
    );
}

/// ONE REFUSAL IS NOT A SIGN-OUT: the usage was refused and the refresh answered as reused, but
/// the second ask, after the look for a rotation, is served. The login reads its usage, so the
/// owner's pin on it stands; the spent refresh token is not sent again.
#[test]
fn a_refusal_the_second_ask_does_not_repeat_leaves_the_login_signed_in() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let now = 1_790_000_000;
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("login");
    hour_login(&home, now, Some("spent"));
    let asks = Arc::new(AtomicUsize::new(0));
    let (port, server) = provider({
        let asks = Arc::clone(&asks);
        move |_, target| match target {
            "/oauth/token" => ("400 Bad Request", json!({"code":"refresh_token_reused"})),
            _ if asks.fetch_add(1, Ordering::SeqCst) == 0 => {
                ("401 Unauthorized", json!({"error":"unauthorized"}))
            }
            _ => (
                "200 OK",
                json!({"plan_type":"pro", "rate_limit":{"allowed":true, "primary_window":{
                    "used_percent":42.0, "limit_window_seconds":18_000, "reset_at":1_790_010_800
                }}}),
            ),
        }
    });
    let got = read_login_with(
        &home,
        &format!("http://127.0.0.1:{port}/backend-api/wham/usage"),
        &format!("http://127.0.0.1:{port}/oauth/token"),
        aterm_spec::wait::MUST_HAPPEN,
        now,
        None,
    );
    let seen = provider_seen(port, server);
    let Reading::Usage(usage) = got else {
        panic!("one refusal does not sign the login out: {got:?} {seen:?}");
    };
    assert_eq!(usage.sample.session_pct, Some(42), "{seen:?}");
    assert_eq!(
        seen.iter().filter(|r| r.starts_with("POST")).count(),
        1,
        "{seen:?}"
    );
}

#[test]
fn a_refresh_with_too_little_time_left_sends_nothing() {
    let now = 1_790_000_000;
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("login");
    let stored = hour_login(&home, now, Some("single-use"));
    let token = |answer: &str| json!({"access_token": answer, "refresh_token": "next"});
    let (port, server) = provider(move |_, _| ("200 OK", token("unused")));
    let got = refresh_outcome(
        &home,
        &stored,
        &format!("http://127.0.0.1:{port}/oauth/token"),
        MIN_REFRESH_BUDGET - Duration::from_millis(1),
        now,
        None,
    );
    assert!(matches!(got, Refreshed::Failed), "{got:?}");
    assert_eq!(
        provider_seen(port, server),
        Vec::<String>::new(),
        "a request whose answer could be cut off never spends the refresh token"
    );
    // CONTROL: with the budget the request goes out.
    let (port, server) = provider(|_, _| ("500 Internal Server Error", json!({})));
    let _ = refresh_outcome(
        &home,
        &stored,
        &format!("http://127.0.0.1:{port}/oauth/token"),
        aterm_spec::wait::MUST_HAPPEN,
        now,
        None,
    );
    assert_eq!(provider_seen(port, server), ["POST /oauth/token"]);
}

/// Another refresh holds the login's lock for the whole of this one's budget, which is more than
/// [`MIN_REFRESH_BUDGET`], so only the lock keeps this refresh from sending its request: the
/// refresh token the other refresh is spending is not sent twice.
#[test]
fn a_refresh_sends_nothing_while_another_holds_the_logins_lock() {
    let now = 1_790_000_000;
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("login");
    let stored = hour_login(&home, now, Some("single-use"));
    let (port, server) = provider(|_, _| ("200 OK", json!({})));
    // Held until this refresh has returned, so held past its whole budget.
    let held = AuthLock::within(&home, Instant::now() + aterm_spec::wait::MUST_HAPPEN)
        .expect("the other refresh's lock");
    let got = refresh_outcome(
        &home,
        &stored,
        &format!("http://127.0.0.1:{port}/oauth/token"),
        MIN_REFRESH_BUDGET + Duration::from_secs(1),
        now,
        None,
    );
    drop(held);
    assert!(matches!(got, Refreshed::Failed), "{got:?}");
    assert_eq!(
        provider_seen(port, server),
        Vec::<String>::new(),
        "the refresh token another refresh holds is not spent twice"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(beside_auth(&home, LOCK_SUFFIX))
            .expect("the lock file")
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
    }
}

#[test]
fn a_refresh_response_a_failed_publication_left_is_published_by_the_next_read() {
    let now = 1_790_000_000;
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("login");
    let stored = hour_login(&home, now, Some("spent-refresh"));
    let before = std::fs::read(home.join(AUTH_FILE)).expect("auth");
    let fresh = make_chatgpt_jwt(Some("owner@example.test"), "pro", now + 7200);
    let answer = json!({"id_token":fresh, "access_token":fresh, "refresh_token":"rotated-refresh"});
    let (port, server) = provider(move |_, _| ("200 OK", answer.clone()));
    let got = refresh_admitted(
        &home,
        &stored,
        &format!("http://127.0.0.1:{port}/oauth/token"),
        aterm_spec::wait::MUST_HAPPEN,
        now,
        |_| true,
        |_, _| Err(std::io::Error::other("injected publication failure")),
    );
    assert_eq!(provider_seen(port, server), ["POST /oauth/token"]);
    assert!(matches!(got, Refreshed::Failed), "{got:?}");
    assert_eq!(std::fs::read(home.join(AUTH_FILE)).expect("auth"), before);
    let journal = RefreshJournal::read(&home)
        .flatten()
        .expect("the response is kept beside the login");
    assert_eq!(
        journal.tokens.refresh_token.as_deref(),
        Some("rotated-refresh")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(beside_auth(&home, JOURNAL_SUFFIX))
            .expect("journal")
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
    }

    // A file that cannot be read now keeps the journal for a later read.
    std::fs::write(home.join(AUTH_FILE), b"half-written").expect("garbage");
    recover_journal(&home, Instant::now() + aterm_spec::wait::MUST_HAPPEN);
    assert!(RefreshJournal::read(&home).flatten().is_some());
    std::fs::write(home.join(AUTH_FILE), &before).expect("restore");

    // The next read publishes it onto the login it was refreshed from.
    recover_journal(&home, Instant::now() + aterm_spec::wait::MUST_HAPPEN);
    let now_stored = load_stored_auth(&home).expect("published");
    assert_eq!(now_stored.refresh_token.as_deref(), Some("rotated-refresh"));
    assert_eq!(now_stored.access_token, fresh);
    assert!(
        RefreshJournal::read(&home).is_none(),
        "spent once published"
    );

    // CONTROL: a journal refreshed from another login is dropped, never published over this one.
    let published = std::fs::read(home.join(AUTH_FILE)).expect("auth");
    RefreshJournal {
        base: "another-login".to_owned(),
        tokens: journal.tokens,
    }
    .write(&home)
    .expect("a stale journal");
    recover_journal(&home, Instant::now() + aterm_spec::wait::MUST_HAPPEN);
    assert_eq!(
        std::fs::read(home.join(AUTH_FILE)).expect("auth"),
        published
    );
    assert!(RefreshJournal::read(&home).is_none());

    // A response naming another account than the row expects is neither kept nor published.
    let stored = hour_login(&home, now, Some("spent-refresh"));
    let before = std::fs::read(home.join(AUTH_FILE)).expect("auth");
    let other = make_chatgpt_jwt(Some("intruder@example.test"), "pro", now + 7200);
    let answer = json!({"id_token":other, "access_token":other, "refresh_token":"theirs"});
    let (port, server) = provider(move |_, _| ("200 OK", answer.clone()));
    let got = refresh_outcome(
        &home,
        &stored,
        &format!("http://127.0.0.1:{port}/oauth/token"),
        aterm_spec::wait::MUST_HAPPEN,
        now,
        Some("owner@example.test"),
    );
    assert_eq!(provider_seen(port, server), ["POST /oauth/token"]);
    assert!(matches!(got, Refreshed::Failed), "{got:?}");
    assert!(
        RefreshJournal::read(&home).is_none(),
        "another account's answer is not kept"
    );
    assert_eq!(std::fs::read(home.join(AUTH_FILE)).expect("auth"), before);
}

#[cfg(unix)]
#[test]
fn auth_lock_coordinates_refresh_reloads_rotated_tokens_and_refuses_symlinks() {
    use std::os::unix::fs::PermissionsExt as _;

    assert_eq!(
        lock_path(Path::new("/home/a/.codex/auth.json")),
        PathBuf::from("/home/a/.codex/auth.json.lock")
    );
    assert_eq!(
        lock_path(Path::new("/pool/acct-b.json")),
        PathBuf::from("/pool/acct-b.json.lock")
    );

    let now = 1_790_000_000;
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("locked-login");
    let initial_jwt = make_chatgpt_jwt(Some("lock@example.com"), "pro", now + 60);
    let rotated_jwt = make_chatgpt_jwt(Some("lock@example.com"), "pro", now + 7200);
    let server_jwt = make_chatgpt_jwt(Some("lock@example.com"), "pro", now + 5400);
    write_auth_doc(
        &home,
        &json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": initial_jwt,
                "access_token": initial_jwt,
                "refresh_token": "rt_initial",
                "account_id": "acct_native_test"
            }
        }),
    )
    .expect("seed initial auth");
    let auth_file = auth_path_in_home(&home);
    let prior = load_stored_auth(&home).expect("load initial");

    // 1. Holding `auth.json.lock` from another open file description (as codex-alab does),
    //    rotating `auth.json` on disk, and releasing the lock causes `refresh_stored_auth` to
    //    wait for the lock, reload the rotated credentials under the lock, and send 0 requests.
    let holder = AuthLock::try_take(&auth_file)
        .expect("open lock")
        .expect("lock free");
    let mode = std::fs::metadata(lock_path(&auth_file))
        .expect("lock metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "auth.json.lock must be created 0600");
    assert!(
        AuthLock::try_take(&auth_file).expect("probe").is_none(),
        "second open sees lock held"
    );

    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind mock token server");
    let port = listener.local_addr().expect("addr").port();
    let token_url = format!("http://127.0.0.1:{port}/oauth/token");
    let home_rot = home.clone();
    let rotated_for_thread = rotated_jwt.clone();
    let rotator = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(80));
        write_auth_doc(
            &home_rot,
            &json!({
                "auth_mode": "chatgpt",
                "tokens": {
                    "id_token": rotated_for_thread,
                    "access_token": rotated_for_thread,
                    "refresh_token": "rt_from_codex_alab",
                    "account_id": "acct_native_test"
                }
            }),
        )
        .expect("codex-alab rotates auth.json under lock");
        drop(holder);
    });

    let reloaded = refresh_stored_auth(
        &home,
        &prior,
        &token_url,
        aterm_spec::wait::MUST_HAPPEN,
        now,
        Some("lock@example.com"),
    )
    .expect("reloaded rotated credentials after lock release");
    rotator.join().expect("rotator thread");
    assert_eq!(
        reloaded.refresh_token.as_deref(),
        Some("rt_from_codex_alab")
    );
    assert_eq!(reloaded.access_token, rotated_jwt);

    // 2. When `refresh_stored_auth` sends a refresh request, it holds `auth.json.lock` across
    //    the HTTP request and publication, releasing it on return.
    let saw_held_at_request = Arc::new(AtomicBool::new(false));
    let saw_for_server = Arc::clone(&saw_held_at_request);
    let auth_for_server = auth_file.clone();
    let server_jwt_clone = server_jwt.clone();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept refresh");
        let _req = read_http_request(&mut stream).expect("read refresh");
        let probe = AuthLock::try_take(&auth_for_server).expect("probe lock at request");
        saw_for_server.store(probe.is_none(), Ordering::SeqCst);
        send_http_json(
            &mut stream,
            "200 OK",
            &json!({
                "id_token": server_jwt_clone,
                "access_token": server_jwt_clone,
                "refresh_token": "rt_from_aterm"
            }),
        );
    });
    let refreshed = refresh_stored_auth(
        &home,
        &reloaded,
        &token_url,
        aterm_spec::wait::MUST_HAPPEN,
        now,
        Some("lock@example.com"),
    )
    .expect("refresh under lock");
    server.join().expect("server thread");
    assert!(
        saw_held_at_request.load(Ordering::SeqCst),
        "auth.json.lock must be held while the refresh HTTP request is in flight"
    );
    assert_eq!(refreshed.refresh_token.as_deref(), Some("rt_from_aterm"));
    assert!(
        AuthLock::try_take(&auth_file)
            .expect("probe after refresh")
            .is_some(),
        "auth.json.lock must be unlocked once refresh_stored_auth returns"
    );

    // 3. A symlinked `auth.json.lock` is refused without following the link or creating its target.
    std::fs::remove_file(lock_path(&auth_file)).expect("remove lock");
    let elsewhere = tmp.path().join("elsewhere.lock");
    std::os::unix::fs::symlink(&elsewhere, lock_path(&auth_file)).expect("symlink lock");
    assert!(AuthLock::try_take(&auth_file).is_err());
    assert!(!elsewhere.exists());
    assert!(
        refresh_stored_auth(
            &home,
            &refreshed,
            &token_url,
            Duration::from_secs(5),
            now,
            None,
        )
        .is_none(),
        "symlinked lock must refuse refresh before any request"
    );
    assert!(!elsewhere.exists());
}
