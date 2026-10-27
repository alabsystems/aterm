// Copyright (c) HarfBuzz developers
// Copyright (c) 2020 Yevhenii Reizner
// SPDX-License-Identifier: MIT AND (MIT OR Apache-2.0)
//
// Derived from unicode-ccc 0.4.0 src/lib.rs, rustybuzz 0.20.1 src/hb/unicode.rs
// (`modified_combining_class`), and src/hb/ot_shape_fallback.rs
// (`conv_combining_class`) and modified by the aterm project in 2026.
// See ../../LICENSE-RUSTYBUZZ, ../../LICENSE-MIT, and ../../LICENSE-APACHE for
// the upstream licenses.

//! Unicode Canonical Combining Class and HarfBuzz modified combining class tables.

/// Character Canonical Combining Class (Unicode 16.0.0).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CanonicalCombiningClass {
    NotReordered = 0,
    Overlay = 1,
    HanReading = 6,
    Nukta = 7,
    KanaVoicing = 8,
    Virama = 9,
    CCC10 = 10,
    CCC11 = 11,
    CCC12 = 12,
    CCC13 = 13,
    CCC14 = 14,
    CCC15 = 15,
    CCC16 = 16,
    CCC17 = 17,
    CCC18 = 18,
    CCC19 = 19,
    CCC20 = 20,
    CCC21 = 21,
    CCC22 = 22,
    CCC23 = 23,
    CCC24 = 24,
    CCC25 = 25,
    CCC26 = 26,
    CCC27 = 27,
    CCC28 = 28,
    CCC29 = 29,
    CCC30 = 30,
    CCC31 = 31,
    CCC32 = 32,
    CCC33 = 33,
    CCC34 = 34,
    CCC35 = 35,
    CCC36 = 36,
    CCC84 = 84,
    CCC91 = 91,
    CCC103 = 103,
    CCC107 = 107,
    CCC118 = 118,
    CCC122 = 122,
    CCC129 = 129,
    CCC130 = 130,
    CCC132 = 132,
    AttachedBelowLeft = 200,
    AttachedBelow = 202,
    AttachedAbove = 214,
    AttachedAboveRight = 216,
    BelowLeft = 218,
    Below = 220,
    BelowRight = 222,
    Left = 224,
    Right = 226,
    AboveLeft = 228,
    Above = 230,
    AboveRight = 232,
    DoubleBelow = 233,
    DoubleAbove = 234,
    IotaSubscript = 240,
}

pub(crate) mod mcc {
    pub(crate) const CCC10: u8 = 22;
    pub(crate) const CCC11: u8 = 15;
    pub(crate) const CCC12: u8 = 16;
    pub(crate) const CCC13: u8 = 17;
    pub(crate) const CCC14: u8 = 23;
    pub(crate) const CCC15: u8 = 18;
    pub(crate) const CCC16: u8 = 19;
    pub(crate) const CCC17: u8 = 20;
    pub(crate) const CCC18: u8 = 21;
    pub(crate) const CCC19: u8 = 14;
    pub(crate) const CCC20: u8 = 24;
    pub(crate) const CCC21: u8 = 12;
    pub(crate) const CCC22: u8 = 25;
    pub(crate) const CCC23: u8 = 13;
    pub(crate) const CCC24: u8 = 10;
    pub(crate) const CCC25: u8 = 11;
    pub(crate) const CCC26: u8 = 26;

    pub(crate) const CCC27: u8 = 28;
    pub(crate) const CCC28: u8 = 29;
    pub(crate) const CCC29: u8 = 30;
    pub(crate) const CCC30: u8 = 31;
    pub(crate) const CCC31: u8 = 32;
    pub(crate) const CCC32: u8 = 33;
    pub(crate) const CCC33: u8 = 27;
    pub(crate) const CCC34: u8 = 34;
    pub(crate) const CCC35: u8 = 35;

    pub(crate) const CCC36: u8 = 36;

    pub(crate) const CCC84: u8 = 0;
    pub(crate) const CCC91: u8 = 0;

    pub(crate) const CCC103: u8 = 3;
    pub(crate) const CCC107: u8 = 107;

    pub(crate) const CCC118: u8 = 118;
    pub(crate) const CCC122: u8 = 122;

    pub(crate) const CCC129: u8 = 129;
    pub(crate) const CCC130: u8 = 132;
    pub(crate) const CCC132: u8 = 131;
}

#[rustfmt::skip]
const MODIFIED_COMBINING_CLASS: [u8; 256] = [
    CanonicalCombiningClass::NotReordered as u8,
    CanonicalCombiningClass::Overlay as u8,
    2, 3, 4, 5, 6,
    CanonicalCombiningClass::Nukta as u8,
    CanonicalCombiningClass::KanaVoicing as u8,
    CanonicalCombiningClass::Virama as u8,

    mcc::CCC10, mcc::CCC11, mcc::CCC12, mcc::CCC13, mcc::CCC14,
    mcc::CCC15, mcc::CCC16, mcc::CCC17, mcc::CCC18, mcc::CCC19,
    mcc::CCC20, mcc::CCC21, mcc::CCC22, mcc::CCC23, mcc::CCC24,
    mcc::CCC25, mcc::CCC26,

    mcc::CCC27, mcc::CCC28, mcc::CCC29, mcc::CCC30, mcc::CCC31,
    mcc::CCC32, mcc::CCC33, mcc::CCC34, mcc::CCC35,

    mcc::CCC36,

    37, 38, 39,
    40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59,
    60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79,
    80, 81, 82, 83,

    mcc::CCC84,
    85, 86, 87, 88, 89, 90,
    mcc::CCC91,
    92, 93, 94, 95, 96, 97, 98, 99, 100, 101, 102,

    mcc::CCC103,
    104, 105, 106,
    mcc::CCC107,
    108, 109, 110, 111, 112, 113, 114, 115, 116, 117,

    mcc::CCC118,
    119, 120, 121,
    mcc::CCC122,
    123, 124, 125, 126, 127, 128,

    mcc::CCC129,
    mcc::CCC130,
    131,
    mcc::CCC132,
    133, 134, 135, 136, 137, 138, 139,

    140, 141, 142, 143, 144, 145, 146, 147, 148, 149,
    150, 151, 152, 153, 154, 155, 156, 157, 158, 159,
    160, 161, 162, 163, 164, 165, 166, 167, 168, 169,
    170, 171, 172, 173, 174, 175, 176, 177, 178, 179,
    180, 181, 182, 183, 184, 185, 186, 187, 188, 189,
    190, 191, 192, 193, 194, 195, 196, 197, 198, 199,

    CanonicalCombiningClass::AttachedBelowLeft as u8,
    201,
    CanonicalCombiningClass::AttachedBelow as u8,
    203, 204, 205, 206, 207, 208, 209, 210, 211, 212, 213,
    CanonicalCombiningClass::AttachedAbove as u8,
    215,
    CanonicalCombiningClass::AttachedAboveRight as u8,
    217,
    CanonicalCombiningClass::BelowLeft as u8,
    219,
    CanonicalCombiningClass::Below as u8,
    221,
    CanonicalCombiningClass::BelowRight as u8,
    223,
    CanonicalCombiningClass::Left as u8,
    225,
    CanonicalCombiningClass::Right as u8,
    227,
    CanonicalCombiningClass::AboveLeft as u8,
    229,
    CanonicalCombiningClass::Above as u8,
    231,
    CanonicalCombiningClass::AboveRight as u8,
    CanonicalCombiningClass::DoubleBelow as u8,
    CanonicalCombiningClass::DoubleAbove as u8,
    235, 236, 237, 238, 239,
    CanonicalCombiningClass::IotaSubscript as u8,
    241, 242, 243, 244, 245, 246, 247, 248, 249, 250, 251, 252, 253, 254,
    255,
];

#[inline]
pub(crate) const fn char_modified_combining_class(u: char) -> u8 {
    match u {
        '\u{1A60}' | '\u{0FC6}' => 254,
        '\u{0F39}' => 127,
        _ => MODIFIED_COMBINING_CLASS[get_canonical_combining_class(u) as usize],
    }
}

#[inline]
pub(crate) const fn conv_combining_class(n: u8) -> CanonicalCombiningClass {
    use CanonicalCombiningClass as Class;
    match n {
        1 => Class::Overlay,
        6 => Class::HanReading,
        7 => Class::Nukta,
        8 => Class::KanaVoicing,
        9 => Class::Virama,
        10 => Class::CCC10,
        11 => Class::CCC11,
        12 => Class::CCC12,
        13 => Class::CCC13,
        14 => Class::CCC14,
        15 => Class::CCC15,
        16 => Class::CCC16,
        17 => Class::CCC17,
        18 => Class::CCC18,
        19 => Class::CCC19,
        20 => Class::CCC20,
        21 => Class::CCC21,
        22 => Class::CCC22,
        23 => Class::CCC23,
        24 => Class::CCC24,
        25 => Class::CCC25,
        26 => Class::CCC26,
        27 => Class::CCC27,
        28 => Class::CCC28,
        29 => Class::CCC29,
        30 => Class::CCC30,
        31 => Class::CCC31,
        32 => Class::CCC32,
        33 => Class::CCC33,
        34 => Class::CCC34,
        35 => Class::CCC35,
        36 => Class::CCC36,
        84 => Class::CCC84,
        91 => Class::CCC91,
        103 => Class::CCC103,
        107 => Class::CCC107,
        118 => Class::CCC118,
        122 => Class::CCC122,
        129 => Class::CCC129,
        130 => Class::CCC130,
        132 => Class::CCC132,
        200 => Class::AttachedBelowLeft,
        202 => Class::AttachedBelow,
        214 => Class::AttachedAbove,
        216 => Class::AttachedAboveRight,
        218 => Class::BelowLeft,
        220 => Class::Below,
        222 => Class::BelowRight,
        224 => Class::Left,
        226 => Class::Right,
        228 => Class::AboveLeft,
        230 => Class::Above,
        232 => Class::AboveRight,
        233 => Class::DoubleBelow,
        234 => Class::DoubleAbove,
        240 => Class::IotaSubscript,
        _ => Class::NotReordered,
    }
}

/// Returns the Unicode Canonical Combining Class of `c`.
pub(crate) const fn get_canonical_combining_class(c: char) -> CanonicalCombiningClass {
    use CanonicalCombiningClass::*;

    match c as u32 {
        0x0334..=0x0338
        | 0x1CD4
        | 0x1CE2..=0x1CE8
        | 0x20D2..=0x20D3
        | 0x20D8..=0x20DA
        | 0x20E5..=0x20E6
        | 0x20EA..=0x20EB
        | 0x10A39
        | 0x16AF0..=0x16AF4
        | 0x1BC9E
        | 0x1D167..=0x1D169 => Overlay,
        0x16FF0..=0x16FF1 => HanReading,
        0x093C
        | 0x09BC
        | 0x0A3C
        | 0x0ABC
        | 0x0B3C
        | 0x0C3C
        | 0x0CBC
        | 0x1037
        | 0x1B34
        | 0x1BE6
        | 0x1C37
        | 0xA9B3
        | 0x110BA
        | 0x11173
        | 0x111CA
        | 0x11236
        | 0x112E9
        | 0x1133B..=0x1133C
        | 0x11446
        | 0x114C3
        | 0x115C0
        | 0x116B7
        | 0x1183A
        | 0x11943
        | 0x11D42
        | 0x1E94A => Nukta,
        0x3099..=0x309A => KanaVoicing,
        0x094D
        | 0x09CD
        | 0x0A4D
        | 0x0ACD
        | 0x0B4D
        | 0x0BCD
        | 0x0C4D
        | 0x0CCD
        | 0x0D3B..=0x0D3C
        | 0x0D4D
        | 0x0DCA
        | 0x0E3A
        | 0x0EBA
        | 0x0F84
        | 0x1039..=0x103A
        | 0x1714..=0x1715
        | 0x1734
        | 0x17D2
        | 0x1A60
        | 0x1B44
        | 0x1BAA..=0x1BAB
        | 0x1BF2..=0x1BF3
        | 0x2D7F
        | 0xA806
        | 0xA82C
        | 0xA8C4
        | 0xA953
        | 0xA9C0
        | 0xAAF6
        | 0xABED
        | 0x10A3F
        | 0x11046
        | 0x11070
        | 0x1107F
        | 0x110B9
        | 0x11133..=0x11134
        | 0x111C0
        | 0x11235
        | 0x112EA
        | 0x1134D
        | 0x113CE..=0x113D0
        | 0x11442
        | 0x114C2
        | 0x115BF
        | 0x1163F
        | 0x116B6
        | 0x1172B
        | 0x11839
        | 0x1193D..=0x1193E
        | 0x119E0
        | 0x11A34
        | 0x11A47
        | 0x11A99
        | 0x11C3F
        | 0x11D44..=0x11D45
        | 0x11D97
        | 0x11F41..=0x11F42
        | 0x1612F => Virama,
        0x05B0 => CCC10,
        0x05B1 => CCC11,
        0x05B2 => CCC12,
        0x05B3 => CCC13,
        0x05B4 => CCC14,
        0x05B5 => CCC15,
        0x05B6 => CCC16,
        0x05B7 => CCC17,
        0x05B8 | 0x05C7 => CCC18,
        0x05B9..=0x05BA => CCC19,
        0x05BB => CCC20,
        0x05BC => CCC21,
        0x05BD => CCC22,
        0x05BF => CCC23,
        0x05C1 => CCC24,
        0x05C2 => CCC25,
        0xFB1E => CCC26,
        0x064B | 0x08F0 => CCC27,
        0x064C | 0x08F1 => CCC28,
        0x064D | 0x08F2 => CCC29,
        0x0618 | 0x064E => CCC30,
        0x0619 | 0x064F => CCC31,
        0x061A | 0x0650 => CCC32,
        0x0651 => CCC33,
        0x0652 => CCC34,
        0x0670 => CCC35,
        0x0711 => CCC36,
        0x0C55 => CCC84,
        0x0C56 => CCC91,
        0x0E38..=0x0E39 => CCC103,
        0x0E48..=0x0E4B => CCC107,
        0x0EB8..=0x0EB9 => CCC118,
        0x0EC8..=0x0ECB => CCC122,
        0x0F71 => CCC129,
        0x0F72 | 0x0F7A..=0x0F7D | 0x0F80 => CCC130,
        0x0F74 => CCC132,
        0x0321..=0x0322 | 0x0327..=0x0328 | 0x1DD0 => AttachedBelow,
        0x1DCE => AttachedAbove,
        0x031B | 0x0F39 | 0x1D165..=0x1D166 | 0x1D16E..=0x1D172 => AttachedAboveRight,
        0x1DFA | 0x302A => BelowLeft,
        0x0316..=0x0319
        | 0x031C..=0x0320
        | 0x0323..=0x0326
        | 0x0329..=0x0333
        | 0x0339..=0x033C
        | 0x0347..=0x0349
        | 0x034D..=0x034E
        | 0x0353..=0x0356
        | 0x0359..=0x035A
        | 0x0591
        | 0x0596
        | 0x059B
        | 0x05A2..=0x05A7
        | 0x05AA
        | 0x05C5
        | 0x0655..=0x0656
        | 0x065C
        | 0x065F
        | 0x06E3
        | 0x06EA
        | 0x06ED
        | 0x0731
        | 0x0734
        | 0x0737..=0x0739
        | 0x073B..=0x073C
        | 0x073E
        | 0x0742
        | 0x0744
        | 0x0746
        | 0x0748
        | 0x07F2
        | 0x07FD
        | 0x0859..=0x085B
        | 0x0899..=0x089B
        | 0x08CF..=0x08D3
        | 0x08E3
        | 0x08E6
        | 0x08E9
        | 0x08ED..=0x08EF
        | 0x08F6
        | 0x08F9..=0x08FA
        | 0x0952
        | 0x0F18..=0x0F19
        | 0x0F35
        | 0x0F37
        | 0x0FC6
        | 0x108D
        | 0x193B
        | 0x1A18
        | 0x1A7F
        | 0x1AB5..=0x1ABA
        | 0x1ABD
        | 0x1ABF..=0x1AC0
        | 0x1AC3..=0x1AC4
        | 0x1ACA
        | 0x1B6C
        | 0x1CD5..=0x1CD9
        | 0x1CDC..=0x1CDF
        | 0x1CED
        | 0x1DC2
        | 0x1DCA
        | 0x1DCF
        | 0x1DF9
        | 0x1DFD
        | 0x1DFF
        | 0x20E8
        | 0x20EC..=0x20EF
        | 0xA92B..=0xA92D
        | 0xAAB4
        | 0xFE27..=0xFE2D
        | 0x101FD
        | 0x102E0
        | 0x10A0D
        | 0x10A3A
        | 0x10AE6
        | 0x10EFD..=0x10EFF
        | 0x10F46..=0x10F47
        | 0x10F4B
        | 0x10F4D..=0x10F50
        | 0x10F83
        | 0x10F85
        | 0x1D17B..=0x1D182
        | 0x1D18A..=0x1D18B
        | 0x1E4EE
        | 0x1E5EF
        | 0x1E8D0..=0x1E8D6 => Below,
        0x059A | 0x05AD | 0x1939 | 0x302D => BelowRight,
        0x302E..=0x302F => Left,
        0x1D16D => Right,
        0x05AE | 0x18A9 | 0x1DF7..=0x1DF8 | 0x302B => AboveLeft,
        0x0300..=0x0314
        | 0x033D..=0x0344
        | 0x0346
        | 0x034A..=0x034C
        | 0x0350..=0x0352
        | 0x0357
        | 0x035B
        | 0x0363..=0x036F
        | 0x0483..=0x0487
        | 0x0592..=0x0595
        | 0x0597..=0x0599
        | 0x059C..=0x05A1
        | 0x05A8..=0x05A9
        | 0x05AB..=0x05AC
        | 0x05AF
        | 0x05C4
        | 0x0610..=0x0617
        | 0x0653..=0x0654
        | 0x0657..=0x065B
        | 0x065D..=0x065E
        | 0x06D6..=0x06DC
        | 0x06DF..=0x06E2
        | 0x06E4
        | 0x06E7..=0x06E8
        | 0x06EB..=0x06EC
        | 0x0730
        | 0x0732..=0x0733
        | 0x0735..=0x0736
        | 0x073A
        | 0x073D
        | 0x073F..=0x0741
        | 0x0743
        | 0x0745
        | 0x0747
        | 0x0749..=0x074A
        | 0x07EB..=0x07F1
        | 0x07F3
        | 0x0816..=0x0819
        | 0x081B..=0x0823
        | 0x0825..=0x0827
        | 0x0829..=0x082D
        | 0x0897..=0x0898
        | 0x089C..=0x089F
        | 0x08CA..=0x08CE
        | 0x08D4..=0x08E1
        | 0x08E4..=0x08E5
        | 0x08E7..=0x08E8
        | 0x08EA..=0x08EC
        | 0x08F3..=0x08F5
        | 0x08F7..=0x08F8
        | 0x08FB..=0x08FF
        | 0x0951
        | 0x0953..=0x0954
        | 0x09FE
        | 0x0F82..=0x0F83
        | 0x0F86..=0x0F87
        | 0x135D..=0x135F
        | 0x17DD
        | 0x193A
        | 0x1A17
        | 0x1A75..=0x1A7C
        | 0x1AB0..=0x1AB4
        | 0x1ABB..=0x1ABC
        | 0x1AC1..=0x1AC2
        | 0x1AC5..=0x1AC9
        | 0x1ACB..=0x1ACE
        | 0x1B6B
        | 0x1B6D..=0x1B73
        | 0x1CD0..=0x1CD2
        | 0x1CDA..=0x1CDB
        | 0x1CE0
        | 0x1CF4
        | 0x1CF8..=0x1CF9
        | 0x1DC0..=0x1DC1
        | 0x1DC3..=0x1DC9
        | 0x1DCB..=0x1DCC
        | 0x1DD1..=0x1DF5
        | 0x1DFB
        | 0x1DFE
        | 0x20D0..=0x20D1
        | 0x20D4..=0x20D7
        | 0x20DB..=0x20DC
        | 0x20E1
        | 0x20E7
        | 0x20E9
        | 0x20F0
        | 0x2CEF..=0x2CF1
        | 0x2DE0..=0x2DFF
        | 0xA66F
        | 0xA674..=0xA67D
        | 0xA69E..=0xA69F
        | 0xA6F0..=0xA6F1
        | 0xA8E0..=0xA8F1
        | 0xAAB0
        | 0xAAB2..=0xAAB3
        | 0xAAB7..=0xAAB8
        | 0xAABE..=0xAABF
        | 0xAAC1
        | 0xFE20..=0xFE26
        | 0xFE2E..=0xFE2F
        | 0x10376..=0x1037A
        | 0x10A0F
        | 0x10A38
        | 0x10AE5
        | 0x10D24..=0x10D27
        | 0x10D69..=0x10D6D
        | 0x10EAB..=0x10EAC
        | 0x10F48..=0x10F4A
        | 0x10F4C
        | 0x10F82
        | 0x10F84
        | 0x11100..=0x11102
        | 0x11366..=0x1136C
        | 0x11370..=0x11374
        | 0x1145E
        | 0x16B30..=0x16B36
        | 0x1D185..=0x1D189
        | 0x1D1AA..=0x1D1AD
        | 0x1D242..=0x1D244
        | 0x1E000..=0x1E006
        | 0x1E008..=0x1E018
        | 0x1E01B..=0x1E021
        | 0x1E023..=0x1E024
        | 0x1E026..=0x1E02A
        | 0x1E08F
        | 0x1E130..=0x1E136
        | 0x1E2AE
        | 0x1E2EC..=0x1E2EF
        | 0x1E4EF
        | 0x1E5EE
        | 0x1E944..=0x1E949 => Above,
        0x0315 | 0x031A | 0x0358 | 0x1DF6 | 0x302C | 0x1E4EC..=0x1E4ED => AboveRight,
        0x035C | 0x035F | 0x0362 | 0x1DFC => DoubleBelow,
        0x035D..=0x035E | 0x0360..=0x0361 | 0x1DCD => DoubleAbove,
        0x0345 => IotaSubscript,
        _ => NotReordered,
    }
}
