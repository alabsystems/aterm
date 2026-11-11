// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn test_ordering() {
    assert!(ContainmentMode::User > ContainmentMode::Safety);
    assert!(ContainmentMode::Safety > ContainmentMode::Containment);
}

#[test]
fn test_parse_rejects_bypass_attempts() {
    assert!("0".parse::<ContainmentMode>().is_err());
    assert!("1".parse::<ContainmentMode>().is_err());
    assert!("2".parse::<ContainmentMode>().is_err());

    assert!(" user".parse::<ContainmentMode>().is_err());
    assert!("user ".parse::<ContainmentMode>().is_err());
    assert!("user\n".parse::<ContainmentMode>().is_err());

    assert!("use".parse::<ContainmentMode>().is_err());
    assert!("contain".parse::<ContainmentMode>().is_err());

    assert!("".parse::<ContainmentMode>().is_err());

    assert!("user\0".parse::<ContainmentMode>().is_err());
}

#[test]
fn test_display_parse_roundtrip() {
    for mode in [
        ContainmentMode::User,
        ContainmentMode::Safety,
        ContainmentMode::Containment,
    ] {
        let s = mode.to_string();
        let parsed: ContainmentMode = s.parse().unwrap();
        assert_eq!(parsed, mode, "roundtrip failed for {mode}");
        let upper: ContainmentMode = s.to_ascii_uppercase().parse().unwrap();
        assert_eq!(upper, mode, "case-insensitive parse failed for {mode}");
    }
}
