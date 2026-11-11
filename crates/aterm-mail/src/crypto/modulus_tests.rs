// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
#[test]
fn pinned_proton_signature_and_changed_modulus() {
    let signed = include_str!("modulus-test.asc");
    assert_eq!(verify(signed).unwrap().len(), 256);
    assert!(verify(&signed.replacen("W2z5", "W3z5", 1)).is_err());
    assert!(verify(&format!("{signed}unexpected")).is_err());
}
