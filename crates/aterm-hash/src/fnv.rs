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

/// FNV-1a/64 over successive writes: [`finish`](Self::finish) is the
/// [`fnv1a_64`] of everything written, concatenated.
#[derive(Clone, Copy, Debug)]
pub struct Fnv1a64(u64);

impl Fnv1a64 {
    /// The hash of no bytes.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self(FNV64_OFFSET)
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
    }

    #[test]
    fn a_stream_of_writes_is_the_hash_of_their_concatenation() {
        let mut h = Fnv1a64::new();
        h.write(b"foo");
        h.write_u8(b'b');
        h.write(b"ar");
        assert_eq!(h.finish(), fnv1a_64(b"foobar"));
        assert_eq!(
            fnv1a_64_extend(fnv1a_64(b"foo"), b"bar"),
            fnv1a_64(b"foobar")
        );
        assert_eq!(fnv_mix_u64(FNV64_OFFSET, u64::from(b'a')), fnv1a_64(b"a"));
    }
}
