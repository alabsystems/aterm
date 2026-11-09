// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::{Read as _, Write as _};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

enum Caller {
    Admin,
    Join,
}

struct NativePrimary {
    paths: Paths,
    expected: u64,
    finished: Arc<AtomicBool>,
    peer: Option<std::thread::JoinHandle<usize>>,
    _root: aterm_tempfile::TempDir,
}

impl NativePrimary {
    fn new(caller: Caller) -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("home"));
        let source = root.path().join("native-budget.vault");
        test_create_unbound(&paths, source.to_str().unwrap(), None, Some("fixture")).unwrap();
        let expected = admin_open(&paths).unwrap().seq();
        let bytes = std::fs::read(&source).unwrap();
        let subscriber = match caller {
            Caller::Admin => Some(subscriber_recipient(&paths).unwrap()),
            Caller::Join => None,
        };
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/v1/native-budget", listener.local_addr().unwrap());
        let mut config = config(&paths).unwrap().unwrap();
        config.source = Source::Url(url.clone());
        save_config(&paths, &config).unwrap();
        assert_eq!(
            ledger_publish::candidate_server_urls(&paths, &config, &[]),
            [url]
        );
        let finished = Arc::new(AtomicBool::new(false));
        let peer = std::thread::spawn({
            let finished = Arc::clone(&finished);
            move || {
                let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
                let mut primaries = 0;
                loop {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            stream.set_nonblocking(false).unwrap();
                            stream
                                .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
                                .unwrap();
                            stream
                                .set_write_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
                                .unwrap();
                            let mut header = Vec::new();
                            while !header.ends_with(b"\r\n\r\n") {
                                let mut byte = [0];
                                stream.read_exact(&mut byte).unwrap();
                                header.push(byte[0]);
                                assert!(header.len() < 16 * 1024);
                            }
                            let header = std::str::from_utf8(&header).unwrap();
                            if !header.starts_with("GET /v1/native-budget HTTP/1.1\r\n") {
                                let line = header.lines().next().unwrap();
                                assert!(
                                    line.starts_with("GET /v1/native-budget/records")
                                        || line.starts_with("POST /v1/native-budget/records")
                                );
                                let _ = stream.write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                                continue;
                            }
                            let observed_reader = header.lines().find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case(transport::SUBSCRIBER_HEADER)
                                    .then(|| value.trim())
                            });
                            assert_eq!(
                                observed_reader,
                                subscriber.as_deref(),
                                "the original caller's reader identity policy stands"
                            );
                            primaries += 1;
                            let head = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                bytes.len()
                            );
                            stream.write_all(head.as_bytes()).unwrap();
                            stream.write_all(&bytes).unwrap();
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if finished.load(Ordering::Acquire) || Instant::now() >= until {
                                return primaries;
                            }
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("owned native primary: {error}"),
                    }
                }
            }
        });
        Self {
            paths,
            expected,
            finished,
            peer: Some(peer),
            _root: root,
        }
    }

    fn finish(&mut self) -> usize {
        self.finished.store(true, Ordering::Release);
        self.peer.take().unwrap().join().unwrap()
    }
}

impl Drop for NativePrimary {
    fn drop(&mut self) {
        self.finished.store(true, Ordering::Release);
        if let Some(peer) = self.peer.take() {
            let _ = peer.join();
        }
    }
}

#[test]
fn a_required_admin_open_waits_for_its_native_primary() {
    let mut primary = NativePrimary::new(Caller::Admin);
    let mut opened = Vec::new();
    for age in [36, 61, 0] {
        let files = [
            primary.paths.source_file(),
            primary.paths.cache_file(),
            primary.paths.identity_file(),
        ];
        let before: Vec<_> = files
            .iter()
            .map(|path| std::fs::read(path).unwrap())
            .collect();
        let got = admin_open_with_candidates_started(
            &primary.paths,
            &[],
            Instant::now() - Duration::from_secs(age),
        )
        .map(|admin| admin.seq());
        if age == 61 {
            assert!(
                got.is_err(),
                "actual expiry obtains no primary authorization"
            );
            let after: Vec<_> = files
                .iter()
                .map(|path| std::fs::read(path).unwrap())
                .collect();
            assert_eq!(
                after, before,
                "expiry retains the source, cache and native identity"
            );
        } else {
            opened.push(got);
        }
    }
    let mut config = config(&primary.paths).unwrap().unwrap();
    let pin = config.signer;
    config.signer = Some(envelope::signer_public(&[91; 32]).unwrap());
    save_config(&primary.paths, &config).unwrap();
    let wrong_pin = admin_open(&primary.paths);
    config.signer = pin;
    save_config(&primary.paths, &config).unwrap();
    let fresh = admin_open(&primary.paths).map(|admin| admin.seq());
    let requests = primary.finish();
    assert_eq!(
        requests, 4,
        "live reads, wrong pin and recovery only; expiry sends nothing"
    );
    for got in opened {
        assert_eq!(got.unwrap(), primary.expected);
    }
    assert!(
        wrong_pin.is_err(),
        "the real signed reply cannot bypass the configured pin"
    );
    assert_eq!(fresh.unwrap(), primary.expected);
}

#[test]
fn a_required_join_waits_for_its_native_primary() {
    let mut primary = NativePrimary::new(Caller::Join);
    let configured = config(&primary.paths).unwrap().unwrap();
    let Source::Url(url) = &configured.source else {
        panic!("owned native primary URL");
    };
    let options = JoinOptions {
        source: format!(
            "{url}#{}",
            crate::b64::encode_padded(&configured.signer.unwrap())
        ),
        host: Some("fixture".into()),
        interactive: false,
        wait: Duration::ZERO,
        confirm: &|_| false,
        password: JoinPassword::Given(None),
        fetched: None,
        requested: &|| {},
        say: &|_| {},
    };
    let mut joined = Vec::new();
    for age in [36, 61, 0] {
        let files = [
            primary.paths.source_file(),
            primary.paths.cache_file(),
            primary.paths.identity_file(),
        ];
        let before: Vec<_> = files
            .iter()
            .map(|path| std::fs::read(path).unwrap())
            .collect();
        let got = join_live_started(
            &primary.paths,
            &options,
            ExpectedConfig::Read(Some(&configured)),
            Instant::now() - Duration::from_secs(age),
        );
        if age == 61 {
            assert!(got.is_err(), "expiry cannot join or queue an admission");
            let after: Vec<_> = files
                .iter()
                .map(|path| std::fs::read(path).unwrap())
                .collect();
            assert_eq!(
                after, before,
                "expiry retains the exact source, cache and native identity"
            );
        } else {
            joined.push(got);
        }
    }
    let fresh = join(&primary.paths, &options);
    let requests = primary.finish();
    assert_eq!(
        requests, 3,
        "only the two live joins and fresh recovery ask the primary"
    );
    for got in joined.into_iter().chain([fresh]) {
        match got.unwrap() {
            JoinOutcome::Joined { seq, .. } => assert_eq!(seq, primary.expected),
            _ => panic!("the existing authenticated member joins, without a pending request"),
        }
    }
}
