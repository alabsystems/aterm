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
//!   only ops writing that value; so does a `Delete` issued where every live op in its past is a
//!   `Delete` (a reseal re-issuing one, F21 rule 6).
//! - Keyed epochs (F21, the evaluator's "Keyed epochs"): a counting `Put` or `Delete` that
//!   depends on an epoch that stopped counting opens through that epoch's key
//!   ([`OpenedAudienceEntries::keyed_keys`]), and the winners it seals stay apart from the
//!   counting ones ([`OpenedAudienceEntries::keyed_winning`]), so no writer reuses the epoch and
//!   no member hands it on.

use super::{
    Body, Draft, EpochAudience, Kind, Log, MAX_FOUNDERS, MemberId, Reader, Record, RecordId, eval,
};
use crate::age::{self, AgeError, Identity, Recipient};
use crate::text::{self, Audience, Entry};
use crate::{Error, Result, SecretBytes};
use aterm_digest::Sha256;
use register::Register;
use ring::{aead, hkdf};
use std::collections::{BTreeMap, BTreeSet};
use zeroize::{Zeroize, Zeroizing};

pub(crate) mod inner;
mod keyed;
mod register;

/// `f`, its openings on this thread run under the NEGATIVE CONTROL `entries-arrival-order`: no
/// sibling beats another, so the first a Mac's log holds wins, and the register oracle counts the
/// disagreement rather than failing on it. For a test of law 5 to show it catches a register that
/// reads arrival order.
#[cfg(test)]
fn reading_arrival_order<T>(f: impl FnOnce() -> T) -> T {
    register::oracle::COUNT_ONLY.set(true);
    register::ARRIVAL_ORDER.set(true);
    let read = f();
    register::ARRIVAL_ORDER.set(false);
    register::oracle::COUNT_ONLY.set(false);
    read
}

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
    /// `basis` is due a rotation ([`Log::table_basis_due`]: P1's gate, [`Log::head_due`]): its
    /// causal past lacks a revoking row's entry or the `Remove`s that row's gate needs, or it is
    /// still wrapped to a recipient the log removed. A row sealed under it now could reach that
    /// Mac, so no writer seals under it until the rotation lands.
    pub due: bool,
}

/// One keyed epoch (F21 rule 4), as [`Log::keyed_epochs`] reads it from the records alone: an
/// epoch that stopped counting, which an honest counting write relied on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keyed {
    /// The keyed [`Body::Epoch`].
    pub epoch: RecordId,
    /// Its audience.
    pub audience: EpochAudience,
    /// The counting `Put`s and `Delete`s that depend on it, in record order.
    pub dependents: Vec<RecordId>,
    /// Who holds its key: its dependents' signers, and each current member that a
    /// [`Body::Wrap`] of it which may open it (one that counts, or one in a dependent's past) is
    /// addressed to.
    pub holders: BTreeSet<MemberId>,
    /// A full reseal of its audience has every dependent in its past: a counting epoch whose
    /// signer, there, held the key of every newest counting epoch of the audience and had attested
    /// (wrapped to itself, in a counting `Wrap`) every keyed epoch of the audience with a
    /// dependent in its past.
    pub covered: bool,
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
    /// Epochs that count and currently seal at least one winning live entry (or the winning
    /// Commons control descriptor), grouped by [`EpochAudience`]. A keyed epoch never enters it.
    pub winning_epochs: BTreeMap<EpochAudience, BTreeSet<RecordId>>,
    /// Keyed epochs (F21: an epoch that stopped counting, which an honest counting write relied
    /// on) that seal at least one winning live entry or the winning Commons control descriptor,
    /// grouped by [`EpochAudience`]. They carry no authority: no writer reuses one and no member
    /// hands one on.
    pub keyed_winning: BTreeMap<EpochAudience, BTreeSet<RecordId>>,
    /// Causally maximal effective [`Body::Epoch`] record ids per [`EpochAudience`].
    pub maximal_epochs: BTreeMap<EpochAudience, Vec<RecordId>>,
    /// Highest effective `generation` observed per [`EpochAudience`].
    pub max_generation: BTreeMap<EpochAudience, u64>,
    /// Effective [`Body::Wrap`] recipients observed per `epoch` [`RecordId`].
    pub wrapped_recipients: BTreeMap<RecordId, BTreeSet<Recipient>>,
    /// `(Audience, name)` registers holding two or more concurrent sibling writes that write
    /// different things (a reseal, which carries a value unchanged, is none).
    pub conflicts: BTreeSet<(Audience, String)>,
    /// Unwrapped symmetric keys of epochs that count, keyed by `epoch` [`RecordId`]. Every path
    /// that decides what to wrap or reuse reads this, so it never holds a keyed epoch.
    pub unwrapped_keys: BTreeMap<RecordId, Zeroizing<[u8; 32]>>,
    /// Unwrapped symmetric keys of keyed epochs (F21 rule 2), keyed by `epoch` [`RecordId`]: they
    /// open the dependents sealed under those epochs, and nothing else.
    pub keyed_keys: BTreeMap<RecordId, Zeroizing<[u8; 32]>>,
    /// Uncovered keyed epochs (F21 rule 4) that a current member holds, by audience, in record
    /// order: each waits for a holder's reseal, whether or not this [`Identity`] opens it.
    pub uncovered_keyed: BTreeMap<EpochAudience, Vec<RecordId>>,
    /// Names whose winning op is a `Delete` sealed under a keyed epoch, by [`Audience`]: a Mac
    /// without that epoch's key still reads the value it deleted, so a reseal issues each again
    /// under its fresh epoch (F21 rule 5).
    pub keyed_deletes: BTreeMap<Audience, BTreeSet<String>>,
    /// The op each winning live entry of [`OpenedAudienceEntries::by_audience`] stands for, by
    /// `(Audience, name)`: its winning `Put`, or for a carry the op whose value it carries. F21
    /// rule 8 reads who signed it, when, and under which epoch ([`Log::read_after_cutoff`]).
    pub winning_ops: BTreeMap<(Audience, String), RecordId>,
    /// The winning `Put` itself of each winning live entry, by `(Audience, name)`: for a carry,
    /// the carry, not the op whose value it carries ([`OpenedAudienceEntries::winning_ops`]). Who
    /// signed the entry as it stands now, under which epoch and in which draft
    /// (`claude_token`'s one-draft check, a member write's outcome).
    pub winning_puts: BTreeMap<(Audience, String), RecordId>,
    /// The winning `Delete` of each register whose winner is one, by `(Audience, name)`: the op
    /// that took the entry away. One-mesh W3's journal of overtaken writes names it
    /// (`client::log_entries`' `Overtaken`).
    pub winning_deletes: BTreeMap<(Audience, String), RecordId>,
    /// Audiences that have at least one effective [`Body::Epoch`] and whose maximal epochs were all
    /// unwrapped by this [`Identity`], less each audience with an uncovered keyed epoch it did not
    /// open (F21 rule 7), whose writes this Mac cannot read yet.
    pub fully_unwrapped_audiences: BTreeSet<EpochAudience>,
    /// Whether any effective vault [`Audience`]'s maximal [`Body::Epoch`] was not unwrapped by
    /// `identity`, or a vault audience has an uncovered keyed epoch `identity` did not open.
    pub hidden_epochs: bool,
    /// Audiences with a keyed epoch a current member holds, covered or not, that this
    /// [`Identity`] did not open ([`OpenedAudienceEntries::reputs_whole`]).
    pub keyless_keyed: BTreeSet<EpochAudience>,
}

impl OpenedAudienceEntries {
    /// Whether `audience` has an uncovered keyed epoch (F21 rule 4) this [`Identity`] did not
    /// open: writes it cannot read wait there for a holder's reseal (rule 7).
    #[must_use]
    pub fn awaits_reseal(&self, audience: &EpochAudience) -> bool {
        self.uncovered_keyed
            .get(audience)
            .is_some_and(|epochs| epochs.iter().any(|e| !self.keyed_keys.contains_key(e)))
    }

    /// Whether a writer here may seal every entry of `audience` again under a fresh epoch (F21
    /// rule 7): it opens the whole audience, and every keyed epoch a current member holds there,
    /// covered or not. Where it lacks one, a reseal's re-put of an honest write it cannot open
    /// reads here as a write, not a carry, so this Mac's view can differ from a holder's (an
    /// edit made between the heal and the reseal can lose a tie-break here and win there); a
    /// full re-put of that view would revert the edit on every Mac that opens the epoch. Such a
    /// Mac writes only what changed, for good.
    #[must_use]
    pub fn reputs_whole(&self, audience: &EpochAudience) -> bool {
        self.fully_unwrapped_audiences.contains(audience) && !self.keyless_keyed.contains(audience)
    }
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

/// The same selected entries with their value-free register evidence. The
/// public convenience readers discard this evidence only after selection.
pub(crate) struct SelectedEntries {
    pub(crate) opened: OpenedEntries,
    #[cfg(any(unix, test))]
    pub(crate) operations: BTreeMap<(Audience, String), RecordId>,
    #[cfg(any(unix, test))]
    pub(crate) puts: BTreeMap<(Audience, String), RecordId>,
    /// Audiences whose current and retained keyed history this reader opens
    /// completely. Other private audiences do not obscure a complete one.
    #[cfg(any(unix, test))]
    pub(crate) complete: BTreeSet<Audience>,
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

/// The most wire bytes the records a re-key signs take, by kind: each record's header with
/// [`super::MAX_PARENTS`] parents around its body, as [`super::Kind::most_bytes`] counts a kind
/// whose fields bound it, here for the kinds whose body depends on their audience and value. A
/// re-key's room is reserved in bytes as well as records with them (one-mesh proofs P6, F6:
/// `client/remove.rs`' `RemovalFill`).
#[cfg(any(unix, test))]
pub(crate) mod most_bytes {
    use super::{DELETE_MAGIC, EpochAudience, PUT_MAGIC, encode_audience_wire};

    /// The AEAD tag a `Put` or `Delete` ciphertext carries past its plaintext.
    const TAG: usize = 16;

    fn audience(audience: &EpochAudience) -> usize {
        let mut out = Vec::new();
        encode_audience_wire(&mut out, audience);
        out.len()
    }

    /// An `Epoch` of `of`: its audience, generation and commitment.
    #[must_use]
    pub(crate) fn epoch(of: &EpochAudience) -> usize {
        super::super::MOST_FRAME + audience(of) + 8 + 32
    }

    /// A `Put` under an epoch of `of` sealing an entry whose name is `name` bytes long, with an
    /// anchor of `anchor` bytes or none, and a value of `value` bytes (the Commons control
    /// descriptor is the value of an entry named `commons::ENTRY` with no anchor).
    #[must_use]
    pub(crate) fn put(
        of: &EpochAudience,
        name: usize,
        anchor: Option<usize>,
        value: usize,
    ) -> usize {
        let plaintext = PUT_MAGIC.len() + 1 + 2 + name + anchor.map_or(0, |a| 2 + a) + 4 + value;
        super::super::MOST_FRAME + audience(of) + 32 + 32 + 4 + plaintext + TAG
    }

    /// A `Delete` under an epoch of `of` of an entry whose name is `name` bytes long.
    #[must_use]
    pub(crate) fn delete(of: &EpochAudience, name: usize) -> usize {
        let plaintext = DELETE_MAGIC.len() + 2 + name;
        super::super::MOST_FRAME + audience(of) + 32 + 32 + 2 + plaintext + TAG
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

#[cfg(test)]
thread_local! {
    static ENTRY_AEAD_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn open_aead(
    epoch_key: &[u8; 32],
    record: &Record,
    audience: &EpochAudience,
    epoch: &RecordId,
    salt: &[u8; 32],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    #[cfg(test)]
    ENTRY_AEAD_READS.with(|reads| reads.set(reads.get() + 1));
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

pub(super) fn open_wrap(
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

#[cfg(test)]
thread_local! {
    /// A build before the Claude Code token class, on for one test thread: a class name's value
    /// is neither sealed again on encode nor opened on decode, so it reads and is re-`Put` as the
    /// outer layer alone holds it (DESIGN-claude-setup-token §4.4).
    pub(crate) static OUTER_ONLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether the value of the entry `name` carries the inner seal: a Claude Code token entry
/// ([`crate::entry_class::claude_token`]), in a build that knows the class.
fn inner_sealed(name: &str) -> bool {
    #[cfg(test)]
    if OUTER_ONLY.with(std::cell::Cell::get) {
        return false;
    }
    crate::entry_class::claude_token(name)
}

/// `entry` as a Mac holding the epoch keys `unwrapped` and `keyed` on `network` reads it: a
/// Claude Code token entry's value opened from its inner seal ([`inner::load`]).
fn open_inner(
    network: &RecordId,
    unwrapped: &EpochKeys,
    keyed: &EpochKeys,
    mut entry: Entry,
) -> Entry {
    if inner_sealed(&entry.name) {
        let value = inner::load(
            network,
            |epoch| unwrapped.get(epoch).or_else(|| keyed.get(epoch)),
            &entry.name,
            entry.value.as_slice(),
        );
        entry.value = SecretBytes::from(value.as_slice());
    }
    entry
}

/// How many bytes the value of the entry `name` takes in a `Put`'s plaintext, for the in-memory
/// `value`: a Claude Code token entry's with its inner seal ([`inner::stored_len`]).
#[cfg(unix)]
#[must_use]
pub(crate) fn stored_value_len(name: &str, value: &[u8]) -> usize {
    if inner_sealed(name) {
        inner::stored_len(value)
    } else {
        value.len()
    }
}

#[derive(Clone)]
enum DecodedOp {
    Put(EntryMeta),
    Delete,
}

/// Selection facts for a valid Put. Its value stays in the immutable original record.
#[derive(Clone)]
struct EntryMeta {
    index: usize,
    name: String,
    if_absent: bool,
    anchor: Option<String>,
    value_class: usize,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct ValueDigest([u8; 32]);

impl Drop for ValueDigest {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Default)]
struct ValueClasses {
    representatives: BTreeMap<(usize, ValueDigest), Vec<usize>>,
}

impl ValueClasses {
    fn intern(
        &mut self,
        opening: &EntryOpening<'_>,
        index: usize,
        value: &[u8],
        digest: fn(&[u8]) -> [u8; 32],
    ) -> Result<usize> {
        let bucket = self
            .representatives
            .entry((value.len(), ValueDigest(digest(value))))
            .or_default();
        for &representative in bucket.iter() {
            let previous = opening.text_put(representative)?;
            if aterm_digest::ct_eq(value, previous.value.as_slice()) {
                return Ok(representative);
            }
        }
        bucket.push(index);
        Ok(index)
    }
}

type EpochKeys = BTreeMap<RecordId, Zeroizing<[u8; 32]>>;

struct EntryOpening<'a> {
    log: &'a Log,
    unwrapped: &'a EpochKeys,
    keyed: &'a EpochKeys,
    effective: &'a eval::Effective,
}

impl EntryOpening<'_> {
    fn text_put(&self, index: usize) -> Result<Entry> {
        let record = &self.log.order[index];
        let Body::Put {
            audience,
            epoch,
            salt,
            ciphertext,
        } = record.body()
        else {
            return Err(Error::new("entry reference is not a Put"));
        };
        let Some(text_audience) = audience.as_text_audience() else {
            return Err(Error::new("entry reference is not a text audience"));
        };
        let key = self
            .log
            .op_key(index, epoch, self.unwrapped, self.keyed, self.effective)
            .ok_or_else(|| Error::new("entry reference has no readable epoch"))?;
        let plain = open_aead(key, record, audience, epoch, salt, ciphertext)?;
        let entry = decode_put_plaintext(&text_audience, &plain)?;
        Ok(self.inner(entry))
    }

    /// `entry` as a reader holds it: a Claude Code token entry's value opened from its inner
    /// seal where this Mac holds the epoch it names ([`inner::load`]); any other entry as it is.
    fn inner(&self, entry: Entry) -> Entry {
        open_inner(self.log.network(), self.unwrapped, self.keyed, entry)
    }

    fn commons_put(&self, index: usize) -> Result<SecretBytes> {
        let record = &self.log.order[index];
        let Body::Put {
            audience: audience @ EpochAudience::Commons,
            epoch,
            salt,
            ciphertext,
        } = record.body()
        else {
            return Err(Error::new("entry reference is not a Commons Put"));
        };
        let key = self
            .log
            .op_key(index, epoch, self.unwrapped, self.keyed, self.effective)
            .ok_or_else(|| Error::new("entry reference has no readable epoch"))?;
        let plain = open_aead(key, record, audience, epoch, salt, ciphertext)?;
        decode_commons_put_plaintext(&plain)
    }
}

struct ResolvedEntries {
    summary: OpenedAudienceEntries,
    entries: BTreeMap<Audience, BTreeMap<String, EntryMeta>>,
    commons: Option<usize>,
}

#[cfg(test)]
std::thread_local! {
    static HOST_WRAPPER_PRECEDENCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl ResolvedEntries {
    fn into_full(self, log: &Log) -> Result<OpenedAudienceEntries> {
        let Self {
            mut summary,
            entries,
            commons,
        } = self;
        let opening = EntryOpening {
            log,
            unwrapped: &summary.unwrapped_keys,
            keyed: &summary.keyed_keys,
            effective: log.effective(),
        };
        let mut by_audience = BTreeMap::new();
        for (audience, entries) in entries {
            let mut values = BTreeMap::new();
            for (name, entry) in entries {
                values.insert(name, opening.text_put(entry.index)?);
            }
            by_audience.insert(audience, values);
        }
        let control = commons
            .map(|index| opening.commons_put(index))
            .transpose()?;
        summary.by_audience = by_audience;
        summary.commons_control = control;
        Ok(summary)
    }

    #[cfg(any(unix, test))]
    fn into_named(self, log: &Log, names: &[&str]) -> Result<Vec<Entry>> {
        Ok(self.into_selection(log, Some(names))?.opened.entries)
    }

    fn into_selection(self, log: &Log, names: Option<&[&str]>) -> Result<SelectedEntries> {
        let Self {
            summary,
            entries,
            commons,
        } = self;
        let opening = EntryOpening {
            log,
            unwrapped: &summary.unwrapped_keys,
            keyed: &summary.keyed_keys,
            effective: log.effective(),
        };
        #[cfg(any(unix, test))]
        let complete = entries
            .keys()
            .filter(|audience| summary.reputs_whole(&EpochAudience::from(*audience)))
            .cloned()
            .collect();
        let mut selected = BTreeMap::new();
        for (name, entry) in merge_visible_entries(entries, &summary.winning_host_audience) {
            if names.is_none_or(|names| names.contains(&name.as_str())) {
                selected.insert(name, opening.text_put(entry.index)?);
            }
        }
        if names.is_none_or(|names| names.contains(&crate::commons::ENTRY))
            && let Some(index) = commons
        {
            selected.insert(
                crate::commons::ENTRY.to_owned(),
                Entry {
                    name: crate::commons::ENTRY.to_owned(),
                    value: opening.commons_put(index)?,
                    audience: Audience::Everyone,
                    if_absent: false,
                    anchor: None,
                },
            );
        }
        #[cfg(any(unix, test))]
        let selected_key = |key: &(Audience, String)| {
            selected
                .get(&key.1)
                .is_some_and(|entry| entry.audience == key.0)
        };
        #[cfg(any(unix, test))]
        let operations = summary
            .winning_ops
            .into_iter()
            .filter(|(key, _)| selected_key(key))
            .collect();
        #[cfg(any(unix, test))]
        let puts = summary
            .winning_puts
            .into_iter()
            .filter(|(key, _)| selected_key(key))
            .collect();
        Ok(SelectedEntries {
            opened: OpenedEntries {
                entries: selected.into_values().collect(),
                conflicts: summary.conflicts,
                unwrapped_epochs: summary.unwrapped_keys.into_keys().collect(),
                hidden_epochs: summary.hidden_epochs,
            },
            #[cfg(any(unix, test))]
            operations,
            #[cfg(any(unix, test))]
            puts,
            #[cfg(any(unix, test))]
            complete,
        })
    }

    #[cfg(any(unix, test))]
    fn into_audience_named(
        self,
        log: &Log,
        audience: &Audience,
        names: &[&str],
    ) -> Result<Vec<Entry>> {
        let Self {
            summary,
            mut entries,
            ..
        } = self;
        let opening = EntryOpening {
            log,
            unwrapped: &summary.unwrapped_keys,
            keyed: &summary.keyed_keys,
            effective: log.effective(),
        };
        entries
            .remove(audience)
            .into_iter()
            .flatten()
            .filter(|(name, _)| names.contains(&name.as_str()))
            .map(|(_, entry)| opening.text_put(entry.index))
            .collect()
    }
}

fn merge_visible_entries<T>(
    by_audience: BTreeMap<Audience, BTreeMap<String, T>>,
    winning_host_audience: &BTreeMap<String, Audience>,
) -> BTreeMap<String, T> {
    let mut everyone = BTreeMap::new();
    let mut hosts = BTreeMap::new();
    for (audience, entries) in by_audience {
        for (name, entry) in entries {
            match &audience {
                Audience::Everyone => {
                    everyone.insert(name, entry);
                }
                Audience::Hosts(_) if winning_host_audience.get(&name) == Some(&audience) => {
                    hosts.insert(name, entry);
                }
                Audience::Hosts(_) => {}
            }
        }
    }
    everyone.extend(hosts);
    everyone
}

fn value_digest(value: &[u8]) -> [u8; 32] {
    Sha256::digest(value)
}

struct CandidateOp {
    index: usize,
    record_id: RecordId,
    epoch: RecordId,
    op: DecodedOp,
}

/// Drop each winning entry whose anchor (`if-absent=<anchor>`) no view of `by_audience` holds as a
/// live `if-absent` entry, as a vault text refuses one: registers merge one name at a time, so a
/// split where one side removed an anchored set while the other added to it, or a member's lone
/// `Put` naming an anchor that is not there, leaves such an entry. Kept, every Mac's materializer
/// would refuse it (and with it every read), and an admin's overlay could not commit it. Dropped,
/// a set stays or goes whole. A view is `[everyone]`, or `[everyone]` under each host's winning
/// host-list entries, as [`Log::open_entry_set`] merges them.
fn drop_dangling_anchors(
    by_audience: &mut BTreeMap<Audience, BTreeMap<String, EntryMeta>>,
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
            let mut view: BTreeMap<&str, (&Audience, &EntryMeta)> = BTreeMap::new();
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
                if !text::anchor_received_fields(&entry.name, entry.anchor.as_deref(), |anchor| {
                    view.get(anchor).map(|(_, entry)| entry.if_absent)
                }) {
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

struct CommonsControlOp {
    index: usize,
    record_id: RecordId,
    value: SecretBytes,
}

struct ControlRef {
    index: usize,
    record_id: RecordId,
    epoch: RecordId,
}

impl Log {
    /// Replay the previous host-wrapper selector while priming an upgrade fixture.
    /// Only its actual read/apply is changed; the scope restores the current selector.
    #[cfg(all(test, unix))]
    pub(crate) fn with_host_wrapper_precedence<T>(read: impl FnOnce() -> T) -> T {
        struct Restore(bool);
        impl Drop for Restore {
            fn drop(&mut self) {
                HOST_WRAPPER_PRECEDENCE.set(self.0);
            }
        }
        let _restore = Restore(HOST_WRAPPER_PRECEDENCE.replace(true));
        read()
    }

    /// Whether this log holds at least one effective [`Body::Epoch`] record.
    #[must_use]
    pub fn has_epochs(&self) -> bool {
        let effective = self.effective();
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
        let effective = self.effective();
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
        self.draft_epoch_following(actor, authored_ms, audience, generation, &[])
    }

    /// [`Log::draft_epoch`], with each held record of `follow` in the epoch's causal past: F21's
    /// reseal follows every dependent of the keyed epochs it retires ([`Log::draft`] pins the head
    /// that reaches each, as it pins a `Put`'s epoch).
    ///
    /// A fresh epoch's own batch seals under it without asking [`Log::head_due`], so the epoch's
    /// gate must be open in this log by construction (one-mesh proofs P1b, `fresh_batch_open`):
    /// the draft pins what the gate reads first ([`Log::gate_pins`], before `follow`), and an
    /// epoch whose drafted past leaves the gate shut ([`Log::gate_opens_after`]) is refused. Only
    /// past [`MAX_PARENTS`](super::MAX_PARENTS) concurrent heads can a pin be left out
    /// ([`Log::draft`]): a row whose recipient a live seat shares needs every `Remove` that took
    /// it, so a seated key's flood of concurrent forked `Remove`s of such a row refuses every
    /// epoch until it is merged.
    ///
    /// # Errors
    /// As [`Log::draft_epoch`], and where the drafted epoch's gate is shut.
    pub fn draft_epoch_following(
        &self,
        actor: &MemberId,
        authored_ms: u64,
        audience: impl Into<EpochAudience>,
        generation: u64,
        follow: &[RecordId],
    ) -> Result<(Draft, Zeroizing<[u8; 32]>)> {
        let audience = audience.into();
        check_audience(&audience)?;
        if generation == 0 {
            return Err(Error::new("epoch generation starts at 1"));
        }
        let mut epoch_key = Zeroizing::new([0u8; 32]);
        aterm_uds::rand::fill(epoch_key.as_mut())?;
        let commitment = epoch_commitment(self.network(), &audience, generation, &epoch_key);
        // NEGATIVE CONTROL `epoch-unpinned`: follow the lowest heads alone and ask nothing of
        // the gate, as before P1b.
        #[cfg(any(test, debug_assertions))]
        let unpinned = aterm_types::negative_control::on("epoch-unpinned");
        #[cfg(not(any(test, debug_assertions)))]
        let unpinned = false;
        let gated = if unpinned {
            Vec::new()
        } else {
            self.gate_pins()
        };
        let pinned: Vec<RecordId> = gated.iter().chain(follow).copied().collect();
        let draft = self.draft_following(
            actor,
            authored_ms,
            Body::Epoch {
                audience,
                generation,
                commitment,
            },
            &pinned,
        );
        if !unpinned && !self.gate_opens_after(&draft.parents) {
            return Err(Error::new(
                "too many concurrent heads for an epoch whose gate is open: more Removes of one \
                 shared seat than a record can follow",
            ));
        }
        Ok((draft, epoch_key))
    }

    /// What an epoch follows so that its gate is open in this log ([`Log::gate_opens_after`]):
    /// for each revoking row ([`Membership::revoking_rows`](eval::Membership::revoking_rows)),
    /// its entry, and the earliest `Remove` that takes it by itself where no live row binds its
    /// recipient, else every `Remove` that took it.
    pub(super) fn gate_pins(&self) -> Vec<RecordId> {
        let at = |id: &RecordId| self.held.get(id).copied().unwrap_or(usize::MAX);
        let mut pins = Vec::new();
        for row in self.membership_ref().revoking_rows() {
            pins.push(row.entry);
            match row.alone.iter().min_by_key(|id| at(id)) {
                Some(&one) if !row.recipient_live => pins.push(one),
                _ => pins.extend(row.removes.iter().copied()),
            }
        }
        pins
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
                capsule: capsule.into(),
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
        let plain = if inner_sealed(&entry.name) {
            let mut stored = entry.clone();
            stored.value = crate::SecretBytes::from(
                inner::store(
                    self.network(),
                    &epoch,
                    epoch_key,
                    &entry.name,
                    entry.value.as_slice(),
                )?
                .as_slice(),
            );
            encode_put_plaintext(&stored)?
        } else {
            encode_put_plaintext(entry)?
        };
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
                ciphertext: ciphertext.into(),
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
                ciphertext: ciphertext.into(),
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
                ciphertext: ciphertext.into(),
            },
        ))
    }

    /// Age recipients belonging to Macs that were removed and not restored in this log.
    #[must_use]
    pub fn removed_recipients(&self) -> BTreeSet<Recipient> {
        let membership = self.membership_ref();
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

    /// Whether `recipient` belongs to a Mac that was removed and not restored in this log
    /// ([`super::Membership::recipient_removed`]).
    #[must_use]
    pub fn recipient_is_removed(&self, recipient: &Recipient) -> bool {
        self.membership_ref().recipient_removed(recipient)
    }

    /// Whether an effective [`Body::Wrap`] in this log is addressed to `recipient`.
    #[must_use]
    pub fn wraps_to(&self, recipient: &Recipient) -> bool {
        let effective = self.effective();
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
        let verdict = self
            .epochs()
            .wraps_newest_to(self, target_audience, recipient);
        #[cfg(test)]
        super::epochs::oracle::check("wraps_maximal_audience_to", verdict, || {
            super::epochs::oracle::wraps_maximal_audience_to(self, target_audience, recipient)
        });
        verdict
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
        let effective = self.effective();
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

    /// Unwrap every keyed epoch (F21 rule 2) of an audience `wanted` names whose key reaches
    /// `identity`, keyed by `epoch` [`RecordId`]: through a counting [`Body::Wrap`] addressed to
    /// `identity` first, whose failure is an error as in [`Log::unwrap_epochs`], else through a
    /// non-counting one that lies in the past of one of the epoch's dependents, where a `Wrap`
    /// that fails to open, or whose header, generation or commitment does not match, is skipped.
    /// Empty when `identity` has been removed or no epoch is keyed.
    ///
    /// # Errors
    /// A counting [`Body::Wrap`] of a keyed epoch addressed to `identity` fails decryption or
    /// its epoch commitment check.
    fn unwrap_keyed_epochs(
        &self,
        identity: &Identity,
        effective: &eval::Effective,
        wanted: impl Fn(&EpochAudience) -> bool,
    ) -> Result<BTreeMap<RecordId, Zeroizing<[u8; 32]>>> {
        let mut keys: BTreeMap<RecordId, Zeroizing<[u8; 32]>> = BTreeMap::new();
        let mut epochs: BTreeMap<RecordId, (&EpochAudience, u64, &[u8; 32])> = BTreeMap::new();
        for (i, record) in self.order.iter().enumerate().skip(1) {
            if let Body::Epoch {
                audience,
                generation,
                commitment,
            } = record.body()
                && effective.keyed(i)
                && wanted(audience)
            {
                epochs.insert(*record.id(), (audience, *generation, commitment));
            }
        }
        let my_recipient = identity.recipient();
        if epochs.is_empty() || self.recipient_is_removed(&my_recipient) {
            return Ok(keys);
        }
        for counting in [true, false] {
            for (i, record) in self.order.iter().enumerate().skip(1) {
                if effective.counts(i) != counting || !effective.opens_wrap(i) {
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
                if *recipient != my_recipient || keys.contains_key(epoch) {
                    continue;
                }
                let Some(&(audience, generation, commitment)) = epochs.get(epoch) else {
                    continue;
                };
                let opened = open_wrap(
                    identity,
                    self.network(),
                    epoch,
                    audience,
                    generation,
                    commitment,
                    capsule,
                );
                let key = if counting {
                    opened?
                } else {
                    opened.ok().flatten()
                };
                if let Some(key) = key {
                    keys.insert(*epoch, key);
                }
            }
        }
        Ok(keys)
    }

    /// The key that opens the counting `Put` or `Delete` at `index`, sealed under `epoch`: a
    /// counting epoch's from `unwrapped`, or, when `index` is one of the keyed `epoch`'s
    /// dependents (F21 rule 1), that epoch's from `keyed`. A counting write under a keyed epoch
    /// that is no dependent (one signed once its signer held the removal) opens for nobody.
    fn op_key<'k>(
        &self,
        index: usize,
        epoch: &RecordId,
        unwrapped: &'k BTreeMap<RecordId, Zeroizing<[u8; 32]>>,
        keyed: &'k BTreeMap<RecordId, Zeroizing<[u8; 32]>>,
        effective: &eval::Effective,
    ) -> Option<&'k Zeroizing<[u8; 32]>> {
        if let Some(key) = unwrapped.get(epoch) {
            return Some(key);
        }
        let at = *self.held.get(epoch)?;
        if effective.dependents(at).binary_search(&index).is_ok() {
            keyed.get(epoch)
        } else {
            None
        }
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
        let membership = self.membership_ref();
        if self.recipient_is_removed(&my_recipient)
            || self.audience_left_out_recipient(&EpochAudience::Commons, &my_recipient)
            || self.audience_left_out_recipient(&EpochAudience::Everyone, &my_recipient)
            || !membership.members().iter().any(|(actor, m)| {
                m.binding.recipient() == my_recipient
                    && !membership.member_node_removed(m)
                    && !self.audience_left_out(&EpochAudience::Commons, actor)
                    && !self.audience_left_out(&EpochAudience::Everyone, actor)
            })
        {
            return Ok(None);
        }
        let effective = self.effective();
        let maximal: Vec<(u64, RecordId, [u8; 32])> = self
            .epochs()
            .newest(&EpochAudience::Commons)
            .iter()
            .filter_map(|&at| match self.order[at].body() {
                Body::Epoch {
                    generation,
                    commitment,
                    ..
                } => Some((*generation, *self.order[at].id(), *commitment)),
                _ => None,
            })
            .collect();
        let Some(&(generation, basis, commitment)) = maximal
            .iter()
            .min_by_key(|&&(g, id, _)| (std::cmp::Reverse(g), id))
        else {
            return Ok(None);
        };
        let mut unwrapped_basis: Option<Zeroizing<[u8; 32]>> = None;
        let due = self.table_basis_due(&basis);
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
            due,
        }))
    }

    /// Whether a table writer waits on the epoch `basis` (a Commons epoch, or the fold's
    /// `[everyone]` epoch): P1's one rule ([`Log::head_due`]), as the vault's writer asks it, over
    /// the recipients its counting `Wrap`s here name. The gate reads no `Wrap`: a Mac that lacks
    /// the `Wrap` to a Mac a `Remove` outside `basis`'s past took out (still arriving, or a heal
    /// by a Mac that holds no `Remove`) seals nothing under it that Mac could open (one-mesh
    /// proofs P1b).
    pub(super) fn table_basis_due(&self, basis: &RecordId) -> bool {
        let effective = self.effective();
        let wrapped: BTreeSet<Recipient> = self
            .order
            .iter()
            .enumerate()
            .skip(1)
            .filter_map(|(i, record)| match record.body() {
                Body::Wrap {
                    epoch, recipient, ..
                } if effective.counts(i) && epoch == basis => Some(*recipient),
                _ => None,
            })
            .collect();
        let wraps_removed = wrapped.iter().any(|r| self.recipient_is_removed(r));
        // NEGATIVE CONTROL `table-held-wrap`: the held `Wrap`s alone, as a table writer read them
        // before P1b.
        #[cfg(any(test, debug_assertions))]
        if aterm_types::negative_control::on("table-held-wrap") {
            return wraps_removed;
        }
        wraps_removed || self.head_due(basis, Some(&wrapped), &self.removed_recipients())
    }

    /// Every counting [`EpochAudience::Commons`] epoch, in record order: the log-era sources a
    /// history handoff (`ACHG1`) can carry the key of to a member that holds no `Wrap` of one.
    #[must_use]
    pub fn commons_epoch_ids(&self) -> Vec<RecordId> {
        let effective = self.effective();
        self.order
            .iter()
            .enumerate()
            .skip(1)
            .filter(|&(i, record)| {
                effective.counts(i)
                    && matches!(
                        record.body(),
                        Body::Epoch {
                            audience: EpochAudience::Commons,
                            ..
                        }
                    )
            })
            .map(|(_, record)| *record.id())
            .collect()
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
        let membership = self.membership_ref();
        if self.recipient_is_removed(&my_recipient)
            || self.audience_left_out_recipient(&EpochAudience::Commons, &my_recipient)
            || self.audience_left_out_recipient(&EpochAudience::Everyone, &my_recipient)
            || !membership.members().iter().any(|(actor, m)| {
                m.binding.recipient() == my_recipient
                    && !membership.member_node_removed(m)
                    && !self.audience_left_out(&EpochAudience::Commons, actor)
                    && !self.audience_left_out(&EpochAudience::Everyone, actor)
            })
        {
            return Ok(None);
        }
        let effective = self.effective();
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

    /// Every keyed epoch (F21 rules 1 and 4), in record order: its dependents, who holds its key,
    /// and whether a full reseal covers it. Structural: it reads records and decrypts nothing, so
    /// every Mac that holds the same records reads the same.
    #[must_use]
    pub fn keyed_epochs(&self) -> Vec<Keyed> {
        self.keyed_epochs_in(self.effective())
    }

    /// Decrypt only the winning [`EpochAudience::Commons`] control descriptor for `identity`,
    /// skipping [`EpochAudience::Everyone`] and [`EpochAudience::Hosts`] X25519 unwraps (`F18`).
    /// A winner sealed under a keyed Commons epoch (F21) opens as well.
    ///
    /// # Errors
    /// Any addressed `Commons` [`Body::Wrap`] or [`Body::Put`] fails authentication or validation.
    pub fn open_commons_control(&self, identity: &Identity) -> Result<Option<SecretBytes>> {
        let my_recipient = identity.recipient();
        let membership = self.membership_ref();
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
                    && !membership.member_node_removed(m)
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
        let effective = self.effective();
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
        let keyed = self.unwrap_keyed_epochs(identity, effective, |audience| {
            *audience == EpochAudience::Commons
        })?;
        if unwrapped.is_empty() && keyed.is_empty() {
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
                let Some(epoch_key) = self.op_key(i, epoch, &unwrapped, &keyed, effective) else {
                    continue;
                };
                let plain = open_aead(epoch_key, record, audience, epoch, salt, ciphertext)?;
                let value = decode_commons_put_plaintext(&plain)?;
                candidates.push(CommonsControlOp {
                    index: i,
                    record_id: *record.id(),
                    value,
                });
            }
        }
        Ok(effective
            .newest(&candidates, |op| op.index)
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
    /// validation. Values are validated during shared selection and winning values are opened
    /// again for the result; discarded revisions are not retained as plaintext.
    pub fn open_audience_entries(
        &self,
        identity: &Identity,
    ) -> Result<Option<OpenedAudienceEntries>> {
        self.resolve_audience_entries(identity, value_digest)?
            .map(|resolved| resolved.into_full(self))
            .transpose()
    }

    /// Open selected winners within one exact text audience after the same complete
    /// validation and global host/anchor closure as [`Log::open_audience_entries`].
    /// Unlike [`Log::open_entries`], this neither merges host values over Everyone nor
    /// requires a current epoch key. An unavailable reader is `None`; a visible resolution
    /// without requested winners is `Some(empty)`.
    #[cfg(any(unix, test))]
    pub(crate) fn open_audience_entries_named(
        &self,
        identity: &Identity,
        audience: &Audience,
        names: &[&str],
    ) -> Result<Option<Vec<Entry>>> {
        self.resolve_audience_entries(identity, value_digest)?
            .map(|resolved| resolved.into_audience_named(self, audience, names))
            .transpose()
    }

    /// Whether a member with no envelope locker can follow this log. Validate all eligible
    /// payloads without materializing their values; a concurrent epoch's wrap gap is not removal.
    #[cfg(unix)]
    pub(crate) fn admits_without_locker(&self, identity: &Identity) -> Result<bool> {
        let me = identity.recipient();
        if self.recipient_is_removed(&me)
            || self.audience_left_out_recipient(&EpochAudience::Everyone, &me)
        {
            return Ok(false);
        }
        let membership = self.membership_ref();
        let has_active_seat = membership.members().values().any(|member| {
            member.binding.recipient() == me && !membership.member_node_removed(member)
        });
        let has_unproved_seat = !has_active_seat
            && membership.unproved().iter().any(|founder| {
                founder.recipient == me
                    && !founder.node_key.is_some_and(|node| {
                        node != [0; 32]
                            && matches!(membership.node(&node), super::NodeStanding::Removed(_))
                    })
            });
        if !has_active_seat && !has_unproved_seat {
            return Ok(false);
        }
        let Some(resolved) = self.resolve_audience_entries(identity, value_digest)? else {
            return Ok(false);
        };
        if has_active_seat {
            return Ok(membership.members().iter().any(|(actor, member)| {
                member.binding.recipient() == me
                    && !membership.member_node_removed(member)
                    && !self.left_out(&resolved.summary, &EpochAudience::Everyone, actor)
            }));
        }
        Ok(true)
    }

    /// Open selected names only after the same complete validation, register selection,
    /// host precedence and anchor closure as [`Log::open_entries`]. A readable view with
    /// none of the names is `Some(empty)`; reader or current-key unavailability is `None`.
    #[cfg(any(unix, test))]
    pub(crate) fn open_entries_named(
        &self,
        identity: &Identity,
        names: &[&str],
    ) -> Result<Option<Vec<Entry>>> {
        self.open_entries_named_with_digest(identity, names, value_digest)
    }

    /// Retain semantic operations from the very resolution that selected the
    /// returned values. `None` names selects every visible winner.
    pub(crate) fn open_selection(
        &self,
        identity: &Identity,
        names: Option<&[&str]>,
    ) -> Result<Option<SelectedEntries>> {
        let Some(resolved) = self.resolve_audience_entries(identity, value_digest)? else {
            return Ok(None);
        };
        if resolved.summary.unwrapped_keys.is_empty()
            || self.audience_left_out_recipient(&EpochAudience::Everyone, &identity.recipient())
        {
            return Ok(None);
        }
        Ok(Some(resolved.into_selection(self, names)?))
    }

    #[cfg(any(unix, test))]
    fn open_entries_named_with_digest(
        &self,
        identity: &Identity,
        names: &[&str],
        digest: fn(&[u8]) -> [u8; 32],
    ) -> Result<Option<Vec<Entry>>> {
        let Some(resolved) = self.resolve_audience_entries(identity, digest)? else {
            return Ok(None);
        };
        if resolved.summary.unwrapped_keys.is_empty()
            || self.audience_left_out_recipient(&EpochAudience::Everyone, &identity.recipient())
        {
            return Ok(None);
        }
        Ok(Some(resolved.into_named(self, names)?))
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "DbHostCarry",
            action = "Ordered",
            reason = "The environment chooses causal or concurrent writes across host audiences."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbHostCarry",
            action = "Read",
            project = "aterm_secrets::membership_log::entries::tests::host_carry_transition"
        )
    )]
    fn resolve_audience_entries(
        &self,
        identity: &Identity,
        digest: fn(&[u8]) -> [u8; 32],
    ) -> Result<Option<ResolvedEntries>> {
        let my_recipient = identity.recipient();
        if self.recipient_is_removed(&my_recipient) {
            return Ok(None);
        }
        let unwrapped = self.unwrap_epochs(identity)?;
        let effective = self.effective();
        let keyed_keys = self.unwrap_keyed_epochs(identity, effective, |_| true)?;

        let epochs = self.epochs();
        let max_generation: BTreeMap<EpochAudience, u64> = epochs
            .audiences()
            .iter()
            .map(|(audience, sealed)| (audience.clone(), sealed.generation))
            .collect();
        let wrapped_recipients: BTreeMap<RecordId, BTreeSet<Recipient>> = epochs
            .wraps()
            .iter()
            .map(|(epoch, to)| (*epoch, to.keys().copied().collect()))
            .collect();

        let mut maximal_epochs: BTreeMap<EpochAudience, Vec<RecordId>> = BTreeMap::new();
        let mut fully_unwrapped_audiences = BTreeSet::new();
        let mut excluded_audiences = BTreeSet::new();
        let mut hidden_epochs = false;
        let left_out_everyone =
            self.audience_left_out_recipient(&EpochAudience::Everyone, &my_recipient);
        for (aud, sealed) in epochs.audiences() {
            let max_ids: Vec<RecordId> = sealed
                .newest
                .iter()
                .map(|&at| *self.order[at].id())
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
        // F21 rule 7: a keyed epoch a current member holds and no full reseal has covered yet
        // leaves its audience not fully unwrapped on a Mac that cannot open it, so no full re-put
        // runs there and its overlay leaves the audience's entries alone.
        let mut uncovered_keyed: BTreeMap<EpochAudience, Vec<RecordId>> = BTreeMap::new();
        let mut keyless_keyed: BTreeSet<EpochAudience> = BTreeSet::new();
        for keyed in self.keyed_epochs_in(effective) {
            if !keyed
                .holders
                .iter()
                .any(|h| self.membership_ref().contains(h))
            {
                continue;
            }
            if !keyed_keys.contains_key(&keyed.epoch) {
                keyless_keyed.insert(keyed.audience.clone());
            }
            if !keyed.covered {
                uncovered_keyed
                    .entry(keyed.audience)
                    .or_default()
                    .push(keyed.epoch);
            }
        }
        for (aud, epochs) in &uncovered_keyed {
            if epochs.iter().any(|epoch| !keyed_keys.contains_key(epoch)) {
                fully_unwrapped_audiences.remove(aud);
                if aud != &EpochAudience::Commons {
                    hidden_epochs = true;
                }
            }
        }

        let mut registers: BTreeMap<(Audience, String), Vec<CandidateOp>> = BTreeMap::new();
        let mut commons_candidates: Vec<ControlRef> = Vec::new();
        let mut value_classes = ValueClasses::default();
        let opening = EntryOpening {
            log: self,
            unwrapped: &unwrapped,
            keyed: &keyed_keys,
            effective,
        };
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
                    let Some(epoch_key) = self.op_key(i, epoch, &unwrapped, &keyed_keys, effective)
                    else {
                        continue;
                    };
                    let plain = open_aead(epoch_key, record, audience, epoch, salt, ciphertext)?;
                    if let Some(text_aud) = audience.as_text_audience() {
                        let entry = opening.inner(decode_put_plaintext(&text_aud, &plain)?);
                        let value_class =
                            value_classes.intern(&opening, i, entry.value.as_slice(), digest)?;
                        let entry = EntryMeta {
                            index: i,
                            name: entry.name,
                            if_absent: entry.if_absent,
                            anchor: entry.anchor,
                            value_class,
                        };
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
                        let _ = decode_commons_put_plaintext(&plain)?;
                        commons_candidates.push(ControlRef {
                            index: i,
                            record_id: *record.id(),
                            epoch: *epoch,
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
                    let Some(epoch_key) = self.op_key(i, epoch, &unwrapped, &keyed_keys, effective)
                    else {
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
        let mut by_audience: BTreeMap<Audience, BTreeMap<String, EntryMeta>> = BTreeMap::new();
        let mut winning_epochs: BTreeMap<EpochAudience, BTreeSet<RecordId>> = BTreeMap::new();
        let mut keyed_winning: BTreeMap<EpochAudience, BTreeSet<RecordId>> = BTreeMap::new();
        let mut keyed_deletes: BTreeMap<Audience, BTreeSet<String>> = BTreeMap::new();
        let mut winning_ops: BTreeMap<(Audience, String), RecordId> = BTreeMap::new();
        let mut winning_puts: BTreeMap<(Audience, String), RecordId> = BTreeMap::new();
        let mut winning_deletes: BTreeMap<(Audience, String), RecordId> = BTreeMap::new();
        let mut host_winners: BTreeMap<String, Vec<(Audience, usize, RecordId)>> = BTreeMap::new();

        let registers: Vec<((Audience, String), Vec<CandidateOp>)> =
            registers.into_iter().collect();
        let ops: Vec<&[CandidateOp]> = registers.iter().map(|(_, ops)| ops.as_slice()).collect();
        let (order, held) = (&self.order, &self.held);
        let classified = Register::classify_all(&ops, effective.reach(), |x| {
            order[x].parents().iter().map(move |parent| held[parent])
        });
        for (((audience, name), candidates), register) in registers.iter().zip(&classified) {
            // A carry changes nothing, so it loses to a concurrent sibling that changes the
            // register, a `Delete` included: a reseal on one side of a split never undoes an edit
            // made on the other. Siblings that write the same thing are no conflict.
            let siblings = register.siblings(effective.reach());
            #[cfg(test)]
            register::oracle::check(candidates, effective, register, &siblings);
            let Some(w) = register.winner(&siblings) else {
                continue;
            };
            let winner = &candidates[w];
            if register.conflict(&siblings, w) {
                conflicts.insert((audience.clone(), name.clone()));
            }
            if let DecodedOp::Put(entry) = &winner.op {
                let semantic = &candidates[register.reps[w]];
                if matches!(audience, Audience::Hosts(_)) {
                    let precedence = semantic;
                    #[cfg(test)]
                    let precedence = if HOST_WRAPPER_PRECEDENCE.get() {
                        winner
                    } else {
                        precedence
                    };
                    host_winners.entry(name.clone()).or_default().push((
                        audience.clone(),
                        precedence.index,
                        precedence.record_id,
                    ));
                }
                winning_ops.insert((audience.clone(), name.clone()), semantic.record_id);
                winning_puts.insert((audience.clone(), name.clone()), winner.record_id);
                by_audience
                    .entry(audience.clone())
                    .or_default()
                    .insert(name.clone(), entry.clone());
                // A keyed epoch carries no authority (F21 rule 3): its winners are kept apart.
                let winners = if keyed_keys.contains_key(&winner.epoch) {
                    &mut keyed_winning
                } else {
                    &mut winning_epochs
                };
                winners
                    .entry(EpochAudience::from(audience))
                    .or_default()
                    .insert(winner.epoch);
            } else {
                winning_deletes.insert((audience.clone(), name.clone()), winner.record_id);
                if keyed_keys.contains_key(&winner.epoch) {
                    keyed_deletes
                        .entry(audience.clone())
                        .or_default()
                        .insert(name.clone());
                }
            }
        }

        let mut commons_control = None;
        if !commons_candidates.is_empty() {
            let siblings = effective.newest(&commons_candidates, |c| c.index);
            if let Some(winner) = siblings.into_iter().min_by_key(|c| c.record_id) {
                commons_control = Some(winner.index);
                let winners = if keyed_keys.contains_key(&winner.epoch) {
                    &mut keyed_winning
                } else {
                    &mut winning_epochs
                };
                winners
                    .entry(EpochAudience::Commons)
                    .or_default()
                    .insert(winner.epoch);
            }
        }

        let mut winning_host_audience: BTreeMap<String, Audience> = BTreeMap::new();
        for (name, candidates) in host_winners {
            let maximal = effective.newest(&candidates, |c| c.1);
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
        let live = |(audience, name): &(Audience, String)| {
            by_audience
                .get(audience)
                .is_some_and(|entries| entries.contains_key(name))
        };
        winning_ops.retain(|key, _| live(key));
        winning_puts.retain(|key, _| live(key));

        Ok(Some(ResolvedEntries {
            entries: by_audience,
            commons: commons_control,
            summary: OpenedAudienceEntries {
                by_audience: BTreeMap::new(),
                commons_control: None,
                winning_host_audience,
                winning_epochs,
                keyed_winning,
                maximal_epochs,
                max_generation,
                wrapped_recipients,
                conflicts,
                unwrapped_keys: unwrapped,
                keyed_keys,
                uncovered_keyed,
                keyed_deletes,
                winning_ops,
                winning_puts,
                winning_deletes,
                fully_unwrapped_audiences,
                hidden_epochs,
                keyless_keyed,
            },
        }))
    }

    /// F21 rule 8: the current entries `member`, a Mac the log removed, could read though they
    /// were written while it was out, as `identity` opens them: each whose winning op (for a
    /// carry, the op it carries) another Mac signed while `member` was out (outside the cutoff of
    /// some `Remove` of `member` that counts, with no counting `Admit` of it after that `Remove`
    /// in its past), sealed under an epoch `member` minted or one a `Wrap` hands one of its
    /// recipients. Resealing them does not take back what `member` read, so a person rotates
    /// those values where they came from. Each is its audience and name, never a value; empty
    /// where `member` is not out or `identity` opens nothing.
    ///
    /// # Errors
    /// As [`Log::open_audience_entries`].
    pub fn read_after_cutoff(
        &self,
        identity: &Identity,
        member: &MemberId,
    ) -> Result<Vec<(Audience, String)>> {
        if self.membership_ref().removal(member).is_none() {
            return Ok(Vec::new());
        }
        Ok(self
            .open_audience_entries(identity)?
            .map(|opened| self.read_after_cutoff_in(&opened, member))
            .unwrap_or_default())
    }

    /// [`Log::read_after_cutoff`] over entries already opened: one opening serves every removed
    /// Mac a caller asks about. Empty where `member` is not out.
    #[must_use]
    pub fn read_after_cutoff_in(
        &self,
        opened: &OpenedAudienceEntries,
        member: &MemberId,
    ) -> Vec<(Audience, String)> {
        if self.membership_ref().removal(member).is_none() {
            return Vec::new();
        }
        let effective = self.effective();
        // Each counting `Remove` of `member` with its cutoff, and each counting `Admit` of it.
        let removes: Vec<(usize, usize)> = self
            .order
            .iter()
            .enumerate()
            .filter(|&(i, _)| effective.counts(i))
            .filter_map(|(i, record)| match record.body() {
                Body::Remove {
                    member: out,
                    cutoff,
                } if out == member => self.record_index(cutoff).map(|cut| (i, cut)),
                _ => None,
            })
            .collect();
        let admits: Vec<usize> = (1..self.order.len())
            .filter(|&i| effective.counts(i))
            .filter(|&i| {
                matches!(self.order[i].body(), Body::Admit { entrant, .. } if entrant.member() == *member)
            })
            .collect();
        // `member` was out at record `at` when some `Remove` of it does not reach `at` and no
        // `Admit` of it that follows that `Remove` lies in `at`'s past (as `cut` reads it).
        let while_out = |at: usize| {
            removes.iter().any(|&(r, cut)| {
                !(cut == at || effective.before(at, cut))
                    && !admits
                        .iter()
                        .any(|&a| effective.before(r, a) && effective.before(a, at))
            })
        };
        let mut recipients: BTreeSet<Recipient> = BTreeSet::new();
        if let Body::Genesis { founders, .. } = self.genesis().body() {
            recipients.extend(
                founders
                    .iter()
                    .filter(|f| f.member() == Some(*member))
                    .map(|f| f.recipient),
            );
        }
        for record in self.order.iter() {
            if let Body::Admit { entrant, .. } = record.body()
                && entrant.member() == *member
            {
                recipients.insert(entrant.request.binding().recipient());
            }
        }
        let reaches = |epoch: &RecordId| {
            self.get(epoch).is_some_and(|r| r.actor() == member)
                || self.order.iter().any(|r| {
                    matches!(r.body(), Body::Wrap { epoch: e, recipient, .. }
                        if e == epoch && recipients.contains(recipient))
                })
        };
        opened
            .winning_ops
            .iter()
            .filter(|(_, op)| {
                let Some(at) = self.record_index(op) else {
                    return false;
                };
                let record = &self.order[at];
                let Body::Put { epoch, .. } = record.body() else {
                    return false;
                };
                record.actor() != member && while_out(at) && reaches(epoch)
            })
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Evaluate and decrypt the secret entries visible to `identity` from this log (`§3.5.3`).
    ///
    /// Returns `Ok(None)` when `identity` has been removed or holds no unwrapped epoch in the log.
    ///
    /// # Errors
    /// Any addressed [`Body::Wrap`], [`Body::Put`] or [`Body::Delete`] fails authentication or
    /// validation.
    pub fn open_entry_set(&self, identity: &Identity) -> Result<Option<OpenedEntries>> {
        Ok(self
            .open_selection(identity, None)?
            .map(|selected| selected.opened))
    }

    /// Convenience wrapper over [`Log::open_entry_set`] returning the merged [`Entry`] list.
    ///
    /// # Errors
    /// Propagates errors from [`Log::open_entry_set`].
    pub fn open_entries(&self, identity: &Identity) -> Result<Option<Vec<Entry>>> {
        Ok(self.open_entry_set(identity)?.map(|opened| opened.entries))
    }

    /// The lineage of the `[everyone]` value `value` of `name`, as `identity` opens this log
    /// (DESIGN-claude-setup-token §4.3): every counting `[everyone]` `Put` of `name` whose value,
    /// as this Mac reads it (opened from its inner seal, [`inner::load`]), is `value`, in record
    /// order, and whether a counting `[everyone]` `Put` this Mac does not open comes before the
    /// first of them, so an earlier `Put` of the value may stand where this Mac cannot see it.
    /// `None` where `identity` is removed. Value-free: it returns record ids, never a value.
    ///
    /// # Errors
    /// A `Wrap` or `Put` addressed to `identity` fails to open.
    #[cfg(unix)]
    pub(crate) fn put_lineage(
        &self,
        identity: &Identity,
        name: &str,
        value: &[u8],
    ) -> Result<Option<Lineage>> {
        if self.recipient_is_removed(&identity.recipient()) {
            return Ok(None);
        }
        let unwrapped = self.unwrap_epochs(identity)?;
        let effective = self.effective();
        let keyed = self.unwrap_keyed_epochs(identity, effective, |_| true)?;
        let mut lineage = Lineage::default();
        let mut unopened = false;
        for (i, record) in self.order.iter().enumerate().skip(1) {
            if !effective.counts(i) {
                continue;
            }
            let Body::Put {
                audience: audience @ EpochAudience::Everyone,
                epoch,
                salt,
                ciphertext,
            } = record.body()
            else {
                continue;
            };
            let Some(epoch_key) = self.op_key(i, epoch, &unwrapped, &keyed, effective) else {
                unopened = true;
                continue;
            };
            let plain = open_aead(epoch_key, record, audience, epoch, salt, ciphertext)?;
            let stored = decode_put_plaintext(&Audience::Everyone, &plain)?;
            if stored.name != name {
                continue;
            }
            let sealed_under = inner_sealed(name)
                .then(|| inner::sealing_epoch(stored.value.as_slice()))
                .flatten();
            let read = open_inner(self.network(), &unwrapped, &keyed, stored);
            if !aterm_digest::ct_eq(read.value.as_slice(), value) {
                continue;
            }
            if lineage.puts.is_empty() {
                lineage.unopened_before = unopened;
            }
            lineage.puts.push(LineagePut {
                record: *record.id(),
                actor: *record.actor(),
                epoch: *epoch,
                sealed_under,
            });
        }
        Ok(Some(lineage))
    }

    /// Every recipient a `Wrap` of `epoch` in this log hands its key to, whether or not the log
    /// still counts that `Wrap`: who could open what `epoch` seals, once they held these records.
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn wrapped_to(&self, epoch: &RecordId) -> BTreeSet<Recipient> {
        self.order
            .iter()
            .filter_map(|record| match record.body() {
                Body::Wrap {
                    epoch: of,
                    recipient,
                    ..
                } if of == epoch => Some(*recipient),
                _ => None,
            })
            .collect()
    }
}

/// The `Put`s of one `[everyone]` value as a Mac opens them ([`Log::put_lineage`]).
#[cfg(unix)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Lineage {
    /// Each `Put` of the value, in record order.
    pub(crate) puts: Vec<LineagePut>,
    /// A counting `[everyone]` `Put` this Mac does not open comes before the first of them.
    pub(crate) unopened_before: bool,
}

/// One `Put` of a [`Lineage`].
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LineagePut {
    /// The `Put`.
    pub(crate) record: RecordId,
    /// Who signed it.
    pub(crate) actor: MemberId,
    /// The epoch it is sealed under.
    pub(crate) epoch: RecordId,
    /// The epoch its value's inner seal names, for a Claude Code token entry
    /// ([`inner::sealing_epoch`]): its own epoch where a current build signed it, an older one
    /// where an older build carried the seal on.
    pub(crate) sealed_under: Option<RecordId>,
}

#[cfg(test)]
#[path = "entries/selective_tests.rs"]
mod selective_tests;

#[cfg(test)]
#[path = "entries_tests.rs"]
mod tests;
