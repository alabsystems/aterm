// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A shared file's outbox (docs/DESIGN-shared-files-2026-11-11.md §1, §2 "Bounds", §4 "Write"):
//! the durable, unsigned write intents this Mac made and has not yet seen published.
//!
//! - **An intent** is `{id, file, ms, by, edits: [{key, column, value, expected}]}`. It names no
//!   actor and carries no signature: whichever writer this Mac's lineage has when the publisher
//!   runs signs it, so an intent made on a fenced Mac is signed by its admitted successor. Each
//!   edit names the candidates its author observed (`expected`): operation digests, or
//!   `intent:<id>` for the write of an intent queued before it. One that keeps a person's side
//!   of a held intent names it (`resolves`), so the held one is resolved once, whatever a crash
//!   between the two writes leaves.
//! - **No refused value.** An intent's values are held to the credential and phone guard
//!   ([`aterm_value_guard`]) before it is accepted, whichever writer offers it (§5, rule 6), and
//!   a read shows [`aterm_value_guard::REFUSED`] for any such text another build left here.
//! - **Three files**, all JSON Lines beside the snapshot: `<leaf>.outbox.jsonl` (queued),
//!   `<leaf>.held.jsonl` (held: its register moved, or this build refuses it; shown as
//!   `⚠ mine ‖ theirs`, never applied and never dropped but by a person's word) and
//!   `<leaf>.done` (the marker of each intent a record carries, written before the intent
//!   leaves the outbox, so a publisher killed between the two republishes nothing).
//! - **Every change is one durable write.** A file is replaced whole by a flushed rename under
//!   `<leaf>.outbox.lock`, so no reader meets a torn line and an accepted intent survives a
//!   power loss. The publisher signs an intent only under a [`Claim`], which holds that lock
//!   from its check that the intent is still queued until its marker is written, so a person's
//!   `outbox drop` either drops it before it is signed or is told it was published.
//! - **Bounds.** Queued and held intents together stay within [`MAX_INTENTS`] and
//!   [`MAX_BYTES`]. An owner intent is never dropped: a full outbox refuses the new write
//!   ([`Refused::Full`]). A mine intent (every key ends `@self`) replaces what earlier mine
//!   intents, queued or held, say of the same cells, so a Mac's own row is kept once at its
//!   latest and its statements never crowd out an owner's write.
//!
//! This crate only keeps intents. Publishing them is `aterm_db::file::publish`, which trusts
//! nothing here: it settles each edit against the verified log.

use crate::{
    File, Frontier, Name,
    snapshot::{Column, PK, Row, Snapshot, Source},
};
use aterm_json::{Map, Value};
use std::{collections::BTreeMap, io, path::Path};

/// The most intents a file's outbox and held list keep together.
pub const MAX_INTENTS: usize = 1_024;
/// The most bytes a file's outbox and held list keep together.
pub const MAX_BYTES: u64 = 4 * 1024 * 1024;
/// The most edits one intent holds: one record's writes, since an intent is published as one
/// record under its own id.
pub const MAX_EDITS: usize = 64;
/// The key suffix the publisher fills with the signing Mac's uuid: `<pk>.@self`.
pub const SELF: &str = "@self";
/// The prefix of an `expected` entry that names an intent queued earlier, not an operation.
pub const INTENT: &str = "intent:";
/// The lifecycle column: a row whose cell there says anything but `live` is not live.
pub const LIFECYCLE: &str = "row";
/// The mark that opens a conflicted cell, and what separates its values (the grid's own).
pub const CONFLICT: &str = "⚠ ";
pub const BETWEEN: &str = " ‖ ";
/// The most done markers kept.
pub const DONE_KEPT: usize = 1_024;

/// One cell an intent writes.
#[derive(Clone, Debug, PartialEq)]
pub struct Edit {
    /// The row's key; empty in a row to be born, whose key the publisher derives.
    pub key: String,
    pub column: String,
    /// One element of a set column, whose `value` is then `true` or `false`.
    pub elem: Option<String>,
    /// The typed value; `null` clears the cell.
    pub value: Value,
    /// The cell as the face shows it once written (a set's whole cell), for the overlay.
    pub text: String,
    /// The candidates the author observed: 64-hex operation digests, or `intent:<id>`.
    pub expected: Vec<String>,
}

/// What a writer asks for, before it is given an id.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Draft {
    /// The edits are one new row's cells, born under the key the publisher derives.
    pub born: bool,
    /// The operation to revert, lower-case hex; its edits are empty.
    pub revert: Option<String>,
    pub edits: Vec<Edit>,
}

/// One accepted write.
#[derive(Clone, Debug, PartialEq)]
pub struct Intent {
    /// 32 lower-case hex: the request id its record is published under.
    pub id: String,
    pub file: Name,
    /// When it was made, Unix milliseconds: this Mac's clock, for display only.
    pub ms: u64,
    /// Who says they made it (`person`, a program's word, `harness`): a claim, never an
    /// admission condition.
    pub by: String,
    pub session: Option<String>,
    pub born: bool,
    pub revert: Option<String>,
    pub edits: Vec<Edit>,
    /// The held intent whose author's side this one keeps (`resolve <id> mine`): that held
    /// intent is resolved from the moment this one is written.
    pub resolves: Option<String>,
}

/// A held intent.
#[derive(Clone, Debug, PartialEq)]
pub struct Held {
    pub intent: Intent,
    /// Why it is held, value-free.
    pub why: String,
    pub ms: u64,
}

/// The marker of an intent a record carries.
#[derive(Clone, Debug, PartialEq)]
pub struct Done {
    pub intent: Intent,
    /// The operation that carries it, lower-case hex; `None` where the table already held
    /// every value it wrote.
    pub op: Option<String>,
    pub ms: u64,
    /// A snapshot projected from that operation has been written: the overlay leaves it out.
    pub seen: bool,
    /// The table generation the publisher settled it in; `None` on a line that does not say.
    pub generation: Option<u32>,
    /// The replica the snapshot here was read from when the marker was written, lower-case hex.
    pub source: Option<String>,
    /// Each register it writes, by stored key, and the candidates (operation digests) the
    /// register held once it was settled: what an edit that follows this intent was written
    /// against. Kept once the marker is seen.
    pub left: BTreeMap<String, Vec<String>>,
}

impl Done {
    /// Whether a snapshot projected from `frontier` holds what this marker's intent wrote:
    /// nothing was signed; the frontier names its operation; or the frontier was read from a
    /// later table generation (a roll restates every row as new records, so the operation is
    /// never named again) or from another replica than the one the snapshot here was, which
    /// never holds it.
    #[must_use]
    pub fn projected(&self, frontier: Option<&Frontier>) -> bool {
        let Some(op) = self.op.as_deref() else {
            return true;
        };
        let Some(frontier) = frontier else {
            return false;
        };
        digest(op).is_some_and(|op| frontier.ids.contains(&op))
            || self
                .generation
                .is_some_and(|generation| frontier.generation > generation)
            || self
                .source
                .as_deref()
                .and_then(digest)
                .is_some_and(|source| source != frontier.source)
    }
}

/// What the publisher writes in an intent's marker once a record carries it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mark {
    /// The operation, lower-case hex; `None` where the table already held every value.
    pub op: Option<String>,
    /// The table generation the pass settled it in.
    pub generation: u32,
    /// Each register it writes and the candidates it held once settled ([`Done::left`]).
    pub left: BTreeMap<String, Vec<String>>,
}

/// An accepted intent, and whether its outbox entry is known to survive a power loss.
#[derive(Debug)]
pub struct Accepted {
    pub intent: Intent,
    /// The outbox holds the intent, but the flush of its directory after the rename failed:
    /// the intent is queued and the next pass publishes it, but a power loss before then may
    /// lose it. Why, from the flush.
    pub unflushed: Option<String>,
}

impl std::ops::Deref for Accepted {
    type Target = Intent;

    fn deref(&self) -> &Intent {
        &self.intent
    }
}

/// What a person's drop of an intent did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dropped {
    /// It was queued or held, and is gone.
    Dropped,
    /// A record already carries it: nothing was dropped (a revert undoes its operation).
    Published,
    /// No queued or held intent has that id.
    Absent,
}

/// A file's intents as they are on disk now.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outbox {
    /// Waiting for the publisher, oldest first; an intent a held line also names is left out.
    pub queued: Vec<Intent>,
    pub held: Vec<Held>,
    /// The newest markers, oldest first.
    pub done: Vec<Done>,
    /// A file that could not be read, or lines in one that are no intents of this build: they
    /// stay where they are.
    pub problem: Option<String>,
}

impl Outbox {
    /// The queued intents no done marker names: what the publisher still has to settle.
    pub fn waiting(&self) -> impl Iterator<Item = &Intent> {
        self.queued
            .iter()
            .filter(|intent| !self.done.iter().any(|done| done.intent.id == intent.id))
    }
}

/// Why a write was not accepted. Nothing was queued.
#[derive(Debug)]
pub enum Refused {
    /// The outbox and held list are at a bound; an owner intent is never dropped to make room.
    Full { intents: usize, bytes: u64 },
    /// The draft is not one an intent holds.
    Malformed(&'static str),
    /// A local file could not be read or written.
    Io(io::Error),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full { intents, bytes } => write!(
                f,
                "the outbox is full ({intents} intents, {bytes} bytes; at most {MAX_INTENTS} and \
                 {MAX_BYTES}): nothing was queued, and nothing queued was dropped"
            ),
            Self::Malformed(why) => f.write_str(why),
            Self::Io(error) => write!(f, "the outbox could not be written: {error}"),
        }
    }
}

impl std::error::Error for Refused {}

impl From<io::Error> for Refused {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// The capability an owner write needs (§5, "In Rust"): an explicit verb of `aterm file`, an
/// explicit commit of a face and the importer are what should make one, and harness and
/// observer code should reach only [`File::state_mine`]. Nothing enforces that yet: the
/// constructors are public, so any crate that depends on this one can make an owner act.
/// Restricting who constructs one is SF-07's (authority).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerAct {
    by: String,
    session: Option<String>,
}

fn session_word(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'@'))
}

impl OwnerAct {
    fn new(by: &str, session: Option<&str>) -> Option<Self> {
        (crate::name::part(by) && session.is_none_or(session_word)).then(|| Self {
            by: by.to_owned(),
            session: session.map(str::to_owned),
        })
    }

    /// A person's or a program's explicit `aterm file` verb. `by` is who says they ran it, a
    /// word `[a-z][a-z0-9-]{0,19}`; `session` the aterm session it ran in.
    #[must_use]
    pub fn verb(by: &str, session: Option<&str>) -> Option<Self> {
        Self::new(by, session)
    }

    /// An explicit commit of a face.
    #[must_use]
    pub fn commit(by: &str, session: Option<&str>) -> Option<Self> {
        Self::new(by, session)
    }

    /// The lab importer.
    #[must_use]
    pub fn importer() -> Self {
        Self {
            by: "importer".to_owned(),
            session: None,
        }
    }

    /// The word the intent's `by` says.
    #[must_use]
    pub fn by(&self) -> &str {
        &self.by
    }
}

fn edit_json(edit: &Edit) -> Value {
    let mut object = Map::new();
    object.insert("key".to_owned(), Value::from(edit.key.as_str()));
    object.insert("column".to_owned(), Value::from(edit.column.as_str()));
    if let Some(elem) = &edit.elem {
        object.insert("elem".to_owned(), Value::from(elem.as_str()));
    }
    object.insert("value".to_owned(), edit.value.clone());
    object.insert("text".to_owned(), Value::from(edit.text.as_str()));
    object.insert(
        "expected".to_owned(),
        Value::Array(
            edit.expected
                .iter()
                .map(|digest| Value::from(digest.as_str()))
                .collect(),
        ),
    );
    Value::Object(object)
}

fn edit_of(value: &Value) -> Option<Edit> {
    Some(Edit {
        key: value.get("key")?.as_str()?.to_owned(),
        column: value.get("column")?.as_str()?.to_owned(),
        elem: match value.get("elem") {
            None | Some(Value::Null) => None,
            Some(elem) => Some(elem.as_str()?.to_owned()),
        },
        value: value.get("value").cloned().unwrap_or(Value::Null),
        text: value
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        expected: value
            .get("expected")?
            .as_array()?
            .iter()
            .map(|digest| digest.as_str().map(str::to_owned))
            .collect::<Option<_>>()?,
    })
}

/// The operation `text` names: 64 hex.
#[must_use]
pub fn digest(text: &str) -> Option<[u8; 32]> {
    let mut out = [0; 32];
    (text.len() == 64 && aterm_codec::hex::decode_into(text.as_bytes(), &mut out)).then_some(out)
}

/// Whether `text` is an intent's id: 32 lower-case hex.
#[must_use]
pub fn is_id(text: &str) -> bool {
    aterm_codec::hex::is_lower(text, 32)
}

impl Intent {
    fn object(&self) -> Map {
        let mut object = Map::new();
        object.insert("id".to_owned(), Value::from(self.id.as_str()));
        object.insert("file".to_owned(), Value::from(self.file.to_string()));
        object.insert("ms".to_owned(), Value::from(self.ms));
        object.insert("by".to_owned(), Value::from(self.by.as_str()));
        if let Some(session) = &self.session {
            object.insert("session".to_owned(), Value::from(session.as_str()));
        }
        if self.born {
            object.insert("born".to_owned(), Value::from(true));
        }
        if let Some(revert) = &self.revert {
            object.insert("revert".to_owned(), Value::from(revert.as_str()));
        }
        if let Some(resolves) = &self.resolves {
            object.insert("resolves".to_owned(), Value::from(resolves.as_str()));
        }
        object.insert(
            "edits".to_owned(),
            Value::Array(self.edits.iter().map(edit_json).collect()),
        );
        object
    }

    fn of(value: &Value, file: &Name) -> Option<Self> {
        let id = value.get("id")?.as_str()?;
        if !is_id(id) || value.get("file")?.as_str()? != file.to_string() {
            return None;
        }
        Some(Self {
            id: id.to_owned(),
            file: file.clone(),
            ms: value.get("ms").and_then(Value::as_u64).unwrap_or(0),
            by: value.get("by")?.as_str()?.to_owned(),
            session: value
                .get("session")
                .and_then(Value::as_str)
                .map(str::to_owned),
            born: value.get("born").and_then(Value::as_bool) == Some(true),
            revert: value
                .get("revert")
                .and_then(Value::as_str)
                .map(str::to_owned),
            edits: value
                .get("edits")?
                .as_array()?
                .iter()
                .map(edit_of)
                .collect::<Option<_>>()?,
            resolves: match value.get("resolves") {
                None | Some(Value::Null) => None,
                Some(id) => Some(id.as_str().filter(|id| is_id(id))?.to_owned()),
            },
        })
    }

    /// Every key ends `@self`: a Mac's own row, which the publisher signs at its own uuid.
    #[must_use]
    pub fn mine(&self) -> bool {
        !self.edits.is_empty() && self.edits.iter().all(|edit| edit.key.ends_with(SELF))
    }

    /// The row key the overlay shows an edit under: a row to be born has none yet.
    fn shown_key(&self, edit: &Edit) -> String {
        if edit.key.is_empty() {
            format!("+{}", &self.id[..8])
        } else {
            edit.key.clone()
        }
    }

    /// The stored keys it writes, `<row>.<column>[.<elem>]`, for a value-free listing.
    #[must_use]
    pub fn keys(&self) -> Vec<String> {
        self.edits
            .iter()
            .map(|edit| {
                let mut key = format!("{}.{}", self.shown_key(edit), edit.column);
                if let Some(elem) = &edit.elem {
                    key.push('.');
                    key.push_str(elem);
                }
                key
            })
            .collect()
    }
}

/// One of the three files: the lines this build reads, and the ones it does not, kept as they
/// are.
struct Lines<T> {
    read: Vec<T>,
    raw: Vec<String>,
}

impl<T> Default for Lines<T> {
    fn default() -> Self {
        Self {
            read: Vec::new(),
            raw: Vec::new(),
        }
    }
}

fn lines<T>(path: &Path, parse: impl Fn(&Value) -> Option<T>) -> io::Result<Lines<T>> {
    // A file within its bound is read whole; one past it was not written by this crate.
    let bytes = match aterm_uds::atomic_file::read_regular(path, MAX_BYTES * 2) {
        Ok((bytes, _)) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Lines::default()),
        Err(error) => return Err(error),
    };
    let text = String::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not UTF-8"))?;
    let mut out = Lines::default();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        match aterm_json::from_str::<Value>(line)
            .ok()
            .and_then(|value| parse(&value))
        {
            Some(item) => out.read.push(item),
            None => out.raw.push(line.to_owned()),
        }
    }
    Ok(out)
}

fn held_of(value: &Value, file: &Name) -> Option<Held> {
    Some(Held {
        intent: Intent::of(value, file)?,
        why: value.get("why")?.as_str()?.to_owned(),
        ms: value.get("held_ms").and_then(Value::as_u64).unwrap_or(0),
    })
}

fn digests_of(value: &Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .filter(|op| digest(op).is_some())
                .map(str::to_owned)
        })
        .collect()
}

fn done_of(value: &Value, file: &Name) -> Option<Done> {
    Some(Done {
        intent: Intent::of(value, file)?,
        op: value.get("op").and_then(Value::as_str).map(str::to_owned),
        ms: value.get("done_ms").and_then(Value::as_u64).unwrap_or(0),
        seen: value.get("seen").and_then(Value::as_bool) == Some(true),
        generation: value
            .get("generation")
            .and_then(Value::as_u64)
            .and_then(|generation| u32::try_from(generation).ok()),
        source: value
            .get("source")
            .and_then(Value::as_str)
            .filter(|source| digest(source).is_some())
            .map(str::to_owned),
        left: match value.get("left") {
            None | Some(Value::Null) => BTreeMap::new(),
            Some(left) => left
                .as_object()?
                .iter()
                .map(|(key, ops)| Some((key.clone(), digests_of(ops)?)))
                .collect::<Option<_>>()?,
        },
    })
}

fn held_line(held: &Held) -> String {
    let mut object = held.intent.object();
    object.insert("why".to_owned(), Value::from(held.why.as_str()));
    object.insert("held_ms".to_owned(), Value::from(held.ms));
    Value::Object(object).to_string()
}

fn done_line(done: &Done) -> String {
    let mut object = done.intent.object();
    object.insert("op".to_owned(), Value::from(done.op.as_deref()));
    object.insert("done_ms".to_owned(), Value::from(done.ms));
    if done.seen {
        object.insert("seen".to_owned(), Value::from(true));
    }
    if let Some(generation) = done.generation {
        object.insert("generation".to_owned(), Value::from(u64::from(generation)));
    }
    if let Some(source) = &done.source {
        object.insert("source".to_owned(), Value::from(source.as_str()));
    }
    if !done.left.is_empty() {
        let left: Map = done
            .left
            .iter()
            .map(|(key, ops)| {
                let ops = ops.iter().map(|op| Value::from(op.as_str())).collect();
                (key.clone(), Value::Array(ops))
            })
            .collect();
        object.insert("left".to_owned(), Value::Object(left));
    }
    Value::Object(object).to_string()
}

/// The held intents a later one keeps the author's side of: resolved, whatever line still says
/// they are held.
fn resolved<'a>(queued: &'a [Intent], done: &'a [Done]) -> impl Fn(&str) -> bool + 'a {
    move |id| {
        queued
            .iter()
            .chain(done.iter().map(|done| &done.intent))
            .any(|intent| intent.resolves.as_deref() == Some(id))
    }
}

/// Whether `text` is a value no intent may carry and no read lays over a snapshot: shaped like a
/// credential or holding a phone number (§5, rule 6).
fn refused(text: &str) -> bool {
    aterm_value_guard::credential_shaped(text) || aterm_value_guard::phone_shaped(text)
}

/// Whether any string of `value` is [`refused`].
fn refuses(value: &Value) -> bool {
    match value {
        Value::String(text) => refused(text),
        Value::Array(items) => items.iter().any(refuses),
        Value::Object(object) => object
            .iter()
            .any(|(key, field)| refused(key) || refuses(field)),
        _ => false,
    }
}

/// The text a read shows for an edit: [`aterm_value_guard::REFUSED`] for one the guard refuses.
fn shown_text(edit: &Edit) -> &str {
    if refused(&edit.text) || refuses(&edit.value) {
        aterm_value_guard::REFUSED
    } else {
        &edit.text
    }
}

fn intent_line(intent: &Intent) -> String {
    Value::Object(intent.object()).to_string()
}

fn body(raw: &[String], read: impl Iterator<Item = String>) -> String {
    let mut text = String::new();
    for line in raw.iter().cloned().chain(read) {
        text.push_str(&line);
        text.push('\n');
    }
    text
}

/// The row an edit writes, made where the table has none.
fn row_of<'a>(rows: &'a mut Vec<Row>, pk: &str) -> &'a mut Row {
    let at = match rows.iter().position(|row| row.pk == pk) {
        Some(at) => at,
        None => {
            rows.push(Row {
                pk: pk.to_owned(),
                live: true,
                ..Row::default()
            });
            rows.len() - 1
        }
    };
    &mut rows[at]
}

fn column_of(columns: &mut Vec<Column>, name: &str) {
    if columns.iter().any(|column| column.name == name) {
        return;
    }
    // Before the derived columns and the key, which stay last.
    let at = columns
        .iter()
        .position(|column| matches!(column.source, Source::Derived | Source::Pk))
        .unwrap_or(columns.len());
    columns.insert(
        at,
        Column {
            name: name.to_owned(),
            source: Source::Schema,
            read_only: false,
            refers: None,
        },
    );
}

/// Lay one written intent's edits over `snapshot`: each cell says what the intent wrote, and a
/// value the guard refuses is never laid there ([`aterm_value_guard::REFUSED`] stands in).
fn apply(snapshot: &mut Snapshot, intent: &Intent) {
    for edit in &intent.edits {
        column_of(&mut snapshot.columns, &edit.column);
        let row = row_of(&mut snapshot.rows, &intent.shown_key(edit));
        let text = shown_text(edit);
        if text.is_empty() {
            row.cells.remove(&edit.column);
        } else {
            row.cells.insert(edit.column.clone(), text.to_owned());
        }
        if text != edit.text {
            // The typed value is the snapshot's; a refused one is never offered.
            continue;
        }
        match &edit.elem {
            Some(elem) => {
                let mut members: Vec<Value> = row
                    .values
                    .get(&edit.column)
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                members.retain(|member| member.as_str() != Some(elem.as_str()));
                if edit.value == Value::Bool(true) {
                    members.push(Value::from(elem.as_str()));
                    members.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
                }
                row.values
                    .insert(edit.column.clone(), Value::Array(members));
            }
            None if edit.value.is_null() => {
                row.values.remove(&edit.column);
            }
            None => {
                row.values.insert(edit.column.clone(), edit.value.clone());
            }
        }
        if edit.column == LIFECYCLE {
            row.live = edit.text.is_empty() || edit.text == "live";
        }
    }
}

/// Mark the cells a held intent would have written: `⚠ mine ‖ theirs`, the typed value still
/// theirs. A held intent is never applied.
fn mark(snapshot: &mut Snapshot, held: &Held) {
    for edit in &held.intent.edits {
        column_of(&mut snapshot.columns, &edit.column);
        let row = row_of(&mut snapshot.rows, &held.intent.shown_key(edit));
        let theirs = row.cell(&edit.column).to_owned();
        let mine = shown_text(edit);
        if theirs.starts_with(CONFLICT) && theirs.contains(mine) {
            continue;
        }
        row.cells.insert(
            edit.column.clone(),
            format!("{CONFLICT}{mine}{BETWEEN}{theirs}"),
        );
    }
}

/// The snapshot a read of a file with intents answers: `published` with every waiting intent
/// and every published one its projection does not hold yet laid over it, and every held one
/// marked. With nothing materialized here the table is this Mac's own writes.
pub(crate) fn overlay(
    name: &Name,
    published: Option<Snapshot>,
    outbox: &Outbox,
    frontier: impl FnOnce() -> Option<Frontier>,
) -> Option<Snapshot> {
    let unseen: Vec<&Done> = outbox
        .done
        .iter()
        .filter(|done| !done.seen && done.op.is_some())
        .collect();
    if outbox.queued.is_empty() && outbox.held.is_empty() && unseen.is_empty() {
        return published;
    }
    let mut snapshot = published.unwrap_or_else(|| Snapshot {
        name: name.clone(),
        kind: "table".to_owned(),
        ext: "tsv".to_owned(),
        rev: 0,
        complete: false,
        at_ms: 0,
        columns: vec![Column {
            name: PK.to_owned(),
            source: Source::Pk,
            read_only: true,
            refers: None,
        }],
        handles: Vec::new(),
        rows: Vec::new(),
    });
    if !unseen.is_empty() {
        let frontier = frontier();
        for done in unseen {
            if !done.projected(frontier.as_ref()) {
                apply(&mut snapshot, &done.intent);
            }
        }
    }
    for intent in outbox.waiting() {
        apply(&mut snapshot, intent);
    }
    for held in &outbox.held {
        mark(&mut snapshot, held);
    }
    Some(snapshot)
}

impl File {
    /// This file's intents as they are on disk now. No error: a file that cannot be read is
    /// named in [`Outbox::problem`] and reads as holding nothing.
    #[must_use]
    pub fn outbox(&self) -> Outbox {
        let (root, name) = (self.root(), self.name());
        let mut problems = Vec::new();
        let mut take = |what: &str, lines: io::Result<usize>| match lines {
            Ok(0) => {}
            Ok(raw) => problems.push(format!("{what}: {raw} lines this build does not read")),
            Err(error) => problems.push(format!("{what} not read: {error}")),
        };
        let held = match lines(&root.held(name), |value| held_of(value, name)) {
            Ok(lines) => {
                take("held intents", Ok(lines.raw.len()));
                lines.read
            }
            Err(error) => {
                take("held intents", Err(error));
                Vec::new()
            }
        };
        let done = match lines(&root.done(name), |value| done_of(value, name)) {
            Ok(lines) => {
                take("done markers", Ok(lines.raw.len()));
                lines.read
            }
            Err(error) => {
                take("done markers", Err(error));
                Vec::new()
            }
        };
        let queued = match lines(&root.outbox(name), |value| Intent::of(value, name)) {
            Ok(lines) => {
                take("outbox", Ok(lines.raw.len()));
                lines.read
            }
            Err(error) => {
                take("outbox", Err(error));
                Vec::new()
            }
        };
        // A held intent a later one resolves is no longer held, whatever a crash between the
        // two writes left in its file.
        let held: Vec<Held> = {
            let resolved = resolved(&queued, &done);
            held.into_iter()
                .filter(|held| !resolved(&held.intent.id))
                .collect()
        };
        Outbox {
            // A publisher killed between holding an intent and dequeuing it left it in both.
            queued: queued
                .into_iter()
                .filter(|intent| !held.iter().any(|held| held.intent.id == intent.id))
                .collect(),
            held,
            done,
            problem: (!problems.is_empty()).then(|| problems.join("; ")),
        }
    }
}

#[cfg(any(unix, windows))]
mod write {
    use super::{
        Accepted, DONE_KEPT, Done, Draft, Dropped, Edit, Held, INTENT, Intent, Lines, MAX_BYTES,
        MAX_EDITS, MAX_INTENTS, Mark, OwnerAct, Refused, SELF, body, done_line, done_of, held_line,
        held_of, intent_line, is_id, lines, refused, refuses, resolved,
    };
    use crate::File;
    use std::{
        io,
        path::Path,
        time::{Duration, Instant},
    };

    /// How long a writer waits for another that is changing the same outbox.
    const LOCK_WAIT: Duration = Duration::from_secs(10);

    fn put(path: &Path, text: &str) -> io::Result<()> {
        aterm_uds::atomic_file::write(path, text.as_bytes(), 0o600)
    }

    /// [`put`], telling a write that did not land (the outer `Err`: the file is as it was) from
    /// one that landed and whose directory flush failed (the inner one).
    fn put_landed(path: &Path, text: &str) -> io::Result<io::Result<()>> {
        let landed = aterm_uds::atomic_file::write_landed(path, text.as_bytes(), 0o600)?;
        #[cfg(test)]
        if seam::unflushed() {
            return Ok(Err(io::Error::other(
                "the directory's flush failed (test seam)",
            )));
        }
        Ok(landed)
    }

    fn well_formed(draft: &Draft, mine: bool) -> Result<(), Refused> {
        let digest = |text: &str| aterm_codec::hex::is_lower(text, 64);
        if let Some(revert) = &draft.revert {
            if !digest(revert) || !draft.edits.is_empty() || draft.born {
                return Err(Refused::Malformed(
                    "a revert names one operation and holds no edit",
                ));
            }
            return Ok(());
        }
        if draft.edits.is_empty() || draft.edits.len() > MAX_EDITS {
            return Err(Refused::Malformed(
                "an intent holds between 1 and 64 edits: one record's writes",
            ));
        }
        for edit in &draft.edits {
            if edit.column.is_empty() || draft.born != edit.key.is_empty() {
                return Err(Refused::Malformed(
                    "an edit names a row and a column; only a row to be born has no key yet",
                ));
            }
            if mine != edit.key.ends_with(SELF) {
                return Err(Refused::Malformed(
                    "a Mac's own row is keyed `<pk>.@self`, and an owner's write never is",
                ));
            }
            if !edit
                .expected
                .iter()
                .all(|entry| digest(entry) || entry.strip_prefix(INTENT).is_some_and(is_id))
            {
                return Err(Refused::Malformed(
                    "an expected candidate is an operation digest or `intent:<id>`",
                ));
            }
            if refused(&edit.text)
                || refuses(&edit.value)
                || edit.elem.as_deref().is_some_and(refused)
            {
                return Err(Refused::Malformed(
                    "a value shaped like a credential or a phone number is never written to a \
                     shared file",
                ));
            }
        }
        Ok(())
    }

    /// The intents `queued` and `held` count against the bounds once `queued` is written.
    fn within_bounds(queued: &Lines<Intent>, held: &Lines<Held>) -> Result<String, Refused> {
        let text = body(&queued.raw, queued.read.iter().map(intent_line));
        let intents = queued.read.len() + queued.raw.len() + held.read.len() + held.raw.len();
        let bytes = (text.len() + body(&held.raw, held.read.iter().map(held_line)).len()) as u64;
        if intents > MAX_INTENTS || bytes > MAX_BYTES {
            return Err(Refused::Full {
                intents: intents - 1,
                bytes,
            });
        }
        Ok(text)
    }

    /// The publisher's hold on one queued intent: the outbox lock, taken once the intent was
    /// seen still queued, kept until its marker or its held line is written. A person's drop
    /// waits for it, and then finds the marker.
    #[must_use = "a claim is released when dropped"]
    pub struct Claim {
        file: File,
        id: String,
        _lock: aterm_uds::file_lock::LockFile,
    }

    impl Claim {
        /// Write the claimed intent's marker under the claim, and release it. The intent leaves
        /// the outbox afterwards ([`File::dequeue`]).
        ///
        /// # Errors
        /// A local file could not be read or written.
        pub fn mark_done(self, mark: &Mark, now_ms: u64) -> io::Result<bool> {
            self.file.mark_done_locked(&self.id, mark, now_ms)
        }

        /// Hold the claimed intent under the claim, and release it.
        ///
        /// # Errors
        /// A local file could not be read or written.
        pub fn hold(self, why: &str, now_ms: u64) -> io::Result<bool> {
            self.file.hold_locked(&self.id, why, now_ms)
        }
    }

    impl File {
        fn outbox_lock(&self) -> io::Result<aterm_uds::file_lock::LockFile> {
            self.make_dirs()?;
            aterm_uds::file_lock::LockFile::take(
                &self.root().outbox_lock(self.name()),
                Instant::now() + LOCK_WAIT,
                "another writer is changing this shared file's outbox",
            )
        }

        fn stored(&self) -> io::Result<(Lines<Intent>, Lines<Held>, Lines<Done>)> {
            let (root, name) = (self.root(), self.name());
            Ok((
                lines(&root.outbox(name), |value| Intent::of(value, name))?,
                lines(&root.held(name), |value| held_of(value, name))?,
                lines(&root.done(name), |value| done_of(value, name))?,
            ))
        }

        fn put_outbox(&self, queued: &Lines<Intent>) -> io::Result<()> {
            put(
                &self.root().outbox(self.name()),
                &body(&queued.raw, queued.read.iter().map(intent_line)),
            )
        }

        fn put_held(&self, held: &Lines<Held>) -> io::Result<()> {
            put(
                &self.root().held(self.name()),
                &body(&held.raw, held.read.iter().map(held_line)),
            )
        }

        fn put_done(&self, done: &Lines<Done>) -> io::Result<()> {
            put(
                &self.root().done(self.name()),
                &body(&done.raw, done.read.iter().map(done_line)),
            )
        }

        fn intent(
            &self,
            by: &str,
            session: Option<&str>,
            draft: Draft,
            now_ms: u64,
        ) -> io::Result<Intent> {
            Ok(Intent {
                id: aterm_uds::rand::hex_token::<16>()?,
                file: self.name().clone(),
                ms: now_ms,
                by: by.to_owned(),
                session: session.map(str::to_owned),
                born: draft.born,
                revert: draft.revert,
                edits: draft.edits,
                resolves: None,
            })
        }

        /// Write `queued` with `intent` last, under the outbox lock: the intent is accepted once
        /// the rename lands, and a failed flush of the directory after it is a warning, never a
        /// refusal (the intent is queued and will be published).
        fn put_accepted(
            &self,
            intent: Intent,
            queued: &mut Lines<Intent>,
            held: &Lines<Held>,
        ) -> Result<Accepted, Refused> {
            queued.read.push(intent.clone());
            let text = within_bounds(queued, held)?;
            let landed = put_landed(&self.root().outbox(self.name()), &text)?;
            Ok(Accepted {
                intent,
                unflushed: landed.err().map(|error| error.to_string()),
            })
        }

        #[cfg_attr(
            any(test, debug_assertions),
            aterm_spec_anchor::refines(
                machine = "SharedFile",
                action = "Accept",
                project = "aterm_db::file::model_tests::write_transition"
            )
        )]
        fn accept_intent(
            &self,
            by: &str,
            session: Option<&str>,
            draft: Draft,
            mine: bool,
            now_ms: u64,
        ) -> Result<Accepted, Refused> {
            well_formed(&draft, mine)?;
            let intent = self.intent(by, session, draft, now_ms)?;
            let _lock = self.outbox_lock()?;
            let (mut queued, mut held, _) = self.stored()?;
            let mut held_changed = false;
            if mine {
                // The latest per cell: what an earlier mine intent, queued or held, says of a
                // cell this one writes is superseded, and an intent left saying nothing is gone.
                let writes = |edit: &Edit| {
                    intent.edits.iter().any(|new| {
                        new.key == edit.key && new.column == edit.column && new.elem == edit.elem
                    })
                };
                for earlier in queued.read.iter_mut().filter(|earlier| earlier.mine()) {
                    earlier.edits.retain(|edit| !writes(edit));
                }
                queued
                    .read
                    .retain(|earlier| !earlier.edits.is_empty() || earlier.revert.is_some());
                for earlier in held.read.iter_mut().filter(|held| held.intent.mine()) {
                    let before = earlier.intent.edits.len();
                    earlier.intent.edits.retain(|edit| !writes(edit));
                    held_changed |= earlier.intent.edits.len() != before;
                }
                held.read.retain(|held| !held.intent.edits.is_empty());
            }
            let accepted = self.put_accepted(intent, &mut queued, &held)?;
            if held_changed {
                // After the outbox: a crash between the two leaves a superseded statement held,
                // which the next statement of the same cells removes.
                self.put_held(&held)?;
            }
            Ok(accepted)
        }

        /// Accept an owner's write: one durable write of the outbox, the intent last. It is
        /// unsigned and names no actor; the publisher signs it once this Mac's writer can.
        ///
        /// # Errors
        /// The outbox is full ([`Refused::Full`]: nothing queued, nothing dropped), the draft is
        /// not an owner intent or carries a value the guard refuses, or the outbox could not be
        /// written. A write that landed is accepted, whatever its directory's flush said
        /// ([`Accepted::unflushed`]).
        pub fn write(
            &self,
            act: &OwnerAct,
            draft: Draft,
            now_ms: u64,
        ) -> Result<Accepted, Refused> {
            self.accept_intent(&act.by, act.session.as_deref(), draft, false, now_ms)
        }

        /// Accept this Mac's statement of its own row, every key `<pk>.@self`: the latest per
        /// cell replaces what earlier statements, queued or held, say of it. The publisher fills
        /// the signing Mac's uuid.
        ///
        /// # Errors
        /// As [`Self::write`], or a key does not end `@self`.
        pub fn state_mine(&self, by: &str, draft: Draft, now_ms: u64) -> Result<Accepted, Refused> {
            if !crate::name::part(by) {
                return Err(Refused::Malformed("`by` is one word [a-z][a-z0-9-]{0,19}"));
            }
            self.accept_intent(by, None, draft, true, now_ms)
        }

        /// Claim the queued intent `id` for signing: `None` where it is no longer waiting (a
        /// person dropped it, or another pass marked or held it). Nothing else changes the
        /// outbox until the claim is released.
        ///
        /// # Errors
        /// The outbox lock was not taken in time, or a local file could not be read.
        pub fn claim(&self, id: &str) -> io::Result<Option<Claim>> {
            let lock = self.outbox_lock()?;
            let (queued, held, done) = self.stored()?;
            let waiting = queued.read.iter().any(|intent| intent.id == id)
                && !held.read.iter().any(|held| held.intent.id == id)
                && !done.read.iter().any(|done| done.intent.id == id);
            Ok(waiting.then(|| Claim {
                file: self.clone(),
                id: id.to_owned(),
                _lock: lock,
            }))
        }

        fn mark_done_locked(&self, id: &str, mark: &Mark, now_ms: u64) -> io::Result<bool> {
            let (queued, _, mut done) = self.stored()?;
            let Some(intent) = queued.read.iter().find(|intent| intent.id == id) else {
                return Ok(false);
            };
            if !done.read.iter().any(|done| done.intent.id == id) {
                done.read.push(Done {
                    intent: intent.clone(),
                    op: mark.op.clone(),
                    ms: now_ms,
                    seen: false,
                    generation: Some(mark.generation),
                    source: self
                        .frontier()
                        .map(|frontier| aterm_codec::hex::encode_lower(&frontier.source)),
                    left: mark.left.clone(),
                });
                self.put_done(&done)?;
            }
            Ok(true)
        }

        /// Write the marker of the queued intent `id`, which `mark` says what carries. The
        /// marker is durable before the intent leaves the outbox ([`Self::dequeue`]), so a
        /// publisher killed between the two republishes nothing. `false` where no such intent is
        /// queued.
        ///
        /// # Errors
        /// A local file could not be read or written.
        pub fn mark_done(&self, id: &str, mark: &Mark, now_ms: u64) -> io::Result<bool> {
            let _lock = self.outbox_lock()?;
            self.mark_done_locked(id, mark, now_ms)
        }

        /// Take the intent `id` out of the outbox. Only an intent with a done marker or a held
        /// line leaves this way; `false` where neither names it, and it stays.
        ///
        /// # Errors
        /// A local file could not be read or written.
        pub fn dequeue(&self, id: &str) -> io::Result<bool> {
            let _lock = self.outbox_lock()?;
            let (mut queued, held, done) = self.stored()?;
            let kept = done.read.iter().any(|done| done.intent.id == id)
                || held.read.iter().any(|held| held.intent.id == id);
            if !kept {
                return Ok(false);
            }
            let before = queued.read.len();
            queued.read.retain(|intent| intent.id != id);
            if queued.read.len() != before {
                self.put_outbox(&queued)?;
            }
            Ok(true)
        }

        fn hold_locked(&self, id: &str, why: &str, now_ms: u64) -> io::Result<bool> {
            let (mut queued, mut held, _) = self.stored()?;
            let Some(intent) = queued.read.iter().find(|intent| intent.id == id).cloned() else {
                return Ok(false);
            };
            if !held.read.iter().any(|held| held.intent.id == id) {
                held.read.push(Held {
                    intent,
                    why: why.to_owned(),
                    ms: now_ms,
                });
                self.put_held(&held)?;
            }
            queued.read.retain(|intent| intent.id != id);
            self.put_outbox(&queued)?;
            Ok(true)
        }

        /// Hold the queued intent `id`: durable in the held list first, then out of the outbox.
        /// It is never applied, and leaves only by a person's word ([`Self::drop_intent`],
        /// [`Self::requeue`]). `false` where no such intent is queued.
        ///
        /// # Errors
        /// A local file could not be read or written.
        pub fn hold(&self, id: &str, why: &str, now_ms: u64) -> io::Result<bool> {
            let _lock = self.outbox_lock()?;
            self.hold_locked(id, why, now_ms)
        }

        /// A person's word drops the intent `id`, queued or held. One a record already carries
        /// (its marker is written) is not dropped: [`Dropped::Published`]. A publisher signing
        /// it holds a [`Claim`], which this waits for.
        ///
        /// # Errors
        /// A local file could not be read or written, or the publisher's claim outlasted the
        /// wait for the outbox lock.
        pub fn drop_intent(&self, id: &str) -> io::Result<Dropped> {
            let _lock = self.outbox_lock()?;
            let (mut queued, mut held, done) = self.stored()?;
            if done.read.iter().any(|done| done.intent.id == id) {
                return Ok(Dropped::Published);
            }
            if resolved(&queued.read, &done.read)(id) {
                return Ok(Dropped::Absent);
            }
            let (was_queued, was_held) = (queued.read.len(), held.read.len());
            queued.read.retain(|intent| intent.id != id);
            held.read.retain(|held| held.intent.id != id);
            // The outbox first: an intent dropped from the held list alone would come back
            // queued where a publisher was killed between holding and dequeuing it.
            if queued.read.len() != was_queued {
                self.put_outbox(&queued)?;
            }
            if held.read.len() != was_held {
                self.put_held(&held)?;
            }
            Ok(
                if queued.read.len() != was_queued || held.read.len() != was_held {
                    Dropped::Dropped
                } else {
                    Dropped::Absent
                },
            )
        }

        /// A person keeps their side of the held intent `id`: `draft`, the same write against
        /// the candidates the table shows now, is queued as a new intent that names the held one
        /// (`resolves`), and the held line goes. From the moment the new intent is written the
        /// held one is resolved, whatever a crash before its line goes leaves, so it is resolved
        /// once. `None` where no such intent is held (or it is resolved already).
        ///
        /// # Errors
        /// As [`Self::write`].
        pub fn requeue(
            &self,
            act: &OwnerAct,
            id: &str,
            draft: Draft,
            now_ms: u64,
        ) -> Result<Option<Accepted>, Refused> {
            well_formed(&draft, false)?;
            let mut intent = self.intent(&act.by, act.session.as_deref(), draft, now_ms)?;
            intent.resolves = Some(id.to_owned());
            let _lock = self.outbox_lock()?;
            let (mut queued, mut held, done) = self.stored()?;
            if !held.read.iter().any(|held| held.intent.id == id)
                || resolved(&queued.read, &done.read)(id)
            {
                return Ok(None);
            }
            // Room first: the held one still counts, so the new one is refused by a full
            // outbox before anything changes.
            let accepted = self.put_accepted(intent, &mut queued, &held)?;
            held.read.retain(|held| held.intent.id != id);
            if let Err(error) = self.put_held(&held) {
                // The new intent names the held one, which every read takes as resolved; the
                // next settle of the markers removes the line.
                return Ok(Some(Accepted {
                    unflushed: Some(format!("its held line stays until a later pass: {error}")),
                    ..accepted
                }));
            }
            Ok(Some(accepted))
        }

        /// Note which done markers the snapshot now holds ([`Done::projected`]): the overlay
        /// leaves those out. A marked line keeps who made the intent and what its registers held
        /// and drops its edits; the newest [`DONE_KEPT`] markers stay, and one whose intent is
        /// still queued always does. A held line a later intent resolves goes.
        ///
        /// # Errors
        /// A local file could not be read or written.
        pub fn settle_done(&self) -> io::Result<()> {
            let frontier = self.frontier();
            let (queued, held, done) = self.stored()?;
            let pending = done.read.iter().any(|done| !done.seen);
            let stale = held
                .read
                .iter()
                .any(|line| resolved(&queued.read, &done.read)(&line.intent.id));
            if !pending && !stale && done.read.len() <= DONE_KEPT {
                return Ok(());
            }
            let _lock = self.outbox_lock()?;
            let (queued, mut held, mut done) = self.stored()?;
            let waiting =
                |done: &Done| queued.read.iter().any(|intent| intent.id == done.intent.id);
            let mut changed = false;
            for line in done.read.iter_mut().filter(|line| !line.seen) {
                if line.projected(frontier.as_ref())
                    && !queued.read.iter().any(|intent| intent.id == line.intent.id)
                {
                    line.seen = true;
                    line.intent.edits.clear();
                    changed = true;
                }
            }
            let mut over = done.read.len().saturating_sub(DONE_KEPT);
            if over > 0 {
                let mut kept = Vec::with_capacity(done.read.len());
                for line in done.read.drain(..) {
                    if over > 0 && !waiting(&line) {
                        over -= 1;
                        changed = true;
                    } else {
                        kept.push(line);
                    }
                }
                done.read = kept;
            }
            let before = held.read.len();
            {
                let resolved = resolved(&queued.read, &done.read);
                held.read.retain(|line| !resolved(&line.intent.id));
            }
            if changed {
                self.put_done(&done)?;
            }
            if held.read.len() != before {
                self.put_held(&held)?;
            }
            Ok(())
        }
    }

    /// The seam of this crate's tests: an outbox write whose rename lands and whose directory
    /// flush fails.
    #[cfg(test)]
    #[path = "seam_tests.rs"]
    pub(crate) mod seam;
}

#[cfg(any(unix, windows))]
pub use write::Claim;
#[cfg(all(test, any(unix, windows)))]
pub(crate) use write::seam;
