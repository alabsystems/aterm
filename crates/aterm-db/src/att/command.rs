// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm db table …`: the generic verbs for any ATT/1 table this build compiles
//! (docs/DESIGN-alab-lab-model-2026-11-01.md §8). Every surface shows a value as it is stored
//! (addresses and phone numbers are ordinary data), a secret column's reference as its
//! secret-store entry's name, and no refusal quotes a value. JSON output is for scripts and
//! agents; there is no MCP server.

use super::{
    Ask, CAPS, Capacity, Cursor, Diff, EntryKind, Filter, Handle, Inert, Page, Point, Revision,
    Roll, Saved, TABLES, Usage, View, Writer, edit, find,
};
use crate::{
    Error, Result,
    database::decode_hex,
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
  get <app>/<role> <collection> <pk> [--at <operation>]\n\
                                        one row\n\
  scan <app>/<role> <collection> [--where col=val]... [--tsv|--json] [--at <operation>]\n\
  history <app>/<role> [<collection> [<pk>]] [--column <name>] [--writer <actor|machine>]\n\
          [--kind aterm|import|explicit] [--limit <n>] [--after <cursor> --revision <rev>]\n\
                                        who changed what, in causal order, a page at a time\n\
                                        (100 entries unless --limit says): `next` is the\n\
                                        --after of the next page and `revision` its\n\
                                        --revision; a page that says `moved` starts again\n\
                                        from the top, since the table moved under the cursor\n\
  diff <app>/<role> [<collection>] <from> [<to>|head]\n\
                                        the cells added, retired, changed and conflict-changed\n\
                                        from one point to a later one, and who made each\n\
  check <app>/<role> [--tsv]            conflicts, dangling references, unique clashes;\n\
                                        --tsv says each in words, a line each\n\
  revert <app>/<role> <operation> [--request <hex>]\n\
  export <app>/<role> <collection> --tsv <file> [--where col=val]... [--at <operation>]\n\
  apply <file>                          write an exported file's changed cells\n\
  edit <app>/<role> <collection> [--where col=val]...\n\
                                        edit in amacs's spreadsheet mode, then write\n\
  A secret column shows •••••• where its cell holds the value sealed, else the name of its\n\
  secret-store entry. A header starting with = is read-only; a conflicted cell shows ⚠ a ‖ b; a new row leaves its pk empty; a deleted row is\n\
  retired. A cell that changed since the export is never overwritten. A point is an\n\
  operation as history prints it: --at reads the table as that operation's causal past holds it.\n";

/// How many entries a page of `history` holds unless `--limit` says.
const HISTORY_PAGE: usize = 100;

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
    match run(
        &paths,
        words,
        &mut |file: &Path, line: usize| super::amacs(file, line, Some(&[])),
        &mut report,
    ) {
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
    json: bool,
    tsv: bool,
    publish: bool,
    file: Option<String>,
    request: Option<String>,
    filters: Vec<Where>,
    /// `--at <operation>`: read as of that operation's causal cut.
    at: Option<Point>,
    /// `history`'s page: its filters, cursor, revision and length.
    page: Option<Ask>,
    /// `history --limit` was given.
    limited: bool,
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
            "--json" => args.json = true,
            "--publish" if verb == "schema" => args.publish = true,
            "--tsv" if verb == "export" => {
                if args.file.is_some() {
                    return Err(Error::new("export takes one --tsv <file>"));
                }
                args.file = Some(value("--tsv")?);
            }
            "--tsv" if matches!(verb, "scan" | "check") => args.tsv = true,
            "--request" if verb == "revert" => {
                if args.request.is_some() {
                    return Err(Error::new("revert takes one --request <hex>"));
                }
                args.request = Some(value("--request")?);
            }
            "--at" if matches!(verb, "get" | "scan" | "export") => {
                if args.at.is_some() {
                    return Err(Error::new(&format!("{verb} takes one --at <operation>")));
                }
                args.at = Some(Point::At(decode_hex::<32>(&value("--at")?)?));
            }
            "--where" if matches!(verb, "scan" | "export" | "edit") => {
                args.filters.push(Where::parse(&value("--where")?)?);
            }
            "--column" | "--writer" | "--kind" | "--after" | "--revision" | "--limit"
                if verb == "history" =>
            {
                let page = args
                    .page
                    .get_or_insert_with(|| Ask::first(Filter::default(), HISTORY_PAGE));
                let given = value(word)?;
                let twice = || Error::new(&format!("history takes one {word}"));
                match word.as_str() {
                    "--column" if page.filter.column.is_none() => {
                        page.filter.column = Some(given);
                    }
                    "--writer" if page.filter.writer.is_none() => {
                        page.filter.writer = Some(match given.len() {
                            32 => Writer::Machine(decode_hex::<16>(&given)?),
                            _ => Writer::Actor(decode_hex::<48>(&given)?),
                        });
                    }
                    "--kind" if page.filter.kind.is_none() => {
                        page.filter.kind =
                            Some(EntryKind::parse(&given).ok_or_else(|| {
                                Error::new("--kind is aterm, import or explicit")
                            })?);
                    }
                    "--after" if page.after.is_none() => page.after = Some(Cursor::parse(&given)?),
                    "--revision" if page.revision.is_none() => {
                        page.revision = Some(Revision::parse(&given)?);
                    }
                    "--limit" if !args.limited => {
                        page.limit = given
                            .parse()
                            .ok()
                            .filter(|limit| *limit > 0)
                            .ok_or_else(|| Error::new("--limit is a count of 1 or more"))?;
                        args.limited = true;
                    }
                    _ => return Err(twice()),
                }
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
        return Err(Error::new(&format!("{verb} takes either --tsv or --json")));
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
            | "diff"
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
            let view = handle.view_at(args.at.unwrap_or(Point::Head))?;
            Ok(Output::Json(get(&view, collection, pk)?))
        }
        "scan" => {
            args.at_most(2)?;
            let handle = handle(args.operand(0, "<app>/<role>")?)?;
            let collection = handle
                .compiled()
                .collection(args.operand(1, "<collection>")?)?;
            let view = handle.view_at(args.at.unwrap_or(Point::Head))?;
            if args.tsv && !args.json {
                let export =
                    view.export_grid(handle.compiled().table, collection, &args.filters)?;
                return Ok(Output::Text(export.text));
            }
            Ok(Output::Json(scan(&view, collection, &args.filters)?))
        }
        "history" => {
            args.at_most(3)?;
            let handle = handle(args.operand(0, "<app>/<role>")?)?;
            let mut ask = args
                .page
                .clone()
                .unwrap_or_else(|| Ask::first(Filter::default(), HISTORY_PAGE));
            if ask.revision.is_some() != ask.after.is_some() {
                return Err(Error::new(
                    "history takes --after and --revision together, as a page's `next` and `revision`",
                ));
            }
            if let Some(collection) = args.operands.get(1) {
                handle.compiled().collection(collection)?;
                ask.filter.collection = Some(collection.clone());
            }
            ask.filter.row = args.operands.get(2).cloned();
            let page = handle.history_page(&ask)?;
            Ok(Output::Json(history(&handle, &page, &ask.filter)))
        }
        "diff" => {
            args.at_most(4)?;
            let handle = handle(args.operand(0, "<app>/<role>")?)?;
            // The first word names a collection only where the table has one of that name;
            // otherwise it is <from>, so a mistyped digest is refused as a digest.
            let mut points = &args.operands[1..];
            let mut collection = None;
            if let Some(found) = points
                .first()
                .and_then(|word| handle.compiled().table.collection(word))
            {
                collection = Some(found);
                points = &points[1..];
            }
            let from = match points.first().map(|word| Point::parse(word)) {
                Some(Ok(Point::At(at))) => at,
                Some(Ok(Point::Head)) => {
                    return Err(Error::new(
                        "diff compares forward from an operation, so <from> is not head",
                    ));
                }
                Some(Err(error)) => return Err(error),
                None => return Err(Error::new("missing <from>")),
            };
            let to = match points.get(1) {
                Some(word) => Point::parse(word)?,
                None => Point::Head,
            };
            if points.len() > 2 {
                return Err(Error::new("too many operands"));
            }
            let diff = handle.diff(from, to)?;
            Ok(Output::Json(diff_json(
                &handle,
                &diff,
                collection.map(|c| c.name),
            )))
        }
        "check" => {
            args.at_most(1)?;
            let handle = handle(args.operand(0, "<app>/<role>")?)?;
            let (view, usage) = handle.usage()?;
            let checked = check(&handle, &view, &usage);
            if args.tsv {
                let findings = checked["findings"]
                    .as_array()
                    .map_or(&[][..], Vec::as_slice);
                return Ok(Output::Text(tsv(findings)));
            }
            Ok(Output::Json(checked))
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
            let export = handle
                .view_at(args.at.unwrap_or(Point::Head))?
                .export_grid(handle.compiled().table, collection, &args.filters)?;
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
            let edited = edit::edit(&handle, collection, &args.filters, open, report)?;
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

pub(crate) use crate::typed::keys::safe_key;

/// A field as JSON for scripts: a value as its column shows it ([`grid::show`]), a conflict's
/// candidates, or a set's members; a field of no compiled column raw ([`grid::raw_shown`]).
fn field_json(column: Option<&crate::typed::Column>, field: &Field) -> Value {
    let one = |x: &Value| match column {
        Some(column) => grid_value(&column.ty, x),
        None => grid::raw_shown(x),
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
                    .map(|elem| {
                        let elem = Value::String(elem.to_owned());
                        match inner {
                            Some(inner) => grid_value(inner, &elem),
                            None => grid::raw_shown(&elem),
                        }
                    })
                    .collect::<Vec<_>>()
            )
        }
    }
}

/// A typed value as JSON: null as null, a secret reference as its entry's name, any other value
/// of a secret column as the mask, any other value its type admits as stored, and one it does not
/// raw ([`grid::raw_shown`]).
fn grid_value(ty: &Ty, x: &Value) -> Value {
    if x.is_null() {
        return Value::Null;
    }
    if matches!(ty, Ty::Secret) || !crate::typed::cell::admissible(ty, x) {
        return match ty {
            Ty::SecretRef | Ty::Secret => Value::String(crate::typed::secret::MASK.to_owned()),
            _ => grid::raw_shown(x),
        };
    }
    match ty {
        Ty::SecretRef => Value::String(grid::show(ty, x)),
        _ => x.clone(),
    }
}

fn row_json(collection: &Collection, pk: &str, row: &crate::typed::cell::Row) -> Value {
    let mut object = Map::new();
    object.insert("pk".into(), Value::String(safe_key(pk).to_owned()));
    for (name, field) in &row.fields {
        let compiled = collection.column(name);
        let column = compiled.filter(|_| !row.raw.contains(name));
        let value = match column {
            // A secret column some candidate of which names no entry of its own cell.
            None if compiled.is_some_and(|c| matches!(c.ty, Ty::SecretRef | Ty::Secret)) => {
                Value::String(crate::typed::secret::MASK.to_owned())
            }
            Some(column) if row.claims.contains_key(name) => row.claims[name]
                .as_ref()
                .map_or(Value::Null, |value| grid_value(&column.ty, value)),
            Some(column) => field_json(Some(column), field),
            None => field_json(None, field),
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

fn get(view: &View, collection: &'static Collection, pk: &str) -> Result<Value> {
    owned_only(collection)?;
    if collection.kind == Kind::Owned {
        let row = view
            .owned
            .get(collection.name)
            .and_then(|read| read.rows.get(pk))
            .ok_or_else(|| Error::new(&format!("{}: no row with that key", collection.name)))?;
        return Ok(json!({"collection": collection.name, "pk": safe_key(pk),
            "machine": keys::uuid32(&row.machine), "row": grid::raw_shown(&row.value)}));
    }
    let projection = view.project(collection);
    let row = projection
        .rows
        .get(pk)
        .ok_or_else(|| Error::new(&format!("{}: no row with that key", collection.name)))?;
    let conflicts: Vec<&str> = row
        .fields
        .iter()
        .filter(|(name, f)| {
            row.claims.get(*name).is_some_and(Option::is_none)
                || match f {
                    Field::Set(elements) => elements.values().any(Field::conflicted),
                    other => other.conflicted(),
                }
        })
        .map(|(name, _)| safe_key(name))
        .collect();
    let mut shown = json!({"collection": collection.name, "row": row_json(collection, pk, row),
        "lifecycle": row.lifecycle().unwrap_or("live"), "conflicts": conflicts,
        "raw": row.raw.iter().map(|name| safe_key(name)).collect::<Vec<_>>()});
    if !row.claims.is_empty() {
        let candidates: Map = row
            .claims
            .keys()
            .filter_map(|name| {
                row.fields.get(name).map(|field| {
                    (
                        safe_key(name).to_owned(),
                        field_json(collection.column(name), field),
                    )
                })
            })
            .collect();
        shown
            .as_object_mut()
            .unwrap()
            .insert("claim_candidates".into(), Value::Object(candidates));
    }
    Ok(shown)
}

fn scan(view: &View, collection: &'static Collection, filters: &[Where]) -> Result<Value> {
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
                        "row": grid::raw_shown(&row.value)})
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
        rows.push(row_json(collection, pk, row));
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
        "caps": {"records": CAPS.records, "bytes": CAPS.bytes,
            "writer_records": CAPS.writer_records, "writer_bytes": CAPS.writer_bytes},
        "quota": {"spent": mine, "of": compiled.table.self_quota},
        "inert": view.quota.over.len() + owned_inert,
        "refused": view.refused.len(),
        "conflicts": conflicts, "malformed": malformed, "schema": schema,
    })
}

/// One finding as a person reads it, from the JSON a `check` reports: its word, what it means
/// ([`check::said`]), the table and rows it names, its columns, and what else it says. The
/// `--tsv` form of `aterm db table check` and `aterm db alab check` prints these ([`tsv`]), and
/// the Tables page's Check view lists them, so both say a finding alike.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    /// The finding's word (`conflict`, `facts-differ`).
    pub kind: String,
    /// What it means ([`check::said`]): the word itself where no sentence is written for it.
    pub said: String,
    /// The table it names; empty where it names none (a spent quota, a roll).
    pub collection: String,
    /// The rows it names, by key, in its order.
    pub pks: Vec<String>,
    /// The columns it names.
    pub columns: Vec<String>,
    /// What else it says, in words: the table a dangling reference names, a quota's count, a
    /// roll's reason and verb, a schema's version.
    pub detail: String,
}

/// [`Line`] of one finding as `check` reports it in JSON. A field it does not know is said by
/// its name and value, so nothing a finding carries is dropped.
pub fn line(finding: &Value) -> Line {
    let text = |value: &Value| match value {
        Value::String(text) => text.clone(),
        other => aterm_json::to_string(other).unwrap_or_default(),
    };
    let texts = |value: Option<&Value>| -> Vec<String> {
        match value {
            Some(Value::Array(items)) => items.iter().map(text).collect(),
            Some(Value::Null) | None => Vec::new(),
            Some(one) => vec![text(one)],
        }
    };
    let kind = finding.get("finding").map(text).unwrap_or_default();
    let mut pks = texts(finding.get("pk"));
    pks.extend(texts(finding.get("pks")));
    let mut columns = texts(finding.get("column"));
    columns.extend(texts(finding.get("columns")));
    let mut detail = Vec::new();
    if let Some(target) = finding.get("target") {
        detail.push(format!("names a {} row", text(target)));
    }
    if let (Some(spent), Some(of)) = (finding.get("spent"), finding.get("of")) {
        let whose = if finding.get("mine") == Some(&Value::Bool(true)) {
            "this Mac".to_owned()
        } else {
            let writer = finding.get("writer").map(text).unwrap_or_default();
            format!("writer {}", writer.get(..8).unwrap_or(&writer))
        };
        detail.push(format!("{whose}: {} of {}", text(spent), text(of)));
    }
    if let Some(reason) = finding.get("reason") {
        detail.push(format!("at its {} limit", text(reason)));
    }
    if let Some(verb) = finding.get("verb") {
        detail.push(format!("run {}", text(verb)));
    }
    if let Some(version) = finding.get("version") {
        detail.push(format!("version {}", text(version)));
    }
    if let Some(said) = finding.get("line") {
        detail.push(text(said));
    }
    const READ: [&str; 15] = [
        "finding",
        "collection",
        "pk",
        "pks",
        "column",
        "columns",
        "target",
        "spent",
        "of",
        "mine",
        "writer",
        "reason",
        "verb",
        "version",
        "line",
    ];
    if let Some(object) = finding.as_object() {
        for (key, value) in object {
            if !READ.contains(&key.as_str()) {
                detail.push(format!("{key} {}", text(value)));
            }
        }
    }
    Line {
        said: check::said(&kind).to_owned(),
        kind,
        collection: finding.get("collection").map(text).unwrap_or_default(),
        pks,
        columns,
        detail: detail.join("; "),
    }
}

/// `findings` as tab-separated lines a person or a spreadsheet reads: a header, then one line
/// for each finding ([`line`]): its word, its table, its rows and its columns (each list a
/// comma apart), what it means, and what else it says. A tab or a line end inside a field reads
/// as a space.
pub fn tsv(findings: &[Value]) -> String {
    let field = |text: &str| text.replace(['\t', '\n', '\r'], " ");
    let mut out = String::from("finding\ttable\trows\tcolumns\tmeans\tdetail\n");
    for found in findings.iter().map(line) {
        let fields = [
            field(&found.kind),
            field(&found.collection),
            field(&found.pks.join(", ")),
            field(&found.columns.join(", ")),
            field(&found.said),
            field(&found.detail),
        ];
        out.push_str(&fields.join("\t"));
        out.push('\n');
    }
    out
}

pub(crate) fn finding_json(finding: &Finding) -> Value {
    let kind = finding.kind();
    match finding {
        Finding::Conflict {
            collection,
            pk,
            column,
        } => json!({
            "finding": kind,
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
            "finding": kind,
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
            "finding": kind,
            "collection": safe_key(collection),
            "columns": columns.iter().map(|c| safe_key(c)).collect::<Vec<_>>(),
            "pks": pks.iter().map(|pk| safe_key(pk)).collect::<Vec<_>>(),
        }),
        Finding::Raw {
            collection,
            pk,
            column,
        } => json!({
            "finding": kind,
            "collection": safe_key(collection),
            "pk": safe_key(pk),
            "column": safe_key(column),
        }),
        Finding::Protected {
            collection,
            pk,
            column,
        } => json!({
            "finding": kind,
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
            // generation this build rolls to names none.
            if crate::att::TABLES.iter().any(|compiled| {
                (compiled.table.application, compiled.table.role) == (table.application, table.role)
                    && compiled
                        .successor
                        .is_some_and(|next| !next.table.names_store())
            }) && let Some(object) = found.as_object_mut()
            {
                object.insert("verb".to_owned(), json!("aterm db alab roll --apply"));
            }
            found
        }
    }
}

/// `check`'s finding for each collection of `table` whose Macs state different versions of its
/// columns in `view` (`schema-split`): `aterm db table check` and `aterm db alab check` each
/// report these.
pub fn schema_findings(table: &Table, view: &View) -> Vec<Value> {
    table
        .collections
        .iter()
        .filter_map(|collection| match view.schema(collection.name) {
            SchemaState::Split { version } => Some(json!({"finding": "schema-split",
                "collection": collection.name, "version": version})),
            _ => None,
        })
        .collect()
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
    findings.extend(schema_findings(compiled.table, view));
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

/// One change as history shows it: a typed cell's value in its grid spelling (a secret cell's
/// reference to an entry not its own as the mask), a schema row by its version, anything else raw
/// ([`grid::raw_shown`]).
fn change_json(handle: &Handle, change: &crate::database::Change) -> Value {
    json!({
        "collection": safe_key(&change.collection),
        "key": safe_key(&change.key),
        "value": stored_json(handle, &change.collection, &change.key, change.value.as_ref()),
    })
}

/// A stored value of cell `key` in `collection` as history and diff show it: a typed cell's value
/// in its grid spelling, a set member's presence, a schema row by its version, anything else raw
/// ([`grid::raw_shown`]).
fn stored_json(handle: &Handle, collection: &str, key: &str, value: Option<&Value>) -> Value {
    if collection == SCHEMA {
        value
            .and_then(|v| v.get("r"))
            .map_or(Value::Null, |r| json!({"schema": grid::raw_shown(r)}))
    } else {
        let collection = handle.compiled().table.collection(collection);
        match (collection, value) {
            (_, None) => Value::Null,
            (Some(collection), Some(value)) if collection.kind == Kind::Cells => {
                let typed = CellKey::parse(key)
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
                    (Some((key, column)), Ok(cell)) => {
                        Value::String(grid::show_at(collection.name, column, key.pk, &cell.x))
                    }
                    _ => grid::raw_shown(value),
                }
            }
            (_, Some(value)) => grid::raw_shown(value),
        }
    }
}

/// `diff`'s answer: each differing cell (or owned row) of `only` (every collection where `None`)
/// with what it held at each point and the operations, writers and machines that made it.
fn diff_json(handle: &Handle, diff: &Diff, only: Option<&str>) -> Value {
    let point = |point: &Point| match point {
        Point::Head => json!("head"),
        Point::At(at) => json!(hex::encode_lower(at)),
    };
    let cells: Vec<Value> = diff
        .cells
        .iter()
        .filter(|cell| only.is_none_or(|name| cell.collection == name))
        .map(|cell| {
            let values = |candidates: &[crate::database::Candidate]| {
                candidates
                    .iter()
                    .map(|c| stored_json(handle, &cell.collection, &cell.key, Some(&c.value)))
                    .collect::<Vec<_>>()
            };
            json!({
                "collection": safe_key(&cell.collection),
                "key": safe_key(&cell.key),
                "change": cell.delta.word(),
                "before": values(&cell.before),
                "after": values(&cell.after),
                "by": cell.by.iter().map(|by| json!({
                    "operation": hex::encode_lower(&by.operation),
                    "author": hex::encode_lower(&by.actor),
                    "machine": by.machine.as_ref().map(keys::uuid32),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({"table": handle.compiled().name(), "from": point(&Point::At(diff.from)),
        "to": point(&diff.to), "cells": cells})
}

/// `history`'s page: each entry with the changes of it `filter` names, its kind and, where it
/// projects nothing, why; the revision it was read at, whether that moved under the cursor
/// asked, and the cursor of the next page.
fn history(handle: &Handle, page: &Page, filter: &Filter) -> Value {
    let operations: Vec<Value> = page
        .entries
        .iter()
        .map(|entry| {
            json!({
                "operation": hex::encode_lower(&entry.operation),
                "machine": entry.machine.as_ref().map(keys::uuid32),
                "authored_ms": entry.authored_ms,
                "hlc": {"ms": entry.hlc.ms, "counter": entry.hlc.counter},
                "request": entry.request.as_ref().map(|r| hex::encode_lower(r)),
                "reverts": entry.reverts.as_ref().map(|r| hex::encode_lower(r)),
                "kind": entry.kind.word(),
                "inert": entry.inert.map(Inert::word),
                "changes": entry.changes.iter().filter(|c| filter.wants(c))
                    .map(|c| change_json(handle, c)).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({"table": handle.compiled().name(), "revision": page.revision.text(),
        "moved": page.moved, "next": page.next.as_ref().map(Cursor::text),
        "operations": operations})
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
/// problems, a save the table's rules refuse among them, nothing is written; after a write both
/// files are rewritten from the table as it is now, with any cell that changed meanwhile showing
/// `⚠ mine ‖ theirs`.
fn apply(paths: &Paths, file: &Path) -> Result<Output> {
    let base = Base::from_json(
        &aterm_json::from_str::<Value>(&read_capped(&baseline_path(file), "grid baseline")?)
            .map_err(|_| Error::new("the grid baseline is not JSON"))?,
    )?;
    let handle = Handle::at(paths.clone(), find(&base.table)?);
    let compiled = handle.compiled();
    let collection = compiled.collection(&base.collection)?;
    let text = read_capped(file, "grid file")?;
    let (plan, applied) = match handle.save(collection, &base, &text)? {
        Saved::Written { plan, applied } => (plan, applied),
        Saved::Problems(found) => {
            return Ok(Output::Json(json!({"written": 0, "problems": found
                .iter()
                .map(|p| json!({"at": edit::place(&base, p), "line": p.line, "rule": p.rule}))
                .collect::<Vec<_>>()})));
        }
    };
    let fresh = handle
        .view()?
        .export_grid(compiled.table, collection, &[])?;
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

#[cfg(test)]
#[path = "command_secret_tests.rs"]
mod secret_tests;
