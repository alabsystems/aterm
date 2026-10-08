// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Compile-time temporal gate for the scrollback-offload detach window and the
//! unified retention limit. The embedded exhaustive interpreter runs while
//! building aterm-grid, and Trust `ty` rechecks the same obligations where
//! installed; a violated or vacuous invariant fails the build. The models are
//! `include!`d from the spec files shared with the conformance tests.

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

/// Gate for the unified total retention limit: `TotalBounded` proves at
/// `Buggy=0` and is caught at `Buggy=1`.
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

/// A discharged gate prints nothing; a violated or vacuous invariant already
/// panics inside `prove_and_catch_scalar`.
///
/// Both models are scalar, so the interpreter always discharges them and a
/// missing `ty` costs only the escalation tier. `TyOnly` (interpreter did not
/// run) fails the build: the gate must not depend on the toolchain installed.
fn report(name: &str, covered: Covered) {
    match covered {
        Covered::Interpreter | Covered::InterpreterAndTy => {}
        Covered::TyOnly => panic!("scalar {name} model did not run in the interpreter"),
    }
}
