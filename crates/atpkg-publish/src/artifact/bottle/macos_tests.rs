// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn object(command: u32, version: u32) -> Vec<u8> {
    let mut bytes = vec![0; 56];
    bytes[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
    bytes[16..20].copy_from_slice(&1_u32.to_le_bytes());
    bytes[20..24].copy_from_slice(&24_u32.to_le_bytes());
    bytes[32..36].copy_from_slice(&command.to_le_bytes());
    bytes[36..40].copy_from_slice(&24_u32.to_le_bytes());
    if command == 0x32 {
        bytes[40..44].copy_from_slice(&1_u32.to_le_bytes());
        bytes[44..48].copy_from_slice(&version.to_le_bytes());
    } else {
        bytes[40..44].copy_from_slice(&version.to_le_bytes());
    }
    bytes
}

#[test]
fn bootstrap_accepts_macos_11_and_rejects_newer_bottle_deployment_targets() {
    for command in [0x32, 0x24] {
        assert!(check(&object(command, MACOS_11), "git").is_ok());
        assert!(check(&object(command, (10 << 16) | (15 << 8)), "git").is_ok());
        assert!(
            check(&object(command, 14 << 16), "git")
                .unwrap_err()
                .contains("14.0.0")
        );
        assert!(check(&object(command, 15 << 16), "openssl-signing").is_err());
    }
    let mut truncated = object(0x32, MACOS_11);
    truncated.truncate(48);
    assert!(check(&truncated, "git").is_err());
    let mut wrong_platform = object(0x32, MACOS_11);
    wrong_platform[40..44].copy_from_slice(&2_u32.to_le_bytes());
    assert!(check(&wrong_platform, "git").is_err());
}
