// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CRC-32 (ISO-HDLC) and Adler-32 checksums for PNG chunks and zlib streams.

use astream_wire::hash::crc32_ieee_update;

/// Running CRC-32/ISO-HDLC checksum over `astream_wire::hash::crc32_ieee_update`.
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

/// Running Adler-32 checksum (RFC 1950 §9).
pub(crate) struct Adler32 {
    a: u32,
    b: u32,
}

const NMAX: usize = 5552;
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

    #[test]
    fn crc32_is_associative_over_splits() {
        let data: Vec<u8> = (0u16..1000).map(|b| (b % 251) as u8).collect();
        let whole = Crc32::of(&[&data]);
        for split in [0, 1, 7, 64, 499, 999, 1000] {
            let (head, tail) = data.split_at(split);
            assert_eq!(Crc32::of(&[head, tail]), whole, "split at {split}");
        }
    }

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

    #[test]
    fn adler32_check_vectors() {
        assert_eq!(adler32(b""), 1);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        let long: Vec<u8> = (0..20_000u32).map(|i| (i % 256) as u8).collect();
        let mut a: u64 = 1;
        let mut b: u64 = 0;
        for &byte in &long {
            a = (a + u64::from(byte)) % 65521;
            b = (b + a) % 65521;
        }
        assert_eq!(u64::from(adler32(&long)), (b << 16) | a);
    }

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
