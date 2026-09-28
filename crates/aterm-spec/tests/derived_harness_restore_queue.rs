// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_spec::{derive::harness_restored_first_attempt_model, verify};

#[test]
fn restored_tabs_get_first_attempts_before_a_retry() {
    let model = harness_restored_first_attempt_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the restore queue must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "restored tabs' first attempts precede retry");
}
