// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn every_delivery_guard_has_a_failing_mutant() {
    aterm_spec::interp::prove_and_catch(&super::credential_delivery_model());
}
