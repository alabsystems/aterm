// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The two checksums PNG carries: CRC-32 (ISO-HDLC) over every chunk, and
//! Adler-32 over the zlib stream the IDAT chunks concatenate into.
//!
//! Both are table-free-to-describe and small enough that a dependency for them
//! would be absurd; both are also load-bearing, because a chunk whose CRC does
//! not match is corruption a decoder must not act on.

/// CRC-32/ISO-HDLC (the polynomial PNG specifies, reflected form `0xEDB8_8320`).
///
/// The table is built on first use rather than written out as 256 literals: the
/// generator is the specification, a literal table is a transcription of it, and
/// a transcription is the thing that can be wrong.
pub(crate) struct Crc32(u32);

/// How many bytes one round of [`Crc32::update`] folds at a time, and so how
/// many tables it needs. Eight is 8 KiB of tables against a 128 KiB L1.
const SLICE: usize = 8;

/// Reflected CRC-32 tables, filled from the polynomial once per process.
///
/// `T[0]` is the classic byte-at-a-time table, and it is still the entire
/// definition: the other seven are DERIVED from it, each folding one more zero
/// byte through the first — `T[k][i] = (T[k-1][i] >> 8) ^ T[0][T[k-1][i] & 0xFF]`.
/// That keeps the property the module opens with. The polynomial is written
/// once, the generator is the specification, and nothing here is a
/// transcription that could be silently wrong.
fn tables() -> &'static [[u32; 256]; SLICE] {
    use std::sync::OnceLock;
    static TABLES: OnceLock<[[u32; 256]; SLICE]> = OnceLock::new();
    TABLES.get_or_init(|| {
        let mut tables = [[0u32; 256]; SLICE];
        let mut n = 0usize;
        while n < 256 {
            // `as u32` is exact: n < 256.
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
            tables[0][n] = c;
            n += 1;
        }
        let mut k = 1;
        while k < SLICE {
            let mut n = 0usize;
            while n < 256 {
                let prev = tables[k - 1][n];
                // `as usize` is exact: masked to 8 bits.
                tables[k][n] = (prev >> 8) ^ tables[0][(prev & 0xFF) as usize];
                n += 1;
            }
            k += 1;
        }
        tables
    })
}

impl Crc32 {
    /// A fresh CRC state (the specified `0xFFFF_FFFF` seed).
    pub(crate) fn new() -> Self {
        Self(0xFFFF_FFFF)
    }

    /// Fold `bytes` into the running CRC, eight at a time.
    ///
    /// The byte-at-a-time loop this replaces was not slow because a table
    /// lookup is slow. It was slow because every iteration's lookup ADDRESS
    /// depends on the previous iteration's result, so the whole scan is one
    /// serial chain of load-use latency, one byte deep, and the machine's
    /// load ports sit idle waiting on it.
    ///
    /// Slice-by-8 breaks the chain. Eight bytes are consumed per round and all
    /// eight lookups are independent — four indices come out of the running CRC
    /// after the leading four bytes are folded in, four come straight from the
    /// input — so the loads issue together and only their XOR reduction is
    /// ordered. Same polynomial, same answer, byte for byte; `tests/oracle.rs`
    /// proves it against the `png` crate over the whole corpus.
    pub(crate) fn update(&mut self, bytes: &[u8]) {
        let tables = tables();
        let mut c = self.0;
        let mut rest = bytes;
        while let Some((head, tail)) = rest.split_first_chunk::<SLICE>() {
            c ^= u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
            // `as usize` is exact everywhere here: each index is masked to 8
            // bits, or is a `u8` widened.
            c = tables[7][(c & 0xFF) as usize]
                ^ tables[6][((c >> 8) & 0xFF) as usize]
                ^ tables[5][((c >> 16) & 0xFF) as usize]
                ^ tables[4][(c >> 24) as usize]
                ^ tables[3][usize::from(head[4])]
                ^ tables[2][usize::from(head[5])]
                ^ tables[1][usize::from(head[6])]
                ^ tables[0][usize::from(head[7])];
            rest = tail;
        }
        // The tail, and every input shorter than a round, take the original
        // one-byte step — which is `T[0]` alone, and so is also the check that
        // the derived tables agree with the table they were derived from.
        for &b in rest {
            c = tables[0][usize::from((c as u8) ^ b)] ^ (c >> 8);
        }
        self.0 = c;
    }

    /// The finished CRC (the seed is inverted out again).
    pub(crate) fn finish(self) -> u32 {
        self.0 ^ 0xFFFF_FFFF
    }

    /// CRC of one contiguous run — the whole of what a chunk needs.
    pub(crate) fn of(parts: &[&[u8]]) -> u32 {
        let mut crc = Self::new();
        for part in parts {
            crc.update(part);
        }
        crc.finish()
    }
}

/// Adler-32 (RFC 1950 §9), the zlib stream's trailing checksum.
///
/// `65521` is the largest prime below 2^16, and the running sums are reduced
/// every `NMAX` bytes — the largest run for which the unreduced `b` accumulator
/// provably cannot overflow `u32`.
pub(crate) fn adler32(data: &[u8]) -> u32 {
    /// The RFC's own bound on how far the sums may run before reduction.
    const NMAX: usize = 5552;
    const BASE: u32 = 65521;
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for chunk in data.chunks(NMAX) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= BASE;
        b %= BASE;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CRC-32 check value every implementation of this polynomial agrees on.
    #[test]
    fn crc32_check_vector() {
        assert_eq!(Crc32::of(&[b"123456789"]), 0xCBF4_3926);
    }

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

    /// Slice-by-8 against the definition it replaces, at EVERY length from 0 to
    /// 64 and so at every residue mod 8.
    ///
    /// This is the test the change needs and the old suite did not have. The
    /// split points above are `[0, 1, 7, 64, 499, 999, 1000]`, which never
    /// leaves a tail of 2, 3, 4, 5 or 6 bytes — and the tail length is exactly
    /// the variable a sliced implementation gets wrong. The reference here is
    /// the one-byte step written out longhand from the polynomial, so it shares
    /// nothing with the derived tables it is checking.
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
                len % SLICE,
            );
        }
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
}
