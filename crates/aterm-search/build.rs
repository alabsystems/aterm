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
use aterm_spec::verify::{Covered, prove_and_catch_scalar};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    gate_streaming_search();
}

fn gate_streaming_search() {
    let m = streaming_search_model();
    let wrap1 = prove_and_catch_scalar(&m, "StreamingSearch compile gate (Wrap=1)");

    // Boundary-clamping navigation is a shipped configuration, not a corner
    // left unchecked. Prove and catch the same model again with Wrap fixed off.
    let wrap0_model = interp::with_consts(&m, &[("Wrap", 0)]);
    let wrap0 = prove_and_catch_scalar(&wrap0_model, "StreamingSearch compile gate (Wrap=0)");

    // `prove_and_catch_scalar` asserts the scalar shape, so the interpreter tier
    // ran for both configurations; `Covered::TyOnly` would mean it did not, and
    // this gate must never report a green it did not earn in-process.
    assert!(
        wrap1 != Covered::TyOnly && wrap0 != Covered::TyOnly,
        "scalar StreamingSearch models must always run in the embedded interpreter"
    );
    if wrap1 == Covered::InterpreterAndTy && wrap0 == Covered::InterpreterAndTy {
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
