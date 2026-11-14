// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The write verbs of `aterm file` (docs/DESIGN-shared-files-2026-11-11.md §3, §4):
//! `set <name> <row> <column> [<value> | --stdin]`, `add`, `rm`, `revert` and `resolve`. Each
//! reads the owner's words against what this Mac's last snapshot showed (its baseline: every
//! cell's candidates), queues one intent (`aterm_file::File::write`, which opens no custody),
//! gives the publisher two seconds ([`publish::INLINE`]) and answers `published <operation>`,
//! `queued <id>: <why>` (exit 0) or `held <id>: <why>` (exit 4, a conflict recorded), or with
//! `--json` the same as one object.
//!
//! - **What the author saw.** An edit's expected candidates are the baseline's for its
//!   register. Where an earlier intent of this Mac still waits on that register, or was
//!   published and the snapshot does not hold it yet, the edit says it follows that intent
//!   (`intent:<id>`): the publisher compares it with what that intent left in the register, so
//!   a second write of a cell is compared with the first, never held against it.
//! - **No value in an answer.** A refusal names the row, the column and the rule.

use super::{
    Type,
    materialize::{self},
    publish::{self, Answer},
};
use crate::{
    att, generation,
    typed::{
        Collection, Column, LIFECYCLE, Ty,
        grid::{self, Base, Seen},
        keys,
    },
};
use aterm_file::{Draft, Dropped, Edit, File, Name, Outbox, OwnerAct, exit, outbox};
use aterm_json::Value;
use aterm_secrets::client::Paths;
use std::{collections::BTreeMap, path::Path, time::Duration};

/// Who runs a verb, and how long its own publication attempt may take.
#[derive(Clone, Copy, Debug)]
pub struct Env<'a> {
    /// Who says they ran it: `person`, or a program's word.
    pub by: &'a str,
    /// The aterm session it ran in.
    pub session: Option<&'a str>,
    /// This Mac's clock, Unix milliseconds.
    pub now_ms: u64,
    /// The publisher's bound.
    pub attempt: Duration,
    /// Answer as one JSON object (`--json`).
    pub json: bool,
    /// What the verb's standard input held, for `set … --stdin`; `None` where it was not read.
    pub stdin: Option<&'a str>,
}

/// Why a verb wrote nothing.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The words are not the verb's: the usage line, for exit [`exit::USAGE`].
    Usage(String),
    /// The write cannot be made here now: exit [`exit::PROBLEMS`].
    Problem(String),
}

fn usage(why: impl Into<String>) -> Refusal {
    Refusal::Usage(why.into())
}

fn problem(why: impl ToString) -> Refusal {
    Refusal::Problem(why.to_string())
}

/// The file a verb names, and what this Mac holds of it.
struct Target {
    file: &'static Type,
    held: File,
    name: Name,
}

impl Target {
    fn of(paths: &Paths, name: &str) -> Result<Self, Refusal> {
        let name = Name::parse(name).map_err(|error| usage(error.to_string()))?;
        let Some(file) = super::find(&name) else {
            return Err(usage(format!("{name}: no such shared file in this build")));
        };
        let root = super::root(&generation::original(paths));
        Ok(Self {
            file,
            held: File::at(&root, &name),
            name,
        })
    }

    /// The baseline of the snapshot here, and the collection it was laid out at.
    fn baseline(&self) -> Result<(Base, &'static Collection), Refusal> {
        let rev = self.held.published().status.rev();
        let stale = || {
            problem(format!(
                "{}: nothing is materialized here to write against; run `aterm file refresh {}`",
                self.name, self.name
            ))
        };
        let bytes = self.held.baseline(rev).ok_or_else(stale)?;
        let value: Value = aterm_json::from_slice(&bytes).map_err(|_| stale())?;
        let base = Base::from_json(&value).map_err(|_| stale())?;
        let collection = att::find(&base.table)
            .and_then(|compiled| compiled.collection(&base.collection))
            .map_err(|_| stale())?;
        if collection.name != self.file.collection.name {
            return Err(stale());
        }
        Ok((base, collection))
    }

    /// The key of the one published row `word` names: its key, or a handle.
    fn row(&self, base: &Base, word: &str) -> Result<String, Refusal> {
        let read = self.held.read();
        let rows = read.named(word);
        let pk = match rows.as_slice() {
            [row] => row.pk.clone(),
            [] => return Err(problem(format!("{}: no row {word}", self.name))),
            many => {
                return Err(problem(format!(
                    "{}: {word} names {} rows; name one by its key",
                    self.name,
                    many.len()
                )));
            }
        };
        if !base.rows.iter().any(|(row, _)| *row == pk) {
            return Err(problem(format!(
                "{}: row {word} is not published yet; it is written once its own write is",
                self.name
            )));
        }
        Ok(pk)
    }
}

/// The value `set` writes from its standard input: the input less one line ending.
fn from_stdin(text: &str) -> &str {
    let text = text.strip_suffix('\n').unwrap_or(text);
    text.strip_suffix('\r').unwrap_or(text)
}

fn assignment(word: &str) -> Result<(&str, &str), Refusal> {
    word.split_once('=')
        .filter(|(column, _)| !column.is_empty())
        .ok_or_else(|| usage(format!("`{word}` is not <column>=<value>")))
}

/// The column of `collection` an owner's verb may write.
fn writable(
    name: &Name,
    collection: &'static Collection,
    column: &str,
) -> Result<&'static Column, Refusal> {
    let Some(schema) = collection
        .column(column)
        .filter(|schema| schema.live_at(collection.version))
    else {
        return Err(problem(format!("{name}: no column {column}")));
    };
    if grid::read_only(schema) || matches!(schema.ty, Ty::Secret) {
        return Err(problem(format!("{name}: column {column} is read-only")));
    }
    Ok(schema)
}

fn hex(digests: &[astream_wire::sync::Digest]) -> Vec<String> {
    digests
        .iter()
        .map(|digest| aterm_codec::hex::encode_lower(digest))
        .collect()
}

/// `edit` as the outbox keeps it.
fn kept(column: &Column, edit: grid::Edit, born: bool) -> Edit {
    let text = match (&column.ty, &edit.elem) {
        // A set's whole cell, as the owner typed it.
        (Ty::Set(_), Some(_)) => edit.mine.clone(),
        _ if edit.x.is_null() => String::new(),
        _ => grid::show(&column.ty, &edit.x, true),
    };
    Edit {
        key: if born { String::new() } else { edit.pk },
        column: edit.column,
        elem: edit.elem,
        value: edit.x,
        text,
        expected: hex(&edit.expected),
    }
}

/// What each edit's author saw where this Mac's own earlier writes are ahead of the snapshot:
/// the latest intent of this Mac on the same register that still waits, or that was published
/// and is not in the snapshot yet (`intent:<id>`). The publisher compares the edit with what
/// that intent left in the register.
fn follow(held: &File, outbox: &Outbox, edits: &mut [Edit]) {
    let frontier = held.frontier();
    let same = |a: &Edit, b: &Edit| a.key == b.key && a.column == b.column && a.elem == b.elem;
    for edit in edits {
        let waiting = outbox
            .waiting()
            .filter(|intent| intent.edits.iter().any(|earlier| same(earlier, edit)))
            .last()
            .map(|intent| intent.id.clone());
        let published = || {
            outbox
                .done
                .iter()
                .rev()
                .filter(|done| {
                    // Settled with nothing signed: the snapshot holds what it left once a pass
                    // has run since (`seen`).
                    !done.seen && (done.op.is_none() || !done.projected(frontier.as_ref()))
                })
                .find(|done| done.intent.edits.iter().any(|e| same(e, edit)))
                .map(|done| done.intent.id.clone())
        };
        if let Some(id) = waiting.or_else(published) {
            edit.expected = vec![format!("{}{id}", outbox::INTENT)];
        }
    }
}

/// The edit `set <row> <column> [<value>]` asks for: `None` clears the cell.
fn set(target: &Target, key: &str, column: &str, text: Option<&str>) -> Result<Vec<Edit>, Refusal> {
    let text = text.unwrap_or_default();
    let (base, collection) = target.baseline()?;
    let pk = target.row(&base, key)?;
    let seen: BTreeMap<String, Seen> = base
        .rows
        .iter()
        .find(|(row, _)| *row == pk)
        .map(|(_, seen)| seen.clone())
        .unwrap_or_default();
    let read = target.held.read();
    let shown = |column: &str| -> String {
        read.rows()
            .iter()
            .find(|row| row.pk == pk)
            .map(|row| row.cell(column).to_owned())
            .unwrap_or_default()
    };
    let schema = writable(&target.name, collection, column)?;
    if schema.fixed {
        return Err(problem(format!(
            "{}: the row's key comes from {column}; retire this row and add another",
            target.name
        )));
    }
    let now = shown(column);
    if now == text {
        return Ok(Vec::new());
    }
    // The candidates are the baseline's; the text an edit is read against is what a read
    // shows now, this Mac's queued writes included.
    let mut cell = seen.get(column).cloned().unwrap_or_default();
    if cell.raw {
        return Err(problem(format!(
            "{}: {column} holds a value a newer build wrote; it is read-only here",
            target.name
        )));
    }
    cell.shown = now;
    let planned = grid::cell_edits(collection, &pk, schema, text, Some(&cell))
        .map_err(|rule| problem(format!("{}: {rule}", target.name)))?;
    Ok(planned
        .into_iter()
        .map(|edit| kept(schema, edit, false))
        .collect())
}

/// The edits `add <column>=<value>…` asks for: one new row's cells, its key left to the
/// publisher, which derives it under the writer from the table as it is then.
fn add(target: &Target, assigns: &[String]) -> Result<Vec<Edit>, Refusal> {
    if assigns.is_empty() {
        return Err(usage("add <name> <column>=<value>..."));
    }
    // The collection the snapshot was laid out at, or the one this build compiles where
    // nothing is materialized yet: a birth is compared with nothing.
    let collection = target
        .baseline()
        .map_or(target.file.collection, |(_, collection)| collection);
    let mut edits = Vec::new();
    let mut given: Vec<&str> = Vec::new();
    for word in assigns {
        let (column, text) = assignment(word)?;
        let schema = writable(&target.name, collection, column)?;
        if given.contains(&schema.name) {
            return Err(usage(format!("{column} is given twice")));
        }
        given.push(schema.name);
        if text.is_empty() {
            continue;
        }
        // The key is not known yet: the values are read under a stand-in key of the grammar,
        // and checked again under the real one when the publisher signs.
        let planned = grid::cell_edits(collection, STAND_IN, schema, text, None)
            .map_err(|rule| problem(format!("{}: {rule}", target.name)))?;
        edits.extend(planned.into_iter().map(|edit| kept(schema, edit, true)));
    }
    for schema in collection.columns {
        let required = schema.live_at(collection.version)
            && !schema.null
            && !grid::read_only(schema)
            && schema.name != LIFECYCLE
            && !matches!(schema.ty, Ty::Set(_) | Ty::Secret);
        if required && !edits.iter().any(|edit| edit.column == schema.name) {
            return Err(problem(format!(
                "{}: a new row needs a value for {}",
                target.name, schema.name
            )));
        }
    }
    if !edits.iter().any(|edit| edit.column == LIFECYCLE)
        && let Some(lifecycle) = collection.column(LIFECYCLE)
    {
        let planned = grid::cell_edits(collection, STAND_IN, lifecycle, "live", None)
            .map_err(|rule| problem(format!("{}: {rule}", target.name)))?;
        edits.extend(planned.into_iter().map(|edit| kept(lifecycle, edit, true)));
    }
    Ok(edits)
}

/// A key of the grammar that names no row: what a new row's values are read under before the
/// publisher derives its key.
const STAND_IN: &str = "new";

/// The edit `rm <key>` asks for: the row retired (or ended, where the table says its rows
/// end), so its history stays.
fn rm(target: &Target, key: &str) -> Result<Vec<Edit>, Refusal> {
    let (base, collection) = target.baseline()?;
    let pk = target.row(&base, key)?;
    let Some(lifecycle) = collection.column(LIFECYCLE) else {
        return Err(problem(format!(
            "{}: its rows have no lifecycle to retire",
            target.name
        )));
    };
    let ends =
        att::find(&base.table).is_ok_and(|compiled| compiled.table.ends.contains(&collection.name));
    let word = if ends { "ended" } else { "retired" };
    let seen = base
        .rows
        .iter()
        .find(|(row, _)| *row == pk)
        .and_then(|(_, seen)| seen.get(LIFECYCLE));
    if seen.is_some_and(|seen| seen.shown == word) {
        return Ok(Vec::new());
    }
    let planned = grid::cell_edits(collection, &pk, lifecycle, word, seen)
        .map_err(|rule| problem(format!("{}: {rule}", target.name)))?;
    Ok(planned
        .into_iter()
        .map(|edit| kept(lifecycle, edit, false))
        .collect())
}

/// The held intent's edits against the candidates the snapshot shows now: the owner keeps their
/// side. A held row to be born or revert would be held again for the same reason (the row its
/// key derives is there, the reverted operation's cells moved), so neither is queued again.
fn again(target: &Target, held: &aterm_file::Intent) -> Result<Draft, Refusal> {
    if held.born {
        return Err(problem(format!(
            "{}: {} is a new row that cannot be born as it was written (its row is there now, \
             or this build refuses it); write its cells with `aterm file set` on that row, and \
             `resolve {} {} theirs` drops this one",
            target.name, held.id, target.name, held.id
        )));
    }
    if held.revert.is_some() {
        return Err(problem(format!(
            "{}: {} is a revert whose operation's cells changed since; revert the operation \
             that wrote them now, and `resolve {} {} theirs` drops this one",
            target.name, held.id, target.name, held.id
        )));
    }
    let (base, _) = target.baseline()?;
    let mut edits = held.edits.clone();
    for edit in &mut edits {
        let seen = base
            .rows
            .iter()
            .find(|(row, _)| *row == edit.key)
            .and_then(|(_, seen)| seen.get(&edit.column));
        edit.expected = match (&edit.elem, seen) {
            (Some(elem), Some(seen)) => seen.elements.get(elem).map(|now| hex(now)),
            (None, Some(seen)) => Some(hex(&seen.candidates)),
            (_, None) => None,
        }
        .unwrap_or_default();
    }
    follow(&target.held, &target.held.outbox(), &mut edits);
    Ok(Draft {
        born: false,
        revert: None,
        edits,
    })
}

/// What a write verb did, as it prints.
#[derive(Debug)]
enum Said {
    Published { id: String, op: Option<String> },
    Queued { id: String, why: String },
    Held { id: String, why: String },
    Unchanged,
    Dropped { id: String },
}

/// `said` for `name`, with `unflushed` (the outbox holds the intent, but its folder's flush
/// failed): the text or JSON to print and the exit code. A held intent is a conflict recorded.
fn answer(name: &Name, said: Said, unflushed: Option<&str>, json: bool) -> (String, u8) {
    let code = if matches!(said, Said::Held { .. }) {
        exit::CONFLICT
    } else {
        exit::OK
    };
    let (state, id, op, why) = match &said {
        Said::Published { id, op } => ("published", Some(id), op.as_ref(), None),
        Said::Queued { id, why } => ("queued", Some(id), None, Some(why)),
        Said::Held { id, why } => ("held", Some(id), None, Some(why)),
        Said::Unchanged => ("unchanged", None, None, None),
        Said::Dropped { id } => ("dropped", Some(id), None, None),
    };
    if json {
        let mut object = aterm_json::Map::new();
        object.insert("file".to_owned(), Value::from(name.to_string()));
        object.insert("state".to_owned(), Value::from(state));
        object.insert("id".to_owned(), Value::from(id.map(String::as_str)));
        object.insert("op".to_owned(), Value::from(op.map(String::as_str)));
        object.insert("why".to_owned(), Value::from(why.map(String::as_str)));
        object.insert("unflushed".to_owned(), Value::from(unflushed));
        return (format!("{}\n", Value::Object(object)), code);
    }
    let mut text = match &said {
        Said::Published { op: Some(op), .. } => format!("published {op}\n"),
        Said::Published { id, op: None } => {
            format!("published: the table already says what {id} writes\n")
        }
        Said::Queued { id, why } => format!("queued {id}: {why}\n"),
        Said::Held { id, why } => format!("held {id}: {why}\n"),
        Said::Unchanged => format!("unchanged: {name} already says that\n"),
        Said::Dropped { id } => format!("dropped {id}: theirs kept\n"),
    };
    if let (Some(why), Some(id)) = (unflushed, id) {
        text.push_str(&format!(
            "note: {id} is in the outbox, but its folder's flush failed ({why}); a power loss \
             before it is published may lose it\n"
        ));
    }
    (text, code)
}

/// Give the publisher its bound for the intent `id`, and say what became of it.
fn attempt(paths: &Paths, config: &Path, target: &Target, id: &str, env: &Env<'_>) -> Said {
    let outcomes = publish::pass(
        paths,
        config,
        Some(&target.name),
        &publish::Pass {
            by: std::time::Instant::now() + env.attempt,
            now_ms: env.now_ms,
        },
    );
    let answer = outcomes
        .iter()
        .find_map(|outcome| outcome.answer(id).cloned());
    let id = id.to_owned();
    match answer {
        Some(Answer::Published { op }) => {
            // The snapshot takes the record up now rather than at the next pass; a pass that
            // cannot leaves the read laying this write over the old snapshot.
            let _ = materialize::pass(
                paths,
                config,
                Some(&target.name),
                &materialize::Pass {
                    now_ms: env.now_ms,
                    ..materialize::Pass::now(false)
                },
            );
            Said::Published { id, op }
        }
        Some(Answer::Held { why }) => Said::Held { id, why },
        Some(Answer::Queued { why }) => Said::Queued { id, why },
        None => Said::Queued {
            id,
            why: "the publisher has not reached it".to_owned(),
        },
    }
}

/// What `set <name> <row> <column> [<value>]` queues, read against this Mac's last snapshot:
/// `None` where the cell already says the value (no value: the cell is empty). The conformance
/// test accepts it itself, so its accept is a step of its own.
pub(crate) fn draft_set(
    paths: &Paths,
    name: &str,
    key: &str,
    column: &str,
    value: Option<&str>,
) -> Result<Option<Draft>, Refusal> {
    let target = Target::of(paths, name)?;
    let mut edits = set(&target, key, column, value)?;
    if edits.is_empty() {
        return Ok(None);
    }
    follow(&target.held, &target.held.outbox(), &mut edits);
    Ok(Some(Draft {
        born: false,
        revert: None,
        edits,
    }))
}

/// Whether `word` is a write verb of `aterm file`.
#[must_use]
pub fn is_verb(word: &str) -> bool {
    matches!(word, "set" | "add" | "rm" | "revert" | "resolve")
}

/// One write verb over `paths`, and what to print with its exit code.
///
/// # Errors
/// Nothing was queued: the words are not the verb's, or the write cannot be made here now.
pub fn verb(
    paths: &Paths,
    config: &Path,
    words: &[String],
    env: &Env<'_>,
) -> Result<(String, u8), Refusal> {
    let act = OwnerAct::verb(env.by, env.session)
        .ok_or_else(|| usage("--by takes one word [a-z][a-z0-9-]{0,19}"))?;
    let word = |at: usize, what: &str| -> Result<&str, Refusal> {
        words.get(at).map(String::as_str).ok_or_else(|| usage(what))
    };
    let verb = word(0, "set|add|rm|revert|resolve <name> ...")?;
    let queued = |target: &Target, draft: Draft| -> Result<(String, u8), Refusal> {
        let accepted = target
            .held
            .write(&act, draft, env.now_ms)
            .map_err(problem)?;
        let said = attempt(paths, config, target, &accepted.id, env);
        Ok(answer(
            &target.name,
            said,
            accepted.unflushed.as_deref(),
            env.json,
        ))
    };
    let edits = |target: &Target, mut edits: Vec<Edit>, born: bool| {
        if edits.is_empty() {
            return Ok(answer(&target.name, Said::Unchanged, None, env.json));
        }
        if !born {
            follow(&target.held, &target.held.outbox(), &mut edits);
        }
        queued(
            target,
            Draft {
                born,
                revert: None,
                edits,
            },
        )
    };
    match verb {
        "set" => {
            let what = "set <name> <row> <column> [<value> | --stdin]";
            let target = Target::of(paths, word(1, what)?)?;
            let (key, column) = (word(2, what)?, word(3, what)?);
            let value = match &words[4..] {
                [] => None,
                [stdin] if stdin == "--stdin" => {
                    Some(from_stdin(env.stdin.ok_or_else(|| {
                        problem("--stdin: nothing was read from standard input")
                    })?))
                }
                [value] => Some(value.as_str()),
                _ => return Err(usage(what)),
            };
            match draft_set(paths, target.file.name, key, column, value)? {
                Some(draft) => queued(&target, draft),
                None => Ok(answer(&target.name, Said::Unchanged, None, env.json)),
            }
        }
        "add" => {
            let target = Target::of(paths, word(1, "add <name> <column>=<value>...")?)?;
            let planned = add(&target, &words[2..])?;
            edits(&target, planned, true)
        }
        "rm" => {
            let what = "rm <name> <key>";
            let target = Target::of(paths, word(1, what)?)?;
            if words.len() != 3 {
                return Err(usage(what));
            }
            let planned = rm(&target, word(2, what)?)?;
            edits(&target, planned, false)
        }
        "revert" => {
            let what = "revert <name> <operation>";
            let target = Target::of(paths, word(1, what)?)?;
            let operation = word(2, what)?;
            if words.len() != 3 || outbox::digest(operation).is_none() {
                return Err(usage(what));
            }
            queued(
                &target,
                Draft {
                    born: false,
                    revert: Some(operation.to_owned()),
                    edits: Vec::new(),
                },
            )
        }
        "resolve" => {
            let what = "resolve <name> <id> mine|theirs";
            let target = Target::of(paths, word(1, what)?)?;
            let (id, side) = (word(2, what)?, word(3, what)?);
            if words.len() != 4 {
                return Err(usage(what));
            }
            let no_such = || {
                problem(format!(
                    "{}: no held intent {}",
                    target.name,
                    keys::safe_key(id)
                ))
            };
            let Some(held) = target
                .held
                .outbox()
                .held
                .into_iter()
                .find(|held| held.intent.id == id)
            else {
                return Err(no_such());
            };
            match side {
                "theirs" => match target.held.drop_intent(id).map_err(problem)? {
                    Dropped::Dropped => Ok(answer(
                        &target.name,
                        Said::Dropped { id: id.to_owned() },
                        None,
                        env.json,
                    )),
                    Dropped::Published | Dropped::Absent => Err(no_such()),
                },
                "mine" => {
                    let draft = again(&target, &held.intent)?;
                    let accepted = target
                        .held
                        .requeue(&act, id, draft, env.now_ms)
                        .map_err(problem)?
                        .ok_or_else(no_such)?;
                    let said = attempt(paths, config, &target, &accepted.id, env);
                    Ok(answer(
                        &target.name,
                        said,
                        accepted.unflushed.as_deref(),
                        env.json,
                    ))
                }
                _ => Err(usage(what)),
            }
        }
        _ => Err(usage("set|add|rm|revert|resolve <name> ...")),
    }
}
