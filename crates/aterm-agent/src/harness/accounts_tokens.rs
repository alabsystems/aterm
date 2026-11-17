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
//!   ([`claude_token::standing`], this Mac's refusal of the token beside it), the table's own
//!   facts and the clock, in §5.1's order.
//! - **The verb** ([`run`]) mints a token for one account in one step: only through aterm's
//!   opener, in aterm's own sign-in window at this Mac's desk or on the account's headless
//!   browser profile right after a headless sign-in proved it, and nobody pastes a code; under a
//!   per-account lock (never `signin.lock`); after a preflight (a seat in the vault's log, no
//!   route in the environment, macOS) and the refusal of a bare run while a token here is in use
//!   or on its way. Then it runs the capture ([`super::setup_token`]), writes both entries as a
//!   member write, records the row and journals `token-minted`. A busy vault after the mint never
//!   throws the token away: the write waits, bounded by the hang detector, then lands in the
//!   vault's waiting writes ("not yet in vault"); Ctrl-C lands it there at once.
//! - **The upkeep's tokens** (plan package FL-138, [`backfill_due`] and [`backfill_run`]): this
//!   Mac's upkeep makes, with nobody there, the missing token of each row signed in here, by a
//!   route no person finishes, and replaces the few it may (one ending soon, by its home Mac;
//!   one Claude Code refused here with a `401`; one minted here by no route), once per gap on
//!   record (no restart runs one again), never a short-lived one with the Claude Code build that
//!   made it ([`SHORT_LIVED_SECS`]), and never past a page only a person can pass.
//!
//! Nothing here prints, journals, logs or writes the token: it lives in the capture's zeroizing
//! buffer, then in the vault.

use super::accounts::{self, Cli, Roster, Row, Store};
use super::accounts_step::Step;
use super::cli::{AccountsHost, Env, TokenMode};
use super::journal::{self, TokenEvent};
use super::profile;
use super::setup_token::{self, Capture, Lifetime, Minted, Program, SignIn};
pub use super::token_state::{TokenState, Unverified};
use super::token_steps;
use super::wake_deadline::WakeDeadline;
use aterm_secrets::SecretBytes;
use aterm_secrets::claude_token::{
    self, Check, ExpiryFrom, Key, Lookup, Meta, MintedBy, Mismatch, Settled, Standing, State,
};
use aterm_secrets::client::{MemberUnset, MemberWritten, OnBusy, Paths, WaitFor, WriteRoute};
use std::collections::{BTreeMap, BTreeSet};
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
    /// When it was verified, where it was: this Mac's verification ([`verify_served`]), or the one
    /// the vault's meta held when the row was made, whichever Mac wrote it.
    pub verified: Option<u64>,
    /// What this Mac's comparison by windows found so far (§5.6), on any Mac the token is in use
    /// on.
    pub verification: Verification,
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
    /// Each account's kept row of the token the vault holds while the account's row is of a later
    /// one, by account key ([`row_of_token`]): the row a mint replaced where the vault held its
    /// token then ([`record`]), or one made for the vault's token where a later write of this
    /// Mac's is still on its way. The vault's token is judged by its own facts here (its verdict,
    /// its refusal) until a reading shows the vault holds the row's token or a later one
    /// ([`Self::row_for`]).
    pub replaced: BTreeMap<String, TokenRow>,
}

impl Table {
    /// This Mac's row of the account `key` for the vault's token as `found` reads it: the
    /// account's row, or, where that row is of another token and the kept row
    /// ([`Self::replaced`]) is the vault's (the same `minted_at`), that token's row. A token is
    /// judged by this Mac's facts of it ([`TokenState::of`]).
    #[must_use]
    pub fn row_for(&self, key: &str, found: &Lookup<Standing>) -> Option<&TokenRow> {
        let minted_at = match found {
            Lookup::Found(standing) => standing.meta.as_ref().map(|meta| meta.minted_at),
            Lookup::Absent | Lookup::Unknown => None,
        };
        let row = self.rows.get(key);
        if minted_at.is_some()
            && row.is_some_and(|row| row.minted_at != minted_at)
            && let Some(kept) = self
                .replaced
                .get(key)
                .filter(|kept| kept.minted_at == minted_at)
        {
            return Some(kept);
        }
        row
    }

    /// The table as `tokens.json` holds it: its rows with no check of their tokens, which
    /// [`CHECK_FILE`] holds ([`Self::checks_json`]).
    #[must_use]
    pub fn to_json(&self) -> String {
        let rows: Vec<aterm_json::Value> =
            self.rows.values().map(|row| row_json(row, false)).collect();
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

    /// This Mac's checks of each row's token as [`CHECK_FILE`] holds them: each row's
    /// [`Verification`] under its account key and its token's `minted_at`, and each kept row
    /// ([`Self::replaced`]) whole.
    #[must_use]
    pub fn checks_json(&self) -> String {
        let checks: Vec<aterm_json::Value> = self
            .rows
            .values()
            .filter(|row| row.verification != Verification::default())
            .map(|row| {
                aterm_json::json!({
                    "key": row.key,
                    "minted_at": row.minted_at,
                    CHECK_KEY: row.verification.to_json(),
                })
            })
            .collect();
        let replaced: Vec<aterm_json::Value> = self
            .replaced
            .values()
            .map(|row| row_json(row, true))
            .collect();
        aterm_json::to_string(&aterm_json::json!({
            "schema": SCHEMA,
            "checks": checks,
            "replaced": replaced,
        }))
        .unwrap_or_default()
    }

    /// The checks `bytes` holds ([`Self::checks_json`]) put on this table: each row's
    /// verification where its account key and `minted_at` are the row's, and each kept row.
    /// `None` where `bytes` is not a check file of schema 1, or a check or a kept row in it names
    /// a verdict this build does not know ([`Verification::of_json`]).
    ///
    /// An earlier build rewrites `tokens.json` without reading this file, moving an account's row
    /// to another token (the vault's, either way, or one it minted) with no verdict or refusal of
    /// it, so a check and a row may no longer meet. A check of a token the account's row is no
    /// longer of is kept as that token's kept row, made from the account's row, where none is
    /// kept: a token whose windows differ still serves nowhere while the vault holds it. A kept
    /// row of the token the account's row is of now gives that row its verdict and refusal where
    /// the row holds none, and goes: it holds this Mac's facts of the very token the row is of
    /// (the row an earlier build moved onto the vault's token, while this build's write of a later
    /// one waited), which a check of the later token never displaces.
    fn with_checks(mut self, bytes: &[u8]) -> Option<Self> {
        let value: aterm_json::Value = aterm_json::from_slice(bytes).ok()?;
        if value.get("schema")?.as_i64()? != SCHEMA {
            return None;
        }
        let mut apart = Vec::new();
        for check in value.get("checks")?.as_array()? {
            let (Some(key), Some(minted_at), Some(verification)) = (
                check.get("key").and_then(aterm_json::Value::as_str),
                check.get("minted_at").and_then(aterm_json::Value::as_u64),
                check.get(CHECK_KEY),
            ) else {
                continue;
            };
            let verification = Verification::of_json(verification)?;
            let Some(row) = self.rows.get_mut(key) else {
                continue;
            };
            if row.minted_at == Some(minted_at) {
                row.verification = verification;
            } else {
                apart.push(TokenRow {
                    minted_at: Some(minted_at),
                    expires_at: None,
                    expiry_from: None,
                    minted_on: None,
                    signer: None,
                    used_on: UsedOn::default(),
                    vault: InVault::InVault,
                    verified: None,
                    verification,
                    removed_at: None,
                    ..row.clone()
                });
            }
        }
        for kept in value.get("replaced")?.as_array()? {
            let verification = match kept.get(CHECK_KEY) {
                Some(check) => Verification::of_json(check)?,
                None => Verification::default(),
            };
            if let Some(row) = row_of(kept) {
                self.replaced.insert(
                    row.key.clone(),
                    TokenRow {
                        verification,
                        ..row
                    },
                );
            }
        }
        for kept in apart {
            self.replaced.entry(kept.key.clone()).or_insert(kept);
        }
        let Self { rows, replaced, .. } = &mut self;
        replaced.retain(|key, kept| {
            let Some(row) = rows
                .get_mut(key)
                .filter(|row| row.minted_at == kept.minted_at)
            else {
                return true;
            };
            if row.verification == Verification::default() {
                row.verification = std::mem::take(&mut kept.verification);
            }
            false
        });
        Some(self)
    }

    /// The table `bytes` holds, where it is one of schema 1, with no check of its tokens
    /// ([`CHECK_FILE`]). A row that does not read is left out.
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

/// The file beside the table that holds this Mac's checks of each account's tokens
/// ([`Table::checks_json`]): each row's [`Verification`] by its account key and its token's
/// `minted_at`, and each kept row ([`Table::replaced`]). Its own file, which no earlier build
/// opens: an earlier build rewrites `tokens.json` whole on every `accounts` report and drops
/// fields it does not know, which would drop a token's verdict (a token whose windows differ
/// would serve again) and the kept row of the token the vault still holds.
pub const CHECK_FILE: &str = "tokens-check.json";

/// The key [`CHECK_FILE`] keeps a [`Verification`] under. Rows an earlier build wrote while the
/// basis was the row's usage `Sample` (which an earlier token's readings fold into) kept it in
/// `tokens.json` under `verification`, which no build reads now: their basis and verdict were
/// reached against windows of unknown origin, so such a row reads with none, and its token awaits
/// a stored-login reading.
const CHECK_KEY: &str = "windows_check";

/// One row as `tokens.json` holds it, or, `with_check`, as [`CHECK_FILE`] keeps a kept row: with
/// its [`Verification`].
fn row_json(row: &TokenRow, with_check: bool) -> aterm_json::Value {
    let mut value = aterm_json::json!({
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
        "removed_at": row.removed_at,
    });
    if with_check && let Some(fields) = value.as_object_mut() {
        fields.insert(CHECK_KEY.to_owned(), row.verification.to_json());
    }
    value
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
        verification: Verification::default(),
        removed_at: secs(value, "removed_at"),
    })
}

/// The folder that holds the table: Claude Code's accounts store.
fn table_dir(store: &Store) -> PathBuf {
    store.shared_dir()
}

/// What one read of the table's two files found ([`read_parts`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableRead {
    /// The table: with its checks, or `tokens.json`'s rows alone where [`Self::unchecked`] says
    /// why the check file did not read.
    pub table: Table,
    /// Why [`CHECK_FILE`] is there and does not read (a restore with the wrong mode, a truncated
    /// file, a later schema), where it does not: no verdict or kept row of it is known, and none
    /// is written over it.
    pub unchecked: Option<String>,
}

impl TableRead {
    /// The table whole.
    ///
    /// # Errors
    /// The check file did not read.
    pub fn whole(self) -> io::Result<Table> {
        match self.unchecked {
            None => Ok(self.table),
            Some(why) => Err(io::Error::other(why)),
        }
    }

    /// The table the vault's token `found` is judged by: whole, or, where the check file did not
    /// read, `tokens.json`'s rows alone for a token this Mac does not compare ([`compared_here`]:
    /// one another Mac minted that is verified, or that serves only its minting Mac until it is),
    /// or for an account whose vault holds none or could not be read. Such a token's verification
    /// is the vault's word, never this Mac's, and the check file holds no verdict of it; a token
    /// this Mac compares is judged by its verdict here, which lives in that file alone.
    ///
    /// # Errors
    /// The check file did not read, and this Mac compares the vault's token.
    pub fn for_token(self, found: &Lookup<Standing>) -> io::Result<Table> {
        match found {
            Lookup::Found(standing) if compared_here(standing) => self.whole(),
            Lookup::Found(_) | Lookup::Absent | Lookup::Unknown => Ok(self.table),
        }
    }
}

/// Whether this Mac's verdict of the token `standing` reads (which lives in [`CHECK_FILE`] alone)
/// is needed to judge it here: it was minted here, or it is not verified and the vault's records
/// put it in use here ([`Standing::serves_here`]: a token aterm's window or headless route made,
/// on any Mac), so this Mac compares it with the account's stored login ([`note_served`],
/// [`note_basis`]) and a verdict here may hold it off this Mac.
fn compared_here(standing: &Standing) -> bool {
    standing.minted_here || (!standing.verified() && standing.serves_here())
}

/// The table on this Mac and its checks ([`CHECK_FILE`]): empty where there is none yet. A check
/// file that is there and does not read is said once in this process's log for each reason
/// ([`say_unchecked`]), and the table is `tokens.json`'s rows alone ([`TableRead::unchecked`]).
///
/// # Errors
/// `tokens.json` is there and does not read, is not private, or is not a table of schema 1.
pub fn read_parts(store: &Store) -> io::Result<TableRead> {
    let dir = table_dir(store);
    let read = |file: &str| match aterm_uds::private_file::read_private(&dir, file, MAX_TABLE) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(aterm_uds::private_file::PrivateFileError::Missing) => Ok(None),
        Err(why) => Err(format!("{file}: {why:?}")),
    };
    let table = match read(TABLE_FILE).map_err(io::Error::other)? {
        Some(bytes) => Table::from_json(&bytes)
            .ok_or_else(|| io::Error::other(format!("{TABLE_FILE} is not a table of schema 1")))?,
        None => Table::default(),
    };
    let checked = match read(CHECK_FILE) {
        Ok(Some(bytes)) => table.clone().with_checks(&bytes).ok_or_else(|| {
            format!("{CHECK_FILE} is not a check file of schema 1 whose verdicts this build knows")
        }),
        Ok(None) => Ok(table.clone()),
        Err(why) => Err(why),
    };
    let unchecked = checked.as_ref().err().cloned();
    say_unchecked(&dir, unchecked.as_deref());
    Ok(TableRead {
        table: checked.unwrap_or(table),
        unchecked,
    })
}

/// The table on this Mac, with its checks ([`CHECK_FILE`]): empty where there is none yet.
///
/// # Errors
/// A file is there and does not read, is not private, or is not a table or a check file.
pub fn read_table(store: &Store) -> io::Result<Table> {
    read_parts(store)?.whole()
}

/// The table on this Mac the vault's token `found` is judged by ([`TableRead::for_token`]): with
/// its checks, or, where the check file does not read, `tokens.json`'s rows alone for a token
/// this Mac does not compare.
///
/// # Errors
/// `tokens.json` does not read, or the check file does not read and this Mac compares the token.
pub fn read_table_for(store: &Store, found: &Lookup<Standing>) -> io::Result<Table> {
    read_parts(store)?.for_token(found)
}

/// Says once in this process's log, for the folder `dir`, why its check file did not read
/// (`unchecked`), again only once that reason changes or the file read in between: a record of
/// the file and the reason for whoever reads the log, never an alert. A check file is never
/// rewritten over one that does not read, so its verdicts stay for the build that reads it.
/// Returns whether this call said it.
fn say_unchecked(dir: &std::path::Path, unchecked: Option<&str>) -> bool {
    static SAID: std::sync::Mutex<BTreeMap<PathBuf, String>> =
        std::sync::Mutex::new(BTreeMap::new());
    let mut said = SAID
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(why) = unchecked else {
        said.remove(dir);
        return false;
    };
    if said.get(dir).is_some_and(|was| was == why) {
        return false;
    }
    said.insert(dir.to_owned(), why.to_owned());
    drop(said);
    aterm_log::warn!(
        "claude tokens: {} does not read ({why}): a token this Mac compares is held busy until \
         it reads, and any other is judged without it; nothing writes over it",
        dir.join(CHECK_FILE).display()
    );
    #[cfg(test)]
    UNCHECKED_SAID.with(|said| said.borrow_mut().push(why.to_owned()));
    true
}

#[cfg(test)]
thread_local! {
    /// The reasons this thread's reads said of a check file that did not read ([`say_unchecked`]),
    /// for its tests.
    pub(super) static UNCHECKED_SAID: std::cell::RefCell<Vec<String>> = const {
        std::cell::RefCell::new(Vec::new())
    };
}

/// Changes the table under `tokens.lock` (waiting for it as `accounts.lock`'s takers do) and,
/// where the change changed it, writes it whole, atomically, mode 0600: its checks
/// ([`CHECK_FILE`]) first, then `tokens.json`, so a check is never missing for a row that needs
/// it.
///
/// # Errors
/// The lock stays held, or the table cannot be read or written.
pub fn update_table(store: &Store, change: impl FnOnce(&mut Table)) -> io::Result<()> {
    update_parts(store, None, change)
}

/// [`update_table`] for a change of this Mac's facts of the vault's token `standing` (its served
/// facts, its refusal): where the check file does not read and this Mac does not compare the
/// token ([`TableRead::for_token`]), the change is made on `tokens.json`'s rows alone and only
/// `tokens.json` is written, the check file left as it is: the change touches no verdict, and a
/// later read of the check file takes it as it takes an earlier build's rewrite of
/// `tokens.json`. A token this Mac compares is refused then: its verdict lives in that file alone.
///
/// # Errors
/// The lock stays held, or the table cannot be read or written.
pub fn update_table_for(
    store: &Store,
    standing: &Standing,
    change: impl FnOnce(&mut Table),
) -> io::Result<()> {
    update_parts(store, Some(standing), change)
}

fn update_parts(
    store: &Store,
    standing: Option<&Standing>,
    change: impl FnOnce(&mut Table),
) -> io::Result<()> {
    let dir = table_dir(store);
    store.ensure()?;
    let _lock = accounts::lock_at_within(&dir.join(TABLE_LOCK), &accounts::LOCK_LADDER)?
        .ok_or_else(|| io::Error::other(format!("{TABLE_LOCK} stayed held")))?;
    let read = read_parts(store)?;
    let whole = read.unchecked.is_none();
    let mut table = match standing {
        Some(standing) => read.for_token(&Lookup::Found(standing.clone()))?,
        None => read.whole()?,
    };
    let was = table.clone();
    change(&mut table);
    // A change that changed nothing writes nothing: a reading that finds its row already saying
    // what it would record (a final verdict the tap's heartbeat look reaches) rewrites no file.
    if table == was {
        return Ok(());
    }
    if whole {
        aterm_uds::private_file::write_private(&dir, CHECK_FILE, table.checks_json().as_bytes())?;
    }
    aterm_uds::private_file::write_private(&dir, TABLE_FILE, table.to_json().as_bytes())
}

/// RECORDS THIS MAC'S REFUSAL OF A TOKEN (§5.6): Claude Code drew its anchored lost-login wall
/// (`alab_policy::vocab::WallKind::Auth`) on a session the wrapper served the token whose standing
/// is `standing`, at `now`, its newest API error answering the HTTP status `status`. The refusal of
/// the token its meta names (its `minted_at`), with that status, is kept beside
/// this Mac's vault state at `paths` ([`claude_token::note_refused`]), where every reading of the
/// token's standing finds it, so [`TokenState::of`] and a `claude-token:` need alike read
/// `rejected`, not in use, until a new token replaces it; never of another token.
/// `token-rejected` is journaled once, at the first refusal. Before it, this Mac's row of that
/// token in the table at `store` ([`row_of_token`]: the account's row, made from the meta where
/// this Mac kept none, or, for a token older than the one the account's row is of while that row's
/// write is on its way, its kept row, made where none was kept) is made or followed, as a served
/// reading's is, so the report shows the refused token's own `verified` and reads its peers. It
/// writes nothing else: no stored-login state (its `Auth`, its learned lifetime), since the stored
/// login was not what was refused, and nothing to the vault, since a refusal is this Mac's
/// observation (each Mac's sessions meet the same refusal at their next request). Returns whether
/// this call recorded it first; a standing with no meta names no token and writes nothing. The
/// row of a token this Mac does not compare is made or followed in `tokens.json` alone where the
/// check file does not read ([`update_table_for`]).
///
/// # Errors
/// The table could not be read or written under `tokens.lock` (for a token this Mac compares,
/// the check file did not read), or the refusal could not be written beside this Mac's vault
/// state; either leaves the refusal for the next lost-login point.
pub fn note_rejected(
    paths: &Paths,
    store: &Store,
    standing: &Standing,
    status: u16,
    now: u64,
) -> io::Result<bool> {
    let Some(meta) = standing.meta.as_ref() else {
        return Ok(false);
    };
    update_table_for(store, standing, |table| {
        row_of_token(table, standing, meta);
    })?;
    let first = claude_token::note_refused(paths, standing, status, now)
        .map_err(|why| io::Error::other(why.to_string()))?;
    if first {
        note(TokenNote::Rejected, &standing.key, now);
    }
    Ok(first)
}

/// Which of this Mac's rows of an account holds its facts of the vault's token
/// ([`row_of_token`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Held {
    /// The account's row.
    Account,
    /// The kept row ([`Table::replaced`]) of a token older than the one the account's row is of.
    Replaced,
}

/// A row of `standing`'s token made from its meta, nothing known of it here yet.
fn fresh_row(standing: &Standing, meta: &Meta) -> TokenRow {
    TokenRow {
        key: standing.key.as_str().to_owned(),
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
        verified: meta.verification().map(|verified| verified.at),
        verification: Verification::default(),
        removed_at: None,
    }
}

/// THIS MAC'S ROW OF THE VAULT'S TOKEN, whose standing is `standing` and meta `meta`, where this
/// Mac's facts of that token (its served facts, its verification, its refusal) go, and which of
/// the account's rows it is:
///
/// - with no row of the account, one made from the meta (a Mac that did not mint it, or a mint
///   whose row was not written);
/// - the account's row, where it is of the vault's token (the same `minted_at`);
/// - where the vault's token is newer than the row's, or the row's own write is gone from the vault
///   (overtaken, removed), the account's row, following it: the kept row of that token comes back
///   with its facts, else the row starts afresh from the meta;
/// - where the vault's token is older than the row's and the row's write is not gone (this Mac's
///   later write still on its way, or a standing read before it landed), the kept row of the
///   vault's token ([`Table::replaced`]), made from the meta where none was kept, under the
///   account's label: the account's row never moves back to an older token on a reading, so a
///   stale standing never takes this Mac's newest token's row from it.
///
/// Once the account's row is the vault's token's, the kept row goes: the vault holds the row's
/// token or a later one.
fn row_of_token<'a>(
    table: &'a mut Table,
    standing: &Standing,
    meta: &Meta,
) -> (&'a mut TokenRow, Held) {
    let key = standing.key.as_str().to_owned();
    let Table { rows, replaced, .. } = table;
    let older = rows.get(&key).and_then(|row| {
        let later = row.minted_at.is_some_and(|minted| meta.minted_at < minted);
        let gone = matches!(row.vault, InVault::Overtaken(_) | InVault::Removed);
        (later && !gone).then(|| row.label.clone())
    });
    if let Some(label) = older {
        let kept = replaced
            .entry(key)
            .and_modify(|kept| {
                if kept.minted_at != Some(meta.minted_at) {
                    *kept = fresh_row(standing, meta);
                }
            })
            .or_insert_with(|| fresh_row(standing, meta));
        if kept.label.is_none() {
            kept.label = label;
        }
        return (kept, Held::Replaced);
    }
    let row = rows
        .entry(key.clone())
        .or_insert_with(|| fresh_row(standing, meta));
    if row.minted_at != Some(meta.minted_at) {
        match replaced
            .remove(&key)
            .filter(|kept| kept.minted_at == Some(meta.minted_at))
        {
            Some(kept) => *row = kept,
            None => {
                *row = TokenRow {
                    label: row.label.take(),
                    used_on: UsedOn {
                        peers: std::mem::take(&mut row.used_on.peers),
                        peers_read_at: row.used_on.peers_read_at,
                        ..UsedOn::default()
                    },
                    ..fresh_row(standing, meta)
                };
            }
        }
    }
    replaced.remove(&key);
    (row, Held::Account)
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
    /// [`accounts::Sample::session`]), each by the reset the sample names. A window whose reset
    /// the sample does not name is left out: the end `Sample::week` gives such a figure is the
    /// latest its window can end, not an instant a reading's reset can be compared with. What
    /// another account's sample shows is what the shared check reads ([`Served::others`]).
    #[must_use]
    pub fn current(sample: &accounts::Sample, now: i64) -> Self {
        Self {
            week_reset: sample.week(now).and(sample.week_reset),
            session_reset: sample.session(now).and(sample.session_reset),
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

    /// Whether these windows name a week still running at `now`: what a basis must name to be
    /// compared with ([`Verdict::of`], [`Verification::current_basis`]).
    #[must_use]
    pub fn week_running(&self, now: i64) -> bool {
        self.week_reset.is_some_and(|reset| reset > now)
    }

    /// Whether `reading` shows these windows: the same week, and the same session where both
    /// name one current at `now`. A reading's session that has ended by `now` (a reading taken
    /// before that window reset, handed on later) says nothing of the session these windows are
    /// in now, and is compared with none.
    #[must_use]
    pub fn shown_by(&self, reading: &Self, now: i64) -> bool {
        let current = |reset: Option<i64>| reset.filter(|reset| *reset > now);
        let Some(week) = current(self.week_reset) else {
            return false;
        };
        reading.week_reset == Some(week)
            && match (current(self.session_reset), current(reading.session_reset)) {
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

/// What this Mac's comparison of a token session's reading with the basis its row recorded found
/// (§5.6): this Mac's finding alone, which the vault holds only as `verified`, written by the Mac
/// whose comparison found the windows equal. `Equal` and `Differ` are final for the token
/// ([`Verdict::is_final`]); `Shared` holds for the week it was found in, and a basis of a later
/// week reopens it ([`note_basis`]), the accounts that shared the windows kept
/// ([`Verification::shared_with`]); `NoBasis` says only that the reading had nothing to be
/// compared with: the next stored-login reading compares it ([`note_basis`]) where its week still
/// runs, and the next token reading is compared again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The reading showed the basis's windows, which no other account here shares: this Mac
    /// writes `verified`, whichever Mac minted the token (until it is written, each reading tries
    /// the write again).
    Equal,
    /// The reading's windows are not the basis's: this Mac starts no session on the token
    /// ([`TokenState::in_use`]), its readings are folded into no row ([`Due::Apart`]), and it
    /// journals `token-differ` once, value-free. Nothing is written to the vault: other Macs judge
    /// it by their own readings, and keep using it.
    Differ,
    /// They are, and another account's current windows on this Mac are too: the comparison could
    /// not tell the accounts apart, so no verification follows that week; a basis of a later week
    /// reopens it, and a later comparison is equal only where this Mac holds a current reading of
    /// every account that shared the windows and none shows them ([`Verification::compare`]): an
    /// account idle here since says nothing of whether they moved apart. That is no evidence of another
    /// account (the windows are the stored login's), so the token serves where its route puts it
    /// in use: every Mac for one aterm's window or headless route made, only the minting Mac for
    /// one a person's terminal made or whose meta names no route, whose surfaces there offer its
    /// replacement ([`TokenState::offer`]).
    Shared,
    /// The row recorded no basis that names a week still running. Not final.
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

    /// Whether this verdict is kept for the token's life: a later reading reopens neither windows
    /// found different nor equal ones whose write is still due. `Shared` is not: a basis of a
    /// later week reopens it ([`note_basis`]), and until then no reading compares it again
    /// ([`Verification::compares`]). Nor is `NoBasis`: no comparison was made.
    #[must_use]
    pub const fn is_final(self) -> bool {
        match self {
            Self::Equal | Self::Differ => true,
            Self::Shared | Self::NoBasis => false,
        }
    }

    /// The comparison of `reading`, a token session's reading naming a week still running at
    /// `now` ([`note_served`] compares no other), with `basis`, the windows the row recorded for
    /// it ([`Verification::basis`]), where `others` are the other accounts' current windows on
    /// this Mac, at `now`. A basis that names no week still running at `now` is none
    /// ([`Windows::week_running`]).
    #[must_use]
    pub fn of(basis: Option<&Windows>, reading: &Windows, others: &[Windows], now: i64) -> Self {
        let Some(basis) = basis.filter(|basis| basis.week_running(now)) else {
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

/// What this Mac's comparison by windows has found of the row's token (§5.6), on any Mac the token
/// is in use on: the basis (the latest reading of a session on the row's stored login here,
/// [`note_basis`]), the served reading of the token compared with it, and what comparing them
/// found. Empty where the token is verified, or not in use here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Verification {
    /// The basis and when it was recorded (unix seconds): the windows of the latest status-line
    /// reading of a session on the row's stored login since the token was made, each replacing
    /// the one before ([`note_basis`]). Never a token session's reading, this token's or an
    /// earlier one's. One that names no week still running is no basis ([`Verdict::of`]).
    pub basis: Option<(u64, Windows)>,
    /// The served reading `verdict` is of: the first that carried rate limits while the row had
    /// a current basis, which a final verdict keeps; until then, the latest that carried them,
    /// which the next stored-login reading compares where its week still runs ([`note_basis`]).
    pub reading: Option<Windows>,
    /// What comparing the two found. `no-basis` is not final ([`Verdict::is_final`]): a row
    /// holding it is compared again at its next served reading.
    pub verdict: Option<Verdict>,
    /// The roster rows whose accounts' current windows showed the token's at a comparison of it
    /// here (`shared`): kept for the token's life, through every reopening ([`note_basis`]), so
    /// a later week's equal windows verify it only where this Mac reads each of those accounts'
    /// current windows apart from the token's ([`Self::compare`]).
    pub shared_with: BTreeSet<String>,
}

impl Verification {
    /// The basis a token reading is compared with at `now`: the recorded one, where it names a
    /// week still running ([`Windows::week_running`]).
    #[must_use]
    pub fn current_basis(&self, now: i64) -> Option<&Windows> {
        self.basis
            .as_ref()
            .map(|(_, basis)| basis)
            .filter(|basis| basis.week_running(now))
    }

    /// Whether a token reading is compared now ([`note_served`]): nothing compared yet, or only
    /// with no basis. Windows found shared wait for a basis of a later week ([`note_basis`]).
    #[must_use]
    pub const fn compares(&self) -> bool {
        matches!(self.verdict, None | Some(Verdict::NoBasis))
    }

    /// COMPARES `reading`, a token reading naming a week still running at `now`, with the current
    /// basis ([`Verdict::of`]), `others` being the other accounts' current windows here, and keeps
    /// what it found: the verdict, the reading and, where they are `shared`, every account whose
    /// windows showed the reading ([`Self::shared_with`]). Equal windows of a token once found
    /// shared are `shared` again unless every account it was shared with has current windows
    /// here, none of them showing the reading's: a comparison with an account this Mac no longer
    /// reads (idle here since, its week ended) cannot tell the two accounts apart either, and the
    /// token may be that account's. The verdict is returned.
    pub fn compare(&mut self, reading: Windows, others: &[Other], now: i64) -> Verdict {
        let windows: Vec<Windows> = others.iter().map(|other| other.windows).collect();
        let mut verdict = Verdict::of(self.current_basis(now), &reading, &windows, now);
        match verdict {
            Verdict::Shared => self.shared_with.extend(
                others
                    .iter()
                    .filter(|other| other.windows.shown_by(&reading, now))
                    .map(|other| other.label.clone()),
            ),
            Verdict::Equal
                if self.shared_with.iter().any(|label| {
                    !others
                        .iter()
                        .any(|other| other.label == *label && other.windows.week_running(now))
                }) =>
            {
                verdict = Verdict::Shared;
            }
            Verdict::Equal | Verdict::Differ | Verdict::NoBasis => {}
        }
        self.verdict = Some(verdict);
        self.reading = Some(reading);
        verdict
    }

    /// Whether what this Mac found of the row's token is kept for the token's life: a final
    /// verdict ([`Verdict::is_final`]), or windows found shared whose accounts this row does not
    /// keep ([`Self::shared_with`]: an earlier build's `shared`, or one an earlier build wrote
    /// back), which stays final as that build made it, since no later comparison could show the
    /// token's account apart from accounts nobody named.
    #[must_use]
    pub fn is_final(&self) -> bool {
        self.verdict.is_some_and(|verdict| {
            verdict.is_final() || (verdict == Verdict::Shared && self.shared_with.is_empty())
        })
    }

    /// Whether verification of the row's token still waits for a basis at `now`: nothing final
    /// found ([`Self::is_final`]) and no current basis ([`Self::current_basis`]), so a reading of
    /// the token now would be compared with nothing.
    #[must_use]
    pub fn awaits_basis(&self, now: i64) -> bool {
        !self.is_final() && self.current_basis(now).is_none()
    }

    fn to_json(&self) -> aterm_json::Value {
        aterm_json::json!({
            "basis_at": self.basis.map(|(at, _)| at),
            "basis": self.basis.map(|(_, windows)| windows.to_json()),
            "reading": self.reading.map(Windows::to_json),
            "verdict": self.verdict.map(Verdict::word),
            "shared_with": self.shared_with.iter().collect::<Vec<_>>(),
        })
    }

    /// The verification `value` holds ([`Self::to_json`]). `None` where it names a verdict this
    /// build does not know: a later build that made one changed what the check file means, so
    /// the file does not read here ([`Table::with_checks`]), rather than letting this build take
    /// the token for one with nothing compared and compare it again.
    fn of_json(value: &aterm_json::Value) -> Option<Self> {
        let basis_at = value.get("basis_at").and_then(aterm_json::Value::as_u64);
        let verdict = match value.get("verdict").filter(|v| !v.is_null()) {
            Some(word) => Some(Verdict::parse(word.as_str()?)?),
            None => None,
        };
        Some(Self {
            basis: basis_at.zip(value.get("basis").map(Windows::of_json)),
            reading: value
                .get("reading")
                .filter(|v| !v.is_null())
                .map(Windows::of_json),
            verdict,
            shared_with: value
                .get("shared_with")
                .and_then(aterm_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|label| label.as_str().map(str::to_owned))
                .collect(),
        })
    }
}

/// Another account's current windows on this Mac ([`Windows::current`]), by the roster row whose
/// sample shows them: what the shared check reads ([`Served::others`], [`note_basis`],
/// [`Verification::compare`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Other {
    /// The row's label.
    pub label: String,
    /// Its account's current windows.
    pub windows: Windows,
}

/// A token session's served reading, as the harness's attribution hands it to [`note_served`]
/// before the reading is folded into the row (§5.6, §5.7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Served {
    /// The roster row the session's tap credits.
    pub label: String,
    /// The reading's windows.
    pub reading: Windows,
    /// The other accounts' current windows on this Mac, as their samples hold them
    /// ([`Windows::current`]), each by its row. A sample holds every reading folded into its row,
    /// an unverified token's included, so a wrong-account token of another row can put its
    /// account's windows here; that only ever turns an equal comparison into a shared one, never
    /// the reverse.
    pub others: Vec<Other>,
    /// Whether the wrapper named the token it served the session, the vault's
    /// (`token_route::ServedToken::Named`): only such a reading is compared for verification. A
    /// session an earlier aterm's wrapper served names none; taken for one on the vault's token
    /// only where that token was made before the session began, its reading counts for the token
    /// and its row but is never compared.
    pub named: bool,
}

/// The evidence [`note_served`] found equal, for [`verify_served`] to write: the token it was read
/// on (its meta's `minted_at` in the standing the reading was judged against), and the windows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verify {
    /// The compared token's `minted_at`: [`claude_token::verify`] writes nothing where the vault
    /// now holds another token.
    pub minted_at: u64,
    /// What the meta's `verified` will say.
    pub evidence: aterm_secrets::claude_token::Verified,
}

/// What [`note_served`] leaves for its caller to do once the reading's fold is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Due {
    /// Nothing.
    Nothing,
    /// This Mac's comparison found the token's windows different from the basis's
    /// ([`Verdict::Differ`]), or the vault's records do not put the token in use here
    /// ([`Standing::serves_here`]: made at a terminal or by an earlier build on another Mac, and
    /// not verified): the reading may be another account's, so the caller folds nothing of it
    /// into the row.
    Apart,
    /// This Mac writes `verified` for this token with this evidence ([`verify_served`]).
    Verify(Verify),
}

/// RECORDS A SERVED READING OF A TOKEN SESSION (§5.6, §5.7), under `tokens.lock`, BEFORE ITS
/// FOLD, on this Mac's row of the token ([`row_of_token`]): this Mac's `first_served_at`
/// (journaling `token-served` the first time) and `last_served_at`, the row's label, and, for a
/// token not verified yet that the vault's records put in use here ([`Standing::serves_here`]:
/// made in aterm's window or headless route, on any Mac, or minted here), this Mac's comparison by
/// windows:
///
/// - the basis is what [`note_basis`] recorded from readings of sessions on the row's stored
///   login here, and nothing else: this writes none, so no token reading, this token's or an
///   earlier one's folded into the row, is ever compared with itself;
/// - each reading that carries rate limits is compared with the basis ([`Verdict::of`]) until
///   one comparison had a basis still in a current window, and that verdict is kept
///   ([`Verification::compares`]; `shared` until a basis of a later week reopens it,
///   [`note_basis`]): the reading's seven-day reset must be the basis's, and its five-hour reset
///   too where both are current, and no other account's current windows here may show it. With
///   no such basis the verdict is `no-basis`, which decides nothing: the reading
///   is kept, and the next stored-login reading compares it at once where its week still runs
///   ([`note_basis`]); the next token reading is compared afresh with the basis then recorded,
///   never a kept reading whose week has ended. A reading that names no seven-day window still
///   running compares nothing;
/// - equal windows leave [`Due::Verify`] at this reading and each later one until the vault's
///   meta says `verified`, for the caller to write once the fold is, on whichever Mac compared
///   them (the one that minted the token or another: a basis is recorded only where the
///   account's stored login reports, so the comparing Mac holds that login): the write is a
///   vault write, never made under a store's lock. Different windows are final: this Mac starts
///   no session on the token ([`TokenState::in_use`]), journals `token-differ` once, value-free,
///   and that reading and each later one leave [`Due::Apart`], so the caller folds none of them
///   into the row. Nothing of them reaches the vault: other Macs judge the token by their own
///   readings, and keep using it.
///
/// A reading of a token the vault's records do not put in use here (made at a terminal or by an
/// earlier build on another Mac, and not verified) is [`Due::Apart`]: no session here starts on
/// such a token, and its readings may be another account's.
///
/// A pair that does not read as one token here ([`Check::Readable`]: two Macs' concurrent writes
/// not settled yet, a value or a seal this Mac does not read) records nothing: no row is made,
/// moved to its meta's token or compared, since that meta is its register's winner, which may be
/// of a token the settle drops, and a row moved to a later `minted_at` never moves back
/// ([`row_of_token`]). Its reading is [`Due::Apart`], verified meta or not: a verification in a
/// meta beside two tokens may be of the token a replacement replaced, not the one the session
/// runs on.
///
/// A row-less account's token, or a reading from no roster row, is never compared: it stays
/// `unverified (no roster row)`. A reading of a session whose wrapper named no token
/// ([`Served::named`]) is never compared, and leaves no write. A reading that names no week
/// still running at `now` (one taken before its week reset, handed on later) compares nothing
/// either, and a reading's five-hour reset that has passed is compared with none
/// ([`Windows::shown_by`]): neither says what the token's windows are now. The kept row of a
/// token older than the account's row's ([`Table::replaced`]: this Mac's later write is on its
/// way) takes the reading's served facts and comparison, and leaves [`Due::Apart`] where its
/// windows differ, but never a write: a write over it would only be refused
/// ([`claude_token::verify`]). `standing` is the token's, read value-free before the fold; its
/// caller hands only a reading of a session the wrapper served this very token
/// (`harness_host.rs`' `tap_step`). A token this Mac does not compare (one another Mac minted that
/// is verified, or that serves only its minting Mac until it is) has its served facts written to
/// `tokens.json` alone where the check file does not read ([`update_table_for`]).
///
/// # Errors
/// The table could not be read or written under `tokens.lock`; for a token this Mac compares, the
/// check file did not read, so its verdict is not known and nothing of the reading is folded.
pub fn note_served(
    store: &Store,
    standing: &Standing,
    served: &Served,
    now: u64,
) -> io::Result<Due> {
    let Some(meta) = standing.meta.as_ref() else {
        return Ok(Due::Nothing);
    };
    if standing.check != Check::Readable {
        return Ok(Due::Apart);
    }
    let at = i64::try_from(now).unwrap_or(i64::MAX);
    let (mut first, mut due, mut differ) = (false, Due::Nothing, false);
    update_table_for(store, standing, |table| {
        let (row, held) = row_of_token(table, standing, meta);
        row.label = Some(served.label.clone());
        if row.used_on.first_served_at.is_none() {
            row.used_on.first_served_at = Some(now);
            first = true;
        }
        row.used_on.last_served_at = Some(now);
        if standing.verified() {
            return;
        }
        if !standing.serves_here() {
            due = Due::Apart;
            return;
        }
        let check = &mut row.verification;
        if served.named && check.compares() {
            if !served.reading.week_running(at) {
                return;
            }
            differ = check.compare(served.reading, &served.others, at) == Verdict::Differ;
        }
        due = match check.verdict {
            Some(Verdict::Equal) if served.named && held == Held::Account => {
                Due::Verify(verify_of(meta, check.reading, now))
            }
            Some(Verdict::Differ) => Due::Apart,
            Some(Verdict::Equal | Verdict::Shared | Verdict::NoBasis) | None => Due::Nothing,
        };
    })?;
    if first {
        note(TokenNote::Served, &standing.key, now);
    }
    if differ {
        note(TokenNote::Differ, &standing.key, now);
    }
    Ok(due)
}

/// The evidence of a comparison that found `reading`'s windows equal to its row's basis, for the
/// token `meta` describes, at `now`: what [`verify_served`] writes into the meta's `verified`.
/// It names no Mac: [`claude_token::verify`] names the seat that writes it, read from the log.
fn verify_of(meta: &Meta, reading: Option<Windows>, now: u64) -> Verify {
    let reset = |reset: Option<i64>| reset.and_then(|r| u64::try_from(r).ok());
    Verify {
        minted_at: meta.minted_at,
        evidence: aterm_secrets::claude_token::Verified {
            on: String::new(),
            at: now,
            by: aterm_secrets::claude_token::BY_WINDOWS.to_owned(),
            seven_day_resets_at: reset(reading.and_then(|r| r.week_reset)),
            five_hour_resets_at: reset(reading.and_then(|r| r.session_reset)),
        },
    }
}

/// Whether this Mac's table holds a row of the account `key` that a stored-login reading may give
/// a basis ([`note_basis`]): the account's row, or its kept row ([`Table::replaced`]), not removed,
/// with no final verdict, or an account's row whose write is not yet in vault (the vault may hold
/// an older token, whose kept row the reading makes). Read without `tokens.lock`, as the cheap
/// look before the vault is read for the token's standing at every reading; [`note_basis`] judges
/// the token again, where the vault's standing says whether it is verified, in use here and
/// refused here (a row of a token the vault's records do not put in use here, or one of a token
/// refused here, passes this look, and records nothing). A table that does not read holds none. A
/// token held for its basis has a row, on any Mac ([`note_held`]); where no row awaits (none of
/// the account, or one of an earlier token with its verdict, while another Mac's newer token may
/// be in the vault), the stored login's reading still looks at the vault now and then
/// (`harness_host.rs`' tap), so any Mac where the account's stored login works compares a token
/// another Mac minted.
#[must_use]
pub fn basis_wanted(store: &Store, key: &Key) -> bool {
    read_table(store).ok().is_some_and(|table| {
        table
            .rows
            .get(key.as_str())
            .is_some_and(|row| awaits_a_verdict(row) || row.vault.not_yet())
            || table
                .replaced
                .get(key.as_str())
                .is_some_and(awaits_a_verdict)
    })
}

/// Whether `row` is of a token this Mac has not removed, with nothing final found
/// ([`Verification::is_final`]): the row a
/// stored-login reading gives a basis, unless this Mac saw the token refused, which its standing
/// says ([`Standing::refused_at`]). Whether the token is verified is the vault's meta's word
/// ([`Standing::verified`]), never the row's.
fn awaits_a_verdict(row: &TokenRow) -> bool {
    row.removed_at.is_none() && !row.verification.is_final()
}

/// What a stored-login reading made of its account's token's verification ([`note_basis`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Based {
    /// It recorded nothing.
    Not,
    /// It is the row's basis now. Where a token reading was kept with no basis to compare it
    /// with, it was compared, and found the windows different or shared, or nothing.
    Recorded,
    /// It is the row's basis now, and the token reading kept with no basis showed the same
    /// windows: this Mac writes `verified` for it with this evidence ([`verify_served`]), as for
    /// [`Due::Verify`].
    Verify(Verify),
}

impl Based {
    /// Whether the reading became the row's basis.
    #[must_use]
    pub const fn recorded(&self) -> bool {
        !matches!(self, Self::Not)
    }
}

/// RECORDS A STORED-LOGIN READING AS ITS ACCOUNT'S TOKEN'S BASIS (§5.6), under `tokens.lock`:
/// `reading` is the windows of a status-line reading of a session the harness's attribution
/// credits to the roster row `label` with the certainty its fold credits it, running on that
/// row's stored login (no vault token on its process login); `standing` is the account's token as
/// this Mac's vault reads it, value-free. Where that token is not verified
/// ([`Standing::verified`]), the vault's records put it in use here ([`Standing::serves_here`]:
/// made in aterm's window or headless route on any Mac, or minted here) and this Mac did not see
/// it refused ([`Standing::refused_at`]), and this Mac's row of it ([`row_of_token`]: made or
/// followed where the account's row is of another token, or the kept row of a token older than
/// the row's) awaits a verdict ([`awaits_a_verdict`]), the reading's windows become that row's
/// basis at `now` in place of any earlier one: a newer reading of the stored login is a better
/// basis, and one whose week has ended compares nothing ([`Verdict::of`]). The row's label
/// becomes `label`, as a served reading's does. A reading that names no week still running at
/// `now` records nothing. Nothing ever writes a token session's reading here, so the basis is
/// never a token's own windows.
///
/// A token reading the row kept with no basis to compare it with ([`Verification::reading`],
/// `no-basis`), whose week still runs at `now`, is compared with the new basis at once, with
/// `others`, the other accounts' current windows here, as [`note_served`] compares a token
/// reading that comes after the basis: the token need not serve another session before its
/// verdict. Windows found different are final at once, so this Mac starts no session on the token
/// ([`TokenState::in_use`]) and journals `token-differ`; found equal, on the account's row, they
/// leave [`Based::Verify`] for the caller to write once the fold is, whichever Mac minted the
/// token. A kept reading whose week has ended compares nothing: the next token reading is
/// compared instead. Windows found shared hold for their week: a basis of the same week is
/// recorded and compares nothing, and one of a later week reopens them (the kept reading and its
/// verdict go, the accounts that shared them stay, [`Verification::shared_with`]), so the next
/// token reading is compared afresh, and is equal only where each of those accounts' current
/// windows here are apart from it ([`Verification::compare`]). A `shared` verdict that names no
/// account (an earlier build's) is final, and no basis reopens it ([`Verification::is_final`]).
/// A pair that does not read as one token here ([`Check::Readable`]) records nothing, as in
/// [`note_served`].
///
/// # Errors
/// The table could not be read or written under `tokens.lock`.
pub fn note_basis(
    store: &Store,
    standing: &Standing,
    label: &str,
    reading: &Windows,
    others: &[Other],
    now: u64,
) -> io::Result<Based> {
    let Some(meta) = standing.meta.as_ref() else {
        return Ok(Based::Not);
    };
    let at = i64::try_from(now).unwrap_or(i64::MAX);
    if standing.check != Check::Readable
        || standing.verified()
        || !standing.serves_here()
        || standing.refused_at.is_some()
        || !reading.week_running(at)
    {
        return Ok(Based::Not);
    }
    let (mut based, mut differ) = (Based::Not, false);
    update_table(store, |table| {
        let (row, held) = row_of_token(table, standing, meta);
        if !awaits_a_verdict(row) {
            return;
        }
        row.label = Some(label.to_owned());
        let check = &mut row.verification;
        check.basis = Some((now, *reading));
        based = Based::Recorded;
        if check.verdict == Some(Verdict::Shared) {
            if check.reading.map(|kept| kept.week_reset) == Some(reading.week_reset) {
                return;
            }
            check.verdict = None;
            check.reading = None;
        }
        let Some(kept) = check.reading.filter(|kept| kept.week_running(at)) else {
            return;
        };
        let verdict = check.compare(kept, others, at);
        differ = verdict == Verdict::Differ;
        if verdict == Verdict::Equal && held == Held::Account {
            based = Based::Verify(verify_of(meta, Some(kept), now));
        }
    })?;
    if differ {
        note(TokenNote::Differ, &standing.key, now);
    }
    Ok(based)
}

/// MAKES THIS MAC'S ROW OF A TOKEN HELD FOR ITS BASIS (§5.6), under `tokens.lock`, where the
/// table holds none of it: the token route found the vault's token, `standing`, in use here and
/// awaiting a stored-login reading with no row of its `minted_at` (a token another Mac minted, a
/// mint whose row was not written, or an account's row of another token), and holds it for the
/// roster row `label`'s stored login. The row [`row_of_token`] gives it, under `label` where it
/// names none, is the one [`basis_wanted`] finds, so the stored login's next reading records the
/// basis and the hold lifts, on the Mac that minted the token and on every other alike.
///
/// # Errors
/// The table could not be read or written under `tokens.lock`.
pub fn note_held(store: &Store, standing: &Standing, label: &str) -> io::Result<()> {
    let Some(meta) = standing.meta.as_ref() else {
        return Ok(());
    };
    update_table(store, |table| {
        let (row, _) = row_of_token(table, standing, meta);
        if row.label.is_none() {
            row.label = Some(label.to_owned());
        }
    })
}

/// How long verification's vault write waits for a vault pass to let go: it runs on a session's
/// attribution, so it never waits like a person's verb; the next reading tries again.
const VERIFY_WAIT: Duration = Duration::from_secs(2);

/// WRITES `verified` FOR THE ACCOUNT `key`'S TOKEN (§5.6) with `verify`, the comparison
/// [`note_served`] found equal, through `vault` ([`claude_token::verify`]: the same token and its
/// meta with `verified` set, naming this Mac's seat, in one draft, on whichever Mac compared it,
/// only while the vault still holds the token compared, `verify.minted_at`, and only over the
/// pair it read), then
/// records it in this Mac's table and journals `token-verified`. A write that waits for the vault
/// pass counts as written: it stands on the pair it read, so the pass signs it, or drops it whole
/// where the token or its meta changed by then (a replacement's included), never putting back
/// what it read; where it was dropped and the vault still holds the token, its meta says no
/// `verified`, so the token's next served reading leaves the write due again. A write the vault
/// holds (busy) or refuses records nothing: the next served reading tries again. Returns whether
/// it was written.
///
/// # Errors
/// The table could not be written under `tokens.lock`.
pub fn verify_served(
    store: &Store,
    vault: &dyn TokenVault,
    key: &Key,
    verify: Verify,
    now: u64,
) -> io::Result<bool> {
    let written = vault.verify(
        key,
        verify.minted_at,
        verify.evidence,
        OnBusy::Refuse(Instant::now() + VERIFY_WAIT),
    );
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
        if let Some(row) = table
            .rows
            .get_mut(key.as_str())
            .filter(|row| row.minted_at == Some(verify.minted_at))
        {
            row.verified = Some(now);
        }
    })?;
    note(TokenNote::Verified, key, now);
    Ok(true)
}

/// SETTLES THE ACCOUNT `key`'S SPLIT PAIR (§5.6) through `vault` ([`claude_token::settle`]),
/// where this Mac found it `mismatched (draft)`: two Macs wrote it at once (one's verification
/// beside another's refutation, say), so no Mac serves it until one writes the pair again, in one
/// draft over every write it read. The seat whose turn it is at `now` (unix seconds) writes; any
/// other reads [`Settled::Theirs`] and writes nothing. It waits for a vault pass no longer than a
/// verification does, never mints, and nothing of a value leaves it; one held off settles at a
/// later reading or upkeep.
#[must_use]
pub fn settle_split(vault: &dyn TokenVault, key: &Key, now: u64) -> Settled {
    vault
        .settle(key, now, Instant::now() + VERIFY_WAIT)
        .unwrap_or_else(Settled::Held)
}

/// SETTLES EVERY SPLIT PAIR `listed` HOLDS (§5.6), this Mac's vault listing ([`claude_token::listed`]),
/// through `vault` at `now` (unix seconds): each account whose pair reads `mismatched (draft)` is
/// settled where it is this Mac's turn ([`settle_split`]). The accounts upkeep makes it at every
/// pass that reads which rows run on their token here, so a split pair is settled with no session
/// of its account on any Mac: a reading's tap settles one too (`harness_host.rs`), but a Mac whose
/// turn it is may run none. Returns whether it wrote a draft, so the caller reads the vault again.
#[must_use]
pub fn settle_splits(vault: &dyn TokenVault, listed: &Lookup<Vec<Standing>>, now: u64) -> bool {
    let Lookup::Found(standings) = listed else {
        return false;
    };
    let mut wrote = false;
    for standing in standings {
        if standing.check == Check::Mismatched(Mismatch::Draft) {
            wrote |= matches!(
                settle_split(vault, &standing.key, now),
                Settled::Written(
                    MemberWritten::Posted | MemberWritten::SignedHere | MemberWritten::Waiting(_)
                )
            );
        }
    }
    wrote
}

impl Unverified {
    /// Why, for the token made at `minted_at`, from this Mac's row of the account (`None`: none
    /// kept), at `now`, on whichever Mac this is. With no final verdict, a row of this token with
    /// no current basis ([`Verification::awaits_basis`]) awaits a stored-login reading, windows
    /// found shared in a week that has since ended included; one with a basis awaits a reading of
    /// the token, or holds its `shared` verdict for that basis's week. A verdict the row keeps of another token (another
    /// `minted_at`) says nothing of this one: a token with no row of its own here (one another Mac
    /// minted, a mint whose row is not written yet or could not be, a table an earlier build
    /// rewrote) has had nothing compared here, so it awaits a stored-login reading, and is held
    /// while this account's stored login is usable here (`token_route::standing_judged`), wherever
    /// it was minted.
    #[must_use]
    pub fn of(row: Option<&TokenRow>, minted_at: Option<u64>, now: u64) -> Self {
        let now = i64::try_from(now).unwrap_or(i64::MAX);
        match row {
            Some(row) if row.label.is_none() => Self::NoRosterRow,
            Some(row) if row.minted_at == minted_at => match row.verification.verdict {
                Some(Verdict::Differ) => Self::WindowsDiffer,
                Some(Verdict::Equal) => Self::WindowsEqual,
                // Windows found shared hold for their basis's week. Once it has ended the token
                // awaits a basis of a later week, which only a session on the stored login gives:
                // the hold puts one there, and its reading reopens the comparison.
                _ if row.verification.awaits_basis(now) => Self::AwaitingBasis,
                Some(Verdict::Shared) => Self::WindowsShared,
                Some(Verdict::NoBasis) | None => Self::NoReadingYet,
            },
            Some(_) | None => Self::AwaitingBasis,
        }
    }
}

impl TokenState {
    /// The state of the account whose vault lookup is `found` (this Mac's refusal of the token
    /// included, [`Standing::refused_at`]) and whose table row is `row` (why a token minted here
    /// is not verified yet), at `now` (unix seconds).
    #[must_use]
    pub fn of(found: &Lookup<Standing>, row: Option<&TokenRow>, now: u64) -> Self {
        let standing = match found {
            Lookup::Unknown => return Self::Unknown,
            Lookup::Absent => return Self::None,
            Lookup::Found(standing) => standing,
        };
        let minted_at = standing.meta.as_ref().map(|meta| meta.minted_at);
        match standing.state(now) {
            State::Unreadable => Self::Unreadable,
            State::Mismatched(why) => Self::Mismatched(why),
            State::Ended => Self::Ended,
            State::Rejected => Self::Rejected {
                elsewhere: standing.serves_elsewhere(),
            },
            State::Unverified {
                minted_here,
                minted_by,
                exposed,
            } => Self::Unverified {
                minted_here,
                minted_by,
                why: Unverified::of(row, minted_at, now),
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
        }
    }

    /// Why a bare run of the verb (no `--replace`) is refused in this state, where it is (§4.6):
    /// while this Mac reads the account's token as `active`, `soon`, `exposed` or `unverified`,
    /// wherever it was minted and whatever this Mac's comparison found of it (one whose windows
    /// differ here is refused too, and replaced only on a person's `--replace`); while it reads
    /// it `rejected` and the vault's records put it in use on another Mac (one Mac's refusal is
    /// no other Mac's), or `unreadable here` (this Mac cannot tell what it replaces); or while a
    /// token made here is not yet in vault (`pending`), so a second Mac never replaces a token in
    /// use on every Mac or one that awaits verification on the Mac that made it, and a re-run
    /// never replaces a write still on its way. `expires_at` dates the words.
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
                why: Unverified::WindowsDiffer,
                ..
            } => "unverified and not used here (its windows are not its stored login's here): \
                  this Mac starts no session on it, and it is valid"
                .to_owned(),
            Self::Unverified {
                minted_here: true,
                why: Unverified::AwaitingBasis,
                ..
            } => "unverified (awaiting a reading of its stored login here), and valid".to_owned(),
            Self::Unverified {
                minted_here: true,
                why: Unverified::WindowsShared,
                minted_by,
                ..
            } if !minted_by.is_some_and(MintedBy::proves_account) => {
                "unverified and serving only this Mac (another account here shows the same \
                 windows, so the comparison could not tell the accounts apart), and valid"
                    .to_owned()
            }
            Self::Unverified {
                minted_here: true,
                minted_by,
                ..
            } if minted_by.is_some_and(MintedBy::proves_account) => {
                "unverified, in use on every Mac".to_owned()
            }
            Self::Unverified {
                minted_here: true, ..
            } => "unverified, serving this Mac".to_owned(),
            Self::Unverified {
                minted_on,
                minted_by,
                ..
            } if minted_by.is_some_and(MintedBy::proves_account) => {
                format!("made on {minted_on}, in use on every Mac, and valid")
            }
            Self::Unverified { minted_on, .. } => {
                format!("made on {minted_on}, awaiting verification there, and valid")
            }
            Self::Rejected { elsewhere: true } => {
                "refused by Claude Code on this Mac, and in use on other Macs".to_owned()
            }
            Self::Unreadable => "unreadable here (this Mac cannot open it), and other Macs may \
                                 use it"
                .to_owned(),
            _ => return None,
        };
        Some(format!("a token for {email} is {state}{until}; {replace}"))
    }

    /// The step a surface offers for the account `label` (its row's label, or its email) in this
    /// state (§5.9), where it offers one: for an account whose token is `none`, `ended`,
    /// `mismatched`, or `rejected` and in use on no other Mac, a bare run, and for a token that
    /// is `soon` or `exposed`, `rejected` while in use on other Macs, `unverified (windows
    /// differ)` on the Mac that made it, or `unverified (windows shared)` there where its route
    /// did not prove the account, a `--replace`, so the step always passes the verb's refusal of
    /// a bare run. A token whose windows differ on the Mac that made it is not used there, and a
    /// session running on it moves back to the account's stored login, so it should be replaced;
    /// another Mac's finding is its own and offers nothing, since a replacement replaces the
    /// token on every Mac. One made at a person's terminal, or whose meta names no route, whose
    /// windows another account here shares could not be told from that account's: it serves
    /// only the Mac that made it and is never verified, so its replacement is compared afresh.
    /// None for `unknown`, `active`, `unreadable here` (this Mac's vault, not the token, is what
    /// to mend), any other `unverified` token wherever it was minted (its verification is still
    /// to come, or its route put it in use on every Mac), or while a token made here is not yet
    /// in vault (`pending`).
    #[must_use]
    pub fn offer(&self, label: &str, pending: bool) -> Option<Step> {
        if pending {
            return None;
        }
        let replace = match self {
            Self::None
            | Self::Ended
            | Self::Rejected { elsewhere: false }
            | Self::Mismatched(_) => false,
            Self::Soon
            | Self::Exposed(_)
            | Self::Rejected { elsewhere: true }
            | Self::Unverified {
                minted_here: true,
                why: Unverified::WindowsDiffer,
                ..
            } => true,
            Self::Unverified {
                minted_here: true,
                why: Unverified::WindowsShared,
                minted_by,
                ..
            } if !minted_by.is_some_and(MintedBy::proves_account) => true,
            Self::Unknown | Self::Active | Self::Unreadable | Self::Unverified { .. } => {
                return None;
            }
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
    /// Write the token's meta again with `verified` set ([`claude_token::verify`]), where the
    /// vault's token is still the one made at `minted_at`.
    ///
    /// # Errors
    /// As that call.
    fn verify(
        &self,
        key: &Key,
        minted_at: u64,
        evidence: aterm_secrets::claude_token::Verified,
        on_busy: OnBusy,
    ) -> Result<MemberWritten, String>;
    /// Settle concurrent writes of the account `key`'s pair ([`claude_token::settle`]) where it
    /// is this Mac's turn at `now` (unix seconds), waiting for a vault pass no longer than `by`. A
    /// stand-in finds nothing to settle.
    ///
    /// # Errors
    /// As that call.
    fn settle(&self, key: &Key, now: u64, by: Instant) -> Result<Settled, String> {
        let _ = (key, now, by);
        Ok(Settled::Nothing)
    }
    /// Post this Mac's records again to every server it reaches, as a write's own post does
    /// ([`aterm_secrets::client::post_records_to_servers`], idempotent): whether a server took
    /// them. A vault that reaches no server takes nothing.
    ///
    /// # Errors
    /// As that call.
    fn post(&self) -> Result<bool, String> {
        Ok(false)
    }
    /// Ask the lab for each of `keys`' tokens through this Mac's need ledger
    /// ([`aterm_secrets::need::ask_for_tokens`]: raised unless a need of the want is under way or
    /// the latest was withdrawn by hand): how many it raised. A stand-in raises none.
    ///
    /// # Errors
    /// As that call.
    fn ask(&self, keys: &[Key]) -> Result<usize, String> {
        let _ = keys;
        Ok(0)
    }
    /// The HTTP status Claude Code refused the token `standing` reads with on this Mac, as this
    /// Mac recorded it ([`claude_token::refused_status`]): `None` where it recorded no refusal
    /// of that token, or none with a status. A stand-in recorded none.
    fn refused_status(&self, standing: &Standing) -> Option<u16> {
        let _ = standing;
        None
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
        minted_at: u64,
        evidence: aterm_secrets::claude_token::Verified,
        on_busy: OnBusy,
    ) -> Result<MemberWritten, String> {
        claude_token::verify(&self.0, key, minted_at, evidence, on_busy).map_err(|e| e.to_string())
    }
    fn settle(&self, key: &Key, now: u64, by: Instant) -> Result<Settled, String> {
        claude_token::settle(&self.0, key, now, by).map_err(|e| e.to_string())
    }
    fn post(&self) -> Result<bool, String> {
        aterm_secrets::client::post_records_to_servers(&self.0)
            .map(|posted| posted > 0)
            .map_err(|e| e.to_string())
    }
    fn ask(&self, keys: &[Key]) -> Result<usize, String> {
        aterm_secrets::need::ask_for_tokens(&self.0, keys)
            .map(|raised| raised.len())
            .map_err(|e| e.to_string())
    }
    fn refused_status(&self, standing: &Standing) -> Option<u16> {
        claude_token::refused_status(&self.0, standing)
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
    /// How the verb names it (its token is made by itself in aterm's sign-in window, and its
    /// lines may reach a log or a captured standard error): its row's label, never its address.
    fn quietly(&self) -> String {
        self.label
            .clone()
            .unwrap_or_else(|| "this account".to_owned())
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

/// Whether the Claude row `label`'s next sign-in is a person's at this Mac on its records
/// ([`super::accounts_sync::left_to_a_person`]), for a run with nobody at this terminal (a run of
/// a person's there asks nothing of this). Records this run cannot read hold the row, as a
/// check's record that cannot be read holds its page: what stands on them is unknown, and a check
/// may be among it (`docs/DESIGN-claude-sign-in-sitting-2026-11-12.md` §3). A row its roster no
/// longer has holds nothing.
fn left_to_a_person_here(env: &Env, store: &accounts::Store, label: &str, now: i64) -> bool {
    let Some(path) = env.accounts_roster_for(Cli::Claude) else {
        return true;
    };
    store
        .read_account_records(Some(&path), now)
        .map_or(true, |records| {
            records.raw_roster.row(label).is_some_and(|raw| {
                super::accounts_sync::left_to_a_person(store, raw, &records.logins)
            })
        })
}

/// The refusal of a run with nobody at this terminal for the account whose token step a check's
/// record holds ([`token_steps::Stopped::holds`]: the page a capture loaded carried Cloudflare's
/// check or block page or a human check, and only a person's act at a terminal on this Mac runs
/// the step over it), or whose steps file does not read, which holds every step; `None` where
/// neither stands.
fn check_record_refusal(store: &Store, account: &Account) -> Option<String> {
    let steps = match token_steps::read(store) {
        Ok(steps) => steps,
        Err(why) => {
            return Some(format!(
                "{}: {why}; while it does not read, every token step stays held, and nothing is \
                 made for {}",
                token_steps::STEPS_FILE,
                account.quietly()
            ));
        }
    };
    let stopped = steps
        .get(account.key.as_str())
        .filter(|stopped| stopped.word.is_bot_protection())?;
    let step = account.label.as_ref().map(|label| {
        Step::Token {
            label: label.clone(),
            replace: false,
        }
        .to_string()
    });
    Some(format!(
        "the token step for {} {}; nothing was made",
        account.quietly(),
        stopped.words(step.as_deref())
    ))
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
    // A run nobody at a terminal of their own started (an agent's, a script's) is told so in
    // aterm's window, which then ends at the read that finds any check instead of waiting on it.
    let via = if super::cli::person_at_terminal(host) {
        Via::Desk
    } else {
        Via::Unattended
    };
    run_with_clock(
        env,
        host,
        name,
        mode,
        (via, None),
        out,
        err,
        Instant::now,
        &mut |_| {},
    )
}

/// Where a run's token page opens ([`run_with_clock`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// The verb's own route for a person at a terminal of their own on this Mac
    /// ([`super::cli::person_at_terminal`]): aterm's sign-in window at this Mac's own desk, and
    /// nowhere else, which waits for the person on a page that carries a check.
    Desk,
    /// The account's own headless browser profile, one a headless sign-in proved (plan package
    /// FL-137): nobody is asked, and no window opens.
    Headless,
    /// Aterm's sign-in window at this Mac's own desk, opened with nobody at a terminal of their
    /// own: by this Mac's upkeep (plan package FL-138, [`backfill_run`]), the token step after a
    /// sign-in nobody at a terminal started ([`after_sign_in`]), or an agent's or a script's
    /// run of the verb. The window is told so ([`setup_token::OpenerRoute::unattended`]): it ends
    /// at the read that finds any check, and a code page whose code no mailbox the window reads
    /// gives ends the run at once.
    Unattended,
}

impl Via {
    /// Whether the run's page opens in aterm's sign-in window.
    const fn window(self) -> bool {
        matches!(self, Self::Desk | Self::Unattended)
    }
}

/// The most a token's capture on the account's headless profile takes from its start to its
/// end: the opener's own drive ([`super::open_url::AUTHORIZE_TIMEOUT`]) twice over, for the
/// browser's start and the CLI's own exchange around it.
const HEADLESS_ROUTE_WITHIN: Duration = Duration::from_secs(3 * 60);

#[allow(
    clippy::too_many_arguments,
    reason = "the verb's inputs, its route, both streams, its clock and its caller's hold, each \
              one its own"
)]
fn run_with_clock(
    env: &Env,
    host: &dyn AccountsHost,
    name: &str,
    mode: TokenMode,
    (via, upkeep): (Via, Option<&Finding>),
    out: &mut dyn Write,
    err: &mut dyn Write,
    now: impl FnOnce() -> Instant,
    unrecorded: &mut dyn FnMut(super::accounts_sync::PendingCheck),
) -> ExitCode {
    let refuse = |err: &mut dyn Write, why: &str| {
        let _ = writeln!(err, "aterm harness accounts token: {why}");
        ExitCode::from(1)
    };
    // The token's page opens through aterm's opener and nowhere else: nobody pastes a code. At
    // this Mac's own desk it opens in aterm's own sign-in window, which authorizes it by itself;
    // right after a headless sign-in, on the profile it proved. Only a Mac's own desk opens that
    // window; elsewhere (an SSH session, another Mac's request, a tab another Mac's drive started
    // or typed into) no page of it could be seen, and the token one Mac's desk makes is in the
    // vault, which serves it to every Mac once it is verified (§5.6). Removal opens nothing: it
    // is that window's or a person's at a terminal of their own, on any system.
    let window = via.window() && host.opens_sign_in_window();
    if matches!(mode, TokenMode::Remove) {
        if !window && !super::cli::person_at_terminal(host) {
            return refuse(
                err,
                "a token is removed at this Mac's own desktop or by a person at its terminal \
                 (not for another Mac or in a tab another Mac's drive started or typed into)",
            );
        }
    } else if !host.on_macos() {
        return refuse(
            err,
            "a token is made only on macOS, where aterm's wrapper serves it",
        );
    } else if via.window() && !window {
        return refuse(
            err,
            "a token is made in aterm's own sign-in window, which opens only at this Mac's own \
             desktop (not over SSH, for another Mac, or in a tab another Mac's drive started or \
             typed into)",
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
    // A row whose sign-in a bot protection stopped, or whose renewal is a person's, is not sent
    // to claude.ai's page with nobody at this terminal, in aterm's window or anywhere else: an
    // agent's run, another Mac's request and the upkeep's own make no token for it
    // (docs/DESIGN-claude-sign-in-sitting-2026-11-12.md §3), and the person's step is said.
    if !matches!(mode, TokenMode::Remove)
        && !super::cli::person_at_terminal(host)
        && let Some(label) = account.label.as_deref()
        && left_to_a_person_here(env, &store, label, host.now().max(env.now))
    {
        return refuse(err, &super::cli::left_words(Cli::Claude, label));
    }
    // Nor is the account's token page loaded again with nobody at this terminal where its token
    // step's own record of a check stands: only a person's act at a terminal on this Mac runs
    // the step over it. This Mac's upkeep judges the record with every other of its own
    // ([`step_held`]).
    if !matches!(mode, TokenMode::Remove)
        && upkeep.is_none()
        && !super::cli::person_at_terminal(host)
        && let Some(why) = check_record_refusal(&store, &account)
    {
        return refuse(err, &why);
    }
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
                &format!(
                    "a token for {} is being made in another tab",
                    account.quietly()
                ),
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
    let table = read_table(&store).ok();
    let standing_wait = if matches!(mode, TokenMode::Remove) {
        STANDING_LOOK
    } else {
        REQUIRED_STANDING_HANG
    };
    let found = match vault.standing(&account.key, Instant::now() + standing_wait) {
        Ok(found) => found,
        Err(why) => return refuse(err, &format!("the vault could not be read: {why}")),
    };
    // This Mac's facts of the vault's token: the account's row, or the kept row of the token a
    // later write of this Mac's replaces.
    let row = table
        .as_ref()
        .and_then(|table| table.row_for(account.key.as_str(), &found))
        .cloned();
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
    let state = TokenState::of(&found, row.as_ref(), now);
    if state == TokenState::Unknown {
        return refuse(err, "the vault could not be read just now; try again");
    }
    // The token the vault holds at the preflight: the mint keeps its row by later reads of the
    // vault, and by this one only where those say nothing ([`record`]).
    let holds = made_at(&found);
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
    // A run this Mac's upkeep started (FL-138) makes the token it found missing and no other:
    // where the vault's token moved since, under this account's lock (another tab or Mac made
    // one, the refusal no longer stands, a write of this Mac's waits), it makes nothing; nor
    // where the account's step is held now (a capture another run held this lock for, a
    // person's or the row's own sign-in's, stopped at a check after the upkeep looked).
    if let Some(finding) = upkeep {
        let cli = host.setup_token_program().map(|(_, version)| version);
        if pending
            || holds != finding.minted_at
            || gap_of(&state, &found, vault.as_ref(), (true, now, cli.as_deref()))
                != Some(finding.gap)
        {
            return refuse(
                err,
                &format!(
                    "the vault's token for {} changed after this Mac's upkeep found it missing; \
                     nothing was made",
                    account.quietly()
                ),
            );
        }
        if step_held(
            env,
            host,
            &store,
            &account,
            (&found, finding.gap),
            cli.as_deref(),
        ) {
            return refuse(
                err,
                &format!(
                    "the token step for {} was held after this Mac's upkeep found it missing \
                     (a capture stopped where only a person goes on); nothing was made",
                    account.quietly()
                ),
            );
        }
        // The record that this gap ran is written before its capture starts, under this lock:
        // no page opens for a run this Mac could not record, so a record file that cannot be
        // written never lets the next pass run the gap again ([`token_steps::Tried`]).
        // `backfill_run` rewrites it as the capture ended, or clears it once the token is made.
        let ran = token_steps::Tried::new(
            (finding.gap, finding.minted_at),
            now,
            cli.as_deref().unwrap_or_default(),
            &aterm_stamp::build_id(),
        );
        if let Err(why) = token_steps::put_tried(&store, account.key.as_str(), Some(&ran)) {
            return refuse(
                err,
                &format!(
                    "{}: {why}; nothing was made for {}",
                    token_steps::TRIED_FILE,
                    account.quietly()
                ),
            );
        }
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
    let page = match via {
        Via::Headless => setup_token::OpenerPage::Headless {
            label: account.label.clone(),
        },
        Via::Desk | Via::Unattended => setup_token::OpenerPage::Window,
    };
    // The opener answers in two files of a private folder this run alone holds.
    let route = match super::cli::opener_link(&store, host) {
        Ok(opener) => aterm_tempfile::tempdir().ok().map(|dir| {
            (
                setup_token::OpenerRoute {
                    opener,
                    page,
                    unattended: via == Via::Unattended,
                    outcome: dir.path().join("outcome"),
                    code: dir.path().join("outcome.code"),
                },
                dir,
            )
        }),
        Err(why) => return refuse(err, &format!("{why}; nothing was made")),
    };
    let Some((route, _held)) = route else {
        return refuse(err, "no private folder could be made for aterm's opener");
    };
    // The opener's route ends by itself: its bound is its own.
    let within = match route.page {
        setup_token::OpenerPage::Window => super::open_url::WINDOW_ROUTE_WITHIN,
        setup_token::OpenerPage::Headless { .. } => HEADLESS_ROUTE_WITHIN,
    };
    let sign_in = host.token_sign_in(&route);
    let checked = CheckedRow::before(
        env,
        &store,
        (account.label.as_deref(), account.key.as_str()),
        now,
    );
    let code = mint(
        (&store, vault.as_ref(), sign_in.as_ref()),
        (&account, &program, cli_version, &env_vars, &route),
        (
            host.token_hang().min(within),
            host.token_write_wait(),
            now,
            holds,
        ),
        (out, err),
    );
    if let Some(checked) = checked {
        if code == ExitCode::SUCCESS {
            checked.end_after(&store, account.key.as_str(), now);
        } else if let Some(pending) = checked.hold_after(&store, account.key.as_str(), now, err) {
            unrecorded(pending);
        }
    }
    code
}

/// The Claude row a token's capture runs for, as it stood before the capture
/// ([`CheckedRow::before`]): where the capture's page carried a bot protection, the row's
/// account is held for a person as a sign-in a check stopped is ([`CheckedRow::hold_after`]),
/// and where the capture made the token, the account's check its step's earlier check wrote
/// ends with the step's record ([`CheckedRow::end_after`]): one check, one lifetime.
struct CheckedRow {
    roster_path: PathBuf,
    row: accounts::Row,
    generation: Option<u64>,
    /// When the account's token step's own record of a check stood before the capture, where
    /// one did ([`token_steps::Stopped`] of a bot protection).
    step_check_at: Option<u64>,
}

impl CheckedRow {
    /// The row `label` of Claude Code's roster, its renewal generation and its account `key`'s
    /// token step's record of a check, before a capture runs: a login made for the row after
    /// this ends any check the capture records. `None` where the account has no row or the
    /// records do not read.
    fn before(
        env: &Env,
        store: &Store,
        (label, key): (Option<&str>, &str),
        now: u64,
    ) -> Option<Self> {
        let roster_path = env.accounts_roster_for(Cli::Claude)?;
        let records = store
            .read_account_records(Some(&roster_path), i64::try_from(now).unwrap_or(i64::MAX))
            .ok()?;
        let row = records.raw_roster.row(label?)?.clone();
        let generation = records
            .logins
            .get(&row.label)
            .map(|binding| binding.renewal_generation);
        let step_check_at = token_steps::read(store).ok().and_then(|steps| {
            steps
                .get(key)
                .filter(|stopped| stopped.word.is_bot_protection())
                .map(|stopped| stopped.at)
        });
        Some(Self {
            roster_path,
            row,
            generation,
            step_check_at,
        })
    }

    /// Where the capture that started at `now` made the token over a check's record of its step
    /// (a person's own run at a terminal on this Mac: no other run passes such a record), ends
    /// the account's [`accounts::CheckStop`] that record wrote ([`Self::hold_after`]) too: the
    /// person passed the very page that carried it, as the token's record says by clearing
    /// ([`token_steps::put`]). Only that check ends: one whose second is not the record's (a
    /// sign-in's check met since, which [`accounts::note_check_stop`] keeps as the later) stands,
    /// and so does this one while the step's record of it does (its clearing did not land).
    fn end_after(&self, store: &Store, key: &str, now: u64) {
        let cleared = || {
            token_steps::read(store)
                .is_ok_and(|steps| steps.get(key).map(|stopped| stopped.at) != self.step_check_at)
        };
        if let Some(at) = self.step_check_at
            && cleared()
        {
            super::accounts_sync::end_token_check(
                store,
                &self.roster_path,
                &self.row,
                (
                    i64::try_from(at).unwrap_or(i64::MAX),
                    i64::try_from(now).unwrap_or(i64::MAX),
                ),
            );
        }
    }

    /// Where the capture that started at `now` left a bot protection's record of the account
    /// `key`'s step ([`token_steps::Stopped`]: Cloudflare's check or block page, a human check,
    /// headless or in aterm's window), records it as the row's account's [`accounts::CheckStop`]
    /// too (`accounts_sync::write_check_stop`), so every door with nobody at this Mac holds the
    /// row and its token step for a person (`accounts_sync::left_to_a_person`) by the one rule a
    /// check's hold ends by: a login made for the row, a signed-in `accounts browser` window, or,
    /// for the step's own record, this Mac's token made after it. The run holds the account's
    /// lock, so the record read is this capture's. Where the account's records stay busy, the
    /// check is answered (and said on `err`) for the caller to hold in its process until it is on
    /// record (the upkeep's `AutoRenewGuard::unrecorded_checks`), as a sign-in's door does.
    fn hold_after(
        &self,
        store: &Store,
        key: &str,
        now: u64,
        err: &mut dyn Write,
    ) -> Option<super::accounts_sync::PendingCheck> {
        let at = token_steps::read(store).ok().and_then(|steps| {
            steps
                .get(key)
                .filter(|stopped| stopped.word.is_bot_protection() && stopped.at >= now)
                .map(|stopped| stopped.at)
        })?;
        let check = super::accounts_sync::PendingCheck {
            roster_path: self.roster_path.clone(),
            row: self.row.clone(),
            generation: self.generation,
            at: i64::try_from(at).unwrap_or(i64::MAX),
        };
        if super::accounts_sync::write_check_stop(store, &check)
            != super::accounts_sync::CheckWrite::Busy
        {
            return None;
        }
        let _ = writeln!(
            err,
            "aterm harness accounts token: {}: the check its page carried could not be recorded \
             on the account (its records stayed busy); the token step's own record holds its \
             token step",
            self.row.label
        );
        Some(check)
    }
}

/// Where the sign-in a run just made of a row was made, which says where its token is made
/// ([`after_sign_in`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignedIn {
    /// In aterm's own sign-in window, in the account's own store, whose session is the row's.
    Window,
    /// Headlessly, by aterm's opener, on a browser profile of aterm's own (a renewal nobody is
    /// at the window for, this Mac's upkeep of a row with a proven profile).
    Headless,
    /// Anywhere else (a person's browser, another browser after the headless try): no browser
    /// session aterm knows to be the row's.
    Elsewhere,
}

/// MAKES AN ACCOUNT'S YEAR-LONG TOKEN IN THE RUN THAT JUST SIGNED IT IN: where `label`'s account
/// has no token in the vault, or only one whose end has passed, and this run could make one
/// ([`step_due_after`]), the verb runs with every refusal of its own, and its exit is the answer;
/// `None` where no token is made, with nothing written, a token the vault holds that only a
/// person replaces included. The caller holds no sign-in lock (§4.6: a token is made under its
/// account's lock, never `signin.lock`), and says where the row's sign-in was made
/// (`signed_in`):
///
/// - in aterm's sign-in window, the token's page opens there too, in the account's own store,
///   whose session is the row's, with nobody asked ([`setup_token::OpenerPage::Window`]; one
///   window at a time, so it waits for another sign-in's), as the bare verb ([`run`]) does;
/// - headlessly, the token's page opens on the account's own headless profile, the second
///   pinned request on the profile that sign-in proved (plan package FL-137;
///   [`setup_token::OpenerPage::Headless`]), where `Authorize` is pressed only on a page that
///   shows the account; with no such profile it is offered instead;
/// - anywhere else no browser session is known to be the row's, and a token made now could be
///   another account's (Claude Code's success names none): the step is said once, on `out`,
///   where the bare verb would make one, and nothing is run or written.
///
/// Who is there decides how the window's page runs, not where the sign-in ran: a sign-in a
/// person at a terminal of their own on this Mac ran in aterm's window ([`super::cli::person_at_terminal`])
/// makes the token there as the bare verb does ([`Via::Desk`]), and a record a stop left
/// ([`token_steps`]) does not hold its run: the person may complete a check there, and aterm
/// presses nothing on one. Any other run (this Mac's upkeep, an agent's or a script's renewal)
/// makes it there with nobody there ([`Via::Unattended`]: the window ends at the read that finds
/// any check), and a headless sign-in on the account's headless profile, where nobody is either:
/// on both, a record of a check holds the run ([`check_holds`]) and the step is offered instead,
/// since only a person's act at a terminal on this Mac goes back to a page that carried one
/// (`docs/DESIGN-claude-sign-in-sitting-2026-11-12.md` §3). A run's capture replaces the record
/// or clears it. Where the capture's check could not be recorded on the account, `unrecorded`
/// is handed it, for a caller that keeps such a check in its process.
pub(super) fn after_sign_in(
    env: &Env,
    host: &dyn AccountsHost,
    label: &str,
    signed_in: SignedIn,
    (out, err): (&mut dyn Write, &mut dyn Write),
    unrecorded: &mut dyn FnMut(super::accounts_sync::PendingCheck),
) -> Option<ExitCode> {
    let via = match signed_in {
        SignedIn::Window if super::cli::person_at_terminal(host) => Via::Desk,
        SignedIn::Window => Via::Unattended,
        SignedIn::Headless => Via::Headless,
        SignedIn::Elsewhere => {
            offer_after_sign_in(env, host, label, out);
            return None;
        }
    };
    let Some(due) = step_due_after(env, host, label, via) else {
        if via == Via::Headless {
            offer_after_sign_in(env, host, label, out);
        }
        return None;
    };
    if via != Via::Desk && check_holds(&due) {
        let where_ = if via == Via::Headless {
            "on the headless browser profile"
        } else {
            "in aterm's sign-in window, and nobody at this Mac's terminal started this run"
        };
        let _ = writeln!(
            out,
            "{label}: its token's consent page carried a check the last time it was tried, so \
             this run makes none {where_}; `{}` run by a person at this Mac makes one",
            Step::Token {
                label: label.to_owned(),
                replace: false,
            }
        );
        return None;
    }
    let _ = match via {
        Via::Desk | Via::Unattended => writeln!(
            out,
            "{label}: it has no year-long token in use here, so this run makes one, as `aterm \
             harness accounts token {label}` does"
        ),
        Via::Headless => writeln!(
            out,
            "{label}: it has no year-long token in use here, so this run makes one on the \
             headless browser profile it just signed in on"
        ),
    };
    let _ = out.flush();
    Some(run_with_clock(
        env,
        host,
        label,
        TokenMode::Make,
        (via, None),
        out,
        err,
        Instant::now,
        unrecorded,
    ))
}

/// Says the step once, on `out`, where the bare verb would make `label`'s token and the run that
/// signed it in makes none ([`after_sign_in`]); nothing is run or written.
fn offer_after_sign_in(env: &Env, host: &dyn AccountsHost, label: &str, out: &mut dyn Write) {
    if step_due_after(env, host, label, Via::Desk).is_none() {
        return;
    }
    let _ = writeln!(
        out,
        "{label}: it has no year-long token in use here; this run signed it in neither in \
         aterm's sign-in window nor on a headless browser profile a sign-in proved, so it makes \
         none: run `{}`",
        Step::Token {
            label: label.to_owned(),
            replace: false,
        }
    );
}

/// What this Mac's upkeep finds of an account's token that it makes in its place with nobody
/// there (plan package FL-138, [`backfill_due`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gap {
    /// The vault holds no token of the account.
    None,
    /// The vault's token's end has passed.
    Ended,
    /// Thirty days or fewer remain of it ([`claude_token::SOON_SECS`], by its own end, verified
    /// or not), and this Mac is the account's home Mac: the first of its warm Macs, so one Mac
    /// of the fleet replaces it, not each one. A Mac that is no account's home (no pool names
    /// one) replaces none: the token serves until it ends, and its end is [`Self::Ended`].
    Soon,
    /// Claude Code refused it on this Mac with a `401`
    /// ([`super::token_route::REFUSED_STATUS`]), as the refusal this Mac recorded says.
    Refused,
    /// This Mac minted it, and its meta names no route that minted it: only this Mac starts
    /// sessions on it until it is verified, where one minted by aterm's window or headless route
    /// serves every Mac ([`MintedBy::serves_unverified`]).
    Unrouted,
}

impl Gap {
    /// The verb's mode that fills the gap: a token where the vault holds none in use, one in the
    /// place of the token it holds otherwise.
    const fn mode(self) -> TokenMode {
        match self {
            Self::None | Self::Ended => TokenMode::Make,
            Self::Soon | Self::Refused | Self::Unrouted => TokenMode::Replace,
        }
    }

    /// The gap's word, as the upkeep's log names it.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Ended => "ended",
            Self::Soon => "ends soon",
            Self::Refused => "refused here",
            Self::Unrouted => "minted here by no route",
        }
    }

    /// Every gap.
    pub const ALL: [Self; 5] = [
        Self::None,
        Self::Ended,
        Self::Soon,
        Self::Refused,
        Self::Unrouted,
    ];

    /// Its closed word in the upkeep's record of the gaps it ran a capture for
    /// ([`token_steps::Tried`]).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Ended => "ended",
            Self::Soon => "soon",
            Self::Refused => "refused",
            Self::Unrouted => "unrouted",
        }
    }

    /// The gap `word` is, as [`Self::as_str`] writes it.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|gap| gap.as_str() == word)
    }

    /// Whether any Mac signed in to the account may find it, so the account's home Mac takes
    /// the first turn at it ([`super::accounts_sync`]'s token upkeep): no token, one that ended,
    /// or one Claude Code refused (every Mac that serves a revoked token is refused). `soon` is
    /// the home Mac's alone, and `unrouted` the minting Mac's.
    #[must_use]
    pub const fn any_mac(self) -> bool {
        matches!(self, Self::None | Self::Ended | Self::Refused)
    }
}

/// The longest lifetime, from its mint to its end, of a token the upkeep calls short-lived:
/// twice [`claude_token::SOON_SECS`]. Claude Code prints the lifetime it made (`valid for N
/// days`, [`setup_token::valid_for`]); a token that lives this long or less is `soon` within
/// its first month, so replacing it as it nears its end, or after it ends, with the Claude Code
/// build that made it would make another such token on that token's clock, without end. The
/// upkeep replaces a short-lived token only with another Claude Code build ([`gap_of`]).
pub const SHORT_LIVED_SECS: u64 = 2 * claude_token::SOON_SECS;

/// Whether `meta`'s token is short-lived ([`SHORT_LIVED_SECS`]) and was made by the Claude Code
/// build `cli` (the one a capture would run now): its end, or its nearing end, is no gap the
/// upkeep fills. A build that is not known on either side counts as the same.
fn short_lived_by(meta: &Meta, cli: Option<&str>) -> bool {
    let same_build = match cli {
        Some(cli) if !cli.is_empty() && !meta.cli.is_empty() => cli == meta.cli,
        _ => true,
    };
    same_build && meta.expires_at.saturating_sub(meta.minted_at) <= SHORT_LIVED_SECS
}

/// An account's token gap this Mac's upkeep found ([`backfill_due`]): the account's key, the gap,
/// the `minted_at` of the token the vault holds (`None`: it holds none), and the route that fills
/// it with nobody there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// The account's key (`ac-<16 hex>`).
    pub key: String,
    /// What is missing.
    pub gap: Gap,
    /// The token the vault holds, by its `minted_at`.
    pub minted_at: Option<u64>,
    /// Where the token's page opens.
    pub via: Via,
}

/// The routes of an account's token page that need no person (plan package FL-138): aterm's
/// sign-in window, where its store holds the account's session or it reads the account's
/// mailbox, and the account's headless profile a sign-in proved. The caller knows them from the
/// account's store and this Mac's mailbox configuration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NobodyRoutes {
    /// Aterm's sign-in window finishes the page with nobody there.
    pub window: bool,
    /// The account's headless profile does.
    pub headless: bool,
}

/// What [`backfill_due`] decides of an account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Backfill {
    /// Its token is missing, and this Mac's upkeep makes it ([`backfill_run`]).
    Due(Finding),
    /// This Mac is not seated in the vault's log, so it stores no token: nothing is due until
    /// it is.
    NotSeated,
    /// Nothing is due.
    Nothing,
}

/// WHETHER THIS MAC'S UPKEEP MAKES `label`'S TOKEN WITH NOBODY THERE, AND WHY (plan package
/// FL-138): on macOS, on the default route, with a route of the page that needs no person
/// (`routes`: aterm's window where this run may open it, else the account's headless profile),
/// where this Mac writes its vault and no write of this Mac's of the token waits, and this Mac's
/// reading of the vault's token ([`TokenState::of`]) is a gap ([`Gap`]):
///
/// - `none` or `ended`: a token is made;
/// - a token whose own end is within 30 days, verified or not, by the account's home Mac alone
///   (`home_mac`): one is made in its place;
/// - `rejected` where Claude Code refused it here with a `401` (a `403`, Cloudflare's block page,
///   is no refusal of the token, and a refusal an earlier build recorded names no status):
///   one is made in its place;
/// - `unverified`, minted here by no route this build knows: one is made in its place, which
///   every Mac then serves.
///
/// A short-lived token ([`SHORT_LIVED_SECS`]) made by the Claude Code build a capture would run
/// now is no gap as it nears its end or once it has ended: that build would make another. Any
/// other reading is no gap: a `mismatched` draft is the vault's to settle
/// ([`rows_on_token_settled`]), and a token that does not read (`unreadable here`, `unknown`)
/// or is in use, another Mac's unverified window or headless token included, is left alone. A
/// gap is not due either where the account's step is held ([`step_held`]): a record left by a
/// capture that a check, or what another try alone does not pass, stopped
/// ([`token_steps::Stopped::holds`]), or this Mac's upkeep's record that it already ran a capture
/// for this gap of this token, whatever that capture ended in ([`token_steps::Tried::holds`]),
/// is held until the Claude Code build a capture would run or this aterm's build differs from the
/// one that ran it, the account's window store or a mailbox configuration file is written after
/// it ([`token_steps::route_at`]), or a token for the account is made after it; a check's record
/// by none of these, only by a person's act at a terminal on this Mac. A stop at a code
/// page nobody typed a code on is also let through by aterm's window reading the account's
/// mailbox now ([`token_steps::mailbox_read`]); the upkeep's record of the run is not. No time
/// alone lets a step through, no restart of this process does, and a file of those records that
/// does not read holds every step. Nor is a gap due where the row's next sign-in is a person's
/// at this Mac on its records (`accounts_sync::left_to_a_person`: a bot protection stopped a
/// sign-in of its account, or its renewal is a person's task): the verb refuses that row to a
/// run with nobody there, so the upkeep spends no sign-in spacing and leaves no record on it. A
/// quick look ([`STANDING_LOOK`]) that writes nothing; anything it cannot read is not due.
pub fn backfill_due(
    env: &Env,
    host: &dyn AccountsHost,
    label: &str,
    home_mac: bool,
    routes: NobodyRoutes,
) -> Backfill {
    let via = if routes.window && host.opens_sign_in_window() {
        Via::Unattended
    } else if routes.headless {
        Via::Headless
    } else {
        return Backfill::Nothing;
    };
    if !host.on_macos() || setup_token::route_in(&host.verb_env()).is_some() {
        return Backfill::Nothing;
    }
    let Ok(account) = account_of(env, label, &host.session_logins(env)) else {
        return Backfill::Nothing;
    };
    let (Some(store), Some(vault)) = (env.accounts_store_for(Cli::Claude), host.token_vault())
    else {
        return Backfill::Nothing;
    };
    let by = || Instant::now() + STANDING_LOOK;
    match vault.route(by()) {
        Ok(WriteRoute::Seated) => {}
        Ok(WriteRoute::NotSeated) => return Backfill::NotSeated,
        Ok(WriteRoute::NoLog | WriteRoute::Busy) | Err(_) => return Backfill::Nothing,
    }
    let Ok(found) = vault.standing(&account.key, by()) else {
        return Backfill::Nothing;
    };
    if vault.waiting_here(&account.key).unwrap_or(true) {
        return Backfill::Nothing;
    }
    let table = read_table(&store).ok();
    let row = table
        .as_ref()
        .and_then(|table| table.row_for(account.key.as_str(), &found));
    let now = u64::try_from(host.now().max(env.now)).unwrap_or(0);
    let state = TokenState::of(&found, row, now);
    let cli = host.setup_token_program().map(|(_, version)| version);
    let Some(gap) = gap_of(
        &state,
        &found,
        vault.as_ref(),
        (home_mac, now, cli.as_deref()),
    ) else {
        return Backfill::Nothing;
    };
    if step_held(env, host, &store, &account, (&found, gap), cli.as_deref()) {
        return Backfill::Nothing;
    }
    // The verb refuses this row to a run with nobody there (`run_with_clock`), so nothing is
    // due: the upkeep spaces no capture for it and records no run of it.
    if account
        .label
        .as_deref()
        .is_some_and(|label| left_to_a_person_here(env, &store, label, host.now().max(env.now)))
    {
        return Backfill::Nothing;
    }
    Backfill::Due(Finding {
        key: account.key.as_str().to_owned(),
        gap,
        minted_at: made_at(&found),
        via,
    })
}

/// The gap this Mac's reading `state` of the vault's token `found` is ([`backfill_due`]), where
/// it is one, at `now`, on the account's home Mac where `home_mac`, `cli` being the Claude Code
/// build a capture would run. Whether the token nears its end is read off its own end, before
/// its verification: [`claude_token::Standing::state`] says `soon` of a verified token alone,
/// and a token the upkeep makes is not verified until a comparison verifies it, which may never
/// come.
fn gap_of(
    state: &TokenState,
    found: &Lookup<Standing>,
    vault: &dyn TokenVault,
    (home_mac, now, cli): (bool, u64, Option<&str>),
) -> Option<Gap> {
    let meta = match found {
        Lookup::Found(standing) => standing.meta.as_ref(),
        Lookup::Absent | Lookup::Unknown => None,
    };
    let short_lived = meta.is_some_and(|meta| short_lived_by(meta, cli));
    let soon =
        meta.is_some_and(|meta| meta.expires_at.saturating_sub(now) <= claude_token::SOON_SECS);
    match state {
        TokenState::None => Some(Gap::None),
        TokenState::Ended => (!short_lived).then_some(Gap::Ended),
        TokenState::Rejected { .. } => match found {
            Lookup::Found(standing)
                if vault.refused_status(standing) == Some(super::token_route::REFUSED_STATUS) =>
            {
                Some(Gap::Refused)
            }
            Lookup::Found(_) | Lookup::Absent | Lookup::Unknown => None,
        },
        TokenState::Unverified {
            minted_here: true,
            minted_by: None,
            ..
        } => Some(Gap::Unrouted),
        TokenState::Soon | TokenState::Unverified { .. } | TokenState::Exposed(_) if soon => {
            (home_mac && !short_lived).then_some(Gap::Soon)
        }
        TokenState::Unknown
        | TokenState::Unreadable
        | TokenState::Mismatched(_)
        | TokenState::Unverified { .. }
        | TokenState::Exposed(_)
        | TokenState::Soon
        | TokenState::Active => None,
    }
}

/// WHETHER `account`'S TOKEN STEP IS HELD, FOR THE GAP `gap` OF THE VAULT'S TOKEN `found`, `cli`
/// being the Claude Code build a capture would run now: a stop's record holds it (FL-136,
/// [`token_steps::Stopped::holds`]), or this Mac's upkeep's record that it ran a capture for that
/// gap of that token does ([`token_steps::Tried::holds`]): each until something it waits for
/// changes. A stop at a code page nobody typed a code on is also let through by aterm's window
/// reading the account's mailbox now ([`token_steps::mailbox_read`], the machine read through
/// [`AccountsHost::machine_var`]); the upkeep's record of the gap's run is not, since the vault's
/// mailbox entries date nothing and a run that ended there left that record as surely as the
/// stop. Neither record is kept in memory, so no restart of this process lets a step through.
/// A file of either that does not read holds it.
fn step_held(
    env: &Env,
    host: &dyn AccountsHost,
    store: &Store,
    account: &Account,
    (found, gap): (&Lookup<Standing>, Gap),
    cli: Option<&str>,
) -> bool {
    let (Ok(steps), Ok(tried)) = (token_steps::read(store), token_steps::read_tried(store)) else {
        return true;
    };
    let key = account.key.as_str();
    let token_at = made_at(found);
    let stopped = steps.get(key);
    let ran = tried
        .get(key)
        .filter(|ran| ran.gap == gap && ran.minted_at == token_at);
    if stopped.is_none() && ran.is_none() {
        return false;
    }
    let aterm = aterm_stamp::build_id();
    let var = |name: &str| host.machine_var(name);
    let since = token_steps::Since {
        cli,
        aterm: Some(&aterm),
        route_at: token_steps::route_at(store, env.home.as_deref(), &account.email),
        token_at,
        mailbox_read: stopped
            .is_some_and(token_steps::Stopped::waits_on_a_mailbox)
            .then(|| {
                token_steps::mailbox_read(
                    store,
                    (env.home.as_deref(), &var),
                    account.label.as_deref(),
                    &account.email,
                )
            }),
    };
    stopped.is_some_and(|stopped| stopped.holds(&since)) || ran.is_some_and(|ran| ran.holds(&since))
}

/// MAKES THE TOKEN THIS MAC'S UPKEEP FOUND MISSING, WITH NOBODY THERE (plan package FL-138): where
/// `label`'s account is still due as `finding` says ([`backfill_due`], judged again now), the verb
/// runs by the finding's route with every refusal of its own, in the gap's mode (a token where
/// the vault holds none in use, else one in the place of the one it holds), and its exit is the
/// answer; `None` where nothing is due now, with nothing written. Under the account's lock the
/// run makes the token only where the vault still holds the token the finding names, with the
/// same gap. In aterm's window the run is unattended ([`Via::Unattended`]): a code page whose code
/// no mailbox the window reads gives ends it at once, and its stop is recorded like any other a
/// check made ([`token_steps`]), so nothing presses or waits on a page meant for a person. A run
/// that made no token, whatever it ended in, leaves this Mac's record that the gap was run
/// ([`token_steps::Tried`], keyed by the account, the gap and the token's `minted_at`, never by
/// the route): [`backfill_due`] holds that gap until something the record waits for changes, in
/// this process and the next; a run that made the token clears it. The record is written under
/// the account's lock before the capture starts, so a run whose record cannot be written opens
/// no page, and a capture cut short (the process killed) leaves it. The caller holds this Mac's
/// sign-in spacing ([`super::sign_in_ladder::SPACING_S`]) and no `signin.lock` (a token is made
/// under its account's lock).
#[allow(
    clippy::too_many_arguments,
    reason = "the account, what the upkeep found of it and how, both streams and the upkeep's \
              hold, each its own"
)]
pub(super) fn backfill_run(
    env: &Env,
    host: &dyn AccountsHost,
    label: &str,
    finding: &Finding,
    home_mac: bool,
    routes: NobodyRoutes,
    (out, err): (&mut dyn Write, &mut dyn Write),
    unrecorded: &mut dyn FnMut(super::accounts_sync::PendingCheck),
) -> Option<ExitCode> {
    match backfill_due(env, host, label, home_mac, routes) {
        Backfill::Due(now) if now == *finding => {}
        Backfill::Due(_) | Backfill::NotSeated | Backfill::Nothing => return None,
    }
    let store = env.accounts_store_for(Cli::Claude)?;
    let _ = writeln!(
        out,
        "{label}: its year-long token is {}, so this Mac's upkeep makes one, as `aterm harness \
         accounts token {label}` does",
        finding.gap.word()
    );
    let _ = out.flush();
    let cli = host
        .setup_token_program()
        .map(|(_, version)| version)
        .unwrap_or_default();
    let now = u64::try_from(host.now().max(env.now)).unwrap_or(0);
    let started = Instant::now();
    let code = run_with_clock(
        env,
        host,
        label,
        finding.gap.mode(),
        (finding.via, Some(finding)),
        out,
        err,
        Instant::now,
        unrecorded,
    );
    let ran = (code != ExitCode::SUCCESS).then(|| {
        token_steps::Tried::new(
            (finding.gap, finding.minted_at),
            stop_second(now, started),
            &cli,
            &aterm_stamp::build_id(),
        )
    });
    if let Err(why) = token_steps::put_tried(&store, &finding.key, ran.as_ref()) {
        let _ = writeln!(
            err,
            "aterm harness accounts token: {}: {why}",
            token_steps::TRIED_FILE
        );
    }
    Some(code)
}

/// Whether a check holds `due`'s step for a run nobody at a terminal started, on the account's
/// headless profile or in aterm's window: the record a capture left is a bot protection's
/// ([`super::open_url::MetWord::is_bot_protection`]), which only a person's act at a terminal on
/// this Mac passes, and this Mac's token made clears ([`token_steps::Stopped::holds`]). A steps
/// file that does not read holds it.
fn check_holds(due: &DueAfter) -> bool {
    token_steps::read(&due.store).map_or(true, |steps| {
        steps
            .get(due.key.as_str())
            .is_some_and(|stopped| stopped.word.is_bot_protection())
    })
}

/// What [`step_due_after`] found of a step that is due: the accounts store and the account's key.
struct DueAfter {
    store: Store,
    key: String,
}

/// When the token `found` names was made, where the vault holds one with its meta.
fn made_at(found: &Lookup<Standing>) -> Option<u64> {
    match found {
        Lookup::Found(standing) => standing.meta.as_ref().map(|meta| meta.minted_at),
        Lookup::Absent | Lookup::Unknown => None,
    }
}

/// Whether `label`'s token step is due in the run that signed it in, on the route `via`: by the
/// verb's own route, this Mac may open its sign-in window; on the headless route, the account has
/// the headless profile a sign-in proved ([`super::headless_oauth::has_browser_profile`]). Either
/// way this is macOS on the default route, this Mac writes its vault, none made here still waits,
/// and the account's state, judged by this Mac's facts of the vault's token ([`Table::row_for`]),
/// is `none` or `ended`: a gap to fill. A token the vault holds that this Mac reads otherwise
/// (`rejected`, `unreadable here`, `mismatched`, or in use) may be every other Mac's: the run
/// leaves it to this Mac's upkeep, which replaces the few it may ([`backfill_due`]), and to a
/// person, whom the verb's own surface offers the step ([`TokenState::offer`]). A quick look
/// ([`STANDING_LOOK`]) that reads and writes nothing else; anything it cannot read is not due
/// (`None`).
fn step_due_after(env: &Env, host: &dyn AccountsHost, label: &str, via: Via) -> Option<DueAfter> {
    if (via.window() && !host.opens_sign_in_window())
        || !host.on_macos()
        || setup_token::route_in(&host.verb_env()).is_some()
    {
        return None;
    }
    let account = account_of(env, label, &host.session_logins(env)).ok()?;
    let (Some(store), Some(vault)) = (env.accounts_store_for(Cli::Claude), host.token_vault())
    else {
        return None;
    };
    if via == Via::Headless
        && !super::headless_oauth::has_browser_profile(&store.dir, label, Some(&account.email))
    {
        return None;
    }
    let by = || Instant::now() + STANDING_LOOK;
    if !vault
        .route(by())
        .is_ok_and(|route| route_refusal(route).is_none())
    {
        return None;
    }
    let found = vault.standing(&account.key, by()).ok()?;
    let table = read_table(&store).ok();
    let row = table
        .as_ref()
        .and_then(|table| table.row_for(account.key.as_str(), &found));
    let now = u64::try_from(host.now().max(env.now)).unwrap_or(0);
    (!vault.waiting_here(&account.key).unwrap_or(true)
        && matches!(
            TokenState::of(&found, row, now),
            TokenState::None | TokenState::Ended
        ))
    .then(|| DueAfter {
        key: account.key.as_str().to_owned(),
        store,
    })
}

/// What [`mint`] makes a token of: the account, the Claude Code build and its version, the
/// verb's environment, and aterm's opener, through which the capture's page opens.
type MintInputs<'a> = (
    &'a Account,
    &'a Program,
    String,
    &'a [(std::ffi::OsString, std::ffi::OsString)],
    &'a setup_token::OpenerRoute,
);

/// The steps after the preflight: the capture, the write, the row, the journal, the outcome.
fn mint(
    (store, vault, sign_in): (&Store, &dyn TokenVault, &dyn SignIn),
    (account, program, cli_version, env_vars, route): MintInputs<'_>,
    (hang, write_wait, now, holds): (Duration, Option<Duration>, u64, Option<u64>),
    (out, err): (&mut dyn Write, &mut dyn Write),
) -> ExitCode {
    let refuse = |err: &mut dyn Write, why: &str| {
        let _ = writeln!(err, "aterm harness accounts token: {why}");
        ExitCode::from(1)
    };
    let deadline = WakeDeadline::after(hang);
    let page = &route.page;
    sign_in.say(&match page {
        setup_token::OpenerPage::Window => format!(
            "making a Claude Code token for {} in aterm's sign-in window",
            account.quietly()
        ),
        setup_token::OpenerPage::Headless { .. } => format!(
            "making a Claude Code token for {} on its headless browser profile",
            account.quietly()
        ),
    });
    let root = store
        .shared_dir()
        .join(TOKENS_DIR)
        .join(account.key.as_str());
    let started = Instant::now();
    let minted = setup_token::capture(
        &Capture {
            program,
            store: &root,
            sign_in_as: &account.email,
            env: env_vars,
            opener: route,
            deadline,
        },
        sign_in,
    );
    // What the capture reached updates the account's step record (FL-136): a stop that holds
    // the step replaces it, and a token made, which passed the page, clears it. Any other end
    // (a browser that did not start, an interrupt, the capture's own hang detector, a token of
    // another account) leaves it as it was: the stop it records still stands until an event
    // releases it.
    let stopped = held_by(&minted).and_then(|word| {
        token_steps::Stopped::new(
            word,
            stop_second(now, started),
            &cli_version,
            &aterm_stamp::build_id(),
        )
    });
    if stopped.is_some() || minted.is_ok() {
        hold(store, &account.key, stopped.as_ref(), err);
    }
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
                    "Claude Code made a token for another account than {}'s; nothing was stored",
                    account.quietly()
                ),
            );
        }
        Err(why) => {
            let mut line = match &why {
                // The opener's own record says where and why, for this run's words alone: the
                // step's record keeps its word.
                setup_token::Error::Opener(word) => opener_words(
                    &route.page,
                    &super::open_url::read_outcome(&route.outcome)
                        .filter(|met| met.word == *word)
                        .map_or_else(|| word.words(), |met| met.words()),
                ),
                _ => capture_words(&why),
            };
            if let Some(stopped) = &stopped {
                line.push_str(&held_words(account, stopped));
            }
            return refuse(err, &line);
        }
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
        // aterm's window presses Authorize only on a consent page that names the row's account,
        // or names none in a store its own acts proved is that account's (the mailbox's code or
        // link, never a code a person typed: `signin_window`); the headless route presses it only
        // on a page that shows the account's address and no human check
        // (`headless_token_consent`); and both refuse a token for another (`WrongAccount`
        // above), so either mint proved the account.
        minted_by: Some(match page {
            setup_token::OpenerPage::Window => MintedBy::Window,
            setup_token::OpenerPage::Headless { .. } => MintedBy::Headless,
        }),
        verified: None,
    };
    // The token the vault holds just before the write, read after the capture's minutes: the one a
    // standing read before the write lands names, so the one whose row a landed write keeps
    // ([`record`]). The preflight's word stands only where this read says nothing.
    let before = vault_holds(
        vault
            .standing(&account.key, Instant::now() + STANDING_LOOK)
            .ok()
            .as_ref(),
    )
    .unwrap_or(holds);
    let patience = write_wait.map_or(deadline, WakeDeadline::after);
    let written = store_patiently(vault, &account.key, &minted.token, &meta, sign_in, patience);
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
    // The vault's token once the write has returned, read before the row. Where it is another
    // token than this one (the write waits, or another Mac's took its place), the row the mint
    // keeps is that token's ([`record`]): a first `--replace`'s write may have landed while this
    // one's waited, and its token been found `differ` since. Where it is this one, the write
    // landed, and the row kept is that of the token the vault held just before it, which
    // standings read before it landed still name. Where this read says nothing, that one too.
    let after = match &written {
        MemberWritten::Posted
        | MemberWritten::SignedHere
        | MemberWritten::Waiting(_)
        | MemberWritten::Overtaken(_) => vault
            .standing(&account.key, Instant::now() + STANDING_LOOK)
            .ok(),
        MemberWritten::Refused(_)
        | MemberWritten::Busy
        | MemberWritten::NotSeated
        | MemberWritten::NoLog => None,
    };
    let holds = match vault_holds(after.as_ref()) {
        Some(Some(at)) if at == meta.minted_at => before,
        Some(after) => after,
        None => before,
    };
    let vault_col = match &written {
        MemberWritten::Posted => InVault::InVault,
        MemberWritten::SignedHere => InVault::SignedHere,
        MemberWritten::Waiting(why) => InVault::Waiting(waits_words(*why).to_owned()),
        MemberWritten::Overtaken(by) => {
            let by = by
                .as_ref()
                .map_or_else(|| "another Mac".to_owned(), |by| by.label.clone());
            if let Err(why) = record(store, account, &meta, InVault::Overtaken(by.clone()), holds) {
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
    // Until the row is written, a session served the new token finds no row of it, which holds
    // it for its basis ([`Unverified::of`]). Its signer is the read's, where it read this token.
    let recorded = record(store, account, &meta, vault_col.clone(), holds);
    if recorded.is_ok()
        && matches!(vault_col, InVault::InVault | InVault::SignedHere)
        && let Some(Lookup::Found(standing)) = after
        && standing.meta.as_ref().map(|m| m.minted_at) == Some(meta.minted_at)
        && let Some(signer) = standing.signer
    {
        let _ = update_table(store, |table| {
            if let Some(row) = table
                .rows
                .get_mut(account.key.as_str())
                .filter(|row| row.minted_at == Some(meta.minted_at))
            {
                row.signer = Some(signer);
            }
        });
    }
    note(
        TokenNote::Minted(meta.expiry_from),
        &account.key,
        meta.minted_at,
    );
    let valid = valid_words(minted_lifetime(&meta), meta.expires_at);
    let held = account.held_note();
    let line = match &vault_col {
        InVault::InVault if meta.minted_by.is_some_and(MintedBy::proves_account) => format!(
            "stored a Claude Code token for {} in the vault, {valid}; it serves every Mac, each \
             one where the account's stored login is usable once a session on that login there \
             has reported its windows{held}",
            account.email
        ),
        InVault::InVault => format!(
            "stored a Claude Code token for {} in the vault, {valid}; it serves this Mac once a \
             session on the account's stored login here has reported its windows, and every Mac \
             once it is verified{held}",
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
    if short_lived_by(&meta, Some(&meta.cli)) {
        let _ = writeln!(
            out,
            "it lasts {} days or less, so the upkeep replaces it only once another Claude Code \
             build is in atpkg's store",
            SHORT_LIVED_SECS / 86_400
        );
    }
    if let Err(why) = recorded {
        let _ = writeln!(
            err,
            "aterm harness accounts token: the token is in the vault's hands, but {TABLE_FILE} \
             was not updated: {why}"
        );
    }
    ExitCode::SUCCESS
}

/// What a capture that ended as `end` holds the account's token step on (FL-136): the closed
/// word of what its opener met, in aterm's sign-in window or on the account's headless profile
/// (FL-137), where another try alone does not pass it
/// ([`super::open_url::MetWord::holds_a_token_step`]), the window's own deadline among them
/// (`window-deadline`, which the window's opener records where the window stood until it);
/// `None` for any other end, a token included. The capture's own hang detector running out
/// (`Error::Hang`) holds nothing on either route: it says only that Claude Code made no token
/// in time, never what a page met, and it runs out where the window never opened (Claude Code
/// showed no address to open, the opener never started) as surely as where it did.
fn held_by(end: &Result<Minted, setup_token::Error>) -> Option<super::open_url::MetWord> {
    match end {
        Err(setup_token::Error::Opener(word)) if word.holds_a_token_step() => Some(*word),
        _ => None,
    }
}

/// The second a capture that started at `started`, in a run whose clock read `now` before it,
/// ended: never earlier than this Mac's clock reads now, so a file the capture itself wrote is
/// never read as written after its stop ([`token_steps::Stopped::holds`]).
fn stop_second(now: u64, started: Instant) -> u64 {
    aterm_time::unix_secs()
        .unwrap_or(0)
        .max(now.saturating_add(started.elapsed().as_secs()))
}

/// Leaves the account `key`'s step record as a capture that just ended says (FL-136): `stopped`,
/// journaled `token-stopped` with its word, or none, for a capture that made a token. What
/// cannot be written is said on `err`; the capture's own outcome stands either way.
fn hold(store: &Store, key: &Key, stopped: Option<&token_steps::Stopped>, err: &mut dyn Write) {
    if let Err(why) = token_steps::put(store, key.as_str(), stopped) {
        let _ = writeln!(
            err,
            "aterm harness accounts token: {}: {why}",
            token_steps::STEPS_FILE
        );
    }
    if let Some(stopped) = stopped {
        note(TokenNote::Stopped(stopped.why()), key, stopped.at);
    }
}

/// What a refusal adds where its stop holds the account's step: what tries it again
/// ([`token_steps::Stopped::again`]).
fn held_words(account: &Account, stopped: &token_steps::Stopped) -> String {
    let step = account.label.as_ref().map(|label| {
        Step::Token {
            label: label.clone(),
            replace: false,
        }
        .to_string()
    });
    format!("; {}", stopped.again(step.as_deref()))
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

/// The verb's words for a capture whose page aterm's opener opened at `page` and that ended on
/// what the opener met, `met` being that in words.
fn opener_words(page: &setup_token::OpenerPage, met: &str) -> String {
    match page {
        setup_token::OpenerPage::Window => {
            format!(
                "the sign-in in aterm's sign-in window did not complete: {met}; nothing was made"
            )
        }
        setup_token::OpenerPage::Headless { .. } => format!(
            "the token's consent page on the account's headless browser profile did not \
             complete: {met}; nothing was made"
        ),
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
        Error::Opener(word) => format!(
            "the sign-in aterm's opener ran did not complete: {}; nothing was made",
            word.words()
        ),
        Error::NoToken => "Claude Code ended without showing a token".to_owned(),
        Error::TwoTokens => "Claude Code showed two different tokens; nothing was kept".to_owned(),
        Error::Unchecked => {
            "Claude Code showed a token before the sign-in it asked for; nothing was kept"
                .to_owned()
        }
        Error::WrongAccount(_) => "Claude Code made a token for another account".to_owned(),
        Error::Failed(failure) => match failure {
            Failure::CodeIncomplete => {
                "Claude Code did not take the code aterm's opener handed over; nothing was made"
                    .to_owned()
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

/// Writes both entries as a member write, never throwing the made token away while the vault can
/// take it (§4.6): each try waits a tick for a vault pass to let go ([`OnBusy::Refuse`], which
/// leaves nothing behind); once `deadline` passes, or at Ctrl-C, the write lands in the vault's
/// waiting writes ([`OnBusy::Wait`], `Waiting{LockDeadline}`), for the next vault pass, as soon
/// as this Mac's other writers of its vault let it read the log and land, which it waits for up
/// to a minute for each of the two locks they hold. One line says it waits.
///
/// # Errors
/// The vault's error, one where another writer of this Mac's vault held a lock past that minute
/// included: nothing is stored then.
pub fn store_patiently(
    vault: &dyn TokenVault,
    key: &Key,
    token: &SecretBytes,
    meta: &Meta,
    sign_in: &dyn SignIn,
    deadline: WakeDeadline,
) -> Result<MemberWritten, String> {
    let mut said = false;
    loop {
        if sign_in.stopped() || deadline.passed() {
            return vault.write(key, token, meta, OnBusy::Wait(Instant::now()));
        }
        let by = Instant::now() + deadline.remaining().min(WRITE_TICK);
        match vault.write(key, token, meta, OnBusy::Refuse(by))? {
            MemberWritten::Busy => {
                if !said {
                    sign_in.say("waiting for the vault (a vault pass is running)");
                    said = true;
                }
            }
            written => return Ok(written),
        }
    }
}

/// Records the account's row after a mint: every owner-named field and the vault column (the
/// signer is set after). The row it replaces, of another token, is kept ([`Table::replaced`])
/// where it is of the token the vault holds (`holds`: the vault's token once the mint's write
/// returned, where that is another than the mint's own; else, the write having landed or that
/// read saying nothing, the one the vault held just before the write; the preflight's word only
/// where neither read said anything), and so is the row kept before it where that one is: the
/// vault holds that token until the new one lands (a write that waits), and a standing read
/// before it landed still names it, so it is judged by its own facts here meanwhile. Which
/// token the vault holds is the vault's word, never the earlier row's `vault` column, which says
/// what the vault said of that row's write when it was made. A token the vault does not hold
/// keeps no row.
fn record(
    store: &Store,
    account: &Account,
    meta: &Meta,
    vault: InVault,
    holds: Option<u64>,
) -> io::Result<()> {
    let key = account.key.as_str().to_owned();
    update_table(store, |table| {
        let earlier = table.rows.insert(
            key.clone(),
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
                signer: None,
                used_on: UsedOn::default(),
                vault,
                verified: None,
                verification: Verification::default(),
                removed_at: None,
            },
        );
        let held = |row: &TokenRow| {
            row.minted_at.is_some() && row.minted_at == holds && holds != Some(meta.minted_at)
        };
        let kept = table.replaced.remove(&key);
        if let Some(kept) = earlier.filter(held).or_else(|| kept.filter(held)) {
            table.replaced.insert(key, kept);
        }
    })
}

/// Which token a read of the vault's standing (`read`, `None` where it failed) says the vault
/// holds: `Some(Some(minted_at))` for one whose meta opens here, `Some(None)` for none, and `None`
/// where the read says nothing (it failed or could not be settled, or the meta does not open).
fn vault_holds(read: Option<&Lookup<Standing>>) -> Option<Option<u64>> {
    match read? {
        Lookup::Found(standing) => standing.meta.as_ref().map(|meta| Some(meta.minted_at)),
        Lookup::Absent => Some(None),
        Lookup::Unknown => None,
    }
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
    /// `token-verified`: this Mac verified the token's account by its windows.
    Verified,
    /// `token-differ`: this Mac's comparison found the token's windows another account's.
    Differ,
    /// `wrong-account`: the capture made a token for another account.
    WrongAccount,
    /// `token-offered`: a surface offered a token for the account, once per state pair.
    Offered,
    /// `token-stopped`, with the stop's word ([`token_steps::Stopped::why`]).
    Stopped(&'static str),
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
        TokenNote::Differ => journal::token_note(TokenEvent::Differ, account, t, None),
        TokenNote::WrongAccount => journal::wrong_token_account(account, t),
        TokenNote::Offered => journal::token_note(TokenEvent::Offered, account, t, None),
        TokenNote::Stopped(why) => journal::token_note(TokenEvent::Stopped, account, t, Some(why)),
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
/// use here with its record naming the row's email ([`super::token_route::standing_judged`]).
/// Renewal is off for them: the upkeep never signs one in by itself, and `accounts renew <label>`
/// says so. A row with no login made here is one too (FL-135) where this Mac's claims cover its
/// account ([`super::claims::covers`]: a pool file, or a lab naming this Mac its home): its fleet
/// token runs it ([`accounts::LoginHere::OnToken`]), so the ladder signs it in nowhere and leaves
/// no login that would enrol it. Where they do not, nothing would keep a second Mac off the
/// account, so it is none, and its sign-in goes on as any row's. Empty where the vault could not
/// be read just now (renewal then goes on as before, which signs in nothing a token would have
/// served).
#[must_use]
pub fn rows_on_token(
    paths: &Paths,
    store: &Store,
    roster: &Roster,
    now: i64,
) -> BTreeMap<String, u64> {
    rows_on_token_read(paths, store, roster, now).unwrap_or_default()
}

/// [`rows_on_token`], or `None` where the vault could not be read just now: for a reader that
/// must not take a row for one on its stored login while it does not know (the machine sweep,
/// `docs/DESIGN-account-discovery-2026-11-09.md` step 4).
#[must_use]
pub fn rows_on_token_read(
    paths: &Paths,
    store: &Store,
    roster: &Roster,
    now: i64,
) -> Option<BTreeMap<String, u64>> {
    rows_on_token_of(
        paths,
        claude_token::listed(paths).unwrap_or(Lookup::Unknown),
        store,
        roster,
        now,
    )
}

/// [`rows_on_token_read`] as the accounts upkeep reads it at every pass (`accounts_sync.rs`'
/// `tokens_here`), with no session of any account needed: every split pair the vault's listing
/// holds is settled first where it is this Mac's turn ([`settle_splits`]), and where one was
/// written the vault is listed again, so the rows a settled pair now serves read as on their
/// token at this pass.
#[must_use]
pub fn rows_on_token_settled(
    paths: &Paths,
    store: &Store,
    roster: &Roster,
    now: i64,
) -> Option<BTreeMap<String, u64>> {
    let list = || claude_token::listed(paths).unwrap_or(Lookup::Unknown);
    let mut listed = list();
    if settle_splits(
        &Vault(paths.clone()),
        &listed,
        u64::try_from(now).unwrap_or(0),
    ) {
        listed = list();
    }
    rows_on_token_of(paths, listed, store, roster, now)
}

/// [`rows_on_token_read`] over a listing of the vault at `paths` already read (`listed`, as
/// [`claude_token::listed`] returns it): `None` where it is unknown. `paths` names the home whose
/// stored logins the judgment's hold reads ([`super::token_route::standing_judged`]); no vault is
/// read here.
#[must_use]
pub fn rows_on_token_of(
    paths: &Paths,
    listed: Lookup<Vec<Standing>>,
    store: &Store,
    roster: &Roster,
    now: i64,
) -> Option<BTreeMap<String, u64>> {
    let mut out = BTreeMap::new();
    let standings = match listed {
        Lookup::Found(standings) => standings,
        Lookup::Absent => return Some(out),
        Lookup::Unknown => return None,
    };
    let now = u64::try_from(now).unwrap_or(0);
    let enrolled = accounts::enrolled(roster);
    for row in &roster.rows {
        let (Some(entry), Some(email), true) = (
            accounts::TokenEntry::of_row(row),
            row.expected_identity_email(),
            enrolled.contains(&row.label) || super::claims::covers(&paths.home, Cli::Claude, row),
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
        if super::token_route::standing_judged(
            &found,
            paths,
            store,
            Some(roster),
            &standing.key,
            &account,
            now,
        ) == super::token_route::TokenFact::InUse
            && let Some(meta) = standing.meta.as_ref()
        {
            out.insert(row.label.clone(), meta.expires_at);
        }
    }
    Some(out)
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
    let mut out: Vec<SessionLogin> = Vec::new();
    for dir in session_login_dirs(store, roster, now) {
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

/// The logins the sessions on this Mac run on, whose records [`session_logins_at`] reads: the
/// machine's default login (`None`, which every session on no row runs on), then the login
/// directory of each row a session recorded in the accounts state of `store` is on, the rows read
/// from the roster at `roster`, each once.
#[must_use]
pub fn session_login_dirs(
    store: Option<&Store>,
    roster: Option<&std::path::Path>,
    now: i64,
) -> Vec<Option<String>> {
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
    dirs
}

#[cfg(test)]
#[path = "accounts_tokens_tests.rs"]
pub(in crate::harness) mod tests;
