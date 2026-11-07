// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A fleet_db replica's dump as import sources (docs/DESIGN-alab-one-mesh-2026-11-05.md §4.7,
//! §8 step 2): the JSON document `aterm db dump <file> --show-passwords` writes (fleet_db's
//! `dump_all`: `computers`, `emails`, `phones`, `ai_accounts`, each an array of row objects),
//! read here from its bytes alone, so nothing of fleet_db's code is needed once it is deleted.
//!
//! Each table becomes one [`Sheet`] of kind [`SheetKind::FleetDb`], its own source
//! (`<file>#<table>`), whose header row is every column the table's rows or their concurrent values
//! hold: the known ones in
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
//! [`Kind::DumpConflicted`] finding, whatever the row's own cell holds.
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

/// The label a dump source's keyed digest derives its key under.
const SOURCE_DIGEST: &[u8] = b"aterm/alab/dump-source-digest/1\0";

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
        let canonical = zeroize::Zeroizing::new(
            aterm_json::to_vec(&json!({"file": file, "rows": items})).map_err(|_| malformed())?,
        );
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
        let account_addresses = if table == Dump::Emails {
            self.dump_account_addresses(sheet)
        } else {
            BTreeSet::new()
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
                Dump::Emails => self.dump_email(&read, &account_addresses)?,
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

    /// The addresses this dump's `ai_accounts` table names: the relations an `emails` row
    /// states are carried by that row.
    fn dump_account_addresses(&self, sheet: &Sheet) -> BTreeSet<String> {
        let file = sheet.name.rsplit_once('#').map_or("", |(file, _)| file);
        let mut addresses = BTreeSet::new();
        for other in self.input.sheets {
            if other.kind == SheetKind::FleetDb(Dump::Accounts)
                && other.name.rsplit_once('#').map(|(f, _)| f) == Some(file)
                && let Some(col) = other.headers.iter().position(|h| h == "email")
            {
                for cells in &other.rows {
                    if let Some(addr) = cells.get(col).and_then(|c| keys::address(c).ok()) {
                        addresses.insert(addr);
                    }
                }
            }
        }
        addresses
    }

    /// A relation column no row of the dump carries: a finding, by name only.
    fn uncarried(&mut self, collection: &'static str, pk: &str, column: &str, at: Coord) {
        self.findings.insert(Finding {
            kind: Kind::Unmapped,
            collection: Some(collection),
            pk: Some(pk.to_owned()),
            column: Some(column.to_owned()),
            at: vec![at],
        });
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
        account_addresses: &BTreeSet<String>,
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
            && (value.eq_ignore_ascii_case("true")
                || value == "1"
                || value.eq_ignore_ascii_case("yes"))
        {
            self.protect(&pk, &at);
        }
        // A flag the replica holds concurrent values for guards the row, as a conflicted
        // registry cell does, and is not written.
        if read.conflicted("protected") {
            self.protected.insert(pk.clone());
        }
        for column in ["assigned_computers", "services"] {
            if let Some((_, at, _)) = read.get(column)
                && !account_addresses.contains(&address)
            {
                self.uncarried(ADDRESS.name, &pk, column, at);
            }
        }
        let protected = self.protected.contains(&pk);
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
            .and_then(|(text, at)| Some((keys::address(&text).ok()?, at)))
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
            // An alias whose root is no other address says nothing the registry can hold.
            (true, None) => {}
            (false, _) => {
                if let Some((_, at, false)) = read.get("account_type") {
                    self.want(&ADDRESS, &pk, "kind", json!("standalone"), &at);
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
            if let Some(phone) = self.phone(&number, None, &at)? {
                self.want(&ADDRESS, &pk, "phone", json!(phone), &at);
            } else {
                self.note(&ADDRESS, &pk, &at);
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
        let Some(pk) = self.phone(&number, label.as_deref(), &at)? else {
            self.find(Kind::FreeText, Some(PHONE.name), None, vec![at]);
            return Ok(None);
        };
        if self.row_conflict(PHONE.name, &pk, read) {
            return Ok(Some(pk));
        }
        for (column, cell, limit) in [
            ("owner_carrier", "carrier", 64),
            ("line_type", "line_type", 32),
            ("device", "device", 64),
        ] {
            if let Some((value, at)) = self.agreed(PHONE.name, &pk, column, read.get(column)) {
                if guard::text(&value, limit).is_ok() && !value.contains('@') {
                    self.want(&PHONE, &pk, cell, json!(value), &at);
                } else {
                    self.note(&PHONE, &pk, &at);
                }
            }
        }
        // A phone fleet_db calls anything but active is the owner's word on its row, which no
        // registry lifecycle word means: its coordinate is kept for the owner.
        if let Some((status, at)) = self.agreed(PHONE.name, &pk, "status", read.get("status"))
            && !status.eq_ignore_ascii_case("active")
        {
            self.note(&PHONE, &pk, &at);
            self.findings.insert(Finding {
                kind: Kind::Unmapped,
                collection: Some(PHONE.name),
                pk: Some(pk.clone()),
                column: Some("status".to_owned()),
                at: vec![at],
            });
        }
        self.dump_note(&PHONE, &pk, read);
        Ok(Some(pk))
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
            Mark::Absent => return Ok(None),
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
        if let Some(region) = terms.region {
            self.want(&ACCOUNT, &pk, "region", json!(region), &tier_at);
        }
        if let Some(flow) = terms.flow {
            self.want(&ACCOUNT, &pk, "flow", json!(flow), &tier_at);
        }
        let suspended = self
            .agreed(ACCOUNT.name, &pk, "status", read.get("status"))
            .filter(|(status, _)| status.eq_ignore_ascii_case("suspended"));
        if paused || suspended.is_some() {
            let at = suspended.map_or(tier_at.clone(), |(_, at)| at);
            self.want(&ACCOUNT, &pk, "status", json!("paused"), &at);
            self.findings.insert(Finding {
                kind: Kind::Paused,
                collection: Some(ACCOUNT.name),
                pk: Some(pk.clone()),
                column: None,
                at: vec![at],
            });
        } else {
            self.birth_status(&pk, &provider_at);
        }
        if let Some((receiving, at)) = self.agreed(
            ADDRESS.name,
            &address_pk,
            "receiving_mailbox",
            read.get("receiving_mailbox"),
        ) {
            match keys::address(&receiving) {
                Ok(receiving) if receiving == address => {}
                // A protected row holds no mailbox, so only its link to its root can say this.
                Ok(receiving) if self.protected.contains(&address_pk) => {
                    let root = keys::address_pk(&receiving).ok();
                    let linked = self
                        .row_get(ADDRESS.name, &address_pk)
                        .and_then(|row| row.values.get("alias_of"))
                        .is_some_and(|want| root.is_some_and(|root| want.x == json!(root)));
                    if !linked {
                        self.uncarried(ADDRESS.name, &address_pk, "receiving_mailbox", at);
                    }
                }
                Ok(receiving) => {
                    if let Some(mb) = self.mailbox(&receiving, kind, &at)? {
                        self.want(&ADDRESS, &address_pk, "mailbox", json!(mb), &at);
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
                    self.note(&MACHINE, &pk, &at);
                }
            }
        }
        for (column, cell) in [("memory_gb", "ram_gb"), ("disk_gb", "disk_gb")] {
            if let Some((value, at)) = self.agreed(MACHINE.name, &pk, column, read.get(column)) {
                match value.parse::<u64>() {
                    Ok(number) => self.want(&MACHINE, &pk, cell, json!(number), &at),
                    Err(_) => self.note(&MACHINE, &pk, &at),
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
                    None => self.note(&MACHINE, &pk, &at),
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
                if guard::text(part, 64).is_ok()
                    && !part.contains('@')
                    && !nick_aliases.iter().any(|s| s == part)
                {
                    nick_aliases.push(part.to_owned());
                    nick_at.get_or_insert_with(|| at.clone());
                }
            }
        }
        // One cell made of both columns: a disagreement on either leaves it as it is.
        if let Some(at) =
            nick_at.filter(|_| !read.conflicted("aliases") && !read.conflicted("nickname_aliases"))
        {
            nick_aliases.truncate(8);
            self.want(&MACHINE, &pk, "nickname_aliases", json!(nick_aliases), &at);
        }
        if let Some((node, at)) = self.agreed(MACHINE.name, &pk, "node_id", read.get("node_id")) {
            let node = node.to_ascii_lowercase();
            if hex::is_lower(&node, 32) {
                self.want(&MACHINE, &pk, "fleet_node", json!(node), &at);
            } else {
                self.note(&MACHINE, &pk, &at);
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
                self.note(&MACHINE, &pk, &at);
            }
        }
        if let Some((status, at)) = self.agreed(MACHINE.name, &pk, "status", read.get("status")) {
            let word = status.to_ascii_lowercase();
            if super::super::registry::MACHINE_STATUS.contains(&word.as_str()) {
                self.statuses.push((pk.clone(), word, at));
            } else {
                self.note(&MACHINE, &pk, &at);
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
                .any(|email| keys::address(email).is_ok_and(|a| !accounted.contains(&a)))
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
