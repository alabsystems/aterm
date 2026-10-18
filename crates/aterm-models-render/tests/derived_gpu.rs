// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for the frame encode's background-buffer slice: it proves at `Buggy = 0` and the
//! mutant is caught.

#[test]
fn gpu_encode_model_never_slices_an_empty_buffer() {
    let m = aterm_models_render::gpu_encode_model();
    assert_eq!(
        aterm_spec::verify::sweep_faults(std::slice::from_ref(&m)),
        Vec::<String>::new()
    );
    aterm_spec::verify::prove_and_catch_scalar(&m, "GpuEncode: NeverSliceEmpty");
}
