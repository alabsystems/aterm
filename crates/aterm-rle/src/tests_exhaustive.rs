// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// Index<u32> valid access via `rle[i]` syntax.
#[test]
fn rle_index_trait_valid_access() {
    let rle = Rle::from_iter([10u8, 20, 20, 30]);
    assert_eq!(rle[0], 10);
    assert_eq!(rle[1], 20);
    assert_eq!(rle[2], 20);
    assert_eq!(rle[3], 30);
}

/// Index<u32> panics on out-of-bounds access.
#[test]
#[should_panic(expected = "index out of bounds")]
fn rle_index_trait_oob_panics() {
    let rle = Rle::from_iter([1u8, 2, 3]);
    let _ = rle[3]; // OOB: valid indices are 0..3
}

/// ExactSizeIterator reports correct remaining count after partial iteration.
#[test]
fn rle_exact_size_iter_after_partial_iteration() {
    let rle = Rle::from_iter([1u8, 1, 2, 3, 3, 3]);
    let mut iter = rle.iter();
    assert_eq!(iter.len(), 6);

    iter.next(); // consume 1st
    assert_eq!(iter.len(), 5);

    iter.next(); // consume 2nd
    iter.next(); // consume 3rd
    assert_eq!(iter.len(), 3);

    iter.next(); // consume 4th
    iter.next(); // consume 5th
    iter.next(); // consume 6th (last)
    assert_eq!(iter.len(), 0);

    // Exhausted — still reports 0
    assert_eq!(iter.next(), None);
    assert_eq!(iter.len(), 0);
}

/// Exhaustive small-domain test for `set` correctness (value written,
/// other values preserved, structural invariants). Covers len 1..=4,
/// all valid indices, and representative new_val values.
#[test]
fn rle_set_correctness_exhaustive() {
    for len in 1u8..=4 {
        for idx in 0..len {
            for &new_val in &[0u8, 1, 99, 255] {
                let mut rle = Rle::with_value(1u8, len as u32);
                let original_len = rle.len();
                rle.set(idx as u32, new_val);

                // Value correctness
                assert_eq!(
                    rle.get(idx as u32),
                    Some(new_val),
                    "set({idx},{new_val}) on len={len}: wrong value",
                );
                // Length preservation
                assert_eq!(
                    rle.len(),
                    original_len,
                    "set({idx},{new_val}) on len={len}: length changed",
                );
                // Other values preserved
                for other in 0..len {
                    if other != idx {
                        assert_eq!(
                            rle.get(other as u32),
                            Some(1),
                            "set({idx},{new_val}) on len={len}: value at {other} changed",
                        );
                    }
                }
                // Structural invariants
                for run in rle.runs() {
                    assert!(run.length > 0, "zero-length run");
                }
                let runs = rle.runs();
                for i in 0..runs.len().saturating_sub(1) {
                    assert_ne!(
                        runs[i].value,
                        runs[i + 1].value,
                        "adjacent duplicate at {i}",
                    );
                }
                let sum: u32 = runs.iter().map(|r| r.length).sum();
                assert_eq!(rle.len(), sum, "sum mismatch");
            }
        }
    }
}

/// Exhaustive small domain for `set_range`: every `(len, start, end)` with
/// len 1..=4, over a uniform sequence and one whose first cell differs. The
/// range reads back the new value, every cell outside it is unchanged, the
/// length holds, and the runs stay non-empty, compact and exactly summed.
#[test]
fn rle_set_range_exhaustive() {
    for len in 1u8..=4 {
        for split in [false, true] {
            for start in 0..len {
                for end in (start + 1)..=len {
                    let mut model = vec![1u8; usize::from(len)];
                    if split && len > 1 {
                        model[0] = 10;
                    }
                    let mut rle = Rle::from_iter(model.iter().copied());
                    rle.set_range(u32::from(start), u32::from(end), 42);
                    for cell in &mut model[usize::from(start)..usize::from(end)] {
                        *cell = 42;
                    }
                    let what = format!("set_range({start},{end},42) on {model:?}");
                    assert_eq!(rle.iter().collect::<Vec<_>>(), model, "{what}");
                    let runs = rle.runs();
                    assert!(runs.iter().all(|run| run.length > 0), "{what}: empty run");
                    assert!(
                        runs.windows(2).all(|w| w[0].value != w[1].value),
                        "{what}: adjacent runs share a value"
                    );
                    let sum: u32 = runs.iter().map(|r| r.length).sum();
                    assert_eq!(rle.len(), sum, "{what}: sum mismatch");
                }
            }
        }
    }
}
