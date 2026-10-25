// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Authority evidence readable without historical decryption keys. Restrictions
//! and historical device bindings constrain an independently validated membership
//! grant; they never grant membership. Union is monotone across branches and recovery.

use super::binding::Binding;
#[cfg(unix)]
use super::node::Node;
use crate::{Error, Result};
#[cfg(all(test, unix))]
use astream_wire::sync::Header;
use astream_wire::sync::{Actor, Digest, Index};
use std::collections::{BTreeMap, BTreeSet};

/// Per-node fact bound. The control journal independently bounds retained nodes.
pub const MAX_FACTS: usize = 64;
#[cfg(unix)]
const MAX_RETIRED_ACTORS: usize = 4096;
#[cfg(unix)]
const MAX_BINDINGS: usize = 4096;

/// Last permitted point on one exact writer's previous-link chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Frontier {
    pub sequence: u64,
    pub digest: Digest,
}

/// Signed authority evidence. None can remove or overwrite an earlier fact.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Fact {
    /// This writer incarnation can never receive another membership grant.
    Retire { actor: Actor },
    /// Current encryption must use this epoch or a later independently committed
    /// one. Conflicting commitments at the highest epoch withhold key use.
    Epoch { generation: u64, commitment: Digest },
    /// Restrict one table's catalog history. Multiple cutoffs intersect; None
    /// permanently withholds every catalog operation by this writer in this table.
    Cutoff {
        table: Digest,
        actor: Actor,
        through: Option<Frontier>,
    },
    /// Permanent authority attestation of an actor's canonical device identity.
    /// This is retained evidence, not an active membership grant.
    Bind { binding: Binding },
}
impl Fact {
    pub(crate) fn validate(&self) -> Result<()> {
        let valid_actor = |actor: &Actor| actor[..32] != [0; 32] && actor[32..] != [0; 16];
        let valid = match self {
            Self::Retire { actor } => valid_actor(actor),
            Self::Epoch {
                generation,
                commitment,
            } => *generation > 0 && *commitment != [0; 32],
            Self::Cutoff {
                table,
                actor,
                through,
            } => {
                *table != [0; 32]
                    && valid_actor(actor)
                    && through.is_none_or(|f| f.sequence > 0 && f.digest != [0; 32])
            }
            Self::Bind { binding } => Binding::decode(&binding.bytes()).is_ok(),
        };
        if valid {
            Ok(())
        } else {
            Err(Error::new("invalid control restriction"))
        }
    }
    pub(crate) fn encode(&self, wire: &mut Vec<u8>) {
        match self {
            Self::Retire { actor } => {
                wire.push(1);
                wire.extend_from_slice(actor);
            }
            Self::Epoch {
                generation,
                commitment,
            } => {
                wire.push(2);
                wire.extend_from_slice(&generation.to_le_bytes());
                wire.extend_from_slice(commitment);
            }
            Self::Cutoff {
                table,
                actor,
                through,
            } => {
                wire.push(3);
                wire.extend_from_slice(table);
                wire.extend_from_slice(actor);
                let frontier = through.unwrap_or(Frontier {
                    sequence: 0,
                    digest: [0; 32],
                });
                wire.extend_from_slice(&frontier.sequence.to_le_bytes());
                wire.extend_from_slice(&frontier.digest);
            }
            Self::Bind { binding } => {
                wire.push(4);
                wire.extend_from_slice(&binding.bytes());
            }
        }
    }
}

/// Current network-key evidence. This says nothing about possession of the key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Epoch {
    /// Legacy encrypted history cannot establish complete key restrictions.
    Legacy,
    Missing,
    Unique {
        generation: u64,
        commitment: Digest,
    },
    Conflict {
        generation: u64,
    },
    Reused {
        generation: u64,
    },
    MissingRetirements {
        generation: u64,
    },
}

/// A union of evidence from a causally ready control journal. Its fields are
/// immutable; consumers cannot relabel its network, head or legacy status.
#[derive(Clone, Debug)]
pub struct Constraints {
    network: [u8; 16],
    authority: [u8; 32],
    head: Digest,
    legacy: bool,
    binding_legacy: bool,
    facts: BTreeSet<Fact>,
    key_coverage: BTreeMap<(u64, Digest), bool>,
    bindings: BTreeMap<Actor, Binding>,
    writer_bindings: BTreeMap<[u8; 32], ([u8; 16], crate::age::Recipient)>,
    ambiguous_binding: bool,
    missing_retired_binding: bool,
    retired_writers: BTreeSet<[u8; 32]>,
    retired_recipients: BTreeSet<crate::age::Recipient>,
}
impl Constraints {
    #[cfg(unix)]
    pub(crate) fn collect<'a>(
        network: [u8; 16],
        authority: [u8; 32],
        head: Digest,
        nodes: impl Iterator<Item = &'a Node>,
    ) -> Result<Self> {
        let nodes: BTreeMap<_, _> = nodes.map(|node| (node.id(), node)).collect();
        if nodes
            .values()
            .any(|node| node.network() != network || node.authority() != authority)
        {
            return Err(Error::new("control evidence has another trust context"));
        }
        let mut out = Self {
            network,
            authority,
            head,
            legacy: false,
            binding_legacy: false,
            facts: BTreeSet::new(),
            key_coverage: BTreeMap::new(),
            bindings: BTreeMap::new(),
            writer_bindings: BTreeMap::new(),
            ambiguous_binding: false,
            missing_retired_binding: false,
            retired_writers: BTreeSet::new(),
            retired_recipients: BTreeSet::new(),
        };
        for node in nodes.values() {
            out.legacy |= node.legacy();
            out.binding_legacy |= !node.binding_evidence();
            out.facts.extend(node.facts().iter().cloned());
        }
        let retired: BTreeMap<_, _> = out
            .facts
            .iter()
            .filter_map(|fact| match fact {
                Fact::Retire { actor } => Some(*actor),
                _ => None,
            })
            .enumerate()
            .map(|(bit, actor)| (actor, bit))
            .collect();
        if retired.len() > MAX_RETIRED_ACTORS {
            return Err(Error::new(
                "control retirements exceed the policy computation bound",
            ));
        }
        let mut binding_count = 0;
        for fact in &out.facts {
            if let Fact::Bind { binding } = fact {
                binding_count += 1;
                if binding_count > MAX_BINDINGS {
                    return Err(Error::new(
                        "control bindings exceed the policy computation bound",
                    ));
                }
                if let Some(previous) = out.bindings.insert(binding.actor(), binding.clone()) {
                    out.ambiguous_binding |= previous != *binding;
                }
                let identity = (*binding.machine(), binding.recipient());
                if let Some(previous) = out.writer_bindings.insert(*binding.writer(), identity) {
                    out.ambiguous_binding |= previous != identity;
                }
            }
        }
        for actor in retired.keys() {
            if let Some(binding) = out.bindings.get(actor) {
                out.retired_writers.insert(*binding.writer());
                out.retired_recipients.insert(binding.recipient());
            } else {
                out.missing_retired_binding = true;
            }
        }
        let mut index = Index::new(nodes.len());
        for node in nodes.values() {
            index
                .insert(node.header())
                .map_err(|_| Error::new("control restriction graph is invalid"))?;
        }
        let projection = index.project();
        if !projection.pending.is_empty()
            || projection.heads.len() != 1
            || !projection.heads.contains(&head)
        {
            return Err(Error::new(
                "control restrictions require their exact ready head",
            ));
        }
        // At the journal's node cap, 4096 nodes x 4096 actors is 2 MiB.
        // Bits name facts, not occurrences: repeating a retirement is idempotent.
        let words = retired.len().div_ceil(64);
        let mut ancestry: BTreeMap<Digest, Vec<u64>> = BTreeMap::new();
        for id in projection.ordered {
            let node = nodes[&id];
            let mut bits = vec![0; words];
            for parent in node.parents() {
                if let Some(known) = ancestry.get(parent) {
                    for (word, inherited) in bits.iter_mut().zip(known) {
                        *word |= inherited;
                    }
                }
            }
            for fact in node.facts() {
                if let Fact::Retire { actor } = fact {
                    let bit = retired[actor];
                    bits[bit / 64] |= 1u64 << (bit % 64);
                }
            }
            let covers = (0..retired.len()).all(|bit| bits[bit / 64] & (1u64 << (bit % 64)) != 0);
            for fact in node.facts() {
                if let Fact::Epoch {
                    generation,
                    commitment,
                } = fact
                {
                    *out.key_coverage
                        .entry((*generation, *commitment))
                        .or_insert(true) &= covers;
                }
            }
            ancestry.insert(id, bits);
        }
        Ok(out)
    }
    /// Network whose pinned authority signed these facts.
    pub const fn network(&self) -> [u8; 16] {
        self.network
    }
    /// Exact ready head when this snapshot was taken; recheck before using it.
    pub const fn head(&self) -> Digest {
        self.head
    }
    /// Legacy encrypted records cannot prove absence of hidden revocations.
    pub const fn has_legacy(&self) -> bool {
        self.legacy
    }
    /// All signed facts, including those on non-selected branches.
    pub fn facts(&self) -> impl Iterator<Item = &Fact> {
        self.facts.iter()
    }
    /// Check a proposed enrollment against all retained binding and retirement
    /// evidence. Success permits consideration only; it grants no membership.
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbBindingLedger",
            action = "ObserveBinding",
            project = "aterm_secrets::commons::constraints::binding_tests::binding_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbBindingLedger",
            action = "ObserveRetirement",
            project = "aterm_secrets::commons::constraints::binding_tests::binding_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbBindingLedger",
            action = "ObserveConflict",
            project = "aterm_secrets::commons::constraints::binding_tests::binding_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbBindingLedger",
            action = "Recover",
            project = "aterm_secrets::commons::constraints::binding_tests::binding_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbBindingLedger",
            action = "ObserveWriterConflict",
            project = "aterm_secrets::commons::constraints::binding_tests::binding_transition"
        )
    )]
    pub fn check_device_candidate(&self, binding: &Binding) -> Result<()> {
        self.complete()?;
        if self.binding_legacy {
            return Err(Error::new(
                "legacy control history cannot establish device bindings",
            ));
        }
        if *binding.network() != self.network || *binding.authority() != self.authority {
            return Err(Error::new("device binding has another trust context"));
        }
        if self.ambiguous_binding || self.missing_retired_binding {
            return Err(Error::new(
                "device binding history is conflicting or incomplete",
            ));
        }
        if self.retired_writers.contains(binding.writer())
            || self.retired_recipients.contains(&binding.recipient())
            || self
                .writer_bindings
                .get(binding.writer())
                .is_some_and(|known| *known != (*binding.machine(), binding.recipient()))
            || self
                .bindings
                .get(&binding.actor())
                .is_some_and(|known| known != binding)
        {
            return Err(Error::new(
                "device binding reuses a retired or conflicting identity",
            ));
        }
        Ok(())
    }
    /// Additionally require this exact binding in the retained signed ledger.
    /// Current roster inclusion and key-release policy remain separate checks.
    pub fn check_recorded_device(&self, binding: &Binding) -> Result<()> {
        self.check_device_candidate(binding)?;
        if self.bindings.get(&binding.actor()) != Some(binding) {
            return Err(Error::new(
                "device binding is not recorded in control history",
            ));
        }
        Ok(())
    }
    /// Refuse incomplete evidence and permanently retired writer incarnations.
    /// Passing this check is not a membership grant.
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbRetirementUnion",
            action = "ObserveRetirement",
            project = "aterm_secrets::commons::constraints::tests::retirement_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbRetirementUnion",
            action = "ObserveOther",
            project = "aterm_secrets::commons::constraints::tests::retirement_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbRetirementUnion",
            action = "Recover",
            project = "aterm_secrets::commons::constraints::tests::retirement_transition"
        )
    )]
    pub fn check_actor(&self, actor: &Actor) -> Result<()> {
        self.complete()?;
        if self.facts.contains(&Fact::Retire { actor: *actor }) {
            Err(Error::new("writer incarnation is permanently retired"))
        } else {
            Ok(())
        }
    }
    fn complete(&self) -> Result<()> {
        if self.legacy {
            Err(Error::new(
                "legacy control history has incomplete restriction evidence",
            ))
        } else {
            Ok(())
        }
    }
    /// Highest signed key epoch. A higher fresh epoch may resolve a conflict;
    /// choosing an older payload or reusing an old key cannot do so.
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbRetirementEpoch",
            action = "Retire",
            project = "aterm_secrets::commons::constraints::tests::rotation_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbRetirementEpoch",
            action = "ConcurrentEpoch",
            project = "aterm_secrets::commons::constraints::tests::rotation_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbRetirementEpoch",
            action = "Rotate",
            project = "aterm_secrets::commons::constraints::tests::rotation_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbRetirementEpoch",
            action = "RepeatRetirement",
            project = "aterm_secrets::commons::constraints::tests::rotation_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbKeyEpochUnion",
            action = "ObserveOld",
            project = "aterm_secrets::commons::constraints::tests::epoch_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbKeyEpochUnion",
            action = "ObserveNew",
            project = "aterm_secrets::commons::constraints::tests::epoch_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbKeyEpochUnion",
            action = "ObserveConflict",
            project = "aterm_secrets::commons::constraints::tests::epoch_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbKeyEpochUnion",
            action = "ObserveFresh",
            project = "aterm_secrets::commons::constraints::tests::epoch_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbKeyEpochUnion",
            action = "ObserveReuse",
            project = "aterm_secrets::commons::constraints::tests::epoch_transition"
        )
    )]
    pub fn epoch(&self) -> Epoch {
        if self.legacy {
            return Epoch::Legacy;
        }
        let highest = self
            .facts
            .iter()
            .filter_map(|f| match f {
                Fact::Epoch { generation, .. } => Some(*generation),
                _ => None,
            })
            .max();
        let Some(generation) = highest else {
            return Epoch::Missing;
        };
        let commitments: BTreeSet<_> = self
            .facts
            .iter()
            .filter_map(|f| match f {
                Fact::Epoch {
                    generation: g,
                    commitment,
                } if *g == generation => Some(*commitment),
                _ => None,
            })
            .collect();
        if commitments.len() != 1 {
            return Epoch::Conflict { generation };
        }
        let commitment = *commitments.first().unwrap();
        if self.facts.iter().any(|f| matches!(f, Fact::Epoch { generation: g, commitment: c } if *g != generation && *c == commitment)) {
            return Epoch::Reused { generation };
        }
        if self.key_coverage.get(&(generation, commitment)) != Some(&true) {
            return Epoch::MissingRetirements { generation };
        }
        Epoch::Unique {
            generation,
            commitment,
        }
    }
    /// Check the actual key against all known epoch evidence before encrypting
    /// new data. Membership and audience checks remain independently required.
    pub fn check_key(&self, generation: u64, key: &[u8; 32]) -> Result<()> {
        self.complete()?;
        let expected = key_commitment(self.network, key);
        match self.epoch() {
            Epoch::Unique {
                generation: g,
                commitment,
            } if g == generation && commitment == expected && *key != [0; 32] => Ok(()),
            _ => Err(Error::new(
                "network key does not match a unique fresh current epoch",
            )),
        }
    }
    /// Prepare a bounded catalog-history filter using this table's authenticated
    /// causal index. It must not filter budget debits or retired actors' liabilities.
    /// A passing operation still needs its normal positive application authorization.
    /// The caller authenticates every indexed operation under this exact network
    /// and table identity; a bare astream index carries no such namespace binding.
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbCatalogCutoff",
            action = "ReceiveParent",
            project = "aterm_secrets::commons::constraints::tests::catalog_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbCatalogCutoff",
            action = "ReceiveChild",
            project = "aterm_secrets::commons::constraints::tests::catalog_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbCatalogCutoff",
            action = "ReceiveCutoff",
            project = "aterm_secrets::commons::constraints::tests::catalog_transition"
        )
    )]
    pub fn catalog(&self, table: Digest, index: &Index) -> Catalog {
        if self.legacy || table == [0; 32] {
            return Catalog {
                visible: BTreeSet::new(),
            };
        }
        let visible: BTreeSet<_> = index.project().ordered.into_iter().collect();
        let mut retired = BTreeSet::new();
        let mut cutoffs: BTreeMap<Actor, Vec<Option<Frontier>>> = BTreeMap::new();
        for fact in &self.facts {
            match fact {
                Fact::Retire { actor } => {
                    retired.insert(*actor);
                }
                Fact::Cutoff {
                    table: fact_table,
                    actor,
                    through,
                } if *fact_table == table => {
                    cutoffs.entry(*actor).or_default().push(*through);
                }
                _ => {}
            }
        }
        let allowed: BTreeMap<_, _> = cutoffs
            .iter()
            .map(|(actor, frontiers)| {
                (
                    *actor,
                    allowed_prefix(index, &visible, *actor, frontiers).unwrap_or_default(),
                )
            })
            .collect();
        let excluded = visible
            .iter()
            .filter(|id| {
                let operation = index.get(id).unwrap();
                match allowed.get(&operation.actor) {
                    Some(prefix) => !prefix.contains(*id),
                    None => retired.contains(&operation.actor),
                }
            })
            .copied()
            .collect();
        // Withhold every dependent too: another writer cannot carry excluded
        // state back into the projection by naming it as a causal parent.
        Catalog {
            visible: index
                .project_excluding(&excluded)
                .ordered
                .into_iter()
                .collect(),
        }
    }
}

/// Domain-separated commitment independent of epoch number, so reuse is visible.
/// As with the existing cryptographic primitives, internal hash state is not a
/// promise to erase every temporary copy of a key from process memory.
pub fn key_commitment(network: [u8; 16], key: &[u8; 32]) -> Digest {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm/commons/network-key/1\0");
    hash.update(network);
    hash.update(key);
    hash.finalize()
}

/// A table-scoped restriction view, computed once for a batch of history reads.
pub struct Catalog {
    visible: BTreeSet<Digest>,
}
impl Catalog {
    /// Whether this operation satisfies the retained catalog restrictions and is
    /// causally visible. Missing or equivocating history never proves a cutoff.
    pub fn permits(&self, id: &Digest) -> bool {
        self.visible.contains(id)
    }
}

/// Walk one exact writer chain once. Check every signed frontier on that chain,
/// retaining the intersection, without repeatedly walking it for each operation.
/// Unrelated causal parents above the permitted prefix do not affect its proof.
fn allowed_prefix(
    index: &Index,
    visible: &BTreeSet<Digest>,
    actor: Actor,
    frontiers: &[Option<Frontier>],
) -> Option<BTreeSet<Digest>> {
    let mut points = BTreeMap::new();
    for frontier in frontiers {
        let frontier = frontier.as_ref()?;
        if points
            .insert(frontier.sequence, frontier.digest)
            .is_some_and(|old| old != frontier.digest)
        {
            return None;
        }
    }
    let (&minimum, _) = points.first_key_value()?;
    let (&highest, digest) = points.last_key_value()?;
    let mut end = index.get(digest)?;
    if end.sequence != highest {
        return None;
    }
    let mut prefix = BTreeSet::new();
    loop {
        if end.actor != actor || points.get(&end.sequence).is_some_and(|id| *id != end.id) {
            return None;
        }
        if aterm_db_core::prefix_candidate(minimum, end.sequence, end.actor == actor)
            && visible.contains(&end.id)
        {
            prefix.insert(end.id);
        }
        if end.sequence == 1 {
            return Some(prefix);
        }
        let previous = index.get(&end.previous?)?;
        if !astream_wire::sync::sequence_follows(previous.sequence, end.sequence) {
            return None;
        }
        end = previous;
    }
}

#[cfg(all(test, unix))]
mod tests;

#[cfg(all(test, unix))]
mod binding_tests;
