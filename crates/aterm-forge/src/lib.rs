// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Third-party dependency surface survey, budget ratchet, and fork attestation (`targo --unverified forge`).

#![allow(
    clippy::disallowed_types,
    reason = "never linked into the shipped binary: the third-party survey behind targo --unverified forge"
)]

pub mod abrew_help;
pub mod attest;
pub mod blame;
pub mod budget;
pub mod check;
pub mod deny;
mod direct_vendor;
pub mod dominator;
pub mod loc;
pub mod model;
pub mod policy;
mod pristine;
pub mod provenance;
pub mod resolve;
pub mod serde_attrs;
pub mod survey;

/// Exit codes matching `aterm-verify`.
pub mod exit {
    /// Every obligation held.
    pub const PASS: u8 = 0;
    /// A policy obligation failed.
    pub const FAIL: u8 = 1;
    /// The command line was wrong.
    pub const USAGE: u8 = 2;
    /// The check could not run.
    pub const COULD_NOT_RUN: u8 = 3;
}

/// Outcome of a `forge` check run.
pub struct Outcome {
    pub ok: bool,
    pub log: String,
}

/// Scope and precision limits printed in failure diagnostics.
pub const PRECISION_NOTE: &str = "    PRECISION / SCOPE (the honest limits of forge):
      - RESOLUTION, not compilation: the graph comes from `targo tree --locked
        --offline -e normal`, which is cargo's own resolver. Feature unification
        across the workspace is therefore accounted exactly, but a package that
        resolves is not proof that a given item inside it is reachable.
      - LOC is PHYSICAL LINES over every `*.rs` under the package root,
        including that package's own tests and examples. It measures the source
        aterm would OWN on vendoring, not the code that reaches codegen.
      - `unsafe` is counted as TOKENS (`\\bunsafe\\b`), deliberately: the objc2
        crates emit nearly all of theirs from `extern_methods!`, where a block
        count reads 0 for an 83k-line crate.
      - COST IS A DOMINATOR. `dom(C) = reach(root) \\ reach(root, block C)`.
        Subtree size double-counts shared dependencies and is never reported.
      - A BUILD CELL THAT CANNOT BUILD IS SKIPPED AND NAMED, never passed.";
