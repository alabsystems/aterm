// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

pub(super) struct Fixture {
    pub(super) _root: aterm_tempfile::TempDir,
    pub(super) paths: Paths,
    pub(super) config: Config,
    pub(super) request: Request,
    pub(super) reply: transport::NoContentReply,
}

pub(super) fn stub(response: Vec<u8>) -> (String, std::thread::JoinHandle<std::io::Result<()>>) {
    stub_noting_empty(response, || {})
}

fn stub_noting_empty(
    response: Vec<u8>,
    mut empty: impl FnMut() + Send + 'static,
) -> (String, std::thread::JoinHandle<std::io::Result<()>>) {
    use std::os::fd::AsFd as _;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/v1/team", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let until = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        loop {
            aterm_uds::ready::until(listener.as_fd(), aterm_uds::ready::Interest::Read, until)?;
            let (mut stream, _) = match listener.accept() {
                Ok(accepted) => accepted,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                    ) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
            if read_request_head(&mut stream, until)?.is_none() {
                // A TCP connect followed by an orderly close supplied no HTTP request.
                // It must not consume this fixture's one response.
                empty();
                continue;
            }
            write_response(&mut stream, &response, until)?;
            return Ok(());
        }
    });
    (url, server)
}

fn write_response(
    stream: &mut std::net::TcpStream,
    mut response: &[u8],
    until: std::time::Instant,
) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::fd::AsFd as _;
    while !response.is_empty() {
        aterm_uds::ready::until(stream.as_fd(), aterm_uds::ready::Interest::Write, until)?;
        match stream.write(response) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(written) => response = &response[written..],
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// As the real server's request reader, distinguish no request from an incomplete request.
/// The same absolute hang detector covers accept, empty connections, and the complete head.
fn read_request_head(
    stream: &mut std::net::TcpStream,
    until: std::time::Instant,
) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::Read as _;
    use std::os::fd::AsFd as _;
    stream.set_nonblocking(true)?;
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        aterm_uds::ready::until(stream.as_fd(), aterm_uds::ready::Interest::Read, until).map_err(
            |error| {
                std::io::Error::new(
                    error.kind(),
                    format!(
                        "HTTP request failed after {} header bytes: {error}",
                        head.len()
                    ),
                )
            },
        )?;
        let mut byte = [0];
        match stream.read(&mut byte) {
            Ok(0) if head.is_empty() => return Ok(None),
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!("HTTP request ended after {} header bytes", head.len()),
                ));
            }
            Ok(_) => head.push(byte[0]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(error) => {
                return Err(std::io::Error::new(
                    error.kind(),
                    format!(
                        "HTTP request failed after {} header bytes: {error}",
                        head.len()
                    ),
                ));
            }
        }
        if head.len() >= 16 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "HTTP request reached {} header bytes, exceeding the fixture's bound",
                    head.len()
                ),
            ));
        }
    }
    Ok(Some(head))
}

#[test]
fn an_empty_closed_connection_does_not_consume_the_http204_response() {
    let (seen, empty) = std::sync::mpsc::channel();
    let (url, server) = stub_noting_empty(
        b"HTTP/1.1 204 No Content\r\nETag: \"9\"\r\n\r\n".to_vec(),
        move || seen.send(()).unwrap(),
    );
    let authority = url
        .strip_prefix("http://")
        .unwrap()
        .split('/')
        .next()
        .unwrap();
    drop(std::net::TcpStream::connect(authority).unwrap());
    empty.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
    let observed =
        transport::fetch_within_as_observed(&url, Some(3), aterm_spec::wait::MUST_HAPPEN, None)
            .unwrap();
    server.join().unwrap().unwrap();
    assert!(matches!(observed.fetched, Fetched::NoNewer(Some(9))));
    assert!(
        observed.http204.is_some(),
        "an actual HTTP 204 was received"
    );
}

#[test]
fn the_http_stub_rejects_an_incomplete_header_before_another_request() {
    use std::io::Write as _;
    let (url, server) = stub(b"HTTP/1.1 204 No Content\r\n\r\n".to_vec());
    let authority = url
        .strip_prefix("http://")
        .unwrap()
        .split('/')
        .next()
        .unwrap();
    let queued = (|| -> std::io::Result<std::net::TcpStream> {
        let mut client = std::net::TcpStream::connect(authority)?;
        client.write_all(b"GET /v1/team HTTP/1.1\r\nHost: test\r\n")?;
        // Queue a complete second request before ending the first. A stub that swallows
        // the partial-header error will answer this request and finish successfully.
        let mut next = std::net::TcpStream::connect(authority)?;
        next.write_all(b"GET /v1/team HTTP/1.1\r\nHost: test\r\n\r\n")?;
        client.shutdown(std::net::Shutdown::Write)?;
        Ok(next)
    })();
    // Keep the second connection open while joining, even under the swallowing control.
    // The stub's one deadline also bounds this join if sending either request failed.
    let ended = server.join().unwrap();
    let _next = queued.unwrap();
    assert_eq!(ended.unwrap_err().kind(), std::io::ErrorKind::UnexpectedEof);
}

pub(super) fn fixture(etag: Option<&str>, with_recipient: bool) -> Fixture {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("member"));
    let subscriber =
        with_recipient.then(|| machine_identity(&paths).unwrap().recipient().to_bech32());
    let mut response = String::from("HTTP/1.1 204 No Content\r\n");
    if let Some(etag) = etag {
        response.push_str(&format!("ETag: {etag}\r\n"));
    }
    response.push_str("\r\n");
    let (url, server) = stub(response.into_bytes());
    let config = Config {
        source: Source::Url(url.clone()),
        name: "team".into(),
        signer: None,
        followed: Vec::new(),
        host: "member".into(),
    };
    save_config(&paths, &config).unwrap();
    {
        let _guard = StateLock::take(&paths).unwrap();
        save_state(&paths, &State::default()).unwrap();
    }
    let request = Request::start(&paths, &config, Some(3), subscriber.as_deref()).unwrap();
    let observed = transport::fetch_within_as_observed(
        &url,
        Some(3),
        Duration::from_secs(60),
        subscriber.as_deref(),
    )
    .unwrap();
    server.join().unwrap().unwrap();
    assert!(matches!(observed.fetched, Fetched::NoNewer(_)));
    Fixture {
        _root: root,
        paths,
        config,
        request,
        reply: observed.http204.expect("actual wire204"),
    }
}

#[test]
fn actual_204_with_any_etag_is_retained_without_admission_and_reads_preserve_received_time() {
    for etag in [Some("\"7\""), None, Some("not-a-sequence")] {
        let f = fixture(etag, false);
        let before = std::fs::read(f.paths.state_file()).unwrap();
        assert_eq!(observe_http204_response(&f.paths, &f.config).unwrap(), None);
        retain(&f.paths, &f.config, &f.request, Some(&f.reply));
        let observed = observe_http204_response(&f.paths, &f.config)
            .unwrap()
            .unwrap();
        assert_eq!(observed.source(), VaultNetworkSource::ConfiguredUrl);
        assert_eq!(observed.responded_at_ms(), f.reply.responded_at_ms());
        let mut state = load_state(&f.paths).unwrap();
        assert_eq!(state.membership, Stored::None);
        assert_eq!(state.heard, 0);
        assert_eq!(state.seq, 0);
        assert!(state.network_materialization.is_none());
        assert!(state.manifest.files.is_empty());
        assert!(matches!(
            observe_materialization(&f.paths, &f.config, || Ok(())).unwrap(),
            MaterializationObservation::Blocked(_)
        ));
        let after = std::fs::read(f.paths.state_file()).unwrap();
        assert_ne!(before, after);
        assert_eq!(
            observe_http204_response(&f.paths, &f.config).unwrap(),
            Some(observed)
        );
        assert_eq!(
            std::fs::read(f.paths.state_file()).unwrap(),
            after,
            "read does not write or restamp"
        );
        state.membership = Stored::Revoked;
        state.failed = true;
        state.pending.insert("synthetic".into(), "00".into());
        save_state(&f.paths, &state).unwrap();
        assert_eq!(
            observe_http204_response(&f.paths, &f.config).unwrap(),
            Some(observed),
            "route evidence is independent of materialization"
        );
    }
}

#[test]
fn coalescing_keeps_original_response_and_only_context_change_bypasses_the_minute() {
    let f = fixture(None, true);
    let recipient = f.request.recipient;
    let mut state = load_state(&f.paths).unwrap();
    let first = f.reply.responded_at_ms();
    assert!(record(
        &mut state, &f.config, recipient, &f.request, &f.reply, first
    ));
    assert!(!record(
        &mut state,
        &f.config,
        recipient,
        &f.request,
        &f.reply,
        first + 59_999
    ));
    assert_eq!(
        state.http204_response.as_ref().unwrap().persisted_at_ms,
        first
    );
    assert!(record(
        &mut state,
        &f.config,
        recipient,
        &f.request,
        &f.reply,
        first + 60_000
    ));
    assert_eq!(
        project(&f.config, recipient, &state)
            .unwrap()
            .responded_at_ms(),
        first
    );
    save_state(&f.paths, &state).unwrap();
    state = load_state(&f.paths).unwrap();
    assert!(
        !record(
            &mut state,
            &f.config,
            recipient,
            &f.request,
            &f.reply,
            first + 60_001
        ),
        "reloading the minute-boundary rewrite must still suppress an immediate repeat"
    );
    assert!(
        !record(
            &mut state,
            &f.config,
            recipient,
            &f.request,
            &f.reply,
            first - 1
        ),
        "wall rollback does not create a new response"
    );
    state.http204_response.as_mut().unwrap().configuration = [219; 32];
    assert!(
        record(
            &mut state,
            &f.config,
            recipient,
            &f.request,
            &f.reply,
            first + 1
        ),
        "an old retained context cannot throttle the current request's response"
    );
}

#[test]
fn retain_never_creates_or_repairs_a_lock_and_a_busy_lock_changes_no_state() {
    use std::os::unix::fs::PermissionsExt as _;
    let f = fixture(None, false);
    let before = std::fs::read(f.paths.state_file()).unwrap();
    let guard = StateLock::take(&f.paths).unwrap();
    retain(&f.paths, &f.config, &f.request, Some(&f.reply));
    drop(guard);
    assert_eq!(std::fs::read(f.paths.state_file()).unwrap(), before);
    std::fs::set_permissions(&f.paths.dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    retain(&f.paths, &f.config, &f.request, Some(&f.reply));
    assert_eq!(
        std::fs::metadata(&f.paths.dir)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o500
    );
    std::fs::set_permissions(&f.paths.dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::set_permissions(f.paths.lock_file(), std::fs::Permissions::from_mode(0o644)).unwrap();
    retain(&f.paths, &f.config, &f.request, Some(&f.reply));
    assert_eq!(
        std::fs::metadata(f.paths.lock_file())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o644
    );
    assert_eq!(std::fs::read(f.paths.state_file()).unwrap(), before);
    std::fs::remove_file(f.paths.lock_file()).unwrap();
    retain(&f.paths, &f.config, &f.request, Some(&f.reply));
    assert!(!f.paths.lock_file().exists());
    assert_eq!(std::fs::read(f.paths.state_file()).unwrap(), before);
}

#[test]
fn full_config_and_optional_recipient_context_gate_the_independent_read() {
    let f = fixture(None, false);
    retain(&f.paths, &f.config, &f.request, Some(&f.reply));
    let original = observe_http204_response(&f.paths, &f.config)
        .unwrap()
        .unwrap();
    let mut changed = f.config.clone();
    changed.host.push_str("-changed");
    save_config(&f.paths, &changed).unwrap();
    assert_eq!(observe_http204_response(&f.paths, &f.config).unwrap(), None);
    assert_eq!(observe_http204_response(&f.paths, &changed).unwrap(), None);
    save_config(&f.paths, &f.config).unwrap();
    machine_identity(&f.paths).unwrap();
    assert_eq!(
        observe_http204_response(&f.paths, &f.config).unwrap(),
        None,
        "None to a new key is a different local context"
    );
    std::fs::remove_file(f.paths.identity_file()).unwrap();
    assert_eq!(
        observe_http204_response(&f.paths, &f.config).unwrap(),
        Some(original)
    );
}

#[test]
fn retain_leaves_read_only_or_absent_state_alone() {
    use std::os::unix::fs::PermissionsExt as _;
    let f = fixture(None, false);
    let before = std::fs::read(f.paths.state_file()).unwrap();
    std::fs::set_permissions(f.paths.state_file(), std::fs::Permissions::from_mode(0o400)).unwrap();
    retain(&f.paths, &f.config, &f.request, Some(&f.reply));
    assert_eq!(
        std::fs::metadata(f.paths.state_file())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o400
    );
    assert_eq!(std::fs::read(f.paths.state_file()).unwrap(), before);
    std::fs::remove_file(f.paths.state_file()).unwrap();
    retain(&f.paths, &f.config, &f.request, Some(&f.reply));
    assert!(!f.paths.state_file().exists());
}

#[test]
fn malformed_optional_receipt_does_not_block_loading_the_ordinary_state() {
    let f = fixture(None, false);
    let mut text = String::from_utf8(std::fs::read(f.paths.state_file()).unwrap()).unwrap();
    text.push_str("\n[http204_response]\nversion = 1\nconfiguration = 'bad'\nrecipient = ''\nresponded_at_ms = 2\npersisted_at_ms = 3\n");
    fsx::write_atomic(&f.paths.state_file(), text.as_bytes(), 0o600).unwrap();
    let state = load_state(&f.paths).unwrap();
    assert!(state.http204_response.is_none());
    assert_eq!(state.membership, Stored::None);
    assert_eq!(state.heard, 0);
    save_state(&f.paths, &state).unwrap();
    assert_eq!(observe_http204_response(&f.paths, &f.config).unwrap(), None);
}
