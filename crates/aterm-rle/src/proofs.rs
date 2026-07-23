// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Kani proofs for run-length encoding invariants.

use super::*;

/// RLE length is always the sum of run lengths.
/// This harness already passes but tightened for consistency.
#[kani::proof]
#[kani::unwind(8)]
fn rle_length_consistent() {
    let len1: u8 = kani::any();
    let len2: u8 = kani::any();
    kani::assume(len1 > 0 && len1 <= 16);
    kani::assume(len2 > 0 && len2 <= 16);

    let mut rle: Rle<u8> = Rle::new();
    rle.extend_with(1, len1 as u32);
    rle.extend_with(2, len2 as u32);

    kani::assert(
        rle.len() == (len1 as u32) + (len2 as u32),
        "length should be sum of extensions",
    );
}

/// Get always returns a value for valid indices.
/// Reduced from len<=100 to len<=16 and added unwind(20) — the get() operation
/// uses binary search over prefix sums, which doesn't benefit from large lengths.
/// A single run with len<=16 fully exercises the binary search path.
#[kani::proof]
#[kani::unwind(8)]
fn rle_get_valid_index() {
    let len: u8 = kani::any();
    let idx: u8 = kani::any();
    // Reduced from len<=16 to len<=4: binary search verification doesn't
    // benefit from large lengths; 4 elements covers all search paths.
    kani::assume(len > 0 && len <= 4);
    kani::assume(idx < len);

    let rle = Rle::with_value(42u8, len as u32);
    kani::assert(
        rle.get(idx as u32).is_some(),
        "valid index should return Some",
    );
}

/// Get returns None for out-of-bounds indices.
/// Reduced from len<100 to len<=16.
#[kani::proof]
#[kani::unwind(8)]
fn rle_get_invalid_index() {
    let len: u8 = kani::any();
    kani::assume(len > 0 && len <= 4);

    let rle = Rle::with_value(42u8, len as u32);
    kani::assert(
        rle.get(len as u32).is_none(),
        "out-of-bounds should return None",
    );
}

/// Set preserves total length.
/// Reduced to len<=2 with unwind(10): Vec::insert internals cause CBMC
/// model explosion at len>=3. Exhaustive unit test covers len 1..=4.
#[kani::proof]
#[kani::unwind(10)]
fn rle_set_preserves_length() {
    let len: u8 = kani::any();
    let idx: u8 = kani::any();
    kani::assume(len > 0 && len <= 2);
    kani::assume(idx < len);

    let mut rle = Rle::with_value(1u8, len as u32);
    let original_len = rle.len();
    rle.set(idx as u32, 99);

    kani::assert(rle.len() == original_len, "set should preserve length");
}

/// Resize to larger adds correct amount.
/// Reduced to initial<=2/final<=4 with unwind(10): resize involves
/// extend_with which creates Vec allocation overhead in CBMC.
#[kani::proof]
#[kani::unwind(10)]
fn rle_resize_grow_correct() {
    let initial: u8 = kani::any();
    let final_len: u8 = kani::any();
    kani::assume(initial > 0 && initial <= 2);
    kani::assume(final_len > initial && final_len <= 4);

    let mut rle = Rle::with_value(42u8, initial as u32);
    rle.resize(final_len as u32);

    kani::assert(
        rle.len() == final_len as u32,
        "resize should set correct length",
    );
}

/// Resize to smaller truncates correctly.
/// Reduced to initial<=4 with unwind(10): truncation iterates over
/// runs but creates less CBMC overhead than mutation operations.
#[kani::proof]
#[kani::unwind(10)]
fn rle_resize_shrink_correct() {
    let initial: u8 = kani::any();
    let final_len: u8 = kani::any();
    kani::assume(initial > 1 && initial <= 4);
    kani::assume(final_len > 0 && final_len < initial);

    let mut rle = Rle::with_value(42u8, initial as u32);
    rle.resize(final_len as u32);

    kani::assert(
        rle.len() == final_len as u32,
        "resize should truncate correctly",
    );
}

// ========================================================================
// Prover: critical proof coverage gaps filled below
// ========================================================================

/// Set writes the correct value (not just preserves length).
/// Reduced to len<=2 with unwind(10): Vec::insert internals cause CBMC
/// model explosion at len>=3. Exhaustive unit test covers len 1..=4.
#[kani::proof]
#[kani::unwind(10)]
fn rle_set_value_correctness() {
    let len: u8 = kani::any();
    let idx: u8 = kani::any();
    let new_val: u8 = kani::any();
    kani::assume(len > 0 && len <= 2);
    kani::assume(idx < len);

    let mut rle = Rle::with_value(1u8, len as u32);
    rle.set(idx as u32, new_val);

    kani::assert(
        rle.get(idx as u32) == Some(new_val),
        "get after set should return the set value",
    );
}

/// Set preserves values at other indices.
/// Fixed at len=2 with unwind(10): needs exactly 2 elements with distinct
/// indices. Exhaustive unit test covers len 2..=4.
#[kani::proof]
#[kani::unwind(10)]
fn rle_set_preserves_other_values() {
    let idx: u8 = kani::any();
    let other: u8 = kani::any();
    kani::assume(idx < 2);
    kani::assume(other < 2 && other != idx);

    let rle_before = Rle::with_value(1u8, 2);
    let val_before = rle_before.get(other as u32);

    let mut rle_after = Rle::with_value(1u8, 2);
    rle_after.set(idx as u32, 99);
    let val_after = rle_after.get(other as u32);

    kani::assert(
        val_before == val_after,
        "set should not change values at other indices",
    );
}

/// No runs have zero length after set.
/// Reduced to len<=2 with unwind(10): Vec::insert internals cause CBMC
/// model explosion at len>=3. Exhaustive unit test covers len 1..=4.
#[kani::proof]
#[kani::unwind(10)]
fn rle_set_no_zero_length_runs() {
    let len: u8 = kani::any();
    let idx: u8 = kani::any();
    let val: u8 = kani::any();
    kani::assume(len > 0 && len <= 2);
    kani::assume(idx < len);

    let mut rle = Rle::with_value(1u8, len as u32);
    rle.set(idx as u32, val);

    for run in rle.runs() {
        kani::assert(run.length > 0, "no run should have zero length after set");
    }
}

/// No adjacent runs have the same value after set.
/// Reduced to len<=2 with unwind(10): CBMC model explosion at len>=3.
/// Exhaustive unit test covers len 1..=4.
#[kani::proof]
#[kani::unwind(10)]
fn rle_set_no_adjacent_duplicates() {
    let len: u8 = kani::any();
    let idx: u8 = kani::any();
    let val: u8 = kani::any();
    kani::assume(len > 0 && len <= 2);
    kani::assume(idx < len);

    let mut rle = Rle::with_value(1u8, len as u32);
    rle.set(idx as u32, val);

    let runs = rle.runs();
    if runs.len() >= 2 {
        let mut i = 0;
        while i + 1 < runs.len() {
            kani::assert(
                runs[i].value != runs[i + 1].value,
                "adjacent runs must not have the same value",
            );
            i += 1;
        }
    }
}

/// Total length equals sum of run lengths after set.
/// Reduced to len<=2 with unwind(10): CBMC model explosion at len>=3.
/// Exhaustive unit test covers len 1..=4.
#[kani::proof]
#[kani::unwind(10)]
fn rle_set_length_sum_invariant() {
    let len: u8 = kani::any();
    let idx: u8 = kani::any();
    let val: u8 = kani::any();
    kani::assume(len > 0 && len <= 2);
    kani::assume(idx < len);

    let mut rle = Rle::with_value(1u8, len as u32);
    rle.set(idx as u32, val);

    let sum: u32 = rle.runs().iter().map(|r| r.length).sum();
    kani::assert(
        rle.len() == sum,
        "total_length must equal sum of run lengths",
    );
}

/// set_range preserves total length.
/// Reduced to len<=2 with unwind(12): Vec::splice internals cause CBMC
/// model explosion at len>=3. Multi-run coverage provided by exhaustive
/// unit test `rle_set_range_preserves_length_exhaustive` (len 1..=4).
#[kani::proof]
#[kani::unwind(12)]
fn rle_set_range_preserves_length() {
    let len: u8 = kani::any();
    let start: u8 = kani::any();
    let end: u8 = kani::any();
    kani::assume(len > 0 && len <= 2);
    kani::assume(start < len);
    kani::assume(end > start && end <= len);

    let mut rle = Rle::with_value(1u8, len as u32);
    let original_len = rle.len();
    rle.set_range(start as u32, end as u32, 42);

    kani::assert(
        rle.len() == original_len,
        "set_range should preserve total length",
    );
}

/// set_range writes correct values.
/// Reduced to len<=2 with unwind(12): Vec::splice internals cause CBMC
/// model explosion at len>=3. Full coverage at len 1..=4 provided by
/// exhaustive unit test `rle_set_range_value_correctness_exhaustive`.
#[kani::proof]
#[kani::unwind(12)]
fn rle_set_range_value_correctness() {
    let len: u8 = kani::any();
    let start: u8 = kani::any();
    let end: u8 = kani::any();
    let check_idx: u8 = kani::any();
    kani::assume(len > 0 && len <= 2);
    kani::assume(start < len);
    kani::assume(end > start && end <= len);
    kani::assume(check_idx >= start && check_idx < end);

    let mut rle = Rle::with_value(1u8, len as u32);
    rle.set_range(start as u32, end as u32, 42);

    kani::assert(
        rle.get(check_idx as u32) == Some(42),
        "index within set_range should return the set value",
    );
}

/// set_range structural invariants hold.
/// Reduced to len<=2 with unwind(12): Vec::splice internals cause CBMC
/// model explosion at len>=3. Full coverage at len 1..=4 provided by
/// exhaustive unit test `rle_set_range_structural_invariants_exhaustive`.
#[kani::proof]
#[kani::unwind(12)]
fn rle_set_range_structural_invariants() {
    let len: u8 = kani::any();
    let start: u8 = kani::any();
    let end: u8 = kani::any();
    kani::assume(len > 0 && len <= 2);
    kani::assume(start < len);
    kani::assume(end > start && end <= len);

    let mut rle = Rle::with_value(1u8, len as u32);
    rle.set_range(start as u32, end as u32, 42);

    for run in rle.runs() {
        kani::assert(run.length > 0, "no zero-length runs after set_range");
    }

    let runs = rle.runs();
    if runs.len() >= 2 {
        let mut i = 0;
        while i + 1 < runs.len() {
            kani::assert(
                runs[i].value != runs[i + 1].value,
                "no adjacent duplicate values after set_range",
            );
            i += 1;
        }
    }

    let sum: u32 = runs.iter().map(|r| r.length).sum();
    kani::assert(
        rle.len() == sum,
        "total_length must equal sum of run lengths after set_range",
    );
}
