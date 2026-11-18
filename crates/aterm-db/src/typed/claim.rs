// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Canonical identity claims over signed cells. Interpreted candidates remain in `Row::fields`;
//! canonical reads use `Row::claims`. Missing ancestry is never evidence of concurrency.
//! A native reactivation reasserts its observed, unambiguous claim fields at that signed
//! event. Activity epochs bind carried claim evidence to the source period: an old scalar
//! cannot regain a claim merely because a newer lifecycle cell arrived independently.

use super::claim_activity as activity;
use super::{
    ClaimRule, Collection,
    cell::{self, CellValue, Cells, Projection},
    owned::Op,
};
use astream_wire::sync::{Digest, Index};
use aterm_digest::Sha256;
use aterm_json::Value;
#[cfg(any(unix, test))]
use aterm_json::json;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

type Coordinate = (String, String);
type History = BTreeMap<Coordinate, BTreeMap<Digest, Value>>;
type Tenures = BTreeMap<(Coordinate, Digest, String), Option<BTreeSet<Digest>>>;

/// A compensation restores a value, not a withdrawn claim's tenure. Its signed record stays
/// unchanged; the typed read attributes an unambiguous restored value to the compensation.
fn restored_value(
    collection: &Collection,
    operation: &Op,
    change: &crate::database::Change,
) -> Option<Value> {
    if operation.revert.is_none() || change.collection != collection.name {
        return None;
    }
    let key = cell::CellKey::parse(&change.key).ok()?;
    collection.column(key.col)?.claim?;
    let value = change.value.as_ref()?;
    cell::carried_refs(value)?;
    let one = Cells::from([(
        (change.collection.clone(), change.key.clone()),
        crate::database::Cell {
            stopped: false,
            candidates: vec![crate::database::Candidate {
                operation: operation.id,
                value: value.clone(),
            }],
        },
    )]);
    let projected = cell::project(collection, &one);
    let row = projected.rows.get(key.pk)?;
    if row.raw.contains(key.col) {
        return None;
    }
    match row.fields.get(key.col)? {
        cell::Field::Value(candidate) => Some(candidate.value.encode()),
        _ => None,
    }
}

/// Interpret compensated carried claims consistently for reads, grid baselines and rolls.
/// Conflicted or opaque beforeimages retain their original evidence and remain unresolved.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabClaimUndo",
        action = "Judge",
        project = "aterm_db::typed::claim::model_tests::undo_transition"
    )
)]
pub(crate) fn restored_cells<'a>(
    collection: &Collection,
    cells: &'a Cells,
    operations: impl Iterator<Item = impl std::borrow::Borrow<Op>>,
) -> Cow<'a, Cells> {
    let mut out = Cow::Borrowed(cells);
    for operation in operations {
        let operation = operation.borrow();
        if operation.revert.is_none() {
            continue;
        }
        for change in &operation.changes {
            let coordinate = (change.collection.clone(), change.key.clone());
            if !out.get(&coordinate).is_some_and(|cell| {
                cell.candidates.iter().any(|candidate| {
                    candidate.operation == operation.id
                        && cell::carried_refs(&candidate.value).is_some()
                })
            }) {
                continue;
            }
            let Some(value) = restored_value(collection, operation, change) else {
                continue;
            };
            for candidate in &mut out.to_mut().get_mut(&coordinate).unwrap().candidates {
                if candidate.operation == operation.id {
                    candidate.value = value.clone();
                }
            }
        }
    }
    out
}

/// An owner-attested first-tenure context, carried with its original cell candidate.
/// `Unknown` retains a missing legacy history without pretending it was concurrent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Provenance {
    Unknown,
    Invalid,
    Known(BTreeMap<Digest, BTreeSet<Digest>>),
}

enum ProvenanceKind {
    Unknown,
    Invalid,
    Known,
}

impl Provenance {
    pub(crate) fn decode(value: &Value) -> Option<Self> {
        let mut starts = BTreeMap::<Digest, BTreeSet<Digest>>::new();
        let kind = Self::scan(value, |start, before| {
            let past = starts.entry(start).or_default();
            if let Some(before) = before {
                past.insert(before);
            }
        })?;
        Some(match kind {
            ProvenanceKind::Unknown => Self::Unknown,
            ProvenanceKind::Invalid => Self::Invalid,
            ProvenanceKind::Known => Self::Known(starts),
        })
    }

    /// Validate every fact once; only an owning reader builds an ancestry index.
    /// `None` begins a tenure; `Some` emits its ancestor. Legacy facts are checked without
    /// emitting evidence, since their decoded meaning remains unknown.
    fn scan(value: &Value, mut fact: impl FnMut(Digest, Option<Digest>)) -> Option<ProvenanceKind> {
        if value.is_null() {
            return Some(ProvenanceKind::Unknown);
        }
        if value == &Value::Bool(false) {
            return Some(ProvenanceKind::Invalid);
        }
        // Earlier arrays prove scalar continuity but do not identify an activity period.
        // Retain them as unknown evidence rather than assigning them fresh authority.
        let (items, legacy, epoch_facts) = if let Some(items) = value.as_array() {
            (items, true, 0)
        } else {
            let object = value.as_object()?;
            if object.len() != 3 || object.get("v")?.as_u64()? != 2 {
                return None;
            }
            let epochs = activity::decode_epochs(object.get("e")?)?;
            let starts = object.get("s")?;
            if starts.is_null() {
                return Some(ProvenanceKind::Unknown);
            }
            if starts == &Value::Bool(false) {
                return Some(ProvenanceKind::Invalid);
            }
            (starts.as_array()?, false, epochs.len())
        };
        if items.is_empty() || items.len() > crate::database::MAX_RECORDS {
            return None;
        }
        let mut facts = items.len().checked_add(epoch_facts)?;
        let mut last = None;
        for item in items {
            let item = item.as_object()?;
            if item.len() != 2 {
                return None;
            }
            let start = digest(item.get("o")?)?;
            if last.is_some_and(|last| last >= start) {
                return None;
            }
            last = Some(start);
            let observed = item.get("p")?.as_array()?;
            facts = facts.checked_add(observed.len())?;
            if facts > crate::database::MAX_RECORDS {
                return None;
            }
            if !legacy {
                fact(start, None);
            }
            let mut previous = None;
            for value in observed {
                let before = digest(value)?;
                if before == start || previous.is_some_and(|last| last >= before) {
                    return None;
                }
                previous = Some(before);
                if !legacy {
                    fact(start, Some(before));
                }
            }
        }
        Some(if legacy {
            ProvenanceKind::Unknown
        } else {
            ProvenanceKind::Known
        })
    }

    #[cfg(any(unix, test))]
    fn encode_in_epochs(&self, epochs: &activity::Epochs) -> crate::Result<Value> {
        let starts = match self {
            Self::Unknown => Value::Null,
            Self::Invalid => Value::Bool(false),
            Self::Known(starts) => Value::Array(starts.iter().map(|(start, past)| json!({
                "o": aterm_codec::hex::encode_lower(start),
                "p": past.iter().map(|id| aterm_codec::hex::encode_lower(id)).collect::<Vec<_>>()
            })).collect()),
        };
        let value = json!({"v": 2, "s": starts, "e": activity::encode_epochs(epochs)});
        if Self::scan(&value, |_, _| {}).is_none() {
            return Err(crate::Error::new(
                "claim provenance exceeds the retained evidence bound; the source generation remains held",
            ));
        }
        Ok(value)
    }

    pub(crate) fn merge(&mut self, other: Self) {
        match (&mut *self, other) {
            (Self::Invalid, _) | (_, Self::Invalid) => *self = Self::Invalid,
            (Self::Unknown, known @ Self::Known(_)) => *self = known,
            (Self::Known(_), Self::Unknown) | (Self::Unknown, Self::Unknown) => {}
            (Self::Known(left), Self::Known(right)) if left.keys().eq(right.keys()) => {
                for (start, past) in right {
                    left.get_mut(&start).unwrap().extend(past);
                }
            }
            _ => *self = Self::Invalid,
        }
    }
}

pub(crate) fn valid_context(value: &Value) -> bool {
    Provenance::scan(value, |_, _| {}).is_some() || activity::Proof::decode(value).is_some()
}

fn context_epochs(value: &Value) -> Option<activity::Epochs> {
    Provenance::scan(value, |_, _| {})?;
    activity::decode_epochs(value.as_object()?.get("e")?)
}

#[cfg(unix)]
fn merged_context(left: &Value, right: &Value) -> crate::Result<Value> {
    if left == &Value::Bool(false) || right == &Value::Bool(false) {
        return Ok(Value::Bool(false));
    }
    let life = |value: &Value| {
        value
            .as_object()
            .is_some_and(|object| object.contains_key("l"))
    };
    if life(left) || life(right) {
        let mut proof = activity::Proof::decode(left).unwrap_or(activity::Proof::Invalid);
        proof.merge(activity::Proof::decode(right).unwrap_or(activity::Proof::Invalid));
        return Ok(proof.encode());
    }
    let mut provenance = Provenance::decode(left)
        .ok_or_else(|| crate::Error::new("invalid carried claim context"))?;
    let next = Provenance::decode(right)
        .ok_or_else(|| crate::Error::new("invalid carried claim context"))?;
    let epochs = match (context_epochs(left), context_epochs(right)) {
        (Some(a), Some(b)) if a != b => return Ok(Value::Bool(false)),
        (Some(a), _) | (_, Some(a)) => Some(a),
        _ => None,
    };
    provenance.merge(next);
    match epochs {
        Some(epochs) => provenance.encode_in_epochs(&epochs),
        None => Ok(match provenance {
            Provenance::Invalid => Value::Bool(false),
            _ => Value::Null,
        }),
    }
}

pub(super) fn digest(value: &Value) -> Option<Digest> {
    let text = value.as_str()?;
    let mut digest = [0; 32];
    (aterm_codec::hex::is_lower(text, 64)
        && aterm_codec::hex::decode_into(text.as_bytes(), &mut digest))
    .then_some(digest)
}

/// The encoded context at each original candidate; separate from its unchanged typed value.
#[cfg(any(unix, test))]
pub(crate) type CarriedClaims = BTreeMap<Coordinate, BTreeMap<Digest, Value>>;

/// Opaque carried contexts survive even where this reader does not know their column.
#[cfg(unix)]
pub(crate) fn carried_claims(cells: &Cells) -> crate::Result<CarriedClaims> {
    let mut out = CarriedClaims::new();
    for (coord, cell) in cells {
        for candidate in &cell.candidates {
            let Some(origins) = cell::carried_refs(&candidate.value) else {
                continue;
            };
            for (item, (origin, _)) in candidate.value["k"].as_array().unwrap().iter().zip(origins)
            {
                let Some(value) = item.get("c") else { continue };
                let next =
                    BTreeMap::from([(coord.clone(), BTreeMap::from([(origin, value.clone())]))]);
                merge_claims(&mut out, next)?;
            }
        }
    }
    Ok(out)
}

#[cfg(unix)]
pub(crate) fn merge_claims(into: &mut CarriedClaims, next: CarriedClaims) -> crate::Result<()> {
    for (coord, origins) in next {
        for (origin, value) in origins {
            let held = into.entry(coord.clone()).or_default();
            if let Some(old) = held.get_mut(&origin) {
                *old = merged_context(old, &value)?;
            } else {
                held.insert(origin, value);
            }
        }
    }
    Ok(())
}

struct Causality<'a> {
    index: &'a Index,
    order: Vec<Digest>,
    provenance: BTreeMap<(Coordinate, Digest), Provenance>,
    carriers: BTreeMap<Digest, BTreeSet<Digest>>,
    past: BTreeMap<Digest, BTreeSet<Digest>>,
    activity: activity::Activity,
    epochs: BTreeMap<(Coordinate, Digest), activity::Proof>,
    implicit: BTreeSet<Coordinate>,
}

impl Causality<'_> {
    fn origin_epochs(
        &self,
        coordinate: &Coordinate,
        origin: &Digest,
        history: &History,
    ) -> Option<activity::Epochs> {
        let key = cell::CellKey::parse(&coordinate.1).ok()?;
        if history
            .get(coordinate)
            .and_then(|writes| writes.get(origin))
            .is_some_and(|value| cell::carried_refs(value).is_none())
            && self.index.get(origin).is_some()
        {
            return self
                .activity
                .at(key.pk, origin)
                .epochs
                .as_ref()
                .map(|epochs| (**epochs).clone());
        }
        match self.epochs.get(&(coordinate.clone(), *origin)) {
            Some(activity::Proof::Known(epochs)) => return Some(epochs.clone()),
            Some(activity::Proof::Invalid) => return None,
            _ => {}
        }
        self.activity
            .current(key.pk)
            .epochs
            .as_ref()
            .filter(|epochs| epochs.len() == 1 && epochs.contains(&None))
            .map(|epochs| (**epochs).clone())
    }

    fn supports(
        &self,
        coordinate: &Coordinate,
        origin: &Digest,
        epochs: &activity::Epochs,
        history: &History,
    ) -> Option<bool> {
        self.origin_epochs(coordinate, origin, history)
            .map(|origin_epochs| !origin_epochs.is_empty() && origin_epochs.is_subset(epochs))
    }

    fn observes(&self, later: &Digest, earlier: &Digest) -> bool {
        if self.index.observes(later, earlier) {
            return true;
        }
        if self.carriers.get(earlier).is_some_and(|carriers| {
            carriers
                .iter()
                .any(|carrier| self.index.observes(later, carrier))
        }) {
            return true;
        }
        let mut todo = vec![*later];
        let mut seen = BTreeSet::new();
        while let Some(id) = todo.pop() {
            if !seen.insert(id) {
                continue;
            }
            if let Some(past) = self.past.get(&id) {
                if past.contains(earlier) {
                    return true;
                }
                todo.extend(past);
            }
        }
        false
    }
}

#[cfg(test)]
thread_local! {
    /// How many claim projections ([`project`]) this thread ran, by collection name, counted
    /// (never timed): the `files-idle` lane's drive (`crate::alab::upkeep`'s tests) reads it
    /// before and after each materializer pass.
    pub(crate) static PROJECTS: std::cell::RefCell<BTreeMap<&'static str, usize>> =
        const { std::cell::RefCell::new(BTreeMap::new()) };
    /// How many claim contexts (`context`: the activity, the history and the causality of one
    /// collection's claim events) this thread built, by collection name: one for each
    /// [`project`], [`interpreted_cells`] and [`snapshot`].
    pub(crate) static CONTEXTS: std::cell::RefCell<BTreeMap<&'static str, usize>> =
        const { std::cell::RefCell::new(BTreeMap::new()) };
}

/// One more of `counter` for `collection`.
#[cfg(test)]
fn count(
    counter: &'static std::thread::LocalKey<std::cell::RefCell<BTreeMap<&'static str, usize>>>,
    collection: &Collection,
) {
    counter.with_borrow_mut(|counts| *counts.entry(collection.name).or_default() += 1);
}

fn context<'a>(
    collection: &Collection,
    operations: impl Iterator<Item = impl std::borrow::Borrow<Op>>,
    index: &'a Index,
) -> (History, Causality<'a>) {
    #[cfg(test)]
    count(&CONTEXTS, collection);
    let operations: Vec<_> = operations.collect();
    let operations: Vec<&Op> = operations.iter().map(std::borrow::Borrow::borrow).collect();
    let activity = activity::Activity::read(collection, &operations, index);
    let mut history = History::new();
    let mut evidence = BTreeMap::<(Coordinate, Digest), (Provenance, BTreeSet<Digest>)>::new();
    let mut epochs = BTreeMap::<(Coordinate, Digest), activity::Proof>::new();
    for operation in operations {
        for change in &operation.changes {
            if change.collection != collection.name {
                continue;
            }
            let Ok(key) = cell::CellKey::parse(&change.key) else {
                continue;
            };
            if !collection
                .column(key.col)
                .is_some_and(|column| column.claim.is_some())
            {
                continue;
            }
            let coord = (change.collection.clone(), change.key.clone());
            let value = restored_value(collection, operation, change)
                .or_else(|| change.value.clone())
                .unwrap_or(Value::Null);
            if let Some(origins) = cell::carried_refs(&value) {
                for (item, (origin, _)) in value["k"].as_array().unwrap().iter().zip(origins) {
                    let provenance = item
                        .get("c")
                        .and_then(Provenance::decode)
                        .unwrap_or(Provenance::Unknown);
                    let period = item
                        .get("c")
                        .and_then(context_epochs)
                        .map_or(activity::Proof::Unknown, activity::Proof::Known);
                    epochs
                        .entry((coord.clone(), origin))
                        .and_modify(|old| old.merge(period.clone()))
                        .or_insert(period);
                    evidence
                        .entry((coord.clone(), origin))
                        .and_modify(|(old, carriers)| {
                            old.merge(provenance.clone());
                            carriers.insert(operation.id);
                        })
                        .or_insert((provenance, BTreeSet::from([operation.id])));
                }
            }
            history
                .entry(coord)
                .or_default()
                .insert(operation.id, value);
        }
    }
    let order = index.project().ordered;
    let mut implicit = BTreeSet::new();
    for id in &order {
        let Some(pks) = activity.reentries.get(id) else {
            continue;
        };
        for pk in pks {
            for column in collection
                .columns
                .iter()
                .filter(|column| column.claim.is_some())
            {
                let coordinate = (collection.name.into(), format!("{pk}.{}", column.name));
                let Some(writes) = history.get(&coordinate) else {
                    continue;
                };
                if writes.contains_key(id) {
                    continue;
                }
                let frontier = latest_writes(writes, index, &order, Some(id));
                let read = cell::project(
                    collection,
                    &Cells::from([(
                        coordinate.clone(),
                        crate::database::Cell {
                            stopped: false,
                            candidates: frontier
                                .into_iter()
                                .map(|operation| crate::database::Candidate {
                                    operation,
                                    value: writes[&operation].clone(),
                                })
                                .collect(),
                        },
                    )]),
                );
                let Some(row) = read.rows.get(pk) else {
                    continue;
                };
                if row.raw.contains(column.name) {
                    continue;
                }
                if let Some(cell::Field::Value(candidate)) = row.fields.get(column.name) {
                    let value = candidate.value.encode();
                    history.get_mut(&coordinate).unwrap().insert(*id, value);
                    implicit.insert(coordinate);
                }
            }
        }
    }
    let mut causal = Causality {
        index,
        order,
        provenance: BTreeMap::new(),
        carriers: BTreeMap::new(),
        past: BTreeMap::new(),
        activity,
        epochs,
        implicit,
    };
    for (key, (provenance, carriers)) in evidence {
        causal.carriers.entry(key.1).or_default().extend(&carriers);
        if let Provenance::Known(starts) = &provenance {
            for (start, past) in starts {
                causal.past.entry(*start).or_default().extend(past);
                // Observing this signed carry also observes the ancestry it attests.
                for id in std::iter::once(start).chain(past) {
                    causal.carriers.entry(*id).or_default().extend(&carriers);
                }
            }
        }
        causal.provenance.insert(key, provenance);
    }
    (history, causal)
}

fn latest_writes(
    writes: &BTreeMap<Digest, Value>,
    index: &Index,
    ordered: &[Digest],
    before: Option<&Digest>,
) -> BTreeSet<Digest> {
    let mut frontier = BTreeSet::new();
    for id in ordered {
        if !writes.contains_key(id)
            || before.is_some_and(|before| before == id || !index.observes(before, id))
        {
            continue;
        }
        frontier.retain(|old| !index.observes(id, old));
        frontier.insert(*id);
    }
    frontier
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabClaimRevival",
        action = "Judge",
        project = "aterm_db::typed::claim::model_tests::revival_transition"
    )
)]
fn activate_cells(cells: &mut Cow<'_, Cells>, history: &History, causal: &Causality<'_>) {
    for coordinate in &causal.implicit {
        if cells.get(coordinate).is_none_or(|cell| cell.stopped) {
            continue;
        }
        let writes = &history[coordinate];
        let frontier = latest_writes(writes, causal.index, &causal.order, None);
        let candidates = frontier
            .into_iter()
            .map(|operation| crate::database::Candidate {
                operation,
                value: writes[&operation].clone(),
            })
            .collect();
        let interpreted = crate::database::Cell {
            stopped: false,
            candidates,
        };
        if cells.get(coordinate) != Some(&interpreted) {
            cells.to_mut().insert(coordinate.clone(), interpreted);
        }
    }
}

#[cfg(any(unix, test))]
pub(crate) fn interpreted_cells<'a>(
    collection: &Collection,
    cells: &'a Cells,
    operations: impl Iterator<Item = impl std::borrow::Borrow<Op>>,
    index: &Index,
) -> Cow<'a, Cells> {
    let operations: Vec<_> = operations.collect();
    let refs = || operations.iter().map(std::borrow::Borrow::borrow);
    let mut cells = restored_cells(collection, cells, refs());
    let (history, causal) = context(collection, refs(), index);
    activate_cells(&mut cells, &history, &causal);
    cells
}

/// Restate first-tenure order against every retained claim event, including withdrawals.
/// A destination may receive these candidates separately, so current values alone cannot
/// determine which older claims it still holds. Only event identities survive, not old values.
#[cfg(any(unix, test))]
pub(crate) fn snapshot(
    collection: &Collection,
    cells: &Cells,
    operations: impl Iterator<Item = impl std::borrow::Borrow<Op>>,
    index: &Index,
) -> crate::Result<CarriedClaims> {
    let operations: Vec<_> = operations.collect();
    let refs = || operations.iter().map(std::borrow::Borrow::borrow);
    let mut cells = restored_cells(collection, cells, refs());
    let (history, causal) = context(collection, refs(), index);
    activate_cells(&mut cells, &history, &causal);
    let mut memo = Tenures::new();
    let projected = cell::project(collection, &cells);
    let mut origins = Vec::new();
    let mut witnesses: BTreeSet<Digest> = history
        .values()
        .flat_map(|writes| writes.keys().copied())
        .collect();
    for (start, past) in &causal.past {
        witnesses.insert(*start);
        witnesses.extend(past);
    }
    for (coord, cell) in cells.iter() {
        if coord.0 != collection.name || cell.stopped {
            continue;
        }
        let Ok(key) = cell::CellKey::parse(&coord.1) else {
            continue;
        };
        if projected
            .rows
            .get(key.pk)
            .is_none_or(|row| row.raw.contains(key.col))
        {
            continue;
        }
        if !collection
            .column(key.col)
            .is_some_and(|column| column.claim.is_some())
        {
            continue;
        }
        for (origin, value) in cell::origins_refs(cell) {
            let Some(wanted) = CellValue::decode(value)
                .ok()
                .and_then(|value| value.x.as_str().map(str::to_owned))
            else {
                continue;
            };
            let starts = tenure(coord, origin, &wanted, &history, &causal, &mut memo);
            if let Some(starts) = &starts {
                witnesses.extend(starts);
            }
            origins.push((coord.clone(), origin, starts));
        }
    }
    let mut out = CarriedClaims::new();
    for (coord, origin, starts) in origins {
        let provenance = starts.map_or(Provenance::Unknown, |starts| {
            Provenance::Known(
                starts
                    .into_iter()
                    .map(|start| {
                        let past = witnesses
                            .iter()
                            .filter(|other| **other != start && causal.observes(&start, other))
                            .copied()
                            .collect();
                        (start, past)
                    })
                    .collect(),
            )
        });
        let encoded = match causal.origin_epochs(&coord, &origin, &history) {
            Some(epochs) => provenance.encode_in_epochs(&epochs)?,
            None => Value::Null,
        };
        out.entry(coord).or_default().insert(origin, encoded);
    }
    for (coordinate, register) in cells.iter() {
        if coordinate.0 != collection.name {
            continue;
        }
        let Ok(key) = cell::CellKey::parse(&coordinate.1) else {
            continue;
        };
        if key.col != super::LIFECYCLE {
            continue;
        }
        for (origin, _) in cell::origins_refs(register) {
            let proof = causal.activity.origin_proof(key.pk, &origin).encode();
            if !valid_context(&proof) {
                return Err(crate::Error::new(
                    "lifecycle evidence exceeds the retained bound; the source generation remains held",
                ));
            }
            out.entry(coordinate.clone())
                .or_default()
                .insert(origin, proof);
        }
    }
    Ok(out)
}

/// Apply a collection's declared claim rules to its typed projection.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabIdentityClaims",
        action = "Judge",
        project = "aterm_db::typed::claim::model_tests::claim_transition"
    )
)]
pub(crate) fn project(
    collection: &Collection,
    cells: &Cells,
    operations: impl Iterator<Item = impl std::borrow::Borrow<Op>>,
    causal: &Index,
) -> Projection {
    judged(collection, cells, operations, causal, None)
}

/// [`project`], and how each of the collection's claimed columns settled there
/// ([`Settlement`]): what a page asks, before a Write, what a value staged in such a column will
/// read as once written.
pub(crate) fn settled(
    collection: &Collection,
    cells: &Cells,
    operations: impl Iterator<Item = impl std::borrow::Borrow<Op>>,
    causal: &Index,
) -> (Projection, Settlements) {
    let mut settlements = Settlements::new();
    let projected = judged(
        collection,
        cells,
        operations,
        causal,
        Some(&mut settlements),
    );
    (projected, settlements)
}

/// [`project`]'s work, each claimed column's [`Settlement`] kept in `keep` where one is given.
fn judged(
    collection: &Collection,
    cells: &Cells,
    operations: impl Iterator<Item = impl std::borrow::Borrow<Op>>,
    causal: &Index,
    mut keep: Option<&mut Settlements>,
) -> Projection {
    #[cfg(test)]
    count(&PROJECTS, collection);
    let operations: Vec<_> = operations.collect();
    let refs = || operations.iter().map(std::borrow::Borrow::borrow);
    let mut cells = restored_cells(collection, cells, refs());
    let (history, causal) = context(collection, refs(), causal);
    activate_cells(&mut cells, &history, &causal);
    let mut projected = cell::project(collection, &cells);
    let mut tenures = Tenures::new();
    for column in collection.columns {
        let Some(rule) = column.claim else { continue };
        let mut rows = BTreeMap::<String, Entry<Digest>>::new();
        for (pk, row) in &projected.rows {
            let live = row.live();
            // Whether a write of this row now begins a tenure the read knows: asked only of a
            // settlement kept for a page.
            let fresh = || {
                keep.is_some() && {
                    let current = causal.activity.current(pk);
                    current.live
                        && current
                            .epochs
                            .as_ref()
                            .is_some_and(|epochs| !epochs.is_empty())
                }
            };
            let Some(field) = row.fields.get(column.name) else {
                if keep.is_some() {
                    rows.insert(
                        pk.clone(),
                        Entry {
                            held: Vec::new(),
                            live,
                            value: None,
                            conflicted: false,
                            starts: None,
                            fresh: fresh(),
                        },
                    );
                }
                continue;
            };
            let held = field
                .candidates()
                .iter()
                .filter_map(|candidate| candidate.value.x.as_str().map(str::to_owned))
                .collect();
            let conflicted = field.conflicted();
            let value = field
                .value()
                .and_then(Value::as_str)
                .filter(|_| !conflicted);
            let mut entry = Entry {
                held,
                live,
                value: value.map(str::to_owned),
                conflicted,
                starts: None,
                fresh: fresh(),
            };
            let Some(value) = value.filter(|_| live) else {
                rows.insert(pk.clone(), entry);
                continue;
            };
            let coordinate = (collection.name.to_owned(), format!("{pk}.{}", column.name));
            let origins = cells
                .get(&coordinate)
                .map(cell::origins_refs)
                .unwrap_or_default();
            let mut starts = Some(BTreeSet::new());
            let current = causal.activity.current(pk);
            let mut coverage = activity::Epochs::new();
            let mut supported = false;
            for (origin, _) in origins {
                let Some(current_epochs) = current.epochs.as_ref().filter(|_| current.live) else {
                    starts = None;
                    break;
                };
                match causal.origin_epochs(&coordinate, &origin, &history) {
                    Some(epochs) if epochs.is_empty() || !epochs.is_subset(current_epochs) => {
                        continue;
                    }
                    Some(epochs) => {
                        supported = true;
                        coverage.extend(epochs);
                    }
                    None => {
                        starts = None;
                        break;
                    }
                }
                starts = starts
                    .zip(tenure(
                        &coordinate,
                        origin,
                        value,
                        &history,
                        &causal,
                        &mut tenures,
                    ))
                    .map(|(mut all, next)| {
                        all.extend(next);
                        all
                    });
            }
            if supported
                && current
                    .epochs
                    .as_ref()
                    .is_some_and(|epochs| !epochs.is_subset(&coverage))
            {
                starts = None;
            }
            entry.starts = starts;
            rows.insert(pk.clone(), entry);
        }
        for (pk, reads) in settle(rule, &rows, |later, earlier| {
            causal.observes(later, earlier)
        }) {
            projected
                .rows
                .get_mut(&pk)
                .unwrap()
                .claims
                .insert(column.name.into(), reads.map(Value::String));
        }
        if let Some(keep) = keep.as_deref_mut() {
            keep.insert(column.name, Settlement::of(rule, rows, &causal));
        }
    }
    projected
}

/// Where one row stands in a claimed column, as [`settle`] weighs it. `S` names when a claim
/// began: an operation of the read ([`Digest`]), or, for a page asking what a Write will do,
/// that Write ([`Start`]).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry<S> {
    /// The value of each candidate its cell holds: no clash label takes one, live or not.
    held: Vec<String>,
    live: bool,
    /// The one value its cell holds; `None` where it holds none, or a conflict.
    value: Option<String>,
    conflicted: bool,
    /// Where it is live and claims `value`: the operations its uninterrupted tenure of `value`
    /// began at, `None` where they cannot be known, none where it began nowhere.
    starts: Option<BTreeSet<S>>,
    /// Whether a write of a new value here now begins a tenure the read knows: the row's
    /// activity is live, in periods the read knows. Kept only for a [`Settlement`].
    fresh: bool,
}

/// What a row's [`Entry`] claims.
enum Standing<'a, S> {
    /// Nothing: it is not live, or its cell holds no value.
    Out,
    /// No value: its cell is conflicted, or its tenure began nowhere.
    Withheld,
    /// This value, from these tenure starts.
    Claims(&'a str, &'a Option<BTreeSet<S>>),
}

impl<S> Entry<S> {
    fn standing(&self) -> Standing<'_, S> {
        if !self.live {
            return Standing::Out;
        }
        if self.conflicted {
            return Standing::Withheld;
        }
        match &self.value {
            None => Standing::Out,
            Some(_) if self.starts.as_ref().is_some_and(BTreeSet::is_empty) => Standing::Withheld,
            Some(value) => Standing::Claims(value, &self.starts),
        }
    }
}

/// Settle one claimed column under `rule`: of the live rows that claim one value, the one whose
/// claim began first keeps it, by [`winner`]; under [`ClaimRule::Label`] each other reads as a
/// clash label of its own ([`clash`]), drawn after every value a row's cell holds and every
/// clash label drawn before it, values in order and rows in key order; under
/// [`ClaimRule::One`], or where no claim can be put first, they read as no value. What each row
/// reads as, where its claim is not simply its value: one that contends, and one withheld.
fn settle<S: Ord + Copy>(
    rule: ClaimRule,
    rows: &BTreeMap<String, Entry<S>>,
    observes: impl Fn(&S, &S) -> bool,
) -> BTreeMap<String, Option<String>> {
    let mut out = BTreeMap::new();
    let mut groups = BTreeMap::<&str, Vec<(String, Option<BTreeSet<S>>)>>::new();
    let mut reserved = BTreeSet::<String>::new();
    for (pk, entry) in rows {
        reserved.extend(entry.held.iter().cloned());
        if entry.live
            && !entry.conflicted
            && let Some(value) = &entry.value
        {
            reserved.insert(value.clone());
        }
        match entry.standing() {
            Standing::Out => {}
            Standing::Withheld => {
                out.insert(pk.clone(), None);
            }
            Standing::Claims(value, starts) => {
                groups
                    .entry(value)
                    .or_default()
                    .push((pk.clone(), starts.clone()));
            }
        }
    }
    for (value, contenders) in groups {
        if contenders.len() < 2 {
            continue;
        }
        let winner = winner(&contenders, &observes);
        for (pk, _) in contenders {
            if winner.as_deref() == Some(pk.as_str()) {
                out.insert(pk, Some(value.to_owned()));
                continue;
            }
            let effective = match (rule, winner.as_ref()) {
                (ClaimRule::Label, Some(_)) => clash(value, &pk, &mut reserved),
                _ => None,
            };
            out.insert(pk, effective);
        }
    }
    out
}

/// When a claim weighed by a [`Settlement`] began: an operation the read holds, or a record of
/// the Write a page asks about, by its place among the records the save appends
/// ([`crate::att::Compiled::appended`]). Each record observes every operation the read holds
/// and every record appended before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Start {
    Read(Digest),
    Written(usize),
}

/// Each claimed column's [`Settlement`] in one collection's read, by column name.
pub type Settlements = BTreeMap<&'static str, Settlement>;

/// How one claimed column of a read settled: each row as the read's own settling weighed it
/// (the values its cell holds, whether it is live, the value it claims and when that claim
/// began), and, of the claims to one value, which observed which. A page asks it, before a
/// Write, what each row will read as once the Write lands on this read ([`Self::written`]), and
/// it answers by the read's own settling ([`settle`], [`winner`], [`clash`]) of the rows the
/// Write leaves, so no copy of the rule is kept elsewhere. The Write appends its rows in
/// records, in order, each one operation that observes every operation the read holds and every
/// record before it: a claim a record begins comes after every claim here and every claim an
/// earlier record begins, and the claims one record begins tie on its one digest, so the
/// smaller key keeps the value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settlement {
    rule: ClaimRule,
    rows: BTreeMap<String, Entry<Start>>,
    /// Of the claims to one value, each pair `(later, earlier)` where the later observed the
    /// earlier.
    observed: BTreeSet<(Digest, Digest)>,
}

/// One row of a Write, as [`Settlement::written`] weighs it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Staged {
    /// The value staged in the claimed column, as the save writes it ([`super::grid::read`]):
    /// `Value::Null` for a cleared cell; `None` where the column is not staged in this row.
    pub value: Option<Value>,
    /// Whether the row is live once written, where the Write stages its lifecycle (a retire,
    /// an end, a restore).
    pub live: Option<bool>,
    /// The record of the Write that carries the row's changes, by its place among the records
    /// the save appends ([`crate::att::Compiled::appended`]): a row's changes all go in one.
    pub record: usize,
}

/// What a row that claims a value reads as in its column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    /// The value its cell holds, which it claims.
    pub claims: String,
    /// What it reads as: the value itself where it keeps it, its clash label where another row
    /// keeps it ([`ClaimRule::Label`]), `None` where its claim is withheld.
    pub reads: Option<String>,
    /// The other live rows that claim the same value, by key.
    pub with: Vec<String>,
}

impl Settlement {
    fn of(rule: ClaimRule, rows: BTreeMap<String, Entry<Digest>>, causal: &Causality<'_>) -> Self {
        let mut groups = BTreeMap::<&str, BTreeSet<Digest>>::new();
        for entry in rows.values() {
            if let Standing::Claims(value, Some(starts)) = entry.standing() {
                groups.entry(value).or_default().extend(starts);
            }
        }
        let mut observed = BTreeSet::new();
        for starts in groups.values() {
            for later in starts {
                for earlier in starts {
                    if later != earlier && causal.observes(later, earlier) {
                        observed.insert((*later, *earlier));
                    }
                }
            }
        }
        let rows = rows
            .into_iter()
            .map(|(pk, entry)| {
                let starts = entry
                    .starts
                    .map(|starts| starts.into_iter().map(Start::Read).collect());
                (
                    pk,
                    Entry {
                        held: entry.held,
                        live: entry.live,
                        value: entry.value,
                        conflicted: entry.conflicted,
                        starts,
                        fresh: entry.fresh,
                    },
                )
            })
            .collect();
        Self {
            rule,
            rows,
            observed,
        }
    }

    /// The column's rule.
    pub const fn rule(&self) -> ClaimRule {
        self.rule
    }

    /// What each live row that claims a value reads as in the read, by key.
    pub fn reads(&self) -> BTreeMap<String, Reading> {
        self.readings(&self.rows)
    }

    /// What each live row that claims a value will read as once a Write of `staged` (by row
    /// key) lands on this read, by key. A key the read does not hold is a row the Write is born
    /// in, live from it. A value staged anew begins a tenure at the record of the Write that
    /// carries it ([`Staged::record`]), where the row's activity is known; the value a row holds
    /// staged again keeps its tenure; a row the Write restores claims its value from that
    /// record; a row it retires or ends claims nothing, though the values its cell holds stay
    /// held.
    pub fn written(&self, staged: &BTreeMap<String, Staged>) -> BTreeMap<String, Reading> {
        let mut rows = self.rows.clone();
        for (pk, change) in staged {
            let entry = rows.entry(pk.clone()).or_insert_with(|| Entry {
                held: Vec::new(),
                live: true,
                value: None,
                conflicted: false,
                starts: None,
                fresh: true,
            });
            let restored = change.live == Some(true) && !entry.live;
            if let Some(live) = change.live {
                entry.live = live;
            }
            let begun = Start::Written(change.record);
            if restored {
                entry.fresh = true;
                entry.starts = Some(BTreeSet::from([begun]));
            }
            if let Some(value) = &change.value {
                let value = value.as_str().map(str::to_owned);
                if restored || entry.conflicted || entry.value != value {
                    entry.starts = entry.fresh.then(|| BTreeSet::from([begun]));
                }
                entry.held = value.iter().cloned().collect();
                entry.value = value;
                entry.conflicted = false;
            }
        }
        self.readings(&rows)
    }

    fn readings(&self, rows: &BTreeMap<String, Entry<Start>>) -> BTreeMap<String, Reading> {
        let settled = settle(self.rule, rows, |later, earlier| match (later, earlier) {
            (Start::Written(_), Start::Read(_)) => true,
            (Start::Written(later), Start::Written(earlier)) => later > earlier,
            (Start::Read(later), Start::Read(earlier)) => {
                self.observed.contains(&(*later, *earlier))
            }
            (Start::Read(_), Start::Written(_)) => false,
        });
        let mut claims = BTreeMap::<&str, Vec<&str>>::new();
        for (pk, entry) in rows {
            if let Standing::Claims(value, _) = entry.standing() {
                claims.entry(value).or_default().push(pk);
            }
        }
        let mut out = BTreeMap::new();
        for (value, pks) in &claims {
            for pk in pks {
                out.insert(
                    (*pk).to_owned(),
                    Reading {
                        claims: (*value).to_owned(),
                        reads: settled
                            .get(*pk)
                            .cloned()
                            .unwrap_or_else(|| Some((*value).to_owned())),
                        with: pks
                            .iter()
                            .filter(|other| *other != pk)
                            .map(|other| (*other).to_owned())
                            .collect(),
                    },
                );
            }
        }
        out
    }
}

/// Repeating a value inherits its uninterrupted tenure. Leaving and returning starts anew.
fn tenure(
    coordinate: &Coordinate,
    operation: Digest,
    wanted: &str,
    history: &History,
    causal: &Causality<'_>,
    memo: &mut Tenures,
) -> Option<BTreeSet<Digest>> {
    let key = (coordinate.clone(), operation, wanted.to_owned());
    if let Some(known) = memo.get(&key) {
        return known.clone();
    }
    let found = tenure_inner(coordinate, operation, wanted, history, causal, memo);
    memo.insert(key, found.clone());
    found
}

fn tenure_inner(
    coordinate: &Coordinate,
    operation: Digest,
    wanted: &str,
    history: &History,
    causal: &Causality<'_>,
    memo: &mut Tenures,
) -> Option<BTreeSet<Digest>> {
    let writes = history.get(coordinate)?;
    let carried = |origin: &Digest| match causal.provenance.get(&(coordinate.clone(), *origin))? {
        Provenance::Unknown | Provenance::Invalid => None,
        Provenance::Known(starts) => Some(starts.keys().copied().collect::<BTreeSet<_>>()),
    };
    if !writes.contains_key(&operation) {
        return carried(&operation);
    }
    if writes.len() == 1
        && writes.get(&operation).is_some_and(|value| {
            cell::carried_refs(value).is_none() && matches_value(value, wanted)
        })
        && causal.index.get(&operation).is_some()
    {
        let key = cell::CellKey::parse(&coordinate.1).ok()?;
        let state = causal.activity.at(key.pk, &operation);
        return (state.live
            && state
                .epochs
                .as_ref()
                .is_some_and(|epochs| !epochs.is_empty()))
        .then(|| BTreeSet::from([operation]));
    }
    // Project each coordinate's last writes along native parents. Ordinary operations share
    // their parent's frontier; writes replace it. This visits long same-value runs once.
    let mut frontiers = BTreeMap::<Digest, std::sync::Arc<BTreeSet<Digest>>>::new();
    let empty = std::sync::Arc::new(BTreeSet::new());
    for id in &causal.order {
        let Some(header) = causal.index.get(id) else {
            continue;
        };
        let mut inherited = Vec::new();
        for parent in &header.parents {
            if let Some(frontier) = frontiers.get(parent)
                && !frontier.is_empty()
                && !inherited
                    .iter()
                    .any(|other| std::sync::Arc::ptr_eq(other, frontier))
            {
                inherited.push(frontier.clone());
            }
        }
        let frontier = match inherited.len() {
            0 => empty.clone(),
            1 => inherited.pop().unwrap(),
            _ => {
                let union: BTreeSet<_> = inherited
                    .iter()
                    .flat_map(|frontier| frontier.iter().copied())
                    .collect();
                std::sync::Arc::new(
                    union
                        .iter()
                        .filter(|candidate| {
                            !union.iter().any(|other| {
                                other != *candidate && causal.index.observes(other, candidate)
                            })
                        })
                        .copied()
                        .collect(),
                )
            }
        };
        let Some(value) = writes.get(id) else {
            frontiers.insert(*id, frontier);
            continue;
        };
        let key = (coordinate.clone(), *id, wanted.to_owned());
        let starts = if !matches_value(value, wanted) {
            None
        } else if let Some(origins) = cell::carried_refs(value) {
            let mut starts = Some(BTreeSet::new());
            for (origin, _) in origins {
                let known = memo
                    .get(&(coordinate.clone(), origin, wanted.to_owned()))
                    .cloned()
                    .unwrap_or_else(|| carried(&origin));
                starts = starts.zip(known).map(|(mut all, next)| {
                    all.extend(next);
                    all
                });
            }
            starts
        } else if !causal
            .activity
            .at(cell::CellKey::parse(&coordinate.1).ok()?.pk, id)
            .live
            || causal
                .activity
                .at(cell::CellKey::parse(&coordinate.1).ok()?.pk, id)
                .epochs
                .as_ref()
                .is_none_or(|epochs| epochs.is_empty())
        {
            None
        } else if causal.activity.reentries.get(id).is_some_and(|pks| {
            cell::CellKey::parse(&coordinate.1).is_ok_and(|key| pks.contains(key.pk))
        }) || frontier.is_empty()
            || frontier
                .iter()
                .any(|parent| !matches_value(&writes[parent], wanted))
        {
            Some(BTreeSet::from([*id]))
        } else {
            let mut starts = Some(BTreeSet::new());
            let mut supported = false;
            let state = causal
                .activity
                .at(cell::CellKey::parse(&coordinate.1).ok()?.pk, id);
            let epochs = state.epochs.as_ref().unwrap();
            for parent in frontier.iter() {
                let origins = cell::carried_refs(&writes[parent])
                    .unwrap_or_else(|| vec![(*parent, &writes[parent])]);
                let mut known = Some(BTreeSet::new());
                let mut inherited = false;
                for (origin, _) in origins {
                    match causal.supports(coordinate, &origin, epochs, history) {
                        Some(false) => continue,
                        None => {
                            inherited = true;
                            known = None;
                            break;
                        }
                        Some(true) => inherited = true,
                    }
                    let next = if cell::carried_refs(&writes[parent]).is_some() {
                        carried(&origin)
                    } else {
                        memo.get(&(coordinate.clone(), origin, wanted.to_owned()))
                            .cloned()
                            .flatten()
                    };
                    known = known.zip(next).map(|(mut all, next)| {
                        all.extend(next);
                        all
                    });
                }
                if !inherited {
                    continue;
                }
                supported = true;
                starts = starts.zip(known).map(|(mut all, next)| {
                    all.extend(next);
                    all
                });
            }
            if supported {
                starts
            } else {
                Some(BTreeSet::from([*id]))
            }
        };
        memo.insert(key, starts);
        frontiers.insert(*id, std::sync::Arc::new(BTreeSet::from([*id])));
    }
    memo.get(&(coordinate.clone(), operation, wanted.to_owned()))
        .cloned()
        .flatten()
}

fn winner<S: Ord + Copy>(
    contenders: &[(String, Option<BTreeSet<S>>)],
    observes: impl Fn(&S, &S) -> bool,
) -> Option<String> {
    let mut claims = Vec::new();
    for (pk, starts) in contenders {
        let starts = starts.as_ref()?;
        claims.extend(starts.iter().map(|start| (*start, pk)));
    }
    // Remove causally later claims before the digest tie-break. Pairwise ordering can cycle.
    claims
        .iter()
        .filter(|(id, _)| {
            !claims
                .iter()
                .any(|(other, _)| other != id && observes(id, other))
        })
        .min()
        .map(|(_, pk)| (*pk).clone())
}

fn clash(value: &str, pk: &str, reserved: &mut BTreeSet<String>) -> Option<String> {
    let base = value.split('-').next()?;
    let mut hash = Sha256::new();
    hash.update(b"aterm/typed/label-clash\0");
    hash.update(pk.as_bytes());
    hash.update(value.as_bytes());
    let digest = hash.finalize();
    let initial = u16::from_be_bytes([digest[0], digest[1]]);
    for offset in 0..=u16::MAX {
        let candidate = format!("{base}-{:04x}", initial.wrapping_add(offset));
        if reserved.insert(candidate.clone()) {
            return Some(candidate);
        }
    }
    None
}

fn matches_value(value: &Value, wanted: &str) -> bool {
    if let Some(origins) = cell::carried_refs(value) {
        origins.iter().all(|(_, value)| {
            CellValue::decode(value)
                .ok()
                .is_some_and(|value| value.x.as_str() == Some(wanted))
        })
    } else {
        CellValue::decode(value)
            .ok()
            .is_some_and(|value| value.x.as_str() == Some(wanted))
    }
}

#[cfg(test)]
#[path = "claim_model_tests.rs"]
mod model_tests;
#[cfg(test)]
#[path = "claim_settlement_tests.rs"]
mod settlement_tests;
#[cfg(test)]
#[path = "claim_tests.rs"]
mod tests;
