// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Causal application synchronization above astream's local durable log.
//!
//! Local broker offsets cannot name multi-writer history. This index uses immutable
//! record digests and actor-incarnation sequences instead. It never authenticates
//! bytes: the caller verifies signatures, authorization and the digest BEFORE insert,
//! and durably stores the full record BEFORE acknowledging it. Rebuild on restart.
//! Inventory is a set of digests, including forks: a sequence watermark alone hides
//! two signed records at the same sequence. No clocks, network, retention or I/O.

use std::collections::{BTreeMap, BTreeSet};

/// SHA-256 content identity, computed and verified by the integrating application.
pub type Digest = [u8; 32];
/// Application-defined public actor key plus fresh installation incarnation.
pub type Actor = [u8; 48];
/// Bound on references carried by one record.
pub const MAX_PARENTS: usize = 64;

/// An authenticated record's replication header; payload semantics live above this index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    /// Content digest of the complete authenticated record.
    pub id: Digest,
    /// Writer and installation incarnation, scoped by the enclosing index.
    pub actor: Actor,
    /// Dense actor sequence, starting at one.
    pub sequence: u64,
    /// Previous record of this actor; also a member of `parents`.
    pub previous: Option<Digest>,
    /// Sorted, unique exact dependencies. References never imply another audience's data.
    pub parents: Vec<Digest>,
}

/// An index refusal; none changes the index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Zero sequence, impossible previous link, unordered/duplicate parents, or self-reference.
    Shape,
    /// A previously supplied digest names a different header.
    DigestCollision,
    /// The configured retained-record bound is full. Do not acknowledge this record.
    Capacity,
}

contract_fn! {
    /// Whether `next` densely follows `previous`, without wrapping at `u64::MAX`.
    pub const fn sequence_follows(previous: u64, next: u64) -> bool
    clauses [ensures result == (previous < u64::MAX && next > 0 && next - 1 == previous)]
    { previous < u64::MAX && next == previous + 1 }
}

contract_fn! {
    /// Discharge one previously unfulfilled edge; saturates only for an empty set.
    pub const fn discharge(unfulfilled: u64) -> u64
    clauses [ensures result <= unfulfilled
             && (result == 0) == (unfulfilled <= 1)
             && (unfulfilled == 0 || result + 1 == unfulfilled)]
    { if unfulfilled == 0 { 0 } else { unfulfilled - 1 } }
}

impl Header {
    fn shaped(&self) -> bool {
        self.sequence > 0
            && (self.sequence == 1) == self.previous.is_none()
            && self.parents.len() <= MAX_PARENTS
            && !self.parents.contains(&self.id)
            && self.parents.windows(2).all(|p| p[0] < p[1])
            && self
                .previous
                .is_none_or(|p| self.parents.binary_search(&p).is_ok())
    }
}

/// Rebuildable, bounded causal index for ONE dataset/audience generation.
#[derive(Clone, Debug)]
pub struct Index {
    records: BTreeMap<Digest, Header>,
    capacity: usize,
}

/// Deterministic projection of the whole known authenticated history.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Projection {
    /// Causal order; simultaneous ready records are ordered by digest.
    pub ordered: Vec<Digest>,
    /// Maximal visible records; include these in a new operation's dependencies.
    pub heads: BTreeSet<Digest>,
    /// Every actor that signed distinct records at the same dense sequence.
    pub quarantined: BTreeSet<Actor>,
    /// Records missing prerequisites, with impossible links, or depending on quarantine.
    pub pending: BTreeSet<Digest>,
}

impl Index {
    /// Empty index with an explicit capacity. The integrator also bounds payload bytes.
    pub fn new(capacity: usize) -> Self {
        Self {
            records: BTreeMap::new(),
            capacity,
        }
    }

    /// Insert a verified header. `false` is an identical duplicate.
    ///
    /// # Errors
    /// [`Refusal`] names malformed headers, digest collisions and exhausted capacity.
    pub fn insert(&mut self, header: Header) -> Result<bool, Refusal> {
        if !header.shaped() {
            return Err(Refusal::Shape);
        }
        if let Some(old) = self.records.get(&header.id) {
            return if old == &header {
                Ok(false)
            } else {
                Err(Refusal::DigestCollision)
            };
        }
        if self.records.len() >= self.capacity {
            return Err(Refusal::Capacity);
        }
        self.records.insert(header.id, header);
        Ok(true)
    }

    /// Every retained digest, including pending records and fork evidence.
    pub fn inventory(&self) -> impl Iterator<Item = &Digest> {
        self.records.keys()
    }

    /// Header by exact content identity.
    pub fn get(&self, id: &Digest) -> Option<&Header> {
        self.records.get(id)
    }

    /// Whether `later` transitively names `earlier` as a dependency. Equality is
    /// reflexive only for a retained record. Missing history never proves observation.
    pub fn observes(&self, later: &Digest, earlier: &Digest) -> bool {
        if !self.records.contains_key(later) {
            return false;
        }
        let mut todo = vec![*later];
        let mut visited = BTreeSet::new();
        while let Some(id) = todo.pop() {
            if !visited.insert(id) {
                continue;
            }
            if &id == earlier {
                return self.records.contains_key(earlier);
            }
            if let Some(header) = self.records.get(&id) {
                todo.extend(&header.parents);
            }
        }
        false
    }

    /// Recompute from the set, withdrawing an equivocating actor AND all its dependents.
    /// Never incrementally accept a winner based on which branch arrived first.
    pub fn project(&self) -> Projection {
        self.project_excluding(&BTreeSet::new())
    }

    /// Derive a projection under deterministic application policy exclusions.
    /// Excluded records remain in inventory and fork detection; every dependent
    /// is withheld by the same exact-parent closure as missing or forked history.
    /// The policy must derive from authenticated retained facts, not arrival time.
    pub fn project_excluding(&self, excluded: &BTreeSet<Digest>) -> Projection {
        let mut out = Projection::default();
        let mut dots = BTreeMap::new();
        for header in self.records.values() {
            if let Some(old) = dots.insert((header.actor, header.sequence), header.id) {
                if old != header.id {
                    out.quarantined.insert(header.actor);
                }
            }
        }
        let mut waiting = BTreeMap::<Digest, u64>::new();
        let mut children = BTreeMap::<Digest, Vec<Digest>>::new();
        let mut ready = BTreeSet::new();
        for header in self.records.values() {
            out.pending.insert(header.id);
            if out.quarantined.contains(&header.actor) || excluded.contains(&header.id) {
                continue;
            }
            if let Some(previous) = header.previous {
                let Some(parent) = self.records.get(&previous) else {
                    continue;
                };
                if parent.actor != header.actor
                    || !sequence_follows(parent.sequence, header.sequence)
                {
                    continue;
                }
            }
            waiting.insert(header.id, header.parents.len() as u64);
            if header.parents.is_empty() {
                ready.insert(header.id);
            }
            for parent in &header.parents {
                children.entry(*parent).or_default().push(header.id);
            }
        }
        while let Some(id) = ready.pop_first() {
            out.pending.remove(&id);
            out.ordered.push(id);
            out.heads.insert(id);
            if let Some(header) = self.records.get(&id) {
                for parent in &header.parents {
                    out.heads.remove(parent);
                }
            }
            for child in children.get(&id).into_iter().flatten() {
                if let Some(count) = waiting.get_mut(child) {
                    *count = discharge(*count);
                    if *count == 0 {
                        ready.insert(*child);
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn header(id: u8, actor: u8, sequence: u64, previous: Option<u8>, parents: &[u8]) -> Header {
        Header {
            id: [id; 32],
            actor: [actor; 48],
            sequence,
            previous: previous.map(|p| [p; 32]),
            parents: parents.iter().map(|p| [*p; 32]).collect(),
        }
    }
    fn permutations(items: &[Header], visit: &mut impl FnMut(&[Header])) {
        fn walk(prefix: &mut Vec<Header>, rest: Vec<Header>, visit: &mut impl FnMut(&[Header])) {
            if rest.is_empty() {
                visit(prefix);
                return;
            }
            for i in 0..rest.len() {
                let mut next = rest.clone();
                prefix.push(next.remove(i));
                walk(prefix, next, visit);
                prefix.pop();
            }
        }
        walk(&mut Vec::new(), items.to_vec(), visit);
    }
    #[test]
    fn offline_islands_converge_after_every_delivery_order_and_duplicate() {
        let records = [
            header(1, 1, 1, None, &[]),
            header(2, 2, 1, None, &[]),
            header(3, 1, 2, Some(1), &[1, 2]),
            header(4, 2, 2, Some(2), &[2]),
        ];
        let mut reference = Index::new(8);
        for h in &records {
            reference.insert(h.clone()).unwrap();
        }
        let expected = reference.project();
        permutations(&records, &mut |order| {
            let mut index = Index::new(8);
            for h in order {
                assert!(index.insert(h.clone()).unwrap());
                assert!(!index.insert(h.clone()).unwrap());
                let p = index.project();
                let mut seen = BTreeSet::new();
                for id in p.ordered {
                    assert!(index
                        .get(&id)
                        .unwrap()
                        .parents
                        .iter()
                        .all(|parent| seen.contains(parent)));
                    seen.insert(id);
                }
            }
            assert_eq!(index.project(), expected);
        });
        assert_eq!(expected.heads, BTreeSet::from([[3; 32], [4; 32]]));
        assert!(reference.observes(&[3; 32], &[1; 32]));
        assert!(!reference.observes(&[4; 32], &[1; 32]));
    }
    #[test]
    fn forks_withdraw_past_effects_and_dependents_even_when_watermarks_match() {
        let a = header(1, 1, 1, None, &[]);
        let b = header(2, 1, 1, None, &[]);
        let child = header(3, 2, 1, None, &[1]);
        permutations(&[a, b, child], &mut |order| {
            let mut index = Index::new(8);
            for h in order {
                index.insert(h.clone()).unwrap();
            }
            let p = index.project();
            assert!(p.ordered.is_empty());
            assert_eq!(p.quarantined, BTreeSet::from([[1; 48]]));
            assert_eq!(p.pending.len(), 3);
            assert_eq!(
                index.inventory().count(),
                3,
                "fork evidence stays in repair inventory"
            );
        });
    }
    #[test]
    fn gaps_cycles_bad_predecessors_and_caps_cannot_become_visible() {
        let mut index = Index::new(3);
        index.insert(header(2, 1, 2, Some(1), &[1])).unwrap();
        assert!(index.project().ordered.is_empty());
        index.insert(header(1, 2, 1, None, &[])).unwrap();
        assert_eq!(index.project().ordered, [[1; 32]]);
        assert_eq!(
            index.insert(header(5, 3, 1, None, &[5])),
            Err(Refusal::Shape)
        );
        index.insert(header(3, 3, 1, None, &[4])).unwrap();
        assert_eq!(
            index.insert(header(4, 4, 1, None, &[3])),
            Err(Refusal::Capacity)
        );
        assert_eq!(index.inventory().count(), 3);
        assert!(!sequence_follows(u64::MAX, 0));
        assert!(!sequence_follows(0, 0));
        assert!(sequence_follows(0, 1));
    }

    #[test]
    fn policy_exclusions_withhold_dependents_without_losing_evidence() {
        let mut index = Index::new(8);
        for h in [
            header(1, 1, 1, None, &[]),
            header(2, 2, 1, None, &[1]),
            header(3, 3, 1, None, &[]),
        ] {
            index.insert(h).unwrap();
        }
        let view = index.project_excluding(&BTreeSet::from([[1; 32]]));
        assert_eq!(view.ordered, [[3; 32]]);
        assert_eq!(view.pending, BTreeSet::from([[1; 32], [2; 32]]));
        assert_eq!(index.inventory().count(), 3);
        assert_eq!(
            index.project().ordered.len(),
            3,
            "the evidence set is unchanged"
        );
    }
}
