// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a log's counting `Epoch`s and `Wrap`s say, read once per log state: each audience's
//! epochs, the newest of them and its highest generation, each epoch's first counting `Wrap` to
//! each recipient, and the verdicts read from them ([`Log::audience_left_out_recipient`],
//! [`Log::audience_left_out`], [`Log::wraps_maximal_audience_to`]).
//!
//! Every handshake asks these again, and each asking scanned every record and tested each pair
//! of an audience's epochs. Now the newest come from one row per newest epoch
//! ([`Reach::maximal`]), a verdict reads one row per newest epoch at most, and a verdict once
//! taken is kept: an audience of `k` epochs in a log of about `k` records costs `O(k^2 / 64)`
//! words once, where each asking cost `O(k^2)` tests. The tender's dues ([`Log::rekey_dues`])
//! and [`Log::newest_epochs`] read the newest and the wraps from it too, which each tend tested
//! pair by pair.
//!
//! A log keeps its cache until it holds an `Epoch`, a `Wrap` or a weighed record ([`resets`]):
//! nothing the cache read changes otherwise. Which records count turns on the weighed records
//! alone, and so do the members' bindings and entries a verdict reads; a new record's row leaves
//! every earlier row as it was, so the newest epochs stay the newest. Clones share the cache
//! until one of them holds such a record.

use super::reach::{Reach, holds, tally};
use super::{Body, EpochAudience, Evidence, Kind, Log, MemberId, RecordId};
use crate::age::Recipient;
use aterm_types::MutexExt as _;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

#[cfg(test)]
pub(super) mod oracle;
#[cfg(test)]
mod tests;

/// A broken cache, for the tests' negative controls.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mutant {
    /// The log keeps its cache when it holds a record of one of these kinds.
    CarryAcross(&'static [Kind]),
    /// A verdict once taken is taken again on every asking.
    Forget,
    /// A verdict reads no seat entry.
    IgnoreSeat,
    /// A verdict reads no prior wrap.
    IgnorePriorWraps,
    /// A newest epoch the seat entry follows learns from a prior wrap all the same.
    IgnoreEntryAfter,
    /// A prior wrap the entry of a Mac that came back follows counts all the same.
    IgnoreReturn,
}

#[cfg(test)]
thread_local! {
    /// The mutant this thread's logs keep their caches as, if any.
    pub(super) static MUTANT: std::cell::Cell<Option<Mutant>> = const { std::cell::Cell::new(None) };
}

/// Whether a log that holds a record of `kind` lets its cache go: a record that mints or wraps
/// an epoch, or one the weighing reads, which can change which records count and who is a
/// member. Every kind is named, so a new one is classified where it is added.
pub(super) fn resets(kind: Kind) -> bool {
    #[cfg(test)]
    if let Some(Mutant::CarryAcross(kept)) = MUTANT.get()
        && kept.contains(&kind)
    {
        return false;
    }
    match kind {
        Kind::Genesis | Kind::Admit | Kind::Remove | Kind::Epoch | Kind::Wrap => true,
        Kind::Name | Kind::Put | Kind::Delete | Kind::Node => false,
    }
}

/// One audience's counting epochs.
#[derive(Debug, Default)]
pub(super) struct Sealed {
    /// Their record indexes, ascending.
    pub(super) epochs: Vec<usize>,
    /// The newest of them, those in no other's past, ascending.
    pub(super) newest: Vec<usize>,
    /// The highest generation among them.
    pub(super) generation: u64,
    /// Verdicts belong to this audience; keys never copy its host list.
    recipients: Mutex<BTreeMap<Recipient, bool>>,
    members: Mutex<BTreeMap<MemberId, bool>>,
}

/// A recipient's seat entry, as a verdict reads it: its record, and whether it is an `Admit`
/// whose evidence is [`Evidence::Listed`].
#[derive(Clone, Copy, Debug)]
pub(super) struct Seat {
    pub(super) at: usize,
    pub(super) listed: bool,
}

/// What a log's counting epochs and wraps say, and the verdicts taken from them so far.
#[derive(Debug)]
pub(super) struct Epochs {
    audiences: BTreeMap<EpochAudience, Sealed>,
    /// For each epoch a counting `Wrap` names, the first such wrap to each recipient.
    wraps: BTreeMap<RecordId, BTreeMap<Recipient, usize>>,
    /// Only identities in the log can affect a verdict. Unknown query keys are never retained.
    known_recipients: BTreeSet<Recipient>,
}

/// The verdict `memo` already holds for `key`. A memo a panic elsewhere poisoned is read as it
/// is: a verdict is written whole or not at all.
fn known<K: Ord, V: Copy>(memo: &Mutex<BTreeMap<K, V>>, key: &K) -> Option<V> {
    #[cfg(test)]
    if MUTANT.get() == Some(Mutant::Forget) {
        return None;
    }
    memo.lock_or_recover().get(key).copied()
}

impl Epochs {
    /// Read `log`'s counting epochs and wraps.
    pub(super) fn of(log: &Log) -> Self {
        let effective = log.effective();
        let mut audiences: BTreeMap<EpochAudience, Sealed> = BTreeMap::new();
        let mut wraps: BTreeMap<RecordId, BTreeMap<Recipient, usize>> = BTreeMap::new();
        for (i, record) in log.order.iter().enumerate().skip(1) {
            if !effective.counts(i) {
                continue;
            }
            match record.body() {
                Body::Epoch {
                    audience,
                    generation,
                    ..
                } => {
                    let sealed = audiences.entry(audience.clone()).or_default();
                    sealed.epochs.push(i);
                    sealed.generation = sealed.generation.max(*generation);
                }
                Body::Wrap {
                    epoch, recipient, ..
                } => {
                    wraps
                        .entry(*epoch)
                        .or_default()
                        .entry(*recipient)
                        .or_insert(i);
                }
                _ => {}
            }
        }
        for sealed in audiences.values_mut() {
            sealed.newest = effective.reach().maximal(&sealed.epochs);
            #[cfg(test)]
            assert_eq!(
                Ok(&sealed.newest),
                oracle::pairwise_newest(&sealed.epochs, &|a, b| effective.before(a, b), usize::MAX)
                    .as_ref(),
                "the newest epochs are the ones no other follows"
            );
        }
        Self {
            known_recipients: log
                .membership_ref()
                .members()
                .values()
                .map(|member| member.binding.recipient())
                .chain(
                    wraps
                        .values()
                        .flat_map(|recipients| recipients.keys().copied()),
                )
                .collect(),
            audiences,
            wraps,
        }
    }

    /// Every audience with a counting epoch, and its epochs.
    pub(super) const fn audiences(&self) -> &BTreeMap<EpochAudience, Sealed> {
        &self.audiences
    }

    /// The newest counting epochs of `audience`, ascending.
    pub(super) fn newest(&self, audience: &EpochAudience) -> &[usize] {
        self.audiences
            .get(audience)
            .map_or(&[][..], |sealed| &sealed.newest)
    }

    /// For each epoch a counting `Wrap` names, the first such wrap to each recipient.
    pub(super) const fn wraps(&self) -> &BTreeMap<RecordId, BTreeMap<Recipient, usize>> {
        &self.wraps
    }

    /// The first counting `Wrap` of the epoch at `at` to `recipient`.
    fn wrap(&self, log: &Log, at: usize, recipient: &Recipient) -> Option<usize> {
        self.wraps.get(log.order[at].id())?.get(recipient).copied()
    }

    /// [`Log::audience_left_out_recipient`], taken once per pair.
    pub(super) fn leaves_out_recipient(
        &self,
        log: &Log,
        audience: &EpochAudience,
        recipient: &Recipient,
    ) -> bool {
        if !self.known_recipients.contains(recipient) {
            return false;
        }
        let Some(sealed) = self.audiences.get(audience) else {
            return false;
        };
        if let Some(verdict) = known(&sealed.recipients, recipient) {
            return verdict;
        }
        let verdict = {
            let prior: Vec<usize> = sealed
                .epochs
                .iter()
                .filter_map(|&e| self.wrap(log, e, recipient))
                .collect();
            knew(
                log.effective().reach(),
                &sealed.newest,
                |at| self.wrap(log, at, recipient).is_some(),
                &prior,
                seat(log, recipient),
                matches!(audience, EpochAudience::Everyone | EpochAudience::Commons),
            )
        };
        sealed
            .recipients
            .lock_or_recover()
            .insert(*recipient, verdict);
        verdict
    }

    /// [`Log::audience_left_out`], taken once per pair.
    pub(super) fn leaves_out_member(
        &self,
        log: &Log,
        audience: &EpochAudience,
        member: &MemberId,
    ) -> bool {
        if !log.membership_ref().contains(member) {
            return true;
        }
        let Some(sealed) = self.audiences.get(audience) else {
            return false;
        };
        if let Some(verdict) = known(&sealed.members, member) {
            return verdict;
        }
        let membership = log.membership_ref();
        let verdict = match membership
            .get(member)
            .and_then(|seat| Some((log.held.get(&seat.entry)?, seat.binding.recipient())))
        {
            None => true,
            Some((&entry, recipient)) => {
                self.leaves_out_recipient(log, audience, &recipient) || {
                    let reach = log.effective().reach();
                    self.newest(audience).iter().any(|&at| {
                        tally(0, 1, 0);
                        self.wrap(log, at, &recipient).is_none() && reach.upto(entry, at)
                    })
                }
            }
        };
        sealed.members.lock_or_recover().insert(*member, verdict);
        verdict
    }

    /// [`Log::wraps_maximal_audience_to`].
    pub(super) fn wraps_newest_to(
        &self,
        log: &Log,
        audience: &EpochAudience,
        recipient: &Recipient,
    ) -> bool {
        let newest = self.newest(audience);
        !newest.is_empty()
            && newest
                .iter()
                .all(|&at| self.wrap(log, at, recipient).is_some())
    }
}

/// The seat entry a verdict on `recipient` reads: of the members bound to it, the newest entry
/// that is not a listed `Admit`, else the newest listed one.
fn seat(log: &Log, recipient: &Recipient) -> Option<Seat> {
    log.membership_ref()
        .members()
        .values()
        .filter(|m| m.binding.recipient() == *recipient)
        .filter_map(|member| {
            let &at = log.held.get(&member.entry)?;
            let listed = matches!(
                log.order[at].body(),
                Body::Admit {
                    evidence: Evidence::Listed,
                    ..
                }
            );
            Some(Seat { at, listed })
        })
        .max_by_key(|seat| (!seat.listed, seat.at))
}

/// Whether one of an audience's `newest` epochs leaves a recipient out on purpose: it holds no
/// wrap to the recipient (`wrapped` says which do), yet its minter knew the recipient, since its
/// past holds the recipient's `seat` entry (when `seated`: the audience is `[everyone]` or the
/// Commons, which every member's entry admits to) or a `prior` wrap to it of another of the
/// audience's epochs (each epoch's first). For a `seated` audience an epoch the entry follows
/// learns nothing from a prior wrap, and neither does one from a wrap a newer entry that is not
/// a listed `Admit` follows: the Mac came back after it.
///
/// It reads one row per newest epoch at most, ORing the prior wraps into one set first.
pub(super) fn knew(
    reach: &Reach,
    newest: &[usize],
    wrapped: impl Fn(usize) -> bool,
    prior: &[usize],
    seat: Option<Seat>,
    seated: bool,
) -> bool {
    #[cfg(test)]
    let (seat, prior) = match MUTANT.get() {
        Some(Mutant::IgnoreSeat) => (None, prior),
        Some(Mutant::IgnorePriorWraps) => (seat, &[][..]),
        _ => (seat, prior),
    };
    let seat = seat.filter(|_| seated);
    let returned = seat.filter(|seat| !seat.listed).map(|seat| seat.at);
    #[cfg(test)]
    let returned = returned.filter(|_| MUTANT.get() != Some(Mutant::IgnoreReturn));
    #[cfg(test)]
    let entry_after = MUTANT.get() != Some(Mutant::IgnoreEntryAfter);
    #[cfg(not(test))]
    let entry_after = true;
    let mut known: Vec<u64> = Vec::new();
    for &w in prior {
        tally(0, 1, 0);
        if returned.is_some_and(|entry| reach.before(w, entry)) {
            continue;
        }
        if known.len() <= w / 64 {
            known.resize(w / 64 + 1, 0);
        }
        known[w / 64] |= 1 << (w % 64);
    }
    newest.iter().any(|&at| {
        tally(0, 1, 0);
        if wrapped(at) {
            return false;
        }
        if seat.is_some_and(|seat| reach.upto(seat.at, at)) {
            return true;
        }
        if known.is_empty() || seat.is_some_and(|seat| entry_after && reach.before(at, seat.at)) {
            return false;
        }
        let row = reach.row(at);
        tally(row.len().min(known.len()), 0, 0);
        holds(&known, 0, at) || row.iter().zip(&known).any(|(past, wrap)| past & wrap != 0)
    })
}
