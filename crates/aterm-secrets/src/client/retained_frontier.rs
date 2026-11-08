// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::membership_log::{Log, RecordId};
use crate::{Error, Result};
use std::collections::BTreeSet;

const MAGIC: &[u8; 4] = b"VFD1";
pub(super) const MAX_IDS: usize = crate::membership_log::MAX_HELD_RECORDS;
pub(super) const MAX_DELTA: usize = 44 + 64 * MAX_IDS;

/// The compact acknowledged frontier and the IDs already retired from it.
/// An ID is added once and retired once, bounding the complete receipt journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Frontier {
    network: RecordId,
    heads: BTreeSet<RecordId>,
    retired: BTreeSet<RecordId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Delta {
    before: [u8; 32],
    added: Vec<RecordId>,
    retired: Vec<RecordId>,
}

impl Frontier {
    pub(super) fn empty(network: RecordId) -> Self {
        Self {
            network,
            heads: BTreeSet::new(),
            retired: BTreeSet::new(),
        }
    }

    pub(super) fn heads(&self) -> Vec<RecordId> {
        self.heads.iter().copied().collect()
    }

    pub(super) fn network(&self) -> &RecordId {
        &self.network
    }

    pub(super) fn names_heads(&self, heads: &[RecordId]) -> bool {
        self.heads.len() == heads.len() && self.heads.iter().eq(heads.iter())
    }

    pub(super) fn is_empty(&self) -> bool {
        self.heads.is_empty()
    }

    pub(super) fn digest(&self) -> [u8; 32] {
        let mut hash = aterm_digest::Sha256::new();
        hash.update(b"vault-frontier/1");
        hash.update(self.network);
        for id in &self.heads {
            hash.update(id);
        }
        hash.finalize()
    }

    /// Advance only from a complete causal prefix; partial intake stays repairable without
    /// changing the acknowledgement or mistaking an old ancestor for a concurrent head.
    pub(super) fn plan(&self, log: &Log) -> Result<Option<Delta>> {
        if log.network() != &self.network {
            return Err(Error::new(
                "membership records belong to another retained network",
            ));
        }
        if !covers(log, &self.network, &self.heads()) {
            return Ok(None);
        }
        let next: BTreeSet<_> = log.heads().into_iter().collect();
        if next == self.heads {
            return Ok(None);
        }
        let delta = Delta {
            before: self.digest(),
            added: next.difference(&self.heads).copied().collect(),
            retired: self.heads.difference(&next).copied().collect(),
        };
        // Validate the exact delta before any durable write can acknowledge it.
        let mut checked = self.clone();
        checked.apply(&delta)?;
        Ok(Some(delta))
    }

    pub(super) fn apply(&mut self, delta: &Delta) -> Result<()> {
        let ordered = |ids: &[RecordId]| ids.windows(2).all(|pair| pair[0] < pair[1]);
        if delta.before != self.digest()
            || delta.added.is_empty()
            || !ordered(&delta.added)
            || !ordered(&delta.retired)
            || delta
                .added
                .iter()
                .any(|id| *id == [0; 32] || self.heads.contains(id) || self.retired.contains(id))
            || delta.retired.iter().any(|id| !self.heads.contains(id))
            || self
                .heads
                .len()
                .saturating_add(self.retired.len())
                .saturating_add(delta.added.len())
                > MAX_IDS
        {
            return Err(Error::new("invalid retained membership frontier delta"));
        }
        for id in &delta.retired {
            self.heads.remove(id);
            self.retired.insert(*id);
        }
        self.heads.extend(delta.added.iter().copied());
        Ok(())
    }

    /// Read exactly the acknowledged causal prefix from a possibly newer raw candidate.
    pub(super) fn project(&self, log: &Log) -> Result<Log> {
        if self.is_empty()
            || !covers(log, &self.network, &self.heads())
            || self.retired.iter().any(|id| log.get(id).is_none())
        {
            return Err(Error::of_kind(
                crate::ErrorKind::MembershipHistoryUnavailable,
                "the acknowledged membership history is unavailable",
            ));
        }
        if self.heads() == log.heads() {
            return Ok(log.clone());
        }
        let projected = log.project_heads(&self.heads())?;
        if projected.heads() != self.heads()
            || self.retired.iter().any(|id| projected.get(id).is_none())
        {
            return Err(Error::of_kind(
                crate::ErrorKind::MembershipHistoryUnavailable,
                "the retained frontier omits previously acknowledged history",
            ));
        }
        Ok(projected)
    }
}

impl Delta {
    pub(super) fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(44 + 32 * (self.added.len() + self.retired.len()));
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&self.before);
        bytes.extend_from_slice(&(self.added.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(self.retired.len() as u32).to_le_bytes());
        for id in self.added.iter().chain(&self.retired) {
            bytes.extend_from_slice(id);
        }
        bytes
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<Self> {
        let invalid = || Error::new("invalid retained membership frontier encoding");
        if bytes.len() < 44 || bytes.len() > MAX_DELTA || &bytes[..4] != MAGIC {
            return Err(invalid());
        }
        let read_count = |at| -> Result<usize> {
            Ok(u32::from_le_bytes(bytes[at..at + 4].try_into().map_err(|_| invalid())?) as usize)
        };
        let added = read_count(36)?;
        let retired = read_count(40)?;
        if added > MAX_IDS || retired > MAX_IDS || bytes.len() != 44 + 32 * (added + retired) {
            return Err(invalid());
        }
        let ids = |at: usize, count: usize| -> Result<Vec<RecordId>> {
            Ok(bytes[at..at + count * 32].as_chunks::<32>().0.to_vec())
        };
        Ok(Self {
            before: bytes[4..36].try_into().map_err(|_| invalid())?,
            added: ids(44, added)?,
            retired: ids(44 + added * 32, retired)?,
        })
    }
}

/// A retained head has all its ancestors; every head must remain in the candidate graph.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultRetainedFrontier",
        action = "OtherNetwork",
        reason = "Environment supplies a candidate for another authenticated network"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultRetainedFrontier",
        action = "LoseAncestor",
        reason = "Environment supplies a valid candidate missing an acknowledged ancestor"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VaultRetainedFrontier",
        action = "Admit",
        project = "aterm_secrets::client::retained_history_tests::retained_frontier_transition"
    )
)]
pub(super) fn covers(log: &Log, network: &RecordId, heads: &[RecordId]) -> bool {
    log.network() == network && heads.iter().all(|head| log.get(head).is_some())
}
