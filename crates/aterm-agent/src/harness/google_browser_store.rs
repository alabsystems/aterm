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
mod tests {
    use super::*;

    fn make_identity(email: &str, reference: &str) -> Identity {
        super::super::google_identity::Registry::parse(&format!("[[identities]]\nid='one'\nemail='{email}'\ncredential_ref='{reference}'\n[[bindings]]\nprovider='fixture'\naccount='one'\nidentity='one'\nexpected_email='{email}'\n")).unwrap().resolve("fixture", "one").unwrap().0.clone()
    }

    #[test]
    fn encrypted_snapshot_survives_restart_but_refuses_substitution_and_damage() {
        let home = aterm_tempfile::tempdir().unwrap();
        let state = aterm_tempfile::tempdir().unwrap();
        let identity = make_identity("one@example.com", "registered-one");
        let claim = identity.claim(state.path()).unwrap();
        let path = claim.profile().join(FILE);
        assert!(load(home.path(), &identity, &claim).unwrap().is_none());
        let cookies = b"[{\"name\":\"fixture\",\"value\":\"never-plaintext-on-disk\"}]";
        save(home.path(), &identity, &claim, cookies).unwrap();
        let mut ciphertext = std::fs::read(&path).unwrap();
        assert!(
            !ciphertext
                .windows(b"never-plaintext-on-disk".len())
                .any(|bytes| bytes == b"never-plaintext-on-disk")
        );
        drop(claim);
        let claim = identity.claim(state.path()).unwrap();
        assert_eq!(
            load(home.path(), &identity, &claim)
                .unwrap()
                .unwrap()
                .as_slice(),
            cookies
        );
        let other = make_identity("other@example.com", "registered-one");
        assert!(load(home.path(), &other, &claim).is_err());
        let changed = make_identity("one@example.com", "registered-replacement");
        assert!(load(home.path(), &changed, &claim).is_err());
        let wrong_machine = aterm_tempfile::tempdir().unwrap();
        aterm_secrets::client::machine_identity(&aterm_secrets::client::Paths::for_home(
            wrong_machine.path(),
        ))
        .unwrap();
        assert!(load(wrong_machine.path(), &identity, &claim).is_err());
        *ciphertext.last_mut().unwrap() ^= 1;
        std::fs::write(&path, ciphertext).unwrap();
        assert!(load(home.path(), &identity, &claim).is_err());
    }
}
