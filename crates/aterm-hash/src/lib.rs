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
#[path = "lib_tests.rs"]
mod tests;
