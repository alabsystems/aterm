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
//! A stored token counts ([`Standing`]) when both names' winning `Put`s come from one draft, the
//! value is a setup-token ([`value_grammar`]), its end ([`Meta::expires_at`]) has not passed, and
//! it is verified, or this Mac minted it (its own seat signed the earliest `Put` of the value). The
//! signer is read from the log, never from the meta, and is shown, never used as a gate. Its
//! exposure is read from the lineage: every epoch that sealed a `Put` of the value, and whether
//! one was handed to a Mac the log now removes.

use crate::client::{
    MemberEntry, MemberUnset, MemberWritten, OnBusy, Paths, member_unset_names, member_waits_here,
    member_write, read_claude_token_until, recipient_label, seat_label,
};
pub use crate::entry_class::Mismatch;
use crate::entry_class::{CLAUDE_TOKEN_META, CLAUDE_TOKEN_PREFIX};
use crate::membership_log::entries::inner::{self, Form};
use crate::membership_log::{Log, MemberId};
use crate::text::Audience;
use crate::{Result, SecretBytes};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

/// What every setup-token begins with.
pub const VALUE_PREFIX: &str = "sk-ant-oat";

/// How long a token stays `soon` before its end: 30 days.
pub const SOON_SECS: u64 = 30 * 86_400;

/// How long [`standing`] waits for a vault writer holding this Mac's state before it reads
/// [`Lookup::Unknown`].
const STANDING_WAIT: Duration = Duration::from_secs(2);

/// One account's key in the Claude realm: `ac-<16 lower-case hex>`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key(String);

impl Key {
    /// The key `key` spells, where it is one.
    #[must_use]
    pub fn parse(key: &str) -> Option<Self> {
        crate::entry_class::claude_token(&format!("{CLAUDE_TOKEN_PREFIX}{key}"))
            .then(|| Self(key.to_owned()))
    }

    /// The key of the token entry `name` (not its meta), where it is one.
    #[must_use]
    pub fn of_entry(name: &str) -> Option<Self> {
        if name.ends_with(CLAUDE_TOKEN_META) {
            return None;
        }
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
    /// What compared: `windows`, the rate-limit windows.
    pub by: String,
    /// The seven-day window's reset the token session read (unix seconds), where it read one.
    pub seven_day_resets_at: Option<u64>,
    /// The five-hour window's reset the token session read (unix seconds), where it read one.
    pub five_hour_resets_at: Option<u64>,
}

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
    /// Absent at mint; written by the verifying draft ([`verify`]).
    pub verified: Option<Verified>,
}

const SCHEMA: i64 = 1;

impl Meta {
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
            "verified": verified,
        });
        aterm_json::to_string(&value).unwrap_or_default()
    }

    /// The record `bytes` holds, where it is one of schema 1.
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
    /// `unverified`: it serves only its minting Mac (`served_here` there), exposure beside it.
    Unverified {
        /// This Mac minted it.
        served_here: bool,
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
            Self::Unverified {
                served_here,
                exposed,
            } => {
                let mut words = if *served_here {
                    "unverified (served here, where it was minted)".to_owned()
                } else {
                    "unverified (served only where it was minted)".to_owned()
                };
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
    /// Whether the meta carries a verification.
    #[must_use]
    pub fn verified(&self) -> bool {
        self.meta
            .as_ref()
            .is_some_and(|meta| meta.verified.is_some())
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
        if !self.verified() {
            return State::Unverified {
                served_here: self.minted_here,
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

    /// Whether sessions on this Mac start on the token at `now`: `active`, `soon` or `exposed`
    /// on every Mac, `unverified` on its minting Mac only.
    #[must_use]
    pub fn in_use(&self, now: u64) -> bool {
        match self.state(now) {
            State::Active | State::Soon | State::Exposed(_) => true,
            State::Unverified { served_here, .. } => served_here,
            _ => false,
        }
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
    write_pair(paths, key, token, meta, on_busy)
}

fn write_pair(
    paths: &Paths,
    key: &Key,
    token: &SecretBytes,
    meta: &Meta,
    on_busy: OnBusy,
) -> Result<MemberWritten> {
    let entry = |name: String, value: SecretBytes| MemberEntry {
        name,
        value,
        if_absent: false,
        anchor: crate::client::Anchor::Drop,
        fleet_reads_anchors: false,
    };
    member_write(
        paths,
        &[
            entry(key.entry(), token.clone()),
            entry(key.meta_entry(), SecretBytes::from(meta.to_json())),
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
    let judged = judge(
        &snapshot.log,
        &snapshot.identity,
        &opened,
        key,
        token.as_ref(),
        meta.as_ref(),
    )?;
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
    let mut out = Vec::with_capacity(keys.len());
    for key in keys {
        let value = |name: String| {
            held.and_then(|entries| entries.get(&name))
                .map(|e| &e.value)
        };
        let read = judge(
            &snapshot.log,
            &snapshot.identity,
            &opened,
            &key,
            value(key.entry()),
            value(key.meta_entry()),
        )?;
        out.push(read.standing);
    }
    Ok(Lookup::Found(out))
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
    };
    let token = match (check, token) {
        (Check::Readable, Some(token)) => token.clone(),
        _ => {
            return Ok(TokenRead {
                token: None,
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
/// one draft (§5.6): the same token, the meta with the verification. Only the Mac that minted the
/// token verifies it; any other is refused, and so is a token that does not read as one here.
///
/// # Errors
/// As [`write`].
pub fn verify(
    paths: &Paths,
    key: &Key,
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
    let (Some(token), Some(meta)) = (read.token(), read.standing.meta.as_ref()) else {
        return Ok(MemberWritten::Refused(format!(
            "the token for {} reads {} here",
            key.as_str(),
            read.standing.state(now_secs()).words()
        )));
    };
    if !read.standing.minted_here {
        return Ok(MemberWritten::Refused(
            "only the Mac that minted a token verifies it".to_owned(),
        ));
    }
    let mut meta = meta.clone();
    meta.verified = Some(evidence);
    write_pair(paths, key, token, &meta, on_busy)
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
