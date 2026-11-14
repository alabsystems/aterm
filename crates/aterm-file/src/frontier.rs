// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a snapshot was projected from, and the monotone rule over it
//! (docs/DESIGN-shared-files-2026-11-11.md §4, Materializer): a snapshot is replaced only by
//! one whose frontier contains the old one, or whose home is a later generation.
//!
//! The rule orders two reads of one replica. A read of another replica (this Mac left its lab
//! and joined another, or set its vault aside and enrolled again as a new Mac) is neither ahead
//! of the snapshot nor behind it: nothing in it compares, so it replaces the snapshot outright
//! ([`Frontier::source`]).

use aterm_digest::Sha256;
use std::collections::BTreeSet;

const MAGIC: &[u8] = b"aterm-file-frontier/1\n";

/// The replica, the home, the table generation and the records a snapshot was projected from.
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
    /// Every record the table retained there, by operation digest.
    pub ids: BTreeSet<[u8; 32]>,
}

impl Frontier {
    /// Whether a snapshot projected from `self` may replace one projected from `old`: another
    /// replica's read always; of one replica a later home, or a later table generation at the
    /// same home, or the same of both with every record `old` had. An older home never replaces
    /// a later one's snapshot, and a view that lacks a record the snapshot already holds never
    /// replaces it.
    #[must_use]
    pub fn admits_over(&self, old: &Self) -> bool {
        if self.source != old.source {
            return true;
        }
        match (self.era, self.generation).cmp(&(old.era, old.generation)) {
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Less => false,
            std::cmp::Ordering::Equal => self.ids.is_superset(&old.ids),
        }
    }

    /// A digest of the whole frontier: equal fingerprints mean nothing arrived, so a pass
    /// projects nothing again.
    #[must_use]
    pub fn fingerprint(&self) -> [u8; 32] {
        let mut sha = Sha256::new();
        sha.update(b"aterm/file/frontier/1\0");
        sha.update(self.source);
        sha.update(self.era.to_le_bytes());
        sha.update(self.generation.to_le_bytes());
        sha.update((self.ids.len() as u64).to_le_bytes());
        for id in &self.ids {
            sha.update(id);
        }
        sha.finalize()
    }

    /// The frontier file's bytes.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(MAGIC.len() + 44 + self.ids.len() * 32);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&self.source);
        bytes.extend_from_slice(&self.era.to_le_bytes());
        bytes.extend_from_slice(&self.generation.to_le_bytes());
        for id in &self.ids {
            bytes.extend_from_slice(id);
        }
        bytes
    }

    /// Read a frontier file's bytes; `None` when they are not one.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let rest = bytes.strip_prefix(MAGIC)?;
        let (source, rest) = rest.split_first_chunk::<32>()?;
        let (era, rest) = rest.split_first_chunk::<8>()?;
        let (generation, rest) = rest.split_first_chunk::<4>()?;
        let (chunks, tail) = rest.as_chunks::<32>();
        if !tail.is_empty() {
            return None;
        }
        let ids: BTreeSet<[u8; 32]> = chunks.iter().copied().collect();
        if ids.len() != chunks.len() {
            return None;
        }
        Some(Self {
            source: *source,
            era: u64::from_le_bytes(*era),
            generation: u32::from_le_bytes(*generation),
            ids,
        })
    }
}
