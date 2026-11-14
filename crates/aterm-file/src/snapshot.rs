// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A shared file's snapshot (docs/DESIGN-shared-files-2026-11-11.md §1): the last verified
//! projection of the replica, a plain local file every program reads and only a pass writes.
//!
//! JSON Lines: one header, one line per row, and a last line holding the SHA-256 of every byte
//! before it, so a reader never takes a cut file for a short table. A row holds each cell as the
//! face shows it (`c`) and, for a column this build types, its value as JSON (`v`); an empty
//! cell is left out of both. No cell holds a secret: a file has no `Secret` column, and a value
//! shaped like a credential is left out by the pass that projects it.

use crate::Name;
use aterm_digest::Sha256;
use aterm_json::{Map, Value};
use std::collections::BTreeMap;

/// The format word of a snapshot.
pub const FORMAT: &str = "aterm-file-snap/1";
/// The most bytes of a snapshot a read takes: four faces' worth, since a row holds both
/// spellings of each cell.
pub const MAX_BYTES: u64 = 16 * 1024 * 1024;
/// The header prefix of a column no one types into.
pub const READ_ONLY: char = '=';
/// The key column's name; its header is `=pk@<rev>`, always last.
pub const PK: &str = "pk";

/// Where a column's cells come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A column of the compiled schema.
    Schema,
    /// A column a newer build wrote: raw, read-only.
    Unknown,
    /// Computed at projection from other cells and other files; never stored as a cell.
    Derived,
    /// The row's key.
    Pk,
}

impl Source {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Schema => "schema",
            Self::Unknown => "unknown",
            Self::Derived => "derived",
            Self::Pk => "pk",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        [Self::Schema, Self::Unknown, Self::Derived, Self::Pk]
            .into_iter()
            .find(|source| source.as_str() == text)
    }
}

/// One column of a table's snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub source: Source,
    /// Its header starts with `=`: the key, a derived or unknown column, or one tools write.
    pub read_only: bool,
    /// The shared file whose rows this column's cells name by key, when one is bound.
    pub refers: Option<String>,
}

impl Column {
    /// The header cell of this column in a face at revision `rev`.
    #[must_use]
    pub fn header(&self, rev: u64) -> String {
        match self.source {
            Source::Pk => format!("{READ_ONLY}{PK}@{rev}"),
            _ if self.read_only => format!("{READ_ONLY}{}", self.name),
            _ => self.name.clone(),
        }
    }
}

/// One row of a table's snapshot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    pub pk: String,
    /// Its lifecycle says live, or says nothing.
    pub live: bool,
    /// Each non-empty cell as the face shows it, by column name.
    pub cells: BTreeMap<String, String>,
    /// Each typed column's value, by column name: a conflict's provisional value, a set's
    /// present members.
    pub values: Map,
}

impl Row {
    /// The cell under `column` as the face shows it; the key under [`PK`].
    #[must_use]
    pub fn cell(&self, column: &str) -> &str {
        if column == PK {
            &self.pk
        } else {
            self.cells.get(column).map_or("", String::as_str)
        }
    }
}

/// One file's snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub name: Name,
    /// The file's kind (`table`).
    pub kind: String,
    /// The face's extension (`tsv`).
    pub ext: String,
    /// Moves by one whenever a pass writes different content.
    pub rev: u64,
    /// No retained record could still become visible when this was projected.
    pub complete: bool,
    /// When it was projected, Unix milliseconds: this Mac's clock, for display only.
    pub at_ms: u64,
    /// The columns in face order, the key last.
    pub columns: Vec<Column>,
    /// The columns a row may be named by besides its key (an account's slug, a machine's
    /// label), in the order a name is tried.
    pub handles: Vec<String>,
    pub rows: Vec<Row>,
}

/// Why bytes are not a snapshot this build reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unreadable(pub &'static str);

impl std::fmt::Display for Unreadable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

fn row_line(row: &Row) -> String {
    let mut object = Map::new();
    object.insert("pk".to_owned(), Value::from(row.pk.as_str()));
    object.insert("live".to_owned(), Value::from(row.live));
    object.insert(
        "c".to_owned(),
        Value::Object(
            row.cells
                .iter()
                .map(|(name, text)| (name.clone(), Value::from(text.as_str())))
                .collect(),
        ),
    );
    object.insert("v".to_owned(), Value::Object(row.values.clone()));
    Value::Object(object).to_string()
}

fn column_json(column: &Column) -> Value {
    let mut object = Map::new();
    object.insert("name".to_owned(), Value::from(column.name.as_str()));
    object.insert("source".to_owned(), Value::from(column.source.as_str()));
    object.insert("ro".to_owned(), Value::from(column.read_only));
    object.insert("ref".to_owned(), Value::from(column.refers.as_deref()));
    Value::Object(object)
}

impl Snapshot {
    /// The digest of what the snapshot says, its revision and time left out: two projections
    /// of one content share it, so a pass that changes nothing keeps the revision.
    #[must_use]
    pub fn content(&self) -> [u8; 32] {
        let mut sha = Sha256::new();
        sha.update(b"aterm/file/snapshot-content/1\0");
        sha.update(self.name.to_string().as_bytes());
        sha.update([0]);
        sha.update(self.kind.as_bytes());
        sha.update([0, u8::from(self.complete)]);
        for column in &self.columns {
            sha.update(column_json(column).to_string().as_bytes());
            sha.update([0]);
        }
        for handle in &self.handles {
            sha.update(handle.as_bytes());
            sha.update([0]);
        }
        for row in &self.rows {
            sha.update(row_line(row).as_bytes());
            sha.update(b"\n");
        }
        sha.finalize()
    }

    /// The snapshot file's bytes.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut header = Map::new();
        header.insert("format".to_owned(), Value::from(FORMAT));
        header.insert("name".to_owned(), Value::from(self.name.to_string()));
        header.insert("kind".to_owned(), Value::from(self.kind.as_str()));
        header.insert("ext".to_owned(), Value::from(self.ext.as_str()));
        header.insert("rev".to_owned(), Value::from(self.rev));
        header.insert("complete".to_owned(), Value::from(self.complete));
        header.insert("at_ms".to_owned(), Value::from(self.at_ms));
        header.insert(
            "columns".to_owned(),
            Value::Array(self.columns.iter().map(column_json).collect()),
        );
        header.insert(
            "handles".to_owned(),
            Value::Array(
                self.handles
                    .iter()
                    .map(|handle| Value::from(handle.as_str()))
                    .collect(),
            ),
        );
        header.insert("rows".to_owned(), Value::from(self.rows.len() as u64));
        let mut text = Value::Object(header).to_string();
        text.push('\n');
        for row in &self.rows {
            text.push_str(&row_line(row));
            text.push('\n');
        }
        let digest = aterm_codec::hex::encode_lower(&Sha256::digest(text.as_bytes()));
        text.push_str(&format!("{{\"end\":\"{digest}\"}}\n"));
        text.into_bytes()
    }

    /// Read a snapshot file's bytes.
    ///
    /// # Errors
    /// They are cut short, changed since they were written, or not this format.
    pub fn decode(bytes: &[u8]) -> Result<Self, Unreadable> {
        let text = std::str::from_utf8(bytes).map_err(|_| Unreadable("not UTF-8"))?;
        let body = text
            .strip_suffix('\n')
            .ok_or(Unreadable("cut short: no final line"))?;
        let (body, last) = body
            .rsplit_once('\n')
            .ok_or(Unreadable("cut short: no rows or end line"))?;
        let end: Value = aterm_json::from_str(last).map_err(|_| Unreadable("no end line"))?;
        let digest = end
            .get("end")
            .and_then(Value::as_str)
            .ok_or(Unreadable("no end line"))?;
        let covered = &text[..body.len() + 1];
        if aterm_codec::hex::encode_lower(&Sha256::digest(covered.as_bytes())) != digest {
            return Err(Unreadable("its bytes are not the ones written"));
        }
        let mut lines = body.split('\n');
        let header: Value = lines
            .next()
            .and_then(|line| aterm_json::from_str(line).ok())
            .ok_or(Unreadable("no header"))?;
        let word = |name: &str| header.get(name).and_then(Value::as_str);
        if word("format") != Some(FORMAT) {
            return Err(Unreadable("another snapshot format"));
        }
        let name = word("name")
            .and_then(|name| Name::parse(name).ok())
            .ok_or(Unreadable("no file name"))?;
        let columns = header
            .get("columns")
            .and_then(Value::as_array)
            .ok_or(Unreadable("no columns"))?
            .iter()
            .map(|column| {
                Some(Column {
                    name: column.get("name")?.as_str()?.to_owned(),
                    source: Source::parse(column.get("source")?.as_str()?)?,
                    read_only: column.get("ro")?.as_bool()?,
                    refers: column.get("ref").and_then(Value::as_str).map(str::to_owned),
                })
            })
            .collect::<Option<Vec<_>>>()
            .ok_or(Unreadable("a malformed column"))?;
        let handles = header
            .get("handles")
            .and_then(Value::as_array)
            .map(|handles| {
                handles
                    .iter()
                    .filter_map(|handle| handle.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let rows = lines
            .map(|line| {
                let row: Value = aterm_json::from_str(line).ok()?;
                Some(Row {
                    pk: row.get("pk")?.as_str()?.to_owned(),
                    live: row.get("live")?.as_bool()?,
                    cells: row
                        .get("c")?
                        .as_object()?
                        .iter()
                        .map(|(name, text)| Some((name.clone(), text.as_str()?.to_owned())))
                        .collect::<Option<_>>()?,
                    values: row.get("v")?.as_object()?.clone(),
                })
            })
            .collect::<Option<Vec<_>>>()
            .ok_or(Unreadable("a malformed row"))?;
        if header.get("rows").and_then(Value::as_u64) != Some(rows.len() as u64) {
            return Err(Unreadable("its row count is not its header's"));
        }
        Ok(Self {
            name,
            kind: word("kind").unwrap_or_default().to_owned(),
            ext: word("ext").unwrap_or_default().to_owned(),
            rev: header.get("rev").and_then(Value::as_u64).unwrap_or(0),
            complete: header.get("complete").and_then(Value::as_bool) == Some(true),
            at_ms: header.get("at_ms").and_then(Value::as_u64).unwrap_or(0),
            columns,
            handles,
            rows,
        })
    }

    /// The column named `name` (the key under [`PK`]).
    #[must_use]
    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|column| column.name == name)
    }

    /// The face of this snapshot: the TSV `aterm db table export` writes, a header row and one
    /// row per record, the last header cell `=pk@<rev>`.
    #[must_use]
    pub fn face(&self) -> String {
        let mut text = String::new();
        for (i, column) in self.columns.iter().enumerate() {
            if i > 0 {
                text.push('\t');
            }
            push_cell(&mut text, &column.header(self.rev));
        }
        text.push('\n');
        for row in &self.rows {
            for (i, column) in self.columns.iter().enumerate() {
                if i > 0 {
                    text.push('\t');
                }
                push_cell(&mut text, row.cell(&column.name));
            }
            text.push('\n');
        }
        text
    }
}

/// One TSV cell as amacs's table codec writes it (`amacs::table::encode`): quoted when it
/// holds a tab, `"`, CR or LF, a `"` inside quotes written `""`.
pub fn push_cell(out: &mut String, value: &str) {
    if !value
        .bytes()
        .any(|byte| matches!(byte, b'\t' | b'"' | b'\r' | b'\n'))
    {
        out.push_str(value);
        return;
    }
    out.push('"');
    for character in value.chars() {
        if character == '"' {
            out.push('"');
        }
        out.push(character);
    }
    out.push('"');
}
