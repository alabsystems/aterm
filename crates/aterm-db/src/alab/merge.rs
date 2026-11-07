// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm db alab merge`: fold each duplicate machine row into the one row its Mac is bound to,
//! with the registry's own `merged_into`. Rows that share a name, as a label or an alias, are one
//! Mac when exactly one of them holds a node cell: that row stays, and the others end up
//! `row=merged` pointing at it.
//!
//! The row that stays takes the Mac's current name as its label: the roster's name among the
//! rows' names, since the roster is what a Mac's membership goes by; with none, its own label
//! stays, and every other name is kept as an alias. A cell only a duplicate holds moves with its
//! provenance, an owner's cell moves over an import's, and a cell both hold differently stays as
//! the node-bound row has it, its column listed. What the owner cleared or removed on the row that
//! stays stands, and so does a cell of a newer build's it cannot read. The row answers every host
//! its duplicates answered, so a Mac that would have adopted a duplicate by host adopts it, and it
//! stays `planned` where a duplicate waited for a Mac by name, the one way `register::pick` adopts by
//! name. A duplicate's live preference moves to the row that stays and ends where it was.
//!
//! Anything else is held and listed: two nodes, or a node the roster states of the name against
//! the one the row holds, or of another name against it, are two Macs; no node gives no row to keep;
//! a duplicate a Mac's own facts row is bound to moves only by that Mac; two rows stating different
//! words of `status` are not plainly one Mac; a cell the merge decides by that this build cannot read
//! is the newer build's to decide; and a conflicted cell is settled first. [`plan`] is pure over what
//! the registry holds, and a plan over its own result is empty. A report holds keys, labels and
//! column names, never a cell's value.

use super::{
    register::same_host,
    registry::{MACHINE, PREFERENCE},
};
use crate::{
    Result,
    database::Change,
    typed::{
        LIFECYCLE,
        cell::{self, CellKey, CellValue, Field, Projection, Row},
        keys,
    },
};
use aterm_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The machine columns a merge writes by its own rules, or that say which row this is.
const NOT_CARRIED: [&str; 7] = [
    "label",
    "aliases",
    "hosts",
    "node",
    "fleet_node",
    "merged_into",
    LIFECYCLE,
];
/// The hosts a row can hold (`hosts`' column).
const MAX_HOSTS: usize = 4;
/// What binds a row to one Mac: the principal it publishes itself, and the older transport id a
/// replica states. A row holding either is that Mac's; two rows holding them are not one to fold.
const NODE_CELLS: [&str; 2] = ["node", "fleet_node"];
/// The preference columns that say which row this is.
const PREFERENCE_IDENTITY: [&str; 3] = ["machine", "account", LIFECYCLE];
/// Provenance moves with a value: another row's coordinates are not a difference worth listing.
const PROVENANCE: [&str; 2] = ["src", "notes_src"];
/// The changes one record may hold (`att::pack`), for a row's carried cells.
const RECORD: usize = 64;

/// The roster's labels, each with the node cells it states of that Mac by column.
pub type Roster = BTreeMap<String, BTreeMap<&'static str, String>>;

/// What a run would do.
#[derive(Debug, Default)]
pub struct Plan {
    pub merges: Vec<Merge>,
    pub held: Vec<Held>,
    /// Each entry stays whole in one record: a preference with its move, then the machine rows.
    pub rows: Vec<Vec<Change>>,
}

/// One Mac's rows folded into the one that stays.
#[derive(Debug, Default)]
pub struct Merge {
    pub keep: String,
    pub gone: Vec<String>,
    pub label_from: Option<String>,
    pub label_to: String,
    pub aliases_added: Vec<String>,
    pub aliases_removed: Vec<String>,
    /// `(duplicate, column)` a cell moved from, a set's element as `column.element`.
    pub carried: Vec<(String, String)>,
    /// `(duplicate, column)` both rows hold differently: the node-bound row's stays.
    pub kept: Vec<(String, String)>,
    pub preferences: Vec<Moved>,
}

/// One preference of a duplicate.
#[derive(Debug)]
pub struct Moved {
    pub from: String,
    pub to: String,
    pub how: How,
    /// The columns it gave the row it moved to, by name.
    pub columns: Vec<String>,
    /// The columns the row it moved to holds differently and keeps, by name.
    pub kept: Vec<String>,
}

/// What a duplicate's preference became.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    /// The row that stays had none: it has one now.
    Moved,
    /// The row that stays had one: it took what it lacked.
    Filled,
    /// An import had ended the row that stays' preference: it is live again, as the duplicate's was.
    Revived,
    /// The owner ended the row that stays' preference: it stays ended.
    OwnerEnded,
}

impl How {
    pub const fn word(self) -> &'static str {
        match self {
            Self::Moved => "moved",
            Self::Filled => "filled",
            Self::Revived => "revived",
            Self::OwnerEnded => "owner-ended",
        }
    }
}

/// Rows that share a name and are not merged, and why.
#[derive(Debug)]
pub struct Held {
    pub rows: Vec<String>,
    pub names: Vec<String>,
    pub why: &'static str,
}

/// Every name a row goes by, each candidate of a conflicted label among them. An alias is a key, so
/// never an address; a label is a value, so another writer may have stored one there, and a report
/// never prints what this build would not take for a label.
fn names_of(row: &Row) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = row
        .fields
        .get("aliases")
        .map(|field| field.members().iter().map(|m| (*m).to_owned()).collect())
        .unwrap_or_default();
    if let Some(field) = row.fields.get("label") {
        names.extend(
            field
                .candidates_x()
                .into_iter()
                .filter(|x| {
                    MACHINE
                        .column("label")
                        .is_some_and(|column| cell::admissible(&column.ty, x))
                })
                .filter_map(Value::as_str)
                .map(str::to_owned),
        );
    }
    names
}

fn label_of(row: &Row) -> Option<&str> {
    row.get("label").and_then(Value::as_str)
}

fn status_of(row: &Row) -> Option<&str> {
    value(row.fields.get("status")).and_then(Value::as_str)
}

/// Whether no Mac is bound to `keep`: it has published no principal and no facts row names it, so its
/// Mac has yet to adopt it.
fn unbound(keep: &str, keeper: &Row, bound: &BTreeSet<String>) -> bool {
    !bound.contains(keep) && value(keeper.fields.get("node")).is_none()
}

/// The live rows that share a name with each other, transitively: each group of two or more.
fn shared_names(machines: &Projection) -> Vec<BTreeSet<&str>> {
    let mut by_name: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    let mut names_by_row: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for (pk, row) in machines.rows.iter().filter(|(_, row)| row.live()) {
        let names = names_of(row);
        for name in &names {
            by_name.entry(name.clone()).or_default().push(pk);
        }
        names_by_row.insert(pk, names);
    }
    let mut seen = BTreeSet::new();
    let mut groups = Vec::new();
    for &start in names_by_row.keys() {
        if !seen.insert(start) {
            continue;
        }
        let mut group = BTreeSet::from([start]);
        let mut todo = vec![start];
        while let Some(pk) = todo.pop() {
            for name in &names_by_row[pk] {
                for &other in &by_name[name] {
                    if seen.insert(other) {
                        group.insert(other);
                        todo.push(other);
                    }
                }
            }
        }
        if group.len() > 1 {
            groups.push(group);
        }
    }
    groups
}

/// Whether a row holds different concurrent values anywhere, a set's elements included.
fn conflicted(row: &Row) -> bool {
    row.fields.values().any(|field| match field {
        Field::Set(elements) => elements
            .values()
            .any(|element| matches!(element, Field::Conflict { .. })),
        Field::Conflict { .. } => true,
        Field::Value(_) => false,
    })
}

/// A register's value, unless it is cleared.
fn value(field: Option<&Field>) -> Option<&Value> {
    match field? {
        Field::Value(candidate) if !candidate.value.x.is_null() => Some(&candidate.value.x),
        _ => None,
    }
}

/// Whether the row that stays lacks what `candidate` says of a cell it holds as `there`: it has
/// none, an import cleared it, or an owner's word meets an import's. What the owner cleared stands.
fn lacks(candidate: &CellValue, there: Option<&Field>) -> bool {
    match there {
        None => true,
        Some(Field::Value(held)) if held.value.x.is_null() => held.value.s.is_some(),
        Some(Field::Value(held)) => held.value.s.is_some() && candidate.s.is_none() && !candidate.a,
        Some(Field::Set(_) | Field::Conflict { .. }) => false,
    }
}

/// The same value and provenance under this schema version, written by the owner's verb.
fn carried(field: &CellValue, version: u32) -> CellValue {
    CellValue {
        r: version,
        x: field.x.clone(),
        s: field.s.clone(),
        a: false,
    }
}

/// Plan the merges of `machines`, moving `preferences`. `bound` holds the machine keys any Mac's
/// own facts row names, which only that Mac rebinds; `roster` what the roster says of each label.
///
/// # Errors
/// A value carried breaks its column's type, which is a bug: nothing is written then.
pub fn plan(
    machines: &Projection,
    preferences: &Projection,
    bound: &BTreeSet<String>,
    roster: &Roster,
) -> Result<Plan> {
    let mut plan = Plan::default();
    for group in shared_names(machines) {
        let rows: Vec<(&str, &Row)> = group.iter().map(|&pk| (pk, &machines.rows[pk])).collect();
        let names: BTreeSet<String> = rows.iter().flat_map(|(_, row)| names_of(row)).collect();
        match choose(&rows, preferences, bound, &names, roster) {
            Err(why) => plan.held.push(Held {
                rows: rows.iter().map(|(pk, _)| (*pk).to_owned()).collect(),
                names: names.into_iter().collect(),
                why,
            }),
            Ok((keep, label)) => {
                let waits = unbound(keep, &machines.rows[keep], bound);
                merge_group(&mut plan, &rows, keep, label, &names, preferences, waits)?;
            }
        }
    }
    Ok(plan)
}

/// The row that stays and the label it takes, or why these rows are not merged.
fn choose<'a>(
    rows: &[(&'a str, &Row)],
    preferences: &Projection,
    bound: &BTreeSet<String>,
    names: &BTreeSet<String>,
    roster: &Roster,
) -> std::result::Result<(&'a str, String), &'static str> {
    let on_group = |row: &Row| {
        rows.iter()
            .any(|(pk, _)| row.get("machine").and_then(Value::as_str) == Some(*pk))
    };
    // A preference whose end is in dispute may be live, so it is settled before it is left behind.
    let unsettled = |row: &Row| {
        matches!(row.fields.get(LIFECYCLE), Some(Field::Conflict { .. }))
            || (row.live() && conflicted(row))
    };
    // What says which preference this is, or whether a row is this Mac's, may be a newer build's.
    if rows.iter().any(|(_, row)| {
        NOT_CARRIED
            .iter()
            .chain(&["status"])
            .any(|column| row.raw.contains(*column))
    }) || preferences.rows.values().any(|row| {
        on_group(row)
            && PREFERENCE_IDENTITY
                .iter()
                .any(|column| row.raw.contains(*column))
    }) {
        return Err("unreadable");
    }
    if rows.iter().any(|(_, row)| conflicted(row))
        || preferences
            .rows
            .values()
            .any(|row| on_group(row) && unsettled(row))
    {
        return Err("conflicted");
    }
    let nodes: Vec<(&'a str, Vec<&Value>)> = rows
        .iter()
        .filter_map(|(pk, row)| {
            let held: Vec<&Value> = NODE_CELLS
                .iter()
                .filter_map(|column| value(row.fields.get(*column)))
                .collect();
            (!held.is_empty()).then_some((*pk, held))
        })
        .collect();
    let keep = match nodes.as_slice() {
        [] => return Err("no-node"),
        [(keep, _)] => *keep,
        [(_, first), rest @ ..]
            if rest
                .iter()
                .all(|(_, held)| held.iter().any(|node| first.contains(node))) =>
        {
            return Err("one-node-twice");
        }
        _ => return Err("two-macs"),
    };
    if rows
        .iter()
        .any(|(pk, _)| *pk != keep && bound.contains(*pk))
    {
        return Err("bound-by-its-mac");
    }
    let keeper = rows
        .iter()
        .find(|(pk, _)| *pk == keep)
        .map(|(_, row)| *row)
        .expect("the row that stays is one of the rows");
    let statuses: BTreeSet<&str> = rows
        .iter()
        .filter_map(|(_, row)| value(row.fields.get("status"))?.as_str())
        .filter(|status| *status != "planned")
        .collect();
    if statuses.len() > 1 {
        return Err("status-differs");
    }
    // A duplicate waiting for its Mac by name, beside a row no Mac is bound to: the row that stays
    // must wait too (`register::pick`), which only an import's `active` lets it, since that word says
    // nothing (`dump_computer`). Any other word is a statement this merge does not overrule.
    let waiting = rows
        .iter()
        .any(|(pk, row)| *pk != keep && status_of(row) == Some("planned"));
    let can_wait = match keeper.fields.get("status") {
        None => true,
        Some(Field::Value(held)) => {
            held.value.x == json!("planned")
                || (held.value.s.is_some()
                    && (held.value.x.is_null() || held.value.x == json!("active")))
        }
        Some(_) => false,
    };
    if waiting && unbound(keep, keeper, bound) && !can_wait {
        return Err("status-differs");
    }
    // The preference a duplicate's moves onto may itself be in dispute.
    let target_unsettled = preferences.rows.values().any(|row| {
        on_group(row)
            && row.live()
            && row
                .get("account")
                .and_then(Value::as_str)
                .and_then(|account| keys::preference_pk(keep, account).ok())
                .and_then(|target| preferences.rows.get(&target))
                .is_some_and(conflicted)
    });
    if target_unsettled {
        return Err("conflicted");
    }
    let mut current = names.iter().filter(|name| roster.contains_key(*name));
    let label = match (current.next(), current.next()) {
        (Some(_), Some(_)) => return Err("two-roster-names"),
        (Some(name), None) => {
            let holds = |column: &str, node: &str| {
                value(keeper.fields.get(column)).map(|held| held.as_str() == Some(node))
            };
            let other = roster[name]
                .iter()
                .any(|(column, node)| holds(column, node) == Some(false))
                || roster.iter().any(|(label, stated)| {
                    label != name
                        && stated
                            .iter()
                            .any(|(column, node)| holds(column, node) == Some(true))
                });
            if other {
                return Err("two-macs");
            }
            name.clone()
        }
        (None, _) => {
            let held = label_of(keeper);
            match held.or_else(|| names.first().map(String::as_str)) {
                Some(label) => label.to_owned(),
                None => return Err("no-node"),
            }
        }
    };
    Ok((keep, label))
}

fn owner_cell(pk: &str, col: &str, elem: Option<&str>, x: Value) -> Result<Change> {
    cell::change(
        &MACHINE,
        CellKey { pk, col, elem },
        &CellValue::owner(MACHINE.version, x),
    )
}

fn merge_group(
    plan: &mut Plan,
    rows: &[(&str, &Row)],
    keep: &str,
    label: String,
    names: &BTreeSet<String>,
    preferences: &Projection,
    waits: bool,
) -> Result<()> {
    let keeper = rows
        .iter()
        .find(|(pk, _)| *pk == keep)
        .map(|(_, row)| *row)
        .expect("the row that stays is one of the rows");
    let gone: Vec<(&str, &Row)> = rows.iter().copied().filter(|(pk, _)| *pk != keep).collect();
    let mut merge = Merge {
        keep: keep.to_owned(),
        gone: gone.iter().map(|(pk, _)| (*pk).to_owned()).collect(),
        label_from: label_of(keeper).map(str::to_owned),
        label_to: label.clone(),
        ..Merge::default()
    };
    // Each preference moves in its own entry, with its end, so a run cut short leaves it on one
    // row and the next run finds nothing left to move of it.
    for (pk, _) in &gone {
        move_preferences(plan, &mut merge, pk, keep, preferences)?;
    }
    let mut carries = Vec::new();
    for (pk, row) in &gone {
        carry_cells(&mut merge, &mut carries, pk, row, keep, keeper, waits)?;
    }
    carry_hosts(&mut merge, &mut carries, &gone, keep, keeper)?;
    plan.rows
        .extend(carries.chunks(RECORD).map(<[Change]>::to_vec));
    // The names and the duplicates' ends go in one record, so no reader sees a name on two rows or
    // on none.
    let mut last = Vec::new();
    if merge.label_from.as_deref() != Some(label.as_str()) {
        // The label keeps the provenance it had, so the source that wrote it can move it again.
        let source = rows
            .iter()
            .find_map(|(_, row)| match row.fields.get("label") {
                Some(Field::Value(candidate)) if candidate.value.x == json!(label) => {
                    candidate.value.s.clone()
                }
                _ => None,
            });
        last.push(cell::change(
            &MACHINE,
            CellKey {
                pk: keep,
                col: "label",
                elem: None,
            },
            &CellValue {
                r: MACHINE.version,
                x: json!(label),
                s: source,
                a: false,
            },
        )?);
    }
    let held = keeper.fields.get("aliases");
    let element = |name: &str| match held {
        Some(Field::Set(elements)) => elements.get(name).and_then(|field| match field {
            Field::Value(candidate) => Some(&candidate.value),
            _ => None,
        }),
        _ => None,
    };
    for name in names {
        let member = held.is_some_and(|field| field.has_member(name));
        if *name == label {
            if member {
                let source = element(name).and_then(|cell| cell.s.clone());
                last.push(cell::change(
                    &MACHINE,
                    CellKey {
                        pk: keep,
                        col: "aliases",
                        elem: Some(name),
                    },
                    &CellValue {
                        r: MACHINE.version,
                        x: json!(false),
                        s: source,
                        a: false,
                    },
                )?);
                merge.aliases_removed.push(name.clone());
            }
        } else if element(name).is_some_and(|cell| cell.x == json!(false) && cell.s.is_none()) {
            // The owner took this name off the row that stays: it stays off.
            let from = gone
                .iter()
                .find(|(_, row)| names_of(row).contains(name))
                .map_or(keep, |(pk, _)| *pk);
            merge
                .kept
                .push((from.to_owned(), format!("aliases.{name}")));
        } else if !member {
            last.push(owner_cell(keep, "aliases", Some(name), json!(true))?);
            merge.aliases_added.push(name.clone());
        }
    }
    for (pk, _) in &gone {
        last.push(owner_cell(pk, "merged_into", None, json!(keep))?);
        last.push(owner_cell(pk, LIFECYCLE, None, json!("merged"))?);
    }
    plan.rows.push(last);
    plan.merges.push(merge);
    Ok(())
}

/// The hosts of the row that stays and of its duplicates together, its own first, as many as a row
/// holds; what does not fit is listed.
fn carry_hosts(
    merge: &mut Merge,
    out: &mut Vec<Change>,
    gone: &[(&str, &Row)],
    keep: &str,
    keeper: &Row,
) -> Result<()> {
    let list = |row: &Row| -> Vec<String> {
        row.get("hosts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    };
    let mut hosts = list(keeper);
    let mut source = match keeper.fields.get("hosts") {
        Some(Field::Value(candidate)) if !candidate.value.x.is_null() => candidate.value.s.clone(),
        _ => None,
    };
    let mut grew = false;
    for (pk, row) in gone {
        for host in list(row) {
            if hosts.iter().any(|held| same_host(held, &host)) {
                continue;
            }
            if hosts.len() == MAX_HOSTS {
                merge.kept.push(((*pk).to_owned(), "hosts".to_owned()));
                continue;
            }
            if !grew
                && hosts.is_empty()
                && let Some(Field::Value(candidate)) = row.fields.get("hosts")
            {
                source.clone_from(&candidate.value.s);
            }
            hosts.push(host);
            grew = true;
            if !merge.carried.iter().any(|(_, column)| column == "hosts") {
                merge.carried.push(((*pk).to_owned(), "hosts".to_owned()));
            }
        }
    }
    if grew {
        out.push(cell::change(
            &MACHINE,
            CellKey {
                pk: keep,
                col: "hosts",
                elem: None,
            },
            &CellValue {
                r: MACHINE.version,
                x: json!(hosts),
                s: source,
                a: false,
            },
        )?);
    }
    Ok(())
}

/// Whether a duplicate's `status` word moves onto the row that stays. `planned` is the word a row
/// holds until a Mac joins it and fleet_db's `active` says nothing of that, so `active` never moves
/// off it while `parked` or `lost` do, and it moves onto a row that `waits` as the Mac's way in.
fn status_moves(word: &CellValue, there: Option<&Field>, waits: bool) -> bool {
    match value(there).and_then(Value::as_str) {
        None => lacks(word, there),
        Some("planned") => word.x != json!("active"),
        Some(_) => waits && word.x == json!("planned"),
    }
}

/// The cells of duplicate `pk` the row that stays lacks, with their provenance. A set takes the
/// elements it never held: one it holds false was removed there. A cell of the row that stays this
/// build cannot read stays. Its `status` is [`status_moves`]'s, and a `planned` it takes over an
/// import's word keeps that import's provenance, so a later import can still correct it.
fn carry_cells(
    merge: &mut Merge,
    out: &mut Vec<Change>,
    pk: &str,
    row: &Row,
    keep: &str,
    keeper: &Row,
    waits: bool,
) -> Result<()> {
    for (column, field) in &row.fields {
        if NOT_CARRIED.contains(&column.as_str()) || row.raw.contains(column) {
            continue;
        }
        if keeper.raw.contains(column) {
            merge.kept.push((pk.to_owned(), column.clone()));
            continue;
        }
        match field {
            Field::Set(elements) => {
                let held = keeper.fields.get(column);
                for (elem, element) in elements {
                    let Field::Value(candidate) = element else {
                        continue;
                    };
                    let shown = format!("{column}.{elem}");
                    let known = matches!(held, Some(Field::Set(have)) if have.contains_key(elem));
                    let taken = merge.carried.iter().any(|(_, column)| *column == shown);
                    if candidate.value.x == json!(true) && !known && !taken {
                        out.push(cell::change(
                            &MACHINE,
                            CellKey {
                                pk: keep,
                                col: column,
                                elem: Some(elem),
                            },
                            &carried(&candidate.value, MACHINE.version),
                        )?);
                        merge.carried.push((pk.to_owned(), shown));
                    }
                }
            }
            Field::Value(candidate) if !candidate.value.x.is_null() => {
                let there = keeper.fields.get(column);
                let held = value(there);
                let status = column == "status";
                let needed = if status {
                    status_moves(&candidate.value, there, waits)
                } else {
                    lacks(&candidate.value, there)
                };
                let taken = merge.carried.iter().any(|(_, col)| col == column);
                if held == Some(&candidate.value.x) {
                    continue;
                }
                if needed && !taken {
                    let mut moved = carried(&candidate.value, MACHINE.version);
                    if status
                        && held.is_some_and(|word| word != &json!("planned"))
                        && let Some(Field::Value(word)) = there
                    {
                        moved.s.clone_from(&word.value.s);
                    }
                    out.push(cell::change(
                        &MACHINE,
                        CellKey {
                            pk: keep,
                            col: column,
                            elem: None,
                        },
                        &moved,
                    )?);
                    merge.carried.push((pk.to_owned(), column.clone()));
                } else if !PROVENANCE.contains(&column.as_str()) {
                    merge.kept.push((pk.to_owned(), column.clone()));
                }
            }
            Field::Value(_) | Field::Conflict { .. } => {}
        }
    }
    // What a newer build wrote can't be read here, so it stays on the duplicate.
    merge
        .kept
        .extend(row.raw.iter().map(|column| (pk.to_owned(), column.clone())));
    Ok(())
}

/// Move every live preference of duplicate `pk` to `keep`, each in an entry of its own.
fn move_preferences(
    plan: &mut Plan,
    merge: &mut Merge,
    pk: &str,
    keep: &str,
    preferences: &Projection,
) -> Result<()> {
    let change = |target: &str, col: &str, value: &CellValue| {
        cell::change(
            &PREFERENCE,
            CellKey {
                pk: target,
                col,
                elem: None,
            },
            value,
        )
    };
    let moving = preferences
        .rows
        .iter()
        .filter(|(_, row)| row.live() && row.get("machine").and_then(Value::as_str) == Some(pk));
    for (from, row) in moving {
        let Some(account) = row.get("account").and_then(Value::as_str) else {
            continue;
        };
        let target = keys::preference_pk(keep, account)?;
        let earlier = merge.preferences.iter().filter(|moved| moved.to == target);
        let given: BTreeSet<&str> = earlier
            .clone()
            .flat_map(|moved| moved.columns.iter().map(String::as_str))
            .collect();
        let already = earlier.clone().next().map(|moved| moved.how);
        let there = preferences.rows.get(&target);
        let how = match (already, there) {
            (Some(How::OwnerEnded), _) => How::OwnerEnded,
            (Some(_), _) => How::Filled,
            (None, None) => How::Moved,
            (None, Some(row)) if row.live() => How::Filled,
            (None, Some(row)) => match row.fields.get(LIFECYCLE) {
                Some(Field::Value(candidate)) if candidate.value.s.is_some() => How::Revived,
                _ => How::OwnerEnded,
            },
        };
        let mut entry = Vec::new();
        let mut columns = Vec::new();
        let mut kept: Vec<String> = row.raw.iter().cloned().collect();
        if how != How::OwnerEnded {
            for (column, field) in &row.fields {
                let Field::Value(candidate) = field else {
                    continue;
                };
                if PREFERENCE_IDENTITY.contains(&column.as_str())
                    || row.raw.contains(column)
                    || candidate.value.x.is_null()
                    || given.contains(column.as_str())
                {
                    continue;
                }
                if there.is_some_and(|row| row.raw.contains(column)) {
                    kept.push(column.clone());
                    continue;
                }
                let on_target = there.and_then(|row| row.fields.get(column));
                if value(on_target) == Some(&candidate.value.x) {
                    continue;
                }
                if lacks(&candidate.value, on_target) {
                    entry.push(change(
                        &target,
                        column,
                        &carried(&candidate.value, PREFERENCE.version),
                    )?);
                    columns.push(column.clone());
                } else if !PROVENANCE.contains(&column.as_str()) {
                    kept.push(column.clone());
                }
            }
        }
        if how == How::Moved {
            for (column, x) in [("machine", json!(keep)), ("account", json!(account))] {
                let source = match row.fields.get(column) {
                    Some(Field::Value(candidate)) => candidate.value.s.clone(),
                    _ => None,
                };
                entry.push(change(
                    &target,
                    column,
                    &CellValue {
                        r: PREFERENCE.version,
                        x,
                        s: source,
                        a: false,
                    },
                )?);
            }
        }
        if matches!(how, How::Moved | How::Revived) {
            let source = match row.fields.get(LIFECYCLE) {
                Some(Field::Value(candidate)) => candidate.value.s.clone(),
                _ => None,
            };
            entry.push(change(
                &target,
                LIFECYCLE,
                &CellValue {
                    r: PREFERENCE.version,
                    x: json!("live"),
                    s: source,
                    a: false,
                },
            )?);
        }
        entry.push(change(
            from,
            LIFECYCLE,
            &CellValue::owner(PREFERENCE.version, json!("ended")),
        )?);
        plan.rows.push(entry);
        merge.preferences.push(Moved {
            from: from.clone(),
            to: target,
            how,
            columns,
            kept,
        });
    }
    Ok(())
}

impl Plan {
    /// The value-free report: keys, labels and column names.
    pub fn report(&self) -> Value {
        let pairs = |list: &[(String, String)]| -> Vec<Value> {
            list.iter()
                .map(|(from, column)| json!({"from": from, "column": column}))
                .collect()
        };
        json!({
            "merges": self.merges.iter().map(|merge| json!({
                "keep": merge.keep,
                "gone": merge.gone,
                "label": {"from": merge.label_from, "to": merge.label_to},
                "aliases_added": merge.aliases_added,
                "aliases_removed": merge.aliases_removed,
                "carried": pairs(&merge.carried),
                "kept": pairs(&merge.kept),
                "preferences": merge.preferences.iter().map(|moved| json!({
                    "from": moved.from,
                    "to": moved.to,
                    "how": moved.how.word(),
                    "columns": moved.columns,
                    "kept": moved.kept,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "held": self.held.iter().map(|held| json!({
                "rows": held.rows,
                "names": held.names,
                "why": held.why,
            })).collect::<Vec<_>>(),
        })
    }
}

#[cfg(test)]
mod tests;
