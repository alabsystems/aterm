// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::accounts::broker::{
    self, Broker, Grant, Token, google,
    host::{Host, Limits},
    transport::{self, Call, Status},
};
use std::{
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    os::fd::AsFd,
    sync::mpsc,
    thread,
};

const INPUT: &[u8] =
    br#"{"model":"gemini-3.8-flash","prompt":"private prompt","max_output_tokens":64}"#;

fn fixture() -> (Fixture, Broker, Token) {
    let f = Fixture::new(1);
    let policy = Policy::new(vec![Resource {
        allocation: Proposal::prepare(
            f.lab.bootstrap(0).trust(),
            Terms {
                resource: [41; 32],
                window: [42; 32],
                unit: "requests".into(),
                limit: 2,
                shares: [(f.lab.bootstrap(0).request().unwrap().binding().actor(), 2)].into(),
            },
        )
        .unwrap(),
        provider: "google".into(),
        adapter: google::ADAPTER.into(),
    }])
    .unwrap();
    let key = aterm_secrets::api_key::encode("google", b"synthetic-provider-key").unwrap();
    f.publish(|doc| {
        doc.set_entry(&Audience::Everyone, "key", key.expose())
            .unwrap();
        doc.set_entry(&Audience::Everyone, ENTRY, policy.text().as_bytes())
            .unwrap();
    });
    f.issue_provider(
        31,
        0,
        Some(policy.reference([41; 32]).unwrap()),
        vec![],
        "google",
    );
    let prepared = f.source(31, 0).prepare_until(deadline()).unwrap();
    prepared.resources().initialize().unwrap();
    let grant = Grant {
        id: [61; 16],
        producer: [62; 32],
        actor: f.lab.bootstrap(0).request().unwrap().binding().actor(),
        binding: prepared.binding().clone(),
        lifetime: Duration::from_secs(300),
    };
    f.publish(|doc| {
        doc.set_entry(
            &Audience::Everyone,
            broker::ENTRY,
            broker::Policy::new(vec![grant.clone()])
                .unwrap()
                .text()
                .as_bytes(),
        )
        .unwrap()
    });
    let b = Broker::at(
        f.lab.paths[0].clone(),
        client::config(&f.lab.paths[0]).unwrap().unwrap(),
    )
    .unwrap();
    let token = b
        .issue_until(grant.id, Duration::from_secs(120), deadline())
        .unwrap();
    (f, b, token)
}

fn poll_to_finish(host: &mut Host, call: &thread::JoinHandle<io::Result<transport::Reply>>) {
    let until = deadline();
    while !call.is_finished() {
        assert!(Instant::now() < until, "Google fixture hang detector");
        host.poll().unwrap();
        aterm_uds::deadline::poll_yield();
    }
}

fn call(
    dir: &std::path::Path,
    token: &Token,
    payload: &[u8],
) -> thread::JoinHandle<io::Result<transport::Reply>> {
    let dir = dir.to_owned();
    let token = aterm_uds::secret::Secret::new(token.bytes());
    let payload = payload.to_vec();
    thread::spawn(move || {
        transport::call_until(
            &dir,
            Call {
                token: token.expose(),
                logical: [93; 16],
                adapter: google::ADAPTER,
                payload: &payload,
            },
            deadline(),
        )
    })
}

fn read_request(stream: &mut TcpStream) -> Vec<u8> {
    let until = deadline();
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        assert!(header.len() < 8192);
        aterm_uds::ready::until(stream.as_fd(), aterm_uds::ready::Interest::Read, until).unwrap();
        let mut byte = [0];
        match stream.read(&mut byte) {
            Ok(1) => header.push(byte[0]),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            result => panic!("request head: {result:?}"),
        }
    }
    let head = String::from_utf8(header.clone()).unwrap();
    assert!(head.starts_with("POST /v1beta/models/gemini-3.8-flash:generateContent HTTP/1.1\r\n"));
    assert!(
        head.to_ascii_lowercase()
            .contains("x-goog-api-key: synthetic-provider-key\r\n")
    );
    assert!(!head.contains("CONNECT "));
    let len = head
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length: ")
                .and_then(|n| n.parse::<usize>().ok())
        })
        .unwrap();
    assert!(len < 8192);
    let mut body = vec![0; len];
    let mut read = 0;
    while read < len {
        aterm_uds::ready::until(stream.as_fd(), aterm_uds::ready::Interest::Read, until).unwrap();
        match stream.read(&mut body[read..]) {
            Ok(0) => panic!("short request"),
            Ok(n) => read += n,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => panic!("request body: {e}"),
        }
    }
    let value: aterm_json::Value = aterm_json::from_slice(&body).unwrap();
    assert_eq!(
        value,
        json!({"contents":[{"role":"user","parts":[{"text":"private prompt"}]}],"generationConfig":{"candidateCount":1,"maxOutputTokens":64,"responseModalities":["TEXT"]}})
    );
    body
}

#[test]
fn google_provider_success_redirect_and_lost_body_keep_one_charge_and_one_request() {
    for (wire, expected, http) in [
        (b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}".as_slice(), Status::Completed, Some(200)),
        (b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/escape\r\nContent-Length: 2\r\n\r\n{}", Status::Rejected, Some(302)),
        (b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n{", Status::Unknown, None),
    ] {
        let (f,b,token)=fixture();
        let dir=transport::tests::socket_dir();
        aterm_uds::private_file::create_private_dir(dir.path()).unwrap();
        let listener=aterm_uds::kept_out(|| TcpListener::bind("127.0.0.1:0")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin=format!("http://{}",listener.local_addr().unwrap());
        let mut host=google::test_bind_at(dir.path(),b,Limits::default(),origin).unwrap();
        let (done, released)=mpsc::channel();
        thread::scope(|scope| {
            let server=scope.spawn(move || {
                aterm_uds::ready::until(listener.as_fd(),aterm_uds::ready::Interest::Read,deadline()).unwrap();
                let (mut stream,_)=aterm_uds::kept_out(|| listener.accept()).unwrap();
                stream.set_nonblocking(true).unwrap();
                read_request(&mut stream);
                stream.set_nonblocking(false).unwrap();
                stream.set_write_timeout(Some(Duration::from_secs(60))).unwrap();
                stream.write_all(wire).unwrap();
                drop(stream);
                let _=released.recv_timeout(Duration::from_secs(60));
                assert!(listener.accept().is_err_and(|e|e.kind()==io::ErrorKind::WouldBlock),"unexpected provider retry");
            });
            let first=call(dir.path(),&token,INPUT);
            poll_to_finish(&mut host,&first);
            let response=first.join().unwrap().unwrap();
            assert_eq!(response.status,expected);
            assert!(response.entered);
            if let Some(http)=http { assert_eq!(google::response(response.body.expose()).unwrap(),(http,b"{}".as_slice())); }
            else { assert!(response.body.is_empty()); }
            let retry=call(dir.path(),&token,INPUT);
            poll_to_finish(&mut host,&retry);
            let retry=retry.join().unwrap().unwrap();
            assert_eq!(retry.status,expected);
            assert!(!retry.entered);
            assert!(retry.body.is_empty());
            host.drain_until(deadline()).unwrap();
            done.send(()).unwrap();
            server.join().unwrap();
        });
        assert_eq!(f.source(31,0).prepare_until(deadline()).unwrap().resources().snapshot().unwrap().used.values().sum::<u64>(),1);
    }
}

#[test]
fn google_invalid_request_does_not_contact_provider_or_consume_its_id() {
    let (f, b, token) = fixture();
    let dir = transport::tests::socket_dir();
    aterm_uds::private_file::create_private_dir(dir.path()).unwrap();
    let listener = aterm_uds::kept_out(|| TcpListener::bind("127.0.0.1:0")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut host = google::test_bind_at(
        dir.path(),
        b,
        Limits::default(),
        format!("http://{}", listener.local_addr().unwrap()),
    )
    .unwrap();
    let invalid = call(
        dir.path(),
        &token,
        br#"{"model":"gemini-x","prompt":"p","max_output_tokens":1,"tools":[]}"#,
    );
    poll_to_finish(&mut host, &invalid);
    let reply = invalid.join().unwrap().unwrap();
    assert_eq!(reply.status, Status::Refused);
    assert!(!reply.entered);
    assert!(
        listener
            .accept()
            .is_err_and(|e| e.kind() == io::ErrorKind::WouldBlock)
    );
    assert_eq!(
        f.source(31, 0)
            .prepare_until(deadline())
            .unwrap()
            .resources()
            .snapshot()
            .unwrap()
            .used
            .values()
            .sum::<u64>(),
        0
    );
    host.drain_until(deadline()).unwrap();
}

#[test]
fn google_one_call_owner_survives_restart_without_replaying_a_logical_request() {
    let (f, broker, _) = fixture();
    let listener = aterm_uds::kept_out(|| TcpListener::bind("127.0.0.1:0")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let (done, released) = mpsc::channel();
    thread::scope(|scope| {
        let server = scope.spawn(move || {
            for _ in 0..2 {
                aterm_uds::ready::until(
                    listener.as_fd(),
                    aterm_uds::ready::Interest::Read,
                    deadline(),
                )
                .unwrap();
                let (mut stream, _) = aterm_uds::kept_out(|| listener.accept()).unwrap();
                stream.set_nonblocking(true).unwrap();
                read_request(&mut stream);
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(60)))
                    .unwrap();
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
                    .unwrap();
            }
            released.recv_timeout(Duration::from_secs(60)).unwrap();
            assert!(
                listener
                    .accept()
                    .is_err_and(|e| e.kind() == io::ErrorKind::WouldBlock)
            );
        });
        let fresh = || {
            Broker::at(
                f.lab.paths[0].clone(),
                client::config(&f.lab.paths[0]).unwrap().unwrap(),
            )
            .unwrap()
        };
        let call = |broker, id, by_account| {
            google::test_request_until(
                broker,
                if by_account {
                    broker::Selection::Account {
                        account: [31; 32],
                        adapter: google::ADAPTER.into(),
                    }
                } else {
                    broker::Selection::Grant([61; 16])
                },
                [id; 16],
                aterm_uds::secret::Secret::new(INPUT),
                deadline(),
                origin.clone(),
            )
            .unwrap()
        };
        let first = call(broker, 94, true);
        assert_eq!(first.status, Status::Completed);
        assert!(first.entered);
        assert_eq!(
            google::response(first.body.expose()).unwrap(),
            (200, b"{}".as_slice())
        );
        let retry = call(fresh(), 94, false);
        assert_eq!(retry.status, Status::Completed);
        assert!(!retry.entered);
        assert!(retry.body.is_empty());
        let second = call(fresh(), 95, true);
        assert_eq!(second.status, Status::Completed);
        assert!(second.entered);
        done.send(()).unwrap();
        server.join().unwrap();
    });
    assert_eq!(
        f.source(31, 0)
            .prepare_until(deadline())
            .unwrap()
            .resources()
            .snapshot()
            .unwrap()
            .used
            .values()
            .sum::<u64>(),
        2
    );
}
