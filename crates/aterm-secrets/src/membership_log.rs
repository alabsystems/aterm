// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The membership log (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.2): signed records that every
//! Mac evaluates the same way, so membership needs no leader and a rejoin is a set union.
//!
//! - [`Record`]: one signed record, addressed by the SHA-256 of its bytes, that names its causal
//!   parents. A `Genesis` founds a network, whose id is the genesis record's id, and names the
//!   Commons root ([`Root`]) every member's binding names. `Admit`, `Remove`, `Name` and `Node`
//!   are signed by a member's device key: the Ed25519 writer key of its Commons binding, with the
//!   binding's incarnation as the rest of its 48-byte actor.
//! - A `Node` record binds its signer's fleet node key (F17): a founder its genesis lists with no
//!   node key gets one, and a member rotates its key. It carries a second signature, by the node
//!   key, over [`node_proof_message`]: [`NODE_PROOF_DOMAIN`], the network and the signer's actor.
//!   That domain is the message aterm-net's fleet node identity signs for a record
//!   (`aterm-fleet/v3 record `, the domain `membership node`, a NUL, then the body), so the two
//!   crates must change together. [`Record::decode`] refuses a proof that does not verify, so no
//!   member binds a key it does not hold.
//! - [`Log`]: the records one Mac holds, every one after its parents. The log only grows, and
//!   inserting the same records in any order, or merging two logs either way, gives the same log.
//! - [`Log::membership`]: the pure evaluator ([`Membership`]), with the quorum `Remove`, the label
//!   rule of §3.2, each member's current node key ([`Member::node_key`]) and the node-key index
//!   ([`Membership::node`]): whether a fleet node key is a member's now, a removed Mac's, two
//!   members', or one a member has since replaced.
//!
//! - [`genesis`]: ALab's Genesis v2 (§3.3.8, F12): its founders, the digest a build checks a
//!   genesis by ([`Log::alab`]), and the possession challenge a founder it lists with no binding
//!   proves its device key by ([`Log::challenge`]).
//!
//! This is a library. Nothing here reads or writes the vault, a file or the network; F13 and F14
//! replicate and evaluate its records across servers and Macs.

use crate::age::Recipient;
use crate::commons::binding::{BINDING_LEN, Binding, REQUEST_LEN, Request};
use crate::text::Audience;
use crate::{Error, Result};
use aterm_digest::Sha256;
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

pub mod admission;
pub mod budgets;
pub mod entries;
mod epochs;
mod eval;
mod fold;
pub mod genesis;
#[cfg(unix)]
pub(crate) mod history;
mod payload;
mod reach;
pub mod rekey;
pub mod store;
#[cfg(unix)]
pub(crate) mod untaken;

pub use admission::{Gap, SeatAudience, Signer, admission_audiences};
pub use astream_wire::sync::MAX_PARENTS;
pub use budgets::{
    AUTOMATIC_KEYED_STOP, Budget, LEGACY_RECORDS, MAX_HELD_RECORDS, MAX_KEYED_RECORDS,
    MAX_WEIGHED_RECORDS, RecordBudgets, RecordCounts,
};
pub use entries::{
    CommonsEpochView, Keyed, OpenedAudienceEntries, OpenedEntries, epoch_commitment,
};
pub use eval::{Member, Membership, NodeStanding, Quorum, Removal, RevokingRow, Undo};
pub use fold::{NETWORK_KEY_INFO, fold_network_key};
pub use genesis::Proof;
pub use payload::Payload;
pub use rekey::{RekeyDue, Turn, audience_tag, secure_twins};
pub use store::{
    MAX_RECORDS_STREAM, MemberVerdict, Store, Written, decode_records, decode_records_bounded,
    encode_records, on_written,
};

#[cfg(any(test, debug_assertions))]
static RECORD_DECODE_ATTEMPTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// The process-wide number of calls to [`Record::decode`], including refused records.
/// Observers compare snapshots after joining their workers; this counter never affects a verdict.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn test_record_decode_attempts() -> usize {
    RECORD_DECODE_ATTEMPTS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Exercise the actual quorum grounder at its current maximum candidate count for heap tests.
///
/// # Panics
/// The relation layout refuses the current bound, or the abstract verdict differs from its
/// known result (all candidates for an empty relation, only candidate zero for a dense order).
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn quorum_relations_allocation_probe(dense: bool) -> usize {
    eval::allocation_probe(dense)
}

/// Prepare real candidate-derived authorization states before a test measures their results.
/// The callback's operation retains its memo and every result through a checksum, then drops
/// them. Graph, states and scratch allocation happen before the callback, which reads the log's
/// own reachability index.
///
/// # Panics
/// `states` is zero or exceeds the distinct membership-void states of holding candidates.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub fn with_authorization_allocation_probe(
    log: &Log,
    states: usize,
    observe: impl FnOnce(&mut dyn FnMut() -> usize),
) {
    eval::with_authorization_allocation_probe(log.history(), states, observe);
}

/// Which audience an [`Body::Epoch`], [`Body::Put`] or [`Body::Delete`] seals.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EpochAudience {
    /// Vault `[everyone]` entries.
    Everyone,
    /// Vault `[<host>, ...]` entries (`1..=255` sorted distinct machine names).
    Hosts(Vec<String>),
    /// The Commons network key and `aterm-commons-control-v1` descriptor (`F18`).
    Commons,
}

impl EpochAudience {
    /// Convert a vault secret [`Audience`] variant back to [`Audience`]; returns `None` for
    /// [`EpochAudience::Commons`].
    #[must_use]
    pub fn as_text_audience(&self) -> Option<Audience> {
        match self {
            Self::Everyone => Some(Audience::Everyone),
            Self::Hosts(hosts) => Some(Audience::Hosts(hosts.clone())),
            Self::Commons => None,
        }
    }
}

impl From<Audience> for EpochAudience {
    fn from(audience: Audience) -> Self {
        match audience {
            Audience::Everyone => Self::Everyone,
            Audience::Hosts(hosts) => Self::Hosts(hosts),
        }
    }
}

impl From<&Audience> for EpochAudience {
    fn from(audience: &Audience) -> Self {
        match audience {
            Audience::Everyone => Self::Everyone,
            Audience::Hosts(hosts) => Self::Hosts(hosts.clone()),
        }
    }
}

/// A record's id: the SHA-256 of its whole signed bytes. A network's id is its genesis's.
pub type RecordId = astream_wire::sync::Digest;
/// A member's identity: its binding's writer key, then its incarnation (`Binding::actor`).
pub type MemberId = astream_wire::sync::Actor;

const MAGIC: &[u8; 4] = b"ALM1";
const VERSION: u8 = 1;
/// Where a record's kind byte sits: after the magic, the layout version and the network.
const KIND_AT: usize = 4 + 1 + 32;
/// Bytes through the parent-count field, before the variable parent list.
const FRAME_PREFIX_LEN: usize = 4 + 1 + 32 + 1 + 48 + 8 + 32 + 1;
/// The most bytes a record takes around its body: the header with [`MAX_PARENTS`] parents and
/// its signed clock, the body's length, and the signature.
const MOST_FRAME: usize = FRAME_PREFIX_LEN + 32 * MAX_PARENTS + 8 + 4 + 64;
const SIGNING_DOMAIN: &[u8] = b"aterm/members/record/1\0";
/// The largest record [`Record::decode`] reads: a 1 MiB secret value (`Put`) plus headers fits
/// with room.
pub const MAX_RECORD: usize = crate::text::MAX_VALUE + 64 * 1024;
/// The most records waiting for a missing parent that a [`Log`] or [`Store`] keeps.
pub const MAX_WAITING: usize = 4096;
/// The longest host name a record carries, in bytes.
pub const MAX_NAME: usize = 64;
/// The longest membership label, in bytes: `m` and five digits ([`is_label`]).
const MAX_LABEL: usize = 6;
/// The most founders a genesis names.
pub const MAX_FOUNDERS: usize = 255;

/// What a record does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// Founds a network and names its first members.
    Genesis,
    /// Lets one Mac in.
    Admit,
    /// Takes one member out after a cutoff.
    Remove,
    /// A member's own display name.
    Name,
    /// Mints a symmetric epoch key for one audience (§3.3.6).
    Epoch,
    /// Wraps an audience's epoch key to one age recipient (§3.3.6).
    Wrap,
    /// Seals a secret entry under an audience's epoch key (§3.3.6, §3.5.3).
    Put,
    /// Tombstones a secret entry name in an audience (§3.5.3).
    Delete,
    /// A member's own fleet node key, which the key itself vouches for.
    Node,
}

impl Kind {
    const fn code(self) -> u8 {
        match self {
            Kind::Genesis => 1,
            Kind::Admit => 2,
            Kind::Remove => 3,
            Kind::Name => 4,
            Kind::Epoch => 5,
            Kind::Wrap => 6,
            Kind::Put => 7,
            Kind::Delete => 8,
            Kind::Node => 9,
        }
    }
    fn from_code(code: u8) -> Result<Self> {
        Ok(match code {
            1 => Kind::Genesis,
            2 => Kind::Admit,
            3 => Kind::Remove,
            4 => Kind::Name,
            5 => Kind::Epoch,
            6 => Kind::Wrap,
            7 => Kind::Put,
            8 => Kind::Delete,
            9 => Kind::Node,
            _ => return Err(Error::new("membership record of an unknown kind")),
        })
    }

    /// The most bytes a record of this kind takes on the wire, as [`Record::decode`] bounds its
    /// header ([`MAX_PARENTS`] parents) and this kind's fields; [`MAX_RECORD`], the bound of
    /// every record, for a genesis, an `Epoch`, a `Put` and a `Delete`, whose founder, host and
    /// ciphertext fields this does not total. Every kind is named, so a new one is bounded where
    /// it is added.
    #[must_use]
    pub const fn most_bytes(self) -> usize {
        let body = match self {
            Kind::Admit => REQUEST_LEN + 32 + (1 + MAX_NAME) + (1 + MAX_LABEL) + 1,
            Kind::Remove => 48 + 32,
            Kind::Name => 1 + MAX_NAME,
            Kind::Wrap => 32 + 32 + 2 + entries::MAX_WRAP_CAPSULE,
            Kind::Node => 32 + 64,
            Kind::Genesis | Kind::Epoch | Kind::Put | Kind::Delete => return MAX_RECORD,
        };
        MOST_FRAME + body
    }
}

/// How the admitting member knew a person meant this Mac (§3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Evidence {
    /// A person pressed Allow and typed the phrase the member showed (§3.3.4).
    Phrase,
    /// No phrase: the admitting member decided by itself. A person's `aterm secrets approve
    /// <code>` on the member, or (since 0.148) the Mac proved to the member, on a fresh
    /// challenge, the fleet node key the log binds to its seat or the fleet's current key, or
    /// (since 0.150) its device key from the member's home ALab LAN, which the owner's
    /// 2026-11-02 ruling makes sufficient proof (`client::readmit`). 0.147 readers know this
    /// code, so no new evidence was needed.
    Manual,
    /// No person: the genesis lists the Mac's recipient with no binding, under the label the
    /// `Admit` carries, and the Mac proved its device key to the member by the possession
    /// challenge ([`Log::challenge`]). [`Log::insert`] refuses one that names any other Mac.
    Listed,
}

impl Evidence {
    const fn code(self) -> u8 {
        match self {
            Evidence::Phrase => 1,
            Evidence::Manual => 2,
            Evidence::Listed => 3,
        }
    }
    fn from_code(code: u8) -> Result<Self> {
        Ok(match code {
            1 => Evidence::Phrase,
            2 => Evidence::Manual,
            3 => Evidence::Listed,
            _ => return Err(Error::new("membership admission of an unknown evidence")),
        })
    }
}

/// One Mac a genesis names or an `Admit` lets in.
#[derive(Clone, Debug)]
pub struct Entrant {
    /// The Mac's self-signed device request: its binding, signed by its writer key, which any
    /// reader checks (`Request::decode`).
    pub request: Request,
    /// Its fleet node key.
    pub node_key: [u8; 32],
    /// Its host name, for display only.
    pub name: String,
    /// The `m<N>` label it claims (C11).
    pub label: String,
}

impl Entrant {
    /// The member this entrant becomes.
    pub fn member(&self) -> MemberId {
        self.request.binding().actor()
    }
}

/// The Commons network and authority a network's bindings name: which network a Mac belongs to.
/// The genesis names it, and [`Log::insert`] refuses an `Admit` whose entrant's binding names
/// another, so a binding made for another lab never enters this one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Root {
    /// The Commons network ([`Binding::network`]).
    pub network: [u8; 16],
    /// The authority the bindings are bound to ([`Binding::authority`]).
    pub authority: [u8; 32],
}

impl Root {
    /// The root `binding` names.
    pub fn of(binding: &Binding) -> Self {
        Self {
            network: *binding.network(),
            authority: *binding.authority(),
        }
    }
    /// Whether `binding` names this root.
    pub fn names(&self, binding: &Binding) -> bool {
        Self::of(binding) == *self
    }
}

/// One Mac a genesis names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Founder {
    /// Its age recipient: what `[machines]` names it by.
    pub recipient: Recipient,
    /// Its Commons binding, where the genesis holds one. A founder with none is a member only
    /// once it proves its device key to a member, who lets it in with an `Admit` of
    /// [`Evidence::Listed`] ([`Log::challenge`]).
    pub binding: Option<Binding>,
    /// Its fleet node key, where the genesis holds one. A founder listed with none binds one by
    /// its own `Node` record ([`Body::Node`]).
    pub node_key: Option<[u8; 32]>,
    /// Its host name, for display only.
    pub name: String,
    /// Its `m<N>` label.
    pub label: String,
}

impl Founder {
    /// The member it is from the genesis on, when the genesis holds its binding.
    pub fn member(&self) -> Option<MemberId> {
        self.binding.as_ref().map(Binding::actor)
    }
}

impl From<Entrant> for Founder {
    fn from(entrant: Entrant) -> Self {
        let binding = entrant.request.binding().clone();
        Self {
            recipient: binding.recipient(),
            binding: Some(binding),
            node_key: (entrant.node_key != [0; 32]).then_some(entrant.node_key),
            name: entrant.name,
            label: entrant.label,
        }
    }
}

/// What a record says.
#[derive(Clone, Debug)]
pub enum Body {
    /// The first members, and the Ed25519 key that signed the genesis: the seed Mac's device
    /// key, or for ALab the `ALAB_SIGNER` lineage. A build checks a genesis by its id, never by
    /// that key (§3.3.8), so the key is provenance only.
    Genesis {
        /// The key whose signature the record carries.
        signer: [u8; 32],
        /// The Commons root every member's binding names.
        root: Root,
        /// The first members, each with a distinct recipient, actor and label, at least one of
        /// them bound.
        founders: Vec<Founder>,
    },
    /// The signer lets `entrant` in.
    Admit {
        /// The Mac let in.
        entrant: Entrant,
        /// How a person meant it.
        evidence: Evidence,
    },
    /// The signer takes `member` out. `member`'s records not in `cutoff`'s causal past (the
    /// cutoff included) stop counting; the cutoff is the last record of `member` the signer
    /// accepted, or the record that let it in when it accepted none.
    Remove {
        /// The member taken out.
        member: MemberId,
        /// The record after which `member`'s records stop counting. It lies in this record's
        /// own causal past.
        cutoff: RecordId,
    },
    /// The signer's own host name, for display only.
    Name {
        /// The name.
        name: String,
    },
    /// Mints a symmetric epoch key for `audience` (§3.3.6).
    Epoch {
        /// Which audience this epoch seals.
        audience: EpochAudience,
        /// Monotone generation number (`>= 1`).
        generation: u64,
        /// Domain-separated SHA-256 commitment to the 32-byte epoch key.
        commitment: [u8; 32],
    },
    /// Wraps `epoch`'s symmetric key to `recipient` via binary age v1 (§3.3.6).
    Wrap {
        /// The [`Body::Epoch`] record id in this record's causal past.
        epoch: RecordId,
        /// The recipient whose age identity opens `capsule`.
        recipient: Recipient,
        /// Binary age v1 ciphertext carrying the committed epoch key.
        capsule: Payload,
    },
    /// Seals a secret entry (name, flags and value) under `epoch` (§3.3.6, §3.5.3).
    Put {
        /// The audience this entry belongs to (matching `epoch`'s audience).
        audience: EpochAudience,
        /// The [`Body::Epoch`] record id in this record's causal past.
        epoch: RecordId,
        /// Random 32-byte HKDF salt deriving this record's one-shot AEAD key.
        salt: [u8; 32],
        /// ChaCha20-Poly1305 ciphertext over `(name, flags, optional anchor, value)`.
        ciphertext: Payload,
    },
    /// Tombstones a secret entry name in `audience` under `epoch` (§3.5.3).
    Delete {
        /// The audience this tombstone belongs to (matching `epoch`'s audience).
        audience: EpochAudience,
        /// The [`Body::Epoch`] record id in this record's causal past.
        epoch: RecordId,
        /// Random 32-byte HKDF salt deriving this record's one-shot AEAD key.
        salt: [u8; 32],
        /// ChaCha20-Poly1305 ciphertext over the deleted entry name.
        ciphertext: Payload,
    },
    /// The signer's own fleet node key: it binds a member whose entry carried none (ALab's
    /// Genesis v2 founders), and it rotates one. Its latest effective `Node` is the member's node
    /// key ([`Member::node_key`]).
    Node {
        /// The node key: the Ed25519 public key of the Mac's fleet node identity.
        node_key: [u8; 32],
        /// The node key's Ed25519 signature over [`node_proof_message`] for this record's own
        /// network and signer, so no member binds a key it does not hold. [`Record::decode`]
        /// refuses a record whose proof does not verify.
        proof: [u8; 64],
    },
}

impl Body {
    /// The record kind this body is.
    pub const fn kind(&self) -> Kind {
        match self {
            Body::Genesis { .. } => Kind::Genesis,
            Body::Admit { .. } => Kind::Admit,
            Body::Remove { .. } => Kind::Remove,
            Body::Name { .. } => Kind::Name,
            Body::Epoch { .. } => Kind::Epoch,
            Body::Wrap { .. } => Kind::Wrap,
            Body::Put { .. } => Kind::Put,
            Body::Delete { .. } => Kind::Delete,
            Body::Node { .. } => Kind::Node,
        }
    }
}

/// A record not yet signed. [`Log::draft`] fills it from what the log holds.
#[derive(Clone, Debug)]
pub struct Draft {
    /// The network: its genesis record's id.
    pub network: RecordId,
    /// The signer's sequence, dense from one.
    pub sequence: u64,
    /// The signer's record at `sequence - 1`; `None` at sequence one.
    pub previous: Option<RecordId>,
    /// The records this one follows. `previous` must be among them; [`Draft::sign`] sorts them
    /// and drops repeats.
    pub parents: Vec<RecordId>,
    /// The signer's claimed wall clock, in UTC milliseconds. It is signed and shown, and never
    /// orders, authorizes or expires anything.
    pub authored_ms: u64,
    /// What it says.
    pub body: Body,
}

impl Draft {
    /// Sign as the device whose Ed25519 writer seed is `seed` and whose incarnation is
    /// `incarnation`, which together give the record's actor.
    ///
    /// # Errors
    /// The seed is not an Ed25519 seed, the body is a genesis ([`Record::found`] signs those),
    /// or the record is malformed (see [`Record::decode`]).
    pub fn sign(self, seed: &[u8; 32], incarnation: [u8; 16]) -> Result<Record> {
        if self.body.kind() == Kind::Genesis {
            return Err(Error::new(
                "a genesis is founded with Record::found, not signed as a draft",
            ));
        }
        let pair = key_pair(seed)?;
        let mut actor = [0; 48];
        actor[..32].copy_from_slice(pair.public_key().as_ref());
        actor[32..].copy_from_slice(&incarnation);
        let mut parents = self.parents;
        parents.sort_unstable();
        parents.dedup();
        let header = Header {
            network: self.network,
            kind: self.body.kind(),
            actor,
            sequence: self.sequence,
            previous: self.previous,
            parents,
            authored_ms: self.authored_ms,
        };
        Record::seal(&header, &encode_body(&self.body), &pair)
    }
}

/// One signed record of the membership log. A copy shares the record's parts rather than copying
/// them: a copy of a [`Store`] copies no record's bytes, so a server that copies its store to
/// try a POST against it (`server::apply_records`) copies none of them, however large its store.
/// Decoded ciphertext payloads are views into the same owned wire allocation.
#[derive(Clone, Debug)]
pub struct Record {
    id: RecordId,
    header: Arc<Header>,
    body: Arc<Body>,
    wire: Arc<[u8]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Header {
    network: RecordId,
    kind: Kind,
    actor: MemberId,
    sequence: u64,
    previous: Option<RecordId>,
    parents: Vec<RecordId>,
    authored_ms: u64,
}

impl Record {
    /// Found a network: a genesis naming `founders`, each bound by its own request, signed with
    /// the Ed25519 `seed`. The root is the one the founders' bindings name.
    ///
    /// # Errors
    /// The seed is not an Ed25519 seed, or a founder is malformed or repeated, or the founders
    /// name two roots (see [`Record::decode`]).
    pub fn found(seed: &[u8; 32], founders: Vec<Entrant>, authored_ms: u64) -> Result<Self> {
        let root = founders.first().map_or(
            Root {
                network: [0; 16],
                authority: [0; 32],
            },
            |first| Root::of(first.request.binding()),
        );
        let founders = founders.into_iter().map(Founder::from).collect();
        Self::genesis(seed, root, founders, authored_ms)
    }

    /// A genesis naming `founders` under `root`, signed with the Ed25519 `seed`: ALab's Genesis
    /// v2 lists its founders by recipient, with a binding where Commons holds one
    /// ([`genesis::alab_founders`]).
    ///
    /// # Errors
    /// The seed is not an Ed25519 seed, or a founder is malformed, repeated or bound under
    /// another root, or none is bound (see [`Record::decode`]).
    pub fn genesis(
        seed: &[u8; 32],
        root: Root,
        founders: Vec<Founder>,
        authored_ms: u64,
    ) -> Result<Self> {
        let pair = key_pair(seed)?;
        let signer: [u8; 32] = pair
            .public_key()
            .as_ref()
            .try_into()
            .map_err(|_| Error::new("an Ed25519 public key is 32 bytes"))?;
        let header = Header {
            network: [0; 32],
            kind: Kind::Genesis,
            actor: [0; 48],
            sequence: 1,
            previous: None,
            parents: Vec::new(),
            authored_ms,
        };
        let body = Body::Genesis {
            signer,
            root,
            founders,
        };
        Self::seal(&header, &encode_body(&body), &pair)
    }

    fn seal(header: &Header, body: &[u8], pair: &Ed25519KeyPair) -> Result<Self> {
        let mut wire = encode_header(header);
        put_len(&mut wire, body.len(), 4)?;
        wire.extend_from_slice(body);
        let signature = pair.sign(&signed_message(&wire));
        wire.extend_from_slice(signature.as_ref());
        Self::decode(&wire)
    }

    /// Read and verify exact bytes: the layout, every bound, and the signature, by the actor's
    /// writer key or, for a genesis, by the key it names. It does not check the record against a
    /// log ([`Log::insert`] does that).
    ///
    /// # Errors
    /// Anything in the bytes is malformed, out of bound, trailing or unsigned.
    pub fn decode(wire: &[u8]) -> Result<Self> {
        #[cfg(any(test, debug_assertions))]
        RECORD_DECODE_ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if wire.len() > MAX_RECORD {
            return Err(Error::new("membership record is too long"));
        }
        let mut reader = Reader::new(wire);
        if reader.take(4)? != MAGIC || reader.byte()? != VERSION {
            return Err(Error::new("not a membership record"));
        }
        let network = reader.array::<32>()?;
        let kind = Kind::from_code(reader.byte()?)?;
        let actor = reader.array::<48>()?;
        let sequence = reader.u64()?;
        let previous = Some(reader.array::<32>()?).filter(|p| *p != [0; 32]);
        let count = usize::from(reader.byte()?);
        if count > MAX_PARENTS {
            return Err(Error::new("membership record names too many parents"));
        }
        let mut parents = Vec::with_capacity(count);
        for _ in 0..count {
            parents.push(reader.array::<32>()?);
        }
        let authored_ms = reader.u64()?;
        let length = reader.len(4)?;
        let body_start = reader.at;
        reader.take(length)?;
        let body_end = reader.at;
        let owned_wire: Arc<[u8]> = Arc::from(wire);
        let body = decode_body(kind, &owned_wire, body_start..body_end)?;
        reader.take(64)?;
        reader.finish()?;
        let header = Header {
            network,
            kind,
            actor,
            sequence,
            previous,
            parents,
            authored_ms,
        };
        check_shape(&header, &body)?;
        let key: &[u8] = match &body {
            Body::Genesis { signer, .. } => signer,
            _ => &actor[..32],
        };
        if !crate::ed25519::verifies(
            key,
            &signed_message(&owned_wire[..body_end]),
            &owned_wire[body_end..],
        ) {
            return Err(Error::new("membership record signature failed"));
        }
        Ok(Self {
            id: Sha256::digest(&owned_wire),
            header: Arc::new(header),
            body: Arc::new(body),
            wire: owned_wire,
        })
    }

    /// The SHA-256 of [`Record::bytes`].
    pub const fn id(&self) -> &RecordId {
        &self.id
    }
    /// The exact signed bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.wire
    }
    /// What the record does.
    pub fn kind(&self) -> Kind {
        self.header.kind
    }
    /// What the record says.
    pub fn body(&self) -> &Body {
        &self.body
    }
    /// The network it belongs to; all zero for a genesis, whose own id is the network's.
    pub fn network(&self) -> &RecordId {
        &self.header.network
    }
    /// The network's genesis id: [`Record::id`] for a genesis, else [`Record::network`].
    pub fn network_id(&self) -> &RecordId {
        match self.header.kind {
            Kind::Genesis => &self.id,
            _ => &self.header.network,
        }
    }
    /// The exact byte length of the first record at the start of `wire`, bounded by [`MAX_RECORD`],
    /// refusing a kind this build does not know.
    pub(super) fn wire_len_prefix(wire: &[u8]) -> Result<usize> {
        let len = Self::known_frame_header(wire)?;
        if wire.len() < len {
            return Err(Error::new("membership record is truncated"));
        }
        Ok(len)
    }

    /// Length from a complete header, before allocating or reading its body.
    fn known_frame_header(wire: &[u8]) -> Result<usize> {
        let (len, _) = Self::frame_header(wire)?;
        let code = wire
            .get(KIND_AT)
            .copied()
            .ok_or_else(|| Error::new("membership record is truncated"))?;
        Kind::from_code(code)?;
        Ok(len)
    }

    fn frame_prefix_reader(wire: &[u8]) -> Result<(Reader<'_>, usize)> {
        let mut reader = Reader::new(wire);
        if reader.take(4)? != MAGIC || reader.byte()? != VERSION {
            return Err(Error::new("not a membership record"));
        }
        let _network = reader.take(32)?;
        let _kind = reader.byte()?;
        let _actor = reader.take(48)?;
        let _sequence = reader.take(8)?;
        let _previous = reader.take(32)?;
        let count = usize::from(reader.byte()?);
        if count > MAX_PARENTS {
            return Err(Error::new("membership record names too many parents"));
        }
        debug_assert_eq!(reader.at, FRAME_PREFIX_LEN);
        Ok((reader, count))
    }

    #[cfg(unix)]
    fn frame_header_len(wire: &[u8]) -> Result<usize> {
        let (reader, count) = Self::frame_prefix_reader(wire)?;
        Ok(reader.at + count * 32 + 8 + 4)
    }

    fn frame_header(wire: &[u8]) -> Result<(usize, Vec<RecordId>)> {
        let (mut reader, count) = Self::frame_prefix_reader(wire)?;
        let mut parents = Vec::with_capacity(count);
        for _ in 0..count {
            parents.push(reader.array::<32>()?);
        }
        let _authored_ms = reader.take(8)?;
        let body_len = reader.len(4)?;
        let len = reader
            .at
            .checked_add(body_len)
            .and_then(|len| len.checked_add(64))
            .filter(|&len| len <= MAX_RECORD)
            .ok_or_else(|| Error::new("membership record is too long"))?;
        Ok((len, parents))
    }

    /// [`Record::wire_len_prefix`] without judging the kind byte, with the parents the frame
    /// names. The layout (`VERSION` 1) frames a record of a kind only a newer build writes as it
    /// frames any other, so a stream holding one still splits into its records' ids
    /// (`store::frames`). Nothing is verified: the parents are whatever the bytes say.
    #[cfg(all(test, unix))]
    pub(super) fn frame_prefix(wire: &[u8]) -> Result<(usize, Vec<RecordId>)> {
        let (len, parents) = Self::frame_header(wire)?;
        if wire.len() < len {
            return Err(Error::new("membership record is truncated"));
        }
        Ok((len, parents))
    }
    /// The signing member; all zero for a genesis.
    pub fn actor(&self) -> &MemberId {
        &self.header.actor
    }
    /// The signer's sequence.
    pub fn sequence(&self) -> u64 {
        self.header.sequence
    }
    /// The signer's previous record.
    pub fn previous(&self) -> Option<&RecordId> {
        self.header.previous.as_ref()
    }
    /// The records it follows, sorted.
    pub fn parents(&self) -> &[RecordId] {
        &self.header.parents
    }
    /// The signer's claimed wall clock, in UTC milliseconds.
    pub fn authored_ms(&self) -> u64 {
        self.header.authored_ms
    }
    /// The replication header astream's causal index takes (`astream_wire::sync::Index`).
    pub fn sync_header(&self) -> astream_wire::sync::Header {
        astream_wire::sync::Header {
            id: self.id,
            actor: self.header.actor,
            sequence: self.header.sequence,
            previous: self.header.previous,
            parents: self.header.parents.clone(),
        }
    }
}

/// The checks that need no other record.
fn check_shape(header: &Header, body: &Body) -> Result<()> {
    let sorted = header.parents.windows(2).all(|w| w[0] < w[1]);
    if !sorted || header.parents.contains(&[0; 32]) {
        return Err(Error::new(
            "membership record parents are not sorted, distinct and non-zero",
        ));
    }
    if let Body::Genesis { root, founders, .. } = body {
        if header.network != [0; 32]
            || header.actor != [0; 48]
            || header.sequence != 1
            || header.previous.is_some()
            || !header.parents.is_empty()
        {
            return Err(Error::new(
                "a genesis names no network, actor, previous or parent",
            ));
        }
        if founders.is_empty() || founders.len() > MAX_FOUNDERS {
            return Err(Error::new("a genesis names from one to 255 founders"));
        }
        let bound: Vec<&Binding> = founders.iter().filter_map(|f| f.binding.as_ref()).collect();
        if bound.is_empty() {
            return Err(Error::new(
                "a genesis binds at least one founder, so a member can sign",
            ));
        }
        if founders.iter().any(|f| {
            f.binding
                .as_ref()
                .is_some_and(|b| b.recipient() != f.recipient)
        }) || !bound.iter().all(|binding| root.names(binding))
        {
            return Err(Error::new(
                "a founder's binding names its recipient and the genesis's Commons root",
            ));
        }
        let recipients: BTreeSet<Recipient> = founders.iter().map(|f| f.recipient).collect();
        let actors: BTreeSet<MemberId> = bound.iter().map(|b| b.actor()).collect();
        let labels: BTreeSet<&str> = founders.iter().map(|f| f.label.as_str()).collect();
        if recipients.len() != founders.len()
            || actors.len() != bound.len()
            || labels.len() != founders.len()
        {
            return Err(Error::new(
                "a genesis names each founder once, each with its own label",
            ));
        }
        return Ok(());
    }
    // A zero signer key is of small order: the signature check refuses it.
    if header.network == [0; 32] {
        return Err(Error::new("a membership record names its network"));
    }
    if header.parents.is_empty() {
        return Err(Error::new(
            "a membership record other than a genesis follows at least one record",
        ));
    }
    match (header.sequence, header.previous) {
        (0, _) => return Err(Error::new("membership sequences start at one")),
        (1, Some(_)) => return Err(Error::new("a first record names no previous")),
        (2.., None) => return Err(Error::new("a later record names its previous")),
        (_, Some(previous)) if !header.parents.contains(&previous) => {
            return Err(Error::new("a record's previous is among its parents"));
        }
        _ => {}
    }
    if let Body::Remove { member, cutoff } = body
        && (member[..32] == [0; 32] || *cutoff == [0; 32])
    {
        return Err(Error::new("a removal names a member and a cutoff"));
    }
    match body {
        Body::Epoch {
            audience,
            generation,
            commitment,
        } => {
            entries::check_audience(audience)?;
            if *generation == 0 || *commitment == [0; 32] {
                return Err(Error::new(
                    "an Epoch names a non-zero generation and commitment",
                ));
            }
        }
        Body::Wrap {
            epoch,
            recipient,
            capsule,
        } => {
            if *epoch == [0; 32]
                || !(1..=entries::MAX_WRAP_CAPSULE).contains(&capsule.len())
                || !crate::x25519::canonical_u(&recipient.0)
                || Recipient::parse(&recipient.to_bech32()).is_err()
            {
                return Err(Error::new(
                    "a Wrap names an Epoch, a valid age recipient and a bounded capsule",
                ));
            }
        }
        Body::Put {
            audience,
            epoch,
            ciphertext,
            ..
        } => {
            entries::check_audience(audience)?;
            if *epoch == [0; 32] || !(17..=entries::MAX_PUT_CIPHERTEXT).contains(&ciphertext.len())
            {
                return Err(Error::new(
                    "a Put names an Epoch and a bounded authenticated ciphertext",
                ));
            }
        }
        Body::Delete {
            audience,
            epoch,
            ciphertext,
            ..
        } => {
            entries::check_audience(audience)?;
            if *epoch == [0; 32]
                || !(17..=entries::MAX_DELETE_CIPHERTEXT).contains(&ciphertext.len())
            {
                return Err(Error::new(
                    "a Delete names an Epoch and a bounded authenticated ciphertext",
                ));
            }
        }
        Body::Node { node_key, proof }
            if !crate::ed25519::verifies(
                node_key,
                &node_proof_message(&header.network, &header.actor),
                proof,
            ) =>
        {
            return Err(Error::new("a node record's proof is not its node key's"));
        }
        _ => {}
    }
    Ok(())
}

/// The domain a `Node` record's proof is signed under. It is the message aterm-net's fleet node
/// identity signs for a record (`aterm-fleet/v3 record `, then the record's domain, then a NUL),
/// with the domain `membership node`: aterm-net produces exactly these bytes, so a change here
/// is a change there.
pub const NODE_PROOF_DOMAIN: &[u8] = b"aterm-fleet/v3 record membership node\0";

/// What a `Node` record's node key signs: [`NODE_PROOF_DOMAIN`], then the record's network (32
/// bytes), then its signer's actor (48 bytes), so a proof binds the key to one member of one
/// network, and no other member or network can reuse it.
pub fn node_proof_message(network: &RecordId, actor: &MemberId) -> Vec<u8> {
    [NODE_PROOF_DOMAIN, network, actor].concat()
}

fn key_pair(seed: &[u8; 32]) -> Result<Ed25519KeyPair> {
    Ed25519KeyPair::from_seed_unchecked(seed)
        .map_err(|_| Error::new("invalid membership signing seed"))
}

fn signed_message(unsigned: &[u8]) -> Vec<u8> {
    [SIGNING_DOMAIN, unsigned].concat()
}

fn encode_header(header: &Header) -> Vec<u8> {
    let mut wire = Vec::with_capacity(192 + 32 * header.parents.len());
    wire.extend_from_slice(MAGIC);
    wire.push(VERSION);
    wire.extend_from_slice(&header.network);
    wire.push(header.kind.code());
    wire.extend_from_slice(&header.actor);
    wire.extend_from_slice(&header.sequence.to_be_bytes());
    wire.extend_from_slice(&header.previous.unwrap_or([0; 32]));
    // More than `MAX_PARENTS` parents encode a wrapped count that `Record::decode` refuses.
    wire.push(u8::try_from(header.parents.len()).unwrap_or(u8::MAX));
    for parent in &header.parents {
        wire.extend_from_slice(parent);
    }
    wire.extend_from_slice(&header.authored_ms.to_be_bytes());
    wire
}

fn encode_body(body: &Body) -> Vec<u8> {
    let mut out = Vec::new();
    match body {
        Body::Genesis {
            signer,
            root,
            founders,
        } => {
            out.extend_from_slice(signer);
            out.extend_from_slice(&root.network);
            out.extend_from_slice(&root.authority);
            // More than `MAX_FOUNDERS` founders encode a count the decoder refuses.
            out.push(u8::try_from(founders.len()).unwrap_or(0));
            for founder in founders {
                encode_founder(&mut out, founder);
            }
        }
        Body::Admit { entrant, evidence } => {
            encode_entrant(&mut out, entrant);
            out.push(evidence.code());
        }
        Body::Remove { member, cutoff } => {
            out.extend_from_slice(member);
            out.extend_from_slice(cutoff);
        }
        Body::Name { name } => encode_text(&mut out, name),
        Body::Epoch {
            audience,
            generation,
            commitment,
        } => {
            entries::encode_audience_wire(&mut out, audience);
            out.extend_from_slice(&generation.to_be_bytes());
            out.extend_from_slice(commitment);
        }
        Body::Wrap {
            epoch,
            recipient,
            capsule,
        } => {
            out.extend_from_slice(epoch);
            out.extend_from_slice(&recipient.0);
            match u16::try_from(capsule.len()) {
                Ok(len) if (1..=entries::MAX_WRAP_CAPSULE).contains(&capsule.len()) => {
                    out.extend_from_slice(&len.to_be_bytes());
                    out.extend_from_slice(capsule);
                }
                _ => out.extend_from_slice(&0u16.to_be_bytes()),
            }
        }
        Body::Put {
            audience,
            epoch,
            salt,
            ciphertext,
        } => {
            entries::encode_audience_wire(&mut out, audience);
            out.extend_from_slice(epoch);
            out.extend_from_slice(salt);
            match u32::try_from(ciphertext.len()) {
                Ok(len) if (17..=entries::MAX_PUT_CIPHERTEXT).contains(&ciphertext.len()) => {
                    out.extend_from_slice(&len.to_be_bytes());
                    out.extend_from_slice(ciphertext);
                }
                _ => out.extend_from_slice(&0u32.to_be_bytes()),
            }
        }
        Body::Delete {
            audience,
            epoch,
            salt,
            ciphertext,
        } => {
            entries::encode_audience_wire(&mut out, audience);
            out.extend_from_slice(epoch);
            out.extend_from_slice(salt);
            match u16::try_from(ciphertext.len()) {
                Ok(len) if (17..=entries::MAX_DELETE_CIPHERTEXT).contains(&ciphertext.len()) => {
                    out.extend_from_slice(&len.to_be_bytes());
                    out.extend_from_slice(ciphertext);
                }
                _ => out.extend_from_slice(&0u16.to_be_bytes()),
            }
        }
        Body::Node { node_key, proof } => {
            out.extend_from_slice(node_key);
            out.extend_from_slice(proof);
        }
    }
    out
}

/// A founder's flags: which of its optional keys follow its recipient.
const FOUNDER_BOUND: u8 = 1;
const FOUNDER_NODE: u8 = 2;

fn encode_founder(out: &mut Vec<u8>, founder: &Founder) {
    out.extend_from_slice(&founder.recipient.0);
    let flags = u8::from(founder.binding.is_some()) * FOUNDER_BOUND
        + u8::from(founder.node_key.is_some()) * FOUNDER_NODE;
    out.push(flags);
    if let Some(binding) = &founder.binding {
        out.extend_from_slice(&binding.bytes());
    }
    if let Some(node_key) = &founder.node_key {
        out.extend_from_slice(node_key);
    }
    encode_text(out, &founder.name);
    encode_text(out, &founder.label);
}

fn encode_entrant(out: &mut Vec<u8>, entrant: &Entrant) {
    out.extend_from_slice(entrant.request.bytes());
    out.extend_from_slice(&entrant.node_key);
    encode_text(out, &entrant.name);
    encode_text(out, &entrant.label);
}

/// A name or label too long for its one-byte length encodes an empty one, which the decoder
/// refuses.
fn encode_text(out: &mut Vec<u8>, text: &str) {
    match u8::try_from(text.len()) {
        Ok(len) => {
            out.push(len);
            out.extend_from_slice(text.as_bytes());
        }
        Err(_) => out.push(0),
    }
}

fn decode_body(kind: Kind, wire: &Arc<[u8]>, range: std::ops::Range<usize>) -> Result<Body> {
    let mut reader = Reader::new(&wire[range.clone()]);
    let body = match kind {
        Kind::Genesis => {
            let signer = reader.array::<32>()?;
            let root = Root {
                network: reader.array::<16>()?,
                authority: reader.array::<32>()?,
            };
            let count = usize::from(reader.byte()?);
            let mut founders = Vec::with_capacity(count);
            for _ in 0..count {
                founders.push(decode_founder(&mut reader)?);
            }
            Body::Genesis {
                signer,
                root,
                founders,
            }
        }
        Kind::Admit => Body::Admit {
            entrant: decode_entrant(&mut reader)?,
            evidence: Evidence::from_code(reader.byte()?)?,
        },
        Kind::Remove => Body::Remove {
            member: reader.array::<48>()?,
            cutoff: reader.array::<32>()?,
        },
        Kind::Name => Body::Name {
            name: decode_name(&mut reader)?,
        },
        Kind::Epoch => Body::Epoch {
            audience: entries::decode_audience_wire(&mut reader)?,
            generation: reader.u64()?,
            commitment: reader.array::<32>()?,
        },
        Kind::Wrap => {
            let epoch = reader.array::<32>()?;
            let recipient = Recipient(reader.array::<32>()?);
            let len = reader.len(2)?;
            if !(1..=entries::MAX_WRAP_CAPSULE).contains(&len) {
                return Err(Error::new("membership Wrap capsule length out of bounds"));
            }
            let capsule = reader.payload(len, wire, range.start)?;
            Body::Wrap {
                epoch,
                recipient,
                capsule,
            }
        }
        Kind::Put => {
            let audience = entries::decode_audience_wire(&mut reader)?;
            let epoch = reader.array::<32>()?;
            let salt = reader.array::<32>()?;
            let len = reader.len(4)?;
            if !(17..=entries::MAX_PUT_CIPHERTEXT).contains(&len) {
                return Err(Error::new("membership Put ciphertext length out of bounds"));
            }
            let ciphertext = reader.payload(len, wire, range.start)?;
            Body::Put {
                audience,
                epoch,
                salt,
                ciphertext,
            }
        }
        Kind::Delete => {
            let audience = entries::decode_audience_wire(&mut reader)?;
            let epoch = reader.array::<32>()?;
            let salt = reader.array::<32>()?;
            let len = reader.len(2)?;
            if !(17..=entries::MAX_DELETE_CIPHERTEXT).contains(&len) {
                return Err(Error::new(
                    "membership Delete ciphertext length out of bounds",
                ));
            }
            let ciphertext = reader.payload(len, wire, range.start)?;
            Body::Delete {
                audience,
                epoch,
                salt,
                ciphertext,
            }
        }
        Kind::Node => Body::Node {
            node_key: reader.array::<32>()?,
            proof: reader.array::<64>()?,
        },
    };
    reader.finish()?;
    Ok(body)
}

fn decode_founder(reader: &mut Reader<'_>) -> Result<Founder> {
    let recipient = Recipient(reader.array::<32>()?);
    let flags = reader.byte()?;
    if flags & !(FOUNDER_BOUND | FOUNDER_NODE) != 0 {
        return Err(Error::new("a membership founder with unknown flags"));
    }
    let binding = if flags & FOUNDER_BOUND == 0 {
        None
    } else {
        Some(Binding::decode(reader.take(BINDING_LEN)?)?)
    };
    let node_key = if flags & FOUNDER_NODE == 0 {
        None
    } else {
        Some(reader.array::<32>()?)
    };
    if !crate::x25519::canonical_u(&recipient.0)
        || Recipient::parse(&recipient.to_bech32()).is_err()
    {
        return Err(Error::new("a membership founder's recipient is no age key"));
    }
    let name = decode_name(reader)?;
    let label = decode_label(reader)?;
    Ok(Founder {
        recipient,
        binding,
        node_key,
        name,
        label,
    })
}

fn decode_entrant(reader: &mut Reader<'_>) -> Result<Entrant> {
    let request = Request::decode(reader.take(REQUEST_LEN)?)?;
    let node_key = reader.array::<32>()?;
    let name = decode_name(reader)?;
    let label = decode_label(reader)?;
    Ok(Entrant {
        request,
        node_key,
        name,
        label,
    })
}

fn decode_label(reader: &mut Reader<'_>) -> Result<String> {
    let label = decode_text(reader)?;
    if !is_label(&label) {
        return Err(Error::new("a membership label is m<N>"));
    }
    Ok(label)
}

fn decode_name(reader: &mut Reader<'_>) -> Result<String> {
    let name = decode_text(reader)?;
    if name.is_empty() || name.len() > MAX_NAME || name.chars().any(char::is_control) {
        return Err(Error::new(
            "a membership name is 1 to 64 bytes with no control character",
        ));
    }
    Ok(name)
}

fn decode_text(reader: &mut Reader<'_>) -> Result<String> {
    let length = usize::from(reader.byte()?);
    String::from_utf8(reader.take(length)?.to_vec())
        .map_err(|_| Error::new("membership text is not UTF-8"))
}

/// `m` and one to five digits with no leading zero: what C11's allowing member writes.
pub fn is_label(label: &str) -> bool {
    label.len() <= MAX_LABEL
        && aterm_types::machine_name::Number::parse(label)
            .is_ok_and(|number| number.to_string() == label)
}

fn put_len(out: &mut Vec<u8>, length: usize, width: usize) -> Result<()> {
    let bytes = u64::try_from(length)
        .map_err(|_| Error::new("membership record is too long"))?
        .to_be_bytes();
    if bytes[..8 - width].iter().any(|b| *b != 0) {
        return Err(Error::new("membership record is too long"));
    }
    out.extend_from_slice(&bytes[8 - width..]);
    Ok(())
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }
    fn take(&mut self, length: usize) -> Result<&'a [u8]> {
        let end = self
            .at
            .checked_add(length)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| Error::new("membership record is truncated"))?;
        let taken = &self.bytes[self.at..end];
        self.at = end;
        Ok(taken)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.take(N)?
            .try_into()
            .map_err(|_| Error::new("membership record is truncated"))
    }
    fn payload(&mut self, length: usize, wire: &Arc<[u8]>, base: usize) -> Result<Payload> {
        let start = base
            .checked_add(self.at)
            .ok_or_else(|| Error::new("membership record is too long"))?;
        self.take(length)?;
        let end = base
            .checked_add(self.at)
            .ok_or_else(|| Error::new("membership record is too long"))?;
        Payload::in_wire(wire, start..end)
    }
    fn byte(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.array::<8>()?))
    }
    fn len(&mut self, width: usize) -> Result<usize> {
        let mut bytes = [0; 8];
        bytes[8 - width..].copy_from_slice(self.take(width)?);
        usize::try_from(u64::from_be_bytes(bytes))
            .map_err(|_| Error::new("membership record is too long"))
    }
    fn finish(&self) -> Result<()> {
        if self.at == self.bytes.len() {
            Ok(())
        } else {
            Err(Error::new("membership record has trailing bytes"))
        }
    }
}

/// What [`Log::insert`] did with a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inserted {
    /// It is in the log now, and so is every waiting record it was the last missing parent of.
    Held,
    /// The log already held it.
    Known,
    /// A parent is missing: the log keeps it aside until the parent arrives.
    Waiting,
}

/// The records one Mac holds of one network, each after all its parents.
///
/// The log only grows. A record whose parents have not all arrived waits beside it and lands
/// when the last one does, so the log a set of records gives does not depend on the order they
/// came in: a rejoin is [`Log::merge`], and no merge record is needed.
#[derive(Clone, Debug)]
pub struct Log {
    /// Every held record, each after its parents; the genesis first.
    order: Vec<Record>,
    held: BTreeMap<RecordId, usize>,
    waiting: BTreeMap<RecordId, Record>,
    /// The bytes of `order`'s records, kept as they change: by [`Log::hold`] alone, since
    /// `order` only grows.
    held_bytes: usize,
    /// The bytes of `waiting`'s records, kept by every change to `waiting`: the waiting insert,
    /// [`Log::land_waiting`]'s remove and [`Log::drop_last_waiting`]. Summing either set on each
    /// insert walked every record, and a store at its bound repeated that walk for each record
    /// it let go (`docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1 F23, rule 6).
    waiting_bytes: usize,
    /// The budgets `order` stays within, fixed when the log was made.
    budgets: RecordBudgets,
    /// How many of `order`'s records count against each budget, kept by [`Log::hold`].
    counts: RecordCounts,
    enforce_seats: bool,
    /// Each held record's strict causal past, extended by [`Log::hold`]. Clones share its rows
    /// until `hold` extends one of them ([`reach`]).
    reach: reach::Reach,
    /// What seats each Mac, kept by every change to `order` and `waiting`, so a record's seat
    /// checks read the Mac's own entries rather than every record.
    seats: BTreeMap<MemberId, Seating>,
    // Clones hold the same immutable prefix until `hold` extends one of them.
    // Its pure evaluation is shared too; no disk observation or grant is cached.
    membership: Arc<OnceLock<Membership>>,
    effective: Arc<OnceLock<eval::Effective>>,
    /// The quorum weighing of the held weighed records ([`eval::Weighing`]), which every
    /// evaluation reads: `hold` lets it go only when it holds a weighed record, since a record of
    /// another kind changes nothing it weighed. Clones share it until one holds a weighed record.
    weighing: Arc<OnceLock<eval::Weighing>>,
    /// What the counting epochs and wraps say and the verdicts taken from them ([`epochs`]):
    /// `hold` lets it go only when it holds an `Epoch`, a `Wrap` or a weighed record, since
    /// nothing it read changes otherwise. Clones share it until one holds such a record.
    epochs: Arc<OnceLock<epochs::Epochs>>,
}

/// What seats one Mac in a log: the genesis binding it as a founder, the held `Admit`s another Mac
/// signed that let it in (record indexes, in order), and how many such `Admit`s wait for a parent.
#[derive(Clone, Debug, Default)]
struct Seating {
    founder: bool,
    admits: Vec<usize>,
    waiting: usize,
}

impl Seating {
    const fn is_empty(&self) -> bool {
        !self.founder && self.admits.is_empty() && self.waiting == 0
    }
}

/// The Mac `record` seats: the entrant of an `Admit` another Mac signed.
fn seated_by(record: &Record) -> Option<MemberId> {
    match record.body() {
        Body::Admit { entrant, .. } if record.actor() != &entrant.member() => {
            Some(entrant.member())
        }
        _ => None,
    }
}

impl Log {
    /// A log of the network `genesis` founds, within [`RecordBudgets::current`].
    ///
    /// # Errors
    /// `genesis` is not a genesis.
    pub fn new(genesis: Record) -> Result<Self> {
        if genesis.kind() != Kind::Genesis {
            return Err(Error::new("a membership log starts at a genesis"));
        }
        let held = BTreeMap::from([(genesis.id, 0)]);
        let held_bytes = genesis.bytes().len();
        let mut reach = reach::Reach::default();
        reach.push(std::iter::empty());
        let mut seats: BTreeMap<MemberId, Seating> = BTreeMap::new();
        if let Body::Genesis { founders, .. } = genesis.body() {
            for member in founders.iter().filter_map(Founder::member) {
                seats.entry(member).or_default().founder = true;
            }
        }
        let counts = budgets::counted(genesis.kind());
        Ok(Self {
            order: vec![genesis],
            held,
            waiting: BTreeMap::new(),
            held_bytes,
            waiting_bytes: 0,
            budgets: RecordBudgets::current(),
            counts,
            enforce_seats: false,
            reach,
            seats,
            membership: Arc::default(),
            effective: Arc::default(),
            weighing: Arc::default(),
            epochs: Arc::default(),
        })
    }

    /// Restrict a verified graph to the causal past of held heads, preserving its budgets
    /// and seat checks. The new graph starts with empty evaluator caches.
    #[cfg(unix)]
    pub(crate) fn project_heads(&self, heads: &[RecordId]) -> Result<Self> {
        let mut records = self.ancestor_closure(heads).into_iter();
        let first = records
            .next()
            .ok_or_else(|| Error::new("membership projection has no genesis"))?;
        let mut projected = Self::new(first)?;
        projected.budgets = self.budgets;
        projected.enforce_seats = self.enforce_seats;
        for record in records {
            let _ = projected.insert(record)?;
        }
        Ok(projected)
    }

    /// What the evaluator reads of this log.
    fn history(&self) -> eval::History<'_> {
        eval::History {
            records: &self.order,
            at: &self.held,
            reach: &self.reach,
        }
    }

    /// The indexes of the held records among `ids`, in order; ids not held are skipped.
    fn indexes(&self, ids: &[RecordId]) -> Vec<usize> {
        ids.iter()
            .filter_map(|id| self.held.get(id).copied())
            .collect()
    }

    /// Whether the held record `target` is one of the held records `from` or lies in one's past.
    fn reaches(&self, from: &[RecordId], target: &RecordId) -> bool {
        self.held
            .get(target)
            .is_some_and(|&t| self.reach.in_past_of(t, &self.indexes(from)))
    }

    /// Refuse records whose signer has no founding or admitted seat in their causal past.
    pub(super) const fn require_seats(&mut self) {
        self.enforce_seats = true;
    }

    /// The network id: the genesis record's id.
    pub fn network(&self) -> &RecordId {
        self.order[0].id()
    }
    /// The genesis record.
    pub fn genesis(&self) -> &Record {
        &self.order[0]
    }
    /// A held record.
    pub fn get(&self, id: &RecordId) -> Option<&Record> {
        self.held.get(id).map(|at| &self.order[*at])
    }
    /// The topological index of a held record (`0` for the genesis, each record after its parents).
    #[must_use]
    pub fn record_index(&self, id: &RecordId) -> Option<usize> {
        self.held.get(id).copied()
    }
    /// Every held record, each after its parents.
    pub fn records(&self) -> impl Iterator<Item = &Record> {
        self.order.iter()
    }
    /// How many records wait for a parent.
    pub fn waiting(&self) -> usize {
        self.waiting.len()
    }
    /// How many records it holds, each after its parents: the length of [`Log::records`].
    pub fn held_count(&self) -> usize {
        self.order.len()
    }
    /// How many of its held records count against each budget.
    #[must_use]
    pub const fn counts(&self) -> RecordCounts {
        self.counts
    }
    /// The budgets it holds its records within.
    #[must_use]
    pub const fn budgets(&self) -> RecordBudgets {
        self.budgets
    }
    /// The first bound that holding `more` records of `more_bytes` wire bytes past what it holds
    /// would pass ([`RecordBudgets::passed_with_bytes`]); `None` where they fit every one.
    #[must_use]
    pub fn passed_by(&self, more: RecordCounts, more_bytes: usize) -> Option<Budget> {
        self.budgets
            .passed_with_bytes(self.counts, more, self.held_bytes, more_bytes)
    }
    /// The held records no held record follows, sorted.
    pub fn heads(&self) -> Vec<RecordId> {
        let followed: BTreeSet<&RecordId> =
            self.order.iter().flat_map(|r| r.parents().iter()).collect();
        let mut heads: Vec<RecordId> = self
            .order
            .iter()
            .map(|r| r.id)
            .filter(|id| !followed.contains(id))
            .collect();
        heads.sort_unstable();
        heads
    }

    /// A draft by `actor` that follows the heads: its sequence one past `actor`'s highest held
    /// record, whose id is its previous and one of its parents. Past [`MAX_PARENTS`] parents it
    /// follows the lowest heads, and the next draft takes up the rest. [`Draft::sign`] it with
    /// `actor`'s key.
    pub fn draft(&self, actor: &MemberId, authored_ms: u64, body: Body) -> Draft {
        self.draft_following(actor, authored_ms, body, &[])
    }

    /// [`Log::draft`], with each held record of `follow` in the draft's causal past as well: it
    /// pins the head that reaches each, as it pins a `Put`'s epoch (F21's reseal follows the
    /// writes it retires). A record whose head finds no room among [`MAX_PARENTS`] pinned
    /// parents is left out.
    pub(crate) fn draft_following(
        &self,
        actor: &MemberId,
        authored_ms: u64,
        body: Body,
        follow: &[RecordId],
    ) -> Draft {
        let previous = self
            .order
            .iter()
            .filter(|r| r.kind() != Kind::Genesis && r.actor() == actor)
            .max_by(|a, b| a.sequence().cmp(&b.sequence()).then(b.id.cmp(&a.id)));
        let heads = self.heads();
        let mut parents: Vec<RecordId> = previous.map(|r| r.id).into_iter().collect();
        let room = MAX_PARENTS - parents.len();
        parents.extend(
            heads
                .iter()
                .filter(|&head| Some(head) != previous.map(|r| &r.id))
                .take(room)
                .copied(),
        );
        let seat = self
            .membership()
            .get(actor)
            .map(|m| m.entry)
            .or_else(|| {
                self.order.iter().rev().find_map(|r| match r.body() {
                    Body::Admit { entrant, .. } if entrant.member() == *actor => Some(r.id),
                    _ => None,
                })
            })
            .filter(|id| id != self.network());
        let body_ref = match &body {
            Body::Remove { cutoff, .. } => Some(*cutoff),
            Body::Wrap { epoch, .. } | Body::Put { epoch, .. } | Body::Delete { epoch, .. } => {
                Some(*epoch)
            }
            _ => None,
        };
        let mut pinned: Vec<RecordId> = previous.map(|r| r.id).into_iter().collect();
        for needed in [seat, body_ref]
            .into_iter()
            .flatten()
            .chain(follow.iter().copied())
        {
            if self.held.contains_key(&needed) && !self.reaches(&parents, &needed) {
                let target = heads
                    .iter()
                    .copied()
                    .find(|head| self.reaches(std::slice::from_ref(head), &needed))
                    .unwrap_or(needed);
                if !parents.contains(&target) {
                    if parents.len() >= MAX_PARENTS
                        && let Some(pos) = parents.iter().rposition(|p| !pinned.contains(p))
                    {
                        parents.remove(pos);
                    }
                    if parents.len() >= MAX_PARENTS {
                        continue;
                    }
                    parents.push(target);
                }
                pinned.push(target);
            } else if !parents
                .iter()
                .any(|p| pinned.contains(p) && self.reaches(std::slice::from_ref(p), &needed))
                && let Some(&covering) = parents
                    .iter()
                    .find(|&p| self.reaches(std::slice::from_ref(p), &needed))
            {
                // A parent already pinned that reaches it keeps it; else the first that does.
                pinned.push(covering);
            }
        }
        Draft {
            network: *self.network(),
            sequence: previous.map_or(1, |r| r.sequence() + 1),
            previous: previous.map(|r| r.id),
            parents,
            authored_ms,
            body,
        }
    }

    /// Take a decoded record.
    ///
    /// A record whose parents are all held is checked against them and held. One with a
    /// parent missing waits, and is checked and held when the last one arrives; one that fails
    /// that check then is dropped. In a store's log only a signer that holds a seat already waits.
    ///
    /// # Errors
    /// The record is another network's, or another genesis; its previous is not its signer's
    /// record at the sequence before; an `Admit`'s entrant is bound under another Commons root
    /// than the genesis's, or one of [`Evidence::Listed`] names no founder the genesis lists with
    /// no binding under the label it carries; a removal's cutoff is not in its causal past, or
    /// is neither a record of the Mac it removes nor the record that let that Mac in; or a `Wrap`,
    /// `Put` or `Delete` names an `epoch` that is not a held [`Body::Epoch`] (for its audience, on
    /// a `Put` or `Delete`) in its causal past. In a store's log it fails when the signer has no
    /// seat in the record's causal past, or, for a record whose parents are missing, no seat in
    /// the held log. It also fails, as [`crate::ErrorKind::MembershipCapacity`], when holding the
    /// record would pass one of the log's budgets ([`Log::passed_by`]: its held total, the
    /// budget of the record's kind, [`Kind::weighed`] or not, or [`MAX_RECORDS_STREAM`] bytes),
    /// or when keeping it waiting would pass [`MAX_WAITING`] waiting records or make the log's
    /// wire bytes exceed [`MAX_RECORDS_STREAM`].
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "Membership",
            action = "Sign1",
            project = "aterm_secrets::membership_log::tests::membership_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "Membership",
            action = "Sign2",
            project = "aterm_secrets::membership_log::tests::membership_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "Membership",
            action = "Sign3",
            project = "aterm_secrets::membership_log::tests::membership_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "Membership",
            action = "Deliver1",
            project = "aterm_secrets::membership_log::tests::membership_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "Membership",
            action = "Deliver2",
            project = "aterm_secrets::membership_log::tests::membership_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "Membership",
            action = "Deliver3",
            project = "aterm_secrets::membership_log::tests::membership_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "SwarmRecordsCapacity",
            action = "AddWeighed",
            project = "aterm_secrets::client::readmit::tests::records_capacity_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "SwarmRecordsCapacity",
            action = "AddKeyed",
            project = "aterm_secrets::client::readmit::tests::records_capacity_transition"
        )
    )]
    pub fn insert(&mut self, record: Record) -> Result<Inserted> {
        if record.kind() == Kind::Genesis {
            return if record.id == *self.network() {
                Ok(Inserted::Known)
            } else {
                Err(Error::new("a record of another network's genesis"))
            };
        }
        if record.network() != self.network() {
            return Err(Error::new("a membership record of another network"));
        }
        if self.held.contains_key(&record.id) {
            return Ok(Inserted::Known);
        }
        if self.waiting.contains_key(&record.id) {
            return Ok(Inserted::Waiting);
        }
        if !record.parents().iter().all(|p| self.held.contains_key(p)) {
            // In a store's log a record waits only for a signer that already holds a seat: a key
            // no founder or `Admit` names cannot fill the waiting room with records whose made-up
            // parents never arrive. A peer sends every seat before the records that need it
            // (`Store::missing`), so this refuses none of a peer's.
            if self.enforce_seats && !self.holds_seat(record.actor()) {
                return Err(Error::new(
                    "a record waits for its parents only when its signer holds a seat in the log",
                ));
            }
            if self.waiting.len() >= MAX_WAITING
                || self.total_bytes().saturating_add(record.bytes().len()) > MAX_RECORDS_STREAM
            {
                return Err(Error::of_kind(
                    crate::ErrorKind::MembershipCapacity,
                    "membership log holds its limit of waiting records",
                ));
            }
            self.wait(record);
            return Ok(Inserted::Waiting);
        }
        if let Some(budget) = self.passed_by(budgets::counted(record.kind()), record.bytes().len())
        {
            return Err(Error::of_kind(
                crate::ErrorKind::MembershipCapacity,
                format!("membership log holds its limit of {}", budget.counts()),
            ));
        }
        self.check_against_parents(&record)?;
        self.hold(record);
        self.land_waiting();
        Ok(Inserted::Held)
    }

    /// Insert every record `other` holds or keeps waiting. A record `other` holds that this log
    /// refuses is skipped and the rest come in: both logs check records the same way, so that
    /// happens only at this log's bounds (its [`RecordBudgets`], [`MAX_WAITING`]), which depend on
    /// what reached each log first.
    ///
    /// # Errors
    /// `other` is another network's log.
    pub fn merge(&mut self, other: &Log) -> Result<()> {
        if other.network() != self.network() {
            return Err(Error::new("cannot merge another network's membership log"));
        }
        for record in other.order.iter().skip(1) {
            let _ = self.insert(record.clone());
        }
        loop {
            let mut progressed = false;
            for record in other.waiting.values() {
                if !self.held.contains_key(&record.id)
                    && !self.waiting.contains_key(&record.id)
                    && self.insert(record.clone()).is_ok()
                {
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }
        Ok(())
    }

    /// An owned membership snapshot as this log says it: the pure evaluator of §3.2.
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "Membership",
            action = "Evaluate1",
            project = "aterm_secrets::membership_log::tests::membership_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "Membership",
            action = "Evaluate2",
            project = "aterm_secrets::membership_log::tests::membership_transition"
        )
    )]
    pub fn membership(&self) -> Membership {
        self.membership_ref().clone()
    }

    /// Borrow this immutable prefix's evaluation for graph queries that retain no snapshot.
    fn membership_ref(&self) -> &Membership {
        self.membership
            .get_or_init(|| eval::evaluate_with(self.history(), self.weighing()))
    }

    /// Whether this log takes the Mac of `binding` out ([`Membership::seat_removed`]): the one
    /// removal predicate the peer check, a witness's judgement and enrollment share.
    #[must_use]
    pub fn seat_removed(&self, binding: &Binding) -> bool {
        self.membership_ref().seat_removed(binding)
    }

    /// This log with `membership` as its evaluation, for a test of a caller's judgement on a
    /// membership no record set here gives (the Unix client's enrollment tests).
    #[cfg(all(test, unix))]
    pub(crate) fn with_membership(mut self, membership: Membership) -> Self {
        self.membership = Arc::new(OnceLock::from(membership));
        self
    }

    fn effective(&self) -> &eval::Effective {
        self.effective
            .get_or_init(|| eval::effective_records(self.history(), self.weighing()))
    }

    /// What the counting epochs and wraps say, read once while no `Epoch`, `Wrap` or weighed
    /// record arrives.
    fn epochs(&self) -> &epochs::Epochs {
        self.epochs.get_or_init(|| epochs::Epochs::of(self))
    }

    /// The quorum weighing of the held records, weighed once while no weighed record arrives.
    fn weighing(&self) -> &eval::Weighing {
        self.weighing.get_or_init(|| eval::weigh(self.history()))
    }

    /// The cutoff of the quorum `Remove` of `actor` that F16's "Undo <actor>'s removals" row
    /// signs a share of (§3.2, §3.3.7): `actor`'s last record before its first contested act, an
    /// `Admit` or a `Remove` of another Mac that no independent Mac built on before it saw a later
    /// act, else the record that let `actor` in. A Mac `actor`'s `Admit`s let in, directly or
    /// through Macs those let in, is not independent, and a record that removes `actor` or follows
    /// a removal of it contests rather than builds on. Where the Macs `actor` removed after that
    /// cutoff could not make the quorum there, the acknowledgments at its highest acts are set
    /// aside until they can, so a Mac left in to carry on and then removed never strands the lab.
    /// Every honest Mac that holds the same records computes the same one, so their shares make
    /// one quorum, and `Admit`s `actor` made where nobody saw them fall after it, so those Macs
    /// never vote. `None` where `actor` has no contested act or no entry in this log.
    #[must_use]
    pub fn undo_cutoff(&self, actor: &MemberId) -> Option<RecordId> {
        eval::undo_cutoff(self.history(), self.weighing(), actor)
    }

    /// Where the quorum `Remove` of `actor` at [`Log::undo_cutoff`] stands: its voters, the
    /// voters whose share this log holds, and whether it took effect. `None` where there is no
    /// such cutoff.
    #[must_use]
    pub fn undo(&self, actor: &MemberId) -> Option<Undo> {
        let mut undo = eval::undo(self.history(), self.weighing(), actor)?;
        undo.took_effect = self
            .membership()
            .quorums()
            .iter()
            .any(|q| q.member == *actor && q.cutoff == undo.cutoff);
        Some(undo)
    }

    /// Return the minimal topologically ordered causal ancestor subgraph ending at `target`
    /// (`Genesis` first, `target` included), or `None` when `target` is not held in this log.
    #[must_use]
    pub fn ancestor_chain(&self, target: &RecordId) -> Option<Vec<Record>> {
        if !self.held.contains_key(target) {
            return None;
        }
        Some(self.ancestor_closure(std::slice::from_ref(target)))
    }

    /// Every held record in the causal closure of `targets` (`Genesis` first, `targets` included),
    /// in deterministic topological replay order (`F18`).
    #[must_use]
    pub fn ancestor_closure(&self, targets: &[RecordId]) -> Vec<Record> {
        let held_targets = self.indexes(targets);
        if held_targets.is_empty() {
            return Vec::new();
        }
        let keep = self.reach.closure(held_targets);
        self.order
            .iter()
            .enumerate()
            .filter(|(i, _)| (keep[i / 64] >> (i % 64)) & 1 == 1)
            .map(|(_, r)| r.clone())
            .collect()
    }

    /// Founding [`Body::Genesis`] or effective [`Body::Admit`] [`RecordId`] that grants `binding`
    /// a seat in this log, or `None` when `binding` holds no effective seat (or was voided out for
    /// good by a quorum `Remove`).
    #[must_use]
    pub fn historical_seat_entry(&self, binding: &Binding) -> Option<RecordId> {
        eval::historical_seat_entry(self.history(), self.weighing(), binding)
    }

    /// Whether `binding` holds a founding or effective admitted seat in this log (even if later
    /// retired by an ordinary `Remove`, unless voided out for good by a quorum `Remove`).
    #[must_use]
    pub fn has_historical_seat(&self, binding: &Binding) -> bool {
        eval::has_historical_seat(self.history(), self.weighing(), binding)
    }

    /// Whether `actor` holds a founding or non-void, authorized, non-cut-off admitted seat in
    /// this log (including actors later retired by an ordinary or quorum `Remove`).
    #[must_use]
    pub fn has_known_actor(&self, actor: &MemberId) -> bool {
        eval::has_known_actor(self.history(), self.weighing(), actor)
    }

    /// Historical Commons writer bindings eligible to authenticate table records sealed under
    /// `epoch`, together with `epoch`'s generation, or `None` when `epoch` is not an effective
    /// [`EpochAudience::Commons`] [`Body::Epoch`] in this log.
    #[must_use]
    pub fn historical_commons_writers(&self, epoch: &RecordId) -> Option<(u64, Vec<Binding>)> {
        let &idx = self.held.get(epoch)?;
        let Body::Epoch {
            audience: EpochAudience::Commons,
            generation,
            ..
        } = self.order[idx].body()
        else {
            return None;
        };
        let writers = eval::historical_bindings_for_epoch(self.history(), self.weighing(), idx)?;
        Some((*generation, writers))
    }

    fn check_against_parents(&self, record: &Record) -> Result<()> {
        let parents = self.indexes(record.parents());
        if self.enforce_seats && !self.has_seat_in_past(record, &parents) {
            return Err(Error::new(
                "a membership record's signer has no seat in its causal past",
            ));
        }
        if let Some(previous) = record.previous() {
            let before = self.get(previous).filter(|p| p.kind() != Kind::Genesis);
            if !before.is_some_and(|p| {
                p.actor() == record.actor() && p.sequence() + 1 == record.sequence()
            }) {
                return Err(Error::new(
                    "a record's previous is its signer's record at the sequence before",
                ));
            }
        }
        if let Body::Admit { entrant, evidence } = record.body() {
            let Body::Genesis { root, founders, .. } = self.genesis().body() else {
                unreachable!("a log starts at a genesis");
            };
            let binding = entrant.request.binding();
            if !root.names(binding) {
                return Err(Error::new(
                    "an entrant's binding names its network's Commons root",
                ));
            }
            // The one admission no person makes: a founder the genesis lists, as it lists it.
            if *evidence == Evidence::Listed
                && !founders.iter().any(|f| {
                    f.binding.is_none()
                        && f.recipient == binding.recipient()
                        && f.label == entrant.label
                })
            {
                return Err(Error::new(
                    "a listed admission names a founder the genesis lists with no binding, under its label",
                ));
            }
        }
        if let Body::Remove { member, cutoff } = record.body() {
            if !self.precedes(cutoff, &parents) {
                return Err(Error::new(
                    "a removal's cutoff is a record in its own causal past",
                ));
            }
            // §3.3.7: the removed Mac's last record the remover accepted, else the record that
            // let it in. A cutoff elsewhere would let a removed Mac pick one that leaves its own
            // removal out of view.
            let names_it = self.get(cutoff).is_some_and(|at| match at.body() {
                Body::Genesis { founders, .. } => {
                    founders.iter().any(|f| f.member() == Some(*member))
                }
                Body::Admit { entrant, .. } => at.actor() == member || entrant.member() == *member,
                _ => at.actor() == member,
            });
            if !names_it {
                return Err(Error::new(
                    "a removal's cutoff is the removed Mac's record or the one that let it in",
                ));
            }
        }
        match record.body() {
            Body::Wrap { epoch, .. }
                if !self.precedes(epoch, &parents)
                    || !matches!(self.get(epoch).map(Record::body), Some(Body::Epoch { .. })) =>
            {
                return Err(Error::new("a Wrap names an Epoch in its causal past"));
            }
            Body::Put {
                audience, epoch, ..
            }
            | Body::Delete {
                audience, epoch, ..
            } if !self.precedes(epoch, &parents)
                || !matches!(
                    self.get(epoch).map(Record::body),
                    Some(Body::Epoch { audience: ep_aud, .. }) if ep_aud == audience
                ) =>
            {
                return Err(Error::new(
                    "a Put or Delete names an Epoch for its audience in its causal past",
                ));
            }
            _ => {}
        }
        Ok(())
    }

    /// Whether `actor` holds a seat anywhere in the log: a founder the genesis binds, or the Mac
    /// a held or waiting `Admit` another Mac signed lets in.
    fn holds_seat(&self, actor: &MemberId) -> bool {
        self.seats.get(actor).is_some_and(|seat| !seat.is_empty())
    }

    /// Whether `record`'s signer is a founder the genesis binds, or a held `Admit` another Mac
    /// signed lets it in within the causal past of the held records at `parents` (`record`'s).
    fn has_seat_in_past(&self, record: &Record, parents: &[usize]) -> bool {
        self.seats.get(record.actor()).is_some_and(|seat| {
            seat.founder
                || seat
                    .admits
                    .iter()
                    .any(|&admit| self.reach.in_past_of(admit, parents))
        })
    }

    /// Whether the held record `ancestor` lies in the causal past of a record whose parents are
    /// the held records at `parents`.
    fn precedes(&self, ancestor: &RecordId, parents: &[usize]) -> bool {
        self.held
            .get(ancestor)
            .is_some_and(|&at| self.reach.in_past_of(at, parents))
    }

    /// The bytes of every held record: what the records of `order` take on the wire, and what a
    /// server's records file of complete records holds (`server::apply_records`).
    pub(crate) const fn held_bytes(&self) -> usize {
        self.held_bytes
    }

    /// The bytes of every record waiting on a parent.
    pub(super) const fn waiting_bytes(&self) -> usize {
        self.waiting_bytes
    }

    pub(super) fn total_bytes(&self) -> usize {
        self.held_bytes().saturating_add(self.waiting_bytes())
    }

    /// Wire bytes of held and waiting records plus the reachability index's bytes.
    /// A cache of decoded logs counts both rather than only the input wire.
    #[must_use]
    pub fn footprint(&self) -> usize {
        self.total_bytes().saturating_add(self.reach.bytes())
    }

    fn hold(&mut self, record: Record) {
        // The evaluation goes first: an `Effective` shares the index's chunks, and a lone log
        // whose cache let go of it extends its last chunk in place.
        self.membership = Arc::default();
        self.effective = Arc::default();
        #[cfg(test)]
        let carried = eval::MUTANT.get() == Some(eval::Mutant::CarryAcross(record.kind()));
        #[cfg(not(test))]
        let carried = false;
        if record.kind().weighed() && !carried {
            self.weighing = Arc::default();
        }
        if epochs::resets(record.kind()) {
            self.epochs = Arc::default();
        }
        let at = self.order.len();
        self.reach
            .push(record.parents().iter().map(|parent| self.held[parent]));
        if let Some(member) = seated_by(&record) {
            self.seats.entry(member).or_default().admits.push(at);
        }
        self.held.insert(record.id, at);
        self.held_bytes = self.held_bytes.saturating_add(record.bytes().len());
        self.counts = self.counts.plus(budgets::counted(record.kind()));
        self.order.push(record);
    }

    /// Keep `record` waiting for a parent, with [`Log::waiting_bytes`] and its seat.
    fn wait(&mut self, record: Record) {
        self.waiting_bytes = self.waiting_bytes.saturating_add(record.bytes().len());
        if let Some(member) = seated_by(&record) {
            self.seats.entry(member).or_default().waiting += 1;
        }
        self.waiting.insert(record.id, record);
    }

    /// Account for `record` no longer waiting: [`Log::waiting_bytes`] and its seat.
    fn unwaited(&mut self, record: &Record) {
        self.waiting_bytes = self.waiting_bytes.saturating_sub(record.bytes().len());
        if let Some(member) = seated_by(record)
            && let Some(seat) = self.seats.get_mut(&member)
        {
            seat.waiting -= 1;
            if seat.is_empty() {
                self.seats.remove(&member);
            }
        }
    }

    /// Hold every waiting record whose parents have all arrived, pass by pass, until a pass
    /// lands none. A pass takes the records ready at its start in id order, so the result is the
    /// same whatever order records came in. A ready record leaves the waiting room whether it
    /// lands or not: one that fails its check against its parents, or would pass a budget
    /// ([`Log::passed_by`]), could never land later, since the held log only grows and the
    /// record's id fixes its causal past. Kept waiting it would be ready again on every pass, and
    /// a waiting `Admit` would still give its Mac a seat ([`Log::holds_seat`]).
    fn land_waiting(&mut self) {
        loop {
            let ready: Vec<RecordId> = self
                .waiting
                .values()
                .filter(|r| r.parents().iter().all(|p| self.held.contains_key(p)))
                .map(|r| r.id)
                .collect();
            #[cfg(test)]
            let ready = budgets::mutant_order(ready);
            let mut landed = false;
            for id in ready {
                let Some(record) = self.waiting.remove(&id) else {
                    continue;
                };
                self.unwaited(&record);
                if self
                    .passed_by(budgets::counted(record.kind()), record.bytes().len())
                    .is_none()
                    && self.check_against_parents(&record).is_ok()
                {
                    self.hold(record);
                    landed = true;
                    continue;
                }
                #[cfg(test)]
                budgets::mutant_keeps_unfit(self, record);
            }
            if !landed {
                return;
            }
        }
    }

    /// Let the waiting record with the highest id go, keeping [`Log::waiting_bytes`] and the
    /// seat it gave; `false` when none waits. A store at its bound calls it to make room
    /// (`store::make_room`).
    pub(super) fn drop_last_waiting(&mut self) -> bool {
        let Some((_, record)) = self.waiting.pop_last() else {
            return false;
        };
        self.unwaited(&record);
        true
    }
}

/// Host for `Membership`'s environment markers, so `spec_xref_closure` accounts for every model
/// action. The scenario actions choose which records the model's Macs sign; the library never
/// chooses them.
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "Membership",
        action = "Labels",
        reason = "Environment: three Macs admitted concurrently under one label"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "Membership",
        action = "RemoveVsAdmit",
        reason = "Environment: one member admits a Mac while another removes it"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "Membership",
        action = "Mutual",
        reason = "Environment: two members remove each other on two islands"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "Membership",
        action = "Cutoff",
        reason = "Environment: a removed member admits a Mac before it hears of its removal"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "Membership",
        action = "MassRemove",
        reason = "Environment: one member removes every other, and two sign a quorum remove"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "Membership",
        action = "LoneShare",
        reason = "Environment: a removed Mac alone signs a quorum share against its remover"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "Membership",
        action = "KeyedWrite",
        reason = "Environment: a removed Mac mints an epoch past its cutoff and a member writes under it"
    )
)]
#[doc(hidden)]
pub fn membership_log_environment() {}

/// `record`'s bytes with its kind byte set to `code`: a test's stand-in for a record of a kind
/// only a newer build writes, which this build frames ([`Record::frame_prefix`]) and refuses to
/// read ([`Record::decode`]).
#[cfg(all(test, unix))]
pub(crate) fn with_kind_code(record: &Record, code: u8) -> Vec<u8> {
    let mut wire = record.bytes().to_vec();
    wire[KIND_AT] = code;
    wire
}

#[cfg(test)]
mod tests;
