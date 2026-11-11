// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: BSD-2-Clause
//
// The format's static data, transcribed from the text of RFC 7932 (§§4, 5, 7.1,
// 8, Appendices A and B), whose Code Components the IETF Trust licenses under the
// Simplified BSD License: see LICENSE-BSD-2. Each table the RFC prints a CRC-32 for
// is held to it by this file's tests.

use super::*;

/// Appendix C's CRC-32.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// THE RFC'S OWN CHECK VALUES: each transcribed table is the length and CRC-32 the RFC
/// prints for it, so no byte of the format's data differs from the RFC's.
#[test]
fn every_table_is_the_length_and_crc_the_rfc_prints() {
    assert_eq!(
        (DICTIONARY.len(), crc32(DICTIONARY)),
        (122_784, 0x5136_cb04)
    );
    let mut sha = aterm_digest::Sha256::new();
    sha.update(DICTIONARY);
    let hex: String = sha.finalize().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        hex,
        "20e42eb1b511c21806d4d227d07e5dd06877d8ce7b3a817f378f313653f35c70"
    );
    for (lut, crc) in [
        (&LUT0, 0x8e91_efb7_u32),
        (&LUT1, 0xd01a_32f4),
        (&LUT2, 0x0dd7_a0d6),
    ] {
        assert_eq!(crc32(lut), crc);
    }
    let mut serialized = Vec::new();
    for t in &TRANSFORMS {
        serialized.extend_from_slice(t.prefix);
        serialized.push(0);
        serialized.push(t.kind.id());
        serialized.extend_from_slice(t.suffix);
        serialized.push(0);
    }
    assert_eq!((serialized.len(), crc32(&serialized)), (648, 0x3d96_5f81));
}

/// The word counts tile the dictionary exactly (§8), and each length code table runs
/// on from the one before: every code's base is the previous base past its extra bits'
/// span, so no length is unreachable or coded twice.
#[test]
fn the_word_counts_tile_the_dictionary_and_the_length_codes_tile_their_ranges() {
    let end = DOFFSET[24] + (24 << NDBITS[24]);
    assert_eq!(end, DICTIONARY.len());
    for (name, table, last) in [
        ("block count", &BLOCK_COUNT[..], 16_793_840_u64),
        ("insert", &INSERT[..], 16_799_809),
        ("copy", &COPY[..], 16_779_333),
    ] {
        for pair in table.windows(2) {
            let ((base, extra), (next, _)) = (pair[0], pair[1]);
            assert_eq!(base + (1 << extra), next, "{name} after {base}");
        }
        let (base, extra) = table[table.len() - 1];
        assert_eq!(u64::from(base) + (1 << extra) - 1, last, "{name}");
    }
}
