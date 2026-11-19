// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How many long reads of its peers a vault server, or a client fanning out to many servers,
//! holds at once: the slots both take before reading an answer whole.

#[cfg(test)]
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::sync::{Condvar, Mutex, PoisonError};

/// How many peers' records a server reads at once (the server's `pull_from_peer`): each body is
/// up to [`crate::membership_log::REPLY_CAP`]. Frames reuse exact known owners or verify
/// as they arrive, and the completed response enters the server's `apply_records`. A pass asks
/// its peers (up to [`crate::discover::MAX_CANDIDATES`]) for their envelopes at once, which finds
/// the dark ones in one budget, and holds this many records bodies: one read while another is
/// stored. A client's records pass reads its peers' longer answers as many at once
/// (`crate::client`'s `fan_out::RECORDS`).
pub(crate) const RECORDS_PULLS_AT_ONCE: usize = 2;

/// How much of each peer's envelope answer a pass reads at once from all its peers (the server's
/// `pull_from_peer`): a `204` and a small envelope fit. A longer answer is asked again and read
/// whole, up to [`crate::envelope::MAX_ENVELOPE`], under one of [`ENVELOPE_PULLS_AT_ONCE`]
/// slots. A client reading many servers' envelopes at once (`crate::client`'s
/// `fan_out::envelopes_within`) reads each answer the same way.
pub(crate) const ENVELOPE_PEEK: usize = 256 << 10;

/// How many peers' whole envelopes longer than [`ENVELOPE_PEEK`] a server reads at once: each
/// is up to [`crate::envelope::MAX_ENVELOPE`] and is parsed again where the server's `put_vault`
/// stores it, so a host that advertises 16 instances of itself costs a pass this many such
/// bodies, not 16. A client's fan-out of envelope reads through `crate::client`'s `fan_out::reads_within` (an admin
/// command's open, the servers' heads read before a mirror, the sync's probes of its peers, a
/// join's probes, and a join request's check of each mirror before it is posted there,
/// `join_routing`, on a joining Mac) holds as many at once, and one
/// from each compiled-in default address, each until it has verified it, and a join request's
/// until it has also left the request with that server. Two client fan-outs that read only the
/// servers the signed swarm policy names, never one a browse named, still read each of them
/// whole at once: the check of each mirror whose join queue an admin Mac reads (`join_queues`)
/// and an admission's read-back (`ledger_publish::verify_approved_in_served_head`).
pub(crate) const ENVELOPE_PULLS_AT_ONCE: usize = 2;

/// Slots for a few pulls at once ([`RECORDS_PULLS_AT_ONCE`] records pulls,
/// [`ENVELOPE_PULLS_AT_ONCE`] whole envelopes).
pub(crate) struct PullSlots {
    /// How many pulls hold a slot at once.
    at_once: usize,
    taken: Mutex<usize>,
    freed: Condvar,
    /// Pulls waiting for a slot now, shared with a test that watches the slots a client's
    /// fan-out made (`crate::client`'s `fan_out::held`).
    #[cfg(test)]
    pub(crate) waiting: Arc<AtomicUsize>,
}

/// One pull's slot, given back when dropped.
pub(crate) struct PullSlot<'a>(&'a PullSlots);

impl PullSlots {
    /// Slots for `at_once` pulls.
    pub(crate) fn new(at_once: usize) -> PullSlots {
        PullSlots {
            at_once,
            taken: Mutex::new(0),
            freed: Condvar::new(),
            #[cfg(test)]
            waiting: Arc::default(),
        }
    }

    /// A slot, once one is free: a slot's pull ends within its budget.
    pub(crate) fn take(&self) -> PullSlot<'_> {
        let mut taken = self.taken.lock().unwrap_or_else(PoisonError::into_inner);
        #[cfg(test)]
        let queued = usize::from(*taken >= self.at_once);
        #[cfg(test)]
        self.waiting.fetch_add(queued, Ordering::SeqCst);
        while *taken >= self.at_once {
            taken = self
                .freed
                .wait(taken)
                .unwrap_or_else(PoisonError::into_inner);
        }
        #[cfg(test)]
        self.waiting.fetch_sub(queued, Ordering::SeqCst);
        *taken += 1;
        PullSlot(self)
    }
}

impl Drop for PullSlot<'_> {
    fn drop(&mut self) {
        let mut taken = self.0.taken.lock().unwrap_or_else(PoisonError::into_inner);
        *taken = taken.saturating_sub(1);
        self.0.freed.notify_one();
    }
}
