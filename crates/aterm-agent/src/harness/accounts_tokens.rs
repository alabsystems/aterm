// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code setup-tokens on this Mac (`docs/DESIGN-claude-setup-token-2026-11-05.md` §4.6,
//! §5.1, slice S6): the account-to-token table, the token states, and the verb
//! `aterm harness accounts token <label|email> [--replace | --remove]`.
//!
//! - **The table.** `tokens.json` in Claude Code's accounts store, mode 0600, schema 1, written
//!   atomically under its own `tokens.lock` (never `accounts.lock`, which a relaunch holds
//!   through dispatch). One row per lab account key, value-free: neither the token nor any digest
//!   of it. Its own file, since an older build that rewrites `state.json` drops fields it does not
//!   know.
//! - **The states** ([`TokenState`]) are computed on each read from the vault's standing
//!   ([`claude_token::standing`]), the table's own facts and the clock, in §5.1's order.
//! - **The verb** ([`run`]) mints a token for one account in one step: for a person at a terminal
//!   only; under a per-account lock (never `signin.lock`); after a preflight (a seat in the
//!   vault's log, no route in the environment, macOS) and the refusal of a bare run while a token
//!   here is in use or on its way. Then it runs the capture ([`super::setup_token`]), writes both
//!   entries as a member write, records the row and journals `token-minted`. A busy vault after
//!   the mint never throws the token away: the write waits, bounded by the hang detector, then
//!   lands in the vault's waiting writes ("not yet in vault"); Ctrl-C lands it there at once.
//!
//! Nothing here prints, journals, logs or writes the token: it lives in the capture's zeroizing
//! buffer, then in the vault.

use super::accounts::{self, Cli, Roster, Row, Store};
use super::accounts_step::Step;
use super::cli::{AccountsHost, Env, TokenMode};
use super::journal::{self, TokenEvent};
use super::profile;
use super::setup_token::{self, Capture, Lifetime, Minted, Person, Program};
use super::wake_deadline::WakeDeadline;
use aterm_secrets::SecretBytes;
use aterm_secrets::claude_token::{self, ExpiryFrom, Key, Lookup, Meta, Mismatch, Standing, State};
use aterm_secrets::client::{MemberUnset, MemberWritten, OnBusy, Paths, WaitFor, WriteRoute};
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// The table's file in Claude Code's accounts store.
pub const TABLE_FILE: &str = "tokens.json";

/// The table's own lock, beside it.
pub const TABLE_LOCK: &str = "tokens.lock";

/// The folder beside them that holds each account's lock (`<ac key>.lock`) and its capture's
/// scratch root (`<ac key>/`).
pub const TOKENS_DIR: &str = "tokens";

/// The table's schema.
const SCHEMA: i64 = 1;

/// The most of `tokens.json` a read takes.
const MAX_TABLE: usize = 1 << 20;

/// How long one try of a write after the mint waits for a vault pass to let go, so Ctrl-C and the
/// hang detector are looked at this often ([`store_patiently`]).
const WRITE_TICK: Duration = Duration::from_secs(1);

/// Hang detector for the required preflight read behind a native vault writer.
const PREFLIGHT_HANG: Duration = Duration::from_secs(60);

/// Hang detector for the standing required before minting or replacing a token.
const REQUIRED_STANDING_HANG: Duration = Duration::from_secs(60);
/// Optional attribution or pre-removal metadata; the authoritative write owns its result.
const STANDING_LOOK: Duration = Duration::from_secs(2);

/// How long a removal waits for this Mac's vault state.
const REMOVE_WAIT: Duration = Duration::from_secs(60);

/// What the vault says of a token this Mac wrote (§5.1's `vault` column).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InVault {
    /// Signed, and a server took it.
    InVault,
    /// Signed into this Mac's log, and no server took it yet.
    SignedHere,
    /// Waiting in the vault's waiting writes for this Mac's vault pass: why (§3.5's words).
    Waiting(String),
    /// Another Mac's write of the account's token stands: that Mac.
    Overtaken(String),
    /// Removed from the vault (`--remove`).
    Removed,
}

impl InVault {
    /// The column as a person reads it.
    #[must_use]
    pub fn words(&self) -> String {
        match self {
            Self::InVault => "in vault".to_owned(),
            Self::SignedHere => "signed here, not yet in vault".to_owned(),
            Self::Waiting(why) => format!("waiting: {why}"),
            Self::Overtaken(by) => format!("overtaken by {by}"),
            Self::Removed => "removed".to_owned(),
        }
    }

    fn word(&self) -> &'static str {
        match self {
            Self::InVault => "in-vault",
            Self::SignedHere => "signed-here",
            Self::Waiting(_) => "waiting",
            Self::Overtaken(_) => "overtaken",
            Self::Removed => "removed",
        }
    }

    fn detail(&self) -> Option<&str> {
        match self {
            Self::Waiting(why) | Self::Overtaken(why) => Some(why),
            _ => None,
        }
    }

    fn parse(word: &str, detail: Option<&str>) -> Option<Self> {
        Some(match word {
            "in-vault" => Self::InVault,
            "signed-here" => Self::SignedHere,
            "waiting" => Self::Waiting(detail?.to_owned()),
            "overtaken" => Self::Overtaken(detail?.to_owned()),
            "removed" => Self::Removed,
            _ => return None,
        })
    }

    /// Whether a person reads it as "not yet in vault".
    #[must_use]
    pub const fn not_yet(&self) -> bool {
        matches!(self, Self::SignedHere | Self::Waiting(_))
    }
}

/// Why a member write waits, as §3.5 words it.
#[must_use]
pub fn waits_words(why: WaitFor) -> &'static str {
    match why {
        WaitFor::RotationDue => "it waits for the vault's new key after a removal",
        WaitFor::NewestUnopened => "it waits until this Mac holds the vault's newest key",
        WaitFor::KeysUnsettled => "it waits for the vault's newest keys to settle into one",
        WaitFor::PendingOfThisMac => "it replaces this Mac's earlier write, which still waited",
        WaitFor::LockDeadline => "this Mac's vault state was busy (a vault pass was running)",
    }
}

/// Which Macs use a token (§5.1's `used_on`): this Mac's served facts, which the harness's
/// attribution records, and the peers the last fleet read named. Never written to the vault.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsedOn {
    /// This Mac's first session on the token (unix seconds).
    pub first_served_at: Option<u64>,
    /// This Mac's latest session on it.
    pub last_served_at: Option<u64>,
    /// The peers whose survey rows named the key at the last fleet read.
    pub peers: Vec<String>,
    /// When that read was.
    pub peers_read_at: Option<u64>,
}

/// The last offer of a token for an account (§5.9): the state pair it was made for, and when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offered {
    /// The pair, with the date each half began (`none:2026-11-05/ending:2027-02-01`).
    pub pair: String,
    /// When it was made (unix seconds).
    pub at: u64,
}

/// One row of the account-to-token table, value-free (§5.1): the owner's fields (the account's
/// email and id from the session's own login record, the vault entry's name, when the token was
/// made and when it ends, the Mac that made it, which Macs use it, its signer) and this Mac's
/// facts about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenRow {
    /// The lab account key (`ac-<16 hex>`), the table's key.
    pub key: String,
    /// The account's email.
    pub email: String,
    /// The `c-<12 hex>` key of the login record the token was made for, where one named it.
    pub account_id: Option<String>,
    /// The roster row's label, where one names the account.
    pub label: Option<String>,
    /// The vault entry's name.
    pub entry: String,
    /// When the token was made (unix seconds).
    pub minted_at: Option<u64>,
    /// When it ends.
    pub expires_at: Option<u64>,
    /// Where that end came from: `printed` or `assumed`.
    pub expiry_from: Option<String>,
    /// The Mac that made it, for display.
    pub minted_on: Option<String>,
    /// The seat that signed it, read from the vault's log, never from the meta.
    pub signer: Option<String>,
    /// Which Macs use it.
    pub used_on: UsedOn,
    /// What the vault says of this Mac's write.
    pub vault: InVault,
    /// When the minting Mac verified it, where it did.
    pub verified: Option<u64>,
    /// On the minting Mac, what verification by windows found so far (§5.6).
    pub verification: Verification,
    /// This Mac's first refusal of it under the route.
    pub rejected_at: Option<u64>,
    /// When `--remove` removed it.
    pub removed_at: Option<u64>,
}

/// The account-to-token table: its rows by account key, and the last offer made for each
/// account (§5.9), kept beside the rows since an account with no token has no row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Table {
    /// Each account's row.
    pub rows: BTreeMap<String, TokenRow>,
    /// Each account's last offer, by account key.
    pub offers: BTreeMap<String, Offered>,
    /// When a surface last retried the post of each `signed here` row's write, by account key
    /// (unix seconds; §5.1: at most once a minute).
    pub posts: BTreeMap<String, u64>,
}

impl Table {
    /// The table as `tokens.json` holds it.
    #[must_use]
    pub fn to_json(&self) -> String {
        let rows: Vec<aterm_json::Value> = self
            .rows
            .values()
            .map(|row| {
                aterm_json::json!({
                    "key": row.key,
                    "email": row.email,
                    "account_id": row.account_id,
                    "label": row.label,
                    "entry": row.entry,
                    "minted_at": row.minted_at,
                    "expires_at": row.expires_at,
                    "expiry_from": row.expiry_from,
                    "minted_on": row.minted_on,
                    "signer": row.signer,
                    "used_on": {
                        "here": {
                            "first_served_at": row.used_on.first_served_at,
                            "last_served_at": row.used_on.last_served_at,
                        },
                        "peers": row.used_on.peers,
                        "peers_read_at": row.used_on.peers_read_at,
                    },
                    "vault": row.vault.word(),
                    "vault_detail": row.vault.detail(),
                    "verified": row.verified,
                    "verification": row.verification.to_json(),
                    "rejected_at": row.rejected_at,
                    "removed_at": row.removed_at,
                })
            })
            .collect();
        let offers: Vec<aterm_json::Value> = self
            .offers
            .iter()
            .map(|(key, offered)| {
                aterm_json::json!({"key": key, "pair": offered.pair, "at": offered.at})
            })
            .collect();
        let posts: Vec<aterm_json::Value> = self
            .posts
            .iter()
            .map(|(key, at)| aterm_json::json!({"key": key, "at": at}))
            .collect();
        aterm_json::to_string(&aterm_json::json!({
            "schema": SCHEMA,
            "rows": rows,
            "offers": offers,
            "posts": posts,
        }))
        .unwrap_or_default()
    }

    /// The table `bytes` holds, where it is one of schema 1. A row that does not read is left out.
    #[must_use]
    pub fn from_json(bytes: &[u8]) -> Option<Self> {
        let value: aterm_json::Value = aterm_json::from_slice(bytes).ok()?;
        if value.get("schema")?.as_i64()? != SCHEMA {
            return None;
        }
        let mut table = Self::default();
        for row in value.get("rows")?.as_array()? {
            if let Some(row) = row_of(row) {
                table.rows.insert(row.key.clone(), row);
            }
        }
        // A table an earlier slice of this build wrote has no offers: none was made.
        for offer in value
            .get("offers")
            .and_then(aterm_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(key) = offer
                .get("key")
                .and_then(aterm_json::Value::as_str)
                .filter(|key| journal::Key::new(key).is_some())
            else {
                continue;
            };
            let (Some(pair), Some(at)) = (
                offer.get("pair").and_then(aterm_json::Value::as_str),
                offer.get("at").and_then(aterm_json::Value::as_u64),
            ) else {
                continue;
            };
            table.offers.insert(
                key.to_owned(),
                Offered {
                    pair: pair.to_owned(),
                    at,
                },
            );
        }
        // A table written before the retries kept their times has none: none was tried.
        for post in value
            .get("posts")
            .and_then(aterm_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            if let (Some(key), Some(at)) = (
                post.get("key")
                    .and_then(aterm_json::Value::as_str)
                    .filter(|key| journal::Key::new(key).is_some()),
                post.get("at").and_then(aterm_json::Value::as_u64),
            ) {
                table.posts.insert(key.to_owned(), at);
            }
        }
        Some(table)
    }
}

fn row_of(value: &aterm_json::Value) -> Option<TokenRow> {
    let text = |of: &aterm_json::Value, key: &str| of.get(key)?.as_str().map(str::to_owned);
    let secs = |of: &aterm_json::Value, key: &str| of.get(key)?.as_u64();
    let used = value.get("used_on");
    let here = used.and_then(|used| used.get("here"));
    let used_on = UsedOn {
        first_served_at: here.and_then(|h| secs(h, "first_served_at")),
        last_served_at: here.and_then(|h| secs(h, "last_served_at")),
        peers: used
            .and_then(|used| used.get("peers")?.as_array())
            .map(|peers| {
                peers
                    .iter()
                    .filter_map(|p| p.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        peers_read_at: used.and_then(|used| secs(used, "peers_read_at")),
    };
    let key = text(value, "key")?;
    journal::Key::new(&key)?;
    Some(TokenRow {
        key,
        email: text(value, "email")?,
        account_id: text(value, "account_id"),
        label: text(value, "label"),
        entry: text(value, "entry")?,
        minted_at: secs(value, "minted_at"),
        expires_at: secs(value, "expires_at"),
        expiry_from: text(value, "expiry_from"),
        minted_on: text(value, "minted_on"),
        signer: text(value, "signer"),
        used_on,
        vault: InVault::parse(
            value.get("vault")?.as_str()?,
            value
                .get("vault_detail")
                .and_then(aterm_json::Value::as_str),
        )?,
        verified: secs(value, "verified"),
        verification: value
            .get("verification")
            .map(Verification::of_json)
            .unwrap_or_default(),
        rejected_at: secs(value, "rejected_at"),
        removed_at: secs(value, "removed_at"),
    })
}

/// The folder that holds the table: Claude Code's accounts store.
fn table_dir(store: &Store) -> PathBuf {
    store.shared_dir()
}

/// The table on this Mac: empty where there is none yet.
///
/// # Errors
/// The file is there and does not read, is not private, or is not a table.
pub fn read_table(store: &Store) -> io::Result<Table> {
    match aterm_uds::private_file::read_private(&table_dir(store), TABLE_FILE, MAX_TABLE) {
        Ok(bytes) => Table::from_json(&bytes)
            .ok_or_else(|| io::Error::other(format!("{TABLE_FILE} is not a table of schema 1"))),
        Err(aterm_uds::private_file::PrivateFileError::Missing) => Ok(Table::default()),
        Err(why) => Err(io::Error::other(format!("{TABLE_FILE}: {why:?}"))),
    }
}

/// Changes the table under `tokens.lock` (waiting for it as `accounts.lock`'s takers do) and
/// writes it whole, atomically, mode 0600.
///
/// # Errors
/// The lock stays held, or the table cannot be read or written.
pub fn update_table(store: &Store, change: impl FnOnce(&mut Table)) -> io::Result<()> {
    let dir = table_dir(store);
    store.ensure()?;
    let _lock = accounts::lock_at_within(&dir.join(TABLE_LOCK), &accounts::LOCK_LADDER)?
        .ok_or_else(|| io::Error::other(format!("{TABLE_LOCK} stayed held")))?;
    let mut table = read_table(store)?;
    change(&mut table);
    aterm_uds::private_file::write_private(&dir, TABLE_FILE, table.to_json().as_bytes())
}

/// RECORDS THIS MAC'S REFUSAL OF A TOKEN (§5.6): Claude Code drew its anchored lost-login wall
/// (`aterm_phase::WallKind::Auth`) on a session the wrapper served the token whose standing is
/// `standing`, at `now`. The row's `rejected_at` is set for the token its meta names (its
/// `minted_at`), the row made from that meta where this Mac kept none (a Mac that did not mint
/// it), so [`TokenState::of`] reads `rejected` until a new token replaces it; `token-rejected` is
/// journaled once, at the first refusal. It writes nothing else: no stored-login state of the row
/// (its `Auth`, its learned lifetime), since the stored login was not what was refused, and
/// nothing to the vault, since a refusal is this Mac's observation (each Mac's sessions meet the
/// same refusal at their next request). Returns whether this call recorded it first.
///
/// # Errors
/// The table could not be read or written under `tokens.lock`.
pub fn note_rejected(store: &Store, standing: &Standing, now: u64) -> io::Result<bool> {
    let Some(meta) = standing.meta.as_ref() else {
        return Ok(false);
    };
    let mut first = false;
    update_table(store, |table| {
        let row = row_following(table, standing, meta);
        if row.rejected_at.is_none() {
            row.rejected_at = Some(now);
            first = true;
        }
    })?;
    if first {
        note(TokenNote::Rejected, &standing.key, now);
    }
    Ok(first)
}

/// The table's row of the account whose token's standing is `standing` and whose meta is `meta`,
/// made from that meta where this Mac kept none (a Mac that did not mint it), and following the
/// vault's token where it is a newer one than the row's (another `minted_at`): this Mac's facts
/// about the older token (its served facts, its verification, its refusal) are of that token,
/// so they go with it.
fn row_following<'a>(table: &'a mut Table, standing: &Standing, meta: &Meta) -> &'a mut TokenRow {
    let key = standing.key.as_str().to_owned();
    let row = table.rows.entry(key.clone()).or_insert_with(|| TokenRow {
        key,
        email: meta.email.clone(),
        account_id: Some(meta.account_id.clone()).filter(|id| !id.is_empty()),
        label: None,
        entry: standing.key.entry(),
        minted_at: Some(meta.minted_at),
        expires_at: Some(meta.expires_at),
        expiry_from: Some(expiry_word(meta.expiry_from).to_owned()),
        minted_on: Some(meta.minted_on.clone()),
        signer: standing.signer.clone(),
        used_on: UsedOn::default(),
        vault: InVault::InVault,
        verified: meta.verified.as_ref().map(|verified| verified.at),
        verification: Verification::default(),
        rejected_at: None,
        removed_at: None,
    });
    if row.minted_at != Some(meta.minted_at) {
        row.minted_at = Some(meta.minted_at);
        row.expires_at = Some(meta.expires_at);
        row.expiry_from = Some(expiry_word(meta.expiry_from).to_owned());
        row.minted_on = Some(meta.minted_on.clone());
        row.signer.clone_from(&standing.signer);
        row.verified = meta.verified.as_ref().map(|verified| verified.at);
        row.verification = Verification::default();
        row.used_on.first_served_at = None;
        row.used_on.last_served_at = None;
        row.rejected_at = None;
    }
    row
}

/// The rate-limit windows a reading or a stored login's sample shows, value-free (§5.6): each
/// window's reset (unix seconds), where it is current.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Windows {
    /// The seven-day window's reset.
    pub week_reset: Option<i64>,
    /// The five-hour window's reset.
    pub session_reset: Option<i64>,
}

impl Windows {
    /// The windows `sample` shows still current at `now` ([`accounts::Sample::week`],
    /// [`accounts::Sample::session`]).
    #[must_use]
    pub fn current(sample: &accounts::Sample, now: i64) -> Self {
        Self {
            week_reset: sample.week(now).map(|(_, reset)| reset),
            session_reset: sample.session(now).map(|(_, reset)| reset),
        }
    }

    /// The windows a status-line reading carried ([`accounts::TapRead`]).
    #[must_use]
    pub fn of_reading(read: &accounts::TapRead) -> Self {
        Self {
            week_reset: read.seven_day.map(|w| w.reset),
            session_reset: read.five_hour.map(|w| w.reset),
        }
    }

    /// Whether `reading` shows these windows: the same week, and the same session where both
    /// name one current at `now`.
    #[must_use]
    pub fn shown_by(&self, reading: &Self, now: i64) -> bool {
        let current = |reset: Option<i64>| reset.filter(|reset| *reset > now);
        let Some(week) = current(self.week_reset) else {
            return false;
        };
        reading.week_reset == Some(week)
            && match (current(self.session_reset), reading.session_reset) {
                (Some(mine), Some(read)) => mine == read,
                _ => true,
            }
    }

    fn to_json(self) -> aterm_json::Value {
        aterm_json::json!({
            "seven_day_resets_at": self.week_reset,
            "five_hour_resets_at": self.session_reset,
        })
    }

    fn of_json(value: &aterm_json::Value) -> Self {
        Self {
            week_reset: value
                .get("seven_day_resets_at")
                .and_then(aterm_json::Value::as_i64),
            session_reset: value
                .get("five_hour_resets_at")
                .and_then(aterm_json::Value::as_i64),
        }
    }
}

/// What the comparison of a token session's first reading with its row's stored-login windows
/// found (§5.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The reading showed the basis's windows, which no other account here shares: the minting
    /// Mac writes `verified` (until it is written, each reading tries the write again).
    Equal,
    /// The reading's windows are not the basis's.
    Differ,
    /// They are, and another account's current windows on this Mac are too.
    Shared,
    /// There was no stored-login reading for the row in a current window.
    NoBasis,
}

impl Verdict {
    const fn word(self) -> &'static str {
        match self {
            Self::Equal => "equal",
            Self::Differ => "differ",
            Self::Shared => "shared",
            Self::NoBasis => "no-basis",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        [Self::Equal, Self::Differ, Self::Shared, Self::NoBasis]
            .into_iter()
            .find(|verdict| verdict.word() == word)
    }

    /// The comparison of `reading`, a token session's first reading carrying rate limits, with
    /// `basis`, the row's stored-login windows recorded before any token reading was folded,
    /// where `others` are the other accounts' current stored-login windows on this Mac, at `now`.
    #[must_use]
    pub fn of(basis: Option<&Windows>, reading: &Windows, others: &[Windows], now: i64) -> Self {
        let Some(basis) = basis.filter(|basis| basis.week_reset.is_some_and(|r| r > now)) else {
            return Self::NoBasis;
        };
        if !basis.shown_by(reading, now) {
            return Self::Differ;
        }
        if others.iter().any(|other| other.shown_by(reading, now)) {
            return Self::Shared;
        }
        Self::Equal
    }
}

/// On the minting Mac, what verification by windows has found of the row's token (§5.6): the
/// basis (the row's stored-login windows when the first session on the token started, recorded
/// before any token reading was folded), the first reading under the route that carried rate
/// limits, and what comparing them found. Empty on every other Mac.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Verification {
    /// The basis and when it was recorded (unix seconds).
    pub basis: Option<(u64, Windows)>,
    /// The first served reading that carried rate limits.
    pub first_reading: Option<Windows>,
    /// What the comparison found.
    pub verdict: Option<Verdict>,
}

impl Verification {
    fn to_json(&self) -> aterm_json::Value {
        aterm_json::json!({
            "basis_at": self.basis.map(|(at, _)| at),
            "basis": self.basis.map(|(_, windows)| windows.to_json()),
            "first_reading": self.first_reading.map(Windows::to_json),
            "verdict": self.verdict.map(Verdict::word),
        })
    }

    fn of_json(value: &aterm_json::Value) -> Self {
        let basis_at = value.get("basis_at").and_then(aterm_json::Value::as_u64);
        Self {
            basis: basis_at.zip(value.get("basis").map(Windows::of_json)),
            first_reading: value
                .get("first_reading")
                .filter(|v| !v.is_null())
                .map(Windows::of_json),
            verdict: value
                .get("verdict")
                .and_then(aterm_json::Value::as_str)
                .and_then(Verdict::parse),
        }
    }
}

/// A token session's served reading, as the harness's attribution hands it to [`note_served`]
/// before the reading is folded into the row (§5.6, §5.7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Served {
    /// The roster row the session's tap credits.
    pub label: String,
    /// The row's stored-login windows as the store holds them before this reading's fold.
    pub basis: Windows,
    /// The reading's windows.
    pub reading: Windows,
    /// The other accounts' current stored-login windows on this Mac.
    pub others: Vec<Windows>,
}

/// What [`note_served`] leaves for its caller to do once the reading's fold is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Due {
    /// Nothing.
    Nothing,
    /// The minting Mac writes `verified` with this evidence ([`verify_served`]).
    Verify(aterm_secrets::claude_token::Verified),
}

/// RECORDS A SERVED READING OF A TOKEN SESSION (§5.6, §5.7), under `tokens.lock`, BEFORE ITS
/// FOLD: this Mac's `first_served_at` (journaling `token-served` the first time) and
/// `last_served_at`, the row's label, and, on the Mac that minted a token not verified yet,
/// verification by windows:
///
/// - the first session's first reading records the basis, the row's stored-login windows
///   (`served.basis`), which its caller read before folding any token reading into the row, so
///   a token's reading is never compared with itself;
/// - the first reading that carries rate limits is compared with the basis ([`Verdict::of`]):
///   its seven-day reset must be the basis's, and its five-hour reset too where both are
///   current, and no other account's current windows here may show it;
/// - equal windows leave [`Due::Verify`], for the caller to write once the fold is: the write is
///   a vault write, never made under a store's lock.
///
/// A row-less account's token, or a reading from no roster row, is never compared: it stays
/// `unverified (no roster row)`. `standing` is the token's, read value-free before the fold.
///
/// # Errors
/// The table could not be read or written under `tokens.lock`.
pub fn note_served(
    store: &Store,
    standing: &Standing,
    served: &Served,
    now: u64,
) -> io::Result<Due> {
    let Some(meta) = standing.meta.as_ref() else {
        return Ok(Due::Nothing);
    };
    let at = i64::try_from(now).unwrap_or(i64::MAX);
    let (mut first, mut due) = (false, Due::Nothing);
    update_table(store, |table| {
        let row = row_following(table, standing, meta);
        row.label = Some(served.label.clone());
        if row.used_on.first_served_at.is_none() {
            row.used_on.first_served_at = Some(now);
            first = true;
        }
        row.used_on.last_served_at = Some(now);
        if !standing.minted_here || standing.verified() || row.verified.is_some() {
            return;
        }
        let check = &mut row.verification;
        if check.basis.is_none() && first {
            check.basis = Some((now, served.basis));
        }
        if served.reading.week_reset.is_none() {
            return;
        }
        let reading = *check.first_reading.get_or_insert(served.reading);
        let verdict = *check.verdict.get_or_insert_with(|| {
            Verdict::of(
                check.basis.as_ref().map(|(_, basis)| basis),
                &reading,
                &served.others,
                at,
            )
        });
        if verdict == Verdict::Equal {
            due = Due::Verify(aterm_secrets::claude_token::Verified {
                on: meta.minted_on.clone(),
                at: now,
                by: "windows".to_owned(),
                seven_day_resets_at: reading.week_reset.and_then(|r| u64::try_from(r).ok()),
                five_hour_resets_at: reading.session_reset.and_then(|r| u64::try_from(r).ok()),
            });
        }
    })?;
    if first {
        note(TokenNote::Served, &standing.key, now);
    }
    Ok(due)
}

/// How long verification's vault write waits for a vault pass to let go: it runs on a session's
/// attribution, so it never waits like a person's verb; the next reading tries again.
const VERIFY_WAIT: Duration = Duration::from_secs(2);

/// WRITES `verified` FOR THE ACCOUNT `key`'S TOKEN (§5.6) with `evidence`, the comparison
/// [`note_served`] found equal, through `vault` ([`claude_token::verify`]: the same token and its
/// meta with `verified` set, in one draft, only on the Mac that minted it), then records it in
/// this Mac's table and journals `token-verified`. A write the vault holds (busy) or refuses
/// records nothing: the next served reading tries again. Returns whether it was written.
///
/// # Errors
/// The table could not be written under `tokens.lock`.
pub fn verify_served(
    store: &Store,
    vault: &dyn TokenVault,
    key: &Key,
    evidence: aterm_secrets::claude_token::Verified,
    now: u64,
) -> io::Result<bool> {
    let written = vault.verify(key, evidence, OnBusy::Refuse(Instant::now() + VERIFY_WAIT));
    match written {
        Ok(MemberWritten::Posted | MemberWritten::SignedHere | MemberWritten::Waiting(_)) => {}
        Ok(
            MemberWritten::Overtaken(_)
            | MemberWritten::Refused(_)
            | MemberWritten::Busy
            | MemberWritten::NotSeated
            | MemberWritten::NoLog,
        )
        | Err(_) => return Ok(false),
    }
    update_table(store, |table| {
        if let Some(row) = table.rows.get_mut(key.as_str()) {
            row.verified = Some(now);
        }
    })?;
    note(TokenNote::Verified, key, now);
    Ok(true)
}

/// Why a token this Mac minted is not verified yet (§5.6, §5.1's `unverified (<why>)`); it
/// serves this Mac meanwhile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unverified {
    /// No served reading carrying rate limits was compared yet, or the row had no stored-login
    /// reading in a current window to compare it with.
    NoReadingYet,
    /// The token session's windows were not the row's stored login's.
    WindowsDiffer,
    /// They were, and another account's current windows on this Mac were too.
    WindowsShared,
    /// No roster row names the account, so no stored-login reading under a row compares.
    NoRosterRow,
}

impl Unverified {
    /// Why, from this Mac's row of the account (`None`: none kept).
    #[must_use]
    pub fn of(row: Option<&TokenRow>) -> Self {
        match row {
            Some(row) if row.label.is_none() => Self::NoRosterRow,
            Some(row) => match row.verification.verdict {
                Some(Verdict::Differ) => Self::WindowsDiffer,
                Some(Verdict::Shared) => Self::WindowsShared,
                Some(Verdict::Equal | Verdict::NoBasis) | None => Self::NoReadingYet,
            },
            None => Self::NoReadingYet,
        }
    }

    /// The reason as §5.1 words it.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::NoReadingYet => "no reading yet",
            Self::WindowsDiffer => "windows differ",
            Self::WindowsShared => "windows shared",
            Self::NoRosterRow => "no roster row",
        }
    }
}

/// A token's state on this Mac (§5.1), in the order its checks run: the first that applies is
/// the state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenState {
    /// The vault could not be read just now (busy, unsigned, failed or mid-apply).
    Unknown,
    /// No entry for the account.
    None,
    /// This Mac opens no epoch the inner seal names.
    Unreadable,
    /// A check failed.
    Mismatched(Mismatch),
    /// Its end has passed.
    Ended,
    /// This Mac saw Claude Code refuse it.
    Rejected,
    /// Not verified yet: it serves only the Mac that minted it.
    Unverified {
        /// This Mac minted it.
        minted_here: bool,
        /// On the minting Mac, why verification has not passed yet.
        why: Unverified,
        /// The Mac that did, as its meta names it.
        minted_on: String,
        /// The removed Macs it was handed to, beside it.
        exposed: Vec<String>,
    },
    /// A Mac the log now removes was handed it.
    Exposed(Vec<String>),
    /// 30 days or fewer until its end.
    Soon,
    /// In use, with more than 30 days left.
    Active,
}

impl TokenState {
    /// The state of the account whose vault lookup is `found` and whose table row is `row`, at
    /// `now` (unix seconds).
    #[must_use]
    pub fn of(found: &Lookup<Standing>, row: Option<&TokenRow>, now: u64) -> Self {
        let standing = match found {
            Lookup::Unknown => return Self::Unknown,
            Lookup::Absent => return Self::None,
            Lookup::Found(standing) => standing,
        };
        let state = standing.state(now);
        match state {
            State::Unreadable => return Self::Unreadable,
            State::Mismatched(why) => return Self::Mismatched(why),
            State::Ended => return Self::Ended,
            _ => {}
        }
        let minted_at = standing.meta.as_ref().map(|meta| meta.minted_at);
        if row.is_some_and(|row| row.rejected_at.is_some() && row.minted_at == minted_at) {
            return Self::Rejected;
        }
        match state {
            State::Unverified {
                served_here,
                exposed,
            } => Self::Unverified {
                minted_here: served_here,
                why: Unverified::of(row),
                minted_on: standing
                    .meta
                    .as_ref()
                    .map(|meta| meta.minted_on.clone())
                    .unwrap_or_default(),
                exposed,
            },
            State::Exposed(macs) => Self::Exposed(macs),
            State::Soon => Self::Soon,
            State::Active => Self::Active,
            State::Unreadable | State::Mismatched(_) | State::Ended => Self::Unknown,
        }
    }

    /// The state as a person reads it, value-free.
    #[must_use]
    pub fn words(&self) -> String {
        match self {
            Self::Unknown => "unknown (the vault could not be read just now)".to_owned(),
            Self::None => "none".to_owned(),
            Self::Unreadable => "unreadable here".to_owned(),
            Self::Mismatched(Mismatch::Draft) => "mismatched (draft)".to_owned(),
            Self::Mismatched(Mismatch::Value) => "mismatched (value)".to_owned(),
            Self::Ended => "ended".to_owned(),
            Self::Rejected => "rejected".to_owned(),
            Self::Unverified {
                minted_here,
                why,
                minted_on,
                exposed,
            } => {
                let mut words = if *minted_here {
                    format!("unverified ({})", why.words())
                } else {
                    format!("unverified (made on {minted_on}; serves that Mac until verified)")
                };
                if !exposed.is_empty() {
                    words.push_str(&format!(", exposed (removed {})", exposed.join(", ")));
                }
                words
            }
            Self::Exposed(macs) => format!("exposed (removed {})", macs.join(", ")),
            Self::Soon => "soon".to_owned(),
            Self::Active => "active".to_owned(),
        }
    }

    /// The state as one word with no blank, as a survey row's `token=` and a status line's
    /// `token=` carry it, and as the offer's pair names it (§5.9, §5.10).
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::None => "none",
            Self::Unreadable => "unreadable",
            Self::Mismatched(_) => "mismatched",
            Self::Ended => "ended",
            Self::Rejected => "rejected",
            Self::Unverified { .. } => "unverified",
            Self::Exposed(_) => "exposed",
            Self::Soon => "soon",
            Self::Active => "active",
        }
    }

    /// Whether sessions on this Mac start on the token (§5.1's *in use*): `active`, `soon` or
    /// `exposed` on every Mac, `unverified` on its minting Mac only.
    #[must_use]
    pub const fn in_use(&self) -> bool {
        matches!(
            self,
            Self::Active
                | Self::Soon
                | Self::Exposed(_)
                | Self::Unverified {
                    minted_here: true,
                    ..
                }
        )
    }

    /// Why a bare run of the verb (no `--replace`) is refused in this state, where it is (§4.6):
    /// while this Mac reads the account's token as `active`, `soon`, `exposed` or `unverified`,
    /// wherever it was minted, or while a token made here is not yet in vault (`pending`), so a
    /// second Mac never replaces a token that awaits verification on the Mac that made it, and a
    /// re-run never replaces a write still on its way. `expires_at` dates the words.
    #[must_use]
    pub fn bare_refusal(
        &self,
        email: &str,
        expires_at: Option<u64>,
        pending: bool,
    ) -> Option<String> {
        let until = expires_at.map_or_else(String::new, |at| format!(" until {}", date(at)));
        let replace = "`--replace` makes a new one in its place (the old one stays valid until it \
                       ends or you revoke it at claude.ai)";
        if pending {
            return Some(format!(
                "a token for {email} made here is not yet in vault: this Mac's vault pass signs \
                 it; {replace}"
            ));
        }
        let state = match self {
            Self::Active => "active".to_owned(),
            Self::Soon => "active (it ends within 30 days)".to_owned(),
            Self::Exposed(macs) => format!("exposed (removed {}) and valid", macs.join(", ")),
            Self::Unverified {
                minted_here: true, ..
            } => "unverified, serving this Mac".to_owned(),
            Self::Unverified { minted_on, .. } => {
                format!("made on {minted_on}, serving that Mac until it is verified, and valid")
            }
            _ => return None,
        };
        Some(format!("a token for {email} is {state}{until}; {replace}"))
    }

    /// The step a surface offers for the account `label` (its row's label, or its email) in this
    /// state (§5.9), where it offers one: for an account with no token in use here (`none`,
    /// `ended`, `rejected`, `mismatched`, `unreadable here`) a bare run, and for a token that is
    /// `soon` or `exposed` a `--replace`, so the step always passes the verb's refusal of a bare
    /// run. None for `unknown`, `active`, an `unverified` token wherever it was minted, or while a
    /// token made here is not yet in vault (`pending`).
    #[must_use]
    pub fn offer(&self, label: &str, pending: bool) -> Option<Step> {
        if pending {
            return None;
        }
        let replace = match self {
            Self::None | Self::Ended | Self::Rejected | Self::Mismatched(_) | Self::Unreadable => {
                false
            }
            Self::Soon | Self::Exposed(_) => true,
            Self::Unknown | Self::Active | Self::Unverified { .. } => return None,
        };
        Some(Step::Token {
            label: label.to_owned(),
            replace,
        })
    }
}

/// `at` (unix seconds) as a UTC date, `YYYY-MM-DD`.
#[must_use]
pub fn date(at: u64) -> String {
    let (y, m, d) = aterm_types::rfc3339::civil_from_days(i64::try_from(at / 86_400).unwrap_or(0));
    format!("{y:04}-{m:02}-{d:02}")
}

/// What the verb's vault is to it: the typed calls of [`claude_token`] on this Mac's vault
/// ([`Vault`]), or a test's stand-in.
pub trait TokenVault {
    /// How this Mac writes its vault, observed until `by`.
    ///
    /// # Errors
    /// The configuration, the records or this Mac's device key cannot be read.
    fn route(&self, by: Instant) -> Result<WriteRoute, String>;
    /// The account `key`'s value-free standing through [`claude_token::read`], observed until `by`.
    ///
    /// # Errors
    /// As that call.
    fn standing(&self, key: &Key, by: Instant) -> Result<Lookup<Standing>, String>;
    /// Every token the vault holds, with its standing, in one read ([`claude_token::listed`]):
    /// what a surface that shows several accounts reads.
    ///
    /// # Errors
    /// As that call.
    fn listed(&self) -> Result<Lookup<Vec<Standing>>, String>;
    /// Whether a write of this Mac's of the token still waits ([`claude_token::waiting_here`]).
    ///
    /// # Errors
    /// As that call.
    fn waiting_here(&self, key: &Key) -> Result<bool, String>;
    /// This Mac's seat's label ([`claude_token::seat_here`]).
    ///
    /// # Errors
    /// As that call.
    fn seat_here(&self) -> Result<Option<String>, String>;
    /// Write both entries ([`claude_token::write`]).
    ///
    /// # Errors
    /// As that call.
    fn write(
        &self,
        key: &Key,
        token: &SecretBytes,
        meta: &Meta,
        on_busy: OnBusy,
    ) -> Result<MemberWritten, String>;
    /// Remove both entries ([`claude_token::remove`]).
    ///
    /// # Errors
    /// As that call.
    fn remove(&self, key: &Key, by: Instant) -> Result<MemberUnset, String>;
    /// Write the token's meta again with `verified` set ([`claude_token::verify`]).
    ///
    /// # Errors
    /// As that call.
    fn verify(
        &self,
        key: &Key,
        evidence: aterm_secrets::claude_token::Verified,
        on_busy: OnBusy,
    ) -> Result<MemberWritten, String>;
    /// Post this Mac's records again to every server it reaches, as a write's own post does
    /// ([`aterm_secrets::client::post_records_to_servers`], idempotent): whether a server took
    /// them. A vault that reaches no server takes nothing.
    ///
    /// # Errors
    /// As that call.
    fn post(&self) -> Result<bool, String> {
        Ok(false)
    }
}

/// The vault at `Paths`: this Mac's, in a run; a scratch one in a test.
pub struct Vault(pub Paths);

impl TokenVault for Vault {
    fn route(&self, by: Instant) -> Result<WriteRoute, String> {
        aterm_secrets::client::write_route(&self.0, by).map_err(|e| e.to_string())
    }
    fn standing(&self, key: &Key, by: Instant) -> Result<Lookup<Standing>, String> {
        Ok(
            match claude_token::read(&self.0, key, by.saturating_duration_since(Instant::now()))
                .map_err(|error| error.to_string())?
            {
                Lookup::Found(read) => Lookup::Found(read.standing),
                Lookup::Absent => Lookup::Absent,
                Lookup::Unknown => Lookup::Unknown,
            },
        )
    }
    fn listed(&self) -> Result<Lookup<Vec<Standing>>, String> {
        claude_token::listed(&self.0).map_err(|e| e.to_string())
    }
    fn waiting_here(&self, key: &Key) -> Result<bool, String> {
        claude_token::waiting_here(&self.0, key).map_err(|e| e.to_string())
    }
    fn seat_here(&self) -> Result<Option<String>, String> {
        claude_token::seat_here(&self.0).map_err(|e| e.to_string())
    }
    fn write(
        &self,
        key: &Key,
        token: &SecretBytes,
        meta: &Meta,
        on_busy: OnBusy,
    ) -> Result<MemberWritten, String> {
        claude_token::write(&self.0, key, token, meta, on_busy).map_err(|e| e.to_string())
    }
    fn remove(&self, key: &Key, by: Instant) -> Result<MemberUnset, String> {
        claude_token::remove(&self.0, key, by).map_err(|e| e.to_string())
    }
    fn verify(
        &self,
        key: &Key,
        evidence: aterm_secrets::claude_token::Verified,
        on_busy: OnBusy,
    ) -> Result<MemberWritten, String> {
        claude_token::verify(&self.0, key, evidence, on_busy).map_err(|e| e.to_string())
    }
    fn post(&self) -> Result<bool, String> {
        aterm_secrets::client::post_records_to_servers(&self.0)
            .map(|posted| posted > 0)
            .map_err(|e| e.to_string())
    }
}

/// This Mac's vault at `paths`, where one is configured: `None` where none is, so a surface says
/// nothing of tokens there (a configuration that cannot be read keeps its vault, which then reads
/// `unknown`).
#[must_use]
pub fn configured_vault(paths: Paths) -> Option<Vault> {
    (!matches!(aterm_secrets::client::config(&paths), Ok(None))).then_some(Vault(paths))
}

/// An account a session on this Mac is logged into, as its own login record names it (§5.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionLogin {
    /// The record's email.
    pub email: String,
    /// The `c-<12 hex>` key of its account, where the record names one.
    pub account_id: Option<String>,
    /// It is the machine's default login's record, which every session on no row runs on.
    pub default: bool,
}

/// The account the verb makes a token for.
struct Account {
    /// Its email: the row's claims email, else the login record's.
    email: String,
    /// The row's label, where one names it.
    label: Option<String>,
    /// The `c-<12 hex>` key of its login record here, where one names it.
    account_id: Option<String>,
    /// The lab key.
    key: Key,
    /// The Mac that holds the row, where another does (`held_by`).
    holder: Option<String>,
}

impl Account {
    /// How the verb names it: `<label> (<email>)`, or the email.
    fn named(&self) -> String {
        match &self.label {
            Some(label) => format!("{label} ({})", self.email),
            None => self.email.clone(),
        }
    }

    /// What the outcome adds for a row another Mac holds: launches here refuse it.
    fn held_note(&self) -> String {
        match (&self.holder, &self.label) {
            (Some(holder), Some(label)) => {
                format!("; the token serves {label} on {holder}, the Mac that holds it")
            }
            _ => String::new(),
        }
    }
}

/// The account `name` names (§4.6): a label names a Claude row with an email; an email names an
/// account a session on this Mac is logged into, or one a row names. A row another Mac holds is
/// accepted, since it is named here.
fn account_of(env: &Env, name: &str, logins: &[SessionLogin]) -> Result<Account, String> {
    let roster = match env.accounts_roster_for(Cli::Claude) {
        Some(path) if path.exists() => Some(
            Roster::load_for(Cli::Claude, &path)
                .map_err(|why| format!("the roster at {} is refused: {why}", path.display()))?,
        ),
        _ => None,
    };
    let rows: &[Row] = roster.as_ref().map_or(&[], |roster| roster.rows.as_slice());
    let (row, email) = if name.contains('@') {
        let row = rows.iter().find(|row| row.matches_email(name));
        let logged_in = logins
            .iter()
            .find(|login| accounts::email_matches(&login.email, name));
        if row.is_none() && logged_in.is_none() {
            return Err(format!(
                "no session on this Mac is logged into {name}, and no row names it: a token is \
                 made for an account a session here is on, or for a row by its label"
            ));
        }
        let email = row
            .and_then(Row::expected_identity_email)
            .or(logged_in.map(|login| login.email.as_str()))
            .unwrap_or(name)
            .to_owned();
        (row, email)
    } else {
        let row = rows.iter().find(|row| row.label == name).ok_or_else(|| {
            format!(
                "no Claude Code row is labelled {name} (`{}` lists them)",
                Step::Report(Cli::Claude)
            )
        })?;
        let email = row.expected_identity_email().ok_or_else(|| {
            format!(
                "{name} names no email, and a token is made for an account by its email \
                 (`aterm harness accounts adopt-default {name} --email <e>` names it)"
            )
        })?;
        (Some(row), email.to_owned())
    };
    let key = row
        .and_then(|row| journal::account_key(Cli::Claude, row))
        .or_else(|| journal::claude_email_key(&email))
        .and_then(|key| Key::parse(key.as_str()))
        .ok_or_else(|| {
            "this aterm cannot key the account (no lab journal is installed), so it names no \
             vault entry for it"
                .to_owned()
        })?;
    let account_id = logins
        .iter()
        .find(|login| accounts::email_matches(&login.email, &email))
        .and_then(|login| login.account_id.clone())
        .or_else(|| {
            let home = env.home.as_deref()?;
            profile::account_for_login(&[], home, row?.dir.as_deref(), &email)
                .map(|key| key.to_string())
        });
    Ok(Account {
        email,
        label: row.map(|row| row.label.clone()),
        account_id,
        key,
        holder: row.and_then(|row| row.held_by.clone()),
    })
}

/// The preflight's words for a write route that cannot store a token (§4.6).
fn route_refusal(route: WriteRoute) -> Option<&'static str> {
    match route {
        WriteRoute::Seated => None,
        WriteRoute::NotSeated => Some(
            "this Mac is not seated in the vault's log yet, so it can store no token \
             (`aterm secrets status` says vault writes:)",
        ),
        WriteRoute::NoLog => {
            Some("this Mac's vault has no membership log, and a token lives only in a vault's log")
        }
        WriteRoute::Busy => {
            Some("this Mac's vault state is busy (a vault pass is running); try again")
        }
    }
}

/// `aterm harness accounts token <name> [--replace | --remove]` (§4.6): the module's summary
/// says what it does. Every refusal comes before any spawn. Exit 0 on a token stored, waiting or
/// removed; 130 at Ctrl-C during the sign-in; 1 on every refusal.
pub fn run(
    env: &Env,
    host: &dyn AccountsHost,
    name: &str,
    mode: TokenMode,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    run_with_clock(env, host, name, mode, out, err, Instant::now)
}

fn run_with_clock(
    env: &Env,
    host: &dyn AccountsHost,
    name: &str,
    mode: TokenMode,
    out: &mut dyn Write,
    err: &mut dyn Write,
    now: impl FnOnce() -> Instant,
) -> ExitCode {
    let refuse = |err: &mut dyn Write, why: &str| {
        let _ = writeln!(err, "aterm harness accounts token: {why}");
        ExitCode::from(1)
    };
    if !super::cli::person_at_terminal(host) {
        return refuse(
            err,
            "a token is made only for a person at this Mac's terminal (an agent's shell has \
             none): run it in a terminal of your own",
        );
    }
    let logins = host.session_logins(env);
    let account = match account_of(env, name, &logins) {
        Ok(account) => account,
        Err(why) => return refuse(err, &why),
    };
    let Some(store) = env.accounts_store_for(Cli::Claude) else {
        return refuse(
            err,
            "no harness state is known, so there is no accounts store",
        );
    };
    let tokens = store.shared_dir().join(TOKENS_DIR);
    if let Err(why) = store
        .ensure()
        .and_then(|()| aterm_types::fs_restricted::ensure_private_dir(&tokens))
    {
        return refuse(err, &format!("{}: {why}", tokens.display()));
    }
    let lock =
        accounts::lock_at_within(&tokens.join(format!("{}.lock", account.key.as_str())), &[]);
    let _lock = match lock {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            return refuse(
                err,
                &format!("a token for {} is being made in another tab", account.email),
            );
        }
        Err(why) => return refuse(err, &format!("the account's lock: {why}")),
    };
    let Some(vault) = host.token_vault() else {
        return refuse(err, "this Mac has no vault to store a token in");
    };
    match vault.route(now() + PREFLIGHT_HANG) {
        Ok(route) => {
            if let Some(why) = route_refusal(route) {
                return refuse(err, why);
            }
        }
        Err(why) => return refuse(err, &format!("the vault could not be read: {why}")),
    }
    let now = u64::try_from(host.now().max(env.now)).unwrap_or(0);
    let row = read_table(&store)
        .ok()
        .and_then(|mut table| table.rows.remove(account.key.as_str()));
    let standing_wait = if matches!(mode, TokenMode::Remove) {
        STANDING_LOOK
    } else {
        REQUIRED_STANDING_HANG
    };
    let found = match vault.standing(&account.key, Instant::now() + standing_wait) {
        Ok(found) => found,
        Err(why) => return refuse(err, &format!("the vault could not be read: {why}")),
    };
    if matches!(mode, TokenMode::Remove) {
        return remove(
            &store,
            vault.as_ref(),
            &account,
            (&found, row),
            now,
            out,
            err,
        );
    }
    let env_vars = host.verb_env();
    if let Some(name) = setup_token::route_in(&env_vars) {
        return refuse(
            err,
            &format!(
                "this terminal routes Claude Code through `{name}`; a token is made only on the \
                 default route"
            ),
        );
    }
    if !host.on_macos() {
        return refuse(
            err,
            "a token is made only on macOS, where aterm's wrapper serves it",
        );
    }
    let state = TokenState::of(&found, row.as_ref(), now);
    if state == TokenState::Unknown {
        return refuse(err, "the vault could not be read just now; try again");
    }
    let pending = vault.waiting_here(&account.key).unwrap_or(true);
    let expires_at = match &found {
        Lookup::Found(standing) => standing.meta.as_ref().map(|meta| meta.expires_at),
        _ => None,
    };
    if !matches!(mode, TokenMode::Replace)
        && let Some(why) = state.bare_refusal(&account.email, expires_at, pending)
    {
        return refuse(err, &why);
    }
    let Some((program, cli_version)) = host.setup_token_program() else {
        return refuse(
            err,
            &format!(
                "no Claude Code build is in atpkg's store to run setup-token with (`{}`)",
                Step::install(Cli::Claude)
            ),
        );
    };
    let Some(person) = host.token_person() else {
        return refuse(err, "this host has no terminal to sign in at");
    };
    mint(
        (&store, vault.as_ref(), person.as_ref()),
        (&account, &program, cli_version, &env_vars),
        (host.token_hang(), host.token_write_wait(), now),
        (out, err),
    )
}

/// The steps after the preflight: the capture, the write, the row, the journal, the outcome.
fn mint(
    (store, vault, person): (&Store, &dyn TokenVault, &dyn Person),
    (account, program, cli_version, env_vars): (
        &Account,
        &Program,
        String,
        &[(std::ffi::OsString, std::ffi::OsString)],
    ),
    (hang, write_wait, now): (Duration, Option<Duration>, u64),
    (out, err): (&mut dyn Write, &mut dyn Write),
) -> ExitCode {
    let refuse = |err: &mut dyn Write, why: &str| {
        let _ = writeln!(err, "aterm harness accounts token: {why}");
        ExitCode::from(1)
    };
    let deadline = WakeDeadline::after(hang);
    person.say(&format!(
        "making a Claude Code token for {}: sign in as {}",
        account.named(),
        account.email
    ));
    let root = store
        .shared_dir()
        .join(TOKENS_DIR)
        .join(account.key.as_str());
    let minted = setup_token::capture(
        &Capture {
            program,
            store: &root,
            sign_in_as: &account.email,
            env: env_vars,
            deadline,
        },
        person,
    );
    let minted: Minted = match minted {
        Ok(minted) => minted,
        Err(setup_token::Error::Stopped) => {
            let _ = writeln!(
                err,
                "aterm harness accounts token: stopped; nothing was made"
            );
            return ExitCode::from(130);
        }
        Err(setup_token::Error::WrongAccount(_)) => {
            note(TokenNote::WrongAccount, &account.key, now);
            return refuse(
                err,
                &format!(
                    "Claude Code made a token for another account; sign in as {}",
                    account.email
                ),
            );
        }
        Err(why) => return refuse(err, &capture_words(&why)),
    };
    let Some((expires_at, expiry_from)) = minted.expiry() else {
        return refuse(
            err,
            "Claude Code made a token that ends within a day; nothing was stored",
        );
    };
    let minted_on = vault
        .seat_here()
        .ok()
        .flatten()
        .unwrap_or_else(|| "this Mac".to_owned());
    let meta = Meta {
        email: account.email.clone(),
        account_id: account.account_id.clone().unwrap_or_default(),
        minted_at: minted.minted_at,
        expires_at,
        expiry_from,
        minted_on,
        cli: cli_version,
        verified: None,
    };
    let patience = write_wait.map_or(deadline, WakeDeadline::after);
    let written = store_patiently(vault, &account.key, &minted.token, &meta, person, patience);
    drop(minted);
    let written = match written {
        Ok(written) => written,
        Err(why) => {
            return refuse(
                err,
                &format!(
                    "Claude Code made a token, but the vault did not take it ({why}); nothing \
                     was stored"
                ),
            );
        }
    };
    let vault_col = match &written {
        MemberWritten::Posted => InVault::InVault,
        MemberWritten::SignedHere => InVault::SignedHere,
        MemberWritten::Waiting(why) => InVault::Waiting(waits_words(*why).to_owned()),
        MemberWritten::Overtaken(by) => {
            let by = by
                .as_ref()
                .map_or_else(|| "another Mac".to_owned(), |by| by.label.clone());
            if let Err(why) = record(store, account, &meta, InVault::Overtaken(by.clone()), None) {
                let _ = writeln!(err, "aterm harness accounts token: {TABLE_FILE}: {why}");
            }
            return refuse(
                err,
                &format!(
                    "Claude Code made a token, but {by}'s write of the account's token took it \
                     first and stands; nothing of this one was stored"
                ),
            );
        }
        MemberWritten::Refused(why) => {
            return refuse(
                err,
                &format!(
                    "Claude Code made a token, but the vault did not take it ({why}); nothing \
                     was stored"
                ),
            );
        }
        MemberWritten::Busy | MemberWritten::NotSeated | MemberWritten::NoLog => {
            let why = match &written {
                MemberWritten::NotSeated => route_refusal(WriteRoute::NotSeated),
                MemberWritten::NoLog => route_refusal(WriteRoute::NoLog),
                _ => route_refusal(WriteRoute::Busy),
            }
            .unwrap_or_default();
            return refuse(
                err,
                &format!(
                    "Claude Code made a token, but the vault did not take it ({why}); nothing \
                     was stored"
                ),
            );
        }
    };
    let signer = match vault_col {
        InVault::InVault | InVault::SignedHere => {
            match vault.standing(&account.key, Instant::now() + STANDING_LOOK) {
                Ok(Lookup::Found(standing)) => standing.signer,
                _ => None,
            }
        }
        _ => None,
    };
    let recorded = record(store, account, &meta, vault_col.clone(), signer);
    note(
        TokenNote::Minted(meta.expiry_from),
        &account.key,
        meta.minted_at,
    );
    let valid = valid_words(minted_lifetime(&meta), meta.expires_at);
    let held = account.held_note();
    let line = match &vault_col {
        InVault::InVault => format!(
            "stored a Claude Code token for {} in the vault, {valid}; it serves this Mac until \
             its account is verified{held}",
            account.email
        ),
        InVault::SignedHere => format!(
            "stored a Claude Code token for {} here, {valid}; not yet in vault: no server \
             answered, and this Mac's vault pass posts it{held}",
            account.email
        ),
        InVault::Waiting(why) => format!(
            "not yet in vault: the token for {} ({valid}) waits in the vault's waiting writes: \
             {why}; this Mac's vault pass signs it{held}",
            account.email
        ),
        InVault::Overtaken(_) | InVault::Removed => String::new(),
    };
    let _ = writeln!(out, "{line}");
    if let Err(why) = recorded {
        let _ = writeln!(
            err,
            "aterm harness accounts token: the token is in the vault's hands, but {TABLE_FILE} \
             was not updated: {why}"
        );
    }
    ExitCode::SUCCESS
}

/// How long a token's meta says it lasts, as the capture's words would: printed days, else
/// assumed.
fn minted_lifetime(meta: &Meta) -> Lifetime {
    match meta.expiry_from {
        ExpiryFrom::Assumed => Lifetime::Unprinted,
        ExpiryFrom::Printed => Lifetime::Days(
            u32::try_from(
                (meta.expires_at.saturating_sub(meta.minted_at) + 43_200).div_ceil(86_400),
            )
            .unwrap_or(u32::MAX),
        ),
    }
}

/// "valid for 1 year (until about 2027-11-04)", as the outcome words a token's lifetime.
fn valid_words(lifetime: Lifetime, expires_at: u64) -> String {
    let until = date(expires_at);
    match lifetime {
        Lifetime::Days(365) => format!("valid for 1 year (until about {until})"),
        Lifetime::Days(1) => format!("valid for 1 day (until about {until})"),
        Lifetime::Days(days) => format!("valid for {days} days (until about {until})"),
        Lifetime::Unprinted | Lifetime::UnderADay => {
            format!("valid until about {until} (Claude Code did not say; a year is assumed)")
        }
    }
}

/// The verb's words for a capture that made no token: fixed sentences, none holding a byte the
/// child wrote.
fn capture_words(why: &setup_token::Error) -> String {
    use setup_token::{Error, Failure};
    match why {
        Error::Route(name) => format!(
            "this terminal routes Claude Code through `{name}`; a token is made only on the \
             default route"
        ),
        Error::Scratch => "the capture's scratch folder could not be made".to_owned(),
        Error::Process => "Claude Code's setup-token could not be run".to_owned(),
        Error::Request => {
            "Claude Code asked for something other than an inference token; nothing was made"
                .to_owned()
        }
        Error::Hang => {
            "the sign-in took longer than the hang detector allows; nothing was made".to_owned()
        }
        Error::Output => {
            "Claude Code said more than the capture reads without a token; nothing was made"
                .to_owned()
        }
        Error::Stopped => "stopped; nothing was made".to_owned(),
        Error::NoToken => "Claude Code ended without showing a token".to_owned(),
        Error::TwoTokens => "Claude Code showed two different tokens; nothing was kept".to_owned(),
        Error::Unchecked => {
            "Claude Code showed a token before the sign-in it asked for; nothing was kept"
                .to_owned()
        }
        Error::WrongAccount(_) => "Claude Code made a token for another account".to_owned(),
        Error::Failed(failure) => match failure {
            Failure::CodeIncomplete => {
                "Claude Code says the pasted code was incomplete; nothing was made".to_owned()
            }
            Failure::Exchange => {
                "Claude Code could not exchange the sign-in for a token; nothing was made"
                    .to_owned()
            }
            Failure::OnHold => "Claude Code says the account is on hold".to_owned(),
            Failure::Withheld => {
                "an organization's policy refused the token, and Claude Code withheld it".to_owned()
            }
            Failure::Listener => {
                "Claude Code could not start the listener its browser sign-in returns to".to_owned()
            }
            Failure::OAuth => "Claude Code's sign-in failed; nothing was made".to_owned(),
        },
    }
}

/// Writes both entries as a member write, never throwing the made token away (§4.6): each try
/// waits a tick for a vault pass to let go ([`OnBusy::Refuse`], which leaves nothing behind);
/// once `deadline` passes, or at Ctrl-C, the write lands in the vault's waiting writes at once
/// ([`OnBusy::Wait`], `Waiting{LockDeadline}`), for the next vault pass. One line says it waits.
///
/// # Errors
/// The vault's error.
pub fn store_patiently(
    vault: &dyn TokenVault,
    key: &Key,
    token: &SecretBytes,
    meta: &Meta,
    person: &dyn Person,
    deadline: WakeDeadline,
) -> Result<MemberWritten, String> {
    let mut said = false;
    loop {
        if person.stopped() || deadline.passed() {
            return vault.write(key, token, meta, OnBusy::Wait(Instant::now()));
        }
        let by = Instant::now() + deadline.remaining().min(WRITE_TICK);
        match vault.write(key, token, meta, OnBusy::Refuse(by))? {
            MemberWritten::Busy => {
                if !said {
                    person.say("waiting for the vault (a vault pass is running)");
                    said = true;
                }
            }
            written => return Ok(written),
        }
    }
}

/// Records the account's row after a mint: every owner-named field, the vault column, the signer.
fn record(
    store: &Store,
    account: &Account,
    meta: &Meta,
    vault: InVault,
    signer: Option<String>,
) -> io::Result<()> {
    update_table(store, |table| {
        table.rows.insert(
            account.key.as_str().to_owned(),
            TokenRow {
                key: account.key.as_str().to_owned(),
                email: meta.email.clone(),
                account_id: account.account_id.clone(),
                label: account.label.clone(),
                entry: account.key.entry(),
                minted_at: Some(meta.minted_at),
                expires_at: Some(meta.expires_at),
                expiry_from: Some(expiry_word(meta.expiry_from).to_owned()),
                minted_on: Some(meta.minted_on.clone()),
                signer,
                used_on: UsedOn::default(),
                vault,
                verified: None,
                verification: Verification::default(),
                rejected_at: None,
                removed_at: None,
            },
        );
    })
}

pub(super) const fn expiry_word(from: ExpiryFrom) -> &'static str {
    match from {
        ExpiryFrom::Printed => "printed",
        ExpiryFrom::Assumed => "assumed",
    }
}

/// A token note the verb, the harness at a refusal ([`note_rejected`]) or a surface's offer
/// ([`super::accounts_token_view::note_offers`]) journals.
pub(super) enum TokenNote {
    /// `token-minted`, with where its end came from.
    Minted(ExpiryFrom),
    /// `token-removed`.
    Removed,
    /// `token-rejected`: Claude Code refused the token on this Mac.
    Rejected,
    /// `token-served`: this Mac's first status-line reading under the token.
    Served,
    /// `token-verified`: the minting Mac verified the token's account by its windows.
    Verified,
    /// `wrong-account`: the capture made a token for another account.
    WrongAccount,
    /// `token-offered`: a surface offered a token for the account, once per state pair.
    Offered,
}

/// Journals `what` about the account `key` at `t`: value-free, the key naming the account.
pub(super) fn note(what: TokenNote, key: &Key, t: u64) {
    let Some(account) = journal::Key::new(key.as_str()) else {
        return;
    };
    let note = match what {
        TokenNote::Minted(from) => {
            journal::token_note(TokenEvent::Minted, account, t, Some(expiry_word(from)))
        }
        TokenNote::Removed => journal::token_note(TokenEvent::Removed, account, t, None),
        TokenNote::Rejected => journal::token_note(TokenEvent::Rejected, account, t, None),
        TokenNote::Served => journal::token_note(TokenEvent::Served, account, t, None),
        TokenNote::Verified => journal::token_note(TokenEvent::Verified, account, t, None),
        TokenNote::WrongAccount => journal::wrong_token_account(account, t),
        TokenNote::Offered => journal::token_note(TokenEvent::Offered, account, t, None),
    };
    #[cfg(test)]
    NOTED.with(|noted| noted.borrow_mut().push(note));
    journal::note(&note);
}

#[cfg(test)]
thread_local! {
    /// The notes this thread's verb journaled, for its tests.
    pub(super) static NOTED: std::cell::RefCell<Vec<journal::Note>> = const {
        std::cell::RefCell::new(Vec::new())
    };
}

/// `--remove` (§4.6): both entries deleted under a fresh vault key, this Mac's waiting write of
/// them taken back, and the row updated.
fn remove(
    store: &Store,
    vault: &dyn TokenVault,
    account: &Account,
    (found, row): (&Lookup<Standing>, Option<TokenRow>),
    now: u64,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let refuse = |err: &mut dyn Write, why: &str| {
        let _ = writeln!(err, "aterm harness accounts token: {why}");
        ExitCode::from(1)
    };
    let removed = match vault.remove(&account.key, Instant::now() + REMOVE_WAIT) {
        Ok(removed) => removed,
        Err(why) => return refuse(err, &format!("the vault could not remove it: {why}")),
    };
    let stays = "it stays valid at claude.ai until it ends or you revoke it there";
    let line = match &removed {
        MemberUnset::Posted => format!(
            "removed the Claude Code token for {} from the vault; {stays}",
            account.email
        ),
        MemberUnset::SignedHere => format!(
            "removed the Claude Code token for {} here; not yet in vault: no server answered, \
             and this Mac's vault pass posts it; {stays}",
            account.email
        ),
        MemberUnset::TookBack => format!(
            "took back this Mac's token for {}, which still waited to be signed: nothing of it \
             reached the vault",
            account.email
        ),
        MemberUnset::Absent => {
            let _ = writeln!(
                out,
                "the vault holds no Claude Code token for {}",
                account.email
            );
            return ExitCode::SUCCESS;
        }
        MemberUnset::Overtaken(by) => {
            let by = by.as_ref().map_or_else(
                || "another write".to_owned(),
                |by| format!("{}'s write", by.label),
            );
            return refuse(
                err,
                &format!(
                    "the token for {} is not removed: {by} put it back",
                    account.email
                ),
            );
        }
        MemberUnset::Refused(why) => return refuse(err, why),
        MemberUnset::Busy => return refuse(err, "the vault is busy; try again"),
        MemberUnset::NotSeated => {
            return refuse(
                err,
                route_refusal(WriteRoute::NotSeated).unwrap_or_default(),
            );
        }
        MemberUnset::NoLog => {
            return refuse(err, route_refusal(WriteRoute::NoLog).unwrap_or_default());
        }
    };
    let meta = match found {
        Lookup::Found(standing) => standing.meta.clone(),
        _ => None,
    };
    let recorded = update_table(store, |table| {
        let row = table
            .rows
            .remove(account.key.as_str())
            .or(row)
            .unwrap_or_else(|| TokenRow {
                key: account.key.as_str().to_owned(),
                email: account.email.clone(),
                account_id: account.account_id.clone(),
                label: account.label.clone(),
                entry: account.key.entry(),
                minted_at: meta.as_ref().map(|m| m.minted_at),
                expires_at: meta.as_ref().map(|m| m.expires_at),
                expiry_from: meta.as_ref().map(|m| expiry_word(m.expiry_from).to_owned()),
                minted_on: meta.as_ref().map(|m| m.minted_on.clone()),
                signer: None,
                used_on: UsedOn::default(),
                vault: InVault::Removed,
                verified: None,
                verification: Verification::default(),
                rejected_at: None,
                removed_at: None,
            });
        table.rows.insert(
            account.key.as_str().to_owned(),
            TokenRow {
                vault: InVault::Removed,
                removed_at: Some(now),
                ..row
            },
        );
    });
    note(TokenNote::Removed, &account.key, now);
    let _ = writeln!(out, "{line}");
    if let Err(why) = recorded {
        let _ = writeln!(
            err,
            "aterm harness accounts token: {TABLE_FILE} was not updated: {why}"
        );
    }
    ExitCode::SUCCESS
}

/// THE ROWS OF `roster` THAT RUN ON THEIR VAULT TOKEN ON THIS MAC (§5.8), each with its token's
/// end (unix seconds): a row whose account's token, read value-free from the vault at `paths` in
/// one read ([`claude_token::listed`]) and judged with this Mac's table in `store` at `now`, is in
/// use here with its record naming the row's email
/// ([`super::token_route::standing_judged`]). Renewal is off for them: the upkeep never signs one
/// in by itself, and `accounts renew <label>` says so. Empty where the vault could not be read
/// just now (renewal then goes on as before, which signs in nothing a token would have served).
#[must_use]
pub fn rows_on_token(
    paths: &Paths,
    store: &Store,
    roster: &Roster,
    now: i64,
) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    let Ok(Lookup::Found(standings)) = claude_token::listed(paths) else {
        return out;
    };
    let now = u64::try_from(now).unwrap_or(0);
    for row in &roster.rows {
        let (Some(entry), Some(email)) = (
            accounts::TokenEntry::of_row(row),
            row.expected_identity_email(),
        ) else {
            continue;
        };
        let Some(standing) = standings
            .iter()
            .find(|standing| standing.key.as_str() == entry.key().as_str())
        else {
            continue;
        };
        let account = super::token_route::Account {
            label: Some(row.label.clone()),
            email: email.to_owned(),
        };
        let found = Lookup::Found(standing.clone());
        if super::token_route::standing_judged(&found, store, &standing.key, &account, now)
            == super::token_route::TokenFact::InUse
            && let Some(meta) = standing.meta.as_ref()
        {
            out.insert(row.label.clone(), meta.expires_at);
        }
    }
    out
}

/// The accounts the sessions on this Mac are logged into (§5.2), each as its own login record
/// names it ([`session_logins_at`] over `env`'s store, roster and home).
#[must_use]
pub fn session_logins(env: &Env) -> Vec<SessionLogin> {
    session_logins_at(
        env.accounts_store_for(Cli::Claude).as_ref(),
        env.accounts_roster_for(Cli::Claude).as_deref(),
        env.home.as_deref(),
        env.now,
    )
}

/// The accounts the sessions on this Mac are logged into (§5.2), each as its own login record
/// names it: the record of the machine's default login (which every session on no row runs on),
/// and the record of the login of each row a session recorded in the accounts state of `store`
/// is on, the rows read from the roster at `roster`, the records under `home`.
#[must_use]
pub fn session_logins_at(
    store: Option<&Store>,
    roster: Option<&std::path::Path>,
    home: Option<&std::path::Path>,
    now: i64,
) -> Vec<SessionLogin> {
    let mut dirs: Vec<Option<String>> = vec![None];
    if let Some(store) = store
        && let Ok((state, _)) = store.load(now)
        && let Some(path) = roster
        && let Ok(roster) = Roster::load_for(Cli::Claude, path)
    {
        for session in state.sessions.values() {
            if let Some(row) = roster.row(&session.account)
                && !dirs.contains(&row.dir)
            {
                dirs.push(row.dir.clone());
            }
        }
    }
    let mut out: Vec<SessionLogin> = Vec::new();
    for dir in dirs {
        if let Some(record) = home.and_then(|home| profile::login_record(home, dir.as_deref()))
            && !out
                .iter()
                .any(|seen| accounts::email_matches(&seen.email, &record.email))
        {
            out.push(SessionLogin {
                email: record.email,
                account_id: record.key.map(|key| key.to_string()),
                default: dir.is_none(),
            });
        }
    }
    out
}

#[cfg(test)]
#[path = "accounts_tokens_tests.rs"]
pub(in crate::harness) mod tests;
