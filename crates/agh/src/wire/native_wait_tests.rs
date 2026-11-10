// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::io::{Read as _, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

struct Provider {
    url: String,
    stop: Arc<AtomicBool>,
    peer: Option<std::thread::JoinHandle<usize>>,
}
impl Provider {
    fn new(status: u16) -> Self {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/user", listener.local_addr().unwrap());
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
                        Err(error) => panic!("owned GitHub provider: {error}"),
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
                        stream.read_exact(&mut byte).unwrap();
                        head.push(byte[0]);
                        assert!(head.len() < 16384);
                    }
                    assert!(
                        std::str::from_utf8(&head)
                            .unwrap()
                            .starts_with("GET /user HTTP/1.1\r\n")
                    );
                    calls += 1;
                    stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Length: 17\r\nConnection: close\r\n\r\n{{\"login\":\"owned\"}}").as_bytes()).unwrap();
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

fn transport() -> HttpTransport {
    HttpTransport::direct(Arc::new(crate::identity::MapEnv::new()))
}
fn request(url: &str, deadlines: Deadlines) -> WireRequest<'static> {
    WireRequest::new(Method::Get, url.to_owned(), Vec::new(), 65536, deadlines)
}
fn answered(
    transport: &HttpTransport,
    url: &str,
    deadlines: Deadlines,
    age: u64,
) -> Result<u16, WireError> {
    let mut response = transport.round_trip_started(
        request(url, deadlines),
        crate::fx::NativeRequestStart::from_started(Instant::now() - Duration::from_secs(age)),
    )?;
    let mut bytes = Vec::new();
    response.body.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, br#"{"login":"owned"}"#);
    Ok(response.status)
}

#[test]
fn required_native_github_defaults_keep_their_original_start_and_recover() {
    let transport = transport();
    for deadlines in [
        Deadlines::JSON,
        Deadlines::for_transfer(17),
        Deadlines::for_upload(17),
        crate::github::users::learn_deadlines(),
    ] {
        for age in [11, 61, 0] {
            let mut provider = Provider::new(200);
            let got = answered(&transport, &provider.url, deadlines, age);
            let fresh = (age == 61).then(|| answered(&transport, &provider.url, deadlines, 0));
            assert_eq!(
                provider.finish(),
                1,
                "expiry sends nothing; the same transport recovers without replay"
            );
            if age == 61 {
                let error = got.unwrap_err();
                assert_eq!(error.phase, Phase::NotSent);
                assert_eq!(error.kind, WireKind::Timeout);
                assert_eq!(fresh.unwrap().unwrap(), 200);
            } else {
                assert_eq!(got.unwrap(), 200);
            }
        }
    }
}

#[test]
fn a_longer_github_default_keeps_explicit_parent_status_cap_and_write_guard() {
    let transport = transport();
    let mut provider = Provider::new(403);
    let expired = answered(
        &transport,
        &provider.url,
        Deadlines::JSON.with_backstop(Duration::from_secs(5)),
        6,
    )
    .unwrap_err();
    assert_eq!(expired.phase, Phase::NotSent);
    assert_eq!(expired.kind, WireKind::Timeout);
    assert_eq!(
        answered(&transport, &provider.url, Deadlines::JSON, 11).unwrap(),
        403
    );
    assert_eq!(provider.finish(), 1);

    let mut provider = Provider::new(200);
    let mut write = request(&provider.url, Deadlines::JSON);
    write.method = Method::Post;
    let error = transport
        .round_trip_started(
            write,
            crate::fx::NativeRequestStart::from_started(Instant::now() - Duration::from_secs(11)),
        )
        .unwrap_err();
    assert_eq!(error.phase, Phase::NotSent);
    assert_eq!(error.kind, WireKind::Local);
    assert_eq!(
        provider.finish(),
        0,
        "a longer wait cannot admit an unpermitted write"
    );

    let mut provider = Provider::new(200);
    let mut bounded = request(&provider.url, Deadlines::JSON);
    bounded.limit = 2;
    let got = transport.round_trip_started(
        bounded,
        crate::fx::NativeRequestStart::from_started(Instant::now() - Duration::from_secs(11)),
    );
    let refused = match got {
        Err(error) => error.kind == WireKind::TooLarge,
        Ok(mut response) => {
            let mut bytes = Vec::new();
            response.body.read_to_end(&mut bytes).is_err()
        }
    };
    assert_eq!(provider.finish(), 1);
    assert!(
        refused,
        "the actual native response still obeys its byte cap"
    );
}
