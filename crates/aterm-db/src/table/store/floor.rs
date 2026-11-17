// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A table's floor (docs/DESIGN-table-checkpoint-2026-11-15.md §5, tracker LT-B10C1): the newest
//! final checkpoint this Mac signed, which the checkpoint pass records and the store counts from.
//!
//! The record of it is a private file beside the custody claim, `<claim>.floor`: [`MAGIC`], the
//! claim's identity (its bytes before the receipt witness, so the floor stands as the witness
//! moves) and the checkpoint's body in its one encoding. A floor of another custody is refused.
//! The store reads it before replay, since replay checks the quota as it admits each record, and
//! confirms after replay that this Mac signed a checkpoint with exactly that body
//! ([`super::Store::open`]); the file names a choice among this Mac's own signatures, never
//! anything a peer said.
//!
//! What it changes is counting alone. A record at or below the floor's frontier (its writer's
//! sequence there) is cold: it stays held, indexed and served, and the table's and each writer's
//! operation counts and bytes measure the records above it. The frontier bounds how many cold
//! records there can be, so a record that arrives later below it is admitted as cold too.

use super::{CLAIM_LEN, WITNESS_AT, WriterHeld};
use crate::{Error, Result, checkpoint::Body};
use astream_wire::sync::{Actor, Header};
use aterm_durable::Flush;
use aterm_uds::private_file::{self, PrivateFileError};
use std::collections::BTreeMap;
use std::path::Path;

const MAGIC: &[u8; 5] = b"ATFL1";
const DOMAIN: &[u8] = b"aterm/commons/table-floor/1\0";
/// The most bytes a floor file holds: its head and a body of 700 writers' tips.
const MAX_LEN: usize = 64 * 1024;

fn name(claim_name: &str) -> String {
    format!("{claim_name}.floor")
}

fn binding(claim: &[u8]) -> Result<[u8; 32]> {
    if claim.len() != CLAIM_LEN || claim[4] != b'1' {
        return Err(Error::new("a table floor requires ready table custody"));
    }
    let mut hash = aterm_digest::Sha256::new();
    hash.update(DOMAIN);
    hash.update(&claim[..WITNESS_AT]);
    Ok(hash.finalize())
}

/// The floor recorded for the custody `claim`, if any.
///
/// # Errors
/// The file is unreadable, of another custody, or not a checkpoint body in its one encoding.
pub(super) fn read(dir: &Path, claim_name: &str, claim: &[u8]) -> Result<Option<Body>> {
    let bytes = match private_file::read_private(dir, &name(claim_name), MAX_LEN) {
        Ok(bytes) => bytes,
        Err(PrivateFileError::Missing) => return Ok(None),
        Err(_) => return Err(Error::new("table floor is unreadable")),
    };
    let rest = bytes
        .strip_prefix(MAGIC.as_slice())
        .and_then(|rest| rest.split_first_chunk::<32>())
        .filter(|(bound, _)| binding(claim).is_ok_and(|own| **bound == own));
    let Some((_, body)) = rest else {
        return Err(Error::new("table floor has another custody identity"));
    };
    crate::checkpoint::decode(body)
        .map(Some)
        .ok_or_else(|| Error::new("table floor is not a checkpoint body"))
}

/// Record `body` as the floor of the custody `claim`, durably.
///
/// # Errors
/// The claim is not ready custody, or the file cannot be written.
pub(super) fn write(dir: &Path, claim_name: &str, claim: &[u8], body: &Body) -> Result<()> {
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&binding(claim)?);
    bytes.extend_from_slice(&crate::checkpoint::encode(body));
    if bytes.len() > MAX_LEN {
        return Err(Error::new("table floor exceeds its bound"));
    }
    private_file::write_private_flushed(dir, &name(claim_name), &bytes, Flush::Durable)?;
    Ok(())
}

/// A store's floor: the body it stands on and what it holds at or below it.
pub(super) struct Floor {
    pub(super) body: Body,
    /// The cold records together.
    pub(super) cold: WriterHeld,
    /// The cold records of each writer.
    pub(super) writers: BTreeMap<Actor, WriterHeld>,
}

impl Floor {
    pub(super) fn new(body: Body) -> Self {
        Self {
            body,
            cold: WriterHeld::default(),
            writers: BTreeMap::new(),
        }
    }

    /// Whether `header`'s record is at or below the floor.
    pub(super) fn covers(&self, header: &Header) -> bool {
        self.body
            .frontier
            .get(&header.actor)
            .is_some_and(|tip| header.sequence <= tip.sequence)
    }

    /// Count a record of `bytes` now held, if it is cold.
    pub(super) fn count(&mut self, header: &Header, bytes: usize) {
        if self.covers(header) {
            for held in [
                &mut self.cold,
                self.writers.entry(header.actor).or_default(),
            ] {
                held.records += 1;
                held.bytes = held.bytes.saturating_add(bytes);
            }
        }
    }

    /// What `writer` holds cold.
    pub(super) fn of(&self, writer: &Actor) -> WriterHeld {
        self.writers.get(writer).copied().unwrap_or_default()
    }
}
