// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The authority rules (docs/DESIGN-shared-files-2026-11-11.md §5): which writes a cells
//! collection's columns take. Commons has no owner identity and membership is total trust, so
//! they protect against wrong code and wrong Macs, not against a hostile member.
//!
//! A collection opts in from one of its versions ([`Collection::authority`]). Every read of it
//! ([`super::View`], `att::View::read`) then holds inert each write its column's [`Writer`]
//! would not make, as if it had never been written: the history is replayed without that one
//! change (`Log::cells_without`), so its register shows what it held before, while the record
//! keeps its other changes, its author and its causal links. Every reader of the view's cells
//! (the grid and its save, the publisher's settle, a revert, the materializer, the lab view, the
//! registry's guard, a roll) therefore reads the same admitted cells. A collection that does
//! not opt in reads every cell as it was written, and nothing of it is judged.
//!
//! A write is one cell at a collection version `r`, flagged `a` when an automatic path wrote it
//! and `s` when the importer did ([`CellValue`]). From the collection's authority version on (`r`
//! at least that version) a write is inert when:
//!
//! | Rule | The write |
//! | --- | --- |
//! | 2 | is automatic, on an Owner column |
//! | 3a | is automatic, on a Birth column, and a lifecycle write of its row is causally before it |
//! | 3b | is automatic, on a Birth column, and its register also holds a write that is not |
//! | 4 | is on a `fixed` column, by any writer, and a lifecycle write of its row is causally before it |
//!
//! and at every version, the verb rule: a write that is automatic or imported, on a
//! [`Writer::Verb`] column, is inert.
//!
//! - **Lifecycle writes** are the operations of the read (each within its writer's quota) that
//!   gave the row's lifecycle cell a value and that rule 2 leaves standing, ordered by
//!   [`Index::observes`]; a value no typed reader decodes is no lifecycle word. A write is after
//!   its row's birth when a lifecycle write is causally before it and no revert that restored
//!   the cell's absence lies between them: such a revert returns the row to unborn, as the owner's
//!   write check (`check::fixed_after_birth`) reads it, and the row's next lifecycle write is a
//!   birth again, its cells judged as any birth's. A birth writes its cells with its lifecycle in
//!   one record, so no cell of a birth is after it.
//! - **A revert** restores, for each key its target wrote, what this read admitted there: the
//!   register before the target, or, where the target's own write there is inert, the register
//!   the revert observed (`Log::compensate` builds exactly that from the same inert changes). A
//!   revert's cell that restores anything else is inert, whichever build signed it: a build
//!   that judged its target's past by other rules, or by none, can restore a write this read
//!   holds inert. Rules 3a and 4, which judge a write by when it was made, do not judge a revert
//!   beyond that; rules 2, 3b and the verb rule judge it as any other write.
//! - **A roll's carried value** (`cell::carried`) restates candidates of the generation before.
//!   Rule 2 and the verb rule judge each of its items by the item's own `a`, `r` and `s`, and
//!   rule 3b by the item's and its register's, as they judge a fresh write. Rules 3a and 4
//!   cannot: a carried item does not say when it was written against its row's lifecycle. A
//!   roll this build makes carries only what its read admitted, so it carries nothing they
//!   hold; one made by a build from before the rules, of a collection that has opted in, can.
//! - Rule 3b and the revert rule read registers, so they are judged on the projection the other
//!   rules leave, and again on each projection they change, until they hold nothing more inert.
//!
//! The records holding an inert write are counted per collection ([`Inert::records`]): `aterm
//! file ls` and `status`, `aterm db table ls` and `check` show them, and `aterm db alab check`
//! names their rows.

use super::{
    Collection, Column, LIFECYCLE, Table, Writer,
    cell::{self, CellKey, CellValue, Cells},
    owned::Op,
};
use crate::{
    database::{Cell, Change},
    documents::Unwritten,
};
use astream_wire::sync::{Digest, Index};
use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The writes one read holds inert.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inert {
    /// Each change the read projects as if it had never been written: its operation, its
    /// collection and its key.
    pub changes: BTreeSet<(Digest, String, String)>,
    /// The items of a roll's carried value the read leaves out of it, by the change that carries
    /// them: the operations they name.
    pub carried: BTreeMap<(Digest, String, String), BTreeSet<Digest>>,
    /// Per collection, how many records hold at least one inert write.
    pub records: BTreeMap<&'static str, usize>,
}

impl Inert {
    /// How many records hold an inert write, in any collection.
    pub fn total(&self) -> usize {
        self.changes
            .iter()
            .map(|(operation, _, _)| operation)
            .chain(self.carried.keys().map(|(operation, _, _)| operation))
            .collect::<BTreeSet<_>>()
            .len()
    }

    /// The keys of the rows of `collection` that hold an inert write.
    pub fn rows(&self, collection: &str) -> BTreeSet<&str> {
        self.changes
            .iter()
            .chain(self.carried.keys())
            .filter(|(_, name, _)| name == collection)
            .filter_map(|(_, _, key)| CellKey::parse(key).ok().map(|key| key.pk))
            .collect()
    }
}

/// Which rules a read judges: every one, except under a test's control (`seam`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rules {
    /// Rule 2.
    pub(crate) owner: bool,
    /// Rule 3a.
    pub(crate) after_birth: bool,
    /// Rule 3b.
    pub(crate) contested: bool,
    /// Rule 4.
    pub(crate) fixed: bool,
    /// A revert's cell restores what the read admitted.
    pub(crate) restored: bool,
    /// The verb rule.
    pub(crate) verb: bool,
    /// The verb rule judges every version, not only the versions the other rules judge.
    pub(crate) verb_every_version: bool,
}

impl Rules {
    pub(crate) const ALL: Self = Self {
        owner: true,
        after_birth: true,
        contested: true,
        fixed: true,
        restored: true,
        verb: true,
        verb_every_version: true,
    };

    #[cfg(not(test))]
    const fn now() -> Self {
        Self::ALL
    }

    #[cfg(test)]
    fn now() -> Self {
        seam::rules()
    }

    /// Whether rule 2 or the verb rule holds `value`, on `column` of a collection judged from
    /// `from`, inert: what a write's own flags decide, whenever it was made.
    fn by_flags(self, column: &Column, value: &CellValue, from: u32) -> bool {
        match column.writer {
            Writer::Owner => self.owner && value.a && value.r >= from,
            Writer::Verb => {
                self.verb
                    && (value.a || value.s.is_some())
                    && (self.verb_every_version || value.r >= from)
            }
            Writer::Birth | Writer::Mine | Writer::Seed => false,
        }
    }
}

/// `table`'s cells under the authority rules, with the writes they hold inert. `ops` are the
/// read's eligible operations within their writers' quotas, `causal` their ancestry, and
/// `project` replays the table's history without the changes it is given
/// (`Log::cells_without`): all of it, or, where it is given an operation, only what that
/// operation observed. Where no operation in `ops` writes a collection of `table` that opts in,
/// `project` replays it as it was written and nothing is judged: every cell `project` holds
/// comes from a change in `ops`, so such a table holds nothing the rules could hold inert.
pub fn read(
    table: &Table,
    ops: &[Op],
    causal: &Index,
    mut project: impl FnMut(Option<Digest>, &Unwritten) -> Cells,
) -> (Cells, Inert) {
    let judged = |name: &str| {
        table
            .collection(name)
            .is_some_and(|collection| collection.authority.is_some())
    };
    if !ops
        .iter()
        .any(|op| op.changes.iter().any(|change| judged(&change.collection)))
    {
        return (project(None, &Unwritten::new()), Inert::default());
    }
    judge(table, ops, causal, &mut project)
}

/// [`read`] of a table one of whose collections opts in: each write those collections hold,
/// judged by every rule.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedFile",
        action = "OwnerWrites",
        project = "aterm_db::file::model_tests::authority_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedFile",
        action = "AutomaticWrites",
        project = "aterm_db::file::model_tests::authority_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedFile",
        action = "Birth",
        project = "aterm_db::file::model_tests::authority_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedFile",
        action = "OwnerSets",
        project = "aterm_db::file::model_tests::authority_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedFile",
        action = "Rewrite",
        project = "aterm_db::file::model_tests::authority_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedFile",
        action = "OwnerNames",
        project = "aterm_db::file::model_tests::authority_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedFile",
        action = "ImportNames",
        project = "aterm_db::file::model_tests::authority_transition"
    )
)]
fn judge(
    table: &Table,
    ops: &[Op],
    causal: &Index,
    project: &mut dyn FnMut(Option<Digest>, &Unwritten) -> Cells,
) -> (Cells, Inert) {
    let rules = Rules::now();
    let mut inert = Inert::default();
    let judged: Vec<(&'static Collection, u32)> = table
        .collections
        .iter()
        .filter_map(|collection| collection.authority.map(|from| (*collection, from)))
        .collect();
    for &(collection, from) in &judged {
        writes(collection, from, rules, ops, causal, &mut inert);
    }
    let mut cells = project(None, &inert.changes);
    loop {
        let mut more = rules.contested && contested(&judged, &cells, &mut inert);
        more |= rules.restored && restored(&judged, ops, project, &mut inert);
        if !more {
            break;
        }
        cells = project(None, &inert.changes);
    }
    strip_carried(&mut cells, &inert.carried);
    let mut records: BTreeMap<&'static str, BTreeSet<Digest>> = BTreeMap::new();
    for (operation, name, _) in inert.changes.iter().chain(inert.carried.keys()) {
        if let Some(collection) = table.collection(name) {
            records
                .entry(collection.name)
                .or_default()
                .insert(*operation);
        }
    }
    inert.records = records
        .into_iter()
        .map(|(name, operations)| (name, operations.len()))
        .collect();
    (cells, inert)
}

/// A change's stored value, as the rules read it.
enum Written<'a> {
    /// One typed cell.
    Cell(CellValue),
    /// A roll's carried candidates, each with the operation that wrote it.
    Carried(Vec<(Digest, &'a Value)>),
    /// A revert's restored absence, or a value no typed reader decodes: nothing to judge.
    Other,
}

fn written(value: Option<&Value>) -> Written<'_> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Written::Other;
    };
    if let Some(items) = cell::carried_refs(value) {
        return Written::Carried(items);
    }
    CellValue::decode(value).map_or(Written::Other, Written::Cell)
}

/// Rules 2, 3a and 4 and the verb rule over every change of `collection` in `ops`.
fn writes(
    collection: &'static Collection,
    from: u32,
    rules: Rules,
    ops: &[Op],
    causal: &Index,
    inert: &mut Inert,
) {
    // Each row's lifecycle writes. A collection with no lifecycle column, which
    // `Collection::check` refuses, has none.
    let lifecycle = collection
        .column(LIFECYCLE)
        .map(|column| lifecycle_writes(collection, column, from, rules, ops))
        .unwrap_or_default();
    for op in ops {
        for change in op
            .changes
            .iter()
            .filter(|change| change.collection == collection.name)
        {
            let Ok(key) = CellKey::parse(&change.key) else {
                continue;
            };
            let Some(column) = collection.column(key.col) else {
                continue;
            };
            let at = (op.id, change.collection.clone(), change.key.clone());
            match written(change.value.as_ref()) {
                Written::Cell(value) => {
                    let after_birth = || {
                        op.revert.is_none()
                            && value.r >= from
                            && lifecycle
                                .get(key.pk)
                                .is_some_and(|row| row.born_before(op.id, causal))
                    };
                    let automatic_birth =
                        rules.after_birth && value.a && matches!(column.writer, Writer::Birth);
                    let fixed = rules.fixed && column.fixed;
                    if rules.by_flags(column, &value, from)
                        || ((automatic_birth || fixed) && after_birth())
                    {
                        inert.changes.insert(at);
                    }
                }
                Written::Carried(items) => {
                    let left: BTreeSet<Digest> = items
                        .iter()
                        .filter(|(_, item)| {
                            CellValue::decode(item)
                                .is_ok_and(|value| rules.by_flags(column, &value, from))
                        })
                        .map(|(operation, _)| *operation)
                        .collect();
                    if left.len() == items.len() {
                        inert.changes.insert(at);
                    } else if !left.is_empty() {
                        inert.carried.insert(at, left);
                    }
                }
                Written::Other => {}
            }
        }
    }
}

/// One row's lifecycle writes and the restored absences that undo them.
#[derive(Default)]
struct Lifecycle {
    /// Each operation that gave the row's lifecycle a value rule 2 leaves standing.
    births: Vec<Digest>,
    /// Each operation that restored the lifecycle's absence (a revert of the row's birth).
    absences: Vec<Digest>,
}

impl Lifecycle {
    /// Whether operation `op` was written after a birth of this row, as `op` saw the row: a
    /// lifecycle write before it (not `op` itself, since a birth writes its cells with its
    /// lifecycle in one record) that no restored absence before `op` had undone. A row a revert
    /// returned to unborn is born again by its next lifecycle write, as the owner's write check
    /// (`check::fixed_after_birth`) reads it: a lifecycle register that holds no value.
    fn born_before(&self, op: Digest, causal: &Index) -> bool {
        self.births.iter().any(|birth| {
            *birth != op
                && causal.observes(&op, birth)
                && !self.absences.iter().any(|absence| {
                    *absence != op
                        && causal.observes(&op, absence)
                        && causal.observes(absence, birth)
                })
        })
    }
}

/// The lifecycle writes of `ops` (`column`, of `collection`, judged from `from`), per row: each
/// operation that gave it a value rule 2 leaves standing, and each that restored its absence.
fn lifecycle_writes<'a>(
    collection: &Collection,
    column: &Column,
    from: u32,
    rules: Rules,
    ops: &'a [Op],
) -> BTreeMap<&'a str, Lifecycle> {
    let mut lifecycle: BTreeMap<&str, Lifecycle> = BTreeMap::new();
    for op in ops {
        for change in op
            .changes
            .iter()
            .filter(|change| change.collection == collection.name)
        {
            let Ok(key) = CellKey::parse(&change.key) else {
                continue;
            };
            if key.col != LIFECYCLE || key.elem.is_some() {
                continue;
            }
            // A restored absence returns the row to unborn.
            if change.value.as_ref().is_none_or(Value::is_null) {
                lifecycle.entry(key.pk).or_default().absences.push(op.id);
                continue;
            }
            let standing = match written(change.value.as_ref()) {
                Written::Cell(value) => !rules.by_flags(column, &value, from),
                Written::Carried(items) => items.iter().any(|(_, item)| {
                    CellValue::decode(item).is_ok_and(|value| !rules.by_flags(column, &value, from))
                }),
                // An undecodable value is no lifecycle word.
                Written::Other => false,
            };
            if standing {
                lifecycle.entry(key.pk).or_default().births.push(op.id);
            }
        }
    }
    lifecycle
}

/// Rule 3b over `cells`: each automatic write on a Birth column of a judged collection whose
/// register also holds a write that is not automatic, a carried item among them. Whether it
/// held one more change inert.
fn contested(judged: &[(&'static Collection, u32)], cells: &Cells, inert: &mut Inert) -> bool {
    let mut more = false;
    for &(collection, from) in judged {
        for ((name, key), register) in cells
            .range((collection.name.to_owned(), String::new())..)
            .take_while(|((name, _), _)| name == collection.name)
        {
            if register.stopped {
                continue;
            }
            let Some(column) = CellKey::parse(key)
                .ok()
                .and_then(|key| collection.column(key.col))
            else {
                continue;
            };
            if !matches!(column.writer, Writer::Birth) {
                continue;
            }
            let mut stated = false;
            let mut automatic = Vec::new();
            // Per carried value: its items' count, and the automatic ones.
            let mut carried = Vec::new();
            for candidate in &register.candidates {
                match written(Some(&candidate.value)) {
                    Written::Cell(value) => {
                        if !value.a {
                            stated = true;
                        } else if value.r >= from {
                            automatic.push(candidate.operation);
                        }
                    }
                    Written::Carried(items) => {
                        let mut held = BTreeSet::new();
                        for (origin, item) in &items {
                            let Ok(value) = CellValue::decode(item) else {
                                continue;
                            };
                            if !value.a {
                                stated = true;
                            } else if value.r >= from {
                                held.insert(*origin);
                            }
                        }
                        carried.push((candidate.operation, items.len(), held));
                    }
                    // A revert that restored absence is the owner's.
                    Written::Other => stated |= candidate.value.is_null(),
                }
            }
            if !stated {
                continue;
            }
            for operation in automatic {
                more |= inert.changes.insert((operation, name.clone(), key.clone()));
            }
            for (operation, items, held) in carried {
                if held.is_empty() {
                    continue;
                }
                let at = (operation, name.clone(), key.clone());
                let mut left = inert.carried.remove(&at).unwrap_or_default();
                left.extend(held);
                if left.len() == items {
                    more |= inert.changes.insert(at);
                } else {
                    inert.carried.insert(at, left);
                }
            }
        }
    }
    more
}

/// The revert rule over `ops`: each change of a revert, in a judged collection, that does not
/// restore what this read admitted at its key. Where its target's write there stands, that is
/// the register before the target; where it is inert, the register the revert observed, which
/// the target never changed. Whether it held one more change inert.
fn restored(
    judged: &[(&'static Collection, u32)],
    ops: &[Op],
    project: &mut dyn FnMut(Option<Digest>, &Unwritten) -> Cells,
    inert: &mut Inert,
) -> bool {
    let mut more = false;
    for op in ops {
        let Some(target) = op.revert else {
            continue;
        };
        let changes: Vec<&Change> = op
            .changes
            .iter()
            .filter(|change| {
                judged
                    .iter()
                    .any(|(collection, _)| collection.name == change.collection)
                    && !inert.changes.contains(&(
                        op.id,
                        change.collection.clone(),
                        change.key.clone(),
                    ))
            })
            .collect();
        if changes.is_empty() {
            continue;
        }
        let before_target = project(Some(target), &inert.changes);
        let mut observed = None;
        let mut held = Vec::new();
        for change in changes {
            let key = (change.collection.clone(), change.key.clone());
            let register = if inert
                .changes
                .contains(&(target, key.0.clone(), key.1.clone()))
            {
                observed
                    .get_or_insert_with(|| project(Some(op.id), &inert.changes))
                    .get(&key)
            } else {
                before_target.get(&key)
            };
            if !restates(register, change.value.as_ref()) {
                held.push((op.id, key.0, key.1));
            }
        }
        for at in held {
            more |= inert.changes.insert(at);
        }
    }
    more
}

/// Whether `value` is what a compensation that read `register` writes: its one candidate's value,
/// or absence where it holds none. A stopped or conflicted register has no such value.
fn restates(register: Option<&Cell>, value: Option<&Value>) -> bool {
    let value = value.filter(|value| !value.is_null());
    match register {
        None => value.is_none(),
        Some(cell) if cell.stopped || cell.candidates.len() > 1 => false,
        Some(cell) => {
            cell.candidates
                .last()
                .map(|candidate| &candidate.value)
                .filter(|value| !value.is_null())
                == value
        }
    }
}

/// Leave out of each carried value in `cells` the items `carried` names.
fn strip_carried(
    cells: &mut Cells,
    carried: &BTreeMap<(Digest, String, String), BTreeSet<Digest>>,
) {
    for ((operation, collection, key), items) in carried {
        let Some(register) = cells.get_mut(&(collection.clone(), key.clone())) else {
            continue;
        };
        for candidate in register
            .candidates
            .iter_mut()
            .filter(|candidate| candidate.operation == *operation)
        {
            if let Some(Value::Array(listed)) = candidate
                .value
                .as_object_mut()
                .and_then(|object| object.get_mut("k"))
            {
                listed.retain(|item| {
                    let mut origin = [0; 32];
                    !item.get("o").and_then(Value::as_str).is_some_and(|hex| {
                        aterm_codec::hex::decode_into(hex.as_bytes(), &mut origin)
                            && items.contains(&origin)
                    })
                });
            }
        }
    }
}

/// The controls of this crate's tests: each turns one rule off on the thread that reads.
#[cfg(test)]
#[path = "authority_seam_tests.rs"]
pub(crate) mod seam;

#[cfg(test)]
#[path = "authority_tests.rs"]
pub(crate) mod tests;
