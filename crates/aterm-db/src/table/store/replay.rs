// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Witness verification precedes historical classification. A crash tail never
//! acquires the authority of bytes already acknowledged in independent custody.

use super::{CONFLICT_SUBJECT, Receipt, SUBJECT, Stored, Witness};
use crate::{
    Error, Result,
    table::{Scope, Session},
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
    let mut witness = Witness::default();
    let mut prefix_checked = acknowledged == witness;
    let mut entries = Vec::new();
    let mut offset = Offset::ZERO;
    loop {
        let batch = log.read_range(offset, 256)?;
        if batch.is_empty() {
            break;
        }
        if pending {
            return Err(Error::new("unfinished table enrollment contains records"));
        }
        for entry in batch {
            if !matches!(entry.subject.as_str(), SUBJECT | CONFLICT_SUBJECT) || entry.body.len() < 8
            {
                return Err(Error::new("foreign table journal record"));
            }
            let received_ms = u64::from_le_bytes(entry.body[..8].try_into().unwrap());
            let wire = entry.body[8..].to_vec();
            let id = Sha256::digest(&wire);
            witness = witness.advance(entry.seq.0, id, received_ms)?;
            if witness.count > acknowledged.count + 1 {
                return Err(Error::new(
                    "table log exceeds its single unwitnessed append allowance",
                ));
            }
            if witness.count == acknowledged.count {
                if witness != acknowledged {
                    return Err(Error::new(
                        "acknowledged table history differs from custody witness",
                    ));
                }
                prefix_checked = true;
            }
            entries.push(Entry {
                receipt: Receipt {
                    id,
                    received_ms,
                    offset: entry.seq.0,
                },
                wire,
                conflict: entry.subject == CONFLICT_SUBJECT,
            });
            offset = Offset(
                entry
                    .seq
                    .0
                    .checked_add(1)
                    .ok_or_else(|| Error::new("table offset exhausted"))?,
            );
        }
    }
    if !prefix_checked {
        return Err(Error::new(
            "table history lost acknowledged custody records",
        ));
    }
    Ok(Replay { entries, witness })
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
    session: &Session<'_>,
    scope: Scope,
    receipt: Receipt,
    wire: Vec<u8>,
    acknowledged: u64,
) -> Result<Stored> {
    let envelope = session.envelope(scope, &wire)?;
    let header = envelope.header.clone();
    let quarantine = if aterm_db_core::quarantine_eligible(
        acknowledged,
        receipt.offset,
        session.discarded_source(&envelope),
    ) {
        Some((envelope.roster, envelope.epoch, envelope.authored_ms))
    } else {
        session.open_envelope(scope, envelope)?;
        None
    };
    if header.id != receipt.id {
        return Err(Error::new("replayed receipt differs from ciphertext"));
    }
    Ok(Stored {
        wire,
        header,
        receipt,
        quarantine,
    })
}

#[cfg(test)]
mod tests;
