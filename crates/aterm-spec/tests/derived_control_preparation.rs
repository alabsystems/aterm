// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn control_preparation_proves_and_catches_proof_before_workers() {
    aterm_spec::verify::prove_and_catch_scalar(
        &aterm_spec::derive::native_update_control_preparation_model(),
        "incoming control preparation",
    );
}
