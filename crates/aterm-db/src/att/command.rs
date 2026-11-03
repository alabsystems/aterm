// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm db table …`: the generic verbs for any ATT/1 table this build compiles
//! (docs/DESIGN-alab-lab-model-2026-11-01.md §8). Every surface masks addresses (`ab…@…`)
//! unless `--show-addresses` is given, and no refusal quotes a value. JSON output is for
//! scripts and agents; there is no MCP server.

use super::{Capacity, Entry, Handle, Roll, TABLES, Usage, View, edit, find};
use crate::{
    Error, Result,
    database::decode_hex,
    table::store::{MAX_BYTES, MAX_RECORDS, WRITER_BYTES, WRITER_RECORDS},
    typed::{
        Collection, Kind, SCHEMA, Table, Ty,
        cell::{CellKey, CellValue, Field},
        check::{self, Finding},
        grid::{self, Base, Where},
        keys,
        owned::SchemaState,
    },
};
use astream_wire::sync::Actor;
use aterm_codec::hex;
use aterm_json::{Map, Value, json};
use aterm_secrets::client::Paths;
use std::{collections::BTreeMap, io::Write, path::Path, process::ExitCode};

/// The verbs' help.
pub const USAGE: &str = "aterm db table — typed ATT/1 tables on Commons (Unix)\n\
  ls                                    each table: records and bytes against the caps, the\n\
                                        per-Mac quota left, inert records, conflicts, schemas\n\
  schema <app>/<role>[/<collection>] [--publish]\n\
                                        compiled and stored schema rows; --publish writes them\n\
  get <app>/<role> <collection> <pk>    one row\n\
  scan <app>/<role> <collection> [--where col=val]... [--tsv|--json]\n\
  history <app>/<role> [<collection> [<pk>]]\n\
                                        who changed what, oldest first\n\
  check <app>/<role>                    conflicts, dangling references, unique clashes\n\
  revert <app>/<role> <operation> [--request <hex>]\n\
  export <app>/<role> <collection> --tsv <file> [--where col=val]...\n\
  apply <file>                          write an exported file's changed cells\n\
  edit <app>/<role> <collection> [--where col=val]...\n\
                                        edit in amacs's spreadsheet mode, then write\n\
  Addresses are masked unless --show-addresses is given. A header starting with = is\n\
  read-only; a conflicted cell shows ⚠ a ‖ b; a new row leaves its pk empty; a deleted row is\n\
  retired. A cell that changed since the export is never overwritten.\n";

/// What a verb prints.
#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    Json(Value),
    Text(String),
}

/// Run `aterm db table <words>` on this Mac's Commons.
pub fn main_entry(words: &[String]) -> ExitCode {
    if words.is_empty() || words.iter().any(|w| w == "--help" || w == "-h") {
        return aterm_uds::stdout::answer_exit("aterm db table", USAGE, 0);
    }
    let paths = match Paths::live() {
        Ok(paths) => paths,
        Err(_) => {
            let _ = writeln!(std::io::stderr(), "aterm db table: no vault paths here");
            return ExitCode::from(1);
        }
    };
    let mut report = |line: &str| {
        let _ = writeln!(std::io::stderr(), "aterm db table: {line}");
    };
    match run(&paths, words, &mut super::amacs, &mut report) {
        Ok(Output::Json(value)) => match aterm_json::to_string(&value) {
            Ok(text) => aterm_uds::stdout::answer_exit("aterm db table", &format!("{text}\n"), 0),
            Err(_) => ExitCode::from(1),
        },
        Ok(Output::Text(text)) => aterm_uds::stdout::answer_exit("aterm db table", &text, 0),
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "aterm db table: {error}");
            ExitCode::from(1)
        }
    }
}

/// The words of one verb: its operands and options.
#[derive(Default)]
struct Args {
    operands: Vec<String>,
    reveal: bool,
    json: bool,
    tsv: bool,
    publish: bool,
    file: Option<String>,
    request: Option<String>,
    filters: Vec<Where>,
}

fn parse(verb: &str, words: &[String]) -> Result<Args> {
    let mut args = Args::default();
    let mut words = words.iter();
    while let Some(word) = words.next() {
        let mut value = |name: &str| {
            words
                .next()
                .cloned()
                .ok_or_else(|| Error::new(&format!("{name} needs a value")))
        };
        match word.as_str() {
            "--show-addresses" => args.reveal = true,
            "--json" => args.json = true,
            "--publish" if verb == "schema" => args.publish = true,
            "--tsv" if verb == "export" => {
                if args.file.is_some() {
                    return Err(Error::new("export takes one --tsv <file>"));
                }
                args.file = Some(value("--tsv")?);
            }
            "--tsv" if verb == "scan" => args.tsv = true,
            "--request" if verb == "revert" => {
                if args.request.is_some() {
                    return Err(Error::new("revert takes one --request <hex>"));
                }
                args.request = Some(value("--request")?);
            }
            "--where" if matches!(verb, "scan" | "export" | "edit") => {
                args.filters.push(Where::parse(&value("--where")?)?);
            }
            flag if flag.starts_with("--") => {
                return Err(Error::new(&format!(
                    "{verb}: unknown option (see `aterm db table --help`)"
                )));
            }
            operand => args.operands.push(operand.to_owned()),
        }
    }
    if args.tsv && args.json {
        return Err(Error::new("scan takes either --tsv or --json"));
    }
    Ok(args)
}

impl Args {
    fn operand(&self, n: usize, what: &str) -> Result<&str> {
        self.operands
            .get(n)
            .map(String::as_str)
            .ok_or_else(|| Error::new(&format!("missing {what}")))
    }

    fn at_most(&self, n: usize) -> Result<()> {
        if self.operands.len() > n {
            return Err(Error::new("too many operands"));
        }
        Ok(())
    }
}

/// Run one verb against the Commons at `paths`: `open` shows a grid file and returns once it is
/// closed (`edit`), `report` hears an edit's problems by row and column.
///
/// # Errors
/// The words are not a verb, or the verb fails.
pub fn run(
    paths: &Paths,
    words: &[String],
    open: edit::Opener<'_>,
    report: &mut dyn FnMut(&str),
) -> Result<Output> {
    let verb = words
        .first()
        .map(String::as_str)
        .ok_or_else(|| Error::new("missing table verb"))?;
    if !matches!(
        verb,
        "ls" | "schema"
            | "get"
            | "scan"
            | "history"
            | "check"
            | "revert"
            | "export"
            | "apply"
            | "edit"
    ) {
        return Err(Error::new(
            "unknown table verb (see `aterm db table --help`)",
        ));
    }
    let args = parse(verb, &words[1..])?;
    let handle = |name: &str| Ok::<_, Error>(Handle::at(paths.clone(), find(name)?));
    match verb {
        "ls" => {
            args.at_most(0)?;
            Ok(Output::Json(json!({"tables": TABLES
                .iter()
                .map(|compiled| ls(&Handle::at(paths.clone(), compiled)))
                .collect::<Vec<_>>()})))
        }
        "schema" => {
            args.at_most(1)?;
            let target = args.operand(0, "<app>/<role>[/<collection>]")?;
            let mut parts = target.splitn(3, '/');
            let name = format!(
                "{}/{}",
                parts.next().unwrap_or_default(),
                parts.next().unwrap_or_default()
            );
            let handle = handle(&name)?;
            let only = parts.next();
            if let Some(collection) = only {
                handle.compiled().collection(collection)?;
            }
            if args.publish {
                if only.is_some() {
                    return Err(Error::new("--publish writes the whole table's schema rows"));
                }
                let written = handle.publish_schema()?;
                return Ok(Output::Json(json!({"table": name, "written": written})));
            }
            Ok(Output::Json(schema(&handle, only)))
        }
        "get" => {
            args.at_most(3)?;
            let handle = handle(args.operand(0, "<app>/<role>")?)?;
            let collection = handle
                .compiled()
                .collection(args.operand(1, "<collection>")?)?;
            let pk = args.operand(2, "<pk>")?;
            let view = handle.view()?;
            Ok(Output::Json(get(&view, collection, pk, args.reveal)?))
        }
        "scan" => {
            args.at_most(2)?;
            let handle = handle(args.operand(0, "<app>/<role>")?)?;
            let collection = handle
                .compiled()
                .collection(args.operand(1, "<collection>")?)?;
            let view = handle.view()?;
            if args.tsv && !args.json {
                let export = grid::export(
                    handle.compiled().table,
                    collection,
                    &view.cells,
                    &args.filters,
                    args.reveal,
                )?;
                return Ok(Output::Text(export.text));
            }
            Ok(Output::Json(scan(
                &view,
                collection,
                &args.filters,
                args.reveal,
            )?))
        }
        "history" => {
            args.at_most(3)?;
            let handle = handle(args.operand(0, "<app>/<role>")?)?;
            let collection = args.operands.get(1).map(String::as_str);
            if let Some(collection) = collection {
                handle.compiled().collection(collection)?;
            }
            let pk = args.operands.get(2).map(String::as_str);
            Ok(Output::Json(history(
                &handle,
                &handle.history()?,
                collection,
                pk,
                args.reveal,
            )))
        }
        "check" => {
            args.at_most(1)?;
            let handle = handle(args.operand(0, "<app>/<role>")?)?;
            let (view, usage) = handle.usage()?;
            Ok(Output::Json(check(&handle, &view, &usage)))
        }
        "revert" => {
            args.at_most(2)?;
            let handle = handle(args.operand(0, "<app>/<role>")?)?;
            let operation = decode_hex::<32>(args.operand(1, "<operation>")?)?;
            let request = match &args.request {
                Some(text) => decode_hex::<16>(text)?,
                None => crate::crypto::random()?,
            };
            let id = handle.revert(request, operation)?;
            Ok(Output::Json(json!({"operation": hex::encode_lower(&id),
                "request": hex::encode_lower(&request)})))
        }
        "export" => {
            args.at_most(2)?;
            let handle = handle(args.operand(0, "<app>/<role>")?)?;
            let collection = handle
                .compiled()
                .collection(args.operand(1, "<collection>")?)?;
            let file = args
                .file
                .as_deref()
                .ok_or_else(|| Error::new("export needs --tsv <file>"))?;
            let export = grid::export(
                handle.compiled().table,
                collection,
                &handle.view()?.cells,
                &args.filters,
                args.reveal,
            )?;
            save(Path::new(file), &export.text, &export.base)?;
            Ok(Output::Json(
                json!({"file": file, "baseline": baseline(Path::new(file)),
                "rows": export.base.rows.len()}),
            ))
        }
        "apply" => {
            args.at_most(1)?;
            apply(paths, Path::new(args.operand(0, "<file>")?))
        }
        "edit" => {
            args.at_most(2)?;
            let handle = handle(args.operand(0, "<app>/<role>")?)?;
            let collection = handle
                .compiled()
                .collection(args.operand(1, "<collection>")?)?;
            let edited = edit::edit(
                &handle,
                collection,
                &args.filters,
                args.reveal,
                open,
                report,
            )?;
            Ok(Output::Json(json!({
                "rounds": edited.rounds,
                "records": edited.applied.iter().map(|a| a.records).sum::<usize>(),
                "cells": edited.applied.iter().map(|a| a.cells).sum::<usize>(),
                "unchanged": edited.applied.iter().map(|a| a.unchanged).sum::<usize>(),
                "abandoned": edited.abandoned,
            })))
        }
        _ => Err(Error::new(
            "unknown table verb (see `aterm db table --help`)",
        )),
    }
}

pub(crate) fn safe_key(key: &str) -> &str {
    if crate::typed::guard::credential_shaped(key) {
        grid::REFUSED
    } else {
        key
    }
}

/// A field as JSON for scripts: a value (addresses masked unless `reveal`), a conflict's
/// candidates, or a set's members.
fn field_json(column: Option<&crate::typed::Column>, field: &Field, reveal: bool) -> Value {
    let one = |x: &Value| match column {
        Some(column) => grid_value(&column.ty, x, reveal),
        None => grid::raw_masked(x, reveal),
    };
    match field {
        Field::Value(candidate) => one(&candidate.value.x),
        Field::Conflict { candidates, .. } => {
            json!({"conflict": candidates.iter().map(|c| one(&c.value.x)).collect::<Vec<_>>()})
        }
        Field::Set(_) => {
            let inner = column.and_then(|c| match c.ty {
                Ty::Set(inner) => Some(inner),
                _ => None,
            });
            json!(
                field
                    .members()
                    .into_iter()
                    .map(|elem| match inner {
                        Some(inner) => grid_value(inner, &Value::String(elem.to_owned()), reveal),
                        None => grid::raw_masked(&Value::String(elem.to_owned()), reveal),
                    })
                    .collect::<Vec<_>>()
            )
        }
    }
}

/// A typed value with its addresses masked unless `reveal`.
fn grid_value(ty: &Ty, x: &Value, reveal: bool) -> Value {
    if !crate::typed::cell::admissible(ty, x) {
        return grid::raw_masked(x, reveal);
    }
    match ty {
        Ty::Email => match x.as_str() {
            Some(address) if !reveal => Value::String(grid::mask(address)),
            _ => x.clone(),
        },
        _ if ty.pii() && !reveal => grid::raw_masked(x, false),
        _ => x.clone(),
    }
}

fn row_json(
    collection: &Collection,
    pk: &str,
    row: &crate::typed::cell::Row,
    reveal: bool,
) -> Value {
    let mut object = Map::new();
    object.insert("pk".into(), Value::String(safe_key(pk).to_owned()));
    for (name, field) in &row.fields {
        let column = collection.column(name).filter(|_| !row.raw.contains(name));
        let value = match column {
            Some(column) => field_json(Some(column), field, reveal),
            None => field_json(None, field, reveal),
        };
        object.insert(safe_key(name).to_owned(), value);
    }
    Value::Object(object)
}

fn owned_only(collection: &Collection) -> Result<()> {
    if collection.kind == Kind::Log {
        return Err(Error::new(&format!(
            "{}: a log collection lives in a ring table, not here",
            collection.name
        )));
    }
    Ok(())
}

fn get(view: &View, collection: &'static Collection, pk: &str, reveal: bool) -> Result<Value> {
    owned_only(collection)?;
    if collection.kind == Kind::Owned {
        let row = view
            .owned
            .get(collection.name)
            .and_then(|read| read.rows.get(pk))
            .ok_or_else(|| Error::new(&format!("{}: no row with that key", collection.name)))?;
        return Ok(json!({"collection": collection.name, "pk": safe_key(pk),
            "machine": keys::uuid32(&row.machine), "row": grid::raw_masked(&row.value, reveal)}));
    }
    let projection = view.project(collection);
    let row = projection
        .rows
        .get(pk)
        .ok_or_else(|| Error::new(&format!("{}: no row with that key", collection.name)))?;
    let conflicts: Vec<&str> = row
        .fields
        .iter()
        .filter(|(_, f)| match f {
            Field::Set(elements) => elements.values().any(Field::conflicted),
            other => other.conflicted(),
        })
        .map(|(name, _)| safe_key(name))
        .collect();
    Ok(
        json!({"collection": collection.name, "row": row_json(collection, pk, row, reveal),
        "lifecycle": row.lifecycle().unwrap_or("live"), "conflicts": conflicts,
        "raw": row.raw.iter().map(|name| safe_key(name)).collect::<Vec<_>>()}),
    )
}

fn scan(
    view: &View,
    collection: &'static Collection,
    filters: &[Where],
    reveal: bool,
) -> Result<Value> {
    owned_only(collection)?;
    if collection.kind == Kind::Owned {
        if !filters.is_empty() {
            return Err(Error::new(&format!(
                "{}: --where filters cells collections only",
                collection.name
            )));
        }
        let rows: Vec<Value> = view
            .owned
            .get(collection.name)
            .map(|read| {
                read.rows
                    .iter()
                    .map(|(pk, row)| {
                        json!({"pk": safe_key(pk), "machine": keys::uuid32(&row.machine),
                        "row": grid::raw_masked(&row.value, reveal)})
                    })
                    .collect()
            })
            .unwrap_or_default();
        return Ok(json!({"collection": collection.name, "rows": rows}));
    }
    let projection = view.project(collection);
    let mut rows = Vec::new();
    'rows: for (pk, row) in &projection.rows {
        for filter in filters {
            if !filter.matches(collection, pk, row)? {
                continue 'rows;
            }
        }
        rows.push(row_json(collection, pk, row, reveal));
    }
    Ok(json!({"collection": collection.name, "rows": rows, "malformed": projection.malformed}))
}

fn stored(state: &SchemaState) -> Value {
    match state {
        SchemaState::Absent => json!("absent"),
        SchemaState::One { version, .. } => json!(version),
        SchemaState::Split { version } => json!({"split": version}),
    }
}

fn schema(handle: &Handle, only: Option<&str>) -> Value {
    let compiled = handle.compiled();
    let view = handle.view();
    let collections: Vec<Value> = compiled
        .table
        .collections
        .iter()
        .filter(|c| only.is_none_or(|name| c.name == name))
        .map(|c| {
            let state = view.as_ref().map(|v| v.schema(c.name));
            json!({"collection": c.name, "compiled": c.version,
                "stored": state.as_ref().map_or_else(|e| json!(e.to_string()), stored),
                "row": c.schema_row()["x"].clone()})
        })
        .collect();
    json!({"table": compiled.name(), "collections": collections})
}

fn ls(handle: &Handle) -> Value {
    let compiled = handle.compiled();
    let (view, usage): (View, Usage) = match handle.usage() {
        Ok(read) => read,
        Err(error) => return json!({"table": compiled.name(), "state": error.to_string()}),
    };
    let mine = view.quota.spent.get(&usage.actor).copied().unwrap_or(0);
    let mut conflicts = 0;
    let mut malformed = 0;
    let mut owned_inert = 0;
    let mut schema = Map::new();
    for collection in compiled.table.collections {
        match collection.kind {
            Kind::Cells => {
                let projection = view.project(collection);
                malformed += projection.malformed;
                conflicts += projection
                    .rows
                    .values()
                    .flat_map(|row| row.fields.values())
                    .filter(|f| match f {
                        Field::Set(elements) => elements.values().any(Field::conflicted),
                        other => other.conflicted(),
                    })
                    .count();
            }
            Kind::Owned => {
                owned_inert += view.owned.get(collection.name).map_or(0, |r| r.inert);
            }
            Kind::Log => {}
        }
        schema.insert(
            collection.name.to_owned(),
            json!({"compiled": collection.version, "stored": stored(&view.schema(collection.name))}),
        );
    }
    json!({
        "table": compiled.name(),
        "state": "subscribed",
        "complete": view.complete,
        "records": usage.records, "bytes": usage.bytes,
        "mine": {"records": usage.mine_records, "bytes": usage.mine_bytes},
        "caps": {"records": MAX_RECORDS, "bytes": MAX_BYTES,
            "writer_records": WRITER_RECORDS, "writer_bytes": WRITER_BYTES},
        "quota": {"spent": mine, "of": compiled.table.self_quota},
        "inert": view.quota.over.len() + owned_inert,
        "refused": view.refused.len(),
        "conflicts": conflicts, "malformed": malformed, "schema": schema,
    })
}

pub(crate) fn finding_json(finding: &Finding) -> Value {
    match finding {
        Finding::Conflict {
            collection,
            pk,
            column,
        } => json!({
            "finding": "conflict",
            "collection": safe_key(collection),
            "pk": safe_key(pk),
            "column": safe_key(column),
        }),
        Finding::Dangling {
            collection,
            pk,
            column,
            target,
        } => json!({
            "finding": "dangling",
            "collection": safe_key(collection),
            "pk": safe_key(pk),
            "column": safe_key(column),
            "target": safe_key(target),
        }),
        Finding::Unique {
            collection,
            columns,
            pks,
        } => json!({
            "finding": "unique",
            "collection": safe_key(collection),
            "columns": columns.iter().map(|c| safe_key(c)).collect::<Vec<_>>(),
            "pks": pks.iter().map(|pk| safe_key(pk)).collect::<Vec<_>>(),
        }),
        Finding::Raw {
            collection,
            pk,
            column,
        } => json!({
            "finding": "raw",
            "collection": safe_key(collection),
            "pk": safe_key(pk),
            "column": safe_key(column),
        }),
    }
}

/// A capacity finding as `check` lists it; `mine` marks this Mac's own writer.
pub(crate) fn capacity_json(table: &Table, found: &Capacity, me: &Actor) -> Value {
    match found {
        Capacity::QuotaSpent { writer, spent } => json!({
            "finding": "quota-spent", "writer": hex::encode_lower(writer), "mine": writer == me,
            "spent": spent, "of": table.self_quota,
        }),
        Capacity::Roll(reason) => {
            let mut found = json!({"finding": "roll", "reason": match reason {
                Roll::Quota => "quota",
                Roll::Records => "records",
                Roll::Bytes => "bytes",
            }});
            // The registry's roll is its own verb (design §3.8); a table with no later
            // generation compiled names none.
            if crate::att::TABLES.iter().any(|compiled| {
                (compiled.table.application, compiled.table.role) == (table.application, table.role)
                    && compiled.successor.is_some()
            }) && let Some(object) = found.as_object_mut()
            {
                object.insert("verb".to_owned(), json!("aterm db alab roll --apply"));
            }
            found
        }
    }
}

fn check(handle: &Handle, view: &View, usage: &Usage) -> Value {
    let compiled = handle.compiled();
    let cells: Vec<&Collection> = compiled
        .table
        .collections
        .iter()
        .copied()
        .filter(|c| c.kind == Kind::Cells)
        .collect();
    let projections: BTreeMap<&str, crate::typed::cell::Projection> =
        cells.iter().map(|c| (c.name, view.project(c))).collect();
    let tables: BTreeMap<&str, &crate::typed::cell::Projection> =
        projections.iter().map(|(name, p)| (*name, p)).collect();
    let mut findings: Vec<Value> = check::findings(&cells, &tables)
        .iter()
        .map(finding_json)
        .collect();
    for collection in compiled.table.collections {
        if let SchemaState::Split { version } = view.schema(collection.name) {
            findings.push(
                json!({"finding": "schema-split", "collection": collection.name,
                "version": version}),
            );
        }
    }
    findings.extend(
        super::capacity(compiled.table, view, usage)
            .iter()
            .map(|found| capacity_json(compiled.table, found, &usage.actor)),
    );
    json!({"table": compiled.name(), "complete": view.complete, "findings": findings,
        "inert": view.quota.over.len(),
        "refused": view.refused.iter().map(|id| hex::encode_lower(id)).collect::<Vec<_>>(),
        "malformed": projections.values().map(|p| p.malformed).sum::<usize>()})
}

/// One change as history shows it: a typed cell's value in its grid spelling (addresses masked
/// unless `reveal`), a schema row by its version, anything else raw and masked.
fn change_json(handle: &Handle, change: &crate::database::Change, reveal: bool) -> Value {
    let value = change.value.as_ref();
    let shown = if change.collection == SCHEMA {
        value.and_then(|v| v.get("r")).map_or(
            Value::Null,
            |r| json!({"schema": grid::raw_masked(r, reveal)}),
        )
    } else {
        let collection = handle.compiled().table.collection(&change.collection);
        match (collection, value) {
            (_, None) => Value::Null,
            (Some(collection), Some(value)) if collection.kind == Kind::Cells => {
                let typed = CellKey::parse(&change.key)
                    .ok()
                    .and_then(|key| collection.column(key.col).map(|c| (key, c)));
                match (typed, CellValue::decode(value)) {
                    (Some((key, column)), Ok(cell))
                        if key.elem.is_some()
                            && matches!(column.ty, Ty::Set(_))
                            && cell.x.as_bool().is_some() =>
                    {
                        cell.x
                    }
                    (Some((_, column)), Ok(cell)) => {
                        Value::String(grid::show(&column.ty, &cell.x, reveal))
                    }
                    _ => grid::raw_masked(value, reveal),
                }
            }
            (_, Some(value)) => grid::raw_masked(value, reveal),
        }
    };
    json!({
        "collection": safe_key(&change.collection),
        "key": safe_key(&change.key),
        "value": shown,
    })
}

fn history(
    handle: &Handle,
    entries: &[Entry],
    collection: Option<&str>,
    pk: Option<&str>,
    reveal: bool,
) -> Value {
    let wanted = |change: &crate::database::Change| {
        collection.is_none_or(|name| change.collection == name)
            && pk.is_none_or(|pk| {
                change.key == pk
                    || change
                        .key
                        .strip_prefix(pk)
                        .is_some_and(|rest| rest.starts_with('.'))
            })
    };
    let operations: Vec<Value> = entries
        .iter()
        .filter(|entry| entry.changes.iter().any(wanted))
        .map(|entry| {
            json!({
                "operation": hex::encode_lower(&entry.operation),
                "machine": entry.machine.as_ref().map(keys::uuid32),
                "authored_ms": entry.authored_ms,
                "request": entry.request.as_ref().map(|r| hex::encode_lower(r)),
                "reverts": entry.reverts.as_ref().map(|r| hex::encode_lower(r)),
                "changes": entry.changes.iter().filter(|c| wanted(c))
                    .map(|c| change_json(handle, c, reveal)).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({"table": handle.compiled().name(), "operations": operations})
}

/// The baseline file an export writes beside its TSV.
pub fn baseline(file: &Path) -> String {
    format!("{}.att.json", file.display())
}

fn baseline_path(file: &Path) -> std::path::PathBuf {
    let mut os = file.as_os_str().to_os_string();
    os.push(".att.json");
    std::path::PathBuf::from(os)
}

/// Write an export's TSV and its baseline, each owner-only (`0600`).
fn save(file: &Path, text: &str, base: &Base) -> Result<()> {
    let base_json = aterm_json::to_vec(&base.to_json())
        .map_err(|_| Error::new("the grid baseline could not be encoded"))?;
    aterm_uds::atomic_file::write(file, text.as_bytes(), 0o600)
        .map_err(|_| Error::new("the grid file could not be written"))?;
    aterm_uds::atomic_file::write(&baseline_path(file), &base_json, 0o600)
        .map_err(|_| Error::new("the grid baseline could not be written"))?;
    Ok(())
}

fn read_capped(path: &Path, what: &str) -> Result<String> {
    super::read_regular_utf8(
        path,
        grid::MAX_BYTES * 4,
        &format!("the {what} could not be read"),
        &format!("the {what} is too large"),
        &format!("the {what} is not UTF-8"),
    )
}

/// `apply <file>`: read an exported file against its baseline and write its changed cells. On
/// problems nothing is written; after a write both files are rewritten from the table as it is
/// now, with any cell that changed meanwhile showing `⚠ mine ‖ theirs`.
fn apply(paths: &Paths, file: &Path) -> Result<Output> {
    let base = Base::from_json(
        &aterm_json::from_str::<Value>(&read_capped(&baseline_path(file), "grid baseline")?)
            .map_err(|_| Error::new("the grid baseline is not JSON"))?,
    )?;
    let handle = Handle::at(paths.clone(), find(&base.table)?);
    let compiled = handle.compiled();
    let collection = compiled.collection(&base.collection)?;
    let text = read_capped(file, "grid file")?;
    let view = handle.view()?;
    let birth = |c: &Collection, values: &_| compiled.born(c, values, &view.cells);
    let plan = match grid::plan(compiled.table, collection, &base, &text, &birth) {
        Ok(plan) => plan,
        Err(problems) => {
            return Ok(Output::Json(json!({"written": 0, "problems": problems
                .iter()
                .map(|p| json!({"at": edit::place(&base, p), "line": p.line, "rule": p.rule}))
                .collect::<Vec<_>>()})));
        }
    };
    let applied = handle.apply(collection, &plan)?;
    let fresh = grid::export(
        compiled.table,
        collection,
        &handle.view()?.cells,
        &[],
        base.reveal,
    )?;
    let reopened = grid::reopen(fresh.base, &applied.stale);
    let rows: std::collections::BTreeSet<&str> =
        base.rows.iter().map(|(pk, _)| pk.as_str()).collect();
    let mut kept = reopened;
    kept.rows
        .retain(|(pk, _)| rows.contains(pk.as_str()) || plan.born.iter().any(|born| born == pk));
    save(file, &grid::render(&kept), &kept)?;
    Ok(Output::Json(json!({
        "written": applied.records, "cells": applied.cells, "unchanged": applied.unchanged,
        "born": applied.born.iter().map(|pk| safe_key(pk)).collect::<Vec<_>>(),
        "retired": applied.retired.iter().map(|pk| safe_key(pk)).collect::<Vec<_>>(),
        "stale": applied.stale.iter().map(|s| json!({"pk": safe_key(&s.pk), "column": safe_key(&s.column)}))
            .collect::<Vec<_>>(),
    })))
}
