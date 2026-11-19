// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The signed table checkpoint's body (docs/DESIGN-table-checkpoint-2026-11-15.md §3, LT-B10B):
//! a pure function of a cut, so two writers at the same cut sign the same bytes and a share is
//! a signature, not a vote.
//!
//! A cut is a causally closed set of records, named by its frontier (each writer's highest
//! sequence in it and that record's digest). The body carries:
//!
//! - the frontier;
//! - `state`: SHA-256 of the table's state at the cut, as its application reads it: for a typed
//!   table its cells, owned rows and past-quota records (`crate::att::checkpoint_state`), each
//!   cell candidate named by its writer and sequence ([`cells_digest`]), so a record above the
//!   cut is judged against it by version vector;
//! - `below`: how many records the cut holds, and SHA-256 of their sorted ids;
//! - `boundary`: SHA-256 of the floor records a replica without the cut's history starts from:
//!   each frontier tip with its past as a version vector. Every replica that holds the cut derives
//!   them (`boundary`), so the body signs their digest, not the records: it stays one frontier
//!   entry per writer however wide the pasts grow, and a catch-up (LT-B10D) sends the records and
//!   checks them against it.
//!
//! [`propose`] picks a cut every current writer has seen, of records at least a given age, and
//! [`cut_at`] computes the body at a given frontier: a Mac signs a body only after computing it
//! itself, and [`verify`] refuses one it computes differently. Signing it into a table, the
//! version fence and the read that starts from it are LT-B10B's store half and LT-B10C.

use crate::{Error, Result, database::Cell, documents::Cells};
use astream_wire::sync::{Actor, Digest, FloorRecord, Index, Projection};
#[cfg(unix)]
use aterm_secrets::commons::finality;
use std::collections::{BTreeMap, BTreeSet};

/// The payload's tag: no application payload (JSON commands, catalog descriptors, `ATCUT1`
/// cutoffs) starts so.
const MAGIC: &[u8; 7] = b"ATCKP1\0";

/// Domain separation for the state digest.
const STATE_DOMAIN: &[u8] = b"aterm/commons/table-checkpoint-state/1\0";
/// Domain separation for the digest of the ids below the cut.
const BELOW_DOMAIN: &[u8] = b"aterm/commons/table-checkpoint-below/1\0";
/// Domain separation for the digest of the cut's floor records.
const BOUNDARY_DOMAIN: &[u8] = b"aterm/commons/table-checkpoint-boundary/1\0";

/// One writer's place in a cut: its highest sequence there and that record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Tip {
    /// The sequence.
    pub(crate) sequence: u64,
    /// The record at that sequence.
    pub(crate) digest: Digest,
}

/// A checkpoint's body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Body {
    /// The table scope's digest.
    pub(crate) scope: Digest,
    /// The table's generation.
    pub(crate) generation: u64,
    /// The cut, by writer.
    pub(crate) frontier: BTreeMap<Actor, Tip>,
    /// SHA-256 of the cells at the cut, canonically encoded.
    pub(crate) state: Digest,
    /// How many records the cut holds.
    pub(crate) below: u64,
    /// SHA-256 of the cut's sorted record ids.
    pub(crate) below_digest: Digest,
    /// SHA-256 of the cut's floor records (`boundary`): each frontier tip with its past.
    pub(crate) boundary: Digest,
}

/// What the cut is taken from: one table's held history.
pub(crate) struct History<'a> {
    /// The table scope's digest.
    pub(crate) scope: Digest,
    /// The table's generation.
    pub(crate) generation: u64,
    /// Every held header.
    pub(crate) index: &'a Index,
    /// The reader's projection of `index`: its order, quarantines and version vectors. A record
    /// this replica withholds is not in its order, so a cut that holds one cannot be computed
    /// here.
    pub(crate) projection: &'a Projection,
    /// A record's signed authoring time.
    pub(crate) authored: &'a dyn Fn(&Digest) -> Option<u64>,
    /// SHA-256 of the table's state made by the records of a cut, as its application reads it.
    pub(crate) state: &'a dyn Fn(&BTreeSet<Digest>) -> Result<Digest>,
}

/// The body of a checkpoint at a cut every one of `writers` has seen and whose records were all
/// authored at or before `before_ms`: the entrywise minimum of the writers' newest records'
/// pasts, then its largest causally closed part of records that old. `None` when that cut is
/// empty, a writer has no record, or a writer is quarantined (a fork leaves no single past).
pub(crate) fn propose(
    history: &History<'_>,
    writers: &BTreeSet<Actor>,
    before_ms: u64,
) -> Option<Body> {
    let projection = history.projection;
    if writers.is_empty() || writers.iter().any(|w| projection.quarantined.contains(w)) {
        return None;
    }
    let mut seen: Option<BTreeMap<Actor, u64>> = None;
    for writer in writers {
        let newest = projection
            .ordered
            .iter()
            .filter_map(|id| history.index.get(id))
            .filter(|header| header.actor == *writer)
            .max_by_key(|header| header.sequence)?;
        let past = past_of(history.index, &newest.id);
        seen = Some(match seen {
            None => past,
            Some(old) => old
                .into_iter()
                .filter_map(|(actor, sequence)| {
                    past.get(&actor).map(|other| (actor, sequence.min(*other)))
                })
                .collect(),
        });
    }
    let seen = seen?;
    let mut cut = BTreeSet::new();
    for id in &projection.ordered {
        let Some(header) = history.index.get(id) else {
            continue;
        };
        let old_enough = (history.authored)(id).is_some_and(|at| at <= before_ms);
        let within = seen
            .get(&header.actor)
            .is_some_and(|highest| header.sequence <= *highest);
        let closed = header
            .parents
            .iter()
            .chain(header.previous.iter())
            .all(|parent| cut.contains(parent));
        if old_enough && within && closed {
            cut.insert(*id);
        }
    }
    let mut frontier = BTreeMap::new();
    for id in &cut {
        let header = history.index.get(id)?;
        let tip = frontier.entry(header.actor).or_insert(Tip {
            sequence: 0,
            digest: [0; 32],
        });
        if header.sequence > tip.sequence {
            *tip = Tip {
                sequence: header.sequence,
                digest: header.id,
            };
        }
    }
    if frontier.is_empty() {
        return None;
    }
    cut_at(history, &frontier).ok()
}

/// The body of the checkpoint at `frontier`.
///
/// # Errors
/// This history does not hold the whole cut (a record below a tip is missing or pending), a tip
/// names another record than the one held at its sequence, or the cut is not causally closed.
pub(crate) fn cut_at(history: &History<'_>, frontier: &BTreeMap<Actor, Tip>) -> Result<Body> {
    let (cut, _) = cut_records(history, frontier)?;
    let state = (history.state)(&cut)?;
    let mut below = aterm_digest::Sha256::new();
    below.update(BELOW_DOMAIN);
    for id in &cut {
        below.update(id);
    }
    Ok(Body {
        scope: history.scope,
        generation: history.generation,
        frontier: frontier.clone(),
        state,
        below: cut.len() as u64,
        below_digest: below.finalize(),
        boundary: boundary_digest(&boundary(history.index, frontier)),
    })
}

/// A body this replica computed itself: the only one [`crate::table::store::Store::sign_checkpoint`]
/// signs, so a Mac never signs a body it was handed.
#[cfg(unix)] // computed and seconded for the unix-only signing pass (`att::checkpoint`)
#[derive(Clone, Debug)]
pub(crate) struct Computed(Body);

#[cfg(unix)]
impl Computed {
    /// The body.
    pub(crate) const fn body(&self) -> &Body {
        &self.0
    }
}

/// The checkpoint this replica would sign: [`propose`]'s body, recomputed at its frontier.
#[cfg(unix)]
pub(crate) fn compute(
    history: &History<'_>,
    writers: &BTreeSet<Actor>,
    before_ms: u64,
) -> Option<Computed> {
    propose(history, writers, before_ms).map(Computed)
}

/// Another writer's share, recomputed here: this replica signs the same body only when it
/// computes the same bytes ([`verify`]).
///
/// # Errors
/// As [`verify`].
#[cfg(unix)]
pub(crate) fn second(history: &History<'_>, body: &Body) -> Result<Computed> {
    verify(history, body)?;
    Ok(Computed(body.clone()))
}

/// The records of the cut at `frontier`, as a set and in the reader's order.
///
/// # Errors
/// As [`cut_at`].
fn cut_records(
    history: &History<'_>,
    frontier: &BTreeMap<Actor, Tip>,
) -> Result<(BTreeSet<Digest>, Vec<Digest>)> {
    let projection = history.projection;
    let mut cut = BTreeSet::new();
    for (actor, tip) in frontier {
        let header = history
            .index
            .get(&tip.digest)
            .filter(|header| header.actor == *actor && header.sequence == tip.sequence)
            .ok_or_else(|| Error::new("a checkpoint's tip is not held here"))?;
        cut.insert(header.id);
        cut.extend(history.index.ancestors(&header.id));
    }
    let ordered: Vec<Digest> = projection
        .ordered
        .iter()
        .filter(|id| cut.contains(*id))
        .copied()
        .collect();
    if ordered.len() != cut.len() {
        return Err(Error::new(
            "this replica does not hold every record of the checkpoint's cut in causal order",
        ));
    }
    for id in &cut {
        let header = history
            .index
            .get(id)
            .ok_or_else(|| Error::new("a record of the cut is not held here"))?;
        if frontier
            .get(&header.actor)
            .is_none_or(|tip| header.sequence > tip.sequence)
        {
            return Err(Error::new(
                "the checkpoint's cut names a record beyond its own frontier",
            ));
        }
    }
    Ok((cut, ordered))
}

/// SHA-256 of `floor` in its one encoding, in id order.
pub(crate) fn boundary_digest(floor: &[FloorRecord]) -> Digest {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(BOUNDARY_DOMAIN);
    hash.update((floor.len() as u64).to_le_bytes());
    for record in floor {
        hash.update(encode_floor(record));
    }
    hash.finalize()
}

/// One floor record's bytes: its header, then its past, every length explicit.
fn encode_floor(record: &FloorRecord) -> Vec<u8> {
    let header = &record.header;
    let mut out = Vec::new();
    out.extend_from_slice(&header.id);
    out.extend_from_slice(&header.actor);
    out.extend_from_slice(&header.sequence.to_le_bytes());
    match header.previous {
        Some(previous) => {
            out.push(1);
            out.extend_from_slice(&previous);
        }
        None => out.push(0),
    }
    out.extend_from_slice(&(header.parents.len() as u32).to_le_bytes());
    for parent in &header.parents {
        out.extend_from_slice(parent);
    }
    out.extend_from_slice(&(record.past.len() as u32).to_le_bytes());
    for (actor, sequence) in &record.past {
        out.extend_from_slice(actor);
        out.extend_from_slice(&sequence.to_le_bytes());
    }
    out
}

/// Refuse a body this replica computes differently at the same frontier.
///
/// # Errors
/// [`cut_at`]'s, or the recomputed body differs from `body` (a forged or mistaken state).
pub(crate) fn verify(history: &History<'_>, body: &Body) -> Result<()> {
    if body.scope != history.scope || body.generation != history.generation {
        return Err(Error::new("a checkpoint of another table or generation"));
    }
    let computed = cut_at(history, &body.frontier)?;
    if computed != *body {
        return Err(Error::new(
            "a checkpoint whose state this replica computes differently is refused",
        ));
    }
    Ok(())
}

/// The bodies among `shares` (each held checkpoint record's signer and body) of the table
/// `scope` at `generation` that are final, each with the seats that signed it, in the order
/// `(below, encoding)`: the last is the table's settled floor (design §5, "Every replica names
/// the same floor", tracker LT-B10D1).
///
/// A body is final by the rule the vault log shares ([`finality`]): `seats` are the current
/// members; the cut's time is its newest frontier tip's authored time; and a seat's last signed
/// time is its own frontier tip's authored time, its newest record in the cut, so a seat absent
/// from the frontier wrote nothing at or below the cut and is not recent at it. `tip_ms` answers
/// a tip's authored time where this replica holds the record the tip names, with that writer and
/// sequence. Every input is one every replica holding the cut's tips holds alike, observers
/// included: the checkpoint records, the membership and the tips. A tip's authored time is in
/// its record's signed envelope, which a replica authenticates without the payload's key, but
/// not in its [`astream_wire::sync::Header`]; a replica that does not hold a tip's record (a
/// fresh one handed only the boundary's headers) finds that body final nowhere: it names an older
/// floor or none, and judges more records as hot, never fewer.
#[cfg(unix)] // its readers, the signing pass (`att::checkpoint`) and the documents log, are unix's
pub(crate) fn finals<'a>(
    shares: impl IntoIterator<Item = (Actor, &'a Body)>,
    scope: Digest,
    generation: u64,
    seats: &BTreeSet<Actor>,
    tip_ms: impl Fn(&Actor, &Tip) -> Option<u64>,
) -> Vec<(Body, BTreeSet<Actor>)> {
    // Each body, by its one encoding, with the members that signed it.
    let mut signers: BTreeMap<Vec<u8>, (&Body, BTreeSet<Actor>)> = BTreeMap::new();
    for (signer, body) in shares {
        if body.scope == scope && body.generation == generation {
            signers
                .entry(encode(body))
                .or_insert_with(|| (body, BTreeSet::new()))
                .1
                .insert(signer);
        }
    }
    let mut finals: Vec<(u64, Vec<u8>, Body, BTreeSet<Actor>)> = signers
        .into_iter()
        .filter_map(|(encoding, (body, signed))| {
            let cut_ms = body
                .frontier
                .iter()
                .map(|(actor, tip)| tip_ms(actor, tip))
                .try_fold(0, |newest, at| Some(newest.max(at?)))?;
            let recent = finality::recent(seats, cut_ms, |seat| {
                body.frontier.get(seat).and_then(|tip| tip_ms(seat, tip))
            });
            finality::is_final(&recent, &signed)
                .then(|| (body.below, encoding, body.clone(), signed))
        })
        .collect();
    finals.sort_unstable_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    finals
        .into_iter()
        .map(|(_, _, body, signed)| (body, signed))
        .collect()
}

/// Each actor's highest sequence in `id`'s past, `id` included.
fn past_of(index: &Index, id: &Digest) -> BTreeMap<Actor, u64> {
    let mut past = BTreeMap::new();
    for record in index.ancestors(id).iter().chain([id]) {
        if let Some(header) = index.get(record) {
            let at = past.entry(header.actor).or_insert(0);
            *at = (*at).max(header.sequence);
        }
    }
    past
}

/// The cut's floor records: each frontier tip with its past. A pure function of the cut, so two
/// writers at one cut derive the same records whatever each holds above it; every other record
/// of the cut is named by its writer's next one, so the tips are the cut's maximal records. A
/// record above the cut that names a deeper one gets that header from the headers below the cut,
/// which stay (LT-B10D).
fn boundary(index: &Index, frontier: &BTreeMap<Actor, Tip>) -> Vec<FloorRecord> {
    frontier
        .values()
        .filter_map(|tip| {
            Some(FloorRecord {
                header: index.get(&tip.digest)?.clone(),
                past: past_of(index, &tip.digest),
            })
        })
        .collect()
}

/// SHA-256 of `cells` in a canonical encoding: each cell in key order, its stop, and each
/// candidate in operation order with its writer, sequence and canonical JSON value.
///
/// # Errors
/// A candidate's operation has no header in `dot`, or a value does not encode.
pub(crate) fn cells_digest(
    cells: &Cells,
    dot: impl Fn(&Digest) -> Option<(Actor, u64)>,
) -> Result<Digest> {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(STATE_DOMAIN);
    hash.update((cells.len() as u64).to_le_bytes());
    for (
        (collection, key),
        Cell {
            stopped,
            candidates,
        },
    ) in cells
    {
        for text in [collection, key] {
            hash.update((text.len() as u64).to_le_bytes());
            hash.update(text.as_bytes());
        }
        hash.update([u8::from(*stopped)]);
        hash.update((candidates.len() as u64).to_le_bytes());
        for candidate in candidates {
            let (actor, sequence) = dot(&candidate.operation)
                .ok_or_else(|| Error::new("a candidate of the cut has no held header"))?;
            let value = aterm_json::to_vec(&candidate.value)
                .map_err(|_| Error::new("a candidate value does not encode"))?;
            hash.update(candidate.operation);
            hash.update(actor);
            hash.update(sequence.to_le_bytes());
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value);
        }
    }
    Ok(hash.finalize())
}

/// The checkpoint's payload: [`MAGIC`] then the body, every length explicit and every set in
/// its sorted order, so a body has one encoding.
pub(crate) fn encode(body: &Body) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&body.scope);
    out.extend_from_slice(&body.generation.to_le_bytes());
    out.extend_from_slice(&(body.frontier.len() as u32).to_le_bytes());
    for (actor, tip) in &body.frontier {
        out.extend_from_slice(actor);
        out.extend_from_slice(&tip.sequence.to_le_bytes());
        out.extend_from_slice(&tip.digest);
    }
    out.extend_from_slice(&body.state);
    out.extend_from_slice(&body.below.to_le_bytes());
    out.extend_from_slice(&body.below_digest);
    out.extend_from_slice(&body.boundary);
    out
}

/// The body `payload` carries, if it is a checkpoint's in its one encoding.
pub(crate) fn decode(payload: &[u8]) -> Option<Body> {
    let mut at = payload.strip_prefix(MAGIC.as_slice())?;
    let scope = take::<32>(&mut at)?;
    let generation = u64::from_le_bytes(take::<8>(&mut at)?);
    let mut frontier = BTreeMap::new();
    for _ in 0..u32::from_le_bytes(take::<4>(&mut at)?) {
        let actor = take::<48>(&mut at)?;
        let sequence = u64::from_le_bytes(take::<8>(&mut at)?);
        let digest = take::<32>(&mut at)?;
        if frontier.insert(actor, Tip { sequence, digest }).is_some() {
            return None;
        }
    }
    let state = take::<32>(&mut at)?;
    let below = u64::from_le_bytes(take::<8>(&mut at)?);
    let below_digest = take::<32>(&mut at)?;
    let boundary = take::<32>(&mut at)?;
    let body = Body {
        scope,
        generation,
        frontier,
        state,
        below,
        below_digest,
        boundary,
    };
    // One encoding only: trailing bytes, unsorted sets or duplicates do not round-trip.
    (at.is_empty() && encode(&body) == payload).then_some(body)
}

fn take<const N: usize>(at: &mut &[u8]) -> Option<[u8; N]> {
    let (head, rest) = at.split_first_chunk::<N>()?;
    *at = rest;
    Some(*head)
}

#[cfg(test)]
#[path = "checkpoint_tests.rs"]
mod tests;
