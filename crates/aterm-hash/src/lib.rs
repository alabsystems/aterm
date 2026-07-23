// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Fast non-cryptographic hashing for aterm.
//!
//! This crate provides an inline implementation of the FxHash algorithm,
//! replacing the external `rustc-hash` dependency. FxHash is a speedy,
//! non-cryptographic hash designed by Orson Peters for use in `rustc`.
//!
//! # Exported types
//!
//! - [`FxHasher`] -- the core hasher implementing [`std::hash::Hasher`]
//! - [`FxBuildHasher`] -- a zero-sized [`BuildHasher`] that creates `FxHasher`s
//! - [`FxHashMap`] -- type alias for `HashMap<K, V, FxBuildHasher>`
//! - [`FxHashSet`] -- type alias for `HashSet<V, FxBuildHasher>`
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

/// Type alias for a hash map using the Fx hashing algorithm.
pub type FxHashMap<K, V> = HashMap<K, V, FxBuildHasher>;

/// Type alias for a hash set using the Fx hashing algorithm.
pub type FxHashSet<V> = HashSet<V, FxBuildHasher>;

// Multiplicative constant chosen for good distribution properties.
// From "Computationally Easy, Spectrally Good Multipliers for Congruential
// Pseudorandom Number Generators" by Guy Steele and Sebastiano Vigna.
#[cfg(target_pointer_width = "64")]
const K: usize = 0xf1357aea2e62a9c5;
#[cfg(target_pointer_width = "32")]
const K: usize = 0x93d765dd;

/// A speedy, non-cryptographic hasher.
///
/// Uses a polynomial hash with a single bit rotation as a finishing step,
/// designed by Orson Peters. This is the same algorithm used by `rustc-hash`.
///
/// **Do not use for cryptographic purposes or untrusted input where HashDoS
/// resistance is required.**
#[derive(Clone)]
pub struct FxHasher {
    hash: usize,
}

impl FxHasher {
    /// Creates an `FxHasher` with a given seed.
    #[must_use]
    pub const fn with_seed(seed: usize) -> FxHasher {
        FxHasher { hash: seed }
    }

    #[inline]
    fn add_to_hash(&mut self, i: usize) {
        self.hash = self.hash.wrapping_add(i).wrapping_mul(K);
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
        self.add_to_hash(i as usize);
        #[cfg(target_pointer_width = "32")]
        self.add_to_hash((i >> 32) as usize);
    }

    #[inline]
    fn write_u128(&mut self, i: u128) {
        // Split `i` into usize-width words via its little-endian byte
        // representation. Bit-identical to the previous
        // `(i & usize::MAX as u128) as usize` / `(i >> 64) as usize` pair
        // (and the 32-bit `i >> 32/64/96` chain): word k of the LE bytes IS
        // those bits of `i`. Written this way because 128-bit shift
        // obligations get no native full-verifier evidence (u128 `>>` stayed
        // DESIGN-REQUIRED at any shift amount), leaving this function
        // INCONCLUSIVE under the strict gate.
        let b = i.to_le_bytes();
        // Unsizing coercion to a slice first: range-indexing an array goes
        // through `std::ops::Index::index`, which the native TrustIr lowering
        // does not model, while the identical range on a slice lowers fine.
        let bs: &[u8] = &b;
        // `read_u64_le(bs)`/`read_u32_le(bs)` read exactly the leading
        // 8/4 bytes, and the `bs.len() >= 16` guards are always true (`b` is
        // `[u8; 16]`); the guards re-establish the slice length locally for
        // the L0 solver, which does not carry the array length through the
        // unsizing coercion.
        #[cfg(target_pointer_width = "64")]
        {
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
            self.add_to_hash(read_u32_le(bs) as usize);
            if bs.len() >= 16 {
                self.add_to_hash(read_u32_le(&bs[4..8]) as usize);
                self.add_to_hash(read_u32_le(&bs[8..12]) as usize);
                self.add_to_hash(read_u32_le(&bs[12..16]) as usize);
            }
        }
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add_to_hash(i);
    }

    #[inline]
    fn finish(&self) -> u64 {
        // Rotate left to move high-entropy top bits down to the bottom,
        // where most hash table implementations compute bucket indices.
        #[cfg(target_pointer_width = "64")]
        const ROTATE: u32 = 26;
        #[cfg(target_pointer_width = "32")]
        const ROTATE: u32 = 15;

        self.hash.rotate_left(ROTATE) as u64
    }
}

// Constants for the byte-hashing helper (digits of pi).
const SEED1: u64 = 0x243f6a8885a308d3;
const SEED2: u64 = 0x13198a2e03707344;
const PREVENT_TRIVIAL_ZERO_COLLAPSE: u64 = 0xa4093822299f31d0;

/// Multiply-mix helper: folds a full u64*u64 product into 64 bits via XOR.
#[inline]
fn multiply_mix(x: u64, y: u64) -> u64 {
    #[cfg(target_pointer_width = "64")]
    {
        let full = (x as u128) * (y as u128);
        // Split the 128-bit product into its two u64 halves via the
        // little-endian byte representation: bit-identical to
        // `(full & u64::MAX as u128) as u64` and `(full >> 64) as u64`, but
        // avoids 128-bit shift obligations, which get no native
        // full-verifier evidence (see `write_u128`).
        let b = full.to_le_bytes();
        // Unsizing coercion to a slice first: range-indexing an array goes
        // through `std::ops::Index::index`, which the native TrustIr lowering
        // does not model, while the identical range on a slice lowers fine.
        let bs: &[u8] = &b;
        // `read_u64_le(bs)` reads exactly the first 8 bytes, so it equals
        // `read_u64_le(&bs[0..8])`. The `bs.len() >= 16` guard is always
        // true (`b` is `[u8; 16]`); it re-establishes the slice length
        // locally for the L0 solver, which does not carry the array length
        // through the unsizing coercion.
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
        let lx = x as u32;
        let ly = y as u32;
        let hx = (x >> 32) as u32;
        let hy = (y >> 32) as u32;

        let afull = (lx as u64) * (hy as u64);
        let bfull = (hx as u64) * (ly as u64);
        afull ^ bfull.rotate_right(32)
    }
}

/// Reads a `u64` from the first 8 bytes of `b`, little-endian.
///
/// Every caller passes a range-sliced subslice of length exactly 8, where
/// this is bit-identical to `u64::from_le_bytes(b.try_into().unwrap())`.
/// The guard covers the (unreachable at all call sites) short-slice case so
/// the function is provably panic-free; it never changes a real result.
///
/// Written with plain indexing, primitive casts, and constant shifts (no
/// `TryInto`/`from_le_bytes` machinery) so the Trust native full verifier
/// can lower it to typed TrustIr: `std::convert::TryInto::try_into` is an
/// unsupported call target in that lowering and was the sole cause of the
/// strict gate's `Result: FAIL` despite all L0 obligations proving.
#[inline]
fn read_u64_le(b: &[u8]) -> u64 {
    if b.len() < 8 {
        return 0;
    }
    // Composed from two little-endian u32 halves via `read_u32_le` so the
    // per-function obligation count stays within the native full verifier's
    // evidence coverage (a single-expression `b[0] | ... | b[7] << 56` fold
    // left the fold's tail hardened-Shl boundaries DESIGN-REQUIRED) and
    // every shift coefficient stays at or below 2^32. Bit-identical to that
    // fold, i.e. to `u64::from_le_bytes` on the same 8 bytes.
    let lo = read_u32_le(&b[0..4]);
    let hi = read_u32_le(&b[4..8]);
    (lo as u64) | ((hi as u64) << 32)
}

/// Reads a `u32` from the first 4 bytes of `b`, little-endian.
///
/// Same contract and rationale as [`read_u64_le`]: callers pass exactly 4
/// bytes, where this is bit-identical to
/// `u32::from_le_bytes(b.try_into().unwrap())`.
#[inline]
fn read_u32_le(b: &[u8]) -> u32 {
    if b.len() < 4 {
        return 0;
    }
    (b[0] as u32) | ((b[1] as u32) << 8) | ((b[2] as u32) << 16) | ((b[3] as u32) << 24)
}

/// A wyhash-inspired non-collision-resistant hash for byte slices, designed
/// by Orson Peters with a focus on small strings and small code size.
#[inline]
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
            // len is 0..=3 here. Slice patterns carry the length statically,
            // so there are no panic paths for the verifier to refute; each
            // arm is the original `lo = bytes[0]`, `mid = bytes[len / 2]`,
            // `hi = bytes[len - 1]` evaluated for that concrete length.
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
                // Unreachable: the enclosing branch established len < 4.
                _ => {}
            }
        }
    } else {
        // The final 16 bytes, read as two direct bounded ranges of `bytes`
        // (len > 16 here, so len - 16 >= 1). This is byte-identical to the
        // previous `suffix = &bytes[len - 16..]` +
        // `get(..).and_then(try_into).map_or(0, from_le_bytes)` chain, whose
        // `0` fallback was unreachable because `suffix` always had length 16.
        //
        // The reads are hoisted above the loop: `bytes` is immutable and the
        // reads are pure, so evaluation order cannot change behavior, and in
        // the pre-loop straight-line context the verifier lowers these bounds
        // checks to its supported form (post-loop they hit an unsupported-MIR
        // `FullVerification::BoundsCheck` path in the trust-vc adapter).
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

/// A [`BuildHasher`] that produces [`FxHasher`] instances.
///
/// This is a zero-sized type that can be used as the hasher parameter for
/// `HashMap` and `HashSet`.
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

    // -----------------------------------------------------------------------
    // Bit-exact compatibility with rustc-hash 2.1
    // -----------------------------------------------------------------------

    macro_rules! test_hash {
        ($($value:expr => $result:expr,)*) => {
            $(assert_eq!(
                FxBuildHasher.hash_one($value), $result,
                "hash mismatch for {:?}", $value
            );)*
        };
    }

    const B32: bool = cfg!(target_pointer_width = "32");

    #[test]
    fn test_fxhasher_unsigned_integers_match_rustc_hash() {
        test_hash! {
            0_u8 => 0,
            1_u8 => if B32 { 3001993707 } else { 12157901119326311915 },
            100_u8 => if B32 { 3844759569 } else { 16751747135202103309 },
            u8::MAX => if B32 { 999399879 } else { 1211781028898739645 },

            0_u16 => 0,
            1_u16 => if B32 { 3001993707 } else { 12157901119326311915 },
            100_u16 => if B32 { 3844759569 } else { 16751747135202103309 },
            u16::MAX => if B32 { 3440503042 } else { 16279819243059860173 },

            0_u32 => 0,
            1_u32 => if B32 { 3001993707 } else { 12157901119326311915 },
            100_u32 => if B32 { 3844759569 } else { 16751747135202103309 },
            u32::MAX => if B32 { 1293006356 } else { 7729994835221066939 },

            0_u64 => 0,
            1_u64 => if B32 { 275023839 } else { 12157901119326311915 },
            100_u64 => if B32 { 1732383522 } else { 16751747135202103309 },
            u64::MAX => if B32 { 1017982517 } else { 6288842954450348564 },

            0_u128 => 0,
            1_u128 => if B32 { 1860738631 } else { 13032756267696824044 },
            100_u128 => if B32 { 1389515751 } else { 12003541609544029302 },
            u128::MAX => if B32 { 2156022013 } else { 11702830760530184999 },

            0_usize => 0,
            1_usize => if B32 { 3001993707 } else { 12157901119326311915 },
            100_usize => if B32 { 3844759569 } else { 16751747135202103309 },
            usize::MAX => if B32 { 1293006356 } else { 6288842954450348564 },
        }
    }

    #[test]
    fn test_fxhasher_signed_integers_match_rustc_hash() {
        test_hash! {
            i8::MIN => if B32 { 2000713177 } else { 6684841074112525780 },
            0_i8 => 0,
            1_i8 => if B32 { 3001993707 } else { 12157901119326311915 },
            i8::MAX => if B32 { 3293686765 } else { 12973684028562874344 },

            i16::MIN => if B32 { 1073764727 } else { 14218860181193086044 },
            0_i16 => 0,
            i16::MAX => if B32 { 2366738315 } else { 2060959061933882993 },

            i32::MIN => if B32 { 16384 } else { 9943947977240134995 },
            0_i32 => 0,
            i32::MAX => if B32 { 1293022740 } else { 16232790931690483559 },

            i64::MIN => if B32 { 16384 } else { 33554432 },
            0_i64 => 0,
            i64::MAX => if B32 { 1017998901 } else { 6288842954483902996 },
        }
    }

    /// Helper to feed raw bytes to the hasher (avoids relying on std Hash impls).
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
            HashBytes(&[]) => if B32 { 2673204745 } else { 17606491139363777937 },
            HashBytes(&[0]) => if B32 { 2948228584 } else { 5448590020104574886 },
            HashBytes(&[0, 0, 0, 0, 0, 0]) => if B32 { 3223252423 } else { 16766921560080789783 },
            HashBytes(&[1]) => if B32 { 2943445104 } else { 5922447956811044110 },
            HashBytes(&[2]) => if B32 { 1055423297 } else { 5229781508510959783 },
            HashBytes(b"uwu") => if B32 { 2699662140 } else { 7168164714682931527 },
            HashBytes(b"These are some bytes for testing rustc_hash.") =>
                if B32 { 2303640537 } else { 2349210501944688211 },
        }
    }

    #[test]
    fn test_fxhasher_with_seed_produces_different_hashes() {
        let seeds = [
            [1, 2],
            [42, 17],
            [124436707, 99237],
            [usize::MIN, usize::MAX],
        ];

        for [a_seed, b_seed] in seeds {
            for x in u8::MIN..=u8::MAX {
                let mut a = FxHasher::with_seed(a_seed);
                let mut b = FxHasher::with_seed(b_seed);
                x.hash(&mut a);
                x.hash(&mut b);
                assert_ne!(a.finish(), b.finish());
            }
        }
    }

    #[test]
    fn test_fxhashmap_basic_operations() {
        let mut map: FxHashMap<u32, &str> = FxHashMap::default();
        map.insert(1, "one");
        map.insert(2, "two");
        map.insert(3, "three");
        assert_eq!(map.len(), 3);
        assert_eq!(map[&1], "one");
        assert_eq!(map.get(&99), None);
    }

    #[test]
    fn test_fxhashset_basic_operations() {
        let mut set: FxHashSet<u32> = FxHashSet::default();
        set.insert(1);
        set.insert(2);
        set.insert(1); // duplicate
        assert_eq!(set.len(), 2);
        assert!(set.contains(&1));
        assert!(!set.contains(&99));
    }

    #[test]
    fn test_fxbuildhasher_is_deterministic() {
        let h1 = FxBuildHasher.hash_one(42_u64);
        let h2 = FxBuildHasher.hash_one(42_u64);
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_fxbuildhasher_differentiates_values() {
        assert_ne!(FxBuildHasher.hash_one(1_u64), FxBuildHasher.hash_one(2_u64));
    }

    #[test]
    fn test_fxhasher_zero_returns_zero() {
        // Zero input through add_to_hash(0) should produce 0 * K = 0.
        assert_eq!(FxBuildHasher.hash_one(0_u32), 0);
    }

    #[test]
    fn test_fxhashmap_with_capacity_and_hasher() {
        // Verify the construction pattern used throughout the codebase.
        let map: FxHashMap<u32, u32> = FxHashMap::with_capacity_and_hasher(16, FxBuildHasher);
        assert_eq!(map.len(), 0);
        assert!(map.capacity() >= 16);
    }

    #[test]
    fn test_fxhasher_default_via_buildhasherdefault() {
        // Verify compatibility with std::hash::BuildHasherDefault<FxHasher>,
        // which is used in aterm-core for Kani-mode deterministic maps.
        use std::hash::BuildHasherDefault;
        let bh: BuildHasherDefault<FxHasher> = BuildHasherDefault::default();
        let h1 = bh.hash_one(42_u64);
        let h2 = FxBuildHasher.hash_one(42_u64);
        assert_eq!(h1, h2);
    }
}
