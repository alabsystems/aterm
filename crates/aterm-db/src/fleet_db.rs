// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Distributed relational database for the 4 canonical lab tables (`computers`, `emails`,
//! `phones`, `ai_accounts`) backed directly by `astream` (`BrokerLog` + `astream_wire::sync`
//! causal operations) and replicated peer-to-peer over `aterm-net` (`FleetOp::Database`).
//!
//! Every fleet machine initializes and opens `~/.aterm/lab-db/operations.astream` directly
//! using the private `fleet.key` that `aterm fleet init|join` wrote, with no Commons audience
//! gate, no designated admin machine, and no external CSV/JSON side files. Since every member
//! can derive the dataset's root key, the dataset has no authority stream: a control record or a
//! permanent stop has no effect, and a fork withdraws only that writer's records from the fork
//! on. Only email rows marked `protected: true` forbid storing a password, enforced at both local
//! commit and remote `Command::decode` ingestion.
//!
//! `docs/DESIGN-alab-one-mesh-2026-11-05.md` supersedes this module: its import reads each replica
//! in place through [`dump_all`], with no dump file, and A13 deletes the module. That read keeps
//! what a replica says about a password, a sheet's has-password word or an earlier build's
//! redaction mark included, so the import can judge it against the registry. So two things are decided against here, as that design provides them:
//! replication that starts by itself (`aterm db serve` and `sync` stay
//! manual), and a way past the quotas `tables` reports (a fleet key rotation and `aterm db rekey`
//! is the only fresh start). A user-marked `protected` is sticky on the write paths here, while
//! decoding is stateless, so a member's own tooling can still clear it. That member already holds
//! every key: the mark guards against mistakes, and the three built-in addresses are refused at
//! decode on every replica.

pub use crate::database::legacy::{is_always_protected_email, storage_key, validate_email_record};
use crate::database::legacy::{sensitive_field, with_normalized_phone_key};
use crate::{
    Error, Result, crypto,
    database::{
        Cell, Change, Command, MAX_RECORDS, MAX_WRITER_BYTES, MAX_WRITER_OPERATIONS, MAX_WRITES,
        View,
    },
    local::{self, Database},
    network::{self, while_busy},
};
use aterm_codec::hex;
#[cfg(test)]
use aterm_digest::Sha256;
use aterm_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write as _,
    net::SocketAddr,
    path::{Path, PathBuf},
};
use zeroize::{Zeroize, Zeroizing};

/// Default port for peer-to-peer `aterm db serve` / `aterm db sync` over `aterm-net`.
pub const DEFAULT_PORT: u16 = 7485;

/// What a read shows in place of a stored password; it is never a password to store.
const REDACTED: &str = "<redacted>";

/// The word a read shows where a replica says a password exists without showing it.
const HAS_PASSWORD: &str = "1";

/// Maximum encoded JSON command payload size per `commit_batch` operation (well below `MAX_COMMAND_BYTES`).
const MAX_BATCH_BYTES: usize = 48 * 1024;

/// Logical table names exposed to users and callers.
pub const TABLES: &[&str] = &["computers", "emails", "phones", "ai_accounts"];

/// Resolve the shared fleet secret from the machine's provisioned `fleet.key` without
/// embedding any secret literal in source code.
pub fn resolve_fleet_secret(dir: &Path) -> Result<Vec<u8>> {
    let mut candidates = vec![dir.to_owned()];
    if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
        candidates.push(parent.to_owned());
    }
    // Where `aterm fleet init|join` writes it, which differs from the data folder on Linux.
    candidates.extend(aterm_types::dirs::state_dir());
    read_fleet_secret(&candidates)
}

/// Read existing enrollment only. An invalid earlier key must not fall through
/// to a different fleet, and reading must never create or replace a key.
fn read_fleet_secret(candidates: &[PathBuf]) -> Result<Vec<u8>> {
    use aterm_uds::private_file;
    for dir in candidates {
        match std::fs::symlink_metadata(dir.join("fleet.key")) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(Error::new(&format!(
                    "cannot inspect provisioned fleet key: {error}"
                )));
            }
        }
        // Keep every read failure after observed existence bound to this candidate.
        let bytes = match private_file::read_private(dir, "fleet.key", 65) {
            Ok(bytes) => Zeroizing::new(bytes),
            Err(error) => {
                return Err(Error::new(&format!(
                    "cannot read provisioned fleet key: {error}"
                )));
            }
        };
        if let Err(error) = aterm_net::fleet::parse_key_file(&bytes) {
            return Err(Error::new(&format!(
                "invalid provisioned fleet key: {error}"
            )));
        }
        return Ok(bytes.trim_ascii().to_vec());
    }
    Err(Error::new(
        "no provisioned fleet key; join the fleet before opening its database",
    ))
}

/// Resolve a user-supplied table name to `(logical_name, collection_name)`.
/// Note: `"phones"` maps to the underlying collection `"phone_numbers"` so legacy
/// `contacts::validate` hooks on `"phones"` in `database.rs` are never triggered.
pub fn resolve_table(raw: &str) -> Result<(&'static str, &'static str)> {
    let trimmed = raw.trim();
    let is = |name: &str| trimmed.eq_ignore_ascii_case(name);
    if is("computers") || is("computer") || is("machines") || is("machine") {
        Ok(("computers", "computers"))
    } else if is("emails") || is("email") || is("mailboxes") || is("addresses") {
        Ok(("emails", "emails"))
    } else if is("phones") || is("phone") || is("phone_numbers") || is("phone-numbers") {
        Ok(("phones", "phone_numbers"))
    } else if is("ai_accounts")
        || is("ai-accounts")
        || is("ai_account")
        || is("ai-account")
        || is("accounts")
    {
        Ok(("ai_accounts", "ai_accounts"))
    } else {
        Err(Error::new(
            "unknown table: expected one of computers, emails, phones, ai_accounts",
        ))
    }
}

fn digits_only(raw: &str) -> String {
    let mut d = String::with_capacity(raw.len());
    for b in raw.bytes().filter(u8::is_ascii_digit) {
        d.push(char::from(b));
    }
    if d.len() == 11 && d.starts_with('1') {
        d.remove(0);
    }
    d
}

/// Normalize a US 10-digit phone number to `XXX-XXX-XXXX`, leaving non-10-digit strings trimmed.
pub fn normalize_phone_key(raw: &str) -> String {
    with_normalized_phone_key(raw, str::to_owned)
}

/// Canonicalize a natural primary key for `logical_table`.
pub fn normalize_natural_key(logical_table: &str, raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(Error::new("table primary key must not be empty"));
    }
    match logical_table {
        "computers" | "emails" => Ok(trimmed.to_ascii_lowercase()),
        "phones" => Ok(normalize_phone_key(trimmed)),
        "ai_accounts" => Ok(trimmed.to_owned()),
        _ => Ok(trimmed.to_ascii_lowercase()),
    }
}

/// Extract the natural primary key from a stored row object, falling back to the storage key.
pub fn natural_key_of(logical_table: &str, stored_key: &str, row: &Value) -> String {
    let field = match logical_table {
        "computers" => "label",
        "emails" => "email",
        "phones" => "phone",
        "ai_accounts" => "id",
        _ => "id",
    };
    row.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(stored_key)
        .to_owned()
}

/// Default database directory (`~/.aterm/lab-db`), or `ATERM_LAB_DB_DIR` when set.
pub fn default_dir() -> Result<PathBuf> {
    if let Some(override_dir) = std::env::var_os("ATERM_LAB_DB_DIR").filter(|s| !s.is_empty()) {
        return Ok(PathBuf::from(override_dir));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| Error::new("HOME is not set"))?;
    Ok(home.join(".aterm").join("lab-db"))
}

/// Refuse the dataset at `dir` while a rekey cut short is still moving rows into it: a key that
/// stands there stops the kept folder's row, so a peer's older row taken now would displace it.
/// `set_aside` marks the kept folder before the rename and the new credentials exist only after
/// it, so a round that loaded those credentials and then calls this sees the mark, where the
/// parent can be listed ([`unfinished_rekeys`]).
pub(crate) fn refuse_unfinished(dir: &Path) -> Result<()> {
    if let Some(kept) = unfinished_rekey(dir)? {
        return Err(Error::new(&format!(
            "a rekey was cut short and the rest of its rows are in {}; `aterm db rekey` finishes the move",
            kept.display()
        )));
    }
    Ok(())
}

fn ensure_fleet_at(dir: &Path) -> Result<()> {
    refuse_unfinished(dir)?;
    let secret = Zeroizing::new(resolve_fleet_secret(dir)?);
    local::ensure_fleet(dir, &secret)
}

/// Open (and auto-initialize on first use) the fleet relational database at `dir`.
pub fn open_at(dir: &Path) -> Result<Database> {
    while_busy(|| {
        ensure_fleet_at(dir)?;
        Database::open(dir)
    })
}

/// Open the default fleet relational database (`~/.aterm/lab-db`).
pub fn open_default() -> Result<(PathBuf, Database)> {
    let dir = default_dir()?;
    let db = open_at(&dir)?;
    Ok((dir, db))
}

/// Validate and normalize a row before writing it to `logical_table`.
pub fn normalize_row(logical_table: &str, natural_key: &str, mut value: Value) -> Result<Value> {
    let obj = value
        .as_object_mut()
        .ok_or_else(|| Error::new("table row must be a JSON object"))?;
    if obj.contains_key("_deleted") {
        return Err(Error::new(
            "row object must not contain reserved \"_deleted\" field; use delete_row",
        ));
    }
    // What a read adds to a row is not part of it, and a redacted password is not a password.
    obj.remove("_conflicts");
    if redacted_fields(obj) {
        return Err(Error::new(
            "this row came from a redacted read; use explicitly provided credentials or a revealed export",
        ));
    }
    if logical_table == "emails" {
        obj.remove("has_password");
    }
    let trimmed_key = natural_key.trim();
    if trimmed_key.is_empty() {
        return Err(Error::new("table primary key must not be empty"));
    }
    match logical_table {
        "computers" => {
            let label = obj
                .get("label")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(trimmed_key)
                .to_ascii_lowercase();
            if !label.eq_ignore_ascii_case(trimmed_key) {
                return Err(Error::new("computers row label must match primary key"));
            }
            obj.insert("label".into(), Value::from(label));
        }
        "emails" => {
            let email = obj
                .get("email")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(trimmed_key)
                .to_ascii_lowercase();
            if !email.eq_ignore_ascii_case(trimmed_key) {
                return Err(Error::new("emails row address must match primary key"));
            }
            if !email.contains('@') {
                return Err(Error::new("emails row requires a valid email address"));
            }
            obj.insert("email".into(), Value::from(email.clone()));
            let always_prot = is_always_protected_email(&email);
            if always_prot && obj.get("protected").and_then(Value::as_bool) == Some(false) {
                return Err(Error::new(
                    "protected personal email address cannot have protected set to false",
                ));
            }
            let protected = always_prot
                || obj
                    .get("protected")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
            obj.insert("protected".into(), Value::from(protected));
            if let Some(ph) = obj
                .get("phone")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                obj.insert("phone".into(), Value::from(normalize_phone_key(ph)));
            }
            if !obj.contains_key("password")
                || obj
                    .get("password")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.trim().is_empty())
            {
                obj.insert("password".into(), Value::Null);
            }
            let skey = storage_key("emails", &email)?;
            validate_email_record(&skey, &value)?;
        }
        "phones" => {
            let raw_phone = obj
                .get("phone")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(trimmed_key);
            let phone = normalize_phone_key(raw_phone);
            if with_normalized_phone_key(trimmed_key, |norm| phone != norm) {
                return Err(Error::new("phones row number must match primary key"));
            }
            obj.insert("phone".into(), Value::from(phone));
        }
        "ai_accounts" => {
            let id = obj
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(trimmed_key)
                .to_owned();
            if id != trimmed_key {
                return Err(Error::new("ai_accounts row id must match primary key"));
            }
            obj.insert("id".into(), Value::from(id));
        }
        _ => {}
    }
    Ok(value)
}

fn is_tombstone(val: &Value) -> bool {
    val.is_null() || val.get("_deleted").and_then(Value::as_bool) == Some(true)
}

/// Drop what a previous build stored as a row's own: the fields a read adds. The mark a redacted
/// read shows was written over a non-empty cell, so it reads as the has-password word it is: no
/// dump carries the mark on, and the import judges it against the registry as it does any such
/// word.
fn drop_read_fields(logical_table: &str, row: &mut Value) {
    let Some(obj) = row.as_object_mut() else {
        return;
    };
    obj.remove("_conflicts");
    if logical_table == "emails" {
        obj.remove("has_password");
        if obj
            .get("password")
            .and_then(Value::as_str)
            .is_some_and(|stored| stored == REDACTED)
        {
            obj.insert("password".into(), Value::from(HAS_PASSWORD));
        }
    }
}

/// How much a row's password cell says: nothing, a has-password word, or a password.
fn password_weight(row: &Value) -> u8 {
    match row.get("password").and_then(Value::as_str).map(str::trim) {
        None | Some("") => 0,
        Some(word) if is_password_flag(word) => 1,
        Some(_) => 2,
    }
}

/// Whether a row's password cell says a password exists: it holds one, or a has-password word.
fn claims_a_password(row: &Value) -> bool {
    row.get("password")
        .and_then(Value::as_str)
        .is_some_and(crate::typed::secret::cell_claims)
}

/// Whether a stored email candidate claims a password after [`drop_read_fields`].
fn claims_stored_password(row: &Value) -> bool {
    row.get("password")
        .and_then(Value::as_str)
        .is_some_and(|pw| {
            if pw == REDACTED {
                crate::typed::secret::cell_claims(HAS_PASSWORD)
            } else {
                crate::typed::secret::cell_claims(pw)
            }
        })
}

/// A row as a conflict judges it: password cells that say the same thing do not disagree, however
/// a build spelled them. A tombstone has no password cell to spell.
fn as_compared(logical_table: &str, row: &Value) -> Value {
    let mut compared = row.clone();
    if logical_table == "emails"
        && !is_tombstone(row)
        && let Some(obj) = compared.as_object_mut()
    {
        obj.insert(
            "password".into(),
            crate::typed::secret::cell_as_compared(row.get("password")),
        );
    }
    compared
}

fn project_cell(logical_table: &str, cell: &Cell) -> Option<Value> {
    if cell.stopped {
        return None;
    }
    // A live value outlives a concurrent delete: the digest order must not silently discard an
    // edit. The row carries the delete in `_conflicts` until a later put or delete settles it.
    let winner = cell
        .candidates
        .iter()
        .rev()
        .find(|c| !is_tombstone(&c.value))?;
    let any_protected = logical_table == "emails"
        && cell.candidates.iter().any(|c| {
            c.value.get("protected").and_then(Value::as_bool) == Some(true)
                || c.value
                    .get("email")
                    .and_then(Value::as_str)
                    .is_some_and(is_always_protected_email)
        });
    // A candidate as a read shows it, which decides a conflict: two values that read alike do not
    // disagree, whatever a previous build stored beside them, and neither do two password cells
    // that say the same thing (`as_compared`). The one exception is below.
    let read = |value: &Value| {
        let mut shown = value.clone();
        drop_read_fields(logical_table, &mut shown);
        if any_protected
            && !is_tombstone(value)
            && let Some(obj) = shown.as_object_mut()
        {
            obj.insert("protected".into(), Value::Bool(true));
            obj.insert("password".into(), Value::Null);
        }
        shown
    };
    let mut row = read(&winner.value);
    if cell.candidates.len() == 1 && (!any_protected || !claims_stored_password(&winner.value)) {
        return Some(row);
    }
    // A stored password outlives a concurrent empty cell or has-password word, as a row outlives a
    // concurrent delete: the digest order must not decide whether a partial edit keeps it, or hide
    // it from `tables` and the import. Both values stay in `_conflicts` until a later write settles
    // them, so the import reports the cell rather than taking either.
    if logical_table == "emails"
        && cell.candidates.len() > 1
        && let Some(held) = cell
            .candidates
            .iter()
            .map(|c| read(&c.value))
            .filter(|value| !is_tombstone(value))
            .max_by_key(password_weight)
        && password_weight(&held) > password_weight(&row)
        && let Some(obj) = row.as_object_mut()
    {
        obj.insert("password".into(), held["password"].clone());
    }
    let row_compared = as_compared(logical_table, &row);
    let mut conflicts: Vec<Value> = Vec::new();
    for candidate in &cell.candidates {
        let mut shown = read(&candidate.value);
        // A protected row shows no password, but one a concurrent value claims is a disagreement,
        // the only sign of it left: a word says so, never the value, so `tables` counts it and the
        // import reports the cell (one-mesh design §4.7).
        if any_protected
            && !is_tombstone(&candidate.value)
            && claims_stored_password(&candidate.value)
            && let Some(obj) = shown.as_object_mut()
        {
            obj.insert("password".into(), Value::from(HAS_PASSWORD));
        }
        if as_compared(logical_table, &shown) != row_compared && !conflicts.contains(&shown) {
            conflicts.push(shown);
        }
    }
    if !conflicts.is_empty()
        && let Some(obj) = row.as_object_mut()
    {
        obj.insert("_conflicts".into(), Value::Array(conflicts));
    }
    Some(row)
}

fn cell_is_protected_email(view: &View, skey: &str) -> bool {
    view.cells
        .get(&("emails".to_owned(), skey.to_owned()))
        .is_some_and(|cell| {
            cell.candidates.iter().any(|c| {
                c.value.get("protected").and_then(Value::as_bool) == Some(true)
                    || c.value
                        .get("email")
                        .and_then(Value::as_str)
                        .is_some_and(is_always_protected_email)
            })
        })
}

fn redacted_fields(object: &Map) -> bool {
    object.iter().any(|(key, value)| {
        (sensitive_field(key, value) && value.as_str() == Some(REDACTED))
            || redacted_credentials(value)
    })
}

fn redacted_credentials(value: &Value) -> bool {
    match value {
        Value::Object(object) => redacted_fields(object),
        Value::Array(values) => values.iter().any(redacted_credentials),
        _ => false,
    }
}

fn erase_credentials(value: Value) {
    match value {
        Value::String(mut text) => text.zeroize(),
        Value::Array(values) => values.into_iter().for_each(erase_credentials),
        Value::Object(values) => values.into_values().for_each(erase_credentials),
        _ => {}
    }
}

fn redact_credentials(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                // An empty credential field holds no secret: it reads as itself, so a redacted
                // read of the row writes back.
                if sensitive_field(key, value) && value.as_str() != Some("") {
                    erase_credentials(std::mem::replace(value, Value::from(REDACTED)));
                } else {
                    redact_credentials(value);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(redact_credentials),
        _ => {}
    }
}

/// Redact credential fields in every table and conflict candidate for ordinary CLI reads.
/// Explicit revealed exports retain their original values for migration and backup.
pub fn redact_row(logical_table: &str, mut row: Value, show_passwords: bool) -> Value {
    if show_passwords {
        return row;
    }
    if logical_table == "emails" {
        let has_pw = claims_a_password(&row);
        if let Some(obj) = row.as_object_mut() {
            obj.insert("has_password".into(), Value::Bool(has_pw));
        }
    }
    redact_credentials(&mut row);
    row
}

/// Project all live rows of `logical_table` from `view`, keyed by their natural primary key.
pub fn scan_table(view: &View, logical_table: &str) -> Result<BTreeMap<String, Value>> {
    let (_, collection) = resolve_table(logical_table)?;
    let mut rows = BTreeMap::new();
    for ((_, stored_key), cell) in view
        .cells
        .range((collection.to_owned(), String::new())..)
        .take_while(|((coll, _), _)| coll == collection)
    {
        if let Some(projected) = project_cell(logical_table, cell) {
            let nat = natural_key_of(logical_table, stored_key, &projected);
            rows.insert(nat, projected);
        }
    }
    Ok(rows)
}

/// Look up one row in `logical_table` by natural key, storage key, or unambiguous computer alias.
pub fn get_row(view: &View, logical_table: &str, key: &str) -> Result<Option<Value>> {
    let (_, collection) = resolve_table(logical_table)?;
    let skey = storage_key(logical_table, key)?;
    if let Some(cell) = view.cells.get(&(collection.to_owned(), skey.clone()))
        && let Some(projected) = project_cell(logical_table, cell)
    {
        return Ok(Some(projected));
    }
    let trimmed = key.trim();
    if trimmed != skey
        && let Some(cell) = view.cells.get(&(collection.to_owned(), trimmed.to_owned()))
        && let Some(projected) = project_cell(logical_table, cell)
    {
        return Ok(Some(projected));
    }
    let mut rows = scan_table(view, logical_table)?;
    let canonical = normalize_natural_key(logical_table, key)?;
    if let Some(val) = rows.remove(&canonical) {
        return Ok(Some(val));
    }
    for (k, v) in &rows {
        if k.eq_ignore_ascii_case(&canonical) {
            return Ok(Some(v.clone()));
        }
    }
    if collection == "computers" {
        let mut matches = Vec::new();
        for v in rows.into_values() {
            let nick_match = v
                .get("nickname")
                .and_then(Value::as_str)
                .is_some_and(|n| n.eq_ignore_ascii_case(&canonical));
            let alias_match = v
                .get("aliases")
                .and_then(Value::as_array)
                .is_some_and(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .any(|a| a.eq_ignore_ascii_case(&canonical))
                });
            let nick_alias_match = v
                .get("nickname_aliases")
                .and_then(Value::as_array)
                .is_some_and(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .any(|a| a.eq_ignore_ascii_case(&canonical))
                });
            if nick_match || alias_match || nick_alias_match {
                matches.push(v);
            }
        }
        return match matches.len() {
            0 => Ok(None),
            1 => Ok(matches.into_iter().next()),
            _ => Err(Error::new(
                "ambiguous computer alias matches multiple rows; use canonical computer label",
            )),
        };
    }
    Ok(None)
}

/// The live row stored under exactly this storage key, never reached through an alias.
fn live_row(view: &View, logical: &str, collection: &str, skey: &str) -> Option<Value> {
    view.cells
        .get(&(collection.to_owned(), skey.to_owned()))
        .and_then(|cell| project_cell(logical, cell))
}

/// Refuse a change that names a row by an alias: reads resolve aliases, but a write through one
/// would change some other row, or start a second row under a name that already means the first.
fn refuse_alias(view: &View, logical: &str, key: &str) -> Result<()> {
    if let Some(row) = get_row(view, logical, key)? {
        let primary = natural_key_of(logical, key, &row);
        return Err(Error::new(&format!(
            "{key} is an alias of {primary}; name the primary key"
        )));
    }
    Ok(())
}

/// Carry onto an email row what the stored row decides: protection is sticky, a row with no
/// `password` field keeps the stored password, since only an explicit `null` clears one, and a
/// has-password word never takes a password's place.
fn inherit_email_state(view: &View, skey: &str, row: &mut Value) -> Result<()> {
    let says_a_word = password_weight(row) == 1;
    let Some(obj) = row.as_object_mut() else {
        return Ok(());
    };
    if cell_is_protected_email(view, skey) {
        if obj.get("protected").and_then(Value::as_bool) == Some(false) {
            return Err(Error::new(
                "protected real email row cannot have protected downgraded to false",
            ));
        }
        obj.insert("protected".into(), Value::Bool(true));
    }
    let stored = live_row(view, "emails", "emails", skey);
    if says_a_word && stored.as_ref().map_or(0, password_weight) == 2 {
        return Err(Error::new(
            "this password is a has-password word and would replace the stored one; omit it to keep the stored one, or give null to clear it",
        ));
    }
    if !obj.contains_key("password")
        && obj.get("protected").and_then(Value::as_bool) != Some(true)
        && let Some(stored) = stored.and_then(|row| row.get("password").cloned())
    {
        obj.insert("password".into(), stored);
    }
    Ok(())
}

/// Replace the row whose primary key is `key` in `logical_table`, or insert it.
pub fn put_row(
    db: &mut Database,
    logical_table: &str,
    key: &str,
    mut value: Value,
) -> Result<Value> {
    let (logical, collection) = resolve_table(logical_table)?;
    let view = db.view();
    let prelim_skey = storage_key(logical, key)?;
    if live_row(&view, logical, collection, &prelim_skey).is_none() {
        refuse_alias(&view, logical, key)?;
    }
    if logical == "emails" {
        inherit_email_state(&view, &prelim_skey, &mut value)?;
    }
    let normalized = normalize_row(logical, key, value)?;
    let nat_key = natural_key_of(logical, key, &normalized);
    let skey = if nat_key.eq_ignore_ascii_case(key.trim()) {
        prelim_skey
    } else {
        storage_key(logical, &nat_key)?
    };
    let request = crypto::random::<16>()?;
    let op = db.commit(Command::Write {
        request,
        changes: vec![Change {
            collection: collection.to_owned(),
            key: skey.clone(),
            value: Some(normalized),
        }],
    })?;
    Ok(json!({
        "ok": true,
        "table": logical,
        "key": nat_key,
        "storage_key": skey,
        "operation": hex::encode_lower(&op),
    }))
}

/// Delete (tombstone) the row whose primary key is `key`, allowing later re-insertion under it.
pub fn delete_row(db: &mut Database, logical_table: &str, key: &str) -> Result<Value> {
    let (logical, collection) = resolve_table(logical_table)?;
    let view = db.view();
    let skey = storage_key(logical, key)?;
    let Some(existing) = live_row(&view, logical, collection, &skey) else {
        refuse_alias(&view, logical, key)?;
        return Err(Error::new("row not found in table"));
    };
    let nat_key = natural_key_of(logical, key, &existing);
    let request = crypto::random::<16>()?;
    let op = db.commit(Command::Write {
        request,
        changes: vec![Change {
            collection: collection.to_owned(),
            key: skey.clone(),
            value: Some(json!({"_deleted": true})),
        }],
    })?;
    Ok(json!({
        "ok": true,
        "table": logical,
        "key": nat_key,
        "storage_key": skey,
        "operation": hex::encode_lower(&op),
        "deleted": true,
    }))
}

/// Commit a batch of changes across the 4 tables, automatically chunking into transactions of
/// at most `MAX_WRITES` (64) changes and `<= MAX_BATCH_BYTES` per `astream` operation.
pub fn commit_batch(db: &mut Database, changes: Vec<Change>) -> Result<Vec<String>> {
    let mut ops = Vec::new();
    for changes in transactions(changes)? {
        let request = crypto::random::<16>()?;
        let op = db.commit(Command::Write { request, changes })?;
        ops.push(hex::encode_lower(&op));
    }
    Ok(ops)
}

/// Split `changes` into the transactions [`commit_batch`] commits, refusing a row none can carry.
fn transactions(changes: Vec<Change>) -> Result<Vec<Vec<Change>>> {
    let mut chunks = Vec::new();
    let mut current_chunk = Vec::new();
    let mut current_bytes = 64usize;

    for change in changes {
        // A row `put` accepted has to load too: alone in a transaction it may use all of one.
        let alone_len = Command::encode_write([0; 16], std::slice::from_ref(&change))?.len();
        if alone_len > crypto::MAX_BODY {
            return Err(Error::new(
                "single row exceeds maximum operation payload limit",
            ));
        }
        let change_bytes = alone_len.saturating_sub(67);
        if !current_chunk.is_empty()
            && (current_chunk.len() >= MAX_WRITES || current_bytes + change_bytes > MAX_BATCH_BYTES)
        {
            chunks.push(std::mem::take(&mut current_chunk));
            current_bytes = 64;
        }
        current_bytes += change_bytes;
        current_chunk.push(change);
    }
    if !current_chunk.is_empty() {
        chunks.push(current_chunk);
    }
    Ok(chunks)
}

/// Refuse changes one writer's retained history could not hold, so a rekey stops before it moves
/// anything rather than part way through. A record costs more than a transaction's encoding, by
/// its header and seal; this allows well over what either takes.
fn one_writer_holds(changes: Vec<Change>) -> Result<()> {
    const RECORD_OVERHEAD: usize = 1024;
    let chunks = transactions(changes)?;
    let operations = chunks.len();
    let mut bytes = 0usize;
    for changes in chunks {
        let command = Command::Write {
            request: [0; 16],
            changes,
        };
        bytes = bytes.saturating_add(command.encode()?.len() + RECORD_OVERHEAD);
    }
    if operations > MAX_WRITER_OPERATIONS || bytes > MAX_WRITER_BYTES {
        return Err(Error::new(
            "these rows are more than one writer can hold in a new dataset, so nothing moved",
        ));
    }
    Ok(())
}

/// Check relational integrity, foreign keys, conflict count, and causal health across all 4 tables.
pub fn tables_summary(db: &Database) -> Result<Value> {
    let view = db.view();
    let computers = scan_table(&view, "computers")?;
    let emails = scan_table(&view, "emails")?;
    let phones = scan_table(&view, "phones")?;
    let ai_accounts = scan_table(&view, "ai_accounts")?;

    // The rows a read shows a conflict on, so the count and the rows cannot disagree.
    let conflicts = [&computers, &emails, &phones, &ai_accounts]
        .into_iter()
        .flat_map(BTreeMap::values)
        .filter(|row| row.get("_conflicts").is_some())
        .count();

    let mut root_mailboxes_with_password = 0usize;
    let mut aliases = 0usize;
    let mut protected_emails = 0usize;
    let mut broken_root_refs = Vec::new();
    let mut broken_phone_refs = Vec::new();
    let mut broken_computer_refs = Vec::new();
    let mut broken_account_emails = Vec::new();
    let mut broken_account_refs = Vec::new();

    let has_ci = |map: &BTreeMap<String, Value>, raw: &str| {
        let trimmed = raw.trim();
        if trimmed.bytes().any(|b| b.is_ascii_uppercase()) {
            map.contains_key(&trimmed.to_ascii_lowercase())
        } else {
            map.contains_key(trimmed)
        }
    };
    for (email, row) in &emails {
        let is_protected = row
            .get("protected")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if is_protected {
            protected_emails += 1;
        }
        let acct_type = row
            .get("account_type")
            .and_then(Value::as_str)
            .unwrap_or("root");
        if acct_type.eq_ignore_ascii_case("alias") {
            aliases += 1;
            let root = row
                .get("root_account")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            if root.is_empty() {
                broken_root_refs.push(format!("{email} -> <missing root_account>"));
            } else if let Some(target) = if root.bytes().any(|b| b.is_ascii_uppercase()) {
                emails.get(&root.to_ascii_lowercase())
            } else {
                emails.get(root)
            } {
                let target_type = target
                    .get("account_type")
                    .and_then(Value::as_str)
                    .unwrap_or("root");
                if !target_type.eq_ignore_ascii_case("root") {
                    broken_root_refs.push(format!("{email} -> {root} (not root)"));
                }
            } else {
                broken_root_refs.push(format!("{email} -> {root}"));
            }
        } else if password_weight(row) == 2 {
            root_mailboxes_with_password += 1;
        }
        if let Some(ph) = row
            .get("phone")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            && !with_normalized_phone_key(ph, |norm| phones.contains_key(norm))
        {
            broken_phone_refs.push(format!("{email} -> {ph}"));
        }
        if let Some(comps) = row.get("assigned_computers").and_then(Value::as_array) {
            for c in comps.iter().filter_map(Value::as_str) {
                if !has_ci(&computers, c) {
                    broken_computer_refs.push(format!("emails:{email} -> {c}"));
                }
            }
        }
    }
    for (id, row) in &ai_accounts {
        for field_name in ["email", "receiving_mailbox"] {
            if let Some(em) = row
                .get(field_name)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                && !has_ci(&emails, em)
            {
                broken_account_emails.push(format!("{id}.{field_name} -> {em}"));
            }
        }
        if let Some(comps) = row.get("assigned_computers").and_then(Value::as_array) {
            for c in comps.iter().filter_map(Value::as_str) {
                if !has_ci(&computers, c) {
                    broken_computer_refs.push(format!("ai_accounts:{id} -> {c}"));
                }
            }
        }
    }

    for (comp, row) in &computers {
        if let Some(accs) = row.get("assigned_accounts").and_then(Value::as_array) {
            for acc in accs.iter().filter_map(Value::as_str) {
                if !ai_accounts.contains_key(acc.trim()) {
                    broken_account_refs.push(format!("computers:{comp} -> {acc}"));
                }
            }
        }
        if let Some(ems) = row.get("assigned_emails").and_then(Value::as_array) {
            for em in ems.iter().filter_map(Value::as_str) {
                if !has_ci(&emails, em) {
                    broken_account_emails.push(format!("computers:{comp} -> {em}"));
                }
            }
        }
    }

    for (ph, row) in &phones {
        if let Some(ems) = row.get("linked_emails").and_then(Value::as_array) {
            for em in ems.iter().filter_map(Value::as_str) {
                if !has_ci(&emails, em) {
                    broken_account_emails.push(format!("phones:{ph} -> {em}"));
                }
            }
        }
        if let Some(comps) = row.get("linked_computers").and_then(Value::as_array) {
            for c in comps.iter().filter_map(Value::as_str) {
                if !has_ci(&computers, c) {
                    broken_computer_refs.push(format!("phones:{ph} -> {c}"));
                }
            }
        }
    }

    let authority_fault = view.authority_fault;
    let quarantined_writers = view.causal.quarantined.len() as u64;
    let invalid_operations = view.invalid.len() as u64;
    let pending_operations = view.causal.pending.len() as u64;
    let operations = db.history.index().inventory().count() as u64;
    let (writer_operations, writer_bytes) =
        db.history.writer_usage(&db.credentials.capability.actor);

    let ok = !authority_fault
        && quarantined_writers == 0
        && pending_operations == 0
        && invalid_operations == 0
        && conflicts == 0
        && broken_root_refs.is_empty()
        && broken_phone_refs.is_empty()
        && broken_computer_refs.is_empty()
        && broken_account_emails.is_empty()
        && broken_account_refs.is_empty();

    Ok(json!({
        "ok": ok,
        "engine": "astream-broker + aterm-net fleet sync",
        "namespace": db.credentials.manifest.namespace,
        "dataset_id": hex::encode_lower(&db.credentials.manifest.id),
        "operations": operations,
        "pending_operations": pending_operations,
        "authority_fault": authority_fault,
        "quarantined_writers": quarantined_writers,
        "invalid_operations": invalid_operations,
        "conflicts": conflicts as u64,
        "tables": {
            "computers": computers.len() as u64,
            "emails": {
                "total": emails.len() as u64,
                "root_with_password": root_mailboxes_with_password as u64,
                "aliases": aliases as u64,
                "protected_no_password": protected_emails as u64,
            },
            "phones": phones.len() as u64,
            "ai_accounts": ai_accounts.len() as u64,
        },
        "capacity": {
            "operations": operations,
            "operations_limit": MAX_RECORDS as u64,
            "journal_bytes": db.journal_bytes() as u64,
            "journal_bytes_limit": local::MAX_JOURNAL as u64,
            "writer_operations": writer_operations as u64,
            "writer_operations_limit": MAX_WRITER_OPERATIONS as u64,
            "writer_bytes": writer_bytes as u64,
            "writer_bytes_limit": MAX_WRITER_BYTES as u64,
        },
        "integrity": {
            "broken_root_refs": broken_root_refs,
            "broken_phone_refs": broken_phone_refs,
            "broken_computer_refs": broken_computer_refs,
            "broken_account_emails": broken_account_emails,
            "broken_account_refs": broken_account_refs,
        }
    }))
}

/// Export all 4 tables to a single canonical JSON document.
pub fn dump_all(db: &Database, show_passwords: bool) -> Result<Value> {
    dump_view(&db.view(), show_passwords)
}

fn dump_view(view: &View, show_passwords: bool) -> Result<Value> {
    let mut out = Map::new();
    for &t in TABLES {
        let rows = scan_table(view, t)?;
        let list: Vec<Value> = rows
            .into_values()
            .map(|r| redact_row(t, r, show_passwords))
            .collect();
        out.insert(t.to_owned(), Value::Array(list));
    }
    Ok(Value::Object(out))
}

/// What an import does with a key the database has already used.
#[derive(Clone, Copy)]
enum Existing {
    /// A restore: the document wins wherever it differs.
    Replace,
    /// A first import: edits and deletions made since stand.
    Keep,
}

/// The writes a document calls for, all validated before any of them commits.
struct Plan {
    changes: Vec<Change>,
    written: Map,
    unchanged: u64,
}

fn plan_rows(view: &View, doc: &Value, existing: Existing) -> Result<Plan> {
    let obj = doc
        .as_object()
        .ok_or_else(|| Error::new("snapshot document must be a JSON object"))?;
    if obj.keys().any(|table| !TABLES.contains(&table.as_str())) {
        return Err(Error::new(
            "snapshot document names an unknown table: expected computers, emails, phones, ai_accounts",
        ));
    }
    let mut plan = Plan {
        changes: Vec::new(),
        written: Map::new(),
        unchanged: 0,
    };
    let mut seen_keys = BTreeSet::new();
    let mut lookup_key = (String::new(), String::new());
    for &logical in TABLES {
        let (_, collection) = resolve_table(logical)?;
        lookup_key.0.clear();
        lookup_key.0.push_str(collection);
        let items = match obj.get(logical) {
            None => &[][..],
            Some(Value::Array(items)) => items.as_slice(),
            Some(_) => return Err(Error::new("each snapshot table must be a JSON array")),
        };
        let mut written = 0u64;
        for item in items {
            let nat = natural_key_of(logical, "", item);
            let mut item_val = item.clone();
            if logical == "emails" {
                let skey = storage_key(logical, &nat)?;
                lookup_key.1.clear();
                lookup_key.1.push_str(&skey);
                // A key that stands is not written, so what is stored there decides nothing here.
                let stands =
                    matches!(existing, Existing::Keep) && view.cells.contains_key(&lookup_key);
                if !stands {
                    inherit_email_state(view, &skey, &mut item_val)?;
                }
            }
            let normalized = normalize_row(logical, &nat, item_val)?;
            let final_nat = natural_key_of(logical, &nat, &normalized);
            let skey = storage_key(logical, &final_nat)?;
            if !seen_keys.insert((collection, skey.clone())) {
                return Err(Error::new(
                    "snapshot document contains duplicate primary keys in table",
                ));
            }
            lookup_key.1.clear();
            lookup_key.1.push_str(&skey);
            let cell = view.cells.get(&lookup_key);
            let settled = match existing {
                Existing::Keep => cell.is_some(),
                // A dump of exactly this state changes nothing, a conflict it shows included.
                Existing::Replace => cell.is_some_and(|cell| {
                    (!cell.candidates.is_empty()
                        && cell.candidates.iter().all(|c| c.value == normalized))
                        || project_cell(logical, cell).as_ref() == Some(item)
                }),
            };
            if settled {
                plan.unchanged += 1;
                continue;
            }
            if cell.is_some_and(|cell| cell.stopped) {
                return Err(Error::new(
                    "snapshot document names a key whose object generation stopped",
                ));
            }
            plan.changes.push(Change {
                collection: collection.to_owned(),
                key: skey,
                value: Some(normalized),
            });
            written += 1;
        }
        plan.written
            .insert(logical.to_owned(), Value::from(written));
    }
    Ok(plan)
}

fn import_rows(db: &mut Database, doc: &Value, existing: Existing) -> Result<Value> {
    let plan = plan_rows(&db.view(), doc, existing)?;
    let ops = commit_batch(db, plan.changes)?;
    Ok(json!({
        "ok": true,
        "imported": Value::Object(plan.written),
        "unchanged": plan.unchanged,
        "operations_committed": ops.len() as u64,
    }))
}

/// Write the rows of a [`dump_all`] document that differ from what is stored. Batches commit one
/// after another; a load cut short leaves the earlier ones, and running it again finishes it.
pub fn load_all(db: &mut Database, doc: &Value) -> Result<Value> {
    import_rows(db, doc, Existing::Replace)
}

/// Move the database at `dir` onto the current fleet key after `aterm fleet init --rotate` or a
/// join to another fleet. Its own saved credentials still open it; the live rows go into a new
/// dataset in its place, and the old folder stays beside it, private and whole.
pub fn rekey_at(dir: &Path) -> Result<Value> {
    let secret = Zeroizing::new(resolve_fleet_secret(dir)?);
    rekey_with(dir, &secret)
}

/// In a kept folder until all of its rows have reached the new dataset. It holds that folder's
/// place in the order the folders were set aside.
pub(crate) const REKEY_UNFINISHED: &str = "rekey-unfinished";

/// The folder an earlier rekey of `dir` kept and did not finish moving, if any.
fn unfinished_rekey(dir: &Path) -> Result<Option<PathBuf>> {
    Ok(unfinished_rekeys(dir)?
        .into_iter()
        .next()
        .map(|(_, path)| path))
}

/// The folders earlier rekeys of `dir` kept and did not finish moving, the last one set aside
/// first: it holds what a load cut short wrote and any edit made since, which the older folders'
/// rows must not displace. A parent this user cannot list shows none, so nothing there is refused
/// for it: a kept folder in a parent it can search but not list (mode 0300, say) stays unfinished
/// and unrefused until the parent can be listed, and a key that stands by then stops that folder's
/// row, as [`refuse_unfinished`] exists to prevent.
fn unfinished_rekeys(dir: &Path) -> Result<Vec<(u64, PathBuf)>> {
    let Some(name) = dir.file_name().and_then(|name| name.to_str()) else {
        return Ok(Vec::new());
    };
    let prefix = format!("{name}.before-rekey-");
    let parent = dir
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            return Ok(Vec::new());
        }
        Err(error) => return Err(error.into()),
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(&prefix))
        {
            continue;
        }
        let path = entry.path();
        if let Ok((open, meta)) = aterm_uds::regular_file::open(&path.join(REKEY_UNFINISHED))
            && meta.len() <= 64
        {
            use std::io::Read as _;
            let mut place = String::with_capacity(meta.len() as usize + 1);
            if open.take(65).read_to_string(&mut place).is_ok() && place.len() <= 64 {
                found.push((place.trim().parse().unwrap_or(0), path));
            }
        }
    }
    found.sort_unstable_by(|a, b| b.cmp(a));
    Ok(found)
}

/// The keys `view` deleted, as tombstones for a new dataset: a dump holds only live rows, so a
/// machine that had not seen a deletion would otherwise bring its row back with no conflict.
fn deletions(view: &View) -> Vec<Change> {
    view.cells
        .iter()
        .filter(|((collection, _), cell)| {
            matches!(
                collection.as_str(),
                "computers" | "emails" | "phone_numbers" | "ai_accounts"
            ) && !cell.candidates.is_empty()
                && cell.candidates.iter().all(|c| is_tombstone(&c.value))
        })
        .map(|((collection, key), _)| Change {
            collection: collection.clone(),
            key: key.clone(),
            value: Some(json!({"_deleted": true})),
        })
        .collect()
}

/// The records `db` holds that write cells and its read leaves out, a forked writer's or one
/// waiting for its parent. A refused record is pending too and never held a row, and a merge writes
/// no cell.
pub(crate) fn withheld_operations(db: &Database) -> Vec<astream_wire::sync::Digest> {
    db.view()
        .causal
        .pending
        .iter()
        .filter(|id| {
            db.history.command(id).is_some_and(|(_, command)| {
                command.changes().is_some_and(|changes| !changes.is_empty())
            })
        })
        .copied()
        .collect()
}

/// What a move to a new dataset does not carry, which stays only in the kept folder.
fn left_behind(view: &View) -> Result<Value> {
    let mut conflicts = Vec::new();
    for &table in TABLES {
        for (key, row) in scan_table(view, table)? {
            if row.get("_conflicts").is_some() {
                conflicts.push(format!("{table}/{key}"));
            }
        }
    }
    Ok(json!({
        "conflicts_settled_by_digest": conflicts,
        "pending_operations": view.causal.pending.len() as u64,
        "quarantined_writers": view.causal.quarantined.len() as u64,
    }))
}

fn rekey_with(dir: &Path, secret: &[u8]) -> Result<Value> {
    let mut kept = unfinished_rekeys(dir)?;
    if kept.is_empty() {
        if while_busy(|| local::belongs_to_fleet(dir, secret))? {
            return Err(Error::new(
                "this database already belongs to the current fleet key; nothing changed",
            ));
        }
        kept.push((0, set_aside(dir, 0)?));
    } else if while_busy(|| local::holds_another_dataset(dir, secret))? {
        // The key rotated after a load was cut short: what it loaded is one more folder to move.
        let place = kept[0].0 + 1;
        kept.insert(0, (place, set_aside(dir, place)?));
    }
    let mut moved = Vec::new();
    for (_, folder) in &kept {
        moved.push(finish_rekey(dir, folder, secret)?);
    }
    Ok(json!({"ok": true, "moved": moved}))
}

/// Mark the dataset at `dir` unfinished and move it to a folder of its own beside `dir`, once what
/// it holds is known to fit a new dataset. A key used twice would name the same folder twice.
fn set_aside(dir: &Path, place: u64) -> Result<PathBuf> {
    let old = while_busy(|| Database::open_for_rekey(dir))?;
    let view = old.view();
    let mut changes =
        plan_rows(&View::default(), &dump_view(&view, true)?, Existing::Keep)?.changes;
    changes.extend(deletions(&view));
    one_writer_holds(changes)?;
    let name = dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::new("invalid database folder name"))?;
    let stem = format!(
        "{name}.before-rekey-{}",
        hex::encode_lower(&old.credentials.manifest.id[..8])
    );
    let kept = std::iter::once(stem.clone())
        .chain((1u32..).map(|n| format!("{stem}-{n}")))
        .map(|folder| dir.with_file_name(folder))
        .find(|folder| !folder.exists())
        .ok_or_else(|| Error::new("no free name beside this database"))?;
    aterm_uds::private_file::write_private(dir, REKEY_UNFINISHED, place.to_string().as_bytes())?;
    std::fs::rename(dir, &kept)?;
    drop(old);
    Ok(kept)
}

/// Move the rows of the kept folder into the dataset at `dir`, which can be run again from any
/// point: a key already used there stands, including an edit made since the move was cut short.
fn finish_rekey(dir: &Path, kept: &Path, secret: &[u8]) -> Result<Value> {
    let rerun = |error: Error| {
        error.context(&format!(
            "the old rows stay in {}, and `aterm db rekey` finishes the move",
            kept.display()
        ))
    };
    let (doc, deleted, left) = while_busy(|| Database::open_for_rekey(kept))
        .and_then(|old| {
            let view = old.view();
            Ok((
                dump_view(&view, true)?,
                deletions(&view),
                left_behind(&view)?,
            ))
        })
        .map_err(rerun)?;
    let moved = while_busy(|| {
        local::ensure_fleet(dir, secret)?;
        Database::open(dir)
    })
    .and_then(|mut db| {
        let moved = import_rows(&mut db, &doc, Existing::Keep)?;
        let view = db.view();
        let mut key_pair = (String::new(), String::new());
        let fresh = deleted
            .into_iter()
            .filter(|change| {
                key_pair.0.clone_from(&change.collection);
                key_pair.1.clone_from(&change.key);
                !view.cells.contains_key(&key_pair)
            })
            .collect();
        commit_batch(&mut db, fresh)?;
        Ok(moved)
    })
    .map_err(rerun)?;
    crate::alab::import::covered::carry(kept, dir).map_err(rerun)?;
    std::fs::remove_file(kept.join(REKEY_UNFINISHED))?;
    Ok(json!({
        "imported": moved.get("imported").cloned().unwrap_or(Value::Null),
        "kept": kept.display().to_string(),
        "left_behind": left,
    }))
}

/// Whether a password cell holds a has-password word, as an export that leaves passwords out does,
/// by the words the import reads ([`crate::typed::secret::flag_says`]). A flag is not a password.
fn is_password_flag(cell: &str) -> bool {
    crate::typed::secret::flag_says_str(cell).is_some()
}

/// Read non-flag email passwords from an optional workbook (`alab-emails.xlsx` or
/// `AI Email Accounting.xlsx`) in `secrets_dir` (`Emails` tab, column C), so `bootstrap`
/// populates real passwords even when `alab-emails.csv` only carries has-password flags (`1`).
/// Returns them with the workbooks that are there and could not be read: a file that holds them
/// and fails to open must not read as one that holds none.
fn read_xlsx_email_passwords(
    secrets_dir: &Path,
) -> (BTreeMap<String, Zeroizing<String>>, Vec<&'static str>) {
    let mut out = BTreeMap::new();
    let mut unread = Vec::new();
    for filename in ["alab-emails.xlsx", "AI Email Accounting.xlsx"] {
        let path = secrets_dir.join(filename);
        let bytes = match aterm_uds::atomic_file::read_regular(&path, 64 * 1024 * 1024) {
            Ok((bytes, _)) => Zeroizing::new(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                unread.push(filename);
                continue;
            }
        };
        let Ok(mut records) = crate::alab::import::xlsx::tab(&bytes, "Emails") else {
            unread.push(filename);
            continue;
        };
        for row in records.iter().skip(1) {
            let email = row.first().map_or("", |s| s.trim()).to_ascii_lowercase();
            let pw = row.get(2).map_or("", |s| s.trim());
            if !email.is_empty() && !pw.is_empty() && !is_password_flag(pw) {
                out.insert(email, Zeroizing::new(pw.to_owned()));
            }
        }
        for row in &mut records {
            row.iter_mut().for_each(Zeroize::zeroize);
        }
        if !out.is_empty() {
            break;
        }
    }
    (out, unread)
}

fn default_webmail_url(provider: &str) -> &'static str {
    match provider {
        "proton" | "protonmail" => "https://mail.proton.me/u/0/inbox",
        "gmail" | "google" => "https://mail.google.com/",
        "outlook" | "hotmail" => "https://outlook.live.com/mail/",
        "privateemail" => "https://privateemail.com/",
        _ => "",
    }
}

/// Bootstrap the 4 canonical tables (`computers`, `emails`, `phones`, `ai_accounts`) from
/// legacy CSV files in `secrets_dir` and enrich with the live fleet roster. A key the database
/// has already used is left alone, so running it again changes nothing: edits and deletions stand.
pub fn bootstrap_from_secrets(db: &mut Database, secrets_dir: &Path) -> Result<Value> {
    let phones_bytes =
        crate::import::read_bounded(&secrets_dir.join(aterm_secrets::entry_class::ALAB_PHONES))?;
    let phones_records = crate::import::parse(&phones_bytes)?;
    let mut phones_map: BTreeMap<String, Map> = BTreeMap::new();
    let mut digits_to_phone: BTreeMap<String, String> = BTreeMap::new();
    for (idx, row) in phones_records.iter().skip(1).enumerate() {
        let raw_phone = row.first().map_or("", |s| s.trim());
        if raw_phone.is_empty() {
            continue;
        }
        let phone = normalize_phone_key(raw_phone);
        let carrier = row.get(1).map_or("", |s| s.trim());
        let line_type = row.get(2).map_or("", |s| s.trim());
        let device = row.get(3).map_or("", |s| s.trim());
        let notes = row.get(4).map_or("", |s| s.trim());
        let d = digits_only(&phone);
        if d.len() >= 7 {
            digits_to_phone.insert(d, phone.clone());
        }
        let mut obj = Map::new();
        obj.insert("phone".into(), Value::from(phone.clone()));
        obj.insert("label".into(), Value::from(format!("phone-{}", idx + 1)));
        obj.insert("owner_carrier".into(), Value::from(carrier));
        obj.insert("line_type".into(), Value::from(line_type));
        obj.insert("device".into(), Value::from(device));
        obj.insert("status".into(), Value::from("active"));
        obj.insert("linked_emails".into(), Value::Array(Vec::new()));
        obj.insert("linked_computers".into(), Value::Array(Vec::new()));
        obj.insert("notes".into(), Value::from(notes));
        phones_map.insert(phone, obj);
    }

    let comp_bytes =
        crate::import::read_bounded(&secrets_dir.join(aterm_secrets::entry_class::ALAB_COMPUTERS))?;
    let comp_records = crate::import::parse(&comp_bytes)?;
    let mut roster_by_label: BTreeMap<String, (Option<String>, Option<String>)> = BTreeMap::new();
    if let Ok(paths) = aterm_secrets::client::Paths::live()
        && let Some(roster) =
            aterm_secrets::client::swarm_roster_until(&paths, crate::admission::deadline())
                .map_err(crate::table::failure)?
    {
        for member in &roster.members {
            if !roster.is_member_denied(member) {
                let lbl = member.label.trim().to_ascii_lowercase();
                roster_by_label.insert(lbl, (member.node_id.clone(), None));
            }
        }
    }
    if let Some(state) = aterm_types::dirs::state_dir() {
        let census_path = state.join("swarm-census.json");
        if let Ok((bytes, _)) = aterm_uds::atomic_file::read_regular(&census_path, 4 * 1024 * 1024)
            && let Ok(val) = aterm_json::from_slice::<Value>(&bytes)
            && let Some(members) = val.get("members").and_then(Value::as_array)
        {
            for m in members {
                let node = m.get("node").and_then(Value::as_str).map(str::to_owned);
                let host = m.get("host").and_then(Value::as_str).map(str::to_owned);
                for key_field in ["label", "fleet_name"] {
                    if let Some(lbl) = m
                        .get(key_field)
                        .and_then(Value::as_str)
                        .map(|s| s.trim().to_ascii_lowercase())
                        .filter(|s| !s.is_empty())
                    {
                        let entry = roster_by_label.entry(lbl).or_insert((None, None));
                        if entry.0.is_none() {
                            entry.0 = node.clone();
                        }
                        if entry.1.is_none() {
                            entry.1 = host.clone();
                        }
                    }
                }
            }
        }
    }
    let mut computers_map: BTreeMap<String, Map> = BTreeMap::new();
    let mut alias_to_computer: BTreeMap<String, String> = BTreeMap::new();
    for row in comp_records.iter().skip(1) {
        let label = row.first().map_or("", |s| s.trim()).to_ascii_lowercase();
        if label.is_empty() {
            continue;
        }
        let make = row.get(1).map_or("", |s| s.trim());
        let arch = row.get(2).map_or("", |s| s.trim());
        let os = row.get(3).map_or("", |s| s.trim());
        let chip = row.get(4).map_or("", |s| s.trim());
        let parse_leading_u64 = |col: usize| {
            let s = row.get(col).map_or("", |s| s.trim());
            let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
            s[..end].parse::<u64>().ok()
        };
        let ram = parse_leading_u64(5);
        let disk = parse_leading_u64(6);
        let aliases_raw = row.get(7).map_or("", |s| s.trim());
        let aliases: Vec<String> = aliases_raw
            .split([',', ';'])
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        alias_to_computer.insert(label.clone(), label.clone());
        for a in &aliases {
            alias_to_computer.insert(a.clone(), label.clone());
            alias_to_computer.insert(a.replace('.', "-"), label.clone());
        }
        let nickname = row.get(8).map_or("", |s| s.trim());
        let nick_aliases: Vec<Value> = row
            .get(9)
            .map_or("", |s| s.trim())
            .split([',', ';'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(Value::from)
            .collect();
        let location = row.get(10).map_or("", |s| s.trim());
        let notes = row.get(11).map_or("", |s| s.trim());
        let roster_hit = roster_by_label
            .get(&label)
            .or_else(|| aliases.iter().find_map(|a| roster_by_label.get(a)));
        let node_id = roster_hit
            .and_then(|(n, _)| n.clone())
            .map_or(Value::Null, Value::from);
        let ip = roster_hit
            .and_then(|(_, h)| h.clone())
            .map_or(Value::Null, Value::from);

        let mut obj = Map::new();
        obj.insert("label".into(), Value::from(label.clone()));
        obj.insert("hostname".into(), Value::from(format!("{label}.local")));
        obj.insert("ip".into(), ip);
        obj.insert("node_id".into(), node_id);
        obj.insert("make".into(), Value::from(make));
        obj.insert("arch".into(), Value::from(arch));
        obj.insert("os".into(), Value::from(os));
        obj.insert("chip".into(), Value::from(chip));
        obj.insert("memory_gb".into(), ram.map_or(Value::Null, Value::from));
        obj.insert("disk_gb".into(), disk.map_or(Value::Null, Value::from));
        obj.insert(
            "aliases".into(),
            Value::Array(aliases.into_iter().map(Value::from).collect()),
        );
        obj.insert("nickname".into(), Value::from(nickname));
        obj.insert("nickname_aliases".into(), Value::Array(nick_aliases));
        obj.insert("location".into(), Value::from(location));
        obj.insert("status".into(), Value::from("active"));
        obj.insert("assigned_accounts".into(), Value::Array(Vec::new()));
        obj.insert("assigned_emails".into(), Value::Array(Vec::new()));
        obj.insert("notes".into(), Value::from(notes));
        computers_map.insert(label, obj);
    }

    let (xlsx_passwords, unread_workbooks) = read_xlsx_email_passwords(secrets_dir);
    let no_passwords = xlsx_passwords.is_empty();
    let emails_bytes =
        crate::import::read_bounded(&secrets_dir.join(aterm_secrets::entry_class::ALAB_EMAILS))?;
    let emails_records = crate::import::parse(&emails_bytes)?;
    let mut emails_map: BTreeMap<String, Map> = BTreeMap::new();
    for row in emails_records.iter().skip(1) {
        let email = row.first().map_or("", |s| s.trim()).to_ascii_lowercase();
        if email.is_empty() {
            continue;
        }
        let root_raw = row.get(1).map_or("", |s| s.trim()).to_ascii_lowercase();
        let pw_raw = row.get(2).map_or("", |s| s.trim());
        let status_raw = row
            .get(3)
            .map_or("active", |s| s.trim())
            .to_ascii_lowercase();
        let prov_raw = row.get(4).map_or("", |s| s.trim()).to_ascii_lowercase();
        let phone_raw = row.get(5).map_or("", |s| s.trim());
        let prot_raw = row.get(6).map_or("", |s| s.trim());
        let notes_raw = row.get(7).map_or("", |s| s.trim());

        let is_protected = !prot_raw.is_empty() || is_always_protected_email(&email);
        let is_alias = !root_raw.is_empty() && root_raw != email;
        let account_type = if is_alias { "alias" } else { "root" };
        let root_account = if is_alias {
            root_raw.clone()
        } else {
            email.clone()
        };
        let password = if is_protected || is_alias {
            Value::Null
        } else if !pw_raw.is_empty() && !is_password_flag(pw_raw) {
            Value::from(pw_raw)
        } else if let Some(xlsx_pw) = xlsx_passwords.get(&email) {
            Value::from(xlsx_pw.as_str())
        } else {
            Value::Null
        };
        let resolved_phone = if phone_raw.is_empty() {
            Value::Null
        } else {
            let norm_ph = normalize_phone_key(phone_raw);
            if phones_map.contains_key(&norm_ph) {
                Value::from(norm_ph)
            } else {
                let d = digits_only(phone_raw);
                digits_to_phone
                    .get(&d)
                    .cloned()
                    .map_or(Value::Null, Value::from)
            }
        };

        let mut obj = Map::new();
        obj.insert("email".into(), Value::from(email.clone()));
        obj.insert("provider".into(), Value::from(prov_raw.as_str()));
        obj.insert("account_type".into(), Value::from(account_type));
        obj.insert("root_account".into(), Value::from(root_account));
        obj.insert("password".into(), password);
        obj.insert(
            "webmail_url".into(),
            Value::from(default_webmail_url(&prov_raw)),
        );
        obj.insert("status".into(), Value::from(status_raw));
        obj.insert("phone".into(), resolved_phone);
        obj.insert("protected".into(), Value::from(is_protected));
        obj.insert("assigned_computers".into(), Value::Array(Vec::new()));
        obj.insert("services".into(), Value::Array(Vec::new()));
        obj.insert("notes".into(), Value::from(notes_raw));
        emails_map.insert(email, obj);
    }

    let acc_bytes = crate::import::read_bounded(
        &secrets_dir.join(aterm_secrets::entry_class::AI_ACCOUNTS_CSV),
    )?;
    let acc_records = crate::import::parse(&acc_bytes)?;
    let mut ai_accounts_map: BTreeMap<String, Map> = BTreeMap::new();
    let mut comp_accounts: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut comp_emails: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut email_computers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut email_services: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut phone_emails: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut phone_computers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    let service_cols: &[(usize, &str, &str)] = &[
        (4, "anthropic", "claude"),
        (5, "openai", "codex"),
        (6, "google", "gemini"),
        (7, "meta", "meta"),
        (8, "kimi", "kimi"),
        (9, "xai", "grok"),
    ];

    for row in acc_records.iter().skip(1) {
        let m_col = row.first().map_or("", |s| s.trim()).to_ascii_lowercase();
        let email = row.get(1).map_or("", |s| s.trim()).to_ascii_lowercase();
        if email.is_empty() || !email.contains('@') {
            continue;
        }
        emails_map.entry(email.clone()).or_insert_with(|| {
            let domain = email.split_once('@').map_or("", |(_, d)| d);
            let prov = if domain.contains("proton") || domain == "pm.me" {
                "proton"
            } else if domain.contains("gmail") || domain.contains("google") {
                "gmail"
            } else if domain.contains("outlook") {
                "outlook"
            } else {
                "other"
            };
            let mut obj = Map::new();
            obj.insert("email".into(), Value::from(email.clone()));
            obj.insert("provider".into(), Value::from(prov));
            obj.insert("account_type".into(), Value::from("root"));
            obj.insert("root_account".into(), Value::from(email.clone()));
            obj.insert("password".into(), Value::Null);
            obj.insert("webmail_url".into(), Value::from(default_webmail_url(prov)));
            obj.insert("status".into(), Value::from("active"));
            obj.insert("phone".into(), Value::Null);
            obj.insert(
                "protected".into(),
                Value::from(is_always_protected_email(&email)),
            );
            obj.insert("assigned_computers".into(), Value::Array(Vec::new()));
            obj.insert("services".into(), Value::Array(Vec::new()));
            obj.insert("notes".into(), Value::from(""));
            obj
        });

        let mut assigned_comps = BTreeSet::new();
        for &c_idx in &[2usize, 3usize] {
            let raw_c = row.get(c_idx).map_or("", |s| s.trim());
            for token in raw_c
                .split([',', ';', ' '])
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                let clean = token.trim_start_matches('*').to_ascii_lowercase();
                let resolved = alias_to_computer
                    .get(&clean)
                    .cloned()
                    .or_else(|| {
                        clean
                            .split_once('.')
                            .and_then(|(base, _)| alias_to_computer.get(base).cloned())
                    })
                    .unwrap_or(clean);
                if computers_map.contains_key(&resolved) {
                    assigned_comps.insert(resolved);
                }
            }
        }
        for c in &assigned_comps {
            comp_emails
                .entry(c.clone())
                .or_default()
                .insert(email.clone());
            email_computers
                .entry(email.clone())
                .or_default()
                .insert(c.clone());
        }

        let notes_joined: String = row
            .iter()
            .skip(10)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("; ");
        let raw_comp_col = row.get(2).map_or("", |s| s.trim());
        let has_note = |needle: &[u8]| {
            notes_joined
                .as_bytes()
                .windows(needle.len())
                .any(|w| w.eq_ignore_ascii_case(needle))
        };
        let is_paused =
            raw_comp_col.starts_with('*') || has_note(b"deactivated") || has_note(b"disabled");
        let acc_status = if is_paused { "suspended" } else { "active" };

        let receiving_mailbox = emails_map
            .get(&email)
            .and_then(|o| o.get("root_account"))
            .and_then(Value::as_str)
            .unwrap_or(&email)
            .to_owned();

        let mut active_services: Vec<(&str, &str, String)> = Vec::new();
        for &(col_idx, prov, svc) in service_cols {
            let val = row.get(col_idx).map_or("", |s| s.trim());
            if !val.is_empty() {
                active_services.push((prov, svc, val.to_owned()));
            }
        }
        if active_services.is_empty() && m_col == "claude" {
            active_services.push(("anthropic", "claude", "1".to_owned()));
        }

        for (prov, svc, tier) in active_services {
            let id = format!("{svc}:{email}");
            email_services
                .entry(email.clone())
                .or_default()
                .insert(svc.to_owned());
            for c in &assigned_comps {
                comp_accounts
                    .entry(c.clone())
                    .or_default()
                    .insert(id.clone());
            }
            let mut obj = Map::new();
            obj.insert("id".into(), Value::from(id.clone()));
            obj.insert("provider".into(), Value::from(prov));
            obj.insert("service".into(), Value::from(svc));
            obj.insert("email".into(), Value::from(email.clone()));
            obj.insert(
                "receiving_mailbox".into(),
                Value::from(receiving_mailbox.clone()),
            );
            obj.insert(
                "assigned_computers".into(),
                Value::Array(assigned_comps.iter().cloned().map(Value::from).collect()),
            );
            obj.insert("tier".into(), Value::from(tier));
            obj.insert("status".into(), Value::from(acc_status));
            obj.insert("notes".into(), Value::from(notes_joined.clone()));
            ai_accounts_map.insert(id, obj);
        }
    }

    for (email, em_obj) in &mut emails_map {
        if let Some(comps) = email_computers.get(email) {
            em_obj.insert(
                "assigned_computers".into(),
                Value::Array(comps.iter().cloned().map(Value::from).collect()),
            );
        }
        if let Some(svcs) = email_services.get(email) {
            em_obj.insert(
                "services".into(),
                Value::Array(svcs.iter().cloned().map(Value::from).collect()),
            );
        }
        if let Some(ph) = em_obj.get("phone").and_then(Value::as_str) {
            phone_emails
                .entry(ph.to_owned())
                .or_default()
                .insert(email.clone());
            if let Some(comps) = email_computers.get(email) {
                for c in comps {
                    phone_computers
                        .entry(ph.to_owned())
                        .or_default()
                        .insert(c.clone());
                }
            }
        }
    }
    for (comp, comp_obj) in &mut computers_map {
        if let Some(accs) = comp_accounts.get(comp) {
            comp_obj.insert(
                "assigned_accounts".into(),
                Value::Array(accs.iter().cloned().map(Value::from).collect()),
            );
        }
        if let Some(ems) = comp_emails.get(comp) {
            comp_obj.insert(
                "assigned_emails".into(),
                Value::Array(ems.iter().cloned().map(Value::from).collect()),
            );
        }
    }
    for (ph, ph_obj) in &mut phones_map {
        if let Some(ems) = phone_emails.get(ph) {
            ph_obj.insert(
                "linked_emails".into(),
                Value::Array(ems.iter().cloned().map(Value::from).collect()),
            );
        }
        if let Some(comps) = phone_computers.get(ph) {
            ph_obj.insert(
                "linked_computers".into(),
                Value::Array(comps.iter().cloned().map(Value::from).collect()),
            );
        }
    }

    let doc = json!({
        "computers": computers_map.into_values().map(Value::Object).collect::<Vec<_>>(),
        "emails": emails_map.into_values().map(Value::Object).collect::<Vec<_>>(),
        "phones": phones_map.into_values().map(Value::Object).collect::<Vec<_>>(),
        "ai_accounts": ai_accounts_map.into_values().map(Value::Object).collect::<Vec<_>>(),
    });
    let mut report = import_rows(db, &doc, Existing::Keep)?;
    if !unread_workbooks.is_empty()
        && let Some(fields) = report.as_object_mut()
    {
        fields.insert("unread_workbooks".into(), json!(unread_workbooks));
        if no_passwords {
            fields.insert("ok".into(), json!(false));
        }
    }
    Ok(report)
}

fn resolve_peer_addr(dir: &Path, raw: &str) -> Result<SocketAddr> {
    let trimmed = raw.trim();
    if let Ok(addr) = trimmed.parse::<SocketAddr>() {
        return Ok(addr);
    }
    if let Ok(addr) = format!("{trimmed}:{DEFAULT_PORT}").parse::<SocketAddr>() {
        return Ok(addr);
    }
    let db = open_at(dir)?;
    let view = db.view();
    let base = trimmed.trim_end_matches(".local");
    if let Some(row) = get_row(&view, "computers", base)?
        && let Some(ip) = row
            .get("ip")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    {
        return format!("{ip}:{DEFAULT_PORT}")
            .parse::<SocketAddr>()
            .map_err(|_| Error::new("computer row has invalid ip address"));
    }
    Err(Error::new(
        "invalid peer address or computer label has no known IP in computers table",
    ))
}

fn default_peers_from_computers(dir: &Path, skip_ip: Option<std::net::IpAddr>) -> Vec<SocketAddr> {
    let Ok(db) = open_at(dir) else {
        return Vec::new();
    };
    let view = db.view();
    let Ok(rows) = scan_table(&view, "computers") else {
        return Vec::new();
    };
    let mut addrs = Vec::new();
    for row in rows.values() {
        if let Some(ip_str) = row
            .get("ip")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            && let Ok(addr) = format!("{ip_str}:{DEFAULT_PORT}").parse::<SocketAddr>()
            && skip_ip != Some(addr.ip())
            && !addrs.contains(&addr)
        {
            addrs.push(addr);
        }
    }
    addrs
}

/// Observe complete inventory rounds with one peer. An exhausted budget, deferred
/// record or transport failure is incomplete even if earlier rounds made progress.
pub(crate) fn sync_peer_with(
    peer: SocketAddr,
    mut round: impl FnMut(&mut network::Repair) -> Result<network::Report>,
) -> Value {
    let mut sent = 0usize;
    let mut received = 0usize;
    let mut deferred = 0usize;
    let mut caught_up = false;
    let mut repair = network::Repair::default();
    let mut error = None;
    for _ in 0..80 {
        match round(&mut repair) {
            Ok(report) => {
                sent += report.sent;
                received += report.received;
                deferred = report.deferred;
                if report.sent == 0 && report.received == 0 {
                    caught_up = deferred == 0;
                    break;
                }
            }
            Err(why) => {
                error = Some(why.to_string());
                break;
            }
        }
    }
    if !caught_up && error.is_none() {
        error = Some(
            if deferred != 0 {
                "peer deferred operations; sync remains incomplete"
            } else {
                "round limit reached; sync remains incomplete"
            }
            .to_owned(),
        );
    }
    let mut result = json!({
        "peer": peer.to_string(),
        "ok": caught_up,
        "sent": sent as u64,
        "received": received as u64,
        "deferred": deferred as u64,
        "caught_up": caught_up,
    });
    if let Some(error) = error {
        result
            .as_object_mut()
            .expect("sync result is an object")
            .insert("error".into(), json!(error));
    }
    result
}

/// Sync the fleet database at `dir` with `peers` over `aterm-net` TLS. Success
/// requires a settled inventory round with every requested peer; it does not
/// establish convergence with other peers or writes made after that observation.
pub fn sync_peers(dir: &Path, peers: &[SocketAddr]) -> Result<Value> {
    if peers.is_empty() {
        return Err(Error::new("sync requires at least one peer"));
    }
    while_busy(|| ensure_fleet_at(dir))?;
    let results: Vec<_> = peers
        .iter()
        .map(|peer| {
            sync_peer_with(*peer, |repair| {
                network::sync_fleet_round_with(dir, *peer, repair)
            })
        })
        .collect();
    Ok(json!({
        "ok": results.iter().all(|r| r.get("ok").and_then(Value::as_bool) == Some(true)),
        "peers": results,
    }))
}

/// Serve the fleet database at `dir` on `listen` (defaulting to `0.0.0.0:7485` so multi-homed
/// fleet Macs accept peers on all interfaces).
pub fn serve_fleet(
    dir: &Path,
    listen: Option<SocketAddr>,
    mut peers: Vec<SocketAddr>,
) -> Result<()> {
    while_busy(|| ensure_fleet_at(dir))?;
    let bind_addr = listen.unwrap_or_else(|| format!("0.0.0.0:{DEFAULT_PORT}").parse().unwrap());
    let listener = aterm_uds::kept_out(|| std::net::TcpListener::bind(bind_addr))?;
    if peers.is_empty() {
        let local_ip = listener
            .local_addr()
            .ok()
            .map(|a| a.ip())
            .filter(|ip| !ip.is_unspecified());
        peers = default_peers_from_computers(dir, local_ip);
    }
    let _ = writeln!(
        std::io::stderr(),
        "aterm db: serving the fleet tables on {bind_addr}, dialing {} peers",
        peers.len()
    );
    network::serve_fleet_until(
        dir,
        listener,
        std::sync::Arc::new(aterm_uds::stop::Stop::new()),
        std::sync::Arc::new(std::sync::Mutex::new(peers)),
    )
}

/// Dispatch fleet relational database commands when invoked via `aterm db <verb> ...`.
/// Returns `Ok(None)` when `words` belongs to a legacy `--dir` command.
pub fn try_run(words: &[String]) -> Result<Option<Value>> {
    try_run_with(words, default_dir)
}

/// [`try_run`] with the replica folder a verb without `--dir` uses: [`default_dir`], or a test's.
pub(crate) fn try_run_with(
    words: &[String],
    default_dir: impl FnOnce() -> Result<PathBuf>,
) -> Result<Option<Value>> {
    let Some(cmd) = words.first().map(String::as_str) else {
        return Ok(None);
    };
    let has_dir_flag = words.iter().any(|w| w == "--dir");
    let is_fleet_verb = matches!(
        cmd,
        "tables"
            | "computers"
            | "emails"
            | "phones"
            | "ai-accounts"
            | "ai_accounts"
            | "delete"
            | "dump"
            | "load"
            | "bootstrap"
            | "rekey"
    );
    let is_positional_crud = !has_dir_flag
        && matches!(
            cmd,
            "get" | "put" | "scan" | "sync" | "serve" | "status" | "history"
        );
    if !is_fleet_verb && !is_positional_crud {
        return Ok(None);
    }

    let mut dir = default_dir()?;
    let mut show_passwords = false;
    let mut args = Vec::new();
    let mut i = 1;
    while i < words.len() {
        match words[i].as_str() {
            "--dir" => {
                let Some(d) = words.get(i + 1) else {
                    return Err(Error::new("--dir requires a path"));
                };
                dir = PathBuf::from(d);
                i += 2;
            }
            "--show-passwords" | "--reveal" => {
                show_passwords = true;
                i += 1;
            }
            other => {
                args.push(other.to_owned());
                i += 1;
            }
        }
    }

    // Once this Mac's own import recorded the replica covered, its rows live in the registry and
    // these writes would edit a copy no one reads (one-mesh design §4.7, A7).
    if matches!(cmd, "put" | "delete" | "load" | "bootstrap" | "rekey") {
        crate::alab::import::covered::refuse_writes(&dir)?;
    }
    // Ahead of a verb's own usage, so every spelling names the rekey. A round refuses for itself.
    if !matches!(cmd, "rekey" | "sync" | "serve") {
        refuse_unfinished(&dir)?;
    }
    match cmd {
        "tables" | "status" => {
            if !args.is_empty() {
                return Err(Error::new("usage: aterm db tables"));
            }
            let db = open_at(&dir)?;
            Ok(Some(tables_summary(&db)?))
        }
        "history" => {
            if !args.is_empty() {
                return Err(Error::new("usage: aterm db history"));
            }
            let db = open_at(&dir)?;
            let ops: Vec<Value> = db
                .history
                .index()
                .inventory()
                .filter_map(|id| db.provenance(id))
                .collect();
            Ok(Some(json!({ "operations": ops })))
        }
        "computers" | "emails" | "phones" | "ai-accounts" | "ai_accounts" => {
            if args.len() > 1 || args.first().is_some_and(|a| a.starts_with("--")) {
                return Err(Error::new(
                    "unexpected arguments: expected optional [<key>] [--show-passwords]",
                ));
            }
            let (logical, _) = resolve_table(cmd)?;
            let db = open_at(&dir)?;
            let view = db.view();
            if let Some(key) = args.first() {
                let row = get_row(&view, logical, key)?
                    .ok_or_else(|| Error::new("row not found in table"))?;
                Ok(Some(redact_row(logical, row, show_passwords)))
            } else {
                let rows: Vec<Value> = scan_table(&view, logical)?
                    .into_values()
                    .map(|r| redact_row(logical, r, show_passwords))
                    .collect();
                Ok(Some(json!({
                    "table": logical,
                    "count": rows.len(),
                    "rows": rows,
                })))
            }
        }
        "scan" => {
            let table = match args.as_slice() {
                [] => "computers",
                [t] if !t.starts_with("--") => t.as_str(),
                [flag, t] if flag == "--collection" => t.as_str(),
                _ => {
                    return Err(Error::new(
                        "usage: aterm db scan [<table>] [--show-passwords]",
                    ));
                }
            };
            let (logical, _) = resolve_table(table)?;
            let db = open_at(&dir)?;
            let view = db.view();
            let rows: Vec<Value> = scan_table(&view, logical)?
                .into_values()
                .map(|r| redact_row(logical, r, show_passwords))
                .collect();
            Ok(Some(json!({
                "table": logical,
                "count": rows.len(),
                "rows": rows,
            })))
        }
        "get" => {
            let [table, key] = args.as_slice() else {
                return Err(Error::new(
                    "usage: aterm db get <table> <key> [--show-passwords]",
                ));
            };
            let (logical, _) = resolve_table(table)?;
            let db = open_at(&dir)?;
            let view = db.view();
            let row = get_row(&view, logical, key)?
                .ok_or_else(|| Error::new("row not found in table"))?;
            Ok(Some(redact_row(logical, row, show_passwords)))
        }
        "put" => {
            let [table, key, raw_json] = args.as_slice() else {
                return Err(Error::new("usage: aterm db put <table> <key> <json>"));
            };
            let (logical, _) = resolve_table(table)?;
            let value: Value = aterm_json::from_slice(raw_json.as_bytes())
                .map_err(|_| Error::new("row value must be valid JSON"))?;
            let mut db = open_at(&dir)?;
            Ok(Some(put_row(&mut db, logical, key, value)?))
        }
        "delete" => {
            let [table, key] = args.as_slice() else {
                return Err(Error::new("usage: aterm db delete <table> <key>"));
            };
            let (logical, _) = resolve_table(table)?;
            let mut db = open_at(&dir)?;
            Ok(Some(delete_row(&mut db, logical, key)?))
        }
        "dump" => {
            if args.len() > 1 || args.first().is_some_and(|a| a.starts_with("--")) {
                return Err(Error::new(
                    "usage: aterm db dump [<file>] [--show-passwords]",
                ));
            }
            let db = open_at(&dir)?;
            if let Some(out_path) = args.first() {
                let doc = dump_all(&db, true)?;
                let bytes = aterm_json::to_vec(&doc)
                    .map_err(|_| Error::new("failed to encode snapshot JSON"))?;
                let p = Path::new(out_path);
                let parent = p
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                let fname = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or_else(|| Error::new("invalid dump file path"))?;
                aterm_uds::private_file::write_private(parent, fname, &bytes)?;
                Ok(Some(json!({ "ok": true, "file": out_path })))
            } else {
                Ok(Some(dump_all(&db, show_passwords)?))
            }
        }
        "load" => {
            let [file_path] = args.as_slice() else {
                return Err(Error::new("usage: aterm db load <file>"));
            };
            // A user-named snapshot may be a stream (`/dev/stdin`, `<(…)`), so not regular-only.
            let bytes = aterm_uds::atomic_file::read_bounded_stream(
                Path::new(file_path),
                64 * 1024 * 1024,
            )?;
            let doc: Value = aterm_json::from_slice(&bytes)
                .map_err(|_| Error::new("snapshot file must be valid JSON"))?;
            let mut db = open_at(&dir)?;
            Ok(Some(load_all(&mut db, &doc)?))
        }
        "bootstrap" => {
            if args.len() > 1 || args.first().is_some_and(|a| a.starts_with("--")) {
                return Err(Error::new("usage: aterm db bootstrap [<secrets-dir>]"));
            }
            let secrets_dir = if let Some(s) = args.first() {
                PathBuf::from(s)
            } else {
                let home = aterm_types::dirs::home_dir()
                    .ok_or_else(|| Error::new("HOME directory unavailable"))?;
                home.join(".secrets")
            };
            let mut db = open_at(&dir)?;
            Ok(Some(bootstrap_from_secrets(&mut db, &secrets_dir)?))
        }
        "rekey" => {
            if !args.is_empty() {
                return Err(Error::new("usage: aterm db rekey"));
            }
            Ok(Some(rekey_at(&dir)?))
        }
        "sync" => {
            let mut peers = Vec::new();
            let mut idx = 0;
            while idx < args.len() {
                let word = if args[idx] == "--peer" || args[idx] == "--peers" {
                    idx += 1;
                    args.get(idx)
                        .ok_or_else(|| Error::new("--peer requires <ip:port|label>"))?
                } else if args[idx].starts_with("--") {
                    return Err(Error::new(
                        "unknown flag for sync: expected <peer[:port]|label> or --peer <peer>",
                    ));
                } else {
                    &args[idx]
                };
                for part in word.split(',').filter(|s| !s.trim().is_empty()) {
                    let addr = resolve_peer_addr(&dir, part)?;
                    if !peers.contains(&addr) {
                        peers.push(addr);
                    }
                }
                idx += 1;
            }
            if peers.is_empty() {
                return Err(Error::new(
                    "usage: aterm db sync <peer-ip[:port]|computer-label> (or --peer <ip:port>)",
                ));
            }
            Ok(Some(sync_peers(&dir, &peers)?))
        }
        "serve" => {
            let mut listen: Option<SocketAddr> = None;
            let mut peers = Vec::new();
            let mut idx = 0;
            while idx < args.len() {
                match args[idx].as_str() {
                    "--listen" => {
                        idx += 1;
                        let w = args
                            .get(idx)
                            .ok_or_else(|| Error::new("--listen requires <ip:port>"))?;
                        listen = Some(
                            w.parse()
                                .map_err(|_| Error::new("invalid listen address"))?,
                        );
                    }
                    "--peers" | "--peer" => {
                        idx += 1;
                        let w = args
                            .get(idx)
                            .ok_or_else(|| Error::new("--peers requires <ip:port|label,...>"))?;
                        for part in w.split(',').filter(|s| !s.trim().is_empty()) {
                            let addr = resolve_peer_addr(&dir, part)?;
                            if !peers.contains(&addr) {
                                peers.push(addr);
                            }
                        }
                    }
                    _ => {
                        return Err(Error::new(
                            "usage: aterm db serve [--listen <ip:port>] [--peers <ip:port,...>]",
                        ));
                    }
                }
                idx += 1;
            }
            serve_fleet(&dir, listen, peers)?;
            Ok(Some(json!({ "stopped": true })))
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
#[path = "fleet_db_tests.rs"]
mod tests;
