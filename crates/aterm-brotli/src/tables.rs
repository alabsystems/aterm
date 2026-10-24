// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: BSD-2-Clause
//
// The format's static data, transcribed from the text of RFC 7932 (§§4, 5, 7.1,
// 8, Appendices A and B), whose Code Components the IETF Trust licenses under the
// Simplified BSD License: see LICENSE-BSD-2. Each table the RFC prints a CRC-32 for
// is held to it by this file's tests.

//! RFC 7932's static tables: the length codes, the context lookup tables, the
//! dictionary and its word transforms.

/// The static dictionary (Appendix A): 122,784 bytes, CRC-32 `0x5136cb04`.
pub(crate) static DICTIONARY: &[u8; 122_784] = include_bytes!("dictionary.bin");

/// `NDBITS[len]`: the dictionary holds `1 << NDBITS[len]` words of `len` bytes (§8).
pub(crate) const NDBITS: [u32; 25] = [
    0, 0, 0, 0, 10, 10, 11, 11, 10, 10, 10, 10, 10, 9, 9, 8, 7, 7, 8, 7, 7, 6, 6, 5, 5,
];

/// `DOFFSET[len]`: where the words of `len` bytes start in [`DICTIONARY`] (§8).
pub(crate) const DOFFSET: [usize; 25] = offsets();

const fn offsets() -> [usize; 25] {
    let mut out = [0usize; 25];
    let mut acc = 0usize;
    let mut len = 4;
    while len < 25 {
        out[len] = acc;
        acc += len << NDBITS[len];
        len += 1;
    }
    out
}

/// The shortest and longest dictionary word (§8).
pub(crate) const WORD_MIN: u32 = 4;
/// The longest dictionary word.
pub(crate) const WORD_MAX: u32 = 24;

/// Block count codes: `(base, extra bits)` (§6).
pub(crate) const BLOCK_COUNT: [(u32, u32); 26] = [
    (1, 2),
    (5, 2),
    (9, 2),
    (13, 2),
    (17, 3),
    (25, 3),
    (33, 3),
    (41, 3),
    (49, 4),
    (65, 4),
    (81, 4),
    (97, 4),
    (113, 5),
    (145, 5),
    (177, 5),
    (209, 5),
    (241, 6),
    (305, 6),
    (369, 7),
    (497, 8),
    (753, 9),
    (1265, 10),
    (2289, 11),
    (4337, 12),
    (8433, 13),
    (16625, 24),
];

/// Insert length codes: `(base, extra bits)` (§5).
pub(crate) const INSERT: [(u32, u32); 24] = [
    (0, 0),
    (1, 0),
    (2, 0),
    (3, 0),
    (4, 0),
    (5, 0),
    (6, 1),
    (8, 1),
    (10, 2),
    (14, 2),
    (18, 3),
    (26, 3),
    (34, 4),
    (50, 4),
    (66, 5),
    (98, 5),
    (130, 6),
    (194, 7),
    (322, 8),
    (578, 9),
    (1090, 10),
    (2114, 12),
    (6210, 14),
    (22594, 24),
];

/// Copy length codes: `(base, extra bits)` (§5).
pub(crate) const COPY: [(u32, u32); 24] = [
    (2, 0),
    (3, 0),
    (4, 0),
    (5, 0),
    (6, 0),
    (7, 0),
    (8, 0),
    (9, 0),
    (10, 1),
    (12, 1),
    (14, 2),
    (18, 2),
    (22, 3),
    (30, 3),
    (38, 4),
    (54, 4),
    (70, 5),
    (102, 5),
    (134, 6),
    (198, 7),
    (326, 8),
    (582, 9),
    (1094, 10),
    (2118, 24),
];

/// The insert-and-copy cells (§5): for `code >> 6`, the first insert length code and the
/// first copy length code of its 8 by 8 block, and whether its distance is the implicit
/// distance code 0.
pub(crate) const CELLS: [(u32, u32, bool); 11] = [
    (0, 0, true),
    (0, 8, true),
    (0, 0, false),
    (0, 8, false),
    (8, 0, false),
    (8, 8, false),
    (0, 16, false),
    (16, 0, false),
    (8, 16, false),
    (16, 8, false),
    (16, 16, false),
];

/// The order code length code lengths are read in (§3.5).
pub(crate) const CODE_LENGTH_ORDER: [usize; 18] =
    [1, 2, 3, 4, 0, 5, 17, 6, 16, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// The fixed prefix code for code length code lengths (§3.5), read four bits at a time
/// least significant first: `(bits it takes, the length it codes)` for each 4-bit peek.
pub(crate) const CODE_LENGTH_CODE: [(u32, u8); 16] = [
    (2, 0),
    (2, 4),
    (2, 3),
    (3, 2),
    (2, 0),
    (2, 4),
    (2, 3),
    (4, 1),
    (2, 0),
    (2, 4),
    (2, 3),
    (3, 2),
    (2, 0),
    (2, 4),
    (2, 3),
    (4, 5),
];

/// What a word transform does to the base word between its prefix and suffix (Appendix B).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// The word as it is.
    Identity,
    /// Its first character upper-cased.
    FermentFirst,
    /// Every character upper-cased.
    FermentAll,
    /// Its first `n` bytes dropped.
    OmitFirst(u8),
    /// Its last `n` bytes dropped.
    OmitLast(u8),
}

impl Kind {
    /// The byte Appendix B's serialization gives the kind.
    #[cfg(test)]
    pub(crate) const fn id(self) -> u8 {
        match self {
            Self::Identity => 0,
            Self::FermentFirst => 1,
            Self::FermentAll => 2,
            Self::OmitFirst(n) => 2 + n,
            Self::OmitLast(n) => 11 + n,
        }
    }
}

/// One word transform: a prefix, the base word transformed, a suffix (Appendix B).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Transform {
    /// Bytes written before the word.
    pub(crate) prefix: &'static [u8],
    /// What is done to the word.
    pub(crate) kind: Kind,
    /// Bytes written after the word.
    pub(crate) suffix: &'static [u8],
}

impl Transform {
    const fn new(prefix: &'static [u8], kind: Kind, suffix: &'static [u8]) -> Self {
        Self {
            prefix,
            kind,
            suffix,
        }
    }
}

/// The context lookup tables (§7.1): the UTF8 mode's `LUT0[p1] | LUT1[p2]` and the
/// Signed mode's `(LUT2[p1] << 3) | LUT2[p2]`. CRC-32 `0x8e91efb7`, `0xd01a32f4` and
/// `0x0dd7a0d6`.
pub(crate) const LUT0: [u8; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 4, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    8, 12, 16, 12, 12, 20, 12, 16, 24, 28, 12, 12, 32, 12, 36, 12, 44, 44, 44, 44, 44, 44, 44, 44,
    44, 44, 32, 32, 24, 40, 28, 12, 12, 48, 52, 52, 52, 48, 52, 52, 52, 48, 52, 52, 52, 52, 52, 48,
    52, 52, 52, 52, 52, 48, 52, 52, 52, 52, 52, 24, 12, 28, 12, 12, 12, 56, 60, 60, 60, 56, 60, 60,
    60, 56, 60, 60, 60, 60, 60, 56, 60, 60, 60, 60, 60, 56, 60, 60, 60, 60, 60, 24, 12, 28, 12, 0,
    0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1,
    0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1,
    2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3,
    2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3,
];
pub(crate) const LUT1: [u8; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1,
    1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1,
    1, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 1, 1, 1, 1, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
];
pub(crate) const LUT2: [u8; 256] = [
    0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3,
    3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3,
    4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
    4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
    5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
    5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 7,
];
/// The 121 word transforms (Appendix B): serialized as each prefix, a zero, the kind's byte,
/// the suffix and a zero, 648 bytes with CRC-32 `0x3d965f81`.
pub(crate) const TRANSFORMS: [Transform; 121] = [
    Transform::new(b"", Kind::Identity, b""),              // 0
    Transform::new(b"", Kind::Identity, b" "),             // 1
    Transform::new(b" ", Kind::Identity, b" "),            // 2
    Transform::new(b"", Kind::OmitFirst(1), b""),          // 3
    Transform::new(b"", Kind::FermentFirst, b" "),         // 4
    Transform::new(b"", Kind::Identity, b" the "),         // 5
    Transform::new(b" ", Kind::Identity, b""),             // 6
    Transform::new(b"s ", Kind::Identity, b" "),           // 7
    Transform::new(b"", Kind::Identity, b" of "),          // 8
    Transform::new(b"", Kind::FermentFirst, b""),          // 9
    Transform::new(b"", Kind::Identity, b" and "),         // 10
    Transform::new(b"", Kind::OmitFirst(2), b""),          // 11
    Transform::new(b"", Kind::OmitLast(1), b""),           // 12
    Transform::new(b", ", Kind::Identity, b" "),           // 13
    Transform::new(b"", Kind::Identity, b", "),            // 14
    Transform::new(b" ", Kind::FermentFirst, b" "),        // 15
    Transform::new(b"", Kind::Identity, b" in "),          // 16
    Transform::new(b"", Kind::Identity, b" to "),          // 17
    Transform::new(b"e ", Kind::Identity, b" "),           // 18
    Transform::new(b"", Kind::Identity, b"\""),            // 19
    Transform::new(b"", Kind::Identity, b"."),             // 20
    Transform::new(b"", Kind::Identity, b"\">"),           // 21
    Transform::new(b"", Kind::Identity, b"\n"),            // 22
    Transform::new(b"", Kind::OmitLast(3), b""),           // 23
    Transform::new(b"", Kind::Identity, b"]"),             // 24
    Transform::new(b"", Kind::Identity, b" for "),         // 25
    Transform::new(b"", Kind::OmitFirst(3), b""),          // 26
    Transform::new(b"", Kind::OmitLast(2), b""),           // 27
    Transform::new(b"", Kind::Identity, b" a "),           // 28
    Transform::new(b"", Kind::Identity, b" that "),        // 29
    Transform::new(b" ", Kind::FermentFirst, b""),         // 30
    Transform::new(b"", Kind::Identity, b". "),            // 31
    Transform::new(b".", Kind::Identity, b""),             // 32
    Transform::new(b" ", Kind::Identity, b", "),           // 33
    Transform::new(b"", Kind::OmitFirst(4), b""),          // 34
    Transform::new(b"", Kind::Identity, b" with "),        // 35
    Transform::new(b"", Kind::Identity, b"'"),             // 36
    Transform::new(b"", Kind::Identity, b" from "),        // 37
    Transform::new(b"", Kind::Identity, b" by "),          // 38
    Transform::new(b"", Kind::OmitFirst(5), b""),          // 39
    Transform::new(b"", Kind::OmitFirst(6), b""),          // 40
    Transform::new(b" the ", Kind::Identity, b""),         // 41
    Transform::new(b"", Kind::OmitLast(4), b""),           // 42
    Transform::new(b"", Kind::Identity, b". The "),        // 43
    Transform::new(b"", Kind::FermentAll, b""),            // 44
    Transform::new(b"", Kind::Identity, b" on "),          // 45
    Transform::new(b"", Kind::Identity, b" as "),          // 46
    Transform::new(b"", Kind::Identity, b" is "),          // 47
    Transform::new(b"", Kind::OmitLast(7), b""),           // 48
    Transform::new(b"", Kind::OmitLast(1), b"ing "),       // 49
    Transform::new(b"", Kind::Identity, b"\n\t"),          // 50
    Transform::new(b"", Kind::Identity, b":"),             // 51
    Transform::new(b" ", Kind::Identity, b". "),           // 52
    Transform::new(b"", Kind::Identity, b"ed "),           // 53
    Transform::new(b"", Kind::OmitFirst(9), b""),          // 54
    Transform::new(b"", Kind::OmitFirst(7), b""),          // 55
    Transform::new(b"", Kind::OmitLast(6), b""),           // 56
    Transform::new(b"", Kind::Identity, b"("),             // 57
    Transform::new(b"", Kind::FermentFirst, b", "),        // 58
    Transform::new(b"", Kind::OmitLast(8), b""),           // 59
    Transform::new(b"", Kind::Identity, b" at "),          // 60
    Transform::new(b"", Kind::Identity, b"ly "),           // 61
    Transform::new(b" the ", Kind::Identity, b" of "),     // 62
    Transform::new(b"", Kind::OmitLast(5), b""),           // 63
    Transform::new(b"", Kind::OmitLast(9), b""),           // 64
    Transform::new(b" ", Kind::FermentFirst, b", "),       // 65
    Transform::new(b"", Kind::FermentFirst, b"\""),        // 66
    Transform::new(b".", Kind::Identity, b"("),            // 67
    Transform::new(b"", Kind::FermentAll, b" "),           // 68
    Transform::new(b"", Kind::FermentFirst, b"\">"),       // 69
    Transform::new(b"", Kind::Identity, b"=\""),           // 70
    Transform::new(b" ", Kind::Identity, b"."),            // 71
    Transform::new(b".com/", Kind::Identity, b""),         // 72
    Transform::new(b" the ", Kind::Identity, b" of the "), // 73
    Transform::new(b"", Kind::FermentFirst, b"'"),         // 74
    Transform::new(b"", Kind::Identity, b". This "),       // 75
    Transform::new(b"", Kind::Identity, b","),             // 76
    Transform::new(b".", Kind::Identity, b" "),            // 77
    Transform::new(b"", Kind::FermentFirst, b"("),         // 78
    Transform::new(b"", Kind::FermentFirst, b"."),         // 79
    Transform::new(b"", Kind::Identity, b" not "),         // 80
    Transform::new(b" ", Kind::Identity, b"=\""),          // 81
    Transform::new(b"", Kind::Identity, b"er "),           // 82
    Transform::new(b" ", Kind::FermentAll, b" "),          // 83
    Transform::new(b"", Kind::Identity, b"al "),           // 84
    Transform::new(b" ", Kind::FermentAll, b""),           // 85
    Transform::new(b"", Kind::Identity, b"='"),            // 86
    Transform::new(b"", Kind::FermentAll, b"\""),          // 87
    Transform::new(b"", Kind::FermentFirst, b". "),        // 88
    Transform::new(b" ", Kind::Identity, b"("),            // 89
    Transform::new(b"", Kind::Identity, b"ful "),          // 90
    Transform::new(b" ", Kind::FermentFirst, b". "),       // 91
    Transform::new(b"", Kind::Identity, b"ive "),          // 92
    Transform::new(b"", Kind::Identity, b"less "),         // 93
    Transform::new(b"", Kind::FermentAll, b"'"),           // 94
    Transform::new(b"", Kind::Identity, b"est "),          // 95
    Transform::new(b" ", Kind::FermentFirst, b"."),        // 96
    Transform::new(b"", Kind::FermentAll, b"\">"),         // 97
    Transform::new(b" ", Kind::Identity, b"='"),           // 98
    Transform::new(b"", Kind::FermentFirst, b","),         // 99
    Transform::new(b"", Kind::Identity, b"ize "),          // 100
    Transform::new(b"", Kind::FermentAll, b"."),           // 101
    Transform::new(b"\xc2\xa0", Kind::Identity, b""),      // 102
    Transform::new(b" ", Kind::Identity, b","),            // 103
    Transform::new(b"", Kind::FermentFirst, b"=\""),       // 104
    Transform::new(b"", Kind::FermentAll, b"=\""),         // 105
    Transform::new(b"", Kind::Identity, b"ous "),          // 106
    Transform::new(b"", Kind::FermentAll, b", "),          // 107
    Transform::new(b"", Kind::FermentFirst, b"='"),        // 108
    Transform::new(b" ", Kind::FermentFirst, b","),        // 109
    Transform::new(b" ", Kind::FermentAll, b"=\""),        // 110
    Transform::new(b" ", Kind::FermentAll, b", "),         // 111
    Transform::new(b"", Kind::FermentAll, b","),           // 112
    Transform::new(b"", Kind::FermentAll, b"("),           // 113
    Transform::new(b"", Kind::FermentAll, b". "),          // 114
    Transform::new(b" ", Kind::FermentAll, b"."),          // 115
    Transform::new(b"", Kind::FermentAll, b"='"),          // 116
    Transform::new(b" ", Kind::FermentAll, b". "),         // 117
    Transform::new(b" ", Kind::FermentFirst, b"=\""),      // 118
    Transform::new(b" ", Kind::FermentAll, b"='"),         // 119
    Transform::new(b" ", Kind::FermentFirst, b"='"),       // 120
];

#[cfg(test)]
mod tests {
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
}
