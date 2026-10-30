// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Audience epochs, wraps and encrypted secret entries in the membership log
//! (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.3.6, §3.5.3, F14).
//!
//! - [`Body::Epoch`]: mints a 32-byte symmetric key for one [`Audience`], publishing a SHA-256
//!   commitment so unwrapping verifies the exact key the minter committed to.
//! - [`Body::Wrap`]: wraps an epoch key to one [`Recipient`] with binary age v1 encryption.
//! - [`Body::Put`] and [`Body::Delete`]: seal secret entry names, flags and values under the
//!   audience's epoch key via HKDF-SHA-256 and ChaCha20-Poly1305 so keyless servers learn neither
//!   entry names nor secret bytes.
//! - [`Log::open_entry_set`]: evaluates each `(Audience, name)` as a causal multi-value register
//!   whose concurrent siblings break ties by lowest [`RecordId`], then merges `[everyone]` under
//!   host audiences identically to envelope lockers. A `Put` that carries the register's value
//!   unchanged (an epoch's reseal) loses to a concurrent sibling that changes it, and replaces
//!   only ops writing that value.

use super::{
    Body, Draft, EpochAudience, Kind, Log, MAX_FOUNDERS, MemberId, Reader, Record, RecordId, eval,
};
use crate::age::{self, AgeError, Identity, Recipient};
use crate::text::{self, Audience, Entry};
use crate::{Error, Result, SecretBytes};
use aterm_digest::Sha256;
use ring::{aead, hkdf};
use std::collections::{BTreeMap, BTreeSet};
use zeroize::{Zeroize, Zeroizing};

/// Maximum binary age capsule size carried by [`Body::Wrap`].
pub const MAX_WRAP_CAPSULE: usize = 1024;
/// Maximum ChaCha20-Poly1305 ciphertext size carried by [`Body::Put`].
pub const MAX_PUT_CIPHERTEXT: usize = text::MAX_VALUE + 1024;
/// Maximum ChaCha20-Poly1305 ciphertext size carried by [`Body::Delete`].
pub const MAX_DELETE_CIPHERTEXT: usize = 1024;

const COMMIT_DOMAIN: &[u8] = b"aterm/members/epoch-commit/1\0";
const WRAP_MAGIC: &[u8; 4] = b"ALW1";
const WRAP_PLAINTEXT_LEN: usize = 4 + 32 + 32 + 32 + 8 + 32;
const PUT_MAGIC: &[u8; 4] = b"ALE1";
const DELETE_MAGIC: &[u8; 4] = b"ALD1";
const ENTRY_HKDF_DOMAIN: &[u8] = b"aterm/members/entry/1";
const ENTRY_AAD_MAGIC: &[u8; 5] = b"ALEA1";

const FLAG_IF_ABSENT: u8 = 1;
const FLAG_HAS_ANCHOR: u8 = 2;

/// Active [`EpochAudience::Commons`] epoch selected from a [`Log`] for one active member
/// [`Identity`] (`F18`).
#[derive(Clone, Debug)]
pub struct CommonsEpochView {
    /// Winning causally maximal [`EpochAudience::Commons`] [`Body::Epoch`] record id.
    pub basis: RecordId,
    /// Monotone generation number of `basis`.
    pub generation: u64,
    /// Unwrapped 32-byte Commons symmetric network key for `basis`.
    pub key: Zeroizing<[u8; 32]>,
}

/// Per-audience state opened from a [`Log`] for one [`Identity`], preserving distinct audiences
/// before `[everyone]` / host merging so writers and envelope-mirror builders can diff per
/// [`Audience`].
#[derive(Clone, Debug)]
pub struct OpenedAudienceEntries {
    /// Winning live vault entries grouped by exact [`Audience`] and entry name.
    pub by_audience: BTreeMap<Audience, BTreeMap<String, Entry>>,
    /// Winning `aterm-commons-control-v1` descriptor sealed under [`EpochAudience::Commons`].
    pub commons_control: Option<SecretBytes>,
    /// Winning [`Audience::Hosts`] section per entry name across all host audiences unwrapped by
    /// this [`Identity`] (resolved by causal order and then lowest [`RecordId`]).
    pub winning_host_audience: BTreeMap<String, Audience>,
    /// Epochs that currently seal at least one winning live entry, grouped by [`EpochAudience`].
    pub winning_epochs: BTreeMap<EpochAudience, BTreeSet<RecordId>>,
    /// Causally maximal effective [`Body::Epoch`] record ids per [`EpochAudience`].
    pub maximal_epochs: BTreeMap<EpochAudience, Vec<RecordId>>,
    /// Highest effective `generation` observed per [`EpochAudience`].
    pub max_generation: BTreeMap<EpochAudience, u64>,
    /// Effective [`Body::Wrap`] recipients observed per `epoch` [`RecordId`].
    pub wrapped_recipients: BTreeMap<RecordId, BTreeSet<Recipient>>,
    /// `(Audience, name)` registers holding two or more concurrent sibling writes that write
    /// different things (a reseal, which carries a value unchanged, is none).
    pub conflicts: BTreeSet<(Audience, String)>,
    /// Unwrapped symmetric keys keyed by `epoch` [`RecordId`].
    pub unwrapped_keys: BTreeMap<RecordId, Zeroizing<[u8; 32]>>,
    /// Audiences that have at least one effective [`Body::Epoch`] and whose maximal epochs were all
    /// unwrapped by this [`Identity`].
    pub fully_unwrapped_audiences: BTreeSet<EpochAudience>,
    /// Whether any effective vault [`Audience`]'s maximal [`Body::Epoch`] was not unwrapped by
    /// `identity`.
    pub hidden_epochs: bool,
}

/// Entries opened for one [`Identity`] from a [`Log`], alongside concurrent sibling conflicts and
/// epoch visibility metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenedEntries {
    /// Merged entries for this machine (`[everyone]` overridden by matching [`Audience::Hosts`]).
    pub entries: Vec<Entry>,
    /// `(Audience, name)` pairs whose current register currently holds two or more concurrent
    /// sibling writes (`Put` or `Delete`) that write different things, resolved deterministically
    /// by lowest [`RecordId`].
    pub conflicts: BTreeSet<(Audience, String)>,
    /// Effective [`Body::Epoch`] record ids this [`Identity`] unwrapped.
    pub unwrapped_epochs: BTreeSet<RecordId>,
    /// Whether any effective audience's latest [`Body::Epoch`] was not wrapped to this [`Identity`].
    pub hidden_epochs: bool,
}

/// Domain-separated SHA-256 commitment binding `epoch_key` to `(network, audience, generation)`.
#[must_use]
pub fn epoch_commitment(
    network: &RecordId,
    audience: &EpochAudience,
    generation: u64,
    epoch_key: &[u8; 32],
) -> [u8; 32] {
    let mut buf = Zeroizing::new(Vec::with_capacity(160));
    buf.extend_from_slice(COMMIT_DOMAIN);
    buf.extend_from_slice(network);
    encode_audience_wire(&mut buf, audience);
    buf.extend_from_slice(&generation.to_be_bytes());
    buf.extend_from_slice(epoch_key);
    Sha256::digest(&buf)
}

pub(super) fn check_audience(audience: &EpochAudience) -> Result<()> {
    match audience {
        EpochAudience::Everyone | EpochAudience::Commons => Ok(()),
        EpochAudience::Hosts(hosts) => {
            if hosts.is_empty() || hosts.len() > MAX_FOUNDERS {
                return Err(Error::new(
                    "a membership host audience names 1 to 255 hosts",
                ));
            }
            if !hosts.iter().all(|h| text::valid_machine_name(h))
                || !hosts.windows(2).all(|w| w[0] < w[1])
            {
                return Err(Error::new(
                    "a membership host audience holds sorted, distinct valid machine names",
                ));
            }
            Ok(())
        }
    }
}

pub(super) fn encode_audience_wire(out: &mut Vec<u8>, audience: &EpochAudience) {
    match audience {
        EpochAudience::Everyone => out.push(0),
        EpochAudience::Hosts(hosts) => {
            out.push(1);
            match u8::try_from(hosts.len()) {
                Ok(count) if count > 0 => {
                    out.push(count);
                    for host in hosts {
                        super::encode_text(out, host);
                    }
                }
                _ => out.push(0),
            }
        }
        EpochAudience::Commons => out.push(2),
    }
}

pub(super) fn decode_audience_wire(reader: &mut Reader<'_>) -> Result<EpochAudience> {
    match reader.byte()? {
        0 => Ok(EpochAudience::Everyone),
        1 => {
            let count = usize::from(reader.byte()?);
            if count == 0 {
                return Err(Error::new(
                    "a membership host audience names at least one host",
                ));
            }
            let mut hosts = Vec::with_capacity(count);
            for _ in 0..count {
                hosts.push(super::decode_text(reader)?);
            }
            let audience = EpochAudience::Hosts(hosts);
            check_audience(&audience)?;
            Ok(audience)
        }
        2 => Ok(EpochAudience::Commons),
        _ => Err(Error::new("membership record has an unknown audience tag")),
    }
}

fn audience_wire(audience: &EpochAudience) -> Vec<u8> {
    let mut out = Vec::new();
    encode_audience_wire(&mut out, audience);
    out
}

fn entry_aad(
    network: &RecordId,
    actor: &MemberId,
    sequence: u64,
    kind: Kind,
    audience: &EpochAudience,
    epoch: &RecordId,
) -> Vec<u8> {
    let mut aad = Vec::with_capacity(144);
    aad.extend_from_slice(ENTRY_AAD_MAGIC);
    aad.extend_from_slice(network);
    aad.extend_from_slice(actor);
    aad.extend_from_slice(&sequence.to_be_bytes());
    aad.push(kind.code());
    encode_audience_wire(&mut aad, audience);
    aad.extend_from_slice(epoch);
    aad
}

fn derive_entry_key(
    epoch_key: &[u8; 32],
    salt: &[u8; 32],
    kind: Kind,
    network: &RecordId,
    epoch: &RecordId,
    audience: &EpochAudience,
) -> aead::LessSafeKey {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(epoch_key);
    let kind_byte = [kind.code()];
    let aud_wire = audience_wire(audience);
    let info = [
        ENTRY_HKDF_DOMAIN,
        &kind_byte,
        network.as_slice(),
        epoch.as_slice(),
        aud_wire.as_slice(),
    ];
    let okm = prk
        .expand(&info, &aead::CHACHA20_POLY1305)
        .expect("ChaCha20-Poly1305 key size is within HKDF-SHA-256 bound");
    aead::LessSafeKey::new(aead::UnboundKey::from(okm))
}

fn check_entry_fields(entry: &Entry) -> Result<()> {
    check_audience(&EpochAudience::from(&entry.audience))?;
    if !text::valid_entry_name(&entry.name) {
        return Err(Error::new("membership entry has an invalid entry name"));
    }
    if entry.value.len() > text::MAX_VALUE {
        return Err(Error::new("membership entry value exceeds MAX_VALUE"));
    }
    if let Some(anchor) = &entry.anchor
        && (!entry.if_absent || !text::valid_entry_name(anchor) || anchor == &entry.name)
    {
        return Err(Error::new("membership entry has an invalid anchor"));
    }
    if crate::commons::reserved_name(&entry.name)
        && (entry.name != crate::commons::ENTRY
            || !matches!(entry.audience, Audience::Hosts(_))
            || entry.if_absent
            || entry.anchor.is_some())
    {
        return Err(Error::new(
            "the Commons control entry requires an explicit host audience without if-absent or an anchor",
        ));
    }
    Ok(())
}

fn encode_put_plaintext(entry: &Entry) -> Result<Zeroizing<Vec<u8>>> {
    check_entry_fields(entry)?;
    let name_bytes = entry.name.as_bytes();
    let name_len =
        u16::try_from(name_bytes.len()).map_err(|_| Error::new("entry name is too long"))?;
    let anchor_bytes = entry.anchor.as_deref().map(str::as_bytes);
    let anchor_len = match anchor_bytes {
        Some(a) => Some(u16::try_from(a.len()).map_err(|_| Error::new("anchor is too long"))?),
        None => None,
    };
    let val_bytes = entry.value.as_slice();
    let val_len =
        u32::try_from(val_bytes.len()).map_err(|_| Error::new("entry value is too long"))?;

    let mut plain = Zeroizing::new(Vec::with_capacity(
        4 + 1
            + 2
            + name_bytes.len()
            + 2
            + anchor_bytes.map_or(0, <[u8]>::len)
            + 4
            + val_bytes.len(),
    ));
    plain.extend_from_slice(PUT_MAGIC);
    let flags = u8::from(entry.if_absent) * FLAG_IF_ABSENT
        + u8::from(anchor_len.is_some()) * FLAG_HAS_ANCHOR;
    plain.push(flags);
    plain.extend_from_slice(&name_len.to_be_bytes());
    plain.extend_from_slice(name_bytes);
    if let (Some(len), Some(bytes)) = (anchor_len, anchor_bytes) {
        plain.extend_from_slice(&len.to_be_bytes());
        plain.extend_from_slice(bytes);
    }
    plain.extend_from_slice(&val_len.to_be_bytes());
    plain.extend_from_slice(val_bytes);
    Ok(plain)
}

fn decode_put_plaintext(audience: &Audience, plain: &[u8]) -> Result<Entry> {
    let mut reader = Reader::new(plain);
    if reader.take(4)? != PUT_MAGIC {
        return Err(Error::new("corrupt membership Put plaintext header"));
    }
    let flags = reader.byte()?;
    if flags & !(FLAG_IF_ABSENT | FLAG_HAS_ANCHOR) != 0 {
        return Err(Error::new("membership Put plaintext has unknown flags"));
    }
    let if_absent = flags & FLAG_IF_ABSENT != 0;
    let has_anchor = flags & FLAG_HAS_ANCHOR != 0;
    let name_len = reader.len(2)?;
    let name = std::str::from_utf8(reader.take(name_len)?)
        .map_err(|_| Error::new("membership Put name is not UTF-8"))?
        .to_owned();
    let anchor = if has_anchor {
        let anchor_len = reader.len(2)?;
        Some(
            std::str::from_utf8(reader.take(anchor_len)?)
                .map_err(|_| Error::new("membership Put anchor is not UTF-8"))?
                .to_owned(),
        )
    } else {
        None
    };
    let val_len = reader.len(4)?;
    if val_len > text::MAX_VALUE {
        return Err(Error::new("membership Put value exceeds MAX_VALUE"));
    }
    let value = SecretBytes::from(reader.take(val_len)?);
    reader.finish()?;
    let entry = Entry {
        name,
        value,
        audience: audience.clone(),
        if_absent,
        anchor,
    };
    check_entry_fields(&entry)?;
    Ok(entry)
}

fn encode_commons_put_plaintext(control_descriptor: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    if !control_descriptor.starts_with(b"ACD1") || control_descriptor.len() > text::MAX_VALUE {
        return Err(Error::new(
            "Commons control Put requires a valid ACD1 descriptor within MAX_VALUE",
        ));
    }
    let name_bytes = crate::commons::ENTRY.as_bytes();
    let name_len =
        u16::try_from(name_bytes.len()).map_err(|_| Error::new("entry name is too long"))?;
    let val_len = u32::try_from(control_descriptor.len())
        .map_err(|_| Error::new("entry value is too long"))?;
    let mut plain = Zeroizing::new(Vec::with_capacity(
        4 + 1 + 2 + name_bytes.len() + 4 + control_descriptor.len(),
    ));
    plain.extend_from_slice(PUT_MAGIC);
    plain.push(0);
    plain.extend_from_slice(&name_len.to_be_bytes());
    plain.extend_from_slice(name_bytes);
    plain.extend_from_slice(&val_len.to_be_bytes());
    plain.extend_from_slice(control_descriptor);
    Ok(plain)
}

fn decode_commons_put_plaintext(plain: &[u8]) -> Result<SecretBytes> {
    let mut reader = Reader::new(plain);
    if reader.take(4)? != PUT_MAGIC {
        return Err(Error::new("corrupt membership Put plaintext header"));
    }
    let flags = reader.byte()?;
    if flags != 0 {
        return Err(Error::new(
            "Commons control Put forbids if-absent and anchor flags",
        ));
    }
    let name_len = reader.len(2)?;
    let name = std::str::from_utf8(reader.take(name_len)?)
        .map_err(|_| Error::new("membership Put name is not UTF-8"))?;
    if name != crate::commons::ENTRY {
        return Err(Error::new(
            "Commons audience Put carries only the Commons control entry",
        ));
    }
    let val_len = reader.len(4)?;
    if val_len > text::MAX_VALUE {
        return Err(Error::new("membership Put value exceeds MAX_VALUE"));
    }
    let bytes = reader.take(val_len)?;
    if !bytes.starts_with(b"ACD1") {
        return Err(Error::new(
            "Commons control Put requires an ACD1 descriptor",
        ));
    }
    let value = SecretBytes::from(bytes);
    reader.finish()?;
    Ok(value)
}

fn encode_delete_plaintext(name: &str) -> Result<Zeroizing<Vec<u8>>> {
    if !text::valid_entry_name(name) {
        return Err(Error::new("membership Delete has an invalid entry name"));
    }
    let name_bytes = name.as_bytes();
    let name_len =
        u16::try_from(name_bytes.len()).map_err(|_| Error::new("entry name is too long"))?;
    let mut plain = Zeroizing::new(Vec::with_capacity(4 + 2 + name_bytes.len()));
    plain.extend_from_slice(DELETE_MAGIC);
    plain.extend_from_slice(&name_len.to_be_bytes());
    plain.extend_from_slice(name_bytes);
    Ok(plain)
}

fn decode_delete_plaintext(plain: &[u8]) -> Result<String> {
    let mut reader = Reader::new(plain);
    if reader.take(4)? != DELETE_MAGIC {
        return Err(Error::new("corrupt membership Delete plaintext header"));
    }
    let name_len = reader.len(2)?;
    let name = std::str::from_utf8(reader.take(name_len)?)
        .map_err(|_| Error::new("membership Delete name is not UTF-8"))?
        .to_owned();
    reader.finish()?;
    if !text::valid_entry_name(&name) {
        return Err(Error::new("membership Delete has an invalid entry name"));
    }
    Ok(name)
}

struct AeadContext<'a> {
    kind: Kind,
    network: &'a RecordId,
    actor: &'a MemberId,
    sequence: u64,
    audience: &'a EpochAudience,
    epoch: &'a RecordId,
}

fn seal_aead(
    epoch_key: &[u8; 32],
    ctx: &AeadContext<'_>,
    mut plain: Zeroizing<Vec<u8>>,
) -> Result<([u8; 32], Vec<u8>)> {
    let mut salt = [0u8; 32];
    aterm_uds::rand::fill(&mut salt)?;
    let key = derive_entry_key(
        epoch_key,
        &salt,
        ctx.kind,
        ctx.network,
        ctx.epoch,
        ctx.audience,
    );
    let aad = entry_aad(
        ctx.network,
        ctx.actor,
        ctx.sequence,
        ctx.kind,
        ctx.audience,
        ctx.epoch,
    );
    let nonce = aead::Nonce::assume_unique_for_key([0u8; 12]);
    let mut buf = Vec::with_capacity(plain.len() + 16);
    buf.extend_from_slice(&plain);
    plain.zeroize();
    key.seal_in_place_append_tag(nonce, aead::Aad::from(&aad), &mut buf)
        .map_err(|_| {
            buf.zeroize();
            Error::new("sealing membership log entry failed")
        })?;
    Ok((salt, buf))
}

fn open_aead(
    epoch_key: &[u8; 32],
    record: &Record,
    audience: &EpochAudience,
    epoch: &RecordId,
    salt: &[u8; 32],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    let key = derive_entry_key(
        epoch_key,
        salt,
        record.kind(),
        record.network(),
        epoch,
        audience,
    );
    let aad = entry_aad(
        record.network(),
        record.actor(),
        record.sequence(),
        record.kind(),
        audience,
        epoch,
    );
    let nonce = aead::Nonce::assume_unique_for_key([0u8; 12]);
    let mut buf = Zeroizing::new(ciphertext.to_vec());
    let plain_len = key
        .open_in_place(nonce, aead::Aad::from(&aad), &mut buf)
        .map_err(|_| Error::new("decrypting membership log entry failed"))?
        .len();
    buf.truncate(plain_len);
    Ok(buf)
}

fn seal_wrap(
    network: &RecordId,
    epoch: &RecordId,
    generation: u64,
    recipient: Recipient,
    epoch_key: &[u8; 32],
) -> Result<Vec<u8>> {
    let mut payload = Zeroizing::new(Vec::with_capacity(WRAP_PLAINTEXT_LEN));
    payload.extend_from_slice(WRAP_MAGIC);
    payload.extend_from_slice(network);
    payload.extend_from_slice(epoch);
    payload.extend_from_slice(&recipient.0);
    payload.extend_from_slice(&generation.to_be_bytes());
    payload.extend_from_slice(epoch_key);
    age::encrypt(std::slice::from_ref(&recipient), &payload)
        .map_err(|e| Error::new(format!("wrapping membership epoch key: {e}")))
}

fn open_wrap(
    identity: &Identity,
    network: &RecordId,
    epoch: &RecordId,
    audience: &EpochAudience,
    generation: u64,
    commitment: &[u8; 32],
    capsule: &[u8],
) -> Result<Option<Zeroizing<[u8; 32]>>> {
    let plain = match age::decrypt(std::slice::from_ref(identity), capsule) {
        Ok(plain) => plain,
        Err(AgeError::NoMatch) => return Ok(None),
        Err(e) => return Err(Error::new(format!("unwrapping membership epoch key: {e}"))),
    };
    let bytes = plain.as_slice();
    if bytes.len() != WRAP_PLAINTEXT_LEN
        || &bytes[..4] != WRAP_MAGIC
        || &bytes[4..36] != network
        || &bytes[36..68] != epoch
        || bytes[68..100] != identity.recipient().0
    {
        return Err(Error::new("membership epoch wrap header mismatch"));
    }
    let wrapped_gen = u64::from_be_bytes(bytes[100..108].try_into().unwrap());
    if wrapped_gen != generation {
        return Err(Error::new("membership epoch wrap generation mismatch"));
    }
    let mut key = Zeroizing::new([0u8; 32]);
    key.copy_from_slice(&bytes[108..140]);
    let actual_commit = epoch_commitment(network, audience, generation, &key);
    if !aterm_digest::ct_eq(&actual_commit, commitment) {
        return Err(Error::new(
            "membership epoch wrap key does not match epoch commitment",
        ));
    }
    Ok(Some(key))
}

#[derive(Clone)]
enum DecodedOp {
    Put(Entry),
    Delete,
}

struct CandidateOp {
    index: usize,
    record_id: RecordId,
    epoch: RecordId,
    op: DecodedOp,
}

/// Whether two register ops write the same thing: two `Delete`s, or two `Put`s of one value with
/// the same flags.
fn same_op(a: &DecodedOp, b: &DecodedOp) -> bool {
    match (a, b) {
        (DecodedOp::Delete, DecodedOp::Delete) => true,
        (DecodedOp::Put(a), DecodedOp::Put(b)) => {
            a.if_absent == b.if_absent
                && a.anchor == b.anchor
                && aterm_digest::ct_eq(a.value.as_slice(), b.value.as_slice())
        }
        _ => false,
    }
}

/// Drop each winning entry whose anchor (`if-absent=<anchor>`) no view of `by_audience` holds as a
/// live `if-absent` entry, as a vault text refuses one: registers merge one name at a time, so a
/// split where one side removed an anchored set while the other added to it, or a member's lone
/// `Put` naming an anchor that is not there, leaves such an entry. Kept, every Mac's materializer
/// would refuse it (and with it every read), and an admin's overlay could not commit it. Dropped,
/// a set stays or goes whole. A view is `[everyone]`, or `[everyone]` under each host's winning
/// host-list entries, as [`Log::open_entry_set`] merges them.
fn drop_dangling_anchors(
    by_audience: &mut BTreeMap<Audience, BTreeMap<String, Entry>>,
    winning_host_audience: &mut BTreeMap<String, Audience>,
) {
    loop {
        let hosts: BTreeSet<String> = by_audience
            .keys()
            .filter_map(|audience| match audience {
                Audience::Hosts(hosts) => Some(hosts.iter().cloned()),
                Audience::Everyone => None,
            })
            .flatten()
            .collect();
        let mut dangling: BTreeSet<(Audience, String)> = BTreeSet::new();
        for host in std::iter::once(None).chain(hosts.iter().map(Some)) {
            let mut view: BTreeMap<&str, (&Audience, &Entry)> = BTreeMap::new();
            if let Some((audience, map)) = by_audience.get_key_value(&Audience::Everyone) {
                for (name, entry) in map {
                    view.insert(name, (audience, entry));
                }
            }
            if let Some(host) = host {
                for (audience, map) in by_audience.iter() {
                    if !matches!(audience, Audience::Hosts(_)) || !audience.includes(host) {
                        continue;
                    }
                    for (name, entry) in map {
                        if winning_host_audience.get(name) == Some(audience) {
                            view.insert(name, (audience, entry));
                        }
                    }
                }
            }
            for (name, (audience, entry)) in &view {
                if !text::anchor_received(entry, |anchor| view.get(anchor).map(|(_, e)| *e)) {
                    dangling.insert(((*audience).clone(), (*name).to_owned()));
                }
            }
        }
        if dangling.is_empty() {
            return;
        }
        for (audience, name) in dangling {
            if let Some(map) = by_audience.get_mut(&audience) {
                map.remove(&name);
            }
            if winning_host_audience.get(&name) == Some(&audience) {
                winning_host_audience.remove(&name);
            }
        }
    }
}

/// One register's ops (in causal order, as `CandidateOp`s) and which of them are carries: a `Put`
/// of the value the register already held in that `Put`'s own causal past, as an epoch's reseal
/// writes when a Mac departs (the writer re-puts every entry under a fresh epoch, changing no
/// value). A carry stands for the op whose value it carries (`reps`); every other op for itself.
struct Register<'a> {
    cands: &'a [CandidateOp],
    carries: Vec<bool>,
    reps: Vec<usize>,
}

impl<'a> Register<'a> {
    fn classify(cands: &'a [CandidateOp], effective: &eval::Effective) -> Self {
        let mut register = Register {
            cands,
            carries: vec![false; cands.len()],
            reps: (0..cands.len()).collect(),
        };
        for k in 0..cands.len() {
            if !matches!(cands[k].op, DecodedOp::Put(_)) {
                continue;
            }
            // What the writer held: the register over `k`'s causal past (`before` is the whole
            // past, so scanning from the newest, an op is maximal unless it lies before one
            // already found maximal).
            let mut maximal: Vec<usize> = Vec::new();
            for b in (0..k).rev() {
                if effective.before(cands[b].index, cands[k].index)
                    && !maximal
                        .iter()
                        .any(|&m| effective.before(cands[b].index, cands[m].index))
                {
                    maximal.push(b);
                }
            }
            let held = register.live(maximal, effective);
            if let Some(w) = register.winner(&held)
                && same_op(&cands[w].op, &cands[k].op)
            {
                register.carries[k] = true;
                register.reps[k] = register.reps[w];
            }
        }
        register
    }

    /// `maximal` less each carry whose value another of them, writing something else, already
    /// replaced: a reseal on one side of a split loses to an edit or `Delete` on the other.
    fn live(&self, maximal: Vec<usize>, effective: &eval::Effective) -> Vec<usize> {
        let cands = self.cands;
        maximal
            .iter()
            .copied()
            .filter(|&c| {
                !self.carries[c]
                    || !maximal.iter().any(|&s| {
                        s != c
                            && !same_op(&cands[s].op, &cands[c].op)
                            && effective.before(cands[self.reps[c]].index, cands[s].index)
                    })
            })
            .collect()
    }

    /// The op every Mac writes among `siblings`: the lowest id of what each stands for.
    fn winner(&self, siblings: &[usize]) -> Option<usize> {
        siblings
            .iter()
            .copied()
            .min_by_key(|&i| (self.cands[self.reps[i]].record_id, self.cands[i].record_id))
    }

    /// The register's siblings. An op replaces what lies in its causal past, except that a carry
    /// replaces only ops writing the value it carries: a reseal after two concurrent edits keeps
    /// both as candidates instead of settling on one.
    fn siblings(&self, effective: &eval::Effective) -> Vec<usize> {
        let cands = self.cands;
        let replaced = |a: usize, b: usize| {
            a != b
                && effective.before(cands[a].index, cands[b].index)
                && (!self.carries[b] || same_op(&cands[a].op, &cands[b].op))
        };
        let maximal = (0..cands.len())
            .filter(|&a| !(0..cands.len()).any(|b| replaced(a, b)))
            .collect();
        self.live(maximal, effective)
    }
}

struct CommonsControlOp {
    index: usize,
    record_id: RecordId,
    epoch: RecordId,
    value: SecretBytes,
}

impl Log {
    /// Whether this log holds at least one effective [`Body::Epoch`] record.
    #[must_use]
    pub fn has_epochs(&self) -> bool {
        let effective = eval::effective_records(&self.order);
        self.order
            .iter()
            .enumerate()
            .skip(1)
            .any(|(i, r)| effective.counts(i) && r.kind() == Kind::Epoch)
    }

    /// Whether this log holds at least one effective [`EpochAudience::Commons`] [`Body::Epoch`]
    /// record (`F18`).
    #[must_use]
    pub fn has_commons_epochs(&self) -> bool {
        let effective = eval::effective_records(&self.order);
        self.order.iter().enumerate().skip(1).any(|(i, r)| {
            effective.counts(i)
                && matches!(
                    r.body(),
                    Body::Epoch {
                        audience: EpochAudience::Commons,
                        ..
                    }
                )
        })
    }

    fn next_sequence_for(&self, actor: &MemberId) -> u64 {
        self.order
            .iter()
            .filter(|r| r.kind() != Kind::Genesis && r.actor() == actor)
            .map(Record::sequence)
            .max()
            .map_or(1, |seq| seq + 1)
    }

    fn verify_held_epoch_key(
        &self,
        epoch: &RecordId,
        epoch_key: &[u8; 32],
    ) -> Result<(EpochAudience, u64)> {
        let Some(record) = self.get(epoch) else {
            return Err(Error::new("membership log does not hold that epoch"));
        };
        let Body::Epoch {
            audience,
            generation,
            commitment,
        } = record.body()
        else {
            return Err(Error::new("referenced record is not an Epoch"));
        };
        let expected = epoch_commitment(self.network(), audience, *generation, epoch_key);
        if !aterm_digest::ct_eq(&expected, commitment) {
            return Err(Error::new("epoch key does not match the Epoch commitment"));
        }
        Ok((audience.clone(), *generation))
    }

    /// Draft a fresh [`Body::Epoch`] for `audience` at `generation` (`>= 1`), returning the
    /// [`Draft`] and its newly generated 32-byte symmetric epoch key.
    ///
    /// # Errors
    /// `audience` is malformed, `generation == 0`, or OS randomness fails.
    pub fn draft_epoch(
        &self,
        actor: &MemberId,
        authored_ms: u64,
        audience: impl Into<EpochAudience>,
        generation: u64,
    ) -> Result<(Draft, Zeroizing<[u8; 32]>)> {
        let audience = audience.into();
        check_audience(&audience)?;
        if generation == 0 {
            return Err(Error::new("epoch generation starts at 1"));
        }
        let mut epoch_key = Zeroizing::new([0u8; 32]);
        aterm_uds::rand::fill(epoch_key.as_mut())?;
        let commitment = epoch_commitment(self.network(), &audience, generation, &epoch_key);
        let draft = self.draft(
            actor,
            authored_ms,
            Body::Epoch {
                audience,
                generation,
                commitment,
            },
        );
        Ok((draft, epoch_key))
    }

    /// Draft a [`Body::Wrap`] encrypting `epoch`'s key to `recipient`.
    ///
    /// # Errors
    /// `epoch` is not held in this log, `epoch_key` does not match `epoch`'s commitment, or age
    /// encryption fails.
    pub fn draft_wrap(
        &self,
        actor: &MemberId,
        authored_ms: u64,
        epoch: RecordId,
        epoch_key: &[u8; 32],
        recipient: Recipient,
    ) -> Result<Draft> {
        let (_, generation) = self.verify_held_epoch_key(&epoch, epoch_key)?;
        let capsule = seal_wrap(self.network(), &epoch, generation, recipient, epoch_key)?;
        Ok(self.draft(
            actor,
            authored_ms,
            Body::Wrap {
                epoch,
                recipient,
                capsule,
            },
        ))
    }

    /// Draft a [`Body::Put`] sealing `entry` under `epoch` and `epoch_key`.
    ///
    /// # Errors
    /// `entry` is invalid, `epoch` is not held or is for a different [`Audience`], `epoch_key`
    /// fails commitment verification, or encryption fails.
    pub fn draft_put(
        &self,
        actor: &MemberId,
        authored_ms: u64,
        epoch: RecordId,
        epoch_key: &[u8; 32],
        entry: &Entry,
    ) -> Result<Draft> {
        let (ep_aud, _) = self.verify_held_epoch_key(&epoch, epoch_key)?;
        let target_aud = EpochAudience::from(&entry.audience);
        if ep_aud != target_aud {
            return Err(Error::new("Put audience does not match Epoch audience"));
        }
        let plain = encode_put_plaintext(entry)?;
        let sequence = self.next_sequence_for(actor);
        let (salt, ciphertext) = seal_aead(
            epoch_key,
            &AeadContext {
                kind: Kind::Put,
                network: self.network(),
                actor,
                sequence,
                audience: &target_aud,
                epoch: &epoch,
            },
            plain,
        )?;
        Ok(self.draft(
            actor,
            authored_ms,
            Body::Put {
                audience: target_aud,
                epoch,
                salt,
                ciphertext,
            },
        ))
    }

    /// Draft a [`Body::Put`] sealing the `aterm-commons-control-v1` descriptor (`ACD1`) under an
    /// [`EpochAudience::Commons`] epoch (`F18`).
    ///
    /// # Errors
    /// `control_descriptor` is not a valid `ACD1` descriptor within [`text::MAX_VALUE`], `epoch` is
    /// not a held [`EpochAudience::Commons`] epoch, `epoch_key` fails commitment verification, or
    /// encryption fails.
    pub fn draft_commons_put(
        &self,
        actor: &MemberId,
        authored_ms: u64,
        epoch: RecordId,
        epoch_key: &[u8; 32],
        control_descriptor: &[u8],
    ) -> Result<Draft> {
        let (ep_aud, _) = self.verify_held_epoch_key(&epoch, epoch_key)?;
        if ep_aud != EpochAudience::Commons {
            return Err(Error::new(
                "Commons control Put requires an EpochAudience::Commons epoch",
            ));
        }
        let plain = encode_commons_put_plaintext(control_descriptor)?;
        let sequence = self.next_sequence_for(actor);
        let (salt, ciphertext) = seal_aead(
            epoch_key,
            &AeadContext {
                kind: Kind::Put,
                network: self.network(),
                actor,
                sequence,
                audience: &EpochAudience::Commons,
                epoch: &epoch,
            },
            plain,
        )?;
        Ok(self.draft(
            actor,
            authored_ms,
            Body::Put {
                audience: EpochAudience::Commons,
                epoch,
                salt,
                ciphertext,
            },
        ))
    }

    /// Draft a [`Body::Delete`] tombstoning `name` in `epoch`'s audience under `epoch_key`.
    ///
    /// # Errors
    /// `name` is invalid, `epoch` is not held or is [`EpochAudience::Commons`], `epoch_key` fails
    /// commitment verification, or encryption fails.
    pub fn draft_delete(
        &self,
        actor: &MemberId,
        authored_ms: u64,
        epoch: RecordId,
        epoch_key: &[u8; 32],
        name: &str,
    ) -> Result<Draft> {
        let (audience, _) = self.verify_held_epoch_key(&epoch, epoch_key)?;
        if audience == EpochAudience::Commons {
            return Err(Error::new(
                "Commons control entries cannot be tombstoned with Delete",
            ));
        }
        let plain = encode_delete_plaintext(name)?;
        let sequence = self.next_sequence_for(actor);
        let (salt, ciphertext) = seal_aead(
            epoch_key,
            &AeadContext {
                kind: Kind::Delete,
                network: self.network(),
                actor,
                sequence,
                audience: &audience,
                epoch: &epoch,
            },
            plain,
        )?;
        Ok(self.draft(
            actor,
            authored_ms,
            Body::Delete {
                audience,
                epoch,
                salt,
                ciphertext,
            },
        ))
    }

    /// Age recipients belonging to Macs that were removed and not restored in this log.
    #[must_use]
    pub fn removed_recipients(&self) -> BTreeSet<Recipient> {
        let membership = self.membership();
        let mut out = membership.removed_recipients().clone();
        for m in membership.members().values() {
            let r = m.binding.recipient();
            if !out.contains(&r) && self.recipient_is_removed(&r) {
                out.insert(r);
            }
        }
        for f in membership.unproved() {
            let r = f.recipient;
            if !out.contains(&r) && self.recipient_is_removed(&r) {
                out.insert(r);
            }
        }
        out
    }

    /// Whether `recipient` belongs to a Mac that was removed and not restored in this log.
    #[must_use]
    pub fn recipient_is_removed(&self, recipient: &Recipient) -> bool {
        let membership = self.membership();
        if membership.removed_recipients().contains(recipient) {
            return true;
        }
        let mut any_for_recipient = false;
        for m in membership.members().values() {
            if m.binding.recipient() == *recipient {
                any_for_recipient = true;
                let node_removed = m.node_key.is_some_and(|nk| {
                    nk != [0; 32] && matches!(membership.node(&nk), eval::NodeStanding::Removed(_))
                });
                if !node_removed {
                    return false;
                }
            }
        }
        for f in membership.unproved() {
            if f.recipient == *recipient {
                any_for_recipient = true;
                let node_removed = f.node_key.is_some_and(|nk| {
                    nk != [0; 32] && matches!(membership.node(&nk), eval::NodeStanding::Removed(_))
                });
                if !node_removed {
                    return false;
                }
            }
        }
        any_for_recipient
    }

    /// Whether an effective [`Body::Wrap`] in this log is addressed to `recipient`.
    #[must_use]
    pub fn wraps_to(&self, recipient: &Recipient) -> bool {
        let effective = eval::effective_records(&self.order);
        self.order.iter().enumerate().skip(1).any(|(i, record)| {
            effective.counts(i)
                && matches!(record.body(), Body::Wrap { recipient: to, .. } if to == recipient)
        })
    }

    /// Whether every causally maximal effective [`Body::Epoch`] for `target_audience` in
    /// this log (and at least one such epoch exists) has an effective [`Body::Wrap`] addressed to
    /// `recipient`.
    #[must_use]
    pub fn wraps_maximal_audience_to(
        &self,
        target_audience: &EpochAudience,
        recipient: &Recipient,
    ) -> bool {
        let effective = eval::effective_records(&self.order);
        let mut epochs: Vec<(usize, RecordId)> = Vec::new();
        let mut wrapped_epochs: BTreeSet<RecordId> = BTreeSet::new();
        for (i, record) in self.order.iter().enumerate().skip(1) {
            if !effective.counts(i) {
                continue;
            }
            match record.body() {
                Body::Epoch { audience, .. } if audience == target_audience => {
                    epochs.push((i, *record.id()));
                }
                Body::Wrap {
                    epoch,
                    recipient: to,
                    ..
                } if to == recipient => {
                    wrapped_epochs.insert(*epoch);
                }
                _ => {}
            }
        }
        let maximal: Vec<RecordId> = epochs
            .iter()
            .filter(|&&(a, _)| {
                !epochs
                    .iter()
                    .any(|&(b, _)| a != b && effective.before(a, b))
            })
            .map(|&(_, id)| id)
            .collect();
        !maximal.is_empty() && maximal.iter().all(|id| wrapped_epochs.contains(id))
    }

    /// Whether this log removed the Mac whose age recipient is `recipient`: no member holds that
    /// recipient now, and a removed one did.
    #[must_use]
    pub fn removes(&self, recipient: &Recipient) -> bool {
        self.recipient_is_removed(recipient)
    }

    /// Unwrap every effective [`Body::Epoch`] key wrapped to `identity` in this log, keyed by
    /// `epoch` [`RecordId`]. Returns an empty map if `identity` has been removed.
    ///
    /// # Errors
    /// A matching [`Body::Wrap`] addressed to `identity` fails decryption or fails its epoch
    /// commitment check.
    pub fn unwrap_epochs(
        &self,
        identity: &Identity,
    ) -> Result<BTreeMap<RecordId, Zeroizing<[u8; 32]>>> {
        let my_recipient = identity.recipient();
        if self.recipient_is_removed(&my_recipient) {
            return Ok(BTreeMap::new());
        }
        let effective = eval::effective_records(&self.order);
        let mut epochs: BTreeMap<RecordId, (&EpochAudience, u64, &[u8; 32])> = BTreeMap::new();
        for (i, record) in self.order.iter().enumerate().skip(1) {
            if !effective.counts(i) {
                continue;
            }
            if let Body::Epoch {
                audience,
                generation,
                commitment,
            } = record.body()
            {
                epochs.insert(*record.id(), (audience, *generation, commitment));
            }
        }
        let mut unwrapped: BTreeMap<RecordId, Zeroizing<[u8; 32]>> = BTreeMap::new();
        for (i, record) in self.order.iter().enumerate().skip(1) {
            if !effective.counts(i) {
                continue;
            }
            let Body::Wrap {
                epoch,
                recipient,
                capsule,
            } = record.body()
            else {
                continue;
            };
            if *recipient != my_recipient || unwrapped.contains_key(epoch) {
                continue;
            }
            let Some(&(audience, generation, commitment)) = epochs.get(epoch) else {
                continue;
            };
            if let Some(key) = open_wrap(
                identity,
                self.network(),
                epoch,
                audience,
                generation,
                commitment,
                capsule,
            )? {
                unwrapped.insert(*epoch, key);
            }
        }
        Ok(unwrapped)
    }

    /// Select and unwrap the active [`EpochAudience::Commons`] epoch for `identity` (`F18`).
    ///
    /// Returns `Ok(None)` when `identity` is not an active bound member in [`Log::membership`],
    /// when no effective [`EpochAudience::Commons`] epoch exists, or when the winning active
    /// `Commons` epoch has not yet been wrapped to `identity`.
    ///
    /// # Errors
    /// Any addressed [`Body::Wrap`] fails authentication or epoch commitment verification.
    pub fn commons_epoch_view(&self, identity: &Identity) -> Result<Option<CommonsEpochView>> {
        let my_recipient = identity.recipient();
        let membership = self.membership();
        if self.recipient_is_removed(&my_recipient)
            || self.audience_left_out_recipient(&EpochAudience::Commons, &my_recipient)
            || self.audience_left_out_recipient(&EpochAudience::Everyone, &my_recipient)
            || !membership.members().iter().any(|(actor, m)| {
                m.binding.recipient() == my_recipient
                    && !m.node_key.is_some_and(|nk| {
                        nk != [0; 32]
                            && matches!(membership.node(&nk), eval::NodeStanding::Removed(_))
                    })
                    && !self.audience_left_out(&EpochAudience::Commons, actor)
                    && !self.audience_left_out(&EpochAudience::Everyone, actor)
            })
        {
            return Ok(None);
        }
        let effective = eval::effective_records(&self.order);
        let mut list: Vec<(usize, RecordId, u64, [u8; 32])> = Vec::new();
        for (i, record) in self.order.iter().enumerate().skip(1) {
            if !effective.counts(i) {
                continue;
            }
            if let Body::Epoch {
                audience: EpochAudience::Commons,
                generation,
                commitment,
            } = record.body()
            {
                list.push((i, *record.id(), *generation, *commitment));
            }
        }
        if list.is_empty() {
            return Ok(None);
        }
        let maximal: Vec<(u64, RecordId, [u8; 32])> = list
            .iter()
            .filter(|&&(a, _, _, _)| {
                !list
                    .iter()
                    .any(|&(b, _, _, _)| a != b && effective.before(a, b))
            })
            .map(|&(_, id, generation, commitment)| (generation, id, commitment))
            .collect();
        let Some(&(generation, basis, commitment)) = maximal
            .iter()
            .min_by_key(|&&(g, id, _)| (std::cmp::Reverse(g), id))
        else {
            return Ok(None);
        };
        let mut unwrapped_basis: Option<Zeroizing<[u8; 32]>> = None;
        for (i, record) in self.order.iter().enumerate().skip(1) {
            if !effective.counts(i) {
                continue;
            }
            if let Body::Wrap {
                epoch,
                recipient,
                capsule,
            } = record.body()
                && *epoch == basis
                && *recipient == my_recipient
                && let Some(key) = open_wrap(
                    identity,
                    self.network(),
                    &basis,
                    &EpochAudience::Commons,
                    generation,
                    &commitment,
                    capsule,
                )?
            {
                unwrapped_basis = Some(key);
                break;
            }
        }
        let Some(key) = unwrapped_basis else {
            return Ok(None);
        };
        Ok(Some(CommonsEpochView {
            basis,
            generation,
            key,
        }))
    }

    /// Unwrap a single effective `EpochAudience::Commons` epoch key by record identifier (`F18`).
    ///
    /// # Errors
    /// Capsule decompression fails cleanly on corruption.
    pub fn unwrap_commons_epoch(
        &self,
        basis: &RecordId,
        identity: &Identity,
    ) -> Result<Option<Zeroizing<[u8; 32]>>> {
        let my_recipient = identity.recipient();
        let membership = self.membership();
        if self.recipient_is_removed(&my_recipient)
            || self.audience_left_out_recipient(&EpochAudience::Commons, &my_recipient)
            || self.audience_left_out_recipient(&EpochAudience::Everyone, &my_recipient)
            || !membership.members().iter().any(|(actor, m)| {
                m.binding.recipient() == my_recipient
                    && !m.node_key.is_some_and(|nk| {
                        nk != [0; 32]
                            && matches!(membership.node(&nk), eval::NodeStanding::Removed(_))
                    })
                    && !self.audience_left_out(&EpochAudience::Commons, actor)
                    && !self.audience_left_out(&EpochAudience::Everyone, actor)
            })
        {
            return Ok(None);
        }
        let effective = eval::effective_records(&self.order);
        let Some((generation, commitment)) =
            self.order.iter().enumerate().skip(1).find_map(|(i, rec)| {
                if !effective.counts(i) || rec.id() != basis {
                    return None;
                }
                match rec.body() {
                    Body::Epoch {
                        audience: EpochAudience::Commons,
                        generation,
                        commitment,
                    } => Some((*generation, *commitment)),
                    _ => None,
                }
            })
        else {
            return Ok(None);
        };
        for (i, record) in self.order.iter().enumerate().skip(1) {
            if !effective.counts(i) {
                continue;
            }
            if let Body::Wrap {
                epoch,
                recipient,
                capsule,
            } = record.body()
                && epoch == basis
                && *recipient == my_recipient
                && let Some(key) = open_wrap(
                    identity,
                    self.network(),
                    basis,
                    &EpochAudience::Commons,
                    generation,
                    &commitment,
                    capsule,
                )?
            {
                return Ok(Some(key));
            }
        }
        Ok(None)
    }

    /// Fast structural check (without X25519 or AEAD decryption) for whether `my_recipient` has
    /// any [`EpochAudience::Commons`] or active [`EpochAudience::Everyone`] wrap gap to close for
    /// `active_recipients` or (when `allow_coalesce` is `true`) concurrent maximal epochs or
    /// maximal epochs still wrapping removed recipients to rotate (`F18`).
    #[must_use]
    pub fn needs_commons_reconcile(
        &self,
        my_recipient: &Recipient,
        active_recipients: &BTreeSet<Recipient>,
        allow_coalesce: bool,
    ) -> bool {
        self.reconcile_due(my_recipient, active_recipients, allow_coalesce, true)
    }

    /// The coverage rule of §3.3.7 for host lists: the header hosts of each host list whose every
    /// causally newest epoch is wrapped to `my_recipient` (so this Mac opens them) and which has two
    /// or more newest epochs, or a newest epoch still wrapped to a Mac this log removed. A removal
    /// by a member outside the list cannot rotate it, so one of the list's own members mints its
    /// next epoch on a vault pass (F16's review).
    #[must_use]
    pub fn hosts_rotation_due(&self, my_recipient: &Recipient) -> Vec<Vec<String>> {
        let effective = eval::effective_records(&self.order);
        let mut epochs: BTreeMap<Vec<String>, Vec<(usize, RecordId)>> = BTreeMap::new();
        let mut wrapped: BTreeMap<RecordId, BTreeSet<Recipient>> = BTreeMap::new();
        for (i, record) in self.order.iter().enumerate().skip(1) {
            if !effective.counts(i) {
                continue;
            }
            match record.body() {
                Body::Epoch {
                    audience: EpochAudience::Hosts(hosts),
                    ..
                } => epochs
                    .entry(hosts.clone())
                    .or_default()
                    .push((i, *record.id())),
                Body::Wrap {
                    epoch, recipient, ..
                } => {
                    wrapped.entry(*epoch).or_default().insert(*recipient);
                }
                _ => {}
            }
        }
        let removed = self.removed_recipients();
        epochs
            .into_iter()
            .filter(|(_, list)| {
                let maximal: Vec<&RecordId> = list
                    .iter()
                    .filter(|&&(a, _)| !list.iter().any(|&(b, _)| a != b && effective.before(a, b)))
                    .map(|(_, id)| id)
                    .collect();
                let opens = maximal.iter().all(|id| {
                    wrapped
                        .get(*id)
                        .is_some_and(|set| set.contains(my_recipient))
                });
                let stale = maximal.len() > 1
                    || maximal.iter().any(|id| {
                        wrapped
                            .get(*id)
                            .is_some_and(|set| !set.is_disjoint(&removed))
                    });
                !maximal.is_empty() && opens && stale
            })
            .map(|(hosts, _)| hosts)
            .collect()
    }

    /// The coverage rule of §3.3.7 alone (keychain "Epochs"): whether the member `my_recipient`,
    /// among `active_recipients`, holds an [`EpochAudience::Commons`] or `[everyone]` epoch whose
    /// audience has two or more causally newest epochs (concurrent removals on two sides of a
    /// split) or a newest epoch still wrapped to a Mac this log removed (a quorum `Remove` voided
    /// the remover's rotation, or a removal reached it before its new epoch), so the next writer
    /// mints one higher epoch that covers them. No wrap gap counts here: those close by the
    /// causal rule (`Log::wrap_gaps`).
    #[must_use]
    pub fn rotation_due(
        &self,
        my_recipient: &Recipient,
        active_recipients: &BTreeSet<Recipient>,
    ) -> bool {
        self.reconcile_due(my_recipient, active_recipients, true, false)
    }

    fn reconcile_due(
        &self,
        my_recipient: &Recipient,
        active_recipients: &BTreeSet<Recipient>,
        allow_coalesce: bool,
        gaps: bool,
    ) -> bool {
        if active_recipients.is_empty() || !active_recipients.contains(my_recipient) {
            return false;
        }
        let effective = eval::effective_records(&self.order);
        let mut commons_epochs: Vec<(usize, RecordId)> = Vec::new();
        let mut everyone_epochs: Vec<(usize, RecordId)> = Vec::new();
        let mut everyone_puts: Vec<(usize, RecordId)> = Vec::new();
        let mut wrapped: BTreeMap<RecordId, BTreeSet<Recipient>> = BTreeMap::new();
        for (i, record) in self.order.iter().enumerate().skip(1) {
            if !effective.counts(i) {
                continue;
            }
            match record.body() {
                Body::Epoch {
                    audience: EpochAudience::Commons,
                    ..
                } => {
                    commons_epochs.push((i, *record.id()));
                }
                Body::Epoch {
                    audience: EpochAudience::Everyone,
                    ..
                } => {
                    everyone_epochs.push((i, *record.id()));
                }
                Body::Put {
                    audience: EpochAudience::Everyone,
                    epoch,
                    ..
                } => {
                    everyone_puts.push((i, *epoch));
                }
                Body::Wrap {
                    epoch, recipient, ..
                } => {
                    wrapped.entry(*epoch).or_default().insert(*recipient);
                }
                _ => {}
            }
        }
        let maximal_commons: Vec<RecordId> = commons_epochs
            .iter()
            .filter(|&&(a, _)| {
                !commons_epochs
                    .iter()
                    .any(|&(b, _)| a != b && effective.before(a, b))
            })
            .map(|&(_, id)| id)
            .collect();
        let maximal_everyone: Vec<RecordId> = everyone_epochs
            .iter()
            .filter(|&&(a, _)| {
                !everyone_epochs
                    .iter()
                    .any(|&(b, _)| a != b && effective.before(a, b))
            })
            .map(|&(_, id)| id)
            .collect();
        if allow_coalesce {
            let removed = self.removed_recipients();
            let holds_any_commons = commons_epochs.iter().any(|(_, id)| {
                wrapped
                    .get(id)
                    .is_some_and(|set| set.contains(my_recipient))
            });
            if holds_any_commons
                && (maximal_commons.len() > 1
                    || (!removed.is_empty()
                        && maximal_commons.iter().any(|id| {
                            wrapped
                                .get(id)
                                .is_some_and(|set| !set.is_disjoint(&removed))
                        })))
            {
                return true;
            }
            let holds_any_everyone = everyone_epochs.iter().any(|(_, id)| {
                wrapped
                    .get(id)
                    .is_some_and(|set| set.contains(my_recipient))
            });
            let can_open_all_max_everyone = !maximal_everyone.is_empty()
                && holds_any_everyone
                && maximal_everyone.iter().all(|id| {
                    wrapped.get(id).is_none_or(|set| {
                        set.contains(my_recipient) || set.is_disjoint(active_recipients)
                    })
                });
            if can_open_all_max_everyone
                && (maximal_everyone.len() > 1
                    || (!removed.is_empty()
                        && maximal_everyone.iter().any(|id| {
                            wrapped
                                .get(id)
                                .is_some_and(|set| !set.is_disjoint(&removed))
                        })))
            {
                return true;
            }
        }
        if !gaps {
            return false;
        }
        let membership = self.membership();
        let left_out_of = |maximal: &[RecordId]| -> BTreeSet<Recipient> {
            membership
                .members()
                .values()
                .filter_map(|seat| {
                    let entry_at = *self.held.get(&seat.entry)?;
                    let recipient = seat.binding.recipient();
                    let left_out = maximal.iter().any(|epoch| {
                        let is_wrapped = wrapped
                            .get(epoch)
                            .is_some_and(|set| set.contains(&recipient));
                        let knew = self
                            .held
                            .get(epoch)
                            .is_some_and(|&at| entry_at == at || effective.before(entry_at, at));
                        !is_wrapped && knew
                    });
                    left_out.then_some(recipient)
                })
                .collect()
        };
        let left_out_commons = left_out_of(&maximal_commons);
        let left_out_everyone = left_out_of(&maximal_everyone);
        if left_out_everyone.contains(my_recipient)
            || self.audience_left_out_recipient(&EpochAudience::Everyone, my_recipient)
        {
            return false;
        }
        let commons_active: BTreeSet<Recipient> = active_recipients
            .iter()
            .copied()
            .filter(|r| {
                !left_out_commons.contains(r)
                    && !left_out_everyone.contains(r)
                    && !self.audience_left_out_recipient(&EpochAudience::Commons, r)
                    && !self.audience_left_out_recipient(&EpochAudience::Everyone, r)
            })
            .collect();
        let everyone_active: BTreeSet<Recipient> = active_recipients
            .iter()
            .copied()
            .filter(|r| {
                !left_out_everyone.contains(r)
                    && !self.audience_left_out_recipient(&EpochAudience::Everyone, r)
            })
            .collect();
        if commons_epochs.iter().any(|(_, ep_id)| {
            wrapped
                .get(ep_id)
                .is_some_and(|set| set.contains(my_recipient) && !commons_active.is_subset(set))
        }) {
            return true;
        }
        for &(_, ep_id) in &everyone_epochs {
            let is_maximal = maximal_everyone.contains(&ep_id);
            let has_unretired_put = !is_maximal
                && everyone_puts.iter().any(|&(p_idx, p_ep)| {
                    p_ep == ep_id
                        && !everyone_epochs
                            .iter()
                            .any(|&(b, _)| effective.before(p_idx, b))
                });
            if (is_maximal || has_unretired_put)
                && wrapped.get(&ep_id).is_some_and(|set| {
                    set.contains(my_recipient) && !everyone_active.is_subset(set)
                })
            {
                return true;
            }
        }
        false
    }

    /// Decrypt only the winning [`EpochAudience::Commons`] control descriptor for `identity`,
    /// skipping [`EpochAudience::Everyone`] and [`EpochAudience::Hosts`] X25519 unwraps (`F18`).
    ///
    /// # Errors
    /// Any addressed `Commons` [`Body::Wrap`] or [`Body::Put`] fails authentication or validation.
    pub fn open_commons_control(&self, identity: &Identity) -> Result<Option<SecretBytes>> {
        let my_recipient = identity.recipient();
        let membership = self.membership();
        if self.recipient_is_removed(&my_recipient)
            || self.audience_left_out_recipient(&EpochAudience::Commons, &my_recipient)
            || self.audience_left_out_recipient(&EpochAudience::Everyone, &my_recipient)
        {
            return Ok(None);
        }
        let has_member_seat = membership
            .members()
            .values()
            .any(|m| m.binding.recipient() == my_recipient);
        let has_unproved_seat = membership
            .unproved()
            .iter()
            .any(|f| f.recipient == my_recipient);
        let permitted = if has_member_seat {
            membership.members().iter().any(|(actor, m)| {
                m.binding.recipient() == my_recipient
                    && !m.node_key.is_some_and(|nk| {
                        nk != [0; 32]
                            && matches!(membership.node(&nk), eval::NodeStanding::Removed(_))
                    })
                    && !self.audience_left_out(&EpochAudience::Commons, actor)
                    && !self.audience_left_out(&EpochAudience::Everyone, actor)
            })
        } else if has_unproved_seat {
            membership.unproved().iter().any(|f| {
                f.recipient == my_recipient
                    && !f.node_key.is_some_and(|nk| {
                        nk != [0; 32]
                            && matches!(membership.node(&nk), eval::NodeStanding::Removed(_))
                    })
                    && self.wraps_maximal_audience_to(&EpochAudience::Commons, &my_recipient)
            })
        } else {
            self.wraps_maximal_audience_to(&EpochAudience::Commons, &my_recipient)
        };
        if !permitted {
            return Ok(None);
        }
        let effective = eval::effective_records(&self.order);
        let mut commons_epochs: BTreeMap<RecordId, (u64, &[u8; 32])> = BTreeMap::new();
        for (i, record) in self.order.iter().enumerate().skip(1) {
            if !effective.counts(i) {
                continue;
            }
            if let Body::Epoch {
                audience: EpochAudience::Commons,
                generation,
                commitment,
            } = record.body()
            {
                commons_epochs.insert(*record.id(), (*generation, commitment));
            }
        }
        if commons_epochs.is_empty() {
            return Ok(None);
        }
        let mut unwrapped: BTreeMap<RecordId, Zeroizing<[u8; 32]>> = BTreeMap::new();
        for (i, record) in self.order.iter().enumerate().skip(1) {
            if !effective.counts(i) {
                continue;
            }
            let Body::Wrap {
                epoch,
                recipient,
                capsule,
            } = record.body()
            else {
                continue;
            };
            if *recipient != my_recipient || unwrapped.contains_key(epoch) {
                continue;
            }
            let Some(&(generation, commitment)) = commons_epochs.get(epoch) else {
                continue;
            };
            if let Some(key) = open_wrap(
                identity,
                self.network(),
                epoch,
                &EpochAudience::Commons,
                generation,
                commitment,
                capsule,
            )? {
                unwrapped.insert(*epoch, key);
            }
        }
        if unwrapped.is_empty() {
            return Ok(None);
        }
        let mut candidates: Vec<CommonsControlOp> = Vec::new();
        for (i, record) in self.order.iter().enumerate().skip(1) {
            if !effective.counts(i) {
                continue;
            }
            if let Body::Put {
                audience: audience @ EpochAudience::Commons,
                epoch,
                salt,
                ciphertext,
            } = record.body()
            {
                let Some(epoch_key) = unwrapped.get(epoch) else {
                    continue;
                };
                let plain = open_aead(epoch_key, record, audience, epoch, salt, ciphertext)?;
                let value = decode_commons_put_plaintext(&plain)?;
                candidates.push(CommonsControlOp {
                    index: i,
                    record_id: *record.id(),
                    epoch: *epoch,
                    value,
                });
            }
        }
        let siblings: Vec<&CommonsControlOp> = candidates
            .iter()
            .filter(|a| {
                !candidates
                    .iter()
                    .any(|b| a.index != b.index && effective.before(a.index, b.index))
            })
            .collect();
        Ok(siblings
            .into_iter()
            .min_by_key(|op| op.record_id)
            .map(|winner| winner.value.clone()))
    }

    /// Evaluate per-audience registers, epoch generations, wrap sets, and decrypted keys for
    /// `identity` (`§3.5.3`).
    ///
    /// Returns `Ok(None)` when `identity` has been removed from this log.
    ///
    /// # Errors
    /// Any addressed [`Body::Wrap`], [`Body::Put`] or [`Body::Delete`] fails authentication or
    /// validation.
    pub fn open_audience_entries(
        &self,
        identity: &Identity,
    ) -> Result<Option<OpenedAudienceEntries>> {
        let my_recipient = identity.recipient();
        if self.recipient_is_removed(&my_recipient) {
            return Ok(None);
        }
        let unwrapped = self.unwrap_epochs(identity)?;
        let effective = eval::effective_records(&self.order);

        let mut epochs_by_aud: BTreeMap<EpochAudience, Vec<(usize, RecordId)>> = BTreeMap::new();
        let mut max_generation: BTreeMap<EpochAudience, u64> = BTreeMap::new();
        let mut wrapped_recipients: BTreeMap<RecordId, BTreeSet<Recipient>> = BTreeMap::new();

        for (i, record) in self.order.iter().enumerate().skip(1) {
            if !effective.counts(i) {
                continue;
            }
            match record.body() {
                Body::Epoch {
                    audience,
                    generation,
                    ..
                } => {
                    epochs_by_aud
                        .entry(audience.clone())
                        .or_default()
                        .push((i, *record.id()));
                    let entry = max_generation.entry(audience.clone()).or_insert(0);
                    *entry = (*entry).max(*generation);
                }
                Body::Wrap {
                    epoch, recipient, ..
                } => {
                    wrapped_recipients
                        .entry(*epoch)
                        .or_default()
                        .insert(*recipient);
                }
                _ => {}
            }
        }

        let mut maximal_epochs: BTreeMap<EpochAudience, Vec<RecordId>> = BTreeMap::new();
        let mut fully_unwrapped_audiences = BTreeSet::new();
        let mut excluded_audiences = BTreeSet::new();
        let mut hidden_epochs = false;
        let left_out_everyone =
            self.audience_left_out_recipient(&EpochAudience::Everyone, &my_recipient);
        for (aud, list) in &epochs_by_aud {
            let max_ids: Vec<RecordId> = list
                .iter()
                .filter(|&&(a, _)| !list.iter().any(|&(b, _)| a != b && effective.before(a, b)))
                .map(|&(_, id)| id)
                .collect();
            let is_excluded =
                left_out_everyone || self.audience_left_out_recipient(aud, &my_recipient);
            if !is_excluded && max_ids.iter().all(|id| unwrapped.contains_key(id)) {
                fully_unwrapped_audiences.insert(aud.clone());
            } else if aud != &EpochAudience::Commons {
                hidden_epochs = true;
            }
            if is_excluded {
                excluded_audiences.insert(aud.clone());
            }
            maximal_epochs.insert(aud.clone(), max_ids);
        }

        let mut registers: BTreeMap<(Audience, String), Vec<CandidateOp>> = BTreeMap::new();
        let mut commons_candidates: Vec<CommonsControlOp> = Vec::new();
        for (i, record) in self.order.iter().enumerate().skip(1) {
            if !effective.counts(i) {
                continue;
            }
            match record.body() {
                Body::Put {
                    audience,
                    epoch,
                    salt,
                    ciphertext,
                } => {
                    if excluded_audiences.contains(audience) {
                        continue;
                    }
                    let Some(epoch_key) = unwrapped.get(epoch) else {
                        continue;
                    };
                    let plain = open_aead(epoch_key, record, audience, epoch, salt, ciphertext)?;
                    if let Some(text_aud) = audience.as_text_audience() {
                        let entry = decode_put_plaintext(&text_aud, &plain)?;
                        registers
                            .entry((text_aud, entry.name.clone()))
                            .or_default()
                            .push(CandidateOp {
                                index: i,
                                record_id: *record.id(),
                                epoch: *epoch,
                                op: DecodedOp::Put(entry),
                            });
                    } else {
                        let value = decode_commons_put_plaintext(&plain)?;
                        commons_candidates.push(CommonsControlOp {
                            index: i,
                            record_id: *record.id(),
                            epoch: *epoch,
                            value,
                        });
                    }
                }
                Body::Delete {
                    audience,
                    epoch,
                    salt,
                    ciphertext,
                } => {
                    let Some(text_aud) = audience.as_text_audience() else {
                        return Err(Error::new(
                            "Commons control entries cannot be tombstoned with Delete",
                        ));
                    };
                    if excluded_audiences.contains(audience) {
                        continue;
                    }
                    let Some(epoch_key) = unwrapped.get(epoch) else {
                        continue;
                    };
                    let plain = open_aead(epoch_key, record, audience, epoch, salt, ciphertext)?;
                    let name = decode_delete_plaintext(&plain)?;
                    registers
                        .entry((text_aud, name))
                        .or_default()
                        .push(CandidateOp {
                            index: i,
                            record_id: *record.id(),
                            epoch: *epoch,
                            op: DecodedOp::Delete,
                        });
                }
                _ => {}
            }
        }

        let mut conflicts = BTreeSet::new();
        let mut by_audience: BTreeMap<Audience, BTreeMap<String, Entry>> = BTreeMap::new();
        let mut winning_epochs: BTreeMap<EpochAudience, BTreeSet<RecordId>> = BTreeMap::new();
        let mut host_winners: BTreeMap<String, Vec<(Audience, usize, RecordId)>> = BTreeMap::new();

        for ((audience, name), candidates) in registers {
            // A carry changes nothing, so it loses to a concurrent sibling that changes the
            // register, a `Delete` included: a reseal on one side of a split never undoes an edit
            // made on the other. Siblings that write the same thing are no conflict.
            let register = Register::classify(&candidates, &effective);
            let siblings = register.siblings(&effective);
            let Some(winner) = register.winner(&siblings).map(|i| &candidates[i]) else {
                continue;
            };
            if siblings
                .iter()
                .any(|&i| !same_op(&candidates[i].op, &winner.op))
            {
                conflicts.insert((audience.clone(), name.clone()));
            }
            if let DecodedOp::Put(entry) = &winner.op {
                if matches!(audience, Audience::Hosts(_)) {
                    host_winners.entry(name.clone()).or_default().push((
                        audience.clone(),
                        winner.index,
                        winner.record_id,
                    ));
                }
                by_audience
                    .entry(audience.clone())
                    .or_default()
                    .insert(name, entry.clone());
                winning_epochs
                    .entry(EpochAudience::from(audience))
                    .or_default()
                    .insert(winner.epoch);
            }
        }

        let mut commons_control = None;
        if !commons_candidates.is_empty() {
            let siblings: Vec<&CommonsControlOp> = commons_candidates
                .iter()
                .filter(|a| {
                    !commons_candidates
                        .iter()
                        .any(|b| a.index != b.index && effective.before(a.index, b.index))
                })
                .collect();
            if let Some(winner) = siblings.into_iter().min_by_key(|c| c.record_id) {
                commons_control = Some(winner.value.clone());
                winning_epochs
                    .entry(EpochAudience::Commons)
                    .or_default()
                    .insert(winner.epoch);
            }
        }

        let mut winning_host_audience: BTreeMap<String, Audience> = BTreeMap::new();
        for (name, candidates) in host_winners {
            let maximal: Vec<&(Audience, usize, RecordId)> = candidates
                .iter()
                .filter(|a| {
                    !candidates
                        .iter()
                        .any(|b| a.1 != b.1 && effective.before(a.1, b.1))
                })
                .collect();
            if maximal.len() > 1 {
                for (aud, _, _) in &maximal {
                    conflicts.insert((aud.clone(), name.clone()));
                }
            }
            if let Some((aud, _, _)) = maximal.into_iter().min_by_key(|c| c.2) {
                winning_host_audience.insert(name, aud.clone());
            }
        }
        drop_dangling_anchors(&mut by_audience, &mut winning_host_audience);

        Ok(Some(OpenedAudienceEntries {
            by_audience,
            commons_control,
            winning_host_audience,
            winning_epochs,
            maximal_epochs,
            max_generation,
            wrapped_recipients,
            conflicts,
            unwrapped_keys: unwrapped,
            fully_unwrapped_audiences,
            hidden_epochs,
        }))
    }

    /// Evaluate and decrypt the secret entries visible to `identity` from this log (`§3.5.3`).
    ///
    /// Returns `Ok(None)` when `identity` has been removed or holds no unwrapped epoch in the log.
    ///
    /// # Errors
    /// Any addressed [`Body::Wrap`], [`Body::Put`] or [`Body::Delete`] fails authentication or
    /// validation.
    pub fn open_entry_set(&self, identity: &Identity) -> Result<Option<OpenedEntries>> {
        let Some(opened) = self.open_audience_entries(identity)? else {
            return Ok(None);
        };
        if opened.unwrapped_keys.is_empty()
            || self.audience_left_out_recipient(&EpochAudience::Everyone, &identity.recipient())
        {
            return Ok(None);
        }
        let mut everyone: BTreeMap<String, Entry> = BTreeMap::new();
        let mut hosts: BTreeMap<String, Entry> = BTreeMap::new();
        for (audience, map) in opened.by_audience {
            for (name, entry) in map {
                match &audience {
                    Audience::Everyone => {
                        everyone.insert(name, entry);
                    }
                    Audience::Hosts(_) => {
                        if opened.winning_host_audience.get(&name) == Some(&audience) {
                            hosts.insert(name, entry);
                        }
                    }
                }
            }
        }
        everyone.extend(hosts);
        if let Some(value) = opened.commons_control {
            everyone.insert(
                crate::commons::ENTRY.to_owned(),
                Entry {
                    name: crate::commons::ENTRY.to_owned(),
                    value,
                    audience: Audience::Everyone,
                    if_absent: false,
                    anchor: None,
                },
            );
        }
        let unwrapped_epochs = opened.unwrapped_keys.into_keys().collect();
        Ok(Some(OpenedEntries {
            entries: everyone.into_values().collect(),
            conflicts: opened.conflicts,
            unwrapped_epochs,
            hidden_epochs: opened.hidden_epochs,
        }))
    }

    /// Convenience wrapper over [`Log::open_entry_set`] returning the merged [`Entry`] list.
    ///
    /// # Errors
    /// Propagates errors from [`Log::open_entry_set`].
    pub fn open_entries(&self, identity: &Identity) -> Result<Option<Vec<Entry>>> {
        Ok(self.open_entry_set(identity)?.map(|opened| opened.entries))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commons::binding::Request;
    use crate::membership_log::{Entrant, Evidence};

    const NETWORK: [u8; 16] = [0x31; 16];
    const AUTHORITY: [u8; 32] = [0x42; 32];

    fn make_entrant(tag: u8, label: &str) -> ([u8; 32], [u8; 16], Identity, MemberId, Entrant) {
        let writer = [tag; 32];
        let machine = [tag.wrapping_add(0x20); 16];
        let incarnation = [tag; 16];
        let identity = Identity::from_secret_bytes([tag.wrapping_add(0x40); 32]);
        let request = Request::create(NETWORK, AUTHORITY, machine, incarnation, &identity, &writer)
            .expect("request");
        let member = request.binding().actor();
        let entrant = Entrant {
            request,
            node_key: [tag.wrapping_add(0x80); 32],
            name: format!("mac-{tag}"),
            label: label.to_owned(),
        };
        (writer, incarnation, identity, member, entrant)
    }

    #[test]
    fn epoch_wrap_put_delete_round_trip_and_refuse_tampering() {
        let (w1, inc1, id1, m1, e1) = make_entrant(1, "m1");
        let (w2, inc2, id2, m2, e2) = make_entrant(2, "m2");
        let (_, _, id_out, _, _) = make_entrant(3, "m3");

        let genesis = Record::found(&[7; 32], vec![e1], 1_000).expect("genesis");
        let mut log = Log::new(genesis).expect("log");
        assert!(!log.has_epochs());

        let admit_m2 = log
            .draft(
                &m1,
                1_001,
                Body::Admit {
                    entrant: e2,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&w1, inc1)
            .expect("admit m2");
        log.insert(admit_m2).expect("insert admit");

        let (ep_draft, ep_key) = log
            .draft_epoch(&m1, 1_002, Audience::Everyone, 1)
            .expect("draft epoch");
        let ep_rec = Record::decode(ep_draft.sign(&w1, inc1).expect("sign epoch").bytes())
            .expect("decode epoch");
        let ep_id = *ep_rec.id();
        log.insert(ep_rec).expect("insert epoch");
        assert!(log.has_epochs());

        for id in [&id1, &id2] {
            let wrap = Record::decode(
                log.draft_wrap(&m1, 1_003, ep_id, &ep_key, id.recipient())
                    .expect("draft wrap")
                    .sign(&w1, inc1)
                    .expect("sign wrap")
                    .bytes(),
            )
            .expect("decode wrap");
            log.insert(wrap).expect("insert wrap");
        }

        // Wrong epoch key is refused by commitment verification on draft_put.
        let bad_entry = Entry {
            name: "api-key".to_owned(),
            value: SecretBytes::from(b"secret-1".as_slice()),
            audience: Audience::Everyone,
            if_absent: false,
            anchor: None,
        };
        assert!(
            log.draft_put(&m1, 1_004, ep_id, &[0x99; 32], &bad_entry)
                .is_err()
        );

        let put1 = Record::decode(
            log.draft_put(&m1, 1_004, ep_id, &ep_key, &bad_entry)
                .expect("draft put")
                .sign(&w1, inc1)
                .expect("sign put")
                .bytes(),
        )
        .expect("decode put");
        log.insert(put1).expect("insert put");

        // Unwrapped outsider reads `None`, while both wrapped members decrypt `secret-1`.
        assert_eq!(log.open_entries(&id_out).expect("outsider"), None);
        assert_eq!(
            log.open_entries(&id2).expect("m2").unwrap()[0]
                .value
                .as_slice(),
            b"secret-1"
        );

        // Delete tombstones the entry cleanly.
        let del = Record::decode(
            log.draft_delete(&m2, 1_005, ep_id, &ep_key, "api-key")
                .expect("draft delete")
                .sign(&w2, inc2)
                .expect("sign delete")
                .bytes(),
        )
        .expect("decode delete");
        let cutoff = *del.id();
        log.insert(del).expect("insert delete");
        assert!(
            log.open_entries(&id1)
                .expect("m1 after delete")
                .unwrap()
                .is_empty()
        );

        // Removing m2 immediately revokes m2's read access in `open_entries`.
        let rem = log
            .draft(&m1, 1_006, Body::Remove { member: m2, cutoff })
            .sign(&w1, inc1)
            .expect("sign remove");
        log.insert(rem).expect("insert remove");
        assert_eq!(log.open_entries(&id2).expect("removed m2"), None);
    }

    /// A three-member log (m1 founds, admits m2 and m9) whose `[everyone]` epoch is wrapped to all
    /// three.
    struct Three {
        log: Log,
        w1: [u8; 32],
        inc1: [u8; 16],
        m1: MemberId,
        w2: [u8; 32],
        inc2: [u8; 16],
        m2: MemberId,
        id1: Identity,
        id2: Identity,
        epoch: RecordId,
        key: Zeroizing<[u8; 32]>,
    }

    fn three() -> Three {
        let (w1, inc1, id1, m1, e1) = make_entrant(51, "m1");
        let (w2, inc2, id2, m2, e2) = make_entrant(52, "m2");
        let (_, _, id9, _, e9) = make_entrant(59, "m9");
        let mut log = Log::new(Record::found(&[7; 32], vec![e1], 1_000).unwrap()).unwrap();
        for entrant in [e2, e9] {
            let admit = log
                .draft(
                    &m1,
                    1_001,
                    Body::Admit {
                        entrant,
                        evidence: Evidence::Phrase,
                    },
                )
                .sign(&w1, inc1)
                .unwrap();
            log.insert(admit).unwrap();
        }
        let (draft, key) = log.draft_epoch(&m1, 1_002, Audience::Everyone, 1).unwrap();
        let ep = draft.sign(&w1, inc1).unwrap();
        let epoch = *ep.id();
        log.insert(ep).unwrap();
        for id in [&id1, &id2, &id9] {
            let wrap = log
                .draft_wrap(&m1, 1_003, epoch, &key, id.recipient())
                .unwrap()
                .sign(&w1, inc1)
                .unwrap();
            log.insert(wrap).unwrap();
        }
        Three {
            log,
            w1,
            inc1,
            m1,
            w2,
            inc2,
            m2,
            id1,
            id2,
            epoch,
            key,
        }
    }

    fn put(name: &str, value: &[u8]) -> Entry {
        Entry {
            name: name.to_owned(),
            value: SecretBytes::from(value),
            audience: Audience::Everyone,
            if_absent: false,
            anchor: None,
        }
    }

    /// A fresh `[everyone]` epoch for m1 and m2 (m9 departs) on `log`, re-putting `entries`
    /// unchanged under it: the reseal a writer drafts when a Mac departs.
    fn reseal(log: &mut Log, t: &Three, generation: u64, entries: &[Entry]) {
        let (draft, key) = log
            .draft_epoch(&t.m1, 2_000, Audience::Everyone, generation)
            .unwrap();
        let ep = draft.sign(&t.w1, t.inc1).unwrap();
        let epoch = *ep.id();
        log.insert(ep).unwrap();
        for id in [&t.id1, &t.id2] {
            let wrap = log
                .draft_wrap(&t.m1, 2_001, epoch, &key, id.recipient())
                .unwrap()
                .sign(&t.w1, t.inc1)
                .unwrap();
            log.insert(wrap).unwrap();
        }
        for entry in entries {
            let rec = log
                .draft_put(&t.m1, 2_002, epoch, &key, entry)
                .unwrap()
                .sign(&t.w1, t.inc1)
                .unwrap();
            log.insert(rec).unwrap();
        }
    }

    /// The value `id` reads for `name`, if any.
    fn read(log: &Log, id: &Identity, name: &str) -> Option<Vec<u8>> {
        log.open_entries(id)
            .unwrap()
            .unwrap()
            .into_iter()
            .find(|e| e.name == name)
            .map(|e| e.value.as_slice().to_vec())
    }

    /// One side of a split reseals every entry when a Mac departs while the other edits or
    /// deletes them under the old epoch. After the heal every edit and every delete stands on
    /// every Mac, with no conflict: a reseal carries a value, it never writes one. Sixteen names
    /// of each, so the values pass a lowest-id tie-break by chance once in 2^32 runs, and the
    /// conflict check never does.
    ///
    /// NEGATIVE CONTROL: classify nothing as a carry (`Register::classify` leaves every
    /// `carries[k]` false): each register's two siblings tie-break by id, so about half the edits
    /// are undone, half the deleted entries come back, and all 32 read as conflicts.
    #[test]
    fn a_reseal_on_one_side_of_a_split_never_undoes_an_edit_or_delete_on_the_other() {
        let t = three();
        let mut base = t.log.clone();
        let names: Vec<String> = (0..32).map(|i| format!("k{i:02}")).collect();
        let v0: Vec<Entry> = names.iter().map(|n| put(n, b"v0")).collect();
        for entry in &v0 {
            let rec = base
                .draft_put(&t.m1, 1_500, t.epoch, &t.key, entry)
                .unwrap()
                .sign(&t.w1, t.inc1)
                .unwrap();
            base.insert(rec).unwrap();
        }

        // Side A: m9 departs, m1 reseals every entry under a fresh epoch.
        let mut side_a = base.clone();
        reseal(&mut side_a, &t, 2, &v0);
        // Side B: m2 rotates the first sixteen and deletes the rest under the old epoch.
        let mut side_b = base.clone();
        for (i, name) in names.iter().enumerate() {
            let draft = if i < 16 {
                side_b.draft_put(&t.m2, 1_600, t.epoch, &t.key, &put(name, b"v1-rotated"))
            } else {
                side_b.draft_delete(&t.m2, 1_600, t.epoch, &t.key, name)
            };
            side_b
                .insert(draft.unwrap().sign(&t.w2, t.inc2).unwrap())
                .unwrap();
        }

        side_a.merge(&side_b).unwrap();
        for id in [&t.id1, &t.id2] {
            for (i, name) in names.iter().enumerate() {
                let want = (i < 16).then(|| b"v1-rotated".to_vec());
                assert_eq!(read(&side_a, id, name), want, "{name} after the heal");
            }
            let opened = side_a.open_entry_set(id).unwrap().unwrap();
            assert!(
                opened.conflicts.is_empty(),
                "a reseal read as a conflict: {:?}",
                opened.conflicts
            );
        }
    }

    /// Two concurrent edits are a conflict. A reseal after it carries the winning value and leaves
    /// the other a candidate; a `Delete` then a `Put` (a person's save) settles it.
    ///
    /// NEGATIVE CONTROL: let a carry replace every op in its past (drop the `same_op` clause in
    /// `Register::siblings`): the reseal settles the conflict, and nobody is told.
    #[test]
    fn a_reseal_keeps_a_conflict_and_a_save_settles_it() {
        let t = three();
        let mut side_a = t.log.clone();
        let mut side_b = t.log.clone();
        side_a
            .insert(
                side_a
                    .draft_put(&t.m1, 1_500, t.epoch, &t.key, &put("c", b"from-m1"))
                    .unwrap()
                    .sign(&t.w1, t.inc1)
                    .unwrap(),
            )
            .unwrap();
        side_b
            .insert(
                side_b
                    .draft_put(&t.m2, 1_500, t.epoch, &t.key, &put("c", b"from-m2"))
                    .unwrap()
                    .sign(&t.w2, t.inc2)
                    .unwrap(),
            )
            .unwrap();
        side_a.merge(&side_b).unwrap();
        let conflict = (Audience::Everyone, "c".to_owned());
        let opened = side_a.open_entry_set(&t.id2).unwrap().unwrap();
        assert!(opened.conflicts.contains(&conflict));
        let winner = read(&side_a, &t.id2, "c").unwrap();

        // m9 departs: m1 reseals the value every Mac writes.
        reseal(&mut side_a, &t, 2, &[put("c", &winner)]);
        let opened = side_a.open_entry_set(&t.id2).unwrap().unwrap();
        assert!(
            opened.conflicts.contains(&conflict),
            "a reseal settled a conflict nobody saw"
        );
        assert_eq!(read(&side_a, &t.id2, "c").unwrap(), winner);

        // A person saves the winning value: a Delete, then a Put, under the newest epoch.
        let held = side_a.open_audience_entries(&t.id1).unwrap().unwrap();
        let newest = held.maximal_epochs[&EpochAudience::Everyone][0];
        let key = held.unwrapped_keys[&newest].clone();
        let del = side_a
            .draft_delete(&t.m1, 3_000, newest, &key, "c")
            .unwrap()
            .sign(&t.w1, t.inc1)
            .unwrap();
        side_a.insert(del).unwrap();
        let save = side_a
            .draft_put(&t.m1, 3_001, newest, &key, &put("c", &winner))
            .unwrap()
            .sign(&t.w1, t.inc1)
            .unwrap();
        side_a.insert(save).unwrap();
        let opened = side_a.open_entry_set(&t.id2).unwrap().unwrap();
        assert!(opened.conflicts.is_empty());
        assert_eq!(read(&side_a, &t.id2, "c").unwrap(), winner);
    }

    /// Registers merge one name at a time: one side of a split removes an anchored set while the
    /// other adds an entry to it, and after the heal that entry's anchor is gone. It is dropped,
    /// so the set goes whole and every Mac's merge of what it reads stays valid.
    ///
    /// NEGATIVE CONTROL: skip `drop_dangling_anchors` in `open_audience_entries`: `new` stays,
    /// anchored to the deleted `seed`, and `text::merge_for` refuses what every Mac reads.
    #[test]
    fn an_entry_added_to_a_set_the_other_side_removed_goes_with_the_set() {
        let t = three();
        let mut base = t.log.clone();
        let anchored = |name: &str, anchor: Option<&str>| Entry {
            name: name.to_owned(),
            value: SecretBytes::from(name.as_bytes()),
            audience: Audience::Everyone,
            if_absent: true,
            anchor: anchor.map(str::to_owned),
        };
        for entry in [
            anchored("seed", None),
            anchored("old", Some("seed")),
            put("plain", b"p"),
        ] {
            let rec = base
                .draft_put(&t.m1, 1_500, t.epoch, &t.key, &entry)
                .unwrap()
                .sign(&t.w1, t.inc1)
                .unwrap();
            base.insert(rec).unwrap();
        }
        let mut side_a = base.clone();
        for name in ["old", "seed"] {
            let rec = side_a
                .draft_delete(&t.m1, 1_600, t.epoch, &t.key, name)
                .unwrap()
                .sign(&t.w1, t.inc1)
                .unwrap();
            side_a.insert(rec).unwrap();
        }
        let mut side_b = base.clone();
        let rec = side_b
            .draft_put(
                &t.m2,
                1_600,
                t.epoch,
                &t.key,
                &anchored("new", Some("seed")),
            )
            .unwrap()
            .sign(&t.w2, t.inc2)
            .unwrap();
        side_b.insert(rec).unwrap();

        side_a.merge(&side_b).unwrap();
        let entries = side_a.open_entries(&t.id2).unwrap().unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["plain"], "what m2 reads after the heal");
        text::merge_for("mac-52", entries).expect("every Mac's merge stays valid");
        let opened = side_a.open_audience_entries(&t.id1).unwrap().unwrap();
        assert!(!opened.by_audience[&Audience::Everyone].contains_key("new"));
    }

    #[test]
    fn draft_past_parent_limit_pins_referenced_epoch_and_resolves_rehomed_host_entries() {
        let (w1, inc1, id1, m1, e1) = make_entrant(11, "m1");
        let (w2, inc2, id2, m2, e2) = make_entrant(12, "m2");
        let (_, _, id3, _, _) = make_entrant(13, "m3");

        let genesis = Record::found(&[9; 32], vec![e1, e2], 2_000).expect("genesis");
        let base = Log::new(genesis).expect("log");
        let mut log = base.clone();

        // Mint MAX_PARENTS + 1 concurrent heads from `base` so `log.heads().len() > MAX_PARENTS`,
        // with the Epoch on one of the heads so naive lowest-16 parent selection could drop it.
        for n in 0..=super::super::MAX_PARENTS {
            let rec = Record::decode(
                base.draft(
                    &m1,
                    2_001 + n as u64,
                    Body::Name {
                        name: format!("branch-{n}"),
                    },
                )
                .sign(&w1, inc1)
                .expect("sign branch")
                .bytes(),
            )
            .expect("decode branch");
            log.insert(rec).expect("insert branch");
        }
        let (ep_draft, ep_key) = base
            .draft_epoch(&m1, 2_100, Audience::Everyone, 1)
            .expect("draft epoch");
        let ep_rec = Record::decode(ep_draft.sign(&w1, inc1).expect("sign epoch").bytes())
            .expect("decode epoch");
        let ep_id = *ep_rec.id();
        log.insert(ep_rec).expect("insert epoch");
        assert!(log.heads().len() > super::super::MAX_PARENTS);

        // Drafting a Wrap and Put by m2 when > MAX_PARENTS heads exist still includes `ep_id` in
        // causal ancestors and passes `Log::insert`.
        let wrap = Record::decode(
            log.draft_wrap(&m2, 2_200, ep_id, &ep_key, id1.recipient())
                .expect("draft wrap past MAX_PARENTS")
                .sign(&w2, inc2)
                .expect("sign wrap")
                .bytes(),
        )
        .expect("decode wrap");
        log.insert(wrap).expect("insert wrap past MAX_PARENTS");

        // Now test overlapping `Audience::Hosts` registers (e.g. an entry rehomed from `[m1 m2]`
        // to `[m1 m2 m3]` sequentially and concurrently).
        let aud_pair = Audience::Hosts(vec!["m1".to_owned(), "m2".to_owned()]);
        let aud_trio = Audience::Hosts(vec!["m1".to_owned(), "m2".to_owned(), "m3".to_owned()]);

        let (ep_pair_draft, key_pair) = log
            .draft_epoch(&m1, 2_300, aud_pair.clone(), 1)
            .expect("draft pair epoch");
        let ep_pair = Record::decode(ep_pair_draft.sign(&w1, inc1).unwrap().bytes()).unwrap();
        let ep_pair_id = *ep_pair.id();
        log.insert(ep_pair).unwrap();
        for id in [&id1, &id2] {
            let w = Record::decode(
                log.draft_wrap(&m1, 2_301, ep_pair_id, &key_pair, id.recipient())
                    .unwrap()
                    .sign(&w1, inc1)
                    .unwrap()
                    .bytes(),
            )
            .unwrap();
            log.insert(w).unwrap();
        }

        let (ep_trio_draft, key_trio) = log
            .draft_epoch(&m1, 2_302, aud_trio.clone(), 1)
            .expect("draft trio epoch");
        let ep_trio = Record::decode(ep_trio_draft.sign(&w1, inc1).unwrap().bytes()).unwrap();
        let ep_trio_id = *ep_trio.id();
        log.insert(ep_trio).unwrap();
        for id in [&id1, &id2, &id3] {
            let w = Record::decode(
                log.draft_wrap(&m1, 2_303, ep_trio_id, &key_trio, id.recipient())
                    .unwrap()
                    .sign(&w1, inc1)
                    .unwrap()
                    .bytes(),
            )
            .unwrap();
            log.insert(w).unwrap();
        }

        let fork_base = log.clone();

        // 1. Sequential rehome (`[m1 m2]` -> `[m1 m2 m3]`): newer host register wins with no error
        //    and no conflict.
        let put_old = Record::decode(
            log.draft_put(
                &m1,
                2_400,
                ep_pair_id,
                &key_pair,
                &Entry {
                    name: "rehomed-secret".to_owned(),
                    value: SecretBytes::from(b"old-pair".as_slice()),
                    audience: aud_pair.clone(),
                    if_absent: false,
                    anchor: None,
                },
            )
            .unwrap()
            .sign(&w1, inc1)
            .unwrap()
            .bytes(),
        )
        .unwrap();
        log.insert(put_old).unwrap();

        let put_new = Record::decode(
            log.draft_put(
                &m2,
                2_401,
                ep_trio_id,
                &key_trio,
                &Entry {
                    name: "rehomed-secret".to_owned(),
                    value: SecretBytes::from(b"new-trio".as_slice()),
                    audience: aud_trio.clone(),
                    if_absent: false,
                    anchor: None,
                },
            )
            .unwrap()
            .sign(&w2, inc2)
            .unwrap()
            .bytes(),
        )
        .unwrap();
        log.insert(put_new).unwrap();

        let opened_seq = log.open_entry_set(&id1).unwrap().unwrap();
        assert!(opened_seq.conflicts.is_empty());
        assert_eq!(opened_seq.entries.len(), 1);
        assert_eq!(opened_seq.entries[0].value.as_slice(), b"new-trio");

        // 2. Concurrent writes to `[m1 m2]` and `[m1 m2 m3]`: deterministic tie-break by lowest
        //    `RecordId` and both host audiences flagged in `conflicts`.
        let mut concurrent_log = fork_base.clone();
        let c_pair = Record::decode(
            fork_base
                .draft_put(
                    &m1,
                    2_500,
                    ep_pair_id,
                    &key_pair,
                    &Entry {
                        name: "rehomed-secret".to_owned(),
                        value: SecretBytes::from(b"concurrent-pair".as_slice()),
                        audience: aud_pair.clone(),
                        if_absent: false,
                        anchor: None,
                    },
                )
                .unwrap()
                .sign(&w1, inc1)
                .unwrap()
                .bytes(),
        )
        .unwrap();
        let c_trio = Record::decode(
            fork_base
                .draft_put(
                    &m2,
                    2_501,
                    ep_trio_id,
                    &key_trio,
                    &Entry {
                        name: "rehomed-secret".to_owned(),
                        value: SecretBytes::from(b"concurrent-trio".as_slice()),
                        audience: aud_trio.clone(),
                        if_absent: false,
                        anchor: None,
                    },
                )
                .unwrap()
                .sign(&w2, inc2)
                .unwrap()
                .bytes(),
        )
        .unwrap();
        concurrent_log.insert(c_pair.clone()).unwrap();
        concurrent_log.insert(c_trio.clone()).unwrap();

        let opened_conc = concurrent_log.open_entry_set(&id1).unwrap().unwrap();
        assert!(
            opened_conc
                .conflicts
                .contains(&(aud_pair, "rehomed-secret".to_owned()))
        );
        assert!(
            opened_conc
                .conflicts
                .contains(&(aud_trio, "rehomed-secret".to_owned()))
        );
        let expected: &[u8] = if c_pair.id() < c_trio.id() {
            b"concurrent-pair"
        } else {
            b"concurrent-trio"
        };
        assert_eq!(opened_conc.entries[0].value.as_slice(), expected);
    }
}
