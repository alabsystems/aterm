// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::io::Write as _;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

struct Provider {
    url: String,
    stop: Arc<AtomicBool>,
    peer: Option<std::thread::JoinHandle<usize>>,
}
impl Provider {
    fn new(status: u16) -> Self {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/owned/index", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let peer = std::thread::spawn({
            let stop = Arc::clone(&stop);
            move || {
                let until = Instant::now() + Duration::from_secs(60);
                let mut calls = 0;
                loop {
                    match aterm_uds::accept(
                        &listener,
                        aterm_uds::AcceptWait::Ready,
                        aterm_uds::AcceptedMode::Blocking,
                    ) {
                        Ok((mut stream, _)) => {
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
                            assert!(head.starts_with("GET /owned/index HTTP/1.1\r\n"));
                            assert!(
                                head.lines().any(
                                    |line| line.eq_ignore_ascii_case("user-agent: aterm-forge")
                                )
                            );
                            calls += 1;
                            stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Length: 14\r\nConnection: close\r\n\r\nowned-registry").as_bytes()).unwrap();
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            if stop.load(Ordering::Acquire) {
                                return calls;
                            }
                            assert!(
                                Instant::now() < until,
                                "owned registry request did not arrive"
                            );
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("owned registry provider: {error}"),
                    }
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

fn registry() -> CratesIo {
    CratesIo {
        client: aterm_http::Client::new(
            aterm_http::Trust::PlatformVerifier,
            aterm_http::ProxyMode::Direct,
            Duration::from_secs(300),
        ),
    }
}

#[test]
fn required_registry_connect_keeps_its_start_and_recovers_without_replay() {
    let registry = registry();
    for age in [31, 61, 0] {
        let mut provider = Provider::new(200);
        let got = registry.get_started(
            &provider.url,
            INDEX_LIMIT,
            Instant::now() - Duration::from_secs(age),
        );
        let fresh = (age == 61).then(|| registry.get(&provider.url, INDEX_LIMIT));
        assert_eq!(
            provider.finish(),
            1,
            "expired connection publishes no request; fresh calls do not replay"
        );
        if age == 61 {
            assert!(got.is_err());
            assert_eq!(fresh.unwrap().unwrap(), b"owned-registry");
        } else {
            assert_eq!(got.unwrap(), b"owned-registry");
        }
    }
}

#[test]
fn longer_registry_connect_keeps_native_status_and_body_limits() {
    for (status, limit) in [(403, INDEX_LIMIT), (200, 2)] {
        let mut provider = Provider::new(status);
        let got = registry().get_started(
            &provider.url,
            limit,
            Instant::now() - Duration::from_secs(31),
        );
        assert_eq!(provider.finish(), 1);
        assert!(got.is_err());
    }
}
