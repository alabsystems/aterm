// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The two checksums PNG carries: CRC-32 (ISO-HDLC) over every chunk, and
//! Adler-32 over the zlib stream the IDAT chunks concatenate into. A chunk
//! whose CRC does not match is corruption a decoder must not act on.

use astream_wire::hash::crc32_ieee_update;

/// CRC-32/ISO-HDLC (the polynomial PNG specifies, reflected form `0xEDB8_8320`)
/// as a running value over astream-wire's `hash::crc32_ieee_update`.
///
/// astream-wire builds the eight slicing-by-8 tables at compile time from the
/// polynomial (no first-use init, no probe per chunk), with every table index
/// proved in bounds by its `u8` type and the result pinned to the `0xCBF4_3926`
/// check value and a bitwise reference there. It is the CRC astream frames
/// every record with and `aterm_codec::crc32` wraps for gzip.
///
/// astream's running value is the finished CRC of the bytes so far: it starts
/// at `0` (the CRC of nothing) and inverts in and out on every call, so there
/// is no seed to apply here and none to strip in [`Crc32::finish`].
pub(crate) struct Crc32(u32);

impl Crc32 {
    /// A fresh CRC: the CRC of no bytes, `0`.
    #[inline]
    pub(crate) const fn new() -> Self {
        Self(0)
    }

    /// Fold `bytes` into the running CRC (astream-wire's slicing-by-8 fold).
    #[inline]
    pub(crate) const fn update(&mut self, bytes: &[u8]) {
        self.0 = crc32_ieee_update(self.0, bytes);
    }

    /// The finished CRC of everything folded so far.
    #[inline]
    pub(crate) const fn finish(self) -> u32 {
        self.0
    }

    /// CRC of `parts` folded in order.
    #[inline]
    pub(crate) fn of(parts: &[&[u8]]) -> u32 {
        let mut crc = Self::new();
        for part in parts {
            crc.update(part);
        }
        crc.finish()
    }
}

/// Adler-32 (RFC 1950 §9), the zlib stream's trailing checksum, folded as the
/// stream's input arrives.
///
/// `65521` is the largest prime below 2^16, and the running sums are reduced
/// every `NMAX` bytes — the largest run for which the unreduced `b` accumulator
/// provably cannot overflow `u32`. Both sums are reduced at the end of every
/// [`Adler32::update`], so a split anywhere folds to the same value.
pub(crate) struct Adler32 {
    a: u32,
    b: u32,
}

/// The RFC's own bound on how far the sums may run before reduction.
const NMAX: usize = 5552;
/// The Adler-32 modulus.
const BASE: u32 = 65521;

impl Adler32 {
    /// The checksum of no bytes (`1`, not `0`).
    #[inline]
    pub(crate) const fn new() -> Self {
        Self { a: 1, b: 0 }
    }

    /// Fold `data` into the running sums.
    #[inline]
    pub(crate) fn update(&mut self, data: &[u8]) {
        let (mut a, mut b) = (self.a, self.b);
        for chunk in data.chunks(NMAX) {
            for &byte in chunk {
                a += u32::from(byte);
                b += a;
            }
            a %= BASE;
            b %= BASE;
        }
        (self.a, self.b) = (a, b);
    }

    /// The checksum of every byte folded so far.
    #[inline]
    pub(crate) const fn finish(&self) -> u32 {
        (self.b << 16) | self.a
    }
}

/// Adler-32 of `data` in one fold.
#[cfg(test)]
pub(crate) fn adler32(data: &[u8]) -> u32 {
    let mut sum = Adler32::new();
    sum.update(data);
    sum.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Folding in pieces must equal folding in one go — the property the chunk
    /// writer relies on when it CRCs a type tag and a body separately.
    #[test]
    fn crc32_is_associative_over_splits() {
        let data: Vec<u8> = (0u16..1000).map(|b| (b % 251) as u8).collect();
        let whole = Crc32::of(&[&data]);
        for split in [0, 1, 7, 64, 499, 999, 1000] {
            let (head, tail) = data.split_at(split);
            assert_eq!(Crc32::of(&[head, tail]), whole, "split at {split}");
        }
    }

    /// astream-wire's slice-by-8 against the bitwise definition at every
    /// length 0..=64, so at every tail length mod 8, through this wrapper. The
    /// reference shares nothing with the tables.
    #[test]
    fn slicing_agrees_with_the_byte_at_a_time_definition_at_every_alignment() {
        fn reference(bytes: &[u8]) -> u32 {
            let mut c: u32 = 0xFFFF_FFFF;
            for &b in bytes {
                c ^= u32::from(b);
                for _ in 0..8 {
                    c = if c & 1 != 0 {
                        0xEDB8_8320 ^ (c >> 1)
                    } else {
                        c >> 1
                    };
                }
            }
            c ^ 0xFFFF_FFFF
        }

        let data: Vec<u8> = (0u16..=64)
            .map(|b| (b.wrapping_mul(37) % 251) as u8)
            .collect();
        for len in 0..=64usize {
            assert_eq!(
                Crc32::of(&[&data[..len]]),
                reference(&data[..len]),
                "length {len} (tail of {} after the sliced rounds)",
                len % 8,
            );
        }
    }

    /// The conformance check on the borrowed fold: the CRC-32 check value, the
    /// empty CRC, and the CRC every PNG ends with (IEND's, over its type tag
    /// alone: `AE 42 60 82`). `of` folds through `new`, `update` and `finish`,
    /// so a wrapper that kept the old register convention (seed `0xFFFF_FFFF`,
    /// `finish` XORing it back out) fails these. Negative control on the
    /// assertions themselves: the same wrapper holding the register seed
    /// finishes to another CRC, so the starting state is something they see.
    #[test]
    fn crc32_known_answers_and_the_seed_convention() {
        assert_eq!(Crc32::of(&[b"123456789"]), 0xCBF4_3926);
        assert_eq!(Crc32::of(&[]), 0);
        assert_eq!(Crc32::of(&[b"IEND"]), 0xAE42_6082);
        assert_eq!(Crc32::of(&[b"IE", b"", b"ND"]), 0xAE42_6082);
        let mut register_seeded = Crc32(0xFFFF_FFFF);
        register_seeded.update(b"IEND");
        assert_ne!(register_seeded.finish(), 0xAE42_6082);
    }

    /// RFC 1950's own example plus the empty case (`1`, not `0`).
    #[test]
    fn adler32_check_vectors() {
        assert_eq!(adler32(b""), 1);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        // Long enough to cross the NMAX reduction boundary more than once.
        let long: Vec<u8> = (0..20_000u32).map(|i| (i % 256) as u8).collect();
        // Reference value computed by the straightforward per-byte definition.
        let mut a: u64 = 1;
        let mut b: u64 = 0;
        for &byte in &long {
            a = (a + u64::from(byte)) % 65521;
            b = (b + a) % 65521;
        }
        assert_eq!(u64::from(adler32(&long)), (b << 16) | a);
    }

    /// Folding in pieces equals folding in one go, at splits on both sides of
    /// the NMAX reduction boundary — the property the streaming compressor
    /// relies on when it folds one scanline at a time.
    #[test]
    fn adler32_is_associative_over_splits() {
        let data: Vec<u8> = (0..20_000u32).map(|i| (i * 7 % 256) as u8).collect();
        let whole = adler32(&data);
        for split in [0, 1, NMAX - 1, NMAX, NMAX + 1, 9_999, 20_000] {
            let mut sum = Adler32::new();
            sum.update(&data[..split]);
            sum.update(&data[split..]);
            assert_eq!(sum.finish(), whole, "split at {split}");
        }
        let mut bytewise = Adler32::new();
        for byte in &data {
            bytewise.update(std::slice::from_ref(byte));
        }
        assert_eq!(bytewise.finish(), whole, "one byte at a time");
    }
}
