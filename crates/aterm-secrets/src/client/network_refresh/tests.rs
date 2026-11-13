// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

struct FailingServer {
    address: std::net::SocketAddr,
    stop: std::sync::Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<Vec<String>>>,
}

impl FailingServer {
    fn start() -> Self {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = std::thread::spawn(move || {
            let mut requests = Vec::new();
            loop {
                let (mut stream, _) = aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Forever,
                    aterm_uds::AcceptedMode::Blocking,
                )
                .unwrap();
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
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
                requests.push(String::from_utf8(head).unwrap());
                stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            }
            requests
        });
        Self {
            address,
            stop,
            worker: Some(worker),
        }
    }

    fn finish(&mut self) -> Vec<String> {
        self.stop.store(true, Ordering::SeqCst);
        std::net::TcpStream::connect_timeout(&self.address, Duration::from_secs(60)).unwrap();
        self.worker.take().unwrap().join().unwrap()
    }
}

impl Drop for FailingServer {
    fn drop(&mut self) {
        if self.worker.is_some() {
            self.finish();
        }
    }
}

fn fixture() -> (aterm_tempfile::TempDir, Paths, Config) {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("member"));
    machine_identity(&paths).unwrap();
    let config = Config {
        source: Source::Url("http://127.0.0.1:1/v1/team".into()),
        mirrors: Vec::new(),
        name: "team".into(),
        signer: None,
        followed: Vec::new(),
        host: "member".into(),
    };
    save_config(&paths, &config).unwrap();
    let _guard = StateLock::take(&paths).unwrap();
    save_state(&paths, &State::default()).unwrap();
    (root, paths, config)
}

fn failure(paths: &Paths, config: &Config) -> Attempt {
    let attempt = Attempt::start(paths, config).unwrap();
    retain(
        paths,
        config,
        &attempt,
        VaultNetworkRefreshOutcome::Failed,
        1_000,
    );
    assert_eq!(
        observe_network_refresh(paths, config)
            .unwrap()
            .unwrap()
            .completed_at_ms(),
        1_000
    );
    attempt
}

#[test]
fn cli_and_subscriber_share_the_actual_failed_refresh_boundary() {
    for subscriber in [false, true] {
        let (_root, paths, mut config) = fixture();
        let mut server = FailingServer::start();
        config.source = Source::Url(format!("http://{}/v1/team", server.address));
        save_config(&paths, &config).unwrap();
        let result = if subscriber {
            subscriber_pass(&paths, &mut None, &FOUND_NONE).map(|(_, outcome)| outcome)
        } else {
            sync(&paths, None)
        };
        let requests = server.finish();
        assert!(result.is_err());
        assert!(
            requests
                .iter()
                .any(|head| head.starts_with("GET /v1/team ") || head.starts_with("GET /v1/team?")),
            "an actual configured envelope request was answered503"
        );
        let observed = observe_network_refresh(&paths, &config).unwrap().unwrap();
        assert_eq!(observed.outcome(), VaultNetworkRefreshOutcome::Failed);
        assert!(observed.completed_at_ms() > 0);
        let state = load_state(&paths).unwrap();
        assert_eq!(state.membership, Stored::None);
        assert!(state.network_materialization.is_none());
        assert!(state.http204_response.is_none());
    }
}

#[test]
fn metadata_reload_preserves_time_and_changed_context_neither_reads_nor_overwrites_it() {
    let (_root, paths, config) = fixture();
    let attempt = failure(&paths, &config);
    let before = std::fs::read(paths.state_file()).unwrap();
    let observed = observe_network_refresh(&paths, &config).unwrap();
    for _ in 0..2 {
        assert_eq!(observe_network_refresh(&paths, &config).unwrap(), observed);
        assert_eq!(std::fs::read(paths.state_file()).unwrap(), before);
    }
    for changed in [
        Config {
            host: "other".into(),
            ..config.clone()
        },
        Config {
            source: Source::Url("http://127.0.0.1:2/v1/team".into()),
            ..config.clone()
        },
        Config {
            signer: Some([7; 32]),
            ..config.clone()
        },
        Config {
            followed: vec![[8; 32]],
            ..config.clone()
        },
    ] {
        save_config(&paths, &changed).unwrap();
        assert_eq!(observe_network_refresh(&paths, &changed).unwrap(), None);
        retain(
            &paths,
            &config,
            &attempt,
            VaultNetworkRefreshOutcome::Succeeded,
            2_000,
        );
        assert_eq!(std::fs::read(paths.state_file()).unwrap(), before);
    }
    save_config(&paths, &config).unwrap();
    let identity = std::fs::read(paths.identity_file()).unwrap();
    fsx::write_atomic(
        &paths.identity_file(),
        Identity::from_secret_bytes([117; 32])
            .to_bech32()
            .as_slice(),
        0o600,
    )
    .unwrap();
    assert_eq!(observe_network_refresh(&paths, &config).unwrap(), None);
    retain(
        &paths,
        &config,
        &attempt,
        VaultNetworkRefreshOutcome::Succeeded,
        2_000,
    );
    assert_eq!(std::fs::read(paths.state_file()).unwrap(), before);
    fsx::write_atomic(&paths.identity_file(), &identity, 0o600).unwrap();
    assert_eq!(observe_network_refresh(&paths, &config).unwrap(), observed);
}

#[test]
fn busy_history_is_unavailable_then_the_same_retained_failure_is_readable_again() {
    let (root, paths, config) = fixture();
    failure(&paths, &config);
    let original = observe_network_refresh(&paths, &config).unwrap().unwrap();
    let bytes = std::fs::read(paths.state_file()).unwrap();
    let absent = Paths::for_home(&root.path().join("absent"));
    assert!(matches!(
        StateLock::observe_classified(&absent).unwrap(),
        StateObservation::Absent
    ));
    assert_eq!(observe_network_refresh(&absent, &config).unwrap(), None);
    assert!(!absent.dir.exists(), "a missing observation is not created");
    {
        let writer = StateLock::take(&paths).unwrap();
        assert!(matches!(
            StateLock::observe_classified(&paths).unwrap(),
            StateObservation::Busy
        ));
        assert!(
            StateLock::observe(&paths).unwrap().is_none(),
            "legacy readers retain their contract"
        );
        assert!(observe_network_refresh(&paths, &config).is_err());
        assert_eq!(std::fs::read(paths.state_file()).unwrap(), bytes);
        drop(writer);
    }
    assert!(matches!(
        StateLock::observe_classified(&paths).unwrap(),
        StateObservation::Held(_)
    ));
    assert_eq!(
        observe_network_refresh(&paths, &config).unwrap(),
        Some(original)
    );
    assert_eq!(std::fs::read(paths.state_file()).unwrap(), bytes);
}

#[test]
fn busy_or_unsafe_optional_state_cannot_change_sync_result_or_follow_a_link() {
    use std::os::unix::fs::symlink;
    let (root, paths, config) = fixture();
    let attempt = failure(&paths, &config);
    let before = std::fs::read(paths.state_file()).unwrap();
    let lock = StateLock::take(&paths).unwrap();
    let result = run(&paths, &config, |refresh| {
        refresh.observed(&Err(Error::new("inert-private-error")));
        Err(Error::new("ordinary-result"))
    });
    assert_eq!(result.unwrap_err().to_string(), "ordinary-result");
    assert_eq!(std::fs::read(paths.state_file()).unwrap(), before);
    drop(lock);
    let kept = root.path().join("kept-state");
    std::fs::rename(paths.state_file(), &kept).unwrap();
    symlink(&kept, paths.state_file()).unwrap();
    retain(
        &paths,
        &config,
        &attempt,
        VaultNetworkRefreshOutcome::Succeeded,
        2_000,
    );
    assert_eq!(std::fs::read(&kept).unwrap(), before);
    assert!(observe_network_refresh(&paths, &config).is_err());
    std::fs::remove_file(paths.state_file()).unwrap();
    std::fs::rename(kept, paths.state_file()).unwrap();
    let result = run(&paths, &config, |refresh| {
        refresh.observed(&Err(Error::new("inert-private-error")));
        Err(Error::new("ordinary-result"))
    });
    assert_eq!(result.unwrap_err().to_string(), "ordinary-result");
    assert!(
        !String::from_utf8(std::fs::read(paths.state_file()).unwrap())
            .unwrap()
            .contains("inert-private-error")
    );
}

#[test]
fn failed_transport_followed_by_real_local_cache_apply_remains_a_failed_refresh() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let held_listener = listener.try_clone().unwrap();
    let server = crate::server::start(
        listener,
        &crate::server::Options {
            listen: "127.0.0.1:0".parse().unwrap(),
            store: root.path().join("store"),
            advertise: None,
            pins: Vec::new(),
            hold: Duration::from_millis(20),
            keep_awake: false,
        },
    )
    .unwrap();
    let paths = Paths::for_home(&root.path().join("member"));
    let import = root.path().join("import");
    fsx::ensure_private_dir(&import).unwrap();
    fsx::write_atomic(&import.join("token"), b"inert-token\n", 0o600).unwrap();
    create(
        &paths,
        &format!("http://{}/v1/team", server.addr),
        Some(&import),
        Some("member"),
    )
    .unwrap();
    sync_reader_until(&paths, Instant::now() + Duration::from_secs(60)).unwrap();
    let config = configured(&paths).unwrap();
    let initial = load_state(&paths).unwrap();
    let materialization = || match observe_materialization(&paths, &config, || Ok(())).unwrap() {
        MaterializationObservation::Ready(held) => held.snapshot.network_materialization(),
        MaterializationObservation::Blocked(_) => None,
    };
    let receipt = materialization().unwrap();
    let cached = fsx::read_private(&paths.cache_file(), envelope::MAX_ENVELOPE)
        .unwrap()
        .unwrap();
    // Inject the failed transport result at the same run boundary; the following apply
    // executes ordinary verification/materialization of genuine retained cache bytes.
    run(&paths, &config, |refresh| {
        refresh.observed(&Err(Error::new("inert-transport-failure")));
        apply_fetched_until(
            &paths,
            config.clone(),
            initial,
            Fetched::Body(cached.as_slice().to_vec()),
            None,
            Some(Instant::now() + Duration::from_secs(60)),
        )
    })
    .unwrap();
    let failed = observe_network_refresh(&paths, &config).unwrap().unwrap();
    assert_eq!(failed.outcome(), VaultNetworkRefreshOutcome::Failed);
    assert_eq!(materialization(), Some(receipt));
    // Raw caller/cache bytes do not enter the network-refresh recorder at all.
    sync_with(&paths, cached.as_slice().to_vec()).unwrap();
    assert_eq!(
        observe_network_refresh(&paths, &config).unwrap(),
        Some(failed)
    );
    assert_eq!(materialization(), Some(receipt));
    let state = std::fs::read_to_string(paths.state_file()).unwrap();
    let malformed = state.replace("outcome = \"failed\"", "outcome = 7");
    fsx::write_atomic(&paths.state_file(), malformed.as_bytes(), 0o600).unwrap();
    assert_eq!(observe_network_refresh(&paths, &config).unwrap(), None);
    assert_eq!(
        materialization(),
        Some(receipt),
        "malformed optional history does not invalidate the guarded cache"
    );
    fsx::write_atomic(&paths.state_file(), state.as_bytes(), 0o600).unwrap();
    server.stop();

    // The same configured source now serves an empty store. A real ordinary PutBack
    // repairs its HTTP404 using the admitted cache and clears the retained failure.
    let repaired_server = crate::server::start(
        held_listener,
        &crate::server::Options {
            listen: "127.0.0.1:0".parse().unwrap(),
            store: root.path().join("empty-store"),
            advertise: None,
            pins: Vec::new(),
            hold: Duration::from_millis(20),
            keep_awake: false,
        },
    )
    .unwrap();
    assert!(matches!(
        sync(&paths, None).unwrap(),
        SyncOutcome::PutBack { .. }
    ));
    assert_eq!(
        repaired_server.held_seq("team"),
        Some(load_state(&paths).unwrap().seq)
    );
    assert_eq!(
        observe_network_refresh(&paths, &config)
            .unwrap()
            .unwrap()
            .outcome(),
        VaultNetworkRefreshOutcome::Succeeded
    );
    assert_eq!(materialization(), Some(receipt));
    repaired_server.stop();
}

#[test]
fn absent_and_malformed_optional_history_is_not_a_success_or_failure() {
    let (_root, paths, config) = fixture();
    assert_eq!(observe_network_refresh(&paths, &config).unwrap(), None);
    failure(&paths, &config);
    let before = std::fs::read_to_string(paths.state_file()).unwrap();
    for text in [
        before.replace("outcome = \"failed\"", "outcome = \"unknown\""),
        before.replace("completed_at_ms = 1000", "completed_at_ms = -1"),
        before.replace("version = 1", "version = 2"),
    ] {
        fsx::write_atomic(&paths.state_file(), text.as_bytes(), 0o600).unwrap();
        assert_eq!(observe_network_refresh(&paths, &config).unwrap(), None);
        assert_eq!(std::fs::read_to_string(paths.state_file()).unwrap(), text);
    }
}

#[test]
fn an_unanswered_poll_classified_by_the_real_transport_keeps_the_prior_failure_and_time() {
    let (_root, paths, config) = fixture();
    failure(&paths, &config);
    let before = std::fs::read(paths.state_file()).unwrap();
    let Source::Url(url) = &config.source else {
        panic!("configured URL");
    };
    let unanswered = transport::test_classify_get_response(
        Err(aterm_http::SendError {
            phase: aterm_http::Phase::Sent,
            error: aterm_http::Error::Io(std::io::Error::from(std::io::ErrorKind::TimedOut)),
        }),
        url,
        Some(3),
        None,
    )
    .unwrap()
    .fetched()
    .unwrap();
    assert!(matches!(unanswered.fetched, Fetched::NoNewer(None)));
    assert!(unanswered.http204.is_none());
    run(&paths, &config, |refresh| {
        refresh.observed(&Ok(unanswered));
        Ok(SyncOutcome::Unchanged { seq: 3 })
    })
    .unwrap();
    assert_eq!(std::fs::read(paths.state_file()).unwrap(), before);
}

/// A pass that ends as the retained one did, within a minute of it, keeps the retained receipt
/// and leaves `state.toml` as it is: each subscriber pass (about every 28 s) replaced the file
/// for its completion time alone (2026-11-11). Another outcome is retained at once, the same
/// outcome again once a minute has passed, and a completion before the retained one (the clock
/// set back) at once.
/// NEGATIVE CONTROL (2026-11-11): with `retain` saving whatever `record` took, the repeated
/// failure 59.999 s later replaces the file (`the retained failure stands`).
#[test]
fn a_repeated_outcome_within_a_minute_keeps_the_retained_receipt_and_the_state_file() {
    use VaultNetworkRefreshOutcome::{Failed, Succeeded};
    let (_root, paths, config) = fixture();
    let attempt = failure(&paths, &config);
    let observed = || {
        let refresh = observe_network_refresh(&paths, &config).unwrap().unwrap();
        (refresh.outcome(), refresh.completed_at_ms())
    };
    let file = || fsx::stamp(&paths.state_file()).unwrap();

    let failed = file();
    retain(
        &paths,
        &config,
        &attempt,
        Failed,
        1_000 + PERSIST_AGAIN_MS - 1,
    );
    assert_eq!(file(), failed, "the retained failure stands");
    assert_eq!(observed(), (Failed, 1_000));

    retain(&paths, &config, &attempt, Succeeded, 2_000);
    assert_eq!(observed(), (Succeeded, 2_000), "another outcome at once");
    let succeeded = file();
    assert_ne!(succeeded, failed);
    retain(
        &paths,
        &config,
        &attempt,
        Succeeded,
        2_000 + PERSIST_AGAIN_MS - 1,
    );
    assert_eq!(file(), succeeded);
    assert_eq!(observed(), (Succeeded, 2_000));

    retain(
        &paths,
        &config,
        &attempt,
        Succeeded,
        2_000 + PERSIST_AGAIN_MS,
    );
    assert_eq!(observed(), (Succeeded, 2_000 + PERSIST_AGAIN_MS));
    retain(&paths, &config, &attempt, Succeeded, 1_500);
    assert_eq!(observed(), (Succeeded, 1_500), "a clock set back");
}
