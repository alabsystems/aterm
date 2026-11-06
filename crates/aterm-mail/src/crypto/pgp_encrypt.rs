// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! OpenPGP ECDH session-key wrapping and integrity-protected message creation.
use super::{CURVE25519_OID, Key, aes, armor, hash, key_len, mpi, packet};
use zeroize::Zeroizing;

#[derive(Clone, Copy)]
pub struct Recipient {
    id: [u8; 8],
    fingerprint: [u8; 20],
    public: [u8; 32],
    hash: u8,
    cipher: u8,
}
impl Key {
    pub fn recipient(&self) -> Recipient {
        Recipient {
            id: self.id,
            fingerprint: self.fingerprint,
            public: self.public,
            hash: self.hash,
            cipher: self.cipher,
        }
    }
}

pub fn public_keys(armored: &str) -> Result<Vec<Recipient>, String> {
    let bytes = armor(armored, "PUBLIC KEY BLOCK")?;
    let mut rest = bytes.as_slice();
    let mut keys = Vec::new();
    while !rest.is_empty() {
        let (tag, body, tail) = packet(rest)?;
        rest = tail;
        if !matches!(tag, 6 | 14) || body.len() < 7 || body[0] != 4 || body[5] != 18 {
            continue;
        }
        let oid_len = usize::from(body[6]);
        let oid = body
            .get(7..7 + oid_len)
            .ok_or("Truncated public-key curve")?;
        if oid != CURVE25519_OID {
            continue;
        }
        let (point, kdf) = mpi(body.get(7 + oid_len..).ok_or("Truncated public key")?)?;
        if point.len() != 33 || point[0] != 64 || kdf.len() != 4 || kdf[0] != 3 || kdf[1] != 1 {
            return Err("Unsupported recipient public key".into());
        }
        let mut digest = ring::digest::Context::new(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY);
        digest.update(&[0x99]);
        digest.update(&(body.len() as u16).to_be_bytes());
        digest.update(body);
        let fingerprint: [u8; 20] = digest
            .finish()
            .as_ref()
            .try_into()
            .map_err(|_| "Invalid public-key fingerprint")?;
        keys.push(Recipient {
            id: fingerprint[12..].try_into().map_err(|_| "Invalid key ID")?,
            fingerprint,
            public: point[1..].try_into().map_err(|_| "Invalid public key")?,
            hash: kdf[2],
            cipher: kdf[3],
        });
    }
    if keys.is_empty() {
        return Err("Recipient has no supported encryption key".into());
    }
    Ok(keys)
}

pub struct EncryptedBody {
    session: Zeroizing<[u8; 32]>,
    pub data: Vec<u8>,
}
impl EncryptedBody {
    pub fn new(plaintext: &[u8]) -> Result<Self, String> {
        if plaintext.len() > 20 * 1024 * 1024 {
            return Err("Message exceeds limit".into());
        }
        let mut session = Zeroizing::new([0u8; 32]);
        aterm_uds::rand::fill(session.as_mut()).map_err(|_| "Could not generate message key")?;
        let mut body = Zeroizing::new(Vec::with_capacity(18 + 6 + 6 + plaintext.len() + 22));
        body.resize(16, 0);
        aterm_uds::rand::fill(&mut body).map_err(|_| "Could not generate message prefix")?;
        body.extend_from_within(14..16);
        let literal_len = (6 + plaintext.len()) as u32;
        body.extend_from_slice(&[0xc0 | 11, 255]);
        body.extend_from_slice(&literal_len.to_be_bytes());
        body.extend_from_slice(&[b'u', 0, 0, 0, 0, 0]);
        body.extend_from_slice(plaintext);
        body.extend_from_slice(&[0xd3, 0x14]);
        let digest = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, &body);
        body.extend_from_slice(digest.as_ref());
        let cfb = aes::encrypt_cfb(session.as_ref(), &[0u8; 16], &body)?;
        let enc_len = (1 + cfb.len()) as u32;
        let mut data = Vec::with_capacity(6 + 1 + cfb.len());
        data.extend_from_slice(&[0xc0 | 18, 255]);
        data.extend_from_slice(&enc_len.to_be_bytes());
        data.push(1);
        data.extend_from_slice(&cfb);
        Ok(Self { session, data })
    }
    pub fn key_packet(&self, recipient: &Recipient) -> Result<Vec<u8>, String> {
        let mut ephemeral = Zeroizing::new([0u8; 32]);
        aterm_uds::rand::fill(ephemeral.as_mut())
            .map_err(|_| "Could not generate ephemeral mail key")?;
        let public = aterm_secrets::x25519::x25519_base(&ephemeral);
        let shared = Zeroizing::new(aterm_secrets::x25519::x25519(&ephemeral, &recipient.public));
        if aterm_digest::ct_eq(shared.as_ref(), &[0u8; 32]) {
            return Err("Invalid recipient public key".into());
        }
        let mut digest = ring::digest::Context::new(hash(recipient.hash)?);
        digest.update(&[0, 0, 0, 1]);
        digest.update(shared.as_ref());
        digest.update(&[CURVE25519_OID.len() as u8]);
        digest.update(&CURVE25519_OID);
        digest.update(&[18, 3, 1, recipient.hash, recipient.cipher]);
        digest.update(b"Anonymous Sender    ");
        digest.update(&recipient.fingerprint);
        let klen = key_len(recipient.cipher)?;
        let finished = digest.finish();
        let mut derived = Zeroizing::new([0u8; 32]);
        derived[..klen].copy_from_slice(&finished.as_ref()[..klen]);
        let mut plain = Zeroizing::new([5u8; 40]);
        plain[0] = 9;
        plain[1..33].copy_from_slice(self.session.as_ref());
        let sum = self
            .session
            .iter()
            .fold(0u16, |a, b| a.wrapping_add(u16::from(*b)));
        plain[33..35].copy_from_slice(&sum.to_be_bytes());
        let wrapped = aes::wrap(&derived[..klen], plain.as_ref())?;
        let mut packet = Vec::with_capacity(14 + 32 + wrapped.len());
        packet.push(3);
        packet.extend_from_slice(&recipient.id);
        packet.extend_from_slice(&[18, 1, 7, 64]);
        packet.extend_from_slice(&public);
        packet.push(wrapped.len() as u8);
        packet.extend_from_slice(&wrapped);
        Ok(frame(1, &packet))
    }
    pub fn armored(&self, recipient: &Recipient) -> Result<String, String> {
        let key_packet = self.key_packet(recipient)?;
        let total_bytes = key_packet.len() + self.data.len();
        let b64_len = total_bytes.div_ceil(3) * 4;
        let mut out = String::with_capacity(b64_len + b64_len / 64 + 64);
        out.push_str("-----BEGIN PGP MESSAGE-----\n\n");
        let (head_chunks, head_rem) = key_packet.as_chunks::<48>();
        for chunk in head_chunks {
            aterm_codec::base64::encode_into(&mut out, chunk)
                .map_err(|_| "Could not encode encrypted mail")?;
            out.push('\n');
        }
        let mut tail = self.data.as_slice();
        if !head_rem.is_empty() {
            let take = (48 - head_rem.len()).min(tail.len());
            let mut stitch = [0u8; 48];
            stitch[..head_rem.len()].copy_from_slice(head_rem);
            stitch[head_rem.len()..head_rem.len() + take].copy_from_slice(&tail[..take]);
            aterm_codec::base64::encode_into(&mut out, &stitch[..head_rem.len() + take])
                .map_err(|_| "Could not encode encrypted mail")?;
            out.push('\n');
            tail = &tail[take..];
        }
        for chunk in tail.chunks(48) {
            aterm_codec::base64::encode_into(&mut out, chunk)
                .map_err(|_| "Could not encode encrypted mail")?;
            out.push('\n');
        }
        out.push_str("-----END PGP MESSAGE-----\n");
        Ok(out)
    }
    pub fn key_base64(&self) -> Result<String, String> {
        aterm_codec::base64::encode(self.session.as_ref())
            .map_err(|_| "Could not encode session key".into())
    }
}

fn frame(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(6 + body.len());
    bytes.extend_from_slice(&[0xc0 | tag, 255]);
    bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
    bytes.extend_from_slice(body);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_message_roundtrips_through_authenticated_decryptor() {
        let keys =
            super::super::unlock(include_str!("../../tests/fixtures/proton-key-v4.asc"), b"")
                .unwrap();
        let content = EncryptedBody::new(b"Hello from aterm").unwrap();
        let armored = content.armored(&keys[0].recipient()).unwrap();
        assert_eq!(
            &*super::super::decrypt(&armored, &keys).unwrap(),
            b"Hello from aterm"
        );
    }
}
