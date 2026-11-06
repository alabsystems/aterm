// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the mesh asks of the stores that hold the lab's tables and the membership log
//! (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.6, "Tables arrive through one consumer").
//!
//! `tables.log` carries records between Macs; it is not where they are read. Each Commons table
//! keeps its operations in its own journal, and the membership log in its records file. The
//! egress ([`crate::egress`]), the ingest consumer ([`crate::ingest`]) and the `Inventory` exchange
//! ([`crate::inventory`]) reach them only through [`Stores`]: the node service hands the vault's
//! ([`vault::VaultStores`]), and a test a stand-in.
//!
//! **What travels.** A table operation goes on `/t/<table digest, 64 lowercase hex>/n-<origin>`
//! with a one-byte kind ahead of its wire ([`Kind`]); a membership record on
//! `/t/membership/n-<origin>`, its signed bytes as they are. The stores judge each one: the
//! writer an operation or a record names is inside its own signed bytes.

use std::collections::BTreeSet;
use std::io;

pub mod vault;

/// A table's digest (`aterm_db::table::Scope::digest`), and an operation's or a record's.
pub type Digest = [u8; 32];

/// A conflict proof's key (`aterm_db::table::conflict::ProofKey`): its actor and slot in the
/// first 48 bytes, then whether it proves a fork (byte 48), then the proof's own digest.
pub type ProofKey = [u8; 81];

/// The subject segment of the membership log on `tables.log`.
pub const MEMBERSHIP: &str = "membership";

/// The kind of a table record, the first byte of its payload on `tables.log`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// An operation.
    Operation,
    /// A conflict certificate.
    Conflict,
}

impl Kind {
    /// Its byte.
    #[must_use]
    pub const fn byte(self) -> u8 {
        match self {
            Self::Operation => 0,
            Self::Conflict => 1,
        }
    }

    /// The kind `byte` names.
    #[must_use]
    pub const fn of(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Operation),
            1 => Some(Self::Conflict),
            _ => None,
        }
    }
}

/// What a store did with one record the mesh delivered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// It entered the store, or the store held it already.
    Applied,
    /// Its writer, its roster or a parent is not in this Mac's view yet: it waits, and is tried
    /// again after the next membership record this Mac applies.
    Waiting(String),
    /// Refused for good: a bad signature, a policy refusal, a table this Mac does not hold.
    Refused(String),
    /// Local contention (another writer held the store, a snapshot moved): nothing changed, and
    /// the record is tried again.
    Busy(String),
}

/// Which store of a table: its primary store, or the recovery store an older peer's records go
/// to after a catalog damage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TableKey {
    /// The table's digest.
    pub table: Digest,
    /// Its recovery store.
    pub recovery: bool,
}

/// One record a table store holds, by its offset in the store's own journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    /// Its offset.
    pub offset: u64,
    /// Its kind.
    pub kind: Kind,
    /// Its wire.
    pub wire: Vec<u8>,
}

/// The stores the mesh moves records between. Each call is one short local batch: nothing here
/// waits on the network.
pub trait Stores: Send + Sync {
    /// Ingest `wires` of `table`, each of `kind`, that the mesh delivered (at most
    /// [`crate::ingest::BATCH`]): one verdict per wire, in order. A record that enters this way
    /// is never published again by this Mac.
    fn ingest_table(&self, table: Digest, kind: Kind, wires: &[Vec<u8>]) -> Vec<Verdict>;

    /// Ingest membership records the mesh delivered, each its signed bytes: one verdict per
    /// record, in order. The ids that enter this way are never published again by this Mac.
    fn ingest_members(&self, records: &[Vec<u8>]) -> Vec<Verdict>;

    /// Every table store this Mac holds, with its length (its journal's record count).
    /// # Errors
    /// Where the stores cannot be read.
    fn table_lengths(&self) -> io::Result<Vec<(TableKey, u64)>>;

    /// The records `key`'s store holds at offsets `from..upto`, never past what it acknowledged.
    /// # Errors
    /// Where the store cannot be read.
    fn table_held(&self, key: TableKey, from: u64, upto: u64) -> io::Result<Vec<Held>>;

    /// The membership log's complete records from index `from` of its held order, and the held
    /// count: records past an index are exactly those that became complete since.
    /// # Errors
    /// Where the records file cannot be read.
    fn members_held(&self, from: usize) -> io::Result<(Vec<Vec<u8>>, usize)>;

    /// The tables this Mac holds for `Inventory`: their digests.
    /// # Errors
    /// Where the stores cannot be read.
    fn tables(&self) -> io::Result<BTreeSet<Digest>>;

    /// The digests of the operations `table`'s store holds, `None` where this Mac holds none.
    /// # Errors
    /// Where the store cannot be read.
    fn table_inventory(&self, table: Digest) -> io::Result<Option<BTreeSet<Digest>>>;

    /// The wires of the operations `ids` of `table` that its store holds.
    /// # Errors
    /// Where the store cannot be read.
    fn table_wires(&self, table: Digest, ids: &[Digest]) -> io::Result<Vec<Vec<u8>>>;

    /// The keys of `table`'s conflict proofs (Commons repair's `ProofKey`), `None` where this Mac
    /// does not hold the table.
    /// # Errors
    /// Where the store cannot be read.
    fn table_proofs(&self, table: Digest) -> io::Result<Option<BTreeSet<ProofKey>>>;

    /// The wires of the conflict proofs `keys` of `table` that its store holds.
    /// # Errors
    /// Where the store cannot be read.
    fn table_proof_wires(&self, table: Digest, keys: &[ProofKey]) -> io::Result<Vec<Vec<u8>>>;

    /// The ids of the membership log's complete records.
    /// # Errors
    /// Where the records file cannot be read.
    fn members_inventory(&self) -> io::Result<BTreeSet<Digest>>;

    /// The signed bytes of the membership records `ids` this Mac holds complete.
    /// # Errors
    /// Where the records file cannot be read.
    fn members_wires(&self, ids: &[Digest]) -> io::Result<Vec<Vec<u8>>>;

    /// Install this process's store sinks so each durable append feeds the egress open now
    /// ([`crate::egress::sinks`]): the vault's stores do; a test's stand-in feeds its egress
    /// itself.
    fn hook(&self) {}
}

/// The subject of `table`'s records `principal` puts on the mesh.
#[must_use]
pub fn table_subject(table: &Digest, principal: &str) -> String {
    format!("/t/{}/{principal}", aterm_codec::hex::encode_lower(table))
}

/// The subject of the membership records `principal` puts on the mesh.
#[must_use]
pub fn members_subject(principal: &str) -> String {
    format!("/t/{MEMBERSHIP}/{principal}")
}

/// What a subject on `tables.log` carries, with its owner segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Carries {
    /// A table's records.
    Table(Digest),
    /// The membership log's.
    Members,
}

/// Read a subject of `tables.log`: what it carries, and its owner (`n-<origin>`); `None` for one
/// that names neither.
#[must_use]
pub fn carries(subject: &str) -> Option<(Carries, &str)> {
    let (segment, owner) = subject.strip_prefix("/t/")?.split_once('/')?;
    if owner.is_empty() || owner.contains('/') {
        return None;
    }
    if segment == MEMBERSHIP {
        return Some((Carries::Members, owner));
    }
    let mut table = [0u8; 32];
    (aterm_codec::hex::is_lower(segment, 64)
        && aterm_codec::hex::decode_into(segment.as_bytes(), &mut table))
    .then_some((Carries::Table(table), owner))
}

/// A table record's payload: its kind's byte, then its wire.
#[must_use]
pub fn table_payload(kind: Kind, wire: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(wire.len() + 1);
    payload.push(kind.byte());
    payload.extend_from_slice(wire);
    payload
}

/// The kind and wire a table record's payload holds.
#[must_use]
pub fn read_table_payload(payload: &[u8]) -> Option<(Kind, &[u8])> {
    let (&byte, wire) = payload.split_first()?;
    Some((Kind::of(byte)?, wire))
}

/// The SHA-256 of `bytes`: a membership record's id is the digest of its signed bytes.
#[must_use]
pub fn digest(bytes: &[u8]) -> Digest {
    aterm_digest::Sha256::digest(bytes)
}
