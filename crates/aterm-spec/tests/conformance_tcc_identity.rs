// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for the exclusivity of the app's TCC identity.
//!
//! Tier-1 — the REAL `aterm_containment::consent::classify_claimants` driven
//! through this model — lives beside that code, in
//! `crates/aterm-containment/tests/conformance_claimants.rs`, because that is
//! the crate whose tests can call the shipping function. A `ty`-green model with
//! no such bind proves a property of the description, not of the program
//! (AGENTS.md, step 3), and this class already recurred once after
//! `tools/dev-app.sh` "fixed" it: that fix stopped NEW claimants being minted
//! and nothing detected the ones already on disk.

use aterm_spec::{derive::tcc_identity_claim_exclusivity_model, verify};

#[test]
fn identity_exclusivity_proves_and_catches_a_silent_conflict() {
    verify::prove_and_catch_scalar(
        &tcc_identity_claim_exclusivity_model(),
        "tcc identity claim exclusivity",
    );
}

/// The Tier-1 this file points at exists and drives THIS model with the real
/// census — so the pointer above cannot rot into a claim about a test that no
/// longer runs the model.
#[test]
fn tier_1_runs_the_real_census_through_this_model() {
    let src = include_str!("../../aterm-containment/tests/conformance_claimants.rs");
    assert!(
        src.contains("tcc_identity_claim_exclusivity_model()"),
        "the containment conformance no longer builds this model"
    );
    assert!(
        src.contains("classify_claimants("),
        "Tier-1 must drive the SHIPPING census, not a restatement of it"
    );
    assert!(
        src.contains("model.fire("),
        "Tier-1 must step the model with what the real census decided"
    );
}
