// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The lab importer (docs/DESIGN-alab-lab-model-2026-11-01.md §9, step 6): the owner's two
//! account sheets and the machines this Mac knows of, planned as typed registry writes. Pure:
//! the caller hands in the parsed sheets, the registry's cells as they are, and the harness's
//! reading of a provider cell; nothing here does I/O.
//!
//! - **Three-way, never over the owner.** A cell is written only when it is absent or still
//!   holds what an earlier import wrote (`cell::import_step`); a cell the owner set, cleared or
//!   resolved since, or a conflicted one, is left as it is and becomes a finding. A row the
//!   owner retired is left whole. An unchanged cell costs nothing, so a re-run writes nothing.
//! - **What the sheets stop saying is retracted, three-way too.** A cell an earlier import of
//!   a sheet kind read now wrote, and that sheet no longer states, is withdrawn: a set element
//!   (a service, an alias) becomes `false`, a region, flow or mailbox link empty, and a seat the
//!   sheets no longer place is vacated; an account or address no longer listed is a finding,
//!   never removed. A sheet kind not read this run retracts nothing.
//! - **No free text crosses.** Notes, free-text cells, the lead's reasoning and losing values
//!   are never copied: a row's `notes_src` holds their source coordinates, and the local review
//!   is the only place their text is shown. A credential-shaped cell is never read at all.
//! - **The harness's reading of the vault sheet** ([`Vendors`]): Kimi Code's bare `1` is held
//!   (no account: its region decides which account a sign-in makes), Codex and Grok Build carry
//!   flow words, and a disqualifying note pauses Claude Code's account whatever vendor it
//!   names, and any other column's account only when it names that vendor or none.
//! - **One key per account**: the canonical account key (`keys::account_pk`) from the claims
//!   realm and the claims email, one provider word per realm.
//! - **Mailbox logins never cross**: a receiving mailbox becomes a row with a random key, a
//!   plain label, a service and a vault entry name; the address codes arrive at stays on this
//!   Mac ([`MailboxKeys`]), never in a key, a cell or a log line. An address never links to the
//!   mailbox it is itself (that link would name the mailbox's login), and a mailbox that is
//!   also an AI login is a finding. No phone row is made from free text.
//! - **Labels** go through the one parser (`machines::Reading`): `M109` is `m109`, `m27-2` is
//!   slot 2 on m27, `m3.2` slot 2 on m3, a bare `m3` slot 1. Every label resolves against the
//!   live rows' labels and aliases, then the Macs this Mac knows of (by label or alias, and
//!   their row by fleet node), before a placeholder is minted, so a re-import after a relabel
//!   finds the original row. Seats hold owner placements only: no allocator move is written.

use super::registry::{ACCOUNT, ADDRESS, APPLICATION, MACHINE, MAILBOX, SEAT, SOURCE};
use crate::{
    Error, Result,
    database::Change,
    machines::{Label, Reading},
    typed::{
        Collection,
        cell::{self, CellKey, CellValue, Cells, Field, ImportStep, Projection, Row},
        guard, keys,
    },
};
use aterm_codec::hex;
use aterm_digest::Sha256;
use aterm_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The two sheet layouts the importer reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SheetKind {
    /// The vault's account sheet (`ai-accounts.csv`): `m, email, computer, claude, Open AI,
    /// google, meta, kimi, x, notes`. It outranks the logins sheet.
    Vault,
    /// The owner's logins sheet (`ai-account-logins.csv`): `computer, AI login email, services,
    /// codes arrive in (my guess), why I guessed, your correction`.
    Logins,
}

impl SheetKind {
    /// The `source.kind` word.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Vault => "vault-sheet",
            Self::Logins => "logins-csv",
        }
    }

    /// The `address.origin` word.
    const fn origin(self) -> &'static str {
        match self {
            Self::Vault => "sheet",
            Self::Logins => "logins-csv",
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
}

impl Sheet {
    /// Parse `bytes` (at most the importer's CSV caps) as a sheet named `name`.
    ///
    /// # Errors
    /// The bytes are not a CSV within the caps, or it has no header.
    pub fn parse(kind: SheetKind, name: &str, bytes: &[u8]) -> Result<Self> {
        let mut records = crate::import::parse(bytes)?;
        if records.is_empty() {
            return Err(Error::new("the sheet has no header row"));
        }
        let headers = records.remove(0);
        let name = name.rsplit('/').next().unwrap_or(name);
        if guard::text(name, 128).is_err() {
            return Err(Error::new("the sheet's file name cannot be recorded"));
        }
        Ok(Self {
            kind,
            name: name.to_owned(),
            source: keys::source_pk(bytes),
            sha256: hex::encode_lower(&Sha256::digest(bytes)),
            headers,
            rows: records,
        })
    }

    /// The text at a 1-based coordinate (row 1 is the header), for the local review only.
    pub fn text(&self, at: &Coord) -> Option<&str> {
        let row = if at.row <= 1 {
            &self.headers
        } else {
            self.rows.get(at.row - 2)?
        };
        row.get(at.col.checked_sub(1)?).map(String::as_str)
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

/// Which mailbox key and label each receiving mailbox has, kept on the importing Mac only
/// (private, beside its vault state), keyed by a digest of the mailbox's address. A mailbox's
/// key is random, never a digest of its login, so this is how a re-import finds the row it made.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MailboxKeys {
    entries: BTreeMap<String, (String, String)>,
}

impl MailboxKeys {
    fn digest(address: &str) -> String {
        let mut input = b"aterm/alab/mailbox-key/1\0".to_vec();
        input.extend_from_slice(address.as_bytes());
        hex::encode_lower(&Sha256::digest(&input))
    }

    /// The stored form.
    pub fn to_json(&self) -> Value {
        let entries: Map = self
            .entries
            .iter()
            .map(|(digest, (pk, label))| (digest.clone(), json!({"pk": pk, "label": label})))
            .collect();
        json!({"format": "alab-mailbox-keys/1", "mailboxes": entries})
    }

    /// Read the stored form.
    ///
    /// # Errors
    /// It is not one.
    pub fn from_json(value: &Value) -> Result<Self> {
        let malformed = || Error::new("the mailbox key file is not one this build reads");
        if value.get("format").and_then(Value::as_str) != Some("alab-mailbox-keys/1") {
            return Err(malformed());
        }
        let mut entries = BTreeMap::new();
        for (digest, entry) in value
            .get("mailboxes")
            .and_then(Value::as_object)
            .ok_or_else(malformed)?
        {
            let pk = entry
                .get("pk")
                .and_then(Value::as_str)
                .ok_or_else(malformed)?;
            let label = entry
                .get("label")
                .and_then(Value::as_str)
                .ok_or_else(malformed)?;
            if !keys::is_digest_pk(pk, "mb") || guard::text(label, 32).is_err() {
                return Err(malformed());
            }
            entries.insert(digest.clone(), (pk.to_owned(), label.to_owned()));
        }
        Ok(Self { entries })
    }

    /// The mailbox keys it holds.
    pub fn pks(&self) -> BTreeSet<&str> {
        self.entries.values().map(|(pk, _)| pk.as_str()).collect()
    }

    /// Mailboxes it names.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether it names none.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
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
    /// Two addresses placed in one slot: the first (higher source, then earlier row) stays.
    SeatClash,
    /// The owner placed this address in another slot of that machine: the import leaves it.
    SeatMoved,
    /// A label names more than one live machine.
    LabelAmbiguous,
    /// A label spelled other than its stored form (`M109`, `m27-2`): read as the stored form.
    LabelRespelled,
    /// A slot outside 1 to 99, or no free slot left on the machine.
    NoSlot,
    /// A cell shaped like a credential: never read; its coordinates only.
    CredentialShaped,
    /// More coordinates than a row keeps: the rest are in this review only.
    CoordinatesTruncated,
    /// A known Mac matched an existing row by its fleet node.
    AdoptedByNode,
    /// More than one live row names a known Mac's fleet node: nothing of that Mac is written
    /// until the owner keeps one.
    NodeAmbiguous,
    /// A known Mac matched no row (`adopt` births none; step 7 does).
    Unadopted,
    /// Codes for this address arrive at the address itself: no mailbox link is written, since
    /// it would name that mailbox's login.
    SelfMailbox,
    /// A mailbox that receives codes for other rows is also listed as an AI login.
    MailboxIsLogin,
    /// An earlier import of a sheet read now made this row, and that sheet no longer lists it:
    /// left as it is for the owner to retire.
    NoLongerListed,
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
            Self::SeatClash => "seat-clash",
            Self::SeatMoved => "seat-moved",
            Self::LabelAmbiguous => "label-ambiguous",
            Self::LabelRespelled => "label-respelled",
            Self::NoSlot => "no-slot",
            Self::CredentialShaped => "credential-shaped",
            Self::CoordinatesTruncated => "coordinates-truncated",
            Self::AdoptedByNode => "adopted-by-node",
            Self::NodeAmbiguous => "node-ambiguous",
            Self::Unadopted => "unadopted",
            Self::SelfMailbox => "self-mailbox",
            Self::MailboxIsLogin => "mailbox-is-login",
            Self::NoLongerListed => "no-longer-listed",
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
    /// The mailbox keys, with any minted now.
    pub keys: MailboxKeys,
}

/// Everything one import reads.
pub struct Input<'a> {
    pub sheets: &'a [Sheet],
    pub macs: &'a [KnownMac],
    pub vendors: &'a dyn Vendors,
    pub keys: MailboxKeys,
    /// The importing Mac's clock, for the source rows' `at` (a claim, never an ordering input).
    pub now_ms: u64,
    /// Whether a label that resolves to no row births one; `adopt` births none.
    pub births: bool,
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
    Some(match word {
        "claude" | "claude code" | "claude-code" => (keys::ANTHROPIC, "claude-code"),
        "chatgpt" | "chat gpt" => (keys::OPENAI, "chatgpt"),
        "codex" | "openai" | "open ai" => (keys::OPENAI, "codex"),
        "google" | "gemini" => (keys::GOOGLE, "gemini-web"),
        "agy" | "antigravity" => (keys::GOOGLE, "agy"),
        "meta" | "meta ai" => (keys::META, "meta-web"),
        "muse" => (keys::META, "muse-code"),
        "kimi" | "kimi code" => (keys::KIMI, "kimi-code"),
        "grok" | "x" | "xai" => (keys::XAI, "grok"),
        _ => return None,
    })
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

/// The value columns an import writes only when a sheet states them, emptied when none does.
const RETRACT_VALUES: [(&str, &str); 4] = [
    ("account", "region"),
    ("account", "flow"),
    ("address", "mailbox_hint"),
    ("address", "mailbox"),
];

/// Whether `field` holds what an earlier import of a source kind in `kinds` wrote: one value
/// whose source names a source row of that kind. Only such a cell is ever retracted.
fn retractable(field: &Field, sources: &Projection, kinds: &BTreeSet<&str>) -> bool {
    let Field::Value(candidate) = field else {
        return false;
    };
    candidate
        .value
        .s
        .as_deref()
        .and_then(|source| sources.rows.get(source))
        .and_then(|row| row.get("kind"))
        .and_then(Value::as_str)
        .is_some_and(|kind| kinds.contains(kind))
}

/// One wanted register.
#[derive(Clone, Debug)]
struct Want {
    x: Value,
    at: Coord,
}

/// One wanted row.
#[derive(Clone, Debug, Default)]
struct RowWant {
    values: BTreeMap<&'static str, Want>,
    elements: BTreeMap<(&'static str, String), Coord>,
    /// The coordinate that created the row: its `src`, and its writes' source.
    src: Option<Coord>,
    notes: Vec<Coord>,
}

/// One wanted seat placement, before slots are settled.
#[derive(Clone, Debug)]
struct Placement {
    machine: String,
    slot: Option<u32>,
    address: String,
    role: &'static str,
    at: Coord,
}

struct Planner<'a> {
    input: &'a Input<'a>,
    current: BTreeMap<&'static str, Projection>,
    rows: BTreeMap<(&'static str, String), RowWant>,
    findings: BTreeSet<Finding>,
    keys: MailboxKeys,
    placements: Vec<Placement>,
    /// Addresses whose accounts and seats the vault sheet decided, with what it decided.
    decided: BTreeMap<String, (BTreeSet<&'static str>, Coord)>,
    seated: BTreeMap<String, BTreeSet<(String, Option<u32>)>>,
    minted_labels: BTreeSet<String>,
    /// The swarm roster as a source: its key and SHA-256, named by its canonical bytes.
    roster: (String, String),
    /// Every address either sheet lists as an AI login.
    logins: BTreeSet<String>,
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
    let current = [&ADDRESS, &ACCOUNT, &MACHINE, &SEAT, &MAILBOX, &SOURCE]
        .into_iter()
        .map(|collection| (collection.name, cell::project(collection, cells)))
        .collect();
    let mut planner = Planner {
        input,
        current,
        rows: BTreeMap::new(),
        findings: BTreeSet::new(),
        keys: input.keys.clone(),
        placements: Vec::new(),
        decided: BTreeMap::new(),
        seated: BTreeMap::new(),
        minted_labels: BTreeSet::new(),
        roster: roster_source(input.macs),
        logins: listed_logins(input.sheets),
    };
    let mut order: Vec<usize> = (0..input.sheets.len()).collect();
    order.sort_by_key(|&n| input.sheets[n].kind);
    for n in order {
        planner.sheet(n);
    }
    planner.known_macs();
    planner.seats();
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

    fn row(&mut self, collection: &'static Collection, pk: &str, at: &Coord) -> &mut RowWant {
        let row = self
            .rows
            .entry((collection.name, pk.to_owned()))
            .or_default();
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
        self.row(collection, pk, at)
            .elements
            .entry((col, elem.to_owned()))
            .or_insert_with(|| at.clone());
    }

    fn note(&mut self, collection: &'static Collection, pk: &str, at: &Coord) {
        let row = self.row(collection, pk, at);
        if !row.notes.contains(at) {
            row.notes.push(at.clone());
        }
    }

    fn sheet(&mut self, n: usize) {
        let sheet = &self.input.sheets[n];
        match sheet.kind {
            SheetKind::Vault => self.vault(n, sheet),
            SheetKind::Logins => self.logins(n, sheet),
        }
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

    /// Want the address row of `address` from `at`; returns its key.
    fn address(&mut self, address: &str, kind: SheetKind, at: &Coord) -> Option<String> {
        let pk = keys::address_pk(address).ok()?;
        let domain = address.split_once('@').map_or("", |(_, d)| d).to_owned();
        self.want(&ADDRESS, &pk, "addr", json!(address), at);
        self.want(&ADDRESS, &pk, "domain", json!(domain), at);
        // The first sheet to list an address (the vault sheet's precedence) names its origin; a
        // later sheet listing it too is no disagreement.
        let named = self
            .rows
            .get(&(ADDRESS.name, pk.clone()))
            .is_some_and(|row| row.values.contains_key("origin"));
        if !named {
            self.want(&ADDRESS, &pk, "origin", json!(kind.origin()), at);
        }
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
            let mut realms = BTreeSet::new();
            for &(col, provider) in &providers {
                let cell = cells.get(col).map_or("", String::as_str);
                let at = coord(sheet, n, row, col);
                if guard::credential_shaped(cell) {
                    continue;
                }
                let (terms, paused) = match self.input.vendors.mark(provider, cell, &notes) {
                    Mark::Absent => continue,
                    Mark::Unread => {
                        self.find(Kind::FreeText, Some(ACCOUNT.name), None, vec![at.clone()]);
                        self.note(&ADDRESS, &address_pk, &at);
                        continue;
                    }
                    Mark::Held => {
                        self.find(Kind::Held, Some(ACCOUNT.name), None, vec![at]);
                        continue;
                    }
                    Mark::Member(terms) => (terms, false),
                    Mark::Paused(terms) => (terms, true),
                };
                let Some((realm, _)) = provider_realm(provider, terms.region) else {
                    self.find(Kind::Held, Some(ACCOUNT.name), None, vec![at]);
                    continue;
                };
                realms.insert(realm);
                let Some(pk) = self.account(&address, &address_pk, realm, &at) else {
                    continue;
                };
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
                }
            }
            self.decided
                .entry(address_pk.clone())
                .and_modify(|(kept, _)| kept.extend(realms.iter().copied()))
                .or_insert((realms, email_at.clone()));
            for (k, &col) in computers.iter().enumerate() {
                let role = if k == 0 { "home" } else { "warm" };
                self.label_cell(n, sheet, row, cells, col, &address_pk, role);
            }
        }
    }

    /// Want the account of `address` at `realm`; returns its key.
    fn account(
        &mut self,
        address: &str,
        address_pk: &str,
        realm: &'static str,
        at: &Coord,
    ) -> Option<String> {
        let pk = keys::account_pk(realm, address).ok()?;
        let provider = keys::provider_of_realm(realm)?;
        self.want(&ACCOUNT, &pk, "provider", json!(provider), at);
        self.want(&ACCOUNT, &pk, "realm", json!(realm), at);
        self.want(&ACCOUNT, &pk, "login", json!(address_pk), at);
        Some(pk)
    }

    /// Read one label cell into seat placements for `address_pk`.
    #[allow(clippy::too_many_arguments)]
    fn label_cell(
        &mut self,
        n: usize,
        sheet: &Sheet,
        row: usize,
        cells: &[String],
        col: usize,
        address_pk: &str,
        role: &'static str,
    ) {
        let Some(text) = cells.get(col).map(|c| c.trim()).filter(|c| !c.is_empty()) else {
            return;
        };
        let at = coord(sheet, n, row, col);
        if guard::credential_shaped(text) {
            return;
        }
        let Ok(reading) = Reading::parse(text) else {
            self.find(Kind::FreeText, Some(SEAT.name), None, vec![at.clone()]);
            self.note(&ADDRESS, address_pk, &at);
            return;
        };
        let stored = match reading {
            Reading::Machine(label) => label.canonical(),
            Reading::Slot(label, slot) => format!("{}.{slot}", label.canonical()),
            Reading::Pair { home, warm } => format!("{}-{}", home.canonical(), warm.canonical()),
        };
        if stored != text {
            self.find(
                Kind::LabelRespelled,
                Some(SEAT.name),
                None,
                vec![at.clone()],
            );
        }
        let lower = sheet.kind == SheetKind::Logins && self.decided.contains_key(address_pk);
        for seat in reading.seats() {
            // A seat the vault sheet decides is only compared: no machine is born for it.
            let machine = if lower {
                self.machine_key(seat.machine)
            } else {
                self.machine_for(seat.machine, &at)
            };
            let Some(machine) = machine else {
                continue;
            };
            let role = if seat.home { role } else { "warm" };
            self.place(sheet.kind, machine, seat.slot, address_pk, role, &at);
        }
    }

    fn place(
        &mut self,
        kind: SheetKind,
        machine: String,
        slot: Option<u32>,
        address: &str,
        role: &'static str,
        at: &Coord,
    ) {
        if kind == SheetKind::Logins
            && let Some((_, vault_at)) = self.decided.get(address)
        {
            let vault_at = vault_at.clone();
            let seats = self.seated.get(address).cloned().unwrap_or_default();
            if !seats.contains(&(machine.clone(), slot)) {
                self.findings.insert(Finding {
                    kind: Kind::SourcesDisagree,
                    collection: Some(SEAT.name),
                    pk: None,
                    column: None,
                    at: vec![vault_at, at.clone()],
                });
            }
            return;
        }
        self.seated
            .entry(address.to_owned())
            .or_default()
            .insert((machine.clone(), slot));
        self.placements.push(Placement {
            machine,
            slot,
            address: address.to_owned(),
            role,
            at: at.clone(),
        });
    }

    fn logins(&mut self, n: usize, sheet: &Sheet) {
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
            if let Some(col) = services {
                self.services(n, sheet, row, cells, col, &address, &address_pk);
            }
            if let Some(col) = computer {
                self.label_cell(n, sheet, row, cells, col, &address_pk, "home");
            }
            for (col, column) in [(guess, "mailbox_hint"), (correction, "mailbox")] {
                let Some(col) = col else { continue };
                let text = cells.get(col).map_or("", |c| c.trim());
                if text.is_empty() || guard::credential_shaped(text) {
                    continue;
                }
                let at = coord(sheet, n, row, col);
                match keys::address(text) {
                    // A link to the mailbox an address is itself would name its login.
                    Ok(mailbox) if mailbox == address => {
                        self.find(
                            Kind::SelfMailbox,
                            Some(ADDRESS.name),
                            Some(&address_pk),
                            vec![at.clone()],
                        );
                    }
                    Ok(mailbox) => {
                        if self.logins.contains(&mailbox) {
                            self.find(
                                Kind::MailboxIsLogin,
                                Some(MAILBOX.name),
                                None,
                                vec![at.clone()],
                            );
                        }
                        let pk = self.mailbox(&mailbox, &at);
                        self.want(&ADDRESS, &address_pk, column, json!(pk), &at);
                    }
                    Err(_) => {
                        self.find(
                            Kind::FreeText,
                            Some(ADDRESS.name),
                            Some(&address_pk),
                            vec![at.clone()],
                        );
                        self.note(&ADDRESS, &address_pk, &at);
                    }
                }
            }
        }
    }

    /// The logins sheet's `services` cell: `+` separates services, `/` joins a service's words.
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
    ) {
        let text = cells.get(col).map_or("", |c| c.trim()).to_ascii_lowercase();
        if text.is_empty() || guard::credential_shaped(&text) {
            return;
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
                    if let Ok(pk) = keys::account_pk(realm, address) {
                        for (_, service) in &read {
                            self.element(&ACCOUNT, &pk, "services", service, &at);
                        }
                    }
                }
                None => {
                    let Some(pk) = self.account(address, address_pk, realm, &at) else {
                        continue;
                    };
                    self.want(&ACCOUNT, &pk, "status", json!("pending"), &at);
                    for (_, service) in &read {
                        self.element(&ACCOUNT, &pk, "services", service, &at);
                    }
                }
            }
        }
        if unread {
            self.find(Kind::FreeText, Some(ACCOUNT.name), None, vec![at.clone()]);
            self.note(&ADDRESS, address_pk, &at);
        }
    }

    /// The mailbox that receives at `address`: its key from this Mac's mailbox keys, else a fresh
    /// random key and label. The address itself goes nowhere in the registry.
    fn mailbox(&mut self, address: &str, at: &Coord) -> String {
        let digest = MailboxKeys::digest(address);
        let domain = address.split_once('@').map_or("", |(_, d)| d);
        let (service, code_path) = mailbox_service(domain);
        let (pk, label) = if let Some(entry) = self.keys.entries.get(&digest) {
            entry.clone()
        } else {
            let pk = keys::random_pk(keys::Random::Mailbox).unwrap_or_default();
            let mut used: BTreeSet<String> = self.minted_labels.clone();
            used.extend(self.keys.entries.values().map(|(_, label)| label.clone()));
            if let Some(projection) = self.current.get(MAILBOX.name) {
                used.extend(
                    projection
                        .rows
                        .values()
                        .filter_map(|row| row.get("label").and_then(Value::as_str))
                        .map(str::to_owned),
                );
            }
            let label = (1..)
                .map(|k| format!("{service}-{k}"))
                .find(|label| !used.contains(label))
                .unwrap_or_default();
            self.minted_labels.insert(label.clone());
            self.keys
                .entries
                .insert(digest, (pk.clone(), label.clone()));
            (pk, label)
        };
        self.want(&MAILBOX, &pk, "label", json!(label), at);
        self.want(&MAILBOX, &pk, "service", json!(service), at);
        self.want(&MAILBOX, &pk, "code_path", json!(code_path), at);
        self.want(
            &MAILBOX,
            &pk,
            "vault_ref",
            json!({"vault": APPLICATION, "entry": "ai-mailbox.toml", "item": format!("mailbox-{pk}")}),
            at,
        );
        pk
    }

    /// The live machine rows whose label or an alias is `label`.
    fn live_by_label(&self, label: &str) -> Vec<String> {
        let mut found: Vec<String> = self.current[MACHINE.name]
            .rows
            .iter()
            .filter(|(_, row)| row.live())
            .filter(|(_, row)| {
                row.get("label").and_then(Value::as_str) == Some(label)
                    || row
                        .fields
                        .get("aliases")
                        .is_some_and(|f| f.members().contains(label))
            })
            .map(|(pk, _)| pk.clone())
            .collect();
        found.sort();
        found
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

    fn live_by_host(&self, hosts: &[String]) -> Option<String> {
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
                                .any(|h| hosts.iter().any(|mine| mine == h))
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
    /// placeholder keyed by the label. `None` when the label is ambiguous or births are off and
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
        let pk = keys::machine_pk(&stored);
        self.want(&MACHINE, &pk, "label", json!(stored), at);
        self.want(&MACHINE, &pk, "status", json!("planned"), at);
        Some(pk)
    }

    /// The live row a known Mac already has: by its fleet node (`true`), then a host, then its
    /// label or an alias. `Err` when more than one live row names its node: then no row is the
    /// Mac's until the owner keeps one.
    fn known_found(&self, mac: &KnownMac) -> std::result::Result<Option<(String, bool)>, ()> {
        let by_node = mac
            .node
            .as_deref()
            .map(|node| self.live_by_node(node))
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
        if !mac.hosts.is_empty()
            && let Some(pk) = self.live_by_host(&mac.hosts)
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
    /// to, for comparing a lower source's seats with the ones a higher source placed.
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
                Ok(None) => known_birth(mac),
                Err(()) => None,
            };
        }
        Some(keys::machine_pk(&stored))
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
            None if self.input.births => known_birth(mac)?,
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
        if let Some(node) = &mac.node {
            self.want(&MACHINE, &pk, "fleet_node", json!(node), &at);
        }
        if !mac.hosts.is_empty() {
            let hosts: Vec<&String> = mac.hosts.iter().take(4).collect();
            self.want(&MACHINE, &pk, "hosts", json!(hosts), &at);
        }
        for alias in &mac.aliases {
            self.element(&MACHINE, &pk, "aliases", &alias.canonical(), &at);
        }
        Some(pk)
    }

    fn known_macs(&mut self) {
        for mac in self.input.macs {
            if mac.label.is_some() || mac.node.is_some() {
                self.known_row(mac);
            }
        }
    }

    /// Settle the placements into seats: explicit slots first, in source order; the first in a
    /// slot stays and a second address there is a clash. A placement with no slot (the second
    /// machine of a row, a two-Mac label) reuses the address's live seat on that machine, else
    /// takes the machine's first free slot. A placement whose address the owner seated in
    /// another slot of that machine is left.
    fn seats(&mut self) {
        let mut taken: BTreeMap<String, (String, Coord)> = BTreeMap::new();
        let mut placements = std::mem::take(&mut self.placements);
        placements.sort_by_key(|p| p.slot.is_none());
        for placement in placements {
            let current_seats: Vec<(String, Row)> = self.current[SEAT.name]
                .rows
                .iter()
                .filter(|(pk, row)| row.live() && pk.starts_with(&seat_prefix(&placement.machine)))
                .map(|(pk, row)| (pk.clone(), row.clone()))
                .collect();
            let holding = current_seats.iter().find(|(_, row)| {
                row.get("address").and_then(Value::as_str) == Some(placement.address.as_str())
            });
            let slot = match placement.slot {
                Some(slot) => slot,
                None => match holding {
                    Some((pk, _)) => slot_of(pk).unwrap_or(0),
                    None => (1..=99u32)
                        .find(|k| {
                            let pk =
                                keys::seat_pk(&placement.machine, *k as u8).unwrap_or_default();
                            // A vacant seat (no address) is free for the next placement.
                            !taken.contains_key(&pk)
                                && !current_seats.iter().any(|(held, row)| {
                                    *held == pk && row.get("address").is_some_and(|a| !a.is_null())
                                })
                        })
                        .unwrap_or(0),
                },
            };
            let Ok(pk) = u8::try_from(slot)
                .map_err(drop)
                .and_then(|slot| keys::seat_pk(&placement.machine, slot).map_err(drop))
            else {
                self.find(
                    Kind::NoSlot,
                    Some(SEAT.name),
                    None,
                    vec![placement.at.clone()],
                );
                continue;
            };
            if let Some((held, row)) = holding
                && *held != pk
                && row
                    .fields
                    .get("address")
                    .is_some_and(|f| matches!(f, Field::Value(c) if c.value.s.is_none()))
            {
                self.find(
                    Kind::SeatMoved,
                    Some(SEAT.name),
                    Some(held),
                    vec![placement.at.clone()],
                );
                continue;
            }
            match taken.get(&pk) {
                Some((address, _)) if *address == placement.address => continue,
                Some((_, first)) => {
                    self.findings.insert(Finding {
                        kind: Kind::SeatClash,
                        collection: Some(SEAT.name),
                        pk: Some(pk.clone()),
                        column: Some("address".to_owned()),
                        at: vec![first.clone(), placement.at.clone()],
                    });
                    continue;
                }
                None => {}
            }
            taken.insert(
                pk.clone(),
                (placement.address.clone(), placement.at.clone()),
            );
            let at = placement.at;
            self.want(&SEAT, &pk, "address", json!(placement.address), &at);
            self.want(&SEAT, &pk, "role", json!(placement.role), &at);
            self.want(&SEAT, &pk, "why", json!("import"), &at);
        }
    }

    fn finish(self) -> Result<Plan> {
        let Self {
            input,
            current,
            rows,
            findings,
            keys,
            roster,
            ..
        } = self;
        let mut plan = Plan {
            findings: findings.into_iter().collect(),
            keys,
            ..Plan::default()
        };
        let collections = [&ADDRESS, &ACCOUNT, &MACHINE, &SEAT, &MAILBOX];
        let mut sources_used: BTreeMap<usize, BTreeMap<&'static str, usize>> = BTreeMap::new();
        let mut written_rows = Vec::new();
        // The source kinds read this run: only what an earlier import of one of them wrote is
        // ever retracted, so a run that reads one sheet leaves the other sheet's cells.
        let kinds: BTreeSet<&str> = input
            .sheets
            .iter()
            .map(|sheet| sheet.kind.word())
            .chain((!input.macs.is_empty()).then_some("swarm-roster"))
            .collect();
        let sources = &current[SOURCE.name];
        let stated: BTreeSet<(&'static str, String)> = rows.keys().cloned().collect();
        for ((name, pk), want) in rows {
            let Some(collection) = collections.iter().copied().find(|c| c.name == name) else {
                continue;
            };
            let source = want
                .src
                .as_ref()
                .map(|at| at.source.clone())
                .unwrap_or_default();
            if let Some(at) = &want.src {
                *sources_used
                    .entry(at.sheet)
                    .or_default()
                    .entry(name)
                    .or_default() += 1;
            }
            let current_row = current[name].rows.get(&pk);
            let count = plan.counts.entry(name).or_default();
            count.rows += 1;
            if current_row.is_none() {
                count.new_rows += 1;
            }
            if let Some(row) = current_row
                && !row.live()
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
            let step = |changes: &mut Vec<Change>,
                        column: &str,
                        elem: Option<&str>,
                        x: Value,
                        field: Option<&Field>,
                        plan: &mut Plan|
             -> Result<()> {
                let shown = match elem {
                    Some(elem) => format!("{column}.{elem}"),
                    None => column.to_owned(),
                };
                let decided = cell::import_step(field, &x);
                let count = plan.counts.entry(name).or_default();
                let outcome = match decided {
                    ImportStep::Write => {
                        count.cells += 1;
                        let value = CellValue::imported(collection.version, x.clone(), &source);
                        changes.push(cell::change(
                            collection,
                            CellKey {
                                pk: &pk,
                                col: column,
                                elem,
                            },
                            &value,
                        )?);
                        Step::Write
                    }
                    ImportStep::Unchanged => {
                        count.unchanged += 1;
                        Step::Unchanged
                    }
                    ImportStep::Finding(_) => {
                        count.kept += 1;
                        let kind = if field.is_some_and(Field::conflicted) {
                            Kind::Conflicted
                        } else {
                            Kind::OwnerKept
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
                    current_row.and_then(|r| r.fields.get(*column)),
                    &mut plan,
                )?;
            }
            for (column, elem) in want.elements.keys() {
                let field = current_row
                    .and_then(|r| r.fields.get(*column))
                    .and_then(|f| match f {
                        Field::Set(elements) => elements.get(elem),
                        _ => None,
                    });
                step(
                    &mut changes,
                    column,
                    Some(elem),
                    Value::Bool(true),
                    field,
                    &mut plan,
                )?;
            }
            // What an earlier import of a kind read now wrote here and no sheet states now.
            if let Some(row) = current_row {
                for (column, field) in &row.fields {
                    match field {
                        Field::Set(elements) if RETRACT_SETS.contains(&(name, column.as_str())) => {
                            for (elem, field) in elements {
                                if field.present()
                                    && retractable(field, sources, &kinds)
                                    && !want
                                        .elements
                                        .keys()
                                        .any(|(c, e)| *c == column.as_str() && e == elem)
                                {
                                    step(
                                        &mut changes,
                                        column,
                                        Some(elem),
                                        Value::Bool(false),
                                        Some(field),
                                        &mut plan,
                                    )?;
                                }
                            }
                        }
                        _ if RETRACT_VALUES.contains(&(name, column.as_str()))
                            && field.value().is_some_and(|x| !x.is_null())
                            && retractable(field, sources, &kinds)
                            && !want.values.contains_key(column.as_str()) =>
                        {
                            step(
                                &mut changes,
                                column,
                                None,
                                Value::Null,
                                Some(field),
                                &mut plan,
                            )?;
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
                    None,
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
                        current_row.and_then(|r| r.fields.get("src")),
                        &mut plan,
                    )?;
                }
                if !want.notes.is_empty() {
                    let mut notes = want.notes.clone();
                    notes.sort();
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
                        current_row.and_then(|r| r.fields.get("notes_src")),
                        &mut plan,
                    )?;
                }
            }
            if !changes.is_empty() {
                written_rows.push(changes);
            }
        }
        // A seat an earlier import placed that no sheet places now is vacated; an account or
        // address no sheet lists now is reported and left for the owner.
        let kind_source = |kind: &str| {
            input
                .sheets
                .iter()
                .find(|sheet| sheet.kind.word() == kind)
                .map(|sheet| sheet.source.clone())
        };
        for (pk, row) in &current[SEAT.name].rows {
            let Some(field) = row.fields.get("address") else {
                continue;
            };
            if stated.contains(&(SEAT.name, pk.clone()))
                || !row.live()
                || field.value().is_none_or(Value::is_null)
                || !retractable(field, sources, &kinds)
            {
                continue;
            }
            let Field::Value(candidate) = field else {
                continue;
            };
            let Some(source) = candidate
                .value
                .s
                .as_deref()
                .and_then(|s| sources.rows.get(s))
                .and_then(|r| r.get("kind"))
                .and_then(Value::as_str)
                .and_then(kind_source)
            else {
                continue;
            };
            if cell::import_step(Some(field), &Value::Null) != ImportStep::Write {
                continue;
            }
            let value = CellValue::imported(SEAT.version, Value::Null, &source);
            let change = cell::change(
                &SEAT,
                CellKey {
                    pk,
                    col: "address",
                    elem: None,
                },
                &value,
            )?;
            plan.counts.entry(SEAT.name).or_default().cells += 1;
            plan.planned.push(Planned {
                collection: SEAT.name,
                pk: pk.clone(),
                column: "address".to_owned(),
                x: Value::Null,
                step: Step::Write,
            });
            written_rows.push(vec![change]);
        }
        for collection in [&ACCOUNT, &ADDRESS] {
            for (pk, row) in &current[collection.name].rows {
                if !stated.contains(&(collection.name, pk.clone()))
                    && row.live()
                    && row
                        .fields
                        .values()
                        .any(|field| retractable(field, sources, &kinds))
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
            for (column, x) in [
                ("kind", json!(kind)),
                ("name", json!(file)),
                ("sha256", json!(sha256)),
                ("rows", json!(count)),
                ("at", json!(input.now_ms)),
                ("counts", Value::Object(counts)),
                (crate::typed::LIFECYCLE, json!("live")),
            ] {
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
        plan.rows = source_rows.into_iter().chain(written_rows).collect();
        plan.findings.sort();
        plan.findings.dedup();
        Ok(plan)
    }
}

/// The key a known Mac with no row is born under: its fleet node, else its label.
fn known_birth(mac: &KnownMac) -> Option<String> {
    match (&mac.node, &mac.label) {
        (Some(node), _) => Some(keys::machine_pk(&format!("n-{node}"))),
        (None, Some(label)) => Some(keys::machine_pk(&label.canonical())),
        (None, None) => None,
    }
}

fn seat_prefix(machine: &str) -> String {
    format!("st-{}-", machine.strip_prefix("mc-").unwrap_or(machine))
}

fn slot_of(seat: &str) -> Option<u32> {
    seat.rsplit('-').next()?.parse().ok()
}

#[cfg(test)]
pub(crate) mod tests;
