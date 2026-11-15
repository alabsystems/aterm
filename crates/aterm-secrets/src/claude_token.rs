// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code setup-tokens in the fleet vault (`docs/DESIGN-claude-setup-token-2026-11-05.md`
//! §4.1–§4.3, slice S4): the typed calls that write, read, judge, verify and remove one account's
//! token, the only code that reads or writes the class ([`crate::entry_class::claude_token`]).
//!
//! An account has two `[everyone]` entries, both `Consumed`: `aterm-claude-token-<key>`, the
//! token, and `aterm-claude-token-<key>-meta`, a value-free JSON record ([`Meta`]). `<key>` is the
//! lab's canonical account key for the Claude realm, `ac-<16 hex>` ([`Key`]). Both are written in
//! one draft (one actor, one `authored_ms`, one epoch) by a member write, so they exist only in a
//! vault's log; each value carries the inner seal at the wire
//! (`membership_log::entries::inner`), so a build that does not know the class holds only
//! ciphertext. Neither ever becomes a file, enters a vault text or prints: `secrets get` refuses
//! the class, and `ls --describe` names no file for it.
//!
//! A stored token is in use ([`Standing::serves_here`], [`State`]) when both names' winning
//! `Put`s come from one draft, the value is a setup-token ([`value_grammar`]) and its end
//! ([`Meta::expires_at`]) has not passed, on every member Mac from the moment it is minted where
//! its mint proved the account: aterm's window or headless route ([`MintedBy::proves_account`])
//! presses `Authorize` only in the account's own session. A token made at a person's terminal, or
//! one whose meta names no route (an earlier build's, or one an earlier build wrote again), is in
//! use only on the Mac that minted it (its own seat signed the earliest `Put` of the value) until
//! it is verified ([`Meta::verification`]: a comparison with the account's stored login, never an
//! earlier build's stamp from a usage sample), then on every Mac. Verification is a label: it
//! widens a terminal token's use and never withdraws a token. A refusal of the token (Claude Code
//! refused it on this Mac, [`note_refused`], kept beside this Mac's vault state), or a comparison
//! that found its windows another account's (the harness's table there), is the observing Mac's
//! alone, and the vault holds neither: a token refused on this Mac is in use nowhere here until a
//! newer one replaces it. The signer is read from the log, never from the meta, and is shown,
//! never used as a gate. Its exposure is read from the lineage: every epoch that sealed a `Put` of
//! the value, and whether one was handed to a Mac the log now removes.

use crate::client::{
    MemberEntry, MemberUnset, MemberWritten, OnBusy, Paths, member_unset_names, member_waits_here,
    member_write, read_claude_token_until, recipient_label, seat_label,
};
use crate::entry_class::{CLAUDE_TOKEN_META, CLAUDE_TOKEN_PREFIX};
pub use crate::entry_class::{MintedBy, Mismatch};
use crate::membership_log::entries::inner::{self, Form};
use crate::membership_log::{Log, MemberId};
use crate::text::Audience;
use crate::{Result, SecretBytes};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

/// What every setup-token begins with.
pub const VALUE_PREFIX: &str = "sk-ant-oat";

/// How long a token stays `soon` before its end: 30 days.
pub const SOON_SECS: u64 = 30 * 86_400;

/// How long [`standing`] waits for a vault writer holding this Mac's state before it reads
/// [`Lookup::Unknown`].
const STANDING_WAIT: Duration = Duration::from_secs(2);

/// One account's key in the Claude realm: `ac-<16 lower-case hex>`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key(String);

impl Key {
    /// The key `key` spells, where it is one. A key never ends in the meta suffix: the class's
    /// grammar strips it from an entry name, and a key that kept it would name the meta record.
    #[must_use]
    pub fn parse(key: &str) -> Option<Self> {
        (!key.ends_with(CLAUDE_TOKEN_META)
            && crate::entry_class::claude_token(&format!("{CLAUDE_TOKEN_PREFIX}{key}")))
        .then(|| Self(key.to_owned()))
    }

    /// The key of the token entry `name` (not its meta, whose stripped key [`Self::parse`]
    /// refuses), where it is one.
    #[must_use]
    pub fn of_entry(name: &str) -> Option<Self> {
        Self::parse(name.strip_prefix(CLAUDE_TOKEN_PREFIX)?)
    }

    /// The key as it is spelled.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The token entry's name.
    #[must_use]
    pub fn entry(&self) -> String {
        format!("{CLAUDE_TOKEN_PREFIX}{}", self.0)
    }

    /// The meta entry's name.
    #[must_use]
    pub fn meta_entry(&self) -> String {
        format!("{CLAUDE_TOKEN_PREFIX}{}{CLAUDE_TOKEN_META}", self.0)
    }
}

/// Whether `value` is a setup-token, and nothing else: `sk-ant-oat\d\d-[A-Za-z0-9_-]+`, with no
/// newline, no space and no second line.
#[must_use]
pub fn value_grammar(value: &[u8]) -> bool {
    let Some(rest) = value.strip_prefix(VALUE_PREFIX.as_bytes()) else {
        return false;
    };
    match rest {
        [a, b, b'-', body @ ..] => {
            a.is_ascii_digit()
                && b.is_ascii_digit()
                && !body.is_empty()
                && body
                    .iter()
                    .all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'-')
        }
        _ => false,
    }
}

/// Where a token's end date came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpiryFrom {
    /// Claude Code printed or stored the server's answer.
    Printed,
    /// The mint time plus a year (31,536,000 s).
    Assumed,
}

impl ExpiryFrom {
    const fn word(self) -> &'static str {
        match self {
            Self::Printed => "printed",
            Self::Assumed => "assumed",
        }
    }
}

/// What showed a token is the account its row names (§5.6): value-free.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verified {
    /// The Mac that verified it.
    pub on: String,
    /// When (unix seconds).
    pub at: u64,
    /// What compared: [`BY_WINDOWS`] (the token session's rate-limit windows with a reading of a
    /// session on the account's stored login), the only word that verifies
    /// ([`Meta::verification`]). An earlier build wrote `windows`, from the row's usage sample,
    /// into which a token's own readings, or an earlier token's, fold: such a stamp proves no
    /// account and verifies nothing on any Mac.
    pub by: String,
    /// The seven-day window's reset the token session read (unix seconds), where it read one.
    pub seven_day_resets_at: Option<u64>,
    /// The five-hour window's reset the token session read (unix seconds), where it read one.
    pub five_hour_resets_at: Option<u64>,
}

/// [`Verified::by`] of a verification whose basis was a reading of a session on the account's
/// stored login (§5.6): the stamp this build writes.
pub const BY_WINDOWS: &str = "windows-login";

/// A token's value-free record, schema 1 (§4.1). The signer is not a field: the standing reads it
/// from the log, since a writer would assert it about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meta {
    /// The email the key is made from.
    pub email: String,
    /// The `c-<12 hex>` account key of the login record the offer read.
    pub account_id: String,
    /// When the success frame was read (unix seconds).
    pub minted_at: u64,
    /// When the token ends (unix seconds).
    pub expires_at: u64,
    /// Where `expires_at` came from.
    pub expiry_from: ExpiryFrom,
    /// The minting Mac's label, for display.
    pub minted_on: String,
    /// The Claude Code version that made the token.
    pub cli: String,
    /// The route that made the token, written at mint: what says whether its mint proved the
    /// account ([`MintedBy::proves_account`]). `None` where the meta names none this build
    /// knows: an earlier build's meta, one an earlier build wrote again (it drops what it does
    /// not know), or a route a later build added.
    pub minted_by: Option<MintedBy>,
    /// Absent at mint; written by the verifying draft ([`verify`]). Only a stamp by
    /// [`BY_WINDOWS`] verifies ([`Meta::verification`]).
    pub verified: Option<Verified>,
}

const SCHEMA: i64 = 1;

impl Meta {
    /// The verification the meta carries, where it is one: a stamp by [`BY_WINDOWS`], a
    /// comparison with a reading of a session on the account's stored login. An earlier build's
    /// stamp from the row's usage sample is none, on every Mac, the minting one included: that
    /// sample can hold another account's readings, so the token serves only where its route
    /// serves an unverified one ([`MintedBy::serves_unverified`]), and its minting Mac compares
    /// it again, whose stamp by [`BY_WINDOWS`] replaces the earlier one ([`verify`]).
    #[must_use]
    pub fn verification(&self) -> Option<&Verified> {
        self.verified
            .as_ref()
            .filter(|verified| verified.by == BY_WINDOWS)
    }

    /// The record as its entry holds it: one line of JSON.
    #[must_use]
    pub fn to_json(&self) -> String {
        let verified = self.verified.as_ref().map(|v| {
            aterm_json::json!({
                "on": v.on,
                "at": v.at,
                "by": v.by,
                "seven_day_resets_at": v.seven_day_resets_at,
                "five_hour_resets_at": v.five_hour_resets_at,
            })
        });
        let value = aterm_json::json!({
            "schema": SCHEMA,
            "email": self.email,
            "account_id": self.account_id,
            "minted_at": self.minted_at,
            "expires_at": self.expires_at,
            "expiry_from": self.expiry_from.word(),
            "minted_on": self.minted_on,
            "cli": self.cli,
            "minted_by": self.minted_by.map(MintedBy::word),
            "verified": verified,
        });
        aterm_json::to_string(&value).unwrap_or_default()
    }

    /// The record `bytes` holds, where it is one of schema 1. A `minted_by` that is absent, null
    /// or a word this build does not know reads as `None`: the record still parses.
    #[must_use]
    pub fn from_json(bytes: &[u8]) -> Option<Self> {
        let value: aterm_json::Value = aterm_json::from_slice(bytes).ok()?;
        if value.get("schema")?.as_i64()? != SCHEMA {
            return None;
        }
        let text = |key: &str| value.get(key)?.as_str().map(str::to_owned);
        let secs = |of: &aterm_json::Value, key: &str| {
            of.get(key)?.as_i64().and_then(|n| u64::try_from(n).ok())
        };
        let verified = match value.get("verified") {
            None | Some(aterm_json::Value::Null) => None,
            Some(v) => Some(Verified {
                on: v.get("on")?.as_str()?.to_owned(),
                at: secs(v, "at")?,
                by: v.get("by")?.as_str()?.to_owned(),
                seven_day_resets_at: secs(v, "seven_day_resets_at"),
                five_hour_resets_at: secs(v, "five_hour_resets_at"),
            }),
        };
        Some(Self {
            email: text("email")?,
            account_id: text("account_id")?,
            minted_at: secs(&value, "minted_at")?,
            expires_at: secs(&value, "expires_at")?,
            expiry_from: match value.get("expiry_from")?.as_str()? {
                "printed" => ExpiryFrom::Printed,
                "assumed" => ExpiryFrom::Assumed,
                _ => return None,
            },
            minted_on: text("minted_on")?,
            cli: text("cli")?,
            minted_by: value
                .get("minted_by")
                .and_then(aterm_json::Value::as_str)
                .and_then(MintedBy::parse),
            verified,
        })
    }
}

/// What this Mac reads of a stored token's two entries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// Both open, the token is a setup-token, and both come from one draft.
    Readable,
    /// An inner seal names an epoch this Mac does not open.
    Unreadable,
    /// A check failed.
    Mismatched(Mismatch),
}

/// A stored token's standing, value-free: what every surface that shows a token's state reads
/// ([`standing`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Standing {
    /// The account's key.
    pub key: Key,
    /// The meta, where it opens and parses.
    pub meta: Option<Meta>,
    /// The draft and value checks.
    pub check: Check,
    /// The label of the seat that signed the earliest `Put` of the token's value this Mac opens;
    /// `None` where an earlier `Put` may stand that this Mac cannot open ("signer not checked
    /// here"). Shown, never a gate.
    pub signer: Option<String>,
    /// This Mac's own seat is that signer: it minted the token.
    pub minted_here: bool,
    /// The labels of the Macs the log now removes that were handed an epoch that sealed a `Put`
    /// of the value (or signed one), in order.
    pub exposed: Vec<String>,
    /// When this Mac recorded Claude Code's refusal of this token (the one its meta's `minted_at`
    /// names), where it did ([`note_refused`]): this Mac's observation, kept beside its vault
    /// state, never in the vault.
    pub refused_at: Option<u64>,
}

/// A stored token's state, in the order its checks run (§5.1, the states a standing decides).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// `unreadable here`.
    Unreadable,
    /// `mismatched (draft|value)`.
    Mismatched(Mismatch),
    /// `ended`: its end has passed.
    Ended,
    /// `rejected`: this Mac saw Claude Code refuse it ([`Standing::refused_at`]).
    Rejected,
    /// `unverified`: no verification yet, exposure beside it. By the vault's records it is in use
    /// on every member Mac where its mint proved the account, else on its minting Mac only
    /// ([`MintedBy::serves_unverified`]). Whether a Mac starts sessions on it is the harness's to
    /// say: a comparison there may have found the token's windows different, or may still wait
    /// for a reading of the account's stored login, and no vault record holds either.
    Unverified {
        /// This Mac minted it.
        minted_here: bool,
        /// [`Meta::minted_by`].
        minted_by: Option<MintedBy>,
        /// [`Standing::exposed`].
        exposed: Vec<String>,
    },
    /// `exposed (removed <Mac>)`: a removed Mac was handed it.
    Exposed(Vec<String>),
    /// `soon`: [`SOON_SECS`] or fewer until its end.
    Soon,
    /// `active`.
    Active,
}

impl State {
    /// The state as a person reads it, value-free.
    #[must_use]
    pub fn words(&self) -> String {
        let removed = |macs: &[String]| format!("exposed (removed {})", macs.join(", "));
        match self {
            Self::Unreadable => "unreadable here".to_owned(),
            Self::Mismatched(Mismatch::Draft) => "mismatched (draft)".to_owned(),
            Self::Mismatched(Mismatch::Value) => "mismatched (value)".to_owned(),
            Self::Ended => "ended".to_owned(),
            Self::Rejected => "rejected".to_owned(),
            Self::Unverified {
                minted_here,
                minted_by,
                exposed,
            } => {
                let route = match minted_by {
                    Some(MintedBy::Window) => " in aterm's window",
                    Some(MintedBy::Headless) => " headless",
                    Some(MintedBy::Terminal) => " at a terminal",
                    None => "",
                };
                let place = if *minted_here {
                    "here"
                } else {
                    "on another Mac"
                };
                let used = if minted_by.is_some_and(MintedBy::proves_account) {
                    "in use on every Mac"
                } else if *minted_here {
                    "in use here only until verified"
                } else {
                    "not in use here until verified"
                };
                let mut words = format!("unverified (made{route} {place}; {used})");
                if !exposed.is_empty() {
                    words.push_str(", ");
                    words.push_str(&removed(exposed));
                }
                words
            }
            Self::Exposed(macs) => removed(macs),
            Self::Soon => "soon".to_owned(),
            Self::Active => "active".to_owned(),
        }
    }
}

impl Standing {
    /// Whether the meta carries a verification ([`Meta::verification`]: a stamp by
    /// [`BY_WINDOWS`]), on every Mac alike.
    #[must_use]
    pub fn verified(&self) -> bool {
        self.meta
            .as_ref()
            .is_some_and(|meta| meta.verification().is_some())
    }

    /// Whether the vault's records put the token in use on this Mac, its end and its checks aside
    /// ([`Self::state`] judges those): verified, or not verified and made by a route that proved
    /// the account, on every member Mac from mint; otherwise only where this Mac minted it
    /// ([`MintedBy::serves_unverified`]).
    #[must_use]
    pub fn serves_here(&self) -> bool {
        self.verified()
            || self
                .meta
                .as_ref()
                .is_some_and(|meta| MintedBy::serves_unverified(meta.minted_by, self.minted_here))
    }

    /// Whether the vault's records put the token in use on some Mac other than this one, its end
    /// and its checks aside: one another Mac minted (in use there at least), one verified, or one
    /// made by a route that proved the account ([`MintedBy::serves_unverified`]). Only a token
    /// this Mac minted at a person's terminal, or whose meta names no route, and that is not
    /// verified, serves this Mac alone: replacing it replaces no other Mac's token.
    #[must_use]
    pub fn serves_elsewhere(&self) -> bool {
        !self.minted_here
            || self.verified()
            || self
                .meta
                .as_ref()
                .is_some_and(|meta| MintedBy::serves_unverified(meta.minted_by, false))
    }

    /// The state at `now` (unix seconds).
    #[must_use]
    pub fn state(&self, now: u64) -> State {
        match self.check {
            Check::Unreadable => return State::Unreadable,
            Check::Mismatched(why) => return State::Mismatched(why),
            Check::Readable => {}
        }
        let Some(meta) = &self.meta else {
            return State::Mismatched(Mismatch::Value);
        };
        if meta.expires_at <= now {
            return State::Ended;
        }
        if self.refused_at.is_some() {
            return State::Rejected;
        }
        if !self.verified() {
            return State::Unverified {
                minted_here: self.minted_here,
                minted_by: meta.minted_by,
                exposed: self.exposed.clone(),
            };
        }
        if !self.exposed.is_empty() {
            return State::Exposed(self.exposed.clone());
        }
        if meta.expires_at - now <= SOON_SECS {
            return State::Soon;
        }
        State::Active
    }

    /// Whether the token is in use on this Mac by what the vault's records and this Mac's
    /// refusals say alone: `active`, `soon` or `exposed`, a verified token every Mac starts
    /// sessions on, never one this Mac saw refused nor one ended. An `unverified` token is not,
    /// even one its route puts in use on every Mac ([`Self::serves_here`]): whether a Mac serves
    /// it is the harness's verdict there (a comparison there may have found its windows different,
    /// or it may still wait for a stored-login reading), which no vault record holds. What a
    /// `claude-token:` need waits for ([`crate::need`]).
    #[must_use]
    pub fn verified_in_use(&self, now: u64) -> bool {
        matches!(
            self.state(now),
            State::Active | State::Soon | State::Exposed(_)
        )
    }
}

/// What a read found: the vault state could not be read just now (busy, unsigned, failed or
/// mid-apply), the vault holds no entry of the account, or what it holds.
#[derive(Debug)]
pub enum Lookup<T> {
    /// `unknown`: this Mac's vault state could not be read just now.
    Unknown,
    /// `none`: no entry of the account.
    Absent,
    /// The entry.
    Found(T),
}

/// A typed read of a token ([`read`]): the token, only where it is a setup-token from one draft
/// that opens here, and its standing.
pub struct TokenRead {
    token: Option<SecretBytes>,
    /// The meta's value as the log holds it, beside [`TokenRead::token`]: what a verification
    /// stands on ([`verify`]), byte for byte, whatever build wrote it.
    meta_value: Option<SecretBytes>,
    /// The standing.
    pub standing: Standing,
}

impl TokenRead {
    /// The token, in a zeroizing buffer, where [`Standing::check`] is [`Check::Readable`]; never a
    /// value outside [`value_grammar`].
    #[must_use]
    pub const fn token(&self) -> Option<&SecretBytes> {
        self.token.as_ref()
    }
}

impl std::fmt::Debug for TokenRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenRead")
            .field("standing", &self.standing)
            .finish_non_exhaustive()
    }
}

fn now_secs() -> u64 {
    aterm_time::unix_secs().unwrap_or(0)
}

/// Write `token` and `meta` as the account `key`'s entries, in one draft, as this Mac, a seated
/// member (§4.2): the class's only writer. It refuses every route but a seat in the vault's log,
/// a value outside [`value_grammar`], and a meta that carries a verification (a shipped build
/// writes one only from its own reading, [`verify`]). `on_busy` says what it does while a vault
/// pass holds this Mac's state; a write that cannot be signed now waits in the vault's own
/// waiting writes and reads "not yet in vault". Writing again replaces both in place.
///
/// # Errors
/// As [`crate::client::member_set`].
pub fn write(
    paths: &Paths,
    key: &Key,
    token: &SecretBytes,
    meta: &Meta,
    on_busy: OnBusy,
) -> Result<MemberWritten> {
    if !value_grammar(token.as_slice()) {
        return Ok(MemberWritten::Refused(format!(
            "the value is not a Claude Code setup-token ({VALUE_PREFIX}NN-…, one line)"
        )));
    }
    if meta.verified.is_some() {
        return Ok(MemberWritten::Refused(
            "a new token is written unverified; its first use verifies it".to_owned(),
        ));
    }
    write_pair(paths, key, token, meta, None, on_busy)
}

/// Write `token` and `meta` as the account `key`'s entries, in one draft. Where `read` names the
/// token and meta values the writer read ([`verify`]: the same token, beside a meta it amends),
/// each entry stands on its value there, so the write is refused where either changed since
/// ([`MemberEntry::read_as`]); otherwise it is a person's write, which replaces what it finds.
fn write_pair(
    paths: &Paths,
    key: &Key,
    token: &SecretBytes,
    meta: &Meta,
    read: Option<(&SecretBytes, &SecretBytes)>,
    on_busy: OnBusy,
) -> Result<MemberWritten> {
    let entry = |name: String, value: SecretBytes, read_as: Option<&SecretBytes>| MemberEntry {
        name,
        value,
        if_absent: false,
        anchor: crate::client::Anchor::Drop,
        fleet_reads_anchors: false,
        read_as: read_as.cloned(),
    };
    member_write(
        paths,
        &[
            entry(key.entry(), token.clone(), read.map(|(token, _)| token)),
            entry(
                key.meta_entry(),
                SecretBytes::from(meta.to_json()),
                read.map(|(_, meta)| meta),
            ),
        ],
        on_busy,
    )
}

/// Read the account `key`'s token through the typed reading (`Reading::ClaudeToken`), waiting for
/// a vault writer holding this Mac's state no longer than `wait`. A value outside
/// [`value_grammar`] is never returned: its standing is `mismatched (value)`. A write that waits
/// in this Mac's waiting writes is not in the log yet, so it reads as absent.
///
/// # Errors
/// This Mac's state or records cannot be read, or a record addressed to it does not open.
pub fn read(paths: &Paths, key: &Key, wait: Duration) -> Result<Lookup<TokenRead>> {
    read_until(paths, key, Instant::now() + wait)
}

fn read_until(paths: &Paths, key: &Key, deadline: Instant) -> Result<Lookup<TokenRead>> {
    let (entry, meta_entry) = (key.entry(), key.meta_entry());
    let Some(snapshot) = read_claude_token_until(paths, &[&entry, &meta_entry], deadline)? else {
        return Ok(Lookup::Unknown);
    };
    let mut values = snapshot.values.into_iter();
    let (token, meta) = (values.next().flatten(), values.next().flatten());
    if token.is_none() && meta.is_none() {
        return Ok(Lookup::Absent);
    }
    let Some(opened) = snapshot.log.open_audience_entries(&snapshot.identity)? else {
        return Ok(Lookup::Unknown);
    };
    let mut judged = judge(
        &snapshot.log,
        &snapshot.identity,
        &opened,
        key,
        token.as_ref(),
        meta.as_ref(),
    )?;
    judged.standing.refused_at = refused_at(&refusals(paths), &judged.standing);
    Ok(Lookup::Found(judged))
}

/// The account `key`'s standing, value-free (§4.3): what every surface that shows a token's
/// state reads. It waits for a vault writer holding this Mac's state only briefly, and reads
/// [`Lookup::Unknown`] past that.
///
/// # Errors
/// As [`read`].
pub fn standing(paths: &Paths, key: &Key) -> Result<Lookup<Standing>> {
    Ok(match read(paths, key, STANDING_WAIT)? {
        Lookup::Found(read) => Lookup::Found(read.standing),
        Lookup::Absent => Lookup::Absent,
        Lookup::Unknown => Lookup::Unknown,
    })
}

/// Every token this Mac's vault holds, with its standing, value-free, in key order
/// (`secrets ls --describe`). [`Lookup::Unknown`] where the vault state could not be read just
/// now; an empty list where it holds none.
///
/// # Errors
/// As [`read`].
pub fn listed(paths: &Paths) -> Result<Lookup<Vec<Standing>>> {
    let Some(snapshot) = read_claude_token_until(paths, &[], Instant::now() + STANDING_WAIT)?
    else {
        return Ok(Lookup::Unknown);
    };
    let Some(opened) = snapshot.log.open_audience_entries(&snapshot.identity)? else {
        return Ok(Lookup::Unknown);
    };
    let held = opened.by_audience.get(&Audience::Everyone);
    let keys: BTreeSet<Key> = held
        .into_iter()
        .flat_map(|entries| entries.keys())
        .filter_map(|name| {
            Key::of_entry(name).or_else(|| {
                Key::of_entry(name.strip_suffix(CLAUDE_TOKEN_META)?)
                    .filter(|_| crate::entry_class::claude_token(name))
            })
        })
        .collect();
    let refused = refusals(paths);
    let mut out = Vec::with_capacity(keys.len());
    for key in keys {
        let value = |name: String| {
            held.and_then(|entries| entries.get(&name))
                .map(|e| &e.value)
        };
        let mut read = judge(
            &snapshot.log,
            &snapshot.identity,
            &opened,
            &key,
            value(key.entry()),
            value(key.meta_entry()),
        )?;
        read.standing.refused_at = refused_at(&refused, &read.standing);
        out.push(read.standing);
    }
    Ok(Lookup::Found(out))
}

/// The file of this Mac's refusals (§5.6), beside its vault state, which no vault entry can name:
/// value-free, one per account.
const REFUSALS: &str = "claude-token-refusals.json";

/// The lock a refusal's writer takes.
const REFUSALS_LOCK: &str = "claude-token-refusals.lock";

/// How long a refusal's writer waits for another to let the lock go: it runs at a session's
/// lost-login point, so it never waits like a person's verb; that session's next one records it.
const REFUSALS_WAIT: Duration = Duration::from_secs(2);

/// The largest refusals file read.
const MAX_REFUSALS: usize = 1 << 16;

/// The refusals file's schema.
const REFUSALS_SCHEMA: i64 = 1;

/// This Mac's refusals by account key: the refused token's `minted_at` and when the refusal was
/// recorded. A file that is absent or does not read holds none: each session here meets a refused
/// token's refusal again at its next request, which records it again ([`note_refused`]).
fn refusals(paths: &Paths) -> BTreeMap<String, (u64, u64)> {
    let Ok(Some(bytes)) = crate::fsx::read_private(&paths.dir.join(REFUSALS), MAX_REFUSALS) else {
        return BTreeMap::new();
    };
    let Ok(value) = aterm_json::from_slice::<aterm_json::Value>(bytes.as_slice()) else {
        return BTreeMap::new();
    };
    if value.get("schema").and_then(aterm_json::Value::as_i64) != Some(REFUSALS_SCHEMA) {
        return BTreeMap::new();
    }
    let secs = |of: &aterm_json::Value, key: &str| {
        of.get(key)?.as_i64().and_then(|n| u64::try_from(n).ok())
    };
    value
        .get("refused")
        .and_then(aterm_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let key = Key::parse(row.get("key")?.as_str()?)?;
            Some((key.0, (secs(row, "minted_at")?, secs(row, "at")?)))
        })
        .collect()
}

/// When this Mac refused the token `standing` reads, by `refused`: only a refusal of that very
/// token, the one its meta's `minted_at` names, counts, so a newer token is never one refused.
fn refused_at(refused: &BTreeMap<String, (u64, u64)>, standing: &Standing) -> Option<u64> {
    let (minted_at, at) = refused.get(standing.key.as_str())?;
    (standing.meta.as_ref()?.minted_at == *minted_at).then_some(*at)
}

/// RECORDS THIS MAC'S REFUSAL OF A TOKEN (§5.6): Claude Code refused, on this Mac at `now`, the
/// token whose standing is `standing`. The refusal is kept beside this Mac's vault state, value-free
/// (the account's key, the token's `minted_at`, when), and never written to the vault: it is this
/// Mac's observation, and each Mac's sessions meet the same refusal at their next request. Every
/// later reading here judges that token `rejected` ([`State::Rejected`]: not in use), until a newer
/// one replaces it; a refusal of the newer one replaces the older record. Returns whether this call
/// recorded it first; a standing with no meta names no token and records nothing.
///
/// # Errors
/// This Mac's vault state folder or the refusals file cannot be written, or another writer held
/// the refusals file's lock past [`REFUSALS_WAIT`].
pub fn note_refused(paths: &Paths, standing: &Standing, now: u64) -> Result<bool> {
    let Some(meta) = standing.meta.as_ref() else {
        return Ok(false);
    };
    crate::fsx::ensure_private_dir(&paths.dir)?;
    let lock_path = paths.dir.join(REFUSALS_LOCK);
    let _held = aterm_uds::file_lock::LockFile::take(
        &lock_path,
        Instant::now() + REFUSALS_WAIT,
        "another refusal is being recorded",
    )
    .map_err(|e| crate::Error::new(format!("{}: {e}", lock_path.display())))?;
    let mut refused = refusals(paths);
    if refused_at(&refused, standing).is_some() {
        return Ok(false);
    }
    refused.insert(standing.key.0.clone(), (meta.minted_at, now));
    let rows: Vec<aterm_json::Value> = refused
        .iter()
        .map(|(key, (minted_at, at))| {
            aterm_json::json!({"key": key, "minted_at": minted_at, "at": at})
        })
        .collect();
    let text =
        aterm_json::to_string(&aterm_json::json!({"schema": REFUSALS_SCHEMA, "refused": rows}))
            .map_err(|e| crate::Error::new(format!("{REFUSALS}: {e}")))?;
    crate::fsx::write_atomic(&paths.dir.join(REFUSALS), text.as_bytes(), 0o600)?;
    Ok(true)
}

/// Whether a write of this Mac's of the account `key`'s token is still to be signed: it waits in
/// the vault's waiting writes, so the log does not hold it yet and it reads "not yet in vault"
/// (§5.1's `vault` column). Where the waiting writes do not read, it may.
///
/// # Errors
/// This Mac's identity cannot be read.
pub fn waiting_here(paths: &Paths, key: &Key) -> Result<bool> {
    member_waits_here(paths, &key.entry())
}

/// This Mac's seat's label in its vault's log, as a token's meta names where it was minted
/// (`minted_on`, display only): `None` where the state could not be read just now or no seat of
/// the log is this Mac's.
///
/// # Errors
/// As [`read`].
pub fn seat_here(paths: &Paths) -> Result<Option<String>> {
    let Some(snapshot) = read_claude_token_until(paths, &[], Instant::now() + STANDING_WAIT)?
    else {
        return Ok(None);
    };
    Ok(own_actor(&snapshot.log, &snapshot.identity).map(|actor| seat_label(&snapshot.log, &actor)))
}

/// The standing of the account `key`'s entries `token` and `meta` (each as the log decodes it),
/// as `identity` opens `log` (`opened`), and the token where it passes.
fn judge(
    log: &Log,
    identity: &crate::age::Identity,
    opened: &crate::membership_log::entries::OpenedAudienceEntries,
    key: &Key,
    token: Option<&SecretBytes>,
    meta: Option<&SecretBytes>,
) -> Result<TokenRead> {
    let forms = [token, meta].map(|value| value.map(|value| inner::of(value.as_slice())));
    let parsed = match forms[1] {
        Some(Form::Opened(bytes)) => Meta::from_json(bytes),
        _ => None,
    };
    let check = match forms {
        [Some(Form::Unreadable(_)), _] | [_, Some(Form::Unreadable(_))] => Check::Unreadable,
        [Some(Form::Unsealed), _] | [_, Some(Form::Unsealed)] => Check::Mismatched(Mismatch::Value),
        [Some(Form::Opened(token)), Some(Form::Opened(_))] => {
            if !value_grammar(token) || parsed.is_none() {
                Check::Mismatched(Mismatch::Value)
            } else if one_draft(log, opened, key) {
                Check::Readable
            } else {
                Check::Mismatched(Mismatch::Draft)
            }
        }
        [Some(Form::Opened(token)), None] if !value_grammar(token) => {
            Check::Mismatched(Mismatch::Value)
        }
        _ => Check::Mismatched(Mismatch::Draft),
    };
    let mut standing = Standing {
        key: key.clone(),
        meta: parsed,
        check,
        signer: None,
        minted_here: false,
        exposed: Vec::new(),
        refused_at: None,
    };
    let (token, meta) = match (check, token, meta) {
        (Check::Readable, Some(token), Some(meta)) => (token.clone(), meta.clone()),
        _ => {
            return Ok(TokenRead {
                token: None,
                meta_value: None,
                standing,
            });
        }
    };
    if let Some(lineage) = log.put_lineage(identity, &key.entry(), token.as_slice())? {
        let first = lineage.puts.first().filter(|_| !lineage.unopened_before);
        standing.signer = first.map(|put| seat_label(log, &put.actor));
        standing.minted_here = first.is_some_and(|put| Some(put.actor) == own_actor(log, identity));
        standing.exposed = exposure(log, &lineage);
    }
    Ok(TokenRead {
        token: Some(token),
        meta_value: Some(meta),
        standing,
    })
}

/// This Mac's seat in `log`: the member whose binding holds `identity`'s recipient.
fn own_actor(log: &Log, identity: &crate::age::Identity) -> Option<MemberId> {
    let recipient = identity.recipient();
    log.membership()
        .members()
        .iter()
        .find(|(_, member)| member.binding.recipient() == recipient)
        .map(|(actor, _)| *actor)
}

/// Whether both of `key`'s names' winning `Put`s come from one draft or one rotation batch (the
/// same actor, `authored_ms` and epoch), neither name conflicted (§4.3 check 1). It catches a
/// split or conflicted write, and says nothing of who wrote the token.
fn one_draft(
    log: &Log,
    opened: &crate::membership_log::entries::OpenedAudienceEntries,
    key: &Key,
) -> bool {
    let names = [key.entry(), key.meta_entry()];
    let mut drafts = BTreeSet::new();
    for name in names {
        let at = (Audience::Everyone, name);
        if opened.conflicts.contains(&at) {
            return false;
        }
        let Some(record) = opened.winning_puts.get(&at).and_then(|id| log.get(id)) else {
            return false;
        };
        let crate::membership_log::Body::Put { epoch, .. } = record.body() else {
            return false;
        };
        drafts.insert((*record.actor(), record.authored_ms(), *epoch));
    }
    drafts.len() == 1
}

/// The labels of the Macs the log removes that could open the token's value (§4.3 exposure):
/// each signer of a `Put` of it, and each minter of, and recipient of a `Wrap` of, an epoch
/// that sealed one or that its inner seal names.
fn exposure(log: &Log, lineage: &crate::membership_log::entries::Lineage) -> Vec<String> {
    let membership = log.membership();
    let removed_actor = |actor: &MemberId| membership.removal(actor).is_some();
    let mut labels = BTreeSet::new();
    let epochs: BTreeSet<_> = lineage
        .puts
        .iter()
        .flat_map(|put| std::iter::once(put.epoch).chain(put.sealed_under))
        .collect();
    for put in &lineage.puts {
        if removed_actor(&put.actor) {
            labels.insert(seat_label(log, &put.actor));
        }
    }
    for epoch in &epochs {
        if let Some(record) = log.get(epoch)
            && removed_actor(record.actor())
        {
            labels.insert(seat_label(log, record.actor()));
        }
        for recipient in log.wrapped_to(epoch) {
            if log.recipient_is_removed(&recipient) {
                labels.insert(recipient_label(log, &recipient));
            }
        }
    }
    labels.into_iter().collect()
}

/// Write the account `key`'s token again with its meta's `verified` set to `evidence`, both in
/// one draft (§5.6): the same token, the meta with the verification (its route, `minted_by`,
/// kept). Only the Mac that minted the token verifies it; any other is refused, and so is a token
/// that does not read as one here. `minted_at` names the token the evidence was read on (its
/// meta's `minted_at` in the standing the reading was judged against): where the vault's token is
/// another (a replacement landed before this call read it), the write is refused, so a
/// verification never stamps a token it did not compare. A verification is of the pair it read:
/// each entry stands on the value it read ([`MemberEntry::read_as`]: the token, and the meta byte
/// for byte as the log holds it), so the write is refused where the token or its meta changed
/// before the write lands: at its draft, where the log holds another value or concurrent values (a
/// replacement that committed after this read, this Mac's own or one a vault pass applied, another
/// Mac's token beside it included), and as it lands what waits, where a write of this Mac's still
/// to be signed leaves the name otherwise (a `--replace` that waits for the vault pass, whose
/// token sessions here do not use yet, even one that keeps the token and changes only its meta). A
/// verification that itself waits for the vault pass is dropped there as overtaken, both entries,
/// where either changed by then. It never replaces a newer token or meta and settles no conflict.
///
/// # Errors
/// As [`write`].
pub fn verify(
    paths: &Paths,
    key: &Key,
    minted_at: u64,
    evidence: Verified,
    on_busy: OnBusy,
) -> Result<MemberWritten> {
    let deadline = match on_busy {
        OnBusy::Refuse(by) | OnBusy::Wait(by) => by,
    };
    let read = match read_until(paths, key, deadline)? {
        Lookup::Found(read) => read,
        Lookup::Absent => {
            return Ok(MemberWritten::Refused(format!(
                "the vault holds no token for {}",
                key.as_str()
            )));
        }
        Lookup::Unknown => return Ok(MemberWritten::Busy),
    };
    let (Some(token), Some(meta_value), Some(meta)) = (
        read.token(),
        read.meta_value.as_ref(),
        read.standing.meta.as_ref(),
    ) else {
        return Ok(MemberWritten::Refused(format!(
            "the token for {} reads {} here",
            key.as_str(),
            read.standing.state(now_secs()).words()
        )));
    };
    if meta.minted_at != minted_at {
        return Ok(MemberWritten::Refused(format!(
            "the vault's token for {} is not the one compared: another replaced it",
            key.as_str()
        )));
    }
    if !read.standing.minted_here {
        return Ok(MemberWritten::Refused(
            "only the Mac that minted a token verifies it".to_owned(),
        ));
    }
    let mut meta = meta.clone();
    meta.verified = Some(evidence);
    write_pair(paths, key, token, &meta, Some((token, meta_value)), on_busy)
}

/// Remove the account `key`'s token and meta, both in one draft under a fresh `[everyone]` epoch
/// that reseals what stays (§3.6, F15), so no Mac admitted afterwards opens either; this Mac's
/// own waiting writes of them are taken back first. A Mac that already opened the token keeps
/// it: only revoking it at claude.ai ends it (§4.7).
///
/// # Errors
/// As [`crate::client::member_unset`].
pub fn remove(paths: &Paths, key: &Key, by: Instant) -> Result<MemberUnset> {
    let (entry, meta) = (key.entry(), key.meta_entry());
    member_unset_names(paths, &[&entry, &meta], by)
}
