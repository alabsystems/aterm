// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn encoded_mutations_match_a_set_without_expanding_postings() {
    use aterm_spec::rng::XorShift;
    use std::collections::BTreeSet;

    let boundaries = [
        0,
        1,
        126,
        127,
        128,
        129,
        16_383,
        16_384,
        16_385,
        (1 << 21) - 1,
        1 << 21,
        (1 << 28) - 1,
        1 << 28,
        u32::MAX - 1,
        u32::MAX,
    ];
    let mut rng = XorShift::new(0xDA17_AED1_7E00_0001);
    let mut bitmap = SparseBitmap::new();
    let mut reference = BTreeSet::new();
    for step in 0..8192 {
        let mut value = if step % 2 == 0 {
            *rng.pick(&boundaries)
        } else {
            u32::try_from(rng.index(1 << 20))
                .unwrap()
                .saturating_mul(4093)
        };
        if step % 5 == 3 && !reference.is_empty() {
            value = *reference.iter().nth(rng.index(reference.len())).unwrap();
        }
        let before = decodes();
        if step % 5 < 3 {
            assert_eq!(bitmap.insert(value), reference.insert(value));
            assert!(!bitmap.insert(value), "an interior duplicate is a no-op");
        } else {
            assert_eq!(bitmap.remove(value), reference.remove(&value));
            assert!(!bitmap.remove(value), "an absent key is a no-op");
        }
        if step % 127 == 0 {
            let watermark = *rng.pick(&boundaries);
            bitmap.drop_below(watermark);
            reference.retain(|&v| v >= watermark);
        }
        assert_eq!(
            decodes(),
            before,
            "mutating a compressed list never expands it"
        );
        assert_encoding(&bitmap, &reference.iter().copied().collect::<Vec<_>>());
    }
}

#[test]
fn borrowed_cursor_matches_both_ends_after_eviction() {
    let values = [0, 1, 127, 128, 16_383, 16_384, 1 << 28, u32::MAX];
    for watermark in values {
        let mut bitmap = SparseBitmap::from_sorted(&values);
        bitmap.drop_below(watermark);
        let expected = bitmap.to_vec();
        for boundary in values.into_iter().flat_map(|v| [v, v.saturating_add(1)]) {
            let mut cursor = bitmap.cursor();
            cursor.seek_forward(boundary);
            assert_eq!(
                cursor.collect::<Vec<_>>(),
                expected
                    .iter()
                    .copied()
                    .filter(|&v| v >= boundary)
                    .collect::<Vec<_>>()
            );
            let mut cursor = bitmap.cursor();
            cursor.seek_backward(boundary);
            assert_eq!(
                cursor.rev().collect::<Vec<_>>(),
                expected
                    .iter()
                    .rev()
                    .copied()
                    .filter(|&v| v < boundary)
                    .collect::<Vec<_>>()
            );
        }
        assert_eq!(bitmap.cursor().collect::<Vec<_>>(), expected);
        assert_eq!(
            bitmap.cursor().rev().collect::<Vec<_>>(),
            expected.iter().rev().copied().collect::<Vec<_>>()
        );
        for split in 0..=expected.len() {
            let mut cursor = bitmap.cursor();
            for &value in &expected[..split] {
                assert_eq!(cursor.next(), Some(value));
            }
            for &value in expected[split..].iter().rev() {
                assert_eq!(cursor.next_back(), Some(value));
            }
            assert_eq!(cursor.next(), None);
            assert_eq!(cursor.next_back(), None);
        }
    }
    assert_eq!(SparseBitmap::default().cursor().next(), None);
    assert_eq!(SparseBitmap::default().cursor().next_back(), None);
}

fn assert_encoding(bm: &SparseBitmap, expected: &[u32]) {
    assert_eq!(bm.to_vec(), expected);
    assert_eq!(bm.len(), expected.len() as u64);
    let canonical = SparseBitmap::from_sorted(expected);
    let head = bm.head as usize;
    assert_eq!(bm.deltas.get(head..).unwrap(), canonical.deltas);
    assert!(head == 0 || head < bm.deltas.len() - head);
    if expected.len() <= 1 {
        assert_eq!(bm.head, 0);
        assert!(bm.deltas.is_empty());
    }
}

#[test]
fn prefix_eviction_preserves_mixed_operations_and_varint_boundaries() {
    use std::collections::BTreeSet;

    let seed = [
        0,
        1,
        127,
        128,
        16_383,
        16_384,
        1 << 28,
        u32::MAX - 1,
        u32::MAX,
    ];
    for watermark in seed.into_iter().flat_map(|v| [v, v.saturating_add(1)]) {
        let mut bm = SparseBitmap::from_sorted(&seed);
        bm.drop_below(watermark);
        let mut expected: Vec<_> = seed.into_iter().filter(|&v| v >= watermark).collect();
        assert_encoding(&bm, &expected);
        while let Some(last) = expected.pop() {
            assert!(bm.remove(last));
            assert_encoding(&bm, &expected);
        }
        bm.insert(0);
        bm.insert(u32::MAX);
        assert_encoding(&bm, &[0, u32::MAX]);
    }

    let mut bm = SparseBitmap::from_sorted(&(0..256).collect::<Vec<_>>());
    let mut expected: BTreeSet<_> = (0..256).collect();
    for step in 0..2048u32 {
        let watermark = step + 1;
        bm.drop_below(watermark);
        expected.retain(|&v| v >= watermark);
        assert_eq!(bm.insert(step + 256), expected.insert(step + 256));
        if step % 19 == 0 {
            let interior = step + 99;
            assert_eq!(bm.remove(interior), expected.remove(&interior));
        }
        if step % 23 == 0 {
            let interior = step + 101;
            assert_eq!(bm.insert(interior), expected.insert(interior));
        }
        if step % 127 == 0 {
            let clone = bm.clone();
            assert_encoding(&clone, &expected.iter().copied().collect::<Vec<_>>());
            let other = SparseBitmap::from_sorted(&[step + 12, step + 99, step + 256]);
            let intersection: Vec<_> = expected
                .iter()
                .copied()
                .filter(|&v| other.contains(v))
                .collect();
            assert_encoding(&(&bm & &other), &intersection);
            let mut assigned = bm.clone();
            assigned &= &other;
            assert_encoding(&assigned, &intersection);
        }
        assert_encoding(&bm, &expected.iter().copied().collect::<Vec<_>>());
    }
    bm.rebuild_from_sorted(&[0, 9, 128]);
    assert_eq!(bm.head, 0, "a rebuild rebases the byte cursor");
    assert_encoding(&bm, &[0, 9, 128]);
    bm.drop_below(u32::MAX);
    assert_encoding(&bm, &[]);
    bm.insert(42);
    assert_encoding(&bm, &[42]);
}

#[test]
fn full_history_eviction_work_is_amortized_and_prefix_storage_is_bounded() {
    const WINDOW: u32 = 2048;
    const STEPS: u32 = WINDOW * 2;
    let seed: Vec<_> = (0..WINDOW).collect();
    let mut bm = SparseBitmap::from_sorted(&seed);
    let initial_capacity = bm.deltas.capacity();
    let decodes_before = decodes();
    let prefix_before = PREFIX_BYTES.with(std::cell::Cell::get);
    let moved_before = COMPACTED_BYTES.with(std::cell::Cell::get);
    for step in 0..STEPS {
        bm.drop_below(step + 1);
        bm.insert(step + WINDOW);
        let head = bm.head as usize;
        assert!(head < bm.deltas.len() - head);
        assert!(bm.deltas.capacity() <= initial_capacity * 2);
    }
    assert_eq!(
        decodes(),
        decodes_before,
        "eviction never decodes survivors"
    );
    let consumed = PREFIX_BYTES.with(std::cell::Cell::get) - prefix_before;
    let moved = COMPACTED_BYTES.with(std::cell::Cell::get) - moved_before;
    assert_eq!(consumed, STEPS as usize, "one gap per evicted row");
    assert!(
        moved <= consumed,
        "compaction amortizes against consumed bytes"
    );
    assert_encoding(&bm, &(STEPS..STEPS + WINDOW).collect::<Vec<_>>());
    #[cfg(target_pointer_width = "64")]
    assert_eq!(
        std::mem::size_of::<SparseBitmap>(),
        std::mem::size_of::<(Vec<u8>, u32, u32, u32)>()
    );
    eprintln!(
        "posting_eviction: rows={WINDOW} steps={STEPS} prefix_bytes={consumed} moved_bytes={moved} \
         retained_capacity={} initial_capacity={initial_capacity} bitmap_bytes={}",
        bm.deltas.capacity(),
        std::mem::size_of::<SparseBitmap>()
    );
}

#[test]
fn test_bitand_disjoint_and_empty() {
    let mut a = SparseBitmap::new();
    a.insert(1);
    a.insert(3);
    let mut b = SparseBitmap::new();
    b.insert(2);
    b.insert(4);
    assert!((&a & &b).is_empty());
    let empty = SparseBitmap::new();
    assert!((&a & &empty).is_empty());
    assert!((&empty & &a).is_empty());
}

fn decodes() -> usize {
    DECODES.with(std::cell::Cell::get)
}

#[test]
fn remove_last_is_o1_and_canonical() {
    let values = [3u32, 4, 200, 70_000, 70_001, 5_000_000];
    let mut bm = SparseBitmap::new();
    for &v in &values {
        bm.insert(v);
    }
    for keep in (0..values.len()).rev() {
        let before = decodes();
        assert!(bm.remove(values[keep]));
        assert_eq!(decodes(), before, "tail removal must not decode");
        let expect = SparseBitmap::from_sorted(&values[..keep]);
        assert_eq!(bm.deltas, expect.deltas, "canonical deltas at keep={keep}");
        assert_eq!(bm.len(), keep as u64);
        if keep > 0 {
            assert_eq!((bm.first, bm.last), (expect.first, expect.last));
        }
        assert_eq!(bm.to_vec(), values[..keep].to_vec());
    }
    assert!(bm.is_empty());
    assert!(!bm.remove(3), "removing from an emptied bitmap is a miss");
    assert!(bm.insert(9), "an emptied bitmap takes a fresh first value");
    assert_eq!(bm.to_vec(), vec![9]);
}
