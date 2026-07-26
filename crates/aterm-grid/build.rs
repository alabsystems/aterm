// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! COMPILE-TIME temporal gate for the scrollback-offload detach-window
//! protocol. The embedded exhaustive interpreter always runs while BUILDING
//! aterm-grid; Trust `ty` additionally checks the same obligations wherever it
//! is installed. A violated invariant (or a vacuous one that fails to catch the
//! reintroduced bug) fails `cargo build` / `cargo check`, not merely tests.
//! ONE source of truth: the model is `include!`d from
//! `offload_window_spec.rs`, shared with the conformance test, so it cannot drift.

use aterm_spec::derive::Model;
use aterm_spec::ty_model;
use aterm_spec::verify::{Discharge, prove_and_catch_tiered};

include!("offload_window_spec.rs");
include!("retention_limit_spec.rs");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=offload_window_spec.rs");
    println!("cargo:rerun-if-changed=retention_limit_spec.rs");
    gate_offload_window();
    gate_retention_limit();
}

/// COMPILE-TIME gate for the unified total retention limit (audit E1): the
/// `TotalBounded` invariant must PROVE at `Buggy=0` and be CAUGHT at `Buggy=1`
/// (the pre-unification store-cap-equals-limit split), so the one-total-limit
/// contract can neither regress nor go vacuous.
fn gate_retention_limit() {
    let m = retention_limit_model();
    let discharge = prove_and_catch_tiered(&m, "UnifiedRetentionLimit compile gate");

    match discharge {
        Discharge::Interpreter => println!(
            "cargo:warning=temporal gate ✓ {} proven and non-vacuous by the embedded exhaustive \
             interpreter; Trust ty escalation was not installed",
            m.name
        ),
        Discharge::InterpreterAndTy => println!(
            "cargo:warning=temporal gate ✓ {} proven and non-vacuous by the embedded exhaustive \
             interpreter and Trust ty",
            m.name
        ),
        Discharge::TyOnly | Discharge::NotRun => {
            panic!("scalar UnifiedRetentionLimit model did not run in the interpreter")
        }
    }
}

fn gate_offload_window() {
    let m = offload_window_model();
    let discharge = prove_and_catch_tiered(&m, "ScrollbackOffloadWindow compile gate");

    match discharge {
        Discharge::Interpreter => println!(
            "cargo:warning=temporal gate ✓ {} proven and non-vacuous by the embedded exhaustive \
             interpreter; Trust ty escalation was not installed",
            m.name
        ),
        Discharge::InterpreterAndTy => println!(
            "cargo:warning=temporal gate ✓ {} proven and non-vacuous by the embedded exhaustive \
             interpreter and Trust ty",
            m.name
        ),
        Discharge::TyOnly | Discharge::NotRun => {
            panic!("scalar ScrollbackOffloadWindow model did not run in the interpreter")
        }
    }
}
