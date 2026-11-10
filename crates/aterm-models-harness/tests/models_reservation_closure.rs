// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#[test]
fn reservation_closure_laws_have_falsifying_controls() {
    let model = aterm_models_harness::reservation_closure_model();
    aterm_spec::interp::prove_and_catch(&model);
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(&model));
    assert!(faults.is_empty(), "{}", faults.join("\n"));
}
