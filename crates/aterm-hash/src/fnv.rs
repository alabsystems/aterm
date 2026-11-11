// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! FNV-1a/64, the workspace's one stable hash.

/// The FNV-1a/64 offset basis: the hash of no bytes.
pub const FNV64_OFFSET: u64 = astream_wire::hash::FNV1A_64_INIT;

/// The FNV-64 prime, 2^40 + 0x1b3.
pub const FNV64_PRIME: u64 = astream_wire::hash::FNV1A_64_PRIME;

/// FNV-1a/64 of `bytes`.
#[inline]
#[must_use]
pub const fn fnv1a_64(bytes: &[u8]) -> u64 {
    fnv1a_64_extend(FNV64_OFFSET, bytes)
}

/// The FNV-1a/64 state `hash` continued over `bytes`: astream-wire's byte loop.
#[inline]
#[must_use]
pub const fn fnv1a_64_extend(hash: u64, bytes: &[u8]) -> u64 {
    astream_wire::hash::fnv1a_64_update(hash, bytes)
}

/// One fold step, `(hash ^ word) * FNV64_PRIME` wrapping: astream-wire's step, on its
/// Clean-origin multiply.
#[inline]
#[must_use]
pub const fn fnv_mix_u64(hash: u64, word: u64) -> u64 {
    astream_wire::hash::fnv1a_64_mix_u64(hash, word)
}

/// The FNV-1a/128 offset basis: the 128-bit hash of no bytes.
pub const FNV128_OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;

/// The FNV-128 prime, 2^88 + 0x13b.
pub const FNV128_PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

/// FNV-1a/128 of `bytes`.
#[inline]
#[must_use]
pub const fn fnv1a_128(bytes: &[u8]) -> u128 {
    fnv1a_128_extend(FNV128_OFFSET, bytes)
}

/// The FNV-1a/128 state `hash` continued over `bytes`.
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

#[inline]
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

/// FNV-1a/64 over successive writes.
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
    pub const fn write(&mut self, bytes: &[u8]) {
        self.0 = fnv1a_64_extend(self.0, bytes);
    }

    /// Appends one byte.
    #[inline]
    pub const fn write_u8(&mut self, byte: u8) {
        self.0 = fnv_mix_u64(self.0, byte as u64);
    }

    /// Appends a `u16` in little-endian byte order.
    #[inline]
    pub const fn write_u16_le(&mut self, val: u16) {
        self.0 = fnv1a_64_u16_le(self.0, val);
    }

    /// Appends a `u32` in little-endian byte order.
    #[inline]
    pub const fn write_u32_le(&mut self, val: u32) {
        self.0 = fnv1a_64_u32_le(self.0, val);
    }

    /// Appends a `u64` in little-endian byte order.
    #[inline]
    pub const fn write_u64_le(&mut self, val: u64) {
        self.0 = fnv1a_64_u64_le(self.0, val);
    }

    /// Mixes a whole 64-bit word via [`fnv_mix_u64`].
    #[inline]
    pub const fn mix_word(&mut self, word: u64) {
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
#[path = "fnv_tests.rs"]
mod tests;
