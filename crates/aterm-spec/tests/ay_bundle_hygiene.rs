// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Source-tree hygiene for the external `ay` certificate escalation.
//!
//! `ay solve` can emit a proof beside its input by default. The proof bundles
//! contain tracked sidecars, so a verification run must opt out explicitly:
//! release `--gate` runs these tests after its clean-tree check and a rewritten
//! sidecar would otherwise produce a `-dirty` binary or abort the cut.

use std::fs;
use std::path::Path;

fn check_bundle_dir(dir: &Path, saw_solver: &mut bool) {
    for entry in fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("read ay proof bundle directory {}: {error}", dir.display()))
    {
        let path = entry.expect("read ay proof bundle entry").path();
        if path.is_dir() {
            check_bundle_dir(&path, saw_solver);
            continue;
        }
        if path.file_name().and_then(|name| name.to_str()) != Some("verify.sh") {
            continue;
        }

        let script = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        for (index, line) in script.lines().enumerate() {
            if !line.contains("\"$AY\" solve") {
                continue;
            }
            *saw_solver = true;
            assert!(
                line.contains("solve --no-proof"),
                "{}:{} invokes `ay solve` without `--no-proof`; verification must not rewrite tracked proof sidecars",
                path.display(),
                index + 1,
            );
        }
    }
}

#[test]
fn ay_bundle_verifiers_never_rewrite_tracked_proof_sidecars() {
    let proof_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../aterm-spec-models/proofs/ay");
    let mut saw_solver = false;
    check_bundle_dir(&proof_root, &mut saw_solver);
    assert!(saw_solver, "no ay bundle solver invocation was checked");
}
