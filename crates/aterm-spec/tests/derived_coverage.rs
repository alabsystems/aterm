// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Derived-model coverage gate (discoverability + anti-regression).
//!
//! The kernel-family TLA+ specs are the load-bearing correctness model of aterm's
//! buffer kernel. This test asserts each one now has a DERIVED, drift-free twin (a
//! Rust `Model` from which the spec is generated), so the hand-written `.tla` can
//! never silently diverge from the code's intended semantics. If a derived model
//! is removed or renamed, this fails — flagging that a kernel-family property
//! regressed to drift-prone hand-maintenance.
//!
//! It also prints an inventory (which specs are derived vs hand-written-only), so
//! an agent reading test output learns where the derived-model feature applies and
//! which specs are still candidates for deriving (see `AGENTS.md`).

use aterm_spec::derive::{
    cursor_model, evict_full_model, kernel_model, ring_model, snapshot_model, subscribe_model,
    transact_model,
};

/// The canonical kernel family, mapped to the derived model that supersedes each
/// hand-written spec. `Evict.tla` is the bounded ring, derived as `ring_model`
/// (module `Ring`). After TRUST_NATIVE_TLA Phase 1 these hand-written `.tla` are
/// quarantined under `aterm-spec-models/specs/legacy/` (no longer in the checked
/// set); this test asserts their DERIVED twins still exist, so the one remaining
/// source of truth for the kernel family cannot silently regress.
const KERNEL_FAMILY: &[(&str, &str)] = &[
    ("Kernel.tla", "Kernel"),
    ("Subscribe.tla", "Subscribe"),
    ("Snapshot.tla", "Snapshot"),
    ("Transact.tla", "Transact"),
    ("Evict.tla", "Ring"),
];

#[test]
fn every_kernel_family_spec_has_a_derived_twin() {
    let derived: Vec<&str> = vec![
        ring_model().name,
        kernel_model().name,
        subscribe_model().name,
        snapshot_model().name,
        transact_model().name,
        cursor_model().name,
        evict_full_model().name,
    ];

    for (spec, model_name) in KERNEL_FAMILY {
        assert!(
            derived.contains(model_name),
            "kernel-family spec {spec} has no derived twin (expected a `Model` named {model_name:?}); \
             derived models present: {derived:?}. Add one (see AGENTS.md) so the spec cannot drift."
        );
    }

    eprintln!("derived-model coverage:");
    eprintln!("  derived models ({}): {derived:?}", derived.len());
    for (spec, model_name) in KERNEL_FAMILY {
        eprintln!("  {spec:<14} -> derived as `{model_name}`");
    }
    // Evict.tla is covered by TWO derived models: the scalar `Ring` (LenBounded)
    // and the function-valued `EvictFull` (EvictOldestContiguous over live[]).
    eprintln!(
        "  Evict.tla      -> also derived as `EvictFull` (EvictOldestContiguous, function-valued)"
    );
    // NEW machines beyond the kernel family (no hand-written .tla twin — derived
    // from the start). TierResidency is the temporal buffer's spill-not-forget
    // property (HIERARCHICAL_SESSIONS.md Addendum B, B.8.2); proven + negative-
    // controlled in derived_ring_ty.rs.
    eprintln!(
        "  (new) temporal-buffer  -> `TierResidency` (NoSilentLoss, function-valued, spill-not-forget)"
    );
    // The session-bug specs remain hand-written (some are bit-precise / protocol
    // shapes that route to other engines); they are candidates for deriving where
    // they are scalar state machines. Surfaced honestly, not asserted.
    eprintln!(
        "  hand-written-only (candidates to derive where scalar): \
         AltScreen, ForkExec, GpuEncode, PathConfine, Sandbox, WriteAll"
    );
}
