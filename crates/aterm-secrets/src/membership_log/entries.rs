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
mod tests {
    use super::*;
    use crate::commons::binding::Request;
    use crate::membership_log::epochs;
    use crate::membership_log::tests::permutations;
    use crate::membership_log::tests::signed::{Mac, generated, three_macs};
    use crate::membership_log::{Entrant, Evidence, Inserted};

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
    fn epoch_visibility_tracks_waiting_wraps_and_new_denials_without_changing_old_clones() {
        let (writer, incarnation, _, actor, first) = make_entrant(1, "m1");
        let (_, _, identity, _, second) = make_entrant(2, "m2");
        let original =
            Log::new(Record::found(&[7; 32], vec![first, second], 1_000).unwrap()).unwrap();
        assert!(!original.has_epochs());
        assert!(!original.has_commons_epochs());
        let mut sender = original.clone();
        let (draft, key) = sender
            .draft_epoch(&actor, 1_001, EpochAudience::Commons, 1)
            .unwrap();
        let epoch = draft.sign(&writer, incarnation).unwrap();
        sender.insert(epoch.clone()).unwrap();
        assert!(sender.has_commons_epochs());
        assert!(sender.audience_left_out_recipient(&EpochAudience::Commons, &identity.recipient()));
        let wrap = sender
            .draft_wrap(&actor, 1_002, *epoch.id(), &key, identity.recipient())
            .unwrap()
            .sign(&writer, incarnation)
            .unwrap();

        let mut receiver = original.clone();
        assert_eq!(receiver.insert(wrap).unwrap(), Inserted::Waiting);
        assert!(!receiver.has_epochs());
        assert_eq!(receiver.insert(epoch).unwrap(), Inserted::Held);
        assert_eq!(receiver.waiting(), 0);
        assert!(receiver.has_commons_epochs());
        assert!(receiver.wraps_to(&identity.recipient()));
        assert!(
            !receiver.audience_left_out_recipient(&EpochAudience::Commons, &identity.recipient())
        );
        assert!(!original.has_epochs());

        let mut rotated = receiver.clone();
        let (draft, _) = rotated
            .draft_epoch(&actor, 1_003, EpochAudience::Commons, 2)
            .unwrap();
        rotated
            .insert(draft.sign(&writer, incarnation).unwrap())
            .unwrap();
        assert!(
            rotated.audience_left_out_recipient(&EpochAudience::Commons, &identity.recipient())
        );
        assert!(
            !receiver.audience_left_out_recipient(&EpochAudience::Commons, &identity.recipient())
        );
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
        assert_eq!(
            log.open_audience_entries_named(&id2, &Audience::Everyone, &["api-key"])
                .expect("removed m2 audience"),
            None
        );
    }

    /// A three-member log (m1 founds, admits m2 and m9) whose `[everyone]` epoch is wrapped to all
    /// three. It keeps m9's keys, so a test can sign as m9 on its side of a split.
    struct Three {
        log: Log,
        w1: [u8; 32],
        inc1: [u8; 16],
        m1: MemberId,
        w2: [u8; 32],
        inc2: [u8; 16],
        m2: MemberId,
        w9: [u8; 32],
        inc9: [u8; 16],
        m9: MemberId,
        id1: Identity,
        id2: Identity,
        id9: Identity,
        /// m9's `Admit`: the cutoff of a removal of m9 that accepted none of its records.
        m9_entry: RecordId,
        epoch: RecordId,
        key: Zeroizing<[u8; 32]>,
    }

    fn three() -> Three {
        let (w1, inc1, id1, m1, e1) = make_entrant(51, "m1");
        let (w2, inc2, id2, m2, e2) = make_entrant(52, "m2");
        let (w9, inc9, id9, m9, e9) = make_entrant(59, "m9");
        let mut log = Log::new(Record::found(&[7; 32], vec![e1], 1_000).unwrap()).unwrap();
        let mut m9_entry = *log.network();
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
            m9_entry = *admit.id();
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
            w9,
            inc9,
            m9,
            id1,
            id2,
            id9,
            m9_entry,
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
    /// NEGATIVE CONTROL: let a carry replace every op in its past (in `Register::siblings`, OR
    /// the carries' rows into `writers` too): the reseal settles the conflict, and nobody is told.
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

    /// What `id` opens: each entry's audience, name and value, and the conflicts.
    type Opened = (
        Vec<(Audience, String, Vec<u8>)>,
        BTreeSet<(Audience, String)>,
    );

    fn opened(log: &Log, id: &Identity) -> Opened {
        let set = log.open_entry_set(id).unwrap().unwrap();
        let mut entries: Vec<_> = set
            .entries
            .iter()
            .map(|e| {
                (
                    e.audience.clone(),
                    e.name.clone(),
                    e.value.as_slice().to_vec(),
                )
            })
            .collect();
        entries.sort();
        (entries, set.conflicts)
    }

    /// Law 5 on signed logs: three Macs write `[everyone]` entries beside each other (m1 reseals
    /// `k1` and `k2` under a fresh epoch, m2 edits `k1` and deletes `k2`, m9 edits `k1` and adds
    /// `k3`, both under the old epoch, each Mac's second record on its first), and a Mac
    /// receives the six records in each of the 720 orders (a record whose parent has not come
    /// yet waits, so the log holds them in an order the arrival chose). Every order reads the
    /// same entries and conflicts, for m1 and for m2: `k1` the conflict of the two edits, the
    /// lower record id's value, its reseal taken out; `k2` gone, its reseal taken out; `k3`.
    /// NEGATIVE CONTROL `entries-arrival-order` (the first sibling a Mac's log holds wins): two
    /// orders read `k1` apart, so the arrival order reaches the register and the test sees it.
    #[test]
    fn every_arrival_order_of_concurrent_writes_reads_the_same_entries() {
        let t = three();
        let mut pre = t.log.clone();
        for entry in [put("k1", b"v0"), put("k2", b"w0")] {
            let rec = pre
                .draft_put(&t.m1, 1_500, t.epoch, &t.key, &entry)
                .unwrap()
                .sign(&t.w1, t.inc1)
                .unwrap();
            pre.insert(rec).unwrap();
        }
        // m9 departs: m1's fresh epoch and its wraps to m1 and m2 are held first, the carries of
        // the reseal arrive with the edits.
        let mut base = pre.clone();
        let (draft, key) = base
            .draft_epoch(&t.m1, 2_000, Audience::Everyone, 2)
            .unwrap();
        let ep = draft.sign(&t.w1, t.inc1).unwrap();
        let fresh = *ep.id();
        base.insert(ep).unwrap();
        for id in [&t.id1, &t.id2] {
            let wrap = base
                .draft_wrap(&t.m1, 2_001, fresh, &key, id.recipient())
                .unwrap()
                .sign(&t.w1, t.inc1)
                .unwrap();
            base.insert(wrap).unwrap();
        }
        let mut records = Vec::new();
        let mut m1 = base.clone();
        for entry in [put("k1", b"v0"), put("k2", b"w0")] {
            let rec = m1
                .draft_put(&t.m1, 2_002, fresh, &key, &entry)
                .unwrap()
                .sign(&t.w1, t.inc1)
                .unwrap();
            m1.insert(rec.clone()).unwrap();
            records.push(rec);
        }
        let mut m2 = pre.clone();
        let edit = m2
            .draft_put(&t.m2, 1_600, t.epoch, &t.key, &put("k1", b"v1-m2"))
            .unwrap()
            .sign(&t.w2, t.inc2)
            .unwrap();
        m2.insert(edit.clone()).unwrap();
        let delete = m2
            .draft_delete(&t.m2, 1_601, t.epoch, &t.key, "k2")
            .unwrap()
            .sign(&t.w2, t.inc2)
            .unwrap();
        let mut m9 = pre.clone();
        let other = m9
            .draft_put(&t.m9, 1_600, t.epoch, &t.key, &put("k1", b"v1-m9"))
            .unwrap()
            .sign(&t.w9, t.inc9)
            .unwrap();
        m9.insert(other.clone()).unwrap();
        let added = m9
            .draft_put(&t.m9, 1_601, t.epoch, &t.key, &put("k3", b"z"))
            .unwrap()
            .sign(&t.w9, t.inc9)
            .unwrap();
        let lower: &[u8] = if edit.id() < other.id() {
            b"v1-m2"
        } else {
            b"v1-m9"
        };
        records.extend([edit, delete, other, added]);

        let arrive = |order: &[usize]| {
            let mut log = base.clone();
            for &i in order {
                log.insert(records[i].clone()).unwrap();
            }
            [opened(&log, &t.id1), opened(&log, &t.id2)]
        };
        let orders = permutations(records.len());
        assert_eq!(orders.len(), 720);
        let want = arrive(&orders[0]);
        let k = |name: &str| (Audience::Everyone, name.to_owned());
        for (entries, conflicts) in &want {
            assert_eq!(
                *conflicts,
                BTreeSet::from([k("k1")]),
                "k1 alone is a conflict"
            );
            let names: Vec<(&str, &[u8])> = entries
                .iter()
                .map(|(audience, name, value)| {
                    assert_eq!(*audience, Audience::Everyone);
                    (name.as_str(), value.as_slice())
                })
                .collect();
            assert_eq!(names, [("k1", lower), ("k3", b"z".as_slice())]);
        }
        for order in &orders {
            assert!(
                arrive(order) == want,
                "arrival order {order:?} reads otherwise"
            );
        }

        let read_apart = reading_arrival_order(|| {
            let first = arrive(&orders[0]);
            orders.iter().any(|order| arrive(order) != first)
        });
        assert!(read_apart, "the first sibling seen winning goes unnoticed");
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

    /// The audiences `member` reseals under F21 rule 5 ([`Log::rekey_dues`]).
    fn reseal_due_on(log: &Log, member: &MemberId) -> BTreeSet<EpochAudience> {
        log.rekey_dues()
            .into_iter()
            .filter(|due| due.reseal && due.resealers.contains(member))
            .map(|due| due.audience)
            .collect()
    }

    /// Sign `draft` with (`seed`, `incarnation`), hold it in `log`, and hand back its id.
    fn hold(log: &mut Log, draft: Draft, seed: &[u8; 32], incarnation: [u8; 16]) -> RecordId {
        let record = draft.sign(seed, incarnation).unwrap();
        let id = *record.id();
        log.insert(record).unwrap();
        id
    }

    /// F21's split: `S=v0` and `U=u0` under the first epoch; on side 1, m1 removes m9 at its
    /// `Admit` and reseals both for m1 and m2; on side 2, m9, unaware, mints `E` (generation 2),
    /// wraps it to all three, carries `S` and `U` under it and puts `W=evil`. With `bogus`, m9
    /// first wraps `E` to m2 under a wrong key.
    struct KeyedSplit {
        side_1: Log,
        side_2: Log,
        /// `E`, the epoch m9 minted past its cutoff.
        epoch: RecordId,
        /// `E`'s key, which m2 holds on side 2 through m9's `Wrap`.
        key: Zeroizing<[u8; 32]>,
    }

    fn keyed_split(t: &Three, bogus: bool) -> KeyedSplit {
        let mut base = t.log.clone();
        for entry in [put("S", b"v0"), put("U", b"u0")] {
            let draft = base
                .draft_put(&t.m1, 1_500, t.epoch, &t.key, &entry)
                .unwrap();
            hold(&mut base, draft, &t.w1, t.inc1);
        }
        let mut side_1 = base.clone();
        let removal = side_1.draft(
            &t.m1,
            1_600,
            Body::Remove {
                member: t.m9,
                cutoff: t.m9_entry,
            },
        );
        hold(&mut side_1, removal, &t.w1, t.inc1);
        reseal(&mut side_1, t, 2, &[put("S", b"v0"), put("U", b"u0")]);
        let mut side_2 = base;
        let (draft, key) = side_2
            .draft_epoch(&t.m9, 1_600, Audience::Everyone, 2)
            .unwrap();
        let epoch = hold(&mut side_2, draft, &t.w9, t.inc9);
        if bogus {
            let capsule =
                seal_wrap(side_2.network(), &epoch, 2, t.id2.recipient(), &[0x99; 32]).unwrap();
            let wrap = side_2.draft(
                &t.m9,
                1_601,
                Body::Wrap {
                    epoch,
                    recipient: t.id2.recipient(),
                    capsule: capsule.into(),
                },
            );
            hold(&mut side_2, wrap, &t.w9, t.inc9);
        }
        for id in [&t.id1, &t.id2, &t.id9] {
            let wrap = side_2
                .draft_wrap(&t.m9, 1_601, epoch, &key, id.recipient())
                .unwrap();
            hold(&mut side_2, wrap, &t.w9, t.inc9);
        }
        for entry in [put("S", b"v0"), put("U", b"u0"), put("W", b"evil")] {
            let draft = side_2.draft_put(&t.m9, 1_602, epoch, &key, &entry).unwrap();
            hold(&mut side_2, draft, &t.w9, t.inc9);
        }
        KeyedSplit {
            side_1,
            side_2,
            epoch,
            key,
        }
    }

    /// m2, on m9's side of the split, edits `S`, adds `T` and deletes `U` under `E`.
    fn m2_writes_under_e(t: &Three, split: &mut KeyedSplit) {
        let log = &mut split.side_2;
        let draft = log
            .draft_put(&t.m2, 1_700, split.epoch, &split.key, &put("S", b"v1"))
            .unwrap();
        hold(log, draft, &t.w2, t.inc2);
        let draft = log
            .draft_put(&t.m2, 1_701, split.epoch, &split.key, &put("T", b"t"))
            .unwrap();
        hold(log, draft, &t.w2, t.inc2);
        let draft = log
            .draft_delete(&t.m2, 1_702, split.epoch, &split.key, "U")
            .unwrap();
        hold(log, draft, &t.w2, t.inc2);
    }

    /// F21's test 1. m9 is removed on one side of a split while, on the other, it mints an epoch
    /// past its cutoff and m2, unaware, edits, adds and deletes under it. After the heal those
    /// writes stand on every Mac, m9's own `Put` reads nowhere, and `E` carries no authority: it
    /// is neither newest nor winning, only keyed-winning.
    ///
    /// NEGATIVE CONTROL: with `Log::unwrap_keyed_epochs` opening nothing (origin/main's reading,
    /// where a cut epoch opens for nobody) this fails "S on m1" (left `v0`, right `v1`); tests 2
    /// and 11 and the late-write test fail with it, while guards 10 and 12 pass.
    #[test]
    fn an_edit_add_and_delete_under_an_epoch_minted_past_its_minters_cutoff_stand_after_the_heal() {
        let t = three();
        let mut split = keyed_split(&t, false);
        m2_writes_under_e(&t, &mut split);
        let mut healed = split.side_1.clone();
        healed.merge(&split.side_2).unwrap();
        for (who, id) in [("m1", &t.id1), ("m2", &t.id2)] {
            assert_eq!(read(&healed, id, "S"), Some(b"v1".to_vec()), "S on {who}");
            assert_eq!(read(&healed, id, "T"), Some(b"t".to_vec()), "T on {who}");
            assert_eq!(read(&healed, id, "U"), None, "U on {who}");
            assert_eq!(read(&healed, id, "W"), None, "W on {who}");
            let opened = healed.open_audience_entries(id).unwrap().unwrap();
            assert!(opened.conflicts.is_empty(), "conflicts on {who}");
            assert!(!opened.maximal_epochs[&EpochAudience::Everyone].contains(&split.epoch));
            assert!(
                !opened
                    .winning_epochs
                    .get(&EpochAudience::Everyone)
                    .is_some_and(|set| set.contains(&split.epoch))
            );
            assert!(opened.keyed_winning[&EpochAudience::Everyone].contains(&split.epoch));
            assert!(opened.keyed_keys.contains_key(&split.epoch));
            assert!(!opened.unwrapped_keys.contains_key(&split.epoch));
        }
    }

    /// F21's attack 3, whose premise the evaluator does not hold: a Mac m9 lets in past its
    /// cutoff signs records that count where their past leaves m9's removal out. On test 1's
    /// split, m9 also admits S on side 2, wraps `E` to it, and S puts `X` under `E`. After the
    /// heal S's `Put` is no dependent of `E`: S is seated only by m9's `Admit`, which is cut, so
    /// `E` is not keyed and no Mac reads `X`. Otherwise a removed Mac could make its own epochs
    /// keyed at will through Macs it lets in, forcing a full reseal each time.
    ///
    /// NEGATIVE CONTROL: with `Graph::depends` not asking how `d`'s signer is seated, this fails
    /// "a Mac m9 let in past its cutoff adds no dependent".
    #[test]
    fn a_mac_the_removed_minter_let_in_past_its_cutoff_adds_no_dependent() {
        let t = three();
        let mut split = keyed_split(&t, false);
        let (ws, incs, ids, ms, es) = make_entrant(73, "m13");
        let side_2 = &mut split.side_2;
        let admit = side_2.draft(
            &t.m9,
            1_650,
            Body::Admit {
                entrant: es,
                evidence: Evidence::Phrase,
            },
        );
        hold(side_2, admit, &t.w9, t.inc9);
        let wrap = side_2
            .draft_wrap(&t.m9, 1_651, split.epoch, &split.key, ids.recipient())
            .unwrap();
        hold(side_2, wrap, &t.w9, t.inc9);
        let draft = side_2
            .draft_put(&ms, 1_652, split.epoch, &split.key, &put("X", b"sybil"))
            .unwrap();
        hold(side_2, draft, &ws, incs);
        let mut healed = split.side_1.clone();
        healed.merge(&split.side_2).unwrap();
        assert!(
            healed.keyed_epochs().is_empty(),
            "a Mac m9 let in past its cutoff adds no dependent"
        );
        for (who, id) in [("m1", &t.id1), ("m2", &t.id2)] {
            assert_eq!(read(&healed, id, "X"), None, "X on {who}");
        }
    }

    /// F21's rule 8 with two removals of one Mac: m1 removes m9 at its `Admit`; m2 then puts
    /// `S=v1` under the first epoch, which m9 holds; m9 signs a `Name` with that write in its
    /// past, and m2 removes m9 again at that `Name`. `S=v1` lies inside the second cutoff but
    /// m9 was out from the first, with no `Admit` of it since, so it is named.
    ///
    /// NEGATIVE CONTROL: excluding every write inside any cutoff (step 10's rule) fails "a write
    /// made while m9 was out, inside a later cutoff, is named".
    #[test]
    fn read_after_cutoff_names_a_write_inside_a_later_cutoff_made_while_the_mac_was_out() {
        let t = three();
        let mut log = t.log.clone();
        let draft = log
            .draft_put(&t.m1, 1_500, t.epoch, &t.key, &put("S", b"v0"))
            .unwrap();
        hold(&mut log, draft, &t.w1, t.inc1);
        let removal = log.draft(
            &t.m1,
            1_600,
            Body::Remove {
                member: t.m9,
                cutoff: t.m9_entry,
            },
        );
        hold(&mut log, removal, &t.w1, t.inc1);
        let draft = log
            .draft_put(&t.m2, 1_700, t.epoch, &t.key, &put("S", b"v1"))
            .unwrap();
        hold(&mut log, draft, &t.w2, t.inc2);
        let name = log.draft(
            &t.m9,
            1_800,
            Body::Name {
                name: "m9-later".to_owned(),
            },
        );
        let later = hold(&mut log, name, &t.w9, t.inc9);
        let removal = log.draft(
            &t.m2,
            1_900,
            Body::Remove {
                member: t.m9,
                cutoff: later,
            },
        );
        hold(&mut log, removal, &t.w2, t.inc2);
        for id in [&t.id1, &t.id2] {
            let opened = log.open_audience_entries(id).unwrap().unwrap();
            assert_eq!(
                log.read_after_cutoff_in(&opened, &t.m9),
                [(Audience::Everyone, "S".to_owned())],
                "a write made while m9 was out, inside a later cutoff, is named"
            );
        }
    }

    /// F21's rule 2 opens a keyed epoch through a non-counting `Wrap` only when that `Wrap` lies
    /// in the past of one of its dependents. On test 1's split with X, whom m1 lets in on side 1,
    /// m9 wraps `E` to X after the heal. That `Wrap` makes X no holder, and X opens no key
    /// through it: a removed Mac cannot add holders (or rank them) after the heal (attacks 4 and
    /// 5).
    ///
    /// NEGATIVE CONTROL: with `Graph::usable_wraps` dropping its "in a dependent's past" clause,
    /// this fails "a Wrap of E after every dependent makes no holder".
    #[test]
    fn a_wrap_the_removed_minter_signs_after_the_heal_opens_nothing() {
        let t = three();
        let mut split = keyed_split(&t, false);
        let (_, _, idx, mx, ex) = make_entrant(74, "m14");
        let side_1 = &mut split.side_1;
        let admit = side_1.draft(
            &t.m1,
            1_650,
            Body::Admit {
                entrant: ex,
                evidence: Evidence::Phrase,
            },
        );
        hold(side_1, admit, &t.w1, t.inc1);
        let held = side_1.open_audience_entries(&t.id1).unwrap().unwrap();
        let e1 = held.maximal_epochs[&EpochAudience::Everyone][0];
        let wrap = side_1
            .draft_wrap(&t.m1, 1_651, e1, &held.unwrapped_keys[&e1], idx.recipient())
            .unwrap();
        hold(side_1, wrap, &t.w1, t.inc1);
        m2_writes_under_e(&t, &mut split);
        let mut healed = split.side_1.clone();
        healed.merge(&split.side_2).unwrap();
        let late = healed
            .draft_wrap(&t.m9, 2_000, split.epoch, &split.key, idx.recipient())
            .unwrap();
        hold(&mut healed, late, &t.w9, t.inc9);
        let keyed = healed.keyed_epochs();
        assert_eq!(keyed.len(), 1, "CONTROL: E is keyed");
        assert!(
            !keyed[0].holders.contains(&mx),
            "a Wrap of E after every dependent makes no holder"
        );
        let x_view = healed.open_audience_entries(&idx).unwrap().unwrap();
        assert!(
            !x_view.keyed_keys.contains_key(&split.epoch),
            "X opens no key through a Wrap no dependent saw"
        );
        assert!(x_view.hidden_epochs, "X still waits for a holder's reseal");
    }

    /// F21's test 1 for Commons (rules 2 and 3 on the control descriptor). The control `c0` sits
    /// under m1's Commons epoch; on side 1 m1 removes m9 and mints a Commons epoch for m1 and m2;
    /// on side 2 m9, unaware, mints `C` for all three and m2 writes the control `v1` under it.
    /// After the heal m1 and m2 read `v1` (through `open_commons_control` and
    /// `open_audience_entries` alike), and `C` carries no authority: it is keyed-winning, never
    /// winning.
    ///
    /// NEGATIVE CONTROLS, each restored with cp from a backup plus touch and confirmed with cmp:
    /// - `open_commons_control` unwrapping no keyed epoch: fails "the control on m1 opens".
    /// - the Commons control winner under a keyed epoch going to `winning_epochs`: fails "a
    ///   keyed Commons epoch carries no authority on m1".
    #[test]
    fn a_commons_control_under_an_epoch_minted_past_its_minters_cutoff_stands_after_the_heal() {
        let t = three();
        let mut base = t.log.clone();
        let (draft, key0) = base
            .draft_epoch(&t.m1, 1_400, EpochAudience::Commons, 1)
            .unwrap();
        let c0 = hold(&mut base, draft, &t.w1, t.inc1);
        for id in [&t.id1, &t.id2, &t.id9] {
            let wrap = base
                .draft_wrap(&t.m1, 1_401, c0, &key0, id.recipient())
                .unwrap();
            hold(&mut base, wrap, &t.w1, t.inc1);
        }
        let put = base
            .draft_commons_put(&t.m1, 1_402, c0, &key0, b"ACD1-c0")
            .unwrap();
        hold(&mut base, put, &t.w1, t.inc1);
        let mut side_1 = base.clone();
        let removal = side_1.draft(
            &t.m1,
            1_600,
            Body::Remove {
                member: t.m9,
                cutoff: t.m9_entry,
            },
        );
        hold(&mut side_1, removal, &t.w1, t.inc1);
        let (draft, key1) = side_1
            .draft_epoch(&t.m1, 1_601, EpochAudience::Commons, 2)
            .unwrap();
        let c1 = hold(&mut side_1, draft, &t.w1, t.inc1);
        for id in [&t.id1, &t.id2] {
            let wrap = side_1
                .draft_wrap(&t.m1, 1_602, c1, &key1, id.recipient())
                .unwrap();
            hold(&mut side_1, wrap, &t.w1, t.inc1);
        }
        let mut side_2 = base;
        let (draft, key) = side_2
            .draft_epoch(&t.m9, 1_600, EpochAudience::Commons, 2)
            .unwrap();
        let epoch = hold(&mut side_2, draft, &t.w9, t.inc9);
        for id in [&t.id1, &t.id2, &t.id9] {
            let wrap = side_2
                .draft_wrap(&t.m9, 1_601, epoch, &key, id.recipient())
                .unwrap();
            hold(&mut side_2, wrap, &t.w9, t.inc9);
        }
        let put = side_2
            .draft_commons_put(&t.m2, 1_700, epoch, &key, b"ACD1-v1")
            .unwrap();
        hold(&mut side_2, put, &t.w2, t.inc2);
        let mut healed = side_1;
        healed.merge(&side_2).unwrap();
        for (who, id) in [("m1", &t.id1), ("m2", &t.id2)] {
            assert_eq!(
                healed
                    .open_commons_control(id)
                    .unwrap()
                    .map(|v| v.as_slice().to_vec()),
                Some(b"ACD1-v1".to_vec()),
                "the control on {who} opens"
            );
            let opened = healed.open_audience_entries(id).unwrap().unwrap();
            assert_eq!(
                opened.commons_control.map(|v| v.as_slice().to_vec()),
                Some(b"ACD1-v1".to_vec()),
                "the control on {who}"
            );
            assert!(
                !opened
                    .winning_epochs
                    .get(&EpochAudience::Commons)
                    .is_some_and(|set| set.contains(&epoch)),
                "a keyed Commons epoch carries no authority on {who}"
            );
            assert!(opened.keyed_winning[&EpochAudience::Commons].contains(&epoch));
            assert!(!opened.maximal_epochs[&EpochAudience::Commons].contains(&epoch));
        }
    }

    /// F21 rule 5 for a host list: m9, unaware of its removal on side 1, mints an epoch of a
    /// list it is not on, wrapped to the list's members m1 and m2, and m2 puts `H=h1` under it.
    /// After the heal the list's one newest epoch is m1's first, wrapped to no removed Mac, so
    /// §3.3.7's coverage rule has nothing to do there; the keyed epoch alone makes the list due
    /// on m2, its holder, which reads `h1`.
    ///
    /// NEGATIVE CONTROL: `Log::rekey_dues` without rule 5's reason (`rotation || coverage ||
    /// reseal` cut to `rotation || coverage`): fails "the list is due on its holder".
    #[test]
    fn a_host_list_with_an_uncovered_keyed_epoch_is_due_on_its_holder() {
        let t = three();
        let hosts = vec!["host-1".to_owned(), "host-2".to_owned()];
        let entry = |value: &[u8]| Entry {
            name: "H".to_owned(),
            value: SecretBytes::from(value),
            audience: Audience::Hosts(hosts.clone()),
            if_absent: false,
            anchor: None,
        };
        let mut base = t.log.clone();
        let (draft, key0) = base
            .draft_epoch(&t.m1, 1_400, EpochAudience::Hosts(hosts.clone()), 1)
            .unwrap();
        let h0 = hold(&mut base, draft, &t.w1, t.inc1);
        for id in [&t.id1, &t.id2] {
            let wrap = base
                .draft_wrap(&t.m1, 1_401, h0, &key0, id.recipient())
                .unwrap();
            hold(&mut base, wrap, &t.w1, t.inc1);
        }
        let put = base
            .draft_put(&t.m1, 1_402, h0, &key0, &entry(b"h0"))
            .unwrap();
        hold(&mut base, put, &t.w1, t.inc1);
        let mut side_1 = base.clone();
        let removal = side_1.draft(
            &t.m1,
            1_600,
            Body::Remove {
                member: t.m9,
                cutoff: t.m9_entry,
            },
        );
        hold(&mut side_1, removal, &t.w1, t.inc1);
        let mut side_2 = base;
        let (draft, key) = side_2
            .draft_epoch(&t.m9, 1_600, EpochAudience::Hosts(hosts.clone()), 2)
            .unwrap();
        let epoch = hold(&mut side_2, draft, &t.w9, t.inc9);
        for id in [&t.id1, &t.id2] {
            let wrap = side_2
                .draft_wrap(&t.m9, 1_601, epoch, &key, id.recipient())
                .unwrap();
            hold(&mut side_2, wrap, &t.w9, t.inc9);
        }
        let put = side_2
            .draft_put(&t.m2, 1_700, epoch, &key, &entry(b"h1"))
            .unwrap();
        hold(&mut side_2, put, &t.w2, t.inc2);
        let mut healed = side_1;
        healed.merge(&side_2).unwrap();
        assert!(
            healed
                .keyed_epochs()
                .iter()
                .any(|k| k.epoch == epoch && !k.covered),
            "CONTROL: the list's keyed epoch is uncovered"
        );
        assert_eq!(
            reseal_due_on(&healed, &t.m2),
            BTreeSet::from([EpochAudience::Hosts(hosts.clone())]),
            "the list is due on its holder"
        );
        let opened = healed.open_audience_entries(&t.id2).unwrap().unwrap();
        assert_eq!(
            opened.by_audience[&Audience::Hosts(hosts.clone())]["H"]
                .value
                .as_slice(),
            b"h1"
        );
    }

    /// F21's test 9 (rule 8). After test 1's heal, `read_after_cutoff(m9)` names `S` and `T`, the
    /// entries m2 wrote under m9's `E` while m9 was out, on m1 and m2 alike. It names neither
    /// `U`, which m2 deleted, nor `W`, m9's own write, and nothing for m2, which is no removed
    /// Mac. After m1 reseals `S` and `T` under an epoch m9 never got, they are still named (a
    /// carry stands for the op it carries), and `N`, which m1 adds under that epoch, is not.
    ///
    /// NEGATIVE CONTROLS (each restored with cp from a backup plus touch, and confirmed with cmp):
    /// - `winning_ops` takes the winning op itself, not the op a carry stands for: fails "after
    ///   the reseal on m1" (left `[]`, right `["S", "T"]`).
    /// - the epoch clause dropped (`reaches` answers `true`): fails "after the reseal on m1"
    ///   (left `["N", "S", "T"]`, right `["S", "T"]`).
    #[test]
    fn read_after_cutoff_names_what_the_removed_mac_could_read() {
        let t = three();
        let mut split = keyed_split(&t, false);
        m2_writes_under_e(&t, &mut split);
        let mut healed = split.side_1.clone();
        healed.merge(&split.side_2).unwrap();
        let named = |log: &Log, id: &Identity, member: &MemberId| -> Vec<String> {
            log.read_after_cutoff(id, member)
                .unwrap()
                .into_iter()
                .map(|(audience, name)| {
                    assert_eq!(audience, Audience::Everyone);
                    name
                })
                .collect()
        };
        for (who, id) in [("m1", &t.id1), ("m2", &t.id2)] {
            assert_eq!(named(&healed, id, &t.m9), ["S", "T"], "on {who}");
            assert!(named(&healed, id, &t.m2).is_empty(), "m2 is in, on {who}");
        }
        reseal(
            &mut healed,
            &t,
            3,
            &[put("S", b"v1"), put("T", b"t"), put("N", b"n")],
        );
        for (who, id) in [("m1", &t.id1), ("m2", &t.id2)] {
            assert_eq!(read(&healed, id, "S"), Some(b"v1".to_vec()), "S on {who}");
            assert_eq!(read(&healed, id, "N"), Some(b"n".to_vec()), "N on {who}");
            assert_eq!(
                named(&healed, id, &t.m9),
                ["S", "T"],
                "after the reseal on {who}"
            );
        }
    }

    /// F21's test 10. m2 writes under `E` only once it holds m1's removal of m9: that write is
    /// no dependent, so `E` stays unkeyed and every Mac reads the value side 1 resealed.
    ///
    /// NEGATIVE CONTROL: with rule 1's good-faith clause dropped from `Graph::depends` this fails
    /// "S on m1" (left `v2`, right `v0`).
    #[test]
    fn a_write_signed_after_its_signer_held_the_removal_keys_nothing() {
        let t = three();
        let split = keyed_split(&t, false);
        let mut side_2 = split.side_2.clone();
        side_2.merge(&split.side_1).unwrap();
        let late = side_2
            .draft_put(&t.m2, 1_800, split.epoch, &split.key, &put("S", b"v2"))
            .unwrap();
        hold(&mut side_2, late, &t.w2, t.inc2);
        let mut healed = split.side_1.clone();
        healed.merge(&side_2).unwrap();
        for (who, id) in [("m1", &t.id1), ("m2", &t.id2)] {
            assert_eq!(read(&healed, id, "S"), Some(b"v0".to_vec()), "S on {who}");
        }
    }

    /// m2 writes under `E` on m9's side, then holds m1's removal of m9 and writes under `E`
    /// again. The first writes keep `E` keyed, but the late one is no dependent, so it opens for
    /// nobody: registers take a counting write under a keyed epoch only when it depends on it.
    ///
    /// NEGATIVE CONTROL: with `Log::op_key` taking any counting write under a keyed epoch,
    /// dependent or not, this fails "S on m1" (left `v2`, right `v1`).
    #[test]
    fn a_late_write_under_a_keyed_epoch_reads_nowhere() {
        let t = three();
        let mut split = keyed_split(&t, false);
        m2_writes_under_e(&t, &mut split);
        let mut side_2 = split.side_2.clone();
        side_2.merge(&split.side_1).unwrap();
        let late = side_2
            .draft_put(&t.m2, 1_800, split.epoch, &split.key, &put("S", b"v2"))
            .unwrap();
        hold(&mut side_2, late, &t.w2, t.inc2);
        let mut healed = split.side_1.clone();
        healed.merge(&side_2).unwrap();
        for (who, id) in [("m1", &t.id1), ("m2", &t.id2)] {
            assert_eq!(read(&healed, id, "S"), Some(b"v1".to_vec()), "S on {who}");
            assert_eq!(read(&healed, id, "T"), Some(b"t".to_vec()), "T on {who}");
        }
    }

    /// F21's test 11. m9 wraps `E` to m2 under a wrong key before its real `Wrap`, both in the
    /// past of m2's writes, and after the heal it signs a garbage `Wrap` to m2 too. m2's read
    /// skips both non-counting `Wrap`s that fail and opens `E` through the real one.
    ///
    /// NEGATIVE CONTROL: with a failing non-counting `Wrap` treated as an error, as a counting one
    /// is, this fails `m2's read is Ok: Error("membership epoch wrap key does not match epoch
    /// commitment", Other)`.
    #[test]
    fn a_bogus_wrap_from_the_removed_minter_never_fails_a_read() {
        let t = three();
        let mut split = keyed_split(&t, true);
        m2_writes_under_e(&t, &mut split);
        let mut healed = split.side_1.clone();
        healed.merge(&split.side_2).unwrap();
        let garbage = healed.draft(
            &t.m9,
            1_900,
            Body::Wrap {
                epoch: split.epoch,
                recipient: t.id2.recipient(),
                capsule: vec![0x5a; 64].into(),
            },
        );
        hold(&mut healed, garbage, &t.w9, t.inc9);
        let opened = healed
            .open_entry_set(&t.id2)
            .expect("m2's read is Ok")
            .unwrap();
        let s = opened.entries.iter().find(|e| e.name == "S").unwrap();
        assert_eq!(s.value.as_slice(), b"v1");
    }

    /// F21's test 12. After test 1's heal no member hands `E` on or reuses it: neither
    /// `Log::admission_epochs` nor `Log::wrap_gaps` names it, and m2's next write would reuse
    /// the one newest epoch, side 1's, which counts.
    ///
    /// NEGATIVE CONTROL: with a keyed epoch's `Put` winners sent to `winning_epochs` this fails "m1
    /// hands E to a new Mac" (and test 1 fails its `winning_epochs` assertion).
    #[test]
    fn a_keyed_epoch_is_never_reused_or_handed_on() {
        let t = three();
        let mut split = keyed_split(&t, false);
        m2_writes_under_e(&t, &mut split);
        let mut healed = split.side_1.clone();
        healed.merge(&split.side_2).unwrap();
        let audiences = [crate::membership_log::SeatAudience::Everyone];
        for (who, id) in [("m1", &t.id1), ("m2", &t.id2)] {
            let opened = healed.open_audience_entries(id).unwrap().unwrap();
            let handed: Vec<RecordId> = Log::admission_epochs(&opened, &audiences)
                .into_iter()
                .map(|(_, epoch)| epoch)
                .collect();
            assert!(!handed.contains(&split.epoch), "{who} hands E to a new Mac");
            assert!(
                healed
                    .wrap_gaps(&opened, &audiences)
                    .iter()
                    .all(|gap| gap.epoch != split.epoch),
                "{who} closes a wrap gap of E"
            );
            let newest = &opened.maximal_epochs[&EpochAudience::Everyone];
            assert_eq!(newest.len(), 1, "one newest epoch on {who}");
            assert_ne!(newest[0], split.epoch);
            assert!(opened.unwrapped_keys.contains_key(&newest[0]));
        }
    }

    /// F21's test 2. Founders A, B, M and Y. On side 2, M removes A, mints `E` for all four,
    /// puts `T`, and Y puts `S=v1` under `E` before M removes B too. After the heal A and B sign
    /// the quorum `Remove` of M at `Log::undo_cutoff`, which voids `E`: Y's write stands for A, B
    /// and Y, and M's own `Put` reads nowhere.
    ///
    /// NEGATIVE CONTROL: with `Graph::effective` keying an epoch only when it is cut, never when a
    /// quorum voided it, this fails "S on A" (left `v0`, right `v1`), while test 1 passes.
    #[test]
    fn an_honest_write_under_the_epoch_of_a_member_a_quorum_voided_stands() {
        let (wa, inca, ida, ma, ea) = make_entrant(61, "m1");
        let (wb, incb, idb, mb, eb) = make_entrant(62, "m2");
        let (wm, incm, idm, mm, em) = make_entrant(63, "m3");
        let (wy, incy, idy, my, ey) = make_entrant(64, "m4");
        let mut base =
            Log::new(Record::found(&[7; 32], vec![ea, eb, em, ey], 1_000).unwrap()).unwrap();
        let genesis = *base.network();
        let (draft, key0) = base.draft_epoch(&ma, 1_001, Audience::Everyone, 1).unwrap();
        let e0 = hold(&mut base, draft, &wa, inca);
        for id in [&ida, &idb, &idm, &idy] {
            let wrap = base
                .draft_wrap(&ma, 1_002, e0, &key0, id.recipient())
                .unwrap();
            hold(&mut base, wrap, &wa, inca);
        }
        let draft = base
            .draft_put(&ma, 1_003, e0, &key0, &put("S", b"v0"))
            .unwrap();
        hold(&mut base, draft, &wa, inca);

        let mut side_2 = base.clone();
        let removal = side_2.draft(
            &mm,
            1_100,
            Body::Remove {
                member: ma,
                cutoff: genesis,
            },
        );
        hold(&mut side_2, removal, &wm, incm);
        let (draft, key) = side_2
            .draft_epoch(&mm, 1_101, Audience::Everyone, 2)
            .unwrap();
        let epoch = hold(&mut side_2, draft, &wm, incm);
        for id in [&ida, &idb, &idm, &idy] {
            let wrap = side_2
                .draft_wrap(&mm, 1_102, epoch, &key, id.recipient())
                .unwrap();
            hold(&mut side_2, wrap, &wm, incm);
        }
        let draft = side_2
            .draft_put(&mm, 1_103, epoch, &key, &put("T", b"evil"))
            .unwrap();
        hold(&mut side_2, draft, &wm, incm);
        let draft = side_2
            .draft_put(&my, 1_104, epoch, &key, &put("S", b"v1"))
            .unwrap();
        hold(&mut side_2, draft, &wy, incy);
        let removal = side_2.draft(
            &mm,
            1_105,
            Body::Remove {
                member: mb,
                cutoff: genesis,
            },
        );
        hold(&mut side_2, removal, &wm, incm);

        let mut healed = base;
        healed.merge(&side_2).unwrap();
        let cutoff = healed.undo_cutoff(&mm).expect("an Undo of M");
        for (seed, inc, by) in [(&wa, inca, &ma), (&wb, incb, &mb)] {
            let share = healed.draft(by, 1_200, Body::Remove { member: mm, cutoff });
            hold(&mut healed, share, seed, inc);
        }
        let membership = healed.membership();
        assert!(
            membership.removal(&mm).is_some_and(|r| r.quorum),
            "M is out by quorum"
        );
        assert!(
            membership.contains(&ma) && membership.contains(&mb),
            "A and B are back"
        );
        for (who, id) in [("A", &ida), ("B", &idb), ("Y", &idy)] {
            assert_eq!(read(&healed, id, "S"), Some(b"v1".to_vec()), "S on {who}");
            assert_eq!(read(&healed, id, "T"), None, "T on {who}");
        }
        assert_eq!(healed.open_entries(&idm).unwrap(), None, "M reads nothing");
    }

    /// F21's attack 9, as the hostile review found it: Y keeps writing while the Undo's shares
    /// are being collected. Founders A, B, M and Y; on side 2, M removes A, mints `E`, and Y puts
    /// `S=v1` under it, then M removes B. After the heal A signs its share of the quorum
    /// `Remove` of M; Y, holding it, still counts M (a lone share is no quorum) and puts `S=v2`
    /// under `E`, its one newest epoch. Then B's share completes the quorum, which voids `E`.
    /// v2 is still a dependent of `E` and stands for A, B and Y: A's share, in its past, was no
    /// removal where Y signed it, so it does not trip the good-faith clause.
    ///
    /// NEGATIVE CONTROL: with `Graph::depends` judging a `Remove` by the quorums that take effect
    /// alone (dropping its `stands_at_d` clause, cc2d71149c's rule), this fails "Y's second write
    /// is a dependent of E", and every Mac reads `S=v1`.
    #[test]
    fn a_write_made_while_the_undo_shares_are_collected_stands() {
        let (wa, inca, ida, ma, ea) = make_entrant(65, "m1");
        let (wb, incb, idb, mb, eb) = make_entrant(66, "m2");
        let (wm, incm, _, mm, em) = make_entrant(67, "m3");
        let (wy, incy, idy, my, ey) = make_entrant(68, "m4");
        let mut base =
            Log::new(Record::found(&[7; 32], vec![ea, eb, em, ey], 1_000).unwrap()).unwrap();
        let genesis = *base.network();
        let (draft, key0) = base.draft_epoch(&ma, 1_001, Audience::Everyone, 1).unwrap();
        let e0 = hold(&mut base, draft, &wa, inca);
        for id in [&ida, &idb, &idy] {
            let wrap = base
                .draft_wrap(&ma, 1_002, e0, &key0, id.recipient())
                .unwrap();
            hold(&mut base, wrap, &wa, inca);
        }
        let draft = base
            .draft_put(&ma, 1_003, e0, &key0, &put("S", b"v0"))
            .unwrap();
        hold(&mut base, draft, &wa, inca);

        let mut log = base;
        let removal = log.draft(
            &mm,
            1_100,
            Body::Remove {
                member: ma,
                cutoff: genesis,
            },
        );
        hold(&mut log, removal, &wm, incm);
        let (draft, key) = log.draft_epoch(&mm, 1_101, Audience::Everyone, 2).unwrap();
        let epoch = hold(&mut log, draft, &wm, incm);
        for id in [&ida, &idb, &idy] {
            let wrap = log
                .draft_wrap(&mm, 1_102, epoch, &key, id.recipient())
                .unwrap();
            hold(&mut log, wrap, &wm, incm);
        }
        let draft = log
            .draft_put(&my, 1_104, epoch, &key, &put("S", b"v1"))
            .unwrap();
        hold(&mut log, draft, &wy, incy);
        let removal = log.draft(
            &mm,
            1_105,
            Body::Remove {
                member: mb,
                cutoff: genesis,
            },
        );
        hold(&mut log, removal, &wm, incm);

        let cutoff = log.undo_cutoff(&mm).expect("an Undo of M");
        let share = log.draft(&ma, 1_200, Body::Remove { member: mm, cutoff });
        hold(&mut log, share, &wa, inca);
        let held = log.open_audience_entries(&idy).unwrap().unwrap();
        assert_eq!(
            held.maximal_epochs[&EpochAudience::Everyone],
            [epoch],
            "CONTROL: with one share, Y still counts M's E as its one newest epoch"
        );
        let draft = log
            .draft_put(&my, 1_210, epoch, &key, &put("S", b"v2"))
            .unwrap();
        let second = hold(&mut log, draft, &wy, incy);
        let share = log.draft(&mb, 1_220, Body::Remove { member: mm, cutoff });
        hold(&mut log, share, &wb, incb);

        let membership = log.membership();
        assert!(
            membership.removal(&mm).is_some_and(|r| r.quorum),
            "CONTROL: M is out by quorum"
        );
        assert!(
            log.keyed_epochs()
                .iter()
                .any(|k| k.epoch == epoch && k.dependents.contains(&second)),
            "Y's second write is a dependent of E"
        );
        for (who, id) in [("A", &ida), ("B", &idb), ("Y", &idy)] {
            assert_eq!(read(&log, id, "S"), Some(b"v2".to_vec()), "S on {who}");
        }
    }

    /// One founder of [`undone_lab`]: its writer seed, incarnation, identity and actor.
    struct Founder {
        seed: [u8; 32],
        inc: [u8; 16],
        id: Identity,
        actor: MemberId,
    }

    impl Founder {
        fn hold(&self, log: &mut Log, body: Body) -> RecordId {
            let draft = log.draft(&self.actor, 1_400, body);
            hold(log, draft, &self.seed, self.inc)
        }
    }

    /// F16's Undo, the F8 lane's shape before F21's split: founders A, B, R and Y with `S=v0`
    /// under A's epoch; B removes A, R and Y, and A and Y undo it by a quorum `Remove` of B,
    /// which voids B's removals. Returns the log, A, R, Y and R's entrant.
    fn undone_lab() -> (Log, Founder, Founder, Founder, Entrant) {
        let founder = |tag: u8, label: &str| {
            let (seed, inc, id, actor, entrant) = make_entrant(tag, label);
            (
                Founder {
                    seed,
                    inc,
                    id,
                    actor,
                },
                entrant,
            )
        };
        let (a, ea) = founder(71, "m1");
        let (b, eb) = founder(72, "m2");
        let (r, er) = founder(73, "m3");
        let (y, ey) = founder(74, "m4");
        let mut log =
            Log::new(Record::found(&[7; 32], vec![ea, eb, er.clone(), ey], 1_000).unwrap())
                .unwrap();
        let genesis = *log.network();
        let (draft, key0) = log
            .draft_epoch(&a.actor, 1_001, Audience::Everyone, 1)
            .unwrap();
        let e0 = hold(&mut log, draft, &a.seed, a.inc);
        for id in [&a.id, &r.id, &y.id] {
            let wrap = log
                .draft_wrap(&a.actor, 1_002, e0, &key0, id.recipient())
                .unwrap();
            hold(&mut log, wrap, &a.seed, a.inc);
        }
        let draft = log
            .draft_put(&a.actor, 1_003, e0, &key0, &put("S", b"v0"))
            .unwrap();
        hold(&mut log, draft, &a.seed, a.inc);
        for member in [a.actor, r.actor, y.actor] {
            b.hold(
                &mut log,
                Body::Remove {
                    member,
                    cutoff: genesis,
                },
            );
        }
        let cutoff = log.undo_cutoff(&b.actor).expect("an Undo of B");
        for by in [&a, &y] {
            by.hold(
                &mut log,
                Body::Remove {
                    member: b.actor,
                    cutoff,
                },
            );
        }
        let membership = log.membership();
        assert!(
            membership.removal(&b.actor).is_some_and(|r| r.quorum),
            "B is out by quorum"
        );
        assert!(
            [a.actor, r.actor, y.actor]
                .iter()
                .all(|m| membership.contains(m)),
            "A, R and Y are back"
        );
        (log, a, r, y, er)
    }

    /// F21's split on `side_2`: R mints `E` for A, R and Y and puts `T`, and Y puts `S=v1` under
    /// `E`. Returns `E` and Y's write.
    fn r_mints_and_y_writes(
        side_2: &mut Log,
        a: &Founder,
        r: &Founder,
        y: &Founder,
    ) -> (RecordId, RecordId) {
        let (draft, key) = side_2
            .draft_epoch(&r.actor, 1_501, Audience::Everyone, 2)
            .unwrap();
        let epoch = hold(side_2, draft, &r.seed, r.inc);
        for id in [&a.id, &r.id, &y.id] {
            let wrap = side_2
                .draft_wrap(&r.actor, 1_502, epoch, &key, id.recipient())
                .unwrap();
            hold(side_2, wrap, &r.seed, r.inc);
        }
        let draft = side_2
            .draft_put(&r.actor, 1_503, epoch, &key, &put("T", b"evil"))
            .unwrap();
        hold(side_2, draft, &r.seed, r.inc);
        let draft = side_2
            .draft_put(&y.actor, 1_504, epoch, &key, &put("S", b"v1"))
            .unwrap();
        let edit = hold(side_2, draft, &y.seed, y.inc);
        (epoch, edit)
    }

    /// After the heal of `side_1` and `side_2`: R is out, Y's `edit` is a dependent of `epoch`,
    /// and A and Y read `S=v1` and no `T`.
    fn y_write_stands(
        side_1: Log,
        side_2: &Log,
        f: (&Founder, &Founder, &Founder),
        keyed: (RecordId, RecordId),
    ) {
        let (a, r, y) = f;
        let (epoch, edit) = keyed;
        let mut healed = side_1;
        healed.merge(side_2).unwrap();
        assert!(!healed.membership().contains(&r.actor), "R is out");
        assert!(
            healed
                .keyed_epochs()
                .iter()
                .any(|k| k.epoch == epoch && k.dependents.contains(&edit)),
            "Y's write is a dependent of E"
        );
        for (who, id) in [("A", &a.id), ("Y", &y.id)] {
            assert_eq!(read(&healed, id, "S"), Some(b"v1".to_vec()), "S on {who}");
            assert_eq!(read(&healed, id, "T"), None, "T on {who}");
        }
    }

    /// F21 after F16's Undo, the F8 lane's shape ([`undone_lab`]). On side 1, A removes R; on
    /// side 2, R mints `E` and Y writes under it ([`r_mints_and_y_writes`]). After the heal Y's
    /// write is a dependent of `E` and stands for A and Y: B's voided removal of R, in its past,
    /// removes nobody, so it neither unseats R where R signed `E` nor makes Y's write one signed
    /// after its signer held R's removal.
    ///
    /// NEGATIVE CONTROLS (each restored with cp from a backup plus touch, confirmed with cmp):
    /// - `Graph::depends` back on authorization with nothing void alone (origin/main's rule):
    ///   fails "Y's write is a dependent of E".
    /// - only the epoch clause back on it (`auth.plain[e]`): fails "Y's write is a dependent of
    ///   E".
    /// - only the `Remove` clause back on it (`auth.plain[r]`, a voided `Remove` cuts): fails
    ///   "Y's write is a dependent of E".
    #[test]
    fn a_removal_a_quorum_undid_never_cuts_an_honest_write_under_a_removed_macs_epoch() {
        let (base, a, r, y, _) = undone_lab();
        let mut side_1 = base.clone();
        a.hold(
            &mut side_1,
            Body::Remove {
                member: r.actor,
                cutoff: *base.network(),
            },
        );
        let mut side_2 = base;
        let keyed = r_mints_and_y_writes(&mut side_2, &a, &r, &y);
        y_write_stands(side_1, &side_2, (&a, &r, &y), keyed);
    }

    /// As [`a_removal_a_quorum_undid_never_cuts_an_honest_write_under_a_removed_macs_epoch`], with
    /// an Allow again between: after the Undo A removes R and Y lets R in again, an `Admit` that
    /// counts only under the quorum (in the view with nothing void B's removal of Y stands). On
    /// side 1 A removes R again, at that `Admit`; on side 2 R mints `E` and Y writes under it.
    /// After the heal Y's write still stands: Y's `Admit` restores R between A's first removal
    /// and `E`, so that removal does not cut `E` in Y's past.
    ///
    /// NEGATIVE CONTROL (restored with cp from a backup plus touch, confirmed with cmp): the
    /// `Admit` clause back on authorization with nothing void (`auth.plain[a]`): fails "Y's write
    /// is a dependent of E".
    #[test]
    fn an_allow_again_by_a_mac_a_voided_member_removed_restores_good_faith() {
        let (mut base, a, r, y, er) = undone_lab();
        let genesis = *base.network();
        a.hold(
            &mut base,
            Body::Remove {
                member: r.actor,
                cutoff: genesis,
            },
        );
        assert!(!base.membership().contains(&r.actor), "A removed R");
        let again = y.hold(
            &mut base,
            Body::Admit {
                entrant: er,
                evidence: Evidence::Phrase,
            },
        );
        assert!(base.membership().contains(&r.actor), "Y let R in again");
        let mut side_1 = base.clone();
        a.hold(
            &mut side_1,
            Body::Remove {
                member: r.actor,
                cutoff: again,
            },
        );
        let mut side_2 = base;
        let keyed = r_mints_and_y_writes(&mut side_2, &a, &r, &y);
        y_write_stands(side_1, &side_2, (&a, &r, &y), keyed);
    }

    /// F21's test 3. m2 deletes `U`; a reseal re-issues that `Delete` under a fresh epoch, while
    /// m2, with its own `Delete` in its past, puts `U=new` on the other side. After the heal
    /// `U=new` stands with no conflict: a resealed `Delete` carries, as a resealed `Put` does.
    ///
    /// NEGATIVE CONTROL: with `Register::classify` never taking a `Delete` as a carry
    /// (origin/main's rule) this fails "a resealed Delete read as a write on m1: {(Everyone,
    /// "U")}". Letting a `Delete` carry with a `Put` live in its past instead fails
    /// `epoch_wrap_put_delete_round_trip_and_refuse_tampering` ("m1 after delete").
    #[test]
    fn a_resealed_delete_loses_to_a_put_that_follows_the_delete_it_carries() {
        let t = three();
        let mut base = t.log.clone();
        let draft = base
            .draft_put(&t.m1, 1_500, t.epoch, &t.key, &put("U", b"u0"))
            .unwrap();
        hold(&mut base, draft, &t.w1, t.inc1);
        let draft = base
            .draft_delete(&t.m2, 1_501, t.epoch, &t.key, "U")
            .unwrap();
        hold(&mut base, draft, &t.w2, t.inc2);

        let mut side_a = base.clone();
        reseal(&mut side_a, &t, 2, &[]);
        let opened = side_a.open_audience_entries(&t.id1).unwrap().unwrap();
        let fresh = opened.maximal_epochs[&EpochAudience::Everyone][0];
        let key = opened.unwrapped_keys[&fresh].clone();
        let draft = side_a.draft_delete(&t.m1, 2_003, fresh, &key, "U").unwrap();
        hold(&mut side_a, draft, &t.w1, t.inc1);

        let mut side_b = base;
        let draft = side_b
            .draft_put(&t.m2, 1_600, t.epoch, &t.key, &put("U", b"new"))
            .unwrap();
        hold(&mut side_b, draft, &t.w2, t.inc2);

        side_a.merge(&side_b).unwrap();
        for (who, id) in [("m1", &t.id1), ("m2", &t.id2)] {
            assert_eq!(read(&side_a, id, "U"), Some(b"new".to_vec()), "U on {who}");
            let opened = side_a.open_entry_set(id).unwrap().unwrap();
            assert!(
                opened.conflicts.is_empty(),
                "a resealed Delete read as a write on {who}: {:?}",
                opened.conflicts
            );
        }
    }

    /// F21's coverage (rule 4) and rule 7 on test 1's split, with two more Macs: X, let in by m1
    /// on side 1 and wrapped side 1's epoch alone, and W, let in by m2 on side 2, wrapped `E` by
    /// m2 and writing under it too.
    ///
    /// - After the heal `E` is keyed and uncovered. m1, m2 and W hold it; m1 opens it and hides
    ///   nothing, while X, which cannot open it, hides `[everyone]`.
    /// - An epoch with no attestation never covers: m1 mints one past every dependent, holding
    ///   side 1's epoch, but has not attested `E`.
    /// - A partial mint never covers: W attests `E` and mints one past every dependent, but it
    ///   lacks side 1's epoch, the newest one there.
    /// - A full reseal covers: m1 attests `E`, then mints one for every member, and X stops
    ///   hiding.
    ///
    /// NEGATIVE CONTROLS, each failing its own assertion:
    /// - without the newest-key clause of a full reseal, "a partial mint never covers";
    /// - without the attestation clause, "an epoch with no attestation never covers";
    /// - with holders only the dependents' signers, "m1 holds E through m9's Wrap, m2 and W as
    ///   writers";
    /// - with rule 7 off, "a Mac without the key of an uncovered keyed epoch hides its audience";
    /// - with rule 7 ignoring the keys a Mac opened, "a holder that opens it hides nothing".
    #[test]
    fn a_keyed_epoch_is_covered_only_by_a_full_reseal() {
        let t = three();
        let mut split = keyed_split(&t, false);
        let (_, _, idx, _, ex) = make_entrant(70, "m10");
        let (ww, incw, idw, mw, ew) = make_entrant(71, "m11");
        let side_1 = &mut split.side_1;
        let admit = side_1.draft(
            &t.m1,
            1_650,
            Body::Admit {
                entrant: ex,
                evidence: Evidence::Phrase,
            },
        );
        hold(side_1, admit, &t.w1, t.inc1);
        let held = side_1.open_audience_entries(&t.id1).unwrap().unwrap();
        let e1 = held.maximal_epochs[&EpochAudience::Everyone][0];
        let wrap = side_1
            .draft_wrap(&t.m1, 1_651, e1, &held.unwrapped_keys[&e1], idx.recipient())
            .unwrap();
        hold(side_1, wrap, &t.w1, t.inc1);
        m2_writes_under_e(&t, &mut split);
        let side_2 = &mut split.side_2;
        let admit = side_2.draft(
            &t.m2,
            1_710,
            Body::Admit {
                entrant: ew,
                evidence: Evidence::Phrase,
            },
        );
        hold(side_2, admit, &t.w2, t.inc2);
        let wrap = side_2
            .draft_wrap(&t.m2, 1_711, split.epoch, &split.key, idw.recipient())
            .unwrap();
        hold(side_2, wrap, &t.w2, t.inc2);
        let draft = side_2
            .draft_put(&mw, 1_712, split.epoch, &split.key, &put("w-entry", b"w"))
            .unwrap();
        hold(side_2, draft, &ww, incw);
        let mut healed = split.side_1.clone();
        healed.merge(&split.side_2).unwrap();

        let keyed = healed.keyed_epochs();
        assert_eq!(keyed.len(), 1);
        assert_eq!(keyed[0].epoch, split.epoch);
        assert_eq!(keyed[0].dependents.len(), 4, "m2's three writes and W's");
        assert_eq!(
            keyed[0].holders,
            BTreeSet::from([t.m1, t.m2, mw]),
            "m1 holds E through m9's Wrap, m2 and W as writers"
        );
        assert!(!keyed[0].covered);
        let x_view = healed.open_audience_entries(&idx).unwrap().unwrap();
        assert_eq!(
            x_view.uncovered_keyed[&EpochAudience::Everyone],
            [split.epoch]
        );
        assert!(
            x_view.hidden_epochs,
            "a Mac without the key of an uncovered keyed epoch hides its audience"
        );
        assert!(
            !x_view
                .fully_unwrapped_audiences
                .contains(&EpochAudience::Everyone)
        );
        let m1_view = healed.open_audience_entries(&t.id1).unwrap().unwrap();
        assert!(
            !m1_view.hidden_epochs,
            "a holder that opens it hides nothing"
        );
        assert!(
            m1_view
                .fully_unwrapped_audiences
                .contains(&EpochAudience::Everyone)
        );

        // m1 mints past every dependent with every newest key, no attestation.
        let mint = |log: &mut Log, by: (&MemberId, &[u8; 32], [u8; 16]), generation| {
            let (draft, key) = log
                .draft_epoch(by.0, 3_000, Audience::Everyone, generation)
                .unwrap();
            let epoch = hold(log, draft, by.1, by.2);
            for id in [&t.id1, &t.id2, &idx, &idw] {
                let wrap = log
                    .draft_wrap(by.0, 3_001, epoch, &key, id.recipient())
                    .unwrap();
                hold(log, wrap, by.1, by.2);
            }
        };
        let attest = |log: &mut Log, by: (&MemberId, &[u8; 32], [u8; 16]), to: &Identity| {
            let wrap = log
                .draft_wrap(by.0, 2_900, split.epoch, &split.key, to.recipient())
                .unwrap();
            hold(log, wrap, by.1, by.2);
        };
        let m1 = (&t.m1, &t.w1, t.inc1);
        let mut bare = healed.clone();
        mint(&mut bare, m1, 3);
        assert!(
            !bare.keyed_epochs()[0].covered,
            "an epoch with no attestation never covers"
        );
        assert!(
            bare.open_audience_entries(&idx)
                .unwrap()
                .unwrap()
                .hidden_epochs
        );

        // W attests `E` and mints, lacking side 1's epoch.
        let mut partial = healed.clone();
        attest(&mut partial, (&mw, &ww, incw), &idw);
        mint(&mut partial, (&mw, &ww, incw), 3);
        assert!(
            !partial.keyed_epochs()[0].covered,
            "a partial mint never covers"
        );
        assert!(
            partial
                .open_audience_entries(&idx)
                .unwrap()
                .unwrap()
                .hidden_epochs
        );

        // m1 attests `E`, then mints: a full reseal.
        let mut full = healed.clone();
        attest(&mut full, m1, &t.id1);
        mint(&mut full, m1, 3);
        assert!(full.keyed_epochs()[0].covered, "a full reseal covers");
        let x_view = full.open_audience_entries(&idx).unwrap().unwrap();
        assert!(x_view.uncovered_keyed.is_empty());
        assert!(!x_view.hidden_epochs);
        assert!(
            x_view
                .fully_unwrapped_audiences
                .contains(&EpochAudience::Everyone)
        );
    }

    /// F21's coverage (rule 4) over two heals in one audience, each resealed by another Mac.
    /// Heal 1 is test 1's split: m1 attests m9's `E` and reseals for m1, m2 and W, a Mac m1 let
    /// in after the heal, which covers `E`. Then a second split: m1 removes W on one side while
    /// W, unaware, mints `F` on the other and m2 puts `S=v3` under it. After heal 2, m2 attests
    /// `F`, the one uncovered keyed epoch, and reseals for m1 and m2. That covers `F`: nobody
    /// attests `E` again, which an earlier full reseal in the epoch's past already covers, so
    /// no reseal is due anywhere and m1 reads `S=v3`.
    ///
    /// NEGATIVE CONTROL: with a full reseal required to attest every keyed epoch of the audience
    /// with a dependent in its past, covered ones included (step 6's rule), this fails "m2's
    /// reseal covers F" (`F` stays uncovered, and m2's reseal would repeat on every vault pass).
    #[test]
    fn a_later_keyed_epoch_is_covered_by_another_macs_full_reseal() {
        let t = three();
        let mut split = keyed_split(&t, false);
        m2_writes_under_e(&t, &mut split);
        let mut healed = split.side_1.clone();
        healed.merge(&split.side_2).unwrap();
        let (ww, incw, idw, mw, ew) = make_entrant(72, "m12");
        let admit = healed.draft(
            &t.m1,
            2_800,
            Body::Admit {
                entrant: ew,
                evidence: Evidence::Phrase,
            },
        );
        let w_entry = hold(&mut healed, admit, &t.w1, t.inc1);
        // Sign `by`'s attestation of `epoch` (a Wrap of it to `to`, `by`'s own recipient), then
        // one higher `[everyone]` epoch wrapped to `ids`.
        let reseal_by = |log: &mut Log,
                         by: (&MemberId, &[u8; 32], [u8; 16]),
                         to: &Identity,
                         keyed: (RecordId, &[u8; 32]),
                         generation: u64,
                         ids: &[&Identity]| {
            let wrap = log
                .draft_wrap(by.0, 2_900, keyed.0, keyed.1, to.recipient())
                .unwrap();
            hold(log, wrap, by.1, by.2);
            let (draft, key) = log
                .draft_epoch(by.0, 3_000, Audience::Everyone, generation)
                .unwrap();
            let epoch = hold(log, draft, by.1, by.2);
            for id in ids {
                let wrap = log
                    .draft_wrap(by.0, 3_001, epoch, &key, id.recipient())
                    .unwrap();
                hold(log, wrap, by.1, by.2);
            }
        };
        reseal_by(
            &mut healed,
            (&t.m1, &t.w1, t.inc1),
            &t.id1,
            (split.epoch, &split.key),
            3,
            &[&t.id1, &t.id2, &idw],
        );
        assert!(
            healed.keyed_epochs().iter().all(|k| k.covered),
            "CONTROL: m1's reseal covers E"
        );

        let mut side_a = healed.clone();
        let removal = side_a.draft(
            &t.m1,
            3_100,
            Body::Remove {
                member: mw,
                cutoff: w_entry,
            },
        );
        hold(&mut side_a, removal, &t.w1, t.inc1);
        let mut side_b = healed;
        let (draft, key_f) = side_b
            .draft_epoch(&mw, 3_100, Audience::Everyone, 4)
            .unwrap();
        let epoch_f = hold(&mut side_b, draft, &ww, incw);
        for id in [&t.id1, &t.id2, &idw] {
            let wrap = side_b
                .draft_wrap(&mw, 3_101, epoch_f, &key_f, id.recipient())
                .unwrap();
            hold(&mut side_b, wrap, &ww, incw);
        }
        let draft = side_b
            .draft_put(&t.m2, 3_102, epoch_f, &key_f, &put("S", b"v3"))
            .unwrap();
        hold(&mut side_b, draft, &t.w2, t.inc2);
        side_a.merge(&side_b).unwrap();
        let keyed = side_a.keyed_epochs();
        assert_eq!(keyed.len(), 2, "CONTROL: E and F are keyed");
        assert!(
            keyed.iter().any(|k| k.epoch == epoch_f && !k.covered),
            "CONTROL: F is uncovered after heal 2"
        );
        let due = reseal_due_on(&side_a, &t.m2);
        assert!(
            due.contains(&EpochAudience::Everyone),
            "CONTROL: m2 is due to reseal F"
        );

        reseal_by(
            &mut side_a,
            (&t.m2, &t.w2, t.inc2),
            &t.id2,
            (epoch_f, &key_f),
            5,
            &[&t.id1, &t.id2],
        );
        assert!(
            side_a.keyed_epochs().iter().all(|k| k.covered),
            "m2's reseal covers F"
        );
        for member in [&t.m1, &t.m2] {
            assert!(
                reseal_due_on(&side_a, member).is_empty(),
                "no reseal is due once F is covered"
            );
        }
        assert_eq!(read(&side_a, &t.id1, "S"), Some(b"v3".to_vec()));
    }

    /// F21's coverage (rule 4) needs every newest dependent of a keyed epoch in a full reseal's
    /// past. On m9's side of test 1's split, m2 lets W in and wraps `E` to it; then that copy
    /// forks, and m2 edits `S` under `E` on one copy while W edits `T` under it on the other: two
    /// concurrent newest dependents. m1 heals with W's copy alone, attests `E` and reseals for
    /// m1, m2 and W, which covers `E` there, its one dependent in the reseal's past. Once m2's
    /// copy merges in, `E` has a dependent the reseal never saw: it stands uncovered, and m1 is
    /// due to reseal again.
    ///
    /// NEGATIVE CONTROL: with a reseal covering a keyed epoch once any of its newest dependents
    /// lies in its past ([`keyed::Mutant::AnyNewestDependent`]), this fails "a dependent past
    /// the reseal leaves E uncovered".
    #[test]
    fn a_full_reseal_that_saw_one_of_two_concurrent_dependents_covers_nothing() {
        let t = three();
        let split = keyed_split(&t, false);
        let (ww, incw, idw, mw, ew) = make_entrant(75, "m15");
        let mut side_2 = split.side_2.clone();
        let admit = side_2.draft(
            &t.m2,
            1_710,
            Body::Admit {
                entrant: ew,
                evidence: Evidence::Phrase,
            },
        );
        hold(&mut side_2, admit, &t.w2, t.inc2);
        let wrap = side_2
            .draft_wrap(&t.m2, 1_711, split.epoch, &split.key, idw.recipient())
            .unwrap();
        hold(&mut side_2, wrap, &t.w2, t.inc2);
        let mut m2_copy = side_2.clone();
        let draft = m2_copy
            .draft_put(&t.m2, 1_720, split.epoch, &split.key, &put("S", b"v1"))
            .unwrap();
        hold(&mut m2_copy, draft, &t.w2, t.inc2);
        let mut w_copy = side_2;
        let draft = w_copy
            .draft_put(&mw, 1_720, split.epoch, &split.key, &put("T", b"t"))
            .unwrap();
        hold(&mut w_copy, draft, &ww, incw);

        // m1 heals with W's copy, attests `E`, then mints an `[everyone]` epoch for m1, m2 and W.
        let mut healed = split.side_1.clone();
        healed.merge(&w_copy).unwrap();
        let wrap = healed
            .draft_wrap(&t.m1, 2_900, split.epoch, &split.key, t.id1.recipient())
            .unwrap();
        hold(&mut healed, wrap, &t.w1, t.inc1);
        let (draft, key) = healed
            .draft_epoch(&t.m1, 3_000, Audience::Everyone, 3)
            .unwrap();
        let epoch = hold(&mut healed, draft, &t.w1, t.inc1);
        for id in [&t.id1, &t.id2, &idw] {
            let wrap = healed
                .draft_wrap(&t.m1, 3_001, epoch, &key, id.recipient())
                .unwrap();
            hold(&mut healed, wrap, &t.w1, t.inc1);
        }
        let keyed = healed.keyed_epochs();
        assert_eq!(keyed.len(), 1, "CONTROL: E is keyed");
        assert_eq!(keyed[0].dependents.len(), 1, "CONTROL: W's write");
        assert!(
            keyed[0].covered,
            "CONTROL: m1's reseal covers E where it saw every dependent"
        );
        assert!(
            reseal_due_on(&healed, &t.m1).is_empty(),
            "CONTROL: no reseal is due once E is covered"
        );

        healed.merge(&m2_copy).unwrap();
        let keyed = healed.keyed_epochs();
        assert_eq!(keyed.len(), 1);
        assert_eq!(keyed[0].dependents.len(), 2, "m2's write and W's");
        assert!(
            !keyed[0].covered,
            "a dependent past the reseal leaves E uncovered"
        );
        assert!(
            reseal_due_on(&healed, &t.m1).contains(&EpochAudience::Everyone),
            "m1 is due to reseal E again"
        );
    }

    /// F21's rule 6 carries a `Delete` only when every live op in its past is a `Delete`: one
    /// that settles a `Put`/`Delete` conflict writes. m1 puts `U=a` while m2 deletes `U`, and m2's
    /// `Delete` wins that conflict: the pair is redrafted, fresh salts in both each time, until
    /// the `Delete`'s record id sorts below the `Put`'s, so the tie-break goes its way on every
    /// run. Once both sides hold both, m1 deletes `U` while m2, unaware, puts `U=b`, and every Mac
    /// reads that as a conflict.
    ///
    /// NEGATIVE CONTROL: with `Register::classify` carrying a `Delete` whenever its past's winner
    /// is a `Delete`, this fails "m1's Delete read as a carry on m1".
    #[test]
    fn a_delete_that_settles_a_conflict_is_no_carry() {
        let t = three();
        let mut base = t.log.clone();
        let draft = base
            .draft_put(&t.m1, 1_500, t.epoch, &t.key, &put("U", b"u0"))
            .unwrap();
        hold(&mut base, draft, &t.w1, t.inc1);
        let conflict = (Audience::Everyone, "U".to_owned());
        // One fresh pair in two sorts the `Delete` first, so 64 pairs all miss once in 2^64 runs.
        // Redrafting the `Delete` alone against one `Put` missed all 64 times one run in 65 on
        // average: a `Put` whose id falls low in the space has few ids below it.
        let (mut side_a, delete) = (0..64)
            .find_map(|_| {
                let mut side_a = base.clone();
                let draft = side_a
                    .draft_put(&t.m1, 1_550, t.epoch, &t.key, &put("U", b"a"))
                    .unwrap();
                let put_a = hold(&mut side_a, draft, &t.w1, t.inc1);
                let record = base
                    .draft_delete(&t.m2, 1_600, t.epoch, &t.key, "U")
                    .unwrap()
                    .sign(&t.w2, t.inc2)
                    .unwrap();
                (*record.id() < put_a).then_some((side_a, record))
            })
            .expect("one of 64 fresh pairs sorts m2's Delete below m1's Put");
        let mut side_b = base.clone();
        side_b.insert(delete).unwrap();
        side_a.merge(&side_b).unwrap();
        side_b.merge(&side_a).unwrap();
        for (who, id) in [("m1", &t.id1), ("m2", &t.id2)] {
            assert_eq!(
                read(&side_a, id, "U"),
                None,
                "CONTROL: m2's first Delete wins its conflict on {who}"
            );
        }

        let draft = side_a
            .draft_delete(&t.m1, 1_700, t.epoch, &t.key, "U")
            .unwrap();
        hold(&mut side_a, draft, &t.w1, t.inc1);
        let draft = side_b
            .draft_put(&t.m2, 1_700, t.epoch, &t.key, &put("U", b"b"))
            .unwrap();
        hold(&mut side_b, draft, &t.w2, t.inc2);
        side_a.merge(&side_b).unwrap();
        for (who, id) in [("m1", &t.id1), ("m2", &t.id2)] {
            let opened = side_a.open_entry_set(id).unwrap().unwrap();
            assert!(
                opened.conflicts.contains(&conflict),
                "m1's Delete read as a carry on {who}"
            );
        }
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

    /// On 200 signed logs ([`generated`]: three Macs write, delete, reseal, mint Commons and
    /// `[mac-1 mac-2]` epochs that leave a Mac out a quarter of the time, remove each other, let
    /// each other back in and write under the epochs of the removed, on diverging copies), each of
    /// the three readings every opening and probe takes, of each copy after each step and of the
    /// merge, is what the one it replaced gives: every epoch verdict ([`epochs::oracle::check`]),
    /// every keyed-epoch reading ([`keyed::oracle::check`]), which finds keyed epochs, covered ones
    /// among them, and every register ([`register::oracle`]), each also the island's fold's reading
    /// (law 5's `aterm_mesh_core::convergence::Register`). Each oracle fails the generation on
    /// a disagreement, so a failure names its reading in its own panic and not in this test's name.
    /// NEGATIVE CONTROLS: with what a writer held keeping every carry, the openings disagree; a
    /// verdict that reads no seat entry, no prior wrap, lets a newest epoch the seat entry follows
    /// learn from a prior wrap, or counts a prior wrap the entry of a Mac that came back follows,
    /// disagrees, each by the first log that takes its branch.
    #[test]
    fn two_hundred_signed_logs_read_as_the_replaced_readings_did() {
        epochs::oracle::CHECKED.set((0, 0));
        keyed::oracle::CHECKED.set((0, 0));
        keyed::oracle::FOUND.set((0, 0));
        register::oracle::CHECKED.set((0, 0));
        register::oracle::ISLAND_CHECKED.set(0);
        let logs: Vec<Log> = (1..=200).map(generated).collect();

        let (checked, wrong) = epochs::oracle::CHECKED.get();
        assert!(checked > 30_000, "the probes checked verdicts: {checked}");
        assert_eq!(wrong, 0, "of {checked} epoch verdicts");
        let (checked, wrong) = keyed::oracle::CHECKED.get();
        let (found, covered) = keyed::oracle::FOUND.get();
        assert_eq!(wrong, 0, "of {checked} keyed-epoch readings");
        assert!(found > 0, "no reading of {checked} found a keyed epoch");
        assert!(
            covered > 0,
            "no reading of {found} found a covered keyed epoch"
        );
        let (checked, wrong) = register::oracle::CHECKED.get();
        assert!(checked > 5_000, "the openings checked registers: {checked}");
        assert_eq!(wrong, 0, "of {checked} registers");
        let island = register::oracle::ISLAND_CHECKED.get();
        assert!(
            island > 5_000,
            "the island's fold resolved registers: {island} of {checked}"
        );

        let (macs, _) = three_macs();
        let left_out = logs
            .iter()
            .filter(|log| {
                macs.iter().any(|mac| {
                    log.audience_left_out_recipient(
                        &EpochAudience::Everyone,
                        &mac.identity.recipient(),
                    )
                })
            })
            .count();
        assert!(left_out > 50, "merges leave a Mac out: {left_out}");

        register::oracle::COUNT_ONLY.set(true);
        register::oracle::CHECKED.set((0, 0));
        register::MUTANT.set(Some(register::Mutant::KeepEveryCarry));
        let (founder, _) = Mac::new(1);
        for log in &logs {
            let _ = log.open_entry_set(&founder.identity).unwrap();
        }
        register::MUTANT.set(None);
        register::oracle::COUNT_ONLY.set(false);
        let (checked, wrong) = register::oracle::CHECKED.get();
        assert!(
            checked > 0 && wrong > 0,
            "keeping every carry goes unnoticed"
        );

        for mutant in [
            epochs::Mutant::IgnoreSeat,
            epochs::Mutant::IgnorePriorWraps,
            epochs::Mutant::IgnoreEntryAfter,
            epochs::Mutant::IgnoreReturn,
        ] {
            epochs::oracle::CHECKED.set((0, 0));
            epochs::oracle::COUNT_ONLY.set(true);
            epochs::MUTANT.set(Some(mutant));
            // The verdict is `wrong > 0`, which the first disagreement settles.
            for seed in 1..=200 {
                let _ = generated(seed);
                if epochs::oracle::CHECKED.get().1 > 0 {
                    break;
                }
            }
            epochs::MUTANT.set(None);
            epochs::oracle::COUNT_ONLY.set(false);
            let (checked, wrong) = epochs::oracle::CHECKED.get();
            assert!(checked > 0 && wrong > 0, "{mutant:?} goes unnoticed");
        }
        epochs::oracle::CHECKED.set((0, 0));
    }

    /// Every keyed-epoch reading the tests take checks itself against the one it replaced
    /// ([`keyed::oracle`]), so the F21 scenarios above are the differential's cases.
    /// NEGATIVE CONTROLS: on the three coverage scenarios, a reading whose reseal signer holds
    /// the key of every newest epoch, that forgets the full reseals already found, or whose
    /// reseal covers a keyed epoch once any newest dependent lies in its past, disagrees with the
    /// replaced one (each scenario's own assertions may fail under the mutant as well). The last
    /// also fails the scenario of two concurrent dependents.
    #[test]
    fn the_keyed_epoch_reading_disagrees_under_each_mutant() {
        for mutant in [
            keyed::Mutant::HoldEveryNewest,
            keyed::Mutant::ForgetCovering,
            keyed::Mutant::AnyNewestDependent,
        ] {
            keyed::oracle::CHECKED.set((0, 0));
            keyed::oracle::COUNT_ONLY.set(true);
            keyed::MUTANT.set(Some(mutant));
            let _ = std::panic::catch_unwind(a_keyed_epoch_is_covered_only_by_a_full_reseal);
            let _ = std::panic::catch_unwind(
                a_later_keyed_epoch_is_covered_by_another_macs_full_reseal,
            );
            let concurrent = std::panic::catch_unwind(
                a_full_reseal_that_saw_one_of_two_concurrent_dependents_covers_nothing,
            );
            keyed::MUTANT.set(None);
            keyed::oracle::COUNT_ONLY.set(false);
            let (checked, wrong) = keyed::oracle::CHECKED.get();
            assert!(checked > 0 && wrong > 0, "{mutant:?} goes unnoticed");
            if mutant == keyed::Mutant::AnyNewestDependent {
                assert!(concurrent.is_err(), "{mutant:?} covers E");
            }
        }
    }

    include!("entries/host_carry_tests.rs");
}
