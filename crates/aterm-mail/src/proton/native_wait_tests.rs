// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::io::{Read as _, Write as _};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const HANG: Duration = Duration::from_secs(60);
const PATH: &str = "/mail/v4/messages";

struct Provider {
    api: String,
    stop: Arc<AtomicBool>,
    peer: Option<std::thread::JoinHandle<usize>>,
}

impl Provider {
    fn new(method: Method, status: u16, code: u64) -> Self {
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
                                "owned native request did not arrive"
                            );
                            std::thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(error) => panic!("owned HTTP provider: {error}"),
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
                        head.starts_with(&format!("{} /api{PATH} HTTP/1.1\r\n", method.as_str()))
                    );
                    for (name, expected) in [
                        ("authorization", "Bearer owned-access"),
                        ("x-pm-uid", "owned-uid"),
                    ] {
                        assert!(head.lines().any(|line| {
                            line.split_once(':').is_some_and(|(key, value)| {
                                key.eq_ignore_ascii_case(name) && value.trim() == expected
                            })
                        }));
                    }
                    let bytes = head
                        .lines()
                        .filter_map(|line| line.split_once(':'))
                        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map_or(0, |(_, value)| value.trim().parse::<usize>().unwrap());
                    assert!(bytes < 1024);
                    let mut body = vec![0; bytes];
                    stream.read_exact(&mut body).unwrap();
                    if matches!(method, Method::Post) {
                        assert_eq!(
                            aterm_json::from_slice::<Value>(&body).unwrap()["Marker"].as_u64(),
                            Some(17)
                        );
                    } else {
                        assert!(body.is_empty());
                    }
                    calls += 1;
                    let body =
                        aterm_json::to_vec(&json!({"Code":code,"Receipt":"owned-proton-response"}))
                            .unwrap();
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

fn session() -> Proton {
    let mut proton = Proton::empty();
    // Keep the production timeout and HTTP backend; the private provider must not use
    // an inherited external proxy.
    proton.client = Client::new(Trust::PlatformVerifier, ProxyMode::Direct, REQUEST_HANG);
    proton.uid = Secret::new("owned-uid".into());
    proton.access = Secret::new("owned-access".into());
    proton.refresh_token = Secret::new("owned-refresh".into());
    proton.expires = 1_900_000_000;
    proton
}

fn credentials(proton: &Proton) -> (String, String, String, u64) {
    (
        proton.uid.expose().to_owned(),
        proton.access.expose().to_owned(),
        proton.refresh_token.expose().to_owned(),
        proton.expires,
    )
}

#[test]
fn required_native_requests_keep_their_original_allowance_and_authority() {
    for method in [Method::Get, Method::Post] {
        for (age, parent_expired) in [(31, false), (61, false), (0, true), (0, false)] {
            let mut provider = Provider::new(method, 200, 1000);
            let mut proton = session();
            let before = credentials(&proton);
            let body = || matches!(method, Method::Post).then(|| json!({"Marker":17}));
            let started = Instant::now() - Duration::from_secs(age);
            let run = |p: &mut Proton| p.call_started(method, PATH, body(), started, &provider.api);
            let got = if parent_expired {
                proton.run_until(Instant::now() - Duration::from_secs(1), run)
            } else {
                run(&mut proton)
            };
            let expired = age == 61 || parent_expired;
            assert!(proton.request_deadline.is_none());
            assert_eq!(credentials(&proton), before);
            assert!(proton.paused.borrow().is_none());
            assert!(proton.last_failure().is_none());
            let fresh = expired
                .then(|| proton.call_started(method, PATH, body(), Instant::now(), &provider.api));
            assert_eq!(
                provider.finish(),
                1,
                "expiry sends no request and a live operation is never replayed"
            );
            if expired {
                assert!(got.is_err());
                assert_eq!(
                    fresh.unwrap().unwrap()["Receipt"].as_str(),
                    Some("owned-proton-response")
                );
            } else {
                assert_eq!(
                    got.unwrap()["Receipt"].as_str(),
                    Some("owned-proton-response")
                );
            }
            assert_eq!(credentials(&proton), before);
        }
    }
}

#[test]
fn a_longer_native_allowance_preserves_actual_http_and_api_refusals() {
    for (status, code) in [(401, 10013), (200, 0)] {
        let mut provider = Provider::new(Method::Get, status, code);
        let proton = session();
        let before = credentials(&proton);
        let got = proton.call_started(
            Method::Get,
            PATH,
            None,
            Instant::now() - Duration::from_secs(31),
            &provider.api,
        );
        assert_eq!(provider.finish(), 1);
        assert!(got.is_err());
        assert_eq!(proton.last_failure().unwrap().api_code, Some(code));
        assert_eq!(credentials(&proton), before);
    }
}
