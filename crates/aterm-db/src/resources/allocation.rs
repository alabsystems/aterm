// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::{Error, Result, crypto::Reader};
use astream_wire::sync::{Actor, Digest};
use aterm_digest::Sha256;
use aterm_secrets::commons::journal::Trust;
use std::collections::BTreeMap;

const MAGIC: &[u8; 5] = b"ARAL1";
const DOMAIN: &[u8] = b"aterm/commons/resource-allocation/1\0";
pub(super) const MAX_SHARES: usize = 63;
pub(super) const MAX_WIRE: usize = 5 + 80 + 64 + 8 + 1 + 32 + 1 + MAX_SHARES * 56;

/// Public proposal metadata. Creating these terms does not create spend rights.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Terms {
    /// Stable resource identity agreed during credential activation. A display
    /// alias must never introduce another identity for the same capacity.
    pub resource: Digest,
    /// Explicit, non-reused resource-window identity. A clock cannot mint one.
    pub window: Digest,
    /// Agreed admission unit, independent of provider telemetry and billing.
    pub unit: String,
    pub limit: u64,
    /// Fixed disjoint rights by certified writer incarnation. Missing actors
    /// have no share. Unassigned capacity stays reserved, never borrowed.
    pub shares: BTreeMap<Actor, u64>,
}

impl Terms {
    fn validate(&self, trust: Trust) -> Result<()> {
        if trust.network == [0; 16]
            || trust.authority == [0; 32]
            || trust.genesis == [0; 32]
            || self.resource == [0; 32]
            || self.window == [0; 32]
            || self.limit == 0
            || self.shares.is_empty()
            || self.shares.len() > MAX_SHARES
            || self.unit.is_empty()
            || self.unit.len() > 32
            || !self
                .unit
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        {
            return Err(Error::new("invalid fixed resource allocation"));
        }
        let mut sum = 0u64;
        for (actor, share) in &self.shares {
            if actor[..32] == [0; 32]
                || actor[32..] == [0; 16]
                || actor[..32] == trust.authority
                || *share == 0
            {
                return Err(Error::new(
                    "allocation needs nonzero device-incarnation shares",
                ));
            }
            sum = sum
                .checked_add(*share)
                .ok_or_else(|| Error::new("allocation shares overflow"))?;
        }
        if sum > self.limit {
            return Err(Error::new("allocation shares exceed the fixed limit"));
        }
        Ok(())
    }
}

/// Canonical untrusted proposal. Its fingerprint authenticates bytes only when
/// an independent credential activation has already selected that fingerprint.
#[derive(Clone, Debug)]
pub struct Proposal {
    trust: Trust,
    terms: Terms,
    bytes: Vec<u8>,
    id: Digest,
}
impl Proposal {
    /// Prepare bounded metadata for review. This signs nothing and grants no rights.
    pub fn prepare(trust: Trust, terms: Terms) -> Result<Self> {
        terms.validate(trust)?;
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&trust.network);
        bytes.extend_from_slice(&trust.authority);
        bytes.extend_from_slice(&trust.genesis);
        bytes.extend_from_slice(&terms.resource);
        bytes.extend_from_slice(&terms.window);
        bytes.extend_from_slice(&terms.limit.to_le_bytes());
        bytes.push(terms.unit.len() as u8);
        bytes.extend_from_slice(terms.unit.as_bytes());
        bytes.push(terms.shares.len() as u8);
        for (actor, share) in &terms.shares {
            bytes.extend_from_slice(actor);
            bytes.extend_from_slice(&share.to_le_bytes());
        }
        let id = fingerprint(&bytes);
        Ok(Self {
            trust,
            terms,
            bytes,
            id,
        })
    }
    /// Decode canonical proposal bytes without accepting their proposed authority.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_WIRE {
            return Err(Error::new("resource allocation exceeds its wire bound"));
        }
        let mut read = Reader::new(bytes);
        if read.take(MAGIC.len())? != MAGIC {
            return Err(Error::new("unknown resource allocation format"));
        }
        let trust = Trust {
            network: read.array()?,
            authority: read.array()?,
            genesis: read.array()?,
        };
        let resource = read.array()?;
        let window = read.array()?;
        let limit = u64::from_le_bytes(read.array()?);
        let size = usize::from(read.byte()?);
        if size > 32 {
            return Err(Error::new("resource unit exceeds its bound"));
        }
        let unit = std::str::from_utf8(read.take(size)?)
            .map_err(|_| Error::new("resource unit is not UTF-8"))?
            .to_owned();
        let count = usize::from(read.byte()?);
        if count > MAX_SHARES {
            return Err(Error::new("too many resource shares"));
        }
        let mut shares = BTreeMap::new();
        let mut previous = None;
        for _ in 0..count {
            let actor = read.array::<48>()?;
            if previous.is_some_and(|old| old >= actor) {
                return Err(Error::new("resource shares are not strictly ordered"));
            }
            previous = Some(actor);
            shares.insert(actor, u64::from_le_bytes(read.array()?));
        }
        read.end()?;
        let proposal = Self::prepare(
            trust,
            Terms {
                resource,
                window,
                unit,
                limit,
                shares,
            },
        )?;
        if proposal.bytes != bytes {
            return Err(Error::new("noncanonical resource allocation"));
        }
        Ok(proposal)
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub const fn fingerprint(&self) -> Digest {
        self.id
    }
    pub fn terms(&self) -> &Terms {
        &self.terms
    }
}

/// An allocation whose exact bytes match an independently trusted activation
/// pin and Commons root. All consumers of one external resource/window must
/// agree on this pin before consuming it; hashing alone cannot prove that.
#[derive(Clone, Debug)]
pub struct Allocation(Proposal);
impl Allocation {
    /// Verify against a pin obtained from coherent credential activation, never
    /// from the same discovered record as `bytes`. No automatic pin replacement
    /// or new-window authority is implied by this constructor.
    pub fn verify(bytes: &[u8], expected: Digest, trust: Trust) -> Result<Self> {
        if bytes.len() > MAX_WIRE {
            return Err(Error::new("resource allocation exceeds its wire bound"));
        }
        if expected == [0; 32] || fingerprint(bytes) != expected {
            return Err(Error::new(
                "resource allocation does not match its activation pin",
            ));
        }
        let proposal = Proposal::decode(bytes)?;
        if proposal.trust != trust {
            return Err(Error::new("resource allocation has another Commons root"));
        }
        Ok(Self(proposal))
    }
    pub const fn id(&self) -> Digest {
        self.0.id
    }
    pub const fn trust(&self) -> Trust {
        self.0.trust
    }
    pub fn terms(&self) -> &Terms {
        &self.0.terms
    }
    pub fn share(&self, actor: &Actor) -> Option<u64> {
        self.0.terms.shares.get(actor).copied()
    }
    pub fn bytes(&self) -> &[u8] {
        &self.0.bytes
    }
}

fn fingerprint(bytes: &[u8]) -> Digest {
    let mut message = DOMAIN.to_vec();
    message.extend_from_slice(bytes);
    Sha256::digest(&message)
}

#[cfg(test)]
mod tests;
