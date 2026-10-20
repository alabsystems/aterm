// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! FNV-1a/64, the workspace's one stable hash.
//!
//! Unlike [`FxHasher`](crate::FxHasher) or std's `DefaultHasher`, its values
//! are fixed by the published algorithm, so they may persist and be compared
//! across processes, crates and releases (the `status` `hash=` stamp,
//! accessibility node ids, cache file names, pet-atlas host ids). It is never
//! a security primitive.
//!
//! - [`fnv1a_64`] hashes one byte slice, and [`fnv1a_64_extend`] continues a
//!   running hash, so extending by `a` and then `b` is the hash of `a ++ b`.
//! - [`Fnv1a64`] is that running hash as a value, for a stream of writes.
//! - [`fnv_mix_u64`] is the word fold: it XORs a whole `u64` into the state
//!   before the multiply. Over a byte zero-extended to `u64` it is one byte
//!   step of the hash; over wider words it is a different function, the one
//!   the scalar fingerprints fold.

/// The FNV-1a/64 offset basis: the hash of no bytes.
pub const FNV64_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// The FNV-64 prime, 2^40 + 0x1b3.
pub const FNV64_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a/64 of `bytes`.
#[inline]
#[must_use]
pub const fn fnv1a_64(bytes: &[u8]) -> u64 {
    fnv1a_64_extend(FNV64_OFFSET, bytes)
}

/// The FNV-1a/64 state `hash` continued over `bytes`.
#[inline]
#[must_use]
pub const fn fnv1a_64_extend(hash: u64, bytes: &[u8]) -> u64 {
    let mut h = hash;
    let mut i = 0;
    while i < bytes.len() {
        h = fnv_mix_u64(h, bytes[i] as u64);
        i += 1;
    }
    h
}

/// One fold step: `(hash ^ word) * FNV64_PRIME`, wrapping.
#[inline]
#[must_use]
pub const fn fnv_mix_u64(hash: u64, word: u64) -> u64 {
    (hash ^ word).wrapping_mul(FNV64_PRIME)
}

/// The FNV-1a/128 offset basis: the 128-bit hash of no bytes.
pub const FNV128_OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;

/// The FNV-128 prime, 2^88 + 0x13b.
pub const FNV128_PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

/// FNV-1a/128 of `bytes`: the 128-bit member of the same published family,
/// for content addresses whose collision space must outlast 64 bits (the
/// render engine's fx art ids, `aterm_core::render::FxArt`).
#[inline]
#[must_use]
pub const fn fnv1a_128(bytes: &[u8]) -> u128 {
    fnv1a_128_extend(FNV128_OFFSET, bytes)
}

/// The FNV-1a/128 state `hash` continued over `bytes`, so extending by `a`
/// and then `b` is the hash of `a ++ b`.
#[inline]
#[must_use]
pub const fn fnv1a_128_extend(hash: u128, bytes: &[u8]) -> u128 {
    let mut h = hash;
    let mut i = 0;
    while i < bytes.len() {
        h = (h ^ bytes[i] as u128).wrapping_mul(FNV128_PRIME);
        i += 1;
    }
    h
}

/// Feed a 16-bit integer in little-endian byte order into FNV-1a/64 state `hash`.
#[inline]
#[must_use]
pub const fn fnv1a_64_u16_le(hash: u64, val: u16) -> u64 {
    let h0 = fnv_mix_u64(hash, (val as u8) as u64);
    fnv_mix_u64(h0, ((val >> 8) as u8) as u64)
}

/// Feed a 32-bit integer in little-endian byte order into FNV-1a/64 state `hash`.
#[inline]
#[must_use]
pub const fn fnv1a_64_u32_le(hash: u64, val: u32) -> u64 {
    let h0 = fnv_mix_u64(hash, (val as u8) as u64);
    let h1 = fnv_mix_u64(h0, ((val >> 8) as u8) as u64);
    let h2 = fnv_mix_u64(h1, ((val >> 16) as u8) as u64);
    fnv_mix_u64(h2, ((val >> 24) as u8) as u64)
}

/// Feed a 64-bit integer in little-endian byte order into FNV-1a/64 state `hash`.
#[inline]
#[must_use]
pub const fn fnv1a_64_u64_le(hash: u64, val: u64) -> u64 {
    let h0 = fnv1a_64_u32_le(hash, val as u32);
    fnv1a_64_u32_le(h0, (val >> 32) as u32)
}

/// Feed a fixed 8-byte array into FNV-1a/64 state `hash`.
#[inline]
#[must_use]
pub const fn fnv1a_64_array8(hash: u64, bytes: [u8; 8]) -> u64 {
    let h0 = fnv_mix_u64(hash, bytes[0] as u64);
    let h1 = fnv_mix_u64(h0, bytes[1] as u64);
    let h2 = fnv_mix_u64(h1, bytes[2] as u64);
    let h3 = fnv_mix_u64(h2, bytes[3] as u64);
    let h4 = fnv_mix_u64(h3, bytes[4] as u64);
    let h5 = fnv_mix_u64(h4, bytes[5] as u64);
    let h6 = fnv_mix_u64(h5, bytes[6] as u64);
    fnv_mix_u64(h6, bytes[7] as u64)
}

const fn hex_nibble_ascii(n: u8) -> u8 {
    match n & 0x0f {
        0 => b'0',
        1 => b'1',
        2 => b'2',
        3 => b'3',
        4 => b'4',
        5 => b'5',
        6 => b'6',
        7 => b'7',
        8 => b'8',
        9 => b'9',
        10 => b'a',
        11 => b'b',
        12 => b'c',
        13 => b'd',
        14 => b'e',
        _ => b'f',
    }
}

/// Format a 64-bit hash into 16 lowercase ASCII hex bytes (`[u8; 16]`), big-endian.
#[inline]
#[must_use]
pub const fn fnv1a_64_to_hex16(hash: u64) -> [u8; 16] {
    [
        hex_nibble_ascii((hash >> 60) as u8),
        hex_nibble_ascii((hash >> 56) as u8),
        hex_nibble_ascii((hash >> 52) as u8),
        hex_nibble_ascii((hash >> 48) as u8),
        hex_nibble_ascii((hash >> 44) as u8),
        hex_nibble_ascii((hash >> 40) as u8),
        hex_nibble_ascii((hash >> 36) as u8),
        hex_nibble_ascii((hash >> 32) as u8),
        hex_nibble_ascii((hash >> 28) as u8),
        hex_nibble_ascii((hash >> 24) as u8),
        hex_nibble_ascii((hash >> 20) as u8),
        hex_nibble_ascii((hash >> 16) as u8),
        hex_nibble_ascii((hash >> 12) as u8),
        hex_nibble_ascii((hash >> 8) as u8),
        hex_nibble_ascii((hash >> 4) as u8),
        hex_nibble_ascii(hash as u8),
    ]
}

/// Map `hash` into `0..bucket_count` (`0` when `bucket_count == 0`).
#[inline]
#[must_use]
pub const fn bucket_index(hash: u64, bucket_count: usize) -> usize {
    if bucket_count == 0 {
        0
    } else {
        (hash as usize) % bucket_count
    }
}

/// FNV-1a/64 over successive writes: [`finish`](Self::finish) is the
/// [`fnv1a_64`] of everything written, concatenated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fnv1a64(u64);

impl Fnv1a64 {
    /// The hash of no bytes.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self(FNV64_OFFSET)
    }

    /// Resume a running FNV-1a/64 from a prior state `hash`.
    #[inline]
    #[must_use]
    pub const fn from_state(hash: u64) -> Self {
        Self(hash)
    }

    /// Whether the state is still at [`FNV64_OFFSET`].
    #[inline]
    #[must_use]
    pub const fn is_empty_basis(&self) -> bool {
        self.0 == FNV64_OFFSET
    }

    /// Appends `bytes`.
    #[inline]
    pub fn write(&mut self, bytes: &[u8]) {
        self.0 = fnv1a_64_extend(self.0, bytes);
    }

    /// Appends one byte.
    #[inline]
    pub fn write_u8(&mut self, byte: u8) {
        self.0 = fnv_mix_u64(self.0, byte as u64);
    }

    /// Appends a `u16` in little-endian byte order.
    #[inline]
    pub fn write_u16_le(&mut self, val: u16) {
        self.0 = fnv1a_64_u16_le(self.0, val);
    }

    /// Appends a `u32` in little-endian byte order.
    #[inline]
    pub fn write_u32_le(&mut self, val: u32) {
        self.0 = fnv1a_64_u32_le(self.0, val);
    }

    /// Appends a `u64` in little-endian byte order.
    #[inline]
    pub fn write_u64_le(&mut self, val: u64) {
        self.0 = fnv1a_64_u64_le(self.0, val);
    }

    /// Mixes a whole 64-bit word via [`fnv_mix_u64`].
    #[inline]
    pub fn mix_word(&mut self, word: u64) {
        self.0 = fnv_mix_u64(self.0, word);
    }

    /// The hash of everything written so far.
    #[inline]
    #[must_use]
    pub const fn finish(&self) -> u64 {
        self.0
    }
}

impl Default for Fnv1a64 {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FNV's published vectors; a wrong prime can agree in the low 40 bits only.
    #[test]
    fn matches_the_published_vectors() {
        assert_eq!(fnv1a_64(b""), FNV64_OFFSET);
        assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a_64(b"foobar"), 0x8594_4171_f739_67e8);
        assert_eq!(fnv1a_128(b""), FNV128_OFFSET);
        assert_eq!(fnv1a_128(b"a"), 0xd228_cb69_6f1a_8caf_7891_2b70_4e4a_8964);
        assert_eq!(
            fnv1a_128_extend(fnv1a_128(b"foo"), b"bar"),
            fnv1a_128(b"foobar")
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
}
