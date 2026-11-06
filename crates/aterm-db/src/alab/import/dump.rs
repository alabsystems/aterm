// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A fleet_db replica's dump as import sources (docs/DESIGN-alab-one-mesh-2026-11-05.md §4.7,
//! §8 step 2): the JSON document `aterm db dump <file> --show-passwords` writes (fleet_db's
//! `dump_all`: `computers`, `emails`, `phones`, `ai_accounts`, each an array of row objects),
//! read here from its bytes alone, so nothing of fleet_db's code is needed once it is deleted.
//!
//! Each table becomes one [`Sheet`] of kind [`SheetKind::FleetDb`], its own source
//! (`<file>#<table>`), whose header row is every column the table's rows hold: the known ones in
//! [`DUMP_TABLES`] order, then any other in name order, so **every column is carried or
//! reported**. [`DUMP_TABLES`] says where each known column goes: a registry cell, a `Secret`
//! cell, a relation the registry already states another way, the row's key, or nowhere by the
//! design's word (`ip`: addresses come from presence). A column it does not name is an
//! [`Kind::Unmapped`] finding, by name only. A password never enters the sheet's cells: it is
//! held as a [`Secret`] at its coordinate ([`Sheet::secrets`]), so no review, finding or log
//! shows it. A row the replica holds concurrent values for (`_conflicts`) imports only the
//! columns its values agree on; each other is a [`Kind::DumpConflicted`] finding.
//!
//! A dump holds passwords, so its source key and `sha256` are a keyed digest ([`super::keyed_source`],
//! under the newest `[everyone]` epoch's key), never a plain hash a reader of the shared `source`
//! row could test guesses against; a dump that differs only in a password is still a new source.

use super::{
    ACCOUNT, ADDRESS, Coord, Dump, Finding, Kind, MACHINE, MAILBOX, Mark, Mode, PHONE, Planner,
    SheetKind, coord, provider_realm,
};
use super::{Sheet, column_service};
use crate::{
    Error, Result,
    machines::Label,
    typed::{
        guard, keys,
        secret::{Secret, cell_as_compared},
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
        // Every column any row holds, the known first: what a read adds (`has_password`,
        // `_conflicts`) is not a column.
        let mut headers: Vec<String> = columns.iter().map(|(c, _)| (*c).to_owned()).collect();
        let mut others = BTreeSet::new();
        for item in items {
            let row = item.as_object().ok_or_else(malformed)?;
            for key in row.keys() {
                if !key.starts_with('_')
                    && key != "has_password"
                    && !headers.iter().any(|h| h == key)
                {
                    others.insert(key.clone());
                }
            }
        }
        headers.extend(others);
        let mut rows = Vec::new();
        let mut secrets = BTreeMap::new();
        let mut conflicted = BTreeSet::new();
        for (index, item) in items.iter().enumerate() {
            let row_at = index + 2;
            let row = item.as_object().ok_or_else(malformed)?;
            let mut cells = Vec::with_capacity(headers.len());
            for (col, header) in headers.iter().enumerate() {
                let value = row.get(header).unwrap_or(&Value::Null);
                if secret_column(table, header) {
                    if let Some(secret) = value.as_str().filter(|s| !s.trim().is_empty()) {
                        secrets.insert((row_at, col), Arc::new(Secret::new(secret.to_owned())));
                    }
                    cells.push(String::new());
                } else {
                    cells.push(text(value));
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
                    let disagree = if secret_column(table, header) {
                        cell_as_compared(row.get(header)) != cell_as_compared(other.get(header))
                    } else {
                        row.get(header) != other.get(header)
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
                    column: guard::text(header, 64)
                        .is_ok()
                        .then(|| header.clone())
                        .filter(|h| !h.contains('@')),
                    at: Vec::new(),
                });
            }
        }
        for (index, cells) in sheet.rows.iter().enumerate() {
            let read = Read {
                n,
                sheet,
                row: index + 2,
                cells,
            };
            if cells.iter().all(|c| c.trim().is_empty())
                && !sheet.secrets.keys().any(|(r, _)| *r == read.row)
            {
                continue;
            }
            match table {
                Dump::Emails => self.dump_email(&read)?,
                Dump::Phones => self.dump_phone(&read)?,
                Dump::Accounts => self.dump_account(&read)?,
                Dump::Computers => self.dump_computer(&read),
            }
        }
        Ok(())
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

    /// Whether this dump's `ai_accounts` table names `address`: the relations an `emails` row
    /// states are carried by that row.
    fn dump_names_account(&self, sheet: &Sheet, address: &str) -> bool {
        let file = sheet.name.rsplit_once('#').map_or("", |(file, _)| file);
        self.input.sheets.iter().any(|other| {
            other.kind == SheetKind::FleetDb(Dump::Accounts)
                && other.name.rsplit_once('#').map(|(f, _)| f) == Some(file)
                && other
                    .headers
                    .iter()
                    .position(|h| h == "email")
                    .is_some_and(|col| {
                        other.rows.iter().any(|cells| {
                            cells
                                .get(col)
                                .and_then(|c| keys::address(c).ok())
                                .is_some_and(|a| a == address)
                        })
                    })
        })
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

    fn dump_email(&mut self, read: &Read<'_>) -> Result<()> {
        let kind = read.sheet.kind;
        let email = read.col("email");
        let Some(address) = self.address_of(read.n, read.sheet, read.row, read.cells, email) else {
            return Ok(());
        };
        let email_at = read.at(email.unwrap_or(0));
        let Some(pk) = self.address(&address, kind, &email_at) else {
            return Ok(());
        };
        if self.row_conflict(ADDRESS.name, &pk, read) {
            return Ok(());
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
            if read.get("protected").is_none() {
                self.agreed(
                    ADDRESS.name,
                    &pk,
                    "protected",
                    Some((String::new(), email_at.clone(), true)),
                );
            }
            self.protected.insert(pk.clone());
        }
        for column in ["assigned_computers", "services"] {
            if let Some((_, at, _)) = read.get(column)
                && !self.dump_names_account(read.sheet, &address)
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
        let alias = read
            .get("account_type")
            .is_some_and(|(word, _, _)| word.eq_ignore_ascii_case("alias"));
        let root = self
            .agreed(ADDRESS.name, &pk, "root_account", read.get("root_account"))
            .and_then(|(text, at)| Some((keys::address(&text).ok()?, at)))
            .filter(|(root, _)| *root != address);
        let mut mailbox = None;
        match (alias, root) {
            (true, Some((root, at))) => {
                self.corrected.insert(pk.clone());
                if let Some(root_pk) = self.address(&root, kind, &at) {
                    self.link_alias(&pk, &root_pk, &at);
                }
                if !protected {
                    let mb = self.mailbox(&root, kind, &at)?;
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
            } else if protected {
                self.findings.insert(Finding {
                    kind: Kind::ProtectedSecret,
                    collection: Some(ADDRESS.name),
                    pk: Some(pk.clone()),
                    column: Some("password".to_owned()),
                    at: vec![at],
                });
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
                let mb = self.mailbox(&address, kind, &at)?;
                self.want(&ADDRESS, &pk, "mailbox", json!(mb), &at);
                mailbox = Some(mb);
            }
        } else if read.conflicted("password") {
            // A concurrent value claims what this row's own empty cell does not.
            self.agreed(
                ADDRESS.name,
                &pk,
                "password",
                Some((String::new(), email_at.clone(), true)),
            );
        }
        if let Some((url, at)) =
            self.agreed(ADDRESS.name, &pk, "webmail_url", read.get("webmail_url"))
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
        Ok(())
    }

    fn dump_phone(&mut self, read: &Read<'_>) -> Result<()> {
        let Some((number, at, conflicted)) = read.get("phone") else {
            return Ok(());
        };
        let label = read
            .get("label")
            .filter(|(text, _, conflicted)| {
                !conflicted && guard::text(text, 64).is_ok() && !text.contains('@')
            })
            .map(|(text, _, _)| text);
        if conflicted {
            self.findings.insert(Finding {
                kind: Kind::DumpConflicted,
                collection: Some(PHONE.name),
                pk: None,
                column: Some("number".to_owned()),
                at: vec![at],
            });
            return Ok(());
        }
        let Some(pk) = self.phone(&number, label.as_deref(), &at)? else {
            self.find(Kind::FreeText, Some(PHONE.name), None, vec![at]);
            return Ok(());
        };
        if self.row_conflict(PHONE.name, &pk, read) {
            return Ok(());
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
        Ok(())
    }

    fn dump_account(&mut self, read: &Read<'_>) -> Result<()> {
        let kind = read.sheet.kind;
        let email = read.col("email");
        let Some(address) = self.address_of(read.n, read.sheet, read.row, read.cells, email) else {
            return Ok(());
        };
        let email_at = read.at(email.unwrap_or(0));
        let Some(address_pk) = self.address(&address, kind, &email_at) else {
            return Ok(());
        };
        // The columns that decide the account's realm, region and flow: one the replica holds
        // concurrent values for decides nothing.
        if let Some(column) = ["provider", "tier", "notes", "service"]
            .into_iter()
            .find(|column| read.conflicted(column))
        {
            self.findings.insert(Finding {
                kind: Kind::DumpConflicted,
                collection: Some(ACCOUNT.name),
                pk: None,
                column: Some(column.to_owned()),
                at: vec![read.get(column).map_or(email_at, |(_, at, _)| at)],
            });
            return Ok(());
        }
        let Some((provider, provider_at, _)) = read.get("provider") else {
            self.find(Kind::FreeText, Some(ACCOUNT.name), None, vec![email_at]);
            return Ok(());
        };
        let Some(provider) = provider_word(&provider) else {
            self.find(Kind::FreeText, Some(ACCOUNT.name), None, vec![provider_at]);
            return Ok(());
        };
        let (tier, tier_at) = read
            .get("tier")
            .map_or((String::new(), provider_at.clone()), |(t, at, _)| (t, at));
        let notes = read.get("notes").map(|(t, _, _)| t).unwrap_or_default();
        let (terms, paused) = match self.input.vendors.mark(provider, &tier, &notes) {
            Mark::Absent => return Ok(()),
            Mark::Unread => {
                self.unsettled_logins.insert(address_pk.clone());
                self.find(Kind::FreeText, Some(ACCOUNT.name), None, vec![tier_at]);
                return Ok(());
            }
            Mark::Held => {
                self.hold_kimi(&address);
                self.find(Kind::Held, Some(ACCOUNT.name), None, vec![tier_at]);
                return Ok(());
            }
            Mark::Member(terms) => (terms, false),
            Mark::Paused(terms) => (terms, true),
        };
        let Some((realm, _)) = provider_realm(provider, terms.region) else {
            self.hold_kimi(&address);
            self.find(Kind::Held, Some(ACCOUNT.name), None, vec![tier_at]);
            return Ok(());
        };
        let Some(pk) = self.account(&address, &address_pk, realm, &provider_at) else {
            return Ok(());
        };
        if self.row_conflict(ACCOUNT.name, &pk, read) {
            return Ok(());
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
        ) && let Ok(receiving) = keys::address(&receiving)
            && receiving != address
            && !self.protected.contains(&address_pk)
        {
            let mb = self.mailbox(&receiving, kind, &at)?;
            self.want(&ADDRESS, &address_pk, "mailbox", json!(mb), &at);
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
        Ok(())
    }

    fn dump_computer(&mut self, read: &Read<'_>) {
        let kind = read.sheet.kind;
        let Some((raw, at, _)) = read.get("label") else {
            return;
        };
        let Ok(label) = Label::parse(&raw) else {
            self.find(Kind::FreeText, Some(MACHINE.name), None, vec![at]);
            return;
        };
        let Some(pk) = self.machine_for(label, &at) else {
            return;
        };
        if self.row_conflict(MACHINE.name, &pk, read) {
            return;
        }
        // A placeholder minted this run stays `planned`: fleet_db wrote `active` for every row.
        let placeholder = self
            .rows
            .get(&(MACHINE.name, pk.clone()))
            .and_then(|row| row.values.get("status"))
            .is_some_and(|want| want.x == json!("planned"));
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
                match Label::parse(part).or_else(|_| Label::parse(&part.replace('.', "-"))) {
                    Ok(alias) if alias.canonical() != label.canonical() => {
                        self.element(&MACHINE, &pk, "aliases", &alias.canonical(), &at);
                    }
                    Ok(_) => {}
                    Err(_) if guard::text(part, 64).is_ok() && !part.contains('@') => {
                        nick_aliases.push(part.to_owned());
                        nick_at.get_or_insert_with(|| at.clone());
                    }
                    Err(_) => self.note(&MACHINE, &pk, &at),
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
        if let Some(at) = nick_at {
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
            && host.strip_suffix(".local").is_none_or(|stem| {
                stem.bytes().any(|b| b.is_ascii_uppercase())
                    || !stem.eq_ignore_ascii_case(raw.trim())
            })
        {
            if guard::text(&host, 64).is_ok() && !host.contains('@') {
                self.want(&MACHINE, &pk, "hosts", json!([host]), &at);
            } else {
                self.note(&MACHINE, &pk, &at);
            }
        }
        if !placeholder
            && let Some((status, at)) = self.agreed(MACHINE.name, &pk, "status", read.get("status"))
        {
            let word = status.to_ascii_lowercase();
            if super::super::registry::MACHINE_STATUS.contains(&word.as_str()) {
                self.want(&MACHINE, &pk, "status", json!(word), &at);
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
                let realms: &[&str] = match service {
                    "claude" => &["claude"],
                    "codex" => &["codex"],
                    "gemini" => &["google"],
                    "meta" => &["meta"],
                    "grok" => &["grok"],
                    "kimi" => &["kimi:global", "kimi:mainland-cn"],
                    _ => &[],
                };
                let account = realms
                    .iter()
                    .filter_map(|realm| keys::account_pk(realm, email).ok())
                    .find(|account| {
                        self.rows.contains_key(&(ACCOUNT.name, account.clone()))
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
