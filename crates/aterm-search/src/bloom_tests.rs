// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn saturation_predicate_matches_the_probability_model_at_integer_boundaries() {
    use aterm_spec::rng::XorShift;
    let mut rng = XorShift::new(0xB100_5A7E_7E00_0001);
    let mut filter = BloomFilter::with_size(64);
    for bits in (1..=4096)
        .map(|n| n * 64)
        .chain(
            (0..4096)
                .map(|_| BloomFilter::storage_layout(rng.range(64, BloomFilter::MAX_BITS + 1)).1),
        )
        .chain([BloomFilter::MAX_BITS])
    {
        // The saturation estimate depends on layout and insertion count, not on bit contents.
        // Avoid allocating a maximum-sized filter to inspect its numerical boundary.
        filter.num_bits = bits;
        // Locate the original probability model's first saturated integer count.
        let mut low = 0;
        let mut high = bits;
        while low < high {
            let mid = low + (high - low) / 2;
            filter.count = mid;
            if filter.estimated_fpr() > 0.5 {
                high = mid;
            } else {
                low = mid + 1;
            }
        }
        let threshold = low;
        for count in [
            0,
            threshold.saturating_sub(1),
            threshold,
            threshold + 1,
            threshold + 2,
            usize::MAX,
        ] {
            filter.count = count;
            assert_eq!(
                filter.is_saturated(),
                filter.estimated_fpr() > 0.5,
                "bits={bits} count={count}"
            );
        }
    }
}

#[test]
#[ignore = "manual saturation predicate cost measurement"]
fn saturation_predicate_cost() {
    let mut filter = BloomFilter::with_capacity(1000);
    const REPEATS: u32 = 1_000_000;
    let mut reference_hits = None;
    for direct in [false, true] {
        let began = std::time::Instant::now();
        let mut hits = 0usize;
        for step in 0..REPEATS {
            filter.count = std::hint::black_box(step as usize % 8000);
            let filter = std::hint::black_box(&filter);
            let saturated = if direct {
                filter.is_saturated()
            } else {
                filter.estimated_fpr() > 0.5
            };
            hits += usize::from(std::hint::black_box(saturated));
        }
        let elapsed = began.elapsed();
        assert_eq!(*reference_hits.get_or_insert(hits), hits);
        eprintln!(
            "saturation_predicate direct={direct} repeats={REPEATS} ns_per_op={:.3}",
            elapsed.as_secs_f64() * 1e9 / f64::from(REPEATS)
        );
    }
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

/// `hash_pair` folds both hashes in one loop, spelled out for the Trust lane: the first is
/// `aterm_hash`'s FNV-1a, and the second its fold from the other seed, rotated.
#[test]
fn hash_pair_folds_as_aterm_hash_does() {
    let inputs: [&[u8]; 4] = [b"", b"a", b"hel", &[0, 0xff, 0x80, 0x7f, 1, 2, 3, 4, 5]];
    for bytes in inputs {
        let seed = aterm_hash::FNV64_OFFSET.rotate_left(17);
        assert_eq!(
            BloomFilter::hash_pair(bytes),
            (
                aterm_hash::fnv1a_64(bytes),
                aterm_hash::fnv1a_64_extend(seed, bytes).rotate_left(31),
            ),
            "{bytes:02x?}"
        );
    }
}
