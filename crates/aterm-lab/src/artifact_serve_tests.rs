// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn args(extra: &[&str]) -> Vec<String> {
    [
        "--bind",
        "127.0.0.1:0",
        "--peer",
        "127.0.0.1",
        "--status",
        "/tmp/status",
        "--file",
        "one=/tmp/one",
    ]
    .into_iter()
    .chain(extra.iter().copied())
    .map(str::to_owned)
    .collect()
}

#[test]
fn configuration_has_explicit_peers_names_and_hard_bounds() {
    assert!(config(&args(&[])).is_ok());
    for bad in [
        vec!["--unknown", "x"],
        vec!["--file", "one=/tmp/two"],
        vec!["--file", "../bad=/tmp/two"],
        vec!["--max-requests", "129"],
        vec!["--lifetime-secs", "3601"],
    ] {
        assert!(config(&args(&bad)).is_err());
    }
    for (flag, value) in [
        ("--bind", "0.0.0.0:0"),
        ("--bind", "198.51.100.1:0"),
        ("--peer", "0.0.0.0"),
        ("--peer", "224.0.0.1"),
    ] {
        let mut argv = args(&[]);
        let index = argv.iter().position(|word| word == flag).unwrap();
        argv[index + 1] = value.into();
        assert!(config(&argv).is_err());
    }
}

#[test]
fn authority_and_routes_are_exact() {
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let input = tmp.path().join("input");
    std::fs::write(&input, b"allowed bytes").unwrap();
    let file = snapshot(
        "one",
        &input,
        &tmp.path().join("snap"),
        Instant::now() + Duration::from_secs(60),
        MAX_FILE,
    )
    .unwrap();
    let files = [file];
    let peer = "127.0.0.1".parse().unwrap();
    let request = |path: &str| format!("GET {path} HTTP/1.1\r\nHost: local\r\n\r\n").into_bytes();
    assert_eq!(
        select(peer, peer, &request("/secret/one"), "secret", &files),
        Ok(0)
    );
    assert_eq!(
        select(
            "127.0.0.2".parse().unwrap(),
            peer,
            &request("/secret/one"),
            "secret",
            &files
        ),
        Err(Refused::Forbidden)
    );
    assert_eq!(
        select(peer, peer, &request("/wrong/one"), "secret", &files),
        Err(Refused::Forbidden)
    );
    for path in [
        "/secret/../one",
        "/secret/%2e%2e",
        "/secret/one?query",
        "/secret/",
        "/secret/missing",
    ] {
        assert_eq!(
            select(peer, peer, &request(path), "secret", &files),
            Err(Refused::Missing)
        );
    }
    assert_eq!(
        read_head(&mut io::Cursor::new(vec![b'a'; MAX_HEADERS + 1])),
        Err(Refused::TooLarge)
    );
}

#[test]
fn transfer_keeps_large_immutable_snapshot_complete_on_a_sanctioned_blocking_accept() {
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let input = tmp.path().join("input");
    let expected: Vec<u8> = (0..8 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    std::fs::write(&input, &expected).unwrap();
    let file = snapshot(
        "one",
        &input,
        &tmp.path().join("snapshot"),
        Instant::now() + Duration::from_secs(60),
        MAX_FILE,
    )
    .unwrap();
    assert_eq!(
        file.sha256,
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&expected))
    );
    std::fs::write(&input, b"changed after snapshot").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let mut client = TcpStream::connect(address).unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        let mut budget = MAX_SENT;
        serve_one(
            stream,
            address.ip(),
            "secret",
            &[file],
            Instant::now() + Duration::from_secs(60),
            &mut budget,
        )
        .unwrap();
    });
    client
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    client
        .write_all(b"GET /secret/one HTTP/1.1\r\nHost: local\r\n\r\n")
        .unwrap();
    let mut bytes = Vec::new();
    client.read_to_end(&mut bytes).unwrap();
    server.join().unwrap();
    let end = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    assert!(String::from_utf8_lossy(&bytes[..end]).contains("Content-Length: 8388608\r\n"));
    assert_eq!(&bytes[end..], expected);
}

#[test]
fn snapshots_refuse_links_and_directories_and_status_never_overwrites() {
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let input = tmp.path().join("input");
    std::fs::write(&input, b"one").unwrap();
    let linked = tmp.path().join("link");
    std::os::unix::fs::symlink(&input, &linked).unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    assert!(snapshot("one", &linked, &tmp.path().join("no"), deadline, MAX_FILE).is_err());
    assert!(
        snapshot(
            "one",
            tmp.path(),
            &tmp.path().join("no"),
            deadline,
            MAX_FILE
        )
        .is_err()
    );
    assert!(snapshot("one", &input, &tmp.path().join("no"), deadline, 2).is_err());
    let status_path = tmp.path().join("status");
    status(
        &status_path,
        "127.0.0.1:1".parse().unwrap(),
        "secret",
        &[],
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(
        std::fs::metadata(&status_path).unwrap().mode() & 0o777,
        0o600
    );
    let before = std::fs::read(&status_path).unwrap();
    assert!(before.ends_with(b"\n"));
    assert!(aterm_json::from_slice::<aterm_json::Value>(&before).is_ok());
    assert!(
        status(
            &status_path,
            "127.0.0.1:2".parse().unwrap(),
            "different",
            &[],
            Duration::from_secs(1)
        )
        .is_err()
    );
    assert_eq!(std::fs::read(status_path).unwrap(), before);
}
