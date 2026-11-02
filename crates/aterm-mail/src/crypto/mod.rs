// SPDX-License-Identifier: Apache-2.0 AND MIT
// Copyright 2026 Andrew Yates
//! Owned Proton authentication primitives. See SOURCE.md for provenance.
mod aes;
#[allow(
    unexpected_cfgs,
    reason = "The absorbed fixsliced AES source retains its inactive compact-implementation gates"
)]
mod aes_fixslice;
mod bcrypt;
mod blowfish_constants;
mod modulus;
pub mod pgp;
pub mod pgp_signature;
pub mod srp;

pub fn mailbox_password(password: &str, salt: &[u8]) -> Result<crate::Secret, String> {
    let salt: &[u8; 16] = salt.try_into().map_err(|_| "Invalid mailbox key salt")?;
    let encoded = zeroize::Zeroizing::new(bcrypt::hash(password.as_bytes(), salt));
    // Proton uses the 31-character hash, without bcrypt's version, cost or salt.
    Ok(crate::Secret::new(encoded[29..].to_owned()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn proton_mailbox_key_password_vector() {
        let salt = aterm_codec::base64::decode("imK9IHsRcA2Zsv+yROZgbw==").unwrap();
        let password = super::mailbox_password("password", &salt).unwrap();
        assert_eq!(password.expose(), "Q.Gd9rSsqE0xQ8Qcf0Q9ckInb4hIzOu");
    }
}
