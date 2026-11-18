// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const WAIT: std::time::Duration = std::time::Duration::from_secs(60);

#[cfg(unix)]
fn pair() -> (PeerClient, CtlStream) {
    let (stream, server) = CtlStream::pair().expect("socket pair");
    (
        PeerClient {
            path: String::new(),
            token: "original".to_owned(),
            first: Some(stream),
        },
        server,
    )
}

#[cfg(unix)]
#[test]
fn counted_and_status_replies_keep_their_distinct_framing() {
    let (mut client, server) = pair();
    (&server)
        .write_all(b"OK 1 sys v=1 view=aterm\n{\"parts\":[]}\n")
        .unwrap();
    assert_eq!(
        client
            .lines("@tqn5r6e0148 sys aterm --json fresh=60000", WAIT)
            .unwrap(),
        ["{\"parts\":[]}"]
    );
    let mut request = String::new();
    BufReader::new(&server).read_line(&mut request).unwrap();
    assert_eq!(request, "AUTH original\n");

    let (mut client, server) = pair();
    (&server).write_all(b"OK 24 80\nOK %2Ftmp\n").unwrap();
    assert_eq!(
        client
            .statuses(&["@1 dims".into(), "@1 cwd".into()], WAIT)
            .unwrap(),
        ["OK 24 80", "OK %2Ftmp"]
    );
}

#[cfg(unix)]
#[test]
fn status_batches_refuse_auth_incomplete_and_malformed_replies() {
    for (reply, kind) in [
        (b"ERR auth\n".as_slice(), io::ErrorKind::PermissionDenied),
        (
            b"OK 24 80\nOK unfinished".as_slice(),
            io::ErrorKind::UnexpectedEof,
        ),
        (b"not a status\n".as_slice(), io::ErrorKind::InvalidData),
    ] {
        let (mut client, server) = pair();
        (&server).write_all(reply).unwrap();
        server.shutdown(std::net::Shutdown::Write).unwrap();
        let error = client
            .statuses(&["@1 dims".into(), "@1 cwd".into()], WAIT)
            .unwrap_err();
        assert_eq!(error.kind(), kind, "{error}");
    }
    let (mut client, _server) = pair();
    assert_eq!(
        client.lines("sessions\nquit", WAIT).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        client
            .lines("sessions", std::time::Duration::ZERO)
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    assert!(
        client.first.is_some(),
        "refused input or an expired budget sends nothing"
    );
}

#[cfg(unix)]
#[test]
fn reconnect_keeps_the_first_token_after_the_token_file_changes() {
    use std::os::unix::fs::OpenOptionsExt as _;
    let _serial = crate::tests::PROBE_SOCKET_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let scratch = aterm_tempfile::TempDir::new_in("/tmp").unwrap();
    let path = scratch.path().join("aterm-9999999.sock");
    let token_file = scratch.path().join("aterm-9999999.token");
    let mut token = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&token_file)
        .unwrap();
    token.write_all(b"original").unwrap();
    drop(token);
    let listener = aterm_uds::CtlListener::bind(&path).unwrap();
    let mut client = PeerClient::connect(path.to_str().unwrap(), WAIT).unwrap();
    std::fs::write(&token_file, "replacement").unwrap();
    let worker = std::thread::spawn(move || {
        for response in ["OK 1\nold-generation\n", "ERR auth\n"] {
            let (mut server, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Until(std::time::Instant::now() + WAIT),
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap();
            server.set_read_timeout(Some(WAIT)).unwrap();
            let mut reader = BufReader::new(&server);
            let mut auth = String::new();
            reader.read_line(&mut auth).unwrap();
            assert_eq!(auth, "AUTH original\n");
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            assert_eq!(request, "sessions\n");
            server.write_all(response.as_bytes()).unwrap();
        }
    });
    assert_eq!(client.lines("sessions", WAIT).unwrap(), ["old-generation"]);
    assert!(
        client.lines("sessions", WAIT).is_err(),
        "replacement authentication never replaces the retained token"
    );
    worker.join().unwrap();
}
