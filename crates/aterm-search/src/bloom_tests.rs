// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn bloom_insert_and_query() {
    let mut bloom = BloomFilter::with_capacity(100);

    bloom.insert("hello");
    bloom.insert("world");

    assert!(bloom.might_contain("hello"));
    assert!(bloom.might_contain("world"));
}

#[test]
fn bloom_no_false_negatives() {
    let mut bloom = BloomFilter::with_capacity(1000);

    let items: Vec<String> = (0..1000).map(|i| format!("item_{i}")).collect();
    for item in &items {
        bloom.insert(item);
    }

    // All inserted items must be found
    for item in &items {
        assert!(bloom.might_contain(item), "False negative for '{item}'");
    }
}

#[test]
fn bloom_false_positive_rate() {
    let mut bloom = BloomFilter::with_capacity(1000);

    // Insert 1000 items
    for i in 0..1000 {
        bloom.insert(&format!("inserted_{i}"));
    }

    // Test 10000 items that were NOT inserted
    let mut false_positives = 0;
    for i in 0..10000 {
        if bloom.might_contain(&format!("not_inserted_{i}")) {
            false_positives += 1;
        }
    }

    // FPR should be around 1% (allow up to 5% for statistical variance)
    let fpr = f64::from(false_positives) / 10000.0;
    assert!(fpr < 0.05, "FPR too high: {:.2}%", fpr * 100.0);
}

#[test]
fn bloom_clear() {
    let mut bloom = BloomFilter::with_capacity(100);
    bloom.insert("test");
    assert!(bloom.might_contain("test"));
    assert_eq!(bloom.count(), 1);

    bloom.clear();
    assert_eq!(bloom.count(), 0);
    // After clearing all bits, the filter must reject the previously-inserted item.
    // With all bits zeroed, false positives are impossible.
    assert!(
        !bloom.might_contain("test"),
        "cleared filter must not report previously-inserted item"
    );
}

#[test]
fn bloom_estimated_fpr() {
    let mut bloom = BloomFilter::with_capacity(1000);

    // Insert items at expected capacity
    for i in 0..1000 {
        bloom.insert(&format!("item_{i}"));
    }

    // Estimated FPR should be around 1%
    let fpr = bloom.estimated_fpr();
    assert!(fpr < 0.02, "Estimated FPR too high: {:.2}%", fpr * 100.0);
}

#[test]
fn bloom_with_size() {
    let bloom = BloomFilter::with_size(1024);
    assert_eq!(bloom.num_bits(), 1024);
}

/// Verify bloom filter lookups are O(1) with respect to filter size.
///
/// The bit lookup operations should be constant regardless of how many bits
/// are in the filter. This verifies the O(1) claims in the module documentation.
///
/// Note: We lookup EXISTING items to ensure all K bits are checked (worst case).
/// Non-existing items can short-circuit when any bit is 0, which is still O(1)
/// but with variable operation counts depending on filter density.
///
/// Addresses claim verification in #1647.
#[test]
fn bloom_lookup_constant_time() {
    fn measure_lookup_ops(num_bits: usize, num_items: usize, num_lookups: usize) -> usize {
        // Clear counters
        take_get_bit_ops();
        take_set_bit_ops();

        let mut bloom = BloomFilter::with_size(num_bits);

        // Insert items
        for i in 0..num_items {
            bloom.insert(&format!("item_{i}"));
        }

        // Clear ops from setup
        take_get_bit_ops();
        take_set_bit_ops();

        // Perform lookups of EXISTING items (checks all K bits)
        for i in 0..num_lookups {
            let result = bloom.might_contain(&format!("item_{}", i % num_items));
            assert!(result, "item_{} should exist in filter", i % num_items);
        }

        take_get_bit_ops()
    }

    let items = 1000;
    let lookups = 1000;

    // Measure with small filter (10K bits) vs large filter (1M bits)
    let ops_small = measure_lookup_ops(10_000, items, lookups);
    let ops_large = measure_lookup_ops(1_000_000, items, lookups);

    // Both should perform the same number of operations (K=7 get_bit calls per lookup)
    assert!(
        ops_small > 0,
        "lookup on 10K-bit filter should perform ops, got 0"
    );
    assert!(
        ops_large > 0,
        "lookup on 1M-bit filter should perform ops, got 0"
    );

    // O(1) = ops should be equal (both do `lookups * K` operations)
    assert_eq!(
        ops_small, ops_large,
        "O(1) lookup: ops should be equal regardless of filter size (small={ops_small}, large={ops_large})"
    );

    // Each lookup checks exactly K=7 bits
    let expected_ops = lookups * K;
    assert_eq!(
        ops_small, expected_ops,
        "Expected exactly {expected_ops} bit ops ({lookups} lookups * K={K}), got {ops_small}"
    );
}

/// Verify bloom filter inserts are O(1) with respect to filter size.
///
/// Each insert should set exactly K=7 bits regardless of filter size.
/// This verifies the O(1) insert claim in the module documentation.
///
/// Addresses claim verification in #1647.
#[test]
fn bloom_insert_constant_time() {
    fn measure_insert_ops(num_bits: usize, num_inserts: usize) -> usize {
        // Clear counters
        take_get_bit_ops();
        take_set_bit_ops();

        let mut bloom = BloomFilter::with_size(num_bits);

        // Perform inserts
        for i in 0..num_inserts {
            bloom.insert(&format!("item_{i}"));
        }

        take_set_bit_ops()
    }

    let inserts = 1000;

    // Measure with small filter (10K bits) vs large filter (1M bits)
    let ops_small = measure_insert_ops(10_000, inserts);
    let ops_large = measure_insert_ops(1_000_000, inserts);

    // Both should perform the same number of operations (K=7 set_bit calls per insert)
    assert!(
        ops_small > 0,
        "insert on 10K-bit filter should perform ops, got 0"
    );
    assert!(
        ops_large > 0,
        "insert on 1M-bit filter should perform ops, got 0"
    );

    // O(1) = ops should be equal (both do `inserts * K` operations)
    assert_eq!(
        ops_small, ops_large,
        "O(1) insert: ops should be equal regardless of filter size (small={ops_small}, large={ops_large})"
    );

    // Each insert sets exactly K=7 bits
    let expected_ops = inserts * K;
    assert_eq!(
        ops_small, expected_ops,
        "Expected exactly {expected_ops} bit ops ({inserts} inserts * K={K}), got {ops_small}"
    );
}

#[test]
fn bloom_insert_bytes() {
    let mut bloom = BloomFilter::with_capacity(100);

    bloom.insert_bytes(b"hello");
    assert!(bloom.might_contain_bytes(b"hello"));
    assert!(!bloom.might_contain_bytes(b"world"));
    assert_eq!(bloom.count(), 1);

    // insert_bytes and insert should agree for valid UTF-8
    bloom.insert("world");
    assert!(bloom.might_contain_bytes(b"world"));
    assert!(bloom.might_contain("world"));
}

#[test]
fn bloom_default() {
    let bloom = BloomFilter::default();
    // Default creates a 10K-capacity filter: 10_000 * 10 = 100_000 bits
    // Rounded to next multiple of 64: 100_032 bits (1563 * 64)
    assert_eq!(bloom.num_bits(), 100_032);
    assert_eq!(bloom.count(), 0);
    assert!(!bloom.might_contain("anything"));
}

#[test]
fn bloom_empty_string() {
    let mut bloom = BloomFilter::with_capacity(100);

    // Insert a known string, verify it's found
    bloom.insert("hello");
    assert!(bloom.might_contain("hello"));
    assert_eq!(bloom.count(), 1);

    // Insert empty string
    bloom.insert("");
    assert!(bloom.might_contain(""));
    assert_eq!(bloom.count(), 2);

    // Original string must still be found (no interference)
    assert!(
        bloom.might_contain("hello"),
        "inserting empty string must not corrupt existing entries"
    );
}

#[test]
fn bloom_with_size_rounding() {
    // Non-power-of-2 sizes get rounded up to the next multiple of 64
    let bloom = BloomFilter::with_size(100);
    // 100 / 64 = 1.5625 → ceil = 2 → 2 * 64 = 128
    assert_eq!(bloom.num_bits(), 128);

    let bloom = BloomFilter::with_size(65);
    // 65 / 64 = 1.015... → ceil = 2 → 128
    assert_eq!(bloom.num_bits(), 128);

    let bloom = BloomFilter::with_size(64);
    // Exact multiple → 64
    assert_eq!(bloom.num_bits(), 64);

    // Minimum is 64 bits even for tiny requests
    let bloom = BloomFilter::with_size(1);
    assert_eq!(bloom.num_bits(), 64);

    let bloom = BloomFilter::with_size(0);
    assert_eq!(bloom.num_bits(), 64);
}

/// Verify bloom filter early-return optimization for non-existing items.
///
/// When checking items not in the filter, the lookup should short-circuit
/// and return early when it finds the first 0 bit. This verifies the
/// "checks at most K=7 bits (early return on miss)" claim in documentation.
///
/// Addresses claim verification in #1647.
#[test]
fn bloom_lookup_early_return_on_miss() {
    // Clear counters
    take_get_bit_ops();
    take_set_bit_ops();

    // Create a large, sparse filter (1M bits with 100 items = very sparse)
    // This ensures non-existing items will almost always hit a 0 bit quickly
    let mut bloom = BloomFilter::with_size(1_000_000);
    for i in 0..100 {
        bloom.insert(&format!("existing_{i}"));
    }

    // Clear setup ops
    take_get_bit_ops();
    take_set_bit_ops();

    // Lookup many non-existing items and track false positives
    let lookups = 1000usize;
    let mut false_positives = 0usize;
    for i in 0..lookups {
        if bloom.might_contain(&format!("nonexistent_{i}")) {
            false_positives += 1;
        }
    }

    // With 100 items in a 1M-bit filter, the theoretical false positive rate
    // is negligible (<0.001%). Allow up to 1% as a generous upper bound.
    assert!(
        false_positives <= lookups / 100,
        "false positive rate too high: {false_positives}/{lookups} ({:.1}%)",
        false_positives as f64 / lookups as f64 * 100.0
    );

    let ops = take_get_bit_ops();

    // With a sparse filter, average ops per miss should be less than K
    // because we return early on the first 0 bit found
    let avg_ops_per_lookup = ops / lookups;

    // Early return means we check fewer than K bits on average for misses
    // In a sparse filter, we typically find a 0 bit within 1-3 checks
    assert!(
        avg_ops_per_lookup < K,
        "Early return optimization: avg ops per miss ({avg_ops_per_lookup}) should be less than K={K} for sparse filter"
    );

    // Verify we're actually doing meaningful work (not zero ops)
    assert!(ops > 0, "Should perform at least some bit checks");
}

/// Zero capacity must produce a valid (minimum-sized) filter, not panic or UB.
#[test]
fn bloom_zero_capacity() {
    let bloom = BloomFilter::with_capacity(0);
    assert_eq!(
        bloom.num_bits(),
        64,
        "zero capacity should produce minimum 64-bit filter"
    );
    assert_eq!(bloom.count(), 0);
}

/// Extremely large capacity values must be capped rather than overflow.
#[test]
fn bloom_large_capacity_no_overflow() {
    // This would overflow `num_u64s * 64` without the cap.
    let bloom = BloomFilter::with_capacity(usize::MAX);
    assert!(bloom.num_bits() > 0, "must produce a positive bit count");
    assert!(
        bloom.num_bits() <= BloomFilter::MAX_BITS,
        "must be capped to MAX_BITS"
    );
    assert_eq!(bloom.num_bits() % 64, 0, "must be word-aligned");
    assert_eq!(bloom.count(), 0);

    // Verify the filter is still functional after capping.
    let mut bloom = bloom;
    bloom.insert("test");
    assert!(bloom.might_contain("test"), "capped filter must still work");
}

/// `with_size` at usize::MAX must be capped, not overflow.
#[test]
fn bloom_with_size_max_no_overflow() {
    let bloom = BloomFilter::with_size(usize::MAX);
    assert!(bloom.num_bits() <= BloomFilter::MAX_BITS);
    assert_eq!(bloom.num_bits() % 64, 0);
}

/// Capacity just above the overflow threshold must be handled.
#[test]
fn bloom_capacity_near_overflow_boundary() {
    // capacity = 2^28 + 1 → requested_bits = (2^28+1)*10 which may overflow on 32-bit
    let bloom = BloomFilter::with_capacity(1 << 28);
    assert!(bloom.num_bits() > 0);
    assert!(bloom.num_bits() <= BloomFilter::MAX_BITS);
    assert_eq!(bloom.num_bits() % 64, 0);
}
