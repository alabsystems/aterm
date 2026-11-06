// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Verifies Proton's clear-signed SRP modulus against its pinned Ed25519 key.

const KEY: &str = include_str!("proton-modulus-key.asc");

pub(super) fn verify(signed: &str) -> Result<Vec<u8>, String> {
    if signed.len() > 16 * 1024 {
        return Err("SRP modulus exceeds limit".into());
    }
    let signed: std::borrow::Cow<'_, str> = if signed.contains('\r') {
        std::borrow::Cow::Owned(signed.replace("\r\n", "\n"))
    } else {
        std::borrow::Cow::Borrowed(signed)
    };
    let body = signed
        .strip_prefix("-----BEGIN PGP SIGNED MESSAGE-----\n")
        .ok_or("Missing signed SRP modulus")?;
    let (_, body) = body
        .split_once("\n\n")
        .ok_or("Malformed signed SRP modulus")?;
    let (text, signature) = body
        .split_once("\n-----BEGIN PGP SIGNATURE-----")
        .ok_or("Missing SRP modulus signature")?;
    // A modulus is one base64 line. Refuse cleartext transformations and added data.
    if text.contains(['\n', '\r']) {
        return Err("Malformed SRP modulus text".into());
    }
    let signature = armor_body(signature, "SIGNATURE")?;
    let (tag, sig, rest) = packet(&signature)?;
    if tag != 2 || !rest.is_empty() || sig.len() < 10 || sig[..3] != [4, 1, 22] {
        return Err("Unsupported SRP modulus signature".into());
    }
    let algorithm = match sig[3] {
        8 => &ring::digest::SHA256,
        10 => &ring::digest::SHA512,
        _ => return Err("Unsupported SRP modulus signature hash".into()),
    };
    let hashed_len = u16::from_be_bytes([sig[4], sig[5]]) as usize;
    let end = 6usize
        .checked_add(hashed_len)
        .ok_or("Invalid signature length")?;
    let hashed = sig.get(..end).ok_or("Truncated SRP signature")?;
    let mut cursor = sig.get(end..).ok_or("Truncated SRP signature")?;
    if cursor.len() < 2 {
        return Err("Truncated SRP signature".into());
    }
    let unhashed_len = u16::from_be_bytes([cursor[0], cursor[1]]) as usize;
    cursor = cursor
        .get(2 + unhashed_len..)
        .ok_or("Truncated SRP signature")?;
    if cursor.len() < 2 {
        return Err("Truncated signature hash".into());
    }
    let prefix = &cursor[..2];
    cursor = &cursor[2..];
    let (r, next) = mpi(cursor)?;
    let (s, next) = mpi(next)?;
    if !next.is_empty() || r.len() > 32 || s.len() > 32 {
        return Err("Invalid Ed25519 signature size".into());
    }
    let mut raw_signature = [0u8; 64];
    raw_signature[32 - r.len()..32].copy_from_slice(r);
    raw_signature[64 - s.len()..].copy_from_slice(s);
    let mut hash = ring::digest::Context::new(algorithm);
    hash.update(text.as_bytes());
    hash.update(hashed);
    hash.update(&[4, 255]);
    hash.update(&(end as u32).to_be_bytes());
    let digest = hash.finish();
    if !aterm_digest::ct_eq(&digest.as_ref()[..2], prefix) {
        return Err("SRP modulus digest mismatch".into());
    }
    let key = pinned_modulus_key()?;
    if !astream_aead::signature::verify(key, digest.as_ref(), &raw_signature) {
        return Err("Proton modulus signature did not verify".into());
    }
    let modulus = aterm_codec::base64::decode(text).map_err(|_| "Invalid SRP modulus encoding")?;
    if modulus.len() != 256 {
        return Err("Invalid SRP modulus length".into());
    }
    Ok(modulus)
}

fn pinned_modulus_key() -> Result<&'static [u8; 32], String> {
    static PINNED: std::sync::OnceLock<Result<[u8; 32], String>> = std::sync::OnceLock::new();
    PINNED
        .get_or_init(|| {
            let key = armor(KEY, "PUBLIC KEY BLOCK")?;
            let (tag, key, _) = packet(&key)?;
            const CURVE: &[u8] = &[9, 43, 6, 1, 4, 1, 218, 71, 15, 1];
            if tag != 6
                || key.len() != 51
                || key[0] != 4
                || key[5] != 22
                || key.get(6..16) != Some(CURVE)
                || key[18] != 64
            {
                return Err("Invalid pinned Proton modulus key".into());
            }
            key[19..]
                .try_into()
                .map_err(|_| "Invalid pinned Proton modulus key".into())
        })
        .as_ref()
        .map_err(Clone::clone)
}

pub(super) fn armor(text: &str, label: &str) -> Result<Vec<u8>, String> {
    let text = text
        .strip_prefix("-----BEGIN PGP ")
        .and_then(|s| s.strip_prefix(label))
        .and_then(|s| s.strip_prefix("-----"))
        .ok_or("Invalid PGP armor")?;
    armor_body(text, label)
}

fn armor_body(text: &str, label: &str) -> Result<Vec<u8>, String> {
    let (body, trailing) = split_armor_end(text, label).ok_or("Unclosed PGP armor")?;
    if !trailing.trim().is_empty() {
        return Err("Trailing data in PGP armor".into());
    }
    let body = split_armor_header(body).ok_or("Missing PGP armor separator")?;
    let mut encoded = zeroize::Zeroizing::new(String::with_capacity(body.len()));
    for line in body.lines().take_while(|line| !line.starts_with('=')) {
        encoded.push_str(line);
    }
    aterm_codec::base64::decode(encoded.trim()).map_err(|_| "Invalid PGP armor encoding".into())
}

fn split_armor_header(body: &str) -> Option<&str> {
    let bytes = body.as_bytes();
    for i in 0..bytes.len().saturating_sub(1) {
        if bytes[i] == b'\n' {
            if bytes[i + 1] == b'\n' {
                return Some(&body[i + 2..]);
            }
            if bytes.get(i + 1..i + 3) == Some(b"\r\n") {
                return Some(&body[i + 3..]);
            }
        }
    }
    None
}

fn split_armor_end<'a>(text: &'a str, label: &str) -> Option<(&'a str, &'a str)> {
    const PREFIX: &str = "-----END PGP ";
    const SUFFIX: &str = "-----";
    let mut offset = 0;
    while let Some(pos) = text[offset..].find(PREFIX) {
        let start = offset + pos;
        let after_prefix = &text[start + PREFIX.len()..];
        if let Some(after_label) = after_prefix.strip_prefix(label)
            && let Some(trailing) = after_label.strip_prefix(SUFFIX)
        {
            return Some((&text[..start], trailing));
        }
        offset = start + PREFIX.len();
    }
    None
}

pub(super) fn packet(input: &[u8]) -> Result<(u8, &[u8], &[u8]), String> {
    let first = *input.first().ok_or("Truncated PGP packet")?;
    if first & 128 == 0 {
        return Err("Invalid PGP packet".into());
    }
    let (tag, length, prefix) = if first & 64 != 0 {
        let b = *input.get(1).ok_or("Truncated PGP packet")?;
        let (length, prefix) = match b {
            0..=191 => (usize::from(b), 2),
            192..=223 => (
                ((usize::from(b) - 192) << 8)
                    + usize::from(*input.get(2).ok_or("Truncated PGP packet")?)
                    + 192,
                3,
            ),
            255 => (
                u32::from_be_bytes(
                    input
                        .get(2..6)
                        .ok_or("Truncated PGP packet")?
                        .try_into()
                        .map_err(|_| "Truncated PGP length")?,
                ) as usize,
                6,
            ),
            _ => return Err("Partial PGP packet is not accepted here".into()),
        };
        (first & 63, length, prefix)
    } else {
        let n = match first & 3 {
            0 => 1,
            1 => 2,
            2 => 4,
            _ => return Err("Indeterminate PGP packet".into()),
        };
        let mut length = 0usize;
        for b in input.get(1..1 + n).ok_or("Truncated PGP packet")? {
            length = (length << 8) | usize::from(*b);
        }
        ((first >> 2) & 15, length, n + 1)
    };
    let end = prefix
        .checked_add(length)
        .ok_or("PGP packet length overflow")?;
    Ok((
        tag,
        input.get(prefix..end).ok_or("Truncated PGP body")?,
        input.get(end..).ok_or("Truncated PGP packet")?,
    ))
}

pub(super) fn mpi(bytes: &[u8]) -> Result<(&[u8], &[u8]), String> {
    let size = bytes.get(..2).ok_or("Truncated PGP integer")?;
    let size = usize::from(u16::from_be_bytes([size[0], size[1]])).div_ceil(8);
    Ok((
        bytes.get(2..2 + size).ok_or("Truncated PGP integer")?,
        bytes.get(2 + size..).ok_or("Truncated PGP integer")?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pinned_proton_signature_and_changed_modulus() {
        let signed = include_str!("modulus-test.asc");
        assert_eq!(verify(signed).unwrap().len(), 256);
        assert!(verify(&signed.replacen("W2z5", "W3z5", 1)).is_err());
        assert!(verify(&format!("{signed}unexpected")).is_err());
    }
}
