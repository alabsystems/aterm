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
//! Each read also lists every row a reference may name and shows each reference as the words its
//! row is known by, its key kept beside them ([`crate::native_tables::rows::targets`]), with
//! which rows a Mac's own statement names ([`held`]) and the label each was written under.
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
use crate::native_tables::rows::{Referrer, RowLife};
#[cfg(unix)]
use crate::native_tables::{
    AsOf, Author, AuthorKind, ColumnKind, Group, Observed, Schema, Sheet, SheetColumn, SheetRow,
    Shown, Since, Written, WrittenCell,
};
use crate::native_tables::{Asked, Opened, Paged, Snapshot, Staged, Wrote};
#[cfg(unix)]
use crate::native_tables::{Logged, LoggedCell, OpenedCell, Valued};

/// The compiled table a sheet was read from, which a new row's key is derived by.
#[cfg(unix)]
pub(crate) type Compiled = aterm_db::att::Compiled;

/// No registry is read here, so no sheet names a compiled table.
#[cfg(not(unix))]
#[derive(Debug)]
pub(crate) enum Compiled {}

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
/// Columns the page does not show: the import coordinates, and the key, which the grid lays out
/// as a column of its own and the page shows where a row is named by it ([`KEY`]).
const HIDDEN: [&str; 3] = ["src", "notes_src", "pk"];

#[cfg(unix)]
/// An owned row's counter, which orders a Mac's own statements and says nothing to a person.
const COUNTER: &str = "n";

#[cfg(unix)]
/// The column of an owned sheet that names the Mac whose row it is.
pub(crate) const MAC: &str = "~mac";

/// The words of a column's header: its name in capitals, words apart. The stored name `mac`
/// (the machine a machine's own facts are of, kept in the generations that hold it) reads
/// `MACHINE`: it names any machine running aterm.
pub(crate) fn label_of(name: &str) -> String {
    if name == "mac" {
        return "MACHINE".to_owned();
    }
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
    // A verb column is the owner's too: the page's save is an owner's write, which the
    // authority rules take there.
    let owners = matches!(schema.writer, Writer::Owner | Writer::Verb);
    let group = match (face, schema.writer) {
        (Face::Owner, Writer::Owner | Writer::Verb) => Group::Stored,
        (Face::Owner, Writer::Birth) => Group::Born,
        (Face::Owner, Writer::Mine | Writer::Seed) => Group::SetByAterm,
        (face, _) => face_group(face),
    };
    // A time reads as its date ([`stamp`]) and a day as its date (`grid::show`), on a cells
    // sheet as on an owned one: their words order as their values do.
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

/// How a value of a cells sheet reads where its words are not the grid's: a time as its date
/// ([`stamp`]), a plan's limits in words ([`limits_words`]). `None` where the grid's text
/// stands.
#[cfg(unix)]
fn set_words(ty: &Ty, value: &aterm_json::Value) -> Option<String> {
    stamp(ty, value).or_else(|| limits_words(ty, value))
}

/// A plan's limits (`aterm_db::alab::registry::LIMIT`) in words, each window as the rest of
/// aterm names it and what it counts: `5-hour: 45 messages · weekly: 225 messages`, a window
/// alone where the plan states no amount (`5-hour · weekly`, its use read in percent), and none
/// where the plan names no window. A limit this build cannot read says its own value, and the
/// others still read in words. `None` for any other type, or a value that is no list.
#[cfg(unix)]
fn limits_words(ty: &Ty, value: &aterm_json::Value) -> Option<String> {
    let Ty::List(Ty::Struct(members), _) = ty else {
        return None;
    };
    if *members != aterm_db::alab::registry::LIMIT {
        return None;
    }
    let said: Vec<String> = value
        .as_array()?
        .iter()
        .map(|limit| limit_words(limit).unwrap_or_else(|| limit.to_string()))
        .collect();
    Some(said.join(" \u{b7} "))
}

/// One limit of a plan in words ([`limits_words`]); `None` where it names no window.
#[cfg(unix)]
fn limit_words(limit: &aterm_json::Value) -> Option<String> {
    let model = limit
        .get("model")
        .and_then(aterm_json::Value::as_str)
        .filter(|model| !model.is_empty());
    let window = match (limit.get("w")?.as_str()?, model) {
        ("5h", None) => "5-hour".to_owned(),
        ("7d", None) => "weekly".to_owned(),
        ("7d-model", None) => "one model's weekly".to_owned(),
        ("5h", Some(model)) => format!("{model} 5-hour"),
        ("7d" | "7d-model", Some(model)) => format!("{model} weekly"),
        ("day", None) => "daily".to_owned(),
        ("month", None) => "monthly".to_owned(),
        ("day", Some(model)) => format!("{model} daily"),
        ("month", Some(model)) => format!("{model} monthly"),
        (other, None) => other.to_owned(),
        (other, Some(model)) => format!("{model} {other}"),
    };
    let unit = limit.get("unit").and_then(aterm_json::Value::as_str);
    let counts = match (limit.get("nominal"), unit) {
        (None | Some(aterm_json::Value::Null), None | Some("pct")) => return Some(window),
        (None | Some(aterm_json::Value::Null), Some("usd-cents")) => "in dollars".to_owned(),
        (None | Some(aterm_json::Value::Null), Some(unit)) => format!("in {unit}"),
        (Some(nominal), unit) => match (nominal.as_u64(), unit) {
            (Some(n), Some("pct")) => format!("{n}%"),
            (Some(cents), Some("usd-cents")) => format!("${}.{:02}", cents / 100, cents % 100),
            (Some(n), Some(unit)) => format!("{n} {unit}"),
            (Some(n), None) => n.to_string(),
            (None, Some(unit)) => format!("{nominal} {unit}"),
            (None, None) => nominal.to_string(),
        },
    };
    Some(format!("{window}: {counts}"))
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

/// What the page calls a writer: the machine row its Mac's facts bind, by that row's name
/// ([`aterm_db::att::Mac::name`]); where none binds, no label, and the start of its Commons
/// machine id (of its actor where the roster binds no machine) for the page to show dimmed.
#[cfg(unix)]
fn named(mac: &aterm_db::att::Mac) -> (Option<String>, String) {
    use aterm_db::att::Mac;
    match mac {
        Mac::Row { key, .. } => (Some(mac.name()), key.clone()),
        Mac::Machine(machine) => (None, aterm_codec::hex::encode_lower(&machine[..4])),
        Mac::Actor(actor) => (None, aterm_codec::hex::encode_lower(&actor[..4])),
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
    // A writer is named as history names its Mac now (`Entry::mac`), in a past table too:
    // history runs past the point, and a Mac labelled since is still that Mac. An owned row
    // whose record history does not list is named by the same join, read from the head.
    let macs = aterm_db::att::Macs::of(&head);
    let names = aterm_db::alab::registry::Names::of(table, &head);
    let author = |entry: &aterm_db::att::Entry| -> Author {
        let (label, id) = named(&entry.mac);
        Author {
            kind: match entry.kind {
                aterm_db::att::EntryKind::Aterm => AuthorKind::Aterm,
                aterm_db::att::EntryKind::Import => AuthorKind::Import,
                aterm_db::att::EntryKind::Explicit => AuthorKind::Explicit,
            },
            label,
            id,
            at_ms: entry.authored_ms,
            hlc: (entry.hlc.ms, entry.hlc.counter),
        }
    };
    let by_operation: BTreeMap<[u8; 32], Author> = history
        .iter()
        .map(|entry| (entry.operation, author(entry)))
        .collect();
    let listed = listed(table);
    // Who names each row, by the rule a retire of it is refused by, and the cells a new row's key
    // is derived from: both as the read found the table.
    let referrers = aterm_db::typed::check::referrers(table, &[], &view.cells);
    let cells = std::sync::Arc::new(view.cells.clone());
    let read = Read {
        compiled,
        table,
        view,
        now: past.as_ref().map(|_| &head),
        history: &history,
        by_operation: &by_operation,
        macs: &macs,
        names: &names,
        listed: &listed,
        since: &since,
        referrers: &referrers,
        cells: &cells,
    };
    let mut projections = BTreeMap::new();
    for shown in listed.iter().filter(|shown| !shown.owned) {
        let collection = compiled.collection(shown.name).map_err(|e| e.to_string())?;
        let (projection, settled) = view.project_settled(collection);
        projections.insert(
            shown.name,
            (collection, projection, std::sync::Arc::new(settled)),
        );
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
    // Each row a reference may name, and every reference shown as the words its row is known
    // by: a link is chosen from rows and stored by key, never written by a label.
    let targets = crate::native_tables::rows::targets(
        &sheets,
        |to, pk| names.words(to, pk).map(str::to_owned),
        |to, pk| written_label(&projections, to, pk),
        &held(table, view, &listed),
    );
    crate::native_tables::rows::name_references(&mut sheets, &targets);
    let check = usage.map(|usage| Box::new(checked(paths, &handle, &head, &usage, &history)));
    let cannot_write = handle.cannot_write(&head);
    let snapshot = Snapshot {
        listed,
        sheets,
        check,
        at: at.map(|operation| AsOf {
            operation,
            by: by_operation.get(&operation).cloned(),
        }),
        read_ms: aterm_time::unix_millis().unwrap_or(0),
        cannot_write,
        targets,
    };
    Ok((snapshot, whole))
}

/// The label row `pk` of collection `to` was written under, where `to` claims its labels
/// ([`aterm_db::typed::ClaimRule::Label`]): its value as written, not the claim it reads as.
#[cfg(unix)]
fn written_label(projections: &Projections, to: &str, pk: &str) -> Option<String> {
    let (collection, projection, _) = projections.get(to)?;
    let column = collection
        .columns
        .iter()
        .find(|c| c.claim == Some(aterm_db::typed::ClaimRule::Label))?;
    projection
        .rows
        .get(pk)?
        .fields
        .get(column.name)?
        .value()?
        .as_str()
        .map(str::to_owned)
}

/// Each row a Mac holds, by collection and key: the row a Mac's own statement names, read as
/// [`Read::observed`] reads it (of each table its row names, the last of its references to that
/// table that holds one, so a facts row's `merged_into` before its `mac`).
#[cfg(unix)]
fn held(
    table: &Table,
    view: &aterm_db::typed::View,
    listed: &[Listed],
) -> std::collections::BTreeSet<(&'static str, String)> {
    let mut held = std::collections::BTreeSet::new();
    for shown in listed.iter().filter(|shown| shown.owned) {
        let (Some(owner), Some(read)) = (table.collection(shown.name), view.owned.get(shown.name))
        else {
            continue;
        };
        for row in read.rows.values() {
            let mut named = std::collections::BTreeSet::new();
            for column in owner.columns.iter().rev() {
                let Ty::Ref(to) = column.ty else {
                    continue;
                };
                if named.contains(to) {
                    continue;
                }
                if let Some(pk) = row
                    .value
                    .get(column.name)
                    .and_then(aterm_json::Value::as_str)
                {
                    named.insert(to);
                    held.insert((to, pk.to_owned()));
                }
            }
        }
    }
    held
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
    let cannot_write = handle.cannot_write(head);
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
    compiled: &'static Compiled,
    table: &'static Table,
    view: &'a aterm_db::typed::View,
    /// The head, where `view` is a past table: what its cells hold now.
    now: Option<&'a aterm_db::typed::View>,
    history: &'a [aterm_db::att::Entry],
    by_operation: &'a BTreeMap<[u8; 32], Author>,
    macs: &'a aterm_db::att::Macs,
    names: &'a aterm_db::alab::registry::Names,
    listed: &'a [Listed],
    since: &'a [aterm_db::att::CellDiff],
    /// The live rows that name each row, by collection and key
    /// ([`aterm_db::typed::check::referrers`]).
    referrers: &'a BTreeMap<(&'static str, String), Vec<(&'static str, String)>>,
    cells: &'a std::sync::Arc<aterm_db::typed::cell::Cells>,
}

/// The cells collections' projections, by name, each with how its claimed columns settled.
#[cfg(unix)]
type Projections = BTreeMap<
    &'static str,
    (
        &'static Collection,
        aterm_db::typed::cell::Projection,
        std::sync::Arc<aterm_db::typed::claim::Settlements>,
    ),
>;

#[cfg(unix)]
impl Read<'_> {
    /// The writer of an owned row: the Mac that signed it, whose own statement it is.
    fn mac_author(&self, row: &aterm_db::typed::owned::OwnedRow) -> Author {
        let mut by = self
            .by_operation
            .get(&row.operation)
            .cloned()
            .unwrap_or_else(|| {
                let (label, id) = named(&self.macs.name(&row.actor, Some(&row.machine)));
                Author {
                    kind: AuthorKind::Aterm,
                    label,
                    id,
                    at_ms: 0,
                    hlc: (0, 0),
                }
            });
        by.kind = AuthorKind::Aterm;
        by
    }

    /// An owned collection as each Mac states it: one row for each writing Mac, under a first
    /// column naming that Mac, every cell read only. A reference holds its row's key, and its
    /// history reads as the words its row is known by ([`aterm_db::alab::registry::Names`]);
    /// each cell's writer is the row's Mac, and when it saw what it states is the row's own `at`.
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
                // A reference holds its row's key here; the read then says its row's words
                // ([`crate::native_tables::rows::name_references`]).
                let text = row
                    .value
                    .get(column.name)
                    .filter(|v| !v.is_null())
                    .map(|value| {
                        stamp(&column.ty, value).unwrap_or_else(|| grid::show(&column.ty, value))
                    })
                    .unwrap_or_default();
                cells.insert(
                    column.name.to_owned(),
                    Shown {
                        text,
                        by: Some(by.clone()),
                        seen_ms,
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
            life: None,
            referrers: BTreeMap::new(),
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
        let (collection, projection, settled) = &projections[name];
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
                // read only. A row's lifecycle is not typed: the pane retires, ends and restores
                // it, after naming what still names the row.
                Some(SheetColumn {
                    name: name.to_owned(),
                    label: label_of(name),
                    group,
                    editable: !read_only && owners && name != aterm_db::typed::LIFECYCLE,
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
            let (other_collection, other_projection, _) = &projections[other.name];
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
        // Every row, whatever its lifecycle: the page's lifecycle filter picks which it shows.
        for (pk, seen) in &base.rows {
            let Some(row) = projection.rows.get(pk) else {
                continue;
            };
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
                // A value reads in words (a time as its date, a plan's limits), whoever writes
                // it; a cell the page edits keeps the grid's text as what it holds.
                let words = schema
                    .zip(candidates.first())
                    .filter(|_| candidates.len() == 1)
                    .and_then(|(schema, candidate)| set_words(&schema.ty, &candidate.value.x));
                let holds = words
                    .as_ref()
                    .filter(|_| column.editable)
                    .map(|_| seen.shown.clone());
                cells.insert(
                    column.name.clone(),
                    Shown {
                        text: if let Some(words) = words {
                            words
                        } else if values.is_empty() {
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
                        refers: None,
                        holds,
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
                    (Some(cell), None) => set_words(&schema.ty, &cell.x).unwrap_or_else(|| {
                        shown_value(collection.name, schema, key.pk, Some(&cell.x))
                    }),
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
        let life = self.life(collection, &base);
        let referrers = self
            .referrers
            .range((name, String::new())..)
            .take_while(|((of, _), _)| *of == name)
            .map(|((_, pk), by)| {
                let by = by
                    .iter()
                    .map(|(sheet, pk)| Referrer {
                        sheet,
                        pk: pk.clone(),
                    })
                    .collect();
                (pk.clone(), by)
            })
            .collect();
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
                compiled: self.compiled,
                collection,
                cells: self.cells.clone(),
                settled: settled.clone(),
            }),
            life,
            referrers,
        };
        Ok((sheet, whole && whole_now))
    }

    /// How rows of `collection`, laid out as `base` is, are added, retired, ended and restored:
    /// its lifecycle as the grid writes it ([`grid::lifecycle`]); whether a new row is born here
    /// or who adds one ([`aterm_db::alab::registry::born_elsewhere`]); the columns a new row is
    /// typed into, every one the grid does not hold read only; and those of them only a verb
    /// sets, with that verb ([`aterm_db::alab::registry::set_by`]). `None` where it has no
    /// lifecycle.
    fn life(&self, collection: &'static Collection, base: &Base) -> Option<RowLife> {
        let lifecycle = grid::lifecycle(self.table, collection)?;
        let mut born_columns = Vec::new();
        let mut set_by = Vec::new();
        for header in &base.header {
            let name = header.trim_start_matches(grid::READ_ONLY);
            if collection.column(name).is_none() || name == aterm_db::typed::LIFECYCLE {
                continue;
            }
            if !header.starts_with(grid::READ_ONLY) {
                born_columns.push(name.to_owned());
            } else if let Some(verb) = aterm_db::alab::registry::set_by(collection.name, name) {
                set_by.push((name.to_owned(), verb));
            }
        }
        Some(RowLife {
            words: lifecycle.words,
            ends: lifecycle.ends,
            born_elsewhere: aterm_db::alab::registry::born_elsewhere(collection.name),
            born_columns,
            set_by,
        })
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

/// Each of `problems` a save of `base`'s grid found, at the row and column it names, with its rule:
/// the page shows the rule at the cell, so a rule's own `<collection>.<column>: ` is not said
/// again. A problem of a new row is placed at the key the page gave it, `born`'s in the order
/// the grid text holds them ([`grid::staged_text`]).
#[cfg(unix)]
fn placed(
    base: &Base,
    born: &[String],
    problems: Vec<grid::Problem>,
) -> Vec<((String, String), String)> {
    problems
        .into_iter()
        .map(|problem| {
            let record = grid::RecordOf::of(base, problem.record);
            let pk = match record {
                grid::RecordOf::Header => String::new(),
                grid::RecordOf::Row(pk) => pk.to_owned(),
                grid::RecordOf::Born(n) => born.get(n).cloned().unwrap_or_default(),
            };
            let mut column = base
                .header
                .get(problem.column)
                .map(|h| h.trim_start_matches(grid::READ_ONLY).to_owned())
                .unwrap_or_default();
            let said = format!("{}.{column}: ", base.collection);
            let mut rule = problem
                .rule
                .strip_prefix(&said)
                .map_or(problem.rule.clone(), str::to_owned);
            // What stops a new row's key is said at the cell of the column the rule names (its
            // key is made from a column left empty, `preference.account: …`), else at the cell
            // that says the row is new (a row the table holds already).
            if matches!(record, grid::RecordOf::Born(_)) && column == grid::PK {
                let named = base
                    .header
                    .iter()
                    .map(|h| h.trim_start_matches(grid::READ_ONLY))
                    .find_map(|c| {
                        let rest = problem
                            .rule
                            .strip_prefix(&format!("{}.{c}: ", base.collection))?;
                        Some((c, rest))
                    });
                match named {
                    Some((c, rest)) => {
                        c.clone_into(&mut column);
                        rest.clone_into(&mut rule);
                    }
                    None => {
                        aterm_db::typed::LIFECYCLE.clone_into(&mut column);
                        if let Some(rest) = rule.strip_prefix(&format!("{}: ", base.collection)) {
                            rule = rest.to_owned();
                        }
                    }
                }
            }
            ((pk, column), rule)
        })
        .collect()
}

/// `staged` (by row and column) split into the cells of `base`'s rows and the new rows, each
/// new row's key ([`crate::native_tables::rows::NEW`]) with its cells by column, in key order.
#[cfg(unix)]
fn born_of(
    staged: &BTreeMap<(String, String), String>,
) -> (
    BTreeMap<(String, String), String>,
    Vec<(String, BTreeMap<String, String>)>,
) {
    let mut cells = BTreeMap::new();
    let mut born: Vec<(String, BTreeMap<String, String>)> = Vec::new();
    for ((pk, column), text) in staged {
        if !crate::native_tables::rows::is_new(pk) {
            cells.insert((pk.clone(), column.clone()), text.clone());
            continue;
        }
        match born.iter_mut().find(|(key, _)| key == pk) {
            Some((_, row)) => {
                row.insert(column.clone(), text.clone());
            }
            None => born.push((pk.clone(), BTreeMap::from([(column.clone(), text.clone())]))),
        }
    }
    (cells, born)
}

/// The grid text a save of `base` reads for `cells` and the new rows `born`, and the new rows'
/// keys in the order it holds them.
#[cfg(unix)]
fn text_of(
    base: &Base,
    cells: &BTreeMap<(String, String), String>,
    born: &[(String, BTreeMap<String, String>)],
) -> (String, Vec<String>) {
    let rows: Vec<BTreeMap<String, String>> = born.iter().map(|(_, row)| row.clone()).collect();
    (
        grid::staged_text(base, cells, &rows),
        born.iter().map(|(key, _)| key.clone()).collect(),
    )
}

/// What the save's own reading of a staged grid found ([`refusals`]).
#[derive(Debug, Default)]
pub(crate) struct Reading {
    /// What it would refuse, by row and column, with the rule.
    pub(crate) refused: std::collections::BTreeMap<(String, String), String>,
    /// Each new row's key as the save would derive it from its cells now, by the key the page
    /// gives it ([`crate::native_tables::rows::NEW`]): where nothing refuses the grid.
    pub(crate) keys: std::collections::BTreeMap<String, String>,
    /// The record of one Write of the grid that carries each cell it writes, by row key (a new
    /// row's as [`Self::keys`] has it) and column, as the save appends them where the Write lands
    /// on the read ([`aterm_db::att::Compiled::appended`]): a cell it leaves out writes nothing.
    /// `None` where the grid is refused, or the save's own rules would refuse the Write.
    pub(crate) records: Option<std::collections::BTreeMap<(String, String), usize>>,
}

/// What the save would refuse of `staged` (by row and column) on `sheet`, by row and column with
/// its rule, and the key each new row would be born under: the save's own reading of the staged
/// grid ([`grid::plan`], as [`aterm_db::att::Handle::save`] reads it), run as each cell is
/// staged, so a value the save cannot take shows refused before Write. A new row's key is
/// derived as the save derives it ([`aterm_db::att::Compiled::born`]), from the table as the read
/// found it. What only the store can judge (the table's rules against the rows it holds, another
/// writer's value since) is still the Write's to find.
#[cfg(unix)]
pub(crate) fn refusals(sheet: &Sheet, staged: &BTreeMap<(String, String), String>) -> Reading {
    let Some(schema) = &sheet.schema else {
        return Reading::default();
    };
    let born = |collection: &Collection, values: &BTreeMap<String, aterm_json::Value>| {
        schema.compiled.born(collection, values, &schema.cells)
    };
    let (cells, new) = born_of(staged);
    let (text, keys) = text_of(&sheet.base, &cells, &new);
    match grid::plan(
        schema.compiled.table,
        schema.collection,
        &sheet.base,
        &text,
        &born,
    ) {
        // A key made at random as the row is written (a phone's) is not this one.
        Ok(_) if aterm_db::alab::registry::born_at_random(schema.collection.name) => {
            Reading::default()
        }
        Ok(plan) => Reading {
            refused: BTreeMap::new(),
            records: carried(schema, &plan),
            keys: keys.into_iter().zip(plan.born).collect(),
        },
        Err(problems) => Reading {
            refused: placed(&sheet.base, &keys, problems).into_iter().collect(),
            ..Reading::default()
        },
    }
}

/// The record of one Write of `plan` that carries each cell it writes, by row key and column:
/// the records the save appends where it lands on the read `schema` holds
/// ([`aterm_db::att::Compiled::appended`]); `None` where the save's rules refuse it or a row
/// passes a record's limits.
#[cfg(unix)]
fn carried(schema: &Schema, plan: &grid::Plan) -> Option<BTreeMap<(String, String), usize>> {
    let records = schema
        .compiled
        .appended(schema.collection, plan, &schema.cells)
        .ok()??;
    let mut out = BTreeMap::new();
    for (record, changes) in records.iter().enumerate() {
        for change in changes {
            if change.collection != schema.collection.name {
                continue;
            }
            if let Ok(key) = aterm_db::typed::cell::CellKey::parse(&change.key) {
                out.insert((key.pk.to_owned(), key.col.to_owned()), record);
            }
        }
    }
    Some(out)
}

/// No registry is read here, so nothing is staged to check.
#[cfg(not(unix))]
pub(crate) fn refusals(
    _: &crate::native_tables::Sheet,
    _: &std::collections::BTreeMap<(String, String), String>,
) -> Reading {
    Reading::default()
}

/// Write each collection's staged cells and new rows, each collection in one save.
///
/// # Errors
/// A save could not reach the registry (no Commons writer, an incomplete history here, a busy
/// store past the deadline).
#[cfg(unix)]
pub(crate) fn write(paths: &Paths, staged: Vec<Staged>) -> Result<Wrote, String> {
    use aterm_db::att::{Handle, REGISTRY, Saved};
    let handle = Handle::at(paths.clone(), &REGISTRY);
    let mut wrote = Wrote::default();
    for Staged { base, cells, born } in staged {
        let (text, keys) = text_of(&base, &cells, &born);
        // A table that cannot be saved is named and the rest are still written.
        let saved = handle
            .compiled()
            .collection(&base.collection)
            .and_then(|collection| handle.save(collection, &base, &text));
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
            Saved::Written { plan, applied } => {
                wrote.cells += applied.cells;
                wrote.added += applied.born.len();
                // The plan holds a key for each new row, in the order the grid text holds them.
                wrote.born.extend(
                    keys.iter()
                        .zip(&plan.born)
                        .map(|(new, pk)| (base.collection.clone(), new.clone(), pk.clone())),
                );
                wrote.kept.extend(
                    applied
                        .stale
                        .into_iter()
                        .map(|s| (base.collection.clone(), s.pk, s.column)),
                );
            }
            Saved::Problems(problems) => {
                for ((pk, column), rule) in placed(&base, &keys, problems) {
                    wrote
                        .refused
                        .push((base.collection.clone(), pk, column, rule));
                }
            }
        }
    }
    Ok(wrote)
}

/// The kind History's page names for `kind`, as `aterm-db` decides it.
#[cfg(unix)]
const fn author_kind(kind: aterm_db::att::EntryKind) -> AuthorKind {
    match kind {
        aterm_db::att::EntryKind::Aterm => AuthorKind::Aterm,
        aterm_db::att::EntryKind::Import => AuthorKind::Import,
        aterm_db::att::EntryKind::Explicit => AuthorKind::Explicit,
    }
}

/// A column's words, as the drawer and the pane say a field: `Location`, `Ram gb`.
#[cfg(unix)]
fn column_words(name: &str) -> String {
    crate::native_tables::sentence(&label_of(name))
}

/// What one stored change of `table` says in History: the row it wrote, its column where it is
/// a typed cell, and what changed in words, the value as the grid shows it (a secret reference
/// as its store entry's name, never a value: [`shown_value`]). An owned or log row's write is
/// one whole-row change.
#[cfg(unix)]
fn change_words(table: &Table, change: &aterm_db::database::Change) -> LoggedCell {
    use aterm_db::typed::cell::{CellKey, CellValue};
    let whole = || LoggedCell {
        collection: change.collection.clone(),
        pk: change.key.clone(),
        column: None,
        words: "whole row".to_owned(),
    };
    let Some(collection) = table.collection(&change.collection) else {
        return whole();
    };
    if collection.kind != Kind::Cells {
        return whole();
    }
    let Ok(key) = CellKey::parse(&change.key) else {
        return whole();
    };
    let words = column_words(key.col);
    let value = change
        .value
        .as_ref()
        .and_then(|value| CellValue::decode(value).ok());
    let now = match (collection.column(key.col), &value, key.elem) {
        (_, Some(cell), Some(elem)) if cell.x.as_bool() == Some(true) => format!("+ {elem}"),
        (_, Some(_), Some(elem)) => format!("\u{2212} {elem}"),
        (Some(schema), Some(cell), None) => set_words(&schema.ty, &cell.x)
            .unwrap_or_else(|| shown_value(collection.name, schema, key.pk, Some(&cell.x))),
        // A column this build does not compile: what it holds is not said.
        (None, Some(_), None) => "a value this build cannot read".to_owned(),
        (_, None, _) => String::new(),
    };
    let now = if now.is_empty() {
        "empty".to_owned()
    } else {
        now
    };
    LoggedCell {
        collection: change.collection.clone(),
        pk: key.pk.to_owned(),
        column: Some(key.col.to_owned()),
        words: format!("{words} \u{2192} {now}"),
    }
}

/// One record of `table`'s history as History lists it.
#[cfg(unix)]
fn logged(table: &Table, entry: &aterm_db::att::Entry) -> Logged {
    let (label, id) = named(&entry.mac);
    Logged {
        operation: entry.operation,
        by: Author {
            kind: author_kind(entry.kind),
            label,
            id,
            at_ms: entry.authored_ms,
            hlc: (entry.hlc.ms, entry.hlc.counter),
        },
        machine: entry.mac.row().map(str::to_owned),
        inert: entry.inert.is_some(),
        cells: entry
            .changes
            .iter()
            .map(|change| change_words(table, change))
            .collect(),
    }
}

/// What History's reader keeps between its reads ([`history`]): the registry's ordered history at
/// one revision, so a page at that revision reads no record again. One revision of one table,
/// replaced when the registry moves; the reader holds it for as long as it reads.
#[cfg(unix)]
pub(crate) type Kept = aterm_db::att::Kept;

/// Where the registry is not kept, History reads nothing to keep.
#[cfg(not(unix))]
#[derive(Debug, Default)]
pub(crate) struct Kept;

/// One page of History ([`crate::native_tables::Asked`]): the registry's records newest first,
/// at least `asked.limit` of those its filters keep where so many are left, read a page at a time
/// (`aterm_db::att::Handle::history_page`, newest first). The table and kind filters are the
/// store's; where a machine or Find is asked, the records the store serves are judged here,
/// until the page is full or [`crate::native_tables::SCAN`] records were looked at: the next
/// page then goes on from the last record looked at. Where `asked.through` names a record, the
/// page holds every record the filters keep from the newest through it, whatever the bounds, so a
/// read again keeps every line that showed: the store's pages are read on until it is passed. A
/// store page that finds the registry moved starts from the newest record again, and the page
/// says so. Every store page is cut from `kept`, the registry's ordered history at the revision
/// it holds, where the registry is still at that revision: Read older, a filter changed and each
/// store page after the first then read no record again
/// (`aterm_db::att::Handle::history_page_kept`).
///
/// # Errors
/// As [`read`].
#[cfg(unix)]
pub(crate) fn history(paths: &Paths, asked: &Asked, kept: &mut Kept) -> Result<Paged, String> {
    use crate::native_tables::{SCAN, place_words, words_of};
    use aterm_db::att::history::{Ask, Cursor, Filter, Revision};
    use aterm_db::att::{Handle, Hlc, REGISTRY};
    let handle = Handle::at(paths.clone(), &REGISTRY);
    let table = handle.compiled().table;
    let tables = history_tables(table);
    let title = |name: &str| {
        tables.iter().find(|(each, _)| each == name).map_or_else(
            || crate::native_tables::sentence(name),
            |(_, title)| title.clone(),
        )
    };
    let row = |collection: &str, pk: &str| {
        asked
            .names
            .get(&(collection.to_owned(), pk.to_owned()))
            .cloned()
            .unwrap_or_else(|| pk.to_owned())
    };
    let judges = asked.filter.reads();
    let cursor = |(ms, counter, operation): (u64, u64, [u8; 32])| Cursor {
        hlc: Hlc { ms, counter },
        operation,
    };
    let through = asked.through.map(cursor);
    let mut ask = Ask {
        filter: Filter {
            collection: asked.filter.table.clone(),
            kind: asked.filter.kind.map(|kind| match kind {
                AuthorKind::Aterm => aterm_db::att::EntryKind::Aterm,
                AuthorKind::Import => aterm_db::att::EntryKind::Import,
                AuthorKind::Explicit => aterm_db::att::EntryKind::Explicit,
            }),
            ..Filter::default()
        },
        after: asked.before.map(cursor),
        // Judged here, a store page is as long as the bound, so one read most often answers.
        limit: if judges { SCAN } else { asked.limit.max(1) },
        revision: asked.revision.map(Revision),
        newest_first: true,
    };
    let mut records = Vec::new();
    let mut looked = 0;
    let mut moved = false;
    loop {
        let page = handle
            .history_page_kept(&ask, kept)
            .map_err(|e| e.to_string())?;
        if page.moved {
            // Asked at an older revision, or moved between two store pages of this read: the
            // store starts from the newest record, and what this read kept goes.
            moved = true;
            records.clear();
            looked = 0;
        }
        let last = page.entries.len().saturating_sub(1);
        let mut stopped = None;
        for (at, entry) in page.entries.iter().enumerate() {
            looked += 1;
            let record = logged(table, entry);
            let keeps = !judges || {
                let (place, changed) = place_words(&record, title, row);
                asked
                    .filter
                    .keeps(&record, &words_of(&record, &place, &changed))
            };
            if keeps {
                records.push(record);
            }
            let passed = through.is_none_or(|through| entry.cursor() <= through);
            if passed && (records.len() >= asked.limit || (judges && looked >= SCAN)) {
                stopped = Some((at, entry.cursor()));
                break;
            }
        }
        if stopped.is_some() || page.next.is_none() {
            let next = match stopped {
                Some((at, _)) if at == last && page.next.is_none() => None,
                Some((_, cursor)) => Some(cursor),
                None => None,
            };
            return Ok(Paged {
                records,
                next: next.map(|next| (next.hlc.ms, next.hlc.counter, next.operation)),
                revision: page.revision.0,
                moved,
                tables,
            });
        }
        // The store's page ended short of the bounds, or of the record asked through.
        ask.after = page.next;
        ask.revision = Some(page.revision);
    }
}

/// Every collection of `table` History can list records of, by name and title: the rail's
/// ([`listed`]) in its order, then every other (a log, a carried face such as `archived`) in the
/// table's order, by its face's title where it has one, else its name in words.
#[cfg(unix)]
pub(crate) fn history_tables(table: &Table) -> Vec<(String, String)> {
    let listed = listed(table);
    let mut tables: Vec<(String, String)> = listed
        .iter()
        .map(|listed| (listed.name.to_owned(), listed.title.to_owned()))
        .collect();
    tables.extend(
        table
            .collections
            .iter()
            .filter(|collection| !listed.iter().any(|l| l.name == collection.name))
            .map(|collection| {
                let title = table.faced(collection.name).map_or_else(
                    || crate::native_tables::sentence(collection.name),
                    |faced| faced.title.to_owned(),
                );
                (collection.name.to_owned(), title)
            }),
    );
    tables
}

/// What record `operation` of the registry wrote, opened for History: each cell with what it
/// wrote, what that replaced, the writes beside it it had not seen and what has written there
/// since (`aterm_db::att::Handle::wrote_named`), each value as the grid shows it and each writer
/// by its machine's name.
///
/// # Errors
/// As [`read`]; or this machine holds no such eligible record.
#[cfg(unix)]
pub(crate) fn record(paths: &Paths, operation: [u8; 32]) -> Result<Opened, String> {
    use aterm_db::att::{Handle, REGISTRY};
    let handle = Handle::at(paths.clone(), &REGISTRY);
    let table = handle.compiled().table;
    let (wrote, macs) = handle.wrote_named(operation).map_err(|e| e.to_string())?;
    let cells = wrote
        .iter()
        .map(|cell| {
            let place = change_words(
                table,
                &aterm_db::database::Change {
                    collection: cell.collection.clone(),
                    key: cell.key.clone(),
                    value: None,
                },
            );
            // What an owned or log row's write replaced, where it replaced one row: what it
            // wrote says which fields it changed.
            let over = match cell.replaced.as_slice() {
                [one] if !one.withheld => one.value.as_ref(),
                _ => None,
            };
            let whole = place.column.is_none();
            let words = |held: &aterm_db::att::Held, over: Option<&aterm_json::Value>| -> String {
                if held.withheld {
                    return "a secret, not shown".to_owned();
                }
                let Some(value) = &held.value else {
                    return "nothing".to_owned();
                };
                if whole {
                    return whole_row_words(value, over);
                }
                change_words(
                    table,
                    &aterm_db::database::Change {
                        collection: cell.collection.clone(),
                        key: cell.key.clone(),
                        value: Some(value.clone()),
                    },
                )
                .words
                .rsplit_once(" \u{2192} ")
                .map_or_else(|| "the whole row".to_owned(), |(_, now)| now.to_owned())
            };
            let valued = |held: &[aterm_db::att::Held]| -> Vec<Valued> {
                held.iter()
                    .map(|held| {
                        // As every line names a writer: its machine row's name, else its id's start.
                        let (label, id) = named(&macs.name(&held.actor, held.machine.as_ref()));
                        Valued {
                            by: label.unwrap_or(id),
                            words: words(held, None),
                        }
                    })
                    .collect()
            };
            OpenedCell {
                collection: cell.collection.clone(),
                pk: place.pk.clone(),
                what: place
                    .column
                    .as_deref()
                    .map_or_else(|| "Whole row".to_owned(), column_words),
                column: place.column,
                now: cell
                    .wrote
                    .iter()
                    .map(|held| words(held, over))
                    .collect::<Vec<_>>()
                    .join(" / "),
                was: valued(&cell.replaced),
                unseen: valued(&cell.unseen),
                since: valued(&cell.since),
            }
        })
        .collect();
    Ok(Opened { cells })
}

/// What an owned or log row's whole value says in History, opened: `a row of 3 fields`; written
/// over `over`, the one row it replaced, the fields whose values differ, in words
/// (`Location, Ram gb changed`), the counter that orders a Mac's own statements left out. Never a
/// value: a field may hold a secret.
#[cfg(unix)]
pub(crate) fn whole_row_words(
    value: &aterm_json::Value,
    over: Option<&aterm_json::Value>,
) -> String {
    fn fields(value: &aterm_json::Value) -> Option<BTreeMap<&String, &aterm_json::Value>> {
        value.as_object().map(|map| {
            map.iter()
                .filter(|(name, _)| name.as_str() != COUNTER)
                .collect()
        })
    }
    let Some(now) = fields(value) else {
        return "a row".to_owned();
    };
    let Some(was) = over.and_then(fields) else {
        return match now.len() {
            1 => "a row of 1 field".to_owned(),
            n => format!("a row of {n} fields"),
        };
    };
    let names: std::collections::BTreeSet<&String> =
        now.keys().chain(was.keys()).copied().collect();
    let changed: Vec<String> = names
        .into_iter()
        .filter(|name| now.get(name) != was.get(name))
        .map(|name| column_words(name))
        .collect();
    if changed.is_empty() {
        "no field changed".to_owned()
    } else {
        format!("{} changed", changed.join(", "))
    }
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
/// (`aterm_db::att::Handle::revision`), which covers the facts and machine rows that name its
/// writers. Two reads at one point under one mark show the same page, so the page's reader
/// reads again only once the mark moves.
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Mark {
    revision: aterm_db::att::Revision,
}

#[cfg(unix)]
impl Mark {
    /// The registry's revision this mark is of, as History's pages name theirs.
    pub(crate) const fn digest(&self) -> [u8; 32] {
        self.revision.0
    }
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
    Ok(Mark { revision })
}

#[cfg(not(unix))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Mark {}

#[cfg(not(unix))]
impl Mark {
    pub(crate) const fn digest(&self) -> [u8; 32] {
        match *self {}
    }
}

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

#[cfg(not(unix))]
pub(crate) fn history(_: &Paths, _: &Asked, _: &mut Kept) -> Result<Paged, String> {
    Err("the registry is kept on macOS and Linux".into())
}

#[cfg(not(unix))]
pub(crate) fn record(_: &Paths, _: [u8; 32]) -> Result<Opened, String> {
    Err("the registry is kept on macOS and Linux".into())
}

#[cfg(all(test, unix))]
#[path = "tables_store_tests.rs"]
mod tests;
