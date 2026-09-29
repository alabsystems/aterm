// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::output_retention_model, verify};

#[test]
fn tiny_output_is_bounded_and_every_eviction_is_disclosed() {
    let model = output_retention_model();
    verify::prove_and_catch_scalar(&model, "tiny output retention");
}
