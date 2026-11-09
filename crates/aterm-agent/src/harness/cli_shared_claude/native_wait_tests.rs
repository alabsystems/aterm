// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::harness::cli::AUTH_CHECK_BUDGET;
use std::io::{Read as _, Write as _};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

const PROFILE: &[u8] = br#"{"account":{"uuid":"owned-account","email":"owned@example.test"},"organization":{"uuid":"owned-org"}}"#;

struct Provider {
    url: String,
    stop: Arc<AtomicBool>,
    peer: Option<std::thread::JoinHandle<usize>>,
}

impl Provider {
    fn new(status: u16, body: &'static [u8]) -> Self {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!(
            "http://{}/api/oauth/profile",
            listener.local_addr().unwrap()
        );
        let stop = Arc::new(AtomicBool::new(false));
        let peer = std::thread::spawn({
            let stop = Arc::clone(&stop);
            move || {
                let until = Instant::now() + Duration::from_secs(60);
                let mut calls = 0;
                loop {
                    let mut stream = match listener.accept() {
                        Ok((stream, _)) => stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if stop.load(Ordering::Acquire) {
                                return calls;
                            }
                            assert!(Instant::now() < until);
                            std::thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(error) => panic!("owned OAuth profile provider: {error}"),
                    };
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(60)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(60)))
                        .unwrap();
                    let mut head = Vec::new();
                    while !head.ends_with(b"\r\n\r\n") {
                        let mut byte = [0];
                        stream.read_exact(&mut byte).unwrap();
                        head.push(byte[0]);
                        assert!(head.len() < 16384);
                    }
                    let head = std::str::from_utf8(&head).unwrap();
                    assert!(head.starts_with("GET /api/oauth/profile HTTP/1.1\r\n"));
                    for (wanted_name, wanted_value) in [
                        ("authorization", "Bearer owned-access"),
                        ("user-agent", "claude-cli/aterm"),
                        ("cache-control", "no-cache"),
                    ] {
                        assert!(head.lines().any(|line| {
                            line.split_once(':').is_some_and(|(name, value)| {
                                name.eq_ignore_ascii_case(wanted_name)
                                    && value.trim() == wanted_value
                            })
                        }));
                    }
                    calls += 1;
                    let mut reply = format!("HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
                    reply.extend_from_slice(body);
                    stream.write_all(&reply).unwrap();
                }
            }
        });
        Self {
            url,
            stop,
            peer: Some(peer),
        }
    }

    fn finish(&mut self) -> usize {
        self.stop.store(true, Ordering::Release);
        self.peer.take().unwrap().join().unwrap()
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(peer) = self.peer.take() {
            let _ = peer.join();
        }
    }
}

fn read(
    provider: &Provider,
    age: u64,
    within: Duration,
) -> Result<ClaudeProfile, ObservationError> {
    read_started(
        "owned-access",
        within,
        Instant::now() - Duration::from_secs(age),
        &provider.url,
        ProxyMode::Direct,
    )
}

#[test]
fn required_native_shared_profile_keeps_original_deadline_and_exact_identity() {
    for (age, short_parent) in [(21, false), (61, false), (0, false), (2, true)] {
        let mut provider = Provider::new(200, PROFILE);
        let got = read(
            &provider,
            age,
            if short_parent {
                Duration::from_secs(1)
            } else {
                AUTH_CHECK_BUDGET
            },
        );
        let expired = age == 61 || short_parent;
        let fresh = expired.then(|| read(&provider, 0, AUTH_CHECK_BUDGET));
        assert_eq!(
            provider.finish(),
            1,
            "expired work sends no request; only the new check reaches the provider"
        );
        let got = if expired {
            assert!(matches!(got, Err(ObservationError::Unavailable(_))));
            fresh.unwrap().unwrap()
        } else {
            got.unwrap()
        };
        assert_eq!(got.email, "owned@example.test");
        assert_eq!(
            got.principal,
            profile::principal_id("owned-account").unwrap()
        );
    }
}

#[test]
fn longer_native_shared_wait_keeps_rejection_shape_and_approved_account_checks() {
    for (status, body, rejected) in [
        (401, PROFILE, true),
        (503, PROFILE, false),
        (200, b"not json".as_slice(), false),
        (200, br#"{"account":{"uuid":"","email":"owned@example.test"},"organization":{"uuid":"owned-org"}}"#.as_slice(), false),
        (200, br#"{"account":{"uuid":"owned-account","email":"owned@example.test"},"organization":{"uuid":""}}"#.as_slice(), false),
    ] {
        let mut provider = Provider::new(status, body);
        let got = read(&provider, 21, AUTH_CHECK_BUDGET);
        assert_eq!(provider.finish(), 1);
        assert!(if rejected { matches!(got, Err(ObservationError::RecoveryRequired(_))) } else { matches!(got, Err(ObservationError::Unavailable(_))) });
    }
    let credentials = br#"{"claudeAiOauth":{"accessToken":"owned-access","expiresAt":4000000}}"#;
    let mut provider = Provider::new(200, PROFILE);
    let got = verify_credentials(credentials, "other@example.test", 1000, |_| {
        read(&provider, 21, AUTH_CHECK_BUDGET)
    });
    assert_eq!(provider.finish(), 1);
    assert!(matches!(got, Err(ObservationError::RecoveryRequired(_))));

    let mut provider = Provider::new(200, PROFILE);
    let got = read_started(
        "bad\r\nheader",
        AUTH_CHECK_BUDGET,
        Instant::now(),
        &provider.url,
        ProxyMode::Direct,
    );
    assert!(matches!(got, Err(ObservationError::RecoveryRequired(_))));
    assert_eq!(provider.finish(), 0);
}
