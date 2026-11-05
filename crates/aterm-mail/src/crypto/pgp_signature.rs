// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Detached Ed25519 signatures protecting Proton address-key tokens.
use super::modulus::{armor, mpi, packet};

pub fn signing_keys(private: &str, password: &[u8]) -> Result<Vec<[u8; 32]>, String> {
    if private.len() > 1024 * 1024 {
        return Err("Private key exceeds limit".into());
    }
    let bytes = zeroize::Zeroizing::new(armor(private, "PRIVATE KEY BLOCK")?);
    let mut rest = bytes.as_slice();
    let mut keys = Vec::new();
    while !rest.is_empty() {
        let (tag, body, tail) = packet(rest)?;
        rest = tail;
        if !matches!(tag, 5 | 7) || body.len() < 19 || body[0] != 4 || body[5] != 22 {
            continue;
        }
        if body.get(6..16) != Some(&[9, 43, 6, 1, 4, 1, 218, 71, 15, 1]) {
            continue;
        }
        let (point, protected) = mpi(&body[16..])?;
        if point.len() == 33 && point[0] == 64 {
            let secret = super::pgp::unlock_packet(protected, password)?;
            let (seed, _) = mpi(&secret)?;
            if seed.len() > 32 {
                return Err("Invalid signing seed".into());
            }
            let mut padded = zeroize::Zeroizing::new([0u8; 32]);
            padded[32 - seed.len()..].copy_from_slice(seed);
            let signer = ring::signature::Ed25519KeyPair::from_seed_unchecked(padded.as_ref())
                .map_err(|_| "Invalid signing seed")?;
            use ring::signature::KeyPair;
            if !aterm_digest::ct_eq(signer.public_key().as_ref(), &point[1..]) {
                return Err("Private and public signing key do not match".into());
            }
            keys.push(point[1..].try_into().map_err(|_| "Invalid signing key")?);
        }
    }
    Ok(keys)
}

pub fn verify(data: &[u8], signature: &str, keys: &[[u8; 32]]) -> Result<(), String> {
    if signature.len() > 64 * 1024 {
        return Err("Signature exceeds limit".into());
    }
    let bytes = armor(signature, "SIGNATURE")?;
    let (tag, sig, rest) = packet(&bytes)?;
    if tag != 2 || !rest.is_empty() || sig.len() < 10 {
        return Err("Unsupported address-token signature".into());
    }
    if sig[0] != 4 || !matches!(sig[1], 0 | 1) || sig[2] != 22 {
        return Err(format!(
            "Unsupported address-token signature (version {}, type {}, algorithm {})",
            sig[0], sig[1], sig[2]
        ));
    }
    let algorithm = match sig[3] {
        8 => &ring::digest::SHA256,
        10 => &ring::digest::SHA512,
        _ => return Err("Unsupported signature hash".into()),
    };
    let end = 6 + usize::from(u16::from_be_bytes([sig[4], sig[5]]));
    let hashed = sig.get(..end).ok_or("Truncated signature")?;
    let cursor = sig
        .get(end..)
        .filter(|s| s.len() >= 2)
        .ok_or("Truncated signature")?;
    let unhashed = usize::from(u16::from_be_bytes([cursor[0], cursor[1]]));
    let cursor = cursor
        .get(2 + unhashed..)
        .filter(|s| s.len() >= 2)
        .ok_or("Truncated signature")?;
    let prefix = &cursor[..2];
    let (r, cursor) = mpi(&cursor[2..])?;
    let (s, tail) = mpi(cursor)?;
    if !tail.is_empty() || r.len() > 32 || s.len() > 32 {
        return Err("Invalid signature size".into());
    }
    let mut raw = [0u8; 64];
    raw[32 - r.len()..32].copy_from_slice(r);
    raw[64 - s.len()..].copy_from_slice(s);
    let mut hash = ring::digest::Context::new(algorithm);
    let canonical;
    let data = if sig[1] == 1 {
        canonical = canonical_text(data);
        canonical.as_slice()
    } else {
        data
    };
    hash.update(data);
    hash.update(hashed);
    hash.update(&[4, 255]);
    hash.update(&(end as u32).to_be_bytes());
    let digest = hash.finish();
    if !aterm_digest::ct_eq(prefix, &digest.as_ref()[..2]) {
        return Err("Address-token signature mismatch".into());
    }
    if keys
        .iter()
        .any(|key| astream_aead::signature::verify(key, digest.as_ref(), &raw))
    {
        Ok(())
    } else {
        Err("Address-token signature did not verify".into())
    }
}

fn canonical_text(data: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(data.len());
    let mut bytes = data.iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        match byte {
            b'\r' => {
                if bytes.peek() == Some(&b'\n') {
                    bytes.next();
                }
                result.extend_from_slice(b"\r\n");
            }
            b'\n' => result.extend_from_slice(b"\r\n"),
            byte => result.push(byte),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    #[test]
    fn canonical_text_signature_accepts_line_endings_but_not_changed_content() {
        use ring::signature::KeyPair;
        let signer = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
        let header = [4, 1, 22, 8, 0, 0];
        let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
        hash.update(b"first\r\nsecond\r\n");
        hash.update(&header);
        hash.update(&[4, 255, 0, 0, 0, 6]);
        let hash = hash.finish();
        let signature = signer.sign(hash.as_ref());
        let mut body = header.to_vec();
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(&hash.as_ref()[..2]);
        for part in signature.as_ref().as_chunks::<32>().0 {
            body.extend_from_slice(&256u16.to_be_bytes());
            body.extend_from_slice(part);
        }
        let mut packet = vec![0xc2, body.len() as u8];
        packet.extend_from_slice(&body);
        let armor = format!(
            "-----BEGIN PGP SIGNATURE-----\n\n{}\n-----END PGP SIGNATURE-----\n",
            aterm_codec::base64::encode(&packet).unwrap()
        );
        let keys = [signer.public_key().as_ref().try_into().unwrap()];
        for text in [
            b"first\nsecond\n".as_slice(),
            b"first\r\nsecond\r\n",
            b"first\rsecond\r",
        ] {
            super::verify(text, &armor, &keys).unwrap();
        }
        assert!(super::verify(b"first\nchanged\n", &armor, &keys).is_err());
    }

    #[test]
    fn published_signature_and_altered_token() {
        let keys = super::signing_keys(include_str!("../../tests/fixtures/proton-key-v4.asc"), b"")
            .unwrap();
        let sig = include_str!("../../tests/fixtures/proton-signature-v4.asc");
        super::verify(b"hello world", sig, &keys).unwrap();
        assert!(super::verify(b"hello earth", sig, &keys).is_err());
    }
}
