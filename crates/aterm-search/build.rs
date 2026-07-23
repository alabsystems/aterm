// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! COMPILE-TIME temporal gate: model-check the derived StreamingSearch lifecycle
//! as part of BUILDING aterm-search. The embedded exhaustive interpreter always
//! runs; Trust `ty` additionally checks the same obligations wherever installed.
//! A violated invariant (or a vacuous one that fails to catch the reintroduced
//! unclamped-index bug) fails `cargo build` / `cargo check` — not merely
//! `cargo test`. ONE source of truth: the model is the
//! REGISTERED `aterm_spec::derive::streaming_search_model()` — the same `Model`
//! the Tier-0 tests, the Tier-1 lockstep (tests/conformance_streaming.rs), the
//! global strict-vacuity audit, and trust-ir spec-link all check — so no layer
//! can check a different spec.

use aterm_spec::derive::streaming_search_model;
use aterm_spec::interp;
use aterm_spec::verify::{Discharge, prove_and_catch_tiered};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    gate_streaming_search();
}

fn gate_streaming_search() {
    let m = streaming_search_model();
    let wrap1 = prove_and_catch_tiered(&m, "StreamingSearch compile gate (Wrap=1)");

    // Boundary-clamping navigation is a shipped configuration, not a corner
    // left unchecked. Prove and catch the same model again with Wrap fixed off.
    let wrap0_model = interp::with_consts(&m, &[("Wrap", 0)]);
    let wrap0 = prove_and_catch_tiered(&wrap0_model, "StreamingSearch compile gate (Wrap=0)");

    assert!(
        matches!(wrap1, Discharge::Interpreter | Discharge::InterpreterAndTy)
            && matches!(wrap0, Discharge::Interpreter | Discharge::InterpreterAndTy),
        "scalar StreamingSearch models must always run in the embedded interpreter"
    );
    if wrap1 == Discharge::InterpreterAndTy && wrap0 == Discharge::InterpreterAndTy {
        println!(
            "cargo:warning=temporal gate ✓ {} proven and non-vacuous for Wrap=0/1 by the \
             embedded exhaustive interpreter and Trust ty",
            m.name
        );
    } else {
        println!(
            "cargo:warning=temporal gate ✓ {} proven and non-vacuous for Wrap=0/1 by the \
             embedded exhaustive interpreter; Trust ty did not discharge both configurations",
            m.name
        );
    }
}
