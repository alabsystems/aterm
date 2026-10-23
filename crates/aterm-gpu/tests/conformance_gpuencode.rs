// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance test binding [`aterm_gpu::should_slice`] to `aterm_models_render::gpu_encode_model`.

use std::collections::BTreeMap;

use aterm_gpu::should_slice;
use aterm_models_render::gpu_encode_model;
use aterm_spec::verify::validate_transition_tiered;

const MAX_CELLS: i64 = 4;
const STRIDE: usize = 12;

type State = BTreeMap<&'static str, i64>;

fn state(bg_inst: i64, encoded: bool, sliced: bool) -> State {
    BTreeMap::from([
        ("bgInst", bg_inst),
        ("encoded", i64::from(encoded)),
        ("sliced", i64::from(sliced)),
    ])
}

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

    let (empty, sliced_empty) = (state(0, false, false), state(0, true, true));
    let (ok, _) = check(&empty, &sliced_empty, "Encode", &[]);
    assert!(!ok, "slicing an EMPTY background buffer MUST be refused");
    let (ok, why) = check(&empty, &sliced_empty, "Encode", &[("Buggy", 1)]);
    assert!(ok, "the unconditional-slice mutant admits it: {why}");

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
