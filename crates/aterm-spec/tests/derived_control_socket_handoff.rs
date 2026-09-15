// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn fixed_socket_handoff_proves_and_catches_early_takeover() {
    aterm_spec::verify::prove_and_catch_scalar(
        &aterm_spec::derive::native_update_control_socket_handoff_model(),
        "fixed control socket handoff",
    );
}
