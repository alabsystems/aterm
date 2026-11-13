// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Tables page's one reach into the registry: a read of every shown collection as the grid
//! lays it out (`aterm_db::typed::grid::export`, addresses and secrets masked), with each cell's
//! candidates and each operation's author, and a write of the staged cells through
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
    Author, AuthorKind, Group, Sheet, SheetColumn, SheetRow, Shown, Written, WrittenCell,
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

/// Read every shown collection of this Mac's registry.
///
/// # Errors
/// The registry cannot be read here: no Commons, or not subscribed, or its store answered busy
/// past the read's deadline.
#[cfg(unix)]
pub(crate) fn read(paths: &Paths) -> Result<Snapshot, String> {
    use aterm_db::att::{Handle, REGISTRY};
    let handle = Handle::at(paths.clone(), &REGISTRY);
    let compiled = handle.compiled();
    let view = handle.view().map_err(|e| e.to_string())?;
    let history = handle.history().map_err(|e| e.to_string())?;
    let labels: BTreeMap<String, String> = view
        .owned
        .get("machine-facts")
        .map(|facts| {
            facts
                .rows
                .iter()
                .filter_map(|(uuid, row)| {
                    Some((uuid.clone(), row.value.get("label")?.as_str()?.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default();
    let author = |entry: &aterm_db::att::Entry| -> Author {
        let machine = entry.machine.as_ref().map(|m| {
            let uuid = aterm_db::typed::keys::uuid32(m);
            labels
                .get(&uuid)
                .cloned()
                .unwrap_or_else(|| uuid.chars().take(8).collect())
        });
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
            machine,
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
        let export = grid::export(compiled.table, collection, &view.cells, &[], false)
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
                    by: author(entry),
                    cells: changed,
                });
            }
        }
        sheets.push(Sheet {
            name,
            title,
            singular,
            columns,
            rows,
            written,
            base,
        });
    }
    Ok(Snapshot {
        sheets,
        read_ms: aterm_time::unix_millis().unwrap_or(0),
    })
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
                    wrote
                        .refused
                        .push((base.collection.clone(), pk, column, problem.rule));
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
pub(crate) fn read(_: &Paths) -> Result<Snapshot, String> {
    Err("the registry is kept on macOS and Linux".into())
}

#[cfg(not(unix))]
pub(crate) fn write(_: &Paths, _: Vec<Staged>) -> Result<Wrote, String> {
    Err("the registry is kept on macOS and Linux".into())
}
