// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::resources::Terms;
use aterm_secrets::commons::journal::Trust;

fn resource(principal: u8, unit: &str) -> Resource {
    Resource {
        allocation: Proposal::prepare(
            Trust {
                network: [1; 16],
                authority: [2; 32],
                genesis: [3; 32],
            },
            Terms {
                resource: [principal; 32],
                window: [12; 32],
                unit: unit.into(),
                limit: 3,
                shares: BTreeMap::from([([4; 48], 3)]),
            },
        )
        .unwrap(),
        provider: "example".into(),
        adapter: "example-v1".into(),
    }
}

fn offline(command: &str, options: BTreeMap<&str, &str>) -> Result<Value> {
    run_with(command, options, || {
        panic!("offline command touched vault paths")
    })
}

#[test]
fn cli_proposes_exact_policy_without_a_vault_or_authority() {
    let row = resource(11, "requests");
    let allocation = hex::encode_lower(row.allocation.bytes());
    let expected = Policy::new(vec![row]).unwrap();
    let output = crate::cli::run(&[
        "accounts-billing-propose".into(),
        "--allocation".into(),
        allocation.clone(),
        "--provider".into(),
        "example".into(),
        "--adapter".into(),
        "example-v1".into(),
    ])
    .unwrap();
    assert_eq!(output["authority"], json!("none"));
    assert_eq!(output["entry"], json!(ENTRY));
    assert_eq!(output["policy"], json!(expected.text()));
    assert_eq!(
        output["resources"][0]["billing"],
        expected.reference([11; 32]).unwrap().value()
    );
    assert_eq!(output["resources"][0]["limit"], json!(3));
    assert_eq!(
        output["resources"][0]["proposed_root"]["network"],
        json!(hex::encode_lower(&[1; 16]))
    );
    assert_eq!(output["resources"][0]["shares"][0]["limit"], json!(3));
    assert_eq!(
        offline(
            "accounts-billing-propose",
            BTreeMap::from([
                ("--allocation", allocation.as_str()),
                ("--provider", "example"),
                ("--adapter", "example-v1"),
            ])
        )
        .unwrap(),
        output
    );
}

#[test]
fn extending_policy_preserves_base_and_refuses_existing_principals() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let file = dir.path().join("base.toml");
    let path = file.to_str().unwrap();
    let base = Policy::new(vec![resource(11, "requests")]).unwrap();
    let original = base.text();
    std::fs::write(&file, &original).unwrap();
    let allocation = hex::encode_lower(resource(13, "requests").allocation.bytes());
    let result = offline(
        "accounts-billing-propose",
        BTreeMap::from([
            ("--allocation", allocation.as_str()),
            ("--provider", "example"),
            ("--adapter", "example-v2"),
            ("--base", path),
        ]),
    )
    .unwrap();
    let proposed = result["policy"].as_str().unwrap();
    let extended = Policy::parse(proposed.as_bytes()).unwrap();
    assert_eq!(extended.reference([11; 32]), base.reference([11; 32]));
    assert_eq!(extended.resources().count(), 2);
    let inspect = offline(
        "accounts-billing-inspect",
        BTreeMap::from([("--file", path)]),
    )
    .unwrap();
    assert_eq!(inspect["authority"], json!("none"));
    assert_eq!(inspect["resources"].as_array().unwrap().len(), 1);
    assert!(inspect.get("policy").is_none());
    let duplicate = hex::encode_lower(resource(11, "requests").allocation.bytes());
    assert!(
        offline(
            "accounts-billing-propose",
            BTreeMap::from([
                ("--allocation", duplicate.as_str()),
                ("--provider", "example"),
                ("--adapter", "replacement"),
                ("--base", path),
            ])
        )
        .is_err()
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
}

#[test]
fn malformed_options_and_terms_refuse_before_opening_any_vault() {
    let account = hex::encode_lower(&[31; 32]);
    for (command, options) in [
        (
            "accounts-billing-prepare",
            BTreeMap::from([
                ("--account", account.as_str()),
                ("--unknown", "sensitive-sentinel"),
            ]),
        ),
        (
            "accounts-billing-prepare",
            BTreeMap::from([("--account", "sensitive-sentinel")]),
        ),
        ("accounts-billing-prepare", BTreeMap::new()),
        (
            "accounts-billing-inspect",
            BTreeMap::from([("--file", "sensitive-sentinel"), ("--unknown", "value")]),
        ),
        ("unsupported", BTreeMap::new()),
    ] {
        let err = offline(command, options).unwrap_err().to_string();
        assert!(!err.contains("sensitive-sentinel"));
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing-base");
    for row in [
        resource(11, "tokens"),
        Resource {
            adapter: "invalid adapter".into(),
            ..resource(11, "requests")
        },
        Resource {
            provider: "invalid provider".into(),
            ..resource(11, "requests")
        },
    ] {
        let allocation = hex::encode_lower(row.allocation.bytes());
        let mut options = BTreeMap::from([
            ("--allocation", allocation.as_str()),
            ("--provider", row.provider.as_str()),
            ("--adapter", row.adapter.as_str()),
        ]);
        let invalid = offline("accounts-billing-propose", options.clone()).unwrap_err();
        options.insert("--base", missing.to_str().unwrap());
        let with_base = offline("accounts-billing-propose", options).unwrap_err();
        assert_eq!(
            invalid.to_string(),
            with_base.to_string(),
            "invalid terms must refuse before the missing file is read"
        );
    }
    let too_large = "f".repeat(8193);
    assert!(
        offline(
            "accounts-billing-propose",
            BTreeMap::from([
                ("--allocation", too_large.as_str()),
                ("--provider", "example"),
                ("--adapter", "example-v1"),
            ])
        )
        .is_err()
    );
    assert!(
        crate::cli::run(&[
            "accounts-billing-prepare".into(),
            "--account".into(),
            account.clone(),
            "--account".into(),
            account
        ])
        .is_err()
    );
}

#[test]
fn policy_file_reads_are_bounded_regular_and_do_not_quote_contents_or_paths() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let file = dir.path().join("sensitive-path-sentinel");
    for contents in [
        b"sensitive-content-sentinel".to_vec(),
        vec![b' '; policy::MAX_BYTES + 1],
    ] {
        std::fs::write(&file, contents).unwrap();
        let err = read_policy(file.to_str().unwrap())
            .err()
            .unwrap()
            .to_string();
        assert!(!err.contains("sensitive-content-sentinel"));
        assert!(!err.contains("sensitive-path-sentinel"));
    }
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    assert!(read_policy(link.to_str().unwrap()).is_err());
    assert!(read_policy(dir.path().to_str().unwrap()).is_err());
    assert!(read_policy(dir.path().join("missing").to_str().unwrap()).is_err());
}
