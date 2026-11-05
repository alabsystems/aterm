// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ATT/1, "aterm typed tables" (docs/DESIGN-alab-lab-model-2026-11-01.md §3): typed
//! collections read out of Commons `Command` v1 records, with no change to the wire, the
//! envelope, the descriptor or repair. A collection declares one of three kinds:
//!
//! - **cells**, for owner data: one causal register per column, keyed `<pk>.<col>` (or
//!   `<pk>.<col>.<elem>` for one element of a set), valued `{"r": version, "x": value}`, so
//!   edits to different columns never collide and an edit replaces only the candidates it
//!   observed ([`cell`]);
//! - **owned**, for what a Mac states about itself: a whole row at the signer's own machine
//!   uuid, inert at any other key ([`owned`]);
//! - **log**, append-only per writer, read through the eligible operations, never cells.
//!
//! Schemas are compiled constants checked by `const fn` at compile time
//! ([`Collection::check`]); a stored copy is inert data for generic tools. Nothing here does
//! I/O: callers read a table and hand its snapshot or eligible operations in.

use crate::database::Change;
use aterm_digest::Sha256;
use aterm_json::{Value, json};

pub mod cell;
pub mod check;
pub mod grid;
pub mod guard;
pub mod keys;
pub mod owned;
pub mod secret;

/// The format word a stored schema row carries.
pub const FORMAT: &str = "att/1";
/// The reserved collection holding each collection's schema row.
pub const SCHEMA: &str = "schema";
/// The reserved lifecycle column of every cells collection.
pub const LIFECYCLE: &str = "row";
/// Longest primary key: `[A-Za-z0-9_-]`, no dot.
pub const MAX_PK: usize = 26;
/// Longest column name: `[a-z0-9_]`.
pub const MAX_COLUMN: usize = 20;
/// Longest stored key, the Commons name limit (`crypto::name`).
pub const MAX_KEY: usize = 64;
/// Most columns one collection declares.
pub const MAX_COLUMNS: usize = 64;

/// How a collection merges (design §3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Owner data: one causal register per column.
    Cells,
    /// One whole row per key, written only by the Mac the key names.
    Owned,
    /// Append-only per writer.
    Log,
}

/// Who a reader accepts as a row's writer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Writers {
    /// Any current Writer, acting for the owner.
    Owner,
    /// The key is the signer's Commons machine uuid.
    SelfKey,
    /// The key ends with `.<signer machine uuid>`.
    SelfSuffix,
}

/// How a collection's primary key is made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Derive {
    /// A fixed word (a provider, a CLI).
    Word,
    /// A digest of the row's birth identity (`keys`).
    BirthSha256,
    /// The signer's machine uuid.
    SignerUuid,
    /// Random at birth: for a key whose natural identity is a low-entropy secret.
    Random,
    /// Built from other keys (a preference, an auth route).
    Composite,
}

/// What a reader shows for a conflicted cell until someone resolves it (design §3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnConflict {
    /// The candidate with the smallest operation digest, flagged.
    Show,
    /// The first of these words among the candidates.
    Prefer(&'static [&'static str]),
    /// The largest number among the candidates.
    Max,
    /// The smallest number among the candidates.
    Min,
    /// No provisional value.
    Hold,
}

/// Who writes a column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Writer {
    /// The owner, through the grid, a verb or the importer.
    Owner,
    /// A Mac's birth write of a row it found missing.
    Birth,
    /// The row's own Mac.
    Mine,
    /// A compiled seed.
    Seed,
}

/// One member of a closed struct type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Member {
    pub name: &'static str,
    pub ty: Ty,
    pub null: bool,
}

/// A column's value type (design §3.3). Integers only: a percentage is `pct_x10`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    Bool,
    Int {
        min: i64,
        max: i64,
    },
    Uint,
    /// Free text of at most this many bytes, guarded (`guard::text`).
    Text(usize),
    /// An account's owner-editable name of at most this many bytes: free text that names no
    /// machine (`guard::account_label`).
    AccountLabel(usize),
    Enum(&'static [&'static str]),
    /// Unix milliseconds: an author's claim, never an ordering input.
    Time,
    /// A UTC day number.
    Day,
    /// An `m<N>` machine label.
    Label,
    /// Lower-case hex of exactly this many digits.
    Hex(usize),
    /// Exactly this many decimal digits, kept as text (a phone's last four, `0101`).
    Digits(usize),
    /// An address: personal, masked on every surface.
    Email,
    /// An https URL with no query or fragment.
    Url,
    /// `{vault, entry, item?}`: names of a vault entry, never its value.
    SecretRef,
    /// An inner-sealed secret, revealed only through the local audited API.
    Secret,
    /// A primary key of the named collection.
    Ref(&'static str),
    Set(&'static Ty),
    List(&'static Ty, usize),
    Struct(&'static [Member]),
    /// A source coordinate `{source, row, col}`: where a value came from, never the value.
    SrcRef,
    /// Bounded JSON, read-only in the grid.
    Json(usize),
    /// One signed Commons record of an earlier table generation, at most this many bytes, as
    /// padded base64 of its wire: still sealed under its own scope, roster and epoch key, so a
    /// reader authenticates it before it reads anything in it (`registry.2`'s archived owned
    /// rows, design §3.8). Read-only in the grid.
    Sealed(usize),
}

impl Ty {
    /// Whether a value of this type can hold an address, so every surface masks it.
    pub const fn pii(&self) -> bool {
        match self {
            Self::Email => true,
            Self::Set(inner) | Self::List(inner, _) => inner.pii(),
            Self::Struct(members) => {
                let mut i = 0;
                while i < members.len() {
                    if members[i].ty.pii() {
                        return true;
                    }
                    i += 1;
                }
                false
            }
            _ => false,
        }
    }

    /// The word a stored schema row spells this type with.
    fn word(&self) -> Value {
        match self {
            Self::Bool => json!("bool"),
            Self::Int { min, max } => json!({"int": [min, max]}),
            Self::Uint => json!("uint"),
            Self::Text(max) => json!({"text": max}),
            Self::AccountLabel(max) => json!({"account_label": max}),
            Self::Enum(words) => json!({"enum": words}),
            Self::Time => json!("time"),
            Self::Day => json!("day"),
            Self::Label => json!("label"),
            Self::Hex(n) => json!({"hex": n}),
            Self::Digits(n) => json!({"digits": n}),
            Self::Email => json!("email"),
            Self::Url => json!("url"),
            Self::SecretRef => json!("secret_ref"),
            Self::Secret => json!("secret"),
            Self::Ref(collection) => json!({"ref": collection}),
            Self::Set(inner) => json!({"set": inner.word()}),
            Self::List(inner, max) => json!({"list": inner.word(), "max": max}),
            Self::Struct(members) => json!({"struct": members
                .iter()
                .map(|m| json!({"name": m.name, "type": m.ty.word(), "null": m.null}))
                .collect::<Vec<_>>()}),
            Self::SrcRef => json!("srcref"),
            Self::Json(max) => json!({"json": max}),
            Self::Sealed(max) => json!({"sealed": max}),
        }
    }
}

/// One column of a collection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Column {
    pub name: &'static str,
    pub ty: Ty,
    pub null: bool,
    pub on_conflict: OnConflict,
    /// The collection version that added it.
    pub since: u32,
    /// The collection version that retired it; its name is never reused.
    pub retired: Option<u32>,
    pub writer: Writer,
    /// The row's key derives from this column, so it is written at birth and never edited: a
    /// row saying another pair under its key would be two mappings at once.
    pub fixed: bool,
}

impl Column {
    /// A required owner column, added at version 1, shown when conflicted.
    pub const fn new(name: &'static str, ty: Ty) -> Self {
        Self {
            name,
            ty,
            null: false,
            on_conflict: OnConflict::Show,
            since: 1,
            retired: None,
            writer: Writer::Owner,
            fixed: false,
        }
    }
    /// The row's key derives from it: written at birth, never edited.
    pub const fn fixed(mut self) -> Self {
        self.fixed = true;
        self
    }
    /// The column may be empty.
    pub const fn null(mut self) -> Self {
        self.null = true;
        self
    }
    /// The provisional value of a conflicted cell.
    pub const fn on_conflict(mut self, rule: OnConflict) -> Self {
        self.on_conflict = rule;
        self
    }
    /// Added at `version`.
    pub const fn since(mut self, version: u32) -> Self {
        self.since = version;
        self
    }
    /// Retired at `version`.
    pub const fn retired(mut self, version: u32) -> Self {
        self.retired = Some(version);
        self
    }
    /// Who writes it.
    pub const fn writer(mut self, writer: Writer) -> Self {
        self.writer = writer;
        self
    }
    /// Whether the column exists at collection version `version`.
    pub const fn live_at(&self, version: u32) -> bool {
        self.since <= version
            && match self.retired {
                Some(retired) => version < retired,
                None => true,
            }
    }
}

/// One typed collection: compiled, authoritative for code that acts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Collection {
    pub name: &'static str,
    pub version: u32,
    pub kind: Kind,
    pub writers: Writers,
    /// The primary key prefix (`mc`, `ad`, …), empty for word keys.
    pub prefix: &'static str,
    pub derive: Derive,
    pub columns: &'static [Column],
    pub unique: &'static [&'static [&'static str]],
    /// Largest canonical row of an owned or log collection.
    pub max_row: usize,
}

/// The default cap of an owned row (design §3.1).
pub const MAX_OWNED_ROW: usize = 32 * 1024;

/// Whether `name` contains one of the secret column words (the `contacts.rs` detail rule).
const fn secret_named(name: &str) -> bool {
    let words: [&[u8]; 7] = [
        b"password",
        b"token",
        b"cookie",
        b"secret",
        b"totp",
        b"recovery",
        b"private_key",
    ];
    let mut w = 0;
    while w < words.len() {
        if contains(name.as_bytes(), words[w]) {
            return true;
        }
        w += 1;
    }
    str_eq(name, "pin") || str_eq(name, "pin_code")
}

const fn contains(hay: &[u8], needle: &[u8]) -> bool {
    if needle.len() > hay.len() {
        return false;
    }
    let mut start = 0;
    while start + needle.len() <= hay.len() {
        let mut i = 0;
        while i < needle.len() && hay[start + i] == needle[i] {
            i += 1;
        }
        if i == needle.len() {
            return true;
        }
        start += 1;
    }
    false
}

const fn str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// `[a-z0-9_]{1,MAX_COLUMN}`.
pub const fn column_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_COLUMN {
        return false;
    }
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if !(b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') {
            return false;
        }
        i += 1;
    }
    true
}

/// `[A-Za-z0-9_-]{1,MAX_PK}`: no dot, so a cell key splits unambiguously.
pub const fn pk_grammar(pk: &str) -> bool {
    let bytes = pk.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_PK {
        return false;
    }
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if !(b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
            return false;
        }
        i += 1;
    }
    true
}

/// A collection name: the Commons name grammar, never starting with `_`.
const fn collection_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_KEY || bytes[0] == b'_' {
        return false;
    }
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if !(b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.') {
            return false;
        }
        i += 1;
    }
    true
}

impl Collection {
    /// The column named `name`.
    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|column| column.name == name)
    }

    /// Whether this compiled declaration is well formed: the grammars, no reused column
    /// name, secret-named columns only as Secret, a lifecycle column on every
    /// cells collection, and writers that fit the kind. Called by [`att_collection!`] at
    /// compile time, so a bad schema never builds.
    ///
    /// # Errors
    /// The first rule the declaration breaks.
    pub const fn check(&self) -> Result<(), &'static str> {
        if !collection_name(self.name) {
            return Err("collection name breaks the name grammar");
        }
        if str_eq(self.name, SCHEMA) {
            return Err("the schema collection is reserved");
        }
        if self.version == 0 {
            return Err("collection versions start at 1");
        }
        if self.columns.is_empty() || self.columns.len() > MAX_COLUMNS {
            return Err("a collection declares 1 to 64 columns");
        }
        if !self.prefix.is_empty() && !pk_grammar(self.prefix) {
            return Err("key prefix breaks the key grammar");
        }
        match (self.kind, self.writers) {
            (Kind::Cells, Writers::Owner)
            | (Kind::Owned, Writers::SelfKey | Writers::SelfSuffix)
            | (Kind::Log, Writers::SelfSuffix) => {}
            _ => return Err("the writers do not fit the collection kind"),
        }
        let mut lifecycle = false;
        let mut i = 0;
        while i < self.columns.len() {
            let column = &self.columns[i];
            if !column_name(column.name) {
                return Err("column name breaks the column grammar");
            }
            if secret_named(column.name) && !matches!(column.ty, Ty::Secret) {
                return Err("a secret-named column must be Secret");
            }
            if column.since == 0 || column.since > self.version {
                return Err("a column's since lies outside the collection's versions");
            }
            if let Some(retired) = column.retired
                && retired <= column.since
            {
                return Err("a column retires after it is added");
            }
            let mut j = i + 1;
            while j < self.columns.len() {
                if str_eq(column.name, self.columns[j].name) {
                    return Err("a column name appears twice");
                }
                j += 1;
            }
            if str_eq(column.name, LIFECYCLE) {
                let Ty::Enum(words) = column.ty else {
                    return Err("the lifecycle column is an enum");
                };
                if words.is_empty() || !str_eq(words[0], "live") {
                    return Err("the lifecycle column's first word is live");
                }
                lifecycle = true;
            }
            i += 1;
        }
        if matches!(self.kind, Kind::Cells) && !lifecycle {
            return Err("a cells collection has the lifecycle column");
        }
        if !matches!(self.kind, Kind::Cells) && (self.max_row == 0 || self.max_row > MAX_OWNED_ROW)
        {
            return Err("an owned or log row is at most 32 KiB");
        }
        Ok(())
    }

    /// The schema row stored in the reserved `schema` collection: inert data a generic tool
    /// can render and check, never code. Only an owner verb writes it (design §3.5).
    pub fn schema_row(&self) -> Value {
        let kind = match self.kind {
            Kind::Cells => "cells",
            Kind::Owned => "owned",
            Kind::Log => "log",
        };
        let writers = match self.writers {
            Writers::Owner => "owner",
            Writers::SelfKey => "self",
            Writers::SelfSuffix => "self-suffix",
        };
        let derive = match self.derive {
            Derive::Word => "word",
            Derive::BirthSha256 => "birth-sha256",
            Derive::SignerUuid => "signer-uuid",
            Derive::Random => "random",
            Derive::Composite => "composite",
        };
        let columns: Vec<Value> = self
            .columns
            .iter()
            .map(|c| {
                let conflict = match c.on_conflict {
                    OnConflict::Show => json!("show"),
                    OnConflict::Prefer(words) => json!({"prefer": words}),
                    OnConflict::Max => json!("max"),
                    OnConflict::Min => json!("min"),
                    OnConflict::Hold => json!("hold"),
                };
                let writer = match c.writer {
                    Writer::Owner => "owner",
                    Writer::Birth => "birth",
                    Writer::Mine => "self",
                    Writer::Seed => "seed",
                };
                json!({"name": c.name, "type": c.ty.word(), "null": c.null,
                    "on_conflict": conflict, "since": c.since, "retired": c.retired,
                    "pii": c.ty.pii(), "writer": writer})
            })
            .collect();
        json!({"r": self.version, "x": {"format": FORMAT, "version": self.version, "kind": kind,
            "writers": writers, "pk": {"prefix": self.prefix, "derive": derive},
            "columns": columns, "unique": self.unique, "max_row": self.max_row}})
    }
}

/// Declare a compiled collection and check it at compile time: a schema that breaks a rule
/// of [`Collection::check`] fails the build with that rule.
#[macro_export]
macro_rules! att_collection {
    ($(#[$meta:meta])* $vis:vis $name:ident = $collection:expr) => {
        $(#[$meta])*
        $vis const $name: $crate::typed::Collection = $collection;
        const _: () = match $name.check() {
            Ok(()) => (),
            Err(why) => panic!("{}", why),
        };
    };
}

/// One typed table: an application role and the collections it holds.
#[derive(Clone, Copy, Debug)]
pub struct Table {
    pub application: &'static str,
    pub role: &'static str,
    pub collections: &'static [&'static Collection],
    /// Automatic records (owned rows, births, seeds) one writer may retain in one table
    /// generation; readers treat the rest as inert (design §3.7).
    pub self_quota: usize,
    /// Cells collections whose rows end rather than retire when the owner removes one (a
    /// preference ends; an account retires), so a deleted grid row takes that lifecycle word.
    pub ends: &'static [&'static str],
}

impl Table {
    /// The collection named `name`.
    pub fn collection(&self, name: &str) -> Option<&'static Collection> {
        self.collections.iter().copied().find(|c| c.name == name)
    }
}

const REQUEST_DOMAIN: &[u8] = b"aterm/att/1/request\0";

/// A content-derived request id for an import, seed or birth write (design §3.6): a re-run
/// returns the first receipt, and equal writes from many Macs carry one id each.
///
/// # Errors
/// The changes cannot be encoded.
pub fn request_id(application: &str, role: &str, changes: &[Change]) -> crate::Result<[u8; 16]> {
    let canonical: Vec<Value> = changes
        .iter()
        .map(|c| json!({"collection": c.collection, "key": c.key, "value": c.value}))
        .collect();
    let bytes = aterm_json::to_vec(&canonical)
        .map_err(|_| crate::Error::new("typed changes could not be encoded"))?;
    let mut sha = Sha256::new();
    sha.update(REQUEST_DOMAIN);
    for part in [application.as_bytes(), role.as_bytes()] {
        sha.update(part);
        sha.update([0]);
    }
    sha.update(&bytes);
    let digest = sha.finalize();
    let mut request = [0; 16];
    request.copy_from_slice(&digest[..16]);
    Ok(request)
}

/// The `AlabCells` steps no code here takes (docs/DESIGN-alab-lab-model-2026-11-01.md §11 step
/// 12): which law a walk checks, and what a writer has received.
#[cfg(any(test, debug_assertions))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AlabCells",
        action = "ChooseRegister",
        reason = "The model's choice of the law one walk checks: no code chooses it. alab_cells_conforms takes the register area's steps from a fresh history."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AlabCells",
        action = "ChooseOwned",
        reason = "The model's choice of the law one walk checks: no code chooses it. alab_cells_conforms takes the owned-row area's steps from a fresh history."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AlabCells",
        action = "ChooseColumns",
        reason = "The model's choice of the law one walk checks: no code chooses it. alab_cells_conforms takes the column area's steps from a fresh history."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AlabCells",
        action = "ChooseQuota",
        reason = "The model's choice of the law one walk checks: no code chooses it. alab_cells_conforms takes the quota area's steps from a fresh history."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AlabCells",
        action = "ChoosePreference",
        reason = "The model's choice of the law one walk checks: no code chooses it. The harness's alab_roster tests take the preference area's steps."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AlabCells",
        action = "SeeA",
        reason = "Commons repair delivering writer B's record to writer A is the environment: no typed code chooses it. alab_cells_conforms makes A's next write name B's latest record as a parent."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AlabCells",
        action = "SeeB",
        reason = "Commons repair delivering writer A's record to writer B is the environment: no typed code chooses it. alab_cells_conforms makes B's next write name A's latest record as a parent."
    )
)]
#[doc(hidden)]
pub fn alab_cells_environment() {}

#[cfg(test)]
mod model_tests;
#[cfg(test)]
mod tests;
