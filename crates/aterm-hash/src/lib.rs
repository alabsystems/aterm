// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Fast non-cryptographic hashing for aterm.
//!
//! # Example
//!
//! ```rust
//! use aterm_hash::FxHashMap;
//!
//! let mut map: FxHashMap<u32, u32> = FxHashMap::default();
//! map.insert(22, 44);
//! assert_eq!(map[&22], 44);
//! ```

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, Hasher};

mod fnv;
#[cfg(any(test, target_pointer_width = "32"))]
mod fx32;
pub use fnv::{
    FNV64_OFFSET, FNV64_PRIME, FNV128_OFFSET, FNV128_PRIME, Fnv1a64, bucket_index, fnv_mix_u64,
    fnv1a_64, fnv1a_64_array8, fnv1a_64_extend, fnv1a_64_to_hex16, fnv1a_64_u16_le,
    fnv1a_64_u32_le, fnv1a_64_u64_le, fnv1a_128, fnv1a_128_extend,
};

/// Type alias for a hash map using the Fx hashing algorithm.
pub type FxHashMap<K, V> = HashMap<K, V, FxBuildHasher>;

/// Type alias for a hash set using the Fx hashing algorithm.
pub type FxHashSet<V> = HashSet<V, FxBuildHasher>;

/// A hash map whose hash carries a per-process secret.
pub type KeyedHashMap<K, V> = HashMap<K, V, KeyedState>;

/// [`KeyedHashMap`]'s hasher builder.
///
/// ```
/// use std::hash::BuildHasher;
/// use aterm_hash::KeyedState;
/// let state = KeyedState::default();
/// assert_eq!(state.hash_one("same"), state.hash_one("same"));
/// // Each map's state carries a key of its own.
/// assert_ne!(state.hash_one("same"), KeyedState::default().hash_one("same"));
/// ```
#[derive(Clone, Debug, Default)]
pub struct KeyedState(std::hash::RandomState);

impl BuildHasher for KeyedState {
    type Hasher = KeyedHasher;

    #[inline]
    fn build_hasher(&self) -> KeyedHasher {
        KeyedHasher(self.0.build_hasher())
    }
}

/// [`KeyedState`]'s hasher.
#[derive(Clone, Debug)]
pub struct KeyedHasher(std::hash::DefaultHasher);

impl Hasher for KeyedHasher {
    #[cfg_attr(debug_assertions, inline(never))]
    fn write(&mut self, bytes: &[u8]) {
        self.0.write(bytes);
    }

    #[cfg_attr(debug_assertions, inline(never))]
    fn finish(&self) -> u64 {
        self.0.finish()
    }
}

#[cfg(target_pointer_width = "64")]
const K: usize = 0xf1357aea2e62a9c5;
#[cfg(target_pointer_width = "32")]
const K: usize = fx32::K32 as usize;

/// A fast, non-cryptographic hasher (the `rustc-hash` FxHash algorithm).
#[derive(Clone)]
pub struct FxHasher {
    hash: usize,
}

impl FxHasher {
    #[inline]
    const fn add_to_hash(&mut self, i: usize) {
        #[cfg(target_pointer_width = "64")]
        {
            self.hash = self.hash.wrapping_add(i).wrapping_mul(K);
        }
        #[cfg(target_pointer_width = "32")]
        {
            let _ = K;
            self.hash = fx32::add_to_hash_32(self.hash as u32, i as u32) as usize;
        }
    }
}

impl Default for FxHasher {
    #[inline]
    fn default() -> FxHasher {
        FxHasher { hash: 0 }
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        // Compress the byte string to a single u64 and feed into the hash.
        self.write_u64(hash_bytes(bytes));
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add_to_hash(i as usize);
    }

    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.add_to_hash(i as usize);
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add_to_hash(i as usize);
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        #[cfg(target_pointer_width = "64")]
        self.add_to_hash(i as usize);
        #[cfg(target_pointer_width = "32")]
        {
            self.hash = fx32::write_u64_32(self.hash as u32, i) as usize;
        }
    }

    #[inline]
    fn write_u128(&mut self, i: u128) {
        #[cfg(target_pointer_width = "64")]
        {
            let b = i.to_le_bytes();
            let bs: &[u8] = &b;
            let lo = read_u64_le(bs);
            let hi = if bs.len() >= 16 {
                read_u64_le(&bs[8..16])
            } else {
                0
            };
            self.add_to_hash(lo as usize);
            self.add_to_hash(hi as usize);
        }
        #[cfg(target_pointer_width = "32")]
        {
            self.hash = fx32::write_u128_32(self.hash as u32, i) as usize;
        }
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add_to_hash(i);
    }

    #[inline]
    fn finish(&self) -> u64 {
        #[cfg(target_pointer_width = "64")]
        {
            const ROTATE: u32 = 26;
            self.hash.rotate_left(ROTATE) as u64
        }
        #[cfg(target_pointer_width = "32")]
        {
            fx32::finish_32(self.hash as u32)
        }
    }
}

const SEED1: u64 = 0x243f6a8885a308d3;
const SEED2: u64 = 0x13198a2e03707344;
const PREVENT_TRIVIAL_ZERO_COLLAPSE: u64 = 0xa4093822299f31d0;

/// Multiply-mix helper: folds a full u64*u64 product into 64 bits via XOR.
#[inline]
fn multiply_mix(x: u64, y: u64) -> u64 {
    #[cfg(target_pointer_width = "64")]
    {
        let full = (x as u128) * (y as u128);
        let b = full.to_le_bytes();
        let bs: &[u8] = &b;
        let lo = read_u64_le(bs);
        let hi = if bs.len() >= 16 {
            read_u64_le(&bs[8..16])
        } else {
            0
        };
        lo ^ hi
    }

    #[cfg(target_pointer_width = "32")]
    {
        fx32::multiply_mix_32(x, y)
    }
}

/// Reads a `u64` from the first 8 bytes of `b`, little-endian.
#[inline]
fn read_u64_le(b: &[u8]) -> u64 {
    if b.len() < 8 {
        return 0;
    }
    let lo = read_u32_le(&b[0..4]);
    let hi = read_u32_le(&b[4..8]);
    (lo as u64) | ((hi as u64) << 32)
}

/// Reads a `u32` from the first 4 bytes of `b`, little-endian.
#[inline]
const fn read_u32_le(b: &[u8]) -> u32 {
    if b.len() < 4 {
        return 0;
    }
    (b[0] as u32) | ((b[1] as u32) << 8) | ((b[2] as u32) << 16) | ((b[3] as u32) << 24)
}

/// A wyhash-inspired, non-collision-resistant hash for byte slices, tuned for
/// small strings and small code size. Out of line in builds with debug
/// assertions, so a caller at opt-level 0 calls this crate's optimised copy
/// rather than inlining its own (dev and test builds keep the crate at
/// opt-level 2).
#[cfg_attr(not(debug_assertions), inline)]
#[cfg_attr(debug_assertions, inline(never))]
fn hash_bytes(bytes: &[u8]) -> u64 {
    let len = bytes.len();
    let mut s0 = SEED1;
    let mut s1 = SEED2;

    if len <= 16 {
        if len >= 8 {
            s0 ^= read_u64_le(&bytes[0..8]);
            s1 ^= read_u64_le(&bytes[len - 8..]);
        } else if len >= 4 {
            s0 ^= read_u32_le(&bytes[0..4]) as u64;
            s1 ^= read_u32_le(&bytes[len - 4..]) as u64;
        } else {
            match *bytes {
                [] => {}
                [a] => {
                    s0 ^= a as u64;
                    s1 ^= ((a as u64) << 8) | (a as u64);
                }
                [a, b] => {
                    s0 ^= a as u64;
                    s1 ^= ((b as u64) << 8) | (b as u64);
                }
                [a, b, c] => {
                    s0 ^= a as u64;
                    s1 ^= ((c as u64) << 8) | (b as u64);
                }
                _ => {}
            }
        }
    } else {
        let tail_lo = read_u64_le(&bytes[len - 16..len - 8]);
        let tail_hi = read_u64_le(&bytes[len - 8..len]);

        let mut off = 0;
        while off < len - 16 {
            let x = read_u64_le(&bytes[off..off + 8]);
            let y = read_u64_le(&bytes[off + 8..off + 16]);
            let t = multiply_mix(s0 ^ x, PREVENT_TRIVIAL_ZERO_COLLAPSE ^ y);
            s0 = s1;
            s1 = t;
            off += 16;
        }

        s0 ^= tail_lo;
        s1 ^= tail_hi;
    }

    multiply_mix(s0, s1) ^ (len as u64)
}

/// [`FxHasher`] over `bytes` alone.
///
/// ```
/// use std::hash::Hasher;
/// use aterm_hash::{FxHasher, fx_hash_bytes};
/// let mut h = FxHasher::default();
/// h.write(b"the same bytes");
/// assert_eq!(fx_hash_bytes(b"the same bytes"), h.finish());
/// ```
#[inline(never)]
#[must_use]
pub fn fx_hash_bytes(bytes: &[u8]) -> u64 {
    let mut h = FxHasher::default();
    h.write(bytes);
    h.finish()
}

/// A zero-sized [`BuildHasher`] that produces [`FxHasher`]s.
///
/// ```
/// use std::hash::BuildHasher;
/// use aterm_hash::FxBuildHasher;
/// assert_ne!(FxBuildHasher.hash_one(1), FxBuildHasher.hash_one(2));
/// ```
#[derive(Copy, Clone, Default)]
pub struct FxBuildHasher;

impl BuildHasher for FxBuildHasher {
    type Hasher = FxHasher;

    #[inline]
    fn build_hasher(&self) -> FxHasher {
        FxHasher::default()
    }
}

#[cfg(test)]
mod tests {
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
}
