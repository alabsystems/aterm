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
    fn blocks(&self, batch: &[[u8; 16]; 4], decrypt: bool) -> [[u8; 16]; 4] {
        match (self, decrypt) {
            (Self::Aes128(k), false) => core::aes128_encrypt(k, batch),
            (Self::Aes128(k), true) => core::aes128_decrypt(k, batch),
            (Self::Aes192(k), false) => core::aes192_encrypt(k, batch),
            (Self::Aes192(k), true) => core::aes192_decrypt(k, batch),
            (Self::Aes256(k), false) => core::aes256_encrypt(k, batch),
            (Self::Aes256(k), true) => core::aes256_decrypt(k, batch),
        }
    }
    fn block(&self, block: [u8; 16], decrypt: bool) -> [u8; 16] {
        let mut out = self.blocks(&[block; 4], decrypt);
        let first = out[0];
        out.zeroize();
        first
    }
}

pub(super) fn cfb(key: &[u8], iv: &[u8], ciphertext: &[u8]) -> Result<Zeroizing<Vec<u8>>, String> {
    let key = Key::new(key)?;
    let mut feedback: [u8; 16] = iv.try_into().map_err(|_| "Invalid AES IV")?;
    let mut plain = Zeroizing::new(Vec::with_capacity(ciphertext.len()));
    let (quads, rem) = ciphertext.as_chunks::<64>();
    for quad in quads {
        let [c0, c1, c2, c3] = *quad.as_chunks::<16>().0.first_chunk::<4>().unwrap();
        let mut masks = key.blocks(&[feedback, c0, c1, c2], false);
        for (mask, block) in masks.iter_mut().zip([c0, c1, c2, c3]) {
            for (m, c) in mask.iter_mut().zip(block) {
                *m ^= c;
            }
        }
        plain.extend_from_slice(masks.as_flattened());
        feedback = c3;
        masks.zeroize();
    }
    for chunk in rem.chunks(16) {
        let mut mask = key.block(feedback, false);
        for (m, &c) in mask[..chunk.len()].iter_mut().zip(chunk) {
            *m ^= c;
        }
        plain.extend_from_slice(&mask[..chunk.len()]);
        feedback[..chunk.len()].copy_from_slice(chunk);
        mask.zeroize();
    }
    feedback.zeroize();
    Ok(plain)
}

pub(super) fn encrypt_cfb_into(
    key: &[u8],
    iv: &[u8],
    plaintext: &[u8],
    output: &mut Vec<u8>,
) -> Result<(), String> {
    let key = Key::new(key)?;
    let mut feedback: [u8; 16] = iv.try_into().map_err(|_| "Invalid AES IV")?;
    output.reserve(plaintext.len());
    for chunk in plaintext.chunks(16) {
        let mut mask = key.block(feedback, false);
        for (i, byte) in chunk.iter().enumerate() {
            feedback[i] = byte ^ mask[i];
        }
        output.extend_from_slice(&feedback[..chunk.len()]);
        mask.zeroize();
    }
    feedback.zeroize();
    Ok(())
}

pub(super) fn wrap(key: &[u8], input: &[u8]) -> Result<Vec<u8>, String> {
    if input.len() < 16 || !input.len().is_multiple_of(8) {
        return Err("Invalid session key size".into());
    }
    let key = Key::new(key)?;
    let n = input.len() / 8;
    let mut a = [0xa6u8; 8];
    let mut output = Vec::with_capacity(8 + input.len());
    output.extend_from_slice(&a);
    output.extend_from_slice(input);
    for j in 0..6 {
        for i in 1..=n {
            let mut block = [0u8; 16];
            block[..8].copy_from_slice(&a);
            block[8..].copy_from_slice(&output[8 * i..8 * (i + 1)]);
            let mut encoded = key.block(block, false);
            block.zeroize();
            let t = ((n * j + i) as u64).to_be_bytes();
            for k in 0..8 {
                a[k] = encoded[k] ^ t[k];
            }
            output[8 * i..8 * (i + 1)].copy_from_slice(&encoded[8..]);
            encoded.zeroize();
        }
    }
    output[..8].copy_from_slice(&a);
    a.zeroize();
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
            block.zeroize();
            a.copy_from_slice(&plain[..8]);
            output[8 * (i - 1)..8 * i].copy_from_slice(&plain[8..]);
            plain.zeroize();
        }
    }
    let ok = aterm_digest::ct_eq(&a, &[0xa6; 8]);
    a.zeroize();
    if !ok {
        return Err("Wrapped key integrity check failed".into());
    }
    Ok(output)
}

#[cfg(test)]
#[path = "aes_tests.rs"]
mod tests;
