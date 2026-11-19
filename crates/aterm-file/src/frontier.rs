// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a snapshot was projected from, and the monotone rule over it
//! (docs/DESIGN-shared-files-2026-11-11.md §4, Materializer;
//! docs/design-commons-at-scale/journal.md §2.11, Sidecar): a snapshot is replaced only by one
//! whose frontier extends the old one, or whose home is a later generation.
//!
//! A frontier names no record. It names each custody store the read was taken from by its
//! digest and that store's custody witness: how many receipts the store had appended and the
//! hash chained over them. Under one custody a higher count is a superset of receipts (the
//! receipt chain is append-only, and a store's open refuses a rollback), so a read whose every
//! store is at least as far along as the snapshot's holds every record the snapshot was made
//! from, with no list of ids. The file's size does not grow with the record count: 256 bytes
//! for a read of one or two stores, and the fixed head and 72 bytes a store past that (a file
//! of the journal ring, read from a week table per week of the window; a table whose family
//! keeps its records across a roll, read with each sealed generation).
//!
//! The rule orders two reads of one replica. A read of another replica (this Mac left its lab
//! and joined another, or set its vault aside and enrolled again as a new Mac) is neither ahead
//! of the snapshot nor behind it: nothing in it compares, so it replaces the snapshot outright
//! ([`Frontier::source`]).

use aterm_digest::Sha256;

const MAGIC: &[u8] = b"aterm-file-frontier/2\n";
/// The id-listing frontier of an earlier build: only its replica is read
/// ([`Frontier::source_of_1`]), and the newest home's next read replaces it.
const MAGIC_1: &[u8] = b"aterm-file-frontier/1\n";
/// The magic, the source, the era, the generation and the count of stores.
const HEAD: usize = 22 + 32 + 8 + 4 + 1;
/// One store: its digest, its witness count and its witness hash.
const ENTRY: usize = 32 + 8 + 32;
/// The view root.
const VIEW: usize = 32;
/// What a frontier file takes at least, zero-padded: one of one or two stores is this, and
/// each store past the second (a ring's weeks, a kept family's sealed generations) adds 72
/// bytes.
pub const BYTES: usize = 256;

/// One custody store a read was taken from, and how far its custody witness had counted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Witnessed {
    /// The store's custody: a digest of its enrollment claim apart from the witness, which
    /// stays while the store appends.
    pub digest: [u8; 32],
    /// How many receipts the store's custody witness counts.
    pub count: u64,
    /// The witness's hash, chained over those receipts.
    pub hash: [u8; 32],
}

impl Witnessed {
    /// Whether this entry is at least as far along as `old` under the same custody: a higher
    /// count, or the same count and the same chain.
    #[must_use]
    pub fn extends(&self, old: &Self) -> bool {
        self.digest == old.digest
            && (self.count > old.count || (self.count == old.count && self.hash == old.hash))
    }
}

/// Where an intent's record was appended: the store and its witness count after the append
/// ([`crate::Done::at`]). A snapshot projected from a frontier whose entry for that store has a
/// count at least as high holds the record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct At {
    pub store: [u8; 32],
    pub count: u64,
}

/// The replica, the home, the table generation and the stores a snapshot was projected from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frontier {
    /// Which replica was read: a digest of the Commons the table belongs to (its trust root and
    /// the table's first scope) and of the actor this Mac's writer lineage began as. Two reads
    /// with one source are reads of one log by one lineage, and only those are ordered.
    pub source: [u8; 32],
    /// Which home of this Mac's writer lineage was opened: 0 the original, 1 its admitted
    /// successor's, and so on.
    pub era: u64,
    /// The generation of the table family that was read (`alab/registry` 1, `registry.2` 2).
    pub generation: u32,
    /// Each store read, in the read's order: a table's one store, or where the read combined a
    /// recovery replica with its primary, the replica's then the primary's; before those, where
    /// the table's family keeps its records across a roll, each sealed generation's stores,
    /// oldest generation first (`aterm_db::file::materialize`); a ring's, each week table's in
    /// the window's order. At most 255.
    pub stores: Vec<Witnessed>,
    /// The root of the state the read folds to, where its table has one (a window-2 generation's
    /// entry state, docs/design-commons-at-scale/window.md); zeros until then. Fingerprinted,
    /// never compared: a projection's key holds the log's judgment of its records beside the
    /// frontier (`aterm_db::file::materialize`).
    pub view: [u8; 32],
}

impl Frontier {
    /// Whether a snapshot projected from `self` may replace one projected from `old`: another
    /// replica's read always; of one replica a later home, or a later table generation at the
    /// same home; and of the same of both only where every store of `old` is matched by one of
    /// `self` at least as far along ([`Witnessed::extends`]). An older home never replaces a
    /// later one's snapshot, and a read with a store behind the snapshot's never replaces it.
    ///
    /// Nor does a read that does not name every store of `old`, on any home: nothing in it says
    /// whether it holds what the store it lacks held. That is a recovery replica's read standing
    /// alone where the primary refused (it lacks the primary's records), and also a read of a
    /// custody enrolled again, or of a primary whose recovery replica the snapshot's read was
    /// combined with has gone, which hold the snapshot as it is until its sidecar is removed
    /// (docs/KNOWN_ISSUES.md).
    #[must_use]
    pub fn admits_over(&self, old: &Self) -> bool {
        if self.source != old.source {
            return true;
        }
        match (self.era, self.generation).cmp(&(old.era, old.generation)) {
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Less => false,
            std::cmp::Ordering::Equal => old
                .stores
                .iter()
                .all(|old| self.stores.iter().any(|new| new.extends(old))),
        }
    }

    /// Whether a snapshot projected from `self` holds the record appended at `at`: the entry
    /// for its store has counted at least that far.
    #[must_use]
    pub fn holds(&self, at: &At) -> bool {
        self.stores
            .iter()
            .any(|store| store.digest == at.store && store.count >= at.count)
    }

    /// How many receipts the stores' witnesses count together (each record and conflict proof
    /// appended to each store; a combined read counts both claims'): how far the read had come,
    /// not how many records its log retains.
    #[must_use]
    pub fn records(&self) -> u64 {
        self.stores
            .iter()
            .fold(0u64, |sum, store| sum.saturating_add(store.count))
    }

    /// A digest of the whole frontier: equal fingerprints mean nothing arrived. The
    /// materializer's key holds it among the rest of what a projection reads.
    #[must_use]
    pub fn fingerprint(&self) -> [u8; 32] {
        let mut sha = Sha256::new();
        sha.update(b"aterm/file/frontier/2\0");
        sha.update(self.encode());
        sha.finalize()
    }

    /// The frontier file's bytes: the head, each store, the view root, and zeros to
    /// [`BYTES`] where that is longer.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let count = u8::try_from(self.stores.len()).unwrap_or(u8::MAX);
        let stores = &self.stores[..usize::from(count)];
        let len = HEAD + stores.len() * ENTRY + VIEW;
        let mut bytes = Vec::with_capacity(len.max(BYTES));
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&self.source);
        bytes.extend_from_slice(&self.era.to_le_bytes());
        bytes.extend_from_slice(&self.generation.to_le_bytes());
        bytes.push(count);
        for store in stores {
            bytes.extend_from_slice(&store.digest);
            bytes.extend_from_slice(&store.count.to_le_bytes());
            bytes.extend_from_slice(&store.hash);
        }
        bytes.extend_from_slice(&self.view);
        bytes.resize(len.max(BYTES), 0);
        bytes
    }

    /// The replica an `aterm-file-frontier/1` file, an earlier build's, names: what a file of
    /// the journal ring keeps rows by ([`Frontier::source`]). Its id list orders nothing now, so
    /// [`Self::decode`] reads it as no frontier: the newest home's next read replaces it, never a
    /// fenced home's, as where a sidecar does not read.
    #[must_use]
    pub fn source_of_1(bytes: &[u8]) -> Option<[u8; 32]> {
        let rest = bytes.strip_prefix(MAGIC_1)?;
        let (source, rest) = rest.split_first_chunk::<32>()?;
        (rest.len() >= 12 && (rest.len() - 12) % 32 == 0).then_some(*source)
    }

    /// Read a frontier file's bytes; `None` when they are not an `aterm-file-frontier/2` one.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let rest = bytes.strip_prefix(MAGIC)?;
        let (source, rest) = rest.split_first_chunk::<32>()?;
        let (era, rest) = rest.split_first_chunk::<8>()?;
        let (generation, rest) = rest.split_first_chunk::<4>()?;
        let (&[count], mut rest) = rest.split_first_chunk::<1>()?;
        let len = HEAD + usize::from(count) * ENTRY + VIEW;
        if bytes.len() != len.max(BYTES) {
            return None;
        }
        let mut stores = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            let (digest, after) = rest.split_first_chunk::<32>()?;
            let (witnessed, after) = after.split_first_chunk::<8>()?;
            let (hash, after) = after.split_first_chunk::<32>()?;
            stores.push(Witnessed {
                digest: *digest,
                count: u64::from_le_bytes(*witnessed),
                hash: *hash,
            });
            rest = after;
        }
        let (view, padding) = rest.split_first_chunk::<32>()?;
        if padding.iter().any(|byte| *byte != 0) {
            return None;
        }
        Some(Self {
            source: *source,
            era: u64::from_le_bytes(*era),
            generation: u32::from_le_bytes(*generation),
            stores,
            view: *view,
        })
    }
}
