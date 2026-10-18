// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Tier-1 conformance: binds the real frame-encode slice decision to
//! `aterm_models_render::gpu_encode_model` (`GpuEncode`).
//!
//! The model proves `NeverSliceEmpty` (a background vertex buffer is sliced only when it
//! holds an instance) and catches an unconditional slice at `Buggy = 1`. Here the
//! `Encode` step's `sliced` is the real [`aterm_gpu::should_slice`] (the GPU-free gate
//! inside `VertexBuffer::upload_via` and the Metal `upload_stream`), driven after a run of
//! `Append` steps (one `BgInstance` per non-default-background cell), and each transition
//! is checked against the named action. Two negative controls must be refused: an empty
//! frame that slices (admitted only by the mutant) and a non-empty frame that does not.

use std::collections::BTreeMap;

use aterm_gpu::should_slice;
use aterm_models_render::gpu_encode_model;
use aterm_spec::verify::validate_transition_tiered;

/// The model's `MaxCells`.
const MAX_CELLS: i64 = 4;
/// Bytes per `BgInstance` (`rect:[u16;4] + color:[u8;4]`), so `bgInst` instances are the
/// byte stream `should_slice` sees in `VertexBuffer::upload_via(.., cast_slice(&bg))`.
const STRIDE: usize = 12;

type State = BTreeMap<&'static str, i64>;

fn state(bg_inst: i64, encoded: bool, sliced: bool) -> State {
    BTreeMap::from([
        ("bgInst", bg_inst),
        ("encoded", i64::from(encoded)),
        ("sliced", i64::from(sliced)),
    ])
}

/// One frame of `n` fills: `n` appends, then the encode whose slice is the real decision.
fn drive_frame(n: i64) -> Vec<(State, State, &'static str)> {
    let mut steps = Vec::new();
    for k in 0..n {
        steps.push((state(k, false, false), state(k + 1, false, false), "Append"));
    }
    let bytes = usize::try_from(n).expect("a frame's fills are non-negative") * STRIDE;
    steps.push((
        state(n, false, false),
        state(n, true, should_slice(bytes)),
        "Encode",
    ));
    steps
}

#[test]
fn real_gpu_encode_slice_decision_conforms_to_the_gpuencode_model() {
    let m = gpu_encode_model();
    let check = |prev: &State, next: &State, action: &str, overrides: &[(&'static str, i64)]| {
        validate_transition_tiered(&m, overrides, prev, next, Some(action), "GpuEncode")
    };

    // Both regimes: a zero-cell frame (not sliced) and every non-empty one (sliced).
    for n in 0..=MAX_CELLS {
        let steps = drive_frame(n);
        let encode = &steps.last().expect("a frame ends in its encode").1;
        assert_eq!(
            encode["sliced"],
            i64::from(n > 0),
            "should_slice(bgInst > 0), n = {n}"
        );
        for (prev, next, action) in &steps {
            let (ok, why) = check(prev, next, action, &[]);
            assert!(
                ok,
                "real {action} {prev:?} -> {next:?} (n = {n}) must conform: {why}"
            );
        }
    }

    // Negative control: encode an empty frame that slices anyway (the wgpu panic). The
    // committed model refuses it; the unconditional-slice mutant admits it.
    let (empty, sliced_empty) = (state(0, false, false), state(0, true, true));
    let (ok, _) = check(&empty, &sliced_empty, "Encode", &[]);
    assert!(!ok, "slicing an EMPTY background buffer MUST be refused");
    let (ok, why) = check(&empty, &sliced_empty, "Encode", &[("Buggy", 1)]);
    assert!(ok, "the unconditional-slice mutant admits it: {why}");

    // Negative control: a non-empty frame whose encode does not slice.
    let (ok, _) = check(
        &state(2, false, false),
        &state(2, true, false),
        "Encode",
        &[],
    );
    assert!(
        !ok,
        "not slicing a NON-empty background buffer MUST be refused"
    );
}
