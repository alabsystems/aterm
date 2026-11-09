// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::io::{Read as _, Write as _};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

const EMAIL: &str = "owned@gmail.com";
const PROFILE: &[u8] =
    br#"{"sub":"owned-subject","email":"owned@gmail.com","email_verified":true}"#;
struct Provider {
    url: String,
    stop: Arc<AtomicBool>,
    peer: Option<std::thread::JoinHandle<usize>>,
}
impl Provider {
    fn new(status: u16, body: &'static [u8]) -> Self {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/v1/userinfo", listener.local_addr().unwrap());
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
                        Err(error) => panic!("owned userinfo provider: {error}"),
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
                    assert!(head.starts_with("GET /v1/userinfo HTTP/1.1\r\n"));
                    assert!(head.lines().any(|line| {
                        line.eq_ignore_ascii_case("authorization: Bearer owned-access")
                    }));
                    assert!(
                        head.lines()
                            .any(|line| line.eq_ignore_ascii_case("cache-control: no-cache"))
                    );
                    calls += 1;
                    let mut reply=format!("HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).into_bytes();
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
fn read(provider: &Provider, age: u64, within: Duration) -> Result<GoogleProfile, Error> {
    read_started(
        "owned-access",
        EMAIL,
        within,
        Instant::now() - Duration::from_secs(age),
        &provider.url,
        ProxyMode::Direct,
    )
}
#[test]
fn required_native_google_profile_keeps_its_original_wait_and_exact_identity() {
    let expected = profile(&aterm_json::from_slice(PROFILE).unwrap(), EMAIL).unwrap();
    for (age, short_parent) in [(31, false), (61, false), (0, false), (2, true)] {
        let mut provider = Provider::new(200, PROFILE);
        let got = read(
            &provider,
            age,
            if short_parent {
                Duration::from_secs(1)
            } else {
                REQUEST_HANG
            },
        );
        let expired = age == 61 || short_parent;
        let fresh = expired.then(|| read(&provider, 0, REQUEST_HANG));
        assert_eq!(
            provider.finish(),
            1,
            "expiry sends no request; a fresh check does not replay the old one"
        );
        let got = if expired {
            assert!(matches!(got, Err(Error::Unavailable)));
            fresh.unwrap().unwrap()
        } else {
            got.unwrap()
        };
        assert_eq!(got.email, expected.email);
        assert_eq!(got.principal, expected.principal);
    }
}
#[test]
fn longer_native_google_wait_keeps_canonical_unauthorized_and_account_checks() {
    for (status, body, kind) in [
        (401, PROFILE, 0),
        (503, PROFILE, 1),
        (
            200,
            br#"{"sub":"owned-subject","email":"owned@gmail.com","email_verified":false}"#
                .as_slice(),
            2,
        ),
        (
            200,
            br#"{"sub":"owned-subject","email":"other@gmail.com","email_verified":true}"#
                .as_slice(),
            2,
        ),
    ] {
        let mut provider = Provider::new(status, body);
        let got = read(&provider, 31, REQUEST_HANG);
        assert_eq!(provider.finish(), 1);
        assert!(match kind {
            0 => matches!(got, Err(Error::CredentialRejected)),
            1 => matches!(got, Err(Error::Unavailable)),
            _ => matches!(got, Err(Error::Refused(_))),
        });
    }
    let mut provider = Provider::new(200, PROFILE);
    assert!(matches!(
        read_started(
            "bad\r\nheader",
            EMAIL,
            REQUEST_HANG,
            Instant::now(),
            &provider.url,
            ProxyMode::Direct
        ),
        Err(Error::Refused(_))
    ));
    assert_eq!(provider.finish(), 0);
}
