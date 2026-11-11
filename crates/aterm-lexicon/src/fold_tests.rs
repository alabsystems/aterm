// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn fold_greek_final_sigma() {
    assert_eq!(fold("γάτες"), fold("γατες"));
}

#[test]
fn fold_is_idempotent() {
    for s in ["fucking", "Kätzchen", "Scheiße", "猫", "γάτα", "بسة", "İ"] {
        assert_eq!(fold(&fold(s)), fold(s), "fold not idempotent for {s}");
    }
    assert_eq!(fold("İ"), fold("i"));
}

#[test]
fn keeps_indic_marks() {
    assert_eq!(fold("बिल्ली"), "बिल्ली");
}

#[test]
fn cjk_detected_as_no_space() {
    assert!(is_no_space_script('猫'));
    assert!(is_no_space_script('ね'));
    assert!(is_no_space_script('고'));
    assert!(!is_no_space_script('a'));
}
