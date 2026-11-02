// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retention of already admitted, immutable account inventory observations.
//! Authentication and completed-exchange admission belong to the producer.

use aterm_types::network_state::AccountsExchangeObservation;
use std::time::Instant;

pub(crate) struct Candidate {
    pub observation: AccountsExchangeObservation,
    pub sampled_at: Instant,
}

/// Retain a later original sample without changing any of its historical facts.
/// A restored observation has no process-local sample; its first new admitted
/// observation establishes one, independently of changes to the wall clock.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountExchangeRetention",
        action = "First",
        project = "aterm_db::network_state::account_exchange::tests::retention_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountExchangeRetention",
        action = "Newer",
        project = "aterm_db::network_state::account_exchange::tests::retention_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountExchangeRetention",
        action = "SameSample",
        project = "aterm_db::network_state::account_exchange::tests::retention_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountExchangeRetention",
        action = "OlderSample",
        project = "aterm_db::network_state::account_exchange::tests::retention_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountExchangeRetention",
        action = "FirstAfterHistory",
        project = "aterm_db::network_state::account_exchange::tests::retention_transition"
    )
)]
pub(crate) fn retain(
    last: &mut Option<AccountsExchangeObservation>,
    last_sample: &mut Option<Instant>,
    candidate: Candidate,
) -> bool {
    if last_sample.is_some_and(|sample| candidate.sampled_at <= sample) {
        return false;
    }
    *last = Some(candidate.observation);
    *last_sample = Some(candidate.sampled_at);
    true
}

#[cfg(test)]
mod tests;
