// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Vault join password generation ([`generate`]), validation ([`strength`]), and HMAC proof.

use crate::SecretBytes;
use ring::pbkdf2;
use std::num::NonZeroU32;

/// PBKDF2 iterations.
pub const ITERATIONS: u32 = 210_000;

/// The shortest password [`strength`] accepts.
pub const MIN_LEN: usize = 12;

/// The longest password a `[join]` line holds.
pub const MAX_LEN: usize = 256;

/// The key a password stretches to for `vault`.
pub struct Key([u8; 32]);

impl Drop for Key {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.0);
    }
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("join::Key(<redacted>)")
    }
}

fn salt(vault: &str) -> Vec<u8> {
    format!("alab-vault/1 join salt\n{vault}\n").into_bytes()
}

fn message(vault: &str, recipient: &str, host: &str) -> Vec<u8> {
    format!("alab-vault/1 join\n{vault}\n{recipient}\n{host}\n").into_bytes()
}

#[cfg(test)]
thread_local! {
    /// How many times [`Key::derive`] stretched a password on this test thread.
    pub(crate) static DERIVED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl Key {
    /// Stretch `password` for `vault`.
    pub fn derive(vault: &str, password: &[u8]) -> Key {
        #[cfg(test)]
        DERIVED.with(|derived| derived.set(derived.get() + 1));
        let password = password.trim_ascii();
        let mut out = [0u8; 32];
        pbkdf2::derive(
            ring::pbkdf2::PBKDF2_HMAC_SHA256,
            NonZeroU32::new(ITERATIONS).unwrap_or(NonZeroU32::MIN),
            &salt(vault),
            password,
            &mut out,
        );
        Key(out)
    }

    /// The proof a machine with `recipient` asking to be `host` sends.
    pub fn prove(&self, vault: &str, recipient: &str, host: &str) -> String {
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &self.0);
        let tag = ring::hmac::sign(&key, &message(vault, recipient, host));
        crate::b64::encode_padded(tag.as_ref())
    }

    /// Whether `proof` is this key's for the request, compared in constant time.
    pub fn verify(&self, vault: &str, recipient: &str, host: &str, proof: &str) -> bool {
        if [vault, recipient, host]
            .iter()
            .any(|field| field.contains('\n'))
        {
            return false;
        }
        let Some(tag) = crate::b64::decode_padded(proof) else {
            return false;
        };
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &self.0);
        ring::hmac::verify(&key, &message(vault, recipient, host), &tag).is_ok()
    }
}

/// What names a proof in an admin Mac's verdict on it (`POST <url>/join/verdict`): the first 8
/// bytes of its SHA-256, in lowercase hex. It is no secret: the join listing shows the proof.
pub fn proof_tag(proof: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, proof.as_bytes());
    aterm_codec::hex::encode_lower(&digest.as_ref()[..8])
}

/// A new password: 20 random characters (100 bits) in four dash-separated groups of five.
pub fn generate() -> std::io::Result<SecretBytes> {
    const ALPHABET: &[u8; 32] = b"abcdefghijkmnpqrstuvwxyz23456789";
    let mut random = [0u8; 20];
    aterm_uds::rand::fill(&mut random)?;
    let mut out = Vec::with_capacity(23);
    for (i, byte) in random.iter().enumerate() {
        if i > 0 && i % 5 == 0 {
            out.push(b'-');
        }
        out.push(ALPHABET[usize::from(byte & 31)]);
    }
    zeroize::Zeroize::zeroize(&mut random);
    Ok(SecretBytes::new(out))
}

/// Why `password` will not do as a join password.
pub fn strength(password: &[u8]) -> Result<(), String> {
    let password = password.trim_ascii();
    if password.len() < MIN_LEN {
        return Err(format!(
            "a join password needs at least {MIN_LEN} characters (it crosses the LAN as a proof an eavesdropper can test guesses against)"
        ));
    }
    if password.len() > MAX_LEN {
        return Err(format!("a join password is at most {MAX_LEN} bytes"));
    }
    if password.iter().any(|b| b.is_ascii_control()) {
        return Err("a join password is one line of printable characters".into());
    }
    if std::str::from_utf8(password).is_err() {
        return Err("a join password is UTF-8 text".into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "join_tests.rs"]
mod tests;
