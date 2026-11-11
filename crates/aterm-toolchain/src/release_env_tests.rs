// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn overrides_go_but_locations_and_inert_diagnostics_stay() {
    for key in [
        "RUSTFLAGS",
        "CARGO_TARGET_DIR",
        "SDKROOT",
        "TARGET_CC_arm64",
        "CARGO_PROFILE_TEST_DEBUG",
    ] {
        assert!(clears(key), "{key}");
    }
    for key in ["HOME", "PATH", "RUST_BACKTRACE", "CARGO_TERM_COLOR"] {
        assert!(!clears(key), "{key}");
    }
    assert!(launcher_env("TARGO_MANIFEST_DIR"));
    assert!(!launcher_env("PATH"));
}
