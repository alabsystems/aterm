// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! CRC-32/ISO-HDLC — the checksum RFC 1952 puts in a gzip trailer.
//!
//! Written as a generator, not a transcription: the 256-entry table is derived
//! from the reflected polynomial `0xEDB8_8320` in a `const fn`, so the
//! specification is the only thing in the file that could be wrong, and it is
//! materialised at compile time rather than behind a `OnceLock` probe on every
//! call.
//!
//! `aterm-png` carries its own slicing-by-8 CRC-32 for PNG chunk checks; that
//! one is tuned for many small chunks and is verified by that crate's own
//! oracle. This one folds one gzip member — a single long run — and stays
//! byte-at-a-time, which is not the bottleneck beside inflate.

/// Reflected CRC-32 table, generated from the polynomial at compile time.
const TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut n = 0usize;
    while n < 256 {
        // Exact: `n < 256`.
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
};

/// A running CRC-32 over a byte stream fed in arbitrary pieces.
///
/// `Crc32::new().update(a).update(b).finish()` equals the CRC of `a` followed by
/// `b` for every split — the property a streaming decoder depends on, since it
/// folds each chunk as it hands it to the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crc32(u32);

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    /// A fresh state (the specified `0xFFFF_FFFF` seed).
    #[must_use]
    pub const fn new() -> Self {
        Self(0xFFFF_FFFF)
    }

    /// Fold `data` into the running checksum.
    pub fn update(&mut self, data: &[u8]) {
        let mut crc = self.0;
        for &byte in data {
            // `(crc ^ byte) & 0xFF < 256`, so the index is in range; `get` keeps
            // that bound local to the verifier and the `None` arm is dead.
            let idx = ((crc ^ u32::from(byte)) & 0xFF) as usize;
            if let Some(&entry) = TABLE.get(idx) {
                crc = entry ^ (crc >> 8);
            }
        }
        self.0 = crc;
    }

    /// The finished checksum (the seed's complement applied).
    #[must_use]
    pub const fn finish(self) -> u32 {
        self.0 ^ 0xFFFF_FFFF
    }

    /// One-shot CRC-32 of `data`.
    #[must_use]
    pub fn of(data: &[u8]) -> u32 {
        let mut crc = Self::new();
        crc.update(data);
        crc.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::Crc32;

    #[test]
    fn check_vector() {
        // The standard CRC-32/ISO-HDLC check value for "123456789".
        assert_eq!(Crc32::of(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn empty_is_zero() {
        assert_eq!(Crc32::of(b""), 0);
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
