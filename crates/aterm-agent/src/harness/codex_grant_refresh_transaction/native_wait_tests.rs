// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_json::Value;
use std::cell::Cell;
use std::io::{BufRead as _, Write as _};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

struct Provider {
    stop: Arc<AtomicBool>,
    peer: Option<std::thread::JoinHandle<usize>>,
}
impl Provider {
    fn start(listener: std::net::TcpListener, status: u16, body: Vec<u8>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let peer = std::thread::spawn({
            let stop = Arc::clone(&stop);
            move || {
                let until = Instant::now() + Duration::from_secs(60);
                let mut calls = 0;
                loop {
                    let mut stream = match aterm_uds::accept(
                        &listener,
                        aterm_uds::AcceptWait::Ready,
                        aterm_uds::AcceptedMode::Blocking,
                    ) {
                        Ok((stream, _)) => stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if stop.load(Ordering::Acquire) {
                                return calls;
                            }
                            assert!(Instant::now() < until);
                            std::thread::sleep(Duration::from_millis(5));
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
                    let mut reader = std::io::BufReader::new(&stream);
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    assert_eq!(line, "POST /oauth/token HTTP/1.1\r\n");
                    let mut length = None;
                    loop {
                        line.clear();
                        reader.read_line(&mut line).unwrap();
                        assert!(!line.is_empty());
                        if line == "\r\n" {
                            break;
                        }
                        if let Some((name, value)) = line.split_once(':')
                            && name.eq_ignore_ascii_case("content-length")
                        {
                            length = Some(value.trim().parse::<usize>().unwrap());
                        }
                    }
                    let mut sent = vec![0; length.unwrap()];
                    reader.read_exact(&mut sent).unwrap();
                    let sent: Value = aterm_json::from_slice(&sent).unwrap();
                    assert_eq!(sent["grant_type"].as_str(), Some("refresh_token"));
                    assert_eq!(sent["client_id"].as_str(), Some(codex_app_login::CLIENT_ID));
                    assert_eq!(sent["refresh_token"].as_str(), Some("fixture-refresh-only"));
                    drop(reader);
                    calls += 1;
                    let mut reply = format!("HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
                    reply.extend_from_slice(&body);
                    stream.write_all(&reply).unwrap();
                }
            }
        });
        Self {
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

struct Case {
    // Stop the provider and release native custody before deleting the private world.
    provider: Provider,
    work: RefreshWorkLease,
    _fixture: super::super::super::maintenance_tests::Fixture,
    document: Vec<u8>,
    response: Vec<u8>,
    status: u16,
}
impl Case {
    fn new(status: u16, malformed: bool) -> Self {
        let fixture = super::super::super::maintenance_tests::fixture();
        let work = RefreshWorkLease::for_env(&fixture.env).unwrap();
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let Value::Object(mut previous) = aterm_json::from_slice::<Value>(&fixture.bytes).unwrap()
        else {
            panic!("fixture document must be an object");
        };
        let Value::Object(mut tokens) = previous.remove("tokens").unwrap() else {
            panic!("fixture tokens must be an object");
        };
        let response = if malformed {
            b"not json".to_vec()
        } else if status == 200 {
            aterm_json::to_vec(&aterm_json::json!({"access_token":tokens.get("access_token").unwrap(), "refresh_token":"owned-rotated", "token_type":"Bearer", "expires_in":3600})).unwrap()
        } else {
            br#"{"error":"owned-provider-response"}"#.to_vec()
        };
        tokens.insert(
            "oauth_provenance".into(),
            aterm_json::json!({"issuer":issuer, "client_id":codex_app_login::CLIENT_ID}),
        );
        previous.insert("tokens".into(), Value::Object(tokens));
        let document = aterm_json::to_vec(&Value::Object(previous)).unwrap();
        let provider = Provider::start(listener, status, response.clone());
        Self {
            provider,
            work,
            _fixture: fixture,
            document,
            response,
            status,
        }
    }

    fn exchange(
        &self,
        age: u64,
        expired_parent: bool,
    ) -> (Result<codex_app_login::RefreshExchange, String>, usize) {
        let first = Cell::new(true);
        let started = Instant::now() - Duration::from_secs(age);
        let clock = || {
            if first.replace(false) {
                started
            } else {
                Instant::now()
            }
        };
        let deadline = if expired_parent {
            crate::harness::auth_deadline::Deadline::after(Duration::ZERO)
        } else {
            crate::harness::auth_deadline::Deadline::new(REFRESH_BUDGET, &clock)
        };
        let mut checked = 0;
        let result = codex_app_login::exchange_owned_refresh(&self.document, deadline, || {
            checked += 1;
            self.work.current()
        });
        self.work.current().unwrap();
        (result, checked)
    }

    fn completed(&self, got: Result<codex_app_login::RefreshExchange, String>) {
        let response = match got {
            Ok(codex_app_login::RefreshExchange::Completed(response)) => response,
            other => panic!("owned native response was not retained: {other:?}"),
        };
        assert_eq!(response.status(), self.status);
        assert_eq!(response.body(), self.response);
        let merged = codex_app_login::merge_owned_refresh(
            &self.document,
            response.status(),
            response.body(),
            response.received_at().unwrap(),
        );
        assert_eq!(
            merged.is_ok(),
            self.status == 200 && self.response != b"not json"
        );
    }
}

#[test]
fn required_native_owner_parent_keeps_original_time_and_starts_no_expired_exchange() {
    for (age, expired_parent) in [(31, false), (61, false), (0, false), (0, true)] {
        let mut case = Case::new(200, false);
        let (got, checked) = case.exchange(age, expired_parent);
        if age == 61 || expired_parent {
            assert!(matches!(
                got,
                Err(_) | Ok(codex_app_login::RefreshExchange::NotSent)
            ));
            assert_eq!(checked, 0);
            let (fresh, checked) = case.exchange(0, false);
            assert_eq!(checked, 1);
            case.completed(fresh);
        } else {
            assert_eq!(checked, 1);
            case.completed(got);
        }
        assert_eq!(
            case.provider.finish(),
            1,
            "only a new operation after unsent expiry reaches the native provider"
        );
    }
}

#[test]
fn longer_native_owner_parent_keeps_real_lease_and_raw_response_custody() {
    for (status, malformed) in [(200, false), (200, true), (403, false), (503, false)] {
        let mut case = Case::new(status, malformed);
        let (got, checked) = case.exchange(31, false);
        assert_eq!(checked, 1);
        case.completed(got);
        assert_eq!(case.provider.finish(), 1);
    }
}
