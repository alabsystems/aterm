// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The lab importer (docs/DESIGN-alab-lab-model-2026-11-01.md §9, step 6;
//! docs/DESIGN-alab-one-mesh-2026-11-05.md §4.7, §8): the owner's sheets, the harness's files,
//! the machines this Mac knows of and fleet_db replicas' dumps, planned as typed registry
//! writes. Pure: the caller hands in the parsed sources, the registry's cells as they are, the
//! table's Secret sealer and the harness's reading of a provider cell; nothing here does I/O.
//!
//! - **Never over the owner, and a sheet only fills.** Each source is read in a [`Mode`]. An
//!   [`Mode::Authoritative`] source (the one replica that holds the corrected rows, imported on
//!   its own Mac first) writes a cell that is absent or still holds what an import wrote
//!   (`cell::import_step`). A [`Mode::FillOnly`] source (every sheet, every other replica's
//!   dump) writes a cell only where it is empty or holds this same source's earlier import; a
//!   cell another source or the owner wrote differently is left as it is and becomes a
//!   [`Kind::Differs`] finding, recorded by row key and column only in the source's row
//!   (`source.differs`), never with a value. A cell the owner set, cleared or resolved since,
//!   or a conflicted one, is left as it is and becomes a finding. A row the owner retired is
//!   left whole. An unchanged cell costs nothing, so a re-run writes nothing.
//! - **Passwords and phone numbers are `Secret` cells.** A dump's password and a full phone
//!   number are sealed at their cell's coordinates through the table's [`Sealer`], compared by
//!   opening the stored token (never by its randomized bytes), so a re-run writes nothing. A
//!   sheet's `Password` column is a has-password flag and never becomes a `Secret`; any `Secret`
//!   value that reads as a flag (`1`, `yes`, under six characters) is refused with a finding. A
//!   protected address holds no password ([`crate::typed::check::protected`]).
//! - **What the sheets stop saying is retracted, three-way too.** A cell an earlier import of
//!   a source kind read now wrote, and that source no longer states, is withdrawn: a set element
//!   (a service, an alias) becomes `false`, a region, flow, mailbox link, alias link, note or rank
//!   empty, and a preference no source states now ends (`row = ended`); an account or address no
//!   longer listed is a finding, never removed. A source not read this run retracts nothing: a
//!   fleet file is its own source, since a kind has several, and a kind with a file that could
//!   not be read is not read. A run with a file or an address cell it could not read ends no
//!   preference at all, since what it missed may state any of them.
//! - **Notes are kept, behind the guard** (design §0). The text of a note cell goes in its row's
//!   `note`, three-way, so an owner's note wins, with its coordinate in `notes_src`. A cell the
//!   note guard refuses (credential-shaped, naming a secret, holding an address) is never copied:
//!   the row keeps its coordinate and a `note-withheld` finding. The lead's reasoning, the `m`
//!   column and losing values stay coordinates, and a credential-shaped cell is never read.
//! - **The harness's reading of the vault sheet** ([`Vendors`]): Kimi Code's bare `1` is held
//!   (no account: its region decides which account a sign-in makes), Codex and Grok Build carry
//!   flow words, and a disqualifying note pauses Claude Code's account whatever vendor it
//!   names, and any other column's account only when it names that vendor or none.
//! - **An address row is matched by its key, and the table wins**: a source's address names the
//!   row its key derives where the table holds that row, whatever that row's `addr` says now
//!   (a renamed row keeps its birth key; its `addr` stays the owner's and is reported kept),
//!   else the one row whose `addr` says it now ([`AddressIndex::matched`]), else a row born
//!   under its key. An address several rows say, with no row under its key, is a
//!   [`Kind::AddressAmbiguous`] finding: nothing of a source row whose own address it is is
//!   written, and an address a row only refers to (a root, a receiving or a mailbox login) links
//!   nothing.
//! - **One key per account**: the canonical account key (`keys::account_pk`) from the claims
//!   realm and the claims email, one provider word per realm. The email is what its login row's
//!   `addr` says in the table where it says one address, else the sheet's: a sheet may name a
//!   renamed row by its birth address, and the table wins.
//! - **A mailbox is its login's** (one-mesh design §4.7): a receiving mailbox becomes a row keyed
//!   from its login address's key ([`keys::mailbox_pk`]), with a plain label, a service and its
//!   `login`, that member-readable address row. A phone's key is random; the importer finds an
//!   existing phone by opening its `number`. No phone row is made from free text.
//! - **Labels** go through the one parser (`machines::Reading`): `M109` is `m109`, `m27-2` is
//!   number 2 on m27, `m3.2` number 2 on m3, a bare `m3` number 1. Every label resolves against
//!   the live rows' labels and aliases, then the Macs this Mac knows of (by label or alias, and
//!   their row by fleet node), before a placeholder is minted, so a re-import after a relabel
//!   finds the original row.
//! - **Preferences are mappings** (design §0): each account of a row gets one preference per
//!   machine its cells, or the harness's fleet and pool files, name. The number is a rank and
//!   nothing else. No machine and no rank is ever read from an address, and no allocator move is
//!   written.
//! - **Aliases.** The owner's correction naming another listed login links this address to it
//!   (`address.alias_of`), as does a `+tag` of a listed login, unless the owner's correction
//!   says anything else. Only a run that reads the logins sheet can know that, so only such a
//!   run writes or withdraws the `+tag` link.

use super::registry::{
    ACCOUNT, ADDRESS, AddressHolder, AddressIndex, DIFFER_COLUMNS, MACHINE, MAILBOX, MAX_DIFFERS,
    PHONE, PREFERENCE, SOURCE,
};
use crate::{
    Error, Result,
    database::Change,
    machines::{Label, Reading},
    typed::{
        Collection,
        cell::{self, CellKey, CellValue, Cells, Field, ImportStep, Projection},
        check, guard, keys,
        secret::{MASK, Sealer, Secret, flag_like},
    },
};
use aterm_codec::hex;
use aterm_digest::Sha256;
use aterm_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

pub mod covered;
mod dump;
mod toml;
pub(crate) mod xlsx;
pub use dump::{DUMP_TABLES, dump_sheets};
pub use toml::{MAX_BYTES as MAILBOX_TOML_MAX, mailbox_sheet};

/// The source layouts the importer reads, highest precedence first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SheetKind {
    /// The vault's account sheet (`ai-accounts.csv`): `m, email, computer, claude, Open AI,
    /// google, meta, kimi, x, notes`. It outranks the logins sheet.
    Vault,
    /// The owner's phone inventory sheet (`alab-phones.csv`).
    Phones,
    /// The owner's computer inventory sheet (`alab-computers.csv`).
    Computers,
    /// The owner's email and alias inventory sheet (`alab-emails.csv`).
    Emails,
    /// The owner's logins sheet (`ai-account-logins.csv`): `computer, AI login email, services,
    /// codes arrive in (my guess), why I guessed, your correction`.
    Logins,
    /// A harness fleet file (`fleet-accounts.toml`, `ai-accounts-<cli>.toml`), as [`Stated`] rows.
    FleetAccounts,
    /// The harness's `ai-pool.toml`, its warm lists as [`Stated`] rows.
    AiPool,
    /// One table of a fleet_db replica's dump (`aterm db dump <file> --show-passwords`), one
    /// source per table: every column carried ([`dump`]).
    FleetDb(Dump),
    /// The harness's `ai-mailbox.toml`: every mailbox it names, each field to its MAILBOX or
    /// login address cell or refused by name ([`toml`]).
    MailboxToml,
}

/// The four tables a fleet_db dump holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Dump {
    /// First, so a phone's own row names its label before an address names its number.
    Phones,
    Emails,
    /// Before the computers, whose `assigned_accounts` name the accounts.
    Accounts,
    Computers,
}

/// How a source writes over what the registry holds (one-mesh design §8, step 2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mode {
    /// The replica that holds the corrected rows, on its own Mac, first: it writes over what an
    /// import wrote, never over the owner.
    Authoritative,
    /// A sheet or another replica: it fills empty cells and its own earlier import's, and every
    /// other difference is a [`Kind::Differs`] finding.
    #[default]
    FillOnly,
}

impl SheetKind {
    /// The `source.kind` word.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Vault => "vault-sheet",
            Self::Phones => "phones-csv",
            Self::Computers => "computers-csv",
            Self::Emails => "emails-csv",
            Self::Logins => "logins-csv",
            Self::FleetAccounts => "fleet-accounts",
            Self::AiPool => "ai-pool",
            Self::FleetDb(_) => "fleet-db",
            Self::MailboxToml => "ai-mailbox",
        }
    }

    /// Whether this kind has several files, each its own source, told apart by name.
    const fn several(self) -> bool {
        matches!(self, Self::FleetAccounts | Self::FleetDb(_))
    }

    /// The kind a sheet's header row says it is, for `--compare-sheet`.
    pub fn detect(headers: &[String]) -> Option<Self> {
        let has = |name: &str| headers.iter().any(|h| h.trim().eq_ignore_ascii_case(name));
        let starts = |name: &str| {
            headers.iter().any(|h| {
                let t = h.trim();
                t.get(..name.len())
                    .is_some_and(|p| p.eq_ignore_ascii_case(name))
            })
        };
        if has("email address") {
            Some(Self::Emails)
        } else if has("ai login email") {
            Some(Self::Logins)
        } else if has("email") && has("computer") {
            Some(Self::Vault)
        } else if (has("id") || has("machine")) && has("make") {
            Some(Self::Computers)
        } else if starts("phone") || has("number") {
            Some(Self::Phones)
        } else {
            None
        }
    }

    /// The `address.origin` word.
    const fn origin(self) -> &'static str {
        match self {
            Self::Vault => "sheet",
            Self::Emails | Self::Phones | Self::Computers => "emails-csv",
            Self::Logins => "logins-csv",
            Self::FleetAccounts | Self::AiPool | Self::MailboxToml => "harness",
            Self::FleetDb(_) => "fleet-db",
        }
    }
}

/// One parsed sheet.
#[derive(Clone, Debug)]
pub struct Sheet {
    pub kind: SheetKind,
    /// The file name only, never a path.
    pub name: String,
    /// The source key: a digest of the file's bytes, so a re-run of the same file collapses.
    pub source: String,
    /// The file's SHA-256, lower-case hex.
    pub sha256: String,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// How this source writes over the registry.
    pub mode: Mode,
    /// The `Secret` values of a dump, by 1-based (row, column): never in `rows`, so no review
    /// or finding can show one.
    pub secrets: BTreeMap<(usize, usize), Arc<Secret>>,
    /// Cells a dump's replica holds concurrent values for, by 1-based (row, column): never
    /// written, each a finding.
    pub conflicted: BTreeSet<(usize, usize)>,
}

/// Whether a sheet header names a credential: its column's cells are flags at most.
fn secret_header(header: &str) -> bool {
    let bytes = header.trim().as_bytes();
    [
        "password",
        "passwd",
        "secret",
        "token",
        "credential",
        "totp",
    ]
    .iter()
    .any(|word| {
        bytes
            .windows(word.len())
            .any(|w| w.eq_ignore_ascii_case(word.as_bytes()))
    })
}

impl Sheet {
    /// Whether the source row `row`, key `pk`, records this sheet: the same kind, and for a kind
    /// with several files the same file. A fleet-accounts file is told apart by its name; a
    /// replica's dump by its bytes, since every replica's dump may share a file name.
    fn wrote(&self, pk: &str, row: &cell::Row) -> bool {
        row.get("kind").and_then(Value::as_str) == Some(self.kind.word())
            && match self.kind {
                SheetKind::FleetDb(_) => pk == self.source,
                kind if kind.several() => {
                    row.get("name").and_then(Value::as_str) == Some(self.name.as_str())
                }
                _ => true,
            }
    }

    /// Parse `bytes` (at most the importer's CSV caps) as a sheet named `name`.
    ///
    /// # Errors
    /// The bytes are not a CSV within the caps, or it has no header.
    pub fn parse(kind: SheetKind, name: &str, bytes: &[u8]) -> Result<Self> {
        let records = crate::import::parse(bytes)?;
        Self::from_records_with_bytes(kind, name, records, bytes)
    }

    /// Parse `bytes` (at most the importer's CSV caps) as a sheet named `name`, detecting its
    /// [`SheetKind`] from the header row in a single CSV pass.
    ///
    /// # Errors
    /// The bytes are not a CSV within the caps, it has no header, or its header row names no
    /// sheet this build reads.
    pub fn parse_detected(name: &str, bytes: &[u8]) -> Result<Self> {
        let mut records = crate::import::parse(bytes)?;
        let Some(headers) = records.first() else {
            return Err(Error::new("the sheet has no header row"));
        };
        let Some(kind) = SheetKind::detect(headers) else {
            for record in &mut records {
                record.iter_mut().for_each(zeroize::Zeroize::zeroize);
            }
            return Err(Error::new(
                "the sheet's header row names no sheet this build reads",
            ));
        };
        Self::from_records_with_bytes(kind, name, records, bytes)
    }

    /// The tab `tab` of the xlsx workbook `bytes`, file `name`, as a sheet read as a CSV is
    /// ([`xlsx::tab`]). Its source is named by its records once each credential column is a
    /// flag, never by the workbook's bytes, which may hold real passwords: no unsalted digest of
    /// one reaches the shared `source` row.
    ///
    /// # Errors
    /// The bytes are not a workbook this build reads, it has no such tab, or the tab has no
    /// header.
    pub fn xlsx(kind: SheetKind, name: &str, tab: &str, bytes: &[u8]) -> Result<Self> {
        let records = xlsx::tab(bytes, tab)?;
        let name = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
        let named = format!("{name}#{}", tab.trim());
        Self::from_records(kind, &named, records, |sheet| {
            aterm_json::to_vec(
                &json!({"tab": tab.trim(), "headers": sheet.headers, "rows": sheet.rows}),
            )
            .unwrap_or_default()
        })
    }

    fn from_records_with_bytes(
        kind: SheetKind,
        name: &str,
        records: Vec<Vec<String>>,
        bytes: &[u8],
    ) -> Result<Self> {
        let mut sheet = Self::build_records(kind, name, records)?;
        sheet.source = keys::source_pk(bytes);
        sheet.sha256 = hex::encode_lower(&Sha256::digest(bytes));
        Ok(sheet)
    }

    /// `records` (header first) as a sheet named `name`, its source named by the bytes `named`
    /// gives for it once its credential columns are flags.
    fn from_records(
        kind: SheetKind,
        name: &str,
        records: Vec<Vec<String>>,
        named: impl FnOnce(&Self) -> Vec<u8>,
    ) -> Result<Self> {
        let mut sheet = Self::build_records(kind, name, records)?;
        let bytes = zeroize::Zeroizing::new(named(&sheet));
        sheet.source = keys::source_pk(&bytes);
        sheet.sha256 = hex::encode_lower(&Sha256::digest(&bytes));
        Ok(sheet)
    }

    fn build_records(kind: SheetKind, name: &str, mut records: Vec<Vec<String>>) -> Result<Self> {
        if records.is_empty() {
            return Err(Error::new("the sheet has no header row"));
        }
        let headers = records.remove(0);
        let name = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
        if name.is_empty() || guard::text(name, 128).is_err() {
            for record in &mut records {
                record.iter_mut().for_each(zeroize::Zeroize::zeroize);
            }
            return Err(Error::new("the sheet's file name cannot be recorded"));
        }
        // A credential column of a sheet is a has-password flag (the owner's `alab-emails.csv`
        // holds `1` there): only whether it is set is kept, so no review or finding shows a cell.
        for (col, _) in headers.iter().enumerate().filter(|(_, h)| secret_header(h)) {
            for row in &mut records {
                if let Some(cell) = row.get_mut(col) {
                    let set = !cell.trim().is_empty();
                    zeroize::Zeroize::zeroize(cell);
                    *cell = if set { "1".into() } else { String::new() };
                }
            }
        }
        Ok(Self {
            kind,
            name: name.to_owned(),
            source: String::new(),
            sha256: String::new(),
            headers,
            rows: records,
            mode: Mode::FillOnly,
            secrets: BTreeMap::new(),
            conflicted: BTreeSet::new(),
        })
    }

    /// A harness file as a sheet: one row per [`Stated`] row, so its coordinates, source row and
    /// review work as a sheet's do. The file is read from `~/.secrets`, so its source key and
    /// `sha256` are its keyed digest under `everyone_key` ([`keyed_source`]), never a plain hash
    /// of a secrets file, tokens or not.
    ///
    /// # Errors
    /// The file name cannot be recorded.
    pub fn stated(
        kind: SheetKind,
        name: &str,
        bytes: &[u8],
        rows: &[Stated],
        everyone_key: &[u8; 32],
    ) -> Result<Self> {
        let name = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
        if name.is_empty() || guard::text(name, 128).is_err() {
            return Err(Error::new("a source's file name cannot be recorded"));
        }
        let (source, sha256) = keyed_source(HARNESS_SOURCE_DIGEST, bytes, everyone_key);
        Ok(Self {
            kind,
            name: name.to_owned(),
            source,
            sha256,
            headers: ["realm", "email", "label", "machines"]
                .map(str::to_owned)
                .to_vec(),
            rows: rows
                .iter()
                .map(|row| {
                    vec![
                        row.realm.clone(),
                        row.email.clone(),
                        row.label.clone().unwrap_or_default(),
                        row.machines.join(" "),
                    ]
                })
                .collect(),
            mode: Mode::FillOnly,
            secrets: BTreeMap::new(),
            conflicted: BTreeSet::new(),
        })
    }

    /// The text at a 1-based coordinate (row 1 is the header), for the local review only.
    pub fn text(&self, at: &Coord) -> Option<&str> {
        let row = match at.row {
            0 => return None,
            1 => &self.headers,
            r => self.rows.get(r - 2)?,
        };
        row.get(at.col.checked_sub(1)?).map(String::as_str)
    }

    /// The address of each row whose password column is set (a flag, never a value), in the
    /// claims spelling, read from the sheet's own address column (an `email`, `login` or `address`
    /// header that names no other address: not a root, recovery, receiving or forwarding one),
    /// with how many rows are set and how many of them name no address: what `--compare-sheet`
    /// holds against the registry, matching each to its row as the import does
    /// ([`AddressIndex::matched`]). The addresses stay in this process and are never printed.
    pub fn password_flags(&self) -> (BTreeSet<String>, usize, usize) {
        let flags: Vec<usize> = (0..self.headers.len())
            .filter(|col| secret_header(&self.headers[*col]))
            .collect();
        let address = self.column(|h| {
            (h.contains("email") || h.contains("login") || h == "address")
                && !["root", "recovery", "receiving", "forward", "alias"]
                    .iter()
                    .any(|other| h.contains(other))
        });
        let mut keyed = BTreeSet::new();
        let (mut set, mut unkeyed) = (0, 0);
        for row in &self.rows {
            if !flags
                .iter()
                .any(|col| row.get(*col).is_some_and(|cell| !cell.trim().is_empty()))
            {
                continue;
            }
            set += 1;
            match address
                .and_then(|col| row.get(col))
                .and_then(|cell| keys::address(cell).ok())
            {
                Some(address) => {
                    keyed.insert(address);
                }
                None => unkeyed += 1,
            }
        }
        (keyed, set, unkeyed)
    }

    fn column(&self, wanted: impl Fn(&str) -> bool) -> Option<usize> {
        self.headers
            .iter()
            .position(|header| wanted(&header.trim().to_ascii_lowercase()))
    }
}

/// A Kimi region or a sign-in flow a provider cell names, in the registry's words.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Terms {
    /// `global` or `mainland-cn`.
    pub region: Option<&'static str>,
    /// `browser`, `device` or `window`.
    pub flow: Option<&'static str>,
}

/// What one provider cell says beside its row's notes, as the harness reads the sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// Empty, `0`, `no` or `false`.
    Absent,
    /// A member.
    Member(Terms),
    /// A member whose notes disqualify this vendor: imported paused.
    Paused(Terms),
    /// A member the pool holds out (Kimi Code with no region).
    Held,
    /// A value the column does not take.
    Unread,
}

/// The harness's reading of the vault sheet's provider cells. `crates/aterm`, which links the
/// harness, supplies `accounts_sync::read_sheet_cell`, so the importer and every Mac's pool read
/// one sheet one way.
pub trait Vendors {
    /// Read `cell` in `provider`'s column (a provider word, `keys::PROVIDERS`) beside `notes`,
    /// the row's cells from its `notes` column on, joined by spaces.
    fn mark(&self, provider: &str, cell: &str, notes: &str) -> Mark;

    /// The rows of the harness file `name` of layout `kind`. The refusal is said by the caller
    /// without its text, which may quote a row. A build that links no harness reads none.
    ///
    /// # Errors
    /// The file is not one the harness reads.
    fn stated(
        &self,
        _kind: SheetKind,
        _name: &str,
        _text: &str,
    ) -> std::result::Result<Vec<Stated>, String> {
        Err("this build does not read the harness's files".to_owned())
    }

    /// What this Mac reads about itself, for `check` to say what its self-registration decides.
    /// A build that links no host reader has none.
    fn this_mac(&self) -> Option<super::register::ThisMac> {
        None
    }

    /// The generated exports' file names (design §9, `super::export`), in render order. A build
    /// that links no harness renders none.
    fn export_names(&self) -> Vec<String> {
        Vec::new()
    }

    /// Each generated export of the registry `view` and the journal's `places`, by file name,
    /// without its seal line, given `current` (each file's text as it is now, by name), rendered
    /// the harness's way (`aterm_agent::harness::alab_exports`).
    ///
    /// # Errors
    /// A current file does not parse, or this build links no harness.
    fn exports(
        &self,
        _view: &super::view::LabView,
        _places: &super::journal::place::Places,
        _current: &BTreeMap<String, String>,
    ) -> std::result::Result<Vec<(String, String)>, String> {
        Err("this build renders no exports".to_owned())
    }
}

/// One account as a harness file states it: its realm, its login, what the harness calls it and
/// the machines it is assigned to, in the file's order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stated {
    pub realm: String,
    pub email: String,
    pub label: Option<String>,
    pub machines: Vec<String>,
}

/// A Mac this Mac knows of outside the sheets (the vault's swarm roster): precedence 4.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KnownMac {
    pub label: Option<Label>,
    pub aliases: Vec<Label>,
    /// Its fleet node id, 32 lower-case hex digits.
    pub node: Option<String>,
    pub hosts: Vec<String>,
}

/// The `sheet` of a coordinate in the swarm roster: its row is the known Mac's 1-based place
/// in [`Input::macs`], its column 0.
pub const ROSTER: usize = usize::MAX;

/// A source coordinate: which sheet, 1-based row (row 1 is the header) and column.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Coord {
    /// Index of the sheet in the input, or [`ROSTER`].
    pub sheet: usize,
    /// The sheet's source key.
    pub source: String,
    pub row: usize,
    pub col: usize,
}

impl Coord {
    fn value(&self) -> Value {
        json!({"source": self.source, "row": self.row, "col": self.col})
    }
}

/// What a finding reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// The owner set this cell since an import: left as it is.
    OwnerKept,
    /// The cell holds different concurrent values: the owner resolves it first.
    Conflicted,
    /// The owner retired or ended this row: the import leaves it whole.
    RetiredKept,
    /// Two sources disagree; the higher one won and the other's coordinates are kept.
    SourcesDisagree,
    /// One address on two rows of a sheet.
    DuplicateAddress,
    /// A cell the importer does not read: its coordinates only.
    FreeText,
    /// Kimi Code with no region: no account until the cell names `global` or `mainland-cn`.
    Held,
    /// The row's notes disqualify this vendor: the account is imported paused.
    Paused,
    /// The address cell is not an address.
    NotAnAddress,
    /// A row with cells but no address.
    NoAddress,
    /// A label names more than one live machine.
    LabelAmbiguous,
    /// A label spelled other than its stored form (`M109`, `m27-2`): read as the stored form.
    LabelRespelled,
    /// A label on a row with no account to prefer: the machine is named, nothing maps to it.
    NoAccount,
    /// A note cell the note guard refuses: its coordinate only, its text never copied.
    NoteWithheld,
    /// A cell shaped like a credential: never read; its coordinates only.
    CredentialShaped,
    /// More coordinates than a row keeps: the rest are in this review only.
    CoordinatesTruncated,
    /// A known Mac matched an existing row by its fleet node.
    AdoptedByNode,
    /// More than one live row names a known Mac's fleet node: nothing of that Mac is written
    /// until the owner keeps one.
    NodeAmbiguous,
    /// More than one address row says a source's address now, and no row holds the key it
    /// derives: until the owner keeps one, nothing of a source row whose own address it is is
    /// written, and a source row that only refers to it (a root, a receiving or a mailbox
    /// login) links nothing there.
    AddressAmbiguous,
    /// A known Mac matched no row (`adopt` births none; step 7 does).
    Unadopted,
    /// An earlier import of a sheet read now made this row, and that sheet no longer lists it:
    /// left as it is for the owner to retire.
    NoLongerListed,
    /// A fill-only source states this cell differently from what another source or the owner
    /// wrote: left as it is, recorded by key and column in the source's row.
    Differs,
    /// A `Secret` value that reads as a flag (`1`, `yes`, under six characters): never written.
    SecretFlagLike,
    /// A dump made without `--show-passwords` (`<redacted>`): never written.
    SecretRedacted,
    /// A protected address holds no password: the value is never written.
    ProtectedSecret,
    /// A dump column with no home in the registry: its name only.
    Unmapped,
    /// The dump's replica holds concurrent values for this cell or row: never written.
    DumpConflicted,
    /// More than one phone row holds this number: the smallest key is used.
    DuplicateNumber,
}

impl Kind {
    /// The finding's word.
    pub const fn word(self) -> &'static str {
        match self {
            Self::OwnerKept => "owner-kept",
            Self::Conflicted => "conflicted",
            Self::RetiredKept => "retired-kept",
            Self::SourcesDisagree => "sources-disagree",
            Self::DuplicateAddress => "duplicate-address",
            Self::FreeText => "free-text",
            Self::Held => "held",
            Self::Paused => "paused",
            Self::NotAnAddress => "not-an-address",
            Self::NoAddress => "no-address",
            Self::LabelAmbiguous => "label-ambiguous",
            Self::LabelRespelled => "label-respelled",
            Self::NoAccount => "no-account",
            Self::NoteWithheld => "note-withheld",
            Self::CredentialShaped => "credential-shaped",
            Self::CoordinatesTruncated => "coordinates-truncated",
            Self::AdoptedByNode => "adopted-by-node",
            Self::NodeAmbiguous => "node-ambiguous",
            Self::AddressAmbiguous => "address-ambiguous",
            Self::Unadopted => "unadopted",
            Self::NoLongerListed => "no-longer-listed",
            Self::Differs => "differs",
            Self::SecretFlagLike => "secret-flag-like",
            Self::SecretRedacted => "secret-redacted",
            Self::ProtectedSecret => "protected-secret",
            Self::Unmapped => "unmapped",
            Self::DumpConflicted => "dump-conflicted",
            Self::DuplicateNumber => "duplicate-number",
        }
    }
}

/// One finding: keys, columns and coordinates, never a value.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Finding {
    pub kind: Kind,
    pub collection: Option<&'static str>,
    pub pk: Option<String>,
    pub column: Option<String>,
    pub at: Vec<Coord>,
}

/// One collection's share of a plan.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Count {
    /// Rows the sources name.
    pub rows: usize,
    /// Of those, rows the registry does not hold yet.
    pub new_rows: usize,
    /// Registers to write.
    pub cells: usize,
    /// Registers that already hold the import's value.
    pub unchanged: usize,
    /// Registers left as the owner has them (findings).
    pub kept: usize,
}

/// What one planned register does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Write,
    Unchanged,
    Kept,
}

/// One planned register, for the local review.
#[derive(Clone, Debug, PartialEq)]
pub struct Planned {
    pub collection: &'static str,
    pub pk: String,
    /// `col` or `col.elem`.
    pub column: String,
    pub x: Value,
    pub step: Step,
}

/// What the import would write.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    /// One row's changes per entry, source rows first; pack them into records.
    pub rows: Vec<Vec<Change>>,
    pub findings: Vec<Finding>,
    pub counts: BTreeMap<&'static str, Count>,
    pub planned: Vec<Planned>,
}

/// Everything one import reads.
pub struct Input<'a> {
    pub sheets: &'a [Sheet],
    pub macs: &'a [KnownMac],
    pub vendors: &'a dyn Vendors,
    /// The registry's Secret cells: sealing an imported value and opening a held one.
    pub sealer: &'a dyn Sealer,
    /// The importing Mac's clock, for the source rows' `at` (a claim, never an ordering input).
    pub now_ms: u64,
    /// Whether a label that resolves to no row births one; `adopt` births none.
    pub births: bool,
    /// The kinds with a file this run found and could not read.
    pub unread: &'a [SheetKind],
}

/// The realm, provider word and service a sheet provider column names; Kimi's realm needs a
/// region.
fn provider_realm(provider: &str, region: Option<&str>) -> Option<(&'static str, &'static str)> {
    let realm = match provider {
        keys::ANTHROPIC => "claude",
        keys::OPENAI => "codex",
        keys::GOOGLE => "google",
        keys::META => "meta",
        keys::XAI => "grok",
        keys::KIMI => match region? {
            "global" => "kimi:global",
            "mainland-cn" => "kimi:mainland-cn",
            _ => return None,
        },
        _ => return None,
    };
    Some((realm, keys::provider_of_realm(realm)?))
}

/// The service word a vault sheet provider column names.
fn column_service(provider: &str) -> &'static str {
    match provider {
        keys::ANTHROPIC => "claude-code",
        keys::OPENAI => "codex",
        keys::GOOGLE => "gemini-web",
        keys::META => "meta-web",
        keys::KIMI => "kimi-code",
        _ => "grok",
    }
}

/// The provider word and service a logins sheet `services` word names.
fn service_word(word: &str) -> Option<(&'static str, &'static str)> {
    const TABLE: &[(&str, &str, &str)] = &[
        ("claude", keys::ANTHROPIC, "claude-code"),
        ("claude code", keys::ANTHROPIC, "claude-code"),
        ("claude-code", keys::ANTHROPIC, "claude-code"),
        ("chatgpt", keys::OPENAI, "chatgpt"),
        ("chat gpt", keys::OPENAI, "chatgpt"),
        ("codex", keys::OPENAI, "codex"),
        ("openai", keys::OPENAI, "codex"),
        ("open ai", keys::OPENAI, "codex"),
        ("google", keys::GOOGLE, "gemini-web"),
        ("gemini", keys::GOOGLE, "gemini-web"),
        ("agy", keys::GOOGLE, "agy"),
        ("antigravity", keys::GOOGLE, "agy"),
        ("meta", keys::META, "meta-web"),
        ("meta ai", keys::META, "meta-web"),
        ("muse", keys::META, "muse-code"),
        ("kimi", keys::KIMI, "kimi-code"),
        ("kimi code", keys::KIMI, "kimi-code"),
        ("grok", keys::XAI, "grok"),
        ("x", keys::XAI, "grok"),
        ("xai", keys::XAI, "grok"),
    ];
    TABLE
        .iter()
        .find_map(|&(name, prov, svc)| word.eq_ignore_ascii_case(name).then_some((prov, svc)))
}

/// A mailbox's service and code path from its address's domain.
fn mailbox_service(domain: &str) -> (&'static str, &'static str) {
    match domain {
        "proton.me" | "protonmail.com" | "protonmail.ch" | "pm.me" => ("proton", "proton-api"),
        "gmail.com" | "googlemail.com" => ("gmail", "imap"),
        "outlook.com" | "hotmail.com" | "live.com" => ("outlook", "imap"),
        _ => ("other", "manual"),
    }
}

/// The most source coordinates a row keeps (`notes_src`).
const MAX_NOTES: usize = 8;

/// The set columns an import writes element by element, retracted when no sheet states them.
const RETRACT_SETS: [(&str, &str); 2] = [("account", "services"), ("machine", "aliases")];

/// The value columns an import writes only when a source states them, emptied when none does.
const RETRACT_VALUES: [(&str, &str); 9] = [
    ("account", "region"),
    ("account", "flow"),
    ("account", "note"),
    ("address", "mailbox_hint"),
    ("address", "mailbox"),
    ("address", "alias_of"),
    ("address", "kind"),
    ("address", "note"),
    ("preference", "rank"),
];

/// The room a row's `note` column gives (`registry::note`).
const NOTE_BYTES: usize = 512;

/// What separates a row's note cells in its `note`.
const NOTE_JOIN: &str = "; ";

/// The sources this run read whole.
struct ReadNow<'a> {
    kinds: BTreeSet<&'a str>,
    /// The files read of a kind with several: reading one says nothing of the others.
    files: Vec<&'a Sheet>,
}

impl ReadNow<'_> {
    /// Whether the source row `row`, key `pk`, is one this run read.
    fn covers(&self, pk: &str, row: &cell::Row) -> bool {
        let Some(kind) = row.get("kind").and_then(Value::as_str) else {
            return false;
        };
        self.kinds.contains(kind)
            && (!several_word(kind) || self.files.iter().any(|sheet| sheet.wrote(pk, row)))
    }
}

/// Whether a `source.kind` word names a kind with several files, told apart by name.
fn several_word(kind: &str) -> bool {
    kind == SheetKind::FleetAccounts.word() || kind == SheetKind::FleetDb(Dump::Emails).word()
}

/// What one source does with one cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decided {
    Write,
    Unchanged,
    /// The owner set it: left, a finding.
    Owner,
    Conflicted,
    /// Another source's import wrote another value and this one only fills: a finding, by key.
    Differs,
}

/// Decide one cell: `same` says whether a held value is what the source states. An absent cell
/// is written, as is one an import wrote empty; one an import wrote is written by an
/// authoritative source, or by the source that wrote it (`own`: the same kind, and the same
/// file for a kind with several), and otherwise differs; one the owner set or a conflicted one
/// is left.
fn decide(field: Option<&Field>, same: impl Fn(&Value) -> bool, mode: Mode, own: bool) -> Decided {
    match field {
        None => Decided::Write,
        Some(Field::Value(candidate)) if same(&candidate.value.x) => Decided::Unchanged,
        Some(Field::Value(candidate)) if candidate.value.s.is_some() => {
            if mode == Mode::Authoritative || own || candidate.value.x.is_null() {
                Decided::Write
            } else {
                Decided::Differs
            }
        }
        Some(Field::Value(_) | Field::Set(_)) => Decided::Owner,
        Some(Field::Conflict { .. }) => Decided::Conflicted,
    }
}

/// The source row of the one value `field` holds, with its key, if an import wrote it.
fn source_row<'p>(field: &Field, sources: &'p Projection) -> Option<(&'p str, &'p cell::Row)> {
    let Field::Value(candidate) = field else {
        return None;
    };
    sources
        .rows
        .get_key_value(candidate.value.s.as_deref()?)
        .map(|(pk, row)| (pk.as_str(), row))
}

/// Whether `field` holds what an earlier import of a source this run read wrote. Only such a cell
/// is ever retracted.
fn retractable(field: &Field, sources: &Projection, read: &ReadNow<'_>) -> bool {
    source_row(field, sources).is_some_and(|(pk, row)| read.covers(pk, row))
}

/// One wanted register.
#[derive(Clone, Debug)]
struct Want {
    x: Value,
    at: Coord,
}

/// One wanted `Secret` cell: the plaintext stays in its zeroizing type.
#[derive(Clone, Debug)]
struct SecretWant {
    value: Arc<Secret>,
    at: Coord,
}

/// One wanted row.
#[derive(Clone, Debug, Default)]
struct RowWant {
    values: BTreeMap<&'static str, Want>,
    secrets: BTreeMap<&'static str, SecretWant>,
    elements: BTreeMap<&'static str, BTreeMap<String, Coord>>,
    /// The coordinate that created the row: its `src`, and its writes' source.
    src: Option<Coord>,
    notes: Vec<Coord>,
}

struct Planner<'a> {
    input: &'a Input<'a>,
    current: BTreeMap<&'static str, Projection>,
    /// `current`'s address rows by what each row's `addr` says now.
    address_index: AddressIndex,
    rows: BTreeMap<&'static str, BTreeMap<String, RowWant>>,
    findings: BTreeSet<Finding>,
    /// Phone keys by their number's digits, found or minted this run, so every source naming
    /// one number names one row.
    phones: BTreeMap<String, String>,
    /// Address keys a source read now marks protected.
    protected: BTreeSet<String>,
    /// Addresses whose accounts and preferences the vault sheet decided, with what it decided.
    decided: BTreeMap<String, (BTreeSet<&'static str>, Coord)>,
    /// The machines and ranks the sheet read so far states for each address.
    placed: BTreeMap<String, BTreeSet<(String, Option<u32>)>>,
    /// Accounts with a label cell this run could not resolve (free text, an ambiguous label, a
    /// credential-shaped cell): nothing is known against their preferences, so none is ended.
    unsettled: BTreeSet<String>,
    /// Addresses with a provider or `services` cell this run could not read: which of their
    /// accounts it means is unknown, so none of theirs is ended.
    unsettled_logins: BTreeSet<String>,
    /// Whether a row's address cell could not be read: that row may state any preference.
    address_unread: bool,
    /// Addresses whose correction cell said anything: no `+tag` guess stands beside it.
    corrected: BTreeSet<String>,
    /// Every address read this run, with its row's key and coordinate.
    listed: BTreeMap<String, (String, Coord)>,
    minted_labels: BTreeSet<String>,
    /// The swarm roster as a source: its key and SHA-256, named by its canonical bytes.
    roster: (String, String),
    /// Every address either sheet lists as an AI login.
    logins: BTreeSet<String>,
}

/// The digits of a phone number as the registry keeps them: ASCII digits only, a US number's
/// leading country `1` dropped, as fleet_db normalized it. `None` below four digits.
fn phone_digits(raw: &str) -> Option<String> {
    let mut digits: String = raw.chars().filter(char::is_ascii_digit).collect();
    if digits.len() == 11 && digits.starts_with('1') {
        digits.remove(0);
    }
    (digits.len() >= 4).then_some(digits)
}

/// Every address the sheets list as an AI login (their email column), credential-shaped cells
/// unread.
fn listed_logins(sheets: &[Sheet]) -> BTreeSet<String> {
    let mut logins = BTreeSet::new();
    for sheet in sheets {
        let Some(email) = sheet.column(|h| h == "email" || h == "ai login email") else {
            continue;
        };
        for cells in &sheet.rows {
            let Some(text) = cells.get(email).map(|c| c.trim()) else {
                continue;
            };
            if !guard::credential_shaped(text)
                && let Ok(address) = keys::address(text)
            {
                logins.insert(address);
            }
        }
    }
    logins
}

/// The label a harness file's keyed source digest derives its key under ([`Sheet::stated`]).
const HARNESS_SOURCE_DIGEST: &[u8] = b"aterm/alab/harness-source-digest/1\0";

/// A source read from secrets (a fleet_db dump, a `~/.secrets` harness file) as its source key
/// and `sha256`: HMAC-SHA256 of `bytes` under a key HKDF derives from `everyone_key` (the newest
/// `[everyone]` epoch's) for `label` alone, as [`covered::replica_digest`] derives its own. Every
/// member holds that key, so every Mac names one source alike and a changed byte (one password)
/// is a new source, while a reader of the shared `source` row without the key cannot test a
/// guess against it, as it could a plain hash.
pub(crate) fn keyed_source(
    label: &[u8],
    bytes: &[u8],
    everyone_key: &[u8; 32],
) -> (String, String) {
    use ring::hmac;
    let prk = zeroize::Zeroizing::new(aterm_digest::hkdf_extract(label, everyone_key));
    let derived = zeroize::Zeroizing::new(aterm_digest::hkdf_expand_32(&prk, label));
    let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, &*derived), bytes);
    let tag = tag.as_ref();
    (
        format!("sr-{}", hex::encode_lower(&tag[..8])),
        hex::encode_lower(tag),
    )
}

/// The swarm roster's source key and SHA-256: a digest of its canonical form (labels, aliases,
/// nodes and hosts in roster order), so the same roster collapses to one source row.
fn roster_source(macs: &[KnownMac]) -> (String, String) {
    let list: Vec<Value> = macs
        .iter()
        .map(|mac| {
            json!({
                "label": mac.label.as_ref().map(Label::canonical),
                "aliases": mac.aliases.iter().map(Label::canonical).collect::<Vec<_>>(),
                "node": mac.node,
                "hosts": mac.hosts,
            })
        })
        .collect();
    let bytes = aterm_json::to_vec(&Value::Array(list)).unwrap_or_default();
    (
        keys::source_pk(&bytes),
        hex::encode_lower(&Sha256::digest(&bytes)),
    )
}

/// Plan an import of `input` over the registry's `cells`.
///
/// # Errors
/// A value the importer built breaks its column's type, which is a bug, not a sheet problem
/// (sheet problems are findings).
pub fn plan(input: &Input<'_>, cells: &Cells) -> Result<Plan> {
    let current: BTreeMap<&'static str, Projection> = [
        &ADDRESS,
        &ACCOUNT,
        &MACHINE,
        &PREFERENCE,
        &MAILBOX,
        &PHONE,
        &SOURCE,
    ]
    .into_iter()
    .map(|collection| (collection.name, cell::project(collection, cells)))
    .collect();
    let address_index = AddressIndex::of(&current[ADDRESS.name]);
    let mut planner = Planner {
        input,
        current,
        address_index,
        rows: BTreeMap::new(),
        findings: BTreeSet::new(),
        phones: BTreeMap::new(),
        protected: BTreeSet::new(),
        decided: BTreeMap::new(),
        placed: BTreeMap::new(),
        unsettled: BTreeSet::new(),
        unsettled_logins: BTreeSet::new(),
        address_unread: false,
        corrected: BTreeSet::new(),
        listed: BTreeMap::new(),
        minted_labels: BTreeSet::new(),
        roster: roster_source(input.macs),
        logins: listed_logins(input.sheets),
    };
    let mut order: Vec<usize> = (0..input.sheets.len()).collect();
    order.sort_by_key(|&n| (input.sheets[n].mode, input.sheets[n].kind));
    for n in order {
        planner.sheet(n)?;
    }
    planner.aliases();
    planner.known_macs();
    planner.finish()
}

fn coord(sheet: &Sheet, n: usize, row: usize, col: usize) -> Coord {
    Coord {
        sheet: n,
        source: sheet.source.clone(),
        row,
        col: col + 1,
    }
}

impl Planner<'_> {
    fn find(
        &mut self,
        kind: Kind,
        collection: Option<&'static str>,
        pk: Option<&str>,
        at: Vec<Coord>,
    ) {
        self.findings.insert(Finding {
            kind,
            collection,
            pk: pk.map(str::to_owned),
            column: None,
            at,
        });
    }

    fn row_get(&self, collection: &'static str, pk: &str) -> Option<&RowWant> {
        self.rows.get(collection)?.get(pk)
    }

    fn has_row(&self, collection: &'static str, pk: &str) -> bool {
        self.row_get(collection, pk).is_some()
    }

    fn row(&mut self, collection: &'static Collection, pk: &str, at: &Coord) -> &mut RowWant {
        let table = self.rows.entry(collection.name).or_default();
        if !table.contains_key(pk) {
            table.insert(pk.to_owned(), RowWant::default());
        }
        let row = table.get_mut(pk).expect("row exists");
        if row.src.is_none() {
            row.src = Some(at.clone());
        }
        row
    }

    /// Want `x` in `collection.pk.col` from `at`, unless a higher source already set it; a
    /// different value is a disagreement, kept as the loser's coordinates.
    fn want(
        &mut self,
        collection: &'static Collection,
        pk: &str,
        col: &'static str,
        x: Value,
        at: &Coord,
    ) {
        let mut disagree = None;
        let row = self.row(collection, pk, at);
        match row.values.get(col) {
            None => {
                row.values.insert(col, Want { x, at: at.clone() });
            }
            Some(kept) if kept.x == x => {}
            Some(kept) => {
                disagree = Some(kept.at.clone());
                row.notes.push(at.clone());
            }
        }
        if let Some(kept) = disagree {
            self.findings.insert(Finding {
                kind: Kind::SourcesDisagree,
                collection: Some(collection.name),
                pk: Some(pk.to_owned()),
                column: Some(col.to_owned()),
                at: vec![kept, at.clone()],
            });
        }
    }

    fn element(
        &mut self,
        collection: &'static Collection,
        pk: &str,
        col: &'static str,
        elem: &str,
        at: &Coord,
    ) {
        let elements = self
            .row(collection, pk, at)
            .elements
            .entry(col)
            .or_default();
        if !elements.contains_key(elem) {
            elements.insert(elem.to_owned(), at.clone());
        }
    }

    fn note(&mut self, collection: &'static Collection, pk: &str, at: &Coord) {
        let row = self.row(collection, pk, at);
        if !row.notes.contains(at) {
            row.notes.push(at.clone());
        }
    }

    fn sheet(&mut self, n: usize) -> Result<()> {
        let sheet = &self.input.sheets[n];
        match sheet.kind {
            SheetKind::Vault => self.vault(n, sheet),
            SheetKind::Phones => self.phones(n, sheet)?,
            SheetKind::Computers => self.computers(n, sheet),
            SheetKind::Emails => self.emails(n, sheet)?,
            SheetKind::Logins => self.logins(n, sheet)?,
            SheetKind::FleetAccounts | SheetKind::AiPool => self.harness(n, sheet),
            SheetKind::FleetDb(table) => self.dump(n, sheet, table)?,
            SheetKind::MailboxToml => self.mailbox_toml(n, sheet)?,
        }
        Ok(())
    }

    /// Want `value` in the `Secret` cell `collection.pk.col` from `at`: refused when it reads as
    /// a flag or a redaction, and kept to the first source that states it, a different value
    /// from a lower one being a disagreement by coordinates only.
    fn want_secret(
        &mut self,
        collection: &'static Collection,
        pk: &str,
        col: &'static str,
        value: Arc<Secret>,
        at: &Coord,
    ) {
        let refused = if value.plaintext() == dump::REDACTED {
            Some(Kind::SecretRedacted)
        } else if flag_like(&value) {
            Some(Kind::SecretFlagLike)
        } else {
            None
        };
        if let Some(kind) = refused {
            self.findings.insert(Finding {
                kind,
                collection: Some(collection.name),
                pk: Some(pk.to_owned()),
                column: Some(col.to_owned()),
                at: vec![at.clone()],
            });
            return;
        }
        let mut disagree = None;
        let row = self.row(collection, pk, at);
        match row.secrets.get(col) {
            None => {
                row.secrets.insert(
                    col,
                    SecretWant {
                        value,
                        at: at.clone(),
                    },
                );
            }
            Some(kept) if kept.value.same(&value) => {}
            Some(kept) => disagree = Some(kept.at.clone()),
        }
        if let Some(kept) = disagree {
            self.findings.insert(Finding {
                kind: Kind::SourcesDisagree,
                collection: Some(collection.name),
                pk: Some(pk.to_owned()),
                column: Some(col.to_owned()),
                at: vec![kept, at.clone()],
            });
        }
    }

    /// Mark an address protected from `at`: protection is only ever added, never cleared by an
    /// import, so a fill-only `false` can never stand between the owner's flag and a password.
    fn protect(&mut self, address_pk: &str, at: &Coord) {
        self.protected.insert(address_pk.to_owned());
        self.want(&ADDRESS, address_pk, "protected", json!(true), at);
    }

    /// The address a row names, or a finding. Credential-shaped cells are reported and never
    /// read; returns the address and the row's readable cells.
    fn address_of(
        &mut self,
        n: usize,
        sheet: &Sheet,
        row: usize,
        cells: &[String],
        email: Option<usize>,
    ) -> Option<String> {
        let mut readable = true;
        for (col, cell) in cells.iter().enumerate() {
            if guard::credential_shaped(cell) {
                self.find(
                    Kind::CredentialShaped,
                    None,
                    None,
                    vec![coord(sheet, n, row, col)],
                );
                if Some(col) == email {
                    readable = false;
                    self.address_unread = true;
                }
            }
        }
        let first = cells.iter().position(|c| !c.trim().is_empty())?;
        let Some(email) = email.filter(|_| readable) else {
            self.find(
                Kind::NoAddress,
                None,
                None,
                vec![coord(sheet, n, row, first)],
            );
            return None;
        };
        let text = cells.get(email).map_or("", |c| c.trim());
        if text.is_empty() {
            self.find(
                Kind::NoAddress,
                None,
                None,
                vec![coord(sheet, n, row, first)],
            );
            return None;
        }
        match keys::address(text) {
            Ok(address) => Some(address),
            Err(_) => {
                self.address_unread = true;
                self.find(
                    Kind::NotAnAddress,
                    None,
                    None,
                    vec![coord(sheet, n, row, email)],
                );
                None
            }
        }
    }

    /// Want the address row of `address` from `at`; returns its key: the row its key derives
    /// where the table holds it or this run planned it, else the one row whose `addr` says it now,
    /// else a row born under its key ([`AddressIndex::matched`]). An address several rows say,
    /// with no row under its key, is a finding, and `None`: the caller writes nothing of a source
    /// row whose own address it is, and links nothing to an address the row only refers to.
    fn address(&mut self, address: &str, kind: SheetKind, at: &Coord) -> Option<String> {
        let derived = keys::address_pk(address).ok()?;
        let pk = if self.has_row(ADDRESS.name, &derived) {
            derived
        } else {
            match self.address_index.matched(address).ok()? {
                AddressHolder::Row(pk) => pk.to_owned(),
                AddressHolder::Several(_) => {
                    self.address_unread = true;
                    self.find(
                        Kind::AddressAmbiguous,
                        Some(ADDRESS.name),
                        None,
                        vec![at.clone()],
                    );
                    return None;
                }
                AddressHolder::Free { .. } => derived,
            }
        };
        let domain = address.split_once('@').map_or("", |(_, d)| d).to_owned();
        self.want(&ADDRESS, &pk, "addr", json!(address), at);
        self.want(&ADDRESS, &pk, "domain", json!(domain), at);
        // The first sheet to list an address (the vault sheet's precedence) names its origin; a
        // later sheet listing it too is no disagreement.
        let named = self.held(&ADDRESS, &pk, "origin").is_some()
            || self
                .row_get(ADDRESS.name, &pk)
                .is_some_and(|row| row.values.contains_key("origin"));
        if !named {
            self.want(&ADDRESS, &pk, "origin", json!(kind.origin()), at);
        }
        self.listed
            .entry(address.to_owned())
            .or_insert_with(|| (pk.clone(), at.clone()));
        Some(pk)
    }

    fn vault(&mut self, n: usize, sheet: &Sheet) {
        let email = sheet.column(|h| h == "email");
        let first_computer = sheet.column(|h| h == "computer");
        let mut computers: Vec<usize> = sheet
            .headers
            .iter()
            .enumerate()
            .filter(|(_, h)| h.trim().eq_ignore_ascii_case("computer"))
            .map(|(at, _)| at)
            .collect();
        // The harness's layout: a blank-headed column right after the first `computer` is a
        // second machine column while a `claude` column follows it.
        if let Some(first) = first_computer
            && sheet
                .headers
                .get(first + 1)
                .is_some_and(|h| h.trim().is_empty())
            && sheet
                .headers
                .iter()
                .enumerate()
                .any(|(at, h)| at > first + 1 && h.trim().eq_ignore_ascii_case("claude"))
        {
            computers.push(first + 1);
        }
        let providers: Vec<(usize, &'static str)> = sheet
            .headers
            .iter()
            .enumerate()
            .filter_map(|(at, h)| crate::import::provider(h).map(|p| (at, p)))
            .collect();
        let notes_start = sheet
            .column(|h| h == "notes")
            .unwrap_or(sheet.headers.len());
        let mut seen: BTreeMap<String, Coord> = BTreeMap::new();
        for (index, cells) in sheet.rows.iter().enumerate() {
            let row = index + 2;
            if cells.iter().all(|c| c.trim().is_empty()) {
                continue;
            }
            let Some(address) = self.address_of(n, sheet, row, cells, email) else {
                continue;
            };
            let email_at = coord(sheet, n, row, email.unwrap_or(0));
            if let Some(first) = seen.get(&address) {
                self.find(
                    Kind::DuplicateAddress,
                    Some(ADDRESS.name),
                    None,
                    vec![first.clone(), email_at.clone()],
                );
            } else {
                seen.insert(address.clone(), email_at.clone());
            }
            let Some(address_pk) = self.address(&address, SheetKind::Vault, &email_at) else {
                continue;
            };
            let notes = if notes_start < cells.len() {
                cells[notes_start..].join(" ")
            } else {
                String::new()
            };
            let note_coords: Vec<Coord> = cells
                .iter()
                .enumerate()
                .filter(|(col, cell)| {
                    !cell.trim().is_empty()
                        && Some(*col) != email
                        && !computers.contains(col)
                        && !providers.iter().any(|(at, _)| at == col)
                })
                .map(|(col, _)| coord(sheet, n, row, col))
                .collect();
            for at in &note_coords {
                self.note(&ADDRESS, &address_pk, at);
            }
            let mut pieces: Vec<(Coord, String)> = cells
                .iter()
                .enumerate()
                .filter(|(col, cell)| {
                    *col >= notes_start
                        && !cell.trim().is_empty()
                        && Some(*col) != email
                        && !computers.contains(col)
                        && !providers.iter().any(|(at, _)| at == col)
                })
                .map(|(col, cell)| (coord(sheet, n, row, col), cell.clone()))
                .collect();
            let mut realms = BTreeSet::new();
            let mut accounts = Vec::new();
            let mut paused_accounts = Vec::new();
            for &(col, provider) in &providers {
                let cell = cells.get(col).map_or("", String::as_str);
                let at = coord(sheet, n, row, col);
                if guard::credential_shaped(cell) {
                    self.unsettled_logins.insert(address_pk.clone());
                    continue;
                }
                let (terms, paused) = match self.input.vendors.mark(provider, cell, &notes) {
                    Mark::Absent => continue,
                    Mark::Unread => {
                        self.unsettled_logins.insert(address_pk.clone());
                        self.find(Kind::FreeText, Some(ACCOUNT.name), None, vec![at.clone()]);
                        self.note(&ADDRESS, &address_pk, &at);
                        pieces.push((at, labelled(sheet, col, cell)));
                        continue;
                    }
                    Mark::Held => {
                        self.hold_kimi(&address, &address_pk);
                        self.find(Kind::Held, Some(ACCOUNT.name), None, vec![at]);
                        continue;
                    }
                    Mark::Member(terms) => (terms, false),
                    Mark::Paused(terms) => (terms, true),
                };
                let Some((realm, _)) = provider_realm(provider, terms.region) else {
                    self.hold_kimi(&address, &address_pk);
                    self.find(Kind::Held, Some(ACCOUNT.name), None, vec![at]);
                    continue;
                };
                realms.insert(realm);
                let Some(pk) = self.account(&address, &address_pk, realm, &at) else {
                    continue;
                };
                accounts.push(pk.clone());
                self.element(&ACCOUNT, &pk, "services", column_service(provider), &at);
                if let Some(region) = terms.region {
                    self.want(&ACCOUNT, &pk, "region", json!(region), &at);
                }
                if let Some(flow) = terms.flow {
                    self.want(&ACCOUNT, &pk, "flow", json!(flow), &at);
                }
                let status = if paused { "paused" } else { "pending" };
                self.want(&ACCOUNT, &pk, "status", json!(status), &at);
                if paused {
                    let mut at_all = vec![at.clone()];
                    at_all.extend(note_coords.iter().cloned());
                    self.find(Kind::Paused, Some(ACCOUNT.name), Some(&pk), at_all);
                    for note in &note_coords {
                        self.note(&ACCOUNT, &pk, note);
                    }
                    paused_accounts.push(pk);
                }
            }
            self.decided
                .entry(address_pk.clone())
                .and_modify(|(kept, _)| kept.extend(realms.iter().copied()))
                .or_insert((realms, email_at.clone()));
            for (k, &col) in computers.iter().enumerate() {
                let cell = LabelCell {
                    n,
                    sheet,
                    row,
                    col,
                    address_pk: &address_pk,
                    accounts: &accounts,
                    first: k == 0,
                };
                if let Some(text) = self.label_cell(&cell, cells) {
                    pieces.push((coord(sheet, n, row, col), labelled(sheet, col, &text)));
                }
            }
            pieces.sort_by_key(|(at, _)| at.col);
            if let Some((text, at)) = self.note_text(&ADDRESS, &address_pk, &pieces) {
                self.want(&ADDRESS, &address_pk, "note", json!(text), &at);
                for pk in &paused_accounts {
                    self.want(&ACCOUNT, pk, "note", json!(text), &at);
                }
            }
        }
    }

    /// The note a row's cells make: each flattened to one line and held to the note guard, the
    /// refused left out with a `note-withheld` finding (their coordinates are the row's already),
    /// the rest joined. The joined text is held to the guard too, since a value that crosses is
    /// the joined one: while it is refused, the last cell goes.
    fn note_text(
        &mut self,
        collection: &'static Collection,
        pk: &str,
        pieces: &[(Coord, String)],
    ) -> Option<(String, Coord)> {
        let mut kept: Vec<(Coord, String)> = Vec::new();
        for (at, text) in pieces {
            let mut flat = String::with_capacity(text.len());
            for word in text.split_whitespace() {
                if !flat.is_empty() {
                    flat.push(' ');
                }
                flat.push_str(word);
            }
            match guard::note_cell(&flat, NOTE_BYTES) {
                Ok(()) if !flat.is_empty() => kept.push((at.clone(), flat)),
                _ => self.withhold(collection, pk, at),
            }
        }
        loop {
            let joined = kept
                .iter()
                .map(|(_, text)| text.as_str())
                .collect::<Vec<_>>()
                .join(NOTE_JOIN);
            if guard::note_cell(&joined, NOTE_BYTES).is_ok() {
                return kept.first().map(|(at, _)| (joined, at.clone()));
            }
            let (at, _) = kept.pop()?;
            self.withhold(collection, pk, &at);
        }
    }

    fn withhold(&mut self, collection: &'static Collection, pk: &str, at: &Coord) {
        self.findings.insert(Finding {
            kind: Kind::NoteWithheld,
            collection: Some(collection.name),
            pk: Some(pk.to_owned()),
            column: Some("note".to_owned()),
            at: vec![at.clone()],
        });
    }

    /// The address an account signing in with row `address_pk`, which a source names as
    /// `address`, is keyed by: what that row's `addr` says in the table now where it says one
    /// address, else `address`. A renamed row keeps its key, so a sheet still naming its birth
    /// address finds it ([`Self::address`]); where the table and the sheet disagree the table
    /// wins, and the view and the harness name the account by what its row says.
    fn signs_in(&self, address: &str, address_pk: &str) -> String {
        self.current[ADDRESS.name]
            .rows
            .get(address_pk)
            .and_then(|row| match row.fields.get("addr") {
                Some(Field::Value(candidate)) => candidate.value.x.as_str(),
                _ => None,
            })
            .and_then(|text| keys::address(text).ok())
            .unwrap_or_else(|| address.to_owned())
    }

    /// Want the account of row `address_pk`, which the source names as `address`, at `realm`:
    /// keyed from the address it signs in as ([`Self::signs_in`]); returns its key.
    fn account(
        &mut self,
        address: &str,
        address_pk: &str,
        realm: &'static str,
        at: &Coord,
    ) -> Option<String> {
        let pk = keys::account_pk(realm, &self.signs_in(address, address_pk)).ok()?;
        let provider = keys::provider_of_realm(realm)?;
        self.want(&ACCOUNT, &pk, "provider", json!(provider), at);
        self.want(&ACCOUNT, &pk, "realm", json!(realm), at);
        self.want(&ACCOUNT, &pk, "login", json!(address_pk), at);
        // The default goes on a row with no label: a name the owner gave is no finding.
        if self.held(&ACCOUNT, &pk, "label").is_none()
            && let Ok(label) = keys::default_account_label(realm, &pk)
        {
            self.want(&ACCOUNT, &pk, "label", json!(label), at);
        }
        Some(pk)
    }

    /// Read one label cell into a preference per account and machine. The first computer cell
    /// reads as the grammar says (a bare `m3` is rank 1, as the sheets order `m3`, `m3.2`); any
    /// other cell is one more preference, ranked only where it writes a number. Returns the text
    /// of a cell that is free text, which the row's note keeps.
    fn label_cell(&mut self, cell: &LabelCell<'_>, cells: &[String]) -> Option<String> {
        let LabelCell {
            n,
            sheet,
            row,
            col,
            address_pk,
            accounts,
            first,
        } = *cell;
        let text = cells.get(col).map(|c| c.trim()).filter(|c| !c.is_empty())?;
        let at = coord(sheet, n, row, col);
        if guard::credential_shaped(text) {
            self.unsettled.extend(accounts.iter().cloned());
            return None;
        }
        let Ok(reading) = Reading::parse(text) else {
            self.find(
                Kind::FreeText,
                Some(PREFERENCE.name),
                None,
                vec![at.clone()],
            );
            self.note(&ADDRESS, address_pk, &at);
            self.unsettled.extend(accounts.iter().cloned());
            return Some(text.to_owned());
        };
        let stored = match reading {
            Reading::Machine(label) => label.canonical(),
            Reading::Slot(label, slot) => format!("{}.{slot}", label.canonical()),
            Reading::Pair { first, second } => {
                format!("{}-{}", first.canonical(), second.canonical())
            }
        };
        if stored != text {
            self.find(
                Kind::LabelRespelled,
                Some(PREFERENCE.name),
                None,
                vec![at.clone()],
            );
        }
        let lower = sheet.kind == SheetKind::Logins && self.decided.contains_key(address_pk);
        for preferred in reading.preferences() {
            // A preference the vault sheet decides is only compared: no machine is born for it.
            let machine = if lower {
                self.machine_key(preferred.machine)
            } else {
                self.machine_for(preferred.machine, &at)
            };
            let Some(machine) = machine else {
                self.unsettled.extend(accounts.iter().cloned());
                continue;
            };
            let rank = if !first && matches!(reading, Reading::Machine(_)) {
                None
            } else {
                preferred.rank
            };
            if lower {
                self.compare(address_pk, &machine, rank, &at);
                continue;
            }
            self.placed
                .entry(address_pk.to_owned())
                .or_default()
                .insert((machine.clone(), rank));
            if accounts.is_empty() {
                self.find(
                    Kind::NoAccount,
                    Some(PREFERENCE.name),
                    None,
                    vec![at.clone()],
                );
            }
            for account in accounts {
                self.prefer(&machine, account, rank, &at, sheet.kind);
            }
        }
        None
    }

    /// A lower sheet's label for an address the vault sheet decided: a finding where the vault
    /// sheet names another machine, or the same machine at another stated number.
    fn compare(&mut self, address_pk: &str, machine: &str, rank: Option<u32>, at: &Coord) {
        let Some((_, vault_at)) = self.decided.get(address_pk).cloned() else {
            return;
        };
        let agrees = self.placed.get(address_pk).is_some_and(|places| {
            places.iter().any(|(held, held_rank)| {
                held == machine && (rank.is_none() || held_rank.is_none() || *held_rank == rank)
            })
        });
        if !agrees {
            self.findings.insert(Finding {
                kind: Kind::SourcesDisagree,
                collection: Some(PREFERENCE.name),
                pk: None,
                column: None,
                at: vec![vault_at, at.clone()],
            });
        }
    }

    /// No sign-in decides a held Kimi cell's region: the Kimi accounts of row `address_pk`
    /// (named `address` in the source, [`Self::signs_in`]) keep their rows.
    fn hold_kimi(&mut self, address: &str, address_pk: &str) {
        let email = self.signs_in(address, address_pk);
        for realm in ["kimi:global", "kimi:mainland-cn"] {
            if let Ok(pk) = keys::account_pk(realm, &email) {
                self.unsettled.insert(pk);
            }
        }
    }

    /// Whether the registry's `collection.pk.column` holds what an import of a source above
    /// `kind` wrote, which `kind` never restates.
    fn held_above(
        &self,
        collection: &'static Collection,
        pk: &str,
        column: &str,
        kind: SheetKind,
    ) -> bool {
        self.current[collection.name]
            .rows
            .get(pk)
            .and_then(|row| row.fields.get(column))
            .filter(|field| field.value().is_some_and(|x| !x.is_null()))
            .and_then(|field| source_row(field, &self.current[SOURCE.name]))
            .and_then(|(_, row)| row.get("kind"))
            .and_then(Value::as_str)
            .and_then(|word| {
                [
                    SheetKind::Vault,
                    SheetKind::Phones,
                    SheetKind::Computers,
                    SheetKind::Emails,
                    SheetKind::Logins,
                    SheetKind::FleetAccounts,
                    SheetKind::AiPool,
                ]
                .into_iter()
                .find(|known| known.word() == word)
            })
            .is_some_and(|source| source < kind)
    }

    /// Want `machine` to prefer `account`. A row the owner wrote stays the owner's: the import
    /// does not call it its own. A rank a higher source wrote stands on a run that does not read
    /// that source: the machines' order must not depend on which Mac imported last.
    fn prefer(
        &mut self,
        machine: &str,
        account: &str,
        rank: Option<u32>,
        at: &Coord,
        kind: SheetKind,
    ) {
        let Ok(pk) = keys::preference_pk(machine, account) else {
            return;
        };
        self.want(&PREFERENCE, &pk, "machine", json!(machine), at);
        self.want(&PREFERENCE, &pk, "account", json!(account), at);
        if let Some(rank) = rank {
            if !self.held_above(&PREFERENCE, &pk, "rank", kind) {
                self.want(&PREFERENCE, &pk, "rank", json!(rank), at);
            } else if self.held(&PREFERENCE, &pk, "rank") != Some(&json!(rank)) {
                self.findings.insert(Finding {
                    kind: Kind::SourcesDisagree,
                    collection: Some(PREFERENCE.name),
                    pk: Some(pk.clone()),
                    column: Some("rank".to_owned()),
                    at: vec![at.clone()],
                });
            }
        }
        let owners = self.current[PREFERENCE.name]
            .rows
            .get(&pk)
            .and_then(|row| row.get("why"))
            .is_some_and(|why| why != &json!("import"));
        if !owners {
            self.want(&PREFERENCE, &pk, "why", json!("import"), at);
        }
    }

    /// A harness file's rows, below both sheets. An address the vault sheet decided keeps the
    /// accounts it gave; otherwise the row makes its account. Its machines are a set, not one
    /// fact to agree on, so each adds a preference no sheet stated, and a preference a sheet
    /// stated keeps the sheet's rank. The row's own label says the rank on the machine it names
    /// (`m8-2` on m8); a warm list and the other machines state no order on their machine. Every
    /// machine the label names is one more preference (`m106-m38`), whatever the row's list says.
    fn harness(&mut self, n: usize, sheet: &Sheet) {
        const REALM: usize = 0;
        const EMAIL: usize = 1;
        const LABEL: usize = 2;
        const MACHINES: usize = 3;
        for (index, cells) in sheet.rows.iter().enumerate() {
            let row = index + 2;
            let Some(address) = self.address_of(n, sheet, row, cells, Some(EMAIL)) else {
                continue;
            };
            let at = coord(sheet, n, row, EMAIL);
            let Some(address_pk) = self.address(&address, sheet.kind, &at) else {
                continue;
            };
            let Some(realm) = keys::canonical_realm(&cells[REALM]) else {
                self.find(
                    Kind::FreeText,
                    Some(ACCOUNT.name),
                    None,
                    vec![coord(sheet, n, row, REALM)],
                );
                continue;
            };
            if let Some((realms, vault_at)) = self.decided.get(&address_pk).cloned()
                && !realms.contains(realm)
            {
                self.findings.insert(Finding {
                    kind: Kind::SourcesDisagree,
                    collection: Some(ACCOUNT.name),
                    pk: None,
                    column: None,
                    at: vec![vault_at, at],
                });
                continue;
            }
            let Some(pk) = self.account(&address, &address_pk, realm, &at) else {
                continue;
            };
            let decided = self.decided.contains_key(&address_pk);
            self.birth_status(&pk, &at);
            if !decided && let Some(provider) = keys::provider_of_realm(realm) {
                self.element(&ACCOUNT, &pk, "services", column_service(provider), &at);
            }
            let legacy = cells[LABEL].trim();
            if !legacy.is_empty() && guard::text(legacy, 64).is_ok() {
                let label_at = coord(sheet, n, row, LABEL);
                self.want(&ACCOUNT, &pk, "legacy_label", json!(legacy), &label_at);
            }
            // `kimi-m16` is `<cli>-<machine label>`.
            let reading = legacy
                .split_once('-')
                .filter(|(cli, _)| !guard::machine_shaped(cli))
                .map_or(legacy, |(_, label)| label);
            let named: Vec<_> = Reading::parse(reading)
                .ok()
                .iter()
                .flat_map(Reading::preferences)
                .collect();
            let machines_at = coord(sheet, n, row, MACHINES);
            let mut listed = Vec::new();
            for word in cells[MACHINES].split_whitespace() {
                let Ok(label) = Label::parse(word) else {
                    self.find(
                        Kind::FreeText,
                        Some(PREFERENCE.name),
                        None,
                        vec![machines_at.clone()],
                    );
                    self.unsettled.insert(pk.clone());
                    continue;
                };
                let Some(machine) = self.machine_for(label, &machines_at) else {
                    self.unsettled.insert(pk.clone());
                    continue;
                };
                listed.push(label);
                let rank = named
                    .iter()
                    .find(|preferred| preferred.machine == label)
                    .and_then(|preferred| preferred.rank);
                self.prefer_below(&machine, &pk, rank, &machines_at, sheet.kind);
            }
            let label_at = coord(sheet, n, row, LABEL);
            for preferred in &named {
                if listed.contains(&preferred.machine) {
                    continue;
                }
                let Some(machine) = self.machine_for(preferred.machine, &label_at) else {
                    self.unsettled.insert(pk.clone());
                    continue;
                };
                self.prefer_below(&machine, &pk, preferred.rank, &label_at, sheet.kind);
            }
        }
    }

    /// Want `machine` to prefer `account` unless a source above this one already stated it this
    /// run.
    fn prefer_below(
        &mut self,
        machine: &str,
        account: &str,
        rank: Option<u32>,
        at: &Coord,
        kind: SheetKind,
    ) {
        let held = keys::preference_pk(machine, account)
            .is_ok_and(|pref| self.has_row(PREFERENCE.name, &pref));
        if !held {
            self.prefer(machine, account, rank, at, kind);
        }
    }

    /// The registry's value of a cell, as this plan found it.
    fn held(&self, collection: &'static Collection, pk: &str, column: &str) -> Option<&Value> {
        self.current[collection.name].rows.get(pk)?.get(column)
    }

    /// A lower source's account starts `pending` only where nothing says otherwise: the vault
    /// sheet, the owner or an earlier import's `paused` stands on a run that reads no vault sheet.
    fn birth_status(&mut self, pk: &str, at: &Coord) {
        let wanted = self
            .row_get(ACCOUNT.name, pk)
            .is_some_and(|row| row.values.contains_key("status"));
        if !wanted && self.held(&ACCOUNT, pk, "status").is_none() {
            self.want(&ACCOUNT, pk, "status", json!("pending"), at);
        }
    }

    /// A `+tag` of a listed login is an alias of it, where no correction said anything. A run with
    /// no logins sheet cannot see the correction, so it guesses nothing.
    fn aliases(&mut self) {
        if !self
            .input
            .sheets
            .iter()
            .any(|s| s.kind == SheetKind::Logins)
        {
            return;
        }
        let listed: Vec<(String, String, Coord)> = self
            .listed
            .iter()
            .map(|(address, (pk, at))| (address.clone(), pk.clone(), at.clone()))
            .collect();
        for (address, pk, at) in listed {
            let Some(base) = super::mail::untagged(&address) else {
                continue;
            };
            let Some(target) = self.listed.get(&base).map(|(pk, _)| pk.clone()) else {
                continue;
            };
            if !self.corrected.contains(&pk) {
                self.link_alias(&pk, &target, &at);
            }
        }
    }

    fn link_alias(&mut self, pk: &str, target: &str, at: &Coord) {
        self.want(&ADDRESS, pk, "alias_of", json!(target), at);
        self.want(&ADDRESS, pk, "kind", json!("alias"), at);
    }

    fn logins(&mut self, n: usize, sheet: &Sheet) -> Result<()> {
        let email = sheet.column(|h| h == "ai login email" || h == "email");
        let computer = sheet.column(|h| h == "computer");
        let services = sheet.column(|h| h == "services");
        let guess = sheet.column(|h| h.starts_with("codes arrive"));
        let correction = sheet.column(|h| h.starts_with("your correction"));
        let mut seen: BTreeMap<String, Coord> = BTreeMap::new();
        for (index, cells) in sheet.rows.iter().enumerate() {
            let row = index + 2;
            if cells.iter().all(|c| c.trim().is_empty()) {
                continue;
            }
            let Some(address) = self.address_of(n, sheet, row, cells, email) else {
                continue;
            };
            let email_at = coord(sheet, n, row, email.unwrap_or(0));
            if let Some(first) = seen.get(&address) {
                self.find(
                    Kind::DuplicateAddress,
                    Some(ADDRESS.name),
                    None,
                    vec![first.clone(), email_at.clone()],
                );
            } else {
                seen.insert(address.clone(), email_at.clone());
            }
            let Some(address_pk) = self.address(&address, SheetKind::Logins, &email_at) else {
                continue;
            };
            // Every other cell is the lead's or free text: coordinates only, unless read below.
            for (col, cell) in cells.iter().enumerate() {
                if !cell.trim().is_empty()
                    && Some(col) != email
                    && Some(col) != computer
                    && Some(col) != services
                    && Some(col) != guess
                    && Some(col) != correction
                {
                    self.note(&ADDRESS, &address_pk, &coord(sheet, n, row, col));
                }
            }
            let accounts = services.map_or_else(Vec::new, |col| {
                self.services(n, sheet, row, cells, col, &address, &address_pk)
            });
            if correction
                .and_then(|col| cells.get(col))
                .is_some_and(|text| !text.trim().is_empty())
            {
                self.corrected.insert(address_pk.clone());
            }
            let mut pieces = Vec::new();
            if let Some(col) = computer {
                let cell = LabelCell {
                    n,
                    sheet,
                    row,
                    col,
                    address_pk: &address_pk,
                    accounts: &accounts,
                    first: true,
                };
                if let Some(text) = self.label_cell(&cell, cells) {
                    pieces.push((coord(sheet, n, row, col), labelled(sheet, col, &text)));
                }
            }
            for (col, column) in [(guess, "mailbox_hint"), (correction, "mailbox")] {
                let Some(col) = col else { continue };
                let text = cells.get(col).map_or("", |c| c.trim());
                if text.is_empty() || guard::credential_shaped(text) {
                    continue;
                }
                let at = coord(sheet, n, row, col);
                match keys::address(text) {
                    Ok(mailbox) => {
                        // The owner's word that codes for this login arrive at another listed
                        // login: the alias link the owner's model names, to the row that login
                        // names (`address`, which the mailbox's own login reads again below).
                        if column == "mailbox"
                            && mailbox != address
                            && self.logins.contains(&mailbox)
                            && let Some(target) = self.address(&mailbox, sheet.kind, &at)
                        {
                            self.link_alias(&address_pk, &target, &at);
                        }
                        if let Some(pk) = self.mailbox(&mailbox, sheet.kind, &at)? {
                            self.want(&ADDRESS, &address_pk, column, json!(pk), &at);
                        }
                    }
                    Err(_) => {
                        self.find(
                            Kind::FreeText,
                            Some(ADDRESS.name),
                            Some(&address_pk),
                            vec![at.clone()],
                        );
                        self.note(&ADDRESS, &address_pk, &at);
                        // The owner's own words are a note; the lead's guess stays a coordinate.
                        if column == "mailbox" {
                            pieces.push((at.clone(), text.to_owned()));
                        }
                    }
                }
            }
            if let Some((text, at)) = self.note_text(&ADDRESS, &address_pk, &pieces) {
                self.want(&ADDRESS, &address_pk, "note", json!(text), &at);
            }
        }
        Ok(())
    }

    /// The logins sheet's `services` cell: `+` separates services, `/` joins a service's words.
    /// Returns the accounts this sheet makes, none where the vault sheet decided the address.
    #[allow(clippy::too_many_arguments)]
    fn services(
        &mut self,
        n: usize,
        sheet: &Sheet,
        row: usize,
        cells: &[String],
        col: usize,
        address: &str,
        address_pk: &str,
    ) -> Vec<String> {
        let mut accounts = Vec::new();
        let text = cells.get(col).map_or("", |c| c.trim());
        if text.is_empty() {
            return accounts;
        }
        if guard::credential_shaped(text) {
            self.unsettled_logins.insert(address_pk.to_owned());
            return accounts;
        }
        let at = coord(sheet, n, row, col);
        let decided = self.decided.get(address_pk).cloned();
        let mut unread = false;
        for group in text.split(['+', ',', ';']) {
            let words: Vec<&str> = group
                .split('/')
                .map(str::trim)
                .filter(|w| !w.is_empty())
                .collect();
            if words.is_empty() {
                continue;
            }
            let read: Vec<(&str, &str)> = words.iter().filter_map(|w| service_word(w)).collect();
            if read.len() != words.len() {
                unread = true;
            }
            let Some(&(provider, _)) = read.first() else {
                continue;
            };
            if read.iter().any(|(p, _)| *p != provider) {
                unread = true;
                continue;
            }
            let Some((realm, _)) = provider_realm(provider, None) else {
                // Kimi with no region, as the vault sheet's bare 1.
                self.hold_kimi(address, address_pk);
                if decided
                    .as_ref()
                    .is_none_or(|(realms, _)| !realms.iter().any(|r| r.starts_with("kimi")))
                {
                    self.find(Kind::Held, Some(ACCOUNT.name), None, vec![at.clone()]);
                }
                continue;
            };
            match &decided {
                Some((realms, vault_at)) if !realms.contains(realm) => {
                    self.findings.insert(Finding {
                        kind: Kind::SourcesDisagree,
                        collection: Some(ACCOUNT.name),
                        pk: None,
                        column: None,
                        at: vec![vault_at.clone(), at.clone()],
                    });
                }
                Some(_) => {
                    if let Ok(pk) = keys::account_pk(realm, &self.signs_in(address, address_pk)) {
                        for (_, service) in &read {
                            self.element(&ACCOUNT, &pk, "services", service, &at);
                        }
                    }
                }
                None => {
                    let Some(pk) = self.account(address, address_pk, realm, &at) else {
                        continue;
                    };
                    self.birth_status(&pk, &at);
                    for (_, service) in &read {
                        self.element(&ACCOUNT, &pk, "services", service, &at);
                    }
                    if !accounts.contains(&pk) {
                        accounts.push(pk);
                    }
                }
            }
        }
        if unread {
            self.unsettled_logins.insert(address_pk.to_owned());
            self.find(Kind::FreeText, Some(ACCOUNT.name), None, vec![at.clone()]);
            self.note(&ADDRESS, address_pk, &at);
        }
        accounts
    }

    /// The mailbox that signs in as `address`: keyed from the key of that address's row
    /// ([`keys::mailbox_pk`], the row [`Self::address`] names, wanted too, from `kind`), with its
    /// `login` that row. A new row's label is the first free `<service>-<n>`; a held row keeps the
    /// label it has. `None` where the address names no one row (several say it): nothing of the
    /// mailbox is written.
    fn mailbox(&mut self, address: &str, kind: SheetKind, at: &Coord) -> Result<Option<String>> {
        let Some(login) = self.address(address, kind, at) else {
            return Ok(None);
        };
        let pk = keys::mailbox_pk(&login)?;
        let domain = address.split_once('@').map_or("", |(_, d)| d);
        let (service, code_path) = mailbox_service(domain);
        let held = self.held(&MAILBOX, &pk, "label").is_some()
            || self
                .row_get(MAILBOX.name, &pk)
                .is_some_and(|row| row.values.contains_key("label"));
        if !held {
            let used: BTreeSet<&str> = self
                .current
                .get(MAILBOX.name)
                .into_iter()
                .flat_map(|projection| projection.rows.values())
                .filter_map(|row| row.get("label").and_then(Value::as_str))
                .chain(self.minted_labels.iter().map(String::as_str))
                .collect();
            let label = (1..)
                .map(|k| format!("{service}-{k}"))
                .find(|label| !used.contains(label.as_str()))
                .ok_or_else(|| Error::new("no mailbox label could be minted"))?;
            self.minted_labels.insert(label.clone());
            self.want(&MAILBOX, &pk, "label", json!(label), at);
        }
        self.want(&MAILBOX, &pk, "service", json!(service), at);
        self.want(&MAILBOX, &pk, "code_path", json!(code_path), at);
        self.want(&MAILBOX, &pk, "login", json!(login), at);
        Ok(Some(pk))
    }

    /// The phone row of a number (`raw`, at least four digits once read by [`phone_digits`]):
    /// the row this run already named for it, else the live row whose sealed `number` opens to
    /// it (the smallest key, with a finding, where several do), else a fresh random key and
    /// label (`label` where it names one no row holds). Its `number` is wanted as a `Secret`
    /// and its `last4` beside it.
    fn phone(&mut self, raw: &str, label: Option<&str>, at: &Coord) -> Result<Option<String>> {
        let Some(digits) = phone_digits(raw) else {
            return Ok(None);
        };
        let last4 = digits[digits.len() - 4..].to_owned();
        let number = Arc::new(Secret::new(digits.clone()));
        let pk = if let Some(pk) = self.phones.get(&digits) {
            pk.clone()
        } else {
            let sealer = self.input.sealer;
            // Every row holding the number, live ones first: a retired row is found and left as
            // it is, as a retired address is, never minted again.
            let mut held: Vec<(bool, String)> = Vec::new();
            let mut unopened = false;
            for (pk, row) in &self.current[PHONE.name].rows {
                let tokens: Vec<&Value> = row
                    .fields
                    .get("number")
                    .map(Field::candidates_x)
                    .unwrap_or_default();
                let mut holds = false;
                for token in tokens.into_iter().filter_map(Value::as_str) {
                    match sealer.open(PHONE.name, pk, "number", token) {
                        Ok(opened) if opened.same(&number) => holds = true,
                        Ok(_) => {}
                        Err(_) => unopened = true,
                    }
                }
                if holds {
                    held.push((!row.live(), pk.clone()));
                }
            }
            held.sort_unstable();
            let live = held.iter().filter(|(retired, _)| !retired).count();
            if live > 1 {
                self.findings.insert(Finding {
                    kind: Kind::DuplicateNumber,
                    collection: Some(PHONE.name),
                    pk: held.first().map(|(_, pk)| pk.clone()),
                    column: Some("number".to_owned()),
                    at: vec![at.clone()],
                });
            }
            if held.is_empty() && unopened {
                return Err(Error::new(
                    "a phone row's number does not open with the key this Mac holds, so whether \
                     the registry already holds a number is unknown: import where every number opens",
                ));
            }
            let pk = match held.into_iter().map(|(_, pk)| pk).next() {
                Some(pk) => pk,
                None => {
                    let pk = keys::random_pk(keys::Random::Phone)?;
                    let used: BTreeSet<&str> = self.current[PHONE.name]
                        .rows
                        .values()
                        .filter_map(|row| row.get("label").and_then(Value::as_str))
                        .chain(self.minted_labels.iter().map(String::as_str))
                        .collect();
                    let given = label.filter(|l| !used.contains(*l)).map(str::to_owned);
                    let label = match given {
                        Some(label) => label,
                        None => (1..)
                            .map(|k| format!("phone-{k}"))
                            .find(|label| !used.contains(label.as_str()))
                            .ok_or_else(|| Error::new("no phone label could be minted"))?,
                    };
                    self.minted_labels.insert(label.clone());
                    self.want(&PHONE, &pk, "label", json!(label), at);
                    pk
                }
            };
            self.phones.insert(digits, pk.clone());
            pk
        };
        self.want(&PHONE, &pk, "last4", json!(last4), at);
        self.want_secret(&PHONE, &pk, "number", number, at);
        Ok(Some(pk))
    }

    fn phones(&mut self, n: usize, sheet: &Sheet) -> Result<()> {
        let number = sheet.column(|h| h.starts_with("phone") || h == "number");
        let carrier = sheet.column(|h| h.starts_with("carrier") || h == "provider");
        let line_type = sheet.column(|h| h.starts_with("line") || h == "type");
        let device = sheet.column(|h| h.starts_with("associated") || h.starts_with("device"));
        let notes = sheet.column(|h| h == "notes");
        for (index, cells) in sheet.rows.iter().enumerate() {
            let row = index + 2;
            if cells.iter().all(|c| c.trim().is_empty()) {
                continue;
            }
            let Some(num_col) = number else {
                continue;
            };
            let raw_num = cells.get(num_col).map_or("", |c| c.trim());
            let num_at = coord(sheet, n, row, num_col);
            let Some(pk) = self.phone(raw_num, None, &num_at)? else {
                if !raw_num.is_empty() {
                    self.find(Kind::FreeText, Some(PHONE.name), None, vec![num_at]);
                }
                continue;
            };
            for (col_opt, column, limit) in [
                (carrier, "carrier", 64),
                (line_type, "line_type", 32),
                (device, "device", 64),
            ] {
                if let Some(col) = col_opt
                    && let Some(val) = cells.get(col).map(|c| c.trim()).filter(|c| !c.is_empty())
                {
                    let at = coord(sheet, n, row, col);
                    if guard::text(val, limit).is_ok() && !val.contains('@') {
                        self.want(&PHONE, &pk, column, json!(val), &at);
                    } else {
                        self.note(&PHONE, &pk, &at);
                    }
                }
            }
            if let Some(col) = notes
                && let Some(val) = cells.get(col).map(|c| c.trim()).filter(|c| !c.is_empty())
            {
                let at = coord(sheet, n, row, col);
                self.note(&PHONE, &pk, &at);
                if let Some((text, note_at)) = self.note_text(&PHONE, &pk, &[(at, val.to_owned())])
                {
                    self.want(&PHONE, &pk, "note", json!(text), &note_at);
                }
            }
        }
        Ok(())
    }

    fn computers(&mut self, n: usize, sheet: &Sheet) {
        let id = sheet.column(|h| h == "id" || h == "machine" || h == "label");
        let make = sheet.column(|h| h == "make");
        let arch = sheet.column(|h| h == "arch");
        let os = sheet.column(|h| h == "os");
        let chip = sheet.column(|h| h == "processor" || h == "chip");
        let ram = sheet.column(|h| h == "ram" || h == "ram_gb");
        let disk = sheet.column(|h| h == "hd" || h == "disk" || h == "disk_gb");
        let aliases = sheet.column(|h| h == "aliases");
        let nickname = sheet.column(|h| h == "nickname");
        let nick_aliases = sheet.column(|h| h == "nickname aliases" || h == "nickname_aliases");
        let location = sheet.column(|h| h == "location");
        let notes = sheet.column(|h| h == "notes");
        for (index, cells) in sheet.rows.iter().enumerate() {
            let row = index + 2;
            if cells.iter().all(|c| c.trim().is_empty()) {
                continue;
            }
            let Some(id_col) = id else {
                continue;
            };
            let raw_id = cells.get(id_col).map_or("", |c| c.trim());
            if raw_id.is_empty() {
                continue;
            }
            let id_at = coord(sheet, n, row, id_col);
            let Ok(label) = Label::parse(raw_id) else {
                self.find(Kind::FreeText, Some(MACHINE.name), None, vec![id_at]);
                continue;
            };
            let Some(pk) = self.machine_for(label, &id_at) else {
                continue;
            };
            for (col_opt, column, limit) in [
                (make, "make", 32),
                (arch, "arch", 16),
                (os, "os", 32),
                (chip, "chip", 32),
                (nickname, "nickname", 64),
                (location, "location", 64),
            ] {
                if let Some(col) = col_opt
                    && let Some(val) = cells.get(col).map(|c| c.trim()).filter(|c| !c.is_empty())
                {
                    let at = coord(sheet, n, row, col);
                    if guard::text(val, limit).is_ok() && !val.contains('@') {
                        self.want(&MACHINE, &pk, column, json!(val), &at);
                    } else {
                        self.note(&MACHINE, &pk, &at);
                    }
                }
            }
            for (col_opt, column) in [(ram, "ram_gb"), (disk, "disk_gb")] {
                if let Some(col) = col_opt
                    && let Some(val) = cells.get(col).map(|c| c.trim()).filter(|c| !c.is_empty())
                {
                    let at = coord(sheet, n, row, col);
                    let digits_end = val
                        .bytes()
                        .position(|b| !b.is_ascii_digit())
                        .unwrap_or(val.len());
                    if let Ok(num) = val[..digits_end].parse::<u64>() {
                        self.want(&MACHINE, &pk, column, json!(num), &at);
                    } else {
                        self.note(&MACHINE, &pk, &at);
                    }
                }
            }
            let mut extra_nick_aliases: Vec<String> = Vec::new();
            let mut nick_at: Option<Coord> = None;
            if let Some(col) = aliases
                && let Some(val) = cells.get(col).map(|c| c.trim()).filter(|c| !c.is_empty())
            {
                let at = coord(sheet, n, row, col);
                for part in val
                    .split([',', ';', ' '])
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    let parsed = Label::parse(part).or_else(|e| {
                        if part.contains('.') {
                            Label::parse(&part.replace('.', "-"))
                        } else {
                            Err(e)
                        }
                    });
                    if let Ok(alias_label) = parsed {
                        let canon = alias_label.canonical();
                        if canon != label.canonical() {
                            self.element(&MACHINE, &pk, "aliases", &canon, &at);
                        }
                    } else if guard::text(part, 64).is_ok()
                        && !part.contains('@')
                        && !extra_nick_aliases.iter().any(|s| s == part)
                    {
                        extra_nick_aliases.push(part.to_owned());
                        nick_at.get_or_insert_with(|| at.clone());
                    }
                }
            }
            if let Some(col) = nick_aliases
                && let Some(val) = cells.get(col).map(|c| c.trim()).filter(|c| !c.is_empty())
            {
                let at = coord(sheet, n, row, col);
                for part in val
                    .split([',', ';'])
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    if guard::text(part, 64).is_ok()
                        && !part.contains('@')
                        && !extra_nick_aliases.iter().any(|s| s == part)
                    {
                        extra_nick_aliases.push(part.to_owned());
                        nick_at.get_or_insert_with(|| at.clone());
                    }
                }
            }
            if !extra_nick_aliases.is_empty()
                && let Some(at) = nick_at
            {
                extra_nick_aliases.truncate(8);
                self.want(
                    &MACHINE,
                    &pk,
                    "nickname_aliases",
                    json!(extra_nick_aliases),
                    &at,
                );
            }
            if let Some(col) = notes
                && let Some(val) = cells.get(col).map(|c| c.trim()).filter(|c| !c.is_empty())
            {
                let at = coord(sheet, n, row, col);
                self.note(&MACHINE, &pk, &at);
                if let Some((text, note_at)) =
                    self.note_text(&MACHINE, &pk, &[(at, val.to_owned())])
                {
                    self.want(&MACHINE, &pk, "note", json!(text), &note_at);
                }
            }
        }
    }

    fn emails(&mut self, n: usize, sheet: &Sheet) -> Result<()> {
        let email = sheet.column(|h| h == "email address" || h == "email");
        let root = sheet.column(|h| h == "root account" || h == "root");
        let password = sheet.column(|h| h == "password");
        let status = sheet.column(|h| h == "status" || h == "account status");
        let provider = sheet.column(|h| h == "provider");
        let phone = sheet.column(|h| h.starts_with("activation phone") || h == "phone");
        let protection = sheet.column(|h| h.starts_with("protection"));
        let notes = sheet.column(|h| h == "notes");
        let mut seen: BTreeMap<String, Coord> = BTreeMap::new();
        for (index, cells) in sheet.rows.iter().enumerate() {
            let row = index + 2;
            if cells.iter().all(|c| c.trim().is_empty()) {
                continue;
            }
            let has_password = password
                .and_then(|col| cells.get(col))
                .is_some_and(|val| !val.trim().is_empty());
            // Never inspect or copy the Password column if present in an unstripped export.
            let sanitized: std::borrow::Cow<'_, [String]> = match password {
                Some(pw_col) if has_password => {
                    let mut copy = Vec::with_capacity(cells.len());
                    for (i, cell) in cells.iter().enumerate() {
                        copy.push(if i == pw_col {
                            String::new()
                        } else {
                            cell.clone()
                        });
                    }
                    std::borrow::Cow::Owned(copy)
                }
                _ => std::borrow::Cow::Borrowed(cells),
            };
            let Some(address) = self.address_of(n, sheet, row, &sanitized, email) else {
                continue;
            };
            let email_at = coord(sheet, n, row, email.unwrap_or(0));
            if let Some(first) = seen.get(&address) {
                self.find(
                    Kind::DuplicateAddress,
                    Some(ADDRESS.name),
                    None,
                    vec![first.clone(), email_at.clone()],
                );
            } else {
                seen.insert(address.clone(), email_at.clone());
            }
            let Some(address_pk) = self.address(&address, SheetKind::Emails, &email_at) else {
                continue;
            };
            let is_protected = protection
                .and_then(|col| sanitized.get(col))
                .is_some_and(|val| !val.trim().is_empty());
            if is_protected && let Some(col) = protection {
                let at = coord(sheet, n, row, col);
                self.protect(&address_pk, &at);
            }
            if let Some(col) = status
                && let Some(val) = sanitized
                    .get(col)
                    .map(|c| c.trim())
                    .filter(|c| !c.is_empty())
            {
                let at = coord(sheet, n, row, col);
                let word = if val.eq_ignore_ascii_case("active") {
                    "active"
                } else if val.eq_ignore_ascii_case("suspended") {
                    "suspended"
                } else if val.eq_ignore_ascii_case("disabled") {
                    "disabled"
                } else {
                    "unknown"
                };
                self.want(&ADDRESS, &address_pk, "status", json!(word), &at);
            }
            if let Some(col) = provider
                && let Some(val) = sanitized
                    .get(col)
                    .map(|c| c.trim())
                    .filter(|c| !c.is_empty())
            {
                let at = coord(sheet, n, row, col);
                let word = if val.eq_ignore_ascii_case("proton")
                    || val.eq_ignore_ascii_case("protonmail")
                {
                    "proton"
                } else if val.eq_ignore_ascii_case("gmail") {
                    "gmail"
                } else if val.eq_ignore_ascii_case("google") {
                    "google"
                } else if val.eq_ignore_ascii_case("outlook") || val.eq_ignore_ascii_case("hotmail")
                {
                    "outlook"
                } else if val.eq_ignore_ascii_case("privateemail") {
                    "privateemail"
                } else {
                    "other"
                };
                self.want(&ADDRESS, &address_pk, "provider", json!(word), &at);
            }
            if let Some(col) = phone
                && let Some(val) = sanitized
                    .get(col)
                    .map(|c| c.trim())
                    .filter(|c| !c.is_empty())
            {
                let at = coord(sheet, n, row, col);
                if let Some(phone_pk) = self.phone(val, None, &at)? {
                    self.want(&ADDRESS, &address_pk, "phone", json!(phone_pk), &at);
                } else {
                    self.note(&ADDRESS, &address_pk, &at);
                }
            }
            if let Some(col) = root
                && let Some(val) = sanitized
                    .get(col)
                    .map(|c| c.trim())
                    .filter(|c| !c.is_empty())
            {
                let at = coord(sheet, n, row, col);
                if let Ok(root_addr) = keys::address(val) {
                    if root_addr != address {
                        self.corrected.insert(address_pk.clone());
                        if let Some(root_pk) = self.address(&root_addr, SheetKind::Emails, &at) {
                            self.link_alias(&address_pk, &root_pk, &at);
                        }
                        if !is_protected
                            && let Some(mb_pk) = self.mailbox(&root_addr, SheetKind::Emails, &at)?
                        {
                            self.want(&ADDRESS, &address_pk, "mailbox", json!(mb_pk), &at);
                        }
                    } else {
                        self.want(&ADDRESS, &address_pk, "kind", json!("standalone"), &at);
                    }
                } else {
                    self.find(
                        Kind::FreeText,
                        Some(ADDRESS.name),
                        Some(&address_pk),
                        vec![at],
                    );
                }
            } else if has_password
                && !is_protected
                && let Some(pw_col) = password
            {
                // The sheet's flag says this address signs in to its own mailbox; its password,
                // where there is one, comes from a replica's dump, never from this column.
                let at = coord(sheet, n, row, pw_col);
                if let Some(mb_pk) = self.mailbox(&address, SheetKind::Emails, &at)? {
                    self.want(&ADDRESS, &address_pk, "mailbox", json!(mb_pk), &at);
                }
                self.want(
                    &ADDRESS,
                    &address_pk,
                    "kind",
                    json!("standalone"),
                    &email_at,
                );
            }
            if let Some(col) = notes
                && let Some(val) = sanitized
                    .get(col)
                    .map(|c| c.trim())
                    .filter(|c| !c.is_empty())
            {
                let at = coord(sheet, n, row, col);
                self.note(&ADDRESS, &address_pk, &at);
                let has_note = self
                    .row_get(ADDRESS.name, &address_pk)
                    .is_some_and(|r| r.values.contains_key("note"));
                if !has_note
                    && let Some((text, note_at)) =
                        self.note_text(&ADDRESS, &address_pk, &[(at, val.to_owned())])
                {
                    self.want(&ADDRESS, &address_pk, "note", json!(text), &note_at);
                }
            }
        }
        Ok(())
    }

    /// The live machine rows whose label or an alias is `label`.
    fn live_by_label(&self, label: &str) -> Vec<String> {
        self.current[MACHINE.name]
            .rows
            .iter()
            .filter(|(_, row)| row.live())
            .filter(|(_, row)| {
                row.get("label").and_then(Value::as_str) == Some(label)
                    || row
                        .fields
                        .get("aliases")
                        .is_some_and(|f| f.has_member(label))
            })
            .map(|(pk, _)| pk.clone())
            .collect()
    }

    /// The live machine rows whose fleet node is `node`: one, or a clash any writer could make,
    /// since `fleet_node` is an owner cell.
    fn live_by_node(&self, node: &str) -> Vec<String> {
        self.current[MACHINE.name]
            .rows
            .iter()
            .filter(|(_, row)| {
                row.live() && row.get("fleet_node").and_then(Value::as_str) == Some(node)
            })
            .map(|(pk, _)| pk.clone())
            .collect()
    }

    fn live_by_host(&self, hosts: &[&str]) -> Option<String> {
        self.current[MACHINE.name]
            .rows
            .iter()
            .find(|(_, row)| {
                row.live()
                    && row
                        .get("hosts")
                        .and_then(Value::as_array)
                        .is_some_and(|list| {
                            list.iter()
                                .filter_map(Value::as_str)
                                .any(|h| hosts.contains(&h))
                        })
            })
            .map(|(pk, _)| pk.clone())
    }

    /// The known Mac a label names, by its label or an alias.
    fn known(&self, label: &Label) -> Option<&KnownMac> {
        self.input
            .macs
            .iter()
            .find(|mac| mac.label.as_ref() == Some(label) || mac.aliases.contains(label))
    }

    /// The machine row a label names: a live row by label or alias; else the known Mac it names,
    /// by its row's fleet node or host, or a new row keyed by its node; else a planned
    /// placeholder keyed by the label ([`keys::planned_machine_pk`]: a retired row holding that key
    /// leaves it, and the placeholder takes the first free `<label>/<n>`). `None` when the label is ambiguous or births are off and
    /// nothing matched.
    fn machine_for(&mut self, label: Label, at: &Coord) -> Option<String> {
        let stored = label.canonical();
        let found = self.live_by_label(&stored);
        match found.len() {
            1 => return found.into_iter().next(),
            0 => {}
            _ => {
                self.find(
                    Kind::LabelAmbiguous,
                    Some(MACHINE.name),
                    None,
                    vec![at.clone()],
                );
                return None;
            }
        }
        if let Some(mac) = self.known(&label).cloned() {
            return self.known_row(&mac);
        }
        if !self.input.births {
            return None;
        }
        // The label's birth key unless a row holds it (a retired Mac the label named, or one
        // relabelled away): then the first free `<label>/<n>`, as `add machine` keys it, so the
        // new preference names a row of its own and the old row's history stays apart.
        let rows = &self.current[MACHINE.name].rows;
        let pk = keys::planned_machine_pk(&stored, |pk| rows.contains_key(pk))?;
        self.want(&MACHINE, &pk, "label", json!(stored), at);
        self.want(&MACHINE, &pk, "status", json!("planned"), at);
        Some(pk)
    }

    /// The live row a known Mac already has: by its fleet node (`true`), then a host, then its
    /// label or an alias. `Err` when more than one live row names its node: then no row is the
    /// Mac's until the owner keeps one.
    fn known_found(&self, mac: &KnownMac) -> std::result::Result<Option<(String, bool)>, ()> {
        let by_node = valid_node(mac)
            .map(|node| self.live_by_node(&node))
            .unwrap_or_default();
        match by_node.as_slice() {
            [pk] => return Ok(Some((pk.clone(), true))),
            [] => {}
            _ => return Err(()),
        }
        Ok(self.known_fallback(mac))
    }

    /// A known Mac's live row by a host, then its label or an alias.
    fn known_fallback(&self, mac: &KnownMac) -> Option<(String, bool)> {
        let hosts = valid_hosts(mac);
        if !hosts.is_empty()
            && let Some(pk) = self.live_by_host(&hosts)
        {
            return Some((pk, false));
        }
        let mut names: Vec<String> = mac.label.iter().map(Label::canonical).collect();
        names.extend(mac.aliases.iter().map(Label::canonical));
        names.iter().find_map(|name| {
            let hits = self.live_by_label(name);
            (hits.len() == 1).then(|| (hits[0].clone(), false))
        })
    }

    /// The machine key a label names, with no side effect: what [`Self::machine_for`] resolves
    /// to, for comparing a lower source's machines with the ones a higher source named.
    fn machine_key(&self, label: Label) -> Option<String> {
        let stored = label.canonical();
        let found = self.live_by_label(&stored);
        match found.len() {
            1 => return found.into_iter().next(),
            0 => {}
            _ => return None,
        }
        if let Some(mac) = self.known(&label) {
            return match self.known_found(mac) {
                Ok(Some((pk, _))) => Some(pk),
                Ok(None) => known_birth(mac, |pk| self.current[MACHINE.name].rows.contains_key(pk)),
                Err(()) => None,
            };
        }
        let rows = &self.current[MACHINE.name].rows;
        keys::planned_machine_pk(&stored, |pk| rows.contains_key(pk))
    }

    /// The row of a known Mac: by fleet node, then host, then its label or an alias among live
    /// rows; else, with births on, a new row keyed by its node (or its label). Its values come
    /// from the roster, so they carry the roster's coordinate, whichever sheet label led here.
    fn known_row(&mut self, mac: &KnownMac) -> Option<String> {
        let label = mac.label.as_ref().map(Label::canonical);
        let place = self
            .input
            .macs
            .iter()
            .position(|known| known == mac)
            .unwrap_or(0);
        let at = Coord {
            sheet: ROSTER,
            source: self.roster.0.clone(),
            row: place + 1,
            col: 0,
        };
        let Ok(found) = self.known_found(mac) else {
            self.findings.insert(Finding {
                kind: Kind::NodeAmbiguous,
                collection: Some(MACHINE.name),
                pk: None,
                column: Some("fleet_node".to_owned()),
                at: vec![at],
            });
            return None;
        };
        if let Some((pk, true)) = &found {
            self.findings.insert(Finding {
                kind: Kind::AdoptedByNode,
                collection: Some(MACHINE.name),
                pk: Some(pk.clone()),
                column: Some("fleet_node".to_owned()),
                at: Vec::new(),
            });
        }
        let found = found.map(|(pk, _)| pk);
        let pk = match found {
            Some(pk) => pk,
            None if self.input.births => {
                let rows = &self.current[MACHINE.name].rows;
                known_birth(mac, |pk| rows.contains_key(pk))?
            }
            None => {
                self.findings.insert(Finding {
                    kind: Kind::Unadopted,
                    collection: Some(MACHINE.name),
                    pk: None,
                    column: label.map(|l| format!("label {l}")),
                    at: Vec::new(),
                });
                return None;
            }
        };
        if let Some(label) = &label {
            self.want(&MACHINE, &pk, "label", json!(label), &at);
        }
        self.want(&MACHINE, &pk, "status", json!("active"), &at);
        if let Some(node) = valid_node(mac) {
            self.want(&MACHINE, &pk, "fleet_node", json!(node), &at);
        }
        let hosts = valid_hosts(mac);
        if !hosts.is_empty() {
            self.want(&MACHINE, &pk, "hosts", json!(hosts), &at);
        }
        for alias in &mac.aliases {
            self.element(&MACHINE, &pk, "aliases", &alias.canonical(), &at);
        }
        Some(pk)
    }

    fn known_macs(&mut self) {
        for mac in self.input.macs {
            if mac.label.is_some() || valid_node(mac).is_some() {
                self.known_row(mac);
            }
        }
    }

    fn finish(self) -> Result<Plan> {
        let Self {
            input,
            current,
            mut rows,
            findings,
            protected,
            roster,
            unsettled,
            unsettled_logins,
            address_unread,
            ..
        } = self;
        let mut plan = Plan {
            findings: findings.into_iter().collect(),
            ..Plan::default()
        };
        let collections = [&ADDRESS, &ACCOUNT, &MACHINE, &PREFERENCE, &MAILBOX, &PHONE];
        let mut touched_rows: BTreeSet<(usize, &'static str, String)> = BTreeSet::new();
        let mut written_rows = Vec::new();
        // The sources read this run: only what an earlier import of one of them wrote is ever
        // retracted, so a run that reads one sheet leaves the other sheet's cells.
        let read = ReadNow {
            kinds: input
                .sheets
                .iter()
                .map(|sheet| sheet.kind)
                .filter(|kind| !input.unread.contains(kind))
                .map(SheetKind::word)
                .chain((!input.macs.is_empty()).then_some("swarm-roster"))
                .collect(),
            files: input
                .sheets
                .iter()
                .filter(|sheet| sheet.kind.several())
                .collect(),
        };
        // The `+tag` rule's cells move only on a run that can see the owner's corrections.
        let logins_read = read.kinds.contains(SheetKind::Logins.word());
        let sources = &current[SOURCE.name];
        // The source this run read that a retraction of `field` is written under: the file of the
        // one that wrote it, by name for a kind with several files.
        let retract_source = |field: &Field| -> Option<(usize, String)> {
            let (wrote_pk, wrote) = source_row(field, sources)?;
            let kind = wrote.get("kind").and_then(Value::as_str)?;
            if kind == "swarm-roster" {
                return (!input.macs.is_empty()).then(|| (ROSTER, roster.0.clone()));
            }
            input
                .sheets
                .iter()
                .enumerate()
                .find(|(_, sheet)| sheet.wrote(wrote_pk, wrote))
                .map(|(n, sheet)| (n, sheet.source.clone()))
        };
        // How the source at `n` writes, and whether `field` is its own earlier import's. The
        // swarm roster only fills: what an import wrote otherwise is the owner's correction.
        let mode_of = |n: usize| {
            input
                .sheets
                .get(n)
                .map_or(Mode::FillOnly, |sheet| sheet.mode)
        };
        let own = |field: Option<&Field>, n: usize| -> bool {
            let Some((wrote_pk, wrote)) = field.and_then(|field| source_row(field, sources)) else {
                return false;
            };
            if n == ROSTER {
                return wrote.get("kind").and_then(Value::as_str) == Some("swarm-roster");
            }
            input
                .sheets
                .get(n)
                .is_some_and(|sheet| sheet.wrote(wrote_pk, wrote))
        };
        // What each source found another import holding otherwise, by key and column.
        let mut differs: BTreeMap<usize, BTreeSet<(&'static str, String, String)>> =
            BTreeMap::new();
        for (&name, table_rows) in &mut rows {
            let Some(collection) = collections.iter().copied().find(|c| c.name == name) else {
                continue;
            };
            for (pk, want) in table_rows {
                let (row_sheet, row_source) = want
                    .src
                    .as_ref()
                    .map_or((0, String::new()), |at| (at.sheet, at.source.clone()));
                if let Some(at) = &want.src {
                    touched_rows.insert((at.sheet, name, pk.clone()));
                }
                let current_row = current[name].rows.get(pk);
                let count = plan.counts.entry(name).or_default();
                count.rows += 1;
                if current_row.is_none() {
                    count.new_rows += 1;
                }
                // A preference an import ended and a source states again comes back; one the owner
                // ended stays ended.
                let revived = current_row.filter(|row| {
                    !row.live()
                        && name == PREFERENCE.name
                        && matches!(row.fields.get(crate::typed::LIFECYCLE),
                            Some(Field::Value(candidate)) if candidate.value.s.is_some())
                });
                // One another import ended that this source only fills stays ended, and nothing of it
                // is written: a finding, by key.
                if let Some(row) = revived {
                    let lifecycle = row.fields.get(crate::typed::LIFECYCLE);
                    let decided = decide(
                        lifecycle,
                        |held| *held == json!("live"),
                        mode_of(row_sheet),
                        own(lifecycle, row_sheet),
                    );
                    if decided != Decided::Write {
                        count.kept += 1;
                        differs.entry(row_sheet).or_default().insert((
                            name,
                            pk.clone(),
                            crate::typed::LIFECYCLE.to_owned(),
                        ));
                        plan.findings.push(Finding {
                            kind: Kind::Differs,
                            collection: Some(name),
                            pk: Some(pk.clone()),
                            column: Some(crate::typed::LIFECYCLE.to_owned()),
                            at: Vec::new(),
                        });
                        continue;
                    }
                }
                if let Some(row) = current_row
                    && !row.live()
                    && revived.is_none()
                {
                    count.kept += 1;
                    plan.findings.push(Finding {
                        kind: Kind::RetiredKept,
                        collection: Some(name),
                        pk: Some(pk.clone()),
                        column: None,
                        at: want.src.iter().cloned().collect(),
                    });
                    continue;
                }
                let mut changes = Vec::new();
                let mut step = |changes: &mut Vec<Change>,
                                column: &str,
                                elem: Option<&str>,
                                x: Value,
                                cell_sheet: usize,
                                cell_source: &str,
                                field: Option<&Field>,
                                plan: &mut Plan|
                 -> Result<()> {
                    let shown = match elem {
                        Some(elem) => format!("{column}.{elem}"),
                        None => column.to_owned(),
                    };
                    let mut decided = decide(
                        field,
                        |held| *held == x,
                        mode_of(cell_sheet),
                        own(field, cell_sheet),
                    );
                    // Provenance moves with a value: another source's coordinates are kept, quietly.
                    if decided == Decided::Differs && matches!(column, "src" | "notes_src") {
                        decided = Decided::Unchanged;
                    }
                    let count = plan.counts.entry(name).or_default();
                    let outcome = match decided {
                        Decided::Write => {
                            count.cells += 1;
                            touched_rows.insert((cell_sheet, name, pk.clone()));
                            let value =
                                CellValue::imported(collection.version, x.clone(), cell_source);
                            changes.push(cell::change(
                                collection,
                                CellKey {
                                    pk,
                                    col: column,
                                    elem,
                                },
                                &value,
                            )?);
                            Step::Write
                        }
                        Decided::Unchanged => {
                            count.unchanged += 1;
                            Step::Unchanged
                        }
                        Decided::Owner | Decided::Conflicted | Decided::Differs => {
                            count.kept += 1;
                            let kind = match decided {
                                Decided::Conflicted => Kind::Conflicted,
                                Decided::Differs => {
                                    differs.entry(cell_sheet).or_default().insert((
                                        name,
                                        pk.clone(),
                                        shown.clone(),
                                    ));
                                    Kind::Differs
                                }
                                _ => Kind::OwnerKept,
                            };
                            plan.findings.push(Finding {
                                kind,
                                collection: Some(name),
                                pk: Some(pk.clone()),
                                column: Some(shown.clone()),
                                at: Vec::new(),
                            });
                            Step::Kept
                        }
                    };
                    plan.planned.push(Planned {
                        collection: name,
                        pk: pk.clone(),
                        column: shown,
                        x,
                        step: outcome,
                    });
                    Ok(())
                };
                for (column, wanted) in &want.values {
                    step(
                        &mut changes,
                        column,
                        None,
                        wanted.x.clone(),
                        wanted.at.sheet,
                        &wanted.at.source,
                        current_row.and_then(|r| r.fields.get(*column)),
                        &mut plan,
                    )?;
                }
                for (&column, elems) in &want.elements {
                    for (elem, at) in elems {
                        let field = current_row
                            .and_then(|r| r.fields.get(column))
                            .and_then(|f| match f {
                                Field::Set(elements) => elements.get(elem),
                                _ => None,
                            });
                        step(
                            &mut changes,
                            column,
                            Some(elem),
                            Value::Bool(true),
                            at.sheet,
                            &at.source,
                            field,
                            &mut plan,
                        )?;
                    }
                }
                // Secret cells: sealed to this cell's coordinates, compared by opening, never shown.
                let guarded = (name == ADDRESS.name && protected.contains(pk))
                    || current_row.is_some_and(check::protected);
                // Merged once `step` is done with them.
                let mut secret_touched = Vec::new();
                let mut secret_differs = Vec::new();
                for (column, wanted) in &want.secrets {
                    let field = current_row.and_then(|r| r.fields.get(*column));
                    let count = plan.counts.entry(name).or_default();
                    if guarded {
                        count.kept += 1;
                        plan.findings.push(Finding {
                            kind: Kind::ProtectedSecret,
                            collection: Some(name),
                            pk: Some(pk.clone()),
                            column: Some((*column).to_owned()),
                            at: vec![wanted.at.clone()],
                        });
                        continue;
                    }
                    let holds = |held: &Value| {
                        held.as_str().is_some_and(|token| {
                            input.sealer.holds(name, pk, column, token, &wanted.value)
                        })
                    };
                    let decided = decide(
                        field,
                        holds,
                        mode_of(wanted.at.sheet),
                        own(field, wanted.at.sheet),
                    );
                    let outcome = match decided {
                        Decided::Write => {
                            let token = input.sealer.seal(name, pk, column, &wanted.value)?;
                            count.cells += 1;
                            secret_touched.push(wanted.at.sheet);
                            let value = CellValue::imported(
                                collection.version,
                                json!(token),
                                &wanted.at.source,
                            );
                            changes.push(cell::change(
                                collection,
                                CellKey {
                                    pk,
                                    col: column,
                                    elem: None,
                                },
                                &value,
                            )?);
                            Step::Write
                        }
                        Decided::Unchanged => {
                            count.unchanged += 1;
                            Step::Unchanged
                        }
                        Decided::Owner | Decided::Conflicted | Decided::Differs => {
                            count.kept += 1;
                            let kind = match decided {
                                Decided::Conflicted => Kind::Conflicted,
                                Decided::Differs => {
                                    secret_differs.push((wanted.at.sheet, (*column).to_owned()));
                                    Kind::Differs
                                }
                                _ => Kind::OwnerKept,
                            };
                            plan.findings.push(Finding {
                                kind,
                                collection: Some(name),
                                pk: Some(pk.clone()),
                                column: Some((*column).to_owned()),
                                at: Vec::new(),
                            });
                            Step::Kept
                        }
                    };
                    plan.planned.push(Planned {
                        collection: name,
                        pk: pk.clone(),
                        column: (*column).to_owned(),
                        x: json!(MASK),
                        step: outcome,
                    });
                }
                // What an earlier import of a kind read now wrote here and no sheet states now.
                if let Some(row) = current_row {
                    for (column, field) in &row.fields {
                        match field {
                            Field::Set(elements)
                                if RETRACT_SETS.contains(&(name, column.as_str())) =>
                            {
                                for (elem, field) in elements {
                                    if field.present()
                                        && retractable(field, sources, &read)
                                        && !want
                                            .elements
                                            .get(column.as_str())
                                            .is_some_and(|elems| elems.contains_key(elem))
                                        && let Some((ret_sheet, ret_source)) = retract_source(field)
                                    {
                                        step(
                                            &mut changes,
                                            column,
                                            Some(elem),
                                            Value::Bool(false),
                                            ret_sheet,
                                            &ret_source,
                                            Some(field),
                                            &mut plan,
                                        )?;
                                    }
                                }
                            }
                            _ if RETRACT_VALUES.contains(&(name, column.as_str()))
                                && field.value().is_some_and(|x| !x.is_null())
                                && retractable(field, sources, &read)
                                && (logins_read
                                    || name != ADDRESS.name
                                    || !matches!(column.as_str(), "alias_of" | "kind"))
                                && !want.values.contains_key(column.as_str()) =>
                            {
                                if let Some((ret_sheet, ret_source)) = retract_source(field) {
                                    step(
                                        &mut changes,
                                        column,
                                        None,
                                        Value::Null,
                                        ret_sheet,
                                        &ret_source,
                                        Some(field),
                                        &mut plan,
                                    )?;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                let fresh = current_row.is_none();
                if fresh {
                    step(
                        &mut changes,
                        crate::typed::LIFECYCLE,
                        None,
                        json!("live"),
                        row_sheet,
                        &row_source,
                        None,
                        &mut plan,
                    )?;
                }
                if let Some(row) = revived {
                    step(
                        &mut changes,
                        crate::typed::LIFECYCLE,
                        None,
                        json!("live"),
                        row_sheet,
                        &row_source,
                        row.fields.get(crate::typed::LIFECYCLE),
                        &mut plan,
                    )?;
                }
                // Provenance moves only with a value: a re-run of an edited file does not rewrite
                // every row's coordinates.
                if (fresh || !changes.is_empty()) && collection.column("src").is_some() {
                    if let Some(at) = &want.src {
                        step(
                            &mut changes,
                            "src",
                            None,
                            at.value(),
                            at.sheet,
                            &at.source,
                            current_row.and_then(|r| r.fields.get("src")),
                            &mut plan,
                        )?;
                    }
                    if !want.notes.is_empty() {
                        let mut notes = std::mem::take(&mut want.notes);
                        notes.sort_unstable();
                        notes.dedup();
                        if notes.len() > MAX_NOTES {
                            plan.findings.push(Finding {
                                kind: Kind::CoordinatesTruncated,
                                collection: Some(name),
                                pk: Some(pk.clone()),
                                column: Some("notes_src".to_owned()),
                                at: notes[MAX_NOTES..].to_vec(),
                            });
                            notes.truncate(MAX_NOTES);
                        }
                        let list: Vec<Value> = notes.iter().map(Coord::value).collect();
                        step(
                            &mut changes,
                            "notes_src",
                            None,
                            Value::Array(list),
                            row_sheet,
                            &row_source,
                            current_row.and_then(|r| r.fields.get("notes_src")),
                            &mut plan,
                        )?;
                    }
                }
                for n in secret_touched {
                    touched_rows.insert((n, name, pk.clone()));
                }
                for (n, column) in secret_differs {
                    differs
                        .entry(n)
                        .or_default()
                        .insert((name, pk.clone(), column));
                }
                if !changes.is_empty() {
                    written_rows.push(changes);
                }
            }
        }
        // A preference an earlier import wrote that no source states now ends, three-way: a row
        // with an owner's cell on it stays and is a finding, and an account with a cell this run
        // could not read keeps its rows, as does every row when a file or an address cell could
        // not be read, since what was missed may state any of them. An account or address no
        // source lists now is reported and left for the owner.
        let login_unsettled = |row: &cell::Row| {
            row.get("account")
                .and_then(Value::as_str)
                .and_then(|account| current[ACCOUNT.name].rows.get(account))
                .and_then(|account| account.get("login"))
                .and_then(Value::as_str)
                .is_some_and(|login| unsettled_logins.contains(login))
        };
        let can_end = input.unread.is_empty() && !address_unread;
        for (pk, row) in current[PREFERENCE.name].rows.iter().filter(|_| can_end) {
            let Some(field) = row.fields.get(crate::typed::LIFECYCLE) else {
                continue;
            };
            if rows
                .get(PREFERENCE.name)
                .is_some_and(|table| table.contains_key(pk))
                || !row.live()
                || !retractable(field, sources, &read)
                || login_unsettled(row)
                || row
                    .get("account")
                    .and_then(Value::as_str)
                    .is_some_and(|account| unsettled.contains(account))
            {
                continue;
            }
            let owner_cell = row.fields.values().any(|field| match field {
                Field::Value(candidate) => candidate.value.s.is_none(),
                Field::Conflict { .. } => true,
                Field::Set(_) => false,
            });
            if owner_cell {
                plan.findings.push(Finding {
                    kind: Kind::OwnerKept,
                    collection: Some(PREFERENCE.name),
                    pk: Some(pk.clone()),
                    column: Some(crate::typed::LIFECYCLE.to_owned()),
                    at: Vec::new(),
                });
                plan.counts.entry(PREFERENCE.name).or_default().kept += 1;
                continue;
            }
            let Some((ret_sheet, source)) = retract_source(field) else {
                continue;
            };
            let ended = json!("ended");
            if cell::import_step(Some(field), &ended) != ImportStep::Write {
                continue;
            }
            let value = CellValue::imported(PREFERENCE.version, ended, &source);
            let change = cell::change(
                &PREFERENCE,
                CellKey {
                    pk,
                    col: crate::typed::LIFECYCLE,
                    elem: None,
                },
                &value,
            )?;
            touched_rows.insert((ret_sheet, PREFERENCE.name, pk.clone()));
            plan.counts.entry(PREFERENCE.name).or_default().cells += 1;
            plan.planned.push(Planned {
                collection: PREFERENCE.name,
                pk: pk.clone(),
                column: crate::typed::LIFECYCLE.to_owned(),
                x: json!("ended"),
                step: Step::Write,
            });
            written_rows.push(vec![change]);
        }
        let mut sources_used: BTreeMap<usize, BTreeMap<&'static str, usize>> = BTreeMap::new();
        for n in differs.keys() {
            sources_used.entry(*n).or_default();
        }
        for (sheet_idx, name, _) in touched_rows {
            *sources_used
                .entry(sheet_idx)
                .or_default()
                .entry(name)
                .or_default() += 1;
        }
        for collection in [&ACCOUNT, &ADDRESS] {
            for (pk, row) in &current[collection.name].rows {
                if !rows
                    .get(collection.name)
                    .is_some_and(|table| table.contains_key(pk))
                    && row.live()
                    && row
                        .fields
                        .values()
                        .any(|field| retractable(field, sources, &read))
                {
                    plan.findings.push(Finding {
                        kind: Kind::NoLongerListed,
                        collection: Some(collection.name),
                        pk: Some(pk.clone()),
                        column: None,
                        at: Vec::new(),
                    });
                }
            }
        }
        let mut source_rows = Vec::new();
        let sources = input
            .sheets
            .iter()
            .enumerate()
            .map(|(n, sheet)| {
                (
                    n,
                    sheet.source.as_str(),
                    sheet.kind.word(),
                    sheet.name.as_str(),
                    sheet.sha256.as_str(),
                    sheet.rows.len(),
                )
            })
            .chain([(
                ROSTER,
                roster.0.as_str(),
                "swarm-roster",
                "swarm roster",
                roster.1.as_str(),
                input.macs.len(),
            )]);
        for (n, source, kind, file, sha256, count) in sources {
            let Some(used) = sources_used.get(&n) else {
                continue;
            };
            if current[SOURCE.name].rows.contains_key(source) {
                continue;
            }
            let counts: Map = used
                .iter()
                .map(|(c, k)| ((*c).to_owned(), json!(k)))
                .collect();
            let mut changes = Vec::new();
            let found: Vec<Value> = differs
                .get(&n)
                .into_iter()
                .flatten()
                .filter_map(|(collection, pk, shown)| {
                    let (column, elem) = match shown.split_once('.') {
                        Some((column, elem)) => (column, Some(elem)),
                        None => (shown.as_str(), None),
                    };
                    DIFFER_COLUMNS.contains(&column).then(|| {
                        json!({"collection": collection, "pk": pk, "column": column, "elem": elem})
                    })
                })
                .take(MAX_DIFFERS)
                .collect();
            let found = (!found.is_empty()).then_some(Value::Array(found));
            for (column, x) in [
                ("kind", json!(kind)),
                ("name", json!(file)),
                ("sha256", json!(sha256)),
                ("rows", json!(count)),
                ("at", json!(input.now_ms)),
                ("counts", Value::Object(counts)),
                ("differs", found.unwrap_or(Value::Null)),
                (crate::typed::LIFECYCLE, json!("live")),
            ]
            .into_iter()
            .filter(|(column, x)| *column != "differs" || !x.is_null())
            {
                let value = CellValue::imported(SOURCE.version, x, source);
                changes.push(cell::change(
                    &SOURCE,
                    CellKey {
                        pk: source,
                        col: column,
                        elem: None,
                    },
                    &value,
                )?);
            }
            let count = plan.counts.entry(SOURCE.name).or_default();
            count.rows += 1;
            count.new_rows += 1;
            count.cells += changes.len();
            source_rows.push(changes);
        }
        plan.rows = if source_rows.is_empty() {
            written_rows
        } else {
            source_rows.extend(written_rows);
            source_rows
        };
        plan.findings.sort_unstable();
        plan.findings.dedup();
        Ok(plan)
    }
}

fn valid_node(mac: &KnownMac) -> Option<String> {
    let node = mac.node.as_deref()?.trim();
    (node.len() == 32 && node.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| node.to_ascii_lowercase())
}

fn valid_hosts(mac: &KnownMac) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for host in &mac.hosts {
        let h = host.trim();
        if !h.is_empty() && guard::text(h, 64).is_ok() && !out.contains(&h) {
            out.push(h);
            if out.len() == 4 {
                break;
            }
        }
    }
    out
}

/// The key a known Mac with no live row is born under: the planned key
/// ([`keys::planned_machine_pk`]) of its fleet node (`n-<node>`), else of its label. Either
/// skips a key `held` by a row: a retired row the node or the label once named keeps its
/// history, and the Mac named again gets a live row of its own.
fn known_birth(mac: &KnownMac, held: impl Fn(&str) -> bool) -> Option<String> {
    match (valid_node(mac), &mac.label) {
        (Some(node), _) => keys::planned_machine_pk(&format!("n-{node}"), held),
        (None, Some(label)) => keys::planned_machine_pk(&label.canonical(), held),
        (None, None) => None,
    }
}

/// One label cell of a row.
#[derive(Clone, Copy)]
struct LabelCell<'a> {
    n: usize,
    sheet: &'a Sheet,
    row: usize,
    col: usize,
    address_pk: &'a str,
    /// The accounts this row makes.
    accounts: &'a [String],
    /// Whether this is the row's first machine cell.
    first: bool,
}

/// A note piece from a cell outside the notes columns: its column's name, then its text.
fn labelled(sheet: &Sheet, col: usize, text: &str) -> String {
    let header = sheet.headers.get(col).map_or("", |h| h.trim());
    let header = if header.is_empty() {
        "computer"
    } else {
        header
    };
    format!("{header}: {text}")
}

#[cfg(test)]
pub(crate) mod tests;
