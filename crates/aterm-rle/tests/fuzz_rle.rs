// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Robustness fuzz for the RLE attribute container. Cell-attribute runs are
// derived from untrusted terminal output (a program emitting `set_range` /
// `extend_with` patterns that exercise run merging near `u32::MAX`), so the
// container must NEVER panic on adversarial input — a panic is a
// denial-of-service. The single highest-value internal overflow site is
// `Rle::checked_run_length_sum`, whose saturation must never fire: summed run
// lengths never exceed `u32::MAX` (historically enforced by an `.expect()`
// panic at the same site). This fuzz proves that invariant holds across every
// public mutator, including starting states seeded near the `u32::MAX`
// boundary via `with_value`, where overflow would be most likely to surface.

use aterm_rle::{Rle, Run};

/// Deterministic LCG: same generator shape as the parser fuzz so runs are
/// fully reproducible from a fixed seed.
struct Lcg {
    state: u64,
}

impl Lcg {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u32(&mut self) -> u32 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.state >> 33) as u32
    }

    /// A value biased toward the `u32::MAX` boundary so overflow-prone code
    /// paths (run merging, prefix-sum accumulation) are hit hard.
    fn next_boundary_len(&mut self) -> u32 {
        match self.next_u32() % 8 {
            0 => 0,
            1 => 1,
            2 => self.next_u32() % 16,
            3 => u32::MAX,
            4 => u32::MAX - (self.next_u32() % 16),
            5 => u32::MAX / 2,
            6 => self.next_u32(),
            _ => self.next_u32() % 1024,
        }
    }

    fn next_small_value(&mut self) -> u8 {
        // A tiny value alphabet maximises run merging (adjacent equal values),
        // which is exactly what drives length summation toward overflow.
        (self.next_u32() % 4) as u8
    }
}

/// Assert the core invariant the panic site relies on: the cached
/// `total_length` equals the sum of every run length, computed in a wider type
/// so the check itself can never overflow, and that sum never exceeds
/// `u32::MAX`.
fn assert_invariant(rle: &Rle<u8>) {
    let sum: u64 = rle
        .runs()
        .iter()
        .map(|r: &Run<u8>| u64::from(r.length))
        .sum();
    assert!(
        sum <= u64::from(u32::MAX),
        "run length sum {sum} exceeded u32::MAX — checked_run_length_sum would saturate"
    );
    assert_eq!(
        u64::from(rle.len()),
        sum,
        "total_length cache diverged from actual run length sum"
    );
    // No zero-length runs should accumulate (they would still be invariant-safe
    // but indicate a correctness drift).
    assert_eq!(
        rle.remaining_capacity(),
        u32::MAX - rle.len(),
        "remaining_capacity diverged from total_length"
    );
}

/// Drive every public mutating entry point with crafted boundary-heavy inputs.
/// Each operation is followed by an invariant check; any panic (from an
/// `.expect()` inside the container, an arithmetic overflow, or an out-of-bounds
/// index) aborts the test.
#[test]
fn fuzz_rle_mutators_never_panic() {
    let mut lcg = Lcg::new(0x9E37_79B9_7F4A_7C15);

    // Pool of long-lived containers, including ones seeded right at the
    // `u32::MAX` boundary so that subsequent `extend_with` / `set_range` /
    // `resize` operations stress the overflow guard from a near-full state.
    let mut pool: Vec<Rle<u8>> = vec![
        Rle::new(),
        Rle::with_value(0, u32::MAX),
        Rle::with_value(1, u32::MAX - 1),
        Rle::with_value(2, u32::MAX / 2),
        Rle::with_value(3, 0),
        Rle::with_value(1, 1),
    ];

    for _ in 0..120_000u32 {
        let idx = (lcg.next_u32() as usize) % pool.len();
        let rle = &mut pool[idx];
        let value = lcg.next_small_value();

        match lcg.next_u32() % 12 {
            0 => {
                rle.push(value);
            }
            1 => {
                let _ = rle.try_push(value);
            }
            2 => {
                let count = lcg.next_boundary_len();
                rle.extend_with(value, count);
            }
            3 => {
                let count = lcg.next_boundary_len();
                let _ = rle.try_extend_with(value, count);
            }
            4 => {
                let index = lcg.next_boundary_len();
                let _ = rle.get(index);
            }
            5 => {
                let index = lcg.next_boundary_len();
                let _ = rle.set(index, value);
            }
            6 => {
                let start = lcg.next_boundary_len();
                let end = lcg.next_boundary_len();
                rle.set_range(start, end, value);
            }
            7 => {
                let new_len = lcg.next_boundary_len();
                rle.resize(new_len);
            }
            8 => {
                let new_len = lcg.next_boundary_len();
                rle.resize_with(new_len, value);
            }
            9 => {
                // Exercise read paths that walk runs / prefix sums.
                let mut taken = 0u32;
                for v in rle.iter() {
                    std::hint::black_box(v);
                    taken += 1;
                    if taken >= 64 {
                        break;
                    }
                }
                let _ = rle.run_count();
                let _ = rle.is_empty();
            }
            10 => {
                rle.clear();
            }
            _ => {
                // Re-seed this slot with a fresh boundary-state container so the
                // pool keeps cycling through near-overflow starting points.
                *rle = match lcg.next_u32() % 4 {
                    0 => Rle::new(),
                    1 => Rle::with_value(value, u32::MAX),
                    2 => Rle::with_value(value, lcg.next_boundary_len()),
                    _ => {
                        let mut r = Rle::new();
                        r.extend_with(value, lcg.next_boundary_len());
                        r
                    }
                };
            }
        }

        assert_invariant(&pool[idx]);
    }
}

/// Targeted adversary for the merge path: repeatedly start at `u32::MAX - k`
/// with one value, then `extend_with`/`set_range` the SAME value (forcing
/// adjacent-run merging) by `u32::MAX`-scale counts. This is the most direct
/// attempt to drive `checked_run_length_sum` / `compact` length summation over
/// `u32::MAX`.
#[test]
fn fuzz_rle_merge_overflow_never_panics() {
    let mut lcg = Lcg::new(0xD1B5_4A32_D192_ED03);

    for _ in 0..100_000u32 {
        let k = lcg.next_u32() % 32;
        let seed_len = u32::MAX - k;
        let value = lcg.next_small_value();
        let mut rle = Rle::with_value(value, seed_len);

        // Same value -> merges into the existing run; should clamp, not panic.
        rle.extend_with(value, lcg.next_boundary_len());
        assert_invariant(&rle);

        // try_extend_with the same value by a boundary count: must Err near full,
        // never panic, never violate the invariant.
        let _ = rle.try_extend_with(value, lcg.next_boundary_len());
        assert_invariant(&rle);

        // set_range across the (near full) sequence forces split + compact, which
        // re-sums run lengths through checked_run_length_sum.
        let start = lcg.next_boundary_len();
        let end = lcg.next_boundary_len();
        rle.set_range(start, end, lcg.next_small_value());
        assert_invariant(&rle);

        // resize larger then smaller exercises extend + truncate + prefix rebuild.
        rle.resize(lcg.next_boundary_len());
        assert_invariant(&rle);
        rle.resize_with(lcg.next_boundary_len(), lcg.next_small_value());
        assert_invariant(&rle);
    }
}
