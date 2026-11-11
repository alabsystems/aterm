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
#[path = "checksum_tests.rs"]
mod tests;
