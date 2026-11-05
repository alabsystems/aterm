// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Bounded OpenPGP v4 Curve25519 mailbox-key and message decryption.
//! Plaintext is released only after the encrypted packet's MDC verifies.

use super::{
    aes,
    modulus::{armor, mpi, packet},
};
use zeroize::Zeroizing;

#[path = "pgp_encrypt.rs"]
mod encrypt;
pub use encrypt::{EncryptedBody, Recipient, public_keys};

pub struct Key {
    id: [u8; 8],
    fingerprint: [u8; 20],
    scalar: Zeroizing<[u8; 32]>,
    oid: Vec<u8>,
    hash: u8,
    cipher: u8,
    public: [u8; 32],
}

fn hash(algorithm: u8) -> Result<&'static ring::digest::Algorithm, String> {
    match algorithm {
        2 => Ok(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY),
        8 => Ok(&ring::digest::SHA256),
        10 => Ok(&ring::digest::SHA512),
        _ => Err("Unsupported OpenPGP hash".into()),
    }
}
fn key_len(algorithm: u8) -> Result<usize, String> {
    match algorithm {
        7 => Ok(16),
        8 => Ok(24),
        9 => Ok(32),
        _ => Err("Unsupported OpenPGP cipher".into()),
    }
}

/// Unlock all Curve25519 encryption subkeys in a private-key armor block.
pub fn unlock(armored: &str, password: &[u8]) -> Result<Vec<Key>, String> {
    if armored.len() > 1024 * 1024 {
        return Err("Private key exceeds limit".into());
    }
    let bytes = armor(armored, "PRIVATE KEY BLOCK")?;
    let mut rest = bytes.as_slice();
    let mut keys = Vec::new();
    while !rest.is_empty() {
        let (tag, body, tail) = packet(rest)?;
        rest = tail;
        if !matches!(tag, 5 | 7) || body.len() < 7 || body[0] != 4 || body[5] != 18 {
            continue;
        }
        let oid_len = usize::from(body[6]);
        let oid = body.get(7..7 + oid_len).ok_or("Truncated key curve")?;
        if oid != [43, 6, 1, 4, 1, 151, 85, 1, 5, 1] {
            continue;
        }
        let (public, parameters) = mpi(body.get(7 + oid_len..).ok_or("Truncated public key")?)?;
        if public.len() != 33 || public[0] != 64 {
            return Err("Unsupported Curve25519 public key".into());
        }
        let kdf = parameters.get(..4).ok_or("Truncated ECDH KDF")?;
        if kdf[0] != 3 || kdf[1] != 1 {
            return Err("Unsupported ECDH KDF".into());
        }
        let encrypted = &parameters[4..];
        let public_len = body.len() - encrypted.len();
        let mut fingerprint_hash =
            ring::digest::Context::new(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY);
        fingerprint_hash.update(&[0x99]);
        fingerprint_hash.update(&(public_len as u16).to_be_bytes());
        fingerprint_hash.update(&body[..public_len]);
        let fingerprint: [u8; 20] = fingerprint_hash
            .finish()
            .as_ref()
            .try_into()
            .map_err(|_| "Invalid fingerprint")?;
        let secret = unlock_packet(encrypted, password)?;
        let (scalar, _) = mpi(&secret)?;
        if scalar.len() > 32 {
            return Err("Invalid Curve25519 scalar".into());
        }
        let mut raw = Zeroizing::new([0u8; 32]);
        for (dst, src) in raw.iter_mut().zip(scalar.iter().rev()) {
            *dst = *src;
        }
        if !aterm_digest::ct_eq(&aterm_secrets::x25519::x25519_base(&raw), &public[1..]) {
            return Err("Private and public encryption key do not match".into());
        }
        keys.push(Key {
            id: fingerprint[12..].try_into().map_err(|_| "Invalid key ID")?,
            fingerprint,
            scalar: raw,
            oid: oid.to_vec(),
            hash: kdf[2],
            cipher: kdf[3],
            public: public[1..].try_into().map_err(|_| "Invalid public key")?,
        });
    }
    if keys.is_empty() {
        return Err("Private key has no supported Curve25519 encryption subkey".into());
    }
    Ok(keys)
}

pub(super) fn unlock_packet(bytes: &[u8], password: &[u8]) -> Result<Zeroizing<Vec<u8>>, String> {
    if password.len() > 4096 {
        return Err("Mailbox password exceeds limit".into());
    }
    let usage = *bytes.first().ok_or("Missing key protection")?;
    if usage == 0 {
        let plain = bytes.get(1..).ok_or("Truncated private key")?;
        let end = plain
            .len()
            .checked_sub(2)
            .ok_or("Truncated private-key checksum")?;
        let sum = plain[..end]
            .iter()
            .fold(0u16, |a, b| a.wrapping_add(u16::from(*b)));
        if !aterm_digest::ct_eq(&sum.to_be_bytes(), &plain[end..]) {
            return Err("Private-key checksum mismatch".into());
        }
        return Ok(Zeroizing::new(plain[..end].to_vec()));
    }
    if !matches!(usage, 254 | 255) {
        return Err("Unsupported private-key protection".into());
    }
    let cipher = *bytes.get(1).ok_or("Missing key cipher")?;
    let mode = *bytes.get(2).ok_or("Missing key derivation")?;
    let algorithm = hash(*bytes.get(3).ok_or("Missing key hash")?)?;
    let (salt, count, offset) = match mode {
        0 => (&[][..], 0usize, 4),
        1 => (bytes.get(4..12).ok_or("Truncated key salt")?, 0, 12),
        3 => {
            let c = *bytes.get(12).ok_or("Missing key iteration count")?;
            (
                bytes.get(4..12).ok_or("Truncated key salt")?,
                (16usize + usize::from(c & 15)) << (usize::from(c >> 4) + 6),
                13,
            )
        }
        _ => return Err("Unsupported private-key derivation".into()),
    };
    let mut key = Zeroizing::new(Vec::with_capacity(64));
    let length = key_len(cipher)?;
    let mut input = Zeroizing::new(Vec::with_capacity(salt.len() + password.len()));
    input.extend_from_slice(salt);
    input.extend_from_slice(password);
    for prefix in 0..4 {
        if key.len() >= length {
            break;
        }
        let mut digest = ring::digest::Context::new(algorithm);
        digest.update(&[0; 4][..prefix]);
        let mut left = count.max(input.len());
        if input.is_empty() {
            return Err("Empty key derivation input".into());
        }
        while left > 0 {
            let n = left.min(input.len());
            digest.update(&input[..n]);
            left -= n;
        }
        key.extend_from_slice(digest.finish().as_ref());
    }
    key.truncate(length);
    let iv = bytes
        .get(offset..offset + 16)
        .ok_or("Truncated private-key IV")?;
    let mut plain = aes::cfb(
        &key,
        iv,
        bytes.get(offset + 16..).ok_or("Truncated private key")?,
    )?;
    let checksum = if usage == 254 { 20 } else { 2 };
    let end = plain
        .len()
        .checked_sub(checksum)
        .ok_or("Truncated private-key checksum")?;
    let valid = if usage == 254 {
        aterm_digest::ct_eq(
            ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, &plain[..end]).as_ref(),
            &plain[end..],
        )
    } else {
        let sum = plain[..end]
            .iter()
            .fold(0u16, |a, b| a.wrapping_add(u16::from(*b)));
        aterm_digest::ct_eq(&sum.to_be_bytes(), &plain[end..])
    };
    if !valid {
        return Err("Mailbox key password did not unlock the private key".into());
    }
    plain.truncate(end);
    Ok(plain)
}

/// Decrypt an integrity-protected message or address-key token.
pub fn decrypt(armored: &str, keys: &[Key]) -> Result<Zeroizing<Vec<u8>>, String> {
    let bytes = armor(armored, "MESSAGE")?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("Encrypted message exceeds limit".into());
    }
    let mut rest = bytes.as_slice();
    let mut session = None;
    let mut encrypted = None;
    while !rest.is_empty() {
        let (tag, body, tail) = owned_packet(rest)?;
        rest = tail;
        match tag {
            1 if body.len() >= 10 && body[0] == 3 && body[9] == 18 => {
                for key in keys
                    .iter()
                    .filter(|key| body[1..9] == key.id || body[1..9] == [0u8; 8])
                {
                    if let Ok(value) = session_key(&body[10..], key) {
                        session = Some(value);
                        break;
                    }
                }
            }
            18 => encrypted = Some(body),
            _ => {}
        }
    }
    let session = session.ok_or("No matching mailbox key could decrypt the message")?;
    let encrypted = encrypted.ok_or("Message has no supported integrity-protected data")?;
    if encrypted.first() != Some(&1) {
        return Err("Unsupported encrypted message version".into());
    }
    let plain = aes::cfb(&session[1..], &[0u8; 16], &encrypted[1..])?;
    if plain.len() < 40 || plain[14..16] != plain[16..18] {
        return Err("Encrypted message prefix did not verify".into());
    }
    let end = plain.len() - 20;
    if plain[end - 2..end] != [0xd3, 0x14]
        || !aterm_digest::ct_eq(
            ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, &plain[..end]).as_ref(),
            &plain[end..],
        )
    {
        return Err("Encrypted message integrity check failed".into());
    }
    literal(&plain[18..end - 2], 0)
}

fn session_key(bytes: &[u8], key: &Key) -> Result<Zeroizing<Vec<u8>>, String> {
    let (point, bytes) = mpi(bytes)?;
    if point.len() != 33 || point[0] != 64 {
        return Err("Unsupported ephemeral key".into());
    }
    let peer: &[u8; 32] = point[1..].try_into().map_err(|_| "Invalid ECDH peer")?;
    let shared = Zeroizing::new(aterm_secrets::x25519::x25519(&key.scalar, peer));
    if aterm_digest::ct_eq(shared.as_ref(), &[0u8; 32]) {
        return Err("Invalid ECDH shared secret".into());
    }
    let mut digest = ring::digest::Context::new(hash(key.hash)?);
    digest.update(&[0, 0, 0, 1]);
    digest.update(shared.as_ref());
    digest.update(&[key.oid.len() as u8]);
    digest.update(&key.oid);
    digest.update(&[18, 3, 1, key.hash, key.cipher]);
    digest.update(b"Anonymous Sender    ");
    digest.update(&key.fingerprint);
    let derived = Zeroizing::new(digest.finish().as_ref().to_vec());
    let length = usize::from(*bytes.first().ok_or("Missing wrapped key size")?);
    let wrapped = bytes.get(1..1 + length).ok_or("Truncated wrapped key")?;
    let mut plain = aes::unwrap(&derived[..key_len(key.cipher)?], wrapped)?;
    let padding = usize::from(*plain.last().ok_or("Empty wrapped key")?);
    if padding == 0
        || padding > plain.len()
        || plain[plain.len() - padding..]
            .iter()
            .any(|b| usize::from(*b) != padding)
    {
        return Err("Invalid key padding".into());
    }
    let unpadded = plain.len() - padding;
    plain.truncate(unpadded);
    let length = key_len(*plain.first().ok_or("Empty session key")?)?;
    if plain.len() != length + 3 {
        return Err("Invalid session key length".into());
    }
    let sum = plain[1..1 + length]
        .iter()
        .fold(0u16, |a, b| a.wrapping_add(u16::from(*b)));
    if !aterm_digest::ct_eq(&sum.to_be_bytes(), &plain[1 + length..]) {
        return Err("Session key checksum mismatch".into());
    }
    plain.truncate(1 + length);
    Ok(plain)
}

fn literal(bytes: &[u8], depth: usize) -> Result<Zeroizing<Vec<u8>>, String> {
    if depth > 8 {
        return Err("Compressed message nesting exceeds limit".into());
    }
    let mut rest = bytes;
    while !rest.is_empty() {
        let (tag, body, tail) = owned_packet(rest)?;
        rest = tail;
        if tag == 11 {
            let name = usize::from(*body.get(1).ok_or("Truncated literal")?);
            return Ok(Zeroizing::new(
                body.get(6 + name..)
                    .ok_or("Truncated literal header")?
                    .to_vec(),
            ));
        }
        if tag == 8 {
            let data = body.get(1..).ok_or("Missing compressed data")?;
            let decompressed = match body.first() {
                Some(0) => data.to_vec(),
                Some(1) => aterm_codec::inflate::inflate(data, 32 * 1024 * 1024)
                    .map_err(|_| "Invalid compressed message")?,
                Some(2) => aterm_codec::inflate::zlib_decompress(data, 32 * 1024 * 1024)
                    .map_err(|_| "Invalid compressed message")?,
                _ => return Err("Unsupported message compression".into()),
            };
            return literal(&Zeroizing::new(decompressed), depth + 1);
        }
    }
    Err("Message contains no literal text".into())
}

type PacketSlice<'a> = (u8, std::borrow::Cow<'a, [u8]>, &'a [u8]);

fn owned_packet(input: &[u8]) -> Result<PacketSlice<'_>, String> {
    if input.len() < 2 || input[0] & 0xc0 != 0xc0 || !(224..=254).contains(&input[1]) {
        let (tag, body, rest) = packet(input)?;
        return Ok((tag, std::borrow::Cow::Borrowed(body), rest));
    }
    let tag = input[0] & 63;
    let mut at = 1usize;
    let mut body = Vec::new();
    loop {
        let first = *input.get(at).ok_or("Truncated partial PGP packet")?;
        at += 1;
        let (partial, length) = match first {
            0..=191 => (false, usize::from(first)),
            192..=223 => {
                let next = *input.get(at).ok_or("Truncated PGP length")?;
                at += 1;
                (
                    false,
                    ((usize::from(first) - 192) << 8) + usize::from(next) + 192,
                )
            }
            224..=254 => (
                true,
                1usize
                    .checked_shl(u32::from(first & 31))
                    .ok_or("PGP length overflow")?,
            ),
            255 => {
                let bytes = input.get(at..at + 4).ok_or("Truncated PGP length")?;
                at += 4;
                (
                    false,
                    u32::from_be_bytes(bytes.try_into().map_err(|_| "Invalid PGP length")?)
                        as usize,
                )
            }
        };
        if body.len().saturating_add(length) > 32 * 1024 * 1024 {
            return Err("PGP packet exceeds size limit".into());
        }
        let end = at.checked_add(length).ok_or("PGP length overflow")?;
        body.extend_from_slice(input.get(at..end).ok_or("Truncated partial PGP body")?);
        at = end;
        if !partial {
            return Ok((tag, std::borrow::Cow::Owned(body), &input[at..]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decrypts_proton_published_message_and_rejects_damage() {
        let keys = unlock(include_str!("../../tests/fixtures/proton-key-v4.asc"), b"").unwrap();
        let message = include_str!("../../tests/fixtures/proton-message-v4.asc");
        assert_eq!(&*decrypt(message, &keys).unwrap(), b"hello world");
        let mut bytes = armor(message, "MESSAGE").unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        let encoded = aterm_codec::base64::encode(&bytes).unwrap();
        let changed =
            format!("-----BEGIN PGP MESSAGE-----\n\n{encoded}\n-----END PGP MESSAGE-----");
        assert!(decrypt(&changed, &keys).is_err());
    }
}
