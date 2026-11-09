// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::io::{Read as _, Write as _};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

const VALID: &[u8] = br#"{"access_token":"owned-access","refresh_token":"owned-rotated","expires_in":3600,"token_type":"Bearer","scope":"https://mail.google.com/"}"#;

struct Provider {
    endpoint: String,
    stop: Arc<AtomicBool>,
    peer: Option<std::thread::JoinHandle<usize>>,
}

impl Provider {
    fn new(expected: &'static [u8], status: u16, body: &'static [u8]) -> Self {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
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
                            assert!(
                                Instant::now() < until,
                                "owned token provider did not finish"
                            );
                            std::thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(error) => panic!("owned token provider: {error}"),
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
                    let head = std::str::from_utf8(&head).unwrap();
                    assert!(head.starts_with("POST /token HTTP/1.1\r\n"));
                    assert!(head.lines().any(|line| {
                        line.eq_ignore_ascii_case("content-type: application/x-www-form-urlencoded")
                    }));
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
                    assert_eq!(form, expected);
                    calls += 1;
                    let head = format!(
                        "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    stream.write_all(head.as_bytes()).unwrap();
                    stream.write_all(body).unwrap();
                }
            }
        });
        Self {
            endpoint,
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

#[test]
fn required_native_google_tokens_keep_forms_rotation_and_original_deadlines() {
    let code = [
        ("client_id", "owned-client"),
        ("code", "owned/code"),
        ("code_verifier", "owned/verifier"),
        ("redirect_uri", "http://127.0.0.1/callback"),
        ("grant_type", "authorization_code"),
        ("client_secret", "owned-secret"),
    ];
    let refresh = [
        ("client_id", "owned-client"),
        ("refresh_token", "owned/refresh"),
        ("grant_type", "refresh_token"),
    ];
    for (fields,form) in [(&code[..],b"client_id=owned-client&code=owned%2Fcode&code_verifier=owned%2Fverifier&redirect_uri=http%3A%2F%2F127.0.0.1%2Fcallback&grant_type=authorization_code&client_secret=owned-secret".as_slice()),(&refresh[..],b"client_id=owned-client&refresh_token=owned%2Frefresh&grant_type=refresh_token".as_slice())] {
        for (age,expired_parent) in [(31,false),(61,false),(0,false),(0,true)] {
            let mut provider=Provider::new(form,200,VALID);
            let now=Instant::now();
            let got=request_started(fields,if expired_parent {now}else{now+Duration::from_secs(60)},now-Duration::from_secs(age),&provider.endpoint,aterm_http::ProxyMode::Direct);
            let expired=age==61||expired_parent;
            let fresh=expired.then(||request_started(fields,Instant::now()+Duration::from_secs(60),Instant::now(),&provider.endpoint,aterm_http::ProxyMode::Direct));
            assert_eq!(provider.finish(),1,"expiry dispatches nothing and token requests are not replayed");
            let tokens=if expired {assert_eq!(got.err().unwrap().kind,ErrorKind::Transport);fresh.unwrap().unwrap()}else{got.unwrap()};
            assert_eq!(tokens.access_token.expose(),"owned-access");
            assert_eq!(tokens.refresh_token.unwrap().expose(),"owned-rotated");
            assert_eq!(tokens.expires_in,3600);
        }
    }
}

#[test]
fn longer_native_google_wait_keeps_provider_refusals_and_token_validation() {
    let fields = [("grant_type", "refresh_token")];
    for (status, body, kind) in [
        (
            400,
            br#"{"error":"invalid_grant"}"#.as_slice(),
            ErrorKind::Rejected,
        ),
        (503, b"unavailable".as_slice(), ErrorKind::Temporary),
        (
            200,
            br#"{"access_token":"owned","expires_in":3600,"token_type":"Other"}"#.as_slice(),
            ErrorKind::InvalidResponse,
        ),
    ] {
        let mut provider = Provider::new(b"grant_type=refresh_token", status, body);
        let got = request_started(
            &fields,
            Instant::now() + Duration::from_secs(60),
            Instant::now() - Duration::from_secs(31),
            &provider.endpoint,
            aterm_http::ProxyMode::Direct,
        );
        assert_eq!(provider.finish(), 1);
        assert_eq!(got.err().unwrap().kind, kind);
    }
}
