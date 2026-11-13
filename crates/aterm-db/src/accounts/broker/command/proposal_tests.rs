// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn inspection_is_offline_and_bad_proposals_never_open_a_vault() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let file = dir.path().join("policy.toml");
    std::fs::write(&file, "version = 1\n").unwrap();
    let inspect = run_with(
        "accounts-broker-inspect",
        BTreeMap::from([("--file", file.to_str().unwrap())]),
        || panic!("offline inspection"),
    )
    .unwrap();
    assert_eq!(inspect["authority"], json!("none"));
    assert_eq!(inspect["grants"], json!([]));
    assert!(inspect.get("policy").is_none());
    let account = "31".repeat(32);
    let id = "61".repeat(16);
    let producer = "62".repeat(32);
    for (key, value) in [
        ("--seconds", "0"),
        ("--seconds", "3601"),
        ("--account", "sensitive-sentinel"),
        ("--grant", "sensitive-sentinel"),
        ("--producer", "sensitive-sentinel"),
        ("--unknown", "sensitive-sentinel"),
    ] {
        let mut options = BTreeMap::from([
            ("--account", account.as_str()),
            ("--grant", id.as_str()),
            ("--producer", producer.as_str()),
        ]);
        options.insert(key, value);
        let error = run_with("accounts-broker-propose", options, || {
            panic!("invalid input touched vault")
        })
        .unwrap_err();
        assert!(!error.to_string().contains("sensitive-sentinel"));
    }
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "version = 1\n");
}

#[test]
fn refresh_options_are_closed_before_opening_any_vault() {
    let id = "61".repeat(16);
    let zero = "00".repeat(16);
    for options in [
        BTreeMap::new(),
        BTreeMap::from([("--grant", "sensitive-sentinel")]),
        BTreeMap::from([("--grant", zero.as_str())]),
        BTreeMap::from([
            ("--grant", id.as_str()),
            ("--producer", "sensitive-sentinel"),
        ]),
        BTreeMap::from([("--grant", id.as_str()), ("--base", "sensitive-sentinel")]),
    ] {
        let error = run_with("accounts-broker-refresh", options, || {
            panic!("invalid refresh opened vault")
        })
        .unwrap_err();
        assert!(!error.to_string().contains("sensitive-sentinel"));
    }
}

#[test]
fn publication_options_are_closed_before_opening_any_vault() {
    let id = "61".repeat(16);
    let base = "62".repeat(32);
    let zero_id = "00".repeat(16);
    let zero_base = "00".repeat(32);
    let valid = BTreeMap::from([
        ("--grant", id.as_str()),
        ("--base", base.as_str()),
        ("--file", "missing-policy.toml"),
    ]);
    for (key, value) in [
        ("--grant", "sensitive-sentinel"),
        ("--base", "sensitive-sentinel"),
        ("--grant", zero_id.as_str()),
        ("--base", zero_base.as_str()),
        ("--unknown", "sensitive-sentinel"),
    ] {
        let mut options = valid.clone();
        options.insert(key, value);
        let error = run_with("accounts-broker-publish", options, || {
            panic!("invalid publication opened vault")
        })
        .unwrap_err();
        assert!(!error.to_string().contains("sensitive-sentinel"));
    }
    for missing in ["--grant", "--base", "--file"] {
        let mut options = valid.clone();
        options.remove(missing);
        assert!(
            run_with("accounts-broker-publish", options, || panic!(
                "incomplete publication opened vault"
            ))
            .is_err()
        );
    }
}

#[test]
fn extending_an_untrusted_base_never_replaces_a_producer_or_id() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let file = dir.path().join("base.toml");
    let grant = Grant {
        id: [61; 16],
        producer: [62; 32],
        actor: [63; 48],
        lifetime: Duration::from_secs(60),
        binding: crate::resources::RequestBinding {
            principal: [64; 32],
            account: [65; 32],
            revision: [66; 32],
            credential: [66; 32],
            policy: [67; 32],
            adapter: "example-v1".into(),
        },
    };
    let original = Policy::new(vec![grant]).unwrap().text();
    std::fs::write(&file, &original).unwrap();
    let account = "41".repeat(32);
    let id = "3d".repeat(16);
    let producer = "3e".repeat(32);
    let other_id = "4f".repeat(16);
    let other_producer = "50".repeat(32);
    for (id, producer) in [(&id, &other_producer), (&other_id, &producer)] {
        let options = BTreeMap::from([
            ("--account", account.as_str()),
            ("--grant", id.as_str()),
            ("--producer", producer.as_str()),
            ("--base", file.to_str().unwrap()),
        ]);
        assert!(
            run_with("accounts-broker-propose", options, || panic!(
                "replacement touched vault"
            ))
            .is_err()
        );
    }
    let inspected = crate::cli::run(&[
        "accounts-broker-inspect".into(),
        "--file".into(),
        file.to_str().unwrap().into(),
    ])
    .unwrap();
    assert_eq!(inspected["grants"][0]["producer"], json!(producer));
    assert_eq!(std::fs::read_to_string(file).unwrap(), original);
}
