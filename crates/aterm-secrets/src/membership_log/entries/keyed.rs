// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The keyed epochs of a log (F21 rules 1 and 4) and which of them a full reseal covers, read
//! from the reachability index: every opening of a log's entries reads them again.
//!
//! Whether a counting epoch is a full reseal turns on the newest epochs of its audience in its
//! past, which is its frontier among the audience's epochs ([`Reach::frontiers`]), one pass for
//! every audience. It also turns on whether a keyed epoch has a dependent in its past, and
//! whether an earlier full reseal in its past covers that epoch already: each is one row of the
//! index against a set. A full reseal covers a keyed epoch when the newest of its dependents lie
//! in the reseal's past. So an audience of `k` epochs in a log of about `k` records costs
//! `O(k^2 / 64)` words plus two rows per epoch for each keyed epoch, where scanning each
//! epoch's audience for its past and testing each pair there cost `O(k^3)` tests.

use super::Keyed;
use crate::age::Recipient;
use crate::membership_log::reach::{Reach, tally};
use crate::membership_log::{Body, EpochAudience, Log, MemberId, RecordId, eval};
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
pub(super) mod oracle;

/// A broken way of reading keyed epochs, for the tests' negative controls.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mutant {
    /// A reseal's signer holds the key of every newest epoch in its past, whoever it is.
    HoldEveryNewest,
    /// A reseal forgets the earlier full reseals in its past: it must attest every keyed epoch
    /// of its audience again, one they covered too.
    ForgetCovering,
    /// A full reseal covers a keyed epoch once any one of its newest dependents lies in the
    /// reseal's past, not every one.
    AnyNewestDependent,
}

#[cfg(test)]
thread_local! {
    /// The mutant this thread reads keyed epochs as, if any.
    pub(super) static MUTANT: std::cell::Cell<Option<Mutant>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn mutant(which: Mutant) -> bool {
    MUTANT.get() == Some(which)
}

/// Whether `set`, from the index's word 0, meets the strict past of record `at`.
fn meets(reach: &Reach, at: usize, set: &[u64]) -> bool {
    let row = reach.row(at);
    tally(row.len().min(set.len()), 0, 0);
    row.iter().zip(set).any(|(past, member)| past & member != 0)
}

/// `set` with record `at` in it.
fn insert(set: &mut Vec<u64>, at: usize) {
    if set.len() <= at / 64 {
        set.resize(at / 64 + 1, 0);
    }
    set[at / 64] |= 1 << (at % 64);
}

impl Log {
    /// [`Log::keyed_epochs`], reading `effective`; in the tests every reading checks itself
    /// against the one it replaced ([`oracle::check`]).
    pub(super) fn keyed_epochs_in(&self, effective: &eval::Effective) -> Vec<Keyed> {
        let found = self.keyed_epochs_read(effective);
        #[cfg(test)]
        oracle::check(self, effective, &found);
        found
    }

    fn keyed_epochs_read(&self, effective: &eval::Effective) -> Vec<Keyed> {
        let keyed: Vec<usize> = (1..self.order.len())
            .filter(|&i| effective.keyed(i))
            .collect();
        if keyed.is_empty() {
            return Vec::new();
        }
        let membership = self.membership();
        let member_of: BTreeMap<Recipient, MemberId> = membership
            .members()
            .iter()
            .map(|(actor, m)| (m.binding.recipient(), *actor))
            .collect();
        // Every recipient a seat binds to each actor: an attestation is a `Wrap` its signer
        // addressed to itself.
        let mut recipients: BTreeMap<MemberId, BTreeSet<Recipient>> = BTreeMap::new();
        let mut epochs: BTreeMap<&EpochAudience, Vec<usize>> = BTreeMap::new();
        let mut wraps: BTreeMap<RecordId, Vec<(usize, Recipient)>> = BTreeMap::new();
        for (i, record) in self.order.iter().enumerate() {
            match record.body() {
                Body::Genesis { founders, .. } => {
                    for founder in founders {
                        if let Some(actor) = founder.member() {
                            recipients
                                .entry(actor)
                                .or_default()
                                .insert(founder.recipient);
                        }
                    }
                }
                Body::Admit { entrant, .. } => {
                    recipients
                        .entry(entrant.member())
                        .or_default()
                        .insert(entrant.request.binding().recipient());
                }
                Body::Epoch { audience, .. } if effective.counts(i) => {
                    epochs.entry(audience).or_default().push(i);
                }
                Body::Wrap {
                    epoch, recipient, ..
                } => wraps.entry(*epoch).or_default().push((i, *recipient)),
                _ => {}
            }
        }
        let audience_of = |e: usize| match self.order[e].body() {
            Body::Epoch { audience, .. } => Some(audience),
            _ => None,
        };
        let wraps_of = |e: usize| wraps.get(self.order[e].id()).map_or(&[][..], Vec::as_slice);
        let own = |actor: &MemberId, recipient: &Recipient| {
            recipients
                .get(actor)
                .is_some_and(|set| set.contains(recipient))
        };
        // At record `at`, `actor` has the key of the counting epoch `h` when it minted `h` or a
        // counting `Wrap` of `h` to it lies in `at`'s past.
        let has_counting_key = |actor: &MemberId, h: usize, at: usize| {
            #[cfg(test)]
            if mutant(Mutant::HoldEveryNewest) {
                return true;
            }
            self.order[h].actor() == actor
                || wraps_of(h).iter().any(|&(w, recipient)| {
                    effective.counts(w) && own(actor, &recipient) && effective.before(w, at)
                })
        };
        // ...and of the keyed epoch `k` only when its attestation of `k` (a counting `Wrap` of
        // `k` it signed to itself) lies in `at`'s past: only a reseal signs one.
        let attests = |actor: &MemberId, k: usize, at: usize| {
            wraps_of(k).iter().any(|&(w, recipient)| {
                effective.counts(w)
                    && self.order[w].actor() == actor
                    && own(actor, &recipient)
                    && effective.before(w, at)
            })
        };
        // Each keyed epoch's dependents and holders: the dependents' signers, and the members
        // rule 2's usable Wraps of it are addressed to.
        let held: Vec<(usize, &EpochAudience, &[usize], BTreeSet<MemberId>)> = keyed
            .iter()
            .filter_map(|&e| {
                let audience = audience_of(e)?;
                let dependents = effective.dependents(e);
                let mut holders: BTreeSet<MemberId> =
                    dependents.iter().map(|&d| *self.order[d].actor()).collect();
                for &(w, recipient) in wraps_of(e) {
                    if effective.opens_wrap(w)
                        && let Some(member) = member_of.get(&recipient)
                    {
                        holders.insert(*member);
                    }
                }
                Some((e, audience, dependents, holders))
            })
            .collect();
        let reach = effective.reach();
        // Each keyed epoch's dependents as one set, and the newest of them: every dependent lies
        // in a reseal's past exactly when the newest do.
        let dependents: Vec<Vec<u64>> = held
            .iter()
            .map(|(_, _, dependents, _)| {
                let mut set = Vec::new();
                for &d in *dependents {
                    insert(&mut set, d);
                }
                set
            })
            .collect();
        let newest_dependents: Vec<Vec<usize>> = held
            .iter()
            .map(|(_, _, dependents, _)| reach.maximal(dependents))
            .collect();
        let (lists, groups): (Vec<&EpochAudience>, Vec<Vec<usize>>) = epochs.into_iter().unzip();
        // Each audience's keyed epochs, and those of them a current member holds.
        let of_audience: Vec<Vec<usize>> = lists
            .iter()
            .map(|&audience| (0..held.len()).filter(|&j| held[j].1 == audience).collect())
            .collect();
        let member_held: Vec<bool> = held
            .iter()
            .map(|(_, _, _, holders)| holders.iter().any(|h| membership.contains(h)))
            .collect();
        // A full reseal: a counting epoch of the audience whose signer, there, has the key of
        // every newest counting epoch of the audience in its past and of every keyed epoch of the
        // audience that a current member holds, with a dependent in its past, that no earlier
        // full reseal in its past covers already. A covered epoch's key goes to no newcomer and
        // nobody attests it again, so a later keyed epoch of the audience is covered by whoever
        // reseals it; one no current member holds nobody can attest, and rule 7 ignores it too.
        // Decided in record order, where an epoch's past comes before it: the full reseals that
        // cover each keyed epoch (every dependent in their past) are kept as one set per keyed
        // epoch.
        let mut covering: Vec<Vec<u64>> = vec![Vec::new(); held.len()];
        let at = &self.held;
        reach.frontiers(
            &groups,
            |x| self.order[x].parents().iter().map(move |parent| at[parent]),
            |g, position, frontier| {
                let e2 = groups[g][position];
                let signer = self.order[e2].actor();
                let newest_held = frontier
                    .positions()
                    .into_iter()
                    .all(|h| has_counting_key(signer, groups[g][h], e2));
                let is_full = newest_held
                    && of_audience[g]
                        .iter()
                        .copied()
                        .filter(|&j| member_held[j] && meets(reach, e2, &dependents[j]))
                        .filter(|&j| {
                            #[cfg(test)]
                            if mutant(Mutant::ForgetCovering) {
                                return true;
                            }
                            !meets(reach, e2, &covering[j])
                        })
                        .all(|j| attests(signer, held[j].0, e2));
                if is_full {
                    for &j in &of_audience[g] {
                        tally(0, newest_dependents[j].len(), 0);
                        let mut seen = newest_dependents[j].iter().map(|&d| reach.before(d, e2));
                        #[cfg(test)]
                        let covers = if mutant(Mutant::AnyNewestDependent) {
                            seen.any(|before| before)
                        } else {
                            seen.all(|before| before)
                        };
                        #[cfg(not(test))]
                        let covers = seen.all(|before| before);
                        if covers {
                            insert(&mut covering[j], e2);
                        }
                    }
                }
            },
        );
        held.into_iter()
            .zip(covering)
            .map(|((e, audience, dependents, holders), covering)| Keyed {
                epoch: *self.order[e].id(),
                audience: audience.clone(),
                dependents: dependents.iter().map(|&d| *self.order[d].id()).collect(),
                holders,
                covered: covering.iter().any(|&word| word != 0),
            })
            .collect()
    }
}
