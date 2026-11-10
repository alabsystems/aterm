// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#[test]
fn recipient_delivery_laws_have_falsifying_controls() {
    let model = aterm_models_harness::recipient_delivery_model();
    aterm_spec::interp::prove_and_catch(&model);
}
