// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code setup-tokens in the fleet vault (`docs/DESIGN-claude-setup-token-2026-11-05.md`
//! §4.1–§4.3, slice S4): the typed calls that write, read, judge, verify, settle and remove one
//! account's token, the only code that reads or writes the class
//! ([`crate::entry_class::claude_token`]).
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
//! it is verified ([`Meta::verification`]: a comparison with the account's stored login on any Mac
//! where that login works, never an earlier build's stamp from a usage sample), then on every Mac. Verification is a label: it
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
use crate::membership_log::entries::DraftPut;
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
    /// The Mac that verified it: the seat that wrote the verification, whichever Mac minted the
    /// token ([`verify`] reads it from the log).
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
    /// serves an unverified one ([`MintedBy::serves_unverified`]), and a Mac that holds the
    /// account's stored login compares it again, whose stamp by [`BY_WINDOWS`] replaces the
    /// earlier one ([`verify`]).
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
    /// This Mac's seat's label in the log the read judged, where one is this Mac's: who a
    /// verification names ([`verify`]).
    seat: Option<String>,
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
        settles: None,
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
    judged.seat =
        own_actor(&snapshot.log, &snapshot.identity).map(|actor| seat_label(&snapshot.log, &actor));
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

/// One refusal this Mac recorded ([`note_refused`]): the refused token's `minted_at`, when the
/// refusal was recorded, and the HTTP status Claude Code refused it with, where the record names
/// one (a record an earlier build wrote names none).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Refusal {
    minted_at: u64,
    at: u64,
    status: Option<u16>,
}

/// This Mac's refusals by account key ([`Refusal`]). A file that is absent or does not read holds
/// none: each session here meets a refused token's refusal again at its next request, which
/// records it again ([`note_refused`]).
fn refusals(paths: &Paths) -> BTreeMap<String, Refusal> {
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
            let status = row
                .get("status")
                .and_then(aterm_json::Value::as_i64)
                .and_then(|n| u16::try_from(n).ok());
            Some((
                key.0,
                Refusal {
                    minted_at: secs(row, "minted_at")?,
                    at: secs(row, "at")?,
                    status,
                },
            ))
        })
        .collect()
}

/// This Mac's refusal of the token `standing` reads, by `refused`: only a refusal of that very
/// token, the one its meta's `minted_at` names, counts, so a newer token is never one refused.
fn refusal_of(refused: &BTreeMap<String, Refusal>, standing: &Standing) -> Option<Refusal> {
    let refusal = refused.get(standing.key.as_str())?;
    (standing.meta.as_ref()?.minted_at == refusal.minted_at).then_some(*refusal)
}

/// When this Mac refused the token `standing` reads, by `refused` ([`refusal_of`]).
fn refused_at(refused: &BTreeMap<String, Refusal>, standing: &Standing) -> Option<u64> {
    refusal_of(refused, standing).map(|refusal| refusal.at)
}

/// The HTTP status Claude Code refused the token `standing` reads with on this Mac, as the refusal
/// this Mac recorded of that very token names it ([`note_refused`]): `None` where this Mac
/// recorded no refusal of it, or the record names no status (an earlier build's).
#[must_use]
pub fn refused_status(paths: &Paths, standing: &Standing) -> Option<u16> {
    refusal_of(&refusals(paths), standing)?.status
}

/// RECORDS THIS MAC'S REFUSAL OF A TOKEN (§5.6): Claude Code refused, on this Mac at `now` and
/// with the HTTP status `status`, the token whose standing is `standing`. The refusal is kept
/// beside this Mac's vault state, value-free (the account's key, the token's `minted_at`, when,
/// the status), and never written to the vault: it is this Mac's observation, and each Mac's
/// sessions meet the same refusal at their next request. Every later reading here judges that
/// token `rejected` ([`State::Rejected`]: not in use), until a newer one replaces it; a refusal of
/// the newer one replaces the older record. Returns whether this call recorded it first; a
/// standing with no meta names no token and records nothing.
///
/// # Errors
/// This Mac's vault state folder or the refusals file cannot be written, or another writer held
/// the refusals file's lock past [`REFUSALS_WAIT`].
pub fn note_refused(paths: &Paths, standing: &Standing, status: u16, now: u64) -> Result<bool> {
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
    refused.insert(
        standing.key.0.clone(),
        Refusal {
            minted_at: meta.minted_at,
            at: now,
            status: Some(status),
        },
    );
    let rows: Vec<aterm_json::Value> = refused
        .iter()
        .map(|(key, refusal)| {
            let mut row = aterm_json::json!({
                "key": key,
                "minted_at": refusal.minted_at,
                "at": refusal.at,
            });
            if let (Some(status), Some(fields)) = (refusal.status, row.as_object_mut()) {
                fields.insert("status".to_owned(), aterm_json::json!(status));
            }
            row
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
/// as `identity` opens `log` (`opened`), and the token where it passes. A pair whose two names'
/// winners come from different drafts, or whose registers hold concurrent writes, reads
/// `mismatched (draft)` before any parse of its values: the winner's meta may be one this build
/// does not read (a later schema's, a refutation's), and the pair is still one [`settle`] makes
/// whole, which `mismatched (value)` would never send it to.
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
            if !one_draft(log, opened, key) {
                Check::Mismatched(Mismatch::Draft)
            } else if !value_grammar(token) || parsed.is_none() {
                Check::Mismatched(Mismatch::Value)
            } else {
                Check::Readable
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
                seat: None,
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
        seat: None,
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
/// kept). Any seated Mac verifies it, the one that minted it or another: the harness compares the
/// token's windows with a reading of the account's stored login wherever that login works, and
/// the comparison is what verifies. The verification names this Mac's seat in the log
/// ([`Verified::on`]), whatever `evidence` says. A token that does not read as one here is
/// refused: a refutation among them, which a later build writes as a meta of a later schema this
/// build does not parse (`mismatched (value)`), so no verification is ever written over one.
/// `minted_at` names the token the evidence was read on (its meta's `minted_at` in the standing
/// the reading was judged against): where the vault's token is another (a replacement landed
/// before this call read it), the write is refused, so a verification never stamps a token it
/// did not compare. A verification is of
/// the pair it read: each entry stands on the value it read ([`MemberEntry::read_as`]: the token,
/// and the meta byte for byte as the log holds it), so the write is refused where the token or
/// its meta changed before the write lands: at its draft, where the log holds another value or
/// concurrent values (a replacement that committed after this read, this Mac's own or one a vault
/// pass applied, another Mac's token beside it included), and as it lands what waits, where a
/// write of this Mac's still to be signed leaves the name otherwise (a `--replace` that waits for
/// the vault pass, whose token sessions here do not use yet, even one that keeps the token and
/// changes only its meta). A verification that itself waits for the vault pass is dropped there
/// as overtaken, both entries, where either changed by then. It never replaces a newer token or
/// meta and settles no conflict: two Macs' concurrent writes of the pair are [`settle`]'s.
///
/// # Errors
/// As [`write`].
pub fn verify(
    paths: &Paths,
    key: &Key,
    minted_at: u64,
    mut evidence: Verified,
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
    let Some(seat) = read.seat.clone() else {
        return Ok(MemberWritten::NotSeated);
    };
    evidence.on = seat;
    let mut meta = meta.clone();
    meta.verified = Some(evidence);
    write_pair(paths, key, token, &meta, Some((token, meta_value)), on_busy)
}

/// Whether the meta `bytes` carries a refutation: a non-null `refuted`, which a later build writes
/// where a comparison found the token another account's, in a meta of a later schema this build
/// does not parse. This build reads it only to keep it at a settle ([`merged_meta`]).
fn refuted(bytes: &[u8]) -> bool {
    aterm_json::from_slice::<aterm_json::Value>(bytes)
        .ok()
        .and_then(|value| value.get("refuted").cloned())
        .is_some_and(|refuted| !refuted.is_null())
}

/// What [`settle`] did of an account's pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Settled {
    /// No concurrent writes split the pair: nothing to settle.
    Nothing,
    /// Another Mac's turn it is to settle it ([`settle`]), whose seat's label this is: display
    /// only, since two Macs can share a label and the turn is a seat's.
    Theirs(String),
    /// Nothing written, and why (value-free): a removal among the siblings, a sibling this Mac
    /// does not open, no meta of the token beside it, or a vault state that could not be read
    /// just now.
    Held(String),
    /// This Mac settled it: what became of its draft.
    Written(MemberWritten),
}

/// How long one seat's turn to settle a split pair lasts before the next seat's begins
/// ([`settle_turn`]): long enough for the Mac whose turn it is to reach the pair at its own
/// accounts upkeep, a vault held busy for a while included, and short enough that a Mac off the
/// LAN, asleep or on a build with no settle leaves the pair split, and served by no Mac, for one
/// turn and no longer.
pub const SETTLE_TURN_SECS: u64 = 30 * 60;

/// The seat whose turn it is at `now` (unix seconds) to settle a pair split since `split_at` (the
/// latest of its siblings' drafts, by its writer's clock): `first` where one is named (the
/// writer of the pair [`settle`] keeps, where the siblings carry different tokens), then each
/// other of `writers` (the siblings' current writers), then each other of `members` (every
/// current seat), each group in seat order, every turn [`SETTLE_TURN_SECS`] long, round and round
/// from the split on. Every Mac that reads the same siblings reads the same order and the same
/// turn (their clocks apart by less than a turn), so one seat settles at a time; a seat whose turn
/// passes unsettled hands it on. Seats, never labels, take turns: two Macs can share a label, and
/// one seat is one Mac. `None` where no seat is current.
pub(crate) fn settle_turn(
    first: Option<MemberId>,
    writers: &BTreeSet<MemberId>,
    members: &BTreeSet<MemberId>,
    split_at: u64,
    now: u64,
) -> Option<MemberId> {
    let order: Vec<MemberId> = first
        .into_iter()
        .chain(writers.iter().copied().filter(|seat| Some(*seat) != first))
        .chain(
            members
                .iter()
                .copied()
                .filter(|seat| Some(*seat) != first && !writers.contains(seat)),
        )
        .collect();
    let turns = u64::try_from(order.len()).ok().filter(|turns| *turns > 0)?;
    let turn = now.saturating_sub(split_at) / SETTLE_TURN_SECS % turns;
    order.get(usize::try_from(turn).ok()?).copied()
}

/// The meta of `metas` (each opened, beside its op) a settle keeps where the siblings carry one
/// token: a refutation before a verification ([`Meta::verification`]) before neither, ties to the
/// lowest op, so every Mac that reads the same siblings picks the same one. `metas` holds only the
/// metas written beside the token ([`settle`]).
fn merged_meta<'a>(
    metas: &[(crate::membership_log::RecordId, &'a [u8])],
) -> Option<(crate::membership_log::RecordId, &'a [u8])> {
    let merit = |bytes: &[u8]| {
        if refuted(bytes) {
            2
        } else if Meta::from_json(bytes).is_some_and(|meta| meta.verification().is_some()) {
            1
        } else {
            0
        }
    };
    metas
        .iter()
        .min_by(|(a_id, a), (b_id, b)| merit(b).cmp(&merit(a)).then(a_id.cmp(b_id)))
        .copied()
}

/// The `minted_at` the meta `bytes` names, read from its JSON whatever its schema (a later
/// build's refutation included): which token of two concurrent replacements is the later mint
/// ([`settle`]). `None` where it names none.
fn minted_at_of(bytes: &[u8]) -> Option<u64> {
    aterm_json::from_slice::<aterm_json::Value>(bytes)
        .ok()?
        .get("minted_at")?
        .as_u64()
}

/// Each of `values` (a register's ops, [`Log::everyone_values`]) opened, where every one is: a
/// `Put` whose inner seal this Mac opens.
fn opened_values(values: &[Option<SecretBytes>]) -> Option<Vec<&[u8]>> {
    values
        .iter()
        .map(|value| match inner::of(value.as_ref()?.as_slice()) {
            Form::Opened(bytes) => Some(bytes),
            Form::Unreadable(_) | Form::Unsealed => None,
        })
        .collect()
}

/// SETTLES CONCURRENT WRITES OF THE ACCOUNT `key`'S PAIR (§5.6): two Macs that wrote the pair at
/// once (a verification and a refutation, say) leave a name's register with siblings, which split
/// the pair's draft (`mismatched (draft)`), so no Mac serves it. One Mac writes the pair again, in
/// one draft standing on every sibling it read ([`MemberEntry::settles`]), never on a write it did
/// not see:
///
/// - the token is one no Mac refuted where one stands (a refuted token is never served, so
///   keeping it would leave the account no token any Mac uses), then the latest mint's: of the
///   tokens the token's register holds, the one beside which a meta names the greatest
///   `minted_at`, by the minting Mac's clock (a person's later replacement stands over an earlier
///   one, so no Mac's row of the account moves back to a token the settle dropped), the
///   register's winner where two name the same;
/// - the meta is the one that weighs most ([`merged_meta`]: refuted, then verified, then
///   neither), byte for byte as the log holds it, of the metas whose draft wrote that token
///   beside them: a meta whose draft's token the register no longer holds is another token's (a
///   verification whose put of the token it compared a concurrent replacement took out, as a
///   re-put of a value held is), so it never lands beside this one. A meta that is a reseal's
///   carry counts as the write it carries
///   ([`stands_for`](crate::membership_log::entries::OpenedAudienceEntries::stands_for)): a
///   reseal re-puts each register's winner in one draft of its own, and the two winners may come
///   from two drafts. A token beside which no meta stands is not kept, and where no meta stands
///   beside any token the register holds, nothing is written. Where a write a meta stands for
///   holds a `Put` this Mac does not open (which may be the token beside it), the pair is not
///   this Mac's to settle: it writes nothing, and a Mac that opens it settles it.
///
/// Whose turn it is to write is a seat's ([`settle_turn`], at `now`, unix seconds): first the
/// writer of the kept pair where the tokens differ, then the siblings' current writers,
/// then every other current seat, each in seat order for [`SETTLE_TURN_SECS`] from the split, so
/// a Mac that never settles (off the LAN, asleep, on a build with no settle) holds the pair for
/// one turn. A writer the log now removes takes no turn. This Mac writes where the turn is its
/// own seat; any other reads [`Settled::Theirs`] and writes nothing. It never mints, never waits
/// for a vault pass (busy past `by`, it writes nothing; the next reading settles), and nothing of
/// a value leaves it.
///
/// # Errors
/// This Mac's state or records cannot be read or written, or a record addressed to it does not
/// open.
pub fn settle(paths: &Paths, key: &Key, now: u64, by: Instant) -> Result<Settled> {
    let held = |why: &str| Ok(Settled::Held(why.to_owned()));
    let (entry, meta_entry) = (key.entry(), key.meta_entry());
    let Some(snapshot) = read_claude_token_until(paths, &[&entry, &meta_entry], by)? else {
        return held("this Mac's vault state could not be read just now");
    };
    let (log, identity) = (&snapshot.log, &snapshot.identity);
    let Some(opened) = log.open_audience_entries(identity)? else {
        return held("this Mac's vault state could not be read just now");
    };
    let ops = [
        opened.everyone_ops(&entry),
        opened.everyone_ops(&meta_entry),
    ];
    if ops.iter().all(|ops| ops.len() < 2) {
        return Ok(Settled::Nothing);
    }
    let (tokens, metas) = (
        log.everyone_values(&opened, &entry, &ops[0])?,
        log.everyone_values(&opened, &meta_entry, &ops[1])?,
    );
    let (Some(tokens), Some(metas)) = (opened_values(&tokens), opened_values(&metas)) else {
        return held("a write of the pair is a removal, or does not open here");
    };
    let draft_of = |id: &crate::membership_log::RecordId| {
        let record = log.get(id)?;
        let crate::membership_log::Body::Put { epoch, .. } = record.body() else {
            return None;
        };
        Some((*record.actor(), record.authored_ms(), *epoch))
    };
    let Some(first) = tokens.first() else {
        return held("the pair holds no token");
    };
    let membership = log.membership_ref();
    let current = |seat: &MemberId| membership.removal(seat).is_none();
    // The writers a current Mac is (a removed one settles nothing, so its turn never comes), and
    // when the latest sibling was drafted.
    let (mut writers, mut split_ms) = (BTreeSet::new(), 0);
    for id in ops.iter().flatten() {
        let Some((actor, authored_ms, _)) = draft_of(id) else {
            return held("a write of the pair is a removal, or does not open here");
        };
        if current(&actor) {
            writers.insert(actor);
        }
        split_ms = split_ms.max(authored_ms);
    }
    let one_token = tokens.iter().all(|token| aterm_digest::ct_eq(token, first));
    // Each meta beside the token its draft wrote with it, by that token's first place among the
    // token register's ops: the draft of the write the meta stands for (a reseal's carry stands
    // for the write whose value it carries, never for its own draft, which pairs two registers'
    // winners), where the register still holds the token that draft put. A meta whose draft's
    // token the register no longer holds is another token's (a verification whose put of the
    // token it compared a concurrent replacement took out), so it never lands beside this one.
    // A write this Mac cannot open may be the token a meta stands beside: the pair is then not
    // this Mac's to settle, and a Mac that opens it settles it.
    let mut drafts = Vec::with_capacity(metas.len());
    for id in &ops[1] {
        let Some((actor, authored_ms, epoch)) = draft_of(&opened.stands_for(id)) else {
            return held("a write of the pair is a removal, or does not open here");
        };
        drafts.push((actor, authored_ms, epoch));
    }
    let mut beside = Vec::with_capacity(metas.len());
    let written = log.everyone_puts_of_drafts(&opened, &entry, &drafts)?;
    for ((id, meta), put) in ops[1].iter().zip(metas.iter().copied()).zip(written) {
        let put = match put {
            DraftPut::Wrote(put) => put,
            DraftPut::Nothing => continue,
            DraftPut::Unopened => {
                return held("a write a meta of the pair stands for does not open here");
            }
        };
        let Form::Opened(with) = inner::of(put.as_slice()) else {
            return held("a write of the pair is a removal, or does not open here");
        };
        if let Some(place) = tokens
            .iter()
            .position(|token| aterm_digest::ct_eq(token, with))
        {
            beside.push((place, *id, meta));
        }
    }
    // The token kept is one no Mac refuted where one stands, then the latest mint's: the one
    // whose meta names the greatest `minted_at`, the token register's winner where two name the
    // same. A refuted token is never served, so a settle that kept it beside one that may still
    // verify would leave the account with no token any Mac uses; a person's later replacement
    // stands over an earlier one, and no Mac's row of the account moves back to a token the
    // settle dropped.
    let winner = opened
        .winning_puts
        .get(&(Audience::Everyone, entry.clone()))
        .copied();
    let refuted_token = |at: usize| {
        beside
            .iter()
            .any(|(place, _, meta)| *place == at && refuted(meta))
    };
    let Some(&(place, ..)) = beside.iter().max_by_key(|(place, _, meta)| {
        (
            !refuted_token(*place),
            minted_at_of(meta),
            Some(ops[0][*place]) == winner,
            std::cmp::Reverse(*place),
        )
    }) else {
        return held("no meta written beside a token the pair holds");
    };
    let with_token: Vec<_> = beside
        .iter()
        .filter(|(at, ..)| *at == place)
        .map(|(_, id, meta)| (*id, *meta))
        .collect();
    let Some((kept, meta)) = merged_meta(&with_token) else {
        return held("no meta written beside a token the pair holds");
    };
    let first_turn = if one_token {
        None
    } else {
        draft_of(&opened.stands_for(&kept))
            .map(|(actor, ..)| actor)
            .filter(current)
    };
    let token = tokens[place];
    if !value_grammar(token) {
        return held("the pair's token is not a setup-token");
    }
    let members: BTreeSet<MemberId> = membership
        .members()
        .keys()
        .copied()
        .filter(current)
        .collect();
    let Some(turn) = settle_turn(first_turn, &writers, &members, split_ms / 1_000, now) else {
        return held("the pair has no current seat to settle it");
    };
    if own_actor(log, identity) != Some(turn) {
        return Ok(Settled::Theirs(seat_label(log, &turn)));
    }
    let put =
        |name: String, value: &[u8], settles: &[crate::membership_log::RecordId]| MemberEntry {
            name,
            value: SecretBytes::from(value),
            if_absent: false,
            anchor: crate::client::Anchor::Drop,
            fleet_reads_anchors: false,
            read_as: None,
            settles: Some(settles.iter().copied().collect()),
        };
    let entries = [put(entry, token, &ops[0]), put(meta_entry, meta, &ops[1])];
    Ok(Settled::Written(member_write(
        paths,
        &entries,
        OnBusy::Refuse(by),
    )?))
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
