// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `Allowed{binding, name}`: a member's signed word that a person pressed Allow for one Mac's
//! request (`docs/DESIGN-fluid-swarm-2026-10-28.md` F10, "Who allowed"). The member signs it
//! with its Commons writer key. An admin's approver admits the request only when the current
//! Commons roster binds that key ([`Allowed::admits`]): `[machines]` recipients are age keys,
//! which cannot sign, so the vault alone cannot say who allowed.

use crate::commons::binding::{BINDING_LEN, Binding};
use crate::commons::roster::Member;
use crate::{Error, Result};
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};

const MAGIC: &[u8; 4] = b"ACA1";
const DOMAIN: &[u8] = b"aterm/alab/allowed/1\0";
/// The longest machine name ([`crate::text::valid_machine_name`]).
const MAX_NAME: usize = 63;
const SIGNATURE: usize = 64;
/// The shortest and longest encodings: a one-letter name and a 63-letter one.
const MIN_LEN: usize = MAGIC.len() + BINDING_LEN + 32 + 1 + 1 + SIGNATURE;
const MAX_LEN: usize = MIN_LEN - 1 + MAX_NAME;
/// The longest padded base64 of an encoding, the bound a join line's ` allowed=` holds to.
pub const MAX_TEXT: usize = MAX_LEN.div_ceil(3) * 4;
const _: () = assert!(MAX_TEXT == crate::transport::MAX_ALLOWED_TEXT);

/// A member's signed Allow for one binding under one machine name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Allowed {
    binding: Binding,
    signer: [u8; 32],
    name: String,
    wire: Vec<u8>,
}

impl Allowed {
    /// The Allow of `binding` under `name`, signed with the member's writer `seed`.
    ///
    /// # Errors
    /// `name` is not a machine name, the seed is not an Ed25519 seed, or the seed is the
    /// binding's own writer: no Mac allows itself.
    pub fn sign(binding: &Binding, name: &str, seed: &[u8; 32]) -> Result<Self> {
        let key = Ed25519KeyPair::from_seed_unchecked(seed)
            .map_err(|_| Error::new("invalid Commons writer seed"))?;
        let mut wire = MAGIC.to_vec();
        wire.extend_from_slice(&binding.bytes());
        wire.extend_from_slice(key.public_key().as_ref());
        wire.push(u8::try_from(name.len()).unwrap_or(u8::MAX));
        wire.extend_from_slice(name.as_bytes());
        let signature = key.sign(&[DOMAIN, wire.as_slice()].concat());
        wire.extend_from_slice(signature.as_ref());
        Self::decode(&wire)
    }

    /// Verify exact bytes: the signature, the binding, a machine name, no trailing data, and
    /// a signer that is not the binding's own writer.
    ///
    /// # Errors
    /// Any of those fails.
    pub fn decode(wire: &[u8]) -> Result<Self> {
        if !(MIN_LEN..=MAX_LEN).contains(&wire.len()) || &wire[..4] != MAGIC {
            return Err(Error::new("invalid Allowed"));
        }
        let mut at = MAGIC.len();
        let binding = Binding::decode(&wire[at..at + BINDING_LEN])?;
        at += BINDING_LEN;
        let signer: [u8; 32] = wire[at..at + 32].try_into().unwrap();
        at += 32;
        let len = usize::from(wire[at]);
        at += 1;
        if wire.len() != at + len + SIGNATURE {
            return Err(Error::new("invalid Allowed length"));
        }
        let name = std::str::from_utf8(&wire[at..at + len])
            .ok()
            .filter(|name| crate::text::valid_machine_name(name))
            .ok_or_else(|| Error::new("an Allowed names no machine"))?
            .to_owned();
        at += len;
        if &signer == binding.writer() {
            return Err(Error::new("an Allowed is signed by the Mac it allows"));
        }
        UnparsedPublicKey::new(&ED25519, signer)
            .verify(&[DOMAIN, &wire[..at]].concat(), &wire[at..])
            .map_err(|_| Error::new("an Allowed's signature failed"))?;
        Ok(Self {
            binding,
            signer,
            name,
            wire: wire.to_vec(),
        })
    }

    /// The Mac it allows.
    pub const fn binding(&self) -> &Binding {
        &self.binding
    }

    /// The writer key that signed it.
    pub const fn signer(&self) -> &[u8; 32] {
        &self.signer
    }

    /// The machine name it allows the Mac under.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Its exact bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.wire
    }

    /// Padded base64 of [`Self::bytes`], as a join line's ` allowed=` carries it.
    #[must_use]
    pub fn to_text(&self) -> String {
        crate::b64::encode_padded(&self.wire)
    }

    /// [`Self::decode`] of padded base64.
    ///
    /// # Errors
    /// The text is not padded base64 of a valid Allowed.
    pub fn from_text(text: &str) -> Result<Self> {
        if text.len() > MAX_TEXT {
            return Err(Error::new("invalid Allowed"));
        }
        Self::decode(&crate::b64::decode_padded(text).ok_or_else(|| Error::new("invalid Allowed"))?)
    }

    /// Whether this admits the request of `recipient` (`age1…`) under `host`, in the network
    /// and authority `trust` names: a member of the current roster `members` holds the
    /// signing key, and the Allow names that recipient and that name.
    #[must_use]
    pub fn admits(
        &self,
        members: &[Member],
        network: &[u8; 16],
        authority: &[u8; 32],
        recipient: &str,
        host: &str,
    ) -> bool {
        self.binding.network() == network
            && self.binding.authority() == authority
            && self.binding.recipient().to_bech32() == recipient
            && self.name == host
            && members
                .iter()
                .any(|member| member.binding().writer() == &self.signer)
    }
}
