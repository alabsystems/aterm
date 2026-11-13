// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm db fleet`: the fleet relations as tables on a command line.
//!
//! It folds what this command can read without asking any machine: the registry and the
//! journal window as an observer, and the network observer's retained history, which no
//! observer is making as this command reads it (`network=held(unobserved)`). It makes no fleet
//! read, so what only a fleet read says (which machines answer now, their agents) is unread
//! here, and says so: every answer carries each input's state, and a relation whose own input
//! was not read exits 1 with no row rather than print an empty fleet. Under the dev seam
//! `ATERM_DEV_FLEET_FIXTURE` (debug and dev-seams builds) it prints the fixture's lab instead.
//!
//! A cell nobody read prints `?` (JSON `null`), never 0; a login address prints masked.

use super::fold::{FleetTables, Inputs, Source, SourceState, Sources, fold};
use super::rows::{self, Cell, Row, Schema};
use crate::alab::{LabRead, Registry, journal};
use crate::typed::{Ty, grid};
use crate::{Error, Result};
use aterm_json::{Value, json};
use aterm_types::network_state::NetworkStateSnapshot;
use std::fmt::Write as _;
use std::process::ExitCode;
use std::sync::Arc;

/// Command help.
pub const USAGE: &str = "aterm db fleet — machines, AI accounts and agents as tables (Unix)\n\
  machines [--where col=val]... [--tsv|--json]\n\
                 every registry machine by number, and each answer no registry row names\n\
  accounts [--where col=val]... [--tsv|--json]\n\
                 every registry account by provider and label: usage as last read, the\n\
                 machines its preferences expect it on, and where evidence says it runs\n\
  agents   [--where col=val]... [--tsv|--json]\n\
                 every agent session a fleet read sighted, by machine and session\n\
  schema   [machines|accounts|agents] [--tsv|--json]\n\
                 each relation's key, order and columns, with their types and meaning\n\
A table prints as TSV with a header row unless --json. A cell nobody read is `?` (JSON null),\n\
never 0; an empty cell is known to hold nothing. --where keeps rows whose cell reads as val\n\
(any item of a list cell). Each input's state (live, held, never, off) is in the JSON answer\n\
and on stderr beside a TSV one. This command asks no machine: a fleet read is a window's, so\n\
`agents` and the machines' answer columns are unread here, the network observer's history is\n\
read as it was retained (`network=held(unobserved)`), and a relation whose own input was not\n\
read prints no row and exits 1.\n";

/// What one invocation asks for.
#[derive(Debug, PartialEq, Eq)]
struct Args {
    verb: String,
    operand: Option<String>,
    json: bool,
    filters: Vec<grid::Where>,
}

fn parse(words: &[String]) -> Result<Args> {
    let mut words = words.iter();
    let verb = words
        .next()
        .ok_or_else(|| Error::new("missing fleet relation (see `aterm db fleet --help`)"))?
        .clone();
    if !matches!(verb.as_str(), "machines" | "accounts" | "agents" | "schema") {
        return Err(Error::new(
            "unknown fleet relation (see `aterm db fleet --help`)",
        ));
    }
    let mut args = Args {
        verb,
        operand: None,
        json: false,
        filters: Vec::new(),
    };
    let mut tsv = false;
    while let Some(word) = words.next() {
        match word.as_str() {
            "--json" => args.json = true,
            "--tsv" => tsv = true,
            "--where" if args.verb != "schema" => {
                let spec = words
                    .next()
                    .ok_or_else(|| Error::new("--where takes col=val"))?;
                args.filters.push(grid::Where::parse(spec)?);
            }
            flag if flag.starts_with("--") => {
                return Err(Error::new("unknown option (see `aterm db fleet --help`)"));
            }
            operand if args.verb == "schema" && args.operand.is_none() => {
                args.operand = Some(operand.to_owned());
            }
            _ => return Err(Error::new("too many operands")),
        }
    }
    if tsv && args.json {
        return Err(Error::new("a table prints as either --tsv or --json"));
    }
    Ok(args)
}

/// One cell as a table shows it: `?` for a cell nobody read, nothing for one known empty, and
/// a value as the registry's grid shows a value of its type, an address masked.
pub fn shown(ty: &Ty, cell: &Cell) -> String {
    match cell {
        Cell::Unread => "?".to_owned(),
        Cell::Empty => String::new(),
        Cell::Value(value) => grid::show(ty, value, false),
    }
}

/// One cell as JSON: `null` for a cell with no value, an address masked.
fn cell_json(ty: &Ty, cell: &Cell) -> Value {
    match cell {
        Cell::Value(value) if ty.pii() => Value::String(grid::show(ty, value, false)),
        other => other.json(),
    }
}

/// Whether a row's cell reads as `text`: its shown text, or for a list cell any item's (for a
/// list of records, any member of any item).
fn cell_matches(ty: &Ty, cell: &Cell, text: &str) -> bool {
    let text = text.trim();
    if shown(ty, cell) == text {
        return true;
    }
    let (Ty::List(inner, _), Cell::Value(Value::Array(items))) = (ty, cell) else {
        return false;
    };
    items.iter().any(|item| match item {
        Value::Object(members) => members.values().any(|member| match member {
            Value::String(word) => word == text,
            other => other.to_string() == text,
        }),
        other => grid::show(inner, other, false) == text,
    })
}

/// The rows of `rows` every filter keeps, as cells.
///
/// # Errors
/// A filter names no column of the relation.
fn kept<R: Row>(rows: &[R], filters: &[grid::Where]) -> Result<Vec<Vec<Cell>>> {
    let schema = R::SCHEMA;
    let filters: Vec<(usize, &str)> = filters
        .iter()
        .map(|filter| {
            schema
                .position(&filter.column)
                .map(|at| (at, filter.text.as_str()))
                .ok_or_else(|| Error::new(&format!("{}: no such column to filter by", schema.name)))
        })
        .collect::<Result<_>>()?;
    Ok(rows
        .iter()
        .map(Row::cells)
        .filter(|cells| {
            filters
                .iter()
                .all(|(at, text)| cell_matches(&schema.columns[*at].column.ty, &cells[*at], text))
        })
        .collect())
}

/// One TSV line: a tab or a control character in a cell prints as a space.
fn tsv_line(out: &mut String, cells: impl IntoIterator<Item = String>) {
    for (i, cell) in cells.into_iter().enumerate() {
        if i > 0 {
            out.push('\t');
        }
        out.extend(cell.chars().map(|c| if c.is_control() { ' ' } else { c }));
    }
    out.push('\n');
}

fn source_json(state: &SourceState) -> Value {
    json!({"state": state.word(), "at": state.at_ms(), "why": state.why()})
}

fn sources_json(sources: &Sources) -> Value {
    json!({
        "registry": source_json(&sources.lab),
        "journal": source_json(&sources.journal),
        "network": source_json(&sources.network),
        "fleet_read": source_json(&sources.read),
    })
}

/// Each input's state as one line: `registry=live journal=held(unreadable) …`.
pub fn sources_line(sources: &Sources) -> String {
    let word = |state: &SourceState| match state.why() {
        Some(why) => format!("{}({why})", state.word()),
        None => state.word().to_owned(),
    };
    format!(
        "registry={} journal={} network={} fleet_read={}",
        word(&sources.lab),
        word(&sources.journal),
        word(&sources.network),
        word(&sources.read),
    )
}

/// What a relation printed: its text, and whether its own input was read.
#[derive(Debug, PartialEq, Eq)]
pub struct Printed {
    pub text: String,
    /// The relation's own input holds a value: its rows, or their absence, are a reading.
    pub read: bool,
}

/// Print `relation` of `tables` as TSV, or as JSON where `json`, keeping the rows every filter
/// keeps.
///
/// # Errors
/// `relation` names none, or a filter names no column of it.
pub fn print(
    tables: &FleetTables,
    relation: &str,
    filters: &[grid::Where],
    json: bool,
) -> Result<Printed> {
    let (schema, rows, read): (&Schema, Vec<Vec<Cell>>, bool) = match relation {
        "machines" => (
            &rows::MACHINES,
            kept(&tables.machines, filters)?,
            tables.sources.lab.at_ms().is_some() || tables.sources.read.at_ms().is_some(),
        ),
        "accounts" => (
            &rows::ACCOUNTS,
            kept(&tables.accounts, filters)?,
            tables.sources.lab.at_ms().is_some(),
        ),
        "agents" => (
            &rows::AGENTS,
            kept(&tables.agents, filters)?,
            tables.sources.read.at_ms().is_some(),
        ),
        _ => return Err(Error::new("unknown fleet relation")),
    };
    let mut text = String::new();
    if json {
        // Members in the schema's column order, which a JSON object's map would sort away.
        let _ = write!(
            text,
            "{{\"relation\":{},\"sources\":{},\"unresolved_accounts\":{},\"shadowed\":{},\"rows\":[",
            Value::String(schema.name.to_owned()),
            sources_json(&tables.sources),
            tables.counts.unresolved_accounts,
            tables.counts.shadowed,
        );
        for (n, cells) in rows.iter().enumerate() {
            text.push_str(if n == 0 { "\n{" } else { ",\n{" });
            for (i, (col, cell)) in schema.columns.iter().zip(cells).enumerate() {
                if i > 0 {
                    text.push(',');
                }
                let _ = write!(
                    text,
                    "{}:{}",
                    Value::String(col.column.name.to_owned()),
                    cell_json(&col.column.ty, cell)
                );
            }
            text.push('}');
        }
        text.push_str("\n]}\n");
    } else {
        tsv_line(
            &mut text,
            schema.columns.iter().map(|col| col.column.name.to_owned()),
        );
        for cells in &rows {
            tsv_line(
                &mut text,
                schema
                    .columns
                    .iter()
                    .zip(cells)
                    .map(|(col, cell)| shown(&col.column.ty, cell)),
            );
        }
    }
    Ok(Printed { text, read })
}

/// The schemas `only` names (every one where it names none), as TSV or JSON.
///
/// # Errors
/// `only` names no relation.
pub fn print_schema(only: Option<&str>, json: bool) -> Result<String> {
    let schemas: Vec<&Schema> = rows::SCHEMAS
        .iter()
        .copied()
        .filter(|schema| only.is_none_or(|name| name == schema.name))
        .collect();
    if schemas.is_empty() {
        return Err(Error::new("unknown fleet relation"));
    }
    if json {
        let relations: Vec<Value> = schemas
            .iter()
            .map(|schema| {
                json!({
                    "relation": schema.name,
                    "doc": schema.doc,
                    "key": schema.key,
                    "order": schema.order,
                    "columns": schema.columns.iter().map(|col| json!({
                        "name": col.column.name,
                        "type": col.column.ty.word(),
                        "null": col.column.null,
                        "pii": col.column.ty.pii(),
                        "doc": col.doc,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        return Ok(format!("{}\n", json!({ "relations": relations })));
    }
    let mut text = String::new();
    tsv_line(
        &mut text,
        ["relation", "column", "type", "null", "key", "doc"].map(str::to_owned),
    );
    for schema in schemas {
        for col in schema.columns {
            let ty = match col.column.ty.word() {
                Value::String(word) => word,
                other => other.to_string(),
            };
            tsv_line(
                &mut text,
                [
                    schema.name.to_owned(),
                    col.column.name.to_owned(),
                    ty,
                    if col.column.null { "null" } else { "" }.to_owned(),
                    if schema.key.contains(&col.column.name) {
                        "key"
                    } else {
                        ""
                    }
                    .to_owned(),
                    col.doc.to_owned(),
                ],
            );
        }
    }
    Ok(text)
}

/// What this command reads on this Mac at `now_ms`: the registry and the journal window as an
/// observer, and the network observer's retained history, in the state a window gives the same
/// observation ([`super::observe`]). No fleet read: this command makes none.
fn inputs_here(now_ms: u64) -> Inputs {
    if let Some(fixture) = super::fixture::under_seam(now_ms) {
        return fixture;
    }
    let mut inputs = Inputs::default();
    let config = aterm_types::dirs::aterm_config_path();
    if let Some(config) = &config {
        match crate::network_state::read_history(config) {
            Ok(network) => {
                super::observe(
                    &mut inputs.network,
                    &Arc::new(NetworkStateSnapshot {
                        network,
                        ..NetworkStateSnapshot::default()
                    }),
                );
            }
            Err(_) => {
                inputs.network = Source::Held {
                    last: None,
                    why: super::HELD_UNREADABLE,
                };
            }
        }
    }
    let (Ok(paths), Some(config)) = (aterm_secrets::client::Paths::live(), config) else {
        inputs.lab = Source::Off;
        inputs.journal = Source::Off;
        return inputs;
    };
    inputs.lab = match Registry::at(paths.clone()).lab(&config, None) {
        Ok(Some(LabRead::Projected(view, _))) => Source::Live {
            value: Arc::new(*view),
            at_ms: now_ms,
        },
        Ok(Some(LabRead::Partial | LabRead::Unchanged)) => Source::Held {
            last: None,
            why: super::HELD_PARTIAL,
        },
        Ok(None) => Source::Off,
        Err(_) => Source::Held {
            last: None,
            why: super::HELD_UNREADABLE,
        },
    };
    inputs.journal = match journal::seen::read(&paths, None, now_ms / 1_000) {
        Ok(Some(seen)) => Source::Live {
            value: Arc::new(seen),
            at_ms: now_ms,
        },
        Ok(None) => Source::Off,
        Err(_) => Source::Held {
            last: None,
            why: super::HELD_UNREADABLE,
        },
    };
    inputs
}

/// Run `aterm db fleet <words>`.
pub fn main_entry(words: &[String]) -> ExitCode {
    const VERB: &str = "aterm db fleet";
    if words.is_empty() || words.iter().any(|w| w == "--help" || w == "-h") {
        return aterm_uds::stdout::answer_exit(VERB, USAGE, 0);
    }
    let refuse = |error: &Error| {
        let _ = aterm_uds::stdout::stderr_line(format_args!("{VERB}: {error}"));
        ExitCode::from(2)
    };
    let args = match parse(words) {
        Ok(args) => args,
        Err(error) => return refuse(&error),
    };
    if args.verb == "schema" {
        return match print_schema(args.operand.as_deref(), args.json) {
            Ok(text) => aterm_uds::stdout::answer_exit(VERB, &text, 0),
            Err(error) => refuse(&error),
        };
    }
    let now_ms = aterm_time::unix_millis().unwrap_or(0);
    let tables = fold(&inputs_here(now_ms), now_ms).tables;
    match print(&tables, &args.verb, &args.filters, args.json) {
        Ok(printed) => {
            if !args.json || !printed.read {
                let _ = aterm_uds::stdout::stderr_line(format_args!(
                    "{VERB}: {}{}",
                    sources_line(&tables.sources),
                    if printed.read {
                        ""
                    } else {
                        ": this relation's own input was not read, so it has no rows to show"
                    }
                ));
            }
            aterm_uds::stdout::answer_exit(VERB, &printed.text, u8::from(!printed.read))
        }
        Err(error) => refuse(&error),
    }
}
