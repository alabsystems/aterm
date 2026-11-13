// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A root-authorized, device-sealed local intake. This domain carries no
//! account decision, membership, allocation, or execution permission.

use super::*;
use crate::commons::binding::Binding;
use ring::signature::Ed25519KeyPair;

const MAGIC: &[u8; 4] = b"AKI1";
const DOMAIN: &[u8] = b"aterm/native-account-input/1\0";
const MAX: usize = 128 * 1024;

fn native_entry(name: &str) -> crate::Result<()> {
    if name == crate::entry_class::AI_NATIVE_POOL || crate::entry_class::native_api_key(name) {
        Ok(())
    } else {
        Err(Error::new("native account entry has another purpose"))
    }
}

/// Publish one native account input or allocation entry with the normal signed
/// vault version/CAS rules and one shared deadline. Other document state comes
/// only from the current opened Admin, never from a saved pending draft.
pub fn admin_set_entry_until(
    paths: &Paths,
    admin: &Admin,
    name: &str,
    expected: Option<[u8; 32]>,
    value: &[u8],
    deadline: Instant,
) -> crate::Result<Published> {
    publication_current(Some(deadline))?;
    let actual = admin.native_entry_digest(name)?;
    if actual != expected {
        return Err(Error::new(
            "native account entry changed before publication",
        ));
    }
    let mut doc = admin.published_doc.clone();
    doc.set_entry(&Audience::Everyone, name, value)
        .map_err(|e| Error::new(e.to_string()))?;
    publish_as_with(
        paths,
        admin,
        &doc,
        false,
        &[],
        false,
        &[],
        None,
        Some(deadline),
    )
}

impl Admin {
    /// Return the published baseline for one native preparation or allocation
    /// entry. Unpublished edits in this Admin are never used for a CAS receipt.
    pub fn native_entry_digest(&self, name: &str) -> crate::Result<Option<[u8; 32]>> {
        native_entry(name)?;
        Ok(self
            .published_doc
            .entry(&Audience::Everyone, name)
            .map(|entry| aterm_digest::Sha256::digest(entry.value.as_slice())))
    }

    /// Seal an explicitly authorized intake to one exact device binding. The
    /// caller retains these bytes durably before performing its first effect.
    /// A later consumer must recheck current root, binding, target and policy.
    pub fn seal_account_input(&self, device: &Binding, payload: &[u8]) -> crate::Result<Vec<u8>> {
        if payload.len() > MAX || device.authority() != &self.commons_authority()? {
            return Err(Error::new(
                "account input has another authority or exceeds its bound",
            ));
        }
        let sealed = crate::age::encrypt(&[device.recipient()], payload)?;
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&device.bytes());
        bytes.extend_from_slice(&(sealed.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&sealed);
        let mut signed = DOMAIN.to_vec();
        signed.extend_from_slice(&bytes);
        let signer = Ed25519KeyPair::from_seed_unchecked(&self.commons_seed)
            .map_err(|_| Error::new("account input signer is unavailable"))?;
        bytes.extend_from_slice(signer.sign(&signed).as_ref());
        Ok(bytes)
    }
}

/// Verify against the caller's current authenticated authority and exact
/// binding before decrypting. A signer named by untrusted input is never used.
/// The returned bytes authorize no effect without the application's current
/// target, source, policy and replay checks.
pub fn open_account_input(
    authority: &[u8; 32],
    device: &Binding,
    identity: &Identity,
    bytes: &[u8],
) -> crate::Result<SecretBytes> {
    let binding = device.bytes();
    let prefix = MAGIC.len() + binding.len() + 4;
    if device.authority() != authority
        || identity.recipient() != device.recipient()
        || bytes.len() < prefix + 64
        || bytes.len() > MAX + 4096
        || &bytes[..4] != MAGIC
        || bytes[4..4 + binding.len()] != binding
    {
        return Err(Error::new(
            "account input has another root, binding or frame",
        ));
    }
    let size = u32::from_le_bytes(bytes[prefix - 4..prefix].try_into().unwrap()) as usize;
    if size > MAX + 1024 || prefix + size + 64 != bytes.len() {
        return Err(Error::new("account input frame length is invalid"));
    }
    let mut signed = DOMAIN.to_vec();
    signed.extend_from_slice(&bytes[..prefix + size]);
    if !crate::ed25519::verifies(authority, &signed, &bytes[prefix + size..]) {
        return Err(Error::new("account input root authorization is invalid"));
    }
    let plain = crate::age::decrypt(
        std::slice::from_ref(identity),
        &bytes[prefix..prefix + size],
    )
    .map_err(|_| Error::new("account input cannot be opened by this device"))?;
    if plain.len() > MAX {
        return Err(Error::new("account input exceeds its bound"));
    }
    Ok(plain)
}

#[cfg(test)]
#[path = "account_input_tests.rs"]
mod tests;
