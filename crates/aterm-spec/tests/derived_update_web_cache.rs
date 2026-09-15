// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn web_cache_proves_and_catches_lost_stage_and_rebound_authority() {
    aterm_spec::verify::prove_and_catch_scalar(
        &aterm_spec::derive::native_update_web_cache_model(),
        "web update cache recovery",
    );
}
