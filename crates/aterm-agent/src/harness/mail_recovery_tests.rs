// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#[test]
fn recovery_hint_does_not_select_another_domain_or_prefix() {
    assert!(super::matches_hint("alex@gmail.com", "a********@gmail.com"));
    assert!(!super::matches_hint("bob@gmail.com", "a********@gmail.com"));
    assert!(!super::matches_hint(
        "alex@other.test",
        "a********@gmail.com"
    ));
    assert!(super::matches_hint("alex@gmail.com", "alex@gmail.com"));
    assert!(!super::matches_hint("alex2@gmail.com", "alex@gmail.com"));
}
