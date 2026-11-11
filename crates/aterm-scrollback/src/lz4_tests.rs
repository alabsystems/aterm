// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn small_claims_decode_to_their_claim_or_fail() {
    let mut bodies: Vec<Vec<u8>> = vec![Vec::new()];
    bodies.extend((0..=u8::MAX).map(|a| vec![a]));
    for a in 0..=u8::MAX {
        bodies.extend((0..=u8::MAX).map(|b| vec![a, b]));
    }
    let mut decoded = 0usize;
    for claim in 0..=16u32 {
        for body in &bodies {
            let input = [&claim.to_le_bytes()[..], body].concat();
            if let Ok(out) = decompress_size_prepended(&input) {
                assert_eq!(out.len(), claim as usize, "{input:02x?}");
                decoded += 1;
            }
        }
    }
    assert!(decoded > 0, "no input decoded: the law held vacuously");
    for short in [&b""[..], b"\x00", b"\x00\x00", b"\xff\xff\xff"] {
        assert_eq!(
            decompress_size_prepended(short),
            Err(Lz4Error::InputTooShort),
            "{short:02x?}"
        );
    }
}

#[test]
fn test_max_decompressed_size_rejection() {
    let size = u32::try_from(MAX_DECOMPRESSED_SIZE + 1).expect("cap fits the prefix");
    let mut data = size.to_le_bytes().to_vec();
    data.push(0x00);
    assert!(matches!(
        decompress_size_prepended(&data),
        Err(Lz4Error::OutputTooLarge(_))
    ));
}

#[test]
fn test_max_decompressed_size_at_limit() {
    let input = vec![0u8; MAX_DECOMPRESSED_SIZE];
    let compressed = compress_prepend_size(&input).unwrap();
    let decompressed = decompress_size_prepended(&compressed).expect("at limit");
    assert_eq!(decompressed.len(), MAX_DECOMPRESSED_SIZE);
}

#[test]
fn test_compression_bomb_rejected_under_limit() {
    let claimed: u32 = 1024 * 1024;
    let mut data = claimed.to_le_bytes().to_vec();
    data.extend(std::iter::repeat_n(0u8, 100));
    let err = decompress_size_prepended(&data)
        .expect_err("impossible ratio must be rejected as a compression bomb");
    match err {
        Lz4Error::CompressionBombSuspected {
            claimed_size,
            compressed_body_len,
        } => {
            assert_eq!(claimed_size, claimed as usize);
            assert_eq!(compressed_body_len, 100);
        }
        other => panic!("expected CompressionBombSuspected, got {other:?}"),
    }
}

#[test]
fn test_legitimate_high_ratio_still_accepted() {
    let input = vec![0u8; 8 * 1024]; // 8 KiB zeros
    let compressed = compress_prepend_size(&input).unwrap();
    let ratio = input.len() / (compressed.len() - 4);
    assert!(
        ratio < MAX_COMPRESSION_RATIO,
        "test assumes 8 KiB zeros compresses with ratio < {MAX_COMPRESSION_RATIO}; got {ratio}"
    );
    let decompressed = decompress_size_prepended(&compressed)
        .expect("legitimate high-ratio input must still round-trip");
    assert_eq!(decompressed, input);
}

#[test]
fn test_ratio_boundary_exactly_at_limit_accepted() {
    let body_len = 64usize;
    let claimed = MAX_COMPRESSION_RATIO * body_len; // exactly at limit
    let claimed_u32 = u32::try_from(claimed).expect("test claim fits in u32");
    let mut data = claimed_u32.to_le_bytes().to_vec();
    data.extend(std::iter::repeat_n(0u8, body_len));
    let err = decompress_size_prepended(&data).expect_err("garbage payload fails decode");
    assert!(
        !matches!(err, Lz4Error::CompressionBombSuspected { .. }),
        "ratio exactly at MAX_COMPRESSION_RATIO must be accepted by the bomb gate; got {err:?}"
    );
}

#[test]
fn test_cross_impl_compatibility_with_aterm_lz4() {
    let input: Vec<u8> = b"scrollback-page-XYZ"
        .iter()
        .copied()
        .cycle()
        .take(4096)
        .collect();
    let compressed = aterm_lz4::compress_prepend_size(&input);
    let decompressed = decompress_size_prepended(&compressed)
        .expect("aterm_lz4 output must decompress via scrollback wrapper");
    assert_eq!(decompressed, input);
}
