// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The signed envelope a vault URL serves (docs/DESIGN-secrets-2026-10-20.md §5).

use crate::{Error, b64};
use ring::signature::{Ed25519KeyPair, KeyPair};

mod digest_cache;

use digest_cache::digest as digest_bytes;

/// The first line.
pub const MAGIC: &str = "alab-vault/1";
/// The largest envelope accepted.
pub const MAX_ENVELOPE: usize = 16 << 20;
/// The most lockers accepted.
pub const MAX_LOCKERS: usize = 64;
/// The most handoff pairs accepted: one per key rotation in the vault's life.
pub const MAX_HANDOFF: usize = 64;
/// The most parent digests accepted on one envelope (one per diverged server head merged).
pub const MAX_PARENTS: usize = 64;

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
    /// The handoff chain: every key `signer` replaced, oldest first, each with its endorsement of the key after it (the last one's, of `signer`).
    pub handoff: Vec<([u8; 32], [u8; 64])>,
    /// SHA-256 digests of the prior envelope head(s) this version descends from (`seq >= 2`).
    pub parents: Vec<[u8; 32]>,
    /// The lockers, in file order.
    pub lockers: Vec<Locker>,
    /// The signature.
    pub signature: [u8; 64],
    raw: Vec<u8>,
    signed_len: usize,
    digest: [u8; 32],
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
    /// SHA-256 digests of the prior envelope head(s) this version descends from (`seq >= 2`).
    pub parents: Vec<[u8; 32]>,
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
    /// The pin gave way to the key this build names ([`Retired::gives_way`]), and the signer is that key or one the chain leads to from it: re-pin to this.
    Replaced([u8; 32]),
}

/// What a build knows of the keys the ALab vault rotated away from (§5).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Retired {
    /// Keys no reader and no server takes an envelope signed by ([`crate::ALAB_RETIRED`]).
    pub(crate) keys: Vec<[u8; 32]>,
    /// The key a pin among `keys` gives way to ([`crate::ALAB_SIGNER`]).
    pub(crate) current: Option<[u8; 32]>,
}

#[cfg(test)]
thread_local! {
    /// What [`Retired::compiled`] answers on this thread instead, in a test of a flow that reaches it (a client's sync, a join) as if this build retired keys.
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

    /// Whether a vault signed by `signer`, reached through `followed` (oldest first), is the ALab
    /// vault this build trusts: `signer` or a key before it is one this build names, and the pin
    /// does not give way ([`Retired::gives_way`]).
    #[cfg(unix)]
    pub(crate) fn names(&self, signer: &[u8; 32], followed: &[[u8; 32]]) -> bool {
        self.lineage(signer, followed) && !self.gives_way(signer, followed)
    }

    /// Whether `signer`, reached through `followed`, is of the ALab vault's line: it or a key
    /// before it is one this build names, whether or not the pin gives way. A server judges its
    /// store by this, since a version a retired key signed still waits there for the one that
    /// replaces it.
    #[cfg(unix)]
    pub(crate) fn lineage(&self, signer: &[u8; 32], followed: &[[u8; 32]]) -> bool {
        self.knows(signer) || followed.iter().any(|key| self.knows(key))
    }

    /// Whether a pin reached through `followed` (the keys before it, oldest first: the handoff chain of the version that moved the pin there) gives way to [`Retired::current`]: the pin is retired, or a retired key in that lineage endorsed a key this build does not know.
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

/// What a reader holds of a vault, for [`verify_for_reader`]: the version it last took, and that version's signer and handoff chain while it still has the version itself, which a newer one must carry on ([`Envelope::check_extends`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Floor {
    /// The seq held.
    pub seq: u64,
    /// The SHA-256 of the envelope held.
    pub digest: [u8; 32],
    /// Its signer and handoff chain, when the reader still has it.
    pub chain: Option<([u8; 32], Vec<Link>)>,
    /// Whether the reader applied it: the same envelope again is [`Verdict::Unchanged`] when it did, [`Verdict::New`] when it holds it unapplied ([`Floor::below`]).
    pub applied: bool,
}

impl Floor {
    /// The floor of a version known only by its seq and digest.
    pub const fn version(seq: u64, digest: [u8; 32]) -> Floor {
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

    /// The floor of an envelope the reader holds whole but has not applied: that envelope reads as new, every older one and another at its seq are refused, and a next one must carry its handoff chain on.
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

/// The next version to publish.
#[cfg(any(unix, test))]
pub(crate) fn next_seq(seq: u64) -> crate::Result<u64> {
    seq.checked_add(1).ok_or_else(|| {
        Error::new("the vault sequence is exhausted; create a new vault instead of wrapping it")
    })
}

/// True when `name` is a vault name: `[a-z0-9][a-z0-9-]{0,62}`.
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

/// Check one armored block's delimiters and lines: the exact `BEGIN` line, at least one line of 1 to 64 characters of the base64 alphabet, the exact `END` line, and a final LF.
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
    const fn new(text: &'a str) -> Self {
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

#[cfg(test)]
thread_local! {
    /// How many envelopes this thread parsed ([`parse`]), for a test that counts a reader's work.
    pub(crate) static TEST_PARSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// How many signatures this thread asked for ([`Envelope::verify_signature`]), kept fact or
    /// not, for the same.
    pub(crate) static TEST_SIGNATURES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Parse an envelope's structure: the magic line, each header once and in order, labels unique (`admin` at most once), each armored block well-delimited, the `signature:` line last followed by exactly one LF, sizes within [`MAX_ENVELOPE`] and [`MAX_LOCKERS`].
pub fn parse(bytes: &[u8]) -> crate::Result<Envelope> {
    #[cfg(test)]
    TEST_PARSES.with(|parses| parses.set(parses.get() + 1));
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
    let mut parents: Vec<[u8; 32]> = Vec::new();
    while let Some((number, _, line)) = lines.peek() {
        if !line.starts_with("parent:") {
            break;
        }
        if seq == 1 {
            return Err(Error::new(format!(
                "envelope line {number}: `parent:` is not allowed at seq 1"
            )));
        }
        if parents.len() == MAX_PARENTS {
            return Err(Error::new(format!(
                "envelope line {number}: more than {MAX_PARENTS} parents"
            )));
        }
        let parent = key_bytes::<32>(lines.header("parent")?, "parent")?;
        if parents.contains(&parent) {
            return Err(Error::new(format!(
                "envelope line {number}: duplicate `parent:` digest"
            )));
        }
        parents.push(parent);
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
        parents,
        lockers,
        signature,
        raw: bytes.to_vec(),
        signed_len,
        digest: digest_bytes(bytes),
    })
}

/// Render and sign: the exact bytes [`parse`] reads back, signed with the Ed25519 key whose seed is `seed`.
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
    if unsigned.seq == 1 && !unsigned.parents.is_empty() {
        return Err(Error::new("envelope: seq 1 cannot name parents"));
    }
    if unsigned.parents.len() > MAX_PARENTS {
        return Err(Error::new(format!(
            "envelope: more than {MAX_PARENTS} parents"
        )));
    }
    for (index, parent) in unsigned.parents.iter().enumerate() {
        if unsigned.parents[..index].contains(parent) {
            return Err(Error::new(format!(
                "envelope: parent {} repeats an earlier parent digest",
                index + 1
            )));
        }
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
    for parent in &unsigned.parents {
        text.push_str(&format!("parent: {}\n", b64::encode_padded(parent)));
    }
    for locker in &unsigned.lockers {
        text.push_str(&format!("locker: {}\n", locker.label));
        text.push_str(&locker.armored);
    }
    if text.len() + SIGNATURE_LINE > MAX_ENVELOPE {
        return Err(Error::new(format!(
            "the vault is too large to publish: sealed, it comes to {} bytes, past the {MAX_ENVELOPE} a version holds; every entry is sealed twice (for the admins and for its audience), so about 5.9 MiB of entries fit",
            text.len() + SIGNATURE_LINE
        )));
    }
    let signature = pair.sign(text.as_bytes());
    text.push_str(&format!(
        "signature: {}\n",
        b64::encode_padded(signature.as_ref())
    ));
    let bytes = text.into_bytes();
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

/// The previous key's endorsement of a new one: an Ed25519 signature by `old_seed` over `alab-vault/1 handoff\n<name>\n<new signer, padded base64>\n`.
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

/// Whether link `index` of `handoff` verifies: link `k`'s key must have signed link
/// `k + 1`'s key, and the last link's key `signer`.
fn link_holds(handoff: &[Link], index: usize, name: &str, signer: &[u8; 32]) -> bool {
    let (previous, endorsement) = &handoff[index];
    let next = handoff.get(index + 1).map_or(signer, |(key, _)| key);
    astream_aead::signature::verify(
        previous,
        handoff_message(name, next).as_bytes(),
        endorsement,
    )
}

/// The first link of `handoff`, from index `from` on, whose endorsement does not verify.
fn broken_link(handoff: &[Link], from: usize, name: &str, signer: &[u8; 32]) -> Option<usize> {
    (from..handoff.len()).find(|&index| !link_holds(handoff, index, name, signer))
}

/// A short fingerprint of a signing key for people to compare: the first 10 bytes of its SHA-256 as `xxxx-xxxx-xxxx-xxxx-xxxx` lowercase hex.
pub fn fingerprint(public: &[u8; 32]) -> String {
    let digest = aterm_digest::Sha256::digest(public);
    let mut out = String::with_capacity(24);
    for (index, pair) in digest[..10].chunks(2).enumerate() {
        if index > 0 {
            out.push('-');
        }
        aterm_codec::hex::push_lower(&mut out, pair);
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
        self.digest
    }

    /// Check the signature over the signed bytes with `signer`.
    pub fn verify_signature(&self) -> crate::Result<()> {
        #[cfg(test)]
        TEST_SIGNATURES.with(|asks| asks.set(asks.get() + 1));
        if crate::ed25519::verifies_cached(
            &self.signer,
            &self.raw[..self.signed_len],
            &self.signature,
        ) {
            Ok(())
        } else {
            Err(Error::new(format!(
                "envelope: the signature does not verify under its signer {}",
                fingerprint(&self.signer)
            )))
        }
    }

    /// Refuse an envelope whose signer is a retired key ([`Retired`]); a retired key as a `previous-signer` is not refused.
    pub(crate) fn check_signer(&self, retired: &Retired) -> crate::Result<()> {
        if retired.keys.contains(&self.signer) {
            return Err(Error::new(format!(
                "envelope: signed by {}, a key this vault rotated away from, which an admin it removed may still hold; refused",
                fingerprint(&self.signer)
            )));
        }
        Ok(())
    }

    /// Check the signer against a pin reached through `followed` (the keys before it, oldest first; empty for a pin set at join).
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

    /// Whether this version's chain names one of `keys` handing off and a link from there on does not verify: no key of the vault's endorsed what it claims, so the version is forged (or damaged), whoever signed it.
    #[cfg(unix)]
    pub(crate) fn forges_handoff_from(&self, keys: &[[u8; 32]]) -> bool {
        self.handoff
            .iter()
            .position(|(previous, _)| keys.contains(previous))
            .is_some_and(|start| {
                broken_link(&self.handoff, start, &self.name, &self.signer).is_some()
            })
    }

    /// The keys of this version's chain its signer was reached from, oldest first: each one from whose link on every endorsement verifies.
    #[cfg(unix)]
    pub(crate) fn reached_from(&self) -> Vec<[u8; 32]> {
        let start = (0..self.handoff.len())
            .rev()
            .find(|&index| !link_holds(&self.handoff, index, &self.name, &self.signer))
            .map_or(0, |broken| broken + 1);
        self.handoff[start..].iter().map(|(key, _)| *key).collect()
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

    /// Refuse a version that does not carry on the handoff chain of the version held, signed by `held_signer` and carrying `held`: every link of `held` must come, in order, right before `held_signer`'s own link (last, when `held_signer` signed this one too).
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

    /// The handoff chain an admin's next version carries after opening this one while it held a version signed by `held_signer` carrying `held`: this one's own when it carries `held` on ([`Envelope::check_extends`]), or, when this one only dropped links from the front of `held` (a rotation by a build before chains, through a server that took it), `held` with this one's links from `held_signer` on, so the next version restores what was dropped.
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

/// Everything a reader checks before it decrypts, in order: the structure, `name` equals `expect_name`, the signer is not retired and the pin, reached through `followed`, holds ([`Envelope::check_pin`]), the signature, and the version against what it holds (`held`): newer is [`Verdict::New`] when it also carries on the held version's handoff chain ([`Envelope::check_extends`]), the same seq with the same digest is [`Verdict::Unchanged`], the same seq with other bytes or a lower seq is an error (a rollback or a forked vault).
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
    let (pin, verdict) =
        judge_for_reader(&envelope, expect_name, pin, followed, held, retired, || {
            envelope.verify_signature()
        })?;
    Ok((envelope, pin, verdict))
}

/// [`verify_for_reader_with`]'s checks after [`parse`], in its order, of an envelope already
/// parsed; `signature` answers [`Envelope::verify_signature`] when that check is reached.
pub(crate) fn judge_for_reader(
    envelope: &Envelope,
    expect_name: &str,
    pin: &[u8; 32],
    followed: &[[u8; 32]],
    held: Option<&Floor>,
    retired: &Retired,
    signature: impl FnOnce() -> crate::Result<()>,
) -> crate::Result<(Pin, Verdict)> {
    if envelope.name != expect_name {
        return Err(Error::new(format!(
            "envelope: names the vault `{}`, not `{expect_name}`",
            envelope.name
        )));
    }
    let pin = envelope.check_pin_with(pin, followed, retired)?;
    signature()?;
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
    Ok((pin, verdict))
}

/// A syntactically valid armored block around `bytes` (not an age file: the envelope never looks inside).
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

/// The handoff chain of a vault named `name` that signed with `seeds` in turn: every key but the last, each with its endorsement of the next.
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
#[path = "envelope_tests.rs"]
mod tests;
