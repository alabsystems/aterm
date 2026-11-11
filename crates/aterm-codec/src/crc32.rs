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
    pub const fn update(&mut self, data: &[u8]) {
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
#[path = "crc32_tests.rs"]
mod tests;
