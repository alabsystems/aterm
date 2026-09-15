// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#[test]
fn completed_checks_joins_and_lock_release_prove_and_catch() {
    use aterm_spec::derive::*;
    for model in [
        native_update_check_receipt_model(),
        native_update_check_join_model(),
        native_update_check_wait_model(),
        native_update_boot_health_lock_model(),
    ] {
        aterm_spec::verify::prove_and_catch_scalar(&model, "update check coordination");
    }
}
