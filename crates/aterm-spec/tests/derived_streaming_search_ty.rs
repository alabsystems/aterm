// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Tier-0 + registry/interpreter locks for the DERIVED StreamingSearch model.
//!
//! `streaming_search_model()` (machine `StreamingSearch`) is the drift-free twin
//! of `aterm-search::streaming::StreamingSearch`. It SUPERSEDES the hand-written
//! `StreamingSearch.tla` the module docs referenced — a spec that was never
//! committed, so there is nothing to quarantine; the derived model is the spec of
//! record (if the missing hand `.tla` ever surfaces it goes to
//! `aterm-spec-models/specs/legacy/` per the Kernel/FdLifecycle convention).
//!
//! This file:
//!   1. **Tier-0 prove-AND-catch** (tiered, VERIFY-1): the interpreter proves all
//!      five invariants over the whole bounded reachable space at `Buggy = 0` and
//!      finds the counterexample at `Buggy = 1` (the dropped invalidation clamp —
//!      the pre-#7472/#7244 index-out-of-range class); `ty` additionally re-proves
//!      the generated TLA+ wherever installed.
//!   2. **Wrap = 0 variant**: the same proof with wraparound navigation disabled
//!      (Next/Prev clamp at the boundary instead of cycling).
//!   3. **Registry lock**: the machine is enrolled in `model_registry()` so the
//!      global strict-vacuity audit, verifier ledger, and trust-ir spec-link all
//!      see it (the aterm-gui `spec_xref_closure` gate's seam).
//!   4. **Action-set pin** (anti-drift, the fd_lifecycle precedent).
//!   5. **Executable-twin spot-walks**: `m.fire` sequences reproducing the
//!      engine's documented lifecycle (scan/auto-complete, capacity, navigation
//!      wrap + clamp, invalidation to NoResults, revival via Add, cancel).
//!
//! Tier-1 (lockstep against the REAL engine) lives in
//! `aterm-search/tests/conformance_streaming.rs`; the compile-time gate in
//! `aterm-search/build.rs`.

use aterm_spec::derive::streaming_search_model;
use aterm_spec::{interp, verify};

/// Tier-0: proven at the committed `Buggy = 0`, caught at `Buggy = 1`.
#[test]
fn derived_streaming_search_proves_and_catches_unclamped_index() {
    verify::prove_and_catch_tiered(
        &streaming_search_model(),
        "derived StreamingSearch spec (invalidation clamp)",
    );
}

/// Tier-0 at `Wrap = 0`: with boundary-clamping navigation the invariants still
/// hold over the whole reachable space (interpreter always; `ty` where installed).
#[test]
fn derived_streaming_search_holds_with_wrap_disabled() {
    let m = interp::with_consts(&streaming_search_model(), &[("Wrap", 0)]);
    match interp::bmc(&m) {
        Ok(n) => eprintln!("StreamingSearch (Wrap=0): proven over {n} states (interpreter)."),
        Err((st, inv)) => panic!("StreamingSearch (Wrap=0): invariant `{inv}` VIOLATED at {st:?}"),
    }
    let base = streaming_search_model();
    let Some(ty) = verify::ty_escalation("derived StreamingSearch (Wrap=0) spec") else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("aterm-ss-wrap0-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mk tempdir");
    let spec = dir.join("StreamingSearch.tla");
    let cfg = dir.join("StreamingSearch.cfg");
    std::fs::write(&spec, base.to_tla()).expect("write derived spec");
    std::fs::write(&cfg, base.to_cfg_with(&[("Wrap", 0)])).expect("write derived cfg");
    let out = std::process::Command::new(&ty)
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
    assert!(
        out.status.success(),
        "derived StreamingSearch (Wrap=0) must model-check clean in ty — TIER DISAGREEMENT \
         (interpreter proved it above)\n{combined}"
    );
    eprintln!("StreamingSearch (Wrap=0): additionally model-checked clean by ty.");
}

/// The machine must participate in the repository-wide spec-link and
/// strict-vacuity closure, not only its direct Tier-0/Tier-1 tests. Regression
/// lock for the registry seam consumed by aterm-gui's closure gate.
#[test]
fn streaming_search_is_registered_for_global_verification() {
    let registered: std::collections::BTreeSet<_> = aterm_spec::xref::model_registry()
        .into_iter()
        .map(|model| model.name)
        .collect();
    assert!(
        registered.contains("StreamingSearch"),
        "StreamingSearch must resolve through the global spec↔source registry"
    );
}

/// Anti-drift defense-in-depth (the fd_lifecycle precedent): pin the exact
/// modeled action set. The closure gate catches an added/renamed action (no
/// resolving anchor), but a behavior deleted from BOTH the model and its anchors
/// leaves nothing uncovered — this pin reddens on that case.
#[test]
fn derived_streaming_search_action_set_is_pinned() {
    let m = streaming_search_model();
    let mut names: Vec<&str> = m.actions.iter().map(|a| a.name).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "Add",
            "Cancel",
            "Invalidate",
            "NextMatch",
            "PrevMatch",
            "Reflow",
            "ScanHit",
            "ScanMiss",
            "Start"
        ],
        "StreamingSearch action set drifted — update the operations.rs \
         #[refines] anchors AND this pin together"
    );
}

/// Executable-twin spot-walks: drive the SAME model through the engine's
/// documented lifecycle and check each invariant-bearing observable.
#[test]
fn streaming_search_interpreter_walks_the_engine_lifecycle() {
    let m = streaming_search_model();
    let inv_ok = |st: &std::collections::BTreeMap<&'static str, i64>| {
        for inv in [
            "CurrentIndexValid",
            "MemoryBounded",
            "TotalMatchesConsistent",
            "ScanProgressConsistent",
            "TerminalShape",
        ] {
            assert!(m.check_invariant(inv, st), "{inv} violated at {st:?}");
        }
    };

    // Scan 3 hit rows: MaxResults=2 stores two, the third COUNTS but does not
    // STORE, and the last row auto-completes to HasResults with cur = 1.
    let mut st = m.init_state();
    assert!(m.fire("Start", &mut st));
    assert_eq!(st["scanp"], 1, "scan starts at row 0 (scanp = row + 1)");
    assert!(m.fire("ScanHit", &mut st));
    assert!(m.fire("ScanHit", &mut st));
    assert_eq!((st["stored"], st["total"]), (2, 2));
    assert!(
        m.fire("ScanHit", &mut st),
        "last-row hit folds the auto-complete"
    );
    assert_eq!(
        (
            st["state"],
            st["scanp"],
            st["stored"],
            st["total"],
            st["cur"]
        ),
        (2, 0, 2, 3, 1),
        "at capacity the hit counts-not-stores; completion selects cur = 1"
    );
    inv_ok(&st);

    // Navigation: wrap forward past the end back to 1; wrap backward to stored.
    assert!(m.fire("NextMatch", &mut st));
    assert_eq!(st["cur"], 2);
    assert!(m.fire("NextMatch", &mut st));
    assert_eq!(st["cur"], 1, "Wrap=1 cycles past the end");
    assert!(m.fire("PrevMatch", &mut st));
    assert_eq!(st["cur"], 2, "Wrap=1 cycles backward to stored");
    inv_ok(&st);

    // Invalidate down to empty: clamp keeps cur valid, empty => NoResults/cur=0.
    assert!(m.fire("Invalidate", &mut st));
    assert_eq!((st["state"], st["stored"], st["cur"]), (2, 1, 1), "clamped");
    assert!(m.fire("Invalidate", &mut st));
    assert_eq!(
        (st["state"], st["stored"], st["cur"]),
        (3, 0, 0),
        "NoResults"
    );
    inv_ok(&st);

    // NoResults revives through Add (content_added finds a fresh match).
    assert!(m.fire("Add", &mut st));
    assert_eq!((st["state"], st["stored"], st["cur"]), (2, 1, 1));
    inv_ok(&st);

    // Reflow restarts the scan; Cancel returns to Idle; nothing fires from Idle
    // except Start.
    assert!(m.fire("Reflow", &mut st));
    assert_eq!((st["state"], st["scanp"], st["stored"]), (1, 1, 0));
    assert!(m.fire("Cancel", &mut st));
    assert_eq!(st, m.init_state(), "Cancel restores the initial state");
    for dead in [
        "ScanHit",
        "ScanMiss",
        "NextMatch",
        "PrevMatch",
        "Add",
        "Invalidate",
        "Reflow",
        "Cancel",
    ] {
        assert!(
            !m.action_enabled(dead, &st),
            "{dead} must be disabled in Idle"
        );
    }
    inv_ok(&st);

    // All-miss scan lands in NoResults with nothing stored.
    assert!(m.fire("Start", &mut st));
    for _ in 0..3 {
        assert!(m.fire("ScanMiss", &mut st));
    }
    assert_eq!(
        (
            st["state"],
            st["scanp"],
            st["stored"],
            st["total"],
            st["cur"]
        ),
        (3, 0, 0, 0, 0)
    );
    assert!(
        !m.action_enabled("NextMatch", &st) && !m.action_enabled("PrevMatch", &st),
        "navigation is disabled in NoResults"
    );
    inv_ok(&st);

    // Wrap = 0: navigation clamps at the boundaries instead of cycling.
    let m0 = interp::with_consts(&m, &[("Wrap", 0)]);
    let mut st = m0.init_state();
    assert!(m0.fire("Start", &mut st));
    assert!(m0.fire("ScanHit", &mut st));
    assert!(m0.fire("ScanHit", &mut st));
    assert!(m0.fire("ScanMiss", &mut st));
    assert_eq!((st["state"], st["stored"], st["cur"]), (2, 2, 1));
    assert!(m0.fire("PrevMatch", &mut st));
    assert_eq!(st["cur"], 1, "Wrap=0 clamps at the start");
    assert!(m0.fire("NextMatch", &mut st));
    assert!(m0.fire("NextMatch", &mut st));
    assert_eq!(st["cur"], 2, "Wrap=0 clamps at the end");
}
