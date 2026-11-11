// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::hash::Hash;

macro_rules! test_hash {
    ($($value:expr => ($expected_32:expr, $expected_64:expr),)*) => {
        $(
            let v = $value;
            let expected_native = if B32 { $expected_32 } else { $expected_64 };
            assert_eq!(
                FxBuildHasher.hash_one(&v), expected_native,
                "native hash mismatch for {:?}", v
            );
            assert_eq!(
                fx32::FxBuildHasher32.hash_one(&v), $expected_32,
                "32-bit hash mismatch for {:?}", v
            );
        )*
    };
}

const B32: bool = cfg!(target_pointer_width = "32");

#[test]
fn test_fxhasher_unsigned_integers_match_rustc_hash() {
    test_hash! {
        0_u8 => (0, 0),
        1_u8 => (3001993707, 12157901119326311915),
        100_u8 => (3844759569, 16751747135202103309),
        u8::MAX => (999399879, 1211781028898739645),

        0_u16 => (0, 0),
        1_u16 => (3001993707, 12157901119326311915),
        100_u16 => (3844759569, 16751747135202103309),
        u16::MAX => (3440503042, 16279819243059860173),

        0_u32 => (0, 0),
        1_u32 => (3001993707, 12157901119326311915),
        100_u32 => (3844759569, 16751747135202103309),
        u32::MAX => (1293006356, 7729994835221066939),

        0_u64 => (0, 0),
        1_u64 => (275023839, 12157901119326311915),
        100_u64 => (1732383522, 16751747135202103309),
        u64::MAX => (1017982517, 6288842954450348564),

        0_u128 => (0, 0),
        1_u128 => (1860738631, 13032756267696824044),
        100_u128 => (1389515751, 12003541609544029302),
        u128::MAX => (2156022013, 11702830760530184999),

        0_usize => (0, 0),
        1_usize => (3001993707, 12157901119326311915),
        100_usize => (3844759569, 16751747135202103309),
        usize::MAX => (1293006356, 6288842954450348564),
    }
}

#[test]
fn test_fxhasher_signed_integers_match_rustc_hash() {
    test_hash! {
        i8::MIN => (2000713177, 6684841074112525780),
        0_i8 => (0, 0),
        1_i8 => (3001993707, 12157901119326311915),
        i8::MAX => (3293686765, 12973684028562874344),

        i16::MIN => (1073764727, 14218860181193086044),
        0_i16 => (0, 0),
        i16::MAX => (2366738315, 2060959061933882993),

        i32::MIN => (16384, 9943947977240134995),
        0_i32 => (0, 0),
        i32::MAX => (1293022740, 16232790931690483559),

        i64::MIN => (16384, 33554432),
        0_i64 => (0, 0),
        i64::MAX => (1017998901, 6288842954483902996),
    }
}

/// Feeds raw bytes to the hasher, bypassing std's `Hash` impls.
#[derive(Debug)]
struct HashBytes(&'static [u8]);
impl Hash for HashBytes {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write(self.0);
    }
}

#[test]
fn test_fxhasher_bytes_match_rustc_hash() {
    test_hash! {
        HashBytes(&[]) => (2673204745, 17606491139363777937),
        HashBytes(&[0]) => (2948228584, 5448590020104574886),
        HashBytes(&[0, 0, 0, 0, 0, 0]) => (3223252423, 16766921560080789783),
        HashBytes(&[1]) => (2943445104, 5922447956811044110),
        HashBytes(&[2]) => (1055423297, 5229781508510959783),
        HashBytes(b"uwu") => (2699662140, 7168164714682931527),
        HashBytes(b"These are some bytes for testing rustc_hash.") =>
            (2303640537, 2349210501944688211),
    }

    // Verify the 1-, 2-, and 3-byte literal slice-pattern arms of `hash_bytes`
    // match rustc-hash 2.1's `lo = bs[0]`, `mid = bs[len / 2]`, `hi = bs[len - 1]` formula.
    for short in [&b"x"[..], &[0, 0], &[1, 2], b"hi", &[0xff, 0x00], b"abc"] {
        let lo = u64::from(short[0]);
        let mid = u64::from(short[short.len() / 2]);
        let hi = u64::from(short[short.len() - 1]);
        let expected_word =
            multiply_mix(SEED1 ^ lo, SEED2 ^ ((hi << 8) | mid)) ^ (short.len() as u64);
        let mut ref_hasher = FxHasher::default();
        ref_hasher.write_u64(expected_word);
        assert_eq!(
            fx_hash_bytes(short),
            ref_hasher.finish(),
            "short slice {short:?}"
        );
    }
}

#[test]
fn test_fxhasher_str_includes_terminator_byte() {
    for s in ["", "a", "hi", "uwu", "aterm_hash::FxHasher"] {
        let mut manual = FxHasher::default();
        manual.write(s.as_bytes());
        manual.write_u8(0xff);
        assert_eq!(FxBuildHasher.hash_one(s), manual.finish(), "{s:?}");
        assert_ne!(
            FxBuildHasher.hash_one(s),
            fx_hash_bytes(s.as_bytes()),
            "{s:?}: str Hash appends 0xff"
        );
    }
    assert_ne!(
        FxBuildHasher.hash_one(("ab", "c")),
        FxBuildHasher.hash_one(("a", "bc"))
    );
}
