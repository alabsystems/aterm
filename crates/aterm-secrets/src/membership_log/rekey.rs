// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! When an audience is due a new epoch, and who mints it (`docs/DESIGN-fluid-swarm-2026-10-28.md`
//! §3.3.7, F21 rule 5), read from the log's counting records alone, so every Mac that holds the
//! same records names the same due, the same Macs and the same order:
//!
//! - [`Log::head_due`]: P1's one rule for a newest epoch (`aterm_mesh_core::revocation`): it is
//!   due a rotation, and no writer seals under it, when its gate is shut (for a revoked seat row,
//!   [`Membership::revoking_rows`], no `Remove` that took it is in the epoch's causal past, or
//!   one is missing while a live row binds its recipient) or it wraps a revoked recipient. The
//!   gate does not read which `Wrap`s of the epoch this Mac holds: another Mac may have handed
//!   the epoch to the removed Mac beside the `Remove` (a heal), and its `Wrap` need not have
//!   arrived here.
//! - [`Log::rekey_dues`]: each audience one of whose causally newest epochs is due by
//!   [`Log::head_due`] (rotation), that has two or more newest epochs that are not [`secure_twins`]
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
//!   from a prefix reads as a recipient left out on purpose ([`Log::left_out`]), so the tender
//!   mints from, and the gap closer closes gaps on, no such epoch while it may still be arriving.
//!   An epoch settles once its minter signs again, or, of a seat audience, once its minter's
//!   `Wrap`s here reach past every recipient its batch could still wrap
//!   ([`Log::batch_could_wrap`]): the rest of the batch can wrap no one, so a batch that ends in
//!   `Wrap`s settles when its last `Wrap` arrives, not after a grace (one-mesh §11 H2).
//! - [`Log::batch_may_wrap`] and [`Log::in_flight`]: what the rest of such a batch may still wrap
//!   (a recipient its audience held before it, above the last one its minter's `Wrap`s reach
//!   here), and of that the recipients a current seat binds. A writer seals under such an epoch
//!   only where the first is inside its plan. A mint that cannot wait (a person's removal, a
//!   writer's fresh epoch, a re-key past the settle grace) carries the second instead of reading
//!   their missing `Wrap`s as left out on purpose, so a seated Mac whose `Wrap` is still on its
//!   way keeps its place in the epoch after it; a Mac with no seat, whose missing `Wrap` may be
//!   the only record of its retirement, stays out of such a mint.
//! - [`Log::undecided`]: those Macs with no seat, which only the batch can decide. A writer seals
//!   under its epoch; the tender waits for the batch within its settle grace, a re-key of
//!   another audience for `[everyone]`'s batch too, and no longer, since no record marks a
//!   batch's end: one that left such a Mac out, above its last `Wrap`, reads the same as one
//!   whose `Wrap` to it is still on its way.
//! - [`secure_twins`]: two or more newest epochs with one recipient set and no removed recipient,
//!   which need no coalescing; a writer seals under the lowest id among them.

use super::{Body, EpochAudience, Log, MemberId, Membership, RecordId};
use crate::age::Recipient;
use aterm_digest::Sha256;
use aterm_mesh_core::revocation::{gate_row, head_due, wraps_removed};
use std::collections::{BTreeMap, BTreeSet};

const RANK_DOMAIN: &[u8] = b"aterm/members/rekey-rank/3\0";
const TAG_DOMAIN: &[u8] = b"aterm/members/rekey-audience/1\0";

#[cfg(test)]
thread_local! {
    /// The negative control of [`secure_twins`]: while set, no newest epochs are twins, so
    /// identical concurrent epochs are due a coalesce as every concurrent pair once was.
    pub(crate) static TWINS_DUE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The negative control of [`Log::batch_may_wrap`]: while set, no `Wrap` a held prefix lacks
    /// is in flight, so every one reads as a recipient left out on purpose, as before.
    pub(crate) static PREFIX_BLIND: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The negative control of [`Log::in_flight`]'s seats: while set, a recipient no current seat
    /// binds is in flight too, so a seatless Mac a retirement left out comes back in.
    pub(crate) static SEATLESS_FLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The negative control of [`Log::undecided`]: while set, nothing is undecided, so a
    /// recipient with no seat whose `Wrap` a held prefix lacks reads as left out on purpose
    /// everywhere: a writer mints over it, an overlay drops it and a re-key of another audience
    /// does not wait for `[everyone]`'s batch, as before.
    pub(crate) static UNDECIDED_BLIND: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The negative control of [`RekeyDue::carriers`]: while set, a member that minted every
    /// newest epoch carries the audience with no `Wrap` of it, as it once did.
    pub(crate) static MINTERS_CARRY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// The negative control of settledness: while set, every epoch reads as settled
    /// ([`Log::unsettled`]), so nothing waits on a minter's batch and no `Wrap` is in flight. A
    /// test sets it for its thread; a lab instance turns it on for its whole process with
    /// `unsettled-ok` in `ATERM_TEST_NEGATIVE_CONTROL` ([`unsettled_ok`]).
    pub(crate) static UNSETTLED_OK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The negative control of a batch's frontier ([`Log::batch_whole`]): while set, an epoch
    /// settles only once its minter signs a record other than a `Wrap` after it, as before
    /// one-mesh §11 H2, so a whole batch that ends in `Wrap`s reads as a prefix until the settle
    /// grace. A test sets it for its thread; a lab instance turns it on for its whole process
    /// with `frontier-blind` in `ATERM_TEST_NEGATIVE_CONTROL` ([`frontier_blind`]).
    pub(crate) static FRONTIER_BLIND: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether [`UNSETTLED_OK`] is on: set on this thread, or named for this process
/// (`unsettled-ok`, [`aterm_types::negative_control::on`]).
#[cfg(any(test, debug_assertions))]
pub(crate) fn unsettled_ok() -> bool {
    UNSETTLED_OK.with(std::cell::Cell::get) || aterm_types::negative_control::on("unsettled-ok")
}

/// Whether [`FRONTIER_BLIND`] is on: set on this thread, or named for this process
/// (`frontier-blind`, [`aterm_types::negative_control::on`]).
#[cfg(any(test, debug_assertions))]
pub(crate) fn frontier_blind() -> bool {
    FRONTIER_BLIND.with(std::cell::Cell::get) || aterm_types::negative_control::on("frontier-blind")
}

/// What a log holds of the `Wrap`s of one epoch ([`Log::batch_wraps`]).
struct BatchWraps {
    /// Every recipient a counting `Wrap` of the epoch names, whoever signed it.
    wrapped: BTreeSet<Recipient>,
    /// The highest recipient its minter's `Wrap`s of it in its own batch reach here.
    frontier: Option<Recipient>,
    /// The index of the minter's last record of that batch this log holds: the `Wrap` at the
    /// frontier, or the `Epoch` where it holds none.
    last: usize,
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
    /// The Macs those removers took out: the removals whose rotation the other members wait for.
    pub removals: BTreeSet<MemberId>,
    /// Every Mac an effective `Remove` took out whose recipient a newest epoch still wraps,
    /// whether or not a remover can carry the audience yet. A tender keeps when it first saw
    /// each, so a removal that waits on a remover only once a heal wraps that remover still
    /// counts its grace from when this Mac first saw the `Remove`.
    pub removed: BTreeSet<MemberId>,
    /// The current members that can carry the audience: each holds a counting `Wrap` of every
    /// newest epoch, and its recipient is not removed. A minter that wrapped an epoch only to
    /// others (an admin writing a host list it is not in) holds no key of it.
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

/// Whether `actor` carries the epoch at `at` for having minted it: only under the negative control
/// [`MINTERS_CARRY`].
#[cfg(test)]
fn minter_carries(log: &Log, at: usize, actor: &MemberId) -> bool {
    MINTERS_CARRY.with(std::cell::Cell::get) && log.order[at].actor() == actor
}

#[cfg(not(test))]
const fn minter_carries(_: &Log, _: usize, _: &MemberId) -> bool {
    false
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
        let cache = self.epochs();
        if cache.audiences().is_empty() {
            return Vec::new();
        }
        let membership = self.membership_ref();
        let removed = self.removed_recipients();
        let seats = self.seat_recipients();
        let keyed: Vec<super::Keyed> = self
            .keyed_epochs()
            .into_iter()
            .filter(|k| !k.covered && k.holders.iter().any(|h| membership.contains(h)))
            .collect();
        let mut dues = Vec::new();
        for (audience, sealed) in cache.audiences() {
            let newest_at = &sealed.newest;
            let mut newest: Vec<RecordId> =
                newest_at.iter().map(|&i| *self.order[i].id()).collect();
            newest.sort_unstable();
            // The recipients each newest epoch's counting `Wrap`s name: all a due reads of them.
            let wrapped: BTreeMap<RecordId, BTreeSet<Recipient>> = newest
                .iter()
                .filter_map(|id| Some((*id, cache.wraps().get(id)?.keys().copied().collect())))
                .collect();
            // Whether a newest epoch still wraps `r`, a recipient the log removed: one-mesh proofs
            // P1's `wraps_removed` kernel, the decision `client/log_entries`' `wraps_removed` (a
            // removal's reserve for its own targets) asks too (P6).
            let wraps_revoked = |r: &Recipient| {
                wraps_removed(
                    removed.contains(r),
                    newest
                        .iter()
                        .any(|id| wrapped.get(id).is_some_and(|set| set.contains(r))),
                )
            };
            let rotation = newest
                .iter()
                .any(|id| self.head_due(id, wrapped.get(id), &removed));
            // The Macs whose `Remove` a newest epoch's shut gate lacks ([`Log::gate_misses`]).
            let ungated: BTreeSet<MemberId> = newest
                .iter()
                .flat_map(|id| self.gate_misses(id))
                .filter_map(|at| match self.order[at].body() {
                    Body::Remove { member, .. } => Some(*member),
                    _ => None,
                })
                .collect();
            let coverage = newest.len() > 1 && !secure_twins(&newest, &wrapped, &removed);
            let carriers: BTreeSet<MemberId> = membership
                .members()
                .iter()
                .filter(|(actor, member)| {
                    let recipient = member.binding.recipient();
                    !removed.contains(&recipient)
                        && newest_at.iter().all(|&at| {
                            minter_carries(self, at, actor)
                                || wrapped
                                    .get(self.order[at].id())
                                    .is_some_and(|set| set.contains(&recipient))
                        })
                })
                .map(|(actor, _)| *actor)
                .collect();
            let mut removers: BTreeSet<MemberId> = BTreeSet::new();
            let mut removals: BTreeSet<MemberId> = BTreeSet::new();
            let mut removed_seats: BTreeSet<MemberId> = BTreeSet::new();
            for (target, removal) in membership.removed() {
                if !ungated.contains(target)
                    && !seats
                        .get(target)
                        .is_some_and(|set| set.iter().any(&wraps_revoked))
                {
                    continue;
                }
                removed_seats.insert(*target);
                let carrying: Vec<MemberId> = removal
                    .by
                    .iter()
                    .filter(|by| carriers.contains(*by))
                    .copied()
                    .collect();
                if !carrying.is_empty() {
                    removals.insert(*target);
                    removers.extend(carrying);
                }
            }
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
                removals,
                removed: removed_seats,
                carriers,
                resealers,
                reseal_first,
            });
        }
        dues
    }

    /// Whether the newest epoch `epoch`, whose counting `Wrap`s here name `wrapped`, is due a
    /// rotation, so that no writer seals under it: P1's one rule
    /// (`aterm_mesh_core::revocation::head_due`). Its gate is shut when [`Log::gate_misses`]
    /// names a `Remove`, and it wraps a revoked recipient when one of `removed` (the caller's
    /// [`Log::removed_recipients`]) is in `wrapped`. `rekey_dues`' rotation and a writer's wait
    /// ask this one rule.
    #[must_use]
    pub fn head_due(
        &self,
        epoch: &RecordId,
        wrapped: Option<&BTreeSet<Recipient>>,
        removed: &BTreeSet<Recipient>,
    ) -> bool {
        let wraps_revoked = removed
            .iter()
            .any(|r| wraps_removed(true, wrapped.is_some_and(|set| set.contains(r))));
        head_due(self.gate_misses(epoch).is_empty(), wraps_revoked)
    }

    /// The `Remove`s that keep `epoch`'s gate shut, as indices into this log's order
    /// ([`Log::gate_misses_where`], over the epoch's causal past); every `Remove` that took a
    /// revoking row where this log does not hold `epoch`.
    fn gate_misses(&self, epoch: &RecordId) -> BTreeSet<usize> {
        let epoch_at = self.held.get(epoch).copied();
        let effective = self.effective();
        self.gate_misses_where(|at| epoch_at.is_some_and(|e| effective.before(at, e)))
    }

    /// The `Remove`s that keep an epoch's gate shut, where `in_past(at)` says whether the record
    /// at index `at` of this log's order lies in the epoch's causal past: for each revoked seat
    /// row ([`Membership::revoking_rows`]) whose gate row is shut
    /// (`aterm_mesh_core::revocation::gate_row`: that past holds neither the row's entry and
    /// every `Remove` that took it, nor, while no live row binds its recipient, the row's entry
    /// and one `Remove` that takes it by itself,
    /// [`RevokingRow::alone`](super::eval::RevokingRow::alone)), the `Remove`s the past lacks,
    /// and every one of them where it lacks the row. Empty exactly when the gate is open.
    ///
    /// The row's entry counts as the `Remove`s do (one-mesh proofs P1b, `gateFix`): a Mac that
    /// holds the epoch but not the row (a seat moved onto a co-holder's recipient beside its
    /// `Remove`) reads that recipient as current and wraps the epoch to it. A quorum's share
    /// that lies in the row's entry's past takes the row only where the view holds enough shares
    /// to accept the quorum, which a Mac that holds the epoch need not, and an Undo's share that
    /// a Mac the Undo's target removed signed is no removal there: neither opens a gate alone.
    pub(super) fn gate_misses_where(&self, in_past: impl Fn(usize) -> bool) -> BTreeSet<usize> {
        #[cfg(any(test, debug_assertions))]
        if aterm_types::negative_control::on("held-wrap-gate") {
            return BTreeSet::new();
        }
        // NEGATIVE CONTROL `rowless-gate`: ask for the `Remove`s alone, as the gate before P1b.
        // NEGATIVE CONTROL `quorum-some-gate`: let any `Remove` that took the row, a quorum's
        // share included, open the one-`Remove` clause, as before P1b.
        #[cfg(any(test, debug_assertions))]
        let (rowless, shares) = (
            aterm_types::negative_control::on("rowless-gate"),
            aterm_types::negative_control::on("quorum-some-gate"),
        );
        #[cfg(not(any(test, debug_assertions)))]
        let (rowless, shares) = (false, false);
        let mut misses = BTreeSet::new();
        for row in self.membership_ref().revoking_rows() {
            let entered = rowless || self.held.get(&row.entry).is_some_and(|&at| in_past(at));
            let takers: Vec<(usize, bool, bool)> = row
                .removes
                .iter()
                .filter_map(|id| {
                    let at = *self.held.get(id)?;
                    Some((at, in_past(at), shares || row.alone.contains(id)))
                })
                .collect();
            let every = takers
                .iter()
                .fold(true, |all, &(_, before, _)| all && before);
            let some = takers
                .iter()
                .fold(false, |any, &(_, before, alone)| any || (before && alone));
            if !gate_row(true, entered && every, row.recipient_live, entered && some) {
                misses.extend(
                    takers
                        .iter()
                        .filter(|&&(_, before, _)| !entered || !before)
                        .map(|&(at, _, _)| at),
                );
            }
        }
        misses
    }

    /// Whether an epoch whose parents are `parents` would have its gate open in this log
    /// ([`Log::gate_misses_where`] over the records `parents` reach): what a fresh epoch's own
    /// batch, which seals under it without asking [`Log::head_due`], needs of its draft.
    pub(super) fn gate_opens_after(&self, parents: &[RecordId]) -> bool {
        let from = self.indexes(parents);
        self.gate_misses_where(|at| self.reach.in_past_of(at, &from))
            .is_empty()
    }

    /// The epochs of `epochs` whose minter's batch this log may hold only a prefix of: it holds
    /// no record its minter signed after the batch's own records of it (a later `Epoch`, `Put`,
    /// a `Wrap` of another epoch or of a later batch: each record follows its signer's prior one,
    /// so any of them puts the whole batch here), and cannot tell from the `Wrap`s it holds that
    /// the batch is whole ([`Log::batch_whole`]). An epoch it does not hold is not named.
    #[must_use]
    pub fn unsettled(&self, epochs: &[RecordId]) -> Vec<RecordId> {
        #[cfg(any(test, debug_assertions))]
        if unsettled_ok() {
            return Vec::new();
        }
        // Under the control, as before H2: only a record other than a `Wrap` settles.
        #[cfg(any(test, debug_assertions))]
        let blind = frontier_blind();
        #[cfg(not(any(test, debug_assertions)))]
        let blind = false;
        let effective = self.effective();
        epochs
            .iter()
            .copied()
            .filter(|id| {
                let Some(&at) = self.held.get(id) else {
                    return false;
                };
                let epoch = &self.order[at];
                let signed_again = self
                    .order
                    .iter()
                    .enumerate()
                    .skip(at + 1)
                    .any(|(i, record)| {
                        record.actor() == epoch.actor()
                            && effective.before(at, i)
                            && match record.body() {
                                Body::Wrap { epoch: of, .. } => {
                                    !blind
                                        && (of != id || record.authored_ms() != epoch.authored_ms())
                                }
                                _ => true,
                            }
                    });
                !signed_again && (blind || !self.batch_whole(at))
            })
            .collect()
    }

    /// Whether this log holds every `Wrap` of the batch that minted the epoch at `at`, though no
    /// record marks a batch's end and its minter signed nothing here since (one-mesh §11 H2):
    /// the epoch is of a seat audience (`[everyone]`, Commons), and every recipient that batch
    /// could wrap ([`Log::batch_could_wrap`]) is wrapped here or sorts below the highest
    /// recipient its minter's `Wrap`s reach here (its frontier). A batch wraps in ascending
    /// recipient order, so the rest of it can wrap only a recipient above the frontier, and
    /// there is none it could. A seat below the frontier that no `Wrap` hands the epoch was left
    /// out on purpose, so its gap heals at once rather than after the settle grace. A host list's
    /// batch wraps the Macs its text names, which no record before its `Put`s carries, so it
    /// settles only when its minter signs again.
    fn batch_whole(&self, at: usize) -> bool {
        let Body::Epoch { audience, .. } = self.order[at].body() else {
            return false;
        };
        if matches!(audience, EpochAudience::Hosts(_)) {
            return false;
        }
        let Some(batch) = self.batch_wraps(at) else {
            return true;
        };
        self.batch_could_wrap(at, batch.last)
            .iter()
            .all(|r| batch.wrapped.contains(r) || batch.frontier.is_some_and(|last| *r < last))
    }

    /// What this log holds of the `Wrap`s of the epoch at `at` ([`BatchWraps`]); `None` where its
    /// minter wrapped it in a later batch too (a `Wrap` of it authored at another time), so the
    /// batch that minted it is whole here.
    fn batch_wraps(&self, at: usize) -> Option<BatchWraps> {
        let record = &self.order[at];
        let (epoch, minter) = (record.id(), record.actor());
        let effective = self.effective();
        let mut out = BatchWraps {
            wrapped: BTreeSet::new(),
            frontier: None,
            last: at,
        };
        // A `Wrap` follows its epoch, which it pins in its causal past.
        for (i, other) in self.order.iter().enumerate().skip(at + 1) {
            if !effective.counts(i) {
                continue;
            }
            let Body::Wrap {
                epoch: of,
                recipient,
                ..
            } = other.body()
            else {
                continue;
            };
            if of != epoch {
                continue;
            }
            out.wrapped.insert(*recipient);
            if other.actor() == minter {
                if other.authored_ms() != record.authored_ms() {
                    return None;
                }
                if out.frontier < Some(*recipient) {
                    out.frontier = Some(*recipient);
                    out.last = i;
                }
            }
        }
        Some(out)
    }

    /// The recipients the batch that minted the epoch at `at` could wrap, read from the causal
    /// past of `last`, its minter's last record of that batch this log holds (a draft follows at
    /// most [`crate::envelope::MAX_PARENTS`] heads and the signer's next record the rest, so that
    /// past holds what the minter knew): each seat the genesis or an `Admit` there binds, unless
    /// a `Remove` there took its member out and this log reads that removal effective (a mint
    /// wraps the current seats its minter knows, one-mesh M1 and M4), and each recipient some
    /// causally newest epoch of the audience before it wraps there ([`Log::heads_before`]),
    /// seated or not, which bounds what a 0.154.0 Mac's intersection carry and a waiting write's
    /// union carry it from. A seat its minter could not know is no recipient of its batch: it is a
    /// gap of the epoch, wherever it sorts. A key no seat binds and no causally newest earlier
    /// epoch of the audience wraps (a `[machines]` key a person's edit lets in) is not named: no record here says the
    /// batch may wrap it, so nothing reads its missing `Wrap` either way, and a re-key's carry,
    /// which takes seats alone, leaves it out wherever the batch reads whole.
    fn batch_could_wrap(&self, at: usize, last: usize) -> BTreeSet<Recipient> {
        let seats = self.seat_recipients();
        let effective = self.effective();
        let membership = self.membership_ref();
        let mut bound: BTreeMap<MemberId, BTreeSet<Recipient>> = BTreeMap::new();
        if let Body::Genesis { founders, .. } = self.genesis().body() {
            for founder in founders {
                if let Some(actor) = founder.member() {
                    bound.entry(actor).or_default().insert(founder.recipient);
                }
            }
        }
        let mut gone: BTreeSet<MemberId> = BTreeSet::new();
        for (i, record) in self.order.iter().enumerate().take(last).skip(1) {
            if !effective.before(i, last) {
                continue;
            }
            match record.body() {
                Body::Admit { entrant, .. } => {
                    bound
                        .entry(entrant.member())
                        .or_default()
                        .insert(entrant.request.binding().recipient());
                }
                Body::Remove { member, .. } if membership.removed().contains_key(member) => {
                    gone.insert(*member);
                }
                _ => {}
            }
        }
        let mut could: BTreeSet<Recipient> = bound
            .into_iter()
            .filter(|(member, _)| !gone.contains(member))
            .flat_map(|(_, set)| set)
            .collect();
        could.extend(self.heads_before(at, last, &seats).into_iter().flatten());
        could
    }

    /// What the minter of the epoch at `at` had seen of its audience's causally newest epochs
    /// before it, read from the causal past of the record at `upto` (the epoch itself, or its
    /// minter's last `Wrap` of it, whose past also holds the heads the epoch's draft left out):
    /// for each such epoch, not `at`'s and not after it, the recipients its counting `Wrap`s there
    /// name, less each one a `Remove` there took out (`seats`: each recipient a seat ever bound,
    /// by member), but an Undo's share that a Mac the Undo's target removed signed, which is no
    /// removal to a minter that holds too few of its quorum's shares. Empty for an audience's
    /// first epoch.
    fn heads_before(
        &self,
        at: usize,
        upto: usize,
        seats: &BTreeMap<MemberId, BTreeSet<Recipient>>,
    ) -> Vec<BTreeSet<Recipient>> {
        let Body::Epoch { audience, .. } = self.order[at].body() else {
            return Vec::new();
        };
        let effective = self.effective();
        // A `Remove` before the epoch its minter read as one: any that counts here but an Undo's
        // share, which a minter holding too few of its quorum's shares reads as no removal.
        // NEGATIVE CONTROL `removed-before-unauthorized`: every counted `Remove`, an Undo's share
        // included. NEGATIVE CONTROL `removed-before-plain`: only one authorized with nothing
        // void, which leaves out every `Remove` a Mac an Undo restored signs.
        #[cfg(any(test, debug_assertions))]
        let known = {
            let unauthorized = aterm_types::negative_control::on("removed-before-unauthorized");
            let plain_only = aterm_types::negative_control::on("removed-before-plain");
            move |i: usize| {
                if unauthorized {
                    effective.counts(i)
                } else if plain_only {
                    effective.counts_plain(i)
                } else {
                    effective.counts_where_held(i)
                }
            }
        };
        #[cfg(not(any(test, debug_assertions)))]
        let known = |i: usize| effective.counts_where_held(i);
        let mut before: Vec<usize> = Vec::new();
        let mut wraps_of: BTreeMap<RecordId, BTreeSet<Recipient>> = BTreeMap::new();
        let mut removed_before: BTreeSet<Recipient> = BTreeSet::new();
        for (i, other) in self.order.iter().enumerate().take(upto).skip(1) {
            if !effective.counts(i) || !effective.before(i, upto) {
                continue;
            }
            match other.body() {
                Body::Wrap {
                    epoch: of,
                    recipient,
                    ..
                } => {
                    wraps_of.entry(*of).or_default().insert(*recipient);
                }
                Body::Epoch { audience: aud, .. }
                    if aud == audience && i != at && !effective.before(at, i) =>
                {
                    before.push(i);
                }
                Body::Remove { member, .. } if known(i) => {
                    removed_before.extend(seats.get(member).into_iter().flatten().copied());
                }
                _ => {}
            }
        }
        effective
            .newest(&before, |&b| b)
            .into_iter()
            .map(|&b| {
                wraps_of
                    .get(self.order[b].id())
                    .map(|set| set.difference(&removed_before).copied().collect())
                    .unwrap_or_default()
            })
            .collect()
    }

    /// What the minter of the epoch at `at` could carry on by the 0.154.0 intersection carry:
    /// each recipient every causally newest epoch of its audience before it wraps by a counting
    /// `Wrap` in its causal past, that no `Remove` before it took out ([`Log::heads_before`]).
    /// Empty for an audience's first epoch.
    fn carry_on(
        &self,
        at: usize,
        seats: &BTreeMap<MemberId, BTreeSet<Recipient>>,
    ) -> BTreeSet<Recipient> {
        let mut heads = self.heads_before(at, at, seats).into_iter();
        let first = heads.next().unwrap_or_default();
        heads.fold(first, |held, set| {
            held.intersection(&set).copied().collect()
        })
    }
    /// Each recipient a seat ever bound, by member, from the genesis and every `Admit`, the Macs
    /// now out included.
    fn seat_recipients(&self) -> BTreeMap<MemberId, BTreeSet<Recipient>> {
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
        seats
    }

    /// What the rest of `epoch`'s batch may still wrap, where this log may hold only a prefix of
    /// it ([`Log::unsettled`]) and its minter is not the Mac whose recipient is `mine`: each
    /// recipient its minter could carry on ([`Log::carry_on`]: every causally newest epoch of its
    /// audience before it wraps it by a `Wrap` in its causal past, and no `Remove` before it took
    /// it out, an Undo's share that a Mac the Undo's target removed signed being no removal to a
    /// minter that holds too few of its quorum's shares), that no held `Wrap` hands it, and that
    /// sorts above every recipient its minter's `Wrap`s of it reach here (a batch wraps in
    /// ascending recipient order, so a recipient below one of them that it lacks was left out on
    /// purpose). Empty for an epoch this log does not hold or that settled, for one the Mac
    /// holding `mine` minted (its batch is whole here), for one whose minter's later batch
    /// wrapped it too (so its own is whole), and for an audience's first epoch. A recipient with
    /// no seat is named too, so this bounds what sealing under `epoch` may reach;
    /// [`Log::in_flight`] carries only the seated ones.
    #[must_use]
    pub fn batch_may_wrap(&self, epoch: &RecordId, mine: &Recipient) -> BTreeSet<Recipient> {
        let none = BTreeSet::new();
        #[cfg(test)]
        if PREFIX_BLIND.with(std::cell::Cell::get) {
            return none;
        }
        let Some(&at) = self.held.get(epoch) else {
            return none;
        };
        let record = &self.order[at];
        if !matches!(record.body(), Body::Epoch { .. }) {
            return none;
        }
        let seats = self.seat_recipients();
        if seats
            .get(record.actor())
            .is_some_and(|set| set.contains(mine))
            || self.unsettled(&[*epoch]).is_empty()
        {
            return none;
        }
        let Some(batch) = self.batch_wraps(at) else {
            return none;
        };
        self.carry_on(at, &seats)
            .into_iter()
            .filter(|r| !batch.wrapped.contains(r) && batch.frontier.is_none_or(|last| *r > last))
            .collect()
    }

    /// The recipients the causally newest epochs of `audience` leave out only while a batch may
    /// still be arriving, for the Mac whose recipient is `mine`: some newest epoch's batch may
    /// still wrap each ([`Log::batch_may_wrap`]), every newest epoch either wraps it or may
    /// still, and a current seat whose node key the log has not removed binds it. A mint carries
    /// them rather than read their missing `Wrap`s as left out on purpose ([`Log::left_out`]); a
    /// newest epoch that settled without one keeps it out. A recipient with no seat (a
    /// `[machines]`-only key, an unproved founder) is never in flight: a retirement leaves it out
    /// with no `Remove`, so its missing `Wrap` is the only record of that, while a seated Mac
    /// leaves by a `Remove`, a removed node key or a roster row each caller reads. Those are
    /// [`Log::undecided`].
    #[must_use]
    pub fn in_flight(&self, audience: &EpochAudience, mine: &Recipient) -> BTreeSet<Recipient> {
        let pending = self.pending(audience, mine);
        if pending.is_empty() {
            return pending;
        }
        let membership = self.membership_ref();
        let seated: BTreeSet<Recipient> = membership
            .members()
            .values()
            .filter(|member| !membership.member_node_removed(member))
            .map(|member| member.binding.recipient())
            .collect();
        #[cfg(test)]
        let seatless_fly = SEATLESS_FLY.with(std::cell::Cell::get);
        #[cfg(not(test))]
        let seatless_fly = false;
        pending
            .into_iter()
            .filter(|r| seatless_fly || seated.contains(r))
            .collect()
    }

    /// The recipients the causally newest epochs of `audience` leave out only while a batch may
    /// still be arriving, as [`Log::in_flight`] reads them, that no current seat binds and the
    /// log has not removed: a `[machines]`-only key or an unproved founder whose `Wrap` the rest
    /// of a batch may bring or may have left out on purpose (a retirement), which nothing here
    /// can tell apart until the batch settles. A mint over such a prefix decides for good: one
    /// that carries the recipient re-admits a retired key, one that drops it leaves a kept one
    /// out once its own epoch is the newest. So a writer seals under the epoch whose batch
    /// decides it instead of minting, an overlay keeps the machine in its document, and the
    /// tender holds a re-key of the audience, and of every other audience while `[everyone]`
    /// names one (a re-key reads `[everyone]`'s leave-out for each), for its settle grace. No
    /// record marks a batch's end (one whose last audience its minter does not open whole ends
    /// in `Wrap`s), so a whole batch that left the recipient out, which sorts above the batch's
    /// last `Wrap` ([`Log::batch_whole`]), reads like this until its minter signs again: a re-key
    /// past the grace, like any mint that cannot wait (a person's removal, a writer's fresh
    /// epoch), leaves it out, as a retirement would.
    #[must_use]
    pub fn undecided(&self, audience: &EpochAudience, mine: &Recipient) -> BTreeSet<Recipient> {
        // Under SEATLESS_FLY every such recipient is in flight instead.
        #[cfg(test)]
        if UNDECIDED_BLIND.with(std::cell::Cell::get) || SEATLESS_FLY.with(std::cell::Cell::get) {
            return BTreeSet::new();
        }
        let pending = self.pending(audience, mine);
        if pending.is_empty() {
            return pending;
        }
        let removed = self.removed_recipients();
        let membership = self.membership_ref();
        // Every recipient in flight is bound by one of these seats.
        let seats: BTreeSet<Recipient> = membership
            .members()
            .values()
            .map(|member| member.binding.recipient())
            .collect();
        pending
            .into_iter()
            .filter(|r| !removed.contains(r) && !seats.contains(r))
            .collect()
    }

    /// The causally newest counting epochs of `audience`, by id.
    #[must_use]
    pub fn newest_epochs(&self, audience: &EpochAudience) -> Vec<RecordId> {
        let mut newest: Vec<RecordId> = self
            .epochs()
            .newest(audience)
            .iter()
            .map(|&i| *self.order[i].id())
            .collect();
        newest.sort_unstable();
        newest
    }

    /// The recipients some causally newest epoch of `audience` may still wrap
    /// ([`Log::batch_may_wrap`] for `mine`) and that every newest epoch wraps or may still wrap,
    /// seated or not.
    fn pending(&self, audience: &EpochAudience, mine: &Recipient) -> BTreeSet<Recipient> {
        let newest = self.newest_epochs(audience);
        if newest.is_empty() {
            return BTreeSet::new();
        }
        let effective = self.effective();
        let mut wrapped: BTreeMap<RecordId, BTreeSet<Recipient>> = BTreeMap::new();
        for (i, record) in self.order.iter().enumerate().skip(1) {
            if effective.counts(i)
                && let Body::Wrap {
                    epoch, recipient, ..
                } = record.body()
                && newest.contains(epoch)
            {
                wrapped.entry(*epoch).or_default().insert(*recipient);
            }
        }
        let may: Vec<BTreeSet<Recipient>> = newest
            .iter()
            .map(|id| self.batch_may_wrap(id, mine))
            .collect();
        let empty = BTreeSet::new();
        may.iter()
            .flatten()
            .filter(|r| {
                newest.iter().zip(&may).all(|(id, may)| {
                    may.contains(r) || wrapped.get(id).unwrap_or(&empty).contains(r)
                })
            })
            .copied()
            .collect()
    }
}
