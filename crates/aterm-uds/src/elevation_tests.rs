// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// The token query answers on every Windows host, and the cached answer is
/// what it says. Which answer depends on how the test runs: an unelevated
/// session reads `false`, an elevated one `true`.
#[test]
fn elevation_is_read_from_the_process_token() {
    let read = super::imp::token_is_elevated().expect("the process token is readable");
    assert_eq!(super::is_elevated(), read);
}
