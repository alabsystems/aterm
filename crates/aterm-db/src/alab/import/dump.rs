// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A fleet_db replica's dump as import sources (docs/DESIGN-alab-one-mesh-2026-11-05.md §4.7,
//! §8 step 2): the JSON document `aterm db dump <file> --show-passwords` writes (fleet_db's
//! `dump_all`: `computers`, `emails`, `phones`, `ai_accounts`, each an array of row objects),
//! read here from its bytes alone, so nothing of fleet_db's code is needed once it is deleted.
//!
//! Each table becomes one [`Sheet`] of kind [`SheetKind::FleetDb`], its own source
//! (`<file>#<table>`) keyed by the whole dump, since a table's read leans on its siblings' (a
//! receiving mailbox's protection, an account read as none): an edit to one table is an edit to
//! every one, so no table takes back as its own word what that edit withdrew. Its header row is
//! every column the table's rows or their concurrent values hold: the known ones in
//! [`DUMP_TABLES`] order, then any other in name order, so **every column is carried or
//! reported**. [`DUMP_TABLES`] says where each known column goes: a registry cell, a `Secret`
//! cell, a relation the registry already states another way, the row's key, or nowhere by the
//! design's word (`ip`: addresses come from presence). A column it does not name is an
//! [`Kind::Unmapped`] finding, by name only, and its text never enters the sheet: it may be a
//! credential by a word no list here holds, and a review shows the text at a finding's coordinates.
//! A password never enters the sheet's cells either: it is held as a [`Secret`] at its coordinate
//! ([`Sheet::secrets`]), so no review, finding or log shows it. A row the replica holds concurrent values for (`_conflicts`) imports only the
//! columns its values agree on, a password's by what it says ([`cell_as_compared`]) and any other's
//! by its text; each other, but for a column the design carries nowhere, is a
//! [`Kind::DumpConflicted`] finding, whatever the row's own cell holds. A cell a guard refuses, or
//! a word the registry has no cell for, is an [`Kind::Unmapped`] finding by its column: no cell holds
//! it, so the replica is not covered until the owner settles it.
//!
//! A dump holds passwords, so its source key and `sha256` are a keyed digest ([`super::keyed_source`],
//! under the newest `[everyone]` epoch's key), never a plain hash a reader of the shared `source`
//! row could test guesses against; a dump that differs only in a password is still a new source.

use super::{
    ACCOUNT, ADDRESS, Coord, Dump, Finding, Kind, MACHINE, MAILBOX, Mark, Mode, PHONE, Planner,
    SheetKind, coord, provider_realm,
};
use super::{Sheet, column_service, names};
use crate::{
    Error, Result,
    machines::Label,
    typed::{
        guard, keys,
        secret::{Secret, cell_as_compared, flag_says},
    },
};
use aterm_codec::hex;
use aterm_json::{Value, json};
#[cfg(test)]
mod tests;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// What fleet_db printed in place of a password it was not asked to show: never a value.
pub(super) const REDACTED: &str = "<redacted>";

/// Where one fleet_db column goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Carry {
    /// A registry cell: `collection.column`.
    Cell(&'static str, &'static str),
    /// A registry `Secret` cell.
    Secret(&'static str, &'static str),
    /// The registry states it as a relation (an address's `phone`, a preference): named.
    Relation(&'static str),
    /// The row's key, from which the registry key derives.
    Key,
    /// Not carried, by the design's word: why.
    Dropped(&'static str),
}

/// Every column fleet_db's tables hold (`fleet_db.rs` `bootstrap_from_secrets`,
/// `normalize_row`), table by table in the dump's names, and where each goes.
/// One dump table: its name in the dump, its kind, and each column with where it goes.
pub type DumpTable = (&'static str, Dump, &'static [(&'static str, Carry)]);

pub const DUMP_TABLES: [DumpTable; 4] = [
    (
        "phones",
        Dump::Phones,
        &[
            ("phone", Carry::Secret("phone", "number")),
            ("label", Carry::Cell("phone", "label")),
            ("owner_carrier", Carry::Cell("phone", "carrier")),
            ("line_type", Carry::Cell("phone", "line_type")),
            ("device", Carry::Cell("phone", "device")),
            ("status", Carry::Cell("phone", "row")),
            ("linked_emails", Carry::Relation("address.phone")),
            ("linked_computers", Carry::Relation("address.phone")),
            ("notes", Carry::Cell("phone", "note")),
        ],
    ),
    (
        "emails",
        Dump::Emails,
        &[
            ("email", Carry::Key),
            ("provider", Carry::Cell("address", "provider")),
            ("account_type", Carry::Cell("address", "kind")),
            ("root_account", Carry::Cell("address", "alias_of")),
            ("password", Carry::Secret("address", "password")),
            ("webmail_url", Carry::Cell("mailbox", "webmail")),
            ("status", Carry::Cell("address", "status")),
            ("phone", Carry::Cell("address", "phone")),
            ("protected", Carry::Cell("address", "protected")),
            (
                "assigned_computers",
                Carry::Relation("preference, by the ai_accounts row"),
            ),
            (
                "services",
                Carry::Relation("account.services, by the ai_accounts row"),
            ),
            ("notes", Carry::Cell("address", "note")),
        ],
    ),
    (
        "ai_accounts",
        Dump::Accounts,
        &[
            ("id", Carry::Key),
            ("provider", Carry::Cell("account", "provider")),
            ("service", Carry::Cell("account", "services")),
            ("email", Carry::Cell("account", "login")),
            ("receiving_mailbox", Carry::Cell("address", "mailbox")),
            ("assigned_computers", Carry::Relation("preference")),
            ("tier", Carry::Cell("account", "region")),
            ("status", Carry::Cell("account", "status")),
            ("notes", Carry::Cell("account", "note")),
        ],
    ),
    (
        "computers",
        Dump::Computers,
        &[
            ("label", Carry::Key),
            ("hostname", Carry::Cell("machine", "hosts")),
            ("ip", Carry::Dropped("addresses come from presence")),
            ("node_id", Carry::Cell("machine", "fleet_node")),
            ("make", Carry::Cell("machine", "make")),
            ("arch", Carry::Cell("machine", "arch")),
            ("os", Carry::Cell("machine", "os")),
            ("chip", Carry::Cell("machine", "chip")),
            ("memory_gb", Carry::Cell("machine", "ram_gb")),
            ("disk_gb", Carry::Cell("machine", "disk_gb")),
            ("aliases", Carry::Cell("machine", "aliases")),
            ("nickname", Carry::Cell("machine", "nickname")),
            (
                "nickname_aliases",
                Carry::Cell("machine", "nickname_aliases"),
            ),
            ("location", Carry::Cell("machine", "location")),
            ("status", Carry::Cell("machine", "status")),
            ("assigned_accounts", Carry::Relation("preference")),
            (
                "assigned_emails",
                Carry::Relation("preference, by assigned_accounts"),
            ),
            ("notes", Carry::Cell("machine", "note")),
        ],
    ),
];

/// The columns of `table` this build knows.
fn known(table: Dump) -> &'static [(&'static str, Carry)] {
    DUMP_TABLES
        .iter()
        .find(|(_, t, _)| *t == table)
        .map_or(&[], |(_, _, columns)| columns)
}

/// Whether a column of `table` holds a password.
fn secret_column(table: Dump, name: &str) -> bool {
    known(table)
        .iter()
        .any(|(column, carry)| *column == name && matches!(carry, Carry::Secret(_, "password")))
        || (!known(table).iter().any(|(column, _)| *column == name) && super::secret_header(name))
}

/// One cell's text in the sheet: a string as it is, a list as its items one per line (an item
/// may hold a space: [`items`] reads them back), a number or a flag as written, and `null` empty.
fn text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(_) => value.to_string(),
        Value::Array(items) => items
            .iter()
            .map(text)
            .filter(|item| !item.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(_) => aterm_json::to_string(value).unwrap_or_default(),
    }
}

/// The items of a list cell: a list's lines, or the parts of a string fleet_db split on `,` and
/// `;`, each trimmed, the empty dropped.
fn items(text: &str) -> impl Iterator<Item = &str> {
    text.split(['\n', ',', ';'])
        .map(str::trim)
        .filter(|item| !item.is_empty())
}

/// The label a dump source's keyed digest derives its key under: version 2 keys each table by the
/// whole dump.
const SOURCE_DIGEST: &[u8] = b"aterm/alab/dump-source-digest/2\0";

/// The four tables of the dump `bytes`, file `name`, as sources read in `mode`, each keyed by
/// its digest under `everyone_key` ([`super::keyed_source`]).
///
/// # Errors
/// The bytes are not a fleet_db dump: not a JSON object, a table that is not an array of
/// objects, or a table name it does not hold.
pub fn dump_sheets(
    name: &str,
    bytes: &[u8],
    mode: Mode,
    everyone_key: &[u8; 32],
) -> Result<Vec<Sheet>> {
    let malformed = || Error::new("the file is not a fleet_db dump (`aterm db dump <file>`)");
    let name = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
    let document: Value = aterm_json::from_slice(bytes).map_err(|_| malformed())?;
    let object = document.as_object().ok_or_else(malformed)?;
    // What every table's source is keyed by, beside its own name.
    let whole = zeroize::Zeroizing::new(aterm_json::to_vec(&document).map_err(|_| malformed())?);
    if object
        .keys()
        .any(|table| !DUMP_TABLES.iter().any(|(word, _, _)| word == table))
    {
        return Err(malformed());
    }
    let mut sheets = Vec::new();
    for (word, table, columns) in DUMP_TABLES {
        let Some(items) = object.get(word) else {
            continue;
        };
        let items = items.as_array().ok_or_else(malformed)?;
        let file = format!("{name}#{word}");
        if file.is_empty() || guard::text(&file, 128).is_err() {
            return Err(Error::new("the dump's file name cannot be recorded"));
        }
        // Every column any row or concurrent value holds, since the digest order alone decides
        // which value a row shows, the known first: what a read adds (`_conflicts`, a concurrent
        // delete's `_deleted`, an email row's `has_password`) is not a column.
        let known_set: BTreeSet<&str> = columns.iter().map(|(c, _)| *c).collect();
        let mut headers: Vec<String> = columns.iter().map(|(c, _)| (*c).to_owned()).collect();
        let mut others = BTreeSet::new();
        for item in items {
            let row = item.as_object().ok_or_else(malformed)?;
            let concurrent = row
                .get("_conflicts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_object);
            for key in std::iter::once(row)
                .chain(concurrent)
                .flat_map(|fields| fields.keys())
            {
                let added = matches!(key.as_str(), "_conflicts" | "_deleted")
                    || (table == Dump::Emails && key == "has_password");
                if !added && !known_set.contains(key.as_str()) {
                    others.insert(key.clone());
                }
            }
        }
        headers.extend(others);
        let is_secret: Vec<bool> = headers.iter().map(|h| secret_column(table, h)).collect();
        let mut rows = Vec::with_capacity(items.len());
        let mut secrets = BTreeMap::new();
        let mut conflicted = BTreeSet::new();
        for (index, item) in items.iter().enumerate() {
            let row_at = index + 2;
            let row = item.as_object().ok_or_else(malformed)?;
            let mut cells = Vec::with_capacity(headers.len());
            for (col, header) in headers.iter().enumerate() {
                let value = row.get(header).unwrap_or(&Value::Null);
                if is_secret[col] {
                    if let Some(secret) = value.as_str().filter(|s| !s.trim().is_empty()) {
                        secrets.insert((row_at, col), Arc::new(Secret::new(secret.to_owned())));
                    }
                    cells.push(String::new());
                } else {
                    let cell = text(value);
                    cells.push(if col >= columns.len() && !cell.trim().is_empty() {
                        UNREAD_CELL.to_owned()
                    } else {
                        cell
                    });
                }
            }
            // The replica's concurrent values: a column they disagree on is never imported,
            // and a concurrent delete marks the whole row.
            for other in row
                .get("_conflicts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(other) = other.as_object() else {
                    conflicted.insert((row_at, usize::MAX));
                    continue;
                };
                if other.get("_deleted").and_then(Value::as_bool) == Some(true) {
                    conflicted.insert((row_at, usize::MAX));
                    continue;
                }
                for (col, header) in headers.iter().enumerate() {
                    // A column the design carries nowhere is none of the registry's business.
                    if matches!(columns.get(col), Some((_, Carry::Dropped(_)))) {
                        continue;
                    }
                    let disagree = if is_secret[col] {
                        cell_as_compared(row.get(header)) != cell_as_compared(other.get(header))
                    } else {
                        let read = |cell: Option<&Value>| text(cell.unwrap_or(&Value::Null));
                        read(row.get(header)).trim() != read(other.get(header)).trim()
                    };
                    if disagree {
                        conflicted.insert((row_at, col));
                    }
                }
            }
            rows.push(cells);
        }
        let mut canonical =
            zeroize::Zeroizing::new(Vec::with_capacity(file.len() + 1 + whole.len()));
        canonical.extend_from_slice(file.as_bytes());
        canonical.push(0);
        canonical.extend_from_slice(&whole);
        let (source, sha256) = super::keyed_source(SOURCE_DIGEST, &canonical, everyone_key);
        sheets.push(Sheet {
            kind: SheetKind::FleetDb(table),
            name: file,
            source,
            sha256,
            headers,
            rows,
            mode,
            secrets,
            conflicted,
            earlier: BTreeSet::new(),
        });
    }
    Ok(sheets)
}

/// What a sheet holds for a cell of a column this build does not name: it says the row holds
/// something, and nothing of what.
const UNREAD_CELL: &str = "(withheld: a column this build does not read)";

/// One row of a dump table as the planner reads it.
struct Read<'s> {
    n: usize,
    sheet: &'s Sheet,
    row: usize,
    cells: &'s [String],
}

impl Read<'_> {
    fn col(&self, name: &str) -> Option<usize> {
        self.sheet.headers.iter().position(|h| h == name)
    }

    fn at(&self, col: usize) -> Coord {
        coord(self.sheet, self.n, self.row, col)
    }

    /// A column's text, with its coordinate and whether the replica holds concurrent values.
    fn get(&self, name: &str) -> Option<(String, Coord, bool)> {
        let col = self.col(name)?;
        let text = self.cells.get(col).map_or("", |c| c.trim());
        let conflicted = self.sheet.conflicted.contains(&(self.row, col));
        (!text.is_empty()).then(|| (text.to_owned(), self.at(col), conflicted))
    }

    fn secret(&self, name: &str) -> Option<(Arc<Secret>, Coord, bool)> {
        let col = self.col(name)?;
        let value = self.sheet.secrets.get(&(self.row, col))?.clone();
        let conflicted = self.sheet.conflicted.contains(&(self.row, col));
        Some((value, self.at(col), conflicted))
    }

    /// Whether the replica holds concurrent values for a column, an empty one included.
    fn conflicted(&self, name: &str) -> bool {
        self.col(name)
            .is_some_and(|col| self.sheet.conflicted.contains(&(self.row, col)))
    }

    fn row_conflicted(&self) -> bool {
        self.sheet.conflicted.contains(&(self.row, usize::MAX))
    }

    /// The columns of this row its concurrent values disagree on, a whole-row mark included.
    fn conflicts(&self) -> impl Iterator<Item = usize> + '_ {
        self.sheet
            .conflicted
            .range((self.row, 0)..(self.row + 1, 0))
            .map(|(_, col)| *col)
    }
}

/// A column's name as a finding may carry it, which a read of a replica's column cannot promise.
fn column_named(header: &str) -> Option<String> {
    guard::text(header, 64)
        .is_ok()
        .then(|| header.to_owned())
        .filter(|h| !h.contains('@'))
}

/// The account realms a `service` of fleet_db's `assigned_accounts` means.
fn assigned_realms(service: &str) -> &'static [&'static str] {
    match service {
        "claude" => &["claude"],
        "codex" => &["codex"],
        "gemini" => &["google"],
        "meta" => &["meta"],
        "grok" => &["grok"],
        "kimi" => &["kimi:global", "kimi:mainland-cn"],
        _ => &[],
    }
}

/// The provider word fleet_db's `ai_accounts.provider` names.
fn provider_word(raw: &str) -> Option<&'static str> {
    keys::PROVIDERS
        .iter()
        .copied()
        .find(|p| p.eq_ignore_ascii_case(raw.trim()))
}

/// Whether a `protected` word says the row is not.
fn says_unprotected(word: &str) -> bool {
    word.eq_ignore_ascii_case("false") || word == "0" || word.eq_ignore_ascii_case("no")
}

/// Whether a cell is one phone number and nothing else: no word or second number whose digits
/// [`super::phone_digits`] would run into it. E.164 holds at most 15 digits. A cell spaced into
/// groups is one national number with its `1` at most (11 digits) unless it is written with its
/// `+`, so two short numbers in one cell read as more; a bare run of digits is a spreadsheet's
/// number cell, which drops the `+`. A spreadsheet's float (`4155550115.0`) is no number either:
/// its last digits are no part of it, while a number grouped by dots (`01.23.45.67.00`) is one.
fn one_number(raw: &str) -> bool {
    let raw = raw.trim();
    let digits = raw.bytes().filter(u8::is_ascii_digit).count();
    let bare = raw.bytes().all(|b| b.is_ascii_digit());
    let float = raw.split_once('.').is_some_and(|(whole, fraction)| {
        !whole.is_empty()
            && whole.bytes().all(|b| b.is_ascii_digit())
            && (1..=2).contains(&fraction.len())
            && fraction.bytes().all(|b| b == b'0')
    });
    raw.chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '.' | '(' | ')' | ' '))
        && digits <= 15
        && (bare || raw.starts_with('+') || digits <= 11)
        && !float
}

/// The table `table` of the dump `sheet` was read from.
fn sibling<'s>(sheets: &'s [Sheet], sheet: &Sheet, table: Dump) -> Option<&'s Sheet> {
    let file = sheet.name.rsplit_once('#').map(|(file, _)| file);
    sheets.iter().find(|other| {
        other.kind == SheetKind::FleetDb(table)
            && other.name.rsplit_once('#').map(|(file, _)| file) == file
    })
}

/// A row's cell by its column's name, blank where the sheet has no such column.
fn named<'c>(sheet: &Sheet, cells: &'c [String], name: &str) -> &'c str {
    sheet
        .headers
        .iter()
        .position(|header| header == name)
        .and_then(|col| cells.get(col))
        .map_or("", |cell| cell.trim())
}

/// A machine as two rows of a dump name it: by its stored label.
fn computer_word(word: &str) -> String {
    Label::parse(word).map_or_else(|_| word.to_ascii_lowercase(), |label| label.canonical())
}

/// A service as two rows of a dump name it: by the service the word means.
fn service_key(word: &str) -> String {
    super::service_word(word).map_or_else(
        || word.to_ascii_lowercase(),
        |(_, service)| service.to_owned(),
    )
}

/// What a dump's `ai_accounts` rows state of one login: what its `emails` row repeats.
#[derive(Default)]
struct AccountRelations {
    computers: BTreeSet<String>,
    services: BTreeSet<String>,
}

impl Planner<'_> {
    /// One dump table's rows, every column carried as [`DUMP_TABLES`] says.
    pub(super) fn dump(&mut self, n: usize, sheet: &Sheet, table: Dump) -> Result<()> {
        let collection = match table {
            Dump::Emails => ADDRESS.name,
            Dump::Phones => PHONE.name,
            Dump::Accounts => ACCOUNT.name,
            Dump::Computers => MACHINE.name,
        };
        for header in &sheet.headers {
            if !known(table).iter().any(|(column, _)| column == header) {
                self.findings.insert(Finding {
                    kind: Kind::Unmapped,
                    collection: Some(collection),
                    pk: None,
                    column: column_named(header),
                    at: Vec::new(),
                });
            }
        }
        let relations = if table == Dump::Emails {
            self.dump_account_relations(sheet)
        } else {
            BTreeMap::new()
        };
        for (index, cells) in sheet.rows.iter().enumerate() {
            let read = Read {
                n,
                sheet,
                row: index + 2,
                cells,
            };
            if cells.iter().all(|c| c.trim().is_empty())
                && !sheet.secrets.keys().any(|(r, _)| *r == read.row)
                && read.conflicts().next().is_none()
            {
                continue;
            }
            let pk = match table {
                Dump::Emails => self.dump_email(&read, &relations)?,
                Dump::Phones => self.dump_phone(&read)?,
                Dump::Accounts => self.dump_account(&read)?,
                Dump::Computers => self.dump_computer(&read),
            };
            self.disagreements(collection, table, pk, &read);
        }
        if table == Dump::Computers {
            self.machine_statuses();
        }
        Ok(())
    }

    /// Settle what the table's rows state of `status`. fleet_db wrote `active` for every row, so that
    /// word says nothing about a row no Mac has joined: moved off `planned`, the row could no longer
    /// be adopted by the Mac it waits for (`register`). Any other word is the owner's, on a birth as
    /// on a later run, so one plan settles it; it is read first, so two rows on one machine settle on
    /// the same word whichever the replica lists first.
    fn machine_statuses(&mut self) {
        let mut statuses = std::mem::take(&mut self.statuses);
        statuses.sort_by_key(|(_, word, _)| word == "active");
        for (pk, word, at) in statuses {
            if word == "active" && self.status_planned(&pk) {
                continue;
            }
            // A placeholder this run minted gives way to what the source states of it.
            if self.status_planned(&pk)
                && let Some(row) = self
                    .rows
                    .get_mut(MACHINE.name)
                    .and_then(|table| table.get_mut(&pk))
            {
                row.values.remove("status");
            }
            self.want(&MACHINE, &pk, "status", json!(word), &at);
        }
    }

    /// The accounts a machine row assigns stay unsettled when the row reaches no machine: it may be
    /// what states their preferences, which must not end for that. A disputed row needs no such word,
    /// since a run reading one takes nothing back (`ReadNow::settled`).
    fn unsettle_assigned(&mut self, read: &Read<'_>) {
        let Some((ids, _, _)) = read.get("assigned_accounts") else {
            return;
        };
        for id in items(&ids).flat_map(str::split_whitespace) {
            let Some((service, email)) = id.split_once(':') else {
                continue;
            };
            self.unsettled.extend(
                assigned_realms(service)
                    .iter()
                    .filter_map(|realm| keys::account_pk(realm, email).ok()),
            );
        }
    }

    /// A finding for every column of a row its concurrent values disagree on ([`dump_sheets`] marks
    /// no [`Carry::Dropped`] one) and for a whole-row mark, whatever the row's own cell holds and
    /// whichever value the digest order showed, so no read above can leave one out and the order
    /// cannot decide `covered`. `pk` is `None` while the row's key is undecided.
    fn disagreements(
        &mut self,
        collection: &'static str,
        table: Dump,
        pk: Option<String>,
        read: &Read<'_>,
    ) {
        for col in read.conflicts() {
            let Some(header) = read.sheet.headers.get(col) else {
                self.findings.insert(Finding {
                    kind: Kind::DumpConflicted,
                    collection: Some(collection),
                    pk: pk.clone(),
                    column: None,
                    at: Vec::new(),
                });
                continue;
            };
            // This build's own word for a column it knows, `password` included; the guard is for
            // a name only a replica's row says.
            let column = if known(table).iter().any(|(column, _)| column == header) {
                Some(header.clone())
            } else {
                column_named(header)
            };
            self.findings.insert(Finding {
                kind: Kind::DumpConflicted,
                collection: Some(collection),
                pk: pk.clone(),
                column,
                at: vec![read.at(col)],
            });
        }
    }

    /// A column of a row the replica holds concurrent values for: a finding, never written.
    fn agreed(
        &mut self,
        collection: &'static str,
        pk: &str,
        column: &str,
        got: Option<(String, Coord, bool)>,
    ) -> Option<(String, Coord)> {
        let (text, at, conflicted) = got?;
        if conflicted {
            self.findings.insert(Finding {
                kind: Kind::DumpConflicted,
                collection: Some(collection),
                pk: Some(pk.to_owned()),
                column: Some(column.to_owned()),
                at: vec![at],
            });
            return None;
        }
        Some((text, at))
    }

    /// A row the replica holds a concurrent delete or an unreadable value for: a finding, and
    /// nothing of it is imported.
    fn row_conflict(&mut self, collection: &'static str, pk: &str, read: &Read<'_>) -> bool {
        let conflicted = read.row_conflicted();
        if conflicted {
            self.findings.insert(Finding {
                kind: Kind::DumpConflicted,
                collection: Some(collection),
                pk: Some(pk.to_owned()),
                column: None,
                at: Vec::new(),
            });
        }
        conflicted
    }

    /// What this dump's `ai_accounts` rows state of each login: an `emails` row's relations are
    /// carried by those rows, element by element.
    fn dump_account_relations(&self, sheet: &Sheet) -> BTreeMap<String, AccountRelations> {
        let mut stated: BTreeMap<String, AccountRelations> = BTreeMap::new();
        let Some(accounts) = sibling(self.input.sheets, sheet, Dump::Accounts) else {
            return stated;
        };
        for cells in &accounts.rows {
            let Ok(address) = keys::address(named(accounts, cells, "email")) else {
                continue;
            };
            let relations = stated.entry(address).or_default();
            relations.computers.extend(
                items(named(accounts, cells, "assigned_computers"))
                    .flat_map(str::split_whitespace)
                    .map(computer_word),
            );
            let service = named(accounts, cells, "service");
            if !service.is_empty() {
                relations.services.insert(service_key(service));
            } else if let Some(provider) = provider_word(named(accounts, cells, "provider")) {
                relations
                    .services
                    .insert(super::column_service(provider).to_owned());
            }
        }
        // A machine's own row states the link too, and a preference is read from it: a login on
        // two rows of the sheet keeps only its last machines on its `ai_accounts` row.
        if let Some(computers) = sibling(self.input.sheets, sheet, Dump::Computers) {
            for cells in &computers.rows {
                let machine = computer_word(named(computers, cells, "label"));
                for id in items(named(computers, cells, "assigned_accounts"))
                    .flat_map(str::split_whitespace)
                {
                    if let Some(address) = id
                        .split_once(':')
                        .and_then(|(_, email)| keys::address(email).ok())
                    {
                        stated
                            .entry(address)
                            .or_default()
                            .computers
                            .insert(machine.clone());
                    }
                }
            }
        }
        stated
    }

    /// The addresses a replica's dump or an emails sheet marks protected, read before any sheet: a
    /// sheet read first makes no mailbox for a row a later source protects. A word that says
    /// neither, and a flag the replica holds concurrent values for, guard the row as a conflicted
    /// registry cell does.
    pub(super) fn dump_marks(&mut self) {
        let dumps = self.input.sheets.iter();
        let marks: Vec<String> = dumps
            .filter(|sheet| sheet.kind == SheetKind::FleetDb(Dump::Emails))
            .flat_map(|sheet| {
                let col = sheet
                    .headers
                    .iter()
                    .position(|header| header == "protected");
                sheet
                    .rows
                    .iter()
                    .enumerate()
                    .filter_map(move |(index, cells)| {
                        let unsaid =
                            col.is_some_and(|col| sheet.conflicted.contains(&(index + 2, col)));
                        let word = named(sheet, cells, "protected");
                        (unsaid || !(word.is_empty() || says_unprotected(word)))
                            .then(|| named(sheet, cells, "email").to_owned())
                    })
            })
            .collect();
        // An emails sheet's protection flag, read in `emails` by the same column words.
        let flagged: Vec<String> = self
            .input
            .sheets
            .iter()
            .filter(|sheet| sheet.kind == SheetKind::Emails)
            .flat_map(|sheet| {
                let email = sheet.column(|h| h == "email address" || h == "email");
                let flag = sheet.column(|h| h.starts_with("protection"));
                sheet.rows.iter().filter_map(move |cells| {
                    let (email, flag) = (email?, flag?);
                    cells
                        .get(flag)
                        .is_some_and(|cell| !cell.trim().is_empty())
                        .then(|| cells.get(email).map(|cell| cell.trim().to_owned()))
                        .flatten()
                })
            })
            .collect();
        for address in marks.into_iter().chain(flagged) {
            if let Some(pk) = keys::address(&address)
                .ok()
                .and_then(|address| self.address_key(&address))
            {
                self.protected.insert(pk);
            }
        }
    }

    /// The labels a replica's `phones` rows of this run state: a label this run makes up leaves
    /// each of them free for the row that says it.
    pub(super) fn replica_phone_labels(&self) -> BTreeSet<String> {
        self.input
            .sheets
            .iter()
            .filter(|sheet| sheet.kind == SheetKind::FleetDb(Dump::Phones))
            .flat_map(|sheet| {
                sheet
                    .rows
                    .iter()
                    .map(|cells| named(sheet, cells, "label").trim().to_owned())
            })
            .collect()
    }

    /// What a column of the dump holds and no row or cell can: a finding, by name only.
    fn uncarried(&mut self, collection: &'static str, pk: &str, column: &str, at: Coord) {
        self.findings.insert(Finding {
            kind: Kind::Unmapped,
            collection: Some(collection),
            pk: Some(pk.to_owned()),
            column: Some(column.to_owned()),
            at: vec![at],
        });
    }

    /// A cell a guard refuses: its coordinate is kept for the owner, and a finding says no cell
    /// holds it.
    fn refused(
        &mut self,
        collection: &'static crate::typed::Collection,
        pk: &str,
        column: &str,
        at: Coord,
    ) {
        self.note(collection, pk, &at);
        self.uncarried(collection.name, pk, column, at);
    }

    /// Whether an address may hold no mailbox: a source read now or the registry marks it.
    pub(super) fn guarded(&self, address_pk: &str) -> bool {
        self.protected.contains(address_pk)
            || self.current[ADDRESS.name]
                .rows
                .get(address_pk)
                .is_some_and(crate::typed::check::protected)
    }

    /// The row an address names, with no side effect: what [`Self::address`] resolves to.
    pub(super) fn address_key(&self, address: &str) -> Option<String> {
        let derived = keys::address_pk(address).ok()?;
        if self.has_row(ADDRESS.name, &derived) {
            return Some(derived);
        }
        match self.address_index.matched(address).ok()? {
            super::AddressHolder::Row(pk) => Some(pk.to_owned()),
            super::AddressHolder::Several(_) => None,
            super::AddressHolder::Free { .. } => Some(derived),
        }
    }

    /// A note column through the note guard into the row's `note`, its coordinate kept.
    fn dump_note(
        &mut self,
        collection: &'static crate::typed::Collection,
        pk: &str,
        read: &Read<'_>,
    ) {
        if let Some((text, at)) = self.agreed(collection.name, pk, "notes", read.get("notes")) {
            self.note(collection, pk, &at);
            if let Some((note, note_at)) = self.note_text(collection, pk, &[(at, text)]) {
                self.want(collection, pk, "note", json!(note), &note_at);
            }
        }
    }

    fn dump_email(
        &mut self,
        read: &Read<'_>,
        relations: &BTreeMap<String, AccountRelations>,
    ) -> Result<Option<String>> {
        let kind = read.sheet.kind;
        let email = read.col("email");
        let Some(address) = self.address_of(read.n, read.sheet, read.row, read.cells, email) else {
            return Ok(None);
        };
        let email_at = read.at(email.unwrap_or(0));
        let Some(pk) = self.address(&address, kind, &email_at) else {
            return Ok(None);
        };
        if self.row_conflict(ADDRESS.name, &pk, read) {
            return Ok(Some(pk));
        }
        if let Some((value, at)) =
            self.agreed(ADDRESS.name, &pk, "protected", read.get("protected"))
        {
            if value.eq_ignore_ascii_case("true")
                || value == "1"
                || value.eq_ignore_ascii_case("yes")
            {
                self.protect(&pk, &at);
            } else if !says_unprotected(&value) {
                // A word that says neither guards the row until the owner says which
                // ([`Self::dump_marks`]).
                self.uncarried(ADDRESS.name, &pk, "protected", at);
            }
        }
        let stated = relations.get(&address);
        if let Some((cell, at, _)) = read.get("assigned_computers")
            && items(&cell)
                .flat_map(str::split_whitespace)
                .map(computer_word)
                .any(|word| {
                    !stated.is_some_and(|stated| self.names_one_of(&word, &stated.computers))
                })
        {
            self.uncarried(ADDRESS.name, &pk, "assigned_computers", at);
        }
        if let Some((cell, at, _)) = read.get("services")
            && items(&cell)
                .map(service_key)
                .any(|word| !stated.is_some_and(|stated| stated.services.contains(&word)))
        {
            self.uncarried(ADDRESS.name, &pk, "services", at);
        }
        // A replica that does not say so yet leaves what the registry says.
        let protected = self.guarded(&pk);
        if let Some((value, at)) = self.agreed(ADDRESS.name, &pk, "provider", read.get("provider"))
        {
            let word = if value.eq_ignore_ascii_case("proton")
                || value.eq_ignore_ascii_case("protonmail")
            {
                "proton"
            } else if value.eq_ignore_ascii_case("gmail") {
                "gmail"
            } else if value.eq_ignore_ascii_case("google") {
                "google"
            } else if value.eq_ignore_ascii_case("outlook") || value.eq_ignore_ascii_case("hotmail")
            {
                "outlook"
            } else if value.eq_ignore_ascii_case("privateemail") {
                "privateemail"
            } else {
                if !value.eq_ignore_ascii_case("other") {
                    self.uncarried(ADDRESS.name, &pk, "provider", at.clone());
                }
                "other"
            };
            self.want(&ADDRESS, &pk, "provider", json!(word), &at);
        }
        if let Some((value, at)) = self.agreed(ADDRESS.name, &pk, "status", read.get("status")) {
            let word = if value.eq_ignore_ascii_case("active") {
                "active"
            } else if value.eq_ignore_ascii_case("suspended") {
                "suspended"
            } else if value.eq_ignore_ascii_case("disabled") {
                "disabled"
            } else {
                if !value.eq_ignore_ascii_case("unknown") {
                    self.uncarried(ADDRESS.name, &pk, "status", at.clone());
                }
                "unknown"
            };
            self.want(&ADDRESS, &pk, "status", json!(word), &at);
        }
        // A word the replica holds concurrent values for decides nothing: not what the row is, nor
        // where its password and mailbox go.
        let undecided = read.conflicted("account_type");
        let alias = !undecided
            && read
                .get("account_type")
                .is_some_and(|(word, _, _)| word.eq_ignore_ascii_case("alias"));
        let root = self
            .agreed(ADDRESS.name, &pk, "root_account", read.get("root_account"))
            .and_then(|(text, at)| match keys::address(&text) {
                Ok(root) => Some((root, at)),
                Err(_) => {
                    self.uncarried(ADDRESS.name, &pk, "root_account", at);
                    None
                }
            })
            .filter(|(root, _)| *root != address);
        let mut mailbox = None;
        match (alias, root) {
            _ if undecided => {}
            (true, Some((root, at))) => {
                self.corrected.insert(pk.clone());
                if let Some(root_pk) = self.address(&root, kind, &at) {
                    self.link_alias(&pk, &root_pk, &at);
                }
                if !protected && let Some(mb) = self.mailbox(&root, kind, &at)? {
                    self.want(&ADDRESS, &pk, "mailbox", json!(mb), &at);
                }
            }
            // An alias whose root is no other address says what no cell can hold; a root cell
            // that is the reason is reported above.
            (true, None) => {
                let rootless = !read.conflicted("root_account")
                    && read.get("root_account").is_none_or(|(text, _, _)| {
                        keys::address(&text).is_ok_and(|root| root == address)
                    });
                if rootless && let Some((_, at, _)) = read.get("account_type") {
                    self.uncarried(ADDRESS.name, &pk, "account_type", at);
                }
            }
            (false, root) => {
                // Only an alias holds another address's name.
                if let Some((_, at)) = root {
                    self.uncarried(ADDRESS.name, &pk, "root_account", at);
                }
                if let Some((word, at, false)) = read.get("account_type") {
                    if word.eq_ignore_ascii_case("root") || word.eq_ignore_ascii_case("standalone")
                    {
                        self.want(&ADDRESS, &pk, "kind", json!("standalone"), &at);
                    } else {
                        self.uncarried(ADDRESS.name, &pk, "account_type", at);
                    }
                }
            }
        }
        if let Some((secret, at, conflicted)) = read.secret("password") {
            if conflicted {
                self.agreed(
                    ADDRESS.name,
                    &pk,
                    "password",
                    Some((String::new(), at, true)),
                );
            } else if flag_says(&secret) == Some(false) {
                // A none word claims no password and no login: it is judged against the registry
                // and makes nothing, as an empty cell makes nothing.
                self.want_secret(&ADDRESS, &pk, "password", secret, &at);
            } else if protected {
                self.findings.insert(Finding {
                    kind: Kind::ProtectedSecret,
                    collection: Some(ADDRESS.name),
                    pk: Some(pk.clone()),
                    column: Some("password".to_owned()),
                    at: vec![at],
                });
            } else if undecided {
                // Reported with the word: nothing says yet whether this row can hold it.
            } else if alias {
                // An alias signs in through its root's mailbox: a password on it has no row.
                self.findings.insert(Finding {
                    kind: Kind::Unmapped,
                    collection: Some(ADDRESS.name),
                    pk: Some(pk.clone()),
                    column: Some("password".to_owned()),
                    at: vec![at],
                });
            } else {
                self.want_secret(&ADDRESS, &pk, "password", secret, &at);
                // A login with a password signs in to its own mailbox.
                if let Some(mb) = self.mailbox(&address, kind, &at)? {
                    self.want(&ADDRESS, &pk, "mailbox", json!(mb), &at);
                    mailbox = Some(mb);
                }
            }
        }
        // The provider's default decides whether a webmail is carried, so one the replica holds
        // concurrent values for decides nothing.
        if let Some((url, at)) =
            self.agreed(ADDRESS.name, &pk, "webmail_url", read.get("webmail_url"))
            && !read.conflicted("provider")
        {
            let default = read
                .get("provider")
                .map_or("", |(p, _, _)| default_webmail(&p));
            if url != default {
                match &mailbox {
                    Some(mb)
                        if crate::typed::cell::validate(
                            &MAILBOX,
                            MAILBOX
                                .column("webmail")
                                .ok_or_else(|| Error::new("mailbox.webmail"))?,
                            &json!(url),
                        )
                        .is_ok() =>
                    {
                        self.want(&MAILBOX, mb, "webmail", json!(url), &at);
                    }
                    _ => {
                        self.findings.insert(Finding {
                            kind: Kind::Unmapped,
                            collection: Some(ADDRESS.name),
                            pk: Some(pk.clone()),
                            column: Some("webmail_url".to_owned()),
                            at: vec![at],
                        });
                    }
                }
            }
        }
        if let Some((number, at)) = self.agreed(ADDRESS.name, &pk, "phone", read.get("phone")) {
            let phone = if one_number(&number) {
                self.phone(&number, None, &at)?
            } else {
                None
            };
            if let Some(phone) = phone {
                self.want(&ADDRESS, &pk, "phone", json!(phone), &at);
            } else {
                self.refused(&ADDRESS, &pk, "phone", at);
            }
        }
        self.dump_note(&ADDRESS, &pk, read);
        Ok(Some(pk))
    }

    fn dump_phone(&mut self, read: &Read<'_>) -> Result<Option<String>> {
        let Some((number, at, conflicted)) = read.get("phone") else {
            let key = read.at(read.col("phone").unwrap_or(0));
            self.find(Kind::FreeText, Some(PHONE.name), None, vec![key]);
            return Ok(None);
        };
        let label = read
            .get("label")
            .filter(|(text, _, conflicted)| {
                !conflicted && guard::text(text, 64).is_ok() && !text.contains('@')
            })
            .map(|(text, _, _)| text);
        if conflicted {
            return Ok(None);
        }
        let pk = if one_number(&number) {
            self.phone(&number, label.as_deref(), &at)?
        } else {
            None
        };
        let Some(pk) = pk else {
            self.find(Kind::FreeText, Some(PHONE.name), None, vec![at]);
            return Ok(None);
        };
        if self.row_conflict(PHONE.name, &pk, read) {
            return Ok(Some(pk));
        }
        self.dump_phone_label(&pk, read);
        self.dump_phone_links(&pk, &number, read);
        for (column, cell, limit) in [
            ("owner_carrier", "carrier", 64),
            ("line_type", "line_type", 32),
            ("device", "device", 64),
        ] {
            if let Some((value, at)) = self.agreed(PHONE.name, &pk, column, read.get(column)) {
                if guard::text(&value, limit).is_ok() && !value.contains('@') {
                    self.want(&PHONE, &pk, cell, json!(value), &at);
                } else {
                    self.refused(&PHONE, &pk, column, at);
                }
            }
        }
        // A phone fleet_db calls anything but active is the owner's word on its row, which no
        // registry lifecycle word means: its coordinate is kept for the owner.
        if let Some((status, at)) = self.agreed(PHONE.name, &pk, "status", read.get("status"))
            && !status.eq_ignore_ascii_case("active")
        {
            self.refused(&PHONE, &pk, "status", at);
        }
        self.dump_note(&PHONE, &pk, read);
        Ok(Some(pk))
    }

    /// A phone's label: a row's label where no other holds it, in place of one this import made
    /// up, and a finding for what no row can say.
    fn dump_phone_label(&mut self, pk: &str, read: &Read<'_>) {
        let Some((text, at, false)) = read.get("label") else {
            return;
        };
        let said = json!(text);
        let wanted = self
            .row_get(PHONE.name, pk)
            .and_then(|row| row.values.get("label"))
            .map(|want| want.x.clone());
        if wanted.as_ref() == Some(&said) {
            return;
        }
        let usable = guard::text(&text, 64).is_ok()
            && !text.contains('@')
            && !self.minted_labels.contains(&text)
            && !self.current[PHONE.name]
                .rows
                .iter()
                .any(|(other, row)| other != pk && row.get("label") == Some(&said));
        match wanted {
            None if usable => {
                self.minted_labels.insert(text);
                self.want(&PHONE, pk, "label", said, &at);
            }
            // A label this run made up gives way to what the source states of the row.
            Some(_) if usable && self.placeholder_labels.remove(pk) => {
                self.minted_labels.insert(text);
                if let Some(row) = self
                    .rows
                    .get_mut(PHONE.name)
                    .and_then(|table| table.get_mut(pk))
                {
                    row.values.remove("label");
                }
                self.want(&PHONE, pk, "label", said, &at);
            }
            _ => self.uncarried(PHONE.name, pk, "label", at),
        }
    }

    /// A phone's links, which the registry states from the other side (`address.phone`, and the
    /// preferences of the addresses naming it): an element no `emails` row says is a finding.
    fn dump_phone_links(&mut self, pk: &str, number: &str, read: &Read<'_>) {
        let digits = super::phone_digits(number);
        let mut addresses = BTreeSet::new();
        let mut machines = BTreeSet::new();
        if let Some(emails) = sibling(self.input.sheets, read.sheet, Dump::Emails) {
            for cells in &emails.rows {
                if digits.is_some() && super::phone_digits(named(emails, cells, "phone")) == digits
                {
                    addresses.extend(keys::address(named(emails, cells, "email")));
                    machines.extend(
                        items(named(emails, cells, "assigned_computers"))
                            .flat_map(str::split_whitespace)
                            .map(computer_word),
                    );
                }
            }
        }
        if let Some((cell, at, _)) = read.get("linked_emails")
            && items(&cell)
                .flat_map(str::split_whitespace)
                .any(|word| !keys::address(word).is_ok_and(|email| addresses.contains(&email)))
        {
            self.uncarried(PHONE.name, pk, "linked_emails", at);
        }
        if let Some((cell, at, _)) = read.get("linked_computers")
            && items(&cell)
                .flat_map(str::split_whitespace)
                .map(computer_word)
                .any(|word| !self.names_one_of(&word, &machines))
        {
            self.uncarried(PHONE.name, pk, "linked_computers", at);
        }
    }

    /// Whether the machine `word` is one of `machines` by any name a source of this run links to it.
    fn names_one_of(&self, word: &str, machines: &BTreeSet<String>) -> bool {
        self.links
            .of(word)
            .iter()
            .any(|name| machines.contains(name))
    }

    fn dump_account(&mut self, read: &Read<'_>) -> Result<Option<String>> {
        // The address a row signs in with, and the columns that decide its realm, region and flow:
        // one the replica holds concurrent values for decides nothing, and [`Self::disagreements`]
        // names it.
        if read.conflicted("email") {
            return Ok(None);
        }
        let kind = read.sheet.kind;
        let email = read.col("email");
        let Some(address) = self.address_of(read.n, read.sheet, read.row, read.cells, email) else {
            return Ok(None);
        };
        let email_at = read.at(email.unwrap_or(0));
        let Some(address_pk) = self.address(&address, kind, &email_at) else {
            return Ok(None);
        };
        if ["provider", "tier", "notes", "service"]
            .into_iter()
            .any(|column| read.conflicted(column))
        {
            return Ok(None);
        }
        let Some((provider, provider_at, _)) = read.get("provider") else {
            self.find(Kind::FreeText, Some(ACCOUNT.name), None, vec![email_at]);
            return Ok(None);
        };
        let Some(provider) = provider_word(&provider) else {
            self.find(Kind::FreeText, Some(ACCOUNT.name), None, vec![provider_at]);
            return Ok(None);
        };
        let (tier, tier_at) = read
            .get("tier")
            .map_or((String::new(), provider_at.clone()), |(t, at, _)| (t, at));
        let notes = read.get("notes").map(|(t, _, _)| t).unwrap_or_default();
        let (terms, paused) = match self.input.vendors.mark(provider, &tier, &notes) {
            Mark::Absent => {
                if let Some((id, _, _)) = read.get("id") {
                    self.no_account.insert(id);
                }
                // No account row holds this row's note, so its login does, as a sheet's row does,
                // unless a member row of the login repeats it.
                if !self.note_on_a_member_row(read, &address, &notes) {
                    self.dump_note(&ADDRESS, &address_pk, read);
                }
                return Ok(None);
            }
            Mark::Unread => {
                self.unsettled_logins.insert(address_pk.clone());
                self.find(Kind::FreeText, Some(ACCOUNT.name), None, vec![tier_at]);
                return Ok(None);
            }
            Mark::Held => {
                self.hold_kimi(&address, &address_pk);
                self.find(Kind::Held, Some(ACCOUNT.name), None, vec![tier_at]);
                return Ok(None);
            }
            Mark::Member(terms) => (terms, false),
            Mark::Paused(terms) => (terms, true),
        };
        let Some((realm, _)) = provider_realm(provider, terms.region) else {
            self.hold_kimi(&address, &address_pk);
            self.find(Kind::Held, Some(ACCOUNT.name), None, vec![tier_at]);
            return Ok(None);
        };
        let Some(pk) = self.account(&address, &address_pk, realm, &provider_at) else {
            return Ok(None);
        };
        if self.row_conflict(ACCOUNT.name, &pk, read) {
            return Ok(Some(pk));
        }
        // fleet_db's `service` names the product where it is one of the provider's; otherwise
        // the provider's default, and the word a finding.
        let service = match read.get("service") {
            Some((word, at, _)) => match super::service_word(&word) {
                Some((of, service)) if of == provider => Some((service, at)),
                _ => {
                    self.uncarried(ACCOUNT.name, &pk, "service", at);
                    None
                }
            },
            None => None,
        };
        let (service, service_at) =
            service.unwrap_or((column_service(provider), provider_at.clone()));
        self.element(&ACCOUNT, &pk, "services", service, &service_at);
        // An id says nothing of its own only where it is `<service>:<this login>`.
        if let Some((id, at, _)) = read.get("id")
            && !id.split_once(':').is_some_and(|(word, login)| {
                super::service_word(word).is_some_and(|(of, _)| of == provider)
                    && keys::address(login).is_ok_and(|login| login == address)
            })
        {
            self.uncarried(ACCOUNT.name, &pk, "id", at);
        }
        // The mark reads `0.5` and `2.5` as a member: only `1` is all of what its cell says.
        if terms.region.is_none() && terms.flow.is_none() && !matches!(tier.as_str(), "1" | "1.0") {
            self.uncarried(ACCOUNT.name, &pk, "tier", tier_at.clone());
        }
        if let Some(region) = terms.region {
            self.want(&ACCOUNT, &pk, "region", json!(region), &tier_at);
        }
        if let Some(flow) = terms.flow {
            self.want(&ACCOUNT, &pk, "flow", json!(flow), &tier_at);
        }
        // fleet_db calls every row it did not suspend `active`, which says no more than the row's
        // being there, unless the registry holds a word it would take back; any other word of the
        // registry's is the row's status.
        match self
            .agreed(ACCOUNT.name, &pk, "status", read.get("status"))
            .map(|(word, at)| (word.to_ascii_lowercase(), at))
        {
            Some((word, at)) if word == "suspended" => {
                // fleet_db's bootstrap writes `suspended` on every vendor of a row whose note says
                // `deactivated` or `disabled`, whichever vendor it names: where this vendor's mark
                // reads a member, the word restates the note, so it is reported and pauses no one.
                let lower = notes.to_ascii_lowercase();
                if !paused && (lower.contains("deactivated") || lower.contains("disabled")) {
                    self.uncarried(ACCOUNT.name, &pk, "status", at);
                    self.birth_status(&pk, &provider_at);
                } else {
                    self.dump_paused(&pk, at);
                }
            }
            Some((word, at))
                if word != "active"
                    && super::super::registry::ACCOUNT_STATUS.contains(&word.as_str()) =>
            {
                self.want(&ACCOUNT, &pk, "status", json!(word), &at);
            }
            stated => {
                let held = self.held(&ACCOUNT, &pk, "status").and_then(Value::as_str);
                let takes_back =
                    !paused && held.is_some_and(|held| !matches!(held, "active" | "pending"));
                if let Some((word, at)) = stated
                    && (word != "active" || takes_back)
                {
                    self.uncarried(ACCOUNT.name, &pk, "status", at);
                }
                if paused {
                    self.dump_paused(&pk, tier_at.clone());
                } else {
                    self.birth_status(&pk, &provider_at);
                }
            }
        }
        if let Some((receiving, at)) = self.agreed(
            ADDRESS.name,
            &address_pk,
            "receiving_mailbox",
            read.get("receiving_mailbox"),
        ) {
            match keys::address(&receiving) {
                Ok(receiving) if receiving == address => {}
                // A protected row holds no mailbox, so only its link to its root can say this: the
                // link this run wants or the registry holds, to the row that address names.
                Ok(receiving) if self.guarded(&address_pk) => {
                    let root = self.address_key(&receiving).map(|root| json!(root));
                    let link = self
                        .row_get(ADDRESS.name, &address_pk)
                        .and_then(|row| row.values.get("alias_of"))
                        .map(|want| want.x.clone())
                        .or_else(|| self.held(&ADDRESS, &address_pk, "alias_of").cloned());
                    if root.is_none() || link != root {
                        self.uncarried(ADDRESS.name, &address_pk, "receiving_mailbox", at);
                    }
                }
                Ok(receiving) => {
                    if let Some(mb) = self.mailbox(&receiving, kind, &at)? {
                        self.want(&ADDRESS, &address_pk, "mailbox", json!(mb), &at);
                    } else if self
                        .address_key(&receiving)
                        .is_some_and(|login| self.guarded(&login))
                    {
                        // A protected address is no mailbox's login: no cell can say this.
                        self.uncarried(ADDRESS.name, &address_pk, "receiving_mailbox", at);
                    }
                }
                Err(_) => self.uncarried(ADDRESS.name, &address_pk, "receiving_mailbox", at),
            }
        }
        if let Some((labels, at)) = self.agreed(
            ACCOUNT.name,
            &pk,
            "assigned_computers",
            read.get("assigned_computers"),
        ) {
            for word in items(&labels).flat_map(str::split_whitespace) {
                let Ok(label) = Label::parse(word) else {
                    self.find(
                        Kind::FreeText,
                        Some(super::PREFERENCE.name),
                        None,
                        vec![at.clone()],
                    );
                    self.unsettled.insert(pk.clone());
                    continue;
                };
                let Some(machine) = self.machine_for(label, &at) else {
                    self.unsettled.insert(pk.clone());
                    continue;
                };
                self.prefer(&machine, &pk, None, &at, kind);
            }
        }
        self.dump_note(&ACCOUNT, &pk, read);
        Ok(Some(pk))
    }

    /// An account imported `paused`: by the replica's word, or by the vendor's mark at `at`.
    /// Whether a row of this table holding an account for `address` states `notes` too.
    fn note_on_a_member_row(&self, read: &Read<'_>, address: &str, notes: &str) -> bool {
        let sheet = read.sheet;
        sheet.rows.iter().any(|cells| {
            named(sheet, cells, "notes") == notes
                && keys::address(named(sheet, cells, "email")).is_ok_and(|email| email == address)
                && provider_word(named(sheet, cells, "provider")).is_some_and(|provider| {
                    matches!(
                        self.input
                            .vendors
                            .mark(provider, named(sheet, cells, "tier"), notes),
                        Mark::Member(_) | Mark::Paused(_)
                    )
                })
        })
    }

    fn dump_paused(&mut self, pk: &str, at: Coord) {
        self.want(&ACCOUNT, pk, "status", json!("paused"), &at);
        self.findings.insert(Finding {
            kind: Kind::Paused,
            collection: Some(ACCOUNT.name),
            pk: Some(pk.to_owned()),
            column: None,
            at: vec![at],
        });
    }

    fn dump_computer(&mut self, read: &Read<'_>) -> Option<String> {
        let kind = read.sheet.kind;
        let Some((raw, at, _)) = read.get("label") else {
            let key = read.at(read.col("label").unwrap_or(0));
            self.find(Kind::FreeText, Some(MACHINE.name), None, vec![key]);
            return None;
        };
        let Ok(label) = Label::parse(&raw) else {
            self.find(Kind::FreeText, Some(MACHINE.name), None, vec![at]);
            return None;
        };
        let node = names::stated_node(read.get("node_id"));
        let host = names::stated_host(read.get("hostname"), raw.trim());
        // Nothing of a row the replica holds a concurrent delete for is imported, its name included.
        let pk = if read.row_conflicted() {
            self.machine_key_at(label, node.as_deref(), host.as_deref())
        } else {
            self.machine_at(label, node.as_deref(), host.as_deref(), &at)
        };
        let Some(pk) = pk else {
            self.unsettle_assigned(read);
            return None;
        };
        if self.row_conflict(MACHINE.name, &pk, read) {
            return Some(pk);
        }
        // Read, so what an earlier import wrote and this row no longer says is weighed.
        self.row(&MACHINE, &pk, &at);
        for (column, cell, limit) in [
            ("make", "make", 32),
            ("arch", "arch", 16),
            ("os", "os", 32),
            ("chip", "chip", 32),
            ("nickname", "nickname", 64),
            ("location", "location", 64),
        ] {
            if let Some((value, at)) = self.agreed(MACHINE.name, &pk, column, read.get(column)) {
                if guard::text(&value, limit).is_ok() && !value.contains('@') {
                    self.want(&MACHINE, &pk, cell, json!(value), &at);
                } else {
                    self.refused(&MACHINE, &pk, column, at);
                }
            }
        }
        for (column, cell) in [("memory_gb", "ram_gb"), ("disk_gb", "disk_gb")] {
            if let Some((value, at)) = self.agreed(MACHINE.name, &pk, column, read.get(column)) {
                match value.parse::<u64>() {
                    Ok(number) => self.want(&MACHINE, &pk, cell, json!(number), &at),
                    Err(_) => self.refused(&MACHINE, &pk, column, at),
                }
            }
        }
        let mut nick_aliases: Vec<String> = Vec::new();
        let mut nick_at = None;
        if let Some((value, at)) = self.agreed(MACHINE.name, &pk, "aliases", read.get("aliases")) {
            for part in items(&value) {
                match names::alias_name(part) {
                    Some(alias) => self.alias_element(&pk, &alias, &at),
                    None if guard::text(part, 64).is_ok() && !part.contains('@') => {
                        nick_aliases.push(part.to_owned());
                        nick_at.get_or_insert_with(|| at.clone());
                    }
                    None => self.refused(&MACHINE, &pk, "aliases", at.clone()),
                }
            }
        }
        if let Some((value, at)) = self.agreed(
            MACHINE.name,
            &pk,
            "nickname_aliases",
            read.get("nickname_aliases"),
        ) {
            for part in items(&value) {
                if nick_aliases.iter().any(|s| s == part) {
                    continue;
                }
                if guard::text(part, 64).is_ok() && !part.contains('@') {
                    nick_aliases.push(part.to_owned());
                    nick_at.get_or_insert_with(|| at.clone());
                } else {
                    self.refused(&MACHINE, &pk, "nickname_aliases", at.clone());
                }
            }
        }
        // One cell made of both columns: a disagreement on either leaves it as it is.
        if let Some(at) =
            nick_at.filter(|_| !read.conflicted("aliases") && !read.conflicted("nickname_aliases"))
        {
            if nick_aliases.len() > 8 {
                self.refused(&MACHINE, &pk, "nickname_aliases", at.clone());
            }
            nick_aliases.truncate(8);
            self.want(&MACHINE, &pk, "nickname_aliases", json!(nick_aliases), &at);
        }
        if let Some((node, at)) = self.agreed(MACHINE.name, &pk, "node_id", read.get("node_id")) {
            let node = node.to_ascii_lowercase();
            if hex::is_lower(&node, 32) {
                self.want(&MACHINE, &pk, "fleet_node", json!(node), &at);
            } else if self.held(&MACHINE, &pk, "node").and_then(Value::as_str)
                != Some(node.strip_prefix("n-").unwrap_or(&node))
            {
                // A mesh id is the Mac's own to publish (`register::own_node`), so a row that
                // says it covers it and no import writes it.
                self.refused(&MACHINE, &pk, "node_id", at);
            }
        }
        // fleet_db wrote `<label>.local` for every row: a host name is carried only where the
        // owner gave another.
        if let Some((host, at)) = self.agreed(MACHINE.name, &pk, "hostname", read.get("hostname"))
            && names::host_given(&host, raw.trim())
        {
            if guard::text(&host, 64).is_ok() && !host.contains('@') {
                if let Some(hosts) = self.hosts_cell(&pk, &host, &at) {
                    self.want(&MACHINE, &pk, "hosts", hosts, &at);
                }
            } else {
                self.refused(&MACHINE, &pk, "hostname", at);
            }
        }
        if let Some((status, at)) = self.agreed(MACHINE.name, &pk, "status", read.get("status")) {
            let word = status.to_ascii_lowercase();
            if super::super::registry::MACHINE_STATUS.contains(&word.as_str()) {
                self.statuses.push((pk.clone(), word, at));
            } else {
                self.refused(&MACHINE, &pk, "status", at);
            }
        }
        let mut accounted = BTreeSet::new();
        if let Some((ids, at)) = self.agreed(
            MACHINE.name,
            &pk,
            "assigned_accounts",
            read.get("assigned_accounts"),
        ) {
            for id in items(&ids).flat_map(str::split_whitespace) {
                let Some((service, email)) = id.split_once(':') else {
                    self.find(
                        Kind::NoAccount,
                        Some(super::PREFERENCE.name),
                        None,
                        vec![at.clone()],
                    );
                    continue;
                };
                if let Ok(address) = keys::address(email) {
                    accounted.insert(address);
                }
                // The row it names is read as holding no account, so no preference is owed,
                // whichever source makes that account, and however early in the run.
                if self.no_account.contains(id) {
                    continue;
                }
                let account = assigned_realms(service)
                    .iter()
                    .filter_map(|realm| keys::account_pk(realm, email).ok())
                    .find(|account| {
                        self.has_row(ACCOUNT.name, account)
                            || self.current[ACCOUNT.name].rows.contains_key(account)
                    });
                match account {
                    Some(account) => self.prefer(&pk, &account, None, &at, kind),
                    None => self.find(
                        Kind::NoAccount,
                        Some(super::PREFERENCE.name),
                        None,
                        vec![at.clone()],
                    ),
                }
            }
        }
        // An address assigned here that no assigned account names says what no row carries.
        if let Some((emails, at, _)) = read.get("assigned_emails")
            && items(&emails)
                .flat_map(str::split_whitespace)
                .any(|email| !keys::address(email).is_ok_and(|a| accounted.contains(&a)))
        {
            self.uncarried(MACHINE.name, &pk, "assigned_emails", at);
        }
        self.dump_note(&MACHINE, &pk, read);
        Some(pk)
    }
}

/// The webmail fleet_db wrote by default for a provider word: carried only where it differs.
fn default_webmail(provider: &str) -> &'static str {
    if provider.eq_ignore_ascii_case("proton") || provider.eq_ignore_ascii_case("protonmail") {
        "https://mail.proton.me/u/0/inbox"
    } else if provider.eq_ignore_ascii_case("gmail") || provider.eq_ignore_ascii_case("google") {
        "https://mail.google.com/"
    } else if provider.eq_ignore_ascii_case("outlook") || provider.eq_ignore_ascii_case("hotmail") {
        "https://outlook.live.com/mail/"
    } else if provider.eq_ignore_ascii_case("privateemail") {
        "https://privateemail.com/"
    } else {
        ""
    }
}
