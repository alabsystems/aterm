// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An independent control history with encrypted payloads and public signed
//! restrictions. Peer repair never advances
//! the ordinary secrets materialization cursor. A unique, complete DAG head is
//! necessary for use; applications must also validate its payload and policy.

use super::constraints::Constraints;
use super::node::{Kind, Node};
use crate::{Error, Result, SecretBytes, age::Identity};
use astream_wire::{
    Offset,
    sync::{Digest, Index},
};
use aterm_journal::{Limits, Log};
#[cfg(test)]
use aterm_uds::private_file;
#[cfg(test)]
use std::fs::OpenOptions;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

/// Maximum retained control records, including pending and competing branches.
pub const MAX_NODES: usize = 4096;
const MAX_BYTES: usize = 128 * 1024 * 1024;
// Leave room to join a generation's many data heads in batches of at most 64.
// This is recovery headroom, not an unbounded-growth or rollover guarantee.
const RECOVERY_NODES: usize = MAX_NODES.div_ceil(astream_wire::sync::MAX_PARENTS - 1);
const MAX_RECOVERY_BYTES: usize = 4 + 16 + 32 + 2 + 64 * 32 + 32 + 8 + 2 + 4 + 64 + 256;
const RECOVERY_BYTES: usize = RECOVERY_NODES * MAX_RECOVERY_BYTES;
const SUBJECT: &str = "commons/control/1";

/// Exact independent trust anchor. Obtain this from authenticated enrollment,
/// never from a peer advertisement. Root handoff requires a separate protocol;
/// replacing an existing journal's anchor is deliberately refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trust {
    /// Network identity bound into every signed node.
    pub network: [u8; 16],
    /// Pinned control authority, distinct from a transport certificate.
    pub authority: [u8; 32],
    /// Exact initial node, preventing competing genesis snapshots.
    pub genesis: Digest,
}
impl Trust {
    fn bytes(self) -> Vec<u8> {
        let mut bytes = b"ACT1".to_vec();
        bytes.extend_from_slice(&self.network);
        bytes.extend_from_slice(&self.authority);
        bytes.extend_from_slice(&self.genesis);
        bytes
    }
}

/// Local first durable observation, independent of the signed author's clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Receipt {
    /// Content identity acknowledged by the durable append.
    pub id: Digest,
    /// This replica's wall-clock observation, never a global ordering rule.
    pub received_ms: u64,
    /// Local astream position; not comparable across machines.
    pub offset: u64,
}

/// Causal state of the entire retained history. Even a late orphan or fork
/// withdraws readiness. Nothing here certifies application payload semantics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// No signed events have arrived.
    Empty,
    /// An append or checkpoint failed; reopen before reading current authority.
    Uncertain,
    /// The pinned genesis or some exact dependencies have not arrived.
    Pending,
    /// Authenticated protocol faults remain evidence and grant no current state.
    Invalid { nodes: Vec<Digest> },
    /// Complete competing branches require an explicit authority recovery.
    Conflict { heads: Vec<Digest> },
    /// Unique complete head and the data node its recovery selection resolves to.
    Ready { head: Digest, basis: Digest },
}

/// Decrypted current data. Debug output redacts the payload via `SecretBytes`.
#[derive(Debug)]
pub struct Current {
    /// Unique causally complete head.
    pub head: Digest,
    /// Data node selected through zero or more recoveries.
    pub basis: Digest,
    /// Opaque application snapshot, still requiring schema and policy validation.
    pub payload: SecretBytes,
    /// Restrictions accumulated across every retained branch, not just `basis`.
    pub constraints: Constraints,
}

/// Bounded strict astream log and its disposable causal index. Open, append and
/// close locally; do not hold this exclusive writer across peer network IO.
pub struct Journal {
    trust: Trust,
    index: Index,
    nodes: BTreeMap<Digest, Node>,
    receipts: BTreeMap<Digest, Receipt>,
    log: Log,
}
impl Journal {
    /// Immutable network, authority and genesis pinned when this journal opened.
    pub const fn trust(&self) -> Trust {
        self.trust
    }
    /// Replay all authenticated ciphertext under the exact immutable anchor.
    /// A missing anchor file can be restored only to the caller's independently
    /// trusted value; replay still verifies every event against it.
    pub fn open(dir: &Path, trust: Trust) -> Result<Self> {
        Self::open_mode(dir, trust, false)
    }

    /// Reopen an established journal. Missing anchor/log/checkpoint state is
    /// damage, not permission to initialize an empty history. An outer durable
    /// bootstrap pin uses this after the journal has first been initialized.
    pub fn open_existing(dir: &Path, trust: Trust) -> Result<Self> {
        Self::open_mode(dir, trust, true)
    }

    fn open_mode(dir: &Path, trust: Trust, established: bool) -> Result<Self> {
        let log = Log::open(
            dir,
            "control",
            &trust.bytes(),
            established,
            Limits {
                records: MAX_NODES,
                bytes: MAX_BYTES,
                record_bytes: super::node::MAX_NODE + 8,
            },
        )?;
        let mut journal = Self {
            trust,
            index: Index::new(MAX_NODES),
            nodes: BTreeMap::new(),
            receipts: BTreeMap::new(),
            log,
        };
        let mut offset = Offset::ZERO;
        loop {
            let batch = journal.log.read_range(offset, 256)?;
            if batch.is_empty() {
                break;
            }
            for record in batch {
                if record.subject != SUBJECT || record.body.len() < 8 {
                    return Err(Error::new("foreign record in control journal"));
                }
                let received_ms = u64::from_le_bytes(
                    record.body[..8]
                        .try_into()
                        .map_err(|_| Error::new("invalid control receipt"))?,
                );
                let node = journal.verify(&record.body[8..])?;
                journal
                    .index
                    .insert(node.header())
                    .map_err(|_| Error::new("control journal index refused a record"))?;
                let id = node.id();
                journal.nodes.entry(id).or_insert(node);
                journal.receipts.entry(id).or_insert(Receipt {
                    id,
                    received_ms,
                    offset: record.seq.0,
                });
                offset = Offset(
                    record
                        .seq
                        .0
                        .checked_add(1)
                        .ok_or_else(|| Error::new("control journal offset exhausted"))?,
                );
            }
        }
        journal.log.finish_open()?;
        Ok(journal)
    }

    fn verify(&self, wire: &[u8]) -> Result<Node> {
        let node = Node::verify(wire, self.trust.network, self.trust.authority)?;
        if node.kind() == Kind::Genesis && node.id() != self.trust.genesis {
            return Err(Error::new("control node names another genesis"));
        }
        Ok(node)
    }

    /// Authenticate and persist exact bytes before acknowledging them. Duplicates
    /// return their original receipt. A failed flush poisons this handle until
    /// reopen: callers retain the prepared bytes and retry those same bytes.
    pub fn ingest(&mut self, wire: &[u8]) -> Result<Receipt> {
        if self.log.is_poisoned() {
            return Err(Error::new(
                "uncertain control append: reopen and retry the exact prepared bytes",
            ));
        }
        let node = self.verify(wire)?;
        let id = node.id();
        if let Some(receipt) = self.receipts.get(&id) {
            return Ok(*receipt);
        }
        let next_bytes = self.log.bytes().saturating_add(wire.len() + 256);
        let (data_nodes, data_bytes) = data_usage(self.nodes.values());
        if node.kind() != Kind::Recovery
            && (data_nodes >= MAX_NODES - RECOVERY_NODES
                || data_bytes.saturating_add(wire.len() + 256) > MAX_BYTES - RECOVERY_BYTES)
        {
            return Err(Error::new(
                "control data capacity exhausted; remaining space is reserved for recovery",
            ));
        }
        let mut proposed = self.index.clone();
        proposed.insert(node.header()).map_err(|_| {
            Error::new("control journal record capacity exhausted or header refused")
        })?;
        // Competing recoveries also consume space. Preserve enough slots and
        // bytes to join the remaining frontier in maximally sized batches.
        let frontier = repair_frontier(&proposed, self.trust.genesis);
        let required = frontier
            .saturating_sub(1)
            .div_ceil(astream_wire::sync::MAX_PARENTS - 1);
        if self.nodes.len() + 1 + required > MAX_NODES
            || next_bytes.saturating_add(required * MAX_RECOVERY_BYTES) > MAX_BYTES
        {
            return Err(Error::new(
                "control journal recovery headroom exhausted; preserve history",
            ));
        }
        let received_ms = aterm_time::unix_millis()
            .ok_or_else(|| Error::new("system clock cannot represent a receipt timestamp"))?;
        let mut body = received_ms.to_le_bytes().to_vec();
        body.extend_from_slice(wire);
        let offset = self.log.append(SUBJECT, body)?.0;
        let receipt = Receipt {
            id,
            received_ms,
            offset,
        };
        self.index = proposed;
        self.nodes.insert(id, node);
        self.receipts.insert(id, receipt);
        Ok(receipt)
    }

    /// Derive readiness from the whole retained set, never receipt order or time.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbMembershipJournal",
            action = "ReceiveLeft",
            project = "aterm_secrets::commons::journal::tests::journal_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbMembershipJournal",
            action = "ReceiveRight",
            project = "aterm_secrets::commons::journal::tests::journal_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbMembershipJournal",
            action = "ReceiveTail",
            project = "aterm_secrets::commons::journal::tests::journal_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbMembershipJournal",
            action = "ReceiveRecovery",
            project = "aterm_secrets::commons::journal::tests::journal_transition"
        )
    )]
    pub fn state(&self) -> State {
        if self.log.is_poisoned() {
            return State::Uncertain;
        }
        if self.nodes.is_empty() {
            return State::Empty;
        }
        let projection = self.index.project();
        let complete =
            self.nodes.contains_key(&self.trust.genesis) && projection.pending.is_empty();
        let invalid: Vec<_> = self
            .nodes
            .values()
            .filter(|node| {
                node.kind() == Kind::Recovery && related_parents(&self.index, node.parents())
            })
            .map(Node::id)
            .collect();
        if !invalid.is_empty() {
            return State::Invalid { nodes: invalid };
        }
        if !complete {
            return State::Pending;
        }
        let heads: Vec<_> = projection.heads.into_iter().collect();
        if !aterm_db_core::control_ready(complete, heads.len() as u64) {
            return State::Conflict { heads };
        }
        let head = heads[0];
        if !self.index.observes(&head, &self.trust.genesis) {
            return State::Pending;
        }
        let mut basis = head;
        // Every selection is a parent, so complete acyclic ancestry terminates.
        for _ in 0..self.nodes.len() {
            let Some(node) = self.nodes.get(&basis) else {
                return State::Pending;
            };
            match node.selected() {
                Some(parent) => basis = parent,
                None => return State::Ready { head, basis },
            }
        }
        State::Pending
    }

    /// Decrypt only a unique complete snapshot; incomplete/conflicting histories
    /// return no current value. Schema and monotone revocation checks belong to
    /// the consuming membership application, before it grants any privileges.
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbFoundation",
            action = "NoAccess",
            project = "aterm_secrets::commons::node::foundation::tests::foundation_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbFoundationRepair",
            action = "FirstA",
            project = "aterm_secrets::commons::node::foundation::tests::repair_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbFoundationRepair",
            action = "FirstB",
            project = "aterm_secrets::commons::node::foundation::tests::repair_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbFoundationRepair",
            action = "Exchange",
            project = "aterm_secrets::commons::node::foundation::tests::repair_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbFoundationRepair",
            action = "Recover",
            project = "aterm_secrets::commons::node::foundation::tests::repair_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbFoundationRepair",
            action = "Rotate",
            project = "aterm_secrets::commons::node::foundation::tests::repair_transition"
        )
    )]
    pub fn current(&self, identity: &Identity) -> Result<Option<Current>> {
        let State::Ready { head, basis } = self.state() else {
            return Ok(None);
        };
        let node = self
            .nodes
            .get(&basis)
            .ok_or_else(|| Error::new("current control basis missing"))?;
        if node.is_foundation() {
            return Ok(None);
        }
        Ok(Some(Current {
            head,
            basis,
            payload: node.open(identity)?,
            constraints: Constraints::collect(
                self.trust.network,
                self.trust.authority,
                head,
                self.nodes.values(),
            )?,
        }))
    }

    /// Open a data snapshot on the selected history, for historical validation
    /// only. A recovery's unselected parents remain evidence, not positive grants.
    /// The roster consumer must separately establish current local membership.
    pub(crate) fn historical(&self, id: Digest, identity: &Identity) -> Result<Current> {
        let (node, constraints) = self.historical_source(id)?;
        Ok(Current {
            head: id,
            basis: id,
            payload: node.open(identity)?,
            constraints,
        })
    }

    pub(crate) fn historical_with_key(&self, id: Digest, key: &[u8; 16]) -> Result<Current> {
        let (node, constraints) = self.historical_source(id)?;
        Ok(Current {
            head: id,
            basis: id,
            payload: node.open_history_key(key)?,
            constraints,
        })
    }

    fn historical_source(&self, id: Digest) -> Result<(&Node, Constraints)> {
        let State::Ready { head, .. } = self.state() else {
            return Err(Error::new(
                "historical access requires ready control history",
            ));
        };
        if !self.selected_ancestor(id, head) {
            return Err(Error::new("roster is outside the selected control history"));
        }
        let ancestors = self.ancestors(id)?;
        Ok((
            &self.nodes[&id],
            Constraints::collect(
                self.trust.network,
                self.trust.authority,
                id,
                ancestors.iter().map(|ancestor| &self.nodes[ancestor]),
            )?,
        ))
    }

    fn ancestors(&self, id: Digest) -> Result<BTreeSet<Digest>> {
        let mut ancestors = BTreeSet::new();
        let mut pending = vec![id];
        while let Some(parent) = pending.pop() {
            if ancestors.insert(parent) {
                let node = self
                    .nodes
                    .get(&parent)
                    .ok_or_else(|| Error::new("missing historical ancestor"))?;
                pending.extend(node.parents());
            }
        }
        Ok(ancestors)
    }

    /// Whether a data node is on this descendant's selected path, including
    /// the descendant itself. Recovery's unselected parents never qualify.
    /// This graph query grants no membership or key access.
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbFoundation",
            action = "NoHistorical",
            project = "aterm_secrets::commons::node::foundation::tests::foundation_transition"
        )
    )]
    pub fn selected_ancestor(&self, ancestor: Digest, descendant: Digest) -> bool {
        let mut cursor = descendant;
        for _ in 0..self.nodes.len() {
            let Some(node) = self.nodes.get(&cursor) else {
                return false;
            };
            if cursor == ancestor && node.has_payload() {
                return true;
            }
            let Some(parent) = node.selected().or_else(|| node.parents().first().copied()) else {
                return false;
            };
            cursor = parent;
        }
        false
    }

    /// Observe the union of restrictive facts without a decryption key, only
    /// under a unique complete current head. This is not a positive grant.
    pub fn constraints(&self) -> Result<Constraints> {
        let State::Ready { head, .. } = self.state() else {
            return Err(Error::new(
                "control restrictions require a complete unambiguous history",
            ));
        };
        Constraints::collect(
            self.trust.network,
            self.trust.authority,
            head,
            self.nodes.values(),
        )
    }

    /// Validate a prepared ordinary child without publishing or retaining it.
    pub(crate) fn prospective(&self, node: &Node) -> Result<Constraints> {
        let State::Ready { head, .. } = self.state() else {
            return Err(Error::new(
                "roster publication requires a ready control journal",
            ));
        };
        if node.kind() != Kind::Update || node.parents() != [head] {
            return Err(Error::new(
                "roster publication must extend the exact current head",
            ));
        }
        Constraints::collect(
            self.trust.network,
            self.trust.authority,
            node.id(),
            self.nodes.values().chain(std::iter::once(node)),
        )
    }

    /// A chosen payload cannot regain authority through cascaded recovery after
    /// a later payload observed it. Recovery-only descendants preserve it.
    pub(crate) fn payload_superseded(&self, basis: Digest) -> bool {
        let mut children: BTreeMap<Digest, Vec<&Node>> = BTreeMap::new();
        for node in self.nodes.values() {
            for parent in node.parents() {
                children.entry(*parent).or_default().push(node);
            }
        }
        let mut pending = vec![basis];
        let mut visited = BTreeSet::new();
        while let Some(parent) = pending.pop() {
            if !visited.insert(parent) {
                continue;
            }
            for child in children.get(&parent).into_iter().flatten() {
                if child.kind() != Kind::Recovery {
                    return true;
                }
                pending.push(child.id());
            }
        }
        false
    }

    /// A key-bearing roster epoch is introduced by one exact node. Replaying
    /// identical node bytes is deduplicated; re-encrypting is another publication.
    pub(crate) fn single_key_publication(&self, generation: u64, commitment: Digest) -> bool {
        let fact = super::constraints::Fact::Epoch {
            generation,
            commitment,
        };
        self.nodes
            .values()
            .filter(|node| node.facts().contains(&fact))
            .take(2)
            .count()
            == 1
    }

    /// Historical authenticity depends on the source's complete causal past,
    /// including every recovery parent. Later publications cannot rewrite that
    /// past. This check alone supplies no current access or selected-path grant.
    pub(crate) fn single_historical_key_publication(
        &self,
        source: Digest,
        generation: u64,
        commitment: Digest,
    ) -> bool {
        let Ok(ancestors) = self.ancestors(source) else {
            return false;
        };
        let fact = super::constraints::Fact::Epoch {
            generation,
            commitment,
        };
        ancestors
            .iter()
            .filter(|id| self.nodes[*id].facts().contains(&fact))
            .take(2)
            .count()
            == 1
    }

    /// All retained content identities, including pending and competing branches.
    pub fn inventory(&self) -> impl Iterator<Item = &Digest> {
        self.nodes.keys()
    }
    /// Signed ciphertext for bounded peer repair, without decrypting it.
    pub fn node(&self, id: &Digest) -> Option<&Node> {
        self.nodes.get(id)
    }
    /// This replica's first durable receipt for an exact node.
    pub fn receipt(&self, id: &Digest) -> Option<Receipt> {
        self.receipts.get(id).copied()
    }
}

// A recovery arriving early cannot consume the allowance of data ancestors still
// to arrive. The independent total-byte and total-node guards cover all records.
fn data_usage<'a>(nodes: impl Iterator<Item = &'a Node>) -> (usize, usize) {
    nodes
        .filter(|n| n.kind() != Kind::Recovery)
        .fold((0, 0), |(count, bytes), node| {
            (count + 1, bytes + node.bytes().len() + 256)
        })
}

// Pending includes every internal node of an incomplete chain. Reserve joins for
// its leaves and distinct missing boundaries instead, independent of arrival order.
fn repair_frontier(index: &Index, genesis: Digest) -> usize {
    let mut leaves: BTreeSet<_> = index.inventory().copied().collect();
    let mut missing = BTreeSet::new();
    for id in index.inventory() {
        for parent in &index.get(id).unwrap().parents {
            leaves.remove(parent);
            if *parent != genesis && index.get(parent).is_none() {
                missing.insert(*parent);
            }
        }
    }
    leaves.len() + missing.len()
}

// One ancestor walk per parent, rather than one full walk per ordered pair.
// Missing ancestors cannot invent a relation; any known relation is still a fault.
fn related_parents(index: &Index, parents: &[Digest]) -> bool {
    for parent in parents {
        let mut todo = index
            .get(parent)
            .map(|h| h.parents.clone())
            .unwrap_or_default();
        let mut seen = BTreeSet::new();
        while let Some(id) = todo.pop() {
            if !seen.insert(id) {
                continue;
            }
            if parents.binary_search(&id).is_ok() {
                return true;
            }
            if let Some(header) = index.get(&id) {
                todo.extend(&header.parents);
            }
        }
    }
    false
}

#[cfg(test)]
mod tests;
