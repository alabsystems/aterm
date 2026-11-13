// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::sync::LazyLock;

use ring::rand::SystemRandom;
use ring::signature::RsaKeyPair;

use super::*;

/// A throwaway RSA-4096 key (`openssl genpkey`, PKCS#8 DER) that signs the generated vectors.
pub(crate) const TEST_KEY_PKCS8: &[u8] =
    include_bytes!("vendor_direct/fixtures/openpgp-test-rsa4096.pk8.der");

pub(crate) const TEST_KEY_CREATED: u32 = 1_700_000_000;

pub(crate) struct TestKey {
    pub(crate) pair: RsaKeyPair,
    pub(crate) pinned: PinnedRsaKey,
}

pub(crate) static TEST_KEY: LazyLock<TestKey> = LazyLock::new(|| {
    let pair = RsaKeyPair::from_pkcs8(TEST_KEY_PKCS8).expect("the test key parses");
    let public = RsaPublicKeyComponents::<Vec<u8>>::from(pair.public());
    let n: &'static [u8] = public.n.leak();
    let e: &'static [u8] = public.e.leak();
    let fingerprint = v4_fingerprint(&key_packet_body(TEST_KEY_CREATED, n, e));
    TestKey {
        pair,
        pinned: PinnedRsaKey {
            n,
            e,
            created: TEST_KEY_CREATED,
            fingerprint,
        },
    }
});

/// An OpenPGP MPI: a bit count, then the value's big-endian bytes without leading zeros.
pub(crate) fn mpi(value: &[u8]) -> Vec<u8> {
    let start = value.iter().position(|&b| b != 0).unwrap_or(value.len());
    let digits = &value[start..];
    let bits = digits
        .first()
        .map_or(0, |top| 8 * digits.len() - top.leading_zeros() as usize);
    let mut out = u16::try_from(bits).unwrap().to_be_bytes().to_vec();
    out.extend_from_slice(digits);
    out
}

pub(crate) fn key_packet_body(created: u32, n: &[u8], e: &[u8]) -> Vec<u8> {
    let mut body = vec![4];
    body.extend_from_slice(&created.to_be_bytes());
    body.push(PKALG_RSA);
    body.extend(mpi(n));
    body.extend(mpi(e));
    body
}

pub(crate) fn v4_fingerprint(key_body: &[u8]) -> [u8; 20] {
    let mut ctx = digest::Context::new(&digest::SHA1_FOR_LEGACY_USE_ONLY);
    ctx.update(&[0x99]);
    ctx.update(&u16::try_from(key_body.len()).unwrap().to_be_bytes());
    ctx.update(key_body);
    ctx.finish().as_ref().try_into().unwrap()
}

/// A subpacket with a one-octet length.
pub(crate) fn subpacket(kind: u8, data: &[u8]) -> Vec<u8> {
    let len = u8::try_from(data.len() + 1).unwrap();
    assert!(len < 192);
    [&[len, kind][..], data].concat()
}

pub(crate) fn issuer(fingerprint: &[u8; 20]) -> Vec<u8> {
    subpacket(
        SUBPACKET_ISSUER_FINGERPRINT,
        &[&[4][..], fingerprint].concat(),
    )
}

pub(crate) fn created_at(t: u32) -> Vec<u8> {
    subpacket(SUBPACKET_CREATION_TIME, &t.to_be_bytes())
}

/// The test key as a pinned key.
#[must_use]
pub(crate) fn test_key() -> PinnedRsaKey {
    TEST_KEY.pinned
}

/// A binary v4 detached signature over `doc` by the test key.
#[must_use]
pub(crate) fn sign_detached(doc: &[u8], created: u32) -> Vec<u8> {
    let hashed = [issuer(&TEST_KEY.pinned.fingerprint), created_at(created)].concat();
    let mut body = vec![SIG_VERSION, SIG_TYPE_BINARY, PKALG_RSA, HASH_SHA512];
    body.extend_from_slice(&u16::try_from(hashed.len()).unwrap().to_be_bytes());
    body.extend_from_slice(&hashed);
    let count = u32::try_from(body.len()).unwrap();
    let message = [doc, &body, &[SIG_VERSION, 0xFF], &count.to_be_bytes()].concat();
    let mut raw = vec![0; TEST_KEY.pair.public().modulus_len()];
    TEST_KEY
        .pair
        .sign(
            &ring::signature::RSA_PKCS1_SHA512,
            &SystemRandom::new(),
            &message,
            &mut raw,
        )
        .unwrap();
    body.extend_from_slice(&[0, 0]);
    body.extend_from_slice(&digest::digest(&digest::SHA512, &message).as_ref()[..2]);
    body.extend(mpi(&raw));
    let len = u32::try_from(body.len()).unwrap();
    let mut packet = vec![0xC0 | TAG_SIGNATURE, 0xFF];
    packet.extend_from_slice(&len.to_be_bytes());
    packet.extend(body);
    packet
}
