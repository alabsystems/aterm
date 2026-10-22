// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The join password (docs/DESIGN-secrets-2026-10-20.md §7): a vault may hold one password in
//! its admin-only `[join]` section. A machine whose person types it sends, with its join
//! request, a proof: HMAC-SHA256 under a key stretched from the password (PBKDF2-HMAC-SHA256,
//! salted with the vault's name) over the vault's name, the machine's key and its host name.
//! An admin's subscriber holds the password, recomputes the proof for each queued request and
//! approves the ones that match, so a machine that knows the password is in within seconds
//! and a request whose key or name was changed on the wire matches nothing.
//!
//! The proof crosses the LAN in the clear, so a password guessed offline from it would admit
//! a stranger: [`generate`] makes 100-bit ones, and [`strength`] refuses short ones.

use crate::SecretBytes;
use ring::pbkdf2;
use std::num::NonZeroU32;

/// PBKDF2 iterations: about a tenth of a second for a machine joining once, and an offline
/// guess costs the same.
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

impl Key {
    /// Stretch `password` for `vault`. Surrounding blanks are not part of a password.
    pub fn derive(vault: &str, password: &[u8]) -> Key {
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

    /// The proof a machine with `recipient` asking to be `host` sends: padded base64.
    pub fn prove(&self, vault: &str, recipient: &str, host: &str) -> String {
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &self.0);
        let tag = ring::hmac::sign(&key, &message(vault, recipient, host));
        crate::b64::encode_padded(tag.as_ref())
    }

    /// Whether `proof` is this key's for the request, compared in constant time. A field
    /// holding a LF verifies nothing: the message joins the fields with LF, so such a field
    /// would let a proof made for one request pass for another.
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

/// A new password: 20 characters (100 bits) from a 32-letter alphabet without `l`, `o`, `0`
/// and `1`, in four groups, `xxxxx-xxxxx-xxxxx-xxxxx`: easy to read off one screen and type
/// on another.
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

/// Why `password` will not do as a join password, if it will not: shorter than [`MIN_LEN`]
/// or longer than [`MAX_LEN`] (blanks around it aside), or holding a control character.
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
mod tests {
    use super::*;

    const RECIPIENT: &str = "age1zvkyg2lqzraa2lnjvqej32nkuu0ues2s82hzrye869xeexvn73equnujwj";

    #[test]
    fn a_proof_binds_the_vault_the_key_and_the_host() {
        let key = Key::derive("alab", b"correct horse battery");
        let proof = key.prove("alab", RECIPIENT, "m9");
        assert!(key.verify("alab", RECIPIENT, "m9", &proof));
        assert!(
            !key.verify("alab", RECIPIENT, "m10", &proof),
            "another host"
        );
        assert!(
            !key.verify("other", RECIPIENT, "m9", &proof),
            "another vault"
        );
        let other = RECIPIENT.replace("age1zv", "age1zw");
        assert!(!key.verify("alab", &other, "m9", &proof), "another key");
        let wrong = Key::derive("alab", b"correct horse battery staple");
        assert!(
            !wrong.verify("alab", RECIPIENT, "m9", &proof),
            "another password"
        );
        assert!(!key.verify("alab", RECIPIENT, "m9", "not base64"));
        assert!(!key.verify("alab", RECIPIENT, "m9", ""));
    }

    #[test]
    fn a_line_break_inside_a_field_never_moves_a_proof_to_another_request() {
        let key = Key::derive("alab", b"correct horse battery");
        // Both spell `alab-vault/1 join\nalab\nage1x\nm9\nz\n` once the fields are joined.
        let proof = key.prove("alab", "age1x\nm9", "z");
        assert!(!key.verify("alab", "age1x", "m9\nz", &proof));
        assert!(!key.verify("alab", "age1x\nm9", "z", &proof));
        let proof = key.prove("a\nb", RECIPIENT, "m9");
        assert!(!key.verify("a", &format!("b\n{RECIPIENT}"), "m9", &proof));
    }

    #[test]
    fn blanks_around_a_password_do_not_count() {
        let typed = Key::derive("alab", b"  correct horse battery\n");
        let stored = Key::derive("alab", b"correct horse battery");
        assert_eq!(
            typed.prove("alab", RECIPIENT, "m9"),
            stored.prove("alab", RECIPIENT, "m9")
        );
    }

    #[test]
    fn generated_passwords_are_strong_distinct_and_grouped() {
        let a = generate().expect("random");
        let b = generate().expect("random");
        assert_ne!(a.as_slice(), b.as_slice());
        let text = a.as_str().expect("utf-8");
        assert_eq!(text.len(), 23);
        assert_eq!(text.split('-').count(), 4);
        assert!(strength(a.as_slice()).is_ok());
    }

    #[test]
    fn weak_passwords_are_refused_without_echoing_them() {
        for weak in [&b"short"[..], b"   eleven-ch   ", b"line\nbreak-here!", b""] {
            let why = strength(weak).expect_err("refused");
            assert!(!why.contains("short") && !why.contains("eleven"), "{why}");
        }
        assert!(strength(b"twelve-chars").is_ok());
        assert!(strength(&[b'x'; 257]).is_err());
    }

    #[test]
    fn debug_never_shows_the_key() {
        let key = Key::derive("alab", b"correct horse battery");
        assert_eq!(format!("{key:?}"), "join::Key(<redacted>)");
    }
}
