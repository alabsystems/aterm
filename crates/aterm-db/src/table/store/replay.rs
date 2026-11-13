// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Witness verification precedes historical classification. A crash tail never
//! acquires the authority of bytes already acknowledged in independent custody.

use super::{CONFLICT_SUBJECT, Receipt, SUBJECT, Stored, Witness};
use crate::{
    Error, Result,
    table::{Envelope, ReadBatch, signatures::Identified},
};
use astream_wire::Offset;
use aterm_journal::Log;
use std::borrow::Cow;

pub(super) struct Replay {
    pub entries: Vec<Entry>,
    pub witness: Witness,
}

pub(super) struct Entry {
    pub receipt: Receipt,
    /// The record's bytes, which `receipt.id` names: their identifier, taken once as they were read.
    pub wire: Identified,
    pub conflict: bool,
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbTableQuarantine",
        action = "ChangedWitness",
        project = "aterm_db::table::store::replay::tests::quarantine_transition"
    )
)]
pub(super) fn read(log: &mut Log, acknowledged: Witness, pending: bool) -> Result<Replay> {
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
            receipt: Receipt {
                id: self.wire.id(),
                received_ms: self.received_ms,
                offset: self.offset(),
            },
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
    if !matches!(record.subject.as_str(), SUBJECT | CONFLICT_SUBJECT) || record.body.len() < 8 {
        return Err(Error::new("foreign table journal record"));
    }
    let received_ms = u64::from_le_bytes(record.body[..8].try_into().unwrap());
    let conflict = record.subject == CONFLICT_SUBJECT;
    let mut wire = record.body;
    wire.drain(..8);
    let wire = Identified::new(wire);
    let through = before.advance(record.seq.0, wire.id(), received_ms)?;
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
    for record in records {
        let before = links
            .last()
            .map_or_else(Witness::default, |link| link.through);
        links.push(identify(record, before)?);
    }
    Ok(())
}

/// A replay's claim steps against the witness the enrollment claim acknowledges: the chain may run
/// one record past it (the sealed append a cut-off writer can leave), must equal it where it
/// counts as many records, and must have reached it by the end.
struct Claim {
    acknowledged: Witness,
    prefix_checked: bool,
}
impl Claim {
    fn new(acknowledged: Witness) -> Self {
        Self {
            acknowledged,
            prefix_checked: acknowledged == Witness::default(),
        }
    }

    /// Admit the chain `through` the next record.
    fn admit(&mut self, through: Witness) -> Result<()> {
        if through.count > self.acknowledged.count + 1 {
            return Err(Error::new(
                "table log exceeds its single unwitnessed append allowance",
            ));
        }
        if through.count == self.acknowledged.count {
            if through != self.acknowledged {
                return Err(Error::new(
                    "acknowledged table history differs from custody witness",
                ));
            }
            self.prefix_checked = true;
        }
        Ok(())
    }

    fn finish(&self) -> Result<()> {
        if !self.prefix_checked {
            return Err(Error::new(
                "table history lost acknowledged custody records",
            ));
        }
        Ok(())
    }
}

/// An observer exposes only independently witnessed receipts. A sealed append
/// awaiting its custody witness is left to the exclusive writer's recovery path.
/// `links` are a journal's identified records ([`identify_onto`]) and `refused` the refusal
/// that stopped their identification, if any: each link is admitted against `acknowledged` in
/// order, then the refusal stands, then the chain must have reached the claim, so the answer is
/// a replay's that pushed each record in turn, its first refusal included. The replay holds the
/// first `acknowledged.count` records under `acknowledged`.
/// Links a reader keeps are copied into the replay; links nothing else holds are moved.
pub(super) fn witnessed(
    links: Cow<'_, [Linked]>,
    refused: Option<Error>,
    acknowledged: Witness,
) -> Result<Replay> {
    let count = witnessed_count(&links, refused, acknowledged)?;
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

/// Custody-verified operation IDs for repair ordering alone. No envelope is authenticated or
/// classified here; these IDs cannot establish an inventory or convergence evidence.
pub(super) fn repair_ids(
    links: &[Linked],
    refused: Option<Error>,
    acknowledged: Witness,
) -> Result<std::collections::BTreeSet<astream_wire::sync::Digest>> {
    let count = witnessed_count(links, refused, acknowledged)?;
    Ok(links[..count]
        .iter()
        .filter(|link| !link.conflict)
        .map(|link| link.wire.id())
        .collect())
}

fn witnessed_count(
    links: &[Linked],
    refused: Option<Error>,
    acknowledged: Witness,
) -> Result<usize> {
    let mut claim = Claim::new(acknowledged);
    for link in links {
        claim.admit(link.through)?;
    }
    if let Some(refused) = refused {
        return Err(refused);
    }
    claim.finish()?;
    usize::try_from(acknowledged.count)
        .map_err(|_| Error::new("table witness count exceeds this reader"))
}

/// [`witnessed`] of `records` identified afresh.
pub(super) fn read_observed(
    records: Vec<astream_broker::BrokerRecord>,
    acknowledged: Witness,
) -> Result<Replay> {
    let mut links = Vec::with_capacity(records.len());
    let refused = identify_onto(&mut links, records).err();
    witnessed(Cow::Owned(links), refused, acknowledged)
}

/// A writer's replay: each record identified ([`identify`]) and admitted against the claim
/// ([`Claim`]) in turn, the one unwitnessed append included.
struct Builder {
    pending: bool,
    witness: Witness,
    claim: Claim,
    entries: Vec<Entry>,
}
impl Builder {
    fn new(acknowledged: Witness, pending: bool) -> Self {
        let cap = usize::try_from(acknowledged.count)
            .unwrap_or(0)
            .min(super::JOURNAL_RECORDS);
        Self {
            pending,
            witness: Witness::default(),
            claim: Claim::new(acknowledged),
            entries: Vec::with_capacity(cap),
        }
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
) -> Result<Stored> {
    let scope = batch.scope;
    // Store::open_region checks live authority around this held-source replay
    // and before finalization. Keep signed-envelope and historical validation
    // per record without repeating those filesystem checks for every entry.
    // `catalog` is whether `scope` is the catalog, which the open judged once.
    let envelope = Envelope::parse_identified(scope, &wire)?;
    super::operation_policy_in(catalog, &envelope.header, envelope.body.len() - 16)?;
    let (header, quarantine, cut) = if aterm_db_core::quarantine_eligible(
        acknowledged,
        receipt.offset,
        batch.discarded_source(&envelope)?,
    ) {
        (
            envelope.header,
            Some((envelope.roster, envelope.epoch, envelope.authored_ms)),
            None,
        )
    } else {
        let (header, cut) = batch.replay_metadata(&wire, envelope)?;
        (header, None, cut)
    };
    if header.id != receipt.id {
        return Err(Error::new("replayed receipt differs from ciphertext"));
    }
    Ok(Stored {
        wire: super::StoredWire::Shared(wire),
        header,
        receipt,
        quarantine,
        cut,
    })
}

#[cfg(test)]
mod tests;
