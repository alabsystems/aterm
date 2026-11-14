// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm file`'s verbs that need no typed table (docs/DESIGN-shared-files-2026-11-11.md §3):
//! `ls`, `cat`, `path`, `status`, `get`, `rows`, `watch`, `history` and `outbox`. Each reads
//! local files through [`File::read`] and nothing else, so none can open custody and none has
//! an answer that is an error about the replica: every answer carries the state word instead.
//! `outbox drop` changes only this Mac's own queue.
//!
//! The verbs that open custody (`refresh`) and the ones that write through the typed table
//! (`set`, `add`, `rm`, `revert`, `resolve`) live in aterm-db, beside the materializer and the
//! publisher, and hand every other word here.

use crate::{Dropped, File, Intent, Name, Read, Root, Row, Status, exit, snapshot::PK};
use aterm_json::{Map, Value};
use std::time::{Duration, Instant};

/// A file this build compiles, as `ls` names it before any pass has run.
#[derive(Clone, Copy, Debug)]
pub struct Known {
    pub name: &'static str,
    pub kind: &'static str,
    pub about: &'static str,
}

/// What a verb answers: stdout, stderr and the exit code.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answer {
    pub out: String,
    pub err: String,
    pub code: u8,
}

impl Answer {
    fn usage(why: &str) -> Self {
        Self {
            out: String::new(),
            err: format!("aterm file: {why}\n"),
            code: exit::USAGE,
        }
    }
}

/// Every verb, for `aterm file` with no words and for a word that is no verb.
pub const USAGE: &str = "\
usage: aterm file <verb> [--json]
  ls                                     every shared file here, with its state
  cat <name> [--published]               the table as TSV, this Mac's queued writes over it
                                         (--published: the snapshot alone); the state on stderr
  path <name>                            where its face is
  status [<name>]                        state, why, revision, rows, face=written|edited|missing
  get <name> <row> [<column>]            one row or one cell; <row> is a key or the row's name
  rows <name> [--where col=val]... [--join <ref column>]
  watch <name> [--timeout <seconds>] [--changes <n>]
                                         a line each time a pass records a new state or revision
  history <name> [--limit <n>]           its newest operations, without values
  refresh [<name>]                       run one materializer pass now (opens custody)
  set <name> <row> <column> [<value> | --stdin]
                                         write one cell (no value clears it)
  add <name> <column>=<value>...         a new row, born under its derived key
  rm <name> <row>                        retire the row (its history stays)
  revert <name> <operation>              undo one operation while its cells still hold it
  resolve <name> <id> mine|theirs        a held write: queue mine again, or keep theirs
  outbox [<name>]                        this Mac's queued and held writes, without values
  outbox drop <id>                       drop one of them
A name is <set>/<leaf>: ai/accounts, ai/links, lab/machines. Every answer carries a state word
(live, held, never, off), and none of them is an error: a read answers from the snapshot on
this Mac whatever the replica's custody is doing. A write is queued on this Mac first and
answers `published <operation>` or `queued <id>: <why>`; a queued write is published when this
Mac's writer can sign it, and one whose cell changed meanwhile is held, shown as
`⚠ mine ‖ theirs`, never applied and never dropped. Exit 0 ok, 2 usage, 3 problems, 4 conflict.
";

struct Words<'a> {
    json: bool,
    rest: Vec<&'a str>,
}

fn words(input: &[String]) -> Words<'_> {
    let mut json = false;
    let mut rest = Vec::with_capacity(input.len());
    for word in input {
        if word == "--json" {
            json = true;
        } else {
            rest.push(word.as_str());
        }
    }
    Words { json, rest }
}

fn pretty(value: &Value) -> String {
    let mut text = value.to_string();
    text.push('\n');
    text
}

fn known_names(root: &Root, known: &[Known]) -> Vec<Name> {
    let mut names: Vec<Name> = known
        .iter()
        .filter_map(|file| Name::parse(file.name).ok())
        .chain(root.recorded())
        .collect();
    names.sort();
    names.dedup();
    names
}

fn named(root: &Root, known: &[Known], word: Option<&str>) -> Result<File, Answer> {
    let names = known_names(root, known);
    let list = || {
        names
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let Some(word) = word else {
        return Err(Answer::usage(&format!("name the shared file: {}", list())));
    };
    let name = Name::parse(word).map_err(|error| Answer::usage(&error.to_string()))?;
    if !names.contains(&name) {
        return Err(Answer::usage(&format!(
            "{name}: no such shared file here (there are: {})",
            list()
        )));
    }
    Ok(File::at(root, &name))
}

fn status_json(file: &File, status: &Status) -> Value {
    let mut object = match status.json() {
        Value::Object(object) => object,
        _ => Map::new(),
    };
    object.insert("file".to_owned(), Value::from(file.name().to_string()));
    object.insert("face".to_owned(), Value::from(file.face().as_str()));
    Value::Object(object)
}

fn status_line(file: &File, status: &Status) -> String {
    let mut line = format!(
        "{}: {} · face={}",
        file.name(),
        status.line(),
        file.face().as_str()
    );
    if let Some(problem) = &status.problem {
        line.push_str(&format!(" · {problem}"));
    }
    line
}

fn row_json(row: &Row) -> Value {
    let mut object = Map::new();
    object.insert(PK.to_owned(), Value::from(row.pk.as_str()));
    object.insert("live".to_owned(), Value::from(row.live));
    object.insert(
        "cells".to_owned(),
        Value::Object(
            row.cells
                .iter()
                .map(|(name, text)| (name.clone(), Value::from(text.as_str())))
                .collect(),
        ),
    );
    object.insert("values".to_owned(), Value::Object(row.values.clone()));
    Value::Object(object)
}

fn table_json(file: &File, read: &Read, columns: &[String], rows: Vec<Value>) -> Value {
    let mut object = match status_json(file, &read.status) {
        Value::Object(object) => object,
        _ => Map::new(),
    };
    object.insert(
        "columns".to_owned(),
        Value::Array(
            columns
                .iter()
                .map(|name| Value::from(name.as_str()))
                .collect(),
        ),
    );
    object.insert("rows".to_owned(), Value::Array(rows));
    Value::Object(object)
}

fn tsv(header: &[String], rows: &[Vec<String>]) -> String {
    let mut text = String::new();
    for line in std::iter::once(header).chain(rows.iter().map(Vec::as_slice)) {
        for (i, cell) in line.iter().enumerate() {
            if i > 0 {
                text.push('\t');
            }
            crate::snapshot::push_cell(&mut text, cell);
        }
        text.push('\n');
    }
    text
}

fn ls(root: &Root, known: &[Known], json: bool) -> Answer {
    let mut lines = String::new();
    let mut files = Vec::new();
    for name in known_names(root, known) {
        let file = File::at(root, &name);
        let status = file.status();
        let compiled = known.iter().find(|file| file.name == name.to_string());
        let kind = compiled.map_or_else(
            || {
                status
                    .state
                    .as_ref()
                    .map(|state| state.kind.clone())
                    .unwrap_or_default()
            },
            |file| file.kind.to_owned(),
        );
        let about = compiled.map_or("", |file| file.about);
        if json {
            let mut object = match status_json(&file, &status) {
                Value::Object(object) => object,
                _ => Map::new(),
            };
            object.insert("kind".to_owned(), Value::from(kind));
            object.insert("about".to_owned(), Value::from(about));
            files.push(Value::Object(object));
        } else {
            lines.push_str(&format!(
                "{name}\t{kind}\t{}\tface={}\t{about}\n",
                status.line(),
                file.face().as_str()
            ));
        }
    }
    if json {
        let mut object = Map::new();
        object.insert("files".to_owned(), Value::Array(files));
        lines = pretty(&Value::Object(object));
    }
    Answer {
        out: lines,
        err: String::new(),
        code: exit::OK,
    }
}

fn cat(file: &File, published: bool, json: bool) -> Answer {
    let read = if published {
        file.published()
    } else {
        file.read()
    };
    if json {
        let columns: Vec<String> = read
            .columns()
            .iter()
            .map(|column| column.name.clone())
            .collect();
        let rows = read.rows().iter().map(row_json).collect();
        return Answer {
            out: pretty(&table_json(file, &read, &columns, rows)),
            err: String::new(),
            code: exit::OK,
        };
    }
    Answer {
        out: read
            .snapshot
            .as_ref()
            .map(crate::Snapshot::face)
            .unwrap_or_default(),
        err: format!("{}\n", status_line(file, &read.status)),
        code: exit::OK,
    }
}

fn path(root: &Root, file: &File, json: bool) -> Answer {
    let ext = file
        .state()
        .map(|state| state.ext)
        .filter(|ext| !ext.is_empty())
        .unwrap_or_else(|| "tsv".to_owned());
    let face = root.face(file.name(), &ext);
    let out = if json {
        let mut object = Map::new();
        object.insert("file".to_owned(), Value::from(file.name().to_string()));
        object.insert(
            "path".to_owned(),
            Value::from(face.to_string_lossy().into_owned()),
        );
        object.insert(
            "snapshot".to_owned(),
            Value::from(root.snapshot(file.name()).to_string_lossy().into_owned()),
        );
        object.insert("face".to_owned(), Value::from(file.face().as_str()));
        pretty(&Value::Object(object))
    } else {
        format!("{}\n", face.display())
    };
    Answer {
        out,
        err: String::new(),
        code: exit::OK,
    }
}

fn status(root: &Root, known: &[Known], word: Option<&str>, json: bool) -> Answer {
    let files = match word {
        Some(word) => match named(root, known, Some(word)) {
            Ok(file) => vec![file],
            Err(answer) => return answer,
        },
        None => known_names(root, known)
            .iter()
            .map(|name| File::at(root, name))
            .collect(),
    };
    let mut out = String::new();
    let mut all = Vec::new();
    for file in &files {
        let status = file.status();
        if json {
            all.push(status_json(file, &status));
        } else {
            out.push_str(&status_line(file, &status));
            out.push('\n');
        }
    }
    if json {
        out = match (word, all.pop()) {
            (Some(_), Some(one)) => pretty(&one),
            (_, last) => {
                all.extend(last);
                let mut object = Map::new();
                object.insert("files".to_owned(), Value::Array(all));
                pretty(&Value::Object(object))
            }
        };
    }
    Answer {
        out,
        err: String::new(),
        code: exit::OK,
    }
}

/// The one row `word` names in `read`: its key, or an exact cell of one of the snapshot's
/// handle columns; `provider:address` is also read as `provider address`.
fn one_row<'a>(file: &File, read: &'a Read, word: &str) -> Result<&'a Row, Answer> {
    let mut rows = read.named(word);
    if rows.is_empty()
        && let Some((before, after)) = word.split_once(':')
    {
        rows = read.named(&format!("{before} {after}"));
    }
    match rows.as_slice() {
        [row] => Ok(row),
        [] => Err(Answer::usage(&format!(
            "{}: no row is named that ({})",
            file.name(),
            read.status.line()
        ))),
        many => {
            let handles = read
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.handles.as_slice())
                .unwrap_or_default();
            let mut why = format!("{}: that names {} rows:\n", file.name(), many.len());
            for row in many {
                for handle in handles {
                    why.push_str(row.cell(handle));
                    why.push_str("  ");
                }
                why.push_str(&row.pk);
                why.push('\n');
            }
            Err(Answer::usage(why.trim_end()))
        }
    }
}

fn get(file: &File, rest: &[&str], json: bool) -> Answer {
    let (Some(word), column) = (rest.first(), rest.get(1)) else {
        return Answer::usage("get <name> <row> [<column>]");
    };
    if rest.len() > 2 {
        return Answer::usage("get <name> <row> [<column>]: quote a row name that holds a space");
    }
    let read = file.read();
    let row = match one_row(file, &read, word) {
        Ok(row) => row,
        Err(answer) => return answer,
    };
    let err = format!("{}\n", status_line(file, &read.status));
    let Some(column) = column else {
        let out = if json {
            let mut object = match row_json(row) {
                Value::Object(object) => object,
                _ => Map::new(),
            };
            object.insert("file".to_owned(), Value::from(file.name().to_string()));
            object.insert("state".to_owned(), Value::from(read.status.word.as_str()));
            pretty(&Value::Object(object))
        } else {
            let mut text = String::new();
            for column in read.columns() {
                let cell = row.cell(&column.name);
                if !cell.is_empty() {
                    text.push_str(&format!("{}\t{cell}\n", column.name));
                }
            }
            text
        };
        return Answer {
            out,
            err: if json { String::new() } else { err },
            code: exit::OK,
        };
    };
    let column = column.trim_start_matches(crate::snapshot::READ_ONLY);
    if !read.columns().iter().any(|known| known.name == column) {
        return Answer::usage(&format!("{}: no column `{column}`", file.name()));
    }
    let cell = row.cell(column);
    let out = if json {
        let mut object = Map::new();
        object.insert("file".to_owned(), Value::from(file.name().to_string()));
        object.insert("state".to_owned(), Value::from(read.status.word.as_str()));
        object.insert(PK.to_owned(), Value::from(row.pk.as_str()));
        object.insert("column".to_owned(), Value::from(column));
        object.insert("cell".to_owned(), Value::from(cell));
        object.insert(
            "value".to_owned(),
            row.values.get(column).cloned().unwrap_or(Value::Null),
        );
        pretty(&Value::Object(object))
    } else {
        format!("{cell}\n")
    };
    Answer {
        out,
        err: if json { String::new() } else { err },
        code: exit::OK,
    }
}

fn rows(root: &Root, file: &File, rest: &[&str], json: bool) -> Answer {
    let mut filters: Vec<(&str, &str)> = Vec::new();
    let mut joins: Vec<&str> = Vec::new();
    let mut words = rest.iter();
    while let Some(word) = words.next() {
        match *word {
            "--where" => match words.next().and_then(|filter| filter.split_once('=')) {
                Some((column, value)) => {
                    filters.push((column.trim_start_matches(crate::snapshot::READ_ONLY), value));
                }
                None => return Answer::usage("--where takes col=val"),
            },
            "--join" => match words.next() {
                Some(column) => joins.push(column),
                None => {
                    return Answer::usage("--join takes a column that names another file's rows");
                }
            },
            other => return Answer::usage(&format!("rows: unexpected `{other}`")),
        }
    }
    let read = file.read();
    let known = |name: &str| read.columns().iter().any(|column| column.name == name);
    let mut header: Vec<String> = read
        .columns()
        .iter()
        .map(|column| column.name.clone())
        .collect();
    if read.snapshot.is_some() {
        for (column, _) in &filters {
            if !known(column) {
                return Answer::usage(&format!("{}: no column `{column}`", file.name()));
            }
        }
    }
    // Each joined file is read once: its rows by key, under `<ref column>.<its column>`.
    let mut joined: Vec<(&str, Read)> = Vec::new();
    for column in joins {
        let Some(other) = read
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.column(column))
            .and_then(|column| column.refers.as_deref())
            .and_then(|name| Name::parse(name).ok())
        else {
            if read.snapshot.is_none() {
                continue;
            }
            return Answer::usage(&format!(
                "{}: `{column}` names no other shared file's rows",
                file.name()
            ));
        };
        let referred = File::at(root, &other);
        let other = referred.read();
        header.extend(
            other
                .columns()
                .iter()
                .filter(|theirs| theirs.name != PK)
                .map(|theirs| format!("{column}.{}", theirs.name)),
        );
        joined.push((column, other));
    }
    let mut lines = Vec::new();
    let mut values = Vec::new();
    for row in read.rows() {
        if !filters
            .iter()
            .all(|(column, value)| row.cell(column) == *value)
        {
            continue;
        }
        let mut line: Vec<String> = read
            .columns()
            .iter()
            .map(|column| row.cell(&column.name).to_owned())
            .collect();
        let mut value = match row_json(row) {
            Value::Object(object) => object,
            _ => Map::new(),
        };
        for (column, other) in &joined {
            let theirs = other
                .rows()
                .iter()
                .find(|theirs| theirs.pk == row.cell(column));
            let mut cells = Map::new();
            for their_column in other.columns().iter().filter(|c| c.name != PK) {
                let cell = theirs.map_or("", |theirs| theirs.cell(&their_column.name));
                line.push(cell.to_owned());
                if !cell.is_empty() {
                    cells.insert(their_column.name.clone(), Value::from(cell));
                }
            }
            value.insert(format!("{column}.cells"), Value::Object(cells));
        }
        lines.push(line);
        values.push(Value::Object(value));
    }
    if json {
        return Answer {
            out: pretty(&table_json(file, &read, &header, values)),
            err: String::new(),
            code: exit::OK,
        };
    }
    Answer {
        out: if read.snapshot.is_some() {
            tsv(&header, &lines)
        } else {
            String::new()
        },
        err: format!("{}\n", status_line(file, &read.status)),
        code: exit::OK,
    }
}

fn history(file: &File, rest: &[&str], json: bool) -> Answer {
    let limit = match rest {
        [] => 50,
        ["--limit", count] => match count.parse::<usize>() {
            Ok(count) => count,
            Err(_) => return Answer::usage("--limit takes a count"),
        },
        _ => return Answer::usage("history <name> [--limit <n>]"),
    };
    let all = file.history();
    let newest = &all[all.len().saturating_sub(limit)..];
    let out = if json {
        let mut object = Map::new();
        object.insert("file".to_owned(), Value::from(file.name().to_string()));
        object.insert("state".to_owned(), Value::from(file.status().word.as_str()));
        object.insert("operations".to_owned(), Value::Array(newest.to_vec()));
        pretty(&Value::Object(object))
    } else {
        let mut text = String::new();
        for entry in newest {
            let word = |name: &str| entry.get(name).and_then(Value::as_str).unwrap_or("-");
            let keys = entry
                .get("keys")
                .and_then(Value::as_array)
                .map(|keys| {
                    keys.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            text.push_str(&format!(
                "{}\t{}\t{}\t{keys}\n",
                entry.get("ms").and_then(Value::as_u64).unwrap_or(0),
                word("op"),
                word("by"),
            ));
        }
        text
    };
    Answer {
        out,
        err: String::new(),
        code: exit::OK,
    }
}

/// One queued or held intent as `outbox` lists it: which, when, whose and which cells, never
/// a value.
fn intent_json(intent: &Intent, state: &str, why: Option<&str>) -> Value {
    let mut object = Map::new();
    object.insert("id".to_owned(), Value::from(intent.id.as_str()));
    object.insert("file".to_owned(), Value::from(intent.file.to_string()));
    object.insert("state".to_owned(), Value::from(state));
    object.insert("ms".to_owned(), Value::from(intent.ms));
    object.insert("by".to_owned(), Value::from(intent.by.as_str()));
    object.insert("session".to_owned(), Value::from(intent.session.as_deref()));
    object.insert("born".to_owned(), Value::from(intent.born));
    object.insert("revert".to_owned(), Value::from(intent.revert.as_deref()));
    object.insert(
        "keys".to_owned(),
        Value::Array(intent.keys().into_iter().map(Value::from).collect()),
    );
    object.insert("why".to_owned(), Value::from(why));
    Value::Object(object)
}

fn intent_line(intent: &Intent, state: &str, why: Option<&str>) -> String {
    let what = match &intent.revert {
        Some(operation) => format!("revert {operation}"),
        None => intent.keys().join(" "),
    };
    let mut line = format!(
        "{}\t{}\t{state}\t{}\t{}\t{what}",
        intent.id, intent.file, intent.ms, intent.by
    );
    if let Some(why) = why {
        line.push('\t');
        line.push_str(why);
    }
    line.push('\n');
    line
}

#[cfg(any(unix, windows))]
fn drop_one(file: &File, id: &str) -> std::io::Result<Dropped> {
    file.drop_intent(id)
}

#[cfg(not(any(unix, windows)))]
fn drop_one(_: &File, _: &str) -> std::io::Result<Dropped> {
    Err(std::io::Error::other("no outbox on this platform"))
}

/// `outbox [<name>]` and `outbox drop <id>`.
fn outbox(root: &Root, known: &[Known], rest: &[&str], json: bool) -> Answer {
    if rest.first() == Some(&"drop") {
        let [_, id] = rest else {
            return Answer::usage("outbox drop <id>");
        };
        for name in known_names(root, known) {
            match drop_one(&File::at(root, &name), id) {
                Ok(Dropped::Absent) => {}
                Ok(Dropped::Published) => {
                    return Answer {
                        out: String::new(),
                        err: format!(
                            "aterm file: {name}: {id} is already published; nothing was dropped \
                             (`aterm file revert` undoes its operation)\n"
                        ),
                        code: exit::PROBLEMS,
                    };
                }
                Ok(Dropped::Dropped) => {
                    let out = if json {
                        let mut object = Map::new();
                        object.insert("dropped".to_owned(), Value::from(*id));
                        object.insert("file".to_owned(), Value::from(name.to_string()));
                        pretty(&Value::Object(object))
                    } else {
                        format!("dropped {id} ({name})\n")
                    };
                    return Answer {
                        out,
                        err: String::new(),
                        code: exit::OK,
                    };
                }
                Err(error) => {
                    return Answer {
                        out: String::new(),
                        err: format!(
                            "aterm file: {name}: its outbox could not be written: {error}\n"
                        ),
                        code: exit::PROBLEMS,
                    };
                }
            }
        }
        return Answer::usage(&format!("no queued or held write has the id {id}"));
    }
    let files = match rest {
        [] => known_names(root, known)
            .iter()
            .map(|name| File::at(root, name))
            .collect(),
        [word] => match named(root, known, Some(word)) {
            Ok(file) => vec![file],
            Err(answer) => return answer,
        },
        _ => return Answer::usage("outbox [<name>] | outbox drop <id>"),
    };
    let mut lines = String::new();
    let mut intents = Vec::new();
    let mut problems = String::new();
    for file in &files {
        let outbox = file.outbox();
        for intent in outbox.waiting() {
            if json {
                intents.push(intent_json(intent, "queued", None));
            } else {
                lines.push_str(&intent_line(intent, "queued", None));
            }
        }
        for held in &outbox.held {
            if json {
                intents.push(intent_json(&held.intent, "held", Some(&held.why)));
            } else {
                lines.push_str(&intent_line(&held.intent, "held", Some(&held.why)));
            }
        }
        if let Some(problem) = &outbox.problem {
            problems.push_str(&format!("{}: {problem}\n", file.name()));
        }
    }
    if json {
        let mut object = Map::new();
        object.insert("intents".to_owned(), Value::Array(intents));
        lines = pretty(&Value::Object(object));
    }
    Answer {
        out: lines,
        err: problems,
        code: exit::OK,
    }
}

/// What `watch` was asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Watch {
    pub json: bool,
    /// Stop after this long.
    pub timeout: Option<Duration>,
    /// Stop after this many lines past the first.
    pub changes: Option<u64>,
}

/// A parsed `aterm file` invocation: an answer made at once, or a watch the caller runs with
/// its own stdout ([`watch`]).
#[derive(Debug)]
pub enum Run {
    Answer(Answer),
    Watch(File, Watch),
}

/// Run one read verb. `known` names the files this build compiles; a file a pass recorded here
/// is read too.
#[must_use]
pub fn run(root: &Root, known: &[Known], input: &[String]) -> Run {
    let Words { json, rest } = words(input);
    let Some((&verb, rest)) = rest.split_first() else {
        return Run::Answer(Answer {
            out: USAGE.to_owned(),
            err: String::new(),
            code: exit::OK,
        });
    };
    let file = |rest: &[&str]| named(root, known, rest.first().copied());
    let only_name = |rest: &[&str], verb: &str| {
        (rest.len() > 1)
            .then(|| Answer::usage(&format!("{verb} <name>: unexpected words after it")))
    };
    Run::Answer(match verb {
        "help" | "--help" | "-h" => Answer {
            out: USAGE.to_owned(),
            err: String::new(),
            code: exit::OK,
        },
        "ls" if rest.is_empty() => ls(root, known, json),
        "ls" => Answer::usage("ls takes no words"),
        "cat" => {
            let published = rest.contains(&"--published");
            let rest: Vec<&str> = rest
                .iter()
                .copied()
                .filter(|word| *word != "--published")
                .collect();
            match (file(&rest), only_name(&rest, "cat")) {
                (Err(answer), _) | (Ok(_), Some(answer)) => answer,
                (Ok(file), None) => cat(&file, published, json),
            }
        }
        "outbox" => outbox(root, known, rest, json),
        "path" => match (file(rest), only_name(rest, "path")) {
            (Err(answer), _) | (Ok(_), Some(answer)) => answer,
            (Ok(file), None) => path(root, &file, json),
        },
        "status" if rest.len() <= 1 => status(root, known, rest.first().copied(), json),
        "status" => Answer::usage("status [<name>]"),
        "get" => match file(rest) {
            Ok(file) => get(&file, &rest[1..], json),
            Err(answer) => answer,
        },
        "rows" => match file(rest) {
            Ok(file) => rows(root, &file, &rest[1..], json),
            Err(answer) => answer,
        },
        "history" => match file(rest) {
            Ok(file) => history(&file, &rest[1..], json),
            Err(answer) => answer,
        },
        "watch" => {
            let file = match file(rest) {
                Ok(file) => file,
                Err(answer) => return Run::Answer(answer),
            };
            let mut watch = Watch {
                json,
                ..Watch::default()
            };
            let mut words = rest[1..].iter();
            while let Some(word) = words.next() {
                let count = words.next().and_then(|count| count.parse::<u64>().ok());
                match (*word, count) {
                    ("--timeout", Some(seconds)) => {
                        watch.timeout = Some(Duration::from_secs(seconds));
                    }
                    ("--changes", Some(count)) => watch.changes = Some(count),
                    _ => {
                        return Run::Answer(Answer::usage(
                            "watch <name> [--timeout <seconds>] [--changes <n>]",
                        ));
                    }
                }
            }
            return Run::Watch(file, watch);
        }
        other => Answer {
            out: String::new(),
            err: format!("aterm file: no verb `{other}`\n{USAGE}"),
            code: exit::USAGE,
        },
    })
}

/// How often `watch` looks at the status file.
const WATCH_EVERY: Duration = Duration::from_millis(250);

/// Follow `file`: one line now, then one each time a pass records a different state word, why
/// or revision. `emit` writes a line and says whether the reader is still there; `pause` waits
/// one look's interval. Returns when the reader is gone, the timeout passes or the asked count
/// of changes was written.
pub fn watch(
    file: &File,
    watch: Watch,
    emit: &mut dyn FnMut(&str) -> bool,
    pause: &mut dyn FnMut(Duration),
) -> u8 {
    // A timeout past what the clock can count is no timeout.
    let until = watch
        .timeout
        .and_then(|timeout| Instant::now().checked_add(timeout));
    let line = |status: &Status| {
        if watch.json {
            status_json(file, status).to_string()
        } else {
            status_line(file, status)
        }
    };
    let key = |status: &Status| (status.word, status.why, status.rev());
    let mut status = file.status();
    if !emit(&line(&status)) {
        return exit::OK;
    }
    let mut written = 0_u64;
    loop {
        if watch.changes.is_some_and(|changes| written >= changes)
            || until.is_some_and(|until| Instant::now() >= until)
        {
            return exit::OK;
        }
        pause(WATCH_EVERY);
        let now = file.status();
        if key(&now) != key(&status) {
            status = now;
            written += 1;
            if !emit(&line(&status)) {
                return exit::OK;
            }
        }
    }
}
