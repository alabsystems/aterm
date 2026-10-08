// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The generator behind every generated-input test: xorshift64 (shifts 13, 7,
//! 17) from a fixed seed, so a failing case reproduces from its seed and case
//! index. One copy, here, beside [`crate::wait`]; a test that draws inputs
//! draws them from [`XorShift`].

/// xorshift64 over a non-zero state.
#[derive(Clone, Debug)]
pub struct XorShift(u64);

impl XorShift {
    /// A generator seeded with `seed`. Zero is xorshift's one fixed point, so it
    /// is replaced by a fixed odd constant.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    /// The next 64 bits.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// A value in `0..n` (modulo bias is immaterial at test sizes).
    ///
    /// # Panics
    ///
    /// When `n` is zero.
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }

    /// An index in `0..n`.
    ///
    /// # Panics
    ///
    /// When `n` is zero.
    pub fn index(&mut self, n: usize) -> usize {
        let n = u64::try_from(n).unwrap_or(u64::MAX);
        usize::try_from(self.below(n)).unwrap_or(usize::MAX)
    }

    /// A value in `lo..hi`.
    ///
    /// # Panics
    ///
    /// When `hi <= lo`.
    pub fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.index(hi - lo)
    }

    /// One element of `from`.
    ///
    /// # Panics
    ///
    /// When `from` is empty.
    pub fn pick<'a, T>(&mut self, from: &'a [T]) -> &'a T {
        &from[self.index(from.len())]
    }
}

#[cfg(test)]
mod tests {
    use super::XorShift;

    #[test]
    fn the_sequence_is_xorshift64_and_fixed_by_its_seed() {
        // Marsaglia's xorshift64 (13, 7, 17) from 1: the published first value.
        let mut a = XorShift::new(1);
        assert_eq!(a.next_u64(), 1_082_269_761);
        let mut b = XorShift::new(1);
        b.next_u64();
        assert_eq!(a.next_u64(), b.next_u64());
        // Zero is no seed: it would stay zero for ever.
        assert_ne!(XorShift::new(0).next_u64(), 0);
    }

    #[test]
    fn bounded_draws_stay_in_bounds() {
        let mut r = XorShift::new(0xB0B);
        for n in 1..64_usize {
            assert!(r.index(n) < n);
            assert!((5..5 + n).contains(&r.range(5, 5 + n)));
        }
        let xs = [3, 1, 4];
        assert!(xs.contains(r.pick(&xs)));
    }
}
