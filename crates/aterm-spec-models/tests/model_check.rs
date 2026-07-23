// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Model-checking as a test (ROADMAP WS-I): drives the `ty` TLA+ explicit-state
// checker over every spec in `specs/` and asserts the invariants hold. This
// makes formal model-checking a first-class `cargo test` + CI artifact rather
// than a manual side ritual.
//
// `ty` is located by a fixed canonical path search (in order): the Trust
// first-party submodule ($HOME/trust/first-party/ty/target/release/ty), the Trust
// stage2 build ($HOME/trust/build/host/stage2/bin/ty), ~/ty/target/release/ty, then
// `ty` on PATH. VERIFICATION GATE (honesty ratchet, batteries-on, see
// `aterm_spec::verify`): verification is always required — an absent Trust `ty` FAILS
// the test with a build hint; build the toolchain once (`cargo build --release -p
// tla-cli` in $HOME/trust/first-party/ty). The canonical search includes the Trust
// submodule path so a standard `cargo test` model-checks the specs with no
// configuration.

use aterm_spec::verify::ty_escalation;
use std::path::{Path, PathBuf};
use std::process::Command;

fn specs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("specs")
}

#[test]
fn ty_model_checks_every_spec() {
    // TIERED (VERIFY-1): hand-written `.tla` specs have no in-process
    // interpreter form (it evaluates derived `Model`s, not TLA+ text), so this
    // external-tool obligation runs only where the Trust toolchain is
    // installed — absent, the prominent notice names exactly what did not run.
    let Some(ty) = ty_escalation("specs") else { return };

    let dir = specs_dir();
    let mut checked = 0usize;
    let mut names: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("read specs/") {
        let path = entry.expect("dir entry").path();
        // Phase 1 (TRUST_NATIVE_TLA): the kernel-family specs were quarantined into
        // `specs/legacy/` (superseded by derived twins). The checked set is the
        // ACTIVE top-level `.tla` only — skip the `legacy/` subdir (and any dir).
        if path.is_dir() {
            assert_eq!(
                path.file_name().and_then(|n| n.to_str()),
                Some("legacy"),
                "unexpected subdirectory in specs/: {path:?}"
            );
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("tla") {
            continue;
        }
        let cfg = path.with_extension("cfg");
        assert!(cfg.exists(), "spec {path:?} has no matching .cfg");

        let out = Command::new(&ty)
            .arg("check")
            .arg(&path)
            .arg("--config")
            .arg(&cfg)
            .output()
            .unwrap_or_else(|e| panic!("failed to run {ty:?}: {e}"));

        assert!(
            out.status.success(),
            "ty check FAILED for {path:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        eprintln!(
            "ty: {} — invariants hold",
            path.file_name().unwrap().to_string_lossy()
        );
        names.push(path.file_name().unwrap().to_string_lossy().into_owned());
        checked += 1;
    }
    assert!(checked > 0, "no .tla specs found in {dir:?}");
    // The ISOLATION-family ledger: after Phase 1 the ACTIVE specs/ holds exactly the
    // capability/sandbox boundary specs (full-TLA+ design intent; bound to source in
    // Phase 2). Every spec named here MUST exist and be checked.
    for required in [
        "Sandbox.tla",     // capability sandbox entry/confinement
        "PathConfine.tla", // path-confinement boundary
        "ForkExec.tla",    // fork/exec capability gate
        "WriteAll.tla",    // atomic write-all discipline
        "AltScreen.tla",   // alternate-screen save/restore
        "GpuEncode.tla",   // GPU encode pipeline ordering
    ] {
        assert!(
            names.iter().any(|n| n == required),
            "required ISOLATION spec missing: {required}"
        );
    }
    // The kernel-family specs (Kernel/Subscribe/Snapshot/Transact/Evict) are
    // SUPERSEDED by drift-free derived twins (aterm-spec::derive) and were
    // quarantined into specs/legacy/ in Phase 1 — they are intentionally NOT in the
    // checked set here. Their derived twins ARE exhaustively `ty check`ed in
    // aterm-spec/tests/derived_ring_ty.rs (one source of truth).
    for retired in [
        "Kernel.tla",
        "Subscribe.tla",
        "Snapshot.tla",
        "Transact.tla",
        "Evict.tla",
        "FdLifecycle.tla",
    ] {
        assert!(
            !names.iter().any(|n| n == retired),
            "spec {retired} must be quarantined to specs/legacy/ (Phase 1), not in the active checked set"
        );
    }
    eprintln!(
        "ty model-checked {checked} active ISOLATION spec(s); kernel family is derived (legacy/ quarantined)"
    );
}
