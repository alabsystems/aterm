// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

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

    for item in &items {
        assert!(bloom.might_contain(item), "False negative for '{item}'");
    }

    // Every 1- and 2-byte key (the index's short keys), alone in a fresh filter and
    // all together in one.
    let keys: Vec<Vec<u8>> = (0..=u8::MAX)
        .map(|a| vec![a])
        .chain((0..=u8::MAX).flat_map(|a| (0..=u8::MAX).map(move |b| vec![a, b])))
        .collect();
    let mut shared = BloomFilter::with_capacity(keys.len());
    for key in &keys {
        let mut fresh = BloomFilter::with_capacity(16);
        fresh.insert_bytes(key);
        assert!(fresh.might_contain_bytes(key), "{key:02x?} alone");
        shared.insert_bytes(key);
    }
    for key in &keys {
        assert!(shared.might_contain_bytes(key), "{key:02x?} among all");
    }
}

#[test]
fn bloom_false_positive_rate() {
    let mut bloom = BloomFilter::with_capacity(1000);

    for i in 0..1000 {
        bloom.insert(&format!("inserted_{i}"));
    }

    let mut false_positives = 0;
    for i in 0..10000 {
        if bloom.might_contain(&format!("not_inserted_{i}")) {
            false_positives += 1;
        }
    }

    // Sized for ~1%; 5% allows for variance.
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
    assert!(
        bloom.bits.iter().all(|&word| word == 0),
        "clear zeroes every word"
    );
    assert!(
        !bloom.might_contain("test"),
        "cleared filter must not report previously-inserted item"
    );
}

#[test]
fn bloom_estimated_fpr() {
    let mut bloom = BloomFilter::with_capacity(1000);

    for i in 0..1000 {
        bloom.insert(&format!("item_{i}"));
    }

    let fpr = bloom.estimated_fpr();
    assert!(fpr < 0.02, "Estimated FPR too high: {:.2}%", fpr * 100.0);
}

#[test]
fn bloom_with_size() {
    let bloom = BloomFilter::with_size(1024);
    assert_eq!(bloom.num_bits(), 1024);
}

/// A lookup of a present item checks exactly K bits whatever the filter size.
#[test]
fn bloom_lookup_constant_time() {
    fn measure_lookup_ops(num_bits: usize, num_items: usize, num_lookups: usize) -> usize {
        take_get_bit_ops();
        take_set_bit_ops();

        let mut bloom = BloomFilter::with_size(num_bits);

        for i in 0..num_items {
            bloom.insert(&format!("item_{i}"));
        }

        take_get_bit_ops();
        take_set_bit_ops();

        // Present items, so every lookup checks all K bits.
        for i in 0..num_lookups {
            let result = bloom.might_contain(&format!("item_{}", i % num_items));
            assert!(result, "item_{} should exist in filter", i % num_items);
        }

        take_get_bit_ops()
    }

    let items = 1000;
    let lookups = 1000;

    let ops_small = measure_lookup_ops(10_000, items, lookups);
    let ops_large = measure_lookup_ops(1_000_000, items, lookups);

    assert!(
        ops_small > 0,
        "lookup on 10K-bit filter should perform ops, got 0"
    );
    assert!(
        ops_large > 0,
        "lookup on 1M-bit filter should perform ops, got 0"
    );

    assert_eq!(
        ops_small, ops_large,
        "O(1) lookup: ops should be equal regardless of filter size (small={ops_small}, large={ops_large})"
    );

    let expected_ops = lookups * K;
    assert_eq!(
        ops_small, expected_ops,
        "Expected exactly {expected_ops} bit ops ({lookups} lookups * K={K}), got {ops_small}"
    );
}

/// An insert sets exactly K bits whatever the filter size.
#[test]
fn bloom_insert_constant_time() {
    fn measure_insert_ops(num_bits: usize, num_inserts: usize) -> usize {
        take_get_bit_ops();
        take_set_bit_ops();

        let mut bloom = BloomFilter::with_size(num_bits);

        for i in 0..num_inserts {
            bloom.insert(&format!("item_{i}"));
        }

        take_set_bit_ops()
    }

    let inserts = 1000;

    let ops_small = measure_insert_ops(10_000, inserts);
    let ops_large = measure_insert_ops(1_000_000, inserts);

    assert!(
        ops_small > 0,
        "insert on 10K-bit filter should perform ops, got 0"
    );
    assert!(
        ops_large > 0,
        "insert on 1M-bit filter should perform ops, got 0"
    );

    assert_eq!(
        ops_small, ops_large,
        "O(1) insert: ops should be equal regardless of filter size (small={ops_small}, large={ops_large})"
    );

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

    bloom.insert("world");
    assert!(bloom.might_contain_bytes(b"world"));
    assert!(bloom.might_contain("world"));
}

#[test]
fn bloom_default() {
    let bloom = BloomFilter::default();
    // 10_000 items * 10 bits, rounded up to a multiple of 64.
    assert_eq!(bloom.num_bits(), 100_032);
    assert_eq!(bloom.count(), 0);
    assert!(!bloom.might_contain("anything"));
}

#[test]
fn bloom_empty_string() {
    let mut bloom = BloomFilter::with_capacity(100);

    bloom.insert("hello");
    assert!(bloom.might_contain("hello"));
    assert_eq!(bloom.count(), 1);

    bloom.insert("");
    assert!(bloom.might_contain(""));
    assert_eq!(bloom.count(), 2);

    assert!(
        bloom.might_contain("hello"),
        "inserting empty string must not corrupt existing entries"
    );
}

#[test]
fn bloom_with_size_rounding() {
    // Sizes round up to a multiple of 64, at least 64.
    let bloom = BloomFilter::with_size(100);
    assert_eq!(bloom.num_bits(), 128);

    let bloom = BloomFilter::with_size(65);
    assert_eq!(bloom.num_bits(), 128);

    let bloom = BloomFilter::with_size(64);
    assert_eq!(bloom.num_bits(), 64);

    let bloom = BloomFilter::with_size(1);
    assert_eq!(bloom.num_bits(), 64);

    let bloom = BloomFilter::with_size(0);
    assert_eq!(bloom.num_bits(), 64);

    // num_bits is a multiple of 64 and exactly the words allocated, which
    // `bits[bit_idx / 64]` in set_bit/get_bit relies on.
    for n in 0..=4096 {
        for bloom in [BloomFilter::with_size(n), BloomFilter::with_capacity(n)] {
            assert_eq!(bloom.num_bits() % 64, 0, "{n}");
            assert_eq!(bloom.num_bits(), bloom.bits.len() * 64, "{n}");
            assert!(bloom.count() == 0 && bloom.bits.iter().all(|&w| w == 0));
        }
    }
}

/// A lookup returns at the first clear bit, so misses check fewer than K bits.
#[test]
fn bloom_lookup_early_return_on_miss() {
    take_get_bit_ops();
    take_set_bit_ops();

    // Sparse (100 items in 1M bits): a miss almost always hits a 0 bit quickly.
    let mut bloom = BloomFilter::with_size(1_000_000);
    for i in 0..100 {
        bloom.insert(&format!("existing_{i}"));
    }

    take_get_bit_ops();
    take_set_bit_ops();

    let lookups = 1000usize;
    let mut false_positives = 0usize;
    for i in 0..lookups {
        if bloom.might_contain(&format!("nonexistent_{i}")) {
            false_positives += 1;
        }
    }

    // The theoretical rate is negligible; 1% is a generous bound.
    assert!(
        false_positives <= lookups / 100,
        "false positive rate too high: {false_positives}/{lookups} ({:.1}%)",
        false_positives as f64 / lookups as f64 * 100.0
    );

    let ops = take_get_bit_ops();

    let avg_ops_per_lookup = ops / lookups;

    assert!(
        avg_ops_per_lookup < K,
        "Early return optimization: avg ops per miss ({avg_ops_per_lookup}) should be less than K={K} for sparse filter"
    );

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

/// A capacity past the bit cap is capped.
#[test]
fn bloom_capacity_near_overflow_boundary() {
    // 10 bits per item puts 2^28 items past MAX_BITS on every target.
    let bloom = BloomFilter::with_capacity(1 << 28);
    assert!(bloom.num_bits() > 0);
    assert!(bloom.num_bits() <= BloomFilter::MAX_BITS);
    assert_eq!(bloom.num_bits() % 64, 0);
}
