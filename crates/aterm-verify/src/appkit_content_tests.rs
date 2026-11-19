// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const CONTEXT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn executable(name: &str, byte: char) -> Executable {
    Executable {
        name: name.to_owned(),
        sha256: byte.to_string().repeat(64),
    }
}

fn redraw(context: &str) -> GroupProof {
    GroupProof::from_executables(
        REDRAW_GROUP,
        context,
        vec![executable(crate::stages::REDRAW_CONFORMANCE_BIN, 'b')],
    )
    .unwrap()
}

fn objc(context: &str) -> GroupProof {
    GroupProof::from_executables(
        OBJC_GROUP,
        context,
        crate::stages::OBJC_DRIVES
            .iter()
            .enumerate()
            .map(|(index, drive)| {
                executable(
                    drive.example,
                    char::from(b'0' + u8::try_from(index).unwrap()),
                )
            })
            .collect(),
    )
    .unwrap()
}

fn runtime(cwd: &str, env: &[(&str, &str)]) -> Runtime {
    Runtime {
        commands: vec![RuntimeCommand {
            program: b"/private/tmp/target/aterm-redraw-conformance".to_vec(),
            args: Vec::new(),
            cwd: cwd.as_bytes().to_vec(),
            env: env
                .iter()
                .map(|(name, value)| (name.as_bytes().to_vec(), value.as_bytes().to_vec()))
                .collect(),
        }],
    }
}

#[test]
fn proofs_round_trip_in_canonical_group_order() {
    let expected: Passed = [redraw(CONTEXT), objc(CONTEXT)]
        .into_iter()
        .map(|proof| (proof.group.clone(), proof))
        .collect();
    let text = encode_passed(&expected).unwrap();
    assert_eq!(read_passed(&text).unwrap(), expected);
}

#[test]
fn key_binds_context_group_target_order_and_every_executable() {
    let proof = redraw(CONTEXT);
    assert_ne!(proof.content_key, redraw(&"c".repeat(64)).content_key);
    assert_ne!(proof.content_key, objc(CONTEXT).content_key);

    let mut changed = proof.executables.clone();
    changed[0].sha256 = "d".repeat(64);
    let changed = GroupProof::from_executables(REDRAW_GROUP, CONTEXT, changed).unwrap();
    assert_ne!(proof.content_key, changed.content_key);

    let mut wrong_order = objc(CONTEXT).executables;
    wrong_order.swap(0, 1);
    assert!(GroupProof::from_executables(OBJC_GROUP, CONTEXT, wrong_order).is_err());
}

/// Negative controls for the hostile-review gap: identical compiler output is not enough to
/// carry a verdict when either child-visible cwd bytes or any environment entry drift.
#[test]
fn key_binds_the_actual_finalized_cwd_and_whole_environment() {
    let executables = vec![executable(crate::stages::REDRAW_CONFORMANCE_BIN, 'b')];
    let baseline_runtime = runtime(
        "/private/tmp/.ubd/aterm-verify-appkit-501.noindex/redraw-conformance",
        &[],
    );
    let baseline = content_key(REDRAW_GROUP, CONTEXT, &executables, Some(&baseline_runtime));
    let mut program_drift = baseline_runtime.clone();
    program_drift.commands[0].program =
        b"/private/tmp/another-image/aterm-redraw-conformance".to_vec();
    assert_ne!(
        baseline,
        content_key(REDRAW_GROUP, CONTEXT, &executables, Some(&program_drift)),
        "equal bytes at a different executable path cannot borrow the original runtime proof"
    );
    let cwd_drift = runtime(
        "/private/tmp/.ubd/aterm-verify-appkit-501.noindex/other",
        &[],
    );
    assert_ne!(
        baseline,
        content_key(REDRAW_GROUP, CONTEXT, &executables, Some(&cwd_drift))
    );
    let env_drift = runtime(
        "/private/tmp/.ubd/aterm-verify-appkit-501.noindex/redraw-conformance",
        &[("HOME", "/Users//different")],
    );
    assert_ne!(
        baseline,
        content_key(REDRAW_GROUP, CONTEXT, &executables, Some(&env_drift))
    );
    assert_ne!(
        baseline,
        content_key(REDRAW_GROUP, CONTEXT, &executables, None),
        "a digest-only wire fixture can never impersonate a finalized runtime proof"
    );
}

#[test]
fn parser_rejects_unknown_duplicate_and_noncanonical_proofs() {
    let proof = redraw(CONTEXT);
    let line = aterm_json::to_string(&proof).unwrap();
    assert!(read_passed(&format!("{line}\n{line}\n")).is_err());

    let mut bad = proof.clone();
    bad.group = "elsewhere".into();
    assert!(read_passed(&aterm_json::to_string(&bad).unwrap()).is_err());
    bad = proof.clone();
    bad.content_key = "ABC".into();
    assert!(read_passed(&aterm_json::to_string(&bad).unwrap()).is_err());
    bad = proof;
    bad.executables[0].sha256 = "0".repeat(63);
    assert!(read_passed(&aterm_json::to_string(&bad).unwrap()).is_err());
}

#[test]
fn file_evidence_fails_closed_when_an_executable_is_missing() {
    let missing = std::path::Path::new("/definitely/not/an/appkit-driver").to_path_buf();
    assert!(
        GroupProof::from_paths(
            REDRAW_GROUP,
            CONTEXT,
            &[(crate::stages::REDRAW_CONFORMANCE_BIN, missing)]
        )
        .is_err()
    );
}

#[test]
fn event_is_bounded_structured_evidence() {
    let proof = redraw(CONTEXT);
    let line = event(&proof, true);
    let value: aterm_json::Value =
        aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap().trim_end()).unwrap();
    assert_eq!(value["event"].as_str(), Some("appkit-group-finished"));
    assert_eq!(value["group"].as_str(), Some(REDRAW_GROUP));
    assert_eq!(
        value["content_key"].as_str(),
        Some(proof.content_key.as_str())
    );
    assert_eq!(value["reused"].as_bool(), Some(true));
    assert_eq!(value["executables"].as_array().unwrap().len(), 1);
}

#[cfg(target_os = "macos")]
#[test]
fn appkit_build_receipt_requires_unique_original_bytes_and_matching_native_completion() {
    let root = std::path::Path::new("/private/owned/build");
    let manifest = root.join("crates/aterm-gui/Cargo.toml");
    let binary = root.join(crate::stages::REDRAW_CONFORMANCE_BIN);
    let paths = [(crate::stages::REDRAW_CONFORMANCE_BIN, binary.clone())];
    let row = aterm_json::json!({
        "reason":"compiler-artifact", "manifest_path":manifest,
        "target":{"name":crate::stages::REDRAW_CONFORMANCE_BIN,"kind":["bin"]},
        "executable":binary, "trust_compile_mode":"build", "trust_compile_kind":"host",
        "trust_compile_target":format!("{}-apple-darwin",std::env::consts::ARCH),
        "trust_unit_identity_sha256":"a".repeat(64), "trust_executable_sha256":"b".repeat(64),
        "profile":{"test":false,"debug_assertions":true}
    });
    let finished = "{\"reason\":\"build-finished\",\"success\":true}";
    let render = |value: &aterm_json::Value| {
        format!("{}\n{finished}\n", aterm_json::to_string(value).unwrap())
    };
    let good = render(&row);
    assert_eq!(
        producer_digests(&paths, &good, &manifest).unwrap()[crate::stages::REDRAW_CONFORMANCE_BIN],
        "b".repeat(64)
    );
    for (field, bad) in [
        ("trust_executable_sha256", aterm_json::Value::Null),
        ("trust_executable_sha256", aterm_json::json!("B".repeat(64))),
        ("manifest_path", aterm_json::json!("/another/Cargo.toml")),
        ("executable", aterm_json::json!("/another/executable")),
        ("trust_compile_mode", aterm_json::json!("test")),
        ("trust_compile_kind", aterm_json::json!("target")),
        (
            "trust_compile_target",
            aterm_json::json!("x86_64-unknown-linux-gnu"),
        ),
        (
            "profile",
            aterm_json::json!({"test":true,"debug_assertions":true}),
        ),
        (
            "profile",
            aterm_json::json!({"test":false,"debug_assertions":false}),
        ),
        (
            "target",
            aterm_json::json!({"name":"another-target","kind":["bin"]}),
        ),
        (
            "target",
            aterm_json::json!({"name":crate::stages::REDRAW_CONFORMANCE_BIN,"kind":["example"]}),
        ),
    ] {
        let mut value = row.clone();
        value.as_object_mut().unwrap().insert(field.to_owned(), bad);
        assert!(
            producer_digests(&paths, &render(&value), &manifest).is_err(),
            "{field}"
        );
    }
    let original = aterm_json::to_string(&row).unwrap();
    let mut conflicting = row.clone();
    conflicting.as_object_mut().unwrap().insert(
        "trust_executable_sha256".into(),
        aterm_json::json!("c".repeat(64)),
    );
    for bad in [
        original.clone(),
        format!("{original}\n{original}\n{finished}"),
        format!(
            "{original}\n{}\n{finished}",
            aterm_json::to_string(&conflicting).unwrap()
        ),
        format!("{good}{finished}\n"),
        format!("{original}\n{{\"reason\":\"build-finished\",\"success\":false}}"),
        format!("{original}\n{{\"reason\":\"build-finished\",\"success\":true,\"success\":true}}"),
        format!("{original}\nnot compiler stdout\n{finished}"),
        format!(
            "{}\n{finished}",
            original.replacen('{', "{\"trust_executable_sha256\":\"different\",", 1)
        ),
    ] {
        assert!(producer_digests(&paths, &bad, &manifest).is_err());
    }
}
