// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reference and unique rules and findings (design §6). A table is one causal domain, so no foreign
//! key or unique constraint is atomic against a concurrent writer and nothing cascades: every write
//! this Mac authors, on any path, is refused when, with the write landed on its own view, a
//! reference it makes names a row that view does not hold, two live rows come to share a declared
//! unique set, a row live rows reference is retired, or a live row comes to name a row that is not
//! live ([`refused`]); and every reader re-checks the converged rows and reports what it finds
//! ([`findings`]), what concurrent writes left among them. A finding or refusal names collections,
//! keys and columns, never a value, so an address never reaches a report.

use super::{
    Collection, Column, Kind, Refused, Table, Ty, View,
    cell::{CellKey, CellValue, Cells, Field, Projection, Row},
    keys,
    owned::OwnedRead,
};
use crate::database::Change;
use std::collections::BTreeMap;

/// Something a reader reports for the owner.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Finding {
    /// A register holds different concurrent values.
    Conflict {
        collection: String,
        pk: String,
        column: String,
    },
    /// A reference names a key its collection does not hold.
    Dangling {
        collection: String,
        pk: String,
        column: String,
        target: String,
    },
    /// Live rows share the values of a unique column set.
    Unique {
        collection: String,
        columns: Vec<String>,
        pks: Vec<String>,
    },
    /// A value this reader cannot interpret: a newer build's column or word.
    Raw {
        collection: String,
        pk: String,
        column: String,
    },
    /// A row whose `protected` cell is true holds a secret, sealed in its cell or named in the
    /// secret store ([`protected`]): a concurrent write or an older build put it there; no table
    /// view shows it and the owner clears it.
    Protected {
        collection: String,
        pk: String,
        column: String,
    },
}

impl Finding {
    /// Its word, as `check` reports it.
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Conflict { .. } => "conflict",
            Self::Dangling { .. } => "dangling",
            Self::Unique { .. } => "unique",
            Self::Raw { .. } => "raw",
            Self::Protected { .. } => "protected-secret",
        }
    }
}

/// What a finding of `kind` means, in a person's words, for each word `aterm db table check`
/// reports of any table: [`Finding::kind`]'s, a schema split, and what the table's capacity asks
/// (`quota-spent`, `roll`, `generation-unopened`). `None` for a word this layer does not report:
/// a lab rule's sentence is beside its rule ([`crate::alab::checks::sentence`]), and [`said`]
/// asks both. A sentence names no value, as a finding names none.
pub fn sentence(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "conflict" => "Two or more values were written to this cell at once; keep one to settle it",
        "dangling" => "A reference names a row its table does not hold",
        "unique" => "Live rows hold one value where only one row may",
        "raw" => "A value a newer build of aterm wrote, which this build cannot read",
        "protected-secret" => {
            "A protected row holds a secret: no table shows it, and the owner clears it"
        }
        "schema-split" => "Machines state different versions of this table's columns",
        "quota-spent" => {
            "A machine's automatic records reached the table's quota: what it states or adds past \
             it counts once the table rolls"
        }
        "roll" => "The table should roll to its next generation",
        "generation-unopened" => {
            "A later generation holds secret cells that do not open under its key"
        }
        _ => return None,
    })
}

/// What a person reads for a finding of `kind`: its sentence ([`sentence`], else the lab's
/// [`crate::alab::checks::sentence`]), else the word itself, so a kind no sentence is written
/// for is still said. The `--tsv` form of `aterm db table check` and `aterm db alab check` and
/// the Tables page's Check view each say a finding through this one function.
pub fn said(kind: &str) -> &str {
    sentence(kind)
        .or_else(|| crate::alab::checks::sentence(kind))
        .unwrap_or(kind)
}

/// A finding of `kind`'s heading, a few words in sentence case, for each word [`sentence`] says;
/// `None` for any other word (a lab rule's heading is beside its sentence,
/// [`crate::alab::checks::heading`]).
pub fn heading(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "conflict" => "Values written at once",
        "dangling" => "Reference to no row",
        "unique" => "Value held twice",
        "raw" => "Value from a newer build",
        "protected-secret" => "Secret in a protected row",
        "schema-split" => "Columns differ between machines",
        "quota-spent" => "Quota spent",
        "roll" => "Roll due",
        "generation-unopened" => "Generation does not open",
        _ => return None,
    })
}

/// What a page heads a finding of `kind` with: its heading ([`heading`], else the lab's
/// [`crate::alab::checks::heading`]), else its word with each dash a space and its first letter
/// a capital (`schema-split` would read `Schema split`), so a kind no heading is written for still
/// reads as words.
pub fn headed(kind: &str) -> String {
    if let Some(heading) = heading(kind).or_else(|| crate::alab::checks::heading(kind)) {
        return heading.to_owned();
    }
    let words = kind.replace('-', " ");
    let mut chars = words.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// Whether a row is protected (design §4.7, "protection is data, not source"): any candidate
/// of its `protected` cell says `true`, so a concurrent unprotect never uncovers a secret.
pub fn protected(row: &super::cell::Row) -> bool {
    use super::cell::Field;
    match row.fields.get("protected") {
        Some(Field::Value(candidate)) => candidate.value.x == aterm_json::Value::Bool(true),
        Some(Field::Conflict { candidates, .. }) => candidates
            .iter()
            .any(|c| c.value.x == aterm_json::Value::Bool(true)),
        _ => false,
    }
}

fn refs<'a>(ty: &Ty, value: &'a aterm_json::Value, out: &mut Vec<(&'static str, &'a str)>) {
    match ty {
        Ty::Ref(target) => {
            if let Some(pk) = value.as_str() {
                out.push((*target, pk));
            }
        }
        // A list, and a set an owned or log row states whole.
        Ty::List(inner, _) | Ty::Set(inner) => {
            for item in value.as_array().into_iter().flatten() {
                refs(inner, item, out);
            }
        }
        Ty::Struct(members) => {
            for member in *members {
                if let Some(field) = value.get(member.name) {
                    refs(&member.ty, field, out);
                }
            }
        }
        _ => {}
    }
}

/// Every reference a projected cells `field` of `column` holds: each present element of a
/// `Set(Ref)`, and each candidate of a register, a conflict's included.
fn field_refs<'a>(column: &Column, field: &'a Field, out: &mut Vec<(&'static str, &'a str)>) {
    match (column.ty, field) {
        (Ty::Set(Ty::Ref(target)), Field::Set(elements)) => {
            for (elem, f) in elements {
                if f.present() {
                    out.push((*target, elem.as_str()));
                }
            }
        }
        (Ty::Set(Ty::Ref(_)), _) => {}
        (ty, Field::Conflict { candidates, .. }) => {
            for candidate in candidates {
                refs(&ty, &candidate.value.x, out);
            }
        }
        (ty, _) => {
            if let Some(value) = field.value() {
                refs(&ty, value, out);
            }
        }
    }
}

/// One column's value as a unique set compares it: its one unconflicted, non-null value.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum UniqueVal<'a> {
    Str(&'a str),
    Json(String),
}

fn unique_val<'a>(row: &'a Row, column: &str) -> Option<UniqueVal<'a>> {
    let value = row
        .fields
        .get(column)
        .filter(|field| !field.conflicted())
        .and_then(|_| row.get(column))
        .filter(|value| !value.is_null())?;
    match value {
        aterm_json::Value::String(s) => Some(UniqueVal::Str(s.as_str())),
        other => aterm_json::to_string(other).ok().map(UniqueVal::Json),
    }
}

/// A live row's values of the unique column set `unique`, when it holds every one of them.
fn unique_key<'a>(row: &'a Row, unique: &[&str]) -> Option<Vec<UniqueVal<'a>>> {
    if !row.live() {
        return None;
    }
    unique
        .iter()
        .map(|column| unique_val(row, column))
        .collect()
}

/// Whether `collection` holds a row keyed `pk` once `rows` land on `cells`: a cell of that row
/// already held ([`super::grid::holds_row`], a retired or ended row included, as [`findings`]
/// reads it) or one `rows` write.
fn holds(collection: &Collection, cells: &Cells, rows: &[Vec<Change>], pk: &str) -> bool {
    super::grid::holds_row(collection, cells, pk)
        || rows.iter().flatten().any(|change| {
            change.collection == collection.name
                && change.value.is_some()
                && CellKey::parse(&change.key).is_ok_and(|key| key.pk == pk)
        })
}

/// `collection`'s rows as they read once `rows` land on `cells`: each cell the write changes
/// holds that write alone, since a write this Mac authors follows every candidate its view
/// holds and replaces them, and a restored absence reads as empty.
fn landed(collection: &Collection, cells: &Cells, rows: &[Vec<Change>]) -> Projection {
    let mut mine: Cells = cells
        .range((collection.name.to_owned(), String::new())..)
        .take_while(|((name, _), _)| name == collection.name)
        .map(|(key, cell)| (key.clone(), cell.clone()))
        .collect();
    for change in rows
        .iter()
        .flatten()
        .filter(|change| change.collection == collection.name)
    {
        mine.insert(
            (change.collection.clone(), change.key.clone()),
            crate::database::Cell {
                stopped: false,
                candidates: vec![crate::database::Candidate {
                    operation: [0; 32],
                    value: change.value.clone().unwrap_or(aterm_json::Value::Null),
                }],
            },
        );
    }
    super::cell::project(collection, &mine)
}

/// The rules every write this Mac authors into `table` passes, whatever path makes it (a grid
/// save, an owner verb, a revert, the importer, `adopt`, `prefer`, a seed, registration or
/// observation pass), judged over `rows` (one record each) against `view`, the table as the
/// writer reads it: a reference the write makes names a row the table holds ([`missing_row`]),
/// no two live rows come to share a declared unique set ([`unique_clash`]), no row live rows
/// reference is retired or ended ([`retired_referenced`]), and no live row comes to name a row
/// that is not live ([`names_not_live`]), the same rule from the referrer's side. Answers the
/// first refusal.
///
/// Only what the write itself makes is judged: a clash, a dangling reference or a reference to
/// an ended row that concurrent writes left in rows or columns the write leaves as they are
/// never holds it back, and [`findings`] still reports it. A peer's records arriving by sync are
/// never judged here, since refusing what another replica already holds would split the
/// replicas; nor is a roll's restatement, which carries what the sealed generation's Macs
/// already wrote.
pub fn refused(
    table: &Table,
    rows: &[Vec<Change>],
    view: &View,
    seeded: fn(&str, &str) -> bool,
) -> Option<Refused> {
    missing_row(table, rows, &view.cells, &view.owned, seeded)
        .or_else(|| unique_clash(table, rows, &view.cells))
        .or_else(|| retired_referenced(table, rows, &view.cells))
        .or_else(|| names_not_live(table, rows, &view.cells))
}

/// Every reference `value`, a value of type `ty`, holds: a `Ref`, each `Ref` of a list or a set
/// an owned or log row states whole, and each inside a struct, as `(target collection, key)`.
pub fn references<'a>(ty: &Ty, value: &'a aterm_json::Value) -> Vec<(&'static str, &'a str)> {
    let mut out = Vec::new();
    refs(ty, value, &mut out);
    out
}

/// The first reference a write's `rows` (one record each) make into `table` that names a key
/// its target cells collection would not hold once they land on `cells`, the table as the
/// writer reads it: a `Ref` cell's value, a `Ref` inside a list or struct, a `Set(Ref)` element
/// the write adds, or a reference an owned or log row states. A row the same write makes may be
/// named. Only what `rows` write is judged, so a reference already dangling in a column the
/// write leaves alone, which a concurrent write may have made so, never holds the write back
/// ([`findings`] still reports it): a cells column the write does not change, and a column of an
/// owned row that states what this writer's row in `owned` already holds there. A row `seeded`
/// says this build compiles (a seed a pass has not written here yet) may be named
/// too; [`findings`] reports the reference until a pass writes it. A reference into a collection
/// that is not a cells collection of `table` is not judged, as [`findings`] does not judge one.
pub fn missing_row(
    table: &Table,
    rows: &[Vec<Change>],
    cells: &Cells,
    owned: &BTreeMap<&str, OwnedRead>,
    seeded: fn(&str, &str) -> bool,
) -> Option<Refused> {
    let missing = |target: &str, pk: &str| {
        table
            .collection(target)
            .filter(|c| c.kind == Kind::Cells)
            .is_some_and(|target| !holds(target, cells, rows, pk) && !seeded(target.name, pk))
    };
    let refuse = |collection: &'static str, pk: &str, column: &str, target: &str| Refused {
        collection,
        pk: pk.to_owned(),
        column: column.to_owned(),
        rule: format!("no such {target} row"),
    };
    for change in rows.iter().flatten() {
        let Some(schema) = table.collection(&change.collection) else {
            continue;
        };
        let Some(value) = &change.value else {
            continue;
        };
        let mut targets = Vec::new();
        if schema.kind != Kind::Cells {
            // An owned or log row is one whole value, its columns its members.
            let held = owned
                .get(schema.name)
                .and_then(|read| read.rows.get(&change.key))
                .filter(|_| schema.kind == Kind::Owned);
            for column in schema.columns {
                let Some(x) = value.get(column.name) else {
                    continue;
                };
                if held.is_some_and(|row| row.value.get(column.name) == Some(x)) {
                    continue;
                }
                targets.clear();
                refs(&column.ty, x, &mut targets);
                if let Some(&(target, _)) = targets.iter().find(|(t, pk)| missing(t, pk)) {
                    return Some(refuse(schema.name, &change.key, column.name, target));
                }
            }
            continue;
        }
        let (Ok(key), Ok(value)) = (CellKey::parse(&change.key), CellValue::decode(value)) else {
            continue;
        };
        let Some(column) = schema.column(key.col) else {
            continue;
        };
        targets.clear();
        match (column.ty, key.elem) {
            (Ty::Set(Ty::Ref(target)), Some(elem)) => {
                if value.x.as_bool() == Some(true) {
                    targets.push((*target, elem));
                }
            }
            (_, Some(_)) => {}
            (ty, None) => refs(&ty, &value.x, &mut targets),
        }
        if let Some(&(target, _)) = targets.iter().find(|(t, pk)| missing(t, pk)) {
            return Some(refuse(schema.name, key.pk, key.col, target));
        }
    }
    None
}

/// The first row a write's `rows` write into a cells collection of `table` that, once they land
/// on `cells`, is live and shares every value of one of its collection's unique sets with
/// another live row that did not hold those same values with it before. Two rows a concurrent
/// write already left sharing one stay as they are: a write that leaves them so is not refused
/// (one that moves both onto other shared values is), and
/// [`findings`] reports them. The refusal names the other row's key and the set's columns,
/// never a value.
pub fn unique_clash(table: &Table, rows: &[Vec<Change>], cells: &Cells) -> Option<Refused> {
    for schema in table
        .collections
        .iter()
        .filter(|c| c.kind == Kind::Cells && !c.unique.is_empty())
    {
        let written: BTreeMap<&str, Vec<&str>> = rows
            .iter()
            .flatten()
            .filter(|change| change.collection == schema.name)
            .filter_map(|change| CellKey::parse(&change.key).ok())
            .fold(BTreeMap::new(), |mut written, key| {
                written.entry(key.pk).or_default().push(key.col);
                written
            });
        if written.is_empty() {
            continue;
        }
        let before = super::cell::project(schema, cells);
        let after = landed(schema, cells, rows);
        for unique in schema.unique {
            for (pk, columns) in &written {
                let Some(mine) = after.rows.get(*pk).and_then(|row| unique_key(row, unique)) else {
                    continue;
                };
                // The pair shared these very values before: a pair that shared others and that
                // one write moves together onto these makes a clash of its own.
                let shared_before = |other: &str| {
                    let was =
                        |pk: &str| before.rows.get(pk).and_then(|row| unique_key(row, unique));
                    was(pk).as_ref() == Some(&mine) && was(other).as_ref() == Some(&mine)
                };
                let Some(other) = after.rows.iter().find_map(|(other, row)| {
                    (other != pk
                        && unique_key(row, unique).as_ref() == Some(&mine)
                        && !shared_before(other))
                    .then_some(other)
                }) else {
                    continue;
                };
                let column = unique
                    .iter()
                    .find(|column| columns.contains(column))
                    .unwrap_or(&unique[0]);
                return Some(Refused {
                    collection: schema.name,
                    pk: (*pk).to_owned(),
                    column: (*column).to_owned(),
                    rule: format!(
                        "live row {} holds the same {}; only one live row may",
                        keys::safe_key(other),
                        unique.join(", ")
                    ),
                });
            }
        }
    }
    None
}

/// The first row of a cells collection of `table` that a write's `rows` make no longer live (its
/// lifecycle `retired`, `ended`, `merged` …) while, once they land on `cells`, a live cells row
/// other than itself references it. An owned row is a Mac's statement about itself, with no
/// lifecycle of its own (a retired Mac's facts row names its retired machine row), and does not
/// count. The refusal names the referencing collection and row, never a value.
pub fn retired_referenced(table: &Table, rows: &[Vec<Change>], cells: &Cells) -> Option<Refused> {
    let mut ended: Vec<(&'static Collection, &str)> = Vec::new();
    for change in rows.iter().flatten() {
        let Some(schema) = table
            .collection(&change.collection)
            .filter(|c| c.kind == Kind::Cells)
        else {
            continue;
        };
        if let Ok(key) = CellKey::parse(&change.key)
            && key.col == super::LIFECYCLE
            && key.elem.is_none()
            && !ended.contains(&(schema, key.pk))
        {
            ended.push((schema, key.pk));
        }
    }
    if ended.is_empty() {
        return None;
    }
    let mut ending: Vec<(&'static Collection, &str)> = Vec::new();
    for (schema, pk) in ended {
        let before = super::cell::project(schema, cells);
        let after = landed(schema, cells, rows);
        if before.rows.get(pk).is_some_and(Row::live)
            && after.rows.get(pk).is_some_and(|row| !row.live())
        {
            ending.push((schema, pk));
        }
    }
    if ending.is_empty() {
        return None;
    }
    let (schema, pk, referring, rpk) =
        naming(table, rows, cells).find_map(|(referring, rpk, named)| {
            named.iter().find_map(|(target, key)| {
                ending
                    .iter()
                    .find(|(schema, pk)| schema.name == *target && *pk == key.as_str())
                    .map(|&(schema, pk)| (schema, pk, referring, rpk.clone()))
            })
        })?;
    Some(Refused {
        collection: schema.name,
        pk: pk.to_owned(),
        column: super::LIFECYCLE.to_owned(),
        rule: format!(
            "live {referring} row {} names this row; end that row or point it elsewhere first",
            keys::safe_key(&rpk)
        ),
    })
}

/// Every row of a cells collection of `table` that live cells rows other than itself name once
/// `rows` land on `cells`, with the naming rows (collection and key) in the table's order: the
/// rows [`retired_referenced`] refuses a row's retirement over, read by the same rule. The
/// Tables page names them before it stages a retire.
pub fn referrers(
    table: &Table,
    rows: &[Vec<Change>],
    cells: &Cells,
) -> BTreeMap<(&'static str, String), Vec<(&'static str, String)>> {
    let mut found: BTreeMap<(&'static str, String), Vec<(&'static str, String)>> = BTreeMap::new();
    for (referring, rpk, named) in naming(table, rows, cells) {
        for target in named {
            let by = found.entry(target).or_default();
            if !by.iter().any(|(c, k)| *c == referring && *k == rpk) {
                by.push((referring, rpk.clone()));
            }
        }
    }
    found
}

/// Each live row of a cells collection of `table` once `rows` land on `cells` (collection and
/// key), with every row it names other than itself (collection and key), by its columns' order:
/// in the table's order of collections, then each collection's keys. An owned row names nothing
/// here, as [`retired_referenced`] says.
fn naming<'a>(
    table: &'a Table,
    rows: &'a [Vec<Change>],
    cells: &'a Cells,
) -> impl Iterator<Item = (&'static str, String, Vec<(&'static str, String)>)> + 'a {
    table
        .collections
        .iter()
        .filter(|c| c.kind == Kind::Cells)
        .flat_map(move |referring| {
            let after = landed(referring, cells, rows);
            let mut found = Vec::new();
            let mut targets = Vec::new();
            for (rpk, row) in after.rows.iter().filter(|(_, row)| row.live()) {
                let mut named: Vec<(&'static str, String)> = Vec::new();
                for (name, field) in &row.fields {
                    let Some(column) = referring.column(name) else {
                        continue;
                    };
                    targets.clear();
                    field_refs(column, field, &mut targets);
                    for &(target, key) in &targets {
                        let itself = target == referring.name && key == rpk;
                        if !itself && !named.iter().any(|(t, k)| *t == target && k == key) {
                            named.push((target, key.to_owned()));
                        }
                    }
                }
                if !named.is_empty() {
                    found.push((referring.name, rpk.clone(), named));
                }
            }
            found
        })
}

/// The first reference a write's `rows` make from a live cells row of `table` to a row of a
/// cells collection that, once they land on `cells`, is held and not live (retired, ended,
/// merged, or its lifecycle conflicted): [`retired_referenced`] from the referrer's side. A
/// referrer the write makes live (a birth, or a revival of an ended or conflicted row) makes
/// every reference it holds then, so each is judged; a referrer live before makes only those its
/// written cells hold that they did not hold before: a value a written cell takes in place of
/// another (a candidate of a conflicted cell it settles is no new reference), and a member of a
/// `Set(Ref)` the write makes present. A write that restates a reference the row already holds
/// adds none and is not refused for it, as an unrelated write is not. A target
/// the table does not hold is [`missing_row`]'s. An owned or log row is a Mac's statement about
/// itself with no lifecycle of its own and is not judged, as [`retired_referenced`] does not
/// count one. The refusal names the referrer's collection, row and column, and the target's
/// collection and key, never a value.
pub fn names_not_live(table: &Table, rows: &[Vec<Change>], cells: &Cells) -> Option<Refused> {
    // Each cells row the write touches, with the columns (and set members) it writes.
    let mut written: BTreeMap<(&'static str, &str), Vec<(&str, Option<&str>)>> = BTreeMap::new();
    for change in rows.iter().flatten() {
        let Some(schema) = table
            .collection(&change.collection)
            .filter(|c| c.kind == Kind::Cells)
        else {
            continue;
        };
        if let Ok(key) = CellKey::parse(&change.key) {
            written
                .entry((schema.name, key.pk))
                .or_default()
                .push((key.col, key.elem));
        }
    }
    // The references the write makes, as (referrer collection, row, column, target, key).
    let mut made: Vec<(&'static str, &str, &'static str, &'static str, String)> = Vec::new();
    let mut referrer = None::<(&'static str, Projection, Projection)>;
    for (&(name, pk), columns) in &written {
        let Some(schema) = table.collection(name) else {
            continue;
        };
        if referrer.as_ref().is_none_or(|(held, ..)| *held != name) {
            referrer = Some((
                name,
                super::cell::project(schema, cells),
                landed(schema, cells, rows),
            ));
        }
        let Some((_, before, after)) = referrer.as_ref() else {
            continue;
        };
        let Some(row) = after.rows.get(pk).filter(|row| row.live()) else {
            continue;
        };
        let held = before.rows.get(pk).filter(|row| row.live());
        for (column_name, field) in &row.fields {
            let Some(column) = schema.column(column_name) else {
                continue;
            };
            let mut buf = Vec::new();
            field_refs(column, field, &mut buf);
            // A live referrer: only a reference its written cells hold that they did not before
            // (a member the write makes present, a value it puts in place of another).
            if let Some(held) = held {
                let set = matches!(column.ty, Ty::Set(Ty::Ref(_)));
                let mut had = Vec::new();
                if let Some(field) = held.fields.get(column_name) {
                    field_refs(column, field, &mut had);
                }
                buf.retain(|&(target, key)| {
                    let cell = set.then_some(key);
                    columns
                        .iter()
                        .any(|&(col, elem)| col == column.name && elem == cell)
                        && !had.contains(&(target, key))
                });
            }
            made.extend(
                buf.iter()
                    .filter(|&&(target, key)| !(target == name && key == pk))
                    .map(|&(target, key)| (name, pk, column.name, target, key.to_owned())),
            );
        }
    }
    let mut targets: BTreeMap<&'static str, Projection> = BTreeMap::new();
    for (collection, pk, column, target, key) in made {
        let Some(schema) = table.collection(target).filter(|c| c.kind == Kind::Cells) else {
            continue;
        };
        let landed = targets
            .entry(schema.name)
            .or_insert_with(|| landed(schema, cells, rows));
        if landed.rows.get(&key).is_some_and(|row| !row.live()) {
            return Some(Refused {
                collection,
                pk: pk.to_owned(),
                column: column.to_owned(),
                rule: format!(
                    "names {target} row {}, which is not live; point it at a live row, leave it \
                     empty, or make that row live first",
                    keys::safe_key(&key)
                ),
            });
        }
    }
    None
}

/// Every finding over the projected `tables`, keyed by collection name, against their compiled
/// `schemas`. A reference into a collection that is not among `tables` is not checked.
pub fn findings(schemas: &[&Collection], tables: &BTreeMap<&str, &Projection>) -> Vec<Finding> {
    let mut found = Vec::new();
    let mut targets = Vec::new();
    for schema in schemas {
        let Some(projection) = tables.get(schema.name) else {
            continue;
        };
        for (pk, row) in &projection.rows {
            if protected(row) {
                for column in schema
                    .columns
                    .iter()
                    .filter(|c| matches!(c.ty, Ty::SecretRef | Ty::Secret))
                {
                    if row
                        .fields
                        .get(column.name)
                        .is_some_and(|field| match field {
                            super::cell::Field::Value(c) => !c.value.x.is_null(),
                            _ => true,
                        })
                    {
                        found.push(Finding::Protected {
                            collection: schema.name.to_owned(),
                            pk: pk.clone(),
                            column: column.name.to_owned(),
                        });
                    }
                }
            }
            for column in &row.raw {
                found.push(Finding::Raw {
                    collection: schema.name.to_owned(),
                    pk: pk.clone(),
                    column: column.clone(),
                });
            }
            for (name, field) in &row.fields {
                let conflicted = match field {
                    super::cell::Field::Set(elements) => elements.values().any(|e| e.conflicted()),
                    other => other.conflicted(),
                };
                if conflicted || row.claims.get(name).is_some_and(Option::is_none) {
                    found.push(Finding::Conflict {
                        collection: schema.name.to_owned(),
                        pk: pk.clone(),
                        column: name.clone(),
                    });
                }
                let Some(column) = schema.column(name) else {
                    continue;
                };
                targets.clear();
                field_refs(column, field, &mut targets);
                for &(target, key) in &targets {
                    if let Some(other) = tables.get(target)
                        && !other.rows.contains_key(key)
                    {
                        found.push(Finding::Dangling {
                            collection: schema.name.to_owned(),
                            pk: pk.clone(),
                            column: name.clone(),
                            target: target.to_owned(),
                        });
                    }
                }
            }
        }
        for unique in schema.unique {
            if let [column] = *unique {
                let mut groups = BTreeMap::<UniqueVal<'_>, Vec<&str>>::new();
                for (pk, row) in projection.rows.iter().filter(|(_, row)| row.live()) {
                    if let Some(val) = unique_val(row, column) {
                        groups.entry(val).or_default().push(pk.as_str());
                    }
                }
                for pks in groups.into_values().filter(|pks| pks.len() > 1) {
                    found.push(Finding::Unique {
                        collection: schema.name.to_owned(),
                        columns: vec![(*column).to_owned()],
                        pks: pks.into_iter().map(str::to_owned).collect(),
                    });
                }
            } else {
                let mut groups = BTreeMap::<Vec<UniqueVal<'_>>, Vec<&str>>::new();
                for (pk, row) in projection.rows.iter().filter(|(_, row)| row.live()) {
                    let values: Option<Vec<UniqueVal<'_>>> = unique
                        .iter()
                        .map(|column| unique_val(row, column))
                        .collect();
                    if let Some(values) = values {
                        groups.entry(values).or_default().push(pk.as_str());
                    }
                }
                for pks in groups.into_values().filter(|pks| pks.len() > 1) {
                    found.push(Finding::Unique {
                        collection: schema.name.to_owned(),
                        columns: unique.iter().map(|c| (*c).to_owned()).collect(),
                        pks: pks.into_iter().map(str::to_owned).collect(),
                    });
                }
            }
        }
    }
    found.sort_unstable();
    found.dedup();
    found
}

#[cfg(test)]
#[path = "check_tests.rs"]
mod tests;
