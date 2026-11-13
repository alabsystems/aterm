// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Tables page's one reach into the registry: a read of every shown collection as the grid
//! lays it out (`aterm_db::typed::grid::export`, addresses and secrets masked), at the head or as
//! of a past operation (`Handle::view_at`) with what changed since (`Handle::diff`), with each
//! cell's candidates and each operation's author, and a write of the staged cells through
//! [`aterm_db::att::Handle::save`], the same save `aterm db table apply` makes. Nothing here
//! runs on the main thread: [`crate::native_tables`]'s worker calls it.

#[cfg(unix)]
use std::collections::BTreeMap;

#[cfg(unix)]
use aterm_db::typed::grid::{self, Base};
#[cfg(unix)]
use aterm_db::typed::{Collection, Ty, Writer};

#[cfg(unix)]
use crate::native_tables::{
    AsOf, Author, AuthorKind, Group, Sheet, SheetColumn, SheetRow, Shown, Since, Written,
    WrittenCell,
};
use crate::native_tables::{Snapshot, Staged, Wrote};

/// The shown collections, in the rail's order: the registry's name, the page's name, and the
/// name of one row.
pub(crate) const SHOWN: [(&str, &str, &str); 5] = [
    ("machine", "Machines", "Machine"),
    ("account", "AI accounts", "AI account"),
    ("address", "Emails", "Email"),
    ("mailbox", "Mailboxes", "Mailbox"),
    ("phone", "Phones", "Phone"),
];

#[cfg(unix)]
/// Columns the page leaves to later packages: the row's lifecycle (retire and restore) and the
/// import coordinates.
const HIDDEN: [&str; 4] = ["row", "src", "notes_src", "pk"];

/// The words of a column's header: its name in capitals, words apart.
pub(crate) fn label_of(name: &str) -> String {
    name.replace('_', " ").to_uppercase()
}

#[cfg(unix)]
fn group_of(column: &Collection, name: &str) -> (Group, bool) {
    let Some(schema) = column.column(name) else {
        return (Group::Stored, false);
    };
    let group = match schema.writer {
        Writer::Owner => Group::Stored,
        Writer::Birth | Writer::Mine | Writer::Seed => Group::SetByAterm,
    };
    let numeric = matches!(schema.ty, Ty::Int { .. } | Ty::Uint);
    (group, numeric)
}

#[cfg(unix)]
/// The collection a reference column names, one or a set of.
const fn target(ty: &Ty) -> Option<&'static str> {
    match ty {
        Ty::Ref(target) => Some(target),
        Ty::Set(Ty::Ref(target)) => Some(target),
        _ => None,
    }
}

#[cfg(unix)]
/// How a typed value reads in the pane and in history: a secret only as whether it holds one,
/// everything else as the grid shows it.
fn shown_value(ty: &Ty, value: Option<&aterm_json::Value>) -> String {
    match value {
        None => String::new(),
        Some(value) if value.is_null() => String::new(),
        Some(_) if matches!(ty, Ty::Secret) => "set".into(),
        Some(value) => grid::show(ty, value, false),
    }
}

/// One seat of this Mac's membership log: the Commons actor it is keyed by, the machine its
/// binding names, and its label (`m73`).
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Seat {
    pub(crate) actor: [u8; 48],
    pub(crate) machine: [u8; 16],
    pub(crate) label: String,
}

/// Every seat this Mac's membership log holds, removed ones included: a removed Mac's signed rows
/// are still its own. None where the vault holds no log here.
#[cfg(unix)]
fn seats(paths: &Paths) -> Vec<Seat> {
    let Ok(read) = aterm_secrets::client::load_acknowledged_and_ahead(paths) else {
        return Vec::new();
    };
    let Some(log) = read.acknowledged.log() else {
        return Vec::new();
    };
    aterm_secrets::client::seats_ever(log)
        .into_iter()
        .map(|(actor, machine, label)| Seat {
            actor,
            machine,
            label,
        })
        .collect()
}

/// Each writer's label, from what aterm already holds: an operation's actor names a seat, the
/// seat a Commons machine, and the machine a label. Nothing here makes a name up: a writer none
/// of these name has no label.
#[cfg(unix)]
#[derive(Debug, Default)]
pub(crate) struct Writers {
    by_actor: BTreeMap<[u8; 48], String>,
    by_machine: BTreeMap<[u8; 16], String>,
}

#[cfg(unix)]
impl Writers {
    /// The labels `machines` (the registry's machine rows), `facts` (each Mac's own statement,
    /// keyed by its Commons machine) and `seats` (this Mac's membership log) give.
    ///
    /// A machine's label is, first, the registry's: the unconflicted `label` of the machine row
    /// its facts row binds (`merged_into` where it states one). Then the label its facts row
    /// states, then its seat's. A facts row's `predecessor` is the same Mac before writer
    /// succession, so that machine's operations take the successor's label.
    pub(crate) fn new(
        machines: &aterm_db::typed::cell::Projection,
        facts: &aterm_db::typed::owned::OwnedRead,
        seats: &[Seat],
    ) -> Self {
        let text = |value: &aterm_json::Value, column: &str| {
            value
                .get(column)
                .and_then(aterm_json::Value::as_str)
                .filter(|t| !t.is_empty())
                .map(str::to_owned)
        };
        let registry = |mac: &str| {
            let row = machines.rows.get(mac)?;
            if row.raw.contains("label") || row.fields.get("label")?.conflicted() {
                return None;
            }
            row.get("label")
                .and_then(aterm_json::Value::as_str)
                .filter(|t| !t.is_empty())
                .map(str::to_owned)
        };
        let mut by_machine: BTreeMap<[u8; 16], String> = BTreeMap::new();
        for row in facts.rows.values() {
            let mac = text(&row.value, "merged_into").or_else(|| text(&row.value, "mac"));
            if let Some(label) = mac
                .as_deref()
                .and_then(registry)
                .or_else(|| text(&row.value, "label"))
            {
                by_machine.insert(row.machine, label);
            }
        }
        let mut by_actor = BTreeMap::new();
        for seat in seats {
            by_actor.insert(seat.actor, seat.label.clone());
            by_machine
                .entry(seat.machine)
                .or_insert_with(|| seat.label.clone());
        }
        // Each pass carries a label one predecessor back; a chain is no longer than the rows.
        for _ in 0..facts.rows.len() {
            let mut carried = false;
            for row in facts.rows.values() {
                let Some(before) = text(&row.value, "predecessor")
                    .and_then(|p| aterm_db::typed::keys::parse_uuid32(&p))
                else {
                    continue;
                };
                if by_machine.contains_key(&before) {
                    continue;
                }
                if let Some(label) = by_machine.get(&row.machine).cloned() {
                    by_machine.insert(before, label);
                    carried = true;
                }
            }
            if !carried {
                break;
            }
        }
        Self {
            by_actor,
            by_machine,
        }
    }

    /// The label of the Mac that signed as `actor` from Commons machine `machine`, where any is
    /// known.
    pub(crate) fn label(&self, actor: &[u8; 48], machine: Option<&[u8; 16]>) -> Option<String> {
        machine
            .and_then(|m| self.by_machine.get(m))
            .or_else(|| self.by_actor.get(actor))
            .cloned()
    }
}

/// What names a writer no label names: the start of its Commons machine id, else of its actor.
#[cfg(unix)]
pub(crate) fn short_id(actor: &[u8; 48], machine: Option<&[u8; 16]>) -> String {
    match machine {
        Some(machine) => aterm_codec::hex::encode_lower(&machine[..4]),
        None => aterm_codec::hex::encode_lower(&actor[..4]),
    }
}

/// The column that names a row of `collection` among the shown `columns`: the first that is
/// unique by itself (a machine's or a mailbox's `label`, an address's `addr`), else its `label`,
/// else its first column.
#[cfg(unix)]
fn key_column(collection: &Collection, columns: &[SheetColumn]) -> String {
    let shown = |name: &str| {
        columns
            .iter()
            .any(|c| c.name == name && c.group == Group::Stored)
    };
    collection
        .unique
        .iter()
        .filter_map(|names| match names {
            [name] => Some(*name),
            _ => None,
        })
        .chain(["label"])
        .find(|name| shown(name))
        .map(str::to_owned)
        .or_else(|| columns.first().map(|c| c.name.clone()))
        .unwrap_or_default()
}

/// Read every shown collection of this Mac's registry, as of the operation `at` names (the head
/// where `None`), and, as of a past one, what changed in each since.
///
/// # Errors
/// The registry cannot be read here: no Commons, or not subscribed, or its store answered busy
/// past the read's deadline; or this Mac holds no operation `at`.
#[cfg(unix)]
pub(crate) fn read(paths: &Paths, at: Option<[u8; 32]>) -> Result<Snapshot, String> {
    use aterm_db::att::{Handle, Point, REGISTRY};
    let handle = Handle::at(paths.clone(), &REGISTRY);
    let compiled = handle.compiled();
    let view = handle
        .view_at(at.map_or(Point::Head, Point::At))
        .map_err(|e| e.to_string())?;
    let history = handle.history().map_err(|e| e.to_string())?;
    let since = match at {
        Some(at) => {
            handle
                .diff(at, Point::Head)
                .map_err(|e| e.to_string())?
                .cells
        }
        None => Vec::new(),
    };
    let empty = aterm_db::typed::owned::OwnedRead::default();
    // A writer is named as the registry names its Mac now, in a past table too: history runs
    // past the point, and a Mac labelled since is still that Mac.
    let head = match at {
        Some(_) => Some(handle.view().map_err(|e| e.to_string())?),
        None => None,
    };
    let names = head.as_ref().unwrap_or(&view);
    let writers = Writers::new(
        &names.project(&aterm_db::alab::registry::MACHINE),
        names.owned.get("machine-facts").unwrap_or(&empty),
        &seats(paths),
    );
    let author = |entry: &aterm_db::att::Entry| -> Author {
        let mut kind = AuthorKind::Person;
        for change in &entry.changes {
            if let Some(cell) = change
                .value
                .as_ref()
                .and_then(|v| aterm_db::typed::cell::CellValue::decode(v).ok())
            {
                if cell.a {
                    kind = AuthorKind::Aterm;
                } else if cell.s.is_some() {
                    kind = AuthorKind::Import;
                }
            }
        }
        Author {
            kind,
            label: writers.label(&entry.actor, entry.machine.as_ref()),
            id: short_id(&entry.actor, entry.machine.as_ref()),
            at_ms: entry.authored_ms,
            hlc: (entry.hlc.ms, entry.hlc.counter),
        }
    };
    let by_operation: BTreeMap<[u8; 32], Author> = history
        .iter()
        .map(|entry| (entry.operation, author(entry)))
        .collect();
    let mut projections = BTreeMap::new();
    for (name, _, _) in SHOWN {
        let collection = compiled.collection(name).map_err(|e| e.to_string())?;
        projections.insert(name, (collection, view.project(collection)));
    }
    let mut sheets = Vec::with_capacity(SHOWN.len());
    for (name, title, singular) in SHOWN {
        let (collection, projection) = &projections[name];
        let collection: &'static Collection = collection;
        let export = view
            .export_grid(compiled.table, collection, &[], false, None)
            .map_err(|e| e.to_string())?;
        let base = export.base;
        let mut columns: Vec<SheetColumn> = base
            .header
            .iter()
            .filter_map(|header| {
                let read_only = header.starts_with(grid::READ_ONLY);
                let name = header.trim_start_matches(grid::READ_ONLY);
                if HIDDEN.contains(&name) {
                    return None;
                }
                let (group, numeric) = group_of(collection, name);
                Some(SheetColumn {
                    name: name.to_owned(),
                    label: label_of(name),
                    group,
                    editable: !read_only && group == Group::Stored,
                    numeric,
                    secret: collection
                        .column(name)
                        .is_some_and(|c| matches!(c.ty, Ty::Secret)),
                    masked: collection.column(name).is_some_and(|c| c.ty.pii()),
                    links: collection
                        .column(name)
                        .and_then(|c| target(&c.ty))
                        .map(str::to_owned),
                })
            })
            .collect();
        // LINKED ROWS: how many live rows of another shown collection name each row.
        let mut linked: Vec<(String, &'static str, BTreeMap<String, usize>)> = Vec::new();
        for (other, other_title, _) in SHOWN {
            let (other_collection, other_projection) = &projections[other];
            for column in other_collection.columns {
                if target(&column.ty) != Some(name) || !column.live_at(other_collection.version) {
                    continue;
                }
                let mut counts = BTreeMap::new();
                for row in other_projection.rows.values().filter(|r| r.live()) {
                    match row.fields.get(column.name) {
                        Some(aterm_db::typed::cell::Field::Set(members)) => {
                            for (member, field) in members {
                                if field.present() {
                                    *counts.entry(member.clone()).or_insert(0) += 1;
                                }
                            }
                        }
                        Some(field) => {
                            for candidate in field.candidates() {
                                if let Some(pk) = candidate.value.x.as_str() {
                                    *counts.entry(pk.to_owned()).or_insert(0) += 1;
                                }
                            }
                        }
                        None => {}
                    }
                }
                let label = format!(
                    "{} \u{b7} {}",
                    other_title.to_uppercase(),
                    label_of(column.name)
                );
                linked.push((format!("~{other}.{}", column.name), other_title, counts));
                columns.push(SheetColumn {
                    name: format!("~{other}.{}", column.name),
                    label,
                    group: Group::Linked,
                    editable: false,
                    numeric: true,
                    secret: false,
                    masked: false,
                    links: Some(other.to_owned()),
                });
            }
        }
        let mut rows = Vec::new();
        for (pk, seen) in &base.rows {
            let Some(row) = projection.rows.get(pk) else {
                continue;
            };
            if !row.live() {
                continue;
            }
            let mut cells = BTreeMap::new();
            for column in &columns {
                if let Some((_, _, counts)) = linked.iter().find(|(n, _, _)| *n == column.name) {
                    let count = counts.get(pk).copied().unwrap_or(0);
                    cells.insert(
                        column.name.clone(),
                        Shown {
                            text: if count == 0 {
                                String::new()
                            } else {
                                count.to_string()
                            },
                            ..Shown::default()
                        },
                    );
                    continue;
                }
                let Some(seen) = seen.get(&column.name) else {
                    continue;
                };
                let schema = collection.column(&column.name);
                let field = row.fields.get(&column.name);
                let candidates = field
                    .map(aterm_db::typed::cell::Field::candidates)
                    .unwrap_or_default();
                let values: Vec<(String, Option<Author>)> = if candidates.len() > 1 {
                    let mut values: Vec<(String, Option<Author>)> = candidates
                        .iter()
                        .map(|c| {
                            (
                                schema.map_or_else(
                                    || seen.shown.clone(),
                                    |s| shown_value(&s.ty, Some(&c.value.x)),
                                ),
                                by_operation.get(&c.operation).cloned(),
                            )
                        })
                        .collect();
                    values.sort_by(|a, b| a.0.cmp(&b.0));
                    values
                } else {
                    Vec::new()
                };
                let by = candidates
                    .first()
                    .filter(|_| candidates.len() == 1)
                    .and_then(|c| by_operation.get(&c.operation).cloned())
                    .or_else(|| {
                        // A set's writer: its newest present member's.
                        seen.elements
                            .values()
                            .flatten()
                            .filter_map(|d| by_operation.get(d))
                            .max_by_key(|a| a.at_ms)
                            .cloned()
                    });
                cells.insert(
                    column.name.clone(),
                    Shown {
                        text: if values.is_empty() {
                            seen.shown.clone()
                        } else {
                            values
                                .iter()
                                .map(|v| v.0.as_str())
                                .collect::<Vec<_>>()
                                .join(" / ")
                        },
                        by,
                        values,
                        raw: seen.raw,
                    },
                );
            }
            rows.push(SheetRow {
                pk: pk.clone(),
                cells,
            });
        }
        let mut written = Vec::new();
        for entry in &history {
            let mut changed = Vec::new();
            for change in entry.changes.iter().filter(|c| c.collection == name) {
                let Ok(key) = aterm_db::typed::cell::CellKey::parse(&change.key) else {
                    continue;
                };
                let Some(schema) = collection.column(key.col) else {
                    continue;
                };
                let cell = change
                    .value
                    .as_ref()
                    .and_then(|v| aterm_db::typed::cell::CellValue::decode(v).ok());
                let now = match (&cell, key.elem) {
                    (Some(cell), Some(elem)) => {
                        if cell.x.as_bool() == Some(true) {
                            format!("+ {elem}")
                        } else {
                            format!("\u{2212} {elem}")
                        }
                    }
                    (Some(cell), None) => shown_value(&schema.ty, Some(&cell.x)),
                    (None, _) => String::new(),
                };
                changed.push(WrittenCell {
                    pk: key.pk.to_owned(),
                    column: key.col.to_owned(),
                    secret: matches!(schema.ty, Ty::Secret),
                    now,
                });
            }
            if !changed.is_empty() {
                written.push(Written {
                    operation: entry.operation,
                    by: author(entry),
                    cells: changed,
                });
            }
        }
        let key = key_column(collection, &columns);
        sheets.push(Sheet {
            name,
            title,
            singular,
            key,
            columns,
            rows,
            written,
            since: since_then(collection, &since, &by_operation),
            base,
        });
    }
    Ok(Snapshot {
        sheets,
        at: at.map(|operation| AsOf {
            operation,
            by: by_operation.get(&operation).cloned(),
        }),
        read_ms: aterm_time::unix_millis().unwrap_or(0),
    })
}

/// What changed in `collection` since the point a past read is at, by row and column: what the
/// cell holds now as the pane shows it (a set by the members put in or taken out), and who wrote
/// that. A secret says only whether it holds one.
#[cfg(unix)]
fn since_then(
    collection: &'static Collection,
    cells: &[aterm_db::att::CellDiff],
    by_operation: &BTreeMap<[u8; 32], Author>,
) -> BTreeMap<(String, String), Since> {
    let mut found: BTreeMap<(String, String), Since> = BTreeMap::new();
    for cell in cells.iter().filter(|c| c.collection == collection.name) {
        let Ok(key) = aterm_db::typed::cell::CellKey::parse(&cell.key) else {
            continue;
        };
        let Some(schema) = collection.column(key.col) else {
            continue;
        };
        let now = match key.elem {
            Some(member) => match cell.delta {
                aterm_db::att::Delta::Retired => format!("\u{2212} {member}"),
                _ => format!("+ {member}"),
            },
            None => {
                let mut values: Vec<String> = cell
                    .after
                    .iter()
                    .filter_map(|c| aterm_db::typed::cell::CellValue::decode(&c.value).ok())
                    .map(|c| shown_value(&schema.ty, Some(&c.x)))
                    .filter(|text| !text.is_empty())
                    .collect();
                values.sort();
                values.dedup();
                values.join(" / ")
            }
        };
        let entry = found
            .entry((key.pk.to_owned(), key.col.to_owned()))
            .or_insert_with(|| Since {
                now: String::new(),
                by: Vec::new(),
                members: key.elem.is_some(),
            });
        if entry.now.is_empty() {
            entry.now = now;
        } else {
            entry.now = format!("{}, {now}", entry.now);
        }
        for maker in &cell.by {
            if let Some(author) = by_operation.get(&maker.operation)
                && !entry.by.contains(author)
            {
                entry.by.push(author.clone());
            }
        }
        entry.by.sort_by_key(|author| author.hlc);
    }
    found
}

#[cfg(unix)]
/// The grid text of `base` with each staged cell's text in place of what it showed.
pub(crate) fn staged_text(base: &Base, staged: &BTreeMap<(String, String), String>) -> String {
    let mut edited = base.clone();
    for (pk, seen) in &mut edited.rows {
        for (column, cell) in seen.iter_mut() {
            if let Some(text) = staged.get(&(pk.clone(), column.clone())) {
                cell.shown.clone_from(text);
            }
        }
    }
    grid::render(&edited)
}

/// Write each collection's staged cells, each collection in one save.
///
/// # Errors
/// A save could not reach the registry (no Commons writer, an incomplete history here, a busy
/// store past the deadline).
#[cfg(unix)]
pub(crate) fn write(paths: &Paths, staged: Vec<Staged>) -> Result<Wrote, String> {
    use aterm_db::att::{Handle, REGISTRY, Saved};
    let handle = Handle::at(paths.clone(), &REGISTRY);
    let mut wrote = Wrote::default();
    for Staged { base, cells } in staged {
        // A table that cannot be saved is named and the rest are still written.
        let saved = handle
            .compiled()
            .collection(&base.collection)
            .and_then(|collection| handle.save(collection, &base, &staged_text(&base, &cells)));
        let saved = match saved {
            Ok(saved) => saved,
            Err(why) => {
                wrote
                    .failed
                    .push((base.collection.clone(), why.to_string()));
                continue;
            }
        };
        match saved {
            Saved::Written { applied, .. } => {
                wrote.cells += applied.cells;
                wrote.kept.extend(
                    applied
                        .stale
                        .into_iter()
                        .map(|s| (base.collection.clone(), s.pk, s.column)),
                );
            }
            Saved::Problems(problems) => {
                for problem in problems {
                    let pk = problem
                        .record
                        .checked_sub(1)
                        .and_then(|i| base.rows.get(i))
                        .map(|(pk, _)| pk.clone())
                        .unwrap_or_default();
                    let column = base
                        .header
                        .get(problem.column)
                        .map(|h| h.trim_start_matches(grid::READ_ONLY).to_owned())
                        .unwrap_or_default();
                    // The page shows the rule at the cell it names, so a rule's own
                    // `<collection>.<column>: ` is not said again.
                    let placed = format!("{}.{column}: ", base.collection);
                    let rule = problem
                        .rule
                        .strip_prefix(&placed)
                        .map_or(problem.rule.clone(), str::to_owned);
                    wrote
                        .refused
                        .push((base.collection.clone(), pk, column, rule));
                }
            }
        }
    }
    Ok(wrote)
}

/// Where a Mac's registry is: the secrets client's paths, which only macOS and Linux have.
#[cfg(unix)]
pub(crate) type Paths = aterm_secrets::client::Paths;

/// No registry is kept here, so there are no paths to one.
#[cfg(not(unix))]
#[derive(Clone, Debug)]
pub(crate) enum Paths {}

/// This Mac's registry.
#[cfg(all(unix, not(test)))]
pub(crate) fn live() -> Result<Paths, String> {
    Paths::live().map_err(|e| e.to_string())
}

/// A test binary never reads this Mac's registry: its page reads only the scratch tables a test
/// names.
#[cfg(test)]
pub(crate) fn live() -> Result<Paths, String> {
    Err("a test reads only its own tables".into())
}

#[cfg(all(not(unix), not(test)))]
pub(crate) fn live() -> Result<Paths, String> {
    Err("the registry is kept on macOS and Linux".into())
}

#[cfg(not(unix))]
pub(crate) fn read(_: &Paths, _: Option<[u8; 32]>) -> Result<Snapshot, String> {
    Err("the registry is kept on macOS and Linux".into())
}

#[cfg(not(unix))]
pub(crate) fn write(_: &Paths, _: Vec<Staged>) -> Result<Wrote, String> {
    Err("the registry is kept on macOS and Linux".into())
}
