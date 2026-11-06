// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The grid round trip (docs/DESIGN-alab-lab-model-2026-11-01.md §8, owner edits): one cells
//! collection laid out as a TSV that amacs opens as a spreadsheet, and the owner's saved file
//! read back as per-cell writes. The TSV is read and written with amacs's own table codec
//! (`amacs::table`), so this reads exactly the cells the grid showed and committed.
//!
//! - **Layout.** One grid column per live schema column in declaration order (so the first
//!   column is the collection's readable name), then any column a newer build wrote that this
//!   build does not know, then the row's key, `=pk`. A header starting with `=` is read-only:
//!   the key, bounded JSON, import coordinates and unknown columns.
//! - **Cells.** A value is shown in its type's plain spelling: a set as its present members, a
//!   list as `a, b`, a record as JSON. An address is masked (`ab…@…`) unless the owner asks
//!   for addresses. A conflicted cell shows `⚠ a ‖ b`; editing it to one value resolves it.
//! - **The baseline.** An export remembers what each cell showed and the operations that were
//!   its candidates ([`Base`]); a save writes a cell only when its text changed, and only while
//!   its candidates are still the exported ones ([`settle`]). A cell that changed meanwhile is
//!   never overwritten: it comes back as `⚠ mine ‖ theirs` for the owner to pick.
//! - **Rows.** A new row with its pk left empty is born under a derived key, every cell in one
//!   record, and never into a row the table already holds ([`settle`] writes a born row all or
//!   nothing); a deleted row is retired (`row` becomes `retired`, or `ended` for a collection
//!   whose rows end), so history stays.
//!
//! Nothing here does I/O; a problem names a row and a column, never a value.

use super::{
    Collection, Column, Kind, LIFECYCLE, Table, Ty,
    cell::{self, CellKey, CellValue, Cells, Field, Projection, Row},
    guard,
};
use crate::{Error, Result, database::Change};
use amacs::table::{Delimiter, Table as Sheet, encode, line_of};
use astream_wire::sync::Digest;
use aterm_codec::hex;
use aterm_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The header prefix of a read-only column.
pub const READ_ONLY: char = '=';
/// The key column's name; its header is `=pk`, always last.
pub const PK: &str = "pk";
/// The mark that opens a conflicted cell.
pub const CONFLICT: &str = "⚠ ";
/// What separates a conflicted cell's values.
pub const BETWEEN: &str = " ‖ ";
/// The format word of a stored baseline.
pub const FORMAT: &str = "att/1-grid";
/// The most bytes a grid file holds: amacs's own limit.
pub const MAX_BYTES: usize = amacs::table::MAX_TABLE_BYTES;

/// An address as every surface shows it unless the owner asks for addresses: the first two
/// characters of its local part, then `…@…`.
pub fn mask(address: &str) -> String {
    let local = address.split('@').next().unwrap_or_default();
    let head: String = local.chars().take(2).collect();
    format!("{head}…@…")
}

/// `value` with every address its type can hold masked, unless `show`.
fn masked(ty: &Ty, value: &Value, show: bool) -> Value {
    if matches!(ty, Ty::Secret) {
        return if value.is_null() {
            Value::Null
        } else {
            Value::String(super::secret::MASK.to_owned())
        };
    }
    if show || !ty.pii() {
        return super::secret::masked(value);
    }
    match (ty, value) {
        (Ty::Email, Value::String(address)) => Value::String(mask(address)),
        (Ty::List(inner, _) | Ty::Set(inner), Value::Array(items)) => {
            Value::Array(items.iter().map(|item| masked(inner, item, show)).collect())
        }
        (Ty::Struct(members), Value::Object(object)) => Value::Object(
            object
                .iter()
                .map(|(key, field)| {
                    let value = members
                        .iter()
                        .find(|m| m.name == key)
                        .map_or_else(|| raw_masked(field, show), |m| masked(&m.ty, field, show));
                    (key.clone(), value)
                })
                .collect(),
        ),
        _ => raw_masked(value, show),
    }
}

/// What every surface shows in place of a string shaped like a credential, naming a secret or
/// holding a phone number, which another writer's build stored though this one refuses it.
pub const REFUSED: &str = "(refused value: not shown)";

/// Whether a stored string is never shown, whatever the reader asks: shaped like a credential,
/// naming a secret, or holding a phone number.
fn refused(text: &str) -> bool {
    guard::withheld(text) || guard::phone_shaped(text)
}

/// A value of no known type: every string shaped like a secret or a phone number replaced with
/// [`REFUSED`], and every string holding `@` masked unless `show`.
pub fn raw_masked(value: &Value, show: bool) -> Value {
    match value {
        Value::String(text) if super::secret::is_sealed(text) => {
            Value::String(super::secret::MASK.to_owned())
        }
        Value::String(text) if refused(text) => Value::String(REFUSED.to_owned()),
        Value::String(text) if !show && text.contains('@') => Value::String(mask(text)),
        Value::Array(items) => Value::Array(items.iter().map(|v| raw_masked(v, show)).collect()),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, field)| (key.clone(), raw_masked(field, show)))
                .collect::<Map>(),
        ),
        other => other.clone(),
    }
}

/// A scalar type shown in its plain spelling.
const fn plain(ty: &Ty) -> bool {
    matches!(
        ty,
        Ty::Bool
            | Ty::Int { .. }
            | Ty::Uint
            | Ty::Text(_)
            | Ty::AccountLabel(_)
            | Ty::Enum(_)
            | Ty::Time
            | Ty::Day
            | Ty::Label
            | Ty::Hex(_)
            | Ty::Digits(_)
            | Ty::Email
            | Ty::Secret
            | Ty::Url
            | Ty::Ref(_)
    )
}

fn compact(value: &Value) -> String {
    aterm_json::to_string(value).unwrap_or_default()
}

/// One value as a cell shows it: `""` for null, a plain scalar as itself, a list of plain
/// scalars as `a, b` (as JSON when an item holds a comma), anything else as JSON. A value that
/// fails its type or the guards (another writer's build stored it) is shown raw
/// ([`raw_masked`]): a secret, a phone number or an unasked address never shows.
pub fn show(ty: &Ty, x: &Value, reveal: bool) -> String {
    let safe = super::secret::mask_type(ty, x);
    let x = &safe;
    if !cell::admissible(ty, x) {
        return match raw_masked(x, reveal) {
            Value::String(text) => text,
            other => compact(&other),
        };
    }
    let x = masked(ty, x, reveal);
    match (&x, ty) {
        (Value::Null, _) => String::new(),
        (Value::String(text), _) if plain(ty) => text.clone(),
        (Value::Bool(_) | Value::Number(_), _) if plain(ty) => compact(&x),
        (Value::Array(items), Ty::List(inner, _)) if plain(inner) => {
            let parts: Vec<String> = items.iter().map(|item| show(inner, item, true)).collect();
            if parts.iter().any(|p| p.contains(',') || p.starts_with('[')) {
                compact(&x)
            } else {
                parts.join(", ")
            }
        }
        _ => compact(&x),
    }
}

/// A projected field as its cell shows it.
pub fn show_field(column: &Column, field: Option<&Field>, reveal: bool) -> String {
    match field {
        None => String::new(),
        Some(Field::Value(candidate)) => show(&column.ty, &candidate.value.x, reveal),
        Some(Field::Conflict { candidates, .. }) => {
            let values: Vec<String> = candidates
                .iter()
                .map(|c| show(&column.ty, &c.value.x, reveal))
                .collect();
            format!("{CONFLICT}{}", values.join(BETWEEN))
        }
        Some(set @ Field::Set(_)) => {
            let inner = match column.ty {
                Ty::Set(inner) => Some(inner),
                _ => None,
            };
            set.members()
                .into_iter()
                .map(|elem| match inner {
                    Some(inner) => show(inner, &Value::String(elem.to_owned()), reveal),
                    None => show_raw_text(elem, reveal),
                })
                .collect::<Vec<_>>()
                .join(", ")
        }
    }
}

fn show_raw_text(text: &str, reveal: bool) -> String {
    match raw_masked(&Value::String(text.to_owned()), reveal) {
        Value::String(masked) => masked,
        other => compact(&other),
    }
}

/// A cell of a column this reader cannot interpret: each candidate's raw value as JSON, masked.
fn show_raw(cell: Option<&crate::database::Cell>, field: Option<&Field>, reveal: bool) -> String {
    let Some(cell) = cell else {
        return match field {
            Some(set @ Field::Set(_)) => set
                .members()
                .into_iter()
                .map(|elem| show_raw_text(elem, reveal))
                .collect::<Vec<_>>()
                .join(", "),
            _ => String::new(),
        };
    };
    let values: Vec<String> = super::cell::origins(cell)
        .into_iter()
        .map(|(_, value)| {
            let x = CellValue::decode(&value).map_or(value, |v| v.x);
            compact(&raw_masked(&x, reveal))
        })
        .collect();
    match values.len() {
        0 => String::new(),
        1 => values.into_iter().next().unwrap_or_default(),
        _ => format!("{CONFLICT}{}", values.join(BETWEEN)),
    }
}

/// Whether the grid shows `column` read-only: bounded JSON and import coordinates are written
/// by tools, never typed (design §3.3), and a column only its row's own Mac writes
/// (`Writer::Mine`, a roll's cover row) is that Mac's statement, never the owner's edit.
pub const fn read_only(column: &Column) -> bool {
    if matches!(column.writer, super::Writer::Mine) {
        return true;
    }
    match column.ty {
        Ty::Json(_) | Ty::SrcRef | Ty::Sealed(_) | Ty::Secret => true,
        Ty::List(inner, _) => matches!(inner, Ty::SrcRef),
        _ => false,
    }
}

/// Parse a cell's text as a value of `ty`: empty is null, a plain scalar in its own spelling,
/// a list of plain scalars as `a, b` or JSON, anything else as JSON. The type and guard checks
/// come after, in [`cell::change`].
///
/// # Errors
/// The text is not a value of the type: the rule, never the text.
pub fn read(ty: &Ty, text: &str) -> std::result::Result<Value, &'static str> {
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    let json = |text: &str| {
        aterm_json::from_str::<Value>(text.trim()).map_err(|_| "this cell holds JSON of its type")
    };
    match ty {
        Ty::Text(_) => Ok(Value::String(text.to_owned())),
        Ty::AccountLabel(_) | Ty::Enum(_) | Ty::Url | Ty::Ref(_) => {
            Ok(Value::String(text.trim().to_owned()))
        }
        Ty::Hex(_) => Ok(Value::String(text.trim().to_ascii_lowercase())),
        Ty::Digits(_) => Ok(Value::String(text.trim().to_owned())),
        Ty::Bool => match text.trim() {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => Err("this column holds true or false"),
        },
        Ty::Int { .. } => text
            .trim()
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| "this column holds an integer"),
        Ty::Uint | Ty::Time | Ty::Day => text
            .trim()
            .parse::<u64>()
            .map(Value::from)
            .map_err(|_| "this column holds a whole number"),
        Ty::Label => crate::machines::Label::parse(text.trim())
            .map(|label| Value::String(label.canonical()))
            .map_err(|_| "a machine label is m<N> or m<N>-xxxx"),
        Ty::Email => super::keys::address(text)
            .map(Value::String)
            .map_err(|_| "this cell holds one whole address"),
        Ty::List(inner, _) if plain(inner) && !text.trim_start().starts_with('[') => text
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(|part| read(inner, part))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map(Value::Array),
        _ => json(text),
    }
}

/// The members a set cell's text names.
fn read_set(text: &str) -> BTreeSet<String> {
    text.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect()
}

/// What one grid column shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A compiled schema column.
    Schema(&'static Column),
    /// A column a newer build wrote: raw, read-only.
    Unknown,
    /// The row's key.
    Pk,
}

/// One grid column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GridColumn {
    pub name: String,
    pub source: Source,
    pub read_only: bool,
}

impl GridColumn {
    /// The header cell's text.
    pub fn header(&self) -> String {
        if self.read_only {
            format!("{READ_ONLY}{}", self.name)
        } else {
            self.name.clone()
        }
    }
}

/// The grid columns of `collection` over `projection`: its live columns, then unknown ones a
/// newer build wrote, then `=pk`.
pub fn layout(collection: &'static Collection, projection: &Projection) -> Vec<GridColumn> {
    layout_with(collection, projection, false)
}

/// The grid column of schema column `column`, its `Secret` editable where `secrets` (the column
/// is revealed).
fn schema_column(column: &'static Column, secrets: bool) -> GridColumn {
    GridColumn {
        name: column.name.to_owned(),
        source: Source::Schema(column),
        read_only: read_only(column) && !(secrets && matches!(column.ty, Ty::Secret)),
    }
}

/// The header of each live `Secret` column of `collection` as its grid writes it, revealed where
/// `secrets` ([`layout_with`]): the columns a grid's opener names to the window as concealed
/// (`edit … secret=`), whose cells no reader of the window but the person's eyes gets
/// (docs/DESIGN-alab-one-mesh-2026-11-05.md §4.8).
#[must_use]
pub fn secret_headers(collection: &'static Collection, secrets: bool) -> Vec<String> {
    collection
        .columns
        .iter()
        .filter(|c| c.live_at(collection.version) && matches!(c.ty, Ty::Secret))
        .map(|c| schema_column(c, secrets).header())
        .collect()
}

/// [`layout`], with each `Secret` column editable where `secrets` (the column is revealed).
fn layout_with(
    collection: &'static Collection,
    projection: &Projection,
    secrets: bool,
) -> Vec<GridColumn> {
    let mut columns: Vec<GridColumn> = collection
        .columns
        .iter()
        .filter(|c| c.live_at(collection.version))
        .map(|c| schema_column(c, secrets))
        .collect();
    let unknown: BTreeSet<&str> = projection
        .rows
        .values()
        .flat_map(|row| row.fields.keys().chain(row.raw.iter()))
        .map(String::as_str)
        .filter(|name| collection.column(name).is_none())
        .collect();
    columns.extend(unknown.into_iter().map(|name| GridColumn {
        name: name.to_owned(),
        source: Source::Unknown,
        read_only: true,
    }));
    columns.push(GridColumn {
        name: PK.to_owned(),
        source: Source::Pk,
        read_only: true,
    });
    columns
}

/// What one exported cell showed and held.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Seen {
    /// The cell's text in the grid.
    pub shown: String,
    /// The operations that were the register's candidates (a scalar or list column).
    pub candidates: Vec<Digest>,
    /// Each set element's candidates (a set column).
    pub elements: BTreeMap<String, Vec<Digest>>,
    /// The cell holds a value this build cannot read: read-only here.
    pub raw: bool,
    /// A cell of a revealed `Secret` column ([`export_opened`]): `shown` is plaintext, which
    /// `Debug` and a stored baseline never show.
    pub secret: bool,
}

impl std::fmt::Debug for Seen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let shown: &str = if self.secret && !self.shown.is_empty() {
            super::secret::MASK
        } else {
            &self.shown
        };
        f.debug_struct("Seen")
            .field("shown", &shown)
            .field("candidates", &self.candidates.len())
            .field("elements", &self.elements.len())
            .field("raw", &self.raw)
            .field("secret", &self.secret)
            .finish()
    }
}

/// The opened values of a revealed `Secret` column, by row key and column: each candidate's
/// value, through the audited reveal (`att::open_secrets`), a cleared candidate as an empty
/// one. A cell absent here (a protected row's, one whose token does not open) shows masked and
/// read-only.
pub type Opened = BTreeMap<(String, String), Vec<super::secret::Secret>>;

/// How a revealed `Secret` cell's opened candidates show: its one value, or `⚠ a ‖ b` sorted and
/// without repeats, a cleared candidate as an empty one.
pub fn show_opened(values: &[super::secret::Secret]) -> String {
    let mut texts: Vec<&str> = values
        .iter()
        .map(super::secret::Secret::plaintext)
        .collect();
    texts.sort_unstable();
    texts.dedup();
    match texts.as_slice() {
        [one] => (*one).to_owned(),
        many => format!("{CONFLICT}{}", many.join(BETWEEN)),
    }
}

/// What an export showed: the baseline a save is compared against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Base {
    /// `<application>/<role>`.
    pub table: String,
    pub collection: String,
    /// The compiled collection version the export was laid out at.
    pub version: u32,
    /// Addresses were shown unmasked.
    pub reveal: bool,
    /// The header row, exactly.
    pub header: Vec<String>,
    /// Each exported row's cells by column name, in export order.
    pub rows: Vec<(String, BTreeMap<String, Seen>)>,
}

/// A filter of `--where col=val`: a row matches when the column's value (any candidate of a
/// conflict, any member of a set) reads as `val`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Where {
    pub column: String,
    pub text: String,
}

impl Where {
    /// Parse `col=val`.
    ///
    /// # Errors
    /// No `=`.
    pub fn parse(spec: &str) -> Result<Self> {
        let (column, text) = spec
            .split_once('=')
            .ok_or_else(|| Error::new("a filter is col=val"))?;
        let column = column.trim();
        if column.is_empty() {
            return Err(Error::new("a filter is col=val"));
        }
        Ok(Self {
            column: column.to_owned(),
            text: text.to_owned(),
        })
    }

    /// Whether `row` of `collection` matches, `pk` being its key.
    ///
    /// # Errors
    /// The column is unknown, or `val` is not a value of its type.
    pub fn matches(&self, collection: &Collection, pk: &str, row: &Row) -> Result<bool> {
        if self.column == PK {
            return Ok(pk == self.text.trim());
        }
        let column = collection.column(&self.column).ok_or_else(|| {
            Error::new(&format!("{}: no such column to filter by", collection.name))
        })?;
        if let Ty::Set(inner) = column.ty {
            if self.text.trim().is_empty() {
                return Ok(row
                    .fields
                    .get(column.name)
                    .is_none_or(|set| set.members().is_empty()));
            }
            let wanted = read(inner, &self.text).map_err(|rule| {
                Error::new(&format!("{}.{}: {rule}", collection.name, column.name))
            })?;
            let Some(elem) = wanted.as_str() else {
                return Ok(false);
            };
            return Ok(row
                .fields
                .get(column.name)
                .is_some_and(|set| set.members().contains(elem)));
        }
        let wanted = read(&column.ty, &self.text)
            .map_err(|rule| Error::new(&format!("{}.{}: {rule}", collection.name, column.name)))?;
        Ok(match row.fields.get(column.name) {
            None => wanted.is_null(),
            Some(Field::Value(candidate)) => candidate.value.x == wanted,
            Some(Field::Conflict { candidates, .. }) => {
                candidates.iter().any(|c| c.value.x == wanted)
            }
            Some(Field::Set(_)) => false,
        })
    }
}

/// The raw candidates of each `(pk, col)` register and set element of `collection`.
type Registers = BTreeMap<(String, String), (Vec<Digest>, BTreeMap<String, Vec<Digest>>)>;

fn registers(collection: &Collection, cells: &Cells) -> Registers {
    let mut out = Registers::new();
    for ((_, key), cell) in cells
        .range((collection.name.to_owned(), String::new())..)
        .take_while(|((name, _), _)| name == collection.name)
    {
        let Ok(parsed) = CellKey::parse(key) else {
            continue;
        };
        let mut digests: Vec<Digest> = cell.candidates.iter().map(|c| c.operation).collect();
        digests.sort_unstable();
        let entry = out
            .entry((parsed.pk.to_owned(), parsed.col.to_owned()))
            .or_default();
        match parsed.elem {
            Some(elem) => {
                entry.1.insert(elem.to_owned(), digests);
            }
            None => entry.0 = digests,
        }
    }
    out
}

/// An export: the TSV the owner edits and the baseline a save is compared against. Its
/// `Debug` gives the text's length only, since a revealed grid's holds plaintext.
#[derive(Clone, PartialEq, Eq)]
pub struct Export {
    pub text: String,
    pub base: Base,
}

impl std::fmt::Debug for Export {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Export")
            .field("text", &self.text.len())
            .field("base", &self.base)
            .finish()
    }
}

/// Lay out the rows of `collection` that every filter matches as a TSV, addresses masked unless
/// `reveal`, with the baseline a save is read against.
///
/// # Errors
/// The collection is not cells, or a filter names an unknown column or an unreadable value.
pub fn export(
    table: &Table,
    collection: &'static Collection,
    cells: &Cells,
    filters: &[Where],
    reveal: bool,
) -> Result<Export> {
    export_opened(table, collection, cells, filters, reveal, None)
}

/// [`export`], with each `Secret` column revealed and editable where `opened` holds its
/// cells' values (the owner's `--edit`, design §4.8): a cell shows its value, or its distinct
/// values as a conflict; a set cell `opened` lacks shows masked and read-only, as does every
/// `Secret` cell of a protected row. Never stored: such a baseline's [`Base::to_json`] masks it.
///
/// # Errors
/// As [`export`].
pub fn export_opened(
    table: &Table,
    collection: &'static Collection,
    cells: &Cells,
    filters: &[Where],
    reveal: bool,
    opened: Option<&Opened>,
) -> Result<Export> {
    if collection.kind != Kind::Cells {
        return Err(Error::new(&format!(
            "{}: only a cells collection is edited in the grid",
            collection.name
        )));
    }
    let projection = cell::project(collection, cells);
    let mut registers = registers(collection, cells);
    let columns = layout_with(collection, &projection, opened.is_some());
    let header: Vec<String> = columns.iter().map(GridColumn::header).collect();
    let mut rows = Vec::new();
    'rows: for (pk, row) in &projection.rows {
        for filter in filters {
            if !filter.matches(collection, pk, row)? {
                continue 'rows;
            }
        }
        let mut seen = BTreeMap::new();
        for column in &columns {
            let (candidates, elements) = registers
                .remove(&(pk.clone(), column.name.clone()))
                .unwrap_or_default();
            let mut raw = row.raw.contains(&column.name);
            let secret = opened.is_some()
                && matches!(column.source, Source::Schema(schema) if matches!(schema.ty, Ty::Secret));
            let shown = match column.source {
                Source::Schema(schema) if secret && !raw => {
                    let field = row.fields.get(&column.name);
                    match opened.and_then(|o| o.get(&(pk.clone(), column.name.clone()))) {
                        Some(values) if !super::check::protected(row) => show_opened(values),
                        _ => {
                            // Unopened, or a protected row's: masked, and never typed over.
                            raw = super::check::protected(row)
                                || field.is_some_and(
                                    |f| !matches!(f, Field::Value(c) if c.value.x.is_null()),
                                );
                            show_field(schema, field, reveal)
                        }
                    }
                }
                Source::Pk => pk.clone(),
                Source::Unknown => show_raw(
                    cells.get(&(collection.name.to_owned(), format!("{pk}.{}", column.name))),
                    row.fields.get(&column.name),
                    reveal,
                ),
                Source::Schema(_) if raw => show_raw(
                    cells.get(&(collection.name.to_owned(), format!("{pk}.{}", column.name))),
                    row.fields.get(&column.name),
                    reveal,
                ),
                Source::Schema(schema) => show_field(schema, row.fields.get(&column.name), reveal),
            };
            seen.insert(
                column.name.clone(),
                Seen {
                    shown,
                    candidates,
                    elements,
                    raw,
                    secret,
                },
            );
        }
        rows.push((pk.clone(), seen));
    }
    let base = Base {
        table: format!("{}/{}", table.application, table.role),
        collection: collection.name.to_owned(),
        version: collection.version,
        reveal,
        header,
        rows,
    };
    Ok(Export {
        text: render(&base),
        base,
    })
}

/// The TSV of a baseline: its header, then each row's shown cells, amacs's quoting.
pub fn render(base: &Base) -> String {
    let names: Vec<&str> = base
        .header
        .iter()
        .map(|h| h.trim_start_matches(READ_ONLY))
        .collect();
    let mut text = String::new();
    for (i, h) in base.header.iter().enumerate() {
        if i > 0 {
            text.push('\t');
        }
        text.push_str(&encode(h, Delimiter::Tab, false));
    }
    text.push('\n');
    for (_, seen) in &base.rows {
        for (i, &name) in names.iter().enumerate() {
            if i > 0 {
                text.push('\t');
            }
            let cell = seen.get(name).map_or("", |s| s.shown.as_str());
            text.push_str(&encode(cell, Delimiter::Tab, false));
        }
        text.push('\n');
    }
    text
}

/// A cell the owner's save cannot take: where, and the rule, never the value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    /// The record's index in the file, the header being 0.
    pub record: usize,
    /// The grid column's index.
    pub column: usize,
    /// The 1-based text line the record starts on, for reopening the file there.
    pub line: usize,
    pub rule: String,
}

/// One register the save changes.
#[derive(Clone, PartialEq)]
pub struct Edit {
    pub pk: String,
    pub column: String,
    pub elem: Option<String>,
    /// The stored key.
    pub key: String,
    /// The typed value written (`x`).
    pub x: Value,
    /// The record value written.
    pub value: Value,
    /// The candidates the export showed: the write goes ahead only while these are still the
    /// register's candidates.
    pub expected: Vec<Digest>,
    /// What the owner's cell said, for showing it back beside a value that changed meanwhile.
    pub mine: String,
    /// The grid column's index.
    pub at: usize,
    /// A revealed `Secret` cell's edit: `x` is the owner's plaintext (or null) until [`seal`]
    /// seals it in the writing session, and `value` is written there. `Debug` never shows it.
    pub secret: bool,
}

impl std::fmt::Debug for Edit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hidden = |shown: &dyn std::fmt::Debug| -> String {
            if self.secret {
                super::secret::MASK.to_owned()
            } else {
                format!("{shown:?}")
            }
        };
        f.debug_struct("Edit")
            .field("pk", &self.pk)
            .field("column", &self.column)
            .field("elem", &self.elem)
            .field("key", &self.key)
            .field("x", &hidden(&self.x))
            .field("value", &hidden(&self.value))
            .field("expected", &self.expected.len())
            .field("mine", &hidden(&self.mine))
            .field("at", &self.at)
            .field("secret", &self.secret)
            .finish()
    }
}

/// What an owner's save asks for, read against its baseline.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    /// Changed registers, grouped by row in file order: a row's changes go in one record.
    pub rows: Vec<(String, Vec<Edit>)>,
    /// Keys of rows born from new grid rows.
    pub born: Vec<String>,
    /// Keys of rows the owner deleted, retired or ended.
    pub retired: Vec<String>,
}

impl Plan {
    /// Whether the save asks for nothing.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// How a table names the rows a collection's new grid rows become: their derived key from the
/// typed values of their cells (by column name), or the rule that stops it.
pub type Birth<'a> = &'a dyn Fn(&Collection, &BTreeMap<String, Value>) -> Result<String>;

struct Reader<'a> {
    collection: &'static Collection,
    text: &'a str,
    sheet: &'a Sheet,
    problems: Vec<Problem>,
}

impl Reader<'_> {
    fn cell(&self, record: usize, column: usize) -> String {
        self.sheet
            .cell(record, column)
            .map(|cell| cell.value(self.text).into_owned())
            .unwrap_or_default()
    }

    fn problem(&mut self, record: usize, column: usize, rule: impl Into<String>) {
        let line = self
            .sheet
            .records
            .get(record)
            .map_or(1, |r| line_of(self.text, r.raw.start));
        self.problems.push(Problem {
            record,
            column,
            line,
            rule: rule.into(),
        });
    }

    /// The edits one changed cell of an existing or new row asks for.
    #[expect(
        clippy::too_many_arguments,
        reason = "one cell's coordinates, column, texts and baseline"
    )]
    fn edits(
        &mut self,
        record: usize,
        at: usize,
        pk: &str,
        column: &'static Column,
        text: &str,
        seen: Option<&Seen>,
        out: &mut Vec<Edit>,
    ) {
        let collection = self.collection;
        let key = |elem: Option<&str>| {
            CellKey {
                pk,
                col: column.name,
                elem,
            }
            .encode()
        };
        if let Ty::Set(_) = column.ty {
            if text.starts_with(CONFLICT) {
                self.problem(record, at, "keep one set: delete the ⚠ and the other value");
                return;
            }
            // A reopened stale set shows `⚠ mine ‖ theirs`: the set as it is now is theirs.
            let before: BTreeSet<String> = seen
                .map(|s| {
                    read_set(match s.shown.strip_prefix(CONFLICT) {
                        Some(both) => both.rsplit(BETWEEN).next().unwrap_or_default(),
                        None => &s.shown,
                    })
                })
                .unwrap_or_default();
            let after = read_set(text);
            for (elem, present) in after
                .difference(&before)
                .map(|e| (e, true))
                .chain(before.difference(&after).map(|e| (e, false)))
            {
                let value = CellValue::owner(collection.version, Value::Bool(present));
                let change = cell::change(
                    collection,
                    CellKey {
                        pk,
                        col: column.name,
                        elem: Some(elem),
                    },
                    &value,
                );
                match (change, key(Some(elem))) {
                    (Ok(change), Ok(key)) => out.push(Edit {
                        pk: pk.to_owned(),
                        column: column.name.to_owned(),
                        elem: Some(elem.clone()),
                        key,
                        x: Value::Bool(present),
                        value: change.value.unwrap_or_default(),
                        expected: seen
                            .and_then(|s| s.elements.get(elem))
                            .cloned()
                            .unwrap_or_default(),
                        mine: text.to_owned(),
                        at,
                        secret: false,
                    }),
                    (Err(error), _) | (_, Err(error)) => {
                        self.problem(record, at, error.to_string());
                    }
                }
            }
            return;
        }
        if text.starts_with(CONFLICT) {
            self.problem(
                record,
                at,
                "a conflicted cell takes one value: delete the ⚠ and keep one",
            );
            return;
        }
        if matches!(column.ty, Ty::Secret) {
            // The owner's plaintext, sealed only in the writing session ([`seal`]).
            let x = if text.is_empty() {
                Value::Null
            } else if super::secret::flag_like(&super::secret::Secret::new(text.to_owned())) {
                self.problem(
                    record,
                    at,
                    format!(
                        "{}.{}: a value shaped like a flag (`1`, `yes`, under six characters) \
                         is not a secret",
                        collection.name, column.name
                    ),
                );
                return;
            } else {
                Value::String(text.to_owned())
            };
            match key(None) {
                Ok(key) => out.push(Edit {
                    pk: pk.to_owned(),
                    column: column.name.to_owned(),
                    elem: None,
                    key,
                    x,
                    value: Value::Null,
                    expected: seen.map(|s| s.candidates.clone()).unwrap_or_default(),
                    mine: text.to_owned(),
                    at,
                    secret: true,
                }),
                Err(error) => self.problem(record, at, error.to_string()),
            }
            return;
        }
        let x = match read(&column.ty, text) {
            Ok(x) => x,
            Err(rule) => {
                self.problem(
                    record,
                    at,
                    format!("{}.{}: {rule}", collection.name, column.name),
                );
                return;
            }
        };
        let value = CellValue::owner(collection.version, x.clone());
        let checked = cell::change(
            collection,
            CellKey {
                pk,
                col: column.name,
                elem: None,
            },
            &value,
        );
        match checked {
            Ok(change) => out.push(Edit {
                pk: pk.to_owned(),
                column: column.name.to_owned(),
                elem: None,
                key: change.key,
                x,
                value: change.value.unwrap_or_default(),
                expected: seen.map(|s| s.candidates.clone()).unwrap_or_default(),
                mine: text.to_owned(),
                at,
                secret: false,
            }),
            Err(error) => self.problem(record, at, error.to_string()),
        }
    }
}

/// Read the owner's saved `text` against the export's `base`: the cells whose text changed,
/// type-checked, as edits that carry the candidates the export showed; new rows (their pk left
/// empty) born under the key `birth` derives; deleted rows retired, or ended when the table says
/// the collection's rows end. Unchanged cells ask for nothing.
///
/// # Errors
/// Every problem found, each at its row and column with its rule and never a value: a changed
/// header, a typed pk, a changed read-only cell, a value that is not of its column's type or
/// that a guard refuses, a required column left empty in a new row, a repeated row.
pub fn plan(
    table: &Table,
    collection: &'static Collection,
    base: &Base,
    text: &str,
    birth: Birth<'_>,
) -> std::result::Result<Plan, Vec<Problem>> {
    let whole = |rule: &str| {
        vec![Problem {
            record: 0,
            column: 0,
            line: 1,
            rule: rule.to_owned(),
        }]
    };
    if base.collection != collection.name || base.version != collection.version {
        return Err(whole(
            "this file was exported from another collection or version",
        ));
    }
    let pk_header = format!("{READ_ONLY}{PK}");
    if base.header.last() != Some(&pk_header) {
        return Err(whole("not a grid baseline"));
    }
    let Ok(sheet) = Sheet::scan(text, Delimiter::Tab) else {
        return Err(whole("the file is larger than the grid reads"));
    };
    let mut reader = Reader {
        collection,
        text,
        sheet: &sheet,
        problems: Vec::new(),
    };
    let width = base.header.len();
    for (at, expected) in base.header.iter().enumerate() {
        if reader.cell(0, at) != *expected {
            reader.problem(
                0,
                at,
                "the header row names the columns; put it back as it was",
            );
        }
    }
    if sheet.records.first().is_some_and(|h| h.cells.len() > width) {
        reader.problem(
            0,
            width,
            "the header row names the columns; put it back as it was",
        );
    }
    if !reader.problems.is_empty() {
        return Err(reader.problems);
    }
    let columns: Vec<(String, Option<&'static Column>, bool)> = base
        .header
        .iter()
        .map(|h| {
            let read_only = h.starts_with(READ_ONLY);
            let name = h.trim_start_matches(READ_ONLY).to_owned();
            let schema = collection
                .column(&name)
                .filter(|c| c.live_at(collection.version));
            (name, schema, read_only)
        })
        .collect();
    let exported: BTreeMap<&str, &BTreeMap<String, Seen>> = base
        .rows
        .iter()
        .map(|(pk, seen)| (pk.as_str(), seen))
        .collect();
    let pk_at = width - 1;
    let mut plan = Plan::default();
    let mut present = BTreeSet::new();
    let mut born = BTreeSet::new();
    for (record, row) in sheet.records.iter().enumerate().skip(1) {
        let texts: Vec<String> = (0..row.cells.len().max(width))
            .map(|at| reader.cell(record, at))
            .collect();
        if texts.iter().all(|t| t.trim().is_empty()) {
            continue;
        }
        if let Some(extra) = texts.iter().skip(width).position(|t| !t.trim().is_empty()) {
            reader.problem(record, width + extra, "this cell is past the last column");
            continue;
        }
        let pk = texts[pk_at].trim().to_owned();
        let mut edits = Vec::new();
        if pk.is_empty() {
            // A new row: every typed cell, the lifecycle `live`, one record.
            let mut values = BTreeMap::new();
            let mut cells = Vec::new();
            for (at, (name, schema, read_only)) in columns.iter().enumerate().take(pk_at) {
                let text = &texts[at];
                if *read_only {
                    if !text.trim().is_empty() {
                        reader.problem(record, at, "this column is read-only (=)");
                    }
                    continue;
                }
                let Some(schema) = schema else { continue };
                match (schema.ty, text.trim().is_empty()) {
                    (Ty::Set(_), _) => cells.push((at, *schema, text.clone())),
                    (_, true) if name == LIFECYCLE => {}
                    (_, true) if !schema.null => {
                        reader.problem(
                            record,
                            at,
                            format!("{}.{name}: a new row needs a value here", collection.name),
                        );
                    }
                    (_, true) => {}
                    // A revealed grid's plaintext, sealed as the save writes it: never read as
                    // JSON, and never a birth column.
                    (Ty::Secret, false) => cells.push((at, *schema, text.clone())),
                    (ty, false) => match read(&ty, text) {
                        Ok(x) => {
                            values.insert(name.clone(), x);
                            cells.push((at, *schema, text.clone()));
                        }
                        Err(rule) => reader.problem(
                            record,
                            at,
                            format!("{}.{name}: {rule}", collection.name),
                        ),
                    },
                }
            }
            let derived = match birth(collection, &values) {
                Ok(pk) if crate::typed::pk_grammar(&pk) => pk,
                Ok(_) => {
                    reader.problem(record, pk_at, "the derived key breaks the key grammar");
                    continue;
                }
                Err(error) => {
                    reader.problem(record, pk_at, error.to_string());
                    continue;
                }
            };
            if exported.contains_key(derived.as_str()) || !born.insert(derived.clone()) {
                reader.problem(
                    record,
                    pk_at,
                    format!("this new row is the row {derived}, which the file already holds"),
                );
                continue;
            }
            for (at, schema, text) in cells {
                reader.edits(record, at, &derived, schema, &text, None, &mut edits);
            }
            if !values.contains_key(LIFECYCLE)
                && let Some(lifecycle) = collection.column(LIFECYCLE)
            {
                reader.edits(record, pk_at, &derived, lifecycle, "live", None, &mut edits);
            }
            if !edits.is_empty() {
                plan.born.push(derived.clone());
                plan.rows.push((derived, edits));
            }
            continue;
        }
        let Some(seen) = exported.get(pk.as_str()) else {
            reader.problem(
                record,
                pk_at,
                "the pk column is read-only; a new row leaves its pk empty",
            );
            continue;
        };
        if !present.insert(pk.clone()) {
            reader.problem(record, pk_at, "this row appears twice");
            continue;
        }
        for (at, (name, schema, read_only)) in columns.iter().enumerate().take(pk_at) {
            let text = &texts[at];
            let held = seen.get(name);
            if held.is_some_and(|s| s.shown == *text) || (held.is_none() && text.is_empty()) {
                continue;
            }
            if *read_only {
                reader.problem(record, at, "this column is read-only (=)");
                continue;
            }
            if held.is_some_and(|s| s.raw && s.secret) {
                reader.problem(
                    record,
                    at,
                    "this row is protected, or its value does not open here: it is read-only",
                );
                continue;
            }
            if held.is_some_and(|s| s.raw) {
                reader.problem(
                    record,
                    at,
                    "this cell holds a value a newer build wrote; it is read-only here",
                );
                continue;
            }
            let Some(schema) = schema else {
                reader.problem(record, at, "this column is read-only (=)");
                continue;
            };
            if schema.fixed {
                reader.problem(
                    record,
                    at,
                    "the row's key comes from this column; end this row and add another",
                );
                continue;
            }
            reader.edits(record, at, &pk, schema, text, held, &mut edits);
        }
        if !edits.is_empty() {
            plan.rows.push((pk, edits));
        }
    }
    // Rows the owner deleted: retired, or ended, so their history stays.
    let word = if table.ends.contains(&collection.name) {
        "ended"
    } else {
        "retired"
    };
    if let Some(lifecycle) = collection.column(LIFECYCLE) {
        for (pk, seen) in &base.rows {
            if present.contains(pk) {
                continue;
            }
            let held = seen.get(LIFECYCLE);
            if held.is_some_and(|s| s.shown == word) {
                continue;
            }
            let mut edits = Vec::new();
            reader.edits(0, pk_at, pk, lifecycle, word, held, &mut edits);
            if !edits.is_empty() {
                plan.retired.push(pk.clone());
                plan.rows.push((pk.clone(), edits));
            }
        }
    }
    if reader.problems.is_empty() {
        Ok(plan)
    } else {
        Err(reader.problems)
    }
}

/// A changed cell whose register moved since the export: never overwritten.
#[derive(Clone, PartialEq, Eq)]
pub struct Stale {
    pub pk: String,
    pub column: String,
    /// What the owner typed, shown back beside the value that moved; never in `Debug`, since
    /// a revealed `Secret` cell's is plaintext.
    pub mine: String,
}

impl std::fmt::Debug for Stale {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stale")
            .field("pk", &self.pk)
            .field("column", &self.column)
            .finish_non_exhaustive()
    }
}

/// Seal each revealed `Secret` edit of `plan` in the writing session, with its `sealer`, and
/// check it against `cells` as they are now: a value on a row stored protected, or one this save
/// protects, is refused (a row is unprotected in a save of its own first). A null clears the
/// cell. Afterwards every edit is an ordinary one: its `x` the sealed token,
/// its `value` the record value [`settle`] writes.
///
/// # Errors
/// A row is protected, sealing fails, or the sealed cell fails its column's checks.
pub fn seal(
    collection: &Collection,
    plan: &mut Plan,
    sealer: &dyn super::secret::Sealer,
    cells: &Cells,
) -> Result<()> {
    let projection = cell::project(collection, cells);
    for (pk, edits) in &mut plan.rows {
        // Protected as stored, or by this save: a save that protects a row and types its password
        // together seals none, and neither does one that unprotects a row and types it.
        let protected = projection
            .rows
            .get(pk.as_str())
            .is_some_and(super::check::protected)
            || edits
                .iter()
                .any(|e| e.column == "protected" && e.x == Value::Bool(true));
        for edit in edits.iter_mut().filter(|e| e.secret && e.value.is_null()) {
            let x = match &edit.x {
                Value::String(text) => {
                    if protected {
                        return Err(Error::new(&format!(
                            "{}.{}: row {} is protected and holds no secret (unprotect it in a \
                             save of its own first)",
                            collection.name,
                            edit.column,
                            crate::att::command::safe_key(pk)
                        )));
                    }
                    let value = super::secret::Secret::new(text.clone());
                    Value::String(sealer.seal(collection.name, pk, &edit.column, &value)?)
                }
                _ => Value::Null,
            };
            let change = cell::change(
                collection,
                CellKey {
                    pk,
                    col: &edit.column,
                    elem: None,
                },
                &CellValue::owner(collection.version, x.clone()),
            )?;
            edit.x = x;
            edit.value = change.value.unwrap_or_default();
        }
    }
    Ok(())
}

/// What a save writes once compared with the table as it is now.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settled {
    /// Each row's changes, for one record each.
    pub rows: Vec<Vec<Change>>,
    /// Edits the table already holds (another writer made the same change).
    pub unchanged: usize,
    /// Cells that changed meanwhile.
    pub stale: Vec<Stale>,
    /// Keys of born rows whose changes this save writes.
    pub born: Vec<String>,
    /// Keys of rows whose lifecycle retirement or ending this save writes.
    pub retired: Vec<String>,
}

/// Whether `cells` hold any cell of the row `pk` in `collection`, in any column.
pub fn holds_row(collection: &Collection, cells: &Cells, pk: &str) -> bool {
    let prefix = format!("{pk}.");
    cells
        .range((collection.name.to_owned(), prefix.clone())..)
        .next()
        .is_some_and(|((name, key), _)| name == collection.name && key.starts_with(&prefix))
}

/// Compare each edit of `plan` with `cells` as they are now (design §8, compare-before-write):
/// an edit goes ahead only while its register's candidates are still exactly the exported
/// ones, so the write replaces just what the owner saw. A register that already holds the
/// owner's value as the owner's costs nothing; one that holds it only as an import's (a peer's
/// import wrote the same value meanwhile) is written, so the value becomes the owner's and no
/// later import replaces it; one that moved to something else is stale and left alone. A born
/// row is all or nothing: once the table holds any cell of its key (a peer made that row
/// meanwhile), none of it is written into a row the owner never saw, and each cell it does not
/// already hold is stale.
pub fn settle(collection: &Collection, plan: &Plan, cells: &Cells) -> Settled {
    let mut settled = Settled::default();
    for (pk, edits) in &plan.rows {
        let mut changes = Vec::new();
        let mut stale = BTreeSet::new();
        let taken = plan.born.contains(pk) && holds_row(collection, cells, pk);
        for edit in edits {
            let cell = cells.get(&(collection.name.to_owned(), edit.key.clone()));
            let mut now: Vec<Digest> = cell
                .map(|c| c.candidates.iter().map(|c| c.operation).collect())
                .unwrap_or_default();
            now.sort_unstable();
            let stopped = cell.is_some_and(|c| c.stopped);
            if !taken && !stopped && now == edit.expected {
                changes.push(Change {
                    collection: collection.name.to_owned(),
                    key: edit.key.clone(),
                    value: Some(edit.value.clone()),
                });
                continue;
            }
            // An import's candidate (its value names a source) is the import's, never the
            // owner's, whatever it holds.
            let mut imported = false;
            let mut values = Vec::new();
            if let Some(c) = cell {
                values.reserve(c.candidates.len());
                for candidate in &c.candidates {
                    match CellValue::decode(&candidate.value) {
                        Ok(v) => {
                            imported |= v.s.is_some();
                            values.push(v.x);
                        }
                        Err(_) => values.push(Value::Null),
                    }
                }
            }
            // A set element reads present when any candidate adds it (add-wins).
            let holds = !stopped
                && match &edit.elem {
                    Some(_) => values.contains(&Value::Bool(true)) == (edit.x == Value::Bool(true)),
                    None if values.is_empty() => edit.x.is_null(),
                    None => values.iter().all(|x| *x == edit.x),
                };
            if holds && imported && !taken {
                changes.push(Change {
                    collection: collection.name.to_owned(),
                    key: edit.key.clone(),
                    value: Some(edit.value.clone()),
                });
            } else if holds {
                settled.unchanged += 1;
            } else if stale.insert(edit.column.clone()) {
                settled.stale.push(Stale {
                    pk: pk.clone(),
                    column: edit.column.clone(),
                    mine: edit.mine.clone(),
                });
            }
        }
        if !changes.is_empty() {
            if !taken && plan.born.contains(pk) {
                settled.born.push(pk.clone());
            }
            if plan.retired.contains(pk) {
                let lifecycle_key = format!("{pk}.{LIFECYCLE}");
                if changes.iter().any(|c| c.key == lifecycle_key) {
                    settled.retired.push(pk.clone());
                }
            }
            settled.rows.push(changes);
        }
    }
    settled
}

/// The baseline to reopen the grid on after a save left `stale` cells: `fresh`, a new export of
/// the table as it is now, with each stale cell showing `⚠ mine ‖ theirs` (its candidates the
/// current ones), so leaving it keeps theirs and editing it to one value writes that.
pub fn reopen(mut fresh: Base, stale: &[Stale]) -> Base {
    for item in stale {
        if let Some((_, seen)) = fresh.rows.iter_mut().find(|(pk, _)| *pk == item.pk)
            && let Some(cell) = seen.get_mut(&item.column)
        {
            cell.shown = format!("{CONFLICT}{}{BETWEEN}{}", item.mine, cell.shown);
        }
    }
    fresh
}

/// The 1-based line of `text`, a [`render`] of `base`, that a reopened file puts the caret on:
/// the first stale row's.
pub fn stale_line(text: &str, base: &Base, stale: &[Stale]) -> usize {
    let row = base
        .rows
        .iter()
        .position(|(pk, _)| stale.iter().any(|s| s.pk == *pk));
    match (row, Sheet::scan(text, Delimiter::Tab)) {
        (Some(row), Ok(sheet)) => sheet
            .records
            .get(row + 1)
            .map_or(1, |record| line_of(text, record.raw.start)),
        _ => 1,
    }
}

impl Base {
    /// The stored baseline: names, shown texts and candidate digests.
    pub fn to_json(&self) -> Value {
        let digests = |d: &[Digest]| d.iter().map(|d| hex::encode_lower(d)).collect::<Vec<_>>();
        let rows: Vec<Value> = self
            .rows
            .iter()
            .map(|(pk, seen)| {
                let cells: Map = seen
                    .iter()
                    .map(|(name, s)| {
                        let elements: Map = s
                            .elements
                            .iter()
                            .map(|(e, d)| (e.clone(), json!(digests(d))))
                            .collect();
                        let shown = if s.secret && !s.shown.is_empty() {
                            super::secret::MASK
                        } else {
                            s.shown.as_str()
                        };
                        (
                            name.clone(),
                            json!({"shown": shown, "candidates": digests(&s.candidates),
                                "elements": elements, "raw": s.raw || s.secret}),
                        )
                    })
                    .collect();
                json!({"pk": pk, "cells": cells})
            })
            .collect();
        json!({"format": FORMAT, "table": self.table, "collection": self.collection,
            "version": self.version, "reveal": self.reveal, "header": self.header, "rows": rows})
    }

    /// Read a stored baseline.
    ///
    /// # Errors
    /// It is not one.
    pub fn from_json(value: &Value) -> Result<Self> {
        let bad = || Error::new("not a grid baseline");
        let exact = |v: &Value, fields: &[&str]| -> Result<()> {
            let Some(object) = v.as_object() else {
                return Err(bad());
            };
            if object.len() != fields.len() || fields.iter().any(|f| !object.contains_key(*f)) {
                return Err(bad());
            }
            Ok(())
        };
        let text = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);
        let digests = |v: Option<&Value>| -> Result<Vec<Digest>> {
            let mut parsed: Vec<Digest> = v
                .and_then(Value::as_array)
                .ok_or_else(bad)?
                .iter()
                .map(|d| {
                    d.as_str()
                        .and_then(|d| crate::database::decode_hex::<32>(d).ok())
                        .ok_or_else(bad)
                })
                .collect::<Result<_>>()?;
            parsed.sort_unstable();
            Ok(parsed)
        };
        exact(
            value,
            &[
                "format",
                "table",
                "collection",
                "version",
                "reveal",
                "header",
                "rows",
            ],
        )?;
        if value.get("format").and_then(Value::as_str) != Some(FORMAT) {
            return Err(bad());
        }
        let header: Vec<String> = value
            .get("header")
            .and_then(Value::as_array)
            .ok_or_else(bad)?
            .iter()
            .map(|h| h.as_str().map(str::to_owned).ok_or_else(bad))
            .collect::<Result<_>>()?;
        let pk_header = format!("{READ_ONLY}{PK}");
        let mut header_names = BTreeSet::new();
        if header.last() != Some(&pk_header)
            || header.iter().any(|h| {
                let stripped = h.trim_start_matches(READ_ONLY);
                stripped.is_empty() || !header_names.insert(stripped)
            })
        {
            return Err(bad());
        }
        let mut rows = Vec::new();
        let mut seen_pks = BTreeSet::new();
        for row in value
            .get("rows")
            .and_then(Value::as_array)
            .ok_or_else(bad)?
        {
            exact(row, &["pk", "cells"])?;
            let pk = text(row, "pk")
                .filter(|pk| !pk.is_empty())
                .ok_or_else(bad)?;
            if !seen_pks.insert(pk.clone()) {
                return Err(bad());
            }
            let mut seen = BTreeMap::new();
            for (name, s) in row
                .get("cells")
                .and_then(Value::as_object)
                .ok_or_else(bad)?
            {
                exact(s, &["shown", "candidates", "elements", "raw"])?;
                let mut elements = BTreeMap::new();
                for (e, d) in s
                    .get("elements")
                    .and_then(Value::as_object)
                    .ok_or_else(bad)?
                {
                    elements.insert(e.clone(), digests(Some(d))?);
                }
                seen.insert(
                    name.clone(),
                    Seen {
                        shown: text(s, "shown").ok_or_else(bad)?,
                        candidates: digests(s.get("candidates"))?,
                        elements,
                        raw: s.get("raw").and_then(Value::as_bool).ok_or_else(bad)?,
                        secret: false,
                    },
                );
            }
            rows.push((pk, seen));
        }
        Ok(Self {
            table: text(value, "table")
                .filter(|s| !s.is_empty())
                .ok_or_else(bad)?,
            collection: text(value, "collection")
                .filter(|s| !s.is_empty())
                .ok_or_else(bad)?,
            version: value
                .get("version")
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(bad)?,
            reveal: value
                .get("reveal")
                .and_then(Value::as_bool)
                .ok_or_else(bad)?,
            header,
            rows,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests;
