// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::harness::codex_app_login::grant_completion::CodeGrantCapture;
use std::path::PathBuf;

struct Capture {
    home: PathBuf,
    before: Option<Vec<u8>>,
    seen: Arc<Mutex<Vec<&'static str>>>,
    durable: bool,
    parent: Option<StagedWitness>,
    cancel_after_arm: Option<crate::harness::account_work::Scope>,
}
struct Output {
    doc: Secret,
    verified: bool,
    durable: bool,
}

impl CodeGrantCapture for Capture {
    type Output = Output;
    fn begin_exchange(&mut self) -> Result<(), String> {
        assert!(
            AuthLock::try_take(&self.home.join(AUTH_FILE))
                .unwrap()
                .is_none()
        );
        self.parent = Some(StagedWitness::open(&self.home.join(AUTH_FILE))?);
        self.seen.lock().unwrap().push("armed");
        if let Some(scope) = &self.cancel_after_arm {
            scope.cancellation().cancel();
        }
        Ok(())
    }
    fn not_dispatched(self) -> Output {
        self.seen.lock().unwrap().push("not-dispatched");
        Output {
            doc: Secret::new(b""),
            verified: false,
            durable: true,
        }
    }
    fn retain_verified(self, grant: FreshCodeGrant<'_>) -> Output {
        grant.consume(|identity, owner, doc, witness| {
            assert_eq!(
                identity,
                &crate::harness::codex_renewal::Identity::from_native(
                    "acct_native_test",
                    "user_native_test"
                )
            );
            assert!(!owner.is_empty());
            assert!(witness.matches_auth_file(&self.home.join(AUTH_FILE)));
            assert!(witness.same_open_parent(self.parent.as_ref().unwrap()));
            assert!(witness.coordinates().is_ok());
            assert!(
                AuthLock::try_take(&self.home.join(AUTH_FILE))
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                std::fs::read(self.home.join(AUTH_FILE)).ok(),
                self.before,
                "retention must precede stage publication"
            );
            self.seen.lock().unwrap().push("verified");
            Output {
                doc: Secret::new(doc),
                verified: true,
                durable: self.durable,
            }
        })
    }
    fn retain_unverified(self, doc: &[u8]) -> Output {
        assert!(
            AuthLock::try_take(&self.home.join(AUTH_FILE))
                .unwrap()
                .is_none()
        );
        assert_eq!(std::fs::read(self.home.join(AUTH_FILE)).ok(), self.before);
        self.seen.lock().unwrap().push("unverified");
        Output {
            doc: Secret::new(doc),
            verified: false,
            durable: false,
        }
    }
    fn retain_rejected(self, rejection: CodeRejection<'_>) -> Output {
        rejection.consume(|status, body, witness| {
            assert_eq!(status, 400);
            assert!(witness.same_open_parent(self.parent.as_ref().unwrap()));
            assert!(
                AuthLock::try_take(&self.home.join(AUTH_FILE))
                    .unwrap()
                    .is_none()
            );
            assert_eq!(std::fs::read(self.home.join(AUTH_FILE)).ok(), self.before);
            self.seen.lock().unwrap().push("rejected");
            Output {
                doc: Secret::new(body),
                verified: false,
                durable: false,
            }
        })
    }
    fn verified_durable(output: &Output) -> bool {
        output.verified && output.durable
    }
}

fn capture_exchange(
    payload: Value,
    durable: bool,
) -> (Option<Option<String>>, Output, Vec<u8>, Vec<&'static str>) {
    capture_exchange_status(payload, durable, "200 OK")
}

fn capture_exchange_status(
    payload: Value,
    durable: bool,
    status: &'static str,
) -> (Option<Option<String>>, Output, Vec<u8>, Vec<&'static str>) {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let home = tmp.path().join("stage");
    let now = 1_790_000_000;
    hour_login(&home, now, Some("previous-refresh"));
    let before = std::fs::read(home.join(AUTH_FILE)).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let server_seen = Arc::clone(&seen);
    let (port, server) = provider(move |_, _| {
        assert_eq!(
            server_seen.lock().unwrap().as_slice(),
            ["armed"],
            "durable pending claim precedes HTTP"
        );
        (status, payload.clone())
    });
    let endpoints = Endpoints {
        issuer: format!("http://127.0.0.1:{port}"),
        token_url: format!("http://127.0.0.1:{port}/oauth/token"),
        callback_ports: vec![],
    };
    let (email, output) = CodeExchange {
        native_scope: None,
        publication: None,
        enrollment: None,
        home: &home,
        endpoints: &endpoints,
        redirect_uri: "http://127.0.0.1/callback",
        code: "single-use-fixture-code",
        code_verifier: "verifier",
        deadline: WakeDeadline::after(aterm_spec::wait::MUST_HAPPEN),
        work: None,
        now,
    }
    .persist_capturing(
        Some("actual-exchange-owner"),
        Some(Capture {
            home: home.clone(),
            before: Some(before.clone()),
            seen: Arc::clone(&seen),
            durable,
            parent: None,
            cancel_after_arm: None,
        }),
    );
    assert_eq!(provider_seen(port, server), ["POST /oauth/token"]);
    let output =
        output.expect("completed response has owned custody regardless of sign-in outcome");
    let after = std::fs::read(home.join(AUTH_FILE)).unwrap();
    if email.is_none() {
        assert_eq!(
            after, before,
            "non-durable or unsupported completion cannot replace the old stage"
        );
    }
    assert!(AuthLock::try_take(&home.join(AUTH_FILE)).unwrap().is_some());
    let trace = seen.lock().unwrap().clone();
    (email, output, after, trace)
}

#[test]
fn fresh_code_capture_retains_exact_doc_under_auth_lock_before_publication() {
    let token = make_chatgpt_jwt(Some("owner@example.test"), "pro", 1_790_003_600);
    let payload = json!({"id_token":token,"access_token":token,"refresh_token":"fresh-refresh"});
    let (email, output, after, trace) = capture_exchange(payload, true);
    assert_eq!(email, Some(Some("owner@example.test".into())));
    assert!(output.verified);
    assert_eq!(output.doc.expose(), after.as_slice());
    assert_eq!(trace, ["armed", "verified"]);
}

#[test]
fn completed_code_response_remains_owned_after_retention_failure_without_stage_publication() {
    let token = make_chatgpt_jwt(Some("owner@example.test"), "pro", 1_790_003_600);
    let (email, output, _, trace) = capture_exchange(
        json!({"id_token":token,"access_token":token,"refresh_token":"fresh-refresh"}),
        false,
    );
    assert!(email.is_none());
    assert!(output.verified && !output.durable);
    let doc: Value = aterm_json::from_slice(output.doc.expose()).unwrap();
    assert_eq!(
        doc["tokens"]["refresh_token"].as_str(),
        Some("fresh-refresh")
    );
    assert_eq!(trace, ["armed", "verified"]);
}

#[test]
fn unsupported_completed_code_responses_are_retained_without_refresh_ownership() {
    let token = make_chatgpt_jwt(Some("owner@example.test"), "pro", 1_790_003_600);
    let unknown_user = make_jwt(
        &json!({"email":"owner@example.test","https://api.openai.com/auth":{"chatgpt_account_id":"acct_native_test"}}),
    );
    let conflicting_user = make_jwt(
        &json!({"email":"owner@example.test","https://api.openai.com/auth":{"chatgpt_account_id":"acct_native_test","chatgpt_user_id":"different-user"}}),
    );
    for payload in [
        json!({"access_token":token,"refresh_token":"retained-missing-id"}),
        json!({"id_token":unknown_user,"access_token":unknown_user,"refresh_token":"retained-unknown-user"}),
        json!({"id_token":token,"access_token":conflicting_user,"refresh_token":"retained-conflicting-user"}),
        json!({"id_token":token,"access_token":token}),
        json!({"id_token":token,"access_token":token,"refresh_token":""}),
        json!({"id_token":token,"access_token":token,"refresh_token":"x".repeat(20*1024)}),
    ] {
        let (email, output, _, trace) = capture_exchange(payload, false);
        assert!(email.is_none());
        assert!(!output.verified);
        assert!(!output.doc.is_empty());
        assert_eq!(trace, ["armed", "unverified"]);
    }
}

#[test]
fn malformed_http_200_response_is_retained_exactly_without_stage_publication() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let home = tmp.path().join("stage");
    let now = 1_790_000_000;
    hour_login(&home, now, Some("previous-refresh"));
    let before = std::fs::read(home.join(AUTH_FILE)).unwrap();
    let body = b"{\"refresh_token\":\"completed-sensitive-response\",";
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        stream
            .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .unwrap();
        let request = read_http_request(&mut stream);
        if request.is_some() {
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        }
        request
    });
    let seen = Arc::new(Mutex::new(Vec::new()));
    let endpoints = Endpoints {
        issuer: format!("http://127.0.0.1:{port}"),
        token_url: format!("http://127.0.0.1:{port}/oauth/token"),
        callback_ports: vec![],
    };
    let (email, output) = CodeExchange {
        native_scope: None,
        publication: None,
        enrollment: None,
        home: &home,
        endpoints: &endpoints,
        redirect_uri: "http://127.0.0.1/callback",
        code: "single-use-code",
        code_verifier: "verifier",
        deadline: WakeDeadline::after(aterm_spec::wait::MUST_HAPPEN),
        work: None,
        now,
    }
    .persist_capturing(
        Some("fresh-exchange-owner"),
        Some(Capture {
            home: home.clone(),
            before: Some(before.clone()),
            seen: Arc::clone(&seen),
            durable: false,
            parent: None,
            cancel_after_arm: None,
        }),
    );
    let _ = TcpStream::connect(("127.0.0.1", port));
    assert!(server.join().unwrap().is_some());
    assert!(email.is_none());
    let output = output.unwrap();
    assert!(!output.verified);
    assert_eq!(output.doc.expose(), body);
    assert_eq!(std::fs::read(home.join(AUTH_FILE)).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn opened_stage_witness_refuses_public_files_hardlinks_and_replaced_parent() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = aterm_tempfile::tempdir().unwrap();
    let stage = tmp.path().join("stage");
    aterm_uds::private_file::create_private_dir(&stage).unwrap();
    let file = stage.join(AUTH_FILE);
    let witness = StagedWitness::open(&file).unwrap();
    aterm_uds::private_file::write_private(&stage, AUTH_FILE, b"synthetic").unwrap();
    assert!(witness.matches_auth_file(&file));
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(!witness.matches_auth_file(&file));
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::hard_link(&file, stage.join("second-link")).unwrap();
    assert!(!witness.matches_auth_file(&file));
    std::fs::remove_file(stage.join("second-link")).unwrap();
    std::fs::rename(&stage, tmp.path().join("held")).unwrap();
    aterm_uds::private_file::create_private_dir(&stage).unwrap();
    assert!(!witness.matches_auth_file(&file));
    let replacement = StagedWitness::open(&file).unwrap();
    assert!(!witness.same_open_parent(&replacement));
}

#[test]
fn cancelled_after_durable_arm_returns_owned_no_dispatch_without_post_or_stage_change() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let home = tmp.path().join("stage");
    let now = 1_790_000_000;
    hour_login(&home, now, Some("previous-refresh"));
    let before = std::fs::read(home.join(AUTH_FILE)).unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let endpoints = Endpoints {
        issuer: format!("http://127.0.0.1:{port}"),
        token_url: format!("http://127.0.0.1:{port}/oauth/token"),
        callback_ports: vec![],
    };
    let lifetime = crate::harness::account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (email, output) = CodeExchange {
        native_scope: None,
        publication: None,
        enrollment: None,
        home: &home,
        endpoints: &endpoints,
        redirect_uri: "http://127.0.0.1/callback",
        code: "single-use-code",
        code_verifier: "verifier",
        deadline: WakeDeadline::after(aterm_spec::wait::MUST_HAPPEN),
        work: Some(&scope),
        now,
    }
    .persist_capturing(
        Some("fresh-exchange-owner"),
        Some(Capture {
            home: home.clone(),
            before: Some(before.clone()),
            seen: Arc::clone(&seen),
            durable: false,
            parent: None,
            cancel_after_arm: Some(scope.clone()),
        }),
    );
    assert!(email.is_none());
    let output = output.expect("pre-send refusal must retain owned cleanup evidence");
    assert!(!output.verified && output.durable);
    assert_eq!(seen.lock().unwrap().as_slice(), ["armed", "not-dispatched"]);
    assert_eq!(
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap_err()
        .kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(std::fs::read(home.join(AUTH_FILE)).unwrap(), before);
}

#[test]
fn sent_code_request_with_no_response_never_mints_no_dispatch_evidence() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let home = tmp.path().join("stage");
    let now = 1_790_000_000;
    hour_login(&home, now, Some("previous-refresh"));
    let before = std::fs::read(home.join(AUTH_FILE)).unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        stream
            .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .unwrap();
        let request = read_http_request(&mut stream);
        assert!(request.is_some());
        // The peer consumed the code request but acknowledges nothing.
    });
    let endpoints = Endpoints {
        issuer: format!("http://127.0.0.1:{port}"),
        token_url: format!("http://127.0.0.1:{port}/oauth/token"),
        callback_ports: vec![],
    };
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (email, output) = CodeExchange {
        native_scope: None,
        publication: None,
        enrollment: None,
        home: &home,
        endpoints: &endpoints,
        redirect_uri: "http://127.0.0.1/callback",
        code: "single-use-code",
        code_verifier: "verifier",
        deadline: WakeDeadline::after(aterm_spec::wait::MUST_HAPPEN),
        work: None,
        now,
    }
    .persist_capturing(
        Some("fresh-exchange-owner"),
        Some(Capture {
            home: home.clone(),
            before: Some(before.clone()),
            seen: Arc::clone(&seen),
            durable: false,
            parent: None,
            cancel_after_arm: None,
        }),
    );
    server.join().unwrap();
    assert!(email.is_none() && output.is_none());
    assert_eq!(seen.lock().unwrap().as_slice(), ["armed"]);
    assert_eq!(std::fs::read(home.join(AUTH_FILE)).unwrap(), before);
}

#[test]
fn expired_before_code_exchange_retains_no_dispatch_without_browser_or_stage_write() {
    struct NeverApprove;
    impl Approver for NeverApprove {
        fn approve(&mut self, _: &Handoff<'_>) -> Approval {
            panic!("no browser before exchange")
        }
    }
    let tmp = aterm_tempfile::tempdir().unwrap();
    let home = tmp.path().join("stage");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (drove, output) = login_with_custody(
        &home,
        &Endpoints::default(),
        &mut NeverApprove,
        &["auth.openai.com"],
        None,
        WakeDeadline::after(Duration::ZERO),
        Some(Capture {
            home: home.clone(),
            before: None,
            seen: Arc::clone(&seen),
            durable: false,
            parent: None,
            cancel_after_arm: None,
        }),
    )
    .into_parts();
    assert!(matches!(drove.outcome, Outcome::Hung));
    assert!(output.is_some());
    assert_eq!(seen.lock().unwrap().as_slice(), ["not-dispatched"]);
    assert!(!home.join(AUTH_FILE).exists());
}

#[test]
fn completed_error_responses_retain_exact_body_without_grant_or_stage_publication() {
    for (status, payload) in [
        (
            "400 Bad Request",
            json!({"error":"invalid_grant","error_description":"synthetic-code-refusal"}),
        ),
        (
            "503 Service Unavailable",
            json!({"refresh_token":"unexpected-sensitive-error-only","native_unknown":true}),
        ),
    ] {
        let expected = aterm_json::to_vec(&payload).unwrap();
        let (email, output, _after, seen) = capture_exchange_status(payload, false, status);
        assert!(email.is_none());
        assert!(!output.verified && !output.durable);
        assert_eq!(output.doc.expose(), expected.as_slice());
        assert_eq!(
            seen,
            [
                "armed",
                if status.starts_with("400") {
                    "rejected"
                } else {
                    "unverified"
                }
            ]
        );
    }
}

#[test]
fn rejection_classifier_rejects_ambiguity_tokens_duplicate_keys_and_other_statuses() {
    assert!(is_definitive_code_rejection(
        400,
        br#"{"error":"invalid_grant","error_description":"Code expired"}"#
    ));
    for body in [
        br#"{"error":"invalid_grant","access_token":"sensitive"}"#.as_slice(),
        br#"{"error":"invalid_grant","refresh_token":null}"#,
        br#"{"error":"invalid_grant","tokens":{}}"#,
        br#"{"error":"invalid_client"}"#,
        br#"{"error":"invalid_grant","error":"invalid_client"}"#,
        br#"{"error":"invalid_client","error":"invalid_grant"}"#,
        br#"{"error":"invalid_grant","error_description":null}"#,
        br#"{"error":"invalid_grant","error_description":{},"id_token":"sensitive"}"#,
        br#"{"error":"invalid_grant","unknown":true}"#,
        br#"{"error":"invalid_grant"} trailing"#,
        b"not-json",
    ] {
        assert!(!is_definitive_code_rejection(400, body));
    }
    for status in [200, 401, 403, 429, 500, 503] {
        assert!(!is_definitive_code_rejection(
            status,
            br#"{"error":"invalid_grant"}"#
        ));
    }
    assert!(!is_definitive_code_rejection(
        400,
        &vec![b' '; MAX_REJECTION_BODY + 1]
    ));
}

#[test]
fn actual_rejected_code_response_waits_for_exact_owned_scope_retirement() {
    use crate::harness::{
        account_work, accounts, codex_enrollment as enrollment, codex_grant_owner as owner,
    };
    for cancel in [false, true] {
        owner::with_code_rejection_fixture(|env, auth, request| {
            // The native admission bookkeeping is a test fixture, not a peer or
            // browser qualification. The HTTP, typed producer, encrypted seal,
            // canonical transaction and counted Scope retirement are real.
            let home = auth.parent().unwrap();
            hour_login(home, env.now, Some("previous-refresh"));
            let before = std::fs::read(auth).unwrap();
            let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
            store
                .transact_logins(env.now, &mut None, |_, logins| {
                    logins.enrollment.admit_native(request)?;
                    Ok(Vec::new())
                })
                .unwrap();
            let lifetime = account_work::Lifetime::default();
            let scope = lifetime.enter().unwrap();
            let (work, control) = enrollment::owned_work(request.clone(), scope.clone());
            let (port, server) =
                provider(|_, _| ("400 Bad Request", json!({"error":"invalid_grant"})));
            let endpoints = Endpoints {
                issuer: format!("http://127.0.0.1:{port}"),
                token_url: format!("http://127.0.0.1:{port}/oauth/token"),
                callback_ports: vec![],
            };
            let capture = owner::prepare(env, request, auth, &"d".repeat(64)).unwrap();
            let (email, output) = CodeExchange {
                native_scope: None,
                publication: None,
                enrollment: None,
                home,
                endpoints: &endpoints,
                redirect_uri: "http://127.0.0.1/callback",
                code: "one-code-only",
                code_verifier: "verifier",
                deadline: WakeDeadline::after(aterm_spec::wait::MUST_HAPPEN),
                work: Some(&scope),
                now: env.now,
            }
            .persist_capturing(Some("owner-for-code-only"), Some(capture));
            assert!(email.is_none());
            let output = output.unwrap();
            assert!(matches!(&output, owner::CaptureOutcome::Rejected(_)));
            assert_eq!(provider_seen(port, server), ["POST /oauth/token"]);
            work.fixture_capture_after_native_admission(env, output);
            assert!(owner::recover_rejection(env, request).unwrap().is_some());
            assert!(
                owner::recover_rejection_retirement(env, request)
                    .unwrap()
                    .is_none()
            );
            enrollment::settle_retired_rejection(env, request).unwrap();

            let (replay, fake_control) =
                enrollment::owned_work(request.clone(), lifetime.enter().unwrap());
            drop(replay);
            assert!(fake_control.retired().is_none());
            fake_control.settle_custody(env).unwrap();
            control.settle_custody(env).unwrap();
            assert_eq!(
                store
                    .load_logins(env.now)
                    .unwrap()
                    .0
                    .enrollment
                    .lookup(request)
                    .unwrap()
                    .unwrap()
                    .phase,
                enrollment::Phase::Running
            );
            if cancel {
                assert_eq!(
                    control.cancel(env).unwrap().phase,
                    enrollment::Phase::CancelRequested
                );
            }
            let saved_outcome = if cancel {
                let path = store
                    .dir
                    .join("credential-owners")
                    .join(&request.id)
                    .join("candidate.age");
                let encrypted = std::fs::read(&path).unwrap();
                std::fs::write(&path, b"unreadable-fixture-outcome").unwrap();
                Some((path, encrypted))
            } else {
                None
            };
            drop(work);
            control.settle_custody(env).unwrap();
            assert!(
                control.retired().is_none(),
                "one actual resource scope remains counted"
            );
            drop(scope);
            assert!(control.retired().is_some());
            if cancel {
                control.settle_custody(env).unwrap();
                let status = store
                    .load_logins(env.now)
                    .unwrap()
                    .0
                    .enrollment
                    .lookup(request)
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    status.phase,
                    enrollment::Phase::CancelRequested,
                    "provider proof is not cancellation retirement authority"
                );
                assert_eq!(
                    control.retired().unwrap().finish(env).unwrap().phase,
                    enrollment::Phase::Cancelled
                );
            } else {
                // Simulate host loss AFTER the genuine resource-retirement
                // write but BEFORE the canonical terminal transaction.
                let retired = control.retired().unwrap();
                assert!(
                    owner::retain_rejection_retirement(env, &retired)
                        .unwrap()
                        .is_some()
                );
                drop(retired);
                drop(control);
                assert_eq!(
                    store
                        .load_logins(env.now)
                        .unwrap()
                        .0
                        .enrollment
                        .lookup(request)
                        .unwrap()
                        .unwrap()
                        .phase,
                    enrollment::Phase::Running
                );
                enrollment::settle_retired_rejection(env, request).unwrap();
                enrollment::settle_retired_rejection(env, request).unwrap();
                let status = store
                    .load_logins(env.now)
                    .unwrap()
                    .0
                    .enrollment
                    .lookup(request)
                    .unwrap()
                    .unwrap();
                assert_eq!(status.phase, enrollment::Phase::Refused);
                assert!(status.generation.is_none());
            }
            if let Some((path, encrypted)) = saved_outcome {
                assert_eq!(
                    std::fs::read(&path).unwrap(),
                    b"unreadable-fixture-outcome",
                    "cancellation must not repair, erase or adopt an unreadable outcome"
                );
                std::fs::write(path, encrypted).unwrap();
                assert!(owner::recover_rejection(env, request).unwrap().is_some());
            }
            assert_eq!(std::fs::read(auth).unwrap(), before);
        });
    }
}

fn native_owner_exchange(storage_failure: bool) {
    use crate::harness::{codex_grant_owner as owner, native_enrollment_admission as admission};
    let _callback_turn = admission::tests::callback_test_turn();
    let root = aterm_tempfile::tempdir().unwrap();
    let machine = root.path().join("machine");
    aterm_secrets::fsx::ensure_private_dir(&machine).unwrap();
    aterm_secrets::client::machine_identity_as(
        &aterm_secrets::client::Paths::locate_for_home(&machine),
        aterm_secrets::Making {
            test_binary: true,
            ..aterm_secrets::Making::default()
        },
    )
    .unwrap();
    let env = admission::tests::environment(root.path(), &machine, "native-state");
    let scope = admission::tests::reservation(&env, "native", "a");
    let request = crate::harness::codex_enrollment::Request {
        id: "a".repeat(32),
        caller: "d".repeat(64),
        label: "native".into(),
        revision: "e".repeat(64),
    };
    let stage = root.path().join("stage");
    let now = 1_790_000_000;
    hour_login(&stage, now, Some("previous-native-test-refresh"));
    let before = std::fs::read(stage.join(AUTH_FILE)).unwrap();
    let mut wrong_request = request.clone();
    wrong_request.id = "b".repeat(32);
    assert!(
        owner::prepare(
            &env,
            &wrong_request,
            &stage.join(AUTH_FILE),
            &"f".repeat(64)
        )
        .unwrap()
        .with_native_scope(&env, Some(Arc::clone(&scope)))
        .is_err(),
        "a real reservation cannot authorize another operation"
    );
    let mut wrong_store = env.clone();
    wrong_store.state = root.path().join("other-account-state");
    assert!(
        owner::prepare(
            &wrong_store,
            &request,
            &stage.join(AUTH_FILE),
            &"f".repeat(64)
        )
        .unwrap()
        .with_native_scope(&wrong_store, Some(Arc::clone(&scope)))
        .is_err(),
        "equal operation metadata cannot cross account stores"
    );
    let prepared = owner::prepare(&env, &request, &stage.join(AUTH_FILE), &"f".repeat(64))
        .unwrap()
        .with_native_scope(&env, Some(Arc::clone(&scope)))
        .unwrap();
    let directory = env
        .accounts_store_for(crate::harness::accounts::Cli::Codex)
        .unwrap()
        .dir
        .join("credential-owners")
        .join(&request.id);
    let moved = directory.with_file_name("retained-owner");
    let server_directory = directory.clone();
    let server_moved = moved.clone();
    let server_env = env.clone();
    let server_stage = stage.clone();
    let token = make_chatgpt_jwt(Some("owner@example.test"), "pro", now + 3600);
    let (port, server) = provider(move |_, _| {
        assert!(
            admission::RefreshExchangeLease::for_env(&server_env).is_err(),
            "actual code POST excludes native refresh"
        );
        assert!(
            AuthLock::try_take(&server_stage.join(AUTH_FILE))
                .unwrap()
                .is_none(),
            "stage lock spans actual HTTP"
        );
        if storage_failure {
            std::fs::rename(&server_directory, &server_moved).unwrap();
        }
        (
            "200 OK",
            json!({"id_token":token,"access_token":token,"refresh_token":"native-fixture-new-refresh"}),
        )
    });
    let endpoints = Endpoints {
        issuer: format!("http://127.0.0.1:{port}"),
        token_url: format!("http://127.0.0.1:{port}/oauth/token"),
        callback_ports: vec![],
    };
    let (email, output) = CodeExchange {
        native_scope: Some(&scope),
        publication: None,
        enrollment: None,
        home: &stage,
        endpoints: &endpoints,
        redirect_uri: "http://127.0.0.1/callback",
        code: "single-use-native-fixture",
        code_verifier: "verifier",
        deadline: WakeDeadline::after(aterm_spec::wait::MUST_HAPPEN),
        work: None,
        now,
    }
    .persist_capturing(Some("actual-native-fixture-owner"), Some(prepared));
    assert_eq!(provider_seen(port, server), ["POST /oauth/token"]);
    let output = output.expect("actual completed response retains native ownership");
    if storage_failure {
        assert!(email.is_none());
        assert_eq!(std::fs::read(stage.join(AUTH_FILE)).unwrap(), before);
        let owner::CaptureOutcome::Pending(pending) = output else {
            panic!("failed encrypted write must retain exact response")
        };
        drop(scope); // The carrier must now own the only remaining reservation.
        assert!(admission::RefreshExchangeLease::for_env(&env).is_err());
        std::fs::rename(&moved, &directory).unwrap();
        let owner::CaptureOutcome::Verified(grant) = pending.retry() else {
            panic!("exact retry must durably settle without another HTTP request")
        };
        owner::with_retained_response(&env, &request, &grant, |_, bytes, _, _, _| {
            assert_ne!(bytes, before.as_slice());
            Ok(())
        })
        .unwrap();
        assert_eq!(
            std::fs::read(stage.join(AUTH_FILE)).unwrap(),
            before,
            "storage retry alone cannot publish credentials"
        );
    } else {
        assert_eq!(email, Some(Some("owner@example.test".into())));
        let owner::CaptureOutcome::Verified(grant) = output else {
            panic!("actual native response must settle")
        };
        owner::with_retained_response(&env, &request, &grant, |_, bytes, _, _, _| {
            assert_eq!(bytes, std::fs::read(stage.join(AUTH_FILE)).unwrap());
            Ok(())
        })
        .unwrap();
        assert!(
            scope.check_exchange().is_err(),
            "durable settlement releases exchange before publication"
        );
        drop(scope);
    }
    admission::RefreshExchangeLease::for_env(&env)
        .unwrap()
        .current()
        .unwrap();
}

#[test]
fn actual_native_code_exchange_holds_gates_and_settles_before_releasing_them() {
    native_owner_exchange(false);
}

#[test]
fn actual_native_response_io_failure_keeps_gates_in_owned_carrier_until_exact_retry() {
    native_owner_exchange(true);
}
