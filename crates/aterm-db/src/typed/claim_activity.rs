// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Activity periods are separate from lifecycle reassertions and storage carries.

use super::{Collection, cell, owned::Op};
use astream_wire::sync::{Digest, Index};
use aterm_json::Value;
#[cfg(any(unix, test))]
use aterm_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// `None` is the initial, implicitly live period; other periods name a signed reactivation.
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

#[derive(Clone)]
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
    pub(super) fn read(collection: &Collection, operations: &[&Op], index: &Index) -> Self {
        let mut out = Self {
            default: State::default(),
            rows: BTreeMap::new(),
            proofs: BTreeMap::new(),
            reentries: BTreeMap::new(),
        };
        let mut rows = BTreeMap::<String, BTreeMap<Digest, (Value, bool)>>::new();
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
                    .insert(operation.id, (value, operation.revert));
            }
        }
        let ordered = index.project().ordered;
        for (pk, writes) in rows {
            let empty = Arc::new(Frame {
                frontier: BTreeSet::new(),
                state: out.default.clone(),
            });
            let mut frames = BTreeMap::<Digest, Arc<Frame>>::new();
            let mut current = BTreeSet::new();
            for id in &ordered {
                let Some(header) = index.get(id) else {
                    continue;
                };
                let mut parents = Vec::new();
                for parent in &header.parents {
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
                                !union.iter().any(|other| {
                                    other != *candidate && index.observes(other, candidate)
                                })
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
                    let mut epochs = Some(Epochs::new());
                    for item in value["k"].as_array().unwrap() {
                        let origin = super::claim::digest(&item["o"]).unwrap();
                        let known = match out.proofs.get(&(pk.clone(), origin)) {
                            Some(Proof::Known(known)) if !known.is_empty() => Some(known),
                            _ => None,
                        };
                        epochs = epochs.zip(known).map(|(mut all, next)| {
                            all.extend(next.iter().copied());
                            all
                        });
                    }
                    epochs.map(Arc::new)
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
                current.retain(|old| !index.observes(id, old));
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
