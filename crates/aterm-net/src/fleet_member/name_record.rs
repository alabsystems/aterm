// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A node's **name record** as it travels (`docs/DESIGN-machine-names-2026-11-04.md` §3.2): the
//! fixed binary layout its canonical bytes take, the ` mname=… mname_sig=…` words a book entry
//! carries, the signature over the node, its key and the fleet seed digest, the seed digest,
//! anchor and boot derivations, and a reader's acceptance.
//!
//! Nothing here reads a book, a file or a clock: callers hand in the entry's node and key, the
//! reader's seed digest, the highest (`eseq`, `rev`) the reader accepted for that node and the
//! time. What `settle` reads is the [`Record`] inside, so a record that passes [`accept`] goes to
//! `aterm_types::machine_name::settle` as it is.

use aterm_digest::{Sha256, hmac_sha256_parts};
use aterm_types::machine_name::{Anchor, Cap, Class, Nickname, NodeId, Number, Record, Was};

use crate::fleet::{NodeIdentity, NodeKey, SIG_LEN, verifies_record};

/// The signature domain of a name record ([`NodeIdentity::sign_record`]).
pub const NAME_DOMAIN: &str = "aterm/machine-name/1";

/// The layout version this build writes and reads; a record of any other is malformed here,
/// which drops only that record (§3.2).
pub const RECORD_V: u8 = 1;

/// The most canonical bytes a record takes: v, flags and presence 3, num 4, since 8, rev 8,
/// eseq 8, prev 4, `base` 17, `tgen` 4, `sgen` 4, `act` 8, anchor 8, boot 8, top 4, cap 12,
/// `was` 1 + 8×4, nickname 1 + 64, `by` 17 (§3.2).
pub const BODY_MAX: usize = 215;

/// The most `was` entries a record carries (§3.2).
pub const WAS_MAX: usize = aterm_types::machine_name::WAS_MAX;

/// The acceptance floor for `since` and `act`: 2026-01-01T00:00:00Z in Unix milliseconds.
pub const FLOOR_MS: u64 = 1_767_225_600_000;

/// How far past the reader's clock a `since` or `act` may lie and still stand (§3.2): one day.
pub const SKEW_MS: u64 = 86_400_000;

/// The highest evidence rank a `seeded` record (or a take of one) carries as its `since` (§7.1).
pub const EVIDENCE_RANK_MAX: u64 = 6;

/// A fleet seed: 16 random bytes the fleet's root mints (§3.3).
pub type Seed = [u8; 16];

/// What a node holding no seed signs under, and what a reader holding none verifies under.
pub const ZERO_DIGEST: [u8; 8] = [0; 8];

const NUM_BITS: u32 = 20;
const NUM_MASK: u32 = (1 << NUM_BITS) - 1;
const WAS_RETIRED: u32 = 1 << 30;
const WAS_LOST: u32 = 1 << 31;

/// How the nickname was set (§2.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NickClass {
    /// The drawn noun, re-checked once at the first numbering.
    Auto,
    /// A person's word, which never changes by itself.
    Chosen,
}

/// What made the last change (§3.2 `by`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ByKind {
    /// A person's press.
    Person,
    /// An agent's or a driver's explicit act.
    Agent,
    /// aterm itself.
    Auto,
}

/// Who made the last change: the node that asked (this one, or the driver of a remote rename).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct By {
    /// The node that asked.
    pub node: NodeId,
    /// Who asked through it.
    pub kind: ByKind,
}

/// A node's name record (§3.2): the claim `settle` reads, and the fields it travels with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameRecord {
    /// The NAME, class, since, `base`/`tgen`, `sgen`, `act`, anchor, `prev`, `waits`,
    /// `ephemeral`, `was`, `top` and cap.
    pub claim: Record,
    /// Rises on every change, from 1: readers keep the highest (`eseq`, `rev`) per node, and 0
    /// is the `rev` of a `machine-name.toml` no record has seeded, below every record.
    pub rev: u64,
    /// The signed `seq` of the self-entry this record was published with (Currency, §3.2).
    pub eseq: u64,
    /// The boot tag ([`boot`]); absent while the node holds no seed.
    pub boot: Option<[u8; 8]>,
    /// The nickname.
    pub nick: Option<Nickname>,
    /// How the nickname was set.
    pub nick_class: NickClass,
    /// Who made the last change.
    pub by: By,
}

/// Why a record's bytes are no record ([`NameRecord::decode`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Malformed {
    /// A version this build does not read.
    Version(u8),
    /// A reserved bit is set, or a field's value is outside its grammar.
    Field(&'static str),
    /// The bytes end inside a field.
    Short,
    /// Bytes follow the last field.
    Trailing,
}

impl NameRecord {
    /// The canonical bytes: the fixed layout of §3.2, big-endian, with at most the first
    /// [`WAS_MAX`] `was` entries ([`Self::sign`] refuses a record with more).
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let c = &self.claim;
        let mut out = Vec::with_capacity(BODY_MAX);
        out.push(RECORD_V);
        let class = match c.class {
            Class::Auto => 0,
            Class::Chosen => 1,
            Class::Seeded => 2,
        };
        let by = match self.by.kind {
            ByKind::Person => 0,
            ByKind::Agent => 1,
            ByKind::Auto => 2,
        };
        out.push(
            class
                | (u8::from(c.waits) << 2)
                | (u8::from(c.ephemeral) << 3)
                | (u8::from(self.nick_class == NickClass::Chosen) << 4)
                | (by << 5),
        );
        out.push(
            u8::from(c.num.is_some())
                | (u8::from(c.prev.is_some()) << 1)
                | (u8::from(c.base.is_some()) << 2)
                | (u8::from(c.anchor.is_some()) << 3)
                | (u8::from(self.boot.is_some()) << 4)
                | (u8::from(c.cap.is_some()) << 5)
                | (u8::from(self.nick.is_some()) << 6),
        );
        if let Some(num) = c.num {
            out.extend_from_slice(&num.get().to_be_bytes());
        }
        out.extend_from_slice(&c.since.to_be_bytes());
        out.extend_from_slice(&self.rev.to_be_bytes());
        out.extend_from_slice(&self.eseq.to_be_bytes());
        if let Some(prev) = c.prev {
            out.extend_from_slice(&prev.get().to_be_bytes());
        }
        if let Some(base) = c.base {
            put_node(&mut out, base);
        }
        out.extend_from_slice(&c.tgen.to_be_bytes());
        out.extend_from_slice(&c.sgen.to_be_bytes());
        out.extend_from_slice(&c.act.to_be_bytes());
        if let Some(anchor) = c.anchor {
            out.extend_from_slice(&anchor.0);
        }
        if let Some(boot) = self.boot {
            out.extend_from_slice(&boot);
        }
        out.extend_from_slice(&c.top.to_be_bytes());
        if let Some(cap) = c.cap {
            out.extend_from_slice(&cap.value.to_be_bytes());
            out.extend_from_slice(&cap.since.to_be_bytes());
        }
        let was = &c.was[..c.was.len().min(WAS_MAX)];
        out.push(u8::try_from(was.len()).unwrap_or(0));
        for entry in was {
            let word = entry.num.get()
                | if entry.retired { WAS_RETIRED } else { 0 }
                | if entry.lost { WAS_LOST } else { 0 };
            out.extend_from_slice(&word.to_be_bytes());
        }
        if let Some(nick) = &self.nick {
            let bytes = nick.as_str().as_bytes();
            out.push(u8::try_from(bytes.len()).unwrap_or(0));
            out.extend_from_slice(bytes);
        }
        put_node(&mut out, self.by.node);
        out
    }

    /// The record `bytes` spell, refusing anything [`Self::encode`] would not write: so one
    /// record has one spelling, and a signature over it says one thing.
    ///
    /// # Errors
    /// The version, a reserved bit, a field's grammar, or the length is wrong.
    pub fn decode(bytes: &[u8]) -> Result<Self, Malformed> {
        let mut r = Reader(bytes);
        let v = r.u8()?;
        if v != RECORD_V {
            return Err(Malformed::Version(v));
        }
        let flags = r.u8()?;
        let present = r.u8()?;
        if flags & 0x80 != 0 || present & 0x80 != 0 {
            return Err(Malformed::Field("reserved bit"));
        }
        let class = match flags & 0b11 {
            0 => Class::Auto,
            1 => Class::Chosen,
            2 => Class::Seeded,
            _ => return Err(Malformed::Field("class")),
        };
        let kind = match (flags >> 5) & 0b11 {
            0 => ByKind::Person,
            1 => ByKind::Agent,
            2 => ByKind::Auto,
            _ => return Err(Malformed::Field("by")),
        };
        let has = |bit: u8| present & (1 << bit) != 0;
        let num = if has(0) { Some(r.number("num")?) } else { None };
        let since = r.u64()?;
        let rev = r.u64()?;
        if rev == 0 {
            return Err(Malformed::Field("rev"));
        }
        let eseq = r.u64()?;
        let prev = if has(1) {
            Some(r.number("prev")?)
        } else {
            None
        };
        let base = if has(2) { Some(r.node()?) } else { None };
        let tgen = r.u32()?;
        let sgen = r.u32()?;
        let act = r.u64()?;
        let anchor = if has(3) {
            Some(Anchor(r.array()?))
        } else {
            None
        };
        let boot = if has(4) { Some(r.array()?) } else { None };
        let top = r.u32()?;
        let cap = if has(5) {
            Some(Cap {
                value: r.u32()?,
                since: r.u64()?,
            })
        } else {
            None
        };
        let count = usize::from(r.u8()?);
        if count > WAS_MAX {
            return Err(Malformed::Field("was"));
        }
        let mut was = Vec::with_capacity(count);
        for _ in 0..count {
            let word = r.u32()?;
            if word & !(NUM_MASK | WAS_RETIRED | WAS_LOST) != 0 {
                return Err(Malformed::Field("was"));
            }
            was.push(Was {
                num: Number::new(word & NUM_MASK).ok_or(Malformed::Field("was"))?,
                retired: word & WAS_RETIRED != 0,
                lost: word & WAS_LOST != 0,
            });
        }
        let nick = if has(6) {
            let len = usize::from(r.u8()?);
            let text = std::str::from_utf8(r.take(len)?).map_err(|_| Malformed::Field("nick"))?;
            Some(Nickname::parse(text).map_err(|_| Malformed::Field("nick"))?)
        } else {
            None
        };
        let by = r.node()?;
        if !r.0.is_empty() {
            return Err(Malformed::Trailing);
        }
        Ok(Self {
            claim: Record {
                num,
                class,
                since,
                base,
                tgen,
                sgen,
                act,
                anchor,
                prev,
                waits: flags & (1 << 2) != 0,
                ephemeral: flags & (1 << 3) != 0,
                was,
                top,
                cap,
            },
            rev,
            eseq,
            boot,
            nick,
            nick_class: if flags & (1 << 4) != 0 {
                NickClass::Chosen
            } else {
                NickClass::Auto
            },
            by: By { node: by, kind },
        })
    }

    /// `me`'s signature of this record under the seed digest `digest` (its fleet's
    /// [`seed_digest`], or [`ZERO_DIGEST`] while it holds none), as an entry carries it; `None`
    /// when `me`'s node id is none this layout spells.
    /// `None` too for a record with more than [`WAS_MAX`] `was` entries, which the layout
    /// cannot carry whole, and for one at `rev` 0, which stands below every record.
    #[must_use]
    pub fn sign(&self, me: &NodeIdentity, digest: [u8; 8]) -> Option<Carried> {
        if self.claim.was.len() > WAS_MAX || self.rev == 0 {
            return None;
        }
        let node = node_of(me.node())?;
        let bytes = self.encode();
        let sig = me.sign_record(NAME_DOMAIN, &signed_body(node, &me.key(), digest, &bytes));
        Carried::new(&bytes, sig)
    }
}

/// The most bytes [`Carried::words`] spells: ` mname=`, 287 base64url characters for
/// [`BODY_MAX`], ` mname_sig=` and 128 hex (§3.2's 433).
pub const WORDS_MAX: usize = 7 + (BODY_MAX * 4).div_ceil(3) + 11 + 2 * SIG_LEN;

/// A record as an entry carries it: its canonical bytes and their signature, not yet verified.
/// Fixed-size, like a signed name, so the entry signature that carries it stays `Copy`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Carried {
    bytes: [u8; BODY_MAX],
    len: u8,
    sig: [u8; SIG_LEN],
}

impl Carried {
    /// `bytes` and their signature `sig`; `None` past [`BODY_MAX`] bytes.
    #[must_use]
    pub fn new(bytes: &[u8], sig: [u8; SIG_LEN]) -> Option<Self> {
        let mut held = [0; BODY_MAX];
        held.get_mut(..bytes.len())?.copy_from_slice(bytes);
        Some(Self {
            bytes: held,
            len: u8::try_from(bytes.len()).ok()?,
            sig,
        })
    }

    /// The canonical bytes, not yet verified.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        self.bytes.get(..usize::from(self.len)).unwrap_or(&[])
    }

    /// The signature of them ([`signed_body`]).
    #[must_use]
    pub const fn sig(&self) -> &[u8; SIG_LEN] {
        &self.sig
    }

    /// ` mname=<base64url> mname_sig=<128 hex>`, at most [`WORDS_MAX`] bytes. (The codec
    /// refuses only input past its limit, far above [`BODY_MAX`]; were it to refuse, the
    /// words would be empty, an entry with no record.)
    #[must_use]
    pub fn words(&self) -> String {
        let mut words = String::with_capacity(WORDS_MAX);
        words.push_str(" mname=");
        if aterm_codec::base64::encode_url_into(&mut words, self.bytes()).is_err() {
            return String::new();
        }
        words.push_str(" mname_sig=");
        words.push_str(&aterm_codec::hex::encode_lower(&self.sig));
        words
    }

    /// The record `mname=` and `mname_sig=` spell: `None` when either is missing or malformed,
    /// so a bad record drops only itself and its entry stands (§3.2).
    #[must_use]
    pub fn of_words(mname: Option<&str>, mname_sig: Option<&str>) -> Option<Self> {
        let mname = mname.filter(|text| text.len() <= (BODY_MAX * 4).div_ceil(3))?;
        let bytes = aterm_codec::base64::decode_url(mname).ok()?;
        let sig = mname_sig.filter(|sig| aterm_codec::hex::is_lower(sig, 2 * SIG_LEN))?;
        Self::new(&bytes, crate::hex_decode(sig)?.try_into().ok()?)
    }
}

/// What a record arrived in (§3.2 Acceptance).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The node's book entry (the reader's book, an issuer's book, or a first-hand read).
    Entry,
    /// A copy outside a book: `name-revs` bytes, a fleet_db or Commons row.
    Copy,
}

/// What a reader makes of a carried record ([`accept`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Accepted {
    /// It stands for its node, signed under `digest`.
    Record {
        /// The record.
        record: NameRecord,
        /// The seed digest it verified under: the reader's, or [`ZERO_DIGEST`].
        digest: [u8; 8],
    },
    /// Its `since`, `act` or cap's `since` lies past the reader's clock plus [`SKEW_MS`]: an
    /// unknown claim until the clock reaches it (§3.2), so a cap dated ahead cannot stay the
    /// latest one (§6.2).
    Ahead(NameRecord),
}

/// Why a reader refused a carried record. A refusal drops only the record (§3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The entry's node id is none this layout spells.
    Node,
    /// No key, seed digest and node it may stand under verifies it.
    Signature,
    /// A copy signed under the zero digest: only a book entry may carry one.
    UnseededCopy,
    /// The bytes are no record.
    Malformed(Malformed),
    /// Its (`eseq`, `rev`) is below the highest this reader accepted for that node.
    Replay {
        /// The highest accepted.
        held: (u64, u64),
    },
    /// Its `since` (where it is no evidence rank the record may carry), `act` or cap's `since`
    /// lies before [`FLOOR_MS`].
    BeforeFloor,
    /// A `tgen` or `sgen` of `u32::MAX`, or a `top` or cap past m999999.
    Bound(&'static str),
}

/// What a reader holding seed digest `digest` (else [`ZERO_DIGEST`]) makes of `carried`,
/// which arrived from `source` in `node`'s entry, whose book binds `key`; `held` is the highest
/// (`eseq`, `rev`) it accepted for that node (`name-revs`) and `now_ms` its clock (§3.2
/// Acceptance).
///
/// # Errors
/// The [`Refused`] reason; the entry stands without its record.
pub fn accept(
    carried: &Carried,
    (node, key): (&str, &NodeKey),
    digest: [u8; 8],
    source: Source,
    held: Option<(u64, u64)>,
    now_ms: u64,
) -> Result<Accepted, Refused> {
    let id = node_of(node).ok_or(Refused::Node)?;
    let verifies = |under: [u8; 8]| {
        verifies_record(
            key,
            NAME_DOMAIN,
            &signed_body(id, key, under, carried.bytes()),
            carried.sig(),
        )
    };
    let under = if verifies(digest) {
        digest
    } else if digest != ZERO_DIGEST && verifies(ZERO_DIGEST) {
        ZERO_DIGEST
    } else {
        return Err(Refused::Signature);
    };
    if under == ZERO_DIGEST && source == Source::Copy {
        return Err(Refused::UnseededCopy);
    }
    let record = NameRecord::decode(carried.bytes()).map_err(Refused::Malformed)?;
    if let Some(held) = held
        && (record.eseq, record.rev) < held
    {
        return Err(Refused::Replay { held });
    }
    let c = &record.claim;
    if c.tgen == u32::MAX {
        return Err(Refused::Bound("tgen"));
    }
    if c.sgen == u32::MAX {
        return Err(Refused::Bound("sgen"));
    }
    if c.top > Number::MAX {
        return Err(Refused::Bound("top"));
    }
    if c.cap.is_some_and(|cap| cap.value > Number::MAX) {
        return Err(Refused::Bound("cap"));
    }
    // An evidence rank stands where §7.1 writes one and where a claim copies it: a seeded
    // record, any take of a claim (`tgen` past 0, a take-back of one's own number included),
    // and a succession (`sgen` past 0). Nothing is lost by the width: any node may sign a
    // seeded record at rank 1 (§12.2).
    let ranked = c.class == Class::Seeded || c.tgen > 0 || c.sgen > 0;
    let since_ranked = ranked && (1..=EVIDENCE_RANK_MAX).contains(&c.since);
    let cap_since = c.cap.map(|cap| cap.since);
    if (c.since < FLOOR_MS && !since_ranked)
        || c.act < FLOOR_MS
        || cap_since.is_some_and(|since| since < FLOOR_MS)
    {
        return Err(Refused::BeforeFloor);
    }
    let bound = now_ms.saturating_add(SKEW_MS);
    if c.since > bound || c.act > bound || cap_since.is_some_and(|since| since > bound) {
        return Ok(Accepted::Ahead(record));
    }
    Ok(Accepted::Record {
        record,
        digest: under,
    })
}

/// The nicknames other nodes' records in this node's books carry (§2.4's "in view", without the
/// `computers` table until stage 5): each record that verifies under its entry's key and the
/// seed digest held here (`digest`), else the zero digest, of every entry whose node is not
/// `me`. A book that cannot be read names none. It steers a first draw away from these words, so
/// a record that verifies under a key no member binds costs one more draw, never a NAME.
#[must_use]
pub fn nicknames_in_books(
    dir: &std::path::Path,
    me: Option<&str>,
    digest: [u8; 8],
) -> Vec<Nickname> {
    carried_in_books(dir)
        .into_iter()
        .filter(|(node, _, _)| Some(node.as_str()) != me)
        .filter_map(|(node, key, carried)| {
            match accept(
                &carried,
                (&node, &key),
                digest,
                Source::Entry,
                None,
                u64::MAX,
            ) {
                Ok(Accepted::Record { record, .. }) => record.nick,
                Ok(_) | Err(_) => None,
            }
        })
        .collect()
}

/// Each entry in `fleet-peers` and `fleet-issuer-book`, in that order, that names its node and
/// key and carries a record, not yet verified. A book that cannot be read holds none.
fn carried_in_books(dir: &std::path::Path) -> Vec<(String, NodeKey, Carried)> {
    let mut carried = Vec::new();
    for file in [super::PEERS_FILE, super::ISSUER_BOOK_FILE] {
        let Ok(bytes) = super::read_private(dir, file, super::PEERS_FILE_MAX) else {
            continue;
        };
        for peer in String::from_utf8_lossy(&bytes)
            .lines()
            .filter_map(super::Peer::parse)
        {
            if let (Some(node), Some(key), Some(record)) = (
                peer.node,
                peer.key,
                peer.signed.as_ref().and_then(|signed| signed.record()),
            ) {
                carried.push((node, key, *record));
            }
        }
    }
    carried
}

/// What a name record's signature covers: `node ‖ key ‖ seed digest ‖ the canonical bytes`
/// (§3.2). Node, key and digest are not sent: the reader supplies them from the entry the
/// record rides in and its own seed, so a record signed in another fleet does not verify here.
#[must_use]
pub fn signed_body(node: NodeId, key: &NodeKey, digest: [u8; 8], bytes: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(17 + NodeKey::LEN + 8 + bytes.len());
    put_node(&mut body, node);
    body.extend_from_slice(key.bytes());
    body.extend_from_slice(&digest);
    body.extend_from_slice(bytes);
    body
}

/// The seed digest: `SHA-256("aterm/fleet-seed-digest/1" ‖ fleet seed)[..8]` (§3.2).
#[must_use]
pub fn seed_digest(seed: &Seed) -> [u8; 8] {
    let mut input = b"aterm/fleet-seed-digest/1".to_vec();
    input.extend_from_slice(seed);
    first8(&Sha256::digest(&input))
}

/// The anchor key: `SHA-256("aterm/machine-anchor-key/1\0" ‖ fleet seed)` (§3.2), stable
/// across key rotations as the seed is.
#[must_use]
pub fn anchor_key(seed: &Seed) -> [u8; 32] {
    let mut input = b"aterm/machine-anchor-key/1\0".to_vec();
    input.extend_from_slice(seed);
    Sha256::digest(&input)
}

/// The machine anchor: `HMAC(anchor key, "aterm/machine-anchor/1" ‖ os-kind ‖ platform
/// id)[..8]` (§3.2). The platform id is read in one spelling (trimmed, upper case), so two
/// readings of one machine give one anchor.
#[must_use]
pub fn anchor(key: &[u8; 32], os_kind: &str, platform_id: &str) -> Anchor {
    let platform = platform_id.trim().to_ascii_uppercase();
    Anchor(first8(&hmac_sha256_parts(
        key,
        &[
            b"aterm/machine-anchor/1",
            os_kind.as_bytes(),
            platform.as_bytes(),
        ],
    )))
}

/// The boot tag: `HMAC(anchor key, "aterm/machine-boot/1" ‖ boot session id)[..8]` (§3.2).
#[must_use]
pub fn boot(key: &[u8; 32], boot_id: &str) -> [u8; 8] {
    first8(&hmac_sha256_parts(
        key,
        &[b"aterm/machine-boot/1", boot_id.trim().as_bytes()],
    ))
}

/// A book's node id as the layout spells it: a legacy id's 16 bytes, a mesh principal
/// `n-<16 hex>`'s 8.
#[must_use]
pub fn node_of(word: &str) -> Option<NodeId> {
    if !crate::fleet::is_node_id(word) {
        return None;
    }
    let hex = word.strip_prefix("n-").unwrap_or(word);
    NodeId::new(&crate::hex_decode(hex)?)
}

/// The book's node id for `node`: [`node_of`] inverted.
#[must_use]
pub fn node_word(node: NodeId) -> String {
    let hex = aterm_codec::hex::encode_lower(node.as_bytes());
    if node.as_bytes().len() == 8 {
        format!("n-{hex}")
    } else {
        hex
    }
}

fn first8(bytes: &[u8; 32]) -> [u8; 8] {
    let mut out = [0; 8];
    out.copy_from_slice(&bytes[..8]);
    out
}

fn put_node(out: &mut Vec<u8>, node: NodeId) {
    let bytes = node.as_bytes();
    out.push(u8::try_from(bytes.len()).unwrap_or(0));
    out.extend_from_slice(bytes);
}

/// A cursor over a record's bytes.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Malformed> {
        let (head, rest) = self.0.split_at_checked(n).ok_or(Malformed::Short)?;
        self.0 = rest;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], Malformed> {
        self.take(N)?.try_into().map_err(|_| Malformed::Short)
    }

    fn u8(&mut self) -> Result<u8, Malformed> {
        Ok(self.array::<1>()?[0])
    }

    fn u32(&mut self) -> Result<u32, Malformed> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, Malformed> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    fn number(&mut self, field: &'static str) -> Result<Number, Malformed> {
        Number::new(self.u32()?).ok_or(Malformed::Field(field))
    }

    fn node(&mut self) -> Result<NodeId, Malformed> {
        let len = usize::from(self.u8()?);
        NodeId::new(self.take(len)?).ok_or(Malformed::Field("node"))
    }
}

#[cfg(test)]
mod tests;
