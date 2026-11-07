// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `ai-mailbox.toml` as an import source (docs/DESIGN-alab-one-mesh-2026-11-05.md §4.7,
//! `--mailbox-toml`): the harness's mailbox file, which the vault materializes on every member,
//! read into MAILBOX rows so its readers can move to the registry and the file can go.
//!
//! The file names mailboxes two ways (`aterm-agent/src/harness/mailbox_routes.rs`,
//! `headless_oauth.rs` `MailboxConfig::parse_toml`): each `[[mailboxes]]` entry, and the
//! top-level fields (`user`, `password`, …), the legacy mailbox every address falls back to.
//! Both may sit at the file's root or under `[ai.mailbox]`, `[harness.mailbox]` or `[mailbox]`,
//! as the harness reads them. Each mailbox becomes one [`Sheet`] row of kind
//! [`SheetKind::MailboxToml`], the top-level one first, and each field one column named as the
//! file spells it, so a finding names a field by its coordinate and its name.
//!
//! **Every field lands or is refused.** Each field this build knows, under every spelling the
//! harness reads ([`FIELDS`]), goes to its cell:
//!
//! - `login` (`user`, `username`, `imap_user`): the mailbox's `login`, an address row; the
//!   mailbox's key derives from it (`keys::mailbox_pk`).
//! - `password`: the login address's own `password`, a `Secret` (one fact, one cell, as A9
//!   reads it).
//! - `mailbox_password` and `totp_secret`: the mailbox's `Secret`s.
//! - `webmail_url`, `imap_url`, `http_url`: `webmail`, `imap` and `http`; `url` only where its
//!   scheme says which (`imap://`, `imaps://`), since the harness tells a webmail page from a
//!   mail API by a guess this build does not repeat.
//! - `receives`: each exact address a `receives` element, each `*@<domain>` and `*` a
//!   `receives_domains` item. The top-level mailbox is the fallback of every address: it
//!   receives `*` unless an entry already does, which routes alike, since an entry's `*` beats
//!   it.
//!
//! Any other field (a bearer token, a per-address `passwords` map, `accounts`, a `*_file`
//! pointer, a maildir, a known field of the wrong type) has no column: it is an
//! [`Kind::Unmapped`] finding by its name, never its value, so the file is `covered` only once
//! every field landed. A URL that carries a `user:password@` part is held as a secret and
//! refused ([`Kind::CredentialShaped`]). Two spellings of one field that disagree, and an entry
//! and the top-level mailbox naming one login with different values, are
//! [`Kind::SourcesDisagree`]: the entry's value is written, as the harness reads it, and the
//! other stays uncovered.
//!
//! **No value leaves the sheet's secrets.** A password, second password or TOTP seed is held as
//! a [`Secret`] at its coordinate ([`Sheet::secrets`]), never in `rows`. The source's key and
//! SHA-256 are taken over the file's fields with each secret as `set`: no unsalted digest of a
//! password goes into the shared `source` row, and a changed password in the same file is the
//! same source, so its own earlier import's cell takes the new value.

use super::{ADDRESS, Coord, Finding, Kind, MAILBOX, Mode, Planner, Sheet, SheetKind, coord};
use crate::{
    Error, Result,
    typed::{cell, guard, keys, secret::Secret},
};
use aterm_codec::hex;
use aterm_digest::Sha256;
use aterm_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// Where one field of the file goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Field {
    Login,
    Password,
    MailboxPassword,
    Totp,
    Webmail,
    Imap,
    Http,
    /// A bare `url`: an IMAP one by its scheme, else refused.
    Url,
    Receives,
}

impl Field {
    /// Whether the field's value is a credential, held as a [`Secret`].
    const fn secret(self) -> bool {
        matches!(self, Self::Password | Self::MailboxPassword | Self::Totp)
    }

    /// Whether the field's value is a URL, whose `user[:password]@` part makes it a secret.
    const fn url(self) -> bool {
        matches!(self, Self::Webmail | Self::Imap | Self::Http | Self::Url)
    }
}

/// Every spelling of a field the harness reads, in the order it reads them: the first spelling
/// a mailbox gives is the one written.
pub(super) const FIELDS: [(&str, Field); 25] = [
    ("login", Field::Login),
    ("user", Field::Login),
    ("username", Field::Login),
    ("imap_user", Field::Login),
    ("password", Field::Password),
    ("pass", Field::Password),
    ("imap_password", Field::Password),
    ("imap_pass", Field::Password),
    ("proton_password", Field::Password),
    ("webmail_password", Field::Password),
    ("mailbox_password", Field::MailboxPassword),
    ("second_password", Field::MailboxPassword),
    ("totp_secret", Field::Totp),
    ("totp", Field::Totp),
    ("otpauth", Field::Totp),
    ("totp_uri", Field::Totp),
    ("otp_secret", Field::Totp),
    ("webmail_url", Field::Webmail),
    ("webmail", Field::Webmail),
    ("imap_url", Field::Imap),
    ("imap", Field::Imap),
    ("http_url", Field::Http),
    ("webhook_url", Field::Http),
    ("url", Field::Url),
    ("receives", Field::Receives),
];

/// The field a column's name spells, where this build knows it.
pub(super) fn field(name: &str) -> Option<Field> {
    FIELDS
        .iter()
        .find(|(spelled, _)| *spelled == name)
        .map(|(_, field)| *field)
}

/// The tables the harness reads the top-level mailbox from, first match wins.
const WRAPPERS: [&[&str]; 3] = [&["ai", "mailbox"], &["harness", "mailbox"], &["mailbox"]];

/// The largest file read.
pub const MAX_BYTES: usize = 4 * 1024 * 1024;

/// The most mailboxes one file holds.
const MAX_MAILBOXES: usize = 256;

/// A column's name as a finding may print it: the file's own spelling where it is a plain key
/// (letters, digits, `_`, `-`, `.`, at most 48) that is not shaped like a credential, else a
/// placeholder. A name is no value, so one that names a secret (`bearer_token`) is printed: it
/// says which field has no column.
fn column_name(raw: &str) -> String {
    let trimmed = raw.trim();
    if !trimmed.is_empty()
        && trimmed.len() <= 48
        && trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        && !guard::credential_shaped(trimmed)
    {
        trimmed.to_owned()
    } else {
        "(a field name)".to_owned()
    }
}

/// Whether a URL names a user (`scheme://user[:password]@host`): its authority holds an `@`.
fn has_userinfo(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split(['/', '?', '#'])
        .next()
        .is_some_and(|authority| authority.contains('@'))
}

/// One mailbox of the file as read: each field by its spelling, a string value as text, a
/// secret as a [`Secret`], a wrong-typed one by a marked name.
#[derive(Default)]
struct Read {
    cells: BTreeMap<String, String>,
    secrets: BTreeMap<String, Arc<Secret>>,
}

impl Read {
    fn is_empty(&self) -> bool {
        self.cells.is_empty() && self.secrets.is_empty()
    }

    /// Read `key = value` of a mailbox. A field of the wrong type keeps its place under its
    /// name and a `{table}`, `[array]` or `(value)` mark, so no column of this build matches it.
    fn field(&mut self, key: &str, value: &aterm_toml::Value) {
        let name = column_name(key);
        let known = field(&name);
        match (known, value) {
            (Some(Field::Receives), aterm_toml::Value::Array(items))
                if items.iter().all(|item| item.as_str().is_some()) =>
            {
                let lines: Vec<&str> = items
                    .iter()
                    .filter_map(aterm_toml::Value::as_str)
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .collect();
                self.cells.insert(name, lines.join("\n"));
            }
            (Some(field), aterm_toml::Value::String(text)) => {
                let text = text.trim();
                if text.is_empty() {
                    return;
                }
                if field.secret() || (field.url() && has_userinfo(text)) {
                    self.secrets
                        .insert(name, Arc::new(Secret::new(text.to_owned())));
                } else {
                    self.cells.insert(name, text.to_owned());
                }
            }
            (_, aterm_toml::Value::Table(_)) => {
                self.cells
                    .insert(format!("{name}{{table}}"), "1".to_owned());
            }
            (_, aterm_toml::Value::Array(_)) => {
                self.cells.insert(format!("{name}[array]"), "1".to_owned());
            }
            (Some(_), _) => {
                self.cells.insert(format!("{name}(value)"), "1".to_owned());
            }
            (None, _) => {
                self.cells.insert(name, "1".to_owned());
            }
        }
    }
}

/// Every string a TOML value holds, wiped: what the parser copied out of the file.
fn wipe(value: aterm_toml::Value) {
    match value {
        aterm_toml::Value::String(mut text) => zeroize::Zeroize::zeroize(&mut text),
        aterm_toml::Value::Array(items) => items.into_iter().for_each(wipe),
        aterm_toml::Value::Table(table) => wipe_table(table),
        _ => {}
    }
}

fn wipe_table(table: aterm_toml::Table) {
    for (_, value) in table {
        wipe(value);
    }
}

/// The table at `path` under `doc`.
fn table_at<'d>(doc: &'d aterm_toml::Table, path: &[&str]) -> Option<&'d aterm_toml::Table> {
    let (first, rest) = path.split_first()?;
    let table = doc.get(first)?.as_table()?;
    if rest.is_empty() {
        Some(table)
    } else {
        table_at(table, rest)
    }
}

/// The mailboxes of `doc`: the top-level one (possibly empty), then each `[[mailboxes]]` entry
/// of the wrapper table and of the root, and a `Read` of every other key outside them, which
/// names no mailbox and so is refused.
fn mailboxes(doc: &aterm_toml::Table) -> Result<(Read, Vec<Read>, Read)> {
    let wrapper = WRAPPERS
        .iter()
        .find(|path| table_at(doc, path).is_some())
        .copied();
    let top_table = wrapper.and_then(|path| table_at(doc, path)).unwrap_or(doc);
    let mut top = Read::default();
    let mut entries = Vec::new();
    let mut stray = Read::default();
    let read_entries = |table: &aterm_toml::Table, entries: &mut Vec<Read>| -> Result<()> {
        match table.get("mailboxes") {
            None => Ok(()),
            Some(aterm_toml::Value::Array(items)) => {
                for item in items {
                    let Some(entry) = item.as_table() else {
                        let mut bad = Read::default();
                        bad.field("mailboxes", item);
                        entries.push(bad);
                        continue;
                    };
                    let mut read = Read::default();
                    for (key, value) in entry {
                        read.field(key, value);
                    }
                    entries.push(read);
                    if entries.len() > MAX_MAILBOXES {
                        return Err(Error::new("the mailbox file names too many mailboxes"));
                    }
                }
                Ok(())
            }
            Some(other) => {
                let mut bad = Read::default();
                bad.field("mailboxes", other);
                entries.push(bad);
                Ok(())
            }
        }
    };
    for (key, value) in top_table {
        if key != "mailboxes" {
            top.field(key, value);
        }
    }
    read_entries(top_table, &mut entries)?;
    if let Some(path) = wrapper {
        read_entries(doc, &mut entries)?;
        // What the root holds besides the wrapper and its entries names no mailbox.
        for (key, value) in doc {
            if key == "mailboxes" {
                continue;
            }
            if path.first() == Some(&key.as_str()) {
                if let (Some(inner), [_, second]) = (value.as_table(), path) {
                    for (inner_key, inner_value) in inner {
                        if inner_key != second {
                            stray.field(&format!("{key}.{inner_key}"), inner_value);
                        }
                    }
                }
                continue;
            }
            stray.field(key, value);
        }
    }
    Ok((top, entries, stray))
}

/// The mailbox file `bytes`, file `name`, as one fill-only source: the top-level mailbox at row
/// 2 (empty where the file states none), then each entry, then a row of whatever else the file
/// holds.
///
/// # Errors
/// The bytes are not UTF-8 TOML within [`MAX_BYTES`], or the file name cannot be recorded.
pub fn mailbox_sheet(name: &str, bytes: &[u8]) -> Result<Sheet> {
    if bytes.len() > MAX_BYTES {
        return Err(Error::new(
            "the mailbox file is larger than this import reads",
        ));
    }
    let name = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
    if name.is_empty() || guard::text(name, 128).is_err() {
        return Err(Error::new("the mailbox file's name cannot be recorded"));
    }
    // Never the parser's own words, which may quote a line.
    let malformed = || Error::new("the mailbox file is not TOML this build reads");
    let text = std::str::from_utf8(bytes).map_err(|_| malformed())?;
    let doc: aterm_toml::Table = aterm_toml::from_str(text).map_err(|_| malformed())?;
    let read = mailboxes(&doc);
    wipe_table(doc);
    let (top, entries, stray) = read?;
    let mut rows_read = vec![top];
    rows_read.extend(entries);
    if !stray.is_empty() {
        rows_read.push(stray);
    }
    // Every spelling any mailbox gives, the known ones first in their reading order.
    let given: BTreeSet<&str> = rows_read
        .iter()
        .flat_map(|read| read.cells.keys().chain(read.secrets.keys()))
        .map(String::as_str)
        .collect();
    let mut headers: Vec<String> = FIELDS
        .iter()
        .map(|(spelled, _)| *spelled)
        .filter(|spelled| given.contains(*spelled))
        .map(str::to_owned)
        .collect();
    headers.extend(
        given
            .iter()
            .filter(|g| field(g).is_none())
            .map(|g| (*g).to_owned()),
    );
    let mut rows = Vec::with_capacity(rows_read.len());
    let mut secrets = BTreeMap::new();
    let mut canonical = Vec::with_capacity(rows_read.len());
    for (index, read) in rows_read.iter().enumerate() {
        let row_at = index + 2;
        let mut cells = Vec::with_capacity(headers.len());
        let mut shape = aterm_json::Map::new();
        for (col, header) in headers.iter().enumerate() {
            if let Some(secret) = read.secrets.get(header) {
                secrets.insert((row_at, col), Arc::clone(secret));
                cells.push(String::new());
                shape.insert(header.clone(), json!("set"));
            } else {
                let text = read.cells.get(header).cloned().unwrap_or_default();
                if !text.is_empty() {
                    shape.insert(header.clone(), json!(text));
                }
                cells.push(text);
            }
        }
        rows.push(cells);
        canonical.push(Value::Object(shape));
    }
    let canonical = aterm_json::to_vec(&json!({"file": name, "mailboxes": canonical}))
        .map_err(|_| malformed())?;
    Ok(Sheet {
        kind: SheetKind::MailboxToml,
        name: name.to_owned(),
        source: keys::source_pk(&canonical),
        sha256: hex::encode_lower(&Sha256::digest(&canonical)),
        headers,
        rows,
        mode: Mode::FillOnly,
        secrets,
        conflicted: BTreeSet::new(),
        earlier: BTreeSet::new(),
    })
}

/// One field of one mailbox as the planner reads it: its spelling's column and coordinate.
struct Given<'s> {
    col: usize,
    at: Coord,
    text: &'s str,
    secret: Option<Arc<Secret>>,
}

/// The receive patterns a mailbox states, as the harness normalizes them
/// (`mailbox_routes::normalize_pattern`): `*`, `*@<domain>` or an exact address.
enum Pattern {
    Any,
    Domain(String),
    Exact(String),
}

fn pattern(raw: &str) -> Option<Pattern> {
    let trimmed = raw.trim();
    if trimmed == "*" {
        return Some(Pattern::Any);
    }
    if trimmed.chars().any(char::is_whitespace) || trimmed.matches('@').count() != 1 {
        return None;
    }
    let (local, domain) = trimmed.split_once('@')?;
    if domain.is_empty() || domain.contains('*') {
        return None;
    }
    match local {
        "" | "*" => Some(Pattern::Domain(domain.to_ascii_lowercase())),
        _ if local.contains('*') => None,
        _ => keys::address(trimmed).ok().map(Pattern::Exact),
    }
}

impl Planner<'_> {
    fn toml_finding(
        &mut self,
        kind: Kind,
        collection: &'static str,
        pk: Option<&str>,
        column: &str,
        at: Vec<Coord>,
    ) {
        self.findings.insert(Finding {
            kind,
            collection: Some(collection),
            pk: pk.map(str::to_owned),
            column: Some(column.to_owned()),
            at,
        });
    }

    /// The mailbox file's rows, every field carried as the module docs say. Entries first, so an
    /// entry's value wins over the top-level mailbox's for one login, as the harness reads it.
    pub(super) fn mailbox_toml(&mut self, n: usize, sheet: &Sheet) -> Result<()> {
        let kind = sheet.kind;
        let mut order: Vec<usize> = (1..sheet.rows.len()).collect();
        order.push(0);
        // Whether an entry receives `*`: then the top-level mailbox is never routed to.
        let receives = sheet.headers.iter().position(|h| h == "receives");
        let any_everywhere = sheet.rows.iter().skip(1).any(|cells| {
            receives.and_then(|col| cells.get(col)).is_some_and(|text| {
                text.lines()
                    .any(|line| matches!(pattern(line), Some(Pattern::Any)))
            })
        });
        // `receives_domains` is one list per mailbox, gathered over every row naming it.
        let mut domains: BTreeMap<String, (Vec<String>, Coord)> = BTreeMap::new();
        for index in order {
            let row = index + 2;
            let Some(cells) = sheet.rows.get(index) else {
                continue;
            };
            let mut given: BTreeMap<Field, Vec<Given<'_>>> = BTreeMap::new();
            let mut unknown = Vec::new();
            for (col, header) in sheet.headers.iter().enumerate() {
                let text = cells.get(col).map_or("", |c| c.as_str());
                let secret = sheet.secrets.get(&(row, col)).cloned();
                if text.is_empty() && secret.is_none() {
                    continue;
                }
                let at = coord(sheet, n, row, col);
                match field(header) {
                    Some(field) => given.entry(field).or_default().push(Given {
                        col,
                        at,
                        text,
                        secret,
                    }),
                    None => unknown.push((header.clone(), at)),
                }
            }
            if given.is_empty() && unknown.is_empty() {
                continue;
            }
            // The login keys the row: without one, nothing of the mailbox has a home.
            let login = given.get(&Field::Login).and_then(|g| g.first());
            let login = match login.map(|g| (keys::address(g.text), g.at.clone())) {
                Some((Ok(address), at)) => Some((address, at)),
                Some((Err(_), at)) => {
                    self.toml_finding(Kind::NotAnAddress, MAILBOX.name, None, "login", vec![at]);
                    None
                }
                None => {
                    let at = coord(sheet, n, row, 0);
                    self.find(Kind::NoAddress, Some(MAILBOX.name), None, vec![at]);
                    None
                }
            };
            let Some((login, login_at)) = login else {
                for (header, at) in unknown {
                    self.toml_finding(Kind::Unmapped, MAILBOX.name, None, &header, vec![at]);
                }
                continue;
            };
            // A protected login holds no mailbox: each field it gives is refused by name, its login
            // too, which alone says a mailbox signs in as it.
            if let Some(login_pk) = self.address(&login, kind, &login_at)
                && self.guarded(&login_pk)
            {
                for spellings in given.values() {
                    for given in spellings {
                        let column = sheet.headers[given.col].clone();
                        self.toml_finding(
                            Kind::ProtectedSecret,
                            ADDRESS.name,
                            Some(&login_pk),
                            &column,
                            vec![given.at.clone()],
                        );
                    }
                }
                for (header, at) in unknown {
                    self.toml_finding(Kind::Unmapped, MAILBOX.name, None, &header, vec![at]);
                }
                continue;
            }
            // The row the login names, and its mailbox: none where several rows say the login.
            let (Some(mb), Some(login_pk)) = (
                self.mailbox(&login, kind, &login_at)?,
                self.address(&login, kind, &login_at),
            ) else {
                continue;
            };
            for (header, at) in unknown {
                self.toml_finding(Kind::Unmapped, MAILBOX.name, Some(&mb), &header, vec![at]);
            }
            for (field, spellings) in &given {
                let Some(first) = spellings.first() else {
                    continue;
                };
                // A second spelling of one field must say the same.
                for other in &spellings[1..] {
                    let same = match (&first.secret, &other.secret) {
                        (Some(a), Some(b)) => a.same(b),
                        (None, None) => first.text == other.text,
                        _ => false,
                    };
                    if !same {
                        let column = &sheet.headers[other.col];
                        self.toml_finding(
                            Kind::SourcesDisagree,
                            MAILBOX.name,
                            Some(&mb),
                            column,
                            vec![first.at.clone(), other.at.clone()],
                        );
                    }
                }
                let header = sheet.headers[first.col].clone();
                let at = first.at.clone();
                match field {
                    Field::Login => {}
                    Field::Password => {
                        if let Some(secret) = &first.secret {
                            self.want_secret(
                                &ADDRESS,
                                &login_pk,
                                "password",
                                Arc::clone(secret),
                                &at,
                            );
                        }
                    }
                    Field::MailboxPassword | Field::Totp => {
                        let column = if *field == Field::Totp {
                            "totp"
                        } else {
                            "mailbox_password"
                        };
                        if let Some(secret) = &first.secret {
                            self.want_secret(&MAILBOX, &mb, column, Arc::clone(secret), &at);
                        }
                    }
                    Field::Webmail | Field::Imap | Field::Http | Field::Url => {
                        if first.secret.is_some() {
                            self.toml_finding(
                                Kind::CredentialShaped,
                                MAILBOX.name,
                                Some(&mb),
                                &header,
                                vec![at],
                            );
                            continue;
                        }
                        let has_prefix = |p: &str| {
                            first
                                .text
                                .get(..p.len())
                                .is_some_and(|s| s.eq_ignore_ascii_case(p))
                        };
                        let column = match field {
                            Field::Webmail => Some("webmail"),
                            Field::Imap | Field::Url
                                if has_prefix("imap://") || has_prefix("imaps://") =>
                            {
                                Some("imap")
                            }
                            Field::Http if has_prefix("https://") || has_prefix("http://") => {
                                Some("http")
                            }
                            _ => None,
                        };
                        let valid = column.and_then(|column| {
                            let schema = MAILBOX.column(column)?;
                            cell::validate(&MAILBOX, schema, &json!(first.text))
                                .is_ok()
                                .then_some(column)
                        });
                        match valid {
                            Some(column) => {
                                self.want(&MAILBOX, &mb, column, json!(first.text), &at)
                            }
                            None => self.toml_finding(
                                Kind::Unmapped,
                                MAILBOX.name,
                                Some(&mb),
                                &header,
                                vec![at],
                            ),
                        }
                    }
                    Field::Receives => {
                        for line in first.text.lines() {
                            match pattern(line) {
                                Some(Pattern::Exact(address)) => {
                                    if let Some(pk) = self.address(&address, kind, &at) {
                                        self.element(&MAILBOX, &mb, "receives", &pk, &at);
                                    }
                                }
                                Some(Pattern::Domain(domain)) => {
                                    let list = &mut domains
                                        .entry(mb.clone())
                                        .or_insert_with(|| (Vec::new(), at.clone()))
                                        .0;
                                    if !list.contains(&domain) {
                                        list.push(domain);
                                    }
                                }
                                Some(Pattern::Any) => {
                                    let list = &mut domains
                                        .entry(mb.clone())
                                        .or_insert_with(|| (Vec::new(), at.clone()))
                                        .0;
                                    if !list.iter().any(|d| d == "*") {
                                        list.push("*".to_owned());
                                    }
                                }
                                None => self.toml_finding(
                                    Kind::Unmapped,
                                    MAILBOX.name,
                                    Some(&mb),
                                    &header,
                                    vec![at.clone()],
                                ),
                            }
                        }
                    }
                }
            }
            // The top-level mailbox is every address's fallback.
            if index == 0 && !any_everywhere {
                let list = &mut domains
                    .entry(mb.clone())
                    .or_insert_with(|| (Vec::new(), login_at.clone()))
                    .0;
                if !list.iter().any(|d| d == "*") {
                    list.push("*".to_owned());
                }
            }
        }
        for (mb, (list, at)) in domains {
            self.want(&MAILBOX, &mb, "receives_domains", json!(list), &at);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
