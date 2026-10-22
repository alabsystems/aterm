// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bloom filter for fast negative trigram lookups: no false negatives, possible
//! false positives. K=7 FNV-1a-derived hashes over a `Vec<u64>` bit array; a lookup
//! checks at most K bits and an insert sets exactly K.
//!
//! ```rust,no_run
//! use aterm_search::BloomFilter;
//!
//! let mut bloom = BloomFilter::with_size(10_000);
//! bloom.insert_bytes(b"hel"); // SearchIndex inserts one trigram at a time
//! assert_eq!(bloom.item_count(), 1);
//! ```
//!
//! [`crate::SearchIndex::might_contain`] probes every trigram of a query.

use aterm_hash::{FNV64_OFFSET, FNV64_PRIME};

/// Number of hash functions (optimal for ~1% FPR at m/n ~= 10).
const K: usize = 7;

/// Bloom filter: false positives are possible, false negatives are not.
#[derive(Debug, Clone)]
pub struct BloomFilter {
    /// Bit array stored as u64 chunks.
    bits: Vec<u64>,
    /// Total number of bits (m).
    num_bits: usize,
    /// Number of items inserted.
    count: usize,
}

impl BloomFilter {
    #[inline]
    const fn requested_bits_for_capacity(expected_items: usize) -> usize {
        // Not `Ord::max`, which the Trust L0 gate treats as opaque.
        let bits = expected_items.saturating_mul(10);
        if bits < 64 { 64 } else { bits }
    }

    /// Smallest capacity that already saturates [`Self::MAX_BITS`], so clamping to it
    /// never changes the filter while bounding the allocation for the Trust L0 gate.
    pub(crate) const MAX_EFFECTIVE_CAPACITY: usize = Self::MAX_BITS / 10 + 1;

    /// Bit cap: keeps `num_u64s * 64` inside `usize` and the allocation within the
    /// Trust L0 gate's per-allocation budget. At 10 bits/item it serves >100M
    /// trigrams; past it the false-positive rate rises, never false negatives.
    const MAX_BITS: usize = if cfg!(target_pointer_width = "64") {
        // ~128 MiB = 1 Gi bits
        1 << 30
    } else {
        // 32-bit: ~32 MiB = 256 Mi bits
        1 << 28
    };

    /// Maximum number of backing `u64` words, restated so `storage_layout` can clamp
    /// with a comparison the Trust L0 gate sees.
    const MAX_U64S: usize = Self::MAX_BITS / 64;

    #[inline]
    const fn storage_layout(requested_bits: usize) -> (usize, usize) {
        // Explicit comparisons (not `max`/`min`/`div_ceil`) so the Trust L0 gate's
        // interval reasoning carries the bounds to the multiply and the allocation;
        // the division result is opaque to it.
        let floored_bits = if requested_bits < 64 {
            64
        } else {
            requested_bits
        };
        let capped_bits = if floored_bits > Self::MAX_BITS {
            Self::MAX_BITS
        } else {
            floored_bits
        };
        // div_ceil(64), spelled out; the saturating_add never saturates.
        let raw_u64s = if capped_bits % 64 == 0 {
            capped_bits / 64
        } else {
            (capped_bits / 64).saturating_add(1)
        };
        // Never fires (`capped_bits <= MAX_BITS`), but the gate needs the bound as
        // a comparison.
        let num_u64s = if raw_u64s > Self::MAX_U64S {
            Self::MAX_U64S
        } else {
            raw_u64s
        };
        // num_u64s <= MAX_U64S, so the product is at most MAX_BITS.
        (num_u64s, num_u64s * 64)
    }

    /// Create a filter sized for ~1% false positives at `expected_items`.
    #[must_use]
    pub fn with_capacity(expected_items: usize) -> Self {
        // Clamping to MAX_EFFECTIVE_CAPACITY never changes the filter; it gives the
        // Trust L0 gate a dominating bound on the allocation.
        let expected_items = if expected_items > Self::MAX_EFFECTIVE_CAPACITY {
            Self::MAX_EFFECTIVE_CAPACITY
        } else {
            expected_items
        };
        Self::with_size(Self::requested_bits_for_capacity(expected_items))
    }

    /// Create a new bloom filter with the specified number of bits.
    #[must_use]
    pub fn with_size(num_bits: usize) -> Self {
        let (num_u64s, final_num_bits) = Self::storage_layout(num_bits);
        // The first arm is dead (`storage_layout` caps `num_u64s`), but the Trust L0
        // gate's allocation recognizer needs a comparison directly dominating each
        // `vec!` count.
        let bits = if num_u64s > Self::MAX_U64S {
            vec![0u64; Self::MAX_U64S]
        } else {
            vec![0u64; num_u64s]
        };
        Self {
            bits,
            num_bits: final_num_bits,
            count: 0,
        }
    }

    /// Insert a string into the bloom filter.
    #[cfg(test)]
    pub fn insert(&mut self, s: &str) {
        let (h1, h2) = Self::hash_pair(s.as_bytes());

        for i in 0..K {
            let bit_idx = self.get_bit_index(h1, h2, i);
            self.set_bit(bit_idx);
        }
        // Saturating only to carry the no-overflow proof for the Trust L0 gate.
        self.count = self.count.saturating_add(1);
    }

    /// Insert bytes into the bloom filter.
    #[inline]
    pub fn insert_bytes(&mut self, bytes: &[u8]) {
        let (h1, h2) = Self::hash_pair(bytes);

        for i in 0..K {
            let bit_idx = self.get_bit_index(h1, h2, i);
            self.set_bit(bit_idx);
        }
        // Saturating only to carry the no-overflow proof for the Trust L0 gate.
        self.count = self.count.saturating_add(1);
    }

    /// Check if a string might be in the set.
    ///
    /// Returns `false` if definitely not present (no false negatives).
    /// Returns `true` if possibly present (may be false positive).
    #[must_use]
    #[cfg(test)]
    pub fn might_contain(&self, s: &str) -> bool {
        self.might_contain_bytes(s.as_bytes())
    }

    /// Check if bytes might be in the set.
    #[inline]
    #[must_use]
    pub(crate) fn might_contain_bytes(&self, bytes: &[u8]) -> bool {
        let (h1, h2) = Self::hash_pair(bytes);

        for i in 0..K {
            let bit_idx = self.get_bit_index(h1, h2, i);
            if !self.get_bit(bit_idx) {
                return false;
            }
        }
        true
    }

    /// Get the number of items inserted (test-only alias for `item_count`).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn count(&self) -> usize {
        self.count
    }

    /// Get the number of bits in the filter.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn num_bits(&self) -> usize {
        self.num_bits
    }

    /// Get the number of items inserted.
    #[inline]
    #[must_use]
    pub const fn item_count(&self) -> usize {
        self.count
    }

    /// Estimated false positive rate, (1 - e^(-k*n/m))^k: 0.0 when empty, near 1.0
    /// when saturated.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "FPR calculation doesn't need full precision"
    )]
    pub fn estimated_fpr(&self) -> f64 {
        if self.count == 0 {
            return 0.0;
        }
        let k = K as f64;
        let n = self.count as f64;
        let m = self.num_bits as f64;
        let exp = (-k * n / m).exp();
        let k_i32 = i32::try_from(K).unwrap_or(7);
        (1.0 - exp).powi(k_i32)
    }

    /// Returns `true` when the estimated false positive rate exceeds 50%: the filter
    /// then passes most queries and should be rebuilt or resized.
    #[inline]
    #[must_use]
    pub fn is_saturated(&self) -> bool {
        self.estimated_fpr() > 0.5
    }

    /// Clear all bits.
    pub fn clear(&mut self) {
        self.bits.fill(0);
        self.count = 0;
    }

    /// Two FNV-1a hashes in one pass, combined as h_i = h1 + i*h2
    /// (Kirsch-Mitzenmacher).
    #[inline]
    const fn hash_pair(bytes: &[u8]) -> (u64, u64) {
        // h1 is plain FNV-1a; h2 starts from a rotated seed and is rotated at the end.
        let mut h1 = FNV64_OFFSET;
        let mut h2 = FNV64_OFFSET.rotate_left(17); // Different seed

        let mut idx = 0usize;
        while idx < bytes.len() {
            let b64 = bytes[idx] as u64;
            h1 ^= b64;
            h1 = h1.wrapping_mul(FNV64_PRIME);
            h2 ^= b64;
            h2 = h2.wrapping_mul(FNV64_PRIME);
            idx += 1;
        }

        // Final mixing for h2 to ensure independence
        h2 = h2.rotate_left(31);

        (h1, h2)
    }

    /// Get the bit index for the i-th hash function.
    #[inline]
    const fn get_bit_index(&self, h1: u64, h2: u64, i: usize) -> usize {
        let combined = h1.wrapping_add((i as u64).wrapping_mul(h2));
        #[allow(
            clippy::cast_possible_truncation,
            reason = "result bounded by modulo num_bits; truncation harmless"
        )]
        let combined_usize = combined as usize;
        // The zero guard on the same local as the divisor discharges the Trust L0
        // gate's remzero obligation; `num_bits >= 64` by construction, so it never
        // fires.
        let num_bits = self.num_bits;
        if num_bits == 0 {
            return 0;
        }
        combined_usize % num_bits
    }

    /// Set a bit in the filter.
    #[inline]
    fn set_bit(&mut self, bit_idx: usize) {
        let word_idx = bit_idx / 64;
        let bit_pos = bit_idx % 64;
        self.bits[word_idx] |= 1u64 << bit_pos;
    }

    /// Get a bit from the filter.
    #[inline]
    fn get_bit(&self, bit_idx: usize) -> bool {
        let word_idx = bit_idx / 64;
        let bit_pos = bit_idx % 64;
        (self.bits[word_idx] & (1u64 << bit_pos)) != 0
    }
}

#[cfg(test)]
#[path = "bloom_tests.rs"]
mod tests;
