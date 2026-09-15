// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn handoff_bind_retry_proves_and_catches_foreign_path_reuse() {
    aterm_spec::verify::prove_and_catch_scalar(
        &aterm_spec::derive::native_update_handoff_bind_retry_model(),
        "handoff bind-only retry",
    );
}
