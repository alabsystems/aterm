// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::io::{Read as _, Write as _};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const HANG: Duration = Duration::from_secs(60);

struct Provider {
    api: String,
    stop: Arc<AtomicBool>,
    peer: Option<std::thread::JoinHandle<usize>>,
}

impl Provider {
    fn new(method: Method, path: String, status: u16, code: u64) -> Self {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let api = format!("http://{}/api", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let peer = std::thread::spawn({
            let stop = Arc::clone(&stop);
            move || {
                let until = Instant::now() + HANG;
                let mut calls = 0;
                loop {
                    let mut stream = match listener.accept() {
                        Ok((stream, _)) => stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if stop.load(Ordering::Acquire) {
                                return calls;
                            }
                            assert!(
                                Instant::now() < until,
                                "owned verification request did not arrive"
                            );
                            std::thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(error) => panic!("owned verification provider: {error}"),
                    };
                    stream.set_nonblocking(false).unwrap();
                    stream.set_read_timeout(Some(HANG)).unwrap();
                    stream.set_write_timeout(Some(HANG)).unwrap();
                    let mut head = Vec::new();
                    while !head.ends_with(b"\r\n\r\n") {
                        let mut byte = [0];
                        stream.read_exact(&mut byte).unwrap();
                        head.push(byte[0]);
                        assert!(head.len() < 16 * 1024);
                    }
                    let head = std::str::from_utf8(&head).unwrap();
                    assert!(
                        head.starts_with(&format!("{} /api{path} HTTP/1.1\r\n", method.as_str()))
                    );
                    for name in ["x-pm-appversion", "user-agent", "accept"] {
                        assert!(head.lines().any(|line| {
                            line.split_once(':').is_some_and(|(key, value)| {
                                key.eq_ignore_ascii_case(name) && !value.trim().is_empty()
                            })
                        }));
                    }
                    calls += 1;
                    let body = aterm_json::to_vec(&aterm_json::json!({"Code":code,"ChallengeDestination":"owned@example.invalid","ChallengeType":"login","Token":"owned-proof"})).unwrap();
                    let head = format!(
                        "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    stream.write_all(head.as_bytes()).unwrap();
                    stream.write_all(&body).unwrap();
                }
            }
        });
        Self {
            api,
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

fn recovery() -> Recovery {
    Recovery {
        token: Secret::new("owned/challenge".into()),
        method: "ownership-email".into(),
        destination: String::new(),
        purpose: Purpose::Unknown,
    }
}

#[test]
fn required_verification_responses_keep_their_challenge_and_single_attempt() {
    for phase in 0..3 {
        let recovery = recovery();
        let before = (
            recovery.token.expose().to_owned(),
            recovery.method.clone(),
            recovery.destination.clone(),
            recovery.purpose,
        );
        let method = if phase == 0 {
            Method::Get
        } else {
            Method::Post
        };
        let mut path = recovery.path();
        if phase == 2 {
            path.push('/');
            crate::oauth::encode_into(&mut path, "owned/code");
        }
        for age in [31, 61, 0] {
            let mut provider = Provider::new(method, path.clone(), 200, 1000);
            let got = request_started(
                method,
                &path,
                Instant::now() - Duration::from_secs(age),
                &provider.api,
                ProxyMode::Direct,
            );
            let expired = age == 61;
            let fresh = expired.then(|| {
                request_started(
                    method,
                    &path,
                    Instant::now(),
                    &provider.api,
                    ProxyMode::Direct,
                )
            });
            assert_eq!(
                provider.finish(),
                1,
                "expiry sends no request and verification is never replayed"
            );
            let value = if expired {
                assert!(got.is_err());
                fresh.unwrap().unwrap()
            } else {
                got.unwrap()
            };
            if phase == 0 {
                assert_eq!(
                    decode_details(&value).unwrap(),
                    ("owned@example.invalid".into(), Purpose::Login)
                );
            } else if phase == 2 {
                let proof = verified(&value, &recovery.method).unwrap();
                assert_eq!(proof.token.expose(), "owned-proof");
                assert_eq!(proof.method, recovery.method);
            }
            assert_eq!(
                (
                    recovery.token.expose().to_owned(),
                    recovery.method.clone(),
                    recovery.destination.clone(),
                    recovery.purpose
                ),
                before
            );
        }
    }
}

#[test]
fn a_longer_verification_allowance_keeps_actual_response_refusals() {
    for (status, code) in [(401, 10013), (200, 0)] {
        let path = recovery().path();
        let mut provider = Provider::new(Method::Get, path.clone(), status, code);
        let got = request_started(
            Method::Get,
            &path,
            Instant::now() - Duration::from_secs(31),
            &provider.api,
            ProxyMode::Direct,
        );
        assert_eq!(provider.finish(), 1);
        assert!(got.is_err());
    }
}
