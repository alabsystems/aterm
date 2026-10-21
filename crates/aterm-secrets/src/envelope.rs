// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The signed envelope a vault URL serves (docs/DESIGN-secrets-2026-10-20.md §5).
//!
//! ```text
//! alab-vault/1
//! name: alab
//! seq: 7
//! time: 1792530000
//! signer: <padded base64, 32 bytes>
//! [previous-signer: <padded base64, 32 bytes>]   (one pair per key rotation, oldest first)
//! [endorsement: <padded base64, 64 bytes>]
//! locker: admin
//! -----BEGIN AGE ENCRYPTED FILE-----
//! …
//! -----END AGE ENCRYPTED FILE-----
//! locker: 1
//! …
//! signature: <padded base64, 64 bytes>
//! ```
//!
//! The signature is Ed25519 (ring) over every byte before the `signature:` line. This module
//! knows nothing of what a locker holds: [`crate::client`] encrypts and decrypts them.
//!
//! The `previous-signer` and `endorsement` pairs are the handoff chain: every key the vault
//! signed with before `signer`, oldest first, each with its signature over the key after it
//! (the next pair's `previous-signer`, and for the last pair `signer`). Every version carries
//! the whole chain, so a machine pinned to any key the vault ever had follows it to `signer`,
//! however many rotations it slept through, and so does a new Mac whose build pins the
//! vault's first key. A vault that never rotated carries no pair; one that rotated once
//! carries one, exactly as before chains. A chain only grows: a reader and the server refuse
//! a version that drops or changes a link of the one they hold ([`Envelope::check_extends`]),
//! so a rotation by a build that carried only the latest link cannot cut the vault off from
//! its first key.
//!
//! The grammar has exactly one spelling of every envelope: each header once, in this order
//! (the handoff pairs at most [`MAX_HANDOFF`] times), `key: value` with one space; numbers in
//! decimal without leading zeros; keys and signatures in canonical padded base64
//! ([`crate::b64`]); LF line ends and no CR anywhere; a locker label `admin` or a decimal
//! from 1 without leading zeros, each label at most once; an armored block from its exact
//! `BEGIN` line to its exact `END` line, every line between them 1 to 64 characters of the
//! base64 alphabet (what else the armor must be is [`crate::age::dearmor`]'s to judge); and
//! the `signature:` line last, followed by exactly one LF. Errors name the line and what is
//! wrong, never its text.

use crate::{Error, b64};
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};

/// The first line.
pub const MAGIC: &str = "alab-vault/1";
/// The largest envelope accepted.
pub const MAX_ENVELOPE: usize = 16 << 20;
/// The most lockers accepted.
pub const MAX_LOCKERS: usize = 64;
/// The most handoff pairs accepted: one per key rotation in the vault's life. A vault that
/// rotated this often is made again (`aterm secrets create`), not rotated once more.
pub const MAX_HANDOFF: usize = 64;

/// The armor's first line (C2SP age.md).
pub const ARMOR_BEGIN: &str = "-----BEGIN AGE ENCRYPTED FILE-----";
/// The armor's last line.
pub const ARMOR_END: &str = "-----END AGE ENCRYPTED FILE-----";
/// The widest line inside an armor.
const ARMOR_COLUMNS: usize = 64;
/// The most digits a numbered locker label has (64 lockers need two; this leaves room).
const MAX_LABEL_DIGITS: usize = 9;
/// The length of the `signature:` line: the key, a space, 88 characters of base64, the LF.
const SIGNATURE_LINE: usize = "signature: ".len() + 88 + 1;

/// One locker: a label (`admin`, or `1`, `2`, …) and an armored age file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locker {
    /// `admin` or a decimal index.
    pub label: String,
    /// The armored age file, from `-----BEGIN` through `-----END …-----` and its LF.
    pub armored: String,
}

/// A parsed envelope (structure only; nothing verified yet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// The vault's name.
    pub name: String,
    /// The version, ≥ 1.
    pub seq: u64,
    /// Unix seconds when it was signed (informational).
    pub time: u64,
    /// The signing key.
    pub signer: [u8; 32],
    /// The handoff chain: every key `signer` replaced, oldest first, each with its
    /// endorsement of the key after it (the last one's, of `signer`). Empty until the vault
    /// first rotates.
    pub handoff: Vec<([u8; 32], [u8; 64])>,
    /// The lockers, in file order.
    pub lockers: Vec<Locker>,
    /// The signature.
    pub signature: [u8; 64],
    raw: Vec<u8>,
    signed_len: usize,
}

/// What signing needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsigned {
    /// The vault's name.
    pub name: String,
    /// The version.
    pub seq: u64,
    /// Unix seconds.
    pub time: u64,
    /// The handoff chain, oldest first ([`Envelope::handoff`]).
    pub handoff: Vec<([u8; 32], [u8; 64])>,
    /// The lockers.
    pub lockers: Vec<Locker>,
}

/// Whether an envelope's signer is the pin, or a key the pin handed off to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pin {
    /// Signed by the pinned key.
    Same,
    /// Signed by a later key the handoff chain leads to from the pinned one: re-pin to this.
    Rotated([u8; 32]),
    /// The pin gave way to the key this build names ([`Retired::gives_way`]), and the signer
    /// is that key or one the chain leads to from it: re-pin to this. What was held under the
    /// old pin floors nothing, since the admin removed may have signed it.
    Replaced([u8; 32]),
}

/// What a build knows of the keys the ALab vault rotated away from (§5). Every admin held
/// the vault key, so an admin a rotation removed keeps a copy of the key it replaced and can
/// still sign with it, or endorse a key of its own with it. Nothing such a key signs is taken
/// again, and a pin that is such a key, or was reached through such a key's endorsement of a
/// key this build does not know, gives way to the key that replaced it, so a fork the
/// removed admin endorses is not followed, nor kept by a machine that followed it before the
/// build knew. A retired key stays in the handoff chain: its endorsement of the key after it
/// is what lets a machine pinned before it follow.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Retired {
    /// Keys no reader and no server takes an envelope signed by ([`crate::ALAB_RETIRED`]).
    pub(crate) keys: Vec<[u8; 32]>,
    /// The key a pin among `keys` gives way to ([`crate::ALAB_SIGNER`]).
    pub(crate) current: Option<[u8; 32]>,
}

#[cfg(test)]
thread_local! {
    /// What [`Retired::compiled`] answers on this thread instead, in a test of a flow that
    /// reaches it (a client's sync, a join) as if this build retired keys.
    pub(crate) static TEST_RETIRED: std::cell::RefCell<Option<Retired>> =
        const { std::cell::RefCell::new(None) };
}

impl Retired {
    /// What this build carries: [`crate::ALAB_RETIRED`] and [`crate::ALAB_SIGNER`].
    pub(crate) fn compiled() -> Retired {
        #[cfg(test)]
        if let Some(retired) = TEST_RETIRED.with(|retired| retired.borrow().clone()) {
            return retired;
        }
        let key = |text: &str| {
            b64::decode_padded(text).and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
        };
        Retired {
            keys: crate::ALAB_RETIRED
                .iter()
                .filter_map(|text| key(text))
                .collect(),
            current: key(crate::ALAB_SIGNER),
        }
    }

    /// Whether `key` is one of the ALab vault's: the key this build names, or a retired one.
    pub(crate) fn knows(&self, key: &[u8; 32]) -> bool {
        self.current.as_ref() == Some(key) || self.keys.contains(key)
    }

    /// Whether a pin reached through `followed` (the keys before it, oldest first: the handoff
    /// chain of the version that moved the pin there) gives way to [`Retired::current`]: the
    /// pin is retired, or a retired key in that lineage endorsed a key this build does not
    /// know. The vault's own successor of a retired key is retired too or the current one, so
    /// such an endorsement is the removed admin's, made with its copy (a fork), and whatever
    /// the lineage reached from it is the fork's too, however it was signed since.
    pub(crate) fn gives_way(&self, pin: &[u8; 32], followed: &[[u8; 32]]) -> bool {
        self.keys.contains(pin)
            || (0..followed.len()).any(|index| {
                let next = followed.get(index + 1).unwrap_or(pin);
                self.keys.contains(&followed[index]) && !self.knows(next)
            })
    }
}

/// One handoff pair: a key the vault signed with, and its endorsement of the key after it.
pub type Link = ([u8; 32], [u8; 64]);

/// What a reader holds of a vault, for [`verify_for_reader`]: the version it last took, and
/// that version's signer and handoff chain while it still has the version itself, which a
/// newer one must carry on ([`Envelope::check_extends`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Floor {
    /// The seq held.
    pub seq: u64,
    /// The SHA-256 of the envelope held.
    pub digest: [u8; 32],
    /// Its signer and handoff chain, when the reader still has it.
    pub chain: Option<([u8; 32], Vec<Link>)>,
    /// Whether the reader applied it: the same envelope again is [`Verdict::Unchanged`] when
    /// it did, [`Verdict::New`] when it holds it unapplied ([`Floor::below`]).
    pub applied: bool,
}

impl Floor {
    /// The floor of a version known only by its seq and digest.
    pub fn version(seq: u64, digest: [u8; 32]) -> Floor {
        Floor {
            seq,
            digest,
            chain: None,
            applied: true,
        }
    }

    /// The floor of an envelope the reader holds whole.
    pub fn of(envelope: &Envelope) -> Floor {
        Floor {
            seq: envelope.seq,
            digest: envelope.digest(),
            chain: Some((envelope.signer, envelope.handoff.clone())),
            applied: true,
        }
    }

    /// The floor of an envelope the reader holds whole but has not applied: that envelope
    /// reads as new, every older one and another at its seq are refused, and a next one must
    /// carry its handoff chain on.
    pub fn below(envelope: &Envelope) -> Floor {
        Floor {
            applied: false,
            ..Floor::of(envelope)
        }
    }
}

/// What a reader concluded about an envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Newer than what the reader holds: apply it.
    New,
    /// The same version and the same bytes the reader holds.
    Unchanged,
}

/// The next version to publish. Exhausting the admitted u64 sequence is an error,
/// never a panic or a wrap to an older version.
#[cfg(any(unix, test))]
pub(crate) fn next_seq(seq: u64) -> crate::Result<u64> {
    seq.checked_add(1).ok_or_else(|| {
        Error::new("the vault sequence is exhausted; create a new vault instead of wrapping it")
    })
}

/// True when `name` is a vault name: `[a-z0-9][a-z0-9-]{0,62}`. The server's paths and a
/// URL's last segment are held to this too, so a name never reaches a file system path
/// with a `/` or a `.` in it.
pub fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 63
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|&b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// True when `label` is a locker label: `admin`, or a decimal from 1 without leading zeros.
pub fn valid_label(label: &str) -> bool {
    label == "admin"
        || (!label.is_empty()
            && label.len() <= MAX_LABEL_DIGITS
            && !label.starts_with('0')
            && label.bytes().all(|b| b.is_ascii_digit()))
}

/// Check one armored block's delimiters and lines: the exact `BEGIN` line, at least one line
/// of 1 to 64 characters of the base64 alphabet, the exact `END` line, and a final LF. The
/// armor's own rules (full 64-column lines, canonical padding) are the age module's.
pub fn check_armor(armored: &str) -> crate::Result<()> {
    let Some(body) = armored.strip_suffix('\n') else {
        return Err(Error::new("armored block: does not end with LF"));
    };
    let mut lines = body.split('\n');
    if lines.next() != Some(ARMOR_BEGIN) {
        return Err(Error::new(
            "armored block: does not start with the BEGIN line",
        ));
    }
    let mut inner = 0usize;
    let mut ended = false;
    for line in lines {
        if ended {
            return Err(Error::new("armored block: lines follow the END line"));
        }
        if line == ARMOR_END {
            ended = true;
            continue;
        }
        if !armor_line_ok(line) {
            return Err(Error::new(format!(
                "armored block: line {} is not 1 to {ARMOR_COLUMNS} base64 characters",
                inner + 2
            )));
        }
        inner += 1;
    }
    if !ended {
        return Err(Error::new("armored block: no END line"));
    }
    if inner == 0 {
        return Err(Error::new("armored block: empty"));
    }
    Ok(())
}

fn armor_line_ok(line: &str) -> bool {
    !line.is_empty()
        && line.len() <= ARMOR_COLUMNS
        && line
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
}

/// The lines of a text that ends in LF, each with its byte offset.
struct Lines<'a> {
    text: &'a str,
    pos: usize,
    number: usize,
}

impl<'a> Lines<'a> {
    fn new(text: &'a str) -> Self {
        Lines {
            text,
            pos: 0,
            number: 0,
        }
    }

    /// The next line without consuming it: (1-based number, offset, text).
    fn peek(&self) -> Option<(usize, usize, &'a str)> {
        if self.pos >= self.text.len() {
            return None;
        }
        let rest = &self.text[self.pos..];
        let end = rest.find('\n').unwrap_or(rest.len());
        Some((self.number + 1, self.pos, &rest[..end]))
    }

    fn advance(&mut self) -> Option<(usize, usize, &'a str)> {
        let line = self.peek()?;
        self.pos += line.2.len() + 1;
        self.number += 1;
        Some(line)
    }

    /// The next line, which must be `key: value`; returns the value.
    fn header(&mut self, key: &str) -> crate::Result<&'a str> {
        let (number, _, line) = self
            .advance()
            .ok_or_else(|| Error::new(format!("envelope: ends before `{key}:`")))?;
        line.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix(": "))
            .ok_or_else(|| Error::new(format!("envelope line {number}: expected `{key}:`")))
    }
}

fn decimal(text: &str, what: &str, min: u64) -> crate::Result<u64> {
    let canonical = !text.is_empty()
        && text.len() <= 20
        && text.bytes().all(|b| b.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    let value = if canonical {
        text.parse::<u64>().ok()
    } else {
        None
    };
    match value {
        Some(value) if value >= min => Ok(value),
        _ => Err(Error::new(format!(
            "envelope: `{what}` is not a decimal number{} without leading zeros",
            if min > 0 { " from 1" } else { "" }
        ))),
    }
}

fn key_bytes<const N: usize>(text: &str, what: &str) -> crate::Result<[u8; N]> {
    b64::decode_padded(text)
        .and_then(|bytes| <[u8; N]>::try_from(bytes.as_slice()).ok())
        .ok_or_else(|| {
            Error::new(format!(
                "envelope: `{what}` is not {N} bytes of canonical padded base64"
            ))
        })
}

/// Parse an envelope's structure: the magic line, each header once and in order, labels
/// unique (`admin` at most once), each armored block well-delimited, the `signature:` line
/// last followed by exactly one LF, sizes within [`MAX_ENVELOPE`] and [`MAX_LOCKERS`].
/// Refuses CR anywhere.
pub fn parse(bytes: &[u8]) -> crate::Result<Envelope> {
    if bytes.len() > MAX_ENVELOPE {
        return Err(Error::new(format!(
            "envelope: larger than {MAX_ENVELOPE} bytes"
        )));
    }
    if bytes.contains(&b'\r') {
        return Err(Error::new("envelope: contains a CR; lines end in LF alone"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| Error::new("envelope: not UTF-8 text"))?;
    if !text.ends_with('\n') {
        return Err(Error::new("envelope: does not end with LF"));
    }
    let mut lines = Lines::new(text);
    if lines.advance().map(|line| line.2) != Some(MAGIC) {
        return Err(Error::new(format!(
            "envelope: the first line is not `{MAGIC}`"
        )));
    }
    let name = lines.header("name")?;
    if !valid_name(name) {
        return Err(Error::new(
            "envelope: `name` is not a vault name ([a-z0-9][a-z0-9-]{0,62})",
        ));
    }
    let seq = decimal(lines.header("seq")?, "seq", 1)?;
    let time = decimal(lines.header("time")?, "time", 0)?;
    let signer = key_bytes::<32>(lines.header("signer")?, "signer")?;
    let mut handoff = Vec::new();
    loop {
        match lines.peek() {
            Some((number, _, line)) if line.starts_with("previous-signer:") => {
                if handoff.len() == MAX_HANDOFF {
                    return Err(Error::new(format!(
                        "envelope line {number}: more than {MAX_HANDOFF} handoffs"
                    )));
                }
                let previous =
                    key_bytes::<32>(lines.header("previous-signer")?, "previous-signer")?;
                let endorsement = key_bytes::<64>(lines.header("endorsement")?, "endorsement")?;
                handoff.push((previous, endorsement));
            }
            Some((number, _, line)) if line.starts_with("endorsement:") => {
                return Err(Error::new(format!(
                    "envelope line {number}: `endorsement:` without `previous-signer:`"
                )));
            }
            _ => break,
        }
    }
    let mut lockers: Vec<Locker> = Vec::new();
    while let Some((number, _, line)) = lines.peek() {
        let Some(label) = line.strip_prefix("locker: ") else {
            break;
        };
        lines.advance();
        if !valid_label(label) {
            return Err(Error::new(format!(
                "envelope line {number}: a locker label is `admin` or a decimal from 1 without leading zeros"
            )));
        }
        if lockers.iter().any(|locker| locker.label == label) {
            return Err(Error::new(format!(
                "envelope line {number}: a second locker with the same label"
            )));
        }
        if lockers.len() == MAX_LOCKERS {
            return Err(Error::new(format!(
                "envelope: more than {MAX_LOCKERS} lockers"
            )));
        }
        let start = match lines.peek() {
            Some((_, start, ARMOR_BEGIN)) => start,
            _ => {
                return Err(Error::new(format!(
                    "envelope line {}: expected the armor's BEGIN line",
                    number + 1
                )));
            }
        };
        let mut end = None;
        while let Some((_, offset, line)) = lines.advance() {
            if line == ARMOR_END {
                end = Some(offset + line.len() + 1);
                break;
            }
        }
        let end = end.ok_or_else(|| {
            Error::new(format!(
                "envelope: the armor of the locker on line {number} has no END line"
            ))
        })?;
        let armored = &text[start..end];
        check_armor(armored)
            .map_err(|e| Error::new(format!("envelope: the locker on line {number}: {e}")))?;
        lockers.push(Locker {
            label: label.to_owned(),
            armored: armored.to_owned(),
        });
    }
    let (number, signed_len, line) = lines
        .advance()
        .ok_or_else(|| Error::new("envelope: no `signature:` line"))?;
    let Some(signature) = line.strip_prefix("signature: ") else {
        return Err(Error::new(format!(
            "envelope line {number}: expected `locker:` or `signature:`"
        )));
    };
    let signature = key_bytes::<64>(signature, "signature")?;
    if lines.peek().is_some() {
        return Err(Error::new(format!(
            "envelope: lines follow the `signature:` line (line {})",
            number + 1
        )));
    }
    Ok(Envelope {
        name: name.to_owned(),
        seq,
        time,
        signer,
        handoff,
        lockers,
        signature,
        raw: bytes.to_vec(),
        signed_len,
    })
}

/// Render and sign: the exact bytes [`parse`] reads back, signed with the Ed25519 key whose
/// seed is `seed`. A handoff chain with any link whose endorsement does not verify (its key's
/// signature over the next key, for this name) is refused: every reader pinned at or before
/// that link would refuse the envelope.
pub fn sign(unsigned: &Unsigned, seed: &[u8; 32]) -> crate::Result<Vec<u8>> {
    let signer = signer_public(seed)?;
    if let Some(link) = broken_link(&unsigned.handoff, 0, &unsigned.name, &signer) {
        return Err(Error::new(format!(
            "envelope: the endorsement of handoff {} of {} is not its key's signature of the next key for this vault",
            link + 1,
            unsigned.handoff.len()
        )));
    }
    sign_unchecked(unsigned, seed)
}

/// [`sign`] without the handoff check (tests build refused envelopes with it).
pub(crate) fn sign_unchecked(unsigned: &Unsigned, seed: &[u8; 32]) -> crate::Result<Vec<u8>> {
    if !valid_name(&unsigned.name) {
        return Err(Error::new(
            "envelope: the name is not a vault name ([a-z0-9][a-z0-9-]{0,62})",
        ));
    }
    if unsigned.seq == 0 {
        return Err(Error::new("envelope: seq starts at 1"));
    }
    if unsigned.lockers.len() > MAX_LOCKERS {
        return Err(Error::new(format!(
            "envelope: more than {MAX_LOCKERS} lockers"
        )));
    }
    if unsigned.handoff.len() > MAX_HANDOFF {
        return Err(Error::new(format!(
            "envelope: more than {MAX_HANDOFF} handoffs; a vault whose key rotated that often is made again with aterm secrets create"
        )));
    }
    for (index, locker) in unsigned.lockers.iter().enumerate() {
        if !valid_label(&locker.label) {
            return Err(Error::new(format!(
                "envelope: locker {} has a label that is neither `admin` nor a decimal from 1",
                index + 1
            )));
        }
        if unsigned.lockers[..index]
            .iter()
            .any(|other| other.label == locker.label)
        {
            return Err(Error::new(format!(
                "envelope: locker {} repeats a label",
                index + 1
            )));
        }
        check_armor(&locker.armored)
            .map_err(|e| Error::new(format!("envelope: locker {}: {e}", index + 1)))?;
    }
    let pair = key_pair(seed)?;
    let mut text = String::new();
    text.push_str(MAGIC);
    text.push('\n');
    text.push_str(&format!("name: {}\n", unsigned.name));
    text.push_str(&format!("seq: {}\n", unsigned.seq));
    text.push_str(&format!("time: {}\n", unsigned.time));
    text.push_str(&format!(
        "signer: {}\n",
        b64::encode_padded(pair.public_key().as_ref())
    ));
    for (previous, endorsement) in &unsigned.handoff {
        text.push_str(&format!(
            "previous-signer: {}\n",
            b64::encode_padded(previous)
        ));
        text.push_str(&format!(
            "endorsement: {}\n",
            b64::encode_padded(endorsement)
        ));
    }
    for locker in &unsigned.lockers {
        text.push_str(&format!("locker: {}\n", locker.label));
        text.push_str(&locker.armored);
    }
    if text.len() + SIGNATURE_LINE > MAX_ENVELOPE {
        return Err(Error::new(format!(
            "envelope: larger than {MAX_ENVELOPE} bytes"
        )));
    }
    let signature = pair.sign(text.as_bytes());
    text.push_str(&format!(
        "signature: {}\n",
        b64::encode_padded(signature.as_ref())
    ));
    let bytes = text.into_bytes();
    // What was signed must be what every reader parses: refuse rather than publish an
    // envelope nobody would accept (too large, for one).
    parse(&bytes)?;
    Ok(bytes)
}

fn key_pair(seed: &[u8; 32]) -> crate::Result<Ed25519KeyPair> {
    Ed25519KeyPair::from_seed_unchecked(seed)
        .map_err(|_| Error::new("envelope: the signing seed is not a usable Ed25519 seed"))
}

/// The Ed25519 public key of a seed.
pub fn signer_public(seed: &[u8; 32]) -> crate::Result<[u8; 32]> {
    let pair = key_pair(seed)?;
    <[u8; 32]>::try_from(pair.public_key().as_ref())
        .map_err(|_| Error::new("envelope: an Ed25519 public key is not 32 bytes"))
}

/// What a handoff endorsement signs.
fn handoff_message(name: &str, new_signer: &[u8; 32]) -> String {
    format!(
        "{MAGIC} handoff\n{name}\n{}\n",
        b64::encode_padded(new_signer)
    )
}

/// The previous key's endorsement of a new one: an Ed25519 signature by `old_seed` over
/// `alab-vault/1 handoff\n<name>\n<new signer, padded base64>\n`.
pub fn endorse(old_seed: &[u8; 32], name: &str, new_signer: &[u8; 32]) -> crate::Result<[u8; 64]> {
    if !valid_name(name) {
        return Err(Error::new(
            "envelope: the name is not a vault name ([a-z0-9][a-z0-9-]{0,62})",
        ));
    }
    let pair = key_pair(old_seed)?;
    let signature = pair.sign(handoff_message(name, new_signer).as_bytes());
    <[u8; 64]>::try_from(signature.as_ref())
        .map_err(|_| Error::new("envelope: an Ed25519 signature is not 64 bytes"))
}

fn verify_endorsement(
    previous: &[u8; 32],
    endorsement: &[u8; 64],
    name: &str,
    signer: &[u8; 32],
) -> Result<(), ring::error::Unspecified> {
    UnparsedPublicKey::new(&ED25519, previous)
        .verify(handoff_message(name, signer).as_bytes(), endorsement)
}

/// The first link of `handoff`, from index `from` on, whose endorsement does not verify:
/// link `k`'s key must have signed link `k + 1`'s key, and the last link's key `signer`.
fn broken_link(
    handoff: &[([u8; 32], [u8; 64])],
    from: usize,
    name: &str,
    signer: &[u8; 32],
) -> Option<usize> {
    (from..handoff.len()).find(|&index| {
        let (previous, endorsement) = &handoff[index];
        let next = handoff.get(index + 1).map_or(signer, |(key, _)| key);
        verify_endorsement(previous, endorsement, name, next).is_err()
    })
}

/// A short fingerprint of a signing key for people to compare: the first 8 bytes of its
/// SHA-256 as `xxxx-xxxx-xxxx-xxxx` lowercase hex.
pub fn fingerprint(public: &[u8; 32]) -> String {
    let digest = aterm_digest::Sha256::digest(public);
    let mut out = String::with_capacity(19);
    for (index, pair) in digest[..8].chunks(2).enumerate() {
        if index > 0 {
            out.push('-');
        }
        for byte in pair {
            out.push_str(&format!("{byte:02x}"));
        }
    }
    out
}

impl Envelope {
    /// The exact bytes parsed.
    pub fn raw(&self) -> &[u8] {
        &self.raw
    }

    /// SHA-256 of [`Envelope::raw`].
    pub fn digest(&self) -> [u8; 32] {
        aterm_digest::Sha256::digest(&self.raw)
    }

    /// Check the signature over the signed bytes with `signer`.
    pub fn verify_signature(&self) -> crate::Result<()> {
        UnparsedPublicKey::new(&ED25519, &self.signer)
            .verify(&self.raw[..self.signed_len], &self.signature)
            .map_err(|_| {
                Error::new(format!(
                    "envelope: the signature does not verify under its signer {}",
                    fingerprint(&self.signer)
                ))
            })
    }

    /// Refuse an envelope whose signer is a retired key ([`Retired`]); a retired key as a
    /// `previous-signer` is not refused.
    pub(crate) fn check_signer(&self, retired: &Retired) -> crate::Result<()> {
        if retired.keys.contains(&self.signer) {
            return Err(Error::new(format!(
                "envelope: signed by {}, a key this vault rotated away from, which an admin it removed may still hold; refused",
                fingerprint(&self.signer)
            )));
        }
        Ok(())
    }

    /// Check the signer against a pin reached through `followed` (the keys before it, oldest
    /// first; empty for a pin set at join). A signer among [`crate::ALAB_RETIRED`] is refused.
    /// Then [`Pin::Same`] when the signer is the pin, or [`Pin::Rotated`] when the pin is a
    /// `previous-signer` of the handoff chain and every link from it on verifies (for this
    /// `name`): the pin endorsed a key that endorsed, in turn, the signer. Links before the
    /// pin are not this reader's to judge. A pin that is among [`crate::ALAB_RETIRED`], or was
    /// reached through a retired key's fork, gives way to [`crate::ALAB_SIGNER`] first, since
    /// the admin removed could endorse a key of its own with the retired one; the chain is
    /// followed from that key instead, and a signer it leads to is [`Pin::Replaced`]. Anything
    /// else is an error naming both fingerprints.
    pub fn check_pin(&self, pin: &[u8; 32], followed: &[[u8; 32]]) -> crate::Result<Pin> {
        self.check_pin_with(pin, followed, &Retired::compiled())
    }

    /// [`Envelope::check_pin`] under `retired` instead of what this build carries.
    pub(crate) fn check_pin_with(
        &self,
        pin: &[u8; 32],
        followed: &[[u8; 32]],
        retired: &Retired,
    ) -> crate::Result<Pin> {
        self.check_signer(retired)?;
        let from = if retired.gives_way(pin, followed) {
            retired.current.ok_or_else(|| {
                Error::new(format!(
                    "envelope: the pinned key {} is, or was reached through, one this vault rotated away from, and this build names no key that replaced it",
                    fingerprint(pin)
                ))
            })?
        } else {
            *pin
        };
        let moved = |signer: [u8; 32]| {
            if from == *pin {
                Pin::Rotated(signer)
            } else {
                Pin::Replaced(signer)
            }
        };
        if self.signer == from {
            return Ok(if from == *pin {
                Pin::Same
            } else {
                Pin::Replaced(self.signer)
            });
        }
        let Some(start) = self
            .handoff
            .iter()
            .position(|(previous, _)| *previous == from)
        else {
            return Err(Error::new(format!(
                "envelope: signed by {}, but the pinned key is {} and it endorsed no handoff to it",
                fingerprint(&self.signer),
                fingerprint(&from)
            )));
        };
        match broken_link(&self.handoff, start, &self.name, &self.signer) {
            None => Ok(moved(self.signer)),
            Some(link) => Err(Error::new(format!(
                "envelope: signed by {}, handed off from the pinned key {}, but the endorsement of handoff {} of {} does not verify",
                fingerprint(&self.signer),
                fingerprint(&from),
                link + 1,
                self.handoff.len()
            ))),
        }
    }

    /// Whether this version's chain names one of `keys` handing off and a link from there on
    /// does not verify: no key of the vault's endorsed what it claims, so the version is
    /// forged (or damaged), whoever signed it.
    #[cfg(unix)]
    pub(crate) fn forges_handoff_from(&self, keys: &[[u8; 32]]) -> bool {
        self.handoff
            .iter()
            .position(|(previous, _)| keys.contains(previous))
            .is_some_and(|start| {
                broken_link(&self.handoff, start, &self.name, &self.signer).is_some()
            })
    }

    /// Where `key` hands off in this version's chain: the index of its link, or the chain's
    /// length when `key` signed this version.
    fn handoff_of(&self, key: &[u8; 32]) -> Option<usize> {
        if self.signer == *key {
            Some(self.handoff.len())
        } else {
            self.handoff
                .iter()
                .position(|(previous, _)| previous == key)
        }
    }

    /// Refuse a version that does not carry on the handoff chain of the version held, signed
    /// by `held_signer` and carrying `held`: every link of `held` must come, in order, right
    /// before `held_signer`'s own link (last, when `held_signer` signed this one too). A chain
    /// only grows, a rotation appending a link and any other publish carrying it unchanged,
    /// so a version that drops a link is a build before chains rotating (it carried the
    /// latest link alone) and would cut every machine pinned before the link off the vault.
    /// Links before `held`'s first are taken: they are history the held version had lost,
    /// which an admin's copy put back ([`Envelope::chain_carried_on`]).
    pub(crate) fn check_extends(&self, held_signer: &[u8; 32], held: &[Link]) -> crate::Result<()> {
        let Some(at) = self.handoff_of(held_signer) else {
            return Err(Error::new(format!(
                "envelope: carries no handoff from {}, which signed the version held here; refused",
                fingerprint(held_signer)
            )));
        };
        let carried = &self.handoff[..at];
        let dropped = (0..held.len()).find(|&index| {
            let back = held.len() - index;
            at < back || carried[at - back] != held[index]
        });
        match dropped {
            None => Ok(()),
            Some(index) => Err(Error::new(format!(
                "envelope: drops or changes handoff {} of {} ({}'s endorsement of the key after it), which the version held here carries; a vault's handoff chain only grows",
                index + 1,
                held.len(),
                fingerprint(&held[index].0)
            ))),
        }
    }

    /// The handoff chain an admin's next version carries after opening this one while it
    /// held a version signed by `held_signer` carrying `held`: this one's own when it carries
    /// `held` on ([`Envelope::check_extends`]), or, when this one only dropped links from the
    /// front of `held` (a rotation by a build before chains, through a server that took it),
    /// `held` with this one's links from `held_signer` on, so the next version restores what
    /// was dropped. Any other difference is refused as `check_extends` refuses it.
    #[cfg(any(unix, test))]
    pub(crate) fn chain_carried_on(
        &self,
        held_signer: &[u8; 32],
        held: &[Link],
    ) -> crate::Result<Vec<Link>> {
        let refused = match self.check_extends(held_signer, held) {
            Ok(()) => return Ok(self.handoff.clone()),
            Err(refused) => refused,
        };
        match self.handoff_of(held_signer) {
            Some(at) if held.ends_with(&self.handoff[..at]) => {
                let mut chain = held.to_vec();
                chain.extend_from_slice(&self.handoff[at..]);
                Ok(chain)
            }
            _ => Err(refused),
        }
    }

    /// The locker labelled `admin`.
    pub fn admin_locker(&self) -> Option<&Locker> {
        self.lockers.iter().find(|locker| locker.label == "admin")
    }
}

/// Everything a reader checks before it decrypts, in order: the structure, `name` equals
/// `expect_name`, the signer is not retired and the pin, reached through `followed`, holds
/// ([`Envelope::check_pin`]), the signature, and the version against what it holds (`held`):
/// newer is [`Verdict::New`] when it also carries on the held version's handoff chain
/// ([`Envelope::check_extends`]), the same seq with the same digest is
/// [`Verdict::Unchanged`], the same seq with other bytes or a lower seq is an error (a
/// rollback or a forked vault). A pin that gave way ([`Pin::Replaced`]) judges nothing by
/// `held`: the admin removed may have signed it, with any seq.
pub fn verify_for_reader(
    bytes: &[u8],
    expect_name: &str,
    pin: &[u8; 32],
    followed: &[[u8; 32]],
    held: Option<&Floor>,
) -> crate::Result<(Envelope, Pin, Verdict)> {
    verify_for_reader_with(
        bytes,
        expect_name,
        pin,
        followed,
        held,
        &Retired::compiled(),
    )
}

/// [`verify_for_reader`] under `retired` instead of what this build carries.
pub(crate) fn verify_for_reader_with(
    bytes: &[u8],
    expect_name: &str,
    pin: &[u8; 32],
    followed: &[[u8; 32]],
    held: Option<&Floor>,
    retired: &Retired,
) -> crate::Result<(Envelope, Pin, Verdict)> {
    let envelope = parse(bytes)?;
    if envelope.name != expect_name {
        return Err(Error::new(format!(
            "envelope: names the vault `{}`, not `{expect_name}`",
            envelope.name
        )));
    }
    let pin = envelope.check_pin_with(pin, followed, retired)?;
    envelope.verify_signature()?;
    let held = held.filter(|_| !matches!(pin, Pin::Replaced(_)));
    let verdict = match held {
        None => Verdict::New,
        Some(floor) if envelope.seq > floor.seq => {
            if let Some((signer, chain)) = &floor.chain {
                envelope.check_extends(signer, chain)?;
            }
            Verdict::New
        }
        Some(floor) if envelope.seq == floor.seq => {
            if !aterm_digest::ct_eq(&envelope.digest(), &floor.digest) {
                return Err(Error::new(format!(
                    "envelope: a different envelope with seq {}, which this machine already holds (a forked vault); refused",
                    floor.seq
                )));
            }
            if floor.applied {
                Verdict::Unchanged
            } else {
                Verdict::New
            }
        }
        Some(floor) => {
            return Err(Error::new(format!(
                "envelope: seq {} is older than seq {}, which this machine already holds (a rollback); refused",
                envelope.seq, floor.seq
            )));
        }
    };
    Ok((envelope, pin, verdict))
}

/// A syntactically valid armored block around `bytes` (not an age file: the envelope
/// never looks inside). For tests of this module and the server's.
#[cfg(test)]
pub(crate) fn test_armor(bytes: &[u8]) -> String {
    let encoded = b64::encode_padded(bytes);
    let mut out = format!("{ARMOR_BEGIN}\n");
    for chunk in encoded.as_bytes().chunks(ARMOR_COLUMNS) {
        out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        out.push('\n');
    }
    out.push_str(ARMOR_END);
    out.push('\n');
    out
}

/// The handoff chain of a vault named `name` that signed with `seeds` in turn: every key but
/// the last, each with its endorsement of the next. For tests of this module and the server's.
#[cfg(test)]
pub(crate) fn test_chain(name: &str, seeds: &[[u8; 32]]) -> Vec<([u8; 32], [u8; 64])> {
    seeds
        .windows(2)
        .map(|pair| {
            let next = signer_public(&pair[1]).expect("public key");
            let endorsement = endorse(&pair[0], name, &next).expect("endorsed");
            (signer_public(&pair[0]).expect("public key"), endorsement)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: [u8; 32] = [7u8; 32];
    const NEW: [u8; 32] = [8u8; 32];
    const STRANGER: [u8; 32] = [9u8; 32];

    fn lockers() -> Vec<Locker> {
        vec![
            Locker {
                label: "admin".to_owned(),
                armored: test_armor(&[1u8; 70]),
            },
            Locker {
                label: "1".to_owned(),
                armored: test_armor(b"everyone"),
            },
        ]
    }

    fn unsigned(seq: u64) -> Unsigned {
        Unsigned {
            name: "alab".to_owned(),
            seq,
            time: 1_792_530_000,
            handoff: Vec::new(),
            lockers: lockers(),
        }
    }

    fn public(seed: &[u8; 32]) -> [u8; 32] {
        signer_public(seed).expect("public key")
    }

    #[test]
    fn a_signed_envelope_renders_exactly_and_parses_back() {
        let bytes = sign(&unsigned(7), &OLD).expect("signed");
        let text = std::str::from_utf8(&bytes).expect("text");
        let signer = b64::encode_padded(&public(&OLD));
        let expected_head = format!(
            "alab-vault/1\nname: alab\nseq: 7\ntime: 1792530000\nsigner: {signer}\nlocker: admin\n{}locker: 1\n{}signature: ",
            test_armor(&[1u8; 70]),
            test_armor(b"everyone")
        );
        assert!(text.starts_with(&expected_head), "{text}");
        assert!(text.ends_with('\n'));
        assert!(!text.ends_with("\n\n"));
        let envelope = parse(&bytes).expect("parsed");
        assert_eq!(envelope.name, "alab");
        assert_eq!(envelope.seq, 7);
        assert_eq!(envelope.time, 1_792_530_000);
        assert_eq!(envelope.signer, public(&OLD));
        assert!(envelope.handoff.is_empty());
        assert_eq!(envelope.lockers, lockers());
        assert_eq!(envelope.raw(), bytes.as_slice());
        assert_eq!(
            envelope.admin_locker().map(|l| l.label.as_str()),
            Some("admin")
        );
        envelope.verify_signature().expect("verifies");
        assert_eq!(envelope.digest(), aterm_digest::Sha256::digest(&bytes));
        assert_eq!(envelope.check_pin(&public(&OLD), &[]), Ok(Pin::Same));
    }

    #[test]
    fn every_single_bit_flip_of_a_signed_envelope_is_refused() {
        let bytes = sign(&unsigned(3), &OLD).expect("signed");
        let pin = public(&OLD);
        verify_for_reader(&bytes, "alab", &pin, &[], None).expect("the original verifies");
        for index in 0..bytes.len() {
            for bit in 0..8 {
                let mut flipped = bytes.clone();
                flipped[index] ^= 1 << bit;
                assert!(
                    verify_for_reader(&flipped, "alab", &pin, &[], None).is_err(),
                    "byte {index} bit {bit} flipped was accepted"
                );
            }
        }
    }

    #[test]
    fn a_truncated_or_extended_envelope_is_refused() {
        let bytes = sign(&unsigned(3), &OLD).expect("signed");
        let pin = public(&OLD);
        for len in 0..bytes.len() {
            assert!(
                verify_for_reader(&bytes[..len], "alab", &pin, &[], None).is_err(),
                "{len}"
            );
        }
        let mut longer = bytes.clone();
        longer.push(b'\n');
        assert!(parse(&longer).is_err());
        let mut crlf = bytes.clone();
        crlf.insert(12, b'\r');
        assert!(parse(&crlf).unwrap_err().0.contains("CR"));
    }

    #[test]
    fn the_wrong_pin_or_the_wrong_name_is_refused() {
        let bytes = sign(&unsigned(3), &OLD).expect("signed");
        let error = verify_for_reader(&bytes, "alab", &public(&STRANGER), &[], None).unwrap_err();
        assert!(error.0.contains(&fingerprint(&public(&OLD))), "{error}");
        assert!(
            error.0.contains(&fingerprint(&public(&STRANGER))),
            "{error}"
        );
        let error = verify_for_reader(&bytes, "other", &public(&OLD), &[], None).unwrap_err();
        assert!(error.0.contains("other"), "{error}");
    }

    #[test]
    fn an_endorsed_rotation_is_accepted_and_re_pins() {
        let endorsement = endorse(&OLD, "alab", &public(&NEW)).expect("endorsed");
        let mut rotated = unsigned(4);
        rotated.handoff = vec![(public(&OLD), endorsement)];
        let bytes = sign(&rotated, &NEW).expect("signed");
        let (envelope, pin, verdict) = verify_for_reader(
            &bytes,
            "alab",
            &public(&OLD),
            &[],
            Some(&Floor::version(3, [0u8; 32])),
        )
        .expect("accepted");
        assert_eq!(pin, Pin::Rotated(public(&NEW)));
        assert_eq!(verdict, Verdict::New);
        assert_eq!(envelope.handoff, vec![(public(&OLD), endorsement)]);
        // Re-pinned, the next envelope by the new key alone is the same signer.
        let next = sign(&unsigned(5), &NEW).expect("signed");
        let (_, pin, _) = verify_for_reader(
            &next,
            "alab",
            &public(&NEW),
            &[],
            Some(&Floor::version(4, [0u8; 32])),
        )
        .expect("accepted");
        assert_eq!(pin, Pin::Same);
        // And the old key no longer verifies an envelope the new one signs without a handoff.
        assert!(verify_for_reader(&next, "alab", &public(&OLD), &[], None).is_err());
    }

    #[test]
    fn an_endorsement_for_another_vault_or_by_another_key_is_refused() {
        let elsewhere = endorse(&OLD, "other", &public(&NEW)).expect("endorsed");
        let mut rotated = unsigned(4);
        rotated.handoff = vec![(public(&OLD), elsewhere)];
        assert!(
            sign(&rotated, &NEW).is_err(),
            "sign refuses a handoff no reader would accept"
        );
        let bytes = sign_unchecked(&rotated, &NEW).expect("rendered");
        let error = verify_for_reader(&bytes, "alab", &public(&OLD), &[], None).unwrap_err();
        assert!(error.0.contains("endorsement"), "{error}");

        // An endorsement the stranger made, claiming to be the pinned key's.
        let forged = endorse(&STRANGER, "alab", &public(&NEW)).expect("endorsed");
        rotated.handoff = vec![(public(&OLD), forged)];
        let bytes = sign_unchecked(&rotated, &NEW).expect("rendered");
        assert!(verify_for_reader(&bytes, "alab", &public(&OLD), &[], None).is_err());

        // An endorsement of a different new key.
        let other_key = endorse(&OLD, "alab", &public(&STRANGER)).expect("endorsed");
        rotated.handoff = vec![(public(&OLD), other_key)];
        let bytes = sign_unchecked(&rotated, &NEW).expect("rendered");
        assert!(verify_for_reader(&bytes, "alab", &public(&OLD), &[], None).is_err());
    }

    /// The seeds of a vault whose key rotated `rotations` times: the first made it, the last
    /// signs now. None is OLD, NEW or STRANGER.
    fn seeds(rotations: usize) -> Vec<[u8; 32]> {
        (0..=rotations)
            .map(|index| [0x40 + u8::try_from(index).expect("small"); 32])
            .collect()
    }

    fn chain(seeds: &[[u8; 32]]) -> Vec<([u8; 32], [u8; 64])> {
        test_chain("alab", seeds)
    }

    fn hex_digest(bytes: &[u8]) -> String {
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes))
    }

    #[test]
    fn no_handoff_and_one_handoff_are_spelled_byte_for_byte_as_before_chains() {
        // The vault m22 serves and every reader of it speak this format. The digests are of
        // what the encoder before handoff chains (8481872747) made of these fixed inputs.
        let none = sign(&unsigned(7), &OLD).expect("signed");
        assert_eq!(
            hex_digest(&none),
            "c576b8917d3fe1ee5f321e2c6e26125bb366995719b52d47d9e93004362dd62b"
        );
        let mut one = unsigned(7);
        let endorsement = endorse(&OLD, "alab", &public(&NEW)).expect("endorsed");
        one.handoff = vec![(public(&OLD), endorsement)];
        let bytes = sign(&one, &NEW).expect("signed");
        let text = std::str::from_utf8(&bytes).expect("text");
        let head = format!(
            "alab-vault/1\nname: alab\nseq: 7\ntime: 1792530000\nsigner: {}\nprevious-signer: {}\nendorsement: {}\nlocker: admin\n",
            b64::encode_padded(&public(&NEW)),
            b64::encode_padded(&public(&OLD)),
            b64::encode_padded(&endorsement)
        );
        assert!(text.starts_with(&head), "{text}");
        assert_eq!(
            hex_digest(&bytes),
            "4b0a33b5f51a0e5402757310f788b88fc15db502a1dbae5788d7e608b8db4f53"
        );
        assert_eq!(parse(&bytes).expect("parsed").handoff, one.handoff);
    }

    #[test]
    fn a_chain_of_rotations_is_followed_from_every_key_the_vault_had() {
        let seeds = seeds(3);
        let mut rotated = unsigned(9);
        rotated.handoff = chain(&seeds);
        let bytes = sign(&rotated, &seeds[3]).expect("signed");
        let text = std::str::from_utf8(&bytes).expect("text");
        let pairs: String = rotated
            .handoff
            .iter()
            .map(|(key, endorsement)| {
                format!(
                    "previous-signer: {}\nendorsement: {}\n",
                    b64::encode_padded(key),
                    b64::encode_padded(endorsement)
                )
            })
            .collect();
        let signer = b64::encode_padded(&public(&seeds[3]));
        assert!(
            text.contains(&format!("\nsigner: {signer}\n{pairs}locker: admin\n")),
            "one pair per rotation, oldest first, after the signer: {text}"
        );
        assert_eq!(parse(&bytes).expect("parsed").handoff, rotated.handoff);
        for seed in &seeds[..3] {
            let (_, pin, _) =
                verify_for_reader(&bytes, "alab", &public(seed), &[], None).expect("followed");
            assert_eq!(pin, Pin::Rotated(public(&seeds[3])));
        }
        let (_, pin, _) =
            verify_for_reader(&bytes, "alab", &public(&seeds[3]), &[], None).expect("the signer");
        assert_eq!(pin, Pin::Same);
        let error = verify_for_reader(&bytes, "alab", &public(&STRANGER), &[], None).unwrap_err();
        assert!(error.0.contains("endorsed no handoff"), "{error}");
    }

    #[test]
    fn a_chain_with_a_broken_link_is_refused_from_every_key_before_the_break() {
        let seeds = seeds(3);
        let mut broken = unsigned(9);
        broken.handoff = chain(&seeds);
        // The middle key endorsed a key of a stranger's, not the one after it.
        broken.handoff[1].1 = endorse(&seeds[1], "alab", &public(&STRANGER)).expect("endorsed");
        let error = sign(&broken, &seeds[3]).expect_err("sign refuses it");
        assert!(error.0.contains("handoff 2 of 3"), "{error}");
        let bytes = sign_unchecked(&broken, &seeds[3]).expect("rendered");
        for seed in &seeds[..2] {
            let error = verify_for_reader(&bytes, "alab", &public(seed), &[], None).unwrap_err();
            assert!(error.0.contains("endorsement of handoff 2 of 3"), "{error}");
        }
        // A reader pinned after the break follows the links it trusts and judges no other.
        let (_, pin, _) =
            verify_for_reader(&bytes, "alab", &public(&seeds[2]), &[], None).expect("followed");
        assert_eq!(pin, Pin::Rotated(public(&seeds[3])));

        // A link dropped or two swapped break the chain just as surely.
        let mut dropped = unsigned(9);
        dropped.handoff = chain(&seeds);
        dropped.handoff.remove(1);
        assert!(sign(&dropped, &seeds[3]).is_err());
        let bytes = sign_unchecked(&dropped, &seeds[3]).expect("rendered");
        assert!(verify_for_reader(&bytes, "alab", &public(&seeds[0]), &[], None).is_err());
        let mut swapped = unsigned(9);
        swapped.handoff = chain(&seeds);
        swapped.handoff.swap(0, 1);
        assert!(sign(&swapped, &seeds[3]).is_err());
        let bytes = sign_unchecked(&swapped, &seeds[3]).expect("rendered");
        assert!(verify_for_reader(&bytes, "alab", &public(&seeds[0]), &[], None).is_err());
    }

    #[test]
    fn the_handoff_chain_is_capped() {
        let seeds = seeds(MAX_HANDOFF + 1);
        let mut long = unsigned(1);
        long.handoff = chain(&seeds[..=MAX_HANDOFF]);
        let bytes = sign(&long, &seeds[MAX_HANDOFF]).expect("64 handoffs");
        assert_eq!(parse(&bytes).expect("parsed").handoff.len(), MAX_HANDOFF);
        let (_, pin, _) =
            verify_for_reader(&bytes, "alab", &public(&seeds[0]), &[], None).expect("followed");
        assert_eq!(pin, Pin::Rotated(public(&seeds[MAX_HANDOFF])));
        long.handoff = chain(&seeds);
        let error = sign(&long, &seeds[MAX_HANDOFF + 1]).expect_err("65 handoffs");
        assert!(error.0.contains("more than 64 handoffs"), "{error}");

        // Parse counts pairs before it judges them: a 65th is refused whoever signed it.
        let pair = format!(
            "previous-signer: {}\nendorsement: {}\n",
            b64::encode_padded(&[1u8; 32]),
            b64::encode_padded(&[2u8; 64])
        );
        let with = |pairs: usize| {
            resigned(|text| {
                text.replacen(
                    "locker: admin\n",
                    &format!("{}locker: admin\n", pair.repeat(pairs)),
                    1,
                )
            })
        };
        assert_eq!(
            parse(&with(MAX_HANDOFF))
                .expect("the control")
                .handoff
                .len(),
            MAX_HANDOFF
        );
        let error = parse(&with(MAX_HANDOFF + 1)).unwrap_err();
        assert!(error.0.contains("more than 64 handoffs"), "{error}");
    }

    #[test]
    fn a_retired_key_signs_nothing_and_a_pin_to_it_follows_the_key_that_replaced_it() {
        let retired = Retired {
            keys: vec![public(&OLD)],
            current: Some(public(&NEW)),
        };
        let verify = |bytes: &[u8], pin: [u8; 32], retired: &Retired| {
            verify_for_reader_with(bytes, "alab", &pin, &[], None, retired).map(|(_, pin, _)| pin)
        };

        // The admin a rotation removed signs with its copy of the old key: a build that does
        // not know the key is retired takes it (the control); one that does refuses it,
        // whatever the reader pinned.
        let direct = sign(&unsigned(9), &OLD).expect("signed");
        assert_eq!(
            verify(&direct, public(&OLD), &Retired::default()),
            Ok(Pin::Same)
        );
        for pin in [public(&OLD), public(&NEW)] {
            let error = verify(&direct, pin, &retired).unwrap_err();
            assert!(error.0.contains("rotated away from"), "{error}");
        }

        // Or it endorses a key of its own with the old one: a reader still pinned to the old
        // key would follow that fork, so its pin gives way to the key that replaced it.
        let mut fork = unsigned(9);
        let endorsement = endorse(&OLD, "alab", &public(&STRANGER)).expect("endorsed");
        fork.handoff = vec![(public(&OLD), endorsement)];
        let fork = sign(&fork, &STRANGER).expect("signed");
        assert_eq!(
            verify(&fork, public(&OLD), &Retired::default()),
            Ok(Pin::Rotated(public(&STRANGER)))
        );
        assert!(verify(&fork, public(&OLD), &retired).is_err());

        // The vault itself, signed by the key that replaced the retired one, which stays in
        // its chain: followed from the retired pin, the same signer under the new one.
        let mut real = unsigned(9);
        let endorsement = endorse(&OLD, "alab", &public(&NEW)).expect("endorsed");
        real.handoff = vec![(public(&OLD), endorsement)];
        let real = sign(&real, &NEW).expect("signed");
        assert_eq!(
            verify(&real, public(&OLD), &retired),
            Ok(Pin::Replaced(public(&NEW)))
        );
        assert_eq!(verify(&real, public(&NEW), &retired), Ok(Pin::Same));
        // A version with no chain still moves a reader off the retired pin.
        let bare = sign(&unsigned(10), &NEW).expect("signed");
        assert_eq!(
            verify(&bare, public(&OLD), &retired),
            Ok(Pin::Replaced(public(&NEW)))
        );
        // With no key named to replace it, a retired pin trusts nothing.
        let unreplaced = Retired {
            keys: vec![public(&OLD)],
            current: None,
        };
        assert!(verify(&real, public(&OLD), &unreplaced).is_err());
    }

    #[test]
    fn a_pin_the_removed_admin_led_a_reader_to_gives_way_and_what_it_held_floors_nothing() {
        // The vault rotated OLD to NEW, and the admin that rotation removed kept OLD. Before a
        // build retired OLD, a reader still pinned to it took what that admin served it.
        let retired = Retired {
            keys: vec![public(&OLD)],
            current: Some(public(&NEW)),
        };
        let mut real = unsigned(10);
        real.handoff = vec![(
            public(&OLD),
            endorse(&OLD, "alab", &public(&NEW)).expect("endorsed"),
        )];
        let real = sign(&real, &NEW).expect("signed");
        let verify = |bytes: &[u8], pin: [u8; 32], followed: &[[u8; 32]], held: &Floor| {
            let take = |retired: &Retired| {
                verify_for_reader_with(bytes, "alab", &pin, followed, Some(held), retired)
                    .map(|(_, pin, verdict)| (pin, verdict))
            };
            (take(&Retired::default()), take(&retired))
        };
        let keys = |envelope: &Envelope| -> Vec<[u8; 32]> {
            envelope.handoff.iter().map(|(key, _)| *key).collect()
        };

        // (a) A fork: OLD endorsed a key of the removed admin's, which signs with any seq.
        let mut fork = unsigned(1000);
        fork.handoff = vec![(
            public(&OLD),
            endorse(&OLD, "alab", &public(&STRANGER)).expect("endorsed"),
        )];
        let fork = sign(&fork, &STRANGER).expect("signed");
        let (taken, pin, _) =
            verify_for_reader_with(&fork, "alab", &public(&OLD), &[], None, &Retired::default())
                .expect("followed before a build knew");
        assert_eq!(pin, Pin::Rotated(public(&STRANGER)));
        let held = Floor::of(&taken);
        let followed = keys(&taken);
        // A build that does not know OLD is retired keeps the fork's key (the control); one
        // that does lets it give way, and the fork's seq 1000 does not floor the vault's 10.
        let (unaware, aware) = verify(&real, public(&STRANGER), &followed, &held);
        assert!(
            unaware
                .as_ref()
                .unwrap_err()
                .0
                .contains("endorsed no handoff"),
            "{unaware:?}"
        );
        assert_eq!(aware, Ok((Pin::Replaced(public(&NEW)), Verdict::New)));
        // Nothing the fork's key signs later is taken either.
        let mut later = unsigned(1001);
        later.handoff = taken.handoff.clone();
        let later = sign(&later, &STRANGER).expect("signed");
        let (unaware, aware) = verify(&later, public(&STRANGER), &followed, &held);
        assert_eq!(unaware, Ok((Pin::Same, Verdict::New)));
        assert!(aware.is_err());
        // Only the lineage tells: the same pin with none recorded keeps the fork's key.
        let (_, aware) = verify(&real, public(&STRANGER), &[], &held);
        assert!(aware.is_err());

        // (b) A version OLD signed with a seq no real version reaches.
        let high = parse(&sign(&unsigned(1 << 62), &OLD).expect("signed")).expect("parsed");
        let (unaware, aware) = verify(&real, public(&OLD), &[], &Floor::of(&high));
        assert!(
            unaware.as_ref().unwrap_err().0.contains("rollback"),
            "{unaware:?}"
        );
        assert_eq!(aware, Ok((Pin::Replaced(public(&NEW)), Verdict::New)));

        // A pin the vault's own chain led to stands, and what it holds floors as ever, also
        // past the keys this build knows.
        let real = parse(&real).expect("parsed");
        let (_, aware) = verify(real.raw(), public(&NEW), &keys(&real), &Floor::of(&real));
        assert_eq!(aware, Ok((Pin::Same, Verdict::Unchanged)));
        let beyond = [10u8; 32];
        let mut rotated = unsigned(11);
        rotated.handoff = real.handoff.clone();
        rotated.handoff.push((
            public(&NEW),
            endorse(&NEW, "alab", &public(&beyond)).expect("endorsed"),
        ));
        let rotated = parse(&sign(&rotated, &beyond).expect("signed")).expect("parsed");
        let mut older = unsigned(9);
        older.handoff = rotated.handoff.clone();
        let older = sign(&older, &beyond).expect("signed");
        let (_, aware) = verify(
            &older,
            public(&beyond),
            &keys(&rotated),
            &Floor::of(&rotated),
        );
        assert!(aware.unwrap_err().0.contains("rollback"));
    }

    #[test]
    fn a_newer_version_carries_on_the_handoff_chain_the_reader_holds() {
        let seeds = seeds(2);
        let key = |index: usize| public(&seeds[index]);
        let full = chain(&seeds);
        let version = |seq: u64, handoff: &[([u8; 32], [u8; 64])], seed: &[u8; 32]| {
            let mut unsigned = unsigned(seq);
            unsigned.handoff = handoff.to_vec();
            sign(&unsigned, seed).expect("signed")
        };
        let read = |bytes: &[u8], pin: [u8; 32], held: &Floor| {
            verify_for_reader(bytes, "alab", &pin, &[], Some(held))
                .map(|(_, pin, verdict)| (pin, verdict))
        };
        // The reader holds the vault at its second key, carrying the one link from the first.
        let held = Floor::of(&parse(&version(5, &full[..1], &seeds[1])).expect("parsed"));

        // A build before chains rotating to the third key carried its own link alone, which
        // would cut off every machine pinned to the first key.
        let cut = version(6, &full[1..], &seeds[2]);
        let error = read(&cut, key(1), &held).unwrap_err();
        assert!(
            error.0.contains("drops or changes handoff 1 of 1"),
            "{error}"
        );
        assert!(error.0.contains(&fingerprint(&key(0))), "{error}");
        // The control: a reader that no longer has its copy judges only seq and digest.
        assert_eq!(
            read(&cut, key(1), &Floor::version(held.seq, held.digest)),
            Ok((Pin::Rotated(key(2)), Verdict::New))
        );
        // A rotation that appends is taken.
        assert_eq!(
            read(&version(6, &full, &seeds[2]), key(1), &held),
            Ok((Pin::Rotated(key(2)), Verdict::New))
        );
        // The same signer carries the chain unchanged.
        let error = read(&version(6, &[], &seeds[1]), key(1), &held).unwrap_err();
        assert!(error.0.contains("handoff 1 of 1"), "{error}");
        assert_eq!(
            read(&version(6, &full[..1], &seeds[1]), key(1), &held),
            Ok((Pin::Same, Verdict::New))
        );
        // A link changed, not dropped: another key's endorsement of the second key, which
        // verifies, in place of the first key's.
        let mut changed = full.clone();
        changed[0] = (
            public(&STRANGER),
            endorse(&STRANGER, "alab", &key(1)).expect("endorsed"),
        );
        let error = read(&version(6, &changed, &seeds[2]), key(1), &held).unwrap_err();
        assert!(error.0.contains("handoff 1 of 1"), "{error}");
        // Links put back before the held chain's first are taken: a reader that holds the cut
        // version takes the one that restores what it dropped.
        let held = Floor::of(&parse(&cut).expect("parsed"));
        assert_eq!(
            read(&version(7, &full, &seeds[2]), key(2), &held),
            Ok((Pin::Same, Verdict::New))
        );
    }

    #[test]
    fn an_admin_puts_back_the_links_a_rotation_by_a_build_before_chains_dropped() {
        let seeds = seeds(3);
        let key = |index: usize| public(&seeds[index]);
        let full = chain(&seeds);
        let held = chain(&seeds[..3]);
        let opened = |handoff: &[([u8; 32], [u8; 64])], seed: &[u8; 32]| {
            let mut unsigned = unsigned(8);
            unsigned.handoff = handoff.to_vec();
            parse(&sign(&unsigned, seed).expect("signed")).expect("parsed")
        };
        // The admin held the vault at the third key; a build before chains rotated it to the
        // fourth carrying only the third key's link.
        let cut = opened(&full[2..], &seeds[3]);
        let restored = cut.chain_carried_on(&key(2), &held).expect("restored");
        assert_eq!(restored, full);
        let mut next = unsigned(9);
        next.handoff = restored;
        let next = sign(&next, &seeds[3]).expect("the restored chain verifies");
        let (_, pin, _) =
            verify_for_reader(&next, "alab", &key(0), &[], None).expect("the first key follows");
        assert_eq!(pin, Pin::Rotated(key(3)));
        // A version that carries the chain on keeps its own; one by the held signer that
        // dropped links gets them back.
        assert_eq!(
            opened(&full, &seeds[3]).chain_carried_on(&key(2), &held),
            Ok(full.clone())
        );
        assert_eq!(
            opened(&[], &seeds[2]).chain_carried_on(&key(2), &held),
            Ok(held.clone())
        );
        // A link changed, not dropped, is not the admin's to undo.
        let mut changed = full[1..].to_vec();
        changed[0] = (
            public(&STRANGER),
            endorse(&STRANGER, "alab", &key(2)).expect("endorsed"),
        );
        let error = opened(&changed, &seeds[3])
            .chain_carried_on(&key(2), &held)
            .unwrap_err();
        assert!(error.0.contains("handoff 1 of 2"), "{error}");
    }

    #[test]
    fn rollback_and_fork_are_refused_and_identical_bytes_are_unchanged() {
        let pin = public(&OLD);
        let seven = sign(&unsigned(7), &OLD).expect("signed");
        let held = Floor::of(&parse(&seven).expect("parsed"));
        let (_, _, verdict) =
            verify_for_reader(&seven, "alab", &pin, &[], Some(&held)).expect("same");
        assert_eq!(verdict, Verdict::Unchanged);

        let six = sign(&unsigned(6), &OLD).expect("signed");
        let error = verify_for_reader(&six, "alab", &pin, &[], Some(&held)).unwrap_err();
        assert!(error.0.contains("rollback"), "{error}");

        let mut fork = unsigned(7);
        fork.time += 1;
        let fork = sign(&fork, &OLD).expect("signed");
        let error = verify_for_reader(&fork, "alab", &pin, &[], Some(&held)).unwrap_err();
        assert!(error.0.contains("fork"), "{error}");

        let eight = sign(&unsigned(next_seq(held.seq).expect("successor")), &OLD).expect("signed");
        let (_, _, verdict) =
            verify_for_reader(&eight, "alab", &pin, &[], Some(&held)).expect("newer");
        assert_eq!(verdict, Verdict::New);

        // The wire format admits the last u64 version: it can be read, but not advanced.
        let penultimate = sign(&unsigned(u64::MAX - 1), &OLD).expect("signed");
        let held = Floor::of(&parse(&penultimate).expect("parsed"));
        let last_seq = next_seq(held.seq).expect("last successor");
        assert_eq!(last_seq, u64::MAX);
        let last = sign(&unsigned(last_seq), &OLD).expect("signed");
        let (last, _, verdict) =
            verify_for_reader(&last, "alab", &pin, &[], Some(&held)).expect("last is newer");
        assert_eq!(last.seq, u64::MAX);
        assert_eq!(verdict, Verdict::New);
        assert!(next_seq(last.seq).unwrap_err().0.contains("exhausted"));
    }

    /// The signed text with one line replaced, re-signed: structure refusals, not signatures.
    fn resigned(edit: impl Fn(&str) -> String) -> Vec<u8> {
        let bytes = sign(&unsigned(2), &OLD).expect("signed");
        let text = std::str::from_utf8(&bytes).expect("text");
        let unsigned_text = &text[..text.find("signature: ").expect("signature line")];
        let edited = edit(unsigned_text);
        let pair = key_pair(&OLD).expect("pair");
        let signature = pair.sign(edited.as_bytes());
        format!(
            "{edited}signature: {}\n",
            b64::encode_padded(signature.as_ref())
        )
        .into_bytes()
    }

    #[test]
    fn the_grammar_has_one_spelling() {
        // The control: an unedited re-signing parses.
        parse(&resigned(str::to_owned)).expect("control");
        let long = format!("{}\n", "A".repeat(65));
        let previous_alone = format!(
            "previous-signer: {}\nlocker: admin\n",
            b64::encode_padded(&[1u8; 32])
        );
        for (what, edit) in [
            ("seq with a leading zero", ("seq: 2\n", "seq: 02\n")),
            ("seq zero", ("seq: 2\n", "seq: 0\n")),
            ("seq with a sign", ("seq: 2\n", "seq: +2\n")),
            ("two spaces", ("seq: 2\n", "seq:  2\n")),
            (
                "time with a leading zero",
                ("time: 1792530000\n", "time: 01792530000\n"),
            ),
            ("uppercase key", ("name: alab\n", "Name: alab\n")),
            ("bad name", ("name: alab\n", "name: Alab\n")),
            ("name with a dot", ("name: alab\n", "name: a.b\n")),
            (
                "headers out of order",
                ("seq: 2\ntime: 1792530000\n", "time: 1792530000\nseq: 2\n"),
            ),
            ("duplicate header", ("seq: 2\n", "seq: 2\nseq: 2\n")),
            ("unknown header", ("seq: 2\n", "seq: 2\ncolour: red\n")),
            ("label with a leading zero", ("locker: 1\n", "locker: 01\n")),
            ("label zero", ("locker: 1\n", "locker: 0\n")),
            ("label word", ("locker: 1\n", "locker: m22\n")),
            ("duplicate label", ("locker: 1\n", "locker: admin\n")),
            ("blank line", ("locker: 1\n", "\nlocker: 1\n")),
            ("armor line too long", ("ZXZlcnlvbmU=\n", long.as_str())),
            (
                "armor line outside the alphabet",
                ("ZXZlcnlvbmU=\n", "ZXZlcnl-bmU=\n"),
            ),
            ("empty armor line", ("ZXZlcnlvbmU=\n", "ZXZlcnlvbmU=\n\n")),
            (
                "bad BEGIN",
                ("locker: 1\n-----BEGIN AGE", "locker: 1\n-----BEGIN  AGE"),
            ),
            (
                "endorsement without previous-signer",
                ("locker: admin\n", "endorsement: AAAA\nlocker: admin\n"),
            ),
            (
                "previous-signer without endorsement",
                ("locker: admin\n", previous_alone.as_str()),
            ),
        ] {
            let bytes = resigned(|text| {
                assert!(text.contains(edit.0), "{what}: fixture lacks {:?}", edit.0);
                text.replacen(edit.0, edit.1, 1)
            });
            assert!(parse(&bytes).is_err(), "{what} was accepted");
        }
        // A missing END line swallows the rest.
        let bytes =
            resigned(|text| text.replacen(&format!("{ARMOR_END}\nlocker: 1\n"), "locker: 1\n", 1));
        assert!(parse(&bytes).is_err());
    }

    #[test]
    fn keys_and_signatures_must_be_canonical_and_sized() {
        let signer = b64::encode_padded(&public(&OLD));
        let short = b64::encode_padded(&[1u8; 31]);
        let bytes = resigned(|text| text.replacen(&signer, &short, 1));
        assert!(parse(&bytes).is_err());
        let bytes = sign(&unsigned(2), &OLD).expect("signed");
        let text = std::str::from_utf8(&bytes).expect("text");
        let unpadded = text.replacen("=\n", "\n", 1);
        assert!(parse(unpadded.as_bytes()).is_err());
    }

    #[test]
    fn sizes_are_bounded() {
        let mut many = unsigned(1);
        many.lockers = (1..=MAX_LOCKERS + 1)
            .map(|index| Locker {
                label: index.to_string(),
                armored: test_armor(b"x"),
            })
            .collect();
        assert!(sign(&many, &OLD).is_err());
        many.lockers.pop();
        let bytes = sign(&many, &OLD).expect("64 lockers");
        assert_eq!(parse(&bytes).expect("parsed").lockers.len(), MAX_LOCKERS);

        let mut big = unsigned(1);
        big.lockers = vec![Locker {
            label: "1".to_owned(),
            armored: test_armor(&vec![0u8; MAX_ENVELOPE]),
        }];
        assert!(sign(&big, &OLD).is_err(), "over 16 MiB");
        assert!(
            parse(&vec![b'a'; MAX_ENVELOPE + 1])
                .unwrap_err()
                .0
                .contains("larger")
        );
    }

    #[test]
    fn sign_refuses_what_parse_would() {
        let mut bad = unsigned(0);
        assert!(sign(&bad, &OLD).is_err(), "seq 0");
        bad.seq = 1;
        bad.name = "../x".to_owned();
        assert!(sign(&bad, &OLD).is_err(), "name");
        let mut bad = unsigned(1);
        bad.lockers[1].label = "admin".to_owned();
        assert!(sign(&bad, &OLD).is_err(), "duplicate label");
        let mut bad = unsigned(1);
        bad.lockers[1].armored.pop();
        assert!(sign(&bad, &OLD).is_err(), "no final LF");
    }

    #[test]
    fn a_fingerprint_is_four_groups_of_hex() {
        let print = fingerprint(&public(&OLD));
        assert_eq!(print.len(), 19);
        let groups: Vec<&str> = print.split('-').collect();
        assert_eq!(groups.len(), 4);
        assert!(groups.iter().all(|g| {
            g.len() == 4
                && g.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }));
        let digest = aterm_digest::Sha256::digest(public(&OLD));
        assert_eq!(&print[..4], format!("{:02x}{:02x}", digest[0], digest[1]));
        assert_ne!(print, fingerprint(&public(&NEW)));
    }

    #[test]
    fn names_and_labels() {
        for good in ["alab", "a", "0", "my-vault", &"a".repeat(63)] {
            assert!(valid_name(good), "{good}");
        }
        for bad in ["", "-a", "A", "a.b", "a/b", "a_b", "..", &"a".repeat(64)] {
            assert!(!valid_name(bad), "{bad}");
        }
        for good in ["admin", "1", "64", "10"] {
            assert!(valid_label(good), "{good}");
        }
        for bad in ["", "0", "01", "Admin", "1a", "-1"] {
            assert!(!valid_label(bad), "{bad}");
        }
    }
}
