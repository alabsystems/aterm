// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Joint possession of a device's age identity and independent writer key.
//! This is an enrollment building block, not admission or a transferable grant.
//! The approver must check the exact binding against current eligible membership
//! before issuing a challenge and again before certifying the returned proof.
//! This proves use of both keys, not physical co-location or hardware attestation.

use crate::{Error, Result, SecretBytes, age};
use aterm_digest::Sha256;
use aterm_time::Instant;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use std::time::Duration;
use zeroize::Zeroize;

pub(crate) const BINDING_LEN: usize = 144;
const PLAIN_LEN: usize = 4 + BINDING_LEN + 32;
/// Exact wire size of a self-signed device request; reject other frame lengths.
pub const REQUEST_LEN: usize = 4 + BINDING_LEN + 64;
/// Exact wire size of a possession response; reject other frame lengths.
pub const RESPONSE_LEN: usize = 4 + 32 + 64;
const REQUEST_DOMAIN: &[u8] = b"aterm/commons/device-request/1\0";
const RESPONSE_DOMAIN: &[u8] = b"aterm/commons/device-possession/1\0";
/// Maximum sealed challenge size, checked before age parsing or decryption.
pub const MAX_CHALLENGE: usize = 2048;
/// Challenges expire by the approver's monotonic clock, never a peer's wall time.
pub const CHALLENGE_LIFETIME: Duration = Duration::from_secs(300);

/// Immutable enrollment identity. Friendly machine names are not credentials.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Binding {
    network: [u8; 16],
    authority: [u8; 32],
    machine: [u8; 16],
    recipient: age::Recipient,
    writer: [u8; 32],
    incarnation: [u8; 16],
}
impl Binding {
    /// Exact lab network, independent of its display name.
    pub const fn network(&self) -> &[u8; 16] {
        &self.network
    }
    /// Vault authority to which this request is bound.
    pub const fn authority(&self) -> &[u8; 32] {
        &self.authority
    }
    /// Stable machine UUID, independent of names and aliases.
    pub const fn machine(&self) -> &[u8; 16] {
        &self.machine
    }
    /// Existing age identity whose possession must also be proved.
    pub const fn recipient(&self) -> age::Recipient {
        self.recipient
    }
    /// Independent Ed25519 writer key.
    pub const fn writer(&self) -> &[u8; 32] {
        &self.writer
    }
    /// Exact writer installation, never inferred from a timestamp.
    pub const fn incarnation(&self) -> &[u8; 16] {
        &self.incarnation
    }
    /// Exact writer incarnation; names are not part of this identity.
    pub fn actor(&self) -> [u8; 48] {
        let mut actor = [0; 48];
        actor[..32].copy_from_slice(&self.writer);
        actor[32..].copy_from_slice(&self.incarnation);
        actor
    }
    pub(crate) fn bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(BINDING_LEN);
        bytes.extend_from_slice(&self.network);
        bytes.extend_from_slice(&self.authority);
        bytes.extend_from_slice(&self.machine);
        bytes.extend_from_slice(&self.recipient.0);
        bytes.extend_from_slice(&self.writer);
        bytes.extend_from_slice(&self.incarnation);
        bytes
    }
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != BINDING_LEN {
            return Err(Error::new("invalid Commons device binding length"));
        }
        let binding = Self {
            network: bytes[..16].try_into().unwrap(),
            authority: bytes[16..48].try_into().unwrap(),
            machine: bytes[48..64].try_into().unwrap(),
            recipient: age::Recipient(bytes[64..96].try_into().unwrap()),
            writer: bytes[96..128].try_into().unwrap(),
            incarnation: bytes[128..144].try_into().unwrap(),
        };
        if binding.network == [0; 16]
            || binding.authority == [0; 32]
            || binding.machine == [0; 16]
            || binding.writer == [0; 32]
            || binding.incarnation == [0; 16]
            || binding.writer == binding.authority
            || !crate::x25519::canonical_u(&binding.recipient.0)
            || age::Recipient::parse(&binding.recipient.to_bech32()).is_err()
        {
            return Err(Error::new("invalid Commons device binding identity"));
        }
        Ok(binding)
    }
}

fn pair(seed: &[u8; 32]) -> Result<Ed25519KeyPair> {
    Ed25519KeyPair::from_seed_unchecked(seed).map_err(|_| Error::new("invalid Commons writer seed"))
}
fn message(domain: &[u8], bytes: &[u8]) -> SecretBytes {
    SecretBytes::new([domain, bytes].concat())
}
fn response_message(id: &[u8; 32], plain: &[u8]) -> SecretBytes {
    SecretBytes::new([RESPONSE_DOMAIN, id.as_slice(), plain].concat())
}

/// A canonical, self-signed request. Its signature proves only writer possession.
/// Network/authority fields must match independently pinned local configuration.
#[derive(Clone, Debug)]
pub struct Request {
    binding: Binding,
    wire: Vec<u8>,
}
impl Request {
    /// Prepare a request from persistent device identities. Reuse these exact
    /// identities after restart; allocating a new incarnation is a new enrollment.
    pub fn create(
        network: [u8; 16],
        authority: [u8; 32],
        machine: [u8; 16],
        incarnation: [u8; 16],
        identity: &age::Identity,
        seed: &[u8; 32],
    ) -> Result<Self> {
        let key = pair(seed)?;
        let binding = Binding {
            network,
            authority,
            machine,
            recipient: identity.recipient(),
            writer: key.public_key().as_ref().try_into().unwrap(),
            incarnation,
        };
        let mut wire = b"ACB1".to_vec();
        wire.extend_from_slice(&binding.bytes());
        wire.extend_from_slice(key.sign(message(REQUEST_DOMAIN, &wire).as_slice()).as_ref());
        Self::decode(&wire)
    }
    /// Verify bounded exact bytes. This accepts no unknown fields or trailing data.
    pub fn decode(wire: &[u8]) -> Result<Self> {
        if wire.len() != REQUEST_LEN || &wire[..4] != b"ACB1" {
            return Err(Error::new("invalid Commons device request"));
        }
        let end = REQUEST_LEN - 64;
        let binding = Binding::decode(&wire[4..end])?;
        UnparsedPublicKey::new(&ED25519, binding.writer)
            .verify(
                message(REQUEST_DOMAIN, &wire[..end]).as_slice(),
                &wire[end..],
            )
            .map_err(|_| Error::new("Commons device request signature failed"))?;
        Ok(Self {
            binding,
            wire: wire.to_vec(),
        })
    }
    /// Identity authenticated by the request's self-signature.
    pub fn binding(&self) -> &Binding {
        &self.binding
    }
    /// Exact bounded public request bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.wire
    }
    /// Open a challenge only for this exact locally prepared request and sign its
    /// domain-separated transcript. A copied public recipient cannot answer it.
    pub fn answer(
        &self,
        identity: &age::Identity,
        seed: &[u8; 32],
        challenge: &[u8],
    ) -> Result<Vec<u8>> {
        let key = pair(seed)?;
        if identity.recipient() != self.binding.recipient
            || key.public_key().as_ref() != self.binding.writer
            || challenge.len() > MAX_CHALLENGE
            || !challenge.starts_with(b"age-encryption.org/v1\n")
        {
            return Err(Error::new("Commons challenge does not match this device"));
        }
        let plain = age::decrypt(std::slice::from_ref(identity), challenge)
            .map_err(|_| Error::new("Commons challenge could not be opened"))?;
        if plain.len() != PLAIN_LEN
            || &plain.as_slice()[..4] != b"ACC1"
            || plain.as_slice()[4..4 + BINDING_LEN] != self.binding.bytes()
        {
            return Err(Error::new("Commons challenge names another binding"));
        }
        let id = Sha256::digest(challenge);
        let mut response = b"ACP1".to_vec();
        response.extend_from_slice(&id);
        response.extend_from_slice(
            key.sign(response_message(&id, plain.as_slice()).as_slice())
                .as_ref(),
        );
        Ok(response)
    }
}

/// Volatile approver state for one challenge. Bound the number of these at the
/// endpoint and drop expired entries using [`Pending::is_live`]. Dropping it,
/// including on restart, invalidates all its responses.
/// It is deliberately neither cloneable nor serializable and never logs a nonce.
#[derive(Debug)]
pub struct Pending {
    binding: Binding,
    id: [u8; 32],
    plain: Option<SecretBytes>,
    created: Instant,
}
/// Evidence of both keys' possession in one local, single-use challenge. This is
/// not membership: certify only after rechecking current eligibility and conflicts.
#[derive(Debug)]
pub struct Possession {
    binding: Binding,
}
impl Possession {
    /// The exact identity whose two private keys answered this challenge.
    pub fn binding(&self) -> &Binding {
        &self.binding
    }
}
impl Pending {
    /// Whether this challenge can still accept a response. Endpoint maintenance
    /// drops entries returning false, wiping their pending nonce even when no
    /// response arrives. This query does not authenticate any response.
    pub fn is_live(&self) -> bool {
        self.plain.is_some() && self.created.elapsed() < CHALLENGE_LIFETIME
    }
    /// Match independently pinned network/authority before issuing. Call only
    /// after checking current eligibility; this primitive cannot decide policy.
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbDevicePossession",
            action = "Issue",
            project = "aterm_secrets::commons::binding::tests::possession_transition"
        )
    )]
    pub fn issue(
        request: &Request,
        network: [u8; 16],
        authority: [u8; 32],
    ) -> Result<(Self, Vec<u8>)> {
        if request.binding.network != network || request.binding.authority != authority {
            return Err(Error::new(
                "Commons request does not match the pinned authority",
            ));
        }
        let mut nonce = [0; 32];
        if let Err(error) = aterm_uds::rand::fill(&mut nonce) {
            nonce.zeroize();
            return Err(error.into());
        }
        let mut plain = Vec::with_capacity(PLAIN_LEN);
        plain.extend_from_slice(b"ACC1");
        plain.extend_from_slice(&request.binding.bytes());
        plain.extend_from_slice(&nonce);
        nonce.zeroize();
        let plain = SecretBytes::new(plain);
        let challenge = age::encrypt(&[request.binding.recipient], plain.as_slice())?;
        if challenge.len() > MAX_CHALLENGE {
            return Err(Error::new("Commons challenge exceeds its bound"));
        }
        Ok((
            Self {
                binding: request.binding.clone(),
                id: Sha256::digest(&challenge),
                plain: Some(plain),
                created: Instant::now(),
            },
            challenge,
        ))
    }
    /// Consume a valid response exactly once. Invalid responses leave a live
    /// challenge usable; observing expiry consumes it. Peers cannot extend it.
    pub fn verify(&mut self, response: &[u8]) -> Result<Possession> {
        self.verify_elapsed(response, self.created.elapsed())
    }
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbDevicePossession",
            action = "ValidProof",
            project = "aterm_secrets::commons::binding::tests::possession_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbDevicePossession",
            action = "InvalidProof",
            project = "aterm_secrets::commons::binding::tests::possession_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "DbDevicePossession",
            action = "Expire",
            project = "aterm_secrets::commons::binding::tests::possession_transition"
        )
    )]
    fn verify_elapsed(&mut self, response: &[u8], elapsed: Duration) -> Result<Possession> {
        if elapsed >= CHALLENGE_LIFETIME {
            self.plain = None;
        }
        let plain = self
            .plain
            .as_ref()
            .ok_or_else(|| Error::new("Commons challenge expired or already consumed"))?;
        if response.len() != RESPONSE_LEN
            || &response[..4] != b"ACP1"
            || response[4..36] != self.id
            || UnparsedPublicKey::new(&ED25519, self.binding.writer)
                .verify(
                    response_message(&self.id, plain.as_slice()).as_slice(),
                    &response[36..],
                )
                .is_err()
        {
            return Err(Error::new("invalid Commons possession response"));
        }
        self.plain = None;
        Ok(Possession {
            binding: self.binding.clone(),
        })
    }
}

#[cfg(test)]
mod tests;
