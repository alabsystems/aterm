// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! When an audience is due a new epoch, and who mints it (`docs/DESIGN-fluid-swarm-2026-10-28.md`
//! §3.3.7, F21 rule 5), read from the log's counting records alone, so every Mac that holds the
//! same records names the same due, the same Macs and the same order:
//!
//! - [`Log::rekey_dues`]: each audience whose causally newest epochs still wrap a recipient the
//!   log removed (rotation), that has two or more newest epochs that are not [`secure_twins`]
//!   (coverage), or that has an uncovered keyed epoch a current member holds and can reseal
//!   (F21 rule 5); with the current members that can carry it, the removers among them and the
//!   resealers.
//! - [`RekeyDue::turn`]: where one member stands. The removers act first, with no grace; where
//!   one can carry the audience, every other member waits a grace and then takes the rank of its
//!   absolute place in a hash order over every seated member, so no Mac's local view shifts
//!   another's rank. Where none can, the carriers take ranks with no grace.
//! - [`Log::unsettled`]: the epochs whose minter's batch this log may hold only a prefix of. A
//!   batch is an `Epoch`, its `Wrap`s in ascending recipient order, then its `Put`s, each record
//!   after the signer's prior one, and the exchange moves a few records a round; a `Wrap` missing
//!   from a prefix reads as a recipient left out on purpose ([`Log::left_out`]), so nobody mints
//!   from, carries recipients from or closes gaps on such an epoch.
//! - [`secure_twins`]: two or more newest epochs with one recipient set and no removed recipient,
//!   which need no coalescing; a writer seals under the lowest id among them.

use super::{Body, EpochAudience, Log, MemberId, Membership, RecordId};
use crate::age::Recipient;
use aterm_digest::Sha256;
use std::collections::{BTreeMap, BTreeSet};

const RANK_DOMAIN: &[u8] = b"aterm/members/rekey-rank/3\0";
const TAG_DOMAIN: &[u8] = b"aterm/members/rekey-audience/1\0";

#[cfg(test)]
thread_local! {
    /// The negative control of [`secure_twins`]: while set, no newest epochs are twins, so
    /// identical concurrent epochs are due a coalesce as every concurrent pair once was.
    pub(crate) static TWINS_DUE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// One audience due a new epoch ([`Log::rekey_dues`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RekeyDue {
    /// The audience.
    pub audience: EpochAudience,
    /// Its causally newest counting epochs, by id.
    pub newest: Vec<RecordId>,
    /// A newest epoch still wraps a recipient the log removed.
    pub rotation: bool,
    /// It has two or more newest epochs that are not [`secure_twins`].
    pub coverage: bool,
    /// F21 rule 5: it has an uncovered keyed epoch a current member holds, and a carrier holds
    /// every such epoch.
    pub reseal: bool,
    /// The carriers that signed an effective `Remove` of a Mac whose recipient a newest epoch
    /// still wraps, with no current member holding that recipient.
    pub removers: BTreeSet<MemberId>,
    /// The current members that can carry the audience: each minted every newest epoch or holds
    /// a counting `Wrap` of it, and its recipient is not removed.
    pub carriers: BTreeSet<MemberId>,
    /// The carriers that hold every uncovered keyed epoch of the audience ([`super::Keyed`]).
    pub resealers: BTreeSet<MemberId>,
    /// The signers of those keyed epochs' dependents, who reseal first.
    pub reseal_first: BTreeSet<MemberId>,
}

/// Where a member stands for one [`RekeyDue`]: it acts `rank` steps after the due's clock, and
/// after the removers' grace too where `after_grace`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Turn {
    /// Steps after the clock.
    pub rank: usize,
    /// It waits the removers' grace first.
    pub after_grace: bool,
}

/// A fixed name for `audience` (the tender's clock is kept by it): `SHA-256` of a domain, the
/// audience's kind, and a host list's names, each ended by a zero byte.
#[must_use]
pub fn audience_tag(audience: &EpochAudience) -> [u8; 32] {
    let mut buf = TAG_DOMAIN.to_vec();
    match audience {
        EpochAudience::Everyone => buf.push(0),
        EpochAudience::Commons => buf.push(1),
        EpochAudience::Hosts(hosts) => {
            buf.push(2);
            for host in hosts {
                buf.extend_from_slice(host.as_bytes());
                buf.push(0);
            }
        }
    }
    Sha256::digest(&buf)
}

/// Whether `newest` (an audience's causally newest epochs) are two or more epochs that wrap one
/// recipient set, none of it in `removed`: concurrent re-keys to the same Macs, which leave no
/// Mac able to read what another cannot, so the audience is not due a coalesce for them.
#[must_use]
pub fn secure_twins(
    newest: &[RecordId],
    wrapped: &BTreeMap<RecordId, BTreeSet<Recipient>>,
    removed: &BTreeSet<Recipient>,
) -> bool {
    #[cfg(test)]
    if TWINS_DUE.with(std::cell::Cell::get) {
        return false;
    }
    let empty = BTreeSet::new();
    let set = |id: &RecordId| wrapped.get(id).unwrap_or(&empty);
    let Some((first, rest)) = newest.split_first() else {
        return false;
    };
    !rest.is_empty()
        && set(first).is_disjoint(removed)
        && rest.iter().all(|id| set(id) == set(first))
}

/// A member's place in every hash order: `SHA-256` of a domain and its actor, the same for every
/// audience, so the Mac ranked first for one audience of a due is first for each of them and
/// one Mac's batch covers them all.
fn rank_hash(actor: &MemberId) -> [u8; 32] {
    let mut buf = RANK_DOMAIN.to_vec();
    buf.extend_from_slice(actor);
    Sha256::digest(&buf)
}

impl RekeyDue {
    /// Where `me` stands: a remover takes its place among the removers in hash order, with no
    /// grace; where there are removers, every other seated member of `membership` waits the
    /// grace and takes its absolute place among all of them. Where none can carry the audience,
    /// the carriers rank (for a reseal alone, the resealers), the signers of the uncovered keyed
    /// epochs' dependents first. `None` when `me` takes no rank.
    #[must_use]
    pub fn turn(&self, membership: &Membership, me: &MemberId) -> Option<Turn> {
        let none = BTreeSet::new();
        if !self.removers.is_empty() {
            if let Some(rank) = place(self.removers.iter(), &none, me) {
                return Some(Turn {
                    rank,
                    after_grace: false,
                });
            }
            return place(membership.members().keys(), &none, me).map(|rank| Turn {
                rank,
                after_grace: true,
            });
        }
        let ranked = if self.rotation || self.coverage {
            &self.carriers
        } else {
            &self.resealers
        };
        place(ranked.iter(), &self.reseal_first, me).map(|rank| Turn {
            rank,
            after_grace: false,
        })
    }
}

/// `me`'s place among `ranked`: those in `first` first, then by [`rank_hash`].
fn place<'a>(
    ranked: impl Iterator<Item = &'a MemberId>,
    first: &BTreeSet<MemberId>,
    me: &MemberId,
) -> Option<usize> {
    let mut order: Vec<(bool, [u8; 32], MemberId)> = ranked
        .map(|actor| (!first.contains(actor), rank_hash(actor), *actor))
        .collect();
    order.sort_unstable();
    order.iter().position(|(_, _, actor)| actor == me)
}

impl Log {
    /// Every audience due a new epoch, by audience: read from counting `Epoch` and `Wrap` records
    /// and the membership alone, so it is the same on every Mac that holds the same records
    /// (module docs).
    #[must_use]
    pub fn rekey_dues(&self) -> Vec<RekeyDue> {
        let effective = self.effective();
        let membership = self.membership();
        let mut epochs: BTreeMap<&EpochAudience, Vec<usize>> = BTreeMap::new();
        let mut wrapped: BTreeMap<RecordId, BTreeSet<Recipient>> = BTreeMap::new();
        for (i, record) in self.order.iter().enumerate().skip(1) {
            if !effective.counts(i) {
                continue;
            }
            match record.body() {
                Body::Epoch { audience, .. } => epochs.entry(audience).or_default().push(i),
                Body::Wrap {
                    epoch, recipient, ..
                } => {
                    wrapped.entry(*epoch).or_default().insert(*recipient);
                }
                _ => {}
            }
        }
        if epochs.is_empty() {
            return Vec::new();
        }
        let removed = self.removed_recipients();
        // Each recipient a seat ever bound to a Mac now out, from the genesis and every Admit.
        let mut seats: BTreeMap<MemberId, BTreeSet<Recipient>> = BTreeMap::new();
        if let Body::Genesis { founders, .. } = self.genesis().body() {
            for founder in founders {
                if let Some(actor) = founder.member() {
                    seats.entry(actor).or_default().insert(founder.recipient);
                }
            }
        }
        for record in &self.order {
            if let Body::Admit { entrant, .. } = record.body() {
                seats
                    .entry(entrant.member())
                    .or_default()
                    .insert(entrant.request.binding().recipient());
            }
        }
        let keyed: Vec<super::Keyed> = self
            .keyed_epochs()
            .into_iter()
            .filter(|k| !k.covered && k.holders.iter().any(|h| membership.contains(h)))
            .collect();
        let mut dues = Vec::new();
        for (audience, list) in epochs {
            let newest_at: Vec<usize> = list
                .iter()
                .copied()
                .filter(|&a| !list.iter().any(|&b| a != b && effective.before(a, b)))
                .collect();
            let mut newest: Vec<RecordId> =
                newest_at.iter().map(|&i| *self.order[i].id()).collect();
            newest.sort_unstable();
            let wraps_removed = |r: &Recipient| {
                removed.contains(r)
                    && newest
                        .iter()
                        .any(|id| wrapped.get(id).is_some_and(|set| set.contains(r)))
            };
            let rotation = removed.iter().any(&wraps_removed);
            let coverage = newest.len() > 1 && !secure_twins(&newest, &wrapped, &removed);
            let carriers: BTreeSet<MemberId> = membership
                .members()
                .iter()
                .filter(|(actor, member)| {
                    let recipient = member.binding.recipient();
                    !removed.contains(&recipient)
                        && newest_at.iter().all(|&at| {
                            self.order[at].actor() == *actor
                                || wrapped
                                    .get(self.order[at].id())
                                    .is_some_and(|set| set.contains(&recipient))
                        })
                })
                .map(|(actor, _)| *actor)
                .collect();
            let removers: BTreeSet<MemberId> = membership
                .removed()
                .iter()
                .filter(|(target, _)| {
                    seats
                        .get(*target)
                        .is_some_and(|set| set.iter().any(&wraps_removed))
                })
                .flat_map(|(_, removal)| removal.by.iter())
                .filter(|by| carriers.contains(*by))
                .copied()
                .collect();
            let uncovered: Vec<&super::Keyed> =
                keyed.iter().filter(|k| k.audience == *audience).collect();
            let resealers: BTreeSet<MemberId> = if uncovered.is_empty() {
                BTreeSet::new()
            } else {
                carriers
                    .iter()
                    .filter(|actor| uncovered.iter().all(|k| k.holders.contains(*actor)))
                    .copied()
                    .collect()
            };
            let reseal = !resealers.is_empty();
            if !(rotation || coverage || reseal) {
                continue;
            }
            let reseal_first = uncovered
                .iter()
                .flat_map(|k| k.dependents.iter())
                .filter_map(|d| self.get(d).map(|r| *r.actor()))
                .collect();
            dues.push(RekeyDue {
                audience: audience.clone(),
                newest,
                rotation,
                coverage,
                reseal,
                removers,
                carriers,
                resealers,
                reseal_first,
            });
        }
        dues
    }

    /// The epochs of `epochs` whose minter's batch this log may hold only a prefix of: it holds
    /// no record by the epoch's minter but a `Wrap` with the epoch in its causal past (a later
    /// `Epoch`, `Put` or any other record settles it). An epoch it does not hold is not named.
    #[must_use]
    pub fn unsettled(&self, epochs: &[RecordId]) -> Vec<RecordId> {
        let effective = self.effective();
        epochs
            .iter()
            .copied()
            .filter(|id| {
                let Some(&at) = self.held.get(id) else {
                    return false;
                };
                let minter = self.order[at].actor();
                !self
                    .order
                    .iter()
                    .enumerate()
                    .skip(at + 1)
                    .any(|(i, record)| {
                        record.actor() == minter
                            && !matches!(record.body(), Body::Wrap { .. })
                            && effective.before(at, i)
                    })
            })
            .collect()
    }
}
