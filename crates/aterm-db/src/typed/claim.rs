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
    if !operation.revert || change.collection != collection.name {
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
        if !operation.revert {
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

impl Provenance {
    pub(crate) fn decode(value: &Value) -> Option<Self> {
        if value.is_null() {
            return Some(Self::Unknown);
        }
        if value == &Value::Bool(false) {
            return Some(Self::Invalid);
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
                return Some(Self::Unknown);
            }
            if starts == &Value::Bool(false) {
                return Some(Self::Invalid);
            }
            (starts.as_array()?, false, epochs.len())
        };
        if items.is_empty() || items.len() > crate::database::MAX_RECORDS {
            return None;
        }
        let mut facts = items.len().checked_add(epoch_facts)?;
        let mut starts = BTreeMap::new();
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
            let mut past = BTreeSet::new();
            for value in observed {
                let before = digest(value)?;
                if before == start || past.last().is_some_and(|last| *last >= before) {
                    return None;
                }
                past.insert(before);
            }
            starts.insert(start, past);
        }
        Some(if legacy {
            Self::Unknown
        } else {
            Self::Known(starts)
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
        if Self::decode(&value).is_none() {
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
    Provenance::decode(value).is_some() || activity::Proof::decode(value).is_some()
}

fn context_epochs(value: &Value) -> Option<activity::Epochs> {
    Provenance::decode(value)?;
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

fn context<'a>(
    collection: &Collection,
    operations: impl Iterator<Item = impl std::borrow::Borrow<Op>>,
    index: &'a Index,
) -> (History, Causality<'a>) {
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
    let operations: Vec<_> = operations.collect();
    let refs = || operations.iter().map(std::borrow::Borrow::borrow);
    let mut cells = restored_cells(collection, cells, refs());
    let (history, causal) = context(collection, refs(), causal);
    activate_cells(&mut cells, &history, &causal);
    let mut projected = cell::project(collection, &cells);
    let mut tenures = Tenures::new();
    for column in collection.columns {
        let Some(rule) = column.claim else { continue };
        let mut groups = BTreeMap::<String, Vec<(String, Option<BTreeSet<Digest>>)>>::new();
        let mut reserved = BTreeSet::<String>::new();
        for (pk, row) in &mut projected.rows {
            let Some(field) = row.fields.get(column.name) else {
                continue;
            };
            for candidate in field.candidates() {
                if let Some(value) = candidate.value.x.as_str() {
                    reserved.insert(value.to_owned());
                }
            }
            if !row.live() {
                continue;
            }
            if field.conflicted() {
                row.claims.insert(column.name.into(), None);
                continue;
            }
            let Some(value) = field.value().and_then(Value::as_str) else {
                continue;
            };
            reserved.insert(value.to_owned());
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
            groups
                .entry(value.to_owned())
                .or_default()
                .push((pk.clone(), starts));
        }
        for (value, contenders) in groups {
            if contenders.len() < 2
                && contenders[0]
                    .1
                    .as_ref()
                    .is_none_or(|starts| !starts.is_empty())
            {
                continue;
            }
            let winner = winner(&contenders, &causal);
            for (pk, _) in contenders {
                if winner.as_deref() == Some(pk.as_str()) {
                    projected
                        .rows
                        .get_mut(&pk)
                        .unwrap()
                        .claims
                        .insert(column.name.into(), Some(Value::String(value.clone())));
                    continue;
                }
                let effective = match (rule, winner.as_ref()) {
                    (ClaimRule::Label, Some(_)) => {
                        clash(&value, &pk, &mut reserved).map(Value::String)
                    }
                    _ => None,
                };
                projected
                    .rows
                    .get_mut(&pk)
                    .unwrap()
                    .claims
                    .insert(column.name.into(), effective);
            }
        }
    }
    projected
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

fn winner(
    contenders: &[(String, Option<BTreeSet<Digest>>)],
    causal: &Causality<'_>,
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
                .any(|(other, _)| other != id && causal.observes(id, other))
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
#[path = "claim_tests.rs"]
mod tests;
