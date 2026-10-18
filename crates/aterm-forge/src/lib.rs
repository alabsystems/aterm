// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-forge`: the owned-fork production line for aterm's third-party surface, behind the
//! `targo --unverified forge` alias.
//!
//! The shipped `aterm` binary resolves many packages aterm did not write
//! (counts per cell in `tools/forge-budget.tsv`; the same-owner astream
//! submodule is excluded via `provenance::is_first_party`). For that surface
//! every build lane disables in-compilation verification (`-Ztrust-verify=off`,
//! appended by `targo --unverified`; `.cargo/config.toml` carried it until
//! 2026-10-07) as a temporary opt-out, and `targo trust` switches every
//! dependency unit off itself.
//!
//! Forge shrinks it on a measured, ratcheted, provenance-carrying basis: it
//! surveys the graph, attributes each package's true cost, notarizes every fork
//! against its pristine upstream, and refuses silent regressions. Pinned counts
//! live only in the ratchet, `tools/forge-budget.tsv`; prose names the row
//! rather than copying digits.
//!
//! # What it does not claim
//!
//! Carving does not delete `-Ztrust-verify=off`: the build lane applies it to
//! every unit, and `targo trust` marks every build script off, so while any
//! third-party build script remains the flag stays. Forge delivers a smaller,
//! owned, decreasing surface with `-p` handles and ratchet rows; the last mile
//! needs `-Ztrust-verify-include-dependencies` or a build-script policy.
//!
//! # Module map
//!
//! | module | role |
//! |---|---|
//! | [`model`] | package identity, graph, cells, per-package facts |
//! | [`resolve`] | `cargo tree` → [`model::Graph`], per cell, offline |
//! | [`dominator`] | `dom(C) = reach(root) \ reach(root, block C)` — true cost |
//! | [`loc`] | LOC, unsafe tokens, build scripts, proc-macro flags, SPDX |
//! | [`survey`] | the inventory report, emitted (never transcribed) |
//! | [`blame`] | which first-party manifest line forces a given package/feature |
//! | [`policy`] | `vendor/forge.toml` — the hand-edited fork ledger, held to the tree by `check` |
//! | [`budget`] | `tools/forge-budget.tsv` — the lower-only ratchet |
//! | [`attest`] | provenance, license and `[patch]`-liveness obligations |
//! | [`check`] | the `forge` check: all of the above, no compilation |
//! | [`serde_attrs`] | every `#[serde(...)]` key and form: the first-party derive's closed set |
#![allow(
    clippy::disallowed_types,
    reason = "never linked into the shipped binary: the third-party survey behind targo --unverified forge"
)]

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
pub mod provenance;
pub mod resolve;
pub mod serde_attrs;
pub mod survey;

/// Exit codes, matching `aterm-verify`'s set so one convention covers every check.
pub mod exit {
    /// Every obligation held.
    pub const PASS: u8 = 0;
    /// A policy obligation failed.
    pub const FAIL: u8 = 1;
    /// The command line was wrong.
    pub const USAGE: u8 = 2;
    /// The check could not run (missing tool, unreadable tree). Never a pass.
    pub const COULD_NOT_RUN: u8 = 3;
}

/// The verdict shape, mirroring `aterm_census::CensusOutcome`: the log is returned rather than
/// printed so each consumer routes it.
pub struct Outcome {
    pub ok: bool,
    pub log: String,
}

/// The limits of this tool, printed verbatim in every red diagnostic so the report cannot
/// over-claim.
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
