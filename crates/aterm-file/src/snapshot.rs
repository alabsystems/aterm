// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A shared file's snapshot (docs/DESIGN-shared-files-2026-11-11.md §1): the last verified
//! projection of the replica, a plain local file every program reads and only a pass writes.
//!
//! JSON Lines: one header, one line per row, and a last line holding the SHA-256 of every byte
//! before it, so a reader never takes a cut file for a short table. The header states each
//! column, and for a column this build's schema compiles its type and whether it may be empty
//! ([`Column::ty`], [`Column::null`]), so a reader needs no store to know what a cell is. A row
//! holds each cell as the
//! face shows it (`c`) and, for a column this build types, its value as JSON (`v`); an empty
//! cell is left out of both. No cell holds a secret: a file has no `Secret` column, and a value
//! shaped like a credential is left out by the pass that projects it.

use crate::Name;
use aterm_digest::Sha256;
use aterm_json::{Map, Value};
use std::{collections::BTreeMap, io};

/// The format word of a snapshot.
pub const FORMAT: &str = "aterm-file-snap/1";
/// The most bytes of a snapshot, or of the baseline beside it, a read takes, and so a pass
/// writes (`File::record` keeps a projection whose snapshot or baseline passes it as too
/// large). A snapshot row holds each cell twice (its shown text and its value), and a baseline
/// row each column's grid register besides, so 5,000 of the publication corpus' packages wrote
/// a 14.6 MB snapshot and a 27.1 MB baseline (TK-09's `a_5000_row_packages_family_has_a_face`):
/// the baseline binds first, at some 24,000 packages, five times the 5,000 the tracker plans for
/// (docs/DESIGN-lab-tracker-2026-11-14.md §7, finding 16), while a read stays a bounded
/// allocation. The files beside a snapshot that are not its rows (the status, the frontier, the
/// history) keep [`crate::file::SIDECAR_BYTES`].
pub const MAX_BYTES: u64 = 128 * 1024 * 1024;
/// The header prefix of a column no one types into.
pub const READ_ONLY: char = '=';
/// The key column's name; its header is `=pk@<rev>`, always last.
pub const PK: &str = "pk";
/// A table's face's extension.
pub const TABLE_EXT: &str = "tsv";

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
    /// The word a snapshot's header and `aterm file schema` say it with.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
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
#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    pub name: String,
    pub source: Source,
    /// Its header starts with `=`: the key, a derived or unknown column, or one tools write.
    pub read_only: bool,
    /// The shared file whose rows this column's cells name by key, when one is bound.
    pub refers: Option<String>,
    /// The column's type as the schema spells it (`"time"`, `{"text":64}`, `{"ref":"account"}`:
    /// the word a stored schema row holds), for a [`Source::Schema`] column a pass projected.
    /// None for the key, a derived or unknown column, a column only a queued write names, and
    /// every column of a snapshot an earlier build wrote.
    pub ty: Option<Value>,
    /// Whether a row may leave the column empty, where the schema says; None wherever
    /// [`Self::ty`] is, and for a column whose schema does not say.
    pub null: Option<bool>,
}

/// What a column is, apart from what its schema says of it: the members every build has
/// written, which are the ones a snapshot's content covers ([`Snapshot::content`]).
fn column_content(column: &Column) -> Value {
    let mut object = Map::new();
    object.insert("name".to_owned(), Value::from(column.name.as_str()));
    object.insert("source".to_owned(), Value::from(column.source.as_str()));
    object.insert("ro".to_owned(), Value::from(column.read_only));
    object.insert("ref".to_owned(), Value::from(column.refers.as_deref()));
    Value::Object(object)
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
    /// The bytes this row takes in a snapshot file, its line's end included: what a byte budget
    /// on a snapshot's rows counts ([`crate::retain`]).
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        row_line(self).len() + 1
    }

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
    /// Moves by one whenever a pass writes different content ([`Self::content`], which a
    /// column's type and nullability are no part of).
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

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// The rows this thread rendered (a snapshot line, a face line, a row's face widths) or
    /// parsed from a snapshot file, counted (never timed): what a materializer pass touched.
    static TOUCHED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// One row rendered or parsed ([`test_rows_touched`]).
#[inline]
pub(crate) fn touch() {
    #[cfg(any(test, debug_assertions))]
    TOUCHED.with(|touched| touched.set(touched.get() + 1));
}

/// How many rows this thread has rendered (a snapshot line, a face line, a row's face widths)
/// or parsed from a snapshot file: `aterm-db`'s materializer tests read it before and after a
/// pass, so a pass after one record is held to the rows that record changed.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn test_rows_touched() -> usize {
    TOUCHED.with(std::cell::Cell::get)
}

pub(crate) fn row_line(row: &Row) -> String {
    touch();
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
    let Value::Object(mut object) = column_content(column) else {
        return Value::Null;
    };
    // Left out where nothing is stated, so such a column's object is the one every earlier
    // build wrote, and a reader that predates the members reads the rest by key.
    if let Some(ty) = &column.ty {
        object.insert("type".to_owned(), ty.clone());
    }
    if let Some(null) = column.null {
        object.insert("null".to_owned(), Value::from(null));
    }
    Value::Object(object)
}

impl Snapshot {
    /// The digest of what the snapshot says, its revision and time left out: two projections
    /// of one content share it, so a pass that changes nothing keeps the revision.
    ///
    /// A column's type and nullability are left out too. They restate the schema and change no
    /// cell, and a build from before they were stated reads and writes the same table without
    /// them: with them in the digest, that build and this one, passing over one home in turn,
    /// would each see the other's snapshot as different content and move the revision, every
    /// face header and every baseline on each turn. So one revision can name a snapshot with
    /// the two members and one without.
    #[must_use]
    pub fn content(&self) -> [u8; 32] {
        self.content_of(self.rows.iter().map(row_line))
    }

    /// [`Self::content`] from each row's line ([`row_line`]), in row order: a rendering that
    /// keeps its rows' lines ([`crate::Rendered`]) digests them without rendering a row again.
    pub(crate) fn content_of(&self, lines: impl Iterator<Item = impl AsRef<str>>) -> [u8; 32] {
        let mut sha = Sha256::new();
        sha.update(b"aterm/file/snapshot-content/1\0");
        sha.update(self.name.to_string().as_bytes());
        sha.update([0]);
        sha.update(self.kind.as_bytes());
        sha.update([0, u8::from(self.complete)]);
        for column in &self.columns {
            sha.update(column_content(column).to_string().as_bytes());
            sha.update([0]);
        }
        for handle in &self.handles {
            sha.update(handle.as_bytes());
            sha.update([0]);
        }
        for line in lines {
            sha.update(line.as_ref().as_bytes());
            sha.update(b"\n");
        }
        sha.finalize()
    }

    /// The snapshot file's bytes.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let lines: Vec<String> = self.rows.iter().map(row_line).collect();
        self.encode_of(lines.iter().map(String::as_str))
    }

    /// [`Self::encode`] from each row's line ([`row_line`]), in row order.
    pub(crate) fn encode_of<'a>(&self, lines: impl Iterator<Item = &'a str>) -> Vec<u8> {
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
        for line in lines {
            text.push_str(line.as_ref());
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
                    ty: column.get("type").filter(|ty| !ty.is_null()).cloned(),
                    null: column.get("null").and_then(Value::as_bool),
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
                touch();
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
    /// row per record, the last header cell `=pk@<rev>`, of the columns [`face_columns`] keeps:
    /// every column where they fit a face, else all but the widest.
    #[must_use]
    pub fn face(&self) -> String {
        let widths = column_widths(&self.columns, &self.rows);
        let kept = face_columns(&self.columns, &widths, self.rows.len());
        self.face_of(
            &kept,
            self.rows
                .iter()
                .map(|row| face_line(&self.columns, &kept, row)),
        )
    }

    /// Every row and column in display spelling, including columns omitted from the editor.
    ///
    /// # Errors
    /// A sparse snapshot can expand beyond the 128 MiB file limit when written as a rectangle.
    pub fn full_tsv(&self) -> io::Result<String> {
        self.full_tsv_with_stamp("")
    }

    /// A complete editable TSV bound to the baseline and this snapshot's content.
    pub(crate) fn export_tsv(&self, stamp: &str) -> io::Result<String> {
        if self
            .columns
            .len()
            .saturating_mul(self.rows.len().saturating_add(1))
            > crate::MAX_EXPORT_CELLS
        {
            return Err(io::Error::other(
                "the editable TSV exceeds the 4,194,304-cell import limit",
            ));
        }
        self.full_tsv_with_stamp(stamp)
    }

    fn full_tsv_with_stamp(&self, stamp: &str) -> io::Result<String> {
        let too_large = || io::Error::other("the complete TSV exceeds the 128 MiB file limit");
        let separators = self.columns.len().max(1) as u64;
        if separators.saturating_mul(self.rows.len() as u64 + 1) > MAX_BYTES {
            return Err(too_large());
        }
        let all: Vec<_> = (0..self.columns.len()).collect();
        let mut text = self.face_header(&all);
        if text.len() as u64 > MAX_BYTES {
            return Err(too_large());
        }
        // The revision cell ends the header, including when an earlier column name
        // contains a quoted newline. Append its stamp before writing the first record.
        text.insert_str(text.len() - 1, stamp);
        for row in &self.rows {
            let line = face_line(&self.columns, &all, row);
            if (text.len() as u64) + (line.len() as u64) + 1 > MAX_BYTES + stamp.len() as u64 {
                return Err(too_large());
            }
            text.push_str(&line);
            text.push('\n');
        }
        Ok(text)
    }

    fn face_header(&self, kept: &[usize]) -> String {
        let mut text = String::new();
        for (i, at) in kept.iter().enumerate() {
            if i > 0 {
                text.push('\t');
            }
            push_cell(&mut text, &self.columns[*at].header(self.rev));
        }
        text.push('\n');
        text
    }

    /// [`Self::face`] of the columns `kept` from each row's face line ([`face_line`]).
    pub(crate) fn face_of(
        &self,
        kept: &[usize],
        lines: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> String {
        let mut text = self.face_header(kept);
        for line in lines {
            text.push_str(line.as_ref());
            text.push('\n');
        }
        text
    }

    /// The names of the columns the face shows ([`face_columns`]), in face order, the key last.
    #[must_use]
    pub fn face_names(&self) -> Vec<&str> {
        let widths = column_widths(&self.columns, &self.rows);
        face_columns(&self.columns, &widths, self.rows.len())
            .into_iter()
            .map(|at| self.columns[at].name.as_str())
            .collect()
    }
}

/// The bytes one cell takes in a face, quoted as [`push_cell`] quotes it.
fn cell_width(value: &str) -> u64 {
    let quoted = value
        .bytes()
        .any(|byte| matches!(byte, b'\t' | b'"' | b'\r' | b'\n'));
    if !quoted {
        return value.len() as u64;
    }
    value.len() as u64 + 2 + value.bytes().filter(|byte| *byte == b'"').count() as u64
}

/// The bytes each of `columns` takes in `row`'s face line, its tab and line end left out.
pub(crate) fn row_widths(columns: &[Column], row: &Row) -> Vec<u64> {
    touch();
    columns
        .iter()
        .map(|column| cell_width(row.cell(&column.name)))
        .collect()
}

/// The bytes each of `columns` takes over every row of `rows` ([`row_widths`]).
pub(crate) fn column_widths(columns: &[Column], rows: &[Row]) -> Vec<u64> {
    let mut widths = vec![0_u64; columns.len()];
    for row in rows {
        for (total, width) in widths.iter_mut().zip(row_widths(columns, row)) {
            *total += width;
        }
    }
    widths
}

/// The columns a face of `rows` rows shows, by place in `columns`, in their order: every
/// column where the face fits [`crate::MAX_FACE_BYTES`], else the columns left once the widest
/// (by `widths`, the bytes each takes over every row; of two as wide, the later) are left out
/// one by one until it fits. The key is never left out, and where the key alone does not fit,
/// the face is the key alone, which is too large to write ([`crate::File::record`] then writes
/// none). The choice reads only what a snapshot's content holds (its columns and its rows'
/// cells), never its revision, so one content always shows the same columns: each header cell
/// is counted at its widest, the key's at the largest revision.
#[must_use]
pub fn face_columns(columns: &[Column], widths: &[u64], rows: usize) -> Vec<usize> {
    // With at least one column, each column contributes one tab or line end per record.
    // Charge that and the header once so dropping thousands of columns stays linear after
    // sorting. Most faces fit in full and need no sort.
    let records = rows as u128 + 1;
    let cost = |at: usize| {
        u128::from(cell_width(&columns[at].header(u64::MAX))) + u128::from(widths[at]) + records
    };
    let mut count = columns.len();
    let mut bytes: u128 = (0..count).map(cost).sum();
    let fits = |count: usize, bytes: u128| {
        count <= crate::MAX_FACE_COLUMNS && bytes <= u128::from(crate::MAX_FACE_BYTES)
    };
    if fits(count, bytes) {
        return (0..count).collect();
    }
    let mut widest: Vec<_> = (0..count)
        .filter(|at| columns[*at].source != Source::Pk)
        .collect();
    widest.sort_unstable_by_key(|at| (widths[*at], *at));
    let mut kept = vec![true; count];
    for at in widest.into_iter().rev() {
        kept[at] = false;
        count -= 1;
        bytes -= cost(at);
        if fits(count, bytes) {
            break;
        }
    }
    (0..columns.len()).filter(|at| kept[*at]).collect()
}

/// One row's face line over the columns `kept` of `columns`, its line end left out.
pub(crate) fn face_line(columns: &[Column], kept: &[usize], row: &Row) -> String {
    touch();
    let mut text = String::new();
    for (i, at) in kept.iter().enumerate() {
        if i > 0 {
            text.push('\t');
        }
        push_cell(&mut text, row.cell(&columns[*at].name));
    }
    text
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
