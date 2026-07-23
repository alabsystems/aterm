// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Tier-0 + reconciliation for the A7 fd-lifecycle DERIVED model.
//!
//! `fd_lifecycle_model()` (machine `FdLifecycle`) is the drift-free, code-bound twin
//! of the `SinkWriter` PTY-master ownership discipline in `aterm-session/src/sink.rs`
//! (initiative A7, WS-G). It SUPERSEDES the hand-written `FdLifecycle.tla`, which is
//! now quarantined to `aterm-spec-models/specs/legacy/` — exactly as the kernel
//! family was when its derived twins took over. There is now ONE registered source
//! of truth for the machine; the two-source-drift risk is gone by construction.
//!
//! This test does three things:
//!
//!   1. **Tier-0 prove-AND-catch**: the TLA+ generated from the Rust `Model` (one
//!      source) is exhaustively model-checked by the real `ty` binary. At the
//!      committed `Buggy = 0` both invariants (`NoUseAfterClose`,
//!      `ClosedImpliesNoClones`) hold over the whole bounded `MaxClones = 3` state
//!      space; at `Buggy = 1` the defect rides the always-live `DropClone` action
//!      (it closes the fd on a NON-last drop — the pre-fix bare-`i32` out-of-band
//!      close), so a subsequent `UseFd` latches a use-after-close and `ty` finds a
//!      COUNTEREXAMPLE — the proof is non-vacuous. Both spec + cfg are derived;
//!      change the model and this re-checks.
//!   2. **Executable twin**: the in-process interpreter BFSes the reachable space at
//!      `Buggy = 0` and confirms both invariants hold on every state — the executable
//!      semantics agree with the proven spec.
//!   3. **Quarantine reconciliation**: assert the hand `FdLifecycle.tla` lives ONLY
//!      under `specs/legacy/` (the superseded quarantine) and is NOT in the active
//!      checked set — so there is no second registered `FdLifecycle` SpecModule that
//!      could disagree with the derived one.
//!
//! VERIFICATION GATE (two-tier, see `aterm_spec::verify`): the in-process
//! interpreter always runs the full prove-and-catch protocol (toolchain-free);
//! the external `ty` additionally re-proves the generated TLA+ wherever the
//! Trust toolchain is installed. The interpreter + quarantine halves run
//! everywhere.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use aterm_spec::derive::{Model, fd_lifecycle_model};

/// The sibling `aterm-spec-models` `specs/` directory. aterm-spec must NOT depend on
/// aterm-spec-models (that would be a dependency cycle), so resolve by path.
fn specs_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ dir")
        .join("aterm-spec-models")
        .join("specs")
}

/// Derive spec + cfg (with `overrides`), run `ty check`, return (success, output).
fn run_ty(ty: &PathBuf, m: &Model, cfg_overrides: &[(&'static str, i64)]) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "aterm-fdlc-{}-{}-{}",
        m.name,
        std::process::id(),
        cfg_overrides
            .iter()
            .map(|(_, v)| v.to_string())
            .collect::<String>()
    ));
    std::fs::create_dir_all(&dir).expect("mk tempdir");
    let spec = dir.join(format!("{}.tla", m.name));
    let cfg = dir.join(format!("{}.cfg", m.name));
    std::fs::write(&spec, m.to_tla()).expect("write derived spec");
    std::fs::write(&cfg, m.to_cfg_with(cfg_overrides)).expect("write derived cfg");
    let out = Command::new(ty)
        .arg("check")
        .arg(&spec)
        .arg("--config")
        .arg(&cfg)
        .output()
        .expect("run ty check");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    (out.status.success(), combined)
}

#[test]
fn derived_fd_lifecycle_proves_and_catches_use_after_close() {
    // TIERED (VERIFY-1): the interpreter always runs the whole prove-and-catch
    // protocol; the escalation below re-proves it through the external `ty`
    // wherever the toolchain is installed (absent → the prominent notice, and
    // the interpreter tier above already covered the model).
    let m = fd_lifecycle_model();
    aterm_spec::interp::prove_and_catch(&m);
    let Some(ty) = aterm_spec::verify::ty_escalation("derived FdLifecycle spec") else {
        return;
    };

    // Buggy = 0 (committed): both invariants hold over the whole bounded space.
    let (ok, out) = run_ty(&ty, &m, &[]);
    assert!(
        ok,
        "derived FdLifecycle (Buggy=0) must model-check clean\n--- generated ---\n{}\n--- ty ---\n{out}",
        m.to_tla()
    );

    // Buggy = 1: DropClone closes the fd on a NON-last drop (the pre-fix bare-i32
    // out-of-band close), so a subsequent UseFd latches the use-after-close — `ty`
    // MUST report a counterexample (the proof is non-vacuous).
    let (bug_ok, bug_out) = run_ty(&ty, &m, &[("Buggy", 1)]);
    assert!(
        !bug_ok,
        "derived FdLifecycle (Buggy=1) MUST yield a counterexample (out-of-band close \
         use-after-close)\n{bug_out}"
    );
    eprintln!(
        "derived FdLifecycle: invariants proven (Buggy=0) and use-after-close caught \
         (Buggy=1 -> counterexample)."
    );
}

#[test]
fn derived_fd_lifecycle_interpreter_holds_invariants_over_reachable_states() {
    let m = fd_lifecycle_model();
    // BFS the bounded reachable state space via the executable twin at Buggy=0
    // (the OutOfBandClose guard is then unsatisfiable, so only the sound actions
    // fire); every reachable state must satisfy both invariants.
    let mut seen: std::collections::BTreeSet<Vec<(&'static str, i64)>> =
        std::collections::BTreeSet::new();
    let mut frontier: Vec<BTreeMap<&'static str, i64>> = vec![m.init_state()];
    let key = |s: &BTreeMap<&'static str, i64>| -> Vec<(&'static str, i64)> {
        s.iter().map(|(k, v)| (*k, *v)).collect()
    };
    let mut checked = 0usize;
    while let Some(state) = frontier.pop() {
        if !seen.insert(key(&state)) {
            continue;
        }
        assert!(
            m.check_invariant("NoUseAfterClose", &state),
            "NoUseAfterClose violated at {state:?}"
        );
        assert!(
            m.check_invariant("ClosedImpliesNoClones", &state),
            "ClosedImpliesNoClones violated at {state:?}"
        );
        checked += 1;
        for action in ["Clone", "UseFd", "DropClone"] {
            for next in m.successors(action, &state) {
                if !seen.contains(&key(&next)) {
                    frontier.push(next);
                }
            }
        }
    }
    assert!(
        checked > 1,
        "interpreter explored too few states ({checked})"
    );
    eprintln!(
        "derived FdLifecycle interpreter: both invariants hold over all {checked} reachable states (Buggy=0)."
    );
}

#[test]
fn derived_fd_lifecycle_action_set_is_pinned() {
    // Anti-drift defense-in-depth: pin the exact modeled action set. The closure
    // gate's obligation-3 catches an ADDED/renamed action (no resolving anchor), but
    // a behavior silently DELETED from BOTH the model and its anchors leaves nothing
    // uncovered — this assertion reddens on that case, closing the gap.
    let m = fd_lifecycle_model();
    let mut names: Vec<&str> = m.actions.iter().map(|a| a.name).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["Clone", "DropClone", "UseFd"],
        "fd_lifecycle action set drifted — update the sink.rs #[refines]/#[spec_unmodeled] anchors AND \
         this pin together"
    );
}

#[test]
fn hand_fd_lifecycle_tla_is_quarantined_not_a_second_source() {
    let dir = specs_dir();
    // The superseded hand spec must live ONLY under legacy/ (quarantined), NEVER in
    // the active top-level set — so the gate registers ONE FdLifecycle (the derived
    // model) and the two sources cannot disagree.
    assert!(
        !dir.join("FdLifecycle.tla").exists(),
        "FdLifecycle.tla must NOT be in the active specs/ set — it is superseded by the derived \
         fd_lifecycle_model() and must be quarantined to specs/legacy/ (single source of truth)."
    );
    assert!(
        dir.join("legacy").join("FdLifecycle.tla").exists(),
        "the superseded hand FdLifecycle.tla must be RETAINED under specs/legacy/ (documentary \
         provenance of the use-after-close defect)."
    );
    eprintln!(
        "reconciliation: hand FdLifecycle.tla is quarantined to legacy/; the derived \
         fd_lifecycle_model() is the single registered source."
    );
}
