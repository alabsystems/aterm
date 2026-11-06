// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What any member signs when a person's Allow and the phrase let a Mac in, and the wrap gaps
//! every member closes (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.2 "Labels", §3.3.6, F15).
//!
//! - [`SeatAudience`]: `[everyone]` and Commons, the audiences every current seat belongs to and
//!   the only ones an admission hands on or a heal closes. A host list names the Macs it is for,
//!   so it has no `SeatAudience`: no seat it leaves out is a gap of it.
//! - [`Log::admission_epochs`]: the epochs an admitting member wraps to the new Mac, in each
//!   audience it names: every epoch that still seals a winning entry, so the new Mac reads
//!   entries sealed before the latest rotation, and the causally newest ones, so it reads and
//!   writes what comes next.
//! - [`Log::admit`]: the `Admit` and those `Wrap`s, signed by the member and held by the log.
//! - [`Log::current_recipients`]: who a rotation and a heal wrap (one-mesh M1,
//!   `docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.2): every seat no `Remove`, quorum `Remove` or
//!   node removal took out, less the seats a caller's roster denies. A recipient with no seat is
//!   no member.
//! - [`Log::wrap_gaps`]: an epoch a current seat lacks (M5). Only a `Remove` revokes: a missing
//!   `Wrap` was not written yet or has not arrived yet, so a current seat lacking a `Wrap` of a
//!   newest epoch is a gap whether or not the epoch's minter knew the seat, and so is a seat
//!   admitted beside the epoch (its entry not in the epoch's causal past) lacking any epoch an
//!   admission hands on. On 2026-11-03 concurrent re-keys over partial batches left Commons
//!   wrapped to 3 of ~88 seats, and the old rule (an omitted seat the minter knew was revoked)
//!   left the other 85 locked out with nothing to heal them.
//! - [`Log::gap_rank`]: which of the current seats holding an epoch closes a gap first, so ~35
//!   Macs that see one gap after a heal do not all sign the same `Wrap`.
//! - [`Log::next_label`]: C11's next free `m<N>` from what the log and the swarm roster hold.

use super::entries::OpenedAudienceEntries;
use super::{
    Body, Entrant, EpochAudience, Evidence, Log, Member, MemberId, Membership, Record, RecordId,
    is_label,
};
use crate::Result;
use crate::age::Recipient;
use aterm_digest::Sha256;
use aterm_mesh_core::gap_heal::{self, gap, heals_omission};
use std::collections::BTreeSet;

/// Epochs of an admission, each with its audience, by audience then id.
type AudienceEpochs = Vec<(EpochAudience, RecordId)>;

const RANK_DOMAIN: &[u8] = b"aterm/members/wrap-gap-rank/1\0";

/// One epoch an effective member lacks a `Wrap` of ([`Log::wrap_gaps`]).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Gap {
    /// The epoch's audience.
    pub audience: EpochAudience,
    /// The `Epoch` record.
    pub epoch: RecordId,
    /// The member that lacks it.
    pub member: MemberId,
    /// That member's age recipient, the one a `Wrap` names.
    pub recipient: Recipient,
    /// When the gap opened, in the records' own clocks: the later of the epoch's and the
    /// member's entry's `authored_ms`. Every Mac reads the same value. The spread of who closes
    /// it counts from when each Mac first sees it, not from this: after a long split every
    /// holder would be past due at once.
    pub opened_ms: u64,
}

/// A member's signing key for [`Log::admit`] and gap `Wrap`s: its actor, writer seed and
/// incarnation.
pub struct Signer<'a> {
    /// The member's actor.
    pub actor: &'a MemberId,
    /// Its Commons writer seed.
    pub seed: &'a [u8; 32],
    /// Its binding's incarnation.
    pub incarnation: [u8; 16],
}

/// An audience every current seat belongs to: `[everyone]`, or F18's Commons. These are the only
/// audiences an admission hands a new Mac ([`Log::admission_epochs`]) and the only ones whose
/// omissions a heal closes ([`Log::wrap_gaps`], [`Log::is_wrap_gap`]). A host list names the Macs
/// it is for, so a seat it does not wrap is no gap of it and no admission hands it on. It has no
/// `SeatAudience` ([`SeatAudience::of`] refuses one): read as one, every seat outside the list
/// would be a gap, and a heal would wrap the list's key to each of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SeatAudience {
    /// Vault `[everyone]` entries.
    Everyone,
    /// The Commons network key and `aterm-commons-control-v1` descriptor (`F18`).
    Commons,
}

impl SeatAudience {
    /// The seat audience `audience` is; `None` for a host list, which names its Macs.
    #[must_use]
    pub const fn of(audience: &EpochAudience) -> Option<Self> {
        match audience {
            EpochAudience::Everyone => Some(Self::Everyone),
            EpochAudience::Commons => Some(Self::Commons),
            EpochAudience::Hosts(_) => None,
        }
    }

    /// The log's name for it.
    #[must_use]
    pub const fn epochs(self) -> EpochAudience {
        match self {
            Self::Everyone => EpochAudience::Everyone,
            Self::Commons => EpochAudience::Commons,
        }
    }
}

impl From<SeatAudience> for EpochAudience {
    fn from(audience: SeatAudience) -> Self {
        audience.epochs()
    }
}

impl Log {
    /// The epochs a member that opened `opened` wraps to a Mac it admits, in each of
    /// `audiences`: those that seal a winning entry and the causally maximal ones, each once,
    /// by audience then id.
    #[must_use]
    pub fn admission_epochs(
        opened: &OpenedAudienceEntries,
        audiences: &[SeatAudience],
    ) -> Vec<(EpochAudience, RecordId)> {
        let mut out = BTreeSet::new();
        for audience in audiences {
            let audience = audience.epochs();
            for epoch in opened
                .winning_epochs
                .get(&audience)
                .into_iter()
                .flatten()
                .chain(opened.maximal_epochs.get(&audience).into_iter().flatten())
            {
                out.insert((audience.clone(), *epoch));
            }
        }
        out.into_iter().collect()
    }

    /// The epochs [`Log::admit_with`] wraps to the Mac it admits, by audience then id: each
    /// [`Log::admission_epochs`] epoch of `audiences` the member that opened `opened` unwrapped.
    #[must_use]
    pub fn admission_wraps(
        opened: &OpenedAudienceEntries,
        audiences: &[SeatAudience],
    ) -> Vec<(EpochAudience, RecordId)> {
        Self::admission_split(opened, audiences).0
    }

    /// [`Log::admission_epochs`] of `audiences`, split into those the member that opened
    /// `opened` unwrapped ([`Log::admission_wraps`]) and those it lacks.
    fn admission_split(
        opened: &OpenedAudienceEntries,
        audiences: &[SeatAudience],
    ) -> (AudienceEpochs, AudienceEpochs) {
        Self::admission_epochs(opened, audiences)
            .into_iter()
            .partition(|(_, epoch)| opened.unwrapped_keys.contains_key(epoch))
    }

    /// Let `entrant` in on a person's Allow and phrase: sign an `Admit` and then a `Wrap` of
    /// every [`Log::admission_epochs`] epoch of `audiences` the signer unwrapped to the entrant's
    /// recipient, insert each, and return them in order. An epoch the signer lacks is allowed
    /// only where it is the signer's own wrap gap ([`Log::is_wrap_gap`]: a Mac admitted on one
    /// side of a split before a holder wrapped it the epoch the other side minted); the new Mac's
    /// entry is not in that epoch's causal past either, so the same holders close it for both.
    /// Any other epoch the signer cannot hand on keeps it from admitting anyone.
    ///
    /// # Errors
    /// The signer holds none of those epochs' keys, or misses one that is no wrap gap of its own,
    /// or drafting, signing or inserting a record fails (the entrant names another network, or
    /// the signer is no member).
    pub fn admit(
        &mut self,
        signer: &Signer<'_>,
        entrant: Entrant,
        opened: &OpenedAudienceEntries,
        audiences: &[SeatAudience],
        authored_ms: u64,
    ) -> Result<Vec<Record>> {
        self.admit_with(
            signer,
            entrant,
            Evidence::Phrase,
            opened,
            audiences,
            authored_ms,
        )
    }

    /// [`Log::admit`] under `evidence`: [`Evidence::Manual`] where no phrase was run, as a
    /// member's re-admission of a Mac that proved its fleet node key or the fleet's key
    /// (`client::readmit`). A 0.147 reader takes an `Admit` of any of the three evidences from
    /// any member, so this signs no record an older Mac refuses.
    ///
    /// # Errors
    /// As [`Log::admit`].
    pub fn admit_with(
        &mut self,
        signer: &Signer<'_>,
        entrant: Entrant,
        evidence: Evidence,
        opened: &OpenedAudienceEntries,
        audiences: &[SeatAudience],
        authored_ms: u64,
    ) -> Result<Vec<Record>> {
        let (epochs, lacking) = Self::admission_split(opened, audiences);
        if epochs.is_empty() {
            return Err(crate::Error::new(
                "this Mac holds no epoch to hand a new Mac",
            ));
        }
        if lacking
            .iter()
            .any(|(_, epoch)| !self.is_wrap_gap(opened, epoch, signer.actor))
        {
            return Err(crate::Error::new(
                "this Mac cannot open every epoch a new Mac needs",
            ));
        }
        let recipient = entrant.request.binding().recipient();
        let mut signed = Vec::with_capacity(epochs.len() + 1);
        let admit = self
            .draft(signer.actor, authored_ms, Body::Admit { entrant, evidence })
            .sign(signer.seed, signer.incarnation)?;
        self.insert(admit.clone())?;
        signed.push(admit);
        for (_, epoch) in epochs {
            let key = &opened.unwrapped_keys[&epoch];
            let wrap = self
                .draft_wrap(signer.actor, authored_ms, epoch, key, recipient)?
                .sign(signer.seed, signer.incarnation)?;
            self.insert(wrap.clone())?;
            signed.push(wrap);
        }
        Ok(signed)
    }

    /// The age recipients of every current seat (one-mesh M1): each seat of
    /// [`Log::membership`] (no effective `Remove` or quorum `Remove` names it) whose node key or
    /// writer key no node removal took out and whose recipient is no removed Mac's. A recipient
    /// no seat binds (one let in through the envelope alone, an unproved founder) is not here. A
    /// host list's recipients are those of these seats that the list names, which its epochs
    /// and the vault's machines say, not the log; `audience` is the caller's, so every audience
    /// reads one answer.
    #[must_use]
    pub fn current_recipients(&self, audience: &EpochAudience) -> BTreeSet<Recipient> {
        self.current_recipients_where(audience, |_| false)
    }

    /// [`Log::current_recipients`] less every seat `denied` names: until each swarm-roster deny is
    /// a `Remove` (one-mesh wave D), the client passes its roster's deny here.
    #[must_use]
    pub fn current_recipients_where(
        &self,
        _audience: &EpochAudience,
        denied: impl Fn(&Member) -> bool,
    ) -> BTreeSet<Recipient> {
        let membership = self.membership();
        membership
            .members()
            .values()
            .filter(|seat| self.seat_is_current(&membership, seat) && !denied(seat))
            .map(|seat| seat.binding.recipient())
            .collect()
    }

    /// Whether `seat`, a seat of `membership`, is current: no node removal took its node key or
    /// writer key out and its recipient is no removed Mac's.
    fn seat_is_current(&self, membership: &Membership, seat: &Member) -> bool {
        !membership.member_node_removed(seat)
            && !self.recipient_is_removed(&seat.binding.recipient())
    }

    /// The wrap gaps in `audiences` that the member that opened `opened` can close (M5), read with
    /// no roster: [`Log::wrap_gaps_denying`] with no seat denied.
    ///
    /// `audiences` are [`SeatAudience`]s, so a host list, which names its Macs, cannot be asked:
    /// read as a seat audience it would name every seat outside the list.
    ///
    /// ```
    /// use aterm_secrets::membership_log::{Gap, Log, OpenedAudienceEntries, SeatAudience};
    /// fn gaps(log: &Log, opened: &OpenedAudienceEntries) -> Vec<Gap> {
    ///     log.wrap_gaps(opened, &[SeatAudience::Everyone, SeatAudience::Commons])
    /// }
    /// ```
    ///
    /// ```compile_fail
    /// use aterm_secrets::membership_log::{EpochAudience, Gap, Log, OpenedAudienceEntries};
    /// fn gaps(log: &Log, opened: &OpenedAudienceEntries) -> Vec<Gap> {
    ///     log.wrap_gaps(opened, &[EpochAudience::Hosts(vec!["m7".to_owned()])])
    /// }
    /// ```
    #[must_use]
    pub fn wrap_gaps(
        &self,
        opened: &OpenedAudienceEntries,
        audiences: &[SeatAudience],
    ) -> Vec<Gap> {
        self.wrap_gaps_denying(opened, audiences, |_| false)
    }

    /// The wrap gaps in `audiences` that the member that opened `opened` can close (M5,
    /// one-mesh proofs P2, `aterm_mesh_core::gap_heal::gap`): for each epoch of
    /// [`Log::admission_epochs`] it unwrapped, each current seat ([`Log::current_recipients`])
    /// with no effective `Wrap` of it where either
    ///
    /// - the epoch heals omissions (`heals_omissions`): it is a causally newest one of its
    ///   audience, and it wraps no removed Mac or no carrier holds every newest epoch there.
    ///   Whether or not its minter knew the seat, a missing `Wrap` is never a revocation; or
    /// - the seat's entry (its `Admit`, or the genesis) is not in the epoch's causal past, so the
    ///   minter could not have known it: a Mac admitted beside the rotation, which needs every
    ///   epoch an admission hands on.
    ///
    /// Only an explicit `Remove` (or a node removal) takes a seat out, so a removed Mac is never
    /// named. `denied` is the client's one deny (both rosters): it decides who can carry a re-key,
    /// and the client drops the gaps of the seats it denies. Whether the epoch's batch has settled
    /// is the client's hold (`close_wrap_gaps`), not this. With [`old_gap_rule`] on, the rule of
    /// 0.154.0 and before: no omission the minter knew is a gap, and a seat any newest epoch
    /// leaves out ([`Log::left_out`]) is skipped.
    #[must_use]
    pub fn wrap_gaps_denying(
        &self,
        opened: &OpenedAudienceEntries,
        audiences: &[SeatAudience],
        denied: impl Fn(&Member) -> bool,
    ) -> Vec<Gap> {
        let old = old_gap_rule();
        let effective = self.effective();
        let membership = self.membership();
        let removed = self.removed_recipients();
        let mut gaps = Vec::new();
        for (audience, epoch) in Self::admission_epochs(opened, audiences) {
            let opens = opened.unwrapped_keys.contains_key(&epoch);
            if !opens {
                continue;
            }
            let Some(&epoch_at) = self.held.get(&epoch) else {
                continue;
            };
            let wrapped = opened.wrapped_recipients.get(&epoch);
            let heals = !old
                && self.heals_omissions(opened, &membership, &audience, &epoch, &removed, &denied);
            for (actor, member) in membership.members() {
                if old
                    && (self.left_out(opened, &audience, actor)
                        || self.left_out(opened, &EpochAudience::Everyone, actor))
                {
                    continue;
                }
                let recipient = member.binding.recipient();
                let Some(&entry_at) = self.held.get(&member.entry) else {
                    continue;
                };
                let known = entry_at == epoch_at || effective.before(entry_at, epoch_at);
                if !gap(
                    opens,
                    old || self.seat_is_current(&membership, member),
                    wrapped.is_some_and(|set| set.contains(&recipient)),
                    known,
                    heals,
                ) {
                    continue;
                }
                let opened_ms = self.order[epoch_at]
                    .authored_ms()
                    .max(self.order[entry_at].authored_ms());
                gaps.push(Gap {
                    audience: audience.clone(),
                    epoch,
                    member: *actor,
                    recipient,
                    opened_ms,
                });
            }
        }
        gaps.sort();
        gaps
    }

    /// Whether a current seat `epoch` omits is a gap of it whatever its minter knew (one-mesh
    /// proofs P2, `aterm_mesh_core::gap_heal::heals_omission`): `epoch` is a causally newest epoch
    /// of `audience`, and it wraps no removed Mac or no carrier holds every newest epoch there (a
    /// current seat `denied` does not name, which may mint the audience's next epoch). An older
    /// epoch is handed on only to a Mac admitted beside it. A newest one that wraps a removed Mac
    /// is due a re-key, which wraps every current seat, so with a carrier a heal would only add
    /// records; with none, the heal is how a carrier comes to be. The rule before PF2 skipped
    /// every newest epoch that wrapped a removed Mac, so two concurrent heads that each did, with
    /// no seat holding both, could neither heal nor re-key.
    fn heals_omissions(
        &self,
        opened: &OpenedAudienceEntries,
        membership: &Membership,
        audience: &EpochAudience,
        epoch: &RecordId,
        removed: &BTreeSet<Recipient>,
        denied: &impl Fn(&Member) -> bool,
    ) -> bool {
        let newest = opened
            .maximal_epochs
            .get(audience)
            .map_or(&[][..], Vec::as_slice);
        let wraps = |at: &RecordId, r: &Recipient| {
            opened
                .wrapped_recipients
                .get(at)
                .is_some_and(|set| set.contains(r))
        };
        let wraps_revoked =
            gap_heal::wraps_revoked(removed.iter().map(|r| (true, wraps(epoch, r))));
        let carried = gap_heal::carrier(membership.members().values().map(|seat| {
            let recipient = seat.binding.recipient();
            (
                self.seat_is_current(membership, seat),
                denied(seat),
                newest.iter().all(|at| wraps(at, &recipient)),
            )
        }));
        // NEGATIVE CONTROL `shipped-heal-rule`: every audience reads as carried, so an omission of
        // an epoch that wraps a removed Mac never heals (the rule before PF2).
        #[cfg(any(test, debug_assertions))]
        let carried = carried || aterm_types::negative_control::on("shipped-heal-rule");
        heals_omission(newest.contains(epoch), wraps_revoked, carried)
    }

    /// Whether `member` lacks `epoch` as a wrap gap ([`Log::wrap_gaps`]): it is a current seat
    /// with no effective `Wrap` of the epoch, and the epoch heals omissions (a newest one of its
    /// audience that wraps no removed Mac, or that no current seat carries: no roster is read
    /// here), or the member's entry (its `Admit`, or the genesis) is not in the epoch's causal
    /// past. Read from `member`'s side, so the caller need not hold the epoch: a
    /// member left out of a newest epoch, or admitted beside a concurrent rotation, asks this of
    /// itself before it admits another Mac, whose seat the same holders heal. A host list's epoch
    /// is no gap of any seat: the list names its Macs ([`SeatAudience`]).
    #[must_use]
    pub fn is_wrap_gap(
        &self,
        opened: &OpenedAudienceEntries,
        epoch: &RecordId,
        member: &MemberId,
    ) -> bool {
        let old = old_gap_rule();
        let membership = self.membership();
        let Some(seat) = membership.get(member) else {
            return false;
        };
        if !old && !self.seat_is_current(&membership, seat) {
            return false;
        }
        let Some(Body::Epoch { audience, .. }) = self.get(epoch).map(Record::body) else {
            return false;
        };
        if SeatAudience::of(audience).is_none() {
            return false;
        }
        if old
            && (self.left_out(opened, audience, member)
                || self.left_out(opened, &EpochAudience::Everyone, member))
        {
            return false;
        }
        if opened
            .wrapped_recipients
            .get(epoch)
            .is_some_and(|set| set.contains(&seat.binding.recipient()))
        {
            return false;
        }
        if !old
            && self.heals_omissions(
                opened,
                &membership,
                audience,
                epoch,
                &self.removed_recipients(),
                &|_: &Member| false,
            )
        {
            return true;
        }
        let (Some(&epoch_at), Some(&entry_at)) = (self.held.get(epoch), self.held.get(&seat.entry))
        else {
            return false;
        };
        entry_at != epoch_at && !self.effective().before(entry_at, epoch_at)
    }

    /// Where `me` stands among the members that can close `gap`: each current seat that holds an
    /// effective `Wrap` of its epoch. The seats the minter's own batch wraps come first, then the
    /// seats a heal or an admission wrapped, each group ordered by
    /// `SHA-256(domain ‖ actor ‖ epoch ‖ recipient)`. Rank 0 closes it at once; rank `k` once it
    /// is still open `k` steps after that member first saw it (the client's `close_wrap_gaps`),
    /// so N holders that see one gap sign one `Wrap`, not N. A seat a heal wraps joins behind the
    /// minter's batch, so the ranks of a heal in progress do not move under it (a newly healed
    /// seat never ranks ahead of a holder already counting its steps), and it still heals once
    /// every holder of the batch is away. A minter that did not wrap its epoch to itself (gen 66's
    /// remover) cannot open it, so it ranks for nothing until a heal wraps it, behind the batch:
    /// ranked first it would hold each of its gaps a step and then, healed, sign beside the next
    /// holder. Whether a newest epoch leaves a holder out of another audience is no reason to
    /// skip it. With [`old_gap_rule`] on, 0.154.0's order: one group, the minter in it, and a
    /// holder a newest epoch leaves out is skipped. `None` when `me` is not one of them.
    #[must_use]
    pub fn gap_rank(
        &self,
        opened: &OpenedAudienceEntries,
        gap: &Gap,
        me: &MemberId,
    ) -> Option<usize> {
        let old = old_gap_rule();
        let membership = self.membership();
        let wrapped = opened.wrapped_recipients.get(&gap.epoch);
        let minter = self.get(&gap.epoch).map(|record| *record.actor());
        let batch: BTreeSet<Recipient> = if old {
            BTreeSet::new()
        } else {
            self.records()
                .filter(|record| Some(*record.actor()) == minter)
                .filter_map(|record| match record.body() {
                    Body::Wrap {
                        epoch, recipient, ..
                    } if *epoch == gap.epoch => Some(*recipient),
                    _ => None,
                })
                .collect()
        };
        let mut holders: Vec<(bool, [u8; 32], MemberId)> = membership
            .members()
            .iter()
            .filter(|(actor, member)| {
                let skipped = if old {
                    self.left_out(opened, &gap.audience, actor)
                        || self.left_out(opened, &EpochAudience::Everyone, actor)
                } else {
                    !self.seat_is_current(&membership, member)
                };
                !skipped
                    && ((old && Some(**actor) == minter)
                        || wrapped.is_some_and(|set| set.contains(&member.binding.recipient())))
            })
            .map(|(actor, member)| {
                let healed = !old && !batch.contains(&member.binding.recipient());
                let mut buf = Vec::with_capacity(RANK_DOMAIN.len() + 160);
                buf.extend_from_slice(RANK_DOMAIN);
                buf.extend_from_slice(actor);
                buf.extend_from_slice(&gap.epoch);
                buf.extend_from_slice(gap.recipient.to_bech32().as_bytes());
                (healed, Sha256::digest(&buf), *actor)
            })
            .collect();
        holders.sort();
        holders.iter().position(|(_, _, actor)| actor == me)
    }

    /// Whether a causally newest epoch of `audience` leaves `member` out on purpose: it holds no
    /// `Wrap` to `member` although `member`'s entry lies in its causal past, so its minter knew
    /// the member and left it out (a rotation that revoked it). An epoch minted concurrently with
    /// the member's entry is a wrap gap ([`Log::wrap_gaps`]), not a leaving out.
    #[must_use]
    pub fn left_out(
        &self,
        opened: &OpenedAudienceEntries,
        audience: &EpochAudience,
        member: &MemberId,
    ) -> bool {
        let membership = self.membership();
        let Some(seat) = membership.get(member) else {
            return true;
        };
        let Some(&entry_at) = self.held.get(&seat.entry) else {
            return true;
        };
        let recipient = seat.binding.recipient();
        let effective = self.effective();
        opened
            .maximal_epochs
            .get(audience)
            .into_iter()
            .flatten()
            .any(|epoch| {
                let wrapped = opened
                    .wrapped_recipients
                    .get(epoch)
                    .is_some_and(|set| set.contains(&recipient));
                let knew = self
                    .held
                    .get(epoch)
                    .is_some_and(|&at| entry_at == at || effective.before(entry_at, at));
                !wrapped && knew
            })
            || self.audience_left_out_recipient(audience, &recipient)
    }

    /// [`Log::left_out`] evaluated directly from the log's effective `Epoch` and `Wrap` records
    /// without decrypting audience entries: also when a newest epoch of `audience` holds no
    /// `Wrap` to `member` although `member`'s own entry lies in its causal past, for any
    /// audience. Taken once per log state ([`epochs`](super::epochs)).
    #[must_use]
    pub fn audience_left_out(&self, audience: &EpochAudience, member: &MemberId) -> bool {
        let verdict = self.epochs().leaves_out_member(self, audience, member);
        #[cfg(test)]
        super::epochs::oracle::check("audience_left_out", verdict, || {
            super::epochs::oracle::audience_left_out(self, audience, member)
        });
        verdict
    }

    /// Whether a causally newest [`Body::Epoch`] of `audience` in this log leaves `recipient` out
    /// on purpose: it holds no [`Body::Wrap`] to `recipient` although `recipient`'s member entry
    /// (for [`EpochAudience::Everyone`] / [`EpochAudience::Commons`]) or a prior [`Body::Wrap`] of
    /// `audience` to `recipient` lies in its causal past. Taken once per log state
    /// ([`epochs`](super::epochs)).
    #[must_use]
    pub fn audience_left_out_recipient(
        &self,
        audience: &EpochAudience,
        recipient: &Recipient,
    ) -> bool {
        let verdict = self
            .epochs()
            .leaves_out_recipient(self, audience, recipient);
        #[cfg(test)]
        super::epochs::oracle::check("audience_left_out_recipient", verdict, || {
            super::epochs::oracle::audience_left_out_recipient(self, audience, recipient)
        });
        verdict
    }

    /// C11's label for a Mac asking under `requested` (`docs/DESIGN-alab-swarm-2026-10-27.md`
    /// C11, held by the log under F15): `requested` itself when it is an `m<N>` that no founder,
    /// no `Admit` this log holds (a removed Mac's included) and nothing in `taken` (the swarm
    /// roster's labels, aliases, claims, deny-list and `[machines]` names, and the reserved
    /// labels) names; otherwise the `m<N>` one above the highest of all of them and of `counted`
    /// (the fleet-accounts sheet's numbers, which hold no label by themselves, as the roster's
    /// `next_free_number` counts them), or, where that would pass `m99999` (the highest an
    /// `Admit` carries, [`is_label`]), the lowest one none of them names. `None` only when every
    /// `m<N>` up to `m99999` is held. Two members that admit two Macs while split can pick one
    /// label; the log's suffix rule settles that after the heal (§3.2).
    #[must_use]
    pub fn next_label(
        &self,
        requested: &str,
        taken: &BTreeSet<String>,
        counted: &BTreeSet<String>,
    ) -> Option<String> {
        let membership = self.membership();
        let held: BTreeSet<&str> = taken
            .iter()
            .chain(membership.claimed_labels())
            .map(String::as_str)
            .collect();
        let lower = requested.to_ascii_lowercase();
        if is_label(&lower) && !held.contains(lower.as_str()) {
            return Some(lower);
        }
        let numbers: BTreeSet<u32> = held
            .iter()
            .copied()
            .chain(counted.iter().map(String::as_str))
            .filter_map(|label| {
                let digits = label.strip_prefix('m')?;
                let end = digits
                    .find(|c: char| !c.is_ascii_digit())
                    .unwrap_or(digits.len());
                digits[..end].parse::<u32>().ok()
            })
            .collect();
        let above = format!(
            "m{}",
            numbers.last().copied().unwrap_or(0).saturating_add(1)
        );
        if is_label(&above) {
            return Some(above);
        }
        (1..=99_999_u32)
            .find(|n| !numbers.contains(n))
            .map(|n| format!("m{n}"))
    }
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// The gap rule's negative control (one-mesh §9 `GapHeal`, A3's `OLD_GAP_RULE`): while set,
    /// [`Log::wrap_gaps`], [`Log::is_wrap_gap`] and [`Log::gap_rank`] read a missing `Wrap` of a
    /// newest epoch as 0.154.0 did, a revocation when the minter knew the seat, so the 88-to-3
    /// replay leaves the omitted seats out. A test sets it for its thread; a lab instance turns
    /// it on for its whole process with `old-gap-rule` in `ATERM_TEST_NEGATIVE_CONTROL`
    /// ([`aterm_types::negative_control::on`]). It sits beside the rule it restores, which reads
    /// it, rather than beside `rekey`'s controls.
    pub static OLD_GAP_RULE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether [`OLD_GAP_RULE`] is on: set on this thread, or named for this process
/// (`old-gap-rule`); never in a release build, which has neither switch.
#[must_use]
pub fn old_gap_rule() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        OLD_GAP_RULE.with(std::cell::Cell::get) || aterm_types::negative_control::on("old-gap-rule")
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

/// The audiences an admitting member wraps (§3.3.6): `[everyone]`, and F18's `commons` (the
/// Commons network key and control descriptor, [`EpochAudience::Commons`]) when the log holds a
/// Commons epoch, so the new Mac takes part in Commons control from the log. Host lists stay
/// closed: they name the Macs they are for.
#[must_use]
pub fn admission_audiences(opened: &OpenedAudienceEntries) -> Vec<SeatAudience> {
    let mut out = vec![SeatAudience::Everyone];
    if opened
        .maximal_epochs
        .get(&EpochAudience::Commons)
        .is_some_and(|epochs| !epochs.is_empty())
    {
        out.push(SeatAudience::Commons);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SecretBytes;
    use crate::age::Identity;
    use crate::commons::binding::Request;
    use crate::text::{Audience, Entry};
    use zeroize::Zeroizing;

    const NETWORK: [u8; 16] = [0x51; 16];
    const AUTHORITY: [u8; 32] = [0x52; 32];

    /// A synthetic Mac: its writer seed, incarnation, age identity, actor and entrant.
    struct Mac {
        seed: [u8; 32],
        incarnation: [u8; 16],
        identity: Identity,
        actor: MemberId,
        entrant: Entrant,
    }

    impl Mac {
        fn new(tag: u8, label: &str) -> Self {
            let seed = [tag; 32];
            let incarnation = [tag; 16];
            let identity = Identity::from_secret_bytes([tag.wrapping_add(0x40); 32]);
            let request = Request::create(
                NETWORK,
                AUTHORITY,
                [tag.wrapping_add(0x20); 16],
                incarnation,
                &identity,
                &seed,
            )
            .expect("request");
            let actor = request.binding().actor();
            Self {
                seed,
                incarnation,
                actor,
                entrant: Entrant {
                    request,
                    node_key: [tag.wrapping_add(0x80); 32],
                    name: format!("mac-{tag}"),
                    label: label.to_owned(),
                },
                identity,
            }
        }

        fn signer(&self) -> Signer<'_> {
            Signer {
                actor: &self.actor,
                seed: &self.seed,
                incarnation: self.incarnation,
            }
        }

        fn sign(&self, log: &mut Log, ms: u64, body: Body) -> Record {
            let record = log
                .draft(&self.actor, ms, body)
                .sign(&self.seed, self.incarnation)
                .expect("sign");
            log.insert(record.clone()).expect("insert");
            record
        }

        /// Mint an `[everyone]` epoch at `generation`, wrap it to each of `to`, and return its
        /// id and key.
        fn mint(
            &self,
            log: &mut Log,
            ms: u64,
            generation: u64,
            to: &[&Mac],
        ) -> (RecordId, Zeroizing<[u8; 32]>) {
            let (draft, key) = log
                .draft_epoch(&self.actor, ms, Audience::Everyone, generation)
                .expect("epoch");
            let epoch = draft.sign(&self.seed, self.incarnation).expect("sign");
            let id = *epoch.id();
            log.insert(epoch).expect("insert epoch");
            for mac in to {
                let wrap = log
                    .draft_wrap(&self.actor, ms, id, &key, mac.identity.recipient())
                    .expect("wrap")
                    .sign(&self.seed, self.incarnation)
                    .expect("sign");
                log.insert(wrap).expect("insert wrap");
            }
            (id, key)
        }

        fn put(
            &self,
            log: &mut Log,
            ms: u64,
            epoch: (RecordId, &[u8; 32]),
            name: &str,
            value: &[u8],
        ) {
            let entry = Entry {
                name: name.to_owned(),
                value: SecretBytes::from(value),
                audience: Audience::Everyone,
                if_absent: false,
                anchor: None,
            };
            let put = log
                .draft_put(&self.actor, ms, epoch.0, epoch.1, &entry)
                .expect("put")
                .sign(&self.seed, self.incarnation)
                .expect("sign");
            log.insert(put).expect("insert put");
        }

        fn reads(&self, log: &Log, name: &str) -> Option<Vec<u8>> {
            log.open_entries(&self.identity)
                .expect("open")?
                .into_iter()
                .find(|entry| entry.name == name)
                .map(|entry| entry.value.as_slice().to_vec())
        }

        fn opened(&self, log: &Log) -> OpenedAudienceEntries {
            log.open_audience_entries(&self.identity)
                .expect("open")
                .expect("not removed")
        }
    }

    /// `m1` founds, admits `m2`, mints `[everyone]` epoch 1 for both and puts `shared`.
    fn two_members() -> (Log, Mac, Mac, (RecordId, Zeroizing<[u8; 32]>)) {
        let m1 = Mac::new(1, "m1");
        let m2 = Mac::new(2, "m2");
        let genesis = Record::found(&[9; 32], vec![m1.entrant.clone()], 1_000).expect("genesis");
        let mut log = Log::new(genesis).expect("log");
        m1.sign(
            &mut log,
            1_001,
            Body::Admit {
                entrant: m2.entrant.clone(),
                evidence: Evidence::Phrase,
            },
        );
        let epoch = m1.mint(&mut log, 1_002, 1, &[&m1, &m2]);
        m1.put(&mut log, 1_003, (epoch.0, &epoch.1), "shared", b"v1");
        (log, m1, m2, epoch)
    }

    /// A host list names the Macs it is for, so a seat it does not wrap is no gap of it. m1 and
    /// m2 hold `[everyone]`. m1 mints an epoch of the host list `[mac-1]`, wrapped to itself
    /// alone, and a newer `[everyone]` epoch wrapped to itself alone. Both are newest in their
    /// audience and wrap no removed Mac, and m2 is a current seat. The `[everyone]` epoch is m2's
    /// gap (the rule fires), the host list's epoch is not, and there is no `SeatAudience` of a
    /// host list to ask `wrap_gaps` or an admission about.
    ///
    /// NEGATIVE CONTROL: drop `is_wrap_gap`'s `SeatAudience::of` refusal. The host list's epoch
    /// is then m2's gap, as a seat audience's would be, and the test fails "a host list's epoch is
    /// no gap of a seat it does not name". `wrap_gaps`' `compile_fail` doctest holds the type:
    /// make `wrap_gaps` generic over `A: Clone + Into<EpochAudience>` (each converted, host lists
    /// dropped), so a host list can be passed again, and that doctest fails "Test compiled
    /// successfully, but it's marked `compile_fail`".
    #[test]
    fn a_host_list_epoch_is_no_gap_of_a_seat_it_does_not_name() {
        let (mut log, m1, m2, _) = two_members();
        let hosts = EpochAudience::Hosts(vec!["mac-1".to_owned()]);
        let (draft, key) = log
            .draft_epoch(&m1.actor, 1_010, hosts.clone(), 1)
            .expect("host-list epoch");
        let listed = draft.sign(&m1.seed, m1.incarnation).expect("sign");
        let listed_id = *listed.id();
        log.insert(listed).expect("insert");
        let wrap = log
            .draft_wrap(&m1.actor, 1_010, listed_id, &key, m1.identity.recipient())
            .expect("wrap")
            .sign(&m1.seed, m1.incarnation)
            .expect("sign");
        log.insert(wrap).expect("insert wrap");
        let (everyone, _) = m1.mint(&mut log, 1_011, 2, &[&m1]);
        let opened = m1.opened(&log);

        assert!(
            opened.maximal_epochs[&hosts].contains(&listed_id)
                && opened.unwrapped_keys.contains_key(&listed_id),
            "CONTROL: the host list's epoch is newest there, and m1 opens it"
        );
        assert!(
            log.is_wrap_gap(&opened, &everyone, &m2.actor),
            "CONTROL: an [everyone] epoch that omits m2 is its gap"
        );
        assert!(
            !log.is_wrap_gap(&opened, &listed_id, &m2.actor),
            "a host list's epoch is no gap of a seat it does not name"
        );
        let gaps = log.wrap_gaps(&opened, &admission_audiences(&opened));
        assert_eq!(
            gaps.iter()
                .map(|gap| (gap.audience.clone(), gap.epoch, gap.member))
                .collect::<Vec<_>>(),
            vec![(EpochAudience::Everyone, everyone, m2.actor)],
            "the heal names m2's [everyone] gap and nothing of the host list"
        );
        assert_eq!(
            SeatAudience::of(&hosts),
            None,
            "no seat audience of a host list"
        );
        for seat in [SeatAudience::Everyone, SeatAudience::Commons] {
            assert_eq!(SeatAudience::of(&seat.epochs()), Some(seat));
        }
    }

    /// The Allowing member wraps every epoch that still seals a current entry, so a Mac it lets
    /// in after a rotation reads what was sealed before it, with no admin locker taking part.
    ///
    /// NEGATIVE CONTROL: make `admission_epochs` take only `maximal_epochs` (wrap only the newest
    /// epoch): the Admit carries no `Wrap` of epoch 1, which seals `old-key`, and the first
    /// assert (`signed.len()`) fails.
    #[test]
    fn a_mac_admitted_after_a_rotation_reads_an_entry_sealed_before_it() {
        let (mut log, m1, m2, epoch1) = two_members();
        m1.put(
            &mut log,
            1_004,
            (epoch1.0, &epoch1.1),
            "old-key",
            b"sealed-before",
        );
        // The rotation: epoch 2 for both members, and a new value under it. `old-key` and
        // `shared` stay sealed under epoch 1.
        let epoch2 = m1.mint(&mut log, 1_005, 2, &[&m1, &m2]);
        m1.put(
            &mut log,
            1_006,
            (epoch2.0, &epoch2.1),
            "new-key",
            b"sealed-after",
        );

        // m2, a plain member, lets m3 in.
        let m3 = Mac::new(3, "m3");
        let opened = m2.opened(&log);
        let signed = log
            .admit(
                &m2.signer(),
                m3.entrant.clone(),
                &opened,
                &[SeatAudience::Everyone],
                1_007,
            )
            .expect("m2 admits m3");
        assert_eq!(signed[0].kind(), super::super::Kind::Admit);
        assert_eq!(
            signed.len(),
            3,
            "an Admit and one Wrap per epoch that seals a current entry"
        );
        assert!(log.membership().contains(&m3.actor));

        assert_eq!(
            m3.reads(&log, "old-key").as_deref(),
            Some(b"sealed-before".as_slice())
        );
        assert_eq!(m3.reads(&log, "shared").as_deref(), Some(b"v1".as_slice()));
        assert_eq!(
            m3.reads(&log, "new-key").as_deref(),
            Some(b"sealed-after".as_slice())
        );
        // A Mac the log never admitted reads nothing.
        assert_eq!(Mac::new(4, "m4").reads(&log, "shared"), None);
    }

    /// An `Admit` and `Wrap`s signed by a key no record seated let no Mac in, even when that key
    /// holds a stolen epoch key: the Mac it names reads nothing, and a store refuses the records.
    ///
    /// NEGATIVE CONTROL: in `eval.rs` `Graph::authorized`, authorize every record
    /// (`authorized[i] = true`): m3 shows as a member and the first `membership()` assert fails.
    #[test]
    fn an_admit_signed_by_a_non_member_is_inert() {
        let (mut log, m1, _m2, epoch1) = two_members();
        let outsider = Mac::new(9, "m9");
        let m3 = Mac::new(3, "m3");
        let admit = outsider.sign(
            &mut log,
            2_000,
            Body::Admit {
                entrant: m3.entrant.clone(),
                evidence: Evidence::Phrase,
            },
        );
        let wrap = log
            .draft_wrap(
                &outsider.actor,
                2_001,
                epoch1.0,
                &epoch1.1,
                m3.identity.recipient(),
            )
            .expect("wrap")
            .sign(&outsider.seed, outsider.incarnation)
            .expect("sign");
        log.insert(wrap.clone()).expect("held, not effective");
        assert!(!log.membership().contains(&m3.actor));
        assert_eq!(m3.reads(&log, "shared"), None);
        // A member's gap rule never wraps to it: it is no member.
        assert!(
            log.wrap_gaps(&m1.opened(&log), &[SeatAudience::Everyone])
                .iter()
                .all(|gap| gap.member != m3.actor)
        );
        // A store, which requires seats, refuses both outright.
        let mut store = super::super::Store::for_vault("team").expect("store");
        assert!(store.insert(log.genesis().clone()).expect("genesis"));
        for record in log.records().skip(1) {
            if record.id() != admit.id() && record.id() != wrap.id() {
                store.insert(record.clone()).expect("a member's record");
            }
        }
        assert!(
            store.insert(admit).is_err(),
            "a store held a non-member's Admit"
        );
    }

    /// One-mesh proofs P2, law 4 (no lockout): two concurrent heads that each wrap a Mac removed
    /// after both, with no seat holding both, are each a gap of the other side's minter, so a
    /// heal makes a carrier (a seat holding every newest epoch) that can re-key. Founders m1, m2
    /// and m3 hold `[everyone]` epoch 1; m1 mints epoch 2a for m1 and m3 while m2 mints epoch 2b
    /// for m2 and m3; then m1 removes m3. Both heads are due (each wraps m3) and no seat holds
    /// both: m1's closer names m2's gap of 2a and m2's names m1's of 2b. Once m1 heals m2 into
    /// 2a, m2 holds both heads, a carrier whose re-key wraps m1, and neither names a gap.
    ///
    /// NEGATIVE CONTROL: `ATERM_TEST_NEGATIVE_CONTROL=shipped-heal-rule` heals no omission of an
    /// epoch that wraps a removed Mac (the rule before PF2): neither side names a gap, no seat
    /// comes to hold both heads, and the first assert fails.
    #[test]
    fn two_due_heads_with_no_carrier_heal_each_other_instead_of_locking_out() {
        let (m1, m2, m3) = (Mac::new(1, "m1"), Mac::new(2, "m2"), Mac::new(3, "m3"));
        let founders = vec![m1.entrant.clone(), m2.entrant.clone(), m3.entrant.clone()];
        let genesis = Record::found(&[9; 32], founders, 1_000).expect("genesis");
        let mut base = Log::new(genesis).expect("log");
        m1.mint(&mut base, 1_001, 1, &[&m1, &m2, &m3]);
        let mut log = base.clone();
        let (a, key_a) = m1.mint(&mut log, 1_100, 2, &[&m1, &m3]);
        let mut side_b = base;
        let (b, _) = m2.mint(&mut side_b, 1_100, 2, &[&m2, &m3]);
        log.merge(&side_b).expect("merge");
        let cutoff = *log.network();
        m1.sign(
            &mut log,
            1_200,
            Body::Remove {
                member: m3.actor,
                cutoff,
            },
        );
        let gaps_of = |log: &Log, mac: &Mac| -> BTreeSet<(RecordId, MemberId)> {
            log.wrap_gaps(&mac.opened(log), &[SeatAudience::Everyone])
                .into_iter()
                .map(|gap| (gap.epoch, gap.member))
                .collect()
        };
        assert_eq!(gaps_of(&log, &m1), BTreeSet::from([(a, m2.actor)]));
        assert_eq!(gaps_of(&log, &m2), BTreeSet::from([(b, m1.actor)]));

        let heal = log
            .draft_wrap(&m1.actor, 1_300, a, &key_a, m2.identity.recipient())
            .expect("wrap")
            .sign(&m1.seed, m1.incarnation)
            .expect("sign");
        log.insert(heal).expect("insert heal");
        assert!(gaps_of(&log, &m1).is_empty());
        assert!(
            gaps_of(&log, &m2).is_empty(),
            "m2 holds both heads, so it re-keys rather than heals"
        );
    }

    /// A Mac admitted on one side of a split, while the other side minted a new epoch for the
    /// members it knew, reads the value put under that epoch once a member that holds it sees
    /// the gap after the heal. An epoch minted after its minter saw the Mac's `Admit` and left
    /// it out is a gap too (one-mesh M5: only a `Remove` revokes), and an explicit `Remove`
    /// ends it.
    ///
    /// NEGATIVE CONTROL: make `wrap_gaps` return no gap (drop the wrap-gap rule): no gap names
    /// m3 after the heal, so nothing wraps epoch 2 to it, and the test fails at
    /// `expect("the gap")`.
    #[test]
    fn a_mac_admitted_on_one_side_reads_a_value_put_under_an_epoch_minted_on_the_other() {
        let (base, m1, m2, _) = two_members();
        let m3 = Mac::new(3, "m3");

        // Side A: m1 admits m3.
        let mut side_a = base.clone();
        let opened = m1.opened(&side_a);
        side_a
            .admit(
                &m1.signer(),
                m3.entrant.clone(),
                &opened,
                &[SeatAudience::Everyone],
                3_000,
            )
            .expect("m1 admits m3");
        // Side B: m2 rotates `[everyone]` for the members it knows and puts a new value.
        let mut side_b = base.clone();
        let epoch2 = m2.mint(&mut side_b, 3_001, 2, &[&m1, &m2]);
        m2.put(
            &mut side_b,
            3_002,
            (epoch2.0, &epoch2.1),
            "b-value",
            b"put-on-b",
        );

        // The heal: a union, in either order.
        let mut healed = side_a.clone();
        healed.merge(&side_b).expect("merge");
        let mut other = side_b.clone();
        other.merge(&side_a).expect("merge");
        assert_eq!(healed.heads(), other.heads());
        assert!(healed.membership().contains(&m3.actor));
        assert_eq!(
            m3.reads(&healed, "b-value"),
            None,
            "epoch 2 was never wrapped to m3"
        );

        // Every holder sees the same gap and the same ranks.
        let gaps_m1 = healed.wrap_gaps(&m1.opened(&healed), &[SeatAudience::Everyone]);
        let gaps_m2 = healed.wrap_gaps(&m2.opened(&healed), &[SeatAudience::Everyone]);
        assert_eq!(gaps_m1, gaps_m2);
        let gap = gaps_m1
            .iter()
            .find(|gap| gap.epoch == epoch2.0 && gap.member == m3.actor)
            .cloned()
            .expect("the gap");
        assert_eq!(gap.opened_ms, 3_001);
        let r1 = healed
            .gap_rank(&m1.opened(&healed), &gap, &m1.actor)
            .expect("m1 holds it");
        let r2 = healed
            .gap_rank(&m2.opened(&healed), &gap, &m2.actor)
            .expect("m2 holds it");
        assert_eq!(BTreeSet::from([r1, r2]), BTreeSet::from([0, 1]));
        assert_eq!(healed.gap_rank(&m1.opened(&healed), &gap, &m3.actor), None);

        // The rank-0 holder closes it; m3 then reads the value put on the other side.
        let closer = if r1 == 0 { &m1 } else { &m2 };
        let opened = closer.opened(&healed);
        let key = &opened.unwrapped_keys[&gap.epoch];
        let wrap = healed
            .draft_wrap(&closer.actor, 3_100, gap.epoch, key, gap.recipient)
            .expect("wrap")
            .sign(&closer.seed, closer.incarnation)
            .expect("sign");
        healed.insert(wrap).expect("insert");
        assert_eq!(
            m3.reads(&healed, "b-value").as_deref(),
            Some(b"put-on-b".as_slice())
        );
        assert!(
            healed
                .wrap_gaps(&m1.opened(&healed), &[SeatAudience::Everyone])
                .is_empty(),
            "a closed gap is closed for every member"
        );

        // m2 now knows m3 and mints epoch 3 without it. That is no revocation: m3 holds a seat
        // no `Remove` names, so epoch 3's omission of it is a gap a holder closes.
        let epoch3 = m2.mint(&mut healed, 3_200, 3, &[&m1, &m2]);
        m2.put(
            &mut healed,
            3_201,
            (epoch3.0, &epoch3.1),
            "after",
            b"m3-omitted",
        );
        let entry_before = healed.membership().get(&m3.actor).expect("m3").entry;
        assert!(healed.left_out(&m1.opened(&healed), &EpochAudience::Everyone, &m3.actor));
        let omitted = |log: &Log| {
            log.wrap_gaps(&m1.opened(log), &[SeatAudience::Everyone])
                .into_iter()
                .filter(|gap| gap.epoch == epoch3.0)
                .map(|gap| gap.member)
                .collect::<Vec<_>>()
        };
        assert_eq!(omitted(&healed), vec![m3.actor], "epoch 3 omits m3: a gap");
        assert!(healed.is_wrap_gap(&m1.opened(&healed), &epoch3.0, &m3.actor));

        // Neither m3 signing a self-Admit after epoch 3 (with a lower RecordId than its original
        // Admit) nor another member signing a redundant later Admit of m3 moves m3's standing
        // entry, and the gap stays one gap.
        for signer in [&m3, &m1] {
            let later = (3_300_u64..)
                .map(|ms| {
                    healed
                        .draft(
                            &signer.actor,
                            ms,
                            Body::Admit {
                                entrant: m3.entrant.clone(),
                                evidence: super::Evidence::Phrase,
                            },
                        )
                        .sign(&signer.seed, signer.incarnation)
                        .expect("sign")
                })
                .find(|r| *r.id() < entry_before)
                .expect("a later Admit with a lower RecordId");
            healed.insert(later).expect("insert");
        }
        assert_eq!(
            healed.membership().get(&m3.actor).expect("m3").entry,
            entry_before,
            "a self-Admit or redundant later Admit moved m3's standing entry"
        );
        assert_eq!(omitted(&healed), vec![m3.actor], "still one gap");

        // An explicit `Remove` is what revokes: m3 is no current seat, and no gap names it.
        let mut removed = healed.clone();
        m1.sign(
            &mut removed,
            3_400,
            Body::Remove {
                member: m3.actor,
                cutoff: entry_before,
            },
        );
        assert!(
            !removed
                .current_recipients(&EpochAudience::Everyone)
                .contains(&m3.identity.recipient())
        );
        assert!(
            omitted(&removed).is_empty(),
            "a removed Mac is never healed"
        );
        assert!(!removed.is_wrap_gap(&m1.opened(&removed), &epoch3.0, &m3.actor));

        // NEGATIVE CONTROL: 0.154.0's rule read epoch 3's omission as m3's revocation.
        OLD_GAP_RULE.with(|on| on.set(true));
        let old = omitted(&healed);
        OLD_GAP_RULE.with(|on| on.set(false));
        assert!(old.is_empty(), "the old rule heals no omission: {old:?}");
    }

    /// A label held at `m99999` (a Mac that asked under it, or a roster row above it) leaves the
    /// next Mac a label an `Admit` can carry: the lowest one nothing holds, not `m100000`, which
    /// `is_label` refuses and which failed every later Allow on that log.
    ///
    /// NEGATIVE CONTROL: make `next_label` return the one above the highest whatever it is:
    /// `"m100000"` comes back and the `is_label` assert fails.
    #[test]
    fn a_label_held_at_m99999_still_leaves_one_an_admit_can_carry() {
        let (log, _m1, _m2, _) = two_members();
        for high in ["m99999", "m100000"] {
            let taken = BTreeSet::from([high.to_owned()]);
            let next = log
                .next_label("Andrews-MacBook-Air", &taken, &BTreeSet::new())
                .expect("a free label");
            assert!(is_label(&next), "{high}: {next}");
            assert_eq!(next, "m3", "m1 founds and m2 is admitted");
        }
        let all: BTreeSet<String> = (1..=99_999_u32).map(|n| format!("m{n}")).collect();
        assert_eq!(
            log.next_label("m5", &all, &BTreeSet::new()),
            None,
            "every m<N> is held"
        );
    }

    /// C11 under F15: the allowing member keeps the label a Mac asks under when nothing holds it,
    /// and otherwise gives the one above the highest label the log or the roster holds.
    #[test]
    fn the_allowing_member_assigns_the_next_free_label() {
        let (mut log, m1, _m2, _) = two_members();
        let none = BTreeSet::new();
        assert_eq!(
            log.next_label("m40", &none, &BTreeSet::new()).as_deref(),
            Some("m40")
        );
        assert_eq!(
            log.next_label("M40", &none, &BTreeSet::new()).as_deref(),
            Some("m40")
        );
        assert_eq!(
            log.next_label("m2", &none, &BTreeSet::new()).as_deref(),
            Some("m3"),
            "m2 is held by an Admit"
        );
        assert_eq!(
            log.next_label("Andrews-MacBook-Air", &none, &BTreeSet::new())
                .as_deref(),
            Some("m3")
        );
        let roster = BTreeSet::from(["m55".to_owned(), "m3".to_owned()]);
        assert_eq!(
            log.next_label("m3", &roster, &BTreeSet::new()).as_deref(),
            Some("m56")
        );
        assert_eq!(
            log.next_label("m40", &roster, &BTreeSet::new()).as_deref(),
            Some("m40")
        );
        // A sheet's number is counted toward the next label and holds none by itself.
        let sheet = BTreeSet::from(["m70".to_owned()]);
        assert_eq!(log.next_label("mac", &none, &sheet).as_deref(), Some("m71"));
        assert_eq!(log.next_label("m70", &none, &sheet).as_deref(), Some("m70"));
        // A removed Mac's label stays held.
        let m7 = Mac::new(7, "m7");
        let admit = m1.sign(
            &mut log,
            4_000,
            Body::Admit {
                entrant: m7.entrant.clone(),
                evidence: Evidence::Phrase,
            },
        );
        m1.sign(
            &mut log,
            4_001,
            Body::Remove {
                member: m7.actor,
                cutoff: *admit.id(),
            },
        );
        assert!(!log.membership().contains(&m7.actor));
        assert_eq!(
            log.next_label("m7", &none, &BTreeSet::new()).as_deref(),
            Some("m8")
        );
        // A removed Mac's own `Admit` (for another Mac or itself) claims no label and cannot
        // squat `m40` or `m99999`.
        let m99 = Mac::new(99, "m99999");
        m7.sign(
            &mut log,
            4_002,
            Body::Admit {
                entrant: m99.entrant,
                evidence: Evidence::Phrase,
            },
        );
        let mut self_m40 = m7.entrant.clone();
        self_m40.label = "m40".to_owned();
        m7.sign(
            &mut log,
            4_003,
            Body::Admit {
                entrant: self_m40,
                evidence: Evidence::Phrase,
            },
        );
        assert_eq!(
            log.next_label("m40", &none, &BTreeSet::new()).as_deref(),
            Some("m40")
        );
        assert_eq!(
            log.next_label("Andrews-MacBook-Air", &none, &BTreeSet::new())
                .as_deref(),
            Some("m8")
        );
    }

    /// `m1` founds and admits `m2`..`m<n>`, then mints `[everyone]` epoch 1 for every seat.
    fn lab(n: u8) -> (Log, Vec<Mac>) {
        let macs: Vec<Mac> = (1..=n)
            .map(|tag| Mac::new(tag, &format!("m{tag}")))
            .collect();
        let genesis =
            Record::found(&[9; 32], vec![macs[0].entrant.clone()], 1_000).expect("genesis");
        let mut log = Log::new(genesis).expect("log");
        for (ms, mac) in (1_001..).zip(&macs[1..]) {
            macs[0].sign(
                &mut log,
                ms,
                Body::Admit {
                    entrant: mac.entrant.clone(),
                    evidence: Evidence::Phrase,
                },
            );
        }
        let all: Vec<&Mac> = macs.iter().collect();
        macs[0].mint(&mut log, 1_500, 1, &all);
        (log, macs)
    }

    /// When each healer first saw each gap, in rank steps: `(healer, epoch, recipient)`.
    type Sight = std::collections::BTreeMap<(MemberId, RecordId, Recipient), usize>;

    /// One rank step of the heal on partial views, by `close_wrap_gaps`' rule: each Mac of
    /// `healers` looks at its own copy of `log` at once and signs each gap it holds a rank `k`
    /// for once `step` is `k` steps past when it first saw that gap (`sight`); then every Mac
    /// takes every other's records. With `unranked` every holder signs every gap it sees, the
    /// closer's negative control. Returns the `Wrap`s signed.
    fn heal_step(
        log: &mut Log,
        healers: &[&Mac],
        sight: &mut Sight,
        step: usize,
        unranked: bool,
    ) -> usize {
        let mut copies = Vec::new();
        let mut signed = 0;
        for healer in healers {
            let mut copy = log.clone();
            let opened = healer.opened(&copy);
            for gap in copy.wrap_gaps(&opened, &[SeatAudience::Everyone]) {
                let first = *sight
                    .entry((healer.actor, gap.epoch, gap.recipient))
                    .or_insert(step);
                let due = unranked
                    || copy
                        .gap_rank(&opened, &gap, &healer.actor)
                        .is_some_and(|rank| step >= first + rank);
                if !due {
                    continue;
                }
                let key = &opened.unwrapped_keys[&gap.epoch];
                let wrap = copy
                    .draft_wrap(&healer.actor, 9_000, gap.epoch, key, gap.recipient)
                    .expect("wrap")
                    .sign(&healer.seed, healer.incarnation)
                    .expect("sign");
                copy.insert(wrap).expect("insert");
                signed += 1;
            }
            copies.push(copy);
        }
        for copy in &copies {
            log.merge(copy).expect("merge");
        }
        signed
    }

    /// Heal step by step until no gap is open, at most `steps`; returns the `Wrap`s signed.
    fn heal(log: &mut Log, healers: &[&Mac], steps: usize) -> usize {
        let mut sight = Sight::new();
        (0..steps)
            .map(|step| heal_step(log, healers, &mut sight, step, false))
            .sum()
    }

    /// The `Wrap`s of `epoch` the log holds, by recipient.
    fn wraps_of(log: &Log, epoch: &RecordId) -> std::collections::BTreeMap<Recipient, usize> {
        let mut out = std::collections::BTreeMap::new();
        for record in log.records() {
            if let Body::Wrap {
                epoch: of,
                recipient,
                ..
            } = record.body()
                && of == epoch
            {
                *out.entry(*recipient).or_insert(0) += 1;
            }
        }
        out
    }

    /// The 2026-11-03 replay (one-mesh §2.1, M5). 88 seats hold `[everyone]` epoch 1; `m4`, holding
    /// only a prefix of a batch, re-keys to epoch 2 wrapped to `m1`..`m3` and not even to itself,
    /// and puts a value under it; `m1` then removes `m88` with an explicit `Remove`. Every holder
    /// of epoch 2 names the same 84 gaps (every current seat it omits), each gap has one holder
    /// ranked first, and a rank step at a time the holders heal it with one `Wrap` per gap, on
    /// partial views, after which every current seat reads the value. A seat that does not hold
    /// epoch 2 heals nothing, and the removed `m88` is never wrapped.
    ///
    /// An epoch's key, and so its id and every gap's rank order, is fresh on each run, so the
    /// replay runs on several.
    ///
    /// NEGATIVE CONTROL: with `OLD_GAP_RULE` on (0.154.0: an omission the minter knew is a
    /// revocation) no holder names a gap, and 85 seats stay out, as Commons epoch 66 did; with
    /// the minter ranked among the holders though it holds no `Wrap` (0.154.0's holder set),
    /// some run signs two `Wrap`s for a gap the minter ranked first.
    #[test]
    fn eighty_eight_seats_wrapped_to_three_heal_one_wrap_per_gap() {
        for _ in 0..6 {
            eighty_eight_replay();
        }
    }

    fn eighty_eight_replay() {
        let (mut log, macs) = lab(88);
        let wrapped: Vec<&Mac> = macs[..3].iter().collect();
        let epoch2 = macs[3].mint(&mut log, 2_000, 2, &wrapped);
        macs[3].put(&mut log, 2_001, (epoch2.0, &epoch2.1), "after", b"epoch-2");
        let removed = &macs[87];
        let cutoff = log.membership().get(&removed.actor).expect("m88").entry;
        macs[0].sign(
            &mut log,
            2_002,
            Body::Remove {
                member: removed.actor,
                cutoff,
            },
        );

        let current = log.current_recipients(&EpochAudience::Everyone);
        assert_eq!(current.len(), 87, "88 seats less one `Remove`");
        assert!(!current.contains(&removed.identity.recipient()));
        let omitted: BTreeSet<MemberId> = macs[3..87].iter().map(|mac| mac.actor).collect();
        for holder in &wrapped {
            let gaps = log.wrap_gaps(&holder.opened(&log), &[SeatAudience::Everyone]);
            assert_eq!(
                gaps.iter().map(|gap| gap.member).collect::<BTreeSet<_>>(),
                omitted,
                "every current seat epoch 2 omits is a gap, the minter's own included"
            );
            assert!(gaps.iter().all(|gap| gap.epoch == epoch2.0));
        }
        // A seat that does not hold epoch 2 heals nothing and ranks for nothing.
        let outside = &macs[10];
        let opened = outside.opened(&log);
        assert!(
            log.wrap_gaps(&opened, &[SeatAudience::Everyone]).is_empty(),
            "a non-holder names no gap it could close"
        );
        let any_gap = log.wrap_gaps(&macs[0].opened(&log), &[SeatAudience::Everyone])[0].clone();
        assert_eq!(log.gap_rank(&opened, &any_gap, &outside.actor), None);

        // NEGATIVE CONTROL: the old rule leaves the 85 out.
        OLD_GAP_RULE.with(|on| on.set(true));
        let old = log.wrap_gaps(&macs[0].opened(&log), &[SeatAudience::Everyone]);
        OLD_GAP_RULE.with(|on| on.set(false));
        assert!(old.is_empty(), "the old rule heals nothing: {old:?}");

        // The heal, a rank step at a time, every current seat looking: the minter `m4` cannot
        // open epoch 2, so it ranks for nothing until healed, and a seat healed ranks behind the
        // batch's holders, so the three holders' ranks hold still for the whole heal.
        let current_macs: Vec<&Mac> = macs[..87].iter().collect();
        let signed = heal(&mut log, &current_macs, 3);
        assert_eq!(signed, omitted.len(), "one `Wrap` per gap");
        let wraps = wraps_of(&log, &epoch2.0);
        assert!(wraps.values().all(|&n| n == 1), "one `Wrap` per recipient");
        assert!(!wraps.contains_key(&removed.identity.recipient()));
        for holder in &wrapped {
            assert!(
                log.wrap_gaps(&holder.opened(&log), &[SeatAudience::Everyone])
                    .is_empty()
            );
        }
        for mac in &macs[..87] {
            assert_eq!(
                mac.reads(&log, "after").as_deref(),
                Some(b"epoch-2".as_slice()),
                "{} reads under the healed epoch",
                mac.entrant.label
            );
            assert!(!log.left_out(&macs[0].opened(&log), &EpochAudience::Everyone, &mac.actor));
        }
        assert_eq!(
            removed.reads(&log, "after"),
            None,
            "a removed Mac is never healed"
        );
    }

    /// N concurrent healers produce one heal per gap (one-mesh M5, item 6 of A1): 8 holders of
    /// an epoch that omits 12 of 20 seats each look at the gaps at the same instant on their own
    /// copy of the log; exactly one ranks first for each gap, so the step signs 12 `Wrap`s and
    /// the next steps sign none.
    ///
    /// NEGATIVE CONTROL: every holder ranked first (as `remove::UNRANKED` does to the closer)
    /// signs 8 `Wrap`s per gap, 96 in all, past the log's one per (gap, epoch).
    #[test]
    fn eight_concurrent_healers_sign_one_wrap_per_gap() {
        let (mut log, macs) = lab(20);
        let holders: Vec<&Mac> = macs[..8].iter().collect();
        let epoch2 = macs[0].mint(&mut log, 2_000, 2, &holders);
        let mut unranked = log.clone();
        let everyone: Vec<&Mac> = macs.iter().collect();

        let mut sight = Sight::new();
        let first = heal_step(&mut log, &everyone, &mut sight, 0, false);
        assert_eq!(first, 12, "one holder ranks first for each gap");
        for step in 1..8 {
            assert_eq!(
                heal_step(&mut log, &everyone, &mut sight, step, false),
                0,
                "step {step}"
            );
        }
        let wraps = wraps_of(&log, &epoch2.0);
        assert_eq!(wraps.len(), 20);
        assert!(wraps.values().all(|&n| n == 1), "{wraps:?}");

        assert_eq!(
            heal_step(&mut unranked, &holders, &mut Sight::new(), 0, true),
            96
        );
        assert!(wraps_of(&unranked, &epoch2.0).values().any(|&n| n == 8));
    }
}
