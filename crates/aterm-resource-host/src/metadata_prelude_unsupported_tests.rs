// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn unsupported_platform_cannot_claim_native_no_fork_enforcement() {
    assert_eq!(
        super::no_fork(std::process::id()).unwrap_err().kind(),
        std::io::ErrorKind::Unsupported
    );
    assert_eq!(
        super::install().unwrap_err().kind(),
        std::io::ErrorKind::Unsupported
    );
}
