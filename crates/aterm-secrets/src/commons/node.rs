// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Signed, recipient-encrypted control DAG nodes. A recovery joins explicit
//! parents and selects an existing payload; it cannot silently write a new one.

use super::constraints::{Fact, Frontier, MAX_FACTS};
#[cfg(unix)]
use crate::age::Recipient;
use crate::{
    Error, Result, SecretBytes,
    age::{self, Identity},
};
use astream_wire::sync::{Digest, MAX_PARENTS};
use ring::signature::{ED25519, UnparsedPublicKey};
#[cfg(unix)]
use ring::signature::{Ed25519KeyPair, KeyPair};

/// Maximum control snapshot plaintext, separate from ordinary secret files.
pub const MAX_PAYLOAD: usize = 256 * 1024;
/// Bound checked before copying or decoding an untrusted node.
pub const MAX_NODE: usize = MAX_PAYLOAD + 32 * 1024;
const LEGACY_DOMAIN: &[u8] = b"aterm/commons/control-node/1\0";
const RESTRICTION_DOMAIN: &[u8] = b"aterm/commons/control-node/2\0";
const DOMAIN: &[u8] = b"aterm/commons/control-node/3\0";
#[cfg(unix)]
const MAGIC: &[u8] = b"ACN3";

/// A control operation prepared by an authority. Persist the returned node and
/// retry its exact bytes; preparing another encrypted node is another operation.
pub enum Edit<'a> {
    /// Establish the immutable network genesis and initial snapshot.
    Create {
        hosts: &'a [String],
        payload: &'a [u8],
    },
    /// Replace the snapshot after one exact predecessor.
    Update {
        parent: Digest,
        hosts: &'a [String],
        payload: &'a [u8],
    },
    /// Join competing heads and explicitly select one existing snapshot.
    Recover {
        heads: &'a [Digest],
        selected: Digest,
    },
}

/// The operation authenticated by a control node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Genesis,
    Update,
    Recovery,
}

/// An authenticated immutable node. Its payload remains encrypted until opened
/// with an age identity; graph readiness alone grants no application privilege.
#[derive(Clone, Debug)]
pub struct Node {
    network: [u8; 16],
    authority: [u8; 32],
    kind: Kind,
    parents: Vec<Digest>,
    selected: Option<Digest>,
    authored_ms: u64,
    ciphertext: Vec<u8>,
    wire: Vec<u8>,
    id: Digest,
    legacy: bool,
    #[cfg(unix)]
    binding_evidence: bool,
    facts: Vec<Fact>,
}

impl Node {
    /// Sign protocol fixtures, including deliberately invalid policy, for
    /// cross-crate conformance tests. Production publication uses the Admin API.
    #[cfg(all(unix, any(test, debug_assertions)))]
    pub fn test_seal(
        seed: &[u8; 32],
        network: [u8; 16],
        edit: Edit<'_>,
        recipients: &[Recipient],
        authored_ms: u64,
        facts: &[Fact],
    ) -> Result<Self> {
        Self::seal_restricted(seed, network, edit, recipients, authored_ms, facts)
    }

    /// Verify exact bytes against an independently trusted network and authority.
    /// This does not choose a current state or enroll an unknown signer.
    pub fn verify(bytes: &[u8], network: [u8; 16], authority: [u8; 32]) -> Result<Self> {
        if bytes.len() > MAX_NODE || bytes.len() < 4 + 16 + 32 + 2 + 32 + 8 + 4 + 64 {
            return Err(Error::new("control node exceeds its size bounds"));
        }
        let split = bytes.len() - 64;
        let (legacy, binding_facts, domain) = match &bytes[..4] {
            b"ACN1" => (true, false, LEGACY_DOMAIN),
            b"ACN2" => (false, false, RESTRICTION_DOMAIN),
            b"ACN3" => (false, true, DOMAIN),
            _ => return Err(Error::new("unknown control node version")),
        };
        let mut signed = domain.to_vec();
        signed.extend_from_slice(&bytes[..split]);
        UnparsedPublicKey::new(&ED25519, authority)
            .verify(&signed, &bytes[split..])
            .map_err(|_| Error::new("control node signature did not verify"))?;
        let mut reader = Reader {
            bytes: &bytes[..split],
            at: 0,
        };
        reader.take(4)?;
        if reader.array::<16>()? != network || reader.array::<32>()? != authority {
            return Err(Error::new(
                "control node names another network or authority",
            ));
        }
        let kind = match reader.take(1)?[0] {
            0 => Kind::Genesis,
            1 => Kind::Update,
            2 => Kind::Recovery,
            _ => return Err(Error::new("unknown control node kind")),
        };
        let count = usize::from(reader.take(1)?[0]);
        if count > MAX_PARENTS {
            return Err(Error::new("too many control parents"));
        }
        let mut parents = Vec::with_capacity(count);
        for _ in 0..count {
            parents.push(reader.array()?);
        }
        let choice: Digest = reader.array()?;
        let selected = (kind == Kind::Recovery).then_some(choice);
        let authored_ms = u64::from_le_bytes(reader.array()?);
        let mut facts = Vec::new();
        if !legacy {
            let count = usize::from(u16::from_le_bytes(reader.array()?));
            if count > MAX_FACTS {
                return Err(Error::new("too many control restrictions"));
            }
            for _ in 0..count {
                let fact = match reader.take(1)?[0] {
                    1 => Fact::Retire {
                        actor: reader.array()?,
                    },
                    2 => Fact::Epoch {
                        generation: u64::from_le_bytes(reader.array()?),
                        commitment: reader.array()?,
                    },
                    3 => {
                        let table = reader.array()?;
                        let actor = reader.array()?;
                        let sequence = u64::from_le_bytes(reader.array()?);
                        let digest = reader.array()?;
                        if (sequence == 0) != (digest == [0; 32]) {
                            return Err(Error::new("invalid control cutoff sentinel"));
                        }
                        Fact::Cutoff {
                            table,
                            actor,
                            through: (sequence > 0).then_some(Frontier { sequence, digest }),
                        }
                    }
                    4 if binding_facts => Fact::Bind {
                        binding: super::binding::Binding::decode(
                            reader.take(super::binding::BINDING_LEN)?,
                        )?,
                    },
                    _ => return Err(Error::new("unknown control restriction")),
                };
                fact.validate()?;
                if let Fact::Bind { binding } = &fact
                    && (*binding.network() != network || *binding.authority() != authority)
                {
                    return Err(Error::new("control binding has another trust context"));
                }
                facts.push(fact);
            }
            if facts.windows(2).any(|pair| pair[0] >= pair[1])
                || (kind == Kind::Recovery && !facts.is_empty())
            {
                return Err(Error::new("noncanonical or misplaced control restrictions"));
            }
        }
        let length = u32::from_le_bytes(reader.array()?) as usize;
        let ciphertext = reader.take(length)?.to_vec();
        if reader.at != split || parents.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(Error::new(
                "control node has trailing bytes or noncanonical parents",
            ));
        }
        let shape = match kind {
            Kind::Genesis => parents.is_empty() && choice == [0; 32] && !ciphertext.is_empty(),
            Kind::Update => parents.len() == 1 && choice == [0; 32] && !ciphertext.is_empty(),
            Kind::Recovery => {
                parents.len() >= 2
                    && parents.binary_search(&choice).is_ok()
                    && ciphertext.is_empty()
            }
        };
        if !shape {
            return Err(Error::new("invalid control node parent or payload shape"));
        }
        if kind != Kind::Recovery && !ciphertext.starts_with(b"age-encryption.org/v1\n") {
            return Err(Error::new("control payload requires binary age v1"));
        }
        let id = aterm_digest::Sha256::digest(bytes);
        if parents.contains(&id) {
            return Err(Error::new("control node depends on itself"));
        }
        Ok(Self {
            network,
            authority,
            kind,
            parents,
            selected,
            authored_ms,
            ciphertext,
            wire: bytes.to_vec(),
            id,
            legacy,
            #[cfg(unix)]
            binding_evidence: binding_facts,
            facts,
        })
    }

    #[cfg(unix)]
    pub(crate) fn seal(
        seed: &[u8; 32],
        network: [u8; 16],
        edit: Edit<'_>,
        recipients: &[Recipient],
        authored_ms: u64,
    ) -> Result<Self> {
        Self::seal_restricted(seed, network, edit, recipients, authored_ms, &[])
    }

    #[cfg(unix)]
    pub(crate) fn seal_restricted(
        seed: &[u8; 32],
        network: [u8; 16],
        edit: Edit<'_>,
        recipients: &[Recipient],
        authored_ms: u64,
        facts: &[Fact],
    ) -> Result<Self> {
        let (kind, mut parents, selected, payload) = match edit {
            Edit::Create { payload, .. } => (Kind::Genesis, Vec::new(), [0; 32], Some(payload)),
            Edit::Update {
                parent, payload, ..
            } => (Kind::Update, vec![parent], [0; 32], Some(payload)),
            Edit::Recover { heads, selected } => (Kind::Recovery, heads.to_vec(), selected, None),
        };
        if parents.len() > MAX_PARENTS
            || facts.len() > MAX_FACTS
            || (kind == Kind::Recovery && !facts.is_empty())
            || payload.is_some_and(|p| p.len() > MAX_PAYLOAD)
            || (payload.is_some() && (recipients.is_empty() || recipients.len() > 64))
        {
            return Err(Error::new(
                "control draft exceeds recipient, parent or payload bounds",
            ));
        }
        let mut facts = facts.to_vec();
        for fact in &facts {
            fact.validate()?;
        }
        facts.sort();
        if facts.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(Error::new("duplicate control restriction"));
        }
        parents.sort_unstable();
        if parents.windows(2).any(|p| p[0] == p[1]) {
            return Err(Error::new("duplicate control parents"));
        }
        let ciphertext = payload
            .map(|p| age::encrypt(recipients, p))
            .transpose()?
            .unwrap_or_default();
        let key = Ed25519KeyPair::from_seed_unchecked(seed)
            .map_err(|_| Error::new("invalid control authority key"))?;
        let authority: [u8; 32] = key
            .public_key()
            .as_ref()
            .try_into()
            .map_err(|_| Error::new("invalid control authority size"))?;
        let mut wire = MAGIC.to_vec();
        wire.extend_from_slice(&network);
        wire.extend_from_slice(&authority);
        wire.push(match kind {
            Kind::Genesis => 0,
            Kind::Update => 1,
            Kind::Recovery => 2,
        });
        wire.push(parents.len() as u8);
        for parent in &parents {
            wire.extend_from_slice(parent);
        }
        wire.extend_from_slice(&selected);
        wire.extend_from_slice(&authored_ms.to_le_bytes());
        wire.extend_from_slice(&(facts.len() as u16).to_le_bytes());
        for fact in facts {
            fact.encode(&mut wire);
        }
        wire.extend_from_slice(&(ciphertext.len() as u32).to_le_bytes());
        wire.extend_from_slice(&ciphertext);
        let mut signed = DOMAIN.to_vec();
        signed.extend_from_slice(&wire);
        wire.extend_from_slice(key.sign(&signed).as_ref());
        Self::verify(&wire, network, authority)
    }

    /// Exact signed wire, with an encrypted payload, for append and peer repair.
    pub fn bytes(&self) -> &[u8] {
        &self.wire
    }
    /// Content identity; independent of local journal offsets and clocks.
    pub const fn id(&self) -> Digest {
        self.id
    }
    /// Operation kind.
    pub const fn kind(&self) -> Kind {
        self.kind
    }
    /// Exact causal parents, sorted and unique.
    pub fn parents(&self) -> &[Digest] {
        &self.parents
    }
    /// Chosen existing state for a recovery.
    pub const fn selected(&self) -> Option<Digest> {
        self.selected
    }
    /// Signed author clock claim; never used for ordering or admission.
    pub const fn authored_ms(&self) -> u64 {
        self.authored_ms
    }
    /// The pinned network identity.
    pub const fn network(&self) -> [u8; 16] {
        self.network
    }
    /// The signing authority, not an individual device identity.
    pub const fn authority(&self) -> [u8; 32] {
        self.authority
    }

    #[cfg(unix)]
    pub(crate) const fn binding_evidence(&self) -> bool {
        self.binding_evidence
    }

    /// Whether this record predates independently readable restriction evidence.
    pub const fn legacy(&self) -> bool {
        self.legacy
    }
    /// Signed restrictive facts, readable even without a payload decryption key.
    pub fn facts(&self) -> &[Fact] {
        &self.facts
    }

    /// Decrypt a data node for historical reading. A recovery's selected node
    /// must be resolved through its journal; it carries no replacement payload.
    pub fn open(&self, identity: &Identity) -> Result<SecretBytes> {
        if self.kind == Kind::Recovery {
            return Err(Error::new("resolve a recovery through its selected parent"));
        }
        let plain = age::decrypt(std::slice::from_ref(identity), &self.ciphertext)
            .map_err(|_| Error::new("control payload could not be decrypted"))?;
        if plain.len() > MAX_PAYLOAD {
            return Err(Error::new("control plaintext exceeds its bound"));
        }
        Ok(plain)
    }

    /// Each content-addressed control event occupies one one-record stream.
    /// Control conflicts are competing DAG heads, independent of actor clocks.
    #[cfg(unix)]
    pub(crate) fn header(&self) -> astream_wire::sync::Header {
        let mut actor = [0; 48];
        actor[..32].copy_from_slice(&self.id);
        astream_wire::sync::Header {
            id: self.id,
            actor,
            sequence: 1,
            previous: None,
            parents: self.parents.clone(),
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .at
            .checked_add(n)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| Error::new("truncated control node"))?;
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.take(N)?
            .try_into()
            .map_err(|_| Error::new("invalid control node field"))
    }
}
