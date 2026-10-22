// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! CRC-32/ISO-HDLC, the checksum in a gzip (RFC 1952) trailer.
//!
//! The fold is `astream_wire::hash::crc32_ieee_update` (slicing-by-8 tables
//! from reflected polynomial `0xEDB8_8320`). Its running value is the finished
//! CRC of the bytes so far (starting at `0` and inverting in and out on each
//! call), so [`Crc32`] stores that directly and [`Crc32::finish`] returns it
//! unchanged.

use astream_wire::hash::crc32_ieee_update;

/// A running CRC-32 over a byte stream fed in arbitrary pieces; every split
/// gives the same result as one update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crc32(u32);

impl Default for Crc32 {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    /// A fresh state: the CRC of no bytes, `0`.
    #[must_use]
    #[inline]
    pub const fn new() -> Self {
        Self(0)
    }

    /// Fold `data` into the running checksum.
    #[inline]
    pub fn update(&mut self, data: &[u8]) {
        self.0 = crc32_ieee_update(self.0, data);
    }

    /// The finished checksum of everything folded so far.
    #[must_use]
    #[inline]
    pub const fn finish(self) -> u32 {
        self.0
    }

    /// One-shot CRC-32 of `data`.
    #[must_use]
    #[inline]
    pub const fn of(data: &[u8]) -> u32 {
        crc32_ieee_update(0, data)
    }
}

#[cfg(test)]
mod tests {
    use super::Crc32;

    /// Standard CRC-32/ISO-HDLC check value, empty input, and gzip trailer CRC
    /// (`printf hello | gzip -n` ends in `86 a6 10 36 05 00 00 00`: little-endian
    /// CRC then ISIZE), checked via both [`Crc32::of`] and streaming [`Crc32`].
    #[test]
    fn check_vector() {
        let streamed = |data: &[u8]| {
            let mut crc = Crc32::new();
            crc.update(data);
            crc.finish()
        };
        for (data, crc) in [
            (&b"123456789"[..], 0xCBF4_3926),
            (b"", 0),
            (b"hello", 0x3610_A686),
        ] {
            assert_eq!(Crc32::of(data), crc, "{data:02x?}");
            assert_eq!(streamed(data), crc, "{data:02x?}");
        }
        assert_eq!(Crc32::default().finish(), 0);
    }

    /// [`Crc32`] holds the finished CRC rather than an uninverted `0xFFFF_FFFF`-seeded
    /// register: reading mid-stream yields the CRC of the prefix, and continuing folds
    /// into the full-stream CRC.
    #[test]
    fn the_state_is_the_finished_crc_and_the_register_seed_is_wrong() {
        let mut crc = Crc32::new();
        crc.update(b"1234");
        assert_eq!(crc.finish(), Crc32::of(b"1234"));
        crc.update(b"56789");
        assert_eq!(crc.finish(), 0xCBF4_3926);
        let mut register_seeded = Crc32(0xFFFF_FFFF);
        register_seeded.update(b"123456789");
        assert_ne!(register_seeded.finish(), 0xCBF4_3926);
    }

    #[test]
    fn folding_is_associative_over_every_split() {
        let data: Vec<u8> = (0..1024u32)
            .map(|i| (i.wrapping_mul(31) & 0xFF) as u8)
            .collect();
        let whole = Crc32::of(&data);
        for split in 0..=data.len() {
            let mut crc = Crc32::new();
            crc.update(&data[..split]);
            crc.update(&data[split..]);
            assert_eq!(crc.finish(), whole, "split at {split}");
        }
    }
}
