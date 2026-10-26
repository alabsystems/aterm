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
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
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
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
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
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until {
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
    let entries: Vec<_> = std::fs::read_dir(&home)
        .expect("read_dir")
        .filter_map(Result::ok)
        .map(|e| e.file_name())
        .collect();
    assert_eq!(
        entries,
        [std::ffi::OsString::from("auth.json")],
        "no SQLite or sidecar files created in login directory"
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
            let _ = rebind.set_read_timeout(Some(Duration::from_secs(5)));
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
        bad_state_finished = (handoff.finished)();

        let (err_status, _) =
            raw_get_loopback(&format!("{redirect}?error=access_denied&state={state}"));
        assert_eq!(err_status, 200);
        let start = Instant::now();
        while !(handoff.finished)() && start.elapsed() < Duration::from_secs(5) {
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
    assert_eq!(usage.wall, None);

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
    assert_eq!(full.wall, Some((SESSION_LIMIT, Some(1_790_010_800))));
    let week = with("\"usedPercent\":31", "\"usedPercent\":100");
    assert_eq!(week.wall, Some((WEEK_LIMIT, Some(1_790_259_200))));
    let refused = with(
        "\"ordinaryUsageAllowed\":true",
        "\"ordinaryUsageAllowed\":false",
    );
    assert_eq!(refused.wall, Some((SESSION_LIMIT, Some(1_790_010_800))));
    let reached = with(
        "\"rateLimitReachedType\":null",
        "\"rateLimitReachedType\":\"workspace_member_credits_depleted\"",
    );
    assert!(reached.wall.is_some());
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
    assert_eq!(spend_wall.wall, Some((SESSION_LIMIT, Some(1_790_010_800))));

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
            week_only.wall
        ),
        (None, Some(0), None)
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

    let slot_dir = tmp.path().join("slot");
    let user_home = tmp.path().join("user-home");
    std::fs::create_dir_all(&slot_dir).expect("slot_dir");
    std::fs::create_dir_all(user_home.join(".codex")).expect("user .codex");
    std::fs::write(
        slot_dir.join("config.toml"),
        "chatgpt_base_url = \"http://evil.example/backend-api\"\n",
    )
    .expect("bad config");
    assert_eq!(
        resolve_usage_url(&slot_dir.join("auth.json"), &user_home),
        "https://chatgpt.com/backend-api/wham/usage",
        "cleartext non-loopback chatgpt_base_url must fall back to HTTPS default"
    );
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
        MESSAGE_MAX.checked_sub(exact.len()).expect("fits in bound"),
    ));
    assert_eq!(exact.len(), MESSAGE_MAX);
    std::fs::write(&auth_path, &exact).expect("write exact MESSAGE_MAX");
    assert_eq!(
        read_account_email(&home).as_deref(),
        Some("bounded@example.com")
    );

    exact.push(' ');
    std::fs::write(&auth_path, &exact).expect("write MESSAGE_MAX + 1");
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
        Instant::now() + Duration::from_secs(30),
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
