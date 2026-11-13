// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A credential delivery is bound to both authenticated actors, both fresh nonces, the
//! request, its offer and the custodian's exact age recipient (§8.2 of the requests design).
//! These codecs grant no membership, policy permission or disk access: the runtime checks
//! those before reading a value and again before publishing it.

use super::custody;
use crate::{Error, SecretBytes, age};
use zeroize::Zeroizing;

/// Maximum total credential bytes in a delivery.
pub const MAX_VALUE: usize = 64 * 1024;
/// Maximum complete sealed delivery, including age overhead and the writer signature.
pub const MAX_DELIVERY: usize = MAX_VALUE + 4096;
/// Maximum monotonic lifetime of an exchange.
pub const LIFETIME: std::time::Duration = std::time::Duration::from_secs(300);
const PROOF_DOMAIN: &[u8] = b"aterm/credentials/custodian/1\0";
const DELIVERY_DOMAIN: &[u8] = b"aterm/credentials/delivery/1\0";

/// The public, connection-bound transcript. None of these fields comes from a credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exchange {
    /// The vault authenticated through the holder's applied signer pin.
    pub vault: String,
    /// Commons network.
    pub network: [u8; 16],
    /// Request id.
    pub request: [u8; 16],
    /// Exact signed Offer operation id.
    pub offer: [u8; 32],
    /// Custodian nonce.
    pub custodian_nonce: [u8; 32],
    /// Holder nonce.
    pub holder_nonce: [u8; 32],
    /// Custodian's channel-authenticated actor.
    pub custodian: [u8; 48],
    /// Holder's channel-authenticated actor.
    pub holder: [u8; 48],
    /// Recipient signed by the custodian and matched against its current binding.
    pub recipient: age::Recipient,
}

impl Exchange {
    fn message(&self, domain: &[u8]) -> crate::Result<Vec<u8>> {
        if self.vault.is_empty()
            || self.vault.len() > 64
            || self.vault.chars().any(char::is_control)
        {
            return Err(Error::new("credential handoff: invalid vault name"));
        }
        let mut out = domain.to_vec();
        out.push(self.vault.len() as u8);
        out.extend_from_slice(self.vault.as_bytes());
        out.extend_from_slice(&self.network);
        out.extend_from_slice(&self.request);
        out.extend_from_slice(&self.offer);
        out.extend_from_slice(&self.custodian_nonce);
        out.extend_from_slice(&self.holder_nonce);
        out.extend_from_slice(&self.custodian);
        out.extend_from_slice(&self.holder);
        out.extend_from_slice(&self.recipient.0);
        Ok(out)
    }

    /// Prove custody with the vault's current signing seed.
    ///
    /// # Errors
    /// The vault name or signing seed is invalid.
    pub fn prove(&self, seed: &[u8; 32]) -> crate::Result<[u8; 64]> {
        custody::sign(seed, &self.message(PROOF_DOMAIN)?)
    }

    /// Check custody against the applied vault signer. Callers additionally check this
    /// transcript's actors, network and recipient against current local membership.
    #[must_use]
    pub fn proven(&self, signer: &[u8; 32], signature: &[u8; 64]) -> bool {
        self.message(PROOF_DOMAIN)
            .is_ok_and(|message| astream_aead::signature::verify(signer, &message, signature))
    }

    /// The proof must also name the authenticated custodian actor and its exact
    /// recipient in the holder's current membership replica.
    #[must_use]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbCredentialDelivery",
            action = "NonCustodian",
            project = "aterm_secrets::need::transfer::tests::delivery_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbCredentialDelivery",
            action = "OtherRecipient",
            project = "aterm_secrets::need::transfer::tests::delivery_transition"
        )
    )]
    pub fn proven_for(
        &self,
        signer: &[u8; 32],
        signature: &[u8; 64],
        custodian: [u8; 48],
        recipient: age::Recipient,
    ) -> bool {
        self.custodian == custodian && self.recipient == recipient && self.proven(signer, signature)
    }

    fn delivery_message(&self, ciphertext: &[u8]) -> crate::Result<Vec<u8>> {
        let mut out = self.message(DELIVERY_DOMAIN)?;
        out.extend_from_slice(&aterm_digest::Sha256::digest(ciphertext));
        Ok(out)
    }

    /// Seal one bounded contribution to the signed recipient, attested by the holder's
    /// current writer key. Plaintext buffers are wiped on every return path.
    ///
    /// # Errors
    /// The contribution is malformed, encryption fails, or the signing seed is invalid.
    pub fn seal(&self, writer_seed: &[u8; 32], values: &Values) -> crate::Result<Vec<u8>> {
        let encoded = values.encode()?;
        let mut plain = Zeroizing::new(b"AQV1".to_vec());
        plain.extend_from_slice(&self.message(&[])?);
        plain.extend_from_slice(encoded.as_slice());
        let ciphertext = age::encrypt(&[self.recipient], &plain)?;
        let signature = custody::sign(writer_seed, &self.delivery_message(&ciphertext)?)?;
        let mut out = signature.to_vec();
        out.extend_from_slice(&ciphertext);
        if out.len() > MAX_DELIVERY {
            return Err(Error::new(
                "credential handoff: sealed delivery exceeds its bound",
            ));
        }
        Ok(out)
    }

    /// Authenticate and open a delivery under this exact transcript, within its monotonic
    /// lifetime. The caller supplies the holder's freshly checked Writer key.
    ///
    /// # Errors
    /// A bound, signature, lifetime, recipient, transcript or contribution fails validation.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbCredentialDelivery",
            action = "Valid",
            project = "aterm_secrets::need::transfer::tests::delivery_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbCredentialDelivery",
            action = "ReplayNonce",
            project = "aterm_secrets::need::transfer::tests::delivery_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbCredentialDelivery",
            action = "OtherCustodian",
            project = "aterm_secrets::need::transfer::tests::delivery_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbCredentialDelivery",
            action = "OtherOffer",
            project = "aterm_secrets::need::transfer::tests::delivery_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbCredentialDelivery",
            action = "Expired",
            project = "aterm_secrets::need::transfer::tests::delivery_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbCredentialDelivery",
            action = "WrongWriter",
            project = "aterm_secrets::need::transfer::tests::delivery_transition"
        )
    )]
    pub fn open(
        &self,
        writer: &[u8; 32],
        identity: age::Identity,
        elapsed: std::time::Duration,
        delivery: &[u8],
    ) -> crate::Result<Values> {
        let refused = || Error::new("credential handoff: delivery did not authenticate");
        if elapsed >= LIFETIME || delivery.len() < 64 || delivery.len() > MAX_DELIVERY {
            return Err(refused());
        }
        let signature = delivery[..64].try_into().map_err(|_| refused())?;
        let ciphertext = &delivery[64..];
        if identity.recipient() != self.recipient
            || !astream_aead::signature::verify(
                writer,
                &self.delivery_message(ciphertext)?,
                &signature,
            )
        {
            return Err(refused());
        }
        let plaintext = age::decrypt(&[identity], ciphertext).map_err(|_| refused())?;
        let mut prefix = b"AQV1".to_vec();
        prefix.extend_from_slice(&self.message(&[])?);
        let bytes = plaintext.as_slice();
        if !bytes.starts_with(&prefix) {
            return Err(refused());
        }
        Values::decode(&bytes[prefix.len()..])
    }
}

/// One want's entries; mailbox deliveries may include that label's TOTP alongside its
/// password. Values never implement a plaintext `Debug` or enter a replicated record.
#[derive(Debug)]
pub struct Values(pub Vec<(String, SecretBytes)>);

impl Values {
    /// Bounded canonical encoding, also used for the local 0600 give file.
    ///
    /// # Errors
    /// There are not one or two distinct valid entries, or their combined value exceeds 64KiB.
    pub fn encode(&self) -> crate::Result<SecretBytes> {
        if self.0.is_empty() || self.0.len() > 2 {
            return Err(Error::new(
                "credential handoff: one or two entries required",
            ));
        }
        let mut total = 0usize;
        // The count byte, then each entry's two length fields, its name and its value: the
        // whole encoding, allocated once, so no value is copied to grow the buffer and left
        // behind unzeroed in the one it outgrew.
        let mut exact = 1usize;
        let mut seen = std::collections::BTreeSet::new();
        for (name, value) in &self.0 {
            total = total.saturating_add(value.as_slice().len());
            if !crate::text::valid_entry_name(name)
                || name.len() > 256
                || !seen.insert(name)
                || total > MAX_VALUE
                || value.as_slice().is_empty()
            {
                return Err(Error::new(
                    "credential handoff: invalid entry or value size",
                ));
            }
            exact += 2 + name.len() + 4 + value.as_slice().len();
        }
        let mut out = Zeroizing::new(Vec::with_capacity(exact));
        let room = out.capacity();
        out.push(self.0.len() as u8);
        for (name, value) in &self.0 {
            out.extend_from_slice(&(name.len() as u16).to_be_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&(value.as_slice().len() as u32).to_be_bytes());
            out.extend_from_slice(value.as_slice());
        }
        debug_assert_eq!(
            (out.len(), out.capacity()),
            (exact, room),
            "the encoding filled the buffer it was given without growing it"
        );
        Ok(SecretBytes::new(std::mem::take(&mut *out)))
    }

    /// Read the exact bounded encoding; trailing bytes and duplicate names are refused.
    ///
    /// # Errors
    /// The encoding or its entry/value bounds are invalid.
    pub fn decode(mut bytes: &[u8]) -> crate::Result<Self> {
        fn take<'a>(bytes: &mut &'a [u8], len: usize) -> crate::Result<&'a [u8]> {
            if bytes.len() < len {
                return Err(Error::new("credential handoff: truncated contribution"));
            }
            let (head, tail) = bytes.split_at(len);
            *bytes = tail;
            Ok(head)
        }
        let count = *take(&mut bytes, 1)?.first().unwrap();
        if !(1..=2).contains(&count) || bytes.len() > MAX_VALUE + 1024 {
            return Err(Error::new("credential handoff: invalid contribution size"));
        }
        let mut values = Vec::new();
        for _ in 0..count {
            let len = u16::from_be_bytes(take(&mut bytes, 2)?.try_into().unwrap()) as usize;
            let name = std::str::from_utf8(take(&mut bytes, len)?)
                .map_err(|_| Error::new("credential handoff: invalid entry name"))?
                .to_owned();
            let len = u32::from_be_bytes(take(&mut bytes, 4)?.try_into().unwrap()) as usize;
            values.push((name, SecretBytes::from(take(&mut bytes, len)?)));
        }
        let values = Self(values);
        if !bytes.is_empty() {
            return Err(Error::new(
                "credential handoff: trailing contribution bytes",
            ));
        }
        values.encode()?;
        Ok(values)
    }
}

#[cfg(test)]
#[path = "transfer_tests.rs"]
mod tests;
