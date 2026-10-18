// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! XXH64, written from the xxHash specification (Yann Collet,
//! `doc/xxhash_spec.md`, "XXH64 Algorithm Description").
//!
//! A zstd frame's `Content_Checksum` is the low 32 bits of XXH64 over the
//! decompressed content with seed 0, stored little-endian (RFC 8878 §3.1.1).
//! The answers are the reference C code's: tests/xxh64_recorded.rs holds it
//! to 577 answers recorded from libzstd's own `ZSTD_XXH64` over xxhsum's
//! sanity buffer.

const P1: u64 = 0x9E37_79B1_85EB_CA87;
const P2: u64 = 0xC2B2_AE3D_27D4_EB4F;
const P3: u64 = 0x1656_67B1_9E37_79F9;
const P4: u64 = 0x85EB_CA77_C2B2_AE63;
const P5: u64 = 0x27D4_EB2F_1656_67C5;

/// Bytes per stripe: four 8-byte lanes, one per accumulator.
const STRIPE: usize = 32;

/// XXH64 of `bytes` under `seed` in one call.
#[must_use]
pub fn xxh64(seed: u64, bytes: &[u8]) -> u64 {
    let mut h = Xxh64::new(seed);
    h.update(bytes);
    h.digest()
}

/// Streaming XXH64: [`update`](Self::update) any number of times, in pieces
/// of any size, then [`digest`](Self::digest). The answer is the one-shot
/// [`xxh64`] of everything fed, however it was split.
#[derive(Clone, Debug)]
pub struct Xxh64 {
    seed: u64,
    acc: [u64; 4],
    /// The start of a stripe not yet consumed; `buf[..buffered]` is live.
    buf: [u8; STRIPE],
    buffered: usize,
    /// Every byte fed, modulo 2^64 (the reference's `total_len`).
    total: u64,
}

impl Xxh64 {
    /// A hasher with nothing fed.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self {
            seed,
            acc: [
                seed.wrapping_add(P1).wrapping_add(P2),
                seed.wrapping_add(P2),
                seed,
                seed.wrapping_sub(P1),
            ],
            buf: [0; STRIPE],
            buffered: 0,
            total: 0,
        }
    }

    /// Feeds `bytes`.
    pub fn update(&mut self, bytes: &[u8]) {
        self.total = self.total.wrapping_add(bytes.len() as u64);
        let mut rest = bytes;
        if self.buffered > 0 {
            let take = (STRIPE - self.buffered).min(rest.len());
            let (head, tail) = rest.split_at(take);
            self.buf[self.buffered..self.buffered + take].copy_from_slice(head);
            self.buffered += take;
            rest = tail;
            if self.buffered < STRIPE {
                return;
            }
            let stripe = self.buf;
            self.consume(&stripe);
            self.buffered = 0;
        }
        let (stripes, tail) = rest.as_chunks::<STRIPE>();
        for stripe in stripes {
            self.consume(stripe);
        }
        self.buf[..tail.len()].copy_from_slice(tail);
        self.buffered = tail.len();
    }

    /// The hash of everything fed so far. Feeding may continue after.
    #[must_use]
    pub fn digest(&self) -> u64 {
        let mut h = if self.total >= STRIPE as u64 {
            let [v1, v2, v3, v4] = self.acc;
            let mut h = v1
                .rotate_left(1)
                .wrapping_add(v2.rotate_left(7))
                .wrapping_add(v3.rotate_left(12))
                .wrapping_add(v4.rotate_left(18));
            for v in self.acc {
                h = merge(h, v);
            }
            h
        } else {
            self.seed.wrapping_add(P5)
        };
        h = h.wrapping_add(self.total);

        let (words, rest) = self.buf[..self.buffered].as_chunks::<8>();
        for word in words {
            h ^= round(0, u64::from_le_bytes(*word));
            h = h.rotate_left(27).wrapping_mul(P1).wrapping_add(P4);
        }
        // At most one 4-byte word remains (fewer than 8 bytes are left).
        let (quads, bytes) = rest.as_chunks::<4>();
        for quad in quads {
            h ^= u64::from(u32::from_le_bytes(*quad)).wrapping_mul(P1);
            h = h.rotate_left(23).wrapping_mul(P2).wrapping_add(P3);
        }
        for &byte in bytes {
            h ^= u64::from(byte).wrapping_mul(P5);
            h = h.rotate_left(11).wrapping_mul(P1);
        }

        h ^= h >> 33;
        h = h.wrapping_mul(P2);
        h ^= h >> 29;
        h = h.wrapping_mul(P3);
        h ^ (h >> 32)
    }

    fn consume(&mut self, stripe: &[u8; STRIPE]) {
        let (lanes, _) = stripe.as_chunks::<8>();
        for (acc, lane) in self.acc.iter_mut().zip(lanes) {
            *acc = round(*acc, u64::from_le_bytes(*lane));
        }
    }
}

fn round(acc: u64, lane: u64) -> u64 {
    acc.wrapping_add(lane.wrapping_mul(P2))
        .rotate_left(31)
        .wrapping_mul(P1)
}

fn merge(acc: u64, v: u64) -> u64 {
    (acc ^ round(0, v)).wrapping_mul(P1).wrapping_add(P4)
}

#[cfg(test)]
mod tests {
    use super::{STRIPE, Xxh64, xxh64};

    /// xxhsum's sanity buffer: byte i is the top byte of the generator after
    /// i steps (start 2654435761, multiply by 11400714785074694797 mod 2^64).
    fn sanity(len: usize) -> Vec<u8> {
        let mut g: u64 = 2_654_435_761;
        (0..len)
            .map(|_| {
                let byte = (g >> 56) as u8;
                g = g.wrapping_mul(11_400_714_785_074_694_797);
                byte
            })
            .collect()
    }

    /// Every way of cutting the input into two or three pieces lands on the
    /// one-shot answer, across the stripe boundary.
    #[test]
    fn split_feeding_matches_one_shot() {
        let buf = sanity(3 * STRIPE + 5);
        let want = xxh64(7, &buf);
        for a in 0..=buf.len() {
            for b in a..=buf.len().min(a + STRIPE + 1) {
                let mut h = Xxh64::new(7);
                h.update(&buf[..a]);
                h.update(&buf[a..b]);
                h.update(&buf[b..]);
                assert_eq!(h.digest(), want, "cut at {a} and {b}");
            }
        }
        let mut bytewise = Xxh64::new(7);
        for byte in &buf {
            bytewise.update(std::slice::from_ref(byte));
        }
        assert_eq!(bytewise.digest(), want);
    }

    /// `digest` reads the state without consuming it.
    #[test]
    fn digest_then_continue() {
        let buf = sanity(100);
        let mut h = Xxh64::new(0);
        h.update(&buf[..40]);
        assert_eq!(h.digest(), xxh64(0, &buf[..40]));
        h.update(&buf[40..]);
        assert_eq!(h.digest(), xxh64(0, &buf));
    }
}
