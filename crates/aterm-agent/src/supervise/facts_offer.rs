// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A PROVEN SELF-CLAIM'S FACTS ACCOUNT (gap 23; `docs/DESIGN-system-awareness-2026-10-16.md`
//! §14.4, "A self-supervising agent's acts"): which of a session's measured facts an agent that
//! supervises itself (codex-alab, on its proven claim) may carry on an act it types anyway, and
//! which it was told. aterm keeps the bookkeeping and makes every judgment; the agent reads a fact
//! at its act (`harness fact`) and acknowledges it once that act started (`harness fact told`).
//!
//! The account is aterm's own [`FactsState`], seeded once per process from the session ledger's
//! fact rows ([`SelfClaimFacts::seed`]), and every episode offered, keyed by the episode, with
//! the claim `C` it was offered under (aterm-gui's `self_claim::Proven`) and the reader it is
//! leased to (one of the claim's root threads). An offered episode is withheld from every other
//! reader for [`OFFER_MS`]; the reader it is leased to may take it again at once, so an act
//! decided again carries it again. It is told only on its ack ([`SelfClaimFacts::ack`]): a
//! refused, failed or lost ack, an act that did not start and a fact left off all leave it
//! untold, as a seed treats an intent with no outcome. Only [`SERVED_KINDS`] are served.
//! Pure and clockless: every instant is the caller's milliseconds.

use std::collections::{BTreeMap, BTreeSet};

use super::facts::{EpisodeKey, Fact, FactKind, FactsReading, FactsState, SeedOutcome};

pub use super::policy::turn_end::limits_place_rule;

/// How long an offered episode is withheld from every other reader (the claim's other root
/// threads) before its ack. It covers the longest gap between an offer and its ack: the act
/// types the fact only within [`super::facts::FACT_FRESH_MS`] (5 s) of its reading (codex-alab's
/// last step before the turn request), then the ack follows the act's record, at most two 5 s
/// calls (`harness_act`, `appnotice`), so an ack lands within 15 s. 30 s is twice that.
pub const OFFER_MS: i64 = 30_000;

/// The kinds a proven self-claim's act may carry (`kinds=`): a call at no progress only, the one
/// kind the self-claim's reading carries (aterm-gui's `vitals_jobs::facts_reading` reads no held
/// part). The occupancy kinds (`hold`, `wake`) are aterm's occupancy lines, typed only under
/// grants the owner seals, which no act of another process's carries; `program-check`, which
/// rides aterm's own turn-end acts, is not served in this gap.
pub const SERVED_KINDS: [FactKind; 1] = [FactKind::NoProgress];

/// [`Pick::Why`]: no reading, or one older than [`super::facts::FACT_FRESH_MS`].
pub const WHY_STALE: &str = "stale";
/// [`Pick::Why`]: the reading names no call at no progress.
pub const WHY_NONE: &str = "none";
/// [`Pick::Why`]: every call the reading names was told.
pub const WHY_TOLD: &str = "told";
/// [`Pick::Why`]: an untold call stands, leased to another reader.
pub const WHY_OFFERED: &str = "offered";

/// What a reader may be offered now ([`SelfClaimFacts::pick`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// The fact to offer.
    Fact(Fact),
    /// None, and why: [`WHY_STALE`], [`WHY_NONE`], [`WHY_TOLD`] or [`WHY_OFFERED`].
    Why(&'static str),
}

/// One episode offered: the fact as offered (its sentence is what the ack books), the claim and
/// the reader it is leased to, and when.
#[derive(Debug, Clone)]
struct Offer<C> {
    fact: Fact,
    claim: C,
    reader: String,
    at_ms: i64,
}

/// A PROVEN SELF-CLAIM'S FACTS ACCOUNT for one session: aterm's own [`FactsState`], seeded once
/// per process from the session ledger, and each episode offered, keyed by the episode, with the
/// claim `C` and the reader it is leased to.
#[derive(Debug, Clone)]
pub struct SelfClaimFacts<C> {
    state: FactsState,
    seeded: bool,
    offers: BTreeMap<EpisodeKey, Offer<C>>,
}

impl<C> Default for SelfClaimFacts<C> {
    fn default() -> Self {
        Self {
            state: FactsState::default(),
            seeded: false,
            offers: BTreeMap::new(),
        }
    }
}

/// `reading`'s served part: its calls at no progress, and no held programs.
fn served(reading: &FactsReading) -> FactsReading {
    FactsReading {
        as_of_ms: reading.as_of_ms,
        no_progress: reading.no_progress.clone(),
        held: None,
    }
}

impl<C: Clone + Eq> SelfClaimFacts<C> {
    /// Whether the account was seeded ([`Self::seed`]).
    #[must_use]
    pub const fn seeded(&self) -> bool {
        self.seeded
    }

    /// Seed the account from the session ledger's fact rows (`occupancy::door::seed_rows_in`),
    /// oldest first, at `now_ms`, once ([`FactsState::seed`]): every later call keeps what the
    /// first made. A row of a kind this account does not serve holds nothing it offers.
    pub fn seed(&mut self, rows: &[(EpisodeKey, i64, SeedOutcome)], now_ms: i64) {
        if self.seeded {
            return;
        }
        self.state.seed(rows, now_ms);
        self.seeded = true;
    }

    /// FOLD IN WHAT THE HOST READ at `now_ms`: [`FactsState::observe`] of its served part, then,
    /// where that part is fresh ([`FactsReading::fresh_at`]), drop each offer whose episode it no
    /// longer names (the call made progress or ended). An absent or stale reading drops nothing.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SelfClaimFact",
            action = "Close1",
            project = "conformance_self_claim_facts::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SelfClaimFact",
            action = "Close2",
            project = "conformance_self_claim_facts::project"
        )
    )]
    pub fn observe(&mut self, reading: Option<&FactsReading>, now_ms: i64) {
        let Some(r) = reading.map(served) else {
            return;
        };
        self.state.observe(Some(&r), now_ms);
        if r.fresh_at(now_ms) {
            let open: BTreeSet<EpisodeKey> = r
                .no_progress
                .iter()
                .filter_map(EpisodeKey::of_call)
                .collect();
            self.offers.retain(|key, _| open.contains(key));
        }
    }

    /// WHAT `reader` OF `claim` MAY BE OFFERED NOW: [`FactsState::pending`] of the reading's
    /// served part over a view in which every offer younger than [`OFFER_MS`] that is leased to
    /// another (claim, reader) is spoken for. The reader's own offer is not excluded, so an act
    /// decided again takes it again. Else why not: [`WHY_STALE`] (no reading, or one that fails
    /// [`FactsReading::tells_at`]), [`WHY_NONE`] (it names no call), [`WHY_TOLD`] (every call it
    /// names was told), [`WHY_OFFERED`] (an untold call is leased to another reader).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SelfClaimFact",
            action = "R1Reads1",
            project = "conformance_self_claim_facts::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SelfClaimFact",
            action = "R1Reads2",
            project = "conformance_self_claim_facts::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SelfClaimFact",
            action = "R2Reads1",
            project = "conformance_self_claim_facts::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SelfClaimFact",
            action = "R2Reads2",
            project = "conformance_self_claim_facts::project"
        )
    )]
    #[must_use]
    pub fn pick(
        &self,
        reading: Option<&FactsReading>,
        claim: &C,
        reader: &str,
        now_ms: i64,
    ) -> Pick {
        let Some(r) = reading.map(served).filter(|r| r.tells_at(now_ms)) else {
            return Pick::Why(WHY_STALE);
        };
        if !r
            .no_progress
            .iter()
            .any(|call| EpisodeKey::of_call(call).is_some())
        {
            return Pick::Why(WHY_NONE);
        }
        if self.state.pending(Some(&r), now_ms).is_none() {
            return Pick::Why(WHY_TOLD);
        }
        let mut view = self.state.clone();
        for (key, offer) in &self.offers {
            let leased = now_ms.saturating_sub(offer.at_ms) < OFFER_MS;
            let own = offer.claim == *claim && offer.reader == reader;
            if leased && !own {
                view.told(*key, offer.at_ms);
            }
        }
        view.pending(Some(&r), now_ms)
            .filter(|fact| SERVED_KINDS.contains(&fact.key.kind))
            .map_or(Pick::Why(WHY_OFFERED), Pick::Fact)
    }

    /// Lease `fact`'s episode to (`claim`, `reader`) at `now_ms`, replacing that episode's offer
    /// only, so a lease its reader takes again is aged from this offer. The Tier-1 bind's negative
    /// controls plant the opposites: `OneOfferSlot` replaces every offer, `LeaseFromFirstOffer`
    /// keeps a lease its reader takes again before it lapses aged from its first offer.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SelfClaimFact",
            action = "R1Reads1",
            project = "conformance_self_claim_facts::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SelfClaimFact",
            action = "R1Reads2",
            project = "conformance_self_claim_facts::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SelfClaimFact",
            action = "R2Reads1",
            project = "conformance_self_claim_facts::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SelfClaimFact",
            action = "R2Reads2",
            project = "conformance_self_claim_facts::project"
        )
    )]
    pub fn offer(&mut self, fact: &Fact, claim: C, reader: &str, now_ms: i64) {
        use super::occupancy::fault::{Fault, armed};
        let at_ms = if armed(Fault::LeaseFromFirstOffer) {
            self.offers
                .get(&fact.key)
                .filter(|offer| {
                    offer.claim == claim
                        && offer.reader == reader
                        && now_ms.saturating_sub(offer.at_ms) < OFFER_MS
                })
                .map_or(now_ms, |offer| offer.at_ms)
        } else {
            now_ms
        };
        if armed(Fault::OneOfferSlot) {
            self.offers.clear();
        }
        self.offers.insert(
            fact.key,
            Offer {
                fact: fact.clone(),
                claim,
                reader: reader.to_string(),
                at_ms,
            },
        );
    }

    /// THE ACK: where the served episode `id` has an offer under `claim` (by any reader: a late
    /// ack after a lapse still counts) and is untold, [`FactsState::told`] at `now_ms`, drop the
    /// offer, and return the fact as it was offered (the sentence its told row books); else `None`
    /// and nothing changes.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SelfClaimFact",
            action = "R1Acks",
            project = "conformance_self_claim_facts::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SelfClaimFact",
            action = "R2Acks",
            project = "conformance_self_claim_facts::project"
        )
    )]
    pub fn ack(&mut self, id: u64, claim: &C, now_ms: i64) -> Option<Fact> {
        let key = SERVED_KINDS
            .into_iter()
            .map(|kind| EpisodeKey { kind, id })
            .find(|key| self.offers.get(key).is_some_and(|o| o.claim == *claim))?;
        if self.state.is_told(key) {
            return None;
        }
        let offer = self.offers.remove(&key)?;
        self.state.told(key, now_ms);
        Some(offer.fact)
    }
}

#[cfg(test)]
#[path = "facts_offer_tests.rs"]
mod tests;
