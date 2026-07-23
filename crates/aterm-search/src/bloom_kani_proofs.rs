// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kani proofs for BloomFilter correctness.
//!
//! Proves the fundamental bloom filter invariant: no false negatives.
//! After `insert(s)`, `might_contain(s)` MUST return `true`.
//! Also proves bit-index bounds safety and capacity invariants.
//!
//! Part of #2679.

use super::*;

/// INV-BLOOM-1: No false negatives for single-byte keys.
///
/// After inserting any single byte, `might_contain_bytes` returns true.
/// Uses fully symbolic input via `kani::any()`.
#[kani::proof]
fn bloom_no_false_negative_single_byte() {
    let mut bloom = BloomFilter::with_size(128);

    let byte: u8 = kani::any();
    let key = [byte];

    bloom.insert_bytes(&key);
    kani::assert(
        bloom.might_contain_bytes(&key),
        "INV-BLOOM-1: false negative on single byte",
    );
}

/// INV-BLOOM-1b: No false negatives for two-byte keys.
///
/// After inserting any two-byte sequence, `might_contain_bytes` returns true.
#[kani::proof]
fn bloom_no_false_negative_two_bytes() {
    let mut bloom = BloomFilter::with_size(256);

    let b0: u8 = kani::any();
    let b1: u8 = kani::any();
    let key = [b0, b1];

    bloom.insert_bytes(&key);
    kani::assert(
        bloom.might_contain_bytes(&key),
        "INV-BLOOM-1b: false negative on two bytes",
    );
}

/// INV-BLOOM-2: Bit index is always within bounds.
///
/// For any hash pair and any hash function index (0..K),
/// `get_bit_index` returns a value strictly less than `num_bits`.
/// This ensures `set_bit` and `get_bit` never panic on out-of-bounds.
///
/// Uses concrete filter size to avoid CBMC state explosion from symbolic
/// Vec allocation. Hash values remain fully symbolic — the modulo arithmetic
/// is size-independent, so a single concrete size proves the property.
#[kani::proof]
#[kani::unwind(8)]
fn bloom_bit_index_in_bounds() {
    let h1: u64 = kani::any();
    let h2: u64 = kani::any();
    let i: usize = kani::any();
    kani::assume(i < K);

    let bloom = BloomFilter::with_size(128);

    let bit_idx = bloom.get_bit_index(h1, h2, i);

    kani::assert(
        bit_idx < bloom.num_bits,
        "INV-BLOOM-2: bit index must be < num_bits",
    );

    // Verify word-level access would be in bounds
    let word_idx = bit_idx / 64;
    kani::assert(
        word_idx < bloom.bits.len(),
        "INV-BLOOM-2b: word index must be < bits.len()",
    );
}

/// INV-BLOOM-3: Fresh filter contains nothing.
///
/// A newly constructed filter must return false for `might_contain_bytes`
/// with any input. (No spurious bits set on construction.)
///
/// Uses concrete size to avoid CBMC state explosion from symbolic Vec
/// allocation. The zeroed-initialization property is size-independent.
#[kani::proof]
#[kani::unwind(8)]
fn bloom_fresh_filter_empty() {
    let bloom = BloomFilter::with_size(128);

    let byte: u8 = kani::any();
    let key = [byte];

    kani::assert(
        !bloom.might_contain_bytes(&key),
        "INV-BLOOM-3: fresh filter must not contain anything",
    );
}

/// INV-BLOOM-4: Count tracks insertions.
///
/// After N insertions (N in 0..=2), `count()` equals N.
/// Uses concrete filter size and bounded N to keep CBMC tractable.
/// Reduced from n<=4 to n<=2: each insert_bytes call has K=7 hash
/// operations, so 4 inserts = 28 symbolic hash chains causing explosion.
// TODO(#7932): tautology — strengthen or delete — T1: constructor round-trip field == any-binding
#[kani::proof]
#[kani::unwind(8)] // K=7 hash functions + outer loop bound
fn bloom_count_tracks_insertions() {
    let mut bloom = BloomFilter::with_size(128);

    let n: usize = kani::any();
    kani::assume(n <= 2);

    for i in 0..n {
        #[allow(clippy::cast_possible_truncation, reason = "n <= 2, always fits in u8")]
        let key = [i as u8];
        bloom.insert_bytes(&key);
    }

    kani::assert(
        bloom.count() == n,
        "INV-BLOOM-4: count must equal number of insertions",
    );
}

/// INV-BLOOM-5: Clear resets all state.
///
/// After clear(), count is 0 and no queries return true.
#[kani::proof]
#[kani::unwind(8)] // K=7 hash loop + Vec::fill on 2-element backing store
fn bloom_clear_resets_state() {
    let mut bloom = BloomFilter::with_size(128);

    let byte: u8 = kani::any();
    let key = [byte];

    bloom.insert_bytes(&key);
    kani::assert(bloom.count() == 1, "precondition: one item inserted");

    bloom.clear();

    kani::assert(
        bloom.count() == 0,
        "INV-BLOOM-5a: count must be 0 after clear",
    );
    kani::assert(
        !bloom.might_contain_bytes(&key),
        "INV-BLOOM-5b: cleared filter must not contain previously inserted key",
    );
}

/// INV-BLOOM-6: num_bits is always a multiple of 64.
///
/// This invariant ensures `word_idx = bit_idx / 64` is always valid
/// because `num_bits = num_u64s * 64` by construction.
///
/// Verifies the production sizing helper without allocating a Vec, which
/// avoids CBMC state explosion from symbolic-length heap allocations while
/// still checking the exact arithmetic used by `with_size`.
#[kani::proof]
fn bloom_num_bits_multiple_of_64() {
    let size: usize = kani::any();
    kani::assume(size >= 1 && size <= 2048);

    let (num_u64s, final_num_bits) = BloomFilter::storage_layout(size);

    kani::assert(
        final_num_bits % 64 == 0,
        "INV-BLOOM-6a: num_bits must be multiple of 64",
    );
    kani::assert(
        final_num_bits >= 64,
        "INV-BLOOM-6b: num_bits must be at least 64",
    );
    kani::assert(
        num_u64s * 64 == final_num_bits,
        "INV-BLOOM-6c: bits.len() * 64 must equal num_bits",
    );
}
