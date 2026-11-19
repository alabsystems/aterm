// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a format-2 log's own pass computes (`docs/DESIGN-vault-log-2026-11-14.md` step 4,
//! tracker FL-148F): the bodies it sheds with no vote, the checkpoint a seat shares, and the
//! final checkpoint a node prunes at. Each is a function of the records alone, read from their
//! frames; nothing here writes a file. The callers are a Mac's pass (`client::log_pass`) and a
//! vault server's take of records (`server::apply_records`), each only for a store that reads
//! format 2 ([`Store::reading`]), which no store of a running build does while
//! [`crate::commons::hearing::READS_FORMAT`] names no release.
//!
//! - **Shedding needs no vote** ([`Log::sheddable`], [`Store::shed_superseded`]). A version-2
//!   `Put` or `Delete` that a newer op of its own signer names as replaced
//!   ([`checkpoint::superseded`]) keeps its frame and loses its ciphertext: its id and signature
//!   stand, every reader still places and verifies it, and the keep set, the checkpoint body
//!   and every statement read only frames. A reader reads a shed op as one it did not open
//!   ([`super::entries`]): a version-2 op is read by its statement, and a version-1 op reads
//!   its register by the values in its past, so none is shed that lies in a held version-1
//!   op's past (no writer signs one there: a log that reads format 2 drafts its `Put`s and
//!   `Delete`s as version 2 alone, [`super::writer`]), and none that a held op carries (its
//!   draft is read, [`Log::sheddable`]). So no reading of
//!   any op changes when another is shed. The Commons audience's ops are not shed: its control
//!   winner is the causally newest of them, read by value. A shed rewrites the whole records
//!   file, so it waits until it is worth that ([`Log::shed_worth`]: the log needs the room, or
//!   the bodies free at least an eighth of its bytes). What it costs: a `Remove` whose cutoff
//!   falls between a seat's op and that seat's own op that replaced it stops the replacement
//!   counting, and the replaced op is the entry's again; a node that shed it reads that entry
//!   without it until it takes the whole record back from a node that holds it (a whole record
//!   takes its frame's place, [`Store::insert`]). The same `Remove` behind a final cut costs the
//!   same in a pruned log (the design's section 5); here it is not bounded by a cut.
//! - **Who proposes** ([`Log::share_draft`]). A seat shares the checkpoint its log proposes
//!   ([`checkpoint::propose`]) where [`checkpoint::may_share`] lets it and it has not shared it
//!   already: it joins a body another seat shared, and proposes one itself only where its log
//!   needs the room ([`Log::wants_room`]: past the automatic writer's byte stop, or past the
//!   automatic keyed stop) and no share of its own, signed within
//!   [`finality::HEARD_WITHIN_MS`], still waits on a body no root holds. So a quiet vault signs
//!   no share, once one Mac's log fills the others join its body as they hear it on its day
//!   (two Macs that hold the same records before the day propose the same body), and a vault
//!   whose bodies do not become final gains at most one round of shares a seat starts each
//!   [`finality::HEARD_WITHIN_MS`], not one a day. A share is a membership-class record: it is
//!   weighed by the byte bound alone and by none of the keyed stop's room, which is kept for it
//!   ([`super::budgets::Authoring::keyed_refusal`]).
//! - **Whose shares a prune waits for** ([`Log::to_prune`]; follow's "A cut's version-1 stubs
//!   in a vault that grew", settled). A node prunes at a checkpoint [`checkpoint::judge`] finds
//!   final (more than half of the cut's recent seats). Where the prune would leave a stub in the
//!   cut that does not check itself against its id (a version-1 record's), its shares must also
//!   be more than half of the seats its own log holds at the prune: a Mac behind the cut that
//!   reads such a stub takes the root only on that count ([`super::served`]), the seats admitted
//!   since the cut among them, so no Mac a pruned node serves refuses its root for want of
//!   shares. A cut that drops only version-2 records waits for its recent seats alone, so
//!   dormant seats hold back no prune of them. A cut that drops version-1 history waits for
//!   more than half of all seats: a vault of three seats, two of which shared, that admits a
//!   fourth prunes it once a third seat shares, and a vault most of whose seats are dormant
//!   prunes no version-1 record (its version-2 records it sheds, with no vote).

use super::checkpoint::{self, checkpoint_id};
use super::{
    Body, Class, Draft, EpochAudience, Kind, Log, MemberId, Record, RecordId, Store, budgets,
};
use crate::Result;
use crate::commons::finality;
use std::collections::{BTreeMap, BTreeSet};

/// The share of a log's bytes ([`Log::total_bytes`]) the ciphertext a shed frees must reach,
/// as its denominator, for a log that does not need the room ([`Log::shed_worth`]): an eighth.
pub const SHED_SHARE: usize = 8;

impl Log {
    /// The held whole version-2 `Put`s and `Delete`s a newer op of their own signer replaced
    /// ([`checkpoint::superseded`]), outside the Commons audience, that no held version-2 op
    /// stands for ([`Log::frame_reps`]) and that lie in the causal past of no held version-1
    /// `Put` or `Delete` of their audience: what the shedding pass sheds. A carry's rep is read
    /// by its draft (the Claude token's settle reads the `Put`s a pair's meta stands for,
    /// `Log::everyone_puts_of_drafts`), so its body is kept as the keep set keeps it. A
    /// version-1 op states nothing and reads its register by the values in its past (a reseal
    /// stands for the earliest op of the value it writes again), so a body shed there would
    /// change which op of the register wins; no writer signs one there (a log that reads format
    /// 2 drafts its `Put`s and `Delete`s as version 2 alone, [`super::writer`]), so this keeps
    /// back only the past of one a build that erred, or a stolen key, signed.
    #[must_use]
    pub fn sheddable(&self) -> BTreeSet<RecordId> {
        let superseded = checkpoint::superseded(self);
        if superseded.is_empty() {
            return superseded;
        }
        let carried: BTreeSet<RecordId> = self.frame_reps().into_values().collect();
        let candidates: Vec<(usize, &EpochAudience)> = self
            .records()
            .enumerate()
            .filter_map(|(at, record)| {
                let (Body::Put { audience, .. } | Body::Delete { audience, .. }) = record.body()
                else {
                    return None;
                };
                (superseded.contains(record.id())
                    && !carried.contains(record.id())
                    && record.version() == super::VERSION_TWO
                    && !record.is_shed()
                    && *audience != EpochAudience::Commons)
                    .then_some((at, audience))
            })
            .collect();
        let Some(&(first, _)) = candidates.first() else {
            return BTreeSet::new();
        };
        #[cfg(test)]
        let first = if mutant(Mutant::ShedUnderVersionOne) {
            usize::MAX
        } else {
            first
        };
        // The held version-1 `Put`s and `Delete`s placed after a candidate.
        let later_ones: Vec<(usize, &EpochAudience)> = self
            .records()
            .enumerate()
            .skip(first.saturating_add(1))
            .filter(|(_, record)| record.version() == super::VERSION)
            .filter_map(|(at, record)| match record.body() {
                Body::Put { audience, .. } | Body::Delete { audience, .. } => Some((at, audience)),
                _ => None,
            })
            .collect();
        candidates
            .into_iter()
            .filter(|&(at, audience)| {
                !later_ones
                    .iter()
                    .any(|&(one, of)| of == audience && self.reach.before(at, one))
            })
            .filter_map(|(at, _)| self.order.get(at).map(|record| *record.id()))
            .collect()
    }

    /// Whether shedding `shed` is worth a rewrite of the whole records file: this log needs the
    /// room ([`Log::wants_room`]), or the ciphertext it frees is at least [`SHED_SHARE`] of the
    /// bytes it holds. So the pass rewrites the file for bytes freed, not for each write that
    /// replaced another.
    #[must_use]
    pub fn shed_worth(&self, shed: &BTreeSet<RecordId>) -> bool {
        let freed: usize = shed
            .iter()
            .filter_map(|id| self.get(id))
            .map(|record| match record.body() {
                Body::Put { ciphertext, .. } | Body::Delete { ciphertext, .. } => ciphertext.len(),
                _ => 0,
            })
            .sum();
        self.wants_room() || freed.saturating_mul(SHED_SHARE) >= self.total_bytes()
    }

    /// Whether this log needs the room a prune frees: its bytes are past the automatic
    /// writer's stop of its byte bound, or its keyed records past the automatic keyed stop.
    #[must_use]
    pub fn wants_room(&self) -> bool {
        let bound = budgets::held_bytes_within(self.stored_at);
        self.total_bytes() > Class::Automatic.stop_within(bound)
            || self.counts.keyed > self.budgets.automatic_keyed_stop()
    }

    /// The share `seat` signs at `now_ms`, unsigned: of the checkpoint this log proposes
    /// ([`checkpoint::propose`]), where [`checkpoint::may_share`] lets `seat` share it, no record
    /// of `seat`'s here shares it already, no root of this log holds its cut, and either another
    /// seat shared that body (`seat` joins it) or this log needs the room ([`Log::wants_room`])
    /// and `seat` holds no share of its own, signed within [`finality::HEARD_WITHIN_MS`] of
    /// `now_ms`, of a body no root of this log holds the cut of (`seat` proposes it). `None`
    /// otherwise, and before any proposal is computed where this log needs no room and holds no
    /// such share of another seat's.
    ///
    /// # Errors
    /// As [`checkpoint::may_share`].
    pub fn share_draft(&self, seat: &MemberId, now_ms: u64) -> Result<Option<Draft>> {
        let unnested: Vec<&Record> = self
            .records()
            .filter(|record| record.kind() == Kind::Checkpoint && !self.nests(record.body()))
            .collect();
        let others = unnested.iter().any(|record| record.actor() != seat);
        let pending = unnested.iter().any(|record| {
            record.actor() == seat
                && record
                    .authored_ms()
                    .saturating_add(finality::HEARD_WITHIN_MS)
                    > now_ms
        });
        #[cfg(test)]
        let pending = pending && !mutant(Mutant::SharesDaily);
        let proposes = !pending && self.wants_room();
        let idle = !(others || proposes);
        #[cfg(test)]
        let idle = idle && !mutant(Mutant::ProposeFirst);
        if idle {
            return Ok(None);
        }
        let Some(plan) = checkpoint::propose(self, now_ms) else {
            return Ok(None);
        };
        let id = checkpoint_id(&plan.body);
        let shares = |record: &&Record| checkpoint_id(record.body()) == id;
        let joins = unnested
            .iter()
            .any(|record| record.actor() != seat && shares(record));
        if self.nests(&plan.body)
            || self
                .records()
                .any(|record| record.actor() == seat && shares(&record))
            || !(joins || proposes)
            || !checkpoint::may_share(self, seat, &plan.body)?
        {
            return Ok(None);
        }
        Ok(Some(self.draft(seat, now_ms, plan.body)))
    }

    /// The checkpoint this log prunes at next: of the bodies its shares sign that no root of
    /// it holds the cut of, the latest by its cut and then the lowest id among those
    /// [`checkpoint::judge`] finds final, and, where its prune would leave a stub in the cut that
    /// does not check itself (a version-1 record's), whose shares are also more than half of the
    /// seats this log holds now (the module's "Whose shares a prune waits for"). `None` where
    /// none is.
    #[must_use]
    pub fn to_prune(&self) -> Option<Body> {
        let seats: BTreeSet<MemberId> = self.membership_ref().members().keys().copied().collect();
        let mut bodies: BTreeMap<RecordId, &Body> = BTreeMap::new();
        for record in self.records() {
            if let Some(id) = checkpoint_id(record.body()) {
                bodies.entry(id).or_insert_with(|| record.body());
            }
        }
        let mut best: Option<(u64, RecordId, &Body)> = None;
        for (id, body) in bodies {
            let Body::Checkpoint { cut_ms, heads, .. } = body else {
                continue;
            };
            if self.nests(body)
                || best.is_some_and(|(cut, at, _)| *cut_ms < cut || (*cut_ms == cut && id > at))
            {
                continue;
            }
            let Ok(judgment) = checkpoint::judge(self, body) else {
                continue;
            };
            if !judgment.is_final {
                continue;
            }
            if !finality::is_final(&seats, &judgment.signed) && !self.self_checked(*cut_ms, heads) {
                continue;
            }
            best = Some((*cut_ms, id, body));
        }
        best.map(|(_, _, body)| body.clone())
    }

    /// Whether every stub a prune at the cut `cut_ms`, `heads` leaves in that cut checks itself
    /// against its id ([`super::rooted::Stub::frame`]): the stubs of the records it drops and
    /// those this log holds there already. A Mac behind the cut that reads only such stubs takes
    /// the root on its cut's recent seats' shares ([`super::served`]); one that reads a
    /// version-1 record's stub waits for more than half of its own seats.
    fn self_checked(&self, cut_ms: u64, heads: &[RecordId]) -> bool {
        #[cfg(test)]
        if mutant(Mutant::AllSeatsAlways) {
            return false;
        }
        let Ok(plan) = checkpoint::plan_at(self, cut_ms, heads) else {
            return false;
        };
        plan.stubs.values().all(|stub| stub.frame.is_some())
            && self
                .stubs_in_past(heads)
                .iter()
                .filter_map(|id| self.stub(id))
                .all(|stub| stub.frame.is_some())
    }
}

/// A broken pass, for the tests' negative controls.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mutant {
    /// A seat proposes a new body whenever its log needs the room, whatever share of its own
    /// still waits.
    SharesDaily,
    /// A seat computes the proposal before asking whether its log needs the room or holds
    /// another seat's share.
    ProposeFirst,
    /// A prune waits for more than half of the seats its log holds whatever stubs it leaves.
    AllSeatsAlways,
    /// A vault server judges what to prune after no take: what a prune turns on reads as
    /// never changing.
    NeverPrunes,
    /// The shedding pass sheds a body in the past of a held version-1 op of its audience.
    ShedUnderVersionOne,
}

#[cfg(test)]
thread_local! {
    /// The mutant this thread's pass is computed as, if any.
    pub(super) static MUTANT: std::cell::Cell<Option<Mutant>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn mutant(which: Mutant) -> bool {
    MUTANT.get() == Some(which)
}

impl Store {
    /// What the checkpoint this store prunes at turns on ([`Log::to_prune`]), counted: the
    /// shares, `Admit`s and `Remove`s it holds (which bodies there are, the seats a prune counts
    /// and which shares count). A held share's cut lies in its past, so the share comes after
    /// every record its body is recomputed from. A take that leaves the count as it was makes
    /// no body final that was not, so a vault server judges what to prune only after a take
    /// that changes it. A share that waited for a parent counts once it is held, in the take
    /// that brought the parent.
    #[must_use]
    pub(crate) fn prune_inputs(&self) -> usize {
        #[cfg(test)]
        if mutant(Mutant::NeverPrunes) {
            return 0;
        }
        self.log().map_or(0, |log| {
            log.records()
                .filter(|record| {
                    matches!(record.kind(), Kind::Checkpoint | Kind::Admit | Kind::Remove)
                })
                .count()
        })
    }

    /// This store with its sheddable bodies shed ([`Log::sheddable`]) and the ids it shed,
    /// ascending; `None` where it reads no format 2, sheds nothing, or what it would shed is not
    /// worth a rewrite ([`Log::shed_worth`]).
    ///
    /// # Errors
    /// The log cannot be rebuilt with the shed frames ([`Log::with_shed`]).
    pub(crate) fn shed_superseded(&self) -> Result<Option<(Self, Vec<RecordId>)>> {
        if !self.reading().admits_two() {
            return Ok(None);
        }
        let Some(log) = self.log() else {
            return Ok(None);
        };
        let shed = log.sheddable();
        if shed.is_empty() || !log.shed_worth(&shed) {
            return Ok(None);
        }
        let mut next = self.clone();
        next.log = Some(log.with_shed(&shed)?);
        #[cfg(any(unix, windows))]
        {
            next.file_order = None;
        }
        Ok(Some((next, shed.into_iter().collect())))
    }

    /// This store pruned at each final checkpoint [`Log::to_prune`] names in turn, and the id
    /// of the last; `None` where it reads no format 2 or none is to prune at.
    ///
    /// # Errors
    /// A prune fails ([`Log::adopt`]).
    pub(crate) fn adopt_final(&self) -> Result<Option<(Self, RecordId)>> {
        if !self.reading().admits_two() {
            return Ok(None);
        }
        let Some(mut log) = self.log().cloned() else {
            return Ok(None);
        };
        let mut last = None;
        while let Some(body) = log.to_prune() {
            let id = checkpoint_id(&body);
            let next = log.adopt(&body)?;
            if !next.nests(&body) {
                break;
            }
            log = next;
            last = id;
        }
        let Some(last) = last else {
            return Ok(None);
        };
        let mut next = self.clone();
        next.log = Some(log);
        #[cfg(any(unix, windows))]
        {
            next.file_order = None;
        }
        Ok(Some((next, last)))
    }
}

#[cfg(test)]
#[path = "pass_tests.rs"]
pub(crate) mod tests;
