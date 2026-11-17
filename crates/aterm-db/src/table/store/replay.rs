// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Witness verification precedes historical classification. A crash tail never
//! acquires the authority of bytes already acknowledged in independent custody.

use super::group::Boundary;
use super::{CONFLICT_SUBJECT, Receipt, SUBJECT, Stored, Witness};
use crate::{
    Error, Result,
    table::{
        Envelope, ReadBatch,
        signatures::{Facts, Identified},
    },
};
use astream_wire::{Offset, sync::Digest};
use aterm_digest::Sha256;
use aterm_journal::Log;
use std::borrow::Cow;

pub(super) struct Replay {
    pub entries: Vec<Entry>,
    pub witness: Witness,
}

pub(super) struct Entry {
    pub received_ms: u64,
    pub offset: u64,
    /// The record's bytes and content identifier, taken once as they were read.
    pub wire: Identified,
    pub conflict: bool,
}

impl Entry {
    /// The immutable identified wire already owns this operation's digest.
    /// Form the complete receipt when consumed instead of duplicating it in
    /// every temporary replay descriptor.
    pub(super) fn receipt(&self) -> Receipt {
        Receipt {
            id: self.wire.id(),
            received_ms: self.received_ms,
            offset: self.offset,
        }
    }
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbTableQuarantine",
        action = "ChangedWitness",
        project = "aterm_db::table::store::replay::tests::quarantine_transition"
    )
)]
pub(super) fn read(
    log: &mut Log,
    acknowledged: impl Into<Boundary>,
    pending: bool,
) -> Result<Replay> {
    let mut replay = Builder::new(acknowledged, pending);
    let mut offset = Offset::ZERO;
    loop {
        let batch = log.read_range(offset, 256)?;
        if batch.is_empty() {
            break;
        }
        for entry in batch {
            offset = replay.push(std::sync::Arc::unwrap_or_clone(entry))?;
        }
    }
    replay.finish()
}

/// [`read`] of the records after the prefix `held` witnesses, which a store kept across sessions
/// already holds (`super::kept`): each record from `held.count` on is identified and its chain
/// continued from `held`, and the claim judged as a whole replay judges it. The prefix itself is
/// not read again; `last` judges the record that ends it, which the log must still hold: its
/// identifier, receipt time and conflict flag as the kept store holds that offset.
///
/// # Errors
/// As [`read`]; the claim acknowledges fewer records than `held` or another prefix; or the log
/// no longer holds the prefix's last record as `last` judges it.
pub(super) fn read_after(
    log: &mut Log,
    held: Witness,
    acknowledged: impl Into<Boundary>,
    last: impl FnOnce(&Digest, u64, bool) -> bool,
) -> Result<Replay> {
    let mut replay = Builder::after(held, acknowledged)?;
    if let Some(before) = held.count.checked_sub(1) {
        let record = log
            .read_range(Offset(before), 1)?
            .into_iter()
            .next()
            .filter(|record| record.seq.0 == before)
            .ok_or_else(|| Error::new("table history lost acknowledged custody records"))?;
        // The chain before it is the kept store's: only the record's own bytes are judged here.
        let (wire, received_ms, conflict) = parts(std::sync::Arc::unwrap_or_clone(record))?;
        if !last(&Sha256::digest(&wire), received_ms, conflict) {
            return Err(Error::new(
                "acknowledged table history differs from custody witness",
            ));
        }
    }
    let mut offset = Offset(held.count);
    loop {
        let batch = log.read_range(offset, 256)?;
        if batch.is_empty() {
            break;
        }
        for entry in batch {
            offset = replay.push(std::sync::Arc::unwrap_or_clone(entry))?;
        }
    }
    replay.finish()
}

/// One journal record as its bytes alone give it: its wire and that wire's SHA-256
/// ([`Identified`]), its local receipt time, whether it is a conflict certificate, and the custody
/// chain through it ([`Witness::advance`]), whose count names its offset. A function of the record
/// and the chain before it alone, so it stays free of any claim, session, key or clock input: a
/// kept table journal ([`super::journals`]) answers every claim from it ([`witnessed`]).
#[derive(Clone)]
pub(super) struct Linked {
    pub wire: Identified,
    pub received_ms: u64,
    pub conflict: bool,
    pub through: Witness,
}
impl Linked {
    /// The record's offset: the chain through it counts it and every record before it.
    const fn offset(&self) -> u64 {
        self.through.count - 1
    }

    fn entry(self) -> Entry {
        Entry {
            received_ms: self.received_ms,
            offset: self.offset(),
            wire: self.wire,
            conflict: self.conflict,
        }
    }
}

/// The record-only steps of a replay's push, in its order: a record of neither table subject, or
/// one too short for its receipt time, is foreign; the receipt time is taken off the body, which
/// leaves the wire, identified once; the chain `before` it advances through it, which refuses a
/// record out of order.
fn identify(record: astream_broker::BrokerRecord, before: Witness) -> Result<Linked> {
    identify_with(record, before, None)
}

/// A table journal record's wire, receipt time and conflict flag: a record of neither table
/// subject, or one too short for its receipt time, is foreign.
fn parts(record: astream_broker::BrokerRecord) -> Result<(Vec<u8>, u64, bool)> {
    if !matches!(record.subject.as_str(), SUBJECT | CONFLICT_SUBJECT) || record.body.len() < 8 {
        return Err(Error::new("foreign table journal record"));
    }
    let received_ms = u64::from_le_bytes(record.body[..8].try_into().unwrap());
    let conflict = record.subject == CONFLICT_SUBJECT;
    let mut wire = record.body;
    wire.drain(..8);
    Ok((wire, received_ms, conflict))
}

fn identify_with(
    record: astream_broker::BrokerRecord,
    before: Witness,
    facts: Option<&Facts>,
) -> Result<Linked> {
    let seq = record.seq.0;
    let (wire, received_ms, conflict) = parts(record)?;
    let wire = match facts {
        Some(facts) => Identified::new_with(wire, Some(facts)),
        None => Identified::new(wire),
    };
    let through = before.advance(seq, wire.id(), received_ms)?;
    Ok(Linked {
        wire,
        received_ms,
        conflict,
        through,
    })
}

/// Identify `records` onto `links`, the chain continuing from the last of `links`; the first
/// refusal stops it, leaving those identified before it.
pub(super) fn identify_onto(
    links: &mut Vec<Linked>,
    records: Vec<astream_broker::BrokerRecord>,
) -> Result<()> {
    identify_onto_with(links, records, None)
}

/// Identify freshly read bytes before attaching any retained cryptographic facts.
pub(super) fn identify_onto_with(
    links: &mut Vec<Linked>,
    records: Vec<astream_broker::BrokerRecord>,
    facts: Option<&Facts>,
) -> Result<()> {
    for record in records {
        let before = links
            .last()
            .map_or_else(Witness::default, |link| link.through);
        links.push(identify_with(record, before, facts)?);
    }
    Ok(())
}

/// A replay's claim steps against the witness the enrollment claim acknowledges: the chain may run
/// one record past it without a group permit, or to an independently permitted exact group
/// endpoint. It must equal the acknowledged prefix where its count matches, and reach that
/// prefix by the end. An unfinished permitted group is never an accepted checkpoint.
struct Claim {
    acknowledged: Witness,
    end: Option<Witness>,
    through: Witness,
    prefix_checked: bool,
}
impl Claim {
    /// The claim's steps for a chain that continues from `held`, a prefix a kept store holds:
    /// the claim must acknowledge at least that prefix, and exactly it where it acknowledges as
    /// many records.
    fn after(boundary: impl Into<Boundary>, held: Witness) -> Result<Self> {
        let boundary = boundary.into();
        let acknowledged = boundary.acknowledged();
        if acknowledged.count < held.count
            || (acknowledged.count == held.count && acknowledged != held)
        {
            return Err(Error::new(
                "acknowledged table history differs from custody witness",
            ));
        }
        Ok(Self {
            acknowledged,
            end: boundary.end(),
            through: held,
            prefix_checked: acknowledged == held,
        })
    }

    fn new(boundary: impl Into<Boundary>) -> Self {
        let boundary = boundary.into();
        let acknowledged = boundary.acknowledged();
        let end = boundary.end();
        Self {
            acknowledged,
            end,
            through: Witness::default(),
            prefix_checked: acknowledged == Witness::default(),
        }
    }

    /// Admit the chain `through` the next record.
    fn admit(&mut self, through: Witness) -> Result<()> {
        let limit = self
            .end
            .map_or(self.acknowledged.count + 1, |end| end.count);
        if through.count > limit {
            return Err(Error::new(if self.end.is_some() {
                "table log exceeds its independent checkpoint group allowance"
            } else {
                "table log exceeds its single unwitnessed append allowance"
            }));
        }
        if through.count == self.acknowledged.count {
            if through != self.acknowledged {
                return Err(Error::new(
                    "acknowledged table history differs from custody witness",
                ));
            }
            self.prefix_checked = true;
        }
        self.through = through;
        Ok(())
    }

    fn finish(&self) -> Result<()> {
        if !self.prefix_checked {
            return Err(Error::new(
                "table history lost acknowledged custody records",
            ));
        }
        if self.through.count > self.acknowledged.count
            && self.end.is_some_and(|end| self.through != end)
        {
            return Err(Error::new(
                "table checkpoint differs from its exact group permission",
            ));
        }
        Ok(())
    }
}

/// An observer exposes only independently witnessed receipts. A sealed append
/// awaiting its custody witness is left to the exclusive writer's recovery path.
/// `links` are a journal's identified records ([`identify_onto`]) and `refused` the refusal
/// that stopped their identification, if any. Checking the contiguous chain's acknowledged
/// prefix and final endpoint answers as admitting every link in order, then the identification
/// refusal, then requiring the chain to have reached the claim. It preserves a replay's first
/// refusal while holding only the first `acknowledged.count` records under `acknowledged`.
/// Links a reader keeps are copied into the replay; links nothing else holds are moved.
pub(super) fn witnessed(
    links: Cow<'_, [Linked]>,
    refused: Option<Error>,
    acknowledged: impl Into<Boundary>,
) -> Result<Replay> {
    let boundary = acknowledged.into();
    let acknowledged = boundary.acknowledged();
    let count = witnessed_count(&links, refused, boundary)?;
    let entries = match links {
        Cow::Owned(links) => links.into_iter().take(count).map(Linked::entry).collect(),
        Cow::Borrowed(links) => links
            .iter()
            .take(count)
            .cloned()
            .map(Linked::entry)
            .collect(),
    };
    Ok(Replay {
        entries,
        witness: acknowledged,
    })
}

/// [`witnessed`] of the records after the prefix `held` witnesses, which a store kept across
/// sessions already holds (`super::kept`): the same judgments of the claim, and the chain through
/// the prefix's last link must be `held`, so these links continue the kept records. `None` where
/// it is not: the caller replays the journal in full.
pub(super) fn witnessed_after(
    links: &[Linked],
    refused: Option<Error>,
    acknowledged: impl Into<Boundary>,
    held: Witness,
) -> Result<Option<Replay>> {
    let boundary = acknowledged.into();
    let count = witnessed_count(links, refused, boundary)?;
    let start = usize::try_from(held.count)
        .map_err(|_| Error::new("table witness count exceeds this reader"))?;
    if start > count {
        return Ok(None);
    }
    let through = start
        .checked_sub(1)
        .map_or(Some(Witness::default()), |last| {
            links.get(last).map(|link| link.through)
        });
    if through != Some(held) {
        return Ok(None);
    }
    Ok(Some(Replay {
        entries: links[start..count]
            .iter()
            .cloned()
            .map(Linked::entry)
            .collect(),
        witness: boundary.acknowledged(),
    }))
}

/// Custody-verified operation IDs for repair ordering alone. No envelope is authenticated or
/// classified here; these IDs cannot establish an inventory or convergence evidence.
pub(super) fn repair_ids(
    links: &[Linked],
    refused: Option<Error>,
    acknowledged: impl Into<Boundary>,
) -> Result<std::collections::BTreeSet<astream_wire::sync::Digest>> {
    let count = witnessed_count(links, refused, acknowledged.into())?;
    Ok(links[..count]
        .iter()
        .filter(|link| !link.conflict)
        .map(|link| link.wire.id())
        .collect())
}

fn witnessed_count(links: &[Linked], refused: Option<Error>, boundary: Boundary) -> Result<usize> {
    let mut claim = Claim::new(boundary);
    // identify_onto already established the contiguous custody chain. Only its
    // acknowledged endpoint can disagree with the claim's hash; only its last
    // endpoint can exceed the allowed tail. Check the prefix first, preserving
    // its refusal ahead of a later tail or identification failure.
    if let Some(link) = boundary
        .acknowledged()
        .count
        .checked_sub(1)
        .and_then(|index| usize::try_from(index).ok())
        .and_then(|index| links.get(index))
    {
        claim.admit(link.through)?;
    }
    if let Some(last) = links.last() {
        claim.admit(last.through)?;
    }
    if let Some(refused) = refused {
        return Err(refused);
    }
    claim.finish()?;
    usize::try_from(boundary.acknowledged().count)
        .map_err(|_| Error::new("table witness count exceeds this reader"))
}

/// [`witnessed`] of `records` identified afresh.
pub(super) fn read_observed(
    records: Vec<astream_broker::BrokerRecord>,
    acknowledged: impl Into<Boundary>,
) -> Result<Replay> {
    let mut links = Vec::with_capacity(records.len());
    let refused = identify_onto(&mut links, records).err();
    witnessed(Cow::Owned(links), refused, acknowledged)
}

/// A writer's replay: each record identified ([`identify`]) and admitted against the claim
/// ([`Claim`]) in turn, the independently allowed unwitnessed tail included.
struct Builder {
    pending: bool,
    witness: Witness,
    claim: Claim,
    entries: Vec<Entry>,
}
impl Builder {
    fn new(boundary: impl Into<Boundary>, pending: bool) -> Self {
        let boundary = boundary.into();
        let cap = usize::try_from(boundary.acknowledged().count)
            .unwrap_or(0)
            .min(super::journal_records());
        Self {
            pending,
            witness: Witness::default(),
            claim: Claim::new(boundary),
            entries: Vec::with_capacity(cap),
        }
    }

    /// A writer's replay of the records after `held` ([`read_after`]).
    fn after(held: Witness, boundary: impl Into<Boundary>) -> Result<Self> {
        Ok(Self {
            pending: false,
            witness: held,
            claim: Claim::after(boundary, held)?,
            entries: Vec::new(),
        })
    }

    fn push(&mut self, entry: astream_broker::BrokerRecord) -> Result<Offset> {
        if self.pending {
            return Err(Error::new("unfinished table enrollment contains records"));
        }
        let seq = entry.seq.0;
        let link = identify(entry, self.witness)?;
        self.witness = link.through;
        self.claim.admit(link.through)?;
        self.entries.push(link.entry());
        Ok(Offset(
            seq.checked_add(1)
                .ok_or_else(|| Error::new("table offset exhausted"))?,
        ))
    }

    fn finish(self) -> Result<Replay> {
        self.claim.finish()?;
        Ok(Replay {
            entries: self.entries,
            witness: self.witness,
        })
    }
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbTableQuarantine",
        action = "Intact",
        project = "aterm_db::table::store::replay::tests::quarantine_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbTableQuarantine",
        action = "ForeignDiscarded",
        project = "aterm_db::table::store::replay::tests::quarantine_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbTableQuarantine",
        action = "OwnDiscarded",
        project = "aterm_db::table::store::replay::tests::quarantine_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbTableQuarantine",
        action = "DiscardedTail",
        project = "aterm_db::table::store::replay::tests::quarantine_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbTableQuarantine",
        action = "MissingSelectedKey",
        project = "aterm_db::table::store::replay::tests::quarantine_transition"
    )
)]
pub(super) fn restore(
    batch: &ReadBatch<'_, '_>,
    receipt: Receipt,
    wire: Identified,
    acknowledged: u64,
    catalog: bool,
    read_only: bool,
) -> Result<(Stored, crate::table::Class)> {
    let scope = batch.scope;
    // Store::open_claimed checks live authority around this held-source replay
    // and before finalization. Keep signed-envelope and historical validation
    // per record without repeating those filesystem checks for every entry.
    // `catalog` is whether `scope` is the catalog, which the open judged once.
    let envelope = Envelope::parse_identified(scope, &wire)?;
    super::operation_policy_in(catalog, &envelope.header, envelope.body.len() - 16)?;
    let (payload, control) = if aterm_db_core::quarantine_eligible(
        acknowledged,
        receipt.offset,
        batch.discarded_source(&envelope)?,
    ) {
        (
            super::PayloadState::Quarantined(envelope.roster, envelope.epoch, envelope.authored_ms),
            super::Control::default(),
        )
    } else {
        match batch.replay_metadata(&wire, &envelope) {
            Ok(control) => (super::PayloadState::Ready, control),
            Err(error)
                if read_only && receipt.offset < acknowledged && error.is_awaiting_membership() =>
            {
                (
                    super::PayloadState::AwaitingMembership,
                    super::Control::default(),
                )
            }
            Err(error) => return Err(error),
        }
    };
    let class = crate::table::Class::of(&envelope);
    let header = envelope.header;
    if header.id != receipt.id {
        return Err(Error::new("replayed receipt differs from ciphertext"));
    }
    Ok((
        Stored {
            wire: super::StoredWire::Shared(wire),
            header,
            receipt,
            payload,
            cut: control.cut,
            checkpoint: control.checkpoint,
        },
        class,
    ))
}

#[cfg(test)]
mod tests;
