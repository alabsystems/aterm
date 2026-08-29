// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Differential oracle: `aterm_codec::base64` against the retired `base64`
//! package's `general_purpose::STANDARD` engine.
//!
//! Every base64 call site aterm ever had was written against that one engine —
//! padded standard alphabet on encode, canonical padding and no trailing bits on
//! decode — and the ones that matter decode PINNED ED25519 ANCHORS, the machine
//! roster, and the signed atpkg index cache. So "close enough" is not the
//! property; the property is that the two agree on
//!
//!   * the exact encoded string for every input, and
//!   * the exact accept/reject verdict AND decoded bytes for every input string.
//!
//! The URL-safe no-pad engine gets the SAME treatment against
//! `general_purpose::URL_SAFE_NO_PAD`. It had none at all until this file grew
//! one, and the decoder behind that name turned out to be the lenient body
//! wearing a strict name — 12,538 disagreements in 200,000 random candidates,
//! every one of them ours-accepts / oracle-refuses. An entry point nothing
//! calls yet is exactly where that goes unnoticed.
//!
//! `base64` is a `[dev-dependencies]` entry only: it contributes nothing to any
//! shipped binary, it exists here to be disagreed with.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as ORACLE;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as URL_ORACLE;

/// Assert both directions agree for one byte string.
fn agree_encode(raw: &[u8]) {
    let ours = aterm_codec::base64::encode(raw).expect("encode within MAX_INPUT_LEN");
    let theirs = ORACLE.encode(raw);
    assert_eq!(ours, theirs, "encode disagreed for {} bytes", raw.len());
    // And the strict decoder must invert the oracle's own output.
    let back = aterm_codec::base64::decode_strict(theirs.as_bytes())
        .expect("oracle output must decode strictly");
    assert_eq!(back, raw, "strict round-trip lost bytes");
}

/// Assert the decoders agree on one candidate string — verdict first, then bytes.
fn agree_decode(text: &[u8]) {
    let ours = aterm_codec::base64::decode_strict(text);
    let theirs = ORACLE.decode(text);
    match (&ours, &theirs) {
        (Ok(a), Ok(b)) => assert_eq!(
            a,
            b,
            "decoded bytes differ for {:?}",
            String::from_utf8_lossy(text)
        ),
        (Err(_), Err(_)) => {}
        _ => panic!(
            "verdict differs for {:?}: ours={:?} oracle={:?}",
            String::from_utf8_lossy(text),
            ours.as_ref().map(Vec::len),
            theirs.as_ref().map(Vec::len),
        ),
    }
}

/// Assert both URL-safe directions agree for one byte string.
fn agree_encode_url(raw: &[u8]) {
    let ours = aterm_codec::base64::encode_url_safe_no_pad(raw).expect("encode within limit");
    let theirs = URL_ORACLE.encode(raw);
    assert_eq!(
        ours,
        theirs,
        "url-safe encode disagreed for {} bytes",
        raw.len()
    );
    let back = aterm_codec::base64::decode_url_safe_no_pad(&theirs)
        .expect("oracle output must decode strictly");
    assert_eq!(back, raw, "url-safe round-trip lost bytes");
}

/// Assert the URL-safe decoders agree on one candidate string.
fn agree_decode_url(text: &str) {
    let ours = aterm_codec::base64::decode_url_safe_no_pad(text);
    let theirs = URL_ORACLE.decode(text);
    match (&ours, &theirs) {
        (Ok(a), Ok(b)) => assert_eq!(a, b, "url-safe decoded bytes differ for {text:?}"),
        (Err(_), Err(_)) => {}
        _ => panic!(
            "url-safe verdict differs for {text:?}: ours={:?} oracle={:?}",
            ours.as_ref().map(Vec::len),
            theirs.as_ref().map(Vec::len),
        ),
    }
}

#[test]
fn rfc4648_vectors_match_the_oracle() {
    for v in [
        b"".as_slice(),
        b"f",
        b"fo",
        b"foo",
        b"foob",
        b"fooba",
        b"foobar",
    ] {
        agree_encode(v);
    }
}

/// Every byte length from 0 to 300 (covering every quad alignment many times
/// over) and every byte value, so no alphabet slot goes unexercised.
#[test]
fn every_short_length_and_every_byte_value_matches() {
    for len in 0..=300usize {
        let raw: Vec<u8> = (0..len)
            .map(|i| (i.wrapping_mul(37) & 0xFF) as u8)
            .collect();
        agree_encode(&raw);
    }
    for b in 0u16..=255 {
        for len in 1..=4usize {
            agree_encode(&vec![b as u8; len]);
        }
    }
}

/// EXHAUSTIVE over short strings drawn from an alphabet chosen to hit every
/// sharp edge at once: two symbols whose low bits are zero (`A`, `Q`), two
/// whose low bits are NOT (`h`, `9` — the trailing-bit rejections), both
/// alphabet-specific symbols (`+`, `/`), the URL-safe symbols that must be
/// REJECTED by a standard decoder (`-`, `_`), the pad byte, and a byte outside
/// the alphabet entirely.
#[test]
fn exhaustive_short_strings_match_the_oracle() {
    const ALPHA: &[u8] = b"AQh9+/-_=!";
    let mut buf = Vec::new();
    // Lengths 0..=4 exhaustively: 1 + 10 + 100 + 1_000 + 10_000 candidates.
    for len in 0..=4usize {
        let total = ALPHA.len().pow(len as u32);
        for n in 0..total {
            buf.clear();
            let mut n = n;
            for _ in 0..len {
                buf.push(ALPHA[n % ALPHA.len()]);
                n /= ALPHA.len();
            }
            agree_decode(&buf);
        }
    }
    // Length 8 (two full quads) over a narrower set, so interior padding and a
    // second-quad trailing-bit violation are both reached.
    const NARROW: &[u8] = b"AQh=/";
    for n in 0..NARROW.len().pow(8) {
        buf.clear();
        let mut n = n;
        for _ in 0..8 {
            buf.push(NARROW[n % NARROW.len()]);
            n /= NARROW.len();
        }
        agree_decode(&buf);
    }
}

/// The non-canonical spellings that motivated `decode_strict`: the lenient
/// `decode` accepts them, the oracle does not, and `decode_strict` must side
/// with the oracle. Asserted as a NAMED property, not just swept up by the
/// enumeration, because it is the reason the trust call sites moved.
#[test]
fn non_canonical_spellings_are_refused_exactly_as_the_oracle_refuses_them() {
    // Trailing bits set: same decoded byte as the canonical spelling.
    for alias in ["Zh==", "Zi==", "Zj==", "Zm8=", "Zm9=", "Zm+="] {
        let lenient = aterm_codec::base64::decode(alias);
        let strict = aterm_codec::base64::decode_strict(alias.as_bytes());
        let oracle = ORACLE.decode(alias);
        assert_eq!(
            strict.is_ok(),
            oracle.is_ok(),
            "{alias}: strict verdict must match the oracle"
        );
        if oracle.is_err() {
            assert!(
                lenient.is_ok(),
                "{alias} was picked because the LENIENT decoder accepts it"
            );
        }
    }
    // Missing padding: accepted by `decode`, refused by the oracle and by strict.
    for unpadded in ["Zg", "Zm8", "SGVsbG8sIHdvcmxkIQ"] {
        assert!(aterm_codec::base64::decode(unpadded).is_ok());
        assert!(ORACLE.decode(unpadded).is_err());
        assert!(aterm_codec::base64::decode_strict(unpadded.as_bytes()).is_err());
    }
    // Interior padding is never a concatenation of two decodes.
    for interior in ["Zg==Zg==", "Zm9v=Zm9v"] {
        assert!(ORACLE.decode(interior).is_err());
        assert!(aterm_codec::base64::decode_strict(interior.as_bytes()).is_err());
    }
}

/// The URL-safe no-pad engine, held to its own oracle: every short length, the
/// two alphabet-specific symbols, the pad byte that must NOT be accepted, and
/// the trailing-bit rejections.
///
/// The alphabet here is chosen the same way the standard sweep's is: symbols
/// whose low bits are zero (`A`, `Q`), symbols whose low bits are not (`h`,
/// `9`), both URL-safe specials (`-`, `_`), the standard specials that must be
/// REFUSED here (`+`, `/`), the pad byte, and a byte outside every alphabet.
#[test]
fn url_safe_no_pad_matches_its_own_oracle() {
    for len in 0..=200usize {
        let raw: Vec<u8> = (0..len)
            .map(|i| (i.wrapping_mul(53) & 0xFF) as u8)
            .collect();
        agree_encode_url(&raw);
    }
    const ALPHA: &[u8] = b"AQh9-_+/=!";
    let mut buf = String::new();
    for len in 0..=4usize {
        let total = ALPHA.len().pow(len as u32);
        for n in 0..total {
            buf.clear();
            let mut n = n;
            for _ in 0..len {
                buf.push(char::from(ALPHA[n % ALPHA.len()]));
                n /= ALPHA.len();
            }
            agree_decode_url(&buf);
        }
    }
    // The named cases that used to split the two decoders: a trailing-bit
    // violation, any `=` at all, and a bare pad byte.
    for text in [
        "B1", "1b=", "=", "==", "A", "AA", "AB", "AAA", "AAB", "Zg==", "Zg",
    ] {
        agree_decode_url(text);
    }
    // Randomized, weighted toward nearly-valid input.
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 32) as u32
    };
    const NEAR: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_=+/ ";
    for _ in 0..200_000 {
        let len = (next() % 24) as usize;
        buf.clear();
        for _ in 0..len {
            buf.push(char::from(NEAR[(next() as usize) % NEAR.len()]));
        }
        agree_decode_url(&buf);
        let raw_len = (next() % 64) as usize;
        let raw: Vec<u8> = (0..raw_len).map(|_| (next() & 0xFF) as u8).collect();
        agree_encode_url(&raw);
    }
}

/// The shape the trust call sites actually feed it: a 32-byte Ed25519 public
/// key is 44 characters with one `=`, and a 64-byte signature is 88 with one.
#[test]
fn ed25519_key_and_signature_shapes_match() {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..2_000 {
        let key: Vec<u8> = (0..32).map(|_| (next() & 0xFF) as u8).collect();
        let sig: Vec<u8> = (0..64).map(|_| (next() & 0xFF) as u8).collect();
        agree_encode(&key);
        agree_encode(&sig);
        let encoded = ORACLE.encode(&key);
        assert_eq!(encoded.len(), 44);
        assert!(encoded.ends_with('='));
        assert_eq!(
            aterm_codec::base64::decode_strict(encoded.as_bytes()).unwrap(),
            key
        );
    }
}

/// Pseudo-random fuzz in both directions. Encode-side inputs are arbitrary byte
/// strings; decode-side inputs are drawn from a base64-ish alphabet so a useful
/// fraction of them are *nearly* valid rather than rejected at the first byte.
#[test]
fn fuzz_both_directions_against_the_oracle() {
    let mut state: u64 = 0xC2B2_AE3D_27D4_EB4F;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as u32
    };
    const NEAR: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=====-_ \n";
    for _ in 0..200_000 {
        let len = (next() % 96) as usize;
        let raw: Vec<u8> = (0..len).map(|_| (next() & 0xFF) as u8).collect();
        agree_encode(&raw);

        let slen = (next() % 24) as usize;
        let text: Vec<u8> = (0..slen)
            .map(|_| NEAR[(next() as usize) % NEAR.len()])
            .collect();
        agree_decode(&text);
    }
}

/// A real corpus: the repository's own bytes. Every file under `crates/` up to a
/// size cap, encoded by both and decoded back — the byte-identical round-trip
/// the `aterm-toml` and `aterm-png` retirements were held to.
#[test]
fn repository_corpus_round_trips_byte_identically() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .to_path_buf();
    let mut stack = vec![root];
    let mut files = 0usize;
    let mut bytes = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                // Skip build output; it is not source and it is enormous.
                if path.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                stack.push(path);
            } else if meta.is_file() && meta.len() <= 512 * 1024 {
                let Ok(raw) = std::fs::read(&path) else {
                    continue;
                };
                agree_encode(&raw);
                files += 1;
                bytes += raw.len();
            }
        }
    }
    assert!(
        files > 500,
        "corpus too small to be meaningful: {files} files"
    );
    eprintln!("base64 oracle corpus: {files} files, {bytes} bytes");
}
