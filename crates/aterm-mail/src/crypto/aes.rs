// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Mail's AES block operations, CFB and RFC 3394 key unwrap.
use super::aes_fixslice as core;
use zeroize::{Zeroize, Zeroizing};

enum Key {
    Aes128([u64; 88]),
    Aes192([u64; 104]),
    Aes256([u64; 120]),
}
impl Drop for Key {
    fn drop(&mut self) {
        match self {
            Self::Aes128(k) => k.zeroize(),
            Self::Aes192(k) => k.zeroize(),
            Self::Aes256(k) => k.zeroize(),
        }
    }
}
impl Key {
    fn new(key: &[u8]) -> Result<Self, String> {
        Ok(match key.len() {
            16 => Self::Aes128(core::aes128_key_schedule(
                key.try_into().map_err(|_| "Invalid AES key")?,
            )),
            24 => Self::Aes192(core::aes192_key_schedule(
                key.try_into().map_err(|_| "Invalid AES key")?,
            )),
            32 => Self::Aes256(core::aes256_key_schedule(
                key.try_into().map_err(|_| "Invalid AES key")?,
            )),
            _ => return Err("Unsupported AES key size".into()),
        })
    }
    fn block(&self, block: [u8; 16], decrypt: bool) -> [u8; 16] {
        let batch = [block; 4];
        match (self, decrypt) {
            (Self::Aes128(k), false) => core::aes128_encrypt(k, &batch)[0],
            (Self::Aes128(k), true) => core::aes128_decrypt(k, &batch)[0],
            (Self::Aes192(k), false) => core::aes192_encrypt(k, &batch)[0],
            (Self::Aes192(k), true) => core::aes192_decrypt(k, &batch)[0],
            (Self::Aes256(k), false) => core::aes256_encrypt(k, &batch)[0],
            (Self::Aes256(k), true) => core::aes256_decrypt(k, &batch)[0],
        }
    }
}

pub(super) fn cfb(key: &[u8], iv: &[u8], ciphertext: &[u8]) -> Result<Zeroizing<Vec<u8>>, String> {
    let key = Key::new(key)?;
    let mut feedback: [u8; 16] = iv.try_into().map_err(|_| "Invalid AES IV")?;
    let mut plain = Zeroizing::new(Vec::with_capacity(ciphertext.len()));
    for chunk in ciphertext.chunks(16) {
        let mut mask = key.block(feedback, false);
        plain.extend(chunk.iter().zip(&mask).map(|(c, m)| c ^ m));
        feedback[..chunk.len()].copy_from_slice(chunk);
        mask.zeroize();
    }
    feedback.zeroize();
    Ok(plain)
}

pub(super) fn encrypt_cfb(key: &[u8], iv: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let key = Key::new(key)?;
    let mut feedback: [u8; 16] = iv.try_into().map_err(|_| "Invalid AES IV")?;
    let mut output = Vec::with_capacity(plaintext.len());
    for chunk in plaintext.chunks(16) {
        let mut mask = key.block(feedback, false);
        for (i, byte) in chunk.iter().enumerate() {
            feedback[i] = byte ^ mask[i];
            output.push(feedback[i]);
        }
        mask.zeroize();
    }
    feedback.zeroize();
    Ok(output)
}

pub(super) fn wrap(key: &[u8], input: &[u8]) -> Result<Vec<u8>, String> {
    if input.len() < 16 || !input.len().is_multiple_of(8) {
        return Err("Invalid session key size".into());
    }
    let key = Key::new(key)?;
    let n = input.len() / 8;
    let mut a = [0xa6u8; 8];
    let mut result = input.to_vec();
    for j in 0..6 {
        for i in 1..=n {
            let mut block = [0u8; 16];
            block[..8].copy_from_slice(&a);
            block[8..].copy_from_slice(&result[(i - 1) * 8..i * 8]);
            let mut encoded = key.block(block, false);
            let t = ((n * j + i) as u64).to_be_bytes();
            for k in 0..8 {
                a[k] = encoded[k] ^ t[k];
            }
            result[(i - 1) * 8..i * 8].copy_from_slice(&encoded[8..]);
            encoded.zeroize();
        }
    }
    let mut output = a.to_vec();
    output.extend_from_slice(&result);
    result.zeroize();
    Ok(output)
}

pub(super) fn unwrap(key: &[u8], input: &[u8]) -> Result<Zeroizing<Vec<u8>>, String> {
    if input.len() < 24 || !input.len().is_multiple_of(8) {
        return Err("Invalid wrapped key size".into());
    }
    let key = Key::new(key)?;
    let n = input.len() / 8 - 1;
    let mut a: [u8; 8] = input[..8].try_into().map_err(|_| "Invalid wrapped key")?;
    let mut output = Zeroizing::new(input[8..].to_vec());
    for j in (0..6).rev() {
        for i in (1..=n).rev() {
            let t = (n * j + i) as u64;
            let mut block = [0u8; 16];
            for (dst, (value, mask)) in block[..8].iter_mut().zip(a.iter().zip(t.to_be_bytes())) {
                *dst = value ^ mask;
            }
            block[8..].copy_from_slice(&output[8 * (i - 1)..8 * i]);
            let mut plain = key.block(block, true);
            a.copy_from_slice(&plain[..8]);
            output[8 * (i - 1)..8 * i].copy_from_slice(&plain[8..]);
            plain.zeroize();
        }
    }
    if !aterm_digest::ct_eq(&a, &[0xa6; 8]) {
        return Err("Wrapped key integrity check failed".into());
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }
    #[test]
    fn fips_aes_and_rfc3394() {
        let key = Key::new(&hex("000102030405060708090a0b0c0d0e0f")).unwrap();
        let plain: [u8; 16] = hex("00112233445566778899aabbccddeeff").try_into().unwrap();
        let cipher: [u8; 16] = hex("69c4e0d86a7b0430d8cdb78070b4c55a").try_into().unwrap();
        assert_eq!(key.block(plain, false), cipher);
        assert_eq!(key.block(cipher, true), plain);
        let wrap = hex("1fa68b0a8112b447aef34bd8fb5a7b829d3e862371d2cfe5");
        assert_eq!(
            &*unwrap(&hex("000102030405060708090a0b0c0d0e0f"), &wrap).unwrap(),
            &hex("00112233445566778899aabbccddeeff")
        );
        let mut damaged = wrap;
        damaged[0] ^= 1;
        assert!(unwrap(&hex("000102030405060708090a0b0c0d0e0f"), &damaged).is_err());
    }
}
