// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("mo-")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .unwrap()
}

fn remote(home: &Path) -> RemoteTransport {
    RemoteTransport {
        home: home.into(),
        credentials: Credentials {
            email: "one@proton.me",
            login: "one",
            password: "test-only",
            mailbox_password: None,
            totp_secret: None,
        }
        .into(),
        end: Some(Instant::now() + Duration::from_secs(60)),
    }
}

fn listener(home: &Path) -> (Endpoint, CtlListener) {
    let endpoint = Endpoint::new(home, "one@proton.me").unwrap();
    let listener = aterm_uds::kept_out(|| CtlListener::bind(&endpoint.socket)).unwrap();
    std::fs::set_permissions(&endpoint.socket, std::fs::Permissions::from_mode(0o600)).unwrap();
    (endpoint, listener)
}

#[test]
fn independent_client_process() {
    let Some(home) = std::env::var_os("ATERM_MAIL_OWNER_TEST_HOME") else {
        return;
    };
    let mut client = remote(Path::new(&home));
    for _ in 0..2 {
        let response = client
            .request(aterm_http::Method::Get, "/core/v4/addresses", None)
            .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"shared");
    }
}

#[test]
fn independent_processes_share_a_live_owner() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let scratch = scratch();
    let (_endpoint, listener) = listener(scratch.path());
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let server = std::thread::spawn(move || {
        serve_connections(
            &listener,
            "one@proton.me",
            Duration::from_secs(5),
            |_, _, _| {
                observed.fetch_add(1, Ordering::SeqCst);
                Ok((json!({"status":200}), Zeroizing::new(b"shared".to_vec())))
            },
        )
        .unwrap()
    });
    // Keep this native client alive while two other OS processes use the owner.
    let mut retained = remote(scratch.path());
    assert_eq!(
        retained
            .request(aterm_http::Method::Get, "/core/v4/addresses", None)
            .unwrap()
            .status,
        200
    );
    std::thread::scope(|scope| {
        let run = || {
            let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "harness::mail_browser::owner::tests::independent_client_process",
                    "--nocapture",
                ])
                .env("ATERM_MAIL_OWNER_TEST_HOME", scratch.path());
            let output =
                aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        let first = scope.spawn(run);
        let second = scope.spawn(run);
        first.join().unwrap();
        second.join().unwrap();
    });
    server.join().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 5);
}

#[test]
fn lost_write_reply_is_not_replayed() {
    let scratch = scratch();
    let (_endpoint, listener) = listener(scratch.path());
    let server = std::thread::spawn(move || {
        aterm_uds::ready::any_until(
            &[listener.as_fd()],
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap();
        let (mut stream, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Nonblocking,
        )
        .unwrap();
        let mut input = DeadlineStream {
            stream: &mut stream,
            end: Instant::now() + Duration::from_secs(60),
        };
        let header = read_frame(&mut input, MAX_HEADER).unwrap();
        let request: Value = aterm_json::from_slice(&header).unwrap();
        assert_eq!(request["method"].as_str(), Some("POST"));
        assert!(!read_frame(&mut input, MAX_BODY).unwrap().is_empty());
        drop(stream);
        listener
    });
    let error = remote(scratch.path())
        .request(
            aterm_http::Method::Post,
            "/mail/v4/messages",
            Some(json!({"test":true})),
        )
        .err()
        .expect("lost reply must fail");
    assert!(error.message.contains("outcome is unknown"));
    let listener = server.join().unwrap();
    assert_eq!(
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Nonblocking,
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn abandoned_and_expired_clients_leave_the_owner_available() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let scratch = scratch();
    let (endpoint, listener) = listener(scratch.path());
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let server = std::thread::spawn(move || {
        serve_connections(
            &listener,
            "one@proton.me",
            Duration::from_secs(5),
            |_, _, _| {
                observed.fetch_add(1, Ordering::SeqCst);
                Ok((json!({"status":200}), Zeroizing::new(Vec::new())))
            },
        )
        .unwrap()
    });
    let end = Instant::now() + Duration::from_secs(60);
    let mut abandoned = endpoint.connect(end).unwrap();
    abandoned.write_all(&[0, 0]).unwrap();
    drop(abandoned);
    let mut expired = endpoint.connect(end).unwrap();
    expired.set_nonblocking(true).unwrap();
    let mut exchange = DeadlineStream {
        stream: &mut expired,
        end,
    };
    let header = aterm_json::to_vec(
        &json!({"version":1,"account":"one@proton.me","operation":"request","expires":0}),
    )
    .unwrap();
    write_frame(&mut exchange, &header, MAX_HEADER).unwrap();
    write_frame(&mut exchange, &[], MAX_BODY).unwrap();
    let reply: Value =
        aterm_json::from_slice(&read_frame(&mut exchange, MAX_HEADER).unwrap()).unwrap();
    assert_eq!(
        reply["error"].as_str(),
        Some("Mail operation deadline expired")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let mut live = remote(scratch.path());
    assert_eq!(
        live.request(aterm_http::Method::Get, "/core/v4/addresses", None)
            .unwrap()
            .status,
        200
    );
    server.join().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn cleanup_does_not_unlink_a_replacement_owner() {
    let scratch = scratch();
    let (endpoint, first) = listener(scratch.path());
    let path = endpoint.socket.clone();
    let identity = aterm_uds::listener::listener_at(&path).unwrap();
    let bound = BoundSocket { endpoint, identity };
    std::fs::remove_file(&path).unwrap();
    let second = aterm_uds::kept_out(|| CtlListener::bind(&path)).unwrap();
    let replacement = aterm_uds::listener::listener_at(&path).unwrap();
    assert_ne!(identity, replacement);
    drop(bound);
    assert_eq!(aterm_uds::listener::listener_at(&path), Some(replacement));
    drop((first, second));
}

#[test]
fn rejected_credentials_retire_the_owner_for_fresh_bootstrap() {
    let scratch = scratch();
    let (_endpoint, listener) = listener(scratch.path());
    let (done, finished) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        serve_connections(
            &listener,
            "one@proton.me",
            Duration::from_secs(3600),
            |_, _, _| {
                Err(ApiError {
                    kind: Kind::Credentials,
                    message: "Rejected test credential".into(),
                })
            },
        )
        .unwrap();
        done.send(()).unwrap();
    });
    let error = remote(scratch.path())
        .request(aterm_http::Method::Get, "/core/v4/addresses", None)
        .err()
        .expect("credential rejection");
    assert_eq!(error.kind, Kind::Credentials);
    finished
        .recv_timeout(Duration::from_secs(60))
        .expect("owner must exit rather than retaining rejected credentials");
    server.join().unwrap();
}

#[test]
fn frames_reject_oversize_before_reading_payload() {
    let mut input = &(u32::MAX.to_be_bytes())[..];
    assert_eq!(
        read_frame(&mut input, MAX_HEADER).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    let mut input = &b"\0\0\0\x03ab"[..];
    assert_eq!(
        read_frame(&mut input, MAX_HEADER).unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn queued_requests_expire_and_cannot_cross_accounts() {
    let expired = json!({"version":1,"account":"one@proton.me","expires":monotonic_ms().unwrap().saturating_sub(1)});
    assert!(request_end(&expired, "one@proton.me").is_err());
    let live =
        json!({"version":1,"account":"one@proton.me","expires":monotonic_ms().unwrap()+60_000});
    assert!(request_end(&live, "two@proton.me").is_err());
    let end = request_end(&live, "one@proton.me").unwrap();
    assert!(end <= Instant::now() + OPERATION);
}

#[test]
fn private_endpoint_refuses_files_and_symlinks() {
    let scratch = scratch();
    let endpoint = Endpoint::new(scratch.path(), "one@proton.me").unwrap();
    std::fs::write(&endpoint.socket, b"not a socket").unwrap();
    assert!(endpoint.metadata().is_err());
    std::fs::remove_file(&endpoint.socket).unwrap();
    std::os::unix::fs::symlink(scratch.path().join("absent"), &endpoint.socket).unwrap();
    assert!(endpoint.metadata().is_err());
    std::fs::remove_file(&endpoint.socket).unwrap();
    let socket = aterm_uds::kept_out(|| CtlListener::bind(&endpoint.socket)).unwrap();
    std::fs::set_permissions(&endpoint.socket, std::fs::Permissions::from_mode(0o666)).unwrap();
    assert!(endpoint.metadata().is_err());
    drop(socket);
}
