// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-forge`: the owned-fork production line for aterm's third-party
//! surface, behind the `targo --unverified forge` alias.
//!
//! # Why this exists
//!
//! The shipped `aterm` binary resolves many packages aterm did not write
//! (counts in `measured::MAC_ARM` / `measured::LINUX`; the same-owner astream
//! submodule is excluded via `provenance::is_first_party`). For that surface
//! every build lane disables in-compilation verification (`-Ztrust-verify=off`,
//! appended by `targo --unverified`; `.cargo/config.toml` carried it until
//! 2026-10-07) as a temporary opt-out, and `targo trust` switches every
//! dependency unit off itself.
//!
//! Forge shrinks it on a measured, ratcheted, provenance-carrying basis: it
//! surveys the graph, attributes each package's true cost, notarizes every fork
//! against its pristine upstream, and refuses silent regressions. Pinned counts
//! live only in `measured`; prose names the constant rather than copying
//! digits.
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
//! | [`loc`] | LOC, unsafe TOKENS, build scripts, proc-macro flags, SPDX |
//! | [`survey`] | the inventory report, emitted (never transcribed) |
//! | [`blame`] | which first-party manifest line forces a given package/feature |
//! | [`policy`] | `vendor/forge.toml` — the fork ledger, hand-edited, held to the tree by `check` |
//! | [`budget`] | `tools/forge-budget.tsv` — the lower-only ratchet |
//! | [`attest`] | provenance, license and `[patch]`-liveness obligations |
//! | [`check`] | the gate reporter: all of the above, no compilation |
//! | `measured` | test-only: the pinned per-cell baseline, in ONE place |

pub mod attest;
pub mod blame;
pub mod budget;
pub mod check;
mod direct_vendor;
pub mod dominator;
pub mod loc;
/// The pinned measurement baseline, read by every test that asserts a real
/// number about this tree. Test-only: shipped verbs measure, never consult it.
#[cfg(test)]
pub mod measured;
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::{Path, PathBuf};

    /// The workspace root (aterm-forge sits two levels under it).
    pub(crate) fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/aterm-forge sits two levels under the workspace root")
            .to_path_buf()
    }
}
pub mod model;
pub mod policy;
pub mod provenance;
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
/// returned rather than printed so each consumer routes it (xtask → stderr,
/// build gate → compile error).
pub struct Outcome {
    pub ok: bool,
    pub log: String,
}

/// The honest limits of this tool, printed verbatim in every RED diagnostic so
/// the report cannot over-claim.
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
