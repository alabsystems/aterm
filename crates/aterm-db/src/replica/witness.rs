// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHICH TABLES DIFFER, ASKED IN A FIRST ROUND (CR1). A round repairs one table, chosen by its
//! initiator, so a Mac returning to a peer whose tables moved on reached the moved one only at
//! its turn of the regular scan. On its first round with a peer (a restart, a new link, a long
//! gap), an initiator asks, and the responder states a short witness of each table both sides
//! list; the initiator repairs the tables whose witnesses differ before the regular turn
//! (`super::Round::exchange_tables`).
//!
//! - **On the wire.** Both ride in the table-ID lists the round already trades, as entries every
//!   receiver since 0.145 reads as a table it does not hold, within the bound it enforces
//!   (`super::MAX_TABLES`), as the clock mark does (`crate::clock`). The *ask* is [`ASK`] then a
//!   16-byte salt; a *chunk* is [`CHUNK`], its index, the chunk count, then [`PER_CHUNK`]
//!   witnesses of the common tables in digest order. The responder sends what fits; a table
//!   whose chunk did not fit reads as differing. A peer that sends no chunk leaves the regular
//!   order as it was.
//! - **A witness.** Four bytes of a salted hash of operation IDs in the table's independently
//!   witnessed receipts (`super::Round::table_witnesses`). This inspects custody and the journal,
//!   without opening a store or authenticating, classifying or decrypting its envelopes. Equal
//!   receipt ID sets give equal witnesses; a table either side cannot inspect gives [`UNKNOWN`],
//!   which never matches. A full open can still refuse those receipts. Conflict proofs are not
//!   covered: a table whose proofs alone differ
//!   waits for its turn of the regular scan, as before.
//! - **Only an order.** A witness never skips the inventories, never acknowledges anything and
//!   never feeds a report's convergence evidence: a peer's chunks change which table is repaired
//!   first, and nothing else.

use astream_wire::sync::Digest;
use aterm_digest::Sha256;
use std::collections::{BTreeMap, BTreeSet};

/// An initiator's ask: these 16 bytes, then its salt. No table digest has them but by chance, and
/// none is a clock mark's ([`crate::clock::MARK_PREFIX`]) or a chunk's.
pub(super) const ASK: &[u8; 16] = b"aterm/wt/ask1\0\0\0";
/// A responder's chunk: these 10 bytes, its index, the chunk count, then [`PER_CHUNK`] witnesses.
pub(super) const CHUNK: &[u8; 10] = b"aterm/wt1\0";
/// Witnesses one chunk carries: a list of 64 entries holds a full catalog's and its chunks.
pub(super) const PER_CHUNK: usize = 5;
/// A witness's length.
const LEN: usize = 4;

/// What a table's witness is: four bytes of a salted hash.
pub(super) type Witness = [u8; LEN];

/// The witness of a table that side could not read: it never matches.
pub(super) const UNKNOWN: Witness = [0; LEN];

/// What a received list carried besides tables.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Heard {
    /// The salt of each ask it carried.
    pub(super) asks: Vec<[u8; 16]>,
    /// Its chunks by index, each with the chunk count it names.
    pub(super) chunks: BTreeMap<u8, (u8, [Witness; PER_CHUNK])>,
}

/// The ask of `salt`: its one spelling.
pub(super) fn ask(salt: [u8; 16]) -> Digest {
    let mut entry = [0; 32];
    entry[..16].copy_from_slice(ASK);
    entry[16..].copy_from_slice(&salt);
    entry
}

/// `received` without its asks and chunks, and those. No ask or chunk is ever a table: never
/// common, selected, adopted or pushed.
pub(super) fn split(mut received: BTreeSet<Digest>) -> (BTreeSet<Digest>, Heard) {
    let mut heard = Heard::default();
    received.retain(|entry| {
        if entry[..16] == ASK[..] {
            heard
                .asks
                .push(entry[16..].try_into().expect("sixteen bytes"));
            false
        } else if entry[..CHUNK.len()] == CHUNK[..] {
            let at = CHUNK.len();
            let witnesses = std::array::from_fn(|n| {
                entry[at + 2 + n * LEN..at + 2 + (n + 1) * LEN]
                    .try_into()
                    .expect("four bytes")
            });
            heard.chunks.insert(entry[at], (entry[at + 1], witnesses));
            false
        } else {
            true
        }
    });
    (received, heard)
}

/// The witness of `table` under `salt`, where `content` is what its table round reads of it; none
/// reads as [`UNKNOWN`].
pub(super) fn witness(salt: &[u8; 16], table: &Digest, content: Option<Digest>) -> Witness {
    let Some(content) = content else {
        return UNKNOWN;
    };
    let mut hash = Sha256::new();
    hash.update(b"aterm/commons/table-witness/1\0");
    hash.update(salt);
    hash.update(table);
    hash.update(content);
    let mut witness: Witness = hash.finalize()[..LEN].try_into().expect("four bytes");
    if witness == UNKNOWN {
        // A read table never says it could not be read.
        witness[LEN - 1] = 1;
    }
    witness
}

/// How many chunks the witnesses of `tables` common tables take.
pub(super) fn chunk_count(tables: usize) -> usize {
    tables.div_ceil(PER_CHUNK)
}

/// The chunks that carry `witnesses` (the common tables', in digest order), the first `room` of
/// them where all do not fit.
pub(super) fn chunks(witnesses: &[Witness], room: usize) -> BTreeSet<Digest> {
    let total = chunk_count(witnesses.len());
    let Ok(total_byte) = u8::try_from(total) else {
        return BTreeSet::new();
    };
    witnesses
        .chunks(PER_CHUNK)
        .enumerate()
        .take(room)
        .map(|(index, part)| {
            let mut entry = [0; 32];
            entry[..CHUNK.len()].copy_from_slice(CHUNK);
            let at = CHUNK.len();
            entry[at] = u8::try_from(index).expect("bounded by the chunk count");
            entry[at + 1] = total_byte;
            for (n, witness) in part.iter().enumerate() {
                entry[at + 2 + n * LEN..at + 2 + (n + 1) * LEN].copy_from_slice(witness);
            }
            entry
        })
        .collect()
}

/// The tables of `common` (both sides' tables, in digest order) whose witness differs: `ours`
/// against what the peer's chunks state, a table no chunk covers or either side could not read
/// included. None when the peer answered nothing this side can place: no chunk, or chunks that
/// name another chunk count than `common` takes.
pub(super) fn differing(
    common: &BTreeSet<Digest>,
    ours: &[Witness],
    theirs: &Heard,
) -> Option<BTreeSet<Digest>> {
    let total = chunk_count(common.len());
    if theirs.chunks.is_empty()
        || ours.len() != common.len()
        || theirs
            .chunks
            .iter()
            .any(|(index, (count, _))| usize::from(*count) != total || usize::from(*index) >= total)
    {
        return None;
    }
    Some(
        common
            .iter()
            .zip(ours)
            .enumerate()
            .filter(|(at, (_, ours))| {
                let index = u8::try_from(at / PER_CHUNK).expect("bounded by the chunk count");
                theirs.chunks.get(&index).is_none_or(|(_, witnesses)| {
                    let theirs = witnesses[at % PER_CHUNK];
                    theirs == UNKNOWN || **ours == UNKNOWN || theirs != **ours
                })
            })
            .map(|(_, (table, _))| *table)
            .collect(),
    )
}

#[cfg(test)]
#[path = "witness_tests.rs"]
mod tests;
