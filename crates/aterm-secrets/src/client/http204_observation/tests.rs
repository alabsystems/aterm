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

pub(super) fn stub(response: Vec<u8>) -> (String, std::thread::JoinHandle<()>) {
    use std::io::{Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/team", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
            assert!(head.len() < 16 * 1024);
        }
        stream.write_all(&response).unwrap();
    });
    (url, server)
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
    server.join().unwrap();
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
