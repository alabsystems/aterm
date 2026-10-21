// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! CRC-32/ISO-HDLC, the checksum in a gzip (RFC 1952) trailer.
//!
//! The fold is astream-wire's `hash::crc32_ieee_update`: the CRC astream
//! frames every record with, built as compile-time slicing-by-8 tables from
//! the reflected polynomial `0xEDB8_8320`, with every table index proved in
//! bounds by its `u8` type and the result pinned to the `0xCBF4_3926` check
//! value and a bitwise reference there. aterm used to carry two more copies (a
//! byte-at-a-time one here, a `OnceLock` slicing one in `aterm-png`); there is
//! now one CRC-32 across aterm and its bus.
//!
//! [`Crc32`] stays as a type because the gzip reader
//! (`inflate::stream::GzipReader`) holds one across `read` calls. astream's
//! running value is the FINISHED CRC of the bytes so far (it starts at `0`,
//! the CRC of nothing, and inverts in and out on each call), so the wrapper
//! holds that and [`Crc32::finish`] has nothing left to undo.

use astream_wire::hash::crc32_ieee_update;

/// A running CRC-32 over a byte stream fed in arbitrary pieces; every split
/// gives the same result as one update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crc32(u32);

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    /// A fresh state: the CRC of no bytes, `0`.
    #[must_use]
    pub const fn new() -> Self {
        Self(0)
    }

    /// Fold `data` into the running checksum.
    pub fn update(&mut self, data: &[u8]) {
        self.0 = crc32_ieee_update(self.0, data);
    }

    /// The finished checksum of everything folded so far.
    #[must_use]
    pub const fn finish(self) -> u32 {
        self.0
    }

    /// One-shot CRC-32 of `data`.
    #[must_use]
    pub const fn of(data: &[u8]) -> u32 {
        crc32_ieee_update(0, data)
    }
}

#[cfg(test)]
mod tests {
    use super::Crc32;

    /// The conformance check on the borrowed fold: the standard CRC-32/ISO-HDLC
    /// check value, the empty CRC, and gzip's own CRC for a known member
    /// (`printf hello | gzip -n` ends in the trailer `86 a6 10 36 05 00 00 00`:
    /// the CRC, little-endian, then ISIZE). Every value is also taken through
    /// the running value the gzip reader holds (`new`, `update`, `finish`), not
    /// only through [`Crc32::of`], which calls astream's fold directly: a
    /// wrapper that kept the old register convention (seed `0xFFFF_FFFF`,
    /// `finish` XORing it back out) passed this test when it checked `of` alone,
    /// and fails it now.
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

    /// The wrapper's state is the finished CRC, not the inverted register a
    /// table loop keeps: a state read mid-stream is the CRC of the bytes so
    /// far, and folding on from it finishes the whole. A wrapper that kept the
    /// old convention fails here, in `check_vector` and in
    /// `folding_is_associative_over_every_split`. Negative control on the
    /// assertions themselves: the same wrapper holding the register seed
    /// (`0xFFFF_FFFF`, what this type used to start from) finishes to another
    /// value, so the state's starting value is something they can see.
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
