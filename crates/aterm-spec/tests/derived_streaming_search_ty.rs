// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Tier-0 + interpreter locks for the DERIVED StreamingSearch model.
//!
//! `streaming_search_model()` (machine `StreamingSearch`) is the drift-free twin
//! of `aterm-search::streaming::StreamingSearch`. It SUPERSEDES the hand-written
//! `StreamingSearch.tla` the module docs referenced — a spec that was never
//! committed, so there is nothing to quarantine; the derived model is the spec of
//! record.
//!
//! This file:
//!   1. **Tier-0 prove-AND-catch** (tiered, VERIFY-1): the interpreter proves all
//!      five invariants over the whole bounded reachable space at `Buggy = 0` and
//!      finds a counterexample at `Buggy = 1` (one slip per law, the dropped
//!      invalidation clamp — the pre-#7472/#7244 index-out-of-range class — among
//!      them; `streaming_search_each_law_has_its_own_slip` pins the others); `ty`
//!      additionally re-proves the generated TLA+ wherever installed.
//!   2. **Wrap = 0 variant**: the same prove-and-catch with wraparound navigation
//!      disabled (Next/Prev clamp at the boundary instead of cycling).
//!   3. **Executable-twin spot-walks**: `m.fire` sequences reproducing the
//!      engine's documented lifecycle (scan/auto-complete, capacity, navigation
//!      wrap + clamp, invalidation to NoResults, revival via Add, cancel). The
//!      walk fires every one of the nine actions, so an action deleted from the
//!      model fails it; an added one has no resolving anchor in `operations.rs`
//!      and fails aterm-gui's `spec_xref_closure`.
//!
//! Tier-1 (lockstep against the REAL engine) lives in
//! `aterm-search/tests/conformance_streaming.rs`.

use aterm_spec::derive::streaming_search_model;
use aterm_spec::{interp, verify};

/// Tier-0 at `Wrap = 0`: boundary-clamping navigation is a shipped setting, so
/// it is proven and caught like the default.
#[test]
fn derived_streaming_search_proves_and_catches_with_wrap_disabled() {
    let m = interp::with_consts(&streaming_search_model(), &[("Wrap", 0)]);
    verify::prove_and_catch_scalar(&m, "derived StreamingSearch spec (Wrap=0)");
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

/// Each law is caught on its OWN slip, not only through the dropped clamp: the
/// capacity test read as `>`, `complete_search` without its `-1` reset, and
/// `content_added` storing a match it never counts.
#[test]
fn streaming_search_each_law_has_its_own_slip() {
    let buggy = interp::with_buggy(&streaming_search_model(), 1);

    let mut scanned = buggy.init_state();
    for action in ["Start", "ScanHit", "ScanHit", "ScanHit"] {
        assert!(buggy.fire(action, &mut scanned), "{action}: {scanned:?}");
    }
    assert_eq!(
        scanned["stored"], 3,
        "the capacity hit stored past the bound"
    );
    assert!(!buggy.check_invariant("MemoryBounded", &scanned));
    assert_eq!(
        (scanned["state"], scanned["scanp"]),
        (2, 4),
        "a finished search still reports its row count as progress"
    );
    assert!(!buggy.check_invariant("ScanProgressConsistent", &scanned));

    let mut added = buggy.init_state();
    for action in ["Start", "ScanMiss", "ScanMiss", "ScanMiss", "Add"] {
        assert!(buggy.fire(action, &mut added), "{action}: {added:?}");
    }
    assert_eq!((added["stored"], added["total"]), (1, 0));
    assert!(!buggy.check_invariant("TotalMatchesConsistent", &added));
}
