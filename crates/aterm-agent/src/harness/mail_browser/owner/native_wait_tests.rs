// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

struct Provider {
    home: aterm_tempfile::TempDir,
    stop: Arc<AtomicBool>,
    peer: Option<std::thread::JoinHandle<(usize, usize)>>,
}

impl Provider {
    fn new(ages: Vec<u64>, refusal: bool) -> Self {
        let home = aterm_tempfile::Builder::new()
            .prefix("mo-wait-")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .unwrap();
        let endpoint = Endpoint::new(home.path(), "one@proton.me").unwrap();
        let listener = aterm_uds::kept_out(|| CtlListener::bind(&endpoint.socket)).unwrap();
        std::fs::set_permissions(&endpoint.socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let peer = std::thread::spawn({
            let stop = Arc::clone(&stop);
            move || {
                let mut ages = std::collections::VecDeque::from(ages);
                let until = Instant::now() + Duration::from_secs(60);
                let (mut received, mut admitted) = (0, 0);
                loop {
                    let mut raw = match aterm_uds::accept(
                        &listener,
                        aterm_uds::AcceptWait::Ready,
                        aterm_uds::AcceptedMode::Nonblocking,
                    ) {
                        Ok((stream, _)) => stream,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            if stop.load(Ordering::Acquire) {
                                return (received, admitted);
                            }
                            assert!(Instant::now() < until);
                            std::thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(error) => panic!("owned mailbox socket: {error}"),
                    };
                    private_peer(&raw).unwrap();
                    let mut input = DeadlineStream {
                        stream: &mut raw,
                        end: Instant::now() + Duration::from_secs(60),
                    };
                    let request: Value =
                        aterm_json::from_slice(&read_frame(&mut input, MAX_HEADER).unwrap())
                            .unwrap();
                    assert_eq!(request["operation"].as_str(), Some("request"));
                    assert_eq!(request["path"].as_str(), Some("/core/v4/addresses"));
                    match request["method"].as_str() {
                        Some("GET") => {
                            assert!(read_frame(&mut input, MAX_BODY).unwrap().is_empty())
                        }
                        Some("POST") => assert_eq!(
                            read_frame(&mut input, MAX_BODY).unwrap().as_slice(),
                            b"{\"owned\":true}"
                        ),
                        method => panic!("unexpected owned method {method:?}"),
                    }
                    received += 1;
                    let age = ages.pop_front().unwrap_or(0);
                    let admitted_end = request_end_started(
                        &request,
                        "one@proton.me",
                        Instant::now() - Duration::from_secs(age),
                    );
                    let (header, body, end) = match admitted_end {
                        Ok(end) => {
                            admitted += 1;
                            if refusal {
                                (
                                    json!({"error":"owned provider refusal", "kind":"Credentials"}),
                                    b"".as_slice(),
                                    end,
                                )
                            } else {
                                (json!({"status":200}), b"owned-response".as_slice(), end)
                            }
                        }
                        Err(error) => (
                            json!({"error":error.message, "kind":format!("{:?}", error.kind)}),
                            b"".as_slice(),
                            Instant::now() + Duration::from_secs(60),
                        ),
                    };
                    let header = aterm_json::to_vec(&header).unwrap();
                    let mut output = DeadlineStream {
                        stream: &mut raw,
                        end,
                    };
                    write_frame(&mut output, &header, MAX_HEADER).unwrap();
                    write_frame(&mut output, body, MAX_BODY).unwrap();
                }
            }
        });
        Self {
            home,
            stop,
            peer: Some(peer),
        }
    }

    fn remote(&self, end: Option<Instant>) -> RemoteTransport {
        RemoteTransport {
            home: self.home.path().into(),
            credentials: Credentials {
                email: "one@proton.me",
                login: "one",
                password: "owned-password",
                mailbox_password: None,
                totp_secret: None,
            }
            .into(),
            end,
        }
    }

    fn finish(&mut self) -> (usize, usize) {
        self.stop.store(true, Ordering::Release);
        self.peer.take().unwrap().join().unwrap()
    }

    fn raw(&self, version: u64, account: &str) -> Value {
        let end = Instant::now() + Duration::from_secs(60);
        let endpoint = Endpoint::new(self.home.path(), "one@proton.me").unwrap();
        let mut raw = endpoint.connect(end).unwrap();
        raw.set_nonblocking(true).unwrap();
        let mut stream = DeadlineStream {
            stream: &mut raw,
            end,
        };
        let header = aterm_json::to_vec(&json!({"version":version, "account":account, "expires":monotonic_ms().unwrap()+60_000, "operation":"request", "method":"GET", "path":"/core/v4/addresses"})).unwrap();
        write_frame(&mut stream, &header, MAX_HEADER).unwrap();
        write_frame(&mut stream, &[], MAX_BODY).unwrap();
        let reply = aterm_json::from_slice(&read_frame(&mut stream, MAX_HEADER).unwrap()).unwrap();
        assert!(read_frame(&mut stream, MAX_BODY).unwrap().is_empty());
        reply
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
    remote: &RemoteTransport,
    age: u64,
    post: bool,
) -> Result<(Value, Zeroizing<Vec<u8>>), ApiError> {
    remote.exchange_started(json!({"operation":"request", "method":if post { "POST" } else { "GET" }, "path":"/core/v4/addresses"}), if post { b"{\"owned\":true}" } else { b"" }, Instant::now() - Duration::from_secs(age))
}

fn answered(got: Result<(Value, Zeroizing<Vec<u8>>), ApiError>) {
    let (reply, body) = got.unwrap();
    assert_eq!(reply["status"].as_u64(), Some(200));
    assert_eq!(body.as_slice(), b"owned-response");
}

#[test]
fn required_native_owner_client_keeps_original_wait_and_sends_no_expired_frame() {
    for post in [false, true] {
        for (age, parent_expired) in [(31, false), (61, false), (0, false), (0, true)] {
            let mut provider = Provider::new(vec![0], false);
            let remote =
                provider.remote(parent_expired.then(|| Instant::now() - Duration::from_secs(1)));
            let got = exchange(&remote, age, post);
            if age == 61 || parent_expired {
                assert!(matches!(
                    got,
                    Err(ApiError {
                        kind: Kind::Network,
                        ..
                    })
                ));
                // A separately admitted read recovers; no unanswered write is replayed.
                answered(exchange(&provider.remote(None), 0, false));
            } else {
                answered(got);
            }
            assert_eq!(provider.finish(), (1, 1));
        }
    }
}

#[test]
fn required_native_owner_server_keeps_original_wait_and_protocol_refusals() {
    for post in [false, true] {
        for age in [31, 61] {
            let mut provider = Provider::new(vec![age, 0], false);
            let got = exchange(&provider.remote(None), 0, post);
            if age == 61 {
                assert!(matches!(
                    got,
                    Err(ApiError {
                        kind: Kind::Network,
                        ..
                    })
                ));
                answered(exchange(&provider.remote(None), 0, false));
                assert_eq!(provider.finish(), (2, 1));
            } else {
                answered(got);
                assert_eq!(provider.finish(), (1, 1));
            }
        }
    }
    let mut provider = Provider::new(vec![31], true);
    assert!(matches!(
        exchange(&provider.remote(None), 0, false),
        Err(ApiError {
            kind: Kind::Credentials,
            ..
        })
    ));
    assert_eq!(provider.finish(), (1, 1));

    for (version, account) in [(2, "one@proton.me"), (1, "two@proton.me")] {
        let mut provider = Provider::new(vec![31], false);
        let reply = provider.raw(version, account);
        assert_eq!(reply["kind"].as_str(), Some("Refused"));
        assert_eq!(provider.finish(), (1, 0));
    }
}
