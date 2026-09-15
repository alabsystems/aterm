// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for the capability publication transaction `aterm fabric on` makes.
//!
//! Tier-1 — the REAL command's effect trace run through this model — lives
//! beside the command, in `crates/aterm-link/tests/fabric_on.rs`
//! (`a_rejected_mint_publishes_nothing_and_the_trace_satisfies_the_model`),
//! because that is the crate whose integration tests can run the binary
//! (`CARGO_BIN_EXE_aterm-link`). It used to be a shell fixture around
//! `tools/fabric-enable.sh`; the script is a wrapper over the command now, and
//! the transaction it modelled is Rust.

use aterm_spec::{derive::fabric_capability_publication_model, verify};

#[test]
fn capability_publication_proves_and_catches_rejected_mint() {
    verify::prove_and_catch_scalar(
        &fabric_capability_publication_model(),
        "fabric capability publication",
    );
}

/// The Tier-1 this file points at exists and drives THIS model with the real
/// grant count — so the pointer above cannot rot into a claim about a test
/// that no longer runs the model.
#[test]
fn tier_1_runs_the_real_command_through_this_model() {
    let src = include_str!("../../aterm-link/tests/fabric_on.rs");
    assert!(
        src.contains("fabric_capability_publication_model()"),
        "crates/aterm-link/tests/fabric_on.rs no longer builds this model"
    );
    assert!(
        src.contains("ATERM_FABRIC_TRACE") && src.contains("model.fire("),
        "crates/aterm-link/tests/fabric_on.rs no longer runs the command's trace through it"
    );
    assert!(
        src.contains("*key == \"Grants\"") && src.contains(".1 = 8;"),
        "Tier-1 must run with the eight grants the command mints"
    );
}
