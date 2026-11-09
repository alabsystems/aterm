// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::io::{Read as _, Write as _};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn head(stream: &mut std::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        bytes.push(byte[0]);
        assert!(bytes.len() < 16384);
    }
    String::from_utf8(bytes).unwrap()
}
struct Proxy {
    url: String,
    stop: Arc<AtomicBool>,
    peer: Option<std::thread::JoinHandle<usize>>,
}
impl Proxy {
    fn new(status: u16) -> Self {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
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
                        Err(error) => panic!("owned curl proxy: {error}"),
                    };
                    stream
                        .set_read_timeout(Some(Duration::from_secs(60)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(60)))
                        .unwrap();
                    assert!(head(&mut stream).starts_with("CONNECT 127.0.0.1:1 HTTP/1.1\r\n"));
                    stream
                        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                        .unwrap();
                    let request = head(&mut stream);
                    assert!(request.starts_with("GET /owned HTTP/1.1\r\n"));
                    assert!(request.lines().any(|line| {
                        line.eq_ignore_ascii_case("authorization: Bearer owned-token")
                    }));
                    calls += 1;
                    stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Length: 10\r\nConnection: close\r\n\r\nowned-curl").as_bytes()).unwrap();
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
impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(peer) = self.peer.take() {
            let _ = peer.join();
        }
    }
}
#[derive(Default)]
struct Counted {
    calls: AtomicUsize,
}
impl Spawn for Counted {
    fn run(
        &self,
        command: &mut aterm_uds::spawnfd::Command,
        stdin: Option<&[u8]>,
    ) -> std::io::Result<std::process::Output> {
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        for (name, wanted) in [("--max-time", "120"), ("--max-filesize", "8388608")] {
            assert!(
                args.windows(2)
                    .any(|pair| pair[0] == name && pair[1] == wanted)
            );
        }
        assert_eq!(
            stdin,
            Some(b"Authorization: Bearer owned-token\n".as_slice())
        );
        self.calls.fetch_add(1, Ordering::AcqRel);
        aterm_update_core::github_rest::Direct.run(command, stdin)
    }
}
fn call(client: &Client, proxy: &Proxy, age: u64) -> Result<Answer, Unanswered> {
    let secret = Secret::new(b"Authorization: Bearer owned-token\n");
    client.run_started(
        "http://127.0.0.1:1/owned",
        &[
            "-X",
            "GET",
            "--proxy",
            &proxy.url,
            "--proxytunnel",
            "--noproxy",
            "",
            "-H",
            "@-",
        ],
        Some(&secret),
        Instant::now() - Duration::from_secs(age),
    )
}
#[test]
fn required_native_curl_connect_keeps_its_start_and_expired_work_spawns_nothing() {
    let root = aterm_tempfile::tempdir().unwrap();
    let spawn = Arc::new(Counted::default());
    let origin = "http://127.0.0.1:1";
    let client = Client::new(
        Origins {
            token: format!("{origin}/token"),
            api: origin.into(),
            upload: origin.into(),
        },
        spawn.clone(),
        root.path(),
    )
    .unwrap();
    for age in [16, 61, 0] {
        let before = spawn.calls.load(Ordering::Acquire);
        let mut proxy = Proxy::new(200);
        let got = call(&client, &proxy, age);
        if age == 61 {
            assert!(matches!(got, Err(Unanswered::Unsent(_))));
            assert_eq!(spawn.calls.load(Ordering::Acquire), before);
            let answer = call(&client, &proxy, 0).unwrap();
            assert_eq!(answer.status, 200);
            assert_eq!(answer.body, b"owned-curl");
        } else {
            let answer = got.unwrap();
            assert_eq!(answer.status, 200);
            assert_eq!(answer.body, b"owned-curl");
        }
        assert_eq!(proxy.finish(), 1);
        assert_eq!(
            spawn.calls.load(Ordering::Acquire),
            before + 1,
            "no request replay"
        );
    }
}
#[test]
fn longer_native_curl_connect_keeps_real_status_and_credential_delivery() {
    let root = aterm_tempfile::tempdir().unwrap();
    let spawn = Arc::new(Counted::default());
    let origin = "http://127.0.0.1:1";
    let client = Client::new(
        Origins {
            token: format!("{origin}/token"),
            api: origin.into(),
            upload: origin.into(),
        },
        spawn.clone(),
        root.path(),
    )
    .unwrap();
    let mut proxy = Proxy::new(403);
    let answer = call(&client, &proxy, 16).unwrap();
    assert_eq!(proxy.finish(), 1);
    assert_eq!(answer.status, 403);
    assert_eq!(spawn.calls.load(Ordering::Acquire), 1);
}
