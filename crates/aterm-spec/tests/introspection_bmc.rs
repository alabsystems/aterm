// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Tier-1 (interpreter-driven) bounded model check of the introspection /
//! recursive-stacking safety models — the SAME `Model`s the `ty` binary checks at
//! Tier-0 (`derived_ring_ty.rs`), here driven through the embedded executable
//! interpreter so the verification runs WITHOUT the external `ty` toolchain.
//!
//! This is genuine model-checking, not example-testing: [`bmc`] enumerates the
//! ENTIRE bounded reachable state space by BFS over `Model::successors` (which
//! fans out the nondeterministic `\in lo..hi` picks exactly as `ty`'s existential
//! search does) and asserts every invariant at every reachable state. Each model
//! uses the `Buggy` convention: the invariant must HOLD across the whole space at
//! `Buggy = 0` and a counterexample state must be REACHABLE at `Buggy = 1` — so the
//! property is both true and non-trivial (it genuinely catches the audit defect).
//!
//! Findings modelled: M1 dispatch completeness, M2 relay-teardown liveness,
//! S1 proxy-registry leak. See docs/TRUST-introspection-audit-detection.md.

// The 7 introspection models are iterated via `harness::instances()`, not named here.
// The checker itself is the PROMOTED shared interpreter tier (aterm_spec::interp) —
// the same functions `verify::{check_model,prove_and_catch,deadlock_free_…}_tiered`
// discharge every derived-model gate with (VERIFY-1).
use aterm_spec::interp::{find_deadlock, prove_and_catch as proves_and_catches, with_buggy};

/// The introspection property suite is the shared instance table; this binary is
/// the Tier-1 (interpreter-BMC) driver over it (the Tier-0 `ty` driver lives in
/// `derived_ring_ty.rs`). Adding a property = one row in `harness::instances()`.
#[path = "common/harness.rs"]
mod harness;

/// THE UMBRELLA (Tier-1, no toolchain): every property-combinator instance is
/// verified by the interpreter — a `Safety` invariant exhaustively over the bounded
/// reachable space (`proves_and_catches`), a `Liveness` instance via the
/// no-successor wedge check (`find_deadlock`, deadlock-free@Buggy=0 / wedge@Buggy=1).
/// Iterated from the ONE shared table; a new property adds a row there, not a fn here.
#[test]
fn property_classes_prove_and_catch_under_bmc() {
    for inst in harness::instances() {
        match inst.class {
            harness::Class::Safety => proves_and_catches(&inst.model),
            harness::Class::Liveness { is_final } => {
                assert!(
                    find_deadlock(&with_buggy(&inst.model, 0), is_final).is_none(),
                    "{} Buggy=0 must be deadlock-free",
                    inst.model.name
                );
                let wedge = find_deadlock(&with_buggy(&inst.model, 1), is_final);
                assert!(
                    wedge.is_some(),
                    "{} Buggy=1 must reach a wedge",
                    inst.model.name
                );
                eprintln!(
                    "{}: wedge caught at {:?} (Buggy=1).",
                    inst.model.name,
                    wedge.unwrap()
                );
            }
        }
    }
}
