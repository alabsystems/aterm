// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Kani proofs for RLE saturation arithmetic and try_extend_with/try_push
//! rejection contracts (#4950, #5010).

use super::*;

fn rle_from_raw_runs(runs: Vec<Run<u8>>, total_length: u32) -> Rle<u8> {
    Rle {
        runs,
        total_length,
        prefix_sums: Vec::new(),
    }
}

/// extend_with clamps to remaining capacity instead of desynchronizing runs.
#[kani::proof]
#[kani::unwind(4)]
fn rle_extend_with_clamps_to_capacity() {
    let slack: u8 = kani::any();
    let extend_count: u8 = kani::any();
    kani::assume(slack <= 4);
    kani::assume(extend_count > 0 && extend_count <= 4);

    let init_len = u32::MAX - slack as u32;
    let expected_second = (extend_count as u32).min(slack as u32);

    let mut rle = rle_from_raw_runs(
        vec![Run {
            value: 1,
            length: init_len,
        }],
        init_len,
    );
    rle.extend_with(2, extend_count as u32);

    let runs = rle.runs();
    kani::assert(runs[0].value == 1, "first run value must be preserved");
    kani::assert(
        runs[0].length == init_len,
        "first run length must be preserved",
    );
    if expected_second == 0 {
        kani::assert(
            runs.len() == 1,
            "no second run should be added once capacity is exhausted",
        );
    } else {
        kani::assert(
            runs.len() == 2,
            "a second run should capture the remaining capacity",
        );
        kani::assert(runs[1].value == 2, "second run value must be preserved");
        kani::assert(
            runs[1].length == expected_second,
            "second run length must clamp to the remaining capacity",
        );
    }
    kani::assert(
        rle.len() == init_len + expected_second,
        "extend_with must clamp to the remaining capacity",
    );
}

/// push becomes a no-op once the sequence reaches capacity.
#[kani::proof]
#[kani::unwind(4)]
fn rle_push_clamps_at_capacity() {
    let val: u8 = kani::any();
    let slack: u8 = kani::any();
    kani::assume(slack <= 2);

    // Base = u32::MAX - slack, so push may or may not hit the capacity boundary.
    let base = u32::MAX - slack as u32;
    let mut rle = rle_from_raw_runs(
        vec![Run {
            value: 1,
            length: base,
        }],
        base,
    );
    rle.push(val);

    let expected = base + u32::from(slack > 0);
    kani::assert(
        rle.len() == expected,
        "push must use the remaining capacity exactly once",
    );

    let runs = rle.runs();
    if slack == 0 || val == 1 {
        kani::assert(
            runs.len() == 1,
            "push should not create a second run when merging or at capacity",
        );
        kani::assert(
            runs[0].value == 1,
            "merged run must keep the original value",
        );
        kani::assert(
            runs[0].length == expected,
            "merged run length must stay exact",
        );
    } else {
        kani::assert(
            runs.len() == 2,
            "push should create a trailing run when slack and a new value exist",
        );
        kani::assert(runs[0].value == 1, "first run value must be preserved");
        kani::assert(runs[0].length == base, "first run length must be preserved");
        kani::assert(
            runs[1].value == val,
            "new trailing run must store the pushed value",
        );
        kani::assert(runs[1].length == 1, "push may add at most one cell");
    }
}

/// Linear find_run fallback stays exact after clamped growth near capacity.
#[kani::proof]
#[kani::unwind(4)]
fn rle_linear_find_run_exact_after_clamp() {
    let slack: u8 = kani::any();
    let len2: u8 = kani::any();
    kani::assume(slack <= 4);
    kani::assume(len2 > 0 && len2 <= 4);

    let l1 = u32::MAX - slack as u32;
    let l2 = len2 as u32;

    let mut rle = rle_from_raw_runs(
        vec![Run {
            value: 1,
            length: l1,
        }],
        l1,
    );
    rle.extend_with(2, l2);

    let expected_second = l2.min(slack as u32);

    // Force the linear fallback path instead of the cached binary search.
    rle.prefix_sums.clear();

    kani::assert(
        rle.get(0) == Some(1),
        "first cell should stay in the first run",
    );
    kani::assert(
        rle.get(l1 - 1) == Some(1),
        "last cell of the first run must still resolve correctly",
    );
    if expected_second == 0 {
        kani::assert(
            rle.get(l1).is_none(),
            "no in-bounds index should exist past capacity once slack is exhausted",
        );
    } else {
        kani::assert(
            rle.get(l1) == Some(2),
            "first cell in the clamped trailing run must resolve correctly",
        );
    }
}

/// Same-value extend merges and clamps to the exact remaining capacity.
#[kani::proof]
#[kani::unwind(4)]
fn rle_extend_with_same_value_clamps() {
    let slack: u8 = kani::any();
    let extra: u8 = kani::any();
    kani::assume(slack <= 4);
    kani::assume(extra > 0 && extra <= 4);

    let base = u32::MAX - slack as u32;
    let mut rle = rle_from_raw_runs(
        vec![Run {
            value: 1,
            length: base,
        }],
        base,
    );
    rle.extend_with(1, extra as u32);

    let expected = base + (extra as u32).min(slack as u32);
    kani::assert(
        rle.len() == expected,
        "same-value extend should consume only remaining capacity",
    );
    kani::assert(
        rle.run_count() == 1,
        "same-value extends should remain a single run",
    );
    kani::assert(
        rle.runs()[0].length == expected,
        "merged run length must stay synchronized with total_length",
    );
}

/// Push preserves structural invariants.
#[kani::proof]
#[kani::unwind(8)]
fn rle_push_structural_invariants() {
    let v0: u8 = kani::any();
    let v1: u8 = kani::any();

    let mut rle: Rle<u8> = Rle::new();
    rle.push(v0);
    rle.push(v1);

    // Total length must reflect number of pushes.
    kani::assert(rle.len() == 2, "total_length equals push count");

    let runs = rle.runs();
    for run in runs {
        kani::assert(run.length > 0, "no zero-length runs after push sequence");
    }

    if v0 == v1 {
        kani::assert(runs.len() == 1, "equal pushes should merge into one run");
        kani::assert(runs[0].length == 2, "merged run should have length 2");
        kani::assert(runs[0].value == v0, "merged run should keep pushed value");
    } else {
        kani::assert(runs.len() == 2, "different pushes should create two runs");
        kani::assert(runs[0].length == 1, "first run length should be 1");
        kani::assert(runs[1].length == 1, "second run length should be 1");
        kani::assert(runs[0].value == v0, "first run should preserve first value");
        kani::assert(
            runs[1].value == v1,
            "second run should preserve second value",
        );
        kani::assert(
            runs[0].value != runs[1].value,
            "adjacent runs must not have duplicate values",
        );
    }
}

// ========================================================================
// try_extend_with / try_push rejection proofs (#5010)
// ========================================================================

/// try_extend_with rejection leaves the RLE completely unchanged.
///
/// When `count > remaining_capacity()`, the operation must return `Err`
/// and the RLE must be byte-identical to its pre-call state: same
/// `total_length`, same run count, same run values and lengths.
#[kani::proof]
#[kani::unwind(4)]
fn rle_try_extend_with_rejection_is_noop() {
    let slack: u8 = kani::any();
    let request: u8 = kani::any();
    kani::assume(slack <= 4);
    // request must exceed slack to trigger the rejection path.
    kani::assume(request > slack && request <= 8);

    let base = u32::MAX - slack as u32;
    let mut rle = rle_from_raw_runs(
        vec![Run {
            value: 1,
            length: base,
        }],
        base,
    );

    // Snapshot pre-call state.
    let len_before = rle.len();
    let run_count_before = rle.run_count();

    let result = rle.try_extend_with(2, request as u32);

    kani::assert(result.is_err(), "overflow request must return Err");

    let err = match result {
        Err(e) => e,
        Ok(()) => unreachable!(),
    };
    kani::assert(
        err.requested == request as u32,
        "error must report the original requested count",
    );
    kani::assert(
        err.available == slack as u32,
        "error must report the actual remaining capacity",
    );

    // RLE must be completely unchanged.
    kani::assert(
        rle.len() == len_before,
        "total_length must not change on rejection",
    );
    kani::assert(
        rle.run_count() == run_count_before,
        "run_count must not change on rejection",
    );
    kani::assert(
        rle.runs()[0].length == base,
        "first run length must not change on rejection",
    );
    kani::assert(
        rle.runs()[0].value == 1,
        "first run value must not change on rejection",
    );
}

/// try_extend_with acceptance preserves structural invariants.
///
/// When `count <= remaining_capacity()`, the operation must succeed,
/// the length must grow by exactly `count`, and the sum-of-runs invariant
/// must hold.
#[kani::proof]
#[kani::unwind(4)]
fn rle_try_extend_with_acceptance_preserves_invariants() {
    let slack: u8 = kani::any();
    let request: u8 = kani::any();
    kani::assume(slack > 0 && slack <= 4);
    // request must be within remaining capacity.
    kani::assume(request > 0 && request <= slack);

    let base = u32::MAX - slack as u32;
    let mut rle = rle_from_raw_runs(
        vec![Run {
            value: 1,
            length: base,
        }],
        base,
    );

    let result = rle.try_extend_with(2, request as u32);

    kani::assert(result.is_ok(), "within-capacity request must succeed");
    kani::assert(
        rle.len() == base + request as u32,
        "total_length must grow by exactly the requested count",
    );

    // Structural: sum of run lengths == total_length.
    let sum: u32 = rle.runs().iter().map(|r| r.length).sum();
    kani::assert(
        rle.len() == sum,
        "total_length must equal sum of run lengths after accepted try_extend_with",
    );

    // No zero-length runs.
    for run in rle.runs() {
        kani::assert(
            run.length > 0,
            "no zero-length runs after accepted try_extend_with",
        );
    }
}

/// try_push rejection at capacity leaves the RLE unchanged.
#[kani::proof]
#[kani::unwind(4)]
fn rle_try_push_rejection_is_noop() {
    let val: u8 = kani::any();

    let mut rle = rle_from_raw_runs(
        vec![Run {
            value: 1,
            length: u32::MAX,
        }],
        u32::MAX,
    );

    let len_before = rle.len();
    let result = rle.try_push(val);

    kani::assert(result.is_err(), "push at u32::MAX capacity must fail");
    kani::assert(
        rle.len() == len_before,
        "total_length must not change on rejected try_push",
    );
    kani::assert(
        rle.run_count() == 1,
        "run_count must not change on rejected try_push",
    );
}
