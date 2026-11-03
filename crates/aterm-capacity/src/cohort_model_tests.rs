// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact checkout representation controls. Scalar model laws and mutants are checked by
//! the shared aterm-models registry sweep; physical conformance lives beside each protocol.

#[test]
fn source_checkout_representation_refuses_ambiguous_or_lossy_spellings() {
    assert!(crate::Need::new("source").checkout_exact("").is_err());
    assert!(
        crate::Need::new("source")
            .checkout_exact("relative/checkout")
            .is_err()
    );
    let complete = if cfg!(windows) {
        r"C:\captured\canonical\checkout"
    } else {
        "/captured/canonical/checkout"
    };
    assert!(crate::ledger::checkout_exact(&format!("{complete}/../other")).is_err());
    assert!(
        crate::Need::new("source")
            .checkout_exact(format!("{complete}/{}🦀", "a".repeat(512)))
            .is_err()
    );
    assert_eq!(crate::ledger::checkout_exact(complete).unwrap(), complete);
    assert_eq!(
        crate::Need::new("source")
            .checkout_exact(complete)
            .unwrap()
            .checkout
            .as_deref(),
        Some(complete)
    );
}
