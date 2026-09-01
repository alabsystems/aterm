// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-forge` — the owned-fork production line for aterm's third-party
//! surface, behind the `cargo forge` alias.
//!
//! # Why this exists
//!
//! The shipped `aterm` binary on aarch64-apple-darwin resolves 148 third-party
//! packages carrying ~2.05M lines of Rust that aterm does not own, cannot edit,
//! and does not verify — 74% more than that on Linux. `.cargo/config.toml`
//! disables in-compilation verification (`-Ztrust-verify=off`) for exactly that
//! reason, as an explicitly temporary opt-out.
//!
//! Those two figures, and every other pinned count, are read from `measured`
//! (`src/measured.rs`) — the single place the baseline lives, so an extraction
//! that shrinks the surface costs ONE edit rather than fourteen red tests.
//!
//! Forge is the instrument for shrinking that surface on a measured, ratcheted,
//! provenance-carrying basis: it SURVEYS the graph, ATTRIBUTES each package's
//! true cost, NOTARIZES every fork against its pristine upstream, and refuses to
//! let any of those numbers regress silently.
//!
//! # What it does NOT claim
//!
//! Carving does not delete `-Ztrust-verify=off`. Those are per-target rustflags
//! applied to every compiled unit, and `targo trust` marks every build script
//! off unconditionally — so while a single third-party build script remains (26
//! do, in the macOS shipped graph), no amount of source deletion retires the
//! flag. What forge delivers is a smaller, owned, monotonically-decreasing
//! surface with `-p` handles and ratchet rows. The last mile needs
//! `-Ztrust-verify-include-dependencies` or a written build-script policy.
//!
//! # Module map
//!
//! | module | role |
//! |---|---|
//! | [`model`] | package identity, graph, cells, per-package facts |
//! | [`resolve`] | `cargo tree` → [`model::Graph`], per cell, offline |
//! | [`dominator`] | `dom(C) = reach(root) \ reach(root, block C)` — true cost |
//! | [`loc`] | LOC, unsafe TOKENS, build scripts, proc-macro flags, SPDX |
//! | [`survey`] | the inventory report, emitted (never transcribed) |
//! | [`blame`] | which first-party manifest line forces a given package/feature |
//! | [`policy`] | `vendor/forge.toml` — forks, decisions, comment-preserving |
//! | [`budget`] | `tools/forge-budget.tsv` — the lower-only ratchet |
//! | [`attest`] | provenance, license and `[patch]`-liveness obligations |
//! | [`mirror`] | Lane 1 generator: `Cargo.lock` → enforced `local-registry` |
//! | [`mirror_bundle`] | Lane 1 delivery: one deterministic, verified-before-unpack bundle |
//! | [`mirror_config`] | the shippable `[source]` fragment, split from `.cargo/config.toml` |
//! | [`check`] | the gate reporter: all of the above, no compilation |
//! | `measured` | test-only: the pinned per-cell baseline, in ONE place |

pub mod attest;
pub mod blame;
pub mod budget;
pub mod check;
pub mod dominator;
pub mod loc;
/// THE pinned measurement baseline, read by every test that asserts a real
/// number about this tree. Test-only on purpose: the shipped verbs MEASURE, and
/// a verb that consulted a hard-coded count would be reporting the answer it
/// was told instead of the one the graph has.
#[cfg(test)]
pub mod measured;
pub mod mirror;
pub mod mirror_bundle;
pub mod mirror_config;
pub mod model;
pub mod policy;
pub mod resolve;
pub mod survey;

/// Exit codes, matching `aterm-verify`'s set so one convention covers the gates.
pub mod exit {
    /// Every obligation held.
    pub const PASS: u8 = 0;
    /// A policy obligation failed — the gate is RED.
    pub const FAIL: u8 = 1;
    /// The command line was wrong.
    pub const USAGE: u8 = 2;
    /// The check could not run (missing tool, unreadable tree). Never a pass.
    pub const COULD_NOT_RUN: u8 = 3;
}

/// The house verdict shape, mirroring `aterm_census::CensusOutcome`: the log is
/// RETURNED rather than printed so each consumer routes it (xtask → stderr,
/// build gate → compile error).
pub struct Outcome {
    pub ok: bool,
    pub log: String,
}

/// The honest limits of this tool, printed verbatim in every RED diagnostic so
/// the report cannot over-claim.
pub const PRECISION_NOTE: &str = "    PRECISION / SCOPE (the honest limits of forge):
      - RESOLUTION, not compilation: the graph comes from `cargo tree --locked
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
