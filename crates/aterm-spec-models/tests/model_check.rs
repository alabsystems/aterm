// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Model-checking as a test (ROADMAP WS-I): drives the `ty` TLA+ explicit-state
// checker over every spec in `specs/` and asserts the invariants hold. This
// makes formal model-checking a first-class `targo --unverified test` obligation
// rather than a manual side ritual.
//
// `ty` is located by `aterm_spec::verify`'s discovery (the atpkg store's shim, then
// PATH), so a plain `targo --unverified test` model-checks the specs with no
// configuration.
// VERIFICATION GATE (honesty ratchet, batteries-on): an absent Trust `ty` is
// reported loudly (`aterm pkg install ty`), never read as a pass.

use aterm_spec::verify::ty_escalation;
use std::path::{Path, PathBuf};
use std::process::Command;

fn specs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("specs")
}

/// `ty_model_checks_every_spec` reads the top level of `specs/` only, so a
/// `.tla` in a subdirectory would never be checked. Runs with or without `ty`.
#[test]
fn specs_has_no_subdirectory() {
    for entry in std::fs::read_dir(specs_dir()).expect("read specs/") {
        let path = entry.expect("dir entry").path();
        assert!(
            !path.is_dir(),
            "unexpected subdirectory in specs/: {path:?} (no spec under it is model-checked)"
        );
    }
}

#[test]
fn ty_model_checks_every_spec() {
    // TIERED (VERIFY-1): hand-written `.tla` specs have no in-process
    // interpreter form (it evaluates derived `Model`s, not TLA+ text), so this
    // external-tool obligation runs only where the Trust toolchain is
    // installed — absent, the prominent notice names exactly what did not run.
    let Some(ty) = ty_escalation("specs") else {
        return;
    };

    let dir = specs_dir();
    let mut checked = 0usize;
    let mut names: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("read specs/") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("tla") {
            continue;
        }
        let cfg = path.with_extension("cfg");
        assert!(cfg.exists(), "spec {path:?} has no matching .cfg");

        let mut cmd = Command::new(&ty);
        cmd.arg("check").arg(&path).arg("--config").arg(&cfg);
        // ARMED. Every spec here: the ISOLATION family — Sandbox, PathConfine,
        // ForkExec, WriteAll, AltScreen, GpuEncode: security-boundary specs — and the
        // two PCP specs (PresentCoalescing, ResizeThrottle, whose Buggy=1 catch lives
        // in aterm-spec's *_ty tests). The `ty` this machine discovers has an
        // unsound partial-order reduction that prints "No errors found
        // (exhaustive)" and exits 0 on a spec whose invariant is violated three
        // steps from Init. The ISOLATION specs happen to agree POR-on and POR-off
        // today, so this is a latent hole rather than an active false proof —
        // armed anyway (every spec here), because "not currently tripping" is not
        // a property a security gate may rest on.
        aterm_spec::verify::arm_whole_space_check(&mut cmd);
        let out = aterm_spec::verify::ty_output(&mut cmd)
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
    // The ISOLATION-family ledger: every capability/sandbox boundary spec named
    // here MUST exist and be checked.
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
    eprintln!("ty model-checked {checked} spec(s)");
}
