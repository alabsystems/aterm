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
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use std::collections::{BTreeMap, BTreeSet};

pub mod admission;
pub mod entries;
mod eval;
pub mod genesis;
pub mod store;

pub use admission::{Gap, Signer, admission_audiences};
pub use astream_wire::sync::MAX_PARENTS;
pub use entries::{CommonsEpochView, OpenedAudienceEntries, OpenedEntries, epoch_commitment};
pub use eval::{Member, Membership, NodeStanding, Quorum, Removal};
pub use genesis::Proof;
pub use store::{MAX_RECORDS_STREAM, Store, decode_records, encode_records};

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
const SIGNING_DOMAIN: &[u8] = b"aterm/members/record/1\0";
/// The largest record [`Record::decode`] reads: a 1 MiB secret value (`Put`) plus headers fits
/// with room.
pub const MAX_RECORD: usize = crate::text::MAX_VALUE + 64 * 1024;
/// The most held records a [`Log`] or [`Store`] keeps, matching Commons control's inventory bound.
pub const MAX_RECORDS: usize = 4096;
/// The most records waiting for a missing parent that a [`Log`] or [`Store`] keeps.
pub const MAX_WAITING: usize = 4096;
#[cfg(unix)]
const _: () = assert!(MAX_RECORDS == crate::commons::journal::MAX_NODES);
#[cfg(unix)]
const _: () = assert!(MAX_WAITING == crate::commons::journal::MAX_NODES);
/// The longest host name a record carries, in bytes.
pub const MAX_NAME: usize = 64;
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
}

/// How the admitting member knew a person meant this Mac (§3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Evidence {
    /// A person pressed Allow and typed the phrase the member showed (§3.3.4).
    Phrase,
    /// A person ran `aterm secrets approve <code>` on the member.
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
        capsule: Vec<u8>,
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
        ciphertext: Vec<u8>,
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
        ciphertext: Vec<u8>,
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

/// One signed record of the membership log.
#[derive(Clone, Debug)]
pub struct Record {
    id: RecordId,
    header: Header,
    body: Body,
    wire: Vec<u8>,
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
        let body = decode_body(kind, reader.take(length)?)?;
        let body_end = reader.at;
        let signature = reader.take(64)?;
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
        UnparsedPublicKey::new(&ED25519, key)
            .verify(&signed_message(&wire[..body_end]), signature)
            .map_err(|_| Error::new("membership record signature failed"))?;
        Ok(Self {
            id: Sha256::digest(wire),
            header,
            body,
            wire: wire.to_vec(),
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
    pub const fn kind(&self) -> Kind {
        self.header.kind
    }
    /// What the record says.
    pub const fn body(&self) -> &Body {
        &self.body
    }
    /// The network it belongs to; all zero for a genesis, whose own id is the network's.
    pub const fn network(&self) -> &RecordId {
        &self.header.network
    }
    /// The network's genesis id: [`Record::id`] for a genesis, else [`Record::network`].
    pub const fn network_id(&self) -> &RecordId {
        match self.header.kind {
            Kind::Genesis => &self.id,
            _ => &self.header.network,
        }
    }
    /// The exact byte length of the first record at the start of `wire`, bounded by [`MAX_RECORD`].
    pub(super) fn wire_len_prefix(wire: &[u8]) -> Result<usize> {
        let mut reader = Reader::new(wire);
        if reader.take(4)? != MAGIC || reader.byte()? != VERSION {
            return Err(Error::new("not a membership record"));
        }
        let _network = reader.take(32)?;
        let _kind = Kind::from_code(reader.byte()?)?;
        let _actor = reader.take(48)?;
        let _sequence = reader.take(8)?;
        let _previous = reader.take(32)?;
        let count = usize::from(reader.byte()?);
        if count > MAX_PARENTS {
            return Err(Error::new("membership record names too many parents"));
        }
        let _parents = reader.take(count * 32)?;
        let _authored_ms = reader.take(8)?;
        let body_len = reader.len(4)?;
        let _body = reader.take(body_len)?;
        let _sig = reader.take(64)?;
        if reader.at > MAX_RECORD {
            return Err(Error::new("membership record is too long"));
        }
        Ok(reader.at)
    }
    /// The signing member; all zero for a genesis.
    pub const fn actor(&self) -> &MemberId {
        &self.header.actor
    }
    /// The signer's sequence.
    pub const fn sequence(&self) -> u64 {
        self.header.sequence
    }
    /// The signer's previous record.
    pub const fn previous(&self) -> Option<&RecordId> {
        self.header.previous.as_ref()
    }
    /// The records it follows, sorted.
    pub fn parents(&self) -> &[RecordId] {
        &self.header.parents
    }
    /// The signer's claimed wall clock, in UTC milliseconds.
    pub const fn authored_ms(&self) -> u64 {
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
    if header.network == [0; 32] || header.actor[..32] == [0; 32] {
        return Err(Error::new(
            "a membership record names its network and signer",
        ));
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
        Body::Node { node_key, proof } => {
            if *node_key == [0; 32] {
                return Err(Error::new("a node record's proof is not its node key's"));
            }
            UnparsedPublicKey::new(&ED25519, node_key)
                .verify(&node_proof_message(&header.network, &header.actor), proof)
                .map_err(|_| Error::new("a node record's proof is not its node key's"))?;
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

fn decode_body(kind: Kind, bytes: &[u8]) -> Result<Body> {
    let mut reader = Reader::new(bytes);
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
            let capsule = reader.take(len)?.to_vec();
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
            let ciphertext = reader.take(len)?.to_vec();
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
            let ciphertext = reader.take(len)?.to_vec();
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
    let Some(digits) = label.strip_prefix('m') else {
        return false;
    };
    (1..=5).contains(&digits.len())
        && !digits.starts_with('0')
        && digits.bytes().all(|b| b.is_ascii_digit())
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
    enforce_seats: bool,
}

impl Log {
    /// A log of the network `genesis` founds.
    ///
    /// # Errors
    /// `genesis` is not a genesis.
    pub fn new(genesis: Record) -> Result<Self> {
        if genesis.kind() != Kind::Genesis {
            return Err(Error::new("a membership log starts at a genesis"));
        }
        let held = BTreeMap::from([(genesis.id, 0)]);
        Ok(Self {
            order: vec![genesis],
            held,
            waiting: BTreeMap::new(),
            enforce_seats: false,
        })
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
    /// Every held record, each after its parents.
    pub fn records(&self) -> impl Iterator<Item = &Record> {
        self.order.iter()
    }
    /// How many records wait for a parent.
    pub fn waiting(&self) -> usize {
        self.waiting.len()
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
        let previous = self
            .order
            .iter()
            .filter(|r| r.kind() != Kind::Genesis && r.actor() == actor)
            .max_by(|a, b| a.sequence().cmp(&b.sequence()).then(b.id.cmp(&a.id)));
        let mut parents: Vec<RecordId> = previous.map(|r| r.id).into_iter().collect();
        let room = MAX_PARENTS - parents.len();
        parents.extend(
            self.heads()
                .into_iter()
                .filter(|head| Some(head) != previous.map(|r| &r.id))
                .take(room),
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
        for needed in [seat, body_ref].into_iter().flatten() {
            if self.held.contains_key(&needed) && !self.ancestors_of(&parents).contains(&needed) {
                let target = self
                    .heads()
                    .into_iter()
                    .find(|head| self.ancestors_of(&[*head]).contains(&needed))
                    .unwrap_or(needed);
                if !parents.contains(&target) {
                    if parents.len() >= MAX_PARENTS
                        && let Some(pos) = parents.iter().rposition(|p| !pinned.contains(p))
                    {
                        parents.remove(pos);
                    }
                    parents.push(target);
                }
                pinned.push(target);
            } else if let Some(&covering) = parents
                .iter()
                .find(|&&p| self.ancestors_of(&[p]).contains(&needed))
            {
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
    /// the held log. It also fails when the log already holds [`MAX_RECORDS`] records or
    /// [`MAX_WAITING`] waiting records. It also fails when holding or keeping the record waiting
    /// would make the log's wire bytes exceed [`MAX_RECORDS_STREAM`].
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
                return Err(Error::new(
                    "membership log holds its limit of waiting records",
                ));
            }
            self.waiting.insert(record.id, record);
            return Ok(Inserted::Waiting);
        }
        if self.order.len() >= MAX_RECORDS
            || self.held_bytes().saturating_add(record.bytes().len()) > MAX_RECORDS_STREAM
        {
            return Err(Error::new("membership log holds its limit of records"));
        }
        self.check_against_parents(&record)?;
        self.hold(record);
        self.land_waiting();
        Ok(Inserted::Held)
    }

    /// Insert every record `other` holds or keeps waiting. A record `other` holds that this log
    /// refuses is skipped and the rest come in: both logs check records the same way, so that
    /// happens only at this log's bounds ([`MAX_RECORDS`], [`MAX_WAITING`]), which depend on what
    /// reached each log first.
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

    /// Membership as this log says it: the pure evaluator of §3.2.
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
        eval::evaluate(&self.order)
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
        let held_targets: Vec<RecordId> = targets
            .iter()
            .copied()
            .filter(|id| self.held.contains_key(id))
            .collect();
        if held_targets.is_empty() {
            return Vec::new();
        }
        let mut keep = self.ancestors_of(&held_targets);
        keep.extend(held_targets);
        self.order
            .iter()
            .filter(|r| keep.contains(&r.id))
            .cloned()
            .collect()
    }

    /// Founding [`Body::Genesis`] or effective [`Body::Admit`] [`RecordId`] that grants `binding`
    /// a seat in this log, or `None` when `binding` holds no effective seat (or was voided out for
    /// good by a quorum `Remove`).
    #[must_use]
    pub fn historical_seat_entry(&self, binding: &Binding) -> Option<RecordId> {
        eval::historical_seat_entry(&self.order, binding)
    }

    /// Whether `binding` holds a founding or effective admitted seat in this log (even if later
    /// retired by an ordinary `Remove`, unless voided out for good by a quorum `Remove`).
    #[must_use]
    pub fn has_historical_seat(&self, binding: &Binding) -> bool {
        eval::has_historical_seat(&self.order, binding)
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
        let writers = eval::historical_bindings_for_epoch(&self.order, idx)?;
        Some((*generation, writers))
    }

    fn check_against_parents(&self, record: &Record) -> Result<()> {
        if self.enforce_seats && !self.has_seat_in_past(record) {
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
            if !self.precedes(cutoff, record) {
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
                if !self.precedes(epoch, record)
                    || !matches!(self.get(epoch).map(Record::body), Some(Body::Epoch { .. })) =>
            {
                return Err(Error::new("a Wrap names an Epoch in its causal past"));
            }
            Body::Put {
                audience, epoch, ..
            }
            | Body::Delete {
                audience, epoch, ..
            } if !self.precedes(epoch, record)
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
    /// a held or waiting `Admit` lets in.
    fn holds_seat(&self, actor: &MemberId) -> bool {
        self.order
            .iter()
            .chain(self.waiting.values())
            .any(|r| match r.body() {
                Body::Genesis { founders, .. } => {
                    founders.iter().any(|f| f.member() == Some(*actor))
                }
                Body::Admit { entrant, .. } => {
                    r.actor() != &entrant.member() && entrant.member() == *actor
                }
                _ => false,
            })
    }

    /// Whether `record`'s signer is a bound founder in genesis or admitted in its causal past.
    fn has_seat_in_past(&self, record: &Record) -> bool {
        let actor = record.actor();
        if let Body::Genesis { founders, .. } = self.genesis().body()
            && founders.iter().any(|f| f.member() == Some(*actor))
        {
            return true;
        }
        for id in self.ancestors_of(record.parents()) {
            if let Some(parent) = self.get(&id)
                && matches!(
                    parent.body(),
                    Body::Admit { entrant, .. }
                        if parent.actor() != &entrant.member() && entrant.member() == *actor
                )
            {
                return true;
            }
        }
        false
    }

    /// Every held record in or reachable from `roots`.
    fn ancestors_of(&self, roots: &[RecordId]) -> BTreeSet<RecordId> {
        let mut seen = BTreeSet::new();
        let mut stack: Vec<RecordId> = roots.to_vec();
        while let Some(id) = stack.pop() {
            if self.held.contains_key(&id)
                && seen.insert(id)
                && let Some(parent) = self.get(&id)
            {
                stack.extend(parent.parents().iter().copied());
            }
        }
        seen
    }

    /// Whether the held record `ancestor` is in `record`'s causal past.
    fn precedes(&self, ancestor: &RecordId, record: &Record) -> bool {
        let mut seen = BTreeSet::new();
        let mut stack: Vec<&RecordId> = record.parents().iter().collect();
        while let Some(id) = stack.pop() {
            if id == ancestor {
                return true;
            }
            if seen.insert(*id)
                && let Some(parent) = self.get(id)
            {
                stack.extend(parent.parents().iter());
            }
        }
        false
    }

    pub(super) fn held_bytes(&self) -> usize {
        self.order.iter().map(|r| r.bytes().len()).sum()
    }

    pub(super) fn waiting_bytes(&self) -> usize {
        self.waiting.values().map(|r| r.bytes().len()).sum()
    }

    pub(super) fn total_bytes(&self) -> usize {
        self.held_bytes().saturating_add(self.waiting_bytes())
    }

    fn hold(&mut self, record: Record) {
        self.held.insert(record.id, self.order.len());
        self.order.push(record);
    }

    /// Hold every waiting record whose parents have all arrived, until none is left to land.
    /// Each is taken in id order, so the result is the same whatever order records came in.
    fn land_waiting(&mut self) {
        loop {
            if self.order.len() >= MAX_RECORDS {
                return;
            }
            let ready: Vec<RecordId> = self
                .waiting
                .values()
                .filter(|r| r.parents().iter().all(|p| self.held.contains_key(p)))
                .map(|r| r.id)
                .collect();
            if ready.is_empty() {
                return;
            }
            for id in ready {
                if self.order.len() >= MAX_RECORDS {
                    return;
                }
                let Some(record) = self.waiting.remove(&id) else {
                    continue;
                };
                if self.held_bytes().saturating_add(record.bytes().len()) <= MAX_RECORDS_STREAM
                    && self.check_against_parents(&record).is_ok()
                {
                    self.hold(record);
                }
            }
        }
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
#[doc(hidden)]
pub fn membership_log_environment() {}

#[cfg(test)]
mod tests;
