// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State as ModelState;

/// `bytes` at `path` as only its owner may write it, whatever the umask: an import refuses a
/// file others can write, which a umask of 002 (Ubuntu's) gives a plain write.
fn private_file(path: &std::path::Path, bytes: &[u8]) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

pub(crate) fn creation_transition(state: &ModelState, action: &str) -> ModelState {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("m1"));
    let source = root.path().join("first.vault");
    let source_text = source.to_str().unwrap();
    let import = root.path().join("import");
    std::fs::create_dir(&import).unwrap();
    private_file(&import.join("example"), b"original");
    let (context, pending) = prepare(&paths, source_text, Some(&import), Some("m1")).unwrap();
    let initial = pending.envelope.clone();
    let config = pending.config().unwrap();
    let mut refused = false;
    let mut monotone = true;
    let mut device_reset = false;
    let scenario = match action {
        "Resume" => {
            resume(&paths).unwrap();
            1
        }
        "ChangedImport" => {
            std::fs::write(import.join("example"), b"changed").unwrap();
            create(&paths, source_text, Some(&import), Some("m1")).unwrap();
            assert_eq!(
                std::fs::read(paths.home.join(".secrets/example")).unwrap(),
                b"original"
            );
            2
        }
        "LostJournal" => {
            std::fs::remove_dir_all(paths.dir.join("creation-outbox")).unwrap();
            refused = create(&paths, source_text, Some(&import), Some("m1")).is_err();
            3
        }
        "ChangedIntent" => {
            refused = create(&paths, source_text, Some(&import), Some("m2")).is_err();
            4
        }
        "CompletedRetry" => {
            finish(&paths, context, pending).unwrap();
            let admin = admin_open(&paths).unwrap();
            admin_rotate(&paths, &admin, &admin.doc).unwrap();
            let before = std::fs::read(&source).unwrap();
            let version = load_state(&paths).unwrap().seq;
            let receipt = create(&paths, source_text, Some(&import), Some("m1")).unwrap();
            assert_eq!(
                receipt.signer,
                crate::b64::encode_padded(&config.signer.unwrap())
            );
            monotone = std::fs::read(&source).unwrap() == before
                && load_state(&paths).unwrap().seq == version
                && version == 2;
            5
        }
        "NewerLocal" => {
            transport::publish(&config.source, &initial).unwrap();
            save_config(&paths, &config).unwrap();
            sync(&paths, None).unwrap();
            let admin = admin_open(&paths).unwrap();
            admin_rotate(&paths, &admin, &admin.doc).unwrap();
            std::fs::remove_file(&source).unwrap();
            create(&paths, source_text, Some(&import), Some("m1")).unwrap();
            monotone = !source.exists() && load_state(&paths).unwrap().seq == 2;
            6
        }
        "ChangedConfig" => {
            let mut changed = config;
            changed.source = Source::Path(root.path().join("other.vault"));
            save_config(&paths, &changed).unwrap();
            refused = create(&paths, source_text, Some(&import), Some("m1")).is_err();
            7
        }
        "BeforeReady" => {
            save_claim(&paths, &pending.intent, [0; 32], Phase::Preparing).unwrap();
            std::fs::write(import.join("example"), b"changed").unwrap();
            resume(&paths).unwrap();
            8
        }
        "LostDevice" => {
            std::fs::remove_dir_all(paths.dir.join("commons")).unwrap();
            refused = resume(&paths).is_err();
            assert!(crate::commons::bootstrap::Device::open(&paths).is_err());
            device_reset = paths.dir.join("commons/device.age").exists();
            9
        }
        _ => panic!("unknown creation action"),
    };
    let published = source.exists();
    let exact = if published && scenario != 5 {
        std::fs::read(&source).unwrap() == initial
    } else {
        true
    };
    let mut next = state.clone();
    next.insert("scenario", scenario);
    next.insert("retained", 1);
    next.insert("published", i64::from(published));
    next.insert("exact", i64::from(exact));
    next.insert("refused", i64::from(refused));
    next.insert("monotone", i64::from(monotone));
    next.insert("device_reset", i64::from(device_reset));
    next
}

#[test]
fn creation_outbox_conforms_across_restart_damage_and_newer_state() {
    let model = aterm_models_db::creation_outbox_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = creation_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {next:?}",
                    action.name
                );
            }
        }
    }
    let initial = model.init_state();
    let mut mutant = creation_transition(&initial, "ChangedImport");
    mutant.insert("exact", 0);
    assert!(
        !model
            .successors("ChangedImport", &initial)
            .contains(&mutant)
    );
    assert!(!model.check_invariant("ExactPublication", &mutant));
}

#[test]
fn lost_http_ack_resumes_retained_creation_without_second_put() {
    use std::io::{Read, Write};
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("m1"));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let source = format!("http://{}/v1/first", listener.local_addr().unwrap());
    let (done, received) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut saved = Vec::new();
        for index in 0..4 {
            let (mut stream, _) = listener.accept().unwrap();
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
                assert!(head.len() < 16 * 1024);
            }
            let head = String::from_utf8(head).unwrap();
            if index == 2 {
                assert!(head.starts_with("PUT "));
                let size: usize = head
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap();
                saved.resize(size, 0);
                stream.read_exact(&mut saved).unwrap();
                // The server committed the body, then its connection died
                // before returning any status line.
                continue;
            }
            assert!(head.starts_with("GET "));
            if index < 2 {
                stream
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .unwrap();
            } else {
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    saved.len()
                )
                .unwrap();
                stream.write_all(&saved).unwrap();
            }
        }
        done.send(saved).unwrap();
    });
    assert!(create(&paths, &source, None, Some("m1")).is_err());
    let context = claim(&paths).unwrap().unwrap().context;
    let log = open(&paths, context, true).unwrap();
    let before = replay(&paths, &log, context).unwrap().unwrap().envelope;
    drop(log);
    resume(&paths).unwrap();
    assert_eq!(
        received.recv_timeout(Duration::from_secs(60)).unwrap(),
        before
    );
    worker.join().unwrap();
    assert_eq!(std::fs::read(paths.cache_file()).unwrap(), before);
    assert_eq!(load_state(&paths).unwrap().seq, 1);
}

#[test]
fn first_publication_preserves_a_file_created_after_the_absence_check() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("m1"));
    let source = root.path().join("first.vault");
    let (_, pending) = prepare(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
    std::fs::write(&source, b"a concurrent file").unwrap();
    assert!(transport::publish_initial(&pending.intent.source, &pending.envelope).is_err());
    assert_eq!(std::fs::read(&source).unwrap(), b"a concurrent file");
    assert!(config(&paths).unwrap().is_none());
}

#[test]
fn unpublished_preparation_resumes_from_its_durable_request() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("m1"));
    let source = root.path().join("first.vault");
    fsx::ensure_private_dir(&paths.dir).unwrap();
    let intent = Intent {
        source: Source::Path(source.clone()),
        host: "m1".into(),
        import: String::new(),
        recipient: machine_identity(&paths).unwrap().recipient().to_bech32(),
    };
    save_claim(&paths, &intent, [0; 32], Phase::Preparing).unwrap();
    resume(&paths).unwrap();
    assert_eq!(load_state(&paths).unwrap().seq, 1);
    assert!(source.exists());
    assert!(
        crate::commons::bootstrap::Bootstrap::required(&paths)
            .unwrap()
            .access()
            .unwrap()
            .is_some()
    );
}

#[test]
fn failed_local_materialization_resumes_without_republishing() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("m1"));
    let source = root.path().join("first.vault");
    let import = root.path().join("import");
    std::fs::create_dir(&import).unwrap();
    private_file(&import.join("example"), b"original");
    let blocked = paths.home.join(".secrets/example");
    std::fs::create_dir_all(&blocked).unwrap();
    let made = create(&paths, source.to_str().unwrap(), Some(&import), Some("m1")).unwrap();
    assert!(made.local.is_some());
    assert_eq!(load_state(&paths).unwrap().seq, 1);
    std::fs::remove_dir(&blocked).unwrap();
    std::fs::remove_file(&source).unwrap();
    resume(&paths).unwrap();
    assert_eq!(std::fs::read(&blocked).unwrap(), b"original");
    assert!(
        !source.exists(),
        "cached authenticated bytes repair the local files offline"
    );
    assert!(!load_state(&paths).unwrap().failed);
}

#[test]
fn remote_successor_is_installed_but_same_version_fork_is_refused() {
    for sequence in [1, 2] {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("m1"));
        let source = root.path().join("first.vault");
        let (_, pending) = prepare(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
        let mut admin = pending.admin(&paths).unwrap();
        admin
            .doc
            .set_entry(&Audience::Everyone, "remote-edit", b"retained")
            .unwrap();
        let bytes = seal(
            "first",
            sequence,
            &admin.seed,
            &admin.commons_seed,
            true,
            &admin.doc,
            &[],
        )
        .unwrap();
        transport::publish(&pending.intent.source, &bytes).unwrap();
        let result = create(&paths, source.to_str().unwrap(), None, Some("m1"));
        if sequence == 1 {
            assert!(result.is_err());
            assert!(config(&paths).unwrap().is_none());
        } else {
            assert_eq!(result.unwrap().local, None);
            assert_eq!(load_state(&paths).unwrap().seq, 2);
            assert_eq!(std::fs::read(paths.cache_file()).unwrap(), bytes);
        }
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
    }
}

#[test]
fn bootstrap_rechecks_the_slot_before_pinning_a_captured_observation() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("m1"));
    let source = root.path().join("first.vault");
    let (_, pending) = prepare(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
    let mut config = pending.config().unwrap();
    save_config(&paths, &config).unwrap();
    apply_fetched(
        &paths,
        config.clone(),
        State::default(),
        Fetched::Body(pending.envelope.clone()),
    )
    .unwrap();
    let snapshot = crate::commons::cached(&paths).unwrap().unwrap();
    assert!(!paths.dir.join("commons.claim").exists());
    config.source = Source::Path(root.path().join("another.vault"));
    save_config(&paths, &config).unwrap();
    assert!(install_bootstrap(&paths, &pending, &snapshot).is_err());
    assert!(!paths.dir.join("commons.claim").exists());
    assert!(!paths.dir.join("commons/default.pin").exists());
}

#[test]
fn invalid_import_and_orphan_history_do_not_reserve_or_publish_a_vault() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("m1"));
    let source = root.path().join("first.vault");
    assert!(
        create(
            &paths,
            source.to_str().unwrap(),
            Some(&root.path().join("missing")),
            Some("m1")
        )
        .is_err()
    );
    assert!(claim(&paths).unwrap().is_none());
    assert!(!source.exists());
    std::fs::create_dir_all(paths.dir.join("commons/control")).unwrap();
    assert!(create(&paths, source.to_str().unwrap(), None, Some("m1")).is_err());
    assert!(claim(&paths).unwrap().is_none());
    assert!(!source.exists());
    std::fs::remove_dir(paths.dir.join("commons/control")).unwrap();
    assert_eq!(
        create(&paths, source.to_str().unwrap(), None, Some("m1"))
            .unwrap()
            .local,
        None
    );
}

#[test]
fn unfinished_creation_refuses_a_join_and_completed_resume_does_no_outbox_io() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("m1"));
    let source = root.path().join("first.vault");
    let (context, pending) = prepare(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
    let plain = root.path().join("plain");
    private_file(&plain, b"[everyone]\ntoken=value\n");
    let options = JoinOptions {
        source: plain.to_str().unwrap().into(),
        host: Some("m1".into()),
        interactive: false,
        wait: Duration::ZERO,
        confirm: &|_| false,
        say: &|_| {},
        password: crate::client::JoinPassword::Given(None),
        fetched: None,
        requested: &|| {},
    };
    let refused = join(&paths, &options).unwrap_err();
    assert!(
        refused.to_string().contains("unfinished vault creation"),
        "{refused}"
    );
    assert!(config(&paths).unwrap().is_none());
    finish(&paths, context, pending).unwrap();
    std::fs::remove_dir_all(paths.dir.join("creation-outbox")).unwrap();
    resume(&paths).unwrap();
    assert!(create(&paths, source.to_str().unwrap(), None, Some("m1")).is_err());
}

#[test]
fn missing_cache_recovers_the_exact_initial_or_a_twice_rotated_remote_version() {
    for rotate in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("m1"));
        let source = root.path().join("first.vault");
        let (_, pending) = prepare(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
        transport::publish(&pending.intent.source, &pending.envelope).unwrap();
        save_config(&paths, &pending.config().unwrap()).unwrap();
        sync(&paths, None).unwrap();
        if rotate {
            for _ in 0..2 {
                let admin = admin_open(&paths).unwrap();
                admin_rotate(&paths, &admin, &admin.doc).unwrap();
            }
        } else {
            std::fs::remove_file(&source).unwrap();
        }
        std::fs::remove_file(paths.cache_file()).unwrap();
        assert_eq!(
            create(&paths, source.to_str().unwrap(), None, Some("m1"))
                .unwrap()
                .local,
            None
        );
        assert_eq!(load_state(&paths).unwrap().seq, if rotate { 3 } else { 1 });
        assert_eq!(source.exists(), rotate);
    }
}
