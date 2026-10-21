// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `aterm_codec::base64`: padded standard alphabet on encode; canonical padding
//! and no trailing bits on strict decode. The strict decoder is what reads the
//! pinned Ed25519 anchors, the machine roster and the signed atpkg index cache,
//! so the property pinned here is exact: `decode_strict` accepts a string iff
//! it is the canonical encoding of what it decodes to.

use aterm_codec::base64::{decode, decode_strict, encode, encode_into, encoded_len};
use aterm_spec::rng::XorShift;

/// Strict accepts `text` exactly when the lenient decode succeeds AND
/// re-encoding reproduces `text`, and then yields the same bytes.
fn strict_is_canonical(text: &[u8]) {
    let strict = decode_strict(text).ok();
    let canonical = std::str::from_utf8(text)
        .ok()
        .and_then(|s| decode(s).ok())
        .filter(|bytes| encode(bytes).is_ok_and(|e| e.as_bytes() == text));
    assert_eq!(
        strict,
        canonical,
        "strict verdict for {:?}",
        String::from_utf8_lossy(text)
    );
}

fn round_trips(raw: &[u8]) {
    let encoded = encode(raw).expect("encode within MAX_INPUT_LEN");
    assert_eq!(encoded.len(), raw.len().div_ceil(3) * 4, "padded length");
    let back = decode_strict(encoded.as_bytes()).expect("our own output decodes strictly");
    assert_eq!(back, raw, "strict round-trip lost bytes");
}

#[test]
fn rfc4648_vectors() {
    for (raw, text) in [
        (&b""[..], ""),
        (b"f", "Zg=="),
        (b"fo", "Zm8="),
        (b"foo", "Zm9v"),
        (b"foob", "Zm9vYg=="),
        (b"fooba", "Zm9vYmE="),
        (b"foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(encode(raw).expect("encode"), text);
        assert_eq!(decode_strict(text.as_bytes()).expect("decode"), raw);
    }
}

/// Every byte length from 0 to 300 and every byte value, so no alphabet slot
/// or quad alignment goes unexercised.
#[test]
fn every_short_length_and_every_byte_value_round_trips() {
    for len in 0..=300usize {
        let raw: Vec<u8> = (0..len)
            .map(|i| (i.wrapping_mul(37) & 0xFF) as u8)
            .collect();
        round_trips(&raw);
    }
    for b in 0u16..=255 {
        for len in 1..=4usize {
            round_trips(&vec![b as u8; len]);
        }
    }
}

/// `encode_into` appends exactly what `encode` returns, onto an empty and onto
/// a non-empty buffer, and `encoded_len` is the length it appends: every input
/// length from 0 to 1023.
#[test]
fn encode_into_appends_exactly_encode() {
    let mut rng = XorShift::new(0x2545_F491_4F6C_DD1D);
    for len in 0..1024usize {
        let raw: Vec<u8> = (0..len).map(|_| rng.next_u64() as u8).collect();
        let expected = encode(&raw).expect("encode within MAX_INPUT_LEN");
        assert_eq!(encoded_len(len), Some(expected.len()), "length {len}");

        let mut fresh = String::new();
        encode_into(&mut fresh, &raw).expect("encode_into within MAX_INPUT_LEN");
        assert_eq!(fresh, expected, "length {len}");

        let mut appended = String::from("OK 1 ");
        encode_into(&mut appended, &raw).expect("encode_into within MAX_INPUT_LEN");
        assert_eq!(
            appended.strip_prefix("OK 1 "),
            Some(expected.as_str()),
            "length {len}"
        );
    }
}

/// A destination already sized by `encoded_len` takes the whole encoding in
/// place. Negative control: one byte short, the same append reallocates, so
/// an unchanged capacity is evidence of no second buffer.
#[test]
fn a_destination_sized_by_encoded_len_never_reallocates() {
    for len in 1..=300usize {
        let raw = vec![0xA5u8; len];
        let need = encoded_len(len).expect("short input");

        let mut sized = String::with_capacity(need);
        let reserved = sized.capacity();
        encode_into(&mut sized, &raw).expect("short input");
        assert_eq!(sized.capacity(), reserved, "length {len}");

        let mut short = String::with_capacity(need - 1);
        let short_reserved = short.capacity();
        encode_into(&mut short, &raw).expect("short input");
        assert_ne!(short.capacity(), short_reserved, "control at length {len}");
        assert_eq!(short, sized);
    }
}

/// Above `MAX_INPUT_LEN` both refuse, and `encode_into` leaves its destination
/// as it was. The oversized input is a zeroed allocation nothing reads.
#[test]
fn oversized_input_is_refused_and_appends_nothing() {
    let max = aterm_codec::MAX_INPUT_LEN;
    assert_eq!(encoded_len(max), Some(max.div_ceil(3) * 4));
    assert_eq!(encoded_len(max + 1), None);

    let oversized = vec![0u8; max + 1];
    let mut out = String::from("kept");
    assert_eq!(
        encode_into(&mut out, &oversized),
        Err(aterm_codec::base64::EncodeError::InputTooLarge(max + 1)),
    );
    assert_eq!(out, "kept");
    assert_eq!(out.capacity(), "kept".len());
}

/// EXHAUSTIVE over short strings from an alphabet that hits every sharp edge:
/// symbols with zero and non-zero low bits (the trailing-bit rejections), both
/// alphabet-specific symbols, the URL-safe symbols a standard decoder must
/// refuse, the pad byte, and a byte outside the alphabet.
#[test]
fn exhaustive_short_strings_are_accepted_only_when_canonical() {
    const ALPHA: &[u8] = b"AQh9+/-_=!";
    let mut buf = Vec::new();
    for len in 0..=4usize {
        for n in 0..ALPHA.len().pow(len as u32) {
            buf.clear();
            let mut n = n;
            for _ in 0..len {
                buf.push(ALPHA[n % ALPHA.len()]);
                n /= ALPHA.len();
            }
            strict_is_canonical(&buf);
        }
    }
    // Two full quads, so interior padding and a second-quad trailing-bit
    // violation are both reached.
    const NARROW: &[u8] = b"AQh=/";
    for n in 0..NARROW.len().pow(8) {
        buf.clear();
        let mut n = n;
        for _ in 0..8 {
            buf.push(NARROW[n % NARROW.len()]);
            n /= NARROW.len();
        }
        strict_is_canonical(&buf);
    }
}

/// The non-canonical spellings that motivated `decode_strict`: the lenient
/// `decode` accepts them, the strict one does not.
#[test]
fn non_canonical_spellings_are_refused() {
    for alias in ["Zh==", "Zi==", "Zj==", "Zm9=", "Zm+="] {
        assert!(
            decode(alias).is_ok(),
            "{alias}: the lenient decoder accepts"
        );
        assert!(decode_strict(alias.as_bytes()).is_err(), "{alias}");
    }
    for unpadded in ["Zg", "Zm8", "SGVsbG8sIHdvcmxkIQ"] {
        assert!(decode(unpadded).is_ok());
        assert!(decode_strict(unpadded.as_bytes()).is_err(), "{unpadded}");
    }
    for interior in ["Zg==Zg==", "Zm9v=Zm9v"] {
        assert!(decode_strict(interior.as_bytes()).is_err(), "{interior}");
    }
}

/// The shapes the trust call sites feed it: a 32-byte Ed25519 public key is 44
/// characters with one `=`, a 64-byte signature 88.
#[test]
fn ed25519_key_and_signature_shapes() {
    let mut rng = XorShift::new(0x9E37_79B9_7F4A_7C15);
    for _ in 0..2_000 {
        let key: Vec<u8> = (0..32).map(|_| rng.next_u64() as u8).collect();
        let sig: Vec<u8> = (0..64).map(|_| rng.next_u64() as u8).collect();
        let k = encode(&key).expect("encode");
        assert_eq!(k.len(), 44);
        assert!(k.ends_with('=') && !k.ends_with("=="));
        assert_eq!(encode(&sig).expect("encode").len(), 88);
        round_trips(&key);
        round_trips(&sig);
    }
}

/// Pseudo-random fuzz in both directions; decode-side inputs come from a
/// base64-ish alphabet so many are NEARLY valid.
#[test]
fn fuzz_both_directions() {
    let mut rng = XorShift::new(0xC2B2_AE3D_27D4_EB4F);
    const NEAR: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=====-_ \n";
    for _ in 0..200_000 {
        let len = rng.index(96);
        let raw: Vec<u8> = (0..len).map(|_| rng.next_u64() as u8).collect();
        round_trips(&raw);

        let slen = rng.index(24);
        let text: Vec<u8> = (0..slen).map(|_| *rng.pick(NEAR)).collect();
        strict_is_canonical(&text);
    }
}
