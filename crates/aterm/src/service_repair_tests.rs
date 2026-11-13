// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{CliInput, PROTOCOL, execute, execute_with_live, option, parse_cli_input};
use std::ffi::OsString;

#[test]
fn paired_operands_do_not_confuse_option_values_with_flag_names() {
    let operands = ["--job", "server", "--nonce", "--job"];
    assert_eq!(option(&operands, "--job").unwrap(), "server");
    assert_eq!(option(&operands, "--nonce").unwrap(), "--job");
    let missing_second = ["--evidence", "--image"];
    assert!(option(&missing_second, "--image").is_err());
}

#[test]
fn offline_verifier_accepts_historical_rollback_without_resolving_live_context() {
    use aterm_json::json;
    use aterm_secrets::service_repair as transaction;
    let node = "0123456789abcdef0123456789abcdef";
    let own = aterm_net::fleet::NodeIdentity::from_seed(node, &[19; 32]).unwrap();
    let request = json!({"node":node,"requester":node,"nonce":"offline-cli-verifier-test",
        "job":"systems.alab.aterm-secrets-server","current_image":"old","desired_image":"new",
        "precondition":{"unit":null,"off":false,"pin":null},
        "unit":{"program":"/inert/aterm","args":["secrets","serve"],"definition":"inert",
            "image":"new","executable_sha256":"inert"},
        "health":{"vault":"team","signer":"expected","records":["record"],"frontier":7},
        "required_protocols":[1]});
    let outcome = transaction::Outcome {
        request: transaction::parse_request(&request).unwrap(),
        verdict: transaction::Verdict::RolledBack,
        detail: "historical rollback receipt".into(),
    };
    let body = transaction::outcome_value(&outcome);
    let sig = own.sign_record(
        aterm_agent::service_activation::OUTCOME_DOMAIN,
        &aterm_json::to_vec(&body).unwrap(),
    );
    let signed =
        json!({"outcome":body,"node":node,"key":own.key().hex(),"signature":super::hex(&sig)});
    let dir = aterm_tempfile::tempdir().unwrap();
    let request_path = dir.path().join("request.json");
    let outcome_path = dir.path().join("outcome.json");
    for (path, value) in [(&request_path, &request), (&outcome_path, &signed)] {
        aterm_secrets::fsx::write_atomic(path, &aterm_json::to_vec(value).unwrap(), 0o600).unwrap();
    }
    let args = [
        "verify-outcome".into(),
        "--target-node".into(),
        node.into(),
        "--target-key".into(),
        own.key().hex(),
        "--request".into(),
        request_path.to_str().unwrap().into(),
        "--outcome".into(),
        outcome_path.to_str().unwrap().into(),
    ];
    let verified =
        execute_with_live(&args, || panic!("offline verifier consulted live context")).unwrap();
    assert_eq!(verified["verified"].as_bool(), Some(true));
    assert_eq!(verified["fresh_activation_proven"].as_bool(), Some(false));
    assert_eq!(verified["outcome"], body);
    assert_eq!(
        super::run(args.iter().map(|s| OsString::from(s.as_str())).collect()),
        std::process::ExitCode::SUCCESS
    );
    aterm_secrets::fsx::write_atomic(
        &outcome_path,
        &vec![b' '; aterm_agent::service_activation::MAX_SIGNED_OUTCOME_BYTES + 1],
        0o600,
    )
    .unwrap();
    assert!(execute_with_live(&args, || panic!("oversized input consulted live context")).is_err());
}

/// Standalone `--json` and `--help` tokens normalize only at flag boundaries, while `--json`
/// or `--help` passed as a paired option value is preserved verbatim, `protocol` rejects
/// trailing arguments, and empty local path options refuse before touching live paths.
/// NEGATIVE CONTROL: unfiltered `.filter(|s| *s != "--json")` stripped `--out --json`'s value
/// and `args.iter().any("--help")` treated `--request --help` as help instead of an option.
#[test]
fn cli_boundaries_preserve_paired_values_and_reject_trailing_or_empty_local_options() {
    let os = |items: &[&str]| items.iter().map(OsString::from).collect::<Vec<_>>();
    assert_eq!(parse_cli_input(os(&["--help"])).unwrap(), CliInput::Help);
    assert_eq!(
        parse_cli_input(os(&["--json", "-h"])).unwrap(),
        CliInput::Help
    );
    assert_eq!(
        parse_cli_input(os(&["--host", "m2", "inspect", "--help"])).unwrap(),
        CliInput::Help
    );
    assert_eq!(
        parse_cli_input(os(&["activate", "--request", "--help"])).unwrap(),
        CliInput::Command(vec!["activate".into(), "--request".into(), "--help".into()])
    );
    assert_eq!(
        parse_cli_input(os(&[
            "--json", "--host", "m2", "--json", "recover", "--job", "server", "--json"
        ]))
        .unwrap(),
        CliInput::Command(vec![
            "--host".into(),
            "m2".into(),
            "recover".into(),
            "--job".into(),
            "server".into()
        ])
    );
    assert_eq!(
        parse_cli_input(os(&["sign", "--request", "in.json", "--out", "--json"])).unwrap(),
        CliInput::Command(vec![
            "sign".into(),
            "--request".into(),
            "in.json".into(),
            "--out".into(),
            "--json".into()
        ])
    );
    assert_eq!(
        execute(&["protocol".into(), "--json".into()]).unwrap()["max"].as_u64(),
        Some(PROTOCOL)
    );
    assert!(execute(&["protocol".into(), "--bogus".into()]).is_err());
    assert!(
        execute(&[
            "stage".into(),
            "--evidence".into(),
            String::new(),
            "--image".into(),
            "/tmp/image".into()
        ])
        .is_err()
    );
    assert!(execute(&["activate".into(), "--request".into(), String::new()]).is_err());
    assert!(
        execute(&[
            "sign".into(),
            "--request".into(),
            "/tmp/in.json".into(),
            "--out".into(),
            String::new()
        ])
        .is_err()
    );
    assert!(
        execute(&[
            "prepare".into(),
            "--job".into(),
            "server".into(),
            "--image".into(),
            "not-hex".into(),
            "--nonce".into(),
            "valid-nonce-123456".into(),
            "--out".into(),
            "/tmp/out.json".into()
        ])
        .is_err()
    );
}
