// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn complete_and_identity_publication_checks_actual_take_despite_unchanged_observers() {
    let a = "a".repeat(64);
    let b = "b".repeat(64);
    let observed = Compilers {
        trustc: "trust".into(),
        rustc: "stock".into(),
        inputs: a.clone(),
    };
    for (actual, after, accepted) in [
        (Some(a.clone()), a.clone(), true),
        // Both independent parent observations still say A after a take built under B.
        (Some(b.clone()), a.clone(), false),
        (Some(a.clone()), b.clone(), false),
        (
            crate::buildplan::TakePurpose::Native.producer_inputs(Some(a.clone())),
            a.clone(),
            !cfg!(target_os = "macos"),
        ),
    ] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let complete_path = dir.path().join(COMPLETE_FILE);
        let index_path = dir.path().join("identity-entry");
        let output = crate::buildplan::BuildOutput {
            app: dir.path().join("aterm.app"),
            snapshot_digest: "snapshot".into(),
            inputs: actual,
            slices: Vec::new(),
            archs: "x86_64 arm64".into(),
            compiler_line: "fixture".into(),
            dsym: None,
            dsym_zip: None,
            x86_64_probe: dir.path().join("probe"),
        };
        let result = publish_build_result(
            output,
            observed.clone(),
            Compilers {
                inputs: after,
                ..observed.clone()
            },
            |built, before| {
                let complete = Complete {
                    schema: SCHEMA.into(),
                    key: "fixture".into(),
                    version: "0.1.0".into(),
                    build: 1,
                    commit: "c".repeat(40),
                    trustc: before.trustc,
                    rustc: before.rustc,
                    inputs: before.inputs,
                    snapshot: built.snapshot_digest,
                    archs: built.archs,
                    compiler_line: built.compiler_line,
                    macho_sha256: "d".repeat(64),
                    bundle_digest: "e".repeat(64),
                    content: None,
                    slices: Vec::new(),
                    index_hint: None,
                };
                write_complete(dir.path(), &complete)?;
                crate::durable::write_atomic_durable(&index_path, complete.key.as_bytes(), 0o600)
                    .map_err(|error| error.to_string())
            },
        );
        assert_eq!(result.is_ok(), accepted, "{result:?}");
        assert_eq!(complete_path.exists(), accepted);
        assert_eq!(index_path.exists(), accepted);
    }
}
