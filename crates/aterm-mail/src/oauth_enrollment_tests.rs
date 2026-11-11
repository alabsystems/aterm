// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
fn response(target: &str, host: &str) -> String {
    format!("GET {target} HTTP/1.1\r\nHost: {host}\r\n\r\n")
}
#[test]
fn desktop_client_only_and_values_are_bounded() {
    assert!(Client::decode(br#"{"installed":{"client_id":"ours","client_secret":"secret","token_uri":"https://untrusted.test"}}"#).is_ok());
    assert!(
        Client::decode(br#"{"installed":{"client_id":"ours","client_secret":null}}"#)
            .unwrap()
            .secret
            .is_none()
    );
    assert!(Client::decode(br#"{"web":{"client_id":"wrong-kind"}}"#).is_err());
    assert!(Client::decode(br#"{"installed":{"client_id":""}}"#).is_err());
    assert!(Client::decode(&vec![b' '; 65537]).is_err());
}
#[test]
fn callback_requires_exact_host_path_state_and_unique_fields() {
    let valid = response("/oauth/callback?state=nonce&code=a%2Bb", "127.0.0.1:1234");
    assert_eq!(
        callback(&valid, 1234, "nonce").unwrap().unwrap().expose(),
        "a+b"
    );
    for changed in [
        valid.replace("nonce", "other"),
        valid.replace("1234", "1235"),
        valid.replace("/oauth/callback?", "/other?"),
        valid.replace("&code=", "&state=nonce&code="),
        valid.replace("a%2Bb", "%0d%0a"),
        valid.replace("a%2Bb", "%zz"),
        valid.replace("Host:", "Host: 127.0.0.1:1234\r\nHost:"),
    ] {
        assert!(callback(&changed, 1234, "nonce").is_err());
    }
    assert!(
        callback(
            &response(
                "/oauth/callback?state=nonce&error=access_denied",
                "127.0.0.1:1234"
            ),
            1234,
            "nonce"
        )
        .unwrap()
        .is_none()
    );
}
#[test]
fn a_callback_whose_bytes_come_after_the_accept_is_read_whole() {
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let end = Instant::now() + Duration::from_secs(60);
    let (mut server, _) =
        aterm_uds::accept(&listener, AcceptWait::Until(end), AcceptedMode::Blocking).unwrap();
    bound_io(&server, Duration::from_secs(60)).unwrap();
    let (reading, read) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let reader = scope.spawn(move || {
            reading.send(()).unwrap();
            request(&mut server, end)
        });
        read.recv().unwrap();
        client
            .write_all(response("/oauth/callback?state=s&code=c", "127.0.0.1:1").as_bytes())
            .unwrap();
        let got = reader.join().unwrap().expect("the request read whole");
        assert!(got.starts_with("GET /oauth/callback?state=s&code=c "));
    });
}
#[test]
fn receiver_ignores_unrelated_requests_and_acknowledges_duplicate_delivery() {
    let enrollment = Enrollment::bind(
        Client {
            id: "ours".into(),
            secret: None,
        },
        "me@gmail.com",
    )
    .unwrap();
    let address = enrollment.listener.local_addr().unwrap();
    let end = Instant::now() + Duration::from_secs(60);
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let receiver = scope.spawn(|| enrollment.receive(end, &stop));
        for target in [
            "/favicon.ico".to_owned(),
            format!(
                "/oauth/callback?state={}&code=the-code",
                enrollment.pending.state
            ),
            format!(
                "/oauth/callback?state={}&code=the-code",
                enrollment.pending.state
            ),
        ] {
            let mut stream = TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(60)))
                .unwrap();
            stream
                .write_all(response(&target, &address.to_string()).as_bytes())
                .unwrap();
            let mut output = String::new();
            stream.read_to_string(&mut output).unwrap();
            assert!(output.starts_with("HTTP/1.1 "));
        }
        stop.store(true, Ordering::Release);
        assert_eq!(receiver.join().unwrap().unwrap().expose(), "the-code");
    });
    assert!(enrollment.receive(Instant::now(), &stop).is_err());
    stop.store(true, Ordering::Release);
    assert!(enrollment.receive(end, &stop).is_err());
}

#[test]
fn failed_authorizer_cancels_the_callback_without_exchanging_tokens() {
    let enrollment = Enrollment::bind(
        Client {
            id: "ours".into(),
            secret: None,
        },
        "me@gmail.com",
    )
    .unwrap();
    let address = enrollment.listener.local_addr().unwrap();
    let result = enrollment.run(Instant::now() + Duration::from_secs(60), |_, _| {
        Err(error(ErrorKind::Rejected))
    });
    assert!(matches!(
        result,
        Err(Error {
            kind: ErrorKind::Rejected
        })
    ));
    assert!(TcpStream::connect(address).is_err());
}

#[test]
fn expired_enrollment_does_not_start_authorization() {
    let enrollment = Enrollment::bind(
        Client {
            id: "ours".into(),
            secret: None,
        },
        "me@gmail.com",
    )
    .unwrap();
    assert!(
        enrollment
            .run(Instant::now(), |_, _| panic!(
                "expired flow must not navigate"
            ))
            .is_err()
    );
}

/// NEGATIVE CONTROL: checking `stop` before draining the accept queue drops an
/// already-sent valid callback.
#[test]
fn receiver_drains_a_queued_callback_when_stopping() {
    let enrollment = Enrollment::bind(
        Client {
            id: "ours".into(),
            secret: None,
        },
        "me@example.test",
    )
    .unwrap();
    let address = enrollment.listener.local_addr().unwrap();
    let mut stream = TcpStream::connect(address).unwrap();
    let target = format!(
        "/oauth/callback?state={}&code=queued-code",
        enrollment.pending.state
    );
    let request_bytes = response(&target, &address.to_string());
    let end = Instant::now() + Duration::from_secs(60);
    stream
        .set_write_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    stream.write_all(request_bytes.as_bytes()).unwrap();
    #[cfg(unix)]
    {
        use std::os::fd::AsFd as _;
        aterm_uds::ready::until(
            enrollment.listener.as_fd(),
            aterm_uds::ready::Interest::Read,
            end,
        )
        .expect("callback queued before stopping");
    }
    let stop = AtomicBool::new(true);
    let code = enrollment
        .receive(end, &stop)
        .expect("queued callback must be drained before stopping");
    assert_eq!(code.expose(), "queued-code");

    let mut response = std::io::BufReader::new(stream.take(16384));
    let mut reply = Vec::new();
    while !reply.ends_with(b"\r\n\r\n") {
        assert_ne!(
            std::io::BufRead::read_until(&mut response, b'\n', &mut reply).unwrap(),
            0
        );
    }
    assert!(reply.starts_with(b"HTTP/1.1 200 OK\r\n"));
    assert!(
        reply
            .windows(b"\r\nContent-Length: 0\r\n".len())
            .any(|part| part == b"\r\nContent-Length: 0\r\n")
    );
}
