// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

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
