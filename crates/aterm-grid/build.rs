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
use aterm_spec::verify::{Covered, prove_and_catch_scalar};

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
    report(
        m.name,
        prove_and_catch_scalar(&m, "UnifiedRetentionLimit compile gate"),
    );
}

fn gate_offload_window() {
    let m = offload_window_model();
    report(
        m.name,
        prove_and_catch_scalar(&m, "ScrollbackOffloadWindow compile gate"),
    );
}

/// Announce a discharged gate as a `cargo:warning` line — the only way a build
/// script can say "this was checked" out loud.
///
/// Note what is NOT here: a "ty missing, gate skipped" outcome. Both models are
/// scalar, so `prove_and_catch_scalar` guarantees the embedded interpreter
/// discharged them exhaustively; a missing `ty` costs the escalation tier only,
/// which is why the `Interpreter` arm prints and succeeds. `TyOnly` would mean
/// the interpreter did NOT run — impossible for a scalar model, and a build
/// failure rather than a print if a future edit makes one of these models
/// function-valued, because a compile gate that quietly degrades to "checked
/// only on machines that happen to have the toolchain" is the exact failure this
/// build script exists to prevent.
fn report(name: &str, covered: Covered) {
    match covered {
        Covered::Interpreter => println!(
            "cargo:warning=temporal gate ✓ {name} proven and non-vacuous by the embedded \
             exhaustive interpreter; Trust ty escalation was not installed"
        ),
        Covered::InterpreterAndTy => println!(
            "cargo:warning=temporal gate ✓ {name} proven and non-vacuous by the embedded \
             exhaustive interpreter and Trust ty"
        ),
        Covered::TyOnly => panic!("scalar {name} model did not run in the interpreter"),
    }
}
