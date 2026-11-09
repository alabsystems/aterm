// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::io::Read as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const FORM: &[u8]=b"grant_type=refresh_token&client_id=17e5f671-d194-4dfb-9706-5516cb48c098&refresh_token=owned%2Frefresh";
const RESPONSE: &[u8]=br#"{"access_token":"owned-access","refresh_token":"owned-rotated","expires_in":3600,"scope":"coding","diagnostic":"discard-me"}"#;

struct Provider {
    url: String,
    stop: Arc<AtomicBool>,
    peer: Option<std::thread::JoinHandle<usize>>,
}
impl Provider {
    fn new(status: u16, body: &'static [u8]) -> Self {
        Self::new_case(status, body, false)
    }

    fn new_case(status: u16, body: &'static [u8], withdrawn: bool) -> Self {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/oauth/token", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let peer = std::thread::spawn({
            let stop = Arc::clone(&stop);
            move || {
                let until = Instant::now() + Duration::from_secs(60);
                let mut calls = 0;
                loop {
                    let slice = std::cmp::min(until, Instant::now() + Duration::from_millis(5));
                    let mut stream = match aterm_uds::accept(
                        &listener,
                        aterm_uds::AcceptWait::Until(slice),
                        aterm_uds::AcceptedMode::Blocking,
                    ) {
                        Ok((stream, _)) => stream,
                        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                            if stop.load(Ordering::Acquire) {
                                return calls;
                            }
                            assert!(Instant::now() < until);
                            continue;
                        }
                        Err(error) => panic!("owned refresh provider: {error}"),
                    };
                    stream
                        .set_read_timeout(Some(Duration::from_secs(60)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(60)))
                        .unwrap();
                    let mut head = Vec::new();
                    while !head.ends_with(b"\r\n\r\n") {
                        let mut byte = [0];
                        let read = stream.read_exact(&mut byte);
                        if withdrawn {
                            assert!(head.is_empty(), "a withdrawn guard sends no HTTP byte");
                            assert_eq!(read.unwrap_err().kind(), std::io::ErrorKind::UnexpectedEof);
                            return calls;
                        }
                        read.unwrap();
                        head.push(byte[0]);
                        assert!(head.len() < 16384);
                    }
                    let head = std::str::from_utf8(&head).unwrap();
                    assert!(head.starts_with("POST /oauth/token HTTP/1.1\r\n"));
                    for (wanted_name, wanted_value) in [
                        ("content-type", "application/x-www-form-urlencoded"),
                        ("accept", "application/json"),
                    ] {
                        assert!(head.lines().any(|line| {
                            line.split_once(':').is_some_and(|(name, value)| {
                                name.eq_ignore_ascii_case(wanted_name)
                                    && value.trim() == wanted_value
                            })
                        }));
                    }
                    let length: usize = head
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                                .map(|(_, value)| value.trim().parse().unwrap())
                        })
                        .unwrap();
                    let mut form = vec![0; length];
                    stream.read_exact(&mut form).unwrap();
                    assert_eq!(form, FORM);
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
fn exchange(
    provider: &Provider,
    age: u64,
    deadline: login_drive::Deadline<'_>,
    guard: Arc<dyn Authority>,
) -> Result<(u16, aterm_secrets::SecretBytes), NativeError> {
    token_response_started(
        &provider.url,
        FORM,
        guard,
        deadline,
        Arc::new(aterm_http::TcpConnector),
        Instant::now() - Duration::from_secs(age),
    )
}

#[test]
fn required_native_kimi_refresh_keeps_its_original_deadline_and_exact_rotation() {
    for (age, expired_parent) in [(31, false), (61, false), (0, false), (0, true)] {
        let mut provider = Provider::new(200, RESPONSE);
        let got = exchange(
            &provider,
            age,
            login_drive::Deadline::after(if expired_parent {
                Duration::ZERO
            } else {
                login_drive::HANG
            }),
            Arc::new(aterm_http::AlwaysAuthorized),
        );
        let expired = age == 61 || expired_parent;
        let fresh = expired.then(|| {
            exchange(
                &provider,
                0,
                login_drive::Deadline::after(login_drive::HANG),
                Arc::new(aterm_http::AlwaysAuthorized),
            )
        });
        assert_eq!(
            provider.finish(),
            1,
            "expired requests send nothing and refresh HTTP is not replayed"
        );
        let (status, body) = if expired {
            assert!(matches!(got, Err(NativeError::Temporary)));
            fresh.unwrap().unwrap()
        } else {
            got.unwrap()
        };
        assert_eq!(status, 200);
        let (candidate, rejected) = response_candidate(status, body.as_slice(), 1000).unwrap();
        assert!(!rejected);
        let value: Value = aterm_json::from_slice(candidate.as_slice()).unwrap();
        assert_eq!(value["access_token"].as_str(), Some("owned-access"));
        assert_eq!(value["refresh_token"].as_str(), Some("owned-rotated"));
        assert_eq!(value["expires_at"].as_i64(), Some(4600));
        assert!(value.get("diagnostic").is_none());
    }
}

#[derive(Debug)]
struct Withdrawn;
impl aterm_http::Guard for Withdrawn {
    fn is_authorized(&self) -> bool {
        false
    }
}
impl Authority for Withdrawn {
    fn receipt_current(&self) -> bool {
        false
    }
}
#[test]
fn longer_native_kimi_wait_keeps_guard_and_structured_provider_refusals() {
    let mut provider = Provider::new_case(200, RESPONSE, true);
    assert!(
        exchange(
            &provider,
            0,
            login_drive::Deadline::after(login_drive::HANG),
            Arc::new(Withdrawn)
        )
        .is_err()
    );
    assert_eq!(provider.finish(), 0);
    for (status,body,rejected) in [(400,br#"{"error":"invalid_grant"}"#.as_slice(),true),(401,b"gateway refusal".as_slice(),false),(200,br#"{"access_token":"owned-access","refresh_token":"owned-refresh","token_type":"Other","expires_in":3600}"#.as_slice(),false)] {
        let mut provider=Provider::new(status,body);let (status,body)=exchange(&provider,31,login_drive::Deadline::after(login_drive::HANG),Arc::new(aterm_http::AlwaysAuthorized)).unwrap();assert_eq!(provider.finish(),1);
        let result=response_candidate(status,body.as_slice(),1000);
        if rejected {assert!(result.unwrap().1);}else{assert!(matches!(result,Err(NativeError::Temporary)));}
    }
}
