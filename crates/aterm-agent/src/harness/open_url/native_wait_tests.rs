// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::cell::Cell;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

fn file_call(path: &Path, age: u64, reads: &Cell<usize>) -> std::io::Result<std::process::Output> {
    let url = format!("file://{}", path.display());
    curl_output_with_started(
        "/usr/bin/curl",
        &["--disable", "--proto", "=file", "-sS"],
        &[("url", &url)],
        &|name| {
            reads.set(reads.get() + 1);
            std::env::var_os(name)
        },
        Instant::now() - Duration::from_secs(age),
    )
}

#[test]
fn required_native_curl_invocation_keeps_original_start_and_output_limits() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("owned-body");
    std::fs::write(&path, b"owned-curl-response").unwrap();
    for age in [31, 61, 0] {
        let reads = Cell::new(0);
        let got = file_call(&path, age, &reads);
        let got = if age == 61 {
            assert_eq!(got.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
            assert_eq!(reads.get(), 0, "expired work builds no command environment");
            file_call(&path, 0, &reads).unwrap()
        } else {
            got.unwrap()
        };
        assert!(reads.get() > 0);
        assert!(got.status.success());
        assert_eq!(got.stdout, b"owned-curl-response");
    }
    let reads = Cell::new(0);
    let absent = file_call(&scratch.path().join("absent"), 31, &reads).unwrap();
    assert!(
        !absent.status.success(),
        "an actual native curl failure stays nonzero"
    );
    let oversized = scratch.path().join("oversized");
    std::fs::write(&oversized, vec![b'x'; CURL_MAX_OUTPUT_BYTES + 1]).unwrap();
    assert!(file_call(&oversized, 0, &reads).is_err());
    let good = file_call(&path, 0, &reads).unwrap();
    assert!(good.status.success());
    assert_eq!(good.stdout, b"owned-curl-response");
    let invalid = curl_output_with_started(
        "/usr/bin/curl",
        &["--disable"],
        &[("url", "bad\0url")],
        &|_| panic!("invalid config must precede command environment"),
        Instant::now(),
    );
    assert_eq!(
        invalid.unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput
    );
}

struct Provider {
    url: String,
    stop: Arc<AtomicBool>,
    peer: Option<std::thread::JoinHandle<usize>>,
}
impl Provider {
    fn new(status: u16) -> Self {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://{}/owned", listener.local_addr().unwrap());
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
                        Err(error) => panic!("owned opener HTTP provider: {error}"),
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
                    assert!(head.starts_with(b"GET /owned HTTP/1.1\r\n"));
                    assert!(std::str::from_utf8(&head).unwrap().lines().any(|line| {
                        line.eq_ignore_ascii_case("authorization: Bearer owned-opener")
                    }));
                    calls += 1;
                    write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Length: 10\r\nConnection: close\r\n\r\nowned-curl").unwrap();
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
fn http_call(
    provider: &Provider,
    age: u64,
    deadline: Deadline<'_>,
) -> std::io::Result<std::process::Output> {
    curl_output_until_started(
        &[
            "--disable",
            "--proto",
            "=http",
            "--noproxy",
            "*",
            "-sS",
            "--fail",
        ],
        &[
            ("url", &provider.url),
            ("header", "Authorization: Bearer owned-opener"),
        ],
        HTTP_REQUEST_HANG,
        deadline,
        Instant::now() - Duration::from_secs(age),
    )
}

#[test]
fn required_native_curl_http_keeps_original_parent_no_expired_wire_and_real_status() {
    for (age, expired_parent) in [(16, false), (61, false), (0, false), (0, true)] {
        let mut provider = Provider::new(200);
        let got = http_call(
            &provider,
            age,
            Deadline::after(if expired_parent {
                Duration::ZERO
            } else {
                Duration::from_secs(120)
            }),
        );
        let expired = age == 61 || expired_parent;
        let fresh =
            expired.then(|| http_call(&provider, 0, Deadline::after(Duration::from_secs(120))));
        assert_eq!(
            provider.finish(),
            1,
            "only a separately admitted fresh request recovers expired work"
        );
        let got = if expired {
            assert_eq!(got.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
            fresh.unwrap().unwrap()
        } else {
            got.unwrap()
        };
        assert!(got.status.success());
        assert_eq!(got.stdout, b"owned-curl");
    }
    let mut provider = Provider::new(403);
    let got = http_call(&provider, 16, Deadline::after(Duration::from_secs(120))).unwrap();
    assert_eq!(provider.finish(), 1);
    assert!(
        !got.status.success(),
        "the real HTTP rejection remains a native curl error"
    );
    assert_eq!(got.status.code(), Some(22));
}
