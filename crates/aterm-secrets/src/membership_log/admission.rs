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
        let membership = self.membership_ref();
        membership
            .members()
            .values()
            .filter(|seat| self.seat_is_current(membership, seat) && !denied(seat))
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
        let membership = self.membership_ref();
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
                && self.heals_omissions(opened, membership, &audience, &epoch, &removed, &denied);
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
                    old || self.seat_is_current(membership, member),
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
        let membership = self.membership_ref();
        let Some(seat) = membership.get(member) else {
            return false;
        };
        if !old && !self.seat_is_current(membership, seat) {
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
                membership,
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
        let membership = self.membership_ref();
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
                    !self.seat_is_current(membership, member)
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
        let membership = self.membership_ref();
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
    /// roster's labels, aliases, claims, deny-list and `[machines]` names: no number is reserved)
    /// names; otherwise the `m<N>` one above the highest of all of them and of `counted`
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
        let membership = self.membership_ref();
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
#[path = "admission_tests.rs"]
mod tests;
