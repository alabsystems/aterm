// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Witness verification precedes historical classification. A crash tail never
//! acquires the authority of bytes already acknowledged in independent custody.

use super::{CONFLICT_SUBJECT, Receipt, SUBJECT, Stored, Witness};
use crate::{
    Error, Result,
    table::{Envelope, ReadBatch},
};
use astream_wire::Offset;
use aterm_digest::Sha256;
use aterm_journal::Log;

pub(super) struct Replay {
    pub entries: Vec<Entry>,
    pub witness: Witness,
}

pub(super) struct Entry {
    pub receipt: Receipt,
    pub wire: Vec<u8>,
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

/// An observer exposes only independently witnessed receipts. A sealed append
/// awaiting its custody witness is left to the exclusive writer's recovery path.
pub(super) fn read_observed(
    records: Vec<astream_broker::BrokerRecord>,
    acknowledged: Witness,
) -> Result<Replay> {
    let mut builder = Builder::new(acknowledged, false);
    for record in records {
        builder.push(record)?;
    }
    let mut replay = builder.finish()?;
    let count = usize::try_from(acknowledged.count)
        .map_err(|_| Error::new("table witness count exceeds this reader"))?;
    replay.entries.truncate(count);
    replay.witness = acknowledged;
    Ok(replay)
}

struct Builder {
    acknowledged: Witness,
    pending: bool,
    witness: Witness,
    prefix_checked: bool,
    entries: Vec<Entry>,
}
impl Builder {
    fn new(acknowledged: Witness, pending: bool) -> Self {
        let cap = usize::try_from(acknowledged.count)
            .unwrap_or(0)
            .min(super::JOURNAL_RECORDS);
        Self {
            acknowledged,
            pending,
            witness: Witness::default(),
            prefix_checked: acknowledged == Witness::default(),
            entries: Vec::with_capacity(cap),
        }
    }

    fn push(&mut self, entry: astream_broker::BrokerRecord) -> Result<Offset> {
        if self.pending {
            return Err(Error::new("unfinished table enrollment contains records"));
        }
        if !matches!(entry.subject.as_str(), SUBJECT | CONFLICT_SUBJECT) || entry.body.len() < 8 {
            return Err(Error::new("foreign table journal record"));
        }
        let received_ms = u64::from_le_bytes(entry.body[..8].try_into().unwrap());
        let id = Sha256::digest(&entry.body[8..]);
        self.witness = self.witness.advance(entry.seq.0, id, received_ms)?;
        if self.witness.count > self.acknowledged.count + 1 {
            return Err(Error::new(
                "table log exceeds its single unwitnessed append allowance",
            ));
        }
        if self.witness.count == self.acknowledged.count {
            if self.witness != self.acknowledged {
                return Err(Error::new(
                    "acknowledged table history differs from custody witness",
                ));
            }
            self.prefix_checked = true;
        }
        let mut wire = entry.body;
        wire.drain(..8);
        self.entries.push(Entry {
            receipt: Receipt {
                id,
                received_ms,
                offset: entry.seq.0,
            },
            wire,
            conflict: entry.subject == CONFLICT_SUBJECT,
        });
        Ok(Offset(
            entry
                .seq
                .0
                .checked_add(1)
                .ok_or_else(|| Error::new("table offset exhausted"))?,
        ))
    }

    fn finish(self) -> Result<Replay> {
        if !self.prefix_checked {
            return Err(Error::new(
                "table history lost acknowledged custody records",
            ));
        }
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
    wire: Vec<u8>,
    acknowledged: u64,
) -> Result<Stored> {
    let scope = batch.scope;
    // Store::open_region checks live authority around this held-source replay
    // and before finalization. Keep signed-envelope and historical validation
    // per record without repeating those filesystem checks for every entry.
    let envelope = Envelope::parse(scope, &wire)?;
    let header = envelope.header.clone();
    super::operation_policy(scope, &header, envelope.body.len() - 16)?;
    let (quarantine, cut) = if aterm_db_core::quarantine_eligible(
        acknowledged,
        receipt.offset,
        batch.discarded_source(&envelope)?,
    ) {
        (
            Some((envelope.roster, envelope.epoch, envelope.authored_ms)),
            None,
        )
    } else {
        let record = batch.open_envelope(envelope)?;
        (None, super::cutoff::decode(record.payload()))
    };
    if header.id != receipt.id {
        return Err(Error::new("replayed receipt differs from ciphertext"));
    }
    Ok(Stored {
        wire,
        header,
        receipt,
        quarantine,
        cut,
    })
}

#[cfg(test)]
mod tests;
