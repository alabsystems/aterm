// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Tables page's one reach into the registry: a read of every collection of the generation in
//! use, listed from the handle's compiled table ([`listed`]), at the head or as of a past
//! operation (`Handle::view_at`) with what changed since (`Handle::diff`), with each cell's
//! candidates and each operation's author, and a write of the staged cells through
//! [`aterm_db::att::Handle::save`], the same save `aterm db table apply` makes. A collection is
//! read by its kind, as `aterm db table scan` reads it: a cells collection as the grid lays it
//! out (`aterm_db::typed::grid::export`: a secret cell names its secret-store entry, and
//! addresses and phone numbers read as stored); an owned one from the view's owned rows, one
//! whole row for each writing Mac, read only. Where the Check view is open, a read also works
//! out what `aterm db alab check` finds (`aterm_db::alab::command::checked`, each finding in
//! the words of its `--tsv` form) and the store's state from the head's usage ([`checked`]).
//! Nothing here runs on the main thread:
//! [`crate::native_tables`]'s worker calls it.

#[cfg(unix)]
use std::collections::BTreeMap;

#[cfg(unix)]
use aterm_db::typed::Faced;
#[cfg(unix)]
use aterm_db::typed::grid::{self, Base};
#[cfg(unix)]
use aterm_db::typed::{Collection, Column, Ty, Writer};
use aterm_db::typed::{Face, Kind, Table};

#[cfg(unix)]
use crate::native_tables::{
    AsOf, Author, AuthorKind, ColumnKind, Group, Observed, Schema, Sheet, SheetColumn, SheetRow,
    Shown, Since, Written, WrittenCell,
};
use crate::native_tables::{Snapshot, Staged, Wrote};

/// One collection the rail lists: the registry's name for it, how a person is shown it, and
/// whether it is read as each Mac's own rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Listed {
    pub(crate) name: &'static str,
    pub(crate) title: &'static str,
    pub(crate) singular: &'static str,
    pub(crate) face: Face,
    /// An owned collection ([`Kind::Owned`]): one whole row for each writing Mac, read only.
    pub(crate) owned: bool,
}

/// Every collection of `table` the rail lists, in the rail's order: the owner's, then what Macs
/// and tools set, then the reference rows ([`Face`]'s order), each face's in the order the
/// table's faces name them. A collection the table names no face for is listed under its own
/// name, the owner's where it is a cells collection and a Mac's where it is owned. A log
/// collection lives in a ring table, and a carried one is read as the rows it carries: neither
/// is listed. Nothing here names a collection: a new one is listed as the table compiles it.
#[cfg_attr(
    not(unix),
    expect(
        dead_code,
        reason = "only the registry read of macOS and Linux lists its tables"
    )
)]
pub(crate) fn listed(table: &Table) -> Vec<Listed> {
    let mut listed: Vec<(usize, usize, Listed)> = table
        .collections
        .iter()
        .enumerate()
        .filter(|(_, collection)| collection.kind != Kind::Log)
        .filter_map(|(at, collection)| {
            let owned = collection.kind == Kind::Owned;
            let faced = table.faced(collection.name);
            let face = faced.map_or(if owned { Face::Mac } else { Face::Owner }, |f| f.face);
            if face == Face::Carried {
                return None;
            }
            let place = table
                .faces
                .iter()
                .position(|f| f.collection == collection.name)
                .unwrap_or(usize::MAX);
            Some((
                place,
                at,
                Listed {
                    name: collection.name,
                    title: faced.map_or(collection.name, |f| f.title),
                    singular: faced.map_or(collection.name, |f| f.singular),
                    face,
                    owned,
                },
            ))
        })
        .collect();
    listed.sort_by_key(|(place, at, listed)| (listed.face, *place, *at));
    listed.into_iter().map(|(_, _, listed)| listed).collect()
}

#[cfg(unix)]
/// Columns the page leaves to later packages: the row's lifecycle (retire and restore) and the
/// import coordinates.
const HIDDEN: [&str; 4] = ["row", "src", "notes_src", "pk"];

#[cfg(unix)]
/// An owned row's counter, which orders a Mac's own statements and says nothing to a person.
const COUNTER: &str = "n";

#[cfg(unix)]
/// The column of an owned sheet that names the Mac whose row it is.
pub(crate) const MAC: &str = "~mac";

/// The words of a column's header: its name in capitals, words apart.
pub(crate) fn label_of(name: &str) -> String {
    name.replace('_', " ").to_uppercase()
}

/// The column group every column of a sheet of `face` sits under where the sheet is not the
/// owner's: the word its rail heading says, so a sheet never reads as another group than the
/// rail puts it in.
#[cfg(unix)]
const fn face_group(face: Face) -> Group {
    match face {
        Face::Owner => Group::Stored,
        Face::Mac | Face::Tool | Face::Carried => Group::SetByAterm,
        Face::Seeded => Group::Reference,
    }
}

/// How column `name` of `collection`, on a sheet of `face`, is shown: its group, how its cells
/// are ordered, and whether it is the owner's column (the only kind the page edits). On the
/// owner's sheets a column sits under who writes it: the owner (STORED), whoever adds the row
/// (SET WHEN ADDED), or aterm (SET BY ATERM); on every other sheet under the sheet's own group
/// ([`face_group`]).
#[cfg(unix)]
fn group_of(collection: &Collection, name: &str, face: Face) -> (Group, ColumnKind, bool) {
    let Some(schema) = collection.column(name) else {
        return (face_group(face), ColumnKind::Text, false);
    };
    let owners = schema.writer == Writer::Owner;
    let group = match (face, schema.writer) {
        (Face::Owner, Writer::Owner) => Group::Stored,
        (Face::Owner, Writer::Birth) => Group::Born,
        (Face::Owner, Writer::Mine | Writer::Seed) => Group::SetByAterm,
        (face, _) => face_group(face),
    };
    // A cells sheet shows a time as its number and a day as its date (`grid::show`), an owned
    // sheet a time as a date too ([`stamp`]): either way their words order as their values do.
    let kind = match schema.ty {
        Ty::Int { .. } | Ty::Uint => ColumnKind::Number,
        Ty::Time | Ty::Day => ColumnKind::Time,
        _ => ColumnKind::Text,
    };
    (group, kind, owners)
}

/// A time (milliseconds) or a day (days since 1970) a Mac stated, as a date on this Mac's clock
/// whose words order as the times do: `2026-11-14 10:23`, `2026-11-14`. None for any other
/// type, or a value that is no such number.
#[cfg(unix)]
fn stamp(ty: &Ty, value: &aterm_json::Value) -> Option<String> {
    match ty {
        Ty::Time => value.as_u64().map(crate::native_tables::stamp_words),
        Ty::Day => value.as_u64().and_then(grid::day_date),
        _ => None,
    }
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
/// How row `pk`'s `column` of `collection` reads in the pane and in history: as the grid shows
/// it, so a secret reference reads as its store entry's name, and one to an entry not the cell's
/// own as the mask ([`grid::show_at`]).
fn shown_value(
    collection: &str,
    column: &Column,
    pk: &str,
    value: Option<&aterm_json::Value>,
) -> String {
    match value {
        None => String::new(),
        Some(value) if value.is_null() => String::new(),
        Some(value) => grid::show_at(collection, column, pk, value),
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

/// The live columns of `collection` in the order `table` lays them out ([`Faced::laid_out`]):
/// as the collection declares them where the table names no face for it.
#[cfg(unix)]
fn laid_out(
    table: &Table,
    collection: &'static Collection,
) -> Vec<&'static aterm_db::typed::Column> {
    match table.faced(collection.name) {
        Some(faced) => faced.laid_out(collection),
        None => collection
            .columns
            .iter()
            .filter(|c| c.live_at(collection.version))
            .collect(),
    }
}

/// The column of a cells sheet that shows each row's own key, where the key is a word a person
/// reads (`codex`, `openai-plus`, `Mac16-8`): never edited, since a row's key is fixed when it is
/// born.
pub(crate) const KEY: &str = "~key";

/// The column that names a row of `collection` among the shown `columns`: the one its display
/// order puts first ([`Faced::naming`]; the row's key, [`KEY`], where that is
/// [`aterm_db::typed::ROW_KEY`]); where the table names none, the first that is unique by itself
/// (a machine's or a mailbox's `label`, an address's `addr`), else its `label`; else, where the
/// collection keys its rows by words (no key prefix: a provider, a CLI, a plan, a Mac model), the
/// row's key, which no other column says; else its `name` (an import's file), else its first
/// column. None where it is the row's key and no column shows it yet: the sheet then adds one.
#[cfg(unix)]
fn key_column(table: &Table, collection: &Collection, columns: &[SheetColumn]) -> Option<String> {
    let shown = |name: &str| {
        columns
            .iter()
            .any(|c| c.name == name && c.group != Group::Linked)
    };
    match table.faced(collection.name).and_then(Faced::naming) {
        Some(aterm_db::typed::ROW_KEY) => return None,
        Some(name) if shown(name) => return Some(name.to_owned()),
        _ => {}
    }
    if let Some(name) = collection
        .unique
        .iter()
        .filter_map(|names| match names {
            [name] => Some(*name),
            _ => None,
        })
        .chain(["label"])
        .find(|name| shown(name))
    {
        return Some(name.to_owned());
    }
    if collection.prefix.is_empty() {
        return None;
    }
    Some(
        ["name"]
            .into_iter()
            .find(|name| shown(name))
            .map(str::to_owned)
            .or_else(|| columns.first().map(|c| c.name.clone()))
            .unwrap_or_default(),
    )
}

/// Read every collection of this Mac's registry the rail lists ([`listed`]), at the generation
/// this Mac uses, as of the operation `at` names (the head where `None`), and, as of a past one,
/// what changed in each since; and, where `check` (a view shows Check), what check finds of the
/// registry and its store's state ([`checked`]). With it, whether the read is whole: every
/// sealed number it asked for opened. One that is not shows a mask a key arriving later would
/// open without moving the registry's [`Mark`], so its reader reads it again.
///
/// # Errors
/// The registry cannot be read here: no Commons, or not subscribed, or its store answered busy
/// past the read's deadline; or this Mac holds no operation `at`.
#[cfg(unix)]
pub(crate) fn read(
    paths: &Paths,
    at: Option<[u8; 32]>,
    check: bool,
) -> Result<(Snapshot, bool), String> {
    use aterm_db::att::{Handle, Point, REGISTRY};
    let handle = Handle::at(paths.clone(), &REGISTRY);
    let compiled = handle.compiled();
    let table = compiled.table;
    // The head, with what its store holds where Check is shown: one read serves both.
    let (head, usage) = if check {
        let (view, usage) = handle.usage().map_err(|e| e.to_string())?;
        (view, Some(usage))
    } else {
        (handle.view().map_err(|e| e.to_string())?, None)
    };
    let past = match at {
        Some(at) => Some(handle.view_at(Point::At(at)).map_err(|e| e.to_string())?),
        None => None,
    };
    let view = past.as_ref().unwrap_or(&head);
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
    let names_view = &head;
    let writers = Writers::new(
        &names_view.project(&aterm_db::alab::registry::MACHINE),
        names_view.owned.get("machine-facts").unwrap_or(&empty),
        &seats(paths),
    );
    let names = aterm_db::alab::registry::Names::of(table, names_view);
    let author = |entry: &aterm_db::att::Entry| -> Author {
        Author {
            kind: match entry.kind {
                aterm_db::att::EntryKind::Aterm => AuthorKind::Aterm,
                aterm_db::att::EntryKind::Import => AuthorKind::Import,
                aterm_db::att::EntryKind::Explicit => AuthorKind::Explicit,
            },
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
    let listed = listed(table);
    let read = Read {
        table,
        view,
        now: past.as_ref().map(|_| &head),
        history: &history,
        by_operation: &by_operation,
        writers: &writers,
        names: &names,
        listed: &listed,
        since: &since,
    };
    let mut projections = BTreeMap::new();
    for shown in listed.iter().filter(|shown| !shown.owned) {
        let collection = compiled.collection(shown.name).map_err(|e| e.to_string())?;
        projections.insert(shown.name, (collection, view.project(collection)));
    }
    let mut sheets = Vec::with_capacity(listed.len());
    let mut whole = true;
    for shown in &listed {
        let collection = compiled.collection(shown.name).map_err(|e| e.to_string())?;
        sheets.push(if shown.owned {
            read.owned_sheet(shown, collection)
        } else {
            let (sheet, opened) = read.cells_sheet(&handle, shown, &projections)?;
            whole &= opened;
            sheet
        });
    }
    let check = usage.map(|usage| Box::new(checked(paths, &handle, &head, &usage, &history)));
    let snapshot = Snapshot {
        listed,
        sheets,
        check,
        at: at.map(|operation| AsOf {
            operation,
            by: by_operation.get(&operation).cloned(),
        }),
        read_ms: aterm_time::unix_millis().unwrap_or(0),
    };
    Ok((snapshot, whole))
}

/// What Check shows of the registry `handle` reads at `paths`, as `head` and its `usage` hold
/// it: each finding `aterm db alab check` reports, apart from this Mac's registration preview
/// (which reads this Mac's own probe), read as the verbs' `--tsv` form reads it
/// ([`aterm_db::att::command::line`]); and the store's state, `history` counting the Macs that
/// wrote it.
///
/// # Errors
/// The registry's members or its phone table cannot be read: the page says so in the view.
#[cfg(unix)]
fn checked(
    paths: &Paths,
    handle: &aterm_db::att::Handle,
    head: &aterm_db::att::View,
    usage: &aterm_db::att::Usage,
    history: &[aterm_db::att::Entry],
) -> Result<crate::native_tables_check::Checked, String> {
    use crate::native_tables_check::{Checked, Found, Store};
    use aterm_db::att::{CAPS, command};
    let found =
        aterm_db::alab::command::checked(paths, handle, head, usage).map_err(|e| e.to_string())?;
    let lines: Vec<command::Line> =
        aterm_db::alab::command::lab_json(&found.lab, &found.seen, &found.duplicates)
            .iter()
            .chain(&found.findings)
            .map(command::line)
            .collect();
    let compiled = handle.compiled();
    let roll = lines
        .iter()
        .find(|line| line.kind == "roll")
        .map(|line| line.detail.clone());
    let writers: std::collections::BTreeSet<&[u8]> = history
        .iter()
        .map(|entry| match &entry.machine {
            Some(machine) => &machine[..],
            None => &entry.actor[..],
        })
        .collect();
    let cannot_write = if aterm_secrets::commons::bootstrap::writer_here(paths) {
        None
    } else {
        Some("it is not a Commons writer here".to_owned())
    };
    let hex = |id: &[u8; 32]| aterm_codec::hex::encode_lower(id);
    let store = Store {
        cannot_write,
        complete: head.complete,
        awaiting: head.awaiting.records,
        records: usage.records,
        records_cap: CAPS.records,
        bytes: usage.bytes,
        bytes_cap: CAPS.bytes,
        mine: usage.mine_records,
        mine_cap: CAPS.writer_records,
        quota_spent: head.quota.spent.get(&usage.actor).copied().unwrap_or(0),
        quota: compiled.table.self_quota,
        refused: head.refused.iter().map(hex).collect(),
        inert: head.quota.over.iter().map(hex).collect(),
        inert_rows: head.owned.values().map(|read| read.inert).sum(),
        generation: compiled.generation,
        table: compiled.name(),
        writers: writers.len(),
        roll,
    };
    Ok(Checked {
        findings: lines
            .into_iter()
            .map(|line| Found {
                kind: line.kind,
                said: line.said,
                collection: line.collection,
                pks: line.pks,
                columns: line.columns,
                detail: line.detail,
            })
            .collect(),
        store,
    })
}

/// What one read of the registry holds, which each sheet is laid out from.
#[cfg(unix)]
struct Read<'a> {
    table: &'static Table,
    view: &'a aterm_db::typed::View,
    /// The head, where `view` is a past table: what its cells hold now.
    now: Option<&'a aterm_db::typed::View>,
    history: &'a [aterm_db::att::Entry],
    by_operation: &'a BTreeMap<[u8; 32], Author>,
    writers: &'a Writers,
    names: &'a aterm_db::alab::registry::Names,
    listed: &'a [Listed],
    since: &'a [aterm_db::att::CellDiff],
}

/// The cells collections' projections, by name.
#[cfg(unix)]
type Projections = BTreeMap<&'static str, (&'static Collection, aterm_db::typed::cell::Projection)>;

/// The column of `collection` a reference in `column` names, where it names a row of one.
#[cfg(unix)]
fn reference(collection: &Collection, column: &str) -> Option<&'static str> {
    collection.column(column).and_then(|c| match c.ty {
        Ty::Ref(target) => Some(target),
        _ => None,
    })
}

#[cfg(unix)]
impl Read<'_> {
    /// The writer of an owned row: the Mac that signed it, whose own statement it is.
    fn mac_author(&self, row: &aterm_db::typed::owned::OwnedRow) -> Author {
        let mut by = self
            .by_operation
            .get(&row.operation)
            .cloned()
            .unwrap_or_else(|| Author {
                kind: AuthorKind::Aterm,
                label: self.writers.label(&row.actor, Some(&row.machine)),
                id: short_id(&row.actor, Some(&row.machine)),
                at_ms: 0,
                hlc: (0, 0),
            });
        by.kind = AuthorKind::Aterm;
        by
    }

    /// An owned collection as each Mac states it: one row for each writing Mac, under a first
    /// column naming that Mac, every cell read only. A reference reads as the words its row is
    /// known by ([`aterm_db::alab::registry::Names`]); each cell's writer is the row's Mac, and
    /// when it saw what it states is the row's own `at`.
    fn owned_sheet(&self, shown: &Listed, collection: &'static Collection) -> Sheet {
        let read = self.view.owned.get(collection.name);
        // Each Mac's row is keyed by the Mac that states it.
        let mut columns = vec![SheetColumn {
            name: MAC.to_owned(),
            label: "STATED BY".to_owned(),
            group: face_group(shown.face),
            editable: false,
            kind: ColumnKind::Text,
            links: Some("machine".to_owned()),
            compiled: None,
            key: true,
        }];
        columns.extend(
            laid_out(self.table, collection)
                .into_iter()
                .filter(|c| c.name != COUNTER)
                .map(|c| {
                    let (group, kind, _) = group_of(collection, c.name, shown.face);
                    SheetColumn {
                        name: c.name.to_owned(),
                        label: label_of(c.name),
                        group,
                        editable: false,
                        kind,
                        links: target(&c.ty).map(str::to_owned),
                        compiled: Some(c),
                        key: c.fixed,
                    }
                }),
        );
        let text_of = |column: &aterm_db::typed::Column, value: Option<&aterm_json::Value>| {
            let Some(value) = value.filter(|v| !v.is_null()) else {
                return String::new();
            };
            match (target(&column.ty), value.as_str()) {
                (Some(to), Some(pk)) => self
                    .names
                    .words(to, pk)
                    .map_or_else(|| pk.to_owned(), str::to_owned),
                _ => stamp(&column.ty, value).unwrap_or_else(|| grid::show(&column.ty, value)),
            }
        };
        let mut rows = Vec::new();
        for (pk, row) in read.map(|r| &r.rows).into_iter().flatten() {
            let by = self.mac_author(row);
            let seen_ms = row.value.get("at").and_then(aterm_json::Value::as_u64);
            let mut cells = BTreeMap::new();
            cells.insert(
                MAC.to_owned(),
                Shown {
                    text: by.label.clone().unwrap_or_else(|| by.id.clone()),
                    by: Some(by.clone()),
                    seen_ms,
                    ..Shown::default()
                },
            );
            for column in collection.columns.iter().filter(|c| c.name != COUNTER) {
                let value = row.value.get(column.name);
                cells.insert(
                    column.name.to_owned(),
                    Shown {
                        text: text_of(column, value),
                        by: Some(by.clone()),
                        seen_ms,
                        names: match (target(&column.ty), value.and_then(|v| v.as_str())) {
                            (Some(to), Some(pk)) => self.names.words(to, pk).map(str::to_owned),
                            _ => None,
                        },
                        ..Shown::default()
                    },
                );
            }
            rows.push(SheetRow {
                pk: pk.clone(),
                cells,
            });
        }
        // Each write of a row, as the cells it changed from the row before it: a Mac states its
        // row whole, so a column it stated again unchanged is no change.
        let mut last: BTreeMap<&str, BTreeMap<&str, String>> = BTreeMap::new();
        let mut written = Vec::new();
        for entry in self.history {
            let mut changed = Vec::new();
            for change in entry
                .changes
                .iter()
                .filter(|c| c.collection == collection.name)
            {
                let before = last.entry(change.key.as_str()).or_default();
                for column in collection.columns.iter().filter(|c| c.name != COUNTER) {
                    let now = text_of(
                        column,
                        change.value.as_ref().and_then(|v| v.get(column.name)),
                    );
                    // A column the row never stated before was empty: stating it empty again
                    // changes nothing.
                    if before.get(column.name).map_or("", String::as_str) != now {
                        changed.push(WrittenCell {
                            pk: change.key.clone(),
                            column: column.name.to_owned(),
                            now: now.clone(),
                        });
                        before.insert(column.name, now);
                    }
                }
            }
            if !changed.is_empty() {
                let mut by = self.by_operation[&entry.operation].clone();
                by.kind = AuthorKind::Aterm;
                written.push(Written {
                    operation: entry.operation,
                    by,
                    cells: changed,
                });
            }
        }
        Sheet {
            name: shown.name,
            title: shown.title,
            singular: shown.singular,
            owned: true,
            key: MAC.to_owned(),
            columns,
            rows,
            written,
            since: BTreeMap::new(),
            observed: BTreeMap::new(),
            base: Base {
                table: format!("{}/{}", self.table.application, self.table.role),
                collection: collection.name.to_owned(),
                version: collection.version,
                header: Vec::new(),
                rows: Vec::new(),
            },
            schema: None,
        }
    }

    /// What each Mac states of the rows of the cells collection `name`, by row and column: an
    /// owned row that references a row of it (the last of its references to that collection
    /// that holds one, so a facts row's `merged_into` before its `mac`) states each of its
    /// columns that the collection also has. Where several rows state one cell, the newest in
    /// the causal order stands ([`Observed::supersedes`]), never the one whose own clock runs
    /// furthest ahead.
    fn observed(
        &self,
        name: &str,
        collection: &Collection,
    ) -> BTreeMap<(String, String), Observed> {
        let mut found: BTreeMap<(String, String), Observed> = BTreeMap::new();
        for shown in self.listed.iter().filter(|shown| shown.owned) {
            let Some(owner) = self.table.collection(shown.name) else {
                continue;
            };
            let refs: Vec<&str> = owner
                .columns
                .iter()
                .filter(|c| target(&c.ty) == Some(name))
                .map(|c| c.name)
                .collect();
            if refs.is_empty() {
                continue;
            }
            for row in self
                .view
                .owned
                .get(shown.name)
                .map(|r| r.rows.values())
                .into_iter()
                .flatten()
            {
                let Some(pk) = refs
                    .iter()
                    .rev()
                    .find_map(|column| row.value.get(*column).and_then(aterm_json::Value::as_str))
                else {
                    continue;
                };
                let by = self.mac_author(row);
                let at_ms = row
                    .value
                    .get("at")
                    .and_then(aterm_json::Value::as_u64)
                    .unwrap_or(by.at_ms);
                for column in owner.columns.iter().filter(|c| {
                    c.name != COUNTER
                        && !refs.contains(&c.name)
                        && !HIDDEN.contains(&c.name)
                        && collection.column(c.name).is_some()
                }) {
                    let Some(value) = row.value.get(column.name).filter(|v| !v.is_null()) else {
                        continue;
                    };
                    let key = (pk.to_owned(), column.name.to_owned());
                    let seen = Observed {
                        text: grid::show(&column.ty, value),
                        by: by.clone(),
                        at_ms,
                        from: shown.title,
                    };
                    if found.get(&key).is_none_or(|held| seen.supersedes(held)) {
                        found.insert(key, seen);
                    }
                }
            }
        }
        found
    }

    /// A cells collection as the grid lays it out, with how many live rows of each other cells
    /// collection name each row, and what each Mac states of its cells ([`Self::observed`]);
    /// and whether every sealed number it asked for opened.
    #[expect(clippy::too_many_lines, reason = "one sheet, laid out in one pass")]
    fn cells_sheet(
        &self,
        handle: &aterm_db::att::Handle,
        shown: &Listed,
        projections: &Projections,
    ) -> Result<(Sheet, bool), String> {
        let name = shown.name;
        let (collection, projection) = &projections[name];
        let collection: &'static Collection = collection;
        // A phone's number is ordinary data a generation builds up to 0.200.0 read holds sealed:
        // it shows as it is, opened with nothing recorded, and stays read-only. Where the open
        // is refused the number shows as the mask and the sheet is still read.
        let (opened, whole) =
            aterm_db::att::open_every_ordinary(handle, collection, &self.view.cells)
                .unwrap_or_default();
        let export = self
            .view
            .export_grid(self.table, collection, &[])
            .map_err(|e| e.to_string())?
            .showing(&opened);
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
                let (group, kind, owners) = group_of(collection, name, shown.face);
                let schema = collection.column(name);
                // The one rule for an edit: the owner's column, which the grid does not hold
                // read only.
                Some(SheetColumn {
                    name: name.to_owned(),
                    label: label_of(name),
                    group,
                    editable: !read_only && owners,
                    kind,
                    links: schema.and_then(|c| target(&c.ty)).map(str::to_owned),
                    compiled: schema,
                    key: schema.is_some_and(|c| c.fixed),
                })
            })
            .collect();
        // The display order compiled beside the collection; a column a newer build wrote after
        // every column this one knows, as the grid lays them out.
        let order: Vec<&str> = laid_out(self.table, collection)
            .into_iter()
            .map(|c| c.name)
            .collect();
        columns.sort_by_key(|c| {
            order
                .iter()
                .position(|name| *name == c.name)
                .unwrap_or(usize::MAX)
        });
        // LINKED ROWS: how many live rows of another cells collection name each row.
        let mut linked: Vec<(String, BTreeMap<String, usize>)> = Vec::new();
        for other in self.listed.iter().filter(|other| !other.owned) {
            let (other_collection, other_projection) = &projections[other.name];
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
                let key = format!("~{}.{}", other.name, column.name);
                linked.push((key.clone(), counts));
                columns.push(SheetColumn {
                    name: key,
                    label: format!(
                        "{} \u{b7} {}",
                        other.title.to_uppercase(),
                        label_of(column.name)
                    ),
                    group: Group::Linked,
                    editable: false,
                    kind: ColumnKind::Number,
                    links: Some(other.name.to_owned()),
                    compiled: None,
                    key: false,
                });
            }
        }
        // A row keyed by words is named by its key, which no column says: a column shows it.
        let key = key_column(self.table, collection, &columns).unwrap_or_else(|| {
            columns.insert(
                0,
                SheetColumn {
                    name: KEY.to_owned(),
                    label: shown.singular.to_uppercase(),
                    group: face_group(shown.face),
                    editable: false,
                    kind: ColumnKind::Text,
                    links: None,
                    compiled: None,
                    key: true,
                },
            );
            KEY.to_owned()
        });
        let mut rows = Vec::new();
        for (pk, seen) in &base.rows {
            let Some(row) = projection.rows.get(pk) else {
                continue;
            };
            if !row.live() {
                continue;
            }
            let mut cells = BTreeMap::new();
            if key == KEY {
                cells.insert(
                    KEY.to_owned(),
                    Shown {
                        text: pk.clone(),
                        ..Shown::default()
                    },
                );
            }
            for column in &columns {
                if let Some((_, counts)) = linked.iter().find(|(n, _)| *n == column.name) {
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
                // A sealed number's candidates as opened, one per candidate in order; none
                // where the cell did not open here, and its candidates then show masked.
                let numbers = opened
                    .get(&(pk.clone(), column.name.clone()))
                    .filter(|numbers| numbers.len() == candidates.len());
                let values: Vec<(String, Option<Author>)> = if candidates.len() > 1 {
                    let mut values: Vec<(String, Option<Author>)> = candidates
                        .iter()
                        .enumerate()
                        .map(|(at, c)| {
                            let text = match (numbers, schema) {
                                (Some(numbers), _) => {
                                    grid::show_opened(std::slice::from_ref(&numbers[at]))
                                }
                                (None, Some(s)) => {
                                    shown_value(collection.name, s, pk, Some(&c.value.x))
                                }
                                (None, None) => seen.shown.clone(),
                            };
                            (text, self.by_operation.get(&c.operation).cloned())
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
                    .and_then(|c| self.by_operation.get(&c.operation).cloned())
                    .or_else(|| {
                        // A set's writer: its newest present member's.
                        seen.elements
                            .values()
                            .flatten()
                            .filter_map(|d| self.by_operation.get(d))
                            .max_by_key(|a| a.at_ms)
                            .cloned()
                    });
                // The words a one-row reference is known by, which the page says beside it.
                let names = reference(collection, &column.name).and_then(|to| {
                    let [one] = &candidates[..] else {
                        return None;
                    };
                    self.names
                        .words(to, one.value.x.as_str()?)
                        .map(str::to_owned)
                });
                cells.insert(
                    column.name.clone(),
                    Shown {
                        text: if values.is_empty() {
                            seen.shown.clone()
                        } else {
                            let mut texts: Vec<&str> =
                                values.iter().map(|v| v.0.as_str()).collect();
                            // Candidates that open to one number are that number, as `aterm db
                            // phones` reads them.
                            if numbers.is_some() {
                                texts.dedup();
                            }
                            texts.join(" / ")
                        },
                        by,
                        values,
                        raw: seen.raw,
                        seen_ms: None,
                        names,
                    },
                );
            }
            rows.push(SheetRow {
                pk: pk.clone(),
                cells,
            });
        }
        let mut written = Vec::new();
        for entry in self.history {
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
                    (Some(cell), None) => {
                        shown_value(collection.name, schema, key.pk, Some(&cell.x))
                    }
                    (None, _) => String::new(),
                };
                changed.push(WrittenCell {
                    pk: key.pk.to_owned(),
                    column: key.col.to_owned(),
                    now,
                });
            }
            if !changed.is_empty() {
                written.push(Written {
                    operation: entry.operation,
                    by: self.by_operation[&entry.operation].clone(),
                    cells: changed,
                });
            }
        }
        // In a past table, a sealed number changed since reads as the grid at Now shows it:
        // opened as the head's cells open here. One that does not open leaves the read not
        // whole, as a number of the table shown does.
        let (opened_now, whole_now) = self.now.map_or_else(
            || (grid::Opened::new(), true),
            |head| {
                aterm_db::att::open_every_ordinary(handle, collection, &head.cells)
                    .unwrap_or_default()
            },
        );
        let sheet = Sheet {
            name,
            title: shown.title,
            singular: shown.singular,
            owned: false,
            key,
            columns,
            rows,
            written,
            since: since_then(collection, self.since, &opened_now, self.by_operation),
            observed: self.observed(name, collection),
            base,
            schema: Some(Schema {
                table: self.table,
                collection,
            }),
        };
        Ok((sheet, whole && whole_now))
    }
}

/// What changed in `collection` since the point a past read is at, by row and column: what the
/// cell holds now as the grid at Now shows it (a set by the members put in or taken out), and
/// who wrote that. A secret column shows the name of the entry its cell names; a number held
/// sealed ([`Column::sealed_ordinary`]) shows as `opened` holds it, the head's cells as they
/// open here, and as the mask where it did not open.
#[cfg(unix)]
fn since_then(
    collection: &'static Collection,
    cells: &[aterm_db::att::CellDiff],
    opened: &grid::Opened,
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
                let numbers = schema
                    .sealed_ordinary()
                    .then(|| opened.get(&(key.pk.to_owned(), key.col.to_owned())))
                    .flatten();
                let mut values: Vec<String> = match numbers {
                    Some(numbers) => numbers
                        .iter()
                        .map(|number| grid::show_opened(std::slice::from_ref(number)))
                        .collect(),
                    None => cell
                        .after
                        .iter()
                        .filter_map(|c| aterm_db::typed::cell::CellValue::decode(&c.value).ok())
                        .map(|c| shown_value(collection.name, schema, key.pk, Some(&c.x)))
                        .collect(),
                };
                values.retain(|text| !text.is_empty());
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

/// Each of `problems` a save of `base`'s grid found, at the row and column it names, with its rule:
/// the page shows the rule at the cell, so a rule's own `<collection>.<column>: ` is not said
/// again.
#[cfg(unix)]
fn placed(base: &Base, problems: Vec<grid::Problem>) -> Vec<((String, String), String)> {
    problems
        .into_iter()
        .map(|problem| {
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
            let said = format!("{}.{column}: ", base.collection);
            let rule = problem
                .rule
                .strip_prefix(&said)
                .map_or(problem.rule.clone(), str::to_owned);
            ((pk, column), rule)
        })
        .collect()
}

/// What the save would refuse of `staged` (by row and column) on `sheet`, by row and column with
/// its rule: the save's own reading of the staged grid ([`grid::plan`], as
/// [`aterm_db::att::Handle::save`] reads it), run as each cell is staged, so a value the save
/// cannot take shows refused before Write. What only the store can judge (the table's rules
/// against the rows it holds, another writer's value since) is still the Write's to find. A
/// staged cell is never a new row, so no row is born here.
#[cfg(unix)]
pub(crate) fn refusals(
    sheet: &Sheet,
    staged: &BTreeMap<(String, String), String>,
) -> BTreeMap<(String, String), String> {
    let Some(schema) = sheet.schema else {
        return BTreeMap::new();
    };
    let unborn = |_: &Collection, _: &BTreeMap<String, aterm_json::Value>| {
        Err(std::io::Error::other("the page stages no new row").into())
    };
    match grid::plan(
        schema.table,
        schema.collection,
        &sheet.base,
        &staged_text(&sheet.base, staged),
        &unborn,
    ) {
        Ok(_) => BTreeMap::new(),
        Err(problems) => placed(&sheet.base, problems).into_iter().collect(),
    }
}

/// No registry is read here, so nothing is staged to check.
#[cfg(not(unix))]
pub(crate) fn refusals(
    _: &crate::native_tables::Sheet,
    _: &std::collections::BTreeMap<(String, String), String>,
) -> std::collections::BTreeMap<(String, String), String> {
    std::collections::BTreeMap::new()
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
                for ((pk, column), rule) in placed(&base, problems) {
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

/// What a read of the registry rests on, read without opening a payload: the table's revision
/// (`aterm_db::att::Handle::revision`) and the seats that name its writers. Two reads at one
/// point under one mark show the same page, so the page's reader reads again only once the mark
/// moves.
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Mark {
    revision: aterm_db::att::Revision,
    seats: Vec<Seat>,
}

/// [`Mark`] of the registry at `paths`.
///
/// # Errors
/// As [`read`].
#[cfg(unix)]
pub(crate) fn mark(paths: &Paths) -> Result<Mark, String> {
    use aterm_db::att::{Handle, REGISTRY};
    let revision = Handle::at(paths.clone(), &REGISTRY)
        .revision()
        .map_err(|e| e.to_string())?;
    Ok(Mark {
        revision,
        seats: seats(paths),
    })
}

#[cfg(not(unix))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Mark {}

#[cfg(not(unix))]
pub(crate) fn mark(_: &Paths) -> Result<Mark, String> {
    Err("the registry is kept on macOS and Linux".into())
}

#[cfg(not(unix))]
pub(crate) fn read(_: &Paths, _: Option<[u8; 32]>, _: bool) -> Result<(Snapshot, bool), String> {
    Err("the registry is kept on macOS and Linux".into())
}

#[cfg(not(unix))]
pub(crate) fn write(_: &Paths, _: Vec<Staged>) -> Result<Wrote, String> {
    Err("the registry is kept on macOS and Linux".into())
}
