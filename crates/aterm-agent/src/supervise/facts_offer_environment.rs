// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The steps of `SelfClaimFact` (`aterm_models_harness::self_claim_fact_model`) that no function
//! here performs. The account's own bind where the real code runs: each read on
//! `SelfClaimFacts::pick` and `SelfClaimFacts::offer`, each ack on `SelfClaimFacts::ack`, each
//! close on `SelfClaimFacts::observe`, each projected by the Tier-1 bind
//! (`tests/conformance_self_claim_facts.rs`). Proof and test builds only.

/// The call's, the act's and the clock's steps, and the mutants of `SelfClaimFact`.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SelfClaimFact",
        action = "Open1",
        reason = "Episode 1's call going quiet past the jobs engine's no-progress verdict: the call's, which the next reading names (JobsBoard::facts). Its counts restart: a new run of the same key. The Tier-1 bind names it in every reading after."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SelfClaimFact",
        action = "Open2",
        reason = "Episode 2's call going quiet: as Open1."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SelfClaimFact",
        action = "Lapse1",
        reason = "Episode 1's lease running out (OFFER_MS) once its holder's act is over: the clock's. The Tier-1 bind moves its clock to the lease's end exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SelfClaimFact",
        action = "Lapse2",
        reason = "Episode 2's lease running out: as Lapse1."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SelfClaimFact",
        action = "R1Drops",
        reason = "Reader 1's act going without its fact (stale at the turn request, not fitting whole, no turn started): codex-alab's, which asks nothing of aterm. The Tier-1 bind does nothing there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SelfClaimFact",
        action = "R2Drops",
        reason = "Reader 2's act going without its fact: as R1Drops."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SelfClaimFact",
        action = "R1Starts",
        reason = "Reader 1's act typed with its rider and its turn started: codex-alab's. The Tier-1 bind does nothing there; the model counts the ride."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SelfClaimFact",
        action = "R2Starts",
        reason = "Reader 2's act typed and started: as R1Starts."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SelfClaimFact",
        action = "R1LosesAck",
        reason = "Reader 1's ack lost or failed (its call timed out, the claim ended): codex-alab's, which reaches aterm with nothing. The Tier-1 bind does nothing there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SelfClaimFact",
        action = "R2LosesAck",
        reason = "Reader 2's ack lost: as R1LosesAck."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SelfClaimFact",
        action = "SlotSteal",
        reason = "Buggy=1 negative control only; one offer slot for the whole account, the first draft's, so a second reader's offer replaces the first's (the Tier-1 bind plants OneOfferSlot in SelfClaimFacts::offer)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SelfClaimFact",
        action = "R1AcksEarly",
        reason = "Buggy=2 negative control only; an ack before the act's turn started, telling an episode whose fact never rode: codex-alab's order, which aterm cannot see"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SelfClaimFact",
        action = "R2ReadsLeased1",
        reason = "Buggy=3 negative control only; a read that ignores another reader's live lease, so both readers' acts carry one episode"
    )
)]
#[doc(hidden)]
pub fn self_claim_fact_environment() {}
