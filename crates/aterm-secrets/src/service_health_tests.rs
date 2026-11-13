// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn health() -> HealthRequirement {
    HealthRequirement {
        vault: "repair-health".to_owned(),
        signer: "a".repeat(64),
        records: vec!["b".repeat(64), "c".repeat(64)],
        frontier: 17,
    }
}

fn write_response(paths: &Paths, name: &str, value: &aterm_json::Value) {
    crate::fsx::write_atomic(
        &paths.dir.join(name),
        &aterm_json::to_vec(value).unwrap(),
        0o600,
    )
    .unwrap();
}

#[test]
fn subscriber_health_binds_challenge_process_image_vault_and_complete_frontier() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    crate::fsx::ensure_private_dir(&paths.dir).unwrap();
    let health = health();
    let nonce = prepare(&paths).unwrap();
    let image = "d".repeat(64);
    let valid = json!({"nonce":nonce,"pid":1234,"image":image,"vault":health.vault,
        "signer":health.signer,"sequence":18,"records":health.records});
    assert!(subscriber(&paths, &nonce, 1234, &image, &health).is_err());
    write_response(&paths, "service-health-response.json", &valid);
    subscriber(&paths, &nonce, 1234, &image, &health).unwrap();
    for (field, bad) in [
        ("nonce", json!("previous challenge")),
        ("pid", json!(1235)),
        ("image", json!("e".repeat(64))),
        ("vault", json!("another-vault")),
        ("signer", json!("f".repeat(64))),
        ("sequence", json!(16)),
        ("records", json!([health.records[0]])),
    ] {
        let mut changed = valid.clone();
        changed.as_object_mut().unwrap().insert(field.into(), bad);
        write_response(&paths, "service-health-response.json", &changed);
        assert!(
            subscriber(&paths, &nonce, 1234, &image, &health).is_err(),
            "accepted wrong {field}"
        );
    }
    let next = prepare(&paths).unwrap();
    assert_ne!(nonce, next);
    write_response(&paths, "service-health-response.json", &valid);
    assert!(subscriber(&paths, &next, 1234, &image, &health).is_err());
}

#[test]
fn server_challenge_is_bound_to_process_port_and_written_once() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    crate::fsx::ensure_private_dir(&paths.dir).unwrap();
    let nonce = prepare_server(&paths).unwrap();
    let pid = std::process::id();
    assert!(server_process(&paths, &nonce, pid, 7100).is_err());
    bound_server(&paths, "127.0.0.1:7100".parse().unwrap()).unwrap();
    server_process(&paths, &nonce, pid, 7100).unwrap();
    assert!(server_process(&paths, "stale challenge", pid, 7100).is_err());
    assert!(server_process(&paths, &nonce, pid.wrapping_add(1), 7100).is_err());
    assert!(server_process(&paths, &nonce, pid, 7101).is_err());
    let marker = json!({"nonce":nonce,"pid":pid,"port":7100,"retained":"first bound proof"});
    write_response(&paths, "server-health-response.json", &marker);
    bound_server(&paths, "127.0.0.1:7101".parse().unwrap()).unwrap();
    let retained: aterm_json::Value = aterm_json::from_slice(
        &std::fs::read(paths.dir.join("server-health-response.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(retained, marker);
    let fresh = prepare_server(&paths).unwrap();
    bound_server(&paths, "127.0.0.1:7101".parse().unwrap()).unwrap();
    server_process(&paths, &fresh, pid, 7101).unwrap();
    assert!(server_process(&paths, &nonce, pid, 7100).is_err());
}

#[test]
fn completed_subscriber_challenge_does_not_hash_or_write_each_round() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    crate::fsx::ensure_private_dir(&paths.dir).unwrap();
    let nonce = prepare(&paths).unwrap();
    let version = hex(&client::service_health_version(&paths).unwrap());
    let marker = json!({"nonce":nonce,"pid":std::process::id(),"retained":"completed round","version":version});
    write_response(&paths, "service-health-response.json", &marker);
    // No vault is configured: a fresh round cannot pass materialization or envelope checks.
    // The already completed process/challenge returns before those reads or any new write.
    after_round(&paths).unwrap();
    let retained: aterm_json::Value =
        aterm_json::from_slice(&std::fs::read(response(&paths)).unwrap()).unwrap();
    assert_eq!(retained, marker);
    crate::fsx::write_atomic(&paths.state_file(), b"seq = 2\n", 0o600).unwrap();
    assert!(
        after_round(&paths).is_err(),
        "new progress must not reuse the old answer"
    );
    prepare(&paths).unwrap();
    assert!(after_round(&paths).is_err());
}

#[test]
fn subscriber_health_version_tracks_progress_but_not_heartbeat_timestamps() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    crate::fsx::ensure_private_dir(&paths.dir).unwrap();
    let write =
        |text: &str| crate::fsx::write_atomic(&paths.state_file(), text.as_bytes(), 0o600).unwrap();
    write("seq = 1\nheard = 10\n");
    let first = client::service_health_version(&paths).unwrap();
    write("seq = 1\nheard = 11\n");
    assert_eq!(client::service_health_version(&paths).unwrap(), first);
    for text in [
        "seq = 2\n".to_owned(),
        format!("seq = 1\ndigest = {:?}\n", "a".repeat(64)),
        format!("seq = 1\nrecords_digest = {:?}\n", "b".repeat(64)),
        format!("seq = 1\nsource_identity = {:?}\n", "c".repeat(64)),
        "seq = 1\nfailed = true\n".to_owned(),
        "seq = 1\nmembership = \"revoked\"\n".to_owned(),
    ] {
        write(&text);
        assert_ne!(
            client::service_health_version(&paths).unwrap(),
            first,
            "{text}"
        );
    }
}

#[test]
fn subscriber_health_refuses_another_vault_signed_by_the_same_key() {
    let seed = [37; 32];
    let config = client::Config {
        source: crate::transport::Source::Path(PathBuf::from("/unused/repair-health.vault")),
        mirrors: Vec::new(),
        name: "repair-health".to_owned(),
        signer: Some(crate::envelope::signer_public(&seed).unwrap()),
        followed: Vec::new(),
        host: "test".to_owned(),
    };
    let signed = |name: &str| {
        crate::envelope::sign(
            &crate::envelope::Unsigned {
                name: name.to_owned(),
                seq: 1,
                time: 1,
                handoff: Vec::new(),
                parents: Vec::new(),
                lockers: Vec::new(),
            },
            &seed,
        )
        .unwrap()
    };
    verify_subscriber_envelope(&signed("repair-health"), &config).unwrap();
    assert!(verify_subscriber_envelope(&signed("other-vault"), &config).is_err());
    let mut forged = signed("repair-health");
    let at = forged.iter().position(|&b| b == b'1').unwrap();
    forged[at] = b'2';
    assert!(verify_subscriber_envelope(&forged, &config).is_err());
}

#[test]
fn server_health_address_uses_only_a_validated_configured_vault() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    crate::fsx::ensure_private_dir(&paths.dir).unwrap();
    crate::fsx::write_atomic(
        &paths.dir.join("source.toml"),
        b"path = '/unused/team.vault'\nname = 'team'\nhost = 'test'\n",
        0o600,
    )
    .unwrap();
    let args = [
        "secrets",
        "serve",
        "--listen",
        "[::]:7484",
        "--store",
        "~/vault-store",
    ]
    .map(str::to_owned);
    let (store, url) = server_address(&paths, &args, "team").unwrap();
    assert_eq!(store, root.path().join("vault-store"));
    assert_eq!(url, "http://[::1]:7484/v1/team");
    for wrong in [
        "alab",
        "other-vault",
        "../team",
        "team?query",
        "team/next",
        "",
    ] {
        assert!(
            server_address(&paths, &args, wrong).is_err(),
            "accepted {wrong:?}"
        );
    }
}

#[test]
fn server_health_verifies_real_signed_vault_and_membership_frontier() {
    let root = aterm_tempfile::tempdir().unwrap();
    let store = root.path().join("server");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let running = crate::server::start(
        listener,
        &crate::server::Options {
            listen: "127.0.0.1:0".parse().unwrap(),
            store: store.clone(),
            advertise: None,
            pins: Vec::new(),
            hold: std::time::Duration::from_millis(100),
            keep_awake: false,
        },
    )
    .unwrap();
    let url = format!("http://{}/v1/team", running.addr);
    let paths = Paths::for_home(&root.path().join("admin"));
    let result = (|| -> Result<Vec<bool>> {
        client::create(&paths, &url, None, Some("health-admin"))?;
        client::seed_vault_genesis(&paths, &root.path().join("team.genesis"))?;
        client::sync(&paths, None)?;
        let args = [
            "secrets".to_owned(),
            "serve".to_owned(),
            "--listen".to_owned(),
            format!("0.0.0.0:{}", running.addr.port()),
            "--store".to_owned(),
            store.to_str().unwrap().to_owned(),
        ];
        let (health_store, health_url) = server_address(&paths, &args, "team")?;
        assert_eq!(health_store, store);
        assert_eq!(health_url, url);
        assert!(server_address(&paths, &args, "alab").is_err());
        let bytes =
            crate::fsx::read_private(&paths.cache_file(), crate::envelope::MAX_ENVELOPE)?.unwrap();
        let envelope = crate::envelope::parse(bytes.as_slice())?;
        let records = client::load_records(&paths)?;
        let requirement = HealthRequirement {
            vault: envelope.name.clone(),
            signer: hex(&envelope.signer),
            frontier: envelope.seq,
            records: records.inventory().iter().map(|id| hex(id)).collect(),
        };
        assert!(!requirement.records.is_empty());
        let valid = server(&health_url, &requirement).is_ok();
        let nonce = prepare(&paths)?;
        after_round(&paths)?;
        let before: aterm_json::Value = aterm_json::from_slice(&std::fs::read(response(&paths))?)
            .map_err(|e| Error::new(e.to_string()))?;
        let admin = client::admin_open(&paths)?;
        let mut doc = admin.doc.clone();
        doc.set_entry(
            &crate::text::Audience::Everyone,
            "health-progress",
            b"new round",
        )
        .map_err(|error| Error::new(error.to_string()))?;
        client::admin_publish(&paths, &admin, &doc)?;
        after_round(&paths)?;
        let after: aterm_json::Value = aterm_json::from_slice(&std::fs::read(response(&paths))?)
            .map_err(|e| Error::new(e.to_string()))?;
        assert_eq!(after["nonce"].as_str(), Some(nonce.as_str()));
        assert_ne!(before["version"], after["version"]);
        assert_ne!(before["sequence"], after["sequence"]);
        let mut wrong_vault = requirement.clone();
        wrong_vault.vault = "another-vault".to_owned();
        let mut wrong_signer = requirement.clone();
        wrong_signer.signer = "0".repeat(64);
        let mut stale = requirement.clone();
        stale.frontier = after["sequence"].as_u64().unwrap() + 1;
        let mut missing = requirement.clone();
        missing.records.push("0".repeat(64));
        Ok(vec![
            valid,
            server(&url, &wrong_vault).is_err(),
            server(&url, &wrong_signer).is_err(),
            server(&url, &stale).is_err(),
            server(&url, &missing).is_err(),
        ])
    })();
    running.stop();
    assert_eq!(result.unwrap(), vec![true; 5]);
}
