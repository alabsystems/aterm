// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Bloom filter for fast negative lookups.
//!
//! A bloom filter provides O(1) probabilistic set membership tests.
//! It has no false negatives but may have false positives.
//!
//! ## Design
//!
//! - Uses k=7 hash functions (optimal for 1% false positive rate)
//! - Bit array stored as `Vec<u64>` for cache efficiency
//! - FNV-1a based hash functions for speed
//!
//! ## Complexity
//!
//! - Lookup: O(1) with respect to filter size - checks at most K=7 bits (early return on miss)
//! - Insert: O(1) with respect to filter size - always sets exactly K=7 bits
//!
//! ## Usage
//!
//! ```rust,no_run
//! use aterm_search::BloomFilter;
//!
//! let mut bloom = BloomFilter::with_size(10_000);
//! bloom.insert("hello");
//! assert!(bloom.might_contain("hello")); // Definitely true
//! // might_contain("xyz") could return true (false positive)
//! // but if it returns false, "xyz" is definitely not present
//! ```

/// Number of hash functions (k=7 is optimal for 1% FPR with m/n ~= 10)
const K: usize = 7;

// Counters for bloom filter bit operations (O(1) verification).
#[cfg(test)]
thread_local! {
    static BLOOM_GET_BIT_OPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static BLOOM_SET_BIT_OPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Increment the get_bit operation counter.
#[cfg(test)]
fn count_get_bit_op() {
    BLOOM_GET_BIT_OPS.with(|c| c.set(c.get() + 1));
}

/// Increment the set_bit operation counter.
#[cfg(test)]
fn count_set_bit_op() {
    BLOOM_SET_BIT_OPS.with(|c| c.set(c.get() + 1));
}

/// Take (read and reset) the get_bit operation count.
#[cfg(test)]
fn take_get_bit_ops() -> usize {
    BLOOM_GET_BIT_OPS.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}

/// Take (read and reset) the set_bit operation count.
#[cfg(test)]
fn take_set_bit_ops() -> usize {
    BLOOM_SET_BIT_OPS.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}

/// FNV-1a prime
const FNV_PRIME: u64 = 0x0000_0100_0000_01B3;

/// FNV-1a offset basis
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// Bloom filter for fast set membership testing.
///
/// False positives are possible, false negatives are not.
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
    fn requested_bits_for_capacity(expected_items: usize) -> usize {
        // Explicit comparison instead of `Ord::max` (which the Trust L0 gate
        // treats as opaque) so bounds on `expected_items` survive to the
        // storage sizing. Identical value on every input.
        let bits = expected_items.saturating_mul(10);
        if bits < 64 { 64 } else { bits }
    }

    /// Smallest capacity that already saturates the [`Self::MAX_BITS`] cap.
    ///
    /// For any `expected_items >= MAX_EFFECTIVE_CAPACITY`,
    /// `requested_bits_for_capacity` yields >= `MAX_BITS`, so `storage_layout`
    /// returns the identical maximal layout. Clamping a capacity to this
    /// value therefore NEVER changes the constructed filter. It also sits
    /// below the Trust L0 gate's per-allocation budget, so clamped counts are
    /// provably bounded at the allocation sites.
    pub(crate) const MAX_EFFECTIVE_CAPACITY: usize = Self::MAX_BITS / 10 + 1;

    /// Maximum number of bits we allow before capping.
    ///
    /// Chosen so that `num_u64s * 64` cannot overflow `usize` and the
    /// resulting `Vec<u64>` stays within a reasonable allocation size
    /// (~128 MiB on 64-bit, ~32 MiB on 32-bit). The 64-bit cap also keeps the
    /// backing store below the Trust L0 gate's per-allocation budget so the
    /// filter allocation is provably bounded. At 10 bits/item this still
    /// serves >100M trigrams (~1000x the documented ~1M-line scrollback
    /// bound); beyond the cap the filter degrades gracefully to a higher
    /// false-positive rate, never to false negatives.
    const MAX_BITS: usize = if cfg!(target_pointer_width = "64") {
        // ~128 MiB = 1 Gi bits
        1 << 30
    } else {
        // 32-bit: ~32 MiB = 256 Mi bits
        1 << 28
    };

    /// Maximum number of backing `u64` words (`MAX_BITS / 64`).
    ///
    /// Restated as its own constant so `storage_layout` can clamp `num_u64s`
    /// with a direct comparison: the Trust L0 gate's integer engines treat
    /// runtime division results as opaque, so the bound implied by
    /// `capped_bits / 64` is not otherwise visible to the
    /// `num_u64s * 64` overflow proof or the allocation-bound proof.
    const MAX_U64S: usize = Self::MAX_BITS / 64;

    #[inline]
    fn storage_layout(requested_bits: usize) -> (usize, usize) {
        // Spelled as explicit comparisons (not `Ord::max`/`Ord::min`/`div_ceil`
        // calls) so the Trust L0 gate's interval reasoning can carry the bounds
        // through to the multiplication and the backing-store allocation.
        // Identical values on every input.
        let floored_bits = if requested_bits < 64 {
            64
        } else {
            requested_bits
        };
        // Cap to prevent overflow in the `num_u64s * 64` multiplication
        // and to avoid unreasonably large allocations.
        let capped_bits = if floored_bits > Self::MAX_BITS {
            Self::MAX_BITS
        } else {
            floored_bits
        };
        // div_ceil(64), spelled out for the same reason. The saturating_add
        // is dead code (capped_bits / 64 < MAX_U64S in the remainder branch);
        // it carries the no-overflow proof past the opaque division result.
        let raw_u64s = if capped_bits % 64 == 0 {
            capped_bits / 64
        } else {
            (capped_bits / 64).saturating_add(1)
        };
        // Dominating clamp: `capped_bits <= MAX_BITS` already guarantees
        // `raw_u64s <= MAX_U64S` mathematically (so the clamp never fires on
        // ANY input — identical values always), but the gate treats the
        // division as opaque, so the bound must be restated as a comparison.
        let num_u64s = if raw_u64s > Self::MAX_U64S {
            Self::MAX_U64S
        } else {
            raw_u64s
        };
        // Safe: num_u64s <= MAX_U64S ensures num_u64s * 64 <= MAX_BITS,
        // which is well within usize range on all supported targets.
        (num_u64s, num_u64s * 64)
    }

    /// Create a new bloom filter with approximately `expected_items` capacity.
    ///
    /// The filter is sized for ~1% false positive rate at the expected capacity.
    #[must_use]
    pub fn with_capacity(expected_items: usize) -> Self {
        // For 1% FPR, we need m/n ~= 10 bits per item
        // Add some headroom
        //
        // Clamp: any capacity at or above MAX_EFFECTIVE_CAPACITY already
        // saturates the MAX_BITS cap in storage_layout, so this produces the
        // IDENTICAL filter on every input while giving the Trust L0 gate a
        // dominating bound on the backing-store allocation.
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
        // Branch-duplicated allocation: storage_layout already guarantees
        // `num_u64s <= MAX_U64S` on every input, so the first arm is dead
        // code and both arms build the identical filter. The Trust L0 gate
        // havocs local-callee results and its allocation recognizer needs the
        // comparison to directly dominate each `vec!` count (a phi-merged
        // clamp is not recognized), so the bound is restated as a branch.
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
    pub fn insert(&mut self, s: &str) {
        let (h1, h2) = Self::hash_pair(s.as_bytes());

        for i in 0..K {
            let bit_idx = self.get_bit_index(h1, h2, i);
            self.set_bit(bit_idx);
        }
        // Saturating: one insert per call means the count can never actually
        // reach usize::MAX, so this is identical to `+= 1` on every reachable
        // path while carrying the no-overflow proof for the Trust L0 gate.
        self.count = self.count.saturating_add(1);
    }

    /// Insert bytes into the bloom filter.
    pub fn insert_bytes(&mut self, bytes: &[u8]) {
        let (h1, h2) = Self::hash_pair(bytes);

        for i in 0..K {
            let bit_idx = self.get_bit_index(h1, h2, i);
            self.set_bit(bit_idx);
        }
        // Saturating: identical to `+= 1` on every reachable path (see insert).
        self.count = self.count.saturating_add(1);
    }

    /// Check if a string might be in the set.
    ///
    /// Returns `false` if definitely not present (no false negatives).
    /// Returns `true` if possibly present (may be false positive).
    #[must_use]
    pub fn might_contain(&self, s: &str) -> bool {
        self.might_contain_bytes(s.as_bytes())
    }

    /// Check if bytes might be in the set.
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
    #[cfg(any(test, kani))]
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
    #[must_use]
    pub fn item_count(&self) -> usize {
        self.count
    }

    /// Estimate the false positive rate.
    ///
    /// Returns a value between 0.0 (empty filter) and ~1.0 (fully saturated).
    /// Uses the theoretical formula: FPR ~= (1 - e^(-k*n/m))^k
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "FPR calculation doesn't need full precision"
    )]
    pub fn estimated_fpr(&self) -> f64 {
        if self.count == 0 {
            return 0.0;
        }
        // FPR ≈ (1 - e^(-k*n/m))^k
        let k = K as f64;
        let n = self.count as f64;
        let m = self.num_bits as f64;
        let exp = (-k * n / m).exp();
        // K = 7 (const), always fits in i32
        let k_i32 = i32::try_from(K).unwrap_or(7);
        (1.0 - exp).powi(k_i32)
    }

    /// Returns `true` when the filter is too saturated to be useful.
    ///
    /// When the estimated false positive rate exceeds 50%, the filter returns
    /// `true` for most queries, degrading to a linear scan. Callers should
    /// rebuild or resize the filter when this returns `true`. Part of #7243.
    #[must_use]
    pub fn is_saturated(&self) -> bool {
        self.estimated_fpr() > 0.5
    }

    /// Clear all bits.
    pub fn clear(&mut self) {
        self.bits.fill(0);
        self.count = 0;
    }

    /// Compute two independent hashes using FNV-1a.
    ///
    /// Uses the Kirsch-Mitzenmacher technique: h_i = h1 + i*h2
    /// Optimized to compute both hashes in a single pass.
    #[inline]
    fn hash_pair(bytes: &[u8]) -> (u64, u64) {
        // Compute both hashes in a single pass.
        // h1: Standard FNV-1a
        // h2: FNV-1a with different seed and rotated intermediate values
        let mut h1 = FNV_OFFSET;
        let mut h2 = FNV_OFFSET.rotate_left(17); // Different seed

        for &b in bytes {
            let b64 = u64::from(b);
            h1 ^= b64;
            h1 = h1.wrapping_mul(FNV_PRIME);
            h2 ^= b64;
            h2 = h2.wrapping_mul(FNV_PRIME);
        }

        // Final mixing for h2 to ensure independence
        h2 = h2.rotate_left(31);

        (h1, h2)
    }

    /// Get the bit index for the i-th hash function.
    #[inline]
    fn get_bit_index(&self, h1: u64, h2: u64, i: usize) -> usize {
        let combined = h1.wrapping_add((i as u64).wrapping_mul(h2));
        // Safe on 64-bit (lossless). On 32-bit: truncation is harmless — result is
        // immediately bounded by modulo num_bits, so any bits lost don't affect correctness
        #[allow(
            clippy::cast_possible_truncation,
            reason = "result bounded by modulo num_bits; truncation harmless"
        )]
        let combined_usize = combined as usize;
        // Single field load + explicit zero guard ahead of the `%`: the Trust
        // L0 gate needs the dominating `num_bits != 0` comparison on the same
        // local as the divisor to discharge the remzero obligation (it treats
        // `checked_rem`'s internal `unlikely()` guard as opaque). `num_bits`
        // is always >= 64 by construction (`storage_layout` floors at 64), so
        // the early return is dead on every reachable path and the result is
        // identical to `combined_usize % self.num_bits`.
        let num_bits = self.num_bits;
        if num_bits == 0 {
            return 0;
        }
        combined_usize % num_bits
    }

    /// Set a bit in the filter.
    #[inline]
    fn set_bit(&mut self, bit_idx: usize) {
        #[cfg(test)]
        count_set_bit_op();

        let word_idx = bit_idx / 64;
        let bit_pos = bit_idx % 64;
        self.bits[word_idx] |= 1u64 << bit_pos;
    }

    /// Get a bit from the filter.
    #[inline]
    fn get_bit(&self, bit_idx: usize) -> bool {
        #[cfg(test)]
        count_get_bit_op();

        let word_idx = bit_idx / 64;
        let bit_pos = bit_idx % 64;
        (self.bits[word_idx] & (1u64 << bit_pos)) != 0
    }
}

impl Default for BloomFilter {
    fn default() -> Self {
        Self::with_capacity(10_000)
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Bloom filters must not produce false negatives after insertion.
    ///
    /// Reduced from 8 to 4 max bytes to keep CBMC tractable: the hash_pair
    /// loop over symbolic bytes creates branching per byte, and K=7 hash
    /// functions multiply that for both insert and query.
    #[kani::proof]
    #[kani::unwind(8)]
    fn no_false_negatives_after_insert_symbolic_bytes() {
        const MAX_INPUT_LEN: usize = 4;
        let bytes: [u8; MAX_INPUT_LEN] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= MAX_INPUT_LEN);

        let mut bloom = BloomFilter::with_size(128);
        let needle = &bytes[..len];
        bloom.insert_bytes(needle);

        kani::assert(
            bloom.might_contain_bytes(needle),
            "inserted bytes must always be reported as present",
        );
    }

    /// Hash-derived bit indices must always stay within bit-vector bounds.
    ///
    /// Uses concrete filter size to avoid CBMC explosion from symbolic Vec
    /// allocation. Keeps hash inputs fully symbolic — the modulo arithmetic
    /// correctness is independent of filter size.
    #[kani::proof]
    #[kani::unwind(8)]
    fn bit_indices_always_within_storage_bounds() {
        const MAX_INPUT_LEN: usize = 4;
        let bytes: [u8; MAX_INPUT_LEN] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= MAX_INPUT_LEN);

        let bloom = BloomFilter::with_size(128);
        let (h1, h2) = BloomFilter::hash_pair(&bytes[..len]);

        for i in 0..K {
            let bit_idx = bloom.get_bit_index(h1, h2, i);
            kani::assert(bit_idx < bloom.num_bits, "bit index must be < num_bits");
            kani::assert(
                bit_idx / 64 < bloom.bits.len(),
                "bit index must map to a valid backing word",
            );
        }
    }

    /// Capacity constructor must keep storage geometry and counters consistent.
    ///
    /// Verifies the production sizing helpers symbolically, then checks a
    /// representative constructor call to ensure the initialized state matches
    /// the computed layout without reintroducing symbolic-allocation blowups.
    #[kani::proof]
    fn with_capacity_initializes_consistent_storage() {
        let expected_items: usize = kani::any();
        kani::assume(expected_items <= 1000);

        let requested_bits = BloomFilter::requested_bits_for_capacity(expected_items);
        let (num_u64s, final_num_bits) = BloomFilter::storage_layout(requested_bits);

        kani::assert(
            final_num_bits >= requested_bits,
            "capacity sizing must not shrink",
        );
        kani::assert(
            final_num_bits % 64 == 0,
            "num_bits must align with u64 word boundaries",
        );
        kani::assert(
            num_u64s * 64 == final_num_bits,
            "bit-vector length must match num_bits exactly",
        );

        let bloom = BloomFilter::with_capacity(7);
        let requested_bits = BloomFilter::requested_bits_for_capacity(7);
        let (num_u64s, final_num_bits) = BloomFilter::storage_layout(requested_bits);

        kani::assert(
            bloom.num_bits == final_num_bits,
            "constructor must use computed bit layout",
        );
        kani::assert(
            bloom.bits.len() == num_u64s,
            "constructor must allocate one word per computed chunk",
        );
        kani::assert(bloom.count == 0, "new filter must start with zero inserts");
    }
}

#[cfg(test)]
#[path = "bloom_tests.rs"]
mod tests;

#[cfg(kani)]
#[path = "bloom_kani_proofs.rs"]
mod kani_proofs;
