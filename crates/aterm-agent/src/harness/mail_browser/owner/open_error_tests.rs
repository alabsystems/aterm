// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_mail::proton_error::{Operation, Recovery};

enum Reply {
    Key,
    KeyDenied,
    Error(Kind),
    RateLimited(&'static str),
    Header(Value),
    Malformed,
    Oversized,
    Lost,
}

fn open_failure(replies: Vec<Reply>) -> MailboxOpenError {
    let home = aterm_tempfile::Builder::new()
        .prefix("mo-open-")
        .suffix(".noindex")
        .tempdir()
        .unwrap();
    let endpoint = Endpoint::new(home.path(), "one@proton.me").unwrap();
    let listener = aterm_uds::kept_out(|| CtlListener::bind(&endpoint.socket)).unwrap();
    std::fs::set_permissions(&endpoint.socket, std::fs::Permissions::from_mode(0o600)).unwrap();
    let expected = replies.len();
    let peer = std::thread::spawn(move || {
        let mut received = 0;
        for (index, reply) in replies.into_iter().enumerate() {
            let end = Instant::now() + Duration::from_secs(60);
            aterm_uds::ready::any_until(&[listener.as_fd()], end).unwrap();
            let (mut raw, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Ready,
                aterm_uds::AcceptedMode::Nonblocking,
            )
            .unwrap();
            private_peer(&raw).unwrap();
            let mut stream = DeadlineStream {
                stream: &mut raw,
                end,
            };
            let request: Value =
                aterm_json::from_slice(&read_frame(&mut stream, MAX_HEADER).unwrap()).unwrap();
            assert!(read_frame(&mut stream, MAX_BODY).unwrap().is_empty());
            assert_eq!(request["account"].as_str(), Some("one@proton.me"));
            if index == 0 {
                assert_eq!(request["operation"].as_str(), Some("key"));
            } else {
                assert_eq!(request["operation"].as_str(), Some("request"));
                assert_eq!(request["method"].as_str(), Some("GET"));
                assert_eq!(request["path"].as_str(), Some("/core/v4/addresses"));
            }
            received += 1;
            let (header, body) = match reply {
                Reply::Key => (json!({}), b"owned-mailbox-key".to_vec()),
                Reply::KeyDenied => {
                    std::fs::set_permissions(
                        &endpoint.socket,
                        std::fs::Permissions::from_mode(0o666),
                    )
                    .unwrap();
                    (json!({}), b"owned-mailbox-key".to_vec())
                }
                Reply::Error(kind) => (
                    json!({"error":"owned setup failure", "kind":format!("{kind:?}")}),
                    Vec::new(),
                ),
                Reply::RateLimited(retry_after) => (
                    json!({"status":429, "retry_after":retry_after}),
                    br#"{"Code":429}"#.to_vec(),
                ),
                Reply::Header(header) => (header, Vec::new()),
                Reply::Malformed => {
                    write_frame(&mut stream, b"{", MAX_HEADER).unwrap();
                    continue;
                }
                Reply::Oversized => {
                    stream
                        .write_all(&((MAX_HEADER as u32) + 1).to_be_bytes())
                        .unwrap();
                    continue;
                }
                Reply::Lost => continue,
            };
            write_frame(
                &mut stream,
                &aterm_json::to_vec(&header).unwrap(),
                MAX_HEADER,
            )
            .unwrap();
            write_frame(&mut stream, &body, MAX_BODY).unwrap();
        }
        (listener, received)
    });
    let error = mailbox(
        home.path(),
        Credentials {
            email: "one@proton.me",
            login: "one",
            password: "owned-password",
            mailbox_password: None,
            totp_secret: None,
        },
        Some(Instant::now() + Duration::from_secs(60)),
    )
    .err()
    .expect("the owned setup must fail");
    let (listener, received) = peer.join().unwrap();
    assert_eq!(received, expected);
    assert_eq!(
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Nonblocking,
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::WouldBlock,
        "setup does not replay a failed exchange"
    );
    error
}

#[test]
fn address_setup_failures_keep_their_typed_recovery_without_replay() {
    for (kind, recovery) in [
        (Kind::Network, Recovery::RetryRead),
        (Kind::Timeout, Recovery::RetryRead),
        (Kind::Credentials, Recovery::Stop),
        (Kind::Verification, Recovery::Verify),
        (Kind::AccessDenied, Recovery::Stop),
    ] {
        let error = open_failure(vec![Reply::Key, Reply::Error(kind)]);
        let failure = error
            .proton_failure
            .expect("address failure was classified");
        assert_eq!(failure.operation, Operation::Addresses);
        assert_eq!(failure.kind, kind);
        assert_eq!(failure.recovery(), recovery);
    }
}

#[test]
fn setup_preserves_server_retry_deadlines_and_stops_on_unparsed_deadlines() {
    for (header, seconds, recovery) in [
        ("47", Some(47), Recovery::RetryRead),
        ("later", None, Recovery::Stop),
    ] {
        let error = open_failure(vec![Reply::Key, Reply::RateLimited(header)]);
        let failure = error.proton_failure.expect("rate limit was classified");
        assert_eq!(failure.operation, Operation::Addresses);
        assert_eq!(failure.kind, Kind::RateLimited);
        assert_eq!(failure.retry_after_seconds, seconds);
        assert_eq!(failure.retry_after_unparsed, seconds.is_none());
        assert_eq!(failure.recovery(), recovery);
    }
}

#[test]
fn key_setup_failures_do_not_invent_a_proton_read_recovery() {
    for reply in [
        Reply::Error(Kind::Network),
        Reply::Error(Kind::Credentials),
        Reply::Lost,
    ] {
        let error = open_failure(vec![reply]);
        assert!(error.proton_failure.is_none());
    }
}

#[test]
fn actual_owner_io_denials_and_invalid_frames_stop_read_recovery() {
    for (replies, kind) in [
        (vec![Reply::KeyDenied], Kind::AccessDenied),
        (vec![Reply::Key, Reply::Oversized], Kind::Protocol),
        (vec![Reply::Key, Reply::Malformed], Kind::Protocol),
        (vec![Reply::Key, Reply::Header(json!([]))], Kind::Protocol),
        (vec![Reply::Key, Reply::Header(json!({}))], Kind::Protocol),
        (
            vec![Reply::Key, Reply::Header(json!({"status":60000}))],
            Kind::Protocol,
        ),
        (
            vec![
                Reply::Key,
                Reply::Header(json!({"status":429,"retry_after":false})),
            ],
            Kind::Protocol,
        ),
        (
            vec![
                Reply::Key,
                Reply::Header(json!({"error":false,"kind":"Network"})),
            ],
            Kind::Protocol,
        ),
    ] {
        let error = open_failure(replies);
        let failure = error
            .proton_failure
            .expect("the address read retains its I/O classification");
        assert_eq!(failure.operation, Operation::Addresses);
        assert_eq!(failure.kind, kind);
        assert_eq!(failure.recovery(), Recovery::Stop);
    }
}
