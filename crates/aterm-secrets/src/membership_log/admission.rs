// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What any member signs when a person's Allow and the phrase let a Mac in, and the wrap gaps
//! every member closes (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.2 "Labels", §3.3.6, F15).
//!
//! - [`Log::admission_epochs`]: the epochs an admitting member wraps to the new Mac, in each
//!   audience it names: every epoch that still seals a winning entry, so the new Mac reads
//!   entries sealed before the latest rotation, and the causally newest ones, so it reads and
//!   writes what comes next.
//! - [`Log::admit`]: the `Admit` and those `Wrap`s, signed by the member and held by the log.
//! - [`Log::wrap_gaps`]: an epoch an effective member lacks although its minter could not have
//!   known that member (its entry is not in the epoch's causal past): a Mac admitted on one side
//!   of a split while the other side minted. A member the minter knew and left out was left out
//!   on purpose (a rotation that revoked it), so that is no gap.
//! - [`Log::gap_rank`]: which of the members holding an epoch closes a gap first, so ~35 Macs
//!   that see one gap after a heal do not all sign the same `Wrap`.
//! - [`Log::next_label`]: C11's next free `m<N>` from what the log and the swarm roster hold.

use super::entries::OpenedAudienceEntries;
use super::{
    Body, Entrant, EpochAudience, Evidence, Log, MemberId, Record, RecordId, eval, is_label,
};
use crate::Result;
use crate::age::Recipient;
use aterm_digest::Sha256;
use std::collections::{BTreeMap, BTreeSet};

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

impl Log {
    /// The epochs a member that opened `opened` wraps to a Mac it admits, in each of
    /// `audiences`: those that seal a winning entry and the causally maximal ones, each once,
    /// by audience then id.
    #[must_use]
    pub fn admission_epochs(
        opened: &OpenedAudienceEntries,
        audiences: &[EpochAudience],
    ) -> Vec<(EpochAudience, RecordId)> {
        let mut out = BTreeSet::new();
        for audience in audiences {
            for epoch in opened
                .winning_epochs
                .get(audience)
                .into_iter()
                .flatten()
                .chain(opened.maximal_epochs.get(audience).into_iter().flatten())
            {
                out.insert((audience.clone(), *epoch));
            }
        }
        out.into_iter().collect()
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
        audiences: &[EpochAudience],
        authored_ms: u64,
    ) -> Result<Vec<Record>> {
        let (epochs, lacking): (Vec<_>, Vec<_>) = Self::admission_epochs(opened, audiences)
            .into_iter()
            .partition(|(_, epoch)| opened.unwrapped_keys.contains_key(epoch));
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
            .draft(
                signer.actor,
                authored_ms,
                Body::Admit {
                    entrant,
                    evidence: Evidence::Phrase,
                },
            )
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

    /// The wrap gaps in `audiences` that the member that opened `opened` can close: each epoch
    /// of [`Log::admission_epochs`] it unwrapped, and each effective member with no effective
    /// `Wrap` of it whose entry (its `Admit`, or the genesis) is not in the epoch's causal past.
    /// The minter of such an epoch could not have known that member; one it knew and left out
    /// is no gap. A removed Mac is no member, so it is never named.
    #[must_use]
    pub fn wrap_gaps(
        &self,
        opened: &OpenedAudienceEntries,
        audiences: &[EpochAudience],
    ) -> Vec<Gap> {
        let effective = eval::effective_records(&self.order);
        let membership = self.membership();
        let mut gaps = Vec::new();
        for (audience, epoch) in Self::admission_epochs(opened, audiences) {
            if !opened.unwrapped_keys.contains_key(&epoch) {
                continue;
            }
            let Some(&epoch_at) = self.held.get(&epoch) else {
                continue;
            };
            let wrapped = opened.wrapped_recipients.get(&epoch);
            for (actor, member) in membership.members() {
                if self.left_out(opened, &audience, actor)
                    || self.left_out(opened, &EpochAudience::Everyone, actor)
                {
                    continue;
                }
                let recipient = member.binding.recipient();
                if wrapped.is_some_and(|set| set.contains(&recipient)) {
                    continue;
                }
                let Some(&entry_at) = self.held.get(&member.entry) else {
                    continue;
                };
                if entry_at == epoch_at || effective.before(entry_at, epoch_at) {
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

    /// Whether `member` lacks `epoch` as a wrap gap ([`Log::wrap_gaps`]): it is an effective
    /// member with no effective `Wrap` of the epoch, and its entry (its `Admit`, or the genesis)
    /// is not in the epoch's causal past, so the epoch's minter could not have known it. Read
    /// from `member`'s side, so the caller need not hold the epoch: a member admitted beside a
    /// concurrent rotation asks this of itself before it admits another Mac.
    #[must_use]
    pub fn is_wrap_gap(
        &self,
        opened: &OpenedAudienceEntries,
        epoch: &RecordId,
        member: &MemberId,
    ) -> bool {
        let membership = self.membership();
        let Some(seat) = membership.get(member) else {
            return false;
        };
        let Some(Body::Epoch { audience, .. }) = self.get(epoch).map(Record::body) else {
            return false;
        };
        if self.left_out(opened, audience, member)
            || self.left_out(opened, &EpochAudience::Everyone, member)
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
        let (Some(&epoch_at), Some(&entry_at)) = (self.held.get(epoch), self.held.get(&seat.entry))
        else {
            return false;
        };
        entry_at != epoch_at && !eval::effective_records(&self.order).before(entry_at, epoch_at)
    }

    /// Where `me` stands among the members that can close `gap`: each effective member that
    /// minted its epoch or holds an effective `Wrap` of it, ordered by
    /// `SHA-256(domain ‖ actor ‖ epoch ‖ recipient)`. Rank 0 closes it at once; rank `k` once it
    /// is still open `k` steps after that member first saw it (the client's `close_wrap_gaps`).
    /// `None` when `me` is not one of them.
    #[must_use]
    pub fn gap_rank(
        &self,
        opened: &OpenedAudienceEntries,
        gap: &Gap,
        me: &MemberId,
    ) -> Option<usize> {
        let membership = self.membership();
        let wrapped = opened.wrapped_recipients.get(&gap.epoch);
        let minter = self.get(&gap.epoch).map(|record| *record.actor());
        let mut holders: Vec<([u8; 32], MemberId)> = membership
            .members()
            .iter()
            .filter(|(actor, member)| {
                !self.left_out(opened, &gap.audience, actor)
                    && !self.left_out(opened, &EpochAudience::Everyone, actor)
                    && (Some(**actor) == minter
                        || wrapped.is_some_and(|set| set.contains(&member.binding.recipient())))
            })
            .map(|(actor, _)| {
                let mut buf = Vec::with_capacity(RANK_DOMAIN.len() + 160);
                buf.extend_from_slice(RANK_DOMAIN);
                buf.extend_from_slice(actor);
                buf.extend_from_slice(&gap.epoch);
                buf.extend_from_slice(gap.recipient.to_bech32().as_bytes());
                (Sha256::digest(&buf), *actor)
            })
            .collect();
        holders.sort();
        holders.iter().position(|(_, actor)| actor == me)
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
        let effective = eval::effective_records(&self.order);
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
    }

    /// [`Log::left_out`] evaluated directly from the log's effective `Epoch` and `Wrap` records
    /// without decrypting audience entries.
    #[must_use]
    pub fn audience_left_out(&self, audience: &EpochAudience, member: &MemberId) -> bool {
        let membership = self.membership();
        let Some(seat) = membership.get(member) else {
            return true;
        };
        let Some(&entry_at) = self.held.get(&seat.entry) else {
            return true;
        };
        let recipient = seat.binding.recipient();
        let effective = eval::effective_records(&self.order);
        let mut epochs: Vec<(usize, RecordId)> = Vec::new();
        let mut wrapped_epochs: BTreeSet<RecordId> = BTreeSet::new();
        for (i, record) in self.order.iter().enumerate().skip(1) {
            if !effective.counts(i) {
                continue;
            }
            match record.body() {
                Body::Epoch { audience: aud, .. } if aud == audience => {
                    epochs.push((i, *record.id()));
                }
                Body::Wrap {
                    epoch,
                    recipient: to,
                    ..
                } if *to == recipient => {
                    wrapped_epochs.insert(*epoch);
                }
                _ => {}
            }
        }
        epochs
            .iter()
            .filter(|&&(a, _)| {
                !epochs
                    .iter()
                    .any(|&(b, _)| a != b && effective.before(a, b))
            })
            .any(|&(at, ref epoch)| {
                !wrapped_epochs.contains(epoch)
                    && (entry_at == at || effective.before(entry_at, at))
            })
    }

    /// Whether a causally newest [`Body::Epoch`] of `audience` in this log leaves `recipient` out
    /// on purpose: it holds no [`Body::Wrap`] to `recipient` although `recipient`'s member entry
    /// (for [`EpochAudience::Everyone`] / [`EpochAudience::Commons`]) or a prior [`Body::Wrap`] of
    /// `audience` to `recipient` lies in its causal past.
    #[must_use]
    pub fn audience_left_out_recipient(
        &self,
        audience: &EpochAudience,
        recipient: &Recipient,
    ) -> bool {
        let membership = self.membership();
        let entry_at = membership
            .members()
            .values()
            .filter(|m| m.binding.recipient() == *recipient)
            .filter_map(|seat| self.held.get(&seat.entry).copied())
            .max();
        let effective = eval::effective_records(&self.order);
        let mut epochs: Vec<(usize, RecordId)> = Vec::new();
        let mut epoch_ids: BTreeSet<RecordId> = BTreeSet::new();
        let mut wrapped_epochs: BTreeMap<RecordId, usize> = BTreeMap::new();
        for (i, record) in self.order.iter().enumerate().skip(1) {
            if !effective.counts(i) {
                continue;
            }
            match record.body() {
                Body::Epoch { audience: aud, .. } if aud == audience => {
                    epochs.push((i, *record.id()));
                    epoch_ids.insert(*record.id());
                }
                Body::Wrap {
                    epoch,
                    recipient: to,
                    ..
                } if to == recipient => {
                    wrapped_epochs.entry(*epoch).or_insert(i);
                }
                _ => {}
            }
        }
        let prior_wrap_indices: Vec<usize> = wrapped_epochs
            .iter()
            .filter_map(|(ep, &w_at)| epoch_ids.contains(ep).then_some(w_at))
            .collect();
        epochs
            .iter()
            .filter(|&&(a, _)| {
                !epochs
                    .iter()
                    .any(|&(b, _)| a != b && effective.before(a, b))
            })
            .any(|&(at, ref epoch)| {
                if wrapped_epochs.contains_key(epoch) {
                    return false;
                }
                let knew_entry =
                    matches!(audience, EpochAudience::Everyone | EpochAudience::Commons)
                        && entry_at.is_some_and(|e_at| e_at == at || effective.before(e_at, at));
                let knew_wrap = prior_wrap_indices
                    .iter()
                    .any(|&w_at| w_at == at || effective.before(w_at, at));
                knew_entry || knew_wrap
            })
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

/// The audiences an admitting member wraps (§3.3.6): `[everyone]`, and F18's `commons` (the
/// Commons network key and control descriptor, [`EpochAudience::Commons`]) when the log holds a
/// Commons epoch, so the new Mac takes part in Commons control from the log. Host lists stay
/// closed: they name the Macs they are for.
#[must_use]
pub fn admission_audiences(opened: &OpenedAudienceEntries) -> Vec<EpochAudience> {
    let mut out = vec![EpochAudience::Everyone];
    if opened
        .maximal_epochs
        .get(&EpochAudience::Commons)
        .is_some_and(|epochs| !epochs.is_empty())
    {
        out.push(EpochAudience::Commons);
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
                &[EpochAudience::Everyone],
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
            log.wrap_gaps(&m1.opened(&log), &[EpochAudience::Everyone])
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

    /// A Mac admitted on one side of a split, while the other side minted a new epoch for the
    /// members it knew, reads the value put under that epoch once a member that holds it sees
    /// the gap after the heal. An epoch minted after its minter saw the Mac's `Admit` and left
    /// it out is no gap: that member was left out on purpose.
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
                &[EpochAudience::Everyone],
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
        let gaps_m1 = healed.wrap_gaps(&m1.opened(&healed), &[EpochAudience::Everyone]);
        let gaps_m2 = healed.wrap_gaps(&m2.opened(&healed), &[EpochAudience::Everyone]);
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
                .wrap_gaps(&m1.opened(&healed), &[EpochAudience::Everyone])
                .is_empty(),
            "a closed gap is closed for every member"
        );

        // m2 now knows m3 and mints epoch 3 without it: a deliberate exclusion, no gap.
        let epoch3 = m2.mint(&mut healed, 3_200, 3, &[&m1, &m2]);
        m2.put(
            &mut healed,
            3_201,
            (epoch3.0, &epoch3.1),
            "after",
            b"m3-left-out",
        );
        let entry_before = healed.membership().get(&m3.actor).expect("m3").entry;
        assert!(healed.left_out(&m1.opened(&healed), &EpochAudience::Everyone, &m3.actor));
        assert!(
            healed
                .wrap_gaps(&m1.opened(&healed), &[EpochAudience::Everyone])
                .iter()
                .all(|gap| gap.epoch != epoch3.0),
            "an epoch whose minter knew m3 and left it out was reopened to m3"
        );

        // Neither m3 signing a self-Admit after epoch 3 (with a lower RecordId than its original
        // Admit) nor another member signing a redundant later Admit of m3 can move m3's standing
        // entry past epoch 3 to reopen epoch 3 as a wrap gap or clear left_out.
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
        assert!(healed.left_out(&m1.opened(&healed), &EpochAudience::Everyone, &m3.actor));
        assert!(
            healed
                .wrap_gaps(&m1.opened(&healed), &[EpochAudience::Everyone])
                .iter()
                .all(|gap| gap.epoch != epoch3.0),
            "a self-Admit or redundant later Admit reopened epoch 3 to m3"
        );
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
}
