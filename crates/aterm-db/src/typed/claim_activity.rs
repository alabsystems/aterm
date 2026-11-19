// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Activity periods are separate from lifecycle reassertions and storage carries.

use super::{Collection, cell, owned::Op};
use astream_wire::sync::{Digest, Index, Projection};
use aterm_json::Value;
#[cfg(any(unix, test))]
use aterm_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// `None` is the initial, implicitly live period; other periods name a signed reactivation, or
/// a live lifecycle origin a carry held with no proof of its period ([`Activity::read_from`]).
pub(super) type Epochs = BTreeSet<Option<Digest>>;

pub(super) fn decode_epochs(value: &Value) -> Option<Epochs> {
    let values = value.as_array()?;
    if values.len() > crate::database::MAX_RECORDS {
        return None;
    }
    let mut out = Epochs::new();
    for value in values {
        let epoch = if value.is_null() {
            None
        } else {
            Some(super::claim::digest(value)?)
        };
        if out.last().is_some_and(|last| *last >= epoch) {
            return None;
        }
        out.insert(epoch);
    }
    Some(out)
}

#[cfg(any(unix, test))]
pub(super) fn encode_epochs(epochs: &Epochs) -> Value {
    Value::Array(
        epochs
            .iter()
            .map(|epoch| {
                epoch.map_or(Value::Null, |id| {
                    Value::String(aterm_codec::hex::encode_lower(&id))
                })
            })
            .collect(),
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Proof {
    Unknown,
    Invalid,
    Known(Epochs),
}

impl Proof {
    pub(super) fn decode(value: &Value) -> Option<Self> {
        if value.is_null() {
            return Some(Self::Unknown);
        }
        if value == &Value::Bool(false) {
            return Some(Self::Invalid);
        }
        let object = value.as_object()?;
        if object.len() != 2 || object.get("v")?.as_u64()? != 2 {
            return None;
        }
        Some(Self::Known(decode_epochs(object.get("l")?)?))
    }

    #[cfg(any(unix, test))]
    pub(super) fn encode(&self) -> Value {
        match self {
            Self::Unknown => Value::Null,
            Self::Invalid => Value::Bool(false),
            Self::Known(epochs) => json!({"v": 2, "l": encode_epochs(epochs)}),
        }
    }

    pub(super) fn merge(&mut self, other: Self) {
        match (&*self, &other) {
            (Self::Invalid, _) | (_, Self::Invalid) => *self = Self::Invalid,
            (Self::Unknown, Self::Known(_)) => *self = other,
            (Self::Known(left), Self::Known(right)) if left != right => *self = Self::Invalid,
            _ => {}
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct State {
    pub(super) live: bool,
    pub(super) epochs: Option<Arc<Epochs>>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            live: true,
            epochs: Some(Arc::new(Epochs::from([None]))),
        }
    }
}

#[derive(Clone)]
struct Frame {
    frontier: BTreeSet<Digest>,
    state: State,
}

struct Row {
    frames: BTreeMap<Digest, Arc<Frame>>,
    current: State,
    #[cfg(any(unix, test))]
    writes: BTreeSet<Digest>,
}

/// What a floor keeps of a collection's lifecycle ([`super::claim::Floor`], tracker LT-B10D3):
/// each row's latest lifecycle writes in the cut, with the row's state at each; the row's state
/// at each claim write the floor keeps; and every proof a carried lifecycle value held.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Kept {
    pub(super) rows: BTreeMap<String, BTreeMap<Digest, KeptLife>>,
    pub(super) at: BTreeMap<String, BTreeMap<Digest, State>>,
    pub(super) proofs: BTreeMap<(String, Digest), Proof>,
}

/// One latest lifecycle write a floor keeps: its value, whether a revert wrote it, and the
/// row's state there.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct KeptLife {
    pub(super) value: Value,
    pub(super) reverted: bool,
    pub(super) state: State,
}

impl Kept {
    /// Whether the floor holds `pk` in the initial period at every write it keeps of it: the
    /// state [`Activity::read_from`]'s equal-assertion shortcut gives every write.
    fn initial(&self, pk: &str) -> bool {
        let initial = State::default();
        self.rows
            .get(pk)
            .is_none_or(|writes| writes.values().all(|write| write.state == initial))
            && self
                .at
                .get(pk)
                .is_none_or(|states| states.values().all(|state| *state == initial))
    }
}

/// A read of the records above a floor from what the floor keeps ([`Kept`]): `below` says
/// whether a record is at or below it. A record above the floor that names one below takes
/// that record's state from the floor's latest lifecycle writes it observes, joined; the
/// initial period where it observes none.
pub(super) struct Seed<'a> {
    pub(super) kept: &'a Kept,
    pub(super) below: &'a dyn Fn(&Digest) -> bool,
}

pub(super) struct Activity {
    default: State,
    rows: BTreeMap<String, Row>,
    proofs: BTreeMap<(String, Digest), Proof>,
    pub(super) reentries: BTreeMap<Digest, BTreeSet<String>>,
}

fn readable_live(
    collection: &Collection,
    pk: &str,
    values: impl Iterator<Item = (Digest, Value)>,
) -> (bool, bool) {
    let cells = cell::Cells::from([(
        (collection.name.into(), format!("{pk}.{}", super::LIFECYCLE)),
        crate::database::Cell {
            stopped: false,
            candidates: values
                .map(|(operation, value)| crate::database::Candidate { operation, value })
                .collect(),
        },
    )]);
    let read = cell::project(collection, &cells);
    read.rows.get(pk).map_or((true, true), |row| {
        (row.live(), !row.raw.contains(super::LIFECYCLE))
    })
}

impl Activity {
    #[cfg(test)]
    pub(super) fn read(
        collection: &Collection,
        operations: &[&Op],
        index: &Index,
        projection: &Projection,
    ) -> Self {
        Self::read_inner::<true>(
            collection,
            operations,
            index,
            projection,
            &projection.ordered,
            None,
        )
    }

    /// The rows' activity over `order`, the records a read walks in causal order: every
    /// ordered record, or, from a floor's [`Seed`], the records it keeps and those above it.
    pub(super) fn read_from(
        collection: &Collection,
        operations: &[&Op],
        index: &Index,
        projection: &Projection,
        order: &[Digest],
        seed: Option<&Seed<'_>>,
    ) -> Self {
        Self::read_inner::<true>(collection, operations, index, projection, order, seed)
    }

    #[cfg(test)]
    pub(super) fn read_reference(
        collection: &Collection,
        operations: &[&Op],
        index: &Index,
    ) -> Self {
        let projection = index.project();
        Self::read_inner::<false>(
            collection,
            operations,
            index,
            &projection,
            &projection.ordered,
            None,
        )
    }

    fn read_inner<const OPTIMIZED: bool>(
        collection: &Collection,
        operations: &[&Op],
        index: &Index,
        projection: &Projection,
        order: &[Digest],
        seed: Option<&Seed<'_>>,
    ) -> Self {
        let mut out = Self {
            default: State::default(),
            rows: BTreeMap::new(),
            proofs: BTreeMap::new(),
            reentries: BTreeMap::new(),
        };
        let mut rows = BTreeMap::<String, BTreeMap<Digest, (Value, bool)>>::new();
        if let Some(seed) = seed {
            out.proofs.clone_from(&seed.kept.proofs);
            for (pk, writes) in &seed.kept.rows {
                let row = rows.entry(pk.clone()).or_default();
                for (id, write) in writes {
                    row.insert(*id, (write.value.clone(), write.reverted));
                }
            }
        }
        for operation in operations {
            for change in &operation.changes {
                if change.collection != collection.name {
                    continue;
                }
                let Ok(key) = cell::CellKey::parse(&change.key) else {
                    continue;
                };
                if key.col != super::LIFECYCLE || key.elem.is_some() {
                    continue;
                }
                let value = change.value.clone().unwrap_or(Value::Null);
                if let Some(origins) = cell::carried_refs(&value) {
                    for (item, (origin, _)) in value["k"].as_array().unwrap().iter().zip(origins) {
                        let proof = item
                            .get("c")
                            .and_then(Proof::decode)
                            .unwrap_or(Proof::Unknown);
                        out.proofs
                            .entry((key.pk.into(), origin))
                            .and_modify(|old| old.merge(proof.clone()))
                            .or_insert(proof);
                    }
                }
                rows.entry(key.pk.into())
                    .or_default()
                    .insert(operation.id, (value, operation.revert.is_some()));
            }
        }
        // The projection's sparse vectors answer reachability across every row and merge.
        // With a checkpoint floor they can also attest ancestry below retained history;
        // lifecycle frames only follow retained parent links.
        let use_vectors = OPTIMIZED && index.floor().is_empty();
        let observes = |later: &Digest, earlier: &Digest| {
            if use_vectors {
                projection.observes(index, later, earlier)
            } else {
                index.observes(later, earlier)
            }
        };
        for (pk, writes) in rows {
            // Equal native assertions cannot conflict on any causal frontier, and no
            // inactive period exists between them. Keep their source identities for
            // lifecycle carries without retaining one frame per unrelated operation.
            let first = &writes.first_key_value().unwrap().1.0;
            if OPTIMIZED
                && cell::carried_refs(first).is_none()
                && writes
                    .values()
                    .all(|(value, reverted)| !reverted && value == first)
                && seed.is_none_or(|seed| seed.kept.initial(&pk))
                && readable_live(
                    collection,
                    &pk,
                    std::iter::once((*writes.first_key_value().unwrap().0, first.clone())),
                ) == (true, true)
            {
                out.rows.insert(
                    pk,
                    Row {
                        frames: BTreeMap::new(),
                        current: out.default.clone(),
                        #[cfg(any(unix, test))]
                        writes: writes.into_keys().collect(),
                    },
                );
                continue;
            }
            let empty = Arc::new(Frame {
                frontier: BTreeSet::new(),
                state: out.default.clone(),
            });
            let mut frames = BTreeMap::<Digest, Arc<Frame>>::new();
            let mut current = BTreeSet::new();
            // From a floor: the row's state at each write it keeps is the floor's, and its
            // latest lifecycle writes there are where its current state starts.
            let kept = seed.map(|seed| {
                let lifecycle = seed.kept.rows.get(&pk);
                for (id, write) in lifecycle.into_iter().flatten() {
                    frames.insert(
                        *id,
                        Arc::new(Frame {
                            frontier: BTreeSet::from([*id]),
                            state: write.state.clone(),
                        }),
                    );
                    current.insert(*id);
                }
                for (id, state) in seed.kept.at.get(&pk).into_iter().flatten() {
                    if !frames.contains_key(id) {
                        let frontier = Self::observed(lifecycle, id, &observes);
                        frames.insert(
                            *id,
                            Arc::new(Frame {
                                frontier,
                                state: state.clone(),
                            }),
                        );
                    }
                }
                (seed, lifecycle)
            });
            for id in order {
                if kept.is_some_and(|(seed, _)| (seed.below)(id)) {
                    continue;
                }
                let Some(header) = index.get(id) else {
                    continue;
                };
                let mut parents = Vec::new();
                for parent in &header.parents {
                    if !frames.contains_key(parent)
                        && let Some((seed, lifecycle)) = kept
                        && (seed.below)(parent)
                    {
                        // A record below the floor the floor does not keep: the latest
                        // lifecycle writes it keeps that this record observes, joined.
                        let frontier = Self::observed(lifecycle, parent, &observes);
                        let frame = if frontier.is_empty() {
                            empty.clone()
                        } else {
                            Arc::new(Self::joined(
                                collection,
                                &pk,
                                frontier,
                                &writes,
                                &frames,
                                &out.default,
                            ))
                        };
                        frames.insert(*parent, frame);
                    }
                    if let Some(frame) = frames.get(parent)
                        && !parents.iter().any(|other| Arc::ptr_eq(other, frame))
                    {
                        parents.push(frame.clone());
                    }
                }
                let prior = match parents.len() {
                    0 => empty.clone(),
                    1 => parents.pop().unwrap(),
                    _ => {
                        let union: BTreeSet<_> = parents
                            .iter()
                            .flat_map(|frame| frame.frontier.iter().copied())
                            .collect();
                        let frontier = union
                            .iter()
                            .filter(|candidate| {
                                !union
                                    .iter()
                                    .any(|other| other != *candidate && observes(other, candidate))
                            })
                            .copied()
                            .collect();
                        Arc::new(Self::joined(
                            collection,
                            &pk,
                            frontier,
                            &writes,
                            &frames,
                            &out.default,
                        ))
                    }
                };
                let Some((value, reverted)) = writes.get(id) else {
                    frames.insert(*id, prior);
                    continue;
                };
                let (live, readable) =
                    readable_live(collection, &pk, std::iter::once((*id, value.clone())));
                let epochs = if !readable {
                    None
                } else if !live {
                    Some(Arc::new(Epochs::new()))
                } else if !reverted && cell::carried_refs(value).is_some() {
                    let items = value["k"].as_array().unwrap();
                    items
                        .iter()
                        .try_fold(Epochs::new(), |mut all, item| {
                            let origin = super::claim::digest(&item["o"]).unwrap();
                            match out.proofs.get(&(pk.clone(), origin)) {
                                Some(Proof::Known(known)) if !known.is_empty() => {
                                    all.extend(known.iter().copied());
                                }
                                // A carry with no proof (0.199.0's roll wrote no claim context)
                                // names no period, yet the row was live in one: it is known
                                // here by the carried origin. A claim written in it keeps its
                                // tenure, and so does a later carry of that claim with that
                                // period; a carried claim with no period of its own stays
                                // unknown (`Causality::origin_epochs`).
                                Some(Proof::Unknown) => {
                                    all.insert(Some(origin));
                                }
                                _ => return None,
                            }
                            Some(all)
                        })
                        .map(Arc::new)
                } else if prior.state.live
                    && prior
                        .state
                        .epochs
                        .as_ref()
                        .is_some_and(|epochs| !epochs.is_empty())
                {
                    prior.state.epochs.clone()
                } else {
                    out.reentries.entry(*id).or_default().insert(pk.clone());
                    Some(Arc::new(Epochs::from([Some(*id)])))
                };
                frames.insert(
                    *id,
                    Arc::new(Frame {
                        frontier: BTreeSet::from([*id]),
                        state: State { live, epochs },
                    }),
                );
                current.retain(|old| !observes(id, old));
                current.insert(*id);
            }
            let state =
                Self::joined(collection, &pk, current, &writes, &frames, &out.default).state;
            out.rows.insert(
                pk,
                Row {
                    frames,
                    current: state,
                    #[cfg(any(unix, test))]
                    writes: writes.into_keys().collect(),
                },
            );
        }
        out
    }

    /// The writes of `lifecycle` (a floor's latest lifecycle writes of one row, which no one of
    /// them observes another) that `id` is or observes.
    fn observed(
        lifecycle: Option<&BTreeMap<Digest, KeptLife>>,
        id: &Digest,
        observes: &impl Fn(&Digest, &Digest) -> bool,
    ) -> BTreeSet<Digest> {
        lifecycle
            .into_iter()
            .flat_map(BTreeMap::keys)
            .filter(|write| *write == id || observes(id, write))
            .copied()
            .collect()
    }

    /// What a floor at the cut `operations` and `order` read keeps of this activity
    /// ([`Kept`]): each row's latest lifecycle writes with the state there, the state at each
    /// of `claims` (a row and a claim write the floor keeps), and every carried proof.
    pub(super) fn kept(
        &self,
        collection: &Collection,
        operations: &[&Op],
        index: &Index,
        order: &[Digest],
        claims: impl Iterator<Item = (String, Digest)>,
    ) -> Kept {
        let mut writes = BTreeMap::<String, BTreeMap<Digest, (Value, bool)>>::new();
        for operation in operations {
            for change in &operation.changes {
                if change.collection != collection.name {
                    continue;
                }
                let Ok(key) = cell::CellKey::parse(&change.key) else {
                    continue;
                };
                if key.col != super::LIFECYCLE || key.elem.is_some() {
                    continue;
                }
                writes.entry(key.pk.into()).or_default().insert(
                    operation.id,
                    (
                        change.value.clone().unwrap_or(Value::Null),
                        operation.revert.is_some(),
                    ),
                );
            }
        }
        let mut rows = BTreeMap::new();
        for (pk, writes) in writes {
            let mut latest = BTreeSet::<Digest>::new();
            for id in order.iter().filter(|id| writes.contains_key(*id)) {
                latest.retain(|old| !index.observes(id, old));
                latest.insert(*id);
            }
            let kept = latest
                .into_iter()
                .map(|id| {
                    let (value, reverted) = writes[&id].clone();
                    let state = self.at(&pk, &id).clone();
                    (
                        id,
                        KeptLife {
                            value,
                            reverted,
                            state,
                        },
                    )
                })
                .collect();
            rows.insert(pk, kept);
        }
        let mut at = BTreeMap::<String, BTreeMap<Digest, State>>::new();
        for (pk, id) in claims {
            let state = self.at(&pk, &id).clone();
            at.entry(pk).or_default().insert(id, state);
        }
        Kept {
            rows,
            at,
            proofs: self.proofs.clone(),
        }
    }

    fn joined(
        collection: &Collection,
        pk: &str,
        frontier: BTreeSet<Digest>,
        writes: &BTreeMap<Digest, (Value, bool)>,
        frames: &BTreeMap<Digest, Arc<Frame>>,
        default: &State,
    ) -> Frame {
        if frontier.is_empty() {
            return Frame {
                frontier,
                state: default.clone(),
            };
        }
        let (live, readable) = readable_live(
            collection,
            pk,
            frontier.iter().map(|id| (*id, writes[id].0.clone())),
        );
        let epochs = if !readable {
            None
        } else if !live {
            Some(Arc::new(Epochs::new()))
        } else {
            frontier
                .iter()
                .try_fold(Epochs::new(), |mut epochs, id| {
                    epochs.extend(frames.get(id)?.state.epochs.as_ref()?.iter().copied());
                    Some(epochs)
                })
                .map(Arc::new)
        };
        Frame {
            frontier,
            state: State { live, epochs },
        }
    }

    pub(super) fn at(&self, pk: &str, id: &Digest) -> &State {
        self.rows
            .get(pk)
            .and_then(|row| row.frames.get(id))
            .map_or(&self.default, |frame| &frame.state)
    }

    pub(super) fn current(&self, pk: &str) -> &State {
        self.rows.get(pk).map_or(&self.default, |row| &row.current)
    }

    #[cfg(any(unix, test))]
    pub(super) fn origin_proof(&self, pk: &str, origin: &Digest) -> Proof {
        if self
            .rows
            .get(pk)
            .is_some_and(|row| row.writes.contains(origin))
        {
            return self
                .at(pk, origin)
                .epochs
                .as_ref()
                .map_or(Proof::Unknown, |epochs| Proof::Known((**epochs).clone()));
        }
        self.proofs
            .get(&(pk.into(), *origin))
            .cloned()
            .unwrap_or(Proof::Unknown)
    }
}
