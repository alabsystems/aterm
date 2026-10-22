// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Fast non-cryptographic hashing for aterm.
//!
//! - [`FxHasher`], [`FxBuildHasher`], [`FxHashMap`], [`FxHashSet`]: an inline
//!   FxHash, bit-compatible with `rustc-hash` 2.1. In-process only, and for
//!   keys aterm makes or trusts.
//! - [`KeyedHashMap`]: a map hashed under a per-process random key, for text
//!   keys an untrusted party chooses (see its docs for the rule).
//! - [`fnv1a_64`] and friends: FNV-1a/64, the one hash whose values may
//!   persist or cross a crate boundary (see the `fnv` module), and its
//!   128-bit sibling [`fnv1a_128`] for content addresses.
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
pub use fnv::{
    FNV64_OFFSET, FNV64_PRIME, FNV128_OFFSET, FNV128_PRIME, Fnv1a64, bucket_index, fnv_mix_u64,
    fnv1a_64, fnv1a_64_array8, fnv1a_64_extend, fnv1a_64_to_hex16, fnv1a_64_u16_le,
    fnv1a_64_u32_le, fnv1a_64_u64_le, fnv1a_128, fnv1a_128_extend,
};

/// Type alias for a hash map using the Fx hashing algorithm.
pub type FxHashMap<K, V> = HashMap<K, V, FxBuildHasher>;

/// Type alias for a hash set using the Fx hashing algorithm.
pub type FxHashSet<V> = HashSet<V, FxBuildHasher>;

/// A hash map whose hash carries a per-process secret: std's [`HashMap`] under
/// [`KeyedState`] (std's SipHash-1-3, keyed from the OS once per thread and
/// stepped for every map).
///
/// Use it for a map keyed by TEXT that an untrusted party chooses and that
/// holds many such keys at once: a render memo keyed by what a pane's program
/// printed. [`FxHashMap`] has no secret, so one precomputed key set lands in a
/// single bucket and control tag on every aterm, and each lookup then
/// string-compares the whole chain. The same holds for an integer key that is
/// an unkeyed hash of such text (a context fingerprint): mixing it well does
/// not stop a search for colliding inputs.
///
/// Through the real renderer, a 32x128
/// screen of such cluster keys took a frame from 1.9 ms to 16 ms. Seeding
/// [`FxHasher`]'s state would not have helped: its byte compression
/// (`hash_bytes`) takes no seed, so keys whose compressed words collide still
/// collide under any starting state. [`FxHashMap`] stays right for keys aterm
/// makes or trusts (glyph ids, line numbers, addresses, shipped or configured
/// data) and for key domains aterm fixes and an untrusted party only picks
/// from, such as a `char` or a word-decoration ident (a grid column and a
/// lexicon form). There few keys can share a probe class and each compare is
/// an integer compare, so a hot path need not pay for SipHash. On a target
/// with no OS entropy (wasm32-unknown-unknown), std derives the keys from
/// addresses, which is no weaker than Fx.
pub type KeyedHashMap<K, V> = HashMap<K, V, KeyedState>;

/// [`KeyedHashMap`]'s hasher builder: std's [`std::hash::RandomState`], its
/// SipHash compiled in THIS crate.
///
/// SipHash is generic code, so std's hasher is compiled into whichever crate
/// hashes with it, at that crate's opt-level: a debug `aterm-gui` (opt-level
/// 0) spent 59% of its Settings text measures hashing its measure memo's
/// keys. Under `debug_assertions` [`KeyedHasher`]'s two methods are kept out
/// of line, so the hashing is this crate's code, which a debug build compiles
/// optimized (the root `Cargo.toml`'s `[profile.dev.package.aterm-hash]`); a
/// release build inlines them as it did std's. The keys and the hash are
/// std's.
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

/// [`KeyedState`]'s hasher: std's keyed SipHash-1-3.
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

// Multiplicative constant chosen for good distribution properties.
// From "Computationally Easy, Spectrally Good Multipliers for Congruential
// Pseudorandom Number Generators" by Guy Steele and Sebastiano Vigna.
#[cfg(target_pointer_width = "64")]
const K: usize = 0xf1357aea2e62a9c5;
#[cfg(target_pointer_width = "32")]
const K: usize = 0x93d765dd;

/// A fast, non-cryptographic hasher (the `rustc-hash` FxHash algorithm): a
/// polynomial hash with a single bit rotation as a finishing step.
///
/// **Do not use for cryptographic purposes or untrusted input where HashDoS
/// resistance is required:** a map keyed by text an untrusted party chooses
/// is a [`KeyedHashMap`].
#[derive(Clone)]
pub struct FxHasher {
    hash: usize,
}

impl FxHasher {
    #[inline]
    const fn add_to_hash(&mut self, i: usize) {
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
        // Split `i` into usize-width words via its little-endian bytes (word k
        // of the bytes is those bits of `i`), avoiding 128-bit shifts, which
        // the strict verifier cannot discharge.
        let b = i.to_le_bytes();
        // Coerce to a slice first: the TrustIr lowering does not model array
        // range-indexing (`Index::index`), but does model slice ranges.
        let bs: &[u8] = &b;
        // The `bs.len() >= 16` guards always hold (`b` is `[u8; 16]`); they
        // restate the length for the L0 solver, which loses it through the
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
        // Split the 128-bit product into its u64 halves via the little-endian
        // bytes, avoiding 128-bit shifts (see `write_u128`).
        let b = full.to_le_bytes();
        // Coerce to a slice first: the TrustIr lowering does not model array
        // range-indexing (`Index::index`), but does model slice ranges.
        let bs: &[u8] = &b;
        // `read_u64_le(bs)` reads exactly the first 8 bytes. The
        // `bs.len() >= 16` guard always holds; it restates the length for the
        // L0 solver.
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
/// Callers pass exactly 8 bytes, where this equals
/// `u64::from_le_bytes(b.try_into().unwrap())`; the short-slice guard only
/// makes it provably panic-free. Plain indexing and constant shifts (no
/// `TryInto`) let the Trust full verifier lower it to TrustIr.
#[inline]
fn read_u64_le(b: &[u8]) -> u64 {
    if b.len() < 8 {
        return 0;
    }
    // Composed from two u32 halves so the function's obligations stay within
    // the full verifier's coverage and every shift coefficient is at most
    // 2^32. Equal to `u64::from_le_bytes` on the same 8 bytes.
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
const fn read_u32_le(b: &[u8]) -> u32 {
    if b.len() < 4 {
        return 0;
    }
    (b[0] as u32) | ((b[1] as u32) << 8) | ((b[2] as u32) << 16) | ((b[3] as u32) << 24)
}

/// A wyhash-inspired, non-collision-resistant hash for byte slices, tuned for
/// small strings and small code size.
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
            // so no panic paths remain; each arm computes `lo = bytes[0]`,
            // `mid = bytes[len / 2]`, `hi = bytes[len - 1]` for that length.
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
        // The final 16 bytes (len > 16 here), read as two bounded ranges.
        // Hoisted above the loop, where the verifier supports these bounds
        // checks; `bytes` is immutable, so the order does not matter.
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

/// [`FxHasher`] over `bytes` alone: `write(bytes)`, then `finish()`.
///
/// OUT OF LINE ON PURPOSE, for a caller hashing a large buffer at once (the
/// running executable's content signature, `aterm-gui`'s `build_info`). An
/// `#[inline]` hasher is compiled into its caller at the caller's opt-level, so
/// a debug `aterm` hashed its 245 MB self at opt-level 0: about 0.7 s of one
/// core on every instance's start, which every `aterm ctl` waits on through
/// `version` (the first answered 0.66 s after connect; ~0.04 s optimized).
/// Here the loop is this crate's code, which a debug build compiles optimized
/// (the root `Cargo.toml`'s `[profile.dev.package.aterm-hash]`).
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

    // Bit-exact compatibility with rustc-hash 2.1.

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
}
