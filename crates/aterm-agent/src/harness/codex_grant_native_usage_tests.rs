// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Synthetic private-world issuer; real canonical custody and quota HTTP.
use super::*;
#[cfg(unix)]
use std::io::{Read, Write};

#[cfg(unix)]
fn reading(outcome: NativeUsageOutcome) -> codex_app_login::Reading {
    match outcome {
        NativeUsageOutcome::Observed(observed) => observed.into_reading(),
        NativeUsageOutcome::Retaining(_) => panic!("unexpected retained response"),
    }
}

#[cfg(unix)]
fn accept(listener: &std::net::TcpListener) -> std::net::TcpStream {
    aterm_uds::accept(
        listener,
        aterm_uds::AcceptWait::Until(Instant::now() + Duration::from_secs(60)),
        aterm_uds::AcceptedMode::Blocking,
    )
    .unwrap_or_else(|error| panic!("bounded local HTTP accept failed: {error}"))
    .0
}

#[test]
fn native_usage_carriers_can_move_to_the_app_recovery_worker() {
    fn send<T: Send>() {}
    send::<NativeUsageRetention>();
    send::<NativeUsageContinuation>();
    send::<NativeUsageDeferred>();
}

#[test]
#[cfg(unix)]
fn canonical_native_usage_gets_once_but_cancelled_or_copied_sources_never_contact() {
    let fixture = crate::harness::codex_enrollment::NativeRowRecoveryFixture::new(&"c".repeat(64));
    fixture.retire_originals().unwrap();
    fixture.publish_first().unwrap();
    let env = fixture.env();
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    let (logins, warning) = store.load_logins(env.now).unwrap();
    assert!(warning.is_none());
    let home = PathBuf::from(logins.get("alpha").unwrap().dir.as_ref().unwrap());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::fs::write(
        home.join("config.toml"),
        format!("chatgpt_base_url='http://{address}'\n"),
    )
    .unwrap();
    let lifetime = account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let cancelled = lifetime.enter().unwrap();
    let cancellation = cancelled.cancellation();
    cancellation.cancel();
    let contacts = std::cell::Cell::new(0);
    let request = |path: &Path, scope: &account_work::Scope| {
        read_native_usage_observed(
            NativeUsageRequest {
                env,
                home: path,
                expected_email: Some("alpha@example.test"),
                now: env.now,
                wait: Duration::from_secs(60),
                scope,
                use_token: provider_read::Use::fixture(
                    provider_read::Read::CodexEndpoint,
                    path,
                    Some("alpha"),
                ),
                source: None,
            },
            &|| contacts.set(contacts.get() + 1),
        )
    };
    assert!(matches!(
        reading(request(&home, &cancelled)),
        codex_app_login::Reading::Unread
    ));
    drop(cancelled);
    assert!(cancellation.retired());
    let legacy = store.signin_now().unwrap();
    assert!(matches!(
        reading(request(&home, &scope)),
        codex_app_login::Reading::Unread
    ));
    drop(legacy);
    assert_eq!(contacts.get(), 0);
    let copy = env.home.as_ref().unwrap().join("copied-source");
    aterm_secrets::fsx::ensure_private_dir(&copy).unwrap();
    for name in ["auth.json", "config.toml"] {
        std::fs::copy(home.join(name), copy.join(name)).unwrap();
    }
    assert!(matches!(
        reading(request(&copy, &scope)),
        codex_app_login::Reading::Unread
    ));
    assert_eq!(contacts.get(), 0);
    assert!(matches!(
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
    ));
    listener.set_nonblocking(false).unwrap();
    let server = std::thread::spawn(move || {
        let mut socket = accept(&listener);
        socket
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        let mut received = Vec::new();
        let mut byte = [0];
        while !received.ends_with(b"\r\n\r\n") {
            socket.read_exact(&mut byte).unwrap();
            received.push(byte[0]);
            assert!(received.len() < 16 * 1024);
        }
        let request = String::from_utf8(received).unwrap();
        assert!(request.starts_with("GET /api/codex/usage HTTP/1.1\r\n"));
        let body = r#"{"rate_limit":{"allowed":true,"limit_reached":false,"secondary_window":{"used_percent":12,"limit_window_seconds":604800,"reset_at":1790086400}}}"#;
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
    });
    assert!(matches!(
        reading(request(&home, &scope)),
        codex_app_login::Reading::Usage(_)
    ));
    server.join().unwrap();
    assert_eq!(contacts.get(), 1);
}

#[test]
#[cfg(unix)]
fn native_quota_proactive_and_401_refresh_share_the_real_owner_journal() {
    for proactive in [false, true] {
        native_quota_refresh(proactive);
    }
}

#[cfg(unix)]
fn native_quota_refresh(proactive: bool) {
    let fixture = crate::harness::codex_enrollment::NativeRowRecoveryFixture::new(&"d".repeat(64));
    fixture.retire_originals().unwrap();
    fixture.publish_first().unwrap();
    let env = fixture.env();
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    let (before, _) = store.load_logins(env.now).unwrap();
    let binding = before
        .enrollment
        .published_grant(
            "alpha",
            before.get("alpha").unwrap().last_success_generation,
        )
        .unwrap();
    let home = PathBuf::from(before.get("alpha").unwrap().dir.as_ref().unwrap());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::fs::write(
        home.join("config.toml"),
        format!("chatgpt_base_url='http://{address}'\n"),
    )
    .unwrap();
    let server = std::thread::spawn(move || {
        let responses: Vec<(&str, &str, &str)> = if proactive {
            vec![
                (
                    "POST /oauth/token ",
                    "200 OK",
                    r#"{"access_token":"quota-refreshed-access","refresh_token":"quota-rotated-refresh"}"#,
                ),
                (
                    "GET /api/codex/usage ",
                    "200 OK",
                    r#"{"rate_limit":{"allowed":true,"limit_reached":false,"secondary_window":{"used_percent":12,"limit_window_seconds":604800,"reset_at":1890086400}}}"#,
                ),
            ]
        } else {
            vec![
                ("GET /api/codex/usage ", "401 Unauthorized", "{}"),
                (
                    "POST /oauth/token ",
                    "200 OK",
                    r#"{"access_token":"quota-refreshed-access","refresh_token":"quota-rotated-refresh"}"#,
                ),
                (
                    "GET /api/codex/usage ",
                    "200 OK",
                    r#"{"rate_limit":{"allowed":true,"limit_reached":false,"secondary_window":{"used_percent":12,"limit_window_seconds":604800,"reset_at":1890086400}}}"#,
                ),
            ]
        };
        for (method, status, body) in responses {
            let mut socket = accept(&listener);
            socket
                .set_read_timeout(Some(Duration::from_secs(60)))
                .unwrap();
            let mut received = Vec::new();
            let mut byte = [0];
            while !received.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut byte).unwrap();
                received.push(byte[0]);
                assert!(received.len() < 16 * 1024);
            }
            let request = String::from_utf8(received).unwrap();
            assert!(
                request.starts_with(method),
                "expected {method}, got {:?}",
                request.lines().next()
            );
            let length = request
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|n| n.parse::<usize>().ok())
                })
                .unwrap_or(0);
            let mut posted = vec![0; length];
            socket.read_exact(&mut posted).unwrap();
            if method.starts_with("POST") {
                let request: aterm_json::Value = aterm_json::from_slice(&posted).unwrap();
                assert_eq!(
                    request
                        .get("grant_type")
                        .and_then(aterm_json::Value::as_str),
                    Some("refresh_token")
                );
                assert_eq!(
                    request
                        .get("refresh_token")
                        .and_then(aterm_json::Value::as_str),
                    Some("test-refresh-alpha")
                );
            }
            write!(socket, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
        assert!(matches!(
            aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Ready,
                aterm_uds::AcceptedMode::Blocking,
            ),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
    });
    let lifetime = account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let now = env.now + if proactive { 8 * 86400 } else { 0 };
    let contacts = std::cell::Cell::new(0);
    let outcome = codex_app_login::with_refresh_fixture_endpoint(
        &format!("http://{address}/oauth/token"),
        || {
            read_native_usage_observed(
                NativeUsageRequest {
                    env,
                    home: &home,
                    expected_email: Some("alpha@example.test"),
                    now,
                    wait: Duration::from_secs(60),
                    scope: &scope,
                    use_token: provider_read::Use::fixture(
                        provider_read::Read::CodexEndpoint,
                        &home,
                        Some("alpha"),
                    ),
                    source: None,
                },
                &|| contacts.set(contacts.get() + 1),
            )
        },
    );
    assert!(matches!(
        reading(outcome),
        codex_app_login::Reading::Usage(_)
    ));
    server.join().unwrap();
    assert_eq!(contacts.get(), 1);
    let (after, _) = store.load_logins(now).unwrap();
    let witness = after.enrollment.owner_witness(&binding).unwrap().unwrap();
    assert_eq!(witness.revision.generation, 2);
    assert!(witness.pending.is_none());
    let claimed = Claimed::open(env, &binding, false).unwrap().unwrap();
    let journal = claimed.journal.as_ref().unwrap();
    assert!(journal.pending.is_none());
    assert_eq!(journal.completed.len(), 1);
    assert!(matches!(
        journal.completed[0].operation,
        actor::JournalOperation::NativeQuota(_)
    ));
}

#[cfg(unix)]
#[test]
fn native_quota_retains_completed_response_after_cancel_and_settles_without_another_http() {
    retained_response_after_cancel(false, false);
}

#[cfg(unix)]
#[test]
fn durable_deferred_response_resumes_under_new_same_app_scope_without_http() {
    retained_response_after_cancel(true, false);
}

#[cfg(unix)]
#[test]
fn cold_discovery_reauthenticates_a_parked_journal_under_a_new_app() {
    retained_response_after_cancel(true, true);
}

#[cfg(unix)]
fn retained_response_after_cancel(held: bool, broken_namespace: bool) {
    let fixture = crate::harness::codex_enrollment::NativeRowRecoveryFixture::new(&"e".repeat(64));
    fixture.retire_originals().unwrap();
    fixture.publish_first().unwrap();
    let env = fixture.env();
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    let (before, _) = store.load_logins(env.now).unwrap();
    let binding = before
        .enrollment
        .published_grant(
            "alpha",
            before.get("alpha").unwrap().last_success_generation,
        )
        .unwrap();
    let home = PathBuf::from(before.get("alpha").unwrap().dir.as_ref().unwrap());
    let claimed = Claimed::open(env, &binding, false).unwrap().unwrap();
    let directory = claimed.directory.clone();
    drop(claimed);
    let displaced = directory.with_extension("retained-quota-response");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::fs::write(
        home.join("config.toml"),
        format!("chatgpt_base_url='http://{address}'\n"),
    )
    .unwrap();
    let directory_in = directory.clone();
    let displaced_in = displaced.clone();
    let server = std::thread::spawn(move || {
        for post in [false, true] {
            let mut socket = accept(&listener);
            socket
                .set_read_timeout(Some(Duration::from_secs(60)))
                .unwrap();
            let mut received = Vec::new();
            let mut byte = [0];
            while !received.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut byte).unwrap();
                received.push(byte[0]);
                assert!(received.len() < 16 * 1024);
            }
            let request = String::from_utf8(received).unwrap();
            assert!(request.starts_with(if post {
                "POST /oauth/token "
            } else {
                "GET /api/codex/usage "
            }));
            let length = request
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|n| n.parse::<usize>().ok())
                })
                .unwrap_or(0);
            let mut posted = vec![0; length];
            socket.read_exact(&mut posted).unwrap();
            let (status, body) = if post {
                std::fs::rename(&directory_in, &displaced_in).unwrap();
                (
                    "200 OK",
                    r#"{"access_token":"retained-quota-access","refresh_token":"retained-quota-refresh"}"#,
                )
            } else {
                ("401 Unauthorized", "{}")
            };
            write!(socket, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
        listener
    });
    let lifetime = account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let cancellation = scope.cancellation();
    let contacts = std::cell::Cell::new(0);
    let outcome = codex_app_login::with_refresh_fixture_endpoint(
        &format!("http://{address}/oauth/token"),
        || {
            read_native_usage_observed(
                NativeUsageRequest {
                    env,
                    home: &home,
                    expected_email: Some("alpha@example.test"),
                    now: env.now,
                    wait: Duration::from_secs(60),
                    scope: &scope,
                    use_token: provider_read::Use::fixture(
                        provider_read::Read::CodexEndpoint,
                        &home,
                        Some("alpha"),
                    ),
                    source: None,
                },
                &|| contacts.set(contacts.get() + 1),
            )
        },
    );
    let NativeUsageOutcome::Retaining(carrier) = outcome else {
        panic!("completed response must retain actual custody")
    };
    let listener = server.join().unwrap();
    cancellation.cancel();
    let pause = lifetime.pause().unwrap();
    drop(scope);
    assert!(!cancellation.retired());
    std::fs::rename(&displaced, &directory).unwrap();
    let continuation = carrier.retry().unwrap();
    assert!(!cancellation.retired());
    if held {
        let roster = env.accounts_roster_for(accounts::Cli::Codex).unwrap();
        let original_roster = std::fs::read(&roster).unwrap();
        let original_auth = std::fs::read(home.join("auth.json")).unwrap();
        let mut policy = accounts::Roster::parse_for(
            accounts::Cli::Codex,
            std::str::from_utf8(&original_roster).unwrap(),
        )
        .unwrap();
        policy
            .rows
            .iter_mut()
            .find(|row| row.label == "alpha")
            .unwrap()
            .held_by = Some("explicit-security-hold".into());
        let text = policy
            .rows
            .iter()
            .map(|row| accounts::row_text(row).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&roster, text).unwrap();
        let NativeUsageSettlement::Deferred(deferred) = continuation.settle().unwrap() else {
            panic!("explicit hold must preserve the durable pending response");
        };
        assert!(
            cancellation.retired(),
            "metadata must not retain the original scope"
        );
        let NativeUsageSettlement::Deferred(deferred) = deferred.resume().unwrap() else {
            panic!("paused App cannot admit maintenance");
        };
        let drained = pause.try_wait().unwrap();
        let foreign = account_work::Lifetime::default();
        let foreign_scope = foreign.enter().unwrap();
        assert!(!cancellation.owned_by(&foreign));
        let NativeUsageSettlement::Deferred(deferred) = deferred.resume().unwrap() else {
            panic!("sealed original App cannot borrow another open App's admission");
        };
        drop(foreign_scope);
        assert!(foreign.pause().unwrap().try_wait().is_ok());
        assert_eq!(
            std::fs::read(home.join("auth.json")).unwrap(),
            original_auth
        );
        let (pending, _) = store.load_logins(env.now).unwrap();
        let witness = pending.enrollment.owner_witness(&binding).unwrap().unwrap();
        assert_eq!(witness.revision.generation, 1);
        assert!(witness.pending.is_some());
        let exclusive =
            codex_source_gate::PublicationLock::for_home(env.home.as_deref().unwrap()).unwrap();
        drop(exclusive);
        drained.resume().unwrap();
        let NativeUsageSettlement::Deferred(deferred) = deferred.resume().unwrap() else {
            panic!("new counted maintenance must still respect the explicit hold");
        };
        assert_eq!(
            std::fs::read(home.join("auth.json")).unwrap(),
            original_auth
        );
        let not_before = deferred.0.not_before.unwrap();
        let failures = deferred.0.failures;
        let NativeUsageSettlement::Deferred(deferred) = deferred
            .resume_at(not_before.checked_sub(Duration::from_millis(1)).unwrap())
            .unwrap()
        else {
            panic!("same-epoch retry must honor its backoff without another Scope");
        };
        assert_eq!(deferred.0.failures, failures);
        assert_eq!(deferred.0.not_before, Some(not_before));
        let mut deferred = deferred;
        while deferred.0.failures < MAX_MAINTENANCE_PACING {
            let due = deferred.0.not_before.unwrap();
            let NativeUsageSettlement::Deferred(next) = deferred.resume_at(due).unwrap() else {
                panic!("explicit hold must remain deferred");
            };
            deferred = next;
        }
        // Contention lasts beyond the saturated pacing counter in the SAME
        // epoch. It must not require a pause/resume to make future progress.
        for _ in 0..3 {
            let due = deferred.0.not_before.unwrap();
            let NativeUsageSettlement::Deferred(next) = deferred.resume_at(due).unwrap() else {
                panic!("explicit hold must still defer");
            };
            assert_eq!(next.0.failures, MAX_MAINTENANCE_PACING);
            deferred = next;
        }
        std::fs::write(&roster, original_roster).unwrap();
        if broken_namespace {
            // An actual epoch change is only a pacing wake. It cannot grant
            // authority over a missing or replaced physical namespace.
            lifetime
                .pause()
                .unwrap()
                .try_wait()
                .unwrap()
                .resume()
                .unwrap();
            let namespace =
                codex_source_gate::existing_namespace_for_home(env.home.as_deref().unwrap())
                    .unwrap();
            let gate = namespace.directory().unwrap().join("gate");
            let displaced_gate = gate.with_extension("deferred-held");
            std::fs::rename(&gate, &displaced_gate).unwrap();
            let NativeUsageSettlement::Deferred(deferred) = deferred.resume().unwrap() else {
                panic!("missing namespace must stay fenced");
            };
            assert!(deferred.0.parked_namespace);
            assert!(
                !gate.exists(),
                "maintenance must never recreate its namespace"
            );
            std::fs::rename(&displaced_gate, &gate).unwrap();
            lifetime
                .pause()
                .unwrap()
                .try_wait()
                .unwrap()
                .resume()
                .unwrap();
            let NativeUsageSettlement::Deferred(deferred) = deferred.resume().unwrap() else {
                panic!("a parked locator needs fresh authenticated discovery");
            };
            assert!(deferred.0.parked_namespace);
            assert_eq!(
                std::fs::read(home.join("auth.json")).unwrap(),
                original_auth
            );
            let fresh_app = account_work::Lifetime::default();
            let discovery = fresh_app.enter().unwrap();
            assert!(!discovery.cancellation().owned_by(&lifetime));
            let (mut tickets, _) = discover_native_usage_deferred(
                env,
                &discovery,
                NativeUsageDiscoveryCursor::default(),
                4,
            )
            .unwrap();
            assert_eq!(
                tickets.len(),
                1,
                "fresh canonical authentication discovers Raw"
            );
            let ticket = tickets.pop().unwrap();
            let NativeUsageSettlement::Deferred(ticket) = ticket.resume().unwrap() else {
                panic!("discovery must retire before its later operation");
            };
            drop(discovery);
            assert!(matches!(
                ticket.resume().unwrap(),
                NativeUsageSettlement::Settled
            ));
            assert!(fresh_app.pause().unwrap().try_wait().is_ok());
            assert!(
                deferred.0.parked_namespace,
                "old metadata never gains fresh App authority"
            );
        } else {
            let due = deferred.0.not_before.unwrap();
            assert!(matches!(
                deferred.resume_at(due).unwrap(),
                NativeUsageSettlement::Settled
            ));
        }
        assert!(cancellation.retired() && cancellation.cancelled());
        assert!(
            lifetime.pause().unwrap().try_wait().is_ok(),
            "new operation retired"
        );
    } else {
        assert!(matches!(
            continuation.settle().unwrap(),
            NativeUsageSettlement::Settled
        ));
        assert!(cancellation.retired());
        assert!(pause.try_wait().is_ok());
    }
    assert_eq!(contacts.get(), 1);
    let (after, _) = store.load_logins(env.now).unwrap();
    let witness = after.enrollment.owner_witness(&binding).unwrap().unwrap();
    assert_eq!(witness.revision.generation, 2);
    assert!(witness.pending.is_none());
    assert!(matches!(
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
    ));
}

#[test]
#[cfg(unix)]
fn loopback_fixture_cannot_bypass_real_refresh_issuer_admission() {
    let fixture = crate::harness::codex_enrollment::NativeRowRecoveryFixture::new(&"f".repeat(64));
    fixture.retire_originals().unwrap();
    fixture.publish_first().unwrap();
    let env = fixture.env();
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    let (logins, _) = store.load_logins(env.now).unwrap();
    let home = PathBuf::from(logins.get("alpha").unwrap().dir.as_ref().unwrap());
    let mut doc: aterm_json::Value =
        aterm_json::from_slice(&std::fs::read(home.join("auth.json")).unwrap()).unwrap();
    let provenance = doc
        .as_object_mut()
        .unwrap()
        .get_mut("tokens")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .get_mut("oauth_provenance")
        .unwrap()
        .as_object_mut()
        .unwrap();
    provenance.insert(
        "issuer".into(),
        aterm_json::Value::String("http://unsupported.invalid".into()),
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let before_send = std::cell::Cell::new(false);
    let result = codex_app_login::with_refresh_fixture_endpoint(
        &format!("http://{address}/oauth/token"),
        || {
            codex_app_login::exchange_owned_refresh(
                &aterm_json::to_vec(&doc).unwrap(),
                crate::harness::auth_deadline::Deadline::after(Duration::from_secs(60)),
                || {
                    before_send.set(true);
                    Ok(())
                },
            )
        },
    );
    assert!(result.is_err());
    assert!(!before_send.get());
    assert!(matches!(
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
    ));
}

#[test]
#[cfg(unix)]
fn two_native_quota_posts_share_catalogue_without_cross_owner_claim_cycles() {
    // Distinct synthetic bundles exercise the algorithm, not provider lineage.
    let fixture = crate::harness::codex_enrollment::NativeRowRecoveryFixture::new(&"a".repeat(64));
    fixture.retire_originals().unwrap();
    fixture.publish_first().unwrap();
    let lifetime = account_work::Lifetime::default();
    let (work, _) = crate::harness::codex_enrollment::owned_work(
        fixture.requests()[1].clone(),
        lifetime.enter().unwrap(),
    );
    fixture.run_recovery(work).unwrap();
    let env = fixture.env();
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    let (before, _) = store.load_logins(env.now).unwrap();
    #[derive(Debug)]
    enum Progress {
        Post(&'static str),
        Returned(&'static str, &'static str),
    }
    let (reached_tx, reached_rx) = std::sync::mpsc::channel();
    let listeners = std::thread::scope(|threads| {
        let mut readers = Vec::new();
        let mut servers = Vec::new();
        let mut release = Vec::new();
        for label in ["alpha", "beta"] {
            let home = PathBuf::from(before.get(label).unwrap().dir.as_ref().unwrap());
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            std::fs::write(
                home.join("config.toml"),
                format!("chatgpt_base_url='http://{address}'\n"),
            )
            .unwrap();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            release.push(release_tx);
            let reader_progress = reached_tx.clone();
            let reached_tx = reached_tx.clone();
            let (finished_tx, finished_rx) = std::sync::mpsc::channel();
            servers.push(threads.spawn(move || {
                'responses: for index in 0..3 {
                    let mut socket = {
                        let deadline = Instant::now() + Duration::from_secs(60);
                        loop {
                            match aterm_uds::accept(
                                &listener,
                                aterm_uds::AcceptWait::Ready,
                                aterm_uds::AcceptedMode::Blocking,
                            ) {
                                Ok((socket, _)) => break socket,
                                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                    if finished_rx.try_recv().is_ok() { break 'responses; }
                                    assert!(Instant::now() < deadline, "quota reader did not finish");
                                    std::thread::sleep(Duration::from_millis(5));
                                }
                                Err(error) => panic!("local HTTP accept failed: {error}"),
                            }
                        }
                    };
                    socket.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
                    let mut received = Vec::new();
                    let mut byte = [0];
                    while !received.ends_with(b"\r\n\r\n") {
                        socket.read_exact(&mut byte).unwrap();
                        received.push(byte[0]);
                        assert!(received.len() < 16 * 1024);
                    }
                    let request = String::from_utf8(received).unwrap();
                    let length = request.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length: ").and_then(|n| n.parse::<usize>().ok())).unwrap_or(0);
                    let mut posted = vec![0; length];
                    socket.read_exact(&mut posted).unwrap();
                    let body;
                    let (status, response) = if index == 1 {
                        assert!(request.starts_with("POST /oauth/token "));
                        let posted: aterm_json::Value = aterm_json::from_slice(&posted).unwrap();
                        assert_eq!(posted.get("refresh_token").and_then(aterm_json::Value::as_str), Some(format!("test-refresh-{label}").as_str()));
                        reached_tx.send(Progress::Post(label)).unwrap();
                        release_rx.recv_timeout(Duration::from_secs(60)).unwrap();
                        body = format!(r#"{{"access_token":"fresh-{label}","refresh_token":"rotated-{label}"}}"#);
                        ("200 OK", body.as_str())
                    } else {
                        assert!(request.starts_with("GET /api/codex/usage "));
                        if index == 0 { ("401 Unauthorized", "{}") }
                        else { ("200 OK", r#"{"rate_limit":{"allowed":true,"limit_reached":false,"secondary_window":{"used_percent":12,"limit_window_seconds":604800,"reset_at":1890086400}}}"#) }
                    };
                    write!(socket, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
                }
                listener
            }));
            let scope = lifetime.enter().unwrap();
            readers.push(threads.spawn(move || {
                let outcome = codex_app_login::with_refresh_fixture_endpoint(
                    &format!("http://{address}/oauth/token"),
                    || {
                        read_native_usage(NativeUsageRequest {
                            env,
                            home: &home,
                            expected_email: Some(&format!("{label}@example.test")),
                            now: env.now,
                            wait: Duration::from_secs(60),
                            scope: &scope,
                            use_token: provider_read::Use::fixture(
                                provider_read::Read::CodexEndpoint,
                                &home,
                                Some(label),
                            ),
                            source: None,
                        })
                    },
                );
                // The server may already have served the final GET. Its join
                // still checks success; this optional wake grants no authority.
                let _ = finished_tx.send(());
                let _ =
                    reader_progress.send(Progress::Returned(label, NATIVE_USAGE_TEST_STAGE.get()));
                outcome
            }));
            // Stage by an actual provider event, not elapsed time or retry.
            // Once A is in flight its account writer is released. B must now
            // proceed against A's durable bound Attempted journal; simultaneous
            // initial account-writer admission is a different fail-closed race.
            match reached_rx.recv_timeout(Duration::from_secs(60)).unwrap() {
                Progress::Post(actual) => assert_eq!(actual, label),
                Progress::Returned(actual, stage) => {
                    panic!(
                        "{actual} returned before both POSTs were in flight; closed native stage={stage}"
                    )
                }
            }
        }
        // No response was released during either observed POST. Both real
        // provider calls must be in flight before this first release.
        for send in release {
            send.send(()).unwrap();
        }
        let mut retained = Vec::new();
        for reader in readers {
            match reader.join().unwrap() {
                NativeUsageOutcome::Observed(observed) => {
                    assert!(!matches!(
                        observed.into_reading(),
                        codex_app_login::Reading::SignedOut
                    ));
                }
                NativeUsageOutcome::Retaining(carrier) => retained.push(carrier),
            }
        }
        let listeners = servers
            .into_iter()
            .map(|server| server.join().unwrap())
            .collect::<Vec<_>>();
        // Both genuine operations have returned. Any storage carrier is still
        // counted; settle it without admitting or sending another HTTP request.
        for carrier in retained {
            assert!(
                matches!(
                    carrier.retry().unwrap().settle().unwrap(),
                    NativeUsageSettlement::Settled
                ),
                "both completed provider operations must settle after their workers join"
            );
        }
        listeners
    });
    let maintenance = lifetime.enter().unwrap();
    for label in ["alpha", "beta"] {
        let binding = before
            .enrollment
            .published_grant(label, before.get(label).unwrap().last_success_generation)
            .unwrap();
        transaction::reconcile_retained(env, &binding, &maintenance).unwrap();
    }
    drop(maintenance);
    assert!(lifetime.pause().unwrap().try_wait().is_ok());
    for listener in listeners {
        assert!(matches!(
            aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Ready,
                aterm_uds::AcceptedMode::Blocking,
            ),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
    }
    let (after, _) = store.load_logins(env.now).unwrap();
    for label in ["alpha", "beta"] {
        let binding = after
            .enrollment
            .published_grant(label, after.get(label).unwrap().last_success_generation)
            .unwrap();
        let witness = after.enrollment.owner_witness(&binding).unwrap().unwrap();
        assert_eq!(witness.revision.generation, 2);
        assert!(witness.pending.is_none());
    }
}

#[test]
#[cfg(unix)]
fn an_unknown_native_quota_attempt_cannot_resend_its_owner_token() {
    let fixture = crate::harness::codex_enrollment::NativeRowRecoveryFixture::new(&"b".repeat(64));
    fixture.retire_originals().unwrap();
    fixture.publish_first().unwrap();
    let env = fixture.env();
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    let (before, _) = store.load_logins(env.now).unwrap();
    let binding = before
        .enrollment
        .published_grant(
            "alpha",
            before.get("alpha").unwrap().last_success_generation,
        )
        .unwrap();
    let home = PathBuf::from(before.get("alpha").unwrap().dir.as_ref().unwrap());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::fs::write(
        home.join("config.toml"),
        format!("chatgpt_base_url='http://{address}'\n"),
    )
    .unwrap();
    let server = std::thread::spawn(move || {
        for post in [false, true] {
            let mut socket = accept(&listener);
            socket
                .set_read_timeout(Some(Duration::from_secs(60)))
                .unwrap();
            let mut received = Vec::new();
            let mut byte = [0];
            while !received.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut byte).unwrap();
                received.push(byte[0]);
                assert!(received.len() < 16 * 1024);
            }
            let request = String::from_utf8(received).unwrap();
            assert!(request.starts_with(if post {
                "POST /oauth/token "
            } else {
                "GET /api/codex/usage "
            }));
            let length = request
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|n| n.parse::<usize>().ok())
                })
                .unwrap_or(0);
            let mut posted = vec![0; length];
            socket.read_exact(&mut posted).unwrap();
            if !post {
                write!(socket, "HTTP/1.1 401 Unauthorized\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}").unwrap();
            }
            // The POST was fully received; EOF cannot become NotSent proof.
        }
        listener
    });
    let lifetime = account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let read = || {
        codex_app_login::with_refresh_fixture_endpoint(
            &format!("http://{address}/oauth/token"),
            || {
                read_native_usage(NativeUsageRequest {
                    env,
                    home: &home,
                    expected_email: Some("alpha@example.test"),
                    now: env.now,
                    wait: Duration::from_secs(60),
                    scope: &scope,
                    use_token: provider_read::Use::fixture(
                        provider_read::Read::CodexEndpoint,
                        &home,
                        Some("alpha"),
                    ),
                    source: None,
                })
            },
        )
    };
    assert_eq!(reading(read()), codex_app_login::Reading::Unread);
    let listener = server.join().unwrap();
    let claimed = Claimed::open(env, &binding, false).unwrap().unwrap();
    let pending = claimed.journal.as_ref().unwrap().pending.as_ref().unwrap();
    assert!(pending.phase == Phase::Attempted);
    assert!(pending.response.is_none());
    drop(claimed);
    assert_eq!(reading(read()), codex_app_login::Reading::Unread);
    assert!(matches!(
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
    ));
}

#[cfg(unix)]
#[test]
fn already_captured_native_grant_refuses_late_current_or_spent_response_alias() {
    for spent in [false, true] {
        let fixture =
            crate::harness::codex_enrollment::NativeRowRecoveryFixture::new(&"c".repeat(64));
        fixture.retire_originals().unwrap();
        fixture.publish_first().unwrap();
        let lifetime = account_work::Lifetime::default();
        let scope = lifetime.enter().unwrap();
        let (work, _) = crate::harness::codex_enrollment::owned_work(
            fixture.requests()[1].clone(),
            scope.clone(),
        );
        fixture.run_recovery(work).unwrap();
        let env = fixture.env();
        let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
        let (initial, _) = store.load_logins(env.now).unwrap();
        let binding = |label| {
            initial
                .enrollment
                .published_grant(label, initial.get(label).unwrap().last_success_generation)
                .unwrap()
        };
        let a = binding("alpha");
        let b = binding("beta");
        transaction::fixture_promote_lineage_response(
            env,
            &a,
            &scope,
            "late-shared-response-token",
        );
        if spent {
            transaction::fixture_promote_lineage_response(
                env,
                &a,
                &scope,
                "later-unshared-source-token",
            );
        }
        let home = PathBuf::from(initial.get("alpha").unwrap().dir.as_ref().unwrap());
        let before_auth = std::fs::read(home.join("auth.json")).unwrap();
        let (before, _) = store.load_logins(env.now).unwrap();
        let before_witness = before.enrollment.owner_witness(&a).unwrap().unwrap();
        let permit =
            provider_read::Use::fixture(provider_read::Read::CodexEndpoint, &home, Some("alpha"))
                .begin_home(provider_read::Read::CodexEndpoint, &home, env.now)
                .unwrap();
        let context = ReadContext {
            permit,
            env: env.clone(),
            home: home.clone(),
            expected_email: Some("alpha@example.test".into()),
            deadline: Instant::now() + Duration::from_secs(60),
            scope: scope.clone(),
        };
        transaction::with_refresh_work_scoped(env, &scope, None, |work| {
            let lineage = work.send_lineage(env, &a).unwrap();
            let on_contact = || {};
            let captured = snapshot(&context, &a, work, &lineage, &on_contact).unwrap();
            // The response is already durable, but it has no B ownership or
            // delivery permission. A's actual captured send resources remain held.
            transaction::fixture_receive_lineage_response(
                env,
                &b,
                &scope,
                "late-shared-response-token",
            );
            assert!(
                captured.contact().is_err(),
                "a captured grant must rejoin its complete own history before provider use"
            );
            Ok(())
        })
        .unwrap();
        assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), before_auth);
        let (after, _) = store.load_logins(env.now).unwrap();
        assert!(after.enrollment.owner_witness(&a).unwrap().as_ref() == Some(&before_witness));
        assert!(
            transaction::with_refresh_work_scoped(env, &scope, None, |work| {
                assert!(work.send_lineage(env, &a).is_err());
                Ok(())
            })
            .is_ok()
        );
    }
}

#[test]
#[cfg(unix)]
fn cold_discovery_pages_past_four_locked_owners_and_settles_fifth_without_http() {
    let fixture = crate::harness::codex_enrollment::NativeRowRecoveryFixture::five(&"f".repeat(64));
    fixture.retire_originals().unwrap();
    let env = fixture.env();
    let original_app = account_work::Lifetime::default();
    let original_scope = original_app.enter().unwrap();
    for request in fixture.requests() {
        let (work, _) =
            crate::harness::codex_enrollment::owned_work(request.clone(), original_scope.clone());
        assert_eq!(
            fixture.run_recovery(work).unwrap().phase,
            crate::harness::codex_enrollment::Phase::Applied
        );
    }
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    let (logins, None) = store.load_logins(env.now).unwrap() else {
        panic!("clean world");
    };
    let bindings = fixture
        .requests()
        .iter()
        .map(|request| {
            logins
                .enrollment
                .published_grant(
                    &request.label,
                    logins.get(&request.label).unwrap().last_success_generation,
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    for (index, binding) in bindings.iter().enumerate() {
        transaction::fixture_receive_lineage_response(
            env,
            binding,
            &original_scope,
            &format!("received-unique-{index}"),
        );
    }
    let original_status = original_scope.cancellation();
    drop(original_scope);
    assert!(original_status.retired());
    drop(original_app);
    // Complete authenticated histories remain readable; only these actual
    // mutation claims are busy. They are not evidence of unknown independence.
    let held = bindings
        .iter()
        .take(4)
        .map(|binding| Claimed::open(env, binding, false).unwrap().unwrap())
        .collect::<Vec<_>>();
    let app = account_work::Lifetime::default();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    codex_app_login::with_refresh_fixture_endpoint(
        &format!("http://{}/oauth/token", listener.local_addr().unwrap()),
        || {
            let scope = app.enter().unwrap();
            let (first, cursor) = discover_native_usage_deferred(
                env,
                &scope,
                NativeUsageDiscoveryCursor::default(),
                4,
            )
            .unwrap();
            assert!(first.is_empty(), "first four are really held");
            let (mut second, cursor) =
                discover_native_usage_deferred(env, &scope, cursor, 4).unwrap();
            assert_eq!(second.len(), 1, "ordering must advance past the held batch");
            let fifth = second.pop().unwrap();
            drop(scope);
            assert!(matches!(
                fifth.resume().unwrap(),
                NativeUsageSettlement::Settled
            ));
            let scope = app.enter().unwrap();
            let (last, _) = discover_native_usage_deferred(env, &scope, cursor, 4).unwrap();
            assert!(last.is_empty());
        },
    );
    let (after, None) = store.load_logins(env.now).unwrap() else {
        panic!("clean world");
    };
    for (index, binding) in bindings.iter().enumerate() {
        let witness = after.enrollment.owner_witness(binding).unwrap().unwrap();
        assert_eq!(witness.revision.generation, if index == 4 { 2 } else { 1 });
        assert_eq!(witness.pending.is_some(), index != 4);
    }
    drop(held);
    assert!(app.pause().unwrap().try_wait().is_ok());
    assert!(matches!(
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
    ));
}

#[test]
#[cfg(unix)]
fn cold_discovery_finishes_anchored_publishing_with_no_pending_attempt_and_no_http() {
    let fixture = crate::harness::codex_enrollment::NativeRowRecoveryFixture::new(&"a".repeat(64));
    fixture.retire_originals().unwrap();
    let applied = fixture.publish_first().unwrap();
    let env = fixture.env();
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    let (before, None) = store.load_logins(env.now).unwrap() else {
        panic!("clean world");
    };
    let binding = before
        .enrollment
        .published_grant("alpha", applied.generation.unwrap())
        .unwrap();
    let old_app = account_work::Lifetime::default();
    let original = old_app.enter().unwrap();
    transaction::fixture_leave_anchored_publishing(env, &binding, &original);
    drop(original);
    drop(old_app);
    let app = account_work::Lifetime::default();
    let scope = app.enter().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    codex_app_login::with_refresh_fixture_endpoint(
        &format!("http://{}/oauth/token", listener.local_addr().unwrap()),
        || {
            let (mut tickets, _) = discover_native_usage_deferred(
                env,
                &scope,
                NativeUsageDiscoveryCursor::default(),
                4,
            )
            .unwrap();
            assert_eq!(tickets.len(), 1);
            drop(scope);
            assert!(matches!(
                tickets.pop().unwrap().resume().unwrap(),
                NativeUsageSettlement::Settled
            ));
        },
    );
    let (after, None) = store.load_logins(env.now).unwrap() else {
        panic!("clean world");
    };
    let witness = after.enrollment.owner_witness(&binding).unwrap().unwrap();
    assert_eq!(
        witness.revision.generation, 2,
        "cleanup must not rotate or reset C2"
    );
    assert!(witness.pending.is_none());
    let claimed = Claimed::open(env, &binding, false).unwrap().unwrap();
    assert!(claimed.journal.as_ref().unwrap().pending.is_none());
    drop(claimed);
    assert!(app.pause().unwrap().try_wait().is_ok());
    assert!(matches!(
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
    ));
}
