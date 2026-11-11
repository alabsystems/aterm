// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// FNV's published vectors; a wrong prime can agree in the low 40 bits only.
#[test]
fn matches_the_published_vectors() {
    let word_fold = |bytes: &[u8]| {
        bytes
            .iter()
            .fold(FNV64_OFFSET, |h, &b| fnv_mix_u64(h, u64::from(b)))
    };
    assert_eq!(FNV64_OFFSET, 0xcbf2_9ce4_8422_2325);
    assert_eq!(FNV64_PRIME, 0x0000_0100_0000_01b3);
    for (bytes, hash) in [
        (&b""[..], FNV64_OFFSET),
        (b"a", 0xaf63_dc4c_8601_ec8c),
        (b"foobar", 0x8594_4171_f739_67e8),
    ] {
        assert_eq!(fnv1a_64(bytes), hash, "{bytes:02x?}");
        assert_eq!(word_fold(bytes), hash, "the word fold, {bytes:02x?}");
    }
    // The platform UUID behind aterm-verify's perf-boxes.tsv `m21  fp:4d119fc8de0a3b9e` row.
    assert_eq!(
        fnv1a_64(b"4F2C6182-55E3-51B1-B505-42FD89F13166"),
        0x4d11_9fc8_de0a_3b9e
    );
    assert_eq!(fnv1a_128(b""), FNV128_OFFSET);
    assert_eq!(fnv1a_128(b"a"), 0xd228_cb69_6f1a_8caf_7891_2b70_4e4a_8964);
    assert_eq!(
        fnv1a_128_extend(fnv1a_128(b"foo"), b"bar"),
        fnv1a_128(b"foobar")
    );
}

/// The byte loop and the word step are astream-wire's, so the step is held here to the
/// formula written out, and the loop to a fold of the step.
#[test]
fn the_word_fold_agrees_with_astreams_byte_loop() {
    let fold = |prime: u64, bytes: &[u8]| {
        bytes
            .iter()
            .fold(FNV64_OFFSET, |h, &b| (h ^ u64::from(b)).wrapping_mul(prime))
    };
    let inputs: [&[u8]; 5] = [
        b"",
        b"a",
        b"foobar",
        &[0x00, 0x80, 0xff, 0x7f, 0x01],
        &[0xa5; 64],
    ];
    for bytes in inputs {
        assert_eq!(fnv1a_64(bytes), astream_wire::fnv1a_64(bytes));
        assert_eq!(
            fnv1a_64(bytes),
            bytes
                .iter()
                .fold(FNV64_OFFSET, |h, &b| fnv_mix_u64(h, u64::from(b))),
            "{bytes:02x?}"
        );
        assert_eq!(fnv1a_64(bytes), fold(FNV64_PRIME, bytes));
    }
    let words = [
        0,
        1,
        0xff,
        0x0102_0304_0506_0708,
        0x8000_0000_0000_0000,
        u64::MAX,
    ];
    for h in [0, FNV64_OFFSET, u64::MAX] {
        for word in words {
            assert_eq!(
                fnv_mix_u64(h, word),
                (h ^ word).wrapping_mul(FNV64_PRIME),
                "{h:#x} {word:#x}"
            );
        }
    }
    let off_by_one_bit = FNV64_PRIME ^ (1 << 41);
    assert_ne!(fold(off_by_one_bit, b"foobar"), fnv1a_64(b"foobar"));
    assert_ne!(fold(off_by_one_bit, b"a"), fnv1a_64(b"a"));
    let word = 0x0102_0304_0506_0708;
    assert_ne!(
        (FNV64_OFFSET ^ word).wrapping_mul(off_by_one_bit),
        fnv_mix_u64(FNV64_OFFSET, word)
    );
}

#[test]
fn a_stream_of_writes_is_the_hash_of_their_concatenation() {
    let mut h = Fnv1a64::new();
    assert!(h.is_empty_basis());
    h.write(b"foo");
    h.write_u8(b'b');
    h.write(b"ar");
    assert!(!h.is_empty_basis());
    assert_eq!(h.finish(), fnv1a_64(b"foobar"));
    assert_eq!(
        fnv1a_64_extend(fnv1a_64(b"foo"), b"bar"),
        fnv1a_64(b"foobar")
    );
    assert_eq!(fnv_mix_u64(FNV64_OFFSET, u64::from(b'a')), fnv1a_64(b"a"));

    let word = 0x0102_0304_0506_0708u64;
    let le_bytes = word.to_le_bytes();
    assert_eq!(fnv1a_64_u64_le(FNV64_OFFSET, word), fnv1a_64(&le_bytes));
    assert_eq!(fnv1a_64_array8(FNV64_OFFSET, le_bytes), fnv1a_64(&le_bytes));
    assert_eq!(
        &fnv1a_64_to_hex16(0x8594_4171_f739_67e8),
        b"85944171f73967e8"
    );
    assert_eq!(bucket_index(123, 0), 0);
    assert_eq!(bucket_index(123, 10), 3);
}
