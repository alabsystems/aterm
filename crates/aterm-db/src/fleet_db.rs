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

/// SHA-256 digests of the 3 personal email addresses that must always remain `protected: true`
/// and never store a password in any operation.
const PROTECTED_EMAIL_DIGESTS: [[u8; 32]; 3] = [
    [
        0x45, 0x1d, 0xba, 0x8f, 0x26, 0x17, 0x58, 0xf5, 0x48, 0x6f, 0x3e, 0x97, 0x35, 0xec, 0xe9,
        0x08, 0xe6, 0xe7, 0xcb, 0xcf, 0xd6, 0x7d, 0x79, 0x55, 0xbd, 0x31, 0x99, 0x04, 0xa4, 0xb9,
        0x18, 0x3e,
    ],
    [
        0x69, 0x77, 0x11, 0xa4, 0x89, 0x55, 0xca, 0xfd, 0x0a, 0xad, 0x70, 0xe2, 0xb4, 0x04, 0xd6,
        0x0c, 0xf3, 0xb9, 0x94, 0xe2, 0xe3, 0x8b, 0xd2, 0x22, 0x1b, 0xbd, 0xeb, 0x78, 0xbf, 0xd5,
        0xc2, 0x0b,
    ],
    [
        0x6a, 0xe3, 0x1e, 0xb5, 0x38, 0xf4, 0x7c, 0x79, 0xac, 0xcd, 0x2a, 0x37, 0x58, 0x3c, 0x7c,
        0xb1, 0xc7, 0xa5, 0x49, 0x29, 0x57, 0x5a, 0x5e, 0x3f, 0x9b, 0x49, 0x71, 0xac, 0x1d, 0xbd,
        0xb4, 0x8f,
    ],
];

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

fn with_normalized_phone_key<R>(raw: &str, f: impl FnOnce(&str) -> R) -> R {
    let trimmed = raw.trim();
    let mut digits = [0u8; 12];
    let mut len = 0;
    for b in trimmed.bytes().filter(u8::is_ascii_digit) {
        if len == digits.len() {
            return f(trimmed);
        }
        digits[len] = b;
        len += 1;
    }
    let ten = if len == 10 {
        Some(&digits[..10])
    } else if len == 11 && digits[0] == b'1' {
        Some(&digits[1..11])
    } else {
        None
    };
    if let Some(d) = ten {
        let buf = [
            d[0], d[1], d[2], b'-', d[3], d[4], d[5], b'-', d[6], d[7], d[8], d[9],
        ];
        let s = std::str::from_utf8(&buf).expect("ASCII digits and hyphens");
        f(s)
    } else {
        f(trimmed)
    }
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

fn sha256_ascii_lowercase(bytes: &[u8]) -> [u8; 32] {
    if !bytes.iter().any(u8::is_ascii_uppercase) {
        return Sha256::digest(bytes);
    }
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 256];
    for chunk in bytes.chunks(buf.len()) {
        for (dst, &src) in buf[..chunk.len()].iter_mut().zip(chunk) {
            *dst = src.to_ascii_lowercase();
        }
        hasher.update(&buf[..chunk.len()]);
    }
    hasher.finalize()
}

/// Convert a natural primary key (`m104`, `root@example.com`, `+1-555-234-5678`,
/// `acc-001`) into a uniform `crypto::name`-safe storage key (`<prefix>-<24 hex chars>`).
pub fn storage_key(logical_table: &str, natural_key: &str) -> Result<String> {
    let trimmed = natural_key.trim();
    if trimmed.is_empty() {
        return Err(Error::new("table primary key must not be empty"));
    }
    let prefix = match logical_table {
        "computers" => "mc",
        "emails" => "em",
        "phones" => "ph",
        "ai_accounts" => "ac",
        _ => "rk",
    };
    let digest = if logical_table == "phones" {
        with_normalized_phone_key(trimmed, |s| sha256_ascii_lowercase(s.as_bytes()))
    } else {
        sha256_ascii_lowercase(trimmed.as_bytes())
    };
    let mut out = String::with_capacity(prefix.len() + 1 + 24);
    out.push_str(prefix);
    out.push('-');
    hex::push_lower(&mut out, &digest[..12]);
    Ok(out)
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

/// Return true if `email` is one of the 3 personal email addresses that must always be `protected: true`.
pub fn is_always_protected_email(email: &str) -> bool {
    let digest = sha256_ascii_lowercase(email.trim().as_bytes());
    PROTECTED_EMAIL_DIGESTS.contains(&digest)
}

/// Validate a decoded `"emails"` record inside `Command::decode` so neither local writes nor
/// remote peers can ever bypass the `protected: true` no-password invariant.
pub fn validate_email_record(stored_key: &str, value: &Value) -> Result<()> {
    let obj = value
        .as_object()
        .ok_or_else(|| Error::new("emails record must be a JSON object"))?;
    if let Some(del) = obj.get("_deleted") {
        if del.as_bool() == Some(true) && obj.len() == 1 {
            return Ok(());
        }
        return Err(Error::new(
            "emails tombstone must contain only \"_deleted\": true",
        ));
    }
    let email = obj
        .get("email")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::new("emails record requires string \"email\" field"))?;
    let trimmed = email.trim();
    if trimmed.is_empty()
        || !trimmed.contains('@')
        || trimmed.len() != email.len()
        || trimmed.bytes().any(|b| b.is_ascii_uppercase())
    {
        return Err(Error::new(
            "emails record requires lowercase email address containing '@'",
        ));
    }
    let expected_skey = storage_key("emails", trimmed)?;
    if stored_key != expected_skey {
        return Err(Error::new(
            "emails storage key does not match canonical email address",
        ));
    }
    let protected = obj
        .get("protected")
        .and_then(Value::as_bool)
        .ok_or_else(|| Error::new("emails record requires boolean \"protected\" field"))?;
    if is_always_protected_email(trimmed) && !protected {
        return Err(Error::new(
            "protected personal email address cannot have protected set to false",
        ));
    }
    if let Some(pw) = obj.get("password")
        && !pw.is_null()
        && pw.as_str().is_none()
    {
        return Err(Error::new(
            "emails \"password\" field must be a string or null",
        ));
    }
    if protected && credential_fields(obj) {
        return Err(Error::new(
            "protected real email rows must not store credential fields",
        ));
    }
    Ok(())
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
    match row.get("password").and_then(Value::as_str).map(str::trim) {
        None | Some("") => false,
        Some(cell) => crate::typed::secret::flag_says_str(cell).unwrap_or(true),
    }
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
    if cell.candidates.len() == 1 && !any_protected {
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
        if any_protected && !is_tombstone(&candidate.value) {
            let mut stored = candidate.value.clone();
            drop_read_fields(logical_table, &mut stored);
            if claims_a_password(&stored)
                && let Some(obj) = shown.as_object_mut()
            {
                obj.insert("password".into(), Value::from(HAS_PASSWORD));
            }
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

/// Whether a field carries credentials rather than an account identifier or secret reference.
fn credential_field(key: &str) -> bool {
    const SUFFIXES: &[&str] = &[
        "password",
        "passwords",
        "passwd",
        "passwds",
        "passphrase",
        "passphrases",
        "secret",
        "secrets",
        "token",
        "tokens",
        "apikey",
        "apikeys",
        "privatekey",
        "privatekeys",
        "credential",
        "credentials",
        "authorization",
        "authorizations",
        "authheader",
        "authheaders",
        "cookie",
        "cookies",
        "totp",
        "otp",
        "seed",
        "seeds",
        "authenticator",
        "recoverycode",
        "recoverycodes",
        "backupcode",
        "backupcodes",
    ];
    let mut buf = [0u8; 16];
    let mut start = buf.len();
    for b in key.bytes().rev().filter(u8::is_ascii_alphanumeric).take(16) {
        start -= 1;
        buf[start] = b.to_ascii_lowercase();
    }
    let tail = &buf[start..];
    SUFFIXES
        .iter()
        .any(|suffix| tail.ends_with(suffix.as_bytes()))
}

fn sensitive_field(key: &str, value: &Value) -> bool {
    if value.is_null() || (key == "has_password" && value.as_bool().is_some()) {
        return false;
    }
    credential_field(key)
}

fn credential_fields(object: &Map) -> bool {
    object
        .iter()
        .any(|(key, value)| sensitive_field(key, value) || contains_credentials(value))
}

fn contains_credentials(value: &Value) -> bool {
    match value {
        Value::Object(object) => credential_fields(object),
        Value::Array(values) => values.iter().any(contains_credentials),
        _ => false,
    }
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
                if sensitive_field(key, value) {
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
    if logical_table == "emails"
        && let Some(obj) = row.as_object_mut()
    {
        let has_pw = obj
            .get("password")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty());
        obj.insert("has_password".into(), Value::Bool(has_pw));
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
        let alone = Command::Write {
            request: [0; 16],
            changes: vec![change],
        };
        let alone_len = alone.encode()?.len();
        if alone_len > crypto::MAX_BODY {
            return Err(Error::new(
                "single row exceeds maximum operation payload limit",
            ));
        }
        let change_bytes = alone_len.saturating_sub(67);
        let Command::Write { mut changes, .. } = alone else {
            unreachable!()
        };
        let change = changes.pop().expect("single change");
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
    let operations = db.history.index.inventory().count() as u64;
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
    for &logical in TABLES {
        let (_, collection) = resolve_table(logical)?;
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
                // A key that stands is not written, so what is stored there decides nothing here.
                let stands = matches!(existing, Existing::Keep)
                    && view
                        .cells
                        .contains_key(&(collection.to_owned(), skey.clone()));
                if !stands {
                    inherit_email_state(view, &skey, &mut item_val)?;
                }
            }
            let normalized = normalize_row(logical, &nat, item_val)?;
            let final_nat = natural_key_of(logical, &nat, &normalized);
            let skey = storage_key(logical, &final_nat)?;
            let key_pair = (collection.to_owned(), skey);
            if !seen_keys.insert(key_pair.clone()) {
                return Err(Error::new(
                    "snapshot document contains duplicate primary keys in table",
                ));
            }
            let cell = view.cells.get(&key_pair);
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
                collection: key_pair.0,
                key: key_pair.1,
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
const REKEY_UNFINISHED: &str = "rekey-unfinished";

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
fn read_xlsx_email_passwords(secrets_dir: &Path) -> BTreeMap<String, Zeroizing<String>> {
    let mut out = BTreeMap::new();
    for filename in ["alab-emails.xlsx", "AI Email Accounting.xlsx"] {
        let path = secrets_dir.join(filename);
        let Ok((bytes, _)) = aterm_uds::atomic_file::read_regular(&path, 64 * 1024 * 1024) else {
            continue;
        };
        let bytes = Zeroizing::new(bytes);
        let Ok(mut records) = crate::alab::import::xlsx::tab(&bytes, "Emails") else {
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
    out
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
    let phones_bytes = crate::import::read_bounded(&secrets_dir.join("alab-phones.csv"))?;
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

    let comp_bytes = crate::import::read_bounded(&secrets_dir.join("alab-computers.csv"))?;
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

    let xlsx_passwords = read_xlsx_email_passwords(secrets_dir);
    let emails_bytes = crate::import::read_bounded(&secrets_dir.join("alab-emails.csv"))?;
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

    let acc_bytes = crate::import::read_bounded(&secrets_dir.join("ai-accounts.csv"))?;
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
    import_rows(db, &doc, Existing::Keep)
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
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
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
                .index
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
            let (bytes, _) =
                aterm_uds::atomic_file::read_regular(Path::new(file_path), 64 * 1024 * 1024)?;
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
mod tests {
    use super::*;

    const SECRET: &[u8] = b"1111111111111111111111111111111111111111";
    const ROTATED: &[u8] = b"2222222222222222222222222222222222222222";

    /// A replica of `SECRET`'s dataset at `dir`.
    fn replica(dir: &Path) -> Database {
        local::ensure_fleet(dir, SECRET).expect("ensure_fleet");
        Database::open(dir).expect("open")
    }

    /// Carry every retained operation both ways, as a sync does.
    fn exchange(a: &mut Database, b: &mut Database) {
        let carry = |from: &Database, to: &mut Database| {
            let wires: Vec<Vec<u8>> = from
                .history
                .index
                .inventory()
                .map(|id| from.history.wire(id).expect("wire").to_vec())
                .collect();
            for wire in wires {
                to.ingest(&wire).expect("ingest");
            }
        };
        carry(a, b);
        carry(b, a);
    }

    fn ok_of(db: &Database) -> Value {
        tables_summary(db).expect("summary")["ok"].clone()
    }

    #[test]
    fn the_key_is_read_from_the_folder_the_fleet_writes_it_to() {
        let state = aterm_types::dirs::state_dir().expect("state dir");
        aterm_uds::private_file::create_private_dir(&state).unwrap();
        aterm_uds::private_file::write_private(&state, "fleet.key", SECRET).unwrap();
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        assert_eq!(
            resolve_fleet_secret(&tmp.path().join("db")).unwrap(),
            SECRET
        );
    }

    #[test]
    fn ordinary_reads_redact_credentials_in_every_table_and_nested_candidate() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut a = replica(&root.path().join("a"));
        let mut b = replica(&root.path().join("b"));
        for (table, key) in [
            ("computers", "m1"),
            ("emails", "root@example.com"),
            ("phones", "216-555-0100"),
            ("ai_accounts", "account-1"),
        ] {
            let raw = json!({
                "password": "fixture-password",
                "has_password": "fixture-mislabelled-secret",
                "app_password": "fixture-app-password",
                "webmailPassword": "fixture-webmail-password",
                "oauth_token": "fixture-oauth-token",
                "private-key": "fixture-private-key",
                "googleAPIKeys": ["fixture-prefixed-api-key"],
                "ssh_private_keys": ["fixture-prefixed-private-key"],
                "imapPasswd": "fixture-prefixed-passwd",
                "requestAuthorization": "fixture-prefixed-authorization",
                "clientAuthHeaders": ["fixture-prefixed-auth-header"],
                "browserCookies": ["fixture-prefixed-cookie"],
                "totp": "fixture-totp",
                "totp_seed": "fixture-totp-seed",
                "authenticatorSeed": "fixture-authenticator-seed",
                "otp_seed": "fixture-otp-seed",
                "googleRecoveryCodes": ["fixture-recovery-code"],
                "backup_codes": ["fixture-backup-code"],
                "oauth": { "access_token": "fixture-access", "refreshToken": "fixture-refresh", "token_count": 7 },
                "clients": [{ "client_secret": "fixture-client-secret", "api-key": "fixture-api-key" }],
                "secret_ref": { "vault": "vault-id", "entry": "entry-id" },
                "notes": "visible noncredential metadata"
            });
            put_row(&mut a, table, key, raw).unwrap();
        }
        exchange(&mut a, &mut b);
        // Two causally concurrent versions must both be redacted, including the losing candidate.
        for (db, password) in [(&mut a, "fixture-a-only"), (&mut b, "fixture-b-only")] {
            put_row(
                db,
                "ai_accounts",
                "account-1",
                json!({"password":password,"has_password":password,"oauth":{"access_token":password}}),
            )
            .unwrap();
        }
        exchange(&mut a, &mut b);
        let shown = dump_all(&a, false).unwrap();
        let encoded = aterm_json::to_string(&shown).unwrap();
        assert!(
            !encoded.contains("fixture-"),
            "ordinary output exposed credential contents"
        );
        for table in TABLES {
            let row = &shown[*table][0];
            assert_eq!(row["password"], json!(REDACTED));
            if *table != "ai_accounts" {
                assert_eq!(row["app_password"], json!(REDACTED));
                assert_eq!(row["webmailPassword"], json!(REDACTED));
                assert_eq!(row["oauth_token"], json!(REDACTED));
                assert_eq!(row["private-key"], json!(REDACTED));
                assert_eq!(row["oauth"]["access_token"], json!(REDACTED));
                assert_eq!(row["oauth"]["refreshToken"], json!(REDACTED));
                assert_eq!(row["clients"][0]["client_secret"], json!(REDACTED));
                assert_eq!(row["clients"][0]["api-key"], json!(REDACTED));
                assert_eq!(row["secret_ref"]["entry"], json!("entry-id"));
                assert_eq!(row["oauth"]["token_count"], json!(7));
            }
        }
        assert_eq!(shown["emails"][0]["has_password"], json!(true));
        assert!(
            shown["ai_accounts"][0]["_conflicts"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty())
        );
        let retained = aterm_json::to_string(&dump_all(&a, true).unwrap()).unwrap();
        assert!(retained.contains("fixture-a-only"));
        assert!(retained.contains("fixture-b-only"));
        let before: BTreeSet<_> = a.history.index.inventory().copied().collect();
        assert!(
            load_all(&mut a, &shown).is_err(),
            "a redacted export cannot overwrite retained credentials"
        );
        assert_eq!(before, a.history.index.inventory().copied().collect());
        let after = aterm_json::to_string(&dump_all(&a, true).unwrap()).unwrap();
        assert_eq!(retained, after);
    }

    #[test]
    fn protected_email_validation_rejects_nested_credential_fields_at_ingestion() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut db = replica(&root.path().join("a"));
        let email = "protected-owner@example.com";
        let key = storage_key("emails", email).unwrap();
        let metadata = json!({
            "email": email, "protected": true,
            "has_password": false,
            "secret_ref": {"vault": "vault-id", "entry": "entry-id"},
            "metadata": {"token_count": 7, "password_changed_at": 2000}
        });
        put_row(&mut db, "emails", email, metadata.clone()).unwrap();
        for field in [
            "password",
            "app-password",
            "mailPasswd",
            "googleAPIKeys",
            "ssh_private_keys",
            "requestAuthorization",
            "authHeaders",
            "browserCookies",
            "backupTokens",
            "totp",
            "totp_seed",
            "authenticatorSeed",
            "otp_seed",
            "googleRecoveryCodes",
            "backup_codes",
            "has_password",
        ] {
            for nested in [false, true] {
                let mut row = metadata.clone();
                if nested {
                    row.as_object_mut()
                        .unwrap()
                        .insert("extensions".into(), json!([{field: "fixture-sensitive"}]));
                } else {
                    row.as_object_mut()
                        .unwrap()
                        .insert(field.into(), json!("fixture-sensitive"));
                }
                let before = db.history.index.inventory().count();
                let written = put_row(&mut db, "emails", email, row.clone());
                if field == "has_password" && !nested {
                    assert!(
                        written.is_ok(),
                        "local metadata is discarded before storage"
                    );
                } else {
                    assert!(written.is_err(), "local {field} nested={nested}");
                    assert_eq!(
                        before,
                        db.history.index.inventory().count(),
                        "refused credentials must not enter history"
                    );
                }
                let bytes = Command::Write {
                    request: [1; 16],
                    changes: vec![Change {
                        collection: "emails".into(),
                        key: key.clone(),
                        value: Some(row),
                    }],
                }
                .encode()
                .unwrap();
                assert!(
                    Command::decode(&bytes).is_err(),
                    "wire {field} nested={nested}"
                );
            }
        }
        let retained = get_row(&db.view(), "emails", email).unwrap().unwrap();
        assert_eq!(retained["secret_ref"], metadata["secret_ref"]);
        assert!(
            !aterm_json::to_string(&retained)
                .unwrap()
                .contains("fixture-sensitive")
        );
        assert_eq!(
            redact_row("emails", retained, false)["has_password"],
            json!(false)
        );
    }

    #[test]
    fn a_rotated_fleet_key_moves_the_rows_and_keeps_the_old_folder() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("db");
        let mut db = replica(&dir);
        put_row(
            &mut db,
            "computers",
            "m1",
            json!({"label": "m1", "notes": "n"}),
        )
        .unwrap();
        put_row(
            &mut db,
            "emails",
            "root@example.com",
            json!({"email": "root@example.com", "password": "pw"}),
        )
        .unwrap();
        drop(db);

        let refused = local::ensure_fleet(&dir, ROTATED).unwrap_err();
        assert!(refused.to_string().contains("aterm db rekey"), "{refused}");

        let report = rekey_with(&dir, ROTATED).unwrap();
        let report = &report["moved"][0];
        assert_eq!(report["imported"]["computers"], json!(1));
        local::ensure_fleet(&dir, ROTATED).expect("the folder now holds the rotated dataset");
        let moved = Database::open(&dir).unwrap();
        assert_eq!(
            get_row(&moved.view(), "emails", "root@example.com")
                .unwrap()
                .unwrap()["password"],
            json!("pw")
        );
        drop(moved);

        let kept = PathBuf::from(report["kept"].as_str().unwrap());
        let old = Database::open(&kept).expect("the old folder still opens");
        assert!(get_row(&old.view(), "computers", "m1").unwrap().is_some());
        drop(old);

        assert!(rekey_with(&dir, ROTATED).is_err(), "nothing left to move");
    }

    #[test]
    fn a_rekey_cut_short_after_the_rename_finishes_when_run_again() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        for (name, partly_loaded) in [("gone", false), ("partly", true)] {
            let dir = tmp.path().join(name);
            let mut db = replica(&dir);
            put_row(&mut db, "computers", "m1", json!({"label": "m1"})).unwrap();
            put_row(&mut db, "computers", "m2", json!({"label": "m2"})).unwrap();
            let id = hex::encode_lower(&db.credentials.manifest.id[..8]);
            drop(db);

            // What a process killed after the rename, or part way through the load, leaves.
            aterm_uds::private_file::write_private(&dir, REKEY_UNFINISHED, b"").unwrap();
            let kept = dir.with_file_name(format!("{name}.before-rekey-{id}"));
            std::fs::rename(&dir, &kept).unwrap();
            if partly_loaded {
                local::ensure_fleet(&dir, ROTATED).unwrap();
                let mut db = Database::open(&dir).unwrap();
                put_row(
                    &mut db,
                    "computers",
                    "m1",
                    json!({"label": "m1", "notes": "since"}),
                )
                .unwrap();
            }

            let report = rekey_with(&dir, ROTATED).unwrap();
            assert_eq!(
                report["moved"][0]["kept"],
                json!(kept.display().to_string())
            );
            let db = Database::open(&dir).unwrap();
            let rows = scan_table(&db.view(), "computers").unwrap();
            assert_eq!(rows.keys().collect::<Vec<_>>(), ["m1", "m2"]);
            if partly_loaded {
                assert_eq!(rows["m1"]["notes"], json!("since"), "an edit since stands");
            }
            drop(db);
            assert!(rekey_with(&dir, ROTATED).is_err(), "nothing left to move");
        }
    }

    #[test]
    fn no_verb_serves_a_dataset_a_cut_short_rekey_is_still_moving() {
        let tmp = fleet_fixture();
        let dir = tmp.path().join("db");
        let secret = resolve_fleet_secret(&dir).unwrap();
        let mut db = open_at(&dir).unwrap();
        put_row(&mut db, "computers", "m1", json!({"label": "m1"})).unwrap();
        let id = hex::encode_lower(&db.credentials.manifest.id[..8]);
        drop(db);
        aterm_uds::private_file::write_private(&dir, REKEY_UNFINISHED, b"").unwrap();
        let kept = dir.with_file_name(format!("db.before-rekey-{id}"));
        std::fs::rename(&dir, &kept).unwrap();

        let refusal = open_at(&dir)
            .err()
            .expect("an empty dataset is not the answer");
        assert!(refusal.0.contains("aterm db rekey"), "{}", refusal.0);
        assert!(!dir.exists(), "and nothing was created in its place");

        rekey_with(&dir, &secret).unwrap();
        let db = open_at(&dir).unwrap();
        assert_eq!(scan_table(&db.view(), "computers").unwrap().len(), 1);
    }

    #[test]
    fn a_stored_placeholder_does_not_stop_a_rekey_and_moves_as_a_word() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("db");
        let mut db = replica(&dir);
        let key = "root@example.com";
        db.commit(Command::Write {
            request: crypto::random::<16>().unwrap(),
            changes: vec![Change {
                collection: "emails".into(),
                key: storage_key("emails", key).unwrap(),
                value: Some(json!({"email": key, "protected": false, "password": REDACTED})),
            }],
        })
        .unwrap();
        drop(db);

        rekey_with(&dir, ROTATED).unwrap();
        let moved = Database::open(&dir).unwrap();
        let row = get_row(&moved.view(), "emails", key).unwrap().unwrap();
        assert_eq!(row["password"], json!(HAS_PASSWORD));
    }

    #[test]
    fn a_control_record_on_a_replica_a_rotation_left_unmarked_does_not_empty_the_rekey() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let victim = tmp.path().join("victim");
        let mut db = replica(&victim);
        put_row(&mut db, "computers", "m1", json!({"label": "m1"})).unwrap();
        drop(db);
        local::test_forget_no_authority(&victim).unwrap();
        let mut unmarked = Database::open(&victim).unwrap();
        for name in ["forger-a", "forger-b"] {
            let forger = tmp.path().join(name);
            local::test_forge_authority(&forger, SECRET).unwrap();
            let mut forger = Database::open(&forger).unwrap();
            let actor = forger.credentials.capability.actor;
            let id = forger
                .commit(Command::Budget {
                    request: crypto::random::<16>().unwrap(),
                    name: name.to_owned(),
                    total: 1,
                    shares: BTreeMap::from([(actor, 1)]),
                })
                .unwrap();
            unmarked.ingest(forger.history.wire(&id).unwrap()).unwrap();
        }
        assert!(unmarked.view().authority_fault);
        drop(unmarked);

        let report = rekey_with(&victim, ROTATED).unwrap();
        assert_eq!(report["moved"][0]["imported"]["computers"], json!(1));
    }

    #[test]
    fn a_row_put_accepted_moves_with_a_rekey() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("db");
        let mut db = replica(&dir);
        put_row(&mut db, "computers", "m1", json!({"label": "m1"})).unwrap();
        let key = "big@example.com";
        let notes = "x".repeat(50_000);
        put_row(
            &mut db,
            "emails",
            key,
            json!({"email": key, "notes": notes}),
        )
        .unwrap();
        put_row(&mut db, "phones", "555-234-5678", json!({})).unwrap();
        drop(db);

        rekey_with(&dir, ROTATED).unwrap();
        let db = Database::open(&dir).unwrap();
        let summary = tables_summary(&db).unwrap();
        assert_eq!(summary["tables"]["computers"], json!(1));
        assert_eq!(summary["tables"]["emails"]["total"], json!(1));
        assert_eq!(summary["tables"]["phones"], json!(1));
    }

    /// Two replicas whose edits to m1 conflict.
    fn conflicting(tmp: &Path) -> (PathBuf, Database, PathBuf, Database) {
        let (adir, bdir) = (tmp.join("a"), tmp.join("b"));
        let (mut a, mut b) = (replica(&adir), replica(&bdir));
        put_row(
            &mut a,
            "computers",
            "m1",
            json!({"label": "m1", "notes": "old"}),
        )
        .unwrap();
        exchange(&mut a, &mut b);
        put_row(
            &mut a,
            "computers",
            "m1",
            json!({"label": "m1", "notes": "a"}),
        )
        .unwrap();
        put_row(
            &mut b,
            "computers",
            "m1",
            json!({"label": "m1", "notes": "b"}),
        )
        .unwrap();
        exchange(&mut a, &mut b);
        (adir, a, bdir, b)
    }

    #[test]
    fn loading_a_dump_back_leaves_a_conflict_it_shows() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let (_, mut a, _, _b) = conflicting(tmp.path());
        let before = a.history.index.inventory().count();
        let dump = dump_all(&a, true).unwrap();

        let report = load_all(&mut a, &dump).unwrap();
        assert_eq!(report["operations_committed"], json!(0));
        assert_eq!(a.history.index.inventory().count(), before);
        assert_eq!(tables_summary(&a).unwrap()["conflicts"], json!(1));

        // A document that does differ still wins, and settles the conflict.
        let doc = json!({"computers": [{"label": "m1", "notes": "chosen"}]});
        load_all(&mut a, &doc).unwrap();
        assert_eq!(tables_summary(&a).unwrap()["conflicts"], json!(0));
    }

    #[test]
    fn a_rekey_names_the_conflicts_it_settles() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let (adir, a, _, _b) = conflicting(tmp.path());
        drop(a);
        let report = rekey_with(&adir, ROTATED).unwrap();
        assert_eq!(
            report["moved"][0]["left_behind"]["conflicts_settled_by_digest"],
            json!(["computers/m1"])
        );
    }

    #[test]
    fn a_rekey_carries_a_deletion_another_machine_has_not_seen() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let (adir, bdir) = (tmp.path().join("a"), tmp.path().join("b"));
        let (mut a, mut b) = (replica(&adir), replica(&bdir));
        put_row(&mut a, "computers", "gone", json!({"label": "gone"})).unwrap();
        exchange(&mut a, &mut b);
        delete_row(&mut a, "computers", "gone").unwrap();
        drop(a);
        drop(b);

        rekey_with(&adir, ROTATED).unwrap();
        rekey_with(&bdir, ROTATED).unwrap();
        let (mut a, mut b) = (
            Database::open(&adir).unwrap(),
            Database::open(&bdir).unwrap(),
        );
        exchange(&mut a, &mut b);

        // The deletion is not lost: the row shows it, and `tables` counts it, until settled.
        let row = get_row(&a.view(), "computers", "gone").unwrap().unwrap();
        assert_eq!(row["_conflicts"], json!([{"_deleted": true}]));
        assert_eq!(tables_summary(&a).unwrap()["conflicts"], json!(1));
    }

    /// Copy a replica's folder, as a backup or a restore does.
    fn copy_folder(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        std::fs::set_permissions(to, std::fs::metadata(from).unwrap().permissions()).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let path = entry.unwrap().path();
            let copy = to.join(path.file_name().unwrap());
            if path.is_dir() {
                copy_folder(&path, &copy);
            } else {
                std::fs::copy(&path, &copy).unwrap();
            }
        }
    }

    #[test]
    fn a_folder_restored_from_a_backup_costs_only_what_it_wrote_since() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let (adir, backup) = (tmp.path().join("a"), tmp.path().join("backup"));
        let mut a = replica(&adir);
        let mut b = replica(&tmp.path().join("b"));
        put_row(&mut a, "computers", "before", json!({"label": "before"})).unwrap();
        drop(a);
        copy_folder(&adir, &backup);

        let mut a = Database::open(&adir).unwrap();
        put_row(&mut a, "computers", "since", json!({"label": "since"})).unwrap();
        exchange(&mut a, &mut b);
        put_row(&mut b, "computers", "theirs", json!({"label": "theirs"})).unwrap();
        drop(a);

        // The restore writes the sequence the original already wrote.
        std::fs::remove_dir_all(&adir).unwrap();
        std::fs::rename(&backup, &adir).unwrap();
        let mut restored = Database::open(&adir).unwrap();
        put_row(
            &mut restored,
            "computers",
            "other",
            json!({"label": "other"}),
        )
        .unwrap();
        exchange(&mut restored, &mut b);

        let summary = tables_summary(&b).unwrap();
        assert_eq!(summary["quarantined_writers"], json!(1));
        assert_eq!(summary["ok"], json!(false));
        let rows = scan_table(&b.view(), "computers").unwrap();
        assert_eq!(rows.keys().collect::<Vec<_>>(), ["before", "theirs"]);
        put_row(&mut b, "computers", "later", json!({"label": "later"}))
            .expect("b can still write");
        assert_eq!(scan_table(&b.view(), "computers").unwrap().len(), 3);
    }

    #[test]
    fn a_write_after_a_kept_control_record_stays_visible_and_writable() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let forger = tmp.path().join("forger");
        local::test_forge_authority(&forger, SECRET).unwrap();
        let mut forger = Database::open(&forger).unwrap();
        let id = forger
            .revoke_writes(crypto::random::<16>().unwrap(), [7; 48], None)
            .unwrap();
        let wire = forger.history.wire(&id).unwrap().to_vec();

        // A replica an earlier build left: it kept the record and wrote with it as a parent.
        let victim = tmp.path().join("victim");
        local::ensure_fleet(&victim, SECRET).unwrap();
        local::test_forget_no_authority(&victim).unwrap();
        let mut db = Database::open(&victim).unwrap();
        db.ingest(&wire).unwrap();
        put_row(&mut db, "computers", "m2", json!({"label": "m2"})).unwrap();
        drop(db);

        let mut healed = replica(&victim);
        assert!(
            get_row(&healed.view(), "computers", "m2")
                .unwrap()
                .is_some()
        );
        put_row(&mut healed, "computers", "m3", json!({"label": "m3"}))
            .expect("it can still write");

        let mut peer = replica(&tmp.path().join("peer"));
        exchange(&mut healed, &mut peer);
        assert_eq!(scan_table(&peer.view(), "computers").unwrap().len(), 2);
    }

    #[test]
    fn no_member_can_stop_a_row_for_good() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let mut a = replica(&tmp.path().join("a"));
        let bdir = tmp.path().join("b");
        let mut b = replica(&bdir);
        let key = "root@example.com";
        put_row(&mut a, "emails", key, json!({"email": key})).unwrap();
        exchange(&mut a, &mut b);
        let first = a.history.index.inventory().next().copied().unwrap();
        let stop = Command::Write {
            request: crypto::random::<16>().unwrap(),
            changes: vec![Change {
                collection: "emails".into(),
                key: storage_key("emails", key).unwrap(),
                value: None,
            }],
        };

        assert!(a.commit(stop.clone()).is_err(), "the verb refuses it");
        // The same record signed by hand, as a member's own tooling can.
        let forged = crypto::seal(
            &a.credentials.manifest,
            &a.credentials.key,
            &a.credentials.seed,
            &a.credentials.capability,
            crypto::Causal {
                sequence: 2,
                previous: Some(first),
                parents: &[first],
            },
            &stop.encode().unwrap(),
        )
        .unwrap();
        b.ingest(&forged).unwrap();
        assert!(get_row(&b.view(), "emails", key).unwrap().is_some());
        assert_eq!(tables_summary(&b).unwrap()["invalid_operations"], json!(1));
        put_row(&mut b, "emails", key, json!({"email": key, "notes": "n"}))
            .expect("still writable");
        drop(b);
        let b = replica(&bdir);
        assert!(
            get_row(&b.view(), "emails", key).unwrap().is_some(),
            "a replay keeps it inert"
        );
    }

    /// A stored has-password word stays visible, so the import can judge it, and counts as no
    /// password. NEGATIVE CONTROL: with `drop_read_fields` nulling it, the import reads nothing
    /// and records that cell covered (`alab::command`'s
    /// `a_flag_shaped_password_is_covered_only_when_its_word_agrees`):
    /// `` left: Number(Number(Unsigned(1))) right: Number(Number(Unsigned(2))) ``.
    #[test]
    fn a_stored_flag_reads_as_stored_and_a_read_field_is_not_taken_for_the_row() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let mut db = replica(&tmp.path().join("db"));
        let key = "root@example.com";
        db.commit(Command::Write {
            request: crypto::random::<16>().unwrap(),
            changes: vec![Change {
                collection: "emails".into(),
                key: storage_key("emails", key).unwrap(),
                value: Some(json!({
                    "email": key,
                    "protected": false,
                    "password": "1",
                    "_conflicts": [{"notes": "stale"}],
                })),
            }],
        })
        .unwrap();

        let row = get_row(&db.view(), "emails", key).unwrap().unwrap();
        assert_eq!(row["password"], json!("1"));
        assert!(row.get("_conflicts").is_none());
        let summary = tables_summary(&db).unwrap();
        assert_eq!(summary["tables"]["emails"]["root_with_password"], json!(0));
        let dump = dump_all(&db, true).unwrap();
        assert_eq!(dump["emails"][0]["password"], json!("1"));
        put_row(&mut db, "emails", key, json!({"email": key, "notes": "n"})).unwrap();
        let row = get_row(&db.view(), "emails", key).unwrap().unwrap();
        assert_eq!(
            row["password"],
            json!("1"),
            "a partial edit changes nothing"
        );

        // A real password replaces the word and is written clean; no word replaces it.
        put_row(
            &mut db,
            "emails",
            key,
            json!({"email": key, "password": "pw"}),
        )
        .unwrap();
        let before = db.history.index.inventory().count();
        for word in ["1", "no", "none", "set", "-"] {
            let refused = put_row(
                &mut db,
                "emails",
                key,
                json!({"email": key, "password": word}),
            );
            assert!(refused.is_err(), "{word}");
            let doc = json!({"emails": [{"email": key, "password": word}]});
            assert!(load_all(&mut db, &doc).is_err(), "{word}");
        }
        assert_eq!(db.history.index.inventory().count(), before);
        let row = get_row(&db.view(), "emails", key).unwrap().unwrap();
        assert_eq!(row["password"], json!("pw"));
        put_row(
            &mut db,
            "emails",
            key,
            json!({"email": key, "password": null}),
        )
        .expect("a null still clears it");
        let row = get_row(&db.view(), "emails", key).unwrap().unwrap();
        assert_eq!(row["password"], Value::Null);
    }

    /// A rekey carries a stored word on, so the replica's import still has it to judge.
    /// NEGATIVE CONTROL: with `drop_read_fields` nulling the word, the new dataset reads no password:
    /// `` left: Null right: String("1") ``.
    #[test]
    fn a_rekey_carries_a_stored_flag_on() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("db");
        let key = "root@example.com";
        let mut db = replica(&dir);
        put_row(
            &mut db,
            "emails",
            key,
            json!({"email": key, "password": "1"}),
        )
        .unwrap();
        drop(db);

        rekey_with(&dir, ROTATED).unwrap();
        let db = Database::open(&dir).unwrap();
        let row = get_row(&db.view(), "emails", key).unwrap().unwrap();
        assert_eq!(row["password"], json!("1"));
    }

    /// `a` stores a real password over the sheet's `1` while `b`, not yet synced, writes `other`
    /// in its place: the cell `a` then holds.
    fn a_password_against(tmp: &Path, other: Value) -> Database {
        let key = "root@example.com";
        let row = |password: Value, notes: &str| json!({"email": key, "protected": false, "password": password, "notes": notes});
        let write = |db: &mut Database, value: Value| {
            db.commit(Command::Write {
                request: crypto::random::<16>().unwrap(),
                changes: vec![Change {
                    collection: "emails".into(),
                    key: storage_key("emails", key).unwrap(),
                    value: Some(value),
                }],
            })
            .unwrap();
        };
        let (mut a, mut b) = (replica(&tmp.join("a")), replica(&tmp.join("b")));
        write(&mut a, row(json!("1"), "base"));
        exchange(&mut a, &mut b);
        write(&mut a, row(json!("real-password"), "base"));
        write(&mut b, row(other, "edit"));
        exchange(&mut a, &mut b);
        a
    }

    /// NEGATIVE CONTROL: with `project_cell`'s password rule dropped, whichever run the digest
    /// order puts the word last reads the word (and a put of the notes alone would then erase the
    /// password): `` left: String("1") right: String("real-password") ``.
    #[test]
    fn a_stored_password_outlives_a_concurrent_empty_one_in_either_digest_order() {
        let key = "root@example.com";
        for other in [json!("1"), Value::Null] {
            let mut orders = BTreeSet::new();
            for _ in 0..64 {
                if orders.len() == 2 {
                    break;
                }
                let tmp = aterm_tempfile::tempdir().expect("tempdir");
                let mut a = a_password_against(tmp.path(), other.clone());
                let view = a.view();
                let cell = &view.cells[&("emails".to_owned(), storage_key("emails", key).unwrap())];
                orders.insert(cell.candidates.last().unwrap().value["notes"] == json!("edit"));
                let row = get_row(&view, "emails", key).unwrap().unwrap();
                assert_eq!(row["password"], json!("real-password"));
                assert!(row["_conflicts"].as_array().is_some_and(|c| !c.is_empty()));
                let summary = tables_summary(&a).unwrap();
                assert_eq!(summary["tables"]["emails"]["root_with_password"], json!(1));
                assert_eq!(summary["conflicts"], json!(1));

                put_row(&mut a, "emails", key, json!({"email": key, "notes": "n"})).unwrap();
                let row = get_row(&a.view(), "emails", key).unwrap().unwrap();
                assert_eq!(row["password"], json!("real-password"), "a partial edit");
                assert!(row.get("_conflicts").is_none());
            }
            assert_eq!(orders.len(), 2, "both digest orders ran");
        }
    }

    /// NEGATIVE CONTROL: with the conflict judged on the stored values, the row shows a conflict
    /// with itself and `tables` counts it: `` left: Some(Array([..])) right: None ``.
    /// NEGATIVE CONTROL: with `project_cell` comparing the stored spelling, an empty cell beside a
    /// null reads as a conflict: `` left: Some(Array([Object({"email": String("root@example.com"),
    /// "notes": String("n"), "protected": Bool(false)})])) right: None ``.
    #[test]
    fn password_cells_that_say_the_same_thing_are_no_conflict() {
        let key = "root@example.com";
        let write = |db: &mut Database, password: Option<Value>| {
            let mut row = json!({"email": key, "protected": false, "notes": "n"});
            if let Some(password) = password {
                row.as_object_mut()
                    .unwrap()
                    .insert("password".into(), password);
            }
            db.commit(Command::Write {
                request: crypto::random::<16>().unwrap(),
                changes: vec![Change {
                    collection: "emails".into(),
                    key: storage_key("emails", key).unwrap(),
                    value: Some(row),
                }],
            })
            .unwrap();
        };
        let concurrent = |tmp: &Path, one: Option<Value>, other: Value| {
            let (mut a, mut b) = (replica(&tmp.join("a")), replica(&tmp.join("b")));
            write(&mut a, Some(Value::Null));
            exchange(&mut a, &mut b);
            write(&mut a, one);
            write(&mut b, Some(other));
            exchange(&mut a, &mut b);
            a
        };
        for (one, other) in [
            (None, json!(null)),
            (Some(json!("")), json!(null)),
            (Some(json!("  ")), json!(null)),
            (Some(json!("0")), json!(null)),
            (Some(json!("none")), json!(null)),
            (Some(json!("0")), json!("no")),
            (Some(json!("yes")), json!("1")),
        ] {
            let tmp = aterm_tempfile::tempdir().expect("tempdir");
            let a = concurrent(tmp.path(), one.clone(), other.clone());
            let row = get_row(&a.view(), "emails", key).unwrap().unwrap();
            assert_eq!(row.get("_conflicts"), None, "{one:?} {other:?}");
            assert_eq!(
                tables_summary(&a).unwrap()["conflicts"],
                json!(0),
                "{one:?} {other:?}"
            );
        }
        for (one, other) in [
            ("1", json!(null)),
            ("1", json!("0")),
            ("real-password", json!(null)),
            ("real-password", json!("1")),
            ("real-password", json!("another-password")),
        ] {
            let tmp = aterm_tempfile::tempdir().expect("tempdir");
            let a = concurrent(tmp.path(), Some(json!(one)), other.clone());
            assert_eq!(
                tables_summary(&a).unwrap()["conflicts"],
                json!(1),
                "{one} {other:?}"
            );
        }
    }

    #[test]
    fn values_that_read_alike_are_no_conflict() {
        let key = "root@example.com";
        for stored in [
            ("_conflicts", json!([{"notes": "stale"}])),
            ("has_password", json!(true)),
        ] {
            let tmp = aterm_tempfile::tempdir().expect("tempdir");
            let (mut a, mut b) = (
                replica(&tmp.path().join("a")),
                replica(&tmp.path().join("b")),
            );
            put_row(&mut a, "emails", key, json!({"email": key, "notes": "old"})).unwrap();
            exchange(&mut a, &mut b);
            let mut junk =
                json!({"email": key, "protected": false, "password": null, "notes": "n"});
            junk.as_object_mut()
                .unwrap()
                .insert(stored.0.to_owned(), stored.1);
            a.commit(Command::Write {
                request: crypto::random::<16>().unwrap(),
                changes: vec![Change {
                    collection: "emails".into(),
                    key: storage_key("emails", key).unwrap(),
                    value: Some(junk),
                }],
            })
            .unwrap();
            put_row(&mut b, "emails", key, json!({"email": key, "notes": "n"})).unwrap();
            exchange(&mut a, &mut b);

            let row = get_row(&a.view(), "emails", key).unwrap().unwrap();
            assert_eq!(row.get("_conflicts"), None);
            let summary = tables_summary(&a).unwrap();
            assert_eq!(summary["conflicts"], json!(0));
            assert_eq!(summary["ok"], json!(true));
        }
    }

    /// NEGATIVE CONTROL: with `drop_read_fields` nulling the mark, the other Mac's dump loads and
    /// clears the password: `` assertion failed: load_all(&mut holder, &dump).is_err() ``.
    #[test]
    fn a_dump_holding_a_stored_mark_cannot_clear_a_password() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let key = "root@example.com";
        let (mut holder, mut other) = (
            replica(&tmp.path().join("holder")),
            replica(&tmp.path().join("other")),
        );
        put_row(
            &mut holder,
            "emails",
            key,
            json!({"email": key, "password": "real-password"}),
        )
        .unwrap();
        other
            .commit(Command::Write {
                request: crypto::random::<16>().unwrap(),
                changes: vec![Change {
                    collection: "emails".into(),
                    key: storage_key("emails", key).unwrap(),
                    value: Some(json!({"email": key, "protected": false, "password": REDACTED})),
                }],
            })
            .unwrap();
        let dump = dump_all(&other, true).unwrap();
        assert!(load_all(&mut holder, &dump).is_err());
        let row = get_row(&holder.view(), "emails", key).unwrap().unwrap();
        assert_eq!(row["password"], json!("real-password"));
    }

    /// NEGATIVE CONTROL: with `refuse_unfinished` out of the round, the peer's older row lands in
    /// the partial dataset and stands through the rerun:
    /// `` called `Result::unwrap_err()` on an `Ok` value ``.
    #[test]
    fn no_round_feeds_a_dataset_a_cut_short_rekey_is_still_moving() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        let mut mine = replica(&a);
        let row = |notes: &str| json!({"label": "m1", "notes": notes});
        put_row(&mut mine, "computers", "m1", row("only here")).unwrap();
        drop(mine);
        local::ensure_fleet(&b, ROTATED).unwrap();
        let mut peer = Database::open(&b).unwrap();
        put_row(&mut peer, "computers", "m1", row("older")).unwrap();
        drop(peer);
        let listener =
            aterm_uds::kept_out(|| std::net::TcpListener::bind("127.0.0.1:0")).expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let server = {
            let (b, running) = (b.clone(), running.clone());
            std::thread::spawn(move || {
                network::serve_fleet_until(
                    &b,
                    listener,
                    running,
                    std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
                )
            })
        };
        cut_short(&a, ROTATED, |_| {});

        let mut repair = network::Repair::default();
        let refused = network::sync_fleet_round_with(&a, addr, &mut repair).unwrap_err();
        assert!(refused.to_string().contains("aterm db rekey"), "{refused}");
        let waiting = network::sync_round_with(&a, addr, &mut repair).unwrap_err();
        assert!(waiting.to_string().contains("aterm db rekey"), "{waiting}");
        let db = Database::open(&a).unwrap();
        assert!(scan_table(&db.view(), "computers").unwrap().is_empty());
        drop(db);

        rekey_with(&a, ROTATED).unwrap();
        let db = Database::open(&a).unwrap();
        let rows = scan_table(&db.view(), "computers").unwrap();
        assert_eq!(rows["m1"]["notes"], json!("only here"));
        drop(db);
        // Once it has finished, a round carries rows as before and both values show.
        network::sync_fleet_round_with(&a, addr, &mut repair).unwrap();
        running.store(false, std::sync::atomic::Ordering::Release);
        let _ = server.join();
        let db = Database::open(&a).unwrap();
        let rows = scan_table(&db.view(), "computers").unwrap();
        assert!(rows["m1"].get("_conflicts").is_some(), "{}", rows["m1"]);
    }

    /// NEGATIVE CONTROL: with `sequence_follows` back to an addition, the view panics:
    /// `` attempt to add with overflow ``.
    #[test]
    fn a_link_from_the_last_sequence_is_an_impossible_link_not_a_panic() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let mut a = replica(&tmp.path().join("a"));
        let mut b = replica(&tmp.path().join("b"));
        put_row(&mut a, "computers", "m1", json!({"label": "m1"})).unwrap();
        exchange(&mut a, &mut b);
        let first = *a.history.index.inventory().next().unwrap();
        let merge = Command::Merge {
            request: crypto::random::<16>().unwrap(),
        }
        .encode()
        .unwrap();
        let seal = |sequence, previous: [u8; 32]| {
            crypto::seal(
                &a.credentials.manifest,
                &a.credentials.key,
                &a.credentials.seed,
                &a.credentials.capability,
                crypto::Causal {
                    sequence,
                    previous: Some(previous),
                    parents: &[previous],
                },
                &merge,
            )
            .unwrap()
        };
        let last = seal(u64::MAX, first);
        let after = seal(7, Sha256::digest(&last));
        b.ingest(&last).unwrap();
        b.ingest(&after).unwrap();

        let view = b.view();
        assert_eq!(scan_table(&view, "computers").unwrap().len(), 1);
        assert_eq!(view.causal.pending.len(), 2);
    }

    /// What a process killed part way through the load leaves, under `key`, with `loaded` written.
    fn cut_short(dir: &Path, key: &[u8], loaded: impl FnOnce(&mut Database)) {
        let db = Database::open(dir).unwrap();
        let id = hex::encode_lower(&db.credentials.manifest.id[..8]);
        drop(db);
        aterm_uds::private_file::write_private(dir, REKEY_UNFINISHED, b"0").unwrap();
        let name = dir.file_name().unwrap().to_str().unwrap();
        std::fs::rename(dir, dir.with_file_name(format!("{name}.before-rekey-{id}"))).unwrap();
        local::ensure_fleet(dir, key).unwrap();
        loaded(&mut Database::open(dir).unwrap());
    }

    /// NEGATIVE CONTROL: with `rekey_with` going straight to the kept folder, the partial dataset
    /// refuses the second key: `` this database belongs to another fleet key ``.
    #[test]
    fn a_key_rotated_again_after_a_load_was_cut_short_still_finishes() {
        const THIRD: &[u8] = b"3333333333333333333333333333333333333333";
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("db");
        let mut db = replica(&dir);
        put_row(
            &mut db,
            "computers",
            "m1",
            json!({"label": "m1", "notes": "old"}),
        )
        .unwrap();
        put_row(&mut db, "computers", "m2", json!({"label": "m2"})).unwrap();
        drop(db);
        cut_short(&dir, ROTATED, |db| {
            put_row(
                db,
                "computers",
                "m1",
                json!({"label": "m1", "notes": "since"}),
            )
            .unwrap();
        });

        let report = rekey_with(&dir, THIRD).expect("the second key takes both folders");
        assert_eq!(report["moved"].as_array().unwrap().len(), 2, "{report}");
        assert!(unfinished_rekey(&dir).unwrap().is_none());
        let db = Database::open(&dir).unwrap();
        let rows = scan_table(&db.view(), "computers").unwrap();
        assert_eq!(rows.keys().collect::<Vec<_>>(), ["m1", "m2"]);
        assert_eq!(rows["m1"]["notes"], json!("since"), "an edit since stands");
        drop(db);
        assert!(rekey_with(&dir, THIRD).is_err(), "nothing left to move");
    }

    /// The order the folders were set aside in decides, not a clock or a directory listing.
    /// NEGATIVE CONTROL: sorted oldest first, the older row displaces the edit:
    /// `` left: String("old") right: String("since") ``.
    #[test]
    fn a_rerun_moves_the_folder_set_aside_last_first() {
        const THIRD: &[u8] = b"3333333333333333333333333333333333333333";
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("db");
        let row = |notes: &str| json!({"label": "m1", "notes": notes});
        put_row(&mut replica(&dir), "computers", "m1", row("old")).unwrap();
        cut_short(&dir, ROTATED, |db| {
            put_row(db, "computers", "m1", row("since")).unwrap();
        });
        // Both folders set aside with the second killed before it loaded anything.
        let id = {
            let db = Database::open(&dir).unwrap();
            hex::encode_lower(&db.credentials.manifest.id[..8])
        };
        aterm_uds::private_file::write_private(&dir, REKEY_UNFINISHED, b"1").unwrap();
        std::fs::rename(&dir, dir.with_file_name(format!("db.before-rekey-{id}"))).unwrap();

        rekey_with(&dir, THIRD).unwrap();
        let db = Database::open(&dir).unwrap();
        let rows = scan_table(&db.view(), "computers").unwrap();
        assert_eq!(rows["m1"]["notes"], json!("since"));
    }

    /// NEGATIVE CONTROL: with the kept folder named by the dataset alone, the third move fails:
    /// `` Directory not empty ``.
    #[test]
    fn a_key_used_again_keeps_each_folder_it_set_aside() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("db");
        put_row(
            &mut replica(&dir),
            "computers",
            "m1",
            json!({"label": "m1"}),
        )
        .unwrap();
        let mut kept = BTreeSet::new();
        for key in [ROTATED, SECRET, ROTATED] {
            let report = rekey_with(&dir, key).unwrap();
            kept.insert(report["moved"][0]["kept"].as_str().unwrap().to_owned());
            local::ensure_fleet(&dir, key).unwrap();
            let db = Database::open(&dir).unwrap();
            assert!(get_row(&db.view(), "computers", "m1").unwrap().is_some());
        }
        assert_eq!(kept.len(), 3, "{kept:?}");
    }

    /// NEGATIVE CONTROL: with `inherit_email_state` run on a key that stands, the document's
    /// `protected: false` refuses the whole document:
    /// `` protected real email row cannot have protected downgraded to false ``.
    #[test]
    fn a_mark_made_since_a_load_was_cut_short_does_not_stop_the_move() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("db");
        let key = "x@example.com";
        let mut db = replica(&dir);
        put_row(&mut db, "computers", "m1", json!({"label": "m1"})).unwrap();
        put_row(
            &mut db,
            "emails",
            key,
            json!({"email": key, "protected": false}),
        )
        .unwrap();
        drop(db);
        cut_short(&dir, ROTATED, |db| {
            put_row(db, "emails", key, json!({"email": key, "protected": true})).unwrap();
        });

        rekey_with(&dir, ROTATED).expect("the mark stands and the rest moves");
        let db = Database::open(&dir).unwrap();
        let view = db.view();
        assert_eq!(
            get_row(&view, "emails", key).unwrap().unwrap()["protected"],
            json!(true)
        );
        assert!(get_row(&view, "computers", "m1").unwrap().is_some());
        drop(db);
        assert!(rekey_with(&dir, ROTATED).is_err(), "nothing left to move");
    }

    /// NEGATIVE CONTROL: with `unfinished_rekeys` passing on the listing's error, every verb
    /// refuses: `` Permission denied ``.
    #[test]
    fn a_parent_this_user_cannot_list_refuses_no_verb() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let parent = tmp.path().join("parent");
        let dir = parent.join("db");
        put_row(
            &mut replica(&dir),
            "computers",
            "m1",
            json!({"label": "m1"}),
        )
        .unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o300)).unwrap();
        let listed = unfinished_rekeys(&dir);
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(listed.unwrap().is_empty());
    }

    /// NEGATIVE CONTROL: with `one_writer_holds` always `Ok`, the rename comes first and the load
    /// then stops on the writer's quota with nothing left to rerun it past.
    #[test]
    fn rows_one_writer_cannot_hold_are_refused_before_anything_moves() {
        let row = |n: usize| Change {
            collection: "computers".into(),
            key: format!("m{n}"),
            value: Some(json!({"label": format!("m{n}"), "notes": "x".repeat(50_000)})),
        };
        assert!(one_writer_holds((0..100).map(row).collect()).is_ok());
        assert!(one_writer_holds((0..180).map(row).collect()).is_err());
    }

    /// NEGATIVE CONTROL: with the server's opens back to `Database::open`, the server drops the
    /// connection beside a neighbour: `report["ok"]` is false, naming a TLS `close_notify`.
    #[test]
    fn a_sync_beside_a_neighbour_of_the_server_gets_every_row() {
        let tmp = fleet_fixture();
        let secret = resolve_fleet_secret(&tmp.path().join("a")).unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        local::ensure_fleet(&a, &secret).unwrap();
        local::ensure_fleet(&b, &secret).unwrap();
        let mut peer = Database::open(&a).unwrap();
        for n in 0..5 {
            let key = format!("m{n}");
            put_row(&mut peer, "computers", &key, json!({"label": key})).unwrap();
        }
        drop(peer);

        let listener =
            aterm_uds::kept_out(|| std::net::TcpListener::bind("127.0.0.1:0")).expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let server = {
            let (a, running) = (a.clone(), running.clone());
            std::thread::spawn(move || {
                network::serve_fleet_until(
                    &a,
                    listener,
                    running,
                    std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
                )
            })
        };
        let neighbour = {
            let (a, running) = (a.clone(), running.clone());
            std::thread::spawn(move || {
                while running.load(std::sync::atomic::Ordering::Acquire) {
                    let held = Database::open(&a);
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    drop(held);
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            })
        };
        let report = sync_peers(&b, &[addr]).unwrap();
        running.store(false, std::sync::atomic::Ordering::Release);
        neighbour.join().unwrap();
        let _ = server.join();

        assert_eq!(report["ok"], json!(true), "{report}");
        let db = Database::open(&b).unwrap();
        assert_eq!(scan_table(&db.view(), "computers").unwrap().len(), 5);
    }

    #[test]
    fn a_control_record_forged_with_the_fleet_key_cannot_hide_the_rows() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let victim = tmp.path().join("victim");
        let mut db = replica(&victim);
        put_row(&mut db, "computers", "m1", json!({"label": "m1"})).unwrap();
        // Two control records from one stream fork it, which withdraws every row.
        let forged: Vec<Vec<u8>> = ["forger-a", "forger-b"]
            .into_iter()
            .map(|name| {
                let forger = tmp.path().join(name);
                local::test_forge_authority(&forger, SECRET).unwrap();
                let mut forger = Database::open(&forger).unwrap();
                let actor = forger.credentials.capability.actor;
                let id = forger
                    .commit(Command::Budget {
                        request: crypto::random::<16>().unwrap(),
                        name: name.to_owned(),
                        total: 1,
                        shares: BTreeMap::from([(actor, 1)]),
                    })
                    .unwrap();
                forger.history.wire(&id).unwrap().to_vec()
            })
            .collect();

        for wire in &forged {
            db.ingest(wire)
                .expect("a replica keeps it as inert evidence");
        }
        let summary = tables_summary(&db).unwrap();
        assert_eq!(summary["authority_fault"], json!(false));
        assert_eq!(summary["tables"]["computers"], json!(1));
        drop(db);

        // Without the mark they wipe the view, which is what the mark prevents ...
        local::test_forget_no_authority(&victim).unwrap();
        let mut unmarked = Database::open(&victim).unwrap();
        for wire in &forged {
            unmarked.ingest(wire).unwrap();
        }
        assert!(unmarked.view().authority_fault);
        assert!(
            scan_table(&unmarked.view(), "computers")
                .unwrap()
                .is_empty()
        );
        drop(unmarked);

        // ... and a replica that kept such records recovers its rows on the next open.
        let healed = replica(&victim);
        let summary = tables_summary(&healed).unwrap();
        assert_eq!(summary["authority_fault"], json!(false));
        assert_eq!(summary["tables"]["computers"], json!(1));
    }

    #[test]
    fn a_concurrent_delete_never_hides_an_edit_and_is_reported_until_settled() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let mut a = replica(&tmp.path().join("a"));
        let mut b = replica(&tmp.path().join("b"));
        put_row(
            &mut a,
            "computers",
            "m1",
            json!({"label": "m1", "notes": "old"}),
        )
        .unwrap();
        exchange(&mut a, &mut b);

        put_row(
            &mut a,
            "computers",
            "m1",
            json!({"label": "m1", "notes": "edit"}),
        )
        .unwrap();
        delete_row(&mut b, "computers", "m1").unwrap();
        exchange(&mut a, &mut b);

        for db in [&a, &b] {
            let row = get_row(&db.view(), "computers", "m1")
                .unwrap()
                .expect("the edit shows");
            assert_eq!(row["notes"], json!("edit"));
            assert_eq!(row["_conflicts"], json!([{"_deleted": true}]));
            let summary = tables_summary(db).unwrap();
            assert_eq!(summary["conflicts"], json!(1));
            assert_eq!(summary["ok"], json!(false));
        }

        delete_row(&mut a, "computers", "m1").unwrap();
        exchange(&mut a, &mut b);
        for db in [&a, &b] {
            assert!(get_row(&db.view(), "computers", "m1").unwrap().is_none());
            assert_eq!(ok_of(db), json!(true));
        }
    }

    #[test]
    fn two_machines_writing_the_same_value_do_not_conflict() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let mut a = replica(&tmp.path().join("a"));
        let mut b = replica(&tmp.path().join("b"));
        let row = json!({"label": "m1", "notes": "same"});
        put_row(&mut a, "computers", "m1", row.clone()).unwrap();
        put_row(&mut b, "computers", "m1", row).unwrap();
        exchange(&mut a, &mut b);

        let seen = get_row(&a.view(), "computers", "m1").unwrap().unwrap();
        assert!(seen.get("_conflicts").is_none());
        assert_eq!(ok_of(&a), json!(true));
    }

    #[test]
    fn an_alias_reads_but_never_changes_a_row() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let mut db = replica(&tmp.path().join("db"));
        put_row(&mut db, "computers", "m2", json!({"label": "m2"})).unwrap();
        put_row(
            &mut db,
            "computers",
            "m1",
            json!({"label": "m1", "nickname": "desk", "aliases": ["m2"]}),
        )
        .unwrap();
        assert!(get_row(&db.view(), "computers", "desk").unwrap().is_some());

        assert!(delete_row(&mut db, "computers", "desk").is_err());
        delete_row(&mut db, "computers", "m2").unwrap();
        // `m2` now only means m1's alias; deleting it again must not take m1 with it.
        assert!(delete_row(&mut db, "computers", "m2").is_err());
        assert!(put_row(&mut db, "computers", "desk", json!({})).is_err());

        let rows = scan_table(&db.view(), "computers").unwrap();
        assert_eq!(rows.keys().collect::<Vec<_>>(), ["m1"]);
    }

    #[test]
    fn a_put_without_a_password_keeps_it_and_null_clears_it() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let mut db = replica(&tmp.path().join("db"));
        let key = "root@example.com";
        put_row(
            &mut db,
            "emails",
            key,
            json!({"email": key, "password": "pw"}),
        )
        .unwrap();

        put_row(&mut db, "emails", key, json!({"email": key, "notes": "n"})).unwrap();
        let row = get_row(&db.view(), "emails", key).unwrap().unwrap();
        assert_eq!(row["password"], json!("pw"));
        assert_eq!(row["notes"], json!("n"));

        // Protecting a row drops its password rather than failing on the kept one.
        put_row(
            &mut db,
            "emails",
            key,
            json!({"email": key, "protected": true}),
        )
        .unwrap();
        let row = get_row(&db.view(), "emails", key).unwrap().unwrap();
        assert_eq!(row["password"], Value::Null);

        let other = "other@example.com";
        put_row(
            &mut db,
            "emails",
            other,
            json!({"email": other, "password": "pw"}),
        )
        .unwrap();
        put_row(
            &mut db,
            "emails",
            other,
            json!({"email": other, "password": null}),
        )
        .unwrap();
        let row = get_row(&db.view(), "emails", other).unwrap().unwrap();
        assert_eq!(row["password"], Value::Null);
    }

    #[test]
    fn a_redacted_dump_cannot_overwrite_a_password() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let mut db = replica(&tmp.path().join("db"));
        let key = "root@example.com";
        put_row(
            &mut db,
            "emails",
            key,
            json!({"email": key, "password": "pw"}),
        )
        .unwrap();

        let redacted = dump_all(&db, false).unwrap();
        assert_eq!(redacted["emails"][0]["password"], json!(REDACTED));
        assert!(load_all(&mut db, &redacted).is_err());
        let refused = put_row(&mut db, "emails", key, redacted["emails"][0].clone());
        assert!(refused.is_err());

        let row = get_row(&db.view(), "emails", key).unwrap().unwrap();
        assert_eq!(row["password"], json!("pw"));
        assert!(row.get("has_password").is_none());
    }

    #[test]
    fn loading_a_dump_again_writes_nothing_and_only_differences_are_written() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let mut db = replica(&tmp.path().join("db"));
        put_row(
            &mut db,
            "computers",
            "m1",
            json!({"label": "m1", "notes": "a"}),
        )
        .unwrap();
        put_row(&mut db, "computers", "m2", json!({"label": "m2"})).unwrap();
        let dump = dump_all(&db, true).unwrap();

        let before = db.history.index.inventory().count();
        let report = load_all(&mut db, &dump).unwrap();
        assert_eq!(report["operations_committed"], json!(0));
        assert_eq!(report["unchanged"], json!(2));
        assert_eq!(db.history.index.inventory().count(), before);

        put_row(
            &mut db,
            "computers",
            "m1",
            json!({"label": "m1", "notes": "b"}),
        )
        .unwrap();
        let report = load_all(&mut db, &dump).unwrap();
        assert_eq!(report["imported"]["computers"], json!(1));
        let row = get_row(&db.view(), "computers", "m1").unwrap().unwrap();
        assert_eq!(row["notes"], json!("a"));
    }

    #[test]
    fn a_stored_redaction_mark_reads_as_a_word_and_blocks_nothing() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("db");
        let mut db = replica(&dir);
        let key = "root@example.com";
        let store_mark = |db: &mut Database| {
            db.commit(Command::Write {
                request: crypto::random::<16>().unwrap(),
                changes: vec![Change {
                    collection: "emails".into(),
                    key: storage_key("emails", key).unwrap(),
                    value: Some(json!({"email": key, "protected": false, "password": REDACTED})),
                }],
            })
            .unwrap();
        };

        store_mark(&mut db);
        let row = get_row(&db.view(), "emails", key).unwrap().unwrap();
        assert_eq!(row["password"], json!(HAS_PASSWORD));
        put_row(&mut db, "emails", key, json!({"email": key, "notes": "n"}))
            .expect("a put that omits the password");

        store_mark(&mut db);
        let dump = dump_all(&db, true).unwrap();
        load_all(&mut db, &dump).expect("its own dump loads");

        store_mark(&mut db);
        drop(db);
        rekey_with(&dir, ROTATED).expect("the key can be rotated away from it");
    }

    #[test]
    fn a_first_import_leaves_edits_and_deletions_alone() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let mut db = replica(&tmp.path().join("db"));
        let doc = json!({"computers": [
            {"label": "m1", "notes": "csv"},
            {"label": "m2", "notes": "csv"},
            {"label": "m3", "notes": "csv"},
        ]});
        import_rows(&mut db, &doc, Existing::Keep).unwrap();
        put_row(
            &mut db,
            "computers",
            "m1",
            json!({"label": "m1", "notes": "edit"}),
        )
        .unwrap();
        delete_row(&mut db, "computers", "m2").unwrap();

        let report = import_rows(&mut db, &doc, Existing::Keep).unwrap();
        assert_eq!(report["operations_committed"], json!(0));
        let rows = scan_table(&db.view(), "computers").unwrap();
        assert_eq!(rows.keys().collect::<Vec<_>>(), ["m1", "m3"]);
        assert_eq!(rows["m1"]["notes"], json!("edit"));
    }

    #[test]
    fn bootstrap_never_stores_a_password_flag_and_a_second_run_writes_nothing() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let sheets = tmp.path().join("sheets");
        std::fs::create_dir(&sheets).unwrap();
        for (name, text) in [
            (
                "alab-phones.csv",
                "phone\n555-234-5678,carrier,mobile,dev,note\n",
            ),
            (
                "alab-computers.csv",
                "label\nm1,Apple,arm64,macOS,M4,128GB,1TB,,desk,,home,note\n",
            ),
            (
                "alab-emails.csv",
                "email\nreal@example.com,,pw-synth,active,proton,,,\nflag@example.com,,1,active,proton,,,\n",
            ),
            ("ai-accounts.csv", "m\n,real@example.com,m1,,1,,,,,,note\n"),
        ] {
            std::fs::write(sheets.join(name), text).unwrap();
        }
        let mut db = replica(&tmp.path().join("db"));

        let first = bootstrap_from_secrets(&mut db, &sheets).unwrap();
        assert_eq!(first["imported"]["emails"], json!(2));
        let view = db.view();
        let password =
            |key: &str| get_row(&view, "emails", key).unwrap().unwrap()["password"].clone();
        assert_eq!(password("real@example.com"), json!("pw-synth"));
        assert_eq!(password("flag@example.com"), Value::Null);

        let again = bootstrap_from_secrets(&mut db, &sheets).unwrap();
        assert_eq!(again["operations_committed"], json!(0));

        // A key that stands is left alone, so a mark made since does not refuse the sheet.
        put_row(
            &mut db,
            "emails",
            "flag@example.com",
            json!({"email": "flag@example.com", "protected": true}),
        )
        .unwrap();
        let again = bootstrap_from_secrets(&mut db, &sheets).expect("the mark stands");
        assert_eq!(again["operations_committed"], json!(0));
    }

    #[test]
    fn bootstrap_reads_real_passwords_from_xlsx_when_csv_holds_flags() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let sheets = tmp.path().join("sheets");
        std::fs::create_dir(&sheets).unwrap();
        for (name, text) in [
            (
                "alab-phones.csv",
                "phone\n555-234-5678,carrier,mobile,dev,note\n",
            ),
            (
                "alab-computers.csv",
                "label\nm1,Apple,arm64,macOS,M4,128GB,1TB,,desk,,home,note\n",
            ),
            (
                "alab-emails.csv",
                "email\nflag@example.com,,1,active,proton,,,\nprot@example.com,,1,active,proton,,1,\nalias@example.com,flag@example.com,1,active,proton,,,\n",
            ),
            ("ai-accounts.csv", "m\n,flag@example.com,m1,,1,,,,,,note\n"),
        ] {
            std::fs::write(sheets.join(name), text).unwrap();
        }
        let parts: &[(&str, &str)] = &[
            (
                "xl/workbook.xml",
                r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Emails" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Email Address</t></is></c><c r="C1" t="inlineStr"><is><t>Password</t></is></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>flag@example.com</t></is></c><c r="C2" t="inlineStr"><is><t>xlsx-real-pw-42</t></is></c></row><row r="3"><c r="A3" t="inlineStr"><is><t>prot@example.com</t></is></c><c r="C3" t="inlineStr"><is><t>xlsx-prot-pw-99</t></is></c></row><row r="4"><c r="A4" t="inlineStr"><is><t>alias@example.com</t></is></c><c r="C4" t="inlineStr"><is><t>xlsx-alias-pw-99</t></is></c></row></sheetData></worksheet>"#,
            ),
        ];
        let mut zip_bytes = Vec::new();
        let mut central = Vec::new();
        for (name, body_str) in parts {
            let body = body_str.as_bytes();
            let offset = zip_bytes.len() as u32;
            let crc = aterm_codec::crc32::Crc32::of(body);
            let len = body.len() as u32;
            let nlen = name.len() as u16;
            zip_bytes.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            zip_bytes.extend_from_slice(&20u16.to_le_bytes());
            zip_bytes.extend_from_slice(&0u16.to_le_bytes());
            zip_bytes.extend_from_slice(&0u16.to_le_bytes());
            zip_bytes.extend_from_slice(&0u32.to_le_bytes());
            zip_bytes.extend_from_slice(&crc.to_le_bytes());
            zip_bytes.extend_from_slice(&len.to_le_bytes());
            zip_bytes.extend_from_slice(&len.to_le_bytes());
            zip_bytes.extend_from_slice(&nlen.to_le_bytes());
            zip_bytes.extend_from_slice(&0u16.to_le_bytes());
            zip_bytes.extend_from_slice(name.as_bytes());
            zip_bytes.extend_from_slice(body);

            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&len.to_le_bytes());
            central.extend_from_slice(&len.to_le_bytes());
            central.extend_from_slice(&nlen.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let cd_offset = zip_bytes.len() as u32;
        let cd_len = central.len() as u32;
        let count = parts.len() as u16;
        zip_bytes.extend_from_slice(&central);
        zip_bytes.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        zip_bytes.extend_from_slice(&0u16.to_le_bytes());
        zip_bytes.extend_from_slice(&0u16.to_le_bytes());
        zip_bytes.extend_from_slice(&count.to_le_bytes());
        zip_bytes.extend_from_slice(&count.to_le_bytes());
        zip_bytes.extend_from_slice(&cd_len.to_le_bytes());
        zip_bytes.extend_from_slice(&cd_offset.to_le_bytes());
        zip_bytes.extend_from_slice(&0u16.to_le_bytes());
        std::fs::write(sheets.join("AI Email Accounting.xlsx"), zip_bytes).unwrap();

        let mut db = replica(&tmp.path().join("db"));
        bootstrap_from_secrets(&mut db, &sheets).unwrap();
        let view = db.view();
        let password =
            |key: &str| get_row(&view, "emails", key).unwrap().unwrap()["password"].clone();
        assert_eq!(password("flag@example.com"), json!("xlsx-real-pw-42"));
        assert_eq!(password("prot@example.com"), Value::Null);
        assert_eq!(password("alias@example.com"), Value::Null);
    }

    #[test]
    fn a_snapshot_with_a_misspelled_table_is_refused_rather_than_ignored() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let mut db = replica(&tmp.path().join("db"));
        assert!(load_all(&mut db, &json!({"ai-accounts": []})).is_err());
        assert!(load_all(&mut db, &json!({"computers": {}})).is_err());
    }

    #[test]
    fn concurrent_commands_wait_their_turn() {
        let tmp = fleet_fixture();
        let dir = tmp.path().join("db");
        put_row(
            &mut open_at(&dir).unwrap(),
            "computers",
            "m1",
            json!({"label": "m1"}),
        )
        .unwrap();
        let readers: Vec<_> = (0..12)
            .map(|_| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    let db = open_at(&dir)?;
                    tables_summary(&db)
                })
            })
            .collect();
        for reader in readers {
            let summary = reader.join().unwrap().expect("each reader gets its turn");
            assert_eq!(summary["tables"]["computers"], json!(1));
        }
    }

    #[test]
    fn a_peer_that_cannot_be_reached_fails_the_sync() {
        let tmp = fleet_fixture();
        let gone = aterm_uds::kept_out(|| std::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap()
            .local_addr()
            .unwrap();
        let report = sync_peers(&tmp.path().join("db"), &[gone]).unwrap();
        assert_eq!(report["ok"], json!(false));
        let peer = &report["peers"][0];
        assert_eq!(peer["ok"], json!(false));
        assert_eq!(peer["caught_up"], json!(false));
        assert!(peer["error"].as_str().is_some());
        assert_eq!(peer["sent"], json!(0));
    }

    #[test]
    fn tables_reports_how_much_of_each_quota_is_used() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let mut db = replica(&tmp.path().join("db"));
        put_row(&mut db, "computers", "m1", json!({"label": "m1"})).unwrap();
        let capacity = tables_summary(&db).unwrap()["capacity"].clone();
        assert_eq!(capacity["operations"], json!(1));
        assert_eq!(capacity["writer_operations"], json!(1));
        assert_eq!(
            capacity["writer_operations_limit"],
            json!(MAX_WRITER_OPERATIONS as u64)
        );
        assert!(capacity["writer_bytes"].as_u64().unwrap() > 0);
        assert!(capacity["journal_bytes"].as_u64().unwrap() > 0);
    }

    #[test]
    fn fleet_sync_requires_a_settled_inventory_round() {
        let peer = "127.0.0.1:7485".parse().unwrap();
        let mut calls = 0;
        let result = sync_peer_with(peer, |_| {
            calls += 1;
            Ok(if calls == 1 {
                network::Report {
                    sent: 3,
                    received: 2,
                    deferred: 0,
                }
            } else {
                network::Report::default()
            })
        });
        assert_eq!(calls, 2);
        assert_eq!(result["ok"], json!(true));
        assert_eq!(result["caught_up"], json!(true));
        assert_eq!(result["sent"], json!(3));
        assert_eq!(result["received"], json!(2));
        assert!(result.get("error").is_none());

        for (report, expected_calls) in [
            (
                network::Report {
                    sent: 0,
                    received: 0,
                    deferred: 1,
                },
                1,
            ),
            (
                network::Report {
                    sent: 1,
                    received: 0,
                    deferred: 0,
                },
                80,
            ),
        ] {
            let mut calls = 0;
            let result = sync_peer_with(peer, |_| {
                calls += 1;
                Ok(report)
            });
            assert_eq!(calls, expected_calls);
            assert_eq!(result["ok"], json!(false));
            assert_eq!(result["caught_up"], json!(false));
            assert_eq!(result["sent"], json!(report.sent * calls));
            assert_eq!(result["deferred"], json!(report.deferred));
            assert!(result.get("error").and_then(Value::as_str).is_some());
        }
    }

    #[test]
    fn a_sync_beside_a_neighbour_gets_every_row() {
        let tmp = fleet_fixture();
        let secret = resolve_fleet_secret(&tmp.path().join("a")).unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        local::ensure_fleet(&a, &secret).unwrap();
        local::ensure_fleet(&b, &secret).unwrap();
        let mut peer = Database::open(&a).unwrap();
        for n in 0..20 {
            let key = format!("m{n}");
            put_row(&mut peer, "computers", &key, json!({"label": key})).unwrap();
        }
        drop(peer);

        let listener =
            aterm_uds::kept_out(|| std::net::TcpListener::bind("127.0.0.1:0")).expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let server = {
            let (a, running) = (a.clone(), running.clone());
            std::thread::spawn(move || {
                network::serve_fleet_until(
                    &a,
                    listener,
                    running,
                    std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
                )
            })
        };
        let neighbour = {
            let (b, running) = (b.clone(), running.clone());
            std::thread::spawn(move || {
                while running.load(std::sync::atomic::Ordering::Acquire) {
                    let held = Database::open(&b);
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    drop(held);
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            })
        };
        let report = sync_peers(&b, &[addr]).unwrap();
        running.store(false, std::sync::atomic::Ordering::Release);
        neighbour.join().unwrap();
        let _ = server.join();

        assert_eq!(report["ok"], json!(true), "{report}");
        let db = Database::open(&b).unwrap();
        assert_eq!(scan_table(&db.view(), "computers").unwrap().len(), 20);
    }

    #[test]
    fn what_a_peer_holds_and_this_replica_does_not_keeps_a_sync_from_being_caught_up() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let mut a = replica(&tmp.path().join("a"));
        put_row(&mut a, "computers", "m1", json!({"label": "m1"})).unwrap();
        let held: BTreeSet<_> = a.history.index.inventory().copied().collect();
        let bdir = tmp.path().join("b");
        let b = replica(&bdir);
        drop(a);
        drop(b);
        assert_eq!(network::unretained(&bdir, &held).unwrap(), held.len());

        let adir = tmp.path().join("a");
        let (mut a, mut b) = (
            Database::open(&adir).unwrap(),
            Database::open(&bdir).unwrap(),
        );
        exchange(&mut a, &mut b);
        drop(b);
        assert_eq!(network::unretained(&bdir, &held).unwrap(), 0);
    }

    #[test]
    fn tables_is_not_ok_while_a_row_waits_for_an_operation_it_depends_on() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let mut a = replica(&tmp.path().join("a"));
        put_row(&mut a, "computers", "m1", json!({"label": "m1"})).unwrap();
        put_row(&mut a, "computers", "m2", json!({"label": "m2"})).unwrap();
        let later = a
            .history
            .index
            .inventory()
            .find(|id| {
                a.history
                    .index
                    .get(id)
                    .is_some_and(|header| header.sequence == 2)
            })
            .map(|id| a.history.wire(id).unwrap().to_vec())
            .unwrap();

        let mut b = replica(&tmp.path().join("b"));
        b.ingest(&later).unwrap();
        let summary = tables_summary(&b).unwrap();
        assert_eq!(summary["pending_operations"], json!(1));
        assert_eq!(summary["ok"], json!(false));
    }

    #[test]
    fn fleet_sync_keeps_partial_progress_on_failure() {
        let mut calls = 0;
        let result = sync_peer_with("127.0.0.1:7485".parse().unwrap(), |_| {
            calls += 1;
            if calls == 1 {
                Ok(network::Report {
                    sent: 2,
                    received: 3,
                    deferred: 1,
                })
            } else {
                Err(Error::new("test peer disconnected"))
            }
        });
        assert_eq!(calls, 2);
        assert_eq!(result["ok"], json!(false));
        assert_eq!(result["caught_up"], json!(false));
        assert_eq!(result["sent"], json!(2));
        assert_eq!(result["received"], json!(3));
        assert_eq!(result["deferred"], json!(1));
        assert_eq!(result["error"], json!("test peer disconnected"));

        let mut calls = 0;
        let peer = "127.0.0.1:7485".parse().unwrap();
        let refused = sync_peer_with(peer, |_| {
            calls += 1;
            Err(Error::from(std::io::Error::from_raw_os_error(65)))
        });
        assert_eq!(
            calls, 1,
            "a failed round (no route to the host) is not retried"
        );
        assert_eq!(refused["ok"], json!(false));
        assert_eq!(refused["caught_up"], json!(false));
        assert!(refused["error"].as_str().unwrap().contains("os error 65"));

        let root = crate::tests::tmp();
        let absent = root.path().join("no-peer");
        assert!(sync_peers(&absent, &[]).is_err());
        assert!(!absent.exists(), "an empty sync must not create a database");
    }

    fn fleet_fixture() -> aterm_tempfile::TempDir {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let key = hex::encode_lower(&crypto::random::<32>().unwrap());
        aterm_uds::private_file::write_private(tmp.path(), "fleet.key", key.as_bytes()).unwrap();
        tmp
    }

    #[test]
    fn missing_fleet_key_does_not_create_an_enrollment() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = aterm_tempfile::tempdir().unwrap();
        let absent = tmp.path().join("absent");
        assert!(read_fleet_secret(std::slice::from_ref(&absent)).is_err());
        assert!(!absent.exists());
        assert!(!tmp.path().join("fleet.key").exists());
        let empty = tmp.path().join("ordinary-folder");
        std::fs::create_dir(&empty).unwrap();
        std::fs::set_permissions(&empty, std::fs::Permissions::from_mode(0o755)).unwrap();
        let provisioned = fleet_fixture();
        let before = std::fs::read(provisioned.path().join("fleet.key")).unwrap();
        assert_eq!(
            read_fleet_secret(&[empty.clone(), provisioned.path().to_owned()]).unwrap(),
            before
        );
        assert!(!empty.join("fleet.key").exists());
    }

    #[test]
    fn invalid_fleet_key_cannot_fall_through_to_another_fleet() {
        use std::os::unix::fs::PermissionsExt as _;
        let first = fleet_fixture();
        let other = fleet_fixture();
        let candidates = [first.path().to_owned(), other.path().to_owned()];
        let key = first.path().join("fleet.key");
        let before = std::fs::read(&key).unwrap();
        assert_eq!(read_fleet_secret(&candidates).unwrap(), before);
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_fleet_secret(&candidates).is_err());
        assert_eq!(std::fs::read(&key).unwrap(), before);
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        aterm_uds::private_file::write_private(first.path(), "fleet.key", b"invalid").unwrap();
        assert!(read_fleet_secret(&candidates).is_err());
        assert_eq!(std::fs::read(key).unwrap(), b"invalid");
    }

    #[test]
    fn fleet_four_tables_crud_and_peer_sync() {
        let tmp = fleet_fixture();
        let node_a = tmp.path().join("node-a");
        let node_b = tmp.path().join("node-b");

        let mut db_a = open_at(&node_a).expect("open node_a");
        // Replicas must never retain `root_seed` (avoiding control-stream forks).
        assert!(!db_a.credentials.has_control_authority());

        put_row(
            &mut db_a,
            "computers",
            "m104",
            json!({
                "label": "m104",
                "hostname": "m104.local",
                "ip": "127.0.0.1",
                "nickname": "desk-max",
                "aliases": ["m104-alias"],
                "chip": "M4 Max",
                "memory_gb": 128,
                "status": "active",
                "notes": "Desk workstation with multiline\nnote"
            }),
        )
        .expect("put computer");

        put_row(
            &mut db_a,
            "phones",
            "+1 (555) 234-5678",
            json!({
                "phone": "+1 (555) 234-5678",
                "label": "phone-1",
                "owner_carrier": "AT&T",
                "status": "active"
            }),
        )
        .expect("put phone");

        // Phone lookup normalizes formatting variants to 555-234-5678.
        let ph_row = get_row(&db_a.view(), "phones", "555.234.5678")
            .expect("get phone")
            .expect("phone present");
        assert_eq!(ph_row["phone"], json!("555-234-5678"));

        put_row(
            &mut db_a,
            "emails",
            "root-m2@example.com",
            json!({
                "email": "root-m2@example.com",
                "provider": "proton",
                "account_type": "root",
                "password": "test-root-password-123",
                "phone": "555-234-5678",
                "protected": false,
                "assigned_computers": ["m104"],
                "notes": "Catch-all Proton root"
            }),
        )
        .expect("put root email");

        put_row(
            &mut db_a,
            "emails",
            "alias-m2@example.com",
            json!({
                "email": "alias-m2@example.com",
                "provider": "proton",
                "account_type": "alias",
                "root_account": "root-m2@example.com",
                "protected": false
            }),
        )
        .expect("put alias email");

        // Protected email must reject storing a password both via `put_row` and raw `Command::decode`.
        assert!(
            put_row(
                &mut db_a,
                "emails",
                "personal-protected@example.com",
                json!({
                    "email": "personal-protected@example.com",
                    "provider": "google",
                    "account_type": "root",
                    "password": "should-fail",
                    "protected": true
                }),
            )
            .is_err()
        );
        let forged_skey = storage_key("emails", "personal-protected@example.com").unwrap();
        assert!(
            validate_email_record(
                &forged_skey,
                &json!({
                    "email": "personal-protected@example.com",
                    "protected": true,
                    "password": "forged-wire-password"
                })
            )
            .is_err()
        );
        assert!(
            validate_email_record(
                &forged_skey,
                &json!({
                    "_deleted": true,
                    "password": "smuggled-in-tombstone"
                })
            )
            .is_err()
        );

        put_row(
            &mut db_a,
            "emails",
            "personal-protected@example.com",
            json!({
                "email": "personal-protected@example.com",
                "provider": "google",
                "account_type": "root",
                "protected": true
            }),
        )
        .expect("put protected email without password");

        // Once `protected: true`, attempting to downgrade `protected` to `false` or attach a password fails.
        assert!(
            put_row(
                &mut db_a,
                "emails",
                "personal-protected@example.com",
                json!({
                    "email": "personal-protected@example.com",
                    "provider": "google",
                    "account_type": "root",
                    "protected": false,
                    "password": "downgrade-attempt"
                }),
            )
            .is_err()
        );

        put_row(
            &mut db_a,
            "ai_accounts",
            "acc-001",
            json!({
                "id": "acc-001",
                "provider": "anthropic",
                "service": "claude",
                "email": "alias-m2@example.com",
                "receiving_mailbox": "root-m2@example.com",
                "assigned_computers": ["m104"],
                "status": "active"
            }),
        )
        .expect("put ai_account");

        // Redaction hides password by default and reveals when requested.
        let raw_email = get_row(&db_a.view(), "emails", "root-m2@example.com")
            .unwrap()
            .unwrap();
        let redacted = redact_row("emails", raw_email.clone(), false);
        assert_eq!(redacted["password"], json!("<redacted>"));
        assert_eq!(redacted["has_password"], json!(true));
        let revealed = redact_row("emails", raw_email, true);
        assert_eq!(revealed["password"], json!("test-root-password-123"));

        // Deleting a non-existent row must error.
        assert!(delete_row(&mut db_a, "computers", "nonexistent-host").is_err());

        let summary = tables_summary(&db_a).expect("summary");
        assert_eq!(summary["ok"], json!(true));
        assert_eq!(summary["authority_fault"], json!(false));
        assert_eq!(summary["conflicts"], json!(0));
        assert_eq!(summary["tables"]["computers"], json!(1));
        assert_eq!(summary["tables"]["phones"], json!(1));
        assert_eq!(summary["tables"]["emails"]["total"], json!(3));
        assert_eq!(summary["tables"]["emails"]["root_with_password"], json!(1));
        assert_eq!(summary["tables"]["emails"]["aliases"], json!(1));
        assert_eq!(
            summary["tables"]["emails"]["protected_no_password"],
            json!(1)
        );
        assert_eq!(summary["tables"]["ai_accounts"], json!(1));

        // Replicate from node_a to fresh replica node_b with zero invitations or Commons.
        let mut db_b = open_at(&node_b).expect("open node_b");
        for id in db_a.history.index.inventory() {
            let wire = db_a.history.wire(id).expect("wire").to_vec();
            db_b.ingest(&wire).expect("ingest on node_b");
        }
        drop(db_a);

        let view_b = db_b.view();
        let email_row = get_row(&view_b, "emails", "root-m2@example.com")
            .expect("get_row")
            .expect("present");
        assert_eq!(email_row["password"], json!("test-root-password-123"));
        let comp_row = get_row(&view_b, "computers", "desk-max")
            .expect("get_row by nickname")
            .expect("present");
        assert_eq!(comp_row["label"], json!("m104"));
        assert_eq!(
            comp_row["notes"],
            json!("Desk workstation with multiline\nnote")
        );
        drop(db_b);

        // Also verify live TLS anti-entropy sync over aterm-net (FleetOp::Database).
        let node_c = tmp.path().join("node-c");
        ensure_fleet_at(&node_c).expect("ensure_fleet node_c");
        let listener =
            aterm_uds::kept_out(|| std::net::TcpListener::bind("127.0.0.1:0")).expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let running_srv = running.clone();
        let node_b_srv = node_b.clone();
        let srv = std::thread::spawn(move || {
            network::serve_fleet_until(
                &node_b_srv,
                listener,
                running_srv,
                std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            )
        });
        let sync_res = sync_peers(&node_c, &[addr]).expect("sync_peers over TLS");
        let node_d = tmp.path().join("node-d");
        ensure_fleet_at(&node_d).expect("ensure_fleet node_d");
        let delegated = crate::cli::run(&[
            "sync".to_owned(),
            "--dir".to_owned(),
            node_d.to_string_lossy().into_owned(),
            "--peer".to_owned(),
            addr.to_string(),
        ])
        .expect("explicit-directory sync over TLS");
        assert_eq!(delegated["ok"], json!(true));
        assert_eq!(delegated["caught_up"], json!(true));
        running.store(false, std::sync::atomic::Ordering::Release);
        let _ = srv.join();
        assert_eq!(sync_res["ok"], json!(true));
        let db_c = open_at(&node_c).expect("open node_c");
        let summary_c = tables_summary(&db_c).expect("summary_c");
        assert_eq!(summary_c["ok"], json!(true));
        assert_eq!(summary_c["tables"]["emails"]["total"], json!(3));
        let db_d = open_at(&node_d).expect("open delegated replica");
        assert_eq!(
            db_d.history
                .index
                .inventory()
                .copied()
                .collect::<BTreeSet<_>>(),
            db_c.history
                .index
                .inventory()
                .copied()
                .collect::<BTreeSet<_>>(),
        );
    }

    #[test]
    fn fleet_ambiguous_alias_and_broken_fk_reporting() {
        let tmp = fleet_fixture();
        let dir = tmp.path().join("node");
        let mut db = open_at(&dir).expect("open");

        put_row(
            &mut db,
            "computers",
            "m1",
            json!({"label": "m1", "aliases": ["shared-alias"]}),
        )
        .unwrap();
        put_row(
            &mut db,
            "computers",
            "m2",
            json!({"label": "m2", "aliases": ["shared-alias"]}),
        )
        .unwrap();

        // Ambiguous alias must return an error rather than picking an arbitrary computer.
        assert!(get_row(&db.view(), "computers", "shared-alias").is_err());

        // Broken foreign key in ai_accounts must set ok = false in tables_summary.
        put_row(
            &mut db,
            "ai_accounts",
            "acc-broken",
            json!({
                "id": "acc-broken",
                "email": "missing@example.com",
                "assigned_computers": ["missing-comp"]
            }),
        )
        .unwrap();
        let summary = tables_summary(&db).unwrap();
        assert_eq!(summary["ok"], json!(false));
        assert!(
            !summary["integrity"]["broken_account_emails"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            !summary["integrity"]["broken_computer_refs"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}
