// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A PROVEN SELF-CLAIM'S FACTS ACCOUNT (gap 23; `docs/DESIGN-system-awareness-2026-10-16.md`
//! §14.4, "A self-supervising agent's acts"): aterm's measured fact rides an act codex-alab types
//! anyway, read at the act (`harness fact`) and acknowledged once the act's turn started (`harness
//! fact told`), each episode told once.
//!
//! The model (`SelfClaimFact`): two episodes (two calls at no progress, the first quiet longer)
//! and two readers (two root threads of one claim). A reader reads at an act (`RrReadsE`): aterm
//! offers it the first open, untold episode whose offer is empty, its own, or lapsed, and leases
//! the episode to it (`offer`, `0` or the reader). The act then goes without the fact (`Drop`:
//! stale, does not fit, not started), or its turn starts (`Start`, the act typed with the rider,
//! counted in `typed`) and its ack reaches aterm (`Ack`, told where an offer of the episode stands)
//! or is lost (`LoseAck`, counted in `late`). A lease lapses (`Lapse`, `OFFER_MS`) only once its
//! holder's act is over, the timing assumption: an act's read, turn and ack take far less than
//! the lease. One clock ages every lease from its last offer, so leases lapse in the order they
//! were last offered: while both episodes hold live leases, `older` names the one offered first
//! (0 while fewer than two are live), and only it may lapse, the younger standing; a read that
//! takes an episode again makes it the younger. An episode opens (`Open`, the call goes quiet: a
//! new run, its counts restarted) and closes (`Close`, an observe forgets it and its offer)
//! whatever reader holds it, but opens again only once no reader holds it, a second timing
//! assumption: a call makes progress and goes quiet again over minutes, an act's span over
//! seconds. (An act still in flight across a close and a reopen would carry the old run's fact
//! beside the new run's offer, and its ack, which takes any reader's offer under the claim, would
//! tell the new run's.)
//!
//! Laws: an episode's fact rides a second act only after an ack was lost (`typed <= 1 + late`),
//! and an episode is told only where its fact rode (`told <= typed`). The counts stop at 2, the
//! first a law refuses, so a mutant's graph stays small. Mutants: one offer slot for the whole
//! account (`SlotSteal`, the first draft's: a second reader's offer replaces the first's, whose
//! ack is then refused and whose episode rides again), an ack before the turn started
//! (`R1AcksEarly`), and no lease (`R2ReadsLeased1`: both readers take one episode). The Tier-1
//! bind drives the real `SelfClaimFacts` through every state (`tests/conformance_self_claim_facts.rs`).

use aterm_spec::derive::Model;

/// THE SELF-CLAIM'S FACTS ACCOUNT.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn self_claim_fact_model() -> Model {
    aterm_spec::ty_model! {
        SelfClaimFact {
            const Buggy = 0;
            var open1 = 0;
            var open2 = 0;
            var told1 = 0;
            var told2 = 0;
            var offer1 = 0;
            var offer2 = 0;
            var lapsed1 = 0;
            var lapsed2 = 0;
            var typed1 = 0;
            var typed2 = 0;
            var late1 = 0;
            var late2 = 0;
            var hand1 = 0;
            var hand2 = 0;
            var started1 = 0;
            var started2 = 0;
            var older = 0;

            action Open1 when (open1 == 0 && (hand1 == 0 || hand1 == 2) && (hand2 == 0 || hand2 == 2)) {
                open1 = 1;
                typed1 = 0;
                late1 = 0;
            }
            action Open2 when (open2 == 0 && (hand1 == 0 || hand1 == 1) && (hand2 == 0 || hand2 == 1)) {
                open2 = 1;
                typed2 = 0;
                late2 = 0;
            }
            action Close1 when (open1 == 1) {
                open1 = 0;
                told1 = 0;
                offer1 = 0;
                lapsed1 = 0;
                older = 0;
            }
            action Close2 when (open2 == 1) {
                open2 = 0;
                told2 = 0;
                offer2 = 0;
                lapsed2 = 0;
                older = 0;
            }

            action R1Reads1 when (
                hand1 == 0 && started1 == 0
                    && open1 == 1 && told1 == 0 && (offer1 == 0 || offer1 == 1 || lapsed1 == 1)
            ) {
                hand1 = 1;
                offer1 = 1;
                lapsed1 = 0;
                older = if (offer2 > 0 && lapsed2 == 0) { 2 } else { 0 };
            }
            action R1Reads2 when (
                hand1 == 0 && started1 == 0
                    && (open1 == 0 || told1 == 1 || (offer1 == 2 && lapsed1 == 0))
                    && open2 == 1 && told2 == 0 && (offer2 == 0 || offer2 == 1 || lapsed2 == 1)
            ) {
                hand1 = 2;
                offer2 = 1;
                lapsed2 = 0;
                older = if (offer1 > 0 && lapsed1 == 0) { 1 } else { 0 };
            }
            action R2Reads1 when (
                hand2 == 0 && started2 == 0
                    && open1 == 1 && told1 == 0 && (offer1 == 0 || offer1 == 2 || lapsed1 == 1)
            ) {
                hand2 = 1;
                offer1 = 2;
                lapsed1 = 0;
                older = if (offer2 > 0 && lapsed2 == 0) { 2 } else { 0 };
            }
            action R2Reads2 when (
                hand2 == 0 && started2 == 0
                    && (open1 == 0 || told1 == 1 || (offer1 == 1 && lapsed1 == 0))
                    && open2 == 1 && told2 == 0 && (offer2 == 0 || offer2 == 2 || lapsed2 == 1)
            ) {
                hand2 = 2;
                offer2 = 2;
                lapsed2 = 0;
                older = if (offer1 > 0 && lapsed1 == 0) { 1 } else { 0 };
            }

            action Lapse1 when (
                offer1 > 0 && lapsed1 == 0
                    && ((offer1 == 1 && (hand1 == 0 || hand1 == 2))
                        || (offer1 == 2 && (hand2 == 0 || hand2 == 2)))
                    && (older == 0 || older == 1)
            ) {
                lapsed1 = 1;
                older = 0;
            }
            action Lapse2 when (
                offer2 > 0 && lapsed2 == 0
                    && ((offer2 == 1 && (hand1 == 0 || hand1 == 1))
                        || (offer2 == 2 && (hand2 == 0 || hand2 == 1)))
                    && (older == 0 || older == 2)
            ) {
                lapsed2 = 1;
                older = 0;
            }

            action R1Drops when (hand1 > 0 && started1 == 0) { hand1 = 0; }
            action R2Drops when (hand2 > 0 && started2 == 0) { hand2 = 0; }
            action R1Starts when (hand1 > 0 && started1 == 0) {
                started1 = 1;
                typed1 = if (hand1 == 1 && 2 > typed1) { typed1 + 1 } else { typed1 };
                typed2 = if (hand1 == 2 && 2 > typed2) { typed2 + 1 } else { typed2 };
            }
            action R2Starts when (hand2 > 0 && started2 == 0) {
                started2 = 1;
                typed1 = if (hand2 == 1 && 2 > typed1) { typed1 + 1 } else { typed1 };
                typed2 = if (hand2 == 2 && 2 > typed2) { typed2 + 1 } else { typed2 };
            }
            action R1Acks when (started1 == 1) {
                told1 = if (hand1 == 1 && offer1 > 0) { 1 } else { told1 };
                told2 = if (hand1 == 2 && offer2 > 0) { 1 } else { told2 };
                offer1 = if (hand1 == 1) { 0 } else { offer1 };
                offer2 = if (hand1 == 2) { 0 } else { offer2 };
                lapsed1 = if (hand1 == 1) { 0 } else { lapsed1 };
                lapsed2 = if (hand1 == 2) { 0 } else { lapsed2 };
                older = 0;
                hand1 = 0;
                started1 = 0;
            }
            action R2Acks when (started2 == 1) {
                told1 = if (hand2 == 1 && offer1 > 0) { 1 } else { told1 };
                told2 = if (hand2 == 2 && offer2 > 0) { 1 } else { told2 };
                offer1 = if (hand2 == 1) { 0 } else { offer1 };
                offer2 = if (hand2 == 2) { 0 } else { offer2 };
                lapsed1 = if (hand2 == 1) { 0 } else { lapsed1 };
                lapsed2 = if (hand2 == 2) { 0 } else { lapsed2 };
                older = 0;
                hand2 = 0;
                started2 = 0;
            }
            action R1LosesAck when (started1 == 1) {
                late1 = if (hand1 == 1 && 2 > late1) { late1 + 1 } else { late1 };
                late2 = if (hand1 == 2 && 2 > late2) { late2 + 1 } else { late2 };
                hand1 = 0;
                started1 = 0;
            }
            action R2LosesAck when (started2 == 1) {
                late1 = if (hand2 == 1 && 2 > late1) { late1 + 1 } else { late1 };
                late2 = if (hand2 == 2 && 2 > late2) { late2 + 1 } else { late2 };
                hand2 = 0;
                started2 = 0;
            }

            action SlotSteal when (Buggy == 1 && offer1 > 0 && offer2 > 0) {
                offer1 = 0;
                lapsed1 = 0;
                older = 0;
            }
            action R1AcksEarly when (
                Buggy == 2 && hand1 == 1 && started1 == 0 && offer1 > 0 && told1 == 0
            ) {
                told1 = 1;
                offer1 = 0;
                lapsed1 = 0;
                hand1 = 0;
                older = 0;
            }
            action R2ReadsLeased1 when (
                Buggy == 3 && hand2 == 0 && started2 == 0
                    && open1 == 1 && told1 == 0 && offer1 == 1 && lapsed1 == 0
            ) {
                hand2 = 1;
                offer1 = 2;
                older = if (offer2 > 0 && lapsed2 == 0) { 2 } else { 0 };
            }

            invariant TypedTwiceOnlyAfterALateAck: typed1 <= 1 + late1 && typed2 <= 1 + late2;
            invariant ToldOnlyWhatRode: told1 <= typed1 && told2 <= typed2;
        }
    }
}
