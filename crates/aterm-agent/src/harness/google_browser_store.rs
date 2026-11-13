// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Machine-encrypted Google cookie snapshots, bound to one claimed identity.

use std::path::Path;

use aterm_secrets::SecretBytes;

use super::google_identity::{Identity, Session};

pub(super) const MAX_BYTES: usize = 512 * 1024;
const FILE: &str = "session.age";
const PREFIX: &[u8] = b"aterm.google-browser.v1\0";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Unavailable;

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Protected Google browser session is unavailable")
    }
}

fn encode(identity: &Identity, cookies: &[u8]) -> Result<SecretBytes, Unavailable> {
    if cookies.is_empty() || cookies.len() > MAX_BYTES {
        return Err(Unavailable);
    }
    let mut bytes = PREFIX.to_vec();
    for field in [
        identity.id().as_bytes(),
        identity.email().as_bytes(),
        identity.credential_ref().unwrap_or_default().as_bytes(),
        cookies,
    ] {
        bytes.extend_from_slice(&(field.len() as u32).to_le_bytes());
        bytes.extend_from_slice(field);
    }
    Ok(SecretBytes::from(bytes))
}

fn decode(identity: &Identity, bytes: &[u8]) -> Result<SecretBytes, Unavailable> {
    let mut rest = bytes.strip_prefix(PREFIX).ok_or(Unavailable)?;
    let mut field = || -> Result<&[u8], Unavailable> {
        let length = u32::from_le_bytes(
            rest.get(..4)
                .ok_or(Unavailable)?
                .try_into()
                .map_err(|_| Unavailable)?,
        ) as usize;
        rest = &rest[4..];
        let value = rest.get(..length).ok_or(Unavailable)?;
        rest = &rest[length..];
        Ok(value)
    };
    for expected in [
        identity.id().as_bytes(),
        identity.email().as_bytes(),
        identity.credential_ref().unwrap_or_default().as_bytes(),
    ] {
        if field()? != expected {
            return Err(Unavailable);
        }
    }
    let cookies = field()?;
    if cookies.is_empty() || cookies.len() > MAX_BYTES || !rest.is_empty() {
        return Err(Unavailable);
    }
    Ok(SecretBytes::from(cookies))
}

/// Revision of the encrypted snapshot while its identity is exclusively claimed.
pub(super) fn fingerprint(claim: &Session) -> Result<Option<[u8; 32]>, Unavailable> {
    let bytes = aterm_secrets::fsx::read_private(&claim.profile().join(FILE), MAX_BYTES + 8192)
        .map_err(|_| Unavailable)?;
    Ok(bytes.map(|bytes| {
        let mut digest = aterm_digest::Sha256::new();
        digest.update(b"aterm.google-browser-revision.v1\0");
        digest.update(bytes.as_slice());
        digest.finalize()
    }))
}

pub(super) fn load(
    home: &Path,
    identity: &Identity,
    claim: &Session,
) -> Result<Option<SecretBytes>, Unavailable> {
    let Some(ciphertext) =
        aterm_secrets::fsx::read_private(&claim.profile().join(FILE), MAX_BYTES + 8192)
            .map_err(|_| Unavailable)?
    else {
        return Ok(None);
    };
    let paths = aterm_secrets::client::Paths::for_home(home);
    let key = aterm_secrets::client::read_machine_identity(&paths)
        .map_err(|_| Unavailable)?
        .ok_or(Unavailable)?;
    let plaintext =
        aterm_secrets::age::decrypt(&[key], ciphertext.as_slice()).map_err(|_| Unavailable)?;
    decode(identity, plaintext.as_slice()).map(Some)
}

pub(super) fn save(
    home: &Path,
    identity: &Identity,
    claim: &Session,
    cookies: &[u8],
) -> Result<(), Unavailable> {
    let plaintext = encode(identity, cookies)?;
    let paths = aterm_secrets::client::Paths::for_home(home);
    let key = aterm_secrets::client::machine_identity(&paths).map_err(|_| Unavailable)?;
    let ciphertext = aterm_secrets::age::encrypt(&[key.recipient()], plaintext.as_slice())
        .map_err(|_| Unavailable)?;
    aterm_secrets::fsx::write_atomic(&claim.profile().join(FILE), &ciphertext, 0o600)
        .map_err(|_| Unavailable)
}

#[cfg(test)]
#[path = "google_browser_store_tests.rs"]
mod tests;
